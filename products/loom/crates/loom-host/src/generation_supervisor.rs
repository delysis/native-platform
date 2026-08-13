use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerationOperationPhase {
    Reserved,
    Queued,
    Running,
    Terminal,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerationTerminalClass {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerationSupervisorPhase {
    Running,
    Quiescing,
    Closed,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GenerationAttemptIdentity {
    pub operation_id: String,
    pub attempt_id: String,
    pub sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationTerminalRecord {
    pub identity: GenerationAttemptIdentity,
    pub class: GenerationTerminalClass,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationOperationSnapshot {
    pub identity: GenerationAttemptIdentity,
    pub phase: GenerationOperationPhase,
    pub cancellation_requested: bool,
    pub authoritative_terminal: Option<GenerationTerminalRecord>,
    pub final_projection: Option<GenerationTerminalRecord>,
    pub progress_projection: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationSupervisorClosedFacts {
    pub lifecycle: GenerationSupervisorPhase,
    pub active_operations: usize,
    pub retained_tasks: usize,
    pub expected_workers: BTreeSet<u64>,
    pub joined_workers: BTreeSet<u64>,
}

#[derive(Clone, Debug)]
pub struct GenerationOperationLease {
    identity: GenerationAttemptIdentity,
}

impl GenerationOperationLease {
    #[must_use]
    pub fn identity(&self) -> &GenerationAttemptIdentity {
        &self.identity
    }
}

#[derive(Debug)]
pub struct GenerationConsumerTicket {
    supervisor: Weak<GenerationSupervisorInner>,
    identity: GenerationAttemptIdentity,
    armed: bool,
}

impl GenerationConsumerTicket {
    #[must_use]
    pub fn identity(&self) -> &GenerationAttemptIdentity {
        &self.identity
    }

    pub fn cancel(mut self) -> Result<(), GenerationSupervisorError> {
        self.request_cancel()?;
        self.armed = false;
        Ok(())
    }

    pub fn detach(mut self) {
        self.armed = false;
    }

    fn request_cancel(&self) -> Result<(), GenerationSupervisorError> {
        let supervisor = self
            .supervisor
            .upgrade()
            .ok_or(GenerationSupervisorError::Closed)?;
        GenerationSupervisor { inner: supervisor }.request_cancel(&self.identity)
    }
}

impl Drop for GenerationConsumerTicket {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.request_cancel();
        }
    }
}

#[derive(Clone, Debug)]
pub struct GenerationSupervisor {
    inner: Arc<GenerationSupervisorInner>,
}

#[derive(Debug)]
struct GenerationSupervisorInner {
    state: Mutex<GenerationSupervisorState>,
    changed: Condvar,
    progress_capacity: usize,
}

#[derive(Debug)]
struct GenerationSupervisorState {
    phase: GenerationSupervisorPhase,
    next_sequence: u64,
    operations: BTreeMap<String, SupervisedOperation>,
    released: BTreeMap<GenerationAttemptIdentity, GenerationOperationSnapshot>,
    expected_workers: BTreeSet<u64>,
    joined_workers: BTreeSet<u64>,
    canonical_close: Option<GenerationSupervisorClosedFacts>,
}

#[derive(Debug)]
struct SupervisedOperation {
    current: GenerationAttemptIdentity,
    phase: GenerationOperationPhase,
    cancellation_requested: bool,
    terminal: Option<GenerationTerminalRecord>,
    progress: VecDeque<u64>,
    attempts: BTreeMap<u64, SupervisedAttempt>,
}

#[derive(Debug)]
struct SupervisedAttempt {
    identity: GenerationAttemptIdentity,
    cancellation_requested: bool,
}

impl GenerationSupervisor {
    pub fn new(progress_capacity: usize) -> Result<Self, GenerationSupervisorError> {
        Self::with_next_sequence(progress_capacity, 1)
    }

    pub fn with_next_sequence(
        progress_capacity: usize,
        next_sequence: u64,
    ) -> Result<Self, GenerationSupervisorError> {
        if progress_capacity == 0 {
            return Err(GenerationSupervisorError::InvalidProgressCapacity);
        }
        Ok(Self {
            inner: Arc::new(GenerationSupervisorInner {
                state: Mutex::new(GenerationSupervisorState {
                    phase: GenerationSupervisorPhase::Running,
                    next_sequence,
                    operations: BTreeMap::new(),
                    released: BTreeMap::new(),
                    expected_workers: BTreeSet::new(),
                    joined_workers: BTreeSet::new(),
                    canonical_close: None,
                }),
                changed: Condvar::new(),
                progress_capacity,
            }),
        })
    }

    pub fn reserve(
        &self,
        operation_id: impl Into<String>,
    ) -> Result<(GenerationConsumerTicket, GenerationOperationLease), GenerationSupervisorError>
    {
        let operation_id = operation_id.into();
        if operation_id.trim().is_empty() {
            return Err(GenerationSupervisorError::EmptyOperationId);
        }
        let mut state = self.lock()?;
        if state.phase != GenerationSupervisorPhase::Running {
            return Err(GenerationSupervisorError::NotAccepting);
        }
        if state.operations.contains_key(&operation_id) {
            return Err(GenerationSupervisorError::DuplicateOperation(operation_id));
        }
        let identity = allocate_identity(&mut state, operation_id.clone())?;
        state.operations.insert(
            operation_id,
            SupervisedOperation {
                current: identity.clone(),
                phase: GenerationOperationPhase::Reserved,
                cancellation_requested: false,
                terminal: None,
                progress: VecDeque::with_capacity(self.inner.progress_capacity),
                attempts: BTreeMap::new(),
            },
        );
        Ok((
            GenerationConsumerTicket {
                supervisor: Arc::downgrade(&self.inner),
                identity: identity.clone(),
                armed: true,
            },
            GenerationOperationLease { identity },
        ))
    }

    pub fn queue(&self, lease: &GenerationOperationLease) -> Result<(), GenerationSupervisorError> {
        self.transition(
            lease,
            GenerationOperationPhase::Reserved,
            GenerationOperationPhase::Queued,
        )
    }

    pub fn start(&self, lease: &GenerationOperationLease) -> Result<(), GenerationSupervisorError> {
        self.transition(
            lease,
            GenerationOperationPhase::Queued,
            GenerationOperationPhase::Running,
        )
    }

    pub fn terminal(
        &self,
        lease: &GenerationOperationLease,
        class: GenerationTerminalClass,
    ) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        let operation = current_operation_mut(&mut state, &lease.identity)?;
        if operation.phase != GenerationOperationPhase::Running || operation.terminal.is_some() {
            return Err(GenerationSupervisorError::InvalidTransition);
        }
        if !operation.attempts.is_empty() {
            return Err(GenerationSupervisorError::AttemptsActive);
        }
        let terminal = GenerationTerminalRecord {
            identity: lease.identity.clone(),
            class,
        };
        operation.terminal = Some(terminal);
        operation.phase = GenerationOperationPhase::Terminal;
        drop(state);
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn release(
        &self,
        lease: &GenerationOperationLease,
    ) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        let operation = current_operation(&state, &lease.identity)?;
        if operation.phase != GenerationOperationPhase::Terminal {
            return Err(GenerationSupervisorError::InvalidTransition);
        }
        if !operation.attempts.is_empty() {
            return Err(GenerationSupervisorError::AttemptsActive);
        }
        let operation = state
            .operations
            .remove(&lease.identity.operation_id)
            .ok_or(GenerationSupervisorError::StaleLease)?;
        let snapshot = snapshot_for(&operation, GenerationOperationPhase::Released);
        state.released.insert(lease.identity.clone(), snapshot);
        drop(state);
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn terminal_and_release(
        &self,
        lease: &GenerationOperationLease,
        class: GenerationTerminalClass,
    ) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        let operation = current_operation(&state, &lease.identity)?;
        if operation.phase != GenerationOperationPhase::Running || operation.terminal.is_some() {
            return Err(GenerationSupervisorError::InvalidTransition);
        }
        if !operation.attempts.is_empty() {
            return Err(GenerationSupervisorError::AttemptsActive);
        }
        terminal_and_release_locked(&mut state, lease, class)?;
        drop(state);
        self.inner.changed.notify_all();
        Ok(())
    }

    /// Records an authoritative failure and releases an operation that could
    /// not reach its executor. This is deliberately distinct from
    /// `finish_operation`, which exists for attempt-hierarchy owners without a
    /// terminal lifecycle.
    pub fn fail_and_release(
        &self,
        lease: &GenerationOperationLease,
    ) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        let operation = current_operation(&state, &lease.identity)?;
        if !matches!(
            operation.phase,
            GenerationOperationPhase::Reserved
                | GenerationOperationPhase::Queued
                | GenerationOperationPhase::Running
        ) || operation.terminal.is_some()
        {
            return Err(GenerationSupervisorError::InvalidTransition);
        }
        if !operation.attempts.is_empty() {
            return Err(GenerationSupervisorError::AttemptsActive);
        }
        terminal_and_release_locked(&mut state, lease, GenerationTerminalClass::Failed)?;
        drop(state);
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn request_cancel(
        &self,
        identity: &GenerationAttemptIdentity,
    ) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        let operation = current_operation_mut(&mut state, identity)?;
        operation.cancellation_requested = true;
        for attempt in operation.attempts.values_mut() {
            attempt.cancellation_requested = true;
        }
        drop(state);
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn publish_progress(
        &self,
        lease: &GenerationOperationLease,
        sequence: u64,
    ) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        let operation = current_operation_mut(&mut state, &lease.identity)?;
        if operation.phase != GenerationOperationPhase::Running {
            return Err(GenerationSupervisorError::InvalidTransition);
        }
        if operation.progress.len() == self.inner.progress_capacity {
            operation.progress.pop_front();
        }
        operation.progress.push_back(sequence);
        Ok(())
    }

    pub fn snapshot(
        &self,
        lease: &GenerationOperationLease,
    ) -> Result<Option<GenerationOperationSnapshot>, GenerationSupervisorError> {
        let state = self.lock()?;
        if let Ok(operation) = current_operation(&state, &lease.identity) {
            return Ok(Some(snapshot_for(operation, operation.phase)));
        }
        Ok(state.released.get(&lease.identity).cloned())
    }

    pub fn current_snapshot(
        &self,
        operation_id: &str,
    ) -> Result<Option<GenerationOperationSnapshot>, GenerationSupervisorError> {
        let state = self.lock()?;
        Ok(state
            .operations
            .get(operation_id)
            .map(|operation| snapshot_for(operation, operation.phase)))
    }

    pub fn current_lease(
        &self,
        operation_id: &str,
    ) -> Result<Option<GenerationOperationLease>, GenerationSupervisorError> {
        let state = self.lock()?;
        Ok(state
            .operations
            .get(operation_id)
            .map(|operation| GenerationOperationLease {
                identity: operation.current.clone(),
            }))
    }

    pub fn cancellation_requested(
        &self,
        lease: &GenerationOperationLease,
    ) -> Result<bool, GenerationSupervisorError> {
        Ok(self
            .snapshot(lease)?
            .is_some_and(|snapshot| snapshot.cancellation_requested))
    }

    pub fn wait_released(
        &self,
        lease: &GenerationOperationLease,
        timeout: Duration,
    ) -> Result<Option<GenerationOperationSnapshot>, GenerationSupervisorError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let mut state = self.lock()?;
        loop {
            if let Some(snapshot) = state.released.get(&lease.identity) {
                return Ok(Some(snapshot.clone()));
            }
            current_operation(&state, &lease.identity)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let (next, timed) = self
                .inner
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| GenerationSupervisorError::Poisoned)?;
            state = next;
            if timed.timed_out() && !state.released.contains_key(&lease.identity) {
                return Ok(None);
            }
        }
    }

    pub fn active_count(&self) -> Result<usize, GenerationSupervisorError> {
        Ok(self.lock()?.operations.len())
    }

    pub fn phase(&self) -> Result<GenerationSupervisorPhase, GenerationSupervisorError> {
        Ok(self.lock()?.phase)
    }

    pub fn quiesce(&self) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        if state.phase == GenerationSupervisorPhase::Running {
            state.phase = GenerationSupervisorPhase::Quiescing;
        }
        for operation in state.operations.values_mut() {
            operation.cancellation_requested = true;
            for attempt in operation.attempts.values_mut() {
                attempt.cancellation_requested = true;
            }
        }
        drop(state);
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn resume(&self) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        if state.phase == GenerationSupervisorPhase::Closed {
            return Err(GenerationSupervisorError::Closed);
        }
        state.phase = GenerationSupervisorPhase::Running;
        state.canonical_close = None;
        Ok(())
    }

    pub fn close(&self) -> Result<GenerationSupervisorClosedFacts, GenerationSupervisorError> {
        let mut state = self.lock()?;
        if let Some(closed) = &state.canonical_close {
            return Ok(closed.clone());
        }
        if !state.operations.is_empty() || state.expected_workers != state.joined_workers {
            return Err(GenerationSupervisorError::NotDrained);
        }
        state.phase = GenerationSupervisorPhase::Closed;
        let closed = closed_facts(&state);
        state.canonical_close = Some(closed.clone());
        Ok(closed)
    }

    pub fn note_worker_started(&self, worker_id: u64) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        if state.canonical_close.is_some() || !state.expected_workers.insert(worker_id) {
            return Err(GenerationSupervisorError::DuplicateWorker(worker_id));
        }
        Ok(())
    }

    pub fn note_worker_joined(&self, worker_id: u64) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        if !state.expected_workers.contains(&worker_id) || !state.joined_workers.insert(worker_id) {
            return Err(GenerationSupervisorError::UnknownWorker(worker_id));
        }
        drop(state);
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn retained_task_count(&self) -> Result<usize, GenerationSupervisorError> {
        let state = self.lock()?;
        Ok(state
            .expected_workers
            .len()
            .saturating_sub(state.joined_workers.len()))
    }

    pub fn closed_facts(
        &self,
    ) -> Result<GenerationSupervisorClosedFacts, GenerationSupervisorError> {
        let state = self.lock()?;
        Ok(state
            .canonical_close
            .clone()
            .unwrap_or_else(|| closed_facts(&state)))
    }

    pub fn create_operation(
        &self,
        operation_id: impl Into<String>,
    ) -> Result<GenerationOperationLease, GenerationSupervisorError> {
        let (ticket, lease) = self.reserve(operation_id)?;
        ticket.detach();
        Ok(lease)
    }

    pub fn start_attempt(
        &self,
        operation: &GenerationOperationLease,
    ) -> Result<GenerationOperationLease, GenerationSupervisorError> {
        let mut state = self.lock()?;
        current_operation(&state, &operation.identity)?;
        let identity = allocate_identity(&mut state, operation.identity.operation_id.clone())?;
        let record = state
            .operations
            .get_mut(&operation.identity.operation_id)
            .ok_or(GenerationSupervisorError::StaleLease)?;
        record.attempts.insert(
            identity.sequence,
            SupervisedAttempt {
                identity: identity.clone(),
                cancellation_requested: record.cancellation_requested,
            },
        );
        Ok(GenerationOperationLease { identity })
    }

    pub fn active_attempts(
        &self,
        operation: &GenerationOperationLease,
    ) -> Result<Vec<GenerationAttemptIdentity>, GenerationSupervisorError> {
        let state = self.lock()?;
        Ok(state
            .operations
            .get(&operation.identity.operation_id)
            .filter(|candidate| candidate.current == operation.identity)
            .map(|candidate| {
                candidate
                    .attempts
                    .values()
                    .map(|attempt| attempt.identity.clone())
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn attempt_cancelled(
        &self,
        attempt: &GenerationOperationLease,
    ) -> Result<bool, GenerationSupervisorError> {
        let state = self.lock()?;
        Ok(state
            .operations
            .get(&attempt.identity.operation_id)
            .and_then(|operation| operation.attempts.get(&attempt.identity.sequence))
            .is_some_and(|candidate| {
                candidate.identity == attempt.identity && candidate.cancellation_requested
            }))
    }

    pub fn finish_attempt(
        &self,
        attempt: &GenerationOperationLease,
    ) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        let operation = state
            .operations
            .get_mut(&attempt.identity.operation_id)
            .ok_or(GenerationSupervisorError::StaleLease)?;
        let removed = operation
            .attempts
            .remove(&attempt.identity.sequence)
            .ok_or(GenerationSupervisorError::StaleLease)?;
        if removed.identity != attempt.identity {
            return Err(GenerationSupervisorError::StaleLease);
        }
        Ok(())
    }

    pub fn finish_operation(
        &self,
        operation: &GenerationOperationLease,
    ) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        let candidate = current_operation(&state, &operation.identity)?;
        if !candidate.attempts.is_empty() {
            return Err(GenerationSupervisorError::AttemptsActive);
        }
        if candidate.terminal.is_some() {
            return Err(GenerationSupervisorError::InvalidTransition);
        }
        terminal_and_release_locked(&mut state, operation, GenerationTerminalClass::Cancelled)?;
        drop(state);
        self.inner.changed.notify_all();
        Ok(())
    }

    fn transition(
        &self,
        lease: &GenerationOperationLease,
        from: GenerationOperationPhase,
        to: GenerationOperationPhase,
    ) -> Result<(), GenerationSupervisorError> {
        let mut state = self.lock()?;
        let operation = current_operation_mut(&mut state, &lease.identity)?;
        if operation.phase != from {
            return Err(GenerationSupervisorError::InvalidTransition);
        }
        operation.phase = to;
        Ok(())
    }

    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, GenerationSupervisorState>, GenerationSupervisorError>
    {
        self.inner
            .state
            .lock()
            .map_err(|_| GenerationSupervisorError::Poisoned)
    }
}

fn allocate_identity(
    state: &mut GenerationSupervisorState,
    operation_id: String,
) -> Result<GenerationAttemptIdentity, GenerationSupervisorError> {
    let sequence = state.next_sequence;
    state.next_sequence = sequence
        .checked_add(1)
        .ok_or(GenerationSupervisorError::SequenceExhausted)?;
    Ok(GenerationAttemptIdentity {
        attempt_id: format!("{operation_id}:{sequence}"),
        operation_id,
        sequence,
    })
}

fn current_operation<'a>(
    state: &'a GenerationSupervisorState,
    identity: &GenerationAttemptIdentity,
) -> Result<&'a SupervisedOperation, GenerationSupervisorError> {
    let operation = state
        .operations
        .get(&identity.operation_id)
        .ok_or(GenerationSupervisorError::StaleLease)?;
    if operation.current != *identity {
        return Err(GenerationSupervisorError::StaleLease);
    }
    Ok(operation)
}

fn current_operation_mut<'a>(
    state: &'a mut GenerationSupervisorState,
    identity: &GenerationAttemptIdentity,
) -> Result<&'a mut SupervisedOperation, GenerationSupervisorError> {
    let operation = state
        .operations
        .get_mut(&identity.operation_id)
        .ok_or(GenerationSupervisorError::StaleLease)?;
    if operation.current != *identity {
        return Err(GenerationSupervisorError::StaleLease);
    }
    Ok(operation)
}

fn snapshot_for(
    operation: &SupervisedOperation,
    phase: GenerationOperationPhase,
) -> GenerationOperationSnapshot {
    GenerationOperationSnapshot {
        identity: operation.current.clone(),
        phase,
        cancellation_requested: operation.cancellation_requested,
        authoritative_terminal: operation.terminal.clone(),
        final_projection: operation.terminal.clone(),
        progress_projection: operation.progress.iter().copied().collect(),
    }
}

fn terminal_and_release_locked(
    state: &mut GenerationSupervisorState,
    lease: &GenerationOperationLease,
    class: GenerationTerminalClass,
) -> Result<(), GenerationSupervisorError> {
    let operation = current_operation_mut(state, &lease.identity)?;
    let terminal = GenerationTerminalRecord {
        identity: lease.identity.clone(),
        class,
    };
    operation.terminal = Some(terminal);
    operation.phase = GenerationOperationPhase::Terminal;
    let operation = state
        .operations
        .remove(&lease.identity.operation_id)
        .ok_or(GenerationSupervisorError::StaleLease)?;
    let snapshot = snapshot_for(&operation, GenerationOperationPhase::Released);
    state.released.insert(lease.identity.clone(), snapshot);
    Ok(())
}

fn closed_facts(state: &GenerationSupervisorState) -> GenerationSupervisorClosedFacts {
    GenerationSupervisorClosedFacts {
        lifecycle: state.phase,
        active_operations: state.operations.len(),
        retained_tasks: state
            .expected_workers
            .len()
            .saturating_sub(state.joined_workers.len()),
        expected_workers: state.expected_workers.clone(),
        joined_workers: state.joined_workers.clone(),
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GenerationSupervisorError {
    #[error("progress capacity must be nonzero")]
    InvalidProgressCapacity,
    #[error("generation operation ID must not be empty")]
    EmptyOperationId,
    #[error("generation supervisor is not accepting work")]
    NotAccepting,
    #[error("generation operation `{0}` is already active")]
    DuplicateOperation(String),
    #[error("generation attempt sequence is exhausted")]
    SequenceExhausted,
    #[error("generation operation transition is invalid")]
    InvalidTransition,
    #[error("generation operation lease is stale")]
    StaleLease,
    #[error("generation operation still owns active attempts")]
    AttemptsActive,
    #[error("generation supervisor has not drained")]
    NotDrained,
    #[error("generation worker {0} is already tracked")]
    DuplicateWorker(u64),
    #[error("generation worker {0} is unknown or already joined")]
    UnknownWorker(u64),
    #[error("generation supervisor is closed")]
    Closed,
    #[error("generation supervisor state is poisoned")]
    Poisoned,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_executor_failure_records_failed_terminal_before_release() {
        let supervisor = GenerationSupervisor::new(4).expect("supervisor");
        let (ticket, lease) = supervisor.reserve("request").expect("reserve");
        supervisor.queue(&lease).expect("queue");
        supervisor
            .fail_and_release(&lease)
            .expect("failed terminal and release");
        ticket.detach();

        let released = supervisor
            .snapshot(&lease)
            .expect("snapshot state")
            .expect("released operation snapshot");
        assert_eq!(released.phase, GenerationOperationPhase::Released);
        assert_eq!(
            released
                .authoritative_terminal
                .as_ref()
                .map(|record| record.class),
            Some(GenerationTerminalClass::Failed)
        );
        assert_eq!(released.final_projection, released.authoritative_terminal);
        assert_eq!(supervisor.active_count(), Ok(0));
    }

    #[test]
    fn poisoned_state_never_projects_successful_shutdown_facts() {
        let supervisor = GenerationSupervisor::new(4).expect("supervisor");
        let poisoner = supervisor.clone();
        let _ = std::thread::spawn(move || {
            let _state = poisoner.inner.state.lock().expect("state lock");
            panic!("controlled generation supervisor poison");
        })
        .join();

        assert_eq!(
            supervisor.closed_facts(),
            Err(GenerationSupervisorError::Poisoned)
        );
        assert_eq!(supervisor.close(), Err(GenerationSupervisorError::Poisoned));
        assert_eq!(
            supervisor.active_count(),
            Err(GenerationSupervisorError::Poisoned)
        );
        assert_eq!(supervisor.phase(), Err(GenerationSupervisorError::Poisoned));
    }
}
