#![forbid(unsafe_code)]
#![cfg(feature = "native-llama")]

use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use llama_native_engine::{NativeModelHandle, NativeModelOwner};
use llama_native_types::{NativeDevice, NativeModelConfig, SamplingConfig};
use loom_inference::{
    BaseWriterBackend, BaseWriterBinding, ControlledBaseWriterBackend, ExactEmbeddingBackend,
    PersistedBindingEvidenceRef, PersistedCaseOutcomeRef, PersistedInferenceBatchRef,
    PersistedInferenceCaseRef, PersistedPromptEvidenceRef, VerifiedBaseWriterCall,
    VerifiedInferenceEnvelope, VerifiedInferenceOutcome,
    native_controlled::{
        ControlledBaseWriterCaseSpec, ControlledInferenceError, EmbeddingNormalizationMode,
        EmbeddingPoolingMode, ExactEmbeddingInput, ExactEmbeddingRequest, NativeControlProgram,
        VerifiedEmbeddingDiagnostic,
    },
    native_llama::{BaseWriterCaseSpec, InferenceError, NativeLlamaWriter, PreparedBaseCompletion},
    verify_persisted_batch_evidence,
};
use loom_research_types::{
    CallEvidenceClass, CallScope, CampaignId, CompiledBaseCompletionPrompt, CompletionPromptTail,
    ExactPromptSource, FrozenBaseCompletionPrompt, ModelCallId, NonEmptyByteRange, StageAttemptId,
    StageId, TrialCaseId, compile_manifest,
};
use loom_types::{BlobId, ProjectId, RevisionId};

const REAL_MODEL_ENV: &str = "MOM_LLAMA_MODEL_PATH";
const MODEL_SHA256: &str = "9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14";
const MODEL_BYTES: u64 = 484_220_320;
const EXACT_PROMPT: &str = "The rain had stopped before Mara reached the house. She put her hand on the door and listened.";
static REAL_MODEL_TEST_LOCK: Mutex<()> = Mutex::new(());

fn compile_binding(model_sha256: &str) -> BaseWriterBinding {
    let source = format!(
        r#"format = "loom.model-bindings.v1"
name = "qwen-real-bridge-proof"
description = "Pinned small base writer for an explicit real-model bridge proof"

[[bindings]]
id = "writer"
role = "base_writer"
model_sha256 = "{model_sha256}"
model_bytes = {MODEL_BYTES}
tokenizer_sha256 = "{MODEL_SHA256}"
architecture = "qwen3"
context_tokens = 512
capabilities = ["completion"]
adapters = []
"#
    );
    let manifest = compile_manifest(source.as_bytes()).expect("real model manifest must compile");
    BaseWriterBinding::compile(&manifest, "writer")
        .expect("real base-writer binding must compile from the intact manifest")
}

fn scope() -> CallScope {
    CallScope::new(
        CampaignId::new(),
        StageId::new(),
        StageAttemptId::new(),
        TrialCaseId::new(),
    )
}

fn compiled_prompt(project_id: ProjectId) -> CompiledBaseCompletionPrompt {
    let revision_id = RevisionId::new();
    let bytes = EXACT_PROMPT.as_bytes();
    let tail = CompletionPromptTail::live_manuscript(
        revision_id,
        BlobId::digest(bytes),
        NonEmptyByteRange::new(0, bytes.len() as u64).expect("nonempty real prompt tail"),
    )
    .expect("bounded real prompt tail");
    FrozenBaseCompletionPrompt::new(
        project_id,
        scope(),
        BlobId::digest(b"real-direct-continuation-treatment-v1"),
        Vec::new(),
        tail,
    )
    .expect("real direct-continuation prompt specification")
    .compile(
        bytes.to_vec(),
        &[ExactPromptSource::new(revision_id, bytes)],
    )
    .expect("exact source-bound real prompt")
}

fn sampling(seed: u32) -> SamplingConfig {
    SamplingConfig {
        seed,
        temperature: 0.7,
        top_k: 20,
        top_p: 0.9,
        min_p: 0.05,
        repeat_penalty: 1.05,
        max_tokens: 16,
        ..SamplingConfig::default()
    }
}

fn smoke_sampling(seed: u32) -> SamplingConfig {
    SamplingConfig {
        max_tokens: 2,
        ..sampling(seed)
    }
}

fn real_model_path() -> PathBuf {
    env::var_os(REAL_MODEL_ENV).map_or_else(
        || {
            panic!(
                "{REAL_MODEL_ENV} must name the explicitly approved {MODEL_SHA256} GGUF for this ignored test"
            )
        },
        PathBuf::from,
    )
}

fn load_real_writer() -> (
    PathBuf,
    NativeModelOwner,
    NativeModelHandle,
    NativeLlamaWriter,
) {
    let model_path = real_model_path();
    let metadata = fs::metadata(&model_path).expect("real GGUF must be readable");
    assert_eq!(metadata.len(), MODEL_BYTES, "real GGUF byte length changed");
    let mut config = NativeModelConfig::local(model_path.clone());
    "loom-qwen-real-bridge-proof".clone_into(&mut config.model_id);
    config.expected_model_sha256 = Some(MODEL_SHA256.to_owned());
    config.device = NativeDevice::Cpu;
    config.context_tokens = 512;
    config.batch_tokens = 128;
    config.max_sequences = 2;
    config.gpu_layers = 0;

    let owner = NativeModelOwner::load(config).expect("pinned real GGUF must load in-process");
    let handle = owner.handle();
    let writer = NativeLlamaWriter::new(handle.clone());
    (model_path, owner, handle, writer)
}

fn prepare_exact_prompt(
    writer: &NativeLlamaWriter,
    project_id: ProjectId,
) -> (PreparedBaseCompletion, BlobId, BlobId, Vec<u32>, String) {
    let binding = compile_binding(MODEL_SHA256);
    let binding_fingerprint = binding.fingerprint();
    let manifest_fingerprint = binding.manifest_fingerprint();
    let prompt = compiled_prompt(project_id);

    let prepared = writer
        .prepare_completion(binding, prompt)
        .expect("live resident must match the compiled binding");
    assert_eq!(prepared.project_id(), project_id);
    assert_eq!(prepared.binding().fingerprint(), binding_fingerprint);
    assert_eq!(
        prepared.binding().manifest_fingerprint(),
        manifest_fingerprint
    );
    assert_eq!(prepared.exact_prompt_utf8(), EXACT_PROMPT.as_bytes());
    assert_eq!(
        prepared.exact_prompt_blob_id(),
        BlobId::digest(EXACT_PROMPT.as_bytes())
    );
    assert!(!prepared.exact_prompt_token_ids().is_empty());
    assert_eq!(
        prepared.prompt_token_count(),
        prepared.exact_prompt_token_ids().len()
    );
    assert_ne!(
        prepared.prompt_fingerprint(),
        prepared.exact_prompt_blob_id()
    );
    let exact_prompt_tokens = prepared.exact_prompt_token_ids().to_vec();
    let prompt_fingerprint = prepared.prompt_fingerprint();
    let prepared_debug = format!("{prepared:?}");
    (
        prepared,
        binding_fingerprint,
        prompt_fingerprint,
        exact_prompt_tokens,
        prepared_debug,
    )
}

fn run_ordered_batch(
    writer: &NativeLlamaWriter,
    prepared: PreparedBaseCompletion,
) -> ([ModelCallId; 2], VerifiedInferenceOutcome, String) {
    let first_call_id = ModelCallId::new();
    let second_call_id = ModelCallId::new();
    let cases = vec![
        BaseWriterCaseSpec::new(first_call_id, sampling(17_011))
            .expect("first real case must compile"),
        BaseWriterCaseSpec::new(second_call_id, sampling(17_012))
            .expect("second real case must compile"),
    ];
    let ticket = writer
        .start(prepared, cases)
        .expect("real ordered batch must start");
    assert!(ticket.request_id().starts_with("loom-base-v1-"));
    let ticket_debug = format!("{ticket:?}");
    let outcome = writer
        .wait(ticket)
        .expect("real owner-worker seal must verify");
    ([first_call_id, second_call_id], outcome, ticket_debug)
}

fn run_single_legacy(
    writer: &NativeLlamaWriter,
    project_id: ProjectId,
    seed: u32,
) -> VerifiedInferenceOutcome {
    let prepared = writer
        .prepare_completion(compile_binding(MODEL_SHA256), compiled_prompt(project_id))
        .expect("legacy exact completion must prepare");
    let ticket = writer
        .start(
            prepared,
            vec![
                BaseWriterCaseSpec::new(ModelCallId::new(), smoke_sampling(seed))
                    .expect("legacy case must compile"),
            ],
        )
        .expect("legacy exact completion must start");
    writer
        .wait(ticket)
        .expect("legacy owner-worker seal must verify")
}

fn run_single_disabled_control(
    writer: &NativeLlamaWriter,
    project_id: ProjectId,
    seed: u32,
) -> loom_inference::native_controlled::VerifiedControlledInference {
    let prepared = writer
        .prepare_controlled_completion(compile_binding(MODEL_SHA256), compiled_prompt(project_id))
        .expect("controlled exact completion must prepare");
    let case = ControlledBaseWriterCaseSpec::new(ModelCallId::new(), smoke_sampling(seed), None)
        .expect("controlled case must compile");
    let control = NativeControlProgram::disabled(writer, "writer")
        .expect("disabled control must bind to the live writer");
    let ticket = writer
        .start_controlled(prepared, vec![case], control)
        .expect("disabled controlled completion must start");
    writer
        .wait_controlled(ticket)
        .expect("controlled owner-worker seal must verify before worker join")
}

fn single_completed(outcome: &VerifiedInferenceOutcome) -> &VerifiedBaseWriterCall {
    let VerifiedInferenceOutcome::Admitted(envelope) = outcome else {
        panic!("real completed call unexpectedly became diagnostic-only");
    };
    let mut completed = envelope.completed_calls();
    let call = completed.next().expect("one completed real call");
    assert!(completed.next().is_none(), "exactly one real call expected");
    call
}

fn terminal_signature(call: &VerifiedBaseWriterCall) -> (String, String, Option<i32>) {
    let audit: serde_json::Value = serde_json::from_slice(call.backend_audit_json())
        .expect("backend terminal evidence must be JSON");
    (
        audit["output"]["state"]
            .as_str()
            .expect("terminal state")
            .to_owned(),
        audit["output"]["finish_reason"]
            .as_str()
            .expect("finish reason")
            .to_owned(),
        call.terminal_sampled_token_id(),
    )
}

fn assert_disabled_control_is_exact_legacy(
    legacy: &VerifiedBaseWriterCall,
    controlled: &VerifiedBaseWriterCall,
) {
    for call in [legacy, controlled] {
        assert_eq!(
            call.runtime_charge().verification_fingerprint(),
            call.verification_fingerprint()
        );
        assert_eq!(
            usize::try_from(call.runtime_charge().completion_tokens())
                .expect("bounded completion token charge fits usize"),
            call.generated_token_ids().len()
        );
        assert!(call.runtime_charge().prompt_tokens() > 0);
    }
    assert_eq!(
        legacy.generated_token_ids(),
        controlled.generated_token_ids(),
        "disabled control changed exact sampled token IDs"
    );
    assert_eq!(
        legacy.raw_output(),
        controlled.raw_output(),
        "disabled control changed raw decoded bytes"
    );
    assert_eq!(
        legacy.displayed_output(),
        controlled.displayed_output(),
        "disabled control changed displayed text"
    );
    assert_eq!(
        terminal_signature(legacy),
        terminal_signature(controlled),
        "disabled control changed the terminal class or stop evidence"
    );
    assert_eq!(
        legacy.output_projection(),
        controlled.output_projection(),
        "disabled control changed raw/display/stop projection geometry"
    );
}

fn assert_exact_ordered_evidence(
    envelope: &VerifiedInferenceEnvelope,
    project_id: ProjectId,
    binding_fingerprint: BlobId,
    prompt_fingerprint: BlobId,
    exact_prompt_tokens: &[u32],
    expected_call_ids: [ModelCallId; 2],
) -> Vec<String> {
    assert_eq!(envelope.project_id(), project_id);
    assert_eq!(envelope.binding().fingerprint(), binding_fingerprint);
    assert_eq!(
        envelope.prompt_evidence().raw_utf8(),
        EXACT_PROMPT.as_bytes()
    );
    assert_eq!(
        envelope.prompt_evidence().raw_blob_id(),
        BlobId::digest(EXACT_PROMPT.as_bytes())
    );
    assert_eq!(
        envelope.prompt_evidence().ordered_token_ids(),
        exact_prompt_tokens
    );
    assert_eq!(
        envelope.prompt_evidence().compiled_fingerprint(),
        prompt_fingerprint
    );
    assert_eq!(envelope.outcomes().len(), 2);
    assert_eq!(envelope.outcomes()[0].input_index(), 0);
    assert_eq!(envelope.outcomes()[1].input_index(), 1);

    let completed = envelope.completed_calls().collect::<Vec<_>>();
    assert_eq!(completed.len(), 2);
    for (index, call) in completed.iter().enumerate() {
        assert_eq!(call.model_call().id(), expected_call_ids[index]);
        assert_eq!(
            call.model_call().evidence_class(),
            CallEvidenceClass::LiveBaseWriterClaim
        );
        assert_eq!(
            call.model_call().identity().prompt_fingerprint(),
            prompt_fingerprint
        );
        assert!(!call.raw_output().is_empty());
        assert!(!call.generated_token_ids().is_empty());
        assert_eq!(call.displayed_output(), call.raw_output());
        assert!(call.output_projection().is_some());
        call.model_call()
            .completed()
            .expect("real completed call must have a completed terminal")
            .token_evidence()
            .verify(call.generated_token_ids())
            .expect("real generated token evidence must be exact");

        let audit: serde_json::Value = serde_json::from_slice(call.backend_audit_json())
            .expect("backend audit must be exact JSON");
        assert_eq!(audit["output"]["input_index"], index);
        assert_eq!(audit["output"]["real_engine_invoked"], true);
        assert_eq!(audit["output"]["fake_fixture"], false);
        assert_eq!(audit["output"]["transport"], "in_process");
        assert_eq!(
            audit["exact_prompt_blob_id"],
            BlobId::digest(EXACT_PROMPT.as_bytes()).to_hex()
        );
        let events: serde_json::Value = serde_json::from_slice(call.event_json())
            .expect("retained event ledger must be exact JSON");
        assert_eq!(events["input_index"], index);
        assert!(
            events["events"]
                .as_array()
                .is_some_and(|events| events.len() >= 3)
        );
    }

    completed
        .iter()
        .map(|call| String::from_utf8_lossy(call.raw_output()).into_owned())
        .collect()
}

fn assert_real_evidence_strictly_replays(envelope: &VerifiedInferenceEnvelope) {
    let cases = envelope
        .outcomes()
        .iter()
        .map(|outcome| {
            let call = outcome
                .completed_call()
                .expect("real proof expects both cases to complete");
            PersistedInferenceCaseRef {
                input_index: outcome.input_index(),
                model_call: call.model_call(),
                raw_output: call.raw_output(),
                generated_token_ids: call.generated_token_ids(),
                event_json: call.event_json(),
                backend_audit_json: call.backend_audit_json(),
                terminal_sampled_token_id: call.terminal_sampled_token_id(),
                outcome: PersistedCaseOutcomeRef::Completed {
                    displayed_output: call.displayed_output(),
                    output_projection: call.output_projection(),
                },
                verification_fingerprint: call.verification_fingerprint(),
            }
        })
        .collect::<Vec<_>>();
    let binding = envelope.binding();
    let prompt = envelope.prompt_evidence();
    let runtime_model_fingerprint = cases[0].model_call.identity().model_fingerprint();
    let checked = verify_persisted_batch_evidence(&PersistedInferenceBatchRef {
        binding: PersistedBindingEvidenceRef {
            binding_id: binding.binding_id(),
            binding_fingerprint: binding.fingerprint(),
            model_sha256: binding.model_sha256(),
            model_byte_len: binding.model_bytes(),
            tokenizer_sha256: binding.tokenizer_sha256(),
            multimodal_projector_sha256: binding.multimodal_projector_sha256(),
            context_tokens: binding.context_tokens(),
        },
        prompt: PersistedPromptEvidenceRef {
            project_id: prompt.project_id(),
            scope: prompt.scope(),
            source_prompt_fingerprint: prompt.source_prompt_fingerprint(),
            content_fingerprint: prompt.content_fingerprint(),
            treatment_recipe_fingerprint: prompt.treatment_recipe_fingerprint(),
            raw_utf8: prompt.raw_utf8(),
            raw_blob_id: prompt.raw_blob_id(),
            form: prompt.form(),
            token_policy: prompt.token_policy(),
            ordered_token_ids: prompt.ordered_token_ids(),
            token_fingerprint: prompt.token_fingerprint(),
            compiled_fingerprint: prompt.compiled_fingerprint(),
        },
        runtime_model_fingerprint,
        backend_request_id: envelope.backend_request_id(),
        cases: &cases,
        verification_fingerprint: envelope.verification_fingerprint(),
    })
    .expect("real producer receipts must strictly replay");
    assert_eq!(checked.completed_case_count(), 2);
    assert_eq!(checked.cancelled_case_count(), 0);
    assert_eq!(checked.cases().len(), 2);
}

fn assert_debug_redacted(rendered_values: &[String], model_path: &Path, outputs: &[String]) {
    let model_path_text = model_path.to_string_lossy();
    for rendered in rendered_values {
        assert!(!rendered.contains(EXACT_PROMPT), "Debug leaked the prompt");
        assert!(
            !rendered.contains(model_path_text.as_ref()),
            "Debug leaked the model path"
        );
        for output in outputs {
            assert!(
                output.is_empty() || !rendered.contains(output),
                "Debug leaked generated prose"
            );
        }
    }
}

fn assert_wrong_binding_fails_closed(
    writer: &NativeLlamaWriter,
    handle: &NativeModelHandle,
    project_id: ProjectId,
    model_path: &Path,
) {
    assert_eq!(handle.status().active_sequences, 0);
    let wrong_binding = compile_binding(&"0".repeat(64));
    let wrong_prompt = compiled_prompt(project_id);
    let error = writer
        .prepare_completion(wrong_binding, wrong_prompt)
        .expect_err("a compiled binding for different model bytes must fail closed");
    assert!(matches!(error, InferenceError::Profile(_)));
    assert_eq!(handle.status().active_sequences, 0);
    let error_debug = format!("{error:?}");
    assert!(!error_debug.contains(EXACT_PROMPT));
    assert!(!error_debug.contains(model_path.to_string_lossy().as_ref()));
}

#[test]
#[ignore = "requires an explicitly supplied, pinned real GGUF and genuine in-process inference"]
fn real_small_gguf_mints_ordered_exact_writer_evidence_and_rejects_wrong_binding() {
    let _guard = REAL_MODEL_TEST_LOCK.lock().expect("real-model test lock");
    let (model_path, owner, handle, writer) = load_real_writer();
    let project_id = ProjectId::new();
    let writer_debug = format!("{writer:?}");
    let (prepared, binding_fingerprint, prompt_fingerprint, prompt_tokens, prepared_debug) =
        prepare_exact_prompt(&writer, project_id);
    let (expected_call_ids, outcome, ticket_debug) = run_ordered_batch(&writer, prepared);
    let outcome_debug = format!("{outcome:?}");
    let VerifiedInferenceOutcome::Admitted(envelope) = outcome else {
        panic!("completed real batch unexpectedly became diagnostic-only");
    };
    assert_real_evidence_strictly_replays(&envelope);
    let generated_outputs = assert_exact_ordered_evidence(
        &envelope,
        project_id,
        binding_fingerprint,
        prompt_fingerprint,
        &prompt_tokens,
        expected_call_ids,
    );
    let envelope_debug = format!("{envelope:?}");
    assert_debug_redacted(
        &[
            writer_debug,
            prepared_debug,
            ticket_debug,
            outcome_debug,
            envelope_debug,
        ],
        &model_path,
        &generated_outputs,
    );
    assert_wrong_binding_fails_closed(&writer, &handle, project_id, &model_path);
    owner
        .shutdown_joined()
        .expect("real native writer owner must join cleanly");
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH to name the pinned Qwen 0.6B GGUF"]
fn real_qwen_disabled_control_embeddings_and_multi_call_lineage_are_exact() {
    let _guard = REAL_MODEL_TEST_LOCK.lock().expect("real-model test lock");
    let (_model_path, owner, _handle, writer) = load_real_writer();
    let project_id = ProjectId::new();
    let seed = 71_337;

    // Admission is per owner-worker seal, not delayed until campaign teardown.
    let legacy = run_single_legacy(&writer, project_id, seed);
    let first = run_single_disabled_control(&writer, project_id, seed);
    let second = run_single_disabled_control(&writer, project_id, seed);
    assert!(first.owner_call_sequence() < second.owner_call_sequence());
    assert_disabled_control_is_exact_legacy(
        single_completed(&legacy),
        single_completed(first.inference()),
    );
    assert_disabled_control_is_exact_legacy(
        single_completed(&legacy),
        single_completed(second.inference()),
    );

    let exact_tokens = legacy_prompt_tokens(&legacy);
    let embedding = run_and_verify_real_embedding(&writer, &exact_tokens, 0);

    let (first_inference, first_lineage) = first.into_parts();
    let (second_inference, second_lineage) = second.into_parts();
    let (embedding_data, embedding_lineage) = embedding.into_parts();
    assert!(matches!(
        first_inference,
        VerifiedInferenceOutcome::Admitted(_)
    ));
    assert!(matches!(
        second_inference,
        VerifiedInferenceOutcome::Admitted(_)
    ));

    // A joined token for another load of identical model bytes is not the
    // owner-worker that minted these seals.
    let (_wrong_path, wrong_owner, _wrong_handle, _wrong_writer) = load_real_writer();
    let wrong_joined = wrong_owner
        .shutdown_joined()
        .expect("wrong comparison worker must join cleanly");
    assert!(matches!(
        first_lineage.verify_joined_worker(&wrong_joined),
        Err(ControlledInferenceError::JoinedWorkerMismatch)
    ));
    assert!(matches!(
        embedding_lineage.verify_joined_worker(&wrong_joined),
        Err(ControlledInferenceError::JoinedWorkerMismatch)
    ));

    // One eventual join binds every retained call lineage from the original
    // resident; none of this changes their already-admitted status.
    let joined = owner
        .shutdown_joined()
        .expect("campaign resident must join exactly once");
    let first_proof = first_lineage
        .bind_joined(&joined)
        .expect("first controlled lineage belongs to joined campaign worker");
    let second_proof = second_lineage
        .bind_joined(&joined)
        .expect("second controlled lineage belongs to joined campaign worker");
    let embedding_proof = embedding_lineage
        .bind_joined(&joined)
        .expect("embedding lineage belongs to joined campaign worker");
    assert!(first_proof.owner_call_sequence() < second_proof.owner_call_sequence());
    assert_eq!(embedding_proof.owner_call_sequence(), 0);
    assert_eq!(
        first_proof.owner_call_sequence(),
        embedding_proof.owner_call_sequence()
    );
    assert_eq!(embedding_data.outputs()[0].exact_token_ids(), exact_tokens);
}

fn run_and_verify_real_embedding(
    writer: &NativeLlamaWriter,
    exact_tokens: &[u32],
    expected_owner_call_sequence: u64,
) -> VerifiedEmbeddingDiagnostic {
    let embedding_request = ExactEmbeddingRequest::new(
        compile_binding(MODEL_SHA256),
        vec![
            ExactEmbeddingInput::new("exact-manuscript-tail", exact_tokens.to_vec())
                .expect("bounded exact-token embedding input"),
        ],
        EmbeddingPoolingMode::None,
        EmbeddingNormalizationMode::None,
    )
    .expect("bounded exact embedding request");
    let embedding_ticket = writer
        .start_embeddings(embedding_request)
        .expect("real diagnostic embedding must start on the same resident");
    let embedding = writer
        .wait_embeddings(embedding_ticket)
        .expect("real embedding seal must verify before worker join");
    let embedding_record = embedding
        .canonical_diagnostic_record()
        .expect("verified embedding must encode as bounded canonical diagnostics");
    let replayed =
        loom_inference::native_controlled::verify_persisted_embedding_diagnostic(&embedding_record)
            .expect("canonical real embedding diagnostics must strictly replay");
    assert_eq!(
        embedding.owner_call_sequence(),
        expected_owner_call_sequence,
        "embedding calls use an independent zero-based owner sequence"
    );
    assert_eq!(embedding.outputs().len(), 1);
    assert_eq!(embedding.outputs()[0].exact_token_ids(), exact_tokens);
    assert_eq!(
        embedding.outputs()[0].row_count() as usize,
        exact_tokens.len()
    );
    assert_eq!(
        embedding.outputs()[0].values().len(),
        exact_tokens.len() * embedding.dimensions() as usize
    );
    assert!(
        embedding.outputs()[0]
            .values()
            .iter()
            .all(|value| value.is_finite())
    );
    assert_eq!(replayed.outputs().len(), 1);
    assert_eq!(replayed.outputs()[0].exact_token_ids(), exact_tokens);
    assert_eq!(
        replayed.output_bits_fingerprint(),
        embedding.output_bits_fingerprint()
    );
    embedding
}

fn legacy_prompt_tokens(outcome: &VerifiedInferenceOutcome) -> Vec<u32> {
    let VerifiedInferenceOutcome::Admitted(envelope) = outcome else {
        panic!("real legacy call must be admitted");
    };
    envelope.prompt_evidence().ordered_token_ids().to_vec()
}
