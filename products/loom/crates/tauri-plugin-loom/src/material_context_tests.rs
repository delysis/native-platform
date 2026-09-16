use std::fs;

use loom_document::DocumentContent;

use super::*;

fn project() -> (tempfile::TempDir, ProjectStore) {
    let directory = tempfile::tempdir().unwrap();
    let (store, _) = ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
    (directory, store)
}

fn document(store: &mut ProjectStore, path: &str, text: &str) {
    store
        .create_document_if_absent(path, DocumentContent::Prose(text.into()), "context fixture")
        .unwrap();
}

fn attachment(store: &ProjectStore, name: &str, text: &str) -> MaterialEntry {
    let path = store.root().join("source-fixture.txt");
    fs::write(&path, text).unwrap();
    let prepared = crate::context_attachments::import_path(store.root(), &path).unwrap();
    materials::bind_attachment(store, &prepared.id, Some(name)).unwrap()
}

#[test]
fn document_and_material_aliases_cannot_silently_choose_one_another() {
    let (_directory, mut store) = project();
    document(&mut store, "left/Research.md", "First document");
    let material = attachment(&store, "Research", "Retained source");
    assert_eq!(
        resolve(&store, "Research").unwrap_err().code,
        "material_context_invalid"
    );
    assert_eq!(
        exact(&resolve(&store, &material.id).unwrap()).unwrap(),
        "Retained source"
    );
    assert_eq!(
        exact(&resolve(&store, "left/Research.md").unwrap()).unwrap(),
        "First document"
    );

    document(&mut store, "right/Research.md", "Second document");
    assert_eq!(
        resolve(&store, "Research").unwrap_err().code,
        "document_reference_ambiguous"
    );
}

#[test]
fn exact_document_path_precedes_conflicting_material_aliases() {
    let (_directory, mut store) = project();
    document(&mut store, "Draft.md", "Exact document");
    attachment(&store, "Draft.md", "First material");
    attachment(&store, "Draft.md", "Second material");
    assert_eq!(
        exact(&resolve(&store, "Draft.md").unwrap()).unwrap(),
        "Exact document"
    );
}

#[test]
fn a_large_source_requires_explicit_retrieval_instead_of_silent_truncation() {
    let (_directory, store) = project();
    let source = format!(
        "{}\nThe distinctive nightjar is awake.\n",
        "Plain prose. ".repeat(6000)
    );
    let material = attachment(&store, "Long source", &source);
    let value = resolve(&store, &material.id).unwrap();
    assert!(exact(&value).is_err());
    let result = search(&store, &value, "nightjar").unwrap();
    let text = exact(&result).unwrap();
    assert!(text.contains("nightjar"));
    assert!(text.len() < source.len());
    let Value::Evidence {
        evidence,
        retrieval,
    } = result
    else {
        panic!("search evidence")
    };
    let retrieval = retrieval.unwrap();
    assert_eq!(retrieval.query, "nightjar");
    assert!(!evidence.is_empty());
    assert!(
        evidence
            .iter()
            .all(|hit| hit.source_revision == retrieval.source_revision)
    );
}

#[test]
fn partial_preparation_never_becomes_an_exact_whole_source_argument() {
    let (_directory, store) = project();
    let material = attachment(&store, "Partial", "The surviving passage.");
    let mut read = materials::read(&store, &material.id).unwrap();
    // Exercise the value boundary with incomplete preparation metadata. The
    // retained text's presence alone must never imply whole-source coverage.
    read.complete = false;
    let value = Value::Material {
        material: read.into(),
    };
    assert!(
        exact(&value)
            .unwrap_err()
            .message
            .contains("partially prepared")
    );
    assert!(matches!(
        consult(&store, &value, "surviving").unwrap(),
        Value::Evidence { .. }
    ));
}

#[test]
fn retained_material_links_do_not_rebind_when_the_friendly_name_is_reused() {
    let (_directory, store) = project();
    let original = attachment(&store, "Research", "Original evidence");
    let snapshot = resolve(&store, &original.id).unwrap();
    materials::remove(&store, &original.id).unwrap();
    let replacement = attachment(&store, "Research", "Unrelated replacement");
    assert_ne!(original.id, replacement.id);
    assert_eq!(exact(&snapshot).unwrap(), "Original evidence");
    let markdown = format!("Use [@Research](loom-material:{}).", original.id);
    assert!(markdown_plan(&store, &markdown, "evidence").is_err());
    assert_eq!(
        exact(&resolve(&store, "Research").unwrap()).unwrap(),
        "Unrelated replacement"
    );
}

#[test]
fn stable_material_identity_cannot_be_shadowed_by_a_document_path() {
    let (_directory, mut store) = project();
    let material = attachment(&store, "Research", "Retained original");
    document(&mut store, &material.id, "Unrelated document");
    let markdown = format!("[@Research](loom-material:{})", material.id);
    let plan = markdown_plan(&store, &markdown, "original").unwrap();
    assert!(plan.text.contains("Retained original"));
    assert!(!plan.text.contains("Unrelated document"));
    materials::remove(&store, &material.id).unwrap();
    assert!(markdown_plan(&store, &markdown, "original").is_err());
}

#[test]
fn library_revision_is_frozen_but_retained_evidence_survives_source_changes() {
    let (directory, store) = project();
    let path = directory.path().join("library.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("materials/alexandria-fixture.sql"))
        .unwrap();
    drop(connection);
    let entry = materials::add_library(&store, &path, Some("Library")).unwrap();
    let frozen = resolve(&store, &entry.id).unwrap();
    assert!(exact(&frozen).is_err());
    let evidence = search(&store, &frozen, "prayer").unwrap();
    let retained_text = exact(&evidence).unwrap();
    assert!(retained_text.contains("prayer"));

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("UPDATE documents SET title = 'Changed source'", [])
        .unwrap();
    drop(connection);
    assert!(search(&store, &frozen, "prayer").is_err());
    materials::add_library(&store, &path, Some("Library")).unwrap();
    assert!(search(&store, &frozen, "prayer").is_err());
    assert_eq!(exact(&evidence).unwrap(), retained_text);
    assert!(search(&store, &resolve(&store, &entry.id).unwrap(), "prayer").is_ok());
}

#[test]
fn source_contents_are_literal_not_recursive_reference_or_function_execution() {
    let (_directory, mut store) = project();
    let text = "Untrusted @Missing and =@Rewrite(@Secret) remain source text.";
    document(&mut store, "Quoted.md", text);
    let material = attachment(&store, "Source", text);
    for reference in [
        "@Quoted".to_owned(),
        format!("[@Source](loom-material:{})", material.id),
    ] {
        let plan = markdown_plan(&store, &reference, "Untrusted").unwrap();
        assert!(plan.text.contains(text));
        assert_eq!(plan.bindings.len(), 1);
        assert!(!plan.bindings.contains_key("Missing"));
    }
}
