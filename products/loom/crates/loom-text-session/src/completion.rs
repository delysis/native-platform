//! Exact manuscript admission shared by desktop frontends. Callers retain their
//! application/session and model admission guards; this read grants neither
//! inference authority nor permission to promote generated text.
use crate::persistence::DocumentAddress;
use loom_store::{LoadedDocument, ProjectStore, StoreError};
use loom_types::{BlobId, DocumentId, ProjectId, RevisionId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceIdentity {
    pub document_id: DocumentId,
    pub revision_id: RevisionId,
    pub visible_blob_id: BlobId,
}

impl SourceIdentity {
    pub fn of(document: &LoadedDocument) -> Self {
        Self {
            document_id: document.document_id,
            revision_id: document.revision_id,
            visible_blob_id: document.blob_id,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SourceRequest<'a> {
    pub document: DocumentAddress<'a>,
    pub source_revision_id: RevisionId,
    pub visible_blob_id: BlobId,
    pub cursor_byte: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("the document ID does not match the requested manuscript")]
    DocumentMismatch,
    #[error("the manuscript revision changed before generation began")]
    RevisionConflict,
    #[error("the visible manuscript bytes changed before generation began")]
    BlobConflict,
    #[error("the generation cursor exceeds this platform's addressable range")]
    CursorOverflow,
    #[error("the generation cursor is not a UTF-8 boundary in the source revision")]
    CursorBoundary,
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Constructed only by a read from the leased store. Keep the exact bytes with
/// their validated cursor; reparsing a display string cannot replace this source.
pub struct CapturedSource {
    project_id: ProjectId,
    document: LoadedDocument,
    cursor: usize,
}

impl std::fmt::Debug for CapturedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedSource")
            .field("project_id", &self.project_id)
            .field("source", &self.identity())
            .field("cursor", &self.cursor)
            .field("source_bytes", &self.document.text.len())
            .finish_non_exhaustive()
    }
}

impl CapturedSource {
    pub fn project_id(&self) -> ProjectId {
        self.project_id
    }
    pub fn identity(&self) -> SourceIdentity {
        SourceIdentity::of(&self.document)
    }
    pub fn document(&self) -> &LoadedDocument {
        &self.document
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn prefix(&self) -> &str {
        &self.document.text[..self.cursor]
    }
}

pub fn capture(
    store: &ProjectStore,
    request: SourceRequest<'_>,
) -> Result<CapturedSource, CaptureError> {
    let document = store.read_document(request.document.relative_path)?;
    if document.document_id != request.document.document_id {
        return Err(CaptureError::DocumentMismatch);
    }
    if document.revision_id != request.source_revision_id {
        return Err(CaptureError::RevisionConflict);
    }
    if document.blob_id != request.visible_blob_id {
        return Err(CaptureError::BlobConflict);
    }
    let cursor = usize::try_from(request.cursor_byte).map_err(|_| CaptureError::CursorOverflow)?;
    if cursor > document.text.len() || !document.text.is_char_boundary(cursor) {
        return Err(CaptureError::CursorBoundary);
    }
    Ok(CapturedSource {
        project_id: store.manifest().project_id,
        document,
        cursor,
    })
}
