    store.get_bytes(&format!("attachment.blob.{attachment_id}"))
}

pub(crate) fn prepare_chat_attachments(
    conversation_id: &str,
    active_messages: &[Message],
    regenerate_user_id: Option<&str>,
) -> Result<std::result::Result<ChatAttachmentContext, AttachmentContextBlocker>> {
    let current_policy_fingerprint = attachment_host()?.policy_fingerprint().to_string();
    let attachment_db = load_attachment_db()?;
    let records = attachment_db
        .attachments
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect::<HashMap<_, _>>();
    let draft_snapshot = if regenerate_user_id.is_none() {
        load_drafts()?
            .drafts
            .into_iter()
            .find(|draft| draft.conversation_id.as_deref() == Some(conversation_id))
    } else {
        None
    };
    let draft_ids = if regenerate_user_id.is_none() {
        draft_snapshot
            .as_ref()
            .map(|draft| draft.attachment_ids.clone())
            .unwrap_or_default()
    } else {
        active_messages
            .iter()
            .find(|message| message.id == regenerate_user_id.unwrap_or_default())
            .map(|message| message.attachment_ids.clone())
            .unwrap_or_default()
    };
    let store = RuntimeStore::current()?;
    let mut text_by_message_id = HashMap::new();
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
        if !resolved.text.is_empty() {
            text_by_message_id.insert(message.id.clone(), resolved.text);
        }
        media.extend(resolved.media);
    }
    let current = match resolve_attachment_set(
        conversation_id,
        &draft_ids,
        if regenerate_user_id.is_some() {
            AttachmentState::Committed
        } else {
            AttachmentState::Staged
        },
        &records,
        &mut resolution,
    )? {
        Ok(resolved) => resolved,
        Err(blocked) => return Ok(Err(blocked)),
    };
    media.extend(current.media);
    Ok(Ok(ChatAttachmentContext {
        staged_ids: if regenerate_user_id.is_none() {
            draft_ids
        } else {
            Vec::new()
        },
        draft_snapshot,
        text_by_message_id,
        current_text: current.text,
        media,
    }))
}

pub(crate) fn commit_generated_exchange(
    fallback_db: ConversationDb,
    conversation: Conversation,
    expected_active_leaf: Option<&str>,
