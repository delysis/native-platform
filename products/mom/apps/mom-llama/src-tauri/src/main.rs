mod app_runtime;
mod command_registry;
mod commands;
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
use tauri::menu::{
    AboutMetadata, HELP_SUBMENU_ID, Menu, MenuItem, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID,
};
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Manager, Runtime, State};

const APPLICATION_QUIT_MENU_ID: &str = "mom-llama.application.quit";
const APPLICATION_QUIT_ACCELERATOR: &str = "CmdOrCtrl+Q";

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
    QuiescingSafe(Option<AppRuntimeHandle>),
    QuiescingDuringBuild,
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
            StartupState::QuiescingSafe(_) | StartupState::QuiescingDuringBuild => {
                Err("Mom Llama is shutting down".to_string())
            }
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
            StartupState::Building | StartupState::QuiescingDuringBuild => {
                *state = StartupState::QuiescingDuringBuild;
                StartupShutdownAction::WaitForBuild
            }
            StartupState::QuiescingSafe(_) => StartupShutdownAction::AlreadyQuiescing,
            _ => {
                *state = StartupState::QuiescingSafe(None);
                StartupShutdownAction::NoRuntime
            }
        }
    }

    fn finish_rejected_build(&self) {
        if let Ok(mut state) = self.state.lock()
            && matches!(&*state, StartupState::QuiescingDuringBuild)
        {
            *state = StartupState::QuiescingSafe(None);
        }
    }

    async fn wait_for_build(&self) {
        loop {
            let still_building = self
                .state
                .lock()
                .map(|state| matches!(&*state, StartupState::QuiescingDuringBuild))
                .unwrap_or(false);
            if !still_building {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    fn ready_runtime(&self) -> Option<AppRuntimeHandle> {
        let state = self.state.lock().ok()?;
        match &*state {
            StartupState::Ready(runtime) | StartupState::QuiescingSafe(Some(runtime)) => {
                Some(runtime.clone())
            }
            _ => None,
        }
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

    let gateway = Arc::clone(&startup.gateway);
    let built = match tauri::async_runtime::spawn_blocking(move || build_runtime(gateway, settings))
        .await
    {
        Ok(built) => built,
        Err(error) => {
            startup.record_failure();
            startup.finish_rejected_build();
            return Err(format!("Mom Llama startup worker failed: {error}"));
        }
    };

    match built {
        Ok(runtime) => match startup.install(&app, runtime) {
            Ok(()) => Ok(()),
            Err(runtime) => {
                runtime.begin_quiesce();
                let _ = runtime.shutdown().await;
                startup.record_failure();
                startup.finish_rejected_build();
                Err("Mom Llama stopped before encrypted runtime startup completed".to_string())
            }
        },
        Err(error) => {
            startup.record_failure();
            startup.finish_rejected_build();
            Err(format!(
                "Mom Llama could not initialize its local runtime: {error:#}"
            ))
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
                if let Some(runtime) = event_startup.ready_runtime() {
                    runtime.begin_quiesce();
                    let result = tauri::async_runtime::block_on(runtime.shutdown());
                    log_shutdown_result(&result);
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
        let safe_to_exit = match &result {
            Ok(_) => true,
            Err(error) => error.summary.native_host_joined,
        };
        if safe_to_exit {
            exit_allowed.store(true, Ordering::Release);
            app_handle.exit(exit_code);
        } else if let Err(error) = result {
            eprintln!("Mom Llama remains open because native shutdown failed: {error}");
        }
    });
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
                startup.wait_for_build().await;
                exit_allowed.store(true, Ordering::Release);
                app_handle.exit(exit_code);
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
) -> Result<AppRuntimeHandle> {
    let native_owner =
        mom_llama_runtime::native_runtime::ProductRuntimeOwner::initialize(&settings)?;
    let (host, model) = mom_llama_runtime::gateway_native_configuration()?;
    let backend = Arc::new(LlamaNativeBackend::new_borrowed(Arc::clone(&host)));
    backend
        .replace_configuration(Arc::clone(&host), model)
        .map_err(anyhow::Error::msg)?;
    gateway
        .register_backend(backend.clone())
        .map_err(anyhow::Error::msg)?;
    Ok(AppRuntimeHandle::new(gateway, backend, native_owner))
}

fn build_gateway_plugin(gateway: Arc<Gateway>) -> TauriPlugin<tauri::Wry> {
    tauri_plugin_free_token_energy::Builder::new()
        .with_gateway(gateway)
        .with_store(Arc::new(MomGatewayStore))
        .with_default_loopback()
        .build()
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
    use super::{StartupAction, StartupController, StartupShutdownAction, smoke_receipt};
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

    #[test]
    fn shutdown_waits_when_native_runtime_construction_has_started() {
        let startup = startup_controller();
        assert!(startup.begin_initialization().is_ok());
        assert!(startup.begin_runtime_build().is_ok());
        assert!(matches!(
            startup.begin_shutdown(),
            StartupShutdownAction::WaitForBuild
        ));
        startup.finish_rejected_build();
        assert!(matches!(
            startup.begin_shutdown(),
            StartupShutdownAction::AlreadyQuiescing
        ));
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
