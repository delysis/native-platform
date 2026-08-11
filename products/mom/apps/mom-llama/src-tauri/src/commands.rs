use llama_native_types::NativeDevice;
use mom_llama_runtime::{
    ChatSendInput, ChatSendOptions, ConversationExportFormat, EngineCheckOptions, KvCachePolicy,
    PathSelection, PathSelectionKind, config::SettingsUpdate,
};
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use rfd::AsyncFileDialog;
use serde::Deserialize;
use serde_json::{Value, to_value};
use std::path::PathBuf;
use tauri::ipc::Response;
use tauri::{Emitter, State, Window};

use crate::app_runtime::{AppRuntimeHandle, AppWorkLease};
use crate::command_registry::command_spec;

const MAX_ATTACHMENT_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;

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
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub async fn mom_llama_pick_file(
    runtime: State<'_, AppRuntimeHandle>,
    kind: String,
) -> Result<Value, String> {
    let Some(path_kind) = PathSelectionKind::parse(&kind) else {
        return picker_blocked(
            "path_selection_kind_invalid",
            format!("Unsupported native file picker kind: {kind}"),
        );
    };
    let lease = runtime.admit(command_spec("mom_llama_pick_file"))?;
    let dialog = match kind.as_str() {
        "model" => {
            let dialog = AsyncFileDialog::new().add_filter("GGUF model", &["gguf"]);
            match mom_llama_runtime::hugging_face_hub_cache_dir() {
                Some(cache) if cache.is_dir() => dialog.set_directory(cache),
                _ => dialog,
            }
        }
        "mmproj" => AsyncFileDialog::new().add_filter("GGUF projector", &["gguf"]),
        "conversation" => AsyncFileDialog::new().add_filter("Conversation", &["json"]),
        "attachment" => AsyncFileDialog::new().add_filter(
            "Documents and media",
            &[
                "txt", "md", "markdown", "rst", "rtf", "tex", "csv", "tsv", "json", "jsonl",
                "yaml", "yml", "toml", "ini", "cfg", "xml", "html", "htm", "css", "svg", "vtt",
                "srt", "ipynb", "log", "sql", "c", "h", "cpp", "hpp", "rs", "py", "js", "jsx",
                "ts", "tsx", "java", "go", "swift", "sh", "pdf", "doc", "docx", "xls", "xlsx",
                "ppt", "pptx", "odt", "ods", "odp", "pages", "numbers", "key", "epub", "eml",
                "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "avif", "wav", "mp3",
                "flac", "ogg", "opus", "m4a", "aac", "aif", "aiff", "caf", "mp4", "m4v", "mov",
                "webm", "mkv", "avi", "zip", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "zst",
                "7z",
            ],
        ),
        "mcp" => AsyncFileDialog::new(),
        _ => unreachable!("validated path selection kind"),
    };
    let path = tokio::select! {
        file = dialog.pick_file() => file.map(|file| file.path().to_path_buf()),
        () = lease.cancelled() => None,
    };
    command_value(mom_llama_runtime::path_select(path_kind, path))
}

#[tauri::command]
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub async fn mom_llama_pick_file(
    runtime: State<'_, AppRuntimeHandle>,
    _kind: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_pick_file"))?;
    picker_blocked(
        "native_file_picker_unsupported",
        "The native file picker is unavailable on this build. Enter or paste an absolute path instead."
            .to_string(),
    )
}

fn picker_blocked(code: &str, message: String) -> Result<Value, String> {
    command_value(Ok(
        mom_llama_runtime::CommandResult::<PathSelection>::blocked(
            "mom_llama.path_select",
            "stub_blocked",
            mom_llama_runtime::Blocker::new(
                code,
                message,
                vec!["Enter or paste an absolute path instead.".to_string()],
            ),
        ),
    ))
}

#[tauri::command]
pub async fn mom_llama_engine_check(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_engine_check"))?,
        move || mom_llama_runtime::engine_check(EngineCheckOptions::default()),
    )
    .await
}

#[tauri::command]
pub fn mom_llama_engine_configure(
    runtime: State<'_, AppRuntimeHandle>,
    model_path: String,
    device: Option<String>,
    context_tokens: Option<u32>,
    batch_tokens: Option<u32>,
    max_parallel_sequences: Option<u32>,
    memory_budget_mib: Option<u64>,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_engine_configure"))?;
    let value = command_value(mom_llama_runtime::configure_engine(
        PathBuf::from(model_path),
        device.as_deref().map(native_device_from_str),
        context_tokens,
        batch_tokens,
        max_parallel_sequences,
        memory_budget_mib.map(mib_to_bytes),
    ))?;
    runtime.refresh_native_model(&lease)?;
    Ok(value)
}

#[tauri::command]
pub fn mom_llama_model_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::model_list())
}

#[tauri::command]
pub fn mom_llama_model_select(
    runtime: State<'_, AppRuntimeHandle>,
    model_path: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_model_select"))?;
    let value = command_value(mom_llama_runtime::model_select(PathBuf::from(model_path)))?;
    runtime.refresh_native_model(&lease)?;
    Ok(value)
}

#[tauri::command]
pub async fn mom_llama_chat_send(
    runtime: State<'_, AppRuntimeHandle>,
    window: Window,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_chat_send"))?;
    let events = window.clone();
    blocking_command(lease, move || {
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
    runtime: State<'_, AppRuntimeHandle>,
    window: Window,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_chat_dispatch"))?;
    let events = window.clone();
    blocking_command(lease, move || {
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
    runtime: State<'_, AppRuntimeHandle>,
    window: Window,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_mention_dispatch"))?;
    let events = window.clone();
    blocking_command(lease, move || {
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
    runtime: State<'_, AppRuntimeHandle>,
    invocation: String,
    target: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_mention_cancel"))?;
    command_value(mom_llama_runtime::mention_cancel(
        &invocation,
        target.as_deref(),
    ))
}

#[tauri::command]
pub async fn mom_llama_mention_synthesize(
    runtime: State<'_, AppRuntimeHandle>,
    invocation: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_mention_synthesize"))?,
        move || mom_llama_runtime::mention_synthesize(&invocation),
    )
    .await
}

#[tauri::command]
pub fn mom_llama_persona_freeze(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    message: String,
    name: String,
    handle: String,
    history: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_freeze"))?;
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
pub async fn mom_llama_persona_update(
    runtime: State<'_, AppRuntimeHandle>,
    profile: mom_llama_runtime::PersonaUpdateInput,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_persona_update"))?,
        move || mom_llama_runtime::persona_update(profile),
    )
    .await
}

#[tauri::command]
pub fn mom_llama_persona_delete(
    runtime: State<'_, AppRuntimeHandle>,
    persona: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_delete"))?;
    command_value(mom_llama_runtime::persona_delete(&persona))
}

#[tauri::command]
pub fn mom_llama_persona_instantiate(
    runtime: State<'_, AppRuntimeHandle>,
    persona: String,
    title: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_instantiate"))?;
    command_value(mom_llama_runtime::persona_instantiate(&persona, title))
}

#[tauri::command]
pub fn mom_llama_persona_group_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::persona_group_list())
}

#[tauri::command]
pub fn mom_llama_persona_group_create(
    runtime: State<'_, AppRuntimeHandle>,
    name: String,
    handle: String,
    personas: Vec<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_group_create"))?;
    command_value(mom_llama_runtime::persona_group_create(
        name, handle, personas,
    ))
}

#[tauri::command]
pub fn mom_llama_persona_group_update(
    runtime: State<'_, AppRuntimeHandle>,
    group: String,
    name: String,
    handle: String,
    personas: Vec<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_group_update"))?;
    command_value(mom_llama_runtime::persona_group_update(
        group, name, handle, personas,
    ))
}

#[tauri::command]
pub fn mom_llama_persona_group_delete(
    runtime: State<'_, AppRuntimeHandle>,
    group: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_group_delete"))?;
    command_value(mom_llama_runtime::persona_group_delete(&group))
}

#[tauri::command]
pub fn mom_llama_chat_cancel(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_chat_cancel"))?;
    command_value(mom_llama_runtime::chat_cancel(&conversation))
}

#[tauri::command]
pub fn mom_llama_chat_skip_reasoning(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_chat_skip_reasoning"))?;
    command_value(mom_llama_runtime::chat_skip_reasoning(&conversation))
}

#[tauri::command]
pub async fn mom_llama_chat_regenerate(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_chat_regenerate"))?,
        move || mom_llama_runtime::chat_regenerate(&conversation, ChatSendOptions::default()),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_chat_continue(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_chat_continue"))?,
        move || mom_llama_runtime::chat_continue(&conversation, ChatSendOptions::default()),
    )
    .await
}

#[tauri::command]
pub fn mom_llama_conversation_new(
    runtime: State<'_, AppRuntimeHandle>,
    title: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_new"))?;
    command_value(mom_llama_runtime::conversation_new(title))
}

#[tauri::command]
pub fn mom_llama_conversation_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_list())
}

#[tauri::command]
pub fn mom_llama_conversation_select(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_select"))?;
    command_value(mom_llama_runtime::conversation_select(&conversation))
}

#[tauri::command]
pub fn mom_llama_conversation_search(query: String) -> Result<Value, String> {
    command_value(mom_llama_runtime::conversation_search(&query))
}

#[tauri::command]
pub fn mom_llama_conversation_rename(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    title: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_rename"))?;
    command_value(mom_llama_runtime::conversation_rename(&conversation, title))
}

#[tauri::command]
pub fn mom_llama_conversation_system_message_update(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    system_message: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_system_message_update"))?;
    command_value(mom_llama_runtime::conversation_system_message_update(
        &conversation,
        system_message,
    ))
}

#[tauri::command]
pub fn mom_llama_conversation_delete(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_delete"))?;
    command_value(mom_llama_runtime::conversation_delete(&conversation))
}

#[tauri::command]
pub fn mom_llama_conversation_fork(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_fork"))?;
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
    runtime: State<'_, AppRuntimeHandle>,
    conversation: Option<String>,
    message: String,
    attachment_ids: Option<Vec<String>>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_draft_update"))?;
    command_value(mom_llama_runtime::draft_update(
        conversation.as_deref(),
        message,
        attachment_ids.unwrap_or_default(),
    ))
}

#[tauri::command]
pub fn mom_llama_draft_clear(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_draft_clear"))?;
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
pub fn mom_llama_conversation_import(
    runtime: State<'_, AppRuntimeHandle>,
    path: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_import"))?;
    let content = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read conversation import {path}: {error}"))?;
    command_value(mom_llama_runtime::conversation_import_json(&content))
}

#[tauri::command]
pub fn mom_llama_message_edit(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    message: String,
    content: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_message_edit"))?;
    command_value(mom_llama_runtime::message_edit(
        &conversation,
        &message,
        content,
    ))
}

#[tauri::command]
pub fn mom_llama_message_delete(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_message_delete"))?;
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
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_message_branch_select"))?;
    command_value(mom_llama_runtime::message_branch_select(
        &conversation,
        &message,
    ))
}

#[tauri::command]
pub async fn mom_llama_attachment_import_text(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    path: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_attachment_import_text"))?,
        move || mom_llama_runtime::text_attachment_import(&conversation, &PathBuf::from(path)),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_attachment_import_paste(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    text: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_attachment_import_paste"))?,
        move || mom_llama_runtime::attachment_import_pasted_text(&conversation, text),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_attachment_import(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    path: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_attachment_import"))?,
        move || mom_llama_runtime::attachment_import(&conversation, &PathBuf::from(path)),
    )
    .await
}

#[tauri::command]
pub fn mom_llama_attachment_list(conversation: Option<String>) -> Result<Value, String> {
    command_value(mom_llama_runtime::attachment_list(conversation.as_deref()))
}

#[tauri::command]
pub async fn mom_llama_attachment_preview(
    runtime: State<'_, AppRuntimeHandle>,
    attachment: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_attachment_preview"))?,
        move || mom_llama_runtime::attachment_preview(&attachment, false),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_attachment_preview_bytes(
    runtime: State<'_, AppRuntimeHandle>,
    attachment: String,
) -> Result<Response, String> {
    blocking_response(
        runtime.admit(command_spec("mom_llama_attachment_preview_bytes"))?,
        move || attachment_preview_response(&attachment),
    )
    .await
}

fn attachment_preview_response(attachment: &str) -> Result<Response, String> {
    let preview = mom_llama_runtime::attachment_preview(attachment, false).map_err(to_error)?;
    let metadata = preview.result.ok_or_else(|| {
        preview
            .blocker
            .map(|blocker| format!("{}: {}", blocker.code, blocker.message))
            .unwrap_or_else(|| {
                "attachment_preview_unavailable: Preview metadata is unavailable.".to_string()
            })
    })?;
    ensure_attachment_preview_size(&metadata.attachment.file_name, metadata.attachment.bytes)?;

    let bytes = mom_llama_runtime::attachments::attachment_bytes(attachment)
        .map_err(to_error)?
        .ok_or_else(|| {
            "attachment_content_missing: The attachment metadata exists, but its content is unavailable."
                .to_string()
        })?;
    let loaded_bytes = u64::try_from(bytes.len()).map_err(|_| {
        "attachment_preview_too_large: Attachment size does not fit in u64.".to_string()
    })?;
    ensure_attachment_preview_size(&metadata.attachment.file_name, loaded_bytes)?;
    if loaded_bytes != metadata.attachment.bytes {
        return Err(format!(
            "attachment_content_size_mismatch: Attachment `{}` declares {} bytes but loaded {} bytes.",
            metadata.attachment.file_name, metadata.attachment.bytes, loaded_bytes
        ));
    }

    Ok(Response::new(bytes))
}

#[tauri::command]
pub fn mom_llama_settings_get() -> Result<Value, String> {
    command_value(mom_llama_runtime::settings_get())
}

#[tauri::command]
pub fn mom_llama_settings_reset(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_settings_reset"))?;
    let value = command_value(mom_llama_runtime::settings_reset())?;
    runtime.refresh_native_model(&lease)?;
    Ok(value)
}

#[tauri::command]
pub fn mom_llama_settings_update(
    runtime: State<'_, AppRuntimeHandle>,
    input: SettingsUpdateInput,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_settings_update"))?;
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
    runtime.refresh_native_model(&lease)?;
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
    runtime: State<'_, AppRuntimeHandle>,
    name: String,
    description: Option<String>,
    prompt_template: String,
    usage_hint: Option<String>,
    cache_policy: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_skill_create"))?;
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
    runtime: State<'_, AppRuntimeHandle>,
    skill: String,
    name: String,
    description: Option<String>,
    prompt_template: String,
    usage_hint: Option<String>,
    cache_policy: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_skill_update"))?;
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
pub fn mom_llama_skill_apply(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    skill: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_skill_apply"))?;
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
pub async fn mom_llama_kv_cache_save(
    runtime: State<'_, AppRuntimeHandle>,
    skill: Option<String>,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_kv_cache_save"))?,
        move || mom_llama_runtime::kv_cache_save(skill),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_kv_cache_restore(
    runtime: State<'_, AppRuntimeHandle>,
    cache: Option<String>,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_kv_cache_restore"))?,
        move || mom_llama_runtime::kv_cache_restore(cache),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_kv_cache_clear(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_kv_cache_clear"))?,
        mom_llama_runtime::kv_cache_clear,
    )
    .await
}

#[tauri::command]
pub fn mom_llama_mcp_status() -> Result<Value, String> {
    command_value(mom_llama_runtime::mcp_status())
}

#[tauri::command]
pub fn mom_llama_mcp_configure(
    runtime: State<'_, AppRuntimeHandle>,
    name: String,
    command: String,
    args: Vec<String>,
    enabled: Option<bool>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_mcp_configure"))?;
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
pub async fn mom_llama_mcp_list_tools(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_list_tools"))?,
        move || mom_llama_runtime::mcp_list_tools(&server),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_mcp_call_tool(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
    tool: String,
    arguments: Value,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_call_tool"))?,
        move || mom_llama_runtime::mcp_call_tool(&server, &tool, arguments),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_mcp_list_resources(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_list_resources"))?,
        move || mom_llama_runtime::mcp_list_resources(&server),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_mcp_read_resource(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
    uri: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_read_resource"))?,
        move || mom_llama_runtime::mcp_read_resource(&server, &uri),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_mcp_list_prompts(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_list_prompts"))?,
        move || mom_llama_runtime::mcp_list_prompts(&server),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_mcp_get_prompt(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
    prompt: String,
    arguments: Value,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_get_prompt"))?,
        move || mom_llama_runtime::mcp_get_prompt(&server, &prompt, arguments),
    )
    .await
}

#[tauri::command]
pub fn mom_llama_tool_loop_prepare(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    prompt: String,
    server: String,
    tool: String,
    arguments: Value,
    max_turns: Option<u32>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_tool_loop_prepare"))?;
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
    runtime: State<'_, AppRuntimeHandle>,
    window: Window,
    input: ToolLoopCommandInput,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_tool_loop_run"))?;
    let events = window.clone();
    blocking_command(lease, move || {
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
pub fn mom_llama_tool_loop_cancel(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_tool_loop_cancel"))?;
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
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
    tool: String,
    policy: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_tool_permission_set"))?;
    let policy = match policy.as_str() {
        "always_allow" => mom_llama_runtime::ToolPermissionPolicy::AlwaysAllow,
        "deny" => mom_llama_runtime::ToolPermissionPolicy::Deny,
        _ => mom_llama_runtime::ToolPermissionPolicy::Ask,
    };
    command_value(mom_llama_runtime::tool_permission_set(server, tool, policy))
}

#[tauri::command]
pub fn mom_llama_tool_permission_revoke(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
    tool: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_tool_permission_revoke"))?;
    command_value(mom_llama_runtime::tool_permission_revoke(&server, &tool))
}

#[tauri::command]
pub fn mom_llama_model_slot_list() -> Result<Value, String> {
    command_value(mom_llama_runtime::model_slot_list())
}

#[tauri::command]
pub async fn mom_llama_model_slot_load(
    runtime: State<'_, AppRuntimeHandle>,
    slot: usize,
    model_path: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_model_slot_load"))?,
        move || mom_llama_runtime::model_slot_load(slot, PathBuf::from(model_path)),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_model_slot_unload(
    runtime: State<'_, AppRuntimeHandle>,
    slot: usize,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_model_slot_unload"))?,
        move || mom_llama_runtime::model_slot_unload(slot),
    )
    .await
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

fn ensure_attachment_preview_size(file_name: &str, bytes: u64) -> Result<(), String> {
    if bytes <= MAX_ATTACHMENT_PREVIEW_BYTES {
        return Ok(());
    }
    Err(format!(
        "attachment_preview_too_large: Attachment `{file_name}` is {bytes} bytes; inline previews are limited to {MAX_ATTACHMENT_PREVIEW_BYTES} bytes."
    ))
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

async fn blocking_command<T, F>(lease: AppWorkLease, operation: F) -> Result<Value, String>
where
    T: serde::Serialize + Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        if lease.cancellation_requested() {
            return Err(
                "Mom Llama cancelled the operation during application shutdown".to_string(),
            );
        }
        let _lease = lease;
        command_value(operation())
    })
    .await
    .map_err(to_error)?
}

async fn blocking_response<F>(lease: AppWorkLease, operation: F) -> Result<Response, String>
where
    F: FnOnce() -> Result<Response, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        if lease.cancellation_requested() {
            return Err(
                "Mom Llama cancelled the operation during application shutdown".to_string(),
            );
        }
        let _lease = lease;
        operation()
    })
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

#[cfg(test)]
mod tests {
    use super::{MAX_ATTACHMENT_PREVIEW_BYTES, ensure_attachment_preview_size};

    fn command_body<'a>(source: &'a str, name: &str) -> &'a str {
        let declaration = format!("pub async fn {name}");
        let Some(start) = source.find(&declaration) else {
            panic!("missing async Tauri command {name}");
        };
        let rest = &source[start..];
        let end = rest.find("\n#[tauri::command]").unwrap_or(rest.len());
        &rest[..end]
    }

    #[test]
    fn attachment_preview_cap_is_checked_at_the_exact_boundary() {
        assert!(ensure_attachment_preview_size("within.png", MAX_ATTACHMENT_PREVIEW_BYTES).is_ok());
        let error = ensure_attachment_preview_size(
            "too-large.png",
            MAX_ATTACHMENT_PREVIEW_BYTES.saturating_add(1),
        )
        .expect_err("a preview over the hard byte ceiling must fail closed");
        assert!(error.starts_with("attachment_preview_too_large:"));
    }

    #[test]
    fn multi_second_tauri_commands_stay_off_the_async_dispatch_thread() {
        let source = include_str!("commands.rs");
        for name in [
            "mom_llama_attachment_import_text",
            "mom_llama_attachment_import_paste",
            "mom_llama_attachment_import",
            "mom_llama_attachment_preview",
            "mom_llama_persona_update",
            "mom_llama_kv_cache_save",
            "mom_llama_kv_cache_restore",
            "mom_llama_kv_cache_clear",
            "mom_llama_mcp_list_tools",
            "mom_llama_mcp_call_tool",
            "mom_llama_mcp_list_resources",
            "mom_llama_mcp_read_resource",
            "mom_llama_mcp_list_prompts",
            "mom_llama_mcp_get_prompt",
            "mom_llama_model_slot_load",
            "mom_llama_model_slot_unload",
        ] {
            assert!(
                command_body(source, name).contains("blocking_command("),
                "{name} must use the shared blocking command boundary"
            );
        }
        assert!(
            command_body(source, "mom_llama_attachment_preview_bytes")
                .contains("blocking_response("),
            "raw attachment previews must read and decrypt outside the async dispatch thread"
        );
    }
}
