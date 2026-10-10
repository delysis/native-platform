use loom_store::DocumentSummary;
use loom_text_session::workspace::{EditorTarget, editor_target};
use loom_types::{DocumentId, DocumentKind};

fn document(path: &str, title: Option<&str>) -> DocumentSummary {
    DocumentSummary {
        document_id: DocumentId::new(),
        relative_path: path.into(),
        display_title: title.map(str::to_owned),
        kind: DocumentKind::Prose,
        active_revision_id: None,
    }
}

#[test]
fn references_use_admitted_identity_path_and_unique_title() {
    let documents = [
        document("Draft.md", Some("Draft")),
        document("Notes/Reading notes.md", Some("Reading")),
        document("Reading.md", Some("Reading")),
    ];
    let current = documents[0].document_id;
    for reference in [None, Some("@document"), Some("@Draft"), Some("@Draft.md")] {
        assert_eq!(
            editor_target(reference, &documents, current),
            EditorTarget::Current
        );
    }
    assert_eq!(
        editor_target(Some("@\"Notes/Reading notes.md\""), &documents, current),
        EditorTarget::Open(documents[1].document_id)
    );
    assert_eq!(
        editor_target(
            Some(&format!("@{}", documents[2].document_id)),
            &documents,
            current
        ),
        EditorTarget::Open(documents[2].document_id)
    );
    for reference in [
        "@Reading",
        "@Missing",
        "@Draft @Reading",
        "prefix @Draft",
        "@\"bad",
    ] {
        assert_eq!(
            editor_target(Some(reference), &documents, current),
            EditorTarget::Unavailable
        );
    }
}

#[test]
fn exact_path_precedes_title_and_absent_titles_use_filename() {
    let documents = [
        document("Draft.md", None),
        document("Other.md", Some("Draft.md")),
    ];
    assert_eq!(
        editor_target(Some("@Draft.md"), &documents, documents[1].document_id),
        EditorTarget::Open(documents[0].document_id)
    );
    assert_eq!(
        editor_target(Some("@Draft"), &documents, documents[1].document_id),
        EditorTarget::Open(documents[0].document_id)
    );
}
