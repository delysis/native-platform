// The full lifecycle surface is exercised by the optional immutable contract
// harness. Product builds use the same implementation through AppRuntime, but
// do not call every hierarchy and observation method directly.
#![cfg_attr(not(all(test, feature = "unstable-w1-contracts")), allow(dead_code))]

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEFAULT_PROGRESS_CAPACITY: usize = 64;

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
    pub active_operations: usize,
    pub retained_tasks: usize,
    pub expected_worker_ids: Vec<String>,
    pub joined_worker_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SupervisorError {
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
    cancellation_requested: AtomicBool,
    data: Mutex<AttemptData>,
}

#[derive(Debug)]
struct OperationEntry {
    explicit: bool,
    cancellation_requested: bool,
    attempts: BTreeMap<u64, Arc<AttemptControl>>,
}

#[derive(Debug)]
struct WorkerEntry {
    join: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct SupervisorState {
    phase: LifecyclePhase,
    next_sequence: u64,
    progress_capacity: usize,
    operations: BTreeMap<String, OperationEntry>,
    attempts: BTreeMap<u64, Arc<AttemptControl>>,
    workers: BTreeMap<String, WorkerEntry>,
    expected_worker_ids: Vec<String>,
    exited_worker_ids: Vec<String>,
    joined_worker_ids: Vec<String>,
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
    _ticket: OperationTicket,
}

#[derive(Clone)]
pub struct ControlledOperation(Arc<ControlledOperationInner>);

struct ControlledOperationInner {
    lease: OperationLease,
    _ticket: Mutex<Option<OperationTicket>>,
    worker_id: String,
    terminal: mpsc::SyncSender<TerminalClass>,
    exit: mpsc::SyncSender<()>,
}

impl<T> SupervisedTask<T> {
    pub async fn wait(self) -> Result<T, String> {
        let result = self.result.await.map_err(|_| {
            "Mom Llama's supervised worker stopped before returning a result".to_owned()
        })?;
        self.supervisor
            .reap_worker(&self.worker_id)
            .map_err(|error| error.to_string())?;
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
                phase: LifecyclePhase::Running,
                next_sequence,
                progress_capacity,
                operations: BTreeMap::new(),
                attempts: BTreeMap::new(),
                workers: BTreeMap::new(),
                expected_worker_ids: Vec::new(),
                exited_worker_ids: Vec::new(),
                joined_worker_ids: Vec::new(),
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
        let mut state = self.lock_state();
        if state.phase != LifecyclePhase::Running {
            return Err(SupervisorError::AdmissionClosed);
        }
        if state.operations.contains_key(operation_id) {
            return Err(SupervisorError::DuplicateOperation);
        }
        state.operations.insert(
            operation_id.to_owned(),
            OperationEntry {
                explicit,
                cancellation_requested: false,
                attempts: BTreeMap::new(),
            },
        );
        Ok(OperationHandle {
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
        let mut state = self.lock_state();
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
        let cancellation_requested = operation_entry.cancellation_requested;
        let identity = AttemptIdentity {
            operation_id: operation.operation_id.clone(),
            attempt_id: format!("{}#attempt-{sequence}", operation.operation_id),
            sequence,
        };
        let attempt = Arc::new(AttemptControl {
            identity,
            cancellation_requested: AtomicBool::new(cancellation_requested),
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
            entry.cancellation_requested = true;
            entry.attempts.values().cloned().collect::<Vec<_>>()
        };
        for attempt in attempts {
            attempt
                .cancellation_requested
                .store(true, Ordering::Release);
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
        attempt
            .cancellation_requested
            .store(true, Ordering::Release);
        Ok(())
    }

    pub fn cancellation_requested(&self, lease: &OperationLease) -> bool {
        lease.attempt.cancellation_requested.load(Ordering::Acquire)
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
            .contains_key(&operation.operation_id)
    }

    pub fn active_count(&self) -> usize {
        self.lock_state().attempts.len()
    }

    pub fn retained_task_count(&self) -> usize {
        self.lock_state().workers.len()
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
            attempt
                .cancellation_requested
                .store(true, Ordering::Release);
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
                    Ok(Ok(_)) => {
                        thread_supervisor.terminal(&thread_lease, TerminalClass::Completed)
                    }
                    Ok(Err(_)) if thread_supervisor.cancellation_requested(&thread_lease) => {
                        thread_supervisor.terminal(&thread_lease, TerminalClass::Cancelled)
                    }
                    Ok(Err(_)) => thread_supervisor.terminal(&thread_lease, TerminalClass::Failed),
                    Err(_) => thread_supervisor.record_executor_panic(&thread_lease),
                }
                .and_then(|()| thread_supervisor.release(&thread_lease));
                let result = match (result, published) {
                    (_, Err(error)) => Err(error.to_string()),
                    (Ok(result), Ok(())) => result,
                    (Err(_), Ok(())) => Err("Mom Llama's supervised worker panicked".to_owned()),
                };
                let _ = result_tx.send(result);
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
            state.expected_worker_ids.push(worker_id.clone());
            state
                .workers
                .insert(worker_id.clone(), WorkerEntry { join: Some(join) });
        }
        if start_tx.send(()).is_err() {
            return Err(SupervisorError::WorkerStart);
        }
        Ok(SupervisedTask {
            result: result_rx,
            worker_id,
            supervisor,
            _ticket: reservation.ticket,
        })
    }

    pub fn spawn_controlled(
        &self,
        operation_id: &str,
    ) -> Result<ControlledOperation, SupervisorError> {
        let reservation = self.reserve(operation_id)?;
        self.queue(&reservation.lease)?;
        self.start(&reservation.lease)?;
        let worker_id = format!(
            "mom-operation-worker-{}",
            reservation.lease.attempt.identity.sequence
        );
        let lease = reservation.lease;
        let thread_lease = lease.clone();
        let supervisor = self.clone();
        let thread_worker_id = worker_id.clone();
        let (start_tx, start_rx) = mpsc::sync_channel(0);
        let (terminal_tx, terminal_rx) = mpsc::sync_channel(1);
        let (exit_tx, exit_rx) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name(worker_id.clone())
            .spawn(move || {
                if start_rx.recv().is_err() {
                    return;
                }
                let Ok(class) = terminal_rx.recv() else {
                    return;
                };
                let _ = supervisor
                    .terminal(&thread_lease, class)
                    .and_then(|()| supervisor.release(&thread_lease));
                let _ = exit_rx.recv();
                supervisor.record_worker_exit(&thread_worker_id);
            })
            .map_err(|_| SupervisorError::WorkerStart)?;
        {
            let mut state = self.lock_state();
            state.expected_worker_ids.push(worker_id.clone());
            state
                .workers
                .insert(worker_id.clone(), WorkerEntry { join: Some(join) });
        }
        start_tx
            .send(())
            .map_err(|_| SupervisorError::WorkerStart)?;
        Ok(ControlledOperation(Arc::new(ControlledOperationInner {
            lease,
            _ticket: Mutex::new(Some(reservation.ticket)),
            worker_id,
            terminal: terminal_tx,
            exit: exit_tx,
        })))
    }

    pub fn spawn_panicking(
        &self,
        operation_id: &str,
    ) -> Result<ControlledOperation, SupervisorError> {
        let reservation = self.reserve(operation_id)?;
        self.queue(&reservation.lease)?;
        self.start(&reservation.lease)?;
        let worker_id = format!(
            "mom-operation-worker-{}",
            reservation.lease.attempt.identity.sequence
        );
        let lease = reservation.lease;
        let thread_lease = lease.clone();
        let supervisor = self.clone();
        let thread_worker_id = worker_id.clone();
        let (start_tx, start_rx) = mpsc::sync_channel(0);
        let (terminal_tx, _terminal_rx) = mpsc::sync_channel(1);
        let (exit_tx, _exit_rx) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name(worker_id.clone())
            .spawn(move || {
                if start_rx.recv().is_err() {
                    return;
                }
                let panicked = catch_unwind(AssertUnwindSafe(|| {
                    panic!("controlled Mom operation panic")
                }))
                .is_err();
                if panicked {
                    let _ = supervisor
                        .record_executor_panic(&thread_lease)
                        .and_then(|()| supervisor.release(&thread_lease));
                }
                supervisor.record_worker_exit(&thread_worker_id);
            })
            .map_err(|_| SupervisorError::WorkerStart)?;
        {
            let mut state = self.lock_state();
            state.expected_worker_ids.push(worker_id.clone());
            state
                .workers
                .insert(worker_id.clone(), WorkerEntry { join: Some(join) });
        }
        start_tx
            .send(())
            .map_err(|_| SupervisorError::WorkerStart)?;
        Ok(ControlledOperation(Arc::new(ControlledOperationInner {
            lease,
            _ticket: Mutex::new(Some(reservation.ticket)),
            worker_id,
            terminal: terminal_tx,
            exit: exit_tx,
        })))
    }

    pub fn request_controlled_terminal(
        &self,
        operation: &ControlledOperation,
        class: TerminalClass,
    ) -> Result<(), SupervisorError> {
        operation
            .0
            .terminal
            .try_send(class)
            .map_err(|_| SupervisorError::InvalidTransition)
    }

    pub fn controlled_snapshot(
        &self,
        operation: &ControlledOperation,
    ) -> Option<OperationSnapshot> {
        self.snapshot(&operation.0.lease)
    }

    pub fn wait_controlled_released(
        &self,
        operation: &ControlledOperation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, SupervisorError> {
        self.wait_for_released(&operation.0.lease, timeout)
    }

    pub fn allow_controlled_exit(
        &self,
        operation: &ControlledOperation,
    ) -> Result<(), SupervisorError> {
        operation
            .0
            .exit
            .try_send(())
            .map_err(|_| SupervisorError::InvalidTransition)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut state = self.lock_state();
        while !state
            .exited_worker_ids
            .iter()
            .any(|worker| worker == &operation.0.worker_id)
        {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(SupervisorError::WorkerTimeout);
            };
            let (next, wait) = self
                .0
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
            if wait.timed_out()
                && !state
                    .exited_worker_ids
                    .iter()
                    .any(|worker| worker == &operation.0.worker_id)
            {
                return Err(SupervisorError::WorkerTimeout);
            }
        }
        Ok(())
    }

    pub fn reap_controlled(&self, operation: &ControlledOperation) -> Result<(), SupervisorError> {
        self.reap_worker(&operation.0.worker_id)
    }

    pub fn cancellation_requested_by_id(&self, operation_id: &str) -> bool {
        self.lock_state()
            .operations
            .get(operation_id)
            .is_some_and(|operation| operation.cancellation_requested)
    }

    pub fn publish_controlled_progress(
        &self,
        operation: &ControlledOperation,
        sequence: u64,
    ) -> Result<(), SupervisorError> {
        self.publish_progress(&operation.0.lease, sequence)
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
            if state.workers.is_empty() {
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
            active_operations: state.attempts.len(),
            retained_tasks: state.workers.len(),
            expected_worker_ids: state.expected_worker_ids.clone(),
            joined_worker_ids: state.joined_worker_ids.clone(),
        };
        state.shutdown = Some(outcome.clone());
        self.0.changed.notify_all();
        outcome
    }

    pub fn reap_worker(&self, worker_id: &str) -> Result<(), SupervisorError> {
        let join = {
            let mut state = self.lock_state();
            let Some(mut worker) = state.workers.remove(worker_id) else {
                if state
                    .joined_worker_ids
                    .iter()
                    .any(|joined| joined == worker_id)
                {
                    return Ok(());
                }
                return Err(SupervisorError::UnknownOperation);
            };
            worker.join.take()
        };
        if let Some(join) = join {
            let _ = join.join();
        }
        let mut state = self.lock_state();
        if !state
            .joined_worker_ids
            .iter()
            .any(|joined| joined == worker_id)
        {
            state.joined_worker_ids.push(worker_id.to_owned());
        }
        self.0.changed.notify_all();
        Ok(())
    }

    fn record_worker_exit(&self, worker_id: &str) {
        let mut state = self.lock_state();
        if !state
            .exited_worker_ids
            .iter()
            .any(|exited| exited == worker_id)
        {
            state.exited_worker_ids.push(worker_id.to_owned());
        }
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
        self.attempt.cancellation_requested.load(Ordering::Acquire)
    }
}

pub fn validate_worker_sets(outcome: &SupervisorShutdownOutcome) -> bool {
    let expected = outcome.expected_worker_ids.iter().collect::<BTreeSet<_>>();
    let joined = outcome.joined_worker_ids.iter().collect::<BTreeSet<_>>();
    expected.len() == outcome.expected_worker_ids.len()
        && joined.len() == outcome.joined_worker_ids.len()
        && expected == joined
}
