//! Descriptive collection definitions in the one workspace Markdown document.
//! No account identity, credentials, continuation state, or acquisition effects.
use super::{MAX_TEMPLATE_BYTES, TEMPLATE_PATH, config_fence_range, load_template};
use crate::IpcFailure;
use loom_document::DocumentContent;
use loom_store::{LoadedDocument, ProjectStore, StoreError, VisibleProjectionState};
use loom_types::{BlobId, RevisionId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

const MAX_COLLECTIONS: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollectionDefinition {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_path: Option<String>,
    pub(crate) scope: CollectionScope,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CollectionScope {
    DriveFolder { id: String },
    GmailQuery { query: String },
}

impl CollectionDefinition {
    pub(crate) fn validate(&self) -> Result<(), IpcFailure> {
        if !self.id.strip_prefix("material-").is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }) {
            return Err(failure("A collection needs a stable material identity."));
        }
        if self.name.trim() != self.name
            || self.name.is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
            || self.name.ends_with('/')
        {
            return Err(failure(
                "A collection name must be a nonempty single line within 256 bytes, without a trailing slash.",
            ));
        }
        if let Some(path) = &self.workspace_path
            && (path.is_empty()
                || path.len() > 4096
                || path.contains('\\')
                || path.chars().any(char::is_control)
                || path
                    .split('/')
                    .any(|part| part.is_empty() || part.starts_with('.')))
        {
            return Err(failure(
                "Use an ordinary project-relative collection placement.",
            ));
        }
        match &self.scope {
            CollectionScope::DriveFolder { id }
                if id.is_empty()
                    || id.len() > 256
                    || !id.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
                    }) =>
            {
                Err(failure(
                    "Choose one explicit Drive folder ID; a blank scope cannot select an account.",
                ))
            }
            CollectionScope::GmailQuery { query }
                if query.trim().is_empty()
                    || query.len() > 4096
                    || query.chars().any(char::is_control) =>
            {
                Err(failure(
                    "A Gmail collection needs an explicit query within 4 KiB.",
                ))
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn fingerprint(&self) -> Result<String, IpcFailure> {
        self.validate()?;
        digest(&("loom.collection.definition.v1", self))
    }

    pub(crate) fn scope_fingerprint(&self) -> Result<String, IpcFailure> {
        self.validate()?;
        digest(&("loom.collection.scope.v1", &self.scope))
    }
}

fn digest(value: &impl Serialize) -> Result<String, IpcFailure> {
    let bytes = serde_json::to_vec(value).map_err(|error| failure(error.to_string()))?;
    Ok(BlobId::digest(&bytes).to_string())
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CollectionDefinitionsSnapshot {
    pub(crate) revision_id: Option<RevisionId>,
    pub(crate) collections: Vec<CollectionDefinition>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Definitions {
    collections: Vec<CollectionDefinition>,
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("workspace_collections_invalid", message, false)
}

pub(super) fn validate_definitions(definitions: &[CollectionDefinition]) -> Result<(), IpcFailure> {
    if definitions.len() > MAX_COLLECTIONS {
        return Err(failure(
            "A workspace supports at most 128 named collections.",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for definition in definitions {
        definition.validate()?;
        if !ids.insert(&definition.id) || !names.insert(&definition.name) {
            return Err(failure(
                "Collection identities and names must be unique within the workspace.",
            ));
        }
    }
    Ok(())
}

fn parse_definitions(markdown: &str) -> Result<Vec<CollectionDefinition>, IpcFailure> {
    if markdown.len() > MAX_TEMPLATE_BYTES {
        return Err(failure("Workspace configuration exceeds 64 KiB."));
    }
    let range = config_fence_range(markdown).map_err(failure)?;
    let text = range.map_or("", |range| &markdown[range]);
    let parsed: Definitions = toml::from_str(text).map_err(|error| failure(error.to_string()))?;
    validate_definitions(&parsed.collections)?;
    Ok(parsed.collections)
}

fn definitions_snapshot(
    loaded: Option<&LoadedDocument>,
) -> Result<CollectionDefinitionsSnapshot, IpcFailure> {
    Ok(CollectionDefinitionsSnapshot {
        revision_id: loaded.map(|document| document.revision_id),
        collections: loaded.map_or_else(
            || Ok(Vec::new()),
            |document| parse_definitions(&document.text),
        )?,
    })
}

/// Read exactly the registered revision. External bytes are never checkpointed here.
pub(crate) fn collection_definitions_current(
    store: &ProjectStore,
) -> Result<CollectionDefinitionsSnapshot, IpcFailure> {
    match store.read_document_bounded(TEMPLATE_PATH, MAX_TEMPLATE_BYTES as u64) {
        Ok(loaded) => definitions_snapshot(Some(&loaded)),
        Err(StoreError::NoActiveRevision(_)) => definitions_snapshot(None),
        Err(error) => Err(IpcFailure::store(error)),
    }
}

pub(crate) fn collection_definitions(
    store: &mut ProjectStore,
) -> Result<CollectionDefinitionsSnapshot, IpcFailure> {
    definitions_snapshot(load_template(store)?.as_ref())
}

pub(crate) fn collection_definition(
    store: &mut ProjectStore,
    id: &str,
) -> Result<Option<CollectionDefinition>, IpcFailure> {
    Ok(collection_definitions(store)?
        .collections
        .into_iter()
        .find(|definition| definition.id == id))
}

pub(crate) fn checked_base(
    store: &mut ProjectStore,
    expected_revision: Option<RevisionId>,
) -> Result<Option<LoadedDocument>, IpcFailure> {
    let loaded = load_template(store)?;
    if loaded.as_ref().map(|document| document.revision_id) != expected_revision {
        return Err(IpcFailure::new(
            "workspace_configuration_changed",
            "The workspace configuration changed. Read its current revision before changing a collection.",
            false,
        ));
    }
    if let Some(loaded) = &loaded {
        parse_definitions(&loaded.text)?;
        reject_pending_draft(store, loaded)?;
    }
    Ok(loaded)
}

fn reject_pending_draft(store: &ProjectStore, loaded: &LoadedDocument) -> Result<(), IpcFailure> {
    if store
        .load_transient_draft(TEMPLATE_PATH)
        .map_err(IpcFailure::store)?
        .is_some_and(|draft| draft.text != loaded.text)
    {
        return Err(failure(
            "Save the pending workspace configuration edits before changing its collections.",
        ));
    }
    Ok(())
}

/// Edit only the authoritative fence; Markdown and all unrelated TOML stay intact.
pub(crate) fn edit_config(
    markdown: &str,
    edit: impl FnOnce(&mut DocumentMut) -> Result<(), IpcFailure>,
) -> Result<String, IpcFailure> {
    if markdown.len() > MAX_TEMPLATE_BYTES {
        return Err(failure("Workspace configuration exceeds 64 KiB."));
    }
    let range = config_fence_range(markdown).map_err(failure)?;
    let mut document = range
        .as_ref()
        .map_or("", |range| &markdown[range.clone()])
        .parse::<DocumentMut>()
        .map_err(|error| failure(error.to_string()))?;
    edit(&mut document)?;
    let serialized = document.to_string();
    let output = if let Some(range) = range {
        let mut output = markdown.to_owned();
        output.replace_range(range, &serialized);
        output
    } else {
        let separator = if markdown.is_empty() || markdown.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        format!("{markdown}{separator}\n```loom-workspace\n{serialized}```\n")
    };
    if output.len() > MAX_TEMPLATE_BYTES {
        return Err(failure("Workspace configuration exceeds 64 KiB."));
    }
    parse_definitions(&output)?;
    Ok(output)
}

pub(crate) fn save_config(
    store: &mut ProjectStore,
    loaded: Option<&LoadedDocument>,
    text: String,
) -> Result<CollectionDefinitionsSnapshot, IpcFailure> {
    save_config_observed(store, loaded, text, None)
}

pub(crate) fn save_config_observed(
    store: &mut ProjectStore,
    loaded: Option<&LoadedDocument>,
    text: String,
    expected_generation: Option<&str>,
) -> Result<CollectionDefinitionsSnapshot, IpcFailure> {
    #[cfg(unix)]
    let observation = if loaded.is_some() {
        let snapshot = crate::materials::metadata::WorkspaceSnapshot::open(store.root())
            .map_err(|error| failure(error.to_string()))?
            .ok_or_else(|| failure("Workspace configuration disappeared."))?;
        if expected_generation.is_some_and(|expected| expected != snapshot.revision)
            || loaded.is_some_and(|loaded| snapshot.bytes != loaded.text.as_bytes())
        {
            return Err(failure(
                "Workspace configuration changed during observation.",
            ));
        }
        Some(snapshot)
    } else {
        None
    };
    #[cfg(not(unix))]
    if expected_generation.is_some() {
        return Err(failure(
            "Workspace metadata mutation is unsupported on this platform.",
        ));
    }
    match loaded {
        Some(loaded) if loaded.text == text =>
        {
            #[cfg(unix)]
            if let Some(observation) = &observation {
                observation
                    .ensure_current(store.root())
                    .map_err(|error| failure(error.to_string()))?;
            }
        }
        Some(loaded) => {
            reject_pending_draft(store, loaded)?;
            let root = store.root().to_path_buf();
            let outcome = store
                .save_document_if_source_with_guard(
                    TEMPLATE_PATH,
                    DocumentContent::Prose(text),
                    "Update workspace configuration",
                    loaded.revision_id,
                    loaded.blob_id,
                    |_| {
                        #[cfg(unix)]
                        observation
                            .as_ref()
                            .ok_or_else(|| {
                                StoreError::Io(std::io::Error::other(
                                    "Workspace observation disappeared",
                                ))
                            })?
                            .ensure_current(&root)
                            .map_err(|error| {
                                StoreError::Io(std::io::Error::other(error.to_string()))
                            })?;
                        #[cfg(not(unix))]
                        let _ = &root;
                        Ok(())
                    },
                )
                .map_err(IpcFailure::store)?;
            if outcome.visible_projection != VisibleProjectionState::Applied {
                return Err(IpcFailure::new(
                    "workspace_configuration_pending",
                    "Workspace configuration needs reconciliation before this change can be acknowledged.",
                    false,
                ));
            }
        }
        None => {
            store
                .create_document_if_absent(
                    TEMPLATE_PATH,
                    DocumentContent::Prose(text),
                    "Create workspace configuration",
                )
                .map_err(IpcFailure::store)?;
        }
    }
    collection_definitions_current(store)
}

pub(crate) fn change_metadata(
    store: &mut ProjectStore,
    revision: Option<RevisionId>,
    id: &str,
    change: crate::materials::MetadataChange<'_>,
    expected_generation: &str,
) -> Result<CollectionDefinitionsSnapshot, IpcFailure> {
    if matches!(change, crate::materials::MetadataChange::Remove) {
        return remove_collection_observed(store, revision, id, Some(expected_generation));
    }
    let base = checked_base(store, revision)?;
    let loaded = base
        .as_ref()
        .ok_or_else(|| failure("The collection definition does not exist."))?;
    let text = edit_config(&loaded.text, |document| {
        let tables = document
            .get_mut("collections")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| failure("The collection definition does not exist."))?;
        let index = tables
            .iter()
            .position(|table| table.get("id").and_then(Item::as_str) == Some(id))
            .ok_or_else(|| failure("The collection definition does not exist."))?;
        match change {
            crate::materials::MetadataChange::Rename(name) => set_owned_field(
                tables
                    .get_mut(index)
                    .ok_or_else(|| failure("The collection definition does not exist."))?,
                "name",
                toml_edit::value(name),
            ),
            crate::materials::MetadataChange::Pin(pinned) => set_owned_field(
                tables
                    .get_mut(index)
                    .ok_or_else(|| failure("The collection definition does not exist."))?,
                "pinned",
                toml_edit::value(pinned),
            ),
            crate::materials::MetadataChange::Remove => {
                tables.remove(index);
            }
        }
        if tables.is_empty() {
            document.remove("collections");
        }
        Ok(())
    })?;
    save_config_observed(store, Some(loaded), text, Some(expected_generation))
}

fn serialized_table(definition: &CollectionDefinition) -> Result<Table, IpcFailure> {
    let text = toml::to_string(&Definitions {
        collections: vec![definition.clone()],
    })
    .map_err(|error| failure(error.to_string()))?;
    let document = text
        .parse::<DocumentMut>()
        .map_err(|error| failure(error.to_string()))?;
    let mut table = document
        .get("collections")
        .and_then(Item::as_array_of_tables)
        .and_then(|tables| tables.get(0))
        .cloned()
        .ok_or_else(|| failure("Unable to encode the collection definition."))?;
    // Source-document table positions cannot be transplanted into another
    // document: they can put a scope beneath the wrong array member.
    table.set_position(None);
    if let Some(Item::Table(scope)) = table.remove("scope") {
        table.insert(
            "scope",
            Item::Value(toml_edit::Value::InlineTable(scope.into_inline_table())),
        );
    }
    Ok(table)
}

pub(super) fn set_owned_field(table: &mut Table, key: &str, mut value: Item) {
    if let (Some(old), Some(new)) = (
        table.get(key).and_then(Item::as_value),
        value.as_value_mut(),
    ) {
        *new.decor_mut() = old.decor().clone();
    }
    table.insert(key, value);
}

pub(crate) fn upsert_collection(
    store: &mut ProjectStore,
    expected_revision: Option<RevisionId>,
    definition: &CollectionDefinition,
) -> Result<CollectionDefinitionsSnapshot, IpcFailure> {
    definition.validate()?;
    let base = checked_base(store, expected_revision)?;
    let replacement = serialized_table(definition)?;
    let text = edit_config(
        base.as_ref().map_or("", |loaded| loaded.text.as_str()),
        |document| {
            if base.is_none() {
                document["panes_enabled"] = toml_edit::value(false);
            }
            if !document.contains_key("collections") {
                document["collections"] = Item::ArrayOfTables(ArrayOfTables::new());
            }
            let tables = document.get_mut("collections").and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| failure("Collections must use [[collections]] tables; the existing value was preserved."))?;
            if let Some(existing) = tables
                .iter_mut()
                .find(|table| table.get("id").and_then(Item::as_str) == Some(&definition.id))
            {
                for key in ["id", "name", "pinned", "scope"] {
                    let value = replacement
                        .get(key)
                        .cloned()
                        .ok_or_else(|| failure("Incomplete collection encoding."))?;
                    set_owned_field(existing, key, value);
                }
                match replacement.get("workspace_path") {
                    Some(value) => set_owned_field(existing, "workspace_path", value.clone()),
                    None => {
                        existing.remove("workspace_path");
                    }
                }
            } else {
                tables.push(replacement);
            }
            Ok(())
        },
    )?;
    if !parse_definitions(&text)?
        .iter()
        .any(|saved| saved == definition)
    {
        return Err(failure(
            "The collection could not be placed in an authoritative workspace fence. Close any unfinished Markdown fence first.",
        ));
    }
    save_config(store, base.as_ref(), text)
}

#[cfg(test)]
pub(crate) fn remove_collection(
    store: &mut ProjectStore,
    expected_revision: Option<RevisionId>,
    id: &str,
) -> Result<CollectionDefinitionsSnapshot, IpcFailure> {
    remove_collection_observed(store, expected_revision, id, None)
}

fn remove_collection_observed(
    store: &mut ProjectStore,
    expected_revision: Option<RevisionId>,
    id: &str,
    expected_generation: Option<&str>,
) -> Result<CollectionDefinitionsSnapshot, IpcFailure> {
    let base = checked_base(store, expected_revision)?;
    let Some(loaded) = &base else {
        return Err(failure("The collection definition does not exist."));
    };
    let text = edit_config(&loaded.text, |document| {
        let tables = document
            .get_mut("collections")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| failure("The collection definition does not exist."))?;
        let index = tables
            .iter()
            .position(|table| table.get("id").and_then(Item::as_str) == Some(id))
            .ok_or_else(|| failure("The collection definition does not exist."))?;
        tables.remove(index);
        if tables.is_empty() {
            document.remove("collections");
        }
        Ok(())
    })?;
    save_config_observed(store, Some(loaded), text, expected_generation)
}

pub(super) fn enable_panes(
    store: &mut ProjectStore,
    loaded: &LoadedDocument,
) -> Result<(), IpcFailure> {
    let text = edit_config(&loaded.text, |document| {
        document["panes_enabled"] = toml_edit::value(true);
        Ok(())
    })?;
    save_config(store, Some(loaded), text)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, ProjectStore) {
        let root = tempfile::tempdir().unwrap();
        let (store, _) = ProjectStore::initialize(root.path().join("Writing"), "Writing").unwrap();
        (root, store)
    }

    fn definition(letter: char, name: &str) -> CollectionDefinition {
        CollectionDefinition {
            id: format!("material-{}", letter.to_string().repeat(64)),
            name: name.into(),
            pinned: false,
            workspace_path: None,
            scope: CollectionScope::DriveFolder {
                id: "chosen_folder-1".into(),
            },
        }
    }

    #[test]
    fn definitions_alone_do_not_enable_panes_and_explicit_enable_preserves_them() {
        let (_root, mut store) = project();
        let added = upsert_collection(&mut store, None, &definition('a', "Research")).unwrap();
        assert_eq!(added.collections.len(), 1);
        let snapshot = super::super::snapshot(&mut store).unwrap();
        assert!(!snapshot.enabled);
        assert!(!snapshot.config.panes_enabled);
        let enabled = super::super::enable(&mut store).unwrap();
        assert!(enabled.enabled);
        assert_eq!(
            collection_definitions_current(&store).unwrap().collections,
            added.collections
        );
        assert!(
            !store
                .read_document(TEMPLATE_PATH)
                .unwrap()
                .text
                .contains("Uncomment a setting")
        );
    }

    #[test]
    fn narrow_edits_preserve_markdown_comments_and_unrelated_unknown_configuration() {
        let (_root, mut store) = project();
        let original = "# My workspace\r\nKeep **this** prose.\r\n\r\n~~~loom-workspace\r\n# preserve my model comment\r\n[panes.chat]\r\nvisible = false # keep this\r\n[future]\r\nunknown = 'preserved'\r\n~~~\r\nTail stays exact.\r\n";
        store
            .create_document_if_absent(
                TEMPLATE_PATH,
                DocumentContent::Prose(original.into()),
                "fixture",
            )
            .unwrap();
        let initial = collection_definitions_current(&store).unwrap();
        let added = upsert_collection(
            &mut store,
            initial.revision_id,
            &definition('a', "Research"),
        )
        .unwrap();
        let text = store.read_document(TEMPLATE_PATH).unwrap().text;
        assert!(
            text.starts_with("# My workspace\r\nKeep **this** prose.\r\n\r\n~~~loom-workspace\r\n")
        );
        assert!(text.ends_with("~~~\r\nTail stays exact.\r\n"));
        assert!(text.contains("visible = false # keep this"));
        assert!(text.contains("unknown = 'preserved'"));
        assert!(text.contains("# preserve my model comment"));
        let mut changed = added.collections[0].clone();
        changed.name = "Renamed research".into();
        changed.pinned = true;
        let edited = upsert_collection(&mut store, added.revision_id, &changed).unwrap();
        assert_eq!(edited.collections, vec![changed]);
        let removed =
            remove_collection(&mut store, edited.revision_id, &definition('a', "x").id).unwrap();
        assert!(removed.collections.is_empty());
        assert!(
            store
                .read_document(TEMPLATE_PATH)
                .unwrap()
                .text
                .contains("unknown = 'preserved'")
        );
    }

    #[test]
    fn stale_revisions_external_edits_and_unsaved_drafts_cannot_be_overwritten() {
        let (_root, mut store) = project();
        let first = upsert_collection(&mut store, None, &definition('a', "First")).unwrap();
        let second =
            upsert_collection(&mut store, first.revision_id, &definition('b', "Second")).unwrap();
        assert!(
            remove_collection(&mut store, first.revision_id, &first.collections[0].id).is_err()
        );
        let before = store.read_document(TEMPLATE_PATH).unwrap();
        let external = format!("{}\nAn external note.\n", before.text);
        std::fs::write(store.root().join(TEMPLATE_PATH), &external).unwrap();
        assert!(collection_definitions_current(&store).is_err());
        assert!(
            upsert_collection(&mut store, second.revision_id, &definition('c', "Third")).is_err()
        );
        let current = collection_definitions(&mut store).unwrap();
        assert_eq!(store.read_document(TEMPLATE_PATH).unwrap().text, external);
        let loaded = store.read_document(TEMPLATE_PATH).unwrap();
        store
            .upsert_transient_draft(
                TEMPLATE_PATH,
                loaded.revision_id,
                0,
                DocumentContent::Prose("Unsaved settings".into()),
            )
            .unwrap();
        assert!(
            upsert_collection(&mut store, current.revision_id, &definition('c', "Third")).is_err()
        );
        assert_eq!(store.read_document(TEMPLATE_PATH).unwrap().text, external);
        assert_eq!(
            store
                .load_transient_draft(TEMPLATE_PATH)
                .unwrap()
                .unwrap()
                .text,
            "Unsaved settings"
        );
    }

    #[test]
    fn incompatible_definitions_are_preserved_and_descriptions_never_include_authority() {
        let (_root, mut store) = project();
        let original = "```loom-workspace\n[[collections]]\nid='future-format'\nname='Do not replace me'\nsecret='unsupported'\n```\n";
        store
            .create_document_if_absent(
                TEMPLATE_PATH,
                DocumentContent::Prose(original.into()),
                "fixture",
            )
            .unwrap();
        let revision = store.read_document(TEMPLATE_PATH).unwrap().revision_id;
        assert!(collection_definitions_current(&store).is_err());
        assert!(
            upsert_collection(&mut store, Some(revision), &definition('a', "Research")).is_err()
        );
        assert_eq!(store.read_document(TEMPLATE_PATH).unwrap().text, original);
        let mut value = definition('a', "Research");
        let scope = value.scope_fingerprint().unwrap();
        let original = value.fingerprint().unwrap();
        value.name = "Renamed".into();
        value.pinned = true;
        assert_eq!(value.scope_fingerprint().unwrap(), scope);
        assert_ne!(value.fingerprint().unwrap(), original);
        value.scope = CollectionScope::GmailQuery {
            query: "from:friend@example.com".into(),
        };
        assert_ne!(value.scope_fingerprint().unwrap(), scope);
        value.scope = CollectionScope::DriveFolder { id: String::new() };
        assert!(value.validate().is_err());
    }

    #[test]
    fn aliases_and_ids_are_unique_and_creation_does_not_clobber_unregistered_settings() {
        let (_root, mut store) = project();
        let original = "My existing private settings.\n";
        std::fs::write(store.root().join(TEMPLATE_PATH), original).unwrap();
        assert!(upsert_collection(&mut store, None, &definition('a', "Research")).is_err());
        assert_eq!(store.read_document(TEMPLATE_PATH).unwrap().text, original);
        let revision = collection_definitions(&mut store).unwrap().revision_id;
        let first = upsert_collection(&mut store, revision, &definition('a', "Research")).unwrap();
        assert!(
            upsert_collection(&mut store, first.revision_id, &definition('b', "Research")).is_err()
        );
        assert_eq!(
            collection_definitions_current(&store)
                .unwrap()
                .collections
                .len(),
            1
        );
        let mut duplicate = first.collections.clone();
        duplicate.push(first.collections[0].clone());
        assert!(validate_definitions(&duplicate).is_err());
    }

    #[test]
    fn unfinished_other_fences_cannot_hide_a_successfully_reported_definition() {
        let (_root, mut store) = project();
        let original = "# Notes\n```example\nUnfinished example\n";
        store
            .create_document_if_absent(
                TEMPLATE_PATH,
                DocumentContent::Prose(original.into()),
                "fixture",
            )
            .unwrap();
        let revision = collection_definitions_current(&store).unwrap().revision_id;
        assert!(upsert_collection(&mut store, revision, &definition('a', "Research")).is_err());
        assert_eq!(store.read_document(TEMPLATE_PATH).unwrap().text, original);
    }
}
