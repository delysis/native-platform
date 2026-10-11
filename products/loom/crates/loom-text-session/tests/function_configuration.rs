use loom_config::{CONFIG_FILE, FunctionFormat};
use loom_document::DocumentContent;
use loom_store::ProjectStore;
use loom_text_session::configuration::{capture, function_recipe};
use loom_types::BlobId;

#[test]
fn authored_mine_functions_take_precedence_and_freeze_exact_bytes_without_registration() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, _) = ProjectStore::initialize(root.path(), "Functions").unwrap();
    store
        .create_document_if_absent(
            ".loom.md",
            DocumentContent::Prose("```loom-workspace\n[functions]\nformat='model'\n```\n".into()),
            "fixture",
        )
        .unwrap();
    let source = "# café 日本語\r\n[workspace.functions]\r\nformat = 'raw'\r\n";
    std::fs::write(root.path().join(CONFIG_FILE), source).unwrap();
    let counts = store.counts().unwrap();
    let frozen = function_recipe(&mut store).unwrap();
    assert_eq!(frozen.format, FunctionFormat::Raw);
    let exact = frozen.configuration.as_ref().unwrap();
    assert_eq!(exact.path, CONFIG_FILE);
    assert_eq!(exact.text, source);
    assert_eq!(exact.blob_id, BlobId::digest(source.as_bytes()));
    assert!(exact.document.is_none());
    assert_eq!(store.counts().unwrap(), counts);
    assert_eq!(
        std::fs::read(root.path().join(CONFIG_FILE)).unwrap(),
        source.as_bytes()
    );
    std::fs::write(
        root.path().join(CONFIG_FILE),
        source.replace("'raw'", "'model'"),
    )
    .unwrap();
    assert_eq!(
        function_recipe(&mut store).unwrap().format,
        FunctionFormat::Model
    );
    assert_eq!(frozen.format, FunctionFormat::Raw);
    assert_eq!(exact.text, source);
    assert_eq!(store.counts().unwrap(), counts);
    std::fs::write(
        root.path().join(CONFIG_FILE),
        source.replace("'raw'", "'guess'"),
    )
    .unwrap();
    assert!(function_recipe(&mut store).is_err());
    assert!(capture(&mut store).unwrap().snapshot.error.is_some());
    assert_eq!(store.counts().unwrap(), counts);
}

#[test]
fn registered_function_configuration_retains_revision_evidence_across_external_changes() {
    for (path, source) in [
        (
            ".loom.md",
            "```loom-workspace\n[functions]\nformat='raw'\n```\n",
        ),
        (CONFIG_FILE, "[workspace.functions]\nformat='raw'\n"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut store, _) = ProjectStore::initialize(root.path(), "Functions").unwrap();
        store
            .create_document_if_absent(path, DocumentContent::Prose(source.into()), "fixture")
            .unwrap();
        let frozen = function_recipe(&mut store).unwrap();
        let original = frozen.configuration.as_ref().unwrap();
        let revision = original.document.as_ref().unwrap().revision_id;
        assert_eq!(frozen.format, FunctionFormat::Raw);
        assert_eq!(
            store.read_blob(original.blob_id).unwrap(),
            source.as_bytes()
        );
        std::fs::write(root.path().join(path), source.replace("'raw'", "'model'")).unwrap();
        let changed = function_recipe(&mut store).unwrap();
        assert_eq!(changed.format, FunctionFormat::Model);
        assert_ne!(
            changed.configuration.unwrap().document.unwrap().revision_id,
            revision
        );
        assert_eq!(original.text, source);
        assert_eq!(
            store.read_blob(original.blob_id).unwrap(),
            source.as_bytes()
        );
        std::fs::write(root.path().join(path), source.replace("'raw'", "'guess'")).unwrap();
        assert!(function_recipe(&mut store).is_err());
        assert!(capture(&mut store).unwrap().snapshot.error.is_some());
    }
}

#[test]
fn absent_function_configuration_uses_model_framing_without_creating_files_or_documents() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, _) = ProjectStore::initialize(root.path(), "Functions").unwrap();
    let counts = store.counts().unwrap();
    let recipe = function_recipe(&mut store).unwrap();
    assert_eq!(recipe.format, FunctionFormat::Model);
    assert!(recipe.configuration.is_none());
    assert_eq!(store.counts().unwrap(), counts);
    assert!(!root.path().join(CONFIG_FILE).exists());
    assert!(!root.path().join(".loom.md").exists());
}
