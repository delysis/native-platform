mod commands;
mod view;

use anyhow::Result;
use fte_backend_llama::LlamaNativeBackend;
use fte_router::{Gateway, GatewayDefaults};
use fte_store::ResponseStore;
use fte_types::{GatewayError, GatewayResponse, RequestId};
use serde_json::json;
use std::sync::{Arc, OnceLock};
use tauri::plugin::TauriPlugin;

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

    let app = tauri::Builder::default()
        .plugin(gateway_plugin())
        .invoke_handler(tauri::generate_handler![
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
        Ok(app) => app.run(|_, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                mom_llama_runtime::unload_resident_model();
            }
        }),
        Err(error) => {
            eprintln!("failed to build Mom Llama: {error}");
            std::process::exit(1);
        }
    }
}

fn gateway_plugin() -> TauriPlugin<tauri::Wry> {
    let gateway = Arc::new(Gateway::new(GatewayDefaults {
        catalog_version: "mom-llama-local-v1".to_string(),
    }));
    match mom_llama_runtime::gateway_native_configuration() {
        Ok((host, model)) => {
            let backend = Arc::new(LlamaNativeBackend::new(Arc::clone(&host)));
            if let Err(error) = backend.replace_configuration(host, model) {
                eprintln!("Mom Llama could not configure its local gateway model: {error}");
            } else if let Err(error) = gateway.register_backend(backend.clone()) {
                eprintln!("Mom Llama could not register its local gateway backend: {error}");
            } else if GATEWAY_NATIVE_BACKEND.set(backend).is_err() {
                eprintln!("Mom Llama local gateway backend was initialized more than once");
            }
        }
        Err(error) => {
            eprintln!("Mom Llama could not initialize its local gateway host: {error}");
        }
    }
    tauri_plugin_free_token_energy::Builder::new()
        .with_gateway(gateway)
        .with_store(Arc::new(MomGatewayStore))
        .with_default_loopback()
        .build()
}

static GATEWAY_NATIVE_BACKEND: OnceLock<Arc<LlamaNativeBackend>> = OnceLock::new();

pub(crate) fn refresh_gateway_native_model() -> Result<(), String> {
    let Some(backend) = GATEWAY_NATIVE_BACKEND.get() else {
        return Ok(());
    };
    let (host, model) = mom_llama_runtime::gateway_native_configuration()
        .map_err(|error| format!("local gateway configuration failed: {error}"))?;
    backend
        .replace_configuration(host, model)
        .map_err(|error| format!("local gateway configuration failed: {error}"))
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
    use super::smoke_receipt;

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
