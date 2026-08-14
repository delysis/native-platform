use fte_backend_llama::LlamaNativeBackend;
use fte_router::Gateway;
use llama_native_host::{NativeHost, ProcessExitJoinedNativeHost};
use serde::Serialize;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, OnceCell};

use crate::command_registry::{CommandClass, CommandSpec};
use crate::operation_supervisor::{
    LifecyclePhase as OperationLifecyclePhase, OperationReservation, OperationSupervisor,
    TerminalClass, validate_worker_sets,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppPhase {
    Running,
    Quiescing,
    Closed,
}

#[derive(Debug)]
struct AppLifecycle {
    phase: AppPhase,
    next_occurrence: u64,
    active_work: BTreeMap<u64, ActiveWork>,
}

#[derive(Debug)]
struct ActiveWork {
    command: &'static str,
    cancellation: Option<Arc<AtomicBool>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppShutdownSummary {
    pub started_at_unix_ms: u64,
    pub completed_at_unix_ms: u64,
    pub elapsed_ms: u64,
    pub gateway_drained: bool,
    pub native_host_joined: bool,
    /// Product-owned operation-supervisor facts at the terminal boundary.
    pub operation_supervisor_phase: OperationLifecyclePhase,
    pub active_operation_count: usize,
    pub retained_operation_task_count: usize,
    pub expected_operation_worker_count: usize,
    pub joined_operation_worker_count: usize,
    /// Resident native workers owned at the terminal drain boundary.
    pub expected_native_worker_count: usize,
    pub joined_native_worker_count: usize,
    /// All operation and native workers owned by this application lifetime.
    pub expected_worker_ids: Vec<String>,
    /// Exact workers whose handles reached a joined terminal boundary.
    pub joined_worker_ids: Vec<String>,
    pub application_work_drained: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppShutdownError {
    pub summary: AppShutdownSummary,
    pub operation_error: Option<String>,
    pub gateway_error: Option<String>,
    pub native_error: Option<String>,
}

impl std::fmt::Display for AppShutdownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Mom Llama shutdown did not fully succeed")?;
        if let Some(error) = &self.operation_error {
            write!(formatter, "; operation supervisor: {error}")?;
        }
        if let Some(error) = &self.gateway_error {
            write!(formatter, "; gateway: {error}")?;
        }
        if let Some(error) = &self.native_error {
            write!(formatter, "; native: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for AppShutdownError {}

struct AppRuntime {
    lifecycle: Mutex<AppLifecycle>,
    work_drained: Notify,
    gateway_finalizer: Arc<dyn GatewayFinalizer>,
    native_backend: Arc<LlamaNativeBackend>,
    native_host: Arc<NativeHost>,
    _native_owner: Option<mom_llama_runtime::native_runtime::ProductRuntimeOwner>,
    cancellation_sweeps: AtomicU64,
    product_canceller: Arc<dyn ProductCanceller>,
    shutdown: OnceCell<Result<AppShutdownSummary, AppShutdownError>>,
    joined_native_host: Mutex<Option<ProcessExitJoinedNativeHost>>,
    native_finalizer: Arc<dyn NativeFinalizer>,
    operation_supervisor: OperationSupervisor,
}

#[derive(Clone)]
pub struct AppRuntimeHandle(Arc<AppRuntime>);

pub struct AppWorkLease {
    runtime: Option<Arc<AppRuntime>>,
    occurrence: u64,
    cancellation: Option<Arc<AtomicBool>>,
    supervised: Option<OperationReservation>,
}

trait NativeFinalizer: Send + Sync {
    fn shutdown(
        &self,
        host: &Arc<NativeHost>,
    ) -> Result<ProcessExitJoinedNativeHost, mom_llama_runtime::ProductShutdownError>;
}

trait GatewayFinalizer: Send + Sync {
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;
}

trait ProductCanceller: Send + Sync {
    fn cancel_all(&self) -> usize;
}

struct RuntimeProductCanceller;

impl ProductCanceller for RuntimeProductCanceller {
    fn cancel_all(&self) -> usize {
        mom_llama_runtime::request_product_cancellation()
    }
}

struct ProductGatewayFinalizer(Arc<Gateway>);

impl GatewayFinalizer for ProductGatewayFinalizer {
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async { self.0.shutdown().await.map_err(|error| error.to_string()) })
    }
}

struct ProductNativeFinalizer;

impl NativeFinalizer for ProductNativeFinalizer {
    fn shutdown(
        &self,
        host: &Arc<NativeHost>,
    ) -> Result<ProcessExitJoinedNativeHost, mom_llama_runtime::ProductShutdownError> {
        mom_llama_runtime::shutdown_product_runtime_for_process_exit(host)
    }
}

impl AppWorkLease {
    pub fn cancellation_requested(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(|control| control.load(Ordering::Acquire))
            || self.supervised.as_ref().is_some_and(|reservation| {
                reservation
                    .lease
                    .supervisor()
                    .is_some_and(|supervisor| supervisor.cancellation_requested(&reservation.lease))
            })
    }

    pub async fn cancelled(&self) {
        while !self.cancellation_requested() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub fn finish(mut self, class: TerminalClass) -> Result<(), String> {
        self.finish_supervised(class)
    }

    pub async fn run_blocking<T, F>(mut self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, String> + Send + 'static,
    {
        let reservation = self
            .supervised
            .take()
            .ok_or_else(|| "Mom Llama's long operation has no supervisor reservation".to_owned())?;
        let supervisor = reservation
            .lease
            .supervisor()
            .ok_or_else(|| "Mom Llama's operation supervisor is unavailable".to_owned())?;
        let task = supervisor
            .spawn(reservation, move |lease| {
                if lease.cancellation_requested() {
                    return Err(
                        "Mom Llama cancelled the operation during application shutdown".to_owned(),
                    );
                }
                let _app_lease = self;
                operation()
            })
            .map_err(|error| error.to_string())?;
        task.wait().await
    }

    pub async fn run_blocking_with_cancellation_evidence<T, F>(
        mut self,
        operation: F,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<(T, bool), String> + Send + 'static,
    {
        let reservation = self
            .supervised
            .take()
            .ok_or_else(|| "Mom Llama's long operation has no supervisor reservation".to_owned())?;
        let supervisor = reservation
            .lease
            .supervisor()
            .ok_or_else(|| "Mom Llama's operation supervisor is unavailable".to_owned())?;
        let task = supervisor
            .spawn(reservation, move |lease| {
                if lease.cancellation_requested() {
                    return Err(
                        "Mom Llama cancelled the operation during application shutdown".to_owned(),
                    );
                }
                let _app_lease = self;
                let (value, authoritative_cancellation) = operation()?;
                if authoritative_cancellation {
                    lease
                        .request_cancellation_from_executor()
                        .map_err(|error| error.to_string())?;
                }
                Ok(value)
            })
            .map_err(|error| error.to_string())?;
        task.wait().await
    }

    fn finish_supervised(&mut self, class: TerminalClass) -> Result<(), String> {
        let Some(reservation) = self.supervised.take() else {
            return Ok(());
        };
        let supervisor = reservation
            .lease
            .supervisor()
            .ok_or_else(|| "Mom Llama's operation supervisor is unavailable".to_owned())?;
        supervisor
            .queue(&reservation.lease)
            .and_then(|()| supervisor.start(&reservation.lease))
            .and_then(|()| supervisor.terminal(&reservation.lease, class))
            .and_then(|()| supervisor.release(&reservation.lease))
            .map_err(|error| error.to_string())
    }
}

impl Drop for AppWorkLease {
    fn drop(&mut self) {
        let terminal = if self.cancellation_requested() {
            TerminalClass::Cancelled
        } else {
            TerminalClass::Failed
        };
        let _ = self.finish_supervised(terminal);
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        let drained = {
            let mut lifecycle = runtime
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let removed = lifecycle
                .active_work
                .remove(&self.occurrence)
                .expect("an application work lease must be released exactly once");
            debug_assert!(!removed.command.is_empty());
            lifecycle.active_work.is_empty()
        };
        if drained {
            runtime.work_drained.notify_waiters();
        }
    }
}

impl AppRuntimeHandle {
    pub fn new(
        gateway: Arc<Gateway>,
        native_backend: Arc<LlamaNativeBackend>,
        native_owner: mom_llama_runtime::native_runtime::ProductRuntimeOwner,
    ) -> Self {
        let native_host = native_owner.host();
        let gateway_finalizer = Arc::new(ProductGatewayFinalizer(Arc::clone(&gateway)));
        Self::with_finalizers(
            gateway_finalizer,
            native_backend,
            native_host,
            Some(native_owner),
            Arc::new(RuntimeProductCanceller),
            Arc::new(ProductNativeFinalizer),
        )
    }

    fn with_finalizers(
        gateway_finalizer: Arc<dyn GatewayFinalizer>,
        native_backend: Arc<LlamaNativeBackend>,
        native_host: Arc<NativeHost>,
        native_owner: Option<mom_llama_runtime::native_runtime::ProductRuntimeOwner>,
        product_canceller: Arc<dyn ProductCanceller>,
        native_finalizer: Arc<dyn NativeFinalizer>,
    ) -> Self {
        Self::with_operation_supervisor(
            gateway_finalizer,
            native_backend,
            native_host,
            native_owner,
            product_canceller,
            native_finalizer,
            OperationSupervisor::new(),
        )
    }

    fn with_operation_supervisor(
        gateway_finalizer: Arc<dyn GatewayFinalizer>,
        native_backend: Arc<LlamaNativeBackend>,
        native_host: Arc<NativeHost>,
        native_owner: Option<mom_llama_runtime::native_runtime::ProductRuntimeOwner>,
        product_canceller: Arc<dyn ProductCanceller>,
        native_finalizer: Arc<dyn NativeFinalizer>,
        operation_supervisor: OperationSupervisor,
    ) -> Self {
        Self(Arc::new(AppRuntime {
            lifecycle: Mutex::new(AppLifecycle {
                phase: AppPhase::Running,
                next_occurrence: 0,
                active_work: BTreeMap::new(),
            }),
            work_drained: Notify::new(),
            gateway_finalizer,
            native_backend,
            native_host,
            _native_owner: native_owner,
            cancellation_sweeps: AtomicU64::new(0),
            product_canceller,
            shutdown: OnceCell::new(),
            joined_native_host: Mutex::new(None),
            native_finalizer,
            operation_supervisor,
        }))
    }

    pub fn admit(&self, command: &'static CommandSpec) -> Result<AppWorkLease, String> {
        let mut lifecycle = self
            .0
            .lifecycle
            .lock()
            .map_err(|_| "Mom Llama application state is unavailable".to_string())?;
        if lifecycle.phase != AppPhase::Running {
            return Err("Mom Llama is shutting down; new work is not admitted".to_string());
        }
        let occurrence = lifecycle
            .next_occurrence
            .checked_add(1)
            .ok_or_else(|| "Mom Llama has too many active operations".to_string())?;
        lifecycle.next_occurrence = occurrence;
        let cancellation = (command.class == CommandClass::LongOperation)
            .then(|| Arc::new(AtomicBool::new(false)));
        let supervised = if command.class == CommandClass::LongOperation {
            let operation_id = format!("{}:{occurrence}", command.name);
            Some(
                self.0
                    .operation_supervisor
                    .reserve(&operation_id)
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        let replaced = lifecycle.active_work.insert(
            occurrence,
            ActiveWork {
                command: command.name,
                cancellation: cancellation.clone(),
            },
        );
        debug_assert!(replaced.is_none());
        Ok(AppWorkLease {
            runtime: Some(Arc::clone(&self.0)),
            occurrence,
            cancellation,
            supervised,
        })
    }

    /// Returns true only to the caller that closes application admission.
    pub fn begin_quiesce(&self) -> bool {
        let cancellations = {
            let Ok(mut lifecycle) = self.0.lifecycle.lock() else {
                return false;
            };
            if lifecycle.phase != AppPhase::Running {
                return false;
            }
            lifecycle.phase = AppPhase::Quiescing;
            lifecycle
                .active_work
                .values()
                .filter_map(|work| work.cancellation.clone())
                .collect::<Vec<_>>()
        };
        for cancellation in cancellations {
            cancellation.store(true, Ordering::Release);
        }
        self.0.operation_supervisor.begin_quiesce();
        true
    }

    pub fn refresh_native_model(&self, _lease: &AppWorkLease) -> Result<(), String> {
        let model = mom_llama_runtime::gateway_native_model_configuration(&self.0.native_host)
            .map_err(|error| format!("local gateway configuration failed: {error}"))?;
        self.0
            .native_backend
            .replace_configuration(Arc::clone(&self.0.native_host), model)
            .map_err(|error| format!("local gateway configuration failed: {error}"))
    }

    pub async fn shutdown(&self) -> Result<AppShutdownSummary, AppShutdownError> {
        self.begin_quiesce();
        self.0
            .shutdown
            .get_or_init(|| async {
                let started = Instant::now();
                let started_at_unix_ms = unix_time_ms();
                // Closing app admission publishes cancellation to every long
                // operation before service owners begin their own drain. The
                // service drain and application lease drain may proceed in
                // parallel, but both precede the sole terminal native join.
                self.request_product_cancellation();
                let gateway_shutdown = self.0.gateway_finalizer.shutdown();
                let app_work_drain = self.wait_for_work_drained();
                let (gateway_result, ()) = tokio::join!(gateway_shutdown, app_work_drain);
                let gateway_error = gateway_result.err();
                let supervisor = self.0.operation_supervisor.shutdown();
                let operation_error = (!validate_worker_sets(&supervisor)).then(|| {
                    "operation worker join identities did not match admitted worker identities"
                        .to_owned()
                });
                // Admission is closed and every application lease plus the
                // gateway has drained, so the resident set cannot grow after
                // this observation. Preserve its cardinality even if the
                // finalizer fails before returning joined evidence.
                let native_worker_ids = self
                    .0
                    .native_host
                    .slots()
                    .into_iter()
                    .map(|slot| format!("mom-native-slot-{}", slot.slot_id))
                    .collect::<Vec<_>>();
                let expected_native_worker_count = native_worker_ids.len();
                let joined = self.0.native_finalizer.shutdown(&self.0.native_host);
                let (native_error, joined_native_worker_count) = match joined {
                    Ok(receipt) => {
                        let count = receipt.joined_worker_count();
                        self.0
                            .joined_native_host
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .replace(receipt);
                        (None, count)
                    }
                    Err(error) => (Some(error.to_string()), 0),
                };
                let operation_supervisor_phase = supervisor.phase;
                let active_operation_count = supervisor.active_operations;
                let retained_operation_task_count = supervisor.retained_tasks;
                let expected_operation_worker_count = supervisor.expected_worker_ids.len();
                let joined_operation_worker_count = supervisor.joined_worker_ids.len();
                let mut expected_worker_ids = supervisor.expected_worker_ids;
                expected_worker_ids.extend(native_worker_ids.iter().cloned());
                let mut joined_worker_ids = supervisor.joined_worker_ids;
                if joined_native_worker_count == native_worker_ids.len() {
                    joined_worker_ids.extend(native_worker_ids);
                }
                self.0
                    .lifecycle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .phase = AppPhase::Closed;
                let summary = AppShutdownSummary {
                    started_at_unix_ms,
                    completed_at_unix_ms: unix_time_ms(),
                    elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    gateway_drained: gateway_error.is_none(),
                    native_host_joined: native_error.is_none(),
                    operation_supervisor_phase,
                    active_operation_count,
                    retained_operation_task_count,
                    expected_operation_worker_count,
                    joined_operation_worker_count,
                    expected_native_worker_count,
                    joined_native_worker_count,
                    expected_worker_ids,
                    joined_worker_ids,
                    application_work_drained: true,
                };
                if operation_error.is_none() && gateway_error.is_none() && native_error.is_none() {
                    Ok(summary)
                } else {
                    Err(AppShutdownError {
                        summary,
                        operation_error,
                        gateway_error,
                        native_error,
                    })
                }
            })
            .await
            .clone()
    }

    async fn wait_for_work_drained(&self) {
        loop {
            let notified = self.0.work_drained.notified();
            if self
                .0
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .active_work
                .is_empty()
            {
                return;
            }
            // Product operations register their concrete cancellation handles
            // after app admission. Keep sweeping while any admitted work is
            // alive so registration after the first shutdown scan cannot lose
            // cancellation.
            self.request_product_cancellation();
            tokio::select! {
                () = notified => {}
                () = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
    }

    fn request_product_cancellation(&self) {
        self.0.cancellation_sweeps.fetch_add(1, Ordering::AcqRel);
        let _ = self.0.product_canceller.cancel_all();
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::{
        AppRuntimeHandle, GatewayFinalizer, NativeFinalizer, ProductCanceller,
        ProductGatewayFinalizer,
    };
    use crate::command_registry::command_spec;
    use fte_backend_llama::LlamaNativeBackend;
    use fte_router::{Gateway, GatewayDefaults};
    use llama_native_host::{NativeHost, NativeHostConfig, ProcessExitJoinedNativeHost};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;

    fn runtime() -> AppRuntimeHandle {
        runtime_with_finalizer(Arc::new(AtomicBool::new(false)))
    }

    struct RecordingFinalizer {
        called: Arc<AtomicBool>,
    }

    struct NoopProductCanceller;

    impl ProductCanceller for NoopProductCanceller {
        fn cancel_all(&self) -> usize {
            0
        }
    }

    impl NativeFinalizer for RecordingFinalizer {
        fn shutdown(
            &self,
            _host: &Arc<NativeHost>,
        ) -> Result<ProcessExitJoinedNativeHost, mom_llama_runtime::ProductShutdownError> {
            self.called.store(true, Ordering::Release);
            Err(mom_llama_runtime::ProductShutdownError::HostMissing)
        }
    }

    fn runtime_with_finalizer(called: Arc<AtomicBool>) -> AppRuntimeHandle {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let gateway = Arc::new(Gateway::new(GatewayDefaults {
            catalog_version: "test".to_string(),
        }));
        AppRuntimeHandle::with_finalizers(
            Arc::new(ProductGatewayFinalizer(gateway)),
            Arc::new(LlamaNativeBackend::new_borrowed(Arc::clone(&host))),
            host,
            None,
            Arc::new(NoopProductCanceller),
            Arc::new(RecordingFinalizer { called }),
        )
    }

    struct BlockingGatewayFinalizer {
        entered: Arc<AtomicBool>,
        release: Arc<Notify>,
    }

    impl GatewayFinalizer for BlockingGatewayFinalizer {
        fn shutdown(&self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            Box::pin(async {
                self.entered.store(true, Ordering::Release);
                self.release.notified().await;
                Ok(())
            })
        }
    }

    #[test]
    fn quiesce_closes_admission_once() {
        let runtime = runtime();
        let command = command_spec("mom_llama_settings_update");
        drop(runtime.admit(command).expect("running admission"));
        assert!(runtime.begin_quiesce());
        assert!(!runtime.begin_quiesce());
        assert!(runtime.admit(command).is_err());
    }

    #[test]
    fn cloned_handles_share_one_admission_gate() {
        let first = runtime();
        let second = first.clone();
        assert!(first.begin_quiesce());
        assert!(!second.begin_quiesce());
        assert!(
            second
                .admit(command_spec("mom_llama_settings_update"))
                .is_err()
        );
    }

    #[tokio::test]
    async fn quiesce_waits_for_every_previously_admitted_operation() {
        let runtime = runtime();
        let command = command_spec("mom_llama_settings_update");
        let first = runtime.admit(command).expect("admit first operation");
        let second = runtime.admit(command).expect("admit second operation");
        assert!(runtime.begin_quiesce());
        assert!(runtime.admit(command).is_err());

        let waiter = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.wait_for_work_drained().await })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(first);
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(second);
        waiter.await.expect("work drain task");
    }

    #[tokio::test]
    async fn direct_native_operation_drains_before_final_join() {
        let finalizer_called = Arc::new(AtomicBool::new(false));
        let runtime = runtime_with_finalizer(Arc::clone(&finalizer_called));
        let lease = runtime
            .admit(command_spec("mom_llama_chat_send"))
            .expect("admit direct native operation");
        let cancellation = lease
            .cancellation
            .as_ref()
            .expect("long operation cancellation")
            .clone();
        let result = Arc::new(Mutex::new(None));
        let shutdown = {
            let runtime = runtime.clone();
            let result = Arc::clone(&result);
            tokio::spawn(async move {
                let shutdown_result = runtime.shutdown().await;
                result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .replace(shutdown_result);
            })
        };

        tokio::task::yield_now().await;
        assert!(cancellation.load(Ordering::Acquire));
        assert!(!finalizer_called.load(Ordering::Acquire));
        drop(lease);
        shutdown.await.expect("shutdown task");
        assert!(finalizer_called.load(Ordering::Acquire));
        assert!(
            result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
        );
    }

    #[tokio::test]
    async fn receipt_and_native_reads_drain_before_final_join() {
        let finalizer_called = Arc::new(AtomicBool::new(false));
        let runtime = runtime_with_finalizer(Arc::clone(&finalizer_called));
        let receipt_read = runtime
            .admit(command_spec("mom_llama_conversation_list"))
            .expect("admit receipt-writing read");
        let native_read = runtime
            .admit(command_spec("mom_llama_model_slot_list"))
            .expect("admit native read");
        assert!(receipt_read.cancellation.is_none());
        assert!(native_read.cancellation.is_none());

        let shutdown = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.shutdown().await })
        };
        tokio::task::yield_now().await;
        assert!(!finalizer_called.load(Ordering::Acquire));

        drop(receipt_read);
        tokio::task::yield_now().await;
        assert!(!finalizer_called.load(Ordering::Acquire));

        drop(native_read);
        let _ = shutdown.await.expect("shutdown task");
        assert!(finalizer_called.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn cancellation_is_reswept_until_late_registered_work_drains() {
        let finalizer_called = Arc::new(AtomicBool::new(false));
        let runtime = runtime_with_finalizer(Arc::clone(&finalizer_called));
        let lease = runtime
            .admit(command_spec("mom_llama_chat_send"))
            .expect("admit long operation");
        let shutdown = {
            let runtime = runtime.clone();
            tokio::spawn(async move { runtime.shutdown().await })
        };

        while runtime.0.cancellation_sweeps.load(Ordering::Acquire) < 3 {
            tokio::task::yield_now().await;
        }
        assert!(lease.cancellation_requested());
        assert!(!finalizer_called.load(Ordering::Acquire));
        drop(lease);
        let _ = shutdown.await.expect("shutdown task");
        assert!(finalizer_called.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn gateway_operation_drains_before_final_join() {
        let gateway_entered = Arc::new(AtomicBool::new(false));
        let gateway_release = Arc::new(Notify::new());
        let finalizer_called = Arc::new(AtomicBool::new(false));
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let runtime = AppRuntimeHandle::with_finalizers(
            Arc::new(BlockingGatewayFinalizer {
                entered: Arc::clone(&gateway_entered),
                release: Arc::clone(&gateway_release),
            }),
            Arc::new(LlamaNativeBackend::new_borrowed(Arc::clone(&host))),
            host,
            None,
            Arc::new(NoopProductCanceller),
            Arc::new(RecordingFinalizer {
                called: Arc::clone(&finalizer_called),
            }),
        );
        let shutdown = tokio::spawn(async move { runtime.shutdown().await });
        while !gateway_entered.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        assert!(!finalizer_called.load(Ordering::Acquire));
        gateway_release.notify_one();
        let _ = shutdown.await.expect("shutdown task");
        assert!(finalizer_called.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn repeated_quit_runs_one_shutdown() {
        let finalizer_called = Arc::new(AtomicBool::new(false));
        let runtime = runtime_with_finalizer(Arc::clone(&finalizer_called));
        let (first, second) = tokio::join!(runtime.shutdown(), runtime.shutdown());
        assert_eq!(first, second);
        assert!(finalizer_called.load(Ordering::Acquire));
    }

    fn command_vs_quit_has_one_winner(command: &'static str) {
        let runtime = runtime();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let command_thread = {
            let runtime = runtime.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                runtime.admit(command_spec(command))
            })
        };
        let quit_thread = {
            let runtime = runtime.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                runtime.begin_quiesce()
            })
        };
        barrier.wait();
        let admitted = command_thread.join().expect("command admission thread");
        assert!(quit_thread.join().expect("quit admission thread"));
        assert!(runtime.admit(command_spec(command)).is_err());
        if let Ok(lease) = admitted {
            drop(lease);
        }
        assert!(runtime.admit(command_spec(command)).is_err());
    }

    #[test]
    fn model_select_vs_quit_has_one_winner() {
        command_vs_quit_has_one_winner("mom_llama_model_select");
    }

    #[test]
    fn settings_update_vs_quit_has_one_winner() {
        command_vs_quit_has_one_winner("mom_llama_settings_update");
    }

    #[test]
    fn receipt_writing_read_vs_quit_has_one_winner() {
        command_vs_quit_has_one_winner("mom_llama_conversation_list");
    }

    #[test]
    fn native_read_vs_quit_has_one_winner() {
        command_vs_quit_has_one_winner("mom_llama_model_slot_list");
    }
}
