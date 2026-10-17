use crate::attachments::{
    ChatAttachmentContext, commit_generated_exchange, prepare_call_greeting_history,
    prepare_chat_attachments, prepare_live_audio_commit, prepare_voice_reply_attachments,
};
use crate::config::{normalize_optional_path, resolve_settings, upstream_setting_string};
use crate::conversation_store::{
    ChatTemplatePolicy, Message, MessageRole, active_leaf_id, get_or_create_conversation, load_db,
    load_db_with_settings, strip_reserved_attribution_prefix,
};
use crate::kv_cache::{
    compatible_cached_prefix_for_owner_with_settings,
    compatible_memory_prefix_for_owner_with_settings, invalidate_cache, invalidate_memory,
    note_committed_session_request, schedule_committed_session_checkpoint,
};
use crate::native_runtime::{resident_model_for_profile, resident_model_for_profile_with_status};
use crate::now_ms;
use crate::operation_scope::{ChatOperationLease, OperationScope};
use crate::receipts::{Blocker, CommandResult};
use crate::skill_store::{applied_skill_prompt, persona_prefix_cache_owner_id};
use anyhow::Result;
use llama_native_engine::{
    GenerationTicket, IncrementalAudioCancellation, IncrementalAudioGeneration,
    IncrementalAudioMonitor, IncrementalAudioResume, NativeModelHandle,
};
use llama_native_types::{
    ChatMessage, ChatRole, ChatTemplateChoice, GenerationCacheMetrics, GenerationEventKind,
    GenerationInput, GenerationOutput, GenerationRequest, GenerationState, NativeError,
    NativeErrorCode, SequenceStateBlob,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub struct VoiceAudioSession {
    request_id: String,
    prepared: std::sync::Arc<VoiceAudioPreparedTurn>,
    operation: ChatOperationLease,
    generation: IncrementalAudioGeneration,
    begin_metrics: VoiceAudioBeginMetrics,
}

/// Immutable preparation for one microphone turn.
///
/// Silence speculation may be belayed several times before the user actually
/// yields the floor. Retaining this object keeps settings, history, prompt,
/// model acquisition, and prefix lookup off every retry while the eventual
/// commit still checks the exact leaf captured here.
#[derive(Debug)]
pub struct VoiceAudioPreparedTurn {
    resume_key: String,
    conversation_id: String,
    message: String,
    db: crate::conversation_store::ConversationDb,
    conversation: crate::conversation_store::Conversation,
    expected_active_leaf: Option<String>,
    settings: crate::Settings,
    model_path: PathBuf,
    parse_reasoning: bool,
    handle: NativeModelHandle,
    request_template: GenerationRequest,
    cache_id: Option<String>,
    preparation_metrics: VoiceAudioBeginMetrics,
}

impl VoiceAudioPreparedTurn {
    #[must_use]
    pub fn matches_turn(&self, conversation_id: &str, message: &str) -> bool {
        self.conversation_id == conversation_id && self.message == message
    }
}

impl VoiceAudioSession {
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    #[must_use]
    pub fn conversation_id(&self) -> &str {
        &self.prepared.conversation_id
    }

    /// Returns the exact immutable persona identity loaded with this turn.
    /// Speech can bind its resident voice while microphone/model preparation
    /// is already in flight instead of reopening the conversation store when
    /// the first generated text reaches TTS.
    #[must_use]
    pub fn persona_identity(&self) -> crate::conversation_store::ConversationPersonaIdentity {
        crate::conversation_store::ConversationPersonaIdentity {
            recipient_ids: self.prepared.conversation.recipient_ids.clone(),
            source_conversation_id: self.prepared.conversation.source_conversation_id.clone(),
        }
    }

    pub fn push_pcm(&self, samples: &[f32]) -> Result<usize> {
        self.generation
            .push_pcm(samples)
            .map_err(anyhow::Error::new)
    }

    pub fn seal_input(&self) -> Result<bool> {
        self.generation.seal_input().map_err(anyhow::Error::new)
    }

    pub fn cancel(&self) -> usize {
        self.generation.cancel()
    }

    #[must_use]
    pub fn cancellation_handle(&self) -> Option<IncrementalAudioCancellation> {
        self.generation.cancellation_handle()
    }

    pub fn wait_for_resume(&self) -> Result<IncrementalAudioResume> {
        self.generation
            .wait_for_resume()
            .map_err(anyhow::Error::new)
    }

    #[must_use]
    pub fn audio_monitor(&self) -> IncrementalAudioMonitor {
        self.generation.monitor()
    }

    #[must_use]
    pub fn begin_metrics(&self) -> &VoiceAudioBeginMetrics {
        &self.begin_metrics
    }

    #[must_use]
    pub fn prepared_turn(&self) -> std::sync::Arc<VoiceAudioPreparedTurn> {
        std::sync::Arc::clone(&self.prepared)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatSendInput {
    pub conversation_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ChatSendOptions {
    /// Maximum lack of generation progress, excluding consumer backpressure.
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

/// Owns the stream interval after a caller has observed `started`.
///
/// All exits from generation, including `?`, early blocked results, and
/// unwinding, cross this guard. Its destructor is deliberately best-effort:
/// the original error remains authoritative, while the live lease is released
/// and the stream receives a terminal failure whenever its callback is usable.
struct ChatStreamLifecycle<'a, F>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    request_id: String,
    conversation_id: String,
    options: ChatSendOptions,
    on_event: &'a mut Option<F>,
    operation: ChatOperationLease,
    terminal_emitted: bool,
    real_engine_invoked: bool,
}

impl<'a, F> ChatStreamLifecycle<'a, F>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    fn new(
        request_id: &str,
        conversation_id: &str,
        options: ChatSendOptions,
        on_event: &'a mut Option<F>,
        operation: ChatOperationLease,
    ) -> Self {
        Self {
            request_id: request_id.to_string(),
            conversation_id: conversation_id.to_string(),
            options,
            on_event,
            operation,
            terminal_emitted: false,
            real_engine_invoked: false,
        }
    }

    fn callback(&mut self) -> &mut Option<F> {
        self.on_event
    }

    fn mark_engine_invoked(&mut self) {
        self.real_engine_invoked = true;
    }

    fn cancellation_requested(&self) -> bool {
        self.operation.cancellation_requested()
    }

    fn arbitrate_terminal(&self) -> bool {
        self.operation.arbitrate_terminal()
    }

    fn with_native_admission<R>(&self, admit: impl FnOnce() -> R) -> Result<Option<R>> {
        self.operation.with_native_admission(admit)
    }

    fn finish_cancelled(&mut self) -> Result<CommandResult<ChatSendOutput>> {
        self.operation.request_cancel();
        self.operation.arbitrate_terminal();
        self.finish(
            "cancelled",
            Some("The local model request was cancelled.".to_string()),
        )?;
        Ok(CommandResult::blocked_with_evidence(
            "mom_llama.chat_send",
            "stub_blocked",
            Blocker::new(
                "chat_cancelled",
                "The local model request was cancelled.",
                vec!["Send the message again to retry.".to_string()],
            ),
            Vec::new(),
            Vec::new(),
            self.real_engine_invoked,
            self.options.fake_fixture,
        ))
    }

    fn finish(&mut self, event: &'static str, message: Option<String>) -> Result<()> {
        assert!(
            matches!(event, "completed" | "cancelled" | "failed"),
            "a chat stream lifecycle can finish only with a terminal event"
        );
        assert!(
            !self.terminal_emitted,
            "a chat stream lifecycle must emit exactly one terminal event"
        );
        self.terminal_emitted = true;
        let mut terminal = stream_event(
            &self.request_id,
            &self.conversation_id,
            event,
            None,
            message,
            self.options,
        );
        terminal.real_engine_invoked = self.real_engine_invoked;
        emit(self.on_event, terminal)
    }
}

impl<F> Drop for ChatStreamLifecycle<'_, F>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    fn drop(&mut self) {
        if self.terminal_emitted {
            return;
        }
        if self.operation.arbitrate_terminal() {
            let _ = self.finish(
                "cancelled",
                Some("The local model request was cancelled.".to_string()),
            );
        } else {
            let _ = self.finish(
                "failed",
                Some("Generation ended before producing a completed response.".to_string()),
            );
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
    #[serde(default)]
    pub first_token_ms: Option<u128>,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub cache_id: Option<String>,
    pub cache_reused: bool,
    #[serde(default)]
    pub cache_metrics: GenerationCacheMetrics,
    /// Present only for an audio-native turn whose microphone attachment was
    /// persisted concurrently with generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_attachment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_audio_metrics: Option<VoiceAudioMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceAudioMetrics {
    pub begin: VoiceAudioBeginMetrics,
    pub accepted_samples: u64,
    pub consumed_samples: u64,
    pub chunks: u64,
    pub high_water_samples: usize,
    pub first_accept_ms: Option<u64>,
    pub first_read_ms: Option<u64>,
    pub finish_ms: Option<u64>,
    pub decode_start_ms: Option<u64>,
    pub prefix_start_ms: Option<u64>,
    pub prefix_finish_ms: Option<u64>,
    pub prefix_ms: u64,
    pub prefix_tokens: usize,
    pub restored_prefix_tokens: usize,
    pub projector_ms: u64,
    pub projected_blocks: u64,
    pub projected_samples: u64,
    pub first_projector_start_ms: Option<u64>,
    pub first_projector_finish_ms: Option<u64>,
    pub last_projector_finish_ms: Option<u64>,
    pub projector_max_ms: u64,
    pub tail_samples: u64,
    #[serde(default)]
    pub tail_projector_ms: u64,
    pub producer_lock_wait_us: u64,
    pub producer_lock_wait_max_us: u64,
    pub ring_write_us: u64,
    pub ring_write_max_us: u64,
    pub archive_copy_us: u64,
    pub archive_copy_max_us: u64,
    pub consumer_lock_wait_us: u64,
    pub consumer_lock_wait_max_us: u64,
    pub ring_read_us: u64,
    pub ring_read_max_us: u64,
    #[serde(default)]
    pub reader_notifications: u64,
    #[serde(default)]
    pub reader_waits: u64,
    #[serde(default)]
    pub reader_wakeups: u64,
    pub first_accept_us: Option<u64>,
    pub first_read_us: Option<u64>,
    pub finish_us: Option<u64>,
    pub decode_start_us: Option<u64>,
    #[serde(default)]
    pub closing_boundary_finish_us: Option<u64>,
    #[serde(default)]
    pub suffix_tokenize_finish_us: Option<u64>,
    #[serde(default)]
    pub suffix_eval_finish_us: Option<u64>,
    #[serde(default)]
    pub sampler_start_us: Option<u64>,
    #[serde(default)]
    pub sampler_ready_us: Option<u64>,
    #[serde(default)]
    pub first_sample_start_us: Option<u64>,
    #[serde(default)]
    pub first_sample_finish_us: Option<u64>,
    #[serde(default)]
    pub first_token_event_us: Option<u64>,
    #[serde(default)]
    pub sampled_tokens: u64,
    #[serde(default)]
    pub nonempty_token_pieces: u64,
    #[serde(default)]
    pub emitted_token_events: u64,
    #[serde(default)]
    pub last_sample_start_us: Option<u64>,
    #[serde(default)]
    pub last_sample_finish_us: Option<u64>,
    #[serde(default)]
    pub last_token_event_us: Option<u64>,
    #[serde(default)]
    pub continuation_decode_start_us: Option<u64>,
    #[serde(default)]
    pub continuation_decode_finish_us: Option<u64>,
    #[serde(default)]
    pub continuation_decode_steps: u64,
    #[serde(default)]
    pub continuation_decode_us: u64,
    #[serde(default)]
    pub continuation_decode_max_us: u64,
    pub prefix_start_us: Option<u64>,
    pub prefix_finish_us: Option<u64>,
    pub prefix_us: u64,
    pub projector_us: u64,
    pub first_projector_start_us: Option<u64>,
    pub first_projector_finish_us: Option<u64>,
    pub last_projector_finish_us: Option<u64>,
    pub projector_max_us: u64,
    #[serde(default)]
    pub tail_projector_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceAudioBeginMetrics {
    pub measured_prompt_tokens: usize,
    pub omitted_history_messages: usize,
    pub total_us: u64,
    /// Context cells allocated by the exact resident used for this turn.
    /// This is reported from the live fingerprint rather than a persisted
    /// preference, so latency receipts prove the effective memory plan.
    pub resident_context_tokens: u32,
    pub settings_cache_hit: bool,
    pub model_resident_hit: bool,
    pub settings_us: u64,
    pub conversation_us: u64,
    pub history_us: u64,
    pub attachments_us: u64,
    pub skills_us: u64,
    pub prompt_us: u64,
    pub model_acquire_us: u64,
    pub cache_lookup_us: u64,
    pub sampling_us: u64,
    pub operation_register_us: u64,
    pub generation_admission_us: u64,
    pub cached_prefix_tokens: usize,
    pub cached_prefix_found: bool,
    pub existing_conversation_fast_path: bool,
    pub attachment_free_fast_path: bool,
    pub skill_free_fast_path: bool,
    pub prepared_reuse: bool,
    pub preparation_source_us: u64,
    pub request_clone_us: u64,
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().try_into().unwrap_or(u64::MAX)
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
    /// Native emission clock for latency milestones. Ordinary deltas omit it
    /// after the first content delta to avoid timing work per token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emitted_at_unix_us: Option<u64>,
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

pub fn chat_send_in_scope(
    scope: &OperationScope,
    input: ChatSendInput,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatSendOutput>> {
    chat_send_supervised(
        scope,
        input,
        options,
        None,
        None::<fn(ChatStreamEvent) -> Result<()>>,
    )
}

/// Start one audio-native turn while the microphone is still open.
///
/// This deliberately excludes the not-yet-finalized voice attachment from
/// storage preparation. The exact WAV is admitted and committed only at
/// `voice_audio_finish_in_scope`; the resident model receives these same
/// samples incrementally through `push_pcm` in the meantime.
pub fn voice_audio_begin_in_scope(
    scope: &OperationScope,
    conversation_id: &str,
    message: &str,
) -> Result<VoiceAudioSession> {
    let total_started = Instant::now();
    let stage_started = Instant::now();
    let (mut settings, settings_cache_hit) = crate::config::resolve_settings_with_cache_status()?;
    let settings_us = elapsed_us(stage_started);

    // Voice capture always starts from a chat already visible in the UI. Do
    // not acquire a write transaction merely to rediscover that unchanged
    // record. A genuinely absent conversation still uses the guarded creation
    // path, including removed-persona and concurrent-removal checks.
    let stage_started = Instant::now();
    let loaded = load_db_with_settings(&settings)?;
    let existing = loaded
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
        .cloned();
    let existing_conversation_fast_path = existing.is_some();
    let (db, mut conversation) = match existing {
        Some(conversation) => (loaded, conversation),
        None => get_or_create_conversation(conversation_id)?,
    };
    let conversation_us = elapsed_us(stage_started);
    let expected_active_leaf = conversation.active_leaf_message_id.clone();
    apply_conversation_model_pair(
        &mut settings,
        conversation.execution_profile.model_path.clone(),
        conversation.selected_model_path.clone(),
        conversation.execution_profile.mmproj_path.clone(),
    );
    let stage_started = Instant::now();
    let active_messages = crate::document::checked_active_messages(&conversation)?;
    let history_us = elapsed_us(stage_started);

    let attachment_free_fast_path = active_messages
        .iter()
        .all(|message| message.attachment_ids.is_empty());
    let skill_free_fast_path = conversation.current_skill_ids.is_empty();
    let stage_started = Instant::now();
    let skill_prompt = applied_skill_prompt(&conversation.current_skill_ids)?;
    let skills_us = elapsed_us(stage_started);
    let stage_started = Instant::now();
    let system_message = conversation
        .execution_profile
        .system_message
        .clone()
        .unwrap_or_else(|| upstream_setting_string(&settings, "systemMessage").unwrap_or_default());
    let parse_reasoning = !upstream_setting_bool(&settings, "disableReasoningParsing");
    let prompt_us = elapsed_us(stage_started);
    let model_path = settings.model_path.clone().unwrap_or_default();
    let stage_started = Instant::now();
    let (handle, model_resident_hit) = resident_model_for_profile_with_status(
        scope,
        &settings,
        &model_path,
        settings.mmproj_path.as_deref(),
    )
    .map_err(|blocked| anyhow::anyhow!(blocked.blocker.message))?;
    let model_acquire_us = elapsed_us(stage_started);
    // Keep the live microphone marker at the current user-message boundary.
    // The stored Message.content remains the user-facing call/note prompt;
    // later history replaces this placeholder with its verified derived-audio
    // occurrence at the same boundary, preserving text-prefix identity.
    let projected_current_message = format!("{message}\n\n[[native-media:live-microphone]]");
    let resident_context_tokens = handle
        .status()
        .fingerprint
        .as_ref()
        .map_or(settings.context_tokens, |fingerprint| {
            fingerprint.context_tokens
        });
    let stage_started = Instant::now();
    let projection = crate::voice_context::fit_voice_context(
        &handle,
        &mut conversation,
        &active_messages,
        crate::voice_context::VoicePrompt {
            settings: &settings,
            system: &system_message,
            skill: &skill_prompt.prompt,
            current: &projected_current_message,
            template: ChatTemplateChoice::PreferNonThinking,
            completion_tokens: 96,
            mode: crate::voice_context::VoiceHistoryMode::Live,
        },
    )?;
    let attachments_us = elapsed_us(stage_started);
    let measured_prompt_tokens = projection.prompt_tokens;
    let omitted_history_messages = projection.omitted_history_messages;
    let messages = projection.messages;
    let history = projection.attachments;
    let stage_started = Instant::now();
    // Historical audio is evaluated after the leading text prefix. Its
    // presence must not prevent reuse of the exact persona/conversation KV
    // shared by both the cached request and this live microphone turn.
    let owner_ids = reusable_prefix_owner_ids(
        &conversation.id,
        conversation.source_conversation_id.as_deref(),
        &conversation.current_skill_ids,
        skill_prompt.cache_owner_id.is_some(),
        skill_prompt.cache_owner_id.as_deref(),
    );
    let (cached_prefix, resident_or_cached_prefix_tokens) = longest_cached_prefix_for_owners(
        &handle,
        &messages,
        &owner_ids,
        &settings,
        true,
        PrefixLookupMode::LiveVoice,
        ChatTemplateChoice::PreferNonThinking,
    )?;
    let cache_lookup_us = elapsed_us(stage_started);
    let cached_prefix_tokens = resident_or_cached_prefix_tokens;
    let cached_prefix_found = cached_prefix_tokens > 0;
    let cache_id = cached_prefix.as_ref().map(|(id, _)| id.clone());
    let cached_prefix = cached_prefix.map(|(_, sequence)| sequence);
    let stage_started = Instant::now();
    let mut sampling =
        settings.sampling_for_profile(conversation.execution_profile.sampling.as_ref())?;
    let phone_call = message.starts_with("[Phone call]");
    // This entry point is used only by live calls and voice notes. Classify
    // the non-call case explicitly so debug builds do not reject the voice
    // note path and release builds do not silently retain text-chat sampling.
    configure_voice_sampling(&mut sampling, phone_call, !phone_call);
    let sampling_us = elapsed_us(stage_started);
    let request_template = GenerationRequest {
        request_id: String::new(),
        model_id: handle.status().model_id,
        input: GenerationInput::Chat {
            messages,
            // Voice latency wins over an optional reasoning channel. The
            // engine applies a verified non-thinking contract where one
            // exists and otherwise preserves the model default.
            template: ChatTemplateChoice::PreferNonThinking,
        },
        sampling,
        // Pending older recordings remain exact model context until a final
        // transcript replaces each one. The incremental engine evaluates
        // these before the live microphone marker.
        media: history.media,
        cached_prefix,
    };
    let preparation_source_us = elapsed_us(total_started);
    let preparation_metrics = VoiceAudioBeginMetrics {
        measured_prompt_tokens,
        omitted_history_messages,
        total_us: 0,
        resident_context_tokens,
        settings_cache_hit,
        model_resident_hit,
        settings_us,
        conversation_us,
        history_us,
        attachments_us,
        skills_us,
        prompt_us,
        model_acquire_us,
        cache_lookup_us,
        sampling_us,
        operation_register_us: 0,
        generation_admission_us: 0,
        cached_prefix_tokens,
        cached_prefix_found,
        existing_conversation_fast_path,
        attachment_free_fast_path,
        skill_free_fast_path,
        prepared_reuse: false,
        preparation_source_us,
        request_clone_us: 0,
    };
    let prepared = std::sync::Arc::new(VoiceAudioPreparedTurn {
        resume_key: Uuid::new_v4().to_string(),
        conversation_id: conversation_id.to_string(),
        message: message.to_string(),
        db,
        conversation,
        expected_active_leaf,
        settings,
        model_path,
        parse_reasoning,
        handle,
        request_template,
        cache_id,
        preparation_metrics,
    });
    start_prepared_voice_audio(scope, prepared, false, total_started)
}

/// Restart the exact prepared microphone turn after resumed speech belays an
/// earlier speculative decode. Only cheap request identity, cancellation
/// registration, and worker admission remain on this path.
pub fn voice_audio_restart_prepared_in_scope(
    scope: &OperationScope,
    prepared: std::sync::Arc<VoiceAudioPreparedTurn>,
) -> Result<VoiceAudioSession> {
    start_prepared_voice_audio(scope, prepared, true, Instant::now())
}

fn start_prepared_voice_audio(
    scope: &OperationScope,
    prepared: std::sync::Arc<VoiceAudioPreparedTurn>,
    reused: bool,
    total_started: Instant,
) -> Result<VoiceAudioSession> {
    let request_id = Uuid::new_v4().to_string();
    let clone_started = Instant::now();
    let mut request = prepared.request_template.clone();
    request.request_id.clone_from(&request_id);
    let request_clone_us = elapsed_us(clone_started);
    let stage_started = Instant::now();
    let operation = scope.register_chat(&request_id, &prepared.conversation_id)?;
    let operation_register_us = elapsed_us(stage_started);
    let stage_started = Instant::now();
    let generation = prepared
        .handle
        .generate_incremental_audio_resumable(request, Some(prepared.resume_key.clone()))
        .map_err(anyhow::Error::new)?;
    let generation_admission_us = elapsed_us(stage_started);
    let mut begin_metrics = prepared.preparation_metrics.clone();
    if reused {
        begin_metrics.settings_us = 0;
        begin_metrics.conversation_us = 0;
        begin_metrics.history_us = 0;
        begin_metrics.attachments_us = 0;
        begin_metrics.skills_us = 0;
        begin_metrics.prompt_us = 0;
        begin_metrics.model_acquire_us = 0;
        begin_metrics.cache_lookup_us = 0;
        begin_metrics.sampling_us = 0;
    }
    begin_metrics.prepared_reuse = reused;
    begin_metrics.request_clone_us = request_clone_us;
    begin_metrics.operation_register_us = operation_register_us;
    begin_metrics.generation_admission_us = generation_admission_us;
    begin_metrics.total_us = elapsed_us(total_started);
    Ok(VoiceAudioSession {
        request_id,
        prepared,
        operation,
        generation,
        begin_metrics,
    })
}

pub fn voice_audio_finish_in_scope<F>(
    session: VoiceAudioSession,
    options: ChatSendOptions,
    on_event: Option<F>,
) -> Result<CommandResult<ChatSendOutput>>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    voice_audio_finish_with_precommit_in_scope(session, options, on_event, || Ok(None))
}

/// Finish an audio-native generation while allowing the caller's already
/// running microphone persistence task to join immediately before the
/// generated exchange is committed. This keeps attachment I/O under model
/// decoding without weakening the exact attachment/message transaction.
pub fn voice_audio_finish_with_precommit_in_scope<F, P>(
    session: VoiceAudioSession,
    options: ChatSendOptions,
    mut on_event: Option<F>,
    before_commit: P,
) -> Result<CommandResult<ChatSendOutput>>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
    P: FnOnce() -> Result<Option<String>>,
{
    let prepared = std::sync::Arc::clone(&session.prepared);
    let audio_monitor = session.generation.monitor();
    let ticket = session.generation.finish().map_err(anyhow::Error::new)?;
    let mut stream = ChatStreamLifecycle::new(
        &session.request_id,
        &prepared.conversation_id,
        options,
        &mut on_event,
        session.operation,
    );
    stream.mark_engine_invoked();
    emit(
        stream.callback(),
        stream_event(
            &session.request_id,
            &prepared.conversation_id,
            "started",
            None,
            Some("Streaming response from the resident audio model.".to_string()),
            options,
        ),
    )?;
    let outputs = consume_chat_ticket(
        ticket,
        &session.request_id,
        &prepared.conversation_id,
        options,
        prepared.parse_reasoning,
        stream.callback(),
    )
    .map_err(chat_ticket_error)?;
    let Some(output) = outputs.into_iter().next() else {
        return Ok(CommandResult::blocked(
            "mom_llama.voice_audio_finish",
            "blocked_native_runtime",
            Blocker::new(
                "native_response_missing",
                "The native model returned no response.",
                vec!["Try the request again.".to_string()],
            ),
        ));
    };
    let audio_stats = audio_monitor.stats();
    if output.state == GenerationState::Cancelled || stream.arbitrate_terminal() {
        return stream.finish_cancelled();
    }
    let parsed = parse_reasoning_output(&output.text, prepared.parse_reasoning);
    let assistant_text = strip_reserved_attribution_prefix(&parsed.content)
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if assistant_text.is_empty() {
        return Ok(empty_native_response_result(
            (!parsed.reasoning.is_empty()).then_some(parsed.reasoning.as_str()),
            false,
        ));
    }
    let voice_attachment_id = before_commit()?;
    let mut conversation = prepared.conversation.clone();
    let active_messages = crate::document::checked_active_messages(&conversation)?;
    let attachments = prepare_live_audio_commit(&prepared.conversation_id, &active_messages)?
        .map_err(|blocked| anyhow::anyhow!(blocked.blocker.message))?;
    let user_message = Message {
        id: Uuid::new_v4().to_string(),
        conversation_id: prepared.conversation_id.clone(),
        role: MessageRole::User,
        content: prepared.message.clone(),
        created_at: now_ms().to_string(),
        parent_id: active_leaf_id(&conversation),
        model: model_name(&prepared.model_path),
        receipt_id: None,
        prompt_tokens: Some(output.metrics.prompt_tokens),
        completion_tokens: None,
        reasoning_content: None,
        reasoning_incomplete: false,
        branch_index: None,
        branch_count: None,
        attribution: None,
        attachment_ids: attachments.staged_ids.clone(),
    };
    let user_message_id = user_message.id.clone();
    let assistant_message = Message {
        id: Uuid::new_v4().to_string(),
        conversation_id: prepared.conversation_id.clone(),
        role: MessageRole::Assistant,
        content: assistant_text.clone(),
        created_at: now_ms().to_string(),
        parent_id: Some(user_message_id.clone()),
        model: model_name(&prepared.model_path),
        receipt_id: Some(format!(
            "mom_llama.voice_audio_finish:{}",
            session.request_id
        )),
        prompt_tokens: None,
        completion_tokens: Some(output.metrics.completion_tokens),
        reasoning_content: (!parsed.reasoning.is_empty()).then_some(parsed.reasoning.clone()),
        reasoning_incomplete: parsed.incomplete,
        branch_index: None,
        branch_count: None,
        attribution: None,
        attachment_ids: Vec::new(),
    };
    let assistant_message_id = assistant_message.id.clone();
    conversation.messages.push(user_message);
    conversation.messages.push(assistant_message);
    conversation.active_leaf_message_id = Some(assistant_message_id.clone());
    conversation.updated_at = now_ms().to_string();
    conversation.selected_model_path = prepared.settings.model_path.clone();
    conversation.execution_profile.model_path = prepared.settings.model_path.clone();
    conversation.execution_profile.mmproj_path = prepared.settings.mmproj_path.clone();
    let generated = vec![user_message_id.clone(), assistant_message_id.clone()];
    let path = commit_generated_exchange(
        prepared.db.clone(),
        conversation,
        prepared.expected_active_leaf.as_deref(),
        &generated,
        &attachments.staged_ids,
        &user_message_id,
        attachments.draft_snapshot.as_ref(),
    )?;
    note_committed_session_request(&prepared.conversation_id, &session.request_id);
    let cache_reused = prefix_cells_reused(&output.metrics.cache);
    let result = ChatSendOutput {
        request_id: session.request_id,
        conversation_id: prepared.conversation_id.clone(),
        user_message_id,
        assistant_message_id,
        assistant_text,
        reasoning_content: (!parsed.reasoning.is_empty()).then_some(parsed.reasoning),
        reasoning_incomplete: parsed.incomplete,
        model_path: prepared.model_path.display().to_string(),
        duration_ms: output.metrics.duration_ms,
        first_token_ms: output.metrics.first_token_ms,
        prompt_tokens: output.metrics.prompt_tokens,
        completion_tokens: output.metrics.completion_tokens,
        cache_id: prepared.cache_id.clone().filter(|_| cache_reused),
        cache_reused,
        cache_metrics: output.metrics.cache.clone(),
        voice_attachment_id,
        voice_audio_metrics: Some(VoiceAudioMetrics {
            begin: session.begin_metrics,
            accepted_samples: audio_stats.accepted_samples,
            consumed_samples: audio_stats.consumed_samples,
            chunks: audio_stats.chunks,
            high_water_samples: audio_stats.high_water_samples,
            first_accept_ms: audio_stats.first_accept_ms,
            first_read_ms: audio_stats.first_read_ms,
            finish_ms: audio_stats.finish_ms,
            decode_start_ms: audio_stats.decode_start_ms,
            prefix_start_ms: audio_stats.prefix_start_ms,
            prefix_finish_ms: audio_stats.prefix_finish_ms,
            prefix_ms: audio_stats.prefix_ms,
            prefix_tokens: audio_stats.prefix_tokens,
            restored_prefix_tokens: audio_stats.restored_prefix_tokens,
            projector_ms: audio_stats.projector_ms,
            projected_blocks: audio_stats.projected_blocks,
            projected_samples: audio_stats.projected_samples,
            first_projector_start_ms: audio_stats.first_projector_start_ms,
            first_projector_finish_ms: audio_stats.first_projector_finish_ms,
            last_projector_finish_ms: audio_stats.last_projector_finish_ms,
            projector_max_ms: audio_stats.projector_max_ms,
            tail_samples: audio_stats.tail_samples,
            tail_projector_ms: audio_stats.tail_projector_ms,
            producer_lock_wait_us: audio_stats.producer_lock_wait_us,
            producer_lock_wait_max_us: audio_stats.producer_lock_wait_max_us,
            ring_write_us: audio_stats.ring_write_us,
            ring_write_max_us: audio_stats.ring_write_max_us,
            archive_copy_us: audio_stats.archive_copy_us,
            archive_copy_max_us: audio_stats.archive_copy_max_us,
            consumer_lock_wait_us: audio_stats.consumer_lock_wait_us,
            consumer_lock_wait_max_us: audio_stats.consumer_lock_wait_max_us,
            ring_read_us: audio_stats.ring_read_us,
            ring_read_max_us: audio_stats.ring_read_max_us,
            reader_notifications: audio_stats.reader_notifications,
            reader_waits: audio_stats.reader_waits,
            reader_wakeups: audio_stats.reader_wakeups,
            first_accept_us: audio_stats.first_accept_us,
            first_read_us: audio_stats.first_read_us,
            finish_us: audio_stats.finish_us,
            decode_start_us: audio_stats.decode_start_us,
            closing_boundary_finish_us: audio_stats.closing_boundary_finish_us,
            suffix_tokenize_finish_us: audio_stats.suffix_tokenize_finish_us,
            suffix_eval_finish_us: audio_stats.suffix_eval_finish_us,
            sampler_start_us: audio_stats.sampler_start_us,
            sampler_ready_us: audio_stats.sampler_ready_us,
            first_sample_start_us: audio_stats.first_sample_start_us,
            first_sample_finish_us: audio_stats.first_sample_finish_us,
            first_token_event_us: audio_stats.first_token_event_us,
            sampled_tokens: audio_stats.sampled_tokens,
            nonempty_token_pieces: audio_stats.nonempty_token_pieces,
            emitted_token_events: audio_stats.emitted_token_events,
            last_sample_start_us: audio_stats.last_sample_start_us,
            last_sample_finish_us: audio_stats.last_sample_finish_us,
            last_token_event_us: audio_stats.last_token_event_us,
            continuation_decode_start_us: audio_stats.continuation_decode_start_us,
            continuation_decode_finish_us: audio_stats.continuation_decode_finish_us,
            continuation_decode_steps: audio_stats.continuation_decode_steps,
            continuation_decode_us: audio_stats.continuation_decode_us,
            continuation_decode_max_us: audio_stats.continuation_decode_max_us,
            prefix_start_us: audio_stats.prefix_start_us,
            prefix_finish_us: audio_stats.prefix_finish_us,
            prefix_us: audio_stats.prefix_us,
            projector_us: audio_stats.projector_us,
            first_projector_start_us: audio_stats.first_projector_start_us,
            first_projector_finish_us: audio_stats.first_projector_finish_us,
            last_projector_finish_us: audio_stats.last_projector_finish_us,
            projector_max_us: audio_stats.projector_max_us,
            tail_projector_us: audio_stats.tail_projector_us,
        }),
    };
    stream.finish("completed", Some(result.assistant_text.clone()))?;
    Ok(CommandResult::passed(
        "mom_llama.voice_audio_finish",
        "real_prompt_smoke_passed",
        result,
        vec![path.display().to_string()],
        Vec::new(),
        true,
        false,
    ))
}

pub fn chat_send_stream_in_scope<F>(
    scope: &OperationScope,
    input: ChatSendInput,
    options: ChatSendOptions,
    on_event: F,
) -> Result<CommandResult<ChatSendOutput>>
where
    F: FnMut(ChatStreamEvent) -> Result<()>,
{
    chat_send_supervised(scope, input, options, None, Some(on_event))
}

pub fn chat_regenerate_in_scope(
    scope: &OperationScope,
    conversation_id: &str,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatSendOutput>> {
    let db = load_db()?;
    let message = match db
        .conversations
        .iter()
        .find(|conversation| conversation.id == conversation_id)
    {
        Some(conversation) => crate::document::checked_active_messages(conversation)?
            .into_iter()
            .rev()
            .find(|message| message.role == MessageRole::User),
        None => None,
    };
    let Some(message) = message else {
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
        scope,
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

pub fn chat_continue_in_scope(
    scope: &OperationScope,
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
    let mut result = chat_send_in_scope(
        scope,
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
    scope: &OperationScope,
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
    apply_conversation_model_pair(
        &mut settings,
        conversation.execution_profile.model_path.clone(),
        conversation.selected_model_path.clone(),
        conversation.execution_profile.mmproj_path.clone(),
    );
    let active_messages = crate::document::checked_active_messages(&conversation)?;
    let voice_reply = regenerate_user_id.is_none()
        && (input.message.starts_with("[Voice note]") || input.message.starts_with("[Phone call]"));
    let call_greeting = input.message.starts_with("[Phone call] [Greeting]");
    let skill_prompt = applied_skill_prompt(&conversation.current_skill_ids)?;
    let system_message = conversation
        .execution_profile
        .system_message
        .clone()
        .unwrap_or_else(|| upstream_setting_string(&settings, "systemMessage").unwrap_or_default());
    let model_path = settings.model_path.clone().unwrap_or_default();
    let request_id = Uuid::new_v4().to_string();
    if !voice_reply && !options.fake_fixture {
        crate::kv_cache::checkpoint_pending_sessions_before_chat(scope, &conversation.id);
    }
    let operation = scope.register_chat(&request_id, &conversation.id)?;
    let voice_handle = if voice_reply && !options.fake_fixture {
        match resident_model_for_profile(
            scope,
            &settings,
            &model_path,
            settings.mmproj_path.as_deref(),
        ) {
            Ok(handle) => Some(handle),
            Err(blocked) => {
                return Ok(CommandResult::blocked(
                    "mom_llama.chat_send",
                    &blocked.readiness,
                    blocked.blocker,
                ));
            }
        }
    } else {
        None
    };
    let voice_projection = if let Some(handle) = &voice_handle {
        Some(crate::voice_context::fit_voice_context(
            handle,
            &mut conversation,
            &active_messages,
            crate::voice_context::VoicePrompt {
                settings: &settings,
                system: &system_message,
                skill: &skill_prompt.prompt,
                current: &input.message,
                template: ChatTemplateChoice::PreferNonThinking,
                completion_tokens: 96,
                mode: if call_greeting {
                    crate::voice_context::VoiceHistoryMode::Greeting
                } else {
                    crate::voice_context::VoiceHistoryMode::Recorded
                },
            },
        )?)
    } else {
        None
    };
    let (prepared_attachments, voice_messages) = if let Some(projection) = voice_projection {
        (Ok(projection.attachments), Some(projection.messages))
    } else {
        (
            if call_greeting {
                prepare_call_greeting_history(&input.conversation_id, &active_messages)?
            } else if voice_reply {
                prepare_voice_reply_attachments(&input.conversation_id, &active_messages)?
            } else {
                prepare_chat_attachments(
                    &input.conversation_id,
                    &active_messages,
                    regenerate_user_id.as_deref(),
                )?
            },
            None,
        )
    };
    let attachment_context = match prepared_attachments {
        Ok(context) => context,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.chat_send",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    if attachment_context
        .draft_snapshot
        .as_ref()
        .is_some_and(|draft| draft.message != input.message)
    {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_send",
            "stub_blocked",
            Blocker::new(
                "draft_changed_before_send",
                "The saved draft changed before this exact message could be admitted.",
                vec!["Refresh the composer and send the current draft.".to_string()],
            ),
        ));
    }
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
    // Pending recordings are conversation memory even during pickup. The
    // measured projection and generation must consume the same media; exact
    // transcripts replace archived audio only after their durable commit.
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
                "This conversation contains image or audio attachments, but the selected model's vision support is unavailable.",
                vec![
                    "Reselect the model so Mom can pair and load its vision support automatically."
                        .to_string(),
                ],
            ),
        ));
    }
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
    let messages = voice_messages.unwrap_or_else(|| {
        build_native_messages(
            &system_message,
            &skill_prompt.prompt,
            context_messages,
            &input.message,
            exclude_reasoning_from_context,
            &attachment_context.text_by_message_id,
            &attachment_context.current_text,
        )
    });
    let mut stream = ChatStreamLifecycle::new(
        &request_id,
        &conversation.id,
        options,
        &mut on_event,
        operation,
    );
    emit(
        stream.callback(),
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
    if stream.cancellation_requested() {
        return stream.finish_cancelled();
    }
    let started = Instant::now();
    let phone_call = input.message.starts_with("[Phone call]");
    let voice_note = input.message.starts_with("[Voice note]");
    let (
        mut assistant_text,
        reasoning_content,
        reasoning_incomplete,
        prompt_tokens,
        completion_tokens,
        duration_ms,
        first_token_ms,
        cache_id,
        cache_reused,
        cache_metrics,
    ) = if options.fake_fixture {
        (
            "Fixture assistant response.".to_string(),
            None,
            false,
            messages.iter().map(|message| message.content.len()).sum(),
            3,
            started.elapsed().as_millis(),
            None,
            None,
            false,
            GenerationCacheMetrics::default(),
        )
    } else {
        if stream.cancellation_requested() {
            return stream.finish_cancelled();
        }
        let handle = match voice_handle {
            Some(handle) => handle,
            None => match resident_model_for_profile(
                scope,
                &settings,
                &model_path,
                settings.mmproj_path.as_deref(),
            ) {
                Ok(handle) => handle,
                Err(blocked) => {
                    return Ok(CommandResult::blocked(
                        "mom_llama.chat_send",
                        &blocked.readiness,
                        blocked.blocker,
                    ));
                }
            },
        };
        let status = handle.status();
        if stream.cancellation_requested() {
            return stream.finish_cancelled();
        }
        let request_template = if phone_call || voice_note {
            ChatTemplateChoice::PreferNonThinking
        } else {
            match &conversation.execution_profile.chat_template {
                ChatTemplatePolicy::ModelDefault => ChatTemplateChoice::ModelDefault,
                ChatTemplatePolicy::FrozenSource(template) => {
                    ChatTemplateChoice::Override(template.clone())
                }
            }
        };
        // Lookup tokenizes with the actual template. Exact shared token IDs,
        // not the template's friendly name, decide whether cached KV is valid.
        let cached_prefix = {
            let owner_ids = reusable_prefix_owner_ids(
                &conversation.id,
                conversation.source_conversation_id.as_deref(),
                &conversation.current_skill_ids,
                skill_prompt.cache_owner_id.is_some(),
                skill_prompt.cache_owner_id.as_deref(),
            );
            longest_cached_prefix_for_owners(
                &handle,
                &messages,
                &owner_ids,
                &settings,
                !media.is_empty(),
                if phone_call {
                    PrefixLookupMode::LiveVoice
                } else {
                    PrefixLookupMode::Persistent
                },
                request_template.clone(),
            )?
            .0
        };
        let mut sampling =
            settings.sampling_for_profile(conversation.execution_profile.sampling.as_ref())?;
        if phone_call || voice_note {
            configure_voice_sampling(&mut sampling, phone_call, voice_note);
        }
        let build_request = |cached_prefix: Option<SequenceStateBlob>| GenerationRequest {
            request_id: request_id.clone(),
            model_id: status.model_id.clone(),
            input: GenerationInput::Chat {
                messages: messages.clone(),
                template: request_template.clone(),
            },
            sampling: sampling.clone(),
            media: media.clone(),
            cached_prefix,
        };
        let attempted_cache_id = cached_prefix.as_ref().map(|(id, _)| id.clone());
        let first_prefix = cached_prefix.as_ref().map(|(_, state)| state.clone());
        let Some(first_ticket) =
            stream.with_native_admission(|| handle.generate(build_request(first_prefix)))?
        else {
            return stream.finish_cancelled();
        };
        stream.mark_engine_invoked();
        let first_ticket = first_ticket.map_err(|error| anyhow::anyhow!(error))?;
        let (outputs, cache_reused) = match consume_chat_ticket(
            first_ticket,
            &request_id,
            &conversation.id,
            options,
            parse_reasoning,
            stream.callback(),
        ) {
            Ok(outputs) => {
                let reused = outputs
                    .iter()
                    .any(|output| prefix_cells_reused(&output.metrics.cache));
                (outputs, reused)
            }
            Err(ChatTicketError::Native(error))
                if error.code == NativeErrorCode::CacheIncompatible
                    && attempted_cache_id.is_some() =>
            {
                if let Some(cache_id) = attempted_cache_id.as_deref() {
                    if phone_call {
                        invalidate_memory(cache_id);
                    } else {
                        invalidate_cache(cache_id)?;
                    }
                }
                let Some(retry) =
                    stream.with_native_admission(|| handle.generate(build_request(None)))?
                else {
                    return stream.finish_cancelled();
                };
                let retry = retry.map_err(|error| anyhow::anyhow!(error))?;
                (
                    consume_chat_ticket(
                        retry,
                        &request_id,
                        &conversation.id,
                        options,
                        parse_reasoning,
                        stream.callback(),
                    )
                    .map_err(chat_ticket_error)?,
                    false,
                )
            }
            Err(error) => return Err(chat_ticket_error(error)),
        };
        let Some(output) = outputs.into_iter().next() else {
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
            return stream.finish_cancelled();
        }
        let parsed = parse_reasoning_output(&output.text, parse_reasoning);
        (
            strip_reserved_attribution_prefix(&parsed.content),
            (!parsed.reasoning.is_empty()).then_some(parsed.reasoning),
            parsed.incomplete,
            output.metrics.prompt_tokens,
            output.metrics.completion_tokens,
            output.metrics.duration_ms,
            output.metrics.first_token_ms,
            attempted_cache_id.filter(|_| cache_reused),
            cache_reused,
            output.metrics.cache.clone(),
        )
    };
    if stream.arbitrate_terminal() {
        return stream.finish_cancelled();
    }
    if phone_call || voice_note {
        assistant_text = assistant_text
            .split("\n\n")
            .next()
            .unwrap_or(&assistant_text)
            .trim()
            .to_string();
    }
    if assistant_text.trim().is_empty() {
        return Ok(empty_native_response_result(
            reasoning_content.as_deref(),
            options.fake_fixture,
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
    let mut generated_message_ids = Vec::with_capacity(2);
    if let Some(user_message) = user_message {
        generated_message_ids.push(user_message.id.clone());
        conversation.messages.push(user_message);
    }
    generated_message_ids.push(assistant_message_id.clone());
    conversation.messages.push(assistant_message);
    conversation.active_leaf_message_id = Some(assistant_message_id.clone());
    if upstream_setting_bool(&settings, "titleGenerationUseFirstLine")
        && crate::conversation_store::is_placeholder_title(&conversation.title, &conversation.id)
    {
        conversation.title = crate::conversation_store::first_line_title(&conversation.messages);
    }
    conversation.updated_at = now_ms().to_string();
    conversation.selected_model_path = settings.model_path.clone();
    conversation.execution_profile.model_path = settings.model_path.clone();
    conversation.execution_profile.mmproj_path = settings.mmproj_path.clone();
    let path = commit_generated_exchange(
        db,
        conversation.clone(),
        expected_active_leaf.as_deref(),
        &generated_message_ids,
        &attachment_context.staged_ids,
        &user_message_id,
        attachment_context.draft_snapshot.as_ref(),
    )?;
    note_committed_session_request(&conversation.id, &request_id);
    if !voice_reply {
        schedule_committed_session_checkpoint(scope, &conversation.id, &request_id);
    }
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
        first_token_ms,
        prompt_tokens,
        completion_tokens,
        cache_id,
        cache_reused,
        cache_metrics,
        voice_attachment_id: None,
        voice_audio_metrics: None,
    };
    stream.finish("completed", Some(result.assistant_text.clone()))?;
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

pub(crate) fn apply_conversation_model_pair(
    settings: &mut crate::Settings,
    profile_model: Option<PathBuf>,
    legacy_selected_model: Option<PathBuf>,
    profile_mmproj: Option<PathBuf>,
) {
    let conversation_model = normalize_optional_path(profile_model)
        .or_else(|| normalize_optional_path(legacy_selected_model));
    let Some(model) = conversation_model else {
        // No conversation override means the global pair remains atomic.
        return;
    };
    settings.model_path = Some(model);
    // `None` is meaningful here: this exact conversation selected a text-only
    // model. Never borrow the global model's projector.
    settings.mmproj_path = normalize_optional_path(profile_mmproj);
    settings.upstream_settings.insert(
        "mmprojPath".to_string(),
        Value::String(
            settings
                .mmproj_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        ),
    );
}

fn empty_native_response_result(
    reasoning_content: Option<&str>,
    fake_fixture: bool,
) -> CommandResult<ChatSendOutput> {
    let emitted_reasoning = reasoning_content.is_some_and(|reasoning| !reasoning.trim().is_empty());
    let (code, message, actions) = if emitted_reasoning {
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
    CommandResult::blocked_with_evidence(
        "mom_llama.chat_send",
        "blocked_native_runtime",
        Blocker::new(code, message, actions),
        Vec::new(),
        Vec::new(),
        !fake_fixture,
        fake_fixture,
    )
}

fn user_turn_is_empty(message: &str, attachments: &ChatAttachmentContext) -> bool {
    message.trim().is_empty()
        && attachments.current_text.trim().is_empty()
        && attachments.media.is_empty()
}

pub fn chat_cancel_in_scope(
    scope: &OperationScope,
    conversation_id: &str,
) -> Result<CommandResult<ChatCancelOutput>> {
    let Some(request_id) = scope.current_chat_request_id(conversation_id, true) else {
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
    let cancellation_accepted = scope.request_chat_cancellation(&request_id);
    let native_sequences_cancelled = scope.cancel_native(&request_id, None);
    if !cancellation_accepted && native_sequences_cancelled == 0 {
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
    Ok(CommandResult::passed(
        "mom_llama.chat_cancel",
        "contracted",
        ChatCancelOutput {
            cancel_path: format!("native://request/{request_id}"),
            request_id,
            conversation_id: conversation_id.to_string(),
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn chat_skip_reasoning_in_scope(
    scope: &OperationScope,
    conversation_id: &str,
) -> Result<CommandResult<ChatSkipReasoningOutput>> {
    let Some(request_id) = scope.current_chat_request_id(conversation_id, false) else {
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
    if scope.skip_native_reasoning(&request_id, Some(branch_id)) == 0 {
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
            request_id,
            conversation_id: conversation_id.to_string(),
            branch_id: branch_id.to_string(),
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub(crate) fn build_native_messages(
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

pub(crate) fn upstream_setting_bool(settings: &crate::config::Settings, key: &str) -> bool {
    settings
        .upstream_settings
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
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
    let mut last_progress = started;
    let maximum_lifetime = timeout
        .saturating_mul(20)
        .min(Duration::from_secs(600))
        .max(timeout);
    let mut timed_out = false;
    let mut reasoning_parser = parse_reasoning.then(ReasoningStreamParser::default);
    let mut first_content_emitted = false;
    loop {
        if ticket.is_backpressured() {
            last_progress = Instant::now();
        }
        if !timed_out
            && (last_progress.elapsed() >= timeout || started.elapsed() >= maximum_lifetime)
        {
            timed_out = true;
            ticket.cancel_all();
        }
        match ticket.events.recv_timeout(Duration::from_millis(20)) {
            Ok(event) => match event.event {
                GenerationEventKind::Delta { text } => {
                    if !text.is_empty() {
                        last_progress = Instant::now();
                    }
                    if let Some(parser) = reasoning_parser.as_mut() {
                        for routed in parser.push(&text) {
                            let content = routed.target == ReasoningTarget::Content;
                            let mut event = stream_event(
                                request_id,
                                conversation_id,
                                match routed.target {
                                    ReasoningTarget::Content => "delta",
                                    ReasoningTarget::Reasoning => "reasoning_delta",
                                },
                                Some(routed.text),
                                None,
                                options,
                            );
                            if content && !first_content_emitted {
                                event.emitted_at_unix_us = Some(unix_now_us());
                                first_content_emitted = true;
                            }
                            emit(on_event, event).map_err(ChatTicketError::Callback)?;
                        }
                    } else {
                        let mut event = stream_event(
                            request_id,
                            conversation_id,
                            "delta",
                            Some(text),
                            None,
                            options,
                        );
                        if !first_content_emitted {
                            event.emitted_at_unix_us = Some(unix_now_us());
                            first_content_emitted = true;
                        }
                        emit(on_event, event).map_err(ChatTicketError::Callback)?;
                    }
                }
                // Native state is not a product terminal. Expose only latency
                // milestones; the lifecycle owner still emits the sole terminal.
                GenerationEventKind::State { state } => {
                    let stage = match state {
                        GenerationState::Prefilling => Some("prefill_started"),
                        GenerationState::Generating => Some("decode_started"),
                        _ => None,
                    };
                    if let Some(stage) = stage {
                        last_progress = Instant::now();
                        let mut event =
                            stream_event(request_id, conversation_id, stage, None, None, options);
                        event.emitted_at_unix_us = Some(unix_now_us());
                        emit(on_event, event).map_err(ChatTicketError::Callback)?;
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
            let content = routed.target == ReasoningTarget::Content;
            let mut event = stream_event(
                request_id,
                conversation_id,
                match routed.target {
                    ReasoningTarget::Content => "delta",
                    ReasoningTarget::Reasoning => "reasoning_delta",
                },
                Some(routed.text),
                None,
                options,
            );
            if content && !first_content_emitted {
                event.emitted_at_unix_us = Some(unix_now_us());
                first_content_emitted = true;
            }
            emit(on_event, event).map_err(ChatTicketError::Callback)?;
        }
    }
    match ticket.wait() {
        _ if timed_out => Err(ChatTicketError::Callback(anyhow::anyhow!(
            "generation stopped progressing for {:.1} seconds or exceeded its bounded lifetime",
            options.timeout_s
        ))),
        result => result.map_err(ChatTicketError::Native),
    }
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
        emitted_at_unix_us: None,
        fake_fixture: options.fake_fixture,
        real_engine_invoked: !options.fake_fixture && event != "started",
    }
}

fn unix_now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_micros().min(u128::from(u64::MAX)) as u64
        })
}

fn prefix_cells_reused(cache: &GenerationCacheMetrics) -> bool {
    // A resident prefix is already evaluated KV in the same model worker.
    // Counting only an imported state blob falsely reports a cache miss for
    // the startup-prepared persona used by call pickup.
    cache.restored_prefix_tokens > 0
        || cache.resident_prefix_tokens > 0
        || cache.batch_shared_prefix_tokens > 0
}

fn configure_voice_sampling(
    sampling: &mut llama_native_types::SamplingConfig,
    phone_call: bool,
    voice_note: bool,
) {
    debug_assert!(phone_call || voice_note);
    if phone_call {
        // Calls should sound spontaneous even when the conversation has a
        // frozen sampling profile. This is the language sampler; the separate
        // acoustic TTS sampler remains tuned for stable PCM generation.
        sampling.temperature = 1.0;
        sampling.dynamic_temperature_range = 0.0;
    }
    sampling.max_tokens = sampling.max_tokens.min(if phone_call { 64 } else { 96 });
    if !sampling.stop.iter().any(|stop| stop == "\n\n") {
        sampling.stop.push("\n\n".to_string());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrefixLookupMode {
    Persistent,
    LiveVoice,
}

fn longest_cached_prefix_for_owners(
    handle: &NativeModelHandle,
    messages: &[ChatMessage],
    owner_ids: &[&str],
    settings: &crate::config::Settings,
    multimodal: bool,
    mode: PrefixLookupMode,
    template: ChatTemplateChoice,
) -> Result<(Option<(String, SequenceStateBlob)>, usize)> {
    let prompt = handle
        .tokenize_messages_with_template(messages.to_vec(), template.clone())
        .map_err(|error| anyhow::anyhow!(error))?;
    let resident_prefix_tokens = if multimodal {
        prompt.resident_media_prefix_tokens
    } else {
        prompt.resident_text_prefix_tokens
    };
    let mut longest: Option<(String, SequenceStateBlob)> = None;
    let mut longest_match = resident_prefix_tokens;
    for owner_id in owner_ids {
        let candidate = if mode == PrefixLookupMode::LiveVoice {
            compatible_memory_prefix_for_owner_with_settings(
                handle,
                messages,
                owner_id,
                settings,
                template.clone(),
                resident_prefix_tokens,
            )?
        } else {
            compatible_cached_prefix_for_owner_with_settings(
                handle,
                messages,
                Some(owner_id),
                settings,
                template.clone(),
                resident_prefix_tokens,
            )?
        };
        if let Some(candidate) = candidate.filter(|(_, sequence)| {
            mode == PrefixLookupMode::Persistent || handle.has_reusable_sequence(sequence)
        }) {
            let matched =
                llama_native_cache::common_prefix_tokens(&candidate.1.token_ids, &prompt.token_ids);
            if matched > longest_match {
                longest_match = matched;
                longest = Some(candidate);
            }
        }
    }
    Ok((longest, longest_match))
}

#[cfg(test)]
fn prefer_longer_prefix(
    current: Option<(String, SequenceStateBlob)>,
    candidate: (String, SequenceStateBlob),
) -> Option<(String, SequenceStateBlob)> {
    if current
        .as_ref()
        .is_some_and(|(_, sequence)| sequence.token_count >= candidate.1.token_count)
    {
        current
    } else {
        Some(candidate)
    }
}

fn reusable_prefix_owner_ids<'a>(
    conversation_id: &'a str,
    source_persona_id: Option<&'a str>,
    skill_ids: &[String],
    skills_cacheable: bool,
    skill_cache_owner_id: Option<&'a str>,
) -> Vec<&'a str> {
    let mut owners = vec![conversation_id];
    let fallback = if source_persona_id.is_some() {
        persona_prefix_cache_owner_id(source_persona_id, skill_ids, skills_cacheable)
    } else if skill_ids.is_empty() {
        None
    } else {
        skill_cache_owner_id
    };
    if let Some(owner) = fallback.filter(|owner| *owner != conversation_id) {
        owners.push(owner);
    }
    owners
}

#[cfg(test)]
mod tests {
    use super::{
        ChatAttachmentContext, ChatSendOptions, ChatStreamLifecycle, Message, MessageRole,
        ReasoningStreamParser, ReasoningTarget, apply_conversation_model_pair,
        build_native_messages, configure_voice_sampling, empty_native_response_result,
        native_context_messages, parse_reasoning_output, prefer_longer_prefix,
        reusable_prefix_owner_ids, user_turn_is_empty,
    };
    use crate::OperationScope;
    use crate::conversation_store::{MessageAttribution, MessageSpeakerKind};
    use std::path::PathBuf;

    #[test]
    fn cached_prefix_selection_prefers_the_longest_compatible_owner() {
        let prefix = |id: &str, tokens: usize| {
            (
                id.to_string(),
                llama_native_types::SequenceStateBlob {
                    sequence_id: 0,
                    token_count: tokens,
                    bytes: vec![1].into(),
                    token_ids: vec![1; tokens],
                    cells: Vec::new(),
                    storage_proof: None,
                },
            )
        };
        let selected =
            prefer_longer_prefix(Some(prefix("conversation", 20)), prefix("persona", 80))
                .expect("a longer persona prefix must win");
        assert_eq!(selected.0, "persona");
        let selected = prefer_longer_prefix(Some(selected), prefix("short", 10))
            .expect("a shorter prefix must not replace the selection");
        assert_eq!(selected.0, "persona");
    }

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
    fn phone_calls_override_frozen_temperature_without_changing_voice_notes() {
        let mut call = llama_native_types::SamplingConfig {
            temperature: 0.2,
            dynamic_temperature_range: 0.4,
            max_tokens: 512,
            ..Default::default()
        };
        configure_voice_sampling(&mut call, true, false);
        assert_eq!(call.temperature, 1.0);
        assert_eq!(call.dynamic_temperature_range, 0.0);
        assert_eq!(call.max_tokens, 64);
        assert_eq!(call.stop, ["\n\n"]);

        let mut note = llama_native_types::SamplingConfig {
            temperature: 0.2,
            max_tokens: 512,
            ..Default::default()
        };
        configure_voice_sampling(&mut note, false, true);
        assert_eq!(note.temperature, 0.2);
        assert_eq!(note.max_tokens, 96);
        assert_eq!(note.stop, ["\n\n"]);
    }

    #[test]
    fn inference_checks_the_exact_conversation_before_broader_prefixes() {
        let no_skills = Vec::new();
        assert_eq!(
            reusable_prefix_owner_ids("conversation", Some("persona"), &no_skills, false, None,),
            ["conversation", "persona"],
        );
        assert_eq!(
            reusable_prefix_owner_ids(
                "conversation",
                None,
                &["skill".to_string()],
                true,
                Some("skill-prefix"),
            ),
            ["conversation", "skill-prefix"],
        );
        assert_eq!(
            reusable_prefix_owner_ids("conversation", None, &no_skills, false, None),
            ["conversation"],
        );
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
    fn conversation_model_and_projector_override_the_global_pair_atomically() {
        let global_model = PathBuf::from("/models/global.gguf");
        let global_projector = PathBuf::from("/models/global-mmproj.gguf");
        let conversation_model = PathBuf::from("/models/conversation.gguf");
        let mut settings = crate::Settings::defaults_for_data_dir(std::env::temp_dir());
        settings.model_path = Some(global_model.clone());
        settings.mmproj_path = Some(global_projector.clone());

        apply_conversation_model_pair(&mut settings, Some(conversation_model.clone()), None, None);
        assert_eq!(settings.model_path, Some(conversation_model));
        assert_eq!(settings.mmproj_path, None);
        assert_eq!(
            settings.upstream_settings.get("mmprojPath"),
            Some(&serde_json::json!(""))
        );

        let mut inherited = crate::Settings::defaults_for_data_dir(std::env::temp_dir());
        inherited.model_path = Some(global_model.clone());
        inherited.mmproj_path = Some(global_projector.clone());
        apply_conversation_model_pair(&mut inherited, None, None, None);
        assert_eq!(inherited.model_path, Some(global_model));
        assert_eq!(inherited.mmproj_path, Some(global_projector));
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
    fn reasoning_only_blocker_preserves_real_inference_evidence() {
        let reasoning_only = empty_native_response_result(Some("private reasoning"), false);
        assert_eq!(
            reasoning_only
                .blocker
                .as_ref()
                .map(|blocker| blocker.code.as_str()),
            Some("native_response_reasoning_only")
        );
        assert!(reasoning_only.receipt.real_engine_invoked);
        assert!(!reasoning_only.receipt.fake_fixture);

        let empty = empty_native_response_result(None, false);
        assert!(empty.receipt.real_engine_invoked);

        let fixture = empty_native_response_result(Some("fixture reasoning"), true);
        assert!(!fixture.receipt.real_engine_invoked);
        assert!(fixture.receipt.fake_fixture);
    }

    #[test]
    fn dropping_started_stream_emits_one_failed_terminal_and_closes_registry_state() {
        let mut events = Vec::new();
        let mut callback = Some(|event| {
            events.push(event);
            Ok(())
        });
        let scope = OperationScope::detached();
        {
            let mut stream = ChatStreamLifecycle::new(
                "request",
                "conversation",
                ChatSendOptions::default(),
                &mut callback,
                scope
                    .register_chat("request", "conversation")
                    .expect("register live chat operation"),
            );
            stream.mark_engine_invoked();
        }

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "failed");
        assert_eq!(events[0].request_id, "request");
        assert!(events[0].real_engine_invoked);
        assert_eq!(scope.active_operation_count(), 0);
    }

    #[test]
    fn cancelled_receipt_and_terminal_preserve_the_same_invocation_fact() {
        for invoked in [false, true] {
            let scope = OperationScope::detached();
            let mut events = Vec::new();
            let mut callback = Some(|event| {
                events.push(event);
                Ok(())
            });
            let result = {
                let mut stream = ChatStreamLifecycle::new(
                    "request",
                    "conversation",
                    ChatSendOptions::default(),
                    &mut callback,
                    scope
                        .register_chat("request", "conversation")
                        .expect("register"),
                );
                if invoked {
                    stream.mark_engine_invoked();
                }
                stream.finish_cancelled().expect("cancel")
            };
            assert!(result.result.is_none());
            assert_eq!(result.blocker.expect("blocker").code, "chat_cancelled");
            assert_eq!(result.receipt.real_engine_invoked, invoked);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].event, "cancelled");
            assert_eq!(events[0].real_engine_invoked, invoked);
            assert_eq!(scope.active_operation_count(), 0);
        }
    }

    #[test]
    fn explicit_terminal_prevents_destructor_from_emitting_a_second_terminal() {
        let mut events = Vec::new();
        let mut callback = Some(|event| {
            events.push(event);
            Ok(())
        });
        let scope = OperationScope::detached();
        {
            let mut stream = ChatStreamLifecycle::new(
                "request",
                "conversation",
                ChatSendOptions::default(),
                &mut callback,
                scope
                    .register_chat("request", "conversation")
                    .expect("register live chat operation"),
            );
            stream
                .finish("completed", Some("answer".to_string()))
                .expect("emit terminal");
        }

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, "completed");
        assert!(!events[0].real_engine_invoked);
        assert_eq!(scope.active_operation_count(), 0);
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
            incomplete: false,
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
            draft_snapshot: None,
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
