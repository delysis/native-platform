use crate::attachments::{commit_generated_exchange, prepare_chat_attachments};
use crate::chat::{
    ChatSendInput, ChatSendOptions, ChatSendOutput, ChatStreamEvent, chat_send_stream,
    native_context_messages,
};
use crate::config::{Settings, resolve_settings, upstream_setting_i64};
use crate::conversation_store::{
    Conversation, ConversationExecutionProfile, ConversationKind, Message, MessageAttribution,
    MessageRole, MessageSpeakerKind, active_leaf_id, active_path_messages, load_db, save_db,
    strip_reserved_attribution_prefix,
};
use crate::kv_cache::ensure_persona_prefix;
use crate::mcp::{McpTool, mcp_call_tool_supervised};
use crate::native_runtime::{cancel_native_request, resident_model_for_profile};
use crate::now_ms;
use crate::personas::{conversation_and_group_handles, persona_instantiate};
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use crate::tool_loop::{
    ToolPermissionPolicy, resolve_tool_contract, tool_permission_policy, validate_tool_arguments,
};
use anyhow::{Result, anyhow};
use crossbeam_channel::TryRecvError;
use llama_native_engine::NativeModelHandle;
use llama_native_types::{
    BranchRequest, ChatMessage, ChatRole, ChatTemplateChoice, GenerationEventKind, GenerationInput,
    GenerationMetrics, GenerationOutput, GenerationRequest, GenerationState,
    SharedPrefixBatchRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

const INVOCATIONS_NAMESPACE: &str = "mention-invocations.v1";
const MAX_TARGETS: usize = 4;
type MentionCancelKey = (String, String);
type MentionCancelRegistry = BTreeMap<MentionCancelKey, Arc<AtomicBool>>;

fn mention_cancel_registry() -> &'static Mutex<MentionCancelRegistry> {
    static REGISTRY: OnceLock<Mutex<MentionCancelRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MentionTargetKind {
    Persona,
    LiveChat,
    Group,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionCandidate {
    pub id: String,
    pub kind: MentionTargetKind,
    pub handle: String,
    pub label: String,
    pub detail: String,
    pub member_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentionTargetSnapshot {
    pub target_id: String,
    pub kind: MentionTargetKind,
    pub handle: String,
    pub label: String,
    pub version: u64,
    pub source_leaf_message_id: Option<String>,
    pub profile: ConversationExecutionProfile,
    pub source_messages: Vec<Message>,
    pub snapshot_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentionTargetResult {
    pub target_id: String,
    pub handle: String,
    pub label: String,
    pub state: GenerationState,
    pub text: String,
    pub model_id: String,
    pub message_id: Option<String>,
    pub metrics: GenerationMetrics,
    #[serde(default)]
    pub cache_id: Option<String>,
    #[serde(default)]
    pub cache_reused: bool,
    #[serde(default)]
    pub tool_receipt_ids: Vec<String>,
    pub real_engine_invoked: bool,
    pub fake_fixture: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MentionInvocationState {
    Running,
    Completed,
    PartiallyCompleted,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentionInvocation {
    pub id: String,
    pub host_conversation_id: String,
    pub user_message_id: String,
    pub addressed_message: String,
    pub host_context: Vec<Message>,
    pub targets: Vec<MentionTargetSnapshot>,
    pub results: Vec<MentionTargetResult>,
    #[serde(default)]
    pub synthesis_message_id: Option<String>,
    #[serde(default)]
    pub synthesis_sha256: Option<String>,
    pub state: MentionInvocationState,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct MentionInvocationDb {
    invocations: Vec<MentionInvocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionDispatchInput {
    pub conversation_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatDispatchOutput {
    Direct {
        conversation_id: String,
        output: ChatSendOutput,
    },
    Mention {
        conversation_id: String,
        invocation: MentionInvocation,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "event", rename_all = "snake_case")]
pub enum ChatDispatchStreamEvent {
    Chat(ChatStreamEvent),
    Mention(MentionStreamEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionStreamEvent {
    pub schema: String,
    pub invocation_id: String,
    pub target_id: String,
    pub handle: String,
    pub label: String,
    pub event: String,
    pub delta: Option<String>,
    pub state: Option<GenerationState>,
    pub real_engine_invoked: bool,
    pub fake_fixture: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentionCancelOutput {
    pub invocation_id: String,
    pub target_id: Option<String>,
    pub cancelled_sequences: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MentionSynthesisOutput {
    pub invocation_id: String,
    pub host_conversation_id: String,
    pub message_id: String,
    pub text: String,
    pub model_id: String,
    pub source_message_ids: Vec<String>,
    pub source_content_sha256: Vec<String>,
    pub metrics: GenerationMetrics,
}

pub fn mention_candidates(
    query: &str,
    current_conversation_id: Option<&str>,
) -> Result<CommandResult<Vec<MentionCandidate>>> {
    let (conversations, groups) = conversation_and_group_handles()?;
    let query = query.trim().trim_start_matches('@').to_ascii_lowercase();
    let mut candidates = conversations
        .into_iter()
        .filter(|conversation| Some(conversation.id.as_str()) != current_conversation_id)
        .filter(|conversation| {
            query.is_empty()
                || conversation
                    .execution_profile
                    .mention_handle
                    .to_ascii_lowercase()
                    .contains(&query)
                || conversation.title.to_ascii_lowercase().contains(&query)
        })
        .map(|conversation| MentionCandidate {
            id: conversation.id,
            kind: if conversation.kind == ConversationKind::PersonaTemplate {
                MentionTargetKind::Persona
            } else {
                MentionTargetKind::LiveChat
            },
            handle: conversation.execution_profile.mention_handle,
            label: conversation.title,
            detail: if conversation.kind == ConversationKind::PersonaTemplate {
                conversation
                    .execution_profile
                    .model_path
                    .as_ref()
                    .and_then(|path| path.file_stem())
                    .and_then(|value| value.to_str())
                    .unwrap_or("Default local model")
                    .to_string()
            } else {
                format!("Updated {}", conversation.updated_at)
            },
            member_count: None,
        })
        .collect::<Vec<_>>();
    candidates.extend(
        groups
            .into_iter()
            .filter(|group| {
                query.is_empty()
                    || group.mention_handle.to_ascii_lowercase().contains(&query)
                    || group.name.to_ascii_lowercase().contains(&query)
            })
            .map(|group| MentionCandidate {
                id: group.id,
                kind: MentionTargetKind::Group,
                handle: group.mention_handle,
                label: group.name,
                detail: format!("{} personas", group.persona_ids.len()),
                member_count: Some(group.persona_ids.len()),
            }),
    );
    candidates.sort_by(|left, right| {
        candidate_rank(left.kind)
            .cmp(&candidate_rank(right.kind))
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
    });
    Ok(CommandResult::passed(
        "mom_llama.mention_candidates",
        "contracted",
        candidates,
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub fn chat_dispatch(
    input: MentionDispatchInput,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatDispatchOutput>> {
    chat_dispatch_stream(
        input,
        options,
        None::<fn(ChatDispatchStreamEvent) -> Result<()>>,
    )
}

pub fn mention_dispatch(
    input: MentionDispatchInput,
    options: ChatSendOptions,
) -> Result<CommandResult<ChatDispatchOutput>> {
    let mut result = chat_dispatch(input, options)?;
    result.command = "mom_llama.mention_dispatch".to_string();
    result.receipt.command = "mom_llama.mention_dispatch".to_string();
    Ok(result)
}

pub fn chat_dispatch_stream<F>(
    mut input: MentionDispatchInput,
    options: ChatSendOptions,
    mut on_event: Option<F>,
) -> Result<CommandResult<ChatDispatchOutput>>
where
    F: FnMut(ChatDispatchStreamEvent) -> Result<()>,
{
    let (_, selected) =
        crate::conversation_store::get_or_create_conversation(&input.conversation_id)?;
    if selected.kind == ConversationKind::PersonaTemplate {
        let instantiated = persona_instantiate(&selected.id, None)?;
        let Some(conversation) = instantiated.result else {
            return Ok(CommandResult::blocked(
                "mom_llama.chat_dispatch",
                &instantiated.readiness,
                instantiated.blocker.unwrap_or_else(|| {
                    Blocker::new(
                        "persona_instantiate_failed",
                        "The persona could not be opened as a chat.",
                        vec!["Try again from Personas in Settings.".to_string()],
                    )
                }),
            ));
        };
        input.conversation_id = conversation.id;
    }
    let handles = parse_handles(&input.message);
    let resolution = resolve_targets(&handles, &input.conversation_id)?;
    if let Some(blocker) = ambiguous_resolution_blocker(&resolution) {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            blocker,
        ));
    }
    if !resolution.unresolved.is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "mention_target_not_found",
                format!(
                    "These mentioned participants are unavailable: {}.",
                    resolution
                        .unresolved
                        .iter()
                        .map(|handle| format!("@{handle}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                vec!["Choose a current autocomplete result and retry.".to_string()],
            ),
        ));
    }
    let resolved = resolution.targets;
    if resolved.len() > MAX_TARGETS {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "mention_target_limit_exceeded",
                "A message may invite at most four distinct targets.",
                vec!["Remove one or more mentions and retry.".to_string()],
            ),
        ));
    }
    if resolved.is_empty() {
        let conversation_id = input.conversation_id.clone();
        let output = chat_send_stream(
            ChatSendInput {
                conversation_id: conversation_id.clone(),
                message: input.message,
            },
            options,
            |event| {
                if let Some(callback) = on_event.as_mut() {
                    callback(ChatDispatchStreamEvent::Chat(event))?;
                }
                Ok(())
            },
        )?;
        if output.status == "blocked" {
            return Ok(CommandResult::blocked(
                "mom_llama.chat_dispatch",
                &output.readiness,
                output.blocker.unwrap_or_else(|| {
                    Blocker::new(
                        "chat_dispatch_blocked",
                        "The local chat request was blocked.",
                        vec!["Check the selected model.".to_string()],
                    )
                }),
            ));
        }
        return Ok(CommandResult::passed(
            "mom_llama.chat_dispatch",
            &output.readiness,
            ChatDispatchOutput::Direct {
                conversation_id,
                output: output.result.expect("passed chat result"),
            },
            output.receipt.changed_paths,
            output.receipt.artifacts_produced,
            output.receipt.real_engine_invoked,
            output.receipt.fake_fixture,
        ));
    }
    dispatch_mentions(input, resolved, options, &mut on_event)
}

pub fn mention_cancel(
    invocation_id: &str,
    target_id: Option<&str>,
) -> Result<CommandResult<MentionCancelOutput>> {
    let flagged = request_mention_cancellation(invocation_id, target_id);
    let native = cancel_native_request(invocation_id, target_id);
    let count = flagged.max(native);
    if count == 0 {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_cancel",
            "stub_blocked",
            Blocker::new(
                "mention_request_not_active",
                "No matching invited response is currently running.",
                vec!["Refresh the conversation.".to_string()],
            ),
        ));
    }
    Ok(CommandResult::passed(
        "mom_llama.mention_cancel",
        "host_integrated",
        MentionCancelOutput {
            invocation_id: invocation_id.to_string(),
            target_id: target_id.map(str::to_string),
            cancelled_sequences: count,
        },
        Vec::new(),
        Vec::new(),
        false,
        false,
    ))
}

pub(crate) fn request_all_mention_cancellation() -> usize {
    let controls = mention_cancel_registry()
        .lock()
        .map(|registry| {
            registry
                .iter()
                .map(|((invocation_id, target_id), control)| {
                    control.store(true, Ordering::Release);
                    (invocation_id.clone(), target_id.clone())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    controls
        .iter()
        .fold(0_usize, |total, (invocation_id, target_id)| {
            total.saturating_add(cancel_native_request(
                invocation_id,
                Some(target_id.as_str()),
            ))
        })
}

pub fn mention_synthesize(invocation_id: &str) -> Result<CommandResult<MentionSynthesisOutput>> {
    let store = RuntimeStore::current()?;
    let invocation = store
        .get::<MentionInvocationDb>(INVOCATIONS_NAMESPACE)?
        .unwrap_or_default()
        .invocations
        .into_iter()
        .find(|invocation| invocation.id == invocation_id);
    let Some(invocation) = invocation else {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_invocation_not_found",
                "The invited responses are no longer available.",
                vec!["Run the Persona group again.".to_string()],
            ),
        ));
    };
    if invocation.synthesis_message_id.is_some() {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_synthesis_already_exists",
                "This set of invited responses already has a synthesis.",
                vec!["Edit or regenerate the existing synthesis message.".to_string()],
            ),
        ));
    }
    let completed = invocation
        .results
        .iter()
        .filter(|result| {
            result.state == GenerationState::Completed
                && result.real_engine_invoked
                && !result.fake_fixture
                && !result.text.trim().is_empty()
        })
        .collect::<Vec<_>>();
    if completed.len() < 2 {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_synthesis_sources_incomplete",
                "At least two completed local-model responses are required for synthesis.",
                vec!["Wait for another invited response to finish.".to_string()],
            ),
        ));
    }

    let settings = resolve_settings()?;
    let db = load_db()?;
    let Some(host) = db
        .conversations
        .iter()
        .find(|conversation| conversation.id == invocation.host_conversation_id)
        .cloned()
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_host_conversation_not_found",
                "The host chat no longer exists.",
                vec!["Open another chat and run the group again.".to_string()],
            ),
        ));
    };
    let model_path = host
        .execution_profile
        .model_path
        .as_deref()
        .or(host.selected_model_path.as_deref())
        .or(settings.model_path.as_deref());
    let Some(model_path) = model_path else {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "blocked_missing_model",
            Blocker::new(
                "model_path_missing",
                "No local model is configured for this chat.",
                vec!["Choose a model in Settings.".to_string()],
            ),
        ));
    };
    let handle = match resident_model_for_profile(
        &settings,
        model_path,
        host.execution_profile.mmproj_path.as_deref(),
    ) {
        Ok(handle) => handle,
        Err(blocked) => {
            return Ok(CommandResult::blocked(
                "mom_llama.mention_synthesize",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let sources = completed
        .iter()
        .map(|result| format!("## {} (@{})\n{}", result.label, result.handle, result.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    let messages = vec![
        ChatMessage {
            role: ChatRole::System,
            content: "Synthesize the invited local-model responses into one useful answer. Preserve material disagreements, distinguish evidence from uncertainty, and do not invent consensus or facts. Do not claim to be any invited Persona.".to_string(),
        },
        ChatMessage {
            role: ChatRole::User,
            content: format!(
                "Original addressed message:\n{}\n\nInvited responses:\n{}",
                invocation.addressed_message, sources
            ),
        },
    ];
    let template = profile_chat_template(&host.execution_profile);
    let prompt_tokens = handle
        .tokenize_messages_with_template(messages.clone(), template.clone())
        .map_err(|error| anyhow!(error))?
        .token_ids
        .len();
    let sampling = host
        .execution_profile
        .sampling
        .clone()
        .unwrap_or_else(|| settings.sampling_config());
    if prompt_tokens.saturating_add(sampling.max_tokens as usize) > settings.context_tokens as usize
    {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_synthesis_context_too_large",
                "The completed invited responses do not fit the synthesis model context.",
                vec!["Reduce Persona output limits and run the group again.".to_string()],
            ),
        ));
    }
    let request_id = format!("{invocation_id}:synthesis");
    let output = handle
        .generate(GenerationRequest {
            request_id: request_id.clone(),
            model_id: handle.status().model_id,
            input: GenerationInput::Chat { messages, template },
            sampling,
            media: Vec::new(),
            cached_prefix: None,
        })
        .map_err(|error| anyhow!(error))?
        .wait()
        .map_err(|error| anyhow!(error))?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("native mention synthesis returned no output"))?;
    if output.state != GenerationState::Completed
        || !output.real_engine_invoked
        || output.fake_fixture
        || output.text.trim().is_empty()
    {
        return Ok(CommandResult::blocked_with_evidence(
            "mom_llama.mention_synthesize",
            "blocked_native_runtime",
            Blocker::new(
                "mention_synthesis_not_completed",
                "The local synthesis model did not complete a real response.",
                vec!["Retry synthesis or check the chat's selected model.".to_string()],
            ),
            vec![store.path().display().to_string()],
            Vec::new(),
            output.real_engine_invoked,
            output.fake_fixture,
        ));
    }

    let mut db = load_db()?;
    let Some(host) = db
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == invocation.host_conversation_id)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.mention_synthesize",
            "stub_blocked",
            Blocker::new(
                "mention_host_conversation_not_found",
                "The host chat was removed before synthesis completed.",
                vec!["Open another chat and run the group again.".to_string()],
            ),
        ));
    };
    let message_id = Uuid::new_v4().to_string();
    host.messages.push(Message {
        id: message_id.clone(),
        conversation_id: host.id.clone(),
        role: MessageRole::Assistant,
        content: output.text.clone(),
        created_at: now_ms().to_string(),
        parent_id: active_leaf_id(host),
        model: Some(output.model_id.clone()),
        receipt_id: Some(format!("mom_llama.mention_synthesize:{invocation_id}")),
        prompt_tokens: Some(output.metrics.prompt_tokens),
        completion_tokens: Some(output.metrics.completion_tokens),
        reasoning_content: None,
        reasoning_incomplete: false,
        branch_index: None,
        branch_count: None,
        attribution: Some(MessageAttribution {
            kind: MessageSpeakerKind::Synthesis,
            source_id: invocation_id.to_string(),
            handle: "synthesis".to_string(),
            label: "Synthesis".to_string(),
            version: 1,
            invocation_id: invocation_id.to_string(),
            target_order: completed.len(),
        }),
        attachment_ids: Vec::new(),
    });
    host.active_leaf_message_id = Some(message_id.clone());
    host.updated_at = now_ms().to_string();
    let conversation_path = save_db(&db)?;
    let synthesis_sha256 = format!("{:x}", Sha256::digest(output.text.as_bytes()));
    store.mutate(INVOCATIONS_NAMESPACE, MentionInvocationDb::default, |db| {
        if let Some(stored) = db
            .invocations
            .iter_mut()
            .find(|stored| stored.id == invocation_id)
        {
            stored.synthesis_message_id = Some(message_id.clone());
            stored.synthesis_sha256 = Some(synthesis_sha256.clone());
            stored.updated_at = now_ms().to_string();
        }
        Ok(())
    })?;
    let result = MentionSynthesisOutput {
        invocation_id: invocation_id.to_string(),
        host_conversation_id: invocation.host_conversation_id,
        message_id,
        text: output.text,
        model_id: output.model_id,
        source_message_ids: completed
            .iter()
            .filter_map(|result| result.message_id.clone())
            .collect(),
        source_content_sha256: completed
            .iter()
            .map(|result| format!("{:x}", Sha256::digest(result.text.as_bytes())))
            .collect(),
        metrics: output.metrics,
    };
    Ok(CommandResult::passed(
        "mom_llama.mention_synthesize",
        "real_prompt_smoke_passed",
        result,
        vec![
            conversation_path.display().to_string(),
            store.path().display().to_string(),
        ],
        Vec::new(),
        true,
        false,
    ))
}

fn dispatch_mentions<F>(
    input: MentionDispatchInput,
    targets: Vec<ResolvedTarget>,
    options: ChatSendOptions,
    on_event: &mut Option<F>,
) -> Result<CommandResult<ChatDispatchOutput>>
where
    F: FnMut(ChatDispatchStreamEvent) -> Result<()>,
{
    let settings = resolve_settings()?;
    let mut db = load_db()?;
    let Some(host_index) = db
        .conversations
        .iter()
        .position(|conversation| conversation.id == input.conversation_id)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "host_conversation_not_found",
                "The host conversation no longer exists.",
                vec!["Open another chat and retry.".to_string()],
            ),
        ));
    };
    let host_snapshot = active_path_messages(&db.conversations[host_index]);
    let attachment_context =
        match prepare_chat_attachments(&input.conversation_id, &host_snapshot, None)? {
            Ok(context) => context,
            Err(blocked) => {
                return Ok(CommandResult::blocked(
                    "mom_llama.chat_dispatch",
                    &blocked.readiness,
                    blocked.blocker,
                ));
            }
        };
    if !attachment_context.media.is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "mention_multimodal_attachment_unsupported",
                "Image and audio attachments are not yet accepted by the shared-prefix mention dispatcher.",
                vec!["Send the attachment to one chat directly, or remove it before invoking Personas.".to_string()],
            ),
        ));
    }
    let invocation_id = Uuid::new_v4().to_string();
    let user_message_id = Uuid::new_v4().to_string();
    let user_message = Message {
        id: user_message_id.clone(),
        conversation_id: input.conversation_id.clone(),
        role: MessageRole::User,
        content: input.message.clone(),
        created_at: now_ms().to_string(),
        parent_id: active_leaf_id(&db.conversations[host_index]),
        model: None,
        receipt_id: None,
        prompt_tokens: None,
        completion_tokens: None,
        reasoning_content: None,
        reasoning_incomplete: false,
        branch_index: None,
        branch_count: None,
        attribution: None,
        attachment_ids: attachment_context.staged_ids.clone(),
    };
    let expected_active_leaf = user_message.parent_id.clone();

    let snapshots = targets
        .iter()
        .map(snapshot_target)
        .collect::<Result<Vec<_>>>()?;
    let now = now_ms().to_string();
    let mut invocation = MentionInvocation {
        id: invocation_id.clone(),
        host_conversation_id: input.conversation_id.clone(),
        user_message_id: user_message_id.clone(),
        addressed_message: input.message.clone(),
        host_context: host_snapshot.clone(),
        targets: snapshots.clone(),
        results: Vec::new(),
        synthesis_message_id: None,
        synthesis_sha256: None,
        state: MentionInvocationState::Running,
        created_at: now.clone(),
        updated_at: now,
    };
    save_invocation(&invocation)?;

    if options.fake_fixture {
        for snapshot in &snapshots {
            emit(
                on_event,
                MentionStreamEvent {
                    schema: "mom_llama.mention_stream_event.v1".to_string(),
                    invocation_id: invocation_id.clone(),
                    target_id: snapshot.target_id.clone(),
                    handle: snapshot.handle.clone(),
                    label: snapshot.label.clone(),
                    event: "started".to_string(),
                    delta: None,
                    state: Some(GenerationState::Queued),
                    real_engine_invoked: false,
                    fake_fixture: true,
                },
            )?;
            let text = format!("Fixture response from @{}", snapshot.handle);
            emit(
                on_event,
                MentionStreamEvent {
                    schema: "mom_llama.mention_stream_event.v1".to_string(),
                    invocation_id: invocation_id.clone(),
                    target_id: snapshot.target_id.clone(),
                    handle: snapshot.handle.clone(),
                    label: snapshot.label.clone(),
                    event: "delta".to_string(),
                    delta: Some(text.clone()),
                    state: None,
                    real_engine_invoked: false,
                    fake_fixture: true,
                },
            )?;
            invocation.results.push(MentionTargetResult {
                target_id: snapshot.target_id.clone(),
                handle: snapshot.handle.clone(),
                label: snapshot.label.clone(),
                state: GenerationState::Completed,
                text,
                model_id: "fake_fixture".to_string(),
                message_id: None,
                metrics: GenerationMetrics::default(),
                cache_id: None,
                cache_reused: false,
                tool_receipt_ids: Vec::new(),
                real_engine_invoked: false,
                fake_fixture: true,
            });
        }
        let host = &mut db.conversations[host_index];
        host.messages.push(user_message);
        host.active_leaf_message_id = Some(user_message_id.clone());
        host.updated_at = now_ms().to_string();
        append_attributed_results(host, &mut invocation);
        let host = host.clone();
        let conversation_path = commit_generated_exchange(
            db,
            host,
            expected_active_leaf.as_deref(),
            &attachment_context.staged_ids,
            &user_message_id,
            true,
        )?;
        invocation.state = invocation_state(&invocation.results);
        invocation.updated_at = now_ms().to_string();
        save_invocation(&invocation)?;
        return Ok(CommandResult::passed(
            "mom_llama.chat_dispatch",
            "fake_fixture_exercised",
            ChatDispatchOutput::Mention {
                conversation_id: input.conversation_id,
                invocation,
            },
            vec![conversation_path.display().to_string()],
            Vec::new(),
            false,
            true,
        ));
    }

    let addressed = append_attachment_context(
        &strip_handles(&input.message, &targets),
        &attachment_context.current_text,
    );
    let participant_names = targets
        .iter()
        .map(|target| format!("@{}", target.conversation.execution_profile.mention_handle))
        .collect::<Vec<_>>()
        .join(", ");
    let mut planned = Vec::new();
    for (order, target) in targets.iter().enumerate() {
        let snapshot = &snapshots[order];
        let model_path = snapshot
            .profile
            .model_path
            .as_deref()
            .or(target.conversation.selected_model_path.as_deref())
            .or(settings.model_path.as_deref());
        let Some(model_path) = model_path else {
            invocation.results.push(blocked_target_result(
                snapshot,
                GenerationState::Failed,
                "No model is configured for this invited chat.",
            ));
            continue;
        };
        let handle = match resident_model_for_profile(
            &settings,
            model_path,
            snapshot.profile.mmproj_path.as_deref(),
        ) {
            Ok(handle) => handle,
            Err(blocked) => {
                invocation.results.push(blocked_target_result(
                    snapshot,
                    GenerationState::Failed,
                    &blocked.blocker.message,
                ));
                continue;
            }
        };
        let tools = match resolve_mention_tools(&snapshot.profile.tool_bindings) {
            Ok(tools) => tools,
            Err(blocker) => {
                invocation.results.push(blocked_target_result(
                    snapshot,
                    GenerationState::Failed,
                    &blocker.message,
                ));
                continue;
            }
        };
        let handoff = match handoff_messages(
            &handle,
            &settings,
            snapshot,
            &host_snapshot,
            &addressed,
            &participant_names,
            &tools,
        ) {
            Ok(handoff) => handoff,
            Err(blocker) => {
                invocation.results.push(blocked_target_result(
                    snapshot,
                    GenerationState::Failed,
                    &blocker.message,
                ));
                continue;
            }
        };
        let cache_use = match snapshot.profile.chat_template {
            crate::conversation_store::ChatTemplatePolicy::ModelDefault => ensure_persona_prefix(
                &handle,
                &cache_owner(snapshot),
                &format!("Invited context for @{}", snapshot.handle),
                &handoff.stable_prefix,
                &handoff.messages,
            )?,
            crate::conversation_store::ChatTemplatePolicy::FrozenSource(_) => None,
        };
        planned.push(PlannedTarget {
            snapshot: snapshot.clone(),
            model_path: model_path.to_path_buf(),
            handle,
            messages: handoff.messages,
            cache_id: cache_use.as_ref().map(|cache| cache.cache_id.clone()),
            cache_reused: cache_use.as_ref().is_some_and(|cache| cache.reused),
            cached_prefix: cache_use.map(|cache| cache.sequence),
            tools,
        });
    }

    let mut groups = BTreeMap::<String, Vec<PlannedTarget>>::new();
    let _cancel_lifecycle = MentionCancelLifecycle::register(&invocation_id, &planned);
    for target in planned {
        let template_hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&target.snapshot.profile.chat_template)?)
        );
        let key = format!(
            "{}|{}|{}",
            target.model_path.display(),
            target
                .snapshot
                .profile
                .mmproj_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            template_hash
        );
        groups.entry(key).or_default().push(target);
    }
    let mut tickets = Vec::new();
    for targets in groups.into_values() {
        for target in &targets {
            emit(
                on_event,
                MentionStreamEvent {
                    schema: "mom_llama.mention_stream_event.v1".to_string(),
                    invocation_id: invocation_id.clone(),
                    target_id: target.snapshot.target_id.clone(),
                    handle: target.snapshot.handle.clone(),
                    label: target.snapshot.label.clone(),
                    event: "started".to_string(),
                    delta: None,
                    state: Some(GenerationState::Queued),
                    real_engine_invoked: false,
                    fake_fixture: false,
                },
            )?;
        }
        let handle = targets[0].handle.clone();
        let status = handle.status();
        let branches = targets
            .iter()
            .map(|target| BranchRequest {
                branch_id: target.snapshot.target_id.clone(),
                label: target.snapshot.label.clone(),
                instruction: String::new(),
                sampling: target
                    .snapshot
                    .profile
                    .sampling
                    .clone()
                    .unwrap_or_else(|| settings.sampling_config()),
                messages: target.messages.clone(),
                cached_prefix: target.cached_prefix.clone(),
            })
            .collect();
        let ticket = handle
            .generate_shared_prefix(SharedPrefixBatchRequest {
                request_id: invocation_id.clone(),
                model_id: status.model_id,
                common_messages: Vec::new(),
                chat_template: match &targets[0].snapshot.profile.chat_template {
                    crate::conversation_store::ChatTemplatePolicy::ModelDefault => {
                        ChatTemplateChoice::ModelDefault
                    }
                    crate::conversation_store::ChatTemplatePolicy::FrozenSource(template) => {
                        ChatTemplateChoice::Override(template.clone())
                    }
                },
                branches,
                cached_prefix: None,
            })
            .map_err(|error| anyhow!(error))?;
        tickets.push((ticket, targets));
    }
    let started = Instant::now();
    let timeout = Duration::from_secs_f64(options.timeout_s.max(0.001));
    let mut disconnected = vec![false; tickets.len()];
    while disconnected.iter().any(|done| !done) {
        if started.elapsed() >= timeout {
            for (ticket, _) in &tickets {
                ticket.cancel_all();
            }
        }
        let mut progress = false;
        for (index, (ticket, targets)) in tickets.iter().enumerate() {
            if disconnected[index] {
                continue;
            }
            loop {
                match ticket.events.try_recv() {
                    Ok(event) => {
                        progress = true;
                        let target = targets
                            .iter()
                            .find(|target| target.snapshot.target_id == event.branch_id);
                        if let Some(target) = target {
                            let (name, delta, state) = match event.event {
                                GenerationEventKind::Delta { text } => ("delta", Some(text), None),
                                GenerationEventKind::State { state } => {
                                    ("state", None, Some(state))
                                }
                                GenerationEventKind::Warning { message, .. } => {
                                    ("warning", Some(message), None)
                                }
                            };
                            emit(
                                on_event,
                                MentionStreamEvent {
                                    schema: "mom_llama.mention_stream_event.v1".to_string(),
                                    invocation_id: invocation_id.clone(),
                                    target_id: target.snapshot.target_id.clone(),
                                    handle: target.snapshot.handle.clone(),
                                    label: target.snapshot.label.clone(),
                                    event: name.to_string(),
                                    delta,
                                    state,
                                    real_engine_invoked: name == "delta"
                                        || state == Some(GenerationState::Completed),
                                    fake_fixture: false,
                                },
                            )?;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected[index] = true;
                        break;
                    }
                }
            }
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    for (ticket, targets) in tickets {
        match ticket.wait() {
            Ok(outputs) => {
                for output in outputs {
                    if let Some(target) = targets
                        .iter()
                        .find(|target| target.snapshot.target_id == output.branch_id)
                    {
                        let (output, tool_receipt_ids) = if output.state
                            == GenerationState::Completed
                            && !target.tools.is_empty()
                        {
                            match finish_tool_bound_mention(
                                target,
                                output,
                                &invocation_id,
                                &settings,
                            ) {
                                Ok(value) => value,
                                Err(blocker) => {
                                    let state = if mention_cancellation_requested(
                                        &invocation_id,
                                        &target.snapshot.target_id,
                                    ) {
                                        GenerationState::Cancelled
                                    } else {
                                        GenerationState::Failed
                                    };
                                    let mut blocked = blocked_target_result(
                                        &target.snapshot,
                                        state,
                                        &blocker.message,
                                    );
                                    blocked.real_engine_invoked = true;
                                    invocation.results.push(blocked);
                                    continue;
                                }
                            }
                        } else {
                            (output, Vec::new())
                        };
                        invocation.results.push(MentionTargetResult {
                            target_id: target.snapshot.target_id.clone(),
                            handle: target.snapshot.handle.clone(),
                            label: target.snapshot.label.clone(),
                            state: output.state,
                            text: strip_reserved_attribution_prefix(&output.text),
                            model_id: output.model_id,
                            message_id: None,
                            metrics: output.metrics,
                            cache_id: target.cache_id.clone(),
                            cache_reused: target.cache_reused,
                            tool_receipt_ids,
                            real_engine_invoked: output.real_engine_invoked,
                            fake_fixture: output.fake_fixture,
                        });
                    }
                }
            }
            Err(error) => {
                for target in targets {
                    invocation.results.push(blocked_target_result(
                        &target.snapshot,
                        GenerationState::Failed,
                        &error.message,
                    ));
                }
            }
        }
    }
    invocation.results.sort_by_key(|result| {
        snapshots
            .iter()
            .position(|target| target.target_id == result.target_id)
            .unwrap_or(usize::MAX)
    });
    invocation.state = invocation_state(&invocation.results);
    invocation.updated_at = now_ms().to_string();
    let real_engine_invoked = invocation.results.iter().any(|result| {
        result.real_engine_invoked
            && !result.fake_fixture
            && result.state == GenerationState::Completed
            && !result.text.trim().is_empty()
    });
    if !real_engine_invoked {
        save_invocation(&invocation)?;
        return Ok(CommandResult::blocked_with_evidence(
            "mom_llama.chat_dispatch",
            "blocked_native_runtime",
            Blocker::new(
                "mention_targets_failed",
                "None of the invited local models completed a response.",
                vec!["Check the target Personas' model profiles and context budgets.".to_string()],
            ),
            vec![RuntimeStore::current()?.path().display().to_string()],
            Vec::new(),
            false,
            false,
        ));
    }
    let mut commit_db = load_db()?;
    let Some(host) = commit_db
        .conversations
        .iter_mut()
        .find(|conversation| conversation.id == input.conversation_id)
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "host_conversation_not_found",
                "The host conversation was removed while invited models were responding.",
                vec!["Open another chat and retry.".to_string()],
            ),
        ));
    };
    host.messages.push(user_message);
    host.active_leaf_message_id = Some(user_message_id.clone());
    host.updated_at = now_ms().to_string();
    append_attributed_results(host, &mut invocation);
    let host = host.clone();
    let conversation_path = commit_generated_exchange(
        commit_db,
        host,
        expected_active_leaf.as_deref(),
        &attachment_context.staged_ids,
        &user_message_id,
        true,
    )?;
    save_invocation(&invocation)?;
    Ok(CommandResult::passed(
        "mom_llama.chat_dispatch",
        "real_prompt_smoke_passed",
        ChatDispatchOutput::Mention {
            conversation_id: input.conversation_id,
            invocation,
        },
        vec![conversation_path.display().to_string()],
        Vec::new(),
        real_engine_invoked,
        false,
    ))
}

#[derive(Clone)]
struct ResolvedTarget {
    kind: MentionTargetKind,
    conversation: Conversation,
}

struct TargetResolution {
    targets: Vec<ResolvedTarget>,
    unresolved: Vec<String>,
    ambiguous: Vec<String>,
}

fn ambiguous_resolution_blocker(resolution: &TargetResolution) -> Option<Blocker> {
    (!resolution.ambiguous.is_empty()).then(|| {
        Blocker::new(
            "mention_target_ambiguous",
            format!(
                "These mentions match more than one saved participant: {}.",
                resolution
                    .ambiguous
                    .iter()
                    .map(|handle| format!("@{handle}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            vec![
                "Rename one of the conflicting handles in Personas or Consult groups.".to_string(),
            ],
        )
    })
}

struct PlannedTarget {
    snapshot: MentionTargetSnapshot,
    model_path: PathBuf,
    handle: NativeModelHandle,
    messages: Vec<ChatMessage>,
    cache_id: Option<String>,
    cache_reused: bool,
    cached_prefix: Option<llama_native_types::SequenceStateBlob>,
    tools: Vec<BoundMentionTool>,
}

struct MentionCancelLifecycle {
    invocation_id: String,
}

impl MentionCancelLifecycle {
    fn register(invocation_id: &str, targets: &[PlannedTarget]) -> Self {
        if let Ok(mut registry) = mention_cancel_registry().lock() {
            for target in targets {
                registry.insert(
                    (invocation_id.to_string(), target.snapshot.target_id.clone()),
                    Arc::new(AtomicBool::new(false)),
                );
            }
        }
        Self {
            invocation_id: invocation_id.to_string(),
        }
    }
}

impl Drop for MentionCancelLifecycle {
    fn drop(&mut self) {
        if let Ok(mut registry) = mention_cancel_registry().lock() {
            registry.retain(|(invocation_id, _), _| invocation_id != &self.invocation_id);
        }
    }
}

fn request_mention_cancellation(invocation_id: &str, target_id: Option<&str>) -> usize {
    mention_cancel_registry()
        .lock()
        .map(|registry| {
            registry
                .iter()
                .filter(|((candidate_invocation, candidate_target), _)| {
                    candidate_invocation == invocation_id
                        && target_id.is_none_or(|target| candidate_target == target)
                })
                .map(|(_, flag)| {
                    flag.store(true, Ordering::Release);
                    1usize
                })
                .sum()
        })
        .unwrap_or_default()
}

fn mention_cancellation_requested(invocation_id: &str, target_id: &str) -> bool {
    mention_cancel_registry()
        .lock()
        .ok()
        .and_then(|registry| {
            registry
                .get(&(invocation_id.to_string(), target_id.to_string()))
                .cloned()
        })
        .is_some_and(|flag| flag.load(Ordering::Acquire))
}

#[derive(Debug, Clone)]
struct BoundMentionTool {
    contract: McpTool,
    server: String,
    policy: ToolPermissionPolicy,
}

fn resolve_targets(handles: &[String], host_id: &str) -> Result<TargetResolution> {
    let (conversations, groups) = conversation_and_group_handles()?;
    Ok(resolve_targets_from_registry(
        handles,
        host_id,
        &conversations,
        &groups,
    ))
}

fn resolve_targets_from_registry(
    handles: &[String],
    host_id: &str,
    conversations: &[Conversation],
    groups: &[crate::personas::PersonaGroup],
) -> TargetResolution {
    let mut by_handle = BTreeMap::<String, Vec<&Conversation>>::new();
    for conversation in conversations {
        by_handle
            .entry(
                conversation
                    .execution_profile
                    .mention_handle
                    .to_ascii_lowercase(),
            )
            .or_default()
            .push(conversation);
    }
    let mut groups_by_handle = BTreeMap::<String, Vec<&crate::personas::PersonaGroup>>::new();
    for group in groups {
        groups_by_handle
            .entry(group.mention_handle.to_ascii_lowercase())
            .or_default()
            .push(group);
    }
    let by_id = conversations
        .iter()
        .map(|conversation| (conversation.id.as_str(), conversation))
        .collect::<BTreeMap<_, _>>();
    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    let mut ambiguous = Vec::new();
    let mut seen = BTreeSet::new();
    for handle in handles {
        let conversation_matches = by_handle.get(handle).map(Vec::as_slice).unwrap_or_default();
        let group_matches = groups_by_handle
            .get(handle)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if conversation_matches.len() + group_matches.len() > 1 {
            ambiguous.push(handle.clone());
            continue;
        }
        if let Some(group) = group_matches.first() {
            let mut group_targets = Vec::new();
            let mut group_seen = BTreeSet::new();
            let mut valid = !group.persona_ids.is_empty();
            for id in &group.persona_ids {
                if let Some(conversation) = by_id.get(id.as_str())
                    && conversation.kind == ConversationKind::PersonaTemplate
                    && group_seen.insert(conversation.id.clone())
                {
                    group_targets.push(ResolvedTarget {
                        kind: MentionTargetKind::Persona,
                        conversation: (*conversation).clone(),
                    });
                } else {
                    valid = false;
                }
            }
            if valid {
                for target in group_targets {
                    if seen.insert(target.conversation.id.clone()) {
                        resolved.push(target);
                    }
                }
            } else {
                unresolved.push(handle.clone());
            }
        } else if let Some(conversation) = conversation_matches.first()
            && conversation.id != host_id
            && seen.insert(conversation.id.clone())
        {
            resolved.push(ResolvedTarget {
                kind: if conversation.kind == ConversationKind::PersonaTemplate {
                    MentionTargetKind::Persona
                } else {
                    MentionTargetKind::LiveChat
                },
                conversation: (*conversation).clone(),
            });
        } else {
            unresolved.push(handle.clone());
        }
    }
    TargetResolution {
        targets: resolved,
        unresolved,
        ambiguous,
    }
}

fn snapshot_target(target: &ResolvedTarget) -> Result<MentionTargetSnapshot> {
    let source_messages = active_path_messages(&target.conversation);
    let encoded = serde_json::to_vec(&(
        &target.conversation.id,
        &target.conversation.execution_profile,
        &target.conversation.active_leaf_message_id,
        &source_messages,
    ))?;
    Ok(MentionTargetSnapshot {
        target_id: target.conversation.id.clone(),
        kind: target.kind,
        handle: target.conversation.execution_profile.mention_handle.clone(),
        label: target.conversation.title.clone(),
        version: target.conversation.execution_profile.version,
        source_leaf_message_id: target.conversation.active_leaf_message_id.clone(),
        profile: target.conversation.execution_profile.clone(),
        source_messages,
        snapshot_sha256: format!("{:x}", Sha256::digest(encoded)),
    })
}

#[derive(Debug)]
struct HandoffMessages {
    stable_prefix: Vec<ChatMessage>,
    messages: Vec<ChatMessage>,
}

fn resolve_mention_tools(
    bindings: &[crate::conversation_store::ToolBinding],
) -> std::result::Result<Vec<BoundMentionTool>, Blocker> {
    let mut tools = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let contract = resolve_tool_contract(&binding.server, &binding.tool)
            .map_err(|error| {
                Blocker::new(
                    "mention_tool_contract_failed",
                    error.to_string(),
                    vec!["Check this Persona's attached tools in Settings.".to_string()],
                )
            })?
            .map_err(|(_, blocker)| blocker)?;
        let policy = tool_permission_policy(&binding.server, &binding.tool).map_err(|error| {
            Blocker::new(
                "mention_tool_permission_failed",
                error.to_string(),
                vec!["Review local tool permissions in Settings.".to_string()],
            )
        })?;
        tools.push(BoundMentionTool {
            contract,
            server: binding.server.clone(),
            policy,
        });
    }
    Ok(tools)
}

fn mention_tool_instructions(tools: &[BoundMentionTool]) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let manifest = tools
        .iter()
        .map(|tool| {
            json!({
                "server": tool.server,
                "tool": tool.contract.name,
                "description": tool.contract.description,
                "input_schema": tool.contract.input_schema,
                "permission": match tool.policy {
                    ToolPermissionPolicy::Ask => "ask",
                    ToolPermissionPolicy::AlwaysAllow => "allow",
                    ToolPermissionPolicy::Deny => "deny",
                }
            })
        })
        .collect::<Vec<_>>();
    format!(
        " Attached local tools are restricted to this manifest: {}. A denied tool is unavailable. If a tool call is necessary, return only {{\"action\":\"call\",\"server\":\"...\",\"tool\":\"...\",\"arguments\":{{...}}}}. Otherwise answer normally.",
        serde_json::to_string(&manifest).unwrap_or_else(|_| "[]".to_string())
    )
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum MentionToolDecision {
    Call {
        server: String,
        tool: String,
        arguments: Value,
    },
    Final {
        answer: String,
    },
}

fn finish_tool_bound_mention(
    target: &PlannedTarget,
    mut output: GenerationOutput,
    invocation_id: &str,
    settings: &Settings,
) -> std::result::Result<(GenerationOutput, Vec<String>), Blocker> {
    let max_turns = upstream_setting_i64(settings, "agenticMaxTurns")
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(4)
        .clamp(1, 8);
    let mut messages = target.messages.clone();
    let mut receipts = Vec::new();
    for _ in 0..max_turns {
        let Some(decision) = parse_mention_tool_decision(&output.text) else {
            return Ok((output, receipts));
        };
        match decision {
            MentionToolDecision::Final { answer } => {
                output.text = answer;
                return Ok((output, receipts));
            }
            MentionToolDecision::Call {
                server,
                tool,
                arguments,
            } => {
                authorize_bound_tool(
                    &target.tools,
                    &server,
                    &tool,
                    &arguments,
                    &target.snapshot.handle,
                )?;
                let call = mcp_call_tool_supervised(&server, &tool, arguments.clone(), &|| {
                    mention_cancellation_requested(invocation_id, &target.snapshot.target_id)
                })
                .map_err(|error| {
                    Blocker::new(
                        "mention_tool_call_failed",
                        error.to_string(),
                        vec!["Check the attached MCP server in Settings.".to_string()],
                    )
                })?;
                if call.status == "blocked" {
                    return Err(call.blocker.unwrap_or_else(|| {
                        Blocker::new(
                            "mention_tool_call_blocked",
                            "The attached local tool was blocked.",
                            vec!["Check local tool permissions and server status.".to_string()],
                        )
                    }));
                }
                receipts.push(call.receipt.task_id);
                let content = call
                    .result
                    .map(|result| result.content)
                    .unwrap_or(Value::Null);
                messages.push(ChatMessage {
                    role: ChatRole::Assistant,
                    content: output.text,
                });
                messages.push(ChatMessage {
                    role: ChatRole::Tool,
                    content: serde_json::to_string(&json!({
                        "server": server,
                        "tool": tool,
                        "arguments": arguments,
                        "result": content,
                    }))
                    .unwrap_or_else(|_| "Local tool returned an unreadable result.".to_string()),
                });
                let ticket = target
                    .handle
                    .generate_shared_prefix(SharedPrefixBatchRequest {
                        request_id: invocation_id.to_string(),
                        model_id: target.handle.status().model_id,
                        common_messages: Vec::new(),
                        chat_template: profile_chat_template(&target.snapshot.profile),
                        branches: vec![BranchRequest {
                            branch_id: target.snapshot.target_id.clone(),
                            label: target.snapshot.label.clone(),
                            instruction: String::new(),
                            sampling: target
                                .snapshot
                                .profile
                                .sampling
                                .clone()
                                .unwrap_or_else(|| settings.sampling_config()),
                            messages: messages.clone(),
                            cached_prefix: None,
                        }],
                        cached_prefix: None,
                    })
                    .map_err(|error| {
                        Blocker::new(
                            "mention_tool_followup_failed",
                            error.message,
                            vec!["Retry the Persona response.".to_string()],
                        )
                    })?;
                output = ticket
                    .wait()
                    .map_err(|error| {
                        Blocker::new(
                            "mention_tool_followup_failed",
                            error.message,
                            vec!["Retry the Persona response.".to_string()],
                        )
                    })?
                    .into_iter()
                    .next()
                    .ok_or_else(|| {
                        Blocker::new(
                            "mention_tool_followup_missing",
                            "The local model returned no answer after the tool call.",
                            vec!["Retry the Persona response.".to_string()],
                        )
                    })?;
                if output.state != GenerationState::Completed {
                    return Err(Blocker::new(
                        "mention_tool_followup_incomplete",
                        "The local model did not complete after the tool call.",
                        vec!["Review the partial response before retrying.".to_string()],
                    ));
                }
            }
        }
    }
    Err(Blocker::new(
        "mention_tool_turn_limit_reached",
        format!(
            "@{} requested another tool call after the bounded {max_turns}-turn limit.",
            target.snapshot.handle
        ),
        vec!["Simplify the request or raise the bounded tool-turn setting.".to_string()],
    ))
}

fn authorize_bound_tool<'a>(
    tools: &'a [BoundMentionTool],
    server: &str,
    tool: &str,
    arguments: &Value,
    handle: &str,
) -> std::result::Result<&'a BoundMentionTool, Blocker> {
    let Some(binding) = tools
        .iter()
        .find(|candidate| candidate.server == server && candidate.contract.name == tool)
    else {
        return Err(Blocker::new(
            "mention_tool_not_attached",
            format!("@{handle} requested a tool that is not attached to this Persona."),
            vec!["Review the Persona's attached tools in Settings.".to_string()],
        ));
    };
    if let Some(blocker) = validate_tool_arguments(&binding.contract.input_schema, arguments) {
        return Err(blocker);
    }
    match binding.policy {
        ToolPermissionPolicy::Deny => Err(Blocker::new(
            "tool_permission_denied",
            format!("Tool `{server}/{tool}` is denied by local policy."),
            vec!["Change or remove the tool permission in Settings.".to_string()],
        )),
        ToolPermissionPolicy::Ask => Err(Blocker::new(
            "mention_tool_approval_required",
            format!(
                "Tool `{server}/{tool}` needs explicit approval before this Persona can continue."
            ),
            vec![
                "Approve the exact tool call, or set an always-allow policy in Settings."
                    .to_string(),
            ],
        )),
        ToolPermissionPolicy::AlwaysAllow => Ok(binding),
    }
}

fn parse_mention_tool_decision(value: &str) -> Option<MentionToolDecision> {
    let trimmed = value.trim();
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    serde_json::from_str(&trimmed[start..=end]).ok()
}

fn handoff_messages(
    handle: &NativeModelHandle,
    settings: &Settings,
    snapshot: &MentionTargetSnapshot,
    host: &[Message],
    addressed: &str,
    participants: &str,
    tools: &[BoundMentionTool],
) -> std::result::Result<HandoffMessages, Blocker> {
    let system = snapshot
        .profile
        .system_message
        .as_deref()
        .filter(|message| !message.trim().is_empty())
        .map(|content| ChatMessage {
            role: ChatRole::System,
            content: content.to_string(),
        });
    let source_messages =
        attachment_enriched_messages(&snapshot.target_id, &snapshot.source_messages)?;
    let host_conversation_id = host
        .first()
        .map(|message| message.conversation_id.as_str())
        .unwrap_or_default();
    let host_messages = attachment_enriched_messages(host_conversation_id, host)?;
    let source_candidates = source_messages
        .iter()
        .flat_map(|message| native_context_messages(message, false))
        .collect::<Vec<_>>();
    let host_candidates = host_messages
        .iter()
        .flat_map(|message| native_context_messages(message, false))
        .collect::<Vec<_>>();
    let template = profile_chat_template(&snapshot.profile);
    let source = recent_within_budget(
        handle,
        source_candidates,
        snapshot.profile.source_history_tokens,
        &template,
    );
    let mut host = recent_within_budget(
        handle,
        host_candidates,
        snapshot.profile.host_context_tokens,
        &template,
    );
    let boundary = ChatMessage {
        role: ChatRole::System,
        content: format!(
            "You are @{}, temporarily invited from a separate local conversation. Your source history above is an immutable snapshot and will not be changed by this reply. Recent host context follows. Reply directly to the final addressed message as your established perspective. Do not claim access to omitted history. Addressed participants: {}.{}",
            snapshot.handle,
            participants,
            mention_tool_instructions(tools)
        ),
    };
    let final_message = ChatMessage {
        role: ChatRole::User,
        content: addressed.trim().to_string(),
    };
    let mut messages = Vec::new();
    if let Some(system) = system {
        messages.push(system);
    }
    messages.extend(source.clone());
    messages.push(boundary.clone());
    messages.append(&mut host);
    messages.push(final_message.clone());
    let output_reserve = snapshot
        .profile
        .sampling
        .as_ref()
        .map(|sampling| sampling.max_tokens)
        .unwrap_or(settings.default_max_tokens) as usize;
    fit_handoff_to_context(
        messages,
        &boundary,
        output_reserve,
        settings.context_tokens as usize,
        |candidate| {
            handle
                .tokenize_messages_with_template(candidate.to_vec(), template.clone())
                .map(|tokens| tokens.token_ids.len())
                .map_err(|error| {
                    Blocker::new(
                        "mention_context_tokenization_failed",
                        error.message,
                        vec!["Check the target model's chat template.".to_string()],
                    )
                })
        },
    )
}

fn attachment_enriched_messages(
    conversation_id: &str,
    messages: &[Message],
) -> std::result::Result<Vec<Message>, Blocker> {
    if conversation_id.is_empty()
        || messages
            .iter()
            .all(|message| message.attachment_ids.is_empty())
    {
        return Ok(messages.to_vec());
    }
    let context = prepare_chat_attachments(conversation_id, messages, Some("__snapshot__"))
        .map_err(|_| {
            Blocker::new(
                "mention_attachment_context_failed",
                "An invited conversation's attachment context could not be loaded.",
                vec!["Open the source chat and inspect its attachments.".to_string()],
            )
        })?
        .map_err(|blocked| blocked.blocker)?;
    if !context.media.is_empty() {
        return Err(Blocker::new(
            "mention_source_multimodal_attachment_unsupported",
            "An invited source contains image or audio context that the shared-prefix mention dispatcher cannot preserve yet.",
            vec!["Use a text-only source branch for this Persona invocation.".to_string()],
        ));
    }
    Ok(messages
        .iter()
        .cloned()
        .map(|mut message| {
            if let Some(attachment_text) = context.text_by_message_id.get(&message.id) {
                message.content = append_attachment_context(&message.content, attachment_text);
            }
            message
        })
        .collect())
}

fn fit_handoff_to_context(
    mut messages: Vec<ChatMessage>,
    boundary: &ChatMessage,
    output_reserve: usize,
    context_tokens: usize,
    mut token_count: impl FnMut(&[ChatMessage]) -> std::result::Result<usize, Blocker>,
) -> std::result::Result<HandoffMessages, Blocker> {
    loop {
        let tokens = token_count(&messages)?;
        if tokens.saturating_add(output_reserve) <= context_tokens {
            let boundary_index = messages
                .iter()
                .position(|message| message == boundary)
                .unwrap_or_default();
            return Ok(HandoffMessages {
                stable_prefix: messages[..boundary_index].to_vec(),
                messages,
            });
        }
        let boundary_index = messages
            .iter()
            .position(|message| message == boundary)
            .unwrap_or_default();
        if messages.len() > boundary_index + 2 {
            messages.remove(boundary_index + 1);
            continue;
        }
        let first_non_system = usize::from(
            messages
                .first()
                .is_some_and(|message| message.role == ChatRole::System),
        );
        if boundary_index > first_non_system {
            messages.remove(first_non_system);
            continue;
        }
        return Err(Blocker::new(
            "mention_context_too_large",
            "The persona instructions and addressed message do not fit the target model context.",
            vec!["Reduce the persona system message or output-token setting.".to_string()],
        ));
    }
}

fn recent_within_budget(
    handle: &NativeModelHandle,
    messages: Vec<ChatMessage>,
    budget: u32,
    chat_template: &ChatTemplateChoice,
) -> Vec<ChatMessage> {
    if budget == 0 {
        return Vec::new();
    }
    let mut selected = Vec::new();
    for message in messages.into_iter().rev() {
        let mut candidate = vec![message];
        candidate.extend(selected.clone());
        let fits = handle
            .tokenize_messages_with_template(candidate.clone(), chat_template.clone())
            .map(|tokens| tokens.token_ids.len() <= budget as usize)
            .unwrap_or(false);
        if !fits {
            break;
        }
        selected = candidate;
    }
    selected
}

fn profile_chat_template(profile: &ConversationExecutionProfile) -> ChatTemplateChoice {
    match &profile.chat_template {
        crate::conversation_store::ChatTemplatePolicy::ModelDefault => {
            ChatTemplateChoice::ModelDefault
        }
        crate::conversation_store::ChatTemplatePolicy::FrozenSource(template) => {
            ChatTemplateChoice::Override(template.clone())
        }
    }
}

fn append_attributed_results(host: &mut Conversation, invocation: &mut MentionInvocation) {
    let mut parent = Some(invocation.user_message_id.clone());
    for (order, result) in invocation.results.iter_mut().enumerate() {
        if result.state != GenerationState::Completed || result.text.trim().is_empty() {
            continue;
        }
        let snapshot = invocation
            .targets
            .iter()
            .find(|target| target.target_id == result.target_id)
            .expect("mention result target snapshot");
        let message_id = Uuid::new_v4().to_string();
        host.messages.push(Message {
            id: message_id.clone(),
            conversation_id: host.id.clone(),
            role: MessageRole::Assistant,
            content: result.text.clone(),
            created_at: now_ms().to_string(),
            parent_id: parent.clone(),
            model: Some(result.model_id.clone()),
            receipt_id: Some(format!("mom_llama.mention_dispatch:{}", invocation.id)),
            prompt_tokens: Some(result.metrics.prompt_tokens),
            completion_tokens: Some(result.metrics.completion_tokens),
            reasoning_content: None,
            reasoning_incomplete: false,
            branch_index: None,
            branch_count: None,
            attribution: Some(MessageAttribution {
                kind: match snapshot.kind {
                    MentionTargetKind::Persona => MessageSpeakerKind::Persona,
                    MentionTargetKind::LiveChat => MessageSpeakerKind::LiveChat,
                    MentionTargetKind::Group => MessageSpeakerKind::Persona,
                },
                source_id: snapshot.target_id.clone(),
                handle: snapshot.handle.clone(),
                label: snapshot.label.clone(),
                version: snapshot.version,
                invocation_id: invocation.id.clone(),
                target_order: order,
            }),
            attachment_ids: Vec::new(),
        });
        result.message_id = Some(message_id.clone());
        parent = Some(message_id);
    }
    if parent.as_deref() != Some(invocation.user_message_id.as_str()) {
        host.active_leaf_message_id = parent;
    }
    host.updated_at = now_ms().to_string();
}

fn save_invocation(invocation: &MentionInvocation) -> Result<()> {
    RuntimeStore::current()?.mutate(INVOCATIONS_NAMESPACE, MentionInvocationDb::default, |db| {
        db.invocations.retain(|item| item.id != invocation.id);
        db.invocations.insert(0, invocation.clone());
        Ok(())
    })
}

fn blocked_target_result(
    snapshot: &MentionTargetSnapshot,
    state: GenerationState,
    message: &str,
) -> MentionTargetResult {
    MentionTargetResult {
        target_id: snapshot.target_id.clone(),
        handle: snapshot.handle.clone(),
        label: snapshot.label.clone(),
        state,
        text: message.to_string(),
        model_id: String::new(),
        message_id: None,
        metrics: GenerationMetrics::default(),
        cache_id: None,
        cache_reused: false,
        tool_receipt_ids: Vec::new(),
        real_engine_invoked: false,
        fake_fixture: false,
    }
}

fn invocation_state(results: &[MentionTargetResult]) -> MentionInvocationState {
    let completed = results
        .iter()
        .filter(|result| result.state == GenerationState::Completed)
        .count();
    let cancelled = results
        .iter()
        .filter(|result| result.state == GenerationState::Cancelled)
        .count();
    if completed == results.len() && completed > 0 {
        MentionInvocationState::Completed
    } else if completed > 0 {
        MentionInvocationState::PartiallyCompleted
    } else if cancelled == results.len() && cancelled > 0 {
        MentionInvocationState::Cancelled
    } else {
        MentionInvocationState::Failed
    }
}

fn parse_handles(message: &str) -> Vec<String> {
    let mut handles = Vec::new();
    for token in mention_tokens(message) {
        let handle = token.handle.to_ascii_lowercase();
        if !handles.contains(&handle) {
            handles.push(handle);
        }
    }
    handles
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MentionToken<'a> {
    start: usize,
    end: usize,
    handle: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeDelimiter {
    marker: char,
    width: usize,
}

fn mention_tokens(message: &str) -> Vec<MentionToken<'_>> {
    let chars = message.char_indices().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut code_delimiter: Option<CodeDelimiter> = None;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index].1;
        if character == '`' || character == '~' {
            let marker = character;
            let mut width = 1;
            while index + width < chars.len() && chars[index + width].1 == marker {
                width += 1;
            }
            match code_delimiter {
                Some(open) if open.marker == marker && open.width == width => {
                    code_delimiter = None;
                }
                None if marker == '`' || width >= 3 => {
                    code_delimiter = Some(CodeDelimiter { marker, width });
                }
                _ => {}
            }
            index += width;
            continue;
        }
        let explicit_boundary = index == 0 || chars[index - 1].1.is_whitespace();
        if code_delimiter.is_none()
            && character == '@'
            && explicit_boundary
            && !is_indented_code_position(message, chars[index].0)
        {
            let start = chars[index].0 + 1;
            let mut end = start;
            index += 1;
            while index < chars.len()
                && (chars[index].1.is_ascii_alphanumeric() || chars[index].1 == '-')
            {
                end = chars[index].0 + chars[index].1.len_utf8();
                index += 1;
            }
            if end > start {
                tokens.push(MentionToken {
                    start: start - 1,
                    end,
                    handle: &message[start..end],
                });
            }
        } else {
            index += 1;
        }
    }
    tokens
}

fn is_indented_code_position(message: &str, byte_index: usize) -> bool {
    let line_start = message[..byte_index]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let prefix = &message[line_start..byte_index];
    prefix.starts_with('\t') || (prefix.len() >= 4 && prefix.bytes().all(|byte| byte == b' '))
}

fn strip_handles(message: &str, targets: &[ResolvedTarget]) -> String {
    let target_handles = targets
        .iter()
        .map(|target| {
            target
                .conversation
                .execution_profile
                .mention_handle
                .to_ascii_lowercase()
        })
        .collect::<BTreeSet<_>>();
    let removals = mention_tokens(message)
        .into_iter()
        .filter(|token| target_handles.contains(&token.handle.to_ascii_lowercase()))
        .map(|token| {
            let mut end = token.end;
            for (offset, character) in message[token.end..].char_indices() {
                if character.is_whitespace() || character.is_ascii_alphanumeric() {
                    break;
                }
                end = token.end + offset + character.len_utf8();
            }
            token.start..end
        })
        .collect::<Vec<_>>();
    let mut stripped = String::with_capacity(message.len());
    let mut cursor = 0;
    for removal in removals {
        stripped.push_str(&message[cursor..removal.start]);
        cursor = removal.end;
    }
    stripped.push_str(&message[cursor..]);
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
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

fn cache_owner(snapshot: &MentionTargetSnapshot) -> String {
    format!(
        "mention:{}:{}:{}",
        snapshot.target_id, snapshot.version, snapshot.snapshot_sha256
    )
}

fn emit<F>(callback: &mut Option<F>, event: MentionStreamEvent) -> Result<()>
where
    F: FnMut(ChatDispatchStreamEvent) -> Result<()>,
{
    if let Some(callback) = callback.as_mut() {
        callback(ChatDispatchStreamEvent::Mention(event))?;
    }
    Ok(())
}

const fn candidate_rank(kind: MentionTargetKind) -> u8 {
    match kind {
        MentionTargetKind::Persona => 0,
        MentionTargetKind::Group => 1,
        MentionTargetKind::LiveChat => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BoundMentionTool, ambiguous_resolution_blocker, authorize_bound_tool,
        fit_handoff_to_context, parse_handles, parse_mention_tool_decision,
        resolve_targets_from_registry,
    };
    use crate::conversation_store::{Conversation, ConversationExecutionProfile, ConversationKind};
    use crate::mcp::McpTool;
    use crate::tool_loop::ToolPermissionPolicy;
    use llama_native_types::{ChatMessage, ChatRole};
    use serde_json::json;

    fn handoff_message(role: ChatRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.to_string(),
        }
    }

    fn mention_conversation(id: &str, handle: &str) -> Conversation {
        Conversation {
            id: id.to_string(),
            title: id.to_string(),
            created_at: "1".to_string(),
            updated_at: "1".to_string(),
            kind: ConversationKind::PersonaTemplate,
            execution_profile: ConversationExecutionProfile {
                mention_handle: handle.to_string(),
                ..ConversationExecutionProfile::default()
            },
            selected_model_path: None,
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: None,
            current_skill_ids: Vec::new(),
            messages: Vec::new(),
        }
    }

    #[test]
    fn mention_parser_is_stable_and_deduplicates_handles() {
        assert_eq!(
            parse_handles("Ask @evidence-lens and @whole-person, then @evidence-lens."),
            vec!["evidence-lens", "whole-person"]
        );
    }

    #[test]
    fn mention_parser_requires_the_explicit_composer_boundary() {
        assert_eq!(
            parse_handles("@leading then\t@after-tab and\n@after-newline"),
            vec!["leading", "after-tab", "after-newline"]
        );
        assert!(parse_handles("mail@example.com prefix@embedded (@parenthesized)").is_empty());
    }

    #[test]
    fn mention_parser_ignores_markdown_code() {
        assert!(
            parse_handles(
                "`@inline` and ``@wide-inline``\n```text\n@fenced\n```\n~~~\n@tilde-fenced\n~~~\n    @indented"
            )
            .is_empty()
        );
    }

    #[test]
    fn duplicate_case_insensitive_registry_handles_are_ambiguous_not_last_wins() {
        let conversations = vec![
            mention_conversation("first", "same-lens"),
            mention_conversation("second", "SAME-LENS"),
        ];
        let resolution =
            resolve_targets_from_registry(&["same-lens".to_string()], "host", &conversations, &[]);
        assert!(resolution.targets.is_empty());
        assert!(resolution.unresolved.is_empty());
        assert_eq!(resolution.ambiguous, vec!["same-lens"]);
        assert_eq!(
            ambiguous_resolution_blocker(&resolution)
                .expect("ambiguous registry must produce a blocker")
                .code,
            "mention_target_ambiguous"
        );
    }

    #[test]
    fn mention_tool_calls_must_match_an_attached_allowlisted_binding_and_schema() {
        let tool = |policy| BoundMentionTool {
            server: "local".to_string(),
            policy,
            contract: McpTool {
                name: "lookup".to_string(),
                description: None,
                input_schema: json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": {"query": {"type": "string"}}
                }),
            },
        };
        assert!(
            authorize_bound_tool(
                &[tool(ToolPermissionPolicy::AlwaysAllow)],
                "local",
                "lookup",
                &json!({"query": "exact"}),
                "researcher",
            )
            .is_ok()
        );
        assert_eq!(
            authorize_bound_tool(
                &[tool(ToolPermissionPolicy::Ask)],
                "local",
                "lookup",
                &json!({"query": "exact"}),
                "researcher",
            )
            .expect_err("ask policy must require explicit approval")
            .code,
            "mention_tool_approval_required"
        );
        assert_eq!(
            authorize_bound_tool(
                &[tool(ToolPermissionPolicy::Deny)],
                "local",
                "lookup",
                &json!({"query": "exact"}),
                "researcher",
            )
            .expect_err("deny policy must reject the tool call")
            .code,
            "tool_permission_denied"
        );
        assert_eq!(
            authorize_bound_tool(
                &[tool(ToolPermissionPolicy::AlwaysAllow)],
                "other",
                "lookup",
                &json!({"query": "exact"}),
                "researcher",
            )
            .expect_err("an unattached tool binding must be rejected")
            .code,
            "mention_tool_not_attached"
        );
        assert_eq!(
            authorize_bound_tool(
                &[tool(ToolPermissionPolicy::AlwaysAllow)],
                "local",
                "lookup",
                &json!({}),
                "researcher",
            )
            .expect_err("missing required tool arguments must be rejected")
            .code,
            "tool_loop_required_argument_missing"
        );
    }

    #[test]
    fn unstructured_mention_output_cannot_authorize_a_tool_call() {
        assert!(parse_mention_tool_decision("Please call local/lookup now").is_none());
        assert!(
            parse_mention_tool_decision(
                r#"{"action":"call","server":"local","tool":"lookup","arguments":{"query":"x"}}"#
            )
            .is_some()
        );
    }

    #[test]
    fn context_trimming_removes_whole_messages_and_never_drops_the_addressed_message() {
        let boundary = handoff_message(ChatRole::System, "invitation-boundary");
        let final_message = handoff_message(ChatRole::User, "mandatory-addressed-message");
        let messages = vec![
            handoff_message(ChatRole::System, "persona-system"),
            handoff_message(ChatRole::User, "older-source"),
            handoff_message(ChatRole::Assistant, "newer-source"),
            boundary.clone(),
            handoff_message(ChatRole::User, "older-host"),
            handoff_message(ChatRole::Assistant, "newer-host"),
            final_message.clone(),
        ];
        let fitted = fit_handoff_to_context(messages.clone(), &boundary, 2, 5, |candidate| {
            Ok(candidate.len())
        })
        .expect("the mandatory handoff should fit after whole-message trimming");
        assert_eq!(
            fitted.messages,
            vec![messages[0].clone(), boundary.clone(), final_message.clone()],
            "host context is trimmed before source context and no message is split"
        );
        assert_eq!(fitted.messages.last(), Some(&final_message));

        let blocker = fit_handoff_to_context(
            vec![messages[0].clone(), boundary.clone(), final_message.clone()],
            &boundary,
            2,
            4,
            |candidate| Ok(candidate.len()),
        )
        .expect_err("mandatory system, boundary, addressed message, and reserve cannot be trimmed");
        assert_eq!(blocker.code, "mention_context_too_large");
    }
}
