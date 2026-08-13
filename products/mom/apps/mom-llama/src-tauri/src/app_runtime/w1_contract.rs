//! Temporary conversion boundary for the canonical Wave 1 shutdown envelope.
//!
//! The product remains authoritative. This adapter converts only the composed
//! shutdown facts that `AppRuntimeHandle::shutdown` actually observes; it does
//! not reconstruct worker state or serialize runtime authority.
//!
//! The compositional adapter below exercises the production operation
//! supervisor directly; it does not maintain a test-owned shadow lifecycle.

mod compositional;

use super::{AppShutdownError, AppShutdownSummary};
use platform_contracts_v0::error::SERVICE_ERROR_SCHEMA_V0;
use platform_contracts_v0::shutdown::CLOSED_SUMMARY_SCHEMA_V0;
use platform_contracts_v0::{
    ClosedSummaryV0, ContractError, ErrorClass, RetryAdvice, ServiceErrorV0, ServiceId,
    ShutdownFailureV0, ShutdownResourceKind, ShutdownResourceState, ShutdownResourceV0,
    SupervisorPhase,
};

const APP_REGISTRY: &str = "mom.app.operation_registry";
const OPERATION_TASKS: &str = "mom.app.operation_workers";
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
    let operation_supervisor_stopped = summary.operation_supervisor_phase
        == crate::operation_supervisor::LifecyclePhase::Closed
        && summary.active_operation_count == 0
        && summary.retained_operation_task_count == 0
        && summary.expected_operation_worker_count == summary.joined_operation_worker_count;

    let resources = vec![
        resource(
            OPERATION_TASKS,
            app.clone(),
            ShutdownResourceKind::TaskSupervisor,
            operation_supervisor_stopped,
            summary.expected_operation_worker_count,
            summary.joined_operation_worker_count,
        ),
        resource(
            APP_REGISTRY,
            app.clone(),
            ShutdownResourceKind::OperationRegistry,
            true,
            0,
            0,
        ),
        resource(
            GATEWAY,
            gateway.clone(),
            ShutdownResourceKind::Runtime,
            summary.gateway_drained,
            0,
            0,
        ),
        resource(
            NATIVE_HOST,
            native.clone(),
            ShutdownResourceKind::WorkerPool,
            summary.native_host_joined,
            summary.expected_native_worker_count,
            summary.joined_native_worker_count,
        ),
    ];

    let mut failures = Vec::new();
    if !operation_supervisor_stopped {
        failures.push(failure(
            "mom.operation.shutdown",
            OPERATION_TASKS,
            app,
            "mom.shutdown.operation_join",
            "the operation supervisor did not reach a fully joined closed boundary",
        ));
    }
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
        phase: match summary.operation_supervisor_phase {
            crate::operation_supervisor::LifecyclePhase::Running => SupervisorPhase::Running,
            crate::operation_supervisor::LifecyclePhase::Quiescing => SupervisorPhase::Quiescing,
            crate::operation_supervisor::LifecyclePhase::Closed => SupervisorPhase::Closed,
        },
        active_operations: summary.active_operation_count,
        retained_tasks: summary.retained_operation_task_count,
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
    expected_workers: usize,
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
        expected_workers,
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
    use super::super::{
        AppRuntimeHandle, GatewayFinalizer, NativeFinalizer, ProductCanceller,
        ProductGatewayFinalizer,
    };
    use super::*;
    use crate::command_registry::command_spec;
    use fte_backend_llama::LlamaNativeBackend;
    use fte_router::{Gateway, GatewayDefaults};
    use llama_native_host::{NativeHost, NativeHostConfig, ProcessExitJoinedNativeHost};
    use std::sync::Arc;

    struct DirectNativeFinalizer;

    impl NativeFinalizer for DirectNativeFinalizer {
        fn shutdown(
            &self,
            host: &Arc<NativeHost>,
        ) -> Result<ProcessExitJoinedNativeHost, mom_llama_runtime::ProductShutdownError> {
            Ok(host.shutdown_for_process_exit())
        }
    }

    struct FailedNativeFinalizer;

    impl NativeFinalizer for FailedNativeFinalizer {
        fn shutdown(
            &self,
            _host: &Arc<NativeHost>,
        ) -> Result<ProcessExitJoinedNativeHost, mom_llama_runtime::ProductShutdownError> {
            Err(mom_llama_runtime::ProductShutdownError::HostMissing)
        }
    }

    struct NoopProductCanceller;

    impl ProductCanceller for NoopProductCanceller {
        fn cancel_all(&self) -> usize {
            0
        }
    }

    fn runtime(native_finalizer: Arc<dyn NativeFinalizer>) -> AppRuntimeHandle {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let gateway = Arc::new(Gateway::new(GatewayDefaults {
            catalog_version: "w1-contract-test".to_owned(),
        }));
        AppRuntimeHandle::with_finalizers(
            Arc::new(ProductGatewayFinalizer(gateway)) as Arc<dyn GatewayFinalizer>,
            Arc::new(LlamaNativeBackend::new_borrowed(Arc::clone(&host))),
            host,
            None,
            Arc::new(NoopProductCanceller),
            native_finalizer,
        )
    }

    #[tokio::test]
    async fn actual_composed_shutdown_forms_a_closed_summary() {
        let runtime = runtime(Arc::new(DirectNativeFinalizer));
        let result = runtime.shutdown().await;
        let closed = closed_summary_v0(&result).expect("canonical closed summary");
        assert!(closed.succeeded());
        assert_eq!(closed.active_operations, 0);
        assert_eq!(closed.retained_tasks, 0);
        assert_eq!(closed.expected_workers, 0);
        assert_eq!(closed.joined_workers, 0);
        assert_eq!(closed.resources.len(), 4);
        assert_eq!(runtime.shutdown().await, result);
    }

    #[tokio::test]
    async fn actual_failed_native_finalizer_stays_failed_without_a_join_claim() {
        let runtime = runtime(Arc::new(FailedNativeFinalizer));
        let result = runtime.shutdown().await;
        let error = result.as_ref().expect_err("injected native failure");
        assert_eq!(error.summary.expected_native_worker_count, 0);
        assert_eq!(error.summary.joined_native_worker_count, 0);
        let private_diagnostic = error.native_error.as_deref().expect("native diagnostic");

        let closed = closed_summary_v0(&result).expect("canonical failed summary");
        assert!(!closed.succeeded());
        assert_eq!(closed.joined_workers, 0);
        assert_eq!(closed.failures.len(), 1);
        let encoded = serde_json::to_string(&closed).expect("serialize summary");
        assert!(!encoded.contains(private_diagnostic));
    }

    #[tokio::test]
    async fn actual_shutdown_waits_for_the_real_app_registry_before_projection() {
        let runtime = runtime(Arc::new(DirectNativeFinalizer));
        let lease = runtime
            .admit(command_spec("mom_llama_chat_send"))
            .expect("admit real app work");
        let shutdown = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.shutdown().await })
        };
        while !lease.cancellation_requested() {
            tokio::task::yield_now().await;
        }
        assert!(!shutdown.is_finished());
        drop(lease);

        let result = shutdown.await.expect("shutdown task");
        let closed = closed_summary_v0(&result).expect("canonical closed summary");
        assert_eq!(closed.active_operations, 0);
        assert!(closed.succeeded());
    }
}
