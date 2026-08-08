pub mod api_server;
pub mod backend;
pub mod catalog;
pub mod commands;
pub mod db;
pub mod eval_store;
pub mod gateway_v2;
pub mod providers;
pub mod rate_limiter;
pub mod router;
#[cfg(target_os = "macos")]
mod speech_smoke;

use crate::db::Database;
use crate::eval_store::EvalStore;
use crate::providers::Capability;
use crate::providers::anthropic;
use crate::providers::completions::CompletionProtocol;
use crate::providers::gemini;
use crate::providers::groq;
use crate::providers::openai_compatible::OpenAiCompatibleProvider;
use crate::providers::openrouter;
use crate::rate_limiter::QuotaTracker;
use crate::router::Router;
use fte_speech_gateway::SpeechGateway;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let gateway_v2 = gateway_v2::GatewayV2::new()
        .unwrap_or_else(|error| panic!("Free Token Energy gateway setup failed: {error}"));
    let plugin_gateway = gateway_v2.gateway();
    let plugin_secrets = gateway_v2.secrets();
    let speech_gateway = Arc::new(SpeechGateway::default());
    let parakeet_gateway = Arc::clone(&speech_gateway);
    #[cfg(target_os = "macos")]
    match tauri::async_runtime::block_on(
        fte_speech_platform::apple_backend::AppleSpeechBackend::discover(),
    ) {
        Ok(backend) => {
            if let Err(error) = speech_gateway.register_backend(Arc::new(backend)) {
                eprintln!("Apple speech backend registration failed: {error}");
            }
        }
        Err(error) => eprintln!("Apple speech backend discovery failed: {error}"),
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_free_token_energy::Builder::new()
                .with_gateway(plugin_gateway)
                .with_default_loopback()
                .build(),
        )
        .plugin(
            tauri_plugin_fte_speech::Builder::new()
                .with_speech_gateway(speech_gateway)
                .build(),
        )
        .setup(move |app| {
            tauri::async_runtime::spawn(async move {
                let backend = fte_speech_parakeet::ParakeetSpeechBackend::discover(
                    fte_speech_parakeet::ParakeetBackendConfig::default(),
                )
                .await;
                if let Err(error) = parakeet_gateway.register_backend(Arc::new(backend)) {
                    eprintln!("Parakeet speech backend registration failed: {error}");
                }
            });
            let app_data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data_dir)?;
            secure_app_data_directory(&app_data_dir)?;

            let db_path = app_data_dir.join("gateway.db");
            let db = Arc::new(Database::new(db_path)?);
            plugin_secrets.bind(Arc::clone(&db))?;

            let quota_tracker = Arc::new(QuotaTracker::new());
            let eval_store = Arc::new(EvalStore::new());

            let mut router = Router::new(quota_tracker.clone(), eval_store.clone(), db.clone());

            register_default_backends(&mut router)?;

            let router = Arc::new(router);
            app.manage(db);
            app.manage(router);

            #[cfg(target_os = "macos")]
            speech_smoke::start_if_requested(app.handle().clone());

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
