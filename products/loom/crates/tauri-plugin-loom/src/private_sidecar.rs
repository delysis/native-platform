//! Private payload encoding only. Callers retain their publication and identity
//! protocols; namespaces bind ciphertext to its project-relative destination.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _};
use std::path::{Component, Path};

use desktop_vault::ProjectVault;
use same_file::Handle;

pub(crate) struct PayloadCodec {
    vault: Option<ProjectVault>,
}

impl std::fmt::Debug for PayloadCodec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PayloadCodec")
            .field("encrypted", &self.vault.is_some())
            .finish()
    }
}

impl PayloadCodec {
    pub(crate) fn open(root: &Path) -> io::Result<Self> {
        Ok(Self {
            vault: ProjectVault::open(root).map_err(io::Error::other)?,
        })
    }

    pub(crate) fn encrypted(&self) -> bool {
        self.vault.is_some()
    }

    pub(crate) fn stored_limit(&self, maximum: u64) -> u64 {
        if self.encrypted() {
            desktop_vault::encrypted_max_len(maximum)
        } else {
            maximum
        }
    }

    pub(crate) fn seal(&self, namespace: &str, bytes: &[u8]) -> io::Result<Vec<u8>> {
        match &self.vault {
            Some(vault) => vault.seal(namespace, bytes).map_err(io::Error::other),
            None => Ok(bytes.to_vec()),
        }
    }

    pub(crate) fn open_bytes(
        &self,
        namespace: &str,
        bytes: &[u8],
        maximum: u64,
    ) -> io::Result<Vec<u8>> {
        match &self.vault {
            Some(vault) => vault
                .open_bytes(namespace, bytes, maximum)
                .map_err(io::Error::other),
            None if bytes.len() as u64 <= maximum => Ok(bytes.to_vec()),
            None => Err(invalid("Private payload exceeds its byte limit.")),
        }
    }
}

pub(crate) fn namespace(root: &Path, path: &Path) -> io::Result<String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| invalid("Private payload is outside the project."))?;
    let names = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => name
                .to_str()
                .ok_or_else(|| invalid("Private payload path is not UTF-8.")),
            _ => Err(invalid("Private payload path is not canonical.")),
        })
        .collect::<io::Result<Vec<_>>>()?;
    if names.len() < 2 || names[0] != ".loom" {
        return Err(invalid("Private payload must be inside .loom."));
    }
    Ok(names.join("/"))
}

/// Read only a bounded ordinary file; authenticate before returning any bytes
/// to a JSON parser, media decoder, or content-hash check.
pub(crate) fn read(root: &Path, path: &Path, maximum: u64) -> io::Result<Option<Vec<u8>>> {
    let namespace = namespace(root, path)?;
    let codec = PayloadCodec::open(root)?;
    let file = match open_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    let limit = codec.stored_limit(maximum);
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid("Private payload is not a bounded ordinary file."));
    }
    let identity = Handle::from_file(file.try_clone()?)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit
        || fs::symlink_metadata(path)?.file_type().is_symlink()
        || Handle::from_path(path)? != identity
    {
        return Err(invalid("Private payload changed during the read."));
    }
    codec.open_bytes(&namespace, &bytes, maximum).map(Some)
}

fn open_file(path: &Path) -> io::Result<File> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid("Private payload is a symbolic link."));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        if fs::symlink_metadata(path)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid("Private payload is a reparse point."));
        }
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(secured: bool) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".loom")).unwrap();
        if secured {
            ProjectVault::initialize_with_key(root.path(), [17; 32]).unwrap();
        }
        root
    }

    #[test]
    #[cfg(unix)]
    fn secured_payloads_reopen_exactly_and_reject_tamper_rebinding_and_plaintext() {
        let root = project(true);
        let path = root.path().join(".loom/context.json");
        let name = namespace(root.path(), &path).unwrap();
        let plaintext = b"private source: a clock beneath the sea";
        let bytes = PayloadCodec::open(root.path())
            .unwrap()
            .seal(&name, plaintext)
            .unwrap();
        assert!(
            !bytes
                .windows(plaintext.len())
                .any(|window| window == plaintext)
        );
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            read(root.path(), &path, plaintext.len() as u64)
                .unwrap()
                .unwrap(),
            plaintext
        );
        assert!(read(root.path(), &path, plaintext.len() as u64 - 1).is_err());

        let other = root.path().join(".loom/other.json");
        fs::write(&other, &bytes).unwrap();
        assert!(read(root.path(), &other, 4096).is_err());
        let foreign = project(true);
        let foreign_path = foreign.path().join(".loom/context.json");
        fs::write(&foreign_path, &bytes).unwrap();
        assert!(read(foreign.path(), &foreign_path, 4096).is_err());

        let mut changed = bytes.clone();
        *changed.last_mut().unwrap() ^= 1;
        fs::write(&path, changed).unwrap();
        assert!(read(root.path(), &path, 4096).is_err());
        fs::write(&path, plaintext).unwrap();
        assert!(read(root.path(), &path, 4096).is_err());
        fs::write(&path, &bytes).unwrap();
        fs::remove_file(root.path().join(".loom/vault.json")).unwrap();
        assert!(read(root.path(), &path, 4096).is_err());
    }

    #[test]
    fn ordinary_projects_keep_exact_bytes_and_enforce_bounds_and_private_paths() {
        let root = project(false);
        let path = root.path().join(".loom/context.json");
        let bytes = b"ordinary existing source";
        let codec = PayloadCodec::open(root.path()).unwrap();
        assert!(!codec.encrypted());
        assert_eq!(codec.seal(".loom/context.json", bytes).unwrap(), bytes);
        fs::write(&path, bytes).unwrap();
        assert_eq!(
            read(root.path(), &path, bytes.len() as u64)
                .unwrap()
                .unwrap(),
            bytes
        );
        assert!(read(root.path(), &path, bytes.len() as u64 - 1).is_err());
        assert!(namespace(root.path(), &root.path().join(".loom/../outside")).is_err());
        assert!(namespace(root.path(), &root.path().join("manuscript.md")).is_err());
        #[cfg(unix)]
        {
            let link = root.path().join(".loom/link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read(root.path(), &link, 4096).is_err());
        }
    }
}
