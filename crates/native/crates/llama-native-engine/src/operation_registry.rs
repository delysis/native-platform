use llama_native_types::{NativeError, NativeErrorCode};
use std::collections::{BTreeMap, HashMap, VecDeque};
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

type NativeResult<T> = Result<T, NativeError>;
const DEFAULT_PROGRESS_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestClass {
    Generation,
    ControlledGeneration,
    Embedding,
    ResidualTraining,
    ResidualEvaluation,
}

#[derive(Debug)]
pub(crate) enum RequestControls {
    Generation {
        cancellations: Vec<(String, Arc<AtomicBool>)>,
        reasoning_forces: Vec<(String, Arc<AtomicBool>)>,
    },
    ControlledGeneration {
        cancellations: Vec<(String, Arc<AtomicBool>)>,
    },
    Embedding {
        cancellation: Arc<AtomicBool>,
    },
    ResidualTraining {
        cancellation: Arc<AtomicBool>,
    },
    ResidualEvaluation {
        cancellation: Arc<AtomicBool>,
    },
}

impl RequestControls {
    fn cancel_all(&self) -> usize {
        match self {
            Self::Generation { cancellations, .. }
            | Self::ControlledGeneration { cancellations } => {
                set_all(cancellations.iter().map(|(_, flag)| flag))
            }
            Self::Embedding { cancellation }
            | Self::ResidualTraining { cancellation }
            | Self::ResidualEvaluation { cancellation } => {
                cancellation.store(true, Ordering::Release);
                1
            }
        }
    }

    fn cancel_named(&self, name: &str) -> bool {
        match self {
            Self::Generation { cancellations, .. }
            | Self::ControlledGeneration { cancellations } => set_named(cancellations, name),
            Self::Embedding { .. }
            | Self::ResidualTraining { .. }
            | Self::ResidualEvaluation { .. } => false,
        }
    }

    fn force_reasoning_exit(&self, name: &str) -> bool {
        match self {
            Self::Generation {
                reasoning_forces, ..
            } => set_named(reasoning_forces, name),
            Self::ControlledGeneration { .. }
            | Self::Embedding { .. }
            | Self::ResidualEvaluation { .. }
            | Self::ResidualTraining { .. } => false,
        }
    }

    fn force_all_reasoning_exits(&self) -> usize {
        match self {
            Self::Generation {
                reasoning_forces, ..
            } => set_all(reasoning_forces.iter().map(|(_, flag)| flag)),
            Self::ControlledGeneration { .. }
            | Self::Embedding { .. }
            | Self::ResidualEvaluation { .. }
            | Self::ResidualTraining { .. } => 0,
        }
    }

    fn cancellation_requested(&self) -> bool {
        match self {
            Self::Generation { cancellations, .. }
            | Self::ControlledGeneration { cancellations } => cancellations
                .iter()
                .any(|(_, flag)| flag.load(Ordering::Acquire)),
            Self::Embedding { cancellation }
            | Self::ResidualTraining { cancellation }
            | Self::ResidualEvaluation { cancellation } => cancellation.load(Ordering::Acquire),
        }
    }
}

fn set_named(flags: &[(String, Arc<AtomicBool>)], name: &str) -> bool {
    flags
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, flag)| {
            flag.store(true, Ordering::Release);
            true
        })
        .unwrap_or(false)
}

fn set_all<'a>(flags: impl Iterator<Item = &'a Arc<AtomicBool>>) -> usize {
    flags
        .map(|flag| {
            flag.store(true, Ordering::Release);
            1_usize
        })
        .sum()
}

#[derive(Debug)]
pub(crate) struct ActiveRequest {
    request_id: String,
    class: RequestClass,
    controls: RequestControls,
    reservation_nonce: u64,
    identity: RequestIdentity,
    lifecycle: Mutex<RequestLifecycle>,
}

impl ActiveRequest {
    #[must_use]
    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    #[must_use]
    pub(crate) const fn class(&self) -> RequestClass {
        self.class
    }

    pub(crate) fn cancel_all(&self) -> usize {
        self.controls.cancel_all()
    }

    pub(crate) fn cancel_named(&self, name: &str) -> bool {
        self.controls.cancel_named(name)
    }

    pub(crate) fn force_reasoning_exit(&self, name: &str) -> bool {
        self.controls.force_reasoning_exit(name)
    }

    pub(crate) fn force_all_reasoning_exits(&self) -> usize {
        self.controls.force_all_reasoning_exits()
    }

    #[must_use]
    pub(crate) fn identity(&self) -> RequestIdentity {
        self.identity.clone()
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> RequestSnapshot {
        let lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        RequestSnapshot {
            identity: self.identity(),
            phase: lifecycle.phase,
            cancellation_requested: self.controls.cancellation_requested(),
            authoritative_terminal: lifecycle.terminal,
            final_projection: lifecycle.terminal,
            progress_projection: lifecycle.progress.iter().copied().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistryPhase {
    Running,
    Quiescing,
    Closed,
}

/// Executor-owned lifecycle for one admitted native attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestPhase {
    Reserved,
    Queued,
    Running,
    Terminal,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestTerminalClass {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RequestIdentity {
    pub(crate) operation_id: String,
    pub(crate) attempt_id: String,
    pub(crate) sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RequestTerminal {
    pub(crate) class: RequestTerminalClass,
    pub(crate) sequence: u64,
}

#[derive(Debug)]
struct RequestLifecycle {
    phase: RequestPhase,
    terminal: Option<RequestTerminal>,
    progress: VecDeque<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) struct RequestSnapshot {
    pub(crate) identity: RequestIdentity,
    pub(crate) phase: RequestPhase,
    pub(crate) cancellation_requested: bool,
    pub(crate) authoritative_terminal: Option<RequestTerminal>,
    pub(crate) final_projection: Option<RequestTerminal>,
    pub(crate) progress_projection: Vec<u64>,
}

#[derive(Debug)]
struct RegistryState {
    phase: RegistryPhase,
    next_nonce: u64,
    progress_capacity: usize,
    active: HashMap<String, Arc<ActiveRequest>>,
    attempts: BTreeMap<u64, Arc<ActiveRequest>>,
    expected_worker_ids: Vec<String>,
    joined_worker_ids: Vec<String>,
    shutdown: Option<RegistryShutdownOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RegistryShutdownOutcome {
    pub(crate) phase: RequestRegistryPhase,
    pub(crate) state_poisoned: bool,
    pub(crate) active_operations: usize,
    pub(crate) retained_tasks: usize,
    pub(crate) expected_workers: usize,
    pub(crate) joined_workers: usize,
    pub(crate) expected_worker_ids: Vec<String>,
    pub(crate) joined_worker_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequestRegistryPhase {
    Closed,
}

#[derive(Debug)]
pub(crate) struct RequestRegistry {
    state: Mutex<RegistryState>,
    drained: Condvar,
    #[cfg(test)]
    releases: AtomicU64,
}

impl RequestRegistry {
    pub(crate) fn new() -> Self {
        Self::with_config(0, DEFAULT_PROGRESS_CAPACITY)
    }

    pub(crate) fn with_config(next_nonce: u64, progress_capacity: usize) -> Self {
        Self {
            state: Mutex::new(RegistryState {
                phase: RegistryPhase::Running,
                next_nonce,
                progress_capacity,
                active: HashMap::new(),
                attempts: BTreeMap::new(),
                expected_worker_ids: Vec::new(),
                joined_worker_ids: Vec::new(),
                shutdown: None,
            }),
            drained: Condvar::new(),
            #[cfg(test)]
            releases: AtomicU64::new(0),
        }
    }

    pub(crate) fn with_external_worker(worker_id: String) -> Self {
        let registry = Self::new();
        registry
            .lock_recovering_poison()
            .expected_worker_ids
            .push(worker_id);
        registry
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        request_id: impl Into<String>,
        class: RequestClass,
        controls: RequestControls,
    ) -> NativeResult<(Arc<ActiveRequest>, RequestLease)> {
        let request_id = request_id.into();
        let mut state = self.state.lock().map_err(|_| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native request registry is poisoned",
            )
        })?;
        if state.phase != RegistryPhase::Running {
            return Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native request admission is closed",
            ));
        }
        if state.active.contains_key(&request_id) {
            return Err(NativeError::new(
                NativeErrorCode::DuplicateActiveRequest,
                format!("native request ID {request_id:?} is already active"),
            ));
        }
        let reservation_nonce = state.next_nonce;
        state.next_nonce = state.next_nonce.checked_add(1).ok_or_else(|| {
            NativeError::new(
                NativeErrorCode::Internal,
                "native request reservation sequence overflowed",
            )
        })?;
        let entry = Arc::new(ActiveRequest {
            request_id: request_id.clone(),
            class,
            controls,
            reservation_nonce,
            identity: RequestIdentity {
                operation_id: request_id.clone(),
                attempt_id: format!("{request_id}#attempt-{reservation_nonce}"),
                sequence: reservation_nonce,
            },
            lifecycle: Mutex::new(RequestLifecycle {
                phase: RequestPhase::Reserved,
                terminal: None,
                progress: VecDeque::new(),
            }),
        });
        state.active.insert(request_id, Arc::clone(&entry));
        state.attempts.insert(reservation_nonce, Arc::clone(&entry));
        Ok((
            Arc::clone(&entry),
            RequestLease {
                registry: Arc::clone(self),
                entry,
            },
        ))
    }

    pub(crate) fn active(&self, request_id: &str) -> Option<Arc<ActiveRequest>> {
        self.lock_recovering_poison()
            .active
            .get(request_id)
            .cloned()
    }

    pub(crate) fn queue(&self, lease: &RequestLease) -> NativeResult<()> {
        self.transition(lease, RequestPhase::Reserved, RequestPhase::Queued)
    }

    pub(crate) fn start(&self, lease: &RequestLease) -> NativeResult<()> {
        self.transition(lease, RequestPhase::Queued, RequestPhase::Running)
    }

    fn transition(
        &self,
        lease: &RequestLease,
        from: RequestPhase,
        to: RequestPhase,
    ) -> NativeResult<()> {
        self.require_current(lease)?;
        let mut lifecycle = lease
            .entry
            .lifecycle
            .lock()
            .map_err(|_| registry_error("native request lifecycle is poisoned"))?;
        if lifecycle.phase != from {
            return Err(invalid_transition());
        }
        lifecycle.phase = to;
        self.drained.notify_all();
        Ok(())
    }

    pub(crate) fn publish_progress(&self, lease: &RequestLease, sequence: u64) -> NativeResult<()> {
        self.require_current(lease)?;
        let capacity = self.lock_recovering_poison().progress_capacity;
        let mut lifecycle = lease
            .entry
            .lifecycle
            .lock()
            .map_err(|_| registry_error("native request lifecycle is poisoned"))?;
        if lifecycle.phase != RequestPhase::Running || lifecycle.terminal.is_some() {
            return Err(invalid_transition());
        }
        if capacity != 0 {
            while lifecycle.progress.len() >= capacity {
                lifecycle.progress.pop_front();
            }
            lifecycle.progress.push_back(sequence);
        }
        Ok(())
    }

    pub(crate) fn terminal(
        &self,
        lease: &RequestLease,
        class: RequestTerminalClass,
    ) -> NativeResult<()> {
        self.require_current(lease)?;
        let mut lifecycle = lease
            .entry
            .lifecycle
            .lock()
            .map_err(|_| registry_error("native request lifecycle is poisoned"))?;
        if lifecycle.phase != RequestPhase::Running || lifecycle.terminal.is_some() {
            return Err(invalid_transition());
        }
        lifecycle.terminal = Some(RequestTerminal {
            class,
            sequence: lease.entry.reservation_nonce,
        });
        lifecycle.phase = RequestPhase::Terminal;
        self.drained.notify_all();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn active_count(&self) -> usize {
        self.lock_recovering_poison().attempts.len()
    }

    #[cfg(test)]
    pub(crate) fn retained_task_count(&self) -> usize {
        0
    }

    pub(crate) fn begin_quiesce_and_cancel_all(&self) {
        let active = {
            let mut state = self.lock_recovering_poison();
            if state.phase == RegistryPhase::Closed {
                return;
            }
            state.phase = RegistryPhase::Quiescing;
            state.attempts.values().cloned().collect::<Vec<_>>()
        };
        for entry in active {
            entry.cancel_all();
        }
        self.drained.notify_all();
    }

    pub(crate) fn shutdown(&self) -> RegistryShutdownOutcome {
        self.begin_quiesce_and_cancel_all();
        {
            let state = self.lock_recovering_poison();
            if let Some(outcome) = &state.shutdown {
                return outcome.clone();
            }
        }
        let mut state = self.lock_recovering_poison();
        while !state.attempts.is_empty() {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.phase = RegistryPhase::Closed;
        let expected_worker_ids = state.expected_worker_ids.clone();
        let joined_worker_ids = state.joined_worker_ids.clone();
        let outcome = RegistryShutdownOutcome {
            phase: RequestRegistryPhase::Closed,
            state_poisoned: self.state.is_poisoned(),
            active_operations: state.attempts.len(),
            retained_tasks: 0,
            expected_workers: expected_worker_ids.len(),
            joined_workers: joined_worker_ids.len(),
            expected_worker_ids,
            joined_worker_ids,
        };
        state.shutdown = Some(outcome.clone());
        self.drained.notify_all();
        outcome
    }

    pub(crate) fn record_external_worker_joined(&self, worker_id: &str) {
        let mut state = self.lock_recovering_poison();
        if !state
            .joined_worker_ids
            .iter()
            .any(|worker| worker == worker_id)
        {
            state.joined_worker_ids.push(worker_id.to_owned());
        }
        self.drained.notify_all();
    }

    pub(crate) fn mark_closed(&self) -> NativeResult<()> {
        let mut state = self.lock_recovering_poison();
        if !state.attempts.is_empty() {
            return Err(NativeError::new(
                NativeErrorCode::Internal,
                format!(
                    "native worker joined with {} active request reservation(s)",
                    state.attempts.len()
                ),
            ));
        }
        state.phase = RegistryPhase::Closed;
        self.drained.notify_all();
        // Draining remains mandatory after poison, but recovered cleanup state
        // cannot grant successful joined-owner authority.
        if self.state.is_poisoned() {
            Err(registry_error("native request registry is poisoned"))
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_until_drained(&self) {
        let mut state = self.lock_recovering_poison();
        while !state.attempts.is_empty() {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[cfg(test)]
    fn contains(&self, request_id: &str) -> bool {
        self.lock_recovering_poison()
            .active
            .contains_key(request_id)
    }

    #[cfg(test)]
    fn release_count(&self) -> u64 {
        self.releases.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn release_if_current(&self, entry: &Arc<ActiveRequest>) {
        let mut state = self.lock_recovering_poison();
        let current_matches = state
            .attempts
            .get(&entry.reservation_nonce)
            .is_some_and(|current| Arc::ptr_eq(current, entry));
        if current_matches {
            if state.active.get(entry.request_id()).is_some_and(|current| {
                Arc::ptr_eq(current, entry) && current.reservation_nonce == entry.reservation_nonce
            }) {
                state.active.remove(entry.request_id());
            }
            state.attempts.remove(&entry.reservation_nonce);
            #[cfg(test)]
            self.releases.fetch_add(1, Ordering::AcqRel);
        }
        if state.attempts.is_empty() {
            self.drained.notify_all();
        }
    }

    fn abandon_if_current(&self, entry: &Arc<ActiveRequest>) {
        let mut state = self.lock_recovering_poison();
        let current_matches = state
            .attempts
            .get(&entry.reservation_nonce)
            .is_some_and(|current| Arc::ptr_eq(current, entry));
        if !current_matches {
            return;
        }
        let mut lifecycle = entry
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if lifecycle.terminal.is_none() {
            lifecycle.terminal = Some(RequestTerminal {
                class: if entry.controls.cancellation_requested() {
                    RequestTerminalClass::Cancelled
                } else {
                    RequestTerminalClass::Failed
                },
                sequence: entry.reservation_nonce,
            });
        }
        state.attempts.remove(&entry.reservation_nonce);
        if state.active.get(entry.request_id()).is_some_and(|current| {
            Arc::ptr_eq(current, entry) && current.reservation_nonce == entry.reservation_nonce
        }) {
            state.active.remove(entry.request_id());
        }
        lifecycle.phase = RequestPhase::Released;
        #[cfg(test)]
        self.releases.fetch_add(1, Ordering::AcqRel);
        drop(lifecycle);
        drop(state);
        self.drained.notify_all();
    }

    fn require_current(&self, lease: &RequestLease) -> NativeResult<()> {
        let state = self.lock_recovering_poison();
        let current = state
            .attempts
            .get(&lease.entry.reservation_nonce)
            .is_some_and(|entry| Arc::ptr_eq(entry, &lease.entry));
        if current { Ok(()) } else { Err(stale_lease()) }
    }

    fn lock_recovering_poison(&self) -> MutexGuard<'_, RegistryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(crate) fn poison_state_for_test(self: &Arc<Self>) {
        let registry = Arc::clone(self);
        assert!(
            std::thread::spawn(move || {
                let _state = registry.state.lock().expect("unpoisoned registry");
                panic!("controlled native request registry poison");
            })
            .join()
            .is_err()
        );
    }
}

fn registry_error(message: impl Into<String>) -> NativeError {
    NativeError::new(NativeErrorCode::Internal, message)
}

fn stale_lease() -> NativeError {
    registry_error("native request lease is stale")
}

fn invalid_transition() -> NativeError {
    registry_error("native request lifecycle transition is invalid")
}

#[derive(Debug)]
pub(crate) struct RequestLease {
    registry: Arc<RequestRegistry>,
    entry: Arc<ActiveRequest>,
}

impl Drop for RequestLease {
    fn drop(&mut self) {
        // The non-clonable executor authority is released exactly once, after
        // the worker has attempted its final result publication.
        self.registry.abandon_if_current(&self.entry);
    }
}

impl RequestLease {
    #[must_use]
    pub(crate) fn sequence(&self) -> u64 {
        self.entry.reservation_nonce
    }

    pub(crate) fn queued(&self) -> NativeResult<()> {
        self.registry.queue(self)
    }

    pub(crate) fn running(&self) -> NativeResult<()> {
        self.registry.start(self)
    }

    pub(crate) fn progress(&self, sequence: u64) -> NativeResult<()> {
        self.registry.publish_progress(self, sequence)
    }

    pub(crate) fn finished(&self, class: RequestTerminalClass) -> NativeResult<()> {
        self.registry.terminal(self, class)
    }

    pub(crate) fn completed_or_failed(&self, succeeded: bool) -> NativeResult<()> {
        let class = if succeeded {
            RequestTerminalClass::Completed
        } else if self.entry.controls.cancellation_requested() {
            RequestTerminalClass::Cancelled
        } else {
            RequestTerminalClass::Failed
        };
        self.finished(class)
    }

    pub(crate) fn cancel_queued(&self) -> NativeResult<()> {
        self.registry.require_current(self)?;
        {
            let mut lifecycle = self
                .entry
                .lifecycle
                .lock()
                .map_err(|_| registry_error("native request lifecycle is poisoned"))?;
            if lifecycle.phase != RequestPhase::Queued || lifecycle.terminal.is_some() {
                return Err(invalid_transition());
            }
            lifecycle.terminal = Some(RequestTerminal {
                class: RequestTerminalClass::Cancelled,
                sequence: self.entry.reservation_nonce,
            });
            lifecycle.phase = RequestPhase::Terminal;
        }
        Ok(()) // Keep executor identity through the final result send attempt.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn controls() -> RequestControls {
        RequestControls::Generation {
            cancellations: vec![("case".to_owned(), Arc::new(AtomicBool::new(false)))],
            reasoning_forces: vec![("case".to_owned(), Arc::new(AtomicBool::new(false)))],
        }
    }

    #[test]
    fn terminal_recording_retains_identity_until_publication_owner_drops() {
        for queued in [true, false] {
            let registry = Arc::new(RequestRegistry::new());
            let (_, lease) = registry
                .reserve("request", RequestClass::Generation, controls())
                .expect("reserve");
            lease.queued().expect("queued");
            if queued {
                lease.cancel_queued().expect("queued cancellation");
            } else {
                lease.running().expect("running");
                lease.completed_or_failed(true).expect("completed");
            }
            assert_eq!(registry.active_count(), 1);
            assert!(
                registry
                    .reserve("request", RequestClass::Generation, controls())
                    .is_err()
            );
            drop(lease); // The executor drops this only after its final send attempt.
            assert_eq!(registry.active_count(), 0);
            assert_eq!(registry.release_count(), 1);
        }
    }

    #[test]
    fn executor_lease_not_ticket_interest_owns_request_identity() {
        let registry = Arc::new(RequestRegistry::new());
        let (ticket_control, lease) = registry
            .reserve("request", RequestClass::Generation, controls())
            .expect("first request reserves its identity");

        drop(ticket_control);
        assert_eq!(
            registry
                .reserve("request", RequestClass::Generation, controls())
                .expect_err("ticket interest cannot release executor identity")
                .code,
            llama_native_types::NativeErrorCode::DuplicateActiveRequest
        );

        drop(lease);
        assert_eq!(registry.release_count(), 1);
        assert!(
            registry
                .reserve("request", RequestClass::Generation, controls())
                .is_ok()
        );
    }

    #[test]
    fn stale_lease_cannot_remove_a_newer_reservation() {
        let registry = Arc::new(RequestRegistry::new());
        let (_, first_lease) = registry
            .reserve("request", RequestClass::Generation, controls())
            .expect("first request");
        let stale_entry = Arc::clone(&first_lease.entry);
        drop(first_lease);

        let (_, second_lease) = registry
            .reserve("request", RequestClass::Generation, controls())
            .expect("second request");
        registry.release_if_current(&stale_entry);

        assert!(registry.contains("request"));
        assert_eq!(registry.release_count(), 1);
        drop(second_lease);
        assert!(!registry.contains("request"));
        assert_eq!(registry.release_count(), 2);
    }

    #[test]
    fn begin_quiesce_rejects_new_reservations_and_cancels_existing() {
        let registry = Arc::new(RequestRegistry::new());
        let cancellation = Arc::new(AtomicBool::new(false));
        let (_, lease) = registry
            .reserve(
                "request",
                RequestClass::Embedding,
                RequestControls::Embedding {
                    cancellation: Arc::clone(&cancellation),
                },
            )
            .expect("request reserves");

        registry.begin_quiesce_and_cancel_all();
        assert!(cancellation.load(Ordering::Acquire));
        assert!(
            registry
                .reserve("other", RequestClass::Generation, controls())
                .is_err()
        );
        drop(lease);
        registry.wait_until_drained();
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn residual_training_has_request_wide_cancellation_and_executor_lease() {
        let registry = Arc::new(RequestRegistry::new());
        let cancellation = Arc::new(AtomicBool::new(false));
        let (control, lease) = registry
            .reserve(
                "training",
                RequestClass::ResidualTraining,
                RequestControls::ResidualTraining {
                    cancellation: Arc::clone(&cancellation),
                },
            )
            .expect("training reserves");
        assert!(!control.cancel_named("pair"));
        assert!(!control.force_reasoning_exit("pair"));
        assert_eq!(control.force_all_reasoning_exits(), 0);
        assert!(!cancellation.load(Ordering::Acquire));
        registry.begin_quiesce_and_cancel_all();
        assert!(cancellation.load(Ordering::Acquire));
        drop(control);
        assert_eq!(registry.active_count(), 1);
        drop(lease);
        registry.wait_until_drained();
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn residual_evaluation_cancellation_survives_ticket_drop_until_executor_release() {
        let registry = Arc::new(RequestRegistry::new());
        let cancellation = Arc::new(AtomicBool::new(false));
        let (control, lease) = registry
            .reserve(
                "evaluation",
                RequestClass::ResidualEvaluation,
                RequestControls::ResidualEvaluation {
                    cancellation: Arc::clone(&cancellation),
                },
            )
            .expect("evaluation reserves");
        assert!(!control.cancel_named("case"));
        assert!(!control.force_reasoning_exit("case"));
        assert_eq!(control.cancel_all(), 1);
        assert!(control.controls.cancellation_requested());
        drop(control);
        assert_eq!(registry.active_count(), 1);
        drop(lease);
        registry.wait_until_drained();
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn cancellation_routes_only_to_the_current_operation() {
        let registry = Arc::new(RequestRegistry::new());
        let old = Arc::new(AtomicBool::new(false));
        let (_, old_lease) = registry
            .reserve(
                "request",
                RequestClass::Embedding,
                RequestControls::Embedding {
                    cancellation: Arc::clone(&old),
                },
            )
            .expect("old request");
        let stale_control = Arc::clone(&old_lease.entry);
        drop(old_lease);

        let current = Arc::new(AtomicBool::new(false));
        let (_, _current_lease) = registry
            .reserve(
                "request",
                RequestClass::Embedding,
                RequestControls::Embedding {
                    cancellation: Arc::clone(&current),
                },
            )
            .expect("current request");

        stale_control.cancel_all();
        assert!(old.load(Ordering::Acquire));
        assert!(!current.load(Ordering::Acquire));
    }

    #[test]
    fn registry_drain_waits_until_the_executor_lease_drops() {
        let registry = Arc::new(RequestRegistry::new());
        let (_, lease) = registry
            .reserve("request", RequestClass::Generation, controls())
            .expect("request reserves");
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let (drained_tx, drained_rx) = std::sync::mpsc::sync_channel(0);
        let waiter_registry = Arc::clone(&registry);
        let waiter = std::thread::spawn(move || {
            started_tx.send(()).expect("test coordinator remains live");
            waiter_registry.wait_until_drained();
            drained_tx.send(()).expect("test coordinator remains live");
        });

        started_rx.recv().expect("drain waiter started");
        assert!(
            drained_rx
                .recv_timeout(std::time::Duration::from_millis(10))
                .is_err(),
            "drain cannot finish while the executor lease remains live"
        );
        drop(lease);
        drained_rx.recv().expect("lease release wakes drain waiter");
        waiter.join().expect("drain waiter joins");
        assert_eq!(registry.release_count(), 1);
    }

    #[test]
    fn closed_registry_requires_zero_executor_leases_and_rejects_reopening() {
        let registry = Arc::new(RequestRegistry::new());
        let (_, lease) = registry
            .reserve("request", RequestClass::Generation, controls())
            .expect("request reserves");

        assert_eq!(
            registry
                .mark_closed()
                .expect_err("an active executor lease prevents a joined proof")
                .code,
            NativeErrorCode::Internal
        );
        drop(lease);
        registry.mark_closed().expect("drained registry closes");
        assert_eq!(registry.release_count(), 1);
        assert_eq!(registry.active_count(), 0);
        assert_eq!(
            registry
                .reserve("request", RequestClass::Generation, controls())
                .expect_err("a closed registry cannot be reopened")
                .code,
            NativeErrorCode::WorkerStopped
        );
    }
}
