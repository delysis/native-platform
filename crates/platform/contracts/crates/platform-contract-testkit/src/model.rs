//! Deterministic, test-only operation-supervisor model.
//!
//! `OperationModelAdapter` is a conformance surface for product test adapters.
//! It is intentionally not a production runtime abstraction.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecyclePhase {
    Running,
    Quiescing,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationPhase {
    Reserved,
    Queued,
    Running,
    Terminal,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalClass {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AttemptIdentity {
    pub operation_id: String,
    pub attempt_id: String,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalRecord {
    pub class: TerminalClass,
    pub sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationSnapshot {
    pub identity: AttemptIdentity,
    pub phase: OperationPhase,
    pub cancellation_requested: bool,
    pub authoritative_terminal: Option<TerminalRecord>,
    pub final_projection: Option<TerminalRecord>,
    pub progress_projection: Vec<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TestConfig {
    pub next_sequence: u64,
    pub progress_capacity: usize,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            next_sequence: 1,
            progress_capacity: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClosedFacts {
    pub lifecycle: LifecyclePhase,
    pub active_operations: usize,
    pub retained_tasks: usize,
    pub expected_workers: usize,
    pub joined_workers: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownOutcome {
    pub facts: ClosedFacts,
    pub expected_worker_ids: Vec<String>,
    pub joined_worker_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ShutdownOutcomeError {
    #[error("shutdown expected-worker IDs are not unique")]
    DuplicateExpectedWorkerId,
    #[error("shutdown joined-worker IDs are not unique")]
    DuplicateJoinedWorkerId,
    #[error("shutdown worker-ID counts do not match scalar facts")]
    CountMismatch,
    #[error("shutdown joined-worker IDs differ from expected-worker IDs")]
    WorkerSetMismatch,
}

impl ShutdownOutcome {
    pub fn validate(&self) -> Result<(), ShutdownOutcomeError> {
        use std::collections::BTreeSet;

        let expected = self.expected_worker_ids.iter().collect::<BTreeSet<_>>();
        if expected.len() != self.expected_worker_ids.len() {
            return Err(ShutdownOutcomeError::DuplicateExpectedWorkerId);
        }
        let joined = self.joined_worker_ids.iter().collect::<BTreeSet<_>>();
        if joined.len() != self.joined_worker_ids.len() {
            return Err(ShutdownOutcomeError::DuplicateJoinedWorkerId);
        }
        if expected.len() != self.facts.expected_workers
            || joined.len() != self.facts.joined_workers
        {
            return Err(ShutdownOutcomeError::CountMismatch);
        }
        if expected != joined {
            return Err(ShutdownOutcomeError::WorkerSetMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitObservation {
    TimedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdapterError {
    AdmissionClosed,
    DuplicateOperation,
    UnknownOperation,
    StaleLease,
    InvalidTransition,
    SequenceExhausted,
}

#[derive(Debug)]
pub struct Reservation<Ticket, Lease> {
    ticket: Ticket,
    lease: Lease,
}

impl<Ticket, Lease> Reservation<Ticket, Lease> {
    #[must_use]
    pub fn into_parts(self) -> (Ticket, Lease) {
        (self.ticket, self.lease)
    }
}

/// Test-only conformance surface implemented by product-specific adapters.
///
/// The associated ticket and lease are deliberately opaque. A product need
/// not share ownership primitives with another product to run this suite.
pub trait OperationModelAdapter: Clone + Sized {
    type Error: std::fmt::Debug;
    type Ticket: Send;
    type Lease: Clone + Send + Sync;

    fn deterministic(config: TestConfig) -> Self;
    fn reserve(
        &self,
        operation_id: &str,
    ) -> Result<Reservation<Self::Ticket, Self::Lease>, Self::Error>;
    fn ticket_identity(&self, ticket: &Self::Ticket) -> AttemptIdentity;
    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity;
    fn queue(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
    fn start(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
    fn publish_progress(&self, lease: &Self::Lease, sequence: u64) -> Result<(), Self::Error>;
    fn request_cancel(&self, ticket: &Self::Ticket) -> Result<(), Self::Error>;
    fn consumer_drop(&self, ticket: Self::Ticket) -> Result<(), Self::Error>;
    fn waiter_timeout(&self, ticket: &Self::Ticket) -> Result<WaitObservation, Self::Error>;
    fn terminal(&self, lease: &Self::Lease, terminal: TerminalClass) -> Result<(), Self::Error>;
    fn record_executor_panic(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error>;
    fn quiesce(&self);
    fn shutdown(&self) -> ClosedFacts;
    fn lifecycle_phase(&self) -> LifecyclePhase;
    fn active_count(&self) -> usize;
    fn retained_task_count(&self) -> usize;
    fn progress_capacity(&self) -> usize;
    fn current_snapshot(&self, operation_id: &str) -> Option<OperationSnapshot>;
    fn lease_snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot>;
}

#[derive(Clone, Debug)]
struct ReferenceOperation {
    identity: AttemptIdentity,
    phase: OperationPhase,
    cancellation_requested: bool,
    terminal: Option<TerminalRecord>,
    progress: VecDeque<u64>,
}

#[derive(Debug)]
struct ReferenceState {
    lifecycle: LifecyclePhase,
    next_sequence: u64,
    progress_capacity: usize,
    active: BTreeMap<String, u64>,
    attempts: BTreeMap<u64, ReferenceOperation>,
    retained_tasks: usize,
    joined_workers: usize,
}

#[derive(Debug)]
struct ReferenceInner {
    state: Mutex<ReferenceState>,
}

#[derive(Clone, Debug)]
pub struct ReferenceAdapter {
    inner: Arc<ReferenceInner>,
}

#[derive(Debug)]
pub struct ReferenceTicket {
    inner: Arc<ReferenceInner>,
    identity: AttemptIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceLease {
    identity: AttemptIdentity,
}

impl Drop for ReferenceTicket {
    fn drop(&mut self) {
        request_reference_cancellation(&self.inner, &self.identity);
    }
}

impl Default for ReferenceAdapter {
    fn default() -> Self {
        Self::deterministic(TestConfig::default())
    }
}

impl ReferenceAdapter {
    fn state(&self) -> MutexGuard<'_, ReferenceState> {
        recover_lock(&self.inner.state)
    }

    fn operation_mut<'a>(
        state: &'a mut ReferenceState,
        identity: &AttemptIdentity,
    ) -> Result<&'a mut ReferenceOperation, AdapterError> {
        let operation = state
            .attempts
            .get_mut(&identity.sequence)
            .ok_or(AdapterError::UnknownOperation)?;
        if operation.identity != *identity || operation.phase == OperationPhase::Released {
            return Err(AdapterError::StaleLease);
        }
        Ok(operation)
    }

    fn snapshot(operation: &ReferenceOperation) -> OperationSnapshot {
        OperationSnapshot {
            identity: operation.identity.clone(),
            phase: operation.phase,
            cancellation_requested: operation.cancellation_requested,
            authoritative_terminal: operation.terminal,
            final_projection: operation.terminal,
            progress_projection: operation.progress.iter().copied().collect(),
        }
    }

    fn terminal_inner(
        &self,
        lease: &ReferenceLease,
        class: TerminalClass,
    ) -> Result<(), AdapterError> {
        let mut state = self.state();
        let operation = Self::operation_mut(&mut state, &lease.identity)?;
        if operation.phase != OperationPhase::Running {
            return Err(AdapterError::InvalidTransition);
        }
        operation.terminal = Some(TerminalRecord {
            class,
            sequence: lease.identity.sequence,
        });
        operation.phase = OperationPhase::Terminal;
        Ok(())
    }
}

impl OperationModelAdapter for ReferenceAdapter {
    type Error = AdapterError;
    type Ticket = ReferenceTicket;
    type Lease = ReferenceLease;

    fn deterministic(config: TestConfig) -> Self {
        assert!(
            config.progress_capacity > 0,
            "progress must be bounded above zero"
        );
        Self {
            inner: Arc::new(ReferenceInner {
                state: Mutex::new(ReferenceState {
                    lifecycle: LifecyclePhase::Running,
                    next_sequence: config.next_sequence,
                    progress_capacity: config.progress_capacity,
                    active: BTreeMap::new(),
                    attempts: BTreeMap::new(),
                    retained_tasks: 0,
                    joined_workers: 0,
                }),
            }),
        }
    }

    fn reserve(
        &self,
        operation_id: &str,
    ) -> Result<Reservation<Self::Ticket, Self::Lease>, Self::Error> {
        let mut state = self.state();
        if state.lifecycle != LifecyclePhase::Running {
            return Err(AdapterError::AdmissionClosed);
        }
        if state.active.contains_key(operation_id) {
            return Err(AdapterError::DuplicateOperation);
        }
        let next = state
            .next_sequence
            .checked_add(1)
            .ok_or(AdapterError::SequenceExhausted)?;
        let sequence = state.next_sequence;
        let identity = AttemptIdentity {
            operation_id: operation_id.to_owned(),
            attempt_id: format!("{operation_id}#{sequence}"),
            sequence,
        };
        state.next_sequence = next;
        state.active.insert(operation_id.to_owned(), sequence);
        state.attempts.insert(
            sequence,
            ReferenceOperation {
                identity: identity.clone(),
                phase: OperationPhase::Reserved,
                cancellation_requested: false,
                terminal: None,
                progress: VecDeque::new(),
            },
        );
        state.retained_tasks = state
            .retained_tasks
            .checked_add(1)
            .expect("test retained-task count exhausted");
        drop(state);
        Ok(Reservation {
            ticket: ReferenceTicket {
                inner: Arc::clone(&self.inner),
                identity: identity.clone(),
            },
            lease: ReferenceLease { identity },
        })
    }

    fn ticket_identity(&self, ticket: &Self::Ticket) -> AttemptIdentity {
        ticket.identity.clone()
    }

    fn lease_identity(&self, lease: &Self::Lease) -> AttemptIdentity {
        lease.identity.clone()
    }

    fn queue(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        let mut state = self.state();
        let operation = Self::operation_mut(&mut state, &lease.identity)?;
        if operation.phase != OperationPhase::Reserved {
            return Err(AdapterError::InvalidTransition);
        }
        operation.phase = OperationPhase::Queued;
        Ok(())
    }

    fn start(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        let mut state = self.state();
        let operation = Self::operation_mut(&mut state, &lease.identity)?;
        if operation.phase != OperationPhase::Queued {
            return Err(AdapterError::InvalidTransition);
        }
        operation.phase = OperationPhase::Running;
        Ok(())
    }

    fn publish_progress(&self, lease: &Self::Lease, sequence: u64) -> Result<(), Self::Error> {
        let mut state = self.state();
        let capacity = state.progress_capacity;
        let operation = Self::operation_mut(&mut state, &lease.identity)?;
        if operation.phase != OperationPhase::Running {
            return Err(AdapterError::InvalidTransition);
        }
        if operation.progress.len() == capacity {
            operation.progress.pop_front();
        }
        operation.progress.push_back(sequence);
        Ok(())
    }

    fn request_cancel(&self, ticket: &Self::Ticket) -> Result<(), Self::Error> {
        request_reference_cancellation(&self.inner, &ticket.identity);
        Ok(())
    }

    fn consumer_drop(&self, ticket: Self::Ticket) -> Result<(), Self::Error> {
        request_reference_cancellation(&self.inner, &ticket.identity);
        drop(ticket);
        Ok(())
    }

    fn waiter_timeout(&self, ticket: &Self::Ticket) -> Result<WaitObservation, Self::Error> {
        let state = self.state();
        state
            .attempts
            .get(&ticket.identity.sequence)
            .filter(|operation| operation.identity == ticket.identity)
            .ok_or(AdapterError::UnknownOperation)?;
        Ok(WaitObservation::TimedOut)
    }

    fn terminal(&self, lease: &Self::Lease, terminal: TerminalClass) -> Result<(), Self::Error> {
        self.terminal_inner(lease, terminal)
    }

    fn record_executor_panic(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        self.terminal_inner(lease, TerminalClass::Failed)
    }

    fn release(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        let mut state = self.state();
        let operation = Self::operation_mut(&mut state, &lease.identity)?;
        if operation.phase != OperationPhase::Terminal {
            return Err(AdapterError::InvalidTransition);
        }
        operation.phase = OperationPhase::Released;
        let removed = state.active.remove(&lease.identity.operation_id);
        if removed != Some(lease.identity.sequence) {
            return Err(AdapterError::StaleLease);
        }
        state.retained_tasks -= 1;
        state.joined_workers = state
            .joined_workers
            .checked_add(1)
            .expect("test joined-worker count exhausted");
        Ok(())
    }

    fn quiesce(&self) {
        let mut state = self.state();
        if state.lifecycle == LifecyclePhase::Running {
            state.lifecycle = LifecyclePhase::Quiescing;
            for sequence in state.active.values().copied().collect::<Vec<_>>() {
                if let Some(operation) = state.attempts.get_mut(&sequence)
                    && operation.phase != OperationPhase::Terminal
                {
                    operation.cancellation_requested = true;
                }
            }
        }
    }

    fn shutdown(&self) -> ClosedFacts {
        self.quiesce();
        let mut state = self.state();
        if state.active.is_empty() && state.retained_tasks == 0 {
            state.lifecycle = LifecyclePhase::Closed;
        }
        closed_facts(&state)
    }

    fn lifecycle_phase(&self) -> LifecyclePhase {
        self.state().lifecycle
    }

    fn active_count(&self) -> usize {
        self.state().active.len()
    }

    fn retained_task_count(&self) -> usize {
        self.state().retained_tasks
    }

    fn progress_capacity(&self) -> usize {
        self.state().progress_capacity
    }

    fn current_snapshot(&self, operation_id: &str) -> Option<OperationSnapshot> {
        let state = self.state();
        let sequence = state.active.get(operation_id)?;
        state.attempts.get(sequence).map(Self::snapshot)
    }

    fn lease_snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        self.state()
            .attempts
            .get(&lease.identity.sequence)
            .filter(|operation| operation.identity == lease.identity)
            .map(Self::snapshot)
    }
}

fn closed_facts(state: &ReferenceState) -> ClosedFacts {
    ClosedFacts {
        lifecycle: state.lifecycle,
        active_operations: state.active.len(),
        retained_tasks: state.retained_tasks,
        expected_workers: state
            .joined_workers
            .checked_add(state.retained_tasks)
            .expect("test expected-worker count exhausted"),
        joined_workers: state.joined_workers,
    }
}

fn request_reference_cancellation(inner: &ReferenceInner, identity: &AttemptIdentity) {
    let mut state = recover_lock(&inner.state);
    if let Some(operation) = state.attempts.get_mut(&identity.sequence)
        && operation.identity == *identity
        && !matches!(
            operation.phase,
            OperationPhase::Terminal | OperationPhase::Released
        )
    {
        operation.cancellation_requested = true;
    }
}

fn recover_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod shutdown_outcome_tests {
    use super::*;

    fn facts(expected_workers: usize, joined_workers: usize) -> ClosedFacts {
        ClosedFacts {
            lifecycle: LifecyclePhase::Closed,
            active_operations: 0,
            retained_tasks: 0,
            expected_workers,
            joined_workers,
        }
    }

    #[test]
    fn shutdown_outcome_rejects_duplicate_worker_ids() {
        let duplicate_expected = ShutdownOutcome {
            facts: facts(2, 1),
            expected_worker_ids: vec!["worker-1".into(), "worker-1".into()],
            joined_worker_ids: vec!["worker-1".into()],
        };
        assert_eq!(
            duplicate_expected.validate(),
            Err(ShutdownOutcomeError::DuplicateExpectedWorkerId)
        );

        let duplicate_joined = ShutdownOutcome {
            facts: facts(1, 2),
            expected_worker_ids: vec!["worker-1".into()],
            joined_worker_ids: vec!["worker-1".into(), "worker-1".into()],
        };
        assert_eq!(
            duplicate_joined.validate(),
            Err(ShutdownOutcomeError::DuplicateJoinedWorkerId)
        );
    }

    #[test]
    fn shutdown_outcome_compares_worker_ids_as_unordered_sets() {
        let reordered = ShutdownOutcome {
            facts: facts(2, 2),
            expected_worker_ids: vec!["worker-2".into(), "worker-10".into()],
            joined_worker_ids: vec!["worker-10".into(), "worker-2".into()],
        };
        assert_eq!(reordered.validate(), Ok(()));
    }
}
