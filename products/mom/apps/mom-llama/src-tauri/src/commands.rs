use llama_native_types::NativeDevice;
use mom_llama_runtime::{
    ChatSendInput, ChatSendOptions, ConsultPersona, ConsultStartInput, ConsultStartOptions,
    ConversationExportFormat, EngineCheckOptions, KvCachePolicy, config::SettingsUpdate,
};
#[cfg(target_os = "macos")]
use rfd::FileDialog;
use serde::Deserialize;
use serde_json::{Value, to_value};
use std::path::PathBuf;
use tauri::ipc::Response;
use tauri::{Emitter, Window};

#[tauri::command]
pub fn mom_llama_render_app() -> Result<Response, String> {
    markup_response(crate::view::render_app())
}

#[tauri::command]
pub fn mom_llama_render_chat_fragment() -> Result<Response, String> {
    markup_response(crate::view::render_chat_fragment())
}

#[tauri::command]
pub fn mom_llama_render_sidebar_fragment() -> Result<Response, String> {
    markup_response(crate::view::render_sidebar_fragment())
}

#[tauri::command]
pub fn mom_llama_render_persona_picker_fragment() -> Result<Response, String> {
    markup_response(crate::view::render_persona_picker_fragment())
}

#[tauri::command]
pub fn mom_llama_render_settings_fragment() -> Result<Response, String> {
    markup_response(crate::view::render_settings_fragment())
}

#[tauri::command]
#[cfg(target_os = "macos")]
pub async fn mom_llama_pick_file(kind: String) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dialog = match kind.as_str() {
            "model" => {
                let dialog = FileDialog::new().add_filter("GGUF model", &["gguf"]);
                match mom_llama_runtime::hugging_face_hub_cache_dir() {
                    Some(cache) if cache.is_dir() => dialog.set_directory(cache),
                    _ => dialog,
                }
            }
            "mmproj" => FileDialog::new().add_filter("GGUF projector", &["gguf"]),
            "conversation" => FileDialog::new().add_filter("Conversation", &["json"]),
            "attachment" => FileDialog::new().add_filter(
                "Supported attachment",
                &[
                    "txt", "md", "csv", "json", "png", "jpg", "jpeg", "webp", "wav", "mp3", "flac",
                    "pdf",
                ],
            ),
            "mcp" => FileDialog::new(),
            other => return Err(format!("unsupported native file picker kind: {other}")),
        };
        Ok(dialog.pick_file().map(|path| path.display().to_string()))
    })
    .await
    .map_err(|error| format!("native file picker task failed: {error}"))?
}

#[tauri::command]
#[cfg(not(target_os = "macos"))]
pub async fn mom_llama_pick_file(_kind: String) -> Result<Option<String>, String> {
    Err("The native file picker is currently available in the macOS release target.".to_string())
}

#[tauri::command]
pub async fn mom_llama_engine_check() -> Result<Value, String> {
    blocking_command(move || mom_llama_runtime::engine_check(EngineCheckOptions::default())).await
}

#[tauri::command]
pub fn mom_llama_engine_configure(
    model_path: String,
    device: Option<String>,
    context_tokens: Option<u32>,
    batch_tokens: Option<u32>,
    max_parallel_sequences: Option<u32>,
    memory_budget_mib: Option<u64>,
) -> Result<Value, String> {
    let value = command_value(mom_llama_runtime::configure_engine(
        PathBuf::from(model_path),
        device.as_deref().map(native_device_from_str),
        context_tokens,
        batch_tokens,
        max_parallel_sequences,
        memory_budget_mib.map(mib_to_bytes),
    ))?;
    crate::refresh_gateway_native_model()?;
    Ok(value)
}

#[tauri::command]
pub fn mom_llama_model_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::model_list())
}

#[tauri::command]
pub fn mom_llama_model_select(model_path: String) -> Result<Value, String> {
    let value = command_value(mom_llama_runtime::model_select(PathBuf::from(model_path)))?;
    crate::refresh_gateway_native_model()?;
    Ok(value)
}

#[tauri::command]
pub async fn mom_llama_chat_send(
    window: Window,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let events = window.clone();
    blocking_command(move || {
        mom_llama_runtime::chat_send_stream(
            ChatSendInput {
                conversation_id: conversation,
                message,
            },
            ChatSendOptions::default(),
            move |event| {
                events
                    .emit("mom_llama_chat_stream", &event)
                    .map_err(anyhow::Error::new)?;
                Ok(())
            },
        )
    })
    .await
}

#[tauri::command]
pub async fn mom_llama_chat_dispatch(
    window: Window,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let events = window.clone();
    blocking_command(move || {
        mom_llama_runtime::chat_dispatch_stream(
            mom_llama_runtime::MentionDispatchInput {
                conversation_id: conversation,
                message,
            },
            ChatSendOptions::default(),
            Some(move |event| {
                events
                    .emit("mom_llama_chat_dispatch_stream", &event)
                    .map_err(anyhow::Error::new)?;
                Ok(())
            }),
        )
    })
    .await
}

#[tauri::command]
pub async fn mom_llama_mention_dispatch(
    window: Window,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let events = window.clone();
    blocking_command(move || {
        let mut result = mom_llama_runtime::chat_dispatch_stream(
            mom_llama_runtime::MentionDispatchInput {
                conversation_id: conversation,
                message,
            },
            ChatSendOptions::default(),
            Some(move |event| {
                events
                    .emit("mom_llama_chat_dispatch_stream", &event)
                    .map_err(anyhow::Error::new)?;
                Ok(())
            }),
        )?;
        result.command = "mom_llama.mention_dispatch".to_string();
        result.receipt.command = "mom_llama.mention_dispatch".to_string();
        Ok(result)
    })
    .await
}

#[tauri::command]
pub fn mom_llama_mention_candidates(
    query: String,
    conversation: Option<String>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::mention_candidates(
        &query,
        conversation.as_deref(),
    ))
}

#[tauri::command]
pub fn mom_llama_mention_cancel(
    invocation: String,
    target: Option<String>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::mention_cancel(
        &invocation,
        target.as_deref(),
    ))
}

#[tauri::command]
pub async fn mom_llama_mention_synthesize(invocation: String) -> Result<Value, String> {
    blocking_command(move || mom_llama_runtime::mention_synthesize(&invocation)).await
}

#[tauri::command]
pub fn mom_llama_persona_freeze(
    conversation: String,
    message: String,
    name: String,
    handle: String,
    history: String,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_freeze(
        mom_llama_runtime::PersonaFreezeInput {
            conversation_id: conversation,
            message_id: message,
            name,
            mention_handle: handle,
            history_mode: match history.as_str() {
                "system_only" => mom_llama_runtime::PersonaHistoryMode::SystemOnly,
                "empty" => mom_llama_runtime::PersonaHistoryMode::Empty,
                _ => mom_llama_runtime::PersonaHistoryMode::Full,
            },
        },
    ))
}

#[tauri::command]
pub fn mom_llama_persona_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_list())
}

#[tauri::command]
pub fn mom_llama_persona_get(persona: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_get(&persona))
}

#[tauri::command]
pub fn mom_llama_persona_update(
    profile: mom_llama_runtime::PersonaUpdateInput,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_update(profile))
}

#[tauri::command]
pub fn mom_llama_persona_delete(persona: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_delete(&persona))
}

#[tauri::command]
pub fn mom_llama_persona_instantiate(
    persona: String,
    title: Option<String>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_instantiate(&persona, title))
}

#[tauri::command]
pub fn mom_llama_persona_group_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_group_list())
}

#[tauri::command]
pub fn mom_llama_persona_group_create(
    name: String,
    handle: String,
    personas: Vec<String>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_group_create(
        name, handle, personas,
    ))
}

#[tauri::command]
pub fn mom_llama_persona_group_update(
    group: String,
    name: String,
    handle: String,
    personas: Vec<String>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_group_update(
        group, name, handle, personas,
    ))
}

#[tauri::command]
pub fn mom_llama_persona_group_delete(group: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_group_delete(&group))
}

#[tauri::command]
pub fn mom_llama_chat_cancel(conversation: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::chat_cancel(&conversation))
}

#[tauri::command]
pub fn mom_llama_chat_skip_reasoning(conversation: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::chat_skip_reasoning(&conversation))
}

#[tauri::command]
pub async fn mom_llama_chat_regenerate(conversation: String) -> Result<Value, String> {
    blocking_command(move || {
        mom_llama_runtime::chat_regenerate(&conversation, ChatSendOptions::default())
    })
    .await
}

#[tauri::command]
pub async fn mom_llama_chat_continue(conversation: String) -> Result<Value, String> {
    blocking_command(move || {
        mom_llama_runtime::chat_continue(&conversation, ChatSendOptions::default())
    })
    .await
}

#[tauri::command]
pub fn mom_llama_consult_panel_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::consult_panel_list())
}

#[tauri::command]
pub fn mom_llama_consult_panel_create(
    name: String,
    personas: Vec<ConsultPersona>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::consult_panel_create(name, personas))
}

#[tauri::command]
pub async fn mom_llama_consult_start(
    window: Window,
    conversation: String,
    prompt: String,
    panel: Option<String>,
) -> Result<Value, String> {
    let events = window.clone();
    blocking_command(move || {
        mom_llama_runtime::consult_start_stream(
            ConsultStartInput {
                conversation_id: conversation,
                prompt,
                panel_id: panel,
            },
            ConsultStartOptions::default(),
            Some(move |event| {
                events
                    .emit("mom_llama_consult_stream", &event)
                    .map_err(anyhow::Error::new)?;
                Ok(())
            }),
        )
    })
    .await
}

#[tauri::command]
pub fn mom_llama_consult_status(run: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::consult_status(&run))
}

#[tauri::command]
pub fn mom_llama_consult_cancel(run: String, seat: Option<String>) -> Result<Value, String> {
    command_value(mom_llama_runtime::consult_cancel(&run, seat.as_deref()))
}

#[tauri::command]
pub async fn mom_llama_consult_synthesize(
    run: String,
    seats: Vec<String>,
) -> Result<Value, String> {
    blocking_command(move || mom_llama_runtime::consult_synthesize(&run, seats)).await
}

#[tauri::command]
pub fn mom_llama_conversation_new(title: Option<String>) -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_new(title))
}

#[tauri::command]
pub fn mom_llama_conversation_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_list())
}

#[tauri::command]
pub fn mom_llama_conversation_select(conversation: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_select(&conversation))
}

#[tauri::command]
pub fn mom_llama_conversation_search(query: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_search(&query))
}

#[tauri::command]
pub fn mom_llama_conversation_rename(conversation: String, title: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_rename(&conversation, title))
}

#[tauri::command]
pub fn mom_llama_conversation_system_message_update(
    conversation: String,
    system_message: Option<String>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_system_message_update(
        &conversation,
        system_message,
    ))
}

#[tauri::command]
pub fn mom_llama_conversation_delete(conversation: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_delete(&conversation))
}

#[tauri::command]
pub fn mom_llama_conversation_fork(conversation: String, message: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_fork(
        &conversation,
        &message,
    ))
}

#[tauri::command]
pub fn mom_llama_conversation_siblings(conversation: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_siblings(&conversation))
}

#[tauri::command]
pub fn mom_llama_draft_get(conversation: Option<String>) -> Result<Value, String> {
    command_value(mom_llama_runtime::draft_get(conversation.as_deref()))
}

#[tauri::command]
pub fn mom_llama_draft_update(
    conversation: Option<String>,
    message: String,
    attachment_ids: Option<Vec<String>>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::draft_update(
        conversation.as_deref(),
        message,
        attachment_ids.unwrap_or_default(),
    ))
}

#[tauri::command]
pub fn mom_llama_draft_clear(conversation: Option<String>) -> Result<Value, String> {
    command_value(mom_llama_runtime::draft_clear(conversation.as_deref()))
}

#[tauri::command]
pub fn mom_llama_conversation_export(
    conversation: String,
    format: Option<String>,
) -> Result<Value, String> {
    let format = match format.as_deref() {
        Some("markdown") => ConversationExportFormat::Markdown,
        _ => ConversationExportFormat::Json,
    };
    command_value(mom_llama_runtime::conversation_export(
        &conversation,
        format,
    ))
}

#[tauri::command]
pub fn mom_llama_conversation_import(path: String) -> Result<Value, String> {
    let content = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read conversation import {path}: {error}"))?;
    command_value(mom_llama_runtime::conversation_import_json(&content))
}

#[tauri::command]
pub fn mom_llama_message_edit(
    conversation: String,
    message: String,
    content: String,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::message_edit(
        &conversation,
        &message,
        content,
    ))
}

#[tauri::command]
pub fn mom_llama_message_delete(conversation: String, message: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::message_delete(&conversation, &message))
}

#[tauri::command]
pub fn mom_llama_message_copy(conversation: String, message: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::message_copy(&conversation, &message))
}

#[tauri::command]
pub fn mom_llama_message_branches(conversation: String, message: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::message_branches(&conversation, &message))
}

#[tauri::command]
pub fn mom_llama_message_branch_select(
    conversation: String,
    message: String,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::message_branch_select(
        &conversation,
        &message,
    ))
}

#[tauri::command]
pub fn mom_llama_attachment_import_text(
    conversation: String,
    path: String,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::text_attachment_import(
        &conversation,
        &PathBuf::from(path),
    ))
}

#[tauri::command]
pub fn mom_llama_attachment_import_paste(
    conversation: String,
    text: String,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::attachment_import_pasted_text(
        &conversation,
        text,
    ))
}

#[tauri::command]
pub fn mom_llama_attachment_import(conversation: String, path: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::attachment_import(
        &conversation,
        &PathBuf::from(path),
    ))
}

#[tauri::command]
pub fn mom_llama_attachment_list(conversation: Option<String>) -> Result<Value, String> {
    command_value(mom_llama_runtime::attachment_list(conversation.as_deref()))
}

#[tauri::command]
pub fn mom_llama_attachment_preview(attachment: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::attachment_preview(&attachment, true))
}

#[tauri::command]
pub fn mom_llama_settings_get() -> Result<Value, String> {
    command_value(mom_llama_runtime::settings_get())
}

#[tauri::command]
pub fn mom_llama_settings_reset() -> Result<Value, String> {
    let value = command_value(mom_llama_runtime::settings_reset())?;
    crate::refresh_gateway_native_model()?;
    Ok(value)
}

#[tauri::command]
pub fn mom_llama_settings_update(input: SettingsUpdateInput) -> Result<Value, String> {
    let value = command_value(mom_llama_runtime::settings_update(SettingsUpdate {
        model_path: input.model_path.map(PathBuf::from),
        mmproj_path: input.mmproj_path.map(PathBuf::from),
        native_device: input.device.as_deref().map(native_device_from_str),
        context_tokens: input.context_tokens,
        batch_tokens: input.batch_tokens,
        max_parallel_sequences: input.max_parallel_sequences,
        resident_memory_budget_bytes: input.memory_budget_mib.map(mib_to_bytes),
        temperature: input.temperature,
        top_p: input.top_p,
        max_tokens: input.max_tokens,
        kv_cache_policy: input.kv_cache_policy.as_deref().map(kv_policy_from_str),
        upstream_settings: input.upstream_settings.and_then(value_to_settings_map),
    }))?;
    crate::refresh_gateway_native_model()?;
    Ok(value)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdateInput {
    model_path: Option<String>,
    mmproj_path: Option<String>,
    device: Option<String>,
    context_tokens: Option<u32>,
    batch_tokens: Option<u32>,
    max_parallel_sequences: Option<u32>,
    memory_budget_mib: Option<u64>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    max_tokens: Option<u32>,
    kv_cache_policy: Option<String>,
    upstream_settings: Option<Value>,
}

#[tauri::command]
pub fn mom_llama_skill_create(
    name: String,
    description: Option<String>,
    prompt_template: String,
    usage_hint: Option<String>,
    cache_policy: Option<String>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::skill_store::skill_create(
        name,
        description.unwrap_or_default(),
        prompt_template,
        usage_hint.unwrap_or_else(|| "Use this prompt before the next answer.".to_string()),
        cache_policy
            .as_deref()
            .map(kv_policy_from_str)
            .unwrap_or(KvCachePolicy::None),
    ))
}

#[tauri::command]
pub fn mom_llama_skill_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::skill_store::skill_list())
}

#[tauri::command]
pub fn mom_llama_skill_update(
    skill: String,
    name: String,
    description: Option<String>,
    prompt_template: String,
    usage_hint: Option<String>,
    cache_policy: Option<String>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::skill_store::skill_update(
        &skill,
        name,
        description.unwrap_or_default(),
        prompt_template,
        usage_hint.unwrap_or_else(|| "Use this prompt before the next answer.".to_string()),
        cache_policy
            .as_deref()
            .map(kv_policy_from_str)
            .unwrap_or(KvCachePolicy::None),
    ))
}

#[tauri::command]
pub fn mom_llama_skill_apply(conversation: String, skill: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::skill_store::skill_apply(
        &conversation,
        &skill,
    ))
}

#[tauri::command]
pub fn mom_llama_kv_cache_status() -> Result<Value, String> {
    command_value(mom_llama_runtime::kv_cache_status())
}

#[tauri::command]
pub fn mom_llama_kv_cache_save(skill: Option<String>) -> Result<Value, String> {
    command_value(mom_llama_runtime::kv_cache_save(skill))
}

#[tauri::command]
pub fn mom_llama_kv_cache_restore(cache: Option<String>) -> Result<Value, String> {
    command_value(mom_llama_runtime::kv_cache_restore(cache))
}

#[tauri::command]
pub fn mom_llama_kv_cache_clear() -> Result<Value, String> {
    command_value(mom_llama_runtime::kv_cache_clear())
}

#[tauri::command]
pub fn mom_llama_mcp_status() -> Result<Value, String> {
    command_value(mom_llama_runtime::mcp_status())
}

#[tauri::command]
pub fn mom_llama_mcp_configure(
    name: String,
    command: String,
    args: Vec<String>,
    enabled: Option<bool>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::mcp_configure(
        name,
        PathBuf::from(command),
        args,
        enabled.unwrap_or(true),
    ))
}

#[tauri::command]
pub fn mom_llama_mcp_list_servers() -> Result<Value, String> {
    command_value(mom_llama_runtime::mcp_list_servers())
}

#[tauri::command]
pub fn mom_llama_mcp_list_tools(server: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::mcp_list_tools(&server))
}

#[tauri::command]
pub fn mom_llama_mcp_call_tool(
    server: String,
    tool: String,
    arguments: Value,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::mcp_call_tool(&server, &tool, arguments))
}

#[tauri::command]
pub fn mom_llama_mcp_list_resources(server: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::mcp_list_resources(&server))
}

#[tauri::command]
pub fn mom_llama_mcp_read_resource(server: String, uri: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::mcp_read_resource(&server, &uri))
}

#[tauri::command]
pub fn mom_llama_mcp_list_prompts(server: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::mcp_list_prompts(&server))
}

#[tauri::command]
pub fn mom_llama_mcp_get_prompt(
    server: String,
    prompt: String,
    arguments: Value,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::mcp_get_prompt(
        &server, &prompt, arguments,
    ))
}

#[tauri::command]
pub fn mom_llama_tool_loop_prepare(
    conversation: String,
    prompt: String,
    server: String,
    tool: String,
    arguments: Value,
    max_turns: Option<u32>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::tool_loop_prepare(
        &conversation,
        prompt,
        server,
        tool,
        arguments,
        max_turns.unwrap_or(4),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolLoopCommandInput {
    conversation: String,
    prompt: String,
    server: String,
    tool: String,
    arguments: Value,
    max_turns: Option<u32>,
    approval_id: Option<String>,
}

#[tauri::command]
pub async fn mom_llama_tool_loop_run(
    window: Window,
    input: ToolLoopCommandInput,
) -> Result<Value, String> {
    let events = window.clone();
    blocking_command(move || {
        mom_llama_runtime::tool_loop_run_stream(
            mom_llama_runtime::ToolLoopRunInput {
                conversation_id: input.conversation,
                prompt: input.prompt,
                server: input.server,
                tool: input.tool,
                arguments: input.arguments,
                max_turns: input.max_turns.unwrap_or(4),
                approval_id: input.approval_id,
            },
            move |event| {
                events
                    .emit("mom_llama_tool_loop_stream", &event)
                    .map_err(anyhow::Error::new)?;
                Ok(())
            },
        )
    })
    .await
}

#[tauri::command]
pub fn mom_llama_tool_loop_cancel(conversation: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::tool_loop_cancel(&conversation))
}

#[tauri::command]
pub fn mom_llama_tool_loop_status(conversation: Option<String>) -> Result<Value, String> {
    command_value(mom_llama_runtime::tool_loop_status(conversation.as_deref()))
}

#[tauri::command]
pub fn mom_llama_tool_permission_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::tool_permission_list())
}

#[tauri::command]
pub fn mom_llama_tool_permission_set(
    server: String,
    tool: String,
    policy: String,
) -> Result<Value, String> {
    let policy = match policy.as_str() {
        "always_allow" => mom_llama_runtime::ToolPermissionPolicy::AlwaysAllow,
        "deny" => mom_llama_runtime::ToolPermissionPolicy::Deny,
        _ => mom_llama_runtime::ToolPermissionPolicy::Ask,
    };
    command_value(mom_llama_runtime::tool_permission_set(server, tool, policy))
}

#[tauri::command]
pub fn mom_llama_tool_permission_revoke(server: String, tool: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::tool_permission_revoke(&server, &tool))
}

#[tauri::command]
pub fn mom_llama_server_configure(
    model_path: Option<String>,
    slots: Option<u32>,
    memory_budget_mib: Option<u64>,
) -> Result<Value, String> {
    command_value(mom_llama_runtime::server_configure(
        model_path.map(PathBuf::from),
        slots,
        memory_budget_mib.map(mib_to_bytes),
    ))
}

#[tauri::command]
pub fn mom_llama_server_status() -> Result<Value, String> {
    command_value(mom_llama_runtime::server_status())
}

#[tauri::command]
pub fn mom_llama_server_start() -> Result<Value, String> {
    command_value(mom_llama_runtime::server_start())
}

#[tauri::command]
pub fn mom_llama_server_stop() -> Result<Value, String> {
    command_value(mom_llama_runtime::server_stop())
}

#[tauri::command]
pub fn mom_llama_model_slot_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::model_slot_list())
}

#[tauri::command]
pub fn mom_llama_model_slot_load(slot: usize, model_path: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::model_slot_load(
        slot,
        PathBuf::from(model_path),
    ))
}

#[tauri::command]
pub fn mom_llama_model_slot_unload(slot: usize) -> Result<Value, String> {
    command_value(mom_llama_runtime::model_slot_unload(slot))
}

fn value_to_settings_map(value: Value) -> Option<std::collections::BTreeMap<String, Value>> {
    let Value::Object(object) = value else {
        return None;
    };
    Some(object.into_iter().collect())
}

fn native_device_from_str(value: &str) -> NativeDevice {
    match value {
        "cpu" => NativeDevice::Cpu,
        "metal" => NativeDevice::Metal,
        _ => NativeDevice::Auto,
    }
}

fn mib_to_bytes(value: u64) -> u64 {
    value.saturating_mul(1024 * 1024)
}

fn markup_response(result: anyhow::Result<String>) -> Result<Response, String> {
    result
        .map(|markup| Response::new(markup.into_bytes()))
        .map_err(to_error)
}

fn command_value<T: serde::Serialize>(result: anyhow::Result<T>) -> Result<Value, String> {
    let result = result.map_err(to_error)?;
    mom_llama_runtime::persist_command_receipt(&result).map_err(to_error)?;
    to_value(result).map_err(to_error)
}

async fn blocking_command<T, F>(operation: F) -> Result<Value, String>
where
    T: serde::Serialize + Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || command_value(operation()))
        .await
        .map_err(to_error)?
}

fn to_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn kv_policy_from_str(value: &str) -> KvCachePolicy {
    match value {
        "prompt_prefix" => KvCachePolicy::PromptPrefix,
        "kv_cache_candidate" => KvCachePolicy::KvCacheCandidate,
        _ => KvCachePolicy::None,
    }
}
