#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use platform_contracts_v0_vertical::{
    ArtifactIdentityV0, EvidenceClaimV0, EvidenceTier, ExecutionKind, TerminalClass,
};
use platform_vertical_fixtures_v0::{
    DurableStateFactV0, EquivalenceProjectionV0, EventFactV0, FactValueV0, LifecycleFactV0,
    ObservationEnvelopeV0, OwnershipFactsV0, StateDispositionV0, VERTICAL_OBSERVATION_SCHEMA_V0,
    VerticalFixtureManifestV0, VerticalIdV0, sha256_identity, validate_baseline, validate_manifest,
    verify_prerequisite_chunks,
};
use serde::Deserialize;

use crate::{
    CompletionPrompt, GenerationBatchRequest, GenerationCase, GenerationEventKind, GenerationInput,
    GenerationRequest, GenerationState, NativeDevice, NativeErrorCode, NativeModelConfig,
    NativeModelOwner, NativeTransport, SamplingConfig, SpecialTokenPolicy,
};

const MANIFEST_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/native-current-qwen-manifest-v0.json");
const BUNDLE_MANIFEST: &str = include_str!("../../../fixtures/w1/MANIFEST.sha256");
const INPUT_BYTES: &[u8] = include_bytes!("../../../fixtures/w1/native-current-qwen-input-v0.json");
const SOURCE_TREE_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/native-current-qwen-source-tree-v0.txt");
const EXPECTED_PROJECTION_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/native-current-qwen-projection-v0.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputFixture {
    schema: String,
    prompt: String,
    seed: u32,
    temperature: f32,
    max_tokens: u32,
    required_model_sha256: String,
}

struct FileChunks {
    file: File,
}

impl Iterator for FileChunks {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut bytes = vec![0_u8; 1024 * 1024];
        let count = self.file.read(&mut bytes).expect("read exact Qwen fixture");
        if count == 0 {
            return None;
        }
        bytes.truncate(count);
        Some(bytes)
    }
}

fn artifact_fact(identity: &ArtifactIdentityV0) -> FactValueV0 {
    FactValueV0::Digest(identity.digest.clone())
}

#[test]
fn w1_current_exact_qwen_manifest_is_authenticated() {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(MANIFEST_BYTES).expect("parse exact-Qwen manifest");
    validate_manifest(&manifest).expect("validate exact-Qwen manifest");
    let case = manifest.cases.first().expect("one exact-Qwen case");
    assert_eq!(manifest.vertical_id, VerticalIdV0::CurrentExactQwen);
    assert_eq!(
        sha256_identity("native-current-qwen-input", INPUT_BYTES),
        case.inputs[0].identity
    );
    assert_eq!(
        sha256_identity("llama-native-engine-source-tree", SOURCE_TREE_BYTES),
        case.source.production_tree
    );
    assert_eq!(
        sha256_identity("native-current-qwen-projection", EXPECTED_PROJECTION_BYTES),
        case.expected_projection
    );
    serde_json::from_slice::<EquivalenceProjectionV0>(EXPECTED_PROJECTION_BYTES)
        .expect("parse exact-Qwen projection");

    let fixtures = [
        ("native-current-qwen-input-v0.json", INPUT_BYTES),
        ("native-current-qwen-manifest-v0.json", MANIFEST_BYTES),
        (
            "native-current-qwen-projection-v0.json",
            EXPECTED_PROJECTION_BYTES,
        ),
        ("native-current-qwen-source-tree-v0.txt", SOURCE_TREE_BYTES),
    ];
    let lines = BUNDLE_MANIFEST.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), fixtures.len());
    for (line, (name, bytes)) in lines.into_iter().zip(fixtures) {
        let (expected_digest, listed_name) = line
            .split_once("  ")
            .expect("bundle manifest uses sha256sum format");
        assert_eq!(listed_name, name);
        assert_eq!(sha256_identity(name, bytes).digest.hex, expected_digest);
    }
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH, MOM_LLAMA_MODEL_SHA256, and the exact W1 Qwen GGUF"]
fn w1_current_exact_qwen_baseline() -> Result<(), Box<dyn std::error::Error>> {
    let manifest: VerticalFixtureManifestV0 = serde_json::from_slice(MANIFEST_BYTES)?;
    let input: InputFixture = serde_json::from_slice(INPUT_BYTES)?;
    let expected_projection: EquivalenceProjectionV0 =
        serde_json::from_slice(EXPECTED_PROJECTION_BYTES)?;
    let case = manifest
        .cases
        .first()
        .expect("the exact-Qwen manifest has one case");
    assert_eq!(manifest.vertical_id, VerticalIdV0::CurrentExactQwen);
    assert_eq!(input.schema, "delysis.native.current_qwen_input.v0");
    assert_eq!(
        sha256_identity("native-current-qwen-input", INPUT_BYTES),
        case.inputs[0].identity
    );
    assert_eq!(
        sha256_identity("llama-native-engine-source-tree", SOURCE_TREE_BYTES),
        case.source.production_tree
    );
    assert_eq!(
        sha256_identity("native-current-qwen-projection", EXPECTED_PROJECTION_BYTES),
        case.expected_projection
    );

    let prerequisite = case.prerequisites.first().expect("exact Qwen prerequisite");
    assert_eq!(
        input.required_model_sha256,
        prerequisite.identity.digest.hex
    );
    let model_path = PathBuf::from(std::env::var("MOM_LLAMA_MODEL_PATH")?);
    assert_eq!(
        std::env::var("MOM_LLAMA_MODEL_SHA256")?,
        prerequisite.identity.digest.hex
    );
    let verified_model = verify_prerequisite_chunks(
        prerequisite.prerequisite_id.clone(),
        &prerequisite.identity,
        FileChunks {
            file: File::open(&model_path)?,
        },
    )?;

    let mut config = NativeModelConfig::local(model_path);
    config.expected_model_sha256 = Some(prerequisite.identity.digest.hex.clone());
    config.device = NativeDevice::Cpu;
    let owner = NativeModelOwner::load(config)?;
    let handle = owner.handle();
    let status = handle.status();
    let fingerprint = status
        .fingerprint
        .as_ref()
        .expect("an admitted real model has a fingerprint");
    assert_eq!(fingerprint.model_sha256, prerequisite.identity.digest.hex);

    let prepared = handle.prepare_input(GenerationInput::Completion {
        prompts: vec![CompletionPrompt::Text {
            text: input.prompt,
            special_tokens: SpecialTokenPolicy::NoBosParseSpecial,
        }],
    })?;
    let prompt_tokens = prepared
        .first()
        .expect("one prepared exact-Qwen prompt")
        .token_ids
        .clone();
    assert!(!prompt_tokens.is_empty());

    let operation_id = "native-current-qwen";
    let request = GenerationBatchRequest {
        request_id: operation_id.to_owned(),
        model_id: status.model_id.clone(),
        cases: vec![GenerationCase {
            case_id: operation_id.to_owned(),
            input: GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Tokens {
                    token_ids: prompt_tokens,
                }],
            },
            sampling: SamplingConfig {
                seed: input.seed,
                temperature: input.temperature,
                max_tokens: input.max_tokens,
                ..SamplingConfig::default()
            },
            cached_prefix: None,
        }],
    };
    let verified = handle.generate_batch(request.clone())?.wait_verified()?;
    assert_eq!(verified.request(), &request);
    assert_eq!(verified.model_fingerprint(), fingerprint);
    let result = verified.outputs().first().expect("one exact-Qwen result");
    assert_eq!(result.state, GenerationState::Completed);
    assert!(result.real_engine_invoked);
    assert!(!result.fake_fixture);
    assert_eq!(result.transport, NativeTransport::InProcess);
    assert!(!result.text.trim().is_empty());
    let terminal_events = verified
        .events()
        .iter()
        .filter(|event| {
            event.request_id == operation_id
                && event.branch_id == operation_id
                && matches!(
                    event.event,
                    GenerationEventKind::State {
                        state: GenerationState::Completed
                    }
                )
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal_events.len(), 1);
    let active_operations = handle.status().active_sequences;
    assert_eq!(active_operations, 0);

    let fim_error = handle
        .generate(GenerationRequest {
            request_id: "native-current-qwen-fim-denied".to_owned(),
            model_id: status.model_id,
            input: GenerationInput::FillInMiddle {
                prefix: "fn main() {".to_owned(),
                suffix: "}".to_owned(),
            },
            sampling: SamplingConfig::default(),
            media: Vec::new(),
            cached_prefix: None,
        })
        .expect_err("unverified fill-in-middle must fail closed");
    assert_eq!(fim_error.code, NativeErrorCode::UnsupportedPromptForm);
    let joined = owner.shutdown_joined()?;
    assert!(joined.belongs_to(&handle));
    assert_eq!(joined.model_id(), request.model_id);
    assert_eq!(joined.expected_worker_ids(), joined.joined_worker_ids());
    assert_eq!(joined.expected_worker_count(), joined.joined_worker_count());
    let retained_tasks = joined
        .expected_worker_count()
        .saturating_sub(joined.joined_worker_count());

    let mut output_facts = BTreeMap::new();
    output_facts.insert(
        "exact_model_sha256".to_owned(),
        artifact_fact(&prerequisite.identity),
    );
    output_facts.insert("fake_fixture".to_owned(), FactValueV0::Boolean(false));
    output_facts.insert(
        "generated_text_nonempty".to_owned(),
        FactValueV0::Boolean(true),
    );
    output_facts.insert("real_engine_invoked".to_owned(), FactValueV0::Boolean(true));
    output_facts.insert(
        "transport".to_owned(),
        FactValueV0::Text("in_process".to_owned()),
    );
    output_facts.insert(
        "unsupported_fim_denied".to_owned(),
        FactValueV0::Boolean(true),
    );
    let projection = EquivalenceProjectionV0 {
        ordered_events: vec![EventFactV0 {
            sequence: 0,
            operation_id: operation_id.to_owned(),
            attempt_id: None,
            correlation_id: None,
            kind: "completed".to_owned(),
            payload: None,
        }],
        durable_state: vec![DurableStateFactV0 {
            state_id: "exact-model-artifact".to_owned(),
            schema_id: "gguf-v3".to_owned(),
            before: Some(prerequisite.identity.clone()),
            after: Some(prerequisite.identity.clone()),
            disposition: StateDispositionV0::Unchanged,
        }],
        lifecycle: vec![LifecycleFactV0 {
            operation_id: operation_id.to_owned(),
            attempt_id: None,
            correlation_id: None,
            terminal: match result.state {
                GenerationState::Completed => TerminalClass::Completed,
                _ => unreachable!("the strict seal admitted one completed result"),
            },
            released: joined.expected_worker_ids() == joined.joined_worker_ids(),
        }],
        ownership: OwnershipFactsV0 {
            active_operations,
            retained_tasks,
            expected_workers: joined.expected_worker_count(),
            joined_workers: joined.joined_worker_count(),
        },
        output_facts,
        fail_closed_facts: vec![
            "exact model digest is verified before runtime admission".to_owned(),
            "unverified fill-in-middle is rejected".to_owned(),
        ],
    };
    assert_eq!(projection, expected_projection);
    let observation = ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id: VerticalIdV0::CurrentExactQwen,
        case_id: case.case_id.clone(),
        implementation_revision: case.source.commit.clone(),
        observed_prerequisites: case.prerequisites.clone(),
        evidence: EvidenceClaimV0 {
            schema: "delysis.evidence_claim.v0".to_owned(),
            tier: EvidenceTier::Operational,
            threat_model: "exact local GGUF execution on the product owner thread".to_owned(),
            exact_source: case.source.production_tree.digest.clone(),
            exact_runtime_or_artifact: prerequisite.identity.digest.clone(),
            execution_kind: ExecutionKind::LocalRuntime,
            omitted_claims: manifest.omitted_claims.clone(),
            negative_evidence: Vec::new(),
        },
        projection,
    };
    validate_baseline(
        &manifest,
        &case.case_id,
        EXPECTED_PROJECTION_BYTES,
        &[verified_model],
        &observation,
    )?;
    Ok(())
}
