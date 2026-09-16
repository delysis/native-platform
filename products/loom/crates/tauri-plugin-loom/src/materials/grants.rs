//! Native application-owned source grants. This file is deliberately outside
//! the workspace: a downloaded workspace cannot authorize reads on this host.
use super::*;

const GRANT_SCHEMA: &str = "loom.local-material-grants.v1";
static GRANT_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grants {
    schema: String,
    project_id: String,
    project_root: PathBuf,
    sources: BTreeMap<String, ApprovedSource>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedSource {
    path: PathBuf,
    version: String,
}

fn private_root(store: &ProjectStore, root: &Path) -> Result<PathBuf> {
    if !root.is_absolute() || root.starts_with(store.root()) {
        return Err(invalid(
            "source grants require native app-private storage outside the workspace",
        ));
    }
    // Only the leaf is created here; the native application owns its data root.
    let parent = root
        .parent()
        .ok_or_else(|| invalid("invalid app-private grant directory"))?;
    for ancestor in parent.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid("source grant directories must not be symlinks"));
        }
    }
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(invalid(
                "source grant directory is not an ordinary directory",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            builder.create(root)?;
        }
        Err(error) => return Err(error.into()),
    }
    let canonical = fs::canonicalize(root)?;
    if canonical.starts_with(fs::canonicalize(store.root())?) {
        return Err(invalid("source grants cannot live in a workspace"));
    }
    Ok(canonical)
}
fn project_key(store: &ProjectStore) -> Result<(String, PathBuf)> {
    Ok((
        store.manifest().project_id.to_string(),
        fs::canonicalize(store.root())?,
    ))
}
fn grant_path(store: &ProjectStore, root: &Path) -> Result<PathBuf> {
    let key = project_key(store)?;
    Ok(private_root(store, root)?.join(format!("{}.json", digest(&serde_json::to_vec(&key)?))))
}
fn load(store: &ProjectStore, root: &Path) -> Result<Grants> {
    let (project_id, project_root) = project_key(store)?;
    let bytes = match read_safe(&grant_path(store, root)?, MAX_STATE_BYTES) {
        Ok(bytes) => bytes,
        Err(MaterialError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Grants {
                schema: GRANT_SCHEMA.into(),
                project_id,
                project_root,
                sources: BTreeMap::new(),
            });
        }
        Err(error) => return Err(error),
    };
    let grants: Grants = serde_json::from_slice(&bytes)?;
    if grants.schema != GRANT_SCHEMA
        || grants.project_id != project_id
        || grants.project_root != project_root
        || grants.sources.len() > MAX_BINDINGS
    {
        return Err(invalid(
            "local source grants do not belong to this workspace",
        ));
    }
    for (id, source) in &grants.sources {
        if !source.path.is_absolute()
            || binding_id(&Source::Library {
                path: source.path.clone(),
            })? != *id
            || !source.version.starts_with("local-file-identity-v1:")
        {
            return Err(invalid("invalid local source grant"));
        }
    }
    Ok(grants)
}
fn save(store: &ProjectStore, root: &Path, grants: &Grants) -> Result<()> {
    let path = grant_path(store, root)?;
    let bytes = serde_json::to_vec(grants)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid("local source grant limit reached"));
    }
    if path.try_exists()? {
        let _ = read_safe(&path, MAX_STATE_BYTES)?;
    }
    let mut file = AtomicWriteFile::open(&path)?;
    file.write_all(&bytes)?;
    file.commit()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
    }
    Ok(())
}

pub(crate) fn persist_selected_grant(store: &ProjectStore, root: &Path, id: &str) -> Result<()> {
    let _lock = GRANT_WRITE_LOCK
        .lock()
        .map_err(|_| invalid("local grant write lock poisoned"))?;
    let granted = grants()
        .lock()
        .map_err(|_| invalid("library capability lock poisoned"))?
        .get(&grant_key(store, id))
        .cloned()
        .ok_or_else(|| MaterialError::NeedsAuthorization(id.into()))?;
    let selected = binding(store, id)?;
    if !matches!(&selected.source, Source::Library { path } if *path == granted.path)
        || file_version(&granted.path)? != granted.version
    {
        return Err(invalid(
            "selected source changed before its access grant was saved",
        ));
    }
    let mut approved = load(store, root)?;
    approved.sources.insert(
        id.into(),
        ApprovedSource {
            path: granted.path.clone(),
            version: granted.version.clone(),
        },
    );
    save(store, root, &approved)
}
pub(crate) fn restore_selected_grants(store: &ProjectStore, root: &Path) -> Result<()> {
    let approved = load(store, root)?;
    let bindings = read_bindings(store)?;
    for binding in &bindings.items {
        let Some(source) = approved.sources.get(&binding.id) else {
            continue;
        };
        if !matches!(&binding.source, Source::Library { path } if *path == source.path) {
            continue;
        }
        if grants()
            .lock()
            .map_err(|_| invalid("library capability lock poisoned"))?
            .contains_key(&grant_key(store, &binding.id))
        {
            continue;
        }
        // An absent, replaced, or modified file needs explicit re-selection.
        // Never copy, checkpoint, rewrite, or repair the external database here.
        if !file_version(&source.path).is_ok_and(|version| version == source.version) {
            continue;
        }
        let restored = add_library(store, &source.path, Some(&binding.name));
        if restored.is_ok()
            && !file_version(&source.path).is_ok_and(|version| version == source.version)
        {
            grants()
                .lock()
                .map_err(|_| invalid("library capability lock poisoned"))?
                .remove(&grant_key(store, &binding.id));
        }
    }
    Ok(())
}
pub(crate) fn forget_selected_grant(store: &ProjectStore, root: &Path, id: &str) -> Result<()> {
    let _lock = GRANT_WRITE_LOCK
        .lock()
        .map_err(|_| invalid("local grant write lock poisoned"))?;
    let mut approved = load(store, root)?;
    approved.sources.remove(id);
    save(store, root, &approved)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_private_grant_write_rolls_back_binding_and_capability() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let (store, _) = ProjectStore::initialize(root.join("Writing"), "Writing").unwrap();
        let path = root.join("library.sqlite3");
        super::super::tests::database(&path);
        let before = fs::read(&path).unwrap();
        let bad_root = root.join("grants");
        fs::write(&bad_root, b"not a directory").unwrap();
        assert!(add_library_persisted(&store, &path, Some(&bad_root)).is_err());
        assert!(list(&store).unwrap().is_empty());
        let id = binding_id(&Source::Library { path: path.clone() }).unwrap();
        assert!(
            !grants()
                .lock()
                .unwrap()
                .contains_key(&grant_key(&store, &id))
        );
        assert_eq!(fs::read(path).unwrap(), before);
    }
    #[test]
    fn private_grant_restores_only_the_same_selected_source_version() {
        let temp = tempfile::tempdir().unwrap();
        // macOS temp paths may traverse /var -> /private/var; the application
        // grants root itself must be canonical and ordinary.
        let temp_root = fs::canonicalize(temp.path()).unwrap();
        let (store, _) = ProjectStore::initialize(temp_root.join("Writing"), "Writing").unwrap();
        let path = temp_root.join("library.sqlite3");
        super::super::tests::database(&path);
        let root = temp_root.join("grants");
        let entry = add_library(&store, &path, None).unwrap();
        persist_selected_grant(&store, &root, &entry.id).unwrap();
        grants()
            .lock()
            .unwrap()
            .remove(&grant_key(&store, &entry.id));
        restore_selected_grants(&store, &root).unwrap();
        assert!(list(&store).unwrap()[0].available);
        forget_selected_grant(&store, &root, &entry.id).unwrap();
        grants()
            .lock()
            .unwrap()
            .remove(&grant_key(&store, &entry.id));
        restore_selected_grants(&store, &root).unwrap();
        assert!(!list(&store).unwrap()[0].available);
        assert!(persist_selected_grant(&store, &root, &entry.id).is_err());
        assert!(restore_selected_grants(&store, &store.root().join("untrusted-grants")).is_err());
    }
}
