use super::*;
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
use platform_contract_testkit::{
    AttemptIdentity, ClosedFacts, LifecycleCoverageManifest, LifecycleImplementation,
    LifecyclePhase, OperationPhase, OperationSnapshot, ShutdownOutcome, TerminalClass,
    TerminalRecord, WaitObservation,
};

pub enum NativeRequestLifecycle {}

impl LifecycleImplementation for NativeRequestLifecycle {
    const PRODUCT: &'static str = "llama-native-kit";
    const IMPLEMENTATION: &'static str = "request-registry-owner-v1";
}

#[derive(Clone)]
struct NativeAdapter {
    registry: Arc<RequestRegistry>,
}

impl NativeAdapter {
    fn deterministic_with(next_sequence: u64, progress_capacity: usize) -> Self {
        Self {
            registry: Arc::new(RequestRegistry::with_config(
                next_sequence,
                progress_capacity,
            )),
        }
    }

    fn deterministic() -> Self {
        Self::deterministic_with(1, 4)
    }

    fn start_controlled(&self, operation_id: &str) -> NativeResult<ControlledRequest> {
        self.registry.spawn_controlled(operation_id)
    }

    fn request_release(
        &self,
        operation: &ControlledRequest,
        class: RequestTerminalClass,
    ) -> NativeResult<()> {
        self.registry.request_controlled_terminal(operation, class)
    }

    fn wait_released(
        &self,
        operation: &ControlledRequest,
        timeout: Duration,
    ) -> NativeResult<OperationSnapshot> {
        self.registry
            .wait_controlled_released(operation, timeout)
            .map(convert_snapshot)
    }

    fn begin_shutdown(&self) -> NativeShutdownWitness {
        NativeShutdownWitness::start(Arc::clone(&self.registry))
    }

    fn shutdown_facts(&self) -> ClosedFacts {
        convert_shutdown(self.registry.shutdown()).facts
    }
}

struct NativeShutdownWitness {
    started: mpsc::Receiver<()>,
    result: mpsc::Receiver<ShutdownOutcome>,
    join: Option<JoinHandle<()>>,
}

impl NativeShutdownWitness {
    fn start(registry: Arc<RequestRegistry>) -> Self {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name("native-w1-shutdown-witness".to_owned())
            .spawn(move || {
                registry.begin_quiesce_and_cancel_all();
                let _ = started_tx.send(());
                let _ = result_tx.send(convert_shutdown(registry.shutdown()));
            })
            .expect("start native shutdown witness");
        Self {
            started: started_rx,
            result: result_rx,
            join: Some(join),
        }
    }
}

impl ShutdownWitness for NativeShutdownWitness {
    type Error = String;

    fn wait_started(&self, timeout: Duration) -> Result<(), Self::Error> {
        self.started
            .recv_timeout(timeout)
            .map_err(|error| error.to_string())
    }

    fn try_complete(&self) -> Result<Option<ShutdownOutcome>, Self::Error> {
        match self.result.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("native shutdown witness disconnected".to_owned())
            }
        }
    }

    fn wait(mut self, timeout: Duration) -> Result<ShutdownOutcome, Self::Error> {
        let result = self
            .result
            .recv_timeout(timeout)
            .map_err(|error| error.to_string())?;
        if let Some(join) = self.join.take() {
            join.join()
                .map_err(|_| "native shutdown witness panicked".to_owned())?;
        }
        Ok(result)
    }
}

#[derive(Clone)]
struct NativeTicket {
    entry: Arc<ActiveRequest>,
}

impl Drop for NativeTicket {
    fn drop(&mut self) {
        self.entry.cancel_all();
    }
}

struct DirectOperation {
    _ticket: NativeTicket,
    lease: RequestLease,
}

fn reserve_direct(
    adapter: &NativeAdapter,
    operation_id: &str,
) -> NativeResult<(NativeTicket, RequestLease)> {
    let (entry, lease) = adapter.registry.reserve(
        operation_id,
        RequestClass::Embedding,
        RequestControls::Embedding {
            cancellation: Arc::new(AtomicBool::new(false)),
        },
    )?;
    Ok((NativeTicket { entry }, lease))
}

fn start_direct(
    adapter: &NativeAdapter,
    operation_id: &str,
) -> NativeResult<(NativeTicket, RequestLease)> {
    let (ticket, lease) = reserve_direct(adapter, operation_id)?;
    adapter.registry.queue(&lease)?;
    adapter.registry.start(&lease)?;
    Ok((ticket, lease))
}

fn finish_direct(
    adapter: &NativeAdapter,
    lease: &RequestLease,
    class: RequestTerminalClass,
) -> NativeResult<()> {
    adapter.registry.terminal(lease, class)?;
    adapter.registry.release(lease)
}

fn convert_identity(identity: RequestIdentity) -> AttemptIdentity {
    AttemptIdentity {
        operation_id: identity.operation_id,
        attempt_id: identity.attempt_id,
        sequence: identity.sequence,
    }
}

fn native_terminal(class: TerminalClass) -> RequestTerminalClass {
    match class {
        TerminalClass::Completed => RequestTerminalClass::Completed,
        TerminalClass::Cancelled => RequestTerminalClass::Cancelled,
        TerminalClass::Failed => RequestTerminalClass::Failed,
    }
}

fn contract_terminal(class: RequestTerminalClass) -> TerminalClass {
    match class {
        RequestTerminalClass::Completed => TerminalClass::Completed,
        RequestTerminalClass::Cancelled => TerminalClass::Cancelled,
        RequestTerminalClass::Failed => TerminalClass::Failed,
    }
}

fn convert_phase(phase: RequestRegistryPhase) -> LifecyclePhase {
    match phase {
        RequestRegistryPhase::Running => LifecyclePhase::Running,
        RequestRegistryPhase::Quiescing => LifecyclePhase::Quiescing,
        RequestRegistryPhase::Closed => LifecyclePhase::Closed,
    }
}

fn convert_snapshot(snapshot: RequestSnapshot) -> OperationSnapshot {
    OperationSnapshot {
        identity: convert_identity(snapshot.identity),
        phase: match snapshot.phase {
            RequestPhase::Reserved => OperationPhase::Reserved,
            RequestPhase::Queued => OperationPhase::Queued,
            RequestPhase::Running => OperationPhase::Running,
            RequestPhase::Terminal => OperationPhase::Terminal,
            RequestPhase::Released => OperationPhase::Released,
        },
        cancellation_requested: snapshot.cancellation_requested,
        authoritative_terminal: snapshot
            .authoritative_terminal
            .map(|terminal| TerminalRecord {
                class: contract_terminal(terminal.class),
                sequence: terminal.sequence,
            }),
        final_projection: snapshot.final_projection.map(|terminal| TerminalRecord {
            class: contract_terminal(terminal.class),
            sequence: terminal.sequence,
        }),
        progress_projection: snapshot.progress_projection,
    }
}

fn convert_shutdown(outcome: RegistryShutdownOutcome) -> ShutdownOutcome {
    ShutdownOutcome {
        facts: ClosedFacts {
            lifecycle: convert_phase(outcome.phase),
            active_operations: outcome.active_operations,
            retained_tasks: outcome.retained_tasks,
            expected_workers: outcome.expected_workers,
            joined_workers: outcome.joined_workers,
        },
        expected_worker_ids: outcome.expected_worker_ids,
        joined_worker_ids: outcome.joined_worker_ids,
    }
}

impl TransitionChainAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type Operation = DirectOperation;

    fn deterministic() -> Self {
        NativeAdapter::deterministic()
    }
    fn reserve(&self, operation_id: &str) -> NativeResult<Self::Operation> {
        let (ticket, lease) = reserve_direct(self, operation_id)?;
        Ok(DirectOperation {
            _ticket: ticket,
            lease,
        })
    }
    fn phase(&self, operation: &Self::Operation) -> Option<OperationPhase> {
        self.registry
            .snapshot(&operation.lease)
            .map(convert_snapshot)
            .map(|snapshot| snapshot.phase)
    }
    fn queue(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.registry.queue(&operation.lease)
    }
    fn start(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.registry.start(&operation.lease)
    }
    fn terminal(&self, operation: &Self::Operation, class: TerminalClass) -> NativeResult<()> {
        self.registry
            .terminal(&operation.lease, native_terminal(class))
    }
    fn release(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.registry.release(&operation.lease)
    }
}

impl RegistryIdentityAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type Guard = NativeTicket;
    type Lease = RequestLease;

    fn deterministic(next_sequence: u64) -> Self {
        Self::deterministic_with(next_sequence, 4)
    }
    fn reserve(&self, operation_id: &str) -> NativeResult<(Self::Guard, Self::Lease)> {
        reserve_direct(self, operation_id)
    }
    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity {
        convert_identity(lease.entry.identity())
    }
    fn complete_and_release(&self, lease: &Self::Lease) -> NativeResult<()> {
        self.registry.queue(lease)?;
        self.registry.start(lease)?;
        finish_direct(self, lease, RequestTerminalClass::Completed)
    }
    fn active_count(&self) -> usize {
        self.registry.active_count()
    }
    fn current_identity(&self, operation_id: &str) -> Option<AttemptIdentity> {
        self.registry
            .current_snapshot(operation_id)
            .map(|snapshot| convert_identity(snapshot.identity))
    }
}

impl AttemptHierarchyAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type Operation = RequestOperation;
    type Attempt = RequestLease;

    fn deterministic() -> Self {
        NativeAdapter::deterministic()
    }
    fn create_operation(&self, operation_id: &str) -> NativeResult<Self::Operation> {
        self.registry.create_operation(operation_id)
    }
    fn start_attempt(&self, operation: &Self::Operation) -> NativeResult<Self::Attempt> {
        self.registry.start_attempt(operation)
    }
    fn attempt_identity(&self, attempt: &Self::Attempt) -> AttemptIdentity {
        convert_identity(attempt.entry.identity())
    }
    fn operation_active(&self, operation: &Self::Operation) -> bool {
        self.registry.operation_active(operation)
    }
    fn active_attempts(&self, operation: &Self::Operation) -> Vec<AttemptIdentity> {
        self.registry
            .operation_attempts(operation)
            .into_iter()
            .map(convert_identity)
            .collect()
    }
    fn request_operation_cancel(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.registry.request_operation_cancel(operation)
    }
    fn cancellation_requested(&self, attempt: &Self::Attempt) -> bool {
        attempt.entry.controls.cancellation_requested()
    }
    fn finish_attempt(&self, attempt: Self::Attempt) -> NativeResult<()> {
        let class = if attempt.entry.controls.cancellation_requested() {
            RequestTerminalClass::Cancelled
        } else {
            RequestTerminalClass::Completed
        };
        finish_direct(self, &attempt, class)
    }
    fn finish_operation(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.registry.finish_operation(operation)
    }
}

impl ConsumerCancellationAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type Ticket = NativeTicket;
    type Lease = RequestLease;

    fn deterministic() -> Self {
        NativeAdapter::deterministic()
    }
    fn start(&self, operation_id: &str) -> NativeResult<(Self::Ticket, Self::Lease)> {
        start_direct(self, operation_id)
    }
    fn ticket_identity(&self, ticket: &Self::Ticket) -> AttemptIdentity {
        convert_identity(ticket.entry.identity())
    }
    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity {
        convert_identity(lease.entry.identity())
    }
    fn active_count(&self) -> usize {
        self.registry.active_count()
    }
    fn current_snapshot(&self, operation_id: &str) -> Option<OperationSnapshot> {
        self.registry
            .current_snapshot(operation_id)
            .map(convert_snapshot)
    }
    fn lease_snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        self.registry.snapshot(lease).map(convert_snapshot)
    }
    fn cancellation_requested(&self, lease: &Self::Lease) -> bool {
        lease.entry.controls.cancellation_requested()
    }
    fn explicit_consumer_drop(&self, ticket: Self::Ticket) -> NativeResult<()> {
        drop(ticket);
        Ok(())
    }
    fn finish_cancelled(&self, lease: &Self::Lease) -> NativeResult<()> {
        finish_direct(self, lease, RequestTerminalClass::Cancelled)
    }
}

impl TerminalAuthorityAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type Guard = NativeTicket;
    type Lease = RequestLease;

    fn deterministic() -> Self {
        NativeAdapter::deterministic()
    }
    fn start(&self, operation_id: &str) -> NativeResult<(Self::Guard, Self::Lease)> {
        start_direct(self, operation_id)
    }
    fn terminal(&self, lease: &Self::Lease, class: TerminalClass) -> NativeResult<()> {
        self.registry.terminal(lease, native_terminal(class))
    }
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        self.registry.snapshot(lease).map(convert_snapshot)
    }
    fn release(&self, lease: &Self::Lease) -> NativeResult<()> {
        self.registry.release(lease)
    }
}

impl WaiterControlAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type Ticket = NativeTicket;
    type Lease = RequestLease;

    fn deterministic() -> Self {
        NativeAdapter::deterministic()
    }
    fn start(&self, operation_id: &str) -> NativeResult<(Self::Ticket, Self::Lease)> {
        start_direct(self, operation_id)
    }
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        self.registry.snapshot(lease).map(convert_snapshot)
    }
    fn waiter_timeout(&self, ticket: &Self::Ticket) -> NativeResult<WaitObservation> {
        if self
            .registry
            .wait_terminal_timeout(&ticket.entry, Duration::ZERO)
        {
            Err(registry_error(
                "native request unexpectedly reached terminal",
            ))
        } else {
            Ok(WaitObservation::TimedOut)
        }
    }
    fn request_cancel(&self, ticket: &Self::Ticket) -> NativeResult<()> {
        ticket.entry.cancel_all();
        Ok(())
    }
    fn finish_cancelled(&self, lease: &Self::Lease) -> NativeResult<()> {
        finish_direct(self, lease, RequestTerminalClass::Cancelled)
    }
}

impl AdmissionQuiesceShutdownBridgeAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type Operation = ControlledRequest;
    type ShutdownWitness = NativeShutdownWitness;

    fn deterministic() -> Self {
        NativeAdapter::deterministic()
    }
    fn reserve(&self, operation_id: &str) -> NativeResult<Self::Operation> {
        self.start_controlled(operation_id)
    }
    fn quiesce(&self) {
        self.registry.begin_quiesce_and_cancel_all();
    }
    fn phase(&self) -> LifecyclePhase {
        convert_phase(self.registry.phase())
    }
    fn active_count(&self) -> usize {
        self.registry.active_count()
    }
    fn retained_task_count(&self) -> usize {
        self.registry.retained_task_count()
    }
    fn cancellation_requested(&self, operation_id: &str) -> bool {
        self.registry.cancellation_requested_by_id(operation_id)
    }
    fn request_cancelled_release(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.request_release(operation, RequestTerminalClass::Cancelled)
    }
    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> NativeResult<OperationSnapshot> {
        NativeAdapter::wait_released(self, operation, timeout)
    }
    fn allow_worker_exit(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.registry.allow_controlled_exit(operation)
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        NativeAdapter::begin_shutdown(self)
    }
    fn shutdown(&self) -> ClosedFacts {
        self.shutdown_facts()
    }
}

impl ProgressShutdownBridgeAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type UnreadProgress = ();
    type Operation = ControlledRequest;
    type ShutdownWitness = NativeShutdownWitness;

    fn deterministic(progress_capacity: usize) -> Self {
        Self::deterministic_with(1, progress_capacity)
    }
    fn start(&self, operation_id: &str) -> NativeResult<((), Self::Operation)> {
        Ok(((), self.start_controlled(operation_id)?))
    }
    fn publish_progress(&self, operation: &Self::Operation, sequence: u64) -> NativeResult<()> {
        self.registry
            .publish_controlled_progress(operation, sequence)
    }
    fn snapshot(&self, operation: &Self::Operation) -> Option<OperationSnapshot> {
        Some(convert_snapshot(
            self.registry.controlled_snapshot(operation),
        ))
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        NativeAdapter::begin_shutdown(self)
    }
    fn request_completed_release(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.request_release(operation, RequestTerminalClass::Completed)
    }
    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> NativeResult<OperationSnapshot> {
        NativeAdapter::wait_released(self, operation, timeout)
    }
    fn allow_worker_exit(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.registry.allow_controlled_exit(operation)
    }
    fn progress_capacity(&self) -> usize {
        self.registry.progress_capacity()
    }
}

impl PanicShutdownBridgeAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type Operation = ControlledRequest;
    type ShutdownWitness = NativeShutdownWitness;

    fn deterministic() -> Self {
        NativeAdapter::deterministic()
    }
    fn run_controlled_panicking_operation(
        &self,
        operation_id: &str,
    ) -> NativeResult<Self::Operation> {
        self.registry.spawn_panicking(operation_id)
    }
    fn wait_failed_release(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> NativeResult<OperationSnapshot> {
        NativeAdapter::wait_released(self, operation, timeout)
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        NativeAdapter::begin_shutdown(self)
    }
}

impl StableShutdownAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type Operation = ControlledRequest;
    type ShutdownWitness = NativeShutdownWitness;

    fn deterministic() -> Self {
        NativeAdapter::deterministic()
    }
    fn start(&self, operation_id: &str) -> NativeResult<Self::Operation> {
        self.start_controlled(operation_id)
    }
    fn request_completed_release(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.request_release(operation, RequestTerminalClass::Completed)
    }
    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> NativeResult<OperationSnapshot> {
        NativeAdapter::wait_released(self, operation, timeout)
    }
    fn allow_worker_exit(&self, operation: &Self::Operation) -> NativeResult<()> {
        self.registry.allow_controlled_exit(operation)
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        NativeAdapter::begin_shutdown(self)
    }
}

impl TaskReapingAdapter for NativeAdapter {
    type Implementation = NativeRequestLifecycle;
    type Error = NativeError;
    type Operation = ControlledRequest;

    fn deterministic() -> Self {
        NativeAdapter::deterministic()
    }
    fn start(&self, operation_id: &str) -> NativeResult<Self::Operation> {
        self.start_controlled(operation_id)
    }
    fn finish(&self, operation: Self::Operation) -> NativeResult<()> {
        self.request_release(&operation, RequestTerminalClass::Completed)?;
        let _ = NativeAdapter::wait_released(self, &operation, Duration::from_secs(5))?;
        self.registry.allow_controlled_exit(&operation)?;
        self.registry.reap_controlled(&operation)
    }
    fn active_count(&self) -> usize {
        self.registry.active_count()
    }
    fn retained_task_count(&self) -> usize {
        self.registry.retained_task_count()
    }
    fn shutdown(&self) -> ClosedFacts {
        self.shutdown_facts()
    }
}

#[test]
fn real_native_request_owner_satisfies_complete_compositional_manifest() {
    let evidence = vec![
        run_transition_chain_suite::<NativeAdapter>("native-request-registry"),
        run_registry_identity_suite::<NativeAdapter>("native-request-registry"),
        run_attempt_hierarchy_suite::<NativeAdapter>("native-request-registry"),
        run_consumer_cancellation_suite::<NativeAdapter>("native-request-control"),
        run_terminal_authority_suite::<NativeAdapter>("native-terminal-authority"),
        run_waiter_control_suite::<NativeAdapter>("native-request-control"),
        run_admission_quiesce_shutdown_bridge_suite::<NativeAdapter>("native-owner-shutdown"),
        run_progress_shutdown_bridge_suite::<NativeAdapter>("native-progress-shutdown"),
        run_panic_shutdown_bridge_suite::<NativeAdapter>("native-panic-shutdown"),
        run_stable_shutdown_suite::<NativeAdapter>("native-owner-shutdown"),
        run_task_reaping_suite::<NativeAdapter>("native-task-supervisor"),
    ];
    let manifest = LifecycleCoverageManifest::<NativeRequestLifecycle>::accept(evidence)
        .expect("all eleven native ownership suites");
    assert_eq!(manifest.product(), "llama-native-kit");
    assert_eq!(manifest.implementation(), "request-registry-owner-v1");
    assert_eq!(manifest.covered().count(), 18);
}
