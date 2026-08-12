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
use std::sync::{Mutex, mpsc};
use std::thread;

use crate::barrier::DeterministicBarrier;
use crate::coverage::{CoverageEvidence, LifecycleImplementation, LifecycleInvariant};
use crate::model::{
    AttemptIdentity, ClosedFacts, LifecyclePhase, OperationModelAdapter, OperationPhase,
    OperationSnapshot, ReferenceAdapter, ReferenceLease, ReferenceTicket, TerminalClass,
    TestConfig, WaitObservation,
};

/// Fixed identity of the testkit's deterministic reference implementation.
pub enum ReferenceLifecycle {}

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
    fn cancellation_requested(&self, operation_id: &str) -> bool;
    fn finish_cancelled(
        &self,
        operation: &Self::Operation,
    ) -> Result<OperationSnapshot, Self::Error>;
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

    fn wait_started(&self) -> Result<(), Self::Error>;
    fn try_complete(&self) -> Result<Option<ClosedFacts>, Self::Error>;
    fn wait_release_observed(&self) -> Result<(), Self::Error>;
    fn allow_worker_exit(&self) -> Result<(), Self::Error>;
    fn wait(self) -> Result<ClosedFacts, Self::Error>;
}

/// Integrated proof that progress backpressure cannot starve terminal release
/// or successful shutdown. Keeping this as one trait preserves the cross-layer
/// invariant when progress and shutdown have different internal owners.
pub trait ProgressShutdownBridgeAdapter: Clone + Sized + Send + Sync + 'static {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug + Send + 'static;
    type Guard;
    type Lease: Clone + Send + Sync + 'static;

    fn deterministic(progress_capacity: usize) -> Self;
    fn start(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error>;
    fn publish_progress(&self, lease: &Self::Lease, sequence: u64) -> Result<(), Self::Error>;
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot>;
    fn terminal(&self, lease: &Self::Lease, class: TerminalClass) -> Result<(), Self::Error>;
    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
    fn progress_capacity(&self) -> usize;
    fn shutdown(&self) -> ClosedFacts;
}

/// Integrated proof that executor panic becomes a safe failed terminal and
/// does not strand registry, task, or worker ownership at shutdown.
pub trait PanicShutdownBridgeAdapter: Clone + Sized {
    type Implementation: LifecycleImplementation;
    type Error: std::fmt::Debug;
    type Guard;
    type Lease;

    fn deterministic() -> Self;
    fn start(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error>;
    fn run_executor(
        &self,
        lease: &Self::Lease,
        executor: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<(), Self::Error>;
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot>;
    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
    fn shutdown(&self) -> ClosedFacts;
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
        let terminal = adapter
            .finish_cancelled(&operation)
            .expect("finish racing admission");
        assert_cancelled_release(&terminal);
        assert_eq!(adapter.active_count(), 0);
    }
    assert!(adapter.reserve("post-quiesce").is_err());

    let adapter = A::deterministic();
    let operation = adapter.reserve("active-shutdown").expect("reserve active");
    let shutdown = adapter.begin_shutdown();
    shutdown.wait_started().expect("shutdown reaches quiescing");
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
    let terminal = adapter
        .finish_cancelled(&operation)
        .expect("active executor terminal and release");
    assert_cancelled_release(&terminal);
    assert_eq!(adapter.active_count(), 0);
    shutdown
        .wait_release_observed()
        .expect("shutdown observes executor release before worker exit");
    assert!(
        shutdown
            .try_complete()
            .expect("inspect shutdown before worker exit")
            .is_none()
    );
    shutdown
        .allow_worker_exit()
        .expect("allow the released worker to exit");
    let closed = shutdown.wait().expect("shutdown joins released worker");
    assert_closed_and_empty(closed);
    assert_eq!(adapter.shutdown(), closed);
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
    let (unread_progress, lease) = adapter.start("saturated-progress").expect("start");
    for sequence in 0..32 {
        adapter
            .publish_progress(&lease, sequence)
            .expect("progress");
    }
    let snapshot = adapter.snapshot(&lease).expect("progress snapshot");
    assert!(snapshot.progress_projection.len() <= adapter.progress_capacity());
    adapter
        .terminal(&lease, TerminalClass::Completed)
        .expect("terminal bypasses progress");
    assert_completed_terminal(&adapter.snapshot(&lease).expect("terminal snapshot"));
    adapter.release(&lease).expect("release bypasses progress");
    assert_closed_and_empty(adapter.shutdown());
    drop(unread_progress);

    let adapter = A::deterministic(2);
    let (unread_progress, lease) = adapter.start("progress-terminal-race").expect("start");
    let barrier = DeterministicBarrier::new(2);
    let progress_adapter = adapter.clone();
    let progress_lease = lease.clone();
    let progress_barrier = barrier.clone();
    let progress = thread::spawn(move || {
        progress_barrier.arrive_and_wait();
        for sequence in 0..1024 {
            if progress_adapter
                .publish_progress(&progress_lease, sequence)
                .is_err()
            {
                break;
            }
        }
    });
    let terminal_adapter = adapter.clone();
    let terminal_lease = lease.clone();
    let terminal_barrier = barrier.clone();
    let terminal = thread::spawn(move || {
        terminal_barrier.arrive_and_wait();
        terminal_adapter
            .terminal(&terminal_lease, TerminalClass::Completed)
            .is_ok()
    });
    barrier.wait_until_all_arrived();
    barrier.release();
    progress.join().expect("progress producer");
    assert!(terminal.join().expect("terminal producer"));
    assert_eq!(
        adapter.snapshot(&lease).expect("terminal").phase,
        OperationPhase::Terminal
    );
    assert_completed_terminal(&adapter.snapshot(&lease).expect("terminal snapshot"));
    adapter.release(&lease).expect("release");
    assert_closed_and_empty(adapter.shutdown());
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
    let (guard, lease) = adapter.start("panic").expect("start");
    adapter
        .run_executor(&lease, Box::new(|| panic!("controlled executor panic")))
        .expect("panic boundary records terminal");
    let snapshot = adapter.snapshot(&lease).expect("panic terminal");
    assert_eq!(snapshot.phase, OperationPhase::Terminal);
    assert_eq!(snapshot.authoritative_terminal, snapshot.final_projection);
    assert_eq!(
        snapshot
            .authoritative_terminal
            .expect("authoritative terminal")
            .class,
        TerminalClass::Failed
    );
    adapter.release(&lease).expect("panic release");
    drop(guard);
    assert_closed_and_empty(adapter.shutdown());
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

pub struct ReferenceShutdownWitness {
    started: Mutex<Option<mpsc::Receiver<()>>>,
    release_observed: Mutex<Option<mpsc::Receiver<()>>>,
    allow_exit: Mutex<Option<mpsc::SyncSender<()>>>,
    result: Mutex<(mpsc::Receiver<ClosedFacts>, Option<ClosedFacts>)>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReferenceShutdownWitnessError {
    #[error("{0} witness may be consumed only once")]
    AlreadyConsumed(&'static str),
    #[error("shutdown worker disconnected before {0}")]
    Disconnected(&'static str),
    #[error("shutdown worker panicked")]
    WorkerPanicked,
}

impl ShutdownWitness for ReferenceShutdownWitness {
    type Error = ReferenceShutdownWitnessError;

    fn wait_started(&self) -> Result<(), Self::Error> {
        self.started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(ReferenceShutdownWitnessError::AlreadyConsumed("start"))?
            .recv()
            .map_err(|_| ReferenceShutdownWitnessError::Disconnected("quiescing phase"))
    }

    fn try_complete(&self) -> Result<Option<ClosedFacts>, Self::Error> {
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
        Ok(result.1)
    }

    fn wait_release_observed(&self) -> Result<(), Self::Error> {
        self.release_observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(ReferenceShutdownWitnessError::AlreadyConsumed(
                "release observation",
            ))?
            .recv()
            .map_err(|_| ReferenceShutdownWitnessError::Disconnected("release observation"))
    }

    fn allow_worker_exit(&self) -> Result<(), Self::Error> {
        self.allow_exit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(ReferenceShutdownWitnessError::AlreadyConsumed(
                "worker-exit gate",
            ))?
            .send(())
            .map_err(|_| ReferenceShutdownWitnessError::Disconnected("worker-exit gate"))
    }

    fn wait(mut self) -> Result<ClosedFacts, Self::Error> {
        let result = {
            let mut result = self
                .result
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match result.1.take() {
                Some(facts) => facts,
                None => result
                    .0
                    .recv()
                    .map_err(|_| ReferenceShutdownWitnessError::Disconnected("completion"))?,
            }
        };
        self.worker
            .take()
            .ok_or(ReferenceShutdownWitnessError::AlreadyConsumed(
                "worker handle",
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

impl AdmissionQuiesceShutdownBridgeAdapter for ReferenceAdapter {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Operation = (ReferenceTicket, ReferenceLease);
    type ShutdownWitness = ReferenceShutdownWitness;
    fn deterministic() -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig::default())
    }
    fn reserve(&self, operation_id: &str) -> Result<Self::Operation, Self::Error> {
        Ok(OperationModelAdapter::reserve(self, operation_id)?.into_parts())
    }
    fn quiesce(&self) {
        OperationModelAdapter::quiesce(self);
    }
    fn phase(&self) -> LifecyclePhase {
        OperationModelAdapter::lifecycle_phase(self)
    }
    fn active_count(&self) -> usize {
        OperationModelAdapter::active_count(self)
    }
    fn cancellation_requested(&self, operation_id: &str) -> bool {
        OperationModelAdapter::current_snapshot(self, operation_id)
            .is_some_and(|value| value.cancellation_requested)
    }
    fn finish_cancelled(
        &self,
        operation: &Self::Operation,
    ) -> Result<OperationSnapshot, Self::Error> {
        OperationModelAdapter::queue(self, &operation.1)?;
        OperationModelAdapter::start(self, &operation.1)?;
        finish_reference(self, &operation.1, TerminalClass::Cancelled)?;
        OperationModelAdapter::lease_snapshot(self, &operation.1)
            .ok_or(crate::model::AdapterError::UnknownOperation)
    }
    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        let adapter = self.clone();
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let (allow_exit_tx, allow_exit_rx) = mpsc::sync_channel(0);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            OperationModelAdapter::quiesce(&adapter);
            started_tx.send(()).expect("shutdown start receiver");
            let facts = adapter.wait_for_shutdown();
            release_tx.send(()).expect("release observation receiver");
            allow_exit_rx.recv().expect("worker-exit gate sender");
            result_tx.send(facts).expect("shutdown result receiver");
        });
        ReferenceShutdownWitness {
            started: Mutex::new(Some(started_rx)),
            release_observed: Mutex::new(Some(release_rx)),
            allow_exit: Mutex::new(Some(allow_exit_tx)),
            result: Mutex::new((result_rx, None)),
            worker: Some(worker),
        }
    }
    fn shutdown(&self) -> ClosedFacts {
        OperationModelAdapter::shutdown(self)
    }
}

impl ProgressShutdownBridgeAdapter for ReferenceAdapter {
    type Implementation = ReferenceLifecycle;
    type Error = crate::model::AdapterError;
    type Guard = ReferenceTicket;
    type Lease = ReferenceLease;
    fn deterministic(progress_capacity: usize) -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig {
            next_sequence: 1,
            progress_capacity,
        })
    }
    fn start(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error> {
        start_reference(self, operation_id)
    }
    fn publish_progress(&self, lease: &Self::Lease, sequence: u64) -> Result<(), Self::Error> {
        OperationModelAdapter::publish_progress(self, lease, sequence)
    }
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        OperationModelAdapter::lease_snapshot(self, lease)
    }
    fn terminal(&self, lease: &Self::Lease, class: TerminalClass) -> Result<(), Self::Error> {
        OperationModelAdapter::terminal(self, lease, class)
    }
    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        OperationModelAdapter::release(self, lease)
    }
    fn progress_capacity(&self) -> usize {
        OperationModelAdapter::progress_capacity(self)
    }
    fn shutdown(&self) -> ClosedFacts {
        OperationModelAdapter::shutdown(self)
    }
}

impl PanicShutdownBridgeAdapter for ReferenceAdapter {
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
    fn run_executor(
        &self,
        lease: &Self::Lease,
        executor: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<(), Self::Error> {
        match thread::spawn(executor).join() {
            Ok(()) => OperationModelAdapter::terminal(self, lease, TerminalClass::Completed),
            Err(_) => OperationModelAdapter::record_executor_panic(self, lease),
        }
    }
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        OperationModelAdapter::lease_snapshot(self, lease)
    }
    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        OperationModelAdapter::release(self, lease)
    }
    fn shutdown(&self) -> ClosedFacts {
        OperationModelAdapter::shutdown(self)
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

fn assert_completed_terminal(snapshot: &OperationSnapshot) {
    assert_terminal(snapshot, TerminalClass::Completed);
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
            run_admission_quiesce_shutdown_bridge_suite::<ReferenceAdapter>("supervisor-bridge"),
            run_progress_shutdown_bridge_suite::<ReferenceAdapter>("progress-bridge"),
            run_panic_shutdown_bridge_suite::<ReferenceAdapter>("panic-bridge"),
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
            Err(AcceptanceError::MissingInvariants(_))
        ));
    }
}
