//! Portable workspace roots are names, never native filesystem capabilities.
//! Only explicit native selection creates an application-private path grant.
use crate::{
    IpcFailure,
    workspace_template::{self, collections},
};
use atomic_write_file::AtomicWriteFile;
use loom_store::{ProjectStore, StoreError};
use loom_types::{BlobId, CommandId, ProjectManifest, RevisionId};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    hash::{Hash, Hasher},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::Mutex,
};
use toml_edit::{ArrayOfTables, Item};

pub(crate) const OWNER_ROOT_ID: &str = "owner";
const MAX_ROOTS: usize = 32;
const MAX_GRANT_BYTES: u64 = 32_768;
const GRANT_SCHEMA: &str = "loom.workspace-root-grant.v1";
static GRANT_WRITES: Mutex<()> = Mutex::new(());
type Result<T> = std::result::Result<T, IpcFailure>;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootDefinition {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RootDescriptor {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) owner: bool,
    pub(crate) available: bool,
    pub(crate) path: Option<PathBuf>,
    pub(crate) project_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct Definitions {
    #[serde(default)]
    roots: Vec<RootDefinition>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Directory {
    path: PathBuf,
    project_id: String,
    identity: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    schema: String,
    owner: Directory,
    root_id: String,
    target: Directory,
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("workspace_root_invalid", message, false)
}
fn io(error: impl std::fmt::Display) -> IpcFailure {
    failure(error.to_string())
}

pub(crate) fn validate_definitions(roots: &[RootDefinition]) -> Result<()> {
    if roots.len() > MAX_ROOTS {
        return Err(failure("A workspace supports at most 32 mounted folders."));
    }
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for root in roots {
        validate_id(&root.id)?;
        if root.name.is_empty()
            || root.name.trim() != root.name
            || root.name.len() > 256
            || root.name.chars().any(char::is_control)
            || root.name.contains(['/', '\\'])
        {
            return Err(failure(
                "A folder name must be a single nonempty name within 256 bytes.",
            ));
        }
        if !ids.insert(&root.id) || !names.insert(&root.name) {
            return Err(failure(
                "Workspace folder identities and names must be unique.",
            ));
        }
    }
    Ok(())
}
fn validate_id(id: &str) -> Result<()> {
    let value = id
        .strip_prefix("root-")
        .ok_or_else(|| failure("Invalid workspace folder identity."))?;
    if value.parse::<CommandId>().map_err(io)?.to_string() != value {
        return Err(failure(
            "Workspace folder identities use canonical spelling.",
        ));
    }
    Ok(())
}
fn parse(markdown: &str) -> Result<Vec<RootDefinition>> {
    if markdown.len() > workspace_template::MAX_TEMPLATE_BYTES {
        return Err(failure("Workspace configuration exceeds 64 KiB."));
    }
    let range = workspace_template::config_fence_range(markdown).map_err(failure)?;
    let value: Definitions =
        toml::from_str(range.map_or("", |range| &markdown[range])).map_err(io)?;
    validate_definitions(&value.roots)?;
    Ok(value.roots)
}
pub(crate) fn current_revision(owner: &ProjectStore) -> Result<Option<RevisionId>> {
    current(owner).map(|(revision, _)| revision)
}

fn current(owner: &ProjectStore) -> Result<(Option<RevisionId>, Vec<RootDefinition>)> {
    match owner.read_document_bounded(
        workspace_template::TEMPLATE_PATH,
        workspace_template::MAX_TEMPLATE_BYTES as u64,
    ) {
        Ok(document) => Ok((Some(document.revision_id), parse(&document.text)?)),
        Err(StoreError::NoActiveRevision(_)) => Ok((None, Vec::new())),
        Err(error) => Err(IpcFailure::store(error)),
    }
}
fn save(
    owner: &mut ProjectStore,
    revision: Option<RevisionId>,
    roots: &[RootDefinition],
) -> Result<()> {
    validate_definitions(roots)?;
    let base = collections::checked_base(owner, revision)?;
    let text = collections::edit_config(
        base.as_ref().map_or("", |value| value.text.as_str()),
        |document| {
            if base.is_none() {
                document["panes_enabled"] = toml_edit::value(false);
            }
            let mut tables = ArrayOfTables::new();
            for root in roots {
                let mut table = document
                    .get("roots")
                    .and_then(Item::as_array_of_tables)
                    .and_then(|tables| {
                        tables.iter().find(|table| {
                            table.get("id").and_then(Item::as_str) == Some(root.id.as_str())
                        })
                    })
                    .cloned()
                    .unwrap_or_default();
                table["id"] = toml_edit::value(&root.id);
                table["name"] = toml_edit::value(&root.name);
                table.set_position(None);
                tables.push(table);
            }
            if roots.is_empty() {
                document.remove("roots");
            } else {
                document["roots"] = Item::ArrayOfTables(tables);
            }
            Ok(())
        },
    )?;
    if parse(&text)? != roots {
        return Err(failure(
            "Close the unfinished Markdown fence before mounting a folder.",
        ));
    }
    collections::save_config(owner, base.as_ref(), text)?;
    Ok(())
}

// Feed same_file's platform file key to an explicit digest, never the randomized
// map hasher. This binds directory identity, not changing directory contents.
#[derive(Default)]
struct IdentityDigest(Sha256);
impl Hasher for IdentityDigest {
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    fn finish(&self) -> u64 {
        let digest = self.0.clone().finalize();
        u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 has eight bytes"))
    }
}
fn ordinary_directory(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(io)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(failure(
                "Workspace grants require ordinary directories without symlinks.",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            if metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                return Err(failure("Workspace grants cannot traverse reparse points."));
            }
        }
    }
    Ok(())
}
fn directory_identity(path: &Path) -> Result<String> {
    ordinary_directory(path)?;
    let handle = same_file::Handle::from_path(path).map_err(io)?;
    let mut digest = IdentityDigest::default();
    digest.write(std::env::consts::OS.as_bytes());
    handle.hash(&mut digest);
    if handle != same_file::Handle::from_path(path).map_err(io)? {
        return Err(failure("The selected folder changed."));
    }
    Ok(format!("{:x}", digest.0.finalize()))
}
fn selected(store: &ProjectStore) -> Result<Directory> {
    let path = fs::canonicalize(store.root()).map_err(io)?;
    Ok(Directory {
        identity: directory_identity(&path)?,
        path,
        project_id: store.manifest().project_id.to_string(),
    })
}
fn private_root(owner: &ProjectStore, root: &Path, create: bool) -> Result<PathBuf> {
    if !root.is_absolute()
        || root.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(failure(
            "Root grants require an absolute native application-private directory.",
        ));
    }
    // The parent is native application state, not portable workspace input.
    // Resolve its platform aliases (notably macOS /var -> /private/var) before
    // checking containment. The grant leaf itself may never be a symlink.
    let parent = fs::canonicalize(
        root.parent()
            .ok_or_else(|| failure("Invalid private grant directory."))?,
    )
    .map_err(io)?;
    ordinary_directory(&parent)?;
    let root = parent.join(
        root.file_name()
            .ok_or_else(|| failure("Invalid private grant directory."))?,
    );
    if root.starts_with(fs::canonicalize(owner.root()).map_err(io)?) {
        return Err(failure("Root grants must live outside the workspace."));
    }
    match fs::symlink_metadata(&root) {
        Ok(_) => ordinary_directory(&root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            builder.create(&root).map_err(io)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(error)),
    }
    Ok(root)
}

fn grant_path(owner: &ProjectStore, id: &str, root: &Path, create: bool) -> Result<PathBuf> {
    validate_id(id)?;
    let key = serde_json::to_vec(&(GRANT_SCHEMA, selected(owner)?, id)).map_err(io)?;
    Ok(private_root(owner, root, create)?.join(format!("{}.json", BlobId::digest(&key))))
}
fn read_file(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
        return Err(failure("Invalid or oversized workspace grant file."));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(io)?;
    let identity = same_file::Handle::from_file(file.try_clone().map_err(io)?).map_err(io)?;
    if !file.metadata().map_err(io)?.is_file() {
        return Err(failure("Workspace grant is not an ordinary file."));
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes).map_err(io)?;
    if u64::try_from(bytes.len()).map_err(io)? > maximum
        || identity != same_file::Handle::from_path(path).map_err(io)?
    {
        return Err(failure("The workspace grant changed while reading."));
    }
    Ok(bytes)
}
fn load_grant(owner: &ProjectStore, id: &str, root: &Path) -> Result<Grant> {
    let path = grant_path(owner, id, root, false)?;
    let grant: Grant = serde_json::from_slice(&read_file(&path, MAX_GRANT_BYTES)?).map_err(io)?;
    if grant.schema != GRANT_SCHEMA
        || grant.root_id != id
        || grant.owner != selected(owner)?
        || !grant.target.path.is_absolute()
    {
        return Err(failure(
            "This folder has no native access grant for this workspace.",
        ));
    }
    Ok(grant)
}
fn validate_grant(grant: &Grant) -> Result<()> {
    if fs::canonicalize(&grant.target.path).map_err(io)? != grant.target.path
        || directory_identity(&grant.target.path)? != grant.target.identity
    {
        return Err(failure(
            "The mounted folder was moved or replaced. Select it again.",
        ));
    }
    ordinary_directory(&grant.target.path.join(".loom"))?;
    let manifest: ProjectManifest = serde_json::from_slice(&read_file(
        &grant.target.path.join(".loom/project.json"),
        1_048_576,
    )?)
    .map_err(io)?;
    if manifest.project_id.to_string() != grant.target.project_id {
        return Err(failure(
            "The mounted folder's project identity changed. Select it again.",
        ));
    }
    Ok(())
}
fn write_grant(owner: &ProjectStore, root: &Path, grant: &Grant) -> Result<()> {
    let path = grant_path(owner, &grant.root_id, root, true)?;
    let bytes = serde_json::to_vec(grant).map_err(io)?;
    if u64::try_from(bytes.len()).map_err(io)? > MAX_GRANT_BYTES {
        return Err(failure("Workspace grant exceeds its size limit."));
    }
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            read_file(&path, MAX_GRANT_BYTES)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(error)),
    }
    let mut file = AtomicWriteFile::open(&path).map_err(io)?;
    file.write_all(&bytes).map_err(io)?;
    file.commit().map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(io)?;
        File::open(
            path.parent()
                .ok_or_else(|| failure("Invalid grant path."))?,
        )
        .map_err(io)?
        .sync_all()
        .map_err(io)?;
    }
    Ok(())
}
fn descriptor(definition: &RootDefinition, grant: Option<Grant>) -> RootDescriptor {
    let available = grant
        .as_ref()
        .is_some_and(|grant| validate_grant(grant).is_ok());
    RootDescriptor {
        id: definition.id.clone(),
        name: definition.name.clone(),
        owner: false,
        available,
        path: grant.as_ref().map(|grant| grant.target.path.clone()),
        project_id: grant.map(|grant| grant.target.project_id),
    }
}
fn owner_descriptor(owner: &ProjectStore) -> RootDescriptor {
    RootDescriptor {
        id: OWNER_ROOT_ID.into(),
        name: owner.manifest().name.clone(),
        owner: true,
        available: true,
        path: Some(owner.root().to_owned()),
        project_id: Some(owner.manifest().project_id.to_string()),
    }
}

pub(crate) fn list(owner: &ProjectStore, private_root: &Path) -> Result<Vec<RootDescriptor>> {
    let (_, roots) = current(owner)?;
    let mut result = vec![owner_descriptor(owner)];
    for definition in roots {
        result.push(descriptor(
            &definition,
            load_grant(owner, &definition.id, private_root).ok(),
        ));
    }
    Ok(result)
}

pub(crate) fn resolve(owner: &ProjectStore, id: &str, private_root: &Path) -> Result<PathBuf> {
    if id == OWNER_ROOT_ID {
        return Ok(owner.root().to_owned());
    }
    if !current(owner)?.1.iter().any(|root| root.id == id) {
        return Err(failure("The folder is not mounted in this workspace."));
    }
    let grant = load_grant(owner, id, private_root)?;
    validate_grant(&grant)?;
    Ok(grant.target.path)
}

/// Recheck after opening a target, before exposing any document commands.
pub(crate) fn validate_opened(
    owner: &ProjectStore,
    id: &str,
    target: &ProjectStore,
    private_root: &Path,
) -> Result<()> {
    let path = resolve(owner, id, private_root)?;
    if target.root() != path {
        return Err(failure("The opened folder differs from its grant."));
    }
    if id == OWNER_ROOT_ID {
        if selected(owner)? != selected(target)? {
            return Err(failure("The owner folder changed."));
        }
    } else if load_grant(owner, id, private_root)?.target != selected(target)? {
        return Err(failure(
            "The opened folder's identity differs from its grant.",
        ));
    }
    Ok(())
}

pub(crate) fn mount(
    owner: &mut ProjectStore,
    target: &ProjectStore,
    private_root: &Path,
    expected_revision: Option<RevisionId>,
) -> Result<RootDescriptor> {
    let _write = GRANT_WRITES
        .lock()
        .map_err(|_| failure("Workspace grant state is unavailable."))?;
    let (revision, mut roots) = current(owner)?;
    if revision != expected_revision {
        return Err(IpcFailure::new(
            "workspace_configuration_changed",
            "The workspace changed while choosing a folder. Select the folder again.",
            false,
        ));
    }
    let owner_key = selected(owner)?;
    let target_key = selected(target)?;
    let grant_directory = self::private_root(owner, private_root, false)?;
    if grant_directory.starts_with(&target_key.path) {
        return Err(failure(
            "Native root grants cannot live inside a mounted folder.",
        ));
    }
    if owner_key == target_key {
        return Ok(owner_descriptor(owner));
    }
    for definition in &roots {
        if load_grant(owner, &definition.id, private_root)
            .is_ok_and(|grant| grant.target == target_key)
        {
            return Ok(descriptor(
                definition,
                Some(load_grant(owner, &definition.id, private_root)?),
            ));
        }
    }
    if roots.len() >= MAX_ROOTS {
        return Err(failure("A workspace supports at most 32 mounted folders."));
    }
    // Refuse pending/external configuration edits before granting native access.
    collections::checked_base(owner, revision)?;
    let mut name = target.manifest().name.trim().to_owned();
    if name.is_empty()
        || name.len() > 220
        || name.chars().any(char::is_control)
        || name.contains(['/', '\\'])
    {
        name = "Folder".into();
    }
    let base_name = name.clone();
    let mut suffix = 2;
    while name == owner.manifest().name || roots.iter().any(|root| root.name == name) {
        name = format!("{base_name} ({suffix})");
        suffix += 1;
    }
    let definition = RootDefinition {
        id: format!("root-{}", CommandId::new()),
        name,
    };
    let grant = Grant {
        schema: GRANT_SCHEMA.into(),
        owner: owner_key,
        root_id: definition.id.clone(),
        target: target_key,
    };
    validate_grant(&grant)?;
    write_grant(owner, private_root, &grant)?;
    roots.push(definition.clone());
    // A failed config commit leaves only an inert private grant: resolve always
    // checks the authoritative declaration first, never a grant directory scan.
    save(owner, revision, &roots)?;
    Ok(descriptor(&definition, Some(grant)))
}

pub(crate) fn remove(owner: &mut ProjectStore, id: &str, private_root: &Path) -> Result<()> {
    if id == OWNER_ROOT_ID {
        return Err(failure("The workspace's own folder cannot be removed."));
    }
    let _write = GRANT_WRITES
        .lock()
        .map_err(|_| failure("Workspace grant state is unavailable."))?;
    let (revision, mut roots) = current(owner)?;
    let index = roots
        .iter()
        .position(|root| root.id == id)
        .ok_or_else(|| failure("The folder is not mounted in this workspace."))?;
    // Revoke the grant first. Reintroducing the same declaration later cannot
    // silently restore authority; a failed config save leaves an unavailable row.
    collections::checked_base(owner, revision)?;
    let grant = grant_path(owner, id, private_root, false)?;
    match fs::remove_file(&grant) {
        Ok(()) => {
            #[cfg(unix)]
            File::open(
                grant
                    .parent()
                    .ok_or_else(|| failure("Invalid grant path."))?,
            )
            .map_err(io)?
            .sync_all()
            .map_err(io)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(error)),
    }
    roots.remove(index);
    save(owner, revision, &roots)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use loom_document::DocumentContent;

    fn setup() -> (tempfile::TempDir, ProjectStore, ProjectStore, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap();
        let (owner, _) = ProjectStore::initialize(root.join("Writing"), "Writing").unwrap();
        let (target, _) = ProjectStore::initialize(root.join("Notes"), "Notes").unwrap();
        let private = root.join("private-grants");
        (temporary, owner, target, private)
    }
    fn copy_tree(source: &Path, target: &Path) {
        fs::create_dir(target).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let destination = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &destination);
            } else {
                fs::copy(entry.path(), destination).unwrap();
            }
        }
    }
    #[test]
    fn mounts_are_portable_names_and_removal_does_not_touch_sources() {
        let (_temporary, mut owner, target, private) = setup();
        fs::write(target.root().join("notes.md"), "Original words\n").unwrap();
        let revision = current_revision(&owner).unwrap();
        let mounted = mount(&mut owner, &target, &private, revision).unwrap();
        assert!(mounted.available);
        assert_eq!(
            resolve(&owner, &mounted.id, &private).unwrap(),
            target.root()
        );
        validate_opened(&owner, &mounted.id, &target, &private).unwrap();
        let revision = current_revision(&owner).unwrap();
        assert_eq!(
            mount(&mut owner, &target, &private, revision).unwrap().id,
            mounted.id
        );
        let config = owner
            .read_document(workspace_template::TEMPLATE_PATH)
            .unwrap()
            .text;
        assert!(config.contains("[[roots]]"));
        assert!(!config.contains(target.root().to_str().unwrap()));
        assert_eq!(
            serde_json::to_value(workspace_template::snapshot(&mut owner).unwrap()).unwrap()["enabled"],
            false
        );
        remove(&mut owner, &mounted.id, &private).unwrap();
        assert_eq!(list(&owner, &private).unwrap().len(), 1);
        assert!(resolve(&owner, &mounted.id, &private).is_err());
        assert_eq!(
            fs::read_to_string(target.root().join("notes.md")).unwrap(),
            "Original words\n"
        );
        // Restoring portable configuration cannot resurrect a removed grant.
        owner
            .save_document(
                workspace_template::TEMPLATE_PATH,
                DocumentContent::Prose(config),
                "restore declaration",
            )
            .unwrap();
        assert!(!list(&owner, &private).unwrap()[1].available);
    }
    #[test]
    fn copied_owner_and_replaced_target_do_not_inherit_native_authority() {
        let (temporary, mut owner, target, private) = setup();
        let revision = current_revision(&owner).unwrap();
        let mounted = mount(&mut owner, &target, &private, revision).unwrap();
        let owner_path = owner.root().to_owned();
        let target_path = target.root().to_owned();
        drop(owner);
        drop(target);
        let copy = fs::canonicalize(temporary.path())
            .unwrap()
            .join("Copied writing");
        copy_tree(&owner_path, &copy);
        let copied = ProjectStore::open(&copy).unwrap();
        assert!(!list(&copied, &private).unwrap()[1].available);
        assert!(resolve(&copied, &mounted.id, &private).is_err());
        let owner = ProjectStore::open(&owner_path).unwrap();
        assert!(resolve(&owner, &mounted.id, &private).is_ok());
        let previous = target_path.with_file_name("Previous notes");
        fs::rename(&target_path, &previous).unwrap();
        copy_tree(&previous, &target_path); // Same manifest identity; different directory.
        assert!(resolve(&owner, &mounted.id, &private).is_err());
        assert!(!list(&owner, &private).unwrap()[1].available);
    }
    #[test]
    fn pending_settings_never_publish_a_mount() {
        let (_temporary, mut owner, target, private) = setup();
        workspace_template::enable(&mut owner).unwrap();
        let document = owner
            .read_document(workspace_template::TEMPLATE_PATH)
            .unwrap();
        owner
            .upsert_transient_draft(
                workspace_template::TEMPLATE_PATH,
                document.revision_id,
                0,
                DocumentContent::Prose("unsaved settings".into()),
            )
            .unwrap();
        let revision = current_revision(&owner).unwrap();
        assert!(mount(&mut owner, &target, &private, revision).is_err());
        assert_eq!(list(&owner, &private).unwrap().len(), 1);
        assert!(!private.exists());
        assert_eq!(
            owner
                .read_document(workspace_template::TEMPLATE_PATH)
                .unwrap()
                .text,
            document.text
        );
    }
    #[test]
    fn stale_picker_and_bad_grant_location_preserve_configuration() {
        let (_temporary, mut owner, target, private) = setup();
        let selected_revision = current_revision(&owner).unwrap();
        owner
            .create_document_if_absent(
                workspace_template::TEMPLATE_PATH,
                DocumentContent::Prose("# My workspace\n".into()),
                "settings changed",
            )
            .unwrap();
        assert!(mount(&mut owner, &target, &private, selected_revision).is_err());
        assert!(!private.exists());
        let revision = current_revision(&owner).unwrap();
        fs::write(&private, "not a directory").unwrap();
        assert!(mount(&mut owner, &target, &private, revision).is_err());
        assert_eq!(list(&owner, &private).unwrap().len(), 1);
        assert_eq!(
            owner
                .read_document(workspace_template::TEMPLATE_PATH)
                .unwrap()
                .text,
            "# My workspace\n"
        );
    }
    #[test]
    fn trusted_parent_aliases_resolve_but_grant_leaf_aliases_do_not() {
        let (_temporary, mut owner, target, private) = setup();
        let parent = private.parent().unwrap();
        let alias = parent.join("app-data-alias");
        std::os::unix::fs::symlink(parent, &alias).unwrap();
        let alias_grants = alias.join("private-grants");
        let revision = current_revision(&owner).unwrap();
        let mounted = mount(&mut owner, &target, &alias_grants, revision).unwrap();
        assert_eq!(
            resolve(&owner, &mounted.id, &private).unwrap(),
            target.root()
        );
        assert_eq!(
            resolve(&owner, &mounted.id, &alias_grants).unwrap(),
            target.root()
        );
        let granted_alias = parent.join("grant-leaf-alias");
        std::os::unix::fs::symlink(&private, &granted_alias).unwrap();
        assert!(resolve(&owner, &mounted.id, &granted_alias).is_err());
        // An aliased parent cannot bypass the outside-workspace restriction.
        let owner_alias = parent.join("owner-alias");
        std::os::unix::fs::symlink(owner.root(), &owner_alias).unwrap();
        assert!(self::private_root(&owner, &owner_alias.join("grants"), false).is_err());
    }

    #[test]
    fn external_edits_and_unfinished_fences_are_not_overwritten() {
        let (_temporary, mut owner, target, private) = setup();
        let original = "# Settings\n~~~example\nAn unfinished fence\n";
        owner
            .create_document_if_absent(
                workspace_template::TEMPLATE_PATH,
                DocumentContent::Prose(original.into()),
                "fixture",
            )
            .unwrap();
        let revision = current_revision(&owner).unwrap();
        assert!(mount(&mut owner, &target, &private, revision).is_err());
        assert_eq!(
            owner
                .read_document(workspace_template::TEMPLATE_PATH)
                .unwrap()
                .text,
            original
        );
        fs::write(
            owner.root().join(workspace_template::TEMPLATE_PATH),
            "External settings\n",
        )
        .unwrap();
        assert!(mount(&mut owner, &target, &private, revision).is_err());
        assert_eq!(
            fs::read_to_string(owner.root().join(workspace_template::TEMPLATE_PATH)).unwrap(),
            "External settings\n"
        );
    }

    #[test]
    fn declarations_and_symlinks_are_not_grants() {
        let (_temporary, mut owner, target, private) = setup();
        let revision = current_revision(&owner).unwrap();
        let mounted = mount(&mut owner, &target, &private, revision).unwrap();
        let grant = grant_path(&owner, &mounted.id, &private, false).unwrap();
        let stolen = grant.with_extension("moved");
        fs::rename(&grant, &stolen).unwrap();
        std::os::unix::fs::symlink(&stolen, &grant).unwrap();
        assert!(resolve(&owner, &mounted.id, &private).is_err());
        assert!(!list(&owner, &private).unwrap()[1].available);
        let unsupported = format!(
            "```loom-workspace\n[[roots]]\nid={:?}\nname='Notes'\npath='/private/unselected'\n```\n",
            mounted.id
        );
        owner
            .save_document(
                workspace_template::TEMPLATE_PATH,
                DocumentContent::Prose(unsupported),
                "untrusted configuration",
            )
            .unwrap();
        assert!(list(&owner, &private).is_err());
    }
}
