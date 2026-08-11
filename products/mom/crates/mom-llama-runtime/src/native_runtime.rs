use crate::config::{KvCachePolicy, Settings};
use crate::engine::{ValidationBlocker, validate_model_path};
use crate::receipts::Blocker;
use llama_native_cache::PrefixCacheValue;
use llama_native_engine::NativeModelHandle;
use llama_native_host::{
    HostCachePolicy, NativeHost, NativeHostConfig, PrefixCacheStore, ProcessExitJoinedNativeHost,
    SystemClock,
};
use llama_native_types::{NativeError, NativeErrorCode, NativeModelConfig, ResidentModelStatus};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

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
    cache_policy: KvCachePolicy,
}

#[derive(Debug)]
struct ProductHost {
    key: ProductHostKey,
    host: Weak<NativeHost>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProductPhase {
    Running,
    Quiescing,
    Closed,
}

struct ProductRuntimeState {
    phase: ProductPhase,
    host: Option<ProductHost>,
}

/// Sole installation authority held by a product composition root. Runtime
/// modules retain only compatibility access through `PRODUCT_HOST`; they can
/// neither manufacture nor replace the process host identity.
pub struct ProductRuntimeOwner {
    host: Arc<NativeHost>,
}

impl ProductRuntimeOwner {
    pub fn initialize(settings: &Settings) -> anyhow::Result<Self> {
        let key = host_key(settings);
        let mut current = product_host()
            .lock()
            .map_err(|_| anyhow::anyhow!("The native model host is unavailable."))?;
        if current.phase != ProductPhase::Running {
            anyhow::bail!("The product runtime is shutting down.");
        }
        if current.host.is_some() {
            anyhow::bail!("The product native host already has an owner.");
        }
        let host =
            create_product_host(&key).map_err(|error| anyhow::anyhow!(error.message.clone()))?;
        current.host = Some(ProductHost {
            key,
            host: Arc::downgrade(&host),
        });
        Ok(Self { host })
    }

    pub fn host(&self) -> Arc<NativeHost> {
        Arc::clone(&self.host)
    }
}

impl Drop for ProductRuntimeOwner {
    fn drop(&mut self) {
        let _ = shutdown_product_runtime_for_process_exit(&self.host);
    }
}

fn installed_host_for_key(
    runtime: &ProductRuntimeState,
    key: &ProductHostKey,
) -> Result<Option<Arc<NativeHost>>, ()> {
    match runtime.host.as_ref() {
        Some(product) if product.key == *key => Ok(product.host.upgrade()),
        Some(_) => Err(()),
        None => Ok(None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductShutdownError {
    StateUnavailable,
    AlreadyShuttingDown,
    AlreadyClosed,
    HostMissing,
    HostIdentityMismatch,
}

impl std::fmt::Display for ProductShutdownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::StateUnavailable => "the product runtime state is unavailable",
            Self::AlreadyShuttingDown => "the product runtime is already shutting down",
            Self::AlreadyClosed => "the product runtime is already closed",
            Self::HostMissing => "the product runtime has no native host to shut down",
            Self::HostIdentityMismatch => "the product runtime native host identity changed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ProductShutdownError {}

static PRODUCT_HOST: OnceLock<Mutex<ProductRuntimeState>> = OnceLock::new();

fn product_host() -> &'static Mutex<ProductRuntimeState> {
    PRODUCT_HOST.get_or_init(|| {
        Mutex::new(ProductRuntimeState {
            phase: ProductPhase::Running,
            host: None,
        })
    })
}

fn host_key(settings: &Settings) -> ProductHostKey {
    ProductHostKey {
        memory_budget_bytes: settings.resident_memory_budget_bytes,
        max_slots: settings.max_parallel_sequences.clamp(1, 4) as usize,
        data_dir: settings.data_dir.clone(),
        cache_policy: settings.kv_cache_policy,
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
            .get_disposable_cache(&Self::document(namespace))
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

    fn clear(&self, namespace: &str) -> Result<usize, NativeError> {
        let document = Self::document(namespace);
        let entries = self
            .store
            .get_disposable_cache::<Vec<PrefixCacheValue>>(&document)
            .map_err(prefix_store_error)?
            .map_or(0, |values| values.len());
        self.store.delete(&document).map_err(prefix_store_error)?;
        Ok(entries)
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
            cache_policy: host_cache_policy(key.cache_policy),
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

const fn host_cache_policy(policy: KvCachePolicy) -> HostCachePolicy {
    match policy {
        KvCachePolicy::None => HostCachePolicy::Disabled,
        KvCachePolicy::PromptPrefix | KvCachePolicy::KvCacheCandidate => {
            HostCachePolicy::MemoryAndPersistent
        }
    }
}

/// Clears every product-owned prefix tier used by direct native generation and
/// by the embedded gateway. This deliberately works while caching is disabled,
/// so switching the runtime policy off cannot strand an older encrypted
/// checkpoint on disk.
pub fn clear_native_prefix_cache(settings: &Settings) -> anyhow::Result<usize> {
    with_host(settings, NativeHost::clear_cache)
        .map_err(|blocked| anyhow::anyhow!(blocked.blocker.message))
}

fn with_host<T>(
    settings: &Settings,
    operation: impl FnOnce(&NativeHost) -> Result<T, NativeError>,
) -> Result<T, ValidationBlocker> {
    let key = host_key(settings);
    let current = product_host().lock().map_err(|_| {
        native_blocker(
            "native_host_poisoned",
            "The native model host is unavailable.",
        )
    })?;
    if current.phase != ProductPhase::Running {
        return Err(native_blocker(
            "product_shutting_down",
            "The product runtime is shutting down.",
        ));
    }
    match installed_host_for_key(&current, &key) {
        Ok(Some(host)) => operation(&host).map_err(native_error_blocker),
        Ok(None) => Err(native_blocker(
            "product_native_host_uninitialized",
            "The product composition root has not initialized the native host.",
        )),
        Err(()) => Err(native_blocker(
            "product_native_host_identity_locked",
            "Host-level native settings changed; restart Mom Llama to apply them.",
        )),
    }
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
    let config = selected_model_config(&settings)?;
    let key = host_key(&settings);
    let current = product_host()
        .lock()
        .map_err(|_| anyhow::anyhow!("The native model host is unavailable."))?;
    if current.phase != ProductPhase::Running {
        anyhow::bail!("The product runtime is shutting down.");
    }
    match installed_host_for_key(&current, &key) {
        Ok(Some(_)) => {}
        Ok(None) => {
            anyhow::bail!("The product composition root has not initialized the native host.")
        }
        Err(()) => {
            anyhow::bail!("Host-level native settings changed; restart Mom Llama to apply them.")
        }
    }
    let host = current
        .host
        .as_ref()
        .and_then(|product| product.host.upgrade())
        .ok_or_else(|| anyhow::anyhow!("The product native host owner was dropped."))?;
    Ok((host, config))
}

/// Returns a model-only configuration for the exact product host already held
/// by the application composition root. Host-level setting changes require a
/// restart; refresh must never manufacture a second process owner.
pub fn gateway_native_model_configuration(
    expected_host: &Arc<NativeHost>,
) -> anyhow::Result<Option<NativeModelConfig>> {
    let settings = crate::config::resolve_settings()?;
    let expected_key = host_key(&settings);
    let current = product_host()
        .lock()
        .map_err(|_| anyhow::anyhow!("The native model host is unavailable."))?;
    if current.phase != ProductPhase::Running {
        anyhow::bail!("The product runtime is shutting down.");
    }
    let product = current
        .host
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("The product native host is not initialized."))?;
    let installed = product
        .host
        .upgrade()
        .ok_or_else(|| anyhow::anyhow!("The product native host owner was dropped."))?;
    if !Arc::ptr_eq(&installed, expected_host) {
        anyhow::bail!("The product native host identity changed.");
    }
    if product.key != expected_key {
        anyhow::bail!("Host-level native settings changed; restart Mom Llama to apply them.");
    }
    drop(current);
    selected_model_config(&settings)
}

fn selected_model_config(settings: &Settings) -> anyhow::Result<Option<NativeModelConfig>> {
    settings
        .model_path
        .as_deref()
        .map(|model_path| -> anyhow::Result<NativeModelConfig> {
            validate_model_path(model_path)
                .map_err(|blocked| anyhow::anyhow!(blocked.blocker.message))?;
            Ok(model_config(
                settings,
                model_path,
                settings.mmproj_path.as_deref(),
            ))
        })
        .transpose()
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
            .host
            .as_ref()
            .and_then(|product| product.host.upgrade())
            .and_then(|host| host.handle(slot_id))
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
        current.host.as_ref().and_then(|host| {
            host.host
                .upgrade()?
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
                .host
                .as_ref()
                .map(|product| {
                    product
                        .host
                        .upgrade()
                        .into_iter()
                        .flat_map(|host| {
                            host.slots().into_iter().map(|slot| ResidentSlotStatus {
                                slot_id: slot.slot_id,
                                model_path: slot.model_path,
                                model_bytes: slot.model_bytes,
                                reserved_bytes: slot.reserved_bytes,
                                status: slot.status,
                            })
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
        .and_then(|current| current.host.as_ref()?.host.upgrade())
        .map(|host| host.unload(slot_id))
        .unwrap_or(false)
}

pub fn unload_resident_model() -> bool {
    product_host()
        .lock()
        .ok()
        .and_then(|current| current.host.as_ref()?.host.upgrade())
        .map(|host| host.unload_all() > 0)
        .unwrap_or(false)
}

pub fn cancel_native_request(request_id: &str, branch_id: Option<&str>) -> usize {
    product_host()
        .lock()
        .ok()
        .and_then(|current| {
            current
                .host
                .as_ref()
                .and_then(|host| host.host.upgrade())
                .map(|host| host.cancel(request_id, branch_id))
        })
        .unwrap_or_default()
}

pub fn skip_native_reasoning(request_id: &str, branch_id: Option<&str>) -> usize {
    product_host()
        .lock()
        .ok()
        .and_then(|current| {
            current
                .host
                .as_ref()
                .and_then(|host| host.host.upgrade())
                .map(|host| host.skip_reasoning(request_id, branch_id))
        })
        .unwrap_or_default()
}

/// Consumes the product runtime's native-host slot exactly once and joins all
/// native workers outside the global state mutex. This is terminal process
/// shutdown, not an ordinary user-requested model unload.
pub fn shutdown_product_runtime_for_process_exit(
    expected: &Arc<NativeHost>,
) -> Result<ProcessExitJoinedNativeHost, ProductShutdownError> {
    let host = {
        let mut runtime = product_host()
            .lock()
            .map_err(|_| ProductShutdownError::StateUnavailable)?;
        take_product_host(&mut runtime, expected)?
    };

    let joined = host.shutdown_for_process_exit();
    let mut runtime = product_host()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    runtime.phase = ProductPhase::Closed;
    Ok(joined)
}

fn take_product_host(
    runtime: &mut ProductRuntimeState,
    expected: &Arc<NativeHost>,
) -> Result<Arc<NativeHost>, ProductShutdownError> {
    match runtime.phase {
        ProductPhase::Running => runtime.phase = ProductPhase::Quiescing,
        ProductPhase::Quiescing => return Err(ProductShutdownError::AlreadyShuttingDown),
        ProductPhase::Closed => return Err(ProductShutdownError::AlreadyClosed),
    }
    let Some(product) = runtime.host.take() else {
        runtime.phase = ProductPhase::Closed;
        return Err(ProductShutdownError::HostMissing);
    };
    let Some(host) = product.host.upgrade() else {
        runtime.phase = ProductPhase::Closed;
        return Err(ProductShutdownError::HostMissing);
    };
    if !Arc::ptr_eq(&host, expected) {
        runtime.phase = ProductPhase::Closed;
        return Err(ProductShutdownError::HostIdentityMismatch);
    }
    Ok(host)
}

fn native_error_blocker(error: NativeError) -> ValidationBlocker {
    let blocker_code = match error.code {
        NativeErrorCode::MemoryBudgetExceeded => {
            "resident_model_memory_budget_exceeded".to_string()
        }
        NativeErrorCode::ModelSlotsFull => "resident_model_slots_full".to_string(),
        _ => error.code.to_string(),
    };
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
            blocker_code,
            error.message,
            vec!["Check the selected model and native runtime settings.".to_string()],
        ),
    }
}

fn native_blocker(code: &str, message: &str) -> ValidationBlocker {
    ValidationBlocker {
        readiness: "blocked_native_runtime".to_string(),
        blocker: Blocker::new(code, message, vec!["Restart Mom Llama.".to_string()]),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HostCachePolicy, PrefixCacheStore, ProductHost, ProductHostKey, ProductPhase,
        ProductPrefixCacheStore, ProductRuntimeState, ProductShutdownError, create_product_host,
        host_cache_policy, installed_host_for_key, take_product_host,
    };
    use crate::config::KvCachePolicy;
    use llama_native_cache::{CacheFingerprint, CacheTier, PrefixCacheMetadata, PrefixCacheValue};
    use llama_native_host::{NativeHost, NativeHostConfig, memory_reservation};
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

    fn test_product(host: &std::sync::Arc<NativeHost>) -> ProductHost {
        ProductHost {
            key: ProductHostKey {
                memory_budget_bytes: 1,
                max_slots: 1,
                data_dir: std::path::PathBuf::from("test"),
                cache_policy: KvCachePolicy::None,
            },
            host: std::sync::Arc::downgrade(host),
        }
    }

    #[test]
    fn product_host_is_taken_exactly_once_for_terminal_shutdown() {
        let host = std::sync::Arc::new(NativeHost::new(NativeHostConfig::default()));
        let mut runtime = ProductRuntimeState {
            phase: ProductPhase::Running,
            host: Some(test_product(&host)),
        };

        let taken = take_product_host(&mut runtime, &host).expect("take exact host");
        assert!(std::sync::Arc::ptr_eq(&taken, &host));
        assert_eq!(runtime.phase, ProductPhase::Quiescing);
        assert!(runtime.host.is_none());
        assert!(matches!(
            take_product_host(&mut runtime, &host),
            Err(ProductShutdownError::AlreadyShuttingDown)
        ));
    }

    #[test]
    fn product_host_take_fails_closed_on_missing_or_wrong_identity() {
        let expected = std::sync::Arc::new(NativeHost::new(NativeHostConfig::default()));
        let actual = std::sync::Arc::new(NativeHost::new(NativeHostConfig::default()));
        let mut missing = ProductRuntimeState {
            phase: ProductPhase::Running,
            host: None,
        };
        assert!(matches!(
            take_product_host(&mut missing, &expected),
            Err(ProductShutdownError::HostMissing)
        ));
        assert_eq!(missing.phase, ProductPhase::Closed);

        let mut mismatch = ProductRuntimeState {
            phase: ProductPhase::Running,
            host: Some(test_product(&actual)),
        };
        assert!(matches!(
            take_product_host(&mut mismatch, &expected),
            Err(ProductShutdownError::HostIdentityMismatch)
        ));
        assert_eq!(mismatch.phase, ProductPhase::Closed);
        assert!(mismatch.host.is_none());
    }

    #[test]
    fn product_host_key_mismatch_cannot_replace_hidden_compatibility_slot() {
        let original = std::sync::Arc::new(NativeHost::new(NativeHostConfig::default()));
        let runtime = ProductRuntimeState {
            phase: ProductPhase::Running,
            host: Some(test_product(&original)),
        };
        let replacement_key = ProductHostKey {
            memory_budget_bytes: 2,
            max_slots: 1,
            data_dir: std::path::PathBuf::from("test"),
            cache_policy: KvCachePolicy::None,
        };
        assert!(installed_host_for_key(&runtime, &replacement_key).is_err());
        assert!(std::sync::Weak::ptr_eq(
            &runtime.host.as_ref().expect("installed host").host,
            &std::sync::Arc::downgrade(&original)
        ));
    }

    #[test]
    fn compatibility_slot_never_owns_or_recreates_the_native_host() {
        let owner = std::sync::Arc::new(NativeHost::new(NativeHostConfig::default()));
        let key = ProductHostKey {
            memory_budget_bytes: 1,
            max_slots: 1,
            data_dir: std::path::PathBuf::from("test"),
            cache_policy: KvCachePolicy::None,
        };
        let runtime = ProductRuntimeState {
            phase: ProductPhase::Running,
            host: Some(ProductHost {
                key: key.clone(),
                host: std::sync::Arc::downgrade(&owner),
            }),
        };
        assert!(installed_host_for_key(&runtime, &key).is_ok_and(|host| host.is_some()));
        drop(owner);
        assert!(installed_host_for_key(&runtime, &key).is_ok_and(|host| host.is_none()));
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

    #[test]
    fn product_cache_policy_controls_every_native_host_cache_tier() {
        assert_eq!(
            host_cache_policy(KvCachePolicy::None),
            HostCachePolicy::Disabled
        );
        assert_eq!(
            host_cache_policy(KvCachePolicy::PromptPrefix),
            HostCachePolicy::MemoryAndPersistent
        );
        assert_eq!(
            host_cache_policy(KvCachePolicy::KvCacheCandidate),
            HostCachePolicy::MemoryAndPersistent
        );

        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-disabled-host-cache-test-{}",
            uuid::Uuid::new_v4()
        ));
        let key = ProductHostKey {
            memory_budget_bytes: 1024 * 1024 * 1024,
            max_slots: 1,
            data_dir: data_dir.clone(),
            cache_policy: KvCachePolicy::None,
        };
        let host = create_product_host(&key).expect("disabled product host");
        let value = cache_value();
        host.cache_insert(value.clone())
            .expect("disabled insert is a no-op");
        assert!(
            host.cache_lookup(&value.metadata.fingerprint, &value.sequence.token_ids)
                .is_none(),
            "the Off policy must not read from the native host memory tier"
        );
        let store = crate::store::RuntimeStore::open(&data_dir).expect("test encrypted store");
        let persistent = ProductPrefixCacheStore { store };
        assert!(
            persistent
                .load("mom-llama")
                .expect("load disabled persistent tier")
                .is_empty(),
            "the Off policy must not write the native host persistent tier"
        );
        std::fs::remove_dir_all(data_dir).expect("remove test directory");
    }

    #[test]
    fn product_prefix_cache_clear_removes_the_encrypted_document() {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-prefix-cache-clear-test-{}",
            uuid::Uuid::new_v4()
        ));
        let store = crate::store::RuntimeStore::open_with_key(&data_dir, [23_u8; 32])
            .expect("test encrypted store");
        let cache = ProductPrefixCacheStore { store };
        cache
            .save("mom-llama", &cache_value())
            .expect("save prefix");
        assert_eq!(cache.clear("mom-llama").expect("clear prefixes"), 1);
        assert!(
            cache
                .load("mom-llama")
                .expect("load cleared prefixes")
                .is_empty()
        );
        std::fs::remove_dir_all(data_dir).expect("remove test directory");
    }

    #[test]
    fn product_prefix_cache_quarantines_corruption_and_recovers_as_a_cold_cache() {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-prefix-cache-corruption-test-{}",
            uuid::Uuid::new_v4()
        ));
        let store = crate::store::RuntimeStore::open_with_key(&data_dir, [29_u8; 32])
            .expect("test encrypted store");
        let store_path = store.path().to_path_buf();
        let cache = ProductPrefixCacheStore { store };
        let value = cache_value();
        cache.save("mom-llama", &value).expect("save prefix");
        rusqlite::Connection::open(&store_path)
            .expect("open encrypted store")
            .execute(
                "UPDATE encrypted_documents SET ciphertext = X'00'
                 WHERE namespace = 'native-host-prefix-cache.mom-llama'",
                [],
            )
            .expect("corrupt disposable prefix row");

        assert!(
            cache
                .load("mom-llama")
                .expect("corrupt disposable cache must be a miss")
                .is_empty()
        );
        cache
            .save("mom-llama", &value)
            .expect("cold cache must accept a replacement");
        assert_eq!(
            cache.load("mom-llama").expect("load replacement"),
            vec![value]
        );
        let quarantine_count: i64 = rusqlite::Connection::open(&store_path)
            .expect("open encrypted store")
            .query_row(
                "SELECT COUNT(*) FROM encrypted_documents
                 WHERE namespace LIKE 'quarantine.disposable-cache.%'",
                [],
                |row| row.get(0),
            )
            .expect("count quarantined cache rows");
        assert_eq!(quarantine_count, 1);
        std::fs::remove_dir_all(data_dir).expect("remove test directory");
    }
}
