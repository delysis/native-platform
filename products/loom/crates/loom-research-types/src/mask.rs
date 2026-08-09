//! Typed, source-bound prompt-mask contracts.

use loom_types::{BlobId, RevisionId};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    BoundError, MAX_SOURCE_BYTES, NonEmptyBoundedVec, NonEmptyByteRange, PromptSourceRange,
    RangeError, TypedPlaceholder,
};

pub const MAX_SURFACE_MASK_SPANS: usize = 1_024;
pub const MAX_FIM_SPECIAL_TOKEN_IDS: usize = 16;

const SURFACE_MASK_FINGERPRINT_DOMAIN: &[u8] = b"loom/surface-prompt-mask/v1\0";
const FIM_PLAN_FINGERPRINT_DOMAIN: &[u8] = b"loom/model-specific-fim-mask/v1\0";
const FIM_RECEIPT_FINGERPRINT_DOMAIN: &[u8] = b"loom/fim-capability-receipt/v1\0";
const FIM_BINDING_FINGERPRINT_DOMAIN: &[u8] = b"loom/capability-bound-fim/v1\0";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceMaskKind {
    Entity,
    Beat,
    State,
    ContentStyle,
    Suffix,
}

impl SurfaceMaskKind {
    const fn domain_tag(self) -> u8 {
        match self {
            Self::Entity => 0,
            Self::Beat => 1,
            Self::State => 2,
            Self::ContentStyle => 3,
            Self::Suffix => 4,
        }
    }

    const fn marker(self) -> &'static [u8] {
        match self {
            Self::Entity => b"<loom-mask:entity>",
            Self::Beat => b"<loom-mask:beat>",
            Self::State => b"<loom-mask:state>",
            Self::ContentStyle => b"<loom-mask:content-style>",
            Self::Suffix => b"<loom-mask:suffix>",
        }
    }
}

/// A visible, deterministic replacement. No caller-provided prose can be
/// inserted through this contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum SurfaceMaskReplacement {
    Omit,
    KindMarker,
    Placeholder { placeholder: TypedPlaceholder },
}

impl SurfaceMaskReplacement {
    fn update_digest(self, digest: &mut Sha256) {
        match self {
            Self::Omit => digest.update([0]),
            Self::KindMarker => digest.update([1]),
            Self::Placeholder { placeholder } => {
                digest.update([2]);
                digest.update([match placeholder.kind() {
                    crate::PlaceholderKind::Role => 0,
                    crate::PlaceholderKind::Object => 1,
                }]);
                digest.update(placeholder.ordinal().to_be_bytes());
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SurfaceMaskSpan {
    range: NonEmptyByteRange,
    replacement: SurfaceMaskReplacement,
}

impl SurfaceMaskSpan {
    pub const fn new(range: NonEmptyByteRange, replacement: SurfaceMaskReplacement) -> Self {
        Self { range, replacement }
    }

    pub const fn range(self) -> NonEmptyByteRange {
        self.range
    }

    pub const fn replacement(self) -> SurfaceMaskReplacement {
        self.replacement
    }

    fn update_digest(self, digest: &mut Sha256) {
        digest.update(self.range.start().to_be_bytes());
        digest.update(self.range.end().to_be_bytes());
        self.replacement.update_digest(digest);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceMaskSpanWire {
    range: NonEmptyByteRange,
    replacement: SurfaceMaskReplacement,
}

impl<'de> Deserialize<'de> for SurfaceMaskSpan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SurfaceMaskSpanWire::deserialize(deserializer)?;
        Ok(Self::new(wire.range, wire.replacement))
    }
}

/// A content-addressed source transformation recipe.
///
/// Applying it produces explicit visible bytes. It has no API that can append
/// FIM control tokens or silently enter `CompiledBaseCompletionPrompt`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SurfacePromptMaskPlan {
    source: PromptSourceRange,
    kind: SurfaceMaskKind,
    spans: NonEmptyBoundedVec<SurfaceMaskSpan, MAX_SURFACE_MASK_SPANS>,
    fingerprint: BlobId,
}

impl SurfacePromptMaskPlan {
    pub fn new(
        source: PromptSourceRange,
        kind: SurfaceMaskKind,
        spans: Vec<SurfaceMaskSpan>,
    ) -> Result<Self, PromptMaskError> {
        let spans = NonEmptyBoundedVec::new(spans)?;
        validate_surface_spans(source, kind, &spans)?;
        let mut plan = Self {
            source,
            kind,
            spans,
            fingerprint: BlobId::digest(&[]),
        };
        plan.fingerprint = plan.compute_fingerprint();
        Ok(plan)
    }

    pub const fn source(&self) -> PromptSourceRange {
        self.source
    }

    pub const fn kind(&self) -> SurfaceMaskKind {
        self.kind
    }

    pub fn spans(&self) -> &[SurfaceMaskSpan] {
        &self.spans
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn apply(
        self,
        expected_revision_id: RevisionId,
        exact_source_bytes: &[u8],
    ) -> Result<AppliedSurfacePromptMask, PromptMaskError> {
        verify_mask_source(self.source, expected_revision_id, exact_source_bytes)?;
        let source_slice = self
            .source
            .range()
            .checked_str(exact_source_bytes)?
            .as_bytes();
        let source_start = usize::try_from(self.source.range().start())
            .map_err(|_| PromptMaskError::RenderedOutputTooLarge)?;
        let mut rendered = Vec::with_capacity(source_slice.len());
        let mut cursor = 0_usize;
        for span in self.spans.iter().copied() {
            let start = usize::try_from(span.range().start())
                .map_err(|_| PromptMaskError::RenderedOutputTooLarge)?
                - source_start;
            let end = usize::try_from(span.range().end())
                .map_err(|_| PromptMaskError::RenderedOutputTooLarge)?
                - source_start;
            extend_mask_output(&mut rendered, &source_slice[cursor..start])?;
            match span.replacement() {
                SurfaceMaskReplacement::Omit => {}
                SurfaceMaskReplacement::KindMarker => {
                    extend_mask_output(&mut rendered, self.kind.marker())?;
                }
                SurfaceMaskReplacement::Placeholder { placeholder } => {
                    let kind = match placeholder.kind() {
                        crate::PlaceholderKind::Role => "role",
                        crate::PlaceholderKind::Object => "object",
                    };
                    let marker = format!("<loom-{kind}:{}>", placeholder.ordinal());
                    extend_mask_output(&mut rendered, marker.as_bytes())?;
                }
            }
            cursor = end;
        }
        extend_mask_output(&mut rendered, &source_slice[cursor..])?;
        if rendered.is_empty() {
            return Err(PromptMaskError::EmptyRenderedOutput);
        }
        let rendered_blob_id = BlobId::digest(&rendered);
        let fingerprint = fingerprint_applied_surface(self.fingerprint, rendered_blob_id);
        Ok(AppliedSurfacePromptMask {
            plan: self,
            rendered,
            rendered_blob_id,
            fingerprint,
        })
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(SURFACE_MASK_FINGERPRINT_DOMAIN);
        update_source_digest(self.source, &mut digest);
        digest.update([self.kind.domain_tag()]);
        digest.update((self.spans.len() as u64).to_be_bytes());
        for span in self.spans.iter().copied() {
            span.update_digest(&mut digest);
        }
        BlobId::from_bytes(digest.finalize().into())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfacePromptMaskPlanWire {
    source: PromptSourceRange,
    kind: SurfaceMaskKind,
    spans: NonEmptyBoundedVec<SurfaceMaskSpan, MAX_SURFACE_MASK_SPANS>,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for SurfacePromptMaskPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SurfacePromptMaskPlanWire::deserialize(deserializer)?;
        let plan = Self::new(wire.source, wire.kind, wire.spans.into_inner())
            .map_err(serde::de::Error::custom)?;
        if plan.fingerprint != wire.fingerprint {
            return Err(serde::de::Error::custom(
                PromptMaskError::PlanFingerprintMismatch,
            ));
        }
        Ok(plan)
    }
}

/// Exact rendered bytes from a source-bound surface mask.
///
/// This is move-only and non-deserializable. A prompt compiler may use the
/// bytes only as an explicit preceding block with a transformation witness.
pub struct AppliedSurfacePromptMask {
    plan: SurfacePromptMaskPlan,
    rendered: Vec<u8>,
    rendered_blob_id: BlobId,
    fingerprint: BlobId,
}

impl AppliedSurfacePromptMask {
    pub const fn plan(&self) -> &SurfacePromptMaskPlan {
        &self.plan
    }

    pub fn rendered_bytes(&self) -> &[u8] {
        &self.rendered
    }

    pub fn rendered_text(&self) -> &str {
        std::str::from_utf8(&self.rendered).expect("surface mask preserves UTF-8")
    }

    pub const fn rendered_blob_id(&self) -> BlobId {
        self.rendered_blob_id
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn into_parts(self) -> (SurfacePromptMaskPlan, Vec<u8>, BlobId, BlobId) {
        (
            self.plan,
            self.rendered,
            self.rendered_blob_id,
            self.fingerprint,
        )
    }
}

impl std::fmt::Debug for AppliedSurfacePromptMask {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppliedSurfacePromptMask")
            .field("plan_fingerprint", &self.plan.fingerprint())
            .field("rendered_len", &self.rendered.len())
            .field("rendered_blob_id", &self.rendered_blob_id)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// A model-specific FIM treatment. It remains separate from raw completion
/// prompt compilation and contains no hidden rendered control bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelSpecificFimMaskPlan {
    source: PromptSourceRange,
    missing_range: NonEmptyByteRange,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    capability_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl ModelSpecificFimMaskPlan {
    pub fn new(
        source: PromptSourceRange,
        missing_range: NonEmptyByteRange,
        model_fingerprint: BlobId,
        tokenizer_fingerprint: BlobId,
        capability_fingerprint: BlobId,
    ) -> Result<Self, PromptMaskError> {
        if missing_range.start() < source.range().start()
            || missing_range.end() > source.range().end()
        {
            return Err(PromptMaskError::FimRangeOutsideSource);
        }
        let mut plan = Self {
            source,
            missing_range,
            model_fingerprint,
            tokenizer_fingerprint,
            capability_fingerprint,
            fingerprint: BlobId::digest(&[]),
        };
        plan.fingerprint = plan.compute_fingerprint();
        Ok(plan)
    }

    pub const fn source(&self) -> PromptSourceRange {
        self.source
    }

    pub const fn missing_range(&self) -> NonEmptyByteRange {
        self.missing_range
    }

    pub const fn model_fingerprint(&self) -> BlobId {
        self.model_fingerprint
    }

    pub const fn tokenizer_fingerprint(&self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn capability_fingerprint(&self) -> BlobId {
        self.capability_fingerprint
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    /// Revalidates the exact immutable source and the missing UTF-8 range.
    /// This still does not validate the backend capability receipt.
    pub fn verify_source(
        &self,
        expected_revision_id: RevisionId,
        exact_source_bytes: &[u8],
    ) -> Result<(), PromptMaskError> {
        verify_mask_source(self.source, expected_revision_id, exact_source_bytes)?;
        let _ = self.missing_range.checked_str(exact_source_bytes)?;
        Ok(())
    }

    /// Structurally binds an exact capability receipt. The backend verifier is
    /// responsible for establishing the receipt's authenticity before calling
    /// this method.
    pub fn bind_capability(
        self,
        receipt: FimCapabilityReceipt,
    ) -> Result<CapabilityBoundFimMask, PromptMaskError> {
        if self.model_fingerprint != receipt.model_fingerprint
            || self.tokenizer_fingerprint != receipt.tokenizer_fingerprint
            || self.capability_fingerprint != receipt.capability_fingerprint
        {
            return Err(PromptMaskError::FimCapabilityMismatch);
        }
        let fingerprint = fingerprint_bound_fim(self.fingerprint, receipt.fingerprint);
        Ok(CapabilityBoundFimMask {
            plan: self,
            receipt,
            fingerprint,
        })
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(FIM_PLAN_FINGERPRINT_DOMAIN);
        update_source_digest(self.source, &mut digest);
        digest.update(self.missing_range.start().to_be_bytes());
        digest.update(self.missing_range.end().to_be_bytes());
        digest.update(self.model_fingerprint.as_bytes());
        digest.update(self.tokenizer_fingerprint.as_bytes());
        digest.update(self.capability_fingerprint.as_bytes());
        BlobId::from_bytes(digest.finalize().into())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelSpecificFimMaskPlanWire {
    source: PromptSourceRange,
    missing_range: NonEmptyByteRange,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    model_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    tokenizer_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    capability_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for ModelSpecificFimMaskPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelSpecificFimMaskPlanWire::deserialize(deserializer)?;
        let plan = Self::new(
            wire.source,
            wire.missing_range,
            wire.model_fingerprint,
            wire.tokenizer_fingerprint,
            wire.capability_fingerprint,
        )
        .map_err(serde::de::Error::custom)?;
        if plan.fingerprint != wire.fingerprint {
            return Err(serde::de::Error::custom(
                PromptMaskError::FimPlanFingerprintMismatch,
            ));
        }
        Ok(plan)
    }
}

/// Persisted exact FIM token/capability evidence. This record is a claim until
/// a backend receipt verifier validates `backend_receipt_blob_id`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FimCapabilityReceipt {
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    capability_fingerprint: BlobId,
    prefix_token_ids: NonEmptyBoundedVec<u32, MAX_FIM_SPECIAL_TOKEN_IDS>,
    suffix_token_ids: NonEmptyBoundedVec<u32, MAX_FIM_SPECIAL_TOKEN_IDS>,
    middle_token_ids: NonEmptyBoundedVec<u32, MAX_FIM_SPECIAL_TOKEN_IDS>,
    backend_receipt_blob_id: BlobId,
    fingerprint: BlobId,
}

impl FimCapabilityReceipt {
    pub fn new(
        model_fingerprint: BlobId,
        tokenizer_fingerprint: BlobId,
        capability_fingerprint: BlobId,
        prefix_token_ids: Vec<u32>,
        suffix_token_ids: Vec<u32>,
        middle_token_ids: Vec<u32>,
        backend_receipt_blob_id: BlobId,
    ) -> Result<Self, PromptMaskError> {
        let prefix_token_ids = NonEmptyBoundedVec::new(prefix_token_ids)?;
        let suffix_token_ids = NonEmptyBoundedVec::new(suffix_token_ids)?;
        let middle_token_ids = NonEmptyBoundedVec::new(middle_token_ids)?;
        let mut receipt = Self {
            model_fingerprint,
            tokenizer_fingerprint,
            capability_fingerprint,
            prefix_token_ids,
            suffix_token_ids,
            middle_token_ids,
            backend_receipt_blob_id,
            fingerprint: BlobId::digest(&[]),
        };
        receipt.fingerprint = receipt.compute_fingerprint();
        Ok(receipt)
    }

    pub const fn model_fingerprint(&self) -> BlobId {
        self.model_fingerprint
    }

    pub const fn tokenizer_fingerprint(&self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn capability_fingerprint(&self) -> BlobId {
        self.capability_fingerprint
    }

    pub fn prefix_token_ids(&self) -> &[u32] {
        &self.prefix_token_ids
    }

    pub fn suffix_token_ids(&self) -> &[u32] {
        &self.suffix_token_ids
    }

    pub fn middle_token_ids(&self) -> &[u32] {
        &self.middle_token_ids
    }

    pub const fn backend_receipt_blob_id(&self) -> BlobId {
        self.backend_receipt_blob_id
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(FIM_RECEIPT_FINGERPRINT_DOMAIN);
        digest.update(self.model_fingerprint.as_bytes());
        digest.update(self.tokenizer_fingerprint.as_bytes());
        digest.update(self.capability_fingerprint.as_bytes());
        update_token_ids(&self.prefix_token_ids, &mut digest);
        update_token_ids(&self.suffix_token_ids, &mut digest);
        update_token_ids(&self.middle_token_ids, &mut digest);
        digest.update(self.backend_receipt_blob_id.as_bytes());
        BlobId::from_bytes(digest.finalize().into())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FimCapabilityReceiptWire {
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    model_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    tokenizer_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    capability_fingerprint: BlobId,
    prefix_token_ids: NonEmptyBoundedVec<u32, MAX_FIM_SPECIAL_TOKEN_IDS>,
    suffix_token_ids: NonEmptyBoundedVec<u32, MAX_FIM_SPECIAL_TOKEN_IDS>,
    middle_token_ids: NonEmptyBoundedVec<u32, MAX_FIM_SPECIAL_TOKEN_IDS>,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    backend_receipt_blob_id: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for FimCapabilityReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FimCapabilityReceiptWire::deserialize(deserializer)?;
        let receipt = Self::new(
            wire.model_fingerprint,
            wire.tokenizer_fingerprint,
            wire.capability_fingerprint,
            wire.prefix_token_ids.into_inner(),
            wire.suffix_token_ids.into_inner(),
            wire.middle_token_ids.into_inner(),
            wire.backend_receipt_blob_id,
        )
        .map_err(serde::de::Error::custom)?;
        if receipt.fingerprint != wire.fingerprint {
            return Err(serde::de::Error::custom(
                PromptMaskError::FimReceiptFingerprintMismatch,
            ));
        }
        Ok(receipt)
    }
}

/// A move-only exact plan/receipt binding. It intentionally has no conversion
/// into `CompiledBaseCompletionPrompt`; FIM execution needs a dedicated native
/// capability path.
///
/// ```compile_fail
/// use loom_research_types::{CapabilityBoundFimMask, CompiledBaseCompletionPrompt};
/// fn raw_base_prompt(_: CompiledBaseCompletionPrompt) {}
/// fn cannot_smuggle_fim(fim: CapabilityBoundFimMask) { raw_base_prompt(fim); }
/// ```
pub struct CapabilityBoundFimMask {
    plan: ModelSpecificFimMaskPlan,
    receipt: FimCapabilityReceipt,
    fingerprint: BlobId,
}

impl CapabilityBoundFimMask {
    pub const fn plan(&self) -> &ModelSpecificFimMaskPlan {
        &self.plan
    }

    pub const fn receipt(&self) -> &FimCapabilityReceipt {
        &self.receipt
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn verify_source(
        &self,
        expected_revision_id: RevisionId,
        exact_source_bytes: &[u8],
    ) -> Result<(), PromptMaskError> {
        self.plan
            .verify_source(expected_revision_id, exact_source_bytes)
    }

    pub fn into_parts(self) -> (ModelSpecificFimMaskPlan, FimCapabilityReceipt, BlobId) {
        (self.plan, self.receipt, self.fingerprint)
    }
}

impl std::fmt::Debug for CapabilityBoundFimMask {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapabilityBoundFimMask")
            .field("plan_fingerprint", &self.plan.fingerprint())
            .field("receipt_fingerprint", &self.receipt.fingerprint())
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PromptMaskError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error(transparent)]
    Range(#[from] RangeError),
    #[error("mask span lies outside the declared source range")]
    MaskOutsideSource,
    #[error("mask spans must be strictly ordered and nonoverlapping")]
    MaskSpansOverlapOrUnordered,
    #[error("entity masks require typed placeholder replacements")]
    EntityMaskRequiresPlaceholder,
    #[error("only entity masks may use typed placeholder replacements")]
    PlaceholderRequiresEntityMask,
    #[error("suffix masks require exactly one omitted span ending at the source range end")]
    InvalidSuffixMask,
    #[error("source revision is {actual}; expected {expected}")]
    SourceRevisionMismatch {
        expected: RevisionId,
        actual: RevisionId,
    },
    #[error("source hashes to {actual}; expected {expected}")]
    SourceBlobMismatch { expected: BlobId, actual: BlobId },
    #[error("rendered mask output exceeds the maximum source size")]
    RenderedOutputTooLarge,
    #[error("rendered mask output is empty")]
    EmptyRenderedOutput,
    #[error("surface mask plan fingerprint mismatch")]
    PlanFingerprintMismatch,
    #[error("FIM missing range lies outside the declared source range")]
    FimRangeOutsideSource,
    #[error("FIM mask plan fingerprint mismatch")]
    FimPlanFingerprintMismatch,
    #[error("FIM capability receipt fingerprint mismatch")]
    FimReceiptFingerprintMismatch,
    #[error("FIM model, tokenizer, or capability fingerprint mismatch")]
    FimCapabilityMismatch,
}

fn validate_surface_spans(
    source: PromptSourceRange,
    kind: SurfaceMaskKind,
    spans: &[SurfaceMaskSpan],
) -> Result<(), PromptMaskError> {
    let mut previous_end = None;
    for span in spans.iter().copied() {
        if span.range().start() < source.range().start()
            || span.range().end() > source.range().end()
        {
            return Err(PromptMaskError::MaskOutsideSource);
        }
        if previous_end.is_some_and(|end| span.range().start() < end) {
            return Err(PromptMaskError::MaskSpansOverlapOrUnordered);
        }
        previous_end = Some(span.range().end());
        match (kind, span.replacement()) {
            (SurfaceMaskKind::Entity, SurfaceMaskReplacement::Placeholder { .. }) => {}
            (SurfaceMaskKind::Entity, _) => {
                return Err(PromptMaskError::EntityMaskRequiresPlaceholder);
            }
            (_, SurfaceMaskReplacement::Placeholder { .. }) => {
                return Err(PromptMaskError::PlaceholderRequiresEntityMask);
            }
            _ => {}
        }
    }
    if kind == SurfaceMaskKind::Suffix
        && (spans.len() != 1
            || spans[0].replacement() != SurfaceMaskReplacement::Omit
            || spans[0].range().end() != source.range().end())
    {
        return Err(PromptMaskError::InvalidSuffixMask);
    }
    Ok(())
}

fn verify_mask_source(
    source: PromptSourceRange,
    expected_revision_id: RevisionId,
    exact_source_bytes: &[u8],
) -> Result<(), PromptMaskError> {
    if source.revision_id() != expected_revision_id {
        return Err(PromptMaskError::SourceRevisionMismatch {
            expected: expected_revision_id,
            actual: source.revision_id(),
        });
    }
    crate::range::validate_source_utf8(exact_source_bytes)?;
    let actual = BlobId::digest(exact_source_bytes);
    if actual != source.source_blob_id() {
        return Err(PromptMaskError::SourceBlobMismatch {
            expected: source.source_blob_id(),
            actual,
        });
    }
    let _ = source.range().checked_str(exact_source_bytes)?;
    Ok(())
}

fn extend_mask_output(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), PromptMaskError> {
    let final_len = output
        .len()
        .checked_add(bytes.len())
        .ok_or(PromptMaskError::RenderedOutputTooLarge)?;
    if final_len > MAX_SOURCE_BYTES {
        return Err(PromptMaskError::RenderedOutputTooLarge);
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn update_source_digest(source: PromptSourceRange, digest: &mut Sha256) {
    digest.update(source.revision_id().as_ulid().to_bytes());
    digest.update(source.source_blob_id().as_bytes());
    digest.update(source.range().start().to_be_bytes());
    digest.update(source.range().end().to_be_bytes());
}

fn update_token_ids(token_ids: &[u32], digest: &mut Sha256) {
    digest.update((token_ids.len() as u64).to_be_bytes());
    for token_id in token_ids {
        digest.update(token_id.to_be_bytes());
    }
}

fn fingerprint_applied_surface(plan: BlobId, rendered: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/applied-surface-prompt-mask/v1\0");
    digest.update(plan.as_bytes());
    digest.update(rendered.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_bound_fim(plan: BlobId, receipt: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(FIM_BINDING_FINGERPRINT_DOMAIN);
    digest.update(plan.as_bytes());
    digest.update(receipt.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}
