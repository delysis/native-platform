//! Production authority adapters for one frozen trial.
//!
//! These adapters turn exact, live subsystem proofs into journal terminals.
//! They never accept a caller-provided output fingerprint, charge, JSON
//! receipt, or success boolean. A successful adapter retains the affine stage
//! command until [`VerifiedStageExecution::commit`] appends its terminal to the
//! exact live journal.

use std::fmt;

use loom_inference::{VerifiedBaseWriterCall, VerifiedInferenceEnvelope, VerifiedInferenceOutcome};
use loom_research_types::{
    CallEvidenceClass, CandidateAssemblyRecord, CandidateProjectionRecord,
    CompiledBaseCompletionPrompt, FrozenTrialStage, GeneratedSpanOccurrenceRecord, PromptTopology,
};
use loom_store::{
    AdmittedCandidateAssembly, AdmittedCandidateProjection, AdmittedModelCall,
    FrozenPromptSourceLease, ProjectStore, VerifiedEvaluationCandidateLease,
};
use loom_types::BlobId;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    AttemptTerminal, BudgetAmount, FrozenTrialSpec, StageCommand, TrialError, TrialJournal,
    TrialSessionId, VerifiedStageTerminalLease,
};

const STAGE_INPUT_DOMAIN: &[u8] = b"loom/frozen-stage-input/v1\0";
const DIRECT_MASK_DOMAIN: &[u8] = b"loom/direct-continuation/no-backtranslation-mask/v1\0";
const DIRECT_PLAN_DOMAIN: &[u8] = b"loom/direct-continuation/no-plan/v1\0";
const DIRECT_RETRIEVAL_DOMAIN: &[u8] = b"loom/direct-continuation/no-retrieval/v1\0";
const PURE_STAGE_EVIDENCE_DOMAIN: &[u8] = b"loom/verified-pure-stage-terminal/v1\0";

/// A verified execution which still owns the exact stage command.
///
/// The typed output is released only after the terminal is durably appended to
/// the matching live journal. Dropping this value cannot mark a stage done.
#[must_use]
pub struct VerifiedStageExecution<T> {
    command: StageCommand,
    terminal: VerifiedStageTerminalLease,
    output: T,
}

impl<T: fmt::Debug> fmt::Debug for VerifiedStageExecution<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedStageExecution")
            .field("stage", &self.command.stage())
            .field("attempt_id", &self.command.attempt_id())
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

impl<T> VerifiedStageExecution<T> {
    /// Append the exact verified terminal and release its typed output.
    pub fn commit(self, journal: &mut TrialJournal) -> Result<T, TrialError> {
        journal.finish(self.command, self.terminal)?;
        Ok(self.output)
    }
}

#[derive(Debug)]
struct StageOutputAuthority {
    _affine: AffineStageOutput,
    trial_run_id: loom_research_types::TrialRunId,
    trial_fingerprint: BlobId,
    session_id: TrialSessionId,
    stage: FrozenTrialStage,
    stage_id: loom_research_types::StageId,
    attempt_id: loom_research_types::StageAttemptId,
    output_fingerprint: BlobId,
}

impl StageOutputAuthority {
    fn into_output_fingerprint(self) -> BlobId {
        self.output_fingerprint
    }
}

/// A zero-sized drop token makes consumption of stage output authority an
/// observable ownership operation without allocating or exposing a clone path.
#[derive(Debug)]
struct AffineStageOutput;

impl Drop for AffineStageOutput {
    fn drop(&mut self) {}
}

macro_rules! deterministic_output {
    ($name:ident) => {
        #[must_use]
        pub struct $name(StageOutputAuthority);

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .field("stage", &self.0.stage)
                    .field("stage_id", &self.0.stage_id)
                    .field("output_fingerprint", &self.0.output_fingerprint)
                    .finish_non_exhaustive()
            }
        }
    };
}

deterministic_output!(FrozenInputsStageOutput);
deterministic_output!(DirectMaskStageOutput);
deterministic_output!(DirectPlanStageOutput);
deterministic_output!(DirectRetrievalStageOutput);
deterministic_output!(CompiledPromptStageOutput);

/// Move-only binding between one live generated call and the Generate stage.
#[must_use]
pub struct GeneratedStageOutput {
    authority: StageOutputAuthority,
    batch_verification_fingerprint: BlobId,
    call_verification_fingerprint: BlobId,
}

impl fmt::Debug for GeneratedStageOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeneratedStageOutput")
            .field(
                "batch_verification_fingerprint",
                &self.batch_verification_fingerprint,
            )
            .field(
                "call_verification_fingerprint",
                &self.call_verification_fingerprint,
            )
            .finish_non_exhaustive()
    }
}

/// Exact current-store call admitted for the direct single-span lane.
#[must_use]
pub struct AdmittedStageOutput {
    authority: StageOutputAuthority,
    admitted_call: AdmittedModelCall,
}

impl fmt::Debug for AdmittedStageOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdmittedStageOutput")
            .field("authority", &self.authority)
            .field("admitted_call", &self.admitted_call)
            .finish_non_exhaustive()
    }
}

/// Exact current-store single-span assembly.
#[must_use]
pub struct AssembledStageOutput {
    authority: StageOutputAuthority,
    admitted_assembly: AdmittedCandidateAssembly,
}

impl fmt::Debug for AssembledStageOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AssembledStageOutput")
            .field("authority", &self.authority)
            .field("admitted_assembly", &self.admitted_assembly)
            .finish_non_exhaustive()
    }
}

/// A Gate command bound to an exact current-store projection and candidate.
///
/// No method can produce a successful gate terminal. A future trusted hard-
/// gate adapter must consume this value together with exact source-bound state
/// and anti-copy evidence. Diagnostic gate reports cannot do so.
#[must_use]
pub struct PreparedGateStage {
    command: StageCommand,
    projection: AdmittedCandidateProjection,
    candidate: VerifiedEvaluationCandidateLease,
}

impl fmt::Debug for PreparedGateStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedGateStage")
            .field("stage_id", &self.command.stage_id())
            .field("projection", &self.projection)
            .field("candidate", &self.candidate)
            .finish_non_exhaustive()
    }
}

impl PreparedGateStage {
    pub const fn candidate_occurrence_id(&self) -> loom_types::ArtifactId {
        self.candidate.occurrence_id()
    }

    pub const fn candidate_blob_id(&self) -> BlobId {
        self.candidate.candidate_blob_id()
    }
}

#[derive(Debug, Error)]
pub enum TrialRuntimeError {
    #[error("stage adapter expected {expected:?}, received {actual:?}")]
    WrongStage {
        expected: FrozenTrialStage,
        actual: FrozenTrialStage,
    },
    #[error("stage command belongs to another frozen trial")]
    TrialMismatch,
    #[error("typed dependency authority does not match the exact stage input")]
    InputAuthorityMismatch,
    #[error("controller-free adapter requires exact direct-continuation topology")]
    DirectContinuationRequired,
    #[error("compiled prompt differs from the exact frozen trial prompt")]
    PromptMismatch,
    #[error("writer inference is diagnostic-only or does not contain exactly one completed call")]
    InferenceShape,
    #[error("single-call trial adapter cannot execute frozen writer cardinality {0}")]
    UnsupportedWriterCallCount(u16),
    #[error("writer inference project, model, tokenizer, prompt, or call scope mismatch")]
    InferenceIdentity,
    #[error("writer runtime charge is not bound to the verified call")]
    RuntimeChargeMismatch,
    #[error("writer runtime duration exceeds the trial accounting domain")]
    RuntimeDurationOverflow,
    #[error("verified writer charge exceeds its durable reservation")]
    ChargeExceedsReservation,
    #[error("store adoption returned a different batch or call cardinality")]
    AdmissionMismatch,
    #[error("store operation rejected exact live evidence")]
    Store(#[source] loom_store::StoreError),
}

impl From<loom_store::StoreError> for TrialRuntimeError {
    fn from(error: loom_store::StoreError) -> Self {
        Self::Store(error)
    }
}

pub fn execute_freeze_inputs(
    command: StageCommand,
    spec: &FrozenTrialSpec,
) -> Result<VerifiedStageExecution<FrozenInputsStageOutput>, TrialRuntimeError> {
    verify_command_header(&command, spec, FrozenTrialStage::FreezeInputs)?;
    verify_stage_input(&command, &[])?;
    let output_fingerprint = spec.fingerprint();
    let evidence = fingerprint_pure_evidence(&command, output_fingerprint, spec.fingerprint());
    Ok(successful_execution(
        command,
        BudgetAmount::default(),
        output_fingerprint,
        evidence,
        FrozenInputsStageOutput,
    ))
}

pub fn execute_direct_backtranslate_mask(
    command: StageCommand,
    spec: &FrozenTrialSpec,
    frozen: &FrozenInputsStageOutput,
) -> Result<VerifiedStageExecution<DirectMaskStageOutput>, TrialRuntimeError> {
    verify_direct_command(
        &command,
        spec,
        FrozenTrialStage::BacktranslateMask,
        &[&frozen.0],
    )?;
    let output_fingerprint = fingerprint_direct_noop(DIRECT_MASK_DOMAIN, spec, &command);
    let evidence =
        fingerprint_pure_evidence(&command, output_fingerprint, frozen.0.output_fingerprint);
    Ok(successful_execution(
        command,
        BudgetAmount::default(),
        output_fingerprint,
        evidence,
        DirectMaskStageOutput,
    ))
}

pub fn execute_direct_plan(
    command: StageCommand,
    spec: &FrozenTrialSpec,
    frozen: &FrozenInputsStageOutput,
    mask: &DirectMaskStageOutput,
) -> Result<VerifiedStageExecution<DirectPlanStageOutput>, TrialRuntimeError> {
    verify_direct_command(
        &command,
        spec,
        FrozenTrialStage::Plan,
        &[&frozen.0, &mask.0],
    )?;
    let output_fingerprint = fingerprint_direct_noop(DIRECT_PLAN_DOMAIN, spec, &command);
    let evidence =
        fingerprint_pure_evidence(&command, output_fingerprint, mask.0.output_fingerprint);
    Ok(successful_execution(
        command,
        BudgetAmount::default(),
        output_fingerprint,
        evidence,
        DirectPlanStageOutput,
    ))
}

pub fn execute_direct_retrieve(
    command: StageCommand,
    spec: &FrozenTrialSpec,
    frozen: &FrozenInputsStageOutput,
    plan: &DirectPlanStageOutput,
) -> Result<VerifiedStageExecution<DirectRetrievalStageOutput>, TrialRuntimeError> {
    verify_direct_command(
        &command,
        spec,
        FrozenTrialStage::Retrieve,
        &[&frozen.0, &plan.0],
    )?;
    let output_fingerprint = fingerprint_direct_noop(DIRECT_RETRIEVAL_DOMAIN, spec, &command);
    let evidence =
        fingerprint_pure_evidence(&command, output_fingerprint, plan.0.output_fingerprint);
    Ok(successful_execution(
        command,
        BudgetAmount::default(),
        output_fingerprint,
        evidence,
        DirectRetrievalStageOutput,
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn execute_compile_prompt(
    command: StageCommand,
    spec: &FrozenTrialSpec,
    frozen: &FrozenInputsStageOutput,
    mask: &DirectMaskStageOutput,
    plan: &DirectPlanStageOutput,
    retrieval: &DirectRetrievalStageOutput,
    prompt: &CompiledBaseCompletionPrompt,
) -> Result<VerifiedStageExecution<CompiledPromptStageOutput>, TrialRuntimeError> {
    verify_direct_command(
        &command,
        spec,
        FrozenTrialStage::CompilePrompt,
        &[&frozen.0, &mask.0, &plan.0, &retrieval.0],
    )?;
    let scope = prompt.scope();
    if prompt.project_id() != spec.project_id()
        || prompt.treatment_recipe_fingerprint() != spec.treatment_fingerprint()
        || prompt.content_fingerprint() != spec.prompt_content_fingerprint()
        || BlobId::digest(prompt.exact_bytes()) != spec.exact_prompt_blob_id()
        || u64::try_from(prompt.exact_bytes().len()).ok() != Some(spec.exact_prompt_byte_len())
        || scope.campaign_id() != spec.campaign_id()
        || scope.stage_id() != spec.generate_stage_id()
        || scope.case_id() != spec.case_id()
    {
        return Err(TrialRuntimeError::PromptMismatch);
    }
    let output_fingerprint = prompt.content_fingerprint();
    let evidence =
        fingerprint_pure_evidence(&command, output_fingerprint, spec.exact_prompt_blob_id());
    Ok(successful_execution(
        command,
        BudgetAmount::default(),
        output_fingerprint,
        evidence,
        CompiledPromptStageOutput,
    ))
}

pub fn execute_generate(
    command: StageCommand,
    spec: &FrozenTrialSpec,
    compiled: &CompiledPromptStageOutput,
    inference: &VerifiedInferenceOutcome,
) -> Result<VerifiedStageExecution<GeneratedStageOutput>, TrialRuntimeError> {
    verify_command_header(&command, spec, FrozenTrialStage::Generate)?;
    verify_stage_input(&command, &[&compiled.0])?;
    let (envelope, call) = exact_writer_call(spec, inference)?;
    verify_writer_identity(&command, spec, envelope, call)?;
    let charge_evidence = call.runtime_charge();
    let charge = verified_writer_charge(
        WriterRuntimeFacts {
            completion_tokens: charge_evidence.completion_tokens(),
            generated_token_count: call.generated_token_ids().len(),
            duration_ms: charge_evidence.duration_ms(),
            verification_fingerprint: charge_evidence.verification_fingerprint(),
            call_verification_fingerprint: call.verification_fingerprint(),
        },
        command.reservation(),
    )?;
    let batch_verification_fingerprint = envelope.verification_fingerprint();
    let call_verification_fingerprint = call.verification_fingerprint();
    Ok(successful_execution(
        command,
        charge,
        batch_verification_fingerprint,
        call_verification_fingerprint,
        |authority| GeneratedStageOutput {
            authority,
            batch_verification_fingerprint,
            call_verification_fingerprint,
        },
    ))
}

pub fn execute_admit(
    command: StageCommand,
    spec: &FrozenTrialSpec,
    generated: GeneratedStageOutput,
    inference: VerifiedInferenceOutcome,
    prompt_source: FrozenPromptSourceLease,
    store: &mut ProjectStore,
) -> Result<VerifiedStageExecution<AdmittedStageOutput>, TrialRuntimeError> {
    verify_command_header(&command, spec, FrozenTrialStage::Admit)?;
    verify_stage_input(&command, &[&generated.authority])?;
    let (envelope, call) = exact_writer_call(spec, &inference)?;
    verify_writer_identity(&command_for_generated(&generated), spec, envelope, call)?;
    if envelope.verification_fingerprint() != generated.batch_verification_fingerprint
        || call.verification_fingerprint() != generated.call_verification_fingerprint
    {
        return Err(TrialRuntimeError::InferenceIdentity);
    }
    let GeneratedStageOutput {
        authority,
        batch_verification_fingerprint,
        call_verification_fingerprint,
    } = generated;
    if authority.into_output_fingerprint() != batch_verification_fingerprint {
        return Err(TrialRuntimeError::InferenceIdentity);
    }
    let adopted = store.adopt_verified_inference(inference, prompt_source)?;
    if adopted.verification_fingerprint() != batch_verification_fingerprint
        || adopted.cancelled_call_count() != 0
    {
        return Err(TrialRuntimeError::AdmissionMismatch);
    }
    let mut calls = adopted.into_admitted_calls();
    if calls.len() != 1 {
        return Err(TrialRuntimeError::AdmissionMismatch);
    }
    let admitted_call = calls.pop().expect("one admitted call checked");
    let output_fingerprint = batch_verification_fingerprint;
    let evidence = call_verification_fingerprint;
    Ok(successful_execution(
        command,
        BudgetAmount::default(),
        output_fingerprint,
        evidence,
        |authority| AdmittedStageOutput {
            authority,
            admitted_call,
        },
    ))
}

pub fn execute_single_span_assembly(
    command: StageCommand,
    spec: &FrozenTrialSpec,
    admitted: AdmittedStageOutput,
    span: GeneratedSpanOccurrenceRecord,
    assembly: CandidateAssemblyRecord,
    store: &mut ProjectStore,
) -> Result<VerifiedStageExecution<AssembledStageOutput>, TrialRuntimeError> {
    verify_command_header(&command, spec, FrozenTrialStage::Assemble)?;
    verify_stage_input(&command, &[&admitted.authority])?;
    if assembly.parts().len() != 1 || assembly.parts()[0].span().id() != span.id() {
        return Err(TrialRuntimeError::AdmissionMismatch);
    }
    let AdmittedStageOutput {
        authority: _,
        admitted_call,
    } = admitted;
    let admitted_span = store.verify_and_record_generated_span(&admitted_call, span)?;
    let admitted_assembly =
        store.verify_and_record_candidate_assembly(assembly, &[&admitted_span])?;
    let output_fingerprint = admitted_assembly.admission_record_id().as_blob_id();
    Ok(successful_execution(
        command,
        BudgetAmount::default(),
        output_fingerprint,
        output_fingerprint,
        |authority| AssembledStageOutput {
            authority,
            admitted_assembly,
        },
    ))
}

/// Bind the Gate command to an exact current-store projection.
///
/// This intentionally does not mint a terminal.
/// `DiagnosticCoreHardGateReport`, receipt hashes, or persisted diagnostic
/// rows are insufficient gate authority.
pub fn prepare_gate_projection(
    command: StageCommand,
    spec: &FrozenTrialSpec,
    assembled: AssembledStageOutput,
    projection: CandidateProjectionRecord,
    store: &mut ProjectStore,
) -> Result<PreparedGateStage, TrialRuntimeError> {
    verify_command_header(&command, spec, FrozenTrialStage::Gate)?;
    verify_stage_input(&command, &[&assembled.authority])?;
    let AssembledStageOutput {
        authority: _,
        admitted_assembly,
    } = assembled;
    let projection =
        store.verify_and_record_candidate_projection(&admitted_assembly, projection)?;
    let candidate = store.freeze_evaluation_candidate(&projection)?;
    Ok(PreparedGateStage {
        command,
        projection,
        candidate,
    })
}

fn exact_writer_call<'a>(
    spec: &FrozenTrialSpec,
    inference: &'a VerifiedInferenceOutcome,
) -> Result<(&'a VerifiedInferenceEnvelope, &'a VerifiedBaseWriterCall), TrialRuntimeError> {
    verify_single_writer_call_spec(spec)?;
    let VerifiedInferenceOutcome::Admitted(envelope) = inference else {
        return Err(TrialRuntimeError::InferenceShape);
    };
    if envelope.outcomes().len() != 1 || envelope.cancelled_diagnostics().next().is_some() {
        return Err(TrialRuntimeError::InferenceShape);
    }
    let mut completed = envelope.completed_calls();
    let call = completed.next().ok_or(TrialRuntimeError::InferenceShape)?;
    if completed.next().is_some() {
        return Err(TrialRuntimeError::InferenceShape);
    }
    Ok((envelope, call))
}

pub(crate) fn verify_single_writer_call_spec(
    spec: &FrozenTrialSpec,
) -> Result<(), TrialRuntimeError> {
    if spec.expected_writer_call_count() != 1 {
        return Err(TrialRuntimeError::UnsupportedWriterCallCount(
            spec.expected_writer_call_count(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) struct WriterRuntimeFacts {
    pub(crate) completion_tokens: u64,
    pub(crate) generated_token_count: usize,
    pub(crate) duration_ms: u128,
    pub(crate) verification_fingerprint: BlobId,
    pub(crate) call_verification_fingerprint: BlobId,
}

pub(crate) fn verified_writer_charge(
    facts: WriterRuntimeFacts,
    reservation: BudgetAmount,
) -> Result<BudgetAmount, TrialRuntimeError> {
    if facts.verification_fingerprint != facts.call_verification_fingerprint
        || u64::try_from(facts.generated_token_count).ok() != Some(facts.completion_tokens)
    {
        return Err(TrialRuntimeError::RuntimeChargeMismatch);
    }
    let wall_time_ms =
        u64::try_from(facts.duration_ms).map_err(|_| TrialRuntimeError::RuntimeDurationOverflow)?;
    let charge = BudgetAmount::new(facts.completion_tokens, 0, 0, wall_time_ms)
        .map_err(|_| TrialRuntimeError::ChargeExceedsReservation)?;
    if !charge.fits_within(reservation) {
        return Err(TrialRuntimeError::ChargeExceedsReservation);
    }
    Ok(charge)
}

/// Reconstruct the original Generate command identity from its output without
/// accepting any caller fields.
fn command_for_generated(generated: &GeneratedStageOutput) -> StageCommandView<'_> {
    StageCommandView { generated }
}

struct StageCommandView<'a> {
    generated: &'a GeneratedStageOutput,
}

impl StageCommandView<'_> {
    fn stage_id(&self) -> loom_research_types::StageId {
        self.generated.authority.stage_id
    }

    fn attempt_id(&self) -> loom_research_types::StageAttemptId {
        self.generated.authority.attempt_id
    }
}

trait GenerateCommandIdentity {
    fn stage_id(&self) -> loom_research_types::StageId;
    fn attempt_id(&self) -> loom_research_types::StageAttemptId;
}

impl GenerateCommandIdentity for StageCommand {
    fn stage_id(&self) -> loom_research_types::StageId {
        self.stage_id()
    }

    fn attempt_id(&self) -> loom_research_types::StageAttemptId {
        self.attempt_id()
    }
}

impl GenerateCommandIdentity for StageCommandView<'_> {
    fn stage_id(&self) -> loom_research_types::StageId {
        self.stage_id()
    }

    fn attempt_id(&self) -> loom_research_types::StageAttemptId {
        self.attempt_id()
    }
}

fn verify_writer_identity<C: GenerateCommandIdentity>(
    command: &C,
    spec: &FrozenTrialSpec,
    envelope: &VerifiedInferenceEnvelope,
    call: &VerifiedBaseWriterCall,
) -> Result<(), TrialRuntimeError> {
    let prompt = envelope.prompt_evidence();
    let identity = call.model_call().identity();
    let scope = identity.scope();
    if envelope.project_id() != spec.project_id()
        || envelope.binding().fingerprint() != spec.model_binding_fingerprint()
        || envelope.binding().model_sha256() != spec.model_fingerprint()
        || envelope.binding().tokenizer_sha256() != spec.tokenizer_fingerprint()
        || prompt.project_id() != spec.project_id()
        || prompt.content_fingerprint() != spec.prompt_content_fingerprint()
        || prompt.raw_blob_id() != spec.exact_prompt_blob_id()
        || u64::try_from(prompt.raw_utf8().len()).ok() != Some(spec.exact_prompt_byte_len())
        || identity.tokenizer_fingerprint() != spec.tokenizer_fingerprint()
        || identity.prompt_fingerprint() != prompt.compiled_fingerprint()
        || scope.campaign_id() != spec.campaign_id()
        || scope.stage_id() != spec.generate_stage_id()
        || scope.attempt_id() != command.attempt_id()
        || scope.case_id() != spec.case_id()
        || command.stage_id() != spec.generate_stage_id()
        || call.model_call().evidence_class() != CallEvidenceClass::LiveBaseWriterClaim
    {
        return Err(TrialRuntimeError::InferenceIdentity);
    }
    Ok(())
}

fn verify_direct_command(
    command: &StageCommand,
    spec: &FrozenTrialSpec,
    expected: FrozenTrialStage,
    dependencies: &[&StageOutputAuthority],
) -> Result<(), TrialRuntimeError> {
    if spec.prompt_topology() != PromptTopology::ExactDirectContinuation {
        return Err(TrialRuntimeError::DirectContinuationRequired);
    }
    verify_command_header(command, spec, expected)?;
    verify_stage_input(command, dependencies)
}

fn verify_command_header(
    command: &StageCommand,
    spec: &FrozenTrialSpec,
    expected: FrozenTrialStage,
) -> Result<(), TrialRuntimeError> {
    if command.stage() != expected {
        return Err(TrialRuntimeError::WrongStage {
            expected,
            actual: command.stage(),
        });
    }
    if command.trial_fingerprint() != spec.fingerprint() {
        return Err(TrialRuntimeError::TrialMismatch);
    }
    Ok(())
}

fn verify_stage_input(
    command: &StageCommand,
    dependencies: &[&StageOutputAuthority],
) -> Result<(), TrialRuntimeError> {
    if dependencies.iter().any(|dependency| {
        dependency.trial_run_id != command.trial_run_id
            || dependency.trial_fingerprint != command.trial_fingerprint
            || dependency.session_id != command.session_id
    }) {
        return Err(TrialRuntimeError::InputAuthorityMismatch);
    }
    let mut digest = Sha256::new();
    digest.update(STAGE_INPUT_DOMAIN);
    digest.update(command.trial_run_id.as_ulid().to_bytes());
    digest.update(command.trial_fingerprint.as_bytes());
    digest.update(command.stage_id.as_ulid().to_bytes());
    digest.update(command.stage_spec_fingerprint.as_bytes());
    digest.update((dependencies.len() as u64).to_be_bytes());
    for dependency in dependencies {
        digest.update(dependency.stage_id.as_ulid().to_bytes());
        digest.update(dependency.output_fingerprint.as_bytes());
    }
    if BlobId::from_bytes(digest.finalize().into()) != command.input_fingerprint {
        return Err(TrialRuntimeError::InputAuthorityMismatch);
    }
    Ok(())
}

fn successful_execution<T>(
    command: StageCommand,
    actual_charge: BudgetAmount,
    output_fingerprint: BlobId,
    live_terminal_evidence_fingerprint: BlobId,
    output: impl FnOnce(StageOutputAuthority) -> T,
) -> VerifiedStageExecution<T> {
    let authority = StageOutputAuthority {
        _affine: AffineStageOutput,
        trial_run_id: command.trial_run_id,
        trial_fingerprint: command.trial_fingerprint,
        session_id: command.session_id,
        stage: command.stage,
        stage_id: command.stage_id,
        attempt_id: command.attempt_id,
        output_fingerprint,
    };
    let terminal = VerifiedStageTerminalLease {
        trial_run_id: command.trial_run_id,
        trial_fingerprint: command.trial_fingerprint,
        session_id: command.session_id,
        attempt_id: command.attempt_id,
        stage_id: command.stage_id,
        command_fingerprint: command.command_fingerprint,
        start_event_fingerprint: command.start_event_fingerprint,
        terminal: AttemptTerminal::Succeeded { output_fingerprint },
        actual_charge,
        live_terminal_evidence_fingerprint,
    };
    VerifiedStageExecution {
        command,
        terminal,
        output: output(authority),
    }
}

fn fingerprint_direct_noop(
    domain: &[u8],
    spec: &FrozenTrialSpec,
    command: &StageCommand,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(spec.fingerprint().as_bytes());
    digest.update(spec.treatment_fingerprint().as_bytes());
    digest.update(command.input_fingerprint.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_pure_evidence(
    command: &StageCommand,
    output_fingerprint: BlobId,
    exact_artifact_fingerprint: BlobId,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(PURE_STAGE_EVIDENCE_DOMAIN);
    digest.update(command.command_fingerprint.as_bytes());
    digest.update(command.start_event_fingerprint.as_bytes());
    digest.update(output_fingerprint.as_bytes());
    digest.update(exact_artifact_fingerprint.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}
