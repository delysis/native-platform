use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;

use atomic_write_file::AtomicWriteFile;

use crate::paths::reject_symlink_target;
use crate::{Result, StoreError};

pub(crate) fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(StoreError::NotRegularFile(path.to_path_buf()));
    }
    if metadata.len() > max_bytes {
        return Err(StoreError::DocumentTooLarge {
            actual_bytes: metadata.len(),
            max_bytes,
        });
    }

    let file = File::open(path)?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let actual_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_bytes > max_bytes {
        return Err(StoreError::DocumentTooLarge {
            actual_bytes,
            max_bytes,
        });
    }
    Ok(bytes)
}

pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_replace_with_policy(path, bytes, FilePolicy::Ordinary)
}

pub(crate) fn atomic_replace_private(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_replace_with_policy(path, bytes, FilePolicy::Private)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FilePolicy {
    Ordinary,
    Private,
}

fn atomic_replace_with_policy(path: &Path, bytes: &[u8], policy: FilePolicy) -> Result<()> {
    reject_symlink_target(path)?;
    let mut options = AtomicWriteFile::options();
    #[cfg(unix)]
    if policy == FilePolicy::Private {
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = policy;
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.commit()?;
    sync_parent(path)?;
    Ok(())
}

/// Creates an empty private file when absent and leaves an existing regular
/// file's permissions unchanged. `SQLite` can then open the file without using
/// its process-umask-derived creation mode.
pub(crate) fn create_private_file_if_absent(path: &Path) -> Result<()> {
    reject_symlink_target(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(path) {
        Ok(file) => {
            file.sync_all()?;
            sync_parent(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            reject_symlink_target(path)
        }
        Err(error) => Err(error.into()),
    }
}

/// Durably prepares bytes and installs them only if `path` is absent.
///
/// The hard-link step is the portable no-clobber primitive. The prepared file
/// lives beside the destination, so the link cannot cross filesystems.
pub(crate) fn atomic_install_if_absent(path: &Path, bytes: &[u8]) -> Result<bool> {
    reject_symlink_target(path)?;
    let temporary = create_durable_sibling(path, bytes)?;
    let installed = match hard_link_if_absent(&temporary, path) {
        Ok(installed) => installed,
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
    };
    fs::remove_file(&temporary)?;
    sync_parent(path)?;
    Ok(installed)
}

pub(crate) fn hard_link_if_absent(source: &Path, destination: &Path) -> Result<bool> {
    reject_symlink_target(destination)?;
    match fs::hard_link(source, destination) {
        Ok(()) => {
            sync_parent(destination)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn create_durable_sibling(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError::CorruptDatabase("projection path has no parent".into()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| StoreError::CorruptDatabase("projection path has no file name".into()))?;
    for _ in 0..16 {
        let temporary = parent.join(format!(
            ".{}.loom-install-{}",
            file_name.to_string_lossy(),
            loom_types::ArtifactId::new()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                    let _ = fs::remove_file(&temporary);
                    return Err(error.into());
                }
                return Ok(temporary);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(StoreError::CorruptDatabase(
        "could not allocate a unique projection staging file".into(),
    ))
}

#[cfg(unix)]
pub(crate) fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn private_atomic_files_are_owner_only_without_rewriting_existing_modes() {
        let root = tempfile::tempdir().expect("temporary directory");
        let path = root.path().join("private");

        atomic_replace_private(&path, b"first").expect("create private file");
        assert_eq!(mode(&path), 0o600);

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .expect("set deliberate existing permissions");
        atomic_replace_private(&path, b"second").expect("replace existing private file");
        assert_eq!(mode(&path), 0o640);
    }

    #[test]
    fn private_file_precreation_does_not_rewrite_an_existing_file() {
        let root = tempfile::tempdir().expect("temporary directory");
        let path = root.path().join("database");

        create_private_file_if_absent(&path).expect("create private file");
        assert_eq!(mode(&path), 0o600);

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
            .expect("set deliberate existing permissions");
        create_private_file_if_absent(&path).expect("accept existing private file");
        assert_eq!(mode(&path), 0o640);
    }
}
