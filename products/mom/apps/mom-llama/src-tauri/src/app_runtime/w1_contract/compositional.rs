use super::super::{
    AppRuntimeHandle, GatewayFinalizer, NativeFinalizer, ProductCanceller, ProductGatewayFinalizer,
};
use crate::operation_supervisor::{
    AttemptIdentity as MomIdentity, ControlledOperation, LifecyclePhase as MomLifecyclePhase,
    OperationHandle, OperationLease, OperationPhase as MomOperationPhase, OperationReservation,
    OperationSnapshot as MomSnapshot, OperationSupervisor, OperationTicket,
    TerminalClass as MomTerminalClass,
};
use fte_backend_llama::LlamaNativeBackend;
use fte_router::{Gateway, GatewayDefaults};
use llama_native_host::{NativeHost, NativeHostConfig, ProcessExitJoinedNativeHost};
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
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

pub enum MomOperationLifecycle {}

impl LifecycleImplementation for MomOperationLifecycle {
    const PRODUCT: &'static str = "mom-llama";
    const IMPLEMENTATION: &'static str = "operation-supervisor-v1";
}

#[derive(Clone)]
struct MomAdapter {
    runtime: AppRuntimeHandle,
    supervisor: OperationSupervisor,
}

struct DirectNativeFinalizer;

impl NativeFinalizer for DirectNativeFinalizer {
    fn shutdown(
        &self,
        host: &Arc<NativeHost>,
    ) -> Result<ProcessExitJoinedNativeHost, mom_llama_runtime::ProductShutdownError> {
        Ok(host.shutdown_for_process_exit())
    }
}

struct NoopProductCanceller;

impl ProductCanceller for NoopProductCanceller {
    fn cancel_all(&self) -> usize {
        0
    }
}

impl MomAdapter {
    fn deterministic_with(next_sequence: u64, progress_capacity: usize) -> Self {
        let supervisor = OperationSupervisor::with_config(next_sequence, progress_capacity);
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let gateway = Arc::new(Gateway::new(GatewayDefaults {
            catalog_version: "w1-compositional-test".to_owned(),
        }));
        let runtime = AppRuntimeHandle::with_operation_supervisor(
            Arc::new(ProductGatewayFinalizer(gateway)) as Arc<dyn GatewayFinalizer>,
            Arc::new(LlamaNativeBackend::new_borrowed(Arc::clone(&host))),
            host,
            None,
            Arc::new(NoopProductCanceller),
            Arc::new(DirectNativeFinalizer),
            supervisor.clone(),
        );
        Self {
            runtime,
            supervisor,
        }
    }

    fn deterministic() -> Self {
        Self::deterministic_with(1, 4)
    }

    fn start_controlled(&self, operation_id: &str) -> Result<ControlledOperation, String> {
        self.supervisor
            .spawn_controlled(operation_id)
            .map_err(|error| error.to_string())
    }

    fn request_release(
        &self,
        operation: &ControlledOperation,
        class: MomTerminalClass,
    ) -> Result<(), String> {
        self.supervisor
            .request_controlled_terminal(operation, class)
            .map_err(|error| error.to_string())
    }

    fn wait_released(
        &self,
        operation: &ControlledOperation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, String> {
        self.supervisor
            .wait_controlled_released(operation, timeout)
            .map(convert_snapshot)
            .map_err(|error| error.to_string())
    }

    fn allow_exit(&self, operation: &ControlledOperation) -> Result<(), String> {
        self.supervisor
            .allow_controlled_exit(operation)
            .map_err(|error| error.to_string())
    }

    fn begin_shutdown(&self) -> MomShutdownWitness {
        MomShutdownWitness::start(self.runtime.clone())
    }

    fn shutdown_facts(&self) -> ClosedFacts {
        self.begin_shutdown()
            .wait(Duration::from_secs(5))
            .expect("real Mom shutdown")
            .facts
    }
}

struct MomShutdownWitness {
    started: mpsc::Receiver<()>,
    result: mpsc::Receiver<Result<ShutdownOutcome, String>>,
    join: Option<thread::JoinHandle<()>>,
}

impl MomShutdownWitness {
    fn start(runtime: AppRuntimeHandle) -> Self {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name("mom-w1-shutdown-witness".to_owned())
            .spawn(move || {
                runtime.begin_quiesce();
                let _ = started_tx.send(());
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())
                    .and_then(|tokio| {
                        tokio
                            .block_on(runtime.shutdown())
                            .map(convert_shutdown)
                            .map_err(|error| error.to_string())
                    });
                let _ = result_tx.send(result);
            })
            .expect("start Mom shutdown witness");
        Self {
            started: started_rx,
            result: result_rx,
            join: Some(join),
        }
    }
}

impl ShutdownWitness for MomShutdownWitness {
    type Error = String;

    fn wait_started(&self, timeout: Duration) -> Result<(), Self::Error> {
        self.started
            .recv_timeout(timeout)
            .map_err(|error| error.to_string())
    }

    fn try_complete(&self) -> Result<Option<ShutdownOutcome>, Self::Error> {
        match self.result.try_recv() {
            Ok(result) => result.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Mom shutdown witness disconnected".to_owned())
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
                .map_err(|_| "Mom shutdown witness panicked".to_owned())?;
        }
        result
    }
}

fn convert_shutdown(summary: super::super::AppShutdownSummary) -> ShutdownOutcome {
    ShutdownOutcome {
        facts: ClosedFacts {
            lifecycle: convert_lifecycle(summary.operation_supervisor_phase),
            active_operations: summary.active_operation_count,
            retained_tasks: summary.retained_operation_task_count,
            expected_workers: summary.expected_worker_ids.len(),
            joined_workers: summary.joined_worker_ids.len(),
        },
        expected_worker_ids: summary.expected_worker_ids,
        joined_worker_ids: summary.joined_worker_ids,
    }
}

fn convert_lifecycle(phase: MomLifecyclePhase) -> LifecyclePhase {
    match phase {
        MomLifecyclePhase::Running => LifecyclePhase::Running,
        MomLifecyclePhase::Quiescing => LifecyclePhase::Quiescing,
        MomLifecyclePhase::Closed => LifecyclePhase::Closed,
    }
}

fn convert_identity(identity: MomIdentity) -> AttemptIdentity {
    AttemptIdentity {
        operation_id: identity.operation_id,
        attempt_id: identity.attempt_id,
        sequence: identity.sequence,
    }
}

fn convert_terminal(class: TerminalClass) -> MomTerminalClass {
    match class {
        TerminalClass::Completed => MomTerminalClass::Completed,
        TerminalClass::Cancelled => MomTerminalClass::Cancelled,
        TerminalClass::Failed => MomTerminalClass::Failed,
    }
}

fn contract_terminal(class: MomTerminalClass) -> TerminalClass {
    match class {
        MomTerminalClass::Completed => TerminalClass::Completed,
        MomTerminalClass::Cancelled => TerminalClass::Cancelled,
        MomTerminalClass::Failed => TerminalClass::Failed,
    }
}

fn convert_snapshot(snapshot: MomSnapshot) -> OperationSnapshot {
    OperationSnapshot {
        identity: convert_identity(snapshot.identity),
        phase: match snapshot.phase {
            MomOperationPhase::Reserved => OperationPhase::Reserved,
            MomOperationPhase::Queued => OperationPhase::Queued,
            MomOperationPhase::Running => OperationPhase::Running,
            MomOperationPhase::Terminal => OperationPhase::Terminal,
            MomOperationPhase::Released => OperationPhase::Released,
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

fn lifecycle(phase: MomLifecyclePhase) -> LifecyclePhase {
    match phase {
        MomLifecyclePhase::Running => LifecyclePhase::Running,
        MomLifecyclePhase::Quiescing => LifecyclePhase::Quiescing,
        MomLifecyclePhase::Closed => LifecyclePhase::Closed,
    }
}

struct DirectOperation {
    _ticket: OperationTicket,
    lease: OperationLease,
}

impl TransitionChainAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type Operation = DirectOperation;

    fn deterministic() -> Self {
        MomAdapter::deterministic()
    }
    fn reserve(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        let OperationReservation { ticket, lease } = self
            .supervisor
            .reserve(operation_id)
            .map_err(|error| error.to_string())?;
        Ok(DirectOperation {
            _ticket: ticket,
            lease,
        })
    }
    fn phase(&self, operation: &Self::Operation) -> Option<OperationPhase> {
        self.supervisor
            .snapshot(&operation.lease)
            .map(convert_snapshot)
            .map(|snapshot| snapshot.phase)
    }
    fn queue(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.supervisor
            .queue(&operation.lease)
            .map_err(|error| error.to_string())
    }
    fn start(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.supervisor
            .start(&operation.lease)
            .map_err(|error| error.to_string())
    }
    fn terminal(
        &self,
        operation: &Self::Operation,
        class: TerminalClass,
    ) -> Result<(), Self::Error> {
        self.supervisor
            .terminal(&operation.lease, convert_terminal(class))
            .map_err(|error| error.to_string())
    }
    fn release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.supervisor
            .release(&operation.lease)
            .map_err(|error| error.to_string())
    }
}

impl RegistryIdentityAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type Guard = OperationTicket;
    type Lease = OperationLease;

    fn deterministic(next_sequence: u64) -> Self {
        Self::deterministic_with(next_sequence, 4)
    }
    fn reserve(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error> {
        let OperationReservation { ticket, lease } = self
            .supervisor
            .reserve(operation_id)
            .map_err(|error| error.to_string())?;
        Ok((ticket, lease))
    }
    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity {
        convert_identity(lease.identity())
    }
    fn complete_and_release(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        self.supervisor
            .queue(lease)
            .and_then(|()| self.supervisor.start(lease))
            .and_then(|()| self.supervisor.terminal(lease, MomTerminalClass::Completed))
            .and_then(|()| self.supervisor.release(lease))
            .map_err(|error| error.to_string())
    }
    fn active_count(&self) -> usize {
        self.supervisor.active_count()
    }
    fn current_identity(&self, operation_id: &str) -> Option<AttemptIdentity> {
        self.supervisor
            .current_identity(operation_id)
            .map(convert_identity)
    }
}

impl AttemptHierarchyAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type Operation = OperationHandle;
    type Attempt = OperationLease;

    fn deterministic() -> Self {
        MomAdapter::deterministic()
    }
    fn create_operation(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        self.supervisor
            .create_operation(operation_id)
            .map_err(|error| error.to_string())
    }
    fn start_attempt(&self, operation: &Self::Operation) -> Result<Self::Attempt, Self::Error> {
        self.supervisor
            .start_attempt(operation)
            .map_err(|error| error.to_string())
    }
    fn attempt_identity(&self, attempt: &Self::Attempt) -> AttemptIdentity {
        convert_identity(attempt.identity())
    }
    fn operation_active(&self, operation: &Self::Operation) -> bool {
        self.supervisor.operation_active(operation)
    }
    fn active_attempts(&self, operation: &Self::Operation) -> Vec<AttemptIdentity> {
        self.supervisor
            .active_attempts(operation)
            .into_iter()
            .map(convert_identity)
            .collect()
    }
    fn request_operation_cancel(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.supervisor
            .request_operation_cancel(operation)
            .map_err(|error| error.to_string())
    }
    fn cancellation_requested(&self, attempt: &Self::Attempt) -> bool {
        self.supervisor.cancellation_requested(attempt)
    }
    fn finish_attempt(&self, attempt: Self::Attempt) -> Result<(), Self::Error> {
        self.supervisor
            .finish_attempt(&attempt)
            .map_err(|error| error.to_string())
    }
    fn finish_operation(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.supervisor
            .finish_operation(operation)
            .map_err(|error| error.to_string())
    }
}

fn start_direct(
    adapter: &MomAdapter,
    operation_id: &str,
) -> Result<(OperationTicket, OperationLease), String> {
    let OperationReservation { ticket, lease } = adapter
        .supervisor
        .reserve(operation_id)
        .map_err(|error| error.to_string())?;
    adapter
        .supervisor
        .queue(&lease)
        .and_then(|()| adapter.supervisor.start(&lease))
        .map_err(|error| error.to_string())?;
    Ok((ticket, lease))
}

fn finish_direct(
    adapter: &MomAdapter,
    lease: &OperationLease,
    class: MomTerminalClass,
) -> Result<(), String> {
    adapter
        .supervisor
        .terminal(lease, class)
        .and_then(|()| adapter.supervisor.release(lease))
        .map_err(|error| error.to_string())
}

impl ConsumerCancellationAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type Ticket = OperationTicket;
    type Lease = OperationLease;

    fn deterministic() -> Self {
        MomAdapter::deterministic()
    }
    fn start(&self, operation_id: &str) -> Result<(Self::Ticket, Self::Lease), Self::Error> {
        start_direct(self, operation_id)
    }
    fn ticket_identity(&self, ticket: &Self::Ticket) -> AttemptIdentity {
        convert_identity(ticket.identity())
    }
    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity {
        convert_identity(lease.identity())
    }
    fn active_count(&self) -> usize {
        self.supervisor.active_count()
    }
    fn current_snapshot(&self, operation_id: &str) -> Option<OperationSnapshot> {
        self.supervisor
            .current_snapshot(operation_id)
            .map(convert_snapshot)
    }
    fn lease_snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        self.supervisor.snapshot(lease).map(convert_snapshot)
    }
    fn cancellation_requested(&self, lease: &Self::Lease) -> bool {
        self.supervisor.cancellation_requested(lease)
    }
    fn explicit_consumer_drop(&self, ticket: Self::Ticket) -> Result<(), Self::Error> {
        drop(ticket);
        Ok(())
    }
    fn finish_cancelled(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        finish_direct(self, lease, MomTerminalClass::Cancelled)
    }
}

impl TerminalAuthorityAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type Guard = OperationTicket;
    type Lease = OperationLease;

    fn deterministic() -> Self {
        MomAdapter::deterministic()
    }
    fn start(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error> {
        start_direct(self, operation_id)
    }
    fn terminal(&self, lease: &Self::Lease, class: TerminalClass) -> Result<(), Self::Error> {
        self.supervisor
            .terminal(lease, convert_terminal(class))
            .map_err(|error| error.to_string())
    }
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        self.supervisor.snapshot(lease).map(convert_snapshot)
    }
    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        self.supervisor
            .release(lease)
            .map_err(|error| error.to_string())
    }
}

impl WaiterControlAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type Ticket = OperationTicket;
    type Lease = OperationLease;

    fn deterministic() -> Self {
        MomAdapter::deterministic()
    }
    fn start(&self, operation_id: &str) -> Result<(Self::Ticket, Self::Lease), Self::Error> {
        start_direct(self, operation_id)
    }
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        self.supervisor.snapshot(lease).map(convert_snapshot)
    }
    fn waiter_timeout(&self, ticket: &Self::Ticket) -> Result<WaitObservation, Self::Error> {
        if ticket.wait_timeout(Duration::ZERO) {
            Err("operation unexpectedly reached terminal".to_owned())
        } else {
            Ok(WaitObservation::TimedOut)
        }
    }
    fn request_cancel(&self, ticket: &Self::Ticket) -> Result<(), Self::Error> {
        self.supervisor
            .request_cancel(ticket)
            .map_err(|error| error.to_string())
    }
    fn finish_cancelled(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        finish_direct(self, lease, MomTerminalClass::Cancelled)
    }
}

impl AdmissionQuiesceShutdownBridgeAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type Operation = ControlledOperation;
    type ShutdownWitness = MomShutdownWitness;

    fn deterministic() -> Self {
        MomAdapter::deterministic()
    }
    fn reserve(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        self.start_controlled(operation_id)
    }
    fn quiesce(&self) {
        self.runtime.begin_quiesce();
    }
    fn phase(&self) -> LifecyclePhase {
        lifecycle(self.supervisor.phase())
    }
    fn active_count(&self) -> usize {
        self.supervisor.active_count()
    }
    fn retained_task_count(&self) -> usize {
        self.supervisor.retained_task_count()
    }
    fn cancellation_requested(&self, operation_id: &str) -> bool {
        self.supervisor.cancellation_requested_by_id(operation_id)
    }
    fn request_cancelled_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.request_release(operation, MomTerminalClass::Cancelled)
    }
    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        MomAdapter::wait_released(self, operation, timeout)
    }
    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.allow_exit(operation)
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        MomAdapter::begin_shutdown(self)
    }
    fn shutdown(&self) -> ClosedFacts {
        self.shutdown_facts()
    }
}

impl ProgressShutdownBridgeAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type UnreadProgress = ();
    type Operation = ControlledOperation;
    type ShutdownWitness = MomShutdownWitness;

    fn deterministic(progress_capacity: usize) -> Self {
        Self::deterministic_with(1, progress_capacity)
    }
    fn start(
        &self,
        operation_id: &str,
    ) -> Result<(Self::UnreadProgress, Self::Operation), Self::Error> {
        Ok(((), self.start_controlled(operation_id)?))
    }
    fn publish_progress(
        &self,
        operation: &Self::Operation,
        sequence: u64,
    ) -> Result<(), Self::Error> {
        self.supervisor
            .publish_controlled_progress(operation, sequence)
            .map_err(|error| error.to_string())
    }
    fn snapshot(&self, operation: &Self::Operation) -> Option<OperationSnapshot> {
        self.supervisor
            .controlled_snapshot(operation)
            .map(convert_snapshot)
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        MomAdapter::begin_shutdown(self)
    }
    fn request_completed_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.request_release(operation, MomTerminalClass::Completed)
    }
    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        MomAdapter::wait_released(self, operation, timeout)
    }
    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.allow_exit(operation)
    }
    fn progress_capacity(&self) -> usize {
        self.supervisor.progress_capacity()
    }
}

impl PanicShutdownBridgeAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type Operation = ControlledOperation;
    type ShutdownWitness = MomShutdownWitness;

    fn deterministic() -> Self {
        MomAdapter::deterministic()
    }
    fn run_controlled_panicking_operation(
        &self,
        operation_id: &str,
    ) -> Result<Self::Operation, Self::Error> {
        self.supervisor
            .spawn_panicking(operation_id)
            .map_err(|error| error.to_string())
    }
    fn wait_failed_release(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        MomAdapter::wait_released(self, operation, timeout)
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        MomAdapter::begin_shutdown(self)
    }
}

impl StableShutdownAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type Operation = ControlledOperation;
    type ShutdownWitness = MomShutdownWitness;

    fn deterministic() -> Self {
        MomAdapter::deterministic()
    }
    fn start(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        self.start_controlled(operation_id)
    }
    fn request_completed_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.request_release(operation, MomTerminalClass::Completed)
    }
    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        MomAdapter::wait_released(self, operation, timeout)
    }
    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.allow_exit(operation)
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        MomAdapter::begin_shutdown(self)
    }
}

impl TaskReapingAdapter for MomAdapter {
    type Implementation = MomOperationLifecycle;
    type Error = String;
    type Operation = ControlledOperation;

    fn deterministic() -> Self {
        MomAdapter::deterministic()
    }
    fn start(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        self.start_controlled(operation_id)
    }
    fn finish(&self, operation: Self::Operation) -> Result<(), Self::Error> {
        self.request_release(&operation, MomTerminalClass::Completed)?;
        let _ = MomAdapter::wait_released(self, &operation, Duration::from_secs(5))?;
        self.allow_exit(&operation)?;
        self.supervisor
            .reap_controlled(&operation)
            .map_err(|error| error.to_string())
    }
    fn active_count(&self) -> usize {
        self.supervisor.active_count()
    }
    fn retained_task_count(&self) -> usize {
        self.supervisor.retained_task_count()
    }
    fn shutdown(&self) -> ClosedFacts {
        self.shutdown_facts()
    }
}

#[test]
fn real_mom_operation_supervisor_satisfies_complete_compositional_manifest() {
    let evidence = vec![
        run_transition_chain_suite::<MomAdapter>("mom-operation-registry"),
        run_registry_identity_suite::<MomAdapter>("mom-operation-registry"),
        run_attempt_hierarchy_suite::<MomAdapter>("mom-operation-registry"),
        run_consumer_cancellation_suite::<MomAdapter>("mom-operation-control"),
        run_terminal_authority_suite::<MomAdapter>("mom-terminal-authority"),
        run_waiter_control_suite::<MomAdapter>("mom-operation-control"),
        run_admission_quiesce_shutdown_bridge_suite::<MomAdapter>("mom-app-shutdown"),
        run_progress_shutdown_bridge_suite::<MomAdapter>("mom-progress-shutdown"),
        run_panic_shutdown_bridge_suite::<MomAdapter>("mom-panic-shutdown"),
        run_stable_shutdown_suite::<MomAdapter>("mom-app-shutdown"),
        run_task_reaping_suite::<MomAdapter>("mom-task-supervisor"),
    ];
    let manifest = LifecycleCoverageManifest::<MomOperationLifecycle>::accept(evidence)
        .expect("all eleven real Mom ownership suites");
    assert_eq!(manifest.product(), "mom-llama");
    assert_eq!(manifest.implementation(), "operation-supervisor-v1");
    assert_eq!(manifest.covered().count(), 18);
}
