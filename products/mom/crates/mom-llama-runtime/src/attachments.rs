    store.get_bytes(&format!("attachment.blob.{attachment_id}"))
}

mod bindings;
mod scoped_context;
pub(crate) use bindings::{AttachmentMediaReference, AttachmentContextSource, SelectedAttachmentSource, SelectedAttachmentMedia, SelectedMediaBuilder, native_media_id, validate_selected_attachment_budget};
pub(crate) use scoped_context::{
    CurrentAttachmentSelection, ScopedChatAttachmentContext, prepare_scoped_chat_attachments,
};

pub(crate) fn prepare_chat_attachments(
    conversation_id: &str,
    active_messages: &[Message],
    regenerate_user_id: Option<&str>,
) -> Result<std::result::Result<ChatAttachmentContext, AttachmentContextBlocker>> {
    let selection = match regenerate_user_id {
        Some(id) if active_messages.iter().any(|message| message.id == id) => {
            CurrentAttachmentSelection::ExistingUser(id)
        }
        Some(_) => CurrentAttachmentSelection::HistoryOnly,
        None => CurrentAttachmentSelection::Draft,
    };
    prepare_scoped_chat_attachments(conversation_id, active_messages, selection)
        .map(|result| result.map(|scoped| scoped.context))
}

pub(crate) fn commit_generated_exchange(
    fallback_db: ConversationDb,
    conversation: Conversation,
    expected_active_leaf: Option<&str>,
