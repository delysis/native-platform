//! Privacy-first route planning and backend orchestration.

pub mod operation_lifecycle;

use fte_types::{
    BackendDescriptor, BackendLocation, BackendRequest, CancelTarget, ErrorClass, GatewayBackend,
    GatewayError, GatewayLifecycle, GatewayRequest, GatewayStatus, GatewayTicket, GatewayUsage,
    ModelDescriptor, ModelSelector, PrivacyPolicy, RequestId, ResolvedRoute, ResponseFormat,
    RouteProfile, TerminalStatus, TicketLifecycleLease,
};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

const DEFAULT_BACKEND_CONCURRENCY: usize = 4;
const DEFAULT_QUEUE_MS: u64 = 30_000;
const CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
const CIRCUIT_OPEN_DURATION: Duration = Duration::from_secs(30);
const NEUTRAL_ROUTE_SIGNAL: f64 = 0.5;
const HEADROOM_WEIGHT: f64 = 0.35;
const EVALUATION_WEIGHT: f64 = 0.30;
const CAPABILITY_WEIGHT: f64 = 0.20;
const LATENCY_WEIGHT: f64 = 0.15;
type RouteResolution = (Arc<dyn GatewayBackend>, ResolvedRoute, Arc<Semaphore>);

#[derive(Debug, Clone)]
pub struct GatewayDefaults {
    pub catalog_version: String,
}

impl Default for GatewayDefaults {
    fn default() -> Self {
        Self {
            catalog_version: "embedded-v1".to_string(),
        }
    }
}

#[derive(Default)]
struct GatewayState {
    backends: BTreeMap<String, Arc<dyn GatewayBackend>>,
    backend_admission: BTreeMap<String, Arc<Semaphore>>,
    all_admission: Vec<Arc<Semaphore>>,
    backend_circuits: BTreeMap<String, BackendCircuit>,
    response_affinity: BTreeMap<String, (String, String)>,
}

#[derive(Debug, Default)]
struct BackendCircuit {
    consecutive_failures: u32,
    open_until: Option<Instant>,
}

impl BackendCircuit {
    fn is_available(&self, now: Instant) -> bool {
        self.open_until.is_none_or(|open_until| now >= open_until)
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.open_until = None;
    }

    fn record_failure(&mut self, now: Instant) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= CIRCUIT_FAILURE_THRESHOLD {
            self.open_until = Some(now + CIRCUIT_OPEN_DURATION);
        }
    }
}

#[derive(Default)]
struct LifecycleState {
    phase: GatewayLifecycle,
    shutdown_error: Option<GatewayError>,
    shutdown_expected_worker_ids: Vec<String>,
    shutdown_joined_worker_ids: Vec<String>,
    shutdown_retained_tasks: usize,
}

#[derive(Default)]
struct LifecycleControl {
    state: Mutex<LifecycleState>,
    operations: operation_lifecycle::OperationRegistry,
    changed: Notify,
}

enum ShutdownDisposition {
    Lead(Vec<RequestId>),
    Wait,
    Complete(GatewayShutdownReport),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GatewayShutdownReport {
    result: Result<(), GatewayError>,
    expected_worker_ids: Vec<String>,
    joined_worker_ids: Vec<String>,
    retained_tasks: usize,
}

impl LifecycleControl {
    fn ensure_running(&self, request_id: &RequestId) -> Result<(), GatewayError> {
        let state = self.lock_state(request_id)?;
        if state.phase == GatewayLifecycle::Running {
            Ok(())
        } else {
            Err(gateway_closed_error(request_id, state.phase))
        }
    }

    fn while_running<T>(
        &self,
        request_id: &RequestId,
        operation: impl FnOnce() -> Result<T, GatewayError>,
    ) -> Result<T, GatewayError> {
        let state = self.lock_state(request_id)?;
        if state.phase != GatewayLifecycle::Running {
            return Err(gateway_closed_error(request_id, state.phase));
        }
        operation()
    }

    fn register_request(
        &self,
        request_id: &RequestId,
        progress_capacity: usize,
    ) -> Result<operation_lifecycle::OperationLease, GatewayError> {
        let state = self.lock_state(request_id)?;
        if state.phase != GatewayLifecycle::Running {
            return Err(gateway_closed_error(request_id, state.phase));
        }
        let (guard, lease) = self
            .operations
            .reserve_with_capacity(&request_id.0, progress_capacity)
            .map_err(|error| operation_registry_error(request_id, error))?;
        lease
            .queue()
            .map_err(|error| operation_registry_error(request_id, error))?;
        lease
            .start()
            .map_err(|error| operation_registry_error(request_id, error))?;
        guard.disarm();
        Ok(lease)
    }

    fn release_request(
        &self,
        lease: &operation_lifecycle::OperationLease,
        terminal: operation_lifecycle::TerminalClass,
    ) -> Result<(), GatewayError> {
        let request_id = RequestId(lease.identity().operation_id);
        let result = lease
            .terminal_and_release(terminal)
            .map_err(|error| operation_registry_error(&request_id, error));
        let recorded = result
            .as_ref()
            .err()
            .map(|error| self.record_lifecycle_error(error.clone()))
            .transpose();
        self.changed.notify_waiters();
        recorded.and(result)
    }

    fn terminal_request(
        &self,
        lease: &operation_lifecycle::OperationLease,
        terminal: operation_lifecycle::TerminalClass,
    ) -> Result<(), GatewayError> {
        let request_id = RequestId(lease.identity().operation_id);
        let result = lease
            .terminal(terminal)
            .map_err(|error| operation_registry_error(&request_id, error));
        let recorded = result
            .as_ref()
            .err()
            .map(|error| self.record_lifecycle_error(error.clone()))
            .transpose();
        self.changed.notify_waiters();
        recorded.and(result)
    }

    fn release_terminalized_request(
        &self,
        lease: &operation_lifecycle::OperationLease,
    ) -> Result<(), GatewayError> {
        let request_id = RequestId(lease.identity().operation_id);
        let result = lease
            .release()
            .map_err(|error| operation_registry_error(&request_id, error));
        let recorded = result
            .as_ref()
            .err()
            .map(|error| self.record_lifecycle_error(error.clone()))
            .transpose();
        self.changed.notify_waiters();
        recorded.and(result)
    }

    fn begin_shutdown(&self) -> Result<ShutdownDisposition, GatewayError> {
        let request_id = RequestId::new();
        let (mut state, state_error) = self.lock_state_for_shutdown(&request_id);
        if let Some(error) = state_error {
            state.shutdown_error.get_or_insert(error);
        }
        match state.phase {
            GatewayLifecycle::Running => {
                state.phase = GatewayLifecycle::Quiescing;
                let (active, registry_error) = self.operations.request_cancel_all_for_shutdown();
                if let Some(error) = registry_error {
                    state
                        .shutdown_error
                        .get_or_insert_with(|| operation_registry_error(&request_id, error));
                }
                let active = active.into_iter().map(RequestId).collect();
                self.changed.notify_waiters();
                Ok(ShutdownDisposition::Lead(active))
            }
            GatewayLifecycle::Quiescing => Ok(ShutdownDisposition::Wait),
            GatewayLifecycle::Closed => {
                Ok(ShutdownDisposition::Complete(Self::shutdown_report(&state)))
            }
        }
    }

    fn snapshot(&self) -> (GatewayLifecycle, usize, Option<GatewayError>) {
        let request_id = RequestId::new();
        let (active_requests, registry_poisoned) = self.operations.diagnostic_active_count();
        match self.state.lock() {
            Ok(state) => {
                let shutdown_error = state.shutdown_error.clone().or_else(|| {
                    registry_poisoned.then(|| {
                        operation_registry_error(
                            &request_id,
                            operation_lifecycle::RegistryError::Poisoned,
                        )
                    })
                });
                (state.phase, active_requests, shutdown_error)
            }
            Err(poisoned) => (
                poisoned.get_ref().phase,
                active_requests,
                Some(lifecycle_state_poisoned(&request_id)),
            ),
        }
    }

    async fn wait_until_drained(&self) -> Result<(), GatewayError> {
        loop {
            let mut changed = Box::pin(self.changed.notified());
            changed.as_mut().enable();
            let request_id = RequestId::new();
            let lifecycle_error = {
                let (state, state_error) = self.lock_state_for_shutdown(&request_id);
                state_error.or_else(|| state.shutdown_error.clone())
            };
            if let Some(error) = lifecycle_error {
                return Err(error);
            }
            let drained = self
                .operations
                .active_count()
                .map_err(|error| operation_registry_error(&request_id, error))?
                == 0;
            if drained {
                return Ok(());
            }
            changed.await;
        }
    }

    async fn wait_until_closed_report(&self) -> Result<GatewayShutdownReport, GatewayError> {
        loop {
            let mut changed = Box::pin(self.changed.notified());
            changed.as_mut().enable();
            let report = {
                let request_id = RequestId::new();
                let (state, state_error) = self.lock_state_for_shutdown(&request_id);
                (state.phase == GatewayLifecycle::Closed).then(|| {
                    let mut report = Self::shutdown_report(&state);
                    if let Some(error) = state_error {
                        report.result = Err(error);
                    }
                    report
                })
            };
            if let Some(report) = report {
                return Ok(report);
            }
            changed.await;
        }
    }

    fn finish_shutdown(&self, report: &GatewayShutdownReport) -> Result<(), GatewayError> {
        let request_id = RequestId::new();
        let (mut state, state_error) = self.lock_state_for_shutdown(&request_id);
        state.phase = GatewayLifecycle::Closed;
        if let Some(error) = &state_error {
            state.shutdown_error.get_or_insert_with(|| error.clone());
        } else if state.shutdown_error.is_none() {
            state.shutdown_error = report.result.as_ref().err().cloned();
        }
        state.shutdown_expected_worker_ids = report.expected_worker_ids.clone();
        state.shutdown_joined_worker_ids = report.joined_worker_ids.clone();
        state.shutdown_retained_tasks = report.retained_tasks;
        self.changed.notify_waiters();
        state_error.map_or(Ok(()), Err)
    }

    fn shutdown_report(state: &LifecycleState) -> GatewayShutdownReport {
        GatewayShutdownReport {
            result: state.shutdown_error.clone().map_or(Ok(()), Err),
            expected_worker_ids: state.shutdown_expected_worker_ids.clone(),
            joined_worker_ids: state.shutdown_joined_worker_ids.clone(),
            retained_tasks: state.shutdown_retained_tasks,
        }
    }

    fn record_lifecycle_error(&self, error: GatewayError) -> Result<(), GatewayError> {
        let mut state = self.lock_state(&error.request_id)?;
        state.shutdown_error.get_or_insert(error);
        self.changed.notify_waiters();
        Ok(())
    }

    fn lock_state(
        &self,
        request_id: &RequestId,
    ) -> Result<std::sync::MutexGuard<'_, LifecycleState>, GatewayError> {
        self.state
            .lock()
            .map_err(|_| lifecycle_state_poisoned(request_id))
    }

    fn lock_state_for_shutdown(
        &self,
        request_id: &RequestId,
    ) -> (
        std::sync::MutexGuard<'_, LifecycleState>,
        Option<GatewayError>,
    ) {
        match self.state.lock() {
            Ok(state) => (state, None),
            Err(poisoned) => (
                poisoned.into_inner(),
                Some(lifecycle_state_poisoned(request_id)),
            ),
        }
    }
}

pub struct Gateway {
    defaults: GatewayDefaults,
    state: RwLock<GatewayState>,
    lifecycle: Arc<LifecycleControl>,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Gateway")
            .field("defaults", &self.defaults)
            .finish_non_exhaustive()
    }
}

impl Gateway {
    #[must_use]
    pub fn new(defaults: GatewayDefaults) -> Self {
        Self {
            defaults,
            state: RwLock::new(GatewayState::default()),
            lifecycle: Arc::new(LifecycleControl::default()),
        }
    }

    pub fn register_backend(&self, backend: Arc<dyn GatewayBackend>) -> Result<(), GatewayError> {
        let descriptor = backend.descriptor();
        if !valid_backend_id(&descriptor.id) {
            return Err(GatewayError::invalid_request(
                &RequestId::new(),
                "backend_id_invalid",
                "backend IDs must use lowercase ASCII letters, digits, '.', '-' or '_'",
            ));
        }
        if descriptor
            .models
            .iter()
            .any(|model| model.backend_id != descriptor.id)
        {
            return Err(GatewayError::invalid_request(
                &RequestId::new(),
                "backend_model_owner_mismatch",
                "every model descriptor must name its owning backend",
            ));
        }
        self.lifecycle.while_running(&RequestId::new(), || {
            let mut state = self.state.write().map_err(|_| {
                GatewayError::unavailable(
                    &RequestId::new(),
                    "gateway_state_poisoned",
                    "gateway state is unavailable",
                )
            })?;
            if state.backends.contains_key(&descriptor.id) {
                return Err(GatewayError::invalid_request(
                    &RequestId::new(),
                    "backend_duplicate",
                    "a backend with that ID is already registered",
                ));
            }
            let admission = Arc::new(Semaphore::new(DEFAULT_BACKEND_CONCURRENCY));
            state
                .backend_admission
                .insert(descriptor.id.clone(), Arc::clone(&admission));
            state.all_admission.push(admission);
            state
                .backend_circuits
                .insert(descriptor.id.clone(), BackendCircuit::default());
            state.backends.insert(descriptor.id, backend);
            Ok(())
        })
    }

    /// Replaces the admission semaphore used by future requests for one
    /// backend. Existing tickets retain their original permits until their
    /// streams or authoritative results are dropped.
    pub fn set_backend_concurrency(
        &self,
        backend_id: &str,
        limit: usize,
    ) -> Result<(), GatewayError> {
        if limit == 0 {
            return Err(GatewayError::invalid_request(
                &RequestId::new(),
                "backend_concurrency_invalid",
                "backend concurrency must be greater than zero",
            ));
        }
        self.lifecycle.while_running(&RequestId::new(), || {
            let mut state = self.state.write().map_err(|_| {
                GatewayError::unavailable(
                    &RequestId::new(),
                    "gateway_state_poisoned",
                    "gateway state is unavailable",
                )
            })?;
            if !state.backends.contains_key(backend_id) {
                return Err(GatewayError::invalid_request(
                    &RequestId::new(),
                    "backend_not_registered",
                    "the backend must be registered before configuring admission",
                ));
            }
            let admission = Arc::new(Semaphore::new(limit));
            state
                .backend_admission
                .insert(backend_id.to_string(), Arc::clone(&admission));
            state.all_admission.push(admission);
            Ok(())
        })
    }

    pub async fn execute(&self, request: GatewayRequest) -> Result<GatewayTicket, GatewayError> {
        let started_at = Instant::now();
        request.validate()?;
        let request_id = request.request_id.clone();
        self.lifecycle.ensure_running(&request_id)?;
        let deadline = request.deadline.clone();
        let event_capacity = request
            .stream
            .event_capacity
            .unwrap_or(fte_types::DEFAULT_EVENT_CAPACITY);
        let candidates = self.resolve_candidates(&request)?;
        let fallback_allowed = request_fallback_allowed(&request);
        let mut last_error = None;

        for (attempt, (backend, route, admission)) in candidates.into_iter().enumerate() {
            if attempt > 0 && !fallback_allowed {
                break;
            }
            let lease = match self
                .admit(&request_id, &request, admission, started_at.elapsed())
                .await
            {
                Ok(lease) => lease,
                Err(error) if retryable_setup_failure(fallback_allowed, &error) => {
                    last_error = Some(error);
                    continue;
                }
                Err(error) => return Err(error),
            };
            let startup_ms = match route.location {
                BackendLocation::LocalEmbedded => deadline.model_load_ms,
                BackendLocation::Hosted => deadline.connect_ms,
            };
            let backend_id = route.backend_id.clone();
            let (startup_limit, total_is_limit) = match stage_deadline(
                startup_ms,
                deadline.total_ms,
                started_at.elapsed(),
                &request_id,
            ) {
                Ok(limit) => limit,
                Err(error) => {
                    lease.finish_error(&error)?;
                    return Err(error);
                }
            };
            let execution = backend.execute(BackendRequest {
                request: request.clone(),
                route,
            });
            let result = if let Some(startup_limit) = startup_limit {
                match tokio::time::timeout(startup_limit, execution).await {
                    Ok(result) => result,
                    Err(_) if total_is_limit => Err(total_timeout(&request_id)),
                    Err(_) => Err(startup_timeout(&request_id, &backend_id)),
                }
            } else {
                execution.await
            };
            match result {
                Ok(ticket) => {
                    if let Err(error) = self.record_backend_success(&backend_id) {
                        lease.finish_error(&error)?;
                        return Err(error);
                    }
                    return Ok(ticket.with_admission_lease_and_deadlines(
                        Box::new(lease),
                        deadline,
                        started_at.elapsed(),
                        event_capacity,
                    ));
                }
                Err(error) => {
                    let retry = retryable_setup_failure(fallback_allowed, &error);
                    if error.retryable
                        && error.code != "request_total_deadline_exceeded"
                        && let Err(state_error) = self.record_backend_failure(&backend_id)
                    {
                        lease.finish_error(&state_error)?;
                        return Err(state_error);
                    }
                    lease.finish_error(&error)?;
                    if retry {
                        last_error = Some(error);
                    } else {
                        return Err(error);
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            GatewayError::unavailable(
                &request_id,
                "route_attempts_exhausted",
                "all eligible routes failed before producing output",
            )
        }))
    }

    pub async fn count_tokens(
        &self,
        request: GatewayRequest,
    ) -> Result<GatewayUsage, GatewayError> {
        let started_at = Instant::now();
        request.validate()?;
        self.lifecycle.ensure_running(&request.request_id)?;
        let (backend, route, admission) = self.resolve(&request)?;
        let lease = self
            .admit(
                &request.request_id,
                &request,
                admission,
                started_at.elapsed(),
            )
            .await?;
        let stage_ms = match route.location {
            BackendLocation::LocalEmbedded => request.deadline.model_load_ms,
            BackendLocation::Hosted => request.deadline.connect_ms,
        };
        let request_id = request.request_id.clone();
        let backend_id = route.backend_id.clone();
        let deadline = stage_deadline(
            stage_ms,
            request.deadline.total_ms,
            started_at.elapsed(),
            &request_id,
        );
        let (limit, total_is_limit) = match deadline {
            Ok(deadline) => deadline,
            Err(error) => return lease.finish_result(Err(error)),
        };
        let count = backend.count_tokens(BackendRequest { request, route });
        let result = if let Some(limit) = limit {
            match tokio::time::timeout(limit, count).await {
                Ok(result) => result,
                Err(_) => Err(if total_is_limit {
                    total_timeout(&request_id)
                } else {
                    startup_timeout(&request_id, &backend_id)
                }),
            }
        } else {
            count.await
        };
        lease.finish_result(result)
    }

    pub fn cancel(&self, request_id: &RequestId, target: CancelTarget) -> usize {
        self.state
            .read()
            .map(|state| {
                state
                    .backends
                    .values()
                    .map(|backend| backend.cancel(request_id, target))
                    .sum()
            })
            .unwrap_or_default()
    }

    pub async fn shutdown(&self) -> Result<(), GatewayError> {
        self.shutdown_report().await.result
    }

    async fn shutdown_report(&self) -> GatewayShutdownReport {
        match self.lifecycle.begin_shutdown() {
            Err(error) => return failed_shutdown_report(error),
            Ok(ShutdownDisposition::Complete(report)) => return report,
            Ok(ShutdownDisposition::Wait) => {
                return self
                    .lifecycle
                    .wait_until_closed_report()
                    .await
                    .unwrap_or_else(failed_shutdown_report);
            }
            Ok(ShutdownDisposition::Lead(active_request_ids)) => {
                let (backends, admissions, state_error) = match self.state.read() {
                    Ok(state) => (
                        state
                            .backends
                            .iter()
                            .map(|(backend_id, backend)| {
                                (
                                    format!("backend-shutdown:{backend_id}"),
                                    Arc::clone(backend),
                                )
                            })
                            .collect::<Vec<_>>(),
                        state.all_admission.clone(),
                        None,
                    ),
                    Err(poisoned) => {
                        // Poisoning does not make the resources behind the
                        // lock disappear. Recover the guard so shutdown still
                        // closes admission, cancels work, and joins every
                        // backend before reporting the failure.
                        let state = poisoned.into_inner();
                        (
                            state
                                .backends
                                .iter()
                                .map(|(backend_id, backend)| {
                                    (
                                        format!("backend-shutdown:{backend_id}"),
                                        Arc::clone(backend),
                                    )
                                })
                                .collect::<Vec<_>>(),
                            state.all_admission.clone(),
                            Some(GatewayError::unavailable(
                                &RequestId::new(),
                                "gateway_state_poisoned",
                                "gateway state was poisoned; resources were drained before closure",
                            )),
                        )
                    }
                };
                for admission in admissions {
                    admission.close();
                }
                for request_id in active_request_ids {
                    for (_, backend) in &backends {
                        let _ = backend.cancel(&request_id, CancelTarget::Request);
                    }
                }

                // The coordinator is intentionally detached: once quiescing
                // begins, dropping one shutdown caller must not strand the
                // gateway forever or leave backend tasks alive.
                let lifecycle = Arc::clone(&self.lifecycle);
                tokio::spawn(async move {
                    let expected_worker_ids = backends
                        .iter()
                        .map(|(worker_id, _)| worker_id.clone())
                        .collect::<Vec<_>>();
                    let mut tasks = JoinSet::new();
                    for (worker_id, backend) in backends {
                        tasks.spawn(async move { (worker_id, backend.shutdown().await) });
                    }
                    let mut first_error = state_error;
                    let mut joined_worker_ids = Vec::with_capacity(expected_worker_ids.len());
                    while let Some(result) = tasks.join_next().await {
                        match result {
                            Ok((worker_id, Ok(()))) => joined_worker_ids.push(worker_id),
                            Ok((worker_id, Err(error))) => {
                                joined_worker_ids.push(worker_id);
                                first_error.get_or_insert(error);
                            }
                            Err(error) => {
                                first_error.get_or_insert_with(|| GatewayError {
                                    code: "backend_shutdown_task_failed".to_string(),
                                    class: fte_types::ErrorClass::Internal,
                                    retryable: false,
                                    http_status: 500,
                                    request_id: RequestId::new(),
                                    provider: None,
                                    safe_detail: format!("a backend shutdown task failed: {error}"),
                                });
                            }
                        }
                    }
                    let retained_tasks = tasks.len();
                    if let Err(error) = lifecycle.wait_until_drained().await {
                        first_error.get_or_insert(error);
                    }
                    let result = first_error.map_or(Ok(()), Err);
                    let _ = lifecycle.finish_shutdown(&GatewayShutdownReport {
                        result,
                        expected_worker_ids,
                        joined_worker_ids,
                        retained_tasks,
                    });
                });
            }
        }
        self.lifecycle
            .wait_until_closed_report()
            .await
            .unwrap_or_else(failed_shutdown_report)
    }

    #[must_use]
    pub fn models(&self) -> Vec<ModelDescriptor> {
        self.state
            .read()
            .map(|state| {
                state
                    .backends
                    .values()
                    .flat_map(|backend| backend.descriptor().models)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns the registered provider/model inventory and current readiness
    /// without exposing backend execution authority.
    #[must_use]
    pub fn backend_snapshots(&self) -> Vec<fte_types::BackendSnapshot> {
        self.state
            .read()
            .map(|state| {
                state
                    .backends
                    .values()
                    .map(|backend| fte_types::BackendSnapshot {
                        descriptor: backend.descriptor(),
                        readiness: backend.readiness(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn status(&self) -> GatewayStatus {
        let (lifecycle, active_requests, shutdown_error) = self.lifecycle.snapshot();
        match self.state.read() {
            Ok(state) => GatewayStatus {
                backend_count: state.backends.len(),
                ready_backend_count: state
                    .backends
                    .values()
                    .filter(|backend| backend.readiness().is_ready())
                    .count(),
                active_requests,
                lifecycle,
                shutdown_error,
                loopback: None,
            },
            Err(_) => GatewayStatus {
                backend_count: 0,
                ready_backend_count: 0,
                active_requests,
                lifecycle,
                shutdown_error: shutdown_error.or_else(|| {
                    Some(GatewayError::unavailable(
                        &RequestId::new(),
                        "gateway_state_poisoned",
                        "gateway state is unavailable",
                    ))
                }),
                loopback: None,
            },
        }
    }

    pub fn record_response_affinity(
        &self,
        response_id: &str,
        route: &ResolvedRoute,
    ) -> Result<(), GatewayError> {
        self.state
            .write()
            .map_err(|_| {
                GatewayError::unavailable(
                    &RequestId::new(),
                    "gateway_state_poisoned",
                    "gateway state is unavailable",
                )
            })?
            .response_affinity
            .insert(
                response_id.to_string(),
                (route.backend_id.clone(), route.model_id.clone()),
            );
        Ok(())
    }

    async fn admit(
        &self,
        request_id: &RequestId,
        request: &GatewayRequest,
        admission: Arc<Semaphore>,
        elapsed: Duration,
    ) -> Result<AdmissionLease, GatewayError> {
        let queue_ms = request.deadline.queue_ms.unwrap_or(DEFAULT_QUEUE_MS);
        let total_remaining = request
            .deadline
            .total_ms
            .map(|value| Duration::from_millis(value).saturating_sub(elapsed));
        if total_remaining.is_some_and(|remaining| remaining.is_zero()) {
            return Err(total_timeout(request_id));
        }
        let permit = if queue_ms == 0 {
            Arc::clone(&admission).try_acquire_owned().map_err(|_| {
                queue_timeout(
                    request_id,
                    "the selected backend has no immediately available slot",
                )
            })?
        } else {
            let queue_duration = Duration::from_millis(queue_ms);
            let (wait, total_is_limit) = total_remaining
                .map(|total| (queue_duration.min(total), total <= queue_duration))
                .unwrap_or((queue_duration, false));
            tokio::time::timeout(wait, Arc::clone(&admission).acquire_owned())
                .await
                .map_err(|_| {
                    if total_is_limit {
                        total_timeout(request_id)
                    } else {
                        queue_timeout(
                            request_id,
                            "the request exceeded its queue deadline before backend admission",
                        )
                    }
                })?
                .map_err(|_| {
                    GatewayError::unavailable(
                        request_id,
                        "backend_admission_closed",
                        "the selected backend stopped accepting requests",
                    )
                })?
        };
        let progress_capacity = request
            .stream
            .event_capacity
            .unwrap_or(fte_types::DEFAULT_EVENT_CAPACITY);
        let operation = self
            .lifecycle
            .register_request(request_id, progress_capacity)?;
        Ok(AdmissionLease {
            _permit: permit,
            operation: Mutex::new(Some(operation)),
            lifecycle: Arc::clone(&self.lifecycle),
        })
    }

    fn record_backend_success(&self, backend_id: &str) -> Result<(), GatewayError> {
        let mut state = self.state.write().map_err(|_| {
            GatewayError::unavailable(
                &RequestId::new(),
                "gateway_state_poisoned",
                "gateway state is unavailable",
            )
        })?;
        let circuit = state.backend_circuits.get_mut(backend_id).ok_or_else(|| {
            GatewayError::unavailable(
                &RequestId::new(),
                "backend_circuit_missing",
                "the selected backend has no circuit breaker",
            )
        })?;
        circuit.record_success();
        Ok(())
    }

    fn record_backend_failure(&self, backend_id: &str) -> Result<(), GatewayError> {
        let mut state = self.state.write().map_err(|_| {
            GatewayError::unavailable(
                &RequestId::new(),
                "gateway_state_poisoned",
                "gateway state is unavailable",
            )
        })?;
        let circuit = state.backend_circuits.get_mut(backend_id).ok_or_else(|| {
            GatewayError::unavailable(
                &RequestId::new(),
                "backend_circuit_missing",
                "the selected backend has no circuit breaker",
            )
        })?;
        circuit.record_failure(Instant::now());
        Ok(())
    }

    fn resolve(&self, request: &GatewayRequest) -> Result<RouteResolution, GatewayError> {
        self.resolve_candidates(request)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &request.request_id,
                    "route_unavailable",
                    "no configured and ready route satisfies the request",
                )
            })
    }

    fn resolve_candidates(
        &self,
        request: &GatewayRequest,
    ) -> Result<Vec<RouteResolution>, GatewayError> {
        let state = self.state.read().map_err(|_| {
            GatewayError::unavailable(
                &request.request_id,
                "gateway_state_poisoned",
                "gateway state is unavailable",
            )
        })?;
        let pinned = request
            .storage
            .previous_response_id
            .as_ref()
            .map(|id| {
                state.response_affinity.get(id).cloned().ok_or_else(|| {
                    GatewayError::invalid_request(
                        &request.request_id,
                        "previous_response_affinity_missing",
                        "the previous response is unavailable or has no route affinity",
                    )
                })
            })
            .transpose()?;

        let mut eligible = Vec::new();
        let mut rejection_codes = Vec::new();
        let now = Instant::now();
        for backend in state.backends.values() {
            let descriptor = backend.descriptor();
            let circuit_available = state
                .backend_circuits
                .get(&descriptor.id)
                .is_some_and(|circuit| circuit.is_available(now));
            for model in &descriptor.models {
                match candidate_allowed(request, &descriptor, model, pinned.as_ref()) {
                    Ok(()) if !backend.readiness().is_ready() => {
                        rejection_codes.push("backend_not_ready");
                    }
                    Ok(()) if !circuit_available => {
                        rejection_codes.push("backend_circuit_open");
                    }
                    Ok(()) => {
                        eligible.push((Arc::clone(backend), descriptor.clone(), model.clone()));
                    }
                    Err(code) => rejection_codes.push(code),
                }
            }
        }
        if eligible.is_empty() {
            let detail = if rejection_codes.contains(&"privacy_local_only") {
                "no local route satisfies the request; hosted fallback is forbidden by policy"
            } else if rejection_codes.contains(&"capability_unsupported") {
                "no ready route supports every requested capability"
            } else if rejection_codes.contains(&"backend_circuit_open") {
                "all eligible routes are temporarily unavailable after repeated setup failures"
            } else {
                "no configured and ready route satisfies the request"
            };
            return Err(GatewayError {
                code: "route_unavailable".to_string(),
                class: if rejection_codes.contains(&"privacy_local_only") {
                    fte_types::ErrorClass::Privacy
                } else {
                    fte_types::ErrorClass::Unavailable
                },
                retryable: true,
                http_status: 503,
                request_id: request.request_id.clone(),
                provider: None,
                safe_detail: detail.to_string(),
            });
        }
        eligible.sort_by(|left, right| {
            route_score(request, &right.2)
                .partial_cmp(&route_score(request, &left.2))
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.1.id.cmp(&right.1.id))
                .then_with(|| left.2.id.cmp(&right.2.id))
        });
        eligible
            .into_iter()
            .map(|(backend, descriptor, model)| {
                let route = ResolvedRoute {
                    backend_id: descriptor.id,
                    model_id: model.id,
                    display_name: model.display_name,
                    location: model.location,
                    catalog_version: self.defaults.catalog_version.clone(),
                };
                let admission = state
                    .backend_admission
                    .get(&route.backend_id)
                    .cloned()
                    .ok_or_else(|| {
                        GatewayError::unavailable(
                            &request.request_id,
                            "backend_admission_missing",
                            "the selected backend has no admission controller",
                        )
                    })?;
                Ok((backend, route, admission))
            })
            .collect()
    }
}

struct AdmissionLease {
    _permit: OwnedSemaphorePermit,
    operation: Mutex<Option<operation_lifecycle::OperationLease>>,
    lifecycle: Arc<LifecycleControl>,
}

impl AdmissionLease {
    fn terminal_for_result<T>(
        result: &Result<T, GatewayError>,
    ) -> operation_lifecycle::TerminalClass {
        match result {
            Ok(_) => operation_lifecycle::TerminalClass::Completed,
            Err(error) if error.class == ErrorClass::Cancelled => {
                operation_lifecycle::TerminalClass::Cancelled
            }
            Err(_) => operation_lifecycle::TerminalClass::Failed,
        }
    }

    fn release_with_terminal(
        &self,
        terminal: operation_lifecycle::TerminalClass,
    ) -> Result<(), GatewayError> {
        let mut operation = self
            .operation
            .lock()
            .map_err(|_| lifecycle_state_poisoned(&RequestId::new()))?;
        let Some(operation) = operation.take() else {
            return Ok(());
        };
        self.lifecycle.release_request(&operation, terminal)
    }

    fn finish_result<T>(self, result: Result<T, GatewayError>) -> Result<T, GatewayError> {
        let terminal = Self::terminal_for_result(&result);
        self.release_with_terminal(terminal)?;
        result
    }

    fn finish_error(self, error: &GatewayError) -> Result<(), GatewayError> {
        let terminal = if error.class == ErrorClass::Cancelled {
            operation_lifecycle::TerminalClass::Cancelled
        } else {
            operation_lifecycle::TerminalClass::Failed
        };
        self.release_with_terminal(terminal)
    }
}

impl Drop for AdmissionLease {
    fn drop(&mut self) {
        let _ = self.release_with_terminal(operation_lifecycle::TerminalClass::Failed);
    }
}

impl TicketLifecycleLease for AdmissionLease {
    fn finish(
        &self,
        result: &Result<fte_types::GatewayResponse, GatewayError>,
    ) -> Result<(), GatewayError> {
        let terminal = match result {
            Ok(response) => match response.status {
                TerminalStatus::Completed => operation_lifecycle::TerminalClass::Completed,
                TerminalStatus::Cancelled => operation_lifecycle::TerminalClass::Cancelled,
                TerminalStatus::Failed => operation_lifecycle::TerminalClass::Failed,
            },
            Err(error) if error.class == ErrorClass::Cancelled => {
                operation_lifecycle::TerminalClass::Cancelled
            }
            Err(_) => operation_lifecycle::TerminalClass::Failed,
        };
        self.release_with_terminal(terminal)
    }

    fn terminal(
        &self,
        result: &Result<fte_types::GatewayResponse, GatewayError>,
    ) -> Result<(), GatewayError> {
        let terminal = match result {
            Ok(response) => match response.status {
                TerminalStatus::Completed => operation_lifecycle::TerminalClass::Completed,
                TerminalStatus::Cancelled => operation_lifecycle::TerminalClass::Cancelled,
                TerminalStatus::Failed => operation_lifecycle::TerminalClass::Failed,
            },
            Err(error) if error.class == ErrorClass::Cancelled => {
                operation_lifecycle::TerminalClass::Cancelled
            }
            Err(_) => operation_lifecycle::TerminalClass::Failed,
        };
        let operation = self
            .operation
            .lock()
            .map_err(|_| lifecycle_state_poisoned(&RequestId::new()))?;
        let Some(operation) = operation.as_ref() else {
            return Ok(());
        };
        self.lifecycle.terminal_request(operation, terminal)
    }

    fn release(&self) -> Result<(), GatewayError> {
        let mut operation = self
            .operation
            .lock()
            .map_err(|_| lifecycle_state_poisoned(&RequestId::new()))?;
        let Some(operation) = operation.take() else {
            return Ok(());
        };
        self.lifecycle.release_terminalized_request(&operation)
    }
}

fn failed_shutdown_report(error: GatewayError) -> GatewayShutdownReport {
    GatewayShutdownReport {
        result: Err(error),
        expected_worker_ids: Vec::new(),
        joined_worker_ids: Vec::new(),
        retained_tasks: 0,
    }
}

fn lifecycle_state_poisoned(request_id: &RequestId) -> GatewayError {
    GatewayError::unavailable(
        request_id,
        "gateway_lifecycle_state_poisoned",
        "gateway lifecycle state is unavailable",
    )
}

fn operation_registry_error(
    request_id: &RequestId,
    error: operation_lifecycle::RegistryError,
) -> GatewayError {
    match error {
        operation_lifecycle::RegistryError::Duplicate => GatewayError {
            code: "request_already_active".to_string(),
            class: ErrorClass::Unavailable,
            retryable: false,
            http_status: 409,
            request_id: request_id.clone(),
            provider: None,
            safe_detail: "a request with this identifier is already active".to_string(),
        },
        operation_lifecycle::RegistryError::Poisoned => GatewayError::unavailable(
            request_id,
            "gateway_operation_registry_poisoned",
            "gateway operation lifecycle state is unavailable",
        ),
        operation_lifecycle::RegistryError::Exhausted => GatewayError::unavailable(
            request_id,
            "gateway_operation_identity_exhausted",
            "gateway operation identity space is exhausted",
        ),
        operation_lifecycle::RegistryError::Stale
        | operation_lifecycle::RegistryError::InvalidTransition => GatewayError::unavailable(
            request_id,
            "gateway_operation_lifecycle_failed",
            "gateway operation lifecycle rejected the transition",
        ),
    }
}

fn gateway_closed_error(request_id: &RequestId, phase: GatewayLifecycle) -> GatewayError {
    let (code, detail) = match phase {
        GatewayLifecycle::Running => (
            "gateway_admission_race",
            "gateway admission changed while the request was being registered",
        ),
        GatewayLifecycle::Quiescing => (
            "gateway_quiescing",
            "the gateway is draining and no longer accepts requests",
        ),
        GatewayLifecycle::Closed => (
            "gateway_closed",
            "the gateway is closed and no longer accepts requests",
        ),
    };
    GatewayError::unavailable(request_id, code, detail)
}

fn queue_timeout(request_id: &RequestId, detail: &str) -> GatewayError {
    GatewayError {
        code: "queue_deadline_exceeded".to_string(),
        class: fte_types::ErrorClass::Timeout,
        retryable: true,
        http_status: 503,
        request_id: request_id.clone(),
        provider: None,
        safe_detail: detail.to_string(),
    }
}

fn startup_timeout(request_id: &RequestId, backend_id: &str) -> GatewayError {
    GatewayError {
        code: "backend_startup_deadline_exceeded".to_string(),
        class: fte_types::ErrorClass::Timeout,
        retryable: true,
        http_status: 504,
        request_id: request_id.clone(),
        provider: Some(backend_id.to_string()),
        safe_detail: "the selected backend exceeded its model-load or provider-connect deadline"
            .to_string(),
    }
}

fn total_timeout(request_id: &RequestId) -> GatewayError {
    GatewayError {
        code: "request_total_deadline_exceeded".to_string(),
        class: fte_types::ErrorClass::Timeout,
        retryable: true,
        http_status: 504,
        request_id: request_id.clone(),
        provider: None,
        safe_detail: "the request exceeded its total deadline".to_string(),
    }
}

fn stage_deadline(
    stage_ms: Option<u64>,
    total_ms: Option<u64>,
    elapsed: Duration,
    request_id: &RequestId,
) -> Result<(Option<Duration>, bool), GatewayError> {
    let stage = stage_ms.map(Duration::from_millis);
    let total_remaining =
        total_ms.map(|value| Duration::from_millis(value).saturating_sub(elapsed));
    if total_remaining.is_some_and(|remaining| remaining.is_zero()) {
        return Err(total_timeout(request_id));
    }
    Ok(match (stage, total_remaining) {
        (Some(stage), Some(total)) if total <= stage => (Some(total), true),
        (Some(stage), Some(_)) => (Some(stage), false),
        (None, Some(total)) => (Some(total), true),
        (Some(stage), None) => (Some(stage), false),
        (None, None) => (None, false),
    })
}

fn candidate_allowed(
    request: &GatewayRequest,
    backend: &BackendDescriptor,
    model: &ModelDescriptor,
    pinned: Option<&(String, String)>,
) -> Result<(), &'static str> {
    if let Some((backend_id, model_id)) = pinned
        && (&backend.id, &model.id) != (backend_id, model_id)
    {
        return Err("response_affinity_mismatch");
    }
    match &request.model {
        ModelSelector::ExactRoute {
            backend_id,
            model_id,
        } if backend_id != &backend.id || model_id != &model.id => {
            return Err("exact_route_mismatch");
        }
        ModelSelector::ExactModel { model_id }
            if model_id != &model.id && !model.aliases.contains(model_id) =>
        {
            return Err("exact_model_mismatch");
        }
        ModelSelector::Profile { name }
            if !matches!(
                name.as_str(),
                "local-only" | "hosted-only" | "prefer-local" | "auto"
            ) =>
        {
            return Err("profile_unknown");
        }
        _ => {}
    }
    if request.routing.privacy == PrivacyPolicy::LocalOnly
        && model.location != BackendLocation::LocalEmbedded
    {
        return Err("privacy_local_only");
    }
    if request.routing.privacy == PrivacyPolicy::HostedOnly
        && model.location != BackendLocation::Hosted
    {
        return Err("privacy_hosted_only");
    }
    if request.routing.profile == RouteProfile::LocalOnly
        && model.location != BackendLocation::LocalEmbedded
    {
        return Err("privacy_local_only");
    }
    if request.routing.profile == RouteProfile::HostedOnly
        && model.location != BackendLocation::Hosted
    {
        return Err("privacy_hosted_only");
    }
    if !model
        .capabilities
        .prompt_forms
        .contains(&request.input.prompt_form())
        || (request.stream.enabled && !model.capabilities.streaming)
        || (!request.tools.is_empty() && !model.capabilities.tools)
        || (!matches!(request.response_format, ResponseFormat::Text)
            && !model.capabilities.structured_output)
    {
        return Err("capability_unsupported");
    }
    if request.cache.requirement == fte_types::CacheRequirement::Required
        && request.cache.mode == fte_types::CacheMode::ProviderNative
        && !model.capabilities.provider_cache
    {
        return Err("capability_unsupported");
    }
    Ok(())
}

fn request_fallback_allowed(request: &GatewayRequest) -> bool {
    request.routing.retry_before_output
        && matches!(request.model, ModelSelector::Profile { .. })
        && request.storage.previous_response_id.is_none()
        && !request
            .tools
            .iter()
            .any(|tool| tool.owner == fte_types::ToolOwner::Gateway)
}

fn retryable_setup_failure(fallback_allowed: bool, error: &GatewayError) -> bool {
    fallback_allowed && error.retryable && error.code != "request_total_deadline_exceeded"
}

fn route_score(request: &GatewayRequest, model: &ModelDescriptor) -> f64 {
    let observations = &model.observed;
    let evaluation = observations.quality.unwrap_or(NEUTRAL_ROUTE_SIGNAL);
    let latency = observations
        .latency_ms
        .map(|value| 1.0 / (1.0 + value as f64 / 1_000.0))
        .unwrap_or(NEUTRAL_ROUTE_SIGNAL);
    let headroom = observations.quota_headroom.unwrap_or(NEUTRAL_ROUTE_SIGNAL);
    let capability = capability_breadth(model);
    let local_preference = if request.routing.profile == RouteProfile::PreferLocal
        && model.location == BackendLocation::LocalEmbedded
    {
        100.0
    } else {
        0.0
    };
    local_preference
        + headroom * HEADROOM_WEIGHT
        + evaluation * EVALUATION_WEIGHT
        + capability * CAPABILITY_WEIGHT
        + latency * LATENCY_WEIGHT
}

fn capability_breadth(model: &ModelDescriptor) -> f64 {
    let capabilities = &model.capabilities;
    let prompt_forms = [
        fte_types::PromptForm::Chat,
        fte_types::PromptForm::Completion,
        fte_types::PromptForm::FillInMiddle,
    ]
    .iter()
    .filter(|form| capabilities.prompt_forms.contains(form))
    .count();
    let modalities = [
        fte_types::Modality::Text,
        fte_types::Modality::Image,
        fte_types::Modality::Audio,
        fte_types::Modality::Document,
    ]
    .iter()
    .filter(|modality| capabilities.modalities.contains(modality))
    .count();
    let feature_flags = [
        capabilities.tools,
        capabilities.structured_output,
        capabilities.reasoning,
        capabilities.streaming,
        capabilities.provider_cache,
    ]
    .into_iter()
    .filter(|enabled| *enabled)
    .count();
    const CAPABILITY_DIMENSIONS: usize = 3 + 4 + 5;
    (prompt_forms + modalities + feature_flags) as f64 / CAPABILITY_DIMENSIONS as f64
}

fn valid_backend_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '-' | '_')
        })
}

#[cfg(all(test, feature = "unstable-w1-contract-tests"))]
mod w1_contract_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use fte_types::{
        BackendReadiness, ContentBlock, GatewayEvent, GenerationInput, InputItem, MessageRole,
        ModelCapabilities, PromptForm, RouteObservations, RoutingPolicy, SamplingOptions,
        StoragePolicy, StreamPolicy, TicketCancellation, ToolPolicy,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use tokio::sync::{mpsc, oneshot};

    struct NeverExecuteBackend(BackendDescriptor);

    #[async_trait]
    impl GatewayBackend for NeverExecuteBackend {
        fn descriptor(&self) -> BackendDescriptor {
            self.0.clone()
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            Err(GatewayError::unavailable(
                &request.request.request_id,
                "not_executed",
                "test route only",
            ))
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            0
        }
    }

    struct CountingFailureBackend {
        descriptor: BackendDescriptor,
        calls: Arc<AtomicUsize>,
    }

    enum LifecycleSabotage {
        Terminalize,
        Poison,
    }

    struct LifecycleSabotageBackend {
        descriptor: BackendDescriptor,
        registry: operation_lifecycle::OperationRegistry,
        sabotage: LifecycleSabotage,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl GatewayBackend for CountingFailureBackend {
        fn descriptor(&self) -> BackendDescriptor {
            self.descriptor.clone()
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            self.calls.fetch_add(1, AtomicOrdering::AcqRel);
            Err(GatewayError::unavailable(
                &request.request.request_id,
                "fixture_provider_unavailable",
                "fixture provider setup failed",
            ))
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            0
        }
    }

    #[async_trait]
    impl GatewayBackend for LifecycleSabotageBackend {
        fn descriptor(&self) -> BackendDescriptor {
            self.descriptor.clone()
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            self.calls.fetch_add(1, AtomicOrdering::AcqRel);
            match self.sabotage {
                LifecycleSabotage::Terminalize => self
                    .registry
                    .current_lease(&request.request.request_id.0)
                    .expect("registry remains available")
                    .expect("admitted operation exists")
                    .terminal(operation_lifecycle::TerminalClass::Failed)
                    .expect("sabotage terminal transition"),
                LifecycleSabotage::Poison => self.registry.poison_for_test(),
            }
            Err(GatewayError::unavailable(
                &request.request.request_id,
                "fixture_provider_unavailable",
                "fixture provider setup failed",
            ))
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            0
        }
    }

    fn descriptor(id: &str, location: BackendLocation) -> BackendDescriptor {
        BackendDescriptor {
            id: id.to_string(),
            display_name: id.to_string(),
            location,
            models: vec![ModelDescriptor {
                id: format!("{id}-model"),
                aliases: Vec::new(),
                display_name: id.to_string(),
                backend_id: id.to_string(),
                location,
                capabilities: ModelCapabilities {
                    prompt_forms: vec![PromptForm::Chat],
                    modalities: vec![],
                    tools: false,
                    structured_output: false,
                    reasoning: false,
                    streaming: true,
                    provider_cache: false,
                },
                context_tokens: Some(4096),
                max_output_tokens: Some(512),
                observed: RouteObservations::default(),
            }],
        }
    }

    fn request() -> GatewayRequest {
        GatewayRequest {
            request_id: RequestId::new(),
            client_id: "test".to_string(),
            model: ModelSelector::Profile {
                name: "local-only".to_string(),
            },
            input: GenerationInput::Chat {
                items: vec![InputItem::Message {
                    id: None,
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: "hello".to_string(),
                    }],
                }],
            },
            sampling: SamplingOptions::default(),
            response_format: ResponseFormat::Text,
            tools: vec![],
            tool_policy: ToolPolicy::default(),
            cache: fte_types::CachePolicy::default(),
            routing: RoutingPolicy::default(),
            storage: StoragePolicy::default(),
            deadline: fte_types::DeadlinePolicy::default(),
            stream: StreamPolicy::default(),
            provider_extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn local_only_never_admits_a_hosted_candidate() {
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(NeverExecuteBackend(descriptor(
                "hosted",
                BackendLocation::Hosted,
            ))))
            .expect("register hosted");
        let error = match gateway.resolve(&request()) {
            Ok(_) => panic!("route must fail"),
            Err(error) => error,
        };
        assert_eq!(error.class, fte_types::ErrorClass::Privacy);
    }

    #[test]
    fn exact_route_bypasses_automatic_scoring_but_not_privacy() {
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(NeverExecuteBackend(descriptor(
                "local",
                BackendLocation::LocalEmbedded,
            ))))
            .expect("register local");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "local".to_string(),
            model_id: "local-model".to_string(),
        };
        let (_, route, _) = gateway.resolve(&request).expect("exact route");
        assert_eq!(route.backend_id, "local");
    }

    #[test]
    fn public_model_aliases_are_resolved_inside_the_gateway() {
        let gateway = Gateway::new(GatewayDefaults::default());
        let mut first = descriptor("first", BackendLocation::LocalEmbedded);
        first.models[0].aliases = vec!["public/model".to_string()];
        let mut second = descriptor("second", BackendLocation::LocalEmbedded);
        second.models[0].aliases = vec!["public/model".to_string()];
        second.models[0].observed.quality = Some(1.0);
        first.models[0].observed.quality = Some(0.0);
        gateway
            .register_backend(Arc::new(NeverExecuteBackend(first)))
            .expect("register first alias");
        gateway
            .register_backend(Arc::new(NeverExecuteBackend(second)))
            .expect("register second alias");

        let mut request = request();
        request.model = ModelSelector::ExactModel {
            model_id: "public/model".to_string(),
        };
        let (_, route, _) = gateway.resolve(&request).expect("resolve public alias");

        assert_eq!(route.backend_id, "second");
        assert_eq!(route.model_id, "second-model");
    }

    #[test]
    fn automatic_routing_uses_fixed_documented_signals() {
        let gateway = Gateway::new(GatewayDefaults::default());
        let mut measured = descriptor("measured", BackendLocation::LocalEmbedded);
        measured.models[0].observed = RouteObservations {
            quality: Some(1.0),
            cost_per_million_input_tokens: Some(1_000.0),
            latency_ms: Some(0),
            quota_headroom: Some(1.0),
            cache_warmth: Some(0.0),
        };
        let mut irrelevant = descriptor("irrelevant", BackendLocation::LocalEmbedded);
        irrelevant.models[0].observed = RouteObservations {
            quality: Some(0.0),
            cost_per_million_input_tokens: Some(0.0),
            latency_ms: Some(u64::MAX),
            quota_headroom: Some(0.0),
            cache_warmth: Some(1.0),
        };
        gateway
            .register_backend(Arc::new(NeverExecuteBackend(measured)))
            .expect("register measured route");
        gateway
            .register_backend(Arc::new(NeverExecuteBackend(irrelevant)))
            .expect("register irrelevant route");

        let mut request = request();
        request.routing.weights.quality = 0.0;
        request.routing.weights.latency = 0.0;
        request.routing.weights.quota = 0.0;
        request.routing.weights.cost = 1_000.0;
        request.routing.weights.cache_warmth = 1_000.0;
        let (_, route, _) = gateway.resolve(&request).expect("resolve fixed policy");

        assert_eq!(route.backend_id, "measured");
    }

    struct CountCancellation {
        count: Arc<AtomicUsize>,
        completion:
            Mutex<Option<oneshot::Sender<Result<fte_types::GatewayResponse, GatewayError>>>>,
    }

    impl TicketCancellation for CountCancellation {
        fn cancel(&self, _target: CancelTarget) -> usize {
            self.count.fetch_add(1, AtomicOrdering::AcqRel);
            if let Ok(mut completion) = self.completion.lock()
                && let Some(completion) = completion.take()
            {
                let _ = completion.send(Err(GatewayError {
                    code: "fixture_cancelled".to_string(),
                    class: fte_types::ErrorClass::Cancelled,
                    retryable: false,
                    http_status: 499,
                    request_id: RequestId::new(),
                    provider: None,
                    safe_detail: "the fixture request was cancelled".to_string(),
                }));
            }
            1
        }
    }

    struct HangingBackend {
        cancelled: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl GatewayBackend for HangingBackend {
        fn descriptor(&self) -> BackendDescriptor {
            descriptor("hanging", BackendLocation::LocalEmbedded)
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            let (_event_tx, event_rx) = mpsc::channel(1);
            let (final_tx, final_rx) = oneshot::channel();
            Ok(GatewayTicket::new(
                request.request.request_id,
                event_rx,
                final_rx,
                Arc::new(CountCancellation {
                    count: Arc::clone(&self.cancelled),
                    completion: Mutex::new(Some(final_tx)),
                }),
                Arc::new(AtomicBool::new(false)),
            ))
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            0
        }

        async fn count_tokens(
            &self,
            _request: BackendRequest,
        ) -> Result<GatewayUsage, GatewayError> {
            std::future::pending().await
        }
    }

    struct NoopCancellation;

    impl TicketCancellation for NoopCancellation {
        fn cancel(&self, _target: CancelTarget) -> usize {
            0
        }
    }

    type FixtureFinalSender = oneshot::Sender<Result<fte_types::GatewayResponse, GatewayError>>;

    struct DrainingBackend {
        completion: Mutex<Option<FixtureFinalSender>>,
        cancellations: Arc<AtomicUsize>,
        shutdowns: Arc<AtomicUsize>,
    }

    struct DelayedFinalBackend {
        completion: Arc<Mutex<Option<FixtureFinalSender>>>,
        cancellations: Arc<AtomicUsize>,
    }

    struct EarlyCompletedEventBackend {
        completion: Arc<Mutex<Option<FixtureFinalSender>>>,
        cancellations: Arc<AtomicUsize>,
    }

    struct ProgressThenDelayedFinalBackend {
        completion: Arc<Mutex<Option<FixtureFinalSender>>>,
        cancellations: Arc<AtomicUsize>,
    }

    fn completed_response(request_id: RequestId, backend_id: &str) -> fte_types::GatewayResponse {
        fte_types::GatewayResponse {
            id: "fixture-response".to_string(),
            request_id,
            model: format!("{backend_id}-model"),
            route: fte_types::ResolvedRoute {
                backend_id: backend_id.to_string(),
                model_id: format!("{backend_id}-model"),
                display_name: backend_id.to_string(),
                location: BackendLocation::LocalEmbedded,
                catalog_version: "test".to_string(),
            },
            output: Vec::new(),
            usage: fte_types::GatewayUsage::default(),
            status: TerminalStatus::Completed,
            previous_response_id: None,
        }
    }

    #[async_trait]
    impl GatewayBackend for DelayedFinalBackend {
        fn descriptor(&self) -> BackendDescriptor {
            descriptor("delayed-final", BackendLocation::LocalEmbedded)
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            let (_event_tx, event_rx) = mpsc::channel(1);
            let (final_tx, final_rx) = oneshot::channel();
            *self.completion.lock().expect("completion state") = Some(final_tx);
            Ok(GatewayTicket::new(
                request.request.request_id,
                event_rx,
                final_rx,
                Arc::new(CountCancellation {
                    count: Arc::clone(&self.cancellations),
                    completion: Mutex::new(None),
                }),
                Arc::new(AtomicBool::new(false)),
            ))
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            self.cancellations.fetch_add(1, AtomicOrdering::AcqRel);
            1
        }
    }

    #[async_trait]
    impl GatewayBackend for EarlyCompletedEventBackend {
        fn descriptor(&self) -> BackendDescriptor {
            descriptor("early-terminal", BackendLocation::LocalEmbedded)
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            let (event_tx, event_rx) = mpsc::channel(1);
            let (final_tx, final_rx) = oneshot::channel();
            *self.completion.lock().expect("completion state") = Some(final_tx);
            event_tx
                .send(GatewayEvent::Completed {
                    request_id: request.request.request_id.clone(),
                    response: Box::new(completed_response(
                        request.request.request_id.clone(),
                        "early-terminal",
                    )),
                })
                .await
                .expect("publish premature completed event");
            Ok(GatewayTicket::new(
                request.request.request_id,
                event_rx,
                final_rx,
                Arc::new(CountCancellation {
                    count: Arc::clone(&self.cancellations),
                    completion: Mutex::new(None),
                }),
                Arc::new(AtomicBool::new(false)),
            ))
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            self.cancellations.fetch_add(1, AtomicOrdering::AcqRel);
            1
        }
    }

    #[async_trait]
    impl GatewayBackend for ProgressThenDelayedFinalBackend {
        fn descriptor(&self) -> BackendDescriptor {
            descriptor("progress-delayed", BackendLocation::LocalEmbedded)
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            let (event_tx, event_rx) = mpsc::channel(1);
            let (final_tx, final_rx) = oneshot::channel();
            *self.completion.lock().expect("completion state") = Some(final_tx);
            event_tx
                .send(GatewayEvent::TextDelta {
                    request_id: request.request.request_id.clone(),
                    output_index: 0,
                    content_index: 0,
                    delta: "progress".to_string(),
                })
                .await
                .expect("publish progress event");
            Ok(GatewayTicket::new(
                request.request.request_id,
                event_rx,
                final_rx,
                Arc::new(CountCancellation {
                    count: Arc::clone(&self.cancellations),
                    completion: Mutex::new(None),
                }),
                Arc::new(AtomicBool::new(false)),
            ))
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            self.cancellations.fetch_add(1, AtomicOrdering::AcqRel);
            1
        }
    }

    #[async_trait]
    impl GatewayBackend for DrainingBackend {
        fn descriptor(&self) -> BackendDescriptor {
            descriptor("draining", BackendLocation::LocalEmbedded)
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            let (_event_tx, event_rx) = mpsc::channel(1);
            let (final_tx, final_rx) = oneshot::channel();
            *self.completion.lock().expect("completion state") = Some(final_tx);
            Ok(GatewayTicket::new(
                request.request.request_id,
                event_rx,
                final_rx,
                Arc::new(NoopCancellation),
                Arc::new(AtomicBool::new(false)),
            ))
        }

        fn cancel(&self, request_id: &RequestId, _target: CancelTarget) -> usize {
            self.cancellations.fetch_add(1, AtomicOrdering::AcqRel);
            let Some(completion) = self.completion.lock().expect("completion state").take() else {
                return 0;
            };
            let _ = completion.send(Err(GatewayError {
                code: "fixture_cancelled".to_string(),
                class: fte_types::ErrorClass::Cancelled,
                retryable: false,
                http_status: 499,
                request_id: request_id.clone(),
                provider: None,
                safe_detail: "the fixture request was cancelled".to_string(),
            }));
            1
        }

        async fn shutdown(&self) -> Result<(), GatewayError> {
            self.shutdowns.fetch_add(1, AtomicOrdering::AcqRel);
            Ok(())
        }
    }

    struct BlockingShutdownBackend {
        entered: Arc<Semaphore>,
        release: Arc<Semaphore>,
        shutdowns: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl GatewayBackend for BlockingShutdownBackend {
        fn descriptor(&self) -> BackendDescriptor {
            descriptor("blocking", BackendLocation::LocalEmbedded)
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            Err(GatewayError::unavailable(
                &request.request.request_id,
                "fixture_not_executed",
                "the lifecycle fixture does not execute requests",
            ))
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            0
        }

        async fn shutdown(&self) -> Result<(), GatewayError> {
            self.shutdowns.fetch_add(1, AtomicOrdering::AcqRel);
            self.entered.add_permits(1);
            self.release
                .acquire()
                .await
                .expect("release lifecycle fixture")
                .forget();
            Ok(())
        }
    }

    #[tokio::test]
    async fn duplicate_active_public_request_id_is_rejected_without_a_second_backend_start() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(HangingBackend {
                cancelled: Arc::clone(&cancelled),
            }))
            .expect("register hanging backend");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "hanging".to_string(),
            model_id: "hanging-model".to_string(),
        };

        let first = gateway
            .execute(request.clone())
            .await
            .expect("first request owns its public identity");
        let duplicate = gateway
            .execute(request)
            .await
            .expect_err("a concurrent public request ID must be unique");

        assert_eq!(duplicate.code, "request_already_active");
        assert_eq!(duplicate.class, ErrorClass::Unavailable);
        assert_eq!(duplicate.http_status, 409);
        assert!(!duplicate.retryable);
        assert_eq!(gateway.status().active_requests, 1);

        drop(first);
        wait_for_no_active(&gateway).await;
        assert_eq!(cancelled.load(AtomicOrdering::Acquire), 1);
    }

    #[tokio::test]
    async fn poisoned_operation_registry_replaces_backend_success_and_shutdown_success() {
        let completion = Arc::new(Mutex::new(None));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(DelayedFinalBackend {
                completion: Arc::clone(&completion),
                cancellations,
            }))
            .expect("register delayed-final backend");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "delayed-final".to_string(),
            model_id: "delayed-final-model".to_string(),
        };
        let request_id = request.request_id.clone();
        let ticket = gateway.execute(request).await.expect("start request");

        gateway.lifecycle.operations.poison_for_test();
        completion
            .lock()
            .expect("completion state")
            .take()
            .expect("pending backend final")
            .send(Ok(completed_response(request_id, "delayed-final")))
            .expect("publish backend success");

        let error = ticket
            .final_response()
            .await
            .expect_err("lifecycle poison must replace backend success");
        assert_eq!(error.code, "gateway_operation_registry_poisoned");

        let error = tokio::time::timeout(Duration::from_secs(1), gateway.shutdown())
            .await
            .expect("poisoned shutdown must terminate")
            .expect_err("poisoned lifecycle cannot report shutdown success");
        assert_eq!(error.code, "gateway_operation_registry_poisoned");
        let status = gateway.status();
        assert_eq!(status.lifecycle, GatewayLifecycle::Closed);
        assert_eq!(status.active_requests, 1);
        assert_eq!(
            status
                .shutdown_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("gateway_operation_registry_poisoned")
        );
    }

    #[tokio::test]
    async fn failed_operation_release_replaces_backend_success() {
        let gateway = Gateway::new(GatewayDefaults::default());
        let request_id = RequestId::new();
        let operation = gateway
            .lifecycle
            .register_request(&request_id, 1)
            .expect("register operation");
        let retained_operation = operation.clone();
        let attempt = operation.start_attempt().expect("start active attempt");
        let permit = Arc::new(Semaphore::new(1))
            .acquire_owned()
            .await
            .expect("admission permit");
        let lease = AdmissionLease {
            _permit: permit,
            operation: Mutex::new(Some(operation)),
            lifecycle: Arc::clone(&gateway.lifecycle),
        };
        let (_event_tx, event_rx) = mpsc::channel(1);
        let (final_tx, final_rx) = oneshot::channel();
        let mut ticket = GatewayTicket::new(
            request_id.clone(),
            event_rx,
            final_rx,
            Arc::new(NoopCancellation),
            Arc::new(AtomicBool::new(false)),
        )
        .with_admission_lease_and_deadlines(
            Box::new(lease),
            fte_types::DeadlinePolicy::default(),
            Duration::ZERO,
            32,
        );

        final_tx
            .send(Ok(completed_response(request_id, "fixture")))
            .expect("publish backend success");
        let event = ticket
            .events
            .recv()
            .await
            .expect("lifecycle failure terminal event");
        let GatewayEvent::Failed { error, .. } = event else {
            panic!("failed atomic lifecycle finish must project Failed: {event:?}");
        };
        assert_eq!(error.code, "gateway_operation_lifecycle_failed");
        assert!(ticket.events.recv().await.is_none());
        let error = ticket
            .final_response()
            .await
            .expect_err("failed release must replace backend success");
        assert_eq!(error.code, "gateway_operation_lifecycle_failed");
        assert_eq!(
            retained_operation
                .snapshot()
                .expect("registry remains valid")
                .expect("operation remains registered")
                .phase,
            operation_lifecycle::OperationPhase::Running
        );

        let shutdown_error = gateway
            .shutdown()
            .await
            .expect_err("failed release must also fail shutdown");
        assert_eq!(shutdown_error.code, "gateway_operation_lifecycle_failed");

        attempt.finish().expect("finish retained attempt");
        retained_operation
            .terminal_and_release(operation_lifecycle::TerminalClass::Failed)
            .expect("release retained operation after test");
    }

    #[tokio::test]
    async fn count_token_deadlines_finalize_admission_explicitly() {
        for deadline_ms in [1, 5] {
            let gateway = Gateway::new(GatewayDefaults::default());
            gateway
                .register_backend(Arc::new(HangingBackend {
                    cancelled: Arc::new(AtomicUsize::new(0)),
                }))
                .expect("register hanging backend");
            let mut request = request();
            request.model = ModelSelector::ExactRoute {
                backend_id: "hanging".to_string(),
                model_id: "hanging-model".to_string(),
            };
            request.deadline.total_ms = Some(deadline_ms);
            let error = gateway
                .count_tokens(request)
                .await
                .expect_err("count-token deadline must fail");
            assert_eq!(error.code, "request_total_deadline_exceeded");
            assert_eq!(gateway.status().active_requests, 0);
        }
    }

    #[tokio::test]
    async fn count_token_lifecycle_failure_replaces_timeout() {
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(HangingBackend {
                cancelled: Arc::new(AtomicUsize::new(0)),
            }))
            .expect("register hanging backend");
        let mut request = request();
        let request_id = request.request_id.clone();
        request.model = ModelSelector::ExactRoute {
            backend_id: "hanging".to_string(),
            model_id: "hanging-model".to_string(),
        };
        request.deadline.total_ms = Some(50);
        let count_gateway = Arc::clone(&gateway);
        let count = tokio::spawn(async move { count_gateway.count_tokens(request).await });
        let operation = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(operation) = gateway
                    .lifecycle
                    .operations
                    .current_lease(&request_id.0)
                    .expect("registry remains valid")
                {
                    break operation;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("count-token admission becomes active");
        let attempt = operation.start_attempt().expect("sabotage active attempt");

        let error = count
            .await
            .expect("count task")
            .expect_err("lifecycle failure replaces timeout");
        assert_eq!(error.code, "gateway_operation_lifecycle_failed");
        assert_eq!(gateway.status().active_requests, 1);
        let snapshot = operation
            .snapshot()
            .expect("registry remains valid")
            .expect("failed finalization remains active");
        assert_eq!(snapshot.phase, operation_lifecycle::OperationPhase::Running);
        assert!(snapshot.terminal.is_none());

        attempt.finish().expect("finish sabotage attempt");
        operation
            .terminal_and_release(operation_lifecycle::TerminalClass::Failed)
            .expect("clean up sabotaged operation");
    }

    #[test]
    fn poisoned_released_snapshot_cannot_partially_release_operation() {
        let registry = operation_lifecycle::OperationRegistry::default();
        let (guard, operation) = registry.reserve("atomic-release").expect("reserve");
        guard.disarm();
        operation.queue().expect("queue");
        operation.start().expect("start");
        operation
            .terminal(operation_lifecycle::TerminalClass::Completed)
            .expect("terminal");
        operation.poison_released_for_test();

        assert_eq!(
            operation.release(),
            Err(operation_lifecycle::RegistryError::Poisoned)
        );
        let snapshot = registry
            .current("atomic-release")
            .expect("registry remains valid")
            .expect("failed release remains active");
        assert_eq!(
            snapshot.phase,
            operation_lifecycle::OperationPhase::Terminal
        );
        assert_eq!(
            snapshot.terminal,
            Some(operation_lifecycle::TerminalClass::Completed)
        );
    }

    #[tokio::test]
    async fn poisoned_lifecycle_state_cannot_report_closed_shutdown_success() {
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(BlockingShutdownBackend {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                shutdowns: Arc::clone(&shutdowns),
            }))
            .expect("register blocking backend");
        let gateway_for_poison = Arc::clone(&gateway);
        let _ = std::thread::spawn(move || {
            let _state = gateway_for_poison
                .lifecycle
                .state
                .lock()
                .expect("lifecycle state");
            panic!("poison lifecycle state for deterministic coverage");
        })
        .join();

        let status = gateway.status();
        assert_eq!(status.lifecycle, GatewayLifecycle::Running);
        assert_eq!(
            status
                .shutdown_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("gateway_lifecycle_state_poisoned")
        );

        let gateway_for_shutdown = Arc::clone(&gateway);
        let shutdown = tokio::spawn(async move { gateway_for_shutdown.shutdown().await });
        entered
            .acquire()
            .await
            .expect("backend shutdown entered")
            .forget();
        assert!(!shutdown.is_finished());
        release.add_permits(1);
        let error = shutdown
            .await
            .expect("shutdown task")
            .expect_err("poisoned lifecycle state cannot close successfully");
        assert_eq!(error.code, "gateway_lifecycle_state_poisoned");
        assert_eq!(shutdowns.load(AtomicOrdering::Acquire), 1);
        let status = gateway.status();
        assert_eq!(status.lifecycle, GatewayLifecycle::Closed);
        assert_eq!(
            status
                .shutdown_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("gateway_lifecycle_state_poisoned")
        );
    }

    #[tokio::test]
    async fn dropped_consumer_ticket_keeps_admission_until_backend_final() {
        let completion = Arc::new(Mutex::new(None));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(DelayedFinalBackend {
                completion: Arc::clone(&completion),
                cancellations: Arc::clone(&cancellations),
            }))
            .expect("register delayed-final backend");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "delayed-final".to_string(),
            model_id: "delayed-final-model".to_string(),
        };

        let ticket = gateway.execute(request).await.expect("start request");
        drop(ticket);
        assert_eq!(gateway.status().active_requests, 1);

        let shutdown_gateway = Arc::clone(&gateway);
        let shutdown = tokio::spawn(async move { shutdown_gateway.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        assert!(cancellations.load(AtomicOrdering::Acquire) >= 1);

        let final_tx = completion
            .lock()
            .expect("completion state")
            .take()
            .expect("pending backend final");
        let _ = final_tx.send(Err(GatewayError::unavailable(
            &RequestId::new(),
            "fixture_backend_stopped",
            "the delayed backend has now terminated",
        )));
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("shutdown must finish after backend final")
            .expect("shutdown task")
            .expect("gateway shutdown");
        assert_eq!(gateway.status().active_requests, 0);
    }

    #[tokio::test]
    async fn shutdown_cancels_active_work_and_waits_for_authoritative_completion() {
        let cancellations = Arc::new(AtomicUsize::new(0));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(DrainingBackend {
                completion: Mutex::new(None),
                cancellations: Arc::clone(&cancellations),
                shutdowns: Arc::clone(&shutdowns),
            }))
            .expect("register draining backend");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "draining".to_string(),
            model_id: "draining-model".to_string(),
        };
        let ticket = gateway.execute(request).await.expect("start request");
        assert_eq!(gateway.status().active_requests, 1);

        tokio::time::timeout(Duration::from_secs(1), gateway.shutdown())
            .await
            .expect("gateway shutdown must not hang")
            .expect("gateway shutdown");
        assert_eq!(cancellations.load(AtomicOrdering::Acquire), 1);
        assert_eq!(shutdowns.load(AtomicOrdering::Acquire), 1);
        assert_eq!(gateway.status().active_requests, 0);
        assert_eq!(gateway.status().lifecycle, GatewayLifecycle::Closed);
        drop(ticket);
    }

    #[tokio::test]
    async fn deadline_does_not_release_admission_before_the_backend_final() {
        let completion = Arc::new(Mutex::new(None));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(DelayedFinalBackend {
                completion: Arc::clone(&completion),
                cancellations: Arc::clone(&cancellations),
            }))
            .expect("register delayed-final backend");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "delayed-final".to_string(),
            model_id: "delayed-final-model".to_string(),
        };
        let request_id = request.request_id.clone();
        request.deadline.first_token_ms = Some(5);
        let ticket = gateway
            .execute(request)
            .await
            .expect("start delayed request");
        let error = ticket
            .final_response()
            .await
            .expect_err("consumer observes first-output deadline");
        assert_eq!(error.code, "first_output_deadline_exceeded");
        assert_eq!(gateway.status().active_requests, 1);
        let lifecycle = gateway
            .lifecycle
            .operations
            .current_lease(&request_id.0)
            .expect("registry remains valid")
            .expect("timed-out operation remains retained");
        let snapshot = lifecycle
            .snapshot()
            .expect("registry remains valid")
            .expect("timed-out operation remains visible");
        assert_eq!(
            snapshot.phase,
            operation_lifecycle::OperationPhase::Terminal
        );
        assert_eq!(
            snapshot.terminal,
            Some(operation_lifecycle::TerminalClass::Failed)
        );

        let shutdown_gateway = Arc::clone(&gateway);
        let shutdown = tokio::spawn(async move { shutdown_gateway.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        assert!(cancellations.load(AtomicOrdering::Acquire) >= 1);

        let final_tx = completion
            .lock()
            .expect("completion state")
            .take()
            .expect("pending backend final");
        let _ = final_tx.send(Ok(completed_response(request_id, "delayed-final")));
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("shutdown waits only until raw backend final")
            .expect("shutdown task")
            .expect("gateway shutdown");
        assert_eq!(gateway.status().active_requests, 0);
        let snapshot = lifecycle
            .snapshot()
            .expect("registry remains valid")
            .expect("released timeout projection remains visible");
        assert_eq!(
            snapshot.phase,
            operation_lifecycle::OperationPhase::Released
        );
        assert_eq!(
            snapshot.terminal,
            Some(operation_lifecycle::TerminalClass::Failed)
        );
    }

    #[tokio::test]
    async fn deferred_release_failure_after_timeout_is_observable_to_shutdown() {
        let completion = Arc::new(Mutex::new(None));
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(DelayedFinalBackend {
                completion: Arc::clone(&completion),
                cancellations: Arc::new(AtomicUsize::new(0)),
            }))
            .expect("register delayed-final backend");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "delayed-final".to_string(),
            model_id: "delayed-final-model".to_string(),
        };
        let request_id = request.request_id.clone();
        request.deadline.first_token_ms = Some(5);
        let ticket = gateway.execute(request).await.expect("start request");
        let error = ticket
            .final_response()
            .await
            .expect_err("consumer observes authoritative timeout");
        assert_eq!(error.class, ErrorClass::Timeout);
        gateway.lifecycle.operations.poison_for_test();

        completion
            .lock()
            .expect("completion state")
            .take()
            .expect("pending backend final")
            .send(Ok(completed_response(request_id, "delayed-final")))
            .expect("publish raw backend completion");

        let error = tokio::time::timeout(Duration::from_secs(1), gateway.shutdown())
            .await
            .expect("poisoned deferred release cannot hang shutdown")
            .expect_err("deferred release failure must fail shutdown");
        assert_eq!(error.code, "gateway_operation_registry_poisoned");
    }

    #[tokio::test]
    async fn premature_completed_event_cannot_outrun_authoritative_timeout() {
        let completion = Arc::new(Mutex::new(None));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(EarlyCompletedEventBackend {
                completion: Arc::clone(&completion),
                cancellations: Arc::clone(&cancellations),
            }))
            .expect("register early-terminal backend");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "early-terminal".to_string(),
            model_id: "early-terminal-model".to_string(),
        };
        let request_id = request.request_id.clone();
        request.deadline.total_ms = Some(5);
        let mut ticket = gateway.execute(request).await.expect("start request");

        let event = tokio::time::timeout(Duration::from_secs(1), ticket.events.recv())
            .await
            .expect("terminal event arrives")
            .expect("terminal event exists");
        let GatewayEvent::Failed { error, .. } = event else {
            panic!("premature backend completion must not outrun timeout: {event:?}");
        };
        assert_eq!(error.code, "request_total_deadline_exceeded");
        assert_eq!(error.class, ErrorClass::Timeout);

        let lifecycle = gateway
            .lifecycle
            .operations
            .current_lease(&request_id.0)
            .expect("registry remains valid")
            .expect("executor still retains release authority");
        let snapshot = lifecycle
            .snapshot()
            .expect("registry remains valid")
            .expect("operation remains visible");
        assert_eq!(
            snapshot.phase,
            operation_lifecycle::OperationPhase::Terminal
        );
        assert_eq!(
            snapshot.terminal,
            Some(operation_lifecycle::TerminalClass::Failed)
        );
        assert!(cancellations.load(AtomicOrdering::Acquire) >= 1);

        let shutdown_gateway = Arc::clone(&gateway);
        let shutdown = tokio::spawn(async move { shutdown_gateway.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        completion
            .lock()
            .expect("completion state")
            .take()
            .expect("pending backend final")
            .send(Ok(completed_response(request_id, "early-terminal")))
            .expect("publish raw backend completion");
        assert!(ticket.events.recv().await.is_none());
        let error = ticket
            .final_response()
            .await
            .expect_err("final result matches timeout event");
        assert_eq!(error.code, "request_total_deadline_exceeded");
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("shutdown completes after raw final")
            .expect("shutdown task")
            .expect("clean shutdown");
        assert_eq!(gateway.status().active_requests, 0);
    }

    #[tokio::test]
    async fn dropped_event_receiver_retains_executor_release_until_raw_final() {
        let completion = Arc::new(Mutex::new(None));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(ProgressThenDelayedFinalBackend {
                completion: Arc::clone(&completion),
                cancellations: Arc::clone(&cancellations),
            }))
            .expect("register progress-delayed backend");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "progress-delayed".to_string(),
            model_id: "progress-delayed-model".to_string(),
        };
        let request_id = request.request_id.clone();
        let ticket = gateway.execute(request).await.expect("start request");
        drop(ticket);
        tokio::time::timeout(Duration::from_secs(1), async {
            while cancellations.load(AtomicOrdering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("event wrapper observes dropped receiver");
        assert_eq!(gateway.status().active_requests, 1);
        assert!(cancellations.load(AtomicOrdering::Acquire) >= 1);

        let shutdown_gateway = Arc::clone(&gateway);
        let shutdown = tokio::spawn(async move { shutdown_gateway.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        completion
            .lock()
            .expect("completion state")
            .take()
            .expect("pending backend final")
            .send(Ok(completed_response(request_id, "progress-delayed")))
            .expect("publish raw backend completion");
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("shutdown completes after raw final")
            .expect("shutdown task")
            .expect("clean shutdown");
        assert_eq!(gateway.status().active_requests, 0);
    }

    #[tokio::test]
    async fn dropped_silent_deadline_ticket_cancels_but_retains_executor_release() {
        let completion = Arc::new(Mutex::new(None));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(DelayedFinalBackend {
                completion: Arc::clone(&completion),
                cancellations: Arc::clone(&cancellations),
            }))
            .expect("register silent backend");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "delayed-final".to_string(),
            model_id: "delayed-final-model".to_string(),
        };
        request.deadline.total_ms = Some(60_000);
        let ticket = gateway.execute(request).await.expect("start request");
        drop(ticket);
        assert!(cancellations.load(AtomicOrdering::Acquire) >= 1);
        assert_eq!(gateway.status().active_requests, 1);

        let shutdown_gateway = Arc::clone(&gateway);
        let shutdown = tokio::spawn(async move { shutdown_gateway.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        completion
            .lock()
            .expect("completion state")
            .take()
            .expect("pending backend final")
            .send(Err(GatewayError::unavailable(
                &RequestId::new(),
                "fixture_backend_stopped",
                "the delayed backend has now terminated",
            )))
            .expect("publish raw backend final");
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("shutdown completes after raw final")
            .expect("shutdown task")
            .expect("clean shutdown");
        assert_eq!(gateway.status().active_requests, 0);
    }

    #[tokio::test]
    async fn shutdown_closes_registration_atomically_and_is_idempotent() {
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(BlockingShutdownBackend {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                shutdowns: Arc::clone(&shutdowns),
            }))
            .expect("register blocking backend");

        let first_gateway = Arc::clone(&gateway);
        let first = tokio::spawn(async move { first_gateway.shutdown().await });
        entered
            .acquire()
            .await
            .expect("backend shutdown entered")
            .forget();
        assert_eq!(gateway.status().lifecycle, GatewayLifecycle::Quiescing);

        let register_error = gateway
            .register_backend(Arc::new(NeverExecuteBackend(descriptor(
                "late",
                BackendLocation::LocalEmbedded,
            ))))
            .expect_err("registration after quiescing must fail");
        assert_eq!(register_error.code, "gateway_quiescing");
        let concurrency_error = gateway
            .set_backend_concurrency("blocking", 2)
            .expect_err("admission mutation after quiescing must fail");
        assert_eq!(concurrency_error.code, "gateway_quiescing");

        let second_gateway = Arc::clone(&gateway);
        let second = tokio::spawn(async move { second_gateway.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        release.add_permits(1);
        first
            .await
            .expect("first shutdown task")
            .expect("first shutdown");
        second
            .await
            .expect("second shutdown task")
            .expect("second shutdown");
        gateway
            .shutdown()
            .await
            .expect("closed shutdown is idempotent");
        assert_eq!(shutdowns.load(AtomicOrdering::Acquire), 1);
        assert_eq!(gateway.status().lifecycle, GatewayLifecycle::Closed);

        let error = gateway
            .execute(request())
            .await
            .expect_err("closed gateways reject new requests");
        assert_eq!(error.code, "gateway_closed");
    }

    #[tokio::test]
    async fn concurrent_shutdown_followers_never_miss_closed_notification() {
        const ROUNDS: usize = 32;
        const FOLLOWERS: usize = 8;

        for _ in 0..ROUNDS {
            let entered = Arc::new(Semaphore::new(0));
            let release = Arc::new(Semaphore::new(0));
            let shutdowns = Arc::new(AtomicUsize::new(0));
            let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
            gateway
                .register_backend(Arc::new(BlockingShutdownBackend {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                    shutdowns: Arc::clone(&shutdowns),
                }))
                .expect("register blocking backend");

            let leader_gateway = Arc::clone(&gateway);
            let leader = tokio::spawn(async move { leader_gateway.shutdown().await });
            entered
                .acquire()
                .await
                .expect("backend shutdown entered")
                .forget();

            let mut followers = JoinSet::new();
            for _ in 0..FOLLOWERS {
                let follower_gateway = Arc::clone(&gateway);
                followers.spawn(async move { follower_gateway.shutdown().await });
            }
            tokio::task::yield_now().await;
            release.add_permits(1);

            tokio::time::timeout(Duration::from_secs(1), async {
                leader
                    .await
                    .expect("leader shutdown task")
                    .expect("leader shutdown");
                while let Some(result) = followers.join_next().await {
                    result
                        .expect("follower shutdown task")
                        .expect("follower shutdown");
                }
            })
            .await
            .expect("all shutdown waiters observe closure");
            assert_eq!(shutdowns.load(AtomicOrdering::Acquire), 1);
        }
    }

    #[tokio::test]
    async fn poisoned_gateway_state_is_still_fully_drained_before_erroring() {
        let cancellations = Arc::new(AtomicUsize::new(0));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(DrainingBackend {
                completion: Mutex::new(None),
                cancellations: Arc::clone(&cancellations),
                shutdowns: Arc::clone(&shutdowns),
            }))
            .expect("register draining backend");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "draining".to_string(),
            model_id: "draining-model".to_string(),
        };
        let ticket = gateway.execute(request).await.expect("start request");

        let gateway_for_poison = Arc::clone(&gateway);
        let _ = std::thread::spawn(move || {
            let _state = gateway_for_poison.state.write().expect("gateway state");
            panic!("poison gateway state for deterministic shutdown coverage");
        })
        .join();
        let status = gateway.status();
        assert_eq!(status.lifecycle, GatewayLifecycle::Running);
        assert_eq!(
            status
                .shutdown_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("gateway_state_poisoned")
        );

        let error = tokio::time::timeout(Duration::from_secs(1), gateway.shutdown())
            .await
            .expect("poisoned shutdown must not hang")
            .expect_err("poison remains observable after resources drain");
        assert_eq!(error.code, "gateway_state_poisoned");
        assert_eq!(cancellations.load(AtomicOrdering::Acquire), 1);
        assert_eq!(shutdowns.load(AtomicOrdering::Acquire), 1);
        assert_eq!(gateway.status().active_requests, 0);
        assert_eq!(gateway.status().lifecycle, GatewayLifecycle::Closed);
        drop(ticket);
    }

    #[tokio::test]
    async fn admission_is_bounded_and_a_dropped_consumer_releases_and_cancels() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(HangingBackend {
                cancelled: Arc::clone(&cancelled),
            }))
            .expect("register hanging backend");
        gateway
            .set_backend_concurrency("hanging", 1)
            .expect("bound admission");

        let first = gateway.execute(request()).await.expect("first admission");
        assert_eq!(gateway.status().active_requests, 1);

        let mut second = request();
        second.deadline.queue_ms = Some(0);
        let error = gateway
            .execute(second)
            .await
            .expect_err("second request must not bypass the one-slot limit");
        assert_eq!(error.code, "queue_deadline_exceeded");

        drop(first);
        wait_for_no_active(&gateway).await;
        assert_eq!(cancelled.load(AtomicOrdering::Acquire), 1);
        assert_eq!(gateway.status().active_requests, 0);
    }

    #[tokio::test]
    async fn profile_routes_retry_only_before_a_ticket_exists() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(NeverExecuteBackend(descriptor(
                "a-fail",
                BackendLocation::LocalEmbedded,
            ))))
            .expect("register failing route");
        gateway
            .register_backend(Arc::new(HangingBackend {
                cancelled: Arc::clone(&cancelled),
            }))
            .expect("register fallback route");

        let mut request = request();
        request.routing.retry_before_output = true;
        let ticket = gateway
            .execute(request)
            .await
            .expect("profile request should use the second route after retryable setup failure");
        assert_eq!(gateway.status().active_requests, 1);
        drop(ticket);
        assert_eq!(cancelled.load(AtomicOrdering::Acquire), 1);
    }

    #[tokio::test]
    async fn fallback_stops_when_setup_error_cannot_finalize_poisoned_lifecycle() {
        let sabotaged_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(LifecycleSabotageBackend {
                descriptor: descriptor("a-sabotage", BackendLocation::LocalEmbedded),
                registry: gateway.lifecycle.operations.clone(),
                sabotage: LifecycleSabotage::Poison,
                calls: Arc::clone(&sabotaged_calls),
            }))
            .expect("register sabotaged route");
        gateway
            .register_backend(Arc::new(CountingFailureBackend {
                descriptor: descriptor("z-fallback", BackendLocation::LocalEmbedded),
                calls: Arc::clone(&fallback_calls),
            }))
            .expect("register fallback route");
        let mut request = request();
        request.routing.retry_before_output = true;

        let error = gateway
            .execute(request)
            .await
            .expect_err("lifecycle failure must stop provider fallback");
        assert_eq!(error.code, "gateway_operation_registry_poisoned");
        assert_eq!(sabotaged_calls.load(AtomicOrdering::Acquire), 1);
        assert_eq!(fallback_calls.load(AtomicOrdering::Acquire), 0);
        let shutdown_error = tokio::time::timeout(Duration::from_secs(1), gateway.shutdown())
            .await
            .expect("poisoned shutdown terminates")
            .expect_err("poisoned lifecycle cannot report shutdown success");
        assert_eq!(shutdown_error.code, "gateway_operation_registry_poisoned");
    }

    #[tokio::test]
    async fn profile_routes_do_not_retry_without_explicit_policy() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(NeverExecuteBackend(descriptor(
                "a-fail",
                BackendLocation::LocalEmbedded,
            ))))
            .expect("register failing route");
        gateway
            .register_backend(Arc::new(HangingBackend {
                cancelled: Arc::clone(&cancelled),
            }))
            .expect("register fallback route");

        let error = gateway
            .execute(request())
            .await
            .expect_err("fallback requires explicit retry_before_output policy");
        assert_eq!(error.code, "not_executed");
        assert_eq!(cancelled.load(AtomicOrdering::Acquire), 0);
    }

    #[tokio::test]
    async fn profile_routes_can_retry_a_backend_startup_timeout() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(SlowStartupBackend("a-slow")))
            .expect("register slow route");
        gateway
            .register_backend(Arc::new(HangingBackend {
                cancelled: Arc::clone(&cancelled),
            }))
            .expect("register fallback route");
        let mut request = request();
        request.routing.retry_before_output = true;
        request.deadline.model_load_ms = Some(5);

        let ticket = gateway
            .execute(request)
            .await
            .expect("startup timeout should permit a pre-output fallback");
        drop(ticket);
        assert_eq!(cancelled.load(AtomicOrdering::Acquire), 1);
    }

    #[tokio::test]
    async fn repeated_retryable_setup_failures_open_the_backend_circuit() {
        let calls = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(CountingFailureBackend {
                descriptor: descriptor("unstable", BackendLocation::LocalEmbedded),
                calls: Arc::clone(&calls),
            }))
            .expect("register unstable route");

        for _ in 0..CIRCUIT_FAILURE_THRESHOLD {
            let error = gateway
                .execute(request())
                .await
                .expect_err("fixture setup must fail");
            assert_eq!(error.code, "fixture_provider_unavailable");
        }
        let error = gateway
            .execute(request())
            .await
            .expect_err("open circuit must remove the route before execution");
        assert_eq!(error.code, "route_unavailable");
        assert!(error.safe_detail.contains("repeated setup failures"));
        assert_eq!(
            calls.load(AtomicOrdering::Acquire),
            CIRCUIT_FAILURE_THRESHOLD as usize
        );
    }

    #[tokio::test]
    async fn exact_routes_never_fallback_after_setup_failure() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(NeverExecuteBackend(descriptor(
                "a-fail",
                BackendLocation::LocalEmbedded,
            ))))
            .expect("register failing route");
        gateway
            .register_backend(Arc::new(HangingBackend {
                cancelled: Arc::clone(&cancelled),
            }))
            .expect("register fallback route");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "a-fail".to_string(),
            model_id: "a-fail-model".to_string(),
        };

        let error = gateway
            .execute(request)
            .await
            .expect_err("an exact route must not be silently replaced");
        assert_eq!(error.code, "not_executed");
        assert_eq!(cancelled.load(AtomicOrdering::Acquire), 0);
        assert_eq!(gateway.status().active_requests, 0);
    }

    #[tokio::test]
    async fn exact_setup_error_is_replaced_when_lifecycle_was_already_terminalized() {
        let calls = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(LifecycleSabotageBackend {
                descriptor: descriptor("sabotage", BackendLocation::LocalEmbedded),
                registry: gateway.lifecycle.operations.clone(),
                sabotage: LifecycleSabotage::Terminalize,
                calls: Arc::clone(&calls),
            }))
            .expect("register sabotaged route");
        let mut request = request();
        request.model = ModelSelector::ExactRoute {
            backend_id: "sabotage".to_string(),
            model_id: "sabotage-model".to_string(),
        };

        let error = gateway
            .execute(request)
            .await
            .expect_err("lifecycle failure must replace setup error");
        assert_eq!(error.code, "gateway_operation_lifecycle_failed");
        assert_eq!(calls.load(AtomicOrdering::Acquire), 1);
        assert_eq!(gateway.status().active_requests, 1);
        let shutdown_error = tokio::time::timeout(Duration::from_secs(1), gateway.shutdown())
            .await
            .expect("failed lifecycle shutdown terminates")
            .expect_err("failed lifecycle cannot report shutdown success");
        assert_eq!(shutdown_error.code, "gateway_operation_lifecycle_failed");
    }

    #[tokio::test]
    async fn total_deadline_preempts_the_queue_deadline() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(HangingBackend {
                cancelled: Arc::clone(&cancelled),
            }))
            .expect("register hanging backend");
        gateway
            .set_backend_concurrency("hanging", 1)
            .expect("bound admission");
        let first = gateway.execute(request()).await.expect("first admission");

        let mut second = request();
        second.deadline.queue_ms = Some(1_000);
        second.deadline.total_ms = Some(5);
        let error = gateway
            .execute(second)
            .await
            .expect_err("total deadline must bound queue admission");
        assert_eq!(error.code, "request_total_deadline_exceeded");
        assert_eq!(gateway.status().active_requests, 1);

        drop(first);
        wait_for_no_active(&gateway).await;
        assert_eq!(cancelled.load(AtomicOrdering::Acquire), 1);
        assert_eq!(gateway.status().active_requests, 0);
    }

    struct SlowStartupBackend(&'static str);

    #[async_trait]
    impl GatewayBackend for SlowStartupBackend {
        fn descriptor(&self) -> BackendDescriptor {
            descriptor(self.0, BackendLocation::LocalEmbedded)
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Err(GatewayError::unavailable(
                &request.request.request_id,
                "unexpected_start",
                "the slow test backend should time out first",
            ))
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            0
        }
    }

    #[tokio::test]
    async fn local_model_startup_obeys_its_distinct_deadline() {
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(SlowStartupBackend("slow")))
            .expect("register slow backend");
        let mut request = request();
        request.deadline.model_load_ms = Some(5);
        let error = gateway
            .execute(request)
            .await
            .expect_err("model startup must time out");
        assert_eq!(error.code, "backend_startup_deadline_exceeded");
        assert_eq!(error.class, fte_types::ErrorClass::Timeout);
        assert_eq!(gateway.status().active_requests, 0);
    }

    #[tokio::test]
    async fn total_deadline_preempts_a_longer_backend_startup_budget() {
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(SlowStartupBackend("slow")))
            .expect("register slow backend");
        let mut request = request();
        request.deadline.model_load_ms = Some(100);
        request.deadline.total_ms = Some(5);
        let error = gateway
            .execute(request)
            .await
            .expect_err("total request budget must preempt model startup");
        assert_eq!(error.code, "request_total_deadline_exceeded");
        assert_eq!(error.class, fte_types::ErrorClass::Timeout);
        assert_eq!(gateway.status().active_requests, 0);
    }

    async fn wait_for_no_active(gateway: &Gateway) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while gateway.status().active_requests != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fixture request must drain");
    }
}
