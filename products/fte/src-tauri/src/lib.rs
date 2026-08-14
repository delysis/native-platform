mod acceptance;
pub mod catalog;
pub mod commands;
pub mod db;
pub mod gateway_runtime;
pub mod secrets;
use crate::db::Database;
use crate::secrets::EphemeralCredentialStore;
use std::sync::Arc;
use tauri::Manager;

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
    let mut plugin_builder = tauri_plugin_free_token_energy::Builder::new()
        .with_gateway(plugin_gateway)
        .with_default_loopback();
    if let Some(isolation) = &acceptance {
        plugin_builder = plugin_builder.with_app_data_dir(isolation.root().to_path_buf());
    }
    let desktop_app_data_dir = acceptance.clone();
    let app = tauri::Builder::default()
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
        Ok(app) => app.run(move |_app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Tauri dispatches plugin events before this callback. The FTE
                // plugin has therefore drained the Gateway and its borrowed
                // adapter before the application-owned native host is joined.
                // This callback must run inside App::run: Builder::run never
                // returns before AppKit begins process-global Metal teardown.
                assert!(
                    event_gateway_runtime.shutdown_native_for_process_exit(),
                    "the application-owned native host did not return process-exit join evidence"
                );
            }
        }),
        Err(error) => eprintln!("Free Token Energy could not start: {error}"),
    }
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
