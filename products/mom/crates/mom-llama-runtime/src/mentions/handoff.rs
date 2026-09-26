//! Compose one target's immutable context before native admission.
//!
//! The lexical @ parser has already selected explicit participants. Referenced
//! text, attachment labels and model output never re-enter that parser here.
//! Whole source messages include their attribution and prepared attachment text;
//! omitting a message also omits all of its media occurrences.

use super::*;
use crate::attachments::{
    AttachmentMediaReference, CurrentAttachmentSelection, ScopedChatAttachmentContext,
    SelectedAttachmentMedia, SelectedAttachmentSource, SelectedMediaBuilder, native_media_id,
    prepare_scoped_chat_attachments, validate_selected_attachment_budget,
};
use llama_native_types::MediaInput;
use workspace_document::context::{
    ContextBudget, ContextMeasure, ContextSelectionError, select_context,
};

const MAX_MEDIA_SOURCE_NOTE_BYTES: usize = 256 * 1024;

pub(super) struct PreparedHandoff {
    pub(super) stable_prefix: Vec<ChatMessage>,
    pub(super) messages: Vec<ChatMessage>,
    pub(super) media: Vec<MediaInput>,
    pub(super) receipt: ContextInputReceipt,
    pub(super) text_prompt_tokens: usize,
}

/// Descriptive retained metadata, never a deserializable native read capability.
/// `native_media_tokens` stays unknown until the actual multimodal worker runs.
#[derive(Serialize)]
pub(super) struct ContextInputReceipt {
    schema: &'static str,
    target_id: String,
    snapshot_sha256: String,
    source_message_ids: Vec<String>,
    host_message_ids: Vec<String>,
    omitted_source_messages: usize,
    omitted_host_messages: usize,
    selected_messages_sha256: String,
    chat_input_sha256: String,
    pub(super) model_fingerprint: ModelFingerprint,
    text_prompt_tokens: usize,
    native_media_tokens: Option<usize>,
    media_sources: Vec<SelectedAttachmentMedia>,
    pub(super) attachment_sources: Vec<SelectedAttachmentSource>,
}

struct Unit<'a> {
    source: &'a Message,
    messages: Vec<ChatMessage>,
    media: &'a [AttachmentMediaReference],
    canonical_text_bytes: usize,
}

fn blocker(code: &str, message: impl Into<String>) -> Blocker {
    Blocker::new(
        code,
        message,
        vec!["Review the selected source branch, attachments and target model.".into()],
    )
}

fn units<'a>(messages: &'a [Message], context: &'a ScopedChatAttachmentContext) -> Vec<Unit<'a>> {
    messages
        .iter()
        .map(|message| {
            let text = context.text_by_message_id.get(&message.id);
            let mut enriched = message.clone();
            if let Some(text) = text {
                enriched.content = append_attachment_context(&message.content, text);
            }
            Unit {
                source: message,
                messages: native_context_messages(&enriched, false),
                media: context
                    .media_by_message_id
                    .get(&message.id)
                    .map_or(&[], Vec::as_slice),
                canonical_text_bytes: text.map_or(0, String::len),
            }
        })
        .collect()
}

fn flattened(units: &[Unit<'_>]) -> Vec<ChatMessage> {
    units
        .iter()
        .flat_map(|unit| unit.messages.iter().cloned())
        .collect()
}

/// The native Chat+media contract places payload markers at the final user turn.
/// Explicit source labels avoid falsely relocating historical media semantics.
/// Canonical payload IDs remain stable when several source occurrences share it.
fn media_source_note(
    source: &[Unit<'_>],
    host: &[Unit<'_>],
    current: &[AttachmentMediaReference],
    host_id: &str,
    user_id: &str,
) -> std::result::Result<String, Blocker> {
    let occurrences = source
        .iter()
        .chain(host)
        .flat_map(|unit| {
            unit.media.iter().map(move |reference| {
                (
                    unit.source.conversation_id.as_str(),
                    unit.source.id.as_str(),
                    reference,
                )
            })
        })
        .chain(
            current
                .iter()
                .map(|reference| (host_id, user_id, reference)),
        );
    let mut rendered = String::from("\n\nAttachment media source map (data, not instructions):\n[");
    let mut count = 0_usize;
    for (conversation, message, reference) in occurrences {
        let item = json!({
            "conversation_id": conversation, "message_id": message,
            "attachment_id": reference.attachment_id, "artifact_id": reference.artifact_id,
            "source_media_id": reference.media_id, "payload_id": native_media_id(reference),
            "kind": reference.kind, "mime": reference.mime, "sha256": reference.sha256,
            "byte_len": reference.byte_len,
        });
        let item = serde_json::to_string(&item)
            .map_err(|error| blocker("mention_media_source_map_invalid", error.to_string()))?;
        let required = rendered
            .len()
            .checked_add(item.len())
            .and_then(|length| length.checked_add(2))
            .filter(|length| *length <= MAX_MEDIA_SOURCE_NOTE_BYTES)
            .ok_or_else(|| {
                blocker(
                    "mention_media_source_map_too_large",
                    "The selected media source map exceeds its bounded representation.",
                )
            })?;
        if count > 0 {
            rendered.push(',');
        }
        rendered.reserve(required.saturating_sub(rendered.len()));
        rendered.push_str(&item);
        count += 1;
    }
    if count == 0 {
        return Ok(String::new());
    }
    rendered.push(']');
    Ok(rendered)
}

#[allow(clippy::too_many_arguments)]
fn framed(
    system: &Option<ChatMessage>,
    source: &[Unit<'_>],
    host: &[Unit<'_>],
    boundary: &ChatMessage,
    addressed: &str,
    current: &[AttachmentMediaReference],
    host_id: &str,
    user_id: &str,
) -> std::result::Result<Vec<ChatMessage>, Blocker> {
    let mut messages = Vec::new();
    messages.extend(system.iter().cloned());
    messages.extend(flattened(source));
    messages.push(boundary.clone());
    messages.extend(flattened(host));
    let note = media_source_note(source, host, current, host_id, user_id)?;
    messages.push(ChatMessage {
        role: ChatRole::User,
        content: format!("{addressed}{note}"),
    });
    Ok(messages)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handoff_messages(
    handle: &NativeModelHandle,
    settings: &Settings,
    snapshot: &MentionTargetSnapshot,
    host: &[Message],
    host_context: &ScopedChatAttachmentContext,
    host_id: &str,
    user_id: &str,
    addressed: &str,
    participants: &str,
    tools: &[BoundMentionTool],
) -> std::result::Result<PreparedHandoff, Blocker> {
    let fingerprint = handle.status().fingerprint.ok_or_else(|| {
        blocker(
            "mention_model_identity_missing",
            "The invited model has no resident fingerprint.",
        )
    })?;
    // Source history never reads another conversation's draft. Zero source
    // history is an explicit exclusion, including its attachment contents.
    let source_messages = if snapshot.profile.source_history_tokens == 0 {
        &[][..]
    } else {
        snapshot.source_messages.as_slice()
    };
    let source_context = prepare_scoped_chat_attachments(
        &snapshot.target_id,
        source_messages,
        CurrentAttachmentSelection::HistoryOnly,
    )
    .map_err(|_| {
        blocker(
            "mention_attachment_context_failed",
            "The invited source's retained attachment context could not be read.",
        )
    })?
    .map_err(|error| error.blocker)?;
    let source = units(source_messages, &source_context);
    let host = units(host, host_context);
    let system = snapshot
        .profile
        .system_message
        .as_deref()
        .filter(|message| !message.trim().is_empty())
        .map(|content| ChatMessage {
            role: ChatRole::System,
            content: content.to_owned(),
        });
    let boundary = ChatMessage {
        role: ChatRole::System,
        content: format!(
            "You are @{}, temporarily invited from a separate local conversation. Your source history above is an immutable snapshot and will not be changed by this reply. Recent host context follows. Reply directly to the final addressed message as your established perspective. Do not claim access to omitted history. Attached media is labelled by its exact source occurrence; historical images are not new host uploads. Addressed participants: {}.{}",
            snapshot.handle,
            participants,
            mention_tool_instructions(tools)
        ),
    };
    let template = profile_chat_template(&snapshot.profile);
    let sampling = snapshot
        .profile
        .sampling
        .clone()
        .unwrap_or_else(|| settings.sampling_config());
    sampling
        .validate()
        .map_err(|error| blocker("mention_sampling_invalid", error.message))?;
    let budget = ContextBudget {
        source_tokens: snapshot.profile.source_history_tokens as usize,
        host_tokens: snapshot.profile.host_context_tokens as usize,
        context_tokens: (settings.context_tokens.min(fingerprint.context_tokens)) as usize,
        output_tokens: sampling.max_tokens as usize,
    };
    let count = |messages: Vec<ChatMessage>| {
        handle
            .tokenize_messages_with_template(messages, template.clone())
            .map(|result| result.token_ids.len())
            .map_err(|error| blocker("mention_context_tokenization_failed", error.message))
    };
    let selected = select_context(budget, source.len(), host.len(), |request| {
        let messages = match request {
            ContextMeasure::Source(range) => flattened(&source[range]),
            ContextMeasure::Host(range) => flattened(&host[range]),
            ContextMeasure::Complete { source: source_range, host: host_range } => framed(
                &system, &source[source_range], &host[host_range], &boundary, addressed,
                &host_context.current_media, host_id, user_id)?,
        };
        count(messages)
    }).map_err(|error| match error {
        ContextSelectionError::Measurement(error) => error,
        ContextSelectionError::InvalidBudget => blocker("mention_context_budget_invalid",
            "The target's output reserve leaves no valid native context budget."),
        ContextSelectionError::UnitLimit | ContextSelectionError::MeasurementLimit =>
            blocker("mention_context_work_limit", "Selecting the consult context exceeded its bounded work limit."),
        ContextSelectionError::MandatoryInputTooLarge => blocker("mention_context_too_large",
            "Persona instructions, addressed input and media source labels do not fit the target context."),
    })?;
    let source_range = selected.source();
    let host_range = selected.host();
    let selected_source = &source[source_range];
    let selected_host = &host[host_range];
    let mut reference_count = host_context.staged_ids.len();
    let mut text_bytes = host_context.current_text.len();
    for unit in selected_source.iter().chain(selected_host) {
        reference_count = reference_count
            .checked_add(unit.source.attachment_ids.len())
            .ok_or_else(|| {
                blocker(
                    "attachment_context_count_exceeded",
                    "Attachment reference accounting overflowed.",
                )
            })?;
        text_bytes = text_bytes
            .checked_add(unit.canonical_text_bytes)
            .ok_or_else(|| {
                blocker(
                    "attachment_context_text_limit_exceeded",
                    "Attachment text accounting overflowed.",
                )
            })?;
    }
    validate_selected_attachment_budget(reference_count, text_bytes)
        .map_err(|error| error.blocker)?;
    let mut media_builder = SelectedMediaBuilder::new();
    for unit in selected_source {
        media_builder
            .add(
                &snapshot.target_id,
                &unit.source.id,
                unit.media,
                &source_context.media,
            )
            .map_err(|error| error.blocker)?;
    }
    for unit in selected_host {
        media_builder
            .add(host_id, &unit.source.id, unit.media, &host_context.media)
            .map_err(|error| error.blocker)?;
    }
    media_builder
        .add(
            host_id,
            user_id,
            &host_context.current_media,
            &host_context.media,
        )
        .map_err(|error| error.blocker)?;
    let (media, media_sources) = media_builder.finish();
    if !media.is_empty() && !tools.is_empty() {
        return Err(blocker(
            "mention_media_tool_continuation_unsupported",
            "This Persona's constrained tool-decision and approval continuation are text-only; media cannot be silently discarded. Use a media-capable Persona without attached tools or an explicitly transformed text attachment.",
        ));
    }
    let messages = framed(
        &system,
        selected_source,
        selected_host,
        &boundary,
        addressed,
        &host_context.current_media,
        host_id,
        user_id,
    )?;
    let final_tokens = count(messages.clone())?;
    if final_tokens != selected.measured_prompt_tokens() {
        return Err(blocker(
            "mention_context_tokenization_changed",
            "The target tokenizer changed while its exact input was being composed.",
        ));
    }
    let mut stable_prefix = Vec::new();
    stable_prefix.extend(system.iter().cloned());
    stable_prefix.extend(flattened(selected_source));
    let selected_records = selected_source
        .iter()
        .chain(selected_host)
        .map(|unit| unit.source)
        .collect::<Vec<_>>();
    let mut attachment_sources = Vec::new();
    for (selected_units, context, conversation) in [
        (
            selected_source,
            &source_context,
            snapshot.target_id.as_str(),
        ),
        (selected_host, host_context, host_id),
    ] {
        for unit in selected_units {
            for representation in context
                .sources_by_message_id
                .get(&unit.source.id)
                .into_iter()
                .flatten()
            {
                attachment_sources.push(SelectedAttachmentSource {
                    conversation_id: conversation.into(),
                    message_id: unit.source.id.clone(),
                    representation: representation.clone(),
                });
            }
        }
    }
    attachment_sources.extend(
        host_context
            .current_sources
            .iter()
            .cloned()
            .map(|representation| SelectedAttachmentSource {
                conversation_id: host_id.into(),
                message_id: user_id.into(),
                representation,
            }),
    );
    let receipt = ContextInputReceipt {
        schema: "mom_llama.consult_input.v1",
        target_id: snapshot.target_id.clone(),
        snapshot_sha256: snapshot.snapshot_sha256.clone(),
        source_message_ids: selected_source
            .iter()
            .map(|unit| unit.source.id.clone())
            .collect(),
        host_message_ids: selected_host
            .iter()
            .map(|unit| unit.source.id.clone())
            .collect(),
        omitted_source_messages: snapshot
            .source_messages
            .len()
            .saturating_sub(selected_source.len()),
        omitted_host_messages: selected.omitted_host_units(),
        selected_messages_sha256: sha256_json(&selected_records)
            .map_err(|error| blocker("mention_context_identity_failed", error.to_string()))?,
        chat_input_sha256: sha256_json(&GenerationInput::Chat {
            messages: messages.clone(),
            template,
        })
        .map_err(|error| blocker("mention_context_identity_failed", error.to_string()))?,
        model_fingerprint: fingerprint,
        text_prompt_tokens: final_tokens,
        native_media_tokens: if media.is_empty() { Some(0) } else { None },
        media_sources,
        attachment_sources,
    };
    Ok(PreparedHandoff {
        stable_prefix,
        messages,
        media,
        receipt,
        text_prompt_tokens: final_tokens,
    })
}

/// The namespace is content-free and bounded; the receipt remains in Mom's
/// encrypted store. A transaction refuses replacement of an existing record.
/// This records *planned input*, not successful execution or model acceptance.
pub(super) fn retain_input_receipt(
    data_dir: &Path,
    invocation_id: &str,
    host_id: &str,
    user_id: &str,
    receipt: &ContextInputReceipt,
) -> Result<()> {
    let key = sha256_json(&(invocation_id, &receipt.target_id))?;
    let namespace = format!("mention-input.v1.{key}");
    let value = json!({ "invocation_id": invocation_id, "host_conversation_id": host_id,
        "user_message_id": user_id, "planned_input": receipt });
    RuntimeStore::open(data_dir)?.mutate_documents(
        &namespace,
        || None::<Value>,
        |existing, _| {
            if existing.is_some() {
                return Err(anyhow!("an immutable consult input receipt already exists"));
            }
            *existing = Some(value.clone());
            Ok(())
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::ChatAttachmentContext;
    use std::collections::HashMap;

    fn message(id: &str, conversation: &str, content: &str) -> Message {
        Message {
            id: id.into(),
            conversation_id: conversation.into(),
            role: MessageRole::User,
            content: content.into(),
            created_at: "1".into(),
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
    fn context() -> ScopedChatAttachmentContext {
        ScopedChatAttachmentContext {
            context: ChatAttachmentContext {
                staged_ids: Vec::new(),
                draft_snapshot: None,
                text_by_message_id: HashMap::new(),
                current_text: String::new(),
                media: Vec::new(),
            },
            media_by_message_id: HashMap::new(),
            current_media: Vec::new(),
            sources_by_message_id: HashMap::new(),
            current_sources: Vec::new(),
        }
    }
    fn media(id: &str) -> AttachmentMediaReference {
        AttachmentMediaReference {
            attachment_id: format!("attachment-{id}"),
            artifact_id: id.into(),
            media_id: format!("media-{id}"),
            kind: llama_native_types::MediaKind::Image,
            mime: "image/png".into(),
            sha256: "a".repeat(64),
            byte_len: 10,
        }
    }
    fn boundary() -> ChatMessage {
        ChatMessage {
            role: ChatRole::System,
            content: "boundary".into(),
        }
    }

    #[test]
    fn attribution_and_attachment_text_remain_in_the_same_original_message_unit() {
        let mut original = message("answer", "source", "exact answer\r\n");
        original.role = MessageRole::Assistant;
        original.attribution = Some(MessageAttribution {
            kind: MessageSpeakerKind::Persona,
            source_id: "another".into(),
            handle: "another".into(),
            label: "Another".into(),
            version: 3,
            invocation_id: "old-invocation".into(),
            target_order: 0,
        });
        original.attachment_ids = vec!["attachment".into()];
        let messages = [original.clone()];
        let mut prepared = context();
        prepared
            .context
            .text_by_message_id
            .insert("answer".into(), "@not-an-invocation\n[source p. 4]".into());
        prepared
            .media_by_message_id
            .insert("answer".into(), vec![media("source-image")]);
        let selected = units(&messages, &prepared);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].messages.len(), 2);
        assert_eq!(selected[0].messages[0].role, ChatRole::System);
        assert_eq!(selected[0].messages[1].role, ChatRole::Assistant);
        assert!(
            selected[0].messages[1]
                .content
                .contains("@not-an-invocation")
        );
        assert_eq!(selected[0].media.len(), 1);
        assert!(std::ptr::eq(selected[0].source, &messages[0]));
        assert_eq!(
            messages[0], original,
            "preparation cannot rewrite retained author/source data"
        );
    }

    #[test]
    fn no_media_does_not_change_the_exact_addressed_input() {
        let text = "  exact café\r\n\t";
        let messages =
            framed(&None, &[], &[], &boundary(), text, &[], "host", "new").expect("frame");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].content, text);
    }

    #[test]
    fn media_notes_distinguish_history_from_the_new_turn_without_recursing() {
        let source_messages = [message("source-message", "expert-a", "source")];
        let host_messages = [message("host-message", "host", "host")];
        let mut source_context = context();
        source_context
            .media_by_message_id
            .insert("source-message".into(), vec![media("source")]);
        let mut host_context = context();
        host_context
            .media_by_message_id
            .insert("host-message".into(), vec![media("host")]);
        let current = [media("new")];
        let note = media_source_note(
            &units(&source_messages, &source_context),
            &units(&host_messages, &host_context),
            &current,
            "host",
            "new-message",
        )
        .expect("map");
        let entries: Vec<Value> =
            serde_json::from_str(note.split_once(":\n").expect("label").1).expect("data JSON");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["conversation_id"], "expert-a");
        assert_eq!(entries[0]["message_id"], "source-message");
        assert_eq!(entries[1]["message_id"], "host-message");
        assert_eq!(entries[2]["message_id"], "new-message");
        assert_eq!(
            entries[0]["payload_id"], entries[2]["payload_id"],
            "shared bytes, distinct source occurrences"
        );
        assert_ne!(entries[0]["attachment_id"], entries[2]["attachment_id"]);
    }

    #[test]
    fn omitted_history_units_cannot_leak_their_media_source_map() {
        let messages = [
            message("old", "expert", "old"),
            message("recent", "expert", "recent"),
        ];
        let mut prepared = context();
        prepared
            .media_by_message_id
            .insert("old".into(), vec![media("old-image")]);
        prepared
            .media_by_message_id
            .insert("recent".into(), vec![media("recent-image")]);
        let source = units(&messages, &prepared);
        let selected = select_context(
            ContextBudget {
                source_tokens: 1,
                host_tokens: 0,
                context_tokens: 10,
                output_tokens: 1,
            },
            source.len(),
            0,
            |request| {
                Ok::<_, &str>(match request {
                    ContextMeasure::Source(range) => range.len(),
                    ContextMeasure::Host(_) => panic!("no host history"),
                    ContextMeasure::Complete { source, .. } => source.len() + 2,
                })
            },
        )
        .expect("whole-unit selection");
        let note = media_source_note(&source[selected.source()], &[], &[], "host", "new")
            .expect("selected map");
        assert!(note.contains("recent-image"));
        assert!(!note.contains("old-image"));
        assert!(!note.contains("expert-b"));
    }

    #[test]
    fn source_labels_are_json_data_not_reparsed_addresses() {
        let mut reference = media("image");
        reference.artifact_id = "quoted\" @expert =run()".into();
        let note = media_source_note(&[], &[], &[reference.clone()], "host", "new").expect("map");
        let value: Vec<Value> =
            serde_json::from_str(note.split_once(":\n").expect("label").1).expect("escaped JSON");
        assert_eq!(value[0]["artifact_id"], reference.artifact_id);
    }
}
