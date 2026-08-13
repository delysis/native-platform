//! Exact, source-bound chat prompt evidence for controller and critic models.
//!
//! This surface is deliberately separate from completion-shaped base-writer
//! prompts. A controller may use a model chat template, but every message,
//! candidate occurrence, source range, ordering decision, and rendered token
//! sequence remains bound to the resulting move-only evidence.

use std::fmt;

use loom_research_types::NonEmptyByteRange;
use loom_types::{ArtifactId, BlobId};
use thiserror::Error;

use crate::canonical::CanonicalDigest;

pub const MAX_CONTROLLER_MESSAGES: usize = 32;
pub const MAX_CONTROLLER_MESSAGE_BYTES: usize = 128 * 1024;
pub const MAX_CONTROLLER_PROMPT_BYTES: usize = 512 * 1024;
pub const MAX_CONTROLLER_CANDIDATES: usize = 8;
pub const MAX_CONTROLLER_SOURCE_BINDINGS: usize = 128;
pub const MAX_CRITIC_KEY_BYTES: usize = 128;
pub const MAX_CHAT_TEMPLATE_BYTES: usize = 256 * 1024;

const PROMPT_SPEC_DOMAIN: &str = "loom/controller-prompt-spec/v1";
#[cfg(feature = "native-llama")]
const COMPILED_PROMPT_DOMAIN: &str = "loom/compiled-controller-prompt/v1";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ControllerMessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// One exact candidate occurrence made visible to a controller prompt.
///
/// `utf8` is checked against `blob_id`; only `range` is permitted to appear in
/// a source-bound message. The bytes are borrowed during compilation and are
/// not silently copied into durable prompt metadata.
#[derive(Clone, Copy)]
pub struct ControllerCandidateSource<'a> {
    pub occurrence_id: ArtifactId,
    pub blob_id: BlobId,
    pub utf8: &'a [u8],
    pub range: NonEmptyByteRange,
}

impl fmt::Debug for ControllerCandidateSource<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControllerCandidateSource")
            .field("occurrence_id", &self.occurrence_id)
            .field("blob_id", &self.blob_id)
            .field("byte_len", &self.utf8.len())
            .field("range", &self.range)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ControllerMessageSource {
    candidate_index: u16,
    message_range: NonEmptyByteRange,
}

impl ControllerMessageSource {
    pub const fn new(candidate_index: u16, message_range: NonEmptyByteRange) -> Self {
        Self {
            candidate_index,
            message_range,
        }
    }

    pub const fn candidate_index(self) -> u16 {
        self.candidate_index
    }

    pub const fn message_range(self) -> NonEmptyByteRange {
        self.message_range
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ControllerMessage {
    role: ControllerMessageRole,
    content: String,
    sources: Vec<ControllerMessageSource>,
}

impl fmt::Debug for ControllerMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControllerMessage")
            .field("role", &self.role)
            .field("content_bytes", &self.content.len())
            .field("content_blob_id", &BlobId::digest(self.content.as_bytes()))
            .field("source_count", &self.sources.len())
            .finish()
    }
}

impl ControllerMessage {
    pub fn new(
        role: ControllerMessageRole,
        content: impl Into<String>,
        sources: Vec<ControllerMessageSource>,
    ) -> Result<Self, ControllerPromptError> {
        let content = content.into();
        if content.is_empty() || content.len() > MAX_CONTROLLER_MESSAGE_BYTES {
            return Err(ControllerPromptError::InvalidMessageLength(content.len()));
        }
        if content.chars().any(|character| character == '\0') {
            return Err(ControllerPromptError::ProhibitedMessageNul);
        }
        Ok(Self {
            role,
            content,
            sources,
        })
    }

    pub const fn role(&self) -> ControllerMessageRole {
        self.role
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn sources(&self) -> &[ControllerMessageSource] {
        &self.sources
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum CriticChatTemplatePolicy {
    ModelDefault,
    ExactOverride(String),
}

impl fmt::Debug for CriticChatTemplatePolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModelDefault => formatter.write_str("CriticChatTemplatePolicy::ModelDefault"),
            Self::ExactOverride(template) => formatter
                .debug_struct("CriticChatTemplatePolicy::ExactOverride")
                .field("byte_len", &template.len())
                .field("blob_id", &BlobId::digest(template.as_bytes()))
                .finish(),
        }
    }
}

impl CriticChatTemplatePolicy {
    pub fn exact_override(template: impl Into<String>) -> Result<Self, ControllerPromptError> {
        let template = template.into();
        if template.is_empty() || template.len() > MAX_CHAT_TEMPLATE_BYTES {
            return Err(ControllerPromptError::InvalidTemplateLength(template.len()));
        }
        Ok(Self::ExactOverride(template))
    }

    pub fn override_template(&self) -> Option<&str> {
        match self {
            Self::ModelDefault => None,
            Self::ExactOverride(template) => Some(template),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum CriticEvaluationTask {
    Criterion { criterion_key: String },
    BlindPair,
}

impl fmt::Debug for CriticEvaluationTask {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Criterion { criterion_key } => formatter
                .debug_struct("CriticEvaluationTask::Criterion")
                .field(
                    "criterion_key_blob",
                    &BlobId::digest(criterion_key.as_bytes()),
                )
                .finish(),
            Self::BlindPair => formatter.write_str("CriticEvaluationTask::BlindPair"),
        }
    }
}

impl CriticEvaluationTask {
    pub fn criterion(criterion_key: impl Into<String>) -> Result<Self, ControllerPromptError> {
        let criterion_key = criterion_key.into();
        if criterion_key.is_empty()
            || criterion_key.len() > MAX_CRITIC_KEY_BYTES
            || criterion_key.chars().any(char::is_control)
        {
            return Err(ControllerPromptError::InvalidCriterionKey);
        }
        Ok(Self::Criterion { criterion_key })
    }

    pub fn criterion_key(&self) -> Option<&str> {
        match self {
            Self::Criterion { criterion_key } => Some(criterion_key),
            Self::BlindPair => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ControllerCandidateBinding {
    occurrence_id: ArtifactId,
    blob_id: BlobId,
    range: NonEmptyByteRange,
}

impl ControllerCandidateBinding {
    pub const fn occurrence_id(self) -> ArtifactId {
        self.occurrence_id
    }

    pub const fn blob_id(self) -> BlobId {
        self.blob_id
    }

    pub const fn range(self) -> NonEmptyByteRange {
        self.range
    }
}

/// Validated prompt specification before the live model renders its template.
pub struct CriticPromptSpec {
    task: CriticEvaluationTask,
    evaluation_attempt_id: ArtifactId,
    messages: Vec<ControllerMessage>,
    candidates: Vec<ControllerCandidateBinding>,
    evaluation_packet_fingerprint: BlobId,
    rubric_fingerprint: BlobId,
    template_policy: CriticChatTemplatePolicy,
    fingerprint: BlobId,
}

impl fmt::Debug for CriticPromptSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CriticPromptSpec")
            .field("task", &self.task)
            .field("evaluation_attempt_id", &self.evaluation_attempt_id)
            .field("message_count", &self.messages.len())
            .field("candidate_count", &self.candidates.len())
            .field(
                "evaluation_packet_fingerprint",
                &self.evaluation_packet_fingerprint,
            )
            .field("rubric_fingerprint", &self.rubric_fingerprint)
            .field("template_policy", &self.template_policy)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

impl CriticPromptSpec {
    pub fn compile(
        task: CriticEvaluationTask,
        evaluation_attempt_id: ArtifactId,
        messages: Vec<ControllerMessage>,
        candidates: &[ControllerCandidateSource<'_>],
        evaluation_packet_fingerprint: BlobId,
        rubric_fingerprint: BlobId,
        template_policy: CriticChatTemplatePolicy,
    ) -> Result<Self, ControllerPromptError> {
        if messages.is_empty() || messages.len() > MAX_CONTROLLER_MESSAGES {
            return Err(ControllerPromptError::InvalidMessageCount(messages.len()));
        }
        if candidates.is_empty() || candidates.len() > MAX_CONTROLLER_CANDIDATES {
            return Err(ControllerPromptError::InvalidCandidateCount(
                candidates.len(),
            ));
        }
        let total_bytes = messages.iter().try_fold(0_usize, |total, message| {
            total
                .checked_add(message.content.len())
                .ok_or(ControllerPromptError::PromptTooLarge)
        })?;
        if total_bytes > MAX_CONTROLLER_PROMPT_BYTES {
            return Err(ControllerPromptError::PromptTooLarge);
        }
        let total_bindings = messages
            .iter()
            .map(|message| message.sources.len())
            .sum::<usize>();
        if total_bindings == 0 || total_bindings > MAX_CONTROLLER_SOURCE_BINDINGS {
            return Err(ControllerPromptError::InvalidSourceBindingCount(
                total_bindings,
            ));
        }

        let mut bound_candidates = Vec::with_capacity(candidates.len());
        for source in candidates {
            if std::str::from_utf8(source.utf8).is_err()
                || BlobId::digest(source.utf8) != source.blob_id
                || source.range.checked_str(source.utf8).is_err()
            {
                return Err(ControllerPromptError::InvalidCandidateSource);
            }
            bound_candidates.push(ControllerCandidateBinding {
                occurrence_id: source.occurrence_id,
                blob_id: source.blob_id,
                range: source.range,
            });
        }

        let mut referenced = vec![false; candidates.len()];
        for message in &messages {
            for binding in &message.sources {
                let candidate_index = usize::from(binding.candidate_index);
                let Some(candidate) = candidates.get(candidate_index) else {
                    return Err(ControllerPromptError::UnknownCandidateIndex);
                };
                let message_text = binding
                    .message_range
                    .checked_str(message.content.as_bytes())
                    .map_err(|_| ControllerPromptError::InvalidMessageSourceRange)?;
                let candidate_text = candidate
                    .range
                    .checked_str(candidate.utf8)
                    .map_err(|_| ControllerPromptError::InvalidCandidateSource)?;
                if message_text.as_bytes() != candidate_text.as_bytes() {
                    return Err(ControllerPromptError::SourceBytesMismatch);
                }
                referenced[candidate_index] = true;
            }
        }
        if referenced.iter().any(|value| !value) {
            return Err(ControllerPromptError::UnreferencedCandidate);
        }

        let fingerprint = fingerprint_spec(
            &task,
            evaluation_attempt_id,
            &messages,
            &bound_candidates,
            evaluation_packet_fingerprint,
            rubric_fingerprint,
            &template_policy,
        );
        Ok(Self {
            task,
            evaluation_attempt_id,
            messages,
            candidates: bound_candidates,
            evaluation_packet_fingerprint,
            rubric_fingerprint,
            template_policy,
            fingerprint,
        })
    }

    pub const fn task(&self) -> &CriticEvaluationTask {
        &self.task
    }

    pub const fn evaluation_attempt_id(&self) -> ArtifactId {
        self.evaluation_attempt_id
    }

    pub fn messages(&self) -> &[ControllerMessage] {
        &self.messages
    }

    pub fn candidates(&self) -> &[ControllerCandidateBinding] {
        &self.candidates
    }

    pub const fn evaluation_packet_fingerprint(&self) -> BlobId {
        self.evaluation_packet_fingerprint
    }

    pub const fn rubric_fingerprint(&self) -> BlobId {
        self.rubric_fingerprint
    }

    pub const fn template_policy(&self) -> &CriticChatTemplatePolicy {
        &self.template_policy
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

/// Move-only evidence for the exact chat prompt admitted to native inference.
pub struct CriticPromptEvidence {
    specification: CriticPromptSpec,
    rendered_prompt_sha256: BlobId,
    exact_token_ids: Vec<i32>,
    exact_token_ids_fingerprint: BlobId,
    chat_template_fingerprint: BlobId,
    compiled_fingerprint: BlobId,
}

impl fmt::Debug for CriticPromptEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CriticPromptEvidence")
            .field("specification_fingerprint", &self.specification.fingerprint)
            .field("message_count", &self.specification.messages.len())
            .field("candidate_count", &self.specification.candidates.len())
            .field("rendered_prompt_sha256", &self.rendered_prompt_sha256)
            .field("token_count", &self.exact_token_ids.len())
            .field(
                "exact_token_ids_fingerprint",
                &self.exact_token_ids_fingerprint,
            )
            .field("chat_template_fingerprint", &self.chat_template_fingerprint)
            .field("compiled_fingerprint", &self.compiled_fingerprint)
            .finish()
    }
}

impl CriticPromptEvidence {
    #[cfg(feature = "native-llama")]
    pub(crate) fn mint(
        specification: CriticPromptSpec,
        rendered_prompt_sha256: BlobId,
        exact_token_ids: Vec<i32>,
        chat_template_fingerprint: BlobId,
    ) -> Result<Self, ControllerPromptError> {
        if exact_token_ids.is_empty() || exact_token_ids.iter().any(|token| *token < 0) {
            return Err(ControllerPromptError::InvalidTokenEvidence);
        }
        let mut tokens = CanonicalDigest::new("loom/controller-exact-token-ids/v1");
        tokens.u64(exact_token_ids.len() as u64);
        for token in &exact_token_ids {
            tokens.u32(token.cast_unsigned());
        }
        let exact_token_ids_fingerprint = tokens.finish_blob();
        let mut compiled = CanonicalDigest::new(COMPILED_PROMPT_DOMAIN);
        compiled.blob(specification.fingerprint);
        compiled.blob(rendered_prompt_sha256);
        compiled.blob(exact_token_ids_fingerprint);
        compiled.blob(chat_template_fingerprint);
        let compiled_fingerprint = compiled.finish_blob();
        Ok(Self {
            specification,
            rendered_prompt_sha256,
            exact_token_ids,
            exact_token_ids_fingerprint,
            chat_template_fingerprint,
            compiled_fingerprint,
        })
    }

    pub const fn specification(&self) -> &CriticPromptSpec {
        &self.specification
    }

    pub const fn task(&self) -> &CriticEvaluationTask {
        self.specification.task()
    }

    pub fn messages(&self) -> &[ControllerMessage] {
        self.specification.messages()
    }

    pub fn candidates(&self) -> &[ControllerCandidateBinding] {
        self.specification.candidates()
    }

    pub const fn evaluation_packet_fingerprint(&self) -> BlobId {
        self.specification.evaluation_packet_fingerprint()
    }

    pub const fn rubric_fingerprint(&self) -> BlobId {
        self.specification.rubric_fingerprint()
    }

    pub const fn template_policy(&self) -> &CriticChatTemplatePolicy {
        self.specification.template_policy()
    }

    pub const fn rendered_prompt_sha256(&self) -> BlobId {
        self.rendered_prompt_sha256
    }

    pub fn exact_token_ids(&self) -> &[i32] {
        &self.exact_token_ids
    }

    pub const fn exact_token_ids_fingerprint(&self) -> BlobId {
        self.exact_token_ids_fingerprint
    }

    pub const fn chat_template_fingerprint(&self) -> BlobId {
        self.chat_template_fingerprint
    }

    pub const fn compiled_fingerprint(&self) -> BlobId {
        self.compiled_fingerprint
    }
}

fn fingerprint_spec(
    task: &CriticEvaluationTask,
    evaluation_attempt_id: ArtifactId,
    messages: &[ControllerMessage],
    candidates: &[ControllerCandidateBinding],
    evaluation_packet_fingerprint: BlobId,
    rubric_fingerprint: BlobId,
    template_policy: &CriticChatTemplatePolicy,
) -> BlobId {
    let mut digest = CanonicalDigest::new(PROMPT_SPEC_DOMAIN);
    match task {
        CriticEvaluationTask::Criterion { criterion_key } => {
            digest.u32(1);
            digest.str(criterion_key);
        }
        CriticEvaluationTask::BlindPair => digest.u32(2),
    }
    digest.bytes(&evaluation_attempt_id.as_ulid().to_bytes());
    digest.blob(evaluation_packet_fingerprint);
    digest.blob(rubric_fingerprint);
    digest.u64(candidates.len() as u64);
    for candidate in candidates {
        digest.bytes(&candidate.occurrence_id.as_ulid().to_bytes());
        digest.blob(candidate.blob_id);
        digest.u64(candidate.range.start());
        digest.u64(candidate.range.end());
    }
    digest.u64(messages.len() as u64);
    for message in messages {
        digest.u32(match message.role {
            ControllerMessageRole::System => 1,
            ControllerMessageRole::User => 2,
            ControllerMessageRole::Assistant => 3,
            ControllerMessageRole::Tool => 4,
        });
        digest.str(&message.content);
        digest.u64(message.sources.len() as u64);
        for source in &message.sources {
            digest.u32(u32::from(source.candidate_index));
            digest.u64(source.message_range.start());
            digest.u64(source.message_range.end());
        }
    }
    match template_policy {
        CriticChatTemplatePolicy::ModelDefault => digest.u32(1),
        CriticChatTemplatePolicy::ExactOverride(template) => {
            digest.u32(2);
            digest.str(template);
        }
    }
    digest.finish_blob()
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ControllerPromptError {
    #[error("controller message count is outside the bounded range: {0}")]
    InvalidMessageCount(usize),
    #[error("controller message byte length is outside the bounded range: {0}")]
    InvalidMessageLength(usize),
    #[error("controller messages cannot contain NUL")]
    ProhibitedMessageNul,
    #[error("controller candidate count is outside the bounded range: {0}")]
    InvalidCandidateCount(usize),
    #[error("controller prompt exceeds the aggregate byte bound")]
    PromptTooLarge,
    #[error("controller source-binding count is outside the bounded range: {0}")]
    InvalidSourceBindingCount(usize),
    #[error("controller candidate bytes, blob, or range are invalid")]
    InvalidCandidateSource,
    #[error("controller message source references an unknown candidate")]
    UnknownCandidateIndex,
    #[error("controller message source range is invalid")]
    InvalidMessageSourceRange,
    #[error("controller message source bytes differ from exact candidate bytes")]
    SourceBytesMismatch,
    #[error("every candidate must have at least one exact message source binding")]
    UnreferencedCandidate,
    #[error("criterion key is empty, oversized, or contains a control character")]
    InvalidCriterionKey,
    #[error("chat template byte length is outside the bounded range: {0}")]
    InvalidTemplateLength(usize),
    #[error("native controller token evidence is empty or malformed")]
    InvalidTokenEvidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(occurrence_id: ArtifactId, bytes: &[u8]) -> ControllerCandidateSource<'_> {
        ControllerCandidateSource {
            occurrence_id,
            blob_id: BlobId::digest(bytes),
            utf8: bytes,
            range: NonEmptyByteRange::new(0, bytes.len() as u64).expect("range"),
        }
    }

    #[test]
    fn prompt_compilation_binds_message_bytes_candidate_order_and_packets() {
        let first = b"First exact candidate.";
        let second = b"Second exact candidate.";
        let content = format!(
            "Candidate A:\n{}\n\nCandidate B:\n{}",
            std::str::from_utf8(first).expect("first"),
            std::str::from_utf8(second).expect("second")
        );
        let first_start = "Candidate A:\n".len() as u64;
        let second_start =
            ("Candidate A:\n".len() + first.len() + "\n\nCandidate B:\n".len()) as u64;
        let message = ControllerMessage::new(
            ControllerMessageRole::User,
            content,
            vec![
                ControllerMessageSource::new(
                    0,
                    NonEmptyByteRange::new(first_start, first_start + first.len() as u64)
                        .expect("first message range"),
                ),
                ControllerMessageSource::new(
                    1,
                    NonEmptyByteRange::new(second_start, second_start + second.len() as u64)
                        .expect("second message range"),
                ),
            ],
        )
        .expect("message");
        let packet = BlobId::digest(b"blind packet");
        let rubric = BlobId::digest(b"rubric");
        let spec = CriticPromptSpec::compile(
            CriticEvaluationTask::BlindPair,
            ArtifactId::new(),
            vec![message],
            &[
                source(ArtifactId::new(), first),
                source(ArtifactId::new(), second),
            ],
            packet,
            rubric,
            CriticChatTemplatePolicy::ModelDefault,
        )
        .expect("source-bound prompt");
        assert_eq!(spec.candidates().len(), 2);
        assert_eq!(spec.evaluation_packet_fingerprint(), packet);
        assert_eq!(spec.rubric_fingerprint(), rubric);
        assert!(!format!("{spec:?}").contains("First exact candidate"));
    }

    #[test]
    fn prompt_rejects_candidate_swap_even_when_metadata_is_repeated() {
        let exact = b"Exact candidate.";
        let other = b"Other candidate.";
        let message = ControllerMessage::new(
            ControllerMessageRole::User,
            std::str::from_utf8(exact).expect("text"),
            vec![ControllerMessageSource::new(
                0,
                NonEmptyByteRange::new(0, exact.len() as u64).expect("range"),
            )],
        )
        .expect("message");
        assert_eq!(
            CriticPromptSpec::compile(
                CriticEvaluationTask::criterion("continuity").expect("task"),
                ArtifactId::new(),
                vec![message],
                &[source(ArtifactId::new(), other)],
                BlobId::digest(b"packet"),
                BlobId::digest(b"rubric"),
                CriticChatTemplatePolicy::ModelDefault,
            )
            .map(|_| ()),
            Err(ControllerPromptError::SourceBytesMismatch)
        );
    }

    #[test]
    fn template_debug_redacts_exact_override_body() {
        let sentinel = "SENSITIVE TEMPLATE BODY";
        let policy = CriticChatTemplatePolicy::exact_override(sentinel).expect("template");
        let debug = format!("{policy:?}");
        assert!(!debug.contains(sentinel));
        assert!(debug.contains("byte_len"));
    }

    #[test]
    fn fresh_evaluation_attempts_cannot_share_a_prompt_fingerprint() {
        let candidate = b"Exact candidate.";
        let candidate_occurrence = ArtifactId::new();
        let make = |attempt| {
            CriticPromptSpec::compile(
                CriticEvaluationTask::criterion("continuity").expect("task"),
                attempt,
                vec![
                    ControllerMessage::new(
                        ControllerMessageRole::User,
                        "Exact candidate.",
                        vec![ControllerMessageSource::new(
                            0,
                            NonEmptyByteRange::new(0, candidate.len() as u64).expect("range"),
                        )],
                    )
                    .expect("message"),
                ],
                &[source(candidate_occurrence, candidate)],
                BlobId::digest(b"packet"),
                BlobId::digest(b"rubric"),
                CriticChatTemplatePolicy::ModelDefault,
            )
            .expect("prompt")
            .fingerprint()
        };
        assert_ne!(make(ArtifactId::new()), make(ArtifactId::new()));
    }
}
