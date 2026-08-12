//! Ownership-specific lifecycle conformance suites.
//!
//! These traits exist only as product test adapters. A product may use a
//! different real component for each suite. Cross-boundary invariants retain
//! dedicated bridge traits, so composition cannot turn two unrelated local
//! facts into one lifecycle claim.

#[cfg(test)]
use std::collections::BTreeMap;
#[cfg(test)]
use std::sync::{Arc, MutexGuard};
#[cfg(test)]
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::Duration;

use crate::barrier::DeterministicBarrier;
use crate::coverage::{CoverageEvidence, LifecycleImplementation, LifecycleInvariant};
#[cfg(test)]
use crate::model::TerminalRecord;
use crate::model::{
    AttemptIdentity, ClosedFacts, LifecyclePhase, OperationModelAdapter, OperationPhase,
    OperationSnapshot, ReferenceAdapter, ReferenceLease, ReferenceTicket, ShutdownOutcome,
    TerminalClass, TestConfig, WaitObservation,
};

/// Fixed identity of the testkit's deterministic reference implementation.
pub enum ReferenceLifecycle {}

const WITNESS_TIMEOUT: Duration = Duration::from_secs(5);

impl LifecycleImplementation for ReferenceLifecycle {
    const PRODUCT: &'static str = "reference-product";
    const IMPLEMENTATION: &'static str = "reference-lifecycle";
}

pub trait TransitionChainAdapter: Clone + Sized {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug;
    type Operation;

    fn deterministic() -> Self;
    fn reserve(&self, operation_id: &str) -> Result<Self::Operation, Self::Error>;
    fn phase(&self, operation: &Self::Operation) -> Option<OperationPhase>;
    fn queue(&self, operation: &Self::Operation) -> Result<(), Self::Error>;
    fn start(&self, operation: &Self::Operation) -> Result<(), Self::Error>;
    fn terminal(
        &self,
        operation: &Self::Operation,
        class: TerminalClass,
    ) -> Result<(), Self::Error>;
    fn release(&self, operation: &Self::Operation) -> Result<(), Self::Error>;
}

pub trait RegistryIdentityAdapter: Clone + Sized {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug;
    type Guard;
    type Lease: Clone;

    fn deterministic(next_sequence: u64) -> Self;
    fn reserve(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error>;
    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity;
    fn complete_and_release(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
    fn active_count(&self) -> usize;
    fn current_identity(&self, operation_id: &str) -> Option<AttemptIdentity>;
}

pub trait AttemptHierarchyAdapter: Clone + Sized {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug;
    type Operation;
    type Attempt;

    fn deterministic() -> Self;
    fn create_operation(&self, operation_id: &str) -> Result<Self::Operation, Self::Error>;
    fn start_attempt(&self, operation: &Self::Operation) -> Result<Self::Attempt, Self::Error>;
    fn attempt_identity(&self, attempt: &Self::Attempt) -> AttemptIdentity;
    fn operation_active(&self, operation: &Self::Operation) -> bool;
    fn active_attempts(&self, operation: &Self::Operation) -> Vec<AttemptIdentity>;
    fn request_operation_cancel(&self, operation: &Self::Operation) -> Result<(), Self::Error>;
    fn cancellation_requested(&self, attempt: &Self::Attempt) -> bool;
    fn finish_attempt(&self, attempt: Self::Attempt) -> Result<(), Self::Error>;
    fn finish_operation(&self, operation: &Self::Operation) -> Result<(), Self::Error>;
}

pub trait ConsumerCancellationAdapter: Clone + Sized {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug;
    type Ticket: Send;
    type Lease;

    fn deterministic() -> Self;
    fn start(&self, operation_id: &str) -> Result<(Self::Ticket, Self::Lease), Self::Error>;
    fn ticket_identity(&self, ticket: &Self::Ticket) -> AttemptIdentity;
    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity;
    fn active_count(&self) -> usize;
    fn current_snapshot(&self, operation_id: &str) -> Option<OperationSnapshot>;
    fn lease_snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot>;
    fn cancellation_requested(&self, lease: &Self::Lease) -> bool;
    fn explicit_consumer_drop(&self, ticket: Self::Ticket) -> Result<(), Self::Error>;
    fn finish_cancelled(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
}

pub trait TerminalAuthorityAdapter: Clone + Sized + Send + Sync + 'static {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug + Send + 'static;
    type Guard;
    type Lease: Clone + Send + Sync + 'static;

    fn deterministic() -> Self;
    fn start(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error>;
    fn terminal(&self, lease: &Self::Lease, class: TerminalClass) -> Result<(), Self::Error>;
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot>;
    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
}

pub trait WaiterControlAdapter: Clone + Sized {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug;
    type Ticket;
    type Lease;

    fn deterministic() -> Self;
    fn start(&self, operation_id: &str) -> Result<(Self::Ticket, Self::Lease), Self::Error>;
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot>;
    fn waiter_timeout(&self, ticket: &Self::Ticket) -> Result<WaitObservation, Self::Error>;
    fn request_cancel(&self, ticket: &Self::Ticket) -> Result<(), Self::Error>;
    fn finish_cancelled(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
}

pub trait AdmissionQuiesceShutdownBridgeAdapter: Clone + Sized + Send + Sync + 'static {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug + Send + 'static;
    type Operation: Send + 'static;
    type ShutdownWitness: ShutdownWitness;

    fn deterministic() -> Self;
    fn reserve(&self, operation_id: &str) -> Result<Self::Operation, Self::Error>;
    fn quiesce(&self);
    fn phase(&self) -> LifecyclePhase;
    fn active_count(&self) -> usize;
    fn retained_task_count(&self) -> usize;
    fn cancellation_requested(&self, operation_id: &str) -> bool;
    fn request_cancelled_release(&self, operation: &Self::Operation) -> Result<(), Self::Error>;
    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error>;
    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error>;
    fn begin_shutdown(&self) -> Self::ShutdownWitness;
    fn shutdown(&self) -> ClosedFacts;
}

/// A nonblocking shutdown invocation plus deterministic phase/result gates.
///
/// Product adapters may back this with a native thread or a genuine async
/// runtime task. Only the completion result crosses into this synchronous
/// conformance harness.
pub trait ShutdownWitness: Sized {
    type Error: std::fmt::Debug;

    fn wait_started(&self, timeout: Duration) -> Result<(), Self::Error>;
    fn try_complete(&self) -> Result<Option<ShutdownOutcome>, Self::Error>;
    fn wait(self, timeout: Duration) -> Result<ShutdownOutcome, Self::Error>;
}

/// Integrated proof that progress backpressure cannot starve terminal release
/// or successful shutdown. Keeping this as one trait preserves the cross-layer
/// invariant when progress and shutdown have different internal owners.
pub trait ProgressShutdownBridgeAdapter: Clone + Sized + Send + Sync + 'static {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug + Send + 'static;
    type UnreadProgress;
    type Operation: Clone + Send + Sync + 'static;
    type ShutdownWitness: ShutdownWitness;

    fn deterministic(progress_capacity: usize) -> Self;
    fn start(
        &self,
        operation_id: &str,
    ) -> Result<(Self::UnreadProgress, Self::Operation), Self::Error>;
    fn publish_progress(
        &self,
        operation: &Self::Operation,
        sequence: u64,
    ) -> Result<(), Self::Error>;
    fn snapshot(&self, operation: &Self::Operation) -> Option<OperationSnapshot>;
    fn begin_shutdown(&self) -> Self::ShutdownWitness;
    fn request_completed_release(&self, operation: &Self::Operation) -> Result<(), Self::Error>;
    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error>;
    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error>;
    fn progress_capacity(&self) -> usize;
}

/// Integrated proof that executor panic becomes a safe failed terminal and
/// does not strand registry, task, or worker ownership at shutdown.
pub trait PanicShutdownBridgeAdapter: Clone + Sized {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug;
    type Operation;
    type ShutdownWitness: ShutdownWitness;

    fn deterministic() -> Self;
    fn run_controlled_panicking_operation(
        &self,
        operation_id: &str,
    ) -> Result<Self::Operation, Self::Error>;
    fn wait_failed_release(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error>;
    fn begin_shutdown(&self) -> Self::ShutdownWitness;
}

pub trait StableShutdownAdapter: Clone + Sized {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug;
    type Operation;

    fn deterministic() -> Self;
    fn start(&self, operation_id: &str) -> Result<Self::Operation, Self::Error>;
    fn finish(&self, operation: Self::Operation) -> Result<(), Self::Error>;
    fn shutdown(&self) -> ClosedFacts;
}

pub trait TaskReapingAdapter: Clone + Sized {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug;
    type Operation;

    fn deterministic() -> Self;
    fn start(&self, operation_id: &str) -> Result<Self::Operation, Self::Error>;
    fn finish(&self, operation: Self::Operation) -> Result<(), Self::Error>;
    fn active_count(&self) -> usize;
    fn retained_task_count(&self) -> usize;
    fn shutdown(&self) -> ClosedFacts;
}

pub fn run_transition_chain_suite<A: TransitionChainAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic();
    let operation = adapter.reserve("transitions").expect("reserve");
    assert_eq!(adapter.phase(&operation), Some(OperationPhase::Reserved));
    assert!(adapter.start(&operation).is_err());
    assert!(adapter.release(&operation).is_err());
    assert!(
        adapter
            .terminal(&operation, TerminalClass::Completed)
            .is_err()
    );
    adapter.queue(&operation).expect("Reserved -> Queued");
    assert_eq!(adapter.phase(&operation), Some(OperationPhase::Queued));
    assert!(
        adapter.queue(&operation).is_err(),
        "Queued -> Queued is invalid"
    );
    assert!(
        adapter.release(&operation).is_err(),
        "Queued -> Released is invalid"
    );
    assert!(
        adapter
            .terminal(&operation, TerminalClass::Completed)
            .is_err(),
        "Queued -> Terminal is invalid"
    );
    adapter.start(&operation).expect("Queued -> Running");
    assert_eq!(adapter.phase(&operation), Some(OperationPhase::Running));
    assert!(
        adapter.start(&operation).is_err(),
        "Running -> Running is invalid"
    );
    assert!(
        adapter.queue(&operation).is_err(),
        "Running -> Queued is invalid"
    );
    assert!(
        adapter.release(&operation).is_err(),
        "Running -> Released is invalid"
    );
    adapter
        .terminal(&operation, TerminalClass::Completed)
        .expect("Running -> Terminal");
    assert_eq!(adapter.phase(&operation), Some(OperationPhase::Terminal));
    assert!(
        adapter.terminal(&operation, TerminalClass::Failed).is_err(),
        "a terminal operation is immutable"
    );
    assert!(
        adapter.queue(&operation).is_err(),
        "Terminal -> Queued is invalid"
    );
    assert!(
        adapter.start(&operation).is_err(),
        "Terminal -> Running is invalid"
    );
    adapter.release(&operation).expect("Terminal -> Released");
    assert_eq!(adapter.phase(&operation), Some(OperationPhase::Released));
    assert!(adapter.release(&operation).is_err());
    assert!(adapter.queue(&operation).is_err());
    assert!(adapter.start(&operation).is_err());
    assert!(adapter.terminal(&operation, TerminalClass::Failed).is_err());
    CoverageEvidence::passed(
        component,
        "transition-chain",
        [LifecycleInvariant::ExactTransitionChain],
    )
}

pub fn run_registry_identity_suite<A: RegistryIdentityAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic(1);
    let (first_guard, first) = adapter.reserve("reused").expect("first reserve");
    assert!(adapter.reserve("reused").is_err());
    assert_eq!(adapter.active_count(), 1);
    let first_identity = adapter.lease_identity(&first);
    adapter
        .complete_and_release(&first)
        .expect("first executor release");
    drop(first_guard);
    let (current_guard, current) = adapter.reserve("reused").expect("identity reuse");
    let current_identity = adapter.lease_identity(&current);
    assert_ne!(first_identity, current_identity);
    assert_eq!(first_identity.operation_id, current_identity.operation_id);
    assert_ne!(first_identity.attempt_id, current_identity.attempt_id);
    assert!(adapter.complete_and_release(&first).is_err());
    assert_eq!(adapter.current_identity("reused"), Some(current_identity));
    adapter
        .complete_and_release(&current)
        .expect("current release");
    drop(current_guard);

    let exhausted = A::deterministic(u64::MAX);
    assert!(exhausted.reserve("exhausted").is_err());
    assert_eq!(exhausted.active_count(), 0);
    CoverageEvidence::passed(
        component,
        "registry-identity",
        [
            LifecycleInvariant::DuplicatePublicId,
            LifecycleInvariant::StaleRelease,
            LifecycleInvariant::SequenceExhaustion,
        ],
    )
}

pub fn run_attempt_hierarchy_suite<A: AttemptHierarchyAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic();
    let operation = adapter
        .create_operation("public-operation")
        .expect("create public operation");
    assert!(adapter.operation_active(&operation));
    let first = adapter.start_attempt(&operation).expect("first attempt");
    let second = adapter.start_attempt(&operation).expect("second attempt");
    let first_identity = adapter.attempt_identity(&first);
    let second_identity = adapter.attempt_identity(&second);
    assert_eq!(first_identity.operation_id, "public-operation");
    assert_eq!(second_identity.operation_id, "public-operation");
    assert_ne!(first_identity.attempt_id, second_identity.attempt_id);
    assert_ne!(first_identity.sequence, second_identity.sequence);
    let active = adapter.active_attempts(&operation);
    assert_eq!(active.len(), 2);
    assert!(active.contains(&first_identity));
    assert!(active.contains(&second_identity));
    adapter
        .request_operation_cancel(&operation)
        .expect("cancel public operation");
    assert!(adapter.cancellation_requested(&first));
    assert!(adapter.cancellation_requested(&second));
    adapter.finish_attempt(first).expect("finish first attempt");
    assert!(adapter.operation_active(&operation));
    assert_eq!(adapter.active_attempts(&operation), [second_identity]);
    adapter
        .finish_attempt(second)
        .expect("finish second attempt");
    assert!(adapter.operation_active(&operation));
    assert!(adapter.active_attempts(&operation).is_empty());
    adapter
        .finish_operation(&operation)
        .expect("finish public operation");
    assert!(!adapter.operation_active(&operation));
    CoverageEvidence::passed(
        component,
        "attempt-hierarchy",
        [LifecycleInvariant::AttemptIdentityUnderPublicOperation],
    )
}

pub fn run_consumer_cancellation_suite<A: ConsumerCancellationAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic();
    let (ticket, lease) = adapter.start("ticket-drop").expect("start");
    assert_eq!(
        adapter.ticket_identity(&ticket),
        adapter.lease_identity(&lease)
    );
    let identity = adapter.lease_identity(&lease);
    drop(ticket);
    assert_eq!(adapter.active_count(), 1);
    assert_eq!(
        adapter.current_snapshot("ticket-drop").map(|s| s.identity),
        Some(identity.clone())
    );
    assert_eq!(
        adapter.lease_snapshot(&lease).map(|s| s.phase),
        Some(OperationPhase::Running)
    );
    assert!(adapter.cancellation_requested(&lease));
    adapter
        .finish_cancelled(&lease)
        .expect("executor retains release authority");

    let (ticket, lease) = adapter.start("explicit-drop").expect("start");
    let identity = adapter.lease_identity(&lease);
    adapter
        .explicit_consumer_drop(ticket)
        .expect("explicit consumer drop");
    assert_eq!(adapter.active_count(), 1);
    assert_eq!(
        adapter
            .current_snapshot("explicit-drop")
            .map(|snapshot| snapshot.identity),
        Some(identity)
    );
    assert_eq!(
        adapter
            .lease_snapshot(&lease)
            .map(|snapshot| snapshot.phase),
        Some(OperationPhase::Running)
    );
    assert!(adapter.cancellation_requested(&lease));
    adapter.finish_cancelled(&lease).expect("finish");
    CoverageEvidence::passed(
        component,
        "consumer-cancellation",
        [
            LifecycleInvariant::TicketDropCancellation,
            LifecycleInvariant::ExplicitConsumerDropCancellation,
        ],
    )
}

pub fn run_terminal_authority_suite<A: TerminalAuthorityAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic();
    let (_guard, lease) = adapter.start("terminal").expect("start");
    adapter
        .terminal(&lease, TerminalClass::Completed)
        .expect("terminal");
    let snapshot = adapter.snapshot(&lease).expect("terminal snapshot");
    assert_terminal(&snapshot, TerminalClass::Completed);
    assert!(adapter.terminal(&lease, TerminalClass::Failed).is_err());
    assert_eq!(adapter.snapshot(&lease), Some(snapshot));
    adapter.release(&lease).expect("release");

    let (_guard, lease) = adapter.start("terminal-race").expect("start race");
    let barrier = DeterministicBarrier::new(2);
    let cancel_adapter = adapter.clone();
    let cancel_lease = lease.clone();
    let cancel_barrier = barrier.clone();
    let cancel = thread::spawn(move || {
        cancel_barrier.arrive_and_wait();
        cancel_adapter
            .terminal(&cancel_lease, TerminalClass::Cancelled)
            .is_ok()
    });
    let complete_adapter = adapter.clone();
    let complete_lease = lease.clone();
    let complete_barrier = barrier.clone();
    let complete = thread::spawn(move || {
        complete_barrier.arrive_and_wait();
        complete_adapter
            .terminal(&complete_lease, TerminalClass::Completed)
            .is_ok()
    });
    barrier.wait_until_all_arrived();
    barrier.release();
    let cancel_won = cancel.join().expect("cancel racer");
    let complete_won = complete.join().expect("complete racer");
    assert_ne!(cancel_won, complete_won);
    let snapshot = adapter.snapshot(&lease).expect("race terminal");
    assert_terminal(
        &snapshot,
        if cancel_won {
            TerminalClass::Cancelled
        } else {
            TerminalClass::Completed
        },
    );
    adapter.release(&lease).expect("release race");
    CoverageEvidence::passed(
        component,
        "terminal-authority",
        [
            LifecycleInvariant::AuthoritativeTerminal,
            LifecycleInvariant::CancelCompleteRace,
        ],
    )
}

pub fn run_waiter_control_suite<A: WaiterControlAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic();
    let (ticket, lease) = adapter.start("timeout").expect("start");
    let before = adapter.snapshot(&lease).expect("before timeout");
    assert_eq!(before.phase, OperationPhase::Running);
    assert!(before.authoritative_terminal.is_none());
    assert!(before.final_projection.is_none());
    assert_eq!(
        adapter.waiter_timeout(&ticket).expect("timeout"),
        WaitObservation::TimedOut
    );
    assert_eq!(adapter.snapshot(&lease), Some(before));
    adapter
        .request_cancel(&ticket)
        .expect("control retained after timeout");
    assert!(
        adapter
            .snapshot(&lease)
            .expect("cancelled snapshot")
            .cancellation_requested
    );
    adapter.finish_cancelled(&lease).expect("finish");
    CoverageEvidence::passed(
        component,
        "waiter-control",
        [LifecycleInvariant::WaiterTimeoutIsObservational],
    )
}

pub fn run_admission_quiesce_shutdown_bridge_suite<A: AdmissionQuiesceShutdownBridgeAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic();
    let barrier = DeterministicBarrier::new(2);
    let admit_adapter = adapter.clone();
    let admit_barrier = barrier.clone();
    let admit = thread::spawn(move || {
        admit_barrier.arrive_and_wait();
        admit_adapter.reserve("racing-admission").ok()
    });
    let quiesce_adapter = adapter.clone();
    let quiesce_barrier = barrier.clone();
    let quiesce = thread::spawn(move || {
        quiesce_barrier.arrive_and_wait();
        quiesce_adapter.quiesce();
    });
    barrier.wait_until_all_arrived();
    barrier.release();
    let admitted = admit.join().expect("admission racer");
    quiesce.join().expect("quiesce racer");
    assert_eq!(adapter.phase(), LifecyclePhase::Quiescing);
    assert_eq!(adapter.active_count(), usize::from(admitted.is_some()));
    if let Some(operation) = admitted {
        assert!(adapter.cancellation_requested("racing-admission"));
        let shutdown = adapter.begin_shutdown();
        shutdown
            .wait_started(WITNESS_TIMEOUT)
            .expect("shutdown reaches quiescing");
        assert!(
            shutdown
                .try_complete()
                .expect("inspect racing shutdown")
                .is_none()
        );
        adapter
            .request_cancelled_release(&operation)
            .expect("let racing worker publish cancellation and release");
        let terminal = adapter
            .wait_released(&operation, WITNESS_TIMEOUT)
            .expect("observe racing worker release");
        assert_cancelled_release(&terminal);
        assert_eq!(adapter.active_count(), 0);
        assert_eq!(adapter.retained_task_count(), 1);
        assert!(
            shutdown
                .try_complete()
                .expect("shutdown remains pending before worker exit")
                .is_none()
        );
        adapter
            .allow_worker_exit(&operation)
            .expect("allow racing worker exit");
        let outcome = shutdown.wait(WITNESS_TIMEOUT).expect("join racing worker");
        assert_shutdown_outcome(&outcome);
        assert_eq!(adapter.shutdown(), outcome.facts);
    }
    assert!(adapter.reserve("post-quiesce").is_err());

    let adapter = A::deterministic();
    let operation = adapter.reserve("active-shutdown").expect("reserve active");
    let shutdown = adapter.begin_shutdown();
    shutdown
        .wait_started(WITNESS_TIMEOUT)
        .expect("shutdown reaches quiescing");
    assert_eq!(adapter.phase(), LifecyclePhase::Quiescing);
    assert_eq!(adapter.active_count(), 1);
    assert!(
        shutdown
            .try_complete()
            .expect("inspect shutdown completion")
            .is_none()
    );
    assert!(adapter.cancellation_requested("active-shutdown"));
    assert!(adapter.reserve("post-shutdown").is_err());
    adapter
        .request_cancelled_release(&operation)
        .expect("let active worker publish cancellation and release");
    let terminal = adapter
        .wait_released(&operation, WITNESS_TIMEOUT)
        .expect("observe active worker terminal and release");
    assert_cancelled_release(&terminal);
    assert_eq!(adapter.active_count(), 0);
    assert_eq!(adapter.retained_task_count(), 1);
    assert!(
        shutdown
            .try_complete()
            .expect("inspect shutdown before worker exit")
            .is_none()
    );
    adapter
        .allow_worker_exit(&operation)
        .expect("allow the released worker to exit");
    let outcome = shutdown
        .wait(WITNESS_TIMEOUT)
        .expect("shutdown joins released worker");
    assert_shutdown_outcome(&outcome);
    assert_eq!(adapter.shutdown(), outcome.facts);
    CoverageEvidence::passed(
        component,
        "admission-quiesce-shutdown-bridge",
        [
            LifecycleInvariant::AdmitQuiesceRace,
            LifecycleInvariant::QuiesceWaitsForReleaseAndJoin,
        ],
    )
}

pub fn run_progress_shutdown_bridge_suite<A: ProgressShutdownBridgeAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic(3);
    let (unread_progress, operation) = adapter.start("saturated-progress").expect("start");
    for sequence in 0..32 {
        adapter
            .publish_progress(&operation, sequence)
            .expect("progress");
    }
    let snapshot = adapter.snapshot(&operation).expect("progress snapshot");
    assert!(snapshot.progress_projection.len() <= adapter.progress_capacity());
    let shutdown = adapter.begin_shutdown();
    shutdown
        .wait_started(WITNESS_TIMEOUT)
        .expect("progress shutdown starts");
    assert!(shutdown.try_complete().expect("inspect shutdown").is_none());
    adapter
        .request_completed_release(&operation)
        .expect("terminal bypasses unread progress");
    let snapshot = adapter
        .wait_released(&operation, WITNESS_TIMEOUT)
        .expect("observe completion and release");
    assert_eq!(snapshot.phase, OperationPhase::Released);
    assert_eq!(snapshot.authoritative_terminal, snapshot.final_projection);
    assert_eq!(
        snapshot.authoritative_terminal.expect("terminal").class,
        TerminalClass::Completed
    );
    assert!(shutdown.try_complete().expect("inspect shutdown").is_none());
    adapter
        .allow_worker_exit(&operation)
        .expect("allow progress worker exit");
    assert_shutdown_outcome(
        &shutdown
            .wait(WITNESS_TIMEOUT)
            .expect("shutdown joins progress worker"),
    );
    drop(unread_progress);

    let adapter = A::deterministic(2);
    let (unread_progress, operation) = adapter.start("progress-terminal-race").expect("start");
    let shutdown = adapter.begin_shutdown();
    shutdown
        .wait_started(WITNESS_TIMEOUT)
        .expect("race shutdown starts");
    let barrier = DeterministicBarrier::new(2);
    let progress_adapter = adapter.clone();
    let progress_operation = operation.clone();
    let progress_barrier = barrier.clone();
    let progress = thread::spawn(move || {
        progress_barrier.arrive_and_wait();
        for sequence in 0..1024 {
            if progress_adapter
                .publish_progress(&progress_operation, sequence)
                .is_err()
            {
                break;
            }
        }
    });
    let terminal_adapter = adapter.clone();
    let terminal_operation = operation.clone();
    let terminal_barrier = barrier.clone();
    let terminal = thread::spawn(move || {
        terminal_barrier.arrive_and_wait();
        terminal_adapter
            .request_completed_release(&terminal_operation)
            .is_ok()
    });
    barrier.wait_until_all_arrived();
    barrier.release();
    progress.join().expect("progress producer");
    assert!(terminal.join().expect("terminal producer"));
    let snapshot = adapter
        .wait_released(&operation, WITNESS_TIMEOUT)
        .expect("race terminal release");
    assert_eq!(snapshot.phase, OperationPhase::Released);
    assert_eq!(snapshot.authoritative_terminal, snapshot.final_projection);
    adapter
        .allow_worker_exit(&operation)
        .expect("allow racing progress worker exit");
    assert_shutdown_outcome(
        &shutdown
            .wait(WITNESS_TIMEOUT)
            .expect("shutdown joins racing progress worker"),
    );
    drop(unread_progress);
    CoverageEvidence::passed(
        component,
        "progress-shutdown-bridge",
        [
            LifecycleInvariant::UnreadProgressCannotBlockFinalOrShutdown,
            LifecycleInvariant::ProgressTerminalRace,
        ],
    )
}

pub fn run_panic_shutdown_bridge_suite<A: PanicShutdownBridgeAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic();
    let operation = adapter
        .run_controlled_panicking_operation("panic")
        .expect("supervisor admits panicking executor");
    let snapshot = adapter
        .wait_failed_release(&operation, WITNESS_TIMEOUT)
        .expect("supervisor catches panic and releases identity");
    assert_eq!(snapshot.phase, OperationPhase::Released);
    assert_eq!(snapshot.authoritative_terminal, snapshot.final_projection);
    assert_eq!(
        snapshot
            .authoritative_terminal
            .expect("authoritative terminal")
            .class,
        TerminalClass::Failed
    );
    let shutdown = adapter.begin_shutdown();
    shutdown
        .wait_started(WITNESS_TIMEOUT)
        .expect("panic shutdown starts");
    assert_shutdown_outcome(
        &shutdown
            .wait(WITNESS_TIMEOUT)
            .expect("shutdown joins panicked worker"),
    );
    CoverageEvidence::passed(
        component,
        "panic-shutdown-bridge",
        [LifecycleInvariant::PanicTerminalAndShutdown],
    )
}

pub fn run_stable_shutdown_suite<A: StableShutdownAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic();
    let operation = adapter.start("shutdown-after-work").expect("start work");
    adapter.finish(operation).expect("finish work");
    let first = adapter.shutdown();
    let second = adapter.shutdown();
    assert_eq!(first, second);
    assert_closed_and_empty(second);
    CoverageEvidence::passed(
        component,
        "stable-shutdown",
        [
            LifecycleInvariant::RepeatedShutdown,
            LifecycleInvariant::ShutdownEmpty,
        ],
    )
}

pub fn run_task_reaping_suite<A: TaskReapingAdapter>(
    component: &str,
) -> CoverageEvidence<A::Implementation> {
    let adapter = A::deterministic();
    let mut active = Vec::new();
    for sequence in 0..8 {
        active.push(
            adapter
                .start(&format!("concurrent-{sequence}"))
                .expect("start bounded active operation"),
        );
        assert_eq!(adapter.active_count(), sequence + 1);
        assert!(adapter.retained_task_count() <= adapter.active_count());
    }
    while let Some(operation) = active.pop() {
        adapter.finish(operation).expect("finish active operation");
        assert!(adapter.retained_task_count() <= adapter.active_count());
    }
    assert_eq!(adapter.active_count(), 0);
    assert_eq!(adapter.retained_task_count(), 0);

    for sequence in 0..32 {
        let operation = adapter
            .start(&format!("historical-{sequence}"))
            .expect("start historical operation");
        adapter
            .finish(operation)
            .expect("operation must self-reap after release");
        assert_eq!(adapter.active_count(), 0);
        assert_eq!(adapter.retained_task_count(), 0);
    }
    let closed = adapter.shutdown();
    assert_eq!(closed.lifecycle, LifecyclePhase::Closed);
    assert_eq!(closed.active_operations, 0);
    assert_eq!(closed.retained_tasks, 0);
    assert_eq!(closed.joined_workers, closed.expected_workers);
    CoverageEvidence::passed(
        component,
        "task-reaping",
        [LifecycleInvariant::RetainedTasksBoundedByActiveConcurrency],
    )
}

fn start_reference(
    adapter: &ReferenceAdapter,
    operation_id: &str,
) -> Result<(ReferenceTicket, ReferenceLease), crate::model::AdapterError> {
    let (ticket, lease) = OperationModelAdapter::reserve(adapter, operation_id)?.into_parts();
    OperationModelAdapter::queue(adapter, &lease)?;
    OperationModelAdapter::start(adapter, &lease)?;
    Ok((ticket, lease))
}

#[cfg(test)]
#[derive(Clone)]
struct ReferenceAttemptHierarchyAdapter {
    state: Arc<Mutex<ReferenceAttemptHierarchyState>>,
}

#[cfg(test)]
struct ReferenceAttemptHierarchyState {
    next_sequence: u64,
    operations: BTreeMap<String, ReferenceAttemptHierarchyOperation>,
}

#[cfg(test)]
struct ReferenceAttemptHierarchyOperation {
    cancellation_requested: bool,
    attempts: BTreeMap<u64, AttemptIdentity>,
}

#[cfg(test)]
#[derive(Clone)]
struct ReferenceHierarchyOperation(String);

#[cfg(test)]
#[derive(Clone)]
struct ReferenceHierarchyAttempt(AttemptIdentity);

#[cfg(test)]
#[derive(Clone)]
struct ReferenceSupervisorBridge {
    inner: Arc<ReferenceSupervisorInner>,
}

#[cfg(test)]
struct ReferenceSupervisorInner {
    state: Mutex<ReferenceSupervisorState>,
    state_changed: std::sync::Condvar,
}

#[cfg(test)]
struct ReferenceSupervisorState {
    lifecycle: LifecyclePhase,
    next_sequence: u64,
    records: BTreeMap<String, OperationSnapshot>,
    active: BTreeMap<String, u64>,
    tasks: BTreeMap<String, Option<thread::JoinHandle<()>>>,
    expected_worker_ids: Vec<String>,
    joined_worker_ids: Vec<String>,
    progress_capacity: usize,
}

#[cfg(test)]
#[derive(Clone)]
struct ReferenceControlledOperation(Arc<ReferenceControlledOperationInner>);

#[cfg(test)]
struct ReferenceControlledOperationInner {
    operation_id: String,
    command: Mutex<Option<mpsc::SyncSender<ReferenceWorkerCommand>>>,
    released: Mutex<Option<mpsc::Receiver<()>>>,
    allow_exit: Mutex<Option<mpsc::SyncSender<()>>>,
}

#[cfg(test)]
enum ReferenceWorkerCommand {
    Finish(TerminalClass),
    Panic,
}

#[cfg(test)]
impl ReferenceSupervisorBridge {
    fn lock(&self) -> MutexGuard<'_, ReferenceSupervisorState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn facts(state: &ReferenceSupervisorState) -> ClosedFacts {
        ClosedFacts {
            lifecycle: state.lifecycle,
            active_operations: state.active.len(),
            retained_tasks: state.tasks.len(),
            expected_workers: state.expected_worker_ids.len(),
            joined_workers: state.joined_worker_ids.len(),
        }
    }

    fn quiesce_inner(&self) {
        let mut state = self.lock();
        if state.lifecycle == LifecyclePhase::Running {
            state.lifecycle = LifecyclePhase::Quiescing;
            let active_ids = state.active.keys().cloned().collect::<Vec<_>>();
            for operation_id in active_ids {
                if let Some(snapshot) = state.records.get_mut(&operation_id) {
                    snapshot.cancellation_requested = true;
                }
            }
        }
        self.inner.state_changed.notify_all();
    }

    fn finish_shutdown(&self) -> ShutdownOutcome {
        let mut state = self.lock();
        while !state.active.is_empty() {
            state = self
                .inner
                .state_changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        let tasks = state
            .tasks
            .iter_mut()
            .map(|(worker_id, handle)| {
                (
                    worker_id.clone(),
                    handle
                        .take()
                        .expect("retained task must own its JoinHandle"),
                )
            })
            .collect::<Vec<_>>();
        drop(state);

        let mut joined = Vec::with_capacity(tasks.len());
        for (worker_id, handle) in tasks {
            handle.join().expect("controlled worker must not panic");
            joined.push(worker_id);
        }

        let mut state = self.lock();
        for worker_id in &joined {
            state.tasks.remove(worker_id);
            state.joined_worker_ids.push(worker_id.clone());
        }
        state.lifecycle = LifecyclePhase::Closed;
        let facts = Self::facts(&state);
        ShutdownOutcome {
            facts,
            expected_worker_ids: state.expected_worker_ids.clone(),
            joined_worker_ids: state.joined_worker_ids.clone(),
        }
    }
}

#[cfg(test)]
struct ReferenceShutdownWitness {
    started: Mutex<Option<mpsc::Receiver<()>>>,
    result: Mutex<(mpsc::Receiver<ShutdownOutcome>, Option<ShutdownOutcome>)>,
    shutdown_task: Option<thread::JoinHandle<()>>,
}

#[cfg(test)]
#[derive(Debug, thiserror::Error)]
enum ReferenceShutdownWitnessError {
    #[error("{0} witness may be consumed only once")]
    AlreadyConsumed(&'static str),
    #[error("shutdown worker disconnected before {0}")]
    Disconnected(&'static str),
    #[error("shutdown worker panicked")]
    WorkerPanicked,
}

#[cfg(test)]
impl ShutdownWitness for ReferenceShutdownWitness {
    type Error = ReferenceShutdownWitnessError;

    fn wait_started(&self, timeout: Duration) -> Result<(), Self::Error> {
        self.started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(ReferenceShutdownWitnessError::AlreadyConsumed("start"))?
            .recv_timeout(timeout)
            .map_err(|_| ReferenceShutdownWitnessError::Disconnected("quiescing phase"))
    }

    fn try_complete(&self) -> Result<Option<ShutdownOutcome>, Self::Error> {
        use std::sync::mpsc::TryRecvError;

        let mut result = self
            .result
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if result.1.is_none() {
            match result.0.try_recv() {
                Ok(facts) => result.1 = Some(facts),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    return Err(ReferenceShutdownWitnessError::Disconnected("completion"));
                }
            }
        }
        Ok(result.1.clone())
    }

    fn wait(mut self, timeout: Duration) -> Result<ShutdownOutcome, Self::Error> {
        let result = {
            let mut result = self
                .result
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match result.1.take() {
                Some(facts) => facts,
                None => result
                    .0
                    .recv_timeout(timeout)
                    .map_err(|_| ReferenceShutdownWitnessError::Disconnected("completion"))?,
            }
        };
        self.shutdown_task
            .take()
            .ok_or(ReferenceShutdownWitnessError::AlreadyConsumed(
                "shutdown task handle",
            ))?
            .join()
            .map_err(|_| ReferenceShutdownWitnessError::WorkerPanicked)?;
        Ok(result)
    }
}

#[cfg(test)]
impl ReferenceAttemptHierarchyAdapter {
    fn state(&self) -> MutexGuard<'_, ReferenceAttemptHierarchyState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
impl AttemptHierarchyAdapter for ReferenceAttemptHierarchyAdapter {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Operation = ReferenceHierarchyOperation;
    type Attempt = ReferenceHierarchyAttempt;

    fn deterministic() -> Self {
        Self {
            state: Arc::new(Mutex::new(ReferenceAttemptHierarchyState {
                next_sequence: 1,
                operations: BTreeMap::new(),
            })),
        }
    }
    fn create_operation(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        let mut state = self.state();
        if state.operations.contains_key(operation_id) {
            return Err(crate::model::AdapterError::DuplicateOperation);
        }
        state.operations.insert(
            operation_id.to_owned(),
            ReferenceAttemptHierarchyOperation {
                cancellation_requested: false,
                attempts: BTreeMap::new(),
            },
        );
        Ok(ReferenceHierarchyOperation(operation_id.to_owned()))
    }
    fn start_attempt(&self, operation: &Self::Operation) -> Result<Self::Attempt, Self::Error> {
        let mut state = self.state();
        let sequence = state.next_sequence;
        state.next_sequence = sequence
            .checked_add(1)
            .ok_or(crate::model::AdapterError::SequenceExhausted)?;
        let owner = state
            .operations
            .get_mut(&operation.0)
            .ok_or(crate::model::AdapterError::UnknownOperation)?;
        let identity = AttemptIdentity {
            operation_id: operation.0.clone(),
            attempt_id: format!("{}#{sequence}", operation.0),
            sequence,
        };
        owner.attempts.insert(sequence, identity.clone());
        Ok(ReferenceHierarchyAttempt(identity))
    }
    fn attempt_identity(&self, attempt: &Self::Attempt) -> AttemptIdentity {
        attempt.0.clone()
    }
    fn operation_active(&self, operation: &Self::Operation) -> bool {
        self.state().operations.contains_key(&operation.0)
    }
    fn active_attempts(&self, operation: &Self::Operation) -> Vec<AttemptIdentity> {
        self.state()
            .operations
            .get(&operation.0)
            .map(|owner| owner.attempts.values().cloned().collect())
            .unwrap_or_default()
    }
    fn request_operation_cancel(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        self.state()
            .operations
            .get_mut(&operation.0)
            .ok_or(crate::model::AdapterError::UnknownOperation)?
            .cancellation_requested = true;
        Ok(())
    }
    fn cancellation_requested(&self, attempt: &Self::Attempt) -> bool {
        self.state()
            .operations
            .get(&attempt.0.operation_id)
            .is_some_and(|owner| {
                owner.cancellation_requested && owner.attempts.contains_key(&attempt.0.sequence)
            })
    }
    fn finish_attempt(&self, attempt: Self::Attempt) -> Result<(), Self::Error> {
        let removed = self
            .state()
            .operations
            .get_mut(&attempt.0.operation_id)
            .ok_or(crate::model::AdapterError::UnknownOperation)?
            .attempts
            .remove(&attempt.0.sequence);
        if removed.as_ref() != Some(&attempt.0) {
            return Err(crate::model::AdapterError::StaleLease);
        }
        Ok(())
    }
    fn finish_operation(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        let mut state = self.state();
        if !state
            .operations
            .get(&operation.0)
            .ok_or(crate::model::AdapterError::UnknownOperation)?
            .attempts
            .is_empty()
        {
            return Err(crate::model::AdapterError::InvalidTransition);
        }
        state.operations.remove(&operation.0);
        Ok(())
    }
}

fn finish_reference(
    adapter: &ReferenceAdapter,
    lease: &ReferenceLease,
    class: TerminalClass,
) -> Result<(), crate::model::AdapterError> {
    OperationModelAdapter::terminal(adapter, lease, class)?;
    OperationModelAdapter::release(adapter, lease)
}

impl TransitionChainAdapter for ReferenceAdapter {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Operation = (ReferenceTicket, ReferenceLease);

    fn deterministic() -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig::default())
    }
    fn reserve(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        Ok(OperationModelAdapter::reserve(self, operation_id)?.into_parts())
    }
    fn phase(&self, operation: &Self::Operation) -> Option<OperationPhase> {
        OperationModelAdapter::lease_snapshot(self, &operation.1).map(|value| value.phase)
    }
    fn queue(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        OperationModelAdapter::queue(self, &operation.1)
    }
    fn start(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        OperationModelAdapter::start(self, &operation.1)
    }
    fn terminal(
        &self,
        operation: &Self::Operation,
        class: TerminalClass,
    ) -> Result<(), Self::Error> {
        OperationModelAdapter::terminal(self, &operation.1, class)
    }
    fn release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        OperationModelAdapter::release(self, &operation.1)
    }
}

impl RegistryIdentityAdapter for ReferenceAdapter {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Guard = ReferenceTicket;
    type Lease = ReferenceLease;

    fn deterministic(next_sequence: u64) -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig {
            next_sequence,
            progress_capacity: 1,
        })
    }
    fn reserve(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error> {
        Ok(OperationModelAdapter::reserve(self, operation_id)?.into_parts())
    }
    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity {
        OperationModelAdapter::lease_identity(self, lease)
    }
    fn complete_and_release(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        OperationModelAdapter::queue(self, lease)?;
        OperationModelAdapter::start(self, lease)?;
        finish_reference(self, lease, TerminalClass::Completed)
    }
    fn active_count(&self) -> usize {
        OperationModelAdapter::active_count(self)
    }
    fn current_identity(&self, operation_id: &str) -> Option<AttemptIdentity> {
        OperationModelAdapter::current_snapshot(self, operation_id).map(|value| value.identity)
    }
}

impl ConsumerCancellationAdapter for ReferenceAdapter {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Ticket = ReferenceTicket;
    type Lease = ReferenceLease;
    fn deterministic() -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig::default())
    }
    fn start(&self, operation_id: &str) -> Result<(Self::Ticket, Self::Lease), Self::Error> {
        start_reference(self, operation_id)
    }
    fn ticket_identity(&self, ticket: &Self::Ticket) -> AttemptIdentity {
        OperationModelAdapter::ticket_identity(self, ticket)
    }
    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity {
        OperationModelAdapter::lease_identity(self, lease)
    }
    fn active_count(&self) -> usize {
        OperationModelAdapter::active_count(self)
    }
    fn current_snapshot(&self, operation_id: &str) -> Option<OperationSnapshot> {
        OperationModelAdapter::current_snapshot(self, operation_id)
    }
    fn lease_snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        OperationModelAdapter::lease_snapshot(self, lease)
    }
    fn cancellation_requested(&self, lease: &Self::Lease) -> bool {
        OperationModelAdapter::lease_snapshot(self, lease)
            .is_some_and(|value| value.cancellation_requested)
    }
    fn explicit_consumer_drop(&self, ticket: Self::Ticket) -> Result<(), Self::Error> {
        OperationModelAdapter::consumer_drop(self, ticket)
    }
    fn finish_cancelled(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        finish_reference(self, lease, TerminalClass::Cancelled)
    }
}

impl TerminalAuthorityAdapter for ReferenceAdapter {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Guard = ReferenceTicket;
    type Lease = ReferenceLease;
    fn deterministic() -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig::default())
    }
    fn start(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error> {
        start_reference(self, operation_id)
    }
    fn terminal(&self, lease: &Self::Lease, class: TerminalClass) -> Result<(), Self::Error> {
        OperationModelAdapter::terminal(self, lease, class)
    }
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        OperationModelAdapter::lease_snapshot(self, lease)
    }
    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        OperationModelAdapter::release(self, lease)
    }
}

impl WaiterControlAdapter for ReferenceAdapter {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Ticket = ReferenceTicket;
    type Lease = ReferenceLease;
    fn deterministic() -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig::default())
    }
    fn start(&self, operation_id: &str) -> Result<(Self::Ticket, Self::Lease), Self::Error> {
        start_reference(self, operation_id)
    }
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        OperationModelAdapter::lease_snapshot(self, lease)
    }
    fn waiter_timeout(&self, ticket: &Self::Ticket) -> Result<WaitObservation, Self::Error> {
        OperationModelAdapter::waiter_timeout(self, ticket)
    }
    fn request_cancel(&self, ticket: &Self::Ticket) -> Result<(), Self::Error> {
        OperationModelAdapter::request_cancel(self, ticket)
    }
    fn finish_cancelled(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        finish_reference(self, lease, TerminalClass::Cancelled)
    }
}

#[cfg(test)]
impl AdmissionQuiesceShutdownBridgeAdapter for ReferenceSupervisorBridge {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Operation = ReferenceControlledOperation;
    type ShutdownWitness = ReferenceShutdownWitness;
    fn deterministic() -> Self {
        Self {
            inner: Arc::new(ReferenceSupervisorInner {
                state: Mutex::new(ReferenceSupervisorState {
                    lifecycle: LifecyclePhase::Running,
                    next_sequence: 1,
                    records: BTreeMap::new(),
                    active: BTreeMap::new(),
                    tasks: BTreeMap::new(),
                    expected_worker_ids: Vec::new(),
                    joined_worker_ids: Vec::new(),
                    progress_capacity: 4,
                }),
                state_changed: std::sync::Condvar::new(),
            }),
        }
    }
    fn reserve(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        let (command_tx, command_rx) = mpsc::sync_channel(0);
        let (released_tx, released_rx) = mpsc::sync_channel(0);
        let (allow_exit_tx, allow_exit_rx) = mpsc::sync_channel(0);
        let worker_id;
        let identity;
        {
            let mut state = self.lock();
            if state.lifecycle != LifecyclePhase::Running {
                return Err(crate::model::AdapterError::AdmissionClosed);
            }
            if state.active.contains_key(operation_id) {
                return Err(crate::model::AdapterError::DuplicateOperation);
            }
            let sequence = state.next_sequence;
            state.next_sequence = sequence
                .checked_add(1)
                .ok_or(crate::model::AdapterError::SequenceExhausted)?;
            worker_id = format!("worker-{sequence}");
            identity = AttemptIdentity {
                operation_id: operation_id.to_owned(),
                attempt_id: format!("{operation_id}#{sequence}"),
                sequence,
            };
            state.active.insert(operation_id.to_owned(), sequence);
            state.records.insert(
                operation_id.to_owned(),
                OperationSnapshot {
                    identity: identity.clone(),
                    phase: OperationPhase::Running,
                    cancellation_requested: false,
                    authoritative_terminal: None,
                    final_projection: None,
                    progress_projection: Vec::new(),
                },
            );
            state.expected_worker_ids.push(worker_id.clone());
        }

        let adapter = self.clone();
        let worker_operation_id = operation_id.to_owned();
        let handle = thread::spawn(move || {
            let command = command_rx.recv().expect("controlled worker command sender");
            let class = match std::panic::catch_unwind(|| match command {
                ReferenceWorkerCommand::Finish(class) => class,
                ReferenceWorkerCommand::Panic => panic!("controlled executor panic"),
            }) {
                Ok(class) => class,
                Err(_) => TerminalClass::Failed,
            };
            {
                let mut state = adapter.lock();
                let snapshot = state
                    .records
                    .get_mut(&worker_operation_id)
                    .expect("controlled operation record");
                let terminal = TerminalRecord {
                    class,
                    sequence: snapshot.identity.sequence,
                };
                snapshot.phase = OperationPhase::Released;
                snapshot.authoritative_terminal = Some(terminal);
                snapshot.final_projection = Some(terminal);
                state.active.remove(&worker_operation_id);
                adapter.inner.state_changed.notify_all();
            }
            released_tx.send(()).expect("controlled release receiver");
            allow_exit_rx.recv().expect("controlled exit sender");
        });
        self.lock().tasks.insert(worker_id, Some(handle));
        Ok(ReferenceControlledOperation(Arc::new(
            ReferenceControlledOperationInner {
                operation_id: operation_id.to_owned(),
                command: Mutex::new(Some(command_tx)),
                released: Mutex::new(Some(released_rx)),
                allow_exit: Mutex::new(Some(allow_exit_tx)),
            },
        )))
    }
    fn quiesce(&self) {
        self.quiesce_inner();
    }
    fn phase(&self) -> LifecyclePhase {
        self.lock().lifecycle
    }
    fn active_count(&self) -> usize {
        self.lock().active.len()
    }
    fn retained_task_count(&self) -> usize {
        self.lock().tasks.len()
    }
    fn cancellation_requested(&self, operation_id: &str) -> bool {
        self.lock()
            .records
            .get(operation_id)
            .is_some_and(|snapshot| snapshot.cancellation_requested)
    }
    fn request_cancelled_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation
            .0
            .command
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(crate::model::AdapterError::InvalidTransition)?
            .send(ReferenceWorkerCommand::Finish(TerminalClass::Cancelled))
            .map_err(|_| crate::model::AdapterError::UnknownOperation)
    }
    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        operation
            .0
            .released
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(crate::model::AdapterError::InvalidTransition)?
            .recv_timeout(timeout)
            .map_err(|_| crate::model::AdapterError::UnknownOperation)?;
        self.lock()
            .records
            .get(&operation.0.operation_id)
            .cloned()
            .ok_or(crate::model::AdapterError::UnknownOperation)
    }
    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation
            .0
            .allow_exit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(crate::model::AdapterError::InvalidTransition)?
            .send(())
            .map_err(|_| crate::model::AdapterError::UnknownOperation)
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        let adapter = self.clone();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let shutdown_task = thread::spawn(move || {
            adapter.quiesce_inner();
            started_tx.send(()).expect("shutdown start receiver");
            result_tx
                .send(adapter.finish_shutdown())
                .expect("shutdown result receiver");
        });
        ReferenceShutdownWitness {
            started: Mutex::new(Some(started_rx)),
            result: Mutex::new((result_rx, None)),
            shutdown_task: Some(shutdown_task),
        }
    }
    fn shutdown(&self) -> ClosedFacts {
        Self::facts(&self.lock())
    }
}

#[cfg(test)]
impl ProgressShutdownBridgeAdapter for ReferenceSupervisorBridge {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type UnreadProgress = ();
    type Operation = ReferenceControlledOperation;
    type ShutdownWitness = ReferenceShutdownWitness;
    fn deterministic(progress_capacity: usize) -> Self {
        let adapter = <Self as AdmissionQuiesceShutdownBridgeAdapter>::deterministic();
        adapter.lock().progress_capacity = progress_capacity;
        adapter
    }
    fn start(
        &self,
        operation_id: &str,
    ) -> Result<(Self::UnreadProgress, Self::Operation), Self::Error> {
        Ok((
            (),
            <Self as AdmissionQuiesceShutdownBridgeAdapter>::reserve(self, operation_id)?,
        ))
    }
    fn publish_progress(
        &self,
        operation: &Self::Operation,
        sequence: u64,
    ) -> Result<(), Self::Error> {
        let mut state = self.lock();
        let capacity = state.progress_capacity;
        let snapshot = state
            .records
            .get_mut(&operation.0.operation_id)
            .ok_or(crate::model::AdapterError::UnknownOperation)?;
        if snapshot.phase != OperationPhase::Running {
            return Err(crate::model::AdapterError::InvalidTransition);
        }
        if snapshot.progress_projection.len() == capacity {
            snapshot.progress_projection.remove(0);
        }
        snapshot.progress_projection.push(sequence);
        Ok(())
    }
    fn snapshot(&self, operation: &Self::Operation) -> Option<OperationSnapshot> {
        self.lock().records.get(&operation.0.operation_id).cloned()
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        <Self as AdmissionQuiesceShutdownBridgeAdapter>::begin_shutdown(self)
    }
    fn request_completed_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation
            .0
            .command
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(crate::model::AdapterError::InvalidTransition)?
            .send(ReferenceWorkerCommand::Finish(TerminalClass::Completed))
            .map_err(|_| crate::model::AdapterError::UnknownOperation)
    }
    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        <Self as AdmissionQuiesceShutdownBridgeAdapter>::wait_released(self, operation, timeout)
    }
    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        <Self as AdmissionQuiesceShutdownBridgeAdapter>::allow_worker_exit(self, operation)
    }
    fn progress_capacity(&self) -> usize {
        self.lock().progress_capacity
    }
}

#[cfg(test)]
impl PanicShutdownBridgeAdapter for ReferenceSupervisorBridge {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Operation = ReferenceControlledOperation;
    type ShutdownWitness = ReferenceShutdownWitness;
    fn deterministic() -> Self {
        <Self as AdmissionQuiesceShutdownBridgeAdapter>::deterministic()
    }
    fn run_controlled_panicking_operation(
        &self,
        operation_id: &str,
    ) -> Result<Self::Operation, Self::Error> {
        let operation =
            <Self as AdmissionQuiesceShutdownBridgeAdapter>::reserve(self, operation_id)?;
        operation
            .0
            .command
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(crate::model::AdapterError::InvalidTransition)?
            .send(ReferenceWorkerCommand::Panic)
            .map_err(|_| crate::model::AdapterError::UnknownOperation)?;
        Ok(operation)
    }
    fn wait_failed_release(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        let snapshot = <Self as AdmissionQuiesceShutdownBridgeAdapter>::wait_released(
            self, operation, timeout,
        )?;
        <Self as AdmissionQuiesceShutdownBridgeAdapter>::allow_worker_exit(self, operation)?;
        Ok(snapshot)
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        <Self as AdmissionQuiesceShutdownBridgeAdapter>::begin_shutdown(self)
    }
}

impl StableShutdownAdapter for ReferenceAdapter {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Operation = (ReferenceTicket, ReferenceLease);
    fn deterministic() -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig::default())
    }
    fn start(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        start_reference(self, operation_id)
    }
    fn finish(&self, operation: Self::Operation) -> Result<(), Self::Error> {
        let (ticket, lease) = operation;
        finish_reference(self, &lease, TerminalClass::Completed)?;
        drop(ticket);
        Ok(())
    }
    fn shutdown(&self) -> ClosedFacts {
        OperationModelAdapter::shutdown(self)
    }
}

impl TaskReapingAdapter for ReferenceAdapter {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Operation = (ReferenceTicket, ReferenceLease);
    fn deterministic() -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig::default())
    }
    fn start(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        start_reference(self, operation_id)
    }
    fn finish(&self, operation: Self::Operation) -> Result<(), Self::Error> {
        let (ticket, lease) = operation;
        finish_reference(self, &lease, TerminalClass::Completed)?;
        drop(ticket);
        Ok(())
    }
    fn active_count(&self) -> usize {
        OperationModelAdapter::active_count(self)
    }
    fn retained_task_count(&self) -> usize {
        OperationModelAdapter::retained_task_count(self)
    }
    fn shutdown(&self) -> ClosedFacts {
        OperationModelAdapter::shutdown(self)
    }
}

fn assert_terminal(snapshot: &OperationSnapshot, expected: TerminalClass) {
    assert_eq!(snapshot.phase, OperationPhase::Terminal);
    let authoritative = snapshot
        .authoritative_terminal
        .expect("authoritative terminal must exist");
    let final_projection = snapshot
        .final_projection
        .expect("final projection must exist");
    assert_eq!(authoritative, final_projection);
    assert_eq!(authoritative.class, expected);
}

fn assert_cancelled_release(snapshot: &OperationSnapshot) {
    assert_eq!(snapshot.phase, OperationPhase::Released);
    assert_eq!(snapshot.authoritative_terminal, snapshot.final_projection);
    assert_eq!(
        snapshot
            .authoritative_terminal
            .expect("authoritative terminal")
            .class,
        TerminalClass::Cancelled
    );
}

fn assert_closed_and_empty(facts: ClosedFacts) {
    assert_eq!(facts.lifecycle, LifecyclePhase::Closed);
    assert_eq!(facts.active_operations, 0);
    assert_eq!(facts.retained_tasks, 0);
    assert!(
        facts.expected_workers > 0,
        "executed work must identify a worker"
    );
    assert_eq!(facts.joined_workers, facts.expected_workers);
}

fn assert_shutdown_outcome(outcome: &ShutdownOutcome) {
    assert_closed_and_empty(outcome.facts);
    assert!(!outcome.expected_worker_ids.is_empty());
    assert_eq!(
        outcome.expected_worker_ids.len(),
        outcome.facts.expected_workers
    );
    assert_eq!(
        outcome.joined_worker_ids.len(),
        outcome.facts.joined_workers
    );
    assert_eq!(outcome.joined_worker_ids, outcome.expected_worker_ids);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AcceptanceError, LifecycleCoverageManifest};

    #[test]
    fn composed_reference_suites_cover_every_normative_invariant() {
        let evidence = vec![
            run_transition_chain_suite::<ReferenceAdapter>("state-machine"),
            run_registry_identity_suite::<ReferenceAdapter>("registry"),
            run_attempt_hierarchy_suite::<ReferenceAttemptHierarchyAdapter>("attempt-supervisor"),
            run_consumer_cancellation_suite::<ReferenceAdapter>("consumer-control"),
            run_terminal_authority_suite::<ReferenceAdapter>("terminal-owner"),
            run_waiter_control_suite::<ReferenceAdapter>("waiter"),
            run_admission_quiesce_shutdown_bridge_suite::<ReferenceSupervisorBridge>(
                "supervisor-bridge",
            ),
            run_progress_shutdown_bridge_suite::<ReferenceSupervisorBridge>("progress-bridge"),
            run_panic_shutdown_bridge_suite::<ReferenceSupervisorBridge>("panic-bridge"),
            run_stable_shutdown_suite::<ReferenceAdapter>("shutdown"),
            run_task_reaping_suite::<ReferenceAdapter>("task-supervisor"),
        ];
        let manifest = LifecycleCoverageManifest::<ReferenceLifecycle>::accept(evidence)
            .expect("complete composed coverage");
        assert_eq!(manifest.covered().count(), 18);
        assert_eq!(manifest.components().count(), 11);
        assert_eq!(manifest.product(), "reference-product");
        assert_eq!(manifest.implementation(), "reference-lifecycle");
    }

    #[test]
    fn partial_evidence_cannot_pass_acceptance() {
        let partial = vec![run_registry_identity_suite::<ReferenceAdapter>("registry")];
        assert!(matches!(
            LifecycleCoverageManifest::<ReferenceLifecycle>::accept(partial),
            Err(AcceptanceError::MissingSuites(_))
        ));
    }

    #[test]
    fn duplicate_suite_evidence_cannot_pass_acceptance() {
        let duplicate = vec![
            run_transition_chain_suite::<ReferenceAdapter>("state-machine"),
            run_transition_chain_suite::<ReferenceAdapter>("shadow-state-machine"),
        ];
        assert!(matches!(
            LifecycleCoverageManifest::<ReferenceLifecycle>::accept(duplicate),
            Err(AcceptanceError::DuplicateSuite("transition-chain"))
        ));
    }
}
