//! Private owner-worker authority for exact-token embedding execution.
//!
//! [`EmbeddingBatchOutput`] deliberately remains a serializable diagnostic
//! claim. Only the resident worker can attach the independent admission hash,
//! captured output-bit hash, artifact checks, and exact worker identity needed
//! to construct [`VerifiedEmbeddingBatch`].

use super::*;

/// The only successful terminal carried by a verified embedding batch.
///
/// Cancellation and failure resolve the ticket with typed errors and can never
/// be relabelled as a completed seal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifiedEmbeddingTerminal {
    Completed,
}

/// Move-only owner-worker authority for one exact-token embedding batch.
///
/// The public serialized [`EmbeddingBatchOutput`] is inspectable evidence, not
/// authority. This type has no public constructor and implements neither
/// `Clone`, `Default`, nor Serde.
///
/// ```compile_fail
/// use llama_native_engine::VerifiedEmbeddingBatch;
/// fn clone_seal(value: &VerifiedEmbeddingBatch) -> VerifiedEmbeddingBatch {
///     value.clone()
/// }
/// ```
///
/// ```compile_fail
/// use llama_native_engine::VerifiedEmbeddingBatch;
/// fn require_default<T: Default>() {}
/// require_default::<VerifiedEmbeddingBatch>();
/// ```
///
/// ```compile_fail
/// use llama_native_engine::VerifiedEmbeddingBatch;
/// fn require_serialize<T: serde::Serialize>() {}
/// require_serialize::<VerifiedEmbeddingBatch>();
/// ```
///
/// ```compile_fail
/// use llama_native_engine::VerifiedEmbeddingBatch;
/// fn require_deserialize<T: for<'de> serde::Deserialize<'de>>() {}
/// require_deserialize::<VerifiedEmbeddingBatch>();
/// ```
///
/// ```compile_fail
/// use llama_native_engine::VerifiedEmbeddingBatch;
/// use llama_native_types::EmbeddingBatchOutput;
/// fn relabel_claim(output: EmbeddingBatchOutput) -> VerifiedEmbeddingBatch {
///     output.into()
/// }
/// ```
///
/// ```compile_fail
/// use llama_native_engine::VerifiedEmbeddingBatch;
/// let _ = VerifiedEmbeddingBatch {};
/// ```
pub struct VerifiedEmbeddingBatch {
    request: EmbeddingBatchRequest,
    output: EmbeddingBatchOutput,
    resident_model_fingerprint: ModelFingerprint,
    execution_fingerprint: ModelFingerprint,
    request_sha256: String,
    output_bits_sha256: String,
    ledger_sha256: String,
    owner_call_sequence: u64,
    terminal: VerifiedEmbeddingTerminal,
    worker_identity: Arc<WorkerIdentity>,
}

impl std::fmt::Debug for VerifiedEmbeddingBatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedEmbeddingBatch")
            .field("request_id", &self.request.request_id())
            .field("model_id", &self.resident_model_fingerprint.model_id)
            .field(
                "model_sha256",
                &self.resident_model_fingerprint.model_sha256,
            )
            .field("input_count", &self.request.inputs().len())
            .field("request_sha256", &self.request_sha256)
            .field("output_bits_sha256", &self.output_bits_sha256)
            .field("ledger_sha256", &self.ledger_sha256)
            .field("owner_call_sequence", &self.owner_call_sequence)
            .field("terminal", &self.terminal)
            .finish()
    }
}

impl VerifiedEmbeddingBatch {
    #[must_use]
    pub const fn request(&self) -> &EmbeddingBatchRequest {
        &self.request
    }

    #[must_use]
    pub const fn output(&self) -> &EmbeddingBatchOutput {
        &self.output
    }

    #[must_use]
    pub const fn resident_model_fingerprint(&self) -> &ModelFingerprint {
        &self.resident_model_fingerprint
    }

    #[must_use]
    pub const fn execution_fingerprint(&self) -> &ModelFingerprint {
        &self.execution_fingerprint
    }

    #[must_use]
    pub fn requested_pooling(&self) -> EmbeddingPooling {
        self.request.pooling()
    }

    #[must_use]
    pub fn requested_normalization(&self) -> EmbeddingNormalization {
        self.request.normalization()
    }

    /// Pooling and normalization as resolved by the live temporary context,
    /// plus the model-reported embedding dimensions.
    #[must_use]
    pub fn resolved_config(&self) -> EmbeddingOutputConfig {
        self.output.config()
    }

    #[must_use]
    pub fn transport(&self) -> NativeTransport {
        self.output.evidence().transport()
    }

    #[must_use]
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }

    #[must_use]
    pub fn output_bits_sha256(&self) -> &str {
        &self.output_bits_sha256
    }

    #[must_use]
    pub fn ledger_sha256(&self) -> &str {
        &self.ledger_sha256
    }

    #[must_use]
    pub const fn owner_call_sequence(&self) -> u64 {
        self.owner_call_sequence
    }

    #[must_use]
    pub const fn terminal(&self) -> VerifiedEmbeddingTerminal {
        self.terminal
    }

    /// Bind this completion to the exact owner thread that was later joined.
    /// A token for another resident, or a replayed serialized output, cannot
    /// satisfy this identity test.
    #[must_use]
    pub fn belongs_to_joined_model(&self, joined: &JoinedNativeModel) -> bool {
        Arc::ptr_eq(&self.worker_identity, &joined.worker_identity)
    }

    /// Discard live authority and retain only the publicly serializable claim.
    #[must_use]
    pub fn into_unverified_output(self) -> EmbeddingBatchOutput {
        self.output
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EmbeddingCompletionTerminal {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug)]
pub(super) struct VerifiedEmbeddingEvidence {
    request: EmbeddingBatchRequest,
    resident_model_fingerprint: ModelFingerprint,
    execution_fingerprint: ModelFingerprint,
    request_sha256: String,
    output_bits_sha256: String,
    ledger_sha256: String,
    owner_call_sequence: u64,
    worker_identity: Arc<WorkerIdentity>,
}

#[derive(Debug)]
pub(super) struct EmbeddingCompletion {
    diagnostic: NativeResult<EmbeddingBatchOutput>,
    authority: Option<NativeResult<Box<VerifiedEmbeddingEvidence>>>,
    terminal: EmbeddingCompletionTerminal,
}

impl EmbeddingCompletion {
    pub(super) fn completed(
        output: EmbeddingBatchOutput,
        authority: NativeResult<VerifiedEmbeddingEvidence>,
    ) -> Self {
        Self {
            diagnostic: Ok(output),
            authority: Some(authority.map(Box::new)),
            terminal: EmbeddingCompletionTerminal::Completed,
        }
    }

    pub(super) fn failed(error: NativeError) -> Self {
        let terminal = if error.code == NativeErrorCode::Cancelled {
            EmbeddingCompletionTerminal::Cancelled
        } else {
            EmbeddingCompletionTerminal::Failed
        };
        Self {
            diagnostic: Err(error),
            authority: None,
            terminal,
        }
    }

    pub(super) fn into_output(self) -> NativeResult<EmbeddingBatchOutput> {
        self.diagnostic
    }

    pub(super) fn into_verified(self) -> NativeResult<VerifiedEmbeddingBatch> {
        let Self {
            diagnostic,
            authority,
            terminal,
        } = self;
        let output = diagnostic?;
        if terminal != EmbeddingCompletionTerminal::Completed {
            return Err(embedding_verification_error(
                "a non-completed embedding terminal cannot mint authority",
            ));
        }
        let evidence = *authority.ok_or_else(|| {
            embedding_verification_error(
                "completed embedding result has no owner-worker authority decision",
            )
        })??;
        Ok(VerifiedEmbeddingBatch {
            request: evidence.request,
            output,
            resident_model_fingerprint: evidence.resident_model_fingerprint,
            execution_fingerprint: evidence.execution_fingerprint,
            request_sha256: evidence.request_sha256,
            output_bits_sha256: evidence.output_bits_sha256,
            ledger_sha256: evidence.ledger_sha256,
            owner_call_sequence: evidence.owner_call_sequence,
            terminal: VerifiedEmbeddingTerminal::Completed,
            worker_identity: evidence.worker_identity,
        })
    }

    #[cfg(test)]
    pub(super) const fn terminal(&self) -> EmbeddingCompletionTerminal {
        self.terminal
    }
}

pub(super) fn embedding_request_sha256(request: &EmbeddingBatchRequest) -> String {
    let mut digest = EmbeddingEvidenceDigest::new("native-embedding-request-v1");
    digest.text(request.request_id());
    digest.text(request.model_id());
    digest.pooling(request.pooling());
    digest.normalization(request.normalization());
    digest.usize(request.inputs().len());
    for input in request.inputs() {
        digest.text(input.input_id());
        digest.usize(input.token_ids().len());
        for token_id in input.token_ids() {
            digest.i32(*token_id);
        }
    }
    digest.finish()
}

pub(super) fn embedding_output_bits_sha256(output: &EmbeddingBatchOutput) -> String {
    let mut digest = EmbeddingEvidenceDigest::new("native-embedding-output-bits-v1");
    digest.text(output.request_id());
    digest.text(output.model_id());
    digest.pooling(output.config().pooling());
    digest.normalization(output.config().normalization());
    digest.u32(output.config().dimensions());
    hash_model_fingerprint(&mut digest, output.model_fingerprint());
    digest.transport(output.evidence().transport());
    digest.bool(output.evidence().real_engine_invoked());
    digest.bool(output.evidence().fake_fixture());
    digest.usize(output.outputs().len());
    for vector in output.outputs() {
        digest.text(vector.input_id());
        digest.usize(vector.input_index());
        digest.usize(vector.token_ids().len());
        for token_id in vector.token_ids() {
            digest.i32(*token_id);
        }
        digest.u32(vector.row_count());
        digest.usize(vector.values().len());
        for value in vector.values() {
            digest.u32(value.to_bits());
        }
    }
    digest.finish()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn verify_embedding_batch_authority(
    request: EmbeddingBatchRequest,
    admitted_request_sha256: &str,
    resident_model_fingerprint: ModelFingerprint,
    expected_execution_fingerprint: ModelFingerprint,
    expected_dimensions: u32,
    output: &EmbeddingBatchOutput,
    captured_output_bits_sha256: &str,
    owner_call_sequence: u64,
    terminals: &[EmbeddingCompletionTerminal],
    cancellation: &AtomicBool,
    worker_identity: Arc<WorkerIdentity>,
    artifacts: &ModelArtifactGuards,
) -> NativeResult<VerifiedEmbeddingEvidence> {
    validate_verified_embedding_batch(
        &request,
        admitted_request_sha256,
        &resident_model_fingerprint,
        &expected_execution_fingerprint,
        expected_dimensions,
        output,
        captured_output_bits_sha256,
        terminals,
        cancellation.load(Ordering::Acquire),
    )?;
    // This is the final fallible check before the private ledger is minted.
    artifacts.verify_strict_unchanged(&resident_model_fingerprint)?;
    let output_bits_sha256 = embedding_output_bits_sha256(output);
    let ledger_sha256 = embedding_ledger_sha256(
        admitted_request_sha256,
        &resident_model_fingerprint,
        &expected_execution_fingerprint,
        output.config(),
        &output_bits_sha256,
        owner_call_sequence,
    );
    Ok(VerifiedEmbeddingEvidence {
        request,
        resident_model_fingerprint,
        execution_fingerprint: expected_execution_fingerprint,
        request_sha256: admitted_request_sha256.to_string(),
        output_bits_sha256,
        ledger_sha256,
        owner_call_sequence,
        worker_identity,
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_verified_embedding_batch(
    request: &EmbeddingBatchRequest,
    admitted_request_sha256: &str,
    resident_model_fingerprint: &ModelFingerprint,
    expected_execution_fingerprint: &ModelFingerprint,
    expected_dimensions: u32,
    output: &EmbeddingBatchOutput,
    captured_output_bits_sha256: &str,
    terminals: &[EmbeddingCompletionTerminal],
    cancelled: bool,
) -> NativeResult<()> {
    if terminals != [EmbeddingCompletionTerminal::Completed] {
        return Err(embedding_verification_error(
            "verified embedding requires exactly one completed owner-worker terminal",
        ));
    }
    if cancelled {
        return Err(NativeError::new(
            NativeErrorCode::Cancelled,
            "a cancelled embedding request cannot mint completion authority",
        ));
    }
    if admitted_request_sha256 != embedding_request_sha256(request) {
        return Err(embedding_verification_error(
            "embedding request bytes changed after admission",
        ));
    }
    if request.model_id() != resident_model_fingerprint.model_id
        || expected_execution_fingerprint.model_id != resident_model_fingerprint.model_id
        || expected_execution_fingerprint.model_size != resident_model_fingerprint.model_size
        || expected_execution_fingerprint.model_sha256 != resident_model_fingerprint.model_sha256
        || expected_execution_fingerprint.tokenizer_sha256
            != resident_model_fingerprint.tokenizer_sha256
        || expected_execution_fingerprint.chat_template_sha256
            != resident_model_fingerprint.chat_template_sha256
        || expected_execution_fingerprint.multimodal_projector_sha256
            != resident_model_fingerprint.multimodal_projector_sha256
        || expected_execution_fingerprint.binding_version
            != resident_model_fingerprint.binding_version
        || expected_execution_fingerprint.build_id != resident_model_fingerprint.build_id
        || expected_execution_fingerprint.backend != resident_model_fingerprint.backend
    {
        return Err(embedding_verification_error(
            "embedding resident and execution fingerprints disagree",
        ));
    }
    if output.request_id() != request.request_id()
        || output.model_id() != request.model_id()
        || output.model_fingerprint() != expected_execution_fingerprint
        || output.config().pooling() != request.pooling()
        || output.config().normalization() != request.normalization()
        || output.config().dimensions() != expected_dimensions
    {
        return Err(embedding_verification_error(
            "embedding output identity or resolved configuration disagrees",
        ));
    }
    let transport = output.evidence();
    if transport.transport() != NativeTransport::InProcess
        || !transport.real_engine_invoked()
        || transport.fake_fixture()
    {
        return Err(embedding_verification_error(
            "embedding output lacks live in-process transport evidence",
        ));
    }
    if output.outputs().len() != request.inputs().len() {
        return Err(embedding_verification_error(
            "embedding output count disagrees with the submitted inputs",
        ));
    }
    for (index, (input, vector)) in request.inputs().iter().zip(output.outputs()).enumerate() {
        if vector.input_index() != index
            || vector.input_id() != input.input_id()
            || vector.token_ids() != input.token_ids()
            || vector.values().iter().any(|value| !value.is_finite())
        {
            return Err(embedding_verification_error(
                "embedding output order, exact tokens, or finite values disagree",
            ));
        }
        if output.config().normalization() == EmbeddingNormalization::L2 {
            let width = output.config().dimensions() as usize;
            for row in vector.values().chunks_exact(width) {
                let norm = row
                    .iter()
                    .map(|value| f64::from(*value).powi(2))
                    .sum::<f64>()
                    .sqrt();
                if !norm.is_finite() || (norm - 1.0).abs() > 1.0e-4 {
                    return Err(embedding_verification_error(
                        "embedding output declared L2 normalization without unit-norm rows",
                    ));
                }
            }
        }
    }
    if captured_output_bits_sha256 != embedding_output_bits_sha256(output) {
        return Err(embedding_verification_error(
            "embedding output bits changed after owner-worker capture",
        ));
    }
    Ok(())
}

fn embedding_ledger_sha256(
    request_sha256: &str,
    resident: &ModelFingerprint,
    execution: &ModelFingerprint,
    resolved: EmbeddingOutputConfig,
    output_bits_sha256: &str,
    owner_call_sequence: u64,
) -> String {
    let mut digest = EmbeddingEvidenceDigest::new("native-embedding-owner-ledger-v1");
    digest.text(request_sha256);
    hash_model_fingerprint(&mut digest, resident);
    hash_model_fingerprint(&mut digest, execution);
    digest.pooling(resolved.pooling());
    digest.normalization(resolved.normalization());
    digest.u32(resolved.dimensions());
    digest.text(output_bits_sha256);
    digest.u64(owner_call_sequence);
    digest.transport(NativeTransport::InProcess);
    digest.text(LLAMA_NATIVE_BUILD_MANIFEST_SHA256);
    digest.text("completed");
    digest.finish()
}

fn embedding_verification_error(message: impl Into<String>) -> NativeError {
    NativeError::new(NativeErrorCode::Internal, message)
}

fn hash_model_fingerprint(digest: &mut EmbeddingEvidenceDigest, value: &ModelFingerprint) {
    digest.text(&value.model_id);
    digest.u64(value.model_size);
    digest.text(&value.model_sha256);
    digest.text(&value.tokenizer_sha256);
    digest.text(&value.chat_template_sha256);
    match &value.multimodal_projector_sha256 {
        Some(projector) => {
            digest.bool(true);
            digest.text(projector);
        }
        None => digest.bool(false),
    }
    digest.text(&value.binding_version);
    digest.text(&value.build_id);
    digest.text(&value.backend);
    digest.u32(value.context_tokens);
    digest.u32(value.batch_tokens);
    digest.u32(value.max_sequences);
    digest.text(&value.rope_config_sha256);
    digest.text(&value.kv_layout_sha256);
}

struct EmbeddingEvidenceDigest(Sha256);

impl EmbeddingEvidenceDigest {
    fn new(domain: &str) -> Self {
        let mut digest = Self(Sha256::new());
        digest.text(domain);
        digest
    }

    fn bytes(&mut self, value: &[u8]) {
        self.u64(value.len() as u64);
        self.0.update(value);
    }

    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn bool(&mut self, value: bool) {
        self.0.update([u8::from(value)]);
    }

    fn i32(&mut self, value: i32) {
        self.0.update(value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.0.update(value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(value.to_le_bytes());
    }

    fn usize(&mut self, value: usize) {
        self.u64(value as u64);
    }

    fn pooling(&mut self, value: EmbeddingPooling) {
        self.0.update([match value {
            EmbeddingPooling::None => 0,
            EmbeddingPooling::Mean => 1,
            EmbeddingPooling::Cls => 2,
            EmbeddingPooling::Last => 3,
            EmbeddingPooling::Rank => 4,
        }]);
    }

    fn normalization(&mut self, value: EmbeddingNormalization) {
        self.0.update([match value {
            EmbeddingNormalization::None => 0,
            EmbeddingNormalization::L2 => 1,
        }]);
    }

    fn transport(&mut self, value: NativeTransport) {
        self.0.update([match value {
            NativeTransport::InProcess => 0,
            NativeTransport::FakeFixture => 1,
        }]);
    }

    fn finish(self) -> String {
        format!("{:x}", self.0.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llama_native_types::{EmbeddingInput, EmbeddingVectorOutput};
    #[cfg(unix)]
    use std::sync::atomic::AtomicU64;

    #[cfg(unix)]
    static TEST_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

    fn resident_fingerprint(model_sha256: &str) -> ModelFingerprint {
        ModelFingerprint {
            model_id: "embedding-model".to_string(),
            model_size: 1024,
            model_sha256: model_sha256.to_string(),
            tokenizer_sha256: "b".repeat(64),
            chat_template_sha256: "c".repeat(64),
            multimodal_projector_sha256: None,
            binding_version: LLAMA_CPP_BINDING_VERSION.to_string(),
            build_id: "embedding-test-build".to_string(),
            backend: "cpu".to_string(),
            context_tokens: 512,
            batch_tokens: 64,
            max_sequences: 1,
            rope_config_sha256: "d".repeat(64),
            kv_layout_sha256: "e".repeat(64),
        }
    }

    fn execution_fingerprint(resident: &ModelFingerprint) -> ModelFingerprint {
        let mut execution = resident.clone();
        execution.context_tokens = 512;
        execution.batch_tokens = 3;
        execution.max_sequences = 1;
        execution.rope_config_sha256 = "1".repeat(64);
        execution.kv_layout_sha256 = "2".repeat(64);
        execution
    }

    fn request() -> EmbeddingBatchRequest {
        EmbeddingBatchRequest::new(
            "embedding-request".to_string(),
            "embedding-model".to_string(),
            vec![
                EmbeddingInput::new("first".to_string(), vec![1, 2, 3]).expect("first input"),
                EmbeddingInput::new("second".to_string(), vec![4, 5]).expect("second input"),
            ],
            EmbeddingPooling::Mean,
            EmbeddingNormalization::L2,
        )
        .expect("embedding request")
    }

    fn make_output(
        execution: ModelFingerprint,
        evidence: EmbeddingTransportEvidence,
        first_values: [f32; 2],
    ) -> EmbeddingBatchOutput {
        EmbeddingBatchOutput::new(
            "embedding-request".to_string(),
            "embedding-model".to_string(),
            EmbeddingOutputConfig::new(EmbeddingPooling::Mean, EmbeddingNormalization::L2, 2)
                .expect("output config"),
            vec![
                EmbeddingVectorOutput::new(
                    "first".to_string(),
                    0,
                    vec![1, 2, 3],
                    1,
                    first_values.to_vec(),
                )
                .expect("first output"),
                EmbeddingVectorOutput::new("second".to_string(), 1, vec![4, 5], 1, vec![0.6, 0.8])
                    .expect("second output"),
            ],
            execution,
            evidence,
        )
        .expect("embedding output")
    }

    fn live_evidence() -> EmbeddingTransportEvidence {
        EmbeddingTransportEvidence::new(NativeTransport::InProcess, true, false)
            .expect("live evidence")
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_fixture(
        request: &EmbeddingBatchRequest,
        admitted_sha256: &str,
        resident: &ModelFingerprint,
        execution: &ModelFingerprint,
        output: &EmbeddingBatchOutput,
        captured_sha256: &str,
        terminals: &[EmbeddingCompletionTerminal],
        cancelled: bool,
    ) -> NativeResult<()> {
        validate_verified_embedding_batch(
            request,
            admitted_sha256,
            resident,
            execution,
            2,
            output,
            captured_sha256,
            terminals,
            cancelled,
        )
    }

    #[test]
    fn exact_embedding_authority_rejects_every_swappable_dimension() {
        let request = request();
        let resident = resident_fingerprint(&"a".repeat(64));
        let execution = execution_fingerprint(&resident);
        let output = make_output(execution.clone(), live_evidence(), [0.8, 0.6]);
        let admitted = embedding_request_sha256(&request);
        let captured = embedding_output_bits_sha256(&output);
        let completed = [EmbeddingCompletionTerminal::Completed];
        validate_fixture(
            &request, &admitted, &resident, &execution, &output, &captured, &completed, false,
        )
        .expect("exact owner-worker components verify");

        let swapped_request = EmbeddingBatchRequest::new(
            request.request_id().to_string(),
            request.model_id().to_string(),
            vec![request.inputs()[1].clone(), request.inputs()[0].clone()],
            request.pooling(),
            request.normalization(),
        )
        .expect("swapped request");
        assert!(
            validate_fixture(
                &swapped_request,
                &admitted,
                &resident,
                &execution,
                &output,
                &captured,
                &completed,
                false,
            )
            .is_err()
        );

        let changed_output = make_output(execution.clone(), live_evidence(), [0.7, 0.6]);
        assert!(
            validate_fixture(
                &request,
                &admitted,
                &resident,
                &execution,
                &changed_output,
                &captured,
                &completed,
                false,
            )
            .is_err()
        );

        let changed_token_request = EmbeddingBatchRequest::new(
            request.request_id().to_string(),
            request.model_id().to_string(),
            vec![
                EmbeddingInput::new("first".to_string(), vec![1, 2, 9])
                    .expect("changed first input"),
                request.inputs()[1].clone(),
            ],
            request.pooling(),
            request.normalization(),
        )
        .expect("changed-token request");
        assert!(
            validate_fixture(
                &changed_token_request,
                &embedding_request_sha256(&changed_token_request),
                &resident,
                &execution,
                &output,
                &captured,
                &completed,
                false,
            )
            .is_err()
        );

        let changed_pooling_request = EmbeddingBatchRequest::new(
            request.request_id().to_string(),
            request.model_id().to_string(),
            request.inputs().to_vec(),
            EmbeddingPooling::Last,
            request.normalization(),
        )
        .expect("changed-pooling request");
        assert!(
            validate_fixture(
                &changed_pooling_request,
                &embedding_request_sha256(&changed_pooling_request),
                &resident,
                &execution,
                &output,
                &captured,
                &completed,
                false,
            )
            .is_err()
        );

        let changed_normalization_request = EmbeddingBatchRequest::new(
            request.request_id().to_string(),
            request.model_id().to_string(),
            request.inputs().to_vec(),
            request.pooling(),
            EmbeddingNormalization::None,
        )
        .expect("changed-normalization request");
        assert!(
            validate_fixture(
                &changed_normalization_request,
                &embedding_request_sha256(&changed_normalization_request),
                &resident,
                &execution,
                &output,
                &captured,
                &completed,
                false,
            )
            .is_err()
        );

        assert!(
            validate_verified_embedding_batch(
                &request, &admitted, &resident, &execution, 3, &output, &captured, &completed,
                false,
            )
            .is_err()
        );

        let falsely_normalized_output = make_output(execution.clone(), live_evidence(), [0.8, 0.8]);
        assert!(
            validate_fixture(
                &request,
                &admitted,
                &resident,
                &execution,
                &falsely_normalized_output,
                &embedding_output_bits_sha256(&falsely_normalized_output),
                &completed,
                false,
            )
            .is_err()
        );

        let mut wrong_execution = execution.clone();
        wrong_execution.tokenizer_sha256 = "9".repeat(64);
        let wrong_fingerprint_output = make_output(wrong_execution, live_evidence(), [0.8, 0.6]);
        assert!(
            validate_fixture(
                &request,
                &admitted,
                &resident,
                &execution,
                &wrong_fingerprint_output,
                &embedding_output_bits_sha256(&wrong_fingerprint_output),
                &completed,
                false,
            )
            .is_err()
        );

        let fixture_output = make_output(
            execution.clone(),
            EmbeddingTransportEvidence::new(NativeTransport::FakeFixture, false, true)
                .expect("fixture evidence"),
            [0.8, 0.6],
        );
        assert!(
            validate_fixture(
                &request,
                &admitted,
                &resident,
                &execution,
                &fixture_output,
                &embedding_output_bits_sha256(&fixture_output),
                &completed,
                false,
            )
            .is_err()
        );

        for terminals in [
            Vec::new(),
            vec![EmbeddingCompletionTerminal::Cancelled],
            vec![
                EmbeddingCompletionTerminal::Completed,
                EmbeddingCompletionTerminal::Completed,
            ],
        ] {
            assert!(
                validate_fixture(
                    &request, &admitted, &resident, &execution, &output, &captured, &terminals,
                    false,
                )
                .is_err()
            );
        }
        assert!(
            validate_fixture(
                &request, &admitted, &resident, &execution, &output, &captured, &completed, true,
            )
            .is_err()
        );
    }

    #[test]
    fn cancelled_captured_output_cannot_be_laundered_into_authority() {
        let request = request();
        let resident = resident_fingerprint(&"a".repeat(64));
        let execution = execution_fingerprint(&resident);
        let output = make_output(execution.clone(), live_evidence(), [0.8, 0.6]);
        let error = validate_fixture(
            &request,
            &embedding_request_sha256(&request),
            &resident,
            &execution,
            &output,
            &embedding_output_bits_sha256(&output),
            &[EmbeddingCompletionTerminal::Completed],
            true,
        )
        .expect_err("late cancellation must revoke strict authority");
        assert_eq!(error.code, NativeErrorCode::Cancelled);

        let legacy = EmbeddingCompletion::completed(output.clone(), Err(error.clone()));
        assert_eq!(
            legacy
                .into_output()
                .expect("compatibility output remains explicitly diagnostic"),
            output
        );
        let strict = EmbeddingCompletion::completed(output, Err(error));
        assert_eq!(
            strict
                .into_verified()
                .expect_err("diagnostic values cannot launder a cancelled call")
                .code,
            NativeErrorCode::Cancelled
        );
    }

    #[test]
    fn public_claim_and_rejected_authority_remain_diagnostic_only() {
        let resident = resident_fingerprint(&"a".repeat(64));
        let execution = execution_fingerprint(&resident);
        let output = make_output(execution, live_evidence(), [0.8, 0.6]);
        let serialized = serde_json::to_vec(&output).expect("serialize public claim");
        let replayed: EmbeddingBatchOutput =
            serde_json::from_slice(&serialized).expect("deserialize public claim");
        let diagnostic = EmbeddingCompletion::completed(
            replayed.clone(),
            Err(embedding_verification_error("no live authority")),
        )
        .into_output()
        .expect("diagnostic wait remains compatible");
        assert_eq!(diagnostic, replayed);
        let error = EmbeddingCompletion::completed(
            replayed,
            Err(embedding_verification_error("no live authority")),
        )
        .into_verified()
        .expect_err("a public replay cannot mint a seal");
        assert_eq!(error.code, NativeErrorCode::Internal);
    }

    #[test]
    fn joined_worker_binding_is_exact_instance_identity() {
        let request = request();
        let resident = resident_fingerprint(&"a".repeat(64));
        let execution = execution_fingerprint(&resident);
        let output = make_output(execution.clone(), live_evidence(), [0.8, 0.6]);
        let request_sha256 = embedding_request_sha256(&request);
        let output_bits_sha256 = embedding_output_bits_sha256(&output);
        let worker_identity = Arc::new(WorkerIdentity);
        let evidence = VerifiedEmbeddingEvidence {
            request,
            resident_model_fingerprint: resident,
            execution_fingerprint: execution,
            request_sha256,
            output_bits_sha256,
            ledger_sha256: "3".repeat(64),
            owner_call_sequence: 0,
            worker_identity: Arc::clone(&worker_identity),
        };
        let verified = EmbeddingCompletion::completed(output, Ok(evidence))
            .into_verified()
            .expect("private evidence mints a seal");
        let joined = JoinedNativeModel {
            model_id: "embedding-model".to_string(),
            worker_identity,
            expected_workers: 0,
            joined_workers: 0,
            expected_worker_ids: Vec::new(),
            joined_worker_ids: Vec::new(),
        };
        let other = JoinedNativeModel {
            model_id: "embedding-model".to_string(),
            worker_identity: Arc::new(WorkerIdentity),
            expected_workers: 0,
            joined_workers: 0,
            expected_worker_ids: Vec::new(),
            joined_worker_ids: Vec::new(),
        };
        assert!(verified.belongs_to_joined_model(&joined));
        assert!(!verified.belongs_to_joined_model(&other));
    }

    #[cfg(unix)]
    #[test]
    fn artifact_mutation_after_output_capture_revokes_embedding_authority() {
        let directory = TestDirectory::new();
        let model_path = directory.path.join("model.gguf");
        std::fs::write(&model_path, b"immutable model bytes").expect("write model artifact");
        let model_guard =
            ModelArtifactGuard::open(&model_path, "model", None).expect("open guarded model");
        let resident = resident_fingerprint(&model_guard.expected_sha256);
        let execution = execution_fingerprint(&resident);
        let request = request();
        let output = make_output(execution.clone(), live_evidence(), [0.8, 0.6]);
        let admitted = embedding_request_sha256(&request);
        let captured = embedding_output_bits_sha256(&output);
        let artifacts = ModelArtifactGuards {
            model: model_guard,
            projector: None,
        };
        verify_embedding_batch_authority(
            request.clone(),
            &admitted,
            resident.clone(),
            execution.clone(),
            2,
            &output,
            &captured,
            0,
            &[EmbeddingCompletionTerminal::Completed],
            &AtomicBool::new(false),
            Arc::new(WorkerIdentity),
            &artifacts,
        )
        .expect("unchanged artifact permits authority");

        std::fs::write(&model_path, b"mutated!!model bytes").expect("mutate same-size artifact");
        assert!(
            verify_embedding_batch_authority(
                request,
                &admitted,
                resident,
                execution,
                2,
                &output,
                &captured,
                0,
                &[EmbeddingCompletionTerminal::Completed],
                &AtomicBool::new(false),
                Arc::new(WorkerIdentity),
                &artifacts,
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    struct TestDirectory {
        path: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl TestDirectory {
        fn new() -> Self {
            let id = TEST_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("test clock follows the Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("llama-native-embedding-authority-{nonce}-{id}",));
            std::fs::create_dir(&path).expect("create test directory");
            Self { path }
        }
    }

    #[cfg(unix)]
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
