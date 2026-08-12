#![forbid(unsafe_code)]
#![cfg(feature = "unstable-w1-vertical-tests")]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use llama_native_types::SamplingConfig;
use loom_backend_llama::{
    CapabilitySupport, ContinuationCase, ExactContinuationRequest, ExactContinuationResult,
    LlamaBackend, LlamaBackendError, LocalDevicePreference, LocalModelProfile, ModelRelease,
};
use loom_types::{
    ArtifactId, BlobId, BranchId, ByteRange, DocumentId, GenerationRunId, GenerationStart,
    GenerationTerminalStatus, InferenceEvidenceKind, PromptMode, PromptRecipe, RevisionId,
};
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

const BASELINE_COMMIT: &str = "949c0006b57f416190e2ae8ab84dc3a944d6b4d1";
const MANIFEST_BYTES: &[u8] = include_bytes!("../../../fixtures/w1/gemma-current-manifest-v0.json");
const INPUT_BYTES: &[u8] = include_bytes!("../../../fixtures/w1/gemma-current-input-v0.json");
const SOURCE_TREE_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/gemma-current-source-tree-v0.txt");
const EXPECTED_PROJECTION_BYTES: &[u8] =
    include_bytes!("../../../fixtures/w1/gemma-current-projection-v0.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GemmaInput {
    schema: String,
    exact_manuscript_prefix: String,
    seeds: [u32; 2],
    sampling: SamplingInput,
    required_model_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SamplingInput {
    temperature: f32,
    top_k: i32,
    top_p: f32,
    min_p: f32,
    repeat_last_n: i32,
    repeat_penalty: f32,
    max_tokens: u32,
}

struct FileChunks {
    file: File,
}

impl Iterator for FileChunks {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut bytes = vec![0_u8; 1024 * 1024];
        let count = self
            .file
            .read(&mut bytes)
            .expect("read exact Gemma fixture");
        if count == 0 {
            return None;
        }
        bytes.truncate(count);
        Some(bytes)
    }
}

fn fixture_identity<'a>(
    case: &'a platform_vertical_fixtures_v0::FixtureCaseV0,
    id: &str,
) -> &'a ArtifactIdentityV0 {
    &case
        .inputs
        .iter()
        .find(|input| input.identity.id == id)
        .unwrap_or_else(|| panic!("missing fixture identity: {id}"))
        .identity
}

fn checked_in_source_tree() -> Vec<u8> {
    let output = Command::new("git")
        .args([
            "ls-tree",
            "-r",
            BASELINE_COMMIT,
            "--",
            "crates/loom-backend-llama/src",
            "crates/loom-types/src",
        ])
        .current_dir(workspace_root())
        .output()
        .expect("read baseline production tree");
    assert!(output.status.success(), "baseline commit must be available");
    output.stdout
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_owned()
}

#[test]
fn w1_current_exact_gemma_manifest_is_authenticated() {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(MANIFEST_BYTES).expect("parse exact-Gemma manifest");
    validate_manifest(&manifest).expect("validate exact-Gemma manifest");
    let case = manifest.cases.first().expect("one exact-Gemma case");
    assert_eq!(manifest.vertical_id, VerticalIdV0::CurrentExactGemma);
    assert_eq!(case.source.commit, BASELINE_COMMIT);
    assert_eq!(
        sha256_identity("loom-current-gemma-input", INPUT_BYTES),
        *fixture_identity(case, "loom-current-gemma-input")
    );
    assert_eq!(checked_in_source_tree(), SOURCE_TREE_BYTES);
    assert_eq!(
        sha256_identity("loom-gemma-production-source-tree", SOURCE_TREE_BYTES),
        case.source.production_tree
    );
    assert_eq!(
        sha256_identity("loom-current-gemma-projection", EXPECTED_PROJECTION_BYTES),
        case.expected_projection
    );
    serde_json::from_slice::<EquivalenceProjectionV0>(EXPECTED_PROJECTION_BYTES)
        .expect("parse exact-Gemma projection");

    let unchanged = Command::new("git")
        .args([
            "diff",
            "--quiet",
            BASELINE_COMMIT,
            "--",
            "crates/loom-backend-llama/src",
            "crates/loom-types/src",
        ])
        .current_dir(workspace_root())
        .status()
        .expect("compare production sources to baseline");
    assert!(
        unchanged.success(),
        "the executable fixture must remain a test-only descendant of its production baseline"
    );
}

#[test]
#[ignore = "requires LOOM_GEMMA4_E2B_BASE_PATH and the exact W1 Gemma GGUF"]
fn w1_current_exact_gemma_baseline() -> Result<(), Box<dyn std::error::Error>> {
    let manifest: VerticalFixtureManifestV0 = serde_json::from_slice(MANIFEST_BYTES)?;
    let input: GemmaInput = serde_json::from_slice(INPUT_BYTES)?;
    let expected_projection: EquivalenceProjectionV0 =
        serde_json::from_slice(EXPECTED_PROJECTION_BYTES)?;
    let case = manifest.cases.first().expect("one exact-Gemma case");
    let prerequisite = case
        .prerequisites
        .first()
        .expect("exact Gemma prerequisite");
    assert_eq!(input.schema, "delysis.loom.current_gemma_input.v0");
    assert_eq!(
        input.required_model_sha256,
        prerequisite.identity.digest.hex
    );

    let model_path = PathBuf::from(std::env::var("LOOM_GEMMA4_E2B_BASE_PATH")?);
    let verified_model = verify_prerequisite_chunks(
        prerequisite.prerequisite_id.clone(),
        &prerequisite.identity,
        FileChunks {
            file: File::open(&model_path)?,
        },
    )?;

    let (request, profile, prompt_blob) =
        build_request(&input, &model_path, &prerequisite.identity.digest.hex)?;
    let backend = LlamaBackend::default();
    let handle = backend.start_exact_continuation(request)?;
    let result = handle.wait_timeout(Duration::from_secs(300))?;

    assert_real_result(&result, &input, &prerequisite.identity, prompt_blob);

    let (mut non_completion_request, _, _) =
        build_request(&input, &model_path, &prerequisite.identity.digest.hex)?;
    non_completion_request.request_id =
        "loom-current-exact-gemma-non-completion-rejection".to_owned();
    non_completion_request.prompt_recipe.mode = PromptMode::FillInMiddle;
    match backend.start_exact_continuation(non_completion_request) {
        Err(LlamaBackendError::InvalidRequest(message)) => {
            assert_eq!(message, "raw continuation requires PromptMode::Completion");
        }
        Err(error) => {
            return Err(format!("non-completion mode failed for the wrong reason: {error}").into());
        }
        Ok(_) => return Err("raw continuation accepted a non-completion prompt mode".into()),
    }

    let release = backend.release_model(&profile)?;
    let ModelRelease::Released { proof } = release else {
        return Err("loaded Gemma model was absent during release".into());
    };
    assert_eq!(proof.matched_slots(), proof.released_slots());
    let shutdown = backend.shutdown_joined()?;
    assert!(backend.owns_joined_runtime(&shutdown));
    let joined_workers = shutdown.joined_worker_count();
    assert_eq!(joined_workers, 1, "the production runtime owns one worker");

    let projection = build_projection(&result, &prerequisite.identity, joined_workers);
    assert_eq!(projection, expected_projection);

    let observation = ObservationEnvelopeV0 {
        schema: VERTICAL_OBSERVATION_SCHEMA_V0.to_owned(),
        vertical_id: VerticalIdV0::CurrentExactGemma,
        case_id: case.case_id.clone(),
        implementation_revision: case.source.commit.clone(),
        observed_prerequisites: case.prerequisites.clone(),
        evidence: EvidenceClaimV0 {
            schema: "delysis.evidence_claim.v0".to_owned(),
            tier: EvidenceTier::Operational,
            threat_model: "exact local GGUF execution through the Loom production backend"
                .to_owned(),
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

fn assert_real_result(
    result: &ExactContinuationResult,
    input: &GemmaInput,
    model_identity: &ArtifactIdentityV0,
    prompt_blob: BlobId,
) {
    assert_eq!(result.model.architecture.as_deref(), Some("gemma4"));
    assert_eq!(result.model.model_sha256, model_identity.digest.hex);
    assert_eq!(result.model.backend, "cpu");
    assert_eq!(
        result.model.capabilities.chat,
        CapabilitySupport::Unsupported
    );
    assert!(result.model.capabilities.completion_text.is_supported());
    assert_eq!(result.candidates.len(), 2);
    assert_eq!(
        result.candidates[0].generation.seed,
        u64::from(input.seeds[0])
    );
    assert_eq!(
        result.candidates[1].generation.seed,
        u64::from(input.seeds[1])
    );
    assert_ne!(
        result.candidates[0].generation.branch_id,
        result.candidates[1].generation.branch_id
    );
    assert!(result.candidates.iter().all(|candidate| {
        candidate.terminal.status == GenerationTerminalStatus::Completed
            && !candidate.token_trace.generated_token_ids.is_empty()
            && candidate
                .token_trace
                .provenance
                .as_ref()
                .is_some_and(|provenance| {
                    provenance.evidence_kind == InferenceEvidenceKind::LiveInference
                        && provenance.metrics.shared_prefix_tokens.unwrap_or_default() > 0
                })
            && has_at_least_four_lexical_tokens(&candidate.output_text)
    }));
    assert_eq!(result.exact_prompt_blob_id, prompt_blob);
    assert_eq!(
        result.exact_manuscript_prefix.as_bytes(),
        input.exact_manuscript_prefix.as_bytes()
    );
}

fn build_request(
    input: &GemmaInput,
    model_path: &std::path::Path,
    model_sha256: &str,
) -> Result<(ExactContinuationRequest, LocalModelProfile, BlobId), serde_json::Error> {
    let mut model = LocalModelProfile::for_gguf(model_path);
    model.expected_model_sha256 = Some(model_sha256.to_owned());
    model.device = LocalDevicePreference::Cpu;
    model.max_parallel_cases = 2;
    let prefix = input.exact_manuscript_prefix.clone();
    let prompt_blob = BlobId::digest(prefix.as_bytes());
    let document_id = DocumentId::new();
    let source_revision_id = RevisionId::new();
    let target = ByteRange::new(prefix.len() as u64, prefix.len() as u64)
        .expect("the exact prompt boundary is ordered");
    let model_environment_artifact_id = ArtifactId::new();
    let prompt_recipe_artifact_id = ArtifactId::new();
    let context_recipe_artifact_id = ArtifactId::new();
    let authority_policy_artifact_id = ArtifactId::new();
    let cases = input
        .seeds
        .iter()
        .map(|seed| {
            ContinuationCase::bind_sampling(
                GenerationStart {
                    run_id: GenerationRunId::new(),
                    branch_id: BranchId::new(),
                    document_id,
                    source_revision_id,
                    target_range: target,
                    model_environment_artifact_id,
                    prompt_recipe_artifact_id,
                    context_recipe_artifact_id,
                    authority_policy_artifact_id,
                    seed: 0,
                    sampling: serde_json::Value::Null,
                },
                SamplingConfig {
                    seed: *seed,
                    temperature: input.sampling.temperature,
                    top_k: input.sampling.top_k,
                    top_p: input.sampling.top_p,
                    min_p: input.sampling.min_p,
                    repeat_last_n: input.sampling.repeat_last_n,
                    repeat_penalty: input.sampling.repeat_penalty,
                    max_tokens: input.sampling.max_tokens,
                    ..SamplingConfig::default()
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let profile = model.clone();
    Ok((
        ExactContinuationRequest {
            request_id: "loom-current-exact-gemma".to_owned(),
            model,
            exact_manuscript_prefix: prefix,
            prompt_recipe: PromptRecipe {
                mode: PromptMode::Completion,
                exact_prompt_blob_id: prompt_blob,
                exact_prompt_token_ids: None,
                ordered_input_artifact_ids: Vec::new(),
                prompt_token_count: None,
            },
            cases,
        },
        profile,
        prompt_blob,
    ))
}

fn build_projection(
    result: &ExactContinuationResult,
    model_identity: &ArtifactIdentityV0,
    joined_workers: usize,
) -> EquivalenceProjectionV0 {
    let operation_ids = [
        "loom-current-exact-gemma-seed-41",
        "loom-current-exact-gemma-seed-42",
    ];
    let ordered_events = operation_ids
        .iter()
        .enumerate()
        .map(|(sequence, operation_id)| EventFactV0 {
            sequence: sequence as u64,
            operation_id: (*operation_id).to_owned(),
            attempt_id: None,
            correlation_id: Some("loom-current-exact-gemma".to_owned()),
            kind: "completed".to_owned(),
            payload: None,
        })
        .collect();
    let lifecycle = operation_ids
        .iter()
        .map(|operation_id| LifecycleFactV0 {
            operation_id: (*operation_id).to_owned(),
            attempt_id: None,
            correlation_id: Some("loom-current-exact-gemma".to_owned()),
            terminal: TerminalClass::Completed,
            released: true,
        })
        .collect();
    let mut output_facts = BTreeMap::new();
    output_facts.insert(
        "architecture".to_owned(),
        FactValueV0::Text("gemma4".to_owned()),
    );
    output_facts.insert(
        "backend".to_owned(),
        FactValueV0::Text(result.model.backend.clone()),
    );
    output_facts.insert("candidate_count".to_owned(), FactValueV0::Integer(2));
    output_facts.insert("chat_supported".to_owned(), FactValueV0::Boolean(false));
    output_facts.insert(
        "completion_text_supported".to_owned(),
        FactValueV0::Boolean(true),
    );
    output_facts.insert("distinct_branch_ids".to_owned(), FactValueV0::Boolean(true));
    output_facts.insert(
        "exact_model_sha256".to_owned(),
        FactValueV0::Digest(model_identity.digest.clone()),
    );
    output_facts.insert("exact_prompt_bound".to_owned(), FactValueV0::Boolean(true));
    output_facts.insert(
        "generated_token_ids_nonempty".to_owned(),
        FactValueV0::Boolean(true),
    );
    output_facts.insert("live_inference".to_owned(), FactValueV0::Boolean(true));
    output_facts.insert(
        "lexical_output_tokens_at_least_four".to_owned(),
        FactValueV0::Boolean(true),
    );
    output_facts.insert("seed_41_observed".to_owned(), FactValueV0::Boolean(true));
    output_facts.insert("seed_42_observed".to_owned(), FactValueV0::Boolean(true));
    output_facts.insert(
        "shared_prefix_tokens_positive".to_owned(),
        FactValueV0::Boolean(true),
    );
    EquivalenceProjectionV0 {
        ordered_events,
        durable_state: vec![DurableStateFactV0 {
            state_id: "exact-model-artifact".to_owned(),
            schema_id: "gguf-v3".to_owned(),
            before: Some(model_identity.clone()),
            after: Some(model_identity.clone()),
            disposition: StateDispositionV0::Unchanged,
        }],
        lifecycle,
        ownership: OwnershipFactsV0 {
            active_operations: 0,
            retained_tasks: 0,
            expected_workers: 1,
            joined_workers,
        },
        output_facts,
        fail_closed_facts: vec![
            "exact model digest is verified before runtime admission".to_owned(),
            "raw continuation rejects non-completion prompt modes before inference".to_owned(),
        ],
    }
}

fn has_at_least_four_lexical_tokens(text: &str) -> bool {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(4)
        .count()
        >= 4
}
