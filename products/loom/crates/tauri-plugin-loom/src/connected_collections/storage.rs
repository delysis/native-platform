use super::*;
use atomic_write_file::AtomicWriteFile;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(invalid("Collection storage requires ordinary directories.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            builder.create(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}
fn root(store: &ProjectStore, id: &str) -> Result<PathBuf> {
    if !valid_collection_id(id) {
        return Err(invalid("Invalid collection ID."));
    }
    let loom = store.root().join(".loom");
    let collections = loom.join("collections");
    let root = collections.join(id);
    for dir in [&loom, &collections, &root, &root.join("snapshots")] {
        directory(dir)?;
    }
    Ok(root)
}
pub(super) fn read(path: &Path) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() > MAX_METADATA_BYTES
        || fs::symlink_metadata(path)?.file_type().is_symlink()
    {
        return Err(invalid("Unsafe or oversized collection metadata."));
    }
    let identity = same_file::Handle::from_file(file.try_clone()?)?;
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_METADATA_BYTES
        || same_file::Handle::from_path(path)? != identity
        || fs::symlink_metadata(path)?.file_type().is_symlink()
    {
        return Err(invalid("Collection metadata changed while reading."));
    }
    Ok(bytes)
}
pub(super) fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_METADATA_BYTES {
        return Err(invalid("Collection metadata limit reached."));
    }
    Ok(bytes)
}
fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}
pub(super) fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(invalid("Unsafe collection metadata destination.")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut file = AtomicWriteFile::open(path)?;
    file.write_all(bytes)?;
    file.commit()?;
    sync_parent(path)
}
pub(super) fn install(path: &Path, bytes: &[u8]) -> Result<()> {
    match read(path) {
        Ok(existing) if existing == bytes => return Ok(()),
        Ok(_) => return Err(invalid("Immutable collection leaf changed.")),
        Err(CollectionError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let temporary = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match fs::hard_link(&temporary, path) {
            Ok(()) => sync_parent(path),
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists && read(path)? == bytes =>
            {
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    })();
    let _ = fs::remove_file(temporary);
    result
}
pub(super) fn load_head(store: &ProjectStore, id: &str) -> Result<Option<CollectionHead>> {
    let path = root(store, id)?.join("head.json");
    let bytes = match read(&path) {
        Ok(bytes) => bytes,
        Err(CollectionError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let mut head: CollectionHead = serde_json::from_slice(&bytes)?;
    head.revision = digest(&bytes);
    Ok(Some(head))
}
pub(super) fn replace_head(
    store: &ProjectStore,
    mut head: CollectionHead,
) -> Result<CollectionHead> {
    validate_head(&head, &head.checkpoint.identity.collection_id)?;
    let bytes = encode(&head)?;
    let root = root(store, &head.checkpoint.identity.collection_id)?;
    // Reject a head that points to an absent or malformed immutable leaf.
    read_snapshot(
        store,
        &head.checkpoint.identity.collection_id,
        &head.snapshot_id,
    )?;
    replace(&root.join("head.json"), &bytes)?;
    head.revision = digest(&bytes);
    Ok(head)
}
pub(super) fn install_snapshot(
    store: &ProjectStore,
    snapshot: &CollectionSnapshot,
) -> Result<String> {
    let bytes = encode(snapshot)?;
    let id = digest(&bytes);
    let path = root(store, &snapshot.identity.collection_id)?
        .join("snapshots")
        .join(format!("{id}.json"));
    install(&path, &bytes)?;
    Ok(id)
}
pub(super) fn load_snapshot(
    store: &ProjectStore,
    collection_id: &str,
    id: &str,
) -> Result<CollectionSnapshot> {
    if !valid_hash(id) {
        return Err(invalid("Invalid collection snapshot ID."));
    }
    let bytes = read(
        &root(store, collection_id)?
            .join("snapshots")
            .join(format!("{id}.json")),
    )?;
    if digest(&bytes) != id {
        return Err(invalid("Collection snapshot content changed."));
    }
    let mut snapshot: CollectionSnapshot = serde_json::from_slice(&bytes)?;
    snapshot.id = id.into();
    Ok(snapshot)
}
