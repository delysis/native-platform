use mom_llama_runtime::{
    Blocker, CommandResult, ComposerAutocompleteAcceptOutput, ComposerAutocompleteAnchor,
    ComposerAutocompleteOutput,
};
use serde::Serialize;
use serde_json::Value;

fn assert_wire_fixture<T: Serialize>(fixture: &Value, result: CommandResult<T>) {
    let actual = serde_json::to_value(result).expect("serialize public command result");
    let mut expected = fixture.clone();
    // Only the receipt clock fields vary. Every status, payload and evidence
    // field consumed by the renderer is compared against actual serialization.
    for field in ["task_id", "created_at"] {
        expected["receipt"][field] = actual["receipt"][field].clone();
    }
    assert_eq!(actual, expected);
}

#[test]
fn autocomplete_renderer_fixtures_match_public_typed_serialization() {
    let source = include_str!("fixtures/composer-autocomplete-results.json");
    let fixtures: Value = serde_json::from_str(source).expect("component wire fixtures");
    let anchor = ComposerAutocompleteAnchor {
        conversation_id: "conversation".to_string(),
        active_leaf_message_id: Some("leaf".to_string()),
        execution_profile_version: 7,
        draft: "Hello 🙂".to_string(),
        selection_start_utf16: 8,
        selection_end_utf16: 8,
        attachment_ids: Vec::new(),
        model_fingerprint_sha256: "a".repeat(64),
        generation_input_sha256: "b".repeat(64),
    };
    let generation = ComposerAutocompleteOutput {
        request_id: "component-request".to_string(),
        anchor: anchor.clone(),
        suffix: " world".to_string(),
        duration_ms: 20,
    };
    let acceptance = ComposerAutocompleteAcceptOutput {
        anchor,
        message: "Hello 🙂 world".to_string(),
        updated_at: "component-fixture".to_string(),
    };
    // These are public DTO/serde fixtures, not model results or native receipts.
    // fake_fixture remains true and real_engine_invoked remains false.
    for (suffix, readiness) in [("", "host_integrated"), ("_unready", "stub_blocked")] {
        assert_wire_fixture(
            &fixtures[format!("generation{suffix}")],
            CommandResult::passed(
                "mom_llama.composer_autocomplete",
                readiness,
                generation.clone(),
                Vec::new(),
                Vec::new(),
                false,
                true,
            ),
        );
        assert_wire_fixture(
            &fixtures[format!("acceptance{suffix}")],
            CommandResult::passed(
                "mom_llama.composer_autocomplete_accept",
                readiness,
                acceptance.clone(),
                Vec::new(),
                Vec::new(),
                false,
                true,
            ),
        );
    }
    for (key, command) in [
        ("generation_blocked", "mom_llama.composer_autocomplete"),
        (
            "acceptance_blocked",
            "mom_llama.composer_autocomplete_accept",
        ),
    ] {
        assert_wire_fixture(
            &fixtures[key],
            CommandResult::<()>::blocked_with_evidence(
                command,
                "host_integrated",
                Blocker::new("component_blocked", "Owned component refusal", Vec::new()),
                Vec::new(),
                Vec::new(),
                false,
                true,
            ),
        );
    }
}
