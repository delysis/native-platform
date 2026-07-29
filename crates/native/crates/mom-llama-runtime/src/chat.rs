use crate::attachments::media_inputs_for_conversation;
use crate::config::{resolve_settings, upstream_setting_string};
use crate::conversation_store::{
    Message, MessageRole, get_or_create_conversation, load_db, upsert_conversation,
};
use crate::kv_cache::{compatible_cached_prefix, invalidate_cache};
use crate::native_runtime::{cancel_native_request, resident_model};
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::skill_store::skill_prompt_prefix;
use crate::store::RuntimeStore;
use anyhow::{Context, Result};
use llama_native_engine::GenerationTicket;
use llama_native_types::{
    ChatMessage, ChatRole, GenerationEventKind, GenerationOutput, GenerationRequest,
    GenerationState, NativeError, NativeErrorCode, SamplingConfig, SequenceStateBlob,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};
use uuid::Uuid;

const ACTIVE_REQUESTS_NAMESPACE: &str = "active-requests.v2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatSendInput {
    pub conversation_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ChatSendOptions {
    pub timeout_s: f64,
    pub fake_fixture: bool,
}

impl Default for ChatSendOptions {
    fn default() -> Self {
        Self {
            timeout_s: 120.0,
            fake_fixture: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatSendOutput {
    pub request_id: String,
    pub conversation_id: String,
    pub user_message_id: String,
    pub assistant_message_id: String,
    pub assistant_text: String,
    pub model_path: String,
    pub duration_ms: u128,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub cache_id: Option<String>,
    pub cache_reused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatRequestState {
    Running,
    Completed,
    CancelRequested,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveChatRequest {
    pub request_id: String,
    pub conversation_id: String,
    pub pid: Option<u32>,
    pub started_at: String,
    pub updated_at: String,
    pub state: ChatRequestState,
    pub cancel_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ActiveChatDb {
    #[serde(default)]
    pub requests: Vec<ActiveChatRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatCancelOutput {
    pub request_id: String,
    pub conversation_id: String,
    pub cancel_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatStreamEvent {
    pub schema: String,
    pub command: String,
    pub request_id: String,
    pub conversation_id: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub fake_fixture: bool,
    pub real_engine_invoked: bool,
}

pub fn chat_send(
    input: ChatSendInput,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatSendOutput>> {
    chat_send_supervised(input, options, None::<fn(ChatStreamEvent) -> Result<()>>)
}

pub fn chat_send_stream<F>(
    input: ChatSendInput,
    options: ChatSendOptions,
    on_event: F,
) -> Result<CommandResult<ChatSendOutput>>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    chat_send_supervised(input, options, Some(on_event))
}

pub fn chat_regenerate(
    conversation_id: &str,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatSendOutput>> {
    let db = load_db()?;
    let Some(message) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
        .and_then(|conversation| {
            conversation
                .messages
                .iter()
                .rev()
                .find(|message| message.role == MessageRole::User)
        })
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_regenerate",
            "stub_blocked",
            Blocker::new(
                "no_user_message",
                "No user message is available to regenerate.",
                vec!["Send a message first.".to_string()],
            ),
        ));
    };
    let message = message.content.clone();
    let mut result = chat_send(
        ChatSendInput {
            conversation_id: conversation_id.to_string(),
            message,
        },
        options,
    )?;
    retag_chat_result(&mut result, "mom_llama.chat_regenerate");
    Ok(result)
}

pub fn chat_continue(
    conversation_id: &str,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatSendOutput>> {
    let db = load_db()?;
    let has_assistant = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
        .is_some_and(|conversation| {
            conversation
                .messages
                .iter()
                .any(|message| message.role == MessageRole::Assistant)
        });
    if !has_assistant {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_continue",
            "stub_blocked",
            Blocker::new(
                "no_assistant_message",
                "No assistant message is available to continue.",
                vec!["Ask a question first.".to_string()],
            ),
        ));
    }
    let mut result = chat_send(
        ChatSendInput {
            conversation_id: conversation_id.to_string(),
            message: "Please continue the previous answer.".to_string(),
        },
        options,
    )?;
    retag_chat_result(&mut result, "mom_llama.chat_continue");
    Ok(result)
}

fn chat_send_supervised<F>(
    input: ChatSendInput,
    options: ChatSendOptions,
    mut on_event: Option<F>,
) -> Result<CommandResult<ChatSendOutput>>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    if input.message.trim().is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_send",
            "stub_blocked",
            Blocker::new(
                "message_empty",
                "Message text is empty.",
                vec!["Type a message before sending.".to_string()],
            ),
        ));
    }
    let settings = resolve_settings()?;
    let media = media_inputs_for_conversation(&input.conversation_id)?;
    if !media.is_empty()
        && !settings
            .mmproj_path
            .as_ref()
            .is_some_and(|path| path.is_file())
    {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_send",
            "blocked_missing_mmproj",
            Blocker::new(
                "mmproj_path_missing",
                "This conversation contains image or audio attachments, but no native multimodal projector is configured.",
                vec!["Choose the matching mmproj GGUF in Settings.".to_string()],
            ),
        ));
    }
    let (db, mut conversation) = get_or_create_conversation(&input.conversation_id)?;
    let skill_prefix = skill_prompt_prefix(&conversation.current_skill_ids)?;
    let system_message = upstream_setting_string(&settings, "systemMessage").unwrap_or_default();
    let messages = build_native_messages(
        &system_message,
        &skill_prefix,
        &conversation.messages,
        &input.message,
    );
    let cached_prefix = if media.is_empty() {
        compatible_cached_prefix(&skill_prefix)?
    } else {
        None
    };
    let request_id = Uuid::new_v4().to_string();
    let cancel_path = format!("native://request/{request_id}");
    register_active_request(
        &settings.data_dir,
        ActiveChatRequest {
            request_id: request_id.clone(),
            conversation_id: conversation.id.clone(),
            pid: None,
            started_at: now_ms().to_string(),
            updated_at: now_ms().to_string(),
            state: ChatRequestState::Running,
            cancel_path: cancel_path.clone(),
        },
    )?;
    emit(
        &mut on_event,
        stream_event(
            &request_id,
            &conversation.id,
            "started",
            None,
            Some(if options.fake_fixture {
                "Started labeled fixture generation.".to_string()
            } else {
                "Started in-process llama.cpp generation.".to_string()
            }),
            options,
        ),
    )?;
    let started = Instant::now();
    let model_path = settings.model_path.clone().unwrap_or_default();
    let (assistant_text, prompt_tokens, completion_tokens, duration_ms, cache_id, cache_reused) =
        if options.fake_fixture {
            (
                "Fixture assistant response.".to_string(),
                messages.iter().map(|message| message.content.len()).sum(),
                3,
                started.elapsed().as_millis(),
                None,
                false,
            )
        } else {
            let handle = match resident_model(&settings) {
                Ok(handle) => handle,
                Err(blocked) => {
                    mark_request_state(&settings.data_dir, &request_id, ChatRequestState::Failed)?;
                    return Ok(CommandResult::blocked(
                        "mom_llama.chat_send",
                        &blocked.readiness,
                        blocked.blocker,
                    ));
                }
            };
            let status = handle.status();
            let sampling = sampling_config(&settings);
            let build_request = |cached_prefix: Option<SequenceStateBlob>| GenerationRequest {
                request_id: request_id.clone(),
                model_id: status.model_id.clone(),
                messages: messages.clone(),
                sampling: sampling.clone(),
                media: media.clone(),
                cached_prefix,
            };
            let attempted_cache_id = cached_prefix.as_ref().map(|(id, _)| id.clone());
            let first_prefix = cached_prefix.as_ref().map(|(_, state)| state.clone());
            let first_ticket = handle
                .generate(build_request(first_prefix))
                .map_err(|error| anyhow::anyhow!(error))?;
            let (outputs, cache_reused) = match consume_chat_ticket(
                first_ticket,
                &request_id,
                &conversation.id,
                options,
                &mut on_event,
            ) {
                Ok(outputs) => (outputs, attempted_cache_id.is_some()),
                Err(ChatTicketError::Native(error))
                    if error.code == NativeErrorCode::CacheIncompatible
                        && attempted_cache_id.is_some() =>
                {
                    if let Some(cache_id) = attempted_cache_id.as_deref() {
                        invalidate_cache(cache_id)?;
                    }
                    let retry = handle
                        .generate(build_request(None))
                        .map_err(|error| anyhow::anyhow!(error))?;
                    (
                        consume_chat_ticket(
                            retry,
                            &request_id,
                            &conversation.id,
                            options,
                            &mut on_event,
                        )
                        .map_err(chat_ticket_error)?,
                        false,
                    )
                }
                Err(error) => return Err(chat_ticket_error(error)),
            };
            let Some(output) = outputs.into_iter().next() else {
                mark_request_state(&settings.data_dir, &request_id, ChatRequestState::Failed)?;
                return Ok(CommandResult::blocked(
                    "mom_llama.chat_send",
                    "blocked_native_runtime",
                    Blocker::new(
                        "native_response_missing",
                        "The native model returned no response.",
                        vec!["Try the request again.".to_string()],
                    ),
                ));
            };
            if output.state == GenerationState::Cancelled {
                mark_request_state(&settings.data_dir, &request_id, ChatRequestState::Cancelled)?;
                return Ok(CommandResult::blocked(
                    "mom_llama.chat_send",
                    "stub_blocked",
                    Blocker::new(
                        "chat_cancelled",
                        "The local model request was cancelled.",
                        vec!["Send the message again to retry.".to_string()],
                    ),
                ));
            }
            (
                output.text,
                output.metrics.prompt_tokens,
                output.metrics.completion_tokens,
                output.metrics.duration_ms,
                attempted_cache_id.filter(|_| cache_reused),
                cache_reused,
            )
        };
    if assistant_text.trim().is_empty() {
        mark_request_state(&settings.data_dir, &request_id, ChatRequestState::Failed)?;
        return Ok(CommandResult::blocked(
            "mom_llama.chat_send",
            "blocked_native_runtime",
            Blocker::new(
                "native_response_empty",
                "The native model returned no assistant text.",
                vec!["Try a smaller prompt or another model.".to_string()],
            ),
        ));
    }
    let user_message = Message {
        id: Uuid::new_v4().to_string(),
        conversation_id: conversation.id.clone(),
        role: MessageRole::User,
        content: input.message,
        created_at: now_ms().to_string(),
        parent_id: conversation
            .messages
            .last()
            .map(|message| message.id.clone()),
        model: model_name(&model_path),
        receipt_id: None,
        prompt_tokens: Some(prompt_tokens),
        completion_tokens: None,
    };
    let assistant_message = Message {
        id: Uuid::new_v4().to_string(),
        conversation_id: conversation.id.clone(),
        role: MessageRole::Assistant,
        content: assistant_text.clone(),
        created_at: now_ms().to_string(),
        parent_id: Some(user_message.id.clone()),
        model: model_name(&model_path),
        receipt_id: Some(format!("mom_llama.chat_send:{request_id}")),
        prompt_tokens: None,
        completion_tokens: Some(completion_tokens),
    };
    let user_message_id = user_message.id.clone();
    let assistant_message_id = assistant_message.id.clone();
    conversation.messages.push(user_message);
    conversation.messages.push(assistant_message);
    if upstream_setting_bool(&settings, "titleGenerationUseFirstLine")
        && should_replace_title(&conversation.title, &conversation.id)
    {
        conversation.title = first_line_title(&conversation.messages);
    }
    conversation.updated_at = now_ms().to_string();
    conversation.selected_model_path = settings.model_path.clone();
    let path = upsert_conversation(db, conversation.clone())?;
    mark_request_state(&settings.data_dir, &request_id, ChatRequestState::Completed)?;
    let result = ChatSendOutput {
        request_id: request_id.clone(),
        conversation_id: conversation.id,
        user_message_id,
        assistant_message_id,
        assistant_text,
        model_path: if options.fake_fixture {
            "fixture.gguf".to_string()
        } else {
            model_path.display().to_string()
        },
        duration_ms,
        prompt_tokens,
        completion_tokens,
        cache_id,
        cache_reused,
    };
    emit(
        &mut on_event,
        stream_event(
            &request_id,
            &result.conversation_id,
            "completed",
            None,
            Some(result.assistant_text.clone()),
            options,
        ),
    )?;
    Ok(CommandResult::passed(
        "mom_llama.chat_send",
        if options.fake_fixture {
            "fake_fixture_exercised"
        } else {
            "real_prompt_smoke_passed"
        },
        result,
        vec![path.display().to_string()],
        Vec::new(),
        !options.fake_fixture,
        options.fake_fixture,
    ))
}

pub fn chat_cancel(conversation_id: &str) -> Result<CommandResult<ChatCancelOutput>> {
    let settings = resolve_settings()?;
    let db = load_active_requests(&settings.data_dir)?;
    let Some(request) = db
        .requests
        .iter()
        .rev()
        .find(|request| {
            request.conversation_id == conversation_id
                && matches!(
                    request.state,
                    ChatRequestState::Running | ChatRequestState::CancelRequested
                )
        })
        .cloned()
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_cancel",
            "stub_blocked",
            Blocker::new(
                "no_active_chat_request",
                format!("No active request is registered for conversation {conversation_id}."),
                vec!["Send a message before cancelling.".to_string()],
            ),
        ));
    };
    let cancelled = cancel_native_request(&request.request_id, None);
    if cancelled == 0 {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_cancel",
            "stub_blocked",
            Blocker::new(
                "chat_request_not_resident",
                "The request finished before cancellation reached the native worker.",
                vec!["Refresh the conversation.".to_string()],
            ),
        ));
    }
    mark_request_state(
        &settings.data_dir,
        &request.request_id,
        ChatRequestState::CancelRequested,
    )?;
    Ok(CommandResult::passed(
        "mom_llama.chat_cancel",
        "contracted",
        ChatCancelOutput {
            request_id: request.request_id,
            conversation_id: conversation_id.to_string(),
            cancel_path: request.cancel_path,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

fn sampling_config(settings: &crate::config::Settings) -> SamplingConfig {
    settings.sampling_config()
}

fn build_native_messages(
    system_message: &str,
    skill_prefix: &str,
    messages: &[Message],
    next_message: &str,
) -> Vec<ChatMessage> {
    let mut result = Vec::new();
    let combined_system = [system_message.trim(), skill_prefix.trim()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !combined_system.is_empty() {
        result.push(ChatMessage {
            role: ChatRole::System,
            content: combined_system,
        });
    }
    result.extend(messages.iter().map(|message| ChatMessage {
        role: match message.role {
            MessageRole::System => ChatRole::System,
            MessageRole::User => ChatRole::User,
            MessageRole::Assistant => ChatRole::Assistant,
        },
        content: message.content.clone(),
    }));
    result.push(ChatMessage {
        role: ChatRole::User,
        content: next_message.to_string(),
    });
    result
}

fn model_name(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string)
}

fn upstream_setting_bool(settings: &crate::config::Settings, key: &str) -> bool {
    settings
        .upstream_settings
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn should_replace_title(title: &str, conversation_id: &str) -> bool {
    matches!(title, "New chat" | "Default chat" | "Untitled conversation")
        || title == conversation_id
}

fn first_line_title(messages: &[Message]) -> String {
    messages
        .iter()
        .find(|message| message.role == MessageRole::User)
        .and_then(|message| message.content.lines().find(|line| !line.trim().is_empty()))
        .map(|line| {
            let mut title = line.trim().chars().take(64).collect::<String>();
            if title.len() < line.trim().len() {
                title.push_str("...");
            }
            title
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "New chat".to_string())
}

fn retag_chat_result(result: &mut CommandResult<ChatSendOutput>, command: &str) {
    result.command = command.to_string();
    result.receipt.command = command.to_string();
    result.receipt.task_id = format!("{command}:{}", result.receipt.created_at);
}

fn emit<F>(on_event: &mut Option<F>, event: ChatStreamEvent) -> Result<()>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    if let Some(callback) = on_event.as_mut() {
        callback(event)?;
    }
    Ok(())
}

enum ChatTicketError {
    Native(NativeError),
    Callback(anyhow::Error),
}

fn chat_ticket_error(error: ChatTicketError) -> anyhow::Error {
    match error {
        ChatTicketError::Native(error) => anyhow::anyhow!(error),
        ChatTicketError::Callback(error) => error,
    }
}

fn consume_chat_ticket<F>(
    ticket: GenerationTicket,
    request_id: &str,
    conversation_id: &str,
    options: ChatSendOptions,
    on_event: &mut Option<F>,
) -> std::result::Result<Vec<GenerationOutput>, ChatTicketError>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    let started = Instant::now();
    let timeout = Duration::from_secs_f64(options.timeout_s.max(0.001));
    loop {
        if started.elapsed() >= timeout {
            ticket.cancel_all();
        }
        match ticket.events.recv_timeout(Duration::from_millis(20)) {
            Ok(event) => match event.event {
                GenerationEventKind::Delta { text } => emit(
                    on_event,
                    stream_event(
                        request_id,
                        conversation_id,
                        "delta",
                        Some(text),
                        None,
                        options,
                    ),
                )
                .map_err(ChatTicketError::Callback)?,
                GenerationEventKind::State { state } => {
                    if state == GenerationState::Cancelled {
                        emit(
                            on_event,
                            stream_event(
                                request_id,
                                conversation_id,
                                "cancelled",
                                None,
                                Some("Generation cancelled.".to_string()),
                                options,
                            ),
                        )
                        .map_err(ChatTicketError::Callback)?;
                    }
                }
                GenerationEventKind::Warning { code, message } => emit(
                    on_event,
                    stream_event(
                        request_id,
                        conversation_id,
                        "warning",
                        None,
                        Some(format!("{code}: {message}")),
                        options,
                    ),
                )
                .map_err(ChatTicketError::Callback)?,
            },
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    ticket.wait().map_err(ChatTicketError::Native)
}

fn stream_event(
    request_id: &str,
    conversation_id: &str,
    event: &str,
    delta: Option<String>,
    message: Option<String>,
    options: ChatSendOptions,
) -> ChatStreamEvent {
    ChatStreamEvent {
        schema: "mom_llama.chat_stream_event.v1".to_string(),
        command: "mom_llama.chat_send".to_string(),
        request_id: request_id.to_string(),
        conversation_id: conversation_id.to_string(),
        event: event.to_string(),
        delta,
        message,
        fake_fixture: options.fake_fixture,
        real_engine_invoked: !options.fake_fixture,
    }
}

fn load_active_requests(data_dir: &Path) -> Result<ActiveChatDb> {
    Ok(RuntimeStore::open(data_dir)?
        .get(ACTIVE_REQUESTS_NAMESPACE)?
        .unwrap_or_default())
}

fn register_active_request(data_dir: &Path, request: ActiveChatRequest) -> Result<()> {
    mutate_active_requests(data_dir, |db| db.requests.push(request))
}

fn mark_request_state(data_dir: &Path, request_id: &str, state: ChatRequestState) -> Result<()> {
    mutate_active_requests(data_dir, |db| {
        for request in &mut db.requests {
            if request.request_id == request_id {
                request.state = state.clone();
                request.updated_at = now_ms().to_string();
            }
        }
    })
    .with_context(|| "failed to persist active request state")
}

fn mutate_active_requests(data_dir: &Path, mutation: impl FnOnce(&mut ActiveChatDb)) -> Result<()> {
    RuntimeStore::open(data_dir)?.mutate(ACTIVE_REQUESTS_NAMESPACE, ActiveChatDb::default, |db| {
        mutation(db);
        Ok(())
    })
}
