use std::{error::Error, fmt};

use loom_research_types::{
    CallScope, CompiledBaseCompletionPrompt, FrozenBaseCompletionPrompt,
    MAX_COMPLETION_PROMPT_BYTES, NonEmptyByteRange,
};
use loom_types::{BlobId, ProjectId};

use crate::BaseWriterBinding;
#[cfg(any(feature = "native-llama", test))]
use crate::canonical::CanonicalDigest;

pub const MAX_BASE_PROMPT_BYTES: usize = MAX_COMPLETION_PROMPT_BYTES;

#[cfg(any(feature = "native-llama", test))]
const COMPILED_PROMPT_DOMAIN: &str = "loom/compiled-base-completion-prompt/v1";
#[cfg(any(feature = "native-llama", test))]
const PROMPT_TOKEN_DOMAIN: &str = "loom/exact-native-token-ids/v1";

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub enum PromptFormEvidence {
    Completion,
}

impl fmt::Debug for PromptFormEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PromptFormEvidence::Completion")
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub enum PromptTokenPolicyEvidence {
    NoBosParseSpecial,
}

impl fmt::Debug for PromptTokenPolicyEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PromptTokenPolicyEvidence::NoBosParseSpecial")
    }
}

/// Move-only evidence for the exact prompt admitted to one verified batch.
///
/// Construction is crate-private and occurs only after backend prompt
/// preparation is checked. The raw bytes, their blob ID, Completion/NoBos
/// semantics, ordered token IDs, token fingerprint, and compiled fingerprint
/// therefore travel as one object rather than caller-reassembled fields.
///
/// ```compile_fail
/// use loom_inference::ExactPromptEvidence;
/// fn duplicate(value: ExactPromptEvidence) {
///     let _copy = value.clone();
/// }
/// ```
///
/// ```compile_fail
/// use loom_inference::ExactPromptEvidence;
/// fn require_default<T: Default>() {}
/// require_default::<ExactPromptEvidence>();
/// ```
///
/// ```compile_fail
/// use loom_inference::ExactPromptEvidence;
/// fn require_serialize<T: serde::Serialize>() {}
/// require_serialize::<ExactPromptEvidence>();
/// ```
///
/// ```compile_fail
/// use loom_inference::ExactPromptEvidence;
/// let _forged = ExactPromptEvidence {};
/// ```
pub struct ExactPromptEvidence {
    frozen_specification: FrozenBaseCompletionPrompt,
    source_prompt_fingerprint: BlobId,
    content_fingerprint: BlobId,
    tail_prompt_range: NonEmptyByteRange,
    raw_utf8: Vec<u8>,
    raw_blob_id: BlobId,
    form: PromptFormEvidence,
    token_policy: PromptTokenPolicyEvidence,
    ordered_token_ids: Vec<u32>,
    token_fingerprint: BlobId,
    compiled_fingerprint: BlobId,
}

impl fmt::Debug for ExactPromptEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactPromptEvidence")
            .field("project_id", &self.frozen_specification.project_id())
            .field("scope", &self.frozen_specification.scope())
            .field("source_prompt_fingerprint", &self.source_prompt_fingerprint)
            .field("content_fingerprint", &self.content_fingerprint)
            .field("tail_prompt_range", &self.tail_prompt_range)
            .field("raw_utf8_bytes", &self.raw_utf8.len())
            .field("raw_blob_id", &self.raw_blob_id)
            .field("form", &self.form)
            .field("token_policy", &self.token_policy)
            .field("token_count", &self.ordered_token_ids.len())
            .field("token_fingerprint", &self.token_fingerprint)
            .field("compiled_fingerprint", &self.compiled_fingerprint)
            .finish()
    }
}

impl ExactPromptEvidence {
    #[cfg(any(feature = "native-llama", test))]
    pub(crate) fn verified_completion_no_bos(
        prompt: CompiledBaseCompletionPrompt,
        token_ids: Vec<u32>,
    ) -> Self {
        let (
            frozen_specification,
            raw_utf8,
            tail_prompt_range,
            content_fingerprint,
            source_prompt_fingerprint,
        ) = prompt.into_parts();
        let raw_blob_id = BlobId::digest(&raw_utf8);
        let mut token_digest = CanonicalDigest::new(PROMPT_TOKEN_DOMAIN);
        token_digest.token_ids_u32(&token_ids);
        let token_fingerprint = token_digest.finish_blob();

        let mut compiled_digest = CanonicalDigest::new(COMPILED_PROMPT_DOMAIN);
        compiled_digest.blob(source_prompt_fingerprint);
        compiled_digest.blob(raw_blob_id);
        compiled_digest.u32(1); // PromptForm::Completion
        compiled_digest.u32(1); // PromptTokenPolicy::NoBosParseSpecial
        compiled_digest.token_ids_u32(&token_ids);
        let compiled_fingerprint = compiled_digest.finish_blob();

        Self {
            frozen_specification,
            source_prompt_fingerprint,
            content_fingerprint,
            tail_prompt_range,
            raw_utf8,
            raw_blob_id,
            form: PromptFormEvidence::Completion,
            token_policy: PromptTokenPolicyEvidence::NoBosParseSpecial,
            ordered_token_ids: token_ids,
            token_fingerprint,
            compiled_fingerprint,
        }
    }

    pub const fn frozen_specification(&self) -> &FrozenBaseCompletionPrompt {
        &self.frozen_specification
    }

    pub const fn project_id(&self) -> ProjectId {
        self.frozen_specification.project_id()
    }

    pub const fn scope(&self) -> CallScope {
        self.frozen_specification.scope()
    }

    pub const fn treatment_recipe_fingerprint(&self) -> BlobId {
        self.frozen_specification.treatment_recipe_fingerprint()
    }

    pub const fn source_prompt_fingerprint(&self) -> BlobId {
        self.source_prompt_fingerprint
    }

    pub const fn content_fingerprint(&self) -> BlobId {
        self.content_fingerprint
    }

    pub const fn tail_prompt_range(&self) -> NonEmptyByteRange {
        self.tail_prompt_range
    }

    pub fn raw_utf8(&self) -> &[u8] {
        &self.raw_utf8
    }

    pub const fn raw_blob_id(&self) -> BlobId {
        self.raw_blob_id
    }

    pub const fn form(&self) -> PromptFormEvidence {
        self.form
    }

    pub const fn token_policy(&self) -> PromptTokenPolicyEvidence {
        self.token_policy
    }

    pub fn ordered_token_ids(&self) -> &[u32] {
        &self.ordered_token_ids
    }

    pub const fn token_fingerprint(&self) -> BlobId {
        self.token_fingerprint
    }

    pub const fn compiled_fingerprint(&self) -> BlobId {
        self.compiled_fingerprint
    }
}

/// Backend-neutral synchronous contract for completion-shaped base writers.
///
/// Associated execution types let native, remote, or test backends preserve
/// their own move-only authorities without introducing backend structs into
/// Loom's public root API.
pub trait BaseWriterBackend {
    type PreparedPrompt;
    type CaseSpec;
    type Ticket;
    type VerifiedBatch;
    type Error: Error + Send + Sync + 'static;

    fn prepare_completion(
        &self,
        binding: BaseWriterBinding,
        prompt: CompiledBaseCompletionPrompt,
    ) -> Result<Self::PreparedPrompt, Self::Error>;

    fn start(
        &self,
        prepared: Self::PreparedPrompt,
        cases: Vec<Self::CaseSpec>,
    ) -> Result<Self::Ticket, Self::Error>;

    fn wait(&self, ticket: Self::Ticket) -> Result<Self::VerifiedBatch, Self::Error>;
}

/// Backend-neutral lifecycle for controlled completion-shaped base writers.
///
/// `wait_controlled` consumes the backend's move-only owner-worker execution
/// seal and returns an admitted batch. Implementations may retain opaque worker
/// lineage so many calls can later be bound to one final joined worker without
/// forcing destructive per-call shutdown.
pub trait ControlledBaseWriterBackend {
    type PreparedPrompt;
    type CaseSpec;
    type ControlProgram;
    type Ticket;
    type VerifiedBatch;
    type Error: Error + Send + Sync + 'static;

    fn prepare_controlled_completion(
        &self,
        binding: BaseWriterBinding,
        prompt: CompiledBaseCompletionPrompt,
    ) -> Result<Self::PreparedPrompt, Self::Error>;

    fn start_controlled(
        &self,
        prepared: Self::PreparedPrompt,
        cases: Vec<Self::CaseSpec>,
        control: Self::ControlProgram,
    ) -> Result<Self::Ticket, Self::Error>;

    fn wait_controlled(&self, ticket: Self::Ticket) -> Result<Self::VerifiedBatch, Self::Error>;
}

/// Backend-neutral lifecycle for exact-token embeddings.
///
/// Embedding results remain diagnostic data. This trait carries no source
/// binding, learning-dataset issuer, ranker activation, promotion, benchmark,
/// or base-writer authorship authority; a later store-owned authority layer
/// must explicitly bind any diagnostic vector before learning can use it.
pub trait ExactEmbeddingBackend {
    type Request;
    type Ticket;
    type VerifiedDiagnostic;
    type Error: Error + Send + Sync + 'static;

    fn start_embeddings(&self, request: Self::Request) -> Result<Self::Ticket, Self::Error>;

    fn wait_embeddings(
        &self,
        ticket: Self::Ticket,
    ) -> Result<Self::VerifiedDiagnostic, Self::Error>;
}

/// Backend-neutral controller contract.
///
/// Controller products are intentionally distinct from base-writer evidence:
/// an implementation may return plans, state proposals, graphs, labels, or
/// abstentions, but cannot mint a writer [`ExactPromptEvidence`] or verified
/// writer envelope through this trait.
pub trait ControllerBackend {
    type Request;
    type Ticket;
    type Response;
    type Error: Error + Send + Sync + 'static;

    fn start(&self, request: Self::Request) -> Result<Self::Ticket, Self::Error>;

    fn wait(&self, ticket: Self::Ticket) -> Result<Self::Response, Self::Error>;
}

#[cfg(test)]
mod tests {
    use loom_research_types::{
        CampaignId, CompletionPromptTail, ExactPromptSource, FrozenBaseCompletionPrompt,
        NonEmptyByteRange, StageAttemptId, StageId, TrialCaseId,
    };
    use loom_types::RevisionId;

    use super::*;

    fn compiled_prompt_for(
        bytes: &[u8],
        project_id: ProjectId,
        scope: CallScope,
        revision_id: RevisionId,
    ) -> CompiledBaseCompletionPrompt {
        let tail = CompletionPromptTail::live_manuscript(
            revision_id,
            BlobId::digest(bytes),
            NonEmptyByteRange::new(0, bytes.len() as u64).expect("nonempty fixture tail"),
        )
        .expect("bounded fixture tail");
        FrozenBaseCompletionPrompt::new(
            project_id,
            scope,
            BlobId::digest(b"fixture-treatment"),
            Vec::new(),
            tail,
        )
        .expect("fixture prompt specification")
        .compile(
            bytes.to_vec(),
            &[ExactPromptSource::new(revision_id, bytes)],
        )
        .expect("exact fixture prompt")
    }

    fn compiled_prompt(bytes: &[u8]) -> CompiledBaseCompletionPrompt {
        compiled_prompt_for(
            bytes,
            ProjectId::new(),
            CallScope::new(
                CampaignId::new(),
                StageId::new(),
                StageAttemptId::new(),
                TrialCaseId::new(),
            ),
            RevisionId::new(),
        )
    }

    #[test]
    fn compiled_prompt_evidence_debug_is_content_redacted() {
        let secret = b"a private manuscript tail".to_vec();
        let prompt = compiled_prompt(&secret);
        let source_prompt_fingerprint = prompt.fingerprint();
        let content_fingerprint = prompt.content_fingerprint();
        let prompt_debug = format!("{prompt:?}");
        let evidence = ExactPromptEvidence::verified_completion_no_bos(prompt, vec![7, 11, 13]);
        assert_eq!(
            evidence.source_prompt_fingerprint(),
            source_prompt_fingerprint
        );
        assert_eq!(evidence.content_fingerprint(), content_fingerprint);

        let evidence_debug = format!("{evidence:?}");
        for debug in [&prompt_debug, &evidence_debug] {
            assert!(!debug.contains("private manuscript"));
            assert!(!debug.contains("[7, 11, 13]"));
        }
    }

    #[test]
    fn prompt_evidence_preserves_retry_stable_content_identity() {
        let project_id = ProjectId::new();
        let campaign_id = CampaignId::new();
        let stage_id = StageId::new();
        let case_id = TrialCaseId::new();
        let revision_id = RevisionId::new();
        let build = |attempt_id| {
            ExactPromptEvidence::verified_completion_no_bos(
                compiled_prompt_for(
                    b"manuscript tail",
                    project_id,
                    CallScope::new(campaign_id, stage_id, attempt_id, case_id),
                    revision_id,
                ),
                vec![1, 2, 3],
            )
        };
        let first = build(StageAttemptId::new());
        let retry = build(StageAttemptId::new());

        assert_ne!(
            first.source_prompt_fingerprint(),
            retry.source_prompt_fingerprint()
        );
        assert_eq!(first.content_fingerprint(), retry.content_fingerprint());
        assert_ne!(first.compiled_fingerprint(), retry.compiled_fingerprint());
    }

    #[test]
    fn prompt_evidence_binds_bytes_semantics_and_ordered_tokens() {
        let project_id = ProjectId::new();
        let scope = CallScope::new(
            CampaignId::new(),
            StageId::new(),
            StageAttemptId::new(),
            TrialCaseId::new(),
        );
        let revision_id = RevisionId::new();
        let left = ExactPromptEvidence::verified_completion_no_bos(
            compiled_prompt_for(b"manuscript tail", project_id, scope, revision_id),
            vec![1, 2, 3],
        );
        let reordered = ExactPromptEvidence::verified_completion_no_bos(
            compiled_prompt_for(b"manuscript tail", project_id, scope, revision_id),
            vec![1, 3, 2],
        );
        let changed_bytes = ExactPromptEvidence::verified_completion_no_bos(
            compiled_prompt_for(b"manuscript tail!", project_id, scope, RevisionId::new()),
            vec![1, 2, 3],
        );

        assert_ne!(left.token_fingerprint(), reordered.token_fingerprint());
        assert_ne!(
            left.compiled_fingerprint(),
            reordered.compiled_fingerprint()
        );
        assert_ne!(left.raw_blob_id(), changed_bytes.raw_blob_id());
        assert_ne!(
            left.compiled_fingerprint(),
            changed_bytes.compiled_fingerprint()
        );
        assert_eq!(left.form(), PromptFormEvidence::Completion);
        assert_eq!(
            left.token_policy(),
            PromptTokenPolicyEvidence::NoBosParseSpecial
        );
    }
}
