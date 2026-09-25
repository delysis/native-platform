use crate::attachments::{commit_generated_exchange_with_journal, prepare_chat_attachments};
mod execution;
mod handoff;
use handoff::handoff_messages;
use crate::attachments::{CurrentAttachmentSelection, prepare_scoped_chat_attachments};
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
#[cfg(any(target_os = "macos", target_os = "linux"))]
use fs2::FileExt;
use llama_native_engine::{ControlledGenerationSubmission, NativeModelHandle};

                vec!["Open another chat and retry.".to_string()],
            ),
        ));
    };
    let host_snapshot = crate::document::checked_active_messages(&db.conversations[host_index])?;
    let attachment_context =
        match prepare_scoped_chat_attachments(&input.conversation_id, &host_snapshot, CurrentAttachmentSelection::Draft)? {
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
            &attachment_context,
            &input.conversation_id,
            &user_message_id,
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
        let cache_use = if handoff.media.is_empty() { match snapshot.profile.chat_template {
            crate::conversation_store::ChatTemplatePolicy::ModelDefault => ensure_persona_prefix(
                &handle,
                &cache_owner(snapshot),
                &format!("Invited context for @{}", snapshot.handle),
                &handoff.stable_prefix,
                &handoff.messages,
            )?,
            crate::conversation_store::ChatTemplatePolicy::FrozenSource(_) => None,
        } } else { None };
        handoff::retain_input_receipt(&settings.data_dir, &invocation_id,
            &input.conversation_id, &user_message_id, &handoff.receipt)?;
        planned.push(PlannedTarget {
            snapshot: snapshot.clone(),
            model_path: model_path.to_path_buf(),
            model_config,
            handle,
            messages: handoff.messages,
            cache_id: cache_use.as_ref().map(|cache| cache.cache_id.clone()),
            media: handoff.media,
            model_fingerprint: handoff.receipt.model_fingerprint,
            text_prompt_tokens: handoff.text_prompt_tokens,
            cached_prefix: cache_use.map(|cache| cache.sequence),
            tools,
        });
    }

    let mut pending_continuations = Vec::new();
    let completions = execution::execute_groups(scope, &invocation_id, planned,
        &settings, options, on_event)?;
    for (outcome, targets) in completions {
        match outcome {
            Ok(outputs) => {
                for output in outputs {
                    if let Some(target) = targets
                        .iter()
                        .find(|target| target.snapshot.target_id == output.branch_id)
                    {

                        } else {
                            (output, Vec::new())
                        };
                        let cancelled = execution::terminal_cancelled(scope, &invocation_id,
                            &target.snapshot.target_id)?;
                        let cache_reused = execution::cache_was_reused(&output.metrics.cache);
                        // Retained native output remains immutable; cancellation only
                        // removes its authority to become a new conversation message.
                        invocation.results.push(MentionTargetResult {
                            target_id: target.snapshot.target_id.clone(),
                            handle: target.snapshot.handle.clone(),
                            label: target.snapshot.label.clone(),
                            state: if cancelled { GenerationState::Cancelled } else { output.state },
                            text: if cancelled { String::new() } else { strip_reserved_attribution_prefix(&output.text) },
                            model_id: output.model_id,
                            message_id: None,
                            metrics: output.metrics,
                            cache_id: target.cache_id.clone(),
                            cache_reused,
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
                        if error.code == llama_native_types::NativeErrorCode::Cancelled {
                            GenerationState::Cancelled
                        } else { GenerationState::Failed },
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
    let has_completed_response = invocation.results.iter().any(|result| {
        result.real_engine_invoked
            && !result.fake_fixture
            && result.state == GenerationState::Completed
            && !result.text.trim().is_empty()
    }) || pending_continuations.iter().any(|continuation| {
        continuation.provisional_output.real_engine_invoked
            && !continuation.provisional_output.fake_fixture
            && continuation.provisional_output.state == GenerationState::Completed
    });
    let real_engine_invoked = invocation.results.iter()
        .any(|result| result.real_engine_invoked && !result.fake_fixture)
        || pending_continuations.iter().any(|continuation|
            continuation.provisional_output.real_engine_invoked && !continuation.provisional_output.fake_fixture);
    if !has_completed_response {
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
            real_engine_invoked,
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
    media: Vec<llama_native_types::MediaInput>,
    model_fingerprint: ModelFingerprint,
    text_prompt_tokens: usize,
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
    let source_messages = crate::document::checked_active_messages(&target.conversation)?;
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

#[cfg(test)]
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

// Retain the existing small fitter only as a regression fixture. Production
// admission uses whole Message units through workspace_document::context.
#[cfg(test)]
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

fn profile_chat_template(profile: &ConversationExecutionProfile) -> ChatTemplateChoice {
    match &profile.chat_template {
        crate::conversation_store::ChatTemplatePolicy::ModelDefault => {
            ChatTemplateChoice::ModelDefault
        }
