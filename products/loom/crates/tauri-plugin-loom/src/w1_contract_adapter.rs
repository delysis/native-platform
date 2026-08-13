use super::*;
use loom_host::platform_contract_testkit;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use loom_host::w1_contract_adapter::{LoomInteractiveLifecycle, owned_lifecycle_evidence};
use loom_host::{
    GenerationOperationLease, GenerationOperationSnapshot, GenerationSupervisorClosedFacts,
    GenerationSupervisorError, GenerationSupervisorPhase,
};
use platform_contract_testkit::compositional_lifecycle::{
    AdmissionQuiesceShutdownBridgeAdapter, PanicShutdownBridgeAdapter,
    ProgressShutdownBridgeAdapter, ShutdownWitness, StableShutdownAdapter, TaskReapingAdapter,
    run_admission_quiesce_shutdown_bridge_suite, run_panic_shutdown_bridge_suite,
    run_progress_shutdown_bridge_suite, run_stable_shutdown_suite, run_task_reaping_suite,
};
use platform_contract_testkit::{
    ClosedFacts, LifecycleCoverageManifest, LifecycleInvariant, LifecyclePhase, OperationPhase,
    OperationSnapshot, ShutdownOutcome, TerminalClass, TerminalRecord,
};

#[derive(Debug)]
struct ContractCancellation;

impl ControlledGenerationWorkerCancellation for ContractCancellation {
    fn cancel_all(&self) {}
}

#[derive(Debug, Default)]
struct WorkerGate {
    allowed: Mutex<bool>,
    changed: Condvar,
}

impl WorkerGate {
    fn wait(&self) {
        let mut allowed = self
            .allowed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*allowed {
            allowed = self
                .changed
                .wait(allowed)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn allow(&self) {
        let mut allowed = self
            .allowed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *allowed = true;
        drop(allowed);
        self.changed.notify_all();
    }
}

#[derive(Clone, Debug)]
struct BridgeOperation {
    request_id: String,
    lease: GenerationOperationLease,
    gate: Arc<WorkerGate>,
}

#[derive(Debug)]
struct BridgeCore {
    application: Mutex<ApplicationPhase>,
    lifecycle: GenerationSupervisor,
    workers: GenerationWorkerRegistry,
    canonical_shutdown: Mutex<Option<ShutdownOutcome>>,
}

#[derive(Clone, Debug)]
struct LoomBridgeAdapter {
    core: Arc<BridgeCore>,
    progress_capacity: usize,
}

impl LoomBridgeAdapter {
    fn with_capacity(progress_capacity: usize) -> Self {
        let lifecycle = GenerationSupervisor::new(progress_capacity)
            .expect("contract progress capacity is valid");
        Self {
            core: Arc::new(BridgeCore {
                application: Mutex::new(ApplicationPhase::Running),
                workers: GenerationWorkerRegistry::new(lifecycle.clone()),
                lifecycle,
                canonical_shutdown: Mutex::new(None),
            }),
            progress_capacity,
        }
    }

    fn reserve_worker(
        &self,
        operation_id: &str,
        panics: bool,
    ) -> Result<BridgeOperation, BridgeError> {
        let admission = self
            .core
            .application
            .lock()
            .map_err(|_| BridgeError::Poisoned)?;
        if *admission != ApplicationPhase::Running {
            return Err(BridgeError::AdmissionClosed);
        }
        let (ticket, lease) = self.core.lifecycle.reserve(operation_id)?;
        self.core.lifecycle.queue(&lease)?;
        self.core.lifecycle.start(&lease)?;
        let reservation = self
            .core
            .workers
            .reserve(operation_id, &admission)
            .map_err(|error| BridgeError::ipc(&error))?;
        let gate = Arc::new(WorkerGate::default());
        let worker_gate = Arc::clone(&gate);
        let admitted = Arc::new(WorkerGate::default());
        let worker_admitted = Arc::clone(&admitted);
        let worker_lifecycle = self.core.lifecycle.clone();
        let worker_lease = lease.clone();
        let worker = std::thread::Builder::new()
            .name(format!("loom-contract-{operation_id}"))
            .spawn(move || {
                worker_admitted.wait();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    assert!(!panics, "controlled Loom executor panic");
                    worker_gate.wait();
                }));
                if result.is_err() {
                    let _ = worker_lifecycle
                        .terminal_and_release(&worker_lease, GenerationTerminalClass::Failed);
                }
            })
            .map_err(|error| BridgeError::Message(error.to_string()))?;
        let attached = reservation.attach(
            worker,
            GenerationWorkerOwner::controlled(Arc::new(ContractCancellation)),
        );
        if let Err(error) = attached {
            gate.allow();
            admitted.allow();
            let failure = BridgeError::ipc(&error.failure);
            let _ = error.worker.join();
            return Err(failure);
        }
        admitted.allow();
        ticket.detach();
        drop(admission);
        Ok(BridgeOperation {
            request_id: operation_id.to_owned(),
            lease,
            gate,
        })
    }

    fn finish_and_release(
        &self,
        operation: &BridgeOperation,
        class: GenerationTerminalClass,
    ) -> Result<(), BridgeError> {
        self.core
            .lifecycle
            .terminal_and_release(&operation.lease, class)?;
        Ok(())
    }

    fn allow_worker_exit(operation: &BridgeOperation) {
        operation.gate.allow();
    }

    fn begin_shutdown_witness(&self) -> LoomShutdownWitness {
        let (started_tx, started_rx) = mpsc::channel();
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let core = Arc::clone(&self.core);
        std::thread::spawn(move || {
            let outcome = (|| -> Result<ShutdownOutcome, BridgeError> {
                if let Some(canonical) = core
                    .canonical_shutdown
                    .lock()
                    .map_err(|_| BridgeError::Poisoned)?
                    .clone()
                {
                    let _ = started_tx.send(());
                    return Ok(canonical);
                }
                {
                    let mut application =
                        core.application.lock().map_err(|_| BridgeError::Poisoned)?;
                    if *application == ApplicationPhase::Running {
                        *application = ApplicationPhase::Closing;
                    }
                }
                core.lifecycle.quiesce()?;
                let _ = started_tx.send(());
                let joined = core
                    .workers
                    .join_all()
                    .map_err(|error| BridgeError::ipc(&error))?;
                let facts = core.lifecycle.close()?;
                *core.application.lock().map_err(|_| BridgeError::Poisoned)? =
                    ApplicationPhase::ExitAuthorized;
                let outcome = ShutdownOutcome {
                    facts: contract_closed_facts(&facts),
                    expected_worker_ids: joined.expected_worker_ids,
                    joined_worker_ids: joined.joined_worker_ids,
                };
                *core
                    .canonical_shutdown
                    .lock()
                    .map_err(|_| BridgeError::Poisoned)? = Some(outcome.clone());
                Ok(outcome)
            })();
            let _ = outcome_tx.send(outcome);
        });
        LoomShutdownWitness {
            started: Mutex::new(started_rx),
            outcome: Mutex::new(outcome_rx),
        }
    }

    fn wait_released(
        &self,
        operation: &BridgeOperation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, BridgeError> {
        self.core
            .lifecycle
            .wait_released(&operation.lease, timeout)?
            .map(contract_snapshot)
            .ok_or(BridgeError::Timeout)
    }

    fn join_operation(&self, operation: &BridgeOperation) -> Result<(), BridgeError> {
        self.core
            .workers
            .join_request(&operation.request_id)
            .map_err(|error| BridgeError::ipc(&error))
    }
}

#[derive(Debug)]
struct LoomShutdownWitness {
    started: Mutex<Receiver<()>>,
    outcome: Mutex<Receiver<Result<ShutdownOutcome, BridgeError>>>,
}

impl ShutdownWitness for LoomShutdownWitness {
    type Error = BridgeError;

    fn wait_started(&self, timeout: Duration) -> Result<(), Self::Error> {
        self.started
            .lock()
            .map_err(|_| BridgeError::Poisoned)?
            .recv_timeout(timeout)
            .map_err(|_| BridgeError::Timeout)
    }

    fn try_complete(&self) -> Result<Option<ShutdownOutcome>, Self::Error> {
        match self
            .outcome
            .lock()
            .map_err(|_| BridgeError::Poisoned)?
            .try_recv()
        {
            Ok(outcome) => outcome.map(Some),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(BridgeError::ChannelClosed),
        }
    }

    fn wait(self, timeout: Duration) -> Result<ShutdownOutcome, Self::Error> {
        self.outcome
            .into_inner()
            .map_err(|_| BridgeError::Poisoned)?
            .recv_timeout(timeout)
            .map_err(|_| BridgeError::Timeout)?
    }
}

impl AdmissionQuiesceShutdownBridgeAdapter for LoomBridgeAdapter {
    type Implementation = LoomInteractiveLifecycle;
    type Error = BridgeError;
    type Operation = BridgeOperation;
    type ShutdownWitness = LoomShutdownWitness;

    fn deterministic() -> Self {
        Self::with_capacity(4)
    }

    fn reserve(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        self.reserve_worker(operation_id, false)
    }

    fn quiesce(&self) {
        if let Ok(mut application) = self.core.application.lock() {
            *application = ApplicationPhase::Closing;
        }
        self.core
            .lifecycle
            .quiesce()
            .expect("admission lifecycle quiesce");
    }

    fn phase(&self) -> LifecyclePhase {
        contract_lifecycle(
            self.core
                .lifecycle
                .phase()
                .expect("admission lifecycle phase"),
        )
    }

    fn active_count(&self) -> usize {
        self.core
            .lifecycle
            .active_count()
            .expect("admission active count")
    }

    fn retained_task_count(&self) -> usize {
        self.core
            .lifecycle
            .retained_task_count()
            .expect("admission retained task count")
    }

    fn cancellation_requested(&self, operation_id: &str) -> bool {
        self.core
            .lifecycle
            .current_snapshot(operation_id)
            .expect("admission cancellation snapshot")
            .is_some_and(|snapshot| snapshot.cancellation_requested)
    }

    fn request_cancelled_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.finish_and_release(operation, GenerationTerminalClass::Cancelled)
    }

    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        self.wait_released(operation, timeout)
    }

    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        Self::allow_worker_exit(operation);
        Ok(())
    }

    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        self.begin_shutdown_witness()
    }

    fn shutdown(&self) -> ClosedFacts {
        contract_closed_facts(
            &self
                .core
                .lifecycle
                .closed_facts()
                .expect("admission closed facts"),
        )
    }
}

impl ProgressShutdownBridgeAdapter for LoomBridgeAdapter {
    type Implementation = LoomInteractiveLifecycle;
    type Error = BridgeError;
    type UnreadProgress = ();
    type Operation = BridgeOperation;
    type ShutdownWitness = LoomShutdownWitness;

    fn deterministic(progress_capacity: usize) -> Self {
        Self::with_capacity(progress_capacity)
    }

    fn start(
        &self,
        operation_id: &str,
    ) -> Result<(Self::UnreadProgress, Self::Operation), Self::Error> {
        Ok(((), self.reserve_worker(operation_id, false)?))
    }

    fn publish_progress(
        &self,
        operation: &Self::Operation,
        sequence: u64,
    ) -> Result<(), Self::Error> {
        self.core
            .lifecycle
            .publish_progress(&operation.lease, sequence)?;
        Ok(())
    }

    fn snapshot(&self, operation: &Self::Operation) -> Option<OperationSnapshot> {
        self.core
            .lifecycle
            .snapshot(&operation.lease)
            .expect("progress operation snapshot")
            .map(contract_snapshot)
    }

    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        self.begin_shutdown_witness()
    }

    fn request_completed_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.finish_and_release(operation, GenerationTerminalClass::Completed)
    }

    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        self.wait_released(operation, timeout)
    }

    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        Self::allow_worker_exit(operation);
        Ok(())
    }

    fn progress_capacity(&self) -> usize {
        self.progress_capacity
    }
}

impl PanicShutdownBridgeAdapter for LoomBridgeAdapter {
    type Implementation = LoomInteractiveLifecycle;
    type Error = BridgeError;
    type Operation = BridgeOperation;
    type ShutdownWitness = LoomShutdownWitness;

    fn deterministic() -> Self {
        Self::with_capacity(4)
    }

    fn run_controlled_panicking_operation(
        &self,
        operation_id: &str,
    ) -> Result<Self::Operation, Self::Error> {
        self.reserve_worker(operation_id, true)
    }

    fn wait_failed_release(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        self.wait_released(operation, timeout)
    }

    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        self.begin_shutdown_witness()
    }
}

impl StableShutdownAdapter for LoomBridgeAdapter {
    type Implementation = LoomInteractiveLifecycle;
    type Error = BridgeError;
    type Operation = BridgeOperation;
    type ShutdownWitness = LoomShutdownWitness;

    fn deterministic() -> Self {
        Self::with_capacity(4)
    }

    fn start(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        self.reserve_worker(operation_id, false)
    }

    fn request_completed_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.finish_and_release(operation, GenerationTerminalClass::Completed)
    }

    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        self.wait_released(operation, timeout)
    }

    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        Self::allow_worker_exit(operation);
        Ok(())
    }

    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        self.begin_shutdown_witness()
    }
}

impl TaskReapingAdapter for LoomBridgeAdapter {
    type Implementation = LoomInteractiveLifecycle;
    type Error = BridgeError;
    type Operation = BridgeOperation;

    fn deterministic() -> Self {
        Self::with_capacity(4)
    }

    fn start(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        self.reserve_worker(operation_id, false)
    }

    fn finish(&self, operation: Self::Operation) -> Result<(), Self::Error> {
        self.finish_and_release(&operation, GenerationTerminalClass::Completed)?;
        Self::allow_worker_exit(&operation);
        self.join_operation(&operation)
    }

    fn active_count(&self) -> usize {
        self.core
            .lifecycle
            .active_count()
            .expect("task-reaping active count")
    }

    fn retained_task_count(&self) -> usize {
        self.core.workers.retained_count()
    }

    fn shutdown(&self) -> ClosedFacts {
        self.core
            .lifecycle
            .quiesce()
            .expect("task-reaping lifecycle quiesce");
        self.core
            .workers
            .join_all()
            .expect("task-reaping worker join");
        contract_closed_facts(
            &self
                .core
                .lifecycle
                .close()
                .expect("task-reaping lifecycle close"),
        )
    }
}

fn contract_snapshot(snapshot: GenerationOperationSnapshot) -> OperationSnapshot {
    OperationSnapshot {
        identity: platform_contract_testkit::AttemptIdentity {
            operation_id: snapshot.identity.operation_id.clone(),
            attempt_id: snapshot.identity.attempt_id.clone(),
            sequence: snapshot.identity.sequence,
        },
        phase: match snapshot.phase {
            loom_host::GenerationOperationPhase::Reserved => OperationPhase::Reserved,
            loom_host::GenerationOperationPhase::Queued => OperationPhase::Queued,
            loom_host::GenerationOperationPhase::Running => OperationPhase::Running,
            loom_host::GenerationOperationPhase::Terminal => OperationPhase::Terminal,
            loom_host::GenerationOperationPhase::Released => OperationPhase::Released,
        },
        cancellation_requested: snapshot.cancellation_requested,
        authoritative_terminal: snapshot
            .authoritative_terminal
            .map(|terminal| TerminalRecord {
                class: contract_terminal(terminal.class),
                sequence: terminal.identity.sequence,
            }),
        final_projection: snapshot.final_projection.map(|terminal| TerminalRecord {
            class: contract_terminal(terminal.class),
            sequence: terminal.identity.sequence,
        }),
        progress_projection: snapshot.progress_projection,
    }
}

const fn contract_terminal(class: GenerationTerminalClass) -> TerminalClass {
    match class {
        GenerationTerminalClass::Completed => TerminalClass::Completed,
        GenerationTerminalClass::Cancelled => TerminalClass::Cancelled,
        GenerationTerminalClass::Failed => TerminalClass::Failed,
    }
}

const fn contract_lifecycle(phase: GenerationSupervisorPhase) -> LifecyclePhase {
    match phase {
        GenerationSupervisorPhase::Running => LifecyclePhase::Running,
        GenerationSupervisorPhase::Quiescing => LifecyclePhase::Quiescing,
        GenerationSupervisorPhase::Closed => LifecyclePhase::Closed,
    }
}

fn contract_closed_facts(facts: &GenerationSupervisorClosedFacts) -> ClosedFacts {
    ClosedFacts {
        lifecycle: contract_lifecycle(facts.lifecycle),
        active_operations: facts.active_operations,
        retained_tasks: facts.retained_tasks,
        expected_workers: facts.expected_workers.len(),
        joined_workers: facts.joined_workers.len(),
    }
}

#[derive(Debug, thiserror::Error)]
enum BridgeError {
    #[error(transparent)]
    Supervisor(#[from] GenerationSupervisorError),
    #[error("application admission is closed")]
    AdmissionClosed,
    #[error("bridge state is poisoned")]
    Poisoned,
    #[error("bridge witness timed out")]
    Timeout,
    #[error("bridge witness channel closed")]
    ChannelClosed,
    #[error("{0}")]
    Message(String),
}

impl BridgeError {
    fn ipc(error: &IpcFailure) -> Self {
        Self::Message(format!("{}: {}", error.code, error.message))
    }
}

#[test]
fn real_loom_owners_accept_one_complete_lifecycle_manifest() {
    let mut evidence = owned_lifecycle_evidence();
    evidence.extend([
        run_admission_quiesce_shutdown_bridge_suite::<LoomBridgeAdapter>(
            "application-generation-bridge",
        ),
        run_progress_shutdown_bridge_suite::<LoomBridgeAdapter>(
            "generation-progress-worker-bridge",
        ),
        run_panic_shutdown_bridge_suite::<LoomBridgeAdapter>("generation-worker-supervision"),
        run_stable_shutdown_suite::<LoomBridgeAdapter>("application-generation-workers"),
        run_task_reaping_suite::<LoomBridgeAdapter>("generation-worker-registry"),
    ]);
    let manifest = LifecycleCoverageManifest::<LoomInteractiveLifecycle>::accept(evidence)
        .expect("all eleven Loom suites are owned by the real product lifecycle");
    assert_eq!(manifest.product(), "loom");
    assert_eq!(manifest.implementation(), "interactive-generation-registry");
    assert_eq!(manifest.covered().count(), 18);
    assert!(manifest.components().count() >= 5);
    assert!(
        manifest
            .covered()
            .any(|invariant| { invariant == LifecycleInvariant::QuiesceWaitsForReleaseAndJoin })
    );
}
