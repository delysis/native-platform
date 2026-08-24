use llama_native_host::{NativeHost, ProcessExitJoinedNativeHost};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
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

const PERSONA_APPROVAL_RECOVERY_WORKER_ID: &str = "mom-persona-approval-recovery";
const PERSONA_APPROVAL_RECOVERY_INTERVAL: Duration = Duration::from_secs(1);

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
    /// All product-owned workers expected during this application lifetime.
    pub expected_worker_ids: Vec<String>,
    /// Exact workers whose handles reached a joined terminal boundary.
    pub joined_worker_ids: Vec<String>,
    pub application_work_drained: bool,
    pub persona_approval_recovery_complete: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppShutdownError {
    pub summary: AppShutdownSummary,
    pub operation_error: Option<String>,
    pub approval_recovery_error: Option<String>,
    pub native_error: Option<String>,
}

impl std::fmt::Display for AppShutdownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Mom Llama shutdown did not fully succeed")?;
        if let Some(error) = &self.operation_error {
            write!(formatter, "; operation supervisor: {error}")?;
        }
        if let Some(error) = &self.approval_recovery_error {
            write!(formatter, "; Persona approval recovery: {error}")?;
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
    native_host: Arc<NativeHost>,
    _native_owner: Option<mom_llama_runtime::native_runtime::ProductRuntimeOwner>,
    cancellation_sweeps: AtomicU64,
    product_canceller: Arc<dyn ProductCanceller>,
    shutdown: OnceCell<Result<AppShutdownSummary, AppShutdownError>>,
    joined_native_host: Mutex<Option<ProcessExitJoinedNativeHost>>,
    native_finalizer: Arc<dyn NativeFinalizer>,
    operation_supervisor: OperationSupervisor,
    persona_approval_recovery: Arc<PersonaApprovalRecoveryWorker>,
    persona_approval_authority: Option<mom_llama_runtime::PersonaToolApprovalRecovery>,
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

trait ProductCanceller: Send + Sync {
    fn cancel_all(&self) -> usize;
}

trait PersonaApprovalReconciler: Send + Sync {
    fn reconcile(&self) -> Result<(), String>;

    fn observe_invocation(&self, _invocation_id: &str) -> Result<(), String> {
        Ok(())
    }
}

struct RuntimePersonaApprovalReconciler {
    recovery: mom_llama_runtime::PersonaToolApprovalRecovery,
}

impl PersonaApprovalReconciler for RuntimePersonaApprovalReconciler {
    fn reconcile(&self) -> Result<(), String> {
        self.recovery
            .reconcile()
            .map_err(|error| format!("persona approval recovery failed: {error:#}"))
    }

    fn observe_invocation(&self, invocation_id: &str) -> Result<(), String> {
        self.recovery
            .observe_invocation(invocation_id)
            .map_err(|error| format!("persona approval deadline registration failed: {error:#}"))
    }
}

#[cfg(test)]
struct NoopPersonaApprovalReconciler;

#[cfg(test)]
impl PersonaApprovalReconciler for NoopPersonaApprovalReconciler {
    fn reconcile(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PersonaApprovalRecoveryShutdown {
    joined: bool,
    error: Option<String>,
    structural_error: Option<String>,
}

enum PersonaApprovalRecoveryControl {
    Stop,
    #[cfg(test)]
    Tick,
}

struct PersonaApprovalRecoveryWorker {
    reconciler: Arc<dyn PersonaApprovalReconciler>,
    control: SyncSender<PersonaApprovalRecoveryControl>,
    handle: Mutex<Option<JoinHandle<()>>>,
    terminal: OnceLock<PersonaApprovalRecoveryShutdown>,
    start_error: Option<String>,
    last_reconcile_error: Arc<Mutex<Option<String>>>,
}

impl PersonaApprovalRecoveryWorker {
    fn start(reconciler: Arc<dyn PersonaApprovalReconciler>, interval: Duration) -> Self {
        let (control, receiver) = std::sync::mpsc::sync_channel(1);
        let last_reconcile_error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&last_reconcile_error);
        let worker_reconciler = Arc::clone(&reconciler);
        // A single joined thread keeps recovery single-flight. A slow sweep
        // delays the next tick instead of spawning overlapping store work.
        let handle = std::thread::Builder::new()
            .name(PERSONA_APPROVAL_RECOVERY_WORKER_ID.to_owned())
            .spawn(move || {
                run_persona_approval_recovery(worker_reconciler, receiver, interval, &worker_error);
            });
        let (handle, start_error) = match handle {
            Ok(handle) => (Some(handle), None),
            Err(error) => (
                None,
                Some(format!(
                    "could not start {PERSONA_APPROVAL_RECOVERY_WORKER_ID}: {error}"
                )),
            ),
        };
        Self {
            reconciler,
            control,
            handle: Mutex::new(handle),
            terminal: OnceLock::new(),
            start_error,
            last_reconcile_error,
        }
    }

    fn shutdown(&self) -> PersonaApprovalRecoveryShutdown {
        self.terminal.get_or_init(|| self.shutdown_once()).clone()
    }

    fn current_error(&self) -> Option<String> {
        self.start_error.clone().or_else(|| {
            self.last_reconcile_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        })
    }

    fn reconcile_now(&self) -> Result<(), String> {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.reconciler.reconcile()))
                .unwrap_or_else(|panic| {
                    Err(format!(
                        "{PERSONA_APPROVAL_RECOVERY_WORKER_ID} final sweep panicked: {}",
                        panic_message(panic)
                    ))
                });
        record_persona_approval_reconciliation(result.clone(), &self.last_reconcile_error);
        result
    }

    fn observe_invocation(&self, invocation_id: &str) -> Result<(), String> {
        self.reconciler.observe_invocation(invocation_id)
    }

    #[cfg(test)]
    fn tick(&self) {
        self.control
            .send(PersonaApprovalRecoveryControl::Tick)
            .expect("approval recovery worker accepts a deterministic tick");
    }

    fn shutdown_once(&self) -> PersonaApprovalRecoveryShutdown {
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let mut errors = self.start_error.iter().cloned().collect::<Vec<_>>();
        let joined = match handle {
            Some(handle) => {
                let _ = self.control.send(PersonaApprovalRecoveryControl::Stop);
                let panic = handle.join().err();
                if let Some(panic) = panic {
                    errors.push(format!(
                        "{PERSONA_APPROVAL_RECOVERY_WORKER_ID} panicked: {}",
                        panic_message(panic)
                    ));
                }
                true
            }
            None => false,
        };
        let structural_error = (!errors.is_empty()).then(|| errors.join("; "));
        if let Some(error) = self
            .last_reconcile_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            errors.push(error);
        }
        PersonaApprovalRecoveryShutdown {
            joined,
            error: (!errors.is_empty()).then(|| errors.join("; ")),
            structural_error,
        }
    }
}

impl Drop for PersonaApprovalRecoveryWorker {
    fn drop(&mut self) {
        let terminal = self.shutdown();
        if let Some(error) = terminal.error {
            eprintln!("Mom Llama approval recovery teardown: {error}");
        }
    }
}

fn run_persona_approval_recovery(
    reconciler: Arc<dyn PersonaApprovalReconciler>,
    receiver: Receiver<PersonaApprovalRecoveryControl>,
    interval: Duration,
    last_error: &Mutex<Option<String>>,
) {
    loop {
        match receiver.recv_timeout(interval) {
            Ok(PersonaApprovalRecoveryControl::Stop) | Err(RecvTimeoutError::Disconnected) => {
                return;
            }
            #[cfg(test)]
            Ok(PersonaApprovalRecoveryControl::Tick) => {
                reconcile_persona_approvals_once(&reconciler, last_error)
            }
            Err(RecvTimeoutError::Timeout) => {
                reconcile_persona_approvals_once(&reconciler, last_error)
            }
        }
    }
}

fn reconcile_persona_approvals_once(
    reconciler: &Arc<dyn PersonaApprovalReconciler>,
    last_error: &Mutex<Option<String>>,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reconciler.reconcile()))
        .unwrap_or_else(|panic| {
            Err(format!(
                "{PERSONA_APPROVAL_RECOVERY_WORKER_ID} sweep panicked: {}",
                panic_message(panic)
            ))
        });
    record_persona_approval_reconciliation(result, last_error);
}

fn record_persona_approval_reconciliation(
    result: Result<(), String>,
    last_error: &Mutex<Option<String>>,
) {
    let mut recorded_error = last_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match result {
        Ok(()) => {
            if recorded_error.take().is_some() {
                eprintln!("Mom Llama persona approval recovery resumed");
            }
        }
        Err(error) => {
            if recorded_error.as_deref() != Some(error.as_str()) {
                eprintln!("Mom Llama persona approval recovery: {error}");
                *recorded_error = Some(error);
            }
        }
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

struct RuntimeProductCanceller;

impl ProductCanceller for RuntimeProductCanceller {
    fn cancel_all(&self) -> usize {
        mom_llama_runtime::request_product_cancellation()
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

struct AppRuntimeConstruction {
    native_host: Arc<NativeHost>,
    native_owner: Option<mom_llama_runtime::native_runtime::ProductRuntimeOwner>,
    product_canceller: Arc<dyn ProductCanceller>,
    native_finalizer: Arc<dyn NativeFinalizer>,
    operation_supervisor: OperationSupervisor,
    persona_approval_reconciler: Arc<dyn PersonaApprovalReconciler>,
    persona_approval_recovery_interval: Duration,
    persona_approval_authority: Option<mom_llama_runtime::PersonaToolApprovalRecovery>,
}

impl AppRuntimeHandle {
    pub fn new(
        native_owner: mom_llama_runtime::native_runtime::ProductRuntimeOwner,
        persona_approval_recovery: mom_llama_runtime::PersonaToolApprovalRecovery,
    ) -> Self {
        let native_host = native_owner.host();
        let persona_approval_authority = persona_approval_recovery.clone();
        Self::with_operation_supervisor(AppRuntimeConstruction {
            native_host,
            native_owner: Some(native_owner),
            product_canceller: Arc::new(RuntimeProductCanceller),
            native_finalizer: Arc::new(ProductNativeFinalizer),
            operation_supervisor: OperationSupervisor::new(),
            persona_approval_reconciler: Arc::new(RuntimePersonaApprovalReconciler {
                recovery: persona_approval_recovery,
            }),
            persona_approval_recovery_interval: PERSONA_APPROVAL_RECOVERY_INTERVAL,
            persona_approval_authority: Some(persona_approval_authority),
        })
    }

    #[cfg(test)]
    fn with_finalizers(
        native_host: Arc<NativeHost>,
        native_owner: Option<mom_llama_runtime::native_runtime::ProductRuntimeOwner>,
        product_canceller: Arc<dyn ProductCanceller>,
        native_finalizer: Arc<dyn NativeFinalizer>,
    ) -> Self {
        Self::with_operation_supervisor(AppRuntimeConstruction {
            native_host,
            native_owner,
            product_canceller,
            native_finalizer,
            operation_supervisor: OperationSupervisor::new(),
            persona_approval_reconciler: Arc::new(NoopPersonaApprovalReconciler),
            persona_approval_recovery_interval: PERSONA_APPROVAL_RECOVERY_INTERVAL,
            persona_approval_authority: None,
        })
    }

    fn with_operation_supervisor(construction: AppRuntimeConstruction) -> Self {
        let AppRuntimeConstruction {
            native_host,
            native_owner,
            product_canceller,
            native_finalizer,
            operation_supervisor,
            persona_approval_reconciler,
            persona_approval_recovery_interval,
            persona_approval_authority,
        } = construction;
        let persona_approval_recovery = Arc::new(PersonaApprovalRecoveryWorker::start(
            persona_approval_reconciler,
            persona_approval_recovery_interval,
        ));
        Self(Arc::new(AppRuntime {
            lifecycle: Mutex::new(AppLifecycle {
                phase: AppPhase::Running,
                next_occurrence: 0,
                active_work: BTreeMap::new(),
            }),
            work_drained: Notify::new(),
            native_host,
            _native_owner: native_owner,
            cancellation_sweeps: AtomicU64::new(0),
            product_canceller,
            shutdown: OnceCell::new(),
            joined_native_host: Mutex::new(None),
            native_finalizer,
            operation_supervisor,
            persona_approval_recovery,
            persona_approval_authority,
        }))
    }

    pub fn admit(&self, command: &'static CommandSpec) -> Result<AppWorkLease, String> {
        if let Some(error) = self.0.persona_approval_recovery.current_error() {
            return Err(format!(
                "Mom Llama's persona approval recovery worker is unavailable: {error}"
            ));
        }
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

    pub fn observe_persona_tool_approval_invocation(
        &self,
        invocation_id: &str,
    ) -> Result<(), String> {
        self.0
            .persona_approval_recovery
            .observe_invocation(invocation_id)
    }

    pub fn persona_tool_approval_recovery(
        &self,
    ) -> Result<mom_llama_runtime::PersonaToolApprovalRecovery, String> {
        self.0
            .persona_approval_authority
            .clone()
            .ok_or_else(|| "Mom Llama's Persona approval authority is unavailable".to_string())
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

    pub async fn shutdown(&self) -> Result<AppShutdownSummary, AppShutdownError> {
        self.begin_quiesce();
        self.0
            .shutdown
            .get_or_init(|| async {
                let started = Instant::now();
                let started_at_unix_ms = unix_time_ms();
                // Closing app admission publishes cancellation to every long
                // operation before application work drains. That drain
                // precedes the sole terminal native join.
                self.request_product_cancellation();
                let persona_approval_worker = Arc::clone(&self.0.persona_approval_recovery);
                let worker_to_stop = Arc::clone(&persona_approval_worker);
                let persona_approval_recovery =
                    match tokio::task::spawn_blocking(move || worker_to_stop.shutdown()).await {
                        Ok(terminal) => terminal,
                        Err(error) => PersonaApprovalRecoveryShutdown {
                            joined: false,
                            error: Some(format!(
                                "{PERSONA_APPROVAL_RECOVERY_WORKER_ID} join task failed: {error}"
                            )),
                            structural_error: Some(format!(
                                "{PERSONA_APPROVAL_RECOVERY_WORKER_ID} join task failed: {error}"
                            )),
                        },
                    };
                self.wait_for_work_drained().await;
                let supervisor = self.0.operation_supervisor.shutdown();
                let final_recovery_worker = Arc::clone(&persona_approval_worker);
                let final_persona_approval_recovery = match tokio::task::spawn_blocking(move || {
                    final_recovery_worker.reconcile_now()
                })
                .await
                {
                    Ok(result) => result.err(),
                    Err(error) => Some(format!(
                        "{PERSONA_APPROVAL_RECOVERY_WORKER_ID} final sweep task failed: {error}"
                    )),
                };
                let mut operation_errors = Vec::new();
                if !validate_worker_sets(&supervisor) {
                    operation_errors.push(
                        "operation worker join identities did not match admitted worker identities"
                            .to_owned(),
                    );
                }
                let operation_error =
                    (!operation_errors.is_empty()).then(|| operation_errors.join("; "));
                let approval_recovery_errors = persona_approval_recovery
                    .structural_error
                    .iter()
                    .cloned()
                    .chain(final_persona_approval_recovery)
                    .collect::<Vec<_>>();
                let approval_recovery_error = (!approval_recovery_errors.is_empty())
                    .then(|| approval_recovery_errors.join("; "));
                // Admission is closed and every application lease has drained,
                // so the resident set cannot grow after
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
                expected_worker_ids.push(PERSONA_APPROVAL_RECOVERY_WORKER_ID.to_owned());
                let mut joined_worker_ids = supervisor.joined_worker_ids;
                if joined_native_worker_count == native_worker_ids.len() {
                    joined_worker_ids.extend(native_worker_ids);
                }
                if persona_approval_recovery.joined {
                    joined_worker_ids.push(PERSONA_APPROVAL_RECOVERY_WORKER_ID.to_owned());
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
                    persona_approval_recovery_complete: approval_recovery_error.is_none(),
                };
                if operation_error.is_none()
                    && approval_recovery_error.is_none()
                    && native_error.is_none()
                {
                    Ok(summary)
                } else {
                    Err(AppShutdownError {
                        summary,
                        operation_error,
                        approval_recovery_error,
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
        AppRuntimeConstruction, AppRuntimeHandle, NativeFinalizer,
        PERSONA_APPROVAL_RECOVERY_WORKER_ID, PersonaApprovalReconciler,
        PersonaApprovalRecoveryWorker, ProductCanceller,
    };
    use crate::command_registry::command_spec;
    use crate::operation_supervisor::OperationSupervisor;
    use llama_native_host::{NativeHost, NativeHostConfig, ProcessExitJoinedNativeHost};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn runtime() -> AppRuntimeHandle {
        runtime_with_finalizer(Arc::new(AtomicBool::new(false)))
    }

    struct RecordingFinalizer {
        called: Arc<AtomicBool>,
    }

    struct ReconciliationOrderingFinalizer {
        reconciled: Arc<AtomicBool>,
        called: Arc<AtomicBool>,
    }

    struct NoopProductCanceller;

    struct RecordingPersonaApprovalReconciler {
        calls: AtomicUsize,
        called: std::sync::mpsc::SyncSender<usize>,
        error: Option<String>,
    }

    impl PersonaApprovalReconciler for RecordingPersonaApprovalReconciler {
        fn reconcile(&self) -> Result<(), String> {
            let call = self.calls.fetch_add(1, Ordering::AcqRel) + 1;
            if call <= 2 {
                let _ = self.called.send(call);
            }
            match &self.error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
    }

    impl ProductCanceller for NoopProductCanceller {
        fn cancel_all(&self) -> usize {
            0
        }
    }

    struct RecordingProductCanceller {
        sweeps: AtomicUsize,
    }

    impl RecordingProductCanceller {
        const fn new() -> Self {
            Self {
                sweeps: AtomicUsize::new(0),
            }
        }
    }

    impl ProductCanceller for RecordingProductCanceller {
        fn cancel_all(&self) -> usize {
            self.sweeps.fetch_add(1, Ordering::AcqRel);
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

    impl NativeFinalizer for ReconciliationOrderingFinalizer {
        fn shutdown(
            &self,
            _host: &Arc<NativeHost>,
        ) -> Result<ProcessExitJoinedNativeHost, mom_llama_runtime::ProductShutdownError> {
            assert!(
                self.reconciled.load(Ordering::Acquire),
                "final Persona approval recovery must precede Native finalization"
            );
            self.called.store(true, Ordering::Release);
            Err(mom_llama_runtime::ProductShutdownError::HostMissing)
        }
    }

    struct FlagPersonaApprovalReconciler {
        reconciled: Arc<AtomicBool>,
    }

    impl PersonaApprovalReconciler for FlagPersonaApprovalReconciler {
        fn reconcile(&self) -> Result<(), String> {
            self.reconciled.store(true, Ordering::Release);
            Ok(())
        }
    }

    fn runtime_with_finalizer(called: Arc<AtomicBool>) -> AppRuntimeHandle {
        runtime_with_canceller(called, Arc::new(NoopProductCanceller))
    }

    fn runtime_with_canceller(
        called: Arc<AtomicBool>,
        product_canceller: Arc<dyn ProductCanceller>,
    ) -> AppRuntimeHandle {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        AppRuntimeHandle::with_finalizers(
            host,
            None,
            product_canceller,
            Arc::new(RecordingFinalizer { called }),
        )
    }

    #[test]
    fn persona_approval_recovery_runs_after_startup_and_joins_without_sleeping() {
        let (called, calls) = std::sync::mpsc::sync_channel(2);
        let worker = PersonaApprovalRecoveryWorker::start(
            Arc::new(RecordingPersonaApprovalReconciler {
                calls: AtomicUsize::new(0),
                called,
                error: None,
            }),
            Duration::from_secs(60),
        );

        worker.tick();
        assert_eq!(calls.recv_timeout(Duration::from_secs(1)), Ok(1));
        worker.tick();
        assert_eq!(calls.recv_timeout(Duration::from_secs(1)), Ok(2));
        let terminal = worker.shutdown();
        assert!(terminal.joined);
        assert_eq!(terminal.error, None);
    }

    #[test]
    fn persona_approval_recovery_retains_terminal_error_evidence() {
        let (called, calls) = std::sync::mpsc::sync_channel(1);
        let worker = PersonaApprovalRecoveryWorker::start(
            Arc::new(RecordingPersonaApprovalReconciler {
                calls: AtomicUsize::new(0),
                called,
                error: Some("locked store".to_owned()),
            }),
            Duration::from_secs(60),
        );

        worker.tick();
        assert_eq!(calls.recv_timeout(Duration::from_secs(1)), Ok(1));
        let terminal = worker.shutdown();
        assert!(terminal.joined);
        assert_eq!(terminal.error.as_deref(), Some("locked store"));
    }

    #[tokio::test]
    async fn final_persona_approval_recovery_waits_for_admitted_work_and_precedes_native_join() {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let reconciled = Arc::new(AtomicBool::new(false));
        let native_called = Arc::new(AtomicBool::new(false));
        let runtime = AppRuntimeHandle::with_operation_supervisor(AppRuntimeConstruction {
            native_host: host,
            native_owner: None,
            product_canceller: Arc::new(NoopProductCanceller),
            native_finalizer: Arc::new(ReconciliationOrderingFinalizer {
                reconciled: Arc::clone(&reconciled),
                called: Arc::clone(&native_called),
            }),
            operation_supervisor: OperationSupervisor::new(),
            persona_approval_reconciler: Arc::new(FlagPersonaApprovalReconciler {
                reconciled: Arc::clone(&reconciled),
            }),
            persona_approval_recovery_interval: Duration::from_secs(60),
            persona_approval_authority: None,
        });
        let lease = runtime
            .admit(command_spec("mom_llama_settings_update"))
            .expect("admitted work");
        let shutdown_runtime = runtime.clone();
        let shutdown = tokio::spawn(async move { shutdown_runtime.shutdown().await });

        tokio::task::yield_now().await;
        assert!(!reconciled.load(Ordering::Acquire));
        assert!(!native_called.load(Ordering::Acquire));

        drop(lease);
        assert!(shutdown.await.expect("shutdown task").is_err());
        assert!(reconciled.load(Ordering::Acquire));
        assert!(native_called.load(Ordering::Acquire));
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

    #[test]
    fn two_runtime_characterization_keeps_injected_admission_and_cancel_seams_isolated() {
        let left_canceller = Arc::new(RecordingProductCanceller::new());
        let right_canceller = Arc::new(RecordingProductCanceller::new());
        let left = runtime_with_canceller(Arc::new(AtomicBool::new(false)), left_canceller.clone());
        let right =
            runtime_with_canceller(Arc::new(AtomicBool::new(false)), right_canceller.clone());
        let barrier = Arc::new(std::sync::Barrier::new(3));

        let close_left = {
            let barrier = Arc::clone(&barrier);
            let left = left.clone();
            std::thread::spawn(move || {
                barrier.wait();
                assert!(left.begin_quiesce());
                left.request_product_cancellation();
            })
        };
        let admit_right = {
            let barrier = Arc::clone(&barrier);
            let right = right.clone();
            std::thread::spawn(move || {
                barrier.wait();
                right.admit(command_spec("mom_llama_chat_send"))
            })
        };

        barrier.wait();
        close_left.join().expect("left quiesce thread");
        let right_lease = admit_right
            .join()
            .expect("right admission thread")
            .expect("the peer runtime must remain open");

        assert!(left.admit(command_spec("mom_llama_chat_send")).is_err());
        assert!(!right_lease.cancellation_requested());
        assert_eq!(left_canceller.sweeps.load(Ordering::Acquire), 1);
        assert_eq!(right_canceller.sweeps.load(Ordering::Acquire), 0);
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
    async fn repeated_quit_runs_one_shutdown() {
        let finalizer_called = Arc::new(AtomicBool::new(false));
        let runtime = runtime_with_finalizer(Arc::clone(&finalizer_called));
        let (first, second) = tokio::join!(runtime.shutdown(), runtime.shutdown());
        assert_eq!(first, second);
        assert!(finalizer_called.load(Ordering::Acquire));
        let summary = first
            .as_ref()
            .expect_err("the injected native finalizer fails")
            .summary
            .clone();
        assert!(
            summary
                .expected_worker_ids
                .iter()
                .any(|worker| worker == PERSONA_APPROVAL_RECOVERY_WORKER_ID)
        );
        assert!(
            summary
                .joined_worker_ids
                .iter()
                .any(|worker| worker == PERSONA_APPROVAL_RECOVERY_WORKER_ID)
        );
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
