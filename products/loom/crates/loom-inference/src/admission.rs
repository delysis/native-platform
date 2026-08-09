use std::fmt;

use loom_research_types::{ModelCall, OutputProjection};
use loom_types::{BlobId, ProjectId};

use crate::{BaseWriterBinding, ExactPromptEvidence};

/// One verified case result in original request order.
///
/// The private payload and private constructors prevent callers from forging
/// or relabeling an outcome. `input_index` is retained explicitly and checked
/// for contiguous order when the native verifier mints the batch.
pub struct VerifiedCaseOutcome {
    input_index: usize,
    kind: VerifiedCaseOutcomeKind,
}

#[cfg_attr(not(feature = "native-llama"), allow(dead_code))]
enum VerifiedCaseOutcomeKind {
    Completed(VerifiedBaseWriterCall),
    Cancelled(CancelledBaseWriterDiagnostic),
}

impl fmt::Debug for VerifiedCaseOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut value = formatter.debug_struct("VerifiedCaseOutcome");
        value.field("input_index", &self.input_index);
        match &self.kind {
            VerifiedCaseOutcomeKind::Completed(call) => value.field("completed", call),
            VerifiedCaseOutcomeKind::Cancelled(diagnostic) => value.field("cancelled", diagnostic),
        };
        value.finish()
    }
}

impl VerifiedCaseOutcome {
    #[cfg(feature = "native-llama")]
    pub(crate) const fn completed(input_index: usize, call: VerifiedBaseWriterCall) -> Self {
        Self {
            input_index,
            kind: VerifiedCaseOutcomeKind::Completed(call),
        }
    }

    #[cfg(feature = "native-llama")]
    pub(crate) const fn cancelled(
        input_index: usize,
        diagnostic: CancelledBaseWriterDiagnostic,
    ) -> Self {
        Self {
            input_index,
            kind: VerifiedCaseOutcomeKind::Cancelled(diagnostic),
        }
    }

    pub const fn input_index(&self) -> usize {
        self.input_index
    }

    pub const fn completed_call(&self) -> Option<&VerifiedBaseWriterCall> {
        match &self.kind {
            VerifiedCaseOutcomeKind::Completed(call) => Some(call),
            VerifiedCaseOutcomeKind::Cancelled(_) => None,
        }
    }

    pub const fn cancelled_diagnostic(&self) -> Option<&CancelledBaseWriterDiagnostic> {
        match &self.kind {
            VerifiedCaseOutcomeKind::Completed(_) => None,
            VerifiedCaseOutcomeKind::Cancelled(diagnostic) => Some(diagnostic),
        }
    }

    fn into_parts(self) -> VerifiedCaseOutcomeParts {
        let kind = match self.kind {
            VerifiedCaseOutcomeKind::Completed(call) => {
                VerifiedCaseOutcomePartsKind::Completed(call.into_parts())
            }
            VerifiedCaseOutcomeKind::Cancelled(diagnostic) => {
                VerifiedCaseOutcomePartsKind::Cancelled(diagnostic.into_parts())
            }
        };
        VerifiedCaseOutcomeParts {
            input_index: self.input_index,
            kind,
        }
    }
}

/// One ordered, move-only case result prepared for store adoption.
pub struct VerifiedCaseOutcomeParts {
    input_index: usize,
    kind: VerifiedCaseOutcomePartsKind,
}

enum VerifiedCaseOutcomePartsKind {
    Completed(VerifiedBaseWriterCallParts),
    Cancelled(CancelledBaseWriterDiagnosticParts),
}

impl fmt::Debug for VerifiedCaseOutcomeParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut value = formatter.debug_struct("VerifiedCaseOutcomeParts");
        value.field("input_index", &self.input_index);
        match &self.kind {
            VerifiedCaseOutcomePartsKind::Completed(call) => value.field("completed", call),
            VerifiedCaseOutcomePartsKind::Cancelled(diagnostic) => {
                value.field("cancelled", diagnostic)
            }
        };
        value.finish()
    }
}

impl VerifiedCaseOutcomeParts {
    pub const fn input_index(&self) -> usize {
        self.input_index
    }

    pub const fn is_completed(&self) -> bool {
        matches!(self.kind, VerifiedCaseOutcomePartsKind::Completed(_))
    }

    /// Consume one outcome without exposing a constructible public enum.
    pub fn consume<R>(
        self,
        completed: impl FnOnce(usize, VerifiedBaseWriterCallParts) -> R,
        cancelled: impl FnOnce(usize, CancelledBaseWriterDiagnosticParts) -> R,
    ) -> R {
        match self.kind {
            VerifiedCaseOutcomePartsKind::Completed(call) => completed(self.input_index, call),
            VerifiedCaseOutcomePartsKind::Cancelled(diagnostic) => {
                cancelled(self.input_index, diagnostic)
            }
        }
    }
}

/// A completed base-writer call admitted by a live backend verifier.
///
/// Runtime charge evidence is minted from the same native output and bound to
/// this call's verification fingerprint. It is accounting evidence only, not
/// a terminal, promotion, or budget-mutation lease.
pub struct VerifiedBaseWriterCall {
    pub(crate) model_call: ModelCall,
    pub(crate) raw_output: Vec<u8>,
    pub(crate) generated_token_ids: Vec<u32>,
    pub(crate) event_json: Vec<u8>,
    pub(crate) backend_audit_json: Vec<u8>,
    pub(crate) displayed_output: Vec<u8>,
    pub(crate) output_projection: Option<OutputProjection>,
    pub(crate) terminal_sampled_token_id: Option<i32>,
    pub(crate) verification_fingerprint: BlobId,
    pub(crate) runtime_charge: VerifiedRuntimeChargeEvidence,
}

/// Move-only typed runtime accounting retained with a verified writer call.
///
/// Callers cannot construct or deserialize this type, and its fingerprint
/// must equal the containing call's verification fingerprint.
pub struct VerifiedRuntimeChargeEvidence {
    prompt_tokens: u64,
    completion_tokens: u64,
    duration_ms: u128,
    verification_fingerprint: BlobId,
}

impl fmt::Debug for VerifiedRuntimeChargeEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedRuntimeChargeEvidence")
            .field("prompt_tokens", &self.prompt_tokens)
            .field("completion_tokens", &self.completion_tokens)
            .field("duration_ms", &self.duration_ms)
            .field("verification_fingerprint", &self.verification_fingerprint)
            .finish()
    }
}

impl VerifiedRuntimeChargeEvidence {
    #[cfg(feature = "native-llama")]
    pub(crate) const fn mint(
        prompt_tokens: u64,
        completion_tokens: u64,
        duration_ms: u128,
        verification_fingerprint: BlobId,
    ) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            duration_ms,
            verification_fingerprint,
        }
    }

    pub const fn prompt_tokens(&self) -> u64 {
        self.prompt_tokens
    }

    pub const fn completion_tokens(&self) -> u64 {
        self.completion_tokens
    }

    pub const fn duration_ms(&self) -> u128 {
        self.duration_ms
    }

    pub const fn verification_fingerprint(&self) -> BlobId {
        self.verification_fingerprint
    }
}

impl fmt::Debug for VerifiedBaseWriterCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedBaseWriterCall")
            .field("call_id", &self.model_call.id())
            .field("raw_output_bytes", &self.raw_output.len())
            .field("generated_token_count", &self.generated_token_ids.len())
            .field("event_json_bytes", &self.event_json.len())
            .field("backend_audit_json_bytes", &self.backend_audit_json.len())
            .field("displayed_output_bytes", &self.displayed_output.len())
            .field("has_projection", &self.output_projection.is_some())
            .field(
                "has_terminal_sampled_token",
                &self.terminal_sampled_token_id.is_some(),
            )
            .field("verification_fingerprint", &self.verification_fingerprint)
            .field("runtime_charge", &self.runtime_charge)
            .finish()
    }
}

impl VerifiedBaseWriterCall {
    pub const fn model_call(&self) -> &ModelCall {
        &self.model_call
    }

    pub fn raw_output(&self) -> &[u8] {
        &self.raw_output
    }

    pub fn generated_token_ids(&self) -> &[u32] {
        &self.generated_token_ids
    }

    pub fn event_json(&self) -> &[u8] {
        &self.event_json
    }

    pub fn backend_audit_json(&self) -> &[u8] {
        &self.backend_audit_json
    }

    pub fn displayed_output(&self) -> &[u8] {
        &self.displayed_output
    }

    pub const fn output_projection(&self) -> Option<&OutputProjection> {
        self.output_projection.as_ref()
    }

    pub const fn token_boundaries_fingerprint(&self) -> Option<BlobId> {
        None
    }

    pub const fn terminal_sampled_token_id(&self) -> Option<i32> {
        self.terminal_sampled_token_id
    }

    pub const fn verification_fingerprint(&self) -> BlobId {
        self.verification_fingerprint
    }

    pub const fn runtime_charge(&self) -> &VerifiedRuntimeChargeEvidence {
        &self.runtime_charge
    }

    fn into_parts(self) -> VerifiedBaseWriterCallParts {
        VerifiedBaseWriterCallParts {
            model_call: self.model_call,
            raw_output: self.raw_output,
            generated_token_ids: self.generated_token_ids,
            event_json: self.event_json,
            backend_audit_json: self.backend_audit_json,
            displayed_output: self.displayed_output,
            output_projection: self.output_projection,
            terminal_sampled_token_id: self.terminal_sampled_token_id,
            verification_fingerprint: self.verification_fingerprint,
            runtime_charge: self.runtime_charge,
        }
    }
}

/// Move-only completed-call evidence for store adoption.
pub struct VerifiedBaseWriterCallParts {
    model_call: ModelCall,
    raw_output: Vec<u8>,
    generated_token_ids: Vec<u32>,
    event_json: Vec<u8>,
    backend_audit_json: Vec<u8>,
    displayed_output: Vec<u8>,
    output_projection: Option<OutputProjection>,
    terminal_sampled_token_id: Option<i32>,
    verification_fingerprint: BlobId,
    runtime_charge: VerifiedRuntimeChargeEvidence,
}

impl fmt::Debug for VerifiedBaseWriterCallParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedBaseWriterCallParts")
            .field("call_id", &self.model_call.id())
            .field("raw_output_bytes", &self.raw_output.len())
            .field("generated_token_count", &self.generated_token_ids.len())
            .field("event_json_bytes", &self.event_json.len())
            .field("backend_audit_json_bytes", &self.backend_audit_json.len())
            .field("displayed_output_bytes", &self.displayed_output.len())
            .field("has_projection", &self.output_projection.is_some())
            .field(
                "has_terminal_sampled_token",
                &self.terminal_sampled_token_id.is_some(),
            )
            .field("verification_fingerprint", &self.verification_fingerprint)
            .field("runtime_charge", &self.runtime_charge)
            .finish()
    }
}

impl VerifiedBaseWriterCallParts {
    pub const fn runtime_charge(&self) -> &VerifiedRuntimeChargeEvidence {
        &self.runtime_charge
    }

    #[allow(clippy::type_complexity)]
    pub fn consume<R>(
        self,
        consumer: impl FnOnce(
            ModelCall,
            Vec<u8>,
            Vec<u32>,
            Vec<u8>,
            Vec<u8>,
            Vec<u8>,
            Option<OutputProjection>,
            Option<i32>,
            BlobId,
        ) -> R,
    ) -> R {
        consumer(
            self.model_call,
            self.raw_output,
            self.generated_token_ids,
            self.event_json,
            self.backend_audit_json,
            self.displayed_output,
            self.output_projection,
            self.terminal_sampled_token_id,
            self.verification_fingerprint,
        )
    }
}

/// A cancelled base-writer call retained only as diagnostic evidence.
pub struct CancelledBaseWriterDiagnostic {
    pub(crate) model_call: ModelCall,
    pub(crate) partial_raw_output: Vec<u8>,
    pub(crate) generated_token_ids: Vec<u32>,
    pub(crate) event_json: Vec<u8>,
    pub(crate) backend_audit_json: Vec<u8>,
    pub(crate) verification_fingerprint: BlobId,
}

impl fmt::Debug for CancelledBaseWriterDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancelledBaseWriterDiagnostic")
            .field("call_id", &self.model_call.id())
            .field("partial_raw_output_bytes", &self.partial_raw_output.len())
            .field("generated_token_count", &self.generated_token_ids.len())
            .field("event_json_bytes", &self.event_json.len())
            .field("backend_audit_json_bytes", &self.backend_audit_json.len())
            .field("verification_fingerprint", &self.verification_fingerprint)
            .finish()
    }
}

impl CancelledBaseWriterDiagnostic {
    pub const fn model_call(&self) -> &ModelCall {
        &self.model_call
    }

    pub fn partial_raw_output(&self) -> &[u8] {
        &self.partial_raw_output
    }

    pub fn generated_token_ids(&self) -> &[u32] {
        &self.generated_token_ids
    }

    pub fn event_json(&self) -> &[u8] {
        &self.event_json
    }

    pub fn backend_audit_json(&self) -> &[u8] {
        &self.backend_audit_json
    }

    pub const fn verification_fingerprint(&self) -> BlobId {
        self.verification_fingerprint
    }

    fn into_parts(self) -> CancelledBaseWriterDiagnosticParts {
        CancelledBaseWriterDiagnosticParts {
            model_call: self.model_call,
            partial_raw_output: self.partial_raw_output,
            generated_token_ids: self.generated_token_ids,
            event_json: self.event_json,
            backend_audit_json: self.backend_audit_json,
            verification_fingerprint: self.verification_fingerprint,
        }
    }
}

/// Move-only cancelled-call diagnostic evidence for store adoption.
pub struct CancelledBaseWriterDiagnosticParts {
    model_call: ModelCall,
    partial_raw_output: Vec<u8>,
    generated_token_ids: Vec<u32>,
    event_json: Vec<u8>,
    backend_audit_json: Vec<u8>,
    verification_fingerprint: BlobId,
}

impl fmt::Debug for CancelledBaseWriterDiagnosticParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancelledBaseWriterDiagnosticParts")
            .field("call_id", &self.model_call.id())
            .field("partial_raw_output_bytes", &self.partial_raw_output.len())
            .field("generated_token_count", &self.generated_token_ids.len())
            .field("event_json_bytes", &self.event_json.len())
            .field("backend_audit_json_bytes", &self.backend_audit_json.len())
            .field("verification_fingerprint", &self.verification_fingerprint)
            .finish()
    }
}

impl CancelledBaseWriterDiagnosticParts {
    pub fn consume<R>(
        self,
        consumer: impl FnOnce(ModelCall, Vec<u8>, Vec<u32>, Vec<u8>, Vec<u8>, BlobId) -> R,
    ) -> R {
        consumer(
            self.model_call,
            self.partial_raw_output,
            self.generated_token_ids,
            self.event_json,
            self.backend_audit_json,
            self.verification_fingerprint,
        )
    }
}

struct VerifiedBatchEvidence {
    project_id: ProjectId,
    binding: BaseWriterBinding,
    prompt_evidence: ExactPromptEvidence,
    backend_request_id: String,
    outcomes: Vec<VerifiedCaseOutcome>,
    verification_fingerprint: BlobId,
}

impl VerifiedBatchEvidence {
    fn debug(&self, name: &str, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct(name)
            .field("project_id", &self.project_id)
            .field("binding", &self.binding)
            .field("prompt_evidence", &self.prompt_evidence)
            .field("outcome_count", &self.outcomes.len())
            .field("completed_count", &self.completed_count())
            .field("verification_fingerprint", &self.verification_fingerprint)
            .finish()
    }

    fn completed_count(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.completed_call().is_some())
            .count()
    }

    fn into_parts(self) -> VerifiedBatchEvidenceParts {
        VerifiedBatchEvidenceParts {
            project_id: self.project_id,
            binding: self.binding,
            prompt_evidence: self.prompt_evidence,
            backend_request_id: self.backend_request_id,
            outcomes: self
                .outcomes
                .into_iter()
                .map(VerifiedCaseOutcome::into_parts)
                .collect(),
            verification_fingerprint: self.verification_fingerprint,
        }
    }
}

/// Admitted batch authority. Its private constructor requires at least one
/// completed call.
///
/// ```compile_fail
/// use loom_inference::VerifiedInferenceEnvelope;
/// fn duplicate(value: VerifiedInferenceEnvelope) {
///     let _copy = value.clone();
/// }
/// ```
///
/// ```compile_fail
/// use loom_inference::VerifiedInferenceEnvelope;
/// fn require_serialize<T: serde::Serialize>() {}
/// require_serialize::<VerifiedInferenceEnvelope>();
/// ```
pub struct VerifiedInferenceEnvelope(VerifiedBatchEvidence);

impl fmt::Debug for VerifiedInferenceEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.debug("VerifiedInferenceEnvelope", formatter)
    }
}

/// Verified all-cancelled batch evidence. This type has no completed-call
/// accessor and cannot become admitted authority.
///
/// ```compile_fail
/// use loom_inference::VerifiedDiagnosticEnvelope;
/// fn cannot_obtain_admitted_calls(value: &VerifiedDiagnosticEnvelope) {
///     let _ = value.completed_calls();
/// }
/// ```
pub struct VerifiedDiagnosticEnvelope(VerifiedBatchEvidence);

impl fmt::Debug for VerifiedDiagnosticEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.debug("VerifiedDiagnosticEnvelope", formatter)
    }
}

/// Result of consuming one live backend batch seal.
///
/// ```compile_fail
/// use loom_inference::VerifiedInferenceOutcome;
/// fn duplicate(value: VerifiedInferenceOutcome) {
///     let _copy = value.clone();
/// }
/// ```
pub enum VerifiedInferenceOutcome {
    Admitted(VerifiedInferenceEnvelope),
    DiagnosticOnly(VerifiedDiagnosticEnvelope),
}

impl fmt::Debug for VerifiedInferenceOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Admitted(value) => formatter.debug_tuple("Admitted").field(value).finish(),
            Self::DiagnosticOnly(value) => formatter
                .debug_tuple("DiagnosticOnly")
                .field(value)
                .finish(),
        }
    }
}

impl VerifiedInferenceOutcome {
    #[cfg(feature = "native-llama")]
    pub(crate) fn mint(
        project_id: ProjectId,
        binding: BaseWriterBinding,
        prompt_evidence: ExactPromptEvidence,
        backend_request_id: String,
        outcomes: Vec<VerifiedCaseOutcome>,
        verification_fingerprint: BlobId,
    ) -> Result<Self, AdmissionMintError> {
        if outcomes.is_empty() {
            return Err(AdmissionMintError::EmptyOutcomes);
        }
        if outcomes
            .iter()
            .enumerate()
            .any(|(expected, outcome)| outcome.input_index != expected)
        {
            return Err(AdmissionMintError::NonContiguousOutcomes);
        }
        let evidence = VerifiedBatchEvidence {
            project_id,
            binding,
            prompt_evidence,
            backend_request_id,
            outcomes,
            verification_fingerprint,
        };
        if evidence.completed_count() == 0 {
            Ok(Self::DiagnosticOnly(VerifiedDiagnosticEnvelope(evidence)))
        } else {
            Ok(Self::Admitted(VerifiedInferenceEnvelope(evidence)))
        }
    }

    pub fn into_parts(self) -> VerifiedInferenceOutcomeParts {
        match self {
            Self::Admitted(value) => VerifiedInferenceOutcomeParts::Admitted(value.into_parts()),
            Self::DiagnosticOnly(value) => {
                VerifiedInferenceOutcomeParts::DiagnosticOnly(value.into_parts())
            }
        }
    }
}

macro_rules! batch_accessors {
    ($type:ty) => {
        impl $type {
            pub const fn project_id(&self) -> ProjectId {
                self.0.project_id
            }

            pub const fn binding(&self) -> &BaseWriterBinding {
                &self.0.binding
            }

            pub const fn prompt_evidence(&self) -> &ExactPromptEvidence {
                &self.0.prompt_evidence
            }

            pub fn backend_request_id(&self) -> &str {
                &self.0.backend_request_id
            }

            pub fn outcomes(&self) -> &[VerifiedCaseOutcome] {
                &self.0.outcomes
            }

            pub const fn verification_fingerprint(&self) -> BlobId {
                self.0.verification_fingerprint
            }
        }
    };
}

batch_accessors!(VerifiedInferenceEnvelope);
batch_accessors!(VerifiedDiagnosticEnvelope);

impl VerifiedInferenceEnvelope {
    pub fn completed_calls(&self) -> impl Iterator<Item = &VerifiedBaseWriterCall> {
        self.0
            .outcomes
            .iter()
            .filter_map(VerifiedCaseOutcome::completed_call)
    }

    pub fn cancelled_diagnostics(&self) -> impl Iterator<Item = &CancelledBaseWriterDiagnostic> {
        self.0
            .outcomes
            .iter()
            .filter_map(VerifiedCaseOutcome::cancelled_diagnostic)
    }

    pub fn into_parts(self) -> VerifiedInferenceParts {
        VerifiedInferenceParts(self.0.into_parts())
    }
}

impl VerifiedDiagnosticEnvelope {
    pub fn cancelled_diagnostics(&self) -> impl Iterator<Item = &CancelledBaseWriterDiagnostic> {
        self.0
            .outcomes
            .iter()
            .filter_map(VerifiedCaseOutcome::cancelled_diagnostic)
    }

    pub fn into_parts(self) -> VerifiedDiagnosticParts {
        VerifiedDiagnosticParts(self.0.into_parts())
    }
}

struct VerifiedBatchEvidenceParts {
    project_id: ProjectId,
    binding: BaseWriterBinding,
    prompt_evidence: ExactPromptEvidence,
    backend_request_id: String,
    outcomes: Vec<VerifiedCaseOutcomeParts>,
    verification_fingerprint: BlobId,
}

impl VerifiedBatchEvidenceParts {
    fn debug(&self, name: &str, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct(name)
            .field("project_id", &self.project_id)
            .field("binding", &self.binding)
            .field("prompt_evidence", &self.prompt_evidence)
            .field("outcome_count", &self.outcomes.len())
            .field("verification_fingerprint", &self.verification_fingerprint)
            .finish()
    }

    fn consume<R>(
        self,
        consumer: impl FnOnce(
            ProjectId,
            BaseWriterBinding,
            ExactPromptEvidence,
            String,
            Vec<VerifiedCaseOutcomeParts>,
            BlobId,
        ) -> R,
    ) -> R {
        consumer(
            self.project_id,
            self.binding,
            self.prompt_evidence,
            self.backend_request_id,
            self.outcomes,
            self.verification_fingerprint,
        )
    }
}

/// Move-only store material for an admitted batch.
///
/// ```compile_fail
/// use loom_inference::VerifiedInferenceParts;
/// fn duplicate(value: VerifiedInferenceParts) {
///     let _copy = value.clone();
/// }
/// ```
pub struct VerifiedInferenceParts(VerifiedBatchEvidenceParts);

impl fmt::Debug for VerifiedInferenceParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.debug("VerifiedInferenceParts", formatter)
    }
}

pub struct VerifiedDiagnosticParts(VerifiedBatchEvidenceParts);

impl fmt::Debug for VerifiedDiagnosticParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.debug("VerifiedDiagnosticParts", formatter)
    }
}

pub enum VerifiedInferenceOutcomeParts {
    Admitted(VerifiedInferenceParts),
    DiagnosticOnly(VerifiedDiagnosticParts),
}

impl fmt::Debug for VerifiedInferenceOutcomeParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Admitted(value) => formatter.debug_tuple("Admitted").field(value).finish(),
            Self::DiagnosticOnly(value) => formatter
                .debug_tuple("DiagnosticOnly")
                .field(value)
                .finish(),
        }
    }
}

macro_rules! parts_consumer {
    ($type:ty) => {
        impl $type {
            pub fn consume<R>(
                self,
                consumer: impl FnOnce(
                    ProjectId,
                    BaseWriterBinding,
                    ExactPromptEvidence,
                    String,
                    Vec<VerifiedCaseOutcomeParts>,
                    BlobId,
                ) -> R,
            ) -> R {
                self.0.consume(consumer)
            }
        }
    };
}

parts_consumer!(VerifiedInferenceParts);
parts_consumer!(VerifiedDiagnosticParts);

#[cfg(feature = "native-llama")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdmissionMintError {
    EmptyOutcomes,
    NonContiguousOutcomes,
}
