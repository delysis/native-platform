pub mod catalog;
pub mod commands;
pub mod credential_migration;
pub mod db;
pub mod gateway_runtime;
pub mod secrets;
use crate::db::Database;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let gateway_runtime = Arc::new(
        gateway_runtime::GatewayRuntimeOwner::new()
            .unwrap_or_else(|error| panic!("Free Token Energy gateway setup failed: {error}")),
    );
    let plugin_gateway = gateway_runtime.gateway();
    let desktop_gateway_runtime = Arc::clone(&gateway_runtime);
    let run_result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_free_token_energy::Builder::new()
                .with_gateway(plugin_gateway)
                .with_default_loopback()
                .build(),
        )
        .setup(move |app| {
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            secure_app_data_directory(&app_data_dir)?;

            let db_path = app_data_dir.join("gateway.db");
            let db = Arc::new(Database::new(db_path)?);
            credential_migration::migrate_legacy_credentials(
                &db,
                desktop_gateway_runtime.credential_store().as_ref(),
                |_| Ok(()),
            )?;
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
        .run(tauri::generate_context!());
    assert!(
        gateway_runtime.shutdown_native_for_process_exit(),
        "the application-owned native host did not return process-exit join evidence"
    );
    if let Err(error) = run_result {
        eprintln!("Free Token Energy could not start: {error}");
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
