//! Privacy-first route planning and backend orchestration.

use fte_types::{
    BackendDescriptor, BackendLocation, BackendRequest, CancelTarget, ErrorClass, GatewayBackend,
    GatewayError, GatewayLifecycle, GatewayRequest, GatewayStatus, GatewayTicket, GatewayUsage,
    ModelDescriptor, ModelSelector, PrivacyPolicy, RequestId, ResolvedRoute, ResponseFormat,
    RouteProfile,
};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
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
    active_requests: BTreeSet<RequestId>,
    shutdown_error: Option<GatewayError>,
}

#[derive(Default)]
struct LifecycleControl {
    state: Mutex<LifecycleState>,
    changed: Notify,
}

enum ShutdownDisposition {
    Lead(Vec<RequestId>),
    Wait,
    Complete(Result<(), GatewayError>),
}

impl LifecycleControl {
    fn ensure_running(&self, request_id: &RequestId) -> Result<(), GatewayError> {
        let state = self.lock_state();
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
        let state = self.lock_state();
        if state.phase != GatewayLifecycle::Running {
            return Err(gateway_closed_error(request_id, state.phase));
        }
        operation()
    }

    fn register_request(&self, request_id: &RequestId) -> Result<(), GatewayError> {
        let mut state = self.lock_state();
        if state.phase != GatewayLifecycle::Running {
            return Err(gateway_closed_error(request_id, state.phase));
        }
        if !state.active_requests.insert(request_id.clone()) {
            return Err(GatewayError {
                code: "request_already_active".to_string(),
                class: ErrorClass::Unavailable,
                retryable: false,
                http_status: 409,
                request_id: request_id.clone(),
                provider: None,
                safe_detail: "a request with this identifier is already active".to_string(),
            });
        }
        Ok(())
    }

    fn release_request(&self, request_id: &RequestId) {
        let mut state = self.lock_state();
        state.active_requests.remove(request_id);
        self.changed.notify_waiters();
    }

    fn begin_shutdown(&self) -> ShutdownDisposition {
        let mut state = self.lock_state();
        match state.phase {
            GatewayLifecycle::Running => {
                state.phase = GatewayLifecycle::Quiescing;
                let active = state.active_requests.iter().cloned().collect();
                self.changed.notify_waiters();
                ShutdownDisposition::Lead(active)
            }
            GatewayLifecycle::Quiescing => ShutdownDisposition::Wait,
            GatewayLifecycle::Closed => {
                ShutdownDisposition::Complete(state.shutdown_error.clone().map_or(Ok(()), Err))
            }
        }
    }

    fn snapshot(&self) -> (GatewayLifecycle, usize, Option<GatewayError>) {
        let state = self.lock_state();
        (
            state.phase,
            state.active_requests.len(),
            state.shutdown_error.clone(),
        )
    }

    async fn wait_until_drained(&self) {
        loop {
            let changed = self.changed.notified();
            let drained = self.lock_state().active_requests.is_empty();
            if drained {
                return;
            }
            changed.await;
        }
    }

    async fn wait_until_closed(&self) -> Result<(), GatewayError> {
        loop {
            let changed = self.changed.notified();
            let result = {
                let state = self.lock_state();
                (state.phase == GatewayLifecycle::Closed)
                    .then(|| state.shutdown_error.clone().map_or(Ok(()), Err))
            };
            if let Some(result) = result {
                return result;
            }
            changed.await;
        }
    }

    fn finish_shutdown(&self, result: &Result<(), GatewayError>) {
        let mut state = self.lock_state();
        state.phase = GatewayLifecycle::Closed;
        state.shutdown_error = result.as_ref().err().cloned();
        self.changed.notify_waiters();
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, LifecycleState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            let execution = backend.execute(BackendRequest {
                request: request.clone(),
                route,
            });
            let (startup_limit, total_is_limit) = stage_deadline(
                startup_ms,
                deadline.total_ms,
                started_at.elapsed(),
                &request_id,
            )?;
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
                    self.record_backend_success(&backend_id)?;
                    return Ok(ticket.with_admission_lease(Box::new(lease)).with_deadlines(
                        deadline,
                        started_at.elapsed(),
                        event_capacity,
                    ));
                }
                Err(error) if retryable_setup_failure(fallback_allowed, &error) => {
                    self.record_backend_failure(&backend_id)?;
                    last_error = Some(error);
                }
                Err(error) => {
                    if error.retryable && error.code != "request_total_deadline_exceeded" {
                        self.record_backend_failure(&backend_id)?;
                    }
                    return Err(error);
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
        let _lease = self
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
        let (limit, total_is_limit) = stage_deadline(
            stage_ms,
            request.deadline.total_ms,
            started_at.elapsed(),
            &request_id,
        )?;
        let count = backend.count_tokens(BackendRequest { request, route });
        if let Some(limit) = limit {
            tokio::time::timeout(limit, count).await.map_err(|_| {
                if total_is_limit {
                    total_timeout(&request_id)
                } else {
                    startup_timeout(&request_id, &backend_id)
                }
            })?
        } else {
            count.await
        }
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
        match self.lifecycle.begin_shutdown() {
            ShutdownDisposition::Complete(result) => return result,
            ShutdownDisposition::Wait => return self.lifecycle.wait_until_closed().await,
            ShutdownDisposition::Lead(active_request_ids) => {
                let (backends, admissions, state_error) = match self.state.read() {
                    Ok(state) => (
                        state.backends.values().cloned().collect::<Vec<_>>(),
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
                            state.backends.values().cloned().collect::<Vec<_>>(),
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
                    for backend in &backends {
                        let _ = backend.cancel(&request_id, CancelTarget::Request);
                    }
                }

                // The coordinator is intentionally detached: once quiescing
                // begins, dropping one shutdown caller must not strand the
                // gateway forever or leave backend tasks alive.
                let lifecycle = Arc::clone(&self.lifecycle);
                tokio::spawn(async move {
                    let mut tasks = JoinSet::new();
                    for backend in backends {
                        tasks.spawn(async move { backend.shutdown().await });
                    }
                    let mut first_error = state_error;
                    while let Some(result) = tasks.join_next().await {
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
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
                    lifecycle.wait_until_drained().await;
                    let result = first_error.map_or(Ok(()), Err);
                    lifecycle.finish_shutdown(&result);
                });
            }
        }
        self.lifecycle.wait_until_closed().await
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
        self.lifecycle.register_request(request_id)?;
        Ok(AdmissionLease {
            _permit: permit,
            request_id: request_id.clone(),
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
    request_id: RequestId,
    lifecycle: Arc<LifecycleControl>,
}

impl Drop for AdmissionLease {
    fn drop(&mut self) {
        self.lifecycle.release_request(&self.request_id);
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
        ModelSelector::ExactModel { model_id } if model_id != &model.id => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use fte_types::{
        BackendReadiness, ContentBlock, GenerationInput, InputItem, MessageRole, ModelCapabilities,
        PromptForm, RouteObservations, RoutingPolicy, SamplingOptions, StoragePolicy, StreamPolicy,
        TicketCancellation, ToolPolicy,
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

    fn descriptor(id: &str, location: BackendLocation) -> BackendDescriptor {
        BackendDescriptor {
            id: id.to_string(),
            display_name: id.to_string(),
            location,
            models: vec![ModelDescriptor {
                id: format!("{id}-model"),
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
                Arc::new(NoopCancellation),
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
            .expect("shutdown waits only until raw backend final")
            .expect("shutdown task")
            .expect("gateway shutdown");
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
