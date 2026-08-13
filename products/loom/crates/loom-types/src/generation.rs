use serde::{Deserialize, Serialize};

use crate::{
    ArtifactId, BlobId, BranchId, ByteRange, CandidateId, CommandId, DocumentId, GenerationEventId,
    GenerationRunId, ModelEnvironmentId, ProjectId, RevisionId, SelectionId,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Critic,
    Writer,
}

impl ModelRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Critic => "critic",
            Self::Writer => "writer",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelEnvironment {
    pub environment_id: ModelEnvironmentId,
    pub model_identifier: String,
    pub model_fingerprint: BlobId,
    pub tokenizer_fingerprint: BlobId,
    pub backend_identifier: String,
    #[serde(default)]
    pub capabilities: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthorityPolicy {
    pub policy_version: u32,
    pub writer_environment_artifact_ids: Vec<ArtifactId>,
    pub critic_environment_artifact_ids: Vec<ArtifactId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptMode {
    Completion,
    FillInMiddle,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptRecipe {
    pub mode: PromptMode,
    pub exact_prompt_blob_id: BlobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_prompt_token_ids: Option<Vec<u32>>,
    pub ordered_input_artifact_ids: Vec<ArtifactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_token_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextRecipe {
    pub source_revision_id: RevisionId,
    pub ordered_source_artifact_ids: Vec<ArtifactId>,
    pub token_budget: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_evidence_blob_id: Option<BlobId>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GenerationStart {
    pub run_id: GenerationRunId,
    pub branch_id: BranchId,
    pub document_id: DocumentId,
    pub source_revision_id: RevisionId,
    pub target_range: ByteRange,
    pub model_environment_artifact_id: ArtifactId,
    pub prompt_recipe_artifact_id: ArtifactId,
    pub context_recipe_artifact_id: ArtifactId,
    pub authority_policy_artifact_id: ArtifactId,
    pub seed: u64,
    #[serde(default)]
    pub sampling: serde_json::Value,
}

pub type GenerationRun = GenerationStart;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TokenObservation {
    pub token_id: u32,
    #[serde(default)]
    pub token_bytes: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_model_logprob: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_constraint_logprob: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_sampler_logprob: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TokenTrace {
    pub generated_token_ids: Vec<u32>,
    #[serde(default)]
    pub observations: Vec<TokenObservation>,
    pub raw_event_stream_blob_id: BlobId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<GenerationProvenance>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceEvidenceKind {
    Fixture,
    HistoricalReceipt,
    LiveInference,
    Mock,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct GenerationMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_prefix_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_cache_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_cache_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode_tokens_per_second: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GenerationProvenance {
    pub evidence_kind: InferenceEvidenceKind,
    #[serde(default)]
    pub metrics: GenerationMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_receipt_blob_id: Option<BlobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_state_blob_id: Option<BlobId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GeneratedSpan {
    pub candidate_id: CandidateId,
    pub run_id: GenerationRunId,
    pub branch_id: BranchId,
    pub output_blob_id: BlobId,
    pub output_byte_range: ByteRange,
    pub token_trace_artifact_id: ArtifactId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthorshipAttestation {
    pub candidate_id: CandidateId,
    pub generated_span_artifact_id: ArtifactId,
    pub promoted_revision_id: RevisionId,
    pub promotion_command_id: CommandId,
    /// The strongest claim this record makes. Interactive selection is useful
    /// product provenance, but is not verified base-writer research authority.
    pub evidence_class: AuthorshipEvidenceClass,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorshipEvidenceClass {
    DiagnosticGenerationSelectedByUser,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionDecision {
    KeepAlternative,
    Promote,
    Reject,
}

impl SelectionDecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::KeepAlternative => "keep_alternative",
            Self::Promote => "promote",
            Self::Reject => "reject",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectionEvent {
    pub selection_id: SelectionId,
    pub candidate_id: CandidateId,
    pub decision: SelectionDecision,
    pub source_revision_id: RevisionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resulting_revision_id: Option<RevisionId>,
    pub command_id: CommandId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationTerminalStatus {
    Cancelled,
    Completed,
    Failed,
    Pruned,
    Rejected,
}

impl GenerationTerminalStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Pruned => "pruned",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GenerationEventKind {
    Queued,
    Prefilling,
    Generating,
    TextDelta {
        text: String,
    },
    Token {
        observation: TokenObservation,
    },
    Warning {
        code: String,
        message: String,
    },
    CancellationRequested,
    CandidateReady {
        candidate_id: CandidateId,
        generated_span_artifact_id: ArtifactId,
    },
}

impl GenerationEventKind {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Prefilling => "prefilling",
            Self::Generating => "generating",
            Self::TextDelta { .. } => "text_delta",
            Self::Token { .. } => "token",
            Self::Warning { .. } => "warning",
            Self::CancellationRequested => "cancellation_requested",
            Self::CandidateReady { .. } => "candidate_ready",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GenerationEvent {
    pub event_id: GenerationEventId,
    pub run_id: GenerationRunId,
    pub branch_id: BranchId,
    pub sequence: u64,
    pub kind: GenerationEventKind,
    pub occurred_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GenerationTerminalEvent {
    pub event_id: GenerationEventId,
    pub run_id: GenerationRunId,
    pub branch_id: BranchId,
    pub sequence: u64,
    pub status: GenerationTerminalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_id: Option<CandidateId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub occurred_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BranchCandidate {
    pub candidate_id: CandidateId,
    pub run_id: GenerationRunId,
    pub branch_id: BranchId,
    pub generated_span_artifact_id: ArtifactId,
    pub token_trace_artifact_id: ArtifactId,
    pub output_blob_id: BlobId,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WeaveCommand {
    pub generation: GenerationStart,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CancelGenerationCommand {
    pub run_id: GenerationRunId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromoteCandidateCommand {
    pub candidate_id: CandidateId,
    pub expected_source_revision_id: RevisionId,
    pub expected_visible_blob_id: BlobId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeepAlternativeCommand {
    pub candidate_id: CandidateId,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "command", content = "payload", rename_all = "snake_case")]
pub enum LoomCommand {
    Weave(WeaveCommand),
    CancelGeneration(CancelGenerationCommand),
    PromoteCandidate(PromoteCandidateCommand),
    KeepAlternative(KeepAlternativeCommand),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CommandEnvelope {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub issued_at_ms: i64,
    pub command: LoomCommand,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "event", content = "payload", rename_all = "snake_case")]
pub enum LoomEvent {
    Generation(GenerationEvent),
    GenerationTerminal(GenerationTerminalEvent),
    CandidatePromoted {
        candidate_id: CandidateId,
        revision_id: RevisionId,
        command_id: CommandId,
    },
    AlternativeKept {
        candidate_id: CandidateId,
        command_id: CommandId,
    },
}
