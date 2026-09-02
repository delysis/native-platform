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
    volume: u64,
    index: u64,
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
        let file = open_no_follow(path)?;
        let metadata = file.metadata()?;
        validate_bounded_regular_file(path, &metadata, max_bytes)?;
        let identity = descriptor_identity(&file, &metadata)?;
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
        if descriptor_identity(&self.file, &metadata)? != self.identity {
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
        let matches = match path_matches_identity(&self.path, self.max_bytes, self.identity) {
            Ok(matches) => matches,
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::VisibleFileIdentityChanged(self.path.clone()));
            }
            Err(error) => return Err(error),
        };
        if !matches {
            return Err(StoreError::VisibleFileIdentityChanged(self.path.clone()));
        }
        Ok(())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn sync_all(&self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }

    pub(crate) fn same_identity(&self, other: &Self) -> bool {
        self.identity == other.identity
    }

    /// Reports whether `path` still names the exact regular file held by this
    /// descriptor without following a final-component symbolic link.
    pub(crate) fn path_has_identity(&self, path: &Path) -> Result<bool> {
        match path_matches_identity(path, self.max_bytes, self.identity) {
            Ok(matches) => Ok(matches),
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
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

fn open_no_follow(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    #[cfg(windows)]
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);

    match options.open(path) {
        Ok(file) => Ok(file),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
            Err(StoreError::SymbolicLink(path.to_path_buf()))
        }
        Err(error) => Err(error.into()),
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

#[cfg(not(windows))]
// This platform-specific helper intentionally shares the fallible signature of
// the Windows descriptor query so callers remain fail-closed on every target.
#[allow(clippy::unnecessary_wraps)]
fn descriptor_identity(_file: &File, metadata: &fs::Metadata) -> Result<FileIdentity> {
    Ok(metadata_identity(metadata))
}

#[cfg(windows)]
fn descriptor_identity(file: &File, _metadata: &fs::Metadata) -> Result<FileIdentity> {
    let information = winx::winapi_util::file::information(file)?;
    Ok(FileIdentity {
        volume: information.volume_serial_number(),
        index: information.file_index(),
    })
}

#[cfg(windows)]
fn path_matches_identity(path: &Path, max_bytes: u64, expected: FileIdentity) -> Result<bool> {
    let current = BoundedNoFollowFile::open(path, max_bytes)?;
    Ok(current.identity == expected)
}

#[cfg(not(windows))]
fn path_matches_identity(path: &Path, max_bytes: u64, expected: FileIdentity) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    validate_bounded_regular_file(path, &metadata, max_bytes)?;
    Ok(metadata_identity(&metadata) == expected)
}

#[cfg(not(windows))]
fn metadata_identity(metadata: &fs::Metadata) -> FileIdentity {
    #[cfg(unix)]
    let identity = FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    #[cfg(not(any(unix, windows)))]
    let identity = FileIdentity {
        length: metadata.len(),
        modified: metadata.modified().ok(),
    };
    identity
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

#[cfg(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android",
    target_os = "redox"
))]
// This cross-platform capability probe intentionally has the same fallible
// signature as the fail-closed implementation on unsupported targets.
#[allow(clippy::unnecessary_wraps)]
pub(crate) const fn ensure_document_lifecycle_supported() -> Result<()> {
    Ok(())
}

#[cfg(not(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android",
    target_os = "redox"
)))]
pub(crate) const fn ensure_document_lifecycle_supported() -> Result<()> {
    Err(StoreError::UnsupportedDocumentLifecyclePlatform)
}

/// Atomically moves the pathname currently at `source` only when
/// `destination` is absent. Callers must validate the captured file after the
/// move: the source may have been atomically replaced immediately before this
/// boundary, but that replacement is retained at the private destination and
/// is never unlinked.
#[cfg(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android",
    target_os = "redox"
))]
pub(crate) fn rename_if_absent(source: &Path, destination: &Path) -> Result<bool> {
    use rustix::fs::{RenameFlags, renameat_with};

    let source_parent = source
        .parent()
        .ok_or_else(|| StoreError::CorruptDatabase("rename source has no parent".into()))?;
    let source_name = source
        .file_name()
        .ok_or_else(|| StoreError::CorruptDatabase("rename source has no file name".into()))?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| StoreError::CorruptDatabase("rename target has no parent".into()))?;
    let destination_name = destination
        .file_name()
        .ok_or_else(|| StoreError::CorruptDatabase("rename target has no file name".into()))?;
    let source_directory = open_directory_no_follow(source_parent)?;
    let destination_directory = open_directory_no_follow(destination_parent)?;

    match renameat_with(
        &source_directory,
        source_name,
        &destination_directory,
        destination_name,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            rustix::fs::fsync(&source_directory).map_err(std::io::Error::from)?;
            rustix::fs::fsync(&destination_directory).map_err(std::io::Error::from)?;
            Ok(true)
        }
        Err(error) if error == rustix::io::Errno::EXIST => Ok(false),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

#[cfg(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android",
    target_os = "redox"
))]
pub(crate) fn sync_rename_parents(source: &Path, destination: &Path) -> Result<()> {
    let source_parent = source
        .parent()
        .ok_or_else(|| StoreError::CorruptDatabase("rename source has no parent".into()))?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| StoreError::CorruptDatabase("rename target has no parent".into()))?;
    let source_directory = open_directory_no_follow(source_parent)?;
    let destination_directory = open_directory_no_follow(destination_parent)?;
    rustix::fs::fsync(&source_directory).map_err(std::io::Error::from)?;
    rustix::fs::fsync(&destination_directory).map_err(std::io::Error::from)?;
    Ok(())
}

#[cfg(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android",
    target_os = "redox"
))]
fn open_directory_no_follow(path: &Path) -> Result<rustix::fd::OwnedFd> {
    use std::path::Component;

    use rustix::fs::{CWD, Mode, OFlags, open, openat};

    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut components = path.components();
    let mut directory = match components.next() {
        Some(Component::RootDir) => open("/", flags, Mode::empty()),
        Some(Component::Normal(component)) => openat(CWD, component, flags, Mode::empty()),
        _ => {
            return Err(StoreError::CorruptDatabase(
                "lifecycle directory path is not anchored".into(),
            ));
        }
    }
    .map_err(std::io::Error::from)?;
    for component in components {
        let Component::Normal(component) = component else {
            return Err(StoreError::CorruptDatabase(
                "lifecycle directory path is not canonical".into(),
            ));
        };
        directory =
            openat(&directory, component, flags, Mode::empty()).map_err(std::io::Error::from)?;
    }
    Ok(directory)
}

#[cfg(not(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android",
    target_os = "redox"
)))]
pub(crate) fn rename_if_absent(_source: &Path, _destination: &Path) -> Result<bool> {
    Err(StoreError::UnsupportedDocumentLifecyclePlatform)
}

#[cfg(not(any(
    target_vendor = "apple",
    target_os = "linux",
    target_os = "android",
    target_os = "redox"
)))]
pub(crate) fn sync_rename_parents(_source: &Path, _destination: &Path) -> Result<()> {
    Err(StoreError::UnsupportedDocumentLifecyclePlatform)
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
