//! Component regressions for the real material codec and publication boundary.
//! Keys and injected failures in this module are confined to synthetic tests.
use super::*;
use desktop_vault::ProjectVault;

const KEY: [u8; 32] = [97; 32];
const TEXT: &str = "Private café 雨 🦉.\r\nUnchanged source.\n";

#[test]
fn secured_evidence_retains_plaintext_identity_and_seals_storage() {
    let (_directory, store) = project(true);
    let expected = evidence(TEXT);
    let plaintext = evidence_payload(&expected).unwrap();
    let retained = retain_evidence(&store, expected).unwrap();
    assert_eq!(retained.id, digest(&plaintext));
    let path = evidence_path(&store, &retained.id);
    let stored = fs::read(&path).unwrap();
    assert!(stored.starts_with(b"MINEENC\x01"));
    assert_ne!(stored, plaintext);
    assert!(serde_json::from_slice::<Value>(&stored).is_err());
    let loaded = read_evidence(&store, &retained.material_id, &retained.id).unwrap();
    assert_eq!(loaded.text.as_bytes(), TEXT.as_bytes());
    assert_eq!(evidence_payload(&loaded).unwrap(), plaintext);
    assert!(read_evidence(&store, "another material", &retained.id).is_err());
    assert_eq!(retain_evidence(&store, loaded).unwrap().id, retained.id);
    assert_eq!(fs::read(&path).unwrap(), stored);
}

fn project(secured: bool) -> (tempfile::TempDir, ProjectStore) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Writing");
    let (store, _) = if secured {
        fs::create_dir_all(root.join(".loom")).unwrap();
        let vault = ProjectVault::initialize_with_key(&root, KEY).unwrap();
        ProjectStore::initialize_with_vault(&root, "Private materials", vault).unwrap()
    } else {
        ProjectStore::initialize(&root, "Ordinary materials").unwrap()
    };
    (directory, store)
}

fn evidence(text: &str) -> MaterialEvidence {
    MaterialEvidence {
        complete: true,
        warnings: Vec::new(),
        id: String::new(),
        reference: String::new(),
        material_id: format!("material-{}", digest(b"synthetic source")),
        title: "Private café 雨".into(),
        text: text.into(),
        source_revision: digest(text.as_bytes()),
        text_sha256: digest(text.as_bytes()),
        locator: json!({"kind": "synthetic_exact_source", "start_byte": 0,
            "end_byte": text.len()}),
        source_evidence: None,
    }
}

fn evidence_path(store: &ProjectStore, id: &str) -> PathBuf {
    storage(store)
        .unwrap()
        .join("evidence")
        .join(format!("{id}.json"))
}

fn bindings(store: &ProjectStore) -> Bindings {
    // A binding alone must not grant filesystem authority.
    let source = Source::Library {
        path: store.root().join("Private café 雨.sqlite3"),
    };
    Bindings {
        schema: SCHEMA.into(),
        items: vec![Binding {
            id: binding_id(&source).unwrap(),
            name: "Private café 雨".into(),
            pinned: false,
            source,
            workspace_path: None,
        }],
    }
}

fn files_in(path: &Path) -> std::collections::BTreeSet<std::ffi::OsString> {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect()
}

#[test]
fn ordinary_materials_keep_plaintext_formats_and_reject_changed_evidence() {
    let (_directory, store) = project(false);
    let expected = bindings(&store);
    let plaintext = serde_json::to_vec(&expected).unwrap();
    write_bindings(&store, &expected).unwrap();
    let path = storage(&store).unwrap().join("bindings.json");
    assert_eq!(fs::read(&path).unwrap(), plaintext);
    assert_eq!(
        serde_json::to_vec(&read_bindings(&store).unwrap()).unwrap(),
        plaintext
    );
    assert!(!list(&store).unwrap()[0].available);
    assert!(store.vault().is_none());
    assert!(!store.root().join(".loom/vault.json").exists());
    let retained = retain_evidence(&store, evidence(TEXT)).unwrap();
    let path = evidence_path(&store, &retained.id);
    assert_eq!(
        fs::read(&path).unwrap(),
        evidence_payload(&retained).unwrap()
    );
    assert_eq!(load_evidence(&store, &retained.id).unwrap().text, TEXT);
    let changed = evidence_payload(&evidence("Another source.\r\n")).unwrap();
    fs::write(&path, &changed).unwrap();
    assert!(load_evidence(&store, &retained.id).is_err());
    assert!(retain_evidence(&store, retained).is_err());
    assert_eq!(fs::read(path).unwrap(), changed);
}

#[test]
fn secured_bindings_reject_tamper_rebinding_and_plaintext_before_replacement() {
    let (_directory, store) = project(true);
    let (_foreign_directory, foreign) = project(true);
    let expected = bindings(&store);
    let plaintext = serde_json::to_vec(&expected).unwrap();
    write_bindings(&store, &expected).unwrap();
    let path = storage(&store).unwrap().join("bindings.json");
    let namespace = crate::private_sidecar::namespace(store.root(), &path).unwrap();
    let stored = fs::read(&path).unwrap();
    assert!(stored.starts_with(b"MINEENC\x01"));
    assert!(serde_json::from_slice::<Value>(&stored).is_err());
    let mut tampered = stored.clone();
    *tampered.last_mut().unwrap() ^= 1;
    let codec = crate::private_sidecar::PayloadCodec::open(store.root()).unwrap();
    let rebound = codec
        .seal(".loom/materials/other.json", &plaintext)
        .unwrap();
    let foreign_payload = crate::private_sidecar::PayloadCodec::open(foreign.root())
        .unwrap()
        .seal(&namespace, &plaintext)
        .unwrap();
    assert_eq!(rebound.len(), stored.len());
    assert_eq!(foreign_payload.len(), stored.len());
    for replacement in [tampered, rebound, foreign_payload, plaintext.clone()] {
        assert_eq!(
            serde_json::to_vec(&read_bindings(&store).unwrap()).unwrap(),
            plaintext
        );
        fs::write(&path, &replacement).unwrap();
        let before = files_in(path.parent().unwrap());
        assert!(read_bindings(&store).is_err());
        assert!(write_bindings(&store, &expected).is_err());
        assert_eq!(fs::read(&path).unwrap(), replacement);
        assert_eq!(files_in(path.parent().unwrap()), before);
        fs::write(&path, &stored).unwrap();
    }
}

#[test]
fn secured_evidence_rejects_equal_length_object_substitution_and_downgrade() {
    let (_directory, store) = project(true);
    let (_foreign_directory, foreign) = project(true);
    let retained = retain_evidence(&store, evidence(TEXT)).unwrap();
    let other_text = TEXT.replace("Private", "Another");
    assert_eq!(other_text.len(), TEXT.len());
    let other = retain_evidence(&store, evidence(&other_text)).unwrap();
    assert_ne!(retained.id, other.id);
    let path = evidence_path(&store, &retained.id);
    let stored = fs::read(&path).unwrap();
    // Actual separately published objects, not just a fabricated wrong namespace.
    let rebound = fs::read(evidence_path(&store, &other.id)).unwrap();
    assert_eq!(stored.len(), rebound.len());
    let payload = evidence_payload(&retained).unwrap();
    let namespace = crate::private_sidecar::namespace(store.root(), &path).unwrap();
    let foreign_payload = crate::private_sidecar::PayloadCodec::open(foreign.root())
        .unwrap()
        .seal(&namespace, &payload)
        .unwrap();
    let mut tampered = stored.clone();
    *tampered.last_mut().unwrap() ^= 1;
    for replacement in [tampered, rebound, foreign_payload, payload] {
        assert_eq!(load_evidence(&store, &retained.id).unwrap().text, TEXT);
        fs::write(&path, &replacement).unwrap();
        let before = files_in(path.parent().unwrap());
        assert!(load_evidence(&store, &retained.id).is_err());
        assert!(retain_evidence(&store, retained.clone()).is_err());
        assert_eq!(fs::read(&path).unwrap(), replacement);
        assert_eq!(files_in(path.parent().unwrap()), before);
        assert_eq!(load_evidence(&store, &other.id).unwrap().text, other_text);
        fs::write(&path, &stored).unwrap();
    }
}

#[test]
fn secured_evidence_still_checks_authenticated_plaintext_identity() {
    let (_directory, store) = project(true);
    let retained = retain_evidence(&store, evidence(TEXT)).unwrap();
    let path = evidence_path(&store, &retained.id);
    let namespace = crate::private_sidecar::namespace(store.root(), &path).unwrap();
    let codec = crate::private_sidecar::PayloadCodec::open(store.root()).unwrap();
    let changed = evidence_payload(&evidence(&TEXT.replace("Private", "Another"))).unwrap();
    let wrong_identity = codec.seal(&namespace, &changed).unwrap();
    fs::write(&path, &wrong_identity).unwrap();
    assert!(load_evidence(&store, &retained.id).is_err());
    assert!(retain_evidence(&store, retained).is_err());
    assert_eq!(fs::read(path).unwrap(), wrong_identity);

    let mut invalid_text = evidence(TEXT);
    invalid_text.text_sha256 = digest(b"not the retained text");
    let payload = evidence_payload(&invalid_text).unwrap();
    let id = digest(&payload);
    let path = evidence_path(&store, &id);
    let namespace = crate::private_sidecar::namespace(store.root(), &path).unwrap();
    let stored = codec.seal(&namespace, &payload).unwrap();
    fs::write(&path, &stored).unwrap();
    assert!(load_evidence(&store, &id).is_err());
    assert_eq!(fs::read(path).unwrap(), stored);
}

#[test]
fn secured_import_remove_reopen_and_reimport_preserve_source_and_evidence() {
    let (_directory, store) = project(true);
    let external = tempfile::tempdir().unwrap();
    let source_path = external.path().join("source.txt");
    fs::write(&source_path, TEXT).unwrap();
    let attachment = context_attachments::import_path(store.root(), &source_path).unwrap();
    let entry = bind_attachment(&store, &attachment.id, Some("Private café 雨")).unwrap();
    let material = read(&store, &entry.id).unwrap();
    assert_eq!(material.evidence.len(), 1);
    let retained = &material.evidence[0];
    let expected = evidence_payload(retained).unwrap();
    assert_eq!(retained.id, digest(&expected));
    assert_eq!(retained.source_revision, material.source_revision);
    let evidence_file = evidence_path(&store, &retained.id);
    let bindings_file = storage(&store).unwrap().join("bindings.json");
    let original_file = store
        .root()
        .join(".loom/attachments/objects")
        .join(&attachment.id);
    let original_stored = fs::read(&original_file).unwrap();
    let evidence_stored = fs::read(&evidence_file).unwrap();
    let bindings_stored = fs::read(&bindings_file).unwrap();
    for bytes in [&original_stored, &evidence_stored, &bindings_stored] {
        assert!(bytes.starts_with(b"MINEENC\x01"));
        assert!(
            !bytes
                .windows(TEXT.len())
                .any(|part| part == TEXT.as_bytes())
        );
    }
    let repeated = context_attachments::import_path(store.root(), &source_path).unwrap();
    assert_eq!(repeated.id, attachment.id);
    assert_eq!(
        bind_attachment(&store, &repeated.id, None).unwrap().id,
        entry.id
    );
    assert_eq!(read(&store, &entry.id).unwrap().evidence[0].id, retained.id);
    assert_eq!(fs::read(&original_file).unwrap(), original_stored);
    assert_eq!(fs::read(&evidence_file).unwrap(), evidence_stored);
    assert_eq!(fs::read(&bindings_file).unwrap(), bindings_stored);
    set_pinned(&store, &entry.id, true).unwrap();
    remove(&store, &entry.id).unwrap();
    assert!(list(&store).unwrap().is_empty());
    let root = store.root().to_path_buf();
    drop(store);
    // This reauthenticates the supplied key; it is not a fresh-process OS test.
    let vault = ProjectVault::open_with_key(&root, KEY).unwrap().unwrap();
    let store = ProjectStore::open_with_vault(&root, vault).unwrap();
    assert!(list(&store).unwrap().is_empty());
    let loaded = read_evidence(&store, &entry.id, &retained.id).unwrap();
    assert_eq!(evidence_payload(&loaded).unwrap(), expected);
    assert_eq!(loaded.text, material.text);
    assert_eq!(fs::read(&evidence_file).unwrap(), evidence_stored);
    assert_eq!(
        context_attachments::original_for_export(store.root(), &attachment.id).unwrap(),
        ("source.txt".to_owned(), TEXT.as_bytes().to_vec())
    );
    assert_eq!(fs::read(source_path).unwrap(), TEXT.as_bytes());
}

#[test]
fn secured_vault_loss_rejects_reads_new_writes_and_reopen() {
    let (_directory, store) = project(true);
    let expected = bindings(&store);
    write_bindings(&store, &expected).unwrap();
    let retained = retain_evidence(&store, evidence(TEXT)).unwrap();
    let evidence_file = evidence_path(&store, &retained.id);
    let root = storage(&store).unwrap();
    let bindings_file = root.join("bindings.json");
    let bindings_stored = fs::read(&bindings_file).unwrap();
    let evidence_stored = fs::read(&evidence_file).unwrap();
    let before = files_in(&root);
    let evidence_before = files_in(&root.join("evidence"));
    let project_root = store.root().to_path_buf();
    let vault = store.vault().unwrap().clone();
    fs::remove_file(project_root.join(".loom/vault.json")).unwrap();
    assert!(read_bindings(&store).is_err());
    assert!(write_bindings(&store, &expected).is_err());
    assert!(load_evidence(&store, &retained.id).is_err());
    assert!(retain_evidence(&store, retained).is_err());
    assert!(retain_evidence(&store, evidence("New private payload.\r\n")).is_err());
    assert_eq!(fs::read(bindings_file).unwrap(), bindings_stored);
    assert_eq!(fs::read(evidence_file).unwrap(), evidence_stored);
    assert_eq!(files_in(&root), before);
    assert_eq!(files_in(&root.join("evidence")), evidence_before);
    drop(store);
    assert!(ProjectStore::open_with_vault(&project_root, vault).is_err());
}

#[test]
fn secured_staging_uses_final_namespace_and_cleans_up_failed_publication() {
    let (_directory, store) = project(true);
    let plaintext = evidence_payload(&evidence(TEXT)).unwrap();
    let path = evidence_path(&store, &digest(&plaintext));
    let parent = path.parent().unwrap();
    let unrelated = parent.join("unrelated.tmp");
    fs::write(&unrelated, b"another operation owns this file").unwrap();
    let before = files_in(parent);
    let mut staged = None;
    let result = install_evidence_with(&store, &path, &plaintext, |temporary, destination| {
        staged = Some(temporary.to_path_buf());
        assert_eq!(destination, path.as_path());
        assert!(!destination.exists());
        let bytes = fs::read(temporary)?;
        assert!(bytes.starts_with(b"MINEENC\x01"));
        let namespace = crate::private_sidecar::namespace(store.root(), destination)?;
        let codec = crate::private_sidecar::PayloadCodec::open(store.root())?;
        assert_eq!(
            codec.open_bytes(&namespace, &bytes, MAX_EVIDENCE_BYTES)?,
            plaintext
        );
        assert!(crate::private_sidecar::read(store.root(), temporary, MAX_EVIDENCE_BYTES).is_err());
        Err(std::io::Error::other("injected pre-publication failure"))
    });
    assert!(result.is_err());
    assert!(!staged.unwrap().exists());
    assert!(!path.exists());
    assert_eq!(files_in(parent), before);
    assert_eq!(
        fs::read(unrelated).unwrap(),
        b"another operation owns this file"
    );
    let recovered = retain_evidence(&store, evidence(TEXT)).unwrap();
    assert_eq!(load_evidence(&store, &recovered.id).unwrap().text, TEXT);
}

#[test]
fn secured_staging_is_removed_during_unwinding() {
    let (_directory, store) = project(true);
    let plaintext = evidence_payload(&evidence(TEXT)).unwrap();
    let path = evidence_path(&store, &digest(&plaintext));
    let before = files_in(path.parent().unwrap());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        install_evidence_with(&store, &path, &plaintext, |temporary, _| {
            assert!(fs::read(temporary).unwrap().starts_with(b"MINEENC\x01"));
            panic!("injected after staging");
        })
    }));
    assert!(result.is_err());
    assert!(!path.exists());
    assert_eq!(files_in(path.parent().unwrap()), before);
}

#[test]
fn competing_publishers_compare_plaintext_without_replacing_the_winner() {
    for identical in [true, false] {
        let (_directory, store) = project(true);
        let plaintext = evidence_payload(&evidence(TEXT)).unwrap();
        let path = evidence_path(&store, &digest(&plaintext));
        let competing = if identical {
            plaintext.clone()
        } else {
            evidence_payload(&evidence(&TEXT.replace("Private", "Another"))).unwrap()
        };
        let mut winner = None;
        let result = install_evidence_with(&store, &path, &plaintext, |temporary, destination| {
            // Interleave another real publisher after our absent check and staging.
            // The outer production function, not this hook, performs its hard link.
            install_evidence(&store, destination, &competing).unwrap();
            let stored = fs::read(destination)?;
            assert_ne!(fs::read(temporary)?, stored);
            winner = Some(stored);
            Ok(())
        });
        assert_eq!(result.is_ok(), identical);
        assert_eq!(fs::read(&path).unwrap(), winner.unwrap());
        assert_eq!(files_in(path.parent().unwrap()).len(), 1);
    }
}

#[test]
fn secured_publisher_rejects_a_competing_plaintext_downgrade() {
    let (_directory, store) = project(true);
    let plaintext = evidence_payload(&evidence(TEXT)).unwrap();
    let path = evidence_path(&store, &digest(&plaintext));
    let result = install_evidence_with(&store, &path, &plaintext, |_, destination| {
        fs::write(destination, &plaintext)
    });
    assert!(result.is_err());
    assert_eq!(fs::read(&path).unwrap(), plaintext);
    assert_eq!(files_in(path.parent().unwrap()).len(), 1);
}

#[test]
fn material_symlinks_do_not_authorize_external_reads_or_writes() {
    for secured in [false, true] {
        let (_directory, store) = project(secured);
        let root = storage(&store).unwrap();
        let external = tempfile::tempdir().unwrap();
        let target = external.path().join("target.json");
        let expected = bindings(&store);
        let plaintext = serde_json::to_vec(&expected).unwrap();
        fs::write(&target, &plaintext).unwrap();
        std::os::unix::fs::symlink(&target, root.join("bindings.json")).unwrap();
        assert!(read_bindings(&store).is_err());
        assert!(write_bindings(&store, &expected).is_err());
        assert_eq!(fs::read(&target).unwrap(), plaintext);
        let payload = evidence_payload(&evidence(TEXT)).unwrap();
        let id = digest(&payload);
        let path = evidence_path(&store, &id);
        let missing = external.path().join("missing.json");
        std::os::unix::fs::symlink(&missing, &path).unwrap();
        assert!(load_evidence(&store, &id).is_err());
        assert!(retain_evidence(&store, evidence(TEXT)).is_err());
        assert!(!missing.exists());
        assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
    }
}

#[test]
fn secured_binding_updates_preserve_identity_and_noop_ciphertext() {
    let (_directory, store) = project(true);
    let mut expected = bindings(&store);
    write_bindings(&store, &expected).unwrap();
    let path = storage(&store).unwrap().join("bindings.json");
    let before = fs::read(&path).unwrap();
    write_bindings(&store, &expected).unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    let id = expected.items[0].id.clone();
    expected.items[0].pinned = true;
    write_bindings(&store, &expected).unwrap();
    let after = fs::read(&path).unwrap();
    assert_ne!(before, after);
    let loaded = read_bindings(&store).unwrap();
    assert_eq!(loaded.items[0].id, id);
    assert!(loaded.items[0].pinned);
    let namespace = crate::private_sidecar::namespace(store.root(), &path).unwrap();
    let plaintext = crate::private_sidecar::PayloadCodec::open(store.root())
        .unwrap()
        .open_bytes(&namespace, &after, MAX_STATE_BYTES)
        .unwrap();
    assert_eq!(plaintext, serde_json::to_vec(&expected).unwrap());
    assert_eq!(files_in(path.parent().unwrap()).len(), 2);
}

#[test]
fn secured_folder_search_retains_exact_revision_evidence() {
    let (_directory, mut store) = project(true);
    store
        .create_document_if_absent(
            "Notes/source.md",
            loom_document::DocumentContent::Prose(TEXT.into()),
            "fixture",
        )
        .unwrap();
    let source = store.read_document("Notes/source.md").unwrap();
    let folder = crate::document_bindings::snapshot_folder(&store, "Notes/").unwrap();
    let result = search_folder(
        &store,
        &folder,
        "Private",
        &FolderScanBudget::default(),
        &|| false,
    )
    .unwrap();
    assert_eq!(result.hits.len(), 1);
    let hit = &result.hits[0];
    let start = usize::try_from(hit.locator["start_byte"].as_u64().unwrap()).unwrap();
    let end = usize::try_from(hit.locator["end_byte"].as_u64().unwrap()).unwrap();
    assert_eq!(hit.text, source.text[start..end]);
    assert!(hit.text.ends_with("\r\n"));
    assert_eq!(hit.source_revision, folder.source_revision);
    assert_eq!(
        hit.locator["artifact_id"],
        serde_json::to_value(source.artifact_id).unwrap()
    );
    let stored = fs::read(evidence_path(&store, &hit.id)).unwrap();
    assert!(stored.starts_with(b"MINEENC\x01"));
    assert_eq!(
        evidence_payload(&load_evidence(&store, &hit.id).unwrap()).unwrap(),
        evidence_payload(hit).unwrap()
    );
    assert_eq!(
        fs::read(store.root().join("Notes/source.md")).unwrap(),
        TEXT.as_bytes()
    );
}

#[test]
fn secured_library_evidence_preserves_host_local_grant_authority() {
    let (_directory, store) = project(true);
    let external = tempfile::tempdir().unwrap();
    let external_root = fs::canonicalize(external.path()).unwrap();
    let database = external_root.join("library.sqlite3");
    super::tests::database(&database);
    let original = fs::read(&database).unwrap();
    let grant_root = external_root.join("grants");
    let entry = add_library_persisted(&store, &database, Some(&grant_root)).unwrap();
    let result = search(&store, &entry.id, "prayer").unwrap();
    assert!(!result.hits.is_empty());
    for hit in &result.hits {
        assert!(hit.source_evidence.is_some());
        let stored = fs::read(evidence_path(&store, &hit.id)).unwrap();
        assert!(stored.starts_with(b"MINEENC\x01"));
        assert_eq!(
            evidence_payload(&load_evidence(&store, &hit.id).unwrap()).unwrap(),
            evidence_payload(hit).unwrap()
        );
    }
    let grant_files = fs::read_dir(&grant_root)
        .unwrap()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(grant_files.len(), 1);
    let grant = read_safe(&grant_files[0].path(), MAX_STATE_BYTES).unwrap();
    assert!(serde_json::from_slice::<Value>(&grant).is_ok());
    grants()
        .lock()
        .unwrap()
        .remove(&grant_key(&store, &entry.id));
    assert!(!list(&store).unwrap()[0].available);
    assert!(search(&store, &entry.id, "prayer").is_err());
    restore_selected_grants(&store, &grant_root).unwrap();
    assert!(list(&store).unwrap()[0].available);
    assert_eq!(fs::read(database).unwrap(), original);
    forget_selected_grant(&store, &grant_root, &entry.id).unwrap();
    remove(&store, &entry.id).unwrap();
}
