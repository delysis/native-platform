//! A bounded inference projection; saved conversation history is never trimmed.
use crate::attachments::{
    AttachmentContextBlocker, ChatAttachmentContext, prepare_call_greeting_history,
    prepare_live_audio_history, prepare_voice_reply_attachments,
};
use crate::chat::{build_native_messages, upstream_setting_bool};
use crate::config::Settings;
use crate::conversation_store::{
    CONVERSATIONS_NAMESPACE, Conversation, ConversationDb, ConversationInferenceWindow, Message,
    MessageAttribution, MessageRole,
};
use crate::store::RuntimeStore;
use anyhow::{Result, anyhow};
use llama_native_engine::NativeModelHandle;
use llama_native_types::{ChatMessage, ChatTemplateChoice, GenerationInput};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(crate) enum VoiceHistoryMode {
    Live,
    Recorded,
    Greeting,
}

impl VoiceHistoryMode {
    fn prompt_limit(self, context: usize, completion_tokens: usize) -> usize {
        // All voice phases share one history projection. Filling the microphone
        // reserve during pickup makes the next capture roll away that freshly
        // evaluated history, even when no new caller content has arrived.
        let microphone_reserve = (context / 4).min(4096);
        context.saturating_sub(microphone_reserve + completion_tokens + 32)
    }
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
    let limit = prompt.mode.prompt_limit(context, prompt.completion_tokens);
    let profile = digest(&(
        // Older windows reserved headroom from the entire prompt, which could
        // put the target below the mandatory persona and discard all history.
        "voice-window-mandatory-budget-v2",
        &conversation.execution_profile,
        prompt.system,
        prompt.skill,
        &prompt.template,
        &fingerprint,
        upstream_setting_bool(prompt.settings, "excludeReasoningFromContext"),
    ))?;
    let store = RuntimeStore::open(&prompt.settings.data_dir)?;
    let generated_voice_reply_ids = store.read_documents(|snapshot| {
        crate::attachments::generated_voice_reply_ids_from_snapshot(
            snapshot,
            &conversation.id,
            active,
        )
    })?;
    let start = valid_window_start(
        active,
        conversation.inference_window.as_ref(),
        &profile,
        &generated_voice_reply_ids,
    )?;
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
    let window = if start == 0 {
        None
    } else {
        Some(ConversationInferenceWindow {
            covered_through_message_id: active[start - 1].id.clone(),
            covered_source_sha256: source_digest(&active[..start], &generated_voice_reply_ids)?,
            profile_sha256: profile,
        })
    };
    if window != conversation.inference_window {
        // Save only a changed projection boundary, before it can replace the
        // worker's resident prefix. A prearmed capture may be cancelled without
        // committing a message; redial must still use this same history. This
        // small metadata transaction neither exports nor persists native KV.
        if !persist_window_if_unchanged(
            &store,
            conversation,
            active,
            &generated_voice_reply_ids,
            window.as_ref(),
        )? {
            return Err(anyhow!(
                "voice_context_changed: Conversation history changed during voice preparation"
            ));
        }
        conversation.inference_window = window;
    }
    Ok(projection)
}

fn persist_window_if_unchanged(
    store: &RuntimeStore,
    expected: &Conversation,
    expected_active: &[Message],
    expected_generated_voice_reply_ids: &BTreeSet<(String, String)>,
    window: Option<&ConversationInferenceWindow>,
) -> Result<bool> {
    let expected_source = source_projection(expected_active, expected_generated_voice_reply_ids);
    store.mutate_documents(
        CONVERSATIONS_NAMESPACE,
        ConversationDb::default,
        |db, documents| {
            crate::personas::reject_removed_conversation_id_from_documents(
                &expected.id,
                documents,
            )?;
            crate::personas::reject_removed_conversation_writes_from_documents(db, documents)?;
            let Some(current) = db
                .conversations
                .iter_mut()
                .find(|item| item.id == expected.id)
            else {
                return Ok(false);
            };
            let current_active = crate::document::checked_active_messages(current)?;
            let current_generated_voice_reply_ids =
                crate::attachments::generated_voice_reply_ids_from_mutations(
                    documents,
                    &current.id,
                    &current_active,
                )?;
            if current.active_leaf_message_id != expected.active_leaf_message_id
                || current.branch_root_message_id != expected.branch_root_message_id
                || current.execution_profile != expected.execution_profile
                || current.selected_model_path != expected.selected_model_path
                || current.current_skill_ids != expected.current_skill_ids
                || current.source_conversation_id != expected.source_conversation_id
                || (current.inference_window != expected.inference_window
                    && current.inference_window.as_ref() != window)
                || source_projection(&current_active, &current_generated_voice_reply_ids)
                    != expected_source
            {
                return Ok(false);
            }
            current.inference_window = window.cloned();
            Ok(true)
        },
    )
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
fn source_digest(
    messages: &[Message],
    generated_voice_reply_ids: &BTreeSet<(String, String)>,
) -> Result<String> {
    digest(&source_projection(messages, generated_voice_reply_ids))
}

type InferenceSourceProjection<'a> = (
    &'a String,
    &'a String,
    MessageRole,
    &'a String,
    &'a Option<String>,
    Vec<&'a String>,
    &'a Option<String>,
    bool,
    &'a Option<MessageAttribution>,
);

fn source_projection<'a>(
    messages: &'a [Message],
    generated_voice_reply_ids: &BTreeSet<(String, String)>,
) -> Vec<InferenceSourceProjection<'a>> {
    messages
        .iter()
        .map(|message| {
            (
                &message.id,
                &message.conversation_id,
                message.role.clone(),
                &message.content,
                &message.parent_id,
                message
                    .attachment_ids
                    .iter()
                    .filter(|id| {
                        message.role != MessageRole::Assistant
                            || !generated_voice_reply_ids.iter().any(
                                |(message_id, attachment_id)| {
                                    message_id == &message.id && attachment_id == *id
                                },
                            )
                    })
                    .collect(),
                &message.reasoning_content,
                message.reasoning_incomplete,
                &message.attribution,
            )
        })
        .collect()
}

fn valid_window_start(
    active: &[Message],
    window: Option<&ConversationInferenceWindow>,
    profile: &str,
    generated_voice_reply_ids: &BTreeSet<(String, String)>,
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
        if source_digest(&active[..=index], generated_voice_reply_ids)?
            == window.covered_source_sha256
        {
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
    let mut rollover_target = None;
    let mut minimal_projection = None;
    loop {
        if start == active.len()
            && let Some(prepared) = minimal_projection.take()
        {
            return Ok((start, prepared));
        }
        let selected = active
            .iter()
            .enumerate()
            .filter(|(index, message)| *index >= start || message.role == MessageRole::System)
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        if let Some((tokens, prepared)) = prepare(&selected)? {
            let target = rollover_target.unwrap_or(limit);
            if tokens <= target {
                return Ok((start, prepared));
            }
        }
        if start == active.len() {
            return Err(anyhow!(
                "The persona and current utterance exceed the available voice context"
            ));
        }
        if rollover_target.is_none() {
            // The persona, system messages and current input cannot be rolled
            // away. Reserve headroom only from the discretionary history
            // budget, otherwise a long persona can make the low-water target
            // impossible even while several recent exchanges still fit.
            let mandatory = active
                .iter()
                .filter(|message| message.role == MessageRole::System)
                .cloned()
                .collect::<Vec<_>>();
            let Some((mandatory_tokens, prepared)) = prepare(&mandatory)? else {
                return Err(anyhow!(
                    "The persona and current utterance exceed the available voice context"
                ));
            };
            if mandatory_tokens > limit {
                return Err(anyhow!(
                    "The persona and current utterance exceed the available voice context"
                ));
            }
            let available_history = limit - mandatory_tokens;
            rollover_target = Some(limit - available_history.div_ceil(3));
            minimal_projection = Some(prepared);
        }
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
    fn greeting_to_microphone_keeps_the_same_history_prefix() -> Result<()> {
        let mut history = Vec::new();
        for index in 0..5 {
            history.push(message(
                &format!("u{index}"),
                MessageRole::User,
                "old speech",
            ));
            history.push(message(
                &format!("a{index}"),
                MessageRole::Assistant,
                "old reply",
            ));
        }
        let measure =
            |messages: &[Message]| Ok(Some((11_200 + messages.len() * 400, messages.to_vec())));
        let (greeting_start, greeting) = fit_window(
            &history,
            0,
            VoiceHistoryMode::Greeting.prompt_limit(18_944, 96),
            measure,
        )?;
        let (live_start, live) = fit_window(
            &history,
            greeting_start,
            VoiceHistoryMode::Live.prompt_limit(18_944, 96),
            measure,
        )?;
        assert_eq!(
            greeting_start, live_start,
            "microphone preparation must not discard history just used for pickup"
        );
        assert_eq!(
            greeting, live,
            "unchanged old content must remain the same cached prefix"
        );
        let (recorded_start, recorded) = fit_window(
            &history,
            0,
            VoiceHistoryMode::Recorded.prompt_limit(18_944, 96),
            measure,
        )?;
        assert_eq!((recorded_start, recorded), (greeting_start, greeting));
        Ok(())
    }

    fn saved_history() -> Conversation {
        let mut messages = Vec::new();
        let mut parent = None;
        for index in 0..6 {
            for (prefix, role) in [("u", MessageRole::User), ("a", MessageRole::Assistant)] {
                let mut item = message(&format!("{prefix}{index}"), role, "saved content");
                item.parent_id = parent;
                parent = Some(item.id.clone());
                messages.push(item);
            }
        }
        Conversation {
            id: "voice-context-test".into(),
            title: "Conversation".into(),
            created_at: "1".into(),
            updated_at: "1".into(),
            kind: crate::conversation_store::ConversationKind::Chat,
            execution_profile: Default::default(),
            selected_model_path: None,
            source_conversation_id: None,
            source_message_id: None,
            branch_root_message_id: None,
            active_leaf_message_id: parent,
            recipient_ids: Vec::new(),
            current_skill_ids: Vec::new(),
            messages,
            inference_window: None,
        }
    }

    fn window_at(active: &[Message], start: usize) -> Result<ConversationInferenceWindow> {
        Ok(ConversationInferenceWindow {
            covered_through_message_id: active[start - 1].id.clone(),
            covered_source_sha256: source_digest(&active[..start], &BTreeSet::new())?,
            profile_sha256: "profile".into(),
        })
    }

    fn generated_voice_reply_record(
        conversation_id: &str,
        message_id: &str,
        id: &str,
    ) -> crate::attachments::AttachmentRecord {
        crate::attachments::AttachmentRecord {
            id: id.to_string(),
            conversation_id: conversation_id.to_string(),
            message_id: message_id.to_string(),
            kind: crate::attachments::AttachmentKind::Audio,
            file_name: "voice-message-reply-fixture.wav".to_string(),
            source_path: "generated://assistant-voice".to_string(),
            stored_path: "private://fixture".to_string(),
            mime: "audio/wav".to_string(),
            bytes: 48,
            sha256: "0".repeat(64),
            created_at: "2".to_string(),
            state: crate::attachments::AttachmentState::Committed,
            root_object_id: Some("object-fixture".to_string()),
            detected_format: Some(attachment_native_types::DetectedFormat::Wav),
            coverage: Some(attachment_native_types::Coverage::Complete),
            manifest_namespace: Some("attachment-manifest-fixture".to_string()),
            policy_fingerprint: Some("policy-fixture".to_string()),
            artifact_count: 1,
            canonical_text_bytes: 0,
            media_objects: 1,
        }
    }

    #[test]
    fn prepared_roll_survives_hangup_and_reopen_without_committing_a_turn() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = RuntimeStore::open_with_key(root.path(), [71; 32])?;
        let original = saved_history();
        let active = crate::document::checked_active_messages(&original)?;
        let mut db = ConversationDb {
            conversations: vec![original.clone()],
            selected_conversation_id: Some(original.id.clone()),
        };
        store.put(CONVERSATIONS_NAMESPACE, &db)?;
        // The saved greeting still fits. A slightly larger microphone framing
        // crosses the same limit, so equal budgets alone cannot cover hangup.
        let limit = VoiceHistoryMode::Greeting.prompt_limit(18_944, 96);
        let (greeting_start, _) = fit_window(&active, 0, limit, |messages| {
            Ok(Some((11_000 + messages.len() * 300, ())))
        })?;
        assert_eq!(greeting_start, 0);
        let (live_start, selected) = fit_window(&active, greeting_start, limit, |messages| {
            Ok(Some((11_300 + messages.len() * 300, messages.to_vec())))
        })?;
        assert!(live_start > 0);
        let window = window_at(&active, live_start)?;
        // Concurrent presentation changes survive the metadata-only update.
        db.conversations[0].title = "Renamed during preparation".into();
        store.put(CONVERSATIONS_NAMESPACE, &db)?;
        assert!(persist_window_if_unchanged(
            &store,
            &original,
            &active,
            &BTreeSet::new(),
            Some(&window)
        )?);
        // A duplicate preparation of the same boundary is harmless.
        assert!(persist_window_if_unchanged(
            &store,
            &original,
            &active,
            &BTreeSet::new(),
            Some(&window)
        )?);
        drop(store); // Hangup/cancellation: no generated exchange is committed.
        let reopened = RuntimeStore::open_with_key(root.path(), [71; 32])?;
        let saved = reopened
            .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?
            .expect("saved history");
        assert_eq!(saved.conversations[0].messages, original.messages);
        assert_eq!(saved.conversations[0].updated_at, original.updated_at);
        assert_eq!(saved.conversations[0].title, "Renamed during preparation");
        assert_eq!(saved.selected_conversation_id, db.selected_conversation_id);
        let resumed_start = valid_window_start(
            &active,
            saved.conversations[0].inference_window.as_ref(),
            "profile",
            &BTreeSet::new(),
        )?;
        let (redial_start, redial) = fit_window(&active, resumed_start, limit, |messages| {
            Ok(Some((11_000 + messages.len() * 300, messages.to_vec())))
        })?;
        assert_eq!(redial_start, live_start);
        assert_eq!(
            redial, selected,
            "redial must reuse the prepared historical prefix"
        );
        Ok(())
    }

    #[test]
    fn generated_voice_publication_keeps_rolled_boundary_but_ordinary_changes_reject() -> Result<()>
    {
        let root = tempfile::tempdir()?;
        let store = RuntimeStore::open_with_key(root.path(), [81; 32])?;
        let original = saved_history();
        let active = crate::document::checked_active_messages(&original)?;
        let start = 8;
        let window = window_at(&active, start)?;
        store.put(
            CONVERSATIONS_NAMESPACE,
            &ConversationDb {
                conversations: vec![original.clone()],
                selected_conversation_id: Some(original.id.clone()),
            },
        )?;

        let mut published = original.clone();
        let assistant = published
            .messages
            .iter_mut()
            .find(|message| message.id == "a1")
            .expect("covered assistant message");
        let generated_id = "generated-voice-a1";
        assistant.attachment_ids.push(generated_id.to_string());
        published.updated_at = "2".to_string();
        store.put(
            CONVERSATIONS_NAMESPACE,
            &ConversationDb {
                conversations: vec![published.clone()],
                selected_conversation_id: Some(original.id.clone()),
            },
        )?;
        let mut attachment_db = crate::attachments::AttachmentDb::default();
        attachment_db.attachments.push(generated_voice_reply_record(
            &original.id,
            "a1",
            generated_id,
        ));
        store.put(crate::attachments::ATTACHMENTS_NAMESPACE, &attachment_db)?;

        let no_generated_ids = BTreeSet::new();
        assert!(persist_window_if_unchanged(
            &store,
            &original,
            &active,
            &no_generated_ids,
            Some(&window),
        )?);
        let published_active = crate::document::checked_active_messages(&published)?;
        let published_generated_ids = store.read_documents(|snapshot| {
            crate::attachments::generated_voice_reply_ids_from_snapshot(
                snapshot,
                &original.id,
                &published_active,
            )
        })?;
        assert_eq!(
            published_generated_ids,
            BTreeSet::from([("a1".to_string(), generated_id.to_string())])
        );
        assert_eq!(
            valid_window_start(
                &published_active,
                Some(&window),
                "profile",
                &published_generated_ids,
            )?,
            start,
            "generated audio does not change the saved inference source"
        );

        // A registry row owned by a1 must not erase the same ID when a
        // malformed/moved reference appears on another assistant message.
        let mut moved_reference = published.clone();
        moved_reference
            .messages
            .iter_mut()
            .find(|message| message.id == "a2")
            .expect("second assistant message")
            .attachment_ids
            .push(generated_id.to_string());
        let moved_active = crate::document::checked_active_messages(&moved_reference)?;
        let moved_projection = source_projection(&moved_active, &published_generated_ids);
        let moved_index = moved_active
            .iter()
            .position(|message| message.id == "a2")
            .expect("second assistant index");
        assert!(
            moved_projection[moved_index]
                .5
                .iter()
                .any(|id| id.as_str() == generated_id),
            "an ID owned by a1 remains authoritative when referenced by a2"
        );

        let mut ordinary_change = published.clone();
        ordinary_change
            .messages
            .iter_mut()
            .find(|message| message.id == "a1")
            .expect("covered assistant message")
            .attachment_ids
            .push("ordinary-attachment".to_string());
        let ordinary_active = crate::document::checked_active_messages(&ordinary_change)?;
        store.put(
            CONVERSATIONS_NAMESPACE,
            &ConversationDb {
                conversations: vec![ordinary_change],
                selected_conversation_id: Some(original.id.clone()),
            },
        )?;
        assert!(
            !persist_window_if_unchanged(
                &store,
                &original,
                &active,
                &no_generated_ids,
                Some(&window),
            )?,
            "ordinary attachment IDs remain inference-authoritative"
        );
        assert_eq!(
            valid_window_start(
                &ordinary_active,
                Some(&window),
                "profile",
                &published_generated_ids,
            )?,
            0,
            "ordinary attachment changes invalidate a prior inference window"
        );

        let mut edited = published;
        edited
            .messages
            .iter_mut()
            .find(|message| message.id == "a1")
            .expect("covered assistant message")
            .content
            .push_str(" changed");
        let edited_active = crate::document::checked_active_messages(&edited)?;
        assert_eq!(
            valid_window_start(
                &edited_active,
                Some(&window),
                "profile",
                &published_generated_ids,
            )?,
            0,
            "generated audio does not mask a source text edit"
        );
        Ok(())
    }

    #[test]
    fn stale_preparation_cannot_replace_source_or_a_newer_window() -> Result<()> {
        let root = tempfile::tempdir()?;
        let store = RuntimeStore::open_with_key(root.path(), [72; 32])?;
        let original = saved_history();
        let active = crate::document::checked_active_messages(&original)?;
        let window = window_at(&active, 4)?;
        for change in 0..8 {
            let mut concurrent = original.clone();
            match change {
                0 => concurrent.messages[0].content = "edited".into(),
                1 => concurrent.active_leaf_message_id = Some("a4".into()),
                2 => concurrent.execution_profile.version += 1,
                3 => concurrent.current_skill_ids.push("different-skill".into()),
                4 => concurrent.selected_model_path = Some("different-model".into()),
                5 => concurrent.source_conversation_id = Some("different-persona".into()),
                6 => concurrent.inference_window = Some(window_at(&active, 8)?),
                _ => concurrent.branch_root_message_id = Some("u1".into()),
            }
            let db = ConversationDb {
                conversations: vec![concurrent],
                selected_conversation_id: None,
            };
            store.put(CONVERSATIONS_NAMESPACE, &db)?;
            assert!(
                !persist_window_if_unchanged(
                    &store,
                    &original,
                    &active,
                    &BTreeSet::new(),
                    Some(&window),
                )?,
                "stale authority case {change}"
            );
            assert_eq!(
                store.get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?,
                Some(db)
            );
        }
        Ok(())
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
    fn long_persona_rollover_retains_recent_exchanges_and_next_turn_reuses_window() -> Result<()> {
        let mut active = vec![message("persona", MessageRole::System, "persona")];
        for index in 0..5 {
            active.push(message(&format!("user-{index}"), MessageRole::User, "turn"));
            active.push(message(
                &format!("reply-{index}"),
                MessageRole::Assistant,
                "reply",
            ));
        }
        let measure = |messages: &[Message]| {
            11_000
                + messages
                    .iter()
                    .filter(|m| m.role != MessageRole::System)
                    .count()
                    * 500
        };
        let (start, selected) = fit_window(&active, 0, 14_000, |messages| {
            Ok(Some((measure(messages), messages.to_vec())))
        })?;
        assert_eq!(measure(&selected), 13_000);
        assert_eq!(selected.len(), 5, "keep persona and two recent exchanges");
        assert_eq!(selected[1].id, "user-3");
        assert_eq!(selected[4].id, "reply-4");

        active.push(message("user-next", MessageRole::User, "next"));
        let mut preparations = 0;
        let (next_start, next) = fit_window(&active, start, 14_000, |messages| {
            preparations += 1;
            Ok(Some((measure(messages), messages.to_vec())))
        })?;
        assert_eq!(
            next_start, start,
            "do not shift the cached prefix on each turn"
        );
        assert_eq!(
            preparations, 1,
            "fitting warm history needs no baseline remeasurement"
        );
        assert_eq!(measure(&next), 13_500);
        Ok(())
    }

    #[test]
    fn impossible_mandatory_prompt_fails_without_repeated_history_preparation() {
        let active = vec![
            message("persona", MessageRole::System, "persona"),
            message("user", MessageRole::User, "turn"),
            message("reply", MessageRole::Assistant, "reply"),
        ];
        let mut preparations = 0;
        let error = fit_window(&active, 0, 10, |messages| {
            preparations += 1;
            Ok(Some((11 + messages.len(), ())))
        })
        .expect_err("mandatory input exceeding the hard limit must fail");
        assert!(error.to_string().contains("available voice context"));
        assert_eq!(preparations, 2);
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
            covered_source_sha256: source_digest(&active[..=2], &BTreeSet::new())?,
            profile_sha256: profile.to_string(),
        };
        assert_eq!(
            valid_window_start(&active, Some(&window), profile, &BTreeSet::new())?,
            3
        );

        // Generated exchange commits can normalize branch presentation while
        // leaving the inference source unchanged. That must retain the stable
        // window; editing source content must still reject it below.
        let mut normalized_branch = active.clone();
        normalized_branch[1].branch_index = Some(0);
        normalized_branch[1].branch_count = Some(2);
        normalized_branch[2].branch_index = Some(0);
        normalized_branch[2].branch_count = Some(2);
        assert_eq!(
            valid_window_start(&normalized_branch, Some(&window), profile, &BTreeSet::new())?,
            3
        );

        let mut changed_source = active.clone();
        changed_source[1].content.push_str(" edited");
        assert_eq!(
            valid_window_start(&changed_source, Some(&window), profile, &BTreeSet::new())?,
            0
        );
        assert_eq!(
            valid_window_start(&active, Some(&window), "profile-b", &BTreeSet::new())?,
            0
        );

        let changed_branch = vec![
            active[0].clone(),
            message("branch-user", MessageRole::User, "different branch"),
            message("branch-answer", MessageRole::Assistant, "different reply"),
            active[3].clone(),
        ];
        assert_eq!(
            valid_window_start(&changed_branch, Some(&window), profile, &BTreeSet::new())?,
            0
        );
        Ok(())
    }
}
