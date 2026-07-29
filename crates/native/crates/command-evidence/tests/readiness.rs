use command_evidence::{
    AcceptanceState, CommandEvidenceContext, CommandEvidenceKind, CommandEvidenceRequirement,
    EvidenceOutcome, ImplementationState, ReadinessReceiptV1, ReadinessReceiptV2, RuntimeProof,
    VersionedReadinessReceipt, derive_effective_command_readiness,
};

fn context() -> CommandEvidenceContext {
    CommandEvidenceContext {
        app_id: "mom-llama".to_string(),
        command_id: "mom_chat_send".to_string(),
        plugin_id: "llama-native".to_string(),
        plugin_version: "0.1.0".to_string(),
        source_path: "apps/mom-llama/src-tauri/src/lib.rs".to_string(),
        source_hash: "source-a".to_string(),
        runtime_fingerprint: "runtime-a".to_string(),
        engine_fingerprint: "engine-a".to_string(),
        model_fingerprint: "model-a".to_string(),
        platform: "macos-aarch64".to_string(),
    }
}

fn requirement() -> CommandEvidenceRequirement {
    CommandEvidenceRequirement {
        id: "real-smoke".to_string(),
        kind: CommandEvidenceKind::RuntimeProbe,
        reference: "verify-p0".to_string(),
        minimum_implementation: ImplementationState::HostIntegrated,
        minimum_runtime_proof: RuntimeProof::RealSmoke,
        minimum_acceptance: AcceptanceState::Unaccepted,
    }
}

fn receipt(
    receipt_context: CommandEvidenceContext,
    outcome: EvidenceOutcome,
    timestamp: &str,
) -> ReadinessReceiptV2 {
    ReadinessReceiptV2 {
        schema_version: ReadinessReceiptV2::SCHEMA_VERSION.to_string(),
        receipt_id: format!("receipt-{timestamp}"),
        requirement_id: "real-smoke".to_string(),
        context: receipt_context,
        implementation: ImplementationState::HostIntegrated,
        runtime_proof: RuntimeProof::RealSmoke,
        acceptance: AcceptanceState::Unaccepted,
        outcome,
        blockers: if outcome == EvidenceOutcome::Passed {
            Vec::new()
        } else {
            vec!["engine failed".to_string()]
        },
        timestamp: timestamp.to_string(),
    }
}

#[test]
fn missing_evidence_fails_closed() {
    let effective = derive_effective_command_readiness(&context(), &[requirement()], &[]);
    assert!(!effective.unlocked);
    assert!(
        effective
            .blockers
            .iter()
            .any(|value| value.contains("missing"))
    );
}

#[test]
fn fixture_only_proof_cannot_satisfy_real_smoke() {
    let mut fixture = receipt(context(), EvidenceOutcome::Passed, "2026-07-17T12:00:00Z");
    fixture.runtime_proof = RuntimeProof::Fixture;
    let effective = derive_effective_command_readiness(
        &context(),
        &[requirement()],
        &[VersionedReadinessReceipt::V2(Box::new(fixture))],
    );
    assert!(!effective.unlocked);
}

#[test]
fn stale_or_wrong_fingerprints_cannot_unlock() {
    let mut stale = context();
    stale.source_hash = "old-source".to_string();
    let mut wrong_runtime = context();
    wrong_runtime.runtime_fingerprint = "other-runtime".to_string();
    for candidate in [stale, wrong_runtime] {
        let effective = derive_effective_command_readiness(
            &context(),
            &[requirement()],
            &[VersionedReadinessReceipt::V2(Box::new(receipt(
                candidate,
                EvidenceOutcome::Passed,
                "2026-07-17T12:00:00Z",
            )))],
        );
        assert!(!effective.unlocked);
    }
}

#[test]
fn wrong_command_or_plugin_cannot_unlock() {
    let mut wrong_command = context();
    wrong_command.command_id = "mom_chat_regenerate".to_string();
    let mut wrong_plugin = context();
    wrong_plugin.plugin_id = "coop-runtime".to_string();
    for candidate in [wrong_command, wrong_plugin] {
        let effective = derive_effective_command_readiness(
            &context(),
            &[requirement()],
            &[VersionedReadinessReceipt::V2(Box::new(receipt(
                candidate,
                EvidenceOutcome::Passed,
                "2026-07-17T12:00:00Z",
            )))],
        );
        assert!(!effective.unlocked);
    }
}

#[test]
fn newest_matching_attempt_wins_and_demotes_an_older_pass() {
    let passed = receipt(context(), EvidenceOutcome::Passed, "2026-07-17T12:00:00Z");
    let blocked = receipt(context(), EvidenceOutcome::Blocked, "2026-07-17T12:01:00Z");
    let effective = derive_effective_command_readiness(
        &context(),
        &[requirement()],
        &[
            VersionedReadinessReceipt::V2(Box::new(passed)),
            VersionedReadinessReceipt::V2(Box::new(blocked)),
        ],
    );
    assert!(!effective.unlocked);
    assert!(
        effective
            .blockers
            .iter()
            .any(|value| value == "engine failed")
    );
}

#[test]
fn v1_receipt_is_history_only() {
    let legacy = ReadinessReceiptV1 {
        fields: serde_json::from_value(serde_json::json!({
            "app_id": "mom-llama",
            "command_id": "mom_chat_send",
            "outcome": "passed"
        }))
        .expect("legacy object"),
    };
    let effective = derive_effective_command_readiness(
        &context(),
        &[requirement()],
        &[VersionedReadinessReceipt::V1(legacy)],
    );
    assert!(!effective.unlocked);
}
