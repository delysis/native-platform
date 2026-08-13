use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::{
    BenchmarkManifest, BoundError, BoundedVec, CompiledManifest, MAX_BENCHMARK_CASES_PER_FUNCTION,
    MAX_BENCHMARK_CONTENDERS, ManifestCompileError, ManifestDocument, ManifestFormat,
    ManifestIntegrityError, ManifestKey,
};
use loom_types::BlobId;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ArmBudget, ArmBudgetAllocation, AssignmentError, AssignmentLabelMap, BenchmarkCaseBinding,
    BlindedBenchmarkAssignment, BuiltInGenreFunction, FRESH_FRONTIER_RUNS, FrozenCandidateProfile,
    FrozenDirectContinuationN1Baseline, MAX_BENCHMARK_ASSIGNMENTS, MIN_CASES_PER_GENRE,
    ORDER_PERMUTATION_CELLS, build_assignment_matrix,
};

pub const MAX_SEALED_CASES: usize =
    BuiltInGenreFunction::ALL.len() * MAX_BENCHMARK_CASES_PER_FUNCTION;
pub const MAX_SEALED_ARMS: usize = MAX_BENCHMARK_CONTENDERS + 1;

const SCHEDULE_FINGERPRINT_DOMAIN: &[u8] = b"loom/benchmark-schedule/v1\0";
const SEAL_FINGERPRINT_DOMAIN: &[u8] = b"loom/benchmark-seal/v1\0";
const SEAL_VERIFICATION_DOMAIN: &[u8] = b"loom/benchmark-seal-verification/v1\0";

pub const REQUIRED_FRONTIER_MODEL: &str = "gpt-5.6-sol";
pub const REQUIRED_FRONTIER_REASONING_EFFORT: &str = "xhigh";

/// Persistable facts emitted by the future campaign/store split verifier.
/// This record is diagnostic without its move-only lease.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BenchmarkSealVerificationRecord {
    seal_fingerprint: BlobId,
    campaign_artifact_receipt_fingerprint: BlobId,
    case_source_receipt_fingerprint: BlobId,
    corpus_exclusion_receipt_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl BenchmarkSealVerificationRecord {
    pub const fn seal_fingerprint(&self) -> BlobId {
        self.seal_fingerprint
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(SEAL_VERIFICATION_DOMAIN);
        digest.update(self.seal_fingerprint.as_bytes());
        digest.update(self.campaign_artifact_receipt_fingerprint.as_bytes());
        digest.update(self.case_source_receipt_fingerprint.as_bytes());
        digest.update(self.corpus_exclusion_receipt_fingerprint.as_bytes());
        BlobId::from_bytes(digest.finalize().into())
    }
}

/// Move-only authority proving campaign identity, exact case sources, and
/// work/ancestry/corpus exclusion. No production mint exists until those
/// verifiers are wired.
#[must_use]
#[derive(Debug)]
pub struct VerifiedBenchmarkSealLease {
    record: BenchmarkSealVerificationRecord,
}

impl VerifiedBenchmarkSealLease {
    pub(crate) fn into_record(
        self,
        seal: &BenchmarkSeal,
    ) -> Result<BenchmarkSealVerificationRecord, BenchmarkSealError> {
        if self.record.seal_fingerprint != seal.fingerprint()
            || self.record.compute_fingerprint() != self.record.fingerprint
        {
            return Err(BenchmarkSealError::SealVerificationLeaseMismatch);
        }
        Ok(self.record)
    }

    #[cfg(test)]
    pub(crate) fn for_test(seal: &BenchmarkSeal) -> Self {
        let mut record = BenchmarkSealVerificationRecord {
            seal_fingerprint: seal.fingerprint(),
            campaign_artifact_receipt_fingerprint: BlobId::digest(b"test-campaign-receipt"),
            case_source_receipt_fingerprint: BlobId::digest(b"test-case-source-receipt"),
            corpus_exclusion_receipt_fingerprint: BlobId::digest(b"test-corpus-exclusion-receipt"),
            fingerprint: BlobId::digest(&[]),
        };
        record.fingerprint = record.compute_fingerprint();
        Self { record }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrontierReviewBinding {
    manifest_model_id: ManifestKey,
    model_fingerprint: BlobId,
    evaluator_protocol_fingerprint: BlobId,
    cli_binary_fingerprint: BlobId,
    cli_version_fingerprint: BlobId,
}

impl FrontierReviewBinding {
    pub fn pinned(
        manifest_model_id: ManifestKey,
        model_fingerprint: BlobId,
        evaluator_protocol_fingerprint: BlobId,
        cli_binary_fingerprint: BlobId,
        cli_version_fingerprint: BlobId,
    ) -> Result<Self, BenchmarkSealError> {
        if manifest_model_id.as_str() != REQUIRED_FRONTIER_MODEL {
            return Err(BenchmarkSealError::ReviewModelNotPinned);
        }
        Ok(Self {
            manifest_model_id,
            model_fingerprint,
            evaluator_protocol_fingerprint,
            cli_binary_fingerprint,
            cli_version_fingerprint,
        })
    }

    pub const fn manifest_model_id(&self) -> &ManifestKey {
        &self.manifest_model_id
    }

    pub const fn model_fingerprint(&self) -> BlobId {
        self.model_fingerprint
    }

    pub const fn evaluator_protocol_fingerprint(&self) -> BlobId {
        self.evaluator_protocol_fingerprint
    }

    pub const fn cli_binary_fingerprint(&self) -> BlobId {
        self.cli_binary_fingerprint
    }

    pub const fn cli_version_fingerprint(&self) -> BlobId {
        self.cli_version_fingerprint
    }

    pub const fn requested_reasoning_effort(&self) -> &'static str {
        REQUIRED_FRONTIER_REASONING_EFFORT
    }

    fn update_digest(&self, digest: &mut Sha256) {
        digest.update((self.manifest_model_id.as_str().len() as u64).to_be_bytes());
        digest.update(self.manifest_model_id.as_str().as_bytes());
        digest.update(self.model_fingerprint.as_bytes());
        digest.update(self.evaluator_protocol_fingerprint.as_bytes());
        digest.update(self.cli_binary_fingerprint.as_bytes());
        digest.update(self.cli_version_fingerprint.as_bytes());
        digest.update((REQUIRED_FRONTIER_REASONING_EFFORT.len() as u64).to_be_bytes());
        digest.update(REQUIRED_FRONTIER_REASONING_EFFORT.as_bytes());
    }
}

#[derive(Clone, Debug)]
pub struct BenchmarkSealInputs {
    pub baseline: FrozenDirectContinuationN1Baseline,
    pub contenders: Vec<FrozenCandidateProfile>,
    pub cases: Vec<BenchmarkCaseBinding>,
    pub arm_budgets: Vec<ArmBudgetAllocation>,
    pub review: FrontierReviewBinding,
}

/// Complete immutable confirmatory schedule. Deserialization is unavailable.
///
/// ```compile_fail
/// use loom_benchmark::BenchmarkSeal;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<BenchmarkSeal>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BenchmarkSeal {
    exact_manifest_source: Vec<u8>,
    manifest_source_hash: BlobId,
    canonical_manifest: Vec<u8>,
    manifest_artifact_hash: BlobId,
    campaign_artifact_hash: BlobId,
    seed: u64,
    baseline: FrozenDirectContinuationN1Baseline,
    contenders: BoundedVec<FrozenCandidateProfile, MAX_BENCHMARK_CONTENDERS>,
    cases: BoundedVec<BenchmarkCaseBinding, MAX_SEALED_CASES>,
    arm_budgets: BoundedVec<ArmBudgetAllocation, MAX_SEALED_ARMS>,
    equal_arm_budget: ArmBudget,
    review: FrontierReviewBinding,
    schedule_fingerprint: BlobId,
    assignments: BoundedVec<BlindedBenchmarkAssignment, MAX_BENCHMARK_ASSIGNMENTS>,
    #[serde(skip)]
    assignment_index: BTreeMap<BlobId, usize>,
    label_mapping_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl BenchmarkSeal {
    pub fn compile(
        exact_manifest_source: &[u8],
        inputs: BenchmarkSealInputs,
    ) -> Result<(Self, AssignmentLabelMap), BenchmarkSealError> {
        let compiled = CompiledManifest::compile(exact_manifest_source)?;
        compiled.verify_integrity()?;
        let ManifestDocument::Benchmark(manifest) = compiled.document() else {
            return Err(BenchmarkSealError::WrongManifestFormat(compiled.format()));
        };
        validate_manifest_protocol(manifest)?;
        validate_profiles(manifest, &inputs)?;
        let cases = validate_cases(manifest, inputs.cases)?;
        let (arm_budgets, equal_arm_budget) =
            validate_budgets(&inputs.baseline, &inputs.contenders, inputs.arm_budgets)?;
        validate_review(
            manifest,
            &inputs.review,
            &inputs.baseline,
            &inputs.contenders,
        )?;

        let contenders = BoundedVec::new(inputs.contenders)?;
        let schedule_fingerprint = fingerprint_schedule(ScheduleFingerprintInputs {
            source_hash: compiled.source_hash().as_blob_id(),
            manifest_hash: compiled.artifact_hash().as_blob_id(),
            campaign_hash: manifest.campaign().artifact_sha256,
            seed: manifest.seed(),
            baseline: &inputs.baseline,
            contenders: &contenders,
            cases: &cases,
            budgets: &arm_budgets,
            review: &inputs.review,
        });
        let contender_fingerprints = contenders
            .iter()
            .map(FrozenCandidateProfile::fingerprint)
            .collect::<Vec<_>>();
        let matrix = build_assignment_matrix(
            schedule_fingerprint,
            manifest.seed(),
            inputs.baseline.fingerprint(),
            &contender_fingerprints,
            &cases,
        )?;
        matrix.mapping.verify_integrity()?;
        let fingerprint = fingerprint_seal(
            schedule_fingerprint,
            matrix.mapping.fingerprint(),
            &matrix.assignments,
        );
        let assignment_index = matrix
            .assignments
            .iter()
            .enumerate()
            .map(|(index, assignment)| (assignment.assignment_id(), index))
            .collect();
        let seal = Self {
            exact_manifest_source: compiled.source_bytes().to_vec(),
            manifest_source_hash: compiled.source_hash().as_blob_id(),
            canonical_manifest: compiled.canonical_bytes().to_vec(),
            manifest_artifact_hash: compiled.artifact_hash().as_blob_id(),
            campaign_artifact_hash: manifest.campaign().artifact_sha256,
            seed: manifest.seed(),
            baseline: inputs.baseline,
            contenders,
            cases,
            arm_budgets,
            equal_arm_budget,
            review: inputs.review,
            schedule_fingerprint,
            assignments: matrix.assignments,
            assignment_index,
            label_mapping_fingerprint: matrix.mapping.fingerprint(),
            fingerprint,
        };
        seal.verify_integrity()?;
        Ok((seal, matrix.mapping))
    }

    pub fn exact_manifest_source(&self) -> &[u8] {
        &self.exact_manifest_source
    }

    pub const fn manifest_source_hash(&self) -> BlobId {
        self.manifest_source_hash
    }

    pub fn canonical_manifest(&self) -> &[u8] {
        &self.canonical_manifest
    }

    pub const fn manifest_artifact_hash(&self) -> BlobId {
        self.manifest_artifact_hash
    }

    pub const fn campaign_artifact_hash(&self) -> BlobId {
        self.campaign_artifact_hash
    }

    pub const fn seed(&self) -> u64 {
        self.seed
    }

    pub const fn baseline(&self) -> &FrozenDirectContinuationN1Baseline {
        &self.baseline
    }

    pub fn contenders(&self) -> &[FrozenCandidateProfile] {
        &self.contenders
    }

    pub fn cases(&self) -> &[BenchmarkCaseBinding] {
        &self.cases
    }

    pub fn arm_budgets(&self) -> &[ArmBudgetAllocation] {
        &self.arm_budgets
    }

    pub const fn equal_arm_budget(&self) -> ArmBudget {
        self.equal_arm_budget
    }

    pub const fn review(&self) -> &FrontierReviewBinding {
        &self.review
    }

    pub const fn schedule_fingerprint(&self) -> BlobId {
        self.schedule_fingerprint
    }

    pub fn assignments(&self) -> &[BlindedBenchmarkAssignment] {
        &self.assignments
    }

    pub fn assignment(&self, id: BlobId) -> Option<&BlindedBenchmarkAssignment> {
        self.assignment_index
            .get(&id)
            .and_then(|index| self.assignments.get(*index))
    }

    pub const fn label_mapping_fingerprint(&self) -> BlobId {
        self.label_mapping_fingerprint
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn contender(&self, fingerprint: BlobId) -> Option<&FrozenCandidateProfile> {
        self.contenders
            .iter()
            .find(|profile| profile.fingerprint() == fingerprint)
    }

    pub fn verify_mapping(&self, mapping: &AssignmentLabelMap) -> Result<(), BenchmarkSealError> {
        mapping.verify_integrity()?;
        if mapping.seal_schedule_fingerprint() != self.schedule_fingerprint
            || mapping.fingerprint() != self.label_mapping_fingerprint
            || mapping.entries().len() != self.assignments.len()
        {
            return Err(BenchmarkSealError::LabelMappingMismatch);
        }
        for (assignment, entry) in self.assignments.iter().zip(mapping.entries()) {
            if assignment.assignment_id() != entry.assignment_id() {
                return Err(BenchmarkSealError::LabelMappingMismatch);
            }
        }
        Ok(())
    }

    pub fn verify_integrity(&self) -> Result<(), BenchmarkSealError> {
        if BlobId::digest(&self.exact_manifest_source) != self.manifest_source_hash {
            return Err(BenchmarkSealError::ManifestSourceHashMismatch);
        }
        let compiled = CompiledManifest::compile(&self.exact_manifest_source)?;
        compiled.verify_integrity()?;
        if compiled.canonical_bytes() != self.canonical_manifest
            || compiled.artifact_hash().as_blob_id() != self.manifest_artifact_hash
        {
            return Err(BenchmarkSealError::ManifestArtifactMismatch);
        }
        let ManifestDocument::Benchmark(manifest) = compiled.document() else {
            return Err(BenchmarkSealError::WrongManifestFormat(compiled.format()));
        };
        validate_manifest_protocol(manifest)?;
        let expected_schedule = fingerprint_schedule(ScheduleFingerprintInputs {
            source_hash: self.manifest_source_hash,
            manifest_hash: self.manifest_artifact_hash,
            campaign_hash: self.campaign_artifact_hash,
            seed: self.seed,
            baseline: &self.baseline,
            contenders: &self.contenders,
            cases: &self.cases,
            budgets: &self.arm_budgets,
            review: &self.review,
        });
        if expected_schedule != self.schedule_fingerprint {
            return Err(BenchmarkSealError::ScheduleFingerprintMismatch);
        }
        let contender_fingerprints = self
            .contenders
            .iter()
            .map(FrozenCandidateProfile::fingerprint)
            .collect::<Vec<_>>();
        let rebuilt = build_assignment_matrix(
            self.schedule_fingerprint,
            self.seed,
            self.baseline.fingerprint(),
            &contender_fingerprints,
            &self.cases,
        )?;
        if rebuilt.assignments != self.assignments
            || rebuilt.mapping.fingerprint() != self.label_mapping_fingerprint
            || self.assignment_index.len() != self.assignments.len()
            || self
                .assignments
                .iter()
                .enumerate()
                .any(|(index, assignment)| {
                    self.assignment_index.get(&assignment.assignment_id()) != Some(&index)
                })
        {
            return Err(BenchmarkSealError::AssignmentMatrixMismatch);
        }
        if fingerprint_seal(
            self.schedule_fingerprint,
            self.label_mapping_fingerprint,
            &self.assignments,
        ) != self.fingerprint
        {
            return Err(BenchmarkSealError::SealFingerprintMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum BenchmarkSealError {
    #[error(transparent)]
    Manifest(#[from] ManifestCompileError),
    #[error(transparent)]
    ManifestIntegrity(#[from] ManifestIntegrityError),
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error(transparent)]
    Assignment(#[from] AssignmentError),
    #[error("expected loom.benchmark.v1, found {0}")]
    WrongManifestFormat(ManifestFormat),
    #[error("nested N must be exactly [1,2,4,8,16,32]")]
    NonCanonicalNestedN,
    #[error("frontier review must use exactly three fresh runs and four permutation cells")]
    NonCanonicalReviewMatrix,
    #[error("benchmark must contain exactly the five built-in genre functions")]
    IncompleteGenreFunctionSet,
    #[error("genre {0:?} has fewer than six work-disjoint cases")]
    TooFewCases(BuiltInGenreFunction),
    #[error("benchmark repeats a manifest, runtime, work, ancestry, or source case identity")]
    CaseSplitCollision,
    #[error("case bindings do not exactly match the manifest function/case matrix")]
    CaseManifestMismatch,
    #[error("benchmark contender profiles do not exactly match manifest IDs/fingerprints")]
    ContenderManifestMismatch,
    #[error("benchmark repeats a profile ID or fingerprint")]
    DuplicateProfile,
    #[error("frontier review binding does not match the manifest or a profile evaluator")]
    ReviewBindingMismatch,
    #[error("frontier review model must be exactly gpt-5.6-sol")]
    ReviewModelNotPinned,
    #[error("arm budgets do not cover every exact profile exactly once")]
    ArmBudgetCoverageMismatch,
    #[error("confirmatory arm budgets are unequal")]
    UnequalArmBudgets,
    #[error("manifest source hash mismatch")]
    ManifestSourceHashMismatch,
    #[error("manifest canonical bytes or artifact hash mismatch")]
    ManifestArtifactMismatch,
    #[error("benchmark schedule fingerprint mismatch")]
    ScheduleFingerprintMismatch,
    #[error("benchmark assignment matrix mismatch")]
    AssignmentMatrixMismatch,
    #[error("benchmark seal fingerprint mismatch")]
    SealFingerprintMismatch,
    #[error("separate assignment label mapping does not match this seal")]
    LabelMappingMismatch,
    #[error("benchmark seal verification lease is invalid or belongs to another seal")]
    SealVerificationLeaseMismatch,
}

fn validate_manifest_protocol(manifest: &BenchmarkManifest) -> Result<(), BenchmarkSealError> {
    let nested = manifest.nested_n().iter().copied().collect::<Vec<_>>();
    if nested != [1_u32, 2, 4, 8, 16, 32] {
        return Err(BenchmarkSealError::NonCanonicalNestedN);
    }
    if manifest.review().fresh_runs != FRESH_FRONTIER_RUNS
        || manifest.review().order_permutation_cells != ORDER_PERMUTATION_CELLS
    {
        return Err(BenchmarkSealError::NonCanonicalReviewMatrix);
    }
    let function_set = manifest
        .functions()
        .iter()
        .filter_map(|function| BuiltInGenreFunction::from_id(function.id.as_str()))
        .collect::<BTreeSet<_>>();
    if function_set != BuiltInGenreFunction::ALL.into_iter().collect()
        || manifest.functions().len() != BuiltInGenreFunction::ALL.len()
    {
        return Err(BenchmarkSealError::IncompleteGenreFunctionSet);
    }
    for function in BuiltInGenreFunction::ALL {
        let case_count = manifest
            .functions()
            .iter()
            .find(|entry| entry.id.as_str() == function.id())
            .map_or(0, |entry| entry.case_ids.len());
        if case_count < MIN_CASES_PER_GENRE {
            return Err(BenchmarkSealError::TooFewCases(function));
        }
    }
    Ok(())
}

fn validate_profiles(
    manifest: &BenchmarkManifest,
    inputs: &BenchmarkSealInputs,
) -> Result<(), BenchmarkSealError> {
    if inputs.contenders.len() != manifest.contenders().len() {
        return Err(BenchmarkSealError::ContenderManifestMismatch);
    }
    let mut ids = BTreeSet::new();
    let mut fingerprints = BTreeSet::new();
    ids.insert(inputs.baseline.candidate().id().as_str());
    fingerprints.insert(inputs.baseline.fingerprint());
    for (declared, profile) in manifest.contenders().iter().zip(&inputs.contenders) {
        if declared.id != *profile.id() || declared.profile_sha256 != profile.fingerprint() {
            return Err(BenchmarkSealError::ContenderManifestMismatch);
        }
        if !ids.insert(profile.id().as_str()) || !fingerprints.insert(profile.fingerprint()) {
            return Err(BenchmarkSealError::DuplicateProfile);
        }
    }
    Ok(())
}

fn validate_cases(
    manifest: &BenchmarkManifest,
    mut cases: Vec<BenchmarkCaseBinding>,
) -> Result<BoundedVec<BenchmarkCaseBinding, MAX_SEALED_CASES>, BenchmarkSealError> {
    let mut manifest_pairs = BTreeSet::new();
    for function in manifest.functions() {
        let Some(kind) = BuiltInGenreFunction::from_id(function.id.as_str()) else {
            return Err(BenchmarkSealError::IncompleteGenreFunctionSet);
        };
        for case_id in function.case_ids.iter() {
            if !manifest_pairs.insert((kind, case_id.as_str().to_owned())) {
                return Err(BenchmarkSealError::CaseSplitCollision);
            }
        }
    }
    let mut binding_pairs = BTreeSet::new();
    let mut runtime_ids = BTreeSet::new();
    let mut works = BTreeSet::new();
    let mut ancestries = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for case in &cases {
        if !binding_pairs.insert((case.function(), case.manifest_id().as_str().to_owned()))
            || !runtime_ids.insert(case.case_id())
            || !works.insert(case.work_fingerprint())
            || !ancestries.insert(case.project_ancestry_fingerprint())
            || !sources.insert(case.source_blob_id())
        {
            return Err(BenchmarkSealError::CaseSplitCollision);
        }
    }
    if binding_pairs != manifest_pairs {
        return Err(BenchmarkSealError::CaseManifestMismatch);
    }
    cases.sort_unstable_by(|left, right| {
        (left.function().ordinal(), left.manifest_id().as_str())
            .cmp(&(right.function().ordinal(), right.manifest_id().as_str()))
    });
    Ok(BoundedVec::new(cases)?)
}

fn validate_budgets(
    baseline: &FrozenDirectContinuationN1Baseline,
    contenders: &[FrozenCandidateProfile],
    allocations: Vec<ArmBudgetAllocation>,
) -> Result<(BoundedVec<ArmBudgetAllocation, MAX_SEALED_ARMS>, ArmBudget), BenchmarkSealError> {
    let expected = std::iter::once(baseline.fingerprint())
        .chain(contenders.iter().map(FrozenCandidateProfile::fingerprint))
        .collect::<BTreeSet<_>>();
    let actual = allocations
        .iter()
        .map(|allocation| allocation.profile_fingerprint())
        .collect::<BTreeSet<_>>();
    if expected != actual || allocations.len() != expected.len() {
        return Err(BenchmarkSealError::ArmBudgetCoverageMismatch);
    }
    let Some(first) = allocations.first().map(|allocation| allocation.budget()) else {
        return Err(BenchmarkSealError::ArmBudgetCoverageMismatch);
    };
    if allocations
        .iter()
        .any(|allocation| allocation.budget() != first)
    {
        return Err(BenchmarkSealError::UnequalArmBudgets);
    }
    let mut allocations = allocations;
    allocations.sort_unstable_by_key(|allocation| allocation.profile_fingerprint());
    Ok((BoundedVec::new(allocations)?, first))
}

fn validate_review(
    manifest: &BenchmarkManifest,
    review: &FrontierReviewBinding,
    baseline: &FrozenDirectContinuationN1Baseline,
    contenders: &[FrozenCandidateProfile],
) -> Result<(), BenchmarkSealError> {
    if manifest.review().frontier_model != review.manifest_model_id
        || review.manifest_model_id.as_str() != REQUIRED_FRONTIER_MODEL
    {
        return Err(BenchmarkSealError::ReviewBindingMismatch);
    }
    if std::iter::once(baseline.candidate())
        .chain(contenders)
        .any(|profile| {
            !profile
                .components()
                .evaluator_fingerprints()
                .contains(&review.model_fingerprint)
        })
    {
        return Err(BenchmarkSealError::ReviewBindingMismatch);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ScheduleFingerprintInputs<'a> {
    source_hash: BlobId,
    manifest_hash: BlobId,
    campaign_hash: BlobId,
    seed: u64,
    baseline: &'a FrozenDirectContinuationN1Baseline,
    contenders: &'a [FrozenCandidateProfile],
    cases: &'a [BenchmarkCaseBinding],
    budgets: &'a [ArmBudgetAllocation],
    review: &'a FrontierReviewBinding,
}

fn fingerprint_schedule(inputs: ScheduleFingerprintInputs<'_>) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(SCHEDULE_FINGERPRINT_DOMAIN);
    digest.update(inputs.source_hash.as_bytes());
    digest.update(inputs.manifest_hash.as_bytes());
    digest.update(inputs.campaign_hash.as_bytes());
    digest.update(inputs.seed.to_be_bytes());
    digest.update(inputs.baseline.fingerprint().as_bytes());
    digest.update((inputs.contenders.len() as u64).to_be_bytes());
    for contender in inputs.contenders {
        digest.update(contender.fingerprint().as_bytes());
    }
    digest.update((inputs.cases.len() as u64).to_be_bytes());
    for case in inputs.cases {
        digest.update(case.fingerprint().as_bytes());
    }
    digest.update((inputs.budgets.len() as u64).to_be_bytes());
    for allocation in inputs.budgets {
        digest.update(allocation.profile_fingerprint().as_bytes());
        allocation.budget().update_digest(&mut digest);
    }
    inputs.review.update_digest(&mut digest);
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_seal(
    schedule: BlobId,
    mapping: BlobId,
    assignments: &[BlindedBenchmarkAssignment],
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(SEAL_FINGERPRINT_DOMAIN);
    digest.update(schedule.as_bytes());
    digest.update(mapping.as_bytes());
    digest.update((assignments.len() as u64).to_be_bytes());
    for assignment in assignments {
        digest.update(assignment.assignment_id().as_bytes());
        digest.update(assignment.permutation_seed().as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}
