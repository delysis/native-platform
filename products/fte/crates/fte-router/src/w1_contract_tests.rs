//! Wave 1 contract checks over the real gateway surfaces.
//!
//! The component adapters bind one production Gateway lifecycle identity.
//! Component-local suites read the Gateway operation registry; bridge suites
//! cross real admission, ticket, bounded progress, supervised backend task,
//! and shutdown-coordinator ownership. The final test accepts all eleven
//! suite results as one typed manifest only after every runner returns.

use super::*;
use async_trait::async_trait;
use fte_types::{
    BackendReadiness, BackendRequest, CachePolicy, CancelTarget, ContentBlock, DeadlinePolicy,
    GatewayEvent, GatewayResponse, GatewayTicket, GatewayUsage, GenerationInput, InputItem,
    MessageRole, ModelCapabilities, ModelSelector, PromptForm, RouteObservations, RoutingPolicy,
    SamplingOptions, StoragePolicy, StreamPolicy, TerminalStatus, TicketCancellation, ToolPolicy,
};
use platform_contract_testkit::compositional_lifecycle::{
    AdmissionQuiesceShutdownBridgeAdapter, AttemptHierarchyAdapter, ConsumerCancellationAdapter,
    PanicShutdownBridgeAdapter, ProgressShutdownBridgeAdapter, RegistryIdentityAdapter,
    ShutdownWitness, StableShutdownAdapter, TaskReapingAdapter, TerminalAuthorityAdapter,
    TransitionChainAdapter, WaiterControlAdapter, run_admission_quiesce_shutdown_bridge_suite,
    run_attempt_hierarchy_suite, run_consumer_cancellation_suite, run_panic_shutdown_bridge_suite,
    run_progress_shutdown_bridge_suite, run_registry_identity_suite, run_stable_shutdown_suite,
    run_task_reaping_suite, run_terminal_authority_suite, run_transition_chain_suite,
    run_waiter_control_suite,
};
use platform_contract_testkit::contracts::{
    self, CapabilityEntryV0, CapabilitySnapshotV0, ContentDigest, DataHandlingV0, DataTierV0,
    LoggingPolicyV0, NetworkPolicyV0, OperationId, PayloadRedactionV0, PrivacyDecisionV0,
    PrivacyDenialV0, PrivacyPolicyV0, ProviderId, Readiness, RedactionStateV0, RetryAdvice,
    RoutePrivacyContextV0, RouteTargetV0, ServiceErrorV0, ServiceId, TriState,
};
use platform_contract_testkit::{
    AttemptIdentity, ClosedFacts, CoverageEvidence, LifecycleCoverageManifest,
    LifecycleImplementation, LifecyclePhase, OperationPhase, OperationSnapshot, ShutdownOutcome,
    TerminalClass, TerminalRecord, WaitObservation, validate_capability_snapshot_v0,
    validate_service_error_v0,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

enum FteGatewayLifecycle {}

impl LifecycleImplementation for FteGatewayLifecycle {
    const PRODUCT: &'static str = "free-token-energy";
    const IMPLEMENTATION: &'static str = "gateway-v2";
}

#[derive(Clone)]
struct GatewayRegistryAdapter {
    registry: operation_lifecycle::OperationRegistry,
}

struct RegistryOperation {
    _guard: operation_lifecycle::ConsumerGuard,
    lease: operation_lifecycle::OperationLease,
}

impl GatewayRegistryAdapter {
    fn with_sequence(next_sequence: u64) -> Self {
        Self {
            registry: operation_lifecycle::OperationRegistry::new(next_sequence, 3),
        }
    }

    fn start_operation(
        &self,
        operation_id: &str,
    ) -> Result<RegistryOperation, operation_lifecycle::RegistryError> {
        let (guard, lease) = self.registry.reserve(operation_id)?;
        lease.queue()?;
        lease.start()?;
        Ok(RegistryOperation {
            _guard: guard,
            lease,
        })
    }
}

fn contract_identity(identity: operation_lifecycle::OperationIdentity) -> AttemptIdentity {
    AttemptIdentity {
        operation_id: identity.operation_id,
        attempt_id: identity.attempt_id,
        sequence: identity.sequence,
    }
}

fn contract_terminal(terminal: operation_lifecycle::TerminalClass) -> TerminalClass {
    match terminal {
        operation_lifecycle::TerminalClass::Completed => TerminalClass::Completed,
        operation_lifecycle::TerminalClass::Cancelled => TerminalClass::Cancelled,
        operation_lifecycle::TerminalClass::Failed => TerminalClass::Failed,
    }
}

fn contract_snapshot(snapshot: operation_lifecycle::OperationSnapshot) -> OperationSnapshot {
    let terminal = snapshot.terminal.map(|terminal| TerminalRecord {
        class: contract_terminal(terminal),
        sequence: snapshot.identity.sequence,
    });
    OperationSnapshot {
        identity: contract_identity(snapshot.identity),
        phase: match snapshot.phase {
            operation_lifecycle::OperationPhase::Reserved => OperationPhase::Reserved,
            operation_lifecycle::OperationPhase::Queued => OperationPhase::Queued,
            operation_lifecycle::OperationPhase::Running => OperationPhase::Running,
            operation_lifecycle::OperationPhase::Terminal => OperationPhase::Terminal,
            operation_lifecycle::OperationPhase::Released => OperationPhase::Released,
        },
        cancellation_requested: snapshot.cancellation_requested,
        authoritative_terminal: terminal,
        final_projection: terminal,
        progress_projection: snapshot.progress,
    }
}

fn registry_terminal(terminal: TerminalClass) -> operation_lifecycle::TerminalClass {
    match terminal {
        TerminalClass::Completed => operation_lifecycle::TerminalClass::Completed,
        TerminalClass::Cancelled => operation_lifecycle::TerminalClass::Cancelled,
        TerminalClass::Failed => operation_lifecycle::TerminalClass::Failed,
    }
}

fn observed<T>(result: Result<T, operation_lifecycle::RegistryError>) -> T {
    result.expect("the deterministic contract registry remains unpoisoned")
}

impl TransitionChainAdapter for GatewayRegistryAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = operation_lifecycle::RegistryError;
    type Operation = RegistryOperation;

    fn deterministic() -> Self {
        Self::with_sequence(1)
    }

    fn reserve(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        let (guard, lease) = self.registry.reserve(operation_id)?;
        Ok(RegistryOperation {
            _guard: guard,
            lease,
        })
    }

    fn phase(&self, operation: &Self::Operation) -> Option<OperationPhase> {
        operation
            .lease
            .snapshot()
            .expect("contract registry snapshot")
            .map(contract_snapshot)
            .map(|s| s.phase)
    }

    fn queue(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation.lease.queue()
    }

    fn start(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation.lease.start()
    }

    fn terminal(
        &self,
        operation: &Self::Operation,
        class: TerminalClass,
    ) -> Result<(), Self::Error> {
        operation.lease.terminal(registry_terminal(class))
    }

    fn release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation.lease.release()
    }
}

impl RegistryIdentityAdapter for GatewayRegistryAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = operation_lifecycle::RegistryError;
    type Guard = operation_lifecycle::ConsumerGuard;
    type Lease = operation_lifecycle::OperationLease;

    fn deterministic(next_sequence: u64) -> Self {
        Self::with_sequence(next_sequence)
    }

    fn reserve(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error> {
        self.registry.reserve(operation_id)
    }

    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity {
        contract_identity(lease.identity())
    }

    fn complete_and_release(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        lease.queue()?;
        lease.start()?;
        lease.terminal(operation_lifecycle::TerminalClass::Completed)?;
        lease.release()
    }

    fn active_count(&self) -> usize {
        observed(self.registry.active_count())
    }

    fn current_identity(&self, operation_id: &str) -> Option<AttemptIdentity> {
        self.registry
            .current(operation_id)
            .expect("contract registry current operation")
            .map(|snapshot| contract_identity(snapshot.identity))
    }
}

impl AttemptHierarchyAdapter for GatewayRegistryAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = operation_lifecycle::RegistryError;
    type Operation = RegistryOperation;
    type Attempt = operation_lifecycle::AttemptLease;

    fn deterministic() -> Self {
        Self::with_sequence(1)
    }

    fn create_operation(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        self.start_operation(operation_id)
    }

    fn start_attempt(&self, operation: &Self::Operation) -> Result<Self::Attempt, Self::Error> {
        operation.lease.start_attempt()
    }

    fn attempt_identity(&self, attempt: &Self::Attempt) -> AttemptIdentity {
        contract_identity(attempt.identity())
    }

    fn operation_active(&self, operation: &Self::Operation) -> bool {
        observed(operation.lease.is_active())
    }

    fn active_attempts(&self, operation: &Self::Operation) -> Vec<AttemptIdentity> {
        operation
            .lease
            .active_attempts()
            .expect("operation remains current")
            .into_iter()
            .map(contract_identity)
            .collect()
    }

    fn request_operation_cancel(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation.lease.request_cancel()
    }

    fn cancellation_requested(&self, attempt: &Self::Attempt) -> bool {
        attempt
            .cancellation_requested()
            .expect("attempt remains current")
    }

    fn finish_attempt(&self, attempt: Self::Attempt) -> Result<(), Self::Error> {
        attempt.finish()
    }

    fn finish_operation(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation
            .lease
            .terminal(operation_lifecycle::TerminalClass::Cancelled)?;
        operation.lease.release()
    }
}

impl ConsumerCancellationAdapter for GatewayRegistryAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = operation_lifecycle::RegistryError;
    type Ticket = operation_lifecycle::ConsumerGuard;
    type Lease = operation_lifecycle::OperationLease;

    fn deterministic() -> Self {
        Self::with_sequence(1)
    }

    fn start(&self, operation_id: &str) -> Result<(Self::Ticket, Self::Lease), Self::Error> {
        let (guard, lease) = self.registry.reserve(operation_id)?;
        lease.queue()?;
        lease.start()?;
        Ok((guard, lease))
    }

    fn ticket_identity(&self, ticket: &Self::Ticket) -> AttemptIdentity {
        contract_identity(ticket.identity())
    }

    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity {
        contract_identity(lease.identity())
    }

    fn active_count(&self) -> usize {
        observed(self.registry.active_count())
    }

    fn current_snapshot(&self, operation_id: &str) -> Option<OperationSnapshot> {
        observed(self.registry.current(operation_id)).map(contract_snapshot)
    }

    fn lease_snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        observed(lease.snapshot()).map(contract_snapshot)
    }

    fn cancellation_requested(&self, lease: &Self::Lease) -> bool {
        lease
            .snapshot()
            .expect("contract registry snapshot")
            .is_some_and(|snapshot| snapshot.cancellation_requested)
    }

    fn explicit_consumer_drop(&self, ticket: Self::Ticket) -> Result<(), Self::Error> {
        ticket.cancel()?;
        drop(ticket);
        Ok(())
    }

    fn finish_cancelled(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        lease.terminal(operation_lifecycle::TerminalClass::Cancelled)?;
        lease.release()
    }
}

impl TerminalAuthorityAdapter for GatewayRegistryAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = operation_lifecycle::RegistryError;
    type Guard = operation_lifecycle::ConsumerGuard;
    type Lease = operation_lifecycle::OperationLease;

    fn deterministic() -> Self {
        Self::with_sequence(1)
    }

    fn start(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error> {
        ConsumerCancellationAdapter::start(self, operation_id)
    }

    fn terminal(&self, lease: &Self::Lease, class: TerminalClass) -> Result<(), Self::Error> {
        lease.terminal(registry_terminal(class))
    }

    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        observed(lease.snapshot()).map(contract_snapshot)
    }

    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        lease.release()
    }
}

impl WaiterControlAdapter for GatewayRegistryAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = operation_lifecycle::RegistryError;
    type Ticket = operation_lifecycle::ConsumerGuard;
    type Lease = operation_lifecycle::OperationLease;

    fn deterministic() -> Self {
        Self::with_sequence(1)
    }

    fn start(&self, operation_id: &str) -> Result<(Self::Ticket, Self::Lease), Self::Error> {
        ConsumerCancellationAdapter::start(self, operation_id)
    }

    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        observed(lease.snapshot()).map(contract_snapshot)
    }

    fn waiter_timeout(&self, _ticket: &Self::Ticket) -> Result<WaitObservation, Self::Error> {
        Ok(WaitObservation::TimedOut)
    }

    fn request_cancel(&self, ticket: &Self::Ticket) -> Result<(), Self::Error> {
        ticket.cancel()
    }

    fn finish_cancelled(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        lease.terminal(operation_lifecycle::TerminalClass::Cancelled)?;
        lease.release()
    }
}

#[derive(Clone)]
struct GatewayStableShutdownAdapter {
    gateway: Arc<Gateway>,
    runtime: Arc<tokio::runtime::Runtime>,
    next_sequence: Arc<AtomicU64>,
    operations: Arc<Mutex<BTreeMap<String, BackendTaskState>>>,
    event_capacity: usize,
}

type BackendTaskState = (Arc<AtomicBool>, Arc<AtomicBool>);

struct ControlledGatewayOperation {
    request_id: RequestId,
    ticket: Mutex<Option<GatewayTicket>>,
    complete: Mutex<Option<oneshot::Sender<TerminalStatus>>>,
    allow_exit: Arc<Semaphore>,
    shutdown_observed: Mutex<Option<mpsc::Receiver<()>>>,
    events: Arc<Mutex<Option<tokio::sync::mpsc::Sender<fte_types::GatewayEvent>>>>,
    backend: Arc<ControlledGatewayBackend>,
    lifecycle: operation_lifecycle::OperationLease,
}

struct ControlledGatewayBackend {
    descriptor: BackendDescriptor,
    complete: Mutex<Option<oneshot::Receiver<TerminalStatus>>>,
    allow_exit: Arc<Semaphore>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown_observed: Mutex<Option<mpsc::SyncSender<()>>>,
    cancelled: Arc<AtomicBool>,
    retained: Arc<AtomicBool>,
    event_capacity: usize,
    events: Arc<Mutex<Option<tokio::sync::mpsc::Sender<fte_types::GatewayEvent>>>>,
}

#[async_trait]
impl GatewayBackend for ControlledGatewayBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }

    fn readiness(&self) -> BackendReadiness {
        BackendReadiness::Ready
    }

    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
        let complete = self
            .complete
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &request.request.request_id,
                    "contract_backend_already_executed",
                    "the controlled contract backend accepts exactly one request",
                )
            })?;
        let request_id = request.request.request_id.clone();
        let route = request.route;
        let (events_tx, events_rx) = tokio::sync::mpsc::channel(self.event_capacity);
        *self
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(events_tx);
        let (final_tx, final_rx) = oneshot::channel();
        let terminal = Arc::new(AtomicBool::new(false));
        let terminal_for_task = Arc::clone(&terminal);
        let allow_exit = Arc::clone(&self.allow_exit);
        let retained = Arc::clone(&self.retained);
        let request_id_for_task = request_id.clone();
        retained.store(true, Ordering::Release);
        let task = tokio::spawn(async move {
            let status = complete.await.unwrap_or(TerminalStatus::Cancelled);
            let response = GatewayResponse {
                id: format!("contract-response-{request_id_for_task}"),
                request_id: request_id_for_task,
                model: route.model_id.clone(),
                route: route.clone(),
                output: Vec::new(),
                usage: GatewayUsage {
                    selected_route: Some(route),
                    ..GatewayUsage::default()
                },
                status,
                previous_response_id: None,
            };
            terminal_for_task.store(true, Ordering::Release);
            let _ = final_tx.send(Ok(response));
            let _permit = allow_exit
                .acquire()
                .await
                .expect("worker-exit gate remains open");
            retained.store(false, Ordering::Release);
        });
        *self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
        Ok(GatewayTicket::new(
            request_id,
            events_rx,
            final_rx,
            Arc::new(ControlledCancellation {
                cancelled: Arc::clone(&self.cancelled),
            }),
            terminal,
        ))
    }

    fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
        usize::from(!self.cancelled.swap(true, Ordering::AcqRel))
    }

    async fn shutdown(&self) -> Result<(), GatewayError> {
        let task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task.await.map_err(|error| GatewayError {
                code: "contract_backend_task_failed".to_owned(),
                class: ErrorClass::Internal,
                retryable: false,
                http_status: 500,
                request_id: RequestId::new(),
                provider: Some(self.descriptor.id.clone()),
                safe_detail: format!("controlled backend task failed: {error}"),
            })?;
        }
        if let Some(observed) = self
            .shutdown_observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = observed.send(());
        }
        Ok(())
    }
}

impl ControlledGatewayBackend {
    async fn reap_finished_task(&self) -> Result<(), GatewayError> {
        let task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task.await.map_err(|error| GatewayError {
                code: "contract_backend_task_failed".to_owned(),
                class: ErrorClass::Internal,
                retryable: false,
                http_status: 500,
                request_id: RequestId::new(),
                provider: Some(self.descriptor.id.clone()),
                safe_detail: format!("controlled backend task failed: {error}"),
            })?;
        }
        Ok(())
    }
}

struct ControlledCancellation {
    cancelled: Arc<AtomicBool>,
}

impl TicketCancellation for ControlledCancellation {
    fn cancel(&self, _target: CancelTarget) -> usize {
        usize::from(!self.cancelled.swap(true, Ordering::AcqRel))
    }
}

struct GatewayShutdownWitness {
    started: Mutex<(Option<mpsc::Receiver<()>>, bool)>,
    result: Mutex<ShutdownWitnessResult>,
    thread: Option<thread::JoinHandle<()>>,
}

type ShutdownWitnessResult = (
    mpsc::Receiver<Result<ShutdownOutcome, GatewayError>>,
    Option<ShutdownOutcome>,
);

impl ShutdownWitness for GatewayShutdownWitness {
    type Error = String;

    fn wait_started(&self, timeout: Duration) -> Result<(), Self::Error> {
        let mut started = self
            .started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if started.1 {
            return Ok(());
        }
        started
            .0
            .take()
            .ok_or_else(|| "shutdown start channel is unavailable".to_owned())?
            .recv_timeout(timeout)
            .map_err(|error| {
                format!("shutdown did not start before the witness deadline: {error}")
            })?;
        started.1 = true;
        Ok(())
    }

    fn try_complete(&self) -> Result<Option<ShutdownOutcome>, Self::Error> {
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.1.is_none() {
            match result.0.try_recv() {
                Ok(Ok(outcome)) => result.1 = Some(outcome),
                Ok(Err(error)) => return Err(error.to_string()),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("shutdown result channel disconnected".to_owned());
                }
            }
        }
        Ok(result.1.clone())
    }

    fn wait(mut self, timeout: Duration) -> Result<ShutdownOutcome, Self::Error> {
        let outcome = {
            let mut result = self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match result.1.take() {
                Some(outcome) => outcome,
                None => result
                    .0
                    .recv_timeout(timeout)
                    .map_err(|error| format!("shutdown did not finish before deadline: {error}"))?
                    .map_err(|error| error.to_string())?,
            }
        };
        self.thread
            .take()
            .ok_or_else(|| "shutdown thread was already joined".to_owned())?
            .join()
            .map_err(|_| "shutdown witness thread panicked".to_owned())?;
        Ok(outcome)
    }
}

fn gateway_shutdown_witness(
    gateway: Arc<Gateway>,
    runtime: Arc<tokio::runtime::Runtime>,
) -> GatewayShutdownWitness {
    let (started_tx, started_rx) = mpsc::sync_channel(0);
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || {
        runtime.block_on(async move {
            let shutdown_gateway = Arc::clone(&gateway);
            let shutdown = tokio::spawn(async move { shutdown_gateway.shutdown_report().await });
            loop {
                if gateway.status().lifecycle != GatewayLifecycle::Running {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let _ = started_tx.send(());
            let result = match shutdown.await {
                Ok(report) => report.result.map(|()| {
                    let status = gateway.status();
                    ShutdownOutcome {
                        facts: ClosedFacts {
                            lifecycle: lifecycle_phase(status.lifecycle),
                            active_operations: status.active_requests,
                            retained_tasks: report.retained_tasks,
                            expected_workers: report.expected_worker_ids.len(),
                            joined_workers: report.joined_worker_ids.len(),
                        },
                        expected_worker_ids: report.expected_worker_ids,
                        joined_worker_ids: report.joined_worker_ids,
                    }
                }),
                Err(error) => Err(GatewayError::unavailable(
                    &RequestId::new(),
                    "contract_shutdown_task_failed",
                    &format!("Gateway shutdown task failed: {error}"),
                )),
            };
            let _ = result_tx.send(result);
        });
    });
    GatewayShutdownWitness {
        started: Mutex::new((Some(started_rx), false)),
        result: Mutex::new((result_rx, None)),
        thread: Some(thread),
    }
}

impl GatewayStableShutdownAdapter {
    fn request(
        sequence: u64,
        operation_id: &str,
        backend_id: &str,
        model_id: &str,
    ) -> GatewayRequest {
        let mut request = local_request();
        request.request_id = RequestId(operation_id.to_owned());
        request.client_id = format!("w1-stable-shutdown-{sequence}");
        request.model = ModelSelector::ExactRoute {
            backend_id: backend_id.to_owned(),
            model_id: model_id.to_owned(),
        };
        request
    }
}

impl StableShutdownAdapter for GatewayStableShutdownAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = GatewayError;
    type Operation = ControlledGatewayOperation;
    type ShutdownWitness = GatewayShutdownWitness;

    fn deterministic() -> Self {
        Self {
            gateway: Arc::new(Gateway::new(GatewayDefaults::default())),
            runtime: Arc::new(tokio::runtime::Runtime::new().expect("contract-test runtime")),
            next_sequence: Arc::new(AtomicU64::new(1)),
            operations: Arc::new(Mutex::new(BTreeMap::new())),
            event_capacity: 1,
        }
    }

    fn start(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        let sequence = self.next_sequence.fetch_add(1, Ordering::AcqRel);
        let backend_id = format!("w1-controlled-backend-{sequence}");
        let model_id = format!("{backend_id}-model");
        let (complete_tx, complete_rx) = oneshot::channel();
        let (shutdown_observed_tx, shutdown_observed_rx) = mpsc::sync_channel(0);
        let allow_exit = Arc::new(Semaphore::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let retained = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(None));
        self.operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                operation_id.to_owned(),
                (Arc::clone(&cancelled), Arc::clone(&retained)),
            );
        let backend = Arc::new(ControlledGatewayBackend {
            descriptor: descriptor(&backend_id, BackendLocation::LocalEmbedded),
            complete: Mutex::new(Some(complete_rx)),
            allow_exit: Arc::clone(&allow_exit),
            task: Mutex::new(None),
            shutdown_observed: Mutex::new(Some(shutdown_observed_tx)),
            cancelled: Arc::clone(&cancelled),
            retained: Arc::clone(&retained),
            event_capacity: self.event_capacity,
            events: Arc::clone(&events),
        });
        self.gateway.register_backend(backend.clone())?;
        let mut request = Self::request(sequence, operation_id, &backend_id, &model_id);
        request.stream.event_capacity = Some(self.event_capacity);
        let request_id = request.request_id.clone();
        let ticket = self.runtime.block_on(self.gateway.execute(request))?;
        let lifecycle = self
            .gateway
            .lifecycle
            .operations
            .current_lease(operation_id)
            .map_err(|error| registry_gateway_error(&request_id, error))?
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &request_id,
                    "contract_operation_registry_missing",
                    "the admitted Gateway operation is missing from its production registry",
                )
            })?;
        Ok(ControlledGatewayOperation {
            request_id,
            ticket: Mutex::new(Some(ticket)),
            complete: Mutex::new(Some(complete_tx)),
            allow_exit,
            shutdown_observed: Mutex::new(Some(shutdown_observed_rx)),
            events,
            backend,
            lifecycle,
        })
    }

    fn request_completed_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation
            .complete
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_completion_already_requested",
                    "completion may be requested only once",
                )
            })?
            .send(TerminalStatus::Completed)
            .map_err(|_| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_completion_disconnected",
                    "the controlled backend stopped before completion",
                )
            })
    }

    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        let ticket = operation
            .ticket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_result_already_observed",
                    "the controlled result may be observed only once",
                )
            })?;
        let response = self
            .runtime
            .block_on(async move { tokio::time::timeout(timeout, ticket.final_response()).await })
            .map_err(|_| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_result_timeout",
                    "the controlled result did not arrive before the contract deadline",
                )
            })??;
        let class = match response.status {
            TerminalStatus::Completed => TerminalClass::Completed,
            TerminalStatus::Cancelled => TerminalClass::Cancelled,
            TerminalStatus::Failed => TerminalClass::Failed,
        };
        let snapshot = operation
            .lifecycle
            .snapshot()
            .map_err(|error| registry_gateway_error(&operation.request_id, error))?
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_release_snapshot_missing",
                    "the Gateway production registry lost its released operation snapshot",
                )
            })?;
        let snapshot = contract_snapshot(snapshot);
        if snapshot
            .authoritative_terminal
            .is_none_or(|terminal| terminal.class != class)
        {
            return Err(GatewayError::unavailable(
                &operation.request_id,
                "contract_terminal_projection_mismatch",
                "the Gateway registry terminal does not match the authoritative response",
            ));
        }
        Ok(snapshot)
    }

    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation.allow_exit.add_permits(1);
        operation
            .shutdown_observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_shutdown_already_observed",
                    "backend shutdown completion may be observed only once",
                )
            })?
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_backend_shutdown_timeout",
                    "the backend worker did not exit before the contract deadline",
                )
            })
    }

    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        gateway_shutdown_witness(Arc::clone(&self.gateway), Arc::clone(&self.runtime))
    }
}

#[derive(Clone)]
struct GatewayAdmissionBridgeAdapter {
    inner: GatewayStableShutdownAdapter,
    pending_shutdown: Arc<Mutex<Option<GatewayShutdownWitness>>>,
    phase_gate: Arc<Semaphore>,
    phase_gate_released: Arc<AtomicBool>,
}

struct QuiescingObservationBackend {
    phase_gate: Arc<Semaphore>,
}

#[async_trait]
impl GatewayBackend for QuiescingObservationBackend {
    fn descriptor(&self) -> BackendDescriptor {
        descriptor("w1-quiescing-observation", BackendLocation::LocalEmbedded)
    }

    fn readiness(&self) -> BackendReadiness {
        BackendReadiness::Ready
    }

    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
        Err(GatewayError::unavailable(
            &request.request.request_id,
            "contract_observation_backend_not_executable",
            "the shutdown observation backend is not an inference route",
        ))
    }

    fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
        0
    }

    async fn shutdown(&self) -> Result<(), GatewayError> {
        self.phase_gate
            .acquire()
            .await
            .expect("quiescing observation gate remains open")
            .forget();
        Ok(())
    }
}

impl AdmissionQuiesceShutdownBridgeAdapter for GatewayAdmissionBridgeAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = GatewayError;
    type Operation = ControlledGatewayOperation;
    type ShutdownWitness = GatewayShutdownWitness;

    fn deterministic() -> Self {
        let inner = <GatewayStableShutdownAdapter as StableShutdownAdapter>::deterministic();
        let phase_gate = Arc::new(Semaphore::new(0));
        inner
            .gateway
            .register_backend(Arc::new(QuiescingObservationBackend {
                phase_gate: Arc::clone(&phase_gate),
            }))
            .expect("register quiescing observation backend");
        Self {
            inner,
            pending_shutdown: Arc::new(Mutex::new(None)),
            phase_gate,
            phase_gate_released: Arc::new(AtomicBool::new(false)),
        }
    }

    fn reserve(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        <GatewayStableShutdownAdapter as StableShutdownAdapter>::start(&self.inner, operation_id)
    }

    fn quiesce(&self) {
        let witness =
            <GatewayStableShutdownAdapter as StableShutdownAdapter>::begin_shutdown(&self.inner);
        witness
            .wait_started(Duration::from_secs(5))
            .expect("real Gateway reaches quiescing");
        *self
            .pending_shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(witness);
    }

    fn phase(&self) -> LifecyclePhase {
        let phase = lifecycle_phase(self.inner.gateway.status().lifecycle);
        if phase == LifecyclePhase::Quiescing
            && !self.phase_gate_released.swap(true, Ordering::AcqRel)
        {
            self.phase_gate.add_permits(1);
        }
        phase
    }

    fn active_count(&self) -> usize {
        self.inner.gateway.status().active_requests
    }

    fn retained_task_count(&self) -> usize {
        self.inner
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|(_, retained)| retained.load(Ordering::Acquire))
            .count()
    }

    fn cancellation_requested(&self, operation_id: &str) -> bool {
        self.inner
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(operation_id)
            .is_some_and(|(cancelled, _)| cancelled.load(Ordering::Acquire))
    }

    fn request_cancelled_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation
            .complete
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_completion_already_requested",
                    "completion may be requested only once",
                )
            })?
            .send(TerminalStatus::Cancelled)
            .map_err(|_| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_completion_disconnected",
                    "the controlled backend stopped before cancellation completion",
                )
            })
    }

    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        <GatewayStableShutdownAdapter as StableShutdownAdapter>::wait_released(
            &self.inner,
            operation,
            timeout,
        )
    }

    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        <GatewayStableShutdownAdapter as StableShutdownAdapter>::allow_worker_exit(
            &self.inner,
            operation,
        )
    }

    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        self.pending_shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or_else(|| {
                <GatewayStableShutdownAdapter as StableShutdownAdapter>::begin_shutdown(&self.inner)
            })
    }

    fn shutdown(&self) -> ClosedFacts {
        let report = self
            .inner
            .runtime
            .block_on(self.inner.gateway.shutdown_report());
        report
            .result
            .expect("Gateway repeated shutdown remains successful");
        let status = self.inner.gateway.status();
        ClosedFacts {
            lifecycle: lifecycle_phase(status.lifecycle),
            active_operations: status.active_requests,
            retained_tasks: self.retained_task_count(),
            expected_workers: report.expected_worker_ids.len(),
            joined_workers: report.joined_worker_ids.len(),
        }
    }
}

#[derive(Clone)]
struct GatewayProgressBridgeAdapter {
    inner: GatewayStableShutdownAdapter,
    progress_capacity: usize,
}

struct GatewayProgressOperation {
    controlled: ControlledGatewayOperation,
}

impl ProgressShutdownBridgeAdapter for GatewayProgressBridgeAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = GatewayError;
    type UnreadProgress = ();
    type Operation = Arc<GatewayProgressOperation>;
    type ShutdownWitness = GatewayShutdownWitness;

    fn deterministic(progress_capacity: usize) -> Self {
        let mut inner = <GatewayStableShutdownAdapter as StableShutdownAdapter>::deterministic();
        inner.event_capacity = progress_capacity.max(1);
        Self {
            inner,
            progress_capacity,
        }
    }

    fn start(
        &self,
        operation_id: &str,
    ) -> Result<(Self::UnreadProgress, Self::Operation), Self::Error> {
        let controlled = <GatewayStableShutdownAdapter as StableShutdownAdapter>::start(
            &self.inner,
            operation_id,
        )?;
        Ok(((), Arc::new(GatewayProgressOperation { controlled })))
    }

    fn publish_progress(
        &self,
        operation: &Self::Operation,
        sequence: u64,
    ) -> Result<(), Self::Error> {
        operation
            .controlled
            .lifecycle
            .publish_progress(sequence)
            .map_err(|error| registry_gateway_error(&operation.controlled.request_id, error))?;
        let sender = operation
            .controlled
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.controlled.request_id,
                    "contract_progress_channel_missing",
                    "the real backend progress channel was not installed",
                )
            })?;
        match sender.try_send(GatewayEvent::Warning {
            request_id: operation.controlled.request_id.clone(),
            code: "contract-progress".to_owned(),
            message: sequence.to_string(),
        }) {
            Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(GatewayError::unavailable(
                    &operation.controlled.request_id,
                    "contract_progress_channel_closed",
                    "the backend progress channel is closed",
                ))
            }
        }
    }

    fn snapshot(&self, operation: &Self::Operation) -> Option<OperationSnapshot> {
        operation
            .controlled
            .lifecycle
            .snapshot()
            .expect("contract registry snapshot")
            .map(contract_snapshot)
    }

    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        <GatewayStableShutdownAdapter as StableShutdownAdapter>::begin_shutdown(&self.inner)
    }

    fn request_completed_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        <GatewayStableShutdownAdapter as StableShutdownAdapter>::request_completed_release(
            &self.inner,
            &operation.controlled,
        )
    }

    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        let mut final_snapshot =
            <GatewayStableShutdownAdapter as StableShutdownAdapter>::wait_released(
                &self.inner,
                &operation.controlled,
                timeout,
            )?;
        if let Some(snapshot) = operation
            .controlled
            .lifecycle
            .snapshot()
            .map_err(|error| registry_gateway_error(&operation.controlled.request_id, error))?
        {
            final_snapshot.progress_projection = snapshot.progress;
        }
        Ok(final_snapshot)
    }

    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        <GatewayStableShutdownAdapter as StableShutdownAdapter>::allow_worker_exit(
            &self.inner,
            &operation.controlled,
        )
    }

    fn progress_capacity(&self) -> usize {
        self.progress_capacity
    }
}

fn registry_gateway_error(
    request_id: &RequestId,
    error: operation_lifecycle::RegistryError,
) -> GatewayError {
    GatewayError::unavailable(
        request_id,
        "gateway_operation_lifecycle_error",
        &format!("Gateway operation lifecycle rejected the transition: {error:?}"),
    )
}

struct CatchingPanicBackend {
    descriptor: BackendDescriptor,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    release_panic: Arc<Semaphore>,
}

#[async_trait]
impl GatewayBackend for CatchingPanicBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }

    fn readiness(&self) -> BackendReadiness {
        BackendReadiness::Ready
    }

    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
        let request_id = request.request.request_id;
        let (_events_tx, events_rx) = tokio::sync::mpsc::channel(1);
        let (final_tx, final_rx) = oneshot::channel();
        let terminal = Arc::new(AtomicBool::new(false));
        let terminal_for_task = Arc::clone(&terminal);
        let request_for_task = request_id.clone();
        let release_panic = Arc::clone(&self.release_panic);
        let task = tokio::spawn(async move {
            release_panic
                .acquire()
                .await
                .expect("panic release gate remains open")
                .forget();
            let panic = tokio::spawn(async { panic!("controlled backend executor panic") }).await;
            let detail = match panic {
                Ok(()) => "controlled executor unexpectedly returned".to_owned(),
                Err(error) => format!("controlled executor panic was caught: {error}"),
            };
            terminal_for_task.store(true, Ordering::Release);
            let _ = final_tx.send(Err(GatewayError {
                code: "backend_executor_panicked".to_owned(),
                class: ErrorClass::Internal,
                retryable: false,
                http_status: 500,
                request_id: request_for_task,
                provider: None,
                safe_detail: detail,
            }));
        });
        *self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
        Ok(GatewayTicket::new(
            request_id,
            events_rx,
            final_rx,
            Arc::new(ControlledCancellation {
                cancelled: Arc::new(AtomicBool::new(false)),
            }),
            terminal,
        ))
    }

    fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
        0
    }

    async fn shutdown(&self) -> Result<(), GatewayError> {
        let task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task.await.map_err(|error| {
                GatewayError::unavailable(
                    &RequestId::new(),
                    "panic_supervisor_failed",
                    &format!("the panic supervisor itself failed: {error}"),
                )
            })?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct GatewayPanicBridgeAdapter {
    gateway: Arc<Gateway>,
    runtime: Arc<tokio::runtime::Runtime>,
    release_panic: Arc<Semaphore>,
}

struct GatewayPanicOperation {
    request_id: RequestId,
    ticket: Mutex<Option<GatewayTicket>>,
    lifecycle: operation_lifecycle::OperationLease,
}

impl PanicShutdownBridgeAdapter for GatewayPanicBridgeAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = GatewayError;
    type Operation = GatewayPanicOperation;
    type ShutdownWitness = GatewayShutdownWitness;

    fn deterministic() -> Self {
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        let release_panic = Arc::new(Semaphore::new(0));
        gateway
            .register_backend(Arc::new(CatchingPanicBackend {
                descriptor: descriptor("w1-panic-backend", BackendLocation::LocalEmbedded),
                task: Mutex::new(None),
                release_panic: Arc::clone(&release_panic),
            }))
            .expect("register panic-supervised backend");
        Self {
            gateway,
            runtime: Arc::new(tokio::runtime::Runtime::new().expect("contract-test runtime")),
            release_panic,
        }
    }

    fn run_controlled_panicking_operation(
        &self,
        operation_id: &str,
    ) -> Result<Self::Operation, Self::Error> {
        let mut request = GatewayStableShutdownAdapter::request(
            1,
            operation_id,
            "w1-panic-backend",
            "w1-panic-backend-model",
        );
        request.stream.event_capacity = Some(1);
        let request_id = request.request_id.clone();
        let ticket = self.runtime.block_on(self.gateway.execute(request))?;
        let lifecycle = self
            .gateway
            .lifecycle
            .operations
            .current_lease(operation_id)
            .map_err(|error| registry_gateway_error(&request_id, error))?
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &request_id,
                    "panic_operation_registry_missing",
                    "the panic-supervised operation is missing from the Gateway registry",
                )
            })?;
        self.release_panic.add_permits(1);
        Ok(GatewayPanicOperation {
            request_id,
            ticket: Mutex::new(Some(ticket)),
            lifecycle,
        })
    }

    fn wait_failed_release(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        let ticket = operation
            .ticket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "panic_result_already_observed",
                    "the panic result may be observed only once",
                )
            })?;
        let result = self
            .runtime
            .block_on(async move { tokio::time::timeout(timeout, ticket.final_response()).await })
            .map_err(|_| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "panic_result_timeout",
                    "the panic supervisor did not publish its failure before the deadline",
                )
            })?;
        if result.is_ok() {
            return Err(GatewayError::unavailable(
                &operation.request_id,
                "panic_result_unexpected_success",
                "the controlled panic unexpectedly completed",
            ));
        }
        operation
            .lifecycle
            .snapshot()
            .map_err(|error| registry_gateway_error(&operation.request_id, error))?
            .map(contract_snapshot)
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "panic_release_snapshot_missing",
                    "the Gateway production registry lost the failed release snapshot",
                )
            })
    }

    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        gateway_shutdown_witness(Arc::clone(&self.gateway), Arc::clone(&self.runtime))
    }
}

#[derive(Clone)]
struct GatewayTaskReapingAdapter {
    inner: GatewayStableShutdownAdapter,
}

impl TaskReapingAdapter for GatewayTaskReapingAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = GatewayError;
    type Operation = ControlledGatewayOperation;

    fn deterministic() -> Self {
        Self {
            inner: <GatewayStableShutdownAdapter as StableShutdownAdapter>::deterministic(),
        }
    }

    fn start(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        <GatewayStableShutdownAdapter as StableShutdownAdapter>::start(&self.inner, operation_id)
    }

    fn finish(&self, operation: Self::Operation) -> Result<(), Self::Error> {
        <GatewayStableShutdownAdapter as StableShutdownAdapter>::request_completed_release(
            &self.inner,
            &operation,
        )?;
        <GatewayStableShutdownAdapter as StableShutdownAdapter>::wait_released(
            &self.inner,
            &operation,
            Duration::from_secs(5),
        )?;
        operation.allow_exit.add_permits(1);
        self.inner
            .runtime
            .block_on(operation.backend.reap_finished_task())
    }

    fn active_count(&self) -> usize {
        self.inner.gateway.status().active_requests
    }

    fn retained_task_count(&self) -> usize {
        self.inner
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter(|(_, retained)| retained.load(Ordering::Acquire))
            .count()
    }

    fn shutdown(&self) -> ClosedFacts {
        let report = self
            .inner
            .runtime
            .block_on(self.inner.gateway.shutdown_report());
        report.result.expect("Gateway task-owner shutdown succeeds");
        let status = self.inner.gateway.status();
        ClosedFacts {
            lifecycle: lifecycle_phase(status.lifecycle),
            active_operations: status.active_requests,
            retained_tasks: self.retained_task_count(),
            expected_workers: report.expected_worker_ids.len(),
            joined_workers: report.joined_worker_ids.len(),
        }
    }
}

fn lifecycle_phase(phase: GatewayLifecycle) -> LifecyclePhase {
    match phase {
        GatewayLifecycle::Running => LifecyclePhase::Running,
        GatewayLifecycle::Quiescing => LifecyclePhase::Quiescing,
        GatewayLifecycle::Closed => LifecyclePhase::Closed,
    }
}

struct SnapshotBackend {
    descriptor: BackendDescriptor,
    readiness: BackendReadiness,
}

#[async_trait]
impl GatewayBackend for SnapshotBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }

    fn readiness(&self) -> BackendReadiness {
        self.readiness.clone()
    }

    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
        Err(GatewayError::unavailable(
            &request.request.request_id,
            "fixture_not_executed",
            "contract fixture exposes inventory only",
        ))
    }

    fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
        0
    }
}

fn descriptor(id: &str, location: BackendLocation) -> BackendDescriptor {
    BackendDescriptor {
        id: id.to_owned(),
        display_name: id.to_owned(),
        location,
        models: vec![ModelDescriptor {
            id: format!("{id}-model"),
            aliases: Vec::new(),
            display_name: id.to_owned(),
            backend_id: id.to_owned(),
            location,
            capabilities: ModelCapabilities {
                prompt_forms: vec![PromptForm::Chat],
                modalities: Vec::new(),
                tools: false,
                structured_output: false,
                reasoning: false,
                streaming: true,
                provider_cache: false,
            },
            context_tokens: Some(4_096),
            max_output_tokens: Some(512),
            observed: RouteObservations::default(),
        }],
    }
}

fn local_request() -> GatewayRequest {
    GatewayRequest {
        request_id: RequestId::new(),
        client_id: "w1-contract-test".to_owned(),
        model: ModelSelector::Profile {
            name: "local-only".to_owned(),
        },
        input: GenerationInput::Chat {
            items: vec![InputItem::Message {
                id: None,
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "inventory probe".to_owned(),
                }],
            }],
        },
        sampling: SamplingOptions::default(),
        response_format: ResponseFormat::Text,
        tools: Vec::new(),
        tool_policy: ToolPolicy::default(),
        cache: CachePolicy::default(),
        routing: RoutingPolicy::default(),
        storage: StoragePolicy::default(),
        deadline: DeadlinePolicy::default(),
        stream: StreamPolicy::default(),
        provider_extensions: BTreeMap::new(),
    }
}

fn local_only_contract_policy() -> PrivacyPolicyV0 {
    PrivacyPolicyV0 {
        schema: contracts::privacy::PRIVACY_POLICY_SCHEMA_V0.to_owned(),
        network: NetworkPolicyV0::Deny,
        data_handling: DataHandlingV0::LocalOnly,
        allowed_provider_ids: Vec::new(),
        allowed_hosted_data_tiers: Vec::new(),
        payload_redaction: PayloadRedactionV0::LocalOnly,
        logging: LoggingPolicyV0::Disabled,
    }
}

fn capability_snapshot(gateway: &Gateway) -> CapabilitySnapshotV0 {
    let backend_snapshots = gateway.backend_snapshots();
    let serialized = serde_json::to_vec(&backend_snapshots).expect("serialize gateway inventory");
    let digest = format!("{:x}", Sha256::digest(serialized));
    let entries = backend_snapshots
        .into_iter()
        .flat_map(|snapshot| {
            let readiness = match snapshot.readiness {
                BackendReadiness::Ready => Readiness::Ready,
                BackendReadiness::Loading => Readiness::Unknown,
                BackendReadiness::NotConfigured { .. } | BackendReadiness::Unavailable { .. } => {
                    Readiness::Unavailable
                }
            };
            let remediation = match &snapshot.readiness {
                BackendReadiness::Ready => None,
                BackendReadiness::Loading => {
                    Some("wait for backend loading to complete".to_owned())
                }
                BackendReadiness::NotConfigured { reason }
                | BackendReadiness::Unavailable { reason } => Some(reason.clone()),
            };
            snapshot.descriptor.models.into_iter().map(move |model| {
                let mut limits = BTreeMap::new();
                if let Some(context_tokens) = model.context_tokens {
                    limits.insert("context_tokens".to_owned(), u64::from(context_tokens));
                }
                if let Some(max_output_tokens) = model.max_output_tokens {
                    limits.insert("max_output_tokens".to_owned(), u64::from(max_output_tokens));
                }
                CapabilityEntryV0 {
                    operation: "chat".to_owned(),
                    backend_or_resource_id: format!("{}/{}", model.backend_id, model.id),
                    readiness,
                    limits,
                    network: if model.location == BackendLocation::Hosted {
                        TriState::Yes
                    } else {
                        TriState::No
                    },
                    privacy_eligible: if model.location == BackendLocation::LocalEmbedded {
                        TriState::Yes
                    } else {
                        TriState::No
                    },
                    evidence_source: "Gateway::backend_snapshots".to_owned(),
                    evidence_outcome: format!("{:?}", snapshot.readiness),
                    observed_at_unix_ms: None,
                    remediation: remediation.clone(),
                }
            })
        })
        .collect();
    CapabilitySnapshotV0 {
        schema: contracts::capability::CAPABILITY_SCHEMA_V0.to_owned(),
        snapshot_id: ContentDigest::sha256(digest).expect("SHA-256 digest must validate"),
        target: "fte-router-gateway".to_owned(),
        services: BTreeMap::from([(
            ServiceId::new("inference-gateway").expect("service ID must validate"),
            entries,
        )]),
        reports: Vec::new(),
    }
}

fn service_error(error: &GatewayError) -> ServiceErrorV0 {
    let class = match error.class {
        ErrorClass::Authentication | ErrorClass::Authorization => contracts::ErrorClass::Permission,
        ErrorClass::InvalidRequest => contracts::ErrorClass::InvalidRequest,
        ErrorClass::Capability => contracts::ErrorClass::Unsupported,
        ErrorClass::Privacy => contracts::ErrorClass::Privacy,
        ErrorClass::Quota | ErrorClass::RateLimit => contracts::ErrorClass::ResourceExhausted,
        ErrorClass::Timeout => contracts::ErrorClass::Timeout,
        ErrorClass::Cancelled => contracts::ErrorClass::Cancelled,
        ErrorClass::Unavailable | ErrorClass::Provider => contracts::ErrorClass::Unavailable,
        ErrorClass::Internal => contracts::ErrorClass::Internal,
    };
    ServiceErrorV0 {
        schema: contracts::error::SERVICE_ERROR_SCHEMA_V0.to_owned(),
        code: error.code.clone(),
        class,
        retry: if error.class == ErrorClass::Privacy || !error.retryable {
            RetryAdvice::Never
        } else {
            RetryAdvice::DifferentRoute
        },
        operation_id: Some(
            OperationId::new(error.request_id.to_string())
                .expect("FTE request IDs must be safe contract operation IDs"),
        ),
        service: ServiceId::new("inference-gateway").expect("service ID must validate"),
        safe_detail: error.safe_detail.clone(),
    }
}

#[test]
fn w1_contract_stable_shutdown_slice_uses_real_gateway_workers() {
    let evidence = run_stable_shutdown_suite::<GatewayStableShutdownAdapter>("gateway-supervisor");
    assert_eq!(evidence.product(), "free-token-energy");
    assert_eq!(evidence.implementation(), "gateway-v2");
    assert_eq!(evidence.suite(), "stable-shutdown");
    assert_eq!(evidence.invariants().count(), 2);
}

#[test]
fn w1_contract_gateway_registry_component_suites() {
    let evidence: Vec<CoverageEvidence<FteGatewayLifecycle>> = vec![
        run_transition_chain_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_registry_identity_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_attempt_hierarchy_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_consumer_cancellation_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_terminal_authority_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_waiter_control_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
    ];
    assert_eq!(evidence.len(), 6);
    assert!(
        evidence
            .iter()
            .all(|item| item.implementation() == "gateway-v2")
    );
}

#[test]
fn w1_contract_gateway_admission_shutdown_bridge() {
    let evidence = run_admission_quiesce_shutdown_bridge_suite::<GatewayAdmissionBridgeAdapter>(
        "gateway-admission-backend-shutdown",
    );
    assert_eq!(evidence.suite(), "admission-quiesce-shutdown-bridge");
}

#[test]
fn w1_contract_gateway_progress_shutdown_bridge() {
    let evidence = run_progress_shutdown_bridge_suite::<GatewayProgressBridgeAdapter>(
        "gateway-progress-backend-shutdown",
    );
    assert_eq!(evidence.suite(), "progress-shutdown-bridge");
}

#[test]
fn w1_contract_gateway_panic_shutdown_bridge() {
    let evidence = run_panic_shutdown_bridge_suite::<GatewayPanicBridgeAdapter>(
        "gateway-panic-supervisor-shutdown",
    );
    assert_eq!(evidence.suite(), "panic-shutdown-bridge");
}

#[test]
fn w1_contract_gateway_task_reaping() {
    let evidence =
        run_task_reaping_suite::<GatewayTaskReapingAdapter>("gateway-backend-task-supervisor");
    assert_eq!(evidence.suite(), "task-reaping");
}

#[test]
fn w1_contract_gateway_full_lifecycle_manifest() {
    let evidence: Vec<CoverageEvidence<FteGatewayLifecycle>> = vec![
        run_transition_chain_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_registry_identity_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_attempt_hierarchy_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_consumer_cancellation_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_terminal_authority_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_waiter_control_suite::<GatewayRegistryAdapter>("gateway-operation-registry"),
        run_admission_quiesce_shutdown_bridge_suite::<GatewayAdmissionBridgeAdapter>(
            "gateway-admission-backend-shutdown",
        ),
        run_progress_shutdown_bridge_suite::<GatewayProgressBridgeAdapter>(
            "gateway-progress-backend-shutdown",
        ),
        run_panic_shutdown_bridge_suite::<GatewayPanicBridgeAdapter>(
            "gateway-panic-supervisor-shutdown",
        ),
        run_stable_shutdown_suite::<GatewayStableShutdownAdapter>("gateway-supervisor"),
        run_task_reaping_suite::<GatewayTaskReapingAdapter>("gateway-backend-task-supervisor"),
    ];
    let manifest = LifecycleCoverageManifest::<FteGatewayLifecycle>::accept(evidence)
        .expect("all eleven suites bind one real Gateway implementation");
    assert_eq!(manifest.product(), "free-token-energy");
    assert_eq!(manifest.implementation(), "gateway-v2");
    assert_eq!(manifest.covered().count(), 18);
}

#[test]
fn w1_contract_local_only_privacy_matches_real_router_without_network() {
    let request = local_request();
    let local = descriptor("local-fixture", BackendLocation::LocalEmbedded);
    let hosted = descriptor("hosted-fixture", BackendLocation::Hosted);
    assert_eq!(
        candidate_allowed(&request, &local, &local.models[0], None),
        Ok(())
    );
    assert_eq!(
        candidate_allowed(&request, &hosted, &hosted.models[0], None),
        Err("privacy_local_only")
    );

    let policy = local_only_contract_policy();
    policy.validate().expect("local-only policy must validate");
    assert_eq!(
        policy.decide(&RoutePrivacyContextV0 {
            target: RouteTargetV0::Local,
            data_tier: DataTierV0::Private,
            redaction: RedactionStateV0::NotApplied,
        }),
        PrivacyDecisionV0::Allowed
    );
    assert_eq!(
        policy.decide(&RoutePrivacyContextV0 {
            target: RouteTargetV0::Hosted {
                provider_id: ProviderId::new("hosted-fixture").expect("provider ID must validate"),
            },
            data_tier: DataTierV0::Private,
            redaction: RedactionStateV0::NotApplied,
        }),
        PrivacyDecisionV0::Denied(PrivacyDenialV0::LocalOnlyBoundary)
    );
}

#[test]
fn w1_contract_capability_envelope_is_derived_from_gateway_inventory() {
    let gateway = Gateway::new(GatewayDefaults::default());
    gateway
        .register_backend(Arc::new(SnapshotBackend {
            descriptor: descriptor("local-fixture", BackendLocation::LocalEmbedded),
            readiness: BackendReadiness::Ready,
        }))
        .expect("register local inventory fixture");
    gateway
        .register_backend(Arc::new(SnapshotBackend {
            descriptor: descriptor("hosted-fixture", BackendLocation::Hosted),
            readiness: BackendReadiness::NotConfigured {
                reason: "fixture credential is absent".to_owned(),
            },
        }))
        .expect("register hosted inventory fixture");

    let snapshot = capability_snapshot(&gateway);
    validate_capability_snapshot_v0(&snapshot).expect("exact capability envelope must validate");
    let entries = snapshot.services.values().flatten().collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| {
        entry.backend_or_resource_id == "hosted-fixture/hosted-fixture-model"
            && entry.readiness == Readiness::Unavailable
            && entry.privacy_eligible == TriState::No
    }));
}

#[test]
fn w1_contract_service_error_wraps_real_privacy_rejection() {
    let gateway = Gateway::new(GatewayDefaults::default());
    gateway
        .register_backend(Arc::new(SnapshotBackend {
            descriptor: descriptor("hosted-fixture", BackendLocation::Hosted),
            readiness: BackendReadiness::Ready,
        }))
        .expect("register hosted fixture");
    let error = gateway
        .resolve(&local_request())
        .err()
        .expect("local-only routing must reject the hosted fixture");
    assert_eq!(error.class, ErrorClass::Privacy);

    let contract_error = service_error(&error);
    validate_service_error_v0(&contract_error).expect("exact service error must validate");
    assert_eq!(contract_error.class, contracts::ErrorClass::Privacy);
    assert_eq!(contract_error.retry, RetryAdvice::Never);
}
