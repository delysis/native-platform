pub mod api_server;
pub mod backend;
pub mod catalog;
pub mod commands;
pub mod db;
pub mod eval_store;
pub mod providers;
pub mod rate_limiter;
pub mod router;

use crate::db::Database;
use crate::eval_store::EvalStore;
use crate::providers::anthropic;
use crate::providers::completions::CompletionProtocol;
use crate::providers::gemini;
use crate::providers::groq;
use crate::providers::openai_compatible::OpenAiCompatibleProvider;
use crate::providers::openrouter;
use crate::providers::Capability;
use crate::rate_limiter::QuotaTracker;
use crate::router::Router;
use std::sync::Arc;
use tauri::Manager;
use tracing::warn;

const DEFAULT_PROXY_PORT: u16 = 1337;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            secure_app_data_directory(&app_data_dir)?;

            let db_path = app_data_dir.join("gateway.db");
            let db = Arc::new(Database::new(db_path)?);

            let quota_tracker = Arc::new(QuotaTracker::new());
            let eval_store = Arc::new(EvalStore::new());

            let mut router = Router::new(quota_tracker.clone(), eval_store.clone(), db.clone());

            register_default_backends(&mut router)?;

            let router = Arc::new(router);
            let proxy = crate::api_server::ProxyManager::new(router.clone());
            let saved_port = db
                .get_setting("proxy_port")?
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|port| *port >= 1024)
                .unwrap_or(DEFAULT_PROXY_PORT);
            let proxy_to_start = proxy.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = proxy_to_start.restart(saved_port).await {
                    warn!("Could not start local API proxy: {error}");
                }
            });

            app.manage(db);
            app.manage(router);
            app.manage(proxy);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::chat_request,
            commands::completion_request,
            commands::save_key,
            commands::delete_key,
            commands::get_providers,
            commands::get_models,
            commands::save_profile_field,
            commands::get_master_profile,
            commands::get_dashboard_stats,
            commands::get_recent_logs,
            commands::get_proxy_status,
            commands::restart_proxy,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| eprintln!("Free Token Energy could not start: {error}"));
}

fn register_default_backends(router: &mut Router) -> anyhow::Result<()> {
    router.add_backend(Box::new(openrouter::provider()))?;
    router.add_backend(Box::new(groq::provider()))?;
    router.add_backend(Box::new(anthropic::provider()))?;
    router.add_backend(Box::new(gemini::provider()))?;
    router.add_backend(Box::new(
        OpenAiCompatibleProvider::new(
            "mistral",
            "Mistral AI",
            "https://api.mistral.ai/v1/chat/completions",
            vec![Capability::Streaming, Capability::Tools],
        )
        .with_completion_endpoint(
            "https://api.mistral.ai/v1/fim/completions",
            CompletionProtocol::MistralFim,
        ),
    ))?;
    router.add_backend(Box::new(OpenAiCompatibleProvider::new(
        "nvidia",
        "NVIDIA NIM",
        "https://integrate.api.nvidia.com/v1/chat/completions",
        vec![Capability::Streaming, Capability::Tools],
    )))?;
    router.add_backend(Box::new(
        OpenAiCompatibleProvider::new(
            "cerebras",
            "Cerebras",
            "https://api.cerebras.ai/v1/chat/completions",
            vec![Capability::Streaming, Capability::Tools],
        )
        .with_completion_endpoint(
            "https://api.cerebras.ai/v1/completions",
            CompletionProtocol::OpenAi,
        ),
    ))?;
    Ok(())
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

    #[test]
    fn bundled_backends_conform_to_the_model_catalog() {
        let path = std::env::temp_dir().join(format!(
            "free-token-energy-backend-catalog-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = Arc::new(Database::new(path).unwrap());
        let mut router = Router::new(
            Arc::new(QuotaTracker::new()),
            Arc::new(EvalStore::new()),
            db,
        );

        register_default_backends(&mut router).unwrap();

        for id in [
            "openrouter",
            "groq",
            "anthropic",
            "gemini",
            "mistral",
            "nvidia",
            "cerebras",
        ] {
            assert!(router.supports_provider(id), "missing backend {id}");
        }
    }
}
