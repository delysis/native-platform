//! Generation-safe ownership for gateway operations and backend attempts.
//!
//! This is the router's production registry.  It deliberately owns lifecycle
//! facts that cannot be inferred from a consumer ticket: public-operation
//! identity, executor release authority, terminal linearization, attempts,
//! bounded progress, and retained task accounting.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationIdentity {
    pub operation_id: String,
    pub attempt_id: String,
    pub sequence: u64,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationSnapshot {
    pub identity: OperationIdentity,
    pub phase: OperationPhase,
    pub cancellation_requested: bool,
    pub terminal: Option<TerminalClass>,
    pub progress: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    Duplicate,
    Exhausted,
    Stale,
    InvalidTransition,
    Poisoned,
}

#[derive(Clone)]
pub struct OperationRegistry {
    inner: Arc<Mutex<RegistryState>>,
    progress_capacity: usize,
}

struct RegistryState {
    next_sequence: u64,
    operations: BTreeMap<String, OperationRecord>,
}

struct OperationRecord {
    identity: OperationIdentity,
    phase: OperationPhase,
    cancellation_requested: bool,
    terminal: Option<TerminalClass>,
    progress: VecDeque<u64>,
    progress_capacity: usize,
    next_attempt_sequence: u64,
    attempts: BTreeMap<u64, OperationIdentity>,
    released: Arc<Mutex<Option<OperationSnapshot>>>,
}

#[derive(Clone)]
pub struct OperationLease {
    registry: OperationRegistry,
    identity: OperationIdentity,
    released: Arc<Mutex<Option<OperationSnapshot>>>,
}

pub struct ConsumerGuard {
    lease: OperationLease,
    cancel_on_drop: bool,
}

#[derive(Clone)]
pub struct AttemptLease {
    registry: OperationRegistry,
    operation: OperationIdentity,
    identity: OperationIdentity,
}

impl OperationRegistry {
    pub fn new(next_sequence: u64, progress_capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryState {
                next_sequence,
                operations: BTreeMap::new(),
            })),
            progress_capacity,
        }
    }

    pub fn reserve(
        &self,
        operation_id: &str,
    ) -> Result<(ConsumerGuard, OperationLease), RegistryError> {
        self.reserve_with_capacity(operation_id, self.progress_capacity)
    }

    pub fn reserve_with_capacity(
        &self,
        operation_id: &str,
        progress_capacity: usize,
    ) -> Result<(ConsumerGuard, OperationLease), RegistryError> {
        let mut state = self.lock()?;
        if state.operations.contains_key(operation_id) {
            return Err(RegistryError::Duplicate);
        }
        let sequence = state.next_sequence;
        state.next_sequence = sequence.checked_add(1).ok_or(RegistryError::Exhausted)?;
        let identity = OperationIdentity {
            operation_id: operation_id.to_owned(),
            attempt_id: format!("operation-{sequence}"),
            sequence,
        };
        let released = Arc::new(Mutex::new(None));
        state.operations.insert(
            operation_id.to_owned(),
            OperationRecord {
                identity: identity.clone(),
                phase: OperationPhase::Reserved,
                cancellation_requested: false,
                terminal: None,
                progress: VecDeque::with_capacity(progress_capacity),
                progress_capacity,
                next_attempt_sequence: 1,
                attempts: BTreeMap::new(),
                released: Arc::clone(&released),
            },
        );
        let lease = OperationLease {
            registry: self.clone(),
            identity,
            released,
        };
        Ok((
            ConsumerGuard {
                lease: lease.clone(),
                cancel_on_drop: true,
            },
            lease,
        ))
    }

    pub fn active_count(&self) -> Result<usize, RegistryError> {
        Ok(self.lock()?.operations.len())
    }

    pub fn current(&self, operation_id: &str) -> Result<Option<OperationSnapshot>, RegistryError> {
        Ok(self.lock()?.operations.get(operation_id).map(snapshot))
    }

    pub fn current_lease(
        &self,
        operation_id: &str,
    ) -> Result<Option<OperationLease>, RegistryError> {
        let state = self.lock()?;
        let Some(record) = state.operations.get(operation_id) else {
            return Ok(None);
        };
        let identity = record.identity.clone();
        let released = Arc::clone(&record.released);
        drop(state);
        Ok(Some(OperationLease {
            registry: self.clone(),
            identity,
            released,
        }))
    }

    pub fn request_cancel_all(&self) -> Result<Vec<String>, RegistryError> {
        let mut state = self.lock()?;
        let ids = state.operations.keys().cloned().collect::<Vec<_>>();
        for record in state.operations.values_mut() {
            record.cancellation_requested = true;
        }
        Ok(ids)
    }

    /// Best-effort cancellation used only after shutdown has already decided
    /// that the registry is poisoned. The caller must surface the returned
    /// integrity error; recovered data is never acceptance evidence.
    pub(crate) fn request_cancel_all_for_shutdown(&self) -> (Vec<String>, Option<RegistryError>) {
        match self.inner.lock() {
            Ok(mut state) => {
                let ids = state.operations.keys().cloned().collect::<Vec<_>>();
                for record in state.operations.values_mut() {
                    record.cancellation_requested = true;
                }
                (ids, None)
            }
            Err(mut poisoned) => {
                let state = poisoned.get_mut();
                let ids = state.operations.keys().cloned().collect::<Vec<_>>();
                for record in state.operations.values_mut() {
                    record.cancellation_requested = true;
                }
                (ids, Some(RegistryError::Poisoned))
            }
        }
    }

    fn record_mut<'a>(
        state: &'a mut RegistryState,
        identity: &OperationIdentity,
    ) -> Result<&'a mut OperationRecord, RegistryError> {
        let record = state
            .operations
            .get_mut(&identity.operation_id)
            .ok_or(RegistryError::Stale)?;
        if record.identity != *identity {
            return Err(RegistryError::Stale);
        }
        Ok(record)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, RegistryState>, RegistryError> {
        self.inner.lock().map_err(|_| RegistryError::Poisoned)
    }

    pub(crate) fn diagnostic_active_count(&self) -> (usize, bool) {
        match self.inner.lock() {
            Ok(state) => (state.operations.len(), false),
            Err(poisoned) => (poisoned.get_ref().operations.len(), true),
        }
    }

    #[cfg(test)]
    pub(crate) fn poison_for_test(&self) {
        let inner = Arc::clone(&self.inner);
        let _ = std::thread::spawn(move || {
            let _state = inner.lock().expect("operation registry state");
            panic!("poison operation registry for deterministic coverage");
        })
        .join();
    }
}

impl Default for OperationRegistry {
    fn default() -> Self {
        Self::new(1, 32)
    }
}

impl OperationLease {
    pub fn identity(&self) -> OperationIdentity {
        self.identity.clone()
    }

    pub fn snapshot(&self) -> Result<Option<OperationSnapshot>, RegistryError> {
        let state = self.registry.lock()?;
        if let Some(record) = state.operations.get(&self.identity.operation_id) {
            return Ok((record.identity == self.identity).then(|| snapshot(record)));
        }
        drop(state);
        Ok(self
            .released
            .lock()
            .map_err(|_| RegistryError::Poisoned)?
            .clone())
    }

    pub fn is_active(&self) -> Result<bool, RegistryError> {
        Ok(self
            .registry
            .current(&self.identity.operation_id)?
            .is_some_and(|snapshot| snapshot.identity == self.identity))
    }

    pub fn queue(&self) -> Result<(), RegistryError> {
        self.transition(OperationPhase::Reserved, OperationPhase::Queued)
    }

    pub fn start(&self) -> Result<(), RegistryError> {
        self.transition(OperationPhase::Queued, OperationPhase::Running)
    }

    pub fn terminal(&self, terminal: TerminalClass) -> Result<(), RegistryError> {
        let mut state = self.registry.lock()?;
        let record = OperationRegistry::record_mut(&mut state, &self.identity)?;
        if record.phase != OperationPhase::Running || record.terminal.is_some() {
            return Err(RegistryError::InvalidTransition);
        }
        record.terminal = Some(terminal);
        record.phase = OperationPhase::Terminal;
        Ok(())
    }

    pub fn release(&self) -> Result<(), RegistryError> {
        let mut state = self.registry.lock()?;
        let record = OperationRegistry::record_mut(&mut state, &self.identity)?;
        if record.phase != OperationPhase::Terminal || !record.attempts.is_empty() {
            return Err(RegistryError::InvalidTransition);
        }
        record.phase = OperationPhase::Released;
        let released = snapshot(record);
        state.operations.remove(&self.identity.operation_id);
        *self.released.lock().map_err(|_| RegistryError::Poisoned)? = Some(released);
        Ok(())
    }

    pub fn request_cancel(&self) -> Result<(), RegistryError> {
        let mut state = self.registry.lock()?;
        OperationRegistry::record_mut(&mut state, &self.identity)?.cancellation_requested = true;
        Ok(())
    }

    pub fn publish_progress(&self, sequence: u64) -> Result<(), RegistryError> {
        let mut state = self.registry.lock()?;
        let record = OperationRegistry::record_mut(&mut state, &self.identity)?;
        if record.phase != OperationPhase::Running || record.terminal.is_some() {
            return Err(RegistryError::InvalidTransition);
        }
        if record.progress_capacity != 0 {
            if record.progress.len() == record.progress_capacity {
                record.progress.pop_front();
            }
            record.progress.push_back(sequence);
        }
        Ok(())
    }

    pub fn start_attempt(&self) -> Result<AttemptLease, RegistryError> {
        let mut state = self.registry.lock()?;
        let record = OperationRegistry::record_mut(&mut state, &self.identity)?;
        if record.phase != OperationPhase::Running {
            return Err(RegistryError::InvalidTransition);
        }
        let sequence = record.next_attempt_sequence;
        record.next_attempt_sequence = sequence.checked_add(1).ok_or(RegistryError::Exhausted)?;
        let identity = OperationIdentity {
            operation_id: self.identity.operation_id.clone(),
            attempt_id: format!("{}-attempt-{sequence}", self.identity.attempt_id),
            sequence,
        };
        record.attempts.insert(sequence, identity.clone());
        Ok(AttemptLease {
            registry: self.registry.clone(),
            operation: self.identity.clone(),
            identity,
        })
    }

    pub fn active_attempts(&self) -> Result<Vec<OperationIdentity>, RegistryError> {
        let mut state = self.registry.lock()?;
        Ok(OperationRegistry::record_mut(&mut state, &self.identity)?
            .attempts
            .values()
            .cloned()
            .collect())
    }

    fn transition(
        &self,
        expected: OperationPhase,
        next: OperationPhase,
    ) -> Result<(), RegistryError> {
        let mut state = self.registry.lock()?;
        let record = OperationRegistry::record_mut(&mut state, &self.identity)?;
        if record.phase != expected {
            return Err(RegistryError::InvalidTransition);
        }
        record.phase = next;
        Ok(())
    }
}

impl ConsumerGuard {
    pub fn identity(&self) -> OperationIdentity {
        self.lease.identity()
    }

    pub fn cancel(&self) -> Result<(), RegistryError> {
        self.lease.request_cancel()
    }

    pub fn disarm(mut self) {
        self.cancel_on_drop = false;
    }
}

impl Drop for ConsumerGuard {
    fn drop(&mut self) {
        if self.cancel_on_drop {
            let _ = self.lease.request_cancel();
        }
    }
}

impl AttemptLease {
    pub fn identity(&self) -> OperationIdentity {
        self.identity.clone()
    }

    pub fn cancellation_requested(&self) -> Result<bool, RegistryError> {
        let mut state = self.registry.lock()?;
        Ok(OperationRegistry::record_mut(&mut state, &self.operation)?.cancellation_requested)
    }

    pub fn finish(self) -> Result<(), RegistryError> {
        let mut state = self.registry.lock()?;
        let record = OperationRegistry::record_mut(&mut state, &self.operation)?;
        record
            .attempts
            .remove(&self.identity.sequence)
            .filter(|current| *current == self.identity)
            .map(|_| ())
            .ok_or(RegistryError::Stale)
    }
}

fn snapshot(record: &OperationRecord) -> OperationSnapshot {
    OperationSnapshot {
        identity: record.identity.clone(),
        phase: record.phase,
        cancellation_requested: record.cancellation_requested,
        terminal: record.terminal,
        progress: record.progress.iter().copied().collect(),
    }
}
