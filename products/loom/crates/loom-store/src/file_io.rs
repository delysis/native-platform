use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(unix)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(windows)]
struct FileIdentity {
    volume: Option<u32>,
    index: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(not(any(unix, windows)))]
struct FileIdentity {
    length: u64,
    modified: Option<std::time::SystemTime>,
}

/// One bounded, no-follow descriptor for a visible file. Its descriptor is
/// retained so callers can read, hash, and validate one object instead of
/// inspecting a path and then reopening whatever the path names later.
#[derive(Debug)]
pub(crate) struct BoundedNoFollowFile {
    file: File,
    path: PathBuf,
    identity: FileIdentity,
    max_bytes: u64,
}

impl BoundedNoFollowFile {
    pub(crate) fn open(path: &Path, max_bytes: u64) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        #[cfg(windows)]
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);

        let file = match options.open(path) {
            Ok(file) => file,
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                return Err(StoreError::SymbolicLink(path.to_path_buf()));
            }
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        validate_bounded_regular_file(path, &metadata, max_bytes)?;
        let identity = file_identity(&metadata);
        Ok(Self {
            file,
            path: path.to_path_buf(),
            identity,
            max_bytes,
        })
    }

    pub(crate) fn read(&mut self) -> Result<Vec<u8>> {
        let metadata = self.file.metadata()?;
        validate_bounded_regular_file(&self.path, &metadata, self.max_bytes)?;
        if file_identity(&metadata) != self.identity {
            return Err(StoreError::VisibleFileIdentityChanged(self.path.clone()));
        }

        self.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        Read::by_ref(&mut self.file)
            .take(self.max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        let actual_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if actual_bytes > self.max_bytes {
            return Err(StoreError::DocumentTooLarge {
                actual_bytes,
                max_bytes: self.max_bytes,
            });
        }
        Ok(bytes)
    }

    /// Confirms that the original store-derived path still names the opened
    /// regular file without following a final-component symbolic link.
    pub(crate) fn ensure_path_binding(&self) -> Result<()> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::VisibleFileIdentityChanged(self.path.clone()));
            }
            Err(error) => return Err(error.into()),
        };
        validate_bounded_regular_file(&self.path, &metadata, self.max_bytes)?;
        if file_identity(&metadata) != self.identity {
            return Err(StoreError::VisibleFileIdentityChanged(self.path.clone()));
        }
        Ok(())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Resolves the current native path from the retained descriptor rather
    /// than reopening the store path. Reveal uses this path only while the
    /// descriptor remains alive and path-bound.
    #[cfg(target_vendor = "apple")]
    pub(crate) fn descriptor_path(&self) -> Result<PathBuf> {
        use std::os::unix::ffi::OsStringExt as _;

        let path = rustix::fs::getpath(&self.file).map_err(std::io::Error::from)?;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(
            path.into_bytes(),
        )))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn descriptor_path(&self) -> Result<PathBuf> {
        use std::os::fd::AsRawFd as _;

        Ok(fs::read_link(format!(
            "/proc/self/fd/{}",
            self.file.as_raw_fd()
        ))?)
    }

    #[cfg(windows)]
    pub(crate) fn descriptor_path(&self) -> Result<PathBuf> {
        Ok(winx::file::get_file_path(&self.file)?)
    }

    #[cfg(not(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        windows
    )))]
    pub(crate) fn descriptor_path(&self) -> Result<PathBuf> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "descriptor-derived reveal paths are unavailable on this platform",
        )
        .into())
    }
}

fn validate_bounded_regular_file(
    path: &Path,
    metadata: &fs::Metadata,
    max_bytes: u64,
) -> Result<()> {
    #[cfg(windows)]
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(StoreError::SymbolicLink(path.to_path_buf()));
    }
    #[cfg(windows)]
    if metadata.volume_serial_number().is_none() || metadata.file_index().is_none() {
        return Err(
            std::io::Error::other("the filesystem did not report a stable file identity").into(),
        );
    }
    if metadata.file_type().is_symlink() {
        return Err(StoreError::SymbolicLink(path.to_path_buf()));
    }
    if !metadata.is_file() {
        return Err(StoreError::NotRegularFile(path.to_path_buf()));
    }
    if metadata.len() > max_bytes {
        return Err(StoreError::DocumentTooLarge {
            actual_bytes: metadata.len(),
            max_bytes,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(windows)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        volume: metadata.volume_serial_number(),
        index: metadata.file_index(),
    }
}

#[cfg(not(any(unix, windows)))]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        length: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

/// Reads a regular file through one descriptor without following a
/// final-component symbolic link and rejects replacement before return.
pub(crate) fn read_bounded_no_follow(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let mut opened = BoundedNoFollowFile::open(path, max_bytes)?;
    let bytes = opened.read()?;
    opened.ensure_path_binding()?;
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
    let options = AtomicWriteFile::options();
    #[cfg(unix)]
    let options = {
        let mut options = options;
        if policy == FilePolicy::Private {
            options.mode(0o600);
        }
        options
    };
    #[cfg(not(unix))]
    let options = {
        let _ = policy;
        options
    };
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
pub(crate) fn sync_parent(path: &Path) -> Result<()> {
    // Rust does not expose a portable directory-flush primitive on Windows.
    // Still validate the parent at the durability boundary so disappearance or
    // replacement is reported instead of silently treating the boundary as
    // infallible.
    if let Some(parent) = path.parent() {
        fs::metadata(parent)?;
    }
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
