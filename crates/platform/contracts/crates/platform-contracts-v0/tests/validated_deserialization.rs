use platform_contracts_v0::capability::CAPABILITY_SCHEMA_V0;
use platform_contracts_v0::error::SERVICE_ERROR_SCHEMA_V0;
use platform_contracts_v0::evidence::EVIDENCE_SCHEMA_V0;
use platform_contracts_v0::lifecycle::TERMINAL_SCHEMA_V0;
use platform_contracts_v0::publication::PUBLICATION_RECEIPT_SCHEMA_V0;
use platform_contracts_v0::shutdown::CLOSED_SUMMARY_SCHEMA_V0;
use platform_contracts_v0::{
    ArtifactIdentityV0, CapabilityEntryV0, CapabilitySnapshotV0, CapabilitySourceReportV0,
    ClosedSummaryV0, ContentDigest, DestinationIdentityV0, ErrorClass, EvidenceClaimV0,
    EvidenceTier, ExecutionKind, OperationId, PublicationOutcomeV0, PublicationReceiptV0,
    Readiness, RetryAdvice, ServiceErrorV0, ServiceId, ShutdownFailureV0, ShutdownResourceKind,
    ShutdownResourceState, ShutdownResourceV0, SupervisorPhase, TerminalClass, TerminalV0,
    TriState,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;

fn assert_decode_rejects<T: DeserializeOwned>(value: impl Into<Value>, case: &str) {
    assert!(
        serde_json::from_value::<T>(value.into()).is_err(),
        "semantic deserialization must reject {case}"
    );
}

fn digest(byte: char) -> ContentDigest {
    ContentDigest::sha256(byte.to_string().repeat(64)).expect("valid digest")
}

fn service_error(class: ErrorClass, operation_id: Option<&str>) -> ServiceErrorV0 {
    ServiceErrorV0 {
        schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
        code: if class == ErrorClass::Cancelled {
            "cancelled"
        } else {
            "worker.failed"
        }
        .to_owned(),
        class,
        retry: RetryAdvice::Never,
        operation_id: operation_id
            .map(|value| OperationId::new(value).expect("valid operation ID")),
        service: ServiceId::new("gateway").expect("valid service ID"),
        safe_detail: "safe detail".to_owned(),
    }
}

fn capability_entry() -> CapabilityEntryV0 {
    CapabilityEntryV0 {
        operation: "generate".to_owned(),
        backend_or_resource_id: "local-model".to_owned(),
        readiness: Readiness::Ready,
        limits: BTreeMap::new(),
        network: TriState::No,
        privacy_eligible: TriState::Yes,
        evidence_source: "runtime probe".to_owned(),
        evidence_outcome: "ready".to_owned(),
        observed_at_unix_ms: Some(1),
        remediation: None,
    }
}

fn artifact() -> ArtifactIdentityV0 {
    ArtifactIdentityV0 {
        id: "artifact-1".to_owned(),
        digest: digest('a'),
        length: 5,
    }
}

fn destination() -> DestinationIdentityV0 {
    DestinationIdentityV0 {
        filesystem_id: "fs-1".to_owned(),
        path_id: "destination-1".to_owned(),
    }
}

fn receipt(file_synced: bool, directory_synced: bool) -> PublicationReceiptV0 {
    PublicationReceiptV0 {
        schema: PUBLICATION_RECEIPT_SCHEMA_V0.to_owned(),
        artifact: artifact(),
        destination: destination(),
        visible: true,
        file_synced,
        directory_synced,
        idempotent_recovery: false,
    }
}

#[test]
fn service_error_and_terminal_decode_only_when_semantically_valid() {
    let mut invalid_error = service_error(ErrorClass::Worker, Some("op-1"));
    invalid_error.code = "worker..failed".to_owned();
    assert_decode_rejects::<ServiceErrorV0>(
        serde_json::to_value(invalid_error).expect("serialize"),
        "an invalid service-error code",
    );

    let terminal = TerminalV0 {
        schema: TERMINAL_SCHEMA_V0.to_owned(),
        operation_id: OperationId::new("op-1").expect("valid operation ID"),
        attempt_id: None,
        class: TerminalClass::Failed,
        error: Some(service_error(ErrorClass::Worker, Some("op-2"))),
    };
    assert_decode_rejects::<TerminalV0>(
        serde_json::to_value(terminal).expect("serialize"),
        "a terminal whose nested error names a different operation",
    );
}

#[test]
fn capability_components_and_snapshot_decode_only_when_semantically_valid() {
    let mut entry = capability_entry();
    entry.readiness = Readiness::Unknown;
    assert_decode_rejects::<CapabilityEntryV0>(
        serde_json::to_value(entry).expect("serialize"),
        "an unknown capability without remediation",
    );

    let report = CapabilitySourceReportV0 {
        source_id: String::new(),
        outcome: "ready".to_owned(),
        safe_detail: None,
    };
    assert_decode_rejects::<CapabilitySourceReportV0>(
        serde_json::to_value(report).expect("serialize"),
        "a source report with an empty source ID",
    );

    let snapshot = CapabilitySnapshotV0 {
        schema: CAPABILITY_SCHEMA_V0.to_owned(),
        snapshot_id: digest('b'),
        target: "gateway".to_owned(),
        services: BTreeMap::new(),
        reports: Vec::new(),
    };
    assert_decode_rejects::<CapabilitySnapshotV0>(
        serde_json::to_value(snapshot).expect("serialize"),
        "an empty capability snapshot",
    );
}

#[test]
fn evidence_decode_rejects_serialized_authority_inflation() {
    let claim = EvidenceClaimV0 {
        schema: EVIDENCE_SCHEMA_V0.to_owned(),
        tier: EvidenceTier::ResearchEligible,
        threat_model: "persisted data cannot recreate live authority".to_owned(),
        exact_source: digest('c'),
        exact_runtime_or_artifact: digest('d'),
        execution_kind: ExecutionKind::LocalRuntime,
        omitted_claims: Vec::new(),
        negative_evidence: Vec::new(),
    };
    assert_decode_rejects::<EvidenceClaimV0>(
        serde_json::to_value(claim).expect("serialize"),
        "a research-eligible v0 claim",
    );
}

#[test]
fn publication_components_and_outcomes_decode_only_when_semantically_valid() {
    let mut invalid_artifact = artifact();
    invalid_artifact.id.clear();
    assert_decode_rejects::<ArtifactIdentityV0>(
        serde_json::to_value(invalid_artifact).expect("serialize"),
        "an artifact with an empty ID",
    );

    let mut invalid_destination = destination();
    invalid_destination.path_id.clear();
    assert_decode_rejects::<DestinationIdentityV0>(
        serde_json::to_value(invalid_destination).expect("serialize"),
        "a destination with an empty path identity",
    );

    let mut invisible = receipt(true, true);
    invisible.visible = false;
    assert_decode_rejects::<PublicationReceiptV0>(
        serde_json::to_value(invisible).expect("serialize"),
        "an invisible publication receipt",
    );

    let not_durable = PublicationOutcomeV0::Published {
        receipt: receipt(true, false),
    };
    assert_decode_rejects::<PublicationOutcomeV0>(
        serde_json::to_value(not_durable).expect("serialize"),
        "a published outcome without directory synchronization",
    );

    let durability_with_storage_error = PublicationOutcomeV0::PublishedDurabilityUnknown {
        receipt: receipt(true, false),
        error: service_error(ErrorClass::Storage, None),
    };
    assert_decode_rejects::<PublicationOutcomeV0>(
        serde_json::to_value(durability_with_storage_error).expect("serialize"),
        "a durability-unknown outcome with a non-publication error",
    );

    let visible_file_sync_unknown = PublicationOutcomeV0::PublishedDurabilityUnknown {
        receipt: receipt(false, false),
        error: service_error(ErrorClass::Publication, None),
    };
    let decoded: PublicationOutcomeV0 = serde_json::from_value(
        serde_json::to_value(&visible_file_sync_unknown).expect("serialize"),
    )
    .expect("visible file-sync failure must decode as typed durability-unknown");
    assert_eq!(decoded, visible_file_sync_unknown);
}

#[test]
fn shutdown_components_and_summary_decode_only_when_semantically_valid() {
    let stopped_without_join = ShutdownResourceV0 {
        resource_id: "speech.host.tasks".to_owned(),
        service: ServiceId::new("speech-host").expect("valid service ID"),
        kind: ShutdownResourceKind::TaskSupervisor,
        state: ShutdownResourceState::Stopped,
        expected_workers: 2,
        joined_workers: 1,
    };
    assert_decode_rejects::<ShutdownResourceV0>(
        serde_json::to_value(stopped_without_join).expect("serialize"),
        "a stopped shutdown resource with an unjoined worker",
    );

    let failure = ShutdownFailureV0 {
        failure_id: "speech.join.1".to_owned(),
        resource_id: "speech.host.tasks".to_owned(),
        service: ServiceId::new("speech-host").expect("valid service ID"),
        error: service_error(ErrorClass::Worker, None),
    };
    assert_decode_rejects::<ShutdownFailureV0>(
        serde_json::to_value(failure).expect("serialize"),
        "a shutdown failure whose nested service does not match",
    );

    let resource = ShutdownResourceV0 {
        resource_id: "speech.host.tasks".to_owned(),
        service: ServiceId::new("speech-host").expect("valid service ID"),
        kind: ShutdownResourceKind::TaskSupervisor,
        state: ShutdownResourceState::Stopped,
        expected_workers: 2,
        joined_workers: 2,
    };
    let aggregate_mismatch = ClosedSummaryV0 {
        schema: CLOSED_SUMMARY_SCHEMA_V0.to_owned(),
        phase: SupervisorPhase::Closed,
        active_operations: 0,
        retained_tasks: 0,
        expected_workers: 3,
        joined_workers: 2,
        resources: vec![resource],
        failures: Vec::new(),
    };
    assert_decode_rejects::<ClosedSummaryV0>(
        serde_json::to_value(aggregate_mismatch).expect("serialize"),
        "a closed summary whose aggregate worker count does not match its resources",
    );
}
