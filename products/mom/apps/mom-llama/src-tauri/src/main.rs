mod app_runtime;
mod command_registry;
mod commands;
mod operation_supervisor;
mod view;
use anyhow::Result;
use app_runtime::AppRuntimeHandle;
use fte_backend_llama::LlamaNativeBackend;
use fte_router::{Gateway, GatewayDefaults};
use fte_store::ResponseStore;
use fte_types::{GatewayError, GatewayResponse, RequestId};
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{any::Any, panic::AssertUnwindSafe};
use tauri::menu::{
    AboutMetadata, HELP_SUBMENU_ID, Menu, MenuItem, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID,
};
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Runtime, State};

const APPLICATION_QUIT_MENU_ID: &str = "mom-llama.application.quit";
const APPLICATION_QUIT_ACCELERATOR: &str = "CmdOrCtrl+Q";

#[cfg(test)]
pub(crate) static APP_DATA_DIR_TEST_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone)]
struct StartupController {
    gateway: Arc<Gateway>,
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
    QuiescingBuildCleanupBlocked(mom_llama_runtime::native_runtime::ProductRuntimeOwner),
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
    AbortWithoutRustTeardown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalExitDecision {
    ReturnToTauri,
    AbortWithoutRustTeardown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedBuildDisposition {
    Retryable,
    RestartRequired,
    ExitSafe,
    RetainAndAbortOnFinalExit,
}

enum FailedBuildCompletion {
    PreOwner,
    PostOwnerCleaned,
    PostOwnerCleanupBlocked(mom_llama_runtime::native_runtime::ProductRuntimeOwner),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailedBuildKind {
    PreOwner,
    PostOwnerCleaned,
    PostOwnerCleanupBlocked,
}

impl FailedBuildCompletion {
    const fn kind(&self) -> FailedBuildKind {
        match self {
            Self::PreOwner => FailedBuildKind::PreOwner,
            Self::PostOwnerCleaned => FailedBuildKind::PostOwnerCleaned,
            Self::PostOwnerCleanupBlocked(_) => FailedBuildKind::PostOwnerCleanupBlocked,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeBuildCleanupEvidence {
    native_host_joined: bool,
    joined_native_worker_count: usize,
    error: Option<String>,
}

struct NativeBuildCleanup {
    evidence: NativeBuildCleanupEvidence,
    blocked_owner: Option<mom_llama_runtime::native_runtime::ProductRuntimeOwner>,
}

#[derive(Debug)]
enum PostOwnerBuildCause<E> {
    Error(E),
    Panic(String),
}

struct PostOwnerBuildFailure<E> {
    cause: PostOwnerBuildCause<E>,
    cleanup: NativeBuildCleanup,
}

struct RuntimeBuildError {
    message: String,
    cleanup: Option<NativeBuildCleanupEvidence>,
    completion: FailedBuildCompletion,
}

impl std::fmt::Display for RuntimeBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)?;
        if let Some(cleanup) = &self.cleanup {
            write!(
                formatter,
                "; native cleanup: joined={}, workers={}",
                cleanup.native_host_joined, cleanup.joined_native_worker_count
            )?;
            if let Some(error) = &cleanup.error {
                write!(formatter, ", error={error}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for RuntimeBuildError {}

impl std::fmt::Debug for RuntimeBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeBuildError")
            .field("message", &self.message)
            .field("cleanup", &self.cleanup)
            .field("completion", &self.completion.kind())
            .finish()
    }
}

impl StartupController {
    fn new(gateway: Arc<Gateway>) -> Self {
        Self {
            gateway,
            state: Arc::new(Mutex::new(StartupState::Idle)),
        }
    }

    fn begin_initialization(&self) -> Result<StartupAction, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Mom Llama startup state is unavailable".to_string())?;
        match &*state {
            StartupState::Ready(_) => Ok(StartupAction::AlreadyReady),
            StartupState::Unlocking | StartupState::Building => {
                Err("Mom Llama is already unlocking its encrypted local data".to_string())
            }
            StartupState::QuiescingSafe(_)
            | StartupState::QuiescingDuringBuild
            | StartupState::QuiescingBlocked(_)
            | StartupState::QuiescingBuildCleanupBlocked(_) => {
                Err("Mom Llama is shutting down".to_string())
            }
            StartupState::RestartRequired => Err(
                "Mom Llama must be restarted before local runtime startup can retry".to_string(),
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
            .map_err(|_| "Mom Llama startup state is unavailable".to_string())?;
        match &*state {
            StartupState::Unlocking => {
                *state = StartupState::Building;
                Ok(())
            }
            StartupState::QuiescingSafe(_) => Err("Mom Llama is shutting down".to_string()),
            _ => Err("Mom Llama startup changed phase unexpectedly".to_string()),
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
            StartupState::QuiescingSafe(_)
            | StartupState::QuiescingBlocked(_)
            | StartupState::QuiescingBuildCleanupBlocked(_) => {
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

    fn finish_failed_build(&self, completion: FailedBuildCompletion) {
        let Ok(mut state) = self.state.lock() else {
            if let FailedBuildCompletion::PostOwnerCleanupBlocked(owner) = completion {
                // A poisoned state lock cannot safely publish join evidence.
                // Intentionally retain the sole native owner for process life;
                // dropping it would silently repeat the failed finalizer.
                std::mem::forget(owner);
            }
            return;
        };
        let disposition = failed_build_disposition(
            matches!(&*state, StartupState::QuiescingDuringBuild),
            completion.kind(),
        );
        *state = match (disposition, completion) {
            (FailedBuildDisposition::Retryable, FailedBuildCompletion::PreOwner) => {
                StartupState::Failed
            }
            (FailedBuildDisposition::RestartRequired, FailedBuildCompletion::PostOwnerCleaned) => {
                StartupState::RestartRequired
            }
            (FailedBuildDisposition::ExitSafe, _) => StartupState::QuiescingSafe(None),
            (
                FailedBuildDisposition::RetainAndAbortOnFinalExit,
                FailedBuildCompletion::PostOwnerCleanupBlocked(owner),
            ) => StartupState::QuiescingBuildCleanupBlocked(owner),
            _ => unreachable!("failed-build disposition and completion must agree"),
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
                    StartupState::QuiescingBlocked(_)
                    | StartupState::QuiescingBuildCleanupBlocked(_) => Some(false),
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
            StartupState::QuiescingBuildCleanupBlocked(owner) => {
                let _keep_owner_alive = owner;
                FinalExitTarget::AbortWithoutRustTeardown
            }
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

const fn failed_build_disposition(
    quit_waiting: bool,
    completion: FailedBuildKind,
) -> FailedBuildDisposition {
    match (quit_waiting, completion) {
        (_, FailedBuildKind::PostOwnerCleanupBlocked) => {
            FailedBuildDisposition::RetainAndAbortOnFinalExit
        }
        (true, _) => FailedBuildDisposition::ExitSafe,
        (false, FailedBuildKind::PreOwner) => FailedBuildDisposition::Retryable,
        (false, FailedBuildKind::PostOwnerCleaned) => FailedBuildDisposition::RestartRequired,
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
    let retry_cached_failure = match startup.begin_initialization()? {
        StartupAction::AlreadyReady => return Ok(()),
        StartupAction::Initialize {
            retry_cached_failure,
        } => retry_cached_failure,
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
            return Err(format!("Mom Llama startup worker failed: {error}"));
        }
    };

    let settings = match settings {
        Ok(settings) => settings,
        Err(error) => {
            startup.record_failure();
            return Err(format!(
                "Mom Llama could not unlock its encrypted local data: {error:#}"
            ));
        }
    };
    startup.begin_runtime_build()?;
    let runtime_data_dir = settings.data_dir.clone();

    let gateway = Arc::clone(&startup.gateway);
    let built = match tauri::async_runtime::spawn_blocking(move || build_runtime(gateway, settings))
        .await
    {
        Ok(built) => built,
        Err(error) => {
            startup.finish_failed_build(FailedBuildCompletion::PreOwner);
            return Err(format!("Mom Llama startup worker failed: {error}"));
        }
    };

    match built {
        Ok(runtime) => match startup.install(&app, runtime) {
            Ok(()) => {
                eprintln!("mom-llama runtime ready: {}", runtime_data_dir.display());
                Ok(())
            }
            Err(runtime) => {
                runtime.begin_quiesce();
                let shutdown = runtime.shutdown().await;
                let safe_to_exit = safe_to_exit_after_shutdown(&shutdown);
                log_shutdown_result(&shutdown);
                startup.finish_rejected_build(runtime, safe_to_exit);
                Err("Mom Llama stopped before encrypted runtime startup completed".to_string())
            }
        },
        Err(error) => {
            let message = format!("Mom Llama could not initialize its local runtime: {error:#}");
            startup.finish_failed_build(error.completion);
            Err(message)
        }
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--dump-html") {
        match view::render_app() {
            Ok(html) => println!("{html}"),
            Err(error) => {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        mom_llama_runtime::unload_resident_model();
        return;
    }

    if std::env::args().any(|arg| arg == "--smoke") {
        if let Err(error) = smoke() {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        mom_llama_runtime::unload_resident_model();
        return;
    }

    // Constructing the product runtime opens the encrypted store and may ask
    // macOS Keychain for authorization. The webview invokes the initializer
    // only after this visible window and AppKit's event loop are live.
    let gateway = Arc::new(Gateway::new(GatewayDefaults {
        catalog_version: "mom-llama-local-v1".to_string(),
    }));
    let startup = StartupController::new(Arc::clone(&gateway));
    let event_startup = startup.clone();
    let gateway_plugin = build_gateway_plugin(gateway);
    let exit_allowed = Arc::new(AtomicBool::new(false));
    let event_exit_allowed = Arc::clone(&exit_allowed);
    let app = tauri::Builder::default()
        // Tauri's stock macOS Quit item calls AppKit `terminate:` directly and
        // can bypass `RunEvent::ExitRequested`. Mom owns a regular Cmd+Q menu
        // command so graceful quit always enters the composed shutdown path.
        .enable_macos_default_menu(false)
        .menu(build_desktop_menu)
        .manage(startup)
        .plugin(gateway_plugin)
        .invoke_handler(tauri::generate_handler![
            mom_llama_runtime_initialize,
            commands::mom_llama_render_app,
            commands::mom_llama_render_chat_fragment,
            commands::mom_llama_render_sidebar_fragment,
            commands::mom_llama_render_persona_picker_fragment,
            commands::mom_llama_render_settings_fragment,
            commands::mom_llama_pick_file,
            commands::mom_llama_engine_check,
            commands::mom_llama_engine_configure,
            commands::mom_llama_model_list,
            commands::mom_llama_model_select,
            commands::mom_llama_chat_send,
            commands::mom_llama_chat_dispatch,
            commands::mom_llama_mention_dispatch,
            commands::mom_llama_chat_cancel,
            commands::mom_llama_chat_skip_reasoning,
            commands::mom_llama_chat_regenerate,
            commands::mom_llama_chat_continue,
            commands::mom_llama_mention_candidates,
            commands::mom_llama_mention_cancel,
            commands::mom_llama_mention_synthesize,
            commands::mom_llama_persona_freeze,
            commands::mom_llama_persona_list,
            commands::mom_llama_persona_get,
            commands::mom_llama_persona_update,
            commands::mom_llama_persona_delete,
            commands::mom_llama_persona_instantiate,
            commands::mom_llama_persona_group_list,
            commands::mom_llama_persona_group_create,
            commands::mom_llama_persona_group_update,
            commands::mom_llama_persona_group_delete,
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
            commands::mom_llama_message_edit,
            commands::mom_llama_message_delete,
            commands::mom_llama_message_branches,
            commands::mom_llama_message_branch_select,
            commands::mom_llama_attachment_import_text,
            commands::mom_llama_attachment_import_paste,
            commands::mom_llama_attachment_import,
            commands::mom_llama_attachment_list,
            commands::mom_llama_attachment_preview,
            commands::mom_llama_attachment_preview_bytes,
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
        ])
        .build(tauri::generate_context!());
    match app {
        Ok(app) => app.run(move |app_handle, event| {
            if let tauri::RunEvent::MenuEvent(menu_event) = &event
                && menu_event.id() == APPLICATION_QUIT_MENU_ID
            {
                request_startup_aware_exit(app_handle, &event_startup, &event_exit_allowed, 0);
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
                let decision = decide_final_exit(&event_startup);
                if decision == FinalExitDecision::AbortWithoutRustTeardown {
                    eprintln!(
                        "Mom Llama aborts final exit because native shutdown lacks joined evidence"
                    );
                    std::process::abort();
                }
            }
        }),
        Err(error) => {
            eprintln!("failed to build Mom Llama: {error}");
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
            eprintln!("Mom Llama remains open because native shutdown failed: {error}");
        }
    });
}

fn safe_to_exit_after_shutdown(
    result: &Result<app_runtime::AppShutdownSummary, app_runtime::AppShutdownError>,
) -> bool {
    match result {
        Ok(_) => true,
        Err(error) => error.summary.native_host_joined,
    }
}

fn decide_final_exit(startup: &StartupController) -> FinalExitDecision {
    decide_final_exit_target(
        startup.final_exit_target(),
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
        FinalExitTarget::AbortWithoutRustTeardown => false,
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
                    exit_allowed.store(true, Ordering::Release);
                    app_handle.exit(exit_code);
                } else {
                    eprintln!("Mom Llama remains open because startup native shutdown failed");
                }
            });
        }
        StartupShutdownAction::NoRuntime => {
            // Unlocking owns no native resources. A late Keychain result cannot
            // advance to native construction after this state transition.
            exit_allowed.store(true, Ordering::Release);
            app_handle.exit(exit_code);
        }
    }
}

fn log_shutdown_result(
    result: &Result<app_runtime::AppShutdownSummary, app_runtime::AppShutdownError>,
) {
    match serde_json::to_string(result) {
        Ok(receipt) => eprintln!("mom-llama shutdown: {receipt}"),
        Err(error) => eprintln!("Mom Llama could not encode its shutdown receipt: {error}"),
    }
}

fn build_runtime(
    gateway: Arc<Gateway>,
    settings: mom_llama_runtime::config::Settings,
) -> std::result::Result<AppRuntimeHandle, RuntimeBuildError> {
    let native_owner = mom_llama_runtime::native_runtime::ProductRuntimeOwner::initialize(
        &settings,
    )
    .map_err(|error| RuntimeBuildError {
        message: format!("native runtime owner initialization failed: {error:#}"),
        cleanup: None,
        completion: FailedBuildCompletion::PreOwner,
    })?;
    let attempt = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<_> {
        let (host, model) = mom_llama_runtime::gateway_native_configuration()?;
        let backend = Arc::new(LlamaNativeBackend::new_borrowed(Arc::clone(&host)));
        backend
            .replace_configuration(Arc::clone(&host), model)
            .map_err(anyhow::Error::msg)?;
        gateway
            .register_backend(backend.clone())
            .map_err(anyhow::Error::msg)?;
        Ok(backend)
    }));
    let (backend, native_owner) = finish_post_owner_build(attempt, native_owner, |owner| {
        cleanup_rejected_native_owner(owner)
    })
    .map_err(|failure| {
        let message = match failure.cause {
            PostOwnerBuildCause::Error(error) => {
                format!("native runtime composition failed: {error:#}")
            }
            PostOwnerBuildCause::Panic(message) => {
                format!("native runtime composition panicked: {message}")
            }
        };
        let NativeBuildCleanup {
            evidence,
            blocked_owner,
        } = failure.cleanup;
        let completion = match blocked_owner {
            Some(owner) => FailedBuildCompletion::PostOwnerCleanupBlocked(owner),
            None => FailedBuildCompletion::PostOwnerCleaned,
        };
        RuntimeBuildError {
            message,
            cleanup: Some(evidence),
            completion,
        }
    })?;
    Ok(AppRuntimeHandle::new(gateway, backend, native_owner))
}

fn finish_post_owner_build<T, E, O>(
    attempt: std::thread::Result<std::result::Result<T, E>>,
    owner: O,
    cleanup: impl FnOnce(O) -> NativeBuildCleanup,
) -> std::result::Result<(T, O), PostOwnerBuildFailure<E>> {
    match attempt {
        Ok(Ok(value)) => Ok((value, owner)),
        Ok(Err(error)) => Err(PostOwnerBuildFailure {
            cause: PostOwnerBuildCause::Error(error),
            cleanup: cleanup(owner),
        }),
        Err(payload) => Err(PostOwnerBuildFailure {
            cause: PostOwnerBuildCause::Panic(panic_message(payload)),
            cleanup: cleanup(owner),
        }),
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

fn cleanup_rejected_native_owner(
    owner: mom_llama_runtime::native_runtime::ProductRuntimeOwner,
) -> NativeBuildCleanup {
    let host = owner.host();
    match mom_llama_runtime::shutdown_product_runtime_for_process_exit(&host) {
        Ok(receipt) => NativeBuildCleanup {
            evidence: NativeBuildCleanupEvidence {
                native_host_joined: true,
                joined_native_worker_count: receipt.joined_worker_count(),
                error: None,
            },
            blocked_owner: None,
        },
        Err(error) => NativeBuildCleanup {
            evidence: NativeBuildCleanupEvidence {
                native_host_joined: false,
                joined_native_worker_count: 0,
                error: Some(error.to_string()),
            },
            // Keep the sole owner and its strong host reference alive. Final
            // Exit will abort without Rust teardown rather than dropping live
            // native resources after a failed join.
            blocked_owner: Some(owner),
        },
    }
}

fn build_gateway_plugin(gateway: Arc<Gateway>) -> TauriPlugin<tauri::Wry> {
    let mut builder = tauri_plugin_free_token_energy::Builder::new()
        .with_gateway(gateway)
        .with_store(Arc::new(MomGatewayStore))
        .with_default_loopback();
    if let Some(app_data_dir) =
        gateway_app_data_dir_override(std::env::var_os("LLAMA_NATIVE_KIT_DATA_DIR"))
    {
        builder = builder.with_app_data_dir(app_data_dir);
    }
    builder.build()
}

fn gateway_app_data_dir_override(
    configured: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    configured
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
}

struct MomGatewayStore;

impl ResponseStore for MomGatewayStore {
    fn put(&self, response: &GatewayResponse) -> Result<(), GatewayError> {
        let bytes = serde_json::to_vec(response).map_err(gateway_store_error)?;
        mom_llama_runtime::gateway_document_put(&gateway_response_namespace(&response.id), &bytes)
            .map_err(gateway_store_error)
    }

    fn get(&self, id: &str) -> Result<Option<GatewayResponse>, GatewayError> {
        mom_llama_runtime::gateway_document_get(&gateway_response_namespace(id))
            .map_err(gateway_store_error)?
            .map(|bytes| serde_json::from_slice(&bytes).map_err(gateway_store_error))
            .transpose()
    }

    fn delete(&self, id: &str) -> Result<bool, GatewayError> {
        mom_llama_runtime::gateway_document_delete(&gateway_response_namespace(id))
            .map_err(gateway_store_error)
    }
}

fn gateway_response_namespace(id: &str) -> String {
    format!("fte.response.v1:{id}")
}

fn gateway_store_error(error: impl std::fmt::Display) -> GatewayError {
    GatewayError {
        code: "mom_llama_gateway_store_error".to_string(),
        class: fte_types::ErrorClass::Internal,
        retryable: false,
        http_status: 500,
        request_id: RequestId::new(),
        provider: None,
        safe_detail: format!("Mom Llama gateway storage failed: {error}"),
    }
}

fn smoke() -> Result<()> {
    let app_html = view::render_app()?;
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
        FailedBuildCompletion, FailedBuildKind, FinalExitDecision, FinalExitTarget,
        NativeBuildCleanup, NativeBuildCleanupEvidence, PostOwnerBuildCause, StartupAction,
        StartupController, StartupShutdownAction, decide_final_exit_target,
        failed_build_disposition, final_exit_decision, finish_post_owner_build,
        gateway_app_data_dir_override, rejected_build_disposition, safe_to_exit_after_shutdown,
        smoke_receipt,
    };
    use crate::app_runtime::{AppShutdownError, AppShutdownSummary};
    use fte_router::{Gateway, GatewayDefaults};
    use std::sync::Arc;

    fn startup_controller() -> StartupController {
        StartupController::new(Arc::new(Gateway::new(GatewayDefaults {
            catalog_version: "startup-test".to_string(),
        })))
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
        startup.finish_failed_build(FailedBuildCompletion::PostOwnerCleaned);
        assert!(startup.wait_for_build().await);
        assert!(matches!(
            startup.begin_shutdown(),
            StartupShutdownAction::AlreadyQuiescing
        ));
    }

    #[test]
    fn only_pre_owner_failure_is_retryable() {
        assert_eq!(
            failed_build_disposition(false, FailedBuildKind::PreOwner),
            super::FailedBuildDisposition::Retryable
        );
        assert_eq!(
            failed_build_disposition(true, FailedBuildKind::PreOwner),
            super::FailedBuildDisposition::ExitSafe
        );
        assert_eq!(
            failed_build_disposition(false, FailedBuildKind::PostOwnerCleaned),
            super::FailedBuildDisposition::RestartRequired
        );
        assert_eq!(
            failed_build_disposition(true, FailedBuildKind::PostOwnerCleaned),
            super::FailedBuildDisposition::ExitSafe
        );
        assert_eq!(
            failed_build_disposition(false, FailedBuildKind::PostOwnerCleanupBlocked),
            super::FailedBuildDisposition::RetainAndAbortOnFinalExit
        );
        assert_eq!(
            failed_build_disposition(true, FailedBuildKind::PostOwnerCleanupBlocked),
            super::FailedBuildDisposition::RetainAndAbortOnFinalExit
        );

        let pre_owner = startup_controller();
        assert!(pre_owner.begin_initialization().is_ok());
        assert!(pre_owner.begin_runtime_build().is_ok());
        pre_owner.finish_failed_build(FailedBuildCompletion::PreOwner);
        assert_eq!(
            pre_owner.begin_initialization(),
            Ok(StartupAction::Initialize {
                retry_cached_failure: true
            })
        );

        let post_owner = startup_controller();
        assert!(post_owner.begin_initialization().is_ok());
        assert!(post_owner.begin_runtime_build().is_ok());
        post_owner.finish_failed_build(FailedBuildCompletion::PostOwnerCleaned);
        assert!(
            post_owner
                .begin_initialization()
                .expect_err("closed product lifecycle must not advertise retry")
                .contains("restarted")
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
            gateway_drained: true,
            native_host_joined: false,
            operation_supervisor_phase: crate::operation_supervisor::LifecyclePhase::Closed,
            active_operation_count: 0,
            retained_operation_task_count: 0,
            expected_operation_worker_count: 0,
            joined_operation_worker_count: 0,
            expected_native_worker_count: 0,
            joined_native_worker_count: 0,
            expected_worker_ids: Vec::new(),
            joined_worker_ids: Vec::new(),
            application_work_drained: true,
        };
        let failure = Err(AppShutdownError {
            summary: summary.clone(),
            operation_error: None,
            gateway_error: None,
            native_error: Some("native join failed".to_string()),
        });
        assert!(!safe_to_exit_after_shutdown(&failure));

        let gateway_only_failure = Err(AppShutdownError {
            summary: AppShutdownSummary {
                native_host_joined: true,
                ..summary
            },
            operation_error: None,
            gateway_error: Some("gateway drain failed".to_string()),
            native_error: None,
        });
        assert!(safe_to_exit_after_shutdown(&gateway_only_failure));
        assert_eq!(
            final_exit_decision(false),
            FinalExitDecision::AbortWithoutRustTeardown
        );
        assert_eq!(final_exit_decision(true), FinalExitDecision::ReturnToTauri);
        assert_eq!(
            decide_final_exit_target(
                FinalExitTarget::AbortWithoutRustTeardown,
                |_| panic!("blocked final exit must not run a runtime finalizer"),
                || panic!("blocked final exit must not wait for a build")
            ),
            FinalExitDecision::AbortWithoutRustTeardown
        );
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
    fn post_owner_error_and_panic_are_terminal_after_cleanup() {
        let failed_cleanup = || NativeBuildCleanup {
            evidence: NativeBuildCleanupEvidence {
                native_host_joined: false,
                joined_native_worker_count: 0,
                error: Some("injected finalizer failure".to_string()),
            },
            blocked_owner: None,
        };
        let attempt: std::thread::Result<Result<(), &str>> = Ok(Err("post-owner error"));
        let error = finish_post_owner_build(attempt, 7, |_| failed_cleanup())
            .expect_err("post-owner error must fail the build");
        assert!(matches!(
            error.cause,
            PostOwnerBuildCause::Error("post-owner error")
        ));
        assert!(!error.cleanup.evidence.native_host_joined);
        assert_eq!(
            error.cleanup.evidence.error.as_deref(),
            Some("injected finalizer failure")
        );

        let panic =
            finish_post_owner_build::<(), &str, _>(Err(Box::new("post-owner panic")), 9, |_| {
                NativeBuildCleanup {
                    evidence: NativeBuildCleanupEvidence {
                        native_host_joined: true,
                        joined_native_worker_count: 1,
                        error: None,
                    },
                    blocked_owner: None,
                }
            })
            .expect_err("post-owner panic must fail the build");
        assert!(matches!(
            panic.cause,
            PostOwnerBuildCause::Panic(ref message) if message == "post-owner panic"
        ));
        assert!(panic.cleanup.evidence.native_host_joined);

        let startup = startup_controller();
        assert!(startup.begin_initialization().is_ok());
        assert!(startup.begin_runtime_build().is_ok());
        startup.finish_failed_build(FailedBuildCompletion::PostOwnerCleaned);
        assert!(
            startup
                .begin_initialization()
                .expect_err("a cleaned-up post-owner panic must require process restart")
                .contains("restarted")
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
        assert!(script.contains("await invoke(\"mom_llama_runtime_initialize\")"));
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
    fn explicit_runtime_data_directory_also_owns_gateway_plugin_state() {
        let root = std::path::PathBuf::from("/tmp/mom-llama-acceptance");
        assert_eq!(
            gateway_app_data_dir_override(Some(root.clone().into_os_string())),
            Some(root)
        );
        assert_eq!(
            gateway_app_data_dir_override(Some(std::ffi::OsString::new())),
            None
        );
        assert_eq!(gateway_app_data_dir_override(None), None);
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
