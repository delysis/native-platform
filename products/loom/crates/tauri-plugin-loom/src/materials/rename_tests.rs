//! Native-owner regressions. These exercise real projects, registrations,
//! immutable evidence and drafts, not a reimplementation of the rename logic.
use super::*;
use loom_document::DocumentContent;

fn project() -> (tempfile::TempDir, ProjectStore) {
    let temp = tempfile::tempdir().unwrap();
    let (store, _) = ProjectStore::initialize(temp.path().join("Writing"), "Writing").unwrap();
    (temp, store)
}

fn source(store: &ProjectStore, text: &str, name: &str) -> MaterialEntry {
    let path = store.root().join("rename-source.txt");
    fs::write(&path, text).unwrap();
    let imported = context_attachments::import_path(store.root(), &path).unwrap();
    let bound = bind_attachment(store, &imported.id, Some(name)).unwrap();
    observed_entry(store, &bound.id).unwrap()
}

fn rename(store: &ProjectStore, material: &MaterialEntry, name: &str) -> Result<MaterialEntry> {
    change_metadata(
        store,
        &material.id,
        material.metadata_revision.as_deref().unwrap(),
        MetadataChange::Rename(name),
    )?
    .ok_or_else(|| invalid("expected renamed material"))
}

fn bindings_path(store: &ProjectStore) -> PathBuf {
    store.root().join(".loom/materials/bindings.json")
}

#[test]
fn display_rename_preserves_source_evidence_full_id_links_and_exact_schema() {
    let (_temp, store) = project();
    let original = source(&store, "\u{feff}café 🦉\r\n  exact bytes\t\n", "Research");
    let read_before = read(&store, &original.id).unwrap();
    let evidence = &read_before.evidence[0];
    let evidence_path = store
        .root()
        .join(".loom/materials/evidence")
        .join(format!("{}.json", evidence.id));
    let evidence_bytes = fs::read(&evidence_path).unwrap();
    let source_path = context_attachments::original_path(
        store.root(),
        original.attachment_id.as_deref().unwrap(),
    )
    .unwrap();
    let source_bytes = fs::read(&source_path).unwrap();
    let before: Value = serde_json::from_slice(&fs::read(bindings_path(&store)).unwrap()).unwrap();
    let name = "  renamed / café 🦉  ";
    let changed = rename(&store, &original, name).unwrap();
    assert_eq!(changed.name, name);
    assert_eq!(changed.id, original.id);
    assert_eq!(changed.attachment_id, original.attachment_id);
    assert_eq!(changed.workspace_path, original.workspace_path);
    assert_eq!(changed.reference, original.reference);
    assert_eq!(changed.pinned, original.pinned);
    assert_ne!(changed.metadata_revision, original.metadata_revision);
    let full_id: String = serde_json::from_str(&original.reference[1..]).unwrap();
    assert_eq!(full_id, original.id);
    assert_eq!(resolve(&store, &full_id).unwrap().id, original.id);
    assert_eq!(fs::read(&source_path).unwrap(), source_bytes);
    assert_eq!(fs::read(&evidence_path).unwrap(), evidence_bytes);
    assert_eq!(
        read_evidence(&store, &original.id, &evidence.id)
            .unwrap()
            .text,
        evidence.text
    );
    assert_eq!(read(&store, &original.id).unwrap().text, read_before.text);
    let after: Value = serde_json::from_slice(&fs::read(bindings_path(&store)).unwrap()).unwrap();
    let mut expected = before;
    expected["items"][0]["name"] = Value::String(name.into());
    assert_eq!(
        after, expected,
        "only the existing display-name field may change"
    );
}

#[test]
fn duplicate_names_and_independent_placements_never_merge_authority() {
    let (_temp, store) = project();
    let original = source(&store, "one retained original", "Original");
    let attachment = original.attachment_id.as_deref().unwrap();
    let first = bind_workspace_attachment(&store, attachment, "Notes/source.txt").unwrap();
    let second = bind_workspace_attachment(&store, attachment, "Drafts/source.txt").unwrap();
    let first = observed_entry(&store, &first.id).unwrap();
    rename(&store, &first, "Shared label").unwrap();
    let second = observed_entry(&store, &second.id).unwrap();
    rename(&store, &second, "Shared label").unwrap();
    assert!(matches!(
        resolve(&store, "Shared label"),
        Err(MaterialError::Ambiguous(_))
    ));
    assert_eq!(resolve(&store, "Notes/source.txt").unwrap().id, first.id);
    assert_eq!(resolve(&store, "Drafts/source.txt").unwrap().id, second.id);
    assert_ne!(first.id, second.id);
    assert_eq!(resolve(&store, &original.id).unwrap().name, "Original");
    assert_eq!(
        bind_workspace_attachment(&store, attachment, "Notes/source.txt")
            .unwrap()
            .name,
        "Shared label"
    );
}

#[test]
fn stale_rename_pin_and_remove_cannot_mutate_a_new_metadata_generation() {
    let (_temp, store) = project();
    let original = source(&store, "original", "Old");
    let changed = rename(&store, &original, "New").unwrap();
    let bytes = fs::read(bindings_path(&store)).unwrap();
    for action in [
        MetadataChange::Rename("overwritten"),
        MetadataChange::Pin(true),
        MetadataChange::Remove,
    ] {
        assert!(
            change_metadata(
                &store,
                &original.id,
                original.metadata_revision.as_deref().unwrap(),
                action
            )
            .is_err()
        );
        assert_eq!(fs::read(bindings_path(&store)).unwrap(), bytes);
    }
    let pinned = change_metadata(
        &store,
        &changed.id,
        changed.metadata_revision.as_deref().unwrap(),
        MetadataChange::Pin(true),
    )
    .unwrap()
    .unwrap();
    assert!(pinned.pinned);
    assert_eq!(pinned.name, "New");
}

#[test]
fn removal_and_readding_identical_source_cannot_revive_the_old_target() {
    let (_temp, store) = project();
    let original = source(&store, "identical bytes", "Original");
    change_metadata(
        &store,
        &original.id,
        original.metadata_revision.as_deref().unwrap(),
        MetadataChange::Remove,
    )
    .unwrap();
    let replacement = source(&store, "identical bytes", "Replacement");
    assert_eq!(
        replacement.id, original.id,
        "stable source ID is not a mutable row lease"
    );
    assert_ne!(replacement.metadata_revision, original.metadata_revision);
    assert!(rename(&store, &original, "Stale edit").is_err());
    assert_eq!(
        resolve(&store, &replacement.id).unwrap().name,
        "Replacement"
    );
}

#[test]
fn malformed_inputs_and_current_schema_rejection_do_not_write_or_repair_state() {
    let (_temp, store) = project();
    let original = source(&store, "original", "Original");
    let path = bindings_path(&store);
    let original_bytes = fs::read(&path).unwrap();
    for name in [
        String::new(),
        " \t".to_owned(),
        "line\nbreak".to_owned(),
        "\0".to_owned(),
        "界".repeat(171),
    ] {
        assert!(rename(&store, &original, &name).is_err());
        assert_eq!(fs::read(&path).unwrap(), original_bytes);
    }
    for invalid_id in ["../bindings.json", "material-a", "/tmp/source"] {
        assert!(
            change_metadata(
                &store,
                invalid_id,
                original.metadata_revision.as_deref().unwrap(),
                MetadataChange::Remove
            )
            .is_err()
        );
    }
    for revision in ["", "../bindings.json", &"A".repeat(64)] {
        assert!(
            change_metadata(
                &store,
                &original.id,
                revision,
                MetadataChange::Rename("New")
            )
            .is_err()
        );
    }
    let current: Value = serde_json::from_slice(&original_bytes).unwrap();
    let mut invalid_cases = Vec::new();
    let mut old_schema = current.clone();
    old_schema["schema"] = Value::String("loom.materials.obsolete".into());
    invalid_cases.push(old_schema);
    let mut unknown = current.clone();
    unknown["aliases"] = json!([]);
    invalid_cases.push(unknown);
    let mut forged = current.clone();
    forged["items"][0]["id"] = Value::String(format!("material-{}", "0".repeat(64)));
    invalid_cases.push(forged);
    let mut protected = current;
    protected["items"][0]["retention"] = Value::String("protected".into());
    invalid_cases.push(protected);
    for invalid_case in invalid_cases {
        let bytes = serde_json::to_vec(&invalid_case).unwrap();
        fs::write(&path, &bytes).unwrap();
        let observed = metadata::Snapshot::open(store.root()).unwrap().unwrap();
        assert!(list(&store).is_err());
        assert!(
            change_metadata(
                &store,
                &original.id,
                &observed.revision,
                MetadataChange::Rename("New")
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn dirty_document_recovery_and_committed_metadata_survive_store_reopen() {
    let (_temp, mut store) = project();
    let text = "committed café 🦉\r\n";
    store
        .create_document_if_absent(
            "Notes/Draft.md",
            DocumentContent::Prose(text.into()),
            "human fixture",
        )
        .unwrap();
    let before = store.read_document("Notes/Draft.md").unwrap();
    let dirty = "uncommitted café 🦉\r\n  keep trailing spaces  ";
    let draft = store
        .upsert_transient_draft(
            "Notes/Draft.md",
            before.revision_id,
            0,
            DocumentContent::Prose(dirty.into()),
        )
        .unwrap()
        .draft;
    let original = source(&store, "source", "Original");
    let root = store.root().to_path_buf();
    let visible = fs::read(root.join("Notes/Draft.md")).unwrap();
    rename(&store, &original, "Renamed").unwrap();
    assert_eq!(store.read_document("Notes/Draft.md").unwrap(), before);
    assert_eq!(
        store.load_transient_draft("Notes/Draft.md").unwrap(),
        Some(draft.clone())
    );
    assert_eq!(fs::read(root.join("Notes/Draft.md")).unwrap(), visible);
    drop(store);
    let store = ProjectStore::open(&root).unwrap();
    assert_eq!(store.read_document("Notes/Draft.md").unwrap(), before);
    assert_eq!(
        store.load_transient_draft("Notes/Draft.md").unwrap(),
        Some(draft)
    );
    assert_eq!(resolve(&store, &original.id).unwrap().name, "Renamed");
    assert!(rename(&store, &original, "stale retry after restart").is_err());
    assert_eq!(fs::read(root.join("Notes/Draft.md")).unwrap(), visible);
}
