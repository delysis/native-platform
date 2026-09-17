//! Native session document writes through the current store authority. Callers
//! retain their application/session admission guard while using the leased store.
use loom_document::DocumentContent;
use loom_store::{IdempotentSaveOutcome, ProjectStore, StoreError, TransientDraftWriteOutcome};
use loom_types::{BlobId, CommandId, DocumentId, DocumentKind, RevisionId};

#[derive(Clone, Copy, Debug)]
pub struct DocumentAddress<'a> {
    pub document_id: DocumentId,
    pub relative_path: &'a str,
}

#[derive(Debug)]
pub struct Checkpoint<'a> {
    pub document: DocumentAddress<'a>,
    pub source_revision_id: RevisionId,
    pub visible_blob_id: BlobId,
    pub command_id: CommandId,
    pub draft_version: Option<u64>,
    pub content: DocumentContent,
}

#[derive(Debug)]
pub struct DraftWrite<'a> {
    pub document: DocumentAddress<'a>,
    pub source_revision_id: RevisionId,
    pub expected_version: u64,
    pub content: DocumentContent,
}

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("The requested document is not registered in this project")]
    NotFound,
    #[error("The document identity does not match the authorized project entry")]
    IdentityMismatch,
    #[error("Hybrid editing is locked until block metadata can be preserved losslessly")]
    HybridUnsupported,
    #[error(transparent)]
    Document(#[from] loom_document::DocumentError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub fn registered_kind(
    store: &ProjectStore,
    address: DocumentAddress<'_>,
) -> Result<DocumentKind, PersistenceError> {
    let document = store
        .list_documents()?
        .into_iter()
        .find(|d| d.relative_path == address.relative_path)
        .ok_or(PersistenceError::NotFound)?;
    if document.document_id != address.document_id {
        return Err(PersistenceError::IdentityMismatch);
    }
    Ok(document.kind)
}

pub fn checkpoint(
    store: &mut ProjectStore,
    request: Checkpoint<'_>,
) -> Result<IdempotentSaveOutcome, PersistenceError> {
    registered_kind(store, request.document)?;
    reject_hybrid(request.content.kind())?;
    // Preserve the original command fingerprint, including its reason string.
    let outcome = if let Some(version) = request.draft_version {
        store.save_document_if_source_idempotent_consuming_draft(
            request.command_id,
            request.document.relative_path,
            request.content,
            "editor idle checkpoint",
            request.source_revision_id,
            request.visible_blob_id,
            version,
        )
    } else {
        store.save_document_if_source_idempotent(
            request.command_id,
            request.document.relative_path,
            request.content,
            "editor idle checkpoint",
            request.source_revision_id,
            request.visible_blob_id,
        )
    }?;
    Ok(outcome)
}

pub fn write_draft(
    store: &mut ProjectStore,
    request: DraftWrite<'_>,
) -> Result<TransientDraftWriteOutcome, PersistenceError> {
    reject_hybrid(request.content.kind())?;
    registered_kind(store, request.document)?;
    let kind = request.content.kind();
    let canonical = request.content.project_visible()?;
    match store.upsert_transient_draft(
        request.document.relative_path,
        request.source_revision_id,
        request.expected_version,
        request.content,
    ) {
        Ok(outcome) => Ok(outcome),
        Err(error @ StoreError::TransientDraftVersionConflict { .. }) => {
            let existing = store.load_transient_draft(request.document.relative_path)?;
            match existing {
                Some(draft)
                    if draft.document_id == request.document.document_id
                        && draft.source_revision_id == request.source_revision_id
                        && draft.kind == kind
                        && draft.text.as_bytes() == canonical.bytes =>
                {
                    Ok(TransientDraftWriteOutcome {
                        draft,
                        replayed: true,
                    })
                }
                _ => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

pub fn clear_draft(
    store: &mut ProjectStore,
    document: DocumentAddress<'_>,
    expected_version: u64,
) -> Result<bool, PersistenceError> {
    registered_kind(store, document)?;
    match store.clear_transient_draft(document.relative_path, expected_version) {
        Ok(cleared) => Ok(cleared),
        Err(error @ StoreError::TransientDraftVersionConflict { .. }) => {
            if store
                .load_transient_draft(document.relative_path)?
                .is_none()
            {
                Ok(true)
            } else {
                Err(error.into())
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn reject_hybrid(kind: DocumentKind) -> Result<(), PersistenceError> {
    if kind == DocumentKind::Hybrid {
        Err(PersistenceError::HybridUnsupported)
    } else {
        Ok(())
    }
}
