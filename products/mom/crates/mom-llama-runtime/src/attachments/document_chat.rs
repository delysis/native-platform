//! Atomic draft preparation for a native document chat view.
//! Ownership is an exact stored draft snapshot, not a filename or UI label.

use super::*;

const SCHEMA: &str = "mom_llama.document_chat_draft.v1";
const MAX_CONTEXTS: usize = 16;
const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentChatBinding {
    schema: String,
    input_id: String,
    draft: DraftMessage,
}

/// Replace only an unchanged adapter-owned draft. Canonical attachments,
/// draft ownership and immutable input evidence commit together. Old failed
/// snapshots remain retained and are never included in the next request.
pub fn prepare_document_chat_in_scope(
    scope: &crate::OperationScope,
    conversation_id: &str,
    message: String,
    context: Vec<String>,
) -> Result<DraftMessage> {
    if !crate::conversation_store::valid_occurrence_id(conversation_id) {
        anyhow::bail!("invalid document chat conversation identity");
    }
    let bytes = context
        .iter()
        .try_fold(message.len(), |total, text| total.checked_add(text.len()))
        .ok_or_else(|| anyhow!("document chat input size overflow"))?;
    if context.len() > MAX_CONTEXTS || bytes > MAX_INPUT_BYTES {
        anyhow::bail!("document chat input exceeds its bounded context budget");
    }
    if scope.cancellation_requested() {
        anyhow::bail!("document chat cancelled before context preparation");
    }
    let host = attachment_host()?;
    let mut prepared = Vec::with_capacity(context.len());
    for text in context {
        if scope.cancellation_requested() {
            anyhow::bail!("document chat cancelled during context preparation");
        }
        let provided = ProvidedAttachment::from_bytes(
            "document-context.txt",
            Some("text/plain".to_string()),
            text.into_bytes(),
        );
        let canonicalized = host.inspect_and_canonicalize(provided)?;
        prepared.push(canonical_attachment_record(
            canonicalized,
            &host,
            conversation_id,
            "document-context.txt".into(),
            "document-chat-context".into(),
            None,
        )?);
    }
    // The ordinary kernel creates chats with the current default profile.
    // Recheck its kind in the write transaction before touching a draft.
    crate::conversation_store::get_or_create_conversation(conversation_id)?;
    let store = RuntimeStore::current()?;
    let namespace = format!("document-chat-draft.v1:{conversation_id}");
    let input_id = Uuid::new_v4().to_string();
    let _lifecycle = lock_attachment_lifecycle()?;
    store.mutate_documents(
        &namespace,
        || None::<DocumentChatBinding>,
        |binding, documents| {
            if scope.cancellation_requested() {
                anyhow::bail!("document chat cancelled before context commit");
            }
            if binding
                .as_ref()
                .is_some_and(|binding| binding.schema != SCHEMA)
            {
                anyhow::bail!("incompatible document chat draft schema");
            }
            let conversations = documents
                .get::<ConversationDb>(CONVERSATIONS_NAMESPACE)?
                .unwrap_or_default();
            let conversation = conversations
                .conversations
                .iter()
                .find(|item| item.id == conversation_id)
                .ok_or_else(|| anyhow!("document chat was removed before context commit"))?;
            if conversation.kind != crate::ConversationKind::Chat {
                anyhow::bail!(
                    "document chat context requires an ordinary chat, not a frozen template"
                );
            }
            let mut drafts = documents
                .get::<DraftDb>(DRAFTS_NAMESPACE)?
                .unwrap_or_default();
            let current = drafts
                .drafts
                .iter()
                .find(|draft| draft.conversation_id.as_deref() == Some(conversation_id));
            if let Some(current) = current {
                let owned = binding
                    .as_ref()
                    .is_some_and(|binding| binding.draft == *current);
                if !owned && (!current.message.is_empty() || !current.attachment_ids.is_empty()) {
                    anyhow::bail!("document chat draft has unrelated edits or attachments");
                }
            }
            let draft = DraftMessage {
                conversation_id: Some(conversation_id.to_string()),
                message,
                attachment_ids: prepared.iter().map(|item| item.record.id.clone()).collect(),
                updated_at: now_ms().to_string(),
            };
            drafts
                .drafts
                .retain(|draft| draft.conversation_id.as_deref() != Some(conversation_id));
            drafts.drafts.push(draft.clone());
            let mut attachments = documents
                .get::<AttachmentDb>(ATTACHMENTS_NAMESPACE)?
                .unwrap_or_default();
            for item in prepared {
                documents.put_bytes(
                    item.record
                        .manifest_namespace
                        .as_ref()
                        .expect("canonical manifest"),
                    &serde_json::to_vec(&item.manifest)?,
                )?;
                for (namespace, bytes) in item.blobs {
                    documents.put_bytes(&namespace, &bytes)?;
                }
                attachments.attachments.push(item.record);
            }
            documents.put_bytes(ATTACHMENTS_NAMESPACE, &serde_json::to_vec(&attachments)?)?;
            documents.put_bytes(DRAFTS_NAMESPACE, &serde_json::to_vec(&drafts)?)?;
            let owned = DocumentChatBinding {
                schema: SCHEMA.into(),
                input_id: input_id.clone(),
                draft: draft.clone(),
            };
            documents.put_receipt(
                &format!("document-chat-input:{input_id}"),
                "mom_llama.document_chat_prepare",
                &owned,
            )?;
            *binding = Some(owned);
            Ok(draft)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("mom-document-draft-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("fixture directory");
            crate::config::set_data_dir_override_for_tests(Some(path.clone()));
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            crate::config::set_data_dir_override_for_tests(None);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn prepare(scope: &crate::OperationScope, message: &str, text: &str) -> DraftMessage {
        prepare_document_chat_in_scope(scope, "chat", message.into(), vec![text.into()])
            .expect("prepare document input")
    }

    #[test]
    fn cancelled_owned_draft_is_replaced_after_reopening_without_losing_failed_sources() {
        let _fixture = Fixture::new();
        let stopped = crate::OperationScope::detached();
        let first = prepare(&stopped, "first question", "old source α");
        stopped.request_cancellation();
        drop(stopped);
        // A new scope and store access represent the next application lifetime.
        let next = crate::OperationScope::detached();
        let second = prepare(&next, "different question", "new source β");
        assert_eq!(second.message, "different question");
        assert_eq!(second.attachment_ids.len(), 1);
        assert_ne!(first.attachment_ids, second.attachment_ids);
        let store = RuntimeStore::current().expect("reopen encrypted store");
        let attachments: AttachmentDb = store
            .get(ATTACHMENTS_NAMESPACE)
            .expect("index")
            .expect("stored index");
        let old = attachments
            .attachments
            .iter()
            .find(|item| item.id == first.attachment_ids[0])
            .expect("retained failed source");
        let manifest: AttachmentManifest = store
            .get(old.manifest_namespace.as_ref().expect("manifest identity"))
            .expect("manifest")
            .expect("retained manifest");
        let root = manifest
            .graph
            .objects
            .iter()
            .find(|object| object.id == manifest.graph.root)
            .expect("root");
        assert_eq!(
            store
                .get_bytes(&object_namespace(&root.id))
                .expect("source bytes")
                .expect("retained bytes"),
            b"old source \xce\xb1"
        );
        let connection = rusqlite::Connection::open(store.path()).expect("receipt observation");
        let receipts: i64 = connection.query_row("SELECT count(*) FROM receipts WHERE command_id = 'mom_llama.document_chat_prepare'", [], |row| row.get(0)).expect("receipt count");
        assert_eq!(receipts, 2);
        assert_eq!(load_drafts().expect("current drafts").drafts[0], second);
    }

    #[test]
    fn unrelated_edits_are_not_overwritten_and_new_context_is_not_published() {
        let _fixture = Fixture::new();
        let scope = crate::OperationScope::detached();
        let first = prepare(&scope, "first question", "source");
        crate::draft_update(Some("chat"), "unrelated edit".into(), first.attachment_ids)
            .expect("author edit");
        let before = load_drafts().expect("draft snapshot");
        let attachment_before = load_attachment_db().expect("attachment snapshot");
        let error = prepare_document_chat_in_scope(
            &scope,
            "chat",
            "new question".into(),
            vec!["new source".into()],
        )
        .expect_err("protect unrelated edit");
        assert!(error.to_string().contains("unrelated edits"));
        assert_eq!(load_drafts().expect("draft after refusal"), before);
        assert_eq!(
            load_attachment_db().expect("attachments after refusal"),
            attachment_before
        );
    }

    #[test]
    fn matching_foreign_draft_is_not_claimed_by_the_document_adapter() {
        let _fixture = Fixture::new();
        crate::draft_update(Some("chat"), "same question".into(), Vec::new())
            .expect("foreign draft");
        let scope = crate::OperationScope::detached();
        let error = prepare_document_chat_in_scope(
            &scope,
            "chat",
            "same question".into(),
            vec!["source".into()],
        )
        .expect_err("not adapter owned");
        assert!(error.to_string().contains("unrelated edits"));
        assert!(load_attachment_db().expect("index").attachments.is_empty());
    }

    #[test]
    fn cancelled_preparation_leaves_owned_input_and_sources_unchanged() {
        let _fixture = Fixture::new();
        let scope = crate::OperationScope::detached();
        let first = prepare(&scope, "first question", "source");
        let before = load_attachment_db().expect("attachments");
        scope.request_cancellation();
        assert!(
            prepare_document_chat_in_scope(&scope, "chat", "other".into(), Vec::new()).is_err()
        );
        assert_eq!(load_drafts().expect("drafts").drafts[0], first);
        assert_eq!(load_attachment_db().expect("attachments"), before);
    }
}
