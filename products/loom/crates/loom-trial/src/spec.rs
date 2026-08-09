use std::collections::{BTreeMap, BTreeSet};

use loom_inference::BaseWriterBinding;
use loom_research_types::{
    CampaignCaseSpec, CampaignId, CompiledBaseCompletionPrompt, CompiledManifest,
    CompletionPromptBlockRole, CompletionPromptTail, FrozenCompletionPromptBlock, FrozenTrialStage,
    ManifestDocument, ManifestKey, PromptTopology, SamplerTreatment, StageGraph, StageGraphId,
    StageId, TreatmentSpec, TrialCaseId,
};
use loom_types::{BlobId, ProjectId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{BudgetAmount, TrialBudgetLimits};

const CASE_FINGERPRINT_DOMAIN: &[u8] = b"loom/frozen-campaign-case/v1\0";
const TREATMENT_FINGERPRINT_DOMAIN: &[u8] = b"loom/frozen-treatment/v1\0";
const STAGE_GRAPH_FINGERPRINT_DOMAIN: &[u8] = b"loom/frozen-stage-graph/v1\0";
const TRIAL_FINGERPRINT_DOMAIN: &[u8] = b"loom/frozen-trial/v1\0";
const PROMPT_TOPOLOGY_PROOF_DOMAIN: &[u8] = b"loom/verified-prompt-topology/v1\0";
const PROMPT_TOPOLOGY_SHAPE_DOMAIN: &[u8] = b"loom/prompt-topology-shape/v1\0";
const FROZEN_TRIAL_RECORD_FORMAT: &str = "loom.frozen-trial-spec.v1";
const FROZEN_STAGE_RECORD_FORMAT: &str = "loom.frozen-stage-spec.v1";

#[derive(Serialize)]
struct FrozenTrialCanonicalRecord<'a> {
    format: &'static str,
    spec: &'a FrozenTrialSpec,
}

#[derive(Serialize)]
struct FrozenStageCanonicalRecord<'a> {
    format: &'static str,
    stage: &'a loom_research_types::FrozenStageSpec,
}

pub fn canonical_stage_record_bytes(
    stage: &loom_research_types::FrozenStageSpec,
) -> Result<Vec<u8>, TrialSpecError> {
    serde_json::to_vec(&FrozenStageCanonicalRecord {
        format: FROZEN_STAGE_RECORD_FORMAT,
        stage,
    })
    .map_err(|_| TrialSpecError::CanonicalRecordSerialization)
}

pub fn canonical_stage_record_fingerprint(
    stage: &loom_research_types::FrozenStageSpec,
) -> Result<BlobId, TrialSpecError> {
    canonical_stage_record_bytes(stage).map(|bytes| BlobId::digest(&bytes))
}

/// Live acceptance of one exact graph-to-raw demonstration set.
///
/// No production constructor exists until the inference/evaluation adapter can
/// bind every writer realization and evaluator receipt. A structurally valid
/// or caller-constructed backtranslation receipt cannot create this lease.
///
/// ```compile_fail
/// use loom_trial::AcceptedBacktranslationDemonstrationLease;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<AcceptedBacktranslationDemonstrationLease>();
/// ```
///
/// ```compile_fail
/// use loom_research_types::BacktranslationAuditionReceipt;
/// use loom_trial::AcceptedBacktranslationDemonstrationLease;
/// fn launder(
///     receipt: BacktranslationAuditionReceipt,
/// ) -> AcceptedBacktranslationDemonstrationLease {
///     receipt.into()
/// }
/// ```
#[must_use]
#[derive(Debug)]
pub struct AcceptedBacktranslationDemonstrationLease {
    treatment: BlobId,
    prompt_content: BlobId,
    accepted_demonstration: BlobId,
    writer_realization_set: BlobId,
    evaluator_receipt_set: BlobId,
}

impl AcceptedBacktranslationDemonstrationLease {
    fn into_parts(self) -> (BlobId, BlobId, BlobId, BlobId, BlobId) {
        (
            self.treatment,
            self.prompt_content,
            self.accepted_demonstration,
            self.writer_realization_set,
            self.evaluator_receipt_set,
        )
    }

    #[cfg(test)]
    pub(crate) fn diagnostic_for_tests(
        treatment_fingerprint: BlobId,
        prompt_content_fingerprint: BlobId,
    ) -> Self {
        Self {
            treatment: treatment_fingerprint,
            prompt_content: prompt_content_fingerprint,
            accepted_demonstration: BlobId::digest(
                b"test-only accepted backtranslation demonstration",
            ),
            writer_realization_set: BlobId::digest(b"test-only verified writer realization set"),
            evaluator_receipt_set: BlobId::digest(b"test-only verified evaluator receipt set"),
        }
    }
}

/// Deterministic issuer for prompt-topology authority.
///
/// Six topologies are decidable from the exact compiled prompt. Graph-to-raw
/// paired apprenticeship additionally requires an affine, adapter-issued
/// [`AcceptedBacktranslationDemonstrationLease`].
#[derive(Clone, Copy, Debug, Default)]
pub struct PromptTopologyVerifier;

impl PromptTopologyVerifier {
    pub fn verify(
        treatment: &TreatmentSpec,
        prompt: &CompiledBaseCompletionPrompt,
    ) -> Result<VerifiedPromptTopologyLease, PromptTopologyVerificationError> {
        if treatment.prompt_topology == PromptTopology::GraphRawPairedApprenticeship {
            return Err(PromptTopologyVerificationError::AcceptedBacktranslationRequired);
        }
        verify_treatment_prompt_binding(treatment, prompt)?;
        verify_topology_shape(treatment.prompt_topology, prompt)?;
        Ok(issue_topology_lease(
            treatment,
            prompt,
            fingerprint_topology_shape(treatment.prompt_topology, prompt),
            None,
            None,
        ))
    }

    pub fn verify_graph_raw(
        treatment: &TreatmentSpec,
        prompt: &CompiledBaseCompletionPrompt,
        accepted: AcceptedBacktranslationDemonstrationLease,
    ) -> Result<VerifiedPromptTopologyLease, PromptTopologyVerificationError> {
        if treatment.prompt_topology != PromptTopology::GraphRawPairedApprenticeship {
            return Err(PromptTopologyVerificationError::WrongGraphTopology);
        }
        verify_treatment_prompt_binding(treatment, prompt)?;
        verify_topology_shape(treatment.prompt_topology, prompt)?;
        let (
            accepted_treatment,
            accepted_prompt,
            accepted_demonstration,
            writer_realizations,
            evaluator_receipts,
        ) = accepted.into_parts();
        let treatment_fingerprint = fingerprint_treatment(treatment);
        if accepted_treatment != treatment_fingerprint
            || accepted_prompt != prompt.content_fingerprint()
        {
            return Err(PromptTopologyVerificationError::AcceptedBacktranslationMismatch);
        }
        let mut digest = Sha256::new();
        digest.update(fingerprint_topology_shape(treatment.prompt_topology, prompt).as_bytes());
        digest.update(accepted_demonstration.as_bytes());
        Ok(issue_topology_lease(
            treatment,
            prompt,
            BlobId::from_bytes(digest.finalize().into()),
            Some(writer_realizations),
            Some(evaluator_receipts),
        ))
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PromptTopologyVerificationError {
    #[error("compiled prompt treatment fingerprint does not match the treatment")]
    TreatmentMismatch,
    #[error("prompt block role/order/witness shape does not match {0:?}")]
    ShapeMismatch(PromptTopology),
    #[error("prompt tail origin does not match {0:?}")]
    TailOriginMismatch(PromptTopology),
    #[error("graph-to-raw topology requires accepted live backtranslation evidence")]
    AcceptedBacktranslationRequired,
    #[error("graph-to-raw verifier was called for a different topology")]
    WrongGraphTopology,
    #[error("accepted backtranslation evidence does not match treatment or prompt")]
    AcceptedBacktranslationMismatch,
}

/// Move-only proof from the compiler responsible for one exact prompt
/// topology.
///
/// The only production issuer is [`PromptTopologyVerifier`]. For graph-to-raw
/// paired apprenticeship it additionally consumes exact writer-realization and
/// evaluator authority through [`AcceptedBacktranslationDemonstrationLease`].
///
/// ```compile_fail
/// use loom_trial::VerifiedPromptTopologyLease;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<VerifiedPromptTopologyLease>();
/// ```
///
/// ```compile_fail
/// use loom_trial::VerifiedPromptTopologyLease;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<VerifiedPromptTopologyLease>();
/// ```
///
/// ```compile_fail
/// use loom_research_types::BacktranslationAuditionReceipt;
/// use loom_trial::VerifiedPromptTopologyLease;
/// fn launder(receipt: BacktranslationAuditionReceipt) -> VerifiedPromptTopologyLease {
///     receipt.into()
/// }
/// ```
#[must_use]
#[derive(Debug)]
pub struct VerifiedPromptTopologyLease {
    treatment_fingerprint: BlobId,
    topology: PromptTopology,
    prompt_content_fingerprint: BlobId,
    compiler_evidence_fingerprint: BlobId,
    writer_realization_set_fingerprint: Option<BlobId>,
    evaluator_receipt_set_fingerprint: Option<BlobId>,
    fingerprint: BlobId,
}

impl VerifiedPromptTopologyLease {
    fn verify(
        &self,
        treatment_fingerprint: BlobId,
        topology: PromptTopology,
        prompt_content_fingerprint: BlobId,
    ) -> Result<(), TrialSpecError> {
        if self.treatment_fingerprint != treatment_fingerprint
            || self.topology != topology
            || self.prompt_content_fingerprint != prompt_content_fingerprint
            || self.fingerprint != self.compute_fingerprint()
        {
            return Err(TrialSpecError::PromptTopologyProofMismatch);
        }
        let graph_raw = topology == PromptTopology::GraphRawPairedApprenticeship;
        if graph_raw
            != (self.writer_realization_set_fingerprint.is_some()
                && self.evaluator_receipt_set_fingerprint.is_some())
        {
            return Err(TrialSpecError::PromptTopologyProofMismatch);
        }
        Ok(())
    }

    fn into_fingerprint(self) -> BlobId {
        self.fingerprint
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(PROMPT_TOPOLOGY_PROOF_DOMAIN);
        digest.update(self.treatment_fingerprint.as_bytes());
        digest.update([prompt_topology_tag(self.topology)]);
        digest.update(self.prompt_content_fingerprint.as_bytes());
        digest.update(self.compiler_evidence_fingerprint.as_bytes());
        update_optional_blob(&mut digest, self.writer_realization_set_fingerprint);
        update_optional_blob(&mut digest, self.evaluator_receipt_set_fingerprint);
        BlobId::from_bytes(digest.finalize().into())
    }
}

/// Trusted artifacts required to compile one immutable trial specification.
#[derive(Debug)]
pub struct FrozenTrialInputs<'a> {
    pub project_id: ProjectId,
    pub project_input_fingerprint: BlobId,
    pub campaign_id: CampaignId,
    pub case_id: TrialCaseId,
    pub campaign_manifest: &'a CompiledManifest,
    pub campaign_case_key: &'a str,
    pub treatment_key: &'a str,
    pub stage_graph: &'a StageGraph,
    pub compiled_prompt: &'a CompiledBaseCompletionPrompt,
    pub prompt_topology_lease: VerifiedPromptTopologyLease,
    pub model_binding: &'a BaseWriterBinding,
    pub stage_budget_maxima: Vec<FrozenStageBudgetMaximum>,
    pub budget: TrialBudgetLimits,
}

/// Exact resource ceiling for one frozen stage attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct FrozenStageBudgetMaximum {
    stage_id: StageId,
    maximum: BudgetAmount,
}

impl FrozenStageBudgetMaximum {
    pub const fn new(stage_id: StageId, maximum: BudgetAmount) -> Self {
        Self { stage_id, maximum }
    }

    pub const fn stage_id(self) -> StageId {
        self.stage_id
    }

    pub const fn maximum(self) -> BudgetAmount {
        self.maximum
    }
}

/// A content-bound plan for one case and one treatment.
///
/// Only fingerprints and bounded identifiers cross into the execution layer;
/// exact prompt/manuscript bytes remain with the prompt and store owners. This
/// type is serializable for archival inspection but deliberately cannot be
/// deserialized into authority. Rebuild it from its trusted inputs instead.
///
/// ```compile_fail
/// use loom_trial::FrozenTrialSpec;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<FrozenTrialSpec>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrozenTrialSpec {
    project_id: ProjectId,
    project_input_fingerprint: BlobId,
    campaign_id: CampaignId,
    case_id: TrialCaseId,
    campaign_manifest_source_fingerprint: BlobId,
    campaign_manifest_fingerprint: BlobId,
    campaign_case_key: ManifestKey,
    campaign_case_fingerprint: BlobId,
    treatment_key: ManifestKey,
    treatment_fingerprint: BlobId,
    prompt_topology: PromptTopology,
    prompt_topology_evidence_fingerprint: BlobId,
    stage_graph_id: StageGraphId,
    stage_graph_fingerprint: BlobId,
    generate_stage_id: StageId,
    prompt_content_fingerprint: BlobId,
    exact_prompt_blob_id: BlobId,
    exact_prompt_byte_len: u64,
    model_binding_manifest_source_fingerprint: BlobId,
    model_binding_manifest_fingerprint: BlobId,
    model_binding_fingerprint: BlobId,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    expected_writer_call_count: u16,
    declared_writer_token_maximum: u64,
    stage_budget_maxima: Vec<FrozenStageBudgetMaximum>,
    budget: TrialBudgetLimits,
    fingerprint: BlobId,
}

impl FrozenTrialSpec {
    pub fn compile(inputs: FrozenTrialInputs<'_>) -> Result<Self, TrialSpecError> {
        let resolved = validate_and_resolve(&inputs)?;
        let prompt = inputs.compiled_prompt;
        let exact_prompt_byte_len = u64::try_from(prompt.exact_bytes().len())
            .map_err(|_| TrialSpecError::PromptLengthOverflow)?;

        let mut spec = Self {
            project_id: inputs.project_id,
            project_input_fingerprint: inputs.project_input_fingerprint,
            campaign_id: inputs.campaign_id,
            case_id: inputs.case_id,
            campaign_manifest_source_fingerprint: inputs
                .campaign_manifest
                .source_hash()
                .as_blob_id(),
            campaign_manifest_fingerprint: inputs.campaign_manifest.artifact_hash().as_blob_id(),
            campaign_case_key: resolved.case.id.clone(),
            campaign_case_fingerprint: fingerprint_campaign_case(resolved.case),
            treatment_key: resolved.treatment.id.clone(),
            treatment_fingerprint: resolved.treatment_fingerprint,
            prompt_topology: resolved.treatment.prompt_topology,
            prompt_topology_evidence_fingerprint: inputs.prompt_topology_lease.into_fingerprint(),
            stage_graph_id: inputs.stage_graph.id(),
            stage_graph_fingerprint: fingerprint_stage_graph(inputs.stage_graph),
            generate_stage_id: resolved.generate_stage_id,
            prompt_content_fingerprint: prompt.content_fingerprint(),
            exact_prompt_blob_id: BlobId::digest(prompt.exact_bytes()),
            exact_prompt_byte_len,
            model_binding_manifest_source_fingerprint: inputs
                .model_binding
                .manifest_source_hash()
                .as_blob_id(),
            model_binding_manifest_fingerprint: inputs
                .model_binding
                .manifest_fingerprint()
                .as_blob_id(),
            model_binding_fingerprint: inputs.model_binding.fingerprint(),
            model_fingerprint: inputs.model_binding.model_sha256(),
            tokenizer_fingerprint: inputs.model_binding.tokenizer_sha256(),
            expected_writer_call_count: resolved.treatment.samples_per_case,
            declared_writer_token_maximum: resolved.declared_writer_token_maximum,
            stage_budget_maxima: resolved.stage_budget_maxima.clone(),
            budget: inputs.budget,
            fingerprint: BlobId::digest(b"uninitialized frozen trial"),
        };
        spec.fingerprint = fingerprint_trial(&spec);
        Ok(spec)
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub const fn project_input_fingerprint(&self) -> BlobId {
        self.project_input_fingerprint
    }

    pub const fn campaign_id(&self) -> CampaignId {
        self.campaign_id
    }

    pub const fn case_id(&self) -> TrialCaseId {
        self.case_id
    }

    pub const fn campaign_manifest_source_fingerprint(&self) -> BlobId {
        self.campaign_manifest_source_fingerprint
    }

    pub const fn campaign_manifest_fingerprint(&self) -> BlobId {
        self.campaign_manifest_fingerprint
    }

    pub fn campaign_case_key(&self) -> &str {
        self.campaign_case_key.as_str()
    }

    pub const fn campaign_case_fingerprint(&self) -> BlobId {
        self.campaign_case_fingerprint
    }

    pub fn treatment_key(&self) -> &str {
        self.treatment_key.as_str()
    }

    pub const fn treatment_fingerprint(&self) -> BlobId {
        self.treatment_fingerprint
    }

    pub const fn prompt_topology(&self) -> PromptTopology {
        self.prompt_topology
    }

    pub const fn prompt_topology_evidence_fingerprint(&self) -> BlobId {
        self.prompt_topology_evidence_fingerprint
    }

    pub const fn stage_graph_id(&self) -> StageGraphId {
        self.stage_graph_id
    }

    pub const fn stage_graph_fingerprint(&self) -> BlobId {
        self.stage_graph_fingerprint
    }

    pub const fn generate_stage_id(&self) -> StageId {
        self.generate_stage_id
    }

    pub const fn prompt_content_fingerprint(&self) -> BlobId {
        self.prompt_content_fingerprint
    }

    pub const fn exact_prompt_blob_id(&self) -> BlobId {
        self.exact_prompt_blob_id
    }

    pub const fn exact_prompt_byte_len(&self) -> u64 {
        self.exact_prompt_byte_len
    }

    pub const fn model_binding_manifest_source_fingerprint(&self) -> BlobId {
        self.model_binding_manifest_source_fingerprint
    }

    pub const fn model_binding_manifest_fingerprint(&self) -> BlobId {
        self.model_binding_manifest_fingerprint
    }

    pub const fn model_binding_fingerprint(&self) -> BlobId {
        self.model_binding_fingerprint
    }

    pub const fn model_fingerprint(&self) -> BlobId {
        self.model_fingerprint
    }

    pub const fn tokenizer_fingerprint(&self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn expected_writer_call_count(&self) -> u16 {
        self.expected_writer_call_count
    }

    pub const fn declared_writer_token_maximum(&self) -> u64 {
        self.declared_writer_token_maximum
    }

    pub fn stage_budget_maxima(&self) -> &[FrozenStageBudgetMaximum] {
        &self.stage_budget_maxima
    }

    pub fn stage_budget_maximum(&self, stage_id: StageId) -> Option<BudgetAmount> {
        self.stage_budget_maxima
            .iter()
            .find(|entry| entry.stage_id == stage_id)
            .map(|entry| entry.maximum)
    }

    pub const fn budget(&self) -> TrialBudgetLimits {
        self.budget
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    /// Exact deterministic JSON persisted as this frozen trial's canonical
    /// record. The live store lease binds the digest of these bytes separately
    /// from the trial's domain-separated semantic fingerprint.
    pub fn canonical_record_bytes(&self) -> Result<Vec<u8>, TrialSpecError> {
        serde_json::to_vec(&FrozenTrialCanonicalRecord {
            format: FROZEN_TRIAL_RECORD_FORMAT,
            spec: self,
        })
        .map_err(|_| TrialSpecError::CanonicalRecordSerialization)
    }

    pub fn canonical_record_fingerprint(&self) -> Result<BlobId, TrialSpecError> {
        self.canonical_record_bytes()
            .map(|bytes| BlobId::digest(&bytes))
    }

    pub(crate) fn verify_integrity(&self) -> Result<(), TrialSpecError> {
        self.budget.verify_fingerprint()?;
        if fingerprint_trial(self) != self.fingerprint {
            return Err(TrialSpecError::TrialFingerprintMismatch);
        }
        Ok(())
    }
}

fn verify_treatment_prompt_binding(
    treatment: &TreatmentSpec,
    prompt: &CompiledBaseCompletionPrompt,
) -> Result<(), PromptTopologyVerificationError> {
    if prompt.treatment_recipe_fingerprint() != fingerprint_treatment(treatment) {
        return Err(PromptTopologyVerificationError::TreatmentMismatch);
    }
    Ok(())
}

fn issue_topology_lease(
    treatment: &TreatmentSpec,
    prompt: &CompiledBaseCompletionPrompt,
    compiler_evidence_fingerprint: BlobId,
    writer_realization_set_fingerprint: Option<BlobId>,
    evaluator_receipt_set_fingerprint: Option<BlobId>,
) -> VerifiedPromptTopologyLease {
    let mut lease = VerifiedPromptTopologyLease {
        treatment_fingerprint: fingerprint_treatment(treatment),
        topology: treatment.prompt_topology,
        prompt_content_fingerprint: prompt.content_fingerprint(),
        compiler_evidence_fingerprint,
        writer_realization_set_fingerprint,
        evaluator_receipt_set_fingerprint,
        fingerprint: BlobId::digest(b"uninitialized verified prompt topology"),
    };
    lease.fingerprint = lease.compute_fingerprint();
    lease
}

fn verify_topology_shape(
    topology: PromptTopology,
    prompt: &CompiledBaseCompletionPrompt,
) -> Result<(), PromptTopologyVerificationError> {
    let blocks = prompt.specification().preceding_blocks();
    let shape_matches = match topology {
        PromptTopology::ExactDirectContinuation => blocks.is_empty(),
        PromptTopology::NaturalBookfrontContinuation => {
            !blocks.is_empty()
                && blocks.iter().all(|block| {
                    block.role() == CompletionPromptBlockRole::Bookfront && witness_is_exact(block)
                })
        }
        PromptTopology::EventLedgerOperatorPair => {
            !blocks.is_empty()
                && blocks.len().is_multiple_of(2)
                && blocks.chunks_exact(2).all(|pair| {
                    pair[0].role() == CompletionPromptBlockRole::OperatorDemonstration
                        && witness_is_transformation(&pair[0])
                        && pair[1].role() == CompletionPromptBlockRole::OperatorDemonstration
                        && witness_is_exact(&pair[1])
                })
        }
        PromptTopology::NearestProjectAnchor => {
            blocks.len() == 1 && blocks[0].role() == CompletionPromptBlockRole::ProjectAnchor
        }
        PromptTopology::RawSceneApprenticeship => {
            !blocks.is_empty()
                && blocks.iter().all(|block| {
                    block.role() == CompletionPromptBlockRole::SourceApprenticeship
                        && witness_is_exact(block)
                })
        }
        PromptTopology::GraphRawPairedApprenticeship => {
            !blocks.is_empty()
                && blocks.len().is_multiple_of(2)
                && blocks.chunks_exact(2).all(|pair| {
                    pair[0].role() == CompletionPromptBlockRole::StoryState
                        && witness_is_transformation(&pair[0])
                        && pair[1].role() == CompletionPromptBlockRole::SourceApprenticeship
                        && witness_is_exact(&pair[1])
                })
        }
        PromptTopology::StagedMovementAssembly => {
            blocks.len() == 2
                && blocks[0].role() == CompletionPromptBlockRole::StoryState
                && witness_is_transformation(&blocks[0])
                && blocks[1].role() == CompletionPromptBlockRole::MovementContract
                && witness_is_transformation(&blocks[1])
        }
    };
    if !shape_matches {
        return Err(PromptTopologyVerificationError::ShapeMismatch(topology));
    }

    let requires_live_tail = topology != PromptTopology::StagedMovementAssembly;
    if requires_live_tail
        && !matches!(
            prompt.specification().tail(),
            CompletionPromptTail::LiveManuscript { .. }
        )
    {
        return Err(PromptTopologyVerificationError::TailOriginMismatch(
            topology,
        ));
    }
    Ok(())
}

fn witness_is_exact(block: &FrozenCompletionPromptBlock) -> bool {
    !block.witness().is_transformation()
}

fn witness_is_transformation(block: &FrozenCompletionPromptBlock) -> bool {
    block.witness().is_transformation()
}

fn fingerprint_topology_shape(
    topology: PromptTopology,
    prompt: &CompiledBaseCompletionPrompt,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(PROMPT_TOPOLOGY_SHAPE_DOMAIN);
    digest.update([prompt_topology_tag(topology)]);
    digest.update([match prompt.specification().tail() {
        CompletionPromptTail::LiveManuscript { .. } => 0,
        CompletionPromptTail::AdmittedAssembly { .. } => 1,
    }]);
    let blocks = prompt.specification().preceding_blocks();
    digest.update((blocks.len() as u64).to_be_bytes());
    for block in blocks {
        digest.update([prompt_block_role_tag(block.role())]);
        digest.update(BlobId::digest(block.bytes().as_bytes()).as_bytes());
        digest.update([u8::from(block.witness().is_transformation())]);
        digest.update((block.witness().sources().len() as u64).to_be_bytes());
        for source in block.witness().sources() {
            digest.update(source.revision_id().as_ulid().to_bytes());
            digest.update(source.source_blob_id().as_bytes());
            digest.update(source.range().start().to_be_bytes());
            digest.update(source.range().end().to_be_bytes());
        }
        update_optional_blob(&mut digest, block.witness().recipe_fingerprint());
        update_optional_blob(&mut digest, block.witness().receipt_fingerprint());
        update_optional_blob(&mut digest, block.witness().rendered_bytes_fingerprint());
    }
    BlobId::from_bytes(digest.finalize().into())
}

const fn prompt_block_role_tag(role: CompletionPromptBlockRole) -> u8 {
    match role {
        CompletionPromptBlockRole::Bookfront => 0,
        CompletionPromptBlockRole::OperatorDemonstration => 1,
        CompletionPromptBlockRole::ProjectAnchor => 2,
        CompletionPromptBlockRole::StoryState => 3,
        CompletionPromptBlockRole::MovementContract => 4,
        CompletionPromptBlockRole::SourceApprenticeship => 5,
    }
}

struct ResolvedTrialInputs<'a> {
    case: &'a CampaignCaseSpec,
    treatment: &'a TreatmentSpec,
    treatment_fingerprint: BlobId,
    generate_stage_id: StageId,
    declared_writer_token_maximum: u64,
    stage_budget_maxima: Vec<FrozenStageBudgetMaximum>,
}

fn validate_and_resolve<'a>(
    inputs: &FrozenTrialInputs<'a>,
) -> Result<ResolvedTrialInputs<'a>, TrialSpecError> {
    inputs.budget.verify_fingerprint()?;
    inputs
        .campaign_manifest
        .verify_integrity()
        .map_err(|_| TrialSpecError::CampaignManifestIntegrity)?;
    let ManifestDocument::Campaign(campaign) = inputs.campaign_manifest.document() else {
        return Err(TrialSpecError::WrongCampaignManifestFormat);
    };

    let case = campaign
        .cases()
        .iter()
        .find(|case| case.id.as_str() == inputs.campaign_case_key)
        .ok_or(TrialSpecError::CampaignCaseNotFound)?;
    let treatment = campaign
        .treatments()
        .iter()
        .find(|treatment| treatment.id.as_str() == inputs.treatment_key)
        .ok_or(TrialSpecError::TreatmentNotFound)?;

    verify_binding_integrity(inputs.model_binding)?;
    if campaign.model_bindings().artifact_sha256
        != inputs.model_binding.manifest_fingerprint().as_blob_id()
    {
        return Err(TrialSpecError::CampaignModelBindingsMismatch);
    }
    if case.max_context_tokens > inputs.model_binding.context_tokens() {
        return Err(TrialSpecError::InsufficientModelContext {
            required: case.max_context_tokens,
            available: inputs.model_binding.context_tokens(),
        });
    }

    let treatment_fingerprint = fingerprint_treatment(treatment);
    let prompt = inputs.compiled_prompt;
    prompt
        .specification()
        .verify_compiled_evidence(
            prompt.exact_bytes(),
            prompt.tail_prompt_range(),
            prompt.fingerprint(),
        )
        .map_err(|_| TrialSpecError::CompiledPromptIntegrity)?;
    if prompt.project_id() != inputs.project_id {
        return Err(TrialSpecError::PromptProjectMismatch);
    }
    let scope = prompt.scope();
    if scope.campaign_id() != inputs.campaign_id {
        return Err(TrialSpecError::PromptCampaignMismatch);
    }
    if scope.case_id() != inputs.case_id {
        return Err(TrialSpecError::PromptCaseMismatch);
    }
    let generate_stage = inputs
        .stage_graph
        .stages()
        .iter()
        .find(|stage| stage.stage() == FrozenTrialStage::Generate)
        .expect("validated stage graphs contain Generate");
    if scope.stage_id() != generate_stage.id() {
        return Err(TrialSpecError::PromptStageMismatch);
    }
    if prompt.treatment_recipe_fingerprint() != treatment_fingerprint {
        return Err(TrialSpecError::PromptTreatmentMismatch);
    }
    inputs.prompt_topology_lease.verify(
        treatment_fingerprint,
        treatment.prompt_topology,
        prompt.content_fingerprint(),
    )?;

    let declared_writer_token_maximum = u64::from(treatment.samples_per_case)
        .checked_mul(u64::from(treatment.max_output_tokens))
        .ok_or(TrialSpecError::WriterDemandOverflow)?;
    if inputs.budget.writer_tokens() < declared_writer_token_maximum {
        return Err(TrialSpecError::InsufficientTrialWriterBudget {
            required: declared_writer_token_maximum,
            available: inputs.budget.writer_tokens(),
        });
    }
    if inputs.budget.writer_tokens() > campaign.budget().max_writer_tokens
        || inputs.budget.controller_tokens() > campaign.budget().max_controller_tokens
        || inputs.budget.evaluations() > campaign.budget().max_evaluations
    {
        return Err(TrialSpecError::TrialBudgetExceedsCampaign);
    }
    let stage_budget_maxima = validate_stage_budget_maxima(
        inputs.stage_graph,
        &inputs.stage_budget_maxima,
        declared_writer_token_maximum,
        inputs.budget,
        treatment.prompt_topology,
    )?;

    Ok(ResolvedTrialInputs {
        case,
        treatment,
        treatment_fingerprint,
        generate_stage_id: generate_stage.id(),
        declared_writer_token_maximum,
        stage_budget_maxima,
    })
}

fn validate_stage_budget_maxima(
    graph: &StageGraph,
    maxima: &[FrozenStageBudgetMaximum],
    declared_writer_token_maximum: u64,
    trial_budget: TrialBudgetLimits,
    prompt_topology: PromptTopology,
) -> Result<Vec<FrozenStageBudgetMaximum>, TrialSpecError> {
    let graph_ids = graph
        .stages()
        .iter()
        .map(loom_research_types::FrozenStageSpec::id)
        .collect::<BTreeSet<_>>();
    let mut by_stage = BTreeMap::new();
    for entry in maxima {
        if !graph_ids.contains(&entry.stage_id) {
            return Err(TrialSpecError::UnknownStageBudget(entry.stage_id));
        }
        if by_stage.insert(entry.stage_id, entry.maximum).is_some() {
            return Err(TrialSpecError::DuplicateStageBudget(entry.stage_id));
        }
    }

    let mut canonical = Vec::with_capacity(graph.stages().len());
    let mut total = BudgetAmount::default();
    for stage in graph.stages() {
        let maximum = by_stage
            .remove(&stage.id())
            .ok_or(TrialSpecError::MissingStageBudget(stage.id()))?;
        maximum.verify_global_bounds()?;
        if !valid_stage_budget_shape(
            stage.stage(),
            maximum,
            declared_writer_token_maximum,
            prompt_topology,
        ) {
            return Err(TrialSpecError::InvalidStageBudget(stage.stage()));
        }
        total = total.checked_add(maximum)?;
        canonical.push(FrozenStageBudgetMaximum::new(stage.id(), maximum));
    }
    if !total.fits_limits(trial_budget) {
        return Err(TrialSpecError::StageBudgetsExceedTrial);
    }
    Ok(canonical)
}

const fn valid_stage_budget_shape(
    stage: FrozenTrialStage,
    maximum: BudgetAmount,
    declared_writer_token_maximum: u64,
    prompt_topology: PromptTopology,
) -> bool {
    if maximum.wall_time_ms() == 0 {
        return false;
    }
    match stage {
        FrozenTrialStage::BacktranslateMask | FrozenTrialStage::Plan => {
            maximum.writer_tokens() == 0
                && match prompt_topology {
                    PromptTopology::ExactDirectContinuation => maximum.controller_tokens() == 0,
                    _ => maximum.controller_tokens() > 0,
                }
                && maximum.evaluations() == 0
        }
        FrozenTrialStage::Generate => {
            maximum.writer_tokens() == declared_writer_token_maximum
                && maximum.controller_tokens() == 0
                && maximum.evaluations() == 0
        }
        FrozenTrialStage::Evaluate => {
            maximum.writer_tokens() == 0
                && maximum.controller_tokens() > 0
                && maximum.evaluations() > 0
        }
        FrozenTrialStage::FreezeInputs
        | FrozenTrialStage::Retrieve
        | FrozenTrialStage::CompilePrompt
        | FrozenTrialStage::Admit
        | FrozenTrialStage::Assemble
        | FrozenTrialStage::Gate
        | FrozenTrialStage::Describe
        | FrozenTrialStage::Archive => {
            maximum.writer_tokens() == 0
                && maximum.controller_tokens() == 0
                && maximum.evaluations() == 0
        }
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum TrialSpecError {
    #[error(transparent)]
    Budget(#[from] crate::BudgetError),
    #[error("campaign manifest failed its integrity check")]
    CampaignManifestIntegrity,
    #[error("expected a loom.campaign.v1 manifest")]
    WrongCampaignManifestFormat,
    #[error("campaign case key is absent")]
    CampaignCaseNotFound,
    #[error("treatment key is absent")]
    TreatmentNotFound,
    #[error("base-writer binding failed source recompilation")]
    ModelBindingIntegrity,
    #[error("campaign references a different model-bindings artifact")]
    CampaignModelBindingsMismatch,
    #[error("model context is too small: required {required}, available {available}")]
    InsufficientModelContext { required: u32, available: u32 },
    #[error("compiled prompt failed its integrity check")]
    CompiledPromptIntegrity,
    #[error("compiled prompt project does not match the trial")]
    PromptProjectMismatch,
    #[error("compiled prompt campaign does not match the trial")]
    PromptCampaignMismatch,
    #[error("compiled prompt case does not match the trial")]
    PromptCaseMismatch,
    #[error("compiled prompt is not scoped to the frozen Generate stage")]
    PromptStageMismatch,
    #[error("compiled prompt treatment does not match the campaign treatment")]
    PromptTreatmentMismatch,
    #[error("compiled prompt topology proof does not match treatment, prompt, or live evidence")]
    PromptTopologyProofMismatch,
    #[error("declared writer demand overflowed")]
    WriterDemandOverflow,
    #[error("trial writer budget {available} cannot reserve declared demand {required}")]
    InsufficientTrialWriterBudget { required: u64, available: u64 },
    #[error("trial budget exceeds its campaign budget")]
    TrialBudgetExceedsCampaign,
    #[error("frozen stage budgets contain duplicate stage {0}")]
    DuplicateStageBudget(StageId),
    #[error("frozen stage budgets contain unknown stage {0}")]
    UnknownStageBudget(StageId),
    #[error("frozen stage budget is missing for stage {0}")]
    MissingStageBudget(StageId),
    #[error("frozen stage budget has an invalid resource shape for {0:?}")]
    InvalidStageBudget(FrozenTrialStage),
    #[error("sum of frozen stage maxima exceeds the trial budget")]
    StageBudgetsExceedTrial,
    #[error("exact prompt length is not representable")]
    PromptLengthOverflow,
    #[error("frozen trial fingerprint mismatch")]
    TrialFingerprintMismatch,
    #[error("frozen trial canonical record serialization failed")]
    CanonicalRecordSerialization,
}

pub fn fingerprint_campaign_case(case: &CampaignCaseSpec) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(CASE_FINGERPRINT_DOMAIN);
    update_text(&mut digest, case.id.as_str());
    update_text(&mut digest, case.genre_function.as_str());
    digest.update(case.source_sha256.as_bytes());
    digest.update(case.max_context_tokens.to_be_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

pub fn fingerprint_treatment(treatment: &TreatmentSpec) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(TREATMENT_FINGERPRINT_DOMAIN);
    update_text(&mut digest, treatment.id.as_str());
    digest.update([prompt_topology_tag(treatment.prompt_topology)]);
    digest.update(treatment.samples_per_case.to_be_bytes());
    digest.update(treatment.max_output_tokens.to_be_bytes());
    update_sampler(&mut digest, &treatment.sampler);
    digest.update((treatment.control_parameters.len() as u64).to_be_bytes());
    for (key, value) in treatment.control_parameters.iter() {
        update_text(&mut digest, key.as_str());
        digest.update(value.to_bits().to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

pub fn fingerprint_stage_graph(graph: &StageGraph) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(STAGE_GRAPH_FINGERPRINT_DOMAIN);
    digest.update(graph.id().as_ulid().to_bytes());
    digest.update((graph.stages().len() as u64).to_be_bytes());
    for stage in graph.stages() {
        digest.update(stage.id().as_ulid().to_bytes());
        digest.update([stage_kind_tag(stage.stage())]);
        digest.update(stage.spec_fingerprint().as_bytes());
        digest.update((stage.dependencies().len() as u64).to_be_bytes());
        for dependency in stage.dependencies() {
            digest.update(dependency.as_ulid().to_bytes());
        }
    }
    digest.update(graph.output().as_ulid().to_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

pub(crate) const fn stage_kind_tag(stage: FrozenTrialStage) -> u8 {
    match stage {
        FrozenTrialStage::FreezeInputs => 0,
        FrozenTrialStage::BacktranslateMask => 1,
        FrozenTrialStage::Plan => 2,
        FrozenTrialStage::Retrieve => 3,
        FrozenTrialStage::CompilePrompt => 4,
        FrozenTrialStage::Generate => 5,
        FrozenTrialStage::Admit => 6,
        FrozenTrialStage::Assemble => 7,
        FrozenTrialStage::Gate => 8,
        FrozenTrialStage::Evaluate => 9,
        FrozenTrialStage::Describe => 10,
        FrozenTrialStage::Archive => 11,
    }
}

fn verify_binding_integrity(binding: &BaseWriterBinding) -> Result<(), TrialSpecError> {
    let manifest = CompiledManifest::compile(binding.manifest_source_bytes())
        .map_err(|_| TrialSpecError::ModelBindingIntegrity)?;
    let rebuilt = BaseWriterBinding::compile(&manifest, binding.binding_id())
        .map_err(|_| TrialSpecError::ModelBindingIntegrity)?;
    if rebuilt != *binding {
        return Err(TrialSpecError::ModelBindingIntegrity);
    }
    Ok(())
}

fn fingerprint_trial(spec: &FrozenTrialSpec) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(TRIAL_FINGERPRINT_DOMAIN);
    digest.update(spec.project_id.as_ulid().to_bytes());
    digest.update(spec.project_input_fingerprint.as_bytes());
    digest.update(spec.campaign_id.as_ulid().to_bytes());
    digest.update(spec.case_id.as_ulid().to_bytes());
    digest.update(spec.campaign_manifest_source_fingerprint.as_bytes());
    digest.update(spec.campaign_manifest_fingerprint.as_bytes());
    update_text(&mut digest, spec.campaign_case_key.as_str());
    digest.update(spec.campaign_case_fingerprint.as_bytes());
    update_text(&mut digest, spec.treatment_key.as_str());
    digest.update(spec.treatment_fingerprint.as_bytes());
    digest.update([prompt_topology_tag(spec.prompt_topology)]);
    digest.update(spec.prompt_topology_evidence_fingerprint.as_bytes());
    digest.update(spec.stage_graph_id.as_ulid().to_bytes());
    digest.update(spec.stage_graph_fingerprint.as_bytes());
    digest.update(spec.generate_stage_id.as_ulid().to_bytes());
    digest.update(spec.prompt_content_fingerprint.as_bytes());
    digest.update(spec.exact_prompt_blob_id.as_bytes());
    digest.update(spec.exact_prompt_byte_len.to_be_bytes());
    digest.update(spec.model_binding_manifest_source_fingerprint.as_bytes());
    digest.update(spec.model_binding_manifest_fingerprint.as_bytes());
    digest.update(spec.model_binding_fingerprint.as_bytes());
    digest.update(spec.model_fingerprint.as_bytes());
    digest.update(spec.tokenizer_fingerprint.as_bytes());
    digest.update(spec.expected_writer_call_count.to_be_bytes());
    digest.update(spec.declared_writer_token_maximum.to_be_bytes());
    digest.update((spec.stage_budget_maxima.len() as u64).to_be_bytes());
    for entry in &spec.stage_budget_maxima {
        digest.update(entry.stage_id.as_ulid().to_bytes());
        entry.maximum.update_digest(&mut digest);
    }
    digest.update(spec.budget.fingerprint().as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn update_sampler(digest: &mut Sha256, sampler: &SamplerTreatment) {
    digest.update(sampler.temperature.to_bits().to_be_bytes());
    digest.update(sampler.top_k.to_be_bytes());
    digest.update(sampler.top_p.to_bits().to_be_bytes());
    digest.update(sampler.min_p.to_bits().to_be_bytes());
    digest.update(sampler.typical_p.to_bits().to_be_bytes());
    digest.update(sampler.repetition_penalty.to_bits().to_be_bytes());
    match sampler.cfg_scale {
        Some(value) => {
            digest.update([1]);
            digest.update(value.to_bits().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn update_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
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

const fn prompt_topology_tag(topology: PromptTopology) -> u8 {
    match topology {
        PromptTopology::ExactDirectContinuation => 0,
        PromptTopology::NaturalBookfrontContinuation => 1,
        PromptTopology::EventLedgerOperatorPair => 2,
        PromptTopology::NearestProjectAnchor => 3,
        PromptTopology::RawSceneApprenticeship => 4,
        PromptTopology::GraphRawPairedApprenticeship => 5,
        PromptTopology::StagedMovementAssembly => 6,
    }
}

#[cfg(test)]
mod controller_free_budget_tests {
    use super::*;

    #[test]
    fn zero_controller_stage_maximum_is_direct_continuation_only() {
        let zero_controller = BudgetAmount::new(0, 0, 0, 1).expect("bounded stage maximum");
        let positive_controller = BudgetAmount::new(0, 1, 0, 1).expect("bounded stage maximum");
        for stage in [FrozenTrialStage::BacktranslateMask, FrozenTrialStage::Plan] {
            assert!(valid_stage_budget_shape(
                stage,
                zero_controller,
                32,
                PromptTopology::ExactDirectContinuation,
            ));
            assert!(
                !valid_stage_budget_shape(
                    stage,
                    positive_controller,
                    32,
                    PromptTopology::ExactDirectContinuation,
                ),
                "direct continuation must not reserve a controller that it never dispatches"
            );
            for topology in [
                PromptTopology::NaturalBookfrontContinuation,
                PromptTopology::EventLedgerOperatorPair,
                PromptTopology::NearestProjectAnchor,
                PromptTopology::RawSceneApprenticeship,
                PromptTopology::GraphRawPairedApprenticeship,
                PromptTopology::StagedMovementAssembly,
            ] {
                assert!(
                    !valid_stage_budget_shape(stage, zero_controller, 32, topology),
                    "{topology:?} must reserve a controller before dispatch"
                );
                assert!(valid_stage_budget_shape(
                    stage,
                    positive_controller,
                    32,
                    topology,
                ));
            }
        }
    }
}
