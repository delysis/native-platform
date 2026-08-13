use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::{BoundError, BoundedVec, ManifestKey, TrialCaseId};
use loom_types::BlobId;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const CONFIRMATORY_N_VALUES: [u8; 6] = [1, 2, 4, 8, 16, 32];
pub const FRESH_FRONTIER_RUNS: u8 = 3;
pub const ORDER_PERMUTATION_CELLS: u8 = 4;
pub const MIN_CASES_PER_GENRE: usize = 6;
pub const MAX_BENCHMARK_ASSIGNMENTS: usize = 500_000;

const CASE_FINGERPRINT_DOMAIN: &[u8] = b"loom/benchmark-case/v1\0";
const ASSIGNMENT_ID_DOMAIN: &[u8] = b"loom/benchmark-assignment/v1\0";
const LABEL_DOMAIN: &[u8] = b"loom/benchmark-opaque-label/v1\0";
const MAPPING_FINGERPRINT_DOMAIN: &[u8] = b"loom/benchmark-label-map/v1\0";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltInGenreFunction {
    IntimateRomanticTension,
    SuspenseThrillerCausality,
    SpeculativeWorldConsistency,
    MysteryRevealLogic,
    VoiceHeavyLiteraryCharacterWork,
}

impl BuiltInGenreFunction {
    pub const ALL: [Self; 5] = [
        Self::IntimateRomanticTension,
        Self::SuspenseThrillerCausality,
        Self::SpeculativeWorldConsistency,
        Self::MysteryRevealLogic,
        Self::VoiceHeavyLiteraryCharacterWork,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::IntimateRomanticTension => "intimate_romantic_tension",
            Self::SuspenseThrillerCausality => "suspense_thriller_causality",
            Self::SpeculativeWorldConsistency => "speculative_world_consistency",
            Self::MysteryRevealLogic => "mystery_reveal_logic",
            Self::VoiceHeavyLiteraryCharacterWork => "voice_heavy_literary_character_work",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|function| function.id() == id)
    }

    pub const fn ordinal(self) -> u8 {
        match self {
            Self::IntimateRomanticTension => 0,
            Self::SuspenseThrillerCausality => 1,
            Self::SpeculativeWorldConsistency => 2,
            Self::MysteryRevealLogic => 3,
            Self::VoiceHeavyLiteraryCharacterWork => 4,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BenchmarkCaseBinding {
    manifest_id: ManifestKey,
    case_id: TrialCaseId,
    function: BuiltInGenreFunction,
    work_fingerprint: BlobId,
    project_ancestry_fingerprint: BlobId,
    source_blob_id: BlobId,
    fingerprint: BlobId,
}

impl BenchmarkCaseBinding {
    pub fn new(
        manifest_id: ManifestKey,
        case_id: TrialCaseId,
        function: BuiltInGenreFunction,
        work_fingerprint: BlobId,
        project_ancestry_fingerprint: BlobId,
        source_blob_id: BlobId,
    ) -> Self {
        let fingerprint = fingerprint_case(
            &manifest_id,
            case_id,
            function,
            work_fingerprint,
            project_ancestry_fingerprint,
            source_blob_id,
        );
        Self {
            manifest_id,
            case_id,
            function,
            work_fingerprint,
            project_ancestry_fingerprint,
            source_blob_id,
            fingerprint,
        }
    }

    pub const fn manifest_id(&self) -> &ManifestKey {
        &self.manifest_id
    }

    pub const fn case_id(&self) -> TrialCaseId {
        self.case_id
    }

    pub const fn function(&self) -> BuiltInGenreFunction {
        self.function
    }

    pub const fn work_fingerprint(&self) -> BlobId {
        self.work_fingerprint
    }

    pub const fn project_ancestry_fingerprint(&self) -> BlobId {
        self.project_ancestry_fingerprint
    }

    pub const fn source_blob_id(&self) -> BlobId {
        self.source_blob_id
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct ArmBudget {
    writer_tokens: u64,
    controller_tokens: u64,
    evaluator_calls: u32,
    frontier_calls: u32,
    wall_time_ms: u64,
}

impl ArmBudget {
    pub fn new(
        writer_tokens: u64,
        controller_tokens: u64,
        evaluator_calls: u32,
        frontier_calls: u32,
        wall_time_ms: u64,
    ) -> Result<Self, AssignmentError> {
        if writer_tokens == 0 || evaluator_calls == 0 || frontier_calls == 0 || wall_time_ms == 0 {
            return Err(AssignmentError::EmptyArmBudget);
        }
        Ok(Self {
            writer_tokens,
            controller_tokens,
            evaluator_calls,
            frontier_calls,
            wall_time_ms,
        })
    }

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

    pub(crate) fn update_digest(self, digest: &mut Sha256) {
        digest.update(self.writer_tokens.to_be_bytes());
        digest.update(self.controller_tokens.to_be_bytes());
        digest.update(self.evaluator_calls.to_be_bytes());
        digest.update(self.frontier_calls.to_be_bytes());
        digest.update(self.wall_time_ms.to_be_bytes());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ArmBudgetAllocation {
    profile_fingerprint: BlobId,
    budget: ArmBudget,
}

impl ArmBudgetAllocation {
    pub const fn new(profile_fingerprint: BlobId, budget: ArmBudget) -> Self {
        Self {
            profile_fingerprint,
            budget,
        }
    }

    pub const fn profile_fingerprint(self) -> BlobId {
        self.profile_fingerprint
    }

    pub const fn budget(self) -> ArmBudget {
        self.budget
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "arm")]
pub enum ProfileArm {
    Baseline { profile_fingerprint: BlobId },
    Contender { profile_fingerprint: BlobId },
}

impl ProfileArm {
    pub const fn profile_fingerprint(self) -> BlobId {
        match self {
            Self::Baseline {
                profile_fingerprint,
            }
            | Self::Contender {
                profile_fingerprint,
            } => profile_fingerprint,
        }
    }

    fn domain_tag(self) -> u8 {
        match self {
            Self::Baseline { .. } => 0,
            Self::Contender { .. } => 1,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct OpaqueCandidateLabel(String);

impl OpaqueCandidateLabel {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for OpaqueCandidateLabel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let Some(suffix) = value.strip_prefix("candidate-") else {
            return Err(serde::de::Error::custom("invalid opaque candidate label"));
        };
        if suffix.len() != 24
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(serde::de::Error::custom("invalid opaque candidate label"));
        }
        Ok(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct PermutationCell {
    index: u8,
    candidate_order_reversed: bool,
    criterion_anchor_order_permuted: bool,
}

impl PermutationCell {
    pub fn from_index(index: u8) -> Result<Self, AssignmentError> {
        if index >= ORDER_PERMUTATION_CELLS {
            return Err(AssignmentError::InvalidPermutationCell(index));
        }
        Ok(Self {
            index,
            candidate_order_reversed: index & 1 != 0,
            criterion_anchor_order_permuted: index & 2 != 0,
        })
    }

    pub const fn index(self) -> u8 {
        self.index
    }

    pub const fn candidate_order_reversed(self) -> bool {
        self.candidate_order_reversed
    }

    pub const fn criterion_anchor_order_permuted(self) -> bool {
        self.criterion_anchor_order_permuted
    }
}

/// Evaluator-visible assignment. It contains no profile fingerprint or arm map.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BlindedBenchmarkAssignment {
    assignment_id: BlobId,
    case_id: TrialCaseId,
    function: BuiltInGenreFunction,
    n: u8,
    fresh_run: u8,
    permutation_cell: PermutationCell,
    left_label: OpaqueCandidateLabel,
    right_label: OpaqueCandidateLabel,
    permutation_seed: BlobId,
}

impl BlindedBenchmarkAssignment {
    pub const fn assignment_id(&self) -> BlobId {
        self.assignment_id
    }

    pub const fn case_id(&self) -> TrialCaseId {
        self.case_id
    }

    pub const fn function(&self) -> BuiltInGenreFunction {
        self.function
    }

    pub const fn n(&self) -> u8 {
        self.n
    }

    pub const fn fresh_run(&self) -> u8 {
        self.fresh_run
    }

    pub const fn permutation_cell(&self) -> PermutationCell {
        self.permutation_cell
    }

    pub const fn left_label(&self) -> &OpaqueCandidateLabel {
        &self.left_label
    }

    pub const fn right_label(&self) -> &OpaqueCandidateLabel {
        &self.right_label
    }

    pub const fn permutation_seed(&self) -> BlobId {
        self.permutation_seed
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AssignmentLabelMapEntry {
    assignment_id: BlobId,
    left_label: OpaqueCandidateLabel,
    left_arm: ProfileArm,
    right_label: OpaqueCandidateLabel,
    right_arm: ProfileArm,
}

impl AssignmentLabelMapEntry {
    pub const fn assignment_id(&self) -> BlobId {
        self.assignment_id
    }

    pub const fn left_arm(&self) -> ProfileArm {
        self.left_arm
    }

    pub const fn right_arm(&self) -> ProfileArm {
        self.right_arm
    }

    pub fn resolve(&self, label: &OpaqueCandidateLabel) -> Option<ProfileArm> {
        if label == &self.left_label {
            Some(self.left_arm)
        } else if label == &self.right_label {
            Some(self.right_arm)
        } else {
            None
        }
    }

    fn update_digest(&self, digest: &mut Sha256) {
        digest.update(self.assignment_id.as_bytes());
        update_label(&self.left_label, digest);
        update_arm(self.left_arm, digest);
        update_label(&self.right_label, digest);
        update_arm(self.right_arm, digest);
    }
}

/// Mapping kept physically separate from evaluator-visible assignments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AssignmentLabelMap {
    seal_schedule_fingerprint: BlobId,
    entries: BoundedVec<AssignmentLabelMapEntry, MAX_BENCHMARK_ASSIGNMENTS>,
    #[serde(skip)]
    index: BTreeMap<BlobId, usize>,
    fingerprint: BlobId,
}

impl AssignmentLabelMap {
    pub const fn seal_schedule_fingerprint(&self) -> BlobId {
        self.seal_schedule_fingerprint
    }

    pub fn entries(&self) -> &[AssignmentLabelMapEntry] {
        &self.entries
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn entry(&self, assignment_id: BlobId) -> Option<&AssignmentLabelMapEntry> {
        self.index
            .get(&assignment_id)
            .and_then(|index| self.entries.get(*index))
    }

    pub(crate) fn verify_integrity(&self) -> Result<(), AssignmentError> {
        if fingerprint_mapping(self.seal_schedule_fingerprint, &self.entries) != self.fingerprint {
            return Err(AssignmentError::MappingFingerprintMismatch);
        }
        let mut assignments = BTreeSet::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if !assignments.insert(entry.assignment_id)
                || entry.left_label == entry.right_label
                || entry.left_arm.profile_fingerprint() == entry.right_arm.profile_fingerprint()
                || self.index.get(&entry.assignment_id) != Some(&index)
            {
                return Err(AssignmentError::InvalidMapping);
            }
        }
        if self.index.len() != self.entries.len() {
            return Err(AssignmentError::InvalidMapping);
        }
        Ok(())
    }
}

pub(crate) struct BuiltAssignmentMatrix {
    pub assignments: BoundedVec<BlindedBenchmarkAssignment, MAX_BENCHMARK_ASSIGNMENTS>,
    pub mapping: AssignmentLabelMap,
}

pub(crate) fn build_assignment_matrix(
    schedule_fingerprint: BlobId,
    seed: u64,
    baseline_fingerprint: BlobId,
    contender_fingerprints: &[BlobId],
    cases: &[BenchmarkCaseBinding],
) -> Result<BuiltAssignmentMatrix, AssignmentError> {
    let expected = contender_fingerprints
        .len()
        .checked_mul(cases.len())
        .and_then(|value| value.checked_mul(CONFIRMATORY_N_VALUES.len()))
        .and_then(|value| value.checked_mul(usize::from(FRESH_FRONTIER_RUNS)))
        .and_then(|value| value.checked_mul(usize::from(ORDER_PERMUTATION_CELLS)))
        .ok_or(AssignmentError::AssignmentCountOverflow)?;
    if expected > MAX_BENCHMARK_ASSIGNMENTS {
        return Err(AssignmentError::TooManyAssignments {
            actual: expected,
            maximum: MAX_BENCHMARK_ASSIGNMENTS,
        });
    }

    let mut assignments = Vec::with_capacity(expected);
    let mut entries = Vec::with_capacity(expected);
    let mut assignment_ids = BTreeSet::new();
    let mut opaque_labels = BTreeSet::new();
    for contender_fingerprint in contender_fingerprints {
        for case in cases {
            for n in CONFIRMATORY_N_VALUES {
                for fresh_run in 0..FRESH_FRONTIER_RUNS {
                    for cell_index in 0..ORDER_PERMUTATION_CELLS {
                        let cell = PermutationCell::from_index(cell_index)?;
                        let assignment_id = fingerprint_assignment_id(
                            schedule_fingerprint,
                            *contender_fingerprint,
                            case,
                            n,
                            fresh_run,
                            cell,
                        );
                        if !assignment_ids.insert(assignment_id) {
                            return Err(AssignmentError::AssignmentIdCollision);
                        }
                        let left_label = derive_label(seed, assignment_id, 0);
                        let right_label = derive_label(seed, assignment_id, 1);
                        if left_label == right_label
                            || !opaque_labels.insert(left_label.clone())
                            || !opaque_labels.insert(right_label.clone())
                        {
                            return Err(AssignmentError::OpaqueLabelCollision);
                        }
                        let orientation = derive_orientation(seed, assignment_id)
                            ^ cell.candidate_order_reversed();
                        let baseline = ProfileArm::Baseline {
                            profile_fingerprint: baseline_fingerprint,
                        };
                        let contender = ProfileArm::Contender {
                            profile_fingerprint: *contender_fingerprint,
                        };
                        let (left_arm, right_arm) = if orientation {
                            (contender, baseline)
                        } else {
                            (baseline, contender)
                        };
                        let permutation_seed = derive_permutation_seed(seed, assignment_id);
                        assignments.push(BlindedBenchmarkAssignment {
                            assignment_id,
                            case_id: case.case_id(),
                            function: case.function(),
                            n,
                            fresh_run,
                            permutation_cell: cell,
                            left_label: left_label.clone(),
                            right_label: right_label.clone(),
                            permutation_seed,
                        });
                        entries.push(AssignmentLabelMapEntry {
                            assignment_id,
                            left_label,
                            left_arm,
                            right_label,
                            right_arm,
                        });
                    }
                }
            }
        }
    }
    if assignments.len() != expected || entries.len() != expected {
        return Err(AssignmentError::IncompleteAssignmentMatrix);
    }
    let entries = BoundedVec::new(entries)?;
    let index = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.assignment_id(), index))
        .collect();
    let mapping = AssignmentLabelMap {
        seal_schedule_fingerprint: schedule_fingerprint,
        fingerprint: fingerprint_mapping(schedule_fingerprint, &entries),
        entries,
        index,
    };
    Ok(BuiltAssignmentMatrix {
        assignments: BoundedVec::new(assignments)?,
        mapping,
    })
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AssignmentError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error("arm budget requires nonzero writer, evaluator, frontier, and wall-time maxima")]
    EmptyArmBudget,
    #[error("permutation cell {0} is outside 0..4")]
    InvalidPermutationCell(u8),
    #[error("assignment count arithmetic overflow")]
    AssignmentCountOverflow,
    #[error("assignment matrix has {actual} rows; maximum is {maximum}")]
    TooManyAssignments { actual: usize, maximum: usize },
    #[error("assignment identifier collision")]
    AssignmentIdCollision,
    #[error("opaque label collision")]
    OpaqueLabelCollision,
    #[error("assignment matrix is incomplete")]
    IncompleteAssignmentMatrix,
    #[error("assignment mapping fingerprint mismatch")]
    MappingFingerprintMismatch,
    #[error("assignment mapping repeats or conflates an assignment")]
    InvalidMapping,
}

fn fingerprint_case(
    id: &ManifestKey,
    case_id: TrialCaseId,
    function: BuiltInGenreFunction,
    work: BlobId,
    ancestry: BlobId,
    source: BlobId,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(CASE_FINGERPRINT_DOMAIN);
    digest.update((id.as_str().len() as u64).to_be_bytes());
    digest.update(id.as_str().as_bytes());
    digest.update(case_id.as_ulid().to_bytes());
    digest.update([function.ordinal()]);
    digest.update(work.as_bytes());
    digest.update(ancestry.as_bytes());
    digest.update(source.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_assignment_id(
    schedule: BlobId,
    contender: BlobId,
    case: &BenchmarkCaseBinding,
    n: u8,
    fresh_run: u8,
    cell: PermutationCell,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(ASSIGNMENT_ID_DOMAIN);
    digest.update(schedule.as_bytes());
    digest.update(contender.as_bytes());
    digest.update(case.fingerprint().as_bytes());
    digest.update([n, fresh_run, cell.index()]);
    BlobId::from_bytes(digest.finalize().into())
}

fn derive_label(seed: u64, assignment_id: BlobId, slot: u8) -> OpaqueCandidateLabel {
    let mut digest = Sha256::new();
    digest.update(LABEL_DOMAIN);
    digest.update(seed.to_be_bytes());
    digest.update(assignment_id.as_bytes());
    digest.update([slot]);
    let bytes: [u8; 32] = digest.finalize().into();
    let mut label = String::from("candidate-");
    for byte in &bytes[..12] {
        use std::fmt::Write;
        write!(&mut label, "{byte:02x}").expect("writing to String cannot fail");
    }
    OpaqueCandidateLabel(label)
}

fn derive_orientation(seed: u64, assignment_id: BlobId) -> bool {
    let mut digest = Sha256::new();
    digest.update(b"loom/benchmark-orientation/v1\0");
    digest.update(seed.to_be_bytes());
    digest.update(assignment_id.as_bytes());
    digest.finalize()[0] & 1 != 0
}

fn derive_permutation_seed(seed: u64, assignment_id: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/benchmark-permutation-seed/v1\0");
    digest.update(seed.to_be_bytes());
    digest.update(assignment_id.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_mapping(schedule: BlobId, entries: &[AssignmentLabelMapEntry]) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(MAPPING_FINGERPRINT_DOMAIN);
    digest.update(schedule.as_bytes());
    digest.update((entries.len() as u64).to_be_bytes());
    for entry in entries {
        entry.update_digest(&mut digest);
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn update_label(label: &OpaqueCandidateLabel, digest: &mut Sha256) {
    digest.update((label.0.len() as u64).to_be_bytes());
    digest.update(label.0.as_bytes());
}

fn update_arm(arm: ProfileArm, digest: &mut Sha256) {
    digest.update([arm.domain_tag()]);
    digest.update(arm.profile_fingerprint().as_bytes());
}
