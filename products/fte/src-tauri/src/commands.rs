use crate::db::Database;
use crate::gateway_runtime::{GatewayRuntimeOwner, LocalModelStatus, ProviderStatus, PublicModel};
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub async fn chat_request(
    runtime: State<'_, Arc<GatewayRuntimeOwner>>,
    req: serde_json::Value,
    task_hint: Option<String>,
) -> Result<serde_json::Value, String> {
    reject_legacy_task_hint(task_hint)?;
    runtime.chat(req).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn completion_request(
    runtime: State<'_, Arc<GatewayRuntimeOwner>>,
    req: serde_json::Value,
    task_hint: Option<String>,
) -> Result<serde_json::Value, String> {
    reject_legacy_task_hint(task_hint)?;
    runtime
        .complete(req)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn choose_local_model(
    app: AppHandle,
    runtime: State<'_, Arc<GatewayRuntimeOwner>>,
    expected_sha256: Option<String>,
) -> Result<LocalModelStatus, String> {
    let selected = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Choose a local GGUF model")
            .add_filter("GGUF model", &["gguf"])
            .blocking_pick_file()
    })
    .await
    .map_err(|error| format!("Local model picker failed: {error}"))?;
    let Some(selected) = selected else {
        return runtime
            .local_model_status()
            .map_err(|error| error.to_string());
    };
    let path = selected
        .into_path()
        .map_err(|error| format!("The selected model is not a local file: {error}"))?;
    runtime
        .configure_local_model(path, expected_sha256)
        .map_err(|error| error.to_string())?;
    runtime
        .local_model_status()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_local_model_status(
    runtime: State<'_, Arc<GatewayRuntimeOwner>>,
) -> Result<LocalModelStatus, String> {
    runtime
        .local_model_status()
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn configure_local_model(
    runtime: State<'_, Arc<GatewayRuntimeOwner>>,
    model_path: String,
    expected_sha256: Option<String>,
) -> Result<String, String> {
    runtime
        .configure_local_model(model_path, expected_sha256)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn save_key(
    runtime: State<'_, Arc<GatewayRuntimeOwner>>,
    provider_id: String,
    key_value: String,
) -> Result<(), String> {
    let provider_id = provider_id.trim().to_ascii_lowercase();
    if !runtime.supports_provider(&provider_id) {
        return Err(format!("Unsupported provider '{provider_id}'."));
    }
    let key_value = key_value.trim();
    if key_value.len() < 8 || key_value.len() > 16_384 {
        return Err("API key must be between 8 and 16384 characters.".to_string());
    }
    if key_value.chars().any(char::is_control) {
        return Err("API key must not contain control characters.".to_string());
    }

    runtime
        .save_credential(&provider_id, key_value)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_key(
    runtime: State<'_, Arc<GatewayRuntimeOwner>>,
    provider_id: String,
) -> Result<bool, String> {
    let provider_id = provider_id.trim().to_ascii_lowercase();
    if !runtime.supports_provider(&provider_id) {
        return Err(format!("Unsupported provider '{provider_id}'."));
    }
    runtime
        .delete_credential(&provider_id)
        .map_err(|e| e.to_string())
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
    runtime: State<'_, Arc<GatewayRuntimeOwner>>,
) -> Result<serde_json::Value, String> {
    let summary = db.get_global_log_summary().map_err(|e| e.to_string())?;
    let headroom = runtime
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
    runtime: State<'_, Arc<GatewayRuntimeOwner>>,
) -> Result<Vec<ProviderStatus>, String> {
    runtime.provider_statuses().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_models(
    runtime: State<'_, Arc<GatewayRuntimeOwner>>,
) -> Result<Vec<PublicModel>, String> {
    Ok(runtime.public_models())
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

fn reject_legacy_task_hint(task_hint: Option<String>) -> Result<(), String> {
    if task_hint.is_some() {
        return Err(
            "task_hint is no longer accepted: the legacy task router was retired, and the modern Gateway has no equivalent typed evaluation signal; omit task_hint"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{looks_like_email, reject_legacy_task_hint};

    #[test]
    fn validates_basic_email_shape() {
        assert!(looks_like_email("person@example.com"));
        assert!(!looks_like_email("person"));
        assert!(!looks_like_email("@example.com"));
        assert!(!looks_like_email("person@example"));
    }

    #[test]
    fn legacy_task_hint_is_explicitly_rejected_instead_of_ignored() {
        assert!(reject_legacy_task_hint(None).is_ok());
        let error = reject_legacy_task_hint(Some("coding".to_string())).unwrap_err();
        assert!(error.contains("no equivalent typed evaluation signal"));
        assert!(reject_legacy_task_hint(Some(String::new())).is_err());
    }
}
