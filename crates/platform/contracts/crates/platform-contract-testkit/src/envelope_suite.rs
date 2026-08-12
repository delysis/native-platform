//! Assertions for the exact version-zero serialized envelopes.
//!
//! These helpers deliberately accept the contract crate's concrete data
//! types. Adapters may construct or return those records, but the testkit does
//! not define a second envelope protocol for them to implement.

use platform_contracts_v0::{CapabilitySnapshotV0, ContractError, ServiceErrorV0};

/// Validate the exact service-error wire type used by version-zero adapters.
pub fn validate_service_error_v0(error: &ServiceErrorV0) -> Result<(), ContractError> {
    error.validate()
}

/// Validate the exact capability-snapshot wire type used by version-zero adapters.
pub fn validate_capability_snapshot_v0(
    snapshot: &CapabilitySnapshotV0,
) -> Result<(), ContractError> {
    snapshot.validate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_contracts_v0::capability::CAPABILITY_SCHEMA_V0;
    use platform_contracts_v0::error::SERVICE_ERROR_SCHEMA_V0;
    use platform_contracts_v0::{
        CapabilityEntryV0, ContentDigest, ErrorClass, Readiness, RetryAdvice, ServiceId, TriState,
    };
    use std::collections::BTreeMap;

    fn service_error(code: &str) -> ServiceErrorV0 {
        ServiceErrorV0 {
            schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
            code: code.to_owned(),
            class: ErrorClass::Unavailable,
            retry: RetryAdvice::DifferentRoute,
            operation_id: None,
            service: ServiceId::new("inference-gateway").expect("valid service ID"),
            safe_detail: "the selected route is unavailable".to_owned(),
        }
    }

    fn capability_entry(
        readiness: Readiness,
        observed_at_unix_ms: Option<u64>,
        remediation: Option<&str>,
    ) -> CapabilityEntryV0 {
        CapabilityEntryV0 {
            operation: "chat".to_owned(),
            backend_or_resource_id: "local-model".to_owned(),
            readiness,
            limits: BTreeMap::from([("context_tokens".to_owned(), 4096)]),
            network: TriState::No,
            privacy_eligible: TriState::Yes,
            evidence_source: "adapter runtime probe".to_owned(),
            evidence_outcome: "probe returned an explicit state".to_owned(),
            observed_at_unix_ms,
            remediation: remediation.map(str::to_owned),
        }
    }

    fn capability_snapshot(entry: CapabilityEntryV0) -> CapabilitySnapshotV0 {
        CapabilitySnapshotV0 {
            schema: CAPABILITY_SCHEMA_V0.to_owned(),
            snapshot_id: ContentDigest::sha256("a".repeat(64)).expect("valid digest"),
            target: "adapter-under-test".to_owned(),
            services: BTreeMap::from([(
                ServiceId::new("inference").expect("valid service ID"),
                vec![entry],
            )]),
            reports: Vec::new(),
        }
    }

    #[test]
    fn exact_v0_types_use_the_exported_schema_identities_and_validation() {
        let error = service_error("provider.unavailable");
        assert_eq!(error.schema, SERVICE_ERROR_SCHEMA_V0);
        validate_service_error_v0(&error).expect("service error must validate");

        let snapshot = capability_snapshot(capability_entry(Readiness::Ready, Some(1), None));
        assert_eq!(snapshot.schema, CAPABILITY_SCHEMA_V0);
        validate_capability_snapshot_v0(&snapshot).expect("capability snapshot must validate");
    }

    #[test]
    fn retired_parallel_schema_names_fail_closed() {
        let mut error = service_error("provider.unavailable");
        error.schema = "platform.error.v0".to_owned();
        assert_eq!(
            validate_service_error_v0(&error),
            Err(ContractError::Invalid { field: "schema" })
        );

        let mut snapshot = capability_snapshot(capability_entry(Readiness::Ready, Some(1), None));
        snapshot.schema = "platform.capability.v0".to_owned();
        assert_eq!(
            validate_capability_snapshot_v0(&snapshot),
            Err(ContractError::Invalid { field: "schema" })
        );
    }

    #[test]
    fn dotted_error_codes_accept_segments_and_reject_empty_or_unsafe_segments() {
        validate_service_error_v0(&service_error("provider.capability_unsupported"))
            .expect("namespaced dotted code must validate");

        for invalid in [
            ".provider",
            "provider.",
            "provider..timeout",
            "provider.NotSafe",
        ] {
            assert_eq!(
                validate_service_error_v0(&service_error(invalid)),
                Err(ContractError::Invalid { field: "code" }),
                "dotted code `{invalid}` must fail closed"
            );
        }
    }

    #[test]
    fn unknown_observation_time_is_preserved_and_unknown_readiness_needs_remediation() {
        let explicit_unknown = capability_snapshot(capability_entry(
            Readiness::Unknown,
            None,
            Some("run the runtime probe before admission"),
        ));
        validate_capability_snapshot_v0(&explicit_unknown)
            .expect("unknown timestamp with remediation must validate");
        let entry = &explicit_unknown.services.values().next().expect("service")[0];
        assert_eq!(entry.observed_at_unix_ms, None);

        let missing_remediation =
            capability_snapshot(capability_entry(Readiness::Unknown, None, None));
        assert_eq!(
            validate_capability_snapshot_v0(&missing_remediation),
            Err(ContractError::Inconsistent {
                field: "remediation"
            })
        );
    }
}
