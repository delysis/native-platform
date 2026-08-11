use crate::attachments::{
    ChatAttachmentContext, commit_generated_exchange, prepare_chat_attachments,
};
use crate::config::{resolve_settings, upstream_setting_string};
use crate::conversation_store::{
    ChatTemplatePolicy, Message, MessageRole, active_leaf_id, active_path_messages,
    get_or_create_conversation, load_db, strip_reserved_attribution_prefix,
};
use crate::kv_cache::{
    compatible_cached_prefix, ensure_persona_prefix, invalidate_cache, persist_session_checkpoint,
};
use crate::native_runtime::{
    cancel_native_request, resident_model_for_profile, skip_native_reasoning,
};
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::skill_store::applied_skill_prompt;
use crate::store::RuntimeStore;
use anyhow::{Context, Result};
use llama_native_engine::GenerationTicket;
use llama_native_types::{
    ChatMessage, ChatRole, ChatTemplateChoice, GenerationEventKind, GenerationInput,
    GenerationOutput, GenerationRequest, GenerationState, NativeError, NativeErrorCode,
    SamplingConfig, SequenceStateBlob,
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
    pub reasoning_content: Option<String>,
    pub reasoning_incomplete: bool,
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
pub struct ChatSkipReasoningOutput {
    pub request_id: String,
    pub conversation_id: String,
    pub branch_id: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReasoningTarget {
    Content,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RoutedDelta {
    target: ReasoningTarget,
    text: String,
}

#[derive(Debug, Default)]
struct ReasoningStreamParser {
    buffer: String,
    in_reasoning: bool,
    saw_reasoning: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedReasoning {
    content: String,
    reasoning: String,
    incomplete: bool,
}

const REASONING_START_MARKERS: &[&str] = &[
    "<think>",
    "<<<reasoning_content_start>>>",
    "[Start thinking]",
];
const REASONING_END_MARKERS: &[&str] =
    &["</think>", "<<<reasoning_content_end>>>", "[End thinking]"];

impl ReasoningStreamParser {
    fn push(&mut self, delta: &str) -> Vec<RoutedDelta> {
        self.buffer.push_str(delta);
        self.drain(false)
    }

    fn finish(&mut self) -> Vec<RoutedDelta> {
        self.drain(true)
    }

    fn drain(&mut self, flush: bool) -> Vec<RoutedDelta> {
        let mut routed = Vec::new();
        loop {
            let markers = if self.in_reasoning {
                REASONING_END_MARKERS
            } else {
                REASONING_START_MARKERS
            };
            if let Some((index, marker)) = earliest_marker(&self.buffer, markers) {
                if index > 0 {
                    let prefix = self.buffer[..index].to_string();
                    self.push_routed(&mut routed, prefix);
                }
                self.buffer.drain(..index + marker.len());
                self.in_reasoning = !self.in_reasoning;
                self.saw_reasoning = true;
                continue;
            }
            let retained = if flush {
                0
            } else {
                longest_marker_prefix_suffix(&self.buffer, markers)
            };
            let emit_len = self.buffer.len().saturating_sub(retained);
            if emit_len > 0 {
                let text = self.buffer[..emit_len].to_string();
                self.buffer.drain(..emit_len);
                self.push_routed(&mut routed, text);
            }
            break;
        }
        routed
    }

    fn push_routed(&self, routed: &mut Vec<RoutedDelta>, text: String) {
        if text.is_empty() {
            return;
        }
        let target = if self.in_reasoning {
            ReasoningTarget::Reasoning
        } else {
            ReasoningTarget::Content
        };
        if let Some(last) = routed.last_mut()
            && last.target == target
        {
            last.text.push_str(&text);
            return;
        }
        routed.push(RoutedDelta { target, text });
    }
}

fn earliest_marker<'a>(value: &str, markers: &'a [&'a str]) -> Option<(usize, &'a str)> {
    markers
        .iter()
        .filter_map(|marker| value.find(marker).map(|index| (index, *marker)))
        .min_by_key(|(index, _)| *index)
}

fn longest_marker_prefix_suffix(value: &str, markers: &[&str]) -> usize {
    markers
        .iter()
        .map(|marker| {
            let limit = value.len().min(marker.len().saturating_sub(1));
            (1..=limit)
                .rev()
                .find(|length| {
                    value.is_char_boundary(value.len() - length)
                        && marker.is_char_boundary(*length)
                        && value[value.len() - length..] == marker[..*length]
                })
                .unwrap_or_default()
        })
        .max()
        .unwrap_or_default()
}

fn parse_reasoning_output(value: &str, enabled: bool) -> ParsedReasoning {
    if !enabled {
        return ParsedReasoning {
            content: value.to_string(),
            ..ParsedReasoning::default()
        };
    }
    let mut parser = ReasoningStreamParser::default();
    let mut parsed = ParsedReasoning::default();
    for delta in parser.push(value).into_iter().chain(parser.finish()) {
        match delta.target {
            ReasoningTarget::Content => parsed.content.push_str(&delta.text),
            ReasoningTarget::Reasoning => parsed.reasoning.push_str(&delta.text),
        }
    }
    parsed.incomplete = parser.saw_reasoning && parser.in_reasoning;
    parsed
}

pub fn chat_send(
    input: ChatSendInput,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatSendOutput>> {
    chat_send_supervised(
        input,
        options,
        None,
        None::<fn(ChatStreamEvent) -> Result<()>>,
    )
}

pub fn chat_send_stream<F>(
    input: ChatSendInput,
    options: ChatSendOptions,
    on_event: F,
) -> Result<CommandResult<ChatSendOutput>>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    chat_send_supervised(input, options, None, Some(on_event))
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
            active_path_messages(conversation)
                .into_iter()
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
    let message_id = message.id.clone();
    let mut result = chat_send_supervised(
        ChatSendInput {
            conversation_id: conversation_id.to_string(),
            message: message.content,
        },
        options,
        Some(message_id),
        None::<fn(ChatStreamEvent) -> Result<()>>,
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
    regenerate_user_id: Option<String>,
    mut on_event: Option<F>,
) -> Result<CommandResult<ChatSendOutput>>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    let mut settings = resolve_settings()?;
    let (db, mut conversation) = get_or_create_conversation(&input.conversation_id)?;
    let expected_active_leaf = conversation.active_leaf_message_id.clone();
    settings.model_path = conversation
        .execution_profile
        .model_path
        .clone()
        .or_else(|| conversation.selected_model_path.clone())
        .or(settings.model_path);
    settings.mmproj_path = conversation
        .execution_profile
        .mmproj_path
        .clone()
        .or(settings.mmproj_path);
    let active_messages = active_path_messages(&conversation);
    let attachment_context = match prepare_chat_attachments(
        &input.conversation_id,
        &active_messages,
        regenerate_user_id.as_deref(),
    )? {
        Ok(context) => context,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.chat_send",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    if user_turn_is_empty(&input.message, &attachment_context) {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_send",
            "stub_blocked",
            Blocker::new(
                "message_empty",
                "The message has no text or model-ready attachment content.",
                vec!["Type a message or attach a supported file before sending.".to_string()],
            ),
        ));
    }
    let media = attachment_context.media.clone();
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
    let skill_prompt = applied_skill_prompt(&conversation.current_skill_ids)?;
    let system_message = conversation
        .execution_profile
        .system_message
        .clone()
        .unwrap_or_else(|| upstream_setting_string(&settings, "systemMessage").unwrap_or_default());
    let parse_reasoning = !upstream_setting_bool(&settings, "disableReasoningParsing");
    let exclude_reasoning_from_context =
        upstream_setting_bool(&settings, "excludeReasoningFromContext");
    let context_messages = if let Some(user_id) = regenerate_user_id.as_deref() {
        let Some(index) = active_messages
            .iter()
            .position(|message| message.id == user_id && message.role == MessageRole::User)
        else {
            return Ok(CommandResult::blocked(
                "mom_llama.chat_regenerate",
                "stub_blocked",
                Blocker::new(
                    "regenerate_user_not_on_active_path",
                    "The user message selected for regeneration is not on the active branch.",
                    vec!["Refresh the conversation and retry.".to_string()],
                ),
            ));
        };
        &active_messages[..index]
    } else {
        active_messages.as_slice()
    };
    let messages = build_native_messages(
        &system_message,
        &skill_prompt.prompt,
        context_messages,
        &input.message,
        exclude_reasoning_from_context,
        &attachment_context.text_by_message_id,
        &attachment_context.current_text,
    );
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
    let (
        assistant_text,
        reasoning_content,
        reasoning_incomplete,
        prompt_tokens,
        completion_tokens,
        duration_ms,
        cache_id,
        cache_reused,
    ) = if options.fake_fixture {
        (
            "Fixture assistant response.".to_string(),
            None,
            false,
            messages.iter().map(|message| message.content.len()).sum(),
            3,
            started.elapsed().as_millis(),
            None,
            false,
        )
    } else {
        let handle = match resident_model_for_profile(
            &settings,
            &model_path,
            settings.mmproj_path.as_deref(),
        ) {
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
        let default_template = matches!(
            conversation.execution_profile.chat_template,
            ChatTemplatePolicy::ModelDefault
        );
        let (cached_prefix, cache_was_preexisting) = if media.is_empty() && default_template {
            if let Some(owner_id) = skill_prompt.cache_owner_id.as_deref() {
                let stable_messages = messages
                    .first()
                    .filter(|message| message.role == ChatRole::System)
                    .cloned()
                    .into_iter()
                    .collect::<Vec<_>>();
                let cache = ensure_persona_prefix(
                    &handle,
                    owner_id,
                    &skill_prompt.cache_label,
                    &stable_messages,
                    &messages,
                )?;
                (
                    cache
                        .as_ref()
                        .map(|cache| (cache.cache_id.clone(), cache.sequence.clone())),
                    cache.is_some_and(|cache| cache.reused),
                )
            } else {
                let cache = compatible_cached_prefix(&handle, &messages)?;
                let reused = cache.is_some();
                (cache, reused)
            }
        } else {
            (None, false)
        };
        let sampling = conversation
            .execution_profile
            .sampling
            .clone()
            .unwrap_or_else(|| sampling_config(&settings));
        let build_request = |cached_prefix: Option<SequenceStateBlob>| GenerationRequest {
            request_id: request_id.clone(),
            model_id: status.model_id.clone(),
            input: GenerationInput::Chat {
                messages: messages.clone(),
                template: match &conversation.execution_profile.chat_template {
                    ChatTemplatePolicy::ModelDefault => ChatTemplateChoice::ModelDefault,
                    ChatTemplatePolicy::FrozenSource(template) => {
                        ChatTemplateChoice::Override(template.clone())
                    }
                },
            },
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
            parse_reasoning,
            &mut on_event,
        ) {
            Ok(outputs) => (
                outputs,
                attempted_cache_id.is_some() && cache_was_preexisting,
            ),
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
                        parse_reasoning,
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
        if media.is_empty()
            && default_template
            && settings.kv_cache_policy.persists_conversation_checkpoints()
        {
            let _ = persist_session_checkpoint(&conversation.id, &handle)?;
        }
        let parsed = parse_reasoning_output(&output.text, parse_reasoning);
        (
            strip_reserved_attribution_prefix(&parsed.content),
            (!parsed.reasoning.is_empty()).then_some(parsed.reasoning),
            parsed.incomplete,
            output.metrics.prompt_tokens,
            output.metrics.completion_tokens,
            output.metrics.duration_ms,
            attempted_cache_id.filter(|_| cache_reused),
            cache_reused,
        )
    };
    if assistant_text.trim().is_empty() {
        mark_request_state(&settings.data_dir, &request_id, ChatRequestState::Failed)?;
        let (code, message, actions) = if reasoning_content
            .as_deref()
            .is_some_and(|reasoning| !reasoning.trim().is_empty())
        {
            (
                "native_response_reasoning_only",
                "The native model exhausted its response before producing visible assistant text.",
                vec![
                    "Increase the response-token budget or skip reasoning during generation."
                        .to_string(),
                    "Try the request again with a more concise prompt.".to_string(),
                ],
            )
        } else {
            (
                "native_response_empty",
                "The native model returned no assistant text.",
                vec!["Try a smaller prompt or another model.".to_string()],
            )
        };
        return Ok(CommandResult::blocked(
            "mom_llama.chat_send",
            "blocked_native_runtime",
            Blocker::new(code, message, actions),
        ));
    }
    let user_message = regenerate_user_id.as_ref().map_or_else(
        || {
            Some(Message {
                id: Uuid::new_v4().to_string(),
                conversation_id: conversation.id.clone(),
                role: MessageRole::User,
                content: input.message,
                created_at: now_ms().to_string(),
                parent_id: active_leaf_id(&conversation),
                model: model_name(&model_path),
                receipt_id: None,
                prompt_tokens: Some(prompt_tokens),
                completion_tokens: None,
                reasoning_content: None,
                reasoning_incomplete: false,
                branch_index: None,
                branch_count: None,
                attribution: None,
                attachment_ids: attachment_context.staged_ids.clone(),
            })
        },
        |_| None,
    );
    let user_message_id = regenerate_user_id
        .clone()
        .or_else(|| user_message.as_ref().map(|message| message.id.clone()))
        .expect("a new or existing user message must back assistant generation");
    let assistant_message = Message {
        id: Uuid::new_v4().to_string(),
        conversation_id: conversation.id.clone(),
        role: MessageRole::Assistant,
        content: assistant_text.clone(),
        created_at: now_ms().to_string(),
        parent_id: Some(user_message_id.clone()),
        model: model_name(&model_path),
        receipt_id: Some(format!("mom_llama.chat_send:{request_id}")),
        prompt_tokens: None,
        completion_tokens: Some(completion_tokens),
        reasoning_content: reasoning_content.clone(),
        reasoning_incomplete,
        branch_index: None,
        branch_count: None,
        attribution: None,
        attachment_ids: Vec::new(),
    };
    let assistant_message_id = assistant_message.id.clone();
    if let Some(user_message) = user_message {
        conversation.messages.push(user_message);
    }
    conversation.messages.push(assistant_message);
    conversation.active_leaf_message_id = Some(assistant_message_id.clone());
    if upstream_setting_bool(&settings, "titleGenerationUseFirstLine")
        && should_replace_title(&conversation.title, &conversation.id)
    {
        conversation.title = first_line_title(&conversation.messages);
    }
    conversation.updated_at = now_ms().to_string();
    conversation.selected_model_path = settings.model_path.clone();
    conversation.execution_profile.model_path = settings.model_path.clone();
    conversation.execution_profile.mmproj_path = settings.mmproj_path.clone();
    let path = commit_generated_exchange(
        db,
        conversation.clone(),
        expected_active_leaf.as_deref(),
        &attachment_context.staged_ids,
        &user_message_id,
        regenerate_user_id.is_none(),
    )?;
    mark_request_state(&settings.data_dir, &request_id, ChatRequestState::Completed)?;
    let result = ChatSendOutput {
        request_id: request_id.clone(),
        conversation_id: conversation.id,
        user_message_id,
        assistant_message_id,
        assistant_text,
        reasoning_content,
        reasoning_incomplete,
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

fn user_turn_is_empty(message: &str, attachments: &ChatAttachmentContext) -> bool {
    message.trim().is_empty()
        && attachments.current_text.trim().is_empty()
        && attachments.media.is_empty()
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

pub fn chat_skip_reasoning(
    conversation_id: &str,
) -> Result<CommandResult<ChatSkipReasoningOutput>> {
    let settings = resolve_settings()?;
    let db = load_active_requests(&settings.data_dir)?;
    let Some(request) = db
        .requests
        .iter()
        .rev()
        .find(|request| {
            request.conversation_id == conversation_id && request.state == ChatRequestState::Running
        })
        .cloned()
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_skip_reasoning",
            "stub_blocked",
            Blocker::new(
                "no_active_reasoning_request",
                format!(
                    "No active reasoning request is registered for conversation {conversation_id}."
                ),
                vec!["Send a message with a reasoning-capable model first.".to_string()],
            ),
        ));
    };
    let branch_id = "assistant";
    if skip_native_reasoning(&request.request_id, Some(branch_id)) == 0 {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_skip_reasoning",
            "stub_blocked",
            Blocker::new(
                "reasoning_request_not_resident",
                "The request finished before the native reasoning control reached it.",
                vec!["Refresh the conversation.".to_string()],
            ),
        ));
    }
    Ok(CommandResult::passed(
        "mom_llama.chat_skip_reasoning",
        "contracted",
        ChatSkipReasoningOutput {
            request_id: request.request_id,
            conversation_id: conversation_id.to_string(),
            branch_id: branch_id.to_string(),
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
    exclude_reasoning_from_context: bool,
    attachment_text_by_message_id: &std::collections::HashMap<String, String>,
    current_attachment_text: &str,
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
    for message in messages {
        let mut message = message.clone();
        if let Some(attachment_text) = attachment_text_by_message_id.get(&message.id) {
            message.content = append_attachment_context(&message.content, attachment_text);
        }
        result.extend(native_context_messages(
            &message,
            exclude_reasoning_from_context,
        ));
    }
    result.push(ChatMessage {
        role: ChatRole::User,
        content: append_attachment_context(next_message, current_attachment_text),
    });
    result
}

fn append_attachment_context(message: &str, attachment_text: &str) -> String {
    if attachment_text.is_empty() {
        message.to_string()
    } else if message.is_empty() {
        attachment_text.to_string()
    } else {
        format!("{message}\n\n{attachment_text}")
    }
}

pub(crate) fn native_context_messages(
    message: &Message,
    exclude_reasoning_from_context: bool,
) -> Vec<ChatMessage> {
    let content = if message.role == MessageRole::Assistant
        && !exclude_reasoning_from_context
        && message
            .reasoning_content
            .as_deref()
            .is_some_and(|reasoning| !reasoning.is_empty())
    {
        format!(
            "<think>{}</think>{}",
            message.reasoning_content.as_deref().unwrap_or_default(),
            strip_reserved_attribution_prefix(&message.content)
        )
    } else if message.role == MessageRole::Assistant {
        strip_reserved_attribution_prefix(&message.content)
    } else {
        message.content.clone()
    };
    let native_message = ChatMessage {
        role: match message.role {
            MessageRole::System => ChatRole::System,
            MessageRole::User => ChatRole::User,
            MessageRole::Assistant => ChatRole::Assistant,
            MessageRole::Tool => ChatRole::Tool,
        },
        content,
    };
    let Some(attribution) = &message.attribution else {
        return vec![native_message];
    };
    vec![
        ChatMessage {
            role: ChatRole::System,
            content: format!(
                "Transcript metadata: the next assistant message was written by invited participant {} (@{}), not by this chat's default assistant. Preserve that distinction internally. Do not add or repeat a speaker label in a later answer unless the user explicitly asks for one.",
                attribution.label, attribution.handle
            ),
        },
        native_message,
    ]
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
    parse_reasoning: bool,
    on_event: &mut Option<F>,
) -> std::result::Result<Vec<GenerationOutput>, ChatTicketError>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    let started = Instant::now();
    let timeout = Duration::from_secs_f64(options.timeout_s.max(0.001));
    let mut reasoning_parser = parse_reasoning.then(ReasoningStreamParser::default);
    loop {
        if started.elapsed() >= timeout {
            ticket.cancel_all();
        }
        match ticket.events.recv_timeout(Duration::from_millis(20)) {
            Ok(event) => match event.event {
                GenerationEventKind::Delta { text } => {
                    if let Some(parser) = reasoning_parser.as_mut() {
                        for routed in parser.push(&text) {
                            emit(
                                on_event,
                                stream_event(
                                    request_id,
                                    conversation_id,
                                    match routed.target {
                                        ReasoningTarget::Content => "delta",
                                        ReasoningTarget::Reasoning => "reasoning_delta",
                                    },
                                    Some(routed.text),
                                    None,
                                    options,
                                ),
                            )
                            .map_err(ChatTicketError::Callback)?;
                        }
                    } else {
                        emit(
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
                        .map_err(ChatTicketError::Callback)?;
                    }
                }
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
    if let Some(parser) = reasoning_parser.as_mut() {
        for routed in parser.finish() {
            emit(
                on_event,
                stream_event(
                    request_id,
                    conversation_id,
                    match routed.target {
                        ReasoningTarget::Content => "delta",
                        ReasoningTarget::Reasoning => "reasoning_delta",
                    },
                    Some(routed.text),
                    None,
                    options,
                ),
            )
            .map_err(ChatTicketError::Callback)?;
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
        real_engine_invoked: !options.fake_fixture && event != "started",
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

#[cfg(test)]
mod tests {
    use super::{
        ChatAttachmentContext, Message, MessageRole, ReasoningStreamParser, ReasoningTarget,
        build_native_messages, native_context_messages, parse_reasoning_output, user_turn_is_empty,
    };
    use crate::conversation_store::{MessageAttribution, MessageSpeakerKind};

    fn assistant_message(reasoning: Option<&str>, content: &str) -> Message {
        Message {
            id: "assistant".to_string(),
            conversation_id: "conversation".to_string(),
            role: MessageRole::Assistant,
            content: content.to_string(),
            created_at: "1".to_string(),
            parent_id: None,
            model: Some("model.gguf".to_string()),
            receipt_id: None,
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_content: reasoning.map(str::to_string),
            reasoning_incomplete: false,
            branch_index: None,
            branch_count: None,
            attribution: None,
            attachment_ids: Vec::new(),
        }
    }

    #[test]
    fn reasoning_parser_routes_markers_split_across_stream_chunks() {
        let mut parser = ReasoningStreamParser::default();
        let mut routed = parser.push("Before <thi");
        routed.extend(parser.push("nk>private plan</th"));
        routed.extend(parser.push("ink>After"));
        routed.extend(parser.finish());
        assert_eq!(
            routed
                .iter()
                .filter(|delta| delta.target == ReasoningTarget::Content)
                .map(|delta| delta.text.as_str())
                .collect::<String>(),
            "Before After"
        );
        assert_eq!(
            routed
                .iter()
                .filter(|delta| delta.target == ReasoningTarget::Reasoning)
                .map(|delta| delta.text.as_str())
                .collect::<String>(),
            "private plan"
        );
    }

    #[test]
    fn reasoning_parser_records_incomplete_reasoning_without_losing_text() {
        let parsed = parse_reasoning_output("<think>unfinished thought", true);
        assert_eq!(parsed.content, "");
        assert_eq!(parsed.reasoning, "unfinished thought");
        assert!(parsed.incomplete);
    }

    #[test]
    fn disabling_reasoning_parsing_preserves_raw_model_output() {
        let parsed = parse_reasoning_output("<think>private</think>answer", false);
        assert_eq!(parsed.content, "<think>private</think>answer");
        assert_eq!(parsed.reasoning, "");
        assert!(!parsed.incomplete);
    }

    #[test]
    fn reasoning_context_policy_is_explicit() {
        let message = assistant_message(Some("private"), "answer");
        let included = build_native_messages(
            "",
            "",
            std::slice::from_ref(&message),
            "next",
            false,
            &Default::default(),
            "",
        );
        assert_eq!(included[0].content, "<think>private</think>answer");
        let excluded =
            build_native_messages("", "", &[message], "next", true, &Default::default(), "");
        assert_eq!(excluded[0].content, "answer");
    }

    #[test]
    fn attributed_speaker_identity_is_metadata_not_assistant_prose() {
        let mut message = assistant_message(
            None,
            "Response from @default-chat: Structurally attributed answer",
        );
        message.attribution = Some(MessageAttribution {
            kind: MessageSpeakerKind::LiveChat,
            source_id: "source".to_string(),
            handle: "default-chat".to_string(),
            label: "Default chat".to_string(),
            version: 1,
            invocation_id: "invocation".to_string(),
            target_order: 0,
        });

        let context = native_context_messages(&message, false);
        assert_eq!(context.len(), 2);
        assert_eq!(context[0].role, llama_native_types::ChatRole::System);
        assert!(context[0].content.contains("Transcript metadata"));
        assert!(!context[0].content.contains("Response from @"));
        assert_eq!(context[1].role, llama_native_types::ChatRole::Assistant);
        assert_eq!(context[1].content, "Structurally attributed answer");
    }

    #[test]
    fn attachment_only_turns_are_valid_but_empty_turns_are_not() {
        let empty = ChatAttachmentContext {
            staged_ids: Vec::new(),
            text_by_message_id: Default::default(),
            current_text: String::new(),
            media: Vec::new(),
        };
        assert!(user_turn_is_empty("   ", &empty));

        let text_attachment = ChatAttachmentContext {
            current_text: "[BEGIN UNTRUSTED ATTACHMENT DATA]".to_string(),
            ..empty.clone()
        };
        assert!(!user_turn_is_empty("", &text_attachment));

        let media_attachment = ChatAttachmentContext {
            media: vec![llama_native_types::MediaInput {
                id: "image".to_string(),
                kind: llama_native_types::MediaKind::Image,
                mime: "image/png".to_string(),
                sha256: "fixture".to_string(),
                bytes: vec![1],
            }],
            ..empty
        };
        assert!(!user_turn_is_empty("", &media_attachment));
    }
}
