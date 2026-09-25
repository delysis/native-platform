use crate::attachments::{commit_generated_exchange_with_journal, prepare_chat_attachments};
use crate::chat::{
    ChatSendInput, ChatSendOptions, ChatSendOutput, ChatStreamEvent, native_context_messages,
};
use crate::config::{Settings, resolve_settings};
use crate::conversation_store::{
    CONVERSATIONS_NAMESPACE, Conversation, ConversationDb, ConversationExecutionProfile,
    ConversationKind, Message, MessageAttribution, MessageRole, MessageSpeakerKind, active_leaf_id,
    active_path_messages, load_db, strip_reserved_attribution_prefix, upsert_conversation,
};
use crate::kv_cache::ensure_persona_prefix;

use crate::tool_loop::{ToolPermissionPolicy, tool_permission_policy, validate_tool_arguments};
use anyhow::{Result, anyhow};
use crossbeam_channel::TryRecvError;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use fs2::FileExt;
use llama_native_engine::{ControlledGenerationSubmission, NativeModelHandle};

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
    if attachment_context
        .draft_snapshot
        .as_ref()
        .is_some_and(|draft| draft.message != input.message)
    {
        return Ok(CommandResult::blocked(
            "mom_llama.chat_dispatch",
            "stub_blocked",
            Blocker::new(
                "draft_changed_before_send",
                "The saved draft changed before this exact Persona message could be admitted.",
                vec!["Refresh the composer and send the current draft.".to_string()],
            ),
        ));
    }
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
            model_config,
            handle,
            messages: handoff.messages,
            cache_id: cache_use.as_ref().map(|cache| cache.cache_id.clone()),
            cache_reused: cache_use.as_ref().is_some_and(|cache| cache.reused),
            cached_prefix: cache_use.map(|cache| cache.sequence),
            tools,
        });
    }

    let mut groups = BTreeMap::<String, Vec<PlannedTarget>>::new();
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
    let mut pending_continuations = Vec::new();
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
    invocation.state = if invocation
        .tool_approvals
        .iter()
        .any(|approval| approval.state == MentionToolApprovalState::Pending)
    {
        MentionInvocationState::AwaitingApproval
    } else {
        invocation_state(&invocation.results)
    };
    invocation.updated_at = now_ms().to_string();
    let real_engine_invoked = invocation.results.iter().any(|result| {
        result.real_engine_invoked
            && !result.fake_fixture
            && result.state == GenerationState::Completed
            && !result.text.trim().is_empty()
    }) || pending_continuations.iter().any(|continuation| {
        continuation.provisional_output.real_engine_invoked
            && !continuation.provisional_output.fake_fixture
            && continuation.provisional_output.state == GenerationState::Completed
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

    })
}

struct PlannedTarget {
    snapshot: MentionTargetSnapshot,
    model_path: PathBuf,
    model_config: NativeModelConfig,
    handle: NativeModelHandle,
    messages: Vec<ChatMessage>,
    cache_id: Option<String>,
    cache_reused: bool,
    cached_prefix: Option<llama_native_types::SequenceStateBlob>,
    tools: Vec<BoundMentionTool>,
}

enum ToolBoundMentionFinish {
    Final {
        output: GenerationOutput,

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

        ToolPermissionPolicy::AlwaysAllow => unreachable!("Persona policy is normalized"),
    }
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
        content: addressed.to_string(),
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
