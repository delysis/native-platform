use crate::barrier::DeterministicBarrier;
use crate::model::{
    ClosedFacts, LifecyclePhase, OperationModelAdapter, OperationPhase, ReferenceAdapter,
    TerminalClass, TestConfig, WaitObservation,
};
use std::thread;

fn adapter<A: OperationModelAdapter>() -> A {
    A::deterministic(TestConfig::default())
}

fn reserve_running<A: OperationModelAdapter>(
    adapter: &A,
    operation_id: &str,
) -> (A::Ticket, A::Lease) {
    let (ticket, lease) = adapter
        .reserve(operation_id)
        .expect("operation reservation must succeed")
        .into_parts();
    adapter
        .queue(&lease)
        .expect("reserved operation must queue");
    adapter.start(&lease).expect("queued operation must start");
    (ticket, lease)
}

pub fn assert_exact_transition_chain<A: OperationModelAdapter>() {
    let adapter = adapter::<A>();
    let (ticket, lease) = adapter
        .reserve("transitions")
        .expect("reserve")
        .into_parts();
    assert_eq!(
        adapter.lease_snapshot(&lease).expect("reserved").phase,
        OperationPhase::Reserved
    );
    assert!(
        adapter.start(&lease).is_err(),
        "Reserved -> Running is invalid"
    );
    assert!(
        adapter.release(&lease).is_err(),
        "Reserved -> Released is invalid"
    );
    assert!(
        adapter.terminal(&lease, TerminalClass::Completed).is_err(),
        "Reserved -> Terminal is invalid"
    );

    adapter.queue(&lease).expect("Reserved -> Queued");
    assert_eq!(
        adapter.lease_snapshot(&lease).expect("queued").phase,
        OperationPhase::Queued
    );
    assert!(
        adapter.queue(&lease).is_err(),
        "Queued -> Queued is invalid"
    );
    assert!(
        adapter.release(&lease).is_err(),
        "Queued -> Released is invalid"
    );
    assert!(
        adapter.terminal(&lease, TerminalClass::Completed).is_err(),
        "Queued -> Terminal is invalid"
    );

    adapter.start(&lease).expect("Queued -> Running");
    assert_eq!(
        adapter.lease_snapshot(&lease).expect("running").phase,
        OperationPhase::Running
    );
    assert!(
        adapter.start(&lease).is_err(),
        "Running -> Running is invalid"
    );
    assert!(
        adapter.queue(&lease).is_err(),
        "Running -> Queued is invalid"
    );

    adapter
        .terminal(&lease, TerminalClass::Completed)
        .expect("Running -> Terminal");
    assert_eq!(
        adapter.lease_snapshot(&lease).expect("terminal").phase,
        OperationPhase::Terminal
    );
    assert!(
        adapter.queue(&lease).is_err(),
        "Terminal -> Queued is invalid"
    );
    assert!(
        adapter.start(&lease).is_err(),
        "Terminal -> Running is invalid"
    );

    adapter.release(&lease).expect("Terminal -> Released");
    assert_eq!(
        adapter
            .lease_snapshot(&lease)
            .expect("released archive")
            .phase,
        OperationPhase::Released
    );
    assert!(adapter.release(&lease).is_err(), "Released is immutable");
    drop(ticket);
}

pub fn assert_ticket_drop_cancels_while_lease_retains_identity<A: OperationModelAdapter>() {
    let adapter = adapter::<A>();
    let (ticket, lease) = reserve_running(&adapter, "consumer-drop");
    let ticket_identity = adapter.ticket_identity(&ticket);
    let lease_identity = adapter.lease_identity(&lease);
    assert_eq!(ticket_identity, lease_identity);
    assert!(!ticket_identity.attempt_id.is_empty());

    drop(ticket);
    let snapshot = adapter
        .lease_snapshot(&lease)
        .expect("executor lease must retain attempt identity");
    assert!(snapshot.cancellation_requested);
    assert_eq!(snapshot.phase, OperationPhase::Running);
    assert_eq!(snapshot.identity, lease_identity);

    adapter
        .terminal(&lease, TerminalClass::Cancelled)
        .expect("executor owns terminal transition");
    adapter.release(&lease).expect("executor owns release");
}

pub fn assert_explicit_consumer_drop_requests_cancellation<A: OperationModelAdapter>() {
    let adapter = adapter::<A>();
    let (ticket, lease) = reserve_running(&adapter, "explicit-consumer-drop");
    adapter
        .consumer_drop(ticket)
        .expect("explicit consumer drop");
    assert!(
        adapter
            .lease_snapshot(&lease)
            .expect("lease remains authoritative")
            .cancellation_requested
    );
    adapter
        .terminal(&lease, TerminalClass::Cancelled)
        .expect("cancel terminal");
    adapter.release(&lease).expect("release");
}

pub fn assert_one_authoritative_terminal_and_projections<A: OperationModelAdapter>() {
    let adapter = adapter::<A>();
    let (ticket, lease) = reserve_running(&adapter, "terminal");
    adapter.publish_progress(&lease, 1).expect("progress");
    adapter
        .terminal(&lease, TerminalClass::Completed)
        .expect("first terminal wins");
    let terminal = adapter.lease_snapshot(&lease).expect("terminal snapshot");
    assert_eq!(terminal.authoritative_terminal, terminal.final_projection);
    assert_eq!(
        terminal
            .authoritative_terminal
            .expect("authoritative terminal")
            .class,
        TerminalClass::Completed
    );
    assert_eq!(terminal.progress_projection, vec![1]);

    assert!(adapter.terminal(&lease, TerminalClass::Failed).is_err());
    assert_eq!(adapter.lease_snapshot(&lease), Some(terminal));
    adapter.release(&lease).expect("release");
    drop(ticket);
}

pub fn assert_waiter_timeout_is_observational_and_retains_control<A: OperationModelAdapter>() {
    let adapter = adapter::<A>();
    let (ticket, lease) = reserve_running(&adapter, "wait-timeout");
    let before = adapter.lease_snapshot(&lease).expect("before timeout");
    assert_eq!(
        adapter
            .waiter_timeout(&ticket)
            .expect("timeout observation"),
        WaitObservation::TimedOut
    );
    assert_eq!(adapter.lease_snapshot(&lease), Some(before));
    adapter
        .request_cancel(&ticket)
        .expect("ticket remains usable after timeout");
    assert!(
        adapter
            .lease_snapshot(&lease)
            .expect("after cancel")
            .cancellation_requested
    );
    adapter
        .terminal(&lease, TerminalClass::Cancelled)
        .expect("terminal");
    adapter.release(&lease).expect("release");
    drop(ticket);
}

pub fn assert_unread_progress_cannot_block_terminal_release_or_shutdown<
    A: OperationModelAdapter,
>() {
    let adapter = A::deterministic(TestConfig {
        next_sequence: 1,
        progress_capacity: 3,
    });
    let (ticket, lease) = reserve_running(&adapter, "saturated-progress");
    for sequence in 0..32 {
        adapter
            .publish_progress(&lease, sequence)
            .expect("bounded progress publication must not block");
    }
    let snapshot = adapter.lease_snapshot(&lease).expect("saturated snapshot");
    assert_eq!(
        snapshot.progress_projection.len(),
        adapter.progress_capacity()
    );
    assert_eq!(snapshot.progress_projection, vec![29, 30, 31]);

    adapter
        .terminal(&lease, TerminalClass::Completed)
        .expect("terminal bypasses unread progress");
    adapter
        .release(&lease)
        .expect("release bypasses unread progress");
    drop(ticket);
    assert_closed_and_empty(adapter.shutdown());
}

pub fn assert_duplicate_ids_fail_without_partial_admission<A: OperationModelAdapter>() {
    let adapter = adapter::<A>();
    let (ticket, lease) = adapter
        .reserve("duplicate")
        .expect("first reserve")
        .into_parts();
    assert!(adapter.reserve("duplicate").is_err());
    assert_eq!(adapter.active_count(), 1);
    adapter.queue(&lease).expect("queue");
    adapter.start(&lease).expect("start");
    adapter
        .terminal(&lease, TerminalClass::Completed)
        .expect("terminal");
    adapter.release(&lease).expect("release");
    drop(ticket);
}

pub fn assert_sequence_exhaustion_fails_without_partial_admission<A: OperationModelAdapter>() {
    let adapter = A::deterministic(TestConfig {
        next_sequence: u64::MAX,
        progress_capacity: 1,
    });
    assert!(adapter.reserve("exhausted").is_err());
    assert_eq!(adapter.active_count(), 0);
    assert_eq!(adapter.retained_task_count(), 0);
}

pub fn assert_stale_release_cannot_remove_reused_id<A: OperationModelAdapter>() {
    let adapter = adapter::<A>();
    let (first_ticket, stale) = reserve_running(&adapter, "reused");
    adapter
        .terminal(&stale, TerminalClass::Completed)
        .expect("first terminal");
    adapter.release(&stale).expect("first release");
    drop(first_ticket);

    let (current_ticket, current) = adapter.reserve("reused").expect("reuse ID").into_parts();
    assert_ne!(
        adapter.lease_identity(&stale),
        adapter.lease_identity(&current)
    );
    assert!(adapter.release(&stale).is_err());
    assert_eq!(
        adapter
            .current_snapshot("reused")
            .expect("current attempt")
            .identity,
        adapter.lease_identity(&current)
    );
    adapter.queue(&current).expect("queue current");
    adapter.start(&current).expect("start current");
    adapter
        .terminal(&current, TerminalClass::Completed)
        .expect("terminal current");
    adapter.release(&current).expect("release current");
    drop(current_ticket);
}

pub fn assert_quiesce_rejects_admission_and_shutdown_waits_for_release<A: OperationModelAdapter>() {
    let adapter = adapter::<A>();
    let (ticket, lease) = reserve_running(&adapter, "quiesce");
    adapter.quiesce();
    assert_eq!(adapter.lifecycle_phase(), LifecyclePhase::Quiescing);
    assert!(adapter.reserve("late").is_err());
    let pending = adapter.shutdown();
    assert_eq!(pending.lifecycle, LifecyclePhase::Quiescing);
    assert_eq!(pending.active_operations, 1);
    assert_eq!(pending.retained_tasks, 1);
    assert!(pending.expected_workers > pending.joined_workers);
    assert!(
        adapter
            .lease_snapshot(&lease)
            .expect("quiesced operation")
            .cancellation_requested
    );
    adapter
        .terminal(&lease, TerminalClass::Cancelled)
        .expect("terminal");
    adapter.release(&lease).expect("release");
    drop(ticket);
    assert_closed_and_empty(adapter.shutdown());
}

pub fn assert_executor_panic_records_terminal_and_releases<A: OperationModelAdapter>() {
    let adapter = adapter::<A>();
    let (ticket, lease) = reserve_running(&adapter, "panic-safe");
    adapter
        .record_executor_panic(&lease)
        .expect("panic boundary must record failure terminal");
    let snapshot = adapter.lease_snapshot(&lease).expect("panic terminal");
    assert_eq!(snapshot.phase, OperationPhase::Terminal);
    assert_eq!(
        snapshot
            .authoritative_terminal
            .expect("failure terminal")
            .class,
        TerminalClass::Failed
    );
    adapter.release(&lease).expect("panic path releases");
    drop(ticket);
    assert_closed_and_empty(adapter.shutdown());
}

pub fn assert_repeated_shutdown_is_stable_and_empty<A: OperationModelAdapter>() {
    let adapter = adapter::<A>();
    let first = adapter.shutdown();
    let second = adapter.shutdown();
    assert_eq!(first, second);
    assert_closed_and_empty(second);
}

pub fn assert_cancel_complete_race_has_one_terminal<A>()
where
    A: OperationModelAdapter + Send + Sync + 'static,
    A::Lease: 'static,
{
    let adapter = adapter::<A>();
    let (ticket, lease) = reserve_running(&adapter, "cancel-complete-race");
    let barrier = DeterministicBarrier::new(2);
    let cancelled_adapter = adapter.clone();
    let cancelled_lease = lease.clone();
    let cancelled_barrier = barrier.clone();
    let cancel = thread::spawn(move || {
        cancelled_barrier.arrive_and_wait();
        cancelled_adapter
            .terminal(&cancelled_lease, TerminalClass::Cancelled)
            .is_ok()
    });
    let completed_adapter = adapter.clone();
    let completed_lease = lease.clone();
    let completed_barrier = barrier.clone();
    let complete = thread::spawn(move || {
        completed_barrier.arrive_and_wait();
        completed_adapter
            .terminal(&completed_lease, TerminalClass::Completed)
            .is_ok()
    });
    barrier.wait_until_all_arrived();
    barrier.release();
    let winners = usize::from(cancel.join().expect("cancel racer"))
        + usize::from(complete.join().expect("complete racer"));
    assert_eq!(winners, 1);
    let snapshot = adapter.lease_snapshot(&lease).expect("race terminal");
    assert_eq!(snapshot.phase, OperationPhase::Terminal);
    assert_eq!(snapshot.authoritative_terminal, snapshot.final_projection);
    adapter.release(&lease).expect("release race winner");
    drop(ticket);
}

pub fn assert_admit_quiesce_race_is_linearizable<A>()
where
    A: OperationModelAdapter + Send + Sync + 'static,
{
    let adapter = adapter::<A>();
    let barrier = DeterministicBarrier::new(2);
    let admitting_adapter = adapter.clone();
    let admitting_barrier = barrier.clone();
    let admit = thread::spawn(move || {
        admitting_barrier.arrive_and_wait();
        admitting_adapter.reserve("racing-admission").is_ok()
    });
    let quiescing_adapter = adapter.clone();
    let quiescing_barrier = barrier.clone();
    let quiesce = thread::spawn(move || {
        quiescing_barrier.arrive_and_wait();
        quiescing_adapter.quiesce();
    });
    barrier.wait_until_all_arrived();
    barrier.release();
    let admitted = admit.join().expect("admission racer");
    quiesce.join().expect("quiesce racer");
    assert_eq!(adapter.lifecycle_phase(), LifecyclePhase::Quiescing);
    assert_eq!(adapter.active_count(), usize::from(admitted));
    if admitted {
        assert!(
            adapter
                .current_snapshot("racing-admission")
                .expect("admitted race operation")
                .cancellation_requested
        );
    }
    assert!(adapter.reserve("post-quiesce").is_err());
}

pub fn assert_progress_terminal_interleaving_cannot_starve_terminal<A>()
where
    A: OperationModelAdapter + Send + Sync + 'static,
    A::Lease: 'static,
    A::Error: Send + 'static,
{
    let adapter = A::deterministic(TestConfig {
        next_sequence: 1,
        progress_capacity: 2,
    });
    let (ticket, lease) = reserve_running(&adapter, "progress-terminal-race");
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
    terminal
        .join()
        .expect("terminal producer")
        .then_some(())
        .expect("terminal must not be starved by progress");
    assert_eq!(
        adapter.lease_snapshot(&lease).expect("terminal").phase,
        OperationPhase::Terminal
    );
    adapter.release(&lease).expect("release");
    drop(ticket);
    assert_closed_and_empty(adapter.shutdown());
}

fn assert_closed_and_empty(facts: ClosedFacts) {
    assert_eq!(facts.lifecycle, LifecyclePhase::Closed);
    assert_eq!(facts.active_operations, 0);
    assert_eq!(facts.retained_tasks, 0);
    assert_eq!(facts.joined_workers, facts.expected_workers);
}

pub fn run_lifecycle_suite<A>()
where
    A: OperationModelAdapter + Send + Sync + 'static,
    A::Lease: 'static,
    A::Error: Send + 'static,
{
    assert_exact_transition_chain::<A>();
    assert_ticket_drop_cancels_while_lease_retains_identity::<A>();
    assert_explicit_consumer_drop_requests_cancellation::<A>();
    assert_one_authoritative_terminal_and_projections::<A>();
    assert_waiter_timeout_is_observational_and_retains_control::<A>();
    assert_unread_progress_cannot_block_terminal_release_or_shutdown::<A>();
    assert_duplicate_ids_fail_without_partial_admission::<A>();
    assert_sequence_exhaustion_fails_without_partial_admission::<A>();
    assert_stale_release_cannot_remove_reused_id::<A>();
    assert_quiesce_rejects_admission_and_shutdown_waits_for_release::<A>();
    assert_executor_panic_records_terminal_and_releases::<A>();
    assert_repeated_shutdown_is_stable_and_empty::<A>();
    assert_cancel_complete_race_has_one_terminal::<A>();
    assert_admit_quiesce_race_is_linearizable::<A>();
    assert_progress_terminal_interleaving_cannot_starve_terminal::<A>();
}

pub fn run_reference_suite() {
    run_lifecycle_suite::<ReferenceAdapter>();
}

#[cfg(test)]
mod tests {
    #[test]
    fn deterministic_thread_safe_reference_adapter_satisfies_full_lifecycle_contract() {
        super::run_reference_suite();
    }
}
