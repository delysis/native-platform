//! Explicit read-only interchange from the deprecated conversation store.
//! Normal encrypted payloads retain their existing codec. Export preserves
//! private metadata as data; it never grants the destination tool authority.
use std::path::PathBuf;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use workspace_document::{
    DocumentLineage, DocumentSelection, DocumentSnapshot, PartContent, PartKind, SnapshotPart,
    SourceKind, SourceLineage, SourceReference,
};

use crate::conversation_store::{
    Conversation, ConversationExecutionProfile, ConversationKind, MessageAttribution,
};

pub(crate) type ConversationSnapshot = DocumentSnapshot<ConversationMetadata, MessageMetadata>;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConversationMetadata {
    title: String,
    created_at: String,
    updated_at: String,
    kind: ConversationKind,
    execution_profile: ConversationExecutionProfile,
    selected_model_path: Option<PathBuf>,
    branch_root_message_id: Option<String>,
    current_skill_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageMetadata {
    created_at: String,
    model: Option<String>,
    receipt_id: Option<String>,
    prompt_tokens: Option<usize>,
    completion_tokens: Option<usize>,
    reasoning_content: Option<String>,
    reasoning_incomplete: bool,
    branch_index: Option<usize>,
    branch_count: Option<usize>,
    attribution: Option<MessageAttribution>,
    attachment_ids: Vec<String>,
}

pub(crate) fn freeze(conversation: &Conversation) -> Result<ConversationSnapshot> {
    let metadata = ConversationMetadata {
        title: conversation.title.clone(),
        created_at: conversation.created_at.clone(),
        updated_at: conversation.updated_at.clone(),
        kind: conversation.kind,
        execution_profile: conversation.execution_profile.clone(),
        selected_model_path: conversation.selected_model_path.clone(),
        branch_root_message_id: conversation.branch_root_message_id.clone(),
        current_skill_ids: conversation.current_skill_ids.clone(),
    };
    ensure!(
        conversation.source_conversation_id.is_some() || conversation.source_message_id.is_none(),
        "source message has no source document"
    );
    let lineage = DocumentLineage {
        revision: None,
        source: conversation
            .source_conversation_id
            .as_ref()
            .map(|id| SourceLineage {
                document_id: id.clone(),
                part_id: conversation.source_message_id.clone(),
            }),
    };
    let parts = conversation
        .messages
        .iter()
        .map(|message| SnapshotPart {
            id: message.id.clone(),
            parent_id: message.parent_id.clone(),
            kind: PartKind::Message(message.role),
            source: SourceReference {
                kind: SourceKind::Message,
                occurrence_id: message.id.clone(),
                start_byte: 0,
                end_byte: message.content.len() as u64,
            },
            content: PartContent::Inline(message.content.clone()),
            metadata: MessageMetadata {
                created_at: message.created_at.clone(),
                model: message.model.clone(),
                receipt_id: message.receipt_id.clone(),
                prompt_tokens: message.prompt_tokens,
                completion_tokens: message.completion_tokens,
                reasoning_content: message.reasoning_content.clone(),
                reasoning_incomplete: message.reasoning_incomplete,
                branch_index: message.branch_index,
                branch_count: message.branch_count,
                attribution: message.attribution.clone(),
                attachment_ids: message.attachment_ids.clone(),
            },
        })
        .collect();
    Ok(DocumentSnapshot::new(
        conversation.id.clone(),
        conversation
            .active_leaf_message_id
            .as_ref()
            .map_or(DocumentSelection::LatestBranch, |head| {
                DocumentSelection::Branch { head: head.clone() }
            }),
        lineage,
        metadata,
        parts,
    )?)
}
