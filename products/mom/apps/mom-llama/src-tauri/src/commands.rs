use llama_native_types::NativeDevice;
use mom_llama_runtime::{
    AttachmentPreviewAnchor, ChatDispatchOutput, ChatSendInput, ChatSendOptions,
    ComposerAutocompleteAnchor, ComposerAutocompleteInput, Conversation, ConversationExportFormat,
    ConversationPersonaIdentity, EngineCheckOptions, KvCachePolicy, PathSelection,
    PathSelectionKind, config::SettingsUpdate,
};
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use rfd::AsyncFileDialog;
use serde::Deserialize;
use serde_json::{Value, to_value};
use speech_native_types::{AudioChunk, PcmSampleFormat};
use std::any::Any;
use std::cell::Cell;
use std::fs;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::ipc::{Channel, InvokeResponseBody, Response};
use tauri::{Emitter, Manager, State, Window};

use crate::app_runtime::{AppRuntimeHandle, AppWorkLease};
use crate::command_registry::command_spec;
use crate::information::{
    AlexandriaRightsDecision, CitationAnchor, ManagedCitationAnchor, ManagedRemovalConfirmation,
    ManagedRemovalSelection,
};
use crate::native_voice_capture::{CaptureTarget, NativeVoiceCapture};

fn panic_payload_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic payload")
}

fn bind_precommit_voice_for_conversation(
    runtime: &AppRuntimeHandle,
    conversation: &Conversation,
) -> Result<bool, String> {
    runtime.speech().bind_precommit_voice(
        &conversation.id,
        &ConversationPersonaIdentity {
            recipient_ids: conversation.recipient_ids.clone(),
            source_conversation_id: conversation.source_conversation_id.clone(),
        },
    )
}

fn emit_voice_speculation_stage(window: &Window, speculation: &str, stage: &str, started: Instant) {
    emit_voice_speculation_stage_with_detail(window, speculation, stage, started, Value::Null);
}

fn emit_voice_speculation_stage_with_detail(
    window: &Window,
    speculation: &str,
    stage: &str,
    started: Instant,
    detail: Value,
) {
    let emitted_at_unix_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_micros().min(u128::from(u64::MAX)) as u64
        });
    let _ = window.emit(
        "mom_llama_voice_speculation_stage",
        serde_json::json!({
            "schema": "mom_llama.voice_speculation_stage.v1",
            "speculation": speculation,
            "stage": stage,
            "command_elapsed_us": started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
            "emitted_at_unix_us": emitted_at_unix_us,
            "detail": detail,
        }),
    );
}

#[tauri::command]
pub fn mom_llama_render_app(runtime: State<'_, AppRuntimeHandle>) -> Result<Response, String> {
    let scope = runtime.operation_scope();
    let _lease = runtime.admit(command_spec("mom_llama_render_app"))?;
    markup_response(crate::view::render_app(&scope))
}

#[tauri::command]
pub fn mom_llama_render_chat_fragment(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Response, String> {
    let scope = runtime.operation_scope();
    let _lease = runtime.admit(command_spec("mom_llama_render_chat_fragment"))?;
    markup_response(crate::view::render_chat_fragment(&scope))
}

#[tauri::command]
pub fn mom_llama_render_sidebar_fragment(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Response, String> {
    let _lease = runtime.admit(command_spec("mom_llama_render_sidebar_fragment"))?;
    markup_response(crate::view::render_sidebar_fragment())
}

#[tauri::command]
pub fn mom_llama_render_settings_fragment(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Response, String> {
    let scope = runtime.operation_scope();
    let _lease = runtime.admit(command_spec("mom_llama_render_settings_fragment"))?;
    markup_response(crate::view::render_settings_fragment(&scope))
}

#[tauri::command]
pub async fn mom_llama_voice_latency_record(
    runtime: State<'_, AppRuntimeHandle>,
    app: tauri::AppHandle,
    trace: String,
) -> Result<(), String> {
    let _lease = runtime.admit(command_spec("mom_llama_voice_latency_record"))?;
    let (id, stage_count, checkpoint) = validate_voice_latency_trace(&trace)?;
    let directory = app.path().app_log_dir().map_err(to_error)?;
    tauri::async_runtime::spawn_blocking(move || {
        fs::create_dir_all(&directory).map_err(to_error)?;
        let history = directory.join("voice-latency-history");
        fs::create_dir_all(&history).map_err(to_error)?;
        let snapshot = history.join(format!("{id}-{checkpoint:04}-{stage_count:02}.json"));
        let mut receipt = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(snapshot)
            .map_err(to_error)?;
        receipt.write_all(trace.as_bytes()).map_err(to_error)?;
        receipt.sync_all().map_err(to_error)?;
        fs::write(directory.join("voice-latency-latest.json"), &trace).map_err(to_error)
    })
    .await
    .map_err(to_error)?
}

const MAX_VOICE_LATENCY_TRACE_BYTES: usize = 64 * 1024;
const MAX_VOICE_LATENCY_STAGES: usize = 512;

fn validate_voice_latency_trace(trace: &str) -> Result<(String, usize, u64), String> {
    if trace.len() > MAX_VOICE_LATENCY_TRACE_BYTES {
        return Err("voice latency trace exceeds its bounded size".to_string());
    }
    let value: Value = serde_json::from_str(trace).map_err(to_error)?;
    let stages = value
        .get("stages")
        .and_then(Value::as_array)
        .ok_or_else(|| "voice latency trace is missing stages".to_string())?;
    if stages.len() > MAX_VOICE_LATENCY_STAGES
        || stages.iter().any(|stage| {
            stage
                .get("stage")
                .and_then(Value::as_str)
                .is_none_or(|name| name.len() > 64)
        })
    {
        return Err("voice latency trace has invalid stages".to_string());
    }
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 64
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .ok_or_else(|| "voice latency trace has an invalid id".to_string())?
        .to_owned();
    let stage_count = stages.len();
    let checkpoint = value
        .get("checkpoint")
        .and_then(Value::as_u64)
        .unwrap_or(stage_count as u64);
    if checkpoint > 10_000 {
        return Err("voice latency trace has an invalid checkpoint".to_string());
    }
    Ok((id, stage_count, checkpoint))
}

#[tauri::command]
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub async fn mom_llama_pick_file(
    runtime: State<'_, AppRuntimeHandle>,
    kind: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_pick_file"))?;
    let Some(path_kind) = PathSelectionKind::parse(&kind) else {
        return picker_blocked(
            "path_selection_kind_invalid",
            format!("Unsupported native file picker kind: {kind}"),
        );
    };
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
    let terminal = if lease.cancellation_requested() {
        crate::operation_supervisor::TerminalClass::Cancelled
    } else {
        crate::operation_supervisor::TerminalClass::Completed
    };
    lease.finish(terminal)?;
    command_value(mom_llama_runtime::path_select(path_kind, path))
}

#[tauri::command]
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub async fn mom_llama_information_pick_alexandria(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_information_pick_alexandria"))?;
    let information = runtime.information();
    let path = tokio::select! {
        file = AsyncFileDialog::new()
            .add_filter("Alexandria SQLite database", &["db", "sqlite", "sqlite3"])
            .pick_file() => file.map(|file| file.path().to_path_buf()),
        () = lease.cancelled() => None,
    };
    let terminal = if lease.cancellation_requested() {
        crate::operation_supervisor::TerminalClass::Cancelled
    } else {
        crate::operation_supervisor::TerminalClass::Completed
    };
    lease.finish(terminal)?;
    let path = path.ok_or_else(|| "No Alexandria database was selected.".to_string())?;
    let grant = information.issue_alexandria_path_grant(path)?;
    to_value(grant).map_err(to_error)
}

#[tauri::command]
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub async fn mom_llama_information_pick_alexandria(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_information_pick_alexandria"))?;
    lease.finish(crate::operation_supervisor::TerminalClass::Failed)?;
    Err("The native Alexandria picker is unavailable on this platform.".to_string())
}

#[tauri::command]
pub async fn mom_llama_information_register_alexandria(
    runtime: State<'_, AppRuntimeHandle>,
    grant_id: String,
    decision: AlexandriaRightsDecision,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_information_register_alexandria"))?;
    let information = runtime.information();
    blocking_value(lease, move || {
        information.register_alexandria(&grant_id, decision)
    })
    .await
}

#[tauri::command]
pub fn mom_llama_information_libraries(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_information_libraries"))?;
    to_value(runtime.information().libraries()?).map_err(to_error)
}

#[tauri::command]
pub async fn mom_llama_information_search(
    runtime: State<'_, AppRuntimeHandle>,
    installation_id: String,
    text: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_information_search"))?;
    let information = runtime.information();
    blocking_value(lease, move || {
        information.search_local(&installation_id, &text)
    })
    .await
}

#[tauri::command]
pub async fn mom_llama_information_open_citation(
    runtime: State<'_, AppRuntimeHandle>,
    citation: CitationAnchor,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_information_open_citation"))?;
    let information = runtime.information();
    blocking_value(lease, move || information.open_citation(&citation)).await
}

#[tauri::command]
pub fn mom_llama_information_grant_model_context(
    runtime: State<'_, AppRuntimeHandle>,
    conversation_id: String,
    installation_id: String,
    confirmed: bool,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_information_grant_model_context"))?;
    to_value(runtime.information().grant_model_context(
        &conversation_id,
        &installation_id,
        confirmed,
    )?)
    .map_err(to_error)
}

#[tauri::command]
pub async fn mom_llama_information_chat_send(
    runtime: State<'_, AppRuntimeHandle>,
    window: Window,
    conversation_id: String,
    grant_id: String,
    query: String,
    message: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_information_chat_send"))?;
    let information = runtime.information();
    let operations = runtime.operation_scope();
    let events = window.clone();
    blocking_command(lease, move || {
        let message = information
            .model_context_message(&conversation_id, &grant_id, &query, &message)
            .map_err(anyhow::Error::msg)?;
        mom_llama_runtime::chat_send_stream_in_scope(
            &operations,
            ChatSendInput {
                conversation_id,
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
pub async fn mom_llama_attachment_library_preview(
    runtime: State<'_, AppRuntimeHandle>,
    anchor: AttachmentPreviewAnchor,
    title: String,
    confirmed_private_use: bool,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_attachment_library_preview"))?;
    let information = runtime.information();
    blocking_value(lease, move || {
        information.preview_attachment(anchor, title, confirmed_private_use)
    })
    .await
}

#[tauri::command]
pub async fn mom_llama_attachment_library_commit(
    runtime: State<'_, AppRuntimeHandle>,
    preview_id: String,
    impact_sha256: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_attachment_library_commit"))?;
    let information = runtime.information();
    blocking_value(lease, move || {
        information.commit_attachment(&preview_id, &impact_sha256)
    })
    .await
}

#[tauri::command]
pub fn mom_llama_information_managed_attachments(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_information_managed_attachments"))?;
    to_value(runtime.information().managed_attachments()?).map_err(to_error)
}

#[tauri::command]
pub fn mom_llama_information_managed_removals(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_information_managed_removals"))?;
    to_value(runtime.information().managed_removals()?).map_err(to_error)
}

#[tauri::command]
pub async fn mom_llama_information_search_managed_attachment(
    runtime: State<'_, AppRuntimeHandle>,
    materialization_id: String,
    text: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec(
        "mom_llama_information_search_managed_attachment",
    ))?;
    let information = runtime.information();
    blocking_value(lease, move || {
        information.search_managed_attachment(&materialization_id, &text)
    })
    .await
}

#[tauri::command]
pub async fn mom_llama_information_open_managed_citation(
    runtime: State<'_, AppRuntimeHandle>,
    citation: ManagedCitationAnchor,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_information_open_managed_citation"))?;
    let information = runtime.information();
    blocking_value(lease, move || information.open_managed_citation(&citation)).await
}

#[tauri::command]
pub async fn mom_llama_information_managed_removal_preview(
    runtime: State<'_, AppRuntimeHandle>,
    selection: ManagedRemovalSelection,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec(
        "mom_llama_information_managed_removal_preview",
    ))?;
    let information = runtime.information();
    blocking_value(lease, move || {
        information.preview_managed_removal(selection)
    })
    .await
}

#[tauri::command]
pub async fn mom_llama_information_managed_removal_commit(
    runtime: State<'_, AppRuntimeHandle>,
    confirmation: ManagedRemovalConfirmation,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_information_managed_removal_commit"))?;
    let information = runtime.information();
    blocking_value(lease, move || {
        information.commit_managed_removal(&confirmation)
    })
    .await
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
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_engine_check"))?,
        move || mom_llama_runtime::engine_check(&scope, EngineCheckOptions::default()),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_runtime_warm(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    let lease = runtime.admit(command_spec("mom_llama_runtime_warm"))?;
    let cancellation = lease.cancellation_control();
    blocking_command(lease, move || {
        mom_llama_runtime::runtime_warm_until_cancelled(&scope, cancellation.as_deref())
    })
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
    let _lease = runtime.admit(command_spec("mom_llama_engine_configure"))?;
    command_value(mom_llama_runtime::configure_engine(
        PathBuf::from(model_path),
        device.as_deref().map(native_device_from_str),
        context_tokens,
        batch_tokens,
        max_parallel_sequences,
        memory_budget_mib.map(mib_to_bytes),
    ))
}

#[tauri::command]
pub fn mom_llama_model_list(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    let _lease = runtime.admit(command_spec("mom_llama_model_list"))?;
    command_value(mom_llama_runtime::model_list(&scope))
}

#[tauri::command]
pub async fn mom_llama_model_select(
    runtime: State<'_, AppRuntimeHandle>,
    model_path: String,
    conversation: Option<String>,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    let conversation = conversation.filter(|value| !value.trim().is_empty());
    let lease = runtime.admit(command_spec("mom_llama_model_select"))?;
    let default_intent = conversation
        .is_none()
        .then(mom_llama_runtime::begin_model_selection)
        .transpose()
        .map_err(|error| error.to_string())?;
    blocking_command(lease, move || match conversation.as_deref() {
        Some(conversation) => mom_llama_runtime::conversation_model_select_and_load(
            &scope,
            conversation,
            PathBuf::from(model_path),
        ),
        None => mom_llama_runtime::model_select_with_intent(
            &scope,
            PathBuf::from(model_path),
            default_intent.expect("default model selection always has an intent"),
        ),
    })
    .await
}

#[tauri::command]
pub async fn mom_llama_chat_send(
    runtime: State<'_, AppRuntimeHandle>,
    window: Window,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_chat_send"))?;
    let operations = runtime.operation_scope();
    let events = window.clone();
    blocking_command(lease, move || {
        mom_llama_runtime::chat_send_stream_in_scope(
            &operations,
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
#[allow(clippy::too_many_arguments)]
pub async fn mom_llama_composer_autocomplete(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    draft: String,
    active_leaf_message_id: Option<String>,
    execution_profile_version: u64,
    selection_start_utf16: u32,
    selection_end_utf16: u32,
    attachment_ids: Option<Vec<String>>,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    let lease = runtime.admit(command_spec("mom_llama_composer_autocomplete"))?;
    let request_id = lease
        .native_request_id()
        .ok_or_else(|| "speculative autocomplete has no native request identity".to_string())?
        .to_string();
    let cancellation = lease
        .cancellation_control()
        .ok_or_else(|| "speculative autocomplete has no cancellation control".to_string())?;
    lease
        .run_blocking(move || {
            let result = mom_llama_runtime::composer_autocomplete_supervised(
                &scope,
                ComposerAutocompleteInput {
                    conversation_id: conversation,
                    draft,
                    active_leaf_message_id,
                    execution_profile_version,
                    selection_start_utf16,
                    selection_end_utf16,
                    attachment_ids: attachment_ids.unwrap_or_default(),
                },
                request_id,
                || cancellation.load(std::sync::atomic::Ordering::Acquire),
            )
            .map_err(to_error)?;
            to_value(result).map_err(to_error)
        })
        .await
}

#[tauri::command]
pub fn mom_llama_composer_autocomplete_cancel(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_composer_autocomplete_cancel"))?;
    let result = mom_llama_runtime::CommandResult::passed(
        "mom_llama.composer_autocomplete_cancel",
        "host_integrated",
        mom_llama_runtime::ComposerAutocompleteCancelOutput {
            cancellation_requested_for: runtime.cancel_speculative()?,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    );
    to_value(result).map_err(to_error)
}

#[tauri::command]
pub async fn mom_llama_composer_autocomplete_accept(
    runtime: State<'_, AppRuntimeHandle>,
    anchor: ComposerAutocompleteAnchor,
    suffix: String,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    let lease = runtime.admit(command_spec("mom_llama_composer_autocomplete_accept"))?;
    lease
        .run_blocking(move || {
            let result = mom_llama_runtime::composer_autocomplete_accept(&scope, anchor, suffix)
                .map_err(to_error)?;
            to_value(result).map_err(to_error)
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_chat_dispatch(
    runtime: State<'_, AppRuntimeHandle>,
    window: Window,
    conversation: String,
    message: String,
    client_request: Option<String>,
) -> Result<Value, String> {
    if client_request.as_ref().is_some_and(|request| {
        request.is_empty()
            || request.len() > 64
            || !request
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return Err("chat dispatch correlation must be a bounded ASCII identifier".into());
    }
    let stream_conversation = conversation.clone();
    let lease = runtime.admit(command_spec("mom_llama_chat_dispatch"))?;
    let runtime = runtime.inner().clone();
    let events = window.clone();
    let result = dispatch_typed(
        runtime,
        lease,
        mom_llama_runtime::MentionDispatchInput {
            conversation_id: conversation,
            message,
        },
        Some(move |event| {
            events
                .emit("mom_llama_chat_dispatch_stream", &event)
                .map_err(anyhow::Error::new)?;
            if let Some(request) = &client_request {
                events.emit("loom_mom_chat_dispatch_stream", serde_json::json!({
                    "client_request": request, "conversation_id": stream_conversation, "event": event,
                })).map_err(anyhow::Error::new)?;
            }
            Ok(())
        }),
    )
    .await?;
    command_value(Ok(result))
}

/// Preserve the structured command receipt until the caller chooses its
/// presentation. Both embedding and Tauri use the same supervised execution.
pub(crate) async fn dispatch_typed<F>(
    runtime: AppRuntimeHandle,
    lease: AppWorkLease,
    input: mom_llama_runtime::MentionDispatchInput,
    on_event: Option<F>,
) -> Result<mom_llama_runtime::CommandResult<ChatDispatchOutput>, String>
where
    F: FnMut(mom_llama_runtime::ChatDispatchStreamEvent) -> anyhow::Result<()> + Send + 'static,
{
    let operations = runtime.operation_scope();
    dispatch_typed_in_scope(runtime, lease, operations, input, None, on_event).await
}

pub(crate) async fn dispatch_typed_in_scope<F>(
    runtime: AppRuntimeHandle,
    lease: AppWorkLease,
    operations: mom_llama_runtime::OperationScope,
    input: mom_llama_runtime::MentionDispatchInput,
    context: Option<Vec<String>>,
    on_event: Option<F>,
) -> Result<mom_llama_runtime::CommandResult<ChatDispatchOutput>, String>
where
    F: FnMut(mom_llama_runtime::ChatDispatchStreamEvent) -> anyhow::Result<()> + Send + 'static,
{
    lease
        .run_blocking_with_cancellation_evidence(move || {
            if let Some(context) = context {
                mom_llama_runtime::prepare_document_chat_in_scope(
                    &operations,
                    &input.conversation_id,
                    input.message.clone(),
                    context,
                )
                .map_err(to_error)?;
            }
            let result = mom_llama_runtime::chat_dispatch_stream_in_scope(
                &operations,
                input,
                ChatSendOptions::default(),
                on_event,
            )
            .map_err(to_error)?;
            observe_dispatch_approvals(&runtime, &result).map_err(to_error)?;
            let cancelled = result.has_authoritative_cancellation_evidence();
            Ok((result, cancelled))
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_consult_sources(
    runtime: State<'_, AppRuntimeHandle>,
    invocation: String,
    target: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_consult_sources"))?;
    blocking_command(lease, move || {
        mom_llama_runtime::consult_sources(&invocation, &target)
    })
    .await
}

#[tauri::command]
pub async fn mom_llama_consult_source_open(
    runtime: State<'_, AppRuntimeHandle>,
    invocation: String,
    target: String,
    conversation: String,
    message: String,
    attachment: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_consult_source_open"))?;
    blocking_command(lease, move || {
        mom_llama_runtime::consult_source_open(
            &invocation,
            &target,
            &conversation,
            &message,
            &attachment,
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
    let operations = runtime.operation_scope();
    let runtime = runtime.inner().clone();
    let events = window.clone();
    blocking_command(lease, move || {
        let mut result = mom_llama_runtime::chat_dispatch_stream_in_scope(
            &operations,
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
        observe_dispatch_approvals(&runtime, &result)?;
        result.command = "mom_llama.mention_dispatch".to_string();
        result.receipt.command = "mom_llama.mention_dispatch".to_string();
        Ok(result)
    })
    .await
}

fn observe_dispatch_approvals(
    runtime: &AppRuntimeHandle,
    result: &mom_llama_runtime::CommandResult<ChatDispatchOutput>,
) -> anyhow::Result<()> {
    if let Some(ChatDispatchOutput::Mention { invocation, .. }) = result.result.as_ref()
        && !invocation.tool_approvals.is_empty()
    {
        runtime
            .observe_persona_tool_approval_invocation(&invocation.id)
            .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

#[tauri::command]
pub fn mom_llama_mention_candidates(
    runtime: State<'_, AppRuntimeHandle>,
    query: String,
    conversation: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_mention_candidates"))?;
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
    command_value(mom_llama_runtime::mention_cancel_in_scope(
        &runtime.operation_scope(),
        &invocation,
        target.as_deref(),
    ))
}

#[tauri::command]
pub async fn mom_llama_mention_synthesize(
    runtime: State<'_, AppRuntimeHandle>,
    invocation: String,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_mention_synthesize"))?,
        move || mom_llama_runtime::mention_synthesize(&scope, &invocation),
    )
    .await
}

#[tauri::command]
pub fn mom_llama_mention_tool_approval_list(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_mention_tool_approval_list"))?;
    command_value(mom_llama_runtime::mention_tool_approval_list(&conversation))
}

#[tauri::command]
pub async fn mom_llama_mention_tool_approval_decide(
    runtime: State<'_, AppRuntimeHandle>,
    invocation: String,
    approval: String,
    decision: mom_llama_runtime::MentionToolApprovalDecision,
) -> Result<Value, String> {
    let recovery = runtime.persona_tool_approval_recovery()?;
    let operations = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_mention_tool_approval_decide"))?,
        move || {
            mom_llama_runtime::mention_tool_approval_decide_with_recovery_in_scope(
                &operations,
                &invocation,
                &approval,
                decision,
                &recovery,
            )
        },
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
pub fn mom_llama_persona_list(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_list"))?;
    command_value(mom_llama_runtime::persona_list())
}

#[tauri::command]
pub fn mom_llama_persona_get(
    runtime: State<'_, AppRuntimeHandle>,
    persona: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_get"))?;
    command_value(mom_llama_runtime::persona_get(&persona))
}

#[tauri::command]
pub async fn mom_llama_persona_update(
    runtime: State<'_, AppRuntimeHandle>,
    profile: mom_llama_runtime::PersonaUpdateInput,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_persona_update"))?,
        move || mom_llama_runtime::persona_update(&scope, profile),
    )
    .await
}

#[tauri::command]
pub fn mom_llama_persona_removal_preview(
    runtime: State<'_, AppRuntimeHandle>,
    persona: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_removal_preview"))?;
    command_value(mom_llama_runtime::persona_removal_preview_in_scope(
        &runtime.operation_scope(),
        &persona,
    ))
}

#[tauri::command]
pub fn mom_llama_persona_remove_from_library(
    runtime: State<'_, AppRuntimeHandle>,
    input: mom_llama_runtime::PersonaRemovalCommitInput,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_remove_from_library"))?;
    command_value(mom_llama_runtime::persona_remove_from_library_in_scope(
        &runtime.operation_scope(),
        input,
    ))
}

#[tauri::command]
pub fn mom_llama_persona_instantiate(
    runtime: State<'_, AppRuntimeHandle>,
    persona: String,
    title: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_instantiate"))?;
    let result = mom_llama_runtime::persona_instantiate(&persona, title).map_err(to_error)?;
    if let Some(conversation) = result.result.as_ref() {
        bind_precommit_voice_for_conversation(runtime.inner(), conversation)?;
    }
    command_value(Ok(result))
}

#[tauri::command]
pub fn mom_llama_persona_group_list(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_persona_group_list"))?;
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
pub async fn mom_llama_chat_cancel(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_chat_cancel"))?;
    let operations = runtime.operation_scope();
    blocking_command(lease, move || {
        mom_llama_runtime::chat_cancel_in_scope(&operations, &conversation)
    })
    .await
}

#[tauri::command]
pub fn mom_llama_chat_skip_reasoning(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_chat_skip_reasoning"))?;
    command_value(mom_llama_runtime::chat_skip_reasoning_in_scope(
        &runtime.operation_scope(),
        &conversation,
    ))
}

#[tauri::command]
pub async fn mom_llama_chat_regenerate(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    blocking_command(runtime.admit(command_spec("mom_llama_chat_regenerate"))?, {
        let operations = runtime.operation_scope();
        move || {
            mom_llama_runtime::chat_regenerate_in_scope(
                &operations,
                &conversation,
                ChatSendOptions::default(),
            )
        }
    })
    .await
}

#[tauri::command]
pub async fn mom_llama_chat_continue(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    blocking_command(runtime.admit(command_spec("mom_llama_chat_continue"))?, {
        let operations = runtime.operation_scope();
        move || {
            mom_llama_runtime::chat_continue_in_scope(
                &operations,
                &conversation,
                ChatSendOptions::default(),
            )
        }
    })
    .await
}

#[tauri::command]
pub fn mom_llama_conversation_draft_open(
    runtime: State<'_, AppRuntimeHandle>,
    persona: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_draft_open"))?;
    command_value(mom_llama_runtime::conversation_draft_open(persona))
}

#[tauri::command]
pub fn mom_llama_conversation_draft_recipients_update(
    runtime: State<'_, AppRuntimeHandle>,
    recipient_ids: Vec<String>,
    name: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec(
        "mom_llama_conversation_draft_recipients_update",
    ))?;
    command_value(mom_llama_runtime::conversation_draft_recipients_update(
        recipient_ids,
        name,
    ))
}

#[tauri::command]
pub fn mom_llama_conversation_draft_submit(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_draft_submit"))?;
    command_value(mom_llama_runtime::conversation_draft_submit())
}

#[tauri::command]
pub fn mom_llama_conversation_draft_call_start(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_draft_call_start"))?;
    let result = mom_llama_runtime::conversation_draft_call_start().map_err(to_error)?;
    if let Some(conversation) = result.result.as_ref() {
        bind_precommit_voice_for_conversation(runtime.inner(), conversation)?;
    }
    command_value(Ok(result))
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
pub fn mom_llama_conversation_list(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_list"))?;
    let result = mom_llama_runtime::conversation_list().map_err(to_error)?;
    if let Some(conversations) = result.result.as_ref() {
        for conversation in conversations {
            bind_precommit_voice_for_conversation(runtime.inner(), conversation)?;
        }
    }
    command_value(Ok(result))
}

#[tauri::command]
pub fn mom_llama_conversation_select(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_select"))?;
    let result = mom_llama_runtime::conversation_select(&conversation).map_err(to_error)?;
    if let Some(conversation) = result.result.as_ref() {
        bind_precommit_voice_for_conversation(runtime.inner(), conversation)?;
    }
    command_value(Ok(result))
}

#[tauri::command]
pub fn mom_llama_conversation_search(
    runtime: State<'_, AppRuntimeHandle>,
    query: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_search"))?;
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
pub fn mom_llama_conversation_siblings(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_siblings"))?;
    command_value(mom_llama_runtime::conversation_siblings(&conversation))
}

#[tauri::command]
pub fn mom_llama_draft_get(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_draft_get"))?;
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

fn resolve_conversation_export_format(
    format: Option<&str>,
) -> Result<ConversationExportFormat, String> {
    match format {
        Some("document") => Ok(ConversationExportFormat::Document),
        Some("markdown") => Ok(ConversationExportFormat::Markdown),
        None | Some("json") => Ok(ConversationExportFormat::Json),
        Some(_) => Err("Unknown conversation export format.".to_owned()),
    }
}

#[tauri::command]
pub fn mom_llama_conversation_export(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    format: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_conversation_export"))?;
    let format = resolve_conversation_export_format(format.as_deref())?;
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
    command_value(mom_llama_runtime::conversation_import_file(
        PathBuf::from(path).as_path(),
    ))
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
pub fn mom_llama_message_record_call_transcript(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    message: String,
    transcript: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_message_record_call_transcript"))?;
    command_value(mom_llama_runtime::message_record_call_transcript(
        &conversation,
        &message,
        transcript,
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
pub fn mom_llama_message_copy(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_message_copy"))?;
    command_value(mom_llama_runtime::message_copy(&conversation, &message))
}

#[tauri::command]
pub async fn mom_llama_speech_read_aloud(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    message: String,
    operation: String,
    require_persona_voice: Option<bool>,
    pcm_stream: Channel<InvokeResponseBody>,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_speech_read_aloud"))?;
    let speech = runtime.speech();
    let result = speech
        .read_aloud_streaming_with_mode(
            &conversation,
            &message,
            &operation,
            require_persona_voice.unwrap_or(false),
            Some(Arc::new(move |chunk| {
                pcm_stream
                    .send(InvokeResponseBody::Raw(pcm_stream_frame(chunk)?))
                    .map_err(|error| format!("speech_stream_ipc_failed: {error}"))
            })),
        )
        .await
        .map_err(to_error)?;
    let terminal = speech_terminal(&result);
    let value = command_value(Ok(result));
    lease.finish(terminal)?;
    value
}

fn pcm_stream_frame(chunk: &AudioChunk) -> Result<Vec<u8>, String> {
    let sample_format = match chunk.format.sample_format {
        PcmSampleFormat::I16Le => 1,
        PcmSampleFormat::I24Le => 2,
        PcmSampleFormat::I32Le => 3,
        PcmSampleFormat::F32Le => 4,
    };
    let channels = u8::try_from(chunk.format.channels).map_err(|_| {
        "speech_stream_channels_invalid: channel count exceeded IPC framing".to_string()
    })?;
    let byte_len = u32::try_from(chunk.data.len()).map_err(|_| {
        "speech_stream_chunk_too_large: PCM packet exceeded IPC framing".to_string()
    })?;
    let mut frame = Vec::with_capacity(32 + chunk.data.len());
    frame.extend_from_slice(b"MPCM");
    frame.push(1);
    frame.push(u8::from(chunk.end_of_stream) | (u8::from(chunk.format.interleaved) << 1));
    frame.push(sample_format);
    frame.push(channels);
    frame.extend_from_slice(&chunk.format.sample_rate_hz.to_le_bytes());
    frame.extend_from_slice(&chunk.sequence.to_le_bytes());
    frame.extend_from_slice(&chunk.sample_offset.to_le_bytes());
    frame.extend_from_slice(&byte_len.to_le_bytes());
    frame.extend_from_slice(&chunk.data);
    Ok(frame)
}

#[tauri::command]
pub async fn mom_llama_speech_precommit_synthesize(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    operation: String,
    text: String,
    pcm_stream: Channel<InvokeResponseBody>,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_speech_precommit_synthesize"))?;
    let speech = runtime.speech();
    let result = speech
        .synthesize_precommit(
            &conversation,
            &operation,
            &text,
            Arc::new(move |chunk| {
                pcm_stream
                    .send(InvokeResponseBody::Raw(pcm_stream_frame(chunk)?))
                    .map_err(|error| format!("speech_stream_ipc_failed: {error}"))
            }),
        )
        .await
        .map_err(to_error)?;
    let terminal = speech_terminal(&result);
    let value = command_value(Ok(result));
    lease.finish(terminal)?;
    value
}

#[tauri::command]
pub fn mom_llama_voice_generation_backpressure(
    runtime: State<'_, AppRuntimeHandle>,
    request: String,
    active: bool,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_voice_generation_backpressure"))?;
    to_value(
        runtime
            .speech()
            .set_generation_backpressure(&request, active)?,
    )
    .map_err(to_error)
}

#[tauri::command]
pub fn mom_llama_message_branches(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_message_branches"))?;
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
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_attachment_import_text"))?,
        move || {
            mom_llama_runtime::text_attachment_import(&scope, &conversation, &PathBuf::from(path))
        },
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_attachment_import_paste(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    text: String,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_attachment_import_paste"))?,
        move || mom_llama_runtime::attachment_import_pasted_text(&scope, &conversation, text),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_attachment_import(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    path: String,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_attachment_import"))?,
        move || mom_llama_runtime::attachment_import(&scope, &conversation, &PathBuf::from(path)),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_voice_message_import(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    bytes: Vec<u8>,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_voice_message_import"))?,
        move || mom_llama_runtime::attachment_import_voice_message(&scope, &conversation, bytes),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_voice_audio_begin(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_audio_begin"))?;
    let operations = runtime.operation_scope();
    let owner = runtime.inner().clone();
    let speech = runtime.speech();
    lease
        .run_blocking(move || {
            let stage = Cell::new("prepare_incremental_generation");
            let result = catch_unwind(AssertUnwindSafe(|| {
                let session = mom_llama_runtime::voice_audio_begin_in_scope(
                    &operations,
                    &conversation,
                    &message,
                )
                .map_err(to_error)?;
                let request_id = session.request_id().to_string();
                let begin_metrics = session.begin_metrics().clone();
                stage.set("bind_consultant_voice");
                if !speech.bind_precommit_voice(&conversation, &session.persona_identity())? {
                    return Err(
                        "The prepared microphone turn is not bound to a consultant voice"
                            .to_string(),
                    );
                }
                stage.set("publish_voice_session");
                let session_id = owner.insert_voice_audio_session(session)?;
                Ok(serde_json::json!({
                    "session": session_id,
                    "request_id": request_id,
                    "conversation_id": conversation,
                    "begin_metrics": begin_metrics,
                }))
            }));
            match result {
                Ok(result) => result,
                Err(payload) => Err(format!(
                    "voice_audio_begin_panicked:{}: {}",
                    stage.get(),
                    panic_payload_message(payload.as_ref()),
                )),
            }
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_voice_capture_prepare(
    runtime: State<'_, AppRuntimeHandle>,
    capture_state: State<'_, NativeVoiceCapture>,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_capture_prepare"))?;
    let controller = capture_state.inner().clone();
    lease
        .run_blocking(move || {
            let metrics = controller.prepare().map_err(to_error)?;
            serde_json::to_value(metrics).map_err(to_error)
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_voice_capture_start(
    runtime: State<'_, AppRuntimeHandle>,
    capture_state: State<'_, NativeVoiceCapture>,
    window: Window,
    capture: String,
    call: bool,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_capture_start"))?;
    let controller = capture_state.inner().clone();
    lease
        .run_blocking(move || {
            let events = window.clone();
            controller
                .start(
                    capture.clone(),
                    None,
                    call,
                    Arc::new(move |event| {
                        let _ = events.emit("mom_llama_voice_capture_event", event);
                    }),
                )
                .map_err(to_error)?;
            Ok(serde_json::json!({
                "capture": capture,
                "native": true,
            }))
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_voice_capture_pause(
    runtime: State<'_, AppRuntimeHandle>,
    capture_state: State<'_, NativeVoiceCapture>,
    capture: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_capture_pause"))?;
    let controller = capture_state.inner().clone();
    lease
        .run_blocking(move || {
            controller.pause_prefill(capture).map_err(to_error)?;
            Ok(serde_json::json!({ "paused": true }))
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_voice_capture_retarget(
    runtime: State<'_, AppRuntimeHandle>,
    capture_state: State<'_, NativeVoiceCapture>,
    capture: String,
    session: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_capture_retarget"))?;
    let sink = runtime.voice_audio_session(&session)?;
    let cancellation = sink
        .cancellation_handle()
        .ok_or_else(|| "The live voice session has no cancellation authority".to_string())?;
    let target = CaptureTarget {
        id: session.clone(),
        sink,
        cancel: Arc::new(move || {
            cancellation.cancel();
        }),
    };
    let controller = capture_state.inner().clone();
    lease
        .run_blocking(move || {
            let started = Instant::now();
            let replay = controller.retarget(capture, target, 0).map_err(to_error)?;
            Ok(serde_json::json!({
                "session": session,
                "retargeted": true,
                "replay": replay,
                "native_elapsed_us": started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
            }))
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_voice_capture_stop(
    runtime: State<'_, AppRuntimeHandle>,
    capture_state: State<'_, NativeVoiceCapture>,
    window: Window,
    capture: String,
) -> Result<Response, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_capture_stop"))?;
    let controller = capture_state.inner().clone();
    lease
        .run_blocking(move || {
            let output = controller.stop(capture.clone()).map_err(to_error)?;
            let _ = window.emit(
                "mom_llama_voice_capture_metrics",
                serde_json::json!({
                    "capture": capture,
                    "heard_voice": output.heard_voice,
                    "wav_bytes": output.wav.len(),
                    "metrics": output.metrics,
                }),
            );
            Ok(Response::new(output.wav))
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_voice_capture_cancel(
    runtime: State<'_, AppRuntimeHandle>,
    capture_state: State<'_, NativeVoiceCapture>,
    window: Window,
    capture: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_capture_cancel"))?;
    let controller = capture_state.inner().clone();
    lease
        .run_blocking(move || {
            let metrics = controller.cancel(capture.clone()).map_err(to_error)?;
            let _ = window.emit(
                "mom_llama_voice_capture_metrics",
                serde_json::json!({
                    "capture": capture,
                    "cancelled": true,
                    "metrics": metrics,
                }),
            );
            Ok(serde_json::json!({ "cancelled": true }))
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_voice_audio_push(
    runtime: State<'_, AppRuntimeHandle>,
    session: String,
    samples: Vec<f32>,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_audio_push"))?;
    let owner = runtime.inner().clone();
    lease
        .run_blocking(move || {
            let buffered_samples = owner.push_voice_audio(&session, samples)?;
            Ok(serde_json::json!({
                "accepted": true,
                "buffered_samples": buffered_samples,
            }))
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_voice_audio_finish(
    runtime: State<'_, AppRuntimeHandle>,
    window: Window,
    session: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_audio_finish"))?;
    let owner = runtime.inner().clone();
    let voice = owner.take_voice_audio_session(&session)?;
    let archive = voice.audio_monitor();
    let conversation = voice.conversation_id().to_string();
    let operations = runtime.operation_scope();
    let import = std::thread::Builder::new()
        .name("mom-voice-attachment-import".to_string())
        .spawn(move || {
            let bytes = archive.wav_bytes().map_err(anyhow::Error::new)?;
            mom_llama_runtime::attachment_import_voice_message(&operations, &conversation, bytes)
        })
        .map_err(|error| format!("voice_attachment_import_spawn_failed: {error}"))?;
    let import = Arc::new(std::sync::Mutex::new(Some(import)));
    let precommit_import = Arc::clone(&import);
    let events = window.clone();
    lease
        .run_blocking_with_cancellation_evidence(move || {
            let result = mom_llama_runtime::voice_audio_finish_with_precommit_in_scope(
                voice,
                ChatSendOptions::default(),
                Some(move |event: mom_llama_runtime::ChatStreamEvent| {
                    events
                        .emit("mom_llama_chat_dispatch_stream", &event)
                        .map_err(anyhow::Error::new)
                }),
                move || {
                    let handle = precommit_import
                        .lock()
                        .map_err(|_| anyhow::anyhow!("voice attachment import lock poisoned"))?
                        .take()
                        .ok_or_else(|| anyhow::anyhow!("voice attachment import already joined"))?;
                    let imported = handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("voice attachment import panicked"))??;
                    let attachment = imported.result.ok_or_else(|| {
                        anyhow::anyhow!(
                            "{}",
                            imported
                                .blocker
                                .map(|blocker| blocker.message)
                                .unwrap_or_else(|| "voice attachment import failed".to_string())
                        )
                    })?;
                    Ok(Some(attachment.attachment.id))
                },
            )
            .map_err(to_error);
            // Generation errors and cancellation can return before the
            // pre-commit hook. Never detach an attachment writer from its turn.
            if let Some(handle) = import
                .lock()
                .map_err(|_| "voice_attachment_import_lock_poisoned".to_string())?
                .take()
            {
                let _ = handle.join();
            }
            let result = result?;
            let cancelled = result.has_authoritative_cancellation_evidence();
            Ok((to_value(result).map_err(to_error)?, cancelled))
        })
        .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "Tauri command arguments are bound by the invoke protocol"
)]
#[tauri::command]
pub async fn mom_llama_voice_audio_speculate(
    runtime: State<'_, AppRuntimeHandle>,
    capture_state: State<'_, NativeVoiceCapture>,
    window: Window,
    session: String,
    speculation: String,
    capture: Option<String>,
    wait_for_silence: Option<bool>,
    generation_timeout_ms: Option<u64>,
) -> Result<Value, String> {
    let command_started = Instant::now();
    emit_voice_speculation_stage(
        &window,
        &speculation,
        "native_command_entered",
        command_started,
    );
    let lease = runtime.admit(command_spec("mom_llama_voice_audio_speculate"))?;
    emit_voice_speculation_stage(
        &window,
        &speculation,
        "native_admission_complete",
        command_started,
    );
    let owner = runtime.inner().clone();
    // Register cancellation authority before touching capture. Resumed speech
    // can then belay this command at every point after native admission.
    let prepared = {
        let voice = owner.voice_audio_session(&session)?;
        voice.prepared_turn()
    };
    let control = owner.begin_voice_speculation_with_prepared(&speculation, Some(prepared))?;
    emit_voice_speculation_stage(
        &window,
        &speculation,
        "native_cancellation_registered",
        command_started,
    );
    let wait_for_silence = wait_for_silence.unwrap_or(false);
    // This timeout is consumed by the chat ticket only after incremental
    // audio has been sealed and generation begins. Time spent speaking while
    // the native silence gate is armed is therefore never charged to it.
    let generation_timeout_ms = generation_timeout_ms.unwrap_or(20_000).clamp(1_000, 30_000);
    let silence_gate = if wait_for_silence {
        let capture = capture
            .as_ref()
            .ok_or_else(|| "native silence speculation requires an active capture".to_string())?;
        let gate = match capture_state.inner().arm_silence(capture.clone()) {
            Ok(gate) => gate,
            Err(error) => {
                control.abort();
                owner.finish_voice_speculation(&speculation);
                return Err(to_error(error));
            }
        };
        emit_voice_speculation_stage(
            &window,
            &speculation,
            "native_silence_armed",
            command_started,
        );
        Some(gate)
    } else {
        None
    };
    // Silence is detected by the native capture owner after it has already
    // flushed the trailing partial PCM batch. Detach that exact producer in
    // this same command so the renderer does not pay a second IPC round trip.
    if !wait_for_silence
        && let Some(capture) = capture
        && let Err(error) = capture_state.inner().pause_prefill(capture)
    {
        control.abort();
        owner.finish_voice_speculation(&speculation);
        return Err(to_error(error));
    }
    let operations = runtime.operation_scope();
    let worker_owner = owner.clone();
    let worker_control = Arc::clone(&control);
    let events = window.clone();
    let worker_events = window.clone();
    let worker_speculation = speculation.clone();
    let result = lease
        .run_blocking_with_cancellation_evidence(move || {
            // This supervisor worker is admitted while the user is still
            // speaking. It sleeps on the capture owner's condition variable
            // and continues directly into generation when silence releases
            // it; there is no async-runtime bounce or second worker dispatch
            // on the latency-critical silence edge.
            emit_voice_speculation_stage(
                &worker_events,
                &worker_speculation,
                "native_generation_worker_prearmed",
                command_started,
            );
            if let Some(gate) = silence_gate {
                gate.wait()?;
                emit_voice_speculation_stage(
                    &worker_events,
                    &worker_speculation,
                    "native_silence_released",
                    command_started,
                );
            }
            emit_voice_speculation_stage(
                &worker_events,
                &worker_speculation,
                "native_capture_detached",
                command_started,
            );
            let voice = match worker_owner.take_voice_audio_session(&session) {
                Ok(voice) => voice,
                Err(error) => {
                    worker_control.abort();
                    worker_owner.finish_voice_speculation(&worker_speculation);
                    return Err(error);
                }
            };
            emit_voice_speculation_stage(
                &worker_events,
                &worker_speculation,
                "native_voice_session_acquired",
                command_started,
            );
            emit_voice_speculation_stage(
                &worker_events,
                &worker_speculation,
                "native_generation_worker_started",
                command_started,
            );
            let progress = voice.audio_monitor();
            let archive = progress.clone();
            let conversation = voice.conversation_id().to_string();
            let import_control = Arc::clone(&worker_control);
            let import_conversation = conversation.clone();
            let import_operations = operations.clone();
            let import = match std::thread::Builder::new()
                .name("mom-speculative-voice-import".to_string())
                .spawn(move || {
                    import_control
                        .wait_for_confirmation()
                        .map_err(anyhow::Error::msg)?;
                    let bytes = archive.wav_bytes().map_err(anyhow::Error::new)?;
                    mom_llama_runtime::attachment_import_voice_message(
                        &import_operations,
                        &import_conversation,
                        bytes,
                    )
                }) {
                Ok(import) => import,
                Err(error) => {
                    worker_control.abort();
                    worker_owner.finish_voice_speculation(&worker_speculation);
                    return Err(format!("voice_speculative_import_spawn_failed: {error}"));
                }
            };
            let import = Arc::new(std::sync::Mutex::new(Some(import)));
            let precommit_import = Arc::clone(&import);
            let failure_control = Arc::clone(&worker_control);
            emit_voice_speculation_stage(
                &worker_events,
                &worker_speculation,
                "native_audio_finish_entered",
                command_started,
            );
            let stream_speculation = worker_speculation.clone();
            let result = mom_llama_runtime::voice_audio_finish_with_precommit_in_scope(
                voice,
                ChatSendOptions {
                    timeout_s: generation_timeout_ms as f64 / 1_000.0,
                    ..ChatSendOptions::default()
                },
                Some(move |event: mom_llama_runtime::ChatStreamEvent| {
                    let detail = serde_json::json!({
                        "kind": "chat",
                        "event": &event.event,
                        "request_id": &event.request_id,
                        "conversation_id": &event.conversation_id,
                        "delta_bytes": event.delta.as_ref().map_or(0, String::len),
                    });
                    emit_voice_speculation_stage_with_detail(
                        &events,
                        &stream_speculation,
                        "native_chat_stream_event",
                        command_started,
                        detail,
                    );
                    events
                        .emit("mom_llama_chat_dispatch_stream", &event)
                        .map_err(anyhow::Error::new)
                }),
                move || {
                    let handle = precommit_import
                        .lock()
                        .map_err(|_| anyhow::anyhow!("voice attachment import lock poisoned"))?
                        .take()
                        .ok_or_else(|| anyhow::anyhow!("voice attachment import already joined"))?;
                    let imported = handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("voice attachment import panicked"))??;
                    let attachment = imported.result.ok_or_else(|| {
                        anyhow::anyhow!(
                            "{}",
                            imported
                                .blocker
                                .map(|blocker| blocker.message)
                                .unwrap_or_else(|| "voice attachment import failed".to_string())
                        )
                    })?;
                    Ok(Some(attachment.attachment.id))
                },
            )
            .map_err(to_error);
            let audio = progress.stats();
            let mut audio_detail = serde_json::Map::new();
            macro_rules! audio_metric {
                ($name:literal, $value:expr) => {
                    audio_detail.insert($name.to_string(), to_value($value).unwrap_or(Value::Null));
                };
            }
            audio_metric!("accepted_samples", audio.accepted_samples);
            audio_metric!("consumed_samples", audio.consumed_samples);
            audio_metric!("chunks", audio.chunks);
            audio_metric!("high_water_samples", audio.high_water_samples);
            audio_metric!("first_accept_us", audio.first_accept_us);
            audio_metric!("first_read_us", audio.first_read_us);
            audio_metric!("finish_us", audio.finish_us);
            audio_metric!("cancellation_requested_us", audio.cancellation_requested_us);
            audio_metric!("cancellation_observed_us", audio.cancellation_observed_us);
            audio_metric!("decode_start_us", audio.decode_start_us);
            audio_metric!("prefix_start_us", audio.prefix_start_us);
            audio_metric!("prefix_finish_us", audio.prefix_finish_us);
            audio_metric!("prefix_us", audio.prefix_us);
            audio_metric!("prefix_tokens", audio.prefix_tokens);
            audio_metric!("restored_prefix_tokens", audio.restored_prefix_tokens);
            audio_metric!("projector_us", audio.projector_us);
            audio_metric!("projected_blocks", audio.projected_blocks);
            audio_metric!("projected_samples", audio.projected_samples);
            audio_metric!("first_projector_start_us", audio.first_projector_start_us);
            audio_metric!("first_projector_finish_us", audio.first_projector_finish_us);
            audio_metric!("last_projector_finish_us", audio.last_projector_finish_us);
            audio_metric!("projector_max_us", audio.projector_max_us);
            audio_metric!("tail_samples", audio.tail_samples);
            audio_metric!("tail_projector_us", audio.tail_projector_us);
            audio_metric!(
                "closing_boundary_finish_us",
                audio.closing_boundary_finish_us
            );
            audio_metric!("suffix_tokenize_finish_us", audio.suffix_tokenize_finish_us);
            audio_metric!("suffix_eval_finish_us", audio.suffix_eval_finish_us);
            audio_metric!("sampler_start_us", audio.sampler_start_us);
            audio_metric!("sampler_ready_us", audio.sampler_ready_us);
            audio_metric!("first_sample_start_us", audio.first_sample_start_us);
            audio_metric!("first_sample_finish_us", audio.first_sample_finish_us);
            audio_metric!("first_token_event_us", audio.first_token_event_us);
            audio_metric!("sampled_tokens", audio.sampled_tokens);
            audio_metric!("nonempty_token_pieces", audio.nonempty_token_pieces);
            audio_metric!("emitted_token_events", audio.emitted_token_events);
            audio_metric!("last_sample_start_us", audio.last_sample_start_us);
            audio_metric!("last_sample_finish_us", audio.last_sample_finish_us);
            audio_metric!("last_token_event_us", audio.last_token_event_us);
            audio_metric!(
                "continuation_decode_start_us",
                audio.continuation_decode_start_us
            );
            audio_metric!(
                "continuation_decode_finish_us",
                audio.continuation_decode_finish_us
            );
            audio_metric!("continuation_decode_steps", audio.continuation_decode_steps);
            audio_metric!("continuation_decode_us", audio.continuation_decode_us);
            audio_metric!(
                "continuation_decode_max_us",
                audio.continuation_decode_max_us
            );
            audio_metric!("producer_lock_wait_us", audio.producer_lock_wait_us);
            audio_metric!("producer_lock_wait_max_us", audio.producer_lock_wait_max_us);
            audio_metric!("ring_write_us", audio.ring_write_us);
            audio_metric!("ring_write_max_us", audio.ring_write_max_us);
            audio_metric!("archive_copy_us", audio.archive_copy_us);
            audio_metric!("archive_copy_max_us", audio.archive_copy_max_us);
            audio_metric!("consumer_lock_wait_us", audio.consumer_lock_wait_us);
            audio_metric!("consumer_lock_wait_max_us", audio.consumer_lock_wait_max_us);
            audio_metric!("ring_read_us", audio.ring_read_us);
            audio_metric!("ring_read_max_us", audio.ring_read_max_us);
            audio_metric!("reader_notifications", audio.reader_notifications);
            audio_metric!("reader_waits", audio.reader_waits);
            audio_metric!("reader_wakeups", audio.reader_wakeups);
            let mut detail = serde_json::Map::new();
            detail.insert("audio".to_string(), Value::Object(audio_detail));
            if let Err(error) = &result {
                detail.insert("error".to_string(), Value::String(error.clone()));
            }
            emit_voice_speculation_stage_with_detail(
                &worker_events,
                &worker_speculation,
                if result.is_ok() {
                    "native_audio_finish_returned"
                } else {
                    "native_audio_finish_failed"
                },
                command_started,
                Value::Object(detail),
            );
            if result.is_err() {
                // A failed or cancelled decode may return before reaching the
                // pre-commit latch. Release the conditional import before
                // joining it so cancellation can never strand this worker.
                failure_control.abort();
            }
            if let Some(handle) = import
                .lock()
                .map_err(|_| "voice_attachment_import_lock_poisoned".to_string())?
                .take()
            {
                let _ = handle.join();
            }
            let result = result?;
            let cancelled = result.has_authoritative_cancellation_evidence();
            Ok((to_value(result).map_err(to_error)?, cancelled))
        })
        .await;
    // A native resumed-speech cancellation may complete before the renderer's
    // handoff command arrives. Keep failed attempts consumer-owned so their
    // immutable preparation and resident audio checkpoint remain reusable.
    if result.is_ok() {
        owner.finish_voice_speculation(&speculation);
    }
    result
}

#[tauri::command]
pub async fn mom_llama_voice_audio_speculation_confirm(
    runtime: State<'_, AppRuntimeHandle>,
    speculation: String,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_voice_audio_speculation_confirm"))?;
    Ok(serde_json::json!({
        "confirmed": runtime.confirm_voice_speculation(&speculation)?,
    }))
}

#[tauri::command]
pub async fn mom_llama_voice_audio_speculation_abort(
    runtime: State<'_, AppRuntimeHandle>,
    speculation: String,
    conversation: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_audio_speculation_abort"))?;
    let owner = runtime.inner().clone();
    let operations = runtime.operation_scope();
    lease
        .run_blocking(move || {
            let aborted = owner.abort_and_remove_voice_speculation(&speculation)?;
            let cancellation = mom_llama_runtime::chat_cancel_in_scope(&operations, &conversation)
                .map_err(to_error)?;
            Ok(serde_json::json!({
                "aborted": aborted,
                "cancellation": cancellation,
            }))
        })
        .await
}

/// Belay one speculative reply and hand the still-open microphone directly to
/// a replacement incremental-audio session. Resumed speech used to cross four
/// renderer/native waits (TTS stop, speculation abort, session restart,
/// capture retarget). Keeping cancellation and retarget under one native
/// admission lets the capture owner replay everything retained so far and
/// resume its 10 ms producer without waiting for renderer bookkeeping. The
/// replacement reuses the exact immutable prompt/model/cache preparation.
#[tauri::command]
pub async fn mom_llama_voice_audio_speculation_abort_and_retarget(
    runtime: State<'_, AppRuntimeHandle>,
    capture_state: State<'_, NativeVoiceCapture>,
    capture: Option<String>,
    speculation: String,
    conversation: String,
    message: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec(
        "mom_llama_voice_audio_speculation_abort_and_retarget",
    ))?;
    let owner = runtime.inner().clone();
    let operations = runtime.operation_scope();
    let controller = capture_state.inner().clone();
    lease
        .run_blocking(move || {
            let total_started = Instant::now();

            // Cancel the old request before registering its replacement. Chat
            // cancellation resolves the newest request for this conversation;
            // reversing this order would cancel the replacement itself.
            let abort_started = Instant::now();
            let (aborted, prepared) = owner.take_voice_speculation_for_handoff(&speculation)?;
            if !prepared.matches_turn(&conversation, &message) {
                return Err("voice_speculation_preparation_identity_changed".to_string());
            }
            let cancellation = mom_llama_runtime::chat_cancel_in_scope(&operations, &conversation)
                .map_err(to_error)?;
            let abort_us = abort_started
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;

            let begin_started = Instant::now();
            let session =
                mom_llama_runtime::voice_audio_restart_prepared_in_scope(&operations, prepared)
                    .map_err(to_error)?;
            let resume_wait_started = Instant::now();
            let resume = session.wait_for_resume().map_err(to_error)?;
            if !resume.reused {
                session.cancel();
                return Err("voice_audio_checkpoint_unavailable: The live audio prefix could not be resumed".to_string());
            }
            let resume_wait_us = resume_wait_started
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;
            let request_id = session.request_id().to_string();
            let begin_metrics = session.begin_metrics().clone();
            let session_id = owner.insert_voice_audio_session(session)?;
            let begin_us = begin_started
                .elapsed()
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;

            // Native capture can retarget its retained ring here. WebKit call
            // capture has the same retained samples in the renderer and uses
            // the returned checkpoint offset to replay only its suffix.
            let (retarget_us, replay) = if let Some(capture) = capture {
                let sink = owner.voice_audio_session(&session_id)?;
                let exact_cancellation = sink.cancellation_handle().ok_or_else(|| {
                    "The replacement voice session has no cancellation authority".to_string()
                })?;
                let target = CaptureTarget {
                    id: session_id.clone(),
                    sink,
                    cancel: Arc::new(move || {
                        exact_cancellation.cancel();
                    }),
                };
                let retarget_started = Instant::now();
                let replay = match controller.retarget(capture, target, resume.sample_offset) {
                    Ok(metrics) => metrics,
                    Err(error) => {
                        let _ = owner.abort_voice_audio_session(&session_id);
                        return Err(to_error(error));
                    }
                };
                (
                    retarget_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
                    serde_json::json!(replay),
                )
            } else {
                (0, Value::Null)
            };

            Ok(serde_json::json!({
                "aborted": aborted,
                "cancellation": cancellation,
                "session": session_id,
                "request_id": request_id,
                "conversation_id": conversation,
                "begin_metrics": begin_metrics,
                "abort_us": abort_us,
                "begin_us": begin_us,
                "resume_wait_us": resume_wait_us,
                "resume_reused": resume.reused,
                "resume_sample_offset": resume.sample_offset,
                "resume_resident_max_sequences": resume.resident_max_sequences,
                "resume_checkpoint_supported": resume.checkpoint_supported,
                "resume_checkpoint_available": resume.checkpoint_available,
                "resume_key_matched": resume.resume_key_matched,
                "resume_request_matched": resume.request_matched,
                "resume_binding_us": resume.binding_us,
                "resume_restore_us": resume.restore_us,
                "retarget_us": retarget_us,
                "replay": replay,
                "total_us": total_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
            }))
        })
        .await
}

#[tauri::command]
pub async fn mom_llama_voice_audio_abort(
    runtime: State<'_, AppRuntimeHandle>,
    session: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_voice_audio_abort"))?;
    let owner = runtime.inner().clone();
    lease
        .run_blocking(move || {
            let cancelled_branches = owner.abort_voice_audio_session(&session)?;
            Ok(serde_json::json!({
                "cancelled": true,
                "cancelled_branches": cancelled_branches,
            }))
        })
        .await
}

#[tauri::command]
pub fn mom_llama_attachment_list(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_attachment_list"))?;
    command_value(mom_llama_runtime::attachment_list(conversation.as_deref()))
}

#[tauri::command]
pub async fn mom_llama_attachment_preview(
    runtime: State<'_, AppRuntimeHandle>,
    attachment: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_attachment_preview"))?,
        move || mom_llama_runtime::attachment_preview(&attachment),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_attachment_preview_content(
    runtime: State<'_, AppRuntimeHandle>,
    attachment: String,
    root_sha256: String,
    artifact: String,
    policy_fingerprint: String,
) -> Result<Value, String> {
    blocking_command(
        runtime.admit(command_spec("mom_llama_attachment_preview_content"))?,
        move || {
            mom_llama_runtime::attachment_preview_content(&AttachmentPreviewAnchor {
                attachment_id: attachment,
                root_sha256,
                artifact_id: artifact,
                policy_fingerprint,
            })
        },
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_attachment_preview_bytes(
    runtime: State<'_, AppRuntimeHandle>,
    attachment: String,
    root_sha256: String,
    artifact: String,
    policy_fingerprint: String,
) -> Result<Response, String> {
    blocking_response(
        runtime.admit(command_spec("mom_llama_attachment_preview_bytes"))?,
        move || {
            attachment_preview_response(&AttachmentPreviewAnchor {
                attachment_id: attachment,
                root_sha256,
                artifact_id: artifact,
                policy_fingerprint,
            })
        },
    )
    .await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn mom_llama_speech_transcribe_attachment(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: String,
    attachment: String,
    root_sha256: String,
    artifact: String,
    policy_fingerprint: String,
    operation: String,
) -> Result<Value, String> {
    let lease = runtime.admit(command_spec("mom_llama_speech_transcribe_attachment"))?;
    let speech = runtime.speech();
    let result = speech
        .transcribe_attachment(
            &conversation,
            &AttachmentPreviewAnchor {
                attachment_id: attachment,
                root_sha256,
                artifact_id: artifact,
                policy_fingerprint,
            },
            &operation,
        )
        .await
        .map_err(to_error)?;
    let terminal = speech_terminal(&result);
    let value = command_value(Ok(result));
    lease.finish(terminal)?;
    value
}

#[tauri::command]
pub fn mom_llama_speech_stop(
    runtime: State<'_, AppRuntimeHandle>,
    operation: Option<String>,
    playback: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_speech_stop"))?;
    command_value(
        runtime
            .speech()
            .stop(operation.as_deref(), playback.as_deref()),
    )
}

fn attachment_preview_response(anchor: &AttachmentPreviewAnchor) -> Result<Response, String> {
    let media = mom_llama_runtime::attachment_preview_media(anchor)
        .map_err(to_error)?
        .map_err(|blocker| format!("{}: {}", blocker.code, blocker.message))?;
    Ok(Response::new(media.bytes))
}

#[tauri::command]
pub fn mom_llama_settings_get(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_settings_get"))?;
    command_value(mom_llama_runtime::settings_get())
}

#[tauri::command]
pub fn mom_llama_settings_reset(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_settings_reset"))?;
    command_value(mom_llama_runtime::settings_reset())
}

#[tauri::command]
pub fn mom_llama_settings_update(
    runtime: State<'_, AppRuntimeHandle>,
    input: SettingsUpdateInput,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_settings_update"))?;
    command_value(mom_llama_runtime::settings_update(SettingsUpdate {
        model_path: path_setting_patch(input.model_path),
        mmproj_path: path_setting_patch(input.mmproj_path),
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
    }))
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
pub fn mom_llama_skill_list(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_skill_list"))?;
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
pub fn mom_llama_kv_cache_status(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_kv_cache_status"))?;
    command_value(mom_llama_runtime::kv_cache_status())
}

#[tauri::command]
pub async fn mom_llama_kv_cache_save(
    runtime: State<'_, AppRuntimeHandle>,
    skill: Option<String>,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_kv_cache_save"))?,
        move || mom_llama_runtime::kv_cache_save(&scope, skill),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_kv_cache_restore(
    runtime: State<'_, AppRuntimeHandle>,
    cache: Option<String>,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_kv_cache_restore"))?,
        move || mom_llama_runtime::kv_cache_restore(&scope, cache),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_kv_cache_clear(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_kv_cache_clear"))?,
        move || mom_llama_runtime::kv_cache_clear(&scope),
    )
    .await
}

#[tauri::command]
pub fn mom_llama_mcp_status(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_mcp_status"))?;
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
pub fn mom_llama_mcp_list_servers(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_mcp_list_servers"))?;
    command_value(mom_llama_runtime::mcp_list_servers())
}

#[tauri::command]
pub async fn mom_llama_mcp_list_tools(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
) -> Result<Value, String> {
    let operations = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_list_tools"))?,
        move || mom_llama_runtime::mcp_list_tools_in_scope(&operations, &server),
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
    let operations = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_call_tool"))?,
        move || mom_llama_runtime::mcp_call_tool_in_scope(&operations, &server, &tool, arguments),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_mcp_list_resources(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
) -> Result<Value, String> {
    let operations = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_list_resources"))?,
        move || mom_llama_runtime::mcp_list_resources_in_scope(&operations, &server),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_mcp_read_resource(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
    uri: String,
) -> Result<Value, String> {
    let operations = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_read_resource"))?,
        move || mom_llama_runtime::mcp_read_resource_in_scope(&operations, &server, &uri),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_mcp_list_prompts(
    runtime: State<'_, AppRuntimeHandle>,
    server: String,
) -> Result<Value, String> {
    let operations = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_list_prompts"))?,
        move || mom_llama_runtime::mcp_list_prompts_in_scope(&operations, &server),
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
    let operations = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_mcp_get_prompt"))?,
        move || {
            mom_llama_runtime::mcp_get_prompt_in_scope(&operations, &server, &prompt, arguments)
        },
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
    command_value(mom_llama_runtime::tool_loop_prepare_in_scope(
        &runtime.operation_scope(),
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
    let operations = runtime.operation_scope();
    let events = window.clone();
    blocking_command(lease, move || {
        mom_llama_runtime::tool_loop_run_stream_in_scope(
            &operations,
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
    command_value(mom_llama_runtime::tool_loop_cancel_in_scope(
        &runtime.operation_scope(),
        &conversation,
    ))
}

#[tauri::command]
pub fn mom_llama_tool_loop_status(
    runtime: State<'_, AppRuntimeHandle>,
    conversation: Option<String>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_tool_loop_status"))?;
    command_value(mom_llama_runtime::tool_loop_status(conversation.as_deref()))
}

#[tauri::command]
pub fn mom_llama_tool_permission_list(
    runtime: State<'_, AppRuntimeHandle>,
) -> Result<Value, String> {
    let _lease = runtime.admit(command_spec("mom_llama_tool_permission_list"))?;
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
pub fn mom_llama_model_slot_list(runtime: State<'_, AppRuntimeHandle>) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    let _lease = runtime.admit(command_spec("mom_llama_model_slot_list"))?;
    command_value(mom_llama_runtime::model_slot_list(&scope))
}

#[tauri::command]
pub async fn mom_llama_model_slot_load(
    runtime: State<'_, AppRuntimeHandle>,
    slot: usize,
    model_path: String,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_model_slot_load"))?,
        move || mom_llama_runtime::model_slot_load(&scope, slot, PathBuf::from(model_path)),
    )
    .await
}

#[tauri::command]
pub async fn mom_llama_model_slot_unload(
    runtime: State<'_, AppRuntimeHandle>,
    slot: usize,
) -> Result<Value, String> {
    let scope = runtime.operation_scope();
    blocking_command(
        runtime.admit(command_spec("mom_llama_model_slot_unload"))?,
        move || mom_llama_runtime::model_slot_unload(&scope, slot),
    )
    .await
}

fn value_to_settings_map(value: Value) -> Option<std::collections::BTreeMap<String, Value>> {
    let Value::Object(object) = value else {
        return None;
    };
    Some(object.into_iter().collect())
}

fn path_setting_patch(value: Option<String>) -> Option<Option<PathBuf>> {
    value.map(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| PathBuf::from(value))
    })
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

fn speech_terminal<T: serde::Serialize>(
    result: &mom_llama_runtime::CommandResult<T>,
) -> crate::operation_supervisor::TerminalClass {
    match result.blocker.as_ref().map(|blocker| blocker.code.as_str()) {
        Some("speech_cancelled") => crate::operation_supervisor::TerminalClass::Cancelled,
        Some(_) => crate::operation_supervisor::TerminalClass::Failed,
        None => crate::operation_supervisor::TerminalClass::Completed,
    }
}

async fn blocking_command<T, F>(lease: AppWorkLease, operation: F) -> Result<Value, String>
where
    T: serde::Serialize + Send + 'static,
    F: FnOnce() -> anyhow::Result<mom_llama_runtime::CommandResult<T>> + Send + 'static,
{
    lease
        .run_blocking_with_cancellation_evidence(move || {
            let result = operation().map_err(to_error)?;
            let authoritative_cancellation = result.has_authoritative_cancellation_evidence();
            let value = command_value(Ok(result))?;
            Ok((value, authoritative_cancellation))
        })
        .await
}

async fn blocking_response<F>(lease: AppWorkLease, operation: F) -> Result<Response, String>
where
    F: FnOnce() -> Result<Response, String> + Send + 'static,
{
    lease.run_blocking(operation).await
}

async fn blocking_value<T, F>(lease: AppWorkLease, operation: F) -> Result<Value, String>
where
    T: serde::Serialize + Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    lease
        .run_blocking(move || {
            let value = operation()?;
            to_value(value).map_err(to_error)
        })
        .await
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
    use super::{
        MAX_VOICE_LATENCY_STAGES, MAX_VOICE_LATENCY_TRACE_BYTES, path_setting_patch,
        pcm_stream_frame, validate_voice_latency_trace,
    };
    use speech_native_types::{AudioChunk, PcmFormat, PcmSampleFormat};
    use std::path::PathBuf;

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
    fn durable_voice_trace_accepts_the_renderer_bound_and_rejects_overflow() {
        let stages = (0..MAX_VOICE_LATENCY_STAGES)
            .map(|_| serde_json::json!({ "stage": "packet" }))
            .collect::<Vec<_>>();
        let trace = serde_json::json!({
            "id": "bounded-voice-trace",
            "checkpoint": 512,
            "stages": stages,
        })
        .to_string();
        let (_, stage_count, checkpoint) =
            validate_voice_latency_trace(&trace).expect("renderer-sized trace remains durable");
        assert_eq!(stage_count, MAX_VOICE_LATENCY_STAGES);
        assert_eq!(checkpoint, 512);

        let overflow = serde_json::json!({
            "id": "overflow-voice-trace",
            "stages": (0..=MAX_VOICE_LATENCY_STAGES)
                .map(|_| serde_json::json!({ "stage": "packet" }))
                .collect::<Vec<_>>(),
        })
        .to_string();
        assert_eq!(
            validate_voice_latency_trace(&overflow).expect_err("overflow trace"),
            "voice latency trace has invalid stages"
        );
        assert_eq!(
            validate_voice_latency_trace(&"x".repeat(MAX_VOICE_LATENCY_TRACE_BYTES + 1))
                .expect_err("oversized trace"),
            "voice latency trace exceeds its bounded size"
        );
    }

    #[test]
    fn document_export_route_preserves_the_requested_format() {
        assert_eq!(
            super::resolve_conversation_export_format(Some("document")),
            Ok(super::ConversationExportFormat::Document)
        );
        assert_eq!(
            super::resolve_conversation_export_format(None),
            Ok(super::ConversationExportFormat::Json)
        );
        assert_eq!(
            super::resolve_conversation_export_format(Some("markdown")),
            Ok(super::ConversationExportFormat::Markdown)
        );
    }

    #[test]
    fn unknown_export_route_does_not_silently_select_json() {
        assert!(super::resolve_conversation_export_format(Some("misspelled")).is_err());
    }

    #[test]
    fn path_setting_patch_distinguishes_omitted_clear_and_replace() {
        assert_eq!(path_setting_patch(None), None);
        assert_eq!(path_setting_patch(Some(String::new())), Some(None));
        assert_eq!(path_setting_patch(Some("   ".to_string())), Some(None));
        assert_eq!(
            path_setting_patch(Some("/models/local.gguf".to_string())),
            Some(Some(PathBuf::from("/models/local.gguf")))
        );
    }

    #[test]
    fn multi_second_tauri_commands_stay_off_the_async_dispatch_thread() {
        let source = include_str!("commands.rs");
        for name in [
            "mom_llama_attachment_import_text",
            "mom_llama_attachment_import_paste",
            "mom_llama_attachment_import",
            "mom_llama_attachment_preview",
            "mom_llama_attachment_preview_content",
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
            "mom_llama_model_select",
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
                .contains("blocking_response(")
                && command_body(source, "mom_llama_attachment_preview_bytes")
                    .contains("AttachmentPreviewAnchor")
                && command_body(source, "mom_llama_attachment_preview_bytes")
                    .contains("root_sha256")
                && command_body(source, "mom_llama_attachment_preview_bytes")
                    .contains("policy_fingerprint"),
            "raw attachment previews must read and decrypt outside the async dispatch thread"
        );

        for name in [
            "mom_llama_chat_cancel",
            "mom_llama_voice_audio_push",
            "mom_llama_voice_audio_speculation_abort",
            "mom_llama_voice_audio_speculation_abort_and_retarget",
            "mom_llama_voice_audio_abort",
        ] {
            let body = command_body(source, name);
            assert!(
                body.contains(".run_blocking(") || body.contains("blocking_command("),
                "{name} must never wait for inference or teardown on the UI dispatch thread"
            );
        }
        for name in [
            "mom_llama_voice_audio_finish",
            "mom_llama_voice_audio_speculate",
        ] {
            assert!(
                !command_body(source, name).contains("bytes: Vec<u8>"),
                "{name} must reuse the bounded native PCM archive instead of round-tripping WAV bytes"
            );
        }
        let speculate = command_body(source, "mom_llama_voice_audio_speculate");
        assert!(
            speculate.contains("capture: Option<String>")
                && speculate.contains("wait_for_silence: Option<bool>")
                && speculate.contains("generation_timeout_ms: Option<u64>")
                && speculate.contains("timeout_s: generation_timeout_ms as f64 / 1_000.0")
                && speculate.contains("arm_silence(capture.clone())")
                && speculate.contains("run_blocking_with_cancellation_evidence(move ||")
                && speculate.contains("gate.wait()?")
                && !speculate.contains("spawn_blocking(move || gate.wait())")
                && speculate.contains("pause_prefill(capture)")
                && speculate.find("begin_voice_speculation_with_prepared(&speculation")
                    < speculate.find("pause_prefill(capture)"),
            "native silence handoff must prearm one supervised worker and continue without a second dispatch"
        );
        for stage in [
            "native_command_entered",
            "native_admission_complete",
            "native_cancellation_registered",
            "native_silence_armed",
            "native_generation_worker_prearmed",
            "native_silence_released",
            "native_capture_detached",
            "native_voice_session_acquired",
            "native_generation_worker_started",
            "native_audio_finish_entered",
            "native_audio_finish_returned",
            "native_audio_finish_failed",
        ] {
            assert!(
                speculate.contains(stage),
                "speculative generation must instrument {stage}"
            );
        }
        for metric in [
            "accepted_samples",
            "consumed_samples",
            "projected_blocks",
            "decode_start_us",
            "cancellation_requested_us",
            "cancellation_observed_us",
            "suffix_eval_finish_us",
            "first_token_event_us",
            "sampled_tokens",
            "nonempty_token_pieces",
            "emitted_token_events",
            "last_sample_finish_us",
            "last_token_event_us",
            "continuation_decode_steps",
            "continuation_decode_us",
            "continuation_decode_max_us",
        ] {
            assert!(
                speculate.contains(metric),
                "failed speculative generation must preserve partial native metric {metric}"
            );
        }
        assert!(
            speculate.contains("detail.insert(\"error\"")
                && speculate.contains("Value::String(error.clone())"),
            "failed speculative generation must preserve the native error text"
        );
        let handoff = command_body(
            source,
            "mom_llama_voice_audio_speculation_abort_and_retarget",
        );
        assert!(
            handoff.find("take_voice_speculation_for_handoff(&speculation)")
                < handoff.find("voice_audio_restart_prepared_in_scope"),
            "resumed-speech handoff must atomically consume the old prepared turn before registering its replacement"
        );
        assert!(
            handoff.find("voice_audio_restart_prepared_in_scope")
                < handoff.find("controller.retarget"),
            "replacement audio authority must exist before retained PCM is replayed"
        );
        assert!(
            handoff.contains("take_voice_speculation_for_handoff(&speculation)")
                && handoff.contains("prepared.matches_turn(&conversation, &message)"),
            "resumed-speech retry must reuse only the exact immutable turn preparation"
        );
        for metric in ["abort_us", "begin_us", "retarget_us", "replay", "total_us"] {
            assert!(
                handoff.contains(metric),
                "resumed-speech handoff must report {metric}"
            );
        }
    }

    #[test]
    fn speech_ipc_is_path_free_and_rebinds_exact_authority() {
        let source = include_str!("commands.rs");
        let read_aloud = command_body(source, "mom_llama_speech_read_aloud");
        assert!(
            read_aloud.contains("&conversation")
                && read_aloud.contains("&message")
                && read_aloud.contains("&operation")
                && read_aloud.contains("pcm_stream")
                && read_aloud.contains("InvokeResponseBody::Raw")
                && !read_aloud.contains("PathBuf")
                && !read_aloud.contains("path:"),
            "Read Aloud IPC must accept only opaque operation and message identity"
        );
        let transcribe = command_body(source, "mom_llama_speech_transcribe_attachment");
        assert!(
            transcribe.contains("AttachmentPreviewAnchor")
                && transcribe.contains("root_sha256")
                && transcribe.contains("artifact_id: artifact")
                && transcribe.contains("policy_fingerprint")
                && !transcribe.contains("PathBuf")
                && !transcribe.contains("path:"),
            "transcription IPC must re-present exact Attachment authority without a path"
        );
    }

    #[test]
    fn pcm_stream_frame_is_binary_bounded_and_self_describing() {
        let chunk = AudioChunk {
            sequence: 7,
            sample_offset: 960,
            format: PcmFormat {
                sample_rate_hz: 24_000,
                channels: 1,
                sample_format: PcmSampleFormat::I16Le,
                interleaved: true,
            },
            data: vec![0x34, 0x12, 0xcd, 0xab],
            end_of_stream: true,
        };
        let frame = pcm_stream_frame(&chunk).expect("frame");
        assert_eq!(&frame[..4], b"MPCM");
        assert_eq!(frame[4], 1);
        assert_eq!(frame[5], 3);
        assert_eq!(frame[6], 1);
        assert_eq!(frame[7], 1);
        assert_eq!(
            u32::from_le_bytes(frame[8..12].try_into().expect("rate")),
            24_000
        );
        assert_eq!(
            u64::from_le_bytes(frame[12..20].try_into().expect("offset")),
            7
        );
        assert_eq!(
            u64::from_le_bytes(frame[20..28].try_into().expect("frames")),
            960
        );
        assert_eq!(
            u32::from_le_bytes(frame[28..32].try_into().expect("bytes")),
            4
        );
        assert_eq!(&frame[32..], chunk.data);
    }
}
