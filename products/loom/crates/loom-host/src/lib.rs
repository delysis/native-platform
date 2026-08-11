#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use thiserror::Error;

use loom_types::{
    BlobId, BranchId, CommandId, DocumentId, GenerationRunId, ProjectId, now_unix_ms,
};

pub const MAX_QUEUE_CAPACITY: usize = 65_536;
pub const DEFAULT_MAX_ACTIVE_GENERATION_BRANCHES: usize = 64;
pub const MAX_ACTIVE_GENERATION_BRANCHES: usize = 4_096;
pub const MAX_FOREGROUND_WINDOW_ID_BYTES: usize = 128;
pub const MAX_FOREGROUND_COMMAND_TTL: Duration = Duration::from_secs(60);
pub const MAX_NATIVE_FOCUS_SAMPLE_AGE: Duration = Duration::from_secs(1);
pub const MAX_FOREGROUND_WINDOWS: usize = 64;
pub const MAX_PENDING_FOREGROUND_COMMANDS: usize = 1_024;

/// Native window identity used by one foreground-command challenge.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ForegroundWindowId(Arc<str>);

impl ForegroundWindowId {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ForegroundCommandError> {
        let value = value.as_ref();
        if value.is_empty() || value.len() > MAX_FOREGROUND_WINDOW_ID_BYTES {
            return Err(ForegroundCommandError::InvalidWindowId);
        }
        Ok(Self(Arc::from(value)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact pending promotion identity. This data may cross IPC; it is not
/// authority until the native host atomically consumes the matching nonce.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForegroundCommandBinding {
    pub application_session_id: CommandId,
    pub window_id: ForegroundWindowId,
    pub document_id: DocumentId,
    pub candidate_fingerprint: BlobId,
    pub command_id: CommandId,
    pub promotion_fingerprint: BlobId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForegroundCommandChallenge {
    pub nonce: CommandId,
    pub binding: ForegroundCommandBinding,
    pub focus_epoch: u64,
    pub issued_at_unix_ms: i64,
    pub expires_at_unix_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForegroundCommandAttempt {
    pub nonce: CommandId,
    pub binding: ForegroundCommandBinding,
}

/// Move-only evidence that the native host sampled one window's focus state.
///
/// The fields are private so ordinary callers cannot construct, alter, clone,
/// or deserialize a sample. A sample is also bound to one registry process and
/// expires quickly. The native application edge must obtain it immediately
/// after reading focus from the platform window API.
#[derive(Debug)]
pub struct NativeWindowFocusSample {
    process_session_fingerprint: BlobId,
    window_id: ForegroundWindowId,
    focused: bool,
    sampled_at: Instant,
}

impl NativeWindowFocusSample {
    /// Returns the native window identity captured at the sampling edge.
    pub const fn window_id(&self) -> &ForegroundWindowId {
        &self.window_id
    }
}

/// Move-only proof of the narrow claim the host can actually make:
/// one command was accepted from the focused bound window in this process.
///
/// It intentionally implements neither `Clone` nor serialization. The nonce
/// remains process-local and is consumed before this value is minted.
#[derive(Debug, Eq, PartialEq)]
pub struct VerifiedForegroundCommand {
    process_session_fingerprint: BlobId,
    binding: ForegroundCommandBinding,
    _nonce: CommandId,
    focus_epoch: u64,
    monotonic_event_index: u64,
    issued_at_unix_ms: i64,
    expires_at_unix_ms: i64,
    occurred_at_unix_ms: i64,
}

impl VerifiedForegroundCommand {
    pub const fn process_session_fingerprint(&self) -> BlobId {
        self.process_session_fingerprint
    }

    pub const fn binding(&self) -> &ForegroundCommandBinding {
        &self.binding
    }

    pub const fn focus_epoch(&self) -> u64 {
        self.focus_epoch
    }

    pub const fn monotonic_event_index(&self) -> u64 {
        self.monotonic_event_index
    }

    pub const fn issued_at_unix_ms(&self) -> i64 {
        self.issued_at_unix_ms
    }

    pub const fn expires_at_unix_ms(&self) -> i64 {
        self.expires_at_unix_ms
    }

    pub const fn occurred_at_unix_ms(&self) -> i64 {
        self.occurred_at_unix_ms
    }
}

#[derive(Debug)]
struct PendingForegroundCommand {
    binding: ForegroundCommandBinding,
    focus_epoch: u64,
    issued_at: Instant,
    expires_at: Instant,
    issued_at_unix_ms: i64,
    expires_at_unix_ms: i64,
}

#[derive(Clone, Copy, Debug, Default)]
struct WindowFocus {
    epoch: u64,
    focused: bool,
}

#[derive(Debug, Default)]
struct ForegroundCommandState {
    windows: BTreeMap<ForegroundWindowId, WindowFocus>,
    pending: BTreeMap<CommandId, PendingForegroundCommand>,
    next_event_index: u64,
}

/// Process-local owner of foreground-command challenges.
#[derive(Debug)]
pub struct ForegroundCommandRegistry {
    process_session_fingerprint: BlobId,
    state: Mutex<ForegroundCommandState>,
}

impl Default for ForegroundCommandRegistry {
    fn default() -> Self {
        let identity = CommandId::new();
        let mut material = Vec::with_capacity(64);
        material.extend_from_slice(b"loom/foreground-command-process-session/v1\0");
        material.extend_from_slice(&identity.as_ulid().to_bytes());
        Self {
            process_session_fingerprint: BlobId::digest(&material),
            state: Mutex::new(ForegroundCommandState::default()),
        }
    }
}

impl ForegroundCommandRegistry {
    pub fn observe_window_focus(
        &self,
        window_id: ForegroundWindowId,
        focused: bool,
    ) -> Result<u64, ForegroundCommandError> {
        let mut state = self.lock()?;
        if !state.windows.contains_key(&window_id) && state.windows.len() >= MAX_FOREGROUND_WINDOWS
        {
            return Err(ForegroundCommandError::RegistryCapacityExceeded);
        }
        let window = state.windows.entry(window_id).or_default();
        window.epoch = window
            .epoch
            .checked_add(1)
            .ok_or(ForegroundCommandError::EventIndexExhausted)?;
        window.focused = focused;
        Ok(window.epoch)
    }

    pub fn issue(
        &self,
        binding: ForegroundCommandBinding,
        ttl: Duration,
    ) -> Result<ForegroundCommandChallenge, ForegroundCommandError> {
        self.issue_at(binding, ttl, Instant::now(), now_unix_ms())
    }

    fn issue_at(
        &self,
        binding: ForegroundCommandBinding,
        ttl: Duration,
        now: Instant,
        now_unix_ms: i64,
    ) -> Result<ForegroundCommandChallenge, ForegroundCommandError> {
        if ttl.is_zero() || ttl > MAX_FOREGROUND_COMMAND_TTL || now_unix_ms <= 0 {
            return Err(ForegroundCommandError::InvalidExpiry);
        }
        let expires_at = now
            .checked_add(ttl)
            .ok_or(ForegroundCommandError::InvalidExpiry)?;
        let ttl_ms =
            i64::try_from(ttl.as_millis()).map_err(|_| ForegroundCommandError::InvalidExpiry)?;
        let expires_at_unix_ms = now_unix_ms
            .checked_add(ttl_ms)
            .ok_or(ForegroundCommandError::InvalidExpiry)?;
        let mut state = self.lock()?;
        state.pending.retain(|_, pending| pending.expires_at >= now);
        if state.pending.len() >= MAX_PENDING_FOREGROUND_COMMANDS {
            return Err(ForegroundCommandError::RegistryCapacityExceeded);
        }
        let focus = state
            .windows
            .get(&binding.window_id)
            .copied()
            .filter(|focus| focus.focused)
            .ok_or(ForegroundCommandError::WindowNotFocused)?;
        let nonce = loop {
            let candidate = CommandId::new();
            if !state.pending.contains_key(&candidate) {
                break candidate;
            }
        };
        state.pending.insert(
            nonce,
            PendingForegroundCommand {
                binding: binding.clone(),
                focus_epoch: focus.epoch,
                issued_at: now,
                expires_at,
                issued_at_unix_ms: now_unix_ms,
                expires_at_unix_ms,
            },
        );
        Ok(ForegroundCommandChallenge {
            nonce,
            binding,
            focus_epoch: focus.epoch,
            issued_at_unix_ms: now_unix_ms,
            expires_at_unix_ms,
        })
    }

    /// Samples focus directly from a Tauri native window and binds that value
    /// to this process-local registry and the window's host-owned label.
    #[cfg(feature = "tauri-native-focus")]
    pub fn sample_tauri_window_focus<R: tauri::Runtime>(
        &self,
        window: &tauri::Window<R>,
    ) -> Result<NativeWindowFocusSample, NativeWindowFocusSampleError> {
        let window_id = ForegroundWindowId::new(window.label())?;
        let focused = window.is_focused()?;
        Ok(self.bind_native_window_focus_sample_at(window_id, focused, Instant::now()))
    }

    /// Constructs a synthetic focus sample for cross-crate regression tests.
    /// This method is absent from normal production dependency builds.
    #[cfg(feature = "test-fixtures")]
    #[doc(hidden)]
    pub fn bind_test_native_window_focus_sample(
        &self,
        window_id: ForegroundWindowId,
        focused: bool,
    ) -> NativeWindowFocusSample {
        self.bind_native_window_focus_sample_at(window_id, focused, Instant::now())
    }

    /// Consumes a challenge using a fresh, registry-bound native focus sample.
    /// The registry's event-derived epoch is checked as well; neither signal
    /// substitutes for the other.
    pub fn consume_with_native_focus(
        &self,
        attempt: ForegroundCommandAttempt,
        native_focus: NativeWindowFocusSample,
    ) -> Result<VerifiedForegroundCommand, ForegroundCommandError> {
        self.consume_with_native_focus_at(attempt, native_focus, Instant::now(), now_unix_ms())
    }

    #[cfg(any(test, feature = "tauri-native-focus", feature = "test-fixtures"))]
    fn bind_native_window_focus_sample_at(
        &self,
        window_id: ForegroundWindowId,
        focused: bool,
        sampled_at: Instant,
    ) -> NativeWindowFocusSample {
        NativeWindowFocusSample {
            process_session_fingerprint: self.process_session_fingerprint,
            window_id,
            focused,
            sampled_at,
        }
    }

    fn consume_with_native_focus_at(
        &self,
        attempt: ForegroundCommandAttempt,
        native_focus: NativeWindowFocusSample,
        now: Instant,
        occurred_at_unix_ms: i64,
    ) -> Result<VerifiedForegroundCommand, ForegroundCommandError> {
        let ForegroundCommandAttempt { nonce, binding } = attempt;
        let NativeWindowFocusSample {
            process_session_fingerprint,
            window_id,
            focused,
            sampled_at,
        } = native_focus;
        let mut state = self.lock()?;
        // Removing under the same mutex as validation makes every presented
        // nonce one-shot, including a mismatched or expired attempt.
        let pending = state
            .pending
            .remove(&nonce)
            .ok_or(ForegroundCommandError::StaleNonce)?;
        if now < pending.issued_at
            || now > pending.expires_at
            || occurred_at_unix_ms < pending.issued_at_unix_ms
            || occurred_at_unix_ms > pending.expires_at_unix_ms
        {
            return Err(ForegroundCommandError::Expired);
        }
        validate_foreground_binding(&pending.binding, &binding)?;
        if process_session_fingerprint != self.process_session_fingerprint {
            return Err(ForegroundCommandError::WrongProcess);
        }
        if window_id != pending.binding.window_id {
            return Err(ForegroundCommandError::WrongWindow);
        }
        let sample_age = now
            .checked_duration_since(sampled_at)
            .ok_or(ForegroundCommandError::InvalidNativeFocusSample)?;
        if sampled_at < pending.issued_at || sample_age > MAX_NATIVE_FOCUS_SAMPLE_AGE {
            return Err(ForegroundCommandError::InvalidNativeFocusSample);
        }
        if !focused {
            return Err(ForegroundCommandError::WindowNotFocused);
        }
        let focus = state
            .windows
            .get(&pending.binding.window_id)
            .copied()
            .ok_or(ForegroundCommandError::FocusChanged)?;
        if !focus.focused || focus.epoch != pending.focus_epoch {
            return Err(ForegroundCommandError::FocusChanged);
        }
        let event_index = state
            .next_event_index
            .checked_add(1)
            .ok_or(ForegroundCommandError::EventIndexExhausted)?;
        state.next_event_index = event_index;
        Ok(VerifiedForegroundCommand {
            process_session_fingerprint: self.process_session_fingerprint,
            binding: pending.binding,
            _nonce: nonce,
            focus_epoch: pending.focus_epoch,
            monotonic_event_index: event_index,
            issued_at_unix_ms: pending.issued_at_unix_ms,
            expires_at_unix_ms: pending.expires_at_unix_ms,
            occurred_at_unix_ms,
        })
    }

    #[cfg(test)]
    fn consume_at(
        &self,
        attempt: ForegroundCommandAttempt,
        now: Instant,
        occurred_at_unix_ms: i64,
    ) -> Result<VerifiedForegroundCommand, ForegroundCommandError> {
        let native_focus =
            self.bind_native_window_focus_sample_at(attempt.binding.window_id.clone(), true, now);
        self.consume_with_native_focus_at(attempt, native_focus, now, occurred_at_unix_ms)
    }

    pub fn revoke_application_session(
        &self,
        session_id: CommandId,
    ) -> Result<usize, ForegroundCommandError> {
        let mut state = self.lock()?;
        let before = state.pending.len();
        state
            .pending
            .retain(|_, pending| pending.binding.application_session_id != session_id);
        Ok(before.saturating_sub(state.pending.len()))
    }

    pub fn revoke_all(&self) -> Result<usize, ForegroundCommandError> {
        let mut state = self.lock()?;
        let revoked = state.pending.len();
        state.pending.clear();
        Ok(revoked)
    }

    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, ForegroundCommandState>, ForegroundCommandError> {
        self.state
            .lock()
            .map_err(|_| ForegroundCommandError::StateUnavailable)
    }
}

fn validate_foreground_binding(
    expected: &ForegroundCommandBinding,
    actual: &ForegroundCommandBinding,
) -> Result<(), ForegroundCommandError> {
    if actual.window_id != expected.window_id {
        return Err(ForegroundCommandError::WrongWindow);
    }
    if actual.application_session_id != expected.application_session_id {
        return Err(ForegroundCommandError::WrongSession);
    }
    if actual.document_id != expected.document_id {
        return Err(ForegroundCommandError::WrongDocument);
    }
    if actual.candidate_fingerprint != expected.candidate_fingerprint {
        return Err(ForegroundCommandError::WrongCandidate);
    }
    if actual.command_id != expected.command_id {
        return Err(ForegroundCommandError::WrongCommand);
    }
    if actual.promotion_fingerprint != expected.promotion_fingerprint {
        return Err(ForegroundCommandError::WrongPromotion);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ForegroundCommandError {
    #[error("foreground window identity is empty or too large")]
    InvalidWindowId,
    #[error("foreground command expiry is outside the bounded domain")]
    InvalidExpiry,
    #[error("foreground command registry is unavailable")]
    StateUnavailable,
    #[error("foreground command registry capacity is exhausted")]
    RegistryCapacityExceeded,
    #[error("the bound window is not focused")]
    WindowNotFocused,
    #[error("the foreground command nonce is stale, unknown, or already consumed")]
    StaleNonce,
    #[error("the foreground command nonce expired")]
    Expired,
    #[error("the foreground command names another window")]
    WrongWindow,
    #[error("the foreground command names another application session")]
    WrongSession,
    #[error("the foreground command names another document")]
    WrongDocument,
    #[error("the foreground command names another candidate")]
    WrongCandidate,
    #[error("the foreground command names another command occurrence")]
    WrongCommand,
    #[error("the foreground command names another pending promotion")]
    WrongPromotion,
    #[error("the native focus sample belongs to another process registry")]
    WrongProcess,
    #[error("the native focus sample is stale or predates the challenge")]
    InvalidNativeFocusSample,
    #[error("the window focus epoch changed before command consumption")]
    FocusChanged,
    #[error("the foreground command event index is exhausted")]
    EventIndexExhausted,
}

#[cfg(feature = "tauri-native-focus")]
#[derive(Debug, Error)]
pub enum NativeWindowFocusSampleError {
    #[error(transparent)]
    InvalidWindow(#[from] ForegroundCommandError),
    #[error("could not verify native window focus: {0}")]
    NativeQuery(#[from] tauri::Error),
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationPersistenceFailure {
    pub identity: GenerationFamilyIdentity,
    pub runs: Vec<(GenerationRunId, BranchId)>,
    pub error: String,
}

#[derive(Debug)]
struct ActiveFamily {
    identity: GenerationFamilyIdentity,
    branches: Vec<(GenerationRunId, BranchId)>,
    cancellation: Option<Arc<dyn BranchCancellation>>,
    pending_cancellations: BTreeSet<BranchId>,
    terminal_persistence_error: Option<String>,
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
    session_idle: Condvar,
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
            session_idle: Condvar::new(),
        })
    }

    pub fn register(
        &self,
        registration: GenerationFamilyRegistration,
    ) -> Result<(), GenerationRegistryError> {
        self.insert_family(
            registration.identity,
            registration.branches,
            Some(registration.cancellation),
        )
    }

    /// Reserves every route before a backend handle exists.
    ///
    /// Cancellation delivered after this returns is retained in the registry
    /// and replayed when `attach_cancellation` installs the concrete handle.
    /// This closes the otherwise-unavoidable admission-to-registration race.
    pub fn reserve(
        &self,
        identity: GenerationFamilyIdentity,
        branches: Vec<(GenerationRunId, BranchId)>,
    ) -> Result<(), GenerationRegistryError> {
        self.insert_family(identity, branches, None)
    }

    pub fn attach_cancellation(
        &self,
        request_id: &str,
        cancellation: Arc<dyn BranchCancellation>,
    ) -> Result<Vec<GenerationRunId>, GenerationRegistryError> {
        let (cancellation, pending) = {
            let mut state = self.lock()?;
            let family = state
                .families
                .get_mut(request_id)
                .ok_or_else(|| GenerationRegistryError::RequestNotActive(request_id.to_owned()))?;
            if family.cancellation.is_some() {
                return Err(GenerationRegistryError::CancellationAlreadyAttached(
                    request_id.to_owned(),
                ));
            }
            family.cancellation = Some(cancellation);
            let cancellation = family
                .cancellation
                .as_ref()
                .map(Arc::clone)
                .ok_or(GenerationRegistryError::CorruptRegistry)?;
            let pending = std::mem::take(&mut family.pending_cancellations);
            let pending = family
                .branches
                .iter()
                .filter_map(|(run_id, branch_id)| {
                    pending.contains(branch_id).then_some((*run_id, *branch_id))
                })
                .collect::<Vec<_>>();
            (cancellation, pending)
        };
        Ok(pending
            .into_iter()
            .filter_map(|(run_id, branch_id)| {
                cancellation.cancel_branch(branch_id).then_some(run_id)
            })
            .collect())
    }

    fn insert_family(
        &self,
        identity: GenerationFamilyIdentity,
        branches: Vec<(GenerationRunId, BranchId)>,
        cancellation: Option<Arc<dyn BranchCancellation>>,
    ) -> Result<(), GenerationRegistryError> {
        if identity.request_id.trim().is_empty() {
            return Err(GenerationRegistryError::EmptyRequestId);
        }
        if branches.is_empty() {
            return Err(GenerationRegistryError::EmptyFamily);
        }
        let mut runs = BTreeSet::new();
        let mut unique_branches = BTreeSet::new();
        for (run_id, branch_id) in &branches {
            if !runs.insert(*run_id) {
                return Err(GenerationRegistryError::DuplicateRun(*run_id));
            }
            if !unique_branches.insert(*branch_id) {
                return Err(GenerationRegistryError::DuplicateBranch(*branch_id));
            }
        }

        let mut state = self.lock()?;
        if state.families.contains_key(&identity.request_id) {
            return Err(GenerationRegistryError::DuplicateRequest(
                identity.request_id,
            ));
        }
        let requested_total = state
            .branch_requests
            .len()
            .checked_add(branches.len())
            .ok_or(GenerationRegistryError::CapacityExceeded {
                active: state.branch_requests.len(),
                requested: branches.len(),
                limit: self.max_active_branches,
            })?;
        if requested_total > self.max_active_branches {
            return Err(GenerationRegistryError::CapacityExceeded {
                active: state.branch_requests.len(),
                requested: branches.len(),
                limit: self.max_active_branches,
            });
        }
        for (run_id, branch_id) in &branches {
            if state.run_branches.contains_key(run_id) {
                return Err(GenerationRegistryError::RunAlreadyActive(*run_id));
            }
            if state.branch_requests.contains_key(branch_id) {
                return Err(GenerationRegistryError::BranchAlreadyActive(*branch_id));
            }
        }

        let request_id = identity.request_id.clone();
        for (run_id, branch_id) in &branches {
            state.run_branches.insert(*run_id, *branch_id);
            state.branch_requests.insert(*branch_id, request_id.clone());
        }
        state.families.insert(
            request_id,
            ActiveFamily {
                identity,
                branches,
                cancellation,
                pending_cancellations: BTreeSet::new(),
                terminal_persistence_error: None,
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
            let mut state = self.lock()?;
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
                .get_mut(&route.identity.request_id)
                .ok_or(GenerationRegistryError::CorruptRegistry)?;
            let cancellation = family.cancellation.as_ref().map(Arc::clone);
            if cancellation.is_none() {
                family.pending_cancellations.insert(route.branch_id);
            }
            (route, cancellation)
        };
        Ok(cancellation.is_none_or(|cancellation| cancellation.cancel_branch(route.branch_id)))
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
        let identity = family.identity;
        drop(state);
        self.session_idle.notify_all();
        Ok(Some(identity))
    }

    pub fn active_branch_count(&self) -> Result<usize, GenerationRegistryError> {
        Ok(self.lock()?.branch_requests.len())
    }

    pub fn mark_terminal_persistence_failure(
        &self,
        request_id: &str,
        error: impl Into<String>,
    ) -> Result<(), GenerationRegistryError> {
        let mut state = self.lock()?;
        let family = state
            .families
            .get_mut(request_id)
            .ok_or_else(|| GenerationRegistryError::RequestNotActive(request_id.to_owned()))?;
        family.terminal_persistence_error = Some(error.into());
        drop(state);
        self.session_idle.notify_all();
        Ok(())
    }

    pub fn terminal_persistence_failures(
        &self,
        project_id: ProjectId,
        session_id: CommandId,
    ) -> Result<Vec<GenerationPersistenceFailure>, GenerationRegistryError> {
        Ok(self
            .lock()?
            .families
            .values()
            .filter(|family| {
                family.identity.project_id == project_id && family.identity.session_id == session_id
            })
            .filter_map(|family| {
                family.terminal_persistence_error.as_ref().map(|error| {
                    GenerationPersistenceFailure {
                        identity: family.identity.clone(),
                        runs: family.branches.clone(),
                        error: error.clone(),
                    }
                })
            })
            .collect())
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
        let (pending, cancellations) = {
            let mut state = self.lock()?;
            let mut pending = Vec::new();
            let mut cancellations = Vec::new();
            for family in state.families.values_mut().filter(|family| {
                family.identity.project_id == project_id && family.identity.session_id == session_id
            }) {
                for (run_id, branch_id) in &family.branches {
                    if let Some(cancellation) = &family.cancellation {
                        cancellations.push((*run_id, *branch_id, Arc::clone(cancellation)));
                    } else {
                        family.pending_cancellations.insert(*branch_id);
                        pending.push(*run_id);
                    }
                }
            }
            (pending, cancellations)
        };
        Ok(pending
            .into_iter()
            .chain(
                cancellations
                    .into_iter()
                    .filter_map(|(run_id, branch_id, cancellation)| {
                        cancellation.cancel_branch(branch_id).then_some(run_id)
                    }),
            )
            .collect())
    }

    /// Requests cancellation for every process-local generation branch.
    ///
    /// This is reserved for application-wide teardown, where no project
    /// session may remain authoritative. Routes stay registered until their
    /// owning workers persist terminal state and call `complete_family`.
    pub fn cancel_all(&self) -> Result<Vec<GenerationRunId>, GenerationRegistryError> {
        let (pending, cancellations) = {
            let mut state = self.lock()?;
            let mut pending = Vec::new();
            let mut cancellations = Vec::new();
            for family in state.families.values_mut() {
                for (run_id, branch_id) in &family.branches {
                    if let Some(cancellation) = &family.cancellation {
                        cancellations.push((*run_id, *branch_id, Arc::clone(cancellation)));
                    } else {
                        family.pending_cancellations.insert(*branch_id);
                        pending.push(*run_id);
                    }
                }
            }
            (pending, cancellations)
        };
        Ok(pending
            .into_iter()
            .chain(
                cancellations
                    .into_iter()
                    .filter_map(|(run_id, branch_id, cancellation)| {
                        cancellation.cancel_branch(branch_id).then_some(run_id)
                    }),
            )
            .collect())
    }

    /// Waits for workers to persist terminal state and release every route for
    /// one session. The timeout is a hard upper bound; callers may retry the
    /// same idempotent command without guessing whether close committed.
    pub fn wait_for_session_idle(
        &self,
        project_id: ProjectId,
        session_id: CommandId,
        timeout: Duration,
    ) -> Result<bool, GenerationRegistryError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let mut state = self.lock()?;
        loop {
            let mut active = false;
            let mut persistence_failed = false;
            for family in state.families.values().filter(|family| {
                family.identity.project_id == project_id && family.identity.session_id == session_id
            }) {
                active = true;
                persistence_failed |= family.terminal_persistence_error.is_some();
            }
            if !active {
                return Ok(true);
            }
            // The worker has stopped and asked the close path to retry its
            // durable terminal write. Waiting longer cannot change that
            // condition, so return control to the repair path immediately.
            if persistence_failed {
                return Ok(false);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let (next, wait) = self
                .session_idle
                .wait_timeout(state, remaining)
                .map_err(|_| GenerationRegistryError::Poisoned)?;
            state = next;
            if wait.timed_out()
                && state.families.values().any(|family| {
                    family.identity.project_id == project_id
                        && family.identity.session_id == session_id
                })
            {
                return Ok(false);
            }
        }
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
    #[error("generation request `{0}` is not active")]
    RequestNotActive(String),
    #[error("generation request `{0}` already has cancellation authority")]
    CancellationAlreadyAttached(String),
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

    fn foreground_binding() -> ForegroundCommandBinding {
        ForegroundCommandBinding {
            application_session_id: CommandId::new(),
            window_id: ForegroundWindowId::new("main").expect("window ID"),
            document_id: DocumentId::new(),
            candidate_fingerprint: BlobId::digest(b"candidate"),
            command_id: CommandId::new(),
            promotion_fingerprint: loom_types::BlobId::digest(b"promotion"),
        }
    }

    fn focused_registry(
        binding: &ForegroundCommandBinding,
    ) -> (ForegroundCommandRegistry, Instant) {
        let registry = ForegroundCommandRegistry::default();
        registry
            .observe_window_focus(binding.window_id.clone(), true)
            .expect("focus main window");
        (registry, Instant::now())
    }

    #[test]
    fn stale_nonce_rejected() {
        let binding = foreground_binding();
        let (registry, now) = focused_registry(&binding);
        let attempt = ForegroundCommandAttempt {
            nonce: CommandId::new(),
            binding,
        };
        assert_eq!(
            registry.consume_at(attempt, now, 10),
            Err(ForegroundCommandError::StaleNonce)
        );
    }

    #[test]
    fn wrong_window_rejected() {
        let binding = foreground_binding();
        let (registry, now) = focused_registry(&binding);
        let challenge = registry
            .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
            .expect("issue challenge");
        let mut wrong = binding;
        wrong.window_id = ForegroundWindowId::new("other").expect("other window");
        assert_eq!(
            registry.consume_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding: wrong,
                },
                now,
                11,
            ),
            Err(ForegroundCommandError::WrongWindow)
        );
    }

    #[test]
    fn focus_epoch_change_rejected() {
        let binding = foreground_binding();
        let (registry, now) = focused_registry(&binding);
        let challenge = registry
            .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
            .expect("issue challenge");
        registry
            .observe_window_focus(binding.window_id.clone(), false)
            .expect("blur window");
        registry
            .observe_window_focus(binding.window_id.clone(), true)
            .expect("refocus window");
        assert_eq!(
            registry.consume_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding,
                },
                now,
                11,
            ),
            Err(ForegroundCommandError::FocusChanged)
        );
    }

    #[test]
    fn wrong_candidate_rejected() {
        let binding = foreground_binding();
        let (registry, now) = focused_registry(&binding);
        let challenge = registry
            .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
            .expect("issue challenge");
        let mut wrong = binding;
        wrong.candidate_fingerprint = BlobId::digest(b"other candidate");
        assert_eq!(
            registry.consume_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding: wrong,
                },
                now,
                11,
            ),
            Err(ForegroundCommandError::WrongCandidate)
        );
    }

    #[test]
    fn session_document_command_and_promotion_substitution_are_rejected() {
        let mutations: [fn(&mut ForegroundCommandBinding); 4] = [
            |binding| binding.application_session_id = CommandId::new(),
            |binding| binding.document_id = DocumentId::new(),
            |binding| binding.command_id = CommandId::new(),
            |binding| binding.promotion_fingerprint = BlobId::digest(b"other promotion"),
        ];
        let expected = [
            ForegroundCommandError::WrongSession,
            ForegroundCommandError::WrongDocument,
            ForegroundCommandError::WrongCommand,
            ForegroundCommandError::WrongPromotion,
        ];
        for (mutate, expected) in mutations.into_iter().zip(expected) {
            let binding = foreground_binding();
            let (registry, now) = focused_registry(&binding);
            let challenge = registry
                .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
                .expect("issue challenge");
            let mut wrong = binding;
            mutate(&mut wrong);
            assert_eq!(
                registry.consume_at(
                    ForegroundCommandAttempt {
                        nonce: challenge.nonce,
                        binding: wrong,
                    },
                    now,
                    11,
                ),
                Err(expected)
            );
        }
    }

    #[test]
    fn expired_nonce_rejected() {
        let binding = foreground_binding();
        let (registry, now) = focused_registry(&binding);
        let challenge = registry
            .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
            .expect("issue challenge");
        assert_eq!(
            registry.consume_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding,
                },
                now + Duration::from_secs(6),
                16,
            ),
            Err(ForegroundCommandError::Expired)
        );
    }

    #[test]
    fn second_use_rejected() {
        let binding = foreground_binding();
        let (registry, now) = focused_registry(&binding);
        let challenge = registry
            .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
            .expect("issue challenge");
        let first = ForegroundCommandAttempt {
            nonce: challenge.nonce,
            binding: binding.clone(),
        };
        registry
            .consume_at(first, now, 11)
            .expect("first use succeeds");
        assert_eq!(
            registry.consume_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding,
                },
                now,
                12,
            ),
            Err(ForegroundCommandError::StaleNonce)
        );
    }

    #[test]
    fn native_focus_recheck_fails_closed_and_spends_nonce() {
        let binding = foreground_binding();
        let (registry, now) = focused_registry(&binding);
        let challenge = registry
            .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
            .expect("issue challenge");
        let unfocused =
            registry.bind_native_window_focus_sample_at(binding.window_id.clone(), false, now);
        assert_eq!(
            registry.consume_with_native_focus_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding: binding.clone(),
                },
                unfocused,
                now,
                11,
            ),
            Err(ForegroundCommandError::WindowNotFocused)
        );
        let focused =
            registry.bind_native_window_focus_sample_at(binding.window_id.clone(), true, now);
        assert_eq!(
            registry.consume_with_native_focus_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding,
                },
                focused,
                now,
                12,
            ),
            Err(ForegroundCommandError::StaleNonce)
        );
    }

    #[test]
    fn native_focus_sample_is_bound_to_registry_and_spends_nonce() {
        let binding = foreground_binding();
        let (registry, now) = focused_registry(&binding);
        let challenge = registry
            .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
            .expect("issue challenge");
        let other_registry = ForegroundCommandRegistry::default();
        let foreign_sample =
            other_registry.bind_native_window_focus_sample_at(binding.window_id.clone(), true, now);
        assert_eq!(
            registry.consume_with_native_focus_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding: binding.clone(),
                },
                foreign_sample,
                now,
                11,
            ),
            Err(ForegroundCommandError::WrongProcess)
        );
        assert_eq!(
            registry.consume_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding,
                },
                now,
                12,
            ),
            Err(ForegroundCommandError::StaleNonce)
        );
    }

    #[test]
    fn stale_native_focus_sample_fails_closed_and_spends_nonce() {
        let binding = foreground_binding();
        let (registry, now) = focused_registry(&binding);
        let challenge = registry
            .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
            .expect("issue challenge");
        let sample =
            registry.bind_native_window_focus_sample_at(binding.window_id.clone(), true, now);
        let late = now + MAX_NATIVE_FOCUS_SAMPLE_AGE + Duration::from_millis(1);
        assert_eq!(
            registry.consume_with_native_focus_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding: binding.clone(),
                },
                sample,
                late,
                12,
            ),
            Err(ForegroundCommandError::InvalidNativeFocusSample)
        );
        assert_eq!(
            registry.consume_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding,
                },
                late,
                13,
            ),
            Err(ForegroundCommandError::StaleNonce)
        );
    }

    #[test]
    fn foreground_registry_bounds_windows_and_pending_challenges() {
        let registry = ForegroundCommandRegistry::default();
        for index in 0..MAX_FOREGROUND_WINDOWS {
            registry
                .observe_window_focus(
                    ForegroundWindowId::new(format!("window-{index}")).expect("window ID"),
                    true,
                )
                .expect("bounded window");
        }
        assert_eq!(
            registry.observe_window_focus(
                ForegroundWindowId::new("one-window-too-many").expect("window ID"),
                true,
            ),
            Err(ForegroundCommandError::RegistryCapacityExceeded)
        );

        let binding = ForegroundCommandBinding {
            window_id: ForegroundWindowId::new("window-0").expect("window ID"),
            ..foreground_binding()
        };
        let now = Instant::now();
        for _ in 0..MAX_PENDING_FOREGROUND_COMMANDS {
            registry
                .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
                .expect("bounded challenge");
        }
        assert_eq!(
            registry.issue_at(binding, Duration::from_secs(5), now, 10),
            Err(ForegroundCommandError::RegistryCapacityExceeded)
        );
    }

    #[test]
    fn restart_rejected() {
        let binding = foreground_binding();
        let (first_registry, now) = focused_registry(&binding);
        let challenge = first_registry
            .issue_at(binding.clone(), Duration::from_secs(5), now, 10)
            .expect("issue challenge");
        let second_registry = ForegroundCommandRegistry::default();
        second_registry
            .observe_window_focus(binding.window_id.clone(), true)
            .expect("focus after restart");
        assert_eq!(
            second_registry.consume_at(
                ForegroundCommandAttempt {
                    nonce: challenge.nonce,
                    binding,
                },
                now,
                11,
            ),
            Err(ForegroundCommandError::StaleNonce)
        );
    }

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
    fn application_teardown_cancels_every_registered_family() {
        let registry = GenerationRegistry::new(4).expect("registry");
        let first_run = GenerationRunId::new();
        let first_branch = BranchId::new();
        let second_run = GenerationRunId::new();
        let second_branch = BranchId::new();
        let first = Arc::new(FakeCancellation::default());
        let second = Arc::new(FakeCancellation::default());
        registry
            .register(family(
                "first",
                ProjectId::new(),
                CommandId::new(),
                vec![(first_run, first_branch)],
                first.clone(),
            ))
            .expect("first family");
        registry
            .register(family(
                "second",
                ProjectId::new(),
                CommandId::new(),
                vec![(second_run, second_branch)],
                second.clone(),
            ))
            .expect("second family");

        let mut cancelled = registry.cancel_all().expect("cancel every family");
        cancelled.sort_unstable();
        let mut expected = vec![first_run, second_run];
        expected.sort_unstable();
        assert_eq!(cancelled, expected);
        assert_eq!(
            *first.branches.lock().expect("first cancellation"),
            vec![first_branch]
        );
        assert_eq!(
            *second.branches.lock().expect("second cancellation"),
            vec![second_branch]
        );
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

    #[test]
    fn cancellation_before_backend_attachment_is_replayed_exactly_once() {
        for index in 0..64 {
            let registry = Arc::new(GenerationRegistry::new(1).expect("registry"));
            let project_id = ProjectId::new();
            let session_id = CommandId::new();
            let run_id = GenerationRunId::new();
            let branch_id = BranchId::new();
            let request_id = format!("reserved-{index}");
            registry
                .reserve(
                    GenerationFamilyIdentity {
                        request_id: request_id.clone(),
                        project_id,
                        session_id,
                        document_id: DocumentId::new(),
                    },
                    vec![(run_id, branch_id)],
                )
                .expect("reserve family before native startup");

            let cancellation = Arc::new(FakeCancellation::default());
            let authority: Arc<dyn BranchCancellation> = cancellation.clone();
            let barrier = Arc::new(std::sync::Barrier::new(3));
            let cancel_thread = {
                let registry = Arc::clone(&registry);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    registry
                        .cancel_session(project_id, session_id)
                        .expect("cancel reserved family")
                })
            };
            let attach_thread = {
                let registry = Arc::clone(&registry);
                let barrier = Arc::clone(&barrier);
                let request_id = request_id.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    registry
                        .attach_cancellation(&request_id, authority)
                        .expect("attach native cancellation authority")
                })
            };
            barrier.wait();
            let cancelled = cancel_thread.join().expect("cancel thread");
            let replayed = attach_thread.join().expect("attach thread");

            assert_eq!(cancelled, vec![run_id]);
            assert!(replayed.is_empty() || replayed == vec![run_id]);
            assert_eq!(
                *cancellation.branches.lock().expect("cancelled branches"),
                vec![branch_id]
            );
            registry
                .complete_family(&request_id)
                .expect("complete family");
        }
    }

    #[test]
    fn session_idle_wait_is_bounded_and_observes_release() {
        let registry = GenerationRegistry::new(1).expect("registry");
        let project_id = ProjectId::new();
        let session_id = CommandId::new();
        registry
            .reserve(
                GenerationFamilyIdentity {
                    request_id: "waiting".to_owned(),
                    project_id,
                    session_id,
                    document_id: DocumentId::new(),
                },
                vec![(GenerationRunId::new(), BranchId::new())],
            )
            .expect("reserve family");

        assert!(
            !registry
                .wait_for_session_idle(project_id, session_id, Duration::ZERO)
                .expect("bounded busy result")
        );
        registry
            .complete_family("waiting")
            .expect("complete family");
        assert!(
            registry
                .wait_for_session_idle(project_id, session_id, Duration::ZERO)
                .expect("idle result")
        );
    }

    #[test]
    fn session_idle_wait_wakes_for_terminal_persistence_repair() {
        let registry = GenerationRegistry::new(1).expect("registry");
        let project_id = ProjectId::new();
        let session_id = CommandId::new();
        let request_id = "persistence-failure";
        registry
            .reserve(
                GenerationFamilyIdentity {
                    request_id: request_id.to_owned(),
                    project_id,
                    session_id,
                    document_id: DocumentId::new(),
                },
                vec![(GenerationRunId::new(), BranchId::new())],
            )
            .expect("reserve family");
        registry
            .mark_terminal_persistence_failure(request_id, "disk unavailable")
            .expect("record persistence failure");

        let started = Instant::now();
        assert!(
            !registry
                .wait_for_session_idle(project_id, session_id, Duration::from_secs(1))
                .expect("wait result")
        );
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
