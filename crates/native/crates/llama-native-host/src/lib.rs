//! Owned, product-neutral lifecycle and cache host for in-process llama.cpp.
//!
//! This crate deliberately has no environment lookup, application storage,
//! Keychain integration, networking, process execution, or process-global
//! registry. Applications construct and own a `NativeHost`, inject time and
//! optional persistent cache storage, then route typed requests to its model
//! handles.

use llama_native_cache::{CacheFingerprint, MemoryPrefixCache, PrefixCacheValue};
use llama_native_engine::{GenerationTicket, NativeModelHandle};
use llama_native_types::{
    GenerationRequest, NativeError, NativeErrorCode, NativeModelConfig, NativeModelDescriptor,
    ResidentModelStatus, SharedPrefixBatchRequest,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub trait HostClock: Send + Sync {
    fn now_ms(&self) -> u128;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl HostClock for SystemClock {
    fn now_ms(&self) -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default()
    }
}

/// Optional persistence boundary for encrypted application-owned cache bytes.
///
/// The host never decides how data is encrypted or where it is stored. A
/// product may inject an authenticated implementation; routers may inject a
/// database-backed implementation; tests may inject an in-memory store.
pub trait PrefixCacheStore: Send + Sync {
    fn load(&self, namespace: &str) -> Result<Vec<PrefixCacheValue>, NativeError>;
    fn save(&self, namespace: &str, value: &PrefixCacheValue) -> Result<(), NativeError>;
    fn delete(&self, namespace: &str, id: &str) -> Result<(), NativeError>;

    fn clear(&self, namespace: &str) -> Result<usize, NativeError> {
        let values = self.load(namespace)?;
        for value in &values {
            self.delete(namespace, &value.metadata.id)?;
        }
        Ok(values.len())
    }
}

#[derive(Debug, Clone)]
pub struct NativeHostConfig {
    pub memory_budget_bytes: u64,
    pub max_slots: usize,
    pub memory_cache_bytes: usize,
    pub cache_namespace: String,
    pub cache_policy: HostCachePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostCachePolicy {
    Disabled,
    MemoryOnly,
    MemoryAndPersistent,
}

impl HostCachePolicy {
    const fn allows_memory(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    const fn allows_persistent(self) -> bool {
        matches!(self, Self::MemoryAndPersistent)
    }
}

impl Default for NativeHostConfig {
    fn default() -> Self {
        Self {
            memory_budget_bytes: 12 * 1024 * 1024 * 1024,
            max_slots: 4,
            memory_cache_bytes: 256 * 1024 * 1024,
            cache_namespace: "llama-native-host".to_string(),
            cache_policy: HostCachePolicy::MemoryAndPersistent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSlotStatus {
    pub slot_id: usize,
    pub model_path: PathBuf,
    pub model_bytes: u64,
    pub reserved_bytes: u64,
    pub status: ResidentModelStatus,
}

#[derive(Debug)]
struct ResidentEntry {
    config: NativeModelConfig,
    handle: NativeModelHandle,
    model_bytes: u64,
    reserved_bytes: u64,
}

#[derive(Debug)]
struct HostState {
    slots: BTreeMap<usize, ResidentEntry>,
    cache: MemoryPrefixCache,
}

pub struct NativeHost {
    config: NativeHostConfig,
    state: Mutex<HostState>,
    load_gate: Mutex<()>,
    clock: Arc<dyn HostClock>,
    persistent_cache: Option<Arc<dyn PrefixCacheStore>>,
}

impl std::fmt::Debug for NativeHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeHost")
            .field("config", &self.config)
            .field("persistent_cache", &self.persistent_cache.is_some())
            .finish_non_exhaustive()
    }
}

impl NativeHost {
    #[must_use]
    pub fn new(config: NativeHostConfig) -> Self {
        Self::with_dependencies(config, Arc::new(SystemClock), None)
    }

    #[must_use]
    pub fn with_dependencies(
        config: NativeHostConfig,
        clock: Arc<dyn HostClock>,
        persistent_cache: Option<Arc<dyn PrefixCacheStore>>,
    ) -> Self {
        Self {
            state: Mutex::new(HostState {
                slots: BTreeMap::new(),
                cache: MemoryPrefixCache::new(config.memory_cache_bytes),
            }),
            load_gate: Mutex::new(()),
            config,
            clock,
            persistent_cache,
        }
    }

    pub fn acquire(&self, model: NativeModelConfig) -> Result<NativeModelHandle, NativeError> {
        if let Some(existing) = self.resident_handle(&model)? {
            return Ok(existing);
        }
        self.with_load_gate(|| {
            if let Some(existing) = self.resident_handle(&model)? {
                return Ok(existing);
            }
            let slot_id = {
                let state = self.state.lock().map_err(host_poisoned)?;
                (0..self.config.max_slots)
                    .find(|slot| !state.slots.contains_key(slot))
                    .ok_or_else(|| {
                        NativeError::new(
                            NativeErrorCode::ModelSlotsFull,
                            "all configured resident model slots are occupied",
                        )
                    })?
            };
            self.load_into_slot_inner(slot_id, model)
        })
    }

    pub fn load_into_slot(
        &self,
        slot_id: usize,
        model: NativeModelConfig,
    ) -> Result<NativeModelHandle, NativeError> {
        self.with_load_gate(|| self.load_into_slot_inner(slot_id, model))
    }

    fn load_into_slot_inner(
        &self,
        slot_id: usize,
        model: NativeModelConfig,
    ) -> Result<NativeModelHandle, NativeError> {
        if slot_id >= self.config.max_slots {
            return Err(NativeError::new(
                NativeErrorCode::InvalidConfig,
                format!("slot {slot_id} is outside the configured host bound"),
            ));
        }
        let model_bytes = std::fs::metadata(&model.model_path)
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::ModelMissing,
                    format!("failed to inspect model size: {error}"),
                )
            })?
            .len();
        let projector_bytes = model
            .mmproj_path
            .as_ref()
            .map(std::fs::metadata)
            .transpose()
            .map_err(|error| {
                NativeError::new(
                    NativeErrorCode::ModelMissing,
                    format!("failed to inspect multimodal projector size: {error}"),
                )
            })?
            .map(|metadata| metadata.len())
            .unwrap_or_default();
        let reserved_bytes = memory_reservation(model_bytes, projector_bytes);
        {
            let state = self.state.lock().map_err(host_poisoned)?;
            if let Some(existing) = state.slots.get(&slot_id)
                && existing.config == model
            {
                return Ok(existing.handle.clone());
            }
            let used = state
                .slots
                .iter()
                .filter(|(candidate, _)| **candidate != slot_id)
                .map(|(_, entry)| entry.reserved_bytes)
                .sum::<u64>();
            if used.saturating_add(reserved_bytes) > self.config.memory_budget_bytes {
                return Err(NativeError::new(
                    NativeErrorCode::MemoryBudgetExceeded,
                    format!(
                        "loading the model would reserve {} bytes, above the {} byte host budget",
                        used.saturating_add(reserved_bytes),
                        self.config.memory_budget_bytes
                    ),
                ));
            }
        }
        let handle = NativeModelHandle::load(model.clone())?;
        let mut state = self.state.lock().map_err(host_poisoned)?;
        state.slots.insert(
            slot_id,
            ResidentEntry {
                config: model,
                handle: handle.clone(),
                model_bytes,
                reserved_bytes,
            },
        );
        Ok(handle)
    }

    fn resident_handle(
        &self,
        model: &NativeModelConfig,
    ) -> Result<Option<NativeModelHandle>, NativeError> {
        Ok(self
            .state
            .lock()
            .map_err(host_poisoned)?
            .slots
            .values()
            .find(|entry| &entry.config == model)
            .map(|entry| entry.handle.clone()))
    }

    fn with_load_gate<T>(
        &self,
        operation: impl FnOnce() -> Result<T, NativeError>,
    ) -> Result<T, NativeError> {
        let _guard = self.load_gate.lock().map_err(host_poisoned)?;
        operation()
    }

    pub fn generate(
        &self,
        model: NativeModelConfig,
        request: GenerationRequest,
    ) -> Result<GenerationTicket, NativeError> {
        self.acquire(model)?.generate(request)
    }

    pub fn generate_shared_prefix(
        &self,
        model: NativeModelConfig,
        request: SharedPrefixBatchRequest,
    ) -> Result<GenerationTicket, NativeError> {
        self.acquire(model)?.generate_shared_prefix(request)
    }

    pub fn cancel(&self, request_id: &str, branch_id: Option<&str>) -> usize {
        self.state
            .lock()
            .map(|state| {
                state
                    .slots
                    .values()
                    .map(|entry| entry.handle.cancel(request_id, branch_id))
                    .sum()
            })
            .unwrap_or_default()
    }

    pub fn skip_reasoning(&self, request_id: &str, branch_id: Option<&str>) -> usize {
        self.state
            .lock()
            .map(|state| {
                state
                    .slots
                    .values()
                    .map(|entry| entry.handle.skip_reasoning(request_id, branch_id))
                    .sum()
            })
            .unwrap_or_default()
    }

    pub fn descriptors(&self) -> Vec<NativeModelDescriptor> {
        self.state
            .lock()
            .map(|state| {
                state
                    .slots
                    .values()
                    .filter_map(|entry| entry.handle.status().descriptor)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn slots(&self) -> Vec<HostSlotStatus> {
        self.state
            .lock()
            .map(|state| {
                state
                    .slots
                    .iter()
                    .map(|(slot_id, entry)| HostSlotStatus {
                        slot_id: *slot_id,
                        model_path: entry.config.model_path.clone(),
                        model_bytes: entry.model_bytes,
                        reserved_bytes: entry.reserved_bytes,
                        status: entry.handle.status(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn handle(&self, slot_id: usize) -> Option<NativeModelHandle> {
        self.state
            .lock()
            .ok()?
            .slots
            .get(&slot_id)
            .map(|entry| entry.handle.clone())
    }

    pub fn unload(&self, slot_id: usize) -> bool {
        self.state
            .lock()
            .map(|mut state| state.slots.remove(&slot_id).is_some())
            .unwrap_or(false)
    }

    pub fn unload_all(&self) -> usize {
        self.state
            .lock()
            .map(|mut state| {
                let count = state.slots.len();
                state.slots.clear();
                count
            })
            .unwrap_or_default()
    }

    pub fn cache_lookup(
        &self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
    ) -> Option<PrefixCacheValue> {
        if !self.config.cache_policy.allows_memory() {
            return None;
        }
        let now = self.clock.now_ms();
        self.state
            .lock()
            .ok()?
            .cache
            .lookup(fingerprint, prompt_token_ids, now)
    }

    pub fn cache_lookup_for_owner(
        &self,
        fingerprint: &CacheFingerprint,
        prompt_token_ids: &[i32],
        owner_id: &str,
    ) -> Option<PrefixCacheValue> {
        if !self.config.cache_policy.allows_memory() {
            return None;
        }
        let now = self.clock.now_ms();
        let mut state = self.state.lock().ok()?;
        let matched = state
            .cache
            .best_match_for_owner(fingerprint, prompt_token_ids, owner_id)?;
        state.cache.get(&matched.id, now)
    }

    pub fn cache_insert(&self, value: PrefixCacheValue) -> Result<Vec<String>, NativeError> {
        if !self.config.cache_policy.allows_memory() {
            return Ok(Vec::new());
        }
        let evicted = self
            .state
            .lock()
            .map_err(host_poisoned)?
            .cache
            .insert(value.clone());
        if self.config.cache_policy.allows_persistent()
            && let Some(store) = &self.persistent_cache
        {
            store.save(&self.config.cache_namespace, &value)?;
        }
        Ok(evicted)
    }

    pub fn restore_persistent_cache(&self) -> Result<usize, NativeError> {
        if !self.config.cache_policy.allows_persistent() {
            return Ok(0);
        }
        let Some(store) = &self.persistent_cache else {
            return Ok(0);
        };
        let values = store.load(&self.config.cache_namespace)?;
        let mut state = self.state.lock().map_err(host_poisoned)?;
        let mut restored = 0;
        for value in values {
            if value.is_valid() {
                state.cache.insert(value);
                restored += 1;
            }
        }
        Ok(restored)
    }

    /// Clears both host-owned prefix tiers for this namespace.
    ///
    /// Persistent deletion is attempted even when lookup is currently disabled,
    /// so a runtime policy change cannot strand reusable state behind an "off"
    /// switch.
    pub fn clear_cache(&self) -> Result<usize, NativeError> {
        let memory_entries = {
            let mut state = self.state.lock().map_err(host_poisoned)?;
            let entries = state.cache.len();
            state.cache.clear();
            entries
        };
        let Some(store) = &self.persistent_cache else {
            return Ok(memory_entries);
        };
        let persistent_entries = store.clear(&self.config.cache_namespace)?;
        Ok(memory_entries.saturating_add(persistent_entries))
    }
}

fn host_poisoned<T>(_error: std::sync::PoisonError<T>) -> NativeError {
    NativeError::new(NativeErrorCode::Internal, "native host state is poisoned")
}

#[must_use]
pub const fn memory_reservation(model_bytes: u64, projector_bytes: u64) -> u64 {
    const MINIMUM_RUNTIME_RESERVE: u64 = 384 * 1024 * 1024;
    let runtime = if model_bytes / 2 > MINIMUM_RUNTIME_RESERVE {
        model_bytes / 2
    } else {
        MINIMUM_RUNTIME_RESERVE
    };
    model_bytes
        .saturating_add(projector_bytes)
        .saturating_add(runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use llama_native_cache::{CacheFingerprint, CacheTier, PrefixCacheMetadata};
    use llama_native_types::{PromptForm, PromptTokenPolicy, SequenceStateBlob};
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct TestPrefixStore {
        values: Mutex<Vec<PrefixCacheValue>>,
    }

    impl PrefixCacheStore for TestPrefixStore {
        fn load(&self, _namespace: &str) -> Result<Vec<PrefixCacheValue>, NativeError> {
            Ok(self.values.lock().expect("test store lock").clone())
        }

        fn save(&self, _namespace: &str, value: &PrefixCacheValue) -> Result<(), NativeError> {
            let mut values = self.values.lock().expect("test store lock");
            values.retain(|candidate| candidate.metadata.id != value.metadata.id);
            values.push(value.clone());
            Ok(())
        }

        fn delete(&self, _namespace: &str, id: &str) -> Result<(), NativeError> {
            self.values
                .lock()
                .expect("test store lock")
                .retain(|candidate| candidate.metadata.id != id);
            Ok(())
        }
    }

    fn cache_value(id: &str, token: i32) -> PrefixCacheValue {
        let fingerprint = CacheFingerprint {
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
        };
        PrefixCacheValue {
            metadata: PrefixCacheMetadata::new(
                id,
                CacheTier::SessionPersistent,
                fingerprint,
                vec![token],
                8,
                1,
            ),
            sequence: SequenceStateBlob {
                sequence_id: 0,
                token_count: 1,
                bytes: vec![token as u8; 8],
                token_ids: vec![token],
            },
        }
    }

    #[test]
    fn hosts_are_owned_and_do_not_share_registry_state() {
        let left = NativeHost::new(NativeHostConfig::default());
        let right = NativeHost::new(NativeHostConfig::default());
        assert!(left.slots().is_empty());
        assert!(right.slots().is_empty());
        assert_eq!(left.unload_all(), 0);
        assert_eq!(right.unload_all(), 0);
    }

    #[test]
    fn memory_reservation_is_bounded_and_includes_projector() {
        let mib = 1024 * 1024;
        assert_eq!(memory_reservation(100 * mib, 0), 484 * mib);
        assert_eq!(
            memory_reservation(4 * 1024 * mib, 500 * mib),
            6 * 1024 * mib + 500 * mib
        );
    }

    #[test]
    fn memory_lru_eviction_does_not_delete_the_persistent_tier() {
        let store = Arc::new(TestPrefixStore::default());
        let host = NativeHost::with_dependencies(
            NativeHostConfig {
                memory_cache_bytes: 8,
                ..NativeHostConfig::default()
            },
            Arc::new(SystemClock),
            Some(store.clone()),
        );
        assert!(host.cache_insert(cache_value("first", 1)).is_ok());
        assert_eq!(
            host.cache_insert(cache_value("second", 2))
                .expect("second insert"),
            vec!["first".to_string()]
        );
        assert_eq!(
            store
                .load("llama-native-host")
                .expect("persistent load")
                .len(),
            2
        );
    }

    #[test]
    fn load_gate_serializes_first_load_critical_sections() {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let start = Arc::new(Barrier::new(9));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let workers = (0..8)
            .map(|_| {
                let host = Arc::clone(&host);
                let start = Arc::clone(&start);
                let active = Arc::clone(&active);
                let maximum = Arc::clone(&maximum);
                thread::spawn(move || {
                    start.wait();
                    host.with_load_gate(|| {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(current, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(5));
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .expect("load gate");
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        for worker in workers {
            worker.join().expect("load worker");
        }
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn disabled_cache_neither_reads_writes_nor_restores_but_clear_still_deletes() {
        let store = Arc::new(TestPrefixStore::default());
        let host = NativeHost::with_dependencies(
            NativeHostConfig {
                cache_policy: HostCachePolicy::Disabled,
                ..NativeHostConfig::default()
            },
            Arc::new(SystemClock),
            Some(store.clone()),
        );
        let value = cache_value("disabled", 1);
        assert!(
            host.cache_insert(value.clone())
                .expect("disabled insert")
                .is_empty()
        );
        assert!(
            host.cache_lookup(&value.metadata.fingerprint, &[1, 2])
                .is_none()
        );
        assert!(
            store
                .load("llama-native-host")
                .expect("store read")
                .is_empty()
        );

        store
            .save("llama-native-host", &value)
            .expect("seed prior persistent value");
        assert_eq!(
            host.restore_persistent_cache().expect("disabled restore"),
            0
        );
        assert_eq!(host.clear_cache().expect("disabled clear"), 1);
        assert!(
            store
                .load("llama-native-host")
                .expect("store read")
                .is_empty()
        );
    }

    #[test]
    fn memory_only_cache_never_writes_or_restores_persistent_values() {
        let store = Arc::new(TestPrefixStore::default());
        let host = NativeHost::with_dependencies(
            NativeHostConfig {
                cache_policy: HostCachePolicy::MemoryOnly,
                ..NativeHostConfig::default()
            },
            Arc::new(SystemClock),
            Some(store.clone()),
        );
        let value = cache_value("memory", 1);
        host.cache_insert(value.clone()).expect("memory insert");
        assert!(
            host.cache_lookup(&value.metadata.fingerprint, &[1, 2])
                .is_some()
        );
        assert!(
            store
                .load("llama-native-host")
                .expect("store read")
                .is_empty()
        );

        store
            .save("llama-native-host", &cache_value("persistent", 2))
            .expect("seed persistent value");
        assert_eq!(
            host.restore_persistent_cache()
                .expect("memory-only restore"),
            0
        );
        assert_eq!(host.clear_cache().expect("memory-only clear"), 2);
        assert!(
            host.cache_lookup(&value.metadata.fingerprint, &[1, 2])
                .is_none()
        );
        assert!(
            store
                .load("llama-native-host")
                .expect("store read")
                .is_empty()
        );
    }
}
