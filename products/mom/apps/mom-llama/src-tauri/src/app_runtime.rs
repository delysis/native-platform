use llama_native_host::{NativeClient, NativeHost, ProcessExitJoinedNativeHost};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, OnceCell};

use crate::command_registry::{AdmissionClass, CommandClass, CommandSpec};
use crate::information::MomInformation;
use crate::operation_supervisor::{
    LifecyclePhase as OperationLifecyclePhase, OperationReservation, OperationSupervisor,
    TerminalClass, validate_worker_sets,
};
use crate::speech::MomSpeech;

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
    admission: AdmissionClass,
    native_request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppShutdownSummary {
    pub started_at_unix_ms: u64,
    pub completed_at_unix_ms: u64,
    pub elapsed_ms: u64,
    pub native_host_joined: bool,
    pub speech_host_joined: bool,
    pub speech_active_operation_count: usize,
    pub speech_retained_playback_count: usize,
    /// Product-owned operation-supervisor facts at the terminal boundary.
    pub operation_supervisor_phase: OperationLifecyclePhase,
    pub active_operation_count: usize,
    pub retained_operation_task_count: usize,
    /// Runtime-local chat/mention/MCP/tool-loop controls still registered
    /// after every admitted application lease drained.
    pub active_product_operation_count: usize,
    pub expected_operation_worker_count: usize,
    pub joined_operation_worker_count: usize,
    /// Resident native workers owned at the terminal drain boundary.
    pub expected_native_worker_count: usize,
    pub joined_native_worker_count: usize,
    /// Bounded recent operation-worker history plus separately tracked application
    /// workers. This is not a census of every worker during the application lifetime.
    pub expected_worker_ids: Vec<String>,
    /// Joined IDs in that diagnostic scope; actual joins remain owned by each drain.
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
    pub speech_error: Option<String>,
}

impl std::fmt::Display for AppShutdownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Blessenger shutdown did not fully succeed")?;
        if let Some(error) = &self.operation_error {
            write!(formatter, "; operation supervisor: {error}")?;
        }
        if let Some(error) = &self.approval_recovery_error {
            write!(formatter, "; Persona approval recovery: {error}")?;
        }
        if let Some(error) = &self.native_error {
            write!(formatter, "; native: {error}")?;
        }
        if let Some(error) = &self.speech_error {
            write!(formatter, "; speech: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for AppShutdownError {}

enum AppNativeHost {
    Owned(Arc<NativeHost>),
    Shared(NativeClient),
}

impl AppNativeHost {
    fn cancel(&self, request: &str, branch: Option<&str>) -> usize {
        match self {
            Self::Owned(host) => host.cancel(request, branch),
            Self::Shared(client) => client.cancel(request, branch),
        }
    }
}

struct AppRuntime {
    lifecycle: Mutex<AppLifecycle>,
    work_drained: Notify,
    native_host: AppNativeHost,
    speech: Arc<MomSpeech>,
    information: Arc<MomInformation>,
    _native_owner: Option<mom_llama_runtime::native_runtime::ProductRuntimeOwner>,
    cancellation_sweeps: AtomicU64,
    operation_scope: mom_llama_runtime::OperationScope,
    shutdown: OnceCell<Result<AppShutdownSummary, AppShutdownError>>,
    joined_native_host: Mutex<Option<ProcessExitJoinedNativeHost>>,
    native_finalizer: Arc<dyn NativeFinalizer>,
    operation_supervisor: OperationSupervisor,
    persona_approval_recovery: Arc<PersonaApprovalRecoveryWorker>,
    persona_approval_authority: Option<mom_llama_runtime::PersonaToolApprovalRecovery>,
    voice_audio_sessions: Mutex<BTreeMap<String, Arc<mom_llama_runtime::VoiceAudioSession>>>,
    voice_speculations: Mutex<BTreeMap<String, Arc<VoiceSpeculationControl>>>,
    /// Cancellation may cross the renderer/native boundary just before the
    /// long-running speculate command registers its control. Remember that
    /// exact identity so a late admission starts aborted instead of escaping
    /// the user's resumed speech or hang-up.
    early_voice_speculation_aborts: Mutex<BTreeSet<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoiceSpeculationState {
    Pending,
    Confirmed,
    Aborted,
}

#[derive(Debug)]
pub(crate) struct VoiceSpeculationControl {
    state: Mutex<VoiceSpeculationState>,
    changed: Condvar,
    prepared: Option<Arc<mom_llama_runtime::VoiceAudioPreparedTurn>>,
    recording: Mutex<Option<Vec<u8>>>,
}

pub(crate) fn complete_voice_wav(recording: &[u8]) -> bool {
    if recording.len() <= 44 || !recording.len().is_multiple_of(2) {
        return false;
    }
    let u32_at = |offset: usize| {
        u32::from_le_bytes(
            recording[offset..offset + 4]
                .try_into()
                .expect("checked WAV header length"),
        )
    };
    let u16_at = |offset: usize| {
        u16::from_le_bytes(
            recording[offset..offset + 2]
                .try_into()
                .expect("checked WAV header length"),
        )
    };
    recording.starts_with(b"RIFF")
        && u32_at(4) as usize == recording.len() - 8
        && &recording[8..16] == b"WAVEfmt "
        && u32_at(16) == 16
        && u16_at(20) == 1
        && u16_at(22) == 1
        && u32_at(24) == 16_000
        && u32_at(28) == 32_000
        && u16_at(32) == 2
        && u16_at(34) == 16
        && &recording[36..40] == b"data"
        && u32_at(40) as usize == recording.len() - 44
}

impl VoiceSpeculationControl {
    fn new(prepared: Option<Arc<mom_llama_runtime::VoiceAudioPreparedTurn>>) -> Self {
        Self {
            state: Mutex::new(VoiceSpeculationState::Pending),
            changed: Condvar::new(),
            prepared,
            recording: Mutex::new(None),
        }
    }

    pub(crate) fn prepared_turn(
        &self,
    ) -> Result<Arc<mom_llama_runtime::VoiceAudioPreparedTurn>, String> {
        self.prepared
            .as_ref()
            .cloned()
            .ok_or_else(|| "The speculative voice turn has no reusable preparation".to_string())
    }

    pub(crate) fn wait_for_confirmation(&self) -> Result<Vec<u8>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Voice speculation state is unavailable".to_string())?;
        while *state == VoiceSpeculationState::Pending {
            state = self
                .changed
                .wait(state)
                .map_err(|_| "Voice speculation state is unavailable".to_string())?;
        }
        match *state {
            VoiceSpeculationState::Confirmed => self
                .recording
                .lock()
                .map_err(|_| "Voice recording is unavailable".to_string())?
                .take()
                .ok_or_else(|| "Confirmed voice turn has no recording".to_string()),
            VoiceSpeculationState::Aborted => Err("voice_speculation_aborted".to_string()),
            VoiceSpeculationState::Pending => unreachable!("wait loop excludes pending state"),
        }
    }

    pub(crate) fn abort(&self) {
        let _ = self.transition(VoiceSpeculationState::Aborted);
    }

    fn confirm_with_recording(&self, recording: Vec<u8>) -> Result<bool, String> {
        if !complete_voice_wav(&recording) {
            return Err("The voice turn has no complete recording".to_string());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Voice speculation state is unavailable".to_string())?;
        if *state != VoiceSpeculationState::Pending {
            return Ok(*state == VoiceSpeculationState::Confirmed);
        }
        *self
            .recording
            .lock()
            .map_err(|_| "Voice recording is unavailable".to_string())? = Some(recording);
        *state = VoiceSpeculationState::Confirmed;
        self.changed.notify_all();
        Ok(true)
    }

    fn transition(&self, next: VoiceSpeculationState) -> Result<bool, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Voice speculation state is unavailable".to_string())?;
        if *state != VoiceSpeculationState::Pending {
            return Ok(*state == next);
        }
        *state = next;
        self.changed.notify_all();
        Ok(true)
    }
}

#[derive(Clone)]
pub struct AppRuntimeHandle(Arc<AppRuntime>);

pub struct AppWorkLease {
    runtime: Option<Arc<AppRuntime>>,
    occurrence: u64,
    cancellation: Option<Arc<AtomicBool>>,
    supervised: Option<OperationReservation>,
    native_request_id: Option<String>,
}

trait NativeFinalizer: Send + Sync {
    fn shutdown(&self, host: &Arc<NativeHost>) -> Result<ProcessExitJoinedNativeHost, String>;
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
            eprintln!("Blessenger approval recovery teardown: {error}");
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
                eprintln!("Blessenger persona approval recovery resumed");
            }
        }
        Err(error) => {
            if recorded_error.as_deref() != Some(error.as_str()) {
                eprintln!("Blessenger persona approval recovery: {error}");
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

struct ProductNativeFinalizer;

impl NativeFinalizer for ProductNativeFinalizer {
    fn shutdown(&self, host: &Arc<NativeHost>) -> Result<ProcessExitJoinedNativeHost, String> {
        Ok(host.shutdown_for_process_exit())
    }
}

impl AppWorkLease {
    pub fn native_request_id(&self) -> Option<&str> {
        self.native_request_id.as_deref()
    }

    pub fn cancellation_control(&self) -> Option<Arc<AtomicBool>> {
        self.cancellation.clone()
    }

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
        let Some(reservation) = self.supervised.take() else {
            return tokio::task::spawn_blocking(move || {
                // Keep the application lease alive until short blocking work
                // has actually stopped. Mutations participate in product
                // shutdown even though only cancellable long operations need
                // an operation-supervisor reservation.
                let _app_lease = self;
                operation()
            })
            .await
            .map_err(|error| format!("Blessenger blocking operation worker failed: {error}"))?;
        };
        let supervisor = reservation
            .lease
            .supervisor()
            .ok_or_else(|| "Blessenger's operation supervisor is unavailable".to_owned())?;
        let task = supervisor
            .spawn(reservation, move |lease| {
                if lease.cancellation_requested() {
                    return Err(
                        "Blessenger cancelled the operation during application shutdown".to_owned(),
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
        let Some(reservation) = self.supervised.take() else {
            return tokio::task::spawn_blocking(move || {
                let _app_lease = self;
                operation().map(|(value, _authoritative_cancellation)| value)
            })
            .await
            .map_err(|error| format!("Blessenger blocking operation worker failed: {error}"))?;
        };
        let supervisor = reservation
            .lease
            .supervisor()
            .ok_or_else(|| "Blessenger's operation supervisor is unavailable".to_owned())?;
        let task = supervisor
            .spawn(reservation, move |lease| {
                if lease.cancellation_requested() {
                    return Err(
                        "Blessenger cancelled the operation during application shutdown".to_owned(),
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
            .ok_or_else(|| "Blessenger's operation supervisor is unavailable".to_owned())?;
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
    native_host: AppNativeHost,
    speech: Arc<MomSpeech>,
    information: Arc<MomInformation>,
    native_owner: Option<mom_llama_runtime::native_runtime::ProductRuntimeOwner>,
    operation_scope: mom_llama_runtime::OperationScope,
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
        speech: Arc<MomSpeech>,
        information: Arc<MomInformation>,
    ) -> Self {
        let native_host = native_owner.host();
        let persona_approval_authority = persona_approval_recovery.clone();
        Self::with_operation_supervisor(AppRuntimeConstruction {
            operation_scope: native_owner.operation_scope(),
            native_host: AppNativeHost::Owned(native_host),
            speech,
            information,
            native_owner: Some(native_owner),
            native_finalizer: Arc::new(ProductNativeFinalizer),
            operation_supervisor: OperationSupervisor::new(),
            persona_approval_reconciler: Arc::new(RuntimePersonaApprovalReconciler {
                recovery: persona_approval_recovery,
            }),
            persona_approval_recovery_interval: PERSONA_APPROVAL_RECOVERY_INTERVAL,
            persona_approval_authority: Some(persona_approval_authority),
        })
    }

    pub(crate) fn new_shared(
        client: NativeClient,
        operation_scope: mom_llama_runtime::OperationScope,
        persona_approval_recovery: mom_llama_runtime::PersonaToolApprovalRecovery,
        speech: Arc<MomSpeech>,
        information: Arc<MomInformation>,
    ) -> Self {
        let authority = persona_approval_recovery.clone();
        Self::with_operation_supervisor(AppRuntimeConstruction {
            native_host: AppNativeHost::Shared(client),
            operation_scope,
            speech,
            information,
            native_owner: None,
            native_finalizer: Arc::new(ProductNativeFinalizer),
            operation_supervisor: OperationSupervisor::new(),
            persona_approval_reconciler: Arc::new(RuntimePersonaApprovalReconciler {
                recovery: persona_approval_recovery,
            }),
            persona_approval_recovery_interval: PERSONA_APPROVAL_RECOVERY_INTERVAL,
            persona_approval_authority: Some(authority),
        })
    }

    #[cfg(test)]
    fn with_finalizers(
        native_host: Arc<NativeHost>,
        native_owner: Option<mom_llama_runtime::native_runtime::ProductRuntimeOwner>,
        native_finalizer: Arc<dyn NativeFinalizer>,
    ) -> Self {
        Self::with_operation_supervisor(AppRuntimeConstruction {
            operation_scope: mom_llama_runtime::OperationScope::for_native_host(&native_host),
            native_host: AppNativeHost::Owned(native_host),
            speech: MomSpeech::empty_for_tests(),
            information: MomInformation::empty_for_tests(),
            native_owner,
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
            speech,
            information,
            native_owner,
            operation_scope,
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
            speech,
            information,
            _native_owner: native_owner,
            cancellation_sweeps: AtomicU64::new(0),
            operation_scope,
            shutdown: OnceCell::new(),
            joined_native_host: Mutex::new(None),
            native_finalizer,
            operation_supervisor,
            persona_approval_recovery,
            persona_approval_authority,
            voice_audio_sessions: Mutex::new(BTreeMap::new()),
            voice_speculations: Mutex::new(BTreeMap::new()),
            early_voice_speculation_aborts: Mutex::new(BTreeSet::new()),
        }))
    }

    pub fn admit(&self, command: &'static CommandSpec) -> Result<AppWorkLease, String> {
        if let Some(error) = self.0.persona_approval_recovery.current_error() {
            return Err(format!(
                "Blessenger's persona approval recovery worker is unavailable: {error}"
            ));
        }
        let mut lifecycle = self
            .0
            .lifecycle
            .lock()
            .map_err(|_| "Blessenger application state is unavailable".to_string())?;
        if lifecycle.phase != AppPhase::Running {
            return Err("Blessenger is shutting down; new work is not admitted".to_string());
        }
        if command.admission == AdmissionClass::Speculative && !lifecycle.active_work.is_empty() {
            return Err(
                "Blessenger is busy with foreground work; speculative autocomplete was not admitted"
                    .to_string(),
            );
        }
        let preempted = if command.admission == AdmissionClass::Foreground {
            lifecycle
                .active_work
                .values()
                .filter(|work| work.admission == AdmissionClass::Speculative)
                .map(|work| (work.cancellation.clone(), work.native_request_id.clone()))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let occurrence = lifecycle
            .next_occurrence
            .checked_add(1)
            .ok_or_else(|| "Blessenger has too many active operations".to_string())?;
        lifecycle.next_occurrence = occurrence;
        let native_request_id = (command.admission == AdmissionClass::Speculative)
            .then(|| format!("{}:{occurrence}", command.name));
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
        for (cancellation, _) in &preempted {
            if let Some(cancellation) = cancellation {
                cancellation.store(true, Ordering::Release);
            }
        }
        let replaced = lifecycle.active_work.insert(
            occurrence,
            ActiveWork {
                command: command.name,
                cancellation: cancellation.clone(),
                admission: command.admission,
                native_request_id: native_request_id.clone(),
            },
        );
        debug_assert!(replaced.is_none());
        drop(lifecycle);
        for (_, request_id) in preempted {
            if let Some(request_id) = request_id {
                self.0.native_host.cancel(&request_id, None);
            }
        }
        Ok(AppWorkLease {
            runtime: Some(Arc::clone(&self.0)),
            occurrence,
            cancellation,
            supervised,
            native_request_id,
        })
    }

    pub fn operation_scope(&self) -> mom_llama_runtime::OperationScope {
        self.0.operation_scope.clone()
    }

    pub fn insert_voice_audio_session(
        &self,
        session: mom_llama_runtime::VoiceAudioSession,
    ) -> Result<String, String> {
        let id = session.request_id().to_string();
        let mut sessions = self
            .0
            .voice_audio_sessions
            .lock()
            .map_err(|_| "Blessenger voice session state is unavailable".to_string())?;
        if sessions.insert(id.clone(), Arc::new(session)).is_some() {
            return Err("Blessenger generated a duplicate voice session identity".to_string());
        }
        Ok(id)
    }

    pub fn push_voice_audio(&self, session: &str, samples: Vec<f32>) -> Result<usize, String> {
        let voice = self
            .0
            .voice_audio_sessions
            .lock()
            .map_err(|_| "Blessenger voice session state is unavailable".to_string())?
            .get(session)
            .ok_or_else(|| "The live voice session is no longer active".to_string())?
            .clone();
        voice.push_pcm(&samples).map_err(|error| error.to_string())
    }

    pub fn voice_audio_session(
        &self,
        session: &str,
    ) -> Result<Arc<mom_llama_runtime::VoiceAudioSession>, String> {
        self.0
            .voice_audio_sessions
            .lock()
            .map_err(|_| "Blessenger voice session state is unavailable".to_string())?
            .get(session)
            .cloned()
            .ok_or_else(|| "The live voice session is no longer active".to_string())
    }

    pub fn take_voice_audio_session(
        &self,
        session: &str,
    ) -> Result<mom_llama_runtime::VoiceAudioSession, String> {
        let voice = self
            .0
            .voice_audio_sessions
            .lock()
            .map_err(|_| "Blessenger voice session state is unavailable".to_string())?
            .remove(session)
            .ok_or_else(|| "The live voice session is no longer active".to_string())?;
        Arc::try_unwrap(voice)
            .map_err(|_| "The live voice session still has an in-flight audio block".to_string())
    }

    pub fn abort_voice_audio_session(&self, session: &str) -> Result<usize, String> {
        let voice = self
            .0
            .voice_audio_sessions
            .lock()
            .map_err(|_| "Blessenger voice session state is unavailable".to_string())?
            .remove(session)
            .ok_or_else(|| "The live voice session is no longer active".to_string())?;
        Ok(voice.cancel())
    }

    pub(crate) fn begin_voice_speculation_with_prepared(
        &self,
        speculation: &str,
        prepared: Option<Arc<mom_llama_runtime::VoiceAudioPreparedTurn>>,
    ) -> Result<Arc<VoiceSpeculationControl>, String> {
        validate_voice_speculation_id(speculation)?;
        let control = Arc::new(VoiceSpeculationControl::new(prepared));
        let mut speculations = self
            .0
            .voice_speculations
            .lock()
            .map_err(|_| "Blessenger voice speculation state is unavailable".to_string())?;
        let mut early_aborts = self
            .0
            .early_voice_speculation_aborts
            .lock()
            .map_err(|_| "Blessenger voice speculation state is unavailable".to_string())?;
        if early_aborts.remove(speculation) {
            return Err("voice_speculation_aborted".to_string());
        }
        if speculations
            .insert(speculation.to_string(), Arc::clone(&control))
            .is_some()
        {
            return Err("Blessenger generated a duplicate voice speculation identity".to_string());
        }
        Ok(control)
    }

    pub(crate) fn confirm_voice_speculation(
        &self,
        speculation: &str,
        recording: Vec<u8>,
    ) -> Result<bool, String> {
        let control = self
            .0
            .voice_speculations
            .lock()
            .map_err(|_| "Blessenger voice speculation state is unavailable".to_string())?
            .get(speculation)
            .cloned()
            .ok_or_else(|| "The speculative voice turn is no longer active".to_string())?;
        control.confirm_with_recording(recording)
    }

    /// Abort and consume an admitted speculation. If admission has not yet
    /// happened, retain the early-abort tombstone so the racing command cannot
    /// resurrect it after the user has stopped capture.
    pub(crate) fn abort_and_remove_voice_speculation(
        &self,
        speculation: &str,
    ) -> Result<bool, String> {
        validate_voice_speculation_id(speculation)?;
        let control = self
            .0
            .voice_speculations
            .lock()
            .map_err(|_| "Blessenger voice speculation state is unavailable".to_string())?
            .remove(speculation);
        if let Some(control) = control {
            let aborted = control.transition(VoiceSpeculationState::Aborted)?;
            self.0
                .early_voice_speculation_aborts
                .lock()
                .map_err(|_| "Blessenger voice speculation state is unavailable".to_string())?
                .remove(speculation);
            return Ok(aborted);
        }
        self.0
            .early_voice_speculation_aborts
            .lock()
            .map_err(|_| "Blessenger voice speculation state is unavailable".to_string())?
            .insert(speculation.to_string());
        Ok(true)
    }

    /// Atomically consume the failed attempt's immutable preparation for a
    /// resumed-speech replacement. The producer may have already cancelled
    /// and joined; keeping cleanup consumer-owned prevents that completion
    /// race from discarding the only checkpoint-compatible turn description.
    pub(crate) fn take_voice_speculation_for_handoff(
        &self,
        speculation: &str,
    ) -> Result<(bool, Arc<mom_llama_runtime::VoiceAudioPreparedTurn>), String> {
        validate_voice_speculation_id(speculation)?;
        let control = self
            .0
            .voice_speculations
            .lock()
            .map_err(|_| "Blessenger voice speculation state is unavailable".to_string())?
            .remove(speculation)
            .ok_or_else(|| "The speculative voice turn is no longer active".to_string())?;
        self.0
            .early_voice_speculation_aborts
            .lock()
            .map_err(|_| "Blessenger voice speculation state is unavailable".to_string())?
            .remove(speculation);
        let prepared = control.prepared_turn()?;
        let aborted = control.transition(VoiceSpeculationState::Aborted)?;
        Ok((aborted, prepared))
    }

    pub(crate) fn finish_voice_speculation(&self, speculation: &str) {
        let _ = self
            .0
            .voice_speculations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(speculation);
        let _ = self
            .0
            .early_voice_speculation_aborts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(speculation);
    }

    pub fn speech(&self) -> Arc<MomSpeech> {
        Arc::clone(&self.0.speech)
    }

    pub fn information(&self) -> Arc<MomInformation> {
        Arc::clone(&self.0.information)
    }

    pub fn cancel_speculative(&self) -> Result<usize, String> {
        let request_ids = self
            .0
            .lifecycle
            .lock()
            .map_err(|_| "Blessenger's operation registry is unavailable".to_string())?
            .active_work
            .values()
            .filter(|work| work.admission == AdmissionClass::Speculative)
            .filter_map(|work| {
                if let Some(cancellation) = &work.cancellation {
                    cancellation.store(true, Ordering::Release);
                }
                work.native_request_id.clone()
            })
            .collect::<Vec<_>>();
        for request_id in &request_ids {
            self.0.native_host.cancel(request_id, None);
        }
        Ok(request_ids.len())
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
            .ok_or_else(|| "Blessenger's Persona approval authority is unavailable".to_string())
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
        self.0.speech.begin_quiesce();
        true
    }

    /// Drain embedded product services without finalizing the application host.
    pub(crate) async fn drain_shared_services(
        &self,
    ) -> Result<AppShutdownSummary, AppShutdownError> {
        assert!(matches!(&self.0.native_host, AppNativeHost::Shared(_)));
        self.shutdown().await
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
                let voice_sessions = self
                    .0
                    .voice_audio_sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                for voice in voice_sessions {
                    voice.cancel();
                }
                self.0
                    .voice_audio_sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clear();
                let speculations = self
                    .0
                    .voice_speculations
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                for speculation in speculations {
                    let _ = speculation.transition(VoiceSpeculationState::Aborted);
                }
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
                if supervisor.state_poisoned {
                    operation_errors.push(
                        "operation supervisor state was poisoned; owned workers were drained but state integrity is unproven".to_owned(),
                    );
                }
                if !validate_worker_sets(&supervisor) {
                    operation_errors.push(
                        "operation worker join identities did not match admitted worker identities"
                            .to_owned(),
                    );
                }
                let operation_error =
                    {
                        let active_product_operations =
                            self.0.operation_scope.active_operation_count();
                        if active_product_operations != 0 {
                            operation_errors.push(format!(
                                "{active_product_operations} runtime-local product operation controls remained after application work drained"
                            ));
                        }
                        (!operation_errors.is_empty()).then(|| operation_errors.join("; "))
                    };
                let speech_shutdown = self.0.speech.shutdown().await;
                let (speech_error, speech_host_joined, speech_active_operation_count, speech_retained_playback_count) =
                    match speech_shutdown {
                        Ok(receipt) => (
                            None,
                            receipt.host_joined,
                            receipt.active_operation_count,
                            receipt.retained_playback_count,
                        ),
                        Err(error) => (Some(error), false, 0, 0),
                    };
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
                let (native_worker_ids, native_error, joined_native_worker_count, native_host_joined) =
                    match &self.0.native_host {
                        AppNativeHost::Owned(host) => {
                            let workers = host.slots().into_iter()
                                .map(|slot| format!("mom-native-slot-{}", slot.slot_id)).collect::<Vec<_>>();
                            match self.0.native_finalizer.shutdown(host) {
                                Ok(receipt) => {
                                    let count = receipt.joined_worker_count();
                                    self.0.joined_native_host.lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner).replace(receipt);
                                    (workers, None, count, true)
                                }
                                Err(error) => (workers, Some(error), 0, false),
                            }
                        }
                        // The application coordinator joins the shared host only
                        // after both product domains have drained.
                        AppNativeHost::Shared(_) => (Vec::new(), None, 0, false),
                    };
                let expected_native_worker_count = native_worker_ids.len();
                let operation_supervisor_phase = supervisor.phase;
                let active_operation_count = supervisor.active_operations;
                let retained_operation_task_count = supervisor.retained_tasks;
                let active_product_operation_count =
                    self.0.operation_scope.active_operation_count();
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
                    native_host_joined,
                    speech_host_joined,
                    speech_active_operation_count,
                    speech_retained_playback_count,
                    operation_supervisor_phase,
                    active_operation_count,
                    retained_operation_task_count,
                    active_product_operation_count,
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
                    && speech_error.is_none()
                {
                    Ok(summary)
                } else {
                    Err(AppShutdownError {
                        summary,
                        operation_error,
                        approval_recovery_error,
                        native_error,
                        speech_error,
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
        let _ = self.0.operation_scope.request_cancellation();
    }
}

fn validate_voice_speculation_id(speculation: &str) -> Result<(), String> {
    if speculation.is_empty()
        || speculation.len() > 64
        || !speculation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("Voice speculation identity is invalid".to_string());
    }
    Ok(())
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
        AppRuntimeConstruction, AppRuntimeHandle, MomSpeech, NativeFinalizer,
        PERSONA_APPROVAL_RECOVERY_WORKER_ID, PersonaApprovalReconciler,
        PersonaApprovalRecoveryWorker, VoiceSpeculationControl, complete_voice_wav,
    };
    use crate::command_registry::command_spec;
    use crate::information::MomInformation;
    use crate::operation_supervisor::{
        LifecyclePhase as OperationLifecyclePhase, OperationSupervisor, SupervisorError,
    };
    use llama_native_host::{NativeHost, NativeHostConfig, ProcessExitJoinedNativeHost};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn runtime() -> AppRuntimeHandle {
        // This composes the real Information private store. Tests using it run
        // only where that storage capability is implemented; independent
        // supervisor and approval-worker tests remain portable.
        runtime_with_finalizer(Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn speculative_turn_requires_complete_recording_before_confirmation() {
        let control = VoiceSpeculationControl::new(None);
        assert!(control.confirm_with_recording(Vec::new()).is_err());
        let mut recording = vec![0; 46];
        recording[..4].copy_from_slice(b"RIFF");
        recording[4..8].copy_from_slice(&38_u32.to_le_bytes());
        recording[8..16].copy_from_slice(b"WAVEfmt ");
        recording[16..20].copy_from_slice(&16_u32.to_le_bytes());
        recording[20..22].copy_from_slice(&1_u16.to_le_bytes());
        recording[22..24].copy_from_slice(&1_u16.to_le_bytes());
        recording[24..28].copy_from_slice(&16_000_u32.to_le_bytes());
        recording[28..32].copy_from_slice(&32_000_u32.to_le_bytes());
        recording[32..34].copy_from_slice(&2_u16.to_le_bytes());
        recording[34..36].copy_from_slice(&16_u16.to_le_bytes());
        recording[36..40].copy_from_slice(b"data");
        recording[40..44].copy_from_slice(&2_u32.to_le_bytes());
        assert!(complete_voice_wav(&recording));
        let mut truncated = recording.clone();
        truncated.pop();
        assert!(!complete_voice_wav(&truncated));
        assert_eq!(control.confirm_with_recording(recording.clone()), Ok(true));
        assert_eq!(control.wait_for_confirmation(), Ok(recording));
    }

    #[test]
    #[cfg(unix)]
    fn resumed_speech_before_speculation_admission_is_not_lost() {
        let runtime = runtime();
        let speculation = "early-abort-fixture";
        assert_eq!(
            runtime.abort_and_remove_voice_speculation(speculation),
            Ok(true)
        );
        assert_eq!(
            runtime
                .begin_voice_speculation_with_prepared(speculation, None)
                .expect_err("an early abort must close later admission"),
            "voice_speculation_aborted"
        );
        assert!(
            runtime
                .begin_voice_speculation_with_prepared(speculation, None)
                .is_ok(),
            "the consumed early-abort identity must not poison future admission"
        );
        runtime.finish_voice_speculation(speculation);
    }

    #[test]
    #[cfg(unix)]
    fn failed_speculation_remains_until_its_abort_consumer_removes_it() {
        let runtime = runtime();
        let speculation = "consumer-owned-failure";
        let control = runtime
            .begin_voice_speculation_with_prepared(speculation, None)
            .expect("speculation is admitted");
        control.abort();
        assert!(
            runtime
                .0
                .voice_speculations
                .lock()
                .expect("speculation registry")
                .contains_key(speculation),
            "producer completion must not erase handoff state"
        );
        assert_eq!(
            runtime.abort_and_remove_voice_speculation(speculation),
            Ok(true)
        );
        assert!(
            !runtime
                .0
                .voice_speculations
                .lock()
                .expect("speculation registry")
                .contains_key(speculation)
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn short_blocking_mutation_runs_without_a_long_operation_reservation() {
        let runtime = runtime();
        let lease = runtime
            .admit(command_spec("mom_llama_voice_audio_push"))
            .expect("short mutation admission");
        assert!(lease.supervised.is_none());

        let value = lease
            .run_blocking(|| Ok::<_, String>(7_u8))
            .await
            .expect("short blocking mutation");

        assert_eq!(value, 7);
        assert!(
            runtime
                .0
                .lifecycle
                .lock()
                .expect("application lifecycle")
                .active_work
                .is_empty(),
            "the short worker must release its application lease"
        );
    }

    #[tokio::test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn embedded_services_drain_before_the_application_native_join() {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let captured = Arc::clone(&host);
        let owner = llama_native_host::ApplicationNativeOwner::new(move || {
            Ok(llama_native_host::ApplicationHost {
                host: Arc::clone(&captured),
                binding: "test-binding".into(),
                memory_budget_bytes: 1024,
            })
        });
        let client = owner.client().expect("chat client");
        assert_eq!(
            client.binding().expect("initialize shared host"),
            "test-binding"
        );
        let runtime = AppRuntimeHandle::with_operation_supervisor(super::AppRuntimeConstruction {
            native_host: super::AppNativeHost::Shared(client.clone()),
            operation_scope: mom_llama_runtime::OperationScope::for_native_host(&host),
            speech: MomSpeech::empty_for_tests(),
            information: MomInformation::empty_for_tests(),
            native_owner: None,
            native_finalizer: Arc::new(RecordingFinalizer {
                called: Arc::new(AtomicBool::new(false)),
            }),
            operation_supervisor: OperationSupervisor::new(),
            persona_approval_reconciler: Arc::new(super::NoopPersonaApprovalReconciler),
            persona_approval_recovery_interval: super::PERSONA_APPROVAL_RECOVERY_INTERVAL,
            persona_approval_authority: None,
        });
        let receipt = runtime
            .drain_shared_services()
            .await
            .expect("shared services drained");
        assert!(!receipt.native_host_joined);
        assert_eq!(receipt.joined_native_worker_count, 0);
        assert_eq!(
            client.binding().expect("product did not finalize host"),
            "test-binding"
        );
        let proof = owner.shutdown_joined().expect("sole application join");
        assert!(proof.belongs_to(&client));
        assert!(client.binding().is_err());
    }

    struct RecordingFinalizer {
        called: Arc<AtomicBool>,
    }

    struct ReconciliationOrderingFinalizer {
        reconciled: Arc<AtomicBool>,
        called: Arc<AtomicBool>,
    }

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

    impl NativeFinalizer for RecordingFinalizer {
        fn shutdown(&self, _host: &Arc<NativeHost>) -> Result<ProcessExitJoinedNativeHost, String> {
            self.called.store(true, Ordering::Release);
            Err("native host unavailable".to_string())
        }
    }

    impl NativeFinalizer for ReconciliationOrderingFinalizer {
        fn shutdown(&self, _host: &Arc<NativeHost>) -> Result<ProcessExitJoinedNativeHost, String> {
            assert!(
                self.reconciled.load(Ordering::Acquire),
                "final Persona approval recovery must precede Native finalization"
            );
            self.called.store(true, Ordering::Release);
            Err("native host unavailable".to_string())
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
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        AppRuntimeHandle::with_finalizers(host, None, Arc::new(RecordingFinalizer { called }))
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
    #[cfg(unix)]
    async fn final_persona_approval_recovery_waits_for_admitted_work_and_precedes_native_join() {
        let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
        let reconciled = Arc::new(AtomicBool::new(false));
        let native_called = Arc::new(AtomicBool::new(false));
        let runtime = AppRuntimeHandle::with_operation_supervisor(AppRuntimeConstruction {
            operation_scope: mom_llama_runtime::OperationScope::for_native_host(&host),
            native_host: super::AppNativeHost::Owned(host),
            speech: MomSpeech::empty_for_tests(),
            information: MomInformation::empty_for_tests(),
            native_owner: None,
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
    #[cfg(unix)]
    fn quiesce_closes_admission_once() {
        let runtime = runtime();
        let command = command_spec("mom_llama_settings_update");
        drop(runtime.admit(command).expect("running admission"));
        assert!(runtime.begin_quiesce());
        assert!(!runtime.begin_quiesce());
        assert!(runtime.admit(command).is_err());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn poisoned_operation_supervisor_drains_workers_and_remains_a_shutdown_error() {
        let native_called = Arc::new(AtomicBool::new(false));
        let runtime = runtime_with_finalizer(Arc::clone(&native_called));
        let supervisor = runtime.0.operation_supervisor.clone();
        let reservation = supervisor.reserve("poison-drain").expect("reserve worker");
        let worker_id = format!(
            "mom-operation-worker-{}",
            reservation.lease.identity().sequence
        );
        let (release, blocked) = std::sync::mpsc::sync_channel(0);
        let task = supervisor
            .spawn(reservation, move |_| {
                blocked.recv().expect("release worker during shutdown");
                Ok(())
            })
            .expect("spawn worker");
        let operation = supervisor
            .create_operation("pre-poison-operation")
            .expect("operation");
        let pending = supervisor
            .reserve("pre-poison-reservation")
            .expect("pending reservation");
        supervisor.poison_state_for_test();
        assert!(matches!(
            supervisor.reserve("after-poison"),
            Err(SupervisorError::StatePoisoned)
        ));
        assert!(matches!(
            supervisor.start_attempt(&operation),
            Err(SupervisorError::StatePoisoned)
        ));
        let rejected_worker_ran = Arc::new(AtomicBool::new(false));
        let ran = Arc::clone(&rejected_worker_ran);
        assert!(matches!(
            supervisor.spawn(pending, move |_| {
                ran.store(true, Ordering::Release);
                Ok(())
            }),
            Err(SupervisorError::StatePoisoned)
        ));
        assert!(!rejected_worker_ran.load(Ordering::Acquire));
        assert_eq!(
            supervisor.active_count(),
            1,
            "only the already-running worker remains"
        );
        supervisor
            .finish_operation(&operation)
            .expect("cleanup still recovers poisoned state");
        let worker_supervisor = supervisor.clone();
        let finalizer_called = Arc::clone(&native_called);
        let releaser = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while worker_supervisor.phase() == OperationLifecyclePhase::Running {
                assert!(
                    std::time::Instant::now() < deadline,
                    "shutdown did not quiesce"
                );
                std::thread::yield_now();
            }
            assert!(!finalizer_called.load(Ordering::Acquire));
            release.send(()).expect("release active worker");
        });
        let error = runtime.shutdown().await.expect_err("faulted shutdown");
        releaser.join().expect("release coordinator");
        drop(task);
        assert!(
            native_called.load(Ordering::Acquire),
            "native cleanup still runs"
        );
        assert_eq!(error.summary.active_operation_count, 0);
        assert_eq!(error.summary.retained_operation_task_count, 0);
        assert_eq!(error.summary.expected_operation_worker_count, 1);
        assert_eq!(error.summary.joined_operation_worker_count, 1);
        assert!(error.summary.joined_worker_ids.contains(&worker_id));
        assert!(
            error
                .operation_error
                .as_deref()
                .is_some_and(|message| message.contains("poison")),
            "supervisor poison was lost: {error:?}"
        );
        assert_eq!(
            runtime
                .shutdown()
                .await
                .expect_err("retained shutdown")
                .operation_error,
            error.operation_error
        );
    }

    #[test]
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn speculative_admission_is_single_flight_and_yields_to_foreground() {
        let runtime = runtime();
        let speculative = runtime
            .admit(command_spec("mom_llama_composer_autocomplete"))
            .expect("first speculative admission");
        assert!(speculative.native_request_id().is_some());
        assert!(
            runtime
                .admit(command_spec("mom_llama_composer_autocomplete"))
                .is_err(),
            "a second speculative request must not queue behind the first"
        );

        let foreground = runtime
            .admit(command_spec("mom_llama_settings_update"))
            .expect("foreground overtakes speculative work");
        assert!(speculative.cancellation_requested());
        assert!(
            runtime
                .admit(command_spec("mom_llama_composer_autocomplete"))
                .is_err(),
            "speculation must remain closed while foreground work is active"
        );

        drop(speculative);
        drop(foreground);
        drop(
            runtime
                .admit(command_spec("mom_llama_composer_autocomplete"))
                .expect("speculation reopens only after foreground drain"),
        );
    }

    #[test]
    #[cfg(unix)]
    fn explicit_speculative_cancel_is_runtime_local_and_idempotent() {
        let left = runtime();
        let right = runtime();
        let left_lease = left
            .admit(command_spec("mom_llama_composer_autocomplete"))
            .expect("left speculative admission");
        let right_lease = right
            .admit(command_spec("mom_llama_composer_autocomplete"))
            .expect("right speculative admission");

        assert_eq!(left.cancel_speculative().expect("cancel left"), 1);
        assert!(left_lease.cancellation_requested());
        assert!(!right_lease.cancellation_requested());
        assert_eq!(left.cancel_speculative().expect("repeat cancel left"), 1);
    }

    #[test]
    #[cfg(unix)]
    fn two_runtime_characterization_keeps_injected_admission_and_cancel_seams_isolated() {
        let left = runtime();
        let right = runtime();
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
        assert_eq!(left.0.cancellation_sweeps.load(Ordering::Acquire), 1);
        assert_eq!(right.0.cancellation_sweeps.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
        assert!(summary.speech_host_joined);
        assert_eq!(summary.speech_active_operation_count, 0);
        assert_eq!(summary.speech_retained_playback_count, 0);
        assert_eq!(summary.active_product_operation_count, 0);
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
    #[cfg(unix)]
    fn model_select_vs_quit_has_one_winner() {
        command_vs_quit_has_one_winner("mom_llama_model_select");
    }

    #[test]
    #[cfg(unix)]
    fn settings_update_vs_quit_has_one_winner() {
        command_vs_quit_has_one_winner("mom_llama_settings_update");
    }

    #[test]
    #[cfg(unix)]
    fn receipt_writing_read_vs_quit_has_one_winner() {
        command_vs_quit_has_one_winner("mom_llama_conversation_list");
    }

    #[test]
    #[cfg(unix)]
    fn native_read_vs_quit_has_one_winner() {
        command_vs_quit_has_one_winner("mom_llama_model_slot_list");
    }
}
