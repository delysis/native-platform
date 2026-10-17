//! A bounded inference projection; saved conversation history is never trimmed.
use crate::attachments::{
    AttachmentContextBlocker, ChatAttachmentContext, prepare_call_greeting_history,
    prepare_live_audio_history, prepare_voice_reply_attachments,
};
use crate::chat::{build_native_messages, upstream_setting_bool};
use crate::config::Settings;
use crate::conversation_store::{Conversation, ConversationInferenceWindow, Message, MessageRole};
use anyhow::{Result, anyhow};
use llama_native_engine::NativeModelHandle;
use llama_native_types::{ChatMessage, ChatTemplateChoice, GenerationInput};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
pub(crate) enum VoiceHistoryMode {
    Live,
    Recorded,
    Greeting,
}

pub(crate) struct VoicePrompt<'a> {
    pub settings: &'a Settings,
    pub system: &'a str,
    pub skill: &'a str,
    pub current: &'a str,
    pub template: ChatTemplateChoice,
    pub completion_tokens: usize,
    pub mode: VoiceHistoryMode,
}

pub(crate) struct VoiceProjection {
    pub attachments: ChatAttachmentContext,
    pub messages: Vec<ChatMessage>,
    pub prompt_tokens: usize,
    pub omitted_history_messages: usize,
}

pub(crate) fn fit_voice_context(
    handle: &NativeModelHandle,
    conversation: &mut Conversation,
    active: &[Message],
    prompt: VoicePrompt<'_>,
) -> Result<VoiceProjection> {
    let fingerprint = handle.status().fingerprint;
    let context = fingerprint
        .as_ref()
        .map_or(prompt.settings.context_tokens, |f| f.context_tokens) as usize;
    // Future microphone samples need their own budget. The worker also checks
    // every live block against the actual context, including closing tokens.
    let microphone_reserve = if matches!(prompt.mode, VoiceHistoryMode::Live) {
        (context / 4).min(4096)
    } else {
        0
    };
    let limit = context.saturating_sub(microphone_reserve + prompt.completion_tokens + 32);
    let profile = digest(&(
        &conversation.execution_profile,
        prompt.system,
        prompt.skill,
        &prompt.template,
        &fingerprint,
        upstream_setting_bool(prompt.settings, "excludeReasoningFromContext"),
    ))?;
    let start = valid_window_start(active, conversation.inference_window.as_ref(), &profile)?;
    let (start, mut projection) = fit_window(active, start, limit, |selected| {
        let attachments = if !matches!(prompt.mode, VoiceHistoryMode::Recorded)
            && selected
                .iter()
                .all(|message| message.attachment_ids.is_empty())
        {
            Ok(ChatAttachmentContext {
                staged_ids: Vec::new(),
                draft_snapshot: None,
                text_by_message_id: Default::default(),
                current_text: String::new(),
                media: Vec::new(),
            })
        } else {
            match prompt.mode {
                VoiceHistoryMode::Live => prepare_live_audio_history(&conversation.id, selected)?,
                VoiceHistoryMode::Recorded => {
                    prepare_voice_reply_attachments(&conversation.id, selected)?
                }
                VoiceHistoryMode::Greeting => {
                    prepare_call_greeting_history(&conversation.id, selected)?
                }
            }
        };
        let attachments = match attachments {
            Ok(value) => value,
            Err(blocked) if history_budget_exceeded(&blocked) => return Ok(None),
            Err(blocked) => return Err(anyhow!(blocked.blocker.message)),
        };
        let messages = build_native_messages(
            prompt.system,
            prompt.skill,
            selected,
            prompt.current,
            upstream_setting_bool(prompt.settings, "excludeReasoningFromContext"),
            &attachments.text_by_message_id,
            &attachments.current_text,
        );
        let prompt_tokens = handle
            .measure_prompt_tokens(
                GenerationInput::Chat {
                    messages: messages.clone(),
                    template: prompt.template.clone(),
                },
                attachments.media.clone(),
            )
            .map_err(|error| anyhow!(error))?;
        Ok(Some((
            prompt_tokens,
            VoiceProjection {
                attachments,
                messages,
                prompt_tokens,
                omitted_history_messages: 0,
            },
        )))
    })?;
    projection.omitted_history_messages = active[..start]
        .iter()
        .filter(|message| message.role != MessageRole::System)
        .count();
    conversation.inference_window = if start == 0 {
        None
    } else {
        Some(ConversationInferenceWindow {
            covered_through_message_id: active[start - 1].id.clone(),
            covered_source_sha256: source_digest(&active[..start])?,
            profile_sha256: profile,
        })
    };
    Ok(projection)
}

fn history_budget_exceeded(blocked: &AttachmentContextBlocker) -> bool {
    matches!(
        blocked.blocker.code.as_str(),
        "attachment_context_count_exceeded"
            | "attachment_context_text_limit_exceeded"
            | "attachment_context_media_count_exceeded"
            | "attachment_context_media_bytes_exceeded"
    )
}

fn digest(value: &(impl Serialize + ?Sized)) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

/// Hash only message authority that affects the inference source. Branch
/// indices and counts are presentation metadata and may be normalized when a
/// generated exchange is committed, so they must not invalidate a saved
/// inference boundary.
fn source_digest(messages: &[Message]) -> Result<String> {
    let source = messages
        .iter()
        .map(|message| {
            (
                &message.id,
                &message.conversation_id,
                message.role.clone(),
                &message.content,
                &message.parent_id,
                &message.attachment_ids,
                &message.reasoning_content,
                message.reasoning_incomplete,
                &message.attribution,
            )
        })
        .collect::<Vec<_>>();
    digest(&source)
}

fn valid_window_start(
    active: &[Message],
    window: Option<&ConversationInferenceWindow>,
    profile: &str,
) -> Result<usize> {
    let Some(window) = window.filter(|window| window.profile_sha256 == profile) else {
        return Ok(0);
    };
    let Some(index) = active
        .iter()
        .position(|message| message.id == window.covered_through_message_id)
    else {
        return Ok(0);
    };
    Ok(
        if source_digest(&active[..=index])? == window.covered_source_sha256 {
            index + 1
        } else {
            0
        },
    )
}

/// Keep a stable boundary until it no longer fits. Rollover leaves headroom so
/// every subsequent turn does not shift and re-evaluate the whole window.
fn fit_window<T>(
    active: &[Message],
    mut start: usize,
    limit: usize,
    mut prepare: impl FnMut(&[Message]) -> Result<Option<(usize, T)>>,
) -> Result<(usize, T)> {
    let mut rolling = false;
    loop {
        let selected = active
            .iter()
            .enumerate()
            .filter(|(index, message)| *index >= start || message.role == MessageRole::System)
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        if let Some((tokens, prepared)) = prepare(&selected)? {
            let target = if rolling { limit * 2 / 3 } else { limit };
            if tokens <= target || (start == active.len() && tokens <= limit) {
                return Ok((start, prepared));
            }
        }
        if start == active.len() {
            return Err(anyhow!(
                "The persona and current utterance exceed the available voice context"
            ));
        }
        rolling = true;
        start += (active.len() - start).div_ceil(4);
        // Keep complete user/assistant exchanges.
        while start < active.len() && active[start].role == MessageRole::Assistant {
            start += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, role: MessageRole, content: &str) -> Message {
        Message {
            id: id.to_string(),
            conversation_id: "voice-context-test".to_string(),
            role,
            content: content.to_string(),
            created_at: "1".to_string(),
            parent_id: None,
            model: None,
            receipt_id: None,
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_content: None,
            reasoning_incomplete: false,
            branch_index: None,
            branch_count: None,
            attribution: None,
            attachment_ids: Vec::new(),
        }
    }

    #[test]
    fn exact_budget_rollover_keeps_whole_exchanges_and_stabilizes_with_headroom() -> Result<()> {
        let active = vec![
            message("system", MessageRole::System, "ss"),
            message("user-1", MessageRole::User, "u"),
            message("assistant-1", MessageRole::Assistant, "a"),
            message("user-2", MessageRole::User, "u"),
            message("assistant-2", MessageRole::Assistant, "a"),
            message("user-3", MessageRole::User, "u"),
            message("assistant-3", MessageRole::Assistant, "a"),
            message("current", MessageRole::User, "u"),
        ];
        let original = active.clone();
        let weight = |messages: &[Message]| {
            messages
                .iter()
                .map(|message| message.content.len())
                .sum::<usize>()
        };

        let (exact_start, exact_selection) = fit_window(&active, 0, weight(&active), |messages| {
            Ok(Some((weight(messages), messages.to_vec())))
        })?;
        assert_eq!(exact_start, 0, "history exactly at budget remains intact");
        assert_eq!(exact_selection, active);

        let (start, selected) = fit_window(&active, 0, 5, |messages| {
            Ok(Some((weight(messages), messages.to_vec())))
        })?;

        assert_eq!(start, 7);
        assert_eq!(
            weight(&selected),
            3,
            "the rollover lands exactly at its low-water target"
        );
        assert_eq!(
            selected
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec!["system", "current"],
            "system content survives and every omitted user turn loses its assistant reply"
        );
        assert_eq!(
            active, original,
            "projection must not edit the source history"
        );

        let mut next_turn = active.clone();
        next_turn.push(message("next-current", MessageRole::User, "u"));
        let (next_start, next_selected) = fit_window(&next_turn, start, 5, |messages| {
            Ok(Some((weight(messages), messages.to_vec())))
        })?;
        assert_eq!(
            next_start, start,
            "the small next turn retains the saved boundary"
        );
        assert_eq!(
            weight(&next_selected),
            4,
            "the saved boundary leaves room below budget"
        );
        assert_eq!(
            active, original,
            "a later projection still leaves source history intact"
        );
        Ok(())
    }

    #[test]
    fn attachment_budget_miss_rolls_history_but_fatal_errors_propagate() -> Result<()> {
        let active = vec![
            message("system", MessageRole::System, "system"),
            message("old", MessageRole::User, "large attachment"),
            message("reply", MessageRole::Assistant, "old reply"),
            message("current", MessageRole::User, "new turn"),
        ];
        let (start, selected) = fit_window(&active, 0, 10, |messages| {
            if messages.iter().any(|message| message.id == "old") {
                return Ok(None);
            }
            Ok(Some((messages.len(), messages.to_vec())))
        })?;
        assert!(
            start > 0,
            "a budget-only attachment miss should trigger rollover"
        );
        assert!(selected.iter().any(|message| message.id == "system"));
        assert!(selected.iter().any(|message| message.id == "current"));
        assert!(!selected.iter().any(|message| message.id == "old"));

        let fatal = fit_window::<Vec<Message>>(&active, 0, 10, |_| {
            Err(anyhow!("attachment inspection failed"))
        })
        .expect_err("fatal attachment inspection failures must stop planning");
        assert!(fatal.to_string().contains("attachment inspection failed"));
        Ok(())
    }

    #[test]
    fn persisted_boundary_is_rejected_after_source_profile_or_branch_changes() -> Result<()> {
        let active = vec![
            message("system", MessageRole::System, "stable system"),
            message("user-1", MessageRole::User, "first source turn"),
            message("assistant-1", MessageRole::Assistant, "first reply"),
            message("current", MessageRole::User, "current turn"),
        ];
        let profile = "profile-a";
        let window = ConversationInferenceWindow {
            covered_through_message_id: "assistant-1".to_string(),
            covered_source_sha256: source_digest(&active[..=2])?,
            profile_sha256: profile.to_string(),
        };
        assert_eq!(valid_window_start(&active, Some(&window), profile)?, 3);

        // Generated exchange commits can normalize branch presentation while
        // leaving the inference source unchanged. That must retain the stable
        // window; editing source content must still reject it below.
        let mut normalized_branch = active.clone();
        normalized_branch[1].branch_index = Some(0);
        normalized_branch[1].branch_count = Some(2);
        normalized_branch[2].branch_index = Some(0);
        normalized_branch[2].branch_count = Some(2);
        assert_eq!(
            valid_window_start(&normalized_branch, Some(&window), profile)?,
            3
        );

        let mut changed_source = active.clone();
        changed_source[1].content.push_str(" edited");
        assert_eq!(
            valid_window_start(&changed_source, Some(&window), profile)?,
            0
        );
        assert_eq!(valid_window_start(&active, Some(&window), "profile-b")?, 0);

        let changed_branch = vec![
            active[0].clone(),
            message("branch-user", MessageRole::User, "different branch"),
            message("branch-answer", MessageRole::Assistant, "different reply"),
            active[3].clone(),
        ];
        assert_eq!(
            valid_window_start(&changed_branch, Some(&window), profile)?,
            0
        );
        Ok(())
    }
}
