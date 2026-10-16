mod app_runtime;
mod command_registry;
mod commands;
mod information;
mod model_acquisition;
// Capture/host stage only; activation awaits the runtime and KV owners.
#[allow(dead_code)]
mod live_transcription;
mod native_voice_capture;
mod operation_supervisor;
mod speech;
mod view;
use anyhow::Result;
use app_runtime::AppRuntimeHandle;
use serde_json::json;
use speech_native_types::SpeechBackend;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{
    AboutMetadata, HELP_SUBMENU_ID, Menu, MenuItem, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

const APPLICATION_SETTINGS_MENU_ID: &str = "mom-llama.application.settings";

const APPLICATION_QUIT_MENU_ID: &str = "mom-llama.application.quit";
const APPLICATION_QUIT_ACCELERATOR: &str = "CmdOrCtrl+Q";

#[derive(Clone)]
struct PreparedSpeech {
    qwen: Arc<Mutex<Arc<speech_native_backend_qwen::QwenSpeechBackend>>>,
    claimed_by_runtime: Arc<AtomicBool>,
    shutdown_started: Arc<AtomicBool>,
    started: std::time::Instant,
}

impl PreparedSpeech {
    fn new(app_data_dir: &std::path::Path) -> Self {
        let qwen = desktop_speech::prepare_local_qwen(
            Some(app_data_dir.join("speech-models")),
            Some(app_data_dir.join("speech-voices/qwen-icl")),
        );
        Self {
            qwen: Arc::new(Mutex::new(qwen)),
            claimed_by_runtime: Arc::new(AtomicBool::new(false)),
            shutdown_started: Arc::new(AtomicBool::new(false)),
            started: std::time::Instant::now(),
        }
    }

    fn backend(&self) -> Arc<speech_native_backend_qwen::QwenSpeechBackend> {
        self.qwen.lock().expect("prepared speech lock").clone()
    }

    fn refresh(&self, app_data_dir: &std::path::Path, voice_root: &std::path::Path) {
        let qwen = desktop_speech::prepare_local_qwen(
            Some(app_data_dir.join("speech-models")),
            Some(voice_root.to_path_buf()),
        );
        *self.qwen.lock().expect("prepared speech lock") = qwen;
    }

    fn claim(&self) {
        self.claimed_by_runtime.store(true, Ordering::Release);
    }

    async fn shutdown_if_unclaimed(&self) -> bool {
        if self.claimed_by_runtime.load(Ordering::Acquire) {
            return true;
        }
        if self
            .shutdown_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.backend().shutdown().await.is_ok()
    }
}

#[derive(Clone)]
struct StartupController {
    state: Arc<Mutex<StartupState>>,
}

enum StartupState {
    Idle,
    Unlocking,
    Building,
    Ready(AppRuntimeHandle),
    Failed,
    RestartRequired,
    QuiescingSafe(Option<AppRuntimeHandle>),
    QuiescingDuringBuild,
    QuiescingBlocked(AppRuntimeHandle),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupAction {
    AlreadyReady,
    Initialize { retry_cached_failure: bool },
}

enum StartupShutdownAction {
    Start(AppRuntimeHandle),
    AlreadyQuiescing,
    WaitForBuild,
    NoRuntime,
}

enum FinalExitTarget {
    NoRuntime,
    Runtime(AppRuntimeHandle),
    WaitForBuild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalExitDecision {
    ReturnToTauri,
    AbortWithoutRustTeardown,
}

struct RuntimeBuildError {
    message: String,
}

impl std::fmt::Display for RuntimeBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeBuildError {}

impl std::fmt::Debug for RuntimeBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeBuildError")
            .field("message", &self.message)
            .finish()
    }
}

impl StartupController {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(StartupState::Idle)),
        }
    }

    fn begin_initialization(&self) -> Result<StartupAction, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Blessenger startup state is unavailable".to_string())?;
        match &*state {
            StartupState::Ready(_) => Ok(StartupAction::AlreadyReady),
            StartupState::Unlocking | StartupState::Building => {
                Err("Blessenger is already unlocking its encrypted local data".to_string())
            }
            StartupState::QuiescingSafe(_)
            | StartupState::QuiescingDuringBuild
            | StartupState::QuiescingBlocked(_) => Err("Blessenger is shutting down".to_string()),
            StartupState::RestartRequired => Err(
                "Blessenger must be restarted before local runtime startup can retry".to_string(),
            ),
            StartupState::Idle => {
                *state = StartupState::Unlocking;
                Ok(StartupAction::Initialize {
                    retry_cached_failure: false,
                })
            }
            StartupState::Failed => {
                *state = StartupState::Unlocking;
                Ok(StartupAction::Initialize {
                    retry_cached_failure: true,
                })
            }
        }
    }

    fn begin_runtime_build(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Blessenger startup state is unavailable".to_string())?;
        match &*state {
            StartupState::Unlocking => {
                *state = StartupState::Building;
                Ok(())
            }
            StartupState::QuiescingSafe(_) => Err("Blessenger is shutting down".to_string()),
            _ => Err("Blessenger startup changed phase unexpectedly".to_string()),
        }
    }

    fn install<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        runtime: AppRuntimeHandle,
    ) -> Result<(), AppRuntimeHandle> {
        let Ok(mut state) = self.state.lock() else {
            return Err(runtime);
        };
        if !matches!(&*state, StartupState::Building) || !app.manage(runtime.clone()) {
            return Err(runtime);
        }
        *state = StartupState::Ready(runtime);
        Ok(())
    }

    fn record_failure(&self) {
        if let Ok(mut state) = self.state.lock()
            && matches!(&*state, StartupState::Unlocking | StartupState::Building)
        {
            *state = StartupState::Failed;
        }
    }

    fn begin_shutdown(&self) -> StartupShutdownAction {
        let Ok(mut state) = self.state.lock() else {
            return StartupShutdownAction::AlreadyQuiescing;
        };
        match &*state {
            StartupState::Ready(runtime) => {
                let runtime = runtime.clone();
                *state = StartupState::QuiescingSafe(Some(runtime.clone()));
                StartupShutdownAction::Start(runtime)
            }
            StartupState::Building => {
                *state = StartupState::QuiescingDuringBuild;
                StartupShutdownAction::WaitForBuild
            }
            StartupState::QuiescingDuringBuild => StartupShutdownAction::AlreadyQuiescing,
            StartupState::QuiescingSafe(_) | StartupState::QuiescingBlocked(_) => {
                StartupShutdownAction::AlreadyQuiescing
            }
            _ => {
                *state = StartupState::QuiescingSafe(None);
                StartupShutdownAction::NoRuntime
            }
        }
    }

    fn finish_rejected_build(&self, runtime: AppRuntimeHandle, safe_to_exit: bool) {
        let Ok(mut state) = self.state.lock() else {
            if !safe_to_exit {
                // Without the state lock there is nowhere to publish the
                // failed join. Keep the live native runtime out of Drop so a
                // poisoned bookkeeping lock cannot turn into unsafe teardown.
                std::mem::forget(runtime);
            }
            return;
        };
        let disposition = rejected_build_disposition(
            matches!(&*state, StartupState::QuiescingDuringBuild),
            safe_to_exit,
        );
        *state = match disposition {
            RejectedBuildDisposition::RestartRequired => StartupState::RestartRequired,
            RejectedBuildDisposition::ExitSafe => StartupState::QuiescingSafe(Some(runtime)),
            RejectedBuildDisposition::RetainAndAbortOnFinalExit => {
                StartupState::QuiescingBlocked(runtime)
            }
        };
    }

    fn finish_failed_build(&self) {
        if let Ok(mut state) = self.state.lock() {
            *state = match &*state {
                StartupState::QuiescingDuringBuild => StartupState::QuiescingSafe(None),
                StartupState::Building => StartupState::Failed,
                _ => return,
            };
        }
    }

    async fn wait_for_build(&self) -> bool {
        loop {
            let disposition = {
                let Ok(state) = self.state.lock() else {
                    return false;
                };
                match &*state {
                    StartupState::QuiescingDuringBuild => None,
                    StartupState::QuiescingSafe(_) => Some(true),
                    StartupState::QuiescingBlocked(_) => Some(false),
                    _ => Some(false),
                }
            };
            if let Some(safe_to_exit) = disposition {
                return safe_to_exit;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    fn final_exit_target(&self) -> FinalExitTarget {
        let Ok(mut state) = self.state.lock() else {
            return FinalExitTarget::WaitForBuild;
        };
        match &*state {
            StartupState::Ready(runtime) => {
                let runtime = runtime.clone();
                *state = StartupState::QuiescingSafe(Some(runtime.clone()));
                FinalExitTarget::Runtime(runtime)
            }
            StartupState::QuiescingSafe(Some(runtime))
            | StartupState::QuiescingBlocked(runtime) => FinalExitTarget::Runtime(runtime.clone()),
            StartupState::Building => {
                *state = StartupState::QuiescingDuringBuild;
                FinalExitTarget::WaitForBuild
            }
            StartupState::QuiescingDuringBuild => FinalExitTarget::WaitForBuild,
            StartupState::Idle
            | StartupState::Unlocking
            | StartupState::Failed
            | StartupState::RestartRequired => {
                *state = StartupState::QuiescingSafe(None);
                FinalExitTarget::NoRuntime
            }
            StartupState::QuiescingSafe(None) => FinalExitTarget::NoRuntime,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RejectedBuildDisposition {
    RestartRequired,
    ExitSafe,
    RetainAndAbortOnFinalExit,
}

const fn rejected_build_disposition(
    quit_waiting: bool,
    native_host_joined: bool,
) -> RejectedBuildDisposition {
    if !native_host_joined {
        RejectedBuildDisposition::RetainAndAbortOnFinalExit
    } else if quit_waiting {
        RejectedBuildDisposition::ExitSafe
    } else {
        RejectedBuildDisposition::RestartRequired
    }
}

#[tauri::command]
async fn mom_llama_runtime_initialize(
    app: AppHandle,
    startup: State<'_, StartupController>,
) -> Result<(), String> {
    initialize_runtime(app, &startup).await
}

async fn initialize_runtime(app: AppHandle, startup: &StartupController) -> Result<(), String> {
    let retry_cached_failure = match startup.begin_initialization()? {
        StartupAction::AlreadyReady => return Ok(()),
        StartupAction::Initialize {
            retry_cached_failure,
        } => retry_cached_failure,
    };

    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("Blessenger could not locate its setup data: {error}"))?;
    let first_run = !model_acquisition::installation_receipt_is_current(&app_data_dir);
    let _ = app.emit(
        "mom_llama_setup_state",
        model_acquisition::SetupState {
            first_run,
            phase: "starting",
        },
    );
    let acquisition_app = app.clone();
    let acquisition = tauri::async_runtime::spawn_blocking(move || {
        model_acquisition::acquire_required_models(first_run, |progress| {
            let _ = acquisition_app.emit("mom_llama_model_acquisition_progress", progress);
        })
    })
    .await
    .map_err(|error| format!("Blessenger setup worker failed: {error}"))?
    .map_err(|error| format!("Blessenger could not finish its first-time setup: {error}"));
    let acquired = match acquisition {
        Ok(acquired) => acquired,
        Err(error) => {
            startup.record_failure();
            return Err(error);
        }
    };

    let settings = match tauri::async_runtime::spawn_blocking(move || {
        if retry_cached_failure {
            mom_llama_runtime::prepare_secure_store_retry().map_err(|error| {
                anyhow::anyhow!("secure-store retry preparation failed: {error}")
            })?;
        }
        mom_llama_runtime::config::resolve_settings()
    })
    .await
    {
        Ok(settings) => settings,
        Err(error) => {
            startup.record_failure();
            return Err(format!("Blessenger startup worker failed: {error}"));
        }
    };

    let mut settings = match settings {
        Ok(settings) => settings,
        Err(error) => {
            startup.record_failure();
            return Err(format!(
                "Blessenger could not unlock its encrypted local data: {error:#}"
            ));
        }
    };
    if bind_acquired_model_pair(&mut settings, &acquired)
        && let Err(error) = mom_llama_runtime::config::save_settings(&settings)
    {
        startup.record_failure();
        return Err(format!(
            "Blessenger could not save its verified model set: {error:#}"
        ));
    }
    if !acquired.qwen_root.join("config.json").is_file() {
        startup.record_failure();
        return Err(
            "Blessenger's voice model did not resolve to its verified installation".to_string(),
        );
    }
    startup.begin_runtime_build()?;
    let runtime_data_dir = settings.data_dir.clone();
    let source_voice_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/speech-voices/qwen-icl");
    let installed_voice_root = resolve_installed_voice_root(
        app.path().resource_dir().ok().as_deref(),
        std::env::current_exe().ok().as_deref(),
        &source_voice_root,
    )?;
    let voice_app = app.clone();
    let voice_root = installed_voice_root.clone();
    tauri::async_runtime::spawn_blocking(move || {
        model_acquisition::validate_persona_voices(&voice_root, |progress| {
            let _ = voice_app.emit("mom_llama_voice_initialization_progress", progress);
        })
    })
    .await
    .map_err(|error| format!("Blessenger voice setup worker failed: {error}"))?
    .map_err(|error| format!("Blessenger could not validate its persona voices: {error}"))?;

    let shared_client = app
        .try_state::<EmbeddedMom>()
        .map(|embedded| embedded.client.clone());
    let embedded = shared_client.is_some();
    let prepared_speech = app
        .try_state::<PreparedSpeech>()
        .map(|value| value.inner().clone());
    if let Some(prepared) = &prepared_speech {
        prepared.refresh(&app_data_dir, &installed_voice_root);
    }
    let prepared_qwen = prepared_speech.as_ref().map(PreparedSpeech::backend);
    let built = match tauri::async_runtime::spawn_blocking(move || {
        build_runtime(settings, shared_client, installed_voice_root, prepared_qwen)
    })
    .await
    {
        Ok(built) => built,
        Err(error) => {
            startup.finish_failed_build();
            return Err(format!("Blessenger startup worker failed: {error}"));
        }
    };

    match built {
        Ok(runtime) => {
            if first_run {
                let scope = runtime.operation_scope();
                let warm = tauri::async_runtime::spawn_blocking(move || {
                    mom_llama_runtime::runtime_warm(&scope)
                })
                .await
                .map_err(|error| format!("Blessenger model warm worker failed: {error}"))?
                .map_err(|error| {
                    format!("Blessenger could not warm its listening model: {error:#}")
                })?;
                let ready = first_run_warm_ready(&warm);
                if !ready {
                    let reason = if let Some(blocker) = warm.blocker.as_ref() {
                        format!("{}: {}", blocker.code, blocker.message)
                    } else if let Some(result) = warm.result.as_ref() {
                        format!(
                            "model loaded: {}, audio projector warmed: {}",
                            result.model_loaded, result.audio_projector_warmed
                        )
                    } else {
                        format!("runtime warm returned {}", warm.status)
                    };
                    runtime.begin_quiesce();
                    let _ = if embedded {
                        runtime.drain_shared_services().await
                    } else {
                        runtime.shutdown().await
                    };
                    startup.record_failure();
                    return Err(format!(
                        "Blessenger could not make its listening model ready: {reason}"
                    ));
                }
            }
            if let Some(prepared) = &prepared_speech {
                prepared.claim();
                eprintln!(
                    "mom-llama prepared speech claimed after {} ms",
                    prepared.started.elapsed().as_millis()
                );
            }
            model_acquisition::write_installation_receipt(&app_data_dir)
                .map_err(|error| format!("Blessenger could not finish setup: {error}"))?;
            match startup.install(&app, runtime) {
                Ok(()) => {
                    let _ = app.emit(
                        "mom_llama_setup_state",
                        model_acquisition::SetupState {
                            first_run,
                            phase: "ready",
                        },
                    );
                    eprintln!("mom-llama runtime ready: {}", runtime_data_dir.display());
                    Ok(())
                }
                Err(runtime) => {
                    runtime.begin_quiesce();
                    let shutdown = if embedded {
                        runtime.drain_shared_services().await
                    } else {
                        runtime.shutdown().await
                    };
                    let safe_to_exit = if embedded {
                        shutdown.is_ok()
                    } else {
                        safe_to_exit_after_shutdown(&shutdown)
                    };
                    log_shutdown_result(&shutdown);
                    startup.finish_rejected_build(runtime, safe_to_exit);
                    Err("Blessenger stopped before encrypted runtime startup completed".to_string())
                }
            }
        }
        Err(error) => {
            let message = format!("Blessenger could not initialize its local runtime: {error:#}");
            startup.finish_failed_build();
            Err(message)
        }
    }
}

fn resolve_installed_voice_root(
    resource_dir: Option<&std::path::Path>,
    executable: Option<&std::path::Path>,
    source_voice_root: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let packaged_from_executable = executable
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .map(|contents| contents.join("Resources"));
    for candidate in resource_dir
        .map(|root| root.join("speech-voices/qwen-icl"))
        .into_iter()
        .chain(packaged_from_executable.map(|root| root.join("speech-voices/qwen-icl")))
        .chain(std::iter::once(source_voice_root.to_path_buf()))
    {
        if candidate.join("manifest.json").is_file() {
            return Ok(candidate);
        }
    }
    Err("Blessenger could not locate its installed voices".to_string())
}

fn bind_acquired_model_pair(
    settings: &mut mom_llama_runtime::config::Settings,
    acquired: &model_acquisition::AcquiredModels,
) -> bool {
    if settings.model_path.as_ref() != Some(&acquired.gemma) {
        return false;
    }
    let previous_projector = settings.mmproj_path.clone();
    let previous_projection = settings.upstream_settings.get("mmprojPath").cloned();
    let previous_mtp = settings.upstream_settings.get("mtpAssistantPath").cloned();
    settings.mmproj_path = Some(acquired.projector.clone());
    settings.upstream_settings.insert(
        "mmprojPath".to_string(),
        json!(acquired.projector.display().to_string()),
    );
    settings.upstream_settings.insert(
        "mtpAssistantPath".to_string(),
        json!(acquired.mtp_assistant.display().to_string()),
    );
    let changed = previous_projector != settings.mmproj_path
        || previous_projection != settings.upstream_settings.get("mmprojPath").cloned()
        || previous_mtp != settings.upstream_settings.get("mtpAssistantPath").cloned();
    if changed {
        settings.verified_model_artifacts = None;
    }
    changed
}

fn first_run_warm_ready(
    warm: &mom_llama_runtime::receipts::CommandResult<mom_llama_runtime::engine::RuntimeWarmOutput>,
) -> bool {
    warm.status == "host_integrated"
        && warm
            .result
            .as_ref()
            .is_some_and(|result| result.model_loaded && result.audio_projector_warmed)
}

fn command_handler() -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        mom_llama_runtime_initialize,
        commands::mom_llama_render_app,
        commands::mom_llama_render_chat_fragment,
        commands::mom_llama_render_sidebar_fragment,
        commands::mom_llama_render_settings_fragment,
        commands::mom_llama_voice_latency_record,
        commands::mom_llama_pick_file,
        commands::mom_llama_information_pick_alexandria,
        commands::mom_llama_information_register_alexandria,
        commands::mom_llama_information_libraries,
        commands::mom_llama_information_search,
        commands::mom_llama_information_open_citation,
        commands::mom_llama_information_grant_model_context,
        commands::mom_llama_information_chat_send,
        commands::mom_llama_attachment_library_preview,
        commands::mom_llama_attachment_library_commit,
        commands::mom_llama_information_managed_attachments,
        commands::mom_llama_information_managed_removals,
        commands::mom_llama_information_search_managed_attachment,
        commands::mom_llama_information_open_managed_citation,
        commands::mom_llama_information_managed_removal_preview,
        commands::mom_llama_information_managed_removal_commit,
        commands::mom_llama_engine_check,
        commands::mom_llama_runtime_warm,
        commands::mom_llama_engine_configure,
        commands::mom_llama_model_list,
        commands::mom_llama_model_select,
        commands::mom_llama_chat_send,
        commands::mom_llama_composer_autocomplete,
        commands::mom_llama_composer_autocomplete_cancel,
        commands::mom_llama_composer_autocomplete_accept,
        commands::mom_llama_chat_dispatch,
        commands::mom_llama_voice_audio_begin,
        commands::mom_llama_voice_capture_prepare,
        commands::mom_llama_voice_capture_start,
        commands::mom_llama_voice_capture_pause,
        commands::mom_llama_voice_capture_retarget,
        commands::mom_llama_voice_capture_stop,
        commands::mom_llama_voice_capture_cancel,
        commands::mom_llama_voice_audio_push,
        commands::mom_llama_voice_audio_finish,
        commands::mom_llama_voice_audio_speculate,
        commands::mom_llama_voice_audio_speculation_confirm,
        commands::mom_llama_voice_audio_speculation_abort,
        commands::mom_llama_voice_audio_speculation_abort_and_retarget,
        commands::mom_llama_voice_audio_abort,
        commands::mom_llama_mention_dispatch,
        commands::mom_llama_chat_cancel,
        commands::mom_llama_chat_skip_reasoning,
        commands::mom_llama_chat_regenerate,
        commands::mom_llama_chat_continue,
        commands::mom_llama_mention_candidates,
        commands::mom_llama_consult_sources,
        commands::mom_llama_consult_source_open,
        commands::mom_llama_mention_cancel,
        commands::mom_llama_mention_synthesize,
        commands::mom_llama_mention_tool_approval_list,
        commands::mom_llama_mention_tool_approval_decide,
        commands::mom_llama_persona_freeze,
        commands::mom_llama_persona_list,
        commands::mom_llama_persona_get,
        commands::mom_llama_persona_update,
        commands::mom_llama_persona_removal_preview,
        commands::mom_llama_persona_remove_from_library,
        commands::mom_llama_persona_instantiate,
        commands::mom_llama_persona_group_list,
        commands::mom_llama_persona_group_create,
        commands::mom_llama_persona_group_update,
        commands::mom_llama_persona_group_delete,
        commands::mom_llama_conversation_draft_open,
        commands::mom_llama_conversation_draft_recipients_update,
        commands::mom_llama_conversation_draft_submit,
        commands::mom_llama_conversation_draft_call_start,
        commands::mom_llama_conversation_new,
        commands::mom_llama_conversation_list,
        commands::mom_llama_conversation_select,
        commands::mom_llama_conversation_search,
        commands::mom_llama_conversation_rename,
        commands::mom_llama_conversation_system_message_update,
        commands::mom_llama_conversation_delete,
        commands::mom_llama_conversation_fork,
        commands::mom_llama_conversation_siblings,
        commands::mom_llama_draft_get,
        commands::mom_llama_draft_update,
        commands::mom_llama_draft_clear,
        commands::mom_llama_conversation_export,
        commands::mom_llama_conversation_import,
        commands::mom_llama_message_copy,
        commands::mom_llama_speech_read_aloud,
        commands::mom_llama_speech_precommit_synthesize,
        commands::mom_llama_voice_generation_backpressure,
        commands::mom_llama_message_edit,
        commands::mom_llama_message_record_voice_transcript,
        commands::mom_llama_message_delete,
        commands::mom_llama_message_branches,
        commands::mom_llama_message_branch_select,
        commands::mom_llama_attachment_import_text,
        commands::mom_llama_attachment_import_paste,
        commands::mom_llama_attachment_import,
        commands::mom_llama_voice_message_import,
        commands::mom_llama_attachment_list,
        commands::mom_llama_attachment_preview,
        commands::mom_llama_attachment_preview_content,
        commands::mom_llama_attachment_preview_bytes,
        commands::mom_llama_speech_transcribe_attachment,
        commands::mom_llama_speech_stop,
        commands::mom_llama_settings_get,
        commands::mom_llama_settings_reset,
        commands::mom_llama_settings_update,
        commands::mom_llama_skill_create,
        commands::mom_llama_skill_update,
        commands::mom_llama_skill_list,
        commands::mom_llama_skill_apply,
        commands::mom_llama_kv_cache_status,
        commands::mom_llama_kv_cache_save,
        commands::mom_llama_kv_cache_restore,
        commands::mom_llama_kv_cache_clear,
        commands::mom_llama_mcp_status,
        commands::mom_llama_mcp_configure,
        commands::mom_llama_mcp_list_servers,
        commands::mom_llama_mcp_list_tools,
        commands::mom_llama_mcp_call_tool,
        commands::mom_llama_mcp_list_resources,
        commands::mom_llama_mcp_read_resource,
        commands::mom_llama_mcp_list_prompts,
        commands::mom_llama_mcp_get_prompt,
        commands::mom_llama_tool_loop_prepare,
        commands::mom_llama_tool_loop_run,
        commands::mom_llama_tool_loop_cancel,
        commands::mom_llama_tool_loop_status,
        commands::mom_llama_tool_permission_list,
        commands::mom_llama_tool_permission_set,
        commands::mom_llama_tool_permission_revoke,
        commands::mom_llama_model_slot_list,
        commands::mom_llama_model_slot_load,
        commands::mom_llama_model_slot_unload,
    ]
}

/// Mom services embedded in an existing application/event loop. Native host
/// ownership stays with that application's coordinator.
#[derive(Clone)]
pub struct EmbeddedMom {
    client: llama_native_host::NativeClient,
    startup: StartupController,
}

impl EmbeddedMom {
    pub fn new(client: llama_native_host::NativeClient) -> Self {
        Self {
            client,
            startup: StartupController::new(),
        }
    }

    pub fn configure(&self, builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
        builder
            .manage(self.clone())
            .manage(self.startup.clone())
            .manage(native_voice_capture::NativeVoiceCapture::new())
            .invoke_handler(command_handler())
    }

    /// Execute an ordinary Mom turn through the same admission and worker
    /// supervisor as the Tauri command. The caller owns its document projection;
    /// this result retains the authoritative message and invocation identities.
    pub async fn dispatch<F>(
        &self,
        app: AppHandle,
        input: mom_llama_runtime::MentionDispatchInput,
        on_event: Option<F>,
    ) -> Result<mom_llama_runtime::CommandResult<mom_llama_runtime::ChatDispatchOutput>, String>
    where
        F: FnMut(mom_llama_runtime::ChatDispatchStreamEvent) -> anyhow::Result<()> + Send + 'static,
    {
        self.dispatch_cancellable(
            app,
            input,
            Vec::new(),
            Arc::new(AtomicBool::new(false)),
            on_event,
        )
        .await
    }

    /// Cancellation is local to this dispatch and is reswept until its owned
    /// worker reaches a terminal result. Cancelling never drops a live worker.
    pub async fn dispatch_cancellable<F>(
        &self,
        app: AppHandle,
        input: mom_llama_runtime::MentionDispatchInput,
        context: Vec<String>,
        cancelled: Arc<AtomicBool>,
        on_event: Option<F>,
    ) -> Result<mom_llama_runtime::CommandResult<mom_llama_runtime::ChatDispatchOutput>, String>
    where
        F: FnMut(mom_llama_runtime::ChatDispatchStreamEvent) -> anyhow::Result<()> + Send + 'static,
    {
        if cancelled.load(Ordering::Acquire) {
            return Err("Mom dispatch was cancelled before startup".into());
        }
        let registered = app
            .try_state::<Self>()
            .ok_or_else(|| "Mom service is not registered in this application".to_string())?;
        if !Arc::ptr_eq(&self.startup.state, &registered.startup.state) {
            return Err("Mom service belongs to another application".to_string());
        }
        initialize_runtime(app.clone(), &self.startup).await?;
        let runtime = app
            .try_state::<AppRuntimeHandle>()
            .ok_or_else(|| "Mom runtime was not installed after startup".to_string())?
            .inner()
            .clone();
        if cancelled.load(Ordering::Acquire) {
            return Err("Mom dispatch was cancelled during startup".into());
        }
        let operations = runtime
            .operation_scope()
            .child()
            .map_err(|error| error.to_string())?;
        let lease = runtime.admit(command_registry::command_spec("mom_llama_chat_dispatch"))?;
        let dispatch = commands::dispatch_typed_in_scope(
            runtime,
            lease,
            operations.clone(),
            input,
            Some(context),
            on_event,
        );
        let mut dispatch = std::pin::pin!(dispatch);
        let mut sweep = tokio::time::interval(std::time::Duration::from_millis(25));
        loop {
            tokio::select! {
                result = &mut dispatch => return result,
                _ = sweep.tick() => {
                    if cancelled.load(Ordering::Acquire) {
                        operations.request_cancellation();
                    }
                }
            }
        }
    }

    pub async fn drain(&self) -> Result<(), String> {
        match self.startup.final_exit_target() {
            FinalExitTarget::NoRuntime => Ok(()),
            FinalExitTarget::Runtime(runtime) => {
                let result = runtime.drain_shared_services().await;
                log_shutdown_result(&result);
                result.map(|_| ()).map_err(|error| error.to_string())
            }
            FinalExitTarget::WaitForBuild => {
                if self.startup.wait_for_build().await {
                    Ok(())
                } else {
                    Err("embedded Mom startup could not drain safely".into())
                }
            }
        }
    }
}

pub fn run() {
    if std::env::args().any(|arg| arg == "--dump-html") {
        match view::render_app(&mom_llama_runtime::OperationScope::detached()) {
            Ok(html) => println!("{html}"),
            Err(error) => {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        return;
    }

    if std::env::args().any(|arg| arg == "--smoke") {
        if let Err(error) = smoke() {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }

    // Constructing the product runtime opens the encrypted store and may ask
    // macOS Keychain for authorization. The webview invokes the initializer
    // only after this visible window and AppKit's event loop are live.
    let startup = StartupController::new();
    let event_startup = startup.clone();
    let exit_allowed = Arc::new(AtomicBool::new(false));
    let event_exit_allowed = Arc::clone(&exit_allowed);
    let app = tauri::Builder::default()
        // A second bundle path still has the same product identity. Stop it
        // before it can independently ask Keychain to unlock Mom's store.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        // Tauri's stock macOS Quit item calls AppKit `terminate:` directly and
        // can bypass `RunEvent::ExitRequested`. Mom owns a regular Cmd+Q menu
        // command so graceful quit always enters the composed shutdown path.
        .enable_macos_default_menu(false)
        .menu(build_desktop_menu)
        .manage(startup)
        .manage(native_voice_capture::NativeVoiceCapture::new())
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            app.manage(PreparedSpeech::new(&app_data_dir));
            Ok(())
        })
        .invoke_handler(command_handler())
        .build(tauri::generate_context!());
    match app {
        Ok(app) => app.run(move |app_handle, event| {
            if let tauri::RunEvent::MenuEvent(menu_event) = &event
                && menu_event.id() == APPLICATION_QUIT_MENU_ID
            {
                request_startup_aware_exit(app_handle, &event_startup, &event_exit_allowed, 0);
                return;
            }
            if let tauri::RunEvent::MenuEvent(menu_event) = &event
                && menu_event.id() == APPLICATION_SETTINGS_MENU_ID
            {
                if let Err(error) = app_handle.emit("mom_llama_settings_open", ()) {
                    eprintln!("Could not open Settings: {error}");
                }
                return;
            }
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if event_exit_allowed.load(Ordering::Acquire) {
                    return;
                }
                api.prevent_exit();
                request_startup_aware_exit(
                    app_handle,
                    &event_startup,
                    &event_exit_allowed,
                    code.unwrap_or(0),
                );
                return;
            }
            if let tauri::RunEvent::Exit = event
                && !event_exit_allowed.load(Ordering::Acquire)
            {
                // Dock Quit and AppleEvent Quit may reach this final boundary
                // without an interceptable ExitRequested. Do not let AppKit
                // or static teardown proceed while Metal owns live resources.
                let decision = decide_final_exit(app_handle, &event_startup);
                if decision == FinalExitDecision::AbortWithoutRustTeardown {
                    eprintln!(
                        "Blessenger aborts final exit because native shutdown lacks joined evidence"
                    );
                    std::process::abort();
                }
            }
        }),
        Err(error) => {
            eprintln!("failed to build Blessenger: {error}");
            std::process::exit(1);
        }
    }
}

fn build_desktop_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let package = app.package_info();
    let about = AboutMetadata {
        name: Some(package.name.clone()),
        version: Some(package.version.to_string()),
        copyright: app.config().bundle.copyright.clone(),
        authors: app
            .config()
            .bundle
            .publisher
            .clone()
            .map(|value| vec![value]),
        ..AboutMetadata::default()
    };
    let quit = MenuItem::with_id(
        app,
        APPLICATION_QUIT_MENU_ID,
        format!("Quit {}", package.name),
        true,
        Some(APPLICATION_QUIT_ACCELERATOR),
    )?;
    let window = Submenu::with_id_and_items(
        app,
        WINDOW_SUBMENU_ID,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            #[cfg(target_os = "macos")]
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    let help = Submenu::with_id_and_items(
        app,
        HELP_SUBMENU_ID,
        "Help",
        true,
        &[
            #[cfg(not(target_os = "macos"))]
            &PredefinedMenuItem::about(app, None, Some(about.clone()))?,
        ],
    )?;

    Menu::with_items(
        app,
        &[
            #[cfg(target_os = "macos")]
            &Submenu::with_items(
                app,
                package.name.clone(),
                true,
                &[
                    &PredefinedMenuItem::about(app, None, Some(about))?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(
                        app,
                        APPLICATION_SETTINGS_MENU_ID,
                        "Settings…",
                        true,
                        Some("CmdOrCtrl+,"),
                    )?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::services(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                ],
            )?,
            &Submenu::with_items(
                app,
                "File",
                true,
                &[
                    &PredefinedMenuItem::close_window(app, None)?,
                    #[cfg(not(target_os = "macos"))]
                    &quit,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?,
            #[cfg(target_os = "macos")]
            &Submenu::with_items(
                app,
                "View",
                true,
                &[&PredefinedMenuItem::fullscreen(app, None)?],
            )?,
            &window,
            &help,
        ],
    )
}

fn request_graceful_exit<R: Runtime>(
    app_handle: &AppHandle<R>,
    runtime: &AppRuntimeHandle,
    exit_allowed: &Arc<AtomicBool>,
    exit_code: i32,
) {
    let _ = app_handle.emit("mom_llama_speech_quiescing", ());
    if exit_allowed.load(Ordering::Acquire) || !runtime.begin_quiesce() {
        return;
    }
    let runtime = runtime.clone();
    let app_handle = app_handle.clone();
    let exit_allowed = Arc::clone(exit_allowed);
    tauri::async_runtime::spawn(async move {
        let result = runtime.shutdown().await;
        log_shutdown_result(&result);
        let safe_to_exit = safe_to_exit_after_shutdown(&result);
        if safe_to_exit {
            exit_allowed.store(true, Ordering::Release);
            app_handle.exit(exit_code);
        } else if let Err(error) = result {
            eprintln!("Blessenger remains open because native shutdown failed: {error}");
        }
    });
}

fn safe_to_exit_after_shutdown(
    result: &Result<app_runtime::AppShutdownSummary, app_runtime::AppShutdownError>,
) -> bool {
    match result {
        Ok(_) => true,
        Err(error) => {
            error.summary.native_host_joined
                && error.summary.speech_host_joined
                && error.summary.persona_approval_recovery_complete
                && error.approval_recovery_error.is_none()
                && error.speech_error.is_none()
        }
    }
}

fn decide_final_exit<R: Runtime>(
    app_handle: &AppHandle<R>,
    startup: &StartupController,
) -> FinalExitDecision {
    let target = startup.final_exit_target();
    if matches!(target, FinalExitTarget::NoRuntime) {
        let safe = app_handle
            .try_state::<PreparedSpeech>()
            .is_none_or(|prepared| {
                tauri::async_runtime::block_on(prepared.shutdown_if_unclaimed())
            });
        return final_exit_decision(safe);
    }
    decide_final_exit_target(
        target,
        |runtime| {
            runtime.begin_quiesce();
            let result = tauri::async_runtime::block_on(runtime.shutdown());
            log_shutdown_result(&result);
            safe_to_exit_after_shutdown(&result)
        },
        || tauri::async_runtime::block_on(startup.wait_for_build()),
    )
}

fn decide_final_exit_target(
    target: FinalExitTarget,
    shutdown_runtime: impl FnOnce(AppRuntimeHandle) -> bool,
    wait_for_build: impl FnOnce() -> bool,
) -> FinalExitDecision {
    let safe_to_return = match target {
        FinalExitTarget::NoRuntime => true,
        FinalExitTarget::Runtime(runtime) => shutdown_runtime(runtime),
        FinalExitTarget::WaitForBuild => wait_for_build(),
    };
    final_exit_decision(safe_to_return)
}

const fn final_exit_decision(safe_to_return: bool) -> FinalExitDecision {
    if safe_to_return {
        FinalExitDecision::ReturnToTauri
    } else {
        FinalExitDecision::AbortWithoutRustTeardown
    }
}

fn request_startup_aware_exit<R: Runtime>(
    app_handle: &AppHandle<R>,
    startup: &StartupController,
    exit_allowed: &Arc<AtomicBool>,
    exit_code: i32,
) {
    match startup.begin_shutdown() {
        StartupShutdownAction::Start(runtime) => {
            request_graceful_exit(app_handle, &runtime, exit_allowed, exit_code);
        }
        StartupShutdownAction::AlreadyQuiescing => {}
        StartupShutdownAction::WaitForBuild => {
            let startup = startup.clone();
            let app_handle = app_handle.clone();
            let exit_allowed = Arc::clone(exit_allowed);
            tauri::async_runtime::spawn(async move {
                if startup.wait_for_build().await {
                    let speech_safe = match app_handle.try_state::<PreparedSpeech>() {
                        Some(prepared) => prepared.shutdown_if_unclaimed().await,
                        None => true,
                    };
                    if speech_safe {
                        exit_allowed.store(true, Ordering::Release);
                        app_handle.exit(exit_code);
                    } else {
                        eprintln!(
                            "Blessenger remains open because prepared speech shutdown failed"
                        );
                    }
                } else {
                    eprintln!("Blessenger remains open because startup native shutdown failed");
                }
            });
        }
        StartupShutdownAction::NoRuntime => {
            // A late Keychain result cannot advance to native construction
            // after this transition, but launch-time speech warming already
            // owns a resident helper and must be joined before AppKit exits.
            let app_handle = app_handle.clone();
            let exit_allowed = Arc::clone(exit_allowed);
            tauri::async_runtime::spawn(async move {
                let speech_safe = match app_handle.try_state::<PreparedSpeech>() {
                    Some(prepared) => prepared.shutdown_if_unclaimed().await,
                    None => true,
                };
                if speech_safe {
                    exit_allowed.store(true, Ordering::Release);
                    app_handle.exit(exit_code);
                } else {
                    eprintln!("Blessenger remains open because prepared speech shutdown failed");
                }
            });
        }
    }
}

fn log_shutdown_result(
    result: &Result<app_runtime::AppShutdownSummary, app_runtime::AppShutdownError>,
) {
    match serde_json::to_string(result) {
        Ok(receipt) => eprintln!("mom-llama shutdown: {receipt}"),
        Err(error) => eprintln!("Blessenger could not encode its shutdown receipt: {error}"),
    }
}

fn build_runtime(
    settings: mom_llama_runtime::config::Settings,
    shared_client: Option<llama_native_host::NativeClient>,
    installed_voice_root: std::path::PathBuf,
    prepared_qwen: Option<Arc<speech_native_backend_qwen::QwenSpeechBackend>>,
) -> std::result::Result<AppRuntimeHandle, RuntimeBuildError> {
    let speech_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| RuntimeBuildError {
            message: format!("speech runtime construction failed: {error}"),
        })?;
    let speech = speech_runtime
        .block_on(async {
            if let Some(prepared_qwen) = prepared_qwen {
                crate::speech::MomSpeech::discover_with_prepared_qwen(
                    &settings.data_dir,
                    &installed_voice_root,
                    Some(prepared_qwen),
                )
                .await
            } else {
                crate::speech::MomSpeech::discover(&settings.data_dir, &installed_voice_root).await
            }
        })
        .map_err(|error| RuntimeBuildError {
            message: format!("speech host initialization failed: {error}"),
        })?;
    let persona_approval_recovery =
        mom_llama_runtime::PersonaToolApprovalRecovery::bind(&settings.data_dir)
            .and_then(|recovery| {
                recovery.reconcile()?;
                Ok(recovery)
            })
            .map_err(|error| RuntimeBuildError {
                message: format!("persona approval recovery initialization failed: {error:#}"),
            })?;
    let information = Arc::new(
        crate::information::MomInformation::open(&settings.data_dir).map_err(|error| {
            RuntimeBuildError {
                message: format!("Information host initialization failed: {error}"),
            }
        })?,
    );
    if let Some(client) = shared_client {
        let scope =
            mom_llama_runtime::native_runtime::shared_operation_scope(&settings, client.clone())
                .map_err(|error| RuntimeBuildError {
                    message: format!("shared native scope initialization failed: {error:#}"),
                })?;
        Ok(AppRuntimeHandle::new_shared(
            client,
            scope,
            persona_approval_recovery,
            speech,
            information,
        ))
    } else {
        let native_owner = mom_llama_runtime::native_runtime::ProductRuntimeOwner::initialize(
            &settings,
        )
        .map_err(|error| RuntimeBuildError {
            message: format!("native runtime owner initialization failed: {error:#}"),
        })?;
        Ok(AppRuntimeHandle::new(
            native_owner,
            persona_approval_recovery,
            speech,
            information,
        ))
    }
}

fn smoke() -> Result<()> {
    let app_html = view::render_app(&mom_llama_runtime::OperationScope::detached())?;
    let smoke = smoke_receipt(app_html.len());
    println!("{}", serde_json::to_string_pretty(&smoke)?);
    Ok(())
}

fn smoke_receipt(rendered_html_bytes: usize) -> serde_json::Value {
    let distinct_contract_command_count = view::CONTROL_SPECS
        .iter()
        .map(|control| control.command)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    json!({
        "schema": "mom_llama.tauri_app_smoke.v2",
        "status": "passed",
        "scope": "render-and-control-registry",
        "runtime": "tauri-maud-htmx",
        "rendered_html_bytes": rendered_html_bytes,
        "registered_affordance_count": view::CONTROL_SPECS.len(),
        "distinct_contract_command_count": distinct_contract_command_count,
        "commands": view::CONTROL_SPECS.iter().map(|control| {
            json!({
                "affordance": control.affordance,
                "command": control.command,
                "tauri_command": control.tauri_command,
                "cli": control.cli,
                "effect": control.effect,
            })
        }).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        FinalExitDecision, FinalExitTarget, StartupAction, StartupController,
        StartupShutdownAction, bind_acquired_model_pair, decide_final_exit_target,
        final_exit_decision, first_run_warm_ready, model_acquisition, rejected_build_disposition,
        resolve_installed_voice_root, safe_to_exit_after_shutdown, smoke_receipt,
    };
    use crate::app_runtime::{AppShutdownError, AppShutdownSummary};
    use serde_json::json;

    #[test]
    fn bundled_voices_resolve_from_executable_when_tauri_resource_path_is_unavailable() {
        let bundle = tempfile::tempdir().expect("bundle");
        let contents = bundle.path().join("Blessenger.app/Contents");
        let voice_root = contents.join("Resources/speech-voices/qwen-icl");
        std::fs::create_dir_all(&voice_root).expect("voice directory");
        std::fs::write(voice_root.join("manifest.json"), "{}").expect("manifest");
        let executable = contents.join("MacOS/mom-llama-app");
        assert_eq!(
            resolve_installed_voice_root(None, Some(&executable), &bundle.path().join("absent"))
                .expect("bundled voices"),
            voice_root
        );
    }

    fn startup_controller() -> StartupController {
        StartupController::new()
    }

    #[test]
    fn startup_controller_serializes_initialization_retry_and_shutdown() {
        let startup = startup_controller();
        assert_eq!(
            startup.begin_initialization(),
            Ok(StartupAction::Initialize {
                retry_cached_failure: false
            })
        );
        assert!(startup.begin_initialization().is_err());

        startup.record_failure();
        assert_eq!(
            startup.begin_initialization(),
            Ok(StartupAction::Initialize {
                retry_cached_failure: true
            })
        );
        assert!(matches!(
            startup.begin_shutdown(),
            StartupShutdownAction::NoRuntime
        ));
        assert!(matches!(
            startup.begin_shutdown(),
            StartupShutdownAction::AlreadyQuiescing
        ));
        assert!(startup.begin_initialization().is_err());
    }

    #[tokio::test]
    async fn shutdown_waits_when_native_runtime_construction_has_started() {
        let startup = startup_controller();
        assert!(startup.begin_initialization().is_ok());
        assert!(startup.begin_runtime_build().is_ok());
        assert!(matches!(
            startup.begin_shutdown(),
            StartupShutdownAction::WaitForBuild
        ));
        assert!(matches!(
            startup.begin_shutdown(),
            StartupShutdownAction::AlreadyQuiescing
        ));
        startup.finish_failed_build();
        assert!(startup.wait_for_build().await);
        assert!(matches!(
            startup.begin_shutdown(),
            StartupShutdownAction::AlreadyQuiescing
        ));
    }

    #[test]
    fn failed_runtime_build_is_retryable_before_owner_publication() {
        let startup = startup_controller();
        assert!(startup.begin_initialization().is_ok());
        assert!(startup.begin_runtime_build().is_ok());
        startup.finish_failed_build();
        assert_eq!(
            startup.begin_initialization(),
            Ok(StartupAction::Initialize {
                retry_cached_failure: true
            })
        );

        assert_eq!(
            rejected_build_disposition(false, true),
            super::RejectedBuildDisposition::RestartRequired
        );
        assert_eq!(
            rejected_build_disposition(true, true),
            super::RejectedBuildDisposition::ExitSafe
        );
        assert_eq!(
            rejected_build_disposition(false, false),
            super::RejectedBuildDisposition::RetainAndAbortOnFinalExit
        );
        assert_eq!(
            rejected_build_disposition(true, false),
            super::RejectedBuildDisposition::RetainAndAbortOnFinalExit
        );
    }

    #[test]
    fn failed_native_join_never_permits_process_exit() {
        let summary = AppShutdownSummary {
            started_at_unix_ms: 1,
            completed_at_unix_ms: 2,
            elapsed_ms: 1,
            native_host_joined: false,
            speech_host_joined: true,
            speech_active_operation_count: 0,
            speech_retained_playback_count: 0,
            operation_supervisor_phase: crate::operation_supervisor::LifecyclePhase::Closed,
            active_operation_count: 0,
            retained_operation_task_count: 0,
            active_product_operation_count: 0,
            expected_operation_worker_count: 0,
            joined_operation_worker_count: 0,
            expected_native_worker_count: 0,
            joined_native_worker_count: 0,
            expected_worker_ids: Vec::new(),
            joined_worker_ids: Vec::new(),
            application_work_drained: true,
            persona_approval_recovery_complete: true,
        };
        let failure = Err(AppShutdownError {
            summary: summary.clone(),
            operation_error: None,
            approval_recovery_error: None,
            native_error: Some("native join failed".to_string()),
            speech_error: None,
        });
        assert!(!safe_to_exit_after_shutdown(&failure));

        let recovery_failure = Err(AppShutdownError {
            summary: AppShutdownSummary {
                native_host_joined: true,
                persona_approval_recovery_complete: false,
                ..summary.clone()
            },
            operation_error: None,
            approval_recovery_error: Some("final approval sweep failed".to_string()),
            native_error: None,
            speech_error: None,
        });
        assert!(!safe_to_exit_after_shutdown(&recovery_failure));
        let speech_failure = Err(AppShutdownError {
            summary: AppShutdownSummary {
                native_host_joined: true,
                speech_host_joined: false,
                persona_approval_recovery_complete: true,
                ..summary
            },
            operation_error: None,
            approval_recovery_error: None,
            native_error: None,
            speech_error: Some("non-preemptive Apple call did not join".to_string()),
        });
        assert!(!safe_to_exit_after_shutdown(&speech_failure));
        assert_eq!(
            final_exit_decision(false),
            FinalExitDecision::AbortWithoutRustTeardown
        );
        assert_eq!(final_exit_decision(true), FinalExitDecision::ReturnToTauri);
        assert_eq!(
            decide_final_exit_target(
                FinalExitTarget::WaitForBuild,
                |_| panic!("build wait target has no ready runtime"),
                || false
            ),
            FinalExitDecision::AbortWithoutRustTeardown
        );
    }

    #[test]
    fn encrypted_runtime_initialization_is_renderer_driven_and_blocking_pool_bound() {
        let main = include_str!("main.rs");
        let index = include_str!("../../ui/index.html");
        let script = include_str!("../../ui/coop-hx.js");

        assert!(main.contains("async fn mom_llama_runtime_initialize"));
        assert!(main.contains("tauri::async_runtime::spawn_blocking"));
        assert!(main.contains("mom_llama_runtime::prepare_secure_store_retry"));
        assert!(index.contains("startup-status"));
        assert!(index.contains("startup-retry"));
        assert!(index.contains("Hi Jan — I’m getting Blessenger ready."));
        assert!(index.contains("startup-progress"));
        assert!(script.contains("await invoke(\"mom_llama_runtime_initialize\")"));
        assert!(script.contains("mom_llama_setup_state"));
        assert!(script.contains("mom_llama_model_acquisition_progress"));
        assert!(script.contains("mom_llama_voice_initialization_progress"));
        assert!(script.contains("Checking ${payload.voice}"));
    }

    #[test]
    fn acquired_default_rebinds_stale_companions_without_changing_custom_model() {
        use std::path::PathBuf;

        let acquired = model_acquisition::AcquiredModels {
            gemma: PathBuf::from("/cache/gemma.gguf"),
            projector: PathBuf::from("/cache/projector.gguf"),
            mtp_assistant: PathBuf::from("/cache/mtp.gguf"),
            qwen_root: PathBuf::from("/cache/qwen"),
        };
        let mut settings =
            mom_llama_runtime::config::Settings::defaults_for_data_dir(PathBuf::from("/data"));
        settings.model_path = Some(acquired.gemma.clone());
        settings.mmproj_path = Some(PathBuf::from("/old/projector.gguf"));
        settings
            .upstream_settings
            .insert("mtpAssistantPath".to_string(), json!("/old/mtp.gguf"));
        assert!(bind_acquired_model_pair(&mut settings, &acquired));
        assert_eq!(settings.mmproj_path.as_ref(), Some(&acquired.projector));
        assert_eq!(
            settings.upstream_settings["mmprojPath"],
            json!("/cache/projector.gguf")
        );
        assert_eq!(
            settings.upstream_settings["mtpAssistantPath"],
            json!("/cache/mtp.gguf")
        );
        assert!(!bind_acquired_model_pair(&mut settings, &acquired));

        settings.model_path = Some(PathBuf::from("/chosen/custom.gguf"));
        settings.mmproj_path = Some(PathBuf::from("/chosen/projector.gguf"));
        let custom = settings.clone();
        assert!(!bind_acquired_model_pair(&mut settings, &acquired));
        assert_eq!(settings, custom);
    }

    #[test]
    fn first_run_accepts_the_runtime_warm_receipt_status() {
        use mom_llama_runtime::engine::RuntimeWarmOutput;
        use mom_llama_runtime::receipts::CommandResult;

        let output = RuntimeWarmOutput {
            model_loaded: true,
            audio_projector_warmed: true,
            audio_projector_warm_us: 1,
            persona_prefixes_ready: 0,
            persona_prefixes_reused: 0,
            settings_us: 1,
            model_acquire_us: 1,
            model_load: None,
        };
        let warm = CommandResult::passed(
            "mom_llama.runtime_warm",
            "host_integrated",
            output.clone(),
            vec![],
            vec![],
            true,
            false,
        );
        assert!(first_run_warm_ready(&warm));
        let missing_audio = CommandResult::passed(
            "mom_llama.runtime_warm",
            "host_integrated",
            RuntimeWarmOutput {
                audio_projector_warmed: false,
                ..output
            },
            vec![],
            vec![],
            true,
            false,
        );
        assert!(!first_run_warm_ready(&missing_audio));
    }

    #[test]
    fn a_second_bundle_path_cannot_open_the_secure_store_again() {
        let main = include_str!("main.rs");
        let plugin = main
            .find(".plugin(tauri_plugin_single_instance::init")
            .expect("single-instance plugin");
        let builder = main
            .find(".build(tauri::generate_context!())")
            .expect("Tauri build");

        assert!(plugin < builder);
        assert!(main.contains("app.get_webview_window(\"main\")"));
        assert!(main.contains("window.set_focus()"));
    }

    #[test]
    fn macos_quit_source_cannot_reintroduce_appkit_terminate_bypass() {
        let source = include_str!("main.rs");
        let predefined_quit = concat!("PredefinedMenuItem::", "quit");
        let predefined_quit_with_text = concat!("PredefinedMenuItem::", "quit_with_text");
        let default_menu = concat!("Menu::", "default");

        assert!(source.contains("enable_macos_default_menu(false)"));
        assert!(source.contains("APPLICATION_QUIT_MENU_ID"));
        assert!(source.contains("RunEvent::Exit"));
        assert!(!source.contains(predefined_quit));
        assert!(!source.contains(predefined_quit_with_text));
        assert!(!source.contains(default_menu));
    }

    #[test]
    fn smoke_receipt_reports_only_derived_registry_evidence() {
        let receipt = smoke_receipt(123);
        assert_eq!(receipt["schema"], "mom_llama.tauri_app_smoke.v2");
        assert_eq!(receipt["scope"], "render-and-control-registry");
        assert_eq!(receipt["rendered_html_bytes"], 123);
        assert!(receipt["registered_affordance_count"].as_u64().is_some());
        assert!(
            receipt["distinct_contract_command_count"]
                .as_u64()
                .is_some()
        );
        assert!(receipt.get("visible_command_count").is_none());
        assert!(receipt.get("forbidden_frontend_frameworks").is_none());
        assert!(receipt.get("localhost_core_route_shim").is_none());
    }
}
