#![forbid(unsafe_code)]

use loom_store::CURRENT_SCHEMA_VERSION;
use loom_types::{
    AuthorityPolicy, CommandEnvelope, ContextRecipe, GenerationTerminalEvent,
    InferenceEvidenceKind, LoomCommand, ModelEnvironment, ProjectManifest, PromoteCandidateCommand,
    PromptMode, PromptRecipe, TokenTrace,
};

#[test]
fn project_v1_manifest_golden_file_remains_readable() {
    let manifest: ProjectManifest = serde_json::from_str(include_str!("fixtures/project-v1.json"))
        .expect("parse v1 project manifest fixture");
    assert_eq!(manifest.format, "loom-project");
    assert_eq!(manifest.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(manifest.name, "Golden Loom Project");
}

#[test]
fn generation_protocol_v1_golden_file_remains_readable() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/generation-protocol-v1.json"))
            .expect("parse generation protocol fixture");
    let environment: ModelEnvironment =
        serde_json::from_value(fixture["model_environment"].clone()).expect("model environment");
    let prompt: PromptRecipe =
        serde_json::from_value(fixture["prompt_recipe"].clone()).expect("prompt recipe");
    let context: ContextRecipe =
        serde_json::from_value(fixture["context_recipe"].clone()).expect("context recipe");
    let policy: AuthorityPolicy =
        serde_json::from_value(fixture["authority_policy"].clone()).expect("authority policy");
    let command: CommandEnvelope =
        serde_json::from_value(fixture["command_envelope"].clone()).expect("command envelope");
    let trace: TokenTrace =
        serde_json::from_value(fixture["token_trace"].clone()).expect("token trace");
    let terminal: GenerationTerminalEvent =
        serde_json::from_value(fixture["terminal_event"].clone()).expect("terminal event");
    let promotion: PromoteCandidateCommand =
        serde_json::from_value(fixture["promote_command"].clone()).expect("promotion command");

    assert_eq!(environment.backend_identifier, "llama-native-kit");
    assert_eq!(prompt.mode, PromptMode::Completion);
    assert_eq!(context.token_budget, 4_096);
    assert_eq!(policy.writer_environment_artifact_ids.len(), 1);
    assert!(matches!(command.command, LoomCommand::Weave(_)));
    assert_eq!(
        trace
            .provenance
            .expect("generation provenance")
            .evidence_kind,
        InferenceEvidenceKind::LiveInference
    );
    assert!(terminal.candidate_id.is_some());
    assert_eq!(
        promotion.expected_source_revision_id,
        context.source_revision_id
    );
}
