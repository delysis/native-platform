use loom_research_types::{StageAttemptId, StageId, TrialRunId};
use loom_types::{ArtifactId, BlobId};

use crate::{AttemptTerminal, BudgetAmount};

#[cfg(test)]
use crate::StageCommand;

/// Process-local identity of the one live executor for a frozen trial.
///
/// The identity is deliberately absent from the durable event format. Replayed
/// events describe prior work; they do not recreate the executor that did it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct TrialSessionId(ArtifactId);

impl TrialSessionId {
    pub(crate) const fn from_artifact_id(session_id: ArtifactId) -> Self {
        Self(session_id)
    }

    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self(ArtifactId::new())
    }
}

/// Sole affine proof that one stage attempt has a durable budget reservation.
///
/// It can only be consumed by starting or abandoning that exact reservation.
/// Serialized `AttemptReserved` events are claims and cannot recreate it.
///
/// ```compile_fail
/// use loom_trial::ReservedStagePermit;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<ReservedStagePermit>();
/// ```
///
/// ```compile_fail
/// use loom_trial::ReservedStagePermit;
/// fn consume(_: ReservedStagePermit) {}
/// fn duplicate(permit: ReservedStagePermit) {
///     consume(permit);
///     consume(permit);
/// }
/// ```
#[must_use]
#[derive(Debug)]
pub struct ReservedStagePermit {
    pub(crate) trial_run_id: TrialRunId,
    pub(crate) trial_fingerprint: BlobId,
    pub(crate) session_id: TrialSessionId,
    pub(crate) attempt_id: StageAttemptId,
    pub(crate) stage_id: StageId,
    pub(crate) reservation_event_fingerprint: BlobId,
}

impl ReservedStagePermit {
    pub const fn attempt_id(&self) -> StageAttemptId {
        self.attempt_id
    }

    pub const fn trial_run_id(&self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn stage_id(&self) -> StageId {
        self.stage_id
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        TrialRunId,
        BlobId,
        TrialSessionId,
        StageAttemptId,
        StageId,
        BlobId,
    ) {
        (
            self.trial_run_id,
            self.trial_fingerprint,
            self.session_id,
            self.attempt_id,
            self.stage_id,
            self.reservation_event_fingerprint,
        )
    }
}

/// Live terminal receipt for one exact running stage command.
///
/// Output identity and resource usage enter the journal only by consuming this
/// lease. There is no public or deserializing constructor. The future
/// inference/store/evaluation adapters are the intended production issuers.
///
/// ```compile_fail
/// use loom_trial::VerifiedStageTerminalLease;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<VerifiedStageTerminalLease>();
/// ```
///
/// ```compile_fail
/// use loom_trial::VerifiedStageTerminalLease;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<VerifiedStageTerminalLease>();
/// ```
#[must_use]
#[derive(Debug)]
pub struct VerifiedStageTerminalLease {
    pub(crate) trial_run_id: TrialRunId,
    pub(crate) trial_fingerprint: BlobId,
    pub(crate) session_id: TrialSessionId,
    pub(crate) attempt_id: StageAttemptId,
    pub(crate) stage_id: StageId,
    pub(crate) command_fingerprint: BlobId,
    pub(crate) start_event_fingerprint: BlobId,
    pub(crate) terminal: AttemptTerminal,
    pub(crate) actual_charge: BudgetAmount,
    pub(crate) live_terminal_evidence_fingerprint: BlobId,
}

impl VerifiedStageTerminalLease {
    pub(crate) fn into_parts(self) -> (AttemptTerminal, BudgetAmount, BlobId) {
        (
            self.terminal,
            self.actual_charge,
            self.live_terminal_evidence_fingerprint,
        )
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_for_tests(
        command: &StageCommand,
        terminal: AttemptTerminal,
        actual_charge: BudgetAmount,
        live_terminal_evidence_fingerprint: BlobId,
    ) -> Self {
        Self {
            trial_run_id: command.trial_run_id,
            trial_fingerprint: command.trial_fingerprint,
            session_id: command.session_id,
            attempt_id: command.attempt_id,
            stage_id: command.stage_id,
            command_fingerprint: command.command_fingerprint,
            start_event_fingerprint: command.start_event_fingerprint,
            terminal,
            actual_charge,
            live_terminal_evidence_fingerprint,
        }
    }
}

/// Read-only request for the store to verify an exact archive at an exact
/// current journal head.
///
/// This value is serializable evidence, not authority. Only a trusted adapter
/// may turn it into a [`VerifiedArchiveCompletionLease`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct TrialCompletionRequest {
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    archive_stage_id: StageId,
    archive_output_fingerprint: BlobId,
    archive_terminal_event_fingerprint: BlobId,
    current_event_fingerprint: BlobId,
}

impl TrialCompletionRequest {
    pub const fn trial_run_id(self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn trial_fingerprint(self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn archive_stage_id(self) -> StageId {
        self.archive_stage_id
    }

    pub const fn archive_output_fingerprint(self) -> BlobId {
        self.archive_output_fingerprint
    }

    pub const fn archive_terminal_event_fingerprint(self) -> BlobId {
        self.archive_terminal_event_fingerprint
    }

    pub const fn current_event_fingerprint(self) -> BlobId {
        self.current_event_fingerprint
    }

    pub(crate) const fn new(
        trial_run_id: TrialRunId,
        trial_fingerprint: BlobId,
        archive_stage_id: StageId,
        archive_output_fingerprint: BlobId,
        archive_terminal_event_fingerprint: BlobId,
        current_event_fingerprint: BlobId,
    ) -> Self {
        Self {
            trial_run_id,
            trial_fingerprint,
            archive_stage_id,
            archive_output_fingerprint,
            archive_terminal_event_fingerprint,
            current_event_fingerprint,
        }
    }
}

/// Affine proof that the exact archived output is durable and the journal head
/// has not moved since it was checked.
///
/// There is no production issuer until the transactional archive/store adapter
/// exists. Replaying a successful archive event cannot mint this lease.
///
/// ```compile_fail
/// use loom_trial::VerifiedArchiveCompletionLease;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<VerifiedArchiveCompletionLease>();
/// ```
#[must_use]
#[derive(Debug)]
pub struct VerifiedArchiveCompletionLease {
    pub(crate) trial_run_id: TrialRunId,
    pub(crate) trial_fingerprint: BlobId,
    pub(crate) session_id: TrialSessionId,
    pub(crate) archive_stage_id: StageId,
    pub(crate) archive_output_fingerprint: BlobId,
    pub(crate) archive_terminal_event_fingerprint: BlobId,
    pub(crate) current_event_fingerprint: BlobId,
    pub(crate) live_completion_evidence_fingerprint: BlobId,
}

impl VerifiedArchiveCompletionLease {
    pub(crate) fn into_parts(self) -> (BlobId, BlobId) {
        (
            self.archive_output_fingerprint,
            self.live_completion_evidence_fingerprint,
        )
    }
}

/// Move-only proof that one exact frozen trial completed against a verified,
/// durable archive at the returned journal head.
///
/// A future campaign adapter may consume this value. It is intentionally not
/// serializable or cloneable, so persisted trial events remain diagnostic
/// claims rather than campaign completion authority.
///
/// ```compile_fail
/// use loom_trial::VerifiedCompletedTrialLease;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<VerifiedCompletedTrialLease>();
/// ```
///
/// ```compile_fail
/// use loom_trial::VerifiedCompletedTrialLease;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<VerifiedCompletedTrialLease>();
/// ```
#[must_use]
#[derive(Debug)]
pub struct VerifiedCompletedTrialLease {
    pub(crate) trial_run_id: TrialRunId,
    pub(crate) trial_fingerprint: BlobId,
    pub(crate) trial_journal_fingerprint: BlobId,
    pub(crate) archive_output_fingerprint: BlobId,
    pub(crate) archive_terminal_event_fingerprint: BlobId,
    pub(crate) actual_charge: BudgetAmount,
    pub(crate) live_completion_evidence_fingerprint: BlobId,
}

impl VerifiedCompletedTrialLease {
    pub const fn trial_run_id(&self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn trial_journal_fingerprint(&self) -> BlobId {
        self.trial_journal_fingerprint
    }

    pub const fn archive_output_fingerprint(&self) -> BlobId {
        self.archive_output_fingerprint
    }

    pub const fn archive_terminal_event_fingerprint(&self) -> BlobId {
        self.archive_terminal_event_fingerprint
    }

    pub const fn actual_charge(&self) -> BudgetAmount {
        self.actual_charge
    }

    pub const fn live_completion_evidence_fingerprint(&self) -> BlobId {
        self.live_completion_evidence_fingerprint
    }

    /// Consumes completion authority at the campaign boundary and exposes
    /// only copyable facts. These facts are inspectable evidence; they cannot
    /// recreate this lease or complete another trial journal.
    pub const fn into_campaign_parts(self) -> VerifiedCompletedTrialParts {
        VerifiedCompletedTrialParts {
            trial_run_id: self.trial_run_id,
            trial_fingerprint: self.trial_fingerprint,
            trial_journal_fingerprint: self.trial_journal_fingerprint,
            archive_output_fingerprint: self.archive_output_fingerprint,
            archive_terminal_event_fingerprint: self.archive_terminal_event_fingerprint,
            actual_charge: self.actual_charge,
            live_completion_evidence_fingerprint: self.live_completion_evidence_fingerprint,
        }
    }
}

/// Inspectable terminal facts released only by consuming a verified completed
/// trial lease. This is evidence, not reusable execution authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedCompletedTrialParts {
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    trial_journal_fingerprint: BlobId,
    archive_output_fingerprint: BlobId,
    archive_terminal_event_fingerprint: BlobId,
    actual_charge: BudgetAmount,
    live_completion_evidence_fingerprint: BlobId,
}

impl VerifiedCompletedTrialParts {
    pub const fn trial_run_id(self) -> TrialRunId {
        self.trial_run_id
    }

    pub const fn trial_fingerprint(self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn trial_journal_fingerprint(self) -> BlobId {
        self.trial_journal_fingerprint
    }

    pub const fn archive_output_fingerprint(self) -> BlobId {
        self.archive_output_fingerprint
    }

    pub const fn archive_terminal_event_fingerprint(self) -> BlobId {
        self.archive_terminal_event_fingerprint
    }

    pub const fn actual_charge(self) -> BudgetAmount {
        self.actual_charge
    }

    pub const fn live_completion_evidence_fingerprint(self) -> BlobId {
        self.live_completion_evidence_fingerprint
    }
}
