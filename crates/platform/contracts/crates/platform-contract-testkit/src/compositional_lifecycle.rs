//! Ownership-specific lifecycle conformance suites.
//!
//! These traits exist only as product test adapters. A product may use a
//! different real component for each suite. Cross-boundary invariants retain
//! dedicated bridge traits, so composition cannot turn two unrelated local
//! facts into one lifecycle claim.

use std::thread;

use crate::barrier::DeterministicBarrier;
use crate::coverage::{CoverageBinding, CoverageEvidence, LifecycleInvariant};
use crate::model::{
    AttemptIdentity, ClosedFacts, LifecyclePhase, OperationModelAdapter, OperationPhase,
    OperationSnapshot, ReferenceAdapter, ReferenceLease, ReferenceTicket, TerminalClass,
    TestConfig, WaitObservation,
};

pub trait TransitionChainAdapter: Clone + Sized {
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

pub trait ConsumerCancellationAdapter: Clone + Sized {
    type Error: std::fmt::Debug;
    type Ticket: Send;
    type Lease;

    fn deterministic() -> Self;
    fn start(&self, operation_id: &str) -> Result<(Self::Ticket, Self::Lease), Self::Error>;
    fn ticket_identity(&self, ticket: &Self::Ticket) -> AttemptIdentity;
    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity;
    fn cancellation_requested(&self, lease: &Self::Lease) -> bool;
    fn explicit_consumer_drop(&self, ticket: Self::Ticket) -> Result<(), Self::Error>;
    fn finish_cancelled(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
}

pub trait TerminalAuthorityAdapter: Clone + Sized + Send + Sync + 'static {
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
    type Error: std::fmt::Debug + Send + 'static;
    type Operation: Send + 'static;

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
    fn shutdown(&self) -> ClosedFacts;
}

/// Integrated proof that progress backpressure cannot starve terminal release
/// or successful shutdown. Keeping this as one trait preserves the cross-layer
/// invariant when progress and shutdown have different internal owners.
pub trait ProgressShutdownBridgeAdapter: Clone + Sized + Send + Sync + 'static {
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
    type Error: std::fmt::Debug;
    type Guard;
    type Lease;

    fn deterministic() -> Self;
    fn start(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error>;
    fn record_executor_panic(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot>;
    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
    fn shutdown(&self) -> ClosedFacts;
}

pub trait StableShutdownAdapter: Clone + Sized {
    fn deterministic() -> Self;
    fn shutdown(&self) -> ClosedFacts;
}

pub trait TaskReapingAdapter: Clone + Sized {
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
    binding: &CoverageBinding,
) -> CoverageEvidence {
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
    assert!(adapter.queue(&operation).is_err());
    assert!(adapter.release(&operation).is_err());
    adapter.start(&operation).expect("Queued -> Running");
    assert_eq!(adapter.phase(&operation), Some(OperationPhase::Running));
    assert!(adapter.start(&operation).is_err());
    adapter
        .terminal(&operation, TerminalClass::Completed)
        .expect("Running -> Terminal");
    assert_eq!(adapter.phase(&operation), Some(OperationPhase::Terminal));
    assert!(adapter.terminal(&operation, TerminalClass::Failed).is_err());
    adapter.release(&operation).expect("Terminal -> Released");
    assert_eq!(adapter.phase(&operation), Some(OperationPhase::Released));
    assert!(adapter.release(&operation).is_err());
    CoverageEvidence::passed(
        binding,
        "transition-chain",
        [LifecycleInvariant::ExactTransitionChain],
    )
}

pub fn run_registry_identity_suite<A: RegistryIdentityAdapter>(
    binding: &CoverageBinding,
) -> CoverageEvidence {
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
        binding,
        "registry-identity",
        [
            LifecycleInvariant::DuplicatePublicId,
            LifecycleInvariant::AttemptIdentityUnderPublicOperation,
            LifecycleInvariant::StaleRelease,
            LifecycleInvariant::SequenceExhaustion,
        ],
    )
}

pub fn run_consumer_cancellation_suite<A: ConsumerCancellationAdapter>(
    binding: &CoverageBinding,
) -> CoverageEvidence {
    let adapter = A::deterministic();
    let (ticket, lease) = adapter.start("ticket-drop").expect("start");
    assert_eq!(
        adapter.ticket_identity(&ticket),
        adapter.lease_identity(&lease)
    );
    drop(ticket);
    assert!(adapter.cancellation_requested(&lease));
    adapter
        .finish_cancelled(&lease)
        .expect("executor retains release authority");

    let (ticket, lease) = adapter.start("explicit-drop").expect("start");
    adapter
        .explicit_consumer_drop(ticket)
        .expect("explicit consumer drop");
    assert!(adapter.cancellation_requested(&lease));
    adapter.finish_cancelled(&lease).expect("finish");
    CoverageEvidence::passed(
        binding,
        "consumer-cancellation",
        [
            LifecycleInvariant::TicketDropCancellation,
            LifecycleInvariant::ExplicitConsumerDropCancellation,
        ],
    )
}

pub fn run_terminal_authority_suite<A: TerminalAuthorityAdapter>(
    binding: &CoverageBinding,
) -> CoverageEvidence {
    let adapter = A::deterministic();
    let (_guard, lease) = adapter.start("terminal").expect("start");
    adapter
        .terminal(&lease, TerminalClass::Completed)
        .expect("terminal");
    let snapshot = adapter.snapshot(&lease).expect("terminal snapshot");
    assert_eq!(snapshot.authoritative_terminal, snapshot.final_projection);
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
    assert_eq!(
        usize::from(cancel.join().expect("cancel racer"))
            + usize::from(complete.join().expect("complete racer")),
        1
    );
    let snapshot = adapter.snapshot(&lease).expect("race terminal");
    assert_eq!(snapshot.authoritative_terminal, snapshot.final_projection);
    adapter.release(&lease).expect("release race");
    CoverageEvidence::passed(
        binding,
        "terminal-authority",
        [
            LifecycleInvariant::AuthoritativeTerminal,
            LifecycleInvariant::CancelCompleteRace,
        ],
    )
}

pub fn run_waiter_control_suite<A: WaiterControlAdapter>(
    binding: &CoverageBinding,
) -> CoverageEvidence {
    let adapter = A::deterministic();
    let (ticket, lease) = adapter.start("timeout").expect("start");
    let before = adapter.snapshot(&lease).expect("before timeout");
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
        binding,
        "waiter-control",
        [LifecycleInvariant::WaiterTimeoutIsObservational],
    )
}

pub fn run_admission_quiesce_shutdown_bridge_suite<A: AdmissionQuiesceShutdownBridgeAdapter>(
    binding: &CoverageBinding,
) -> CoverageEvidence {
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
    adapter.quiesce();
    let pending = adapter.shutdown();
    assert_eq!(pending.lifecycle, LifecyclePhase::Quiescing);
    assert_eq!(pending.active_operations, 1);
    assert_eq!(pending.retained_tasks, 1);
    assert!(pending.expected_workers > pending.joined_workers);
    assert!(adapter.cancellation_requested("active-shutdown"));
    let terminal = adapter
        .finish_cancelled(&operation)
        .expect("active executor terminal and release");
    assert_cancelled_release(&terminal);
    assert_eq!(adapter.active_count(), 0);
    assert_closed_and_empty(adapter.shutdown());
    CoverageEvidence::passed(
        binding,
        "admission-quiesce-shutdown-bridge",
        [
            LifecycleInvariant::AdmitQuiesceRace,
            LifecycleInvariant::QuiesceWaitsForReleaseAndJoin,
        ],
    )
}

pub fn run_progress_shutdown_bridge_suite<A: ProgressShutdownBridgeAdapter>(
    binding: &CoverageBinding,
) -> CoverageEvidence {
    let adapter = A::deterministic(3);
    let (guard, lease) = adapter.start("saturated-progress").expect("start");
    for sequence in 0..32 {
        adapter
            .publish_progress(&lease, sequence)
            .expect("progress");
    }
    let snapshot = adapter.snapshot(&lease).expect("progress snapshot");
    assert_eq!(
        snapshot.progress_projection.len(),
        adapter.progress_capacity()
    );
    assert_eq!(snapshot.progress_projection, vec![29, 30, 31]);
    adapter
        .terminal(&lease, TerminalClass::Completed)
        .expect("terminal bypasses progress");
    assert_completed_terminal(&adapter.snapshot(&lease).expect("terminal snapshot"));
    adapter.release(&lease).expect("release bypasses progress");
    drop(guard);
    assert_closed_and_empty(adapter.shutdown());

    let adapter = A::deterministic(2);
    let (guard, lease) = adapter.start("progress-terminal-race").expect("start");
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
    drop(guard);
    assert_closed_and_empty(adapter.shutdown());
    CoverageEvidence::passed(
        binding,
        "progress-shutdown-bridge",
        [
            LifecycleInvariant::UnreadProgressCannotBlockFinalOrShutdown,
            LifecycleInvariant::ProgressTerminalRace,
        ],
    )
}

pub fn run_panic_shutdown_bridge_suite<A: PanicShutdownBridgeAdapter>(
    binding: &CoverageBinding,
) -> CoverageEvidence {
    let adapter = A::deterministic();
    let (guard, lease) = adapter.start("panic").expect("start");
    adapter
        .record_executor_panic(&lease)
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
        binding,
        "panic-shutdown-bridge",
        [LifecycleInvariant::PanicTerminalAndShutdown],
    )
}

pub fn run_stable_shutdown_suite<A: StableShutdownAdapter>(
    binding: &CoverageBinding,
) -> CoverageEvidence {
    let adapter = A::deterministic();
    let first = adapter.shutdown();
    let second = adapter.shutdown();
    assert_eq!(first, second);
    assert_closed_and_empty(second);
    CoverageEvidence::passed(
        binding,
        "stable-shutdown",
        [
            LifecycleInvariant::RepeatedShutdown,
            LifecycleInvariant::ShutdownEmpty,
        ],
    )
}

pub fn run_task_reaping_suite<A: TaskReapingAdapter>(
    binding: &CoverageBinding,
) -> CoverageEvidence {
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
        binding,
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

fn finish_reference(
    adapter: &ReferenceAdapter,
    lease: &ReferenceLease,
    class: TerminalClass,
) -> Result<(), crate::model::AdapterError> {
    OperationModelAdapter::terminal(adapter, lease, class)?;
    OperationModelAdapter::release(adapter, lease)
}

impl TransitionChainAdapter for ReferenceAdapter {
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
    type Error = crate::model::AdapterError;
    type Operation = (ReferenceTicket, ReferenceLease);
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
    fn shutdown(&self) -> ClosedFacts {
        OperationModelAdapter::shutdown(self)
    }
}

impl ProgressShutdownBridgeAdapter for ReferenceAdapter {
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
    type Error = crate::model::AdapterError;
    type Guard = ReferenceTicket;
    type Lease = ReferenceLease;
    fn deterministic() -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig::default())
    }
    fn start(&self, operation_id: &str) -> Result<(Self::Guard, Self::Lease), Self::Error> {
        start_reference(self, operation_id)
    }
    fn record_executor_panic(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        OperationModelAdapter::record_executor_panic(self, lease)
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
    fn deterministic() -> Self {
        <Self as OperationModelAdapter>::deterministic(TestConfig::default())
    }
    fn shutdown(&self) -> ClosedFacts {
        OperationModelAdapter::shutdown(self)
    }
}

impl TaskReapingAdapter for ReferenceAdapter {
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
    assert_eq!(snapshot.phase, OperationPhase::Terminal);
    assert_eq!(snapshot.authoritative_terminal, snapshot.final_projection);
    assert_eq!(
        snapshot
            .authoritative_terminal
            .expect("authoritative terminal")
            .class,
        TerminalClass::Completed
    );
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
    assert_eq!(facts.joined_workers, facts.expected_workers);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AcceptanceError, LifecycleCoverageManifest};

    fn binding(component: &str) -> CoverageBinding {
        CoverageBinding::new("reference-product", "reference-lifecycle", component)
            .expect("coverage binding")
    }

    #[test]
    fn composed_reference_suites_cover_every_normative_invariant() {
        let evidence = vec![
            run_transition_chain_suite::<ReferenceAdapter>(&binding("state-machine")),
            run_registry_identity_suite::<ReferenceAdapter>(&binding("registry")),
            run_consumer_cancellation_suite::<ReferenceAdapter>(&binding("consumer-control")),
            run_terminal_authority_suite::<ReferenceAdapter>(&binding("terminal-owner")),
            run_waiter_control_suite::<ReferenceAdapter>(&binding("waiter")),
            run_admission_quiesce_shutdown_bridge_suite::<ReferenceAdapter>(&binding(
                "supervisor-bridge",
            )),
            run_progress_shutdown_bridge_suite::<ReferenceAdapter>(&binding("progress-bridge")),
            run_panic_shutdown_bridge_suite::<ReferenceAdapter>(&binding("panic-bridge")),
            run_stable_shutdown_suite::<ReferenceAdapter>(&binding("shutdown")),
            run_task_reaping_suite::<ReferenceAdapter>(&binding("task-supervisor")),
        ];
        let manifest =
            LifecycleCoverageManifest::accept("reference-product", "reference-lifecycle", evidence)
                .expect("complete composed coverage");
        assert_eq!(manifest.covered().count(), 18);
        assert_eq!(manifest.components().count(), 10);
        assert_eq!(manifest.product(), "reference-product");
        assert_eq!(manifest.implementation(), "reference-lifecycle");
    }

    #[test]
    fn partial_or_cross_product_evidence_cannot_pass_acceptance() {
        let partial = vec![run_registry_identity_suite::<ReferenceAdapter>(&binding(
            "registry",
        ))];
        assert!(matches!(
            LifecycleCoverageManifest::accept("reference-product", "reference-lifecycle", partial,),
            Err(AcceptanceError::MissingInvariants(_))
        ));

        let foreign = vec![run_registry_identity_suite::<ReferenceAdapter>(
            &CoverageBinding::new("another-product", "reference-lifecycle", "registry")
                .expect("binding"),
        )];
        assert!(matches!(
            LifecycleCoverageManifest::accept("reference-product", "reference-lifecycle", foreign,),
            Err(AcceptanceError::WrongProduct { .. })
        ));

        let other_implementation = vec![run_registry_identity_suite::<ReferenceAdapter>(
            &CoverageBinding::new("reference-product", "speech-lifecycle", "registry")
                .expect("binding"),
        )];
        assert!(matches!(
            LifecycleCoverageManifest::accept(
                "reference-product",
                "reference-lifecycle",
                other_implementation,
            ),
            Err(AcceptanceError::WrongImplementation { .. })
        ));
    }
}
