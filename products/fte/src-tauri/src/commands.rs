use crate::api_server::{ProxyManager, ProxyStatus};
use crate::backend::CredentialRequirement;
use crate::db::Database;
use crate::providers::{ChatRequest, ChatResponse, CompletionRequest, CompletionResponse};
use crate::router::Router;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn chat_request(
    router: State<'_, Arc<Router>>,
    req: ChatRequest,
    task_hint: Option<String>,
) -> Result<ChatResponse, String> {
    let hint = task_hint.unwrap_or_else(|| "general".to_string());
    router.chat(&req, &hint).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn completion_request(
    router: State<'_, Arc<Router>>,
    req: CompletionRequest,
    task_hint: Option<String>,
) -> Result<CompletionResponse, String> {
    let hint = task_hint.unwrap_or_else(|| "general".to_string());
    router
        .complete(&req, &hint)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn save_key(
    db: State<'_, Arc<Database>>,
    router: State<'_, Arc<Router>>,
    provider_id: String,
    key_value: String,
) -> Result<(), String> {
    let provider_id = provider_id.trim().to_ascii_lowercase();
    if !router.supports_provider(&provider_id) {
        return Err(format!("Unsupported provider '{provider_id}'."));
    }
    if router.credential_requirement(&provider_id) != Some(CredentialRequirement::ApiKey) {
        return Err(format!(
            "Inference backend '{provider_id}' does not accept a provider API key."
        ));
    }
    let key_value = key_value.trim();
    if key_value.len() < 8 || key_value.len() > 16_384 {
        return Err("API key must be between 8 and 16384 characters.".to_string());
    }
    if key_value.chars().any(char::is_control) {
        return Err("API key must not contain control characters.".to_string());
    }

    db.save_api_key(&provider_id, key_value)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_key(
    db: State<'_, Arc<Database>>,
    router: State<'_, Arc<Router>>,
    provider_id: String,
) -> Result<bool, String> {
    let provider_id = provider_id.trim().to_ascii_lowercase();
    if !router.supports_provider(&provider_id) {
        return Err(format!("Unsupported provider '{provider_id}'."));
    }
    if router.credential_requirement(&provider_id) != Some(CredentialRequirement::ApiKey) {
        return Err(format!(
            "Inference backend '{provider_id}' does not use a provider API key."
        ));
    }
    db.delete_api_key(&provider_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn save_profile_field(
    db: State<'_, Arc<Database>>,
    key: String,
    value: String,
) -> Result<(), String> {
    let key = key.trim();
    let value = value.trim();
    let max_length = match key {
        "email" => 320,
        "name" => 200,
        "password_hint" => 200,
        _ => return Err(format!("Unsupported profile field '{key}'.")),
    };
    if value.len() > max_length {
        return Err(format!("{key} exceeds the {max_length}-character limit."));
    }
    if value.chars().any(|character| character.is_control()) {
        return Err(format!("{key} must not contain control characters."));
    }
    if key == "email" && !value.is_empty() && !looks_like_email(value) {
        return Err("Enter a valid email address.".to_string());
    }

    db.save_profile_field(key, value).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_master_profile(
    db: State<'_, Arc<Database>>,
) -> Result<std::collections::HashMap<String, String>, String> {
    db.get_master_profile().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_dashboard_stats(
    db: State<'_, Arc<Database>>,
    router: State<'_, Arc<Router>>,
) -> Result<serde_json::Value, String> {
    let summary = db.get_global_log_summary().map_err(|e| e.to_string())?;
    let headroom = router
        .global_headroom_percent()
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "total_tokens": summary.total_tokens,
        "avg_latency": summary.avg_latency_ms,
        "request_count": summary.request_count,
        "headroom": headroom,
    }))
}

#[tauri::command]
pub async fn get_recent_logs(
    db: State<'_, Arc<Database>>,
) -> Result<Vec<serde_json::Value>, String> {
    let logs = db.get_recent_logs(50).map_err(|e| e.to_string())?;
    Ok(logs
        .into_iter()
        .map(|l| {
            serde_json::json!({
                "timestamp": l.timestamp,
                "provider": l.provider_id,
                "model": l.model_id,
                "tokens": l.tokens_used,
                "latency": l.latency_ms,
                "status": l.status_code,
            })
        })
        .collect())
}

#[tauri::command]
pub async fn get_providers(
    router: State<'_, Arc<Router>>,
) -> Result<Vec<crate::router::ProviderStatus>, String> {
    router.provider_statuses().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_models(
    router: State<'_, Arc<Router>>,
) -> Result<Vec<crate::router::PublicModel>, String> {
    Ok(router.public_models())
}

#[tauri::command]
pub async fn get_proxy_status(proxy: State<'_, Arc<ProxyManager>>) -> Result<ProxyStatus, String> {
    Ok(proxy.status().await)
}

#[tauri::command]
pub async fn restart_proxy(
    db: State<'_, Arc<Database>>,
    proxy: State<'_, Arc<ProxyManager>>,
    port: u16,
) -> Result<ProxyStatus, String> {
    let proxy = proxy.inner().clone();
    let status = proxy
        .restart(port)
        .await
        .map_err(|error| error.to_string())?;
    db.save_setting("proxy_port", &port.to_string())
        .map_err(|error| error.to_string())?;
    Ok(status)
}

fn looks_like_email(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !value.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::looks_like_email;

    #[test]
    fn validates_basic_email_shape() {
        assert!(looks_like_email("person@example.com"));
        assert!(!looks_like_email("person"));
        assert!(!looks_like_email("@example.com"));
        assert!(!looks_like_email("person@example"));
    }
}
