//! Backend registry and execution service for interoperable speech consumers.
//!
//! The service owns no Tauri state. It plans over the descriptors of actually
//! registered backends, pins the resolved backend/model/voice into the request,
//! and then dispatches through the protocol-neutral `SpeechBackend` trait.

use serde::{Deserialize, Serialize};
use speech_native_router::{SpeechRouteError, SpeechRoutePlan, SpeechRouter};
use speech_native_types::{
    AudioInput, CapabilitySourceReport, DEFAULT_SPEECH_EVENT_CAPACITY, EncodedAudioFormat,
    PlatformCapabilitySnapshot, PlatformTarget, ProbeSourceStatus, SPEECH_CAPABILITY_SCHEMA,
    SpeechBackend, SpeechBackendDescriptor, SpeechCancellation, SpeechCapability,
    SpeechDeadlinePolicy, SpeechError, SpeechErrorClass, SpeechRequestId, SpeechRouteSelector,
    SpeechUsage, SynthesisEvent, SynthesisRequest, SynthesisTicket, TaskSupervisor,
    TaskSupervisorError, TranscriptionEvent, TranscriptionInput, TranscriptionRequest,
    TranscriptionTicket,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::time::Instant;

pub mod operation_lifecycle;

const REGISTERED_SOURCE_ID: &str = "registered-speech-backends";

#[derive(Debug, thiserror::Error, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpeechHostError {
    #[error("speech route planning failed: {error}")]
    Route { error: SpeechRouteError },
    #[error("speech backend failed: {error}")]
    Backend { error: SpeechError },
    #[error("speech gateway state is unavailable")]
    StateUnavailable,
    #[error("speech backend id is already registered: {backend_id}")]
    BackendDuplicate { backend_id: String },
    #[error("speech backend descriptor is invalid: {detail}")]
    BackendInvalid { detail: String },
    #[error("selected speech backend is no longer registered: {backend_id}")]
    BackendMissing { backend_id: String },
    #[error("speech host admission is closed")]
    AdmissionClosed,
    #[error("speech request id is already active: {request_id}")]
    RequestDuplicate { request_id: SpeechRequestId },
    #[error("speech request nonce space is exhausted")]
    NonceExhausted,
    #[error("one or more speech backends failed during shutdown")]
    Shutdown { failures: Vec<SpeechError> },
}

impl From<SpeechRouteError> for SpeechHostError {
    fn from(error: SpeechRouteError) -> Self {
        Self::Route { error }
    }
}

impl From<SpeechError> for SpeechHostError {
    fn from(error: SpeechError) -> Self {
        Self::Backend { error }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeechHostStatus {
    pub target: PlatformTarget,
    pub backends: Vec<SpeechBackendDescriptor>,
}

pub struct SpeechHost {
    target: PlatformTarget,
    router: SpeechRouter,
    lifecycle: Arc<HostLifecycle>,
    clock: Arc<dyn DeadlineClock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostPhase {
    Running,
    Quiescing,
    Closed,
}

struct ActiveRoute {
    backend: Arc<dyn SpeechBackend>,
    identity: operation_lifecycle::OperationIdentity,
    cancellation: Arc<CancellationSignal>,
}

struct RegisteredBackend {
    backend: Arc<dyn SpeechBackend>,
    registration_id: String,
    admission_capacity: usize,
    limiter: Arc<Semaphore>,
}

struct CancellationSignal {
    changed: Notify,
    backend_cancel_accepted: Mutex<bool>,
}

struct HostState {
    phase: HostPhase,
    shutdown_started: bool,
    backends: BTreeMap<String, RegisteredBackend>,
    routes: BTreeMap<SpeechRequestId, ActiveRoute>,
    shutdown_result: Option<Result<(), SpeechHostError>>,
}

struct HostShutdownCompletion {
    result: Result<(), SpeechHostError>,
}

struct HostLifecycle {
    state: Mutex<HostState>,
    operations: operation_lifecycle::OperationRegistry,
    faulted: AtomicBool,
    changed: Notify,
    tasks: Arc<TaskSupervisor>,
}

struct HostCancellation {
    lifecycle: Weak<HostLifecycle>,
    request_id: SpeechRequestId,
    identity: operation_lifecycle::OperationIdentity,
    _consumer: operation_lifecycle::ConsumerGuard,
}

struct ReservedRoute {
    plan: SpeechRoutePlan,
    backend: Arc<dyn SpeechBackend>,
    consumer: operation_lifecycle::ConsumerGuard,
    operation: operation_lifecycle::OperationLease,
    limiter: Arc<Semaphore>,
    cancellation: Arc<CancellationSignal>,
}

type DeadlineSleep = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

trait DeadlineClock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep_until(&self, deadline: Instant) -> DeadlineSleep;
}

struct TokioDeadlineClock;

impl DeadlineClock for TokioDeadlineClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep_until(&self, deadline: Instant) -> DeadlineSleep {
        Box::pin(tokio::time::sleep_until(deadline))
    }
}

#[derive(Clone, Copy)]
struct RequestBudget {
    queue_deadline: Option<Instant>,
    model_load: Option<Duration>,
    first_result: Option<Duration>,
    idle_stream: Option<Duration>,
    total_deadline: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineKind {
    Queue,
    ModelLoad,
    FirstResult,
    IdleStream,
    Total,
}

#[derive(Clone, Copy)]
enum DeadlineStage {
    Dispatch,
    Monitor,
}

#[derive(Clone, Copy)]
struct BudgetDeadline {
    kind: DeadlineKind,
    at: Instant,
}

#[derive(Clone, Copy)]
struct MonitorBudget {
    first_result_deadline: Option<Instant>,
    idle_stream: Option<Duration>,
    idle_stream_deadline: Option<Instant>,
    total_deadline: Option<Instant>,
    saw_result: bool,
}

struct SetupGuard {
    lifecycle: Arc<HostLifecycle>,
    request_id: SpeechRequestId,
    operation: operation_lifecycle::OperationLease,
    armed: bool,
}

struct ExecutorOperation {
    lifecycle: Arc<HostLifecycle>,
    request_id: SpeechRequestId,
    backend: Arc<dyn SpeechBackend>,
    cancellation: Arc<CancellationSignal>,
    _backend_lease: OwnedSemaphorePermit,
    operation: operation_lifecycle::OperationLease,
    attempt: Option<operation_lifecycle::AttemptLease>,
    finished: bool,
}

type DispatchFuture<T> = Pin<Box<dyn Future<Output = Result<T, SpeechError>> + Send + 'static>>;
type TicketJoin<T> = fn(T) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

struct PendingDispatch<T: Send + 'static> {
    lifecycle: Arc<HostLifecycle>,
    label: String,
    request_id: SpeechRequestId,
    backend: Arc<dyn SpeechBackend>,
    cancellation: Arc<CancellationSignal>,
    dispatch: Option<DispatchFuture<T>>,
    executor: Option<ExecutorOperation>,
    join_ticket: TicketJoin<T>,
    terminal: operation_lifecycle::TerminalClass,
    stop_requested: bool,
}

impl std::fmt::Debug for SpeechHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpeechHost")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl Default for SpeechHost {
    fn default() -> Self {
        Self::new(PlatformTarget::current())
    }
}

impl SpeechHost {
    #[must_use]
    pub fn new(target: PlatformTarget) -> Self {
        Self::with_clock(target, Arc::new(TokioDeadlineClock))
    }

    fn with_clock(target: PlatformTarget, clock: Arc<dyn DeadlineClock>) -> Self {
        Self {
            target,
            router: SpeechRouter,
            clock,
            lifecycle: Arc::new(HostLifecycle {
                state: Mutex::new(HostState {
                    phase: HostPhase::Running,
                    shutdown_started: false,
                    backends: BTreeMap::new(),
                    routes: BTreeMap::new(),
                    shutdown_result: None,
                }),
                operations: operation_lifecycle::OperationRegistry::default(),
                faulted: AtomicBool::new(false),
                changed: Notify::new(),
                tasks: Arc::new(TaskSupervisor::default()),
            }),
        }
    }

    pub fn register_backend(&self, backend: Arc<dyn SpeechBackend>) -> Result<(), SpeechHostError> {
        let descriptor = backend.descriptor();
        descriptor
            .validate()
            .map_err(|error| SpeechHostError::BackendInvalid {
                detail: error.to_string(),
            })?;
        let mut state = self.lifecycle.state.lock().map_err(|_| {
            self.lifecycle.mark_faulted();
            SpeechHostError::StateUnavailable
        })?;
        if state.phase != HostPhase::Running {
            return Err(SpeechHostError::AdmissionClosed);
        }
        if state.backends.contains_key(&descriptor.id) {
            return Err(SpeechHostError::BackendDuplicate {
                backend_id: descriptor.id,
            });
        }
        let capacity = conservative_backend_capacity(&descriptor)?;
        let registration_id = descriptor.id.clone();
        state.backends.insert(
            registration_id.clone(),
            RegisteredBackend {
                backend,
                registration_id,
                admission_capacity: capacity,
                limiter: Arc::new(Semaphore::new(capacity)),
            },
        );
        Ok(())
    }

    pub fn status(&self) -> Result<SpeechHostStatus, SpeechHostError> {
        Ok(SpeechHostStatus {
            target: self.target.clone(),
            backends: self.descriptors()?,
        })
    }

    pub fn descriptors(&self) -> Result<Vec<SpeechBackendDescriptor>, SpeechHostError> {
        let registered = {
            let state = self.lifecycle.state.lock().map_err(|_| {
                self.lifecycle.mark_faulted();
                SpeechHostError::StateUnavailable
            })?;
            state
                .backends
                .values()
                .map(|registered| {
                    (
                        registered.registration_id.clone(),
                        registered.admission_capacity,
                        Arc::clone(&registered.backend),
                    )
                })
                .collect::<Vec<_>>()
        };
        registered
            .into_iter()
            .map(|(registration_id, admission_capacity, backend)| {
                let descriptor = backend.descriptor();
                validate_observed_descriptor(&registration_id, admission_capacity, &descriptor)?;
                Ok(descriptor)
            })
            .collect()
    }

    pub fn snapshot(&self) -> Result<PlatformCapabilitySnapshot, SpeechHostError> {
        Ok(PlatformCapabilitySnapshot {
            schema: SPEECH_CAPABILITY_SCHEMA.to_string(),
            captured_at_unix_ms: unix_time_ms(),
            target: self.target.clone(),
            adapter_candidates: Vec::new(),
            source_reports: vec![CapabilitySourceReport {
                source_id: REGISTERED_SOURCE_ID.to_string(),
                status: ProbeSourceStatus::Succeeded,
                detail: None,
                backends: self.descriptors()?,
            }],
        })
    }

    pub fn plan_transcription(
        &self,
        request: &TranscriptionRequest,
    ) -> Result<SpeechRoutePlan, SpeechHostError> {
        Ok(self.router.plan_transcription(request, &self.snapshot()?)?)
    }

    pub fn plan_synthesis(
        &self,
        request: &SynthesisRequest,
    ) -> Result<SpeechRoutePlan, SpeechHostError> {
        Ok(self.router.plan_synthesis(request, &self.snapshot()?)?)
    }

    pub async fn transcribe(
        &self,
        mut request: TranscriptionRequest,
    ) -> Result<TranscriptionTicket, SpeechHostError> {
        let request_id = request.context.request_id.clone();
        let budget = RequestBudget::new(&request.context.deadline, &request_id, self.clock.now())?;
        let ReservedRoute {
            plan,
            backend,
            consumer,
            operation,
            limiter,
            cancellation,
        } = self.reserve_transcription(&request)?;
        pin_route(&mut request.context.route, &plan);
        let mut setup = SetupGuard::new(
            Arc::clone(&self.lifecycle),
            request_id.clone(),
            operation.clone(),
        );
        operation.queue().map_err(|error| {
            self.lifecycle.mark_faulted();
            map_registry_error(error)
        })?;
        let backend_lease = match self
            .acquire_backend_lease(
                limiter,
                budget,
                &request_id,
                &plan,
                &operation,
                &cancellation,
            )
            .await
        {
            Ok(lease) => lease,
            Err(error) if is_cancelled_host_error(&error) => {
                setup.cancel_and_release()?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let attempt = operation
            .start()
            .and_then(|()| operation.start_attempt())
            .map_err(|error| {
                self.lifecycle.mark_faulted();
                map_registry_error(error)
            })?;
        let mut executor = ExecutorOperation {
            lifecycle: Arc::clone(&self.lifecycle),
            request_id: request_id.clone(),
            backend: Arc::clone(&backend),
            cancellation: Arc::clone(&cancellation),
            _backend_lease: backend_lease,
            operation: operation.clone(),
            attempt: Some(attempt),
            finished: false,
        };
        setup.disarm();
        if operation_cancelled(&operation)? {
            let error = request_cancelled_error(
                &request_id,
                &plan.selected.route.backend_id,
                "speech request was cancelled before backend dispatch",
            );
            executor
                .finish(operation_lifecycle::TerminalClass::Cancelled)
                .map_err(map_registry_error)?;
            return Err(error.into());
        }
        let dispatch_backend = Arc::clone(&backend);
        let dispatch: DispatchFuture<TranscriptionTicket> =
            Box::pin(async move { dispatch_backend.transcribe(request).await });
        let mut pending = PendingDispatch::new(
            Arc::clone(&self.lifecycle),
            format!("host-abandoned-dispatch-join:{request_id}"),
            request_id.clone(),
            Arc::clone(&backend),
            Arc::clone(&cancellation),
            dispatch,
            executor,
            boxed_join_transcription_ticket,
        );
        let dispatch_operation = operation.clone();
        let dispatch_cancellation = Arc::clone(&cancellation);
        let dispatch_clock = Arc::clone(&self.clock);
        let dispatch_deadline = budget.model_load_cutoff(self.clock.now());
        let dispatch_outcome = match dispatch_deadline {
            Some(deadline) => tokio::select! {
                biased;
                () = wait_for_cancellation(&dispatch_operation, &dispatch_cancellation) => FinalOutcome::Cancelled,
                result = pending.dispatch_mut() => match elapsed_deadline(
                    Some(deadline),
                    dispatch_clock.now(),
                    result.is_ok(),
                ) {
                    Some(kind) => FinalOutcome::BackendAfterDeadline(kind, result),
                    None => FinalOutcome::Backend(result),
                },
                _ = dispatch_clock.sleep_until(deadline.at) => FinalOutcome::TimedOut(deadline.kind),
            },
            None => tokio::select! {
                biased;
                () = wait_for_cancellation(&dispatch_operation, &dispatch_cancellation) => FinalOutcome::Cancelled,
                result = pending.dispatch_mut() => FinalOutcome::Backend(result),
            },
        };
        let backend_ticket_result = match dispatch_outcome {
            FinalOutcome::Backend(result) => result,
            FinalOutcome::TimedOut(kind) => {
                let error = deadline_timeout_error(
                    &request_id,
                    &plan.selected.route.backend_id,
                    kind,
                    DeadlineStage::Dispatch,
                );
                pending.request_stop(operation_lifecycle::TerminalClass::Failed)?;
                return Err(error.into());
            }
            FinalOutcome::BackendAfterDeadline(kind, result) => {
                let error = deadline_timeout_error(
                    &request_id,
                    &plan.selected.route.backend_id,
                    kind,
                    DeadlineStage::Dispatch,
                );
                pending.retain_completed_dispatch(result);
                pending.request_stop(operation_lifecycle::TerminalClass::Failed)?;
                return Err(error.into());
            }
            FinalOutcome::Cancelled => {
                let error = request_cancelled_error(
                    &request_id,
                    &plan.selected.route.backend_id,
                    "speech request was cancelled during backend dispatch",
                );
                pending.request_stop(operation_lifecycle::TerminalClass::Cancelled)?;
                return Err(error.into());
            }
        };
        let mut executor = pending.complete();
        let mut backend_ticket = match backend_ticket_result {
            Ok(ticket) => ticket,
            Err(error) => {
                let finished = executor
                    .finish_backend(terminal_for_error(&error))
                    .map_err(map_registry_error)?;
                if finished.cancellation_requested {
                    return Err(request_cancelled_error(
                        &request_id,
                        &plan.selected.route.backend_id,
                        "speech request was cancelled before backend dispatch completed",
                    )
                    .into());
                }
                return Err(error.into());
            }
        };
        let mut backend_events = std::mem::replace(&mut backend_ticket.events, mpsc::channel(1).1);
        let (event_sender, events) = mpsc::channel(DEFAULT_SPEECH_EVENT_CAPACITY);
        let audio_sink = backend_ticket.audio_sink.take();
        let (final_sender, final_receiver) = oneshot::channel();
        let ticket_cancellation = Arc::new(HostCancellation {
            lifecycle: Arc::downgrade(&self.lifecycle),
            request_id: request_id.clone(),
            identity: operation.identity(),
            _consumer: consumer,
        });
        let monitor_request_id = request_id.clone();
        let monitor_backend_id = plan.selected.route.backend_id.clone();
        let monitor_operation = operation.clone();
        let monitor_cancellation = Arc::clone(&cancellation);
        let monitor_clock = Arc::clone(&self.clock);
        let mut monitor_budget = budget.monitor(self.clock.now());
        self.spawn_monitor(format!("host-final-relay:{request_id}"), async move {
            let backend_final = backend_ticket.final_response();
            tokio::pin!(backend_final);
            let mut backend_events_open = true;
            let mut backend_terminal = None;
            let outcome = loop {
                let deadline = monitor_budget.next_deadline();
                let timeout = wait_for_deadline(Arc::clone(&monitor_clock), deadline);
                tokio::pin!(timeout);
                tokio::select! {
                    biased;
                    () = wait_for_cancellation(&monitor_operation, &monitor_cancellation) => break FinalOutcome::Cancelled,
                    result = &mut backend_final => break match elapsed_deadline(
                        deadline,
                        monitor_clock.now(),
                        result.is_ok(),
                    ) {
                        Some(kind) => FinalOutcome::BackendAfterDeadline(kind, result),
                        None => FinalOutcome::Backend(result),
                    },
                    event = backend_events.recv(), if backend_events_open => match event {
                        Some(event) => {
                            if let Some(kind) = monitor_budget.observe_event(
                                monitor_clock.now(),
                                transcription_event_is_result(&event),
                            ) {
                                break FinalOutcome::TimedOut(kind);
                            }
                            forward_transcription_event(
                                &event_sender,
                                event,
                                &mut backend_terminal,
                            );
                        }
                        None => backend_events_open = false,
                    },
                    kind = &mut timeout => break FinalOutcome::TimedOut(kind),
                }
            };
            match outcome {
                FinalOutcome::Backend(result) => {
                    drain_transcription_events(
                        &event_sender,
                        &mut backend_events,
                        &mut backend_terminal,
                    );
                    let finalized = executor.terminalize(terminal_for_result(&result), true);
                    let delivered = match &finalized {
                        Ok(finished) if finished.cancellation_requested => {
                            Err(request_cancelled_error(
                                &monitor_request_id,
                                &monitor_backend_id,
                                "speech request cancellation won final arbitration",
                            ))
                        }
                        Ok(_) => result,
                        Err(error) => Err(lifecycle_speech_error(&monitor_request_id, error)),
                    };
                    let terminal = transcription_terminal_event(
                        &monitor_request_id,
                        &delivered,
                        backend_terminal,
                    );
                    let terminal_delivered = send_transcription_terminal(&event_sender, terminal);
                    let _consumer_gone = final_sender.send(delivered).is_err();
                    let released = if finalized.is_ok() { executor.release_terminal() } else { Ok(()) };
                    finalized
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                        .and(released.map_err(|error| error.to_string()))
                        .and(terminal_delivered)
                }
                FinalOutcome::Cancelled => {
                    let cancellation_error = request_cancelled_error(
                        &monitor_request_id,
                        &monitor_backend_id,
                        "speech request was cancelled before its backend final",
                    );
                    executor
                        .request_cancel()
                        .map_err(|registry_error| registry_error.to_string())?;
                    drain_transcription_until_final(
                        &mut backend_final,
                        &mut backend_events,
                        &mut backend_events_open,
                    )
                    .await;
                    let finalized = executor.terminalize(operation_lifecycle::TerminalClass::Cancelled, false);
                    let delivered = match &finalized {
                        Ok(_) => Err(cancellation_error),
                        Err(error) => Err(lifecycle_speech_error(&monitor_request_id, error)),
                    };
                    let terminal = transcription_terminal_event(
                        &monitor_request_id,
                        &delivered,
                        backend_terminal,
                    );
                    let terminal_delivered =
                        send_transcription_terminal(&event_sender, terminal);
                    let _consumer_gone = final_sender.send(delivered).is_err();
                    let released = if finalized.is_ok() { executor.release_terminal() } else { Ok(()) };
                    finalized
                        .map(|_| ())
                        .map_err(|registry_error| registry_error.to_string())
                        .and(released.map_err(|error| error.to_string()))
                        .and(terminal_delivered)
                }
                FinalOutcome::TimedOut(kind) => {
                    let error = deadline_timeout_error(
                        &monitor_request_id,
                        &monitor_backend_id,
                        kind,
                        DeadlineStage::Monitor,
                    );
                    executor
                        .request_cancel()
                        .map_err(|registry_error| registry_error.to_string())?;
                    let terminal_delivered = send_transcription_terminal(
                        &event_sender,
                        TranscriptionEvent::Failed {
                            request_id: monitor_request_id.clone(),
                            error: error.clone(),
                        },
                    );
                    let _consumer_gone = final_sender.send(Err(error)).is_err();
                    drain_transcription_until_final(
                        &mut backend_final,
                        &mut backend_events,
                        &mut backend_events_open,
                    )
                    .await;
                    executor
                        .finish(operation_lifecycle::TerminalClass::Failed)
                        .map_err(|registry_error| registry_error.to_string())
                        .and(terminal_delivered)
                }
                FinalOutcome::BackendAfterDeadline(kind, _result) => {
                    let error = deadline_timeout_error(
                        &monitor_request_id,
                        &monitor_backend_id,
                        kind,
                        DeadlineStage::Monitor,
                    );
                    executor
                        .request_cancel()
                        .map_err(|registry_error| registry_error.to_string())?;
                    let terminal_delivered = send_transcription_terminal(
                        &event_sender,
                        TranscriptionEvent::Failed {
                            request_id: monitor_request_id.clone(),
                            error: error.clone(),
                        },
                    );
                    let _consumer_gone = final_sender.send(Err(error)).is_err();
                    executor
                        .finish(operation_lifecycle::TerminalClass::Failed)
                        .map_err(|registry_error| registry_error.to_string())
                        .and(terminal_delivered)
                }
            }
        })?;
        Ok(TranscriptionTicket::new(
            request_id,
            events,
            final_receiver,
            ticket_cancellation,
            audio_sink,
        ))
    }

    pub async fn synthesize(
        &self,
        mut request: SynthesisRequest,
    ) -> Result<SynthesisTicket, SpeechHostError> {
        let request_id = request.context.request_id.clone();
        let budget = RequestBudget::new(&request.context.deadline, &request_id, self.clock.now())?;
        let ReservedRoute {
            plan,
            backend,
            consumer,
            operation,
            limiter,
            cancellation,
        } = self.reserve_synthesis(&request)?;
        pin_route(&mut request.context.route, &plan);
        let mut setup = SetupGuard::new(
            Arc::clone(&self.lifecycle),
            request_id.clone(),
            operation.clone(),
        );
        operation.queue().map_err(|error| {
            self.lifecycle.mark_faulted();
            map_registry_error(error)
        })?;
        let backend_lease = match self
            .acquire_backend_lease(
                limiter,
                budget,
                &request_id,
                &plan,
                &operation,
                &cancellation,
            )
            .await
        {
            Ok(lease) => lease,
            Err(error) if is_cancelled_host_error(&error) => {
                setup.cancel_and_release()?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let attempt = operation
            .start()
            .and_then(|()| operation.start_attempt())
            .map_err(|error| {
                self.lifecycle.mark_faulted();
                map_registry_error(error)
            })?;
        let mut executor = ExecutorOperation {
            lifecycle: Arc::clone(&self.lifecycle),
            request_id: request_id.clone(),
            backend: Arc::clone(&backend),
            cancellation: Arc::clone(&cancellation),
            _backend_lease: backend_lease,
            operation: operation.clone(),
            attempt: Some(attempt),
            finished: false,
        };
        setup.disarm();
        if operation_cancelled(&operation)? {
            let error = request_cancelled_error(
                &request_id,
                &plan.selected.route.backend_id,
                "speech request was cancelled before backend dispatch",
            );
            executor
                .finish(operation_lifecycle::TerminalClass::Cancelled)
                .map_err(map_registry_error)?;
            return Err(error.into());
        }
        let dispatch_backend = Arc::clone(&backend);
        let dispatch: DispatchFuture<SynthesisTicket> =
            Box::pin(async move { dispatch_backend.synthesize(request).await });
        let mut pending = PendingDispatch::new(
            Arc::clone(&self.lifecycle),
            format!("host-abandoned-dispatch-join:{request_id}"),
            request_id.clone(),
            Arc::clone(&backend),
            Arc::clone(&cancellation),
            dispatch,
            executor,
            boxed_join_synthesis_ticket,
        );
        let dispatch_operation = operation.clone();
        let dispatch_cancellation = Arc::clone(&cancellation);
        let dispatch_clock = Arc::clone(&self.clock);
        let dispatch_deadline = budget.model_load_cutoff(self.clock.now());
        let dispatch_outcome = match dispatch_deadline {
            Some(deadline) => tokio::select! {
                biased;
                () = wait_for_cancellation(&dispatch_operation, &dispatch_cancellation) => FinalOutcome::Cancelled,
                result = pending.dispatch_mut() => match elapsed_deadline(
                    Some(deadline),
                    dispatch_clock.now(),
                    result.is_ok(),
                ) {
                    Some(kind) => FinalOutcome::BackendAfterDeadline(kind, result),
                    None => FinalOutcome::Backend(result),
                },
                _ = dispatch_clock.sleep_until(deadline.at) => FinalOutcome::TimedOut(deadline.kind),
            },
            None => tokio::select! {
                biased;
                () = wait_for_cancellation(&dispatch_operation, &dispatch_cancellation) => FinalOutcome::Cancelled,
                result = pending.dispatch_mut() => FinalOutcome::Backend(result),
            },
        };
        let backend_ticket_result = match dispatch_outcome {
            FinalOutcome::Backend(result) => result,
            FinalOutcome::TimedOut(kind) => {
                let error = deadline_timeout_error(
                    &request_id,
                    &plan.selected.route.backend_id,
                    kind,
                    DeadlineStage::Dispatch,
                );
                pending.request_stop(operation_lifecycle::TerminalClass::Failed)?;
                return Err(error.into());
            }
            FinalOutcome::BackendAfterDeadline(kind, result) => {
                let error = deadline_timeout_error(
                    &request_id,
                    &plan.selected.route.backend_id,
                    kind,
                    DeadlineStage::Dispatch,
                );
                pending.retain_completed_dispatch(result);
                pending.request_stop(operation_lifecycle::TerminalClass::Failed)?;
                return Err(error.into());
            }
            FinalOutcome::Cancelled => {
                let error = request_cancelled_error(
                    &request_id,
                    &plan.selected.route.backend_id,
                    "speech request was cancelled during backend dispatch",
                );
                pending.request_stop(operation_lifecycle::TerminalClass::Cancelled)?;
                return Err(error.into());
            }
        };
        let mut executor = pending.complete();
        let mut backend_ticket = match backend_ticket_result {
            Ok(ticket) => ticket,
            Err(error) => {
                let finished = executor
                    .finish_backend(terminal_for_error(&error))
                    .map_err(map_registry_error)?;
                if finished.cancellation_requested {
                    return Err(request_cancelled_error(
                        &request_id,
                        &plan.selected.route.backend_id,
                        "speech request was cancelled before backend dispatch completed",
                    )
                    .into());
                }
                return Err(error.into());
            }
        };
        let mut backend_events = std::mem::replace(&mut backend_ticket.events, mpsc::channel(1).1);
        let (event_sender, events) = mpsc::channel(DEFAULT_SPEECH_EVENT_CAPACITY);
        let (final_sender, final_receiver) = oneshot::channel();
        let ticket_cancellation = Arc::new(HostCancellation {
            lifecycle: Arc::downgrade(&self.lifecycle),
            request_id: request_id.clone(),
            identity: operation.identity(),
            _consumer: consumer,
        });
        let monitor_request_id = request_id.clone();
        let monitor_backend_id = plan.selected.route.backend_id.clone();
        let monitor_operation = operation.clone();
        let monitor_cancellation = Arc::clone(&cancellation);
        let monitor_clock = Arc::clone(&self.clock);
        let mut monitor_budget = budget.monitor(self.clock.now());
        self.spawn_monitor(format!("host-final-relay:{request_id}"), async move {
            let backend_final = backend_ticket.final_response();
            tokio::pin!(backend_final);
            let mut backend_events_open = true;
            let mut backend_terminal = None;
            let outcome = loop {
                let deadline = monitor_budget.next_deadline();
                let timeout = wait_for_deadline(Arc::clone(&monitor_clock), deadline);
                tokio::pin!(timeout);
                tokio::select! {
                    biased;
                    () = wait_for_cancellation(&monitor_operation, &monitor_cancellation) => break FinalOutcome::Cancelled,
                    result = &mut backend_final => break match elapsed_deadline(
                        deadline,
                        monitor_clock.now(),
                        result.is_ok(),
                    ) {
                        Some(kind) => FinalOutcome::BackendAfterDeadline(kind, result),
                        None => FinalOutcome::Backend(result),
                    },
                    event = backend_events.recv(), if backend_events_open => match event {
                        Some(event) => {
                            if let Some(kind) = monitor_budget.observe_event(
                                monitor_clock.now(),
                                synthesis_event_is_result(&event),
                            ) {
                                break FinalOutcome::TimedOut(kind);
                            }
                            forward_synthesis_event(
                                &event_sender,
                                event,
                                &mut backend_terminal,
                            );
                        }
                        None => backend_events_open = false,
                    },
                    kind = &mut timeout => break FinalOutcome::TimedOut(kind),
                }
            };
            match outcome {
                FinalOutcome::Backend(result) => {
                    drain_synthesis_events(
                        &event_sender,
                        &mut backend_events,
                        &mut backend_terminal,
                    );
                    let finalized = executor.terminalize(terminal_for_result(&result), true);
                    let delivered = match &finalized {
                        Ok(finished) if finished.cancellation_requested => {
                            Err(request_cancelled_error(
                                &monitor_request_id,
                                &monitor_backend_id,
                                "speech request cancellation won final arbitration",
                            ))
                        }
                        Ok(_) => result,
                        Err(error) => Err(lifecycle_speech_error(&monitor_request_id, error)),
                    };
                    let terminal =
                        synthesis_terminal_event(&monitor_request_id, &delivered, backend_terminal);
                    let terminal_delivered = send_synthesis_terminal(&event_sender, terminal);
                    let _consumer_gone = final_sender.send(delivered).is_err();
                    let released = if finalized.is_ok() { executor.release_terminal() } else { Ok(()) };
                    finalized
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                        .and(released.map_err(|error| error.to_string()))
                        .and(terminal_delivered)
                }
                FinalOutcome::Cancelled => {
                    let cancellation_error = request_cancelled_error(
                        &monitor_request_id,
                        &monitor_backend_id,
                        "speech request was cancelled before its backend final",
                    );
                    executor
                        .request_cancel()
                        .map_err(|registry_error| registry_error.to_string())?;
                    drain_synthesis_until_final(
                        &mut backend_final,
                        &mut backend_events,
                        &mut backend_events_open,
                    )
                    .await;
                    let finalized = executor.terminalize(operation_lifecycle::TerminalClass::Cancelled, false);
                    let delivered = match &finalized {
                        Ok(_) => Err(cancellation_error),
                        Err(error) => Err(lifecycle_speech_error(&monitor_request_id, error)),
                    };
                    let terminal = synthesis_terminal_event(
                        &monitor_request_id,
                        &delivered,
                        backend_terminal,
                    );
                    let terminal_delivered = send_synthesis_terminal(&event_sender, terminal);
                    let _consumer_gone = final_sender.send(delivered).is_err();
                    let released = if finalized.is_ok() { executor.release_terminal() } else { Ok(()) };
                    finalized
                        .map(|_| ())
                        .map_err(|registry_error| registry_error.to_string())
                        .and(released.map_err(|error| error.to_string()))
                        .and(terminal_delivered)
                }
                FinalOutcome::TimedOut(kind) => {
                    let error = deadline_timeout_error(
                        &monitor_request_id,
                        &monitor_backend_id,
                        kind,
                        DeadlineStage::Monitor,
                    );
                    executor
                        .request_cancel()
                        .map_err(|registry_error| registry_error.to_string())?;
                    let terminal_delivered = send_synthesis_terminal(
                        &event_sender,
                        SynthesisEvent::Failed {
                            request_id: monitor_request_id.clone(),
                            error: error.clone(),
                        },
                    );
                    let _consumer_gone = final_sender.send(Err(error)).is_err();
                    drain_synthesis_until_final(
                        &mut backend_final,
                        &mut backend_events,
                        &mut backend_events_open,
                    )
                    .await;
                    executor
                        .finish(operation_lifecycle::TerminalClass::Failed)
                        .map_err(|registry_error| registry_error.to_string())
                        .and(terminal_delivered)
                }
                FinalOutcome::BackendAfterDeadline(kind, _result) => {
                    let error = deadline_timeout_error(
                        &monitor_request_id,
                        &monitor_backend_id,
                        kind,
                        DeadlineStage::Monitor,
                    );
                    executor
                        .request_cancel()
                        .map_err(|registry_error| registry_error.to_string())?;
                    let terminal_delivered = send_synthesis_terminal(
                        &event_sender,
                        SynthesisEvent::Failed {
                            request_id: monitor_request_id.clone(),
                            error: error.clone(),
                        },
                    );
                    let _consumer_gone = final_sender.send(Err(error)).is_err();
                    executor
                        .finish(operation_lifecycle::TerminalClass::Failed)
                        .map_err(|registry_error| registry_error.to_string())
                        .and(terminal_delivered)
                }
            }
        })?;
        Ok(SynthesisTicket::new(
            request_id,
            events,
            final_receiver,
            ticket_cancellation,
        ))
    }

    #[must_use]
    pub fn cancel(&self, request_id: &SpeechRequestId) -> usize {
        self.lifecycle.cancel(request_id, None)
    }

    pub fn quiesce(&self) -> Result<(), SpeechHostError> {
        let (active, limiters) = {
            let mut state = self.lifecycle.state.lock().map_err(|_| {
                self.lifecycle.mark_faulted();
                SpeechHostError::StateUnavailable
            })?;
            if state.phase != HostPhase::Running {
                return Ok(());
            }
            state.phase = HostPhase::Quiescing;
            (
                state.routes.keys().cloned().collect::<Vec<_>>(),
                state
                    .backends
                    .values()
                    .map(|backend| Arc::clone(&backend.limiter))
                    .collect::<Vec<_>>(),
            )
        };
        for request_id in active {
            self.lifecycle.cancel(&request_id, None);
        }
        for limiter in limiters {
            limiter.close();
        }
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), SpeechHostError> {
        let start = {
            let mut state = self.lifecycle.state.lock().map_err(|_| {
                self.lifecycle.mark_faulted();
                SpeechHostError::StateUnavailable
            })?;
            match state.phase {
                HostPhase::Running => {
                    state.phase = HostPhase::Quiescing;
                    state.shutdown_started = true;
                    Some((
                        state
                            .backends
                            .values()
                            .map(|backend| {
                                (
                                    backend.registration_id.clone(),
                                    Arc::clone(&backend.backend),
                                )
                            })
                            .collect::<Vec<_>>(),
                        state.routes.keys().cloned().collect::<Vec<_>>(),
                        state
                            .backends
                            .values()
                            .map(|backend| Arc::clone(&backend.limiter))
                            .collect::<Vec<_>>(),
                    ))
                }
                HostPhase::Quiescing if !state.shutdown_started => {
                    state.shutdown_started = true;
                    Some((
                        state
                            .backends
                            .values()
                            .map(|backend| {
                                (
                                    backend.registration_id.clone(),
                                    Arc::clone(&backend.backend),
                                )
                            })
                            .collect::<Vec<_>>(),
                        state.routes.keys().cloned().collect::<Vec<_>>(),
                        state
                            .backends
                            .values()
                            .map(|backend| Arc::clone(&backend.limiter))
                            .collect::<Vec<_>>(),
                    ))
                }
                HostPhase::Quiescing => None,
                HostPhase::Closed => {
                    return state
                        .shutdown_result
                        .clone()
                        .unwrap_or(Err(SpeechHostError::StateUnavailable));
                }
            }
        };
        if let Some((backends, active, limiters)) = start {
            for request_id in &active {
                self.lifecycle.cancel(request_id, None);
            }
            for limiter in limiters {
                limiter.close();
            }
            if let Err(error) = HostLifecycle::spawn_shutdown_coordinator(
                Arc::clone(&self.lifecycle),
                backends,
                active,
            ) {
                self.lifecycle
                    .publish_shutdown(HostShutdownCompletion { result: Err(error) });
            }
        }
        self.wait_for_shutdown().await
    }

    fn reserve_transcription(
        &self,
        request: &TranscriptionRequest,
    ) -> Result<ReservedRoute, SpeechHostError> {
        self.reserve(
            request.context.request_id.clone(),
            |snapshot| self.router.plan_transcription(request, snapshot),
            |plan, snapshot| {
                let capability = exact_selected_capability(plan, snapshot)?;
                preflight_complete_audio(request, capability, &plan.selected.route.backend_id)
            },
        )
    }

    fn reserve_synthesis(
        &self,
        request: &SynthesisRequest,
    ) -> Result<ReservedRoute, SpeechHostError> {
        self.reserve(
            request.context.request_id.clone(),
            |snapshot| self.router.plan_synthesis(request, snapshot),
            |_plan, _snapshot| Ok(()),
        )
    }

    fn reserve(
        &self,
        request_id: SpeechRequestId,
        plan: impl FnOnce(&PlatformCapabilitySnapshot) -> Result<SpeechRoutePlan, SpeechRouteError>,
        preflight: impl FnOnce(
            &SpeechRoutePlan,
            &PlatformCapabilitySnapshot,
        ) -> Result<(), SpeechHostError>,
    ) -> Result<ReservedRoute, SpeechHostError> {
        {
            let state = self.lifecycle.state.lock().map_err(|_| {
                self.lifecycle.mark_faulted();
                SpeechHostError::StateUnavailable
            })?;
            if state.phase != HostPhase::Running {
                return Err(SpeechHostError::AdmissionClosed);
            }
        }
        let snapshot = self.snapshot()?;
        let route = plan(&snapshot)?;
        preflight(&route, &snapshot)?;
        let mut state = self.lifecycle.state.lock().map_err(|_| {
            self.lifecycle.mark_faulted();
            SpeechHostError::StateUnavailable
        })?;
        if state.phase != HostPhase::Running {
            return Err(SpeechHostError::AdmissionClosed);
        }
        if state.routes.contains_key(&request_id) {
            return Err(SpeechHostError::RequestDuplicate { request_id });
        }
        let backend_id = &route.selected.route.backend_id;
        let registered =
            state
                .backends
                .get(backend_id)
                .ok_or_else(|| SpeechHostError::BackendMissing {
                    backend_id: backend_id.clone(),
                })?;
        let backend = Arc::clone(&registered.backend);
        let limiter = Arc::clone(&registered.limiter);
        let cancellation = Arc::new(CancellationSignal::default());
        let (consumer, operation) = self
            .lifecycle
            .operations
            .reserve(&request_id.0)
            .map_err(map_registry_error)?;
        state.routes.insert(
            request_id,
            ActiveRoute {
                backend: Arc::clone(&backend),
                identity: operation.identity(),
                cancellation: Arc::clone(&cancellation),
            },
        );
        Ok(ReservedRoute {
            plan: route,
            backend,
            consumer,
            operation,
            limiter,
            cancellation,
        })
    }

    async fn acquire_backend_lease(
        &self,
        limiter: Arc<Semaphore>,
        budget: RequestBudget,
        request_id: &SpeechRequestId,
        plan: &SpeechRoutePlan,
        operation: &operation_lifecycle::OperationLease,
        cancellation: &CancellationSignal,
    ) -> Result<OwnedSemaphorePermit, SpeechHostError> {
        let acquire = limiter.acquire_owned();
        tokio::pin!(acquire);
        let acquired = match budget.queue_cutoff() {
            Some(BudgetDeadline {
                kind: DeadlineKind::Queue,
                at,
            }) => tokio::select! {
                biased;
                () = wait_for_cancellation(operation, cancellation) => Err(SpeechHostError::Backend {
                    error: request_cancelled_error(
                        request_id,
                        &plan.selected.route.backend_id,
                        "speech request was cancelled while queued for backend capacity",
                    ),
                }),
                permit = &mut acquire => permit.map_err(|_| SpeechHostError::AdmissionClosed),
                _ = self.clock.sleep_until(at) => Err(SpeechHostError::Backend {
                    error: request_timeout_error(
                        request_id,
                        &plan.selected.route.backend_id,
                        "speech_queue_timeout",
                        "speech request exceeded its backend queue deadline",
                    ),
                }),
            },
            Some(BudgetDeadline {
                kind: DeadlineKind::Total,
                at,
            }) => tokio::select! {
                biased;
                () = wait_for_cancellation(operation, cancellation) => Err(SpeechHostError::Backend {
                    error: request_cancelled_error(
                        request_id,
                        &plan.selected.route.backend_id,
                        "speech request was cancelled while queued for backend capacity",
                    ),
                }),
                permit = &mut acquire => permit.map_err(|_| SpeechHostError::AdmissionClosed),
                _ = self.clock.sleep_until(at) => Err(SpeechHostError::Backend {
                    error: request_timeout_error(
                        request_id,
                        &plan.selected.route.backend_id,
                        "speech_total_timeout",
                        "speech request exceeded its total deadline while queued",
                    ),
                }),
            },
            Some(_) => return Err(SpeechHostError::StateUnavailable),
            None => tokio::select! {
                biased;
                () = wait_for_cancellation(operation, cancellation) => Err(SpeechHostError::Backend {
                    error: request_cancelled_error(
                        request_id,
                        &plan.selected.route.backend_id,
                        "speech request was cancelled while queued for backend capacity",
                    ),
                }),
                permit = &mut acquire => permit.map_err(|_| SpeechHostError::AdmissionClosed),
            },
        }?;
        Ok(acquired)
    }

    fn spawn_monitor(
        &self,
        label: String,
        monitor: impl Future<Output = Result<(), String>> + Send + 'static,
    ) -> Result<(), SpeechHostError> {
        self.lifecycle
            .tasks
            .spawn(label, monitor)
            .map_err(map_task_supervisor_error)
    }

    async fn wait_for_shutdown(&self) -> Result<(), SpeechHostError> {
        loop {
            let changed = self.lifecycle.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let result = {
                let state = self.lifecycle.state.lock().map_err(|_| {
                    self.lifecycle.mark_faulted();
                    SpeechHostError::StateUnavailable
                })?;
                (state.phase == HostPhase::Closed).then(|| state.shutdown_result.clone())
            };
            if let Some(result) = result {
                return result.unwrap_or(Err(SpeechHostError::StateUnavailable));
            }
            if self.lifecycle.is_faulted() {
                return Err(SpeechHostError::StateUnavailable);
            }
            changed.await;
        }
    }
}

enum FinalOutcome<T> {
    Backend(T),
    BackendAfterDeadline(DeadlineKind, T),
    Cancelled,
    TimedOut(DeadlineKind),
}

impl Default for CancellationSignal {
    fn default() -> Self {
        Self {
            changed: Notify::new(),
            backend_cancel_accepted: Mutex::new(false),
        }
    }
}

impl CancellationSignal {
    fn notify(&self) {
        self.changed.notify_waiters();
    }

    fn cancel_backend(
        &self,
        backend: &Arc<dyn SpeechBackend>,
        request_id: &SpeechRequestId,
    ) -> Result<(), ()> {
        let mut accepted = self.backend_cancel_accepted.lock().map_err(|_| ())?;
        if !*accepted && backend.cancel(request_id) != 0 {
            *accepted = true;
        }
        Ok(())
    }
}

impl<T: Send + 'static> PendingDispatch<T> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        lifecycle: Arc<HostLifecycle>,
        label: String,
        request_id: SpeechRequestId,
        backend: Arc<dyn SpeechBackend>,
        cancellation: Arc<CancellationSignal>,
        dispatch: DispatchFuture<T>,
        executor: ExecutorOperation,
        join_ticket: TicketJoin<T>,
    ) -> Self {
        Self {
            lifecycle,
            label,
            request_id,
            backend,
            cancellation,
            dispatch: Some(dispatch),
            executor: Some(executor),
            join_ticket,
            terminal: operation_lifecycle::TerminalClass::Cancelled,
            stop_requested: false,
        }
    }

    fn dispatch_mut(&mut self) -> Pin<&mut (dyn Future<Output = Result<T, SpeechError>> + Send)> {
        self.dispatch
            .as_mut()
            .expect("pending dispatch owns its future")
            .as_mut()
    }

    fn request_stop(
        &mut self,
        terminal: operation_lifecycle::TerminalClass,
    ) -> Result<(), SpeechHostError> {
        self.terminal = terminal;
        let executor = self
            .executor
            .as_mut()
            .ok_or(SpeechHostError::StateUnavailable)?;
        if operation_cancelled(&executor.operation)? {
            executor.cancellation.notify();
        } else {
            executor.request_cancel().map_err(map_registry_error)?;
        }
        self.stop_requested = true;
        Ok(())
    }

    fn retain_completed_dispatch(&mut self, result: Result<T, SpeechError>) {
        self.dispatch = Some(Box::pin(std::future::ready(result)));
    }

    fn complete(mut self) -> ExecutorOperation {
        self.dispatch.take();
        self.executor
            .take()
            .expect("completed dispatch owns its executor")
    }
}

impl<T: Send + 'static> Drop for PendingDispatch<T> {
    fn drop(&mut self) {
        let (Some(dispatch), Some(mut executor)) = (self.dispatch.take(), self.executor.take())
        else {
            return;
        };
        if !self.stop_requested && executor.request_cancel().is_err() {
            self.lifecycle.mark_faulted();
        }
        let cancellation = Arc::clone(&self.cancellation);
        let backend = Arc::clone(&self.backend);
        let request_id = self.request_id.clone();
        let join_ticket = self.join_ticket;
        let terminal = self.terminal;
        let monitor = async move {
            if let Ok(ticket) = dispatch.await {
                cancellation
                    .cancel_backend(&backend, &request_id)
                    .map_err(|()| "speech cancellation state is unavailable".to_owned())?;
                join_ticket(ticket).await;
            }
            executor
                .finish(terminal)
                .map_err(|registry_error| registry_error.to_string())
        };
        if self
            .lifecycle
            .tasks
            .spawn(self.label.clone(), monitor)
            .is_err()
        {
            self.lifecycle.mark_faulted();
        }
    }
}

async fn wait_for_cancellation(
    operation: &operation_lifecycle::OperationLease,
    cancellation: &CancellationSignal,
) {
    loop {
        let changed = cancellation.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        match operation.snapshot() {
            Ok(Some(snapshot)) if snapshot.cancellation_requested => return,
            Ok(Some(_)) => changed.await,
            Ok(None) | Err(_) => return,
        }
    }
}

fn boxed_join_transcription_ticket(
    ticket: TranscriptionTicket,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    Box::pin(join_transcription_ticket(ticket))
}

fn boxed_join_synthesis_ticket(
    ticket: SynthesisTicket,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    Box::pin(join_synthesis_ticket(ticket))
}

async fn drain_transcription_until_final<F>(
    backend_final: &mut Pin<&mut F>,
    backend_events: &mut mpsc::Receiver<TranscriptionEvent>,
    backend_events_open: &mut bool,
) where
    F: Future,
{
    loop {
        tokio::select! {
            biased;
            _result = backend_final.as_mut() => break,
            event = backend_events.recv(), if *backend_events_open => {
                if event.is_none() {
                    *backend_events_open = false;
                }
            },
        }
    }
}

async fn drain_synthesis_until_final<F>(
    backend_final: &mut Pin<&mut F>,
    backend_events: &mut mpsc::Receiver<SynthesisEvent>,
    backend_events_open: &mut bool,
) where
    F: Future,
{
    loop {
        tokio::select! {
            biased;
            _result = backend_final.as_mut() => break,
            event = backend_events.recv(), if *backend_events_open => {
                if event.is_none() {
                    *backend_events_open = false;
                }
            },
        }
    }
}

async fn join_transcription_ticket(mut ticket: TranscriptionTicket) {
    let mut events = std::mem::replace(&mut ticket.events, mpsc::channel(1).1);
    let final_response = ticket.final_response();
    tokio::pin!(final_response);
    let mut events_open = true;
    loop {
        tokio::select! {
            biased;
            _result = &mut final_response => break,
            event = events.recv(), if events_open => {
                if event.is_none() {
                    events_open = false;
                }
            },
        }
    }
}

async fn join_synthesis_ticket(mut ticket: SynthesisTicket) {
    let mut events = std::mem::replace(&mut ticket.events, mpsc::channel(1).1);
    let final_response = ticket.final_response();
    tokio::pin!(final_response);
    let mut events_open = true;
    loop {
        tokio::select! {
            biased;
            _result = &mut final_response => break,
            event = events.recv(), if events_open => {
                if event.is_none() {
                    events_open = false;
                }
            },
        }
    }
}

fn forward_transcription_event(
    public: &mpsc::Sender<TranscriptionEvent>,
    event: TranscriptionEvent,
    backend_terminal: &mut Option<TranscriptionEvent>,
) {
    if event.is_terminal() {
        if backend_terminal.is_none() {
            *backend_terminal = Some(event);
        }
    } else if public.capacity() > 1 {
        let _bounded_progress = public.try_send(event);
    }
}

fn transcription_event_is_result(event: &TranscriptionEvent) -> bool {
    match event {
        TranscriptionEvent::Partial { text, .. } => !text.is_empty(),
        TranscriptionEvent::Segment { segment, .. } => !segment.text.is_empty(),
        _ => false,
    }
}

fn drain_transcription_events(
    public: &mpsc::Sender<TranscriptionEvent>,
    backend: &mut mpsc::Receiver<TranscriptionEvent>,
    backend_terminal: &mut Option<TranscriptionEvent>,
) {
    while let Ok(event) = backend.try_recv() {
        forward_transcription_event(public, event, backend_terminal);
    }
}

fn transcription_terminal_event(
    request_id: &SpeechRequestId,
    result: &Result<speech_native_types::TranscriptionResponse, SpeechError>,
    backend_terminal: Option<TranscriptionEvent>,
) -> TranscriptionEvent {
    match result {
        Ok(response) => TranscriptionEvent::Completed {
            request_id: request_id.clone(),
            response: response.clone(),
        },
        Err(error) if error.class == SpeechErrorClass::Cancelled => {
            let usage = match backend_terminal {
                Some(TranscriptionEvent::Cancelled { usage, .. }) => usage,
                _ => SpeechUsage::default(),
            };
            TranscriptionEvent::Cancelled {
                request_id: request_id.clone(),
                usage,
            }
        }
        Err(error) => TranscriptionEvent::Failed {
            request_id: request_id.clone(),
            error: error.clone(),
        },
    }
}

fn send_transcription_terminal(
    public: &mpsc::Sender<TranscriptionEvent>,
    event: TranscriptionEvent,
) -> Result<(), String> {
    match public.try_send(event) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            Err("speech transcription terminal event capacity invariant failed".to_owned())
        }
    }
}

fn forward_synthesis_event(
    public: &mpsc::Sender<SynthesisEvent>,
    event: SynthesisEvent,
    backend_terminal: &mut Option<SynthesisEvent>,
) {
    if event.is_terminal() {
        if backend_terminal.is_none() {
            *backend_terminal = Some(event);
        }
    } else if public.capacity() > 1 {
        let _bounded_progress = public.try_send(event);
    }
}

fn synthesis_event_is_result(event: &SynthesisEvent) -> bool {
    match event {
        SynthesisEvent::Audio { chunk, .. } => !chunk.data.is_empty(),
        SynthesisEvent::Alignment { alignment, .. } => !alignment.text.is_empty(),
        _ => false,
    }
}

fn drain_synthesis_events(
    public: &mpsc::Sender<SynthesisEvent>,
    backend: &mut mpsc::Receiver<SynthesisEvent>,
    backend_terminal: &mut Option<SynthesisEvent>,
) {
    while let Ok(event) = backend.try_recv() {
        forward_synthesis_event(public, event, backend_terminal);
    }
}

fn synthesis_terminal_event(
    request_id: &SpeechRequestId,
    result: &Result<speech_native_types::SynthesisResponse, SpeechError>,
    backend_terminal: Option<SynthesisEvent>,
) -> SynthesisEvent {
    match result {
        Ok(response) => SynthesisEvent::Completed {
            request_id: request_id.clone(),
            response: response.clone(),
        },
        Err(error) if error.class == SpeechErrorClass::Cancelled => {
            let usage = match backend_terminal {
                Some(SynthesisEvent::Cancelled { usage, .. }) => usage,
                _ => SpeechUsage::default(),
            };
            SynthesisEvent::Cancelled {
                request_id: request_id.clone(),
                usage,
            }
        }
        Err(error) => SynthesisEvent::Failed {
            request_id: request_id.clone(),
            error: error.clone(),
        },
    }
}

fn send_synthesis_terminal(
    public: &mpsc::Sender<SynthesisEvent>,
    event: SynthesisEvent,
) -> Result<(), String> {
    match public.try_send(event) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            Err("speech synthesis terminal event capacity invariant failed".to_owned())
        }
    }
}

impl RequestBudget {
    fn new(
        policy: &SpeechDeadlinePolicy,
        request_id: &SpeechRequestId,
        started: Instant,
    ) -> Result<Self, SpeechHostError> {
        checked_deadline(started, policy.model_load_ms, request_id, "model-load")?;
        checked_deadline(started, policy.first_result_ms, request_id, "first-result")?;
        checked_deadline(started, policy.idle_stream_ms, request_id, "idle-stream")?;
        Ok(Self {
            queue_deadline: checked_deadline(started, policy.queue_ms, request_id, "queue")?,
            model_load: policy.model_load_ms.map(Duration::from_millis),
            first_result: policy.first_result_ms.map(Duration::from_millis),
            idle_stream: policy.idle_stream_ms.map(Duration::from_millis),
            total_deadline: checked_deadline(started, policy.total_ms, request_id, "total")?,
        })
    }

    fn queue_cutoff(self) -> Option<BudgetDeadline> {
        earliest_deadline(
            self.queue_deadline.map(|at| BudgetDeadline {
                kind: DeadlineKind::Queue,
                at,
            }),
            self.total_deadline,
        )
    }

    fn model_load_cutoff(self, started: Instant) -> Option<BudgetDeadline> {
        earliest_deadline(
            self.model_load.map(|duration| BudgetDeadline {
                kind: DeadlineKind::ModelLoad,
                at: started.checked_add(duration).unwrap_or(started),
            }),
            self.total_deadline,
        )
    }

    fn monitor(self, started: Instant) -> MonitorBudget {
        MonitorBudget {
            first_result_deadline: self
                .first_result
                .map(|duration| started.checked_add(duration).unwrap_or(started)),
            idle_stream: self.idle_stream,
            idle_stream_deadline: None,
            total_deadline: self.total_deadline,
            saw_result: false,
        }
    }
}

impl MonitorBudget {
    fn next_deadline(self) -> Option<BudgetDeadline> {
        let stage = if self.saw_result {
            self.idle_stream_deadline.map(|at| BudgetDeadline {
                kind: DeadlineKind::IdleStream,
                at,
            })
        } else {
            self.first_result_deadline.map(|at| BudgetDeadline {
                kind: DeadlineKind::FirstResult,
                at,
            })
        };
        earliest_deadline(stage, self.total_deadline)
    }

    fn observe_event(&mut self, now: Instant, is_result: bool) -> Option<DeadlineKind> {
        if let Some(kind) = elapsed_deadline(self.next_deadline(), now, is_result) {
            return Some(kind);
        }
        if is_result {
            self.saw_result = true;
            self.first_result_deadline = None;
            self.idle_stream_deadline = self
                .idle_stream
                .map(|duration| now.checked_add(duration).unwrap_or(now));
        }
        None
    }
}

fn elapsed_deadline(
    deadline: Option<BudgetDeadline>,
    now: Instant,
    is_result: bool,
) -> Option<DeadlineKind> {
    deadline.and_then(|deadline| {
        (now > deadline.at
            || (now == deadline.at && (deadline.kind == DeadlineKind::Total || !is_result)))
            .then_some(deadline.kind)
    })
}

fn earliest_deadline(
    stage: Option<BudgetDeadline>,
    total: Option<Instant>,
) -> Option<BudgetDeadline> {
    match (stage, total) {
        (Some(stage), Some(total)) if total <= stage.at => Some(BudgetDeadline {
            kind: DeadlineKind::Total,
            at: total,
        }),
        (Some(stage), _) => Some(stage),
        (None, Some(at)) => Some(BudgetDeadline {
            kind: DeadlineKind::Total,
            at,
        }),
        (None, None) => None,
    }
}

async fn wait_for_deadline(
    clock: Arc<dyn DeadlineClock>,
    deadline: Option<BudgetDeadline>,
) -> DeadlineKind {
    match deadline {
        Some(deadline) => {
            clock.sleep_until(deadline.at).await;
            deadline.kind
        }
        None => std::future::pending::<DeadlineKind>().await,
    }
}

fn checked_deadline(
    started: Instant,
    milliseconds: Option<u64>,
    request_id: &SpeechRequestId,
    kind: &str,
) -> Result<Option<Instant>, SpeechHostError> {
    milliseconds
        .map(|milliseconds| {
            started
                .checked_add(Duration::from_millis(milliseconds))
                .ok_or_else(|| SpeechHostError::Backend {
                    error: SpeechError::invalid_request(
                        request_id,
                        "deadline_out_of_range",
                        &format!("configured {kind} deadline is outside the monotonic clock range"),
                    ),
                })
        })
        .transpose()
}

fn conservative_backend_capacity(
    descriptor: &SpeechBackendDescriptor,
) -> Result<usize, SpeechHostError> {
    let capacity = descriptor
        .capabilities
        .iter()
        .map(|capability| capability.limits.max_concurrent_requests.unwrap_or(1))
        .min()
        .unwrap_or(1);
    if capacity == 0 {
        return Err(SpeechHostError::BackendInvalid {
            detail: format!(
                "backend {} advertises a zero concurrent-request capacity",
                descriptor.id
            ),
        });
    }
    usize::try_from(capacity).map_err(|_| SpeechHostError::BackendInvalid {
        detail: format!(
            "backend {} capacity does not fit this platform",
            descriptor.id
        ),
    })
}

fn validate_observed_descriptor(
    registration_id: &str,
    admission_capacity: usize,
    descriptor: &SpeechBackendDescriptor,
) -> Result<(), SpeechHostError> {
    descriptor
        .validate()
        .map_err(|error| SpeechHostError::BackendInvalid {
            detail: error.to_string(),
        })?;
    if descriptor.id != registration_id {
        return Err(SpeechHostError::BackendInvalid {
            detail: format!(
                "registered backend {registration_id} changed its stable backend identity to {}",
                descriptor.id
            ),
        });
    }
    let observed_capacity = conservative_backend_capacity(descriptor)?;
    if observed_capacity != admission_capacity {
        return Err(SpeechHostError::BackendInvalid {
            detail: format!(
                "registered backend {registration_id} changed its admission capacity from {admission_capacity} to {observed_capacity}"
            ),
        });
    }
    Ok(())
}

fn exact_selected_capability<'a>(
    plan: &SpeechRoutePlan,
    snapshot: &'a PlatformCapabilitySnapshot,
) -> Result<&'a SpeechCapability, SpeechHostError> {
    snapshot
        .source_reports
        .iter()
        .filter(|report| report.status == ProbeSourceStatus::Succeeded)
        .flat_map(|report| report.backends.iter())
        .find(|backend| backend.id == plan.selected.route.backend_id)
        .and_then(|backend| {
            backend
                .capabilities
                .iter()
                .find(|capability| capability.id == plan.selected.capability_id)
        })
        .ok_or(SpeechHostError::StateUnavailable)
}

fn preflight_complete_audio(
    request: &TranscriptionRequest,
    capability: &SpeechCapability,
    backend_id: &str,
) -> Result<(), SpeechHostError> {
    let TranscriptionInput::Complete { audio } = &request.input else {
        return Ok(());
    };
    let duration_ms = match audio {
        AudioInput::Pcm { format, data } => {
            let bytes_per_frame = format.bytes_per_frame();
            let frames = data
                .len()
                .checked_div(bytes_per_frame)
                .and_then(|frames| u64::try_from(frames).ok())
                .ok_or_else(|| {
                    invalid_audio_error(
                        &request.context.request_id,
                        backend_id,
                        "speech_pcm_geometry_invalid",
                        "PCM frame geometry cannot be represented safely",
                    )
                })?;
            audio_duration_ms(frames, format.sample_rate_hz).ok_or_else(|| {
                invalid_audio_error(
                    &request.context.request_id,
                    backend_id,
                    "speech_pcm_geometry_invalid",
                    "PCM duration cannot be represented safely",
                )
            })?
        }
        AudioInput::Encoded {
            format: EncodedAudioFormat::Wav,
            data,
        } => wav_duration_ms(data, &request.context.request_id, backend_id)?,
        AudioInput::Encoded { .. } | AudioInput::Asset { .. } => return Ok(()),
    };
    if capability
        .limits
        .max_audio_ms
        .is_some_and(|maximum| duration_ms > maximum)
    {
        return Err(SpeechHostError::Backend {
            error: SpeechError {
                code: "speech_audio_too_long".to_owned(),
                class: SpeechErrorClass::InvalidRequest,
                retryable: false,
                request_id: request.context.request_id.clone(),
                backend_id: Some(backend_id.to_owned()),
                safe_detail: format!(
                    "audio duration {duration_ms} ms exceeds capability {} limit {} ms",
                    capability.id,
                    capability.limits.max_audio_ms.unwrap_or_default()
                ),
            },
        });
    }
    Ok(())
}

fn audio_duration_ms(frames: u64, sample_rate_hz: u32) -> Option<u64> {
    let sample_rate = u128::from(sample_rate_hz);
    let numerator = u128::from(frames).checked_mul(1_000)?;
    let rounded_up = numerator.checked_add(sample_rate.checked_sub(1)?)? / sample_rate;
    u64::try_from(rounded_up).ok()
}

fn wav_duration_ms(
    bytes: &[u8],
    request_id: &SpeechRequestId,
    backend_id: &str,
) -> Result<u64, SpeechHostError> {
    const RIFF_HEADER_BYTES: usize = 12;
    const CHUNK_HEADER_BYTES: usize = 8;
    const MAX_WAV_CHUNKS: usize = 4_096;
    const MAX_WAV_METADATA_BYTES: usize = 1_048_576;
    if bytes.len() < RIFF_HEADER_BYTES || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV must contain a complete RIFF/WAVE header",
        ));
    }
    let riff_size = read_u32_le(bytes, 4).ok_or_else(|| {
        invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV RIFF size is missing",
        )
    })?;
    let riff_end = usize::try_from(riff_size)
        .ok()
        .and_then(|size| size.checked_add(8))
        .filter(|end| *end == bytes.len())
        .ok_or_else(|| {
            invalid_audio_error(
                request_id,
                backend_id,
                "speech_wav_geometry_invalid",
                "WAV RIFF size does not bind the complete supplied byte sequence",
            )
        })?;
    let mut format = None;
    let mut data_bytes = None;
    let mut offset = RIFF_HEADER_BYTES;
    let mut chunk_count = 0_usize;
    let mut metadata_bytes = 0_usize;
    while offset < riff_end {
        chunk_count = chunk_count.checked_add(1).ok_or_else(|| {
            invalid_audio_error(
                request_id,
                backend_id,
                "speech_wav_geometry_invalid",
                "WAV chunk count overflowed",
            )
        })?;
        if chunk_count > MAX_WAV_CHUNKS {
            return Err(invalid_audio_error(
                request_id,
                backend_id,
                "speech_wav_geometry_invalid",
                "WAV contains too many chunks for bounded preflight",
            ));
        }
        let header_end = offset.checked_add(CHUNK_HEADER_BYTES).ok_or_else(|| {
            invalid_audio_error(
                request_id,
                backend_id,
                "speech_wav_geometry_invalid",
                "WAV chunk header offset overflowed",
            )
        })?;
        if header_end > riff_end {
            return Err(invalid_audio_error(
                request_id,
                backend_id,
                "speech_wav_geometry_invalid",
                "WAV ends inside a chunk header",
            ));
        }
        let chunk_size = usize::try_from(read_u32_le(bytes, offset + 4).unwrap_or_default())
            .map_err(|_| {
                invalid_audio_error(
                    request_id,
                    backend_id,
                    "speech_wav_geometry_invalid",
                    "WAV chunk size does not fit this platform",
                )
            })?;
        let chunk_end = header_end.checked_add(chunk_size).ok_or_else(|| {
            invalid_audio_error(
                request_id,
                backend_id,
                "speech_wav_geometry_invalid",
                "WAV chunk size overflowed",
            )
        })?;
        if chunk_end > riff_end {
            return Err(invalid_audio_error(
                request_id,
                backend_id,
                "speech_wav_geometry_invalid",
                "WAV chunk exceeds the declared RIFF size",
            ));
        }
        let chunk_id = &bytes[offset..offset + 4];
        if chunk_id != b"data" {
            metadata_bytes = metadata_bytes
                .checked_add(CHUNK_HEADER_BYTES)
                .and_then(|total| total.checked_add(chunk_size))
                .and_then(|total| total.checked_add(chunk_size % 2))
                .ok_or_else(|| {
                    invalid_audio_error(
                        request_id,
                        backend_id,
                        "speech_wav_geometry_invalid",
                        "WAV metadata size overflowed",
                    )
                })?;
            if metadata_bytes > MAX_WAV_METADATA_BYTES {
                return Err(invalid_audio_error(
                    request_id,
                    backend_id,
                    "speech_wav_geometry_invalid",
                    "WAV metadata exceeds the bounded preflight allowance",
                ));
            }
        }
        if chunk_id == b"fmt " {
            if format.is_some() {
                return Err(invalid_audio_error(
                    request_id,
                    backend_id,
                    "speech_wav_geometry_invalid",
                    "WAV contains more than one format chunk",
                ));
            }
            format = Some(parse_wav_format(
                &bytes[header_end..chunk_end],
                request_id,
                backend_id,
            )?);
        } else if chunk_id == b"data" && data_bytes.replace(chunk_size).is_some() {
            return Err(invalid_audio_error(
                request_id,
                backend_id,
                "speech_wav_geometry_invalid",
                "WAV contains more than one data chunk",
            ));
        }
        offset = chunk_end
            .checked_add(chunk_size % 2)
            .filter(|next| *next <= riff_end)
            .ok_or_else(|| {
                invalid_audio_error(
                    request_id,
                    backend_id,
                    "speech_wav_geometry_invalid",
                    "WAV chunk padding exceeds the declared RIFF size",
                )
            })?;
    }
    let (sample_rate_hz, block_align) = format.ok_or_else(|| {
        invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV format chunk is missing",
        )
    })?;
    let data_bytes = data_bytes.filter(|size| *size != 0).ok_or_else(|| {
        invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV data chunk is missing or empty",
        )
    })?;
    if !data_bytes.is_multiple_of(block_align) {
        return Err(invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV data does not contain complete sample frames",
        ));
    }
    let frames = u64::try_from(data_bytes / block_align).map_err(|_| {
        invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV frame count does not fit the duration model",
        )
    })?;
    audio_duration_ms(frames, sample_rate_hz).ok_or_else(|| {
        invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV duration cannot be represented safely",
        )
    })
}

fn parse_wav_format(
    bytes: &[u8],
    request_id: &SpeechRequestId,
    backend_id: &str,
) -> Result<(u32, usize), SpeechHostError> {
    if bytes.len() < 16 {
        return Err(invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV format chunk is shorter than 16 bytes",
        ));
    }
    let encoding = read_u16_le(bytes, 0).unwrap_or_default();
    let channels = read_u16_le(bytes, 2).unwrap_or_default();
    let sample_rate_hz = read_u32_le(bytes, 4).unwrap_or_default();
    let byte_rate = read_u32_le(bytes, 8).unwrap_or_default();
    let block_align = read_u16_le(bytes, 12).unwrap_or_default();
    let bits_per_sample = read_u16_le(bytes, 14).unwrap_or_default();
    if !(1..=32).contains(&channels)
        || !(8_000..=384_000).contains(&sample_rate_hz)
        || block_align == 0
        || bits_per_sample == 0
    {
        return Err(invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV format geometry is unsupported or zero",
        ));
    }
    let effective_encoding = match encoding {
        1 | 3 => {
            if bytes.len() != 16 {
                let extra_bytes = usize::from(read_u16_le(bytes, 16).unwrap_or_default());
                let declared_end = 18_usize.checked_add(extra_bytes);
                if bytes.len() < 18 || declared_end != Some(bytes.len()) {
                    return Err(invalid_audio_error(
                        request_id,
                        backend_id,
                        "speech_wav_geometry_invalid",
                        "classic WAV format extension does not bind the complete format chunk",
                    ));
                }
            }
            encoding
        }
        0xfffe => parse_extensible_wav_encoding(bytes, bits_per_sample, request_id, backend_id)?,
        _ => {
            return Err(invalid_audio_error(
                request_id,
                backend_id,
                "speech_wav_geometry_invalid",
                "WAV encoding is not PCM or IEEE float",
            ));
        }
    };
    let width_supported = match effective_encoding {
        1 => matches!(bits_per_sample, 8 | 16 | 24 | 32),
        3 => bits_per_sample == 32,
        _ => false,
    };
    if !width_supported {
        return Err(invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV sample width is not supported by the private decoder boundary",
        ));
    }
    let expected_block_align = u32::from(channels)
        .checked_mul(u32::from(bits_per_sample).div_ceil(8))
        .filter(|expected| *expected == u32::from(block_align));
    let expected_byte_rate = sample_rate_hz
        .checked_mul(u32::from(block_align))
        .filter(|expected| *expected == byte_rate);
    if expected_block_align.is_none() || expected_byte_rate.is_none() {
        return Err(invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "WAV block alignment or byte rate is inconsistent",
        ));
    }
    Ok((sample_rate_hz, usize::from(block_align)))
}

fn parse_extensible_wav_encoding(
    bytes: &[u8],
    bits_per_sample: u16,
    request_id: &SpeechRequestId,
    backend_id: &str,
) -> Result<u16, SpeechHostError> {
    const EXTENSIBLE_BASE_BYTES: usize = 18;
    const EXTENSIBLE_MINIMUM_EXTRA_BYTES: usize = 22;
    const PCM_SUBFORMAT: [u8; 16] = [
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    const FLOAT_SUBFORMAT: [u8; 16] = [
        0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b,
        0x71,
    ];
    let extra_bytes = usize::from(read_u16_le(bytes, 16).unwrap_or_default());
    let declared_end = EXTENSIBLE_BASE_BYTES.checked_add(extra_bytes);
    if extra_bytes < EXTENSIBLE_MINIMUM_EXTRA_BYTES || declared_end != Some(bytes.len()) {
        return Err(invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "extensible WAV format extension is incomplete",
        ));
    }
    let valid_bits = read_u16_le(bytes, 18).unwrap_or_default();
    let encoding = match bytes.get(24..40) {
        Some(subformat) if subformat == PCM_SUBFORMAT => 1,
        Some(subformat) if subformat == FLOAT_SUBFORMAT => 3,
        _ => {
            return Err(invalid_audio_error(
                request_id,
                backend_id,
                "speech_wav_geometry_invalid",
                "extensible WAV subformat is not PCM or IEEE float",
            ));
        }
    };
    let valid_bits_supported = match encoding {
        1 => matches!(valid_bits, 8 | 16 | 24 | 32) && valid_bits <= bits_per_sample,
        3 => valid_bits == 32 && bits_per_sample == 32,
        _ => false,
    };
    if !valid_bits_supported {
        return Err(invalid_audio_error(
            request_id,
            backend_id,
            "speech_wav_geometry_invalid",
            "extensible WAV valid-bit geometry is unsupported or inconsistent",
        ));
    }
    Ok(encoding)
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    Some(u16::from_le_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    Some(u32::from_le_bytes(bytes.get(offset..end)?.try_into().ok()?))
}

fn invalid_audio_error(
    request_id: &SpeechRequestId,
    backend_id: &str,
    code: &str,
    detail: &str,
) -> SpeechHostError {
    SpeechHostError::Backend {
        error: SpeechError {
            code: code.to_owned(),
            class: SpeechErrorClass::InvalidRequest,
            retryable: false,
            request_id: request_id.clone(),
            backend_id: Some(backend_id.to_owned()),
            safe_detail: detail.to_owned(),
        },
    }
}

fn request_timeout_error(
    request_id: &SpeechRequestId,
    backend_id: &str,
    code: &str,
    detail: &str,
) -> SpeechError {
    SpeechError {
        code: code.to_owned(),
        class: SpeechErrorClass::Timeout,
        retryable: true,
        request_id: request_id.clone(),
        backend_id: Some(backend_id.to_owned()),
        safe_detail: detail.to_owned(),
    }
}

fn deadline_timeout_error(
    request_id: &SpeechRequestId,
    backend_id: &str,
    kind: DeadlineKind,
    stage: DeadlineStage,
) -> SpeechError {
    let (code, detail) = match (kind, stage) {
        (DeadlineKind::ModelLoad, _) => (
            "speech_model_load_timeout",
            "speech request exceeded its backend model-load deadline",
        ),
        (DeadlineKind::FirstResult, _) => (
            "speech_first_result_timeout",
            "speech request exceeded its first-result deadline",
        ),
        (DeadlineKind::IdleStream, _) => (
            "speech_idle_stream_timeout",
            "speech request exceeded its streaming idle deadline",
        ),
        (DeadlineKind::Total, DeadlineStage::Dispatch) => (
            "speech_total_timeout",
            "speech request exceeded its total deadline before backend admission completed",
        ),
        (DeadlineKind::Total, DeadlineStage::Monitor) => (
            "speech_total_timeout",
            "speech request exceeded its total deadline",
        ),
        (DeadlineKind::Queue, _) => (
            "speech_queue_timeout",
            "speech request exceeded its backend queue deadline",
        ),
    };
    request_timeout_error(request_id, backend_id, code, detail)
}

fn request_cancelled_error(
    request_id: &SpeechRequestId,
    backend_id: &str,
    detail: &str,
) -> SpeechError {
    SpeechError {
        code: "speech_request_cancelled".to_owned(),
        class: SpeechErrorClass::Cancelled,
        retryable: false,
        request_id: request_id.clone(),
        backend_id: Some(backend_id.to_owned()),
        safe_detail: detail.to_owned(),
    }
}

fn is_cancelled_host_error(error: &SpeechHostError) -> bool {
    matches!(
        error,
        SpeechHostError::Backend { error }
            if error.class == SpeechErrorClass::Cancelled
    )
}

fn operation_cancelled(
    operation: &operation_lifecycle::OperationLease,
) -> Result<bool, SpeechHostError> {
    Ok(operation
        .snapshot()
        .map_err(map_registry_error)?
        .is_some_and(|snapshot| snapshot.cancellation_requested))
}

fn map_task_supervisor_error(error: TaskSupervisorError) -> SpeechHostError {
    match error {
        TaskSupervisorError::AdmissionClosed => SpeechHostError::AdmissionClosed,
        TaskSupervisorError::StateUnavailable | TaskSupervisorError::RuntimeUnavailable => {
            SpeechHostError::StateUnavailable
        }
    }
}

fn map_registry_error(error: operation_lifecycle::RegistryError) -> SpeechHostError {
    match error {
        operation_lifecycle::RegistryError::Duplicate => SpeechHostError::StateUnavailable,
        operation_lifecycle::RegistryError::Exhausted => SpeechHostError::NonceExhausted,
        operation_lifecycle::RegistryError::Stale
        | operation_lifecycle::RegistryError::InvalidTransition
        | operation_lifecycle::RegistryError::StateUnavailable => SpeechHostError::StateUnavailable,
    }
}

fn lifecycle_speech_error(
    request_id: &SpeechRequestId,
    error: &operation_lifecycle::RegistryError,
) -> SpeechError {
    SpeechError::unavailable(
        request_id,
        "speech_lifecycle_finalization_failed",
        &format!("speech lifecycle finalization failed: {error}"),
    )
}

impl HostLifecycle {
    fn spawn_shutdown_coordinator(
        lifecycle: Arc<Self>,
        backends: Vec<(String, Arc<dyn SpeechBackend>)>,
        active: Vec<SpeechRequestId>,
    ) -> Result<(), SpeechHostError> {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| SpeechHostError::StateUnavailable)?;
        runtime.spawn(async move {
            let worker_lifecycle = Arc::clone(&lifecycle);
            let joined = tokio::spawn(async move {
                HostLifecycle::run_shutdown(worker_lifecycle, backends, active).await
            })
            .await;
            let completion = joined.unwrap_or_else(|error| HostShutdownCompletion {
                result: Err(SpeechHostError::Shutdown {
                    failures: vec![SpeechError::unavailable(
                        &SpeechRequestId("speech-host-shutdown".to_owned()),
                        "speech_host_shutdown_panicked",
                        &format!("speech host shutdown coordinator panicked: {error}"),
                    )],
                }),
            });
            lifecycle.publish_shutdown(completion);
        });
        Ok(())
    }

    async fn run_shutdown(
        lifecycle: Arc<Self>,
        backends: Vec<(String, Arc<dyn SpeechBackend>)>,
        active: Vec<SpeechRequestId>,
    ) -> HostShutdownCompletion {
        let mut infrastructure_error = None;
        for request_id in active {
            lifecycle.cancel(&request_id, None);
        }
        let mut failures = Vec::new();
        for (registration_id, backend) in backends {
            let request_id = SpeechRequestId(format!("{registration_id}.shutdown"));
            let error = match tokio::spawn(async move { backend.shutdown().await }).await {
                Ok(result) => result.err(),
                Err(join_error) => Some(SpeechError::unavailable(
                    &request_id,
                    "speech_backend_shutdown_panicked",
                    &format!("speech backend shutdown panicked: {join_error}"),
                )),
            };
            failures.extend(error.clone());
        }
        if let Err(error) = lifecycle.wait_for_active_empty().await {
            infrastructure_error.get_or_insert(error);
        }
        if let Err(error) = lifecycle.tasks.begin_shutdown() {
            infrastructure_error.get_or_insert(map_task_supervisor_error(error));
        }
        if let Err(error) = lifecycle.tasks.wait_for_idle().await {
            infrastructure_error.get_or_insert(map_task_supervisor_error(error));
        }
        let monitor_failure = match lifecycle.tasks.failure_summary() {
            Ok(failure) => failure,
            Err(error) => {
                infrastructure_error.get_or_insert(map_task_supervisor_error(error));
                None
            }
        };
        if let Some(summary) = &monitor_failure {
            failures.push(SpeechError::unavailable(
                &SpeechRequestId("speech-host-monitor".to_owned()),
                "speech_host_monitor_failed",
                &format!(
                    "speech host monitor '{}' failed ({:?}): {}; {} additional failure(s)",
                    summary.first.label,
                    summary.first.kind,
                    summary.first.detail,
                    summary.additional_failures
                ),
            ));
        }
        if lifecycle.is_faulted() {
            failures.push(SpeechError::unavailable(
                &SpeechRequestId("speech-host-lifecycle".to_owned()),
                "speech_host_lifecycle_failed",
                "speech host lifecycle ownership encountered an internal state failure",
            ));
        }
        let result = if let Some(error) = infrastructure_error {
            Err(error)
        } else if failures.is_empty() {
            Ok(())
        } else {
            Err(SpeechHostError::Shutdown { failures })
        };
        HostShutdownCompletion { result }
    }

    fn publish_shutdown(&self, completion: HostShutdownCompletion) {
        let Ok(mut state) = self.state.lock() else {
            self.mark_faulted();
            return;
        };
        state.shutdown_result = Some(completion.result);
        state.phase = HostPhase::Closed;
        self.changed.notify_waiters();
    }

    async fn wait_for_active_empty(&self) -> Result<(), SpeechHostError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.is_faulted() {
                return Err(SpeechHostError::StateUnavailable);
            }
            if self.operations.active_count().map_err(map_registry_error)? == 0 {
                return Ok(());
            }
            changed.await;
        }
    }

    fn mark_faulted(&self) {
        self.faulted.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }

    fn is_faulted(&self) -> bool {
        self.faulted.load(Ordering::Acquire)
            || self.operations.diagnostic_faulted()
            || self.tasks.diagnostic_faulted()
    }

    fn cancel(
        &self,
        request_id: &SpeechRequestId,
        identity: Option<&operation_lifecycle::OperationIdentity>,
    ) -> usize {
        let route = match self.state.lock() {
            Ok(state) => state.routes.get(request_id).and_then(|route| {
                identity
                    .is_none_or(|identity| identity == &route.identity)
                    .then(|| {
                        (
                            Arc::clone(&route.backend),
                            route.identity.clone(),
                            Arc::clone(&route.cancellation),
                        )
                    })
            }),
            Err(_) => {
                self.mark_faulted();
                return 0;
            }
        };
        let Some((backend, route_identity, cancellation)) = route else {
            return 0;
        };
        self.cancel_captured(request_id, route_identity, backend, cancellation, identity)
    }

    fn cancel_captured(
        &self,
        request_id: &SpeechRequestId,
        route_identity: operation_lifecycle::OperationIdentity,
        backend: Arc<dyn SpeechBackend>,
        cancellation: Arc<CancellationSignal>,
        identity: Option<&operation_lifecycle::OperationIdentity>,
    ) -> usize {
        let operation = match self.operations.current_lease(&request_id.0) {
            Ok(Some(operation)) => operation,
            Ok(None) => return 0,
            Err(_) => {
                self.mark_faulted();
                return 0;
            }
        };
        if route_identity != operation.identity() {
            return 0;
        }
        if identity.is_some_and(|identity| identity != &operation.identity()) {
            return 0;
        }
        let newly_requested = match operation.request_cancel_once() {
            Ok(newly_requested) => newly_requested,
            Err(_) => {
                if self.operations.diagnostic_faulted() {
                    self.mark_faulted();
                }
                return 0;
            }
        };
        if !newly_requested {
            return 0;
        }
        cancellation.notify();
        let phase = operation
            .snapshot()
            .ok()
            .flatten()
            .map(|snapshot| snapshot.phase);
        if matches!(phase, Some(operation_lifecycle::OperationPhase::Running))
            && cancellation.cancel_backend(&backend, request_id).is_err()
        {
            self.mark_faulted();
        }
        1
    }

    fn release_route(
        &self,
        request_id: &SpeechRequestId,
        identity: &operation_lifecycle::OperationIdentity,
    ) -> Result<(), SpeechHostError> {
        let mut state = self.state.lock().map_err(|_| {
            self.mark_faulted();
            SpeechHostError::StateUnavailable
        })?;
        if !state
            .routes
            .get(request_id)
            .is_some_and(|route| &route.identity == identity)
        {
            self.mark_faulted();
            return Err(SpeechHostError::StateUnavailable);
        }
        state.routes.remove(request_id);
        self.changed.notify_waiters();
        Ok(())
    }
}

impl SpeechCancellation for HostCancellation {
    fn cancel(&self, request_id: &SpeechRequestId) -> usize {
        if request_id != &self.request_id {
            return 0;
        }
        let Some(lifecycle) = self.lifecycle.upgrade() else {
            return 0;
        };
        lifecycle.cancel(request_id, Some(&self.identity))
    }
}

impl SetupGuard {
    fn new(
        lifecycle: Arc<HostLifecycle>,
        request_id: SpeechRequestId,
        operation: operation_lifecycle::OperationLease,
    ) -> Self {
        Self {
            lifecycle,
            request_id,
            operation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn cancel_and_release(&mut self) -> Result<(), SpeechHostError> {
        self.operation
            .cancel_setup_and_release()
            .map_err(map_registry_error)?;
        self.lifecycle
            .release_route(&self.request_id, &self.operation.identity())?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for SetupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let released = self.operation.fail_setup_and_release();
        let route_released = self
            .lifecycle
            .release_route(&self.request_id, &self.operation.identity());
        if released.is_err() || route_released.is_err() {
            self.lifecycle.mark_faulted();
        }
    }
}

impl ExecutorOperation {
    fn request_cancel(&mut self) -> Result<(), operation_lifecycle::RegistryError> {
        self.operation.request_cancel_once()?;
        self.cancellation.notify();
        self.cancellation
            .cancel_backend(&self.backend, &self.request_id)
            .map_err(|()| {
                self.lifecycle.mark_faulted();
                operation_lifecycle::RegistryError::StateUnavailable
            })
    }

    fn cancel_and_finish(
        &mut self,
        terminal: operation_lifecycle::TerminalClass,
    ) -> Result<(), operation_lifecycle::RegistryError> {
        self.request_cancel()?;
        self.finish(terminal)
    }

    fn finish(
        &mut self,
        terminal: operation_lifecycle::TerminalClass,
    ) -> Result<(), operation_lifecycle::RegistryError> {
        self.finish_inner(terminal, false).map(|_| ())
    }

    fn finish_backend(
        &mut self,
        terminal: operation_lifecycle::TerminalClass,
    ) -> Result<operation_lifecycle::OperationSnapshot, operation_lifecycle::RegistryError> {
        self.finish_inner(terminal, true)
    }

    fn terminalize(
        &mut self,
        terminal: operation_lifecycle::TerminalClass,
        cancellation_wins: bool,
    ) -> Result<operation_lifecycle::OperationSnapshot, operation_lifecycle::RegistryError> {
        let attempt = self
            .attempt
            .as_ref()
            .ok_or(operation_lifecycle::RegistryError::Stale)?;
        let snapshot = self
            .operation
            .finish_attempt_terminal(attempt, terminal, cancellation_wins)
            .inspect_err(|_| self.lifecycle.mark_faulted())?;
        self.attempt.take();
        Ok(snapshot)
    }

    fn release_terminal(&mut self) -> Result<(), operation_lifecycle::RegistryError> {
        self.operation
            .release()
            .inspect_err(|_| self.lifecycle.mark_faulted())?;
        self.lifecycle
            .release_route(&self.request_id, &self.operation.identity())
            .map_err(|_| operation_lifecycle::RegistryError::StateUnavailable)?;
        self.finished = true;
        Ok(())
    }

    fn finish_inner(
        &mut self,
        terminal: operation_lifecycle::TerminalClass,
        cancellation_wins: bool,
    ) -> Result<operation_lifecycle::OperationSnapshot, operation_lifecycle::RegistryError> {
        if self.finished {
            return Err(operation_lifecycle::RegistryError::Stale);
        }
        let attempt = self
            .attempt
            .as_ref()
            .ok_or(operation_lifecycle::RegistryError::Stale)?;
        let finished = if cancellation_wins {
            self.operation
                .finish_backend_attempt_and_release(attempt, terminal)
        } else {
            self.operation.finish_attempt_and_release(attempt, terminal)
        }
        .inspect_err(|_| self.lifecycle.mark_faulted())?;
        self.attempt.take();
        let identity = self.operation.identity();
        self.lifecycle
            .release_route(&self.request_id, &identity)
            .map_err(|_| operation_lifecycle::RegistryError::StateUnavailable)?;
        self.finished = true;
        Ok(finished)
    }
}

impl Drop for ExecutorOperation {
    fn drop(&mut self) {
        if !self.finished {
            let result = if self.attempt.is_none() {
                self.release_terminal()
            } else {
                self.cancel_and_finish(operation_lifecycle::TerminalClass::Cancelled)
            };
            if result.is_err() {
                self.lifecycle.mark_faulted();
            }
        }
    }
}

fn terminal_for_result<T>(result: &Result<T, SpeechError>) -> operation_lifecycle::TerminalClass {
    match result {
        Ok(_) => operation_lifecycle::TerminalClass::Completed,
        Err(error) => terminal_for_error(error),
    }
}

const fn terminal_for_error(error: &SpeechError) -> operation_lifecycle::TerminalClass {
    if matches!(error.class, SpeechErrorClass::Cancelled) {
        operation_lifecycle::TerminalClass::Cancelled
    } else {
        operation_lifecycle::TerminalClass::Failed
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn pin_route(route: &mut SpeechRouteSelector, plan: &SpeechRoutePlan) {
    *route = SpeechRouteSelector::ExactBackend {
        backend_id: plan.selected.route.backend_id.clone(),
        model_id: plan.selected.route.model_id.clone(),
        voice_id: plan.selected.route.voice_id.clone(),
    };
}

#[must_use]
pub fn service_error(request_id: &SpeechRequestId, error: SpeechHostError) -> SpeechError {
    SpeechError {
        code: "speech_gateway_failed".to_string(),
        class: SpeechErrorClass::Unavailable,
        retryable: false,
        request_id: request_id.clone(),
        backend_id: None,
        safe_detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use speech_native_types::{
        AcceptedAudio, AlignmentGranularity, AudioInput, AudioOutputFormat, AudioOutputKind,
        CapabilityAvailability, CapabilityEvidence, DiarizationPolicy, EvidenceKind,
        EvidenceOutcome, NetworkBehavior, PcmFormat, PcmSampleFormat, SpeechBackendKind,
        SpeechBackendReadiness, SpeechCancellation, SpeechCapability, SpeechCapabilityLimits,
        SpeechDeadlinePolicy, SpeechOperationCapability, SpeechRequestContext, SpeechResolvedRoute,
        SpeechRoutingPolicy, SpeechUsage, SynthesisCapabilities, SynthesisEvent, SynthesisInput,
        SynthesisOutput, SynthesisResponse, TimestampGranularity, TranscriptionCapabilities,
        TranscriptionInput, TranscriptionResponse, TranscriptionTask, UsageProvenance,
        VoiceDescriptor, VoiceQuality, VoiceSelector,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{mpsc, oneshot};

    #[derive(Clone)]
    struct ManualClock {
        state: Arc<ManualClockState>,
    }

    struct ManualClockState {
        now: Mutex<Instant>,
        changed: Notify,
    }

    impl ManualClock {
        fn new() -> Self {
            Self {
                state: Arc::new(ManualClockState {
                    now: Mutex::new(Instant::now()),
                    changed: Notify::new(),
                }),
            }
        }

        fn advance(&self, duration: Duration) {
            let mut now = self.state.now.lock().expect("lock manual clock");
            *now = now
                .checked_add(duration)
                .expect("fixture clock remains in monotonic range");
            drop(now);
            self.state.changed.notify_waiters();
        }
    }

    impl DeadlineClock for ManualClock {
        fn now(&self) -> Instant {
            *self.state.now.lock().expect("lock manual clock")
        }

        fn sleep_until(&self, deadline: Instant) -> DeadlineSleep {
            let state = Arc::clone(&self.state);
            Box::pin(async move {
                loop {
                    let changed = state.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if *state.now.lock().expect("lock manual clock") >= deadline {
                        return;
                    }
                    changed.await;
                }
            })
        }
    }

    fn host_with_manual_clock() -> (SpeechHost, ManualClock) {
        let clock = ManualClock::new();
        let host = SpeechHost::with_clock(PlatformTarget::current(), Arc::new(clock.clone()));
        (host, clock)
    }

    #[derive(Default)]
    struct FixtureCancellation;

    impl SpeechCancellation for FixtureCancellation {
        fn cancel(&self, _request_id: &SpeechRequestId) -> usize {
            0
        }
    }

    struct FixtureBackend {
        descriptor: SpeechBackendDescriptor,
        calls: AtomicUsize,
        shutdown_calls: AtomicUsize,
        fail_shutdown: bool,
        panic_shutdown: bool,
    }

    struct DurationProbeBackend {
        descriptor: SpeechBackendDescriptor,
        calls: AtomicUsize,
    }

    struct DeferredCancellation {
        calls: Arc<AtomicUsize>,
    }

    impl SpeechCancellation for DeferredCancellation {
        fn cancel(&self, _request_id: &SpeechRequestId) -> usize {
            self.calls.fetch_add(1, Ordering::AcqRel);
            1
        }
    }

    struct DeferredBackend {
        descriptor: SpeechBackendDescriptor,
        finals: Mutex<BTreeMap<SpeechRequestId, oneshot::Sender<SynthesisResponse>>>,
        events: Mutex<BTreeMap<SpeechRequestId, mpsc::Sender<SynthesisEvent>>>,
        cancel_calls: Arc<AtomicUsize>,
        shutdown_calls: AtomicUsize,
        changed: Notify,
    }

    struct DeferredTranscriptionBackend {
        descriptor: SpeechBackendDescriptor,
        finals: Mutex<BTreeMap<SpeechRequestId, oneshot::Sender<TranscriptionResponse>>>,
        calls: AtomicUsize,
        cancel_calls: Arc<AtomicUsize>,
        changed: Notify,
    }

    struct MutableDescriptorBackend {
        descriptor: Mutex<SpeechBackendDescriptor>,
    }

    struct DeferredDispatchBackend {
        descriptor: SpeechBackendDescriptor,
        dispatches: Mutex<BTreeMap<SpeechRequestId, oneshot::Sender<()>>>,
        cancel_calls: Arc<AtomicUsize>,
        shutdown_calls: AtomicUsize,
        changed: Notify,
        shutdown_changed: Notify,
    }

    impl DeferredBackend {
        fn emit(&self, request_id: &SpeechRequestId, event: SynthesisEvent) {
            self.events
                .lock()
                .expect("lock deferred events")
                .get(request_id)
                .expect("deferred request must own an event sender")
                .try_send(event)
                .expect("deferred event channel has bounded fixture capacity");
        }

        fn complete(&self, request_id: &SpeechRequestId) {
            let sender = self
                .finals
                .lock()
                .expect("lock deferred finals")
                .remove(request_id)
                .expect("deferred request must exist");
            self.events
                .lock()
                .expect("lock deferred events")
                .remove(request_id);
            let _ = sender.send(deferred_response(request_id, &self.descriptor.id));
            self.changed.notify_waiters();
        }

        async fn wait_until_active(&self, request_id: &SpeechRequestId) {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self
                    .finals
                    .lock()
                    .expect("lock deferred finals")
                    .contains_key(request_id)
                {
                    return;
                }
                changed.await;
            }
        }
    }

    impl DeferredTranscriptionBackend {
        fn complete(&self, request_id: &SpeechRequestId) {
            let sender = self
                .finals
                .lock()
                .expect("lock deferred transcription finals")
                .remove(request_id)
                .expect("deferred transcription request must exist");
            let _ = sender.send(deferred_transcription_response(
                request_id,
                &self.descriptor.id,
            ));
            self.changed.notify_waiters();
        }

        async fn wait_until_active(&self, request_id: &SpeechRequestId) {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self
                    .finals
                    .lock()
                    .expect("lock deferred transcription finals")
                    .contains_key(request_id)
                {
                    return;
                }
                changed.await;
            }
        }
    }

    impl DeferredDispatchBackend {
        fn release(&self, request_id: &SpeechRequestId) {
            let sender = self
                .dispatches
                .lock()
                .expect("lock deferred dispatches")
                .remove(request_id)
                .expect("deferred dispatch must exist");
            let _ = sender.send(());
            self.changed.notify_waiters();
        }

        async fn wait_until_dispatching(&self, request_id: &SpeechRequestId) {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self
                    .dispatches
                    .lock()
                    .expect("lock deferred dispatches")
                    .contains_key(request_id)
                {
                    return;
                }
                changed.await;
            }
        }

        async fn wait_until_shutdown(&self) {
            loop {
                let changed = self.shutdown_changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.shutdown_calls.load(Ordering::Acquire) != 0 {
                    return;
                }
                changed.await;
            }
        }
    }

    #[async_trait]
    impl SpeechBackend for DeferredBackend {
        fn descriptor(&self) -> SpeechBackendDescriptor {
            self.descriptor.clone()
        }

        fn readiness(&self) -> SpeechBackendReadiness {
            self.descriptor.readiness.clone()
        }

        async fn transcribe(
            &self,
            request: TranscriptionRequest,
        ) -> Result<TranscriptionTicket, SpeechError> {
            Err(SpeechError::unavailable(
                &request.context.request_id,
                "fixture_transcription_unsupported",
                "fixture supports only synthesis",
            ))
        }

        async fn synthesize(
            &self,
            request: SynthesisRequest,
        ) -> Result<SynthesisTicket, SpeechError> {
            let request_id = request.context.request_id;
            let (event_sender, event_receiver) = mpsc::channel(8);
            let (backend_final_sender, backend_final_receiver) = oneshot::channel();
            let (release_sender, release_receiver) = oneshot::channel();
            self.finals
                .lock()
                .map_err(|_| {
                    SpeechError::unavailable(
                        &request_id,
                        "fixture_state_unavailable",
                        "fixture state is unavailable",
                    )
                })?
                .insert(request_id.clone(), release_sender);
            self.events
                .lock()
                .map_err(|_| {
                    SpeechError::unavailable(
                        &request_id,
                        "fixture_state_unavailable",
                        "fixture state is unavailable",
                    )
                })?
                .insert(request_id.clone(), event_sender);
            self.changed.notify_waiters();
            let response_id = request_id.clone();
            let backend_id = self.descriptor.id.clone();
            tokio::spawn(async move {
                let result = release_receiver.await.map_or_else(
                    |_| {
                        Err(SpeechError::unavailable(
                            &response_id,
                            "fixture_release_closed",
                            "fixture release closed",
                        ))
                    },
                    Ok,
                );
                let _ = backend_final_sender.send(result);
                drop(backend_id);
            });
            Ok(SynthesisTicket::new(
                request_id,
                event_receiver,
                backend_final_receiver,
                Arc::new(DeferredCancellation {
                    calls: Arc::clone(&self.cancel_calls),
                }),
            ))
        }

        fn cancel(&self, request_id: &SpeechRequestId) -> usize {
            if self
                .finals
                .lock()
                .is_ok_and(|finals| finals.contains_key(request_id))
            {
                self.cancel_calls.fetch_add(1, Ordering::AcqRel);
                1
            } else {
                0
            }
        }

        async fn shutdown(&self) -> Result<(), SpeechError> {
            self.shutdown_calls.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    #[async_trait]
    impl SpeechBackend for DeferredTranscriptionBackend {
        fn descriptor(&self) -> SpeechBackendDescriptor {
            self.descriptor.clone()
        }

        fn readiness(&self) -> SpeechBackendReadiness {
            self.descriptor.readiness.clone()
        }

        async fn transcribe(
            &self,
            request: TranscriptionRequest,
        ) -> Result<TranscriptionTicket, SpeechError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let request_id = request.context.request_id;
            let (event_sender, event_receiver) = mpsc::channel(2);
            drop(event_sender);
            let (backend_final_sender, backend_final_receiver) = oneshot::channel();
            let (release_sender, release_receiver) = oneshot::channel();
            self.finals
                .lock()
                .map_err(|_| {
                    SpeechError::unavailable(
                        &request_id,
                        "fixture_state_unavailable",
                        "fixture state is unavailable",
                    )
                })?
                .insert(request_id.clone(), release_sender);
            self.changed.notify_waiters();
            let response_id = request_id.clone();
            tokio::spawn(async move {
                let result = release_receiver.await.map_or_else(
                    |_| {
                        Err(SpeechError::unavailable(
                            &response_id,
                            "fixture_release_closed",
                            "fixture release closed",
                        ))
                    },
                    Ok,
                );
                let _ = backend_final_sender.send(result);
            });
            Ok(TranscriptionTicket::new(
                request_id,
                event_receiver,
                backend_final_receiver,
                Arc::new(DeferredCancellation {
                    calls: Arc::clone(&self.cancel_calls),
                }),
                None,
            ))
        }

        async fn synthesize(
            &self,
            request: SynthesisRequest,
        ) -> Result<SynthesisTicket, SpeechError> {
            Err(SpeechError::unavailable(
                &request.context.request_id,
                "fixture_synthesis_unsupported",
                "fixture supports only transcription",
            ))
        }

        fn cancel(&self, request_id: &SpeechRequestId) -> usize {
            if self
                .finals
                .lock()
                .is_ok_and(|finals| finals.contains_key(request_id))
            {
                self.cancel_calls.fetch_add(1, Ordering::AcqRel);
                1
            } else {
                0
            }
        }

        async fn shutdown(&self) -> Result<(), SpeechError> {
            Ok(())
        }
    }

    #[async_trait]
    impl SpeechBackend for MutableDescriptorBackend {
        fn descriptor(&self) -> SpeechBackendDescriptor {
            self.descriptor
                .lock()
                .expect("lock mutable descriptor")
                .clone()
        }

        fn readiness(&self) -> SpeechBackendReadiness {
            self.descriptor().readiness
        }

        async fn transcribe(
            &self,
            request: TranscriptionRequest,
        ) -> Result<TranscriptionTicket, SpeechError> {
            Err(SpeechError::unavailable(
                &request.context.request_id,
                "fixture_transcription_unsupported",
                "mutable descriptor fixture does not execute speech",
            ))
        }

        async fn synthesize(
            &self,
            request: SynthesisRequest,
        ) -> Result<SynthesisTicket, SpeechError> {
            Err(SpeechError::unavailable(
                &request.context.request_id,
                "fixture_synthesis_unsupported",
                "mutable descriptor fixture does not execute speech",
            ))
        }

        fn cancel(&self, _request_id: &SpeechRequestId) -> usize {
            0
        }

        async fn shutdown(&self) -> Result<(), SpeechError> {
            Ok(())
        }
    }

    #[async_trait]
    impl SpeechBackend for DeferredDispatchBackend {
        fn descriptor(&self) -> SpeechBackendDescriptor {
            self.descriptor.clone()
        }

        fn readiness(&self) -> SpeechBackendReadiness {
            self.descriptor.readiness.clone()
        }

        async fn transcribe(
            &self,
            request: TranscriptionRequest,
        ) -> Result<TranscriptionTicket, SpeechError> {
            Err(SpeechError::unavailable(
                &request.context.request_id,
                "fixture_transcription_unsupported",
                "fixture supports only synthesis",
            ))
        }

        async fn synthesize(
            &self,
            request: SynthesisRequest,
        ) -> Result<SynthesisTicket, SpeechError> {
            let request_id = request.context.request_id;
            let (release_sender, release_receiver) = oneshot::channel();
            self.dispatches
                .lock()
                .map_err(|_| {
                    SpeechError::unavailable(
                        &request_id,
                        "fixture_state_unavailable",
                        "fixture state is unavailable",
                    )
                })?
                .insert(request_id.clone(), release_sender);
            self.changed.notify_waiters();
            release_receiver.await.map_err(|_| {
                SpeechError::unavailable(
                    &request_id,
                    "fixture_dispatch_release_closed",
                    "fixture dispatch release closed",
                )
            })?;
            let response = deferred_response(&request_id, &self.descriptor.id);
            let (event_sender, event_receiver) = mpsc::channel(1);
            drop(event_sender);
            let (final_sender, final_receiver) = oneshot::channel();
            let _ = final_sender.send(Ok(response));
            Ok(SynthesisTicket::new(
                request_id,
                event_receiver,
                final_receiver,
                Arc::new(DeferredCancellation {
                    calls: Arc::clone(&self.cancel_calls),
                }),
            ))
        }

        fn cancel(&self, request_id: &SpeechRequestId) -> usize {
            if self
                .dispatches
                .lock()
                .is_ok_and(|dispatches| dispatches.contains_key(request_id))
            {
                self.cancel_calls.fetch_add(1, Ordering::AcqRel);
                1
            } else {
                0
            }
        }

        async fn shutdown(&self) -> Result<(), SpeechError> {
            self.shutdown_calls.fetch_add(1, Ordering::AcqRel);
            self.shutdown_changed.notify_waiters();
            Ok(())
        }
    }

    #[async_trait]
    impl SpeechBackend for DurationProbeBackend {
        fn descriptor(&self) -> SpeechBackendDescriptor {
            self.descriptor.clone()
        }

        fn readiness(&self) -> SpeechBackendReadiness {
            self.descriptor.readiness.clone()
        }

        async fn transcribe(
            &self,
            request: TranscriptionRequest,
        ) -> Result<TranscriptionTicket, SpeechError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Err(SpeechError::unavailable(
                &request.context.request_id,
                "duration_probe_reached",
                "fixture proves the host dispatched audio above the advertised duration limit",
            ))
        }

        async fn synthesize(
            &self,
            request: SynthesisRequest,
        ) -> Result<SynthesisTicket, SpeechError> {
            Err(SpeechError::unavailable(
                &request.context.request_id,
                "fixture_synthesis_unsupported",
                "fixture supports only transcription",
            ))
        }

        fn cancel(&self, _request_id: &SpeechRequestId) -> usize {
            0
        }

        async fn shutdown(&self) -> Result<(), SpeechError> {
            Ok(())
        }
    }

    #[async_trait]
    impl SpeechBackend for FixtureBackend {
        fn descriptor(&self) -> SpeechBackendDescriptor {
            self.descriptor.clone()
        }

        fn readiness(&self) -> SpeechBackendReadiness {
            self.descriptor.readiness.clone()
        }

        async fn transcribe(
            &self,
            request: TranscriptionRequest,
        ) -> Result<TranscriptionTicket, SpeechError> {
            Err(SpeechError::unavailable(
                &request.context.request_id,
                "fixture_transcription_unsupported",
                "fixture supports only synthesis",
            ))
        }

        async fn synthesize(
            &self,
            request: SynthesisRequest,
        ) -> Result<SynthesisTicket, SpeechError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let request_id = request.context.request_id.clone();
            let SpeechRouteSelector::ExactBackend {
                backend_id,
                model_id,
                voice_id,
            } = request.context.route
            else {
                return Err(SpeechError::invalid_request(
                    &request_id,
                    "fixture_route_unpinned",
                    "gateway did not pin the selected route",
                ));
            };
            let route = SpeechResolvedRoute {
                backend_id,
                model_id,
                voice_id,
                backend_kind: self.descriptor.kind,
                network: NetworkBehavior::Never,
            };
            let response = SynthesisResponse {
                request_id: request_id.clone(),
                route: route.clone(),
                output: SynthesisOutput::Complete {
                    audio: b"RIFFfixtureWAVE".to_vec(),
                    format: AudioOutputFormat::Wav,
                },
                duration_ms: Some(1),
                alignments: Vec::new(),
                usage: SpeechUsage {
                    provenance: UsageProvenance::Exact,
                    real_local_inference: true,
                    ..SpeechUsage::default()
                },
            };
            let (event_sender, event_receiver) = mpsc::channel(4);
            let (final_sender, final_receiver) = oneshot::channel();
            event_sender
                .try_send(SynthesisEvent::Started {
                    request_id: request_id.clone(),
                    route,
                })
                .map_err(|_| {
                    SpeechError::unavailable(
                        &request_id,
                        "fixture_event_failed",
                        "fixture event channel failed",
                    )
                })?;
            event_sender
                .try_send(SynthesisEvent::Completed {
                    request_id: request_id.clone(),
                    response: response.clone(),
                })
                .map_err(|_| {
                    SpeechError::unavailable(
                        &request_id,
                        "fixture_event_failed",
                        "fixture event channel failed",
                    )
                })?;
            drop(event_sender);
            let _ = final_sender.send(Ok(response));
            Ok(SynthesisTicket::new(
                request_id,
                event_receiver,
                final_receiver,
                Arc::new(FixtureCancellation),
            ))
        }

        fn cancel(&self, _request_id: &SpeechRequestId) -> usize {
            0
        }

        async fn shutdown(&self) -> Result<(), SpeechError> {
            self.shutdown_calls.fetch_add(1, Ordering::AcqRel);
            assert!(!self.panic_shutdown, "fixture backend shutdown panic");
            if self.fail_shutdown {
                Err(SpeechError::unavailable(
                    &SpeechRequestId(format!("{}.shutdown", self.descriptor.id)),
                    "fixture_shutdown_failed",
                    "fixture shutdown failed",
                ))
            } else {
                Ok(())
            }
        }
    }

    fn fixture_backend(id: &str) -> Arc<FixtureBackend> {
        Arc::new(FixtureBackend {
            descriptor: SpeechBackendDescriptor {
                id: id.to_string(),
                display_name: id.to_string(),
                kind: SpeechBackendKind::EmbeddedModel,
                readiness: SpeechBackendReadiness::Ready,
                capabilities: vec![SpeechCapability {
                    id: format!("{id}.synthesis"),
                    backend_id: id.to_string(),
                    model_id: Some("fixture-voice-model".to_string()),
                    operation: SpeechOperationCapability::Synthesis(SynthesisCapabilities {
                        returned_audio: vec![AudioOutputKind::Wav],
                        voice_selection: true,
                        ..SynthesisCapabilities::default()
                    }),
                    availability: CapabilityAvailability::Available,
                    network: NetworkBehavior::Never,
                    languages: vec!["en-US".to_string()],
                    limits: SpeechCapabilityLimits::default(),
                    evidence: vec![CapabilityEvidence {
                        source_id: "fixture".to_string(),
                        source_version: Some("1".to_string()),
                        kind: EvidenceKind::RuntimeApi,
                        outcome: EvidenceOutcome::Confirmed,
                        observed_at_unix_ms: 1,
                        detail: "fixture backend".to_string(),
                    }],
                }],
                models: Vec::new(),
                voices: vec![VoiceDescriptor {
                    id: "fixture-voice".to_string(),
                    name: "Fixture".to_string(),
                    language: "en-US".to_string(),
                    gender: None,
                    quality: Some(VoiceQuality::Normal),
                    expected_latency: None,
                    network: NetworkBehavior::Never,
                    installed: true,
                }],
            },
            calls: AtomicUsize::new(0),
            shutdown_calls: AtomicUsize::new(0),
            fail_shutdown: false,
            panic_shutdown: false,
        })
    }

    fn failing_fixture_backend(id: &str) -> Arc<FixtureBackend> {
        let mut backend = fixture_backend(id);
        Arc::get_mut(&mut backend)
            .expect("new fixture backend must be uniquely owned")
            .fail_shutdown = true;
        backend
    }

    fn panicking_fixture_backend(id: &str) -> Arc<FixtureBackend> {
        let mut backend = fixture_backend(id);
        Arc::get_mut(&mut backend)
            .expect("new fixture has one owner")
            .panic_shutdown = true;
        backend
    }

    fn deferred_backend(id: &str) -> Arc<DeferredBackend> {
        deferred_backend_with_capacity(id, None)
    }

    fn deferred_streaming_backend(id: &str) -> Arc<DeferredBackend> {
        let mut backend = deferred_backend(id);
        {
            let backend = Arc::get_mut(&mut backend).expect("new deferred backend has one owner");
            let SpeechOperationCapability::Synthesis(capability) =
                &mut backend.descriptor.capabilities[0].operation
            else {
                panic!("deferred backend must expose synthesis");
            };
            capability.streaming_audio = true;
        }
        backend
    }

    fn deferred_backend_with_capacity(
        id: &str,
        max_concurrent_requests: Option<u32>,
    ) -> Arc<DeferredBackend> {
        let mut descriptor = fixture_backend(id).descriptor();
        descriptor.capabilities[0].limits.max_concurrent_requests = max_concurrent_requests;
        Arc::new(DeferredBackend {
            descriptor,
            finals: Mutex::new(BTreeMap::new()),
            events: Mutex::new(BTreeMap::new()),
            cancel_calls: Arc::new(AtomicUsize::new(0)),
            shutdown_calls: AtomicUsize::new(0),
            changed: Notify::new(),
        })
    }

    fn deferred_transcription_backend(id: &str) -> Arc<DeferredTranscriptionBackend> {
        let mut descriptor = duration_probe_backend(id, 10_000).descriptor.clone();
        descriptor.capabilities[0].limits.max_concurrent_requests = Some(1);
        Arc::new(DeferredTranscriptionBackend {
            descriptor,
            finals: Mutex::new(BTreeMap::new()),
            calls: AtomicUsize::new(0),
            cancel_calls: Arc::new(AtomicUsize::new(0)),
            changed: Notify::new(),
        })
    }

    fn mutable_descriptor_backend(id: &str) -> Arc<MutableDescriptorBackend> {
        let mut descriptor = fixture_backend(id).descriptor();
        descriptor.capabilities[0].limits.max_concurrent_requests = Some(1);
        Arc::new(MutableDescriptorBackend {
            descriptor: Mutex::new(descriptor),
        })
    }

    fn deferred_dispatch_backend(id: &str) -> Arc<DeferredDispatchBackend> {
        let mut descriptor = fixture_backend(id).descriptor();
        descriptor.capabilities[0].limits.max_concurrent_requests = Some(1);
        Arc::new(DeferredDispatchBackend {
            descriptor,
            dispatches: Mutex::new(BTreeMap::new()),
            cancel_calls: Arc::new(AtomicUsize::new(0)),
            shutdown_calls: AtomicUsize::new(0),
            changed: Notify::new(),
            shutdown_changed: Notify::new(),
        })
    }

    fn duration_probe_backend(id: &str, max_audio_ms: u64) -> Arc<DurationProbeBackend> {
        Arc::new(DurationProbeBackend {
            descriptor: SpeechBackendDescriptor {
                id: id.to_owned(),
                display_name: id.to_owned(),
                kind: SpeechBackendKind::EmbeddedModel,
                readiness: SpeechBackendReadiness::Ready,
                capabilities: vec![SpeechCapability {
                    id: format!("{id}.transcription"),
                    backend_id: id.to_owned(),
                    model_id: Some("fixture-transcription-model".to_owned()),
                    operation: SpeechOperationCapability::Transcription(
                        TranscriptionCapabilities {
                            accepted_audio: vec![AcceptedAudio::Pcm, AcceptedAudio::Wav],
                            ..TranscriptionCapabilities::default()
                        },
                    ),
                    availability: CapabilityAvailability::Available,
                    network: NetworkBehavior::Never,
                    languages: vec!["en".to_owned()],
                    limits: SpeechCapabilityLimits {
                        max_audio_ms: Some(max_audio_ms),
                        ..SpeechCapabilityLimits::default()
                    },
                    evidence: vec![CapabilityEvidence {
                        source_id: "fixture".to_owned(),
                        source_version: Some("1".to_owned()),
                        kind: EvidenceKind::RuntimeApi,
                        outcome: EvidenceOutcome::Confirmed,
                        observed_at_unix_ms: 1,
                        detail: "duration preflight fixture".to_owned(),
                    }],
                }],
                models: Vec::new(),
                voices: Vec::new(),
            },
            calls: AtomicUsize::new(0),
        })
    }

    fn deferred_response(request_id: &SpeechRequestId, backend_id: &str) -> SynthesisResponse {
        SynthesisResponse {
            request_id: request_id.clone(),
            route: SpeechResolvedRoute {
                backend_id: backend_id.to_string(),
                model_id: Some("fixture-voice-model".to_string()),
                voice_id: Some("fixture-voice".to_string()),
                backend_kind: SpeechBackendKind::EmbeddedModel,
                network: NetworkBehavior::Never,
            },
            output: SynthesisOutput::Complete {
                audio: b"RIFFfixtureWAVE".to_vec(),
                format: AudioOutputFormat::Wav,
            },
            duration_ms: Some(1),
            alignments: Vec::new(),
            usage: SpeechUsage::default(),
        }
    }

    fn deferred_transcription_response(
        request_id: &SpeechRequestId,
        backend_id: &str,
    ) -> TranscriptionResponse {
        TranscriptionResponse {
            request_id: request_id.clone(),
            route: SpeechResolvedRoute {
                backend_id: backend_id.to_owned(),
                model_id: Some("fixture-transcription-model".to_owned()),
                voice_id: None,
                backend_kind: SpeechBackendKind::EmbeddedModel,
                network: NetworkBehavior::Never,
            },
            text: "fixture transcript".to_owned(),
            language: Some("en".to_owned()),
            segments: Vec::new(),
            usage: SpeechUsage::default(),
        }
    }

    fn request() -> SynthesisRequest {
        SynthesisRequest {
            context: SpeechRequestContext {
                request_id: SpeechRequestId("speech-service-test".to_string()),
                client_id: "test".to_string(),
                route: SpeechRouteSelector::Auto,
                routing: SpeechRoutingPolicy::default(),
                deadline: SpeechDeadlinePolicy::default(),
            },
            input: SynthesisInput::Text {
                text: "hello".to_string(),
            },
            voice: VoiceSelector::Auto,
            language: Some("en-US".to_string()),
            rate: 1.0,
            pitch: 1.0,
            volume: 1.0,
            output: AudioOutputFormat::Wav,
            alignment: AlignmentGranularity::None,
            stream: false,
        }
    }

    fn exact_request(request_id: &str, backend_id: &str) -> SynthesisRequest {
        let mut request = request();
        request.context.request_id = SpeechRequestId(request_id.to_string());
        request.context.route = SpeechRouteSelector::ExactBackend {
            backend_id: backend_id.to_string(),
            model_id: Some("fixture-voice-model".to_string()),
            voice_id: Some("fixture-voice".to_string()),
        };
        request
    }

    fn exact_pcm_transcription_request(request_id: &str, backend_id: &str) -> TranscriptionRequest {
        TranscriptionRequest {
            context: SpeechRequestContext {
                request_id: SpeechRequestId(request_id.to_owned()),
                client_id: "test".to_owned(),
                route: SpeechRouteSelector::ExactBackend {
                    backend_id: backend_id.to_owned(),
                    model_id: Some("fixture-transcription-model".to_owned()),
                    voice_id: None,
                },
                routing: SpeechRoutingPolicy::default(),
                deadline: SpeechDeadlinePolicy::default(),
            },
            input: TranscriptionInput::Complete {
                audio: AudioInput::Pcm {
                    format: PcmFormat {
                        sample_rate_hz: 16_000,
                        channels: 1,
                        sample_format: PcmSampleFormat::I16Le,
                        interleaved: true,
                    },
                    data: vec![0; 64],
                },
            },
            language: Some("en".to_owned()),
            task: TranscriptionTask::Transcribe,
            timestamps: TimestampGranularity::None,
            diarization: DiarizationPolicy::Disabled,
            partial_results: false,
            punctuation: true,
            hotwords: Vec::new(),
        }
    }

    fn exact_wav_transcription_request(request_id: &str, backend_id: &str) -> TranscriptionRequest {
        let wav = classic_wav(1, 1, 16_000, 16, vec![0_u8; 64]);
        let mut request = exact_pcm_transcription_request(request_id, backend_id);
        request.input = TranscriptionInput::Complete {
            audio: AudioInput::Encoded {
                format: EncodedAudioFormat::Wav,
                data: wav,
            },
        };
        request
    }

    fn wav_request(request_id: &str, backend_id: &str, wav: Vec<u8>) -> TranscriptionRequest {
        let mut request = exact_pcm_transcription_request(request_id, backend_id);
        request.input = TranscriptionInput::Complete {
            audio: AudioInput::Encoded {
                format: EncodedAudioFormat::Wav,
                data: wav,
            },
        };
        request
    }

    fn classic_wav(
        encoding: u16,
        channels: u16,
        sample_rate_hz: u32,
        bits_per_sample: u16,
        data: Vec<u8>,
    ) -> Vec<u8> {
        let bytes_per_sample = bits_per_sample.div_ceil(8);
        let block_align = channels
            .checked_mul(bytes_per_sample)
            .expect("fixture block alignment fits u16");
        let byte_rate = sample_rate_hz
            .checked_mul(u32::from(block_align))
            .expect("fixture byte rate fits u32");
        let mut format = Vec::with_capacity(16);
        format.extend_from_slice(&encoding.to_le_bytes());
        format.extend_from_slice(&channels.to_le_bytes());
        format.extend_from_slice(&sample_rate_hz.to_le_bytes());
        format.extend_from_slice(&byte_rate.to_le_bytes());
        format.extend_from_slice(&block_align.to_le_bytes());
        format.extend_from_slice(&bits_per_sample.to_le_bytes());
        wav_from_chunks(vec![(*b"fmt ", format), (*b"data", data)])
    }

    fn extensible_wav(
        subformat_encoding: u16,
        channels: u16,
        sample_rate_hz: u32,
        bits_per_sample: u16,
        valid_bits_per_sample: u16,
        data: Vec<u8>,
    ) -> Vec<u8> {
        let bytes_per_sample = bits_per_sample.div_ceil(8);
        let block_align = channels
            .checked_mul(bytes_per_sample)
            .expect("fixture block alignment fits u16");
        let byte_rate = sample_rate_hz
            .checked_mul(u32::from(block_align))
            .expect("fixture byte rate fits u32");
        let mut format = Vec::with_capacity(40);
        format.extend_from_slice(&0xfffe_u16.to_le_bytes());
        format.extend_from_slice(&channels.to_le_bytes());
        format.extend_from_slice(&sample_rate_hz.to_le_bytes());
        format.extend_from_slice(&byte_rate.to_le_bytes());
        format.extend_from_slice(&block_align.to_le_bytes());
        format.extend_from_slice(&bits_per_sample.to_le_bytes());
        format.extend_from_slice(&22_u16.to_le_bytes());
        format.extend_from_slice(&valid_bits_per_sample.to_le_bytes());
        format.extend_from_slice(&0_u32.to_le_bytes());
        format.extend_from_slice(&subformat_encoding.to_le_bytes());
        format.extend_from_slice(&0_u16.to_le_bytes());
        format.extend_from_slice(&0_u16.to_le_bytes());
        format.extend_from_slice(&0x0010_u16.to_le_bytes());
        format.extend_from_slice(&[0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71]);
        wav_from_chunks(vec![(*b"fmt ", format), (*b"data", data)])
    }

    fn wav_from_chunks(chunks: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
        let payload_size = chunks.iter().fold(4_usize, |total, (_, payload)| {
            total
                .checked_add(8)
                .and_then(|total| total.checked_add(payload.len()))
                .and_then(|total| total.checked_add(payload.len() % 2))
                .expect("fixture RIFF length fits usize")
        });
        let riff_size = u32::try_from(payload_size).expect("fixture RIFF length fits u32");
        let mut wav = Vec::with_capacity(payload_size + 8);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&riff_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        for (id, payload) in chunks {
            wav.extend_from_slice(&id);
            wav.extend_from_slice(
                &u32::try_from(payload.len())
                    .expect("fixture chunk size fits u32")
                    .to_le_bytes(),
            );
            wav.extend_from_slice(&payload);
            if !payload.len().is_multiple_of(2) {
                wav.push(0);
            }
        }
        wav
    }

    fn assert_wav_preflight_error(error: SpeechHostError) {
        let SpeechHostError::Backend { error } = error else {
            panic!("expected WAV preflight backend error");
        };
        assert_eq!(error.code, "speech_wav_geometry_invalid");
        assert_eq!(error.class, SpeechErrorClass::InvalidRequest);
    }

    async fn wait_for_operation_phase(
        host: &SpeechHost,
        request_id: &SpeechRequestId,
        expected: operation_lifecycle::OperationPhase,
    ) {
        loop {
            if host
                .lifecycle
                .operations
                .current(&request_id.0)
                .expect("read operation phase")
                .is_some_and(|snapshot| snapshot.phase == expected)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn wait_for_active_count(host: &SpeechHost, expected: usize) {
        loop {
            let changed = host.lifecycle.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if host
                .lifecycle
                .operations
                .active_count()
                .expect("read active operation count")
                == expected
            {
                return;
            }
            changed.await;
        }
    }

    #[tokio::test]
    async fn service_plans_pins_and_executes_registered_backend() {
        let gateway = SpeechHost::default();
        let backend = fixture_backend("fixture.tts");
        gateway
            .register_backend(backend.clone())
            .expect("register fixture backend");
        let mut ticket = gateway
            .synthesize(request())
            .await
            .expect("dispatch synthesis");
        let mut terminal = 0;
        while let Some(event) = ticket.events.recv().await {
            terminal += usize::from(event.is_terminal());
        }
        let response = ticket.final_response().await.expect("final response");
        assert_eq!(response.route.backend_id, "fixture.tts");
        assert_eq!(
            response.route.model_id.as_deref(),
            Some("fixture-voice-model")
        );
        assert_eq!(response.route.voice_id.as_deref(), Some("fixture-voice"));
        assert_eq!(terminal, 1);
        assert_eq!(backend.calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn model_load_deadline_cancels_and_joins_a_blocked_dispatch() {
        let (host, clock) = host_with_manual_clock();
        let host = Arc::new(host);
        let backend = deferred_dispatch_backend("model-load-deadline.tts");
        host.register_backend(backend.clone())
            .expect("register blocked-dispatch backend");
        let request_id = SpeechRequestId("model-load-deadline".to_owned());
        let mut request = exact_request(&request_id.0, "model-load-deadline.tts");
        request.context.deadline.model_load_ms = Some(10);
        let dispatch_host = Arc::clone(&host);
        let dispatch = tokio::spawn(async move { dispatch_host.synthesize(request).await });
        backend.wait_until_dispatching(&request_id).await;

        clock.advance(Duration::from_millis(10));
        let error = match dispatch.await.expect("dispatch caller joins") {
            Err(error) => error,
            Ok(_ticket) => panic!("model-load deadline must reject an incomplete dispatch"),
        };
        let SpeechHostError::Backend { error } = error else {
            panic!("expected model-load timeout error");
        };
        assert_eq!(error.code, "speech_model_load_timeout");
        assert_eq!(error.class, SpeechErrorClass::Timeout);
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            host.lifecycle
                .operations
                .active_count()
                .expect("read active operations"),
            1,
            "timed-out dispatch remains owned until the backend ticket is joined"
        );

        backend.release(&request_id);
        wait_for_active_count(&host, 0).await;
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn total_deadline_wins_a_synthesis_dispatch_ready_at_its_cutoff() {
        let (host, clock) = host_with_manual_clock();
        let host = Arc::new(host);
        let backend = deferred_dispatch_backend("dispatch-cutoff.tts");
        host.register_backend(backend.clone())
            .expect("register deferred-dispatch backend");
        let request_id = SpeechRequestId("dispatch-cutoff".to_owned());
        let mut request = exact_request(&request_id.0, "dispatch-cutoff.tts");
        request.context.deadline.total_ms = Some(10);
        let dispatch_host = Arc::clone(&host);
        let dispatch = tokio::spawn(async move { dispatch_host.synthesize(request).await });
        backend.wait_until_dispatching(&request_id).await;

        // Make both branches ready without yielding. Total owns equality even
        // though the successful dispatch branch is biased ahead of the timer.
        clock.advance(Duration::from_millis(10));
        backend.release(&request_id);
        let error = match dispatch.await.expect("dispatch caller joins") {
            Err(error) => error,
            Ok(_ticket) => panic!("total deadline must win the dispatch tie"),
        };
        let SpeechHostError::Backend { error } = error else {
            panic!("expected total timeout error");
        };
        assert_eq!(error.code, "speech_total_timeout");
        wait_for_active_count(&host, 0).await;
    }

    #[tokio::test]
    async fn first_result_deadline_cancels_once_and_joins_a_late_final() {
        let (host, clock) = host_with_manual_clock();
        let backend = deferred_backend("first-result-deadline.tts");
        host.register_backend(backend.clone())
            .expect("register deferred backend");
        let request_id = SpeechRequestId("first-result-deadline".to_owned());
        let mut request = exact_request(&request_id.0, "first-result-deadline.tts");
        request.context.deadline.first_result_ms = Some(10);
        let mut ticket = host
            .synthesize(request)
            .await
            .expect("admit deferred request");
        backend.wait_until_active(&request_id).await;

        clock.advance(Duration::from_millis(10));
        let event = ticket
            .events
            .recv()
            .await
            .expect("receive timeout terminal");
        assert!(matches!(
            event,
            SynthesisEvent::Failed { error, .. }
                if error.code == "speech_first_result_timeout"
                    && error.class == SpeechErrorClass::Timeout
        ));
        let error = ticket
            .final_response()
            .await
            .expect_err("first-result deadline must win before the backend final");
        assert_eq!(error.code, "speech_first_result_timeout");
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            host.lifecycle
                .operations
                .active_count()
                .expect("read active operations"),
            1,
            "late backend work remains supervised after the public timeout"
        );

        backend.complete(&request_id);
        wait_for_active_count(&host, 0).await;
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn transcription_first_result_deadline_uses_the_same_host_budget() {
        let (host, clock) = host_with_manual_clock();
        let backend = deferred_transcription_backend("first-result-deadline.stt");
        host.register_backend(backend.clone())
            .expect("register deferred transcription backend");
        let request_id = SpeechRequestId("first-result-deadline-stt".to_owned());
        let mut request =
            exact_pcm_transcription_request(&request_id.0, "first-result-deadline.stt");
        request.context.deadline.first_result_ms = Some(10);
        let mut ticket = host
            .transcribe(request)
            .await
            .expect("admit deferred transcription");
        backend.wait_until_active(&request_id).await;

        clock.advance(Duration::from_millis(10));
        let event = ticket
            .events
            .recv()
            .await
            .expect("receive timeout terminal");
        assert!(matches!(
            event,
            TranscriptionEvent::Failed { error, .. }
                if error.code == "speech_first_result_timeout"
        ));
        let error = ticket
            .final_response()
            .await
            .expect_err("transcription first-result deadline must win");
        assert_eq!(error.code, "speech_first_result_timeout");
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);

        backend.complete(&request_id);
        wait_for_active_count(&host, 0).await;
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn total_deadline_wins_a_transcription_final_ready_after_its_cutoff() {
        let (host, clock) = host_with_manual_clock();
        let backend = deferred_transcription_backend("final-cutoff.stt");
        host.register_backend(backend.clone())
            .expect("register deferred transcription backend");
        let request_id = SpeechRequestId("final-cutoff-stt".to_owned());
        let mut request = exact_pcm_transcription_request(&request_id.0, "final-cutoff.stt");
        request.context.deadline.total_ms = Some(10);
        let mut ticket = host
            .transcribe(request)
            .await
            .expect("admit deferred transcription");
        backend.wait_until_active(&request_id).await;

        // Make the successful final and timer ready in one executor turn. The
        // post-read monotonic arbitration must reject the late final.
        clock.advance(Duration::from_millis(11));
        backend.complete(&request_id);
        let event = ticket
            .events
            .recv()
            .await
            .expect("receive authoritative timeout terminal");
        assert!(matches!(
            event,
            TranscriptionEvent::Failed { error, .. }
                if error.code == "speech_total_timeout"
        ));
        assert!(
            ticket.events.recv().await.is_none(),
            "the host publishes exactly one terminal"
        );
        let error = ticket
            .final_response()
            .await
            .expect_err("total deadline must reject the late backend final");
        assert_eq!(error.code, "speech_total_timeout");
        wait_for_active_count(&host, 0).await;
    }

    #[tokio::test]
    async fn idle_stream_deadline_resets_only_after_observed_results() {
        let (host, clock) = host_with_manual_clock();
        let backend = deferred_streaming_backend("idle-stream-deadline.tts");
        host.register_backend(backend.clone())
            .expect("register deferred backend");
        let request_id = SpeechRequestId("idle-stream-deadline".to_owned());
        let mut request = exact_request(&request_id.0, "idle-stream-deadline.tts");
        request.stream = true;
        request.context.deadline = SpeechDeadlinePolicy {
            first_result_ms: Some(10),
            idle_stream_ms: Some(5),
            ..SpeechDeadlinePolicy::default()
        };
        let mut ticket = host
            .synthesize(request)
            .await
            .expect("admit streaming request");
        backend.wait_until_active(&request_id).await;

        for (elapsed_ms, sequence) in [(4_u64, 0_u64), (4, 1)] {
            clock.advance(Duration::from_millis(elapsed_ms));
            backend.emit(
                &request_id,
                SynthesisEvent::Audio {
                    request_id: request_id.clone(),
                    chunk: speech_native_types::AudioChunk {
                        sequence,
                        sample_offset: sequence,
                        format: PcmFormat {
                            sample_rate_hz: 16_000,
                            channels: 1,
                            sample_format: PcmSampleFormat::I16Le,
                            interleaved: true,
                        },
                        data: vec![0, 0],
                        end_of_stream: false,
                    },
                },
            );
            assert!(matches!(
                ticket.events.recv().await,
                Some(SynthesisEvent::Audio { chunk, .. }) if chunk.sequence == sequence
            ));
        }

        clock.advance(Duration::from_millis(4));
        backend.emit(
            &request_id,
            SynthesisEvent::Warning {
                request_id: request_id.clone(),
                code: "fixture_warning".to_owned(),
                message: "warnings are not streaming results".to_owned(),
            },
        );
        assert!(matches!(
            ticket.events.recv().await,
            Some(SynthesisEvent::Warning { .. })
        ));
        clock.advance(Duration::from_millis(1));
        let event = ticket.events.recv().await.expect("receive idle terminal");
        assert!(matches!(
            event,
            SynthesisEvent::Failed { error, .. }
                if error.code == "speech_idle_stream_timeout"
        ));
        let error = ticket
            .final_response()
            .await
            .expect_err("idle deadline must win after the last result");
        assert_eq!(error.code, "speech_idle_stream_timeout");
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);

        backend.complete(&request_id);
        wait_for_active_count(&host, 0).await;
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn total_deadline_cancels_once_and_wins_a_late_backend_final() {
        let host = SpeechHost::default();
        let backend = deferred_backend("total-deadline.tts");
        host.register_backend(backend.clone())
            .expect("register deferred backend");
        let request_id = SpeechRequestId("total-deadline".to_owned());
        let mut request = exact_request(&request_id.0, "total-deadline.tts");
        request.context.deadline = SpeechDeadlinePolicy {
            total_ms: Some(10),
            ..SpeechDeadlinePolicy::default()
        };

        let mut ticket = host
            .synthesize(request)
            .await
            .expect("admit deferred request");
        tokio::time::advance(Duration::from_millis(10)).await;

        let event = ticket
            .events
            .recv()
            .await
            .expect("host publishes one authoritative timeout terminal");
        let SynthesisEvent::Failed {
            request_id: event_request_id,
            error: event_error,
        } = event
        else {
            panic!("timeout must publish a failed terminal event");
        };
        assert_eq!(event_request_id, request_id);
        assert_eq!(event_error.code, "speech_total_timeout");
        let error = ticket
            .final_response()
            .await
            .expect_err("total deadline must win before the backend final");
        assert_eq!(error.code, "speech_total_timeout");
        assert_eq!(error.class, SpeechErrorClass::Timeout);
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            host.lifecycle
                .operations
                .active_count()
                .expect("read active operations"),
            1,
            "the backend lease remains owned until its late terminal is joined"
        );
        backend.complete(&request_id);
        wait_for_active_count(&host, 0).await;
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn total_deadline_keeps_dispatch_owned_until_late_ticket_is_joined() {
        let host = Arc::new(SpeechHost::default());
        let backend = deferred_dispatch_backend("dispatch-deadline.tts");
        host.register_backend(backend.clone())
            .expect("register deferred-dispatch backend");
        let request_id = SpeechRequestId("dispatch-deadline".to_owned());
        let mut request = exact_request(&request_id.0, "dispatch-deadline.tts");
        request.context.deadline.total_ms = Some(10);
        let task_host = Arc::clone(&host);
        let dispatch = tokio::spawn(async move { task_host.synthesize(request).await });
        backend.wait_until_dispatching(&request_id).await;

        tokio::time::advance(Duration::from_millis(10)).await;
        let error = match dispatch.await.expect("dispatch caller joins") {
            Err(error) => error,
            Ok(_ticket) => panic!("total deadline must reject an incomplete dispatch"),
        };
        let SpeechHostError::Backend { error } = error else {
            panic!("expected dispatch total-timeout error");
        };
        assert_eq!(error.code, "speech_total_timeout");
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            host.lifecycle
                .operations
                .active_count()
                .expect("read active operations"),
            1,
            "the backend lease remains owned while late dispatch admission is joined"
        );

        backend.release(&request_id);
        wait_for_active_count(&host, 0).await;
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn aborted_pending_dispatch_remains_supervised_until_shutdown_joins_it() {
        let host = Arc::new(SpeechHost::default());
        let backend = deferred_dispatch_backend("abandoned-dispatch.tts");
        host.register_backend(backend.clone())
            .expect("register deferred-dispatch backend");
        let request_id = SpeechRequestId("abandoned-dispatch".to_owned());
        let dispatch_host = Arc::clone(&host);
        let dispatch_request_id = request_id.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_host
                .synthesize(exact_request(
                    &dispatch_request_id.0,
                    "abandoned-dispatch.tts",
                ))
                .await
        });
        backend.wait_until_dispatching(&request_id).await;

        dispatch.abort();
        let join_error = match dispatch.await {
            Err(error) => error,
            Ok(_) => panic!("aborted caller must not return a ticket"),
        };
        assert!(join_error.is_cancelled());
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            host.lifecycle
                .operations
                .active_count()
                .expect("read active operations"),
            1,
            "caller abort cannot release the route while backend dispatch is live"
        );
        {
            let state = host.lifecycle.state.lock().expect("lock host state");
            let registered = state
                .backends
                .get("abandoned-dispatch.tts")
                .expect("registered deferred backend");
            assert_eq!(registered.limiter.available_permits(), 0);
            assert!(state.routes.contains_key(&request_id));
        }

        let shutdown_host = Arc::clone(&host);
        let shutdown = tokio::spawn(async move { shutdown_host.shutdown().await });
        backend.wait_until_shutdown().await;
        assert!(
            !shutdown.is_finished(),
            "shutdown cannot finish while abandoned dispatch remains blocked"
        );

        backend.release(&request_id);
        shutdown
            .await
            .expect("shutdown caller joins")
            .expect("shutdown drains abandoned dispatch");
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
        assert_eq!(
            host.lifecycle
                .operations
                .active_count()
                .expect("read drained operations"),
            0
        );
        assert!(
            host.lifecycle
                .state
                .lock()
                .expect("lock closed host state")
                .routes
                .is_empty()
        );
        let tasks = host
            .lifecycle
            .tasks
            .snapshot()
            .expect("read joined task evidence");
        assert_eq!(tasks.active, 0);
        assert!(tasks.completed_tasks >= 1);
    }

    #[tokio::test(start_paused = true)]
    async fn backend_final_observed_before_total_deadline_wins_without_cancellation() {
        let host = SpeechHost::default();
        let backend = deferred_backend("deadline-boundary.tts");
        host.register_backend(backend.clone())
            .expect("register boundary fixture");
        let request_id = SpeechRequestId("deadline-boundary".to_owned());
        let mut request = exact_request(&request_id.0, "deadline-boundary.tts");
        request.context.deadline.total_ms = Some(10);

        let mut ticket = host
            .synthesize(request)
            .await
            .expect("admit boundary request");
        backend.complete(&request_id);

        let event = ticket
            .events
            .recv()
            .await
            .expect("host publishes the backend-authoritative terminal");
        assert!(matches!(
            event,
            SynthesisEvent::Completed {
                request_id: event_request_id,
                ..
            } if event_request_id == request_id
        ));
        ticket
            .final_response()
            .await
            .expect("backend final observed before the cutoff wins");
        wait_for_active_count(&host, 0).await;
        tokio::time::advance(Duration::from_millis(10)).await;
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn queue_deadline_expires_without_backend_dispatch() {
        let host = Arc::new(SpeechHost::default());
        let backend = deferred_backend_with_capacity("queue-deadline.tts", Some(1));
        host.register_backend(backend.clone())
            .expect("register queue fixture");
        let first_id = SpeechRequestId("queue-holder".to_owned());
        let queued_id = SpeechRequestId("queue-expiring".to_owned());
        let first = host
            .synthesize(exact_request(&first_id.0, "queue-deadline.tts"))
            .await
            .expect("admit queue holder");
        let queued_host = Arc::clone(&host);
        let queued_request_id = queued_id.clone();
        let queued = tokio::spawn(async move {
            let mut request = exact_request(&queued_request_id.0, "queue-deadline.tts");
            request.context.deadline.queue_ms = Some(10);
            queued_host.synthesize(request).await
        });
        wait_for_operation_phase(
            &host,
            &queued_id,
            operation_lifecycle::OperationPhase::Queued,
        )
        .await;

        tokio::time::advance(Duration::from_millis(10)).await;
        let error = match queued.await.expect("queued task joins") {
            Err(error) => error,
            Ok(_ticket) => panic!("queue deadline must reject the queued request"),
        };
        let SpeechHostError::Backend { error } = error else {
            panic!("expected queue timeout error");
        };
        assert_eq!(error.code, "speech_queue_timeout");
        assert_eq!(error.class, SpeechErrorClass::Timeout);
        assert_eq!(backend.finals.lock().expect("lock fixture finals").len(), 1);
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 0);
        assert_eq!(
            host.lifecycle
                .operations
                .active_count()
                .expect("read active operations"),
            1
        );

        backend.complete(&first_id);
        first.final_response().await.expect("queue holder final");
        wait_for_active_count(&host, 0).await;
    }

    #[tokio::test]
    async fn queued_synthesis_cancellation_releases_fifo_slot_without_dispatch() {
        let host = Arc::new(SpeechHost::default());
        let backend = deferred_backend_with_capacity("queued-cancel.tts", Some(1));
        host.register_backend(backend.clone())
            .expect("register single-capacity synthesis backend");
        let holder_id = SpeechRequestId("queued-cancel-holder".to_owned());
        let queued_id = SpeechRequestId("queued-cancel-waiter".to_owned());
        let successor_id = SpeechRequestId("queued-cancel-successor".to_owned());
        let holder = host
            .synthesize(exact_request(&holder_id.0, "queued-cancel.tts"))
            .await
            .expect("admit synthesis holder");
        let queued_host = Arc::clone(&host);
        let queued_request_id = queued_id.clone();
        let queued = tokio::spawn(async move {
            queued_host
                .synthesize(exact_request(&queued_request_id.0, "queued-cancel.tts"))
                .await
        });
        wait_for_operation_phase(
            &host,
            &queued_id,
            operation_lifecycle::OperationPhase::Queued,
        )
        .await;
        let successor_host = Arc::clone(&host);
        let successor_request_id = successor_id.clone();
        let successor = tokio::spawn(async move {
            successor_host
                .synthesize(exact_request(&successor_request_id.0, "queued-cancel.tts"))
                .await
        });
        wait_for_operation_phase(
            &host,
            &successor_id,
            operation_lifecycle::OperationPhase::Queued,
        )
        .await;

        assert_eq!(host.cancel(&queued_id), 1);
        let error = match queued.await.expect("queued synthesis caller joins") {
            Err(error) => error,
            Ok(_ticket) => panic!("queued synthesis cancellation must reject admission"),
        };
        let SpeechHostError::Backend { error } = error else {
            panic!("expected typed queued cancellation");
        };
        assert_eq!(error.code, "speech_request_cancelled");
        assert_eq!(error.class, SpeechErrorClass::Cancelled);
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 0);
        assert_eq!(
            backend.finals.lock().expect("lock synthesis finals").len(),
            1
        );
        assert!(!successor.is_finished());

        backend.complete(&holder_id);
        holder
            .final_response()
            .await
            .expect("synthesis holder completes");
        backend.wait_until_active(&successor_id).await;
        let successor = successor
            .await
            .expect("successor synthesis caller joins")
            .expect("successor takes the released FIFO slot");
        backend.complete(&successor_id);
        successor
            .final_response()
            .await
            .expect("successor synthesis completes");
        wait_for_active_count(&host, 0).await;
    }

    #[tokio::test]
    async fn queued_transcription_cancellation_releases_fifo_slot_without_dispatch() {
        let host = Arc::new(SpeechHost::default());
        let backend = deferred_transcription_backend("queued-cancel.asr");
        host.register_backend(backend.clone())
            .expect("register single-capacity transcription backend");
        let holder_id = SpeechRequestId("queued-transcription-holder".to_owned());
        let queued_id = SpeechRequestId("queued-transcription-waiter".to_owned());
        let successor_id = SpeechRequestId("queued-transcription-successor".to_owned());
        let holder = host
            .transcribe(exact_pcm_transcription_request(
                &holder_id.0,
                "queued-cancel.asr",
            ))
            .await
            .expect("admit transcription holder");
        let queued_host = Arc::clone(&host);
        let queued_request_id = queued_id.clone();
        let queued = tokio::spawn(async move {
            queued_host
                .transcribe(exact_pcm_transcription_request(
                    &queued_request_id.0,
                    "queued-cancel.asr",
                ))
                .await
        });
        wait_for_operation_phase(
            &host,
            &queued_id,
            operation_lifecycle::OperationPhase::Queued,
        )
        .await;
        let successor_host = Arc::clone(&host);
        let successor_request_id = successor_id.clone();
        let successor = tokio::spawn(async move {
            successor_host
                .transcribe(exact_pcm_transcription_request(
                    &successor_request_id.0,
                    "queued-cancel.asr",
                ))
                .await
        });
        wait_for_operation_phase(
            &host,
            &successor_id,
            operation_lifecycle::OperationPhase::Queued,
        )
        .await;

        assert_eq!(host.cancel(&queued_id), 1);
        let error = match queued.await.expect("queued transcription caller joins") {
            Err(error) => error,
            Ok(_ticket) => panic!("queued transcription cancellation must reject admission"),
        };
        let SpeechHostError::Backend { error } = error else {
            panic!("expected typed queued cancellation");
        };
        assert_eq!(error.code, "speech_request_cancelled");
        assert_eq!(error.class, SpeechErrorClass::Cancelled);
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 0);
        assert_eq!(backend.calls.load(Ordering::Acquire), 1);
        assert_eq!(
            backend
                .finals
                .lock()
                .expect("lock transcription finals")
                .len(),
            1
        );
        assert!(!successor.is_finished());

        backend.complete(&holder_id);
        holder
            .final_response()
            .await
            .expect("transcription holder completes");
        backend.wait_until_active(&successor_id).await;
        let successor = successor
            .await
            .expect("successor transcription caller joins")
            .expect("successor takes the released FIFO slot");
        backend.complete(&successor_id);
        successor
            .final_response()
            .await
            .expect("successor transcription completes");
        wait_for_active_count(&host, 0).await;
    }

    #[tokio::test]
    async fn accepted_synthesis_cancel_wins_late_success_but_not_committed_success() {
        let host = SpeechHost::default();
        let backend = deferred_backend_with_capacity("cancel-race.tts", Some(1));
        host.register_backend(backend.clone())
            .expect("register synthesis race backend");

        let cancel_first_id = SpeechRequestId("synthesis-cancel-first".to_owned());
        let mut cancelled = host
            .synthesize(exact_request(&cancel_first_id.0, "cancel-race.tts"))
            .await
            .expect("admit cancel-first synthesis");
        assert_eq!(host.cancel(&cancel_first_id), 1);
        backend.complete(&cancel_first_id);
        assert!(matches!(
            cancelled.events.recv().await,
            Some(SynthesisEvent::Cancelled { request_id, .. }) if request_id == cancel_first_id
        ));
        let error = cancelled
            .final_response()
            .await
            .expect_err("accepted cancellation owns the public final");
        assert_eq!(error.code, "speech_request_cancelled");
        assert_eq!(error.class, SpeechErrorClass::Cancelled);
        wait_for_active_count(&host, 0).await;
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);

        let success_first_id = SpeechRequestId("synthesis-success-first".to_owned());
        let success = host
            .synthesize(exact_request(&success_first_id.0, "cancel-race.tts"))
            .await
            .expect("admit success-first synthesis");
        backend.complete(&success_first_id);
        success
            .final_response()
            .await
            .expect("backend success commits before late cancellation");
        wait_for_active_count(&host, 0).await;
        assert_eq!(host.cancel(&success_first_id), 0);
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn accepted_transcription_cancel_wins_late_success_but_not_committed_success() {
        let host = SpeechHost::default();
        let backend = deferred_transcription_backend("cancel-race.asr");
        host.register_backend(backend.clone())
            .expect("register transcription race backend");

        let cancel_first_id = SpeechRequestId("transcription-cancel-first".to_owned());
        let mut cancelled = host
            .transcribe(exact_pcm_transcription_request(
                &cancel_first_id.0,
                "cancel-race.asr",
            ))
            .await
            .expect("admit cancel-first transcription");
        assert_eq!(host.cancel(&cancel_first_id), 1);
        backend.complete(&cancel_first_id);
        assert!(matches!(
            cancelled.events.recv().await,
            Some(TranscriptionEvent::Cancelled { request_id, .. }) if request_id == cancel_first_id
        ));
        let error = cancelled
            .final_response()
            .await
            .expect_err("accepted cancellation owns the public final");
        assert_eq!(error.code, "speech_request_cancelled");
        assert_eq!(error.class, SpeechErrorClass::Cancelled);
        wait_for_active_count(&host, 0).await;
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);

        let success_first_id = SpeechRequestId("transcription-success-first".to_owned());
        let success = host
            .transcribe(exact_pcm_transcription_request(
                &success_first_id.0,
                "cancel-race.asr",
            ))
            .await
            .expect("admit success-first transcription");
        backend.complete(&success_first_id);
        success
            .final_response()
            .await
            .expect("backend success commits before late cancellation");
        wait_for_active_count(&host, 0).await;
        assert_eq!(host.cancel(&success_first_id), 0);
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn single_capacity_backend_holds_n_plus_one_behind_one_fifo_lease() {
        let host = Arc::new(SpeechHost::default());
        let backend = deferred_backend_with_capacity("capacity-gate.tts", Some(1));
        host.register_backend(backend.clone())
            .expect("register capacity fixture");
        let first_id = SpeechRequestId("capacity-first".to_owned());
        let second_id = SpeechRequestId("capacity-second".to_owned());
        let third_id = SpeechRequestId("capacity-third".to_owned());

        let first = host
            .synthesize(exact_request(&first_id.0, "capacity-gate.tts"))
            .await
            .expect("admit first request");
        let second_host = Arc::clone(&host);
        let second_request_id = second_id.clone();
        let second = tokio::spawn(async move {
            second_host
                .synthesize(exact_request(&second_request_id.0, "capacity-gate.tts"))
                .await
        });
        wait_for_operation_phase(
            &host,
            &second_id,
            operation_lifecycle::OperationPhase::Queued,
        )
        .await;
        let third_host = Arc::clone(&host);
        let third_request_id = third_id.clone();
        let third = tokio::spawn(async move {
            third_host
                .synthesize(exact_request(&third_request_id.0, "capacity-gate.tts"))
                .await
        });
        wait_for_operation_phase(
            &host,
            &third_id,
            operation_lifecycle::OperationPhase::Queued,
        )
        .await;

        assert_eq!(backend.finals.lock().expect("lock fixture finals").len(), 1);

        backend.complete(&first_id);
        first
            .final_response()
            .await
            .expect("first fixture response");
        backend.wait_until_active(&second_id).await;
        let second = second
            .await
            .expect("second admission task joins")
            .expect("second request is admitted next");
        assert!(
            !third.is_finished(),
            "third request must remain behind second"
        );
        backend.complete(&second_id);
        second
            .final_response()
            .await
            .expect("second fixture response");
        backend.wait_until_active(&third_id).await;
        let third = third
            .await
            .expect("third admission task joins")
            .expect("third request is admitted last");
        backend.complete(&third_id);
        third
            .final_response()
            .await
            .expect("third fixture response");
    }

    #[test]
    fn absent_capacity_is_conservatively_registered_as_one() {
        let host = SpeechHost::default();
        host.register_backend(deferred_backend("absent-capacity.tts"))
            .expect("register backend without capacity");
        let state = host.lifecycle.state.lock().expect("lock host state");
        assert_eq!(
            state
                .backends
                .get("absent-capacity.tts")
                .expect("registered backend")
                .limiter
                .available_permits(),
            1
        );
    }

    #[test]
    fn zero_capacity_descriptor_is_rejected() {
        let host = SpeechHost::default();
        assert!(matches!(
            host.register_backend(deferred_backend_with_capacity("zero-capacity.tts", Some(0))),
            Err(SpeechHostError::BackendInvalid { .. })
        ));
    }

    #[test]
    fn live_descriptor_status_and_planning_change_without_resizing_admission() {
        let host = SpeechHost::default();
        let backend = mutable_descriptor_backend("mutable-status.tts");
        host.register_backend(backend.clone())
            .expect("register mutable descriptor fixture");
        host.plan_synthesis(&exact_request("mutable-before", "mutable-status.tts"))
            .expect("initial live descriptor plans");

        {
            let mut descriptor = backend.descriptor.lock().expect("lock live descriptor");
            descriptor.display_name = "Live status changed".to_owned();
            descriptor.capabilities[0].availability = CapabilityAvailability::Unavailable;
        }
        let status = host.status().expect("observe current descriptor status");
        assert_eq!(status.backends[0].display_name, "Live status changed");
        assert_eq!(
            status.backends[0].capabilities[0]
                .limits
                .max_concurrent_requests,
            Some(1)
        );
        assert!(
            host.plan_synthesis(&exact_request("mutable-unavailable", "mutable-status.tts",))
                .is_err(),
            "planning must use the current unavailable capability"
        );

        let state = host.lifecycle.state.lock().expect("lock host state");
        assert_eq!(
            state
                .backends
                .get("mutable-status.tts")
                .expect("registered mutable backend")
                .limiter
                .available_permits(),
            1,
            "registration freezes the independently owned admission capacity"
        );
        drop(state);

        backend
            .descriptor
            .lock()
            .expect("lock live descriptor capacity")
            .capabilities[0]
            .limits
            .max_concurrent_requests = Some(2);
        assert!(matches!(
            host.status(),
            Err(SpeechHostError::BackendInvalid { detail })
                if detail.contains("changed its admission capacity from 1 to 2")
        ));
    }

    #[tokio::test]
    async fn complete_pcm_duration_is_preflighted_before_backend_dispatch() {
        let host = SpeechHost::default();
        let backend = duration_probe_backend("duration-probe.asr", 1);
        host.register_backend(backend.clone())
            .expect("register duration fixture");
        let request = exact_pcm_transcription_request("duration-over-limit", "duration-probe.asr");
        let TranscriptionInput::Complete {
            audio: AudioInput::Pcm { format, data },
        } = &request.input
        else {
            panic!("duration fixture must be complete PCM");
        };
        let frames = data.len() / format.bytes_per_frame();
        let duration_ms = u64::try_from(frames).expect("fixture frame count fits u64") * 1_000
            / u64::from(format.sample_rate_hz);
        assert_eq!(duration_ms, 2);

        let error = match host.transcribe(request).await {
            Err(error) => error,
            Ok(_ticket) => panic!("over-limit PCM must fail host preflight"),
        };
        let SpeechHostError::Backend { error } = error else {
            panic!("expected backend probe error");
        };
        assert_eq!(error.code, "speech_audio_too_long");
        assert_eq!(error.backend_id.as_deref(), Some("duration-probe.asr"));
        assert!(
            error
                .safe_detail
                .contains("duration-probe.asr.transcription")
        );
        assert_eq!(backend.calls.load(Ordering::Acquire), 0);
        assert_eq!(
            host.lifecycle
                .operations
                .active_count()
                .expect("read active operations"),
            0,
            "preflight rejection never reserves an operation"
        );
    }

    #[tokio::test]
    async fn complete_wav_duration_is_preflighted_before_backend_dispatch() {
        let host = SpeechHost::default();
        let backend = duration_probe_backend("wav-duration.asr", 1);
        host.register_backend(backend.clone())
            .expect("register WAV duration fixture");

        let error = match host
            .transcribe(exact_wav_transcription_request(
                "wav-duration-over-limit",
                "wav-duration.asr",
            ))
            .await
        {
            Err(error) => error,
            Ok(_ticket) => panic!("over-limit WAV must fail host preflight"),
        };
        let SpeechHostError::Backend { error } = error else {
            panic!("expected WAV preflight error");
        };
        assert_eq!(error.code, "speech_audio_too_long");
        assert_eq!(backend.calls.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn malformed_wav_geometry_is_rejected_before_backend_dispatch() {
        let host = SpeechHost::default();
        let backend = duration_probe_backend("wav-geometry.asr", 10_000);
        host.register_backend(backend.clone())
            .expect("register WAV geometry fixture");
        let mut request =
            exact_pcm_transcription_request("wav-geometry-invalid", "wav-geometry.asr");
        request.input = TranscriptionInput::Complete {
            audio: AudioInput::Encoded {
                format: EncodedAudioFormat::Wav,
                data: b"not a wav".to_vec(),
            },
        };

        let error = match host.transcribe(request).await {
            Err(error) => error,
            Ok(_ticket) => panic!("malformed WAV must fail host preflight"),
        };
        let SpeechHostError::Backend { error } = error else {
            panic!("expected WAV geometry error");
        };
        assert_eq!(error.code, "speech_wav_geometry_invalid");
        assert_eq!(backend.calls.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn hostile_wav_geometry_is_bounded_and_never_reaches_the_backend() {
        let host = SpeechHost::default();
        let backend = duration_probe_backend("wav-hostile.asr", 10_000);
        host.register_backend(backend.clone())
            .expect("register hostile WAV fixture");
        let valid = classic_wav(1, 1, 16_000, 16, vec![0; 32]);
        let valid_format = valid[20..36].to_vec();
        let mut cases = Vec::new();

        let mut maximum_riff = b"RIFF".to_vec();
        maximum_riff.extend_from_slice(&u32::MAX.to_le_bytes());
        maximum_riff.extend_from_slice(b"WAVE");
        cases.push(("maximum-riff-size", maximum_riff));

        let mut maximum_chunk = b"RIFF".to_vec();
        maximum_chunk.extend_from_slice(&12_u32.to_le_bytes());
        maximum_chunk.extend_from_slice(b"WAVEJUNK");
        maximum_chunk.extend_from_slice(&u32::MAX.to_le_bytes());
        cases.push(("maximum-chunk-size", maximum_chunk));

        let mut too_many_chunks = vec![(*b"fmt ", valid_format.clone())];
        too_many_chunks.extend((0..4_095).map(|_| (*b"JUNK", Vec::new())));
        too_many_chunks.push((*b"data", vec![0; 2]));
        cases.push(("excessive-chunk-count", wav_from_chunks(too_many_chunks)));

        cases.push((
            "excessive-metadata",
            wav_from_chunks(vec![
                (*b"fmt ", valid_format.clone()),
                (*b"JUNK", vec![0; 1_048_577]),
                (*b"data", vec![0; 2]),
            ]),
        ));

        for (name, offset, value) in [
            ("zero-channels", 22, 0_u16),
            ("too-many-channels", 22, 33_u16),
            ("unsupported-pcm-width", 34, 12_u16),
            ("inconsistent-block-align", 32, 4_u16),
        ] {
            let mut wav = valid.clone();
            wav[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            cases.push((name, wav));
        }
        for (name, value) in [
            ("sample-rate-below-bound", 7_999_u32),
            ("sample-rate-above-bound", 384_001_u32),
        ] {
            let mut wav = valid.clone();
            wav[24..28].copy_from_slice(&value.to_le_bytes());
            cases.push((name, wav));
        }
        let mut unsupported_encoding = valid.clone();
        unsupported_encoding[20..22].copy_from_slice(&2_u16.to_le_bytes());
        cases.push(("unsupported-encoding", unsupported_encoding));

        let unsupported_float = classic_wav(3, 1, 16_000, 64, vec![0; 8]);
        cases.push(("unsupported-float-width", unsupported_float));

        let mut incomplete_classic_format = valid_format.clone();
        incomplete_classic_format.push(0);
        cases.push((
            "incomplete-classic-extension",
            wav_from_chunks(vec![
                (*b"fmt ", incomplete_classic_format),
                (*b"data", vec![0; 2]),
            ]),
        ));

        let mut inconsistent_byte_rate = valid.clone();
        inconsistent_byte_rate[28..32].copy_from_slice(&1_u32.to_le_bytes());
        cases.push(("inconsistent-byte-rate", inconsistent_byte_rate));

        let mut overflowing_byte_rate = valid.clone();
        overflowing_byte_rate[24..28].copy_from_slice(&384_000_u32.to_le_bytes());
        overflowing_byte_rate[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        overflowing_byte_rate[32..34].copy_from_slice(&u16::MAX.to_le_bytes());
        cases.push(("byte-rate-overflow", overflowing_byte_rate));

        cases.push((
            "incomplete-data-frame",
            classic_wav(1, 2, 16_000, 16, vec![0; 3]),
        ));
        cases.push((
            "extensible-valid-bits-mismatch",
            extensible_wav(1, 1, 16_000, 24, 20, vec![0; 3]),
        ));
        let mut unknown_extensible = extensible_wav(1, 1, 16_000, 16, 16, vec![0; 2]);
        unknown_extensible[44..46].copy_from_slice(&2_u16.to_le_bytes());
        cases.push(("unknown-extensible-subformat", unknown_extensible));
        let extensible = extensible_wav(1, 1, 16_000, 16, 16, vec![0; 2]);
        let mut trailing_extensible_format = extensible[20..60].to_vec();
        trailing_extensible_format.push(0);
        cases.push((
            "trailing-extensible-format-bytes",
            wav_from_chunks(vec![
                (*b"fmt ", trailing_extensible_format),
                (*b"data", vec![0; 2]),
            ]),
        ));

        let mut short_extensible = extensible;
        short_extensible[16..20].copy_from_slice(&18_u32.to_le_bytes());
        let remove_start = 38;
        let remove_end = 60;
        short_extensible.drain(remove_start..remove_end);
        let short_riff_size = u32::try_from(short_extensible.len() - 8)
            .expect("short extensible fixture length fits u32");
        short_extensible[4..8].copy_from_slice(&short_riff_size.to_le_bytes());
        cases.push(("truncated-extensible-format", short_extensible));

        cases.push(("truncated-riff-header", valid[..11].to_vec()));
        let mut truncated_chunk_header = b"RIFF".to_vec();
        truncated_chunk_header.extend_from_slice(&8_u32.to_le_bytes());
        truncated_chunk_header.extend_from_slice(b"WAVEfmt ");
        cases.push(("truncated-chunk-header", truncated_chunk_header));
        let mut truncated_chunk = valid.clone();
        truncated_chunk.pop();
        let truncated_riff_size =
            u32::try_from(truncated_chunk.len() - 8).expect("truncated fixture length fits u32");
        truncated_chunk[4..8].copy_from_slice(&truncated_riff_size.to_le_bytes());
        cases.push(("truncated-chunk-payload", truncated_chunk));

        let mut missing_padding =
            wav_from_chunks(vec![(*b"fmt ", valid_format.clone()), (*b"JUNK", vec![0])]);
        missing_padding.pop();
        let missing_padding_size =
            u32::try_from(missing_padding.len() - 8).expect("padding fixture length fits u32");
        missing_padding[4..8].copy_from_slice(&missing_padding_size.to_le_bytes());
        cases.push(("truncated-chunk-padding", missing_padding));

        cases.push((
            "duplicate-format",
            wav_from_chunks(vec![
                (*b"fmt ", valid_format.clone()),
                (*b"fmt ", valid_format),
                (*b"data", vec![0; 2]),
            ]),
        ));
        cases.push((
            "duplicate-data",
            wav_from_chunks(vec![
                (*b"fmt ", valid[20..36].to_vec()),
                (*b"data", vec![0; 2]),
                (*b"data", vec![0; 2]),
            ]),
        ));

        for (index, (name, wav)) in cases.into_iter().enumerate() {
            let request_id = format!("wav-hostile-{index}-{name}");
            let error = match host
                .transcribe(wav_request(&request_id, "wav-hostile.asr", wav))
                .await
            {
                Err(error) => error,
                Ok(_ticket) => panic!("hostile WAV case {name} reached backend dispatch"),
            };
            assert_wav_preflight_error(error);
        }
        assert_eq!(
            backend.calls.load(Ordering::Acquire),
            0,
            "all hostile inputs stop at private host preflight"
        );
    }

    #[test]
    fn valid_classic_and_extensible_wav_boundaries_have_exact_duration() {
        let request_id = SpeechRequestId("wav-valid-boundaries".to_owned());
        let backend_id = "wav-valid.asr";
        let classic_minimum = classic_wav(1, 1, 8_000, 8, vec![0; 8_000]);
        assert_eq!(
            wav_duration_ms(&classic_minimum, &request_id, backend_id),
            Ok(1_000)
        );
        let classic_maximum = classic_wav(1, 32, 384_000, 32, vec![0; 128]);
        assert_eq!(
            wav_duration_ms(&classic_maximum, &request_id, backend_id),
            Ok(1)
        );
        let extensible_pcm = extensible_wav(1, 2, 48_000, 32, 24, vec![0; 384_000]);
        assert_eq!(
            wav_duration_ms(&extensible_pcm, &request_id, backend_id),
            Ok(1_000)
        );
        let extensible_float = extensible_wav(3, 1, 48_000, 32, 32, vec![0; 192_000]);
        assert_eq!(
            wav_duration_ms(&extensible_float, &request_id, backend_id),
            Ok(1_000)
        );
    }

    #[tokio::test]
    async fn exact_wav_duration_boundary_dispatches_and_one_frame_over_does_not() {
        let host = SpeechHost::default();
        let backend = duration_probe_backend("wav-boundary.asr", 1_000);
        host.register_backend(backend.clone())
            .expect("register WAV boundary fixture");
        let exact = classic_wav(1, 1, 8_000, 16, vec![0; 16_000]);
        let exact_error = match host
            .transcribe(wav_request("wav-exact-boundary", "wav-boundary.asr", exact))
            .await
        {
            Err(error) => error,
            Ok(_ticket) => panic!("duration probe fixture never returns a ticket"),
        };
        assert!(matches!(
            exact_error,
            SpeechHostError::Backend { ref error } if error.code == "duration_probe_reached"
        ));
        assert_eq!(backend.calls.load(Ordering::Acquire), 1);

        let over = classic_wav(1, 1, 8_000, 16, vec![0; 16_002]);
        let over_error = match host
            .transcribe(wav_request("wav-over-boundary", "wav-boundary.asr", over))
            .await
        {
            Err(error) => error,
            Ok(_ticket) => panic!("one-frame-over WAV must fail preflight"),
        };
        assert!(matches!(
            over_error,
            SpeechHostError::Backend { ref error } if error.code == "speech_audio_too_long"
        ));
        assert_eq!(
            backend.calls.load(Ordering::Acquire),
            1,
            "over-boundary WAV does not add a backend dispatch"
        );
    }

    #[test]
    fn duplicate_backend_registration_fails_closed() {
        let gateway = SpeechHost::default();
        gateway
            .register_backend(fixture_backend("fixture.tts"))
            .expect("first registration");
        assert!(matches!(
            gateway.register_backend(fixture_backend("fixture.tts")),
            Err(SpeechHostError::BackendDuplicate { .. })
        ));
    }

    #[tokio::test]
    async fn shutdown_attempts_every_backend_and_reports_all_failures() {
        let gateway = SpeechHost::default();
        let healthy = fixture_backend("healthy.tts");
        let first_failure = failing_fixture_backend("failure-a.tts");
        let second_failure = failing_fixture_backend("failure-b.tts");
        for backend in [&healthy, &first_failure, &second_failure] {
            gateway
                .register_backend(backend.clone())
                .expect("register shutdown fixture");
        }

        let error = gateway
            .shutdown()
            .await
            .expect_err("shutdown must report failures");
        let SpeechHostError::Shutdown { failures } = error else {
            panic!("expected aggregated shutdown failure");
        };
        assert_eq!(failures.len(), 2);
        assert_eq!(healthy.shutdown_calls.load(Ordering::Acquire), 1);
        assert_eq!(first_failure.shutdown_calls.load(Ordering::Acquire), 1);
        assert_eq!(second_failure.shutdown_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn host_owns_request_identity_until_backend_final() {
        let host = SpeechHost::default();
        let backend_a = deferred_backend("deferred-a.tts");
        let backend_b = deferred_backend("deferred-b.tts");
        host.register_backend(backend_a.clone())
            .expect("register backend A");
        host.register_backend(backend_b.clone())
            .expect("register backend B");

        let request_id = SpeechRequestId("global-request-id".to_string());
        let ticket = host
            .synthesize(exact_request(&request_id.0, "deferred-a.tts"))
            .await
            .expect("start deferred request");
        drop(ticket);
        assert_eq!(backend_a.cancel_calls.load(Ordering::Acquire), 1);
        assert_eq!(backend_b.cancel_calls.load(Ordering::Acquire), 0);

        assert!(matches!(
            host.synthesize(exact_request(&request_id.0, "deferred-b.tts"))
                .await,
            Err(SpeechHostError::RequestDuplicate { .. })
        ));
        assert_eq!(host.cancel(&request_id), 0);
        assert_eq!(backend_a.cancel_calls.load(Ordering::Acquire), 1);
        assert_eq!(backend_b.cancel_calls.load(Ordering::Acquire), 0);

        backend_a.complete(&request_id);
        loop {
            let changed = host.lifecycle.changed.notified();
            if host
                .lifecycle
                .operations
                .active_count()
                .expect("read active operations")
                == 0
            {
                break;
            }
            changed.await;
        }

        let ticket = host
            .synthesize(exact_request(&request_id.0, "deferred-b.tts"))
            .await
            .expect("request id is reusable after backend final");
        backend_b.complete(&request_id);
        let response = ticket.final_response().await.expect("deferred final");
        assert_eq!(response.route.backend_id, "deferred-b.tts");
    }

    #[tokio::test]
    async fn shutdown_waits_for_backend_final_and_retains_result() {
        let host = Arc::new(SpeechHost::default());
        let backend = deferred_backend("deferred.tts");
        host.register_backend(backend.clone())
            .expect("register deferred backend");
        let request_id = SpeechRequestId("shutdown-held-request".to_string());
        let ticket = host
            .synthesize(exact_request(&request_id.0, "deferred.tts"))
            .await
            .expect("start deferred request");
        drop(ticket);

        let leader_host = Arc::clone(&host);
        let mut leader = tokio::spawn(async move { leader_host.shutdown().await });
        let waiter_host = Arc::clone(&host);
        let mut waiter = tokio::spawn(async move { waiter_host.shutdown().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut leader)
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut waiter)
                .await
                .is_err()
        );

        backend.complete(&request_id);
        leader
            .await
            .expect("leader task joins")
            .expect("leader result");
        waiter
            .await
            .expect("waiter task joins")
            .expect("waiter result");
        host.shutdown().await.expect("retained shutdown result");
        assert_eq!(backend.shutdown_calls.load(Ordering::Acquire), 1);
        assert!(matches!(
            host.register_backend(fixture_backend("late.tts")),
            Err(SpeechHostError::AdmissionClosed)
        ));
        assert!(matches!(
            host.synthesize(exact_request("late", "deferred.tts")).await,
            Err(SpeechHostError::AdmissionClosed)
        ));
    }

    #[tokio::test]
    async fn aborting_first_shutdown_caller_does_not_strand_followers() {
        let host = Arc::new(SpeechHost::default());
        let backend = deferred_backend("abort-safe.tts");
        host.register_backend(backend.clone())
            .expect("register deferred backend");
        let request_id = SpeechRequestId("abort-safe".to_owned());
        let ticket = host
            .synthesize(exact_request(&request_id.0, "abort-safe.tts"))
            .await
            .expect("start deferred request");
        drop(ticket);

        let first_host = Arc::clone(&host);
        let first = tokio::spawn(async move { first_host.shutdown().await });
        tokio::task::yield_now().await;
        first.abort();
        first.await.expect_err("first caller is aborted");
        backend.complete(&request_id);

        tokio::time::timeout(std::time::Duration::from_secs(1), host.shutdown())
            .await
            .expect("detached coordinator completes")
            .expect("retained shutdown succeeds");
        assert_eq!(backend.shutdown_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn backend_shutdown_panic_is_retained_for_every_caller() {
        let host = SpeechHost::default();
        host.register_backend(panicking_fixture_backend("panic.tts"))
            .expect("register panicking backend");

        let first = host
            .shutdown()
            .await
            .expect_err("backend panic fails shutdown");
        let retained = host
            .shutdown()
            .await
            .expect_err("backend panic result is retained");
        assert_eq!(first, retained);
        let SpeechHostError::Shutdown { failures } = first else {
            panic!("backend panic must be a shutdown failure");
        };
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].code, "speech_backend_shutdown_panicked");
    }

    #[tokio::test]
    async fn poisoned_finalization_fails_consumer_and_shutdown_without_hanging() {
        let host = Arc::new(SpeechHost::default());
        let backend = deferred_backend("poisoned-finalization.tts");
        host.register_backend(backend.clone())
            .expect("register deferred backend");
        let request_id = SpeechRequestId("poisoned-finalization".to_owned());
        let ticket = host
            .synthesize(exact_request(&request_id.0, "poisoned-finalization.tts"))
            .await
            .expect("start deferred request");
        host.lifecycle
            .operations
            .current_lease(&request_id.0)
            .expect("read production operation registry")
            .expect("operation is active")
            .poison_released_slot_for_test();

        let leader_host = Arc::clone(&host);
        let leader = tokio::spawn(async move { leader_host.shutdown().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if host
                    .lifecycle
                    .state
                    .lock()
                    .expect("read shutdown state")
                    .shutdown_started
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("leader enters shutdown");
        let follower_host = Arc::clone(&host);
        let follower = tokio::spawn(async move { follower_host.shutdown().await });

        backend.complete(&request_id);
        let consumer_error =
            tokio::time::timeout(std::time::Duration::from_secs(1), ticket.final_response())
                .await
                .expect("consumer finalization must not hang")
                .expect_err("poisoned finalization must fail the consumer");
        assert_eq!(consumer_error.code, "speech_lifecycle_finalization_failed");

        let (leader_result, follower_result) =
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                tokio::join!(leader, follower)
            })
            .await
            .expect("leader and follower must not hang on poisoned finalization");
        assert_eq!(
            leader_result.expect("leader joins"),
            Err(SpeechHostError::StateUnavailable)
        );
        assert_eq!(
            follower_result.expect("follower joins"),
            Err(SpeechHostError::StateUnavailable)
        );
        assert_eq!(backend.shutdown_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn ten_thousand_fixture_operations_self_reap_task_state() {
        let host = SpeechHost::default();
        host.register_backend(fixture_backend("fixture.tts"))
            .expect("register fixture backend");

        for index in 0..10_000 {
            let ticket = host
                .synthesize(exact_request(
                    &format!("bounded-task-{index}"),
                    "fixture.tts",
                ))
                .await
                .expect("fixture request must be admitted");
            ticket
                .final_response()
                .await
                .expect("every fixture request has a final response");
        }

        host.lifecycle
            .tasks
            .wait_for_idle()
            .await
            .expect("task supervisor remains available");
        assert_eq!(
            host.lifecycle
                .operations
                .active_count()
                .expect("read active operations"),
            0
        );
        let task_state = host
            .lifecycle
            .tasks
            .snapshot()
            .expect("task supervisor remains available");
        assert_eq!(task_state.active, 0);
        assert_eq!(task_state.retained_failures, 0);
    }

    #[tokio::test]
    async fn monitor_panic_is_preserved_in_shutdown_evidence() {
        let host = SpeechHost::default();
        host.spawn_monitor("fixture-monitor-panic".to_owned(), async {
            panic!("fixture monitor panic")
        })
        .expect("spawn fixture monitor");

        let error = host
            .shutdown()
            .await
            .expect_err("monitor panic must fail shutdown");
        let SpeechHostError::Shutdown { failures } = error else {
            panic!("expected shutdown failure");
        };
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].code, "speech_host_monitor_failed");
        assert!(failures[0].safe_detail.contains("fixture monitor panic"));
    }

    #[test]
    fn host_nonce_exhaustion_fails_closed() {
        let host = SpeechHost::default();
        host.register_backend(fixture_backend("fixture.tts"))
            .expect("register fixture backend");
        host.lifecycle
            .operations
            .set_next_sequence_for_test(u64::MAX)
            .expect("set exhausted sequence");

        assert!(matches!(
            host.reserve_synthesis(&exact_request("nonce-exhausted", "fixture.tts")),
            Err(SpeechHostError::NonceExhausted)
        ));
        assert!(
            host.lifecycle
                .operations
                .active_count()
                .expect("read active operations")
                == 0
        );
    }

    #[test]
    fn setup_guard_drop_rolls_back_registry_and_route() {
        let host = SpeechHost::default();
        host.register_backend(fixture_backend("fixture.tts"))
            .expect("register fixture backend");
        let request_id = SpeechRequestId("setup-rollback".to_owned());
        let reserved = host
            .reserve_synthesis(&exact_request(&request_id.0, "fixture.tts"))
            .expect("reserve setup fixture");
        let setup = SetupGuard::new(
            Arc::clone(&host.lifecycle),
            request_id,
            reserved.operation.clone(),
        );
        reserved.operation.queue().expect("queue once");
        drop(setup);
        assert_eq!(
            host.lifecycle.operations.active_count(),
            Ok(0),
            "rollback releases the production registry"
        );
        assert!(
            host.lifecycle
                .state
                .lock()
                .expect("host state")
                .routes
                .is_empty(),
            "rollback removes the matching production route"
        );
        assert!(!host.lifecycle.faulted.load(Ordering::Acquire));
    }

    #[test]
    fn mismatched_route_release_is_observable_and_preserves_route() {
        let host = SpeechHost::default();
        host.register_backend(fixture_backend("fixture.tts"))
            .expect("register fixture backend");
        let request_id = SpeechRequestId("route-mismatch".to_owned());
        let reserved = host
            .reserve_synthesis(&exact_request(&request_id.0, "fixture.tts"))
            .expect("reserve route fixture");
        let mut wrong = reserved.operation.identity();
        wrong.sequence = wrong
            .sequence
            .checked_add(1)
            .expect("fixture sequence room");

        assert_eq!(
            host.lifecycle.release_route(&request_id, &wrong),
            Err(SpeechHostError::StateUnavailable)
        );
        assert!(
            host.lifecycle
                .state
                .lock()
                .expect("host state")
                .routes
                .contains_key(&request_id)
        );
        assert!(host.lifecycle.faulted.load(Ordering::Acquire));
    }

    #[test]
    fn cancellation_tolerates_natural_request_id_generation_change() {
        let host = SpeechHost::default();
        let backend = deferred_backend("cancel-identity.tts");
        host.register_backend(backend.clone())
            .expect("register deferred backend");
        let request_id = SpeechRequestId("cancel-identity".to_owned());
        let old = host
            .reserve_synthesis(&exact_request(&request_id.0, "cancel-identity.tts"))
            .expect("reserve old generation");
        let old_identity = old.operation.identity();
        old.operation.queue().expect("queue old generation");
        old.operation.start().expect("start old generation");
        let attempt = old.operation.start_attempt().expect("old attempt");
        old.operation
            .finish_attempt_and_release(&attempt, operation_lifecycle::TerminalClass::Completed)
            .expect("release old generation");
        host.lifecycle
            .release_route(&request_id, &old_identity)
            .expect("release old route");
        let current = host
            .reserve_synthesis(&exact_request(&request_id.0, "cancel-identity.tts"))
            .expect("reserve reused request ID");

        assert_eq!(
            host.lifecycle.cancel_captured(
                &request_id,
                old_identity.clone(),
                backend.clone(),
                Arc::clone(&old.cancellation),
                None,
            ),
            0
        );
        assert_eq!(
            host.lifecycle.cancel_captured(
                &request_id,
                old_identity.clone(),
                backend.clone(),
                Arc::clone(&old.cancellation),
                Some(&old_identity),
            ),
            0
        );
        assert_eq!(backend.cancel_calls.load(Ordering::Acquire), 0);
        assert!(!host.lifecycle.faulted.load(Ordering::Acquire));
        assert!(
            current
                .operation
                .is_active()
                .expect("current generation state")
        );
        current
            .operation
            .fail_setup_and_release()
            .expect("clean current generation");
        host.lifecycle
            .release_route(&request_id, &current.operation.identity())
            .expect("clean current route");
    }
}
