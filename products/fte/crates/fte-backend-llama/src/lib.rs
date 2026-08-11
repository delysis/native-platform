//! In-process llama-native adapter for Free Token Energy.
//!
//! This is the only gateway crate that knows both the canonical FTE contracts
//! and llama-native-kit. It contains no HTTP, process, shell, or executable
//! discovery path.

use async_trait::async_trait;
use fte_types::{
    BackendDescriptor, BackendLocation, BackendReadiness, BackendRequest, CacheMode, CacheOutcome,
    CachePolicy, CacheReceipt, CacheRequirement, CacheTier, CancelTarget, CompletionPrompt,
    ContentBlock, ErrorClass, GatewayBackend, GatewayError, GatewayEvent, GatewayResponse,
    GatewayTicket, GatewayUsage, GenerationInput, InputItem, MessageRole, Modality,
    ModelCapabilities, ModelDescriptor, OutputItem, PromptForm, RequestId, ResolvedRoute,
    RouteObservations, TerminalStatus, TicketCancellation, UsageProvenance,
};
use llama_native_cache::{
    CacheFingerprint, CacheTier as NativeCacheTier, PrefixCacheMetadata, PrefixCacheValue,
};
use llama_native_host::NativeHost;
use llama_native_types::{
    BranchRequest, ChatMessage, ChatRole, ChatTemplateChoice,
    CompletionPrompt as NativeCompletionPrompt, GenerationEventKind as NativeEventKind,
    GenerationInput as NativeInput, GenerationOutput as NativeOutput,
    GenerationRequest as NativeRequest, GenerationState as NativeState, NativeError,
    NativeErrorCode, NativeModelConfig, PreparedPrompt, PromptForm as NativePromptForm,
    SamplingConfig, SharedPrefixBatchRequest, SpecialTokenPolicy,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, mpsc, oneshot};
use uuid::Uuid;

pub const BACKEND_ID: &str = "llama-native";

#[derive(Clone)]
struct BackendConfiguration {
    epoch: u64,
    host: Arc<NativeHost>,
    models: BTreeMap<String, NativeModelConfig>,
}

pub struct LlamaNativeBackend {
    configuration: RwLock<BackendConfiguration>,
    lifecycle: Arc<BackendLifecycle>,
}

impl std::fmt::Debug for LlamaNativeBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LlamaNativeBackend")
            .field("configuration", &"shared native host and model profiles")
            .field("host_ownership", &"borrowed")
            .finish_non_exhaustive()
    }
}

impl LlamaNativeBackend {
    /// Borrows an application-owned native host.
    ///
    /// Adapter shutdown closes this backend's admission and drains its own
    /// requests. It never unloads, permanently closes, or otherwise assumes
    /// process-lifetime ownership of `host`.
    #[must_use]
    pub fn new_borrowed(host: Arc<NativeHost>) -> Self {
        Self {
            configuration: RwLock::new(BackendConfiguration {
                epoch: 0,
                host,
                models: BTreeMap::new(),
            }),
            lifecycle: Arc::new(BackendLifecycle::new()),
        }
    }

    /// Compatibility spelling for [`Self::new_borrowed`].
    ///
    /// The supplied host is borrowed; this adapter never owns native-host
    /// process shutdown authority.
    #[must_use]
    pub fn new(host: Arc<NativeHost>) -> Self {
        Self::new_borrowed(host)
    }

    /// Rebinds the gateway to a product-owned host and its complete configured
    /// model set. Requests already admitted keep their previous host alive;
    /// later requests see the host and model registry as one atomic snapshot.
    pub fn replace_configuration(
        &self,
        host: Arc<NativeHost>,
        models: impl IntoIterator<Item = NativeModelConfig>,
    ) -> Result<(), GatewayError> {
        let request_id = RequestId::new();
        let models = models
            .into_iter()
            .map(|config| (config.model_id.clone(), config))
            .collect::<BTreeMap<_, _>>();
        let _admission = self.lifecycle.lock_running(&request_id)?;
        let mut configuration = self.configuration.write().map_err(|_| {
            GatewayError::unavailable(
                &request_id,
                "native_configuration_registry_poisoned",
                "the local native configuration registry is unavailable",
            )
        })?;
        let epoch = next_configuration_epoch(configuration.epoch, &request_id)?;
        *configuration = BackendConfiguration {
            epoch,
            host,
            models,
        };
        Ok(())
    }

    fn configuration_snapshot(
        &self,
        request_id: &RequestId,
    ) -> Result<BackendConfiguration, GatewayError> {
        self.configuration
            .read()
            .map(|configuration| configuration.clone())
            .map_err(|_| {
                GatewayError::unavailable(
                    request_id,
                    "native_configuration_registry_poisoned",
                    "the local native configuration registry is unavailable",
                )
            })
    }

    fn admit_host_operation(
        &self,
        request_id: &RequestId,
    ) -> Result<(Arc<NativeHost>, u64, OperationLease), GatewayError> {
        let mut lifecycle = self.lifecycle.lock_state();
        ensure_backend_running(&lifecycle, request_id)?;
        let configuration = self.configuration_snapshot(request_id)?;
        let epoch = configuration.epoch;
        let host = configuration.host;
        let operation =
            self.lifecycle
                .admit_locked(&mut lifecycle, request_id.clone(), Arc::clone(&host))?;
        Ok((host, epoch, operation))
    }

    fn admit_request_configuration(
        &self,
        model_id: &str,
        request_id: &RequestId,
    ) -> Result<(Arc<NativeHost>, NativeModelConfig, OperationLease), GatewayError> {
        let mut lifecycle = self.lifecycle.lock_state();
        ensure_backend_running(&lifecycle, request_id)?;
        let configuration = self.configuration_snapshot(request_id)?;
        let model = configuration.models.get(model_id).cloned().ok_or_else(|| {
            GatewayError::invalid_request(
                request_id,
                "native_model_not_registered",
                "the requested local model profile is not registered",
            )
        })?;
        let host = configuration.host;
        let operation =
            self.lifecycle
                .admit_locked(&mut lifecycle, request_id.clone(), Arc::clone(&host))?;
        Ok((host, model, operation))
    }

    /// Loads and registers an exact native model profile.
    ///
    /// `NativeHost::acquire` is single-profile idempotent: repeated registration
    /// reuses the resident worker rather than loading a second model.
    pub fn register_model(
        &self,
        config: NativeModelConfig,
    ) -> Result<ModelDescriptor, GatewayError> {
        let request_id = RequestId::new();
        let (host, configuration_epoch, operation) = self.admit_host_operation(&request_id)?;
        let handle = host
            .acquire(config.clone())
            .map_err(|error| map_native_error(&request_id, error))?;
        let status = handle.status();
        let descriptor = status.descriptor.ok_or_else(|| {
            GatewayError::unavailable(
                &request_id,
                "native_model_descriptor_missing",
                "the loaded model did not return an inspected descriptor",
            )
        })?;
        operation.ensure_continuing()?;
        self.commit_registered_model(&request_id, &host, configuration_epoch, config)?;
        Ok(map_model_descriptor(&descriptor))
    }

    fn commit_registered_model(
        &self,
        request_id: &RequestId,
        host: &Arc<NativeHost>,
        configuration_epoch: u64,
        config: NativeModelConfig,
    ) -> Result<(), GatewayError> {
        let _admission = self.lifecycle.lock_running(request_id)?;
        let mut configuration = self.configuration.write().map_err(|_| {
            GatewayError::unavailable(
                request_id,
                "native_configuration_registry_poisoned",
                "the local native configuration registry is unavailable",
            )
        })?;
        if configuration.epoch != configuration_epoch || !Arc::ptr_eq(&configuration.host, host) {
            return Err(GatewayError::unavailable(
                request_id,
                "native_configuration_changed",
                "the local native configuration changed while the model was loading; retry registration",
            ));
        }
        configuration.epoch = next_configuration_epoch(configuration.epoch, request_id)?;
        configuration.models.insert(config.model_id.clone(), config);
        Ok(())
    }

    /// Registers an explicit model profile without loading it. The first
    /// admitted request uses the host's single-flight resident acquisition.
    pub fn configure_model(&self, config: NativeModelConfig) -> Result<(), GatewayError> {
        let request_id = RequestId::new();
        let _admission = self.lifecycle.lock_running(&request_id)?;
        let mut configuration = self.configuration.write().map_err(|_| {
            GatewayError::unavailable(
                &request_id,
                "native_configuration_registry_poisoned",
                "the local native configuration registry is unavailable",
            )
        })?;
        configuration.epoch = next_configuration_epoch(configuration.epoch, &request_id)?;
        configuration.models.insert(config.model_id.clone(), config);
        Ok(())
    }
}

fn next_configuration_epoch(epoch: u64, request_id: &RequestId) -> Result<u64, GatewayError> {
    epoch.checked_add(1).ok_or_else(|| {
        GatewayError::unavailable(
            request_id,
            "native_configuration_epoch_exhausted",
            "the local native configuration epoch is exhausted",
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendPhase {
    Running,
    Quiescing,
    Closed,
}

struct ActiveOperation {
    control: Arc<OperationControl>,
}

struct BackendLifecycleState {
    phase: BackendPhase,
    next_operation_id: u64,
    active: BTreeMap<u64, ActiveOperation>,
}

struct BackendLifecycle {
    state: Mutex<BackendLifecycleState>,
    changed: Notify,
}

impl BackendLifecycle {
    fn new() -> Self {
        Self {
            state: Mutex::new(BackendLifecycleState {
                phase: BackendPhase::Running,
                next_operation_id: 1,
                active: BTreeMap::new(),
            }),
            changed: Notify::new(),
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, BackendLifecycleState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_running(
        &self,
        request_id: &RequestId,
    ) -> Result<std::sync::MutexGuard<'_, BackendLifecycleState>, GatewayError> {
        let state = self.lock_state();
        ensure_backend_running(&state, request_id)?;
        Ok(state)
    }

    fn admit_locked(
        self: &Arc<Self>,
        state: &mut BackendLifecycleState,
        request_id: RequestId,
        host: Arc<NativeHost>,
    ) -> Result<OperationLease, GatewayError> {
        let operation_id = state.next_operation_id;
        state.next_operation_id = state.next_operation_id.checked_add(1).ok_or_else(|| {
            GatewayError::unavailable(
                &request_id,
                "native_operation_id_exhausted",
                "the local native backend exhausted its operation id space",
            )
        })?;
        let control = Arc::new(OperationControl {
            native_request_ids: Mutex::new(BTreeSet::from([request_id.0.clone()])),
            request_id,
            host,
            request_cancelled: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            cancelled_outputs: Mutex::new(BTreeSet::new()),
        });
        let previous = state.active.insert(
            operation_id,
            ActiveOperation {
                control: Arc::clone(&control),
            },
        );
        debug_assert!(previous.is_none());
        Ok(OperationLease(Arc::new(OperationLeaseInner {
            operation_id,
            lifecycle: Arc::clone(self),
            control,
        })))
    }

    fn finish(&self, operation_id: u64) {
        let mut state = self.lock_state();
        state.active.remove(&operation_id);
        if state.phase == BackendPhase::Quiescing && state.active.is_empty() {
            state.phase = BackendPhase::Closed;
        }
        drop(state);
        self.changed.notify_waiters();
    }

    fn cancel(&self, request_id: &RequestId, target: CancelTarget) -> usize {
        let controls = {
            let state = self.lock_state();
            state
                .active
                .values()
                .filter(|operation| operation.control.request_id == *request_id)
                .map(|operation| Arc::clone(&operation.control))
                .collect::<Vec<_>>()
        };
        controls
            .into_iter()
            .map(|control| control.cancel(target))
            .sum()
    }

    fn begin_shutdown(&self) -> Vec<Arc<OperationControl>> {
        let mut state = self.lock_state();
        if state.phase == BackendPhase::Running {
            state.phase = BackendPhase::Quiescing;
        }
        for operation in state.active.values() {
            operation.control.draining.store(true, Ordering::Release);
        }
        if state.active.is_empty() {
            state.phase = BackendPhase::Closed;
        }
        let controls = state
            .active
            .values()
            .map(|operation| Arc::clone(&operation.control))
            .collect::<Vec<_>>();
        drop(state);
        self.changed.notify_waiters();
        controls
    }
}

fn ensure_backend_running(
    state: &BackendLifecycleState,
    request_id: &RequestId,
) -> Result<(), GatewayError> {
    if state.phase == BackendPhase::Running {
        Ok(())
    } else {
        Err(backend_closed_error(request_id))
    }
}

fn backend_closed_error(request_id: &RequestId) -> GatewayError {
    GatewayError::unavailable(
        request_id,
        "native_backend_closed",
        "the local native backend is quiescing or closed and accepts no new work",
    )
}

fn backend_cancelled_error(request_id: &RequestId) -> GatewayError {
    GatewayError {
        code: "native_request_cancelled".to_string(),
        class: ErrorClass::Cancelled,
        retryable: false,
        http_status: 499,
        request_id: request_id.clone(),
        provider: Some(BACKEND_ID.to_string()),
        safe_detail: "the local native request was cancelled before generation started".to_string(),
    }
}

struct OperationControl {
    request_id: RequestId,
    host: Arc<NativeHost>,
    native_request_ids: Mutex<BTreeSet<String>>,
    request_cancelled: AtomicBool,
    draining: AtomicBool,
    cancelled_outputs: Mutex<BTreeSet<usize>>,
}

impl OperationControl {
    fn register_native_request_id(&self, native_request_id: String) {
        self.native_request_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(native_request_id.clone());
        if self.request_cancelled.load(Ordering::Acquire) {
            let _ = self.host.cancel(&native_request_id, None);
        }
    }

    fn cancel(&self, target: CancelTarget) -> usize {
        match target {
            CancelTarget::Request => {
                let newly_accepted = !self.request_cancelled.swap(true, Ordering::AcqRel);
                let native_cancelled: usize = self
                    .native_request_ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .map(|native_request_id| self.host.cancel(native_request_id, None))
                    .sum();
                native_cancelled.max(usize::from(newly_accepted))
            }
            CancelTarget::Output(index) => {
                let newly_accepted = self
                    .cancelled_outputs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(index);
                let native_cancelled =
                    cancel_native(&self.host, &self.request_id, CancelTarget::Output(index));
                native_cancelled.max(usize::from(newly_accepted))
            }
        }
    }

    fn apply_pending_cancellation(&self) -> usize {
        if self.request_cancelled.load(Ordering::Acquire) {
            return self
                .native_request_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .map(|native_request_id| self.host.cancel(native_request_id, None))
                .sum();
        }
        self.cancelled_outputs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .map(|index| cancel_native(&self.host, &self.request_id, CancelTarget::Output(index)))
            .sum()
    }
}

#[derive(Clone)]
struct OperationLease(Arc<OperationLeaseInner>);

impl OperationLease {
    fn control(&self) -> Arc<OperationControl> {
        Arc::clone(&self.0.control)
    }

    fn ensure_continuing(&self) -> Result<(), GatewayError> {
        let state = self.0.lifecycle.lock_state();
        if state.phase != BackendPhase::Running {
            return Err(backend_closed_error(&self.0.control.request_id));
        }
        if self.0.control.request_cancelled.load(Ordering::Acquire) {
            return Err(backend_cancelled_error(&self.0.control.request_id));
        }
        Ok(())
    }

    fn begin_native<T>(
        &self,
        start: impl FnOnce() -> Result<T, NativeError>,
    ) -> Result<T, GatewayError> {
        let state = self.0.lifecycle.lock_state();
        ensure_backend_running(&state, &self.0.control.request_id)?;
        if self.0.control.request_cancelled.load(Ordering::Acquire) {
            return Err(backend_cancelled_error(&self.0.control.request_id));
        }
        let result =
            start().map_err(|error| map_native_error(&self.0.control.request_id, error))?;
        self.0.control.apply_pending_cancellation();
        Ok(result)
    }
}

struct OperationLeaseInner {
    operation_id: u64,
    lifecycle: Arc<BackendLifecycle>,
    control: Arc<OperationControl>,
}

impl Drop for OperationLeaseInner {
    fn drop(&mut self) {
        self.lifecycle.finish(self.operation_id);
    }
}

async fn tracked_spawn_blocking<T, F>(
    operation: &OperationLease,
    request_id: &RequestId,
    action: &'static str,
    task: F,
) -> Result<T, GatewayError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let work = operation.clone();
    tokio::task::spawn_blocking(move || {
        let _work = work;
        task()
    })
    .await
    .map_err(|error| blocking_task_error(request_id, action, error))
}

#[async_trait]
impl GatewayBackend for LlamaNativeBackend {
    fn descriptor(&self) -> BackendDescriptor {
        let configuration = self.configuration_snapshot(&RequestId::new()).ok();
        let inspected = configuration
            .as_ref()
            .map(|configuration| configuration.host.descriptors())
            .unwrap_or_default();
        let configured = configuration
            .map(|configuration| configuration.models.into_values().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut models = inspected
            .iter()
            .map(map_model_descriptor)
            .collect::<Vec<_>>();
        for config in configured {
            if !models.iter().any(|model| model.id == config.model_id) {
                models.push(configured_model_descriptor(&config));
            }
        }
        BackendDescriptor {
            id: BACKEND_ID.to_string(),
            display_name: "llama.cpp (in process)".to_string(),
            location: BackendLocation::LocalEmbedded,
            models,
        }
    }

    fn readiness(&self) -> BackendReadiness {
        if self.lifecycle.lock_state().phase != BackendPhase::Running {
            return BackendReadiness::Unavailable {
                reason: "the local native backend is quiescing or closed".to_string(),
            };
        }
        if self
            .configuration
            .read()
            .map(|configuration| configuration.models.is_empty())
            .unwrap_or(true)
        {
            BackendReadiness::NotConfigured {
                reason: "no local GGUF model is registered".to_string(),
            }
        } else {
            BackendReadiness::Ready
        }
    }

    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
        let request_id = request.request.request_id.clone();
        validate_cache_policy(&request.request.cache, &request.request.input, &request_id)?;
        let input = translate_input(&request.request.input, &request_id)?;
        let sampling = translate_sampling(&request.request.sampling);
        let (host, config, operation) =
            self.admit_request_configuration(&request.route.model_id, &request_id)?;
        let host_for_acquire = Arc::clone(&host);
        let config_for_acquire = config.clone();
        let request_for_acquire = request_id.clone();
        let handle = tracked_spawn_blocking(
            &operation,
            &request_id,
            "native model acquisition",
            move || {
                host_for_acquire
                    .acquire(config_for_acquire)
                    .map_err(|error| map_native_error(&request_for_acquire, error))
            },
        )
        .await??;
        operation.ensure_continuing()?;
        let handle_for_prepare = handle.clone();
        let input_for_prepare = input.clone();
        let request_for_prepare = request_id.clone();
        let prepared = tracked_spawn_blocking(
            &operation,
            &request_id,
            "native prompt preparation",
            move || {
                handle_for_prepare
                    .prepare_input(input_for_prepare)
                    .map_err(|error| map_native_error(&request_for_prepare, error))
            },
        )
        .await??;
        operation.ensure_continuing()?;
        let fingerprint = native_cache_fingerprint(&handle, prepared.first())?;
        let owner = cache_owner(&request.request.cache);
        let lookup_cached = if cache_lookup_enabled(request.request.cache.mode)
            && prepared.len() == 1
        {
            owner.as_deref().map_or_else(
                || host.cache_lookup(&fingerprint, &prepared[0].token_ids),
                |owner| host.cache_lookup_for_owner(&fingerprint, &prepared[0].token_ids, owner),
            )
        } else {
            None
        };
        let cache_hit = lookup_cached.is_some();
        let mut cached = lookup_cached;
        let mut miss_reason = None;
        if cached.is_none()
            && cache_lookup_enabled(request.request.cache.mode)
            && prepared.len() == 1
        {
            let handle_for_prefix = handle.clone();
            let request_for_prefix = request_id.clone();
            let model_for_prefix = config.model_id.clone();
            let input_for_prefix = request.request.input.clone();
            let sampling_for_prefix = sampling.clone();
            let prepared_for_prefix = prepared[0].clone();
            let fingerprint_for_prefix = fingerprint.clone();
            let policy_for_prefix = request.request.cache.clone();
            operation
                .control()
                .register_native_request_id(format!("{}:prefix", request_id.0));
            let prefix = tracked_spawn_blocking(
                &operation,
                &request_id,
                "native prefix prefill",
                move || {
                    prefill_cache_prefix(
                        &handle_for_prefix,
                        PrefixPrefillSpec {
                            request_id: &request_for_prefix,
                            model_id: &model_for_prefix,
                            input: &input_for_prefix,
                            sampling: &sampling_for_prefix,
                            full_prompt: &prepared_for_prefix,
                            fingerprint: &fingerprint_for_prefix,
                            policy: &policy_for_prefix,
                        },
                    )
                },
            )
            .await?;
            operation.ensure_continuing()?;
            match prefix {
                Ok(Some(value)) => {
                    let host_for_insert = Arc::clone(&host);
                    let value_for_insert = value.clone();
                    let insertion = tracked_spawn_blocking(
                        &operation,
                        &request_id,
                        "native prefix storage",
                        move || host_for_insert.cache_insert(value_for_insert),
                    )
                    .await?;
                    operation.ensure_continuing()?;
                    match insertion {
                        Ok(_) => {
                            cached = Some(value);
                            miss_reason = Some(
                                "cold miss; a compatible pre-generation prefix was checkpointed"
                                    .to_string(),
                            );
                        }
                        Err(error) => {
                            let error = map_native_error(&request_id, error);
                            if request.request.cache.requirement == CacheRequirement::Required {
                                return Err(error);
                            }
                            miss_reason = Some(format!(
                                "prefix checkpoint storage was unavailable: {}",
                                error.safe_detail
                            ));
                        }
                    }
                }
                Ok(None) => {
                    miss_reason = Some(format!(
                        "no stable local prefix of at least {MINIMUM_AUTOMATIC_PREFIX_TOKENS} tokens was available"
                    ));
                }
                Err(error) => {
                    if request.request.cache.requirement == CacheRequirement::Required {
                        return Err(error);
                    }
                    miss_reason = Some(format!(
                        "prefix checkpointing was skipped: {}",
                        error.safe_detail
                    ));
                }
            }
        }
        if request.request.cache.requirement == CacheRequirement::Required && cached.is_none() {
            return Err(GatewayError {
                code: "required_cache_unavailable".to_string(),
                class: ErrorClass::Capability,
                retryable: false,
                http_status: 409,
                request_id: request_id.clone(),
                provider: Some(BACKEND_ID.to_string()),
                safe_detail: miss_reason.unwrap_or_else(|| {
                    "the required cache mode is not available for this local request".to_string()
                }),
            });
        }
        let cache_receipt = if cache_hit {
            cache_receipt(&request.request.cache, cached.as_ref(), None)
        } else {
            cache_receipt(&request.request.cache, None, miss_reason)
        };
        let native_request = NativeRequest {
            request_id: request_id.0.clone(),
            model_id: config.model_id.clone(),
            input,
            sampling,
            media: Vec::new(),
            cached_prefix: cached.map(|value| value.sequence),
        };
        let capacity = request
            .request
            .stream
            .event_capacity
            .unwrap_or(fte_types::DEFAULT_EVENT_CAPACITY)
            .clamp(32, 4096);
        let (event_tx, event_rx) = mpsc::channel(capacity);
        let terminal_permit = event_tx.clone().try_reserve_owned().map_err(|_| {
            GatewayError::unavailable(
                &request_id,
                "gateway_terminal_event_reservation_failed",
                "the gateway could not reserve terminal event capacity",
            )
        })?;
        let native_ticket = operation.begin_native(|| handle.generate(native_request))?;
        let (final_tx, final_rx) = oneshot::channel();
        let terminal = Arc::new(AtomicBool::new(false));
        let operation_control = operation.control();
        let cancellation: Arc<dyn TicketCancellation> = Arc::new(NativeCancellation {
            control: Arc::clone(&operation_control),
        });
        let response_id = format!("resp_{}", Uuid::new_v4());
        let output_ids = (0..prepared.len())
            .map(|_| format!("msg_{}", Uuid::new_v4()))
            .collect::<Vec<_>>();
        event_tx
            .try_send(GatewayEvent::ResponseCreated {
                request_id: request_id.clone(),
                response_id: response_id.clone(),
                route: request.route.clone(),
            })
            .map_err(|_| {
                GatewayError::unavailable(
                    &request_id,
                    "gateway_event_channel_closed",
                    "the gateway event consumer closed before generation started",
                )
            })?;
        for (output_index, output_id) in output_ids.iter().enumerate() {
            let item = OutputItem::Message {
                id: output_id.clone(),
                role: MessageRole::Assistant,
                content: Vec::new(),
            };
            event_tx
                .try_send(GatewayEvent::OutputItemAdded {
                    request_id: request_id.clone(),
                    output_index,
                    item,
                })
                .map_err(|_| {
                    GatewayError::unavailable(
                        &request_id,
                        "gateway_event_channel_closed",
                        "the gateway event consumer closed before generation started",
                    )
                })?;
            event_tx
                .try_send(GatewayEvent::ContentPartAdded {
                    request_id: request_id.clone(),
                    output_index,
                    content_index: 0,
                    part: ContentBlock::Text {
                        text: String::new(),
                    },
                })
                .map_err(|_| {
                    GatewayError::unavailable(
                        &request_id,
                        "gateway_event_channel_closed",
                        "the gateway event consumer closed before generation started",
                    )
                })?;
        }

        let terminal_for_worker = Arc::clone(&terminal);
        let request_id_for_worker = request_id.clone();
        let route = request.route;
        let previous_response_id = request.request.storage.previous_response_id;
        tokio::task::spawn_blocking(move || {
            let _operation = operation;
            let native_events = native_ticket.events.clone();
            while let Ok(event) = native_events.recv() {
                if let Some(mapped) = map_native_event(&request_id_for_worker, event)
                    && !blocking_send_event(&event_tx, &operation_control, mapped)
                {
                    native_ticket.cancel_all();
                    break;
                }
            }
            let result = native_ticket
                .wait()
                .map_err(|error| map_native_error(&request_id_for_worker, error))
                .and_then(|outputs| {
                    build_response(
                        &request_id_for_worker,
                        &response_id,
                        route,
                        previous_response_id,
                        cache_receipt,
                        output_ids,
                        outputs,
                    )
                });
            if let Ok(response) = &result {
                for (output_index, item) in response.output.iter().enumerate() {
                    if let OutputItem::Message { content, .. } = item {
                        for (content_index, part) in content.iter().enumerate() {
                            let _ = blocking_send_event(
                                &event_tx,
                                &operation_control,
                                GatewayEvent::ContentPartCompleted {
                                    request_id: request_id_for_worker.clone(),
                                    output_index,
                                    content_index,
                                    part: part.clone(),
                                },
                            );
                        }
                    }
                    let _ = blocking_send_event(
                        &event_tx,
                        &operation_control,
                        GatewayEvent::OutputItemCompleted {
                            request_id: request_id_for_worker.clone(),
                            output_index,
                            item: item.clone(),
                        },
                    );
                }
                let _ = blocking_send_event(
                    &event_tx,
                    &operation_control,
                    GatewayEvent::UsageUpdated {
                        request_id: request_id_for_worker.clone(),
                        usage: response.usage.clone(),
                    },
                );
            }
            let terminal_event = match &result {
                Ok(response) if response.status == TerminalStatus::Completed => {
                    GatewayEvent::Completed {
                        request_id: request_id_for_worker.clone(),
                        response: Box::new(response.clone()),
                    }
                }
                Ok(response) => GatewayEvent::Cancelled {
                    request_id: request_id_for_worker.clone(),
                    usage: response.usage.clone(),
                },
                Err(error) if error.class == ErrorClass::Cancelled => GatewayEvent::Cancelled {
                    request_id: request_id_for_worker.clone(),
                    usage: GatewayUsage::default(),
                },
                Err(error) => GatewayEvent::Failed {
                    request_id: request_id_for_worker.clone(),
                    error: error.clone(),
                },
            };
            send_terminal_event(terminal_permit, &terminal_for_worker, terminal_event);
            let _ = final_tx.send(result);
        });

        Ok(GatewayTicket::new(
            request_id,
            event_rx,
            final_rx,
            cancellation,
            terminal,
        ))
    }

    async fn count_tokens(&self, request: BackendRequest) -> Result<GatewayUsage, GatewayError> {
        let request_id = request.request.request_id.clone();
        let input = translate_input(&request.request.input, &request_id)?;
        let (host, config, operation) =
            self.admit_request_configuration(&request.route.model_id, &request_id)?;
        let request_for_count = request_id.clone();
        let control_for_count = operation.control();
        tracked_spawn_blocking(
            &operation,
            &request_id,
            "native token counting",
            move || {
                if control_for_count.request_cancelled.load(Ordering::Acquire) {
                    return Err(backend_cancelled_error(&request_for_count));
                }
                let handle = host
                    .acquire(config)
                    .map_err(|error| map_native_error(&request_for_count, error))?;
                if control_for_count.request_cancelled.load(Ordering::Acquire) {
                    return Err(backend_cancelled_error(&request_for_count));
                }
                let prepared = handle
                    .prepare_input(input)
                    .map_err(|error| map_native_error(&request_for_count, error))?;
                Ok(GatewayUsage {
                    input_tokens: Some(
                        prepared
                            .iter()
                            .map(|prompt| prompt.token_ids.len() as u64)
                            .sum(),
                    ),
                    provenance: UsageProvenance::Exact,
                    selected_route: Some(request.route),
                    real_local_inference: false,
                    ..GatewayUsage::default()
                })
            },
        )
        .await?
    }

    fn cancel(&self, request_id: &RequestId, target: CancelTarget) -> usize {
        self.lifecycle.cancel(request_id, target)
    }

    async fn shutdown(&self) -> Result<(), GatewayError> {
        let operations = self.lifecycle.begin_shutdown();
        loop {
            for operation in &operations {
                // Some synchronous native calls register their cancellation
                // keys immediately after FTE begins draining. Retrying while
                // the operation is still admitted closes that narrow race and
                // remains idempotent at the native flag registry.
                operation.cancel(CancelTarget::Request);
            }
            let changed = self.lifecycle.changed.notified();
            if self.lifecycle.lock_state().phase == BackendPhase::Closed {
                return Ok(());
            }
            tokio::select! {
                () = changed => {}
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    }
}

struct NativeCancellation {
    control: Arc<OperationControl>,
}

impl TicketCancellation for NativeCancellation {
    fn cancel(&self, target: CancelTarget) -> usize {
        self.control.cancel(target)
    }
}

fn blocking_send_event(
    sender: &mpsc::Sender<GatewayEvent>,
    control: &OperationControl,
    mut event: GatewayEvent,
) -> bool {
    loop {
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
            Err(mpsc::error::TrySendError::Full(returned)) => {
                if control.draining.load(Ordering::Acquire)
                    || control.request_cancelled.load(Ordering::Acquire)
                {
                    return false;
                }
                event = returned;
                std::thread::park_timeout(Duration::from_millis(1));
            }
        }
    }
}

fn send_terminal_event(
    permit: mpsc::OwnedPermit<GatewayEvent>,
    terminal_observed: &AtomicBool,
    event: GatewayEvent,
) {
    drop(permit.send(event));
    terminal_observed.store(true, Ordering::Release);
}

fn cancel_native(host: &NativeHost, request_id: &RequestId, target: CancelTarget) -> usize {
    match target {
        CancelTarget::Request => host.cancel(&request_id.0, None),
        CancelTarget::Output(index) => {
            let completion = format!("completion-{index}");
            host.cancel(&request_id.0, Some(&completion))
                + (index == 0) as usize * host.cancel(&request_id.0, Some("assistant"))
        }
    }
}

fn translate_input(
    input: &GenerationInput,
    request_id: &RequestId,
) -> Result<NativeInput, GatewayError> {
    match input {
        GenerationInput::Chat { items } => Ok(NativeInput::Chat {
            messages: items
                .iter()
                .map(|item| translate_message(item, request_id))
                .collect::<Result<Vec<_>, _>>()?,
            template: ChatTemplateChoice::ModelDefault,
        }),
        GenerationInput::Completion { prompts } => Ok(NativeInput::Completion {
            prompts: prompts
                .iter()
                .map(|prompt| match prompt {
                    CompletionPrompt::Text { text, add_bos } => NativeCompletionPrompt::Text {
                        text: text.clone(),
                        special_tokens: if *add_bos {
                            SpecialTokenPolicy::AddBosParseSpecial
                        } else {
                            SpecialTokenPolicy::NoBosParseSpecial
                        },
                    },
                    CompletionPrompt::Tokens { token_ids } => NativeCompletionPrompt::Tokens {
                        token_ids: token_ids.clone(),
                    },
                })
                .collect(),
        }),
        GenerationInput::FillInMiddle { prefix, suffix } => Ok(NativeInput::FillInMiddle {
            prefix: prefix.clone(),
            suffix: suffix.clone(),
        }),
    }
}

fn translate_message(
    item: &InputItem,
    request_id: &RequestId,
) -> Result<ChatMessage, GatewayError> {
    let InputItem::Message { role, content, .. } = item else {
        return Err(GatewayError {
            code: "native_input_item_unsupported".to_string(),
            class: ErrorClass::Capability,
            retryable: false,
            http_status: 400,
            request_id: request_id.clone(),
            provider: Some(BACKEND_ID.to_string()),
            safe_detail: "local generation currently accepts message Items only; tool and reasoning Items require a capable tool-loop edge".to_string(),
        });
    };
    let mut text = String::new();
    for block in content {
        match block {
            ContentBlock::Text { text: part } | ContentBlock::Thinking { text: part, .. } => {
                text.push_str(part);
            }
            _ => {
                return Err(GatewayError {
                    code: "native_content_block_unsupported".to_string(),
                    class: ErrorClass::Capability,
                    retryable: false,
                    http_status: 400,
                    request_id: request_id.clone(),
                    provider: Some(BACKEND_ID.to_string()),
                    safe_detail: "this local model route accepts text content only".to_string(),
                });
            }
        }
    }
    Ok(ChatMessage {
        role: match role {
            MessageRole::System | MessageRole::Developer => ChatRole::System,
            MessageRole::User => ChatRole::User,
            MessageRole::Assistant => ChatRole::Assistant,
            MessageRole::Tool => ChatRole::Tool,
        },
        content: text,
    })
}

fn translate_sampling(options: &fte_types::SamplingOptions) -> SamplingConfig {
    let mut sampling = SamplingConfig::default();
    if let Some(value) = options.max_output_tokens {
        sampling.max_tokens = value;
    }
    if let Some(value) = options.temperature {
        sampling.temperature = value;
    }
    if let Some(value) = options.top_p {
        sampling.top_p = value;
    }
    if let Some(value) = options.top_k {
        sampling.top_k = value;
    }
    if let Some(value) = options.min_p {
        sampling.min_p = value;
    }
    if let Some(value) = options.seed {
        sampling.seed = value;
    }
    if let Some(value) = options.presence_penalty {
        sampling.presence_penalty = value;
    }
    if let Some(value) = options.frequency_penalty {
        sampling.frequency_penalty = value;
    }
    sampling.stop.clone_from(&options.stop);
    sampling
}

fn map_native_event(
    request_id: &RequestId,
    event: llama_native_types::GenerationEvent,
) -> Option<GatewayEvent> {
    match event.event {
        NativeEventKind::Delta { text } => Some(GatewayEvent::TextDelta {
            request_id: request_id.clone(),
            output_index: event.input_index,
            content_index: 0,
            delta: text,
        }),
        NativeEventKind::Warning { code, message } => Some(GatewayEvent::Warning {
            request_id: request_id.clone(),
            code,
            message,
        }),
        NativeEventKind::State { .. } => None,
    }
}

fn build_response(
    request_id: &RequestId,
    response_id: &str,
    route: ResolvedRoute,
    previous_response_id: Option<String>,
    cache_receipt: CacheReceipt,
    output_ids: Vec<String>,
    outputs: Vec<NativeOutput>,
) -> Result<GatewayResponse, GatewayError> {
    let cancelled = outputs
        .iter()
        .any(|output| output.state == NativeState::Cancelled);
    let failed = outputs
        .iter()
        .any(|output| output.state == NativeState::Failed);
    if failed {
        return Err(GatewayError::unavailable(
            request_id,
            "native_generation_failed",
            "the local runtime reported a failed terminal state",
        ));
    }
    let usage = GatewayUsage {
        input_tokens: Some(
            outputs
                .iter()
                .map(|output| output.metrics.prompt_tokens as u64)
                .sum(),
        ),
        output_tokens: Some(
            outputs
                .iter()
                .map(|output| output.metrics.completion_tokens as u64)
                .sum(),
        ),
        reasoning_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        provenance: UsageProvenance::Exact,
        queue_ms: None,
        model_load_ms: None,
        time_to_first_token_ms: outputs
            .iter()
            .filter_map(|output| output.metrics.first_token_ms)
            .min()
            .map(|value| value as u64),
        total_ms: outputs
            .iter()
            .map(|output| output.metrics.duration_ms)
            .max()
            .map(|value| value as u64),
        selected_route: Some(route.clone()),
        cache: Some(cache_receipt),
        real_local_inference: outputs.iter().any(|output| {
            output.real_engine_invoked
                && !output.fake_fixture
                && output.transport == llama_native_types::NativeTransport::InProcess
        }),
    };
    Ok(GatewayResponse {
        id: response_id.to_string(),
        request_id: request_id.clone(),
        model: route.model_id.clone(),
        route,
        output: outputs
            .into_iter()
            .zip(output_ids)
            .map(|(output, id)| OutputItem::Message {
                id,
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text { text: output.text }],
            })
            .collect(),
        usage,
        status: if cancelled {
            TerminalStatus::Cancelled
        } else {
            TerminalStatus::Completed
        },
        previous_response_id,
    })
}

fn map_model_descriptor(native: &llama_native_types::NativeModelDescriptor) -> ModelDescriptor {
    ModelDescriptor {
        id: native.model_id.clone(),
        display_name: native.display_name.clone(),
        backend_id: BACKEND_ID.to_string(),
        location: BackendLocation::LocalEmbedded,
        capabilities: ModelCapabilities {
            prompt_forms: native
                .capabilities
                .prompt_forms
                .iter()
                .map(|form| match form {
                    NativePromptForm::Chat => PromptForm::Chat,
                    NativePromptForm::Completion => PromptForm::Completion,
                    NativePromptForm::FillInMiddle => PromptForm::FillInMiddle,
                })
                .collect(),
            modalities: if native.capabilities.multimodal {
                vec![Modality::Text, Modality::Image, Modality::Audio]
            } else {
                vec![Modality::Text]
            },
            tools: false,
            structured_output: false,
            reasoning: false,
            streaming: native.capabilities.streaming,
            provider_cache: false,
        },
        context_tokens: Some(native.context_tokens),
        max_output_tokens: None,
        observed: RouteObservations::default(),
    }
}

fn configured_model_descriptor(config: &NativeModelConfig) -> ModelDescriptor {
    ModelDescriptor {
        id: config.model_id.clone(),
        display_name: config
            .model_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(&config.model_id)
            .to_string(),
        backend_id: BACKEND_ID.to_string(),
        location: BackendLocation::LocalEmbedded,
        capabilities: ModelCapabilities {
            prompt_forms: vec![PromptForm::Chat, PromptForm::Completion],
            modalities: if config.mmproj_path.is_some() {
                vec![Modality::Text, Modality::Image, Modality::Audio]
            } else {
                vec![Modality::Text]
            },
            tools: false,
            structured_output: false,
            reasoning: false,
            streaming: true,
            provider_cache: false,
        },
        context_tokens: Some(config.context_tokens),
        max_output_tokens: None,
        observed: RouteObservations::default(),
    }
}

fn native_cache_fingerprint(
    handle: &llama_native_engine::NativeModelHandle,
    prepared: Option<&PreparedPrompt>,
) -> Result<CacheFingerprint, GatewayError> {
    let request_id = RequestId::new();
    let prepared = prepared.ok_or_else(|| {
        GatewayError::invalid_request(
            &request_id,
            "native_prompt_missing",
            "the local request produced no prepared prompt",
        )
    })?;
    let status = handle.status();
    let model = status.fingerprint.ok_or_else(|| {
        GatewayError::unavailable(
            &request_id,
            "native_fingerprint_missing",
            "the resident local model did not expose its cache fingerprint",
        )
    })?;
    Ok(CacheFingerprint {
        prompt_form: prepared.prompt_form,
        prompt_token_policy: prepared.token_policy,
        model_sha256: model.model_sha256,
        binding_version: model.binding_version,
        build_id: model.build_id,
        tokenizer_sha256: model.tokenizer_sha256,
        chat_template_sha256: model.chat_template_sha256,
        multimodal_projector_sha256: model.multimodal_projector_sha256,
        lora_adapters_sha256: Vec::new(),
        context_tokens: model.context_tokens,
        batch_tokens: model.batch_tokens,
        max_sequences: model.max_sequences,
        device: model.backend,
        rope_config_sha256: model.rope_config_sha256,
        kv_layout_sha256: model.kv_layout_sha256,
    })
}

fn cache_owner(policy: &CachePolicy) -> Option<String> {
    policy.owner_namespace.as_ref().map(|namespace| {
        format!(
            "{}:{}",
            namespace,
            policy.owner_version.as_deref().unwrap_or("unversioned")
        )
    })
}

fn validate_cache_policy(
    policy: &CachePolicy,
    input: &GenerationInput,
    request_id: &RequestId,
) -> Result<(), GatewayError> {
    if policy.mode == CacheMode::ProviderNative && policy.requirement == CacheRequirement::Required
    {
        return Err(GatewayError {
            code: "provider_cache_unavailable_on_local_route".to_string(),
            class: ErrorClass::Capability,
            retryable: false,
            http_status: 409,
            request_id: request_id.clone(),
            provider: Some(BACKEND_ID.to_string()),
            safe_detail:
                "provider-native caching cannot be required on an embedded llama.cpp route"
                    .to_string(),
        });
    }
    if policy.mode != CacheMode::StablePrefix {
        return Ok(());
    }
    if cache_owner(policy).is_none() {
        return Err(GatewayError::invalid_request(
            request_id,
            "stable_prefix_owner_missing",
            "stable prefix caching requires an owner namespace and version",
        ));
    }
    let GenerationInput::Chat { items } = input else {
        return Err(GatewayError {
            code: "stable_prefix_requires_chat".to_string(),
            class: ErrorClass::Capability,
            retryable: false,
            http_status: 400,
            request_id: request_id.clone(),
            provider: Some(BACKEND_ID.to_string()),
            safe_detail: "stable prefix packs currently require canonical chat Items".to_string(),
        });
    };
    let Some(boundary) = policy.stable_prefix_items else {
        return Err(GatewayError::invalid_request(
            request_id,
            "stable_prefix_boundary_missing",
            "stable prefix caching requires the number of leading stable chat Items",
        ));
    };
    if boundary == 0 || boundary > items.len() {
        return Err(GatewayError::invalid_request(
            request_id,
            "stable_prefix_boundary_invalid",
            "the stable prefix boundary must identify at least one existing leading chat Item",
        ));
    }
    Ok(())
}

const fn cache_lookup_enabled(mode: CacheMode) -> bool {
    !matches!(mode, CacheMode::Disabled | CacheMode::ProviderNative)
}

fn requested_cache_tier(mode: CacheMode) -> CacheTier {
    match mode {
        CacheMode::Persistent => CacheTier::PersistentPrefix,
        CacheMode::StablePrefix => CacheTier::StablePrefixPack,
        CacheMode::ProviderNative => CacheTier::ProviderNative,
        CacheMode::Memory | CacheMode::Adaptive => CacheTier::MemoryPrefix,
        CacheMode::Disabled => CacheTier::None,
    }
}

fn cache_receipt(
    policy: &CachePolicy,
    cached: Option<&PrefixCacheValue>,
    miss_reason: Option<String>,
) -> CacheReceipt {
    if policy.mode == CacheMode::Disabled {
        return CacheReceipt {
            tier: CacheTier::None,
            outcome: CacheOutcome::Disabled,
            reason: Some("disabled by request policy".to_string()),
        };
    }
    if policy.mode == CacheMode::ProviderNative {
        return CacheReceipt {
            tier: CacheTier::ProviderNative,
            outcome: CacheOutcome::Rejected,
            reason: Some(
                "provider-native caching does not apply to embedded llama.cpp".to_string(),
            ),
        };
    }
    match cached {
        Some(value) => CacheReceipt {
            tier: match value.metadata.tier {
                NativeCacheTier::MemoryLru => CacheTier::MemoryPrefix,
                NativeCacheTier::SessionPersistent => CacheTier::PersistentPrefix,
                NativeCacheTier::PersonaPack => CacheTier::StablePrefixPack,
            },
            outcome: CacheOutcome::Hit,
            reason: None,
        },
        None => CacheReceipt {
            tier: requested_cache_tier(policy.mode),
            outcome: CacheOutcome::Miss,
            reason: miss_reason.or_else(|| {
                Some("no compatible prefix fingerprint and token sequence matched".to_string())
            }),
        },
    }
}

const MINIMUM_AUTOMATIC_PREFIX_TOKENS: usize = 256;

struct PrefixPrefillSpec<'a> {
    request_id: &'a RequestId,
    model_id: &'a str,
    input: &'a GenerationInput,
    sampling: &'a SamplingConfig,
    full_prompt: &'a PreparedPrompt,
    fingerprint: &'a CacheFingerprint,
    policy: &'a CachePolicy,
}

fn prefill_cache_prefix(
    handle: &llama_native_engine::NativeModelHandle,
    spec: PrefixPrefillSpec<'_>,
) -> Result<Option<PrefixCacheValue>, GatewayError> {
    let PrefixPrefillSpec {
        request_id,
        model_id,
        input,
        sampling,
        full_prompt,
        fingerprint,
        policy,
    } = spec;
    let GenerationInput::Chat { items } = input else {
        return Ok(None);
    };
    let boundary = policy.stable_prefix_items.unwrap_or(items.len());
    if boundary == 0 || boundary > items.len() {
        return Err(GatewayError::invalid_request(
            request_id,
            "cache_prefix_boundary_invalid",
            "the cache prefix boundary must identify at least one existing leading chat Item",
        ));
    }
    let common_messages = items[..boundary]
        .iter()
        .map(|item| translate_message(item, request_id))
        .collect::<Result<Vec<_>, _>>()?;
    let mut probe_sampling = sampling.clone();
    probe_sampling.max_tokens = probe_sampling.max_tokens.max(1);
    let branches = ["cache-probe-a", "cache-probe-b"]
        .into_iter()
        .map(|branch_id| BranchRequest {
            branch_id: branch_id.to_string(),
            label: branch_id.to_string(),
            instruction: String::new(),
            sampling: probe_sampling.clone(),
            messages: Vec::new(),
            cached_prefix: None,
        })
        .collect();
    let sequence = handle
        .prefill_shared_prefix(SharedPrefixBatchRequest {
            request_id: format!("{}:prefix", request_id.0),
            model_id: model_id.to_string(),
            common_messages,
            chat_template: ChatTemplateChoice::ModelDefault,
            branches,
            cached_prefix: None,
        })
        .map_err(|error| map_native_error(request_id, error))?;
    if sequence.token_ids.len() >= full_prompt.token_ids.len()
        || !full_prompt.token_ids.starts_with(&sequence.token_ids)
    {
        return Err(GatewayError {
            code: "native_cache_prefix_not_reusable".to_string(),
            class: ErrorClass::Capability,
            retryable: false,
            http_status: 409,
            request_id: request_id.clone(),
            provider: Some(BACKEND_ID.to_string()),
            safe_detail: "the exact rendered stable prefix is not a strict token prefix of the submitted chat"
                .to_string(),
        });
    }
    if sequence.token_ids.len() < MINIMUM_AUTOMATIC_PREFIX_TOKENS {
        return Ok(None);
    }
    let tier = match policy.mode {
        CacheMode::Persistent => NativeCacheTier::SessionPersistent,
        CacheMode::StablePrefix => NativeCacheTier::PersonaPack,
        CacheMode::Memory | CacheMode::Adaptive => NativeCacheTier::MemoryLru,
        CacheMode::Disabled | CacheMode::ProviderNative => return Ok(None),
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let mut metadata = PrefixCacheMetadata::new(
        format!("prefix_{}", Uuid::new_v4()),
        tier,
        fingerprint.clone(),
        sequence.token_ids.clone(),
        sequence.bytes.len(),
        now_ms,
    );
    if let Some(owner) = cache_owner(policy) {
        metadata = metadata.with_owner(owner);
    }
    Ok(Some(PrefixCacheValue { metadata, sequence }))
}

fn blocking_task_error(
    request_id: &RequestId,
    operation: &str,
    error: tokio::task::JoinError,
) -> GatewayError {
    let failure = if error.is_cancelled() {
        "was cancelled before completion"
    } else {
        "terminated unexpectedly"
    };
    GatewayError {
        code: "native_blocking_task_failed".to_string(),
        class: ErrorClass::Internal,
        retryable: false,
        http_status: 500,
        request_id: request_id.clone(),
        provider: Some(BACKEND_ID.to_string()),
        safe_detail: format!("{operation} {failure}"),
    }
}

fn map_native_error(request_id: &RequestId, error: NativeError) -> GatewayError {
    let code = error.code;
    let (class, retryable, http_status) = match code {
        NativeErrorCode::InvalidConfig
        | NativeErrorCode::PromptTooLarge
        | NativeErrorCode::UnsupportedPromptForm
        | NativeErrorCode::UnsupportedParameter => (ErrorClass::InvalidRequest, false, 400),
        NativeErrorCode::ModelMissing | NativeErrorCode::ModelNotLoaded => {
            (ErrorClass::Unavailable, false, 503)
        }
        NativeErrorCode::ModelInvalid
        | NativeErrorCode::ModelLoadFailed
        | NativeErrorCode::ModelSlotsFull
        | NativeErrorCode::MemoryBudgetExceeded
        | NativeErrorCode::ContextCreateFailed
        | NativeErrorCode::WorkerStopped => (ErrorClass::Unavailable, true, 503),
        NativeErrorCode::ModelInUse | NativeErrorCode::DuplicateActiveRequest => {
            (ErrorClass::Unavailable, false, 409)
        }
        NativeErrorCode::QueueFull => (ErrorClass::RateLimit, true, 429),
        NativeErrorCode::Cancelled => (ErrorClass::Cancelled, false, 499),
        NativeErrorCode::UnsupportedMedia => (ErrorClass::Capability, false, 422),
        NativeErrorCode::CacheIncompatible => (ErrorClass::Capability, false, 409),
        NativeErrorCode::DecodeFailed | NativeErrorCode::Internal => {
            (ErrorClass::Internal, false, 500)
        }
    };
    GatewayError {
        code: format!("native_{code}"),
        class,
        retryable,
        http_status,
        request_id: request_id.clone(),
        provider: Some(BACKEND_ID.to_string()),
        safe_detail: native_error_safe_detail(code).to_string(),
    }
}

const fn native_error_safe_detail(code: NativeErrorCode) -> &'static str {
    match code {
        NativeErrorCode::InvalidConfig => "the local native request configuration is invalid",
        NativeErrorCode::ModelMissing => "the configured local model file is unavailable",
        NativeErrorCode::ModelInvalid => "the configured local model did not pass validation",
        NativeErrorCode::ModelLoadFailed => "llama.cpp could not load the configured local model",
        NativeErrorCode::ModelNotLoaded => "the requested local model is not resident",
        NativeErrorCode::ModelInUse => "the requested local model slot is in use",
        NativeErrorCode::DuplicateActiveRequest => {
            "the local native request identifier is already active"
        }
        NativeErrorCode::ModelSlotsFull => "all configured local model slots are occupied",
        NativeErrorCode::MemoryBudgetExceeded => {
            "the local model exceeds the configured memory budget"
        }
        NativeErrorCode::ContextCreateFailed => {
            "llama.cpp could not create the configured local context"
        }
        NativeErrorCode::PromptTooLarge => "the local prompt exceeds the admitted context budget",
        NativeErrorCode::UnsupportedPromptForm => {
            "the local model does not support the requested prompt form"
        }
        NativeErrorCode::UnsupportedParameter => {
            "the local backend does not support a requested parameter"
        }
        NativeErrorCode::UnsupportedMedia => "the local model does not support the requested media",
        NativeErrorCode::DecodeFailed => "llama.cpp failed while decoding local model output",
        NativeErrorCode::Cancelled => "the local native request was cancelled",
        NativeErrorCode::QueueFull => "the local native command queue is full",
        NativeErrorCode::WorkerStopped => "the local native model worker is stopped",
        NativeErrorCode::CacheIncompatible => {
            "the local prefix cache is incompatible with this request"
        }
        NativeErrorCode::Internal => "the local native backend failed internally",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fte_types::SamplingOptions;
    use llama_native_host::NativeHostConfig;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn completion_translation_preserves_whitespace_and_exact_tokens() {
        let request_id = RequestId::new();
        let input = GenerationInput::Completion {
            prompts: vec![
                CompletionPrompt::Text {
                    text: "  exact whitespace\n".to_string(),
                    add_bos: false,
                },
                CompletionPrompt::Tokens {
                    token_ids: vec![1, 2, 3],
                },
            ],
        };
        let translated = translate_input(&input, &request_id).expect("translate completion");
        let NativeInput::Completion { prompts } = translated else {
            panic!("completion must remain completion input");
        };
        assert_eq!(
            prompts[0],
            NativeCompletionPrompt::Text {
                text: "  exact whitespace\n".to_string(),
                special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
            }
        );
        assert_eq!(
            prompts[1],
            NativeCompletionPrompt::Tokens {
                token_ids: vec![1, 2, 3]
            }
        );
    }

    #[test]
    fn sampling_translation_does_not_invent_unset_values() {
        let native = translate_sampling(&SamplingOptions {
            max_output_tokens: Some(42),
            temperature: Some(0.2),
            ..SamplingOptions::default()
        });
        assert_eq!(native.max_tokens, 42);
        assert_eq!(native.temperature, 0.2);
        assert_eq!(native.top_p, SamplingConfig::default().top_p);
    }

    #[test]
    fn native_errors_are_structurally_classified() {
        let error = map_native_error(
            &RequestId::new(),
            NativeError::new(NativeErrorCode::MemoryBudgetExceeded, "full"),
        );
        assert_eq!(error.code, "native_memory_budget_exceeded");
        assert_eq!(error.class, ErrorClass::Unavailable);
        assert!(error.retryable);

        let unsupported_media = map_native_error(
            &RequestId::new(),
            NativeError::new(
                NativeErrorCode::UnsupportedMedia,
                "the configured model does not accept audio input",
            ),
        );
        assert_eq!(unsupported_media.code, "native_unsupported_media");
        assert_eq!(unsupported_media.class, ErrorClass::Capability);
        assert_eq!(unsupported_media.http_status, 422);
        assert!(!unsupported_media.retryable);

        let model_in_use = map_native_error(
            &RequestId::new(),
            NativeError::new(NativeErrorCode::ModelInUse, "slot owns another model"),
        );
        assert_eq!(model_in_use.code, "native_model_in_use");
        assert_eq!(model_in_use.class, ErrorClass::Unavailable);
        assert_eq!(model_in_use.http_status, 409);
        assert!(!model_in_use.retryable);

        let queue_full = map_native_error(
            &RequestId::new(),
            NativeError::new(NativeErrorCode::QueueFull, "command queue is full"),
        );
        assert_eq!(queue_full.code, "native_queue_full");
        assert_eq!(queue_full.class, ErrorClass::RateLimit);
        assert_eq!(queue_full.http_status, 429);

        let duplicate = map_native_error(
            &RequestId::new(),
            NativeError::new(
                NativeErrorCode::DuplicateActiveRequest,
                "untrusted native detail",
            ),
        );
        assert_eq!(duplicate.class, ErrorClass::Unavailable);
        assert!(!duplicate.retryable);
        assert_eq!(duplicate.http_status, 409);
        assert_eq!(
            duplicate.safe_detail,
            "the local native request identifier is already active"
        );
        assert!(!duplicate.safe_detail.contains("untrusted"));
        assert!(queue_full.retryable);

        let private_path = "/Users/private/models/secret-model.gguf";
        let load_error = map_native_error(
            &RequestId::new(),
            NativeError::new(
                NativeErrorCode::ModelLoadFailed,
                format!("failed to mmap {private_path}"),
            ),
        );
        assert!(!load_error.safe_detail.contains(private_path));
        assert!(!load_error.safe_detail.contains("/Users/"));
    }

    #[test]
    fn product_host_reconfiguration_replaces_host_and_models_atomically() {
        let first_host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let backend = LlamaNativeBackend::new(Arc::clone(&first_host));
        backend
            .replace_configuration(
                Arc::clone(&first_host),
                [NativeModelConfig::local(PathBuf::from("first.gguf"))],
            )
            .expect("first configuration");
        assert_eq!(backend.descriptor().models[0].id, "first");
        let request_id = RequestId::new();
        let (first_snapshot_host, first_snapshot_model, _first_operation) = backend
            .admit_request_configuration("first", &request_id)
            .expect("first request configuration");
        assert!(Arc::ptr_eq(&first_snapshot_host, &first_host));
        assert_eq!(first_snapshot_model.model_id, "first");

        let second_host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        backend
            .replace_configuration(
                Arc::clone(&second_host),
                [NativeModelConfig::local(PathBuf::from("second.gguf"))],
            )
            .expect("second configuration");
        let models = backend.descriptor().models;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "second");
        let (second_snapshot_host, second_snapshot_model, _second_operation) = backend
            .admit_request_configuration("second", &request_id)
            .expect("second request configuration");
        assert!(Arc::ptr_eq(&second_snapshot_host, &second_host));
        assert_eq!(second_snapshot_model.model_id, "second");

        assert!(Arc::ptr_eq(&first_snapshot_host, &first_host));
        assert_eq!(first_snapshot_model.model_id, "first");
    }

    #[test]
    fn cancellation_after_rebind_targets_the_operation_captured_host() {
        let first_host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let backend = LlamaNativeBackend::new_borrowed(Arc::clone(&first_host));
        backend
            .replace_configuration(
                Arc::clone(&first_host),
                [NativeModelConfig::local(PathBuf::from("first.gguf"))],
            )
            .expect("first configuration");
        let request_id = RequestId::new();
        let (captured_host, _, operation) = backend
            .admit_request_configuration("first", &request_id)
            .expect("capture first operation");

        let second_host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        backend
            .replace_configuration(
                Arc::clone(&second_host),
                [NativeModelConfig::local(PathBuf::from("second.gguf"))],
            )
            .expect("second configuration");
        let control = operation.control();
        control.register_native_request_id(format!("{}:prefix", request_id.0));
        assert!(Arc::ptr_eq(&captured_host, &first_host));
        assert!(Arc::ptr_eq(&control.host, &first_host));
        assert!(!Arc::ptr_eq(&control.host, &second_host));

        assert_eq!(backend.cancel(&request_id, CancelTarget::Output(2)), 1);
        assert_eq!(backend.cancel(&request_id, CancelTarget::Output(2)), 0);
        assert_eq!(backend.cancel(&request_id, CancelTarget::Request), 1);
        assert_eq!(backend.cancel(&request_id, CancelTarget::Request), 0);
        assert!(control.request_cancelled.load(Ordering::Acquire));
        assert_eq!(
            control
                .native_request_ids
                .lock()
                .expect("native request ids")
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![request_id.0.clone(), format!("{}:prefix", request_id.0)]
        );
    }

    #[test]
    fn same_host_replacement_rejects_a_stale_registration_commit() {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let backend = LlamaNativeBackend::new_borrowed(Arc::clone(&host));
        backend
            .replace_configuration(
                Arc::clone(&host),
                [NativeModelConfig::local(PathBuf::from("initial.gguf"))],
            )
            .expect("initial configuration");
        let request_id = RequestId::new();
        let captured = backend
            .configuration_snapshot(&request_id)
            .expect("capture configuration");
        backend
            .replace_configuration(
                Arc::clone(&host),
                [NativeModelConfig::local(PathBuf::from("replacement.gguf"))],
            )
            .expect("same-host replacement");

        let error = backend
            .commit_registered_model(
                &request_id,
                &captured.host,
                captured.epoch,
                NativeModelConfig::local(PathBuf::from("stale.gguf")),
            )
            .expect_err("stale registration must not contaminate replacement registry");
        assert_eq!(error.code, "native_configuration_changed");
        assert_eq!(
            backend
                .descriptor()
                .models
                .into_iter()
                .map(|model| model.id)
                .collect::<Vec<_>>(),
            vec!["replacement".to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_waits_for_orphaned_blocking_work_and_keeps_host_borrowed() {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let backend = Arc::new(LlamaNativeBackend::new_borrowed(Arc::clone(&host)));
        backend
            .configure_model(NativeModelConfig::local(PathBuf::from("configured.gguf")))
            .expect("configure model");
        let request_id = RequestId::new();
        let (_, _, operation) = backend
            .admit_request_configuration("configured", &request_id)
            .expect("admit operation");
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let request_for_task = request_id.clone();
        let outer = tokio::spawn(async move {
            tracked_spawn_blocking(
                &operation,
                &request_for_task,
                "test blocking operation",
                move || {
                    let _ = started_tx.send(());
                    let _ = release_rx.recv();
                },
            )
            .await
        });
        started_rx.await.expect("blocking operation started");
        outer.abort();
        let _ = outer.await;

        let backend_for_shutdown = Arc::clone(&backend);
        let mut shutdown = tokio::spawn(async move { backend_for_shutdown.shutdown().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut shutdown)
                .await
                .is_err(),
            "shutdown must wait for detached spawn_blocking work"
        );
        let error = backend
            .configure_model(NativeModelConfig::local(PathBuf::from("late.gguf")))
            .expect_err("configuration after quiesce must fail");
        assert_eq!(error.code, "native_backend_closed");
        let error = backend
            .replace_configuration(
                Arc::clone(&host),
                [NativeModelConfig::local(PathBuf::from("replacement.gguf"))],
            )
            .expect_err("replacement after quiesce must fail");
        assert_eq!(error.code, "native_backend_closed");
        let error = backend
            .register_model(NativeModelConfig::local(PathBuf::from("late-load.gguf")))
            .expect_err("model loading after quiesce must fail before file access");
        assert_eq!(error.code, "native_backend_closed");
        let error = match backend.admit_request_configuration("configured", &RequestId::new()) {
            Ok(_) => panic!("new request admission after quiesce must fail"),
            Err(error) => error,
        };
        assert_eq!(error.code, "native_backend_closed");
        assert!(matches!(
            backend.readiness(),
            BackendReadiness::Unavailable { .. }
        ));

        release_tx.send(()).expect("release blocking operation");
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("shutdown completed after work terminated")
            .expect("shutdown task joined")
            .expect("backend drain succeeded");
        host.shutdown_joined()
            .expect("adapter shutdown must leave borrowed host ownership intact");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_interrupts_a_full_undrained_event_channel() {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let control = Arc::new(OperationControl {
            request_id: RequestId::new(),
            host,
            native_request_ids: Mutex::new(BTreeSet::new()),
            request_cancelled: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            cancelled_outputs: Mutex::new(BTreeSet::new()),
        });
        let (event_tx, mut event_rx) = mpsc::channel(2);
        let terminal_permit = event_tx
            .clone()
            .try_reserve_owned()
            .expect("reserve terminal event capacity");
        event_tx
            .try_send(GatewayEvent::Warning {
                request_id: control.request_id.clone(),
                code: "first".to_string(),
                message: "fills the queue".to_string(),
            })
            .expect("fill event queue");
        let control_for_send = Arc::clone(&control);
        let sender = tokio::task::spawn_blocking(move || {
            blocking_send_event(
                &event_tx,
                &control_for_send,
                GatewayEvent::Warning {
                    request_id: control_for_send.request_id.clone(),
                    code: "second".to_string(),
                    message: "must not pin drain".to_string(),
                },
            )
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        control.draining.store(true, Ordering::Release);
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), sender)
                .await
                .expect("blocked sender noticed drain")
                .expect("blocking sender joined")
        );
        let terminal_observed = AtomicBool::new(false);
        send_terminal_event(
            terminal_permit,
            &terminal_observed,
            GatewayEvent::Cancelled {
                request_id: control.request_id.clone(),
                usage: GatewayUsage::default(),
            },
        );
        assert!(terminal_observed.load(Ordering::Acquire));
        let mut terminal_count = 0;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(
                event,
                GatewayEvent::Completed { .. }
                    | GatewayEvent::Cancelled { .. }
                    | GatewayEvent::Failed { .. }
            ) {
                terminal_count += 1;
            }
        }
        assert_eq!(terminal_count, 1, "exactly one terminal event is retained");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_cancellation_interrupts_a_full_undrained_event_channel() {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let request_id = RequestId::new();
        let control = Arc::new(OperationControl {
            native_request_ids: Mutex::new(BTreeSet::from([request_id.0.clone()])),
            request_id,
            host,
            request_cancelled: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            cancelled_outputs: Mutex::new(BTreeSet::new()),
        });
        let (event_tx, mut event_rx) = mpsc::channel(2);
        let terminal_permit = event_tx
            .clone()
            .try_reserve_owned()
            .expect("reserve terminal event capacity");
        event_tx
            .try_send(GatewayEvent::Warning {
                request_id: control.request_id.clone(),
                code: "first".to_string(),
                message: "fills the queue".to_string(),
            })
            .expect("fill event queue");
        let control_for_send = Arc::clone(&control);
        let sender = tokio::task::spawn_blocking(move || {
            blocking_send_event(
                &event_tx,
                &control_for_send,
                GatewayEvent::Warning {
                    request_id: control_for_send.request_id.clone(),
                    code: "second".to_string(),
                    message: "must not pin cancellation".to_string(),
                },
            )
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        control.cancel(CancelTarget::Request);
        assert!(
            !tokio::time::timeout(Duration::from_secs(1), sender)
                .await
                .expect("blocked sender noticed request cancellation")
                .expect("blocking sender joined")
        );
        let terminal_observed = AtomicBool::new(false);
        send_terminal_event(
            terminal_permit,
            &terminal_observed,
            GatewayEvent::Cancelled {
                request_id: control.request_id.clone(),
                usage: GatewayUsage::default(),
            },
        );
        assert!(terminal_observed.load(Ordering::Acquire));
        let mut terminal_count = 0;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(
                event,
                GatewayEvent::Completed { .. }
                    | GatewayEvent::Cancelled { .. }
                    | GatewayEvent::Failed { .. }
            ) {
                terminal_count += 1;
            }
        }
        assert_eq!(terminal_count, 1, "exactly one terminal event is retained");
    }

    #[test]
    fn stable_prefix_requires_an_owned_explicit_leading_item_boundary() {
        let request_id = RequestId::new();
        let input = GenerationInput::Chat {
            items: vec![InputItem::Message {
                id: None,
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: "stable persona".to_string(),
                }],
            }],
        };
        let mut policy = CachePolicy {
            mode: CacheMode::StablePrefix,
            owner_namespace: Some("persona:one".to_string()),
            owner_version: Some("7".to_string()),
            ..CachePolicy::default()
        };
        let error = validate_cache_policy(&policy, &input, &request_id)
            .expect_err("an implicit stable boundary must fail");
        assert_eq!(error.code, "stable_prefix_boundary_missing");

        policy.stable_prefix_items = Some(1);
        validate_cache_policy(&policy, &input, &request_id)
            .expect("an owned explicit prefix must pass");
    }

    #[test]
    fn cold_receipts_report_the_requested_local_tier() {
        let receipt = cache_receipt(
            &CachePolicy {
                mode: CacheMode::Persistent,
                ..CachePolicy::default()
            },
            None,
            Some("cold".to_string()),
        );
        assert_eq!(receipt.tier, CacheTier::PersistentPrefix);
        assert_eq!(receipt.outcome, CacheOutcome::Miss);
        assert_eq!(receipt.reason.as_deref(), Some("cold"));
    }
}
