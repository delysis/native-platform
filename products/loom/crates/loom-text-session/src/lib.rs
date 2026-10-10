#![forbid(unsafe_code)]
//! Bounded plain-text project operations for interchangeable Loom frontends.
//!
//! This crate owns file selection policy, conflict checks and persistence through
//! `loom-store`. It has no dependency on a view, renderer, window or EASL runtime.
pub mod autosave;
pub mod completion;
pub mod configuration;
mod documents;
pub mod lifecycle;
pub mod persistence;
pub mod reconciliation;
pub mod worker;
pub mod workspace;
#[cfg(test)]
use loom_document::DocumentContent;
use loom_store::{DocumentSummary, ProjectStore};
use loom_types::DocumentKind;
use std::path::Path;

pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_MANUSCRIPTS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextTarget {
    Manuscript(usize),
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Store(#[from] loom_store::StoreError),
    #[error(transparent)]
    Persistence(#[from] persistence::PersistenceError),
    #[error("Open this document before editing it")]
    DocumentNotLoaded,
    #[error("A draft from another source revision is preserved and needs reconciliation")]
    StaleDraft,
    #[error("Retry the pending checkpoint before changing or discarding its request")]
    CheckpointPending,
    #[error("This request belongs to another project session")]
    ProjectMismatch,
    #[error("The view cannot represent this document: {0}")]
    Admission(String),
    #[error("This plain-text session supports at most 16 manuscripts")]
    DocumentLimit,
    #[error("Text exceeds the session's 1 MiB limit")]
    TextLimit,
    #[error("Hybrid manuscripts require a structured editor")]
    StructuredDocument,
    #[error("Unknown manuscript")]
    UnknownDocument,
    #[error(transparent)]
    Creation(#[from] lifecycle::CreateError),
    #[error(transparent)]
    Completion(#[from] completion::CaptureError),
    #[error("Checkpoint the current manuscript before requesting completion")]
    CompletionNotCheckpointed,
    #[error("The document changed outside this editor; reopen before saving")]
    Conflict,
    #[error("Revision recorded; visible-file recovery required")]
    RecoveryRequired,
}

#[derive(Debug)]
pub struct TextProject {
    store: ProjectStore,
    entries: Vec<DocumentSummary>,
    loaded: documents::OpenDocuments,
}

impl TextProject {
    pub fn open(root: &Path) -> Result<Self, SessionError> {
        #[cfg(any(test, not(target_os = "macos")))]
        let mut store = ProjectStore::open_folder(root)?;
        #[cfg(all(not(test), target_os = "macos"))]
        let mut store = ProjectStore::open_folder_encrypted(root)?;
        let mut entries = store.list_documents()?;
        if entries.len() > MAX_MANUSCRIPTS {
            return Err(SessionError::DocumentLimit);
        }
        if entries.iter().any(|e| e.kind == DocumentKind::Hybrid) {
            return Err(SessionError::StructuredDocument);
        }
        if entries.is_empty() {
            lifecycle::create_untitled(&mut store)?;
            entries = store.list_documents()?;
        }
        store.record_open()?;
        Ok(Self {
            store,
            entries,
            loaded: documents::OpenDocuments::default(),
        })
    }

    pub fn root(&self) -> &Path {
        self.store.root()
    }
    pub fn entries(&self) -> &[DocumentSummary] {
        &self.entries
    }

    fn descriptor(&self, target: TextTarget) -> Result<(&str, DocumentKind), SessionError> {
        match target {
            TextTarget::Manuscript(index) => self
                .entries
                .get(index)
                .map(|e| (e.relative_path.as_str(), e.kind))
                .ok_or(SessionError::UnknownDocument),
        }
    }

    pub fn new_manuscript(&mut self) -> Result<usize, SessionError> {
        if self.entries.len() >= MAX_MANUSCRIPTS {
            return Err(SessionError::DocumentLimit);
        }
        let path = lifecycle::create_untitled(&mut self.store)?;
        self.entries = self.store.list_documents()?;
        self.entries
            .iter()
            .position(|e| e.relative_path == path)
            .ok_or(SessionError::UnknownDocument)
    }

    pub fn close(&mut self) -> Result<(), SessionError> {
        self.store.record_close()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verse_drafts_and_checkpoints_keep_every_byte_and_the_captured_kind() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, _) = ProjectStore::initialize(root.path(), "Verse").unwrap();
        store
            .save_document(
                "manuscript/poem.md",
                DocumentContent::Verse("first\r\n".into()),
                "fixture",
            )
            .unwrap();
        drop(store);
        let mut project = TextProject::open(root.path()).unwrap();
        let target = TextTarget::Manuscript(0);
        let baseline = project.read(target).unwrap();
        let text = "  café\r\n\r\n\tsecond  \n";
        project.journal(target, &baseline, text).unwrap();
        drop(project);
        let mut project = TextProject::open(root.path()).unwrap();
        assert_eq!(project.read(target).unwrap(), text);
        project.save(target, &baseline, text).unwrap();
        assert_eq!(project.baseline(target).unwrap(), text);
        assert_eq!(project.entries()[0].kind, DocumentKind::Verse);
        assert_eq!(
            std::fs::read(root.path().join("manuscript/poem.md")).unwrap(),
            text.as_bytes()
        );
    }
    #[test]
    fn crash_draft_recovery_and_checkpoint_share_the_original_store_protocol() {
        let root = tempfile::tempdir().unwrap();
        let mut project = TextProject::open(root.path()).unwrap();
        let target = TextTarget::Manuscript(0);
        let baseline = project.read(target).unwrap();
        let path = project.entries()[0].relative_path.clone();
        let text = "café\r\n\t日本語\n";
        let counts = project.store.counts().unwrap();
        project.journal(target, &baseline, text).unwrap();
        assert_eq!(project.store.counts().unwrap(), counts);
        assert_eq!(project.store.read_document(&path).unwrap().text, baseline);
        drop(project); // Simulated process loss before a semantic checkpoint.
        let mut project = TextProject::open(root.path()).unwrap();
        assert_eq!(project.read(target).unwrap(), text);
        assert_eq!(project.baseline(target).unwrap(), baseline);
        assert!(project.has_pending_writes(target));
        project
            .save(target, &baseline, "a further human edit")
            .unwrap();
        assert!(!project.has_pending_writes(target));
        assert!(project.store.load_transient_draft(&path).unwrap().is_none());
        assert_eq!(project.baseline(target).unwrap(), "a further human edit");
        assert_eq!(
            project.store.read_document(&path).unwrap().text,
            "a further human edit"
        );
    }

    #[test]
    fn reverting_to_baseline_clears_the_exact_draft_without_a_new_revision() {
        let root = tempfile::tempdir().unwrap();
        let mut project = TextProject::open(root.path()).unwrap();
        let target = TextTarget::Manuscript(0);
        let baseline = project.read(target).unwrap();
        project
            .journal(target, &baseline, "temporary edit")
            .unwrap();
        let counts = project.store.counts().unwrap();
        project.save(target, &baseline, &baseline).unwrap();
        assert!(!project.has_pending_writes(target));
        assert_eq!(project.store.counts().unwrap(), counts);
        assert_eq!(project.read(target).unwrap(), baseline);
    }

    #[test]
    fn stale_recovery_and_external_conflicts_preserve_both_versions() {
        let root = tempfile::tempdir().unwrap();
        let mut project = TextProject::open(root.path()).unwrap();
        let target = TextTarget::Manuscript(0);
        let baseline = project.read(target).unwrap();
        let path = project.entries()[0].relative_path.clone();
        project.journal(target, &baseline, "recovered").unwrap();
        let draft = project.store.load_transient_draft(&path).unwrap().unwrap();
        project
            .store
            .save_document(
                &path,
                DocumentContent::Prose("new source".into()),
                "fixture",
            )
            .unwrap();
        assert!(matches!(
            project.read(target),
            Err(SessionError::StaleDraft)
        ));
        assert_eq!(
            project.store.load_transient_draft(&path).unwrap().unwrap(),
            draft
        );
        assert_eq!(
            project.store.read_document(&path).unwrap().text,
            "new source"
        );
    }
    #[test]
    fn a_new_revision_with_identical_text_invalidates_an_older_editor() {
        let root = tempfile::tempdir().unwrap();
        let mut project = TextProject::open(root.path()).unwrap();
        let target = TextTarget::Manuscript(0);
        let baseline = project.read(target).unwrap();
        let path = project.entries()[0].relative_path.clone();
        project
            .store
            .save_document(
                &path,
                DocumentContent::Prose(baseline.clone()),
                "a different authoritative operation",
            )
            .unwrap();
        assert!(project.save(target, &baseline, "stale writer").is_err());
        assert_eq!(project.store.read_document(&path).unwrap().text, baseline);
    }
    #[test]
    fn store_conflict_and_kind_rules_are_independent_of_the_view() {
        let root = tempfile::tempdir().unwrap();
        let (mut store, _) = ProjectStore::initialize(root.path(), "Verse").unwrap();
        store
            .create_document_if_absent(
                "manuscript/poem.md",
                DocumentContent::Verse("first\n".into()),
                "fixture",
            )
            .unwrap();
        drop(store);
        let mut project = TextProject::open(root.path()).unwrap();
        let target = TextTarget::Manuscript(0);
        project.read(target).unwrap();
        project.save(target, "first\n", "second\n").unwrap();
        assert_eq!(project.entries()[0].kind, DocumentKind::Verse);
        assert!(matches!(
            project.save(target, "first\n", "stale\n"),
            Err(SessionError::Conflict)
        ));
        assert_eq!(project.read(target).unwrap(), "second\n");
        let path = root.path().join(&project.entries()[0].relative_path);
        std::fs::write(&path, "outside\n").unwrap();
        assert!(project.save(target, "second\n", "overwrite\n").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "outside\n");
    }
}
