use crate::config::Settings;
use crate::engine::{ValidationBlocker, validate_model_path};
use crate::receipts::Blocker;
use llama_native_cache::PrefixCacheValue;
use llama_native_engine::NativeModelHandle;
use llama_native_host::{NativeHost, NativeHostConfig, PrefixCacheStore, SystemClock};
use llama_native_types::{NativeError, NativeErrorCode, NativeModelConfig, ResidentModelStatus};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentSlotStatus {
    pub slot_id: usize,
    pub model_path: PathBuf,
    pub model_bytes: u64,
    pub reserved_bytes: u64,
    pub status: ResidentModelStatus,
}

const PERSISTENT_PREFIX_CACHE_MAX_ENTRIES: usize = 128;
const PERSISTENT_PREFIX_CACHE_MAX_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProductHostKey {
    memory_budget_bytes: u64,
    max_slots: usize,
    data_dir: PathBuf,
}

#[derive(Debug)]
struct ProductHost {
    key: ProductHostKey,
    host: Arc<NativeHost>,
}

static PRODUCT_HOST: OnceLock<Mutex<Option<ProductHost>>> = OnceLock::new();

fn product_host() -> &'static Mutex<Option<ProductHost>> {
    PRODUCT_HOST.get_or_init(|| Mutex::new(None))
}

fn host_key(settings: &Settings) -> ProductHostKey {
    ProductHostKey {
        memory_budget_bytes: settings.resident_memory_budget_bytes,
        max_slots: settings.max_parallel_sequences.clamp(1, 4) as usize,
        data_dir: settings.data_dir.clone(),
    }
}

struct ProductPrefixCacheStore {
    store: crate::store::RuntimeStore,
}

impl ProductPrefixCacheStore {
    fn document(namespace: &str) -> String {
        format!("native-host-prefix-cache.{namespace}")
    }
}

impl PrefixCacheStore for ProductPrefixCacheStore {
    fn load(&self, namespace: &str) -> Result<Vec<PrefixCacheValue>, NativeError> {
        self.store
            .get(&Self::document(namespace))
            .map(|values| values.unwrap_or_default())
            .map_err(prefix_store_error)
    }

    fn save(&self, namespace: &str, value: &PrefixCacheValue) -> Result<(), NativeError> {
        let document = Self::document(namespace);
        self.store
            .mutate(&document, Vec::<PrefixCacheValue>::new, |values| {
                values.retain(|candidate| candidate.metadata.id != value.metadata.id);
                values.push(value.clone());
                values.sort_by_key(|candidate| candidate.metadata.last_used_at_ms);
                while values.len() > PERSISTENT_PREFIX_CACHE_MAX_ENTRIES
                    || values
                        .iter()
                        .map(|candidate| candidate.metadata.state_bytes)
                        .sum::<usize>()
                        > PERSISTENT_PREFIX_CACHE_MAX_BYTES
                {
                    values.remove(0);
                }
                Ok(())
            })
            .map_err(prefix_store_error)
    }

    fn delete(&self, namespace: &str, id: &str) -> Result<(), NativeError> {
        let document = Self::document(namespace);
        self.store
            .mutate(&document, Vec::<PrefixCacheValue>::new, |values| {
                values.retain(|candidate| candidate.metadata.id != id);
                Ok(())
            })
            .map_err(prefix_store_error)
    }
}

fn prefix_store_error(error: anyhow::Error) -> NativeError {
    NativeError::new(
        NativeErrorCode::Internal,
        format!("encrypted native prefix-cache storage failed: {error}"),
    )
}

fn create_product_host(key: &ProductHostKey) -> Result<Arc<NativeHost>, NativeError> {
    let persistent_store =
        crate::store::RuntimeStore::open(&key.data_dir).map_err(prefix_store_error)?;
    let host = NativeHost::with_dependencies(
        NativeHostConfig {
            memory_budget_bytes: key.memory_budget_bytes,
            max_slots: key.max_slots,
            cache_namespace: "mom-llama".to_string(),
            ..NativeHostConfig::default()
        },
        Arc::new(SystemClock),
        Some(Arc::new(ProductPrefixCacheStore {
            store: persistent_store,
        })),
    );
    host.restore_persistent_cache()?;
    Ok(Arc::new(host))
}

fn with_host<T>(
    settings: &Settings,
    operation: impl FnOnce(&NativeHost) -> Result<T, NativeError>,
) -> Result<T, ValidationBlocker> {
    let key = host_key(settings);
    let mut current = product_host().lock().map_err(|_| {
        native_blocker(
            "native_host_poisoned",
            "The native model host is unavailable.",
        )
    })?;
    if current.as_ref().is_none_or(|host| host.key != key) {
        *current = Some(ProductHost {
            host: create_product_host(&key).map_err(native_error_blocker)?,
            key,
        });
    }
    operation(
        current
            .as_ref()
            .expect("product host is initialized")
            .host
            .as_ref(),
    )
    .map_err(native_error_blocker)
}

/// Returns the product-owned host and exact configured model profile for a
/// reusable in-process gateway adapter. This never constructs a second model
/// owner and therefore preserves resident reuse, cancellation, and cache state.
pub fn gateway_native_host_and_model() -> anyhow::Result<(Arc<NativeHost>, NativeModelConfig)> {
    let (host, model) = gateway_native_configuration()?;
    let model = model.ok_or_else(|| anyhow::anyhow!("No local GGUF model is configured."))?;
    Ok((host, model))
}

/// Returns the current product-owned host and optional configured model. This
/// is safe to call after settings changes so an embedded gateway can rebind
/// future requests without constructing an independent model/cache owner.
pub fn gateway_native_configuration() -> anyhow::Result<(Arc<NativeHost>, Option<NativeModelConfig>)>
{
    let settings = crate::config::resolve_settings()?;
    let config = settings
        .model_path
        .as_deref()
        .map(|model_path| -> anyhow::Result<NativeModelConfig> {
            validate_model_path(model_path)
                .map_err(|blocked| anyhow::anyhow!(blocked.blocker.message))?;
            Ok(model_config(
                &settings,
                model_path,
                settings.mmproj_path.as_deref(),
            ))
        })
        .transpose()?;
    let key = host_key(&settings);
    let mut current = product_host()
        .lock()
        .map_err(|_| anyhow::anyhow!("The native model host is unavailable."))?;
    if current.as_ref().is_none_or(|host| host.key != key) {
        *current = Some(ProductHost {
            host: create_product_host(&key)
                .map_err(|error| anyhow::anyhow!(error.message.clone()))?,
            key,
        });
    }
    let host = Arc::clone(&current.as_ref().expect("product host is initialized").host);
    Ok((host, config))
}

fn model_config(
    settings: &Settings,
    model_path: &Path,
    mmproj_path: Option<&Path>,
) -> NativeModelConfig {
    let mut config = NativeModelConfig::local(model_path.to_path_buf());
    config.device = settings.native_device;
    config.context_tokens = settings.context_tokens;
    config.batch_tokens = settings.batch_tokens;
    config.max_sequences = settings.max_parallel_sequences.clamp(1, 4);
    config.mmproj_path = mmproj_path.map(Path::to_path_buf);
    config
}

pub fn resident_model(settings: &Settings) -> Result<NativeModelHandle, ValidationBlocker> {
    resident_model_for_slot(settings, 0, settings.model_path.as_deref())
}

pub fn resident_model_for_profile(
    settings: &Settings,
    model_path: &Path,
    mmproj_path: Option<&Path>,
) -> Result<NativeModelHandle, ValidationBlocker> {
    validate_model_path(model_path)?;
    let config = model_config(settings, model_path, mmproj_path);
    with_host(settings, |host| host.acquire(config))
}

pub fn resident_model_for_slot(
    settings: &Settings,
    slot_id: usize,
    requested_model_path: Option<&Path>,
) -> Result<NativeModelHandle, ValidationBlocker> {
    if slot_id >= host_key(settings).max_slots {
        return Err(native_blocker(
            "native_slot_out_of_range",
            "The requested resident model slot is outside the configured bound.",
        ));
    }
    if requested_model_path.is_none() {
        let current = product_host().lock().map_err(|_| {
            native_blocker(
                "native_host_poisoned",
                "The native model host is unavailable.",
            )
        })?;
        if let Some(handle) = current
            .as_ref()
            .and_then(|product| product.host.handle(slot_id))
        {
            return Ok(handle);
        }
    }
    let Some(model_path) = requested_model_path else {
        return Err(ValidationBlocker {
            readiness: "blocked_missing_model".to_string(),
            blocker: Blocker::new(
                "model_path_missing",
                "No GGUF model path is configured.",
                vec!["Choose a local GGUF model in Settings.".to_string()],
            ),
        });
    };
    validate_model_path(model_path)?;
    let config = model_config(settings, model_path, settings.mmproj_path.as_deref());
    with_host(settings, |host| host.load_into_slot(slot_id, config))
}

pub fn resident_status() -> Option<ResidentModelStatus> {
    product_host().lock().ok().and_then(|current| {
        current.as_ref().and_then(|host| {
            host.host
                .slots()
                .into_iter()
                .find(|slot| slot.slot_id == 0)
                .map(|slot| slot.status)
        })
    })
}

pub fn resident_slots() -> Vec<ResidentSlotStatus> {
    product_host()
        .lock()
        .map(|current| {
            current
                .as_ref()
                .map(|product| {
                    product
                        .host
                        .slots()
                        .into_iter()
                        .map(|slot| ResidentSlotStatus {
                            slot_id: slot.slot_id,
                            model_path: slot.model_path,
                            model_bytes: slot.model_bytes,
                            reserved_bytes: slot.reserved_bytes,
                            status: slot.status,
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

pub fn unload_resident_slot(slot_id: usize) -> bool {
    product_host()
        .lock()
        .ok()
        .and_then(|current| current.as_ref().map(|host| host.host.unload(slot_id)))
        .unwrap_or(false)
}

pub fn unload_resident_model() -> bool {
    product_host()
        .lock()
        .ok()
        .and_then(|current| current.as_ref().map(|host| host.host.unload_all() > 0))
        .unwrap_or(false)
}

pub fn cancel_native_request(request_id: &str, branch_id: Option<&str>) -> usize {
    product_host()
        .lock()
        .ok()
        .and_then(|current| {
            current
                .as_ref()
                .map(|host| host.host.cancel(request_id, branch_id))
        })
        .unwrap_or_default()
}

pub fn skip_native_reasoning(request_id: &str, branch_id: Option<&str>) -> usize {
    product_host()
        .lock()
        .ok()
        .and_then(|current| {
            current
                .as_ref()
                .map(|host| host.host.skip_reasoning(request_id, branch_id))
        })
        .unwrap_or_default()
}

fn native_error_blocker(error: NativeError) -> ValidationBlocker {
    ValidationBlocker {
        readiness: match error.code {
            NativeErrorCode::ModelMissing => "blocked_missing_model",
            NativeErrorCode::ModelInvalid | NativeErrorCode::ModelLoadFailed => {
                "blocked_invalid_model"
            }
            NativeErrorCode::MemoryBudgetExceeded | NativeErrorCode::ModelSlotsFull => {
                "blocked_memory_budget"
            }
            _ => "blocked_native_runtime",
        }
        .to_string(),
        blocker: Blocker::new(
            error.code.to_string(),
            error.message,
            vec!["Check the selected model and native runtime settings.".to_string()],
        ),
    }
}

fn native_blocker(code: &str, message: &str) -> ValidationBlocker {
    ValidationBlocker {
        readiness: "blocked_native_runtime".to_string(),
        blocker: Blocker::new(code, message, vec!["Restart Mom Llama Lab.".to_string()]),
    }
}

#[cfg(test)]
mod tests {
    use super::{PrefixCacheStore, ProductPrefixCacheStore};
    use llama_native_cache::{CacheFingerprint, CacheTier, PrefixCacheMetadata, PrefixCacheValue};
    use llama_native_host::memory_reservation;
    use llama_native_types::{PromptForm, PromptTokenPolicy, SequenceStateBlob};

    fn cache_value() -> PrefixCacheValue {
        let token_ids = vec![1, 2, 3];
        PrefixCacheValue {
            metadata: PrefixCacheMetadata::new(
                "persistent-test",
                CacheTier::SessionPersistent,
                CacheFingerprint {
                    prompt_form: PromptForm::Chat,
                    prompt_token_policy: PromptTokenPolicy::ChatTemplate,
                    model_sha256: "model".to_string(),
                    binding_version: "binding".to_string(),
                    build_id: "build".to_string(),
                    tokenizer_sha256: "tokenizer".to_string(),
                    chat_template_sha256: "template".to_string(),
                    multimodal_projector_sha256: None,
                    lora_adapters_sha256: Vec::new(),
                    context_tokens: 128,
                    batch_tokens: 32,
                    max_sequences: 1,
                    device: "cpu".to_string(),
                    rope_config_sha256: "rope".to_string(),
                    kv_layout_sha256: "kv".to_string(),
                },
                token_ids.clone(),
                3,
                1,
            ),
            sequence: SequenceStateBlob {
                sequence_id: 0,
                token_count: token_ids.len(),
                bytes: vec![4, 5, 6],
                token_ids,
            },
        }
    }

    #[test]
    fn resident_budget_reserves_runtime_context_and_projector_memory() {
        let mib = 1024 * 1024;
        assert_eq!(memory_reservation(100 * mib, 0), 484 * mib);
        assert_eq!(
            memory_reservation(4 * 1024 * mib, 500 * mib),
            6 * 1024 * mib + 500 * mib
        );
    }

    #[test]
    fn product_prefix_cache_round_trips_through_encrypted_storage() {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-prefix-cache-test-{}",
            uuid::Uuid::new_v4()
        ));
        let store = crate::store::RuntimeStore::open_with_key(&data_dir, [19_u8; 32])
            .expect("test encrypted store");
        let cache = ProductPrefixCacheStore { store };
        let value = cache_value();
        cache.save("test", &value).expect("save prefix");
        assert_eq!(cache.load("test").expect("load prefix"), vec![value]);
        cache
            .delete("test", "persistent-test")
            .expect("delete prefix");
        assert!(cache.load("test").expect("load after delete").is_empty());
        std::fs::remove_dir_all(data_dir).expect("remove test directory");
    }
}
