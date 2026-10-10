//! Source-bound external-edit review for the native session. Preview is read
//! only. Applying a reviewed result records the existing immutable import/merge
//! workflow and deliberately leaves transient drafts for explicit consumption.
use crate::persistence::{self, DocumentAddress, PersistenceError};
use loom_document::{DocumentContent, MergeOutcome, three_way_merge};
use loom_store::{ExternalReconciliationOutcome, ExternalReconciliationRequest, ProjectStore};
use loom_types::{ArtifactId, BlobId, CommandId, DocumentId, DocumentKind, ProjectId, RevisionId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppSource {
    Caller,
    TransientDraft,
    Base,
}

#[derive(Debug)]
pub struct PreviewRequest<'a> {
    pub project_id: ProjectId,
    pub document: DocumentAddress<'a>,
    pub revision_id: RevisionId,
    pub base_blob_id: BlobId,
    pub app_text: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub struct Preview {
    pub project_id: ProjectId,
    pub document_id: DocumentId,
    pub relative_path: String,
    pub kind: DocumentKind,
    pub active_revision_id: RevisionId,
    pub active_artifact_id: ArtifactId,
    pub base_blob_id: BlobId,
    pub app_blob_id: BlobId,
    pub external_blob_id: BlobId,
    pub external_visible_blob_id: BlobId,
    pub base_text: String,
    pub app_text: String,
    pub external_text: String,
    pub external_visible_text: String,
    pub app_source: AppSource,
    pub draft_version: Option<u64>,
    pub outcome: MergeOutcome,
}

#[derive(Debug)]
pub struct ApplyRequest<'a> {
    pub document: DocumentAddress<'a>,
    pub revision_id: RevisionId,
    pub base_blob_id: BlobId,
    pub visible_blob_id: BlobId,
    pub content: DocumentContent,
    pub reason: String,
    pub command_id: CommandId,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("This reconciliation does not belong to the open project")]
    ProjectMismatch,
    #[error("The document identity does not match the requested document")]
    DocumentMismatch,
    #[error("The requested path is not the registered document path")]
    PathMismatch,
    #[error("The active revision changed before reconciliation preview")]
    RevisionMismatch,
    #[error("The active base blob changed before reconciliation preview")]
    BaseMismatch,
    #[error("Hybrid reconciliation requires lossless block metadata")]
    HybridUnsupported,
    #[error("The external document was deleted; restore or import it before reconciling")]
    Deleted,
    #[error("The visible document still matches the active revision")]
    Unchanged,
    #[error("The immutable reconciliation base does not match its content identity")]
    BaseHashMismatch,
    #[error("The external reconciliation snapshot changed while it was being read")]
    ExternalHashMismatch,
    #[error("The recoverable draft is not based on the active reconciliation source")]
    StaleDraft,
    #[error("The immutable base is not canonical for its registered document kind")]
    NoncanonicalBase,
    #[error("The reconciliation kind does not match the registered document")]
    KindMismatch,
    #[error(transparent)]
    Store(#[from] loom_store::StoreError),
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    Document(#[from] loom_document::DocumentError),
    #[error(transparent)]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error(transparent)]
    Merge(#[from] loom_document::MergeError),
}

pub fn preview(store: &ProjectStore, request: &PreviewRequest<'_>) -> Result<Preview, Error> {
    if store.manifest().project_id != request.project_id {
        return Err(Error::ProjectMismatch);
    }
    let snapshot = store.reconciliation_snapshot(request.document.relative_path)?;
    if snapshot.document_id != request.document.document_id {
        return Err(Error::DocumentMismatch);
    }
    if snapshot.relative_path != request.document.relative_path {
        return Err(Error::PathMismatch);
    }
    if snapshot.active_revision_id != request.revision_id {
        return Err(Error::RevisionMismatch);
    }
    if snapshot.active_blob_id != request.base_blob_id {
        return Err(Error::BaseMismatch);
    }
    if snapshot.kind == DocumentKind::Hybrid {
        return Err(Error::HybridUnsupported);
    }
    let visible = snapshot.visible.as_ref().ok_or(Error::Deleted)?;
    if snapshot.visible_matches_active {
        return Err(Error::Unchanged);
    }
    if BlobId::digest(snapshot.base_text.as_bytes()) != snapshot.active_blob_id {
        return Err(Error::BaseHashMismatch);
    }
    if BlobId::digest(visible.text.as_bytes()) != visible.blob_id {
        return Err(Error::ExternalHashMismatch);
    }
    let draft = store.load_transient_draft(request.document.relative_path)?;
    if let Some(draft) = &draft
        && (draft.document_id != snapshot.document_id
            || draft.source_revision_id != snapshot.active_revision_id
            || draft.kind != snapshot.kind
            || draft.blob_id != BlobId::digest(draft.text.as_bytes()))
    {
        return Err(Error::StaleDraft);
    }
    let (candidate, app_source) = match request.app_text {
        Some(text) => (text, AppSource::Caller),
        None => match &draft {
            Some(draft) => (draft.text.as_str(), AppSource::TransientDraft),
            None => (snapshot.base_text.as_str(), AppSource::Base),
        },
    };
    let base_text = canonical_text(snapshot.kind, &snapshot.base_text)?;
    if base_text != snapshot.base_text {
        return Err(Error::NoncanonicalBase);
    }
    let app_text = canonical_text(snapshot.kind, candidate)?;
    let external_text = canonical_text(snapshot.kind, &visible.text)?;
    let outcome = three_way_merge(snapshot.kind, &base_text, &app_text, &external_text)?;
    Ok(Preview {
        project_id: request.project_id,
        document_id: snapshot.document_id,
        relative_path: snapshot.relative_path,
        kind: snapshot.kind,
        active_revision_id: snapshot.active_revision_id,
        active_artifact_id: snapshot.active_artifact_id,
        base_blob_id: snapshot.active_blob_id,
        app_blob_id: BlobId::digest(app_text.as_bytes()),
        external_blob_id: BlobId::digest(external_text.as_bytes()),
        external_visible_blob_id: visible.blob_id,
        base_text,
        app_text,
        external_text,
        external_visible_text: visible.text.clone(),
        app_source,
        draft_version: draft.as_ref().map(|draft| draft.version),
        outcome,
    })
}

pub fn apply(
    store: &mut ProjectStore,
    request: ApplyRequest<'_>,
) -> Result<ExternalReconciliationOutcome, Error> {
    if request.content.kind() == DocumentKind::Hybrid {
        return Err(Error::HybridUnsupported);
    }
    if persistence::registered_kind(store, request.document)? != request.content.kind() {
        return Err(Error::KindMismatch);
    }
    Ok(store.reconcile_external_idempotent(
        request.command_id,
        ExternalReconciliationRequest {
            relative_path: request.document.relative_path.into(),
            expected_active_revision_id: request.revision_id,
            expected_base_blob_id: request.base_blob_id,
            expected_visible_blob_id: request.visible_blob_id,
            resolved_content: request.content,
            reason: request.reason,
        },
    )?)
}

fn canonical_text(kind: DocumentKind, text: &str) -> Result<String, Error> {
    let content = DocumentContent::from_visible(kind, text.as_bytes().to_vec())?;
    Ok(String::from_utf8(content.project_visible()?.bytes)?)
}
