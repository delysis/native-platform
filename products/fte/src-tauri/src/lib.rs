mod acceptance;
pub mod catalog;
pub mod commands;
pub mod db;
pub mod gateway_runtime;
pub mod secrets;
use crate::db::Database;
use crate::secrets::EphemeralCredentialStore;
use fte_router::GatewayShutdownReport;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tauri::menu::{
    AboutMetadata, HELP_SUBMENU_ID, Menu, MenuItem, PredefinedMenuItem, Submenu, WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Manager, Runtime};

const APPLICATION_QUIT_MENU_ID: &str = "free-token-energy.application.quit";
const APPLICATION_QUIT_ACCELERATOR: &str = "CmdOrCtrl+Q";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let acceptance = acceptance::AcceptanceIsolation::from_environment()
        .unwrap_or_else(|error| panic!("Free Token Energy acceptance isolation failed: {error}"));
    if let Some(isolation) = &acceptance {
        secure_app_data_directory(isolation.root()).unwrap_or_else(|error| {
            panic!("Free Token Energy acceptance directory could not be secured: {error}")
        });
    }
    let gateway_runtime = Arc::new(
        match &acceptance {
            Some(_) => gateway_runtime::GatewayRuntimeOwner::new_with_store(Arc::new(
                EphemeralCredentialStore::default(),
            )),
            None => gateway_runtime::GatewayRuntimeOwner::new(),
        }
        .unwrap_or_else(|error| panic!("Free Token Energy gateway setup failed: {error}")),
    );
    let plugin_gateway = gateway_runtime.gateway();
    let desktop_gateway_runtime = Arc::clone(&gateway_runtime);
    let event_gateway_runtime = Arc::clone(&gateway_runtime);
    let exit_allowed = Arc::new(AtomicBool::new(false));
    let event_exit_allowed = Arc::clone(&exit_allowed);
    let shutdown_started = Arc::new(AtomicBool::new(false));
    let event_shutdown_started = Arc::clone(&shutdown_started);
    let mut plugin_builder = tauri_plugin_free_token_energy::Builder::new()
        .with_gateway(plugin_gateway)
        .with_default_loopback()
        .with_application_managed_exit();
    if let Some(isolation) = &acceptance {
        plugin_builder = plugin_builder.with_app_data_dir(isolation.root().to_path_buf());
    }
    let desktop_app_data_dir = acceptance.clone();
    let app = tauri::Builder::default()
        // The stock macOS Quit item may enter AppKit termination before the
        // native host is drained. Own Cmd+Q so teardown runs asynchronously
        // while the event loop is still available to Metal.
        .enable_macos_default_menu(false)
        .menu(build_desktop_menu)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(plugin_builder.build())
        .setup(move |app| {
            let app_data_dir = match &desktop_app_data_dir {
                Some(isolation) => isolation.root().to_path_buf(),
                None => app.path().app_data_dir()?,
            };
            std::fs::create_dir_all(&app_data_dir)?;
            secure_app_data_directory(&app_data_dir)?;

            let db_path = match &desktop_app_data_dir {
                Some(isolation) => isolation.desktop_database(),
                None => app_data_dir.join("gateway.db"),
            };
            let db = Arc::new(Database::new(db_path)?);
            desktop_gateway_runtime.bind_database(Arc::clone(&db))?;
            desktop_gateway_runtime.restore_local_model_configuration()?;

            app.manage(db);
            app.manage(desktop_gateway_runtime);
            eprintln!(
                "free-token-energy runtime ready: {}",
                app_data_dir.display()
            );

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::chat_request,
            commands::completion_request,
            commands::configure_local_model,
            commands::choose_local_model,
            commands::get_local_model_status,
            commands::save_key,
            commands::delete_key,
            commands::get_providers,
            commands::get_models,
            commands::save_profile_field,
            commands::get_master_profile,
            commands::get_dashboard_stats,
            commands::get_recent_logs,
        ])
        .build(tauri::generate_context!());
    match app {
        Ok(app) => app.run(move |app_handle, event| {
            if let tauri::RunEvent::MenuEvent(menu_event) = &event
                && menu_event.id() == APPLICATION_QUIT_MENU_ID
            {
                request_graceful_exit(
                    app_handle,
                    &event_gateway_runtime,
                    &event_shutdown_started,
                    &event_exit_allowed,
                    0,
                );
                return;
            }
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if event_exit_allowed.load(Ordering::Acquire) {
                    return;
                }
                api.prevent_exit();
                request_graceful_exit(
                    app_handle,
                    &event_gateway_runtime,
                    &event_shutdown_started,
                    &event_exit_allowed,
                    code.unwrap_or(0),
                );
                return;
            }
            if let tauri::RunEvent::Exit = event
                && !event_exit_allowed.load(Ordering::Acquire)
            {
                // Some OS-level termination paths cannot be delayed. A hard
                // stop is preferable to joining a Metal worker from inside
                // AppKit's synchronous terminate callback, which can deadlock
                // process exit.
                eprintln!(
                    "Free Token Energy aborts an uncoordinated final exit before Rust or Metal teardown"
                );
                std::process::abort();
            }
        }),
        Err(error) => eprintln!("Free Token Energy could not start: {error}"),
    }
}

fn request_graceful_exit<R: Runtime>(
    app_handle: &AppHandle<R>,
    runtime: &Arc<gateway_runtime::GatewayRuntimeOwner>,
    shutdown_started: &Arc<AtomicBool>,
    exit_allowed: &Arc<AtomicBool>,
    exit_code: i32,
) {
    if exit_allowed.load(Ordering::Acquire) || shutdown_started.swap(true, Ordering::AcqRel) {
        return;
    }
    let app_handle = app_handle.clone();
    let runtime = Arc::clone(runtime);
    let shutdown_started = Arc::clone(shutdown_started);
    let exit_allowed = Arc::clone(exit_allowed);
    tauri::async_runtime::spawn(async move {
        let started_at = Instant::now();
        let report = runtime.shutdown_with_report().await;
        let gateway_drained = gateway_report_is_drained(&report.gateway);
        eprintln!(
            "free-token-energy shutdown: elapsed_ms={} gateway_drained={} native_host_joined={} expected_workers={} joined_workers={} retained_tasks={}",
            started_at.elapsed().as_millis(),
            gateway_drained,
            report.native_host_joined,
            report.gateway.expected_worker_ids.len(),
            report.gateway.joined_worker_ids.len(),
            report.gateway.retained_tasks,
        );
        if gateway_drained && report.native_host_joined {
            exit_allowed.store(true, Ordering::Release);
            app_handle.exit(exit_code);
        } else {
            shutdown_started.store(false, Ordering::Release);
            eprintln!("Free Token Energy remains open because shutdown did not join every worker");
        }
    });
}

fn gateway_report_is_drained(report: &GatewayShutdownReport) -> bool {
    report.retained_tasks == 0
        && report.expected_worker_ids.len() == report.joined_worker_ids.len()
        && report
            .expected_worker_ids
            .iter()
            .all(|worker| report.joined_worker_ids.contains(worker))
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

#[cfg(unix)]
fn secure_app_data_directory(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn secure_app_data_directory(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shutdown_report(joined_worker_ids: &[&str], retained_tasks: usize) -> GatewayShutdownReport {
        GatewayShutdownReport {
            result: Ok(()),
            expected_worker_ids: vec!["hosted".to_string(), "llama-native".to_string()],
            joined_worker_ids: joined_worker_ids
                .iter()
                .map(|worker| (*worker).to_string())
                .collect(),
            retained_tasks,
        }
    }

    #[test]
    fn exit_requires_every_expected_gateway_worker_and_no_retained_task() {
        assert!(gateway_report_is_drained(&shutdown_report(
            &["llama-native", "hosted"],
            0,
        )));
        assert!(!gateway_report_is_drained(
            &shutdown_report(&["hosted"], 0,)
        ));
        assert!(!gateway_report_is_drained(&shutdown_report(
            &["hosted", "llama-native"],
            1,
        )));
    }
}
