use crate::OperationScope;
use crate::config::{KvCachePolicy, Settings};
use crate::engine::{ValidationBlocker, validate_model_path};
use crate::receipts::Blocker;
use crate::store::{DocumentMutations, DocumentSnapshot};
use llama_native_cache::{PrefixCacheMetadata, PrefixCacheValue};
use llama_native_engine::NativeModelHandle;
use llama_native_host::{
    HostCachePolicy, NativeHost, NativeHostConfig, PrefixCachePromotionLease, PrefixCacheStore,
    SystemClock,
};
use llama_native_types::{NativeError, NativeErrorCode, NativeModelConfig, ResidentModelStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
const PRODUCT_PREFIX_CACHE_NAMESPACE: &str = "mom-llama";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProductHostKey {
    memory_budget_bytes: u64,
    max_slots: usize,
    data_dir: PathBuf,
    cache_policy: KvCachePolicy,
}

/// Composition-root owner. Every caller receives its explicit operation scope;
/// dropping this owner cancels that scope and joins this host's workers.
pub struct ProductRuntimeOwner {
    host: Arc<NativeHost>,
    scope: OperationScope,
}

impl ProductRuntimeOwner {
    pub fn initialize(settings: &Settings) -> anyhow::Result<Self> {
        Self::from_key(host_key(settings)).map_err(|error| anyhow::anyhow!(error.message))
    }

    fn from_key(key: ProductHostKey) -> Result<Self, NativeError> {
        let host = create_product_host(&key)?;
        let scope = OperationScope::for_product_host(&host, key);
        Ok(Self { host, scope })
    }

    pub fn host(&self) -> Arc<NativeHost> {
        Arc::clone(&self.host)
    }

    pub fn operation_scope(&self) -> OperationScope {
        self.scope.clone()
    }
}

impl Drop for ProductRuntimeOwner {
    fn drop(&mut self) {
        let _ = self.scope.request_cancellation();
        let _joined = self.host.shutdown_for_process_exit();
    }
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

struct ProductPrefixCachePromotionLease {
    lease: crate::personas::PersonaCacheOwnerLease,
}

impl PrefixCachePromotionLease for ProductPrefixCachePromotionLease {
    fn owner_generation(&self) -> u64 {
        self.lease.owner_generation()
    }

    fn validate(&self) -> Result<(), NativeError> {
        match self.lease.validate().map_err(prefix_store_error)? {
            true => Ok(()),
            false => Err(NativeError::new(
                NativeErrorCode::CacheIncompatible,
                "Persona cache owner generation changed before live promotion",
            )),
        }
    }
}

impl ProductPrefixCacheStore {
    fn document(namespace: &str) -> String {
        format!("native-host-prefix-cache.{namespace}")
    }

    fn entry(namespace: &str, id: &str) -> String {
        format!(
            "{}.entry.{:x}",
            Self::document(namespace),
            Sha256::digest(id.as_bytes())
        )
    }
}

pub(crate) fn persona_native_cache_ids_from_snapshot(
    snapshot: &DocumentSnapshot<'_, '_, '_>,
    owner_id: &str,
) -> anyhow::Result<Vec<String>> {
    let values = snapshot
        .get::<Vec<PrefixCacheMetadata>>(&ProductPrefixCacheStore::document(
            PRODUCT_PREFIX_CACHE_NAMESPACE,
        ))?
        .unwrap_or_default();
    Ok(prefix_cache_ids_for_owner(&values, owner_id))
}

pub(crate) fn persona_native_cache_ids_from_documents(
    documents: &DocumentMutations<'_, '_, '_>,
    owner_id: &str,
) -> anyhow::Result<Vec<String>> {
    let values = documents
        .get::<Vec<PrefixCacheMetadata>>(&ProductPrefixCacheStore::document(
            PRODUCT_PREFIX_CACHE_NAMESPACE,
        ))?
        .unwrap_or_default();
    Ok(prefix_cache_ids_for_owner(&values, owner_id))
}

pub(crate) fn remove_persona_native_cache_from_documents(
    documents: &mut DocumentMutations<'_, '_, '_>,
    owner_id: &str,
) -> anyhow::Result<Vec<String>> {
    let namespace = ProductPrefixCacheStore::document(PRODUCT_PREFIX_CACHE_NAMESPACE);
    let mut values = documents
        .get::<Vec<PrefixCacheMetadata>>(&namespace)?
        .unwrap_or_default();
    let removed = prefix_cache_ids_for_owner(&values, owner_id);
    for id in &removed {
        documents.delete(&ProductPrefixCacheStore::entry(
            PRODUCT_PREFIX_CACHE_NAMESPACE,
            id,
        ));
    }
    values.retain(|value| value.owner_id.as_deref() != Some(owner_id));
    documents.put_bytes(&namespace, &serde_json::to_vec(&values)?)?;
    Ok(removed)
}

fn prefix_cache_ids_for_owner(values: &[PrefixCacheMetadata], owner_id: &str) -> Vec<String> {
    let mut ids = values
        .iter()
        .filter(|value| value.owner_id.as_deref() == Some(owner_id))
        .map(|value| value.id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

pub(crate) fn invalidate_loaded_native_cache_owner(
    scope: &OperationScope,
    owner_id: &str,
) -> anyhow::Result<usize> {
    let Some(host) = scope.native_host() else {
        return Ok(0);
    };
    host.invalidate_live_cache_owner(owner_id)
        .map_err(|error| anyhow::anyhow!(error.message))
}

impl PrefixCacheStore for ProductPrefixCacheStore {
    fn list(&self, namespace: &str) -> Result<Vec<PrefixCacheMetadata>, NativeError> {
        self.store
            .get_disposable_cache_with_members(
                &Self::document(namespace),
                Some(&format!("{}.entry.", Self::document(namespace))),
            )
            .map(|values| values.unwrap_or_default())
            .map_err(prefix_store_error)
    }

    fn load_entry(
        &self,
        namespace: &str,
        id: &str,
    ) -> Result<Option<PrefixCacheValue>, NativeError> {
        self.store
            .get_disposable_cache(&Self::entry(namespace, id))
            .map_err(prefix_store_error)
    }

    fn save(&self, namespace: &str, value: &PrefixCacheValue) -> Result<(), NativeError> {
        if !value.is_valid() {
            return Err(NativeError::new(
                NativeErrorCode::CacheIncompatible,
                "refusing an invalid persistent prefix",
            ));
        }
        self.store.mutate_documents(&Self::document(namespace), Vec::<PrefixCacheMetadata>::new, |entries, documents| {
            if let Some(owner_id) = value.metadata.owner_id.as_deref()
                && crate::personas::persona_cache_owner_is_removed_from_documents(documents, owner_id)?
            { anyhow::bail!("refusing to restore cache authority for removed Persona owner {owner_id}"); }
            entries.retain(|entry| entry.id != value.metadata.id);
            entries.push(value.metadata.clone());
            entries.sort_by_key(|entry| entry.last_used_at_ms);
            while entries.len() > PERSISTENT_PREFIX_CACHE_MAX_ENTRIES
                || entries.iter().try_fold(0_usize, |total, entry| total.checked_add(entry.state_bytes))
                    .is_none_or(|bytes| bytes > PERSISTENT_PREFIX_CACHE_MAX_BYTES) {
                let removed = entries.remove(0);
                documents.delete(&Self::entry(namespace, &removed.id));
            }
            if entries.iter().any(|entry| entry.id == value.metadata.id) {
                documents.put_bytes(&Self::entry(namespace, &value.metadata.id), &serde_json::to_vec(value)?)?;
            }
            Ok(())
        }).map_err(prefix_store_error)
    }

    fn acquire_owner_promotion_lease(
        &self,
        _namespace: &str,
        owner_id: &str,
    ) -> Result<Option<Box<dyn PrefixCachePromotionLease>>, NativeError> {
        crate::personas::acquire_persona_cache_owner_lease(&self.store, owner_id)
            .map(|lease| {
                lease.map(|lease| {
                    Box::new(ProductPrefixCachePromotionLease { lease })
                        as Box<dyn PrefixCachePromotionLease>
                })
            })
            .map_err(prefix_store_error)
    }

    fn delete(&self, namespace: &str, id: &str) -> Result<(), NativeError> {
        self.store
            .mutate_documents(
                &Self::document(namespace),
                Vec::<PrefixCacheMetadata>::new,
                |entries, documents| {
                    entries.retain(|entry| entry.id != id);
                    documents.delete(&Self::entry(namespace, id));
                    Ok(())
                },
            )
            .map_err(prefix_store_error)
    }

    fn clear(&self, namespace: &str) -> Result<usize, NativeError> {
        self.store
            .clear_cache_family(
                &Self::document(namespace),
                &format!("{}.entry.", Self::document(namespace)),
            )
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
            cache_namespace: PRODUCT_PREFIX_CACHE_NAMESPACE.to_string(),
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
pub fn clear_native_prefix_cache(
    scope: &crate::OperationScope,
    settings: &Settings,
) -> anyhow::Result<usize> {
    with_host(scope, settings, NativeHost::clear_cache)
        .map_err(|blocked| anyhow::anyhow!(blocked.blocker.message))
}

fn with_host<T>(
    scope: &OperationScope,
    settings: &Settings,
    operation: impl FnOnce(&NativeHost) -> Result<T, NativeError>,
) -> Result<T, ValidationBlocker> {
    if !scope.matches_native_key(&host_key(settings)) {
        return Err(native_blocker(
            "product_native_host_identity_locked",
            "Host-level native settings changed; restart Mom Llama to apply them.",
        ));
    }
    let host = scope.native_host().ok_or_else(|| {
        native_blocker(
            "product_native_host_unavailable",
            "This operation scope has no running native host.",
        )
    })?;
    operation(&host).map_err(native_error_blocker)
}

pub(crate) fn model_configuration_for_profile(
    settings: &Settings,
    model_path: &Path,
    mmproj_path: Option<&Path>,
) -> Result<NativeModelConfig, ValidationBlocker> {
    validate_model_path(model_path)?;
    let mut config = NativeModelConfig::local(model_path.to_path_buf());
    config.device = settings.native_device;
    config.context_tokens = settings.context_tokens;
    config.batch_tokens = settings.batch_tokens;
    config.max_sequences = settings.max_parallel_sequences.clamp(1, 4);
    // This boundary consumes an exact profile. In particular, a frozen/imported
    // `None` must remain `None` if a sibling projector appears later. Ordinary
    // model selection resolves and persists its pair before reaching the host.
    config.mmproj_path = mmproj_path
        .filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_path_buf);
    Ok(config)
}

pub fn resident_model(
    scope: &crate::OperationScope,
    settings: &Settings,
) -> Result<NativeModelHandle, ValidationBlocker> {
    resident_model_for_slot(scope, settings, 0, settings.model_path.as_deref())
}

pub fn resident_model_for_profile(
    scope: &crate::OperationScope,
    settings: &Settings,
    model_path: &Path,
    mmproj_path: Option<&Path>,
) -> Result<NativeModelHandle, ValidationBlocker> {
    let config = model_configuration_for_profile(settings, model_path, mmproj_path)?;
    resident_model_for_configuration(scope, settings, &config)
}

/// Resolves the exact profile only when its worker is already resident. This
/// boundary never loads a model and is used by speculative product work that
/// must disappear rather than compete for residency.
pub(crate) fn resident_model_for_profile_if_loaded(
    scope: &crate::OperationScope,
    settings: &Settings,
    model_path: &Path,
    mmproj_path: Option<&Path>,
) -> Result<Option<NativeModelHandle>, ValidationBlocker> {
    let config = model_configuration_for_profile(settings, model_path, mmproj_path)?;
    validate_model_path(&config.model_path)?;
    with_host(scope, settings, |host| host.resident(&config))
}

pub(crate) fn resident_model_for_configuration(
    scope: &crate::OperationScope,
    settings: &Settings,
    config: &NativeModelConfig,
) -> Result<NativeModelHandle, ValidationBlocker> {
    validate_model_path(&config.model_path)?;
    with_host(scope, settings, |host| host.acquire(config.clone()))
}

/// Reuses the exact frozen resident identity or reloads only the exact private
/// configuration captured before an approval was made durable. Current model
/// tuning settings are intentionally ignored; only the AppRuntime host identity
/// and its process-level ownership policy come from `settings`.
pub(crate) fn resident_model_for_frozen_config(
    scope: &crate::OperationScope,
    settings: &Settings,
    config: &NativeModelConfig,
    expected: &llama_native_types::ModelFingerprint,
) -> Result<NativeModelHandle, ValidationBlocker> {
    validate_model_path(&config.model_path)?;
    with_host(scope, settings, |host| {
        if let Some(slot_id) = host
            .slots()
            .into_iter()
            .find(|slot| slot.status.fingerprint.as_ref() == Some(expected))
            .map(|slot| slot.slot_id)
        {
            return host.handle(slot_id).ok_or_else(|| {
                NativeError::new(
                    NativeErrorCode::ModelMissing,
                    "the exact frozen Persona model handle disappeared",
                )
            });
        }
        let handle = host.acquire(config.clone())?;
        if handle.status().fingerprint.as_ref() != Some(expected) {
            return Err(NativeError::new(
                NativeErrorCode::ModelInvalid,
                "the frozen Persona model configuration no longer resolves to its exact fingerprint",
            ));
        }
        Ok(handle)
    })
}

/// Returns only an already-resident model with the exact immutable identity.
/// Approval resumption must never start an uncancellable model load after an
/// external-effect intent has been durably consumed.
pub fn resident_model_for_fingerprint(
    scope: &crate::OperationScope,
    settings: &Settings,
    expected: &llama_native_types::ModelFingerprint,
) -> Result<NativeModelHandle, ValidationBlocker> {
    with_host(scope, settings, |host| {
        let slot_id = host
            .slots()
            .into_iter()
            .find(|slot| slot.status.fingerprint.as_ref() == Some(expected))
            .map(|slot| slot.slot_id)
            .ok_or_else(|| {
                NativeError::new(
                    NativeErrorCode::ModelMissing,
                    "the exact frozen Persona model is no longer resident",
                )
            })?;
        host.handle(slot_id).ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::ModelMissing,
                "the exact frozen Persona model handle is no longer available",
            )
        })
    })
}

pub fn resident_model_for_slot(
    scope: &crate::OperationScope,
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
        if let Some(handle) = with_host(scope, settings, |host| Ok(host.handle(slot_id)))? {
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
    let config =
        model_configuration_for_profile(settings, model_path, settings.mmproj_path.as_deref())?;
    with_host(scope, settings, |host| host.load_into_slot(slot_id, config))
}

pub fn resident_status(scope: &OperationScope) -> Option<ResidentModelStatus> {
    scope
        .native_host()?
        .slots()
        .into_iter()
        .find(|slot| slot.slot_id == 0)
        .map(|slot| slot.status)
}

pub fn resident_slots(scope: &OperationScope) -> Vec<ResidentSlotStatus> {
    scope
        .native_host()
        .into_iter()
        .flat_map(|host| host.slots())
        .map(|slot| ResidentSlotStatus {
            slot_id: slot.slot_id,
            model_path: slot.model_path,
            model_bytes: slot.model_bytes,
            reserved_bytes: slot.reserved_bytes,
            status: slot.status,
        })
        .collect()
}

pub fn unload_resident_slot(scope: &OperationScope, slot_id: usize) -> bool {
    scope.native_host().is_some_and(|host| host.unload(slot_id))
}

pub fn unload_resident_model(scope: &OperationScope) -> bool {
    scope
        .native_host()
        .is_some_and(|host| host.unload_all() > 0)
}

pub fn cancel_native_request(
    scope: &OperationScope,
    request_id: &str,
    branch_id: Option<&str>,
) -> usize {
    scope.cancel_native(request_id, branch_id)
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
        HostCachePolicy, PrefixCacheStore, ProductHostKey, ProductPrefixCacheStore,
        ProductRuntimeOwner, create_product_host, host_cache_policy,
    };
    use crate::config::KvCachePolicy;
    use llama_native_cache::{CacheFingerprint, CacheTier, PrefixCacheMetadata, PrefixCacheValue};
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
    fn product_owners_have_independent_scopes_and_terminal_drop() {
        let first_dir =
            std::env::temp_dir().join(format!("mom-owner-first-{}", uuid::Uuid::new_v4()));
        let second_dir =
            std::env::temp_dir().join(format!("mom-owner-second-{}", uuid::Uuid::new_v4()));
        let key = |data_dir| ProductHostKey {
            memory_budget_bytes: 1024 * 1024 * 1024,
            max_slots: 1,
            data_dir,
            cache_policy: KvCachePolicy::None,
        };
        let first = ProductRuntimeOwner::from_key(key(first_dir.clone())).expect("first owner");
        let second = ProductRuntimeOwner::from_key(key(second_dir.clone())).expect("second owner");
        let first_scope = first.operation_scope();
        let second_scope = second.operation_scope();
        assert!(!std::sync::Arc::ptr_eq(&first.host(), &second.host()));
        assert!(first_scope.matches_native_key(&key(first_dir.clone())));
        assert!(!first_scope.matches_native_key(&key(second_dir.clone())));
        drop(first);
        assert!(first_scope.native_host().is_none());
        assert!(second_scope.native_host().is_some());
        second_scope
            .native_host()
            .expect("second remains live")
            .clear_cache()
            .expect("second cache");
        drop(second);
        assert!(second_scope.native_host().is_none());
        std::fs::remove_dir_all(first_dir).expect("first cleanup");
        std::fs::remove_dir_all(second_dir).expect("second cleanup");
    }

    #[test]
    fn changing_one_prefix_keeps_other_encrypted_payload_bytes_unchanged() {
        let dir =
            std::env::temp_dir().join(format!("mom-prefix-entry-cost-{}", uuid::Uuid::new_v4()));
        let store = crate::store::RuntimeStore::open_with_key(&dir, [19; 32]).expect("store");
        let path = store.path().to_path_buf();
        let cache = ProductPrefixCacheStore { store };
        let mut first = cache_value();
        let mut second = first.clone();
        second.metadata.id = "second".into();
        cache.save("cost", &first).expect("first");
        cache.save("cost", &second).expect("second");
        let snapshot = || {
            let db = rusqlite::Connection::open(&path).expect("db");
            let mut query = db
                .prepare("SELECT namespace, ciphertext FROM encrypted_documents ORDER BY namespace")
                .expect("query");
            query
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .expect("rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("snapshot")
        };
        let before = snapshot();
        assert_eq!(
            before.len(),
            3,
            "one small index and two independently encrypted entries"
        );
        first.metadata.last_used_at_ms += 1;
        cache.save("cost", &first).expect("update");
        let after = snapshot();
        assert_eq!(
            before
                .iter()
                .zip(&after)
                .filter(|(old, new)| old == new)
                .count(),
            1,
            "the unrelated encrypted payload must not be rewritten"
        );
        assert_eq!(
            cache.load_entry("cost", "second").expect("exact read"),
            Some(second)
        );
        assert_eq!(cache.clear("cost").expect("clear"), 2);
        assert!(
            snapshot().is_empty(),
            "clear atomically removes metadata and every payload"
        );
        std::fs::remove_dir_all(dir).expect("cleanup");
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
