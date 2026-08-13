use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::{BoundedVec, TrialCaseId, TrialRunOrigin, TrialRunRecord};
use loom_store::{
    CampaignJournalEventPersistence, CampaignJournalMutation, CampaignTrialOutcome,
    ExclusiveResearchSessionLease, PersistedResearchJournalEvent, PersistedResearchSubjectSnapshot,
    ProjectStore, ResearchBudgetMaximum, ResearchJournalBudget, ResearchJournalWriter,
    ResearchSessionKind, SearchDecisionPersistenceKind,
};
use loom_types::{ArtifactId, BlobId, ProjectId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ArchiveDecisionKind, ArchiveError, ArchiveUpdate, BlockedFactorialPlan, CampaignBudgetAmount,
    CampaignBudgetError, CampaignBudgetLedger, CampaignSpecError, DispatchedTrialPermit,
    EvaluationLeasePurpose, FactorialError, FrozenCampaignSpec, HalvingError,
    MAX_DECISION_TRIAL_REFERENCES, MapElitesArchive, NestedPoolError, NestedPoolPlan,
    PressureAction, PressureCurveDecision, PressureError, ReservedTrialPermit, SchedulerSessionId,
    SearchDecisionEffect, SearchDecisionError, SearchDecisionReceipt, SuccessiveHalvingDecision,
    TrialAttemptId, VerifiedEvaluatedCandidateLease, VerifiedNestedPoolLease,
    VerifiedPressureCurveLease, VerifiedSchedulerTerminalLease, VerifiedTrialTerminalLease,
};

pub const MAX_CAMPAIGN_EVENTS: usize = 262_144;
pub const MAX_CAMPAIGN_ATTEMPTS: usize = 65_536;
pub const MAX_CAMPAIGN_DECISIONS: usize = 65_536;

const EVENT_DOMAIN: &[u8] = b"loom/campaign-event/v1\0";
const CAMPAIGN_EVENT_RECORD_FORMAT: &str = "loom.campaign-event.v1";
const CAMPAIGN_TRIAL_ATTEMPT_RECORD_FORMAT: &str = "loom.campaign-trial-attempt.v1";
const CAMPAIGN_TRIAL_RESERVATION_RECORD_FORMAT: &str = "loom.campaign-trial-budget-reservation.v1";
const CAMPAIGN_TRIAL_CHARGE_RECORD_FORMAT: &str = "loom.campaign-trial-budget-charge.v1";
const SEARCH_DECISION_RECORD_FORMAT: &str = "loom.campaign-search-decision-record.v1";
const RECOVERY_DIAGNOSTIC_DOMAIN: &[u8] = b"loom/campaign-crash-recovery/v1\0";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CampaignTrialTerminal {
    Completed { trial_journal_fingerprint: BlobId },
    Failed { diagnostic_fingerprint: BlobId },
    Cancelled { diagnostic_fingerprint: BlobId },
    Interrupted { diagnostic_fingerprint: BlobId },
}

impl CampaignTrialTerminal {
    const fn tag(self) -> u8 {
        match self {
            Self::Completed { .. } => 0,
            Self::Failed { .. } => 1,
            Self::Cancelled { .. } => 2,
            Self::Interrupted { .. } => 3,
        }
    }

    const fn evidence(self) -> BlobId {
        match self {
            Self::Completed {
                trial_journal_fingerprint,
            } => trial_journal_fingerprint,
            Self::Failed {
                diagnostic_fingerprint,
            }
            | Self::Cancelled {
                diagnostic_fingerprint,
            }
            | Self::Interrupted {
                diagnostic_fingerprint,
            } => diagnostic_fingerprint,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignDisposition {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum CampaignEventKind {
    Prepared {
        campaign_fingerprint: BlobId,
        store_lease_fingerprint: BlobId,
    },
    Started,
    PauseRequested {
        reason_fingerprint: BlobId,
    },
    Paused,
    Resumed,
    CancelRequested {
        reason_fingerprint: BlobId,
    },
    TrialReserved {
        attempt_id: TrialAttemptId,
        trial_fingerprint: BlobId,
        attempt_ordinal: u16,
        reservation: CampaignBudgetAmount,
    },
    TrialDispatched {
        attempt_id: TrialAttemptId,
    },
    TrialFinished {
        attempt_id: TrialAttemptId,
        terminal: CampaignTrialTerminal,
        actual_charge: CampaignBudgetAmount,
        live_terminal_evidence_fingerprint: BlobId,
    },
    TrialReservationReleased {
        attempt_id: TrialAttemptId,
        diagnostic_fingerprint: BlobId,
    },
    SearchDecisionRecorded {
        receipt: Box<SearchDecisionReceipt>,
    },
    CampaignClosed {
        disposition: CampaignDisposition,
        diagnostic_fingerprint: Option<BlobId>,
        scheduler_evidence_fingerprint: Option<BlobId>,
    },
}

/// One immutable claim in the campaign event chain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignEvent {
    sequence: u32,
    previous_event_fingerprint: Option<BlobId>,
    kind: CampaignEventKind,
    fingerprint: BlobId,
}

/// Allocation-bounded wire container for persisted event streams.
///
/// Decoding a raw `Vec<CampaignEvent>` is deliberately not the supported
/// persistence boundary because its allocation would precede replay limits.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CampaignEventBatch(BoundedVec<CampaignEvent, MAX_CAMPAIGN_EVENTS>);

impl CampaignEventBatch {
    pub fn new(events: Vec<CampaignEvent>) -> Result<Self, CampaignError> {
        BoundedVec::new(events)
            .map(Self)
            .map_err(|_| CampaignError::EventLimit)
    }

    pub fn events(&self) -> &[CampaignEvent] {
        &self.0
    }

    pub fn into_events(self) -> Vec<CampaignEvent> {
        self.0.into_inner()
    }
}

impl CampaignEvent {
    fn new(
        sequence: u32,
        previous_event_fingerprint: Option<BlobId>,
        kind: CampaignEventKind,
    ) -> Self {
        let fingerprint = fingerprint_event(sequence, previous_event_fingerprint, &kind);
        Self {
            sequence,
            previous_event_fingerprint,
            kind,
            fingerprint,
        }
    }

    pub const fn sequence(&self) -> u32 {
        self.sequence
    }

    pub const fn previous_event_fingerprint(&self) -> Option<BlobId> {
        self.previous_event_fingerprint
    }

    pub const fn kind(&self) -> &CampaignEventKind {
        &self.kind
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    Prepared,
    Running,
    PauseRequested,
    Paused,
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
}

impl CampaignStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignTrialAttemptStatus {
    Reserved,
    Dispatched,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialSchedulingStatus {
    Unscheduled,
    Runnable,
    WaitingForDependencies,
    Active,
    Completed,
    Eliminated,
    PressureStopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CampaignSnapshot {
    status: CampaignStatus,
    budget: CampaignBudgetLedger,
    attempt_count: usize,
    active_attempt_count: usize,
    successful_trial_count: usize,
    decision_count: usize,
    scheduled_trial_count: usize,
    eliminated_trial_count: usize,
    stopped_trial_count: usize,
    archive_generation: Option<u64>,
    last_event_fingerprint: BlobId,
}

impl CampaignSnapshot {
    pub const fn status(self) -> CampaignStatus {
        self.status
    }

    pub const fn budget(self) -> CampaignBudgetLedger {
        self.budget
    }

    pub const fn attempt_count(self) -> usize {
        self.attempt_count
    }

    pub const fn active_attempt_count(self) -> usize {
        self.active_attempt_count
    }

    pub const fn successful_trial_count(self) -> usize {
        self.successful_trial_count
    }

    pub const fn decision_count(self) -> usize {
        self.decision_count
    }

    pub const fn scheduled_trial_count(self) -> usize {
        self.scheduled_trial_count
    }

    pub const fn eliminated_trial_count(self) -> usize {
        self.eliminated_trial_count
    }

    pub const fn stopped_trial_count(self) -> usize {
        self.stopped_trial_count
    }

    pub const fn archive_generation(self) -> Option<u64> {
        self.archive_generation
    }

    pub const fn last_event_fingerprint(self) -> BlobId {
        self.last_event_fingerprint
    }
}

/// Strict replay output. It intentionally has no method that emits a trial
/// command, inference request, store write, or manuscript authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CheckedCampaignReplay {
    campaign_fingerprint: BlobId,
    snapshot: CampaignSnapshot,
    event_count: usize,
}

impl CheckedCampaignReplay {
    pub const fn campaign_fingerprint(self) -> BlobId {
        self.campaign_fingerprint
    }

    pub const fn snapshot(self) -> CampaignSnapshot {
        self.snapshot
    }

    pub const fn event_count(self) -> usize {
        self.event_count
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AttemptState {
    Reserved,
    Dispatched,
    Finished(CampaignTrialTerminal),
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AttemptRecord {
    trial_fingerprint: BlobId,
    reservation: CampaignBudgetAmount,
    reservation_event_fingerprint: BlobId,
    dispatch_event_fingerprint: Option<BlobId>,
    state: AttemptState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HalvingProgress {
    rung: u16,
    decision_fingerprint: BlobId,
    survivors: BTreeSet<BlobId>,
    evaluation_coverage_fingerprint: BlobId,
    equal_budget_fingerprint: BlobId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PressureProgress {
    decision_fingerprint: BlobId,
    next_level: Option<u16>,
    affected_trials: BTreeSet<BlobId>,
    evaluation_coverage_fingerprint: BlobId,
    cumulative_compute: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ArchiveProgress {
    generation: u64,
    fingerprint: BlobId,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ReplayState {
    initialized: bool,
    status: Option<CampaignStatus>,
    budget: CampaignBudgetLedger,
    attempts: BTreeMap<TrialAttemptId, AttemptRecord>,
    successful_trials: BTreeSet<BlobId>,
    successful_charges: BTreeMap<BlobId, CampaignBudgetAmount>,
    scheduled_trials: BTreeSet<BlobId>,
    scheduled_order: Vec<BlobId>,
    eliminated_trials: BTreeSet<BlobId>,
    stopped_trials: BTreeSet<BlobId>,
    halving: BTreeMap<TrialCaseId, HalvingProgress>,
    pressure: BTreeMap<BlobId, PressureProgress>,
    nested_pools: BTreeSet<(TrialCaseId, BlobId)>,
    archive: Option<ArchiveProgress>,
    decision_ids: BTreeSet<ArtifactId>,
    decision_artifacts: BTreeSet<BlobId>,
    last_event_fingerprint: Option<BlobId>,
    next_sequence: u32,
}

#[derive(Debug)]
enum CampaignJournalPersistence {
    Store(Box<ResearchJournalWriter>),
    #[cfg(test)]
    Diagnostic {
        lease_fingerprint: BlobId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CanonicalCampaignEventRecord {
    format: String,
    project_id: ProjectId,
    campaign_fingerprint: BlobId,
    session_id: ArtifactId,
    store_lease_fingerprint: BlobId,
    event: CampaignEvent,
}

#[derive(Serialize)]
struct CanonicalCampaignTrialAttemptRecord {
    format: &'static str,
    campaign_fingerprint: BlobId,
    event_fingerprint: BlobId,
    attempt_id: TrialAttemptId,
    trial_fingerprint: BlobId,
    attempt_ordinal: u16,
}

#[derive(Serialize)]
struct CanonicalCampaignBudgetRecord {
    format: &'static str,
    campaign_fingerprint: BlobId,
    event_fingerprint: BlobId,
    attempt_id: TrialAttemptId,
    amount: CampaignBudgetAmount,
}

#[derive(Serialize)]
struct CanonicalSearchDecisionRecord<'a> {
    format: &'static str,
    campaign_fingerprint: BlobId,
    event_fingerprint: BlobId,
    decision_index: u32,
    receipt: &'a SearchDecisionReceipt,
}

fn canonical_campaign_event_bytes(
    spec: &FrozenCampaignSpec,
    event: &CampaignEvent,
    session_id: ArtifactId,
    store_lease_fingerprint: BlobId,
) -> Result<Vec<u8>, CampaignError> {
    Ok(serde_json::to_vec(&CanonicalCampaignEventRecord {
        format: CAMPAIGN_EVENT_RECORD_FORMAT.to_owned(),
        project_id: spec.project_id(),
        campaign_fingerprint: spec.fingerprint(),
        session_id,
        store_lease_fingerprint,
        event: event.clone(),
    })?)
}

fn canonical_campaign_attempt_bytes(
    campaign_fingerprint: BlobId,
    event_fingerprint: BlobId,
    attempt_id: TrialAttemptId,
    trial_fingerprint: BlobId,
    attempt_ordinal: u16,
) -> Result<Vec<u8>, CampaignError> {
    Ok(serde_json::to_vec(&CanonicalCampaignTrialAttemptRecord {
        format: CAMPAIGN_TRIAL_ATTEMPT_RECORD_FORMAT,
        campaign_fingerprint,
        event_fingerprint,
        attempt_id,
        trial_fingerprint,
        attempt_ordinal,
    })?)
}

fn canonical_campaign_budget_bytes(
    format: &'static str,
    campaign_fingerprint: BlobId,
    event_fingerprint: BlobId,
    attempt_id: TrialAttemptId,
    amount: CampaignBudgetAmount,
) -> Result<Vec<u8>, CampaignError> {
    Ok(serde_json::to_vec(&CanonicalCampaignBudgetRecord {
        format,
        campaign_fingerprint,
        event_fingerprint,
        attempt_id,
        amount,
    })?)
}

fn canonical_search_decision_bytes(
    campaign_fingerprint: BlobId,
    event_fingerprint: BlobId,
    decision_index: u32,
    receipt: &SearchDecisionReceipt,
) -> Result<Vec<u8>, CampaignError> {
    Ok(serde_json::to_vec(&CanonicalSearchDecisionRecord {
        format: SEARCH_DECISION_RECORD_FORMAT,
        campaign_fingerprint,
        event_fingerprint,
        decision_index,
        receipt,
    })?)
}

/// Mutable reducer that can append scheduling claims only.
#[derive(Debug)]
pub struct CampaignJournal {
    spec: FrozenCampaignSpec,
    session_id: SchedulerSessionId,
    persistence: CampaignJournalPersistence,
    events: Vec<CampaignEvent>,
    state: ReplayState,
    recovered_dispatches: Vec<DispatchedTrialPermit>,
}

impl CampaignJournal {
    pub fn new(
        spec: FrozenCampaignSpec,
        store: &ProjectStore,
        session_lease: ExclusiveResearchSessionLease,
    ) -> Result<Self, CampaignError> {
        spec.verify()?;
        if session_lease.kind() != ResearchSessionKind::Campaign
            || session_lease.subject_fingerprint() != spec.fingerprint()
            || session_lease.project_id() != spec.project_id()
        {
            return Err(CampaignError::SessionLeaseMismatch);
        }
        if session_lease.record_fingerprint() != spec.canonical_record_fingerprint()? {
            return Err(CampaignError::SessionRecordMismatch);
        }
        verify_persisted_campaign_snapshot(&spec, &session_lease)?;
        let session_id = SchedulerSessionId::from_artifact_id(session_lease.session_id());
        let store_lease_fingerprint = session_lease.lease_fingerprint();
        let persistence = CampaignJournalPersistence::Store(Box::new(
            store.open_research_journal_writer(session_lease)?,
        ));
        if let CampaignJournalPersistence::Store(writer) = &persistence
            && !writer.load_campaign_events()?.is_empty()
        {
            return Err(CampaignError::JournalAlreadyExists);
        }
        let mut journal = Self {
            spec,
            session_id,
            persistence,
            events: Vec::new(),
            state: ReplayState::default(),
            recovered_dispatches: Vec::new(),
        };
        journal.append(CampaignEventKind::Prepared {
            campaign_fingerprint: journal.spec.fingerprint(),
            store_lease_fingerprint,
        })?;
        Ok(journal)
    }

    /// Reopens one exact persisted scheduler under a fresh affine store
    /// lease. Pre-crash reservations are released. Already-dispatched runs
    /// retain their exact durable identity and are reissued once through
    /// [`Self::take_recovered_dispatches`].
    pub fn resume(
        spec: FrozenCampaignSpec,
        store: &ProjectStore,
        session_lease: ExclusiveResearchSessionLease,
    ) -> Result<Self, CampaignError> {
        spec.verify()?;
        if session_lease.kind() != ResearchSessionKind::Campaign
            || session_lease.subject_fingerprint() != spec.fingerprint()
            || session_lease.project_id() != spec.project_id()
        {
            return Err(CampaignError::SessionLeaseMismatch);
        }
        if session_lease.record_fingerprint() != spec.canonical_record_fingerprint()? {
            return Err(CampaignError::SessionRecordMismatch);
        }
        verify_persisted_campaign_snapshot(&spec, &session_lease)?;
        let session_id = SchedulerSessionId::from_artifact_id(session_lease.session_id());
        let writer = store.open_research_journal_writer(session_lease)?;
        let persisted = writer.load_campaign_events()?;
        if persisted.is_empty() {
            return Err(CampaignError::EmptyJournal);
        }
        let events = decode_persisted_campaign_events(&spec, &persisted)?;
        let state = reduce_events(&spec, &events)?;
        if state.status.is_some_and(CampaignStatus::is_terminal) {
            return Err(CampaignError::CampaignTerminal);
        }
        let mut journal = Self {
            spec,
            session_id,
            persistence: CampaignJournalPersistence::Store(Box::new(writer)),
            events,
            state,
            recovered_dispatches: Vec::new(),
        };
        journal.reconcile_pre_crash_attempts()?;
        Ok(journal)
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_for_tests(spec: FrozenCampaignSpec) -> Result<Self, CampaignError> {
        spec.verify()?;
        let session_id = SchedulerSessionId::new();
        let store_lease_fingerprint = BlobId::digest(b"test-only live campaign store lease");
        let mut journal = Self {
            spec,
            session_id,
            persistence: CampaignJournalPersistence::Diagnostic {
                lease_fingerprint: store_lease_fingerprint,
            },
            events: Vec::new(),
            state: ReplayState::default(),
            recovered_dispatches: Vec::new(),
        };
        journal.append(CampaignEventKind::Prepared {
            campaign_fingerprint: journal.spec.fingerprint(),
            store_lease_fingerprint,
        })?;
        Ok(journal)
    }

    pub fn replay(
        spec: &FrozenCampaignSpec,
        events: &[CampaignEvent],
    ) -> Result<CheckedCampaignReplay, CampaignError> {
        let state = reduce_events(spec, events)?;
        Ok(CheckedCampaignReplay {
            campaign_fingerprint: spec.fingerprint(),
            snapshot: snapshot_from(&state),
            event_count: events.len(),
        })
    }

    pub fn replay_batch(
        spec: &FrozenCampaignSpec,
        events: &CampaignEventBatch,
    ) -> Result<CheckedCampaignReplay, CampaignError> {
        Self::replay(spec, events.events())
    }

    /// Restores an appendable scheduling journal after strict replay. Restored
    /// events remain claims; this type has no trial execution authority.
    #[cfg(test)]
    pub(crate) fn restore_for_tests(
        spec: FrozenCampaignSpec,
        events: Vec<CampaignEvent>,
    ) -> Result<Self, CampaignError> {
        let state = reduce_events(&spec, &events)?;
        Ok(Self {
            spec,
            session_id: SchedulerSessionId::new(),
            persistence: CampaignJournalPersistence::Diagnostic {
                lease_fingerprint: BlobId::digest(b"test-only restored campaign journal"),
            },
            events,
            state,
            recovered_dispatches: Vec::new(),
        })
    }

    pub const fn spec(&self) -> &FrozenCampaignSpec {
        &self.spec
    }

    pub fn store_lease_fingerprint(&self) -> BlobId {
        match &self.persistence {
            CampaignJournalPersistence::Store(writer) => writer.lease_fingerprint(),
            #[cfg(test)]
            CampaignJournalPersistence::Diagnostic { lease_fingerprint } => *lease_fingerprint,
        }
    }

    pub fn events(&self) -> &[CampaignEvent] {
        &self.events
    }

    pub fn snapshot(&self) -> CampaignSnapshot {
        snapshot_from(&self.state)
    }

    /// Moves out the same-run permits reconstructed from durable dispatched
    /// events. Each resumed journal issues this set at most once.
    pub fn take_recovered_dispatches(&mut self) -> Vec<DispatchedTrialPermit> {
        std::mem::take(&mut self.recovered_dispatches)
    }

    pub fn attempt_status(&self, attempt_id: TrialAttemptId) -> Option<CampaignTrialAttemptStatus> {
        self.state
            .attempts
            .get(&attempt_id)
            .map(|attempt| public_attempt_status(attempt.state))
    }

    pub fn trial_scheduling_status(
        &self,
        trial_fingerprint: BlobId,
    ) -> Result<TrialSchedulingStatus, CampaignError> {
        let trial = self
            .spec
            .trial(trial_fingerprint)
            .ok_or(CampaignError::UnknownTrial(trial_fingerprint))?;
        if self.state.eliminated_trials.contains(&trial_fingerprint) {
            return Ok(TrialSchedulingStatus::Eliminated);
        }
        if self.state.stopped_trials.contains(&trial_fingerprint) {
            return Ok(TrialSchedulingStatus::PressureStopped);
        }
        if self.state.successful_trials.contains(&trial_fingerprint) {
            return Ok(TrialSchedulingStatus::Completed);
        }
        if self.state.attempts.values().any(|attempt| {
            attempt.trial_fingerprint == trial_fingerprint
                && matches!(
                    attempt.state,
                    AttemptState::Reserved | AttemptState::Dispatched
                )
        }) {
            return Ok(TrialSchedulingStatus::Active);
        }
        if !self.state.scheduled_trials.contains(&trial_fingerprint) {
            return Ok(TrialSchedulingStatus::Unscheduled);
        }
        if trial
            .dependencies()
            .iter()
            .any(|dependency| !self.state.successful_trials.contains(dependency))
        {
            return Ok(TrialSchedulingStatus::WaitingForDependencies);
        }
        Ok(TrialSchedulingStatus::Runnable)
    }

    pub fn start(&mut self) -> Result<(), CampaignError> {
        self.append(CampaignEventKind::Started)
    }

    pub fn request_pause(&mut self, reason_fingerprint: BlobId) -> Result<(), CampaignError> {
        self.append(CampaignEventKind::PauseRequested { reason_fingerprint })
    }

    pub fn acknowledge_paused(&mut self) -> Result<(), CampaignError> {
        self.append(CampaignEventKind::Paused)
    }

    pub fn resume_running(&mut self) -> Result<(), CampaignError> {
        self.append(CampaignEventKind::Resumed)
    }

    pub fn request_cancel(&mut self, reason_fingerprint: BlobId) -> Result<(), CampaignError> {
        self.append(CampaignEventKind::CancelRequested { reason_fingerprint })
    }

    /// Reserves the frozen trial's complete maximum before a dispatch claim.
    pub fn reserve_trial(
        &mut self,
        trial_fingerprint: BlobId,
        attempt_id: TrialAttemptId,
    ) -> Result<ReservedTrialPermit, CampaignError> {
        let trial = self
            .spec
            .trial(trial_fingerprint)
            .ok_or(CampaignError::UnknownTrial(trial_fingerprint))?;
        let count = self
            .state
            .attempts
            .values()
            .filter(|attempt| attempt.trial_fingerprint == trial_fingerprint)
            .count();
        let attempt_ordinal = u16::try_from(count + 1).map_err(|_| CampaignError::AttemptLimit)?;
        self.append(CampaignEventKind::TrialReserved {
            attempt_id,
            trial_fingerprint,
            attempt_ordinal,
            reservation: trial.budget_maximum(),
        })?;
        Ok(ReservedTrialPermit {
            campaign_fingerprint: self.spec.fingerprint(),
            session_id: self.session_id,
            attempt_id,
            trial_fingerprint,
            trial_spec_fingerprint: trial_fingerprint,
            reservation_event_fingerprint: self.snapshot().last_event_fingerprint(),
        })
    }

    pub fn dispatch_reserved(
        &mut self,
        permit: ReservedTrialPermit,
    ) -> Result<DispatchedTrialPermit, CampaignError> {
        self.verify_reserved_permit(&permit)?;
        let (
            campaign_fingerprint,
            session_id,
            attempt_id,
            trial_fingerprint,
            trial_spec_fingerprint,
            _,
        ) = permit.into_parts();
        let reservation = self
            .state
            .attempts
            .get(&attempt_id)
            .ok_or(CampaignError::AttemptNotFound(attempt_id))?
            .reservation;
        self.append(CampaignEventKind::TrialDispatched { attempt_id })?;
        Ok(DispatchedTrialPermit {
            campaign_fingerprint,
            session_id,
            attempt_id,
            trial_fingerprint,
            trial_spec_fingerprint,
            reservation,
            dispatch_event_fingerprint: self.snapshot().last_event_fingerprint(),
        })
    }

    pub fn accept_trial_terminal(
        &mut self,
        terminal_lease: VerifiedTrialTerminalLease,
    ) -> Result<(), CampaignError> {
        self.verify_trial_terminal_lease(&terminal_lease)?;
        let (attempt_id, terminal, actual_charge, live_terminal_evidence_fingerprint) =
            terminal_lease.into_parts();
        self.append(CampaignEventKind::TrialFinished {
            attempt_id,
            terminal,
            actual_charge,
            live_terminal_evidence_fingerprint,
        })
    }

    /// Conservatively charges the whole maximum when a dispatched trial has
    /// no trustworthy terminal usage receipt after recovery.
    pub fn reconcile_interrupted(
        &mut self,
        permit: DispatchedTrialPermit,
        diagnostic_fingerprint: BlobId,
    ) -> Result<(), CampaignError> {
        self.verify_dispatched_permit(&permit)?;
        let (_, _, attempt_id, _, _, _, _) = permit.into_parts();
        let attempt = self
            .state
            .attempts
            .get(&attempt_id)
            .ok_or(CampaignError::AttemptNotFound(attempt_id))?;
        self.append(CampaignEventKind::TrialFinished {
            attempt_id,
            terminal: CampaignTrialTerminal::Interrupted {
                diagnostic_fingerprint,
            },
            actual_charge: attempt.reservation,
            live_terminal_evidence_fingerprint: diagnostic_fingerprint,
        })
    }

    pub fn abandon_reserved(
        &mut self,
        permit: ReservedTrialPermit,
        diagnostic_fingerprint: BlobId,
    ) -> Result<(), CampaignError> {
        self.verify_reserved_permit(&permit)?;
        let (_, _, attempt_id, _, _, _) = permit.into_parts();
        self.append(CampaignEventKind::TrialReservationReleased {
            attempt_id,
            diagnostic_fingerprint,
        })
    }

    pub fn schedule_blocked_factorial(
        &mut self,
        plan: &BlockedFactorialPlan,
    ) -> Result<BlobId, CampaignError> {
        require_status(&self.state, CampaignStatus::Running)?;
        let trial_ids = plan.trial_fingerprints().collect::<Vec<_>>();
        let mut case_id = None;
        let mut common_budget = None;
        for trial_fingerprint in &trial_ids {
            let node = self
                .spec
                .trial(*trial_fingerprint)
                .ok_or(CampaignError::DecisionUnknownTrial(*trial_fingerprint))?;
            let arm = if plan.baseline().trial_fingerprint() == *trial_fingerprint {
                plan.baseline()
            } else {
                plan.variants()
                    .iter()
                    .find(|arm| arm.trial_fingerprint() == *trial_fingerprint)
                    .ok_or(CampaignError::FactorialBinding)?
            };
            if node.treatment_fingerprint() != arm.treatment_fingerprint() {
                return Err(CampaignError::FactorialBinding);
            }
            if case_id
                .replace(node.case_id())
                .is_some_and(|id| id != node.case_id())
            {
                return Err(CampaignError::UnequalCaseBlock);
            }
            if common_budget
                .replace(node.budget_maximum())
                .is_some_and(|budget| budget != node.budget_maximum())
            {
                return Err(CampaignError::UnequalTrialBudgets);
            }
            if self.state.scheduled_trials.contains(trial_fingerprint) {
                return Err(CampaignError::TrialAlreadyScheduled(*trial_fingerprint));
            }
        }
        let seeded_trial_order = self.seeded_topological_order(plan.fingerprint(), &trial_ids)?;
        let effect = SearchDecisionEffect::BlockedFactorialScheduled {
            artifact_fingerprint: plan.fingerprint(),
            seed: self.spec.seed(),
            seeded_trial_order: bounded_trials(seeded_trial_order)?,
        };
        self.append_verified_decision(effect)
    }

    pub fn record_nested_pool(
        &mut self,
        lease: VerifiedNestedPoolLease,
    ) -> Result<NestedPoolPlan, CampaignError> {
        require_status(&self.state, CampaignStatus::Running)?;
        if lease.campaign_fingerprint != self.spec.fingerprint()
            || lease.session_id != self.session_id
        {
            return Err(CampaignError::EvaluationLeaseMismatch);
        }
        let node = self
            .spec
            .trials()
            .iter()
            .find(|node| {
                node.case_id() == lease.case_id
                    && node.treatment_fingerprint() == lease.treatment_fingerprint
            })
            .ok_or(CampaignError::PoolBinding)?;
        if !self
            .state
            .successful_trials
            .contains(&node.trial_fingerprint())
        {
            return Err(CampaignError::TrialNotSuccessfullyCompleted(
                node.trial_fingerprint(),
            ));
        }
        let plan = NestedPoolPlan::new(
            lease.case_id,
            lease.treatment_fingerprint,
            lease.ordered_occurrences,
        )?;
        let effect = SearchDecisionEffect::NestedPoolRecorded {
            artifact_fingerprint: plan.fingerprint(),
            case_id: plan.case_id(),
            treatment_fingerprint: plan.treatment_fingerprint(),
            exact_boundaries_fingerprint: plan.exact_boundaries_fingerprint(),
            pool_evidence_fingerprint: lease.pool_evidence_fingerprint,
        };
        self.append_verified_decision(effect)?;
        Ok(plan)
    }

    pub fn apply_successive_halving(
        &mut self,
        rung: u16,
        leases: Vec<VerifiedEvaluatedCandidateLease>,
        survivor_count: usize,
    ) -> Result<SuccessiveHalvingDecision, CampaignError> {
        require_status(&self.state, CampaignStatus::Running)?;
        if leases.len() < 2 || leases.len() > MAX_DECISION_TRIAL_REFERENCES {
            return Err(CampaignError::Halving(HalvingError::InvalidCandidateCount(
                leases.len(),
            )));
        }
        let first = leases.first().expect("halving lease count checked");
        let case_id = first.case_id;
        let coverage = first.coverage_fingerprint;
        let budget =
            self.verified_evaluation_budget(first, EvaluationLeasePurpose::Halving { rung })?;
        let expected_prior = self.state.halving.get(&case_id);
        if rung == 1 {
            if expected_prior.is_some() {
                return Err(CampaignError::HalvingRungOutOfOrder);
            }
        } else {
            let prior = expected_prior.ok_or(CampaignError::HalvingRungOutOfOrder)?;
            if prior.rung.checked_add(1) != Some(rung) {
                return Err(CampaignError::HalvingRungOutOfOrder);
            }
        }
        let mut candidate_trials = BTreeSet::new();
        for lease in &leases {
            let current_budget =
                self.verified_evaluation_budget(lease, EvaluationLeasePurpose::Halving { rung })?;
            if lease.case_id != case_id || lease.coverage_fingerprint != coverage {
                return Err(CampaignError::UnequalEvaluationCoverage);
            }
            if current_budget != budget {
                return Err(CampaignError::UnequalTrialBudgets);
            }
            candidate_trials.insert(lease.trial_fingerprint);
        }
        if let Some(prior) = expected_prior
            && candidate_trials != prior.survivors
        {
            return Err(CampaignError::HalvingSurvivorMismatch);
        }
        let candidates = leases
            .into_iter()
            .map(VerifiedEvaluatedCandidateLease::into_halving_candidate)
            .collect();
        let decision =
            SuccessiveHalvingDecision::decide(self.spec.seed(), rung, candidates, survivor_count)?;
        let survivors = decision
            .survivors()
            .iter()
            .map(|candidate| candidate.trial_fingerprint())
            .collect();
        let eliminated = decision
            .eliminated()
            .iter()
            .map(|candidate| candidate.trial_fingerprint())
            .collect();
        let effect = SearchDecisionEffect::SuccessiveHalvingApplied {
            artifact_fingerprint: decision.fingerprint(),
            rung,
            case_id,
            prior_decision_fingerprint: expected_prior.map(|prior| prior.decision_fingerprint),
            evaluation_coverage_fingerprint: coverage,
            equal_budget_fingerprint: budget,
            survivors: bounded_trials(survivors)?,
            eliminated: bounded_trials(eliminated)?,
        };
        self.append_verified_decision(effect)?;
        Ok(decision)
    }

    pub fn apply_pressure_curve(
        &mut self,
        lease: VerifiedPressureCurveLease,
    ) -> Result<PressureCurveDecision, CampaignError> {
        require_status(&self.state, CampaignStatus::Running)?;
        if lease.campaign_fingerprint != self.spec.fingerprint()
            || lease.session_id != self.session_id
        {
            return Err(CampaignError::EvaluationLeaseMismatch);
        }
        let prior = self.state.pressure.get(&lease.family_fingerprint);
        if prior.map(|value| value.decision_fingerprint) != lease.prior_decision_fingerprint {
            return Err(CampaignError::PressureSequenceMismatch);
        }
        let affected = lease
            .affected_trials
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if affected.len() != lease.affected_trials.len() || affected.is_empty() {
            return Err(CampaignError::PressureTrialSet);
        }
        if prior.is_some_and(|value| {
            !value.affected_trials.is_subset(&affected)
                || value.evaluation_coverage_fingerprint != lease.evaluation_coverage_fingerprint
        }) {
            return Err(CampaignError::PressureSequenceMismatch);
        }
        for trial in &affected {
            if self.spec.trial(*trial).is_none() || !self.state.scheduled_trials.contains(trial) {
                return Err(CampaignError::DecisionUnknownTrial(*trial));
            }
            if self.state.stopped_trials.contains(trial) {
                return Err(CampaignError::TrialPressureStopped(*trial));
            }
        }
        let charges = lease
            .execution_charges
            .iter()
            .copied()
            .collect::<BTreeMap<_, _>>();
        if charges.len() != lease.execution_charges.len()
            || charges.keys().copied().collect::<BTreeSet<_>>() != affected
        {
            return Err(CampaignError::PressureChargeMismatch);
        }
        for (trial, charge) in &charges {
            if self.state.successful_charges.get(trial) != Some(charge) {
                return Err(CampaignError::PressureChargeMismatch);
            }
        }
        let cumulative_compute = charges.values().try_fold(0_u64, |total, charge| {
            total.checked_add(compute_units(*charge))
        });
        let cumulative_compute = cumulative_compute.ok_or(CampaignError::PressureChargeMismatch)?;
        let decision = PressureCurveDecision::decide(lease.policy, lease.points)?;
        let observed_level = decision
            .points()
            .last()
            .expect("pressure decision has at least two points")
            .level();
        if decision
            .points()
            .last()
            .expect("pressure decision has at least two points")
            .cumulative_compute()
            != cumulative_compute
        {
            return Err(CampaignError::PressureChargeMismatch);
        }
        if let Some(prior) = prior
            && prior.next_level != Some(observed_level)
        {
            return Err(CampaignError::PressureSequenceMismatch);
        }
        let affected_trials = bounded_trials(affected.iter().copied().collect())?;
        let effect = match decision.action() {
            PressureAction::Continue { next_level } => SearchDecisionEffect::PressureAdvanced {
                artifact_fingerprint: decision.fingerprint(),
                family_fingerprint: lease.family_fingerprint,
                observed_level,
                next_level,
                affected_trials,
                evaluation_coverage_fingerprint: lease.evaluation_coverage_fingerprint,
                evaluation_receipt_fingerprint: lease.evaluation_receipt_fingerprint,
                execution_charge_fingerprint: fingerprint_pressure_charges(&charges),
                cumulative_compute,
                prior_decision_fingerprint: lease.prior_decision_fingerprint,
            },
            PressureAction::Stop { .. } => SearchDecisionEffect::PressureStopped {
                artifact_fingerprint: decision.fingerprint(),
                family_fingerprint: lease.family_fingerprint,
                observed_level,
                stopped_trials: affected_trials,
                evaluation_coverage_fingerprint: lease.evaluation_coverage_fingerprint,
                evaluation_receipt_fingerprint: lease.evaluation_receipt_fingerprint,
                execution_charge_fingerprint: fingerprint_pressure_charges(&charges),
                cumulative_compute,
                prior_decision_fingerprint: lease.prior_decision_fingerprint,
            },
        };
        self.append_verified_decision(effect)?;
        Ok(decision)
    }

    pub fn initialize_archive(
        &mut self,
        archive: &MapElitesArchive,
    ) -> Result<BlobId, CampaignError> {
        require_status(&self.state, CampaignStatus::Running)?;
        archive.verify()?;
        if archive.campaign_fingerprint() != self.spec.fingerprint()
            || archive.generation() != 0
            || archive.parent_fingerprint().is_some()
        {
            return Err(CampaignError::ArchiveSequenceMismatch);
        }
        self.append_verified_decision(SearchDecisionEffect::MapElitesInitialized {
            artifact_fingerprint: archive.fingerprint(),
            generation: 0,
        })
    }

    pub fn consider_archive_candidate(
        &mut self,
        archive: &MapElitesArchive,
        lease: VerifiedEvaluatedCandidateLease,
    ) -> Result<ArchiveUpdate, CampaignError> {
        require_status(&self.state, CampaignStatus::Running)?;
        let progress = self
            .state
            .archive
            .ok_or(CampaignError::ArchiveNotInitialized)?;
        if archive.campaign_fingerprint() != self.spec.fingerprint()
            || archive.generation() != progress.generation
            || archive.fingerprint() != progress.fingerprint
        {
            return Err(CampaignError::ArchiveSequenceMismatch);
        }
        self.verified_evaluation_budget(
            &lease,
            EvaluationLeasePurpose::Archive {
                parent_fingerprint: archive.fingerprint(),
            },
        )?;
        let update = archive.consider(lease)?;
        if update.decision().kind() == ArchiveDecisionKind::AlreadyPresent {
            return Ok(update);
        }
        let occurrence_id = update.decision().challenger_occurrence();
        let commitment = update
            .snapshot()
            .occurrence_commitment(occurrence_id)
            .ok_or(CampaignError::ArchiveSequenceMismatch)?;
        self.append_verified_decision(SearchDecisionEffect::MapElitesAdvanced {
            artifact_fingerprint: update.snapshot().fingerprint(),
            generation: update.snapshot().generation(),
            parent_fingerprint: archive.fingerprint(),
            admitted_occurrence_id: occurrence_id,
            admitted_occurrence_commitment: commitment,
        })?;
        Ok(update)
    }

    fn verified_evaluation_budget(
        &self,
        lease: &VerifiedEvaluatedCandidateLease,
        purpose: EvaluationLeasePurpose,
    ) -> Result<BlobId, CampaignError> {
        if lease.campaign_fingerprint != self.spec.fingerprint()
            || lease.session_id != self.session_id
            || lease.purpose != purpose
        {
            return Err(CampaignError::EvaluationLeaseMismatch);
        }
        let node = self
            .spec
            .trial(lease.trial_fingerprint)
            .ok_or(CampaignError::DecisionUnknownTrial(lease.trial_fingerprint))?;
        if node.case_id() != lease.case_id
            || node.treatment_fingerprint() != lease.treatment_fingerprint
        {
            return Err(CampaignError::EvaluationLeaseMismatch);
        }
        let charge = self
            .state
            .successful_charges
            .get(&lease.trial_fingerprint)
            .ok_or(CampaignError::TrialNotSuccessfullyCompleted(
                lease.trial_fingerprint,
            ))?;
        if *charge != lease.actual_charge {
            return Err(CampaignError::EvaluationChargeMismatch);
        }
        Ok(fingerprint_budget(node.budget_maximum()))
    }

    fn seeded_topological_order(
        &self,
        plan_fingerprint: BlobId,
        trial_ids: &[BlobId],
    ) -> Result<Vec<BlobId>, CampaignError> {
        let mut pending = trial_ids.iter().copied().collect::<BTreeSet<_>>();
        let mut ordered = Vec::with_capacity(trial_ids.len());
        while !pending.is_empty() {
            let mut ready = pending
                .iter()
                .copied()
                .filter(|trial_fingerprint| {
                    self.spec
                        .trial(*trial_fingerprint)
                        .expect("factorial trial was verified")
                        .dependencies()
                        .iter()
                        .all(|dependency| !pending.contains(dependency))
                })
                .collect::<Vec<_>>();
            if ready.is_empty() {
                return Err(CampaignError::FactorialBinding);
            }
            ready.sort_unstable_by_key(|trial| {
                seeded_order_key(self.spec.seed(), plan_fingerprint, *trial)
            });
            let selected = ready[0];
            for dependency in self
                .spec
                .trial(selected)
                .expect("factorial trial was verified")
                .dependencies()
            {
                if !trial_ids.contains(dependency)
                    && !self.state.scheduled_trials.contains(dependency)
                    && !self.state.successful_trials.contains(dependency)
                {
                    return Err(CampaignError::FactorialDependencyOutsideSchedule {
                        trial: selected,
                        dependency: *dependency,
                    });
                }
            }
            pending.remove(&selected);
            ordered.push(selected);
        }
        Ok(ordered)
    }

    fn append_verified_decision(
        &mut self,
        effect: SearchDecisionEffect,
    ) -> Result<BlobId, CampaignError> {
        let receipt = SearchDecisionReceipt::from_verified_effect(ArtifactId::new(), effect)?;
        let fingerprint = receipt.fingerprint();
        self.append(CampaignEventKind::SearchDecisionRecorded {
            receipt: Box::new(receipt),
        })?;
        Ok(fingerprint)
    }

    #[cfg(test)]
    pub(crate) const fn session_id_for_tests(&self) -> SchedulerSessionId {
        self.session_id
    }

    /// Proves from the current live reducer and exact event head that no
    /// scheduled or pressure-curve work remains runnable or required.
    pub fn verify_scheduler_terminal(
        &self,
    ) -> Result<VerifiedSchedulerTerminalLease, CampaignError> {
        verify_scheduler_terminal_state(&self.state)?;
        let event_head_fingerprint = self.snapshot().last_event_fingerprint();
        Ok(VerifiedSchedulerTerminalLease {
            campaign_fingerprint: self.spec.fingerprint(),
            session_id: self.session_id,
            event_head_fingerprint,
            scheduler_evidence_fingerprint: scheduler_terminal_evidence_fingerprint(
                self.spec.fingerprint(),
                self.session_id,
                event_head_fingerprint,
                &self.state,
            ),
        })
    }

    pub fn complete(
        &mut self,
        terminal_lease: VerifiedSchedulerTerminalLease,
    ) -> Result<(), CampaignError> {
        self.verify_scheduler_terminal_lease(&terminal_lease)?;
        let scheduler_evidence_fingerprint = terminal_lease.into_evidence();
        self.close(
            CampaignDisposition::Completed,
            None,
            Some(scheduler_evidence_fingerprint),
        )
    }

    pub fn fail(&mut self, diagnostic_fingerprint: BlobId) -> Result<(), CampaignError> {
        self.close(
            CampaignDisposition::Failed,
            Some(diagnostic_fingerprint),
            None,
        )
    }

    pub fn cancelled(&mut self, diagnostic_fingerprint: BlobId) -> Result<(), CampaignError> {
        self.close(
            CampaignDisposition::Cancelled,
            Some(diagnostic_fingerprint),
            None,
        )
    }

    fn close(
        &mut self,
        disposition: CampaignDisposition,
        diagnostic_fingerprint: Option<BlobId>,
        scheduler_evidence_fingerprint: Option<BlobId>,
    ) -> Result<(), CampaignError> {
        self.append(CampaignEventKind::CampaignClosed {
            disposition,
            diagnostic_fingerprint,
            scheduler_evidence_fingerprint,
        })
    }

    fn verify_reserved_permit(&self, permit: &ReservedTrialPermit) -> Result<(), CampaignError> {
        if permit.campaign_fingerprint != self.spec.fingerprint()
            || permit.session_id != self.session_id
            || permit.trial_spec_fingerprint != permit.trial_fingerprint
        {
            return Err(CampaignError::ExecutionPermitMismatch);
        }
        let attempt = self
            .state
            .attempts
            .get(&permit.attempt_id)
            .ok_or(CampaignError::AttemptNotFound(permit.attempt_id))?;
        if attempt.state != AttemptState::Reserved
            || attempt.trial_fingerprint != permit.trial_fingerprint
            || permit.reservation_event_fingerprint != attempt.reservation_event_fingerprint
        {
            return Err(CampaignError::ExecutionPermitMismatch);
        }
        Ok(())
    }

    fn verify_dispatched_permit(
        &self,
        permit: &DispatchedTrialPermit,
    ) -> Result<(), CampaignError> {
        if permit.campaign_fingerprint != self.spec.fingerprint()
            || permit.session_id != self.session_id
            || permit.trial_spec_fingerprint != permit.trial_fingerprint
        {
            return Err(CampaignError::ExecutionPermitMismatch);
        }
        let attempt = self
            .state
            .attempts
            .get(&permit.attempt_id)
            .ok_or(CampaignError::AttemptNotFound(permit.attempt_id))?;
        if attempt.state != AttemptState::Dispatched
            || attempt.trial_fingerprint != permit.trial_fingerprint
            || attempt.reservation != permit.reservation
            || Some(permit.dispatch_event_fingerprint) != attempt.dispatch_event_fingerprint
        {
            return Err(CampaignError::ExecutionPermitMismatch);
        }
        Ok(())
    }

    fn verify_trial_terminal_lease(
        &self,
        lease: &VerifiedTrialTerminalLease,
    ) -> Result<(), CampaignError> {
        if lease.campaign_fingerprint != self.spec.fingerprint()
            || lease.session_id != self.session_id
            || lease.trial_spec_fingerprint != lease.trial_fingerprint
        {
            return Err(CampaignError::TrialTerminalLeaseMismatch);
        }
        let attempt = self
            .state
            .attempts
            .get(&lease.attempt_id)
            .ok_or(CampaignError::AttemptNotFound(lease.attempt_id))?;
        if attempt.state != AttemptState::Dispatched
            || attempt.trial_fingerprint != lease.trial_fingerprint
            || attempt.reservation != lease.reservation
            || Some(lease.dispatch_event_fingerprint) != attempt.dispatch_event_fingerprint
        {
            return Err(CampaignError::TrialTerminalLeaseMismatch);
        }
        Ok(())
    }

    fn verify_scheduler_terminal_lease(
        &self,
        lease: &VerifiedSchedulerTerminalLease,
    ) -> Result<(), CampaignError> {
        if lease.campaign_fingerprint != self.spec.fingerprint()
            || lease.session_id != self.session_id
            || lease.event_head_fingerprint != self.snapshot().last_event_fingerprint()
        {
            return Err(CampaignError::SchedulerTerminalLeaseMismatch);
        }
        Ok(())
    }

    fn append(&mut self, kind: CampaignEventKind) -> Result<(), CampaignError> {
        if self.events.len() >= MAX_CAMPAIGN_EVENTS {
            return Err(CampaignError::EventLimit);
        }
        let event = CampaignEvent::new(
            self.state.next_sequence,
            self.state.last_event_fingerprint,
            kind,
        );
        let mut next = self.state.clone();
        apply_event(&self.spec, &mut next, &event)?;
        self.persist_event(&event)?;
        self.state = next;
        self.events.push(event);
        Ok(())
    }

    fn persist_event(&mut self, event: &CampaignEvent) -> Result<(), CampaignError> {
        #[cfg(test)]
        if self.has_diagnostic_persistence() {
            return Ok(());
        }

        let event_bytes = self.canonical_event_bytes(event)?;

        let attempt_bytes;
        let run_bytes;
        let reservation_bytes;
        let charge_bytes;
        let decision_bytes;
        let mutation = match &event.kind {
            CampaignEventKind::Prepared { .. } => CampaignJournalMutation::Prepared,
            CampaignEventKind::Started => CampaignJournalMutation::Started,
            CampaignEventKind::PauseRequested { .. } => CampaignJournalMutation::PauseRequested,
            CampaignEventKind::Paused => CampaignJournalMutation::Paused,
            CampaignEventKind::Resumed => CampaignJournalMutation::Resumed,
            CampaignEventKind::CancelRequested { .. } => CampaignJournalMutation::CancelRequested,
            CampaignEventKind::TrialReserved {
                attempt_id,
                trial_fingerprint,
                attempt_ordinal,
                reservation,
            } => {
                run_bytes = canonical_campaign_trial_run_bytes(
                    &self.spec,
                    *attempt_id,
                    *trial_fingerprint,
                )?;
                attempt_bytes = canonical_campaign_attempt_bytes(
                    self.spec.fingerprint(),
                    event.fingerprint,
                    *attempt_id,
                    *trial_fingerprint,
                    *attempt_ordinal,
                )?;
                reservation_bytes = canonical_campaign_budget_bytes(
                    CAMPAIGN_TRIAL_RESERVATION_RECORD_FORMAT,
                    self.spec.fingerprint(),
                    event.fingerprint,
                    *attempt_id,
                    *reservation,
                )?;
                CampaignJournalMutation::TrialReserved {
                    attempt_id: attempt_id.as_trial_run_id(),
                    trial_fingerprint: *trial_fingerprint,
                    attempt_ordinal: *attempt_ordinal,
                    reservation: research_journal_budget(*reservation),
                    canonical_run_bytes: &run_bytes,
                    canonical_attempt_bytes: &attempt_bytes,
                    canonical_reservation_bytes: &reservation_bytes,
                }
            }
            CampaignEventKind::TrialDispatched { attempt_id } => {
                trial_dispatched_mutation(*attempt_id)
            }
            CampaignEventKind::TrialFinished {
                attempt_id,
                terminal,
                actual_charge,
                ..
            } => {
                charge_bytes = canonical_campaign_budget_bytes(
                    CAMPAIGN_TRIAL_CHARGE_RECORD_FORMAT,
                    self.spec.fingerprint(),
                    event.fingerprint,
                    *attempt_id,
                    *actual_charge,
                )?;
                CampaignJournalMutation::TrialFinished {
                    attempt_id: attempt_id.as_trial_run_id(),
                    outcome: persisted_campaign_outcome(*terminal),
                    charge: research_journal_budget(*actual_charge),
                    canonical_charge_bytes: &charge_bytes,
                }
            }
            CampaignEventKind::TrialReservationReleased { attempt_id, .. } => {
                trial_reservation_released_mutation(*attempt_id)
            }
            CampaignEventKind::SearchDecisionRecorded { receipt } => {
                let decision_index = u32::try_from(self.state.decision_ids.len())
                    .map_err(|_| CampaignError::DecisionLimit)?;
                decision_bytes = canonical_search_decision_bytes(
                    self.spec.fingerprint(),
                    event.fingerprint,
                    decision_index,
                    receipt,
                )?;
                let (kind, parent_archive_fingerprint) =
                    persisted_search_decision_kind(receipt.effect());
                CampaignJournalMutation::SearchDecisionRecorded {
                    decision_index,
                    kind,
                    parent_archive_fingerprint,
                    canonical_decision_bytes: &decision_bytes,
                }
            }
            CampaignEventKind::CampaignClosed { .. } => CampaignJournalMutation::CampaignClosed,
        };
        self.append_persisted_event(event, &event_bytes, mutation)
    }

    #[cfg(test)]
    fn has_diagnostic_persistence(&self) -> bool {
        matches!(
            self.persistence,
            CampaignJournalPersistence::Diagnostic { .. }
        )
    }

    fn persistence_identity(&self) -> (ArtifactId, BlobId) {
        match &self.persistence {
            CampaignJournalPersistence::Store(writer) => {
                (writer.session_id(), writer.lease_fingerprint())
            }
            #[cfg(test)]
            CampaignJournalPersistence::Diagnostic { .. } => unreachable!("checked by caller"),
        }
    }

    fn canonical_event_bytes(&self, event: &CampaignEvent) -> Result<Vec<u8>, CampaignError> {
        let (session_id, store_lease_fingerprint) = self.persistence_identity();
        canonical_campaign_event_bytes(&self.spec, event, session_id, store_lease_fingerprint)
    }

    fn append_persisted_event(
        &mut self,
        event: &CampaignEvent,
        event_bytes: &[u8],
        mutation: CampaignJournalMutation<'_>,
    ) -> Result<(), CampaignError> {
        #[cfg(not(test))]
        let CampaignJournalPersistence::Store(writer) = &mut self.persistence;
        #[cfg(test)]
        let writer = match &mut self.persistence {
            CampaignJournalPersistence::Store(writer) => writer,
            CampaignJournalPersistence::Diagnostic { .. } => unreachable!("checked by caller"),
        };
        writer.append_campaign_event(CampaignJournalEventPersistence {
            campaign_fingerprint: self.spec.fingerprint(),
            event_index: event.sequence,
            previous_event_fingerprint: event.previous_event_fingerprint,
            event_fingerprint: event.fingerprint,
            canonical_event_bytes: event_bytes,
            mutation,
        })?;
        Ok(())
    }

    fn reconcile_pre_crash_attempts(&mut self) -> Result<(), CampaignError> {
        let active = self
            .state
            .attempts
            .iter()
            .filter_map(|(attempt_id, record)| match record.state {
                AttemptState::Reserved | AttemptState::Dispatched => Some((*attempt_id, *record)),
                AttemptState::Finished(_) | AttemptState::Released => None,
            })
            .collect::<Vec<_>>();
        for (attempt_id, record) in active {
            let diagnostic = recovery_diagnostic(
                self.spec.fingerprint(),
                self.state
                    .last_event_fingerprint
                    .expect("persisted campaign has an event head"),
                attempt_id,
                record.state,
            );
            match record.state {
                AttemptState::Reserved => {
                    self.append(CampaignEventKind::TrialReservationReleased {
                        attempt_id,
                        diagnostic_fingerprint: diagnostic,
                    })?;
                }
                AttemptState::Dispatched => {
                    self.recovered_dispatches.push(DispatchedTrialPermit {
                        campaign_fingerprint: self.spec.fingerprint(),
                        session_id: self.session_id,
                        attempt_id,
                        trial_fingerprint: record.trial_fingerprint,
                        trial_spec_fingerprint: record.trial_fingerprint,
                        reservation: record.reservation,
                        dispatch_event_fingerprint: record
                            .dispatch_event_fingerprint
                            .expect("dispatched attempt has event fingerprint"),
                    });
                }
                AttemptState::Finished(_) | AttemptState::Released => {
                    unreachable!("filtered active attempts")
                }
            }
        }
        Ok(())
    }
}

fn canonical_campaign_trial_run_bytes(
    spec: &FrozenCampaignSpec,
    attempt_id: TrialAttemptId,
    trial_fingerprint: BlobId,
) -> Result<Vec<u8>, CampaignError> {
    TrialRunRecord::new(
        attempt_id.as_trial_run_id(),
        trial_fingerprint,
        TrialRunOrigin::Campaign {
            campaign_id: spec.campaign_id(),
            campaign_fingerprint: spec.fingerprint(),
        },
    )
    .canonical_bytes()
    .map_err(CampaignError::from)
}

fn trial_dispatched_mutation(attempt_id: TrialAttemptId) -> CampaignJournalMutation<'static> {
    CampaignJournalMutation::TrialDispatched {
        attempt_id: attempt_id.as_trial_run_id(),
    }
}

fn trial_reservation_released_mutation(
    attempt_id: TrialAttemptId,
) -> CampaignJournalMutation<'static> {
    CampaignJournalMutation::TrialReservationReleased {
        attempt_id: attempt_id.as_trial_run_id(),
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CampaignError {
    #[error(transparent)]
    Spec(#[from] CampaignSpecError),
    #[error(transparent)]
    Budget(#[from] CampaignBudgetError),
    #[error(transparent)]
    Decision(#[from] SearchDecisionError),
    #[error(transparent)]
    Factorial(#[from] FactorialError),
    #[error(transparent)]
    NestedPool(#[from] NestedPoolError),
    #[error(transparent)]
    Halving(#[from] HalvingError),
    #[error(transparent)]
    Pressure(#[from] PressureError),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error("campaign journal contains no events")]
    EmptyJournal,
    #[error("live campaign session lease does not match the frozen campaign")]
    SessionLeaseMismatch,
    #[error("live campaign session record does not match the frozen campaign's canonical bytes")]
    SessionRecordMismatch,
    #[error("persisted normalized campaign or trial rows do not match the frozen campaign")]
    SessionNormalizedSnapshotMismatch,
    #[error("durable campaign journal storage failed")]
    Store,
    #[error("durable campaign journal JSON encoding or decoding failed")]
    Json,
    #[error("a durable campaign journal already exists; use resume")]
    JournalAlreadyExists,
    #[error("persisted campaign journal record is malformed or non-canonical")]
    MalformedJournalRecord,
    #[error("affine trial execution permit does not match this live journal and attempt")]
    ExecutionPermitMismatch,
    #[error("verified trial terminal lease does not match its dispatched permit")]
    TrialTerminalLeaseMismatch,
    #[error("verified scheduler terminal lease does not match the current event head")]
    SchedulerTerminalLeaseMismatch,
    #[error("campaign journal does not begin with its prepared event")]
    MissingPrepared,
    #[error("campaign event limit exceeded")]
    EventLimit,
    #[error("campaign trial attempt limit exceeded")]
    AttemptLimit,
    #[error("campaign search decision limit exceeded")]
    DecisionLimit,
    #[error("event sequence mismatch: expected {expected}, received {actual}")]
    EventSequence { expected: u32, actual: u32 },
    #[error("event previous-fingerprint link mismatch")]
    PreviousEventFingerprint,
    #[error("event fingerprint mismatch")]
    EventFingerprint,
    #[error("campaign prepared event is invalid or misplaced")]
    InvalidPrepared,
    #[error("invalid campaign state transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: CampaignStatus,
        to: CampaignStatus,
    },
    #[error("campaign is terminal")]
    CampaignTerminal,
    #[error("campaign has active trial attempts")]
    ActiveAttempts,
    #[error("campaign has no successfully completed trial")]
    NoSuccessfulTrial,
    #[error("unknown frozen trial {0}")]
    UnknownTrial(BlobId),
    #[error("trial {trial} dependency {dependency} has not completed")]
    DependencyNotSatisfied { trial: BlobId, dependency: BlobId },
    #[error("trial {0} has already completed")]
    TrialAlreadyCompleted(BlobId),
    #[error("trial {0} has not been scheduled by a verified search operation")]
    TrialNotScheduled(BlobId),
    #[error("trial {0} has already been scheduled")]
    TrialAlreadyScheduled(BlobId),
    #[error("trial reservation is out of frozen seeded order: expected {expected}, got {actual}")]
    TrialReservationOutOfOrder { expected: BlobId, actual: BlobId },
    #[error("factorial trial {trial} depends on unscheduled trial {dependency}")]
    FactorialDependencyOutsideSchedule { trial: BlobId, dependency: BlobId },
    #[error("trial {0} was eliminated by successive halving")]
    TrialEliminated(BlobId),
    #[error("trial {0} was stopped by its pressure curve")]
    TrialPressureStopped(BlobId),
    #[error("trial {0} already has an active attempt")]
    TrialAttemptActive(BlobId),
    #[error("trial attempt {0} was already used")]
    DuplicateAttemptId(TrialAttemptId),
    #[error("trial attempt {0} is absent")]
    AttemptNotFound(TrialAttemptId),
    #[error("trial attempt {0} is not reserved")]
    AttemptNotReserved(TrialAttemptId),
    #[error("trial attempt {0} is not dispatched")]
    AttemptNotDispatched(TrialAttemptId),
    #[error("trial attempt ordinal is not the next immutable retry ordinal")]
    AttemptOrdinal,
    #[error("trial reservation does not equal the frozen maximum")]
    ReservationMismatch,
    #[error("an interrupted trial must conservatively charge its whole reservation")]
    InterruptedChargeMismatch,
    #[error("search decision {0} was already recorded")]
    DuplicateDecision(ArtifactId),
    #[error("search artifact {0} was already applied")]
    DuplicateDecisionArtifact(BlobId),
    #[error("search decision references unknown frozen trial {0}")]
    DecisionUnknownTrial(BlobId),
    #[error("blocked factorial does not match the frozen trial bindings")]
    FactorialBinding,
    #[error("blocked factorial arms do not share one case")]
    UnequalCaseBlock,
    #[error("search candidates do not have equal frozen budgets")]
    UnequalTrialBudgets,
    #[error("verified evaluation leases do not have common coverage")]
    UnequalEvaluationCoverage,
    #[error("verified evaluation lease does not match this live scheduler")]
    EvaluationLeaseMismatch,
    #[error("verified evaluation charge does not match the admitted terminal charge")]
    EvaluationChargeMismatch,
    #[error("trial {0} lacks a verified successful terminal")]
    TrialNotSuccessfullyCompleted(BlobId),
    #[error("nested pool is not bound to a successful frozen case/treatment")]
    PoolBinding,
    #[error("the case/treatment already has a nested pool")]
    DuplicateNestedPool,
    #[error("successive-halving rung is missing, repeated, or out of order")]
    HalvingRungOutOfOrder,
    #[error("successive-halving candidates do not equal the prior survivors")]
    HalvingSurvivorMismatch,
    #[error("successive-halving evidence does not match frozen completed trials")]
    HalvingEvidenceMismatch,
    #[error("pressure curve decision is out of order")]
    PressureSequenceMismatch,
    #[error("pressure curve has an invalid affected-trial set")]
    PressureTrialSet,
    #[error("pressure compute does not match immutable successful execution charges")]
    PressureChargeMismatch,
    #[error("MAP-Elites archive has not been initialized")]
    ArchiveNotInitialized,
    #[error("MAP-Elites archive generation or parent does not match scheduler state")]
    ArchiveSequenceMismatch,
    #[error("scheduled or pressure-required work remains")]
    RequiredWorkRemaining,
    #[error("campaign disposition has invalid diagnostic evidence")]
    InvalidDispositionEvidence,
}

impl From<loom_store::StoreError> for CampaignError {
    fn from(_: loom_store::StoreError) -> Self {
        Self::Store
    }
}

impl From<serde_json::Error> for CampaignError {
    fn from(_: serde_json::Error) -> Self {
        Self::Json
    }
}

fn verify_persisted_campaign_snapshot(
    spec: &FrozenCampaignSpec,
    lease: &ExclusiveResearchSessionLease,
) -> Result<(), CampaignError> {
    let PersistedResearchSubjectSnapshot::Campaign(snapshot) = lease.snapshot() else {
        return Err(CampaignError::SessionNormalizedSnapshotMismatch);
    };
    if snapshot.campaign_id() != spec.campaign_id()
        || snapshot.campaign_fingerprint() != spec.fingerprint()
        || snapshot.project_id() != spec.project_id()
        || snapshot.manifest_source_fingerprint() != spec.manifest_source_fingerprint()
        || snapshot.manifest_fingerprint() != spec.manifest_fingerprint()
        || snapshot.project_input_fingerprint() != spec.project_input_fingerprint()
        || snapshot.seed() != spec.seed()
        || snapshot.maximum() != research_budget_from_limits(spec.budget_limits())
        || snapshot.record_fingerprint() != spec.canonical_record_fingerprint()?
        || snapshot.trials().len() != spec.trials().len()
    {
        return Err(CampaignError::SessionNormalizedSnapshotMismatch);
    }
    for (persisted, frozen) in snapshot.trials().iter().zip(spec.trials()) {
        if persisted.trial_fingerprint() != frozen.trial_fingerprint()
            || persisted.trial_case_id() != frozen.case_id()
            || persisted.treatment_fingerprint() != frozen.treatment_fingerprint()
            || persisted.maximum() != research_budget_from_amount(frozen.budget_maximum())
            || persisted.dependencies() != frozen.dependencies()
        {
            return Err(CampaignError::SessionNormalizedSnapshotMismatch);
        }
    }
    Ok(())
}

const fn research_budget_from_limits(limits: crate::CampaignBudgetLimits) -> ResearchBudgetMaximum {
    ResearchBudgetMaximum {
        writer_tokens: limits.writer_tokens(),
        controller_tokens: limits.controller_tokens(),
        evaluations: limits.evaluations(),
        wall_time_ms: limits.wall_time_ms(),
    }
}

const fn research_budget_from_amount(amount: CampaignBudgetAmount) -> ResearchBudgetMaximum {
    ResearchBudgetMaximum {
        writer_tokens: amount.writer_tokens(),
        controller_tokens: amount.controller_tokens(),
        evaluations: amount.evaluations(),
        wall_time_ms: amount.wall_time_ms(),
    }
}

fn reduce_events(
    spec: &FrozenCampaignSpec,
    events: &[CampaignEvent],
) -> Result<ReplayState, CampaignError> {
    spec.verify()?;
    if events.is_empty() {
        return Err(CampaignError::EmptyJournal);
    }
    if events.len() > MAX_CAMPAIGN_EVENTS {
        return Err(CampaignError::EventLimit);
    }
    let mut state = ReplayState::default();
    for event in events {
        apply_event(spec, &mut state, event)?;
    }
    if !state.initialized {
        return Err(CampaignError::MissingPrepared);
    }
    Ok(state)
}

fn apply_event(
    spec: &FrozenCampaignSpec,
    state: &mut ReplayState,
    event: &CampaignEvent,
) -> Result<(), CampaignError> {
    if event.sequence != state.next_sequence {
        return Err(CampaignError::EventSequence {
            expected: state.next_sequence,
            actual: event.sequence,
        });
    }
    if event.previous_event_fingerprint != state.last_event_fingerprint {
        return Err(CampaignError::PreviousEventFingerprint);
    }
    if fingerprint_event(
        event.sequence,
        event.previous_event_fingerprint,
        &event.kind,
    ) != event.fingerprint
    {
        return Err(CampaignError::EventFingerprint);
    }

    apply_event_kind(spec, state, event.sequence, event.fingerprint, &event.kind)?;

    state.last_event_fingerprint = Some(event.fingerprint);
    state.next_sequence = state
        .next_sequence
        .checked_add(1)
        .ok_or(CampaignError::EventLimit)?;
    Ok(())
}

fn apply_event_kind(
    spec: &FrozenCampaignSpec,
    state: &mut ReplayState,
    sequence: u32,
    event_fingerprint: BlobId,
    kind: &CampaignEventKind,
) -> Result<(), CampaignError> {
    match kind {
        CampaignEventKind::Prepared {
            campaign_fingerprint,
            store_lease_fingerprint: _,
        } => {
            if state.initialized || sequence != 0 || *campaign_fingerprint != spec.fingerprint() {
                return Err(CampaignError::InvalidPrepared);
            }
            state.initialized = true;
            state.status = Some(CampaignStatus::Prepared);
        }
        CampaignEventKind::Started => {
            transition(state, CampaignStatus::Prepared, CampaignStatus::Running)?;
        }
        CampaignEventKind::PauseRequested { .. } => {
            transition(
                state,
                CampaignStatus::Running,
                CampaignStatus::PauseRequested,
            )?;
        }
        CampaignEventKind::Paused => {
            if has_active_attempts(state) {
                return Err(CampaignError::ActiveAttempts);
            }
            transition(
                state,
                CampaignStatus::PauseRequested,
                CampaignStatus::Paused,
            )?;
        }
        CampaignEventKind::Resumed => {
            transition(state, CampaignStatus::Paused, CampaignStatus::Running)?;
        }
        CampaignEventKind::CancelRequested { .. } => {
            apply_cancel_requested(state)?;
        }
        CampaignEventKind::TrialReserved {
            attempt_id,
            trial_fingerprint,
            attempt_ordinal,
            reservation,
        } => apply_trial_reserved(
            spec,
            state,
            *attempt_id,
            *trial_fingerprint,
            *attempt_ordinal,
            *reservation,
            event_fingerprint,
        )?,
        CampaignEventKind::TrialDispatched { attempt_id } => {
            apply_trial_dispatched(state, *attempt_id, event_fingerprint)?;
        }
        CampaignEventKind::TrialFinished {
            attempt_id,
            terminal,
            actual_charge,
            live_terminal_evidence_fingerprint: _,
        } => apply_trial_finished(spec, state, *attempt_id, *terminal, *actual_charge)?,
        CampaignEventKind::TrialReservationReleased { attempt_id, .. } => {
            apply_reservation_released(state, *attempt_id)?;
        }
        CampaignEventKind::SearchDecisionRecorded { receipt } => {
            apply_search_decision(spec, state, receipt)?;
        }
        CampaignEventKind::CampaignClosed {
            disposition,
            diagnostic_fingerprint,
            scheduler_evidence_fingerprint,
        } => {
            apply_campaign_closed(
                state,
                *disposition,
                *diagnostic_fingerprint,
                *scheduler_evidence_fingerprint,
            )?;
        }
    }
    Ok(())
}

fn apply_cancel_requested(state: &mut ReplayState) -> Result<(), CampaignError> {
    let from = status(state)?;
    if !matches!(
        from,
        CampaignStatus::Prepared
            | CampaignStatus::Running
            | CampaignStatus::PauseRequested
            | CampaignStatus::Paused
    ) {
        return Err(CampaignError::InvalidTransition {
            from,
            to: CampaignStatus::CancelRequested,
        });
    }
    state.status = Some(CampaignStatus::CancelRequested);
    Ok(())
}

fn apply_trial_reserved(
    spec: &FrozenCampaignSpec,
    state: &mut ReplayState,
    attempt_id: TrialAttemptId,
    trial_fingerprint: BlobId,
    attempt_ordinal: u16,
    reservation: CampaignBudgetAmount,
    reservation_event_fingerprint: BlobId,
) -> Result<(), CampaignError> {
    require_status(state, CampaignStatus::Running)?;
    if state.attempts.len() >= MAX_CAMPAIGN_ATTEMPTS {
        return Err(CampaignError::AttemptLimit);
    }
    if state.attempts.contains_key(&attempt_id) {
        return Err(CampaignError::DuplicateAttemptId(attempt_id));
    }
    let trial = spec
        .trial(trial_fingerprint)
        .ok_or(CampaignError::UnknownTrial(trial_fingerprint))?;
    if !state.scheduled_trials.contains(&trial_fingerprint) {
        return Err(CampaignError::TrialNotScheduled(trial_fingerprint));
    }
    if state.eliminated_trials.contains(&trial_fingerprint) {
        return Err(CampaignError::TrialEliminated(trial_fingerprint));
    }
    if state.stopped_trials.contains(&trial_fingerprint) {
        return Err(CampaignError::TrialPressureStopped(trial_fingerprint));
    }
    if state.successful_trials.contains(&trial_fingerprint) {
        return Err(CampaignError::TrialAlreadyCompleted(trial_fingerprint));
    }
    let has_prior_attempt = state
        .attempts
        .values()
        .any(|attempt| attempt.trial_fingerprint == trial_fingerprint);
    if !has_prior_attempt
        && let Some(expected) = state.scheduled_order.iter().copied().find(|candidate| {
            !state
                .attempts
                .values()
                .any(|attempt| attempt.trial_fingerprint == *candidate)
                && !state.eliminated_trials.contains(candidate)
                && !state.stopped_trials.contains(candidate)
        })
        && expected != trial_fingerprint
    {
        return Err(CampaignError::TrialReservationOutOfOrder {
            expected,
            actual: trial_fingerprint,
        });
    }
    if state.attempts.values().any(|attempt| {
        attempt.trial_fingerprint == trial_fingerprint
            && matches!(
                attempt.state,
                AttemptState::Reserved | AttemptState::Dispatched
            )
    }) {
        return Err(CampaignError::TrialAttemptActive(trial_fingerprint));
    }
    for dependency in trial.dependencies() {
        if !state.successful_trials.contains(dependency) {
            return Err(CampaignError::DependencyNotSatisfied {
                trial: trial_fingerprint,
                dependency: *dependency,
            });
        }
    }
    let expected_ordinal = state
        .attempts
        .values()
        .filter(|attempt| attempt.trial_fingerprint == trial_fingerprint)
        .count()
        + 1;
    if usize::from(attempt_ordinal) != expected_ordinal {
        return Err(CampaignError::AttemptOrdinal);
    }
    if reservation != trial.budget_maximum() {
        return Err(CampaignError::ReservationMismatch);
    }
    state.budget.reserve(spec.budget_limits(), reservation)?;
    state.attempts.insert(
        attempt_id,
        AttemptRecord {
            trial_fingerprint,
            reservation,
            reservation_event_fingerprint,
            dispatch_event_fingerprint: None,
            state: AttemptState::Reserved,
        },
    );
    Ok(())
}

fn apply_trial_dispatched(
    state: &mut ReplayState,
    attempt_id: TrialAttemptId,
    dispatch_event_fingerprint: BlobId,
) -> Result<(), CampaignError> {
    require_status(state, CampaignStatus::Running)?;
    let attempt = state
        .attempts
        .get_mut(&attempt_id)
        .ok_or(CampaignError::AttemptNotFound(attempt_id))?;
    if attempt.state != AttemptState::Reserved {
        return Err(CampaignError::AttemptNotReserved(attempt_id));
    }
    attempt.state = AttemptState::Dispatched;
    attempt.dispatch_event_fingerprint = Some(dispatch_event_fingerprint);
    Ok(())
}

fn apply_trial_finished(
    spec: &FrozenCampaignSpec,
    state: &mut ReplayState,
    attempt_id: TrialAttemptId,
    terminal: CampaignTrialTerminal,
    actual_charge: CampaignBudgetAmount,
) -> Result<(), CampaignError> {
    require_one_of(
        state,
        &[
            CampaignStatus::Running,
            CampaignStatus::PauseRequested,
            CampaignStatus::CancelRequested,
        ],
    )?;
    let attempt = state
        .attempts
        .get_mut(&attempt_id)
        .ok_or(CampaignError::AttemptNotFound(attempt_id))?;
    if attempt.state != AttemptState::Dispatched {
        return Err(CampaignError::AttemptNotDispatched(attempt_id));
    }
    if matches!(terminal, CampaignTrialTerminal::Interrupted { .. })
        && actual_charge != attempt.reservation
    {
        return Err(CampaignError::InterruptedChargeMismatch);
    }
    state
        .budget
        .reconcile(spec.budget_limits(), attempt.reservation, actual_charge)?;
    attempt.state = AttemptState::Finished(terminal);
    if matches!(terminal, CampaignTrialTerminal::Completed { .. }) {
        state.successful_trials.insert(attempt.trial_fingerprint);
        state
            .successful_charges
            .insert(attempt.trial_fingerprint, actual_charge);
    }
    Ok(())
}

fn apply_reservation_released(
    state: &mut ReplayState,
    attempt_id: TrialAttemptId,
) -> Result<(), CampaignError> {
    require_one_of(
        state,
        &[
            CampaignStatus::Running,
            CampaignStatus::PauseRequested,
            CampaignStatus::CancelRequested,
        ],
    )?;
    let attempt = state
        .attempts
        .get_mut(&attempt_id)
        .ok_or(CampaignError::AttemptNotFound(attempt_id))?;
    if attempt.state != AttemptState::Reserved {
        return Err(CampaignError::AttemptNotReserved(attempt_id));
    }
    state.budget.release(attempt.reservation)?;
    attempt.state = AttemptState::Released;
    Ok(())
}

fn apply_search_decision(
    spec: &FrozenCampaignSpec,
    state: &mut ReplayState,
    receipt: &SearchDecisionReceipt,
) -> Result<(), CampaignError> {
    require_status(state, CampaignStatus::Running)?;
    if state.decision_ids.len() >= MAX_CAMPAIGN_DECISIONS {
        return Err(CampaignError::DecisionLimit);
    }
    receipt.verify()?;
    if !state.decision_ids.insert(receipt.decision_id()) {
        return Err(CampaignError::DuplicateDecision(receipt.decision_id()));
    }
    for trial in receipt.referenced_trials() {
        if spec.trial(*trial).is_none() {
            return Err(CampaignError::DecisionUnknownTrial(*trial));
        }
    }
    if !state
        .decision_artifacts
        .insert(receipt.effect().artifact_fingerprint())
    {
        return Err(CampaignError::DuplicateDecisionArtifact(
            receipt.effect().artifact_fingerprint(),
        ));
    }
    apply_search_effect(spec, state, receipt.effect(), receipt.fingerprint())?;
    Ok(())
}

fn apply_search_effect(
    spec: &FrozenCampaignSpec,
    state: &mut ReplayState,
    effect: &SearchDecisionEffect,
    receipt_fingerprint: BlobId,
) -> Result<(), CampaignError> {
    match effect {
        SearchDecisionEffect::BlockedFactorialScheduled { .. } => {
            apply_factorial_effect(spec, state, effect)?;
        }
        SearchDecisionEffect::NestedPoolRecorded { .. } => {
            apply_nested_pool_effect(spec, state, effect)?;
        }
        SearchDecisionEffect::SuccessiveHalvingApplied { .. } => {
            apply_halving_effect(spec, state, effect, receipt_fingerprint)?;
        }
        SearchDecisionEffect::PressureAdvanced { .. }
        | SearchDecisionEffect::PressureStopped { .. } => {
            apply_pressure_effect(state, effect, receipt_fingerprint)?;
        }
        SearchDecisionEffect::MapElitesInitialized { .. }
        | SearchDecisionEffect::MapElitesAdvanced { .. } => {
            apply_archive_effect(state, effect)?;
        }
    }
    Ok(())
}

fn apply_factorial_effect(
    spec: &FrozenCampaignSpec,
    state: &mut ReplayState,
    effect: &SearchDecisionEffect,
) -> Result<(), CampaignError> {
    let SearchDecisionEffect::BlockedFactorialScheduled {
        artifact_fingerprint,
        seed,
        seeded_trial_order,
    } = effect
    else {
        unreachable!("factorial helper receives a factorial effect")
    };
    if *seed != spec.seed() || seeded_trial_order.len() < 2 {
        return Err(CampaignError::FactorialBinding);
    }
    verify_seeded_topological_effect(spec, state, *artifact_fingerprint, seeded_trial_order)?;
    let mut case_id = None;
    let mut budget = None;
    for trial_fingerprint in &**seeded_trial_order {
        let node = spec
            .trial(*trial_fingerprint)
            .ok_or(CampaignError::DecisionUnknownTrial(*trial_fingerprint))?;
        if case_id
            .replace(node.case_id())
            .is_some_and(|id| id != node.case_id())
        {
            return Err(CampaignError::UnequalCaseBlock);
        }
        if budget
            .replace(node.budget_maximum())
            .is_some_and(|value| value != node.budget_maximum())
        {
            return Err(CampaignError::UnequalTrialBudgets);
        }
        if !state.scheduled_trials.insert(*trial_fingerprint) {
            return Err(CampaignError::TrialAlreadyScheduled(*trial_fingerprint));
        }
        state.scheduled_order.push(*trial_fingerprint);
    }
    Ok(())
}

fn apply_nested_pool_effect(
    spec: &FrozenCampaignSpec,
    state: &mut ReplayState,
    effect: &SearchDecisionEffect,
) -> Result<(), CampaignError> {
    let SearchDecisionEffect::NestedPoolRecorded {
        case_id,
        treatment_fingerprint,
        ..
    } = effect
    else {
        unreachable!("nested-pool helper receives a nested-pool effect")
    };
    if !spec.trials().iter().any(|node| {
        node.case_id() == *case_id
            && node.treatment_fingerprint() == *treatment_fingerprint
            && state.successful_trials.contains(&node.trial_fingerprint())
    }) {
        return Err(CampaignError::PoolBinding);
    }
    if !state
        .nested_pools
        .insert((*case_id, *treatment_fingerprint))
    {
        return Err(CampaignError::DuplicateNestedPool);
    }
    Ok(())
}

fn apply_halving_effect(
    spec: &FrozenCampaignSpec,
    state: &mut ReplayState,
    effect: &SearchDecisionEffect,
    receipt_fingerprint: BlobId,
) -> Result<(), CampaignError> {
    let SearchDecisionEffect::SuccessiveHalvingApplied {
        rung,
        case_id,
        prior_decision_fingerprint: _,
        evaluation_coverage_fingerprint,
        equal_budget_fingerprint,
        survivors,
        eliminated,
        ..
    } = effect
    else {
        unreachable!("halving helper receives a halving effect")
    };
    if *rung == 0 || survivors.is_empty() || eliminated.is_empty() {
        return Err(CampaignError::HalvingRungOutOfOrder);
    }
    verify_halving_prior(state, effect)?;
    for trial in survivors.iter().chain(eliminated.iter()) {
        let node = spec
            .trial(*trial)
            .ok_or(CampaignError::DecisionUnknownTrial(*trial))?;
        if node.case_id() != *case_id
            || !state.successful_trials.contains(trial)
            || fingerprint_budget(node.budget_maximum()) != *equal_budget_fingerprint
        {
            return Err(CampaignError::HalvingEvidenceMismatch);
        }
    }
    state.eliminated_trials.extend(eliminated.iter().copied());
    state.halving.insert(
        *case_id,
        HalvingProgress {
            rung: *rung,
            decision_fingerprint: receipt_fingerprint,
            survivors: survivors.iter().copied().collect(),
            evaluation_coverage_fingerprint: *evaluation_coverage_fingerprint,
            equal_budget_fingerprint: *equal_budget_fingerprint,
        },
    );
    Ok(())
}

fn verify_halving_prior(
    state: &ReplayState,
    effect: &SearchDecisionEffect,
) -> Result<(), CampaignError> {
    let SearchDecisionEffect::SuccessiveHalvingApplied {
        rung,
        case_id,
        prior_decision_fingerprint,
        evaluation_coverage_fingerprint,
        equal_budget_fingerprint,
        survivors,
        eliminated,
        ..
    } = effect
    else {
        unreachable!("halving-prior helper receives a halving effect")
    };
    let prior = state.halving.get(case_id);
    if *rung == 1 && prior.is_none() && prior_decision_fingerprint.is_none() {
        return Ok(());
    }
    let prior = prior.ok_or(CampaignError::HalvingRungOutOfOrder)?;
    let candidates = survivors
        .iter()
        .chain(eliminated.iter())
        .copied()
        .collect::<BTreeSet<_>>();
    if prior.rung.checked_add(1) != Some(*rung)
        || Some(prior.decision_fingerprint) != *prior_decision_fingerprint
        || candidates != prior.survivors
        || prior.evaluation_coverage_fingerprint != *evaluation_coverage_fingerprint
        || prior.equal_budget_fingerprint != *equal_budget_fingerprint
    {
        return Err(CampaignError::HalvingSurvivorMismatch);
    }
    Ok(())
}

fn apply_pressure_effect(
    state: &mut ReplayState,
    effect: &SearchDecisionEffect,
    receipt_fingerprint: BlobId,
) -> Result<(), CampaignError> {
    match effect {
        SearchDecisionEffect::PressureAdvanced {
            family_fingerprint,
            observed_level,
            next_level,
            affected_trials,
            evaluation_coverage_fingerprint,
            cumulative_compute,
            prior_decision_fingerprint,
            ..
        } => {
            verify_pressure_progress(
                state,
                *family_fingerprint,
                *observed_level,
                *prior_decision_fingerprint,
                affected_trials,
                *evaluation_coverage_fingerprint,
                *cumulative_compute,
            )?;
            if *next_level <= *observed_level {
                return Err(CampaignError::PressureSequenceMismatch);
            }
            store_pressure_progress(
                state,
                *family_fingerprint,
                receipt_fingerprint,
                Some(*next_level),
                affected_trials,
                *evaluation_coverage_fingerprint,
                *cumulative_compute,
            );
        }
        SearchDecisionEffect::PressureStopped {
            family_fingerprint,
            observed_level,
            stopped_trials,
            evaluation_coverage_fingerprint,
            cumulative_compute,
            prior_decision_fingerprint,
            ..
        } => {
            verify_pressure_progress(
                state,
                *family_fingerprint,
                *observed_level,
                *prior_decision_fingerprint,
                stopped_trials,
                *evaluation_coverage_fingerprint,
                *cumulative_compute,
            )?;
            state.stopped_trials.extend(stopped_trials.iter().copied());
            store_pressure_progress(
                state,
                *family_fingerprint,
                receipt_fingerprint,
                None,
                stopped_trials,
                *evaluation_coverage_fingerprint,
                *cumulative_compute,
            );
        }
        _ => unreachable!("pressure helper receives only pressure effects"),
    }
    Ok(())
}

fn store_pressure_progress(
    state: &mut ReplayState,
    family_fingerprint: BlobId,
    decision_fingerprint: BlobId,
    next_level: Option<u16>,
    affected_trials: &[BlobId],
    coverage: BlobId,
    cumulative_compute: u64,
) {
    state.pressure.insert(
        family_fingerprint,
        PressureProgress {
            decision_fingerprint,
            next_level,
            affected_trials: affected_trials.iter().copied().collect(),
            evaluation_coverage_fingerprint: coverage,
            cumulative_compute,
        },
    );
}

fn apply_archive_effect(
    state: &mut ReplayState,
    effect: &SearchDecisionEffect,
) -> Result<(), CampaignError> {
    match effect {
        SearchDecisionEffect::MapElitesInitialized {
            artifact_fingerprint,
            generation,
        } => {
            if *generation != 0 || state.archive.is_some() {
                return Err(CampaignError::ArchiveSequenceMismatch);
            }
            state.archive = Some(ArchiveProgress {
                generation: 0,
                fingerprint: *artifact_fingerprint,
            });
        }
        SearchDecisionEffect::MapElitesAdvanced {
            artifact_fingerprint,
            generation,
            parent_fingerprint,
            ..
        } => {
            let prior = state.archive.ok_or(CampaignError::ArchiveNotInitialized)?;
            if *parent_fingerprint != prior.fingerprint
                || prior.generation.checked_add(1) != Some(*generation)
            {
                return Err(CampaignError::ArchiveSequenceMismatch);
            }
            state.archive = Some(ArchiveProgress {
                generation: *generation,
                fingerprint: *artifact_fingerprint,
            });
        }
        _ => unreachable!("archive helper receives only archive effects"),
    }
    Ok(())
}

fn verify_seeded_topological_effect(
    spec: &FrozenCampaignSpec,
    state: &ReplayState,
    plan_fingerprint: BlobId,
    order: &[BlobId],
) -> Result<(), CampaignError> {
    let mut pending = order.iter().copied().collect::<BTreeSet<_>>();
    if pending.len() != order.len() {
        return Err(CampaignError::FactorialBinding);
    }
    let mut available_dependencies = state.scheduled_trials.clone();
    available_dependencies.extend(state.successful_trials.iter().copied());
    for actual in order {
        let mut ready = pending
            .iter()
            .copied()
            .filter(|trial_fingerprint| {
                spec.trial(*trial_fingerprint).is_some_and(|node| {
                    node.dependencies().iter().all(|dependency| {
                        !pending.contains(dependency) && available_dependencies.contains(dependency)
                    })
                })
            })
            .collect::<Vec<_>>();
        ready.sort_unstable_by_key(|trial| seeded_order_key(spec.seed(), plan_fingerprint, *trial));
        if ready.first() != Some(actual) {
            return Err(CampaignError::FactorialBinding);
        }
        pending.remove(actual);
        available_dependencies.insert(*actual);
    }
    Ok(())
}

fn verify_pressure_progress(
    state: &ReplayState,
    family_fingerprint: BlobId,
    observed_level: u16,
    prior_decision_fingerprint: Option<BlobId>,
    affected_trials: &[BlobId],
    evaluation_coverage_fingerprint: BlobId,
    cumulative_compute: u64,
) -> Result<(), CampaignError> {
    let affected = affected_trials.iter().copied().collect::<BTreeSet<_>>();
    if affected.is_empty() || affected.len() != affected_trials.len() {
        return Err(CampaignError::PressureTrialSet);
    }
    if let Some(trial) = affected
        .iter()
        .find(|trial| !state.scheduled_trials.contains(trial))
    {
        return Err(CampaignError::TrialNotScheduled(*trial));
    }
    if cumulative_compute == 0 {
        return Err(CampaignError::PressureChargeMismatch);
    }
    match state.pressure.get(&family_fingerprint) {
        None if prior_decision_fingerprint.is_none() => Ok(()),
        Some(prior)
            if Some(prior.decision_fingerprint) == prior_decision_fingerprint
                && prior.next_level == Some(observed_level)
                && prior.affected_trials.is_subset(&affected)
                && prior.evaluation_coverage_fingerprint == evaluation_coverage_fingerprint
                && cumulative_compute > prior.cumulative_compute =>
        {
            Ok(())
        }
        _ => Err(CampaignError::PressureSequenceMismatch),
    }
}

fn apply_campaign_closed(
    state: &mut ReplayState,
    disposition: CampaignDisposition,
    diagnostic_fingerprint: Option<BlobId>,
    scheduler_evidence_fingerprint: Option<BlobId>,
) -> Result<(), CampaignError> {
    if has_active_attempts(state) {
        return Err(CampaignError::ActiveAttempts);
    }
    let (expected_state, valid_from) = match disposition {
        CampaignDisposition::Completed => {
            (CampaignStatus::Completed, &[CampaignStatus::Running][..])
        }
        CampaignDisposition::Failed => (
            CampaignStatus::Failed,
            &[
                CampaignStatus::Prepared,
                CampaignStatus::Running,
                CampaignStatus::PauseRequested,
                CampaignStatus::Paused,
                CampaignStatus::CancelRequested,
            ][..],
        ),
        CampaignDisposition::Cancelled => (
            CampaignStatus::Cancelled,
            &[CampaignStatus::CancelRequested][..],
        ),
    };
    require_one_of(state, valid_from)?;
    if (disposition == CampaignDisposition::Completed) != diagnostic_fingerprint.is_none() {
        return Err(CampaignError::InvalidDispositionEvidence);
    }
    if (disposition == CampaignDisposition::Completed) != scheduler_evidence_fingerprint.is_some() {
        return Err(CampaignError::InvalidDispositionEvidence);
    }
    if disposition == CampaignDisposition::Completed {
        verify_scheduler_terminal_state(state)?;
    }
    state.status = Some(expected_state);
    Ok(())
}

fn status(state: &ReplayState) -> Result<CampaignStatus, CampaignError> {
    state.status.ok_or(CampaignError::MissingPrepared)
}

fn transition(
    state: &mut ReplayState,
    expected: CampaignStatus,
    next: CampaignStatus,
) -> Result<(), CampaignError> {
    require_status(state, expected)?;
    state.status = Some(next);
    Ok(())
}

fn require_status(state: &ReplayState, expected: CampaignStatus) -> Result<(), CampaignError> {
    let actual = status(state)?;
    if actual != expected {
        return Err(CampaignError::InvalidTransition {
            from: actual,
            to: expected,
        });
    }
    Ok(())
}

fn require_one_of(state: &ReplayState, expected: &[CampaignStatus]) -> Result<(), CampaignError> {
    let actual = status(state)?;
    if actual.is_terminal() {
        return Err(CampaignError::CampaignTerminal);
    }
    if !expected.contains(&actual) {
        return Err(CampaignError::InvalidTransition {
            from: actual,
            to: expected[0],
        });
    }
    Ok(())
}

fn has_active_attempts(state: &ReplayState) -> bool {
    state.attempts.values().any(|attempt| {
        matches!(
            attempt.state,
            AttemptState::Reserved | AttemptState::Dispatched
        )
    })
}

fn verify_scheduler_terminal_state(state: &ReplayState) -> Result<(), CampaignError> {
    require_status(state, CampaignStatus::Running)?;
    if has_active_attempts(state) {
        return Err(CampaignError::ActiveAttempts);
    }
    if state.successful_trials.is_empty() {
        return Err(CampaignError::NoSuccessfulTrial);
    }
    let unfinished = state.scheduled_trials.iter().any(|trial| {
        !state.successful_trials.contains(trial)
            && !state.eliminated_trials.contains(trial)
            && !state.stopped_trials.contains(trial)
    });
    if unfinished
        || state
            .pressure
            .values()
            .any(|progress| progress.next_level.is_some())
        || state
            .halving
            .values()
            .any(|progress| progress.survivors.len() > 1)
        || state.archive.is_some_and(|archive| archive.generation == 0)
    {
        return Err(CampaignError::RequiredWorkRemaining);
    }
    Ok(())
}

fn scheduler_terminal_evidence_fingerprint(
    campaign_fingerprint: BlobId,
    session_id: SchedulerSessionId,
    event_head_fingerprint: BlobId,
    state: &ReplayState,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/campaign-scheduler-terminal/v1\0");
    digest.update(campaign_fingerprint.as_bytes());
    digest.update(session_id.as_artifact_id().as_ulid().to_bytes());
    digest.update(event_head_fingerprint.as_bytes());
    for set in [
        &state.scheduled_trials,
        &state.successful_trials,
        &state.eliminated_trials,
        &state.stopped_trials,
    ] {
        digest.update((set.len() as u64).to_be_bytes());
        for fingerprint in set {
            digest.update(fingerprint.as_bytes());
        }
    }
    digest.update((state.pressure.len() as u64).to_be_bytes());
    for (family, progress) in &state.pressure {
        digest.update(family.as_bytes());
        digest.update(progress.decision_fingerprint.as_bytes());
        match progress.next_level {
            Some(level) => {
                digest.update([1]);
                digest.update(level.to_be_bytes());
            }
            None => digest.update([0]),
        }
    }
    digest.update((state.halving.len() as u64).to_be_bytes());
    for (case_id, progress) in &state.halving {
        digest.update(case_id.as_ulid().to_bytes());
        digest.update(progress.rung.to_be_bytes());
        digest.update(progress.decision_fingerprint.as_bytes());
        digest.update((progress.survivors.len() as u64).to_be_bytes());
        for survivor in &progress.survivors {
            digest.update(survivor.as_bytes());
        }
    }
    match state.archive {
        Some(archive) => {
            digest.update([1]);
            digest.update(archive.generation.to_be_bytes());
            digest.update(archive.fingerprint.as_bytes());
        }
        None => digest.update([0]),
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn public_attempt_status(state: AttemptState) -> CampaignTrialAttemptStatus {
    match state {
        AttemptState::Reserved => CampaignTrialAttemptStatus::Reserved,
        AttemptState::Dispatched => CampaignTrialAttemptStatus::Dispatched,
        AttemptState::Finished(CampaignTrialTerminal::Completed { .. }) => {
            CampaignTrialAttemptStatus::Completed
        }
        AttemptState::Finished(CampaignTrialTerminal::Failed { .. }) => {
            CampaignTrialAttemptStatus::Failed
        }
        AttemptState::Finished(CampaignTrialTerminal::Cancelled { .. }) => {
            CampaignTrialAttemptStatus::Cancelled
        }
        AttemptState::Finished(CampaignTrialTerminal::Interrupted { .. }) => {
            CampaignTrialAttemptStatus::Interrupted
        }
        AttemptState::Released => CampaignTrialAttemptStatus::Released,
    }
}

fn snapshot_from(state: &ReplayState) -> CampaignSnapshot {
    let active_attempt_count = state
        .attempts
        .values()
        .filter(|attempt| {
            matches!(
                attempt.state,
                AttemptState::Reserved | AttemptState::Dispatched
            )
        })
        .count();
    CampaignSnapshot {
        status: state.status.expect("prepared campaign status"),
        budget: state.budget,
        attempt_count: state.attempts.len(),
        active_attempt_count,
        successful_trial_count: state.successful_trials.len(),
        decision_count: state.decision_ids.len(),
        scheduled_trial_count: state.scheduled_trials.len(),
        eliminated_trial_count: state.eliminated_trials.len(),
        stopped_trial_count: state.stopped_trials.len(),
        archive_generation: state.archive.map(|archive| archive.generation),
        last_event_fingerprint: state
            .last_event_fingerprint
            .expect("prepared campaign event fingerprint"),
    }
}

fn bounded_trials(
    values: Vec<BlobId>,
) -> Result<BoundedVec<BlobId, MAX_DECISION_TRIAL_REFERENCES>, CampaignError> {
    let actual = values.len();
    BoundedVec::new(values).map_err(|_| {
        CampaignError::Decision(SearchDecisionError::InvalidTrialReferenceCount(actual))
    })
}

fn seeded_order_key(seed: u64, plan_fingerprint: BlobId, trial_fingerprint: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/campaign-seeded-trial-order/v1\0");
    digest.update(seed.to_be_bytes());
    digest.update(plan_fingerprint.as_bytes());
    digest.update(trial_fingerprint.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_budget(amount: CampaignBudgetAmount) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/campaign-equal-budget/v1\0");
    amount.update_digest(&mut digest);
    BlobId::from_bytes(digest.finalize().into())
}

pub(crate) fn compute_units(charge: CampaignBudgetAmount) -> u64 {
    charge
        .writer_tokens()
        .checked_add(charge.controller_tokens())
        .and_then(|value| value.checked_add(u64::from(charge.evaluations())))
        .and_then(|value| value.checked_add(charge.wall_time_ms()))
        .expect("bounded campaign resource maxima cannot overflow u64")
}

fn fingerprint_pressure_charges(charges: &BTreeMap<BlobId, CampaignBudgetAmount>) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/pressure-execution-charges/v1\0");
    digest.update((charges.len() as u64).to_be_bytes());
    for (trial, charge) in charges {
        digest.update(trial.as_bytes());
        charge.update_digest(&mut digest);
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_event(sequence: u32, previous: Option<BlobId>, kind: &CampaignEventKind) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(EVENT_DOMAIN);
    digest.update(sequence.to_be_bytes());
    match previous {
        Some(previous) => {
            digest.update([1]);
            digest.update(previous.as_bytes());
        }
        None => digest.update([0]),
    }
    match kind {
        CampaignEventKind::Prepared {
            campaign_fingerprint,
            store_lease_fingerprint,
        } => {
            digest.update([0]);
            digest.update(campaign_fingerprint.as_bytes());
            digest.update(store_lease_fingerprint.as_bytes());
        }
        CampaignEventKind::Started => digest.update([1]),
        CampaignEventKind::PauseRequested { reason_fingerprint } => {
            digest.update([2]);
            digest.update(reason_fingerprint.as_bytes());
        }
        CampaignEventKind::Paused => digest.update([3]),
        CampaignEventKind::Resumed => digest.update([4]),
        CampaignEventKind::CancelRequested { reason_fingerprint } => {
            digest.update([5]);
            digest.update(reason_fingerprint.as_bytes());
        }
        CampaignEventKind::TrialReserved {
            attempt_id,
            trial_fingerprint,
            attempt_ordinal,
            reservation,
        } => {
            digest.update([6]);
            update_attempt_id(&mut digest, *attempt_id);
            digest.update(trial_fingerprint.as_bytes());
            digest.update(attempt_ordinal.to_be_bytes());
            reservation.update_digest(&mut digest);
        }
        CampaignEventKind::TrialDispatched { attempt_id } => {
            digest.update([7]);
            update_attempt_id(&mut digest, *attempt_id);
        }
        CampaignEventKind::TrialFinished {
            attempt_id,
            terminal,
            actual_charge,
            live_terminal_evidence_fingerprint,
        } => {
            digest.update([8]);
            update_attempt_id(&mut digest, *attempt_id);
            digest.update([terminal.tag()]);
            digest.update(terminal.evidence().as_bytes());
            actual_charge.update_digest(&mut digest);
            digest.update(live_terminal_evidence_fingerprint.as_bytes());
        }
        CampaignEventKind::TrialReservationReleased {
            attempt_id,
            diagnostic_fingerprint,
        } => {
            digest.update([9]);
            update_attempt_id(&mut digest, *attempt_id);
            digest.update(diagnostic_fingerprint.as_bytes());
        }
        CampaignEventKind::SearchDecisionRecorded { receipt } => {
            digest.update([10]);
            digest.update(receipt.fingerprint().as_bytes());
        }
        CampaignEventKind::CampaignClosed {
            disposition,
            diagnostic_fingerprint,
            scheduler_evidence_fingerprint,
        } => {
            digest.update([11, disposition_tag(*disposition)]);
            match diagnostic_fingerprint {
                Some(fingerprint) => {
                    digest.update([1]);
                    digest.update(fingerprint.as_bytes());
                }
                None => digest.update([0]),
            }
            match scheduler_evidence_fingerprint {
                Some(fingerprint) => {
                    digest.update([1]);
                    digest.update(fingerprint.as_bytes());
                }
                None => digest.update([0]),
            }
        }
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn decode_persisted_campaign_events(
    spec: &FrozenCampaignSpec,
    persisted: &[PersistedResearchJournalEvent],
) -> Result<Vec<CampaignEvent>, CampaignError> {
    if persisted.len() > MAX_CAMPAIGN_EVENTS {
        return Err(CampaignError::EventLimit);
    }
    let mut events = Vec::with_capacity(persisted.len());
    for row in persisted {
        let record: CanonicalCampaignEventRecord =
            serde_json::from_slice(row.canonical_record_bytes())?;
        if serde_json::to_vec(&record)? != row.canonical_record_bytes()
            || record.format != CAMPAIGN_EVENT_RECORD_FORMAT
            || record.project_id != spec.project_id()
            || record.campaign_fingerprint != spec.fingerprint()
            || record.event.sequence != row.event_index()
            || record.event.previous_event_fingerprint != row.previous_event_fingerprint()
            || record.event.fingerprint != row.event_fingerprint()
            || BlobId::digest(row.canonical_record_bytes()) != row.record_fingerprint()
        {
            return Err(CampaignError::MalformedJournalRecord);
        }
        if let CampaignEventKind::Prepared {
            store_lease_fingerprint,
            ..
        } = &record.event.kind
            && *store_lease_fingerprint != record.store_lease_fingerprint
        {
            return Err(CampaignError::MalformedJournalRecord);
        }
        events.push(record.event);
    }
    Ok(events)
}

const fn research_journal_budget(amount: CampaignBudgetAmount) -> ResearchJournalBudget {
    ResearchJournalBudget {
        writer_tokens: amount.writer_tokens(),
        controller_tokens: amount.controller_tokens(),
        evaluations: amount.evaluations(),
        wall_time_ms: amount.wall_time_ms(),
    }
}

const fn persisted_campaign_outcome(terminal: CampaignTrialTerminal) -> CampaignTrialOutcome {
    match terminal {
        CampaignTrialTerminal::Completed { .. } => CampaignTrialOutcome::Completed,
        CampaignTrialTerminal::Failed { .. } => CampaignTrialOutcome::Failed,
        CampaignTrialTerminal::Cancelled { .. } => CampaignTrialOutcome::Cancelled,
        CampaignTrialTerminal::Interrupted { .. } => CampaignTrialOutcome::Interrupted,
    }
}

const fn persisted_search_decision_kind(
    effect: &SearchDecisionEffect,
) -> (SearchDecisionPersistenceKind, Option<BlobId>) {
    match effect {
        SearchDecisionEffect::BlockedFactorialScheduled { .. } => (
            SearchDecisionPersistenceKind::BlockedFactorialScheduled,
            None,
        ),
        SearchDecisionEffect::NestedPoolRecorded { .. } => {
            (SearchDecisionPersistenceKind::NestedPoolRecorded, None)
        }
        SearchDecisionEffect::SuccessiveHalvingApplied { .. } => (
            SearchDecisionPersistenceKind::SuccessiveHalvingApplied,
            None,
        ),
        SearchDecisionEffect::PressureAdvanced { .. } => {
            (SearchDecisionPersistenceKind::PressureAdvanced, None)
        }
        SearchDecisionEffect::PressureStopped { .. } => {
            (SearchDecisionPersistenceKind::PressureStopped, None)
        }
        SearchDecisionEffect::MapElitesInitialized { .. } => {
            (SearchDecisionPersistenceKind::MapElitesInitialized, None)
        }
        SearchDecisionEffect::MapElitesAdvanced {
            parent_fingerprint, ..
        } => (
            SearchDecisionPersistenceKind::MapElitesAdvanced,
            Some(*parent_fingerprint),
        ),
    }
}

fn recovery_diagnostic(
    campaign_fingerprint: BlobId,
    event_head: BlobId,
    attempt_id: TrialAttemptId,
    state: AttemptState,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(RECOVERY_DIAGNOSTIC_DOMAIN);
    digest.update(campaign_fingerprint.as_bytes());
    digest.update(event_head.as_bytes());
    update_attempt_id(&mut digest, attempt_id);
    match state {
        AttemptState::Reserved => digest.update([0]),
        AttemptState::Dispatched => digest.update([1]),
        AttemptState::Finished(terminal) => {
            digest.update([2, terminal.tag()]);
            digest.update(terminal.evidence().as_bytes());
        }
        AttemptState::Released => digest.update([3]),
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn update_attempt_id(digest: &mut Sha256, attempt_id: TrialAttemptId) {
    digest.update(attempt_id.as_trial_run_id().as_ulid().to_bytes());
}

const fn disposition_tag(disposition: CampaignDisposition) -> u8 {
    match disposition {
        CampaignDisposition::Completed => 0,
        CampaignDisposition::Failed => 1,
        CampaignDisposition::Cancelled => 2,
    }
}
