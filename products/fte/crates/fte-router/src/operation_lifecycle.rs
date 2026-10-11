//! Generation-safe ownership for admitted gateway operations.
//!
//! Guards public-operation identity, cancellation, and terminal/release
//! transitions. Backend workers and the ticket executor own physical completion.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationIdentity {
    pub operation_id: String,
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
}

struct RegistryState {
    next_sequence: u64,
    operations: BTreeMap<String, OperationRecord>,
}

struct OperationRecord {
    identity: OperationIdentity,
    phase: OperationPhase,
    cancellation_requested: bool,
    cancellation: CancellationToken,
    terminal: Option<TerminalClass>,
    released: Arc<Mutex<Option<OperationSnapshot>>>,
}

#[derive(Clone)]
pub struct OperationLease {
    registry: OperationRegistry,
    identity: OperationIdentity,
    released: Arc<Mutex<Option<OperationSnapshot>>>,
    cancellation: CancellationToken,
}

pub struct ConsumerGuard {
    lease: OperationLease,
    cancel_on_drop: bool,
}

impl OperationRegistry {
    pub fn new(next_sequence: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryState {
                next_sequence,
                operations: BTreeMap::new(),
            })),
        }
    }

    pub fn reserve(
        &self,
        operation_id: &str,
    ) -> Result<(ConsumerGuard, OperationLease), RegistryError> {
        let mut state = self.lock()?;
        if state.operations.contains_key(operation_id) {
            return Err(RegistryError::Duplicate);
        }
        let sequence = state.next_sequence;
        state.next_sequence = sequence.checked_add(1).ok_or(RegistryError::Exhausted)?;
        let identity = OperationIdentity {
            operation_id: operation_id.to_owned(),
            sequence,
        };
        let released = Arc::new(Mutex::new(None));
        let cancellation = CancellationToken::new();
        state.operations.insert(
            operation_id.to_owned(),
            OperationRecord {
                identity: identity.clone(),
                phase: OperationPhase::Reserved,
                cancellation_requested: false,
                cancellation: cancellation.clone(),
                terminal: None,
                released: Arc::clone(&released),
            },
        );
        let lease = OperationLease {
            registry: self.clone(),
            identity,
            released,
            cancellation,
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
        let cancellation = record.cancellation.clone();
        drop(state);
        Ok(Some(OperationLease {
            registry: self.clone(),
            identity,
            released,
            cancellation,
        }))
    }

    pub fn request_cancel_all(&self) -> Result<Vec<String>, RegistryError> {
        let mut state = self.lock()?;
        let ids = state.operations.keys().cloned().collect::<Vec<_>>();
        for record in state.operations.values_mut() {
            record.cancellation_requested = true;
            record.cancellation.cancel();
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
                    record.cancellation.cancel();
                }
                (ids, None)
            }
            Err(mut poisoned) => {
                let state = poisoned.get_mut();
                let ids = state.operations.keys().cloned().collect::<Vec<_>>();
                for record in state.operations.values_mut() {
                    record.cancellation_requested = true;
                    record.cancellation.cancel();
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
        Self::new(1)
    }
}

impl OperationLease {
    pub(crate) fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
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

    /// Check every normal completion precondition before publication, retaining
    /// the identity until the caller has attempted its final send.
    pub fn terminal_for_publication(&self, terminal: TerminalClass) -> Result<(), RegistryError> {
        let mut state = self.registry.lock()?;
        let _released_slot = self.released.lock().map_err(|_| RegistryError::Poisoned)?;
        let record = OperationRegistry::record_mut(&mut state, &self.identity)?;
        if record.phase != OperationPhase::Running || record.terminal.is_some() {
            return Err(RegistryError::InvalidTransition);
        }
        record.terminal = Some(terminal);
        record.phase = OperationPhase::Terminal;
        Ok(())
    }

    pub fn terminal_and_release(&self, terminal: TerminalClass) -> Result<(), RegistryError> {
        let mut state = self.registry.lock()?;
        let mut released_slot = self.released.lock().map_err(|_| RegistryError::Poisoned)?;
        let record = OperationRegistry::record_mut(&mut state, &self.identity)?;
        if !matches!(
            record.phase,
            OperationPhase::Queued | OperationPhase::Running
        ) || (record.phase == OperationPhase::Queued && terminal == TerminalClass::Completed)
            || record.terminal.is_some()
        {
            return Err(RegistryError::InvalidTransition);
        }
        record.terminal = Some(terminal);
        record.phase = OperationPhase::Released;
        let released = snapshot(record);
        state.operations.remove(&self.identity.operation_id);
        *released_slot = Some(released);
        Ok(())
    }

    pub fn release(&self) -> Result<(), RegistryError> {
        let mut state = self.registry.lock()?;
        let mut released_slot = self.released.lock().map_err(|_| RegistryError::Poisoned)?;
        let record = OperationRegistry::record_mut(&mut state, &self.identity)?;
        if record.phase != OperationPhase::Terminal {
            return Err(RegistryError::InvalidTransition);
        }
        record.phase = OperationPhase::Released;
        let released = snapshot(record);
        state.operations.remove(&self.identity.operation_id);
        *released_slot = Some(released);
        Ok(())
    }

    pub fn request_cancel(&self) -> Result<(), RegistryError> {
        let mut state = self.registry.lock()?;
        let record = OperationRegistry::record_mut(&mut state, &self.identity)?;
        record.cancellation_requested = true;
        record.cancellation.cancel();
        Ok(())
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

    #[cfg(test)]
    pub(crate) fn poison_released_for_test(&self) {
        let released = Arc::clone(&self.released);
        let _ = std::thread::spawn(move || {
            let _slot = released.lock().expect("released operation snapshot");
            panic!("poison released operation snapshot for deterministic coverage");
        })
        .join();
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

fn snapshot(record: &OperationRecord) -> OperationSnapshot {
    OperationSnapshot {
        identity: record.identity.clone(),
        phase: record.phase,
        cancellation_requested: record.cancellation_requested,
        terminal: record.terminal,
    }
}
