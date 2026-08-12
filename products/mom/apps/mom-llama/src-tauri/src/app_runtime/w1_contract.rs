//! Temporary conversion boundary for the canonical Wave 1 shutdown envelope.
//!
//! The product remains authoritative. This adapter converts only the composed
//! shutdown facts that `AppRuntimeHandle::shutdown` actually observes; it does
//! not reconstruct worker state or serialize runtime authority.

use super::{AppShutdownError, AppShutdownSummary};
use platform_contracts_v0::error::SERVICE_ERROR_SCHEMA_V0;
use platform_contracts_v0::shutdown::CLOSED_SUMMARY_SCHEMA_V0;
use platform_contracts_v0::{
    ClosedSummaryV0, ContractError, ErrorClass, RetryAdvice, ServiceErrorV0, ServiceId,
    ShutdownFailureV0, ShutdownResourceKind, ShutdownResourceState, ShutdownResourceV0,
    SupervisorPhase,
};

const APP_REGISTRY: &str = "mom.app.operation_registry";
const GATEWAY: &str = "mom.gateway.runtime";
const NATIVE_HOST: &str = "mom.native.worker_pool";

pub fn closed_summary_v0(
    result: &Result<AppShutdownSummary, AppShutdownError>,
) -> Result<ClosedSummaryV0, ContractError> {
    let summary = match result {
        Ok(summary) => summary,
        Err(error) => &error.summary,
    };
    if !summary.application_work_drained {
        return Err(ContractError::Inconsistent {
            field: "active_operations",
        });
    }

    let app = ServiceId::new("mom-app").map_err(|_| ContractError::Invalid { field: "service" })?;
    let gateway =
        ServiceId::new("gateway").map_err(|_| ContractError::Invalid { field: "service" })?;
    let native = ServiceId::new("llama-native-host")
        .map_err(|_| ContractError::Invalid { field: "service" })?;

    let resources = vec![
        resource(
            APP_REGISTRY,
            app,
            ShutdownResourceKind::OperationRegistry,
            true,
            0,
        ),
        resource(
            GATEWAY,
            gateway.clone(),
            ShutdownResourceKind::Runtime,
            summary.gateway_drained,
            0,
        ),
        resource(
            NATIVE_HOST,
            native.clone(),
            ShutdownResourceKind::WorkerPool,
            summary.native_host_joined,
            summary.joined_native_worker_count,
        ),
    ];

    let mut failures = Vec::new();
    if !summary.gateway_drained {
        failures.push(failure(
            "mom.gateway.shutdown",
            GATEWAY,
            gateway,
            "mom.shutdown.gateway",
            "the application gateway did not drain successfully",
        ));
    }
    if !summary.native_host_joined {
        failures.push(failure(
            "mom.native.shutdown",
            NATIVE_HOST,
            native,
            "mom.shutdown.native_join",
            "the native host did not return joined-shutdown evidence",
        ));
    }

    let expected_workers = resources.iter().try_fold(0usize, |total, resource| {
        total
            .checked_add(resource.expected_workers)
            .ok_or(ContractError::Invalid {
                field: "expected_workers",
            })
    })?;
    let joined_workers = resources.iter().try_fold(0usize, |total, resource| {
        total
            .checked_add(resource.joined_workers)
            .ok_or(ContractError::Invalid {
                field: "joined_workers",
            })
    })?;
    let value = ClosedSummaryV0 {
        schema: CLOSED_SUMMARY_SCHEMA_V0.to_owned(),
        phase: SupervisorPhase::Closed,
        active_operations: 0,
        retained_tasks: 0,
        expected_workers,
        joined_workers,
        resources,
        failures,
    };
    value.validate()?;
    Ok(value)
}

fn resource(
    resource_id: &str,
    service: ServiceId,
    kind: ShutdownResourceKind,
    stopped: bool,
    joined_workers: usize,
) -> ShutdownResourceV0 {
    ShutdownResourceV0 {
        resource_id: resource_id.to_owned(),
        service,
        kind,
        state: if stopped {
            ShutdownResourceState::Stopped
        } else {
            ShutdownResourceState::Failed
        },
        // The application knows how many native workers actually joined. It
        // does not fabricate a larger configured-worker count on failure.
        expected_workers: joined_workers,
        joined_workers,
    }
}

fn failure(
    failure_id: &str,
    resource_id: &str,
    service: ServiceId,
    code: &str,
    safe_detail: &str,
) -> ShutdownFailureV0 {
    ShutdownFailureV0 {
        failure_id: failure_id.to_owned(),
        resource_id: resource_id.to_owned(),
        service: service.clone(),
        error: ServiceErrorV0 {
            schema: SERVICE_ERROR_SCHEMA_V0.to_owned(),
            code: code.to_owned(),
            class: ErrorClass::Worker,
            retry: RetryAdvice::AfterRestart,
            operation_id: None,
            service,
            safe_detail: safe_detail.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn success() -> AppShutdownSummary {
        AppShutdownSummary {
            started_at_unix_ms: 1,
            completed_at_unix_ms: 2,
            elapsed_ms: 1,
            gateway_drained: true,
            native_host_joined: true,
            joined_native_worker_count: 1,
            application_work_drained: true,
        }
    }

    #[test]
    fn real_product_success_facts_form_a_closed_joined_summary() {
        let closed = closed_summary_v0(&Ok(success())).expect("canonical closed summary");
        assert!(closed.succeeded());
        assert_eq!(closed.active_operations, 0);
        assert_eq!(closed.retained_tasks, 0);
        assert_eq!(closed.expected_workers, 1);
        assert_eq!(closed.joined_workers, 1);
        assert_eq!(closed.resources.len(), 3);
    }

    #[test]
    fn failed_native_join_stays_a_failed_resource_with_no_join_claim() {
        let mut summary = success();
        summary.native_host_joined = false;
        summary.joined_native_worker_count = 0;
        let failure = AppShutdownError {
            summary,
            gateway_error: None,
            native_error: Some("private diagnostic not serialized".to_owned()),
        };
        let closed = closed_summary_v0(&Err(failure)).expect("canonical failed summary");
        assert!(!closed.succeeded());
        assert_eq!(closed.joined_workers, 0);
        assert_eq!(closed.failures.len(), 1);
        let encoded = serde_json::to_string(&closed).expect("serialize summary");
        assert!(!encoded.contains("private diagnostic"));
    }

    #[test]
    fn undrained_application_work_cannot_be_relabeled_closed() {
        let mut summary = success();
        summary.application_work_drained = false;
        assert!(closed_summary_v0(&Ok(summary)).is_err());
    }
}
