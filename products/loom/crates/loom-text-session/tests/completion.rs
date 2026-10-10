use loom_document::DocumentContent;
use loom_store::{LoadedDocument, ProjectStore};
use loom_text_session::{
    completion::{self, CaptureError, SourceIdentity, SourceRequest},
    persistence::DocumentAddress,
};
use loom_types::{BlobId, DocumentId, RevisionId};

fn fixture() -> (tempfile::TempDir, ProjectStore, LoadedDocument) {
    let root = tempfile::tempdir().unwrap();
    let (mut store, _) = ProjectStore::initialize(root.path(), "Completion boundary").unwrap();
    store
        .save_document(
            "source.md",
            DocumentContent::Prose("# α\r\n\r\n**café 🌍**\n".into()),
            "fixture",
        )
        .unwrap();
    let document = store.read_document("source.md").unwrap();
    (root, store, document)
}

fn request(document: &LoadedDocument, cursor_byte: u64) -> SourceRequest<'_> {
    SourceRequest {
        document: DocumentAddress {
            document_id: document.document_id,
            relative_path: &document.relative_path,
        },
        source_revision_id: document.revision_id,
        visible_blob_id: document.blob_id,
        cursor_byte,
    }
}

#[test]
fn captured_prefix_retains_literal_markdown_and_line_endings_without_writes() {
    let (root, store, document) = fixture();
    let counts = store.counts().unwrap();
    for cursor in 0..=document.text.len() {
        let result =
            completion::capture(&store, request(&document, u64::try_from(cursor).unwrap()));
        if document.text.is_char_boundary(cursor) {
            let captured = result.unwrap();
            assert_eq!(captured.project_id(), store.manifest().project_id);
            assert_eq!(captured.identity(), SourceIdentity::of(&document));
            assert_eq!(captured.document().text, document.text);
            assert_eq!(captured.cursor(), cursor);
            assert_eq!(captured.prefix(), &document.text[..cursor]);
            let debug = format!("{captured:?}");
            assert!(!debug.contains("café"));
            assert!(!debug.contains("source.md"));
        } else {
            assert!(matches!(result, Err(CaptureError::CursorBoundary)));
        }
    }
    assert_eq!(store.counts().unwrap(), counts);
    assert_eq!(
        std::fs::read_to_string(root.path().join("source.md")).unwrap(),
        document.text
    );
}

#[test]
fn forged_identity_revision_blob_and_cursor_are_rejected_before_any_write() {
    let (_root, store, document) = fixture();
    let counts = store.counts().unwrap();
    let mut wrong = request(&document, 0);
    wrong.document.document_id = DocumentId::new();
    assert!(matches!(
        completion::capture(&store, wrong),
        Err(CaptureError::DocumentMismatch)
    ));
    let mut wrong = request(&document, 0);
    wrong.source_revision_id = RevisionId::new();
    assert!(matches!(
        completion::capture(&store, wrong),
        Err(CaptureError::RevisionConflict)
    ));
    let mut wrong = request(&document, 0);
    wrong.visible_blob_id = BlobId::digest(b"different source");
    assert!(matches!(
        completion::capture(&store, wrong),
        Err(CaptureError::BlobConflict)
    ));
    assert!(matches!(
        completion::capture(&store, request(&document, u64::MAX)),
        Err(CaptureError::CursorBoundary | CaptureError::CursorOverflow)
    ));
    assert_eq!(store.counts().unwrap(), counts);
}

#[test]
fn a_capture_cannot_be_reused_to_admit_changed_source_or_another_document() {
    let (root, mut store, document) = fixture();
    let captured = completion::capture(&store, request(&document, 0)).unwrap();
    store
        .save_document(
            "other.md",
            DocumentContent::Prose(document.text.clone()),
            "fixture",
        )
        .unwrap();
    let mut other = request(&document, 0);
    other.document.relative_path = "other.md";
    assert!(matches!(
        completion::capture(&store, other),
        Err(CaptureError::DocumentMismatch)
    ));
    store
        .save_document(
            "source.md",
            DocumentContent::Prose("new source".into()),
            "fixture",
        )
        .unwrap();
    let counts = store.counts().unwrap();
    assert!(matches!(
        completion::capture(&store, request(&document, 0)),
        Err(CaptureError::RevisionConflict)
    ));
    assert_eq!(captured.document().text, document.text);
    assert_eq!(store.counts().unwrap(), counts);
    let current = store.read_document("source.md").unwrap();
    std::fs::write(root.path().join("source.md"), "external edit").unwrap();
    assert!(completion::capture(&store, request(&current, 0)).is_err());
    assert_eq!(
        std::fs::read_to_string(root.path().join("source.md")).unwrap(),
        "external edit"
    );
    assert_eq!(store.counts().unwrap(), counts);
}
