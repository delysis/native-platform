use std::collections::BTreeSet;

use loom_research_types::{BoundError, ManifestKey, NonEmptyBoundedVec};
use loom_types::BlobId;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_PROFILE_MODELS: usize = 32;
pub const MAX_PROFILE_EVALUATORS: usize = 32;
pub const MAX_PROFILE_CORPORA: usize = 64;

const COMPONENT_FINGERPRINT_DOMAIN: &[u8] = b"loom/harness-profile-components/v2\0";
const CANDIDATE_FINGERPRINT_DOMAIN: &[u8] = b"loom/frozen-candidate-profile/v1\0";
const BASELINE_FINGERPRINT_DOMAIN: &[u8] = b"loom/verified-direct-n1-baseline/v1\0";
const REVIEWED_FINGERPRINT_DOMAIN: &[u8] = b"loom/frontier-reviewed-profile/v2\0";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileModelRole {
    BaseWriter,
    Controller,
    LocalCritic,
    FrontierCritic,
    Embedder,
    RewardModel,
}

impl ProfileModelRole {
    pub(crate) const fn domain_tag(self) -> u8 {
        match self {
            Self::BaseWriter => 0,
            Self::Controller => 1,
            Self::LocalCritic => 2,
            Self::FrontierCritic => 3,
            Self::Embedder => 4,
            Self::RewardModel => 5,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileModelBinding {
    role: ProfileModelRole,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    adapter_stack_fingerprint: BlobId,
}

impl ProfileModelBinding {
    pub const fn new(
        role: ProfileModelRole,
        model_fingerprint: BlobId,
        tokenizer_fingerprint: BlobId,
        adapter_stack_fingerprint: BlobId,
    ) -> Self {
        Self {
            role,
            model_fingerprint,
            tokenizer_fingerprint,
            adapter_stack_fingerprint,
        }
    }

    pub const fn role(&self) -> ProfileModelRole {
        self.role
    }

    pub const fn model_fingerprint(&self) -> BlobId {
        self.model_fingerprint
    }

    pub const fn tokenizer_fingerprint(&self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn adapter_stack_fingerprint(&self) -> BlobId {
        self.adapter_stack_fingerprint
    }

    fn update_digest(&self, digest: &mut Sha256) {
        digest.update([self.role.domain_tag()]);
        digest.update(self.model_fingerprint.as_bytes());
        digest.update(self.tokenizer_fingerprint.as_bytes());
        digest.update(self.adapter_stack_fingerprint.as_bytes());
    }
}

/// Every behaviorally relevant component of one reusable harness candidate.
///
/// This value is a frozen specification, not evidence that any component ran.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HarnessProfileComponents {
    models: NonEmptyBoundedVec<ProfileModelBinding, MAX_PROFILE_MODELS>,
    prompt_fingerprint: BlobId,
    sampler_fingerprint: BlobId,
    control_fingerprint: BlobId,
    ranker_fingerprint: BlobId,
    search_fingerprint: BlobId,
    evaluator_fingerprints: NonEmptyBoundedVec<BlobId, MAX_PROFILE_EVALUATORS>,
    corpus_fingerprints: NonEmptyBoundedVec<BlobId, MAX_PROFILE_CORPORA>,
    selected_n: u8,
    fingerprint: BlobId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessProfileComponentInputs {
    pub models: Vec<ProfileModelBinding>,
    pub prompt_fingerprint: BlobId,
    pub sampler_fingerprint: BlobId,
    pub control_fingerprint: BlobId,
    pub ranker_fingerprint: BlobId,
    pub search_fingerprint: BlobId,
    pub evaluator_fingerprints: Vec<BlobId>,
    pub corpus_fingerprints: Vec<BlobId>,
    pub selected_n: u8,
}

impl HarnessProfileComponents {
    pub fn new(mut inputs: HarnessProfileComponentInputs) -> Result<Self, ProfileError> {
        if !crate::CONFIRMATORY_N_VALUES.contains(&inputs.selected_n) {
            return Err(ProfileError::UnsupportedSelectedN(inputs.selected_n));
        }
        inputs.evaluator_fingerprints.sort_unstable();
        inputs.corpus_fingerprints.sort_unstable();
        let models = NonEmptyBoundedVec::new(inputs.models)?;
        let evaluator_fingerprints = NonEmptyBoundedVec::new(inputs.evaluator_fingerprints)?;
        let corpus_fingerprints = NonEmptyBoundedVec::new(inputs.corpus_fingerprints)?;
        validate_models(&models)?;
        validate_unique_hashes("evaluator", &evaluator_fingerprints)?;
        validate_unique_hashes("corpus", &corpus_fingerprints)?;
        let mut components = Self {
            models,
            prompt_fingerprint: inputs.prompt_fingerprint,
            sampler_fingerprint: inputs.sampler_fingerprint,
            control_fingerprint: inputs.control_fingerprint,
            ranker_fingerprint: inputs.ranker_fingerprint,
            search_fingerprint: inputs.search_fingerprint,
            evaluator_fingerprints,
            corpus_fingerprints,
            selected_n: inputs.selected_n,
            fingerprint: BlobId::digest(&[]),
        };
        components.fingerprint = components.compute_fingerprint();
        Ok(components)
    }

    pub fn models(&self) -> &[ProfileModelBinding] {
        &self.models
    }

    pub const fn prompt_fingerprint(&self) -> BlobId {
        self.prompt_fingerprint
    }

    pub const fn sampler_fingerprint(&self) -> BlobId {
        self.sampler_fingerprint
    }

    pub const fn control_fingerprint(&self) -> BlobId {
        self.control_fingerprint
    }

    pub const fn ranker_fingerprint(&self) -> BlobId {
        self.ranker_fingerprint
    }

    pub const fn search_fingerprint(&self) -> BlobId {
        self.search_fingerprint
    }

    pub fn evaluator_fingerprints(&self) -> &[BlobId] {
        &self.evaluator_fingerprints
    }

    pub fn corpus_fingerprints(&self) -> &[BlobId] {
        &self.corpus_fingerprints
    }

    pub const fn selected_n(&self) -> u8 {
        self.selected_n
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(COMPONENT_FINGERPRINT_DOMAIN);
        digest.update((self.models.len() as u64).to_be_bytes());
        for model in self.models.iter() {
            model.update_digest(&mut digest);
        }
        digest.update(self.prompt_fingerprint.as_bytes());
        digest.update(self.sampler_fingerprint.as_bytes());
        digest.update(self.control_fingerprint.as_bytes());
        digest.update(self.ranker_fingerprint.as_bytes());
        digest.update(self.search_fingerprint.as_bytes());
        update_hashes(&self.evaluator_fingerprints, &mut digest);
        update_hashes(&self.corpus_fingerprints, &mut digest);
        digest.update([self.selected_n]);
        BlobId::from_bytes(digest.finalize().into())
    }
}

/// Frozen, unreviewed harness candidate. This type carries no qualification.
///
/// ```compile_fail
/// use loom_benchmark::FrozenCandidateProfile;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<FrozenCandidateProfile>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrozenCandidateProfile {
    id: ManifestKey,
    components: HarnessProfileComponents,
    fingerprint: BlobId,
}

impl FrozenCandidateProfile {
    pub fn new(id: ManifestKey, components: HarnessProfileComponents) -> Self {
        let fingerprint = fingerprint_candidate(&id, components.fingerprint());
        Self {
            id,
            components,
            fingerprint,
        }
    }

    pub const fn id(&self) -> &ManifestKey {
        &self.id
    }

    pub const fn components(&self) -> &HarnessProfileComponents {
        &self.components
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

/// Move-only evidence that a verifier inspected the compiled prompt treatment
/// and found exact direct continuation with N=1.
///
/// There is deliberately no production constructor until the prompt/trial
/// verifier is integrated. Hash possession is not verification authority.
///
/// ```compile_fail
/// use loom_benchmark::VerifiedDirectContinuationBaselineLease;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<VerifiedDirectContinuationBaselineLease>();
/// ```
#[must_use]
#[derive(Debug)]
pub struct VerifiedDirectContinuationBaselineLease {
    candidate: BlobId,
    treatment: BlobId,
    verifier_receipt: BlobId,
    authority: BaselineLeaseAuthority,
}

#[derive(Debug)]
struct BaselineLeaseAuthority;

impl VerifiedDirectContinuationBaselineLease {
    #[cfg(test)]
    pub(crate) const fn for_test(
        candidate_fingerprint: BlobId,
        treatment_fingerprint: BlobId,
        verifier_receipt_fingerprint: BlobId,
    ) -> Self {
        Self {
            candidate: candidate_fingerprint,
            treatment: treatment_fingerprint,
            verifier_receipt: verifier_receipt_fingerprint,
            authority: BaselineLeaseAuthority,
        }
    }
}

/// Typed direct-continuation N=1 control arm.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrozenDirectContinuationN1Baseline {
    candidate: FrozenCandidateProfile,
    treatment_fingerprint: BlobId,
    verifier_receipt_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl FrozenDirectContinuationN1Baseline {
    pub fn from_verified(
        candidate: FrozenCandidateProfile,
        lease: VerifiedDirectContinuationBaselineLease,
    ) -> Result<Self, ProfileError> {
        let VerifiedDirectContinuationBaselineLease {
            candidate: verified_candidate,
            treatment,
            verifier_receipt,
            authority: _authority,
        } = lease;
        if candidate.fingerprint() != verified_candidate {
            return Err(ProfileError::BaselineLeaseMismatch);
        }
        if candidate.components().selected_n() != 1 {
            return Err(ProfileError::BaselineMustUseN1);
        }
        let fingerprint =
            fingerprint_baseline(candidate.fingerprint(), treatment, verifier_receipt);
        Ok(Self {
            candidate,
            treatment_fingerprint: treatment,
            verifier_receipt_fingerprint: verifier_receipt,
            fingerprint,
        })
    }

    pub const fn candidate(&self) -> &FrozenCandidateProfile {
        &self.candidate
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub const fn treatment_fingerprint(&self) -> BlobId {
        self.treatment_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontierProfileRole {
    EfficientKnee,
    BalancedStudio,
    MaximumQuality,
}

impl FrontierProfileRole {
    pub const ALL: [Self; 3] = [
        Self::EfficientKnee,
        Self::BalancedStudio,
        Self::MaximumQuality,
    ];

    pub(crate) const fn domain_tag(self) -> u8 {
        match self {
            Self::EfficientKnee => 0,
            Self::BalancedStudio => 1,
            Self::MaximumQuality => 2,
        }
    }
}

/// Immutable provisional result derived only from a verified finalist
/// selection. It has no public constructor and cannot be deserialized.
///
/// ```compile_fail
/// use loom_benchmark::FrontierReviewedProvisionalProfile;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<FrontierReviewedProvisionalProfile>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrontierReviewedProvisionalProfile {
    candidate: FrozenCandidateProfile,
    role: FrontierProfileRole,
    seal_fingerprint: BlobId,
    journal_chain_head: BlobId,
    journal_record_fingerprint: BlobId,
    finalist_selection_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl FrontierReviewedProvisionalProfile {
    pub(crate) fn from_selection(
        candidate: FrozenCandidateProfile,
        role: FrontierProfileRole,
        seal_fingerprint: BlobId,
        journal_chain_head: BlobId,
        journal_record_fingerprint: BlobId,
        finalist_selection_fingerprint: BlobId,
    ) -> Self {
        let fingerprint = fingerprint_reviewed(
            candidate.fingerprint(),
            role,
            seal_fingerprint,
            journal_chain_head,
            journal_record_fingerprint,
            finalist_selection_fingerprint,
        );
        Self {
            candidate,
            role,
            seal_fingerprint,
            journal_chain_head,
            journal_record_fingerprint,
            finalist_selection_fingerprint,
            fingerprint,
        }
    }

    pub const fn candidate(&self) -> &FrozenCandidateProfile {
        &self.candidate
    }

    pub const fn role(&self) -> FrontierProfileRole {
        self.role
    }

    pub const fn seal_fingerprint(&self) -> BlobId {
        self.seal_fingerprint
    }

    pub const fn journal_chain_head(&self) -> BlobId {
        self.journal_chain_head
    }

    pub const fn journal_record_fingerprint(&self) -> BlobId {
        self.journal_record_fingerprint
    }

    pub const fn finalist_selection_fingerprint(&self) -> BlobId {
        self.finalist_selection_fingerprint
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProfileError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error("profile repeats an exact model binding")]
    DuplicateModelBinding,
    #[error("profile requires at least one base writer binding")]
    MissingBaseWriter,
    #[error("profile repeats an {0} fingerprint")]
    DuplicateFingerprint(&'static str),
    #[error("profile selected N={0}, outside [1,2,4,8,16,32]")]
    UnsupportedSelectedN(u8),
    #[error("the direct-continuation baseline must select N=1")]
    BaselineMustUseN1,
    #[error("direct-continuation verifier lease belongs to another candidate")]
    BaselineLeaseMismatch,
}

fn validate_models(models: &[ProfileModelBinding]) -> Result<(), ProfileError> {
    if !models
        .iter()
        .any(|model| model.role() == ProfileModelRole::BaseWriter)
    {
        return Err(ProfileError::MissingBaseWriter);
    }
    let mut unique = BTreeSet::new();
    for model in models {
        let key = (
            model.role,
            model.model_fingerprint,
            model.tokenizer_fingerprint,
            model.adapter_stack_fingerprint,
        );
        if !unique.insert(key) {
            return Err(ProfileError::DuplicateModelBinding);
        }
    }
    Ok(())
}

fn validate_unique_hashes(label: &'static str, hashes: &[BlobId]) -> Result<(), ProfileError> {
    let mut unique = BTreeSet::new();
    for hash in hashes {
        if !unique.insert(*hash) {
            return Err(ProfileError::DuplicateFingerprint(label));
        }
    }
    Ok(())
}

fn update_hashes(hashes: &[BlobId], digest: &mut Sha256) {
    digest.update((hashes.len() as u64).to_be_bytes());
    for hash in hashes {
        digest.update(hash.as_bytes());
    }
}

fn fingerprint_candidate(id: &ManifestKey, components: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(CANDIDATE_FINGERPRINT_DOMAIN);
    digest.update((id.as_str().len() as u64).to_be_bytes());
    digest.update(id.as_str().as_bytes());
    digest.update(components.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_baseline(candidate: BlobId, treatment: BlobId, verifier: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(BASELINE_FINGERPRINT_DOMAIN);
    digest.update(candidate.as_bytes());
    digest.update(treatment.as_bytes());
    digest.update(verifier.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_reviewed(
    candidate: BlobId,
    role: FrontierProfileRole,
    seal: BlobId,
    journal: BlobId,
    journal_record: BlobId,
    selection: BlobId,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(REVIEWED_FINGERPRINT_DOMAIN);
    digest.update(candidate.as_bytes());
    digest.update([role.domain_tag()]);
    digest.update(seal.as_bytes());
    digest.update(journal.as_bytes());
    digest.update(journal_record.as_bytes());
    digest.update(selection.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}
