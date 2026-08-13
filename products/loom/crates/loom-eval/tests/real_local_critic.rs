#![forbid(unsafe_code)]
#![cfg(feature = "local-critic")]

use std::{
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::Mutex,
};

use llama_native_engine::NativeModelOwner;
use llama_native_types::{NativeDevice, NativeModelConfig, SamplingConfig};
use loom_eval::{
    CandidateEvidenceSource, LocalCriterionContext, ValidatedCriterionObservation,
    consume_local_criterion_response,
};
use loom_inference::local_critic::{
    CriticConstraint, CriticConstraintKind, CriticWorkerLineage, NativeLocalCritic,
};
use loom_inference::{
    ControllerCandidateSource, ControllerMessage, ControllerMessageRole, ControllerMessageSource,
    CriticBinding, CriticChatTemplatePolicy, CriticEvaluationTask, CriticPromptSpec,
};
use loom_research_types::{NonEmptyByteRange, compile_manifest};
use loom_types::{ArtifactId, BlobId};
use sha2::{Digest, Sha256};

const MODEL_PATH: &str =
    "/Users/george/Library/Application Support/app.horary.desktop/models/gemma-4-e2b-it-q6-k.gguf";
const MODEL_BYTES: u64 = 4_501_718_688;
const MODEL_SHA256: &str = "242d9d4d3c1c9c257eaba0ab840dfe908b724ca6004c51fd1b854673db2ca831";
// Pinned from the first explicit native inspection of the exact artifact.
const TOKENIZER_SHA256: &str = "242d9d4d3c1c9c257eaba0ab840dfe908b724ca6004c51fd1b854673db2ca831";
const CANDIDATE: &str = "Mara opened the door. The lamp inside was already burning.";
static REAL_CRITIC_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
#[ignore = "requires the pinned 4.5 GB Gemma 4 instruct GGUF and a working Metal backend"]
fn real_gemma_chat_template_structured_criterion_is_exact_and_joins_cleanly() {
    let _guard = REAL_CRITIC_TEST_LOCK
        .lock()
        .expect("real critic test lock must not be poisoned");
    let model_path = PathBuf::from(MODEL_PATH);
    assert_exact_model_file(&model_path);

    let mut config = NativeModelConfig::local(model_path);
    config.model_id = "loom-real-gemma-local-critic".to_owned();
    config.expected_model_sha256 = Some(MODEL_SHA256.to_owned());
    config.device = NativeDevice::Metal;
    config.context_tokens = 2_048;
    config.batch_tokens = 512;
    config.max_sequences = 1;
    config.gpu_layers = -1;
    let owner = NativeModelOwner::load(config).expect("pinned Gemma critic must load in-process");
    let handle = owner.handle();
    let status = handle.status();
    let fingerprint = status
        .fingerprint
        .as_ref()
        .expect("loaded critic must expose a model fingerprint");
    assert_eq!(fingerprint.model_size, MODEL_BYTES);
    assert_eq!(fingerprint.model_sha256, MODEL_SHA256);
    assert_eq!(
        fingerprint.tokenizer_sha256, TOKENIZER_SHA256,
        "pin the independently inspected tokenizer contract before accepting this proof"
    );

    let result = run_live_criterion(handle);
    let joined = owner
        .shutdown_joined()
        .expect("real critic owner worker must join cleanly");
    let lineage = result.expect("real constrained critic response must validate without repair");
    lineage
        .verify_joined(&joined)
        .expect("critic response must belong to the exact joined worker");
}

struct RealCriterionCase {
    binding: CriticBinding,
    prompt: CriticPromptSpec,
    constraint: CriticConstraint,
    sampling: SamplingConfig,
    evaluation_attempt_id: ArtifactId,
    candidate_occurrence: ArtifactId,
    candidate_blob: BlobId,
    candidate_range: NonEmptyByteRange,
    packet_fingerprint: BlobId,
    rubric_fingerprint: BlobId,
    constraint_fingerprint: BlobId,
}

fn build_real_criterion_case() -> Result<RealCriterionCase, String> {
    let candidate_occurrence = ArtifactId::new();
    let candidate_blob = BlobId::digest(CANDIDATE.as_bytes());
    let candidate_range =
        NonEmptyByteRange::new(0, CANDIDATE.len() as u64).map_err(|error| error.to_string())?;
    let prefix =
        "Evaluate continuity in the exact candidate below. Cite it exactly.\n\nCANDIDATE:\n";
    let user = format!("{prefix}{CANDIDATE}");
    let message_range = NonEmptyByteRange::new(prefix.len() as u64, user.len() as u64)
        .map_err(|error| error.to_string())?;
    let messages = vec![
        ControllerMessage::new(
            ControllerMessageRole::System,
            "Return only one JSON object matching the supplied schema. Do not add commentary.",
            Vec::new(),
        )
        .map_err(|error| error.to_string())?,
        ControllerMessage::new(
            ControllerMessageRole::User,
            user,
            vec![ControllerMessageSource::new(0, message_range)],
        )
        .map_err(|error| error.to_string())?,
    ];
    let evaluation_attempt_id = ArtifactId::new();
    let packet_fingerprint = BlobId::digest(b"real-gemma-local-critic-packet-v1");
    let rubric_fingerprint = BlobId::digest(b"fiction-core-v1-continuity-real-proof");
    let prompt = CriticPromptSpec::compile(
        CriticEvaluationTask::criterion("continuity").map_err(|error| error.to_string())?,
        evaluation_attempt_id,
        messages,
        &[ControllerCandidateSource {
            occurrence_id: candidate_occurrence,
            blob_id: candidate_blob,
            utf8: CANDIDATE.as_bytes(),
            range: candidate_range,
        }],
        packet_fingerprint,
        rubric_fingerprint,
        CriticChatTemplatePolicy::ModelDefault,
    )
    .map_err(|error| error.to_string())?;
    let schema = exact_criterion_schema(
        candidate_occurrence,
        candidate_blob,
        candidate_range,
        CANDIDATE,
    );
    let constraint = CriticConstraint::new(
        CriticConstraintKind::JsonSchema,
        "real-gemma-continuity-schema-v1",
        schema,
    )
    .map_err(|error| error.to_string())?;
    let constraint_fingerprint = constraint.fingerprint();
    let sampling = SamplingConfig {
        seed: 0x5eed_2409,
        temperature: 0.2,
        top_k: 20,
        top_p: 0.9,
        min_p: 0.0,
        repeat_penalty: 1.0,
        max_tokens: 256,
        stop: Vec::new(),
        ..SamplingConfig::default()
    };
    Ok(RealCriterionCase {
        binding: compile_real_binding(),
        prompt,
        constraint,
        sampling,
        evaluation_attempt_id,
        candidate_occurrence,
        candidate_blob,
        candidate_range,
        packet_fingerprint,
        rubric_fingerprint,
        constraint_fingerprint,
    })
}

fn run_live_criterion(
    handle: llama_native_engine::NativeModelHandle,
) -> Result<CriticWorkerLineage, String> {
    let RealCriterionCase {
        binding,
        prompt,
        constraint,
        sampling,
        evaluation_attempt_id,
        candidate_occurrence,
        candidate_blob,
        candidate_range,
        packet_fingerprint,
        rubric_fingerprint,
        constraint_fingerprint,
    } = build_real_criterion_case()?;
    let critic = NativeLocalCritic::new(handle);
    let prepared = critic
        .prepare(binding, prompt)
        .map_err(|error| error.to_string())?;
    assert!(prepared.prompt_evidence().exact_token_ids().len() > 8);
    assert_ne!(
        prepared.prompt_evidence().chat_template_fingerprint(),
        BlobId::digest(b"")
    );
    let ticket = critic
        .start(prepared, constraint, &sampling)
        .map_err(|error| error.to_string())?;
    let response = critic.wait(ticket).map_err(|error| error.to_string())?;
    let mut evaluation = consume_local_criterion_response(
        response,
        LocalCriterionContext {
            evaluation_attempt_id,
            criterion_key: "continuity",
            candidate: CandidateEvidenceSource {
                occurrence_id: candidate_occurrence,
                blob_id: candidate_blob,
                utf8: CANDIDATE.as_bytes(),
            },
            candidate_range,
            evaluation_packet_fingerprint: packet_fingerprint,
            rubric_fingerprint,
            constraint_fingerprint,
        },
    )
    .map_err(|error| error.to_string())?;
    let ValidatedCriterionObservation::Scored { evidence, .. } = evaluation.observation() else {
        return Err(format!(
            "grammar-constrained output did not survive exact evidence validation: {:?}",
            evaluation.observation()
        ));
    };
    if evidence.len() != 1
        || evidence[0].candidate_occurrence_id() != candidate_occurrence
        || evidence[0].candidate_blob_id() != candidate_blob
        || evidence[0].range() != candidate_range
    {
        return Err("validated evidence changed exact candidate identity or range".to_owned());
    }
    let diagnostic = evaluation
        .response()
        .diagnostic_record()
        .to_canonical_json()
        .map_err(|error| error.to_string())?;
    if diagnostic.is_empty() {
        return Err("critic diagnostic record unexpectedly empty".to_owned());
    }
    evaluation
        .take_lineage()
        .ok_or_else(|| "live critic response lost its optional worker lineage".to_owned())
}

fn compile_real_binding() -> CriticBinding {
    let source = format!(
        r#"format = "loom.model-bindings.v1"
name = "real-gemma-local-critic"
description = "Pinned Gemma 4 instruct local critic proof"

[[bindings]]
id = "critic"
role = "critic"
model_sha256 = "{MODEL_SHA256}"
model_bytes = {MODEL_BYTES}
tokenizer_sha256 = "{TOKENIZER_SHA256}"
architecture = "gemma4"
context_tokens = 2048
capabilities = ["chat", "json_schema"]
adapters = []
"#
    );
    let manifest = compile_manifest(source.as_bytes()).expect("real critic manifest must compile");
    CriticBinding::compile(&manifest, "critic").expect("real critic binding must compile")
}

fn exact_criterion_schema(
    occurrence_id: ArtifactId,
    blob_id: BlobId,
    range: NonEmptyByteRange,
    quote: &str,
) -> String {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["criterion_key", "outcome", "evidence"],
        "properties": {
            "criterion_key": {"type": "string", "enum": ["continuity"]},
            "outcome": {
                "type": "object",
                "additionalProperties": false,
                "required": ["outcome", "score"],
                "properties": {
                    "outcome": {"type": "string", "enum": ["score"]},
                    "score": {"type": "integer", "minimum": 0, "maximum": 1_000_000}
                }
            },
            "evidence": {
                "type": "array",
                "minItems": 1,
                "maxItems": 1,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["candidate_occurrence_id", "candidate_blob_id", "range", "quote"],
                    "properties": {
                        "candidate_occurrence_id": {"type": "string", "enum": [occurrence_id.to_string()]},
                        "candidate_blob_id": {"type": "string", "enum": [blob_id.to_hex()]},
                        "range": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["start", "end"],
                            "properties": {
                                "start": {"type": "integer", "enum": [range.start()]},
                                "end": {"type": "integer", "enum": [range.end()]}
                            }
                        },
                        "quote": {"type": "string", "enum": [quote]}
                    }
                }
            }
        }
    })
    .to_string()
}

fn assert_exact_model_file(path: &Path) {
    let metadata = fs::metadata(path).expect("pinned real critic GGUF must be readable");
    assert_eq!(
        metadata.len(),
        MODEL_BYTES,
        "real critic GGUF byte length changed"
    );
    let file = File::open(path).expect("open pinned critic GGUF");
    let mut reader = BufReader::with_capacity(8 * 1024 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 8 * 1024 * 1024];
    loop {
        let read = reader.read(&mut buffer).expect("hash pinned critic GGUF");
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    assert_eq!(
        format!("{:x}", hasher.finalize()),
        MODEL_SHA256,
        "real critic GGUF digest changed"
    );
}
