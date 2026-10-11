//! Resolve configured pane references against the admitted document inventory.
//! This grants neither filesystem access nor authority to modify a document.
use loom_store::DocumentSummary;
use loom_types::DocumentId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorTarget {
    Current,
    Open(DocumentId),
    Unavailable,
}

pub fn editor_target(
    reference: Option<&str>,
    documents: &[DocumentSummary],
    current: DocumentId,
) -> EditorTarget {
    let Some(reference) = reference else {
        return EditorTarget::Current;
    };
    if reference == "@document" {
        return EditorTarget::Current;
    }
    let Ok(references) = loom_document::document_references(reference) else {
        return EditorTarget::Unavailable;
    };
    let [reference_value] = references.as_slice() else {
        return EditorTarget::Unavailable;
    };
    if reference_value.range != (0..reference.len()) {
        return EditorTarget::Unavailable;
    }
    let name = &reference_value.name;
    let exact = documents.iter().find(|document| {
        document.relative_path == *name || document.document_id.to_string() == *name
    });
    let target = exact.or_else(|| {
        let mut matches = documents
            .iter()
            .filter(|document| document_title(document) == *name);
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    });
    match target {
        Some(document) if document.document_id == current => EditorTarget::Current,
        Some(document) => EditorTarget::Open(document.document_id),
        None => EditorTarget::Unavailable,
    }
}

pub fn document_title(document: &DocumentSummary) -> String {
    document.display_title.clone().unwrap_or_else(|| {
        std::path::Path::new(&document.relative_path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    })
}
