use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use loom_types::{BlobId, ProjectId, RevisionId};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    BoundError, BoundedVec, CallScope, CandidateAssemblyId, NonEmptyBoundedVec, NonEmptyByteRange,
    RangeError,
};

/// The tail is a part, so a prompt may contain at most 63 preceding blocks.
pub const MAX_COMPLETION_PROMPT_PARTS: usize = 64;
pub const MAX_COMPLETION_PROMPT_BLOCKS: usize = MAX_COMPLETION_PROMPT_PARTS - 1;
pub const MAX_COMPLETION_PROMPT_BLOCK_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_COMPLETION_PROMPT_BYTES: usize = 16 * 1024 * 1024;
/// JSON can expand control-heavy UTF-8 prompt blocks by up to six bytes per
/// input byte; leave bounded room for witnesses and structural metadata.
pub const MAX_FROZEN_PROMPT_SPECIFICATION_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_PROMPT_SOURCE_BINDINGS: usize = 128;
pub const MAX_PROMPT_TRANSFORMATION_SOURCES: usize = 16;

const PROMPT_FINGERPRINT_DOMAIN: &[u8] = b"loom/base-completion-prompt/v1\0";
const PROMPT_CONTENT_FINGERPRINT_DOMAIN: &[u8] = b"loom/base-completion-prompt-content/v1\0";

/// The complete and closed set of roles allowed before a raw base-model tail.
///
/// This enum deliberately has no system, assistant, chat-template, FIM prefix,
/// FIM suffix, or hidden-control role. Bytes are concatenated exactly as
/// declared; this contract never inserts separators or control tokens.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionPromptBlockRole {
    Bookfront,
    OperatorDemonstration,
    ProjectAnchor,
    StoryState,
    MovementContract,
    SourceApprenticeship,
}

impl CompletionPromptBlockRole {
    pub const ALL: [Self; 6] = [
        Self::Bookfront,
        Self::OperatorDemonstration,
        Self::ProjectAnchor,
        Self::StoryState,
        Self::MovementContract,
        Self::SourceApprenticeship,
    ];

    const fn domain_tag(self) -> u8 {
        match self {
            Self::Bookfront => 0,
            Self::OperatorDemonstration => 1,
            Self::ProjectAnchor => 2,
            Self::StoryState => 3,
            Self::MovementContract => 4,
            Self::SourceApprenticeship => 5,
        }
    }
}

/// Exact, nonempty UTF-8 bytes for one preceding prompt block.
#[derive(Clone, Eq, PartialEq)]
pub struct ExactPromptBlockBytes(Vec<u8>);

impl ExactPromptBlockBytes {
    pub fn new(bytes: Vec<u8>) -> Result<Self, PromptCompileError> {
        if bytes.is_empty() {
            return Err(PromptCompileError::EmptyBlock);
        }
        if bytes.len() > MAX_COMPLETION_PROMPT_BLOCK_BYTES {
            return Err(PromptCompileError::BlockTooLarge {
                actual: bytes.len(),
                maximum: MAX_COMPLETION_PROMPT_BLOCK_BYTES,
            });
        }
        let _ = std::str::from_utf8(&bytes).map_err(|_| PromptCompileError::InvalidBlockUtf8)?;
        Ok(Self(bytes))
    }

    pub fn from_text(text: impl Into<String>) -> Result<Self, PromptCompileError> {
        Self::new(text.into().into_bytes())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        // Construction and deserialization validate UTF-8.
        std::str::from_utf8(&self.0).expect("ExactPromptBlockBytes invariant")
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

impl fmt::Debug for ExactPromptBlockBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactPromptBlockBytes")
            .field("len", &self.0.len())
            .field("sha256", &BlobId::digest(&self.0))
            .finish()
    }
}

impl Serialize for ExactPromptBlockBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ExactPromptBlockBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value.into_bytes()).map_err(de::Error::custom)
    }
}

/// An exact nonempty range in one immutable source revision.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct PromptSourceRange {
    revision_id: RevisionId,
    source_blob_id: BlobId,
    range: NonEmptyByteRange,
}

impl PromptSourceRange {
    pub fn new(
        revision_id: RevisionId,
        source_blob_id: BlobId,
        range: NonEmptyByteRange,
    ) -> Result<Self, PromptCompileError> {
        if range.end() > crate::MAX_SOURCE_BYTES as u64 {
            return Err(PromptCompileError::SourceRangeTooLarge {
                end: range.end(),
                maximum: crate::MAX_SOURCE_BYTES,
            });
        }
        Ok(Self {
            revision_id,
            source_blob_id,
            range,
        })
    }

    pub const fn revision_id(self) -> RevisionId {
        self.revision_id
    }

    pub const fn source_blob_id(self) -> BlobId {
        self.source_blob_id
    }

    pub const fn range(self) -> NonEmptyByteRange {
        self.range
    }

    fn update_digest(self, digest: &mut Sha256) {
        digest.update(self.revision_id.as_ulid().to_bytes());
        digest.update(self.source_blob_id.as_bytes());
        digest.update(self.range.start().to_be_bytes());
        digest.update(self.range.end().to_be_bytes());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptSourceRangeWire {
    #[serde(deserialize_with = "crate::bounded::deserialize_revision_id")]
    revision_id: RevisionId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    source_blob_id: BlobId,
    range: NonEmptyByteRange,
}

impl<'de> Deserialize<'de> for PromptSourceRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PromptSourceRangeWire::deserialize(deserializer)?;
        Self::new(wire.revision_id, wire.source_blob_id, wire.range).map_err(de::Error::custom)
    }
}

/// Exactly one source or transformation witness for a preceding block.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PromptBlockWitness(PromptBlockWitnessKind);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
enum PromptBlockWitnessKind {
    ExactSource {
        source: PromptSourceRange,
    },
    Transformation {
        sources: NonEmptyBoundedVec<PromptSourceRange, MAX_PROMPT_TRANSFORMATION_SOURCES>,
        recipe_fingerprint: BlobId,
        receipt_fingerprint: BlobId,
        rendered_bytes_fingerprint: BlobId,
    },
}

impl PromptBlockWitness {
    pub const fn exact_source(source: PromptSourceRange) -> Self {
        Self(PromptBlockWitnessKind::ExactSource { source })
    }

    pub fn transformation(
        sources: Vec<PromptSourceRange>,
        recipe_fingerprint: BlobId,
        receipt_fingerprint: BlobId,
        rendered_bytes_fingerprint: BlobId,
    ) -> Result<Self, PromptCompileError> {
        let sources = NonEmptyBoundedVec::new(sources)?;
        validate_unique_source_ranges(&sources)?;
        Ok(Self(PromptBlockWitnessKind::Transformation {
            sources,
            recipe_fingerprint,
            receipt_fingerprint,
            rendered_bytes_fingerprint,
        }))
    }

    pub fn sources(&self) -> &[PromptSourceRange] {
        match &self.0 {
            PromptBlockWitnessKind::ExactSource { source } => std::slice::from_ref(source),
            PromptBlockWitnessKind::Transformation { sources, .. } => sources,
        }
    }

    pub const fn is_transformation(&self) -> bool {
        matches!(self.0, PromptBlockWitnessKind::Transformation { .. })
    }

    pub const fn recipe_fingerprint(&self) -> Option<BlobId> {
        match self.0 {
            PromptBlockWitnessKind::ExactSource { .. } => None,
            PromptBlockWitnessKind::Transformation {
                recipe_fingerprint, ..
            } => Some(recipe_fingerprint),
        }
    }

    pub const fn receipt_fingerprint(&self) -> Option<BlobId> {
        match self.0 {
            PromptBlockWitnessKind::ExactSource { .. } => None,
            PromptBlockWitnessKind::Transformation {
                receipt_fingerprint,
                ..
            } => Some(receipt_fingerprint),
        }
    }

    pub const fn rendered_bytes_fingerprint(&self) -> Option<BlobId> {
        match self.0 {
            PromptBlockWitnessKind::ExactSource { .. } => None,
            PromptBlockWitnessKind::Transformation {
                rendered_bytes_fingerprint,
                ..
            } => Some(rendered_bytes_fingerprint),
        }
    }

    fn validate_for_block(&self, bytes: &ExactPromptBlockBytes) -> Result<(), PromptCompileError> {
        if let PromptBlockWitnessKind::Transformation {
            rendered_bytes_fingerprint,
            ..
        } = self.0
            && BlobId::digest(bytes.as_bytes()) != rendered_bytes_fingerprint
        {
            return Err(PromptCompileError::RenderedBytesFingerprintMismatch);
        }
        Ok(())
    }

    fn update_digest(&self, digest: &mut Sha256) {
        match &self.0 {
            PromptBlockWitnessKind::ExactSource { source } => {
                digest.update([0]);
                source.update_digest(digest);
            }
            PromptBlockWitnessKind::Transformation {
                sources,
                recipe_fingerprint,
                receipt_fingerprint,
                rendered_bytes_fingerprint,
            } => {
                digest.update([1]);
                digest.update((sources.len() as u64).to_be_bytes());
                for source in sources.iter().copied() {
                    source.update_digest(digest);
                }
                digest.update(recipe_fingerprint.as_bytes());
                digest.update(receipt_fingerprint.as_bytes());
                digest.update(rendered_bytes_fingerprint.as_bytes());
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
enum PromptBlockWitnessWire {
    ExactSource {
        source: PromptSourceRange,
    },
    Transformation {
        sources: NonEmptyBoundedVec<PromptSourceRange, MAX_PROMPT_TRANSFORMATION_SOURCES>,
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        recipe_fingerprint: BlobId,
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        receipt_fingerprint: BlobId,
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        rendered_bytes_fingerprint: BlobId,
    },
}

impl<'de> Deserialize<'de> for PromptBlockWitness {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match PromptBlockWitnessWire::deserialize(deserializer)? {
            PromptBlockWitnessWire::ExactSource { source } => Ok(Self::exact_source(source)),
            PromptBlockWitnessWire::Transformation {
                sources,
                recipe_fingerprint,
                receipt_fingerprint,
                rendered_bytes_fingerprint,
            } => Self::transformation(
                sources.into_inner(),
                recipe_fingerprint,
                receipt_fingerprint,
                rendered_bytes_fingerprint,
            )
            .map_err(de::Error::custom),
        }
    }
}

/// One exact preceding block in semantic order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrozenCompletionPromptBlock {
    role: CompletionPromptBlockRole,
    bytes: ExactPromptBlockBytes,
    witness: PromptBlockWitness,
}

impl FrozenCompletionPromptBlock {
    pub fn new(
        role: CompletionPromptBlockRole,
        bytes: ExactPromptBlockBytes,
        witness: PromptBlockWitness,
    ) -> Result<Self, PromptCompileError> {
        witness.validate_for_block(&bytes)?;
        Ok(Self {
            role,
            bytes,
            witness,
        })
    }

    pub const fn role(&self) -> CompletionPromptBlockRole {
        self.role
    }

    pub const fn bytes(&self) -> &ExactPromptBlockBytes {
        &self.bytes
    }

    pub const fn witness(&self) -> &PromptBlockWitness {
        &self.witness
    }

    fn update_digest(&self, digest: &mut Sha256) {
        digest.update([self.role.domain_tag()]);
        digest.update((self.bytes.len() as u64).to_be_bytes());
        digest.update(self.bytes.as_bytes());
        self.witness.update_digest(digest);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenCompletionPromptBlockWire {
    role: CompletionPromptBlockRole,
    bytes: ExactPromptBlockBytes,
    witness: PromptBlockWitness,
}

impl<'de> Deserialize<'de> for FrozenCompletionPromptBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FrozenCompletionPromptBlockWire::deserialize(deserializer)?;
        Self::new(wire.role, wire.bytes, wire.witness).map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompletionTailOrigin {
    LiveManuscript,
    AdmittedAssembly { assembly_id: CandidateAssemblyId },
}

/// The exact live ending which must be the final bytes of the compiled prompt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum CompletionPromptTail {
    LiveManuscript {
        source_revision_id: RevisionId,
        source_blob_id: BlobId,
        range: NonEmptyByteRange,
    },
    AdmittedAssembly {
        assembly_id: CandidateAssemblyId,
        assembled_blob_id: BlobId,
        range: NonEmptyByteRange,
    },
}

impl CompletionPromptTail {
    pub fn live_manuscript(
        source_revision_id: RevisionId,
        source_blob_id: BlobId,
        range: NonEmptyByteRange,
    ) -> Result<Self, PromptCompileError> {
        validate_tail_range(range)?;
        Ok(Self::LiveManuscript {
            source_revision_id,
            source_blob_id,
            range,
        })
    }

    pub fn admitted_assembly(
        assembly_id: CandidateAssemblyId,
        assembled_blob_id: BlobId,
        range: NonEmptyByteRange,
    ) -> Result<Self, PromptCompileError> {
        validate_tail_range(range)?;
        Ok(Self::AdmittedAssembly {
            assembly_id,
            assembled_blob_id,
            range,
        })
    }

    pub const fn origin(self) -> CompletionTailOrigin {
        match self {
            Self::LiveManuscript { .. } => CompletionTailOrigin::LiveManuscript,
            Self::AdmittedAssembly { assembly_id, .. } => {
                CompletionTailOrigin::AdmittedAssembly { assembly_id }
            }
        }
    }

    pub const fn source_revision_id(self) -> Option<RevisionId> {
        match self {
            Self::LiveManuscript {
                source_revision_id, ..
            } => Some(source_revision_id),
            Self::AdmittedAssembly { .. } => None,
        }
    }

    pub const fn assembly_id(self) -> Option<CandidateAssemblyId> {
        match self {
            Self::LiveManuscript { .. } => None,
            Self::AdmittedAssembly { assembly_id, .. } => Some(assembly_id),
        }
    }

    pub const fn source_blob_id(self) -> BlobId {
        match self {
            Self::LiveManuscript { source_blob_id, .. } => source_blob_id,
            Self::AdmittedAssembly {
                assembled_blob_id, ..
            } => assembled_blob_id,
        }
    }

    pub const fn range(self) -> NonEmptyByteRange {
        match self {
            Self::LiveManuscript { range, .. } | Self::AdmittedAssembly { range, .. } => range,
        }
    }

    fn update_digest(self, digest: &mut Sha256) {
        match self {
            Self::LiveManuscript {
                source_revision_id,
                source_blob_id,
                range,
            } => {
                digest.update([0]);
                digest.update(source_revision_id.as_ulid().to_bytes());
                digest.update(source_blob_id.as_bytes());
                digest.update(range.start().to_be_bytes());
                digest.update(range.end().to_be_bytes());
            }
            Self::AdmittedAssembly {
                assembly_id,
                assembled_blob_id,
                range,
            } => {
                digest.update([1]);
                digest.update(assembly_id.as_ulid().to_bytes());
                digest.update(assembled_blob_id.as_bytes());
                digest.update(range.start().to_be_bytes());
                digest.update(range.end().to_be_bytes());
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
enum CompletionPromptTailWire {
    LiveManuscript {
        #[serde(deserialize_with = "crate::bounded::deserialize_revision_id")]
        source_revision_id: RevisionId,
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        source_blob_id: BlobId,
        range: NonEmptyByteRange,
    },
    AdmittedAssembly {
        assembly_id: CandidateAssemblyId,
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        assembled_blob_id: BlobId,
        range: NonEmptyByteRange,
    },
}

impl<'de> Deserialize<'de> for CompletionPromptTail {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match CompletionPromptTailWire::deserialize(deserializer)? {
            CompletionPromptTailWire::LiveManuscript {
                source_revision_id,
                source_blob_id,
                range,
            } => Self::live_manuscript(source_revision_id, source_blob_id, range),
            CompletionPromptTailWire::AdmittedAssembly {
                assembly_id,
                assembled_blob_id,
                range,
            } => Self::admitted_assembly(assembly_id, assembled_blob_id, range),
        }
        .map_err(de::Error::custom)
    }
}

fn validate_tail_range(range: NonEmptyByteRange) -> Result<(), PromptCompileError> {
    if range.end() > crate::MAX_SOURCE_BYTES as u64 {
        return Err(PromptCompileError::SourceRangeTooLarge {
            end: range.end(),
            maximum: crate::MAX_SOURCE_BYTES,
        });
    }
    Ok(())
}

/// A serializable frozen specification for one raw base-completion prompt.
///
/// Compilation consumes this value, exact assembled prompt bytes, and every
/// referenced immutable source. No chat template or FIM transformation exists
/// on this path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FrozenBaseCompletionPrompt {
    project_id: ProjectId,
    scope: CallScope,
    treatment_recipe_fingerprint: BlobId,
    preceding_blocks: BoundedVec<FrozenCompletionPromptBlock, MAX_COMPLETION_PROMPT_BLOCKS>,
    tail: CompletionPromptTail,
}

impl FrozenBaseCompletionPrompt {
    pub fn new(
        project_id: ProjectId,
        scope: CallScope,
        treatment_recipe_fingerprint: BlobId,
        preceding_blocks: Vec<FrozenCompletionPromptBlock>,
        tail: CompletionPromptTail,
    ) -> Result<Self, PromptCompileError> {
        let preceding_blocks = BoundedVec::new(preceding_blocks)?;
        let total = preceding_blocks.iter().try_fold(0_usize, |total, block| {
            total
                .checked_add(block.bytes.len())
                .ok_or(PromptCompileError::PromptTooLarge {
                    actual: usize::MAX,
                    maximum: MAX_COMPLETION_PROMPT_BYTES,
                })
        })?;
        if total > MAX_COMPLETION_PROMPT_BYTES {
            return Err(PromptCompileError::PromptTooLarge {
                actual: total,
                maximum: MAX_COMPLETION_PROMPT_BYTES,
            });
        }
        Ok(Self {
            project_id,
            scope,
            treatment_recipe_fingerprint,
            preceding_blocks,
            tail,
        })
    }

    pub const fn project_id(&self) -> ProjectId {
        self.project_id
    }

    pub const fn scope(&self) -> CallScope {
        self.scope
    }

    pub const fn treatment_recipe_fingerprint(&self) -> BlobId {
        self.treatment_recipe_fingerprint
    }

    pub fn preceding_blocks(&self) -> &[FrozenCompletionPromptBlock] {
        &self.preceding_blocks
    }

    pub const fn tail(&self) -> CompletionPromptTail {
        self.tail
    }

    /// Verifies source-independent evidence preserved after the original
    /// source-bound compilation. This cannot create an inference-ready prompt:
    /// it only checks the frozen block bytes, exact EOF tail geometry, UTF-8,
    /// and the private canonical fingerprint against already compiled bytes.
    pub fn verify_compiled_evidence(
        &self,
        exact_bytes: &[u8],
        tail_prompt_range: NonEmptyByteRange,
        expected_fingerprint: BlobId,
    ) -> Result<(), PromptCompileError> {
        self.verify_compiled_geometry(exact_bytes, tail_prompt_range)?;
        if fingerprint_prompt(self, exact_bytes, tail_prompt_range) != expected_fingerprint {
            return Err(PromptCompileError::CompiledEvidenceFingerprintMismatch);
        }
        Ok(())
    }

    /// Verifies the exact retry-stable prompt content without accepting an
    /// attempt-bound prompt fingerprint as a substitute.
    ///
    /// Like [`Self::verify_compiled_evidence`], this is diagnostic replay and
    /// cannot create an inference-ready prompt or source-admission authority.
    pub fn verify_compiled_content_evidence(
        &self,
        exact_bytes: &[u8],
        tail_prompt_range: NonEmptyByteRange,
        expected_content_fingerprint: BlobId,
    ) -> Result<(), PromptCompileError> {
        self.verify_compiled_geometry(exact_bytes, tail_prompt_range)?;
        if fingerprint_prompt_content(self, exact_bytes, tail_prompt_range)
            != expected_content_fingerprint
        {
            return Err(PromptCompileError::CompiledEvidenceFingerprintMismatch);
        }
        Ok(())
    }

    fn verify_compiled_geometry(
        &self,
        exact_bytes: &[u8],
        tail_prompt_range: NonEmptyByteRange,
    ) -> Result<(), PromptCompileError> {
        if exact_bytes.is_empty() {
            return Err(PromptCompileError::EmptyPrompt);
        }
        if exact_bytes.len() > MAX_COMPLETION_PROMPT_BYTES {
            return Err(PromptCompileError::PromptTooLarge {
                actual: exact_bytes.len(),
                maximum: MAX_COMPLETION_PROMPT_BYTES,
            });
        }
        let _ =
            std::str::from_utf8(exact_bytes).map_err(|_| PromptCompileError::InvalidPromptUtf8)?;
        if tail_prompt_range.end() < exact_bytes.len() as u64 {
            let tail_end = usize::try_from(tail_prompt_range.end()).map_err(|_| {
                PromptCompileError::PromptTooLarge {
                    actual: usize::MAX,
                    maximum: MAX_COMPLETION_PROMPT_BYTES,
                }
            })?;
            return Err(PromptCompileError::ExtraSuffix {
                extra_bytes: exact_bytes.len() - tail_end,
            });
        }
        let tail_bytes = tail_prompt_range
            .as_range()
            .checked_slice(exact_bytes)
            .map_err(|error| prompt_tail_range_error(self.tail, error))?;
        let preceding_len = self
            .preceding_blocks
            .iter()
            .try_fold(0_usize, |total, block| total.checked_add(block.bytes.len()))
            .ok_or(PromptCompileError::PromptTooLarge {
                actual: usize::MAX,
                maximum: MAX_COMPLETION_PROMPT_BYTES,
            })?;
        if preceding_len as u64 != tail_prompt_range.start()
            || self.tail.range().end() - self.tail.range().start() != tail_bytes.len() as u64
        {
            return Err(PromptCompileError::PromptBytesMismatch);
        }
        let mut offset = 0_usize;
        for block in self.preceding_blocks.iter() {
            let end = offset + block.bytes.len();
            if exact_bytes.get(offset..end) != Some(block.bytes.as_bytes()) {
                return Err(PromptCompileError::PromptBytesMismatch);
            }
            offset = end;
        }
        Ok(())
    }

    pub fn compile(
        self,
        exact_prompt_bytes: Vec<u8>,
        exact_sources: &[ExactPromptSource<'_>],
    ) -> Result<CompiledBaseCompletionPrompt, PromptCompileError> {
        compile_base_completion_prompt(self, exact_prompt_bytes, exact_sources)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenBaseCompletionPromptWire {
    #[serde(deserialize_with = "crate::bounded::deserialize_project_id")]
    project_id: ProjectId,
    scope: CallScope,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    treatment_recipe_fingerprint: BlobId,
    preceding_blocks: BoundedVec<FrozenCompletionPromptBlock, MAX_COMPLETION_PROMPT_BLOCKS>,
    tail: CompletionPromptTail,
}

impl<'de> Deserialize<'de> for FrozenBaseCompletionPrompt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FrozenBaseCompletionPromptWire::deserialize(deserializer)?;
        Self::new(
            wire.project_id,
            wire.scope,
            wire.treatment_recipe_fingerprint,
            wire.preceding_blocks.into_inner(),
            wire.tail,
        )
        .map_err(de::Error::custom)
    }
}

/// Exact source bytes supplied only for compilation and never serialized into
/// the prompt specification.
#[derive(Clone, Copy, Debug)]
pub struct ExactPromptSource<'a> {
    key: PromptSourceKey,
    bytes: &'a [u8],
}

impl<'a> ExactPromptSource<'a> {
    pub const fn new(revision_id: RevisionId, bytes: &'a [u8]) -> Self {
        Self {
            key: PromptSourceKey::Revision(revision_id),
            bytes,
        }
    }

    pub const fn admitted_assembly(assembly_id: CandidateAssemblyId, bytes: &'a [u8]) -> Self {
        Self {
            key: PromptSourceKey::Assembly(assembly_id),
            bytes,
        }
    }

    pub const fn revision_id(self) -> Option<RevisionId> {
        match self.key {
            PromptSourceKey::Revision(revision_id) => Some(revision_id),
            PromptSourceKey::Assembly(_) => None,
        }
    }

    pub const fn assembly_id(self) -> Option<CandidateAssemblyId> {
        match self.key {
            PromptSourceKey::Revision(_) => None,
            PromptSourceKey::Assembly(assembly_id) => Some(assembly_id),
        }
    }

    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PromptSourceKey {
    Revision(RevisionId),
    Assembly(CandidateAssemblyId),
}

/// The sole inference-ready raw base-completion prompt value.
///
/// It is intentionally move-only and does not implement `Deserialize`.
///
/// ```compile_fail
/// use loom_research_types::CompiledBaseCompletionPrompt;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<CompiledBaseCompletionPrompt>();
/// ```
///
/// ```compile_fail
/// use loom_research_types::CompiledBaseCompletionPrompt;
/// fn needs_deserialize<T: serde::de::DeserializeOwned>() {}
/// needs_deserialize::<CompiledBaseCompletionPrompt>();
/// ```
pub struct CompiledBaseCompletionPrompt {
    specification: FrozenBaseCompletionPrompt,
    exact_bytes: Vec<u8>,
    tail_prompt_range: NonEmptyByteRange,
    content_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl fmt::Debug for CompiledBaseCompletionPrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompiledBaseCompletionPrompt")
            .field("project_id", &self.project_id())
            .field("scope", &self.scope())
            .field("exact_byte_len", &self.exact_bytes.len())
            .field("tail_prompt_range", &self.tail_prompt_range)
            .field("content_fingerprint", &self.content_fingerprint)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl CompiledBaseCompletionPrompt {
    pub const fn specification(&self) -> &FrozenBaseCompletionPrompt {
        &self.specification
    }

    pub const fn project_id(&self) -> ProjectId {
        self.specification.project_id
    }

    pub const fn scope(&self) -> CallScope {
        self.specification.scope
    }

    pub const fn treatment_recipe_fingerprint(&self) -> BlobId {
        self.specification.treatment_recipe_fingerprint
    }

    pub fn exact_bytes(&self) -> &[u8] {
        &self.exact_bytes
    }

    pub fn exact_text(&self) -> &str {
        // Compilation validates the complete prompt as UTF-8.
        std::str::from_utf8(&self.exact_bytes).expect("compiled prompt UTF-8 invariant")
    }

    pub const fn tail_prompt_range(&self) -> NonEmptyByteRange {
        self.tail_prompt_range
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    /// Exact prompt identity that excludes only the execution attempt ID.
    ///
    /// This lets an immutable trial retry a byte-identical prompt under a fresh
    /// attempt while retaining every project, campaign, stage, case,
    /// treatment, role, source-range, tail, and byte commitment.
    pub const fn content_fingerprint(&self) -> BlobId {
        self.content_fingerprint
    }

    /// Consumes the inference-ready value without discarding its frozen
    /// provenance. Inference must retain all five returned parts with its call.
    pub fn into_parts(
        self,
    ) -> (
        FrozenBaseCompletionPrompt,
        Vec<u8>,
        NonEmptyByteRange,
        BlobId,
        BlobId,
    ) {
        (
            self.specification,
            self.exact_bytes,
            self.tail_prompt_range,
            self.content_fingerprint,
            self.fingerprint,
        )
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PromptCompileError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error("preceding prompt block is empty")]
    EmptyBlock,
    #[error("preceding prompt block has {actual} bytes; maximum is {maximum}")]
    BlockTooLarge { actual: usize, maximum: usize },
    #[error("preceding prompt block is not valid UTF-8")]
    InvalidBlockUtf8,
    #[error("source range end {end} exceeds maximum source bytes {maximum}")]
    SourceRangeTooLarge { end: u64, maximum: usize },
    #[error("transformation witness repeats a source range")]
    DuplicateWitnessSource,
    #[error("transformation rendered-byte fingerprint does not match its exact block bytes")]
    RenderedBytesFingerprintMismatch,
    #[error("compiler received {actual} exact sources; maximum is {maximum}")]
    TooManySourceBindings { actual: usize, maximum: usize },
    #[error("compiler received source revision {0} more than once")]
    DuplicateSourceBinding(RevisionId),
    #[error("compiler received admitted assembly {0} more than once")]
    DuplicateAssemblySourceBinding(CandidateAssemblyId),
    #[error("compiler did not receive exact bytes for source revision {0}")]
    MissingSourceBinding(RevisionId),
    #[error("compiler did not receive exact bytes for admitted assembly {0}")]
    MissingAssemblySourceBinding(CandidateAssemblyId),
    #[error("compiler received an unreferenced source revision {0}")]
    UnexpectedSourceBinding(RevisionId),
    #[error("compiler received an unreferenced admitted assembly {0}")]
    UnexpectedAssemblySourceBinding(CandidateAssemblyId),
    #[error("source revision {revision_id} hashes to {actual}, expected {expected}")]
    SourceBlobMismatch {
        revision_id: RevisionId,
        expected: BlobId,
        actual: BlobId,
    },
    #[error("admitted assembly {assembly_id} hashes to {actual}, expected {expected}")]
    AssemblyBlobMismatch {
        assembly_id: CandidateAssemblyId,
        expected: BlobId,
        actual: BlobId,
    },
    #[error("source revision {revision_id} has an invalid UTF-8 range: {error}")]
    SourceRange {
        revision_id: RevisionId,
        error: RangeError,
    },
    #[error("admitted assembly {assembly_id} has an invalid UTF-8 range: {error}")]
    AssemblySourceRange {
        assembly_id: CandidateAssemblyId,
        error: RangeError,
    },
    #[error("exact-source block {block_index} bytes differ from its witnessed source range")]
    BlockSourceBytesMismatch { block_index: usize },
    #[error("tail range ends at {range_end}, but exact source ends at {source_end}")]
    TailNotAtSourceEnd { range_end: u64, source_end: u64 },
    #[error("compiled prompt is empty")]
    EmptyPrompt,
    #[error("compiled prompt has {actual} bytes; maximum is {maximum}")]
    PromptTooLarge { actual: usize, maximum: usize },
    #[error("assembled prompt bytes are not valid UTF-8")]
    InvalidPromptUtf8,
    #[error("assembled prompt bytes do not exactly reconstruct the declared blocks and tail")]
    PromptBytesMismatch,
    #[error("assembled prompt contains {extra_bytes} bytes after the exact live tail")]
    ExtraSuffix { extra_bytes: usize },
    #[error("compiled prompt evidence fingerprint does not match its frozen specification")]
    CompiledEvidenceFingerprintMismatch,
}

fn validate_unique_source_ranges(sources: &[PromptSourceRange]) -> Result<(), PromptCompileError> {
    let mut unique = BTreeSet::new();
    for source in sources.iter().copied() {
        let key = (
            source.revision_id,
            source.source_blob_id,
            source.range.start(),
            source.range.end(),
        );
        if !unique.insert(key) {
            return Err(PromptCompileError::DuplicateWitnessSource);
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ValidatedPromptSource<'a> {
    bytes: &'a [u8],
    blob_id: BlobId,
}

struct ValidatedPromptSources<'a> {
    by_key: BTreeMap<PromptSourceKey, ValidatedPromptSource<'a>>,
    used: BTreeSet<PromptSourceKey>,
}

impl<'a> ValidatedPromptSources<'a> {
    fn new(sources: &[ExactPromptSource<'a>]) -> Result<Self, PromptCompileError> {
        if sources.len() > MAX_PROMPT_SOURCE_BINDINGS {
            return Err(PromptCompileError::TooManySourceBindings {
                actual: sources.len(),
                maximum: MAX_PROMPT_SOURCE_BINDINGS,
            });
        }
        let mut by_key = BTreeMap::new();
        for source in sources.iter().copied() {
            crate::range::validate_source_utf8(source.bytes)
                .map_err(|error| prompt_source_range_error(source.key, error))?;
            let value = ValidatedPromptSource {
                bytes: source.bytes,
                blob_id: BlobId::digest(source.bytes),
            };
            if by_key.insert(source.key, value).is_some() {
                return Err(duplicate_prompt_source_error(source.key));
            }
        }
        Ok(Self {
            by_key,
            used: BTreeSet::new(),
        })
    }

    fn resolve_nonempty(
        &mut self,
        source: PromptSourceRange,
    ) -> Result<&'a str, PromptCompileError> {
        let key = PromptSourceKey::Revision(source.revision_id);
        let resolved = self.resolve(key, source.source_blob_id)?;
        source
            .range
            .checked_str(resolved.bytes)
            .map_err(|error| prompt_source_range_error(key, error))
    }

    fn resolve_tail(&mut self, tail: CompletionPromptTail) -> Result<&'a [u8], PromptCompileError> {
        let key = match tail {
            CompletionPromptTail::LiveManuscript {
                source_revision_id, ..
            } => PromptSourceKey::Revision(source_revision_id),
            CompletionPromptTail::AdmittedAssembly { assembly_id, .. } => {
                PromptSourceKey::Assembly(assembly_id)
            }
        };
        let resolved = self.resolve(key, tail.source_blob_id())?;
        let source_end = resolved.bytes.len() as u64;
        if tail.range().end() != source_end {
            return Err(PromptCompileError::TailNotAtSourceEnd {
                range_end: tail.range().end(),
                source_end,
            });
        }
        tail.range()
            .as_range()
            .checked_slice(resolved.bytes)
            .map_err(|error| prompt_source_range_error(key, error))
    }

    fn resolve(
        &mut self,
        key: PromptSourceKey,
        expected_blob_id: BlobId,
    ) -> Result<ValidatedPromptSource<'a>, PromptCompileError> {
        let resolved = self
            .by_key
            .get(&key)
            .copied()
            .ok_or_else(|| missing_prompt_source_error(key))?;
        if resolved.blob_id != expected_blob_id {
            return Err(prompt_source_blob_mismatch(
                key,
                expected_blob_id,
                resolved.blob_id,
            ));
        }
        self.used.insert(key);
        Ok(resolved)
    }

    fn reject_unused(&self) -> Result<(), PromptCompileError> {
        if let Some(key) = self.by_key.keys().find(|key| !self.used.contains(key)) {
            return Err(unexpected_prompt_source_error(*key));
        }
        Ok(())
    }
}

fn duplicate_prompt_source_error(key: PromptSourceKey) -> PromptCompileError {
    match key {
        PromptSourceKey::Revision(revision_id) => {
            PromptCompileError::DuplicateSourceBinding(revision_id)
        }
        PromptSourceKey::Assembly(assembly_id) => {
            PromptCompileError::DuplicateAssemblySourceBinding(assembly_id)
        }
    }
}

fn missing_prompt_source_error(key: PromptSourceKey) -> PromptCompileError {
    match key {
        PromptSourceKey::Revision(revision_id) => {
            PromptCompileError::MissingSourceBinding(revision_id)
        }
        PromptSourceKey::Assembly(assembly_id) => {
            PromptCompileError::MissingAssemblySourceBinding(assembly_id)
        }
    }
}

fn unexpected_prompt_source_error(key: PromptSourceKey) -> PromptCompileError {
    match key {
        PromptSourceKey::Revision(revision_id) => {
            PromptCompileError::UnexpectedSourceBinding(revision_id)
        }
        PromptSourceKey::Assembly(assembly_id) => {
            PromptCompileError::UnexpectedAssemblySourceBinding(assembly_id)
        }
    }
}

fn prompt_source_blob_mismatch(
    key: PromptSourceKey,
    expected: BlobId,
    actual: BlobId,
) -> PromptCompileError {
    match key {
        PromptSourceKey::Revision(revision_id) => PromptCompileError::SourceBlobMismatch {
            revision_id,
            expected,
            actual,
        },
        PromptSourceKey::Assembly(assembly_id) => PromptCompileError::AssemblyBlobMismatch {
            assembly_id,
            expected,
            actual,
        },
    }
}

fn prompt_source_range_error(key: PromptSourceKey, error: RangeError) -> PromptCompileError {
    match key {
        PromptSourceKey::Revision(revision_id) => {
            PromptCompileError::SourceRange { revision_id, error }
        }
        PromptSourceKey::Assembly(assembly_id) => {
            PromptCompileError::AssemblySourceRange { assembly_id, error }
        }
    }
}

fn prompt_tail_range_error(tail: CompletionPromptTail, error: RangeError) -> PromptCompileError {
    match tail {
        CompletionPromptTail::LiveManuscript {
            source_revision_id, ..
        } => PromptCompileError::SourceRange {
            revision_id: source_revision_id,
            error,
        },
        CompletionPromptTail::AdmittedAssembly { assembly_id, .. } => {
            PromptCompileError::AssemblySourceRange { assembly_id, error }
        }
    }
}

fn compile_base_completion_prompt(
    specification: FrozenBaseCompletionPrompt,
    exact_prompt_bytes: Vec<u8>,
    exact_sources: &[ExactPromptSource<'_>],
) -> Result<CompiledBaseCompletionPrompt, PromptCompileError> {
    let mut sources = ValidatedPromptSources::new(exact_sources)?;
    let mut expected_bytes = Vec::new();
    for (block_index, block) in specification.preceding_blocks.iter().enumerate() {
        match &block.witness.0 {
            PromptBlockWitnessKind::ExactSource { source } => {
                let source_text = sources.resolve_nonempty(*source)?;
                if source_text.as_bytes() != block.bytes.as_bytes() {
                    return Err(PromptCompileError::BlockSourceBytesMismatch { block_index });
                }
            }
            PromptBlockWitnessKind::Transformation {
                sources: inputs, ..
            } => {
                for input in inputs.iter().copied() {
                    let _ = sources.resolve_nonempty(input)?;
                }
            }
        }
        extend_bounded(&mut expected_bytes, block.bytes.as_bytes())?;
    }

    let tail_bytes = sources.resolve_tail(specification.tail)?;
    extend_bounded(&mut expected_bytes, tail_bytes)?;
    sources.reject_unused()?;

    if expected_bytes.is_empty() {
        return Err(PromptCompileError::EmptyPrompt);
    }
    if exact_prompt_bytes.len() > MAX_COMPLETION_PROMPT_BYTES {
        return Err(PromptCompileError::PromptTooLarge {
            actual: exact_prompt_bytes.len(),
            maximum: MAX_COMPLETION_PROMPT_BYTES,
        });
    }
    let _ = std::str::from_utf8(&exact_prompt_bytes)
        .map_err(|_| PromptCompileError::InvalidPromptUtf8)?;
    if exact_prompt_bytes != expected_bytes {
        if exact_prompt_bytes.starts_with(&expected_bytes) {
            return Err(PromptCompileError::ExtraSuffix {
                extra_bytes: exact_prompt_bytes.len() - expected_bytes.len(),
            });
        }
        return Err(PromptCompileError::PromptBytesMismatch);
    }

    let tail_start = exact_prompt_bytes.len() - tail_bytes.len();
    let tail_prompt_range =
        NonEmptyByteRange::new(tail_start as u64, exact_prompt_bytes.len() as u64)
            .expect("bounded nonempty compiled tail range");
    let content_fingerprint =
        fingerprint_prompt_content(&specification, &exact_prompt_bytes, tail_prompt_range);
    let fingerprint = fingerprint_prompt(&specification, &exact_prompt_bytes, tail_prompt_range);
    Ok(CompiledBaseCompletionPrompt {
        specification,
        exact_bytes: exact_prompt_bytes,
        tail_prompt_range,
        content_fingerprint,
        fingerprint,
    })
}

fn extend_bounded(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), PromptCompileError> {
    let actual =
        output
            .len()
            .checked_add(bytes.len())
            .ok_or(PromptCompileError::PromptTooLarge {
                actual: usize::MAX,
                maximum: MAX_COMPLETION_PROMPT_BYTES,
            })?;
    if actual > MAX_COMPLETION_PROMPT_BYTES {
        return Err(PromptCompileError::PromptTooLarge {
            actual,
            maximum: MAX_COMPLETION_PROMPT_BYTES,
        });
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn fingerprint_prompt(
    specification: &FrozenBaseCompletionPrompt,
    exact_prompt_bytes: &[u8],
    tail_prompt_range: NonEmptyByteRange,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(PROMPT_FINGERPRINT_DOMAIN);
    digest.update(specification.project_id.as_ulid().to_bytes());
    let scope = specification.scope;
    digest.update(scope.campaign_id().as_ulid().to_bytes());
    digest.update(scope.stage_id().as_ulid().to_bytes());
    digest.update(scope.attempt_id().as_ulid().to_bytes());
    digest.update(scope.case_id().as_ulid().to_bytes());
    digest.update(specification.treatment_recipe_fingerprint.as_bytes());
    digest.update((specification.preceding_blocks.len() as u64).to_be_bytes());
    for block in specification.preceding_blocks.iter() {
        block.update_digest(&mut digest);
    }
    specification.tail.update_digest(&mut digest);
    digest.update(tail_prompt_range.start().to_be_bytes());
    digest.update(tail_prompt_range.end().to_be_bytes());
    digest.update((exact_prompt_bytes.len() as u64).to_be_bytes());
    digest.update(exact_prompt_bytes);
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_prompt_content(
    specification: &FrozenBaseCompletionPrompt,
    exact_prompt_bytes: &[u8],
    tail_prompt_range: NonEmptyByteRange,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(PROMPT_CONTENT_FINGERPRINT_DOMAIN);
    digest.update(specification.project_id.as_ulid().to_bytes());
    let scope = specification.scope;
    digest.update(scope.campaign_id().as_ulid().to_bytes());
    digest.update(scope.stage_id().as_ulid().to_bytes());
    digest.update(scope.case_id().as_ulid().to_bytes());
    digest.update(specification.treatment_recipe_fingerprint.as_bytes());
    digest.update((specification.preceding_blocks.len() as u64).to_be_bytes());
    for block in specification.preceding_blocks.iter() {
        block.update_digest(&mut digest);
    }
    specification.tail.update_digest(&mut digest);
    digest.update(tail_prompt_range.start().to_be_bytes());
    digest.update(tail_prompt_range.end().to_be_bytes());
    digest.update((exact_prompt_bytes.len() as u64).to_be_bytes());
    digest.update(exact_prompt_bytes);
    BlobId::from_bytes(digest.finalize().into())
}
