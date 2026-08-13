use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use loom_types::BlobId;

use crate::{
    BoundedText, ByteRange, CampaignId, GeneratedSpanOccurrenceId, MAX_GENERATED_TOKENS,
    MAX_RAW_OUTPUT_BYTES, ModelCallId, NonEmptyByteRange, NonEmptyTokenRange, RangeError,
    StageAttemptId, StageId, TrialCaseId,
};

pub type TerminalMessage = BoundedText<1_024>;

/// Declared authorship/evidence class observed at the inference boundary.
///
/// A `Live*Claim` is persisted diagnostic evidence, never proof of admission.
/// Only `loom-inference` may consume the native backend's opaque generation
/// seal and mint a `VerifiedInferenceEnvelope`; no value in this crate can
/// upgrade these records.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CallEvidenceClass {
    LiveBaseWriterClaim,
    LiveInstructEditorClaim,
    LiveLocalCriticClaim,
    LiveCodexCriticClaim,
    Fixture,
    Mock,
    HistoricalReceipt,
}

impl CallEvidenceClass {
    pub const fn is_live_base_writer_claim(self) -> bool {
        matches!(self, Self::LiveBaseWriterClaim)
    }

    const fn domain_tag(self) -> u8 {
        match self {
            Self::LiveBaseWriterClaim => 0,
            Self::LiveInstructEditorClaim => 1,
            Self::LiveLocalCriticClaim => 2,
            Self::LiveCodexCriticClaim => 3,
            Self::Fixture => 4,
            Self::Mock => 5,
            Self::HistoricalReceipt => 6,
        }
    }

    const fn requires_live_receipt(self) -> bool {
        matches!(
            self,
            Self::LiveBaseWriterClaim
                | Self::LiveInstructEditorClaim
                | Self::LiveLocalCriticClaim
                | Self::LiveCodexCriticClaim
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[allow(clippy::struct_field_names)]
pub struct CallIdentity {
    scope: CallScope,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    prompt_fingerprint: BlobId,
    sampler_fingerprint: BlobId,
    control_program_fingerprint: BlobId,
    seed: u64,
}

impl CallIdentity {
    pub const fn new(
        scope: CallScope,
        model_fingerprint: BlobId,
        tokenizer_fingerprint: BlobId,
        prompt_fingerprint: BlobId,
        sampler_fingerprint: BlobId,
        control_program_fingerprint: BlobId,
        seed: u64,
    ) -> Self {
        Self {
            scope,
            model_fingerprint,
            tokenizer_fingerprint,
            prompt_fingerprint,
            sampler_fingerprint,
            control_program_fingerprint,
            seed,
        }
    }

    pub const fn scope(&self) -> CallScope {
        self.scope
    }

    pub const fn model_fingerprint(&self) -> BlobId {
        self.model_fingerprint
    }

    pub const fn tokenizer_fingerprint(&self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn prompt_fingerprint(&self) -> BlobId {
        self.prompt_fingerprint
    }

    pub const fn sampler_fingerprint(&self) -> BlobId {
        self.sampler_fingerprint
    }

    pub const fn control_program_fingerprint(&self) -> BlobId {
        self.control_program_fingerprint
    }

    pub const fn seed(&self) -> u64 {
        self.seed
    }

    fn update_digest(&self, digest: &mut Sha256) {
        self.scope.update_digest(digest);
        digest.update(self.model_fingerprint.as_bytes());
        digest.update(self.tokenizer_fingerprint.as_bytes());
        digest.update(self.prompt_fingerprint.as_bytes());
        digest.update(self.sampler_fingerprint.as_bytes());
        digest.update(self.control_program_fingerprint.as_bytes());
        digest.update(self.seed.to_be_bytes());
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
pub struct CallScope {
    campaign_id: CampaignId,
    stage_id: StageId,
    attempt_id: StageAttemptId,
    case_id: TrialCaseId,
}

impl CallScope {
    pub const fn new(
        campaign_id: CampaignId,
        stage_id: StageId,
        attempt_id: StageAttemptId,
        case_id: TrialCaseId,
    ) -> Self {
        Self {
            campaign_id,
            stage_id,
            attempt_id,
            case_id,
        }
    }

    pub const fn campaign_id(self) -> CampaignId {
        self.campaign_id
    }

    pub const fn stage_id(self) -> StageId {
        self.stage_id
    }

    pub const fn attempt_id(self) -> StageAttemptId {
        self.attempt_id
    }

    pub const fn case_id(self) -> TrialCaseId {
        self.case_id
    }

    fn update_digest(self, digest: &mut Sha256) {
        digest.update(self.campaign_id.as_ulid().to_bytes());
        digest.update(self.stage_id.as_ulid().to_bytes());
        digest.update(self.attempt_id.as_ulid().to_bytes());
        digest.update(self.case_id.as_ulid().to_bytes());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
struct CallIdentityWire {
    scope: CallScope,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    model_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    tokenizer_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    prompt_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    sampler_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    control_program_fingerprint: BlobId,
    seed: u64,
}

impl<'de> Deserialize<'de> for CallIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CallIdentityWire::deserialize(deserializer)?;
        Ok(Self::new(
            wire.scope,
            wire.model_fingerprint,
            wire.tokenizer_fingerprint,
            wire.prompt_fingerprint,
            wire.sampler_fingerprint,
            wire.control_program_fingerprint,
            wire.seed,
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TokenEvidence {
    token_count: u32,
    token_ids_fingerprint: BlobId,
}

impl TokenEvidence {
    pub fn from_exact(token_ids: &[u32]) -> Result<Self, CallError> {
        let token_count = u32::try_from(token_ids.len()).map_err(|_| CallError::TooManyTokens {
            actual: token_ids.len(),
            maximum: MAX_GENERATED_TOKENS,
        })?;
        if token_count > MAX_GENERATED_TOKENS {
            return Err(CallError::TooManyTokens {
                actual: token_ids.len(),
                maximum: MAX_GENERATED_TOKENS,
            });
        }
        Ok(Self {
            token_count,
            token_ids_fingerprint: fingerprint_token_ids(token_ids),
        })
    }

    pub const fn token_count(&self) -> u32 {
        self.token_count
    }

    pub const fn token_ids_fingerprint(&self) -> BlobId {
        self.token_ids_fingerprint
    }

    pub fn verify(&self, token_ids: &[u32]) -> Result<(), CallError> {
        if token_ids.len() != self.token_count as usize {
            return Err(CallError::TokenCountMismatch {
                expected: self.token_count,
                actual: token_ids.len(),
            });
        }
        if fingerprint_token_ids(token_ids) != self.token_ids_fingerprint {
            return Err(CallError::TokenFingerprintMismatch);
        }
        Ok(())
    }

    fn update_digest(&self, digest: &mut Sha256) {
        digest.update(self.token_count.to_be_bytes());
        digest.update(self.token_ids_fingerprint.as_bytes());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenEvidenceWire {
    token_count: u32,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    token_ids_fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for TokenEvidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TokenEvidenceWire::deserialize(deserializer)?;
        if wire.token_count > MAX_GENERATED_TOKENS {
            return Err(serde::de::Error::custom(CallError::TooManyTokens {
                actual: wire.token_count as usize,
                maximum: MAX_GENERATED_TOKENS,
            }));
        }
        Ok(Self {
            token_count: wire.token_count,
            token_ids_fingerprint: wire.token_ids_fingerprint,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompletedCall {
    raw_output_blob_id: BlobId,
    raw_output_byte_len: u64,
    token_evidence: TokenEvidence,
    raw_event_stream_blob_id: BlobId,
    backend_receipt_blob_id: Option<BlobId>,
}

impl CompletedCall {
    pub fn new(
        raw_output: &[u8],
        token_ids: &[u32],
        raw_event_stream_blob_id: BlobId,
        backend_receipt_blob_id: Option<BlobId>,
    ) -> Result<Self, CallError> {
        if raw_output.len() as u64 > MAX_RAW_OUTPUT_BYTES {
            return Err(CallError::RawOutputTooLarge {
                actual: raw_output.len(),
                maximum: MAX_RAW_OUTPUT_BYTES,
            });
        }
        Ok(Self {
            raw_output_blob_id: BlobId::digest(raw_output),
            raw_output_byte_len: raw_output.len() as u64,
            token_evidence: TokenEvidence::from_exact(token_ids)?,
            raw_event_stream_blob_id,
            backend_receipt_blob_id,
        })
    }

    pub const fn raw_output_blob_id(&self) -> BlobId {
        self.raw_output_blob_id
    }

    pub const fn raw_output_byte_len(&self) -> u64 {
        self.raw_output_byte_len
    }

    pub const fn token_evidence(&self) -> &TokenEvidence {
        &self.token_evidence
    }

    pub const fn raw_event_stream_blob_id(&self) -> BlobId {
        self.raw_event_stream_blob_id
    }

    pub const fn backend_receipt_blob_id(&self) -> Option<BlobId> {
        self.backend_receipt_blob_id
    }

    pub(crate) fn verify_exact(
        &self,
        raw_output: &[u8],
        token_ids: &[u32],
    ) -> Result<(), CallError> {
        if raw_output.len() as u64 != self.raw_output_byte_len {
            return Err(CallError::RawOutputLengthMismatch {
                expected: self.raw_output_byte_len,
                actual: raw_output.len(),
            });
        }
        if BlobId::digest(raw_output) != self.raw_output_blob_id {
            return Err(CallError::RawOutputFingerprintMismatch);
        }
        self.token_evidence.verify(token_ids)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletedCallWire {
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    raw_output_blob_id: BlobId,
    raw_output_byte_len: u64,
    token_evidence: TokenEvidence,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    raw_event_stream_blob_id: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_optional_blob_id")]
    backend_receipt_blob_id: Option<BlobId>,
}

impl<'de> Deserialize<'de> for CompletedCall {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CompletedCallWire::deserialize(deserializer)?;
        if wire.raw_output_byte_len > MAX_RAW_OUTPUT_BYTES {
            return Err(serde::de::Error::custom(CallError::RawOutputTooLarge {
                actual: usize::try_from(wire.raw_output_byte_len).unwrap_or(usize::MAX),
                maximum: MAX_RAW_OUTPUT_BYTES,
            }));
        }
        Ok(Self {
            raw_output_blob_id: wire.raw_output_blob_id,
            raw_output_byte_len: wire.raw_output_byte_len,
            token_evidence: wire.token_evidence,
            raw_event_stream_blob_id: wire.raw_event_stream_blob_id,
            backend_receipt_blob_id: wire.backend_receipt_blob_id,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "status",
    content = "detail",
    rename_all = "snake_case"
)]
pub enum CallTerminal {
    Completed(CompletedCall),
    Failed { message: TerminalMessage },
    Cancelled { message: TerminalMessage },
    Rejected { message: TerminalMessage },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelCall {
    id: ModelCallId,
    identity: CallIdentity,
    evidence_class: CallEvidenceClass,
    terminal: CallTerminal,
}

impl ModelCall {
    pub fn new(
        id: ModelCallId,
        identity: CallIdentity,
        evidence_class: CallEvidenceClass,
        terminal: CallTerminal,
    ) -> Result<Self, CallError> {
        if let CallTerminal::Completed(completed) = &terminal
            && evidence_class.requires_live_receipt()
            && completed.backend_receipt_blob_id.is_none()
        {
            return Err(CallError::MissingLiveBackendReceipt);
        }
        Ok(Self {
            id,
            identity,
            evidence_class,
            terminal,
        })
    }

    pub const fn id(&self) -> ModelCallId {
        self.id
    }

    pub const fn identity(&self) -> &CallIdentity {
        &self.identity
    }

    pub const fn evidence_class(&self) -> CallEvidenceClass {
        self.evidence_class
    }

    pub const fn terminal(&self) -> &CallTerminal {
        &self.terminal
    }

    pub fn completed(&self) -> Result<&CompletedCall, CallError> {
        match &self.terminal {
            CallTerminal::Completed(completed) => Ok(completed),
            CallTerminal::Failed { .. }
            | CallTerminal::Cancelled { .. }
            | CallTerminal::Rejected { .. } => Err(CallError::CallDidNotComplete),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelCallWire {
    id: ModelCallId,
    identity: CallIdentity,
    evidence_class: CallEvidenceClass,
    terminal: CallTerminal,
}

impl<'de> Deserialize<'de> for ModelCall {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelCallWire::deserialize(deserializer)?;
        Self::new(wire.id, wire.identity, wire.evidence_class, wire.terminal)
            .map_err(serde::de::Error::custom)
    }
}

/// Exact, contiguous partition of one raw completion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OutputProjection {
    raw_output_byte_len: u64,
    displayed: NonEmptyByteRange,
    endpoint_excluded_tail: ByteRange,
    trimmed_stop_suffix: ByteRange,
}

impl OutputProjection {
    pub fn new(
        raw_output: &[u8],
        displayed_end: u64,
        endpoint_tail_end: u64,
    ) -> Result<Self, CallError> {
        let _ = crate::range::validate_raw_utf8(raw_output)?;
        let projection = Self::from_ranges(
            raw_output.len() as u64,
            NonEmptyByteRange::new(0, displayed_end)?,
            ByteRange::new(displayed_end, endpoint_tail_end)?,
            ByteRange::new(endpoint_tail_end, raw_output.len() as u64)?,
        )?;
        projection.verify_raw_bytes(raw_output)?;
        Ok(projection)
    }

    fn from_ranges(
        raw_output_byte_len: u64,
        displayed: NonEmptyByteRange,
        endpoint_excluded_tail: ByteRange,
        trimmed_stop_suffix: ByteRange,
    ) -> Result<Self, CallError> {
        if raw_output_byte_len > MAX_RAW_OUTPUT_BYTES {
            return Err(CallError::RawOutputTooLarge {
                actual: usize::try_from(raw_output_byte_len).unwrap_or(usize::MAX),
                maximum: MAX_RAW_OUTPUT_BYTES,
            });
        }
        if displayed.start() != 0
            || displayed.end() != endpoint_excluded_tail.start()
            || endpoint_excluded_tail.end() != trimmed_stop_suffix.start()
            || trimmed_stop_suffix.end() != raw_output_byte_len
        {
            return Err(CallError::InvalidOutputPartition);
        }
        Ok(Self {
            raw_output_byte_len,
            displayed,
            endpoint_excluded_tail,
            trimmed_stop_suffix,
        })
    }

    pub const fn raw_output_byte_len(&self) -> u64 {
        self.raw_output_byte_len
    }

    pub const fn displayed(&self) -> NonEmptyByteRange {
        self.displayed
    }

    pub const fn endpoint_excluded_tail(&self) -> ByteRange {
        self.endpoint_excluded_tail
    }

    pub const fn trimmed_stop_suffix(&self) -> ByteRange {
        self.trimmed_stop_suffix
    }

    pub fn displayed_str<'a>(&self, raw_output: &'a [u8]) -> Result<&'a str, CallError> {
        self.verify_raw_bytes(raw_output)?;
        Ok(self.displayed.checked_str(raw_output)?)
    }

    pub fn verify_raw_bytes(&self, raw_output: &[u8]) -> Result<(), CallError> {
        let _ = crate::range::validate_raw_utf8(raw_output)?;
        if raw_output.len() as u64 != self.raw_output_byte_len {
            return Err(CallError::RawOutputLengthMismatch {
                expected: self.raw_output_byte_len,
                actual: raw_output.len(),
            });
        }
        let _ = self.displayed.checked_str(raw_output)?;
        let _ = self.endpoint_excluded_tail.checked_slice(raw_output)?;
        let _ = self.trimmed_stop_suffix.checked_slice(raw_output)?;
        Ok(())
    }

    fn update_digest(&self, digest: &mut Sha256) {
        for value in [
            self.raw_output_byte_len,
            self.displayed.start(),
            self.displayed.end(),
            self.endpoint_excluded_tail.start(),
            self.endpoint_excluded_tail.end(),
            self.trimmed_stop_suffix.start(),
            self.trimmed_stop_suffix.end(),
        ] {
            digest.update(value.to_be_bytes());
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputProjectionWire {
    raw_output_byte_len: u64,
    displayed: NonEmptyByteRange,
    endpoint_excluded_tail: ByteRange,
    trimmed_stop_suffix: ByteRange,
}

impl<'de> Deserialize<'de> for OutputProjection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = OutputProjectionWire::deserialize(deserializer)?;
        Self::from_ranges(
            wire.raw_output_byte_len,
            wire.displayed,
            wire.endpoint_excluded_tail,
            wire.trimmed_stop_suffix,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpanCallBinding {
    identity: CallIdentity,
    evidence_class: CallEvidenceClass,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    raw_output_blob_id: BlobId,
    raw_output_byte_len: u64,
    token_evidence: TokenEvidence,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    raw_event_stream_blob_id: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_optional_blob_id")]
    backend_receipt_blob_id: Option<BlobId>,
    #[serde(deserialize_with = "crate::bounded::deserialize_optional_blob_id")]
    token_boundaries_fingerprint: Option<BlobId>,
}

impl SpanCallBinding {
    fn from_call(call: &ModelCall) -> Result<Self, CallError> {
        let completed = call.completed()?;
        Ok(Self {
            identity: call.identity.clone(),
            evidence_class: call.evidence_class,
            raw_output_blob_id: completed.raw_output_blob_id,
            raw_output_byte_len: completed.raw_output_byte_len,
            token_evidence: completed.token_evidence.clone(),
            raw_event_stream_blob_id: completed.raw_event_stream_blob_id,
            backend_receipt_blob_id: completed.backend_receipt_blob_id,
            token_boundaries_fingerprint: None,
        })
    }

    fn update_digest(&self, digest: &mut Sha256) {
        self.identity.update_digest(digest);
        digest.update([self.evidence_class.domain_tag()]);
        digest.update(self.raw_output_blob_id.as_bytes());
        digest.update(self.raw_output_byte_len.to_be_bytes());
        self.token_evidence.update_digest(digest);
        digest.update(self.raw_event_stream_blob_id.as_bytes());
        update_optional_blob(digest, self.backend_receipt_blob_id);
        update_optional_blob(digest, self.token_boundaries_fingerprint);
    }

    fn validate_declared(&self) -> Result<(), CallError> {
        if self.raw_output_byte_len > MAX_RAW_OUTPUT_BYTES {
            return Err(CallError::RawOutputTooLarge {
                actual: usize::try_from(self.raw_output_byte_len).unwrap_or(usize::MAX),
                maximum: MAX_RAW_OUTPUT_BYTES,
            });
        }
        if self.evidence_class.requires_live_receipt() && self.backend_receipt_blob_id.is_none() {
            return Err(CallError::MissingLiveBackendReceipt);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
/// Deterministic integrity checksum for a declared extraction record. It is
/// not evidence that inference ran.
pub struct ExtractionReceipt(BlobId);

impl ExtractionReceipt {
    pub const fn fingerprint(self) -> BlobId {
        self.0
    }
}

impl<'de> Deserialize<'de> for ExtractionReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        crate::bounded::deserialize_blob_id(deserializer).map(Self)
    }
}

/// Serializable evidence record for one extraction occurrence.
///
/// This record can prove internal hash/range consistency, but never that the
/// claimed backend call actually ran. `VerifiedInferenceEnvelope` authority is
/// intentionally absent from this crate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GeneratedSpanOccurrenceRecord {
    id: GeneratedSpanOccurrenceId,
    call_id: ModelCallId,
    raw_output_blob_id: BlobId,
    projection: OutputProjection,
    token_range: Option<NonEmptyTokenRange>,
    call_binding: SpanCallBinding,
    extraction_receipt: ExtractionReceipt,
}

impl GeneratedSpanOccurrenceRecord {
    /// Constructs an internally consistent declared record without asserting
    /// token-to-byte alignment or live admission.
    pub fn from_declared_call(
        id: GeneratedSpanOccurrenceId,
        call: &ModelCall,
        raw_output: &[u8],
        token_ids: &[u32],
        projection: OutputProjection,
    ) -> Result<Self, CallError> {
        let completed = call.completed()?;
        completed.verify_exact(raw_output, token_ids)?;
        projection.verify_raw_bytes(raw_output)?;
        if projection.raw_output_byte_len != completed.raw_output_byte_len {
            return Err(CallError::ProjectionCallLengthMismatch);
        }
        Self::from_declared_call_with_token_mapping(
            id, call, raw_output, token_ids, projection, None,
        )
    }

    /// Constructs a declared token-mapping record. The boundary fingerprint is
    /// evidence for later native replay, not an admission credential.
    pub fn from_declared_call_with_token_mapping(
        id: GeneratedSpanOccurrenceId,
        call: &ModelCall,
        raw_output: &[u8],
        token_ids: &[u32],
        projection: OutputProjection,
        token_mapping: Option<DeclaredTokenMapping>,
    ) -> Result<Self, CallError> {
        let completed = call.completed()?;
        completed.verify_exact(raw_output, token_ids)?;
        projection.verify_raw_bytes(raw_output)?;
        if projection.raw_output_byte_len != completed.raw_output_byte_len {
            return Err(CallError::ProjectionCallLengthMismatch);
        }
        let token_range = token_mapping.map(|mapping| mapping.range);
        validate_token_range(token_range, completed.token_evidence.token_count)?;
        let mut call_binding = SpanCallBinding::from_call(call)?;
        call_binding.token_boundaries_fingerprint =
            token_mapping.map(|mapping| mapping.boundaries_fingerprint);
        call_binding.validate_declared()?;
        let receipt = compute_extraction_receipt(
            id,
            call.id,
            completed.raw_output_blob_id,
            &projection,
            token_range,
            &call_binding,
        );
        Ok(Self {
            id,
            call_id: call.id,
            raw_output_blob_id: completed.raw_output_blob_id,
            projection,
            token_range,
            call_binding,
            extraction_receipt: receipt,
        })
    }

    pub const fn id(&self) -> GeneratedSpanOccurrenceId {
        self.id
    }

    pub const fn call_id(&self) -> ModelCallId {
        self.call_id
    }

    pub const fn raw_output_blob_id(&self) -> BlobId {
        self.raw_output_blob_id
    }

    pub const fn output_byte_range(&self) -> NonEmptyByteRange {
        self.projection.displayed
    }

    pub const fn projection(&self) -> &OutputProjection {
        &self.projection
    }

    pub const fn token_range(&self) -> Option<NonEmptyTokenRange> {
        self.token_range
    }

    pub const fn token_boundaries_fingerprint_claim(&self) -> Option<BlobId> {
        self.call_binding.token_boundaries_fingerprint
    }

    pub const fn evidence_class(&self) -> CallEvidenceClass {
        self.call_binding.evidence_class
    }

    pub const fn extraction_receipt(&self) -> ExtractionReceipt {
        self.extraction_receipt
    }

    pub const fn has_live_base_writer_claim(&self) -> bool {
        self.call_binding.evidence_class.is_live_base_writer_claim()
    }

    pub fn verify_exact(&self, evidence: &ExactCallEvidence<'_>) -> Result<(), CallError> {
        if evidence.call.id != self.call_id {
            return Err(CallError::CallIdMismatch);
        }
        let completed = evidence.call.completed()?;
        completed.verify_exact(evidence.raw_output, evidence.token_ids)?;
        if completed.raw_output_blob_id != self.raw_output_blob_id {
            return Err(CallError::RawOutputFingerprintMismatch);
        }
        self.projection.verify_raw_bytes(evidence.raw_output)?;
        let mut expected_binding = SpanCallBinding::from_call(evidence.call)?;
        // Token alignment is a persisted claim for the verifier to replay.
        expected_binding.token_boundaries_fingerprint =
            self.call_binding.token_boundaries_fingerprint;
        if expected_binding != self.call_binding {
            return Err(CallError::CallBindingMismatch);
        }
        validate_declared_token_mapping(
            self.token_range,
            self.call_binding.token_boundaries_fingerprint,
            completed.token_evidence.token_count,
        )?;
        let expected_receipt = compute_extraction_receipt(
            self.id,
            self.call_id,
            self.raw_output_blob_id,
            &self.projection,
            self.token_range,
            &self.call_binding,
        );
        if expected_receipt != self.extraction_receipt {
            return Err(CallError::ExtractionReceiptMismatch);
        }
        Ok(())
    }

    pub fn displayed_str<'a>(
        &self,
        evidence: &'a ExactCallEvidence<'a>,
    ) -> Result<&'a str, CallError> {
        self.verify_exact(evidence)?;
        self.projection.displayed_str(evidence.raw_output)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratedSpanOccurrenceRecordWire {
    id: GeneratedSpanOccurrenceId,
    call_id: ModelCallId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    raw_output_blob_id: BlobId,
    projection: OutputProjection,
    token_range: Option<NonEmptyTokenRange>,
    call_binding: SpanCallBinding,
    extraction_receipt: ExtractionReceipt,
}

impl<'de> Deserialize<'de> for GeneratedSpanOccurrenceRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = GeneratedSpanOccurrenceRecordWire::deserialize(deserializer)?;
        wire.call_binding
            .validate_declared()
            .map_err(serde::de::Error::custom)?;
        if wire.raw_output_blob_id != wire.call_binding.raw_output_blob_id {
            return Err(serde::de::Error::custom(
                CallError::RawOutputFingerprintMismatch,
            ));
        }
        if wire.projection.raw_output_byte_len != wire.call_binding.raw_output_byte_len {
            return Err(serde::de::Error::custom(
                CallError::ProjectionCallLengthMismatch,
            ));
        }
        validate_declared_token_mapping(
            wire.token_range,
            wire.call_binding.token_boundaries_fingerprint,
            wire.call_binding.token_evidence.token_count,
        )
        .map_err(serde::de::Error::custom)?;
        let expected = compute_extraction_receipt(
            wire.id,
            wire.call_id,
            wire.raw_output_blob_id,
            &wire.projection,
            wire.token_range,
            &wire.call_binding,
        );
        if expected != wire.extraction_receipt {
            return Err(serde::de::Error::custom(
                CallError::ExtractionReceiptMismatch,
            ));
        }
        Ok(Self {
            id: wire.id,
            call_id: wire.call_id,
            raw_output_blob_id: wire.raw_output_blob_id,
            projection: wire.projection,
            token_range: wire.token_range,
            call_binding: wire.call_binding,
            extraction_receipt: wire.extraction_receipt,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeclaredTokenMapping {
    range: NonEmptyTokenRange,
    boundaries_fingerprint: BlobId,
}

impl DeclaredTokenMapping {
    pub const fn new(
        range: NonEmptyTokenRange,
        boundaries_fingerprint: BlobId,
    ) -> Result<Self, CallError> {
        if range.start() != 0 {
            return Err(CallError::TokenMappingMustBeginAtOutputStart);
        }
        Ok(Self {
            range,
            boundaries_fingerprint,
        })
    }

    pub const fn range(self) -> NonEmptyTokenRange {
        self.range
    }

    pub const fn boundaries_fingerprint(self) -> BlobId {
        self.boundaries_fingerprint
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExactCallEvidence<'a> {
    call: &'a ModelCall,
    raw_output: &'a [u8],
    token_ids: &'a [u32],
}

impl<'a> ExactCallEvidence<'a> {
    pub const fn new(call: &'a ModelCall, raw_output: &'a [u8], token_ids: &'a [u32]) -> Self {
        Self {
            call,
            raw_output,
            token_ids,
        }
    }

    pub const fn call(&self) -> &ModelCall {
        self.call
    }

    pub const fn raw_output(&self) -> &[u8] {
        self.raw_output
    }

    pub const fn token_ids(&self) -> &[u32] {
        self.token_ids
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CallError {
    #[error(transparent)]
    Range(#[from] RangeError),
    #[error("raw output has {actual} bytes; maximum is {maximum}")]
    RawOutputTooLarge { actual: usize, maximum: u64 },
    #[error("completion has {actual} token IDs; maximum is {maximum}")]
    TooManyTokens { actual: usize, maximum: u32 },
    #[error("token count mismatch: expected {expected}, received {actual}")]
    TokenCountMismatch { expected: u32, actual: usize },
    #[error("token IDs do not match the recorded fingerprint")]
    TokenFingerprintMismatch,
    #[error("raw output length mismatch: expected {expected}, received {actual}")]
    RawOutputLengthMismatch { expected: u64, actual: usize },
    #[error("raw output bytes do not match the completed call")]
    RawOutputFingerprintMismatch,
    #[error("a declared live call is missing its backend receipt claim")]
    MissingLiveBackendReceipt,
    #[error("model call did not complete")]
    CallDidNotComplete,
    #[error("output projection is not an exact contiguous raw-output partition")]
    InvalidOutputPartition,
    #[error("output projection length does not match its model call")]
    ProjectionCallLengthMismatch,
    #[error("token range ends at {end}, beyond token count {token_count}")]
    TokenRangeOutOfBounds { end: u32, token_count: u32 },
    #[error("token mapping for a displayed output prefix must begin at token zero")]
    TokenMappingMustBeginAtOutputStart,
    #[error(
        "token range and token-boundary fingerprint must either both be present or both absent"
    )]
    TokenMappingEvidenceMismatch,
    #[error("model call occurrence does not match the span occurrence")]
    CallIdMismatch,
    #[error("model, tokenizer, prompt, sampler, control, role, tokens, or receipt changed")]
    CallBindingMismatch,
    #[error("span extraction receipt does not match its bound evidence")]
    ExtractionReceiptMismatch,
}

fn validate_token_range(
    token_range: Option<NonEmptyTokenRange>,
    token_count: u32,
) -> Result<(), CallError> {
    if let Some(range) = token_range
        && range.end() > token_count
    {
        return Err(CallError::TokenRangeOutOfBounds {
            end: range.end(),
            token_count,
        });
    }
    Ok(())
}

fn validate_declared_token_mapping(
    token_range: Option<NonEmptyTokenRange>,
    boundaries_fingerprint: Option<BlobId>,
    token_count: u32,
) -> Result<(), CallError> {
    match (token_range, boundaries_fingerprint) {
        (Some(range), Some(_)) => {
            if range.start() != 0 {
                return Err(CallError::TokenMappingMustBeginAtOutputStart);
            }
            validate_token_range(Some(range), token_count)
        }
        (None, None) => Ok(()),
        (Some(_), None) | (None, Some(_)) => Err(CallError::TokenMappingEvidenceMismatch),
    }
}

fn fingerprint_token_ids(token_ids: &[u32]) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/token-ids/v1\0");
    digest.update((token_ids.len() as u64).to_be_bytes());
    for token_id in token_ids {
        digest.update(token_id.to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn compute_extraction_receipt(
    occurrence_id: GeneratedSpanOccurrenceId,
    call_id: ModelCallId,
    raw_output_blob_id: BlobId,
    projection: &OutputProjection,
    token_range: Option<NonEmptyTokenRange>,
    binding: &SpanCallBinding,
) -> ExtractionReceipt {
    let mut digest = Sha256::new();
    digest.update(b"loom/generated-span-extraction/v1\0");
    digest.update(occurrence_id.as_ulid().to_bytes());
    digest.update(call_id.as_ulid().to_bytes());
    digest.update(raw_output_blob_id.as_bytes());
    projection.update_digest(&mut digest);
    match token_range {
        Some(range) => {
            digest.update([1]);
            digest.update(range.start().to_be_bytes());
            digest.update(range.end().to_be_bytes());
        }
        None => digest.update([0]),
    }
    binding.update_digest(&mut digest);
    ExtractionReceipt(BlobId::from_bytes(digest.finalize().into()))
}

fn update_optional_blob(digest: &mut Sha256, blob_id: Option<BlobId>) {
    match blob_id {
        Some(blob_id) => {
            digest.update([1]);
            digest.update(blob_id.as_bytes());
        }
        None => digest.update([0]),
    }
}
