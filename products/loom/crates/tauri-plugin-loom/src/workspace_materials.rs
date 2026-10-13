//! Ordinary material definitions share the workspace's conflict-checked fence.
use super::{MAX_TEMPLATE_BYTES, TEMPLATE_PATH, collections, config_fence, load_template};
use crate::{
    IpcFailure,
    materials::{self, Binding},
};
use loom_store::{ProjectStore, StoreError};
use loom_types::RevisionId;
use serde::{Deserialize, Serialize};
use toml_edit::{ArrayOfTables, DocumentMut, Item};

#[derive(Default, Deserialize, Serialize)]
struct Definitions {
    materials: Option<Vec<Binding>>,
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("workspace_materials_invalid", message, false)
}

fn parse(markdown: &str) -> Result<Option<Vec<Binding>>, IpcFailure> {
    let definitions: Definitions = toml::from_str(config_fence(markdown).map_err(failure)?)
        .map_err(|error| failure(error.to_string()))?;
    if let Some(items) = &definitions.materials {
        materials::validate_bindings(items).map_err(|error| failure(error.to_string()))?;
    }
    Ok(definitions.materials)
}

pub(crate) fn current(
    store: &ProjectStore,
) -> Result<(Option<RevisionId>, Option<Vec<Binding>>), IpcFailure> {
    match store.read_document_bounded(TEMPLATE_PATH, MAX_TEMPLATE_BYTES as u64) {
        Ok(loaded) => Ok((Some(loaded.revision_id), parse(&loaded.text)?)),
        Err(StoreError::NoActiveRevision(_)) => Ok((None, None)),
        Err(error) => Err(IpcFailure::store(error)),
    }
}

pub(crate) fn prepare(store: &mut ProjectStore) -> Result<(), IpcFailure> {
    materials::metadata::refuse_legacy_bindings(store.root())
        .map_err(|error| failure(error.to_string()))?;
    load_template(store)?;
    Ok(())
}

pub(crate) fn save(
    store: &mut ProjectStore,
    revision: Option<RevisionId>,
    items: &[Binding],
) -> Result<(), IpcFailure> {
    save_observed(store, revision, items, None)
}

pub(crate) fn save_observed(
    store: &mut ProjectStore,
    revision: Option<RevisionId>,
    items: &[Binding],
    expected_generation: Option<&str>,
) -> Result<(), IpcFailure> {
    materials::validate_bindings(items).map_err(|error| failure(error.to_string()))?;
    let base = collections::checked_base(store, revision)?;
    let encoded = toml::to_string(&Definitions {
        materials: Some(items.to_vec()),
    })
    .map_err(|error| failure(error.to_string()))?;
    let encoded = encoded
        .parse::<DocumentMut>()
        .map_err(|error| failure(error.to_string()))?;
    let text = collections::edit_config(
        base.as_ref().map_or("", |loaded| loaded.text.as_str()),
        |document| {
            if base.is_none() {
                document["panes_enabled"] = toml_edit::value(false);
            }
            let mut tables = ArrayOfTables::new();
            for replacement in encoded
                .get("materials")
                .and_then(Item::as_array_of_tables)
                .into_iter()
                .flat_map(|tables| tables.iter())
            {
                let id = replacement.get("id").and_then(Item::as_str);
                let old = document
                    .get("materials")
                    .and_then(Item::as_array_of_tables)
                    .and_then(|tables| {
                        tables
                            .iter()
                            .find(|table| table.get("id").and_then(Item::as_str) == id)
                    });
                let mut table = old.cloned().unwrap_or_default();
                for (key, value) in replacement {
                    let mut value = value.clone();
                    if let Item::Table(nested) = value {
                        value =
                            Item::Value(toml_edit::Value::InlineTable(nested.into_inline_table()));
                    }
                    collections::set_owned_field(&mut table, key, value);
                }
                if !replacement.contains_key("workspace_path") {
                    table.remove("workspace_path");
                }
                table.set_position(None);
                tables.push(table);
            }
            document["materials"] = if tables.is_empty() {
                toml_edit::value(toml_edit::Array::new())
            } else {
                Item::ArrayOfTables(tables)
            };
            Ok(())
        },
    )?;
    if parse(&text)?.as_deref() != Some(items) {
        return Err(failure(
            "Material definitions could not be placed in the workspace fence. Close any unfinished Markdown fence first.",
        ));
    }
    collections::save_config_observed(store, base.as_ref(), text, expected_generation)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use loom_document::DocumentContent;
    use std::fs;

    fn project() -> (tempfile::TempDir, ProjectStore) {
        let root = tempfile::tempdir().unwrap();
        let (store, _) = ProjectStore::initialize(root.path().join("Writing"), "Writing").unwrap();
        (root, store)
    }

    fn source(store: &mut ProjectStore, name: &str) -> materials::MaterialEntry {
        let path = store.root().join(format!("{name}.txt"));
        fs::write(&path, name).unwrap();
        let attachment = crate::context_attachments::import_path(store.root(), &path).unwrap();
        materials::bind_attachment(store, &attachment.id, Some(name)).unwrap()
    }

    #[test]
    fn edits_share_one_fence_and_preserve_unrelated_settings_and_source_identity() {
        let (_root, mut store) = project();
        let original = "# Workspace\nKeep this prose.\n\n```loom-workspace\n# My choice\n[theme]\nmode = 'system' # keep this\n```\nAfterword.\n";
        store
            .create_document_if_absent(
                TEMPLATE_PATH,
                DocumentContent::Prose(original.into()),
                "fixture",
            )
            .unwrap();
        let first = source(&mut store, "First");
        let second = source(&mut store, "Second");
        materials::set_pinned(&mut store, &first.id, true).unwrap();
        materials::remove(&mut store, &second.id).unwrap();
        let text = store.read_document(TEMPLATE_PATH).unwrap().text;
        assert!(text.starts_with("# Workspace\nKeep this prose.\n\n```loom-workspace\n"));
        assert!(text.ends_with("```\nAfterword.\n"));
        assert!(text.contains("mode = 'system' # keep this"));
        assert!(text.contains("# My choice"));
        assert!(super::super::parse_config(&text).is_ok());
        let retained = materials::resolve(&store, &first.id).unwrap();
        assert!(retained.pinned);
        assert_eq!(retained.attachment_id, first.attachment_id);
        assert!(!store.root().join(".loom/materials/bindings.json").exists());
        assert_eq!(materials::read(&store, &first.id).unwrap().text, "First");
    }

    #[test]
    fn stale_configuration_and_unsaved_edits_are_never_overwritten() {
        let (_root, mut store) = project();
        let first = source(&mut store, "First");
        let (revision, items) = current(&store).unwrap();
        source(&mut store, "Second");
        assert_eq!(
            save(&mut store, revision, &items.unwrap())
                .unwrap_err()
                .code,
            "workspace_configuration_changed"
        );
        let loaded = store.read_document(TEMPLATE_PATH).unwrap();
        store
            .upsert_transient_draft(
                TEMPLATE_PATH,
                loaded.revision_id,
                0,
                DocumentContent::Prose("Unsaved settings".into()),
            )
            .unwrap();
        assert!(materials::set_pinned(&mut store, &first.id, true).is_err());
        assert!(materials::remove(&mut store, &first.id).is_err());
        assert_eq!(
            store.read_document(TEMPLATE_PATH).unwrap().text,
            loaded.text
        );
        assert_eq!(
            store
                .load_transient_draft(TEMPLATE_PATH)
                .unwrap()
                .unwrap()
                .text,
            "Unsaved settings"
        );
        assert!(!materials::resolve(&store, &first.id).unwrap().pinned);
    }

    #[test]
    fn copied_configuration_describes_a_library_without_granting_access() {
        let (root, mut store) = project();
        let library = root.path().join("library.sqlite3");
        let connection = rusqlite::Connection::open(&library).unwrap();
        connection
            .execute_batch(include_str!("materials/alexandria-fixture.sql"))
            .unwrap();
        drop(connection);
        let original = fs::read(&library).unwrap();
        let entry = materials::add_library(&mut store, &library, Some("Research")).unwrap();
        let attachment = source(&mut store, "Note");
        let text = store.read_document(TEMPLATE_PATH).unwrap().text;
        let (_copy_root, mut copy) = project();
        fs::write(copy.root().join(TEMPLATE_PATH), text).unwrap();
        prepare(&mut copy).unwrap();
        let listed = materials::list(&copy).unwrap();
        assert_eq!(listed[0].id, entry.id);
        assert!(!listed[0].available);
        assert!(!materials::resolve(&copy, &attachment.id).unwrap().available);
        assert!(materials::read(&copy, &attachment.id).is_err());
        assert!(matches!(
            materials::search(&copy, &entry.id, "prayer"),
            Err(materials::MaterialError::NeedsAuthorization(_))
        ));
        assert_eq!(fs::read(library).unwrap(), original);
    }

    #[test]
    fn retired_bindings_refuse_material_admission_and_preparation_without_mutation() {
        for encrypted in [false, true] {
            for configured in [false, true] {
                let parent = tempfile::tempdir().unwrap();
                let root = parent.path().join("Writing");
                let (mut store, _) = if encrypted {
                    fs::create_dir_all(root.join(".loom")).unwrap();
                    let vault =
                        desktop_vault::ProjectVault::initialize_with_key(&root, [51; 32]).unwrap();
                    ProjectStore::initialize_with_vault(&root, "Writing", vault).unwrap()
                } else {
                    ProjectStore::initialize(&root, "Writing").unwrap()
                };
                let entry = source(&mut store, "Research");
                let (_, items) = current(&store).unwrap();
                let previous = serde_json::to_vec(&serde_json::json!({
                    "schema": "loom.materials.v1", "items": items.unwrap()
                }))
                .unwrap();
                if !configured {
                    store
                        .save_document(
                            TEMPLATE_PATH,
                            DocumentContent::Prose("# Workspace\n".into()),
                            "fixture without definitions",
                        )
                        .unwrap();
                }
                let source_path = crate::context_attachments::original_path(
                    store.root(),
                    entry.attachment_id.as_deref().unwrap(),
                )
                .unwrap();
                let original_source = fs::read(&source_path).unwrap();
                let path = store.root().join(".loom/materials/bindings.json");
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                let stored = crate::private_sidecar::PayloadCodec::open(store.root())
                    .unwrap()
                    .seal(".loom/materials/bindings.json", &previous)
                    .unwrap();
                fs::write(&path, &stored).unwrap();
                let workspace = fs::read(store.root().join(TEMPLATE_PATH)).unwrap();
                let counts = store.counts().unwrap();
                let names = fs::read_dir(path.parent().unwrap())
                    .unwrap()
                    .map(|item| item.unwrap().file_name())
                    .collect::<std::collections::BTreeSet<_>>();
                let listed = materials::list(&store);
                let admitted = materials::admit(&store, &entry.id, 1_024);
                let prepared = prepare(&mut store);
                assert!(
                    listed.is_err() && admitted.is_err() && prepared.is_err(),
                    "retired admission: encrypted={encrypted}, configured={configured}, list={listed:?}, admit={admitted:?}, prepare={prepared:?}, legacy_after={:?}",
                    fs::read(&path).ok()
                );
                assert_eq!(fs::read(&path).unwrap(), stored);
                assert_eq!(
                    fs::read(store.root().join(TEMPLATE_PATH)).unwrap(),
                    workspace
                );
                assert_eq!(store.counts().unwrap(), counts);
                assert_eq!(fs::read(&source_path).unwrap(), original_source);
                assert_eq!(
                    fs::read_dir(path.parent().unwrap())
                        .unwrap()
                        .map(|item| item.unwrap().file_name())
                        .collect::<std::collections::BTreeSet<_>>(),
                    names
                );
            }
        }
    }

    #[test]
    fn retired_binding_presence_refuses_unsafe_leafs_without_following_them() {
        use std::os::unix::fs::symlink;
        for kind in ["directory", "symlink", "dangling", "hardlink"] {
            let (_parent, mut store) = project();
            let entry = source(&mut store, "Current");
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("untouched");
            fs::write(&target, b"untouched private source").unwrap();
            let path = store.root().join(".loom/materials/bindings.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            match kind {
                "directory" => fs::create_dir(&path).unwrap(),
                "symlink" => symlink(&target, &path).unwrap(),
                "dangling" => symlink(outside.path().join("missing"), &path).unwrap(),
                "hardlink" => fs::hard_link(&target, &path).unwrap(),
                _ => unreachable!(),
            }
            let metadata = fs::symlink_metadata(&path).unwrap();
            let link = metadata
                .file_type()
                .is_symlink()
                .then(|| fs::read_link(&path).unwrap());
            let workspace = fs::read(store.root().join(TEMPLATE_PATH)).unwrap();
            let counts = store.counts().unwrap();
            let listed = materials::list(&store);
            let admitted = materials::admit(&store, &entry.id, 1_024);
            let prepared = prepare(&mut store);
            assert!(
                listed.is_err() && admitted.is_err() && prepared.is_err(),
                "retired {kind}: list={listed:?}, admit={admitted:?}, prepare={prepared:?}"
            );
            assert_eq!(fs::read(&target).unwrap(), b"untouched private source");
            assert_eq!(
                fs::read(store.root().join(TEMPLATE_PATH)).unwrap(),
                workspace
            );
            assert_eq!(store.counts().unwrap(), counts);
            assert_eq!(
                fs::symlink_metadata(&path).unwrap().file_type(),
                metadata.file_type()
            );
            if let Some(link) = link {
                assert_eq!(fs::read_link(&path).unwrap(), link);
            }
        }
    }

    #[test]
    fn archive_only_metadata_never_revives_bindings_or_blocks_retained_evidence() {
        let (_parent, mut store) = project();
        let entry = source(&mut store, "Exact retained source");
        let retained = materials::read(&store, &entry.id).unwrap();
        let evidence = &retained.evidence[0];
        let (_, items) = current(&store).unwrap();
        let previous = serde_json::to_vec(&serde_json::json!({
            "schema": "loom.materials.v1", "items": items.unwrap()
        }))
        .unwrap();
        materials::remove(&mut store, &entry.id).unwrap();
        let archive = store.root().join(".loom/materials").join(format!(
            "bindings-{}.json",
            loom_types::BlobId::digest(&previous)
        ));
        fs::write(&archive, &previous).unwrap();
        let workspace = fs::read(store.root().join(TEMPLATE_PATH)).unwrap();
        let counts = store.counts().unwrap();
        prepare(&mut store).unwrap();
        assert!(materials::list(&store).unwrap().is_empty());
        assert!(materials::admit(&store, &entry.id, 1_024).is_err());
        assert_eq!(
            materials::read_evidence(&store, &entry.id, &evidence.id)
                .unwrap()
                .text,
            retained.text
        );
        assert_eq!(fs::read(&archive).unwrap(), previous);
        assert_eq!(
            fs::read(store.root().join(TEMPLATE_PATH)).unwrap(),
            workspace
        );
        assert_eq!(store.counts().unwrap(), counts);
    }

    #[test]
    fn retired_bindings_refuse_before_adopting_an_unregistered_workspace_document() {
        let (_parent, mut store) = project();
        let workspace_path = store.root().join(TEMPLATE_PATH);
        let author_bytes = b"# Author-owned workspace\r\nKeep exact bytes.\r\n";
        fs::write(&workspace_path, author_bytes).unwrap();
        assert!(
            !store
                .list_documents()
                .unwrap()
                .iter()
                .any(|document| document.relative_path == TEMPLATE_PATH)
        );
        let path = store.root().join(".loom/materials/bindings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let retired = b"{\"schema\":\"loom.materials.v1\",\"items\":[]}";
        fs::write(&path, retired).unwrap();
        let counts = store.counts().unwrap();
        let result = prepare(&mut store);
        assert!(result.is_err(), "retired pre-load result={result:?}");
        assert_eq!(store.counts().unwrap(), counts);
        assert!(
            !store
                .list_documents()
                .unwrap()
                .iter()
                .any(|document| document.relative_path == TEMPLATE_PATH)
        );
        assert_eq!(fs::read(&workspace_path).unwrap(), author_bytes);
        assert_eq!(fs::read(&path).unwrap(), retired);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }
}
