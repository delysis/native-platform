use platform_contracts_v0::{
    CapabilitySnapshotV0, ClosedSummaryV0, DataTierV0, EvidenceClaimV0, EvidenceTier,
    ExecutionKind, PrivacyDecisionV0, PrivacyDenialV0, PrivacyPolicyV0, ProviderId,
    PublicationOutcomeV0, RedactionStateV0, RoutePrivacyContextV0, RouteTargetV0, ServiceErrorV0,
    TerminalV0,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const FIXTURE_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/v0");

#[derive(Clone, Copy)]
struct Golden {
    name: &'static str,
    bytes: &'static [u8],
}

macro_rules! golden {
    ($name:literal) => {
        Golden {
            name: $name,
            bytes: include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/v0/",
                $name
            )),
        }
    };
}

const TERMINAL_COMPLETED: Golden = golden!("terminal-completed.json");
const TERMINAL_CANCELLED: Golden = golden!("terminal-cancelled.json");
const SERVICE_ERROR: Golden = golden!("service-error.json");
const CAPABILITY_KNOWN: Golden = golden!("capability-known.json");
const CAPABILITY_UNKNOWN: Golden = golden!("capability-unknown.json");
const EVIDENCE_OPERATIONAL: Golden = golden!("evidence-operational.json");
const EVIDENCE_REPRODUCIBLE: Golden = golden!("evidence-reproducible.json");
const SHUTDOWN_SUCCESS: Golden = golden!("shutdown-success.json");
const SHUTDOWN_MULTIPLE_FAILURES: Golden = golden!("shutdown-multiple-failures.json");
const PUBLICATION_NOT_PUBLISHED: Golden = golden!("publication-not-published.json");
const PUBLICATION_PUBLISHED: Golden = golden!("publication-published.json");
const PUBLICATION_DURABILITY_UNKNOWN: Golden = golden!("publication-durability-unknown.json");
const PUBLICATION_VISIBLE_FILE_SYNC_UNKNOWN: Golden =
    golden!("publication-visible-file-sync-unknown.json");
const PRIVACY_POLICY_LOCAL_ONLY: Golden = golden!("privacy-policy-local-only.json");
const PRIVACY_POLICY_HOSTED_EXPLICIT: Golden = golden!("privacy-policy-hosted-explicit.json");

const ALL_GOLDENS: [Golden; 15] = [
    CAPABILITY_KNOWN,
    CAPABILITY_UNKNOWN,
    EVIDENCE_OPERATIONAL,
    EVIDENCE_REPRODUCIBLE,
    PRIVACY_POLICY_HOSTED_EXPLICIT,
    PRIVACY_POLICY_LOCAL_ONLY,
    PUBLICATION_DURABILITY_UNKNOWN,
    PUBLICATION_VISIBLE_FILE_SYNC_UNKNOWN,
    PUBLICATION_NOT_PUBLISHED,
    PUBLICATION_PUBLISHED,
    SERVICE_ERROR,
    SHUTDOWN_MULTIPLE_FAILURES,
    SHUTDOWN_SUCCESS,
    TERMINAL_CANCELLED,
    TERMINAL_COMPLETED,
];

fn deserialize_validate_round_trip<T>(golden: Golden, validate: impl FnOnce(&T)) -> T
where
    T: DeserializeOwned + Serialize,
{
    let source: Value = serde_json::from_slice(golden.bytes).expect("golden fixture must be JSON");
    assert_no_authority_inflation(&source, golden.name);

    let typed: T = serde_json::from_slice(golden.bytes).unwrap_or_else(|error| {
        panic!(
            "{} must deserialize to its precise v0 type: {error}",
            golden.name
        )
    });
    validate(&typed);

    let serialized = serde_json::to_value(&typed).unwrap_or_else(|error| {
        panic!("{} must reserialize after validation: {error}", golden.name)
    });
    assert_eq!(
        serialized, source,
        "{} must round-trip with semantic JSON equality",
        golden.name
    );
    typed
}

fn assert_no_authority_inflation(value: &Value, fixture: &str) {
    match value {
        Value::Object(object) => {
            for forbidden in [
                "authority",
                "foreground_command",
                "live_authority",
                "provider_credential",
            ] {
                assert!(
                    !object.contains_key(forbidden),
                    "{fixture} must not mint fixture-only authority through `{forbidden}`"
                );
            }
            for nested in object.values() {
                assert_no_authority_inflation(nested, fixture);
            }
        }
        Value::Array(array) => {
            for nested in array {
                assert_no_authority_inflation(nested, fixture);
            }
        }
        _ => {}
    }
}

#[test]
fn terminal_goldens_match_the_wire_contract() {
    let completed = deserialize_validate_round_trip::<TerminalV0>(TERMINAL_COMPLETED, |value| {
        value.validate().expect("completed terminal must validate");
    });
    assert!(completed.error.is_none());

    let cancelled = deserialize_validate_round_trip::<TerminalV0>(TERMINAL_CANCELLED, |value| {
        value.validate().expect("cancelled terminal must validate");
    });
    let nested_operation = cancelled
        .error
        .as_ref()
        .expect("cancelled terminal must carry an error")
        .operation_id
        .as_ref();
    assert_eq!(
        nested_operation,
        Some(&cancelled.operation_id),
        "nested service error must identify the enclosing operation"
    );
}

#[test]
fn service_error_golden_matches_the_wire_contract() {
    deserialize_validate_round_trip::<ServiceErrorV0>(SERVICE_ERROR, |value| {
        value.validate().expect("service error must validate");
    });
}

#[test]
fn capability_goldens_match_the_wire_contract() {
    for golden in [CAPABILITY_KNOWN, CAPABILITY_UNKNOWN] {
        deserialize_validate_round_trip::<CapabilitySnapshotV0>(golden, |value| {
            value.validate().expect("capability snapshot must validate");
        });
    }
}

#[test]
fn evidence_goldens_match_the_wire_contract_without_inflation() {
    let operational =
        deserialize_validate_round_trip::<EvidenceClaimV0>(EVIDENCE_OPERATIONAL, |value| {
            value
                .validate()
                .expect("operational evidence must validate");
        });
    assert_eq!(operational.execution_kind, ExecutionKind::Fixture);
    assert_eq!(operational.tier, EvidenceTier::Operational);

    let reproducible =
        deserialize_validate_round_trip::<EvidenceClaimV0>(EVIDENCE_REPRODUCIBLE, |value| {
            value
                .validate()
                .expect("reproducible evidence must validate");
        });
    assert_eq!(reproducible.execution_kind, ExecutionKind::LocalRuntime);
    assert_eq!(reproducible.tier, EvidenceTier::Reproducible);
}

#[test]
fn shutdown_goldens_match_the_wire_contract() {
    let success = deserialize_validate_round_trip::<ClosedSummaryV0>(SHUTDOWN_SUCCESS, |value| {
        value.validate().expect("closed summary must validate");
    });
    assert!(success.succeeded());
    assert_eq!(success.expected_workers, success.joined_workers);
    assert_eq!(success.resources.len(), 3);

    let failed =
        deserialize_validate_round_trip::<ClosedSummaryV0>(SHUTDOWN_MULTIPLE_FAILURES, |value| {
            value
                .validate()
                .expect("failed closed summary must validate")
        });
    assert!(!failed.succeeded());
    assert_eq!(failed.failures.len(), 3);
    assert_eq!(
        failed
            .failures
            .iter()
            .filter(|failure| failure.resource_id == "speech.host.final-relays")
            .count(),
        2,
        "multiple failures for one resource must not be collapsed"
    );
}

#[test]
fn publication_goldens_match_the_wire_contract() {
    for golden in [
        PUBLICATION_NOT_PUBLISHED,
        PUBLICATION_PUBLISHED,
        PUBLICATION_DURABILITY_UNKNOWN,
        PUBLICATION_VISIBLE_FILE_SYNC_UNKNOWN,
    ] {
        deserialize_validate_round_trip::<PublicationOutcomeV0>(golden, |value| {
            value.validate().expect("publication outcome must validate");
        });
    }

    let visible_file_sync_unknown = deserialize_validate_round_trip::<PublicationOutcomeV0>(
        PUBLICATION_VISIBLE_FILE_SYNC_UNKNOWN,
        |value| value.validate().expect("publication outcome must validate"),
    );
    let PublicationOutcomeV0::PublishedDurabilityUnknown { receipt, .. } =
        visible_file_sync_unknown
    else {
        panic!("file-sync fixture must be a durability-unknown publication");
    };
    assert!(receipt.visible);
    assert!(!receipt.file_synced);
    assert!(!receipt.directory_synced);
    assert!(receipt.idempotent_recovery);
}

#[test]
fn privacy_policy_goldens_round_trip_and_fail_closed() {
    let local =
        deserialize_validate_round_trip::<PrivacyPolicyV0>(PRIVACY_POLICY_LOCAL_ONLY, |value| {
            value.validate().expect("local-only policy must validate");
        });
    assert_eq!(
        local.decide(&RoutePrivacyContextV0 {
            target: RouteTargetV0::Local,
            data_tier: DataTierV0::Restricted,
            redaction: RedactionStateV0::NotApplied,
        }),
        PrivacyDecisionV0::Allowed
    );
    assert_eq!(
        local.decide(&hosted_route("provider.cerebras", DataTierV0::Public)),
        PrivacyDecisionV0::Denied(PrivacyDenialV0::LocalOnlyBoundary)
    );

    let hosted = deserialize_validate_round_trip::<PrivacyPolicyV0>(
        PRIVACY_POLICY_HOSTED_EXPLICIT,
        |value| {
            value
                .validate()
                .expect("explicit hosted policy must validate")
        },
    );
    assert_eq!(
        hosted.decide(&hosted_route("provider.cerebras", DataTierV0::Private)),
        PrivacyDecisionV0::Allowed
    );
    assert_eq!(
        hosted.decide(&hosted_route("provider.other", DataTierV0::Private)),
        PrivacyDecisionV0::Denied(PrivacyDenialV0::ProviderNotAllowed)
    );
    assert_eq!(
        hosted.decide(&hosted_route("provider.cerebras", DataTierV0::Restricted)),
        PrivacyDecisionV0::Denied(PrivacyDenialV0::DataTierNotAllowed)
    );
    assert_eq!(
        hosted.decide(&RoutePrivacyContextV0 {
            target: RouteTargetV0::Unknown,
            data_tier: DataTierV0::Public,
            redaction: RedactionStateV0::Unknown,
        }),
        PrivacyDecisionV0::Denied(PrivacyDenialV0::UnknownRoute)
    );
}

fn hosted_route(provider_id: &str, data_tier: DataTierV0) -> RoutePrivacyContextV0 {
    RoutePrivacyContextV0 {
        target: RouteTargetV0::Hosted {
            provider_id: ProviderId::new(provider_id).expect("provider ID"),
        },
        data_tier,
        redaction: RedactionStateV0::Applied,
    }
}

#[test]
fn fixture_manifest_covers_and_authenticates_every_golden() {
    let manifest = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/v0/MANIFEST.sha256"
    ));
    let mut declared = BTreeMap::new();

    for (line_number, line) in manifest.lines().enumerate() {
        let (digest, name) = line.split_once("  ").unwrap_or_else(|| {
            panic!(
                "{FIXTURE_ROOT}/MANIFEST.sha256 line {} is malformed",
                line_number + 1
            )
        });
        assert_eq!(digest.len(), 64, "manifest digest must be SHA-256");
        assert!(
            digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "manifest digest must use lowercase hexadecimal"
        );
        assert!(
            declared.insert(name, digest).is_none(),
            "manifest must not repeat {name}"
        );
    }

    for golden in ALL_GOLDENS {
        let expected = declared
            .remove(golden.name)
            .unwrap_or_else(|| panic!("manifest must list {}", golden.name));
        let actual = format!("{:x}", Sha256::digest(golden.bytes));
        assert_eq!(
            actual, expected,
            "{} digest must match manifest",
            golden.name
        );
    }

    assert!(
        declared.is_empty(),
        "manifest contains files not compiled into the golden suite: {declared:?}"
    );
}
