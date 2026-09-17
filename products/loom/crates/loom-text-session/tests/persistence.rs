use loom_document::DocumentContent;
use loom_store::{ProjectStore, StoreError, VisibleProjectionState};
use loom_text_session::persistence::{
    self, Checkpoint, DocumentAddress, DraftWrite, PersistenceError,
};
use loom_types::{CommandId, DocumentId};

fn fixture() -> (tempfile::TempDir, ProjectStore, loom_store::LoadedDocument) {
    let root = tempfile::tempdir().unwrap();
    let (mut store, _) = ProjectStore::initialize(root.path(), "Shared persistence").unwrap();
    store
        .save_document(
            "manuscript/one.md",
            DocumentContent::Prose("source".into()),
            "fixture",
        )
        .unwrap();
    let source = store.read_document("manuscript/one.md").unwrap();
    (root, store, source)
}

fn address(source: &loom_store::LoadedDocument) -> DocumentAddress<'_> {
    DocumentAddress {
        document_id: source.document_id,
        relative_path: &source.relative_path,
    }
}

#[test]
fn exact_draft_checkpoint_can_cross_adapters_and_replay_after_a_lost_reply() {
    let (_root, mut store, source) = fixture();
    let counts = store.counts().unwrap();
    let text = "café\r\n\t日本語\n";
    let canonical = text;
    let written = persistence::write_draft(
        &mut store,
        DraftWrite {
            document: address(&source),
            source_revision_id: source.revision_id,
            expected_version: 0,
            content: DocumentContent::Prose(text.into()),
        },
    )
    .unwrap();
    assert_eq!(store.counts().unwrap(), counts);
    assert_eq!(
        store.read_document(&source.relative_path).unwrap().text,
        source.text
    );
    assert_eq!(
        store
            .load_transient_draft(&source.relative_path)
            .unwrap()
            .unwrap()
            .text,
        canonical
    );
    let command_id = CommandId::new();
    let request = || Checkpoint {
        document: address(&source),
        source_revision_id: source.revision_id,
        visible_blob_id: source.blob_id,
        command_id,
        draft_version: Some(0),
        content: DocumentContent::Prose(text.into()),
    };
    // A second adapter knows only the original version because the draft reply
    // was lost. The shared policy consumes only that exact successor draft.
    let saved = persistence::checkpoint(&mut store, request()).unwrap();
    assert_eq!(saved.visible_projection, VisibleProjectionState::Applied);
    assert!(!saved.replayed);
    assert!(written.draft.version > 0);
    assert!(
        store
            .load_transient_draft(&source.relative_path)
            .unwrap()
            .is_none()
    );
    let counts = store.counts().unwrap();
    let replay = persistence::checkpoint(&mut store, request()).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.save, saved.save);
    assert_eq!(store.counts().unwrap(), counts);
    assert_eq!(
        store.read_document(&source.relative_path).unwrap().text,
        canonical
    );
}

#[test]
fn draft_retry_and_clear_never_replace_a_newer_different_draft() {
    let (_root, mut store, source) = fixture();
    let write = |store: &mut ProjectStore, expected_version, text: &str| {
        persistence::write_draft(
            store,
            DraftWrite {
                document: address(&source),
                source_revision_id: source.revision_id,
                expected_version,
                content: DocumentContent::Prose(text.into()),
            },
        )
    };
    let first = write(&mut store, 0, "first").unwrap();
    let current = write(&mut store, first.draft.version, "current").unwrap();
    let replay = write(&mut store, 0, "current").unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.draft, current.draft);
    assert!(matches!(
        write(&mut store, 0, "stale"),
        Err(PersistenceError::Store(
            StoreError::TransientDraftVersionConflict { .. }
        ))
    ));
    assert!(matches!(
        persistence::clear_draft(&mut store, address(&source), first.draft.version),
        Err(PersistenceError::Store(
            StoreError::TransientDraftVersionConflict { .. }
        ))
    ));
    assert_eq!(
        store
            .load_transient_draft(&source.relative_path)
            .unwrap()
            .unwrap(),
        current.draft
    );
    assert!(persistence::clear_draft(&mut store, address(&source), current.draft.version).unwrap());
    assert!(persistence::clear_draft(&mut store, address(&source), current.draft.version).unwrap());
}

#[test]
fn all_writes_bind_the_document_identity_and_checkpoint_preserves_external_bytes() {
    let (root, mut store, source) = fixture();
    let wrong = DocumentAddress {
        document_id: DocumentId::new(),
        relative_path: &source.relative_path,
    };
    let counts = store.counts().unwrap();
    assert!(matches!(
        persistence::write_draft(
            &mut store,
            DraftWrite {
                document: wrong,
                source_revision_id: source.revision_id,
                expected_version: 0,
                content: DocumentContent::Prose("wrong".into()),
            }
        ),
        Err(PersistenceError::IdentityMismatch)
    ));
    assert!(matches!(
        persistence::clear_draft(&mut store, wrong, 0),
        Err(PersistenceError::IdentityMismatch)
    ));
    let command_id = CommandId::new();
    assert!(matches!(
        persistence::checkpoint(
            &mut store,
            Checkpoint {
                document: wrong,
                source_revision_id: source.revision_id,
                visible_blob_id: source.blob_id,
                command_id,
                draft_version: None,
                content: DocumentContent::Prose("wrong".into()),
            }
        ),
        Err(PersistenceError::IdentityMismatch)
    ));
    assert_eq!(store.counts().unwrap(), counts);
    std::fs::write(root.path().join(&source.relative_path), "external").unwrap();
    assert!(
        persistence::checkpoint(
            &mut store,
            Checkpoint {
                document: address(&source),
                source_revision_id: source.revision_id,
                visible_blob_id: source.blob_id,
                command_id,
                draft_version: None,
                content: DocumentContent::Prose("overwrite".into()),
            }
        )
        .is_err()
    );
    assert!(store.load_receipt(command_id).unwrap().is_none());
    assert_eq!(
        std::fs::read_to_string(root.path().join(&source.relative_path)).unwrap(),
        "external"
    );
    assert_eq!(store.counts().unwrap(), counts);
}
