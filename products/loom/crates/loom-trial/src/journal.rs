use std::collections::BTreeMap;

use loom_research_types::{
    BoundedVec, FrozenStageSpec, FrozenTrialStage, StageAttemptId, StageGraph, StageId, TrialRunId,
    TrialRunRecord,
};
use loom_store::{
    ExclusiveResearchSessionLease, PersistedResearchJournalEvent, PersistedResearchSubjectSnapshot,
    ProjectStore, ResearchBudgetMaximum, ResearchJournalBudget, ResearchJournalWriter,
    ResearchSessionKind, TrialJournalEventPersistence, TrialJournalMutation, TrialStageOutcome,
};
use loom_types::{ArtifactId, BlobId, ProjectId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    BudgetAmount, BudgetError, BudgetLedger, FrozenTrialSpec, ReservedStagePermit,
    TrialCompletionRequest, TrialSessionId, TrialSpecError, VerifiedArchiveCompletionLease,
    VerifiedCompletedTrialLease, VerifiedStageTerminalLease, canonical_stage_record_fingerprint,
    fingerprint_stage_graph, stage_kind_tag,
};

pub const MAX_STAGE_ATTEMPTS_PER_TRIAL: usize = 1_024;
pub const MAX_TRIAL_EVENTS: usize = 4_096;

const STAGE_INPUT_DOMAIN: &[u8] = b"loom/frozen-stage-input/v1\0";
const STAGE_COMMAND_DOMAIN: &[u8] = b"loom/frozen-stage-command/v1\0";
const TRIAL_EVENT_DOMAIN: &[u8] = b"loom/frozen-trial-event/v1\0";
const TRIAL_EVENT_RECORD_FORMAT: &str = "loom.trial-event.v1";
const STAGE_ATTEMPT_RECORD_FORMAT: &str = "loom.stage-attempt.v1";
const STAGE_RESERVATION_RECORD_FORMAT: &str = "loom.stage-budget-reservation.v1";
const STAGE_CHARGE_RECORD_FORMAT: &str = "loom.stage-budget-charge.v1";
const RECOVERY_DIAGNOSTIC_DOMAIN: &[u8] = b"loom/trial-crash-recovery/v1\0";

/// Whether a frozen stage may spend model-call budget.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialWorkClass {
    Pure,
    OptionalController,
    Writer,
    Evaluation,
}

impl TrialWorkClass {
    pub const fn for_stage(stage: FrozenTrialStage) -> Self {
        match stage {
            FrozenTrialStage::BacktranslateMask | FrozenTrialStage::Plan => {
                Self::OptionalController
            }
            FrozenTrialStage::Generate => Self::Writer,
            FrozenTrialStage::Evaluate => Self::Evaluation,
            FrozenTrialStage::FreezeInputs
            | FrozenTrialStage::Retrieve
            | FrozenTrialStage::CompilePrompt
            | FrozenTrialStage::Admit
            | FrozenTrialStage::Assemble
            | FrozenTrialStage::Gate
            | FrozenTrialStage::Describe
            | FrozenTrialStage::Archive => Self::Pure,
        }
    }
}

/// Affine command issued only after a durable reservation and start event.
///
/// It is neither prose authority nor terminal inference evidence. It must be
/// consumed with a matching [`VerifiedStageTerminalLease`] or reconciled as an
/// interruption.
///
/// ```compile_fail
/// use loom_trial::StageCommand;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<StageCommand>();
/// ```
///
/// ```compile_fail
/// use loom_trial::StageCommand;
/// fn consume(_: StageCommand) {}
/// fn duplicate(command: StageCommand) {
///     consume(command);
///     consume(command);
/// }
/// ```
#[must_use]
#[derive(Debug)]
pub struct StageCommand {
    pub(crate) trial_run_id: TrialRunId,
    pub(crate) trial_fingerprint: BlobId,
    pub(crate) session_id: TrialSessionId,
    pub(crate) stage_id: StageId,
    pub(crate) stage: FrozenTrialStage,
    pub(crate) attempt_id: StageAttemptId,
    pub(crate) attempt_ordinal: u16,
    pub(crate) stage_spec_fingerprint: BlobId,
    pub(crate) input_fingerprint: BlobId,
    pub(crate) reservation: BudgetAmount,
    pub(crate) reservation_event_fingerprint: BlobId,
    pub(crate) command_fingerprint: BlobId,
    pub(crate) start_event_fingerprint: BlobId,
    pub(crate) prompt_content_fingerprint: Option<BlobId>,
    pub(crate) model_binding_fingerprint: Option<BlobId>,
}

impl StageCommand {
    pub const fn trial_run_id(&self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn stage_id(&self) -> StageId {
        self.stage_id
    }

    pub const fn stage(&self) -> FrozenTrialStage {
        self.stage
    }

    pub const fn work_class(&self) -> TrialWorkClass {
        TrialWorkClass::for_stage(self.stage)
    }

    pub const fn attempt_id(&self) -> StageAttemptId {
        self.attempt_id
    }

    pub const fn attempt_ordinal(&self) -> u16 {
        self.attempt_ordinal
    }

    pub const fn stage_spec_fingerprint(&self) -> BlobId {
        self.stage_spec_fingerprint
    }

    pub const fn input_fingerprint(&self) -> BlobId {
        self.input_fingerprint
    }

    pub const fn reservation(&self) -> BudgetAmount {
        self.reservation
    }

    pub const fn command_fingerprint(&self) -> BlobId {
        self.command_fingerprint
    }

    pub const fn start_event_fingerprint(&self) -> BlobId {
        self.start_event_fingerprint
    }

    pub const fn prompt_content_fingerprint(&self) -> Option<BlobId> {
        self.prompt_content_fingerprint
    }

    pub const fn model_binding_fingerprint(&self) -> Option<BlobId> {
        self.model_binding_fingerprint
    }

    fn into_attempt_id(self) -> StageAttemptId {
        self.attempt_id
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttemptTerminal {
    Succeeded { output_fingerprint: BlobId },
    Failed { diagnostic_fingerprint: BlobId },
    Cancelled { diagnostic_fingerprint: BlobId },
    Interrupted { diagnostic_fingerprint: BlobId },
    Abandoned { diagnostic_fingerprint: BlobId },
}

impl AttemptTerminal {
    pub const fn output_fingerprint(self) -> Option<BlobId> {
        match self {
            Self::Succeeded { output_fingerprint } => Some(output_fingerprint),
            Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. }
            | Self::Abandoned { .. } => None,
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::Succeeded { .. } => 0,
            Self::Failed { .. } => 1,
            Self::Cancelled { .. } => 2,
            Self::Interrupted { .. } => 3,
            Self::Abandoned { .. } => 4,
        }
    }

    const fn evidence_fingerprint(self) -> BlobId {
        match self {
            Self::Succeeded { output_fingerprint }
            | Self::Failed {
                diagnostic_fingerprint: output_fingerprint,
            }
            | Self::Cancelled {
                diagnostic_fingerprint: output_fingerprint,
            }
            | Self::Interrupted {
                diagnostic_fingerprint: output_fingerprint,
            }
            | Self::Abandoned {
                diagnostic_fingerprint: output_fingerprint,
            } => output_fingerprint,
        }
    }
}

/// Provenance class for the evidence attached to an attempt terminal.
///
/// A conservative interruption is not mislabeled as a live backend receipt.
/// It may only accompany `AttemptTerminal::Interrupted` and always charges the
/// complete reservation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
pub enum StageTerminalEvidence {
    VerifiedLive { receipt_fingerprint: BlobId },
    ConservativeInterruption { diagnostic_fingerprint: BlobId },
}

impl StageTerminalEvidence {
    const fn tag(self) -> u8 {
        match self {
            Self::VerifiedLive { .. } => 0,
            Self::ConservativeInterruption { .. } => 1,
        }
    }

    const fn fingerprint(self) -> BlobId {
        match self {
            Self::VerifiedLive {
                receipt_fingerprint,
            } => receipt_fingerprint,
            Self::ConservativeInterruption {
                diagnostic_fingerprint,
            } => diagnostic_fingerprint,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialDisposition {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum TrialEventKind {
    Prepared {
        trial_run_id: TrialRunId,
        trial_fingerprint: BlobId,
        store_lease_fingerprint: BlobId,
    },
    AttemptReserved {
        attempt_id: StageAttemptId,
        stage_id: StageId,
        attempt_ordinal: u16,
        reservation: BudgetAmount,
        input_fingerprint: BlobId,
    },
    AttemptStarted {
        attempt_id: StageAttemptId,
        command_fingerprint: BlobId,
    },
    AttemptFinished {
        attempt_id: StageAttemptId,
        command_fingerprint: BlobId,
        terminal: AttemptTerminal,
        actual_charge: BudgetAmount,
        terminal_evidence: StageTerminalEvidence,
    },
    AttemptAbandoned {
        attempt_id: StageAttemptId,
        diagnostic_fingerprint: BlobId,
    },
    TrialClosed {
        disposition: TrialDisposition,
        diagnostic_fingerprint: Option<BlobId>,
        live_completion_evidence_fingerprint: Option<BlobId>,
    },
}

/// One immutable event in the trial hash chain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrialEvent {
    sequence: u32,
    previous_event_fingerprint: Option<BlobId>,
    kind: TrialEventKind,
    fingerprint: BlobId,
}

/// Allocation-bounded persistence boundary for a trial event stream.
///
/// Decoding a raw `Vec<TrialEvent>` is deliberately not the supported wire
/// boundary because the allocation would precede the replay limit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TrialEventBatch(BoundedVec<TrialEvent, MAX_TRIAL_EVENTS>);

impl TrialEventBatch {
    pub fn new(events: Vec<TrialEvent>) -> Result<Self, TrialError> {
        BoundedVec::new(events)
            .map(Self)
            .map_err(|_| TrialError::EventLimit)
    }

    pub fn events(&self) -> &[TrialEvent] {
        &self.0
    }

    pub fn into_events(self) -> Vec<TrialEvent> {
        self.0.into_inner()
    }
}

impl TrialEvent {
    fn new(
        sequence: u32,
        previous_event_fingerprint: Option<BlobId>,
        kind: TrialEventKind,
    ) -> Self {
        let fingerprint = fingerprint_event(sequence, previous_event_fingerprint, kind);
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

    pub const fn kind(&self) -> TrialEventKind {
        self.kind
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialStatus {
    Prepared,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl TrialStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageAttemptStatus {
    Reserved,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Abandoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct TrialSnapshot {
    trial_run_id: TrialRunId,
    status: TrialStatus,
    budget: BudgetLedger,
    attempt_count: usize,
    active_attempt_count: usize,
    successful_stage_count: usize,
    last_event_fingerprint: BlobId,
}

/// Strict replay output with no method that emits a reservation, command,
/// terminal lease, store write, or completion authority.
///
/// ```compile_fail
/// # use loom_trial::CheckedTrialReplay;
/// fn resume(mut replay: CheckedTrialReplay) {
///     replay.complete();
/// }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CheckedTrialReplay {
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    snapshot: TrialSnapshot,
    event_count: usize,
}

impl CheckedTrialReplay {
    pub const fn trial_run_id(self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn trial_fingerprint(self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn snapshot(self) -> TrialSnapshot {
        self.snapshot
    }

    pub const fn event_count(self) -> usize {
        self.event_count
    }
}

impl TrialSnapshot {
    pub const fn trial_run_id(self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn status(self) -> TrialStatus {
        self.status
    }

    pub const fn budget(self) -> BudgetLedger {
        self.budget
    }

    pub const fn attempt_count(self) -> usize {
        self.attempt_count
    }

    pub const fn active_attempt_count(self) -> usize {
        self.active_attempt_count
    }

    pub const fn successful_stage_count(self) -> usize {
        self.successful_stage_count
    }

    pub const fn last_event_fingerprint(self) -> BlobId {
        self.last_event_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InternalAttemptState {
    Reserved,
    Running { command_fingerprint: BlobId },
    Terminal(AttemptTerminal),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AttemptRecord {
    stage_id: StageId,
    ordinal: u16,
    reservation: BudgetAmount,
    input_fingerprint: BlobId,
    reservation_event_fingerprint: BlobId,
    start_event_fingerprint: Option<BlobId>,
    state: InternalAttemptState,
}

#[derive(Clone, Copy, Debug)]
struct CommandBuildContext {
    trial_run_id: TrialRunId,
    session_id: TrialSessionId,
    attempt_id: StageAttemptId,
    record: AttemptRecord,
    command_fingerprint: BlobId,
    start_event_fingerprint: BlobId,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ReplayState {
    initialized: bool,
    trial_run_id: Option<TrialRunId>,
    status: Option<TrialStatus>,
    budget: BudgetLedger,
    attempts: BTreeMap<StageAttemptId, AttemptRecord>,
    stage_outputs: BTreeMap<StageId, BlobId>,
    stage_terminal_event_fingerprints: BTreeMap<StageId, BlobId>,
    last_event_fingerprint: Option<BlobId>,
    next_sequence: u32,
}

#[derive(Debug)]
enum TrialJournalPersistence {
    Store(Box<ResearchJournalWriter>),
    #[cfg(test)]
    Diagnostic {
        lease_fingerprint: BlobId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CanonicalTrialEventRecord {
    format: String,
    project_id: ProjectId,
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    session_id: ArtifactId,
    store_lease_fingerprint: BlobId,
    event: TrialEvent,
}

#[derive(Serialize)]
struct CanonicalStageAttemptRecord {
    format: &'static str,
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    event_fingerprint: BlobId,
    attempt_id: StageAttemptId,
    stage_id: StageId,
    attempt_ordinal: u16,
    input_fingerprint: BlobId,
}

#[derive(Serialize)]
struct CanonicalStageBudgetRecord {
    format: &'static str,
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    event_fingerprint: BlobId,
    attempt_id: StageAttemptId,
    amount: BudgetAmount,
}

fn canonical_trial_event_bytes(
    spec: &FrozenTrialSpec,
    trial_run_id: TrialRunId,
    event: &TrialEvent,
    session_id: ArtifactId,
    store_lease_fingerprint: BlobId,
) -> Result<Vec<u8>, TrialError> {
    Ok(serde_json::to_vec(&CanonicalTrialEventRecord {
        format: TRIAL_EVENT_RECORD_FORMAT.to_owned(),
        project_id: spec.project_id(),
        trial_run_id,
        trial_fingerprint: spec.fingerprint(),
        session_id,
        store_lease_fingerprint,
        event: event.clone(),
    })?)
}

fn canonical_stage_attempt_bytes(
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    event_fingerprint: BlobId,
    attempt_id: StageAttemptId,
    stage_id: StageId,
    attempt_ordinal: u16,
    input_fingerprint: BlobId,
) -> Result<Vec<u8>, TrialError> {
    Ok(serde_json::to_vec(&CanonicalStageAttemptRecord {
        format: STAGE_ATTEMPT_RECORD_FORMAT,
        trial_run_id,
        trial_fingerprint,
        event_fingerprint,
        attempt_id,
        stage_id,
        attempt_ordinal,
        input_fingerprint,
    })?)
}

fn canonical_stage_budget_bytes(
    format: &'static str,
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    event_fingerprint: BlobId,
    attempt_id: StageAttemptId,
    amount: BudgetAmount,
) -> Result<Vec<u8>, TrialError> {
    Ok(serde_json::to_vec(&CanonicalStageBudgetRecord {
        format,
        trial_run_id,
        trial_fingerprint,
        event_fingerprint,
        attempt_id,
        amount,
    })?)
}

/// Mutable in-memory reducer over immutable trial events.
#[derive(Debug)]
pub struct TrialJournal {
    spec: FrozenTrialSpec,
    trial_run_id: TrialRunId,
    stage_graph: StageGraph,
    session_id: TrialSessionId,
    persistence: TrialJournalPersistence,
    events: Vec<TrialEvent>,
    state: ReplayState,
}

impl TrialJournal {
    pub fn new(
        spec: FrozenTrialSpec,
        stage_graph: StageGraph,
        store: &ProjectStore,
        session_lease: ExclusiveResearchSessionLease,
    ) -> Result<Self, TrialError> {
        verify_spec_and_graph(&spec, &stage_graph)?;
        if session_lease.kind() != ResearchSessionKind::Trial
            || session_lease.trial_run_id().is_none()
            || session_lease.project_id() != spec.project_id()
        {
            return Err(TrialError::SessionLeaseMismatch);
        }
        verify_persisted_trial_snapshot(&spec, &stage_graph, &session_lease)?;
        let trial_run_id = session_lease
            .trial_run_id()
            .ok_or(TrialError::SessionLeaseMismatch)?;
        let session_id = TrialSessionId::from_artifact_id(session_lease.session_id());
        let store_lease_fingerprint = session_lease.lease_fingerprint();
        let persistence = TrialJournalPersistence::Store(Box::new(
            store.open_research_journal_writer(session_lease)?,
        ));
        if let TrialJournalPersistence::Store(writer) = &persistence
            && !writer.load_trial_events()?.is_empty()
        {
            return Err(TrialError::JournalAlreadyExists);
        }
        let mut journal = Self {
            spec,
            trial_run_id,
            stage_graph,
            session_id,
            persistence,
            events: Vec::new(),
            state: ReplayState::default(),
        };
        journal.append(TrialEventKind::Prepared {
            trial_run_id,
            trial_fingerprint: journal.spec.fingerprint(),
            store_lease_fingerprint,
        })?;
        Ok(journal)
    }

    /// Reopens one exact persisted journal under a newly minted store lease.
    /// Reserved work is released and started work is charged at its complete
    /// reservation before this method returns new dispatch authority.
    pub fn resume(
        spec: FrozenTrialSpec,
        stage_graph: StageGraph,
        store: &ProjectStore,
        session_lease: ExclusiveResearchSessionLease,
    ) -> Result<Self, TrialError> {
        verify_spec_and_graph(&spec, &stage_graph)?;
        if session_lease.kind() != ResearchSessionKind::Trial
            || session_lease.trial_run_id().is_none()
            || session_lease.project_id() != spec.project_id()
        {
            return Err(TrialError::SessionLeaseMismatch);
        }
        verify_persisted_trial_snapshot(&spec, &stage_graph, &session_lease)?;
        let trial_run_id = session_lease
            .trial_run_id()
            .ok_or(TrialError::SessionLeaseMismatch)?;
        let session_id = TrialSessionId::from_artifact_id(session_lease.session_id());
        let writer = store.open_research_journal_writer(session_lease)?;
        let persisted = writer.load_trial_events()?;
        if persisted.is_empty() {
            return Err(TrialError::EmptyJournal);
        }
        let events = decode_persisted_trial_events(&spec, trial_run_id, &persisted)?;
        let state = reduce_events(&spec, &stage_graph, &events)?;
        if state.status.is_some_and(TrialStatus::is_terminal) {
            return Err(TrialError::TrialAlreadyTerminal);
        }
        let mut journal = Self {
            spec,
            trial_run_id,
            stage_graph,
            session_id,
            persistence: TrialJournalPersistence::Store(Box::new(writer)),
            events,
            state,
        };
        journal.reconcile_pre_crash_attempts()?;
        Ok(journal)
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_for_tests(
        spec: FrozenTrialSpec,
        stage_graph: StageGraph,
    ) -> Result<Self, TrialError> {
        verify_spec_and_graph(&spec, &stage_graph)?;
        let session_id = TrialSessionId::new();
        let trial_run_id = TrialRunId::new();
        let store_lease_fingerprint = BlobId::digest(b"test-only live trial store lease");
        let mut journal = Self {
            spec,
            trial_run_id,
            stage_graph,
            session_id,
            persistence: TrialJournalPersistence::Diagnostic {
                lease_fingerprint: store_lease_fingerprint,
            },
            events: Vec::new(),
            state: ReplayState::default(),
        };
        journal.append(TrialEventKind::Prepared {
            trial_run_id,
            trial_fingerprint: journal.spec.fingerprint(),
            store_lease_fingerprint,
        })?;
        Ok(journal)
    }

    pub fn replay(
        spec: &FrozenTrialSpec,
        stage_graph: &StageGraph,
        events: &[TrialEvent],
    ) -> Result<CheckedTrialReplay, TrialError> {
        let state = reduce_events(spec, stage_graph, events)?;
        Ok(CheckedTrialReplay {
            trial_run_id: state.trial_run_id.ok_or(TrialError::MissingPreparedEvent)?,
            trial_fingerprint: spec.fingerprint(),
            snapshot: snapshot_from(&state),
            event_count: events.len(),
        })
    }

    pub fn replay_batch(
        spec: &FrozenTrialSpec,
        stage_graph: &StageGraph,
        events: &TrialEventBatch,
    ) -> Result<CheckedTrialReplay, TrialError> {
        Self::replay(spec, stage_graph, events.events())
    }

    pub const fn spec(&self) -> &FrozenTrialSpec {
        &self.spec
    }

    pub const fn trial_run_id(&self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn stage_graph(&self) -> &StageGraph {
        &self.stage_graph
    }

    pub fn store_lease_fingerprint(&self) -> BlobId {
        match &self.persistence {
            TrialJournalPersistence::Store(writer) => writer.lease_fingerprint(),
            #[cfg(test)]
            TrialJournalPersistence::Diagnostic { lease_fingerprint } => *lease_fingerprint,
        }
    }

    pub fn events(&self) -> &[TrialEvent] {
        &self.events
    }

    pub fn snapshot(&self) -> TrialSnapshot {
        snapshot_from(&self.state)
    }

    pub fn attempt_status(&self, attempt_id: StageAttemptId) -> Option<StageAttemptStatus> {
        self.state
            .attempts
            .get(&attempt_id)
            .map(|attempt| public_attempt_status(attempt.state))
    }

    /// Reserves worst-case resources before a stage becomes runnable.
    ///
    /// Every retry receives a fresh attempt ID. Generate binds that ID later,
    /// when the runtime verifies a live prompt with the frozen content identity;
    /// the immutable trial never freezes or relabels an execution attempt.
    pub fn reserve_stage(
        &mut self,
        stage_id: StageId,
        attempt_id: StageAttemptId,
        reservation: BudgetAmount,
    ) -> Result<ReservedStagePermit, TrialError> {
        self.ensure_open()?;
        if self.state.attempts.len() >= MAX_STAGE_ATTEMPTS_PER_TRIAL {
            return Err(TrialError::AttemptLimit);
        }
        if self.state.attempts.contains_key(&attempt_id) {
            return Err(TrialError::DuplicateAttemptId(attempt_id));
        }
        let stage = self
            .stage_graph
            .stage(stage_id)
            .ok_or(TrialError::UnknownStage(stage_id))?;
        ensure_stage_ready(&self.state, stage)?;
        validate_reservation(&self.spec, stage.id(), stage.stage(), reservation)?;
        let count = self
            .state
            .attempts
            .values()
            .filter(|attempt| attempt.stage_id == stage_id)
            .count();
        let ordinal = u16::try_from(count + 1).map_err(|_| TrialError::AttemptLimit)?;
        let input_fingerprint = fingerprint_stage_input(&self.spec, stage, &self.state)?;
        self.append(TrialEventKind::AttemptReserved {
            attempt_id,
            stage_id,
            attempt_ordinal: ordinal,
            reservation,
            input_fingerprint,
        })?;
        Ok(ReservedStagePermit {
            trial_run_id: self.trial_run_id,
            trial_fingerprint: self.spec.fingerprint(),
            session_id: self.session_id,
            attempt_id,
            stage_id,
            reservation_event_fingerprint: self.snapshot().last_event_fingerprint(),
        })
    }

    pub fn start_reserved(
        &mut self,
        permit: ReservedStagePermit,
    ) -> Result<StageCommand, TrialError> {
        self.ensure_open()?;
        self.verify_reserved_permit(&permit)?;
        let (_, _, _, attempt_id, _, _) = permit.into_parts();
        let record = *self
            .state
            .attempts
            .get(&attempt_id)
            .ok_or(TrialError::AttemptNotFound(attempt_id))?;
        if record.state != InternalAttemptState::Reserved {
            return Err(TrialError::AttemptNotReserved(attempt_id));
        }
        let stage = self
            .stage_graph
            .stage(record.stage_id)
            .expect("recorded stage exists")
            .clone();
        let command_fingerprint =
            fingerprint_command(&self.spec, self.trial_run_id, &stage, attempt_id, record);
        self.append(TrialEventKind::AttemptStarted {
            attempt_id,
            command_fingerprint,
        })?;
        Ok(build_command(
            &self.spec,
            &stage,
            &CommandBuildContext {
                trial_run_id: self.trial_run_id,
                session_id: self.session_id,
                attempt_id,
                record,
                command_fingerprint,
                start_event_fingerprint: self.snapshot().last_event_fingerprint(),
            },
        ))
    }

    pub fn finish(
        &mut self,
        command: StageCommand,
        terminal_lease: VerifiedStageTerminalLease,
    ) -> Result<(), TrialError> {
        self.ensure_open()?;
        verify_command(
            &self.spec,
            &self.stage_graph,
            &self.state,
            self.session_id,
            &command,
        )?;
        verify_terminal_lease(&command, &terminal_lease)?;
        let attempt_id = command.into_attempt_id();
        let (terminal, actual_charge, live_terminal_evidence_fingerprint) =
            terminal_lease.into_parts();
        self.append(TrialEventKind::AttemptFinished {
            attempt_id,
            command_fingerprint: self.running_command_fingerprint(attempt_id)?,
            terminal,
            actual_charge,
            terminal_evidence: StageTerminalEvidence::VerifiedLive {
                receipt_fingerprint: live_terminal_evidence_fingerprint,
            },
        })
    }

    /// Conservatively charges the entire reservation when a started external
    /// call has no trustworthy terminal usage report.
    pub fn reconcile_interrupted(
        &mut self,
        command: StageCommand,
        diagnostic_fingerprint: BlobId,
    ) -> Result<(), TrialError> {
        self.ensure_open()?;
        verify_command(
            &self.spec,
            &self.stage_graph,
            &self.state,
            self.session_id,
            &command,
        )?;
        let attempt_id = command.into_attempt_id();
        let record = *self
            .state
            .attempts
            .get(&attempt_id)
            .ok_or(TrialError::AttemptNotFound(attempt_id))?;
        let InternalAttemptState::Running {
            command_fingerprint,
        } = record.state
        else {
            return Err(TrialError::AttemptNotRunning(attempt_id));
        };
        self.append(TrialEventKind::AttemptFinished {
            attempt_id,
            command_fingerprint,
            terminal: AttemptTerminal::Interrupted {
                diagnostic_fingerprint,
            },
            actual_charge: record.reservation,
            terminal_evidence: StageTerminalEvidence::ConservativeInterruption {
                diagnostic_fingerprint,
            },
        })
    }

    pub fn abandon_reserved(
        &mut self,
        permit: ReservedStagePermit,
        diagnostic_fingerprint: BlobId,
    ) -> Result<(), TrialError> {
        self.ensure_open()?;
        self.verify_reserved_permit(&permit)?;
        let (_, _, _, attempt_id, _, _) = permit.into_parts();
        let record = self
            .state
            .attempts
            .get(&attempt_id)
            .ok_or(TrialError::AttemptNotFound(attempt_id))?;
        if record.state != InternalAttemptState::Reserved {
            return Err(TrialError::AttemptNotReserved(attempt_id));
        }
        self.append(TrialEventKind::AttemptAbandoned {
            attempt_id,
            diagnostic_fingerprint,
        })
    }

    pub fn completion_request(&self) -> Result<TrialCompletionRequest, TrialError> {
        self.ensure_open()?;
        if has_active_attempt(&self.state) {
            return Err(TrialError::ActiveAttempts);
        }
        let archive_stage_id = self.stage_graph.output();
        let archive_output_fingerprint = self
            .state
            .stage_outputs
            .get(&archive_stage_id)
            .copied()
            .ok_or(TrialError::ArchiveNotSucceeded)?;
        let archive_terminal_event_fingerprint = self
            .state
            .stage_terminal_event_fingerprints
            .get(&archive_stage_id)
            .copied()
            .ok_or(TrialError::ArchiveNotSucceeded)?;
        Ok(TrialCompletionRequest::new(
            self.trial_run_id,
            self.spec.fingerprint(),
            archive_stage_id,
            archive_output_fingerprint,
            archive_terminal_event_fingerprint,
            self.snapshot().last_event_fingerprint(),
        ))
    }

    pub fn complete(
        &mut self,
        completion_lease: VerifiedArchiveCompletionLease,
    ) -> Result<VerifiedCompletedTrialLease, TrialError> {
        let request = self.completion_request()?;
        self.verify_completion_lease(request, &completion_lease)?;
        let archive_terminal_event_fingerprint =
            completion_lease.archive_terminal_event_fingerprint;
        let (archive_output_fingerprint, live_completion_evidence_fingerprint) =
            completion_lease.into_parts();
        self.close(
            TrialDisposition::Completed,
            None,
            Some(live_completion_evidence_fingerprint),
        )?;
        Ok(VerifiedCompletedTrialLease {
            trial_run_id: self.trial_run_id,
            trial_fingerprint: self.spec.fingerprint(),
            trial_journal_fingerprint: self.snapshot().last_event_fingerprint(),
            archive_output_fingerprint,
            archive_terminal_event_fingerprint,
            actual_charge: self.snapshot().budget().charged(),
            live_completion_evidence_fingerprint,
        })
    }

    pub fn fail(&mut self, diagnostic_fingerprint: BlobId) -> Result<(), TrialError> {
        self.close(TrialDisposition::Failed, Some(diagnostic_fingerprint), None)
    }

    pub fn cancel(&mut self, diagnostic_fingerprint: BlobId) -> Result<(), TrialError> {
        self.close(
            TrialDisposition::Cancelled,
            Some(diagnostic_fingerprint),
            None,
        )
    }

    fn close(
        &mut self,
        disposition: TrialDisposition,
        diagnostic_fingerprint: Option<BlobId>,
        live_completion_evidence_fingerprint: Option<BlobId>,
    ) -> Result<(), TrialError> {
        self.ensure_open()?;
        if has_active_attempt(&self.state) {
            return Err(TrialError::ActiveAttempts);
        }
        if disposition == TrialDisposition::Completed
            && !self
                .state
                .stage_outputs
                .contains_key(&self.stage_graph.output())
        {
            return Err(TrialError::ArchiveNotSucceeded);
        }
        if (disposition == TrialDisposition::Completed)
            != (diagnostic_fingerprint.is_none() && live_completion_evidence_fingerprint.is_some())
        {
            return Err(TrialError::InvalidDispositionEvidence);
        }
        self.append(TrialEventKind::TrialClosed {
            disposition,
            diagnostic_fingerprint,
            live_completion_evidence_fingerprint,
        })
    }

    #[cfg(test)]
    pub(crate) fn completion_lease_for_tests(
        &self,
        live_completion_evidence_fingerprint: BlobId,
    ) -> Result<VerifiedArchiveCompletionLease, TrialError> {
        let request = self.completion_request()?;
        Ok(VerifiedArchiveCompletionLease {
            trial_run_id: request.trial_run_id(),
            trial_fingerprint: request.trial_fingerprint(),
            session_id: self.session_id,
            archive_stage_id: request.archive_stage_id(),
            archive_output_fingerprint: request.archive_output_fingerprint(),
            archive_terminal_event_fingerprint: request.archive_terminal_event_fingerprint(),
            current_event_fingerprint: request.current_event_fingerprint(),
            live_completion_evidence_fingerprint,
        })
    }

    fn verify_reserved_permit(&self, permit: &ReservedStagePermit) -> Result<(), TrialError> {
        if permit.trial_run_id != self.trial_run_id
            || permit.trial_fingerprint != self.spec.fingerprint()
            || permit.session_id != self.session_id
        {
            return Err(TrialError::ReservationPermitMismatch);
        }
        let record = self
            .state
            .attempts
            .get(&permit.attempt_id)
            .ok_or(TrialError::AttemptNotFound(permit.attempt_id))?;
        if record.stage_id != permit.stage_id
            || record.state != InternalAttemptState::Reserved
            || record.reservation_event_fingerprint != permit.reservation_event_fingerprint
        {
            return Err(TrialError::ReservationPermitMismatch);
        }
        Ok(())
    }

    fn verify_completion_lease(
        &self,
        request: TrialCompletionRequest,
        lease: &VerifiedArchiveCompletionLease,
    ) -> Result<(), TrialError> {
        if lease.trial_run_id != request.trial_run_id()
            || lease.trial_fingerprint != request.trial_fingerprint()
            || lease.session_id != self.session_id
            || lease.archive_stage_id != request.archive_stage_id()
            || lease.archive_output_fingerprint != request.archive_output_fingerprint()
            || lease.archive_terminal_event_fingerprint
                != request.archive_terminal_event_fingerprint()
            || lease.current_event_fingerprint != request.current_event_fingerprint()
        {
            return Err(TrialError::CompletionLeaseMismatch);
        }
        Ok(())
    }

    fn running_command_fingerprint(
        &self,
        attempt_id: StageAttemptId,
    ) -> Result<BlobId, TrialError> {
        let record = self
            .state
            .attempts
            .get(&attempt_id)
            .ok_or(TrialError::AttemptNotFound(attempt_id))?;
        match record.state {
            InternalAttemptState::Running {
                command_fingerprint,
            } => Ok(command_fingerprint),
            InternalAttemptState::Reserved | InternalAttemptState::Terminal(_) => {
                Err(TrialError::AttemptNotRunning(attempt_id))
            }
        }
    }

    fn append(&mut self, kind: TrialEventKind) -> Result<(), TrialError> {
        if self.events.len() >= MAX_TRIAL_EVENTS {
            return Err(TrialError::EventLimit);
        }
        let event = TrialEvent::new(
            self.state.next_sequence,
            self.state.last_event_fingerprint,
            kind,
        );
        let mut next_state = self.state.clone();
        apply_event(&self.spec, &self.stage_graph, &mut next_state, &event)?;
        self.persist_event(&event)?;
        self.state = next_state;
        self.events.push(event);
        Ok(())
    }

    fn persist_event(&mut self, event: &TrialEvent) -> Result<(), TrialError> {
        #[cfg(test)]
        if matches!(self.persistence, TrialJournalPersistence::Diagnostic { .. }) {
            return Ok(());
        }

        let (session_id, store_lease_fingerprint) = self.store_persistence_identity();
        let event_bytes = canonical_trial_event_bytes(
            &self.spec,
            self.trial_run_id,
            event,
            session_id,
            store_lease_fingerprint,
        )?;

        let attempt_bytes;
        let reservation_bytes;
        let charge_bytes;
        let mutation = match event.kind {
            TrialEventKind::Prepared { .. } => TrialJournalMutation::Prepared,
            TrialEventKind::AttemptReserved {
                attempt_id,
                stage_id,
                attempt_ordinal,
                reservation,
                input_fingerprint,
            } => {
                attempt_bytes = canonical_stage_attempt_bytes(
                    self.trial_run_id,
                    self.spec.fingerprint(),
                    event.fingerprint,
                    attempt_id,
                    stage_id,
                    attempt_ordinal,
                    input_fingerprint,
                )?;
                reservation_bytes = canonical_stage_budget_bytes(
                    STAGE_RESERVATION_RECORD_FORMAT,
                    self.trial_run_id,
                    self.spec.fingerprint(),
                    event.fingerprint,
                    attempt_id,
                    reservation,
                )?;
                TrialJournalMutation::AttemptReserved {
                    attempt_id,
                    stage_id,
                    attempt_ordinal,
                    reservation: research_journal_budget(reservation),
                    canonical_attempt_bytes: &attempt_bytes,
                    canonical_reservation_bytes: &reservation_bytes,
                }
            }
            TrialEventKind::AttemptStarted { attempt_id, .. } => {
                TrialJournalMutation::AttemptStarted { attempt_id }
            }
            TrialEventKind::AttemptFinished {
                attempt_id,
                terminal,
                actual_charge,
                ..
            } => {
                charge_bytes = canonical_stage_budget_bytes(
                    STAGE_CHARGE_RECORD_FORMAT,
                    self.trial_run_id,
                    self.spec.fingerprint(),
                    event.fingerprint,
                    attempt_id,
                    actual_charge,
                )?;
                TrialJournalMutation::AttemptFinished {
                    attempt_id,
                    outcome: persisted_trial_outcome(terminal)?,
                    terminal_output_fingerprint: terminal.output_fingerprint(),
                    charge: research_journal_budget(actual_charge),
                    canonical_charge_bytes: &charge_bytes,
                }
            }
            TrialEventKind::AttemptAbandoned { attempt_id, .. } => {
                TrialJournalMutation::AttemptAbandoned { attempt_id }
            }
            TrialEventKind::TrialClosed { .. } => TrialJournalMutation::TrialClosed,
        };
        let persistence = TrialJournalEventPersistence {
            trial_run_id: self.trial_run_id,
            trial_fingerprint: self.spec.fingerprint(),
            event_index: event.sequence,
            previous_event_fingerprint: event.previous_event_fingerprint,
            event_fingerprint: event.fingerprint,
            canonical_event_bytes: &event_bytes,
            mutation,
        };
        self.store_writer().append_trial_event(persistence)?;
        Ok(())
    }

    fn store_persistence_identity(&self) -> (ArtifactId, BlobId) {
        match &self.persistence {
            TrialJournalPersistence::Store(writer) => {
                (writer.session_id(), writer.lease_fingerprint())
            }
            #[cfg(test)]
            TrialJournalPersistence::Diagnostic { .. } => unreachable!("diagnostic returned early"),
        }
    }

    fn store_writer(&mut self) -> &mut ResearchJournalWriter {
        match &mut self.persistence {
            TrialJournalPersistence::Store(writer) => writer,
            #[cfg(test)]
            TrialJournalPersistence::Diagnostic { .. } => unreachable!("diagnostic returned early"),
        }
    }

    fn reconcile_pre_crash_attempts(&mut self) -> Result<(), TrialError> {
        let active = self
            .state
            .attempts
            .iter()
            .filter_map(|(attempt_id, record)| match record.state {
                InternalAttemptState::Reserved | InternalAttemptState::Running { .. } => {
                    Some((*attempt_id, *record))
                }
                InternalAttemptState::Terminal(_) => None,
            })
            .collect::<Vec<_>>();
        for (attempt_id, record) in active {
            let diagnostic = recovery_diagnostic(
                self.spec.fingerprint(),
                self.state
                    .last_event_fingerprint
                    .expect("persisted journal has an event head"),
                attempt_id,
                record.state,
            );
            match record.state {
                InternalAttemptState::Reserved => {
                    self.append(TrialEventKind::AttemptAbandoned {
                        attempt_id,
                        diagnostic_fingerprint: diagnostic,
                    })?;
                }
                InternalAttemptState::Running {
                    command_fingerprint,
                } => {
                    self.append(TrialEventKind::AttemptFinished {
                        attempt_id,
                        command_fingerprint,
                        terminal: AttemptTerminal::Interrupted {
                            diagnostic_fingerprint: diagnostic,
                        },
                        actual_charge: record.reservation,
                        terminal_evidence: StageTerminalEvidence::ConservativeInterruption {
                            diagnostic_fingerprint: diagnostic,
                        },
                    })?;
                }
                InternalAttemptState::Terminal(_) => unreachable!("filtered active attempts"),
            }
        }
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), TrialError> {
        if self.state.status.is_some_and(TrialStatus::is_terminal) {
            return Err(TrialError::TrialAlreadyTerminal);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum TrialError {
    #[error(transparent)]
    Spec(#[from] TrialSpecError),
    #[error(transparent)]
    Budget(#[from] BudgetError),
    #[error("stage graph does not match the frozen trial")]
    StageGraphMismatch,
    #[error("live trial session lease does not match the frozen trial")]
    SessionLeaseMismatch,
    #[error("persisted normalized trial or stage rows do not match the frozen trial")]
    SessionNormalizedSnapshotMismatch,
    #[error("durable trial journal storage failed")]
    Store,
    #[error("durable trial journal JSON encoding or decoding failed")]
    Json,
    #[error("a durable trial journal already exists; use resume")]
    JournalAlreadyExists,
    #[error("persisted trial journal record is malformed or non-canonical")]
    MalformedJournalRecord,
    #[error("journal contains no events")]
    EmptyJournal,
    #[error("journal does not begin with its prepared event")]
    MissingPreparedEvent,
    #[error("trial event limit exceeded")]
    EventLimit,
    #[error("trial attempt limit exceeded")]
    AttemptLimit,
    #[error("event sequence mismatch: expected {expected}, received {actual}")]
    EventSequence { expected: u32, actual: u32 },
    #[error("event previous-fingerprint link mismatch")]
    PreviousEventFingerprint,
    #[error("event content fingerprint mismatch")]
    EventFingerprint,
    #[error("prepared event is misplaced or references another trial")]
    InvalidPreparedEvent,
    #[error("trial is already terminal")]
    TrialAlreadyTerminal,
    #[error("unknown stage {0}")]
    UnknownStage(StageId),
    #[error("stage {stage} dependency {dependency} has not succeeded")]
    DependencyNotSatisfied { stage: StageId, dependency: StageId },
    #[error("stage {0} has already succeeded")]
    StageAlreadySucceeded(StageId),
    #[error("stage {0} already has an active attempt")]
    StageAttemptActive(StageId),
    #[error("attempt id {0} was already used")]
    DuplicateAttemptId(StageAttemptId),
    #[error("attempt {0} is absent")]
    AttemptNotFound(StageAttemptId),
    #[error("attempt {0} is not reserved")]
    AttemptNotReserved(StageAttemptId),
    #[error("attempt {0} is not running")]
    AttemptNotRunning(StageAttemptId),
    #[error("attempt ordinal is not the next immutable retry ordinal")]
    AttemptOrdinal,
    #[error("attempt input fingerprint mismatch")]
    AttemptInputFingerprint,
    #[error("reservation is invalid for stage {0:?}")]
    InvalidReservation(FrozenTrialStage),
    #[error("stage command does not match the active attempt")]
    CommandMismatch,
    #[error("stage reservation permit does not match the live reserved attempt")]
    ReservationPermitMismatch,
    #[error("verified terminal lease does not match the consumed stage command")]
    TerminalLeaseMismatch,
    #[error("stage charge is invalid for stage {0:?}")]
    InvalidCharge(FrozenTrialStage),
    #[error("frozen stage output fingerprint mismatch")]
    ExpectedOutputFingerprint,
    #[error("attempt terminal transition is invalid")]
    InvalidAttemptTerminal,
    #[error("attempt terminal uses the wrong evidence class")]
    InvalidTerminalEvidenceClass,
    #[error("active attempts must be reconciled before closing")]
    ActiveAttempts,
    #[error("archive stage has not succeeded")]
    ArchiveNotSucceeded,
    #[error("verified archive completion lease does not match the current journal head")]
    CompletionLeaseMismatch,
    #[error("trial disposition has invalid diagnostic evidence")]
    InvalidDispositionEvidence,
}

impl From<loom_store::StoreError> for TrialError {
    fn from(_: loom_store::StoreError) -> Self {
        Self::Store
    }
}

impl From<serde_json::Error> for TrialError {
    fn from(_: serde_json::Error) -> Self {
        Self::Json
    }
}

fn verify_persisted_trial_snapshot(
    spec: &FrozenTrialSpec,
    graph: &StageGraph,
    lease: &ExclusiveResearchSessionLease,
) -> Result<(), TrialError> {
    let PersistedResearchSubjectSnapshot::Trial(snapshot) = lease.snapshot() else {
        return Err(TrialError::SessionNormalizedSnapshotMismatch);
    };
    let run_record = TrialRunRecord::new(
        snapshot.trial_run_id(),
        spec.fingerprint(),
        snapshot.run_origin(),
    );
    if lease.trial_run_id() != Some(snapshot.trial_run_id())
        || lease.record_fingerprint() != snapshot.run_record_fingerprint()
        || run_record.record_fingerprint()? != snapshot.run_record_fingerprint()
        || snapshot.campaign_id() != spec.campaign_id()
        || snapshot.project_id() != spec.project_id()
        || snapshot.trial_fingerprint() != spec.fingerprint()
        || snapshot.trial_case_id() != spec.case_id()
        || snapshot.treatment_fingerprint() != spec.treatment_fingerprint()
        || snapshot.prompt_content_fingerprint() != spec.prompt_content_fingerprint()
        || snapshot.model_binding_fingerprint() != spec.model_binding_fingerprint()
        || snapshot.expected_writer_call_count() != spec.expected_writer_call_count()
        || snapshot.declared_writer_token_maximum() != spec.declared_writer_token_maximum()
        || snapshot.maximum() != research_budget_from_limits(spec.budget())
        || snapshot.trial_record_fingerprint() != spec.canonical_record_fingerprint()?
        || snapshot.stages().len() != graph.stages().len()
    {
        return Err(TrialError::SessionNormalizedSnapshotMismatch);
    }
    for (persisted, frozen) in snapshot.stages().iter().zip(graph.stages()) {
        let maximum = spec
            .stage_budget_maximum(frozen.id())
            .ok_or(TrialError::SessionNormalizedSnapshotMismatch)?;
        if persisted.stage_id() != frozen.id()
            || persisted.stage() != frozen.stage()
            || persisted.stage_spec_fingerprint() != frozen.spec_fingerprint()
            || persisted.maximum() != research_budget_from_amount(maximum)
            || persisted.record_fingerprint() != canonical_stage_record_fingerprint(frozen)?
            || persisted.dependencies() != frozen.dependencies()
        {
            return Err(TrialError::SessionNormalizedSnapshotMismatch);
        }
    }
    Ok(())
}

const fn research_budget_from_limits(limits: crate::TrialBudgetLimits) -> ResearchBudgetMaximum {
    ResearchBudgetMaximum {
        writer_tokens: limits.writer_tokens(),
        controller_tokens: limits.controller_tokens(),
        evaluations: limits.evaluations(),
        wall_time_ms: limits.wall_time_ms(),
    }
}

const fn research_budget_from_amount(amount: BudgetAmount) -> ResearchBudgetMaximum {
    ResearchBudgetMaximum {
        writer_tokens: amount.writer_tokens(),
        controller_tokens: amount.controller_tokens(),
        evaluations: amount.evaluations(),
        wall_time_ms: amount.wall_time_ms(),
    }
}

fn verify_spec_and_graph(
    spec: &FrozenTrialSpec,
    stage_graph: &StageGraph,
) -> Result<(), TrialError> {
    spec.verify_integrity()?;
    if stage_graph.id() != spec.stage_graph_id()
        || fingerprint_stage_graph(stage_graph) != spec.stage_graph_fingerprint()
    {
        return Err(TrialError::StageGraphMismatch);
    }
    Ok(())
}

fn reduce_events(
    spec: &FrozenTrialSpec,
    stage_graph: &StageGraph,
    events: &[TrialEvent],
) -> Result<ReplayState, TrialError> {
    verify_spec_and_graph(spec, stage_graph)?;
    if events.is_empty() {
        return Err(TrialError::EmptyJournal);
    }
    if events.len() > MAX_TRIAL_EVENTS {
        return Err(TrialError::EventLimit);
    }
    let mut state = ReplayState::default();
    for event in events {
        apply_event(spec, stage_graph, &mut state, event)?;
    }
    if !state.initialized {
        return Err(TrialError::MissingPreparedEvent);
    }
    Ok(state)
}

fn snapshot_from(state: &ReplayState) -> TrialSnapshot {
    let active_attempt_count = state
        .attempts
        .values()
        .filter(|attempt| {
            matches!(
                attempt.state,
                InternalAttemptState::Reserved | InternalAttemptState::Running { .. }
            )
        })
        .count();
    TrialSnapshot {
        trial_run_id: state.trial_run_id.expect("prepared journal run ID"),
        status: state.status.expect("prepared journal status"),
        budget: state.budget,
        attempt_count: state.attempts.len(),
        active_attempt_count,
        successful_stage_count: state.stage_outputs.len(),
        last_event_fingerprint: state
            .last_event_fingerprint
            .expect("prepared journal fingerprint"),
    }
}

fn apply_event(
    spec: &FrozenTrialSpec,
    graph: &StageGraph,
    reducer: &mut ReplayState,
    event: &TrialEvent,
) -> Result<(), TrialError> {
    validate_event_link(reducer, event)?;

    match event.kind {
        TrialEventKind::Prepared {
            trial_run_id,
            trial_fingerprint,
            store_lease_fingerprint,
        } => {
            apply_prepared(
                spec,
                reducer,
                event.sequence,
                trial_run_id,
                trial_fingerprint,
                store_lease_fingerprint,
            )?;
        }
        TrialEventKind::AttemptReserved {
            attempt_id,
            stage_id,
            attempt_ordinal,
            reservation,
            input_fingerprint,
        } => apply_reservation(
            spec,
            graph,
            reducer,
            ReservationEvent {
                attempt_id,
                stage_id,
                attempt_ordinal,
                reservation,
                input_fingerprint,
            },
            event.fingerprint,
        )?,
        TrialEventKind::AttemptStarted {
            attempt_id,
            command_fingerprint,
        } => apply_started(
            spec,
            graph,
            reducer,
            attempt_id,
            command_fingerprint,
            event.fingerprint,
        )?,
        TrialEventKind::AttemptFinished {
            attempt_id,
            command_fingerprint,
            terminal,
            actual_charge,
            terminal_evidence,
        } => apply_finished(
            spec,
            graph,
            reducer,
            FinishedEvent {
                attempt_id,
                command_fingerprint,
                terminal,
                actual_charge,
                terminal_evidence,
                terminal_event_fingerprint: event.fingerprint,
            },
        )?,
        TrialEventKind::AttemptAbandoned {
            attempt_id,
            diagnostic_fingerprint,
        } => apply_abandoned(spec, reducer, attempt_id, diagnostic_fingerprint)?,
        TrialEventKind::TrialClosed {
            disposition,
            diagnostic_fingerprint,
            live_completion_evidence_fingerprint,
        } => apply_closed(
            graph,
            reducer,
            disposition,
            diagnostic_fingerprint,
            live_completion_evidence_fingerprint,
        )?,
    }

    reducer.last_event_fingerprint = Some(event.fingerprint);
    reducer.next_sequence = reducer
        .next_sequence
        .checked_add(1)
        .ok_or(TrialError::EventLimit)?;
    Ok(())
}

fn validate_event_link(reducer: &ReplayState, event: &TrialEvent) -> Result<(), TrialError> {
    if event.sequence != reducer.next_sequence {
        return Err(TrialError::EventSequence {
            expected: reducer.next_sequence,
            actual: event.sequence,
        });
    }
    if event.previous_event_fingerprint != reducer.last_event_fingerprint {
        return Err(TrialError::PreviousEventFingerprint);
    }
    if fingerprint_event(event.sequence, event.previous_event_fingerprint, event.kind)
        != event.fingerprint
    {
        return Err(TrialError::EventFingerprint);
    }
    Ok(())
}

fn apply_prepared(
    spec: &FrozenTrialSpec,
    reducer: &mut ReplayState,
    sequence: u32,
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    _store_lease_fingerprint: BlobId,
) -> Result<(), TrialError> {
    if reducer.initialized || sequence != 0 || trial_fingerprint != spec.fingerprint() {
        return Err(TrialError::InvalidPreparedEvent);
    }
    reducer.initialized = true;
    reducer.trial_run_id = Some(trial_run_id);
    reducer.status = Some(TrialStatus::Prepared);
    Ok(())
}

fn apply_reservation(
    spec: &FrozenTrialSpec,
    graph: &StageGraph,
    reducer: &mut ReplayState,
    event: ReservationEvent,
    reservation_event_fingerprint: BlobId,
) -> Result<(), TrialError> {
    ensure_replay_open(reducer)?;
    if reducer.attempts.len() >= MAX_STAGE_ATTEMPTS_PER_TRIAL {
        return Err(TrialError::AttemptLimit);
    }
    if reducer.attempts.contains_key(&event.attempt_id) {
        return Err(TrialError::DuplicateAttemptId(event.attempt_id));
    }
    let stage_spec = graph
        .stage(event.stage_id)
        .ok_or(TrialError::UnknownStage(event.stage_id))?;
    ensure_stage_ready(reducer, stage_spec)?;
    validate_reservation(spec, stage_spec.id(), stage_spec.stage(), event.reservation)?;
    let expected_ordinal = reducer
        .attempts
        .values()
        .filter(|attempt| attempt.stage_id == event.stage_id)
        .count()
        + 1;
    if usize::from(event.attempt_ordinal) != expected_ordinal {
        return Err(TrialError::AttemptOrdinal);
    }
    if event.input_fingerprint != fingerprint_stage_input(spec, stage_spec, reducer)? {
        return Err(TrialError::AttemptInputFingerprint);
    }
    reducer.budget.reserve(spec.budget(), event.reservation)?;
    reducer.attempts.insert(
        event.attempt_id,
        AttemptRecord {
            stage_id: event.stage_id,
            ordinal: event.attempt_ordinal,
            reservation: event.reservation,
            input_fingerprint: event.input_fingerprint,
            reservation_event_fingerprint,
            start_event_fingerprint: None,
            state: InternalAttemptState::Reserved,
        },
    );
    reducer.status = Some(TrialStatus::Running);
    Ok(())
}

#[derive(Clone, Copy)]
struct ReservationEvent {
    attempt_id: StageAttemptId,
    stage_id: StageId,
    attempt_ordinal: u16,
    reservation: BudgetAmount,
    input_fingerprint: BlobId,
}

fn apply_started(
    spec: &FrozenTrialSpec,
    graph: &StageGraph,
    reducer: &mut ReplayState,
    attempt_id: StageAttemptId,
    command_fingerprint: BlobId,
    start_event_fingerprint: BlobId,
) -> Result<(), TrialError> {
    ensure_replay_open(reducer)?;
    let record = *reducer
        .attempts
        .get(&attempt_id)
        .ok_or(TrialError::AttemptNotFound(attempt_id))?;
    if record.state != InternalAttemptState::Reserved {
        return Err(TrialError::AttemptNotReserved(attempt_id));
    }
    let stage_spec = graph.stage(record.stage_id).expect("recorded stage exists");
    if command_fingerprint
        != fingerprint_command(
            spec,
            reducer.trial_run_id.expect("prepared run"),
            stage_spec,
            attempt_id,
            record,
        )
    {
        return Err(TrialError::CommandMismatch);
    }
    let attempt = reducer.attempts.get_mut(&attempt_id).expect("attempt");
    attempt.start_event_fingerprint = Some(start_event_fingerprint);
    attempt.state = InternalAttemptState::Running {
        command_fingerprint,
    };
    Ok(())
}

fn apply_finished(
    spec: &FrozenTrialSpec,
    graph: &StageGraph,
    reducer: &mut ReplayState,
    event: FinishedEvent,
) -> Result<(), TrialError> {
    ensure_replay_open(reducer)?;
    validate_terminal_evidence(event.terminal, event.terminal_evidence)?;
    let record = *reducer
        .attempts
        .get(&event.attempt_id)
        .ok_or(TrialError::AttemptNotFound(event.attempt_id))?;
    if record.state
        != (InternalAttemptState::Running {
            command_fingerprint: event.command_fingerprint,
        })
    {
        return Err(TrialError::AttemptNotRunning(event.attempt_id));
    }
    let stage_spec = graph.stage(record.stage_id).expect("recorded stage exists");
    validate_terminal(
        spec,
        stage_spec.stage(),
        event.terminal,
        record.reservation,
        event.actual_charge,
    )?;
    reducer
        .budget
        .reconcile(spec.budget(), record.reservation, event.actual_charge)?;
    reducer
        .attempts
        .get_mut(&event.attempt_id)
        .expect("attempt")
        .state = InternalAttemptState::Terminal(event.terminal);
    if let Some(output_fingerprint) = event.terminal.output_fingerprint() {
        reducer
            .stage_outputs
            .insert(record.stage_id, output_fingerprint);
        reducer
            .stage_terminal_event_fingerprints
            .insert(record.stage_id, event.terminal_event_fingerprint);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct FinishedEvent {
    attempt_id: StageAttemptId,
    command_fingerprint: BlobId,
    terminal: AttemptTerminal,
    actual_charge: BudgetAmount,
    terminal_evidence: StageTerminalEvidence,
    terminal_event_fingerprint: BlobId,
}

fn validate_terminal_evidence(
    terminal: AttemptTerminal,
    evidence: StageTerminalEvidence,
) -> Result<(), TrialError> {
    let valid = match (terminal, evidence) {
        (
            AttemptTerminal::Interrupted {
                diagnostic_fingerprint: terminal_diagnostic,
            },
            StageTerminalEvidence::ConservativeInterruption {
                diagnostic_fingerprint: evidence_diagnostic,
            },
        ) => terminal_diagnostic == evidence_diagnostic,
        (
            AttemptTerminal::Succeeded { .. }
            | AttemptTerminal::Failed { .. }
            | AttemptTerminal::Cancelled { .. },
            StageTerminalEvidence::VerifiedLive { .. },
        ) => true,
        (
            AttemptTerminal::Abandoned { .. },
            StageTerminalEvidence::VerifiedLive { .. }
            | StageTerminalEvidence::ConservativeInterruption { .. },
        )
        | (AttemptTerminal::Interrupted { .. }, StageTerminalEvidence::VerifiedLive { .. })
        | (
            AttemptTerminal::Succeeded { .. }
            | AttemptTerminal::Failed { .. }
            | AttemptTerminal::Cancelled { .. },
            StageTerminalEvidence::ConservativeInterruption { .. },
        ) => false,
    };
    if !valid {
        return Err(TrialError::InvalidTerminalEvidenceClass);
    }
    Ok(())
}

fn apply_abandoned(
    spec: &FrozenTrialSpec,
    reducer: &mut ReplayState,
    attempt_id: StageAttemptId,
    diagnostic_fingerprint: BlobId,
) -> Result<(), TrialError> {
    ensure_replay_open(reducer)?;
    let record = *reducer
        .attempts
        .get(&attempt_id)
        .ok_or(TrialError::AttemptNotFound(attempt_id))?;
    if record.state != InternalAttemptState::Reserved {
        return Err(TrialError::AttemptNotReserved(attempt_id));
    }
    reducer
        .budget
        .reconcile(spec.budget(), record.reservation, BudgetAmount::default())?;
    reducer
        .attempts
        .get_mut(&attempt_id)
        .expect("attempt")
        .state = InternalAttemptState::Terminal(AttemptTerminal::Abandoned {
        diagnostic_fingerprint,
    });
    Ok(())
}

fn apply_closed(
    graph: &StageGraph,
    reducer: &mut ReplayState,
    disposition: TrialDisposition,
    diagnostic_fingerprint: Option<BlobId>,
    live_completion_evidence_fingerprint: Option<BlobId>,
) -> Result<(), TrialError> {
    ensure_replay_open(reducer)?;
    if has_active_attempt(reducer) {
        return Err(TrialError::ActiveAttempts);
    }
    if disposition == TrialDisposition::Completed
        && !reducer.stage_outputs.contains_key(&graph.output())
    {
        return Err(TrialError::ArchiveNotSucceeded);
    }
    if (disposition == TrialDisposition::Completed)
        != (diagnostic_fingerprint.is_none() && live_completion_evidence_fingerprint.is_some())
    {
        return Err(TrialError::InvalidDispositionEvidence);
    }
    reducer.status = Some(match disposition {
        TrialDisposition::Completed => TrialStatus::Completed,
        TrialDisposition::Failed => TrialStatus::Failed,
        TrialDisposition::Cancelled => TrialStatus::Cancelled,
    });
    Ok(())
}

fn ensure_replay_open(state: &ReplayState) -> Result<(), TrialError> {
    if !state.initialized {
        return Err(TrialError::MissingPreparedEvent);
    }
    if state.status.is_some_and(TrialStatus::is_terminal) {
        return Err(TrialError::TrialAlreadyTerminal);
    }
    Ok(())
}

fn ensure_stage_ready(
    reducer: &ReplayState,
    stage_spec: &FrozenStageSpec,
) -> Result<(), TrialError> {
    if reducer.stage_outputs.contains_key(&stage_spec.id()) {
        return Err(TrialError::StageAlreadySucceeded(stage_spec.id()));
    }
    if reducer.attempts.values().any(|attempt| {
        attempt.stage_id == stage_spec.id()
            && matches!(
                attempt.state,
                InternalAttemptState::Reserved | InternalAttemptState::Running { .. }
            )
    }) {
        return Err(TrialError::StageAttemptActive(stage_spec.id()));
    }
    for dependency in stage_spec.dependencies() {
        if !reducer.stage_outputs.contains_key(dependency) {
            return Err(TrialError::DependencyNotSatisfied {
                stage: stage_spec.id(),
                dependency: *dependency,
            });
        }
    }
    Ok(())
}

fn validate_reservation(
    spec: &FrozenTrialSpec,
    stage_id: StageId,
    stage: FrozenTrialStage,
    reservation: BudgetAmount,
) -> Result<(), TrialError> {
    reservation.verify_global_bounds()?;
    if spec.stage_budget_maximum(stage_id) != Some(reservation) {
        return Err(TrialError::InvalidReservation(stage));
    }
    let valid = match TrialWorkClass::for_stage(stage) {
        TrialWorkClass::Pure => {
            reservation.writer_tokens() == 0
                && reservation.controller_tokens() == 0
                && reservation.evaluations() == 0
        }
        TrialWorkClass::OptionalController => {
            reservation.writer_tokens() == 0 && reservation.evaluations() == 0
        }
        TrialWorkClass::Writer => {
            reservation.writer_tokens() == spec.declared_writer_token_maximum()
                && reservation.controller_tokens() == 0
                && reservation.evaluations() == 0
        }
        TrialWorkClass::Evaluation => {
            reservation.writer_tokens() == 0 && reservation.evaluations() > 0
        }
    };
    if !valid {
        return Err(TrialError::InvalidReservation(stage));
    }
    Ok(())
}

fn validate_live_terminal(
    stage: FrozenTrialStage,
    reservation: BudgetAmount,
    terminal: AttemptTerminal,
    actual_charge: BudgetAmount,
) -> Result<(), TrialError> {
    if matches!(
        terminal,
        AttemptTerminal::Interrupted { .. } | AttemptTerminal::Abandoned { .. }
    ) {
        return Err(TrialError::InvalidAttemptTerminal);
    }
    if !actual_charge.fits_within(reservation) {
        return Err(TrialError::Budget(BudgetError::ChargeExceedsReservation));
    }
    validate_charge_class(stage, terminal, actual_charge)
}

fn validate_terminal(
    spec: &FrozenTrialSpec,
    stage: FrozenTrialStage,
    terminal: AttemptTerminal,
    reservation: BudgetAmount,
    actual_charge: BudgetAmount,
) -> Result<(), TrialError> {
    if !actual_charge.fits_within(reservation) {
        return Err(TrialError::Budget(BudgetError::ChargeExceedsReservation));
    }
    validate_charge_class(stage, terminal, actual_charge)?;
    if let AttemptTerminal::Succeeded { output_fingerprint } = terminal {
        let expected = match stage {
            FrozenTrialStage::FreezeInputs => Some(spec.fingerprint()),
            FrozenTrialStage::CompilePrompt => Some(spec.prompt_content_fingerprint()),
            FrozenTrialStage::BacktranslateMask
            | FrozenTrialStage::Plan
            | FrozenTrialStage::Retrieve
            | FrozenTrialStage::Generate
            | FrozenTrialStage::Admit
            | FrozenTrialStage::Assemble
            | FrozenTrialStage::Gate
            | FrozenTrialStage::Evaluate
            | FrozenTrialStage::Describe
            | FrozenTrialStage::Archive => None,
        };
        if expected.is_some_and(|expected| expected != output_fingerprint) {
            return Err(TrialError::ExpectedOutputFingerprint);
        }
    }
    Ok(())
}

fn validate_charge_class(
    stage: FrozenTrialStage,
    terminal: AttemptTerminal,
    charge: BudgetAmount,
) -> Result<(), TrialError> {
    charge.verify_global_bounds()?;
    let succeeded = matches!(terminal, AttemptTerminal::Succeeded { .. });
    let valid = match TrialWorkClass::for_stage(stage) {
        TrialWorkClass::Pure => {
            charge.writer_tokens() == 0
                && charge.controller_tokens() == 0
                && charge.evaluations() == 0
        }
        TrialWorkClass::OptionalController => {
            charge.writer_tokens() == 0 && charge.evaluations() == 0
        }
        TrialWorkClass::Writer => {
            charge.controller_tokens() == 0
                && charge.evaluations() == 0
                && (!succeeded || charge.writer_tokens() > 0)
        }
        TrialWorkClass::Evaluation => {
            charge.writer_tokens() == 0 && (!succeeded || charge.evaluations() > 0)
        }
    };
    if !valid {
        return Err(TrialError::InvalidCharge(stage));
    }
    Ok(())
}

fn verify_command(
    spec: &FrozenTrialSpec,
    graph: &StageGraph,
    state: &ReplayState,
    session_id: TrialSessionId,
    command: &StageCommand,
) -> Result<(), TrialError> {
    if command.trial_run_id != state.trial_run_id.expect("prepared run")
        || command.trial_fingerprint != spec.fingerprint()
        || command.session_id != session_id
    {
        return Err(TrialError::CommandMismatch);
    }
    let record = *state
        .attempts
        .get(&command.attempt_id)
        .ok_or(TrialError::AttemptNotFound(command.attempt_id))?;
    let InternalAttemptState::Running {
        command_fingerprint,
    } = record.state
    else {
        return Err(TrialError::AttemptNotRunning(command.attempt_id));
    };
    let stage_spec = graph.stage(record.stage_id).expect("recorded stage exists");
    let expected = fingerprint_command(
        spec,
        command.trial_run_id,
        stage_spec,
        command.attempt_id,
        record,
    );
    if command.command_fingerprint != expected
        || command_fingerprint != expected
        || command.stage_id != record.stage_id
        || command.stage != stage_spec.stage()
        || command.attempt_ordinal != record.ordinal
        || command.stage_spec_fingerprint != stage_spec.spec_fingerprint()
        || command.input_fingerprint != record.input_fingerprint
        || command.reservation != record.reservation
        || command.reservation_event_fingerprint != record.reservation_event_fingerprint
        || command.start_event_fingerprint != record.start_event_fingerprint.expect("running start")
    {
        return Err(TrialError::CommandMismatch);
    }
    Ok(())
}

fn verify_terminal_lease(
    command: &StageCommand,
    lease: &VerifiedStageTerminalLease,
) -> Result<(), TrialError> {
    if lease.trial_run_id != command.trial_run_id
        || lease.trial_fingerprint != command.trial_fingerprint
        || lease.session_id != command.session_id
        || lease.attempt_id != command.attempt_id
        || lease.stage_id != command.stage_id
        || lease.command_fingerprint != command.command_fingerprint
        || lease.start_event_fingerprint != command.start_event_fingerprint
    {
        return Err(TrialError::TerminalLeaseMismatch);
    }
    validate_live_terminal(
        command.stage,
        command.reservation,
        lease.terminal,
        lease.actual_charge,
    )
}

fn build_command(
    spec: &FrozenTrialSpec,
    stage: &FrozenStageSpec,
    context: &CommandBuildContext,
) -> StageCommand {
    let CommandBuildContext {
        trial_run_id,
        session_id,
        attempt_id,
        record,
        command_fingerprint,
        start_event_fingerprint,
    } = *context;
    let is_writer = stage.stage() == FrozenTrialStage::Generate;
    StageCommand {
        trial_run_id,
        trial_fingerprint: spec.fingerprint(),
        session_id,
        stage_id: stage.id(),
        stage: stage.stage(),
        attempt_id,
        attempt_ordinal: record.ordinal,
        stage_spec_fingerprint: stage.spec_fingerprint(),
        input_fingerprint: record.input_fingerprint,
        reservation: record.reservation,
        reservation_event_fingerprint: record.reservation_event_fingerprint,
        command_fingerprint,
        start_event_fingerprint,
        prompt_content_fingerprint: is_writer.then_some(spec.prompt_content_fingerprint()),
        model_binding_fingerprint: is_writer.then_some(spec.model_binding_fingerprint()),
    }
}

fn fingerprint_stage_input(
    spec: &FrozenTrialSpec,
    stage_spec: &FrozenStageSpec,
    reducer: &ReplayState,
) -> Result<BlobId, TrialError> {
    let mut digest = Sha256::new();
    digest.update(STAGE_INPUT_DOMAIN);
    digest.update(
        reducer
            .trial_run_id
            .expect("prepared run before stage input")
            .as_ulid()
            .to_bytes(),
    );
    digest.update(spec.fingerprint().as_bytes());
    digest.update(stage_spec.id().as_ulid().to_bytes());
    digest.update(stage_spec.spec_fingerprint().as_bytes());
    digest.update((stage_spec.dependencies().len() as u64).to_be_bytes());
    for dependency in stage_spec.dependencies() {
        let output =
            reducer
                .stage_outputs
                .get(dependency)
                .ok_or(TrialError::DependencyNotSatisfied {
                    stage: stage_spec.id(),
                    dependency: *dependency,
                })?;
        digest.update(dependency.as_ulid().to_bytes());
        digest.update(output.as_bytes());
    }
    Ok(BlobId::from_bytes(digest.finalize().into()))
}

fn fingerprint_command(
    spec: &FrozenTrialSpec,
    trial_run_id: TrialRunId,
    stage: &FrozenStageSpec,
    attempt_id: StageAttemptId,
    record: AttemptRecord,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(STAGE_COMMAND_DOMAIN);
    digest.update(trial_run_id.as_ulid().to_bytes());
    digest.update(spec.fingerprint().as_bytes());
    digest.update(stage.id().as_ulid().to_bytes());
    digest.update([stage_kind_tag(stage.stage())]);
    digest.update(attempt_id.as_ulid().to_bytes());
    digest.update(record.ordinal.to_be_bytes());
    digest.update(stage.spec_fingerprint().as_bytes());
    digest.update(record.input_fingerprint.as_bytes());
    record.reservation.update_digest(&mut digest);
    digest.update(record.reservation_event_fingerprint.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_event(
    sequence: u32,
    previous_event_fingerprint: Option<BlobId>,
    kind: TrialEventKind,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(TRIAL_EVENT_DOMAIN);
    digest.update(sequence.to_be_bytes());
    update_optional_blob(&mut digest, previous_event_fingerprint);
    match kind {
        TrialEventKind::Prepared {
            trial_run_id,
            trial_fingerprint,
            store_lease_fingerprint,
        } => {
            digest.update([0]);
            digest.update(trial_run_id.as_ulid().to_bytes());
            digest.update(trial_fingerprint.as_bytes());
            digest.update(store_lease_fingerprint.as_bytes());
        }
        TrialEventKind::AttemptReserved {
            attempt_id,
            stage_id,
            attempt_ordinal,
            reservation,
            input_fingerprint,
        } => {
            digest.update([1]);
            digest.update(attempt_id.as_ulid().to_bytes());
            digest.update(stage_id.as_ulid().to_bytes());
            digest.update(attempt_ordinal.to_be_bytes());
            reservation.update_digest(&mut digest);
            digest.update(input_fingerprint.as_bytes());
        }
        TrialEventKind::AttemptStarted {
            attempt_id,
            command_fingerprint,
        } => {
            digest.update([2]);
            digest.update(attempt_id.as_ulid().to_bytes());
            digest.update(command_fingerprint.as_bytes());
        }
        TrialEventKind::AttemptFinished {
            attempt_id,
            command_fingerprint,
            terminal,
            actual_charge,
            terminal_evidence,
        } => {
            digest.update([3]);
            digest.update(attempt_id.as_ulid().to_bytes());
            digest.update(command_fingerprint.as_bytes());
            digest.update([terminal.tag()]);
            digest.update(terminal.evidence_fingerprint().as_bytes());
            actual_charge.update_digest(&mut digest);
            digest.update([terminal_evidence.tag()]);
            digest.update(terminal_evidence.fingerprint().as_bytes());
        }
        TrialEventKind::AttemptAbandoned {
            attempt_id,
            diagnostic_fingerprint,
        } => {
            digest.update([4]);
            digest.update(attempt_id.as_ulid().to_bytes());
            digest.update(diagnostic_fingerprint.as_bytes());
        }
        TrialEventKind::TrialClosed {
            disposition,
            diagnostic_fingerprint,
            live_completion_evidence_fingerprint,
        } => {
            digest.update([5]);
            digest.update([match disposition {
                TrialDisposition::Completed => 0,
                TrialDisposition::Failed => 1,
                TrialDisposition::Cancelled => 2,
            }]);
            update_optional_blob(&mut digest, diagnostic_fingerprint);
            update_optional_blob(&mut digest, live_completion_evidence_fingerprint);
        }
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn decode_persisted_trial_events(
    spec: &FrozenTrialSpec,
    trial_run_id: TrialRunId,
    persisted: &[PersistedResearchJournalEvent],
) -> Result<Vec<TrialEvent>, TrialError> {
    if persisted.len() > MAX_TRIAL_EVENTS {
        return Err(TrialError::EventLimit);
    }
    let mut events = Vec::with_capacity(persisted.len());
    for row in persisted {
        let record: CanonicalTrialEventRecord =
            serde_json::from_slice(row.canonical_record_bytes())?;
        if serde_json::to_vec(&record)? != row.canonical_record_bytes()
            || record.format != TRIAL_EVENT_RECORD_FORMAT
            || record.project_id != spec.project_id()
            || record.trial_run_id != trial_run_id
            || record.trial_fingerprint != spec.fingerprint()
            || record.event.sequence != row.event_index()
            || record.event.previous_event_fingerprint != row.previous_event_fingerprint()
            || record.event.fingerprint != row.event_fingerprint()
            || BlobId::digest(row.canonical_record_bytes()) != row.record_fingerprint()
        {
            return Err(TrialError::MalformedJournalRecord);
        }
        if let TrialEventKind::Prepared {
            store_lease_fingerprint,
            ..
        } = record.event.kind
            && store_lease_fingerprint != record.store_lease_fingerprint
        {
            return Err(TrialError::MalformedJournalRecord);
        }
        events.push(record.event);
    }
    Ok(events)
}

const fn research_journal_budget(amount: BudgetAmount) -> ResearchJournalBudget {
    ResearchJournalBudget {
        writer_tokens: amount.writer_tokens(),
        controller_tokens: amount.controller_tokens(),
        evaluations: amount.evaluations(),
        wall_time_ms: amount.wall_time_ms(),
    }
}

fn persisted_trial_outcome(terminal: AttemptTerminal) -> Result<TrialStageOutcome, TrialError> {
    match terminal {
        AttemptTerminal::Succeeded { .. } => Ok(TrialStageOutcome::Succeeded),
        AttemptTerminal::Failed { .. } => Ok(TrialStageOutcome::Failed),
        AttemptTerminal::Cancelled { .. } => Ok(TrialStageOutcome::Cancelled),
        AttemptTerminal::Interrupted { .. } => Ok(TrialStageOutcome::Interrupted),
        AttemptTerminal::Abandoned { .. } => Err(TrialError::InvalidAttemptTerminal),
    }
}

fn recovery_diagnostic(
    trial_fingerprint: BlobId,
    event_head: BlobId,
    attempt_id: StageAttemptId,
    state: InternalAttemptState,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(RECOVERY_DIAGNOSTIC_DOMAIN);
    digest.update(trial_fingerprint.as_bytes());
    digest.update(event_head.as_bytes());
    digest.update(attempt_id.as_ulid().to_bytes());
    match state {
        InternalAttemptState::Reserved => digest.update([0]),
        InternalAttemptState::Running {
            command_fingerprint,
        } => {
            digest.update([1]);
            digest.update(command_fingerprint.as_bytes());
        }
        InternalAttemptState::Terminal(terminal) => {
            digest.update([2, terminal.tag()]);
            digest.update(terminal.evidence_fingerprint().as_bytes());
        }
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn update_optional_blob(digest: &mut Sha256, value: Option<BlobId>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.as_bytes());
        }
        None => digest.update([0]),
    }
}

fn public_attempt_status(state: InternalAttemptState) -> StageAttemptStatus {
    match state {
        InternalAttemptState::Reserved => StageAttemptStatus::Reserved,
        InternalAttemptState::Running { .. } => StageAttemptStatus::Running,
        InternalAttemptState::Terminal(terminal) => match terminal {
            AttemptTerminal::Succeeded { .. } => StageAttemptStatus::Succeeded,
            AttemptTerminal::Failed { .. } => StageAttemptStatus::Failed,
            AttemptTerminal::Cancelled { .. } => StageAttemptStatus::Cancelled,
            AttemptTerminal::Interrupted { .. } => StageAttemptStatus::Interrupted,
            AttemptTerminal::Abandoned { .. } => StageAttemptStatus::Abandoned,
        },
    }
}

fn has_active_attempt(state: &ReplayState) -> bool {
    state.attempts.values().any(|attempt| {
        matches!(
            attempt.state,
            InternalAttemptState::Reserved | InternalAttemptState::Running { .. }
        )
    })
}
