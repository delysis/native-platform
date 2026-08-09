//! Live in-process structured critic inference.
//!
//! A critic response is evaluation input only. This module never constructs a
//! writer `ModelCall`, inference envelope, generated span, assembly, promotion
//! authority, or benchmark authority.

use std::{fmt, str::FromStr};

use llama_native_engine::{
    ControlledGenerationSubmission, ControlledGenerationTicket, JoinedNativeModel,
    NativeModelHandle, VerifiedControlledGenerationBatch, VerifiedControlledGenerationTerminal,
};
use llama_native_types::{
    ChatMessage, ChatRole, ChatTemplateChoice, ConstraintArtifactReference, ControlProgram,
    ControlledGenerationBatchRequest, ControlledGenerationCase, DistributionObservationPolicy,
    ExactTokenPrompt, ExtendedSamplerProgram, GenerationEventKind, GenerationState, NativeError,
    NativeTransport, SamplingConfig, StructuredConstraint, StructuredConstraintKind,
    TerminalSelector,
};
use loom_types::BlobId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ControllerMessageRole, ControllerPromptError, CriticBinding, CriticChatTemplatePolicy,
    CriticPromptEvidence, CriticPromptSpec, VerifiedRuntimeChargeEvidence,
    canonical::{CanonicalDigest, model_fingerprint_id, sampling_fingerprint},
    profile::ProfileError,
};

pub const MAX_CRITIC_DIAGNOSTIC_BYTES: usize = 4 * 1024 * 1024;
const DIAGNOSTIC_FORMAT: &str = "loom.local-critic-diagnostic.v1";
const REQUEST_DOMAIN: &str = "loom/local-critic-request/v1";
const RESPONSE_DOMAIN: &str = "loom/verified-local-critic-response/v1";
const DIAGNOSTIC_DOMAIN: &str = "loom/local-critic-diagnostic-record/v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriticConstraintKind {
    JsonSchema,
    Gbnf,
}

impl CriticConstraintKind {
    const fn capability(self) -> &'static str {
        match self {
            Self::JsonSchema => "json_schema",
            Self::Gbnf => "gbnf",
        }
    }

    const fn native(self) -> StructuredConstraintKind {
        match self {
            Self::JsonSchema => StructuredConstraintKind::JsonSchema,
            Self::Gbnf => StructuredConstraintKind::Gbnf,
        }
    }
}

/// One immutable structured-output body. The body is kept outside the
/// serializable native request and checked against its content reference at
/// queue admission.
pub struct CriticConstraint {
    kind: CriticConstraintKind,
    artifact_id: String,
    body: String,
    body_blob_id: BlobId,
    native: StructuredConstraint,
    fingerprint: BlobId,
}

impl fmt::Debug for CriticConstraint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CriticConstraint")
            .field("kind", &self.kind)
            .field(
                "artifact_id_blob",
                &BlobId::digest(self.artifact_id.as_bytes()),
            )
            .field("body_bytes", &self.body.len())
            .field("body_blob_id", &self.body_blob_id)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl CriticConstraint {
    pub fn new(
        kind: CriticConstraintKind,
        artifact_id: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Self, LocalCriticError> {
        let artifact_id = artifact_id.into();
        let body = body.into();
        let body_blob_id = BlobId::digest(body.as_bytes());
        let byte_len = u32::try_from(body.len()).map_err(|_| LocalCriticError::Constraint)?;
        let reference =
            ConstraintArtifactReference::new(artifact_id.clone(), body_blob_id.to_hex(), byte_len)?;
        let native = match kind {
            CriticConstraintKind::JsonSchema => StructuredConstraint::JsonSchema { reference },
            CriticConstraintKind::Gbnf => StructuredConstraint::Gbnf { reference },
        };
        let mut digest = CanonicalDigest::new("loom/local-critic-constraint/v1");
        digest.u32(match kind {
            CriticConstraintKind::JsonSchema => 1,
            CriticConstraintKind::Gbnf => 2,
        });
        digest.str(&artifact_id);
        digest.blob(body_blob_id);
        digest.u64(body.len() as u64);
        Ok(Self {
            kind,
            artifact_id,
            body,
            body_blob_id,
            native,
            fingerprint: digest.finish_blob(),
        })
    }

    pub const fn kind(&self) -> CriticConstraintKind {
        self.kind
    }

    pub const fn body_blob_id(&self) -> BlobId {
        self.body_blob_id
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone)]
pub struct NativeLocalCritic {
    handle: NativeModelHandle,
}

impl fmt::Debug for NativeLocalCritic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NativeLocalCritic { resident: <redacted> }")
    }
}

impl NativeLocalCritic {
    pub const fn new(handle: NativeModelHandle) -> Self {
        Self { handle }
    }

    pub fn prepare(
        &self,
        binding: CriticBinding,
        prompt: CriticPromptSpec,
    ) -> Result<PreparedCriticPrompt, LocalCriticError> {
        let status = self.handle.status();
        let fingerprint = status
            .fingerprint
            .ok_or(LocalCriticError::ResidentNotReady)?;
        let descriptor = status
            .descriptor
            .ok_or(LocalCriticError::ResidentNotReady)?;
        binding.verify_native_model(&fingerprint)?;
        if !descriptor.capabilities.exact.prompts.chat
            || !descriptor.capabilities.chat_template_available
        {
            return Err(LocalCriticError::NativeChatUnavailable);
        }
        let template = native_template(prompt.template_policy());
        let native_messages = prompt
            .messages()
            .iter()
            .map(|message| ChatMessage {
                role: match message.role() {
                    ControllerMessageRole::System => ChatRole::System,
                    ControllerMessageRole::User => ChatRole::User,
                    ControllerMessageRole::Assistant => ChatRole::Assistant,
                    ControllerMessageRole::Tool => ChatRole::Tool,
                },
                content: message.content().to_owned(),
            })
            .collect();
        let tokenized = self
            .handle
            .tokenize_messages_with_template(native_messages, template)?;
        let rendered_prompt_sha256 = parse_digest(&tokenized.rendered_sha256)?;
        let template_fingerprint = match prompt.template_policy() {
            CriticChatTemplatePolicy::ModelDefault => {
                parse_digest(&fingerprint.chat_template_sha256)?
            }
            CriticChatTemplatePolicy::ExactOverride(template) => {
                BlobId::digest(template.as_bytes())
            }
        };
        let prompt_evidence = CriticPromptEvidence::mint(
            prompt,
            rendered_prompt_sha256,
            tokenized.token_ids,
            template_fingerprint,
        )?;
        Ok(PreparedCriticPrompt {
            handle: self.handle.clone(),
            binding,
            prompt_evidence,
            model_fingerprint_id: model_fingerprint_id(&fingerprint),
            model_fingerprint: fingerprint,
            structured_constraints: descriptor
                .capabilities
                .exact
                .evidence
                .structured_constraints,
        })
    }

    pub fn start(
        &self,
        prepared: PreparedCriticPrompt,
        constraint: CriticConstraint,
        sampling: &SamplingConfig,
    ) -> Result<CriticTicket, LocalCriticError> {
        start_critic(prepared, constraint, sampling)
    }

    pub fn wait(&self, ticket: CriticTicket) -> Result<VerifiedCriticResponse, LocalCriticError> {
        ticket.wait()
    }
}

pub struct PreparedCriticPrompt {
    handle: NativeModelHandle,
    binding: CriticBinding,
    prompt_evidence: CriticPromptEvidence,
    model_fingerprint_id: BlobId,
    model_fingerprint: llama_native_types::ModelFingerprint,
    structured_constraints: llama_native_types::StructuredConstraintCapabilities,
}

impl fmt::Debug for PreparedCriticPrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCriticPrompt")
            .field("binding", &self.binding)
            .field("prompt_evidence", &self.prompt_evidence)
            .field("model_fingerprint_id", &self.model_fingerprint_id)
            .finish_non_exhaustive()
    }
}

impl PreparedCriticPrompt {
    pub const fn prompt_evidence(&self) -> &CriticPromptEvidence {
        &self.prompt_evidence
    }
}

pub struct CriticTicket {
    native: Option<ControlledGenerationTicket>,
    pending: Option<PendingCritic>,
}

impl fmt::Debug for CriticTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CriticTicket")
            .field("active", &self.native.is_some())
            .finish_non_exhaustive()
    }
}

impl CriticTicket {
    pub fn cancel(&self) -> bool {
        self.native
            .as_ref()
            .is_some_and(|ticket| ticket.cancel_case("critic-response"))
    }

    pub fn wait(mut self) -> Result<VerifiedCriticResponse, LocalCriticError> {
        let native = self
            .native
            .take()
            .ok_or(LocalCriticError::AlreadyConsumed)?;
        let pending = self
            .pending
            .take()
            .ok_or(LocalCriticError::AlreadyConsumed)?;
        let seal = native.wait_verified()?;
        verify_critic_response(pending, seal)
    }
}

struct PendingCritic {
    handle: NativeModelHandle,
    binding: CriticBinding,
    prompt_evidence: CriticPromptEvidence,
    model_fingerprint_id: BlobId,
    model_fingerprint: llama_native_types::ModelFingerprint,
    request: ControlledGenerationBatchRequest,
    constraint_kind: CriticConstraintKind,
    constraint_fingerprint: BlobId,
    constraint_body_blob_id: BlobId,
    sampling_fingerprint: BlobId,
}

fn start_critic(
    prepared: PreparedCriticPrompt,
    constraint: CriticConstraint,
    sampling: &SamplingConfig,
) -> Result<CriticTicket, LocalCriticError> {
    if !prepared
        .binding
        .supports_constraint(constraint.kind.capability())
        || !prepared
            .structured_constraints
            .supports(constraint.kind.native())
    {
        return Err(LocalCriticError::ConstraintCapabilityMismatch);
    }
    if !sampling.stop.is_empty() || sampling.max_tokens == 0 {
        return Err(LocalCriticError::InvalidSampling);
    }
    let identity = prepared
        .handle
        .controlled_model_identity(prepared.binding.binding_id())?;
    if identity.fingerprint() != &prepared.model_fingerprint {
        return Err(LocalCriticError::ModelIdentityMismatch);
    }
    let control = ControlProgram::new(
        identity,
        Vec::new(),
        Some(constraint.native.clone()),
        Vec::new(),
        ExtendedSamplerProgram::default(),
        TerminalSelector::Distribution,
        DistributionObservationPolicy::default(),
        Vec::new(),
    )?;
    let native_case = ControlledGenerationCase::new(
        "critic-response".to_owned(),
        ExactTokenPrompt::new(prepared.prompt_evidence.exact_token_ids().to_vec())?,
        None,
        sampling.clone(),
    )?;
    let request_id = derive_request_id(
        &prepared,
        constraint.fingerprint,
        sampling_fingerprint(sampling),
    );
    let request = ControlledGenerationBatchRequest::new(request_id, vec![native_case], control)?;
    let native = prepared
        .handle
        .generate_controlled(ControlledGenerationSubmission::new(
            request.clone(),
            Some(constraint.body),
        )?)?;
    Ok(CriticTicket {
        native: Some(native),
        pending: Some(PendingCritic {
            handle: prepared.handle,
            binding: prepared.binding,
            prompt_evidence: prepared.prompt_evidence,
            model_fingerprint_id: prepared.model_fingerprint_id,
            model_fingerprint: prepared.model_fingerprint,
            request,
            constraint_kind: constraint.kind,
            constraint_fingerprint: constraint.fingerprint,
            constraint_body_blob_id: constraint.body_blob_id,
            sampling_fingerprint: sampling_fingerprint(sampling),
        }),
    })
}

fn derive_request_id(
    prepared: &PreparedCriticPrompt,
    constraint_fingerprint: BlobId,
    sampler_fingerprint: BlobId,
) -> String {
    let mut digest = CanonicalDigest::new(REQUEST_DOMAIN);
    digest.blob(prepared.binding.fingerprint());
    digest.blob(prepared.model_fingerprint_id);
    digest.blob(prepared.prompt_evidence.compiled_fingerprint());
    digest.blob(constraint_fingerprint);
    digest.blob(sampler_fingerprint);
    format!("loom-critic-{}", digest.finish_blob().to_hex())
}

/// Move-only verified critic output, before the evaluation crate validates its
/// closed payload and exact evidence quotations.
pub struct VerifiedCriticResponse {
    evidence: VerifiedCriticResponseEvidence,
    lineage: Option<CriticWorkerLineage>,
}

impl fmt::Debug for VerifiedCriticResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedCriticResponse")
            .field("evidence", &self.evidence)
            .field("has_worker_lineage", &self.lineage.is_some())
            .finish()
    }
}

impl VerifiedCriticResponse {
    pub const fn evidence(&self) -> &VerifiedCriticResponseEvidence {
        &self.evidence
    }

    pub fn into_parts(self) -> (VerifiedCriticResponseEvidence, Option<CriticWorkerLineage>) {
        (self.evidence, self.lineage)
    }
}

/// The response facts retained when optional worker lineage is split off.
pub struct VerifiedCriticResponseEvidence {
    binding: CriticBinding,
    prompt: CriticPromptEvidence,
    raw_output: Vec<u8>,
    generated_token_ids: Vec<i32>,
    constraint_kind: CriticConstraintKind,
    constraint_fingerprint: BlobId,
    constraint_body_blob_id: BlobId,
    model_fingerprint_id: BlobId,
    sampler_fingerprint: BlobId,
    request_fingerprint: BlobId,
    native_output_fingerprint: BlobId,
    native_event_fingerprint: BlobId,
    native_runtime_ledger_fingerprint: BlobId,
    verification_fingerprint: BlobId,
    runtime_charge: VerifiedRuntimeChargeEvidence,
}

impl fmt::Debug for VerifiedCriticResponseEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedCriticResponseEvidence")
            .field("binding", &self.binding)
            .field("prompt", &self.prompt)
            .field("raw_output_bytes", &self.raw_output.len())
            .field("raw_output_blob_id", &BlobId::digest(&self.raw_output))
            .field("generated_token_count", &self.generated_token_ids.len())
            .field("constraint_kind", &self.constraint_kind)
            .field("constraint_fingerprint", &self.constraint_fingerprint)
            .field("model_fingerprint_id", &self.model_fingerprint_id)
            .field("verification_fingerprint", &self.verification_fingerprint)
            .field("runtime_charge", &self.runtime_charge)
            .finish_non_exhaustive()
    }
}

impl VerifiedCriticResponseEvidence {
    pub const fn binding(&self) -> &CriticBinding {
        &self.binding
    }

    pub const fn prompt(&self) -> &CriticPromptEvidence {
        &self.prompt
    }

    pub fn raw_output(&self) -> &[u8] {
        &self.raw_output
    }

    pub fn generated_token_ids(&self) -> &[i32] {
        &self.generated_token_ids
    }

    pub const fn constraint_kind(&self) -> CriticConstraintKind {
        self.constraint_kind
    }

    pub const fn constraint_fingerprint(&self) -> BlobId {
        self.constraint_fingerprint
    }

    pub const fn model_fingerprint_id(&self) -> BlobId {
        self.model_fingerprint_id
    }

    pub const fn verification_fingerprint(&self) -> BlobId {
        self.verification_fingerprint
    }

    pub const fn runtime_charge(&self) -> &VerifiedRuntimeChargeEvidence {
        &self.runtime_charge
    }

    pub fn diagnostic_record(&self) -> CriticDiagnosticRecord {
        let mut record = CriticDiagnosticRecord {
            format: DIAGNOSTIC_FORMAT.to_owned(),
            binding_fingerprint: self.binding.fingerprint(),
            prompt_fingerprint: self.prompt.compiled_fingerprint(),
            constraint_kind: self.constraint_kind,
            constraint_fingerprint: self.constraint_fingerprint,
            constraint_body_blob_id: self.constraint_body_blob_id,
            model_fingerprint_id: self.model_fingerprint_id,
            sampler_fingerprint: self.sampler_fingerprint,
            request_fingerprint: self.request_fingerprint,
            native_output_fingerprint: self.native_output_fingerprint,
            native_event_fingerprint: self.native_event_fingerprint,
            native_runtime_ledger_fingerprint: self.native_runtime_ledger_fingerprint,
            raw_output: self.raw_output.clone(),
            generated_token_ids: self.generated_token_ids.clone(),
            verification_fingerprint: self.verification_fingerprint,
            record_fingerprint: BlobId::digest(b"placeholder"),
        };
        record.record_fingerprint = record.derive_fingerprint();
        record
    }
}

pub struct CriticWorkerLineage {
    seal: VerifiedControlledGenerationBatch,
    handle: NativeModelHandle,
}

impl fmt::Debug for CriticWorkerLineage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CriticWorkerLineage")
            .field("owner_call_sequence", &self.seal.owner_call_sequence())
            .finish_non_exhaustive()
    }
}

impl CriticWorkerLineage {
    pub const fn owner_call_sequence(&self) -> u64 {
        self.seal.owner_call_sequence()
    }

    pub fn verify_joined(&self, joined: &JoinedNativeModel) -> Result<(), LocalCriticError> {
        if !joined.belongs_to(&self.handle) || !self.seal.belongs_to_joined_model(joined) {
            return Err(LocalCriticError::JoinedWorkerMismatch);
        }
        Ok(())
    }
}

fn verify_critic_response(
    pending: PendingCritic,
    seal: VerifiedControlledGenerationBatch,
) -> Result<VerifiedCriticResponse, LocalCriticError> {
    validate_critic_seal(&pending, &seal)?;
    let output = validate_critic_output(&pending, &seal)?;
    validate_critic_events(&pending, &seal, output.text.as_bytes())?;
    mint_verified_critic_response(pending, seal)
}

fn validate_critic_seal(
    pending: &PendingCritic,
    seal: &VerifiedControlledGenerationBatch,
) -> Result<(), LocalCriticError> {
    if seal.terminal() != VerifiedControlledGenerationTerminal::Completed
        || seal.output().request() != &pending.request
        || seal.model_fingerprint() != &pending.model_fingerprint
        || seal.output().cases().len() != 1
        || seal.terminal_sampled_token_ids().len() != 1
        || parse_digest(seal.request_sha256())?
            != parse_digest(&pending.request.fingerprint_sha256())?
    {
        return Err(LocalCriticError::NativeSealMismatch);
    }
    pending
        .binding
        .verify_native_model(seal.model_fingerprint())?;
    Ok(())
}

fn validate_critic_output<'a>(
    pending: &PendingCritic,
    seal: &'a VerifiedControlledGenerationBatch,
) -> Result<&'a llama_native_types::GenerationOutput, LocalCriticError> {
    let case = &seal.output().cases()[0];
    let output = case.generation();
    if case.case_id() != "critic-response"
        || output.request_id != pending.request.request_id()
        || output.branch_id != "critic-response"
        || output.input_index != 0
        || output.state != GenerationState::Completed
        || output.text.is_empty()
        || output.generated_token_ids.is_empty()
        || output.generated_token_ids.len() != output.metrics.completion_tokens
        || !output.real_engine_invoked
        || output.fake_fixture
        || output.transport != NativeTransport::InProcess
        || !case.distribution_observations().is_empty()
    {
        return Err(LocalCriticError::NativeOutputMismatch);
    }
    Ok(output)
}

fn validate_critic_events(
    pending: &PendingCritic,
    seal: &VerifiedControlledGenerationBatch,
    expected_output: &[u8],
) -> Result<(), LocalCriticError> {
    let mut event_text = String::new();
    let mut terminal = false;
    for (index, event) in seal.events().iter().enumerate() {
        if event.request_id != pending.request.request_id()
            || event.branch_id != "critic-response"
            || event.input_index != 0
            || event.sequence_id != 0
            || event.event_index != index as u64
            || terminal
        {
            return Err(LocalCriticError::NativeEventMismatch);
        }
        match &event.event {
            GenerationEventKind::State {
                state: GenerationState::Prefilling,
            } if index == 0 => {}
            GenerationEventKind::State {
                state: GenerationState::Generating,
            } if index == 1 => {}
            GenerationEventKind::Delta { text } if index >= 2 && !text.is_empty() => {
                event_text.push_str(text);
            }
            GenerationEventKind::State {
                state: GenerationState::Completed,
            } if index >= 2 => terminal = true,
            GenerationEventKind::State { .. }
            | GenerationEventKind::Delta { .. }
            | GenerationEventKind::Warning { .. } => {
                return Err(LocalCriticError::NativeEventMismatch);
            }
        }
    }
    if !terminal || event_text.as_bytes() != expected_output {
        return Err(LocalCriticError::NativeEventMismatch);
    }
    Ok(())
}

fn mint_verified_critic_response(
    pending: PendingCritic,
    seal: VerifiedControlledGenerationBatch,
) -> Result<VerifiedCriticResponse, LocalCriticError> {
    let output = seal.output().cases()[0].generation();
    let request_fingerprint = parse_digest(seal.request_sha256())?;
    let native_output_fingerprint = parse_digest(seal.output_sha256())?;
    let native_event_fingerprint = parse_digest(seal.event_stream_sha256())?;
    let native_runtime_ledger_fingerprint = parse_digest(seal.runtime_operation_ledger_sha256())?;
    let raw_output = output.text.as_bytes().to_vec();
    let generated_token_ids = output.generated_token_ids.clone();
    let mut verification = CanonicalDigest::new(RESPONSE_DOMAIN);
    verification.blob(pending.binding.fingerprint());
    verification.blob(pending.prompt_evidence.compiled_fingerprint());
    verification.blob(pending.constraint_fingerprint);
    verification.blob(pending.model_fingerprint_id);
    verification.blob(pending.sampling_fingerprint);
    verification.blob(request_fingerprint);
    verification.blob(native_output_fingerprint);
    verification.blob(native_event_fingerprint);
    verification.blob(native_runtime_ledger_fingerprint);
    verification.bytes(&raw_output);
    verification.u64(generated_token_ids.len() as u64);
    for token in &generated_token_ids {
        verification
            .u32(u32::try_from(*token).map_err(|_| LocalCriticError::NativeOutputMismatch)?);
    }
    let verification_fingerprint = verification.finish_blob();
    let runtime_charge = VerifiedRuntimeChargeEvidence::mint(
        output.metrics.prompt_tokens as u64,
        output.metrics.completion_tokens as u64,
        output.metrics.duration_ms,
        verification_fingerprint,
    );
    Ok(VerifiedCriticResponse {
        evidence: VerifiedCriticResponseEvidence {
            binding: pending.binding,
            prompt: pending.prompt_evidence,
            raw_output,
            generated_token_ids,
            constraint_kind: pending.constraint_kind,
            constraint_fingerprint: pending.constraint_fingerprint,
            constraint_body_blob_id: pending.constraint_body_blob_id,
            model_fingerprint_id: pending.model_fingerprint_id,
            sampler_fingerprint: pending.sampling_fingerprint,
            request_fingerprint,
            native_output_fingerprint,
            native_event_fingerprint,
            native_runtime_ledger_fingerprint,
            verification_fingerprint,
            runtime_charge,
        },
        lineage: Some(CriticWorkerLineage {
            seal,
            handle: pending.handle,
        }),
    })
}

/// Persistable diagnostics. Deserializing and checking this record never
/// recreates [`VerifiedCriticResponse`] or any evaluation authority.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriticDiagnosticRecord {
    format: String,
    binding_fingerprint: BlobId,
    prompt_fingerprint: BlobId,
    constraint_kind: CriticConstraintKind,
    constraint_fingerprint: BlobId,
    constraint_body_blob_id: BlobId,
    model_fingerprint_id: BlobId,
    sampler_fingerprint: BlobId,
    request_fingerprint: BlobId,
    native_output_fingerprint: BlobId,
    native_event_fingerprint: BlobId,
    native_runtime_ledger_fingerprint: BlobId,
    raw_output: Vec<u8>,
    generated_token_ids: Vec<i32>,
    verification_fingerprint: BlobId,
    record_fingerprint: BlobId,
}

impl fmt::Debug for CriticDiagnosticRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CriticDiagnosticRecord")
            .field("record_fingerprint", &self.record_fingerprint)
            .field("raw_output_bytes", &self.raw_output.len())
            .field("raw_output_blob_id", &BlobId::digest(&self.raw_output))
            .field("generated_token_count", &self.generated_token_ids.len())
            .finish_non_exhaustive()
    }
}

impl CriticDiagnosticRecord {
    fn derive_fingerprint(&self) -> BlobId {
        let mut digest = CanonicalDigest::new(DIAGNOSTIC_DOMAIN);
        digest.str(&self.format);
        for value in [
            self.binding_fingerprint,
            self.prompt_fingerprint,
            self.constraint_fingerprint,
            self.constraint_body_blob_id,
            self.model_fingerprint_id,
            self.sampler_fingerprint,
            self.request_fingerprint,
            self.native_output_fingerprint,
            self.native_event_fingerprint,
            self.native_runtime_ledger_fingerprint,
            self.verification_fingerprint,
        ] {
            digest.blob(value);
        }
        digest.u32(match self.constraint_kind {
            CriticConstraintKind::JsonSchema => 1,
            CriticConstraintKind::Gbnf => 2,
        });
        digest.bytes(&self.raw_output);
        digest.u64(self.generated_token_ids.len() as u64);
        for token in &self.generated_token_ids {
            digest.u32(token.cast_unsigned());
        }
        digest.finish_blob()
    }

    pub fn to_canonical_json(&self) -> Result<Vec<u8>, CriticDiagnosticError> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.is_empty() || bytes.len() > MAX_CRITIC_DIAGNOSTIC_BYTES {
            return Err(CriticDiagnosticError::Size);
        }
        Ok(bytes)
    }
}

pub struct CheckedCriticDiagnostic {
    record_fingerprint: BlobId,
    response_fingerprint: BlobId,
    raw_output_blob_id: BlobId,
}

impl fmt::Debug for CheckedCriticDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckedCriticDiagnostic")
            .field("record_fingerprint", &self.record_fingerprint)
            .field("response_fingerprint", &self.response_fingerprint)
            .field("raw_output_blob_id", &self.raw_output_blob_id)
            .finish()
    }
}

impl CheckedCriticDiagnostic {
    pub const fn record_fingerprint(&self) -> BlobId {
        self.record_fingerprint
    }
}

pub fn check_critic_diagnostic_json(
    bytes: &[u8],
) -> Result<CheckedCriticDiagnostic, CriticDiagnosticError> {
    if bytes.is_empty() || bytes.len() > MAX_CRITIC_DIAGNOSTIC_BYTES {
        return Err(CriticDiagnosticError::Size);
    }
    let record: CriticDiagnosticRecord = serde_json::from_slice(bytes)?;
    if serde_json::to_vec(&record)? != bytes {
        return Err(CriticDiagnosticError::NonCanonical);
    }
    if record.format != DIAGNOSTIC_FORMAT
        || record.raw_output.is_empty()
        || record.generated_token_ids.is_empty()
        || record.generated_token_ids.iter().any(|token| *token < 0)
        || record.derive_fingerprint() != record.record_fingerprint
    {
        return Err(CriticDiagnosticError::Invalid);
    }
    Ok(CheckedCriticDiagnostic {
        record_fingerprint: record.record_fingerprint,
        response_fingerprint: record.verification_fingerprint,
        raw_output_blob_id: BlobId::digest(&record.raw_output),
    })
}

fn native_template(policy: &CriticChatTemplatePolicy) -> ChatTemplateChoice {
    match policy {
        CriticChatTemplatePolicy::ModelDefault => ChatTemplateChoice::ModelDefault,
        CriticChatTemplatePolicy::ExactOverride(template) => {
            ChatTemplateChoice::Override(template.clone())
        }
    }
}

fn parse_digest(value: &str) -> Result<BlobId, LocalCriticError> {
    BlobId::from_str(value).map_err(|_| LocalCriticError::MalformedNativeDigest)
}

#[derive(Error)]
pub enum LocalCriticError {
    #[error(transparent)]
    Native(#[from] NativeError),
    #[error(transparent)]
    Prompt(#[from] ControllerPromptError),
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error("resident critic model is not ready with an exact descriptor")]
    ResidentNotReady,
    #[error("resident model does not expose a verified chat-template input path")]
    NativeChatUnavailable,
    #[error("structured constraint body is invalid")]
    Constraint,
    #[error("manifest and live model disagree about structured-constraint support")]
    ConstraintCapabilityMismatch,
    #[error("critic sampling must have a positive limit and no text stop sequence")]
    InvalidSampling,
    #[error("live controlled identity changed after prompt preparation")]
    ModelIdentityMismatch,
    #[error("native controlled critic seal does not match the frozen request")]
    NativeSealMismatch,
    #[error("native controlled critic output is malformed or not live in-process evidence")]
    NativeOutputMismatch,
    #[error("native controlled critic event ledger is malformed")]
    NativeEventMismatch,
    #[error("native evidence contains a malformed digest")]
    MalformedNativeDigest,
    #[error("critic ticket was already consumed")]
    AlreadyConsumed,
    #[error("critic lineage belongs to another joined worker")]
    JoinedWorkerMismatch,
}

impl fmt::Debug for LocalCriticError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[derive(Debug, Error)]
pub enum CriticDiagnosticError {
    #[error("critic diagnostic JSON is malformed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("critic diagnostic JSON is empty or exceeds its bound")]
    Size,
    #[error("critic diagnostic JSON is not canonical compact JSON")]
    NonCanonical,
    #[error("critic diagnostic invariants failed")]
    Invalid,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic() -> CriticDiagnosticRecord {
        let raw_output = br#"{"outcome":{"outcome":"abstain"}}"#.to_vec();
        let mut record = CriticDiagnosticRecord {
            format: DIAGNOSTIC_FORMAT.to_owned(),
            binding_fingerprint: BlobId::digest(b"binding"),
            prompt_fingerprint: BlobId::digest(b"prompt"),
            constraint_kind: CriticConstraintKind::JsonSchema,
            constraint_fingerprint: BlobId::digest(b"constraint"),
            constraint_body_blob_id: BlobId::digest(b"schema body"),
            model_fingerprint_id: BlobId::digest(b"model"),
            sampler_fingerprint: BlobId::digest(b"sampler"),
            request_fingerprint: BlobId::digest(b"request"),
            native_output_fingerprint: BlobId::digest(b"native output"),
            native_event_fingerprint: BlobId::digest(b"events"),
            native_runtime_ledger_fingerprint: BlobId::digest(b"runtime"),
            raw_output,
            generated_token_ids: vec![1, 2, 3],
            verification_fingerprint: BlobId::digest(b"verified response"),
            record_fingerprint: BlobId::digest(b"placeholder"),
        };
        record.record_fingerprint = record.derive_fingerprint();
        record
    }

    #[test]
    fn constraint_debug_redacts_schema_body() {
        let sentinel = r#"{"type":"object","description":"SECRET SENTINEL"}"#;
        let constraint = CriticConstraint::new(
            CriticConstraintKind::JsonSchema,
            "criterion-schema",
            sentinel,
        )
        .expect("constraint");
        let debug = format!("{constraint:?}");
        assert!(!debug.contains("SECRET SENTINEL"));
        assert_eq!(
            constraint.body_blob_id(),
            BlobId::digest(sentinel.as_bytes())
        );
    }

    #[test]
    fn diagnostic_replay_checks_canonical_bytes_but_mints_no_live_response() {
        let bytes = diagnostic()
            .to_canonical_json()
            .expect("canonical diagnostic");
        let checked = check_critic_diagnostic_json(&bytes).expect("checked diagnostic");
        assert_eq!(
            checked.record_fingerprint(),
            diagnostic().record_fingerprint
        );
        assert!(format!("{checked:?}").contains("CheckedCriticDiagnostic"));

        let mut tampered: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        tampered["raw_output"] = serde_json::json!([120]);
        let tampered = serde_json::to_vec(&tampered).expect("tampered JSON");
        assert!(check_critic_diagnostic_json(&tampered).is_err());
    }

    #[test]
    fn constraint_kind_changes_its_bound_fingerprint() {
        let body = "root ::= \"{}\"";
        let json = CriticConstraint::new(CriticConstraintKind::JsonSchema, "same-artifact", body)
            .expect("json");
        let gbnf =
            CriticConstraint::new(CriticConstraintKind::Gbnf, "same-artifact", body).expect("gbnf");
        assert_ne!(json.fingerprint(), gbnf.fingerprint());
    }
}
