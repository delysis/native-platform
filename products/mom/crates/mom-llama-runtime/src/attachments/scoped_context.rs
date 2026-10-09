//! One attachment preparation path for direct chat, consults and regeneration.
//! History-only access is an enum case, never an invented message-ID sentinel.

use super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) enum CurrentAttachmentSelection<'a> {
    Draft,
    ExistingUser(&'a str),
    HistoryOnly,
}

#[derive(Clone, Debug)]
pub(crate) struct ScopedChatAttachmentContext {
    pub context: ChatAttachmentContext,
    pub media_by_message_id: HashMap<String, Vec<AttachmentMediaReference>>,
    pub current_media: Vec<AttachmentMediaReference>,
    pub sources_by_message_id: HashMap<String, Vec<AttachmentContextSource>>,
    pub current_sources: Vec<AttachmentContextSource>,
}

impl std::ops::Deref for ScopedChatAttachmentContext {
    type Target = ChatAttachmentContext;
    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

pub(crate) fn prepare_scoped_chat_attachments(
    conversation_id: &str,
    active_messages: &[Message],
    current_selection: CurrentAttachmentSelection<'_>,
) -> Result<std::result::Result<ScopedChatAttachmentContext, AttachmentContextBlocker>> {
    let mut message_ids = BTreeSet::new();
    if active_messages
        .iter()
        .any(|message| !message_ids.insert(message.id.as_str()))
    {
        return Ok(Err(context_blocker(
            "attachment_message_identity_conflict",
            "The selected history contains duplicate message occurrence IDs.".into(),
        )));
    }
    if active_messages
        .iter()
        .any(|message| message.conversation_id != conversation_id)
    {
        return Ok(Err(context_blocker(
            "attachment_message_ownership_mismatch",
            "A selected message belongs to a different conversation.".into(),
        )));
    }
    let current_policy_fingerprint = attachment_host()?.policy_fingerprint().to_string();
    let attachment_db = load_attachment_db()?;
    let records = attachment_db
        .attachments
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect::<HashMap<_, _>>();
    if records.len() != attachment_db.attachments.len() {
        return Ok(Err(context_blocker(
            "attachment_record_identity_conflict",
            "The retained attachment registry contains duplicate occurrence IDs.".into(),
        )));
    }
    let draft_snapshot = if matches!(current_selection, CurrentAttachmentSelection::Draft) {
        load_drafts()?
            .drafts
            .into_iter()
            .find(|draft| draft.conversation_id.as_deref() == Some(conversation_id))
    } else {
        None
    };
    let current_ids = match current_selection {
        CurrentAttachmentSelection::Draft => draft_snapshot
            .as_ref()
            .map(|draft| draft.attachment_ids.clone())
            .unwrap_or_default(),
        CurrentAttachmentSelection::ExistingUser(id) => {
            let Some(message) = active_messages.iter().find(|message| message.id == id) else {
                return Ok(Err(context_blocker(
                    "attachment_regeneration_message_missing",
                    "The selected regeneration message is absent from its exact history.".into(),
                )));
            };
            if message.role != crate::conversation_store::MessageRole::User {
                return Ok(Err(context_blocker(
                    "attachment_regeneration_role_invalid",
                    "Only an exact user message can own regeneration input attachments.".into(),
                )));
            }
            message.attachment_ids.clone()
        }
        CurrentAttachmentSelection::HistoryOnly => Vec::new(),
    };
    let store = RuntimeStore::current()?;
    let mut text_by_message_id = HashMap::new();
    let mut sources_by_message_id = HashMap::new();
    let mut media_by_message_id = HashMap::new();
    let mut media = Vec::new();
    let mut emitted_media = llama_native_types::media_identity::MediaIdentityLedger::default();
    let mut budget = ActiveAttachmentBudget::default();
    let mut resolution = AttachmentResolution {
        store: &store,
        emitted_media: &mut emitted_media,
        budget: &mut budget,
        current_policy_fingerprint: &current_policy_fingerprint,
    };
    for message in active_messages {
        if message.attachment_ids.is_empty() {
            continue;
        }
        let resolved = match resolve_attachment_set(
            conversation_id,
            &message.attachment_ids,
            AttachmentState::Committed,
            &records,
            &mut resolution,
        )? {
            Ok(resolved) => resolved,
            Err(blocked) => return Ok(Err(blocked)),
        };
        if !resolved.context.text.is_empty() {
            text_by_message_id.insert(message.id.clone(), resolved.context.text);
        }
        media_by_message_id.insert(message.id.clone(), resolved.references);
        sources_by_message_id.insert(message.id.clone(), resolved.sources);
        media.extend(resolved.context.media);
    }
    let current = match resolve_attachment_set(
        conversation_id,
        &current_ids,
        if matches!(current_selection, CurrentAttachmentSelection::Draft) {
            AttachmentState::Staged
        } else {
            AttachmentState::Committed
        },
        &records,
        &mut resolution,
    )? {
        Ok(resolved) => resolved,
        Err(blocked) => return Ok(Err(blocked)),
    };
    media.extend(current.context.media);
    Ok(Ok(ScopedChatAttachmentContext {
        context: ChatAttachmentContext {
            staged_ids: if matches!(current_selection, CurrentAttachmentSelection::Draft) {
                current_ids
            } else {
                Vec::new()
            },
            draft_snapshot,
            text_by_message_id,
            current_text: current.context.text,
            media,
        },
        media_by_message_id,
        current_media: current.references,
        sources_by_message_id,
        current_sources: current.sources,
    }))
}
