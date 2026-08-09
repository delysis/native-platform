use std::fmt;

use loom_research_types::{ManifestKey, TrialCaseId, TrialRunId};
use loom_search::UnitScore;
use loom_trial::{BudgetAmount, VerifiedCompletedTrialLease, VerifiedCompletedTrialParts};
use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    CampaignBudgetAmount, CampaignTrialTerminal, HalvingOutcome, PressurePoint, PressurePolicy,
};

/// Causal identity for one campaign-level trial dispatch attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TrialAttemptId(TrialRunId);

impl TrialAttemptId {
    pub fn new() -> Self {
        Self(TrialRunId::new())
    }

    pub const fn from_trial_run_id(value: TrialRunId) -> Self {
        Self(value)
    }

    pub const fn as_trial_run_id(self) -> TrialRunId {
        self.0
    }
}

impl Default for TrialAttemptId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TrialAttemptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SchedulerSessionId(ArtifactId);

impl SchedulerSessionId {
    pub(crate) const fn from_artifact_id(session_id: ArtifactId) -> Self {
        Self(session_id)
    }

    pub(crate) const fn as_artifact_id(self) -> ArtifactId {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self(ArtifactId::new())
    }
}

/// Sole affine proof that a frozen trial maximum is durably reserved.
///
/// It is neither serializable nor cloneable. Consuming it is the only route to
/// either a dispatch permit or a pre-dispatch abandonment.
///
/// ```compile_fail
/// use loom_campaign::ReservedTrialPermit;
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<ReservedTrialPermit>();
/// ```
#[derive(Debug)]
pub struct ReservedTrialPermit {
    pub(crate) campaign_fingerprint: BlobId,
    pub(crate) session_id: SchedulerSessionId,
    pub(crate) attempt_id: TrialAttemptId,
    pub(crate) trial_fingerprint: BlobId,
    pub(crate) trial_spec_fingerprint: BlobId,
    pub(crate) reservation_event_fingerprint: BlobId,
}

impl ReservedTrialPermit {
    pub const fn attempt_id(&self) -> TrialAttemptId {
        self.attempt_id
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn trial_run_id(&self) -> TrialRunId {
        self.attempt_id.as_trial_run_id()
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        BlobId,
        SchedulerSessionId,
        TrialAttemptId,
        BlobId,
        BlobId,
        BlobId,
    ) {
        (
            self.campaign_fingerprint,
            self.session_id,
            self.attempt_id,
            self.trial_fingerprint,
            self.trial_spec_fingerprint,
            self.reservation_event_fingerprint,
        )
    }
}

impl Drop for ReservedTrialPermit {
    fn drop(&mut self) {
        // Deliberately empty. Dropping the affine value loses live release or
        // dispatch authority; crash recovery reconciles its durable reserve.
    }
}

/// Sole affine permission to execute one already-reserved frozen trial.
///
/// The dispatch event is durable before this value is returned. Losing it
/// after a crash requires conservative reconciliation by a future trusted
/// persistence adapter; it can never be downgraded to a reservation release.
#[derive(Debug)]
pub struct DispatchedTrialPermit {
    pub(crate) campaign_fingerprint: BlobId,
    pub(crate) session_id: SchedulerSessionId,
    pub(crate) attempt_id: TrialAttemptId,
    pub(crate) trial_fingerprint: BlobId,
    pub(crate) trial_spec_fingerprint: BlobId,
    pub(crate) reservation: CampaignBudgetAmount,
    pub(crate) dispatch_event_fingerprint: BlobId,
}

impl DispatchedTrialPermit {
    pub const fn attempt_id(&self) -> TrialAttemptId {
        self.attempt_id
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn trial_run_id(&self) -> TrialRunId {
        self.attempt_id.as_trial_run_id()
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        BlobId,
        SchedulerSessionId,
        TrialAttemptId,
        BlobId,
        BlobId,
        CampaignBudgetAmount,
        BlobId,
    ) {
        (
            self.campaign_fingerprint,
            self.session_id,
            self.attempt_id,
            self.trial_fingerprint,
            self.trial_spec_fingerprint,
            self.reservation,
            self.dispatch_event_fingerprint,
        )
    }
}

impl Drop for DispatchedTrialPermit {
    fn drop(&mut self) {
        // Deliberately empty: `Drop` makes affine consumption visible to
        // static analysis. Losing a permit is reconciled conservatively from
        // the durable dispatched event on the next live journal session.
    }
}

/// Live terminal and usage proof derived from the exact move-only
/// `loom-trial` completion lease and campaign dispatch permit.
///
/// There is intentionally no production constructor. Serialized trial events,
/// fingerprints, booleans, or replay snapshots cannot mint this lease.
///
/// ```compile_fail
/// use loom_campaign::VerifiedTrialTerminalLease;
/// fn requires_deserialize<T: serde::de::DeserializeOwned>() {}
/// requires_deserialize::<VerifiedTrialTerminalLease>();
/// ```
#[derive(Debug)]
pub struct VerifiedTrialTerminalLease {
    pub(crate) campaign_fingerprint: BlobId,
    pub(crate) session_id: SchedulerSessionId,
    pub(crate) attempt_id: TrialAttemptId,
    pub(crate) trial_fingerprint: BlobId,
    pub(crate) trial_spec_fingerprint: BlobId,
    pub(crate) reservation: CampaignBudgetAmount,
    pub(crate) dispatch_event_fingerprint: BlobId,
    pub(crate) terminal: CampaignTrialTerminal,
    pub(crate) actual_charge: CampaignBudgetAmount,
    pub(crate) live_terminal_evidence_fingerprint: BlobId,
}

impl VerifiedTrialTerminalLease {
    pub const fn attempt_id(&self) -> TrialAttemptId {
        self.attempt_id
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub const fn trial_run_id(&self) -> TrialRunId {
        self.attempt_id.as_trial_run_id()
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        TrialAttemptId,
        CampaignTrialTerminal,
        CampaignBudgetAmount,
        BlobId,
    ) {
        (
            self.attempt_id,
            self.terminal,
            self.actual_charge,
            self.live_terminal_evidence_fingerprint,
        )
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_for_tests(
        permit: &DispatchedTrialPermit,
        terminal: CampaignTrialTerminal,
        actual_charge: CampaignBudgetAmount,
        live_terminal_evidence_fingerprint: BlobId,
    ) -> Self {
        Self {
            campaign_fingerprint: permit.campaign_fingerprint,
            session_id: permit.session_id,
            attempt_id: permit.attempt_id,
            trial_fingerprint: permit.trial_fingerprint,
            trial_spec_fingerprint: permit.trial_spec_fingerprint,
            reservation: permit.reservation,
            dispatch_event_fingerprint: permit.dispatch_event_fingerprint,
            terminal,
            actual_charge,
            live_terminal_evidence_fingerprint,
        }
    }
}

/// Verifies one completed `loom-trial` execution against the exact affine
/// campaign dispatch that launched it.
///
/// Both inputs are consumed. The returned lease is the only value that can
/// finish the dispatch in a live [`crate::CampaignJournal`]. In particular,
/// the persisted trial journal head, archive hashes, or budget JSON cannot be
/// supplied independently by a caller.
pub fn verify_completed_trial(
    permit: DispatchedTrialPermit,
    completed: VerifiedCompletedTrialLease,
) -> Result<VerifiedTrialTerminalLease, CampaignAuthorityError> {
    let facts = CompletedTrialFacts::from_verified(completed.into_campaign_parts());
    verify_completed_trial_facts(permit, facts)
}

#[derive(Clone, Copy, Debug)]
struct CompletedTrialFacts {
    trial_run_id: TrialRunId,
    trial_fingerprint: BlobId,
    trial_journal_fingerprint: BlobId,
    archive_output_fingerprint: BlobId,
    archive_terminal_event_fingerprint: BlobId,
    actual_charge: BudgetAmount,
    live_completion_evidence_fingerprint: BlobId,
}

impl CompletedTrialFacts {
    fn from_verified(completed: VerifiedCompletedTrialParts) -> Self {
        Self {
            trial_run_id: completed.trial_run_id(),
            trial_fingerprint: completed.trial_fingerprint(),
            trial_journal_fingerprint: completed.trial_journal_fingerprint(),
            archive_output_fingerprint: completed.archive_output_fingerprint(),
            archive_terminal_event_fingerprint: completed.archive_terminal_event_fingerprint(),
            actual_charge: completed.actual_charge(),
            live_completion_evidence_fingerprint: completed.live_completion_evidence_fingerprint(),
        }
    }
}

fn verify_completed_trial_facts(
    permit: DispatchedTrialPermit,
    facts: CompletedTrialFacts,
) -> Result<VerifiedTrialTerminalLease, CampaignAuthorityError> {
    let (
        campaign_fingerprint,
        session_id,
        attempt_id,
        trial_fingerprint,
        trial_spec_fingerprint,
        reservation,
        dispatch_event_fingerprint,
    ) = permit.into_parts();
    if attempt_id.as_trial_run_id() != facts.trial_run_id
        || trial_spec_fingerprint != trial_fingerprint
        || facts.trial_fingerprint != trial_fingerprint
    {
        return Err(CampaignAuthorityError::TrialMismatch);
    }
    let actual_charge = CampaignBudgetAmount::new(
        facts.actual_charge.writer_tokens(),
        facts.actual_charge.controller_tokens(),
        facts.actual_charge.evaluations(),
        facts.actual_charge.wall_time_ms(),
    )?;
    if !actual_charge.fits(reservation) {
        return Err(CampaignAuthorityError::ChargeExceedsReservation);
    }
    let live_terminal_evidence_fingerprint = completed_trial_evidence_fingerprint(
        TerminalEvidenceBinding {
            campaign_fingerprint,
            session_id,
            attempt_id,
            trial_fingerprint,
            reservation,
            dispatch_event_fingerprint,
        },
        facts,
        actual_charge,
    );
    Ok(VerifiedTrialTerminalLease {
        campaign_fingerprint,
        session_id,
        attempt_id,
        trial_fingerprint,
        trial_spec_fingerprint,
        reservation,
        dispatch_event_fingerprint,
        terminal: CampaignTrialTerminal::Completed {
            trial_journal_fingerprint: facts.trial_journal_fingerprint,
        },
        actual_charge,
        live_terminal_evidence_fingerprint,
    })
}

#[derive(Clone, Copy)]
struct TerminalEvidenceBinding {
    campaign_fingerprint: BlobId,
    session_id: SchedulerSessionId,
    attempt_id: TrialAttemptId,
    trial_fingerprint: BlobId,
    reservation: CampaignBudgetAmount,
    dispatch_event_fingerprint: BlobId,
}

fn completed_trial_evidence_fingerprint(
    binding: TerminalEvidenceBinding,
    facts: CompletedTrialFacts,
    actual_charge: CampaignBudgetAmount,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/campaign-verified-trial-terminal/v1\0");
    digest.update(binding.campaign_fingerprint.as_bytes());
    digest.update(binding.session_id.0.as_ulid().to_bytes());
    digest.update(binding.attempt_id.as_trial_run_id().as_ulid().to_bytes());
    digest.update(binding.trial_fingerprint.as_bytes());
    digest.update(binding.dispatch_event_fingerprint.as_bytes());
    digest.update(binding.reservation.writer_tokens().to_be_bytes());
    digest.update(binding.reservation.controller_tokens().to_be_bytes());
    digest.update(binding.reservation.evaluations().to_be_bytes());
    digest.update(binding.reservation.wall_time_ms().to_be_bytes());
    digest.update(facts.trial_journal_fingerprint.as_bytes());
    digest.update(facts.archive_output_fingerprint.as_bytes());
    digest.update(facts.archive_terminal_event_fingerprint.as_bytes());
    digest.update(facts.live_completion_evidence_fingerprint.as_bytes());
    digest.update(actual_charge.writer_tokens().to_be_bytes());
    digest.update(actual_charge.controller_tokens().to_be_bytes());
    digest.update(actual_charge.evaluations().to_be_bytes());
    digest.update(actual_charge.wall_time_ms().to_be_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CampaignAuthorityError {
    #[error("completed trial does not match its exact dispatched frozen trial")]
    TrialMismatch,
    #[error("verified trial charge exceeds its exact campaign reservation")]
    ChargeExceedsReservation,
    #[error(transparent)]
    Budget(#[from] crate::CampaignBudgetError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EvaluationLeasePurpose {
    Halving { rung: u16 },
    Archive { parent_fingerprint: BlobId },
}

/// Nonforgeable evaluated-candidate evidence expected from the missing trusted
/// `loom-eval` adapter.
///
/// The lease is affine and purpose-bound. Persisted scores remain claims and
/// cannot be promoted into archive or halving authority.
#[derive(Debug)]
pub struct VerifiedEvaluatedCandidateLease {
    pub(crate) campaign_fingerprint: BlobId,
    pub(crate) session_id: SchedulerSessionId,
    pub(crate) purpose: EvaluationLeasePurpose,
    pub(crate) trial_fingerprint: BlobId,
    pub(crate) case_id: TrialCaseId,
    pub(crate) treatment_fingerprint: BlobId,
    pub(crate) occurrence_id: ArtifactId,
    pub(crate) content_blob_id: BlobId,
    pub(crate) evaluation_receipt_fingerprint: BlobId,
    pub(crate) coverage_fingerprint: BlobId,
    pub(crate) actual_charge: CampaignBudgetAmount,
    pub(crate) outcome: HalvingOutcome,
    pub(crate) quality: UnitScore,
    pub(crate) descriptors: Vec<(ManifestKey, UnitScore)>,
}

/// Live evidence that an exact ordered set of generated occurrences belongs
/// to one frozen case/treatment pool. No production constructor exists until
/// the trial runtime can compose exact Generate/Admit/Assemble authority for
/// the same run with current-store checked pool facts. SQL/journal rows alone
/// are deliberately insufficient.
///
/// ```compile_fail
/// # use loom_campaign::CampaignJournal;
/// # use loom_store::CheckedCampaignNestedPoolEvidence;
/// fn checked_rows_are_not_authority(
///     journal: &mut CampaignJournal,
///     checked: CheckedCampaignNestedPoolEvidence,
/// ) {
///     journal.record_nested_pool(checked);
/// }
/// ```
#[derive(Debug)]
pub struct VerifiedNestedPoolLease {
    pub(crate) campaign_fingerprint: BlobId,
    pub(crate) session_id: SchedulerSessionId,
    pub(crate) case_id: TrialCaseId,
    pub(crate) treatment_fingerprint: BlobId,
    pub(crate) ordered_occurrences: Vec<ArtifactId>,
    pub(crate) pool_evidence_fingerprint: BlobId,
}

impl VerifiedNestedPoolLease {
    #[cfg(test)]
    pub(crate) fn diagnostic_for_tests(
        campaign_fingerprint: BlobId,
        session_id: SchedulerSessionId,
        case_id: TrialCaseId,
        treatment_fingerprint: BlobId,
        ordered_occurrences: Vec<ArtifactId>,
    ) -> Self {
        Self {
            campaign_fingerprint,
            session_id,
            case_id,
            treatment_fingerprint,
            ordered_occurrences,
            pool_evidence_fingerprint: BlobId::digest(b"live admitted occurrence pool"),
        }
    }
}

/// Live, coverage-bound pressure observations. Serialized scalar points cannot
/// construct this lease.
#[derive(Debug)]
pub struct VerifiedPressureCurveLease {
    pub(crate) campaign_fingerprint: BlobId,
    pub(crate) session_id: SchedulerSessionId,
    pub(crate) family_fingerprint: BlobId,
    pub(crate) affected_trials: Vec<BlobId>,
    pub(crate) policy: PressurePolicy,
    pub(crate) points: Vec<PressurePoint>,
    pub(crate) evaluation_coverage_fingerprint: BlobId,
    pub(crate) evaluation_receipt_fingerprint: BlobId,
    pub(crate) execution_charges: Vec<(BlobId, CampaignBudgetAmount)>,
    pub(crate) prior_decision_fingerprint: Option<BlobId>,
}

impl VerifiedPressureCurveLease {
    #[cfg(test)]
    pub(crate) fn diagnostic_for_tests(input: DiagnosticPressureCurveInput) -> Self {
        Self {
            campaign_fingerprint: input.campaign_fingerprint,
            session_id: input.session_id,
            family_fingerprint: input.family_fingerprint,
            affected_trials: input.affected_trials,
            policy: input.policy,
            points: input.points,
            evaluation_coverage_fingerprint: BlobId::digest(b"pressure coverage"),
            evaluation_receipt_fingerprint: BlobId::digest(b"pressure evaluation receipt"),
            execution_charges: input.execution_charges,
            prior_decision_fingerprint: input.prior_decision_fingerprint,
        }
    }
}

#[cfg(test)]
pub(crate) struct DiagnosticPressureCurveInput {
    pub(crate) campaign_fingerprint: BlobId,
    pub(crate) session_id: SchedulerSessionId,
    pub(crate) family_fingerprint: BlobId,
    pub(crate) affected_trials: Vec<BlobId>,
    pub(crate) policy: PressurePolicy,
    pub(crate) points: Vec<PressurePoint>,
    pub(crate) execution_charges: Vec<(BlobId, CampaignBudgetAmount)>,
    pub(crate) prior_decision_fingerprint: Option<BlobId>,
}

impl VerifiedEvaluatedCandidateLease {
    pub const fn occurrence_id(&self) -> ArtifactId {
        self.occurrence_id
    }

    pub const fn trial_fingerprint(&self) -> BlobId {
        self.trial_fingerprint
    }

    pub(crate) fn into_halving_candidate(self) -> crate::HalvingCandidate {
        crate::HalvingCandidate::new(self.trial_fingerprint, self.occurrence_id, self.outcome)
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_for_tests(input: DiagnosticEvaluatedCandidateInput) -> Self {
        Self {
            campaign_fingerprint: input.campaign_fingerprint,
            session_id: input.session_id,
            purpose: input.purpose,
            trial_fingerprint: input.trial_fingerprint,
            case_id: input.case_id,
            treatment_fingerprint: input.treatment_fingerprint,
            occurrence_id: input.occurrence_id,
            content_blob_id: input.content_blob_id,
            evaluation_receipt_fingerprint: BlobId::digest(b"live evaluation receipt"),
            coverage_fingerprint: input.coverage_fingerprint,
            actual_charge: input.actual_charge,
            outcome: input.outcome,
            quality: input.quality,
            descriptors: input.descriptors,
        }
    }
}

#[cfg(test)]
pub(crate) struct DiagnosticEvaluatedCandidateInput {
    pub(crate) campaign_fingerprint: BlobId,
    pub(crate) session_id: SchedulerSessionId,
    pub(crate) purpose: EvaluationLeasePurpose,
    pub(crate) trial_fingerprint: BlobId,
    pub(crate) case_id: TrialCaseId,
    pub(crate) treatment_fingerprint: BlobId,
    pub(crate) occurrence_id: ArtifactId,
    pub(crate) content_blob_id: BlobId,
    pub(crate) coverage_fingerprint: BlobId,
    pub(crate) actual_charge: CampaignBudgetAmount,
    pub(crate) outcome: HalvingOutcome,
    pub(crate) quality: UnitScore,
    pub(crate) descriptors: Vec<(ManifestKey, UnitScore)>,
}

/// Affine confirmation derived directly from the current live campaign
/// reducer and exact event head that no required or runnable work remains.
#[derive(Debug)]
pub struct VerifiedSchedulerTerminalLease {
    pub(crate) campaign_fingerprint: BlobId,
    pub(crate) session_id: SchedulerSessionId,
    pub(crate) event_head_fingerprint: BlobId,
    pub(crate) scheduler_evidence_fingerprint: BlobId,
}

impl VerifiedSchedulerTerminalLease {
    pub(crate) fn into_evidence(self) -> BlobId {
        self.scheduler_evidence_fingerprint
    }
}

#[cfg(test)]
mod tests {
    use loom_trial::BudgetAmount;

    use super::*;

    fn permit(
        trial_fingerprint: BlobId,
        reservation: CampaignBudgetAmount,
    ) -> DispatchedTrialPermit {
        DispatchedTrialPermit {
            campaign_fingerprint: BlobId::digest(b"campaign"),
            session_id: SchedulerSessionId::new(),
            attempt_id: TrialAttemptId::new(),
            trial_fingerprint,
            trial_spec_fingerprint: trial_fingerprint,
            reservation,
            dispatch_event_fingerprint: BlobId::digest(b"dispatch"),
        }
    }

    fn duplicate_permit_for_test(source: &DispatchedTrialPermit) -> DispatchedTrialPermit {
        DispatchedTrialPermit {
            campaign_fingerprint: source.campaign_fingerprint,
            session_id: source.session_id,
            attempt_id: source.attempt_id,
            trial_fingerprint: source.trial_fingerprint,
            trial_spec_fingerprint: source.trial_spec_fingerprint,
            reservation: source.reservation,
            dispatch_event_fingerprint: source.dispatch_event_fingerprint,
        }
    }

    fn facts(trial_fingerprint: BlobId, charge: BudgetAmount) -> CompletedTrialFacts {
        CompletedTrialFacts {
            trial_fingerprint,
            trial_journal_fingerprint: BlobId::digest(b"trial journal head"),
            archive_output_fingerprint: BlobId::digest(b"archive output"),
            archive_terminal_event_fingerprint: BlobId::digest(b"archive terminal event"),
            actual_charge: charge,
            live_completion_evidence_fingerprint: BlobId::digest(b"live completion"),
        }
    }

    #[test]
    fn exact_completed_trial_facts_bind_budget_and_all_archive_identities() {
        let trial = BlobId::digest(b"trial");
        let reservation = CampaignBudgetAmount::new(20, 10, 3, 100).expect("reservation");
        let charge = BudgetAmount::new(12, 4, 2, 80).expect("charge");
        let exact_permit = permit(trial, reservation);
        let first = verify_completed_trial_facts(
            duplicate_permit_for_test(&exact_permit),
            facts(trial, charge),
        )
        .expect("matching completed trial");
        assert_eq!(
            first.actual_charge,
            CampaignBudgetAmount::new(12, 4, 2, 80).expect("exact campaign charge")
        );
        assert_eq!(
            first.terminal,
            CampaignTrialTerminal::Completed {
                trial_journal_fingerprint: BlobId::digest(b"trial journal head")
            }
        );

        let mut changed_journal = facts(trial, charge);
        changed_journal.trial_journal_fingerprint = BlobId::digest(b"different trial journal");
        let mut changed_output = facts(trial, charge);
        changed_output.archive_output_fingerprint = BlobId::digest(b"different archive output");
        let mut changed_archive_terminal = facts(trial, charge);
        changed_archive_terminal.archive_terminal_event_fingerprint =
            BlobId::digest(b"different archive terminal");
        let mut changed_completion = facts(trial, charge);
        changed_completion.live_completion_evidence_fingerprint =
            BlobId::digest(b"different live completion");
        for changed in [
            changed_journal,
            changed_output,
            changed_archive_terminal,
            changed_completion,
        ] {
            let observed =
                verify_completed_trial_facts(duplicate_permit_for_test(&exact_permit), changed)
                    .expect("independently verified changed completion");
            assert_ne!(
                first.live_terminal_evidence_fingerprint,
                observed.live_terminal_evidence_fingerprint
            );
        }
    }

    #[test]
    fn wrong_trial_and_cost_inflation_never_mint_a_terminal_lease() {
        let trial = BlobId::digest(b"trial");
        let reservation = CampaignBudgetAmount::new(20, 10, 3, 100).expect("reservation");
        let charge = BudgetAmount::new(12, 4, 2, 80).expect("charge");
        assert!(matches!(
            verify_completed_trial_facts(
                permit(trial, reservation),
                facts(BlobId::digest(b"other trial"), charge),
            ),
            Err(CampaignAuthorityError::TrialMismatch)
        ));

        let mut wrong_spec = permit(trial, reservation);
        wrong_spec.trial_spec_fingerprint = BlobId::digest(b"substituted frozen trial spec");
        assert!(matches!(
            verify_completed_trial_facts(wrong_spec, facts(trial, charge)),
            Err(CampaignAuthorityError::TrialMismatch)
        ));

        for inflated in [
            BudgetAmount::new(21, 4, 2, 80).expect("writer inflation"),
            BudgetAmount::new(12, 11, 2, 80).expect("controller inflation"),
            BudgetAmount::new(12, 4, 4, 80).expect("evaluation inflation"),
            BudgetAmount::new(12, 4, 2, 101).expect("wall-time inflation"),
        ] {
            assert!(matches!(
                verify_completed_trial_facts(permit(trial, reservation), facts(trial, inflated),),
                Err(CampaignAuthorityError::ChargeExceedsReservation)
            ));
        }
    }
}
