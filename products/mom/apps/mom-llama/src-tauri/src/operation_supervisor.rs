// Product builds use this implementation through AppRuntime. Some hierarchy
// and observation methods are intentionally retained for focused unit tests.
#![allow(dead_code)]

use operation_lifecycle::{InstanceId, RunControl, WorkerLedger};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEFAULT_PROGRESS_CAPACITY: usize = 64;
const COMPLETED_WORKER_HISTORY: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisorShutdownOutcome {
    pub phase: LifecyclePhase,
    pub state_poisoned: bool,
    pub active_operations: usize,
    pub retained_tasks: usize,
    pub expected_worker_ids: Vec<String>,
    pub joined_worker_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SupervisorError {
    #[error("operation supervisor state is poisoned")]
    StatePoisoned,
    #[error("operation admission is closed")]
    AdmissionClosed,
    #[error("an operation with this public ID is already active")]
    DuplicateOperation,
    #[error("operation or attempt is unknown")]
    UnknownOperation,
    #[error("the operation lease is stale")]
    StaleLease,
    #[error("the requested lifecycle transition is invalid")]
    InvalidTransition,
    #[error("the operation sequence is exhausted")]
    SequenceExhausted,
    #[error("the supervised worker could not be started")]
    WorkerStart,
    #[error("the supervised worker did not reach the requested boundary")]
    WorkerTimeout,
}

#[derive(Debug)]
struct AttemptData {
    phase: OperationPhase,
    terminal: Option<TerminalRecord>,
    progress: VecDeque<u64>,
}

#[derive(Debug)]
struct AttemptControl {
    identity: AttemptIdentity,
    control: RunControl,
    data: Mutex<AttemptData>,
}

#[derive(Debug)]
struct OperationEntry {
    identity: InstanceId,
    explicit: bool,
    cancellation_requested: bool,
    attempts: BTreeMap<u64, Arc<AttemptControl>>,
}

#[derive(Debug)]
struct WorkerEntry {
    join: Option<JoinHandle<()>>,
    joined: Arc<OnceLock<()>>,
}

#[derive(Debug)]
struct SupervisorState {
    #[cfg(test)]
    publication_barriers: Option<Arc<(std::sync::Barrier, std::sync::Barrier)>>,
    phase: LifecyclePhase,
    next_sequence: u64,
    progress_capacity: usize,
    operations: BTreeMap<String, OperationEntry>,
    attempts: BTreeMap<u64, Arc<AttemptControl>>,
    workers: BTreeMap<String, WorkerEntry>,
    worker_history: WorkerLedger<String>,
    exited_worker_ids: BTreeSet<String>,
    shutdown: Option<SupervisorShutdownOutcome>,
}

#[derive(Debug)]
struct SupervisorInner {
    state: Mutex<SupervisorState>,
    changed: Condvar,
}

#[derive(Clone, Debug)]
pub struct OperationSupervisor(Arc<SupervisorInner>);

#[derive(Clone, Debug)]
pub struct OperationHandle {
    identity: InstanceId,
    operation_id: String,
    supervisor: Weak<SupervisorInner>,
}

#[derive(Debug)]
pub struct OperationTicket {
    attempt: Arc<AttemptControl>,
    supervisor: Weak<SupervisorInner>,
}

#[derive(Clone, Debug)]
pub struct OperationLease {
    attempt: Arc<AttemptControl>,
    supervisor: Weak<SupervisorInner>,
}

#[derive(Debug)]
pub struct OperationReservation {
    pub ticket: OperationTicket,
    pub lease: OperationLease,
}

pub struct SupervisedTask<T> {
    result: tokio::sync::oneshot::Receiver<Result<T, String>>,
    worker_id: String,
    supervisor: OperationSupervisor,
    joined: Arc<OnceLock<()>>,
    _ticket: OperationTicket,
}

impl<T> SupervisedTask<T> {
    pub async fn wait(self) -> Result<T, String> {
        let result = self.result.await.map_err(|_| {
            "Mom Llama's supervised worker stopped before returning a result".to_owned()
        })?;
        if self.joined.get().is_none() {
            let reaped = self.supervisor.reap_worker(&self.worker_id);
            // Another owner may finish joining and age out the diagnostic
            // history between our first observation and acquiring the lock.
            if self.joined.get().is_none() {
                reaped.map_err(|error| error.to_string())?;
            }
        }
        result
    }
}

impl OperationSupervisor {
    pub fn new() -> Self {
        Self::with_config(1, DEFAULT_PROGRESS_CAPACITY)
    }

    pub fn with_config(next_sequence: u64, progress_capacity: usize) -> Self {
        Self(Arc::new(SupervisorInner {
            state: Mutex::new(SupervisorState {
                #[cfg(test)]
                publication_barriers: None,
                phase: LifecyclePhase::Running,
                next_sequence,
                progress_capacity,
                operations: BTreeMap::new(),
                attempts: BTreeMap::new(),
                workers: BTreeMap::new(),
                worker_history: WorkerLedger::new(COMPLETED_WORKER_HISTORY),
                exited_worker_ids: BTreeSet::new(),
                shutdown: None,
            }),
            changed: Condvar::new(),
        }))
    }

    pub fn reserve(&self, operation_id: &str) -> Result<OperationReservation, SupervisorError> {
        let operation = self.create_operation_inner(operation_id, false)?;
        let lease = match self.start_attempt_inner(&operation, OperationPhase::Reserved) {
            Ok(lease) => lease,
            Err(error) => {
                self.lock_state().operations.remove(operation_id);
                return Err(error);
            }
        };
        Ok(OperationReservation {
            ticket: OperationTicket {
                attempt: Arc::clone(&lease.attempt),
                supervisor: Arc::downgrade(&self.0),
            },
            lease,
        })
    }

    pub fn create_operation(&self, operation_id: &str) -> Result<OperationHandle, SupervisorError> {
        self.create_operation_inner(operation_id, true)
    }

    fn create_operation_inner(
        &self,
        operation_id: &str,
        explicit: bool,
    ) -> Result<OperationHandle, SupervisorError> {
        if operation_id.is_empty() {
            return Err(SupervisorError::UnknownOperation);
        }
        let mut state = self.lock_admission_state()?;
        if state.phase != LifecyclePhase::Running {
            return Err(SupervisorError::AdmissionClosed);
        }
        if state.operations.contains_key(operation_id) {
            return Err(SupervisorError::DuplicateOperation);
        }
        let identity = InstanceId::default();
        state.operations.insert(
            operation_id.to_owned(),
            OperationEntry {
                identity: identity.clone(),
                explicit,
                cancellation_requested: false,
                attempts: BTreeMap::new(),
            },
        );
        Ok(OperationHandle {
            identity,
            operation_id: operation_id.to_owned(),
            supervisor: Arc::downgrade(&self.0),
        })
    }

    pub fn start_attempt(
        &self,
        operation: &OperationHandle,
    ) -> Result<OperationLease, SupervisorError> {
        self.start_attempt_inner(operation, OperationPhase::Running)
    }

    fn start_attempt_inner(
        &self,
        operation: &OperationHandle,
        phase: OperationPhase,
    ) -> Result<OperationLease, SupervisorError> {
        let Some(owner) = operation.supervisor.upgrade() else {
            return Err(SupervisorError::StaleLease);
        };
        if !Arc::ptr_eq(&owner, &self.0) {
            return Err(SupervisorError::StaleLease);
        }
        let mut state = self.lock_admission_state()?;
        if state.phase != LifecyclePhase::Running {
            return Err(SupervisorError::AdmissionClosed);
        }
        let sequence = state.next_sequence;
        let Some(next_sequence) = sequence.checked_add(1) else {
            return Err(SupervisorError::SequenceExhausted);
        };
        let operation_entry = state
            .operations
            .get(&operation.operation_id)
            .ok_or(SupervisorError::UnknownOperation)?;
        if operation_entry.identity != operation.identity {
            return Err(SupervisorError::StaleLease);
        }
        let cancellation_requested = operation_entry.cancellation_requested;
        let identity = AttemptIdentity {
            operation_id: operation.operation_id.clone(),
            attempt_id: format!("{}#attempt-{sequence}", operation.operation_id),
            sequence,
        };
        let attempt = Arc::new(AttemptControl {
            identity,
            control: RunControl::new(cancellation_requested),
            data: Mutex::new(AttemptData {
                phase,
                terminal: None,
                progress: VecDeque::new(),
            }),
        });
        state.next_sequence = next_sequence;
        state.attempts.insert(sequence, Arc::clone(&attempt));
        state
            .operations
            .get_mut(&operation.operation_id)
            .ok_or(SupervisorError::UnknownOperation)?
            .attempts
            .insert(sequence, Arc::clone(&attempt));
        Ok(OperationLease {
            attempt,
            supervisor: Arc::downgrade(&self.0),
        })
    }

    pub fn queue(&self, lease: &OperationLease) -> Result<(), SupervisorError> {
        self.transition(lease, OperationPhase::Reserved, OperationPhase::Queued)
    }

    pub fn start(&self, lease: &OperationLease) -> Result<(), SupervisorError> {
        self.transition(lease, OperationPhase::Queued, OperationPhase::Running)
    }

    fn transition(
        &self,
        lease: &OperationLease,
        from: OperationPhase,
        to: OperationPhase,
    ) -> Result<(), SupervisorError> {
        self.require_current(lease)?;
        let mut data = lease
            .attempt
            .data
            .lock()
            .map_err(|_| SupervisorError::UnknownOperation)?;
        if data.phase != from {
            return Err(SupervisorError::InvalidTransition);
        }
        data.phase = to;
        Ok(())
    }

    pub fn publish_progress(
        &self,
        lease: &OperationLease,
        sequence: u64,
    ) -> Result<(), SupervisorError> {
        self.require_current(lease)?;
        let capacity = self.lock_state().progress_capacity;
        let mut data = lease
            .attempt
            .data
            .lock()
            .map_err(|_| SupervisorError::UnknownOperation)?;
        if data.phase != OperationPhase::Running || data.terminal.is_some() {
            return Err(SupervisorError::InvalidTransition);
        }
        if capacity == 0 {
            return Ok(());
        }
        while data.progress.len() >= capacity {
            data.progress.pop_front();
        }
        data.progress.push_back(sequence);
        Ok(())
    }

    pub fn terminal(
        &self,
        lease: &OperationLease,
        class: TerminalClass,
    ) -> Result<(), SupervisorError> {
        self.require_current(lease)?;
        let mut data = lease
            .attempt
            .data
            .lock()
            .map_err(|_| SupervisorError::UnknownOperation)?;
        if data.phase != OperationPhase::Running || data.terminal.is_some() {
            return Err(SupervisorError::InvalidTransition);
        }
        lease
            .attempt
            .control
            .claim_terminal()
            .ok_or(SupervisorError::InvalidTransition)?;
        data.terminal = Some(TerminalRecord {
            class,
            sequence: lease.attempt.identity.sequence,
        });
        data.phase = OperationPhase::Terminal;
        self.0.changed.notify_all();
        Ok(())
    }

    pub fn record_executor_panic(&self, lease: &OperationLease) -> Result<(), SupervisorError> {
        self.terminal(lease, TerminalClass::Failed)
    }

    pub fn release(&self, lease: &OperationLease) -> Result<(), SupervisorError> {
        self.require_current(lease)?;
        {
            let mut data = lease
                .attempt
                .data
                .lock()
                .map_err(|_| SupervisorError::UnknownOperation)?;
            if data.phase != OperationPhase::Terminal {
                return Err(SupervisorError::InvalidTransition);
            }
            data.terminal.ok_or(SupervisorError::InvalidTransition)?;
            data.phase = OperationPhase::Released;
        }
        let mut state = self.lock_state();
        let sequence = lease.attempt.identity.sequence;
        let current = state
            .attempts
            .get(&sequence)
            .is_some_and(|attempt| Arc::ptr_eq(attempt, &lease.attempt));
        if !current {
            return Err(SupervisorError::StaleLease);
        }
        state.attempts.remove(&sequence);
        let operation_id = &lease.attempt.identity.operation_id;
        let remove_operation = {
            let operation = state
                .operations
                .get_mut(operation_id)
                .ok_or(SupervisorError::UnknownOperation)?;
            operation.attempts.remove(&sequence);
            !operation.explicit && operation.attempts.is_empty()
        };
        if remove_operation {
            state.operations.remove(operation_id);
        }
        self.0.changed.notify_all();
        Ok(())
    }

    pub fn finish_attempt(&self, lease: &OperationLease) -> Result<(), SupervisorError> {
        let class = if self.cancellation_requested(lease) {
            TerminalClass::Cancelled
        } else {
            TerminalClass::Completed
        };
        self.terminal(lease, class)?;
        self.release(lease)
    }

    pub fn finish_operation(&self, operation: &OperationHandle) -> Result<(), SupervisorError> {
        let mut state = self.lock_state();
        let entry = state
            .operations
            .get(&operation.operation_id)
            .ok_or(SupervisorError::UnknownOperation)?;
        if entry.identity != operation.identity {
            return Err(SupervisorError::StaleLease);
        }
        if !entry.explicit || !entry.attempts.is_empty() {
            return Err(SupervisorError::InvalidTransition);
        }
        state.operations.remove(&operation.operation_id);
        self.0.changed.notify_all();
        Ok(())
    }

    pub fn request_operation_cancel(
        &self,
        operation: &OperationHandle,
    ) -> Result<(), SupervisorError> {
        let attempts = {
            let mut state = self.lock_state();
            let entry = state
                .operations
                .get_mut(&operation.operation_id)
                .ok_or(SupervisorError::UnknownOperation)?;
            if entry.identity != operation.identity {
                return Err(SupervisorError::StaleLease);
            }
            entry.cancellation_requested = true;
            entry.attempts.values().cloned().collect::<Vec<_>>()
        };
        for attempt in attempts {
            attempt.control.request_cancel();
        }
        Ok(())
    }

    pub fn request_cancel(&self, ticket: &OperationTicket) -> Result<(), SupervisorError> {
        self.request_cancel_attempt(&ticket.attempt)
    }

    fn request_cancel_attempt(&self, attempt: &Arc<AttemptControl>) -> Result<(), SupervisorError> {
        let state = self.lock_state();
        let current = state
            .attempts
            .get(&attempt.identity.sequence)
            .is_some_and(|candidate| Arc::ptr_eq(candidate, attempt));
        if !current {
            return Err(SupervisorError::StaleLease);
        }
        attempt.control.request_cancel();
        Ok(())
    }

    pub fn cancellation_requested(&self, lease: &OperationLease) -> bool {
        lease.attempt.control.cancellation_requested()
    }

    pub fn snapshot(&self, lease: &OperationLease) -> Option<OperationSnapshot> {
        let data = lease.attempt.data.lock().ok()?;
        Some(OperationSnapshot {
            identity: lease.attempt.identity.clone(),
            phase: data.phase,
            cancellation_requested: self.cancellation_requested(lease),
            authoritative_terminal: data.terminal,
            final_projection: data.terminal,
            progress_projection: data.progress.iter().copied().collect(),
        })
    }

    pub fn current_snapshot(&self, operation_id: &str) -> Option<OperationSnapshot> {
        let state = self.lock_state();
        let operation = state.operations.get(operation_id)?;
        let attempt = operation.attempts.values().next()?.clone();
        drop(state);
        let lease = OperationLease {
            attempt,
            supervisor: Arc::downgrade(&self.0),
        };
        self.snapshot(&lease)
    }

    pub fn current_identity(&self, operation_id: &str) -> Option<AttemptIdentity> {
        self.current_snapshot(operation_id)
            .map(|snapshot| snapshot.identity)
    }

    pub fn active_attempts(&self, operation: &OperationHandle) -> Vec<AttemptIdentity> {
        self.lock_state()
            .operations
            .get(&operation.operation_id)
            .filter(|entry| entry.identity == operation.identity)
            .map(|entry| {
                entry
                    .attempts
                    .values()
                    .map(|attempt| attempt.identity.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn operation_active(&self, operation: &OperationHandle) -> bool {
        self.lock_state()
            .operations
            .get(&operation.operation_id)
            .is_some_and(|entry| entry.identity == operation.identity)
    }

    pub fn active_count(&self) -> usize {
        self.lock_state().attempts.len()
    }

    pub fn retained_task_count(&self) -> usize {
        self.lock_state().worker_history.outstanding_count()
    }

    pub fn progress_capacity(&self) -> usize {
        self.lock_state().progress_capacity
    }

    pub fn phase(&self) -> LifecyclePhase {
        self.lock_state().phase
    }

    pub fn begin_quiesce(&self) {
        let attempts = {
            let mut state = self.lock_state();
            if state.phase == LifecyclePhase::Closed {
                return;
            }
            state.phase = LifecyclePhase::Quiescing;
            for operation in state.operations.values_mut() {
                operation.cancellation_requested = true;
            }
            state.attempts.values().cloned().collect::<Vec<_>>()
        };
        for attempt in attempts {
            attempt.control.request_cancel();
        }
        self.0.changed.notify_all();
    }

    pub fn spawn<T, F>(
        &self,
        reservation: OperationReservation,
        operation: F,
    ) -> Result<SupervisedTask<T>, SupervisorError>
    where
        T: Send + 'static,
        F: FnOnce(&OperationLease) -> Result<T, String> + Send + 'static,
    {
        if let Err(error) = self.lock_admission_state() {
            // No worker owns this reservation yet. Consumer drop only requests
            // cancellation, so refusal must release its executor identity here.
            self.queue(&reservation.lease)?;
            self.start(&reservation.lease)?;
            self.terminal(&reservation.lease, TerminalClass::Failed)?;
            self.release(&reservation.lease)?;
            return Err(error);
        }
        self.queue(&reservation.lease)?;
        self.start(&reservation.lease)?;
        let worker_id = format!(
            "mom-operation-worker-{}",
            reservation.lease.attempt.identity.sequence
        );
        let lease = reservation.lease;
        let supervisor = self.clone();
        let thread_supervisor = self.clone();
        let thread_worker_id = worker_id.clone();
        let thread_lease = lease.clone();
        let (start_tx, start_rx) = mpsc::sync_channel(0);
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let joined = Arc::new(OnceLock::new());
        let join = thread::Builder::new()
            .name(worker_id.clone())
            .spawn(move || {
                if start_rx.recv().is_err() {
                    let _ = thread_supervisor
                        .terminal(&thread_lease, TerminalClass::Failed)
                        .and_then(|()| thread_supervisor.release(&thread_lease));
                    thread_supervisor.record_worker_exit(&thread_worker_id);
                    return;
                }
                let result = catch_unwind(AssertUnwindSafe(|| operation(&thread_lease)));
                let published = match &result {
                    Ok(Ok(_)) if thread_supervisor.cancellation_requested(&thread_lease) => {
                        thread_supervisor.terminal(&thread_lease, TerminalClass::Cancelled)
                    }
                    Ok(Ok(_)) => {
                        thread_supervisor.terminal(&thread_lease, TerminalClass::Completed)
                    }
                    Ok(Err(_)) if thread_supervisor.cancellation_requested(&thread_lease) => {
                        thread_supervisor.terminal(&thread_lease, TerminalClass::Cancelled)
                    }
                    Ok(Err(_)) => thread_supervisor.terminal(&thread_lease, TerminalClass::Failed),
                    Err(_) => thread_supervisor.record_executor_panic(&thread_lease),
                };
                let result = match (result, published) {
                    (_, Err(error)) => Err(error.to_string()),
                    (Ok(result), Ok(())) => result,
                    (Err(_), Ok(())) => Err("Mom Llama's supervised worker panicked".to_owned()),
                };
                #[cfg(test)]
                if let Some(barriers) =
                    { thread_supervisor.lock_state().publication_barriers.clone() }
                {
                    barriers.0.wait();
                    barriers.1.wait();
                }
                let _ = result_tx.send(result);
                let _ = thread_supervisor.release(&thread_lease);
                thread_supervisor.record_worker_exit(&thread_worker_id);
            })
            .map_err(|_| {
                let _ = self
                    .terminal(&lease, TerminalClass::Failed)
                    .and_then(|()| self.release(&lease));
                SupervisorError::WorkerStart
            })?;
        {
            let mut state = self.lock_state();
            assert!(
                state.worker_history.note_started(worker_id.clone()),
                "worker sequence is unique"
            );
            state.workers.insert(
                worker_id.clone(),
                WorkerEntry {
                    join: Some(join),
                    joined: Arc::clone(&joined),
                },
            );
        }
        if start_tx.send(()).is_err() {
            return Err(SupervisorError::WorkerStart);
        }
        Ok(SupervisedTask {
            result: result_rx,
            worker_id,
            supervisor,
            joined,
            _ticket: reservation.ticket,
        })
    }

    pub fn shutdown(&self) -> SupervisorShutdownOutcome {
        self.begin_quiesce();
        {
            let state = self.lock_state();
            if let Some(outcome) = &state.shutdown {
                return outcome.clone();
            }
        }
        loop {
            let mut state = self.lock_state();
            while !state.attempts.is_empty() {
                state = self
                    .0
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if state.worker_history.outstanding_count() == 0 {
                break;
            }
            let worker_id = state
                .exited_worker_ids
                .iter()
                .find(|worker_id| state.workers.contains_key(*worker_id))
                .cloned();
            if let Some(worker_id) = worker_id {
                drop(state);
                let _ = self.reap_worker(&worker_id);
                continue;
            }
            let state = self
                .0
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(state);
        }
        let mut state = self.lock_state();
        state.phase = LifecyclePhase::Closed;
        let outcome = SupervisorShutdownOutcome {
            phase: LifecyclePhase::Closed,
            // Poison recovery permits cleanup; it cannot certify clean state.
            state_poisoned: self.0.state.is_poisoned(),
            active_operations: state.attempts.len(),
            retained_tasks: state.worker_history.outstanding_count(),
            expected_worker_ids: state.worker_history.expected().into_iter().collect(),
            joined_worker_ids: state.worker_history.joined().iter().cloned().collect(),
        };
        state.shutdown = Some(outcome.clone());
        self.0.changed.notify_all();
        outcome
    }

    pub fn reap_worker(&self, worker_id: &str) -> Result<(), SupervisorError> {
        let (join, joined) = {
            let mut state = self.lock_state();
            loop {
                let Some(worker) = state.workers.get_mut(worker_id) else {
                    if state.worker_history.was_joined(&worker_id.to_owned()) {
                        return Ok(());
                    }
                    return Err(SupervisorError::UnknownOperation);
                };
                if let Some(join) = worker.join.take() {
                    break (join, Arc::clone(&worker.joined));
                }
                state = self
                    .0
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let _ = join.join();
        let mut state = self.lock_state();
        if !state.worker_history.note_joined(&worker_id.to_owned()) {
            return Err(SupervisorError::UnknownOperation);
        }
        let _ = joined.set(());
        state.workers.remove(worker_id);
        state.exited_worker_ids.remove(worker_id);
        self.0.changed.notify_all();
        Ok(())
    }

    fn record_worker_exit(&self, worker_id: &str) {
        let mut state = self.lock_state();
        state.exited_worker_ids.insert(worker_id.to_owned());
        self.0.changed.notify_all();
    }

    pub fn wait_for_released(
        &self,
        lease: &OperationLease,
        timeout: Duration,
    ) -> Result<OperationSnapshot, SupervisorError> {
        let deadline = Instant::now() + timeout;
        loop {
            let snapshot = self
                .snapshot(lease)
                .ok_or(SupervisorError::UnknownOperation)?;
            if snapshot.phase == OperationPhase::Released {
                return Ok(snapshot);
            }
            if Instant::now() >= deadline {
                return Err(SupervisorError::WorkerTimeout);
            }
            thread::yield_now();
        }
    }

    fn require_current(&self, lease: &OperationLease) -> Result<(), SupervisorError> {
        let Some(owner) = lease.supervisor.upgrade() else {
            return Err(SupervisorError::StaleLease);
        };
        if !Arc::ptr_eq(&owner, &self.0) {
            return Err(SupervisorError::StaleLease);
        }
        let state = self.lock_state();
        let current = state
            .attempts
            .get(&lease.attempt.identity.sequence)
            .is_some_and(|attempt| Arc::ptr_eq(attempt, &lease.attempt));
        if current {
            Ok(())
        } else {
            Err(SupervisorError::StaleLease)
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, SupervisorState> {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_admission_state(&self) -> Result<MutexGuard<'_, SupervisorState>, SupervisorError> {
        self.0
            .state
            .lock()
            .map_err(|_| SupervisorError::StatePoisoned)
    }

    #[cfg(test)]
    pub(crate) fn poison_state_for_test(&self) {
        let supervisor = self.clone();
        assert!(
            std::thread::spawn(move || {
                let _state = supervisor.0.state.lock().expect("unpoisoned supervisor");
                panic!("controlled Mom operation supervisor poison");
            })
            .join()
            .is_err()
        );
    }
}

impl OperationTicket {
    pub fn identity(&self) -> AttemptIdentity {
        self.attempt.identity.clone()
    }

    /// Observes terminality without consuming cancellation or executor authority.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let Some(supervisor) = self.supervisor.upgrade() else {
            return false;
        };
        let deadline = Instant::now() + timeout;
        let mut state = supervisor
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let terminal = self
                .attempt
                .data
                .lock()
                .map(|data| data.terminal.is_some())
                .unwrap_or(true);
            if terminal {
                return true;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (next, wait) = supervisor
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if wait.timed_out() {
                return false;
            }
        }
    }
}

impl Drop for OperationTicket {
    fn drop(&mut self) {
        let terminal = self
            .attempt
            .data
            .lock()
            .map(|data| data.terminal.is_some())
            .unwrap_or(true);
        if terminal {
            return;
        }
        if let Some(supervisor) = self.supervisor.upgrade() {
            let supervisor = OperationSupervisor(supervisor);
            let _ = supervisor.request_cancel_attempt(&self.attempt);
        }
    }
}

impl OperationLease {
    pub fn identity(&self) -> AttemptIdentity {
        self.attempt.identity.clone()
    }

    pub fn supervisor(&self) -> Option<OperationSupervisor> {
        self.supervisor.upgrade().map(OperationSupervisor)
    }

    pub fn cancellation_requested(&self) -> bool {
        self.attempt.control.cancellation_requested()
    }

    pub fn request_cancellation_from_executor(&self) -> Result<(), SupervisorError> {
        self.supervisor()
            .ok_or(SupervisorError::StaleLease)?
            .request_cancel_attempt(&self.attempt)
    }
}

pub fn validate_worker_sets(outcome: &SupervisorShutdownOutcome) -> bool {
    let expected = outcome.expected_worker_ids.iter().collect::<BTreeSet<_>>();
    let joined = outcome.joined_worker_ids.iter().collect::<BTreeSet<_>>();
    expected.len() == outcome.expected_worker_ids.len()
        && joined.len() == outcome.joined_worker_ids.len()
        && expected == joined
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[test]
    fn operation_handles_do_not_target_other_owners_or_reused_public_ids() {
        let left = OperationSupervisor::new();
        let right = OperationSupervisor::new();
        let old = left
            .create_operation("same-request")
            .expect("left operation");
        let other = right
            .create_operation("same-request")
            .expect("right operation");
        assert_eq!(
            right.request_operation_cancel(&old),
            Err(SupervisorError::StaleLease)
        );
        assert_eq!(
            right.finish_operation(&old),
            Err(SupervisorError::StaleLease)
        );
        assert!(right.operation_active(&other));
        left.finish_operation(&old)
            .expect("release old incarnation");
        let new = left
            .create_operation("same-request")
            .expect("new incarnation");
        assert!(matches!(
            left.start_attempt(&old),
            Err(SupervisorError::StaleLease)
        ));
        assert_eq!(
            left.request_operation_cancel(&old),
            Err(SupervisorError::StaleLease)
        );
        assert!(left.operation_active(&new));
    }

    #[test]
    fn shutdown_waits_for_a_join_already_owned_by_another_thread() {
        let supervisor = OperationSupervisor::new();
        let (release, blocked) = mpsc::sync_channel(0);
        let worker_id = "joining-worker".to_owned();
        let join = thread::spawn(move || blocked.recv().expect("release worker"));
        {
            let mut state = supervisor.lock_state();
            assert!(state.worker_history.note_started(worker_id.clone()));
            state.workers.insert(
                worker_id.clone(),
                WorkerEntry {
                    join: Some(join),
                    joined: Arc::new(OnceLock::new()),
                },
            );
        }
        let reaper = {
            let supervisor = supervisor.clone();
            let worker_id = worker_id.clone();
            thread::spawn(move || supervisor.reap_worker(&worker_id))
        };
        // Wait until the reaper owns the handle and is blocked inside join.
        let deadline = Instant::now() + Duration::from_secs(5);
        while supervisor
            .lock_state()
            .workers
            .get(&worker_id)
            .is_some_and(|worker| worker.join.is_some())
        {
            assert!(Instant::now() < deadline, "reaper took the handle");
            thread::yield_now();
        }
        let (closed, outcome) = mpsc::sync_channel(1);
        let (starting, started) = mpsc::sync_channel(0);
        let shutdown = {
            let supervisor = supervisor.clone();
            thread::spawn(move || {
                supervisor.begin_quiesce();
                starting.send(()).expect("shutdown started");
                closed
                    .send(supervisor.shutdown())
                    .expect("shutdown outcome")
            })
        };
        started.recv().expect("shutdown is scheduled");
        let premature = outcome.recv_timeout(Duration::from_millis(50));
        release.send(()).expect("release blocked worker");
        reaper.join().expect("reaper").expect("joined worker");
        shutdown.join().expect("shutdown caller");
        assert!(
            premature.is_err(),
            "shutdown returned before the owner joined"
        );
        let outcome = outcome.recv().expect("joined shutdown outcome");
        assert_eq!(outcome.retained_tasks, 0);
        assert_eq!(outcome.joined_worker_ids, [worker_id]);
        assert!(validate_worker_sets(&outcome));
    }

    #[test]
    fn completed_worker_diagnostics_are_bounded_without_losing_join_accounting() {
        let supervisor = OperationSupervisor::new();
        for id in 0..300 {
            let worker_id = format!("worker-{id}");
            let join = thread::spawn(|| {});
            {
                let mut state = supervisor.lock_state();
                assert!(state.worker_history.note_started(worker_id.clone()));
                state.workers.insert(
                    worker_id.clone(),
                    WorkerEntry {
                        join: Some(join),
                        joined: Arc::new(OnceLock::new()),
                    },
                );
            }
            supervisor.record_worker_exit(&worker_id);
            supervisor.reap_worker(&worker_id).expect("joined worker");
        }
        let state = supervisor.lock_state();
        assert!(state.exited_worker_ids.is_empty());
        assert_eq!(state.worker_history.outstanding_count(), 0);
        assert_eq!(
            state.worker_history.joined().len(),
            COMPLETED_WORKER_HISTORY
        );
    }

    #[test]
    fn public_identity_survives_until_final_send_even_when_consumer_drops() {
        for panics in [false, true] {
            let supervisor = OperationSupervisor::new();
            let barriers = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
            supervisor.lock_state().publication_barriers = Some(Arc::clone(&barriers));
            let reservation = supervisor.reserve("publication").expect("reserve");
            let lease = reservation.lease.clone();
            let (proceed, ready) = mpsc::sync_channel(0);
            let task = supervisor
                .spawn(reservation, move |lease| {
                    ready.recv().expect("consumer dropped before execution");
                    for n in 0..256 {
                        lease
                            .supervisor()
                            .expect("owner")
                            .publish_progress(lease, n)
                            .expect("progress");
                    }
                    assert!(!panics, "controlled executor panic");
                    Ok(())
                })
                .expect("spawn");
            let worker = task.worker_id.clone();
            drop(task);
            proceed.send(()).expect("allow execution");
            barriers.0.wait();
            let duplicate = supervisor.reserve("publication");
            barriers.1.wait();
            assert!(matches!(
                duplicate,
                Err(SupervisorError::DuplicateOperation)
            ));
            let snapshot = supervisor
                .wait_for_released(&lease, Duration::from_secs(5))
                .expect("released");
            assert_eq!(
                snapshot.progress_projection.len(),
                DEFAULT_PROGRESS_CAPACITY
            );
            assert_eq!(
                snapshot.authoritative_terminal.expect("terminal").class,
                if panics {
                    TerminalClass::Failed
                } else {
                    TerminalClass::Cancelled
                }
            );
            supervisor.reap_worker(&worker).expect("join");
            assert!(supervisor.reserve("publication").is_ok());
        }
    }
}
