//! Common document projection and validated chat-branch selection.
//! This is NOT a store codec, chat template, authorization grant or migration.
//! Full typed Message metadata remains available; display never invents roles.

use crate::conversation_store::{Conversation, Message};
use workspace_document::{BranchIndex, Document, DocumentError, HistoryError, HistoryNode, PartKind};

impl HistoryNode for Message {
    type Id = String;
    fn id(&self) -> &Self::Id { &self.id }
    fn parent_id(&self) -> Option<&Self::Id> { self.parent_id.as_ref() }
}

/// Unlike the historical display projection, inference cannot repair a stale
/// selected head, duplicate IDs, missing ancestry or cycles by guessing a path.
/// The shared index validates unselected branches as well as the selected one.
/// Valid inputs retain the existing role-filtered sibling ordering and fields.
pub fn checked_active_messages(conversation: &Conversation) -> Result<Vec<Message>, HistoryError> {
    let index = BranchIndex::new(&conversation.messages)?;
    let head = conversation.active_leaf_message_id.as_ref()
        .or_else(|| conversation.messages.last().map(|message| &message.id));
    index.path(head)?.into_iter().map(|message| {
        let mut siblings = index.siblings(&message.id)?;
        siblings.retain(|candidate| candidate.role == message.role);
        siblings.sort_by(|left, right| left.created_at.cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id)));
        let mut selected = message.clone();
        selected.branch_count = Some(siblings.len());
        selected.branch_index = siblings.iter().position(|candidate| candidate.id == message.id)
            .map(|position| position + 1);
        Ok(selected)
    }).collect()
}

/// Resolve an explicit branch into the same immutable projection used by a
/// manuscript. No history or attachment bytes are copied into a new store.
pub fn from_conversation(conversation: &Conversation) -> anyhow::Result<Document<'_, &str, &Message>> {
    let index = BranchIndex::new(&conversation.messages)?;
    let head = conversation.active_leaf_message_id.as_ref()
        .or_else(|| conversation.messages.last().map(|message| &message.id));
    let mut document = Document::new();
    for message in index.path(head)? {
        document.push_text(message.id.as_str(), &message.content,
            PartKind::Message(message.role.clone()), message)?;
    }
    Ok(document)
}

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
    use crate::conversation_store::{ConversationExecutionProfile, ConversationKind, MessageRole};

    fn message(id: &str, parent: Option<&str>, role: MessageRole, text: &str) -> Message {
        Message {
            id: id.into(), conversation_id: "chat".into(), role, content: text.into(),
            created_at: "1".into(), parent_id: parent.map(str::to_owned), model: None,
            receipt_id: None, prompt_tokens: None, completion_tokens: None, reasoning_content: None,
            reasoning_incomplete: false, branch_index: None, branch_count: None, attribution: None,
            attachment_ids: Vec::new(),
        }
    }
    fn conversation(messages: Vec<Message>, head: Option<&str>) -> Conversation {
        Conversation {
            id: "chat".into(), title: "chat".into(), created_at: "1".into(), updated_at: "1".into(),
            kind: ConversationKind::Chat, execution_profile: ConversationExecutionProfile::default(),
            selected_model_path: None, source_conversation_id: None, source_message_id: None,
            branch_root_message_id: None, active_leaf_message_id: head.map(str::to_owned),
            current_skill_ids: Vec::new(), messages,
        }
    }

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

    #[test]
    fn checked_path_preserves_the_valid_display_projection_and_source() {
        let input = conversation(vec![
            message("u", None, MessageRole::User, "  exact\r\n"),
            message("a", Some("u"), MessageRole::Assistant, "first"),
            message("b", Some("u"), MessageRole::Assistant, "second"),
            message("t", Some("u"), MessageRole::Tool, "tool"),
        ], Some("b"));
        let original = input.clone();
        assert_eq!(checked_active_messages(&input).expect("valid branch"),
            crate::conversation_store::active_path_messages(&input));
        let document = from_conversation(&input).expect("project branch");
        assert_eq!(document.text(), "  exact\r\nsecond");
        assert_eq!(document.parts().len(), 2);
        assert!(std::ptr::eq(*document.parts()[1].metadata(), &input.messages[2]));
        assert_eq!(input, original);
    }

    #[test]
    fn an_explicit_missing_head_never_selects_the_last_other_branch() {
        let input = conversation(vec![message("u", None, MessageRole::User, "private")], Some("gone"));
        assert_eq!(checked_active_messages(&input).expect_err("stale head"), HistoryError::MissingHead);
        assert!(from_conversation(&input).is_err());
    }

    #[test]
    fn corrupt_ancestry_never_becomes_a_truncated_consult_snapshot() {
        for (messages, expected) in [
            (vec![message("a", None, MessageRole::User, "one"),
                message("a", None, MessageRole::User, "two")], HistoryError::DuplicateId),
            (vec![message("a", Some("gone"), MessageRole::User, "orphan")], HistoryError::MissingParent),
            (vec![message("a", Some("b"), MessageRole::User, "cycle"),
                message("b", Some("a"), MessageRole::Assistant, "cycle")], HistoryError::Cycle),
        ] {
            let input = conversation(messages, Some("a"));
            assert_eq!(checked_active_messages(&input).expect_err("corrupt graph"), expected);
        }
    }

    #[test]
    fn unselected_corrupt_branches_are_not_ignored() {
        let input = conversation(vec![
            message("good", None, MessageRole::User, "selected"),
            message("bad", Some("bad"), MessageRole::User, "unselected"),
        ], Some("good"));
        assert_eq!(checked_active_messages(&input).expect_err("cycle"), HistoryError::Cycle);
    }

    #[test]
    fn absent_selection_keeps_the_existing_last_message_default() {
        let input = conversation(vec![
            message("u", None, MessageRole::User, "u"),
            message("a", Some("u"), MessageRole::Assistant, "a"),
        ], None);
        assert_eq!(checked_active_messages(&input).expect("last message"),
            crate::conversation_store::active_path_messages(&input));
        assert!(checked_active_messages(&conversation(Vec::new(), None)).expect("empty").is_empty());
    }
}
