mod commands;
mod view;

use anyhow::Result;
use serde_json::json;

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
        .invoke_handler(tauri::generate_handler![
            commands::mom_llama_render_app,
            commands::mom_llama_render_chat_fragment,
            commands::mom_llama_render_sidebar_fragment,
            commands::mom_llama_render_settings_fragment,
            commands::mom_llama_pick_file,
            commands::mom_llama_engine_check,
            commands::mom_llama_engine_configure,
            commands::mom_llama_model_list,
            commands::mom_llama_model_select,
            commands::mom_llama_chat_send,
            commands::mom_llama_chat_cancel,
            commands::mom_llama_chat_regenerate,
            commands::mom_llama_chat_continue,
            commands::mom_llama_consult_panel_list,
            commands::mom_llama_consult_start,
            commands::mom_llama_consult_status,
            commands::mom_llama_consult_cancel,
            commands::mom_llama_consult_synthesize,
            commands::mom_llama_conversation_new,
            commands::mom_llama_conversation_list,
            commands::mom_llama_conversation_select,
            commands::mom_llama_conversation_search,
            commands::mom_llama_conversation_rename,
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
            commands::mom_llama_attachment_import_text,
            commands::mom_llama_attachment_import,
            commands::mom_llama_attachment_list,
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
            commands::mom_llama_tool_loop_run,
            commands::mom_llama_server_configure,
            commands::mom_llama_server_status,
            commands::mom_llama_server_start,
            commands::mom_llama_server_stop,
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
            eprintln!("failed to build Mom Llama Lab: {error}");
            std::process::exit(1);
        }
    }
}

fn smoke() -> Result<()> {
    let app_html = view::render_app()?;
    let smoke = json!({
        "schema": "mom_llama.tauri_app_smoke.v1",
        "status": "passed",
        "runtime": "tauri-maud-htmx",
        "rendered_html_bytes": app_html.len(),
        "visible_command_count": view::CONTROL_SPECS.len(),
        "forbidden_frontend_frameworks": false,
        "localhost_core_route_shim": false,
        "commands": view::CONTROL_SPECS.iter().map(|control| {
            json!({
                "affordance": control.affordance,
                "command": control.command,
                "tauri_command": control.tauri_command,
                "cli": control.cli,
                "effect": control.effect,
            })
        }).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&smoke)?);
    Ok(())
}
