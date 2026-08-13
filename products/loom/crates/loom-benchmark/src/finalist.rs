//! Finalist qualification from verified, complete primary benchmark evidence.

use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::{BoundError, BoundedVec, TrialCaseId};
use loom_types::BlobId;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ArmBudget, AssignmentLabelMap, BenchmarkRunClass, BenchmarkSeal, BenchmarkSealError,
    BlindedPairOutcome, BuiltInGenreFunction, CandidateEvidenceRecord, FrontierProfileRole,
    FrontierReviewedProvisionalProfile, FrozenCandidateProfile, ProfileArm,
    QualificationBenchmarkRunJournal,
};

pub const MAX_CLOSE_READ_FINDINGS: usize = 64;
pub const MAX_FINALIST_BLOCKERS: usize = 16;
pub const MAX_FINALISTS: usize = 3;

const CLOSE_READ_FINGERPRINT_DOMAIN: &[u8] = b"loom/verified-close-read/v1\0";
const CHARGE_FINGERPRINT_DOMAIN: &[u8] = b"loom/verified-profile-charge/v1\0";
const COMPUTE_POLICY_DOMAIN: &[u8] = b"loom/benchmark-compute-policy/v1\0";
const FINALIST_SELECTION_DOMAIN: &[u8] = b"loom/benchmark-finalist-selection/v3\0";

const EVALUATOR_CALL_UNITS: u128 = 4_096;
const FRONTIER_CALL_UNITS: u128 = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case", tag = "defect")]
pub enum CloseReadDefect {
    TherapeuticSummary,
    BlurredBlocking,
    GenericEroticCompression,
    EventSubstitution,
    SourceCadenceImitation,
    RewardHackedPolish,
    Other { protocol_code: u32 },
}

impl CloseReadDefect {
    fn update_digest(self, digest: &mut Sha256) {
        match self {
            Self::TherapeuticSummary => digest.update([0]),
            Self::BlurredBlocking => digest.update([1]),
            Self::GenericEroticCompression => digest.update([2]),
            Self::EventSubstitution => digest.update([3]),
            Self::SourceCadenceImitation => digest.update([4]),
            Self::RewardHackedPolish => digest.update([5]),
            Self::Other { protocol_code } => {
                digest.update([6]);
                digest.update(protocol_code.to_be_bytes());
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CloseReadFinding {
    defect: CloseReadDefect,
    recurring: bool,
    unresolved: bool,
    evidence_receipt_fingerprint: BlobId,
}

impl CloseReadFinding {
    pub const fn defect(self) -> CloseReadDefect {
        self.defect
    }

    pub const fn is_blocking(self) -> bool {
        self.recurring && self.unresolved
    }

    fn update_digest(self, digest: &mut Sha256) {
        self.defect.update_digest(digest);
        digest.update([u8::from(self.recurring), u8::from(self.unresolved)]);
        digest.update(self.evidence_receipt_fingerprint.as_bytes());
    }

    #[cfg(test)]
    pub(crate) const fn for_test(
        defect: CloseReadDefect,
        recurring: bool,
        unresolved: bool,
        evidence_receipt_fingerprint: BlobId,
    ) -> Self {
        Self {
            defect,
            recurring,
            unresolved,
            evidence_receipt_fingerprint,
        }
    }
}

/// Diagnostic record emitted by a future manuscript-first close-read verifier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManuscriptFirstCloseReadRecord {
    seal_fingerprint: BlobId,
    profile_fingerprint: BlobId,
    reviewer_fingerprint: BlobId,
    reviewer_independence_receipt_fingerprint: BlobId,
    protocol_fingerprint: BlobId,
    manuscript_packet_fingerprint: BlobId,
    findings: BoundedVec<CloseReadFinding, MAX_CLOSE_READ_FINDINGS>,
    fingerprint: BlobId,
}

impl ManuscriptFirstCloseReadRecord {
    pub const fn profile_fingerprint(&self) -> BlobId {
        self.profile_fingerprint
    }

    pub fn findings(&self) -> &[CloseReadFinding] {
        &self.findings
    }

    pub fn has_blocking_defect(&self) -> bool {
        self.findings.iter().any(|finding| finding.is_blocking())
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(CLOSE_READ_FINGERPRINT_DOMAIN);
        digest.update(self.seal_fingerprint.as_bytes());
        digest.update(self.profile_fingerprint.as_bytes());
        digest.update(self.reviewer_fingerprint.as_bytes());
        digest.update(self.reviewer_independence_receipt_fingerprint.as_bytes());
        digest.update(self.protocol_fingerprint.as_bytes());
        digest.update(self.manuscript_packet_fingerprint.as_bytes());
        digest.update((self.findings.len() as u64).to_be_bytes());
        for finding in self.findings.iter().copied() {
            finding.update_digest(&mut digest);
        }
        BlobId::from_bytes(digest.finalize().into())
    }
}

/// Move-only close-read authority. No production issuer exists until the
/// blinded close-read adapter validates exact manuscripts and evidence spans.
#[must_use]
#[derive(Debug)]
pub struct VerifiedCloseReadLease {
    record: ManuscriptFirstCloseReadRecord,
}

impl VerifiedCloseReadLease {
    fn into_record(
        self,
        seal: &BenchmarkSeal,
    ) -> Result<ManuscriptFirstCloseReadRecord, FinalistError> {
        if self.record.seal_fingerprint != seal.fingerprint()
            || self.record.compute_fingerprint() != self.record.fingerprint
        {
            return Err(FinalistError::CloseReadCoverageMismatch);
        }
        Ok(self.record)
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        seal: &BenchmarkSeal,
        profile: &FrozenCandidateProfile,
        findings: Vec<CloseReadFinding>,
    ) -> Result<Self, FinalistError> {
        let findings = BoundedVec::new(findings)?;
        let mut unique = BTreeSet::new();
        if findings
            .iter()
            .any(|finding| !unique.insert(finding.defect()))
        {
            return Err(FinalistError::DuplicateCloseReadDefect);
        }
        let mut record = ManuscriptFirstCloseReadRecord {
            seal_fingerprint: seal.fingerprint(),
            profile_fingerprint: profile.fingerprint(),
            reviewer_fingerprint: BlobId::digest(b"test-independent-reviewer"),
            reviewer_independence_receipt_fingerprint: BlobId::digest(
                b"test-reviewer-independence",
            ),
            protocol_fingerprint: BlobId::digest(b"test-close-read-protocol"),
            manuscript_packet_fingerprint: BlobId::digest(
                &[
                    b"test-manuscript-packet".as_slice(),
                    profile.fingerprint().as_bytes(),
                ]
                .concat(),
            ),
            findings,
            fingerprint: BlobId::digest(&[]),
        };
        record.fingerprint = record.compute_fingerprint();
        Ok(Self { record })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct BudgetChargeTotals {
    writer_tokens: u64,
    controller_tokens: u64,
    evaluator_calls: u32,
    frontier_calls: u32,
    wall_time_ms: u64,
}

impl BudgetChargeTotals {
    pub const fn writer_tokens(self) -> u64 {
        self.writer_tokens
    }

    pub const fn controller_tokens(self) -> u64 {
        self.controller_tokens
    }

    pub const fn evaluator_calls(self) -> u32 {
        self.evaluator_calls
    }

    pub const fn frontier_calls(self) -> u32 {
        self.frontier_calls
    }

    pub const fn wall_time_ms(self) -> u64 {
        self.wall_time_ms
    }

    fn fits(self, budget: ArmBudget) -> bool {
        self.writer_tokens <= budget.writer_tokens()
            && self.controller_tokens <= budget.controller_tokens()
            && self.evaluator_calls <= budget.evaluator_calls()
            && self.frontier_calls <= budget.frontier_calls()
            && self.wall_time_ms <= budget.wall_time_ms()
    }

    fn update_digest(self, digest: &mut Sha256) {
        digest.update(self.writer_tokens.to_be_bytes());
        digest.update(self.controller_tokens.to_be_bytes());
        digest.update(self.evaluator_calls.to_be_bytes());
        digest.update(self.frontier_calls.to_be_bytes());
        digest.update(self.wall_time_ms.to_be_bytes());
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VerifiedProfileComputeCostRecord {
    seal_fingerprint: BlobId,
    profile_fingerprint: BlobId,
    charges: BudgetChargeTotals,
    compute_policy_fingerprint: BlobId,
    charge_ledger_receipt_fingerprint: BlobId,
    compute_units: u64,
    fingerprint: BlobId,
}

impl VerifiedProfileComputeCostRecord {
    pub const fn profile_fingerprint(&self) -> BlobId {
        self.profile_fingerprint
    }

    pub const fn charges(&self) -> BudgetChargeTotals {
        self.charges
    }

    pub const fn compute_units(&self) -> u64 {
        self.compute_units
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn validate(&self, seal: &BenchmarkSeal) -> Result<(), FinalistError> {
        let budget = seal
            .arm_budgets()
            .iter()
            .find(|allocation| allocation.profile_fingerprint() == self.profile_fingerprint)
            .map(|allocation| allocation.budget())
            .ok_or(FinalistError::CostCoverageMismatch)?;
        let derived_units = derive_compute_units(self.charges)?;
        if self.seal_fingerprint != seal.fingerprint()
            || self.compute_policy_fingerprint != compute_policy_fingerprint()
            || !self.charges.fits(budget)
            || derived_units == 0
            || derived_units != self.compute_units
            || self.compute_fingerprint() != self.fingerprint
        {
            return Err(FinalistError::InvalidVerifiedCost);
        }
        Ok(())
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(CHARGE_FINGERPRINT_DOMAIN);
        digest.update(self.seal_fingerprint.as_bytes());
        digest.update(self.profile_fingerprint.as_bytes());
        self.charges.update_digest(&mut digest);
        digest.update(self.compute_policy_fingerprint.as_bytes());
        digest.update(self.charge_ledger_receipt_fingerprint.as_bytes());
        digest.update(self.compute_units.to_be_bytes());
        BlobId::from_bytes(digest.finalize().into())
    }
}

/// Move-only authority from an immutable budget-charge ledger.
#[must_use]
#[derive(Debug)]
pub struct VerifiedProfileComputeCostLease {
    record: VerifiedProfileComputeCostRecord,
}

impl VerifiedProfileComputeCostLease {
    fn into_record(
        self,
        seal: &BenchmarkSeal,
    ) -> Result<VerifiedProfileComputeCostRecord, FinalistError> {
        self.record.validate(seal)?;
        Ok(self.record)
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        seal: &BenchmarkSeal,
        profile: &FrozenCandidateProfile,
        charges: BudgetChargeTotals,
    ) -> Result<Self, FinalistError> {
        let compute_units = derive_compute_units(charges)?;
        let mut record = VerifiedProfileComputeCostRecord {
            seal_fingerprint: seal.fingerprint(),
            profile_fingerprint: profile.fingerprint(),
            charges,
            compute_policy_fingerprint: compute_policy_fingerprint(),
            charge_ledger_receipt_fingerprint: BlobId::digest(
                &[
                    b"test-charge-ledger".as_slice(),
                    profile.fingerprint().as_bytes(),
                ]
                .concat(),
            ),
            compute_units,
            fingerprint: BlobId::digest(&[]),
        };
        record.fingerprint = record.compute_fingerprint();
        record.validate(seal)?;
        Ok(Self { record })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RateMillionths(u32);

impl RateMillionths {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(1_000_000);

    pub const fn millionths(self) -> u32 {
        self.0
    }

    fn from_unit(value: f64, rounding: RateRounding) -> Self {
        Self(round_bounded_rate(value, rounding))
    }
}

#[derive(Clone, Copy)]
enum RateRounding {
    Nearest,
    Floor,
    Ceil,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ClusteredWinInterval {
    point: RateMillionths,
    lower_95: RateMillionths,
    upper_95: RateMillionths,
    case_count: u32,
    cell_count: u32,
    abstention_count: u32,
}

impl ClusteredWinInterval {
    pub const fn point(self) -> RateMillionths {
        self.point
    }

    pub const fn lower_95(self) -> RateMillionths {
        self.lower_95
    }

    pub const fn upper_95(self) -> RateMillionths {
        self.upper_95
    }

    pub const fn case_count(self) -> u32 {
        self.case_count
    }

    pub const fn cell_count(self) -> u32 {
        self.cell_count
    }

    pub const fn abstention_count(self) -> u32 {
        self.abstention_count
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileConfirmatoryMetrics {
    profile_fingerprint: BlobId,
    overall: ClusteredWinInterval,
    genre_points: Vec<(BuiltInGenreFunction, RateMillionths)>,
    provenance_verified: u64,
    provenance_total: u64,
    assembly_verified: u64,
    assembly_total: u64,
    baseline_gate_rate: RateMillionths,
    contender_gate_rate: RateMillionths,
}

impl ProfileConfirmatoryMetrics {
    pub const fn profile_fingerprint(&self) -> BlobId {
        self.profile_fingerprint
    }

    pub const fn overall(&self) -> ClusteredWinInterval {
        self.overall
    }

    pub fn genre_points(&self) -> &[(BuiltInGenreFunction, RateMillionths)] {
        &self.genre_points
    }

    pub const fn provenance_is_complete(&self) -> bool {
        self.provenance_total != 0 && self.provenance_verified == self.provenance_total
    }

    pub const fn assembly_is_complete(&self) -> bool {
        self.assembly_total != 0 && self.assembly_verified == self.assembly_total
    }

    pub const fn baseline_gate_rate(&self) -> RateMillionths {
        self.baseline_gate_rate
    }

    pub const fn contender_gate_rate(&self) -> RateMillionths {
        self.contender_gate_rate
    }

    fn minimum_genre_point(&self) -> u32 {
        self.genre_points
            .iter()
            .map(|(_, rate)| rate.millionths())
            .min()
            .unwrap_or(0)
    }

    fn update_digest(&self, digest: &mut Sha256) {
        digest.update(self.profile_fingerprint.as_bytes());
        update_interval(self.overall, digest);
        digest.update((self.genre_points.len() as u64).to_be_bytes());
        for (function, point) in &self.genre_points {
            digest.update([function.ordinal()]);
            digest.update(point.millionths().to_be_bytes());
        }
        digest.update(self.provenance_verified.to_be_bytes());
        digest.update(self.provenance_total.to_be_bytes());
        digest.update(self.assembly_verified.to_be_bytes());
        digest.update(self.assembly_total.to_be_bytes());
        digest.update(self.baseline_gate_rate.millionths().to_be_bytes());
        digest.update(self.contender_gate_rate.millionths().to_be_bytes());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "blocker")]
pub enum FinalistBlocker {
    IncompleteProvenance,
    IncompleteAssembly,
    OverallPointNotAbove55,
    OverallLower95NotAbove50,
    GenreBelow50 { function: BuiltInGenreFunction },
    HardGateRegressionAbove2Points,
    RecurringCloseReadDefect,
    DominatedByCheaperEqualQuality { profile_fingerprint: BlobId },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FinalistAssessment {
    profile_fingerprint: BlobId,
    metrics: ProfileConfirmatoryMetrics,
    close_read: ManuscriptFirstCloseReadRecord,
    compute_cost: VerifiedProfileComputeCostRecord,
    blockers: BoundedVec<FinalistBlocker, MAX_FINALIST_BLOCKERS>,
}

impl FinalistAssessment {
    pub const fn profile_fingerprint(&self) -> BlobId {
        self.profile_fingerprint
    }

    pub const fn metrics(&self) -> &ProfileConfirmatoryMetrics {
        &self.metrics
    }

    pub fn blockers(&self) -> &[FinalistBlocker] {
        &self.blockers
    }

    pub fn is_eligible(&self) -> bool {
        self.blockers.is_empty()
    }

    pub const fn compute_cost(&self) -> &VerifiedProfileComputeCostRecord {
        &self.compute_cost
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct FinalistRoleAssignment {
    role: FrontierProfileRole,
    candidate_profile_fingerprint: BlobId,
}

impl FinalistRoleAssignment {
    pub const fn role(self) -> FrontierProfileRole {
        self.role
    }

    pub const fn candidate_profile_fingerprint(self) -> BlobId {
        self.candidate_profile_fingerprint
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FinalistSelection {
    seal_fingerprint: BlobId,
    journal_chain_head: BlobId,
    journal_record_fingerprint: BlobId,
    assessments: Vec<FinalistAssessment>,
    roles: BoundedVec<FinalistRoleAssignment, MAX_FINALISTS>,
    fingerprint: BlobId,
    reviewed_profiles: BoundedVec<FrontierReviewedProvisionalProfile, MAX_FINALISTS>,
}

impl FinalistSelection {
    pub const fn seal_fingerprint(&self) -> BlobId {
        self.seal_fingerprint
    }

    pub const fn journal_record_fingerprint(&self) -> BlobId {
        self.journal_record_fingerprint
    }

    pub const fn journal_chain_head(&self) -> BlobId {
        self.journal_chain_head
    }

    pub fn assessments(&self) -> &[FinalistAssessment] {
        &self.assessments
    }

    pub fn roles(&self) -> &[FinalistRoleAssignment] {
        &self.roles
    }

    pub fn reviewed_profiles(&self) -> &[FrontierReviewedProvisionalProfile] {
        &self.reviewed_profiles
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

pub fn evaluate_finalists(
    seal: &BenchmarkSeal,
    mapping: &AssignmentLabelMap,
    journal: &QualificationBenchmarkRunJournal,
    close_reads: Vec<VerifiedCloseReadLease>,
    costs: Vec<VerifiedProfileComputeCostLease>,
) -> Result<FinalistSelection, FinalistError> {
    seal.verify_integrity()?;
    seal.verify_mapping(mapping)?;
    if journal.seal_fingerprint() != seal.fingerprint() || !journal.primary_is_complete() {
        return Err(FinalistError::IncompletePrimaryMatrix);
    }
    let journal_record = journal
        .snapshot()
        .map_err(|error| FinalistError::JournalSnapshot(error.to_string()))?;
    let close_reads = exact_close_reads(seal, close_reads)?;
    let costs = exact_costs(seal, costs)?;
    let mut assessments = Vec::with_capacity(seal.contenders().len());
    for profile in seal.contenders() {
        let metrics = compute_profile_metrics(seal, mapping, journal, profile)?;
        let close_read = close_reads
            .get(&profile.fingerprint())
            .ok_or(FinalistError::CloseReadCoverageMismatch)?
            .clone();
        let compute_cost = costs
            .get(&profile.fingerprint())
            .ok_or(FinalistError::CostCoverageMismatch)?
            .clone();
        let blockers = threshold_blockers(&metrics, &close_read);
        assessments.push(FinalistAssessment {
            profile_fingerprint: profile.fingerprint(),
            metrics,
            close_read,
            compute_cost,
            blockers: BoundedVec::new(blockers)?,
        });
    }

    apply_dominance_blockers(&mut assessments)?;
    let role_assignments = select_frontier_roles(&assessments);
    let roles = BoundedVec::new(role_assignments)?;
    let fingerprint = fingerprint_selection(
        seal.fingerprint(),
        journal.chain_head(),
        journal_record.fingerprint(),
        &assessments,
        &roles,
    );
    let reviewed_profiles = roles
        .iter()
        .map(|assignment| {
            let candidate = seal
                .contender(assignment.candidate_profile_fingerprint)
                .ok_or(FinalistError::MappingMismatch)?
                .clone();
            Ok(FrontierReviewedProvisionalProfile::from_selection(
                candidate,
                assignment.role,
                seal.fingerprint(),
                journal.chain_head(),
                journal_record.fingerprint(),
                fingerprint,
            ))
        })
        .collect::<Result<Vec<_>, FinalistError>>()?;
    Ok(FinalistSelection {
        seal_fingerprint: seal.fingerprint(),
        journal_chain_head: journal.chain_head(),
        journal_record_fingerprint: journal_record.fingerprint(),
        assessments,
        roles,
        fingerprint,
        reviewed_profiles: BoundedVec::new(reviewed_profiles)?,
    })
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum FinalistError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error("benchmark seal validation failed: {0}")]
    Seal(String),
    #[error("close read repeats a defect")]
    DuplicateCloseReadDefect,
    #[error("primary assignment matrix is incomplete")]
    IncompletePrimaryMatrix,
    #[error("verified benchmark journal could not produce an exact snapshot: {0}")]
    JournalSnapshot(String),
    #[error("close-read leases do not cover every contender exactly once")]
    CloseReadCoverageMismatch,
    #[error("verified charge-ledger leases do not cover every contender exactly once")]
    CostCoverageMismatch,
    #[error("verified compute cost is invalid, over budget, or not derived from charges")]
    InvalidVerifiedCost,
    #[error("compute-unit arithmetic overflow")]
    ComputeOverflow,
    #[error("assignment label mapping is incomplete or refers to the wrong contender")]
    MappingMismatch,
    #[error("selected-N primary cells are incomplete for a case")]
    IncompleteCaseCells,
    #[error("case-clustered interval requires at least two cases")]
    TooFewCaseClusters,
    #[error("case/cell count cannot be represented")]
    CountOverflow,
}

impl From<BenchmarkSealError> for FinalistError {
    fn from(error: BenchmarkSealError) -> Self {
        Self::Seal(error.to_string())
    }
}

#[derive(Clone, Copy)]
enum RelativeOutcome {
    BaselineWin,
    ContenderWin,
    Tie,
    Abstain,
}

struct UnblindedCell<'a> {
    case_id: TrialCaseId,
    function: BuiltInGenreFunction,
    outcome: RelativeOutcome,
    contender: &'a CandidateEvidenceRecord,
    baseline: &'a CandidateEvidenceRecord,
}

fn compute_profile_metrics(
    seal: &BenchmarkSeal,
    mapping: &AssignmentLabelMap,
    journal: &QualificationBenchmarkRunJournal,
    profile: &FrozenCandidateProfile,
) -> Result<ProfileConfirmatoryMetrics, FinalistError> {
    let mut selected_cells = Vec::new();
    let mut provenance_verified = 0_u64;
    let mut provenance_total = 0_u64;
    let mut assembly_verified = 0_u64;
    let mut assembly_total = 0_u64;
    for (assignment, entry) in seal.assignments().iter().zip(mapping.entries()) {
        if assignment.assignment_id() != entry.assignment_id() {
            return Err(FinalistError::MappingMismatch);
        }
        let contender_arm = [entry.left_arm(), entry.right_arm()]
            .into_iter()
            .find(|arm| matches!(arm, ProfileArm::Contender { .. }))
            .ok_or(FinalistError::MappingMismatch)?;
        if contender_arm.profile_fingerprint() != profile.fingerprint() {
            continue;
        }
        let run_id = journal
            .primary_run_for_assignment(assignment.assignment_id())
            .ok_or(FinalistError::IncompletePrimaryMatrix)?;
        let run = journal
            .run(run_id)
            .ok_or(FinalistError::IncompletePrimaryMatrix)?;
        if run.class() != BenchmarkRunClass::Primary {
            return Err(FinalistError::IncompletePrimaryMatrix);
        }
        let pair = run.packet().pair();
        let (outcome, contender, baseline) =
            unblind_result(entry.left_arm(), run.outcome(), pair.left(), pair.right());
        provenance_total += 1;
        assembly_total += 1;
        provenance_verified += u64::from(contender.provenance_verified());
        assembly_verified += u64::from(contender.assembly_verified());
        if assignment.n() == profile.components().selected_n() {
            selected_cells.push(UnblindedCell {
                case_id: assignment.case_id(),
                function: assignment.function(),
                outcome,
                contender,
                baseline,
            });
        }
    }
    let overall = clustered_interval(&selected_cells)?;
    let mut genre_points = Vec::with_capacity(BuiltInGenreFunction::ALL.len());
    for function in BuiltInGenreFunction::ALL {
        let genre_cells = selected_cells
            .iter()
            .filter(|cell| cell.function == function)
            .collect::<Vec<_>>();
        genre_points.push((function, clustered_point(&genre_cells)?));
    }
    let baseline_gate_rate = boolean_rate(
        selected_cells
            .iter()
            .map(|cell| cell.baseline.hard_gate_passed()),
    )?;
    let contender_gate_rate = boolean_rate(
        selected_cells
            .iter()
            .map(|cell| cell.contender.hard_gate_passed()),
    )?;
    Ok(ProfileConfirmatoryMetrics {
        profile_fingerprint: profile.fingerprint(),
        overall,
        genre_points,
        provenance_verified,
        provenance_total,
        assembly_verified,
        assembly_total,
        baseline_gate_rate,
        contender_gate_rate,
    })
}

fn unblind_result<'a>(
    left_arm: ProfileArm,
    outcome: BlindedPairOutcome,
    left: &'a CandidateEvidenceRecord,
    right: &'a CandidateEvidenceRecord,
) -> (
    RelativeOutcome,
    &'a CandidateEvidenceRecord,
    &'a CandidateEvidenceRecord,
) {
    let left_is_contender = matches!(left_arm, ProfileArm::Contender { .. });
    let relative = match (outcome, left_is_contender) {
        (BlindedPairOutcome::LeftWin, true) | (BlindedPairOutcome::RightWin, false) => {
            RelativeOutcome::ContenderWin
        }
        (BlindedPairOutcome::RightWin, true) | (BlindedPairOutcome::LeftWin, false) => {
            RelativeOutcome::BaselineWin
        }
        (BlindedPairOutcome::Tie, _) => RelativeOutcome::Tie,
        (BlindedPairOutcome::Abstain, _) => RelativeOutcome::Abstain,
    };
    let (contender, baseline) = if left_is_contender {
        (left, right)
    } else {
        (right, left)
    };
    (relative, contender, baseline)
}

fn clustered_interval(cells: &[UnblindedCell<'_>]) -> Result<ClusteredWinInterval, FinalistError> {
    let by_case = scores_by_case(cells)?;
    if by_case.len() < 2 {
        return Err(FinalistError::TooFewCaseClusters);
    }
    let scores = by_case.values().map(|values| values.0).collect::<Vec<_>>();
    let count_u32 = u32::try_from(scores.len()).map_err(|_| FinalistError::CountOverflow)?;
    let count = f64::from(count_u32);
    let mean = scores.iter().sum::<f64>() / count;
    let squared_error = scores
        .iter()
        .map(|score| (score - mean).powi(2))
        .sum::<f64>();
    let variance = squared_error / f64::from(count_u32 - 1);
    let standard_error = (variance / count).sqrt();
    let critical = conservative_student_t_975(scores.len() - 1);
    let margin = critical * standard_error;
    let cell_count = u32::try_from(cells.len()).map_err(|_| FinalistError::CountOverflow)?;
    let abstention_count = u32::try_from(
        cells
            .iter()
            .filter(|cell| matches!(cell.outcome, RelativeOutcome::Abstain))
            .count(),
    )
    .map_err(|_| FinalistError::CountOverflow)?;
    Ok(ClusteredWinInterval {
        point: RateMillionths::from_unit(mean, RateRounding::Nearest),
        lower_95: RateMillionths::from_unit(mean - margin, RateRounding::Floor),
        upper_95: RateMillionths::from_unit(mean + margin, RateRounding::Ceil),
        case_count: count_u32,
        cell_count,
        abstention_count,
    })
}

fn clustered_point(cells: &[&UnblindedCell<'_>]) -> Result<RateMillionths, FinalistError> {
    let mut by_case = BTreeMap::<TrialCaseId, (u64, u64)>::new();
    for cell in cells {
        let entry = by_case.entry(cell.case_id).or_default();
        entry.0 += outcome_units(cell.outcome);
        entry.1 += 1;
    }
    if by_case.is_empty() {
        return Err(FinalistError::TooFewCaseClusters);
    }
    ensure_twelve_cells(&by_case)?;
    let units = by_case
        .values()
        .map(|(units, count)| case_unit_ratio(*units, *count))
        .collect::<Result<Vec<_>, _>>()?;
    let count = u32::try_from(units.len()).map_err(|_| FinalistError::CountOverflow)?;
    let mean = units.iter().sum::<f64>() / f64::from(count);
    Ok(RateMillionths::from_unit(mean, RateRounding::Nearest))
}

fn scores_by_case(
    cells: &[UnblindedCell<'_>],
) -> Result<BTreeMap<TrialCaseId, (f64, u64)>, FinalistError> {
    let mut raw = BTreeMap::<TrialCaseId, (u64, u64)>::new();
    for cell in cells {
        let entry = raw.entry(cell.case_id).or_default();
        entry.0 += outcome_units(cell.outcome);
        entry.1 += 1;
    }
    ensure_twelve_cells(&raw)?;
    raw.into_iter()
        .map(|(case, (units, count))| Ok((case, (case_unit_ratio(units, count)?, count))))
        .collect()
}

fn ensure_twelve_cells<T>(by_case: &BTreeMap<TrialCaseId, (T, u64)>) -> Result<(), FinalistError> {
    if by_case.values().any(|(_, count)| *count != 12) {
        return Err(FinalistError::IncompleteCaseCells);
    }
    Ok(())
}

fn outcome_units(outcome: RelativeOutcome) -> u64 {
    match outcome {
        RelativeOutcome::ContenderWin => 1_000_000,
        RelativeOutcome::Tie => 500_000,
        RelativeOutcome::BaselineWin | RelativeOutcome::Abstain => 0,
    }
}

fn boolean_rate(values: impl Iterator<Item = bool>) -> Result<RateMillionths, FinalistError> {
    let mut passed = 0_u128;
    let mut total = 0_u128;
    for value in values {
        passed += u128::from(value);
        total += 1;
    }
    if total == 0 {
        return Err(FinalistError::IncompletePrimaryMatrix);
    }
    let millionths = (passed * 1_000_000 + total / 2) / total;
    let millionths = u32::try_from(millionths).map_err(|_| FinalistError::CountOverflow)?;
    Ok(RateMillionths(millionths))
}

fn case_unit_ratio(units: u64, count: u64) -> Result<f64, FinalistError> {
    let units = u32::try_from(units).map_err(|_| FinalistError::CountOverflow)?;
    let count = u32::try_from(count).map_err(|_| FinalistError::CountOverflow)?;
    Ok(f64::from(units) / (f64::from(count) * 1_000_000.0))
}

fn round_bounded_rate(value: f64, rounding: RateRounding) -> u32 {
    let scaled = if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0) * 1_000_000.0
    };
    let mut low = 0_u32;
    let mut high = 1_000_000_u32;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if f64::from(middle) <= scaled {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    match rounding {
        RateRounding::Ceil if f64::from(low) < scaled => low + 1,
        RateRounding::Nearest if scaled - f64::from(low) >= 0.5 => low + 1,
        RateRounding::Floor | RateRounding::Nearest | RateRounding::Ceil => low,
    }
}

fn threshold_blockers(
    metrics: &ProfileConfirmatoryMetrics,
    close_read: &ManuscriptFirstCloseReadRecord,
) -> Vec<FinalistBlocker> {
    let mut blockers = Vec::new();
    if !metrics.provenance_is_complete() {
        blockers.push(FinalistBlocker::IncompleteProvenance);
    }
    if !metrics.assembly_is_complete() {
        blockers.push(FinalistBlocker::IncompleteAssembly);
    }
    if metrics.overall.point().millionths() <= 550_000 {
        blockers.push(FinalistBlocker::OverallPointNotAbove55);
    }
    if metrics.overall.lower_95().millionths() <= 500_000 {
        blockers.push(FinalistBlocker::OverallLower95NotAbove50);
    }
    for (function, point) in &metrics.genre_points {
        if point.millionths() < 500_000 {
            blockers.push(FinalistBlocker::GenreBelow50 {
                function: *function,
            });
        }
    }
    let regression = metrics
        .baseline_gate_rate
        .millionths()
        .saturating_sub(metrics.contender_gate_rate.millionths());
    if regression > 20_000 {
        blockers.push(FinalistBlocker::HardGateRegressionAbove2Points);
    }
    if close_read.has_blocking_defect() {
        blockers.push(FinalistBlocker::RecurringCloseReadDefect);
    }
    blockers
}

fn exact_close_reads(
    seal: &BenchmarkSeal,
    leases: Vec<VerifiedCloseReadLease>,
) -> Result<BTreeMap<BlobId, ManuscriptFirstCloseReadRecord>, FinalistError> {
    let expected = contender_fingerprints(seal);
    let mut records = BTreeMap::new();
    for lease in leases {
        let record = lease.into_record(seal)?;
        if records
            .insert(record.profile_fingerprint(), record)
            .is_some()
        {
            return Err(FinalistError::CloseReadCoverageMismatch);
        }
    }
    if records.keys().copied().collect::<BTreeSet<_>>() != expected {
        return Err(FinalistError::CloseReadCoverageMismatch);
    }
    Ok(records)
}

fn exact_costs(
    seal: &BenchmarkSeal,
    leases: Vec<VerifiedProfileComputeCostLease>,
) -> Result<BTreeMap<BlobId, VerifiedProfileComputeCostRecord>, FinalistError> {
    let expected = contender_fingerprints(seal);
    let mut records = BTreeMap::new();
    for lease in leases {
        let record = lease.into_record(seal)?;
        if records
            .insert(record.profile_fingerprint(), record)
            .is_some()
        {
            return Err(FinalistError::CostCoverageMismatch);
        }
    }
    if records.keys().copied().collect::<BTreeSet<_>>() != expected {
        return Err(FinalistError::CostCoverageMismatch);
    }
    Ok(records)
}

fn contender_fingerprints(seal: &BenchmarkSeal) -> BTreeSet<BlobId> {
    seal.contenders()
        .iter()
        .map(FrozenCandidateProfile::fingerprint)
        .collect()
}

fn apply_dominance_blockers(assessments: &mut [FinalistAssessment]) -> Result<(), FinalistError> {
    let prequalified = assessments
        .iter()
        .filter(|assessment| assessment.blockers.is_empty())
        .map(|assessment| assessment.profile_fingerprint)
        .collect::<BTreeSet<_>>();
    for index in 0..assessments.len() {
        if !prequalified.contains(&assessments[index].profile_fingerprint) {
            continue;
        }
        if let Some(dominator) = assessments.iter().find(|other| {
            other.profile_fingerprint != assessments[index].profile_fingerprint
                && prequalified.contains(&other.profile_fingerprint)
                && no_worse_on_every_quality_dimension(other, &assessments[index])
                && at_least_twenty_percent_cheaper(other, &assessments[index])
        }) {
            let mut blockers = assessments[index].blockers.clone().into_inner();
            blockers.push(FinalistBlocker::DominatedByCheaperEqualQuality {
                profile_fingerprint: dominator.profile_fingerprint,
            });
            assessments[index].blockers = BoundedVec::new(blockers)?;
        }
    }
    Ok(())
}

fn no_worse_on_every_quality_dimension(
    left: &FinalistAssessment,
    right: &FinalistAssessment,
) -> bool {
    left.metrics.overall.point().millionths() >= right.metrics.overall.point().millionths()
        && left.metrics.overall.lower_95().millionths()
            >= right.metrics.overall.lower_95().millionths()
        && left.metrics.contender_gate_rate().millionths()
            >= right.metrics.contender_gate_rate().millionths()
        && left
            .metrics
            .genre_points
            .iter()
            .all(|(function, left_point)| {
                right
                    .metrics
                    .genre_points
                    .iter()
                    .find(|(candidate, _)| candidate == function)
                    .is_some_and(|(_, right_point)| {
                        left_point.millionths() >= right_point.millionths()
                    })
            })
}

fn at_least_twenty_percent_cheaper(left: &FinalistAssessment, right: &FinalistAssessment) -> bool {
    u128::from(left.compute_cost.compute_units()) * 100
        <= u128::from(right.compute_cost.compute_units()) * 80
}

fn select_frontier_roles(assessments: &[FinalistAssessment]) -> Vec<FinalistRoleAssignment> {
    let eligible = assessments
        .iter()
        .filter(|assessment| assessment.is_eligible())
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return Vec::new();
    }
    let Some(efficient) = eligible
        .iter()
        .copied()
        .max_by(|left, right| compare_efficiency(left, right))
    else {
        return Vec::new();
    };
    let mut selected = BTreeSet::from([efficient.profile_fingerprint]);
    let mut roles = vec![FinalistRoleAssignment {
        role: FrontierProfileRole::EfficientKnee,
        candidate_profile_fingerprint: efficient.profile_fingerprint,
    }];

    if let Some(maximum) = eligible
        .iter()
        .copied()
        .filter(|assessment| !selected.contains(&assessment.profile_fingerprint))
        .max_by(|left, right| compare_quality(left, right))
    {
        selected.insert(maximum.profile_fingerprint);
        roles.push(FinalistRoleAssignment {
            role: FrontierProfileRole::MaximumQuality,
            candidate_profile_fingerprint: maximum.profile_fingerprint,
        });
    }

    if let Some(balanced) = eligible
        .iter()
        .copied()
        .filter(|assessment| !selected.contains(&assessment.profile_fingerprint))
        .max_by(|left, right| compare_balance(left, right))
    {
        roles.push(FinalistRoleAssignment {
            role: FrontierProfileRole::BalancedStudio,
            candidate_profile_fingerprint: balanced.profile_fingerprint,
        });
    }
    roles.sort_unstable_by_key(|assignment| assignment.role);
    roles
}

fn compare_efficiency(left: &FinalistAssessment, right: &FinalistAssessment) -> std::cmp::Ordering {
    let left_surplus = quality_surplus(left);
    let right_surplus = quality_surplus(right);
    (left_surplus * u128::from(right.compute_cost.compute_units()))
        .cmp(&(right_surplus * u128::from(left.compute_cost.compute_units())))
        .then_with(|| {
            right
                .compute_cost
                .compute_units()
                .cmp(&left.compute_cost.compute_units())
        })
        .then_with(|| right.profile_fingerprint.cmp(&left.profile_fingerprint))
}

fn compare_quality(left: &FinalistAssessment, right: &FinalistAssessment) -> std::cmp::Ordering {
    quality_tuple(left)
        .cmp(&quality_tuple(right))
        .then_with(|| {
            right
                .compute_cost
                .compute_units()
                .cmp(&left.compute_cost.compute_units())
        })
        .then_with(|| right.profile_fingerprint.cmp(&left.profile_fingerprint))
}

fn compare_balance(left: &FinalistAssessment, right: &FinalistAssessment) -> std::cmp::Ordering {
    let left_min = left.metrics.minimum_genre_point();
    let right_min = right.metrics.minimum_genre_point();
    left_min
        .cmp(&right_min)
        .then_with(|| {
            left.metrics
                .overall
                .lower_95()
                .millionths()
                .cmp(&right.metrics.overall.lower_95().millionths())
        })
        .then_with(|| {
            right
                .compute_cost
                .compute_units()
                .cmp(&left.compute_cost.compute_units())
        })
        .then_with(|| right.profile_fingerprint.cmp(&left.profile_fingerprint))
}

fn quality_surplus(assessment: &FinalistAssessment) -> u128 {
    u128::from(
        assessment
            .metrics
            .overall
            .point()
            .millionths()
            .saturating_sub(550_000),
    ) + u128::from(
        assessment
            .metrics
            .overall
            .lower_95()
            .millionths()
            .saturating_sub(500_000),
    ) + u128::from(
        assessment
            .metrics
            .minimum_genre_point()
            .saturating_sub(500_000),
    )
}

fn quality_tuple(assessment: &FinalistAssessment) -> (u32, u32, u32, u32) {
    (
        assessment.metrics.minimum_genre_point(),
        assessment.metrics.overall.lower_95().millionths(),
        assessment.metrics.overall.point().millionths(),
        assessment.metrics.contender_gate_rate().millionths(),
    )
}

fn derive_compute_units(charges: BudgetChargeTotals) -> Result<u64, FinalistError> {
    let units = u128::from(charges.writer_tokens)
        .checked_add(u128::from(charges.controller_tokens))
        .and_then(|value| {
            value.checked_add(u128::from(charges.evaluator_calls) * EVALUATOR_CALL_UNITS)
        })
        .and_then(|value| {
            value.checked_add(u128::from(charges.frontier_calls) * FRONTIER_CALL_UNITS)
        })
        .and_then(|value| value.checked_add(u128::from(charges.wall_time_ms)))
        .ok_or(FinalistError::ComputeOverflow)?;
    u64::try_from(units).map_err(|_| FinalistError::ComputeOverflow)
}

fn compute_policy_fingerprint() -> BlobId {
    let mut digest = Sha256::new();
    digest.update(COMPUTE_POLICY_DOMAIN);
    digest.update(EVALUATOR_CALL_UNITS.to_be_bytes());
    digest.update(FRONTIER_CALL_UNITS.to_be_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

const T_975: [f64; 30] = [
    12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160,
    2.145, 2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056,
    2.052, 2.048, 2.045, 2.042,
];

fn conservative_student_t_975(degrees_of_freedom: usize) -> f64 {
    T_975
        .get(degrees_of_freedom.saturating_sub(1))
        .copied()
        .unwrap_or(T_975[T_975.len() - 1])
}

fn fingerprint_selection(
    seal: BlobId,
    journal_head: BlobId,
    journal_record: BlobId,
    assessments: &[FinalistAssessment],
    roles: &[FinalistRoleAssignment],
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(FINALIST_SELECTION_DOMAIN);
    digest.update(seal.as_bytes());
    digest.update(journal_head.as_bytes());
    digest.update(journal_record.as_bytes());
    digest.update(compute_policy_fingerprint().as_bytes());
    digest.update((assessments.len() as u64).to_be_bytes());
    for assessment in assessments {
        assessment.metrics.update_digest(&mut digest);
        digest.update(assessment.close_read.fingerprint().as_bytes());
        digest.update(assessment.compute_cost.fingerprint().as_bytes());
        digest.update((assessment.blockers.len() as u64).to_be_bytes());
        for blocker in assessment.blockers.iter().copied() {
            update_blocker(blocker, &mut digest);
        }
    }
    digest.update((roles.len() as u64).to_be_bytes());
    for assignment in roles {
        digest.update([assignment.role.domain_tag()]);
        digest.update(assignment.candidate_profile_fingerprint.as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn update_interval(interval: ClusteredWinInterval, digest: &mut Sha256) {
    digest.update(interval.point.millionths().to_be_bytes());
    digest.update(interval.lower_95.millionths().to_be_bytes());
    digest.update(interval.upper_95.millionths().to_be_bytes());
    digest.update(interval.case_count.to_be_bytes());
    digest.update(interval.cell_count.to_be_bytes());
    digest.update(interval.abstention_count.to_be_bytes());
}

fn update_blocker(blocker: FinalistBlocker, digest: &mut Sha256) {
    match blocker {
        FinalistBlocker::IncompleteProvenance => digest.update([0]),
        FinalistBlocker::IncompleteAssembly => digest.update([1]),
        FinalistBlocker::OverallPointNotAbove55 => digest.update([2]),
        FinalistBlocker::OverallLower95NotAbove50 => digest.update([3]),
        FinalistBlocker::GenreBelow50 { function } => {
            digest.update([4, function.ordinal()]);
        }
        FinalistBlocker::HardGateRegressionAbove2Points => digest.update([5]),
        FinalistBlocker::RecurringCloseReadDefect => digest.update([6]),
        FinalistBlocker::DominatedByCheaperEqualQuality {
            profile_fingerprint,
        } => {
            digest.update([7]);
            digest.update(profile_fingerprint.as_bytes());
        }
    }
}

#[cfg(test)]
pub(crate) const fn test_charges(
    writer_tokens: u64,
    controller_tokens: u64,
    evaluator_calls: u32,
    frontier_calls: u32,
    wall_time_ms: u64,
) -> BudgetChargeTotals {
    BudgetChargeTotals {
        writer_tokens,
        controller_tokens,
        evaluator_calls,
        frontier_calls,
        wall_time_ms,
    }
}

#[cfg(test)]
pub(crate) fn boolean_rate_for_test(
    values: impl Iterator<Item = bool>,
) -> Result<RateMillionths, FinalistError> {
    boolean_rate(values)
}

#[cfg(test)]
pub(crate) fn student_t_for_test(degrees_of_freedom: usize) -> f64 {
    conservative_student_t_975(degrees_of_freedom)
}
