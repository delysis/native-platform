//! Borrowed common document projection over an explicitly selected chat path.
//! This is NOT a store codec, chat template, authorization grant or migration.
//! Full typed Message metadata remains available; display never invents roles.

use crate::conversation_store::Message;
use workspace_document::{Document, DocumentError, PartKind};

/// The caller owns branch selection and supplies its exact ordered messages.
/// Metadata borrows the original record, retaining attribution, attachments,
/// receipts, parent IDs, reasoning, model identity and all other product fields.
pub fn from_messages(messages: &[Message]) -> Result<Document<'_, &str, &Message>, DocumentError> {
    let mut document = Document::new();
    for message in messages {
        document.push_text(
            message.id.as_str(),
            &message.content,
            PartKind::Message(message.role.clone()),
            message,
        )?;
    }
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation_store::MessageRole;

    #[test]
    fn selected_path_retains_full_metadata_and_exact_authored_bytes() {
        let message = Message {
            id: "assistant-occurrence".into(),
            conversation_id: "chat".into(),
            role: MessageRole::Assistant,
            content: "  café\r\n".into(),
            created_at: "2026-09-24".into(),
            parent_id: Some("user-occurrence".into()),
            model: Some("pinned-model".into()),
            receipt_id: Some("receipt".into()),
            prompt_tokens: Some(3),
            completion_tokens: Some(2),
            reasoning_content: Some("retained private reasoning".into()),
            reasoning_incomplete: false,
            branch_index: Some(0),
            branch_count: Some(2),
            attribution: None,
            attachment_ids: vec!["attachment-occurrence".into()],
        };
        let messages = [message];
        let document = from_messages(&messages).expect("common projection");
        assert_eq!(document.text(), messages[0].content);
        assert!(std::ptr::eq(*document.parts()[0].metadata(), &messages[0]));
        assert_eq!(
            document.parts()[0].metadata().attachment_ids,
            messages[0].attachment_ids
        );
        assert!(!document.text().contains("retained private reasoning"));
        assert_eq!(
            document.parts()[0].kind(),
            PartKind::Message(MessageRole::Assistant)
        );
    }
}
