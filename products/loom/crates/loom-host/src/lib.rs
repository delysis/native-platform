#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

use thiserror::Error;

use loom_types::{BranchId, CommandId, DocumentId, GenerationRunId, ProjectId};

pub const MAX_QUEUE_CAPACITY: usize = 65_536;
pub const DEFAULT_MAX_ACTIVE_GENERATION_BRANCHES: usize = 64;
pub const MAX_ACTIVE_GENERATION_BRANCHES: usize = 4_096;

/// Minimal cancellation authority retained by the lifecycle registry. The
/// concrete inference handle stays behind this boundary, so the host owns
/// routing without depending on one backend implementation.
pub trait BranchCancellation: std::fmt::Debug + Send + Sync + 'static {
    fn cancel_branch(&self, branch_id: BranchId) -> bool;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationFamilyIdentity {
    pub request_id: String,
    pub project_id: ProjectId,
    pub session_id: CommandId,
    pub document_id: DocumentId,
}

#[derive(Debug)]
pub struct GenerationFamilyRegistration {
    pub identity: GenerationFamilyIdentity,
    pub branches: Vec<(GenerationRunId, BranchId)>,
    pub cancellation: Arc<dyn BranchCancellation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveGenerationRoute {
    pub identity: GenerationFamilyIdentity,
    pub run_id: GenerationRunId,
    pub branch_id: BranchId,
}

#[derive(Debug)]
struct ActiveFamily {
    identity: GenerationFamilyIdentity,
    branches: Vec<(GenerationRunId, BranchId)>,
    cancellation: Arc<dyn BranchCancellation>,
}

#[derive(Debug, Default)]
struct GenerationRegistryState {
    families: BTreeMap<String, ActiveFamily>,
    branch_requests: BTreeMap<BranchId, String>,
    run_branches: BTreeMap<GenerationRunId, BranchId>,
}

/// Process-local owner for active generation lifecycles.
///
/// Durable state belongs to `loom-store`; this registry contains only live
/// cancellation routes. Losing it on restart therefore cannot make a branch
/// appear completed or erase its persisted provenance.
#[derive(Debug)]
pub struct GenerationRegistry {
    max_active_branches: usize,
    state: Mutex<GenerationRegistryState>,
}

impl Default for GenerationRegistry {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ACTIVE_GENERATION_BRANCHES)
            .expect("the default active-generation limit is valid")
    }
}

impl GenerationRegistry {
    pub fn new(max_active_branches: usize) -> Result<Self, GenerationRegistryError> {
        if max_active_branches == 0 || max_active_branches > MAX_ACTIVE_GENERATION_BRANCHES {
            return Err(GenerationRegistryError::InvalidCapacity {
                capacity: max_active_branches,
            });
        }
        Ok(Self {
            max_active_branches,
            state: Mutex::new(GenerationRegistryState::default()),
        })
    }

    pub fn register(
        &self,
        registration: GenerationFamilyRegistration,
    ) -> Result<(), GenerationRegistryError> {
        if registration.identity.request_id.trim().is_empty() {
            return Err(GenerationRegistryError::EmptyRequestId);
        }
        if registration.branches.is_empty() {
            return Err(GenerationRegistryError::EmptyFamily);
        }
        let mut runs = BTreeSet::new();
        let mut branches = BTreeSet::new();
        for (run_id, branch_id) in &registration.branches {
            if !runs.insert(*run_id) {
                return Err(GenerationRegistryError::DuplicateRun(*run_id));
            }
            if !branches.insert(*branch_id) {
                return Err(GenerationRegistryError::DuplicateBranch(*branch_id));
            }
        }

        let mut state = self.lock()?;
        if state
            .families
            .contains_key(&registration.identity.request_id)
        {
            return Err(GenerationRegistryError::DuplicateRequest(
                registration.identity.request_id,
            ));
        }
        let requested_total = state
            .branch_requests
            .len()
            .checked_add(registration.branches.len())
            .ok_or(GenerationRegistryError::CapacityExceeded {
                active: state.branch_requests.len(),
                requested: registration.branches.len(),
                limit: self.max_active_branches,
            })?;
        if requested_total > self.max_active_branches {
            return Err(GenerationRegistryError::CapacityExceeded {
                active: state.branch_requests.len(),
                requested: registration.branches.len(),
                limit: self.max_active_branches,
            });
        }
        for (run_id, branch_id) in &registration.branches {
            if state.run_branches.contains_key(run_id) {
                return Err(GenerationRegistryError::RunAlreadyActive(*run_id));
            }
            if state.branch_requests.contains_key(branch_id) {
                return Err(GenerationRegistryError::BranchAlreadyActive(*branch_id));
            }
        }

        let request_id = registration.identity.request_id.clone();
        for (run_id, branch_id) in &registration.branches {
            state.run_branches.insert(*run_id, *branch_id);
            state.branch_requests.insert(*branch_id, request_id.clone());
        }
        state.families.insert(
            request_id,
            ActiveFamily {
                identity: registration.identity,
                branches: registration.branches,
                cancellation: registration.cancellation,
            },
        );
        Ok(())
    }

    pub fn route_for_run(
        &self,
        run_id: GenerationRunId,
    ) -> Result<Option<ActiveGenerationRoute>, GenerationRegistryError> {
        let state = self.lock()?;
        let Some(branch_id) = state.run_branches.get(&run_id).copied() else {
            return Ok(None);
        };
        route_for_branch_locked(&state, branch_id).map(Some)
    }

    pub fn cancel_run(
        &self,
        project_id: ProjectId,
        session_id: CommandId,
        run_id: GenerationRunId,
    ) -> Result<bool, GenerationRegistryError> {
        let (route, cancellation) = {
            let state = self.lock()?;
            let branch_id = state
                .run_branches
                .get(&run_id)
                .copied()
                .ok_or(GenerationRegistryError::RunNotActive(run_id))?;
            let route = route_for_branch_locked(&state, branch_id)?;
            if route.identity.project_id != project_id || route.identity.session_id != session_id {
                return Err(GenerationRegistryError::SessionMismatch);
            }
            let family = state
                .families
                .get(&route.identity.request_id)
                .ok_or(GenerationRegistryError::CorruptRegistry)?;
            (route, Arc::clone(&family.cancellation))
        };
        Ok(cancellation.cancel_branch(route.branch_id))
    }

    pub fn complete_family(
        &self,
        request_id: &str,
    ) -> Result<Option<GenerationFamilyIdentity>, GenerationRegistryError> {
        let mut state = self.lock()?;
        let Some(family) = state.families.remove(request_id) else {
            return Ok(None);
        };
        for (run_id, branch_id) in &family.branches {
            state.run_branches.remove(run_id);
            state.branch_requests.remove(branch_id);
        }
        Ok(Some(family.identity))
    }

    pub fn active_branch_count(&self) -> Result<usize, GenerationRegistryError> {
        Ok(self.lock()?.branch_requests.len())
    }

    pub fn has_active_session(
        &self,
        project_id: ProjectId,
        session_id: CommandId,
    ) -> Result<bool, GenerationRegistryError> {
        Ok(self.lock()?.families.values().any(|family| {
            family.identity.project_id == project_id && family.identity.session_id == session_id
        }))
    }

    /// Requests cancellation for every active branch owned by one project
    /// session. Routes remain registered until their workers persist terminal
    /// state and call `complete_family`.
    pub fn cancel_session(
        &self,
        project_id: ProjectId,
        session_id: CommandId,
    ) -> Result<Vec<GenerationRunId>, GenerationRegistryError> {
        let cancellations = {
            let state = self.lock()?;
            state
                .families
                .values()
                .filter(|family| {
                    family.identity.project_id == project_id
                        && family.identity.session_id == session_id
                })
                .flat_map(|family| {
                    family.branches.iter().map(|(run_id, branch_id)| {
                        (*run_id, *branch_id, Arc::clone(&family.cancellation))
                    })
                })
                .collect::<Vec<_>>()
        };
        Ok(cancellations
            .into_iter()
            .filter_map(|(run_id, branch_id, cancellation)| {
                cancellation.cancel_branch(branch_id).then_some(run_id)
            })
            .collect())
    }

    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, GenerationRegistryState>, GenerationRegistryError> {
        self.state
            .lock()
            .map_err(|_| GenerationRegistryError::Poisoned)
    }
}

fn route_for_branch_locked(
    state: &GenerationRegistryState,
    branch_id: BranchId,
) -> Result<ActiveGenerationRoute, GenerationRegistryError> {
    let request_id = state
        .branch_requests
        .get(&branch_id)
        .ok_or(GenerationRegistryError::BranchNotActive(branch_id))?;
    let family = state
        .families
        .get(request_id)
        .ok_or(GenerationRegistryError::CorruptRegistry)?;
    let run_id = family
        .branches
        .iter()
        .find_map(|(run_id, candidate)| (*candidate == branch_id).then_some(*run_id))
        .ok_or(GenerationRegistryError::CorruptRegistry)?;
    Ok(ActiveGenerationRoute {
        identity: family.identity.clone(),
        run_id,
        branch_id,
    })
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GenerationRegistryError {
    #[error(
        "active-generation capacity {capacity} is outside 1..={MAX_ACTIVE_GENERATION_BRANCHES}"
    )]
    InvalidCapacity { capacity: usize },
    #[error("generation request ID must not be empty")]
    EmptyRequestId,
    #[error("generation family must contain at least one branch")]
    EmptyFamily,
    #[error("generation family repeats run {0}")]
    DuplicateRun(GenerationRunId),
    #[error("generation family repeats branch {0}")]
    DuplicateBranch(BranchId),
    #[error("generation request `{0}` is already active")]
    DuplicateRequest(String),
    #[error("generation run {0} is already active")]
    RunAlreadyActive(GenerationRunId),
    #[error("generation branch {0} is already active")]
    BranchAlreadyActive(BranchId),
    #[error("generation run {0} is not active")]
    RunNotActive(GenerationRunId),
    #[error("generation branch {0} is not active")]
    BranchNotActive(BranchId),
    #[error(
        "active-generation capacity exceeded: {active} active + {requested} requested > {limit}"
    )]
    CapacityExceeded {
        active: usize,
        requested: usize,
        limit: usize,
    },
    #[error("generation command belongs to another project session")]
    SessionMismatch,
    #[error("active-generation registry is internally inconsistent")]
    CorruptRegistry,
    #[error("active-generation registry is poisoned")]
    Poisoned,
}

#[derive(Clone, Debug, Default)]
pub struct AgencyGate {
    focus_mode: Arc<AtomicBool>,
    automation_enabled: Arc<AtomicBool>,
}

impl AgencyGate {
    pub fn set_focus_mode(&self, enabled: bool) {
        self.focus_mode.store(enabled, Ordering::Release);
    }

    pub fn set_automation_enabled(&self, enabled: bool) {
        self.automation_enabled.store(enabled, Ordering::Release);
    }

    #[must_use]
    pub fn focus_mode(&self) -> bool {
        self.focus_mode.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn automation_enabled(&self) -> bool {
        self.automation_enabled.load(Ordering::Acquire)
    }

    pub fn admit_manual_generation(&self) -> Result<(), AgencyAdmissionError> {
        if self.focus_mode() {
            return Err(AgencyAdmissionError::FocusMode);
        }
        Ok(())
    }

    pub fn admit_automation(&self) -> Result<(), AgencyAdmissionError> {
        self.admit_manual_generation()?;
        if !self.automation_enabled() {
            return Err(AgencyAdmissionError::AutomationDisabled);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AgencyAdmissionError {
    #[error("focus mode blocks model generation")]
    FocusMode,
    #[error("project automation has not been explicitly enabled")]
    AutomationDisabled,
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub struct JobSender<T> {
    sender: SyncSender<T>,
}

impl<T> Clone for JobSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

impl<T> JobSender<T> {
    pub fn try_submit(&self, job: T) -> Result<(), SubmitError<T>> {
        self.sender.try_send(job).map_err(|error| match error {
            TrySendError::Full(job) => SubmitError::Full(job),
            TrySendError::Disconnected(job) => SubmitError::Disconnected(job),
        })
    }
}

#[derive(Debug)]
pub struct JobReceiver<T> {
    receiver: Receiver<T>,
}

impl<T> JobReceiver<T> {
    pub fn try_receive(&self) -> Result<Option<T>, QueueDisconnected> {
        match self.receiver.try_recv() {
            Ok(job) => Ok(Some(job)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(QueueDisconnected),
        }
    }
}

pub fn bounded_job_queue<T>(
    capacity: usize,
) -> Result<(JobSender<T>, JobReceiver<T>), QueueConfigError> {
    if capacity == 0 || capacity > MAX_QUEUE_CAPACITY {
        return Err(QueueConfigError { capacity });
    }
    let (sender, receiver) = sync_channel(capacity);
    Ok((JobSender { sender }, JobReceiver { receiver }))
}

#[derive(Debug, Error)]
pub enum SubmitError<T> {
    #[error("bounded job queue is full")]
    Full(T),
    #[error("bounded job queue is disconnected")]
    Disconnected(T),
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("queue capacity {capacity} is outside 1..={MAX_QUEUE_CAPACITY}")]
pub struct QueueConfigError {
    capacity: usize,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("bounded job queue is disconnected")]
pub struct QueueDisconnected;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct FakeCancellation {
        branches: Mutex<Vec<BranchId>>,
    }

    impl BranchCancellation for FakeCancellation {
        fn cancel_branch(&self, branch_id: BranchId) -> bool {
            self.branches
                .lock()
                .expect("fake cancellation lock")
                .push(branch_id);
            true
        }
    }

    fn family(
        request_id: &str,
        project_id: ProjectId,
        session_id: CommandId,
        branches: Vec<(GenerationRunId, BranchId)>,
        cancellation: Arc<dyn BranchCancellation>,
    ) -> GenerationFamilyRegistration {
        GenerationFamilyRegistration {
            identity: GenerationFamilyIdentity {
                request_id: request_id.to_string(),
                project_id,
                session_id,
                document_id: DocumentId::new(),
            },
            branches,
            cancellation,
        }
    }

    #[test]
    fn queue_applies_backpressure() {
        let (sender, receiver) = bounded_job_queue(1).expect("queue");
        sender.try_submit(1).expect("first job");
        assert!(matches!(sender.try_submit(2), Err(SubmitError::Full(2))));
        assert_eq!(receiver.try_receive().expect("receive"), Some(1));
    }

    #[test]
    fn cancellation_is_shared() {
        let first = CancellationToken::default();
        let second = first.clone();
        first.cancel();
        assert!(second.is_cancelled());
    }

    #[test]
    fn focus_mode_atomically_blocks_manual_and_automatic_generation() {
        let gate = AgencyGate::default();
        gate.set_automation_enabled(true);
        assert!(gate.admit_manual_generation().is_ok());
        assert!(gate.admit_automation().is_ok());
        gate.set_focus_mode(true);
        assert_eq!(
            gate.admit_manual_generation(),
            Err(AgencyAdmissionError::FocusMode)
        );
        assert_eq!(
            gate.admit_automation(),
            Err(AgencyAdmissionError::FocusMode)
        );
    }

    #[test]
    fn automation_is_opt_in_even_when_manual_generation_is_available() {
        let gate = AgencyGate::default();
        assert!(gate.admit_manual_generation().is_ok());
        assert_eq!(
            gate.admit_automation(),
            Err(AgencyAdmissionError::AutomationDisabled)
        );
    }

    #[test]
    fn generation_registry_routes_cancellation_and_releases_family_atomically() {
        let registry = GenerationRegistry::new(4).expect("registry");
        let project_id = ProjectId::new();
        let session_id = CommandId::new();
        let run_id = GenerationRunId::new();
        let branch_id = BranchId::new();
        let cancellation = Arc::new(FakeCancellation::default());
        registry
            .register(family(
                "request-a",
                project_id,
                session_id,
                vec![(run_id, branch_id)],
                cancellation.clone(),
            ))
            .expect("register family");
        assert!(
            registry
                .has_active_session(project_id, session_id)
                .expect("active session")
        );
        assert!(
            registry
                .cancel_run(project_id, session_id, run_id)
                .expect("cancel run")
        );
        assert_eq!(
            *cancellation.branches.lock().expect("cancelled branches"),
            vec![branch_id]
        );
        let completed = registry
            .complete_family("request-a")
            .expect("complete family")
            .expect("registered identity");
        assert_eq!(completed.project_id, project_id);
        assert_eq!(registry.active_branch_count().expect("active count"), 0);
        assert!(registry.route_for_run(run_id).expect("route").is_none());
    }

    #[test]
    fn generation_registry_rejects_over_capacity_without_partial_routes() {
        let registry = GenerationRegistry::new(1).expect("registry");
        let project_id = ProjectId::new();
        let session_id = CommandId::new();
        let error = registry
            .register(family(
                "too-wide",
                project_id,
                session_id,
                vec![
                    (GenerationRunId::new(), BranchId::new()),
                    (GenerationRunId::new(), BranchId::new()),
                ],
                Arc::new(FakeCancellation::default()),
            ))
            .expect_err("capacity must reject family");
        assert!(matches!(
            error,
            GenerationRegistryError::CapacityExceeded { .. }
        ));
        assert_eq!(registry.active_branch_count().expect("active count"), 0);
    }
}
