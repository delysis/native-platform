//! Owned, product-neutral lifecycle and cache host for in-process llama.cpp.
//!
//! This crate deliberately has no environment lookup, application storage,
//! Keychain integration, networking, process execution, or process-global
//! registry. Applications construct and own a `NativeHost`, inject time and
//! optional persistent cache storage, then route typed requests to its model
//! handles.

use llama_native_cache::{CacheFingerprint, MemoryPrefixCache, PrefixCacheValue};
use llama_native_engine::{
    GenerationTicket, JoinedNativeModel, NativeModelHandle, NativeModelOwner,
};
use llama_native_types::{
    GenerationBatchRequest, GenerationRequest, ModelFingerprint, NativeDevice, NativeError,
    NativeErrorCode, NativeModelConfig, NativeModelDescriptor, ResidentModelStatus,
    SharedPrefixBatchRequest,
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

#[derive(Clone, PartialEq, Eq)]
pub struct HostSlotStatus {
    pub slot_id: usize,
    pub model_path: PathBuf,
    pub model_bytes: u64,
    pub reserved_bytes: u64,
    pub status: ResidentModelStatus,
}

/// Linear evidence that one host slot's native owner thread was joined.
#[derive(Debug)]
pub struct JoinedHostSlot {
    host_identity: Arc<HostIdentity>,
    slot_id: usize,
    worker: JoinedNativeModel,
}

impl JoinedHostSlot {
    #[must_use]
    pub const fn slot_id(&self) -> usize {
        self.slot_id
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        self.worker.model_id()
    }

    #[must_use]
    pub fn belongs_to(&self, host: &NativeHost) -> bool {
        Arc::ptr_eq(&self.host_identity, &host.identity)
    }

    #[must_use]
    pub fn worker_belongs_to(&self, handle: &NativeModelHandle) -> bool {
        self.worker.belongs_to(handle)
    }
}

#[derive(Debug)]
#[must_use = "slot shutdown attempts must be matched before teardown can advance"]
pub enum HostSlotShutdown {
    Vacant,
    Joined(JoinedHostSlot),
}

/// Linear evidence that a host was permanently closed to admission only after
/// every resident native model worker had returned and been joined.
#[derive(Debug)]
pub struct JoinedNativeHost {
    host_identity: Arc<HostIdentity>,
    joined_worker_count: usize,
}

impl JoinedNativeHost {
    #[must_use]
    pub const fn joined_worker_count(&self) -> usize {
        self.joined_worker_count
    }

    /// Returns true only for the exact host instance that minted this token.
    #[must_use]
    pub fn belongs_to(&self, host: &NativeHost) -> bool {
        Arc::ptr_eq(&self.host_identity, &host.identity)
    }
}

impl std::fmt::Debug for HostSlotStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostSlotStatus")
            .field("slot_id", &self.slot_id)
            .field("model_path", &"<redacted>")
            .field("model_bytes", &self.model_bytes)
            .field("reserved_bytes", &self.reserved_bytes)
            .field("status", &self.status)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ResidentModelKey {
    model_id: String,
    model_path: PathBuf,
    mmproj_path: Option<PathBuf>,
    device: NativeDevice,
    context_tokens: u32,
    batch_tokens: u32,
    max_sequences: u32,
    gpu_layers: i32,
}

impl From<&NativeModelConfig> for ResidentModelKey {
    fn from(config: &NativeModelConfig) -> Self {
        Self {
            model_id: config.model_id.clone(),
            model_path: config.model_path.clone(),
            mmproj_path: config.mmproj_path.clone(),
            device: config.device,
            context_tokens: config.context_tokens,
            batch_tokens: config.batch_tokens,
            max_sequences: config.max_sequences,
            gpu_layers: config.gpu_layers,
        }
    }
}

impl std::fmt::Debug for ResidentModelKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResidentModelKey")
            .field("model_id", &self.model_id)
            .field("model_path", &"<redacted>")
            .field("has_mmproj", &self.mmproj_path.is_some())
            .field("device", &self.device)
            .field("context_tokens", &self.context_tokens)
            .field("batch_tokens", &self.batch_tokens)
            .field("max_sequences", &self.max_sequences)
            .field("gpu_layers", &self.gpu_layers)
            .finish()
    }
}

#[derive(Debug)]
struct ResidentEntry {
    key: ResidentModelKey,
    fingerprint: ModelFingerprint,
    owner: NativeModelOwner,
    model_bytes: u64,
    reserved_bytes: u64,
}

#[derive(Debug)]
struct HostState {
    slots: BTreeMap<usize, ResidentEntry>,
    cache: MemoryPrefixCache,
    phase: HostPhase,
    joined_worker_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostPhase {
    Running,
    Quiescing,
    Stopped,
    ShutdownFailed,
}

#[derive(Debug)]
struct HostIdentity;

pub struct NativeHost {
    identity: Arc<HostIdentity>,
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

impl Drop for NativeHost {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.phase = HostPhase::Quiescing;
        let slots = std::mem::take(&mut state.slots);
        for entry in slots.values() {
            entry.owner.begin_shutdown();
        }
        let mut failed = false;
        for (_, entry) in slots {
            if entry.owner.shutdown_joined().is_err() {
                failed = true;
            }
        }
        state.phase = if failed {
            HostPhase::ShutdownFailed
        } else {
            HostPhase::Stopped
        };
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
            identity: Arc::new(HostIdentity),
            state: Mutex::new(HostState {
                slots: BTreeMap::new(),
                cache: MemoryPrefixCache::new(config.memory_cache_bytes),
                phase: HostPhase::Running,
                joined_worker_count: 0,
            }),
            load_gate: Mutex::new(()),
            config,
            clock,
            persistent_cache,
        }
    }

    pub fn acquire(&self, model: NativeModelConfig) -> Result<NativeModelHandle, NativeError> {
        validate_digest_assertions(&model)?;
        self.ensure_running()?;
        if let Some(existing) = self.resident_handle(&model)? {
            return Ok(existing);
        }
        self.with_load_gate(|| {
            self.ensure_running()?;
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
        validate_digest_assertions(&model)?;
        self.ensure_running()?;
        let requested_key = ResidentModelKey::from(&model);
        {
            let state = self.state.lock().map_err(host_poisoned)?;
            if let Some(existing) = state.slots.get(&slot_id) {
                let reusable_slot = resolve_resident_slot(
                    std::iter::once((slot_id, &existing.key, &existing.fingerprint)),
                    &requested_key,
                    &model,
                )?;
                if reusable_slot.is_some() {
                    return Ok(existing.owner.handle());
                }
                return Err(NativeError::new(
                    NativeErrorCode::ModelInUse,
                    format!(
                        "slot {slot_id} owns a different native model; join that worker before reusing the slot"
                    ),
                ));
            }
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
        let owner = NativeModelOwner::load(model.clone())?;
        let fingerprint = owner.status().fingerprint.ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::ModelLoadFailed,
                "loaded resident did not publish a model fingerprint",
            )
        })?;
        let handle = owner.handle();
        let mut state = self.state.lock().map_err(host_poisoned)?;
        state.slots.insert(
            slot_id,
            ResidentEntry {
                key: requested_key,
                fingerprint,
                owner,
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
        let requested_key = ResidentModelKey::from(model);
        let state = self.state.lock().map_err(host_poisoned)?;
        ensure_host_running(&state)?;
        let slot_id = resolve_resident_slot(
            state
                .slots
                .iter()
                .map(|(slot_id, entry)| (*slot_id, &entry.key, &entry.fingerprint)),
            &requested_key,
            model,
        )?;
        Ok(slot_id.and_then(|slot_id| state.slots.get(&slot_id).map(|entry| entry.owner.handle())))
    }

    fn with_load_gate<T>(
        &self,
        operation: impl FnOnce() -> Result<T, NativeError>,
    ) -> Result<T, NativeError> {
        let _guard = self.load_gate.lock().map_err(host_poisoned)?;
        operation()
    }

    fn ensure_running(&self) -> Result<(), NativeError> {
        let state = self.state.lock().map_err(host_poisoned)?;
        ensure_host_running(&state)
    }

    pub fn generate(
        &self,
        model: NativeModelConfig,
        request: GenerationRequest,
    ) -> Result<GenerationTicket, NativeError> {
        self.acquire(model)?.generate(request)
    }

    pub fn generate_batch(
        &self,
        model: NativeModelConfig,
        request: GenerationBatchRequest,
    ) -> Result<GenerationTicket, NativeError> {
        self.acquire(model)?.generate_batch(request)
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
                    .map(|entry| entry.owner.handle().cancel(request_id, branch_id))
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
                    .map(|entry| entry.owner.handle().skip_reasoning(request_id, branch_id))
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
                    .filter_map(|entry| entry.owner.status().descriptor)
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
                        model_path: entry.key.model_path.clone(),
                        model_bytes: entry.model_bytes,
                        reserved_bytes: entry.reserved_bytes,
                        status: entry.owner.status(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn handle(&self, slot_id: usize) -> Option<NativeModelHandle> {
        let state = self.state.lock().ok()?;
        ensure_host_running(&state).ok()?;
        state.slots.get(&slot_id).map(|entry| entry.owner.handle())
    }

    /// Compatibility wrapper around joined slot shutdown.
    ///
    /// `true` means the native owner thread was joined, not merely that a map
    /// entry was removed. External command clients are revoked by the unique
    /// host-owned worker owner and cannot prolong worker lifetime.
    pub fn unload(&self, slot_id: usize) -> bool {
        matches!(
            self.shutdown_slot_joined(slot_id),
            Ok(HostSlotShutdown::Joined(_))
        )
    }

    pub fn unload_all(&self) -> usize {
        self.with_load_gate(|| {
            self.ensure_running()?;
            {
                let state = self.state.lock().map_err(host_poisoned)?;
                state
                    .slots
                    .values()
                    .for_each(|entry| entry.owner.begin_shutdown());
            }
            let slot_ids = self
                .state
                .lock()
                .map_err(host_poisoned)?
                .slots
                .keys()
                .copied()
                .collect::<Vec<_>>();
            Ok(slot_ids
                .into_iter()
                .filter_map(|slot_id| self.shutdown_slot_joined_inner(slot_id).ok())
                .filter(|outcome| matches!(outcome, HostSlotShutdown::Joined(_)))
                .count())
        })
        .unwrap_or_default()
    }

    /// Removes one resident slot only after consuming its unique worker owner
    /// and joining the native thread. Cloneable command clients are revoked;
    /// they never own the worker join handle.
    pub fn shutdown_slot_joined(&self, slot_id: usize) -> Result<HostSlotShutdown, NativeError> {
        self.with_load_gate(|| {
            self.ensure_running()?;
            match self.shutdown_slot_joined_inner(slot_id) {
                Ok(outcome) => Ok(outcome),
                Err(error) => {
                    self.drain_remaining_after_failure();
                    Err(error)
                }
            }
        })
    }

    fn drain_remaining_after_failure(&self) {
        let slot_ids = match self.state.lock() {
            Ok(state) => {
                for entry in state.slots.values() {
                    entry.owner.begin_shutdown();
                }
                state.slots.keys().copied().collect::<Vec<_>>()
            }
            Err(_) => return,
        };
        for slot_id in slot_ids {
            let _ = self.shutdown_slot_joined_inner(slot_id);
        }
    }

    fn shutdown_slot_joined_inner(&self, slot_id: usize) -> Result<HostSlotShutdown, NativeError> {
        let Some(entry) = self
            .state
            .lock()
            .map_err(host_poisoned)?
            .slots
            .remove(&slot_id)
        else {
            return Ok(HostSlotShutdown::Vacant);
        };
        let ResidentEntry {
            key: _,
            fingerprint: _,
            owner,
            model_bytes: _,
            reserved_bytes: _,
        } = entry;
        let worker = match owner.shutdown_joined() {
            Ok(worker) => worker,
            Err(error) => {
                if let Ok(mut state) = self.state.lock() {
                    state.phase = HostPhase::ShutdownFailed;
                }
                return Err(error);
            }
        };
        let mut state = self.state.lock().map_err(host_poisoned)?;
        state.joined_worker_count = state.joined_worker_count.checked_add(1).ok_or_else(|| {
            state.phase = HostPhase::ShutdownFailed;
            NativeError::new(
                NativeErrorCode::Internal,
                "native host joined-worker accounting overflowed",
            )
        })?;
        Ok(HostSlotShutdown::Joined(JoinedHostSlot {
            host_identity: Arc::clone(&self.identity),
            slot_id,
            worker,
        }))
    }

    /// Permanently closes model admission and returns linear host-shutdown
    /// evidence only after every resident model worker has been joined.
    pub fn shutdown_joined(&self) -> Result<JoinedNativeHost, NativeError> {
        self.with_load_gate(|| {
            {
                let mut state = self.state.lock().map_err(host_poisoned)?;
                match state.phase {
                    HostPhase::Running => state.phase = HostPhase::Quiescing,
                    HostPhase::Quiescing => {}
                    HostPhase::Stopped => {
                        return Err(NativeError::new(
                            NativeErrorCode::WorkerStopped,
                            "native host shutdown was already completed",
                        ));
                    }
                    HostPhase::ShutdownFailed => {
                        return Err(NativeError::new(
                            NativeErrorCode::WorkerStopped,
                            "native host shutdown previously failed; joined authority is permanently unavailable",
                        ));
                    }
                }
            }
            {
                let state = self.state.lock().map_err(host_poisoned)?;
                state
                    .slots
                    .values()
                    .for_each(|entry| entry.owner.begin_shutdown());
            }
            let slot_ids = self
                .state
                .lock()
                .map_err(host_poisoned)?
                .slots
                .keys()
                .copied()
                .collect::<Vec<_>>();
            let mut first_join_error = None;
            for slot_id in slot_ids {
                match self.shutdown_slot_joined_inner(slot_id) {
                    Ok(HostSlotShutdown::Vacant | HostSlotShutdown::Joined(_)) => {}
                    Err(error) => {
                        first_join_error.get_or_insert(error);
                    }
                }
            }
            if let Some(error) = first_join_error {
                return Err(error);
            }
            let mut state = self.state.lock().map_err(host_poisoned)?;
            if !state.slots.is_empty() {
                return Err(NativeError::new(
                    NativeErrorCode::Internal,
                    "native host gained a resident slot while its shutdown gate was held",
                ));
            }
            state.phase = HostPhase::Stopped;
            let joined_worker_count = state.joined_worker_count;
            Ok(JoinedNativeHost {
                host_identity: Arc::clone(&self.identity),
                joined_worker_count,
            })
        })
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

fn ensure_host_running(state: &HostState) -> Result<(), NativeError> {
    match state.phase {
        HostPhase::Running => Ok(()),
        HostPhase::Quiescing => Err(NativeError::new(
            NativeErrorCode::WorkerStopped,
            "native host admission is closed while joined shutdown drains workers",
        )),
        HostPhase::Stopped => Err(NativeError::new(
            NativeErrorCode::WorkerStopped,
            "native host admission is permanently closed after joined shutdown",
        )),
        HostPhase::ShutdownFailed => Err(NativeError::new(
            NativeErrorCode::WorkerStopped,
            "native host admission is permanently closed after a failed worker join",
        )),
    }
}

fn resolve_resident_slot<'a>(
    candidates: impl IntoIterator<Item = (usize, &'a ResidentModelKey, &'a ModelFingerprint)>,
    requested_key: &ResidentModelKey,
    requested_config: &NativeModelConfig,
) -> Result<Option<usize>, NativeError> {
    for (slot_id, key, fingerprint) in candidates {
        if key == requested_key {
            validate_resident_digest_assertions(requested_config, fingerprint)?;
            return Ok(Some(slot_id));
        }
    }
    Ok(None)
}

fn validate_digest_assertions(config: &NativeModelConfig) -> Result<(), NativeError> {
    validate_digest_syntax(
        "expected_model_sha256",
        config.expected_model_sha256.as_deref(),
    )?;
    validate_digest_syntax(
        "expected_mmproj_sha256",
        config.expected_mmproj_sha256.as_deref(),
    )?;
    if config.expected_mmproj_sha256.is_some() && config.mmproj_path.is_none() {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            "expected_mmproj_sha256 requires a multimodal projector path",
        ));
    }
    Ok(())
}

fn validate_digest_syntax(field: &str, value: Option<&str>) -> Result<(), NativeError> {
    if let Some(value) = value
        && (value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(NativeError::new(
            NativeErrorCode::InvalidConfig,
            format!("{field} must be exactly 64 lowercase hexadecimal characters"),
        ));
    }
    Ok(())
}

fn validate_resident_digest_assertions(
    config: &NativeModelConfig,
    fingerprint: &ModelFingerprint,
) -> Result<(), NativeError> {
    validate_digest_assertions(config)?;
    if config
        .expected_model_sha256
        .as_deref()
        .is_some_and(|expected| expected != fingerprint.model_sha256)
    {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "resident model fingerprint does not match expected_model_sha256",
        ));
    }
    if config
        .expected_mmproj_sha256
        .as_deref()
        .is_some_and(|expected| {
            fingerprint.multimodal_projector_sha256.as_deref() != Some(expected)
        })
    {
        return Err(NativeError::new(
            NativeErrorCode::ModelInvalid,
            "resident projector fingerprint does not match expected_mmproj_sha256",
        ));
    }
    Ok(())
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

    fn resident_test_config(expected_model_sha256: Option<&str>) -> NativeModelConfig {
        let mut config = NativeModelConfig::local(PathBuf::from("/private/MODEL_SENTINEL.gguf"));
        config.model_id = "resident-test-model".to_string();
        config.expected_model_sha256 = expected_model_sha256.map(str::to_owned);
        config.device = NativeDevice::Cpu;
        config.context_tokens = 512;
        config.batch_tokens = 64;
        config.max_sequences = 1;
        config.gpu_layers = 0;
        config
    }

    fn resident_test_fingerprint(model_sha256: &str) -> ModelFingerprint {
        ModelFingerprint {
            model_id: "resident-test-model".to_string(),
            model_size: 17,
            model_sha256: model_sha256.to_string(),
            tokenizer_sha256: model_sha256.to_string(),
            chat_template_sha256: "chat-template".to_string(),
            multimodal_projector_sha256: None,
            binding_version: "binding".to_string(),
            build_id: "build".to_string(),
            backend: "cpu".to_string(),
            context_tokens: 512,
            batch_tokens: 64,
            max_sequences: 1,
            rope_config_sha256: "rope".to_string(),
            kv_layout_sha256: "kv".to_string(),
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
    fn joined_host_shutdown_permanently_closes_admission() {
        let host = NativeHost::new(NativeHostConfig::default());
        let other = NativeHost::new(NativeHostConfig::default());
        let joined = host.shutdown_joined().expect("empty shutdown");
        assert_eq!(joined.joined_worker_count(), 0);
        assert!(joined.belongs_to(&host));
        assert!(!joined.belongs_to(&other));

        let error = host
            .acquire(NativeModelConfig::local(PathBuf::from("never-opened.gguf")))
            .expect_err("stopped host must reject admission before file access");
        assert_eq!(error.code, NativeErrorCode::WorkerStopped);
        assert!(host.shutdown_joined().is_err());
    }

    #[test]
    fn digest_assertions_validate_without_participating_in_resident_identity() {
        const CORRECT: &str = "1111111111111111111111111111111111111111111111111111111111111111";
        const WRONG: &str = "2222222222222222222222222222222222222222222222222222222222222222";
        let unpinned = resident_test_config(None);
        let pinned = resident_test_config(Some(CORRECT));
        let fingerprint = resident_test_fingerprint(CORRECT);
        let unpinned_key = ResidentModelKey::from(&unpinned);
        let pinned_key = ResidentModelKey::from(&pinned);

        assert_eq!(unpinned_key, pinned_key);
        assert_eq!(
            resolve_resident_slot(
                std::iter::once((3, &unpinned_key, &fingerprint)),
                &pinned_key,
                &pinned,
            )
            .expect("a correct assertion reuses an unpinned resident"),
            Some(3)
        );
        assert_eq!(
            resolve_resident_slot(
                std::iter::once((3, &pinned_key, &fingerprint)),
                &unpinned_key,
                &unpinned,
            )
            .expect("an unpinned request reuses a pinned resident"),
            Some(3)
        );

        let wrong = resident_test_config(Some(WRONG));
        let error = resolve_resident_slot(
            std::iter::once((3, &unpinned_key, &fingerprint)),
            &ResidentModelKey::from(&wrong),
            &wrong,
        )
        .expect_err("a wrong assertion rejects the resident");
        assert_eq!(error.code, NativeErrorCode::ModelInvalid);
    }

    #[test]
    #[ignore = "requires MOM_LLAMA_MODEL_PATH, MOM_LLAMA_MODEL_SHA256, and a real local GGUF"]
    fn real_digest_assertions_reuse_one_resident_and_wrong_digest_cannot_mutate_slots()
    -> Result<(), Box<dyn std::error::Error>> {
        let model_path = PathBuf::from(std::env::var("MOM_LLAMA_MODEL_PATH")?);
        let model_sha256 = std::env::var("MOM_LLAMA_MODEL_SHA256")?;
        let make_config = |expected_model_sha256: Option<String>| {
            let mut config = NativeModelConfig::local(model_path.clone());
            config.expected_model_sha256 = expected_model_sha256;
            config.device = NativeDevice::Cpu;
            config.context_tokens = 512;
            config.batch_tokens = 64;
            config.max_sequences = 1;
            config.gpu_layers = 0;
            config
        };
        let host_config = NativeHostConfig {
            max_slots: 1,
            cache_policy: HostCachePolicy::Disabled,
            ..NativeHostConfig::default()
        };

        let unpinned_first = NativeHost::new(host_config.clone());
        unpinned_first.acquire(make_config(None))?;
        unpinned_first.acquire(make_config(Some(model_sha256.clone())))?;
        assert_eq!(unpinned_first.slots().len(), 1);

        let pinned_first = NativeHost::new(host_config);
        pinned_first.acquire(make_config(Some(model_sha256)))?;
        pinned_first.acquire(make_config(None))?;
        assert_eq!(pinned_first.slots().len(), 1);

        let before = pinned_first.slots();
        let error = pinned_first
            .acquire(make_config(Some("0".repeat(64))))
            .expect_err("a wrong digest must reject the resident");
        assert_eq!(error.code, NativeErrorCode::ModelInvalid);
        assert_eq!(pinned_first.slots(), before);
        Ok(())
    }

    #[test]
    #[ignore = "requires MOM_LLAMA_MODEL_PATH, MOM_LLAMA_MODEL_SHA256, and a real local GGUF"]
    fn real_joined_shutdown_revokes_live_clients_then_joins_and_stops_admission()
    -> Result<(), Box<dyn std::error::Error>> {
        let model_path = PathBuf::from(std::env::var("MOM_LLAMA_MODEL_PATH")?);
        let mut config = NativeModelConfig::local(model_path);
        config.expected_model_sha256 = Some(std::env::var("MOM_LLAMA_MODEL_SHA256")?);
        config.device = NativeDevice::Cpu;
        config.context_tokens = 512;
        config.batch_tokens = 64;
        config.max_sequences = 1;
        config.gpu_layers = 0;
        let host = NativeHost::new(NativeHostConfig {
            memory_budget_bytes: u64::MAX,
            max_slots: 1,
            memory_cache_bytes: 0,
            cache_namespace: "joined-shutdown-real-test".to_string(),
            cache_policy: HostCachePolicy::Disabled,
        });
        let handle = host.acquire(config.clone())?;

        let joined = host.shutdown_joined()?;
        assert_eq!(joined.joined_worker_count(), 1);
        assert!(joined.belongs_to(&host));
        assert!(host.slots().is_empty());
        assert_eq!(
            handle
                .snapshot_sequence(0)
                .expect_err("a revoked client cannot submit work")
                .code,
            NativeErrorCode::WorkerStopped
        );
        assert_eq!(
            host.acquire(config)
                .expect_err("joined host stays stopped")
                .code,
            NativeErrorCode::WorkerStopped
        );
        Ok(())
    }

    #[test]
    fn host_slot_debug_redacts_both_operational_model_paths() {
        let slot = HostSlotStatus {
            slot_id: 3,
            model_path: PathBuf::from("/private/HOST_PATH_SENTINEL.gguf"),
            model_bytes: 17,
            reserved_bytes: 23,
            status: ResidentModelStatus {
                model_id: "model".to_string(),
                model_path: PathBuf::from("/private/RESIDENT_PATH_SENTINEL.gguf"),
                state: llama_native_types::ModelRuntimeState::Ready,
                fingerprint: None,
                descriptor: None,
                active_sequences: 0,
                max_sequences: 1,
            },
        };

        let debug = format!("{slot:?}");
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("slot_id: 3"));
        assert!(!debug.contains("HOST_PATH_SENTINEL"));
        assert!(!debug.contains("RESIDENT_PATH_SENTINEL"));
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
