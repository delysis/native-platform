//! Fixed native metadata observations, never a renderer-selected filesystem API.
//! Retired binding files are refused in place through native descriptors;
//! current workspace configuration remains owned by the project store.
use std::path::Path;

#[cfg(unix)]
use super::{MAX_STATE_BYTES, digest};
use super::{Result, invalid};

#[cfg(unix)]
mod native {
    use std::fs::File;
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Component;

    use rustix::fs::{AtFlags, Mode, OFlags, open, openat, statat};

    use super::{MAX_STATE_BYTES, Path, Result, digest, invalid};

    const NAME: &str = "bindings.json";
    const DIRECTORY: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const READ_FILE: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC)
        .union(OFlags::NONBLOCK);

    fn directory(path: &Path) -> Result<File> {
        let mut components = path.components();
        if components.next() != Some(Component::RootDir) {
            return Err(invalid(
                "material metadata needs the native absolute project root",
            ));
        }
        let mut descriptor = open("/", DIRECTORY, Mode::empty()).map_err(std::io::Error::from)?;
        for component in components {
            let Component::Normal(name) = component else {
                return Err(invalid("material metadata root is not canonical"));
            };
            descriptor = openat(&descriptor, name, DIRECTORY, Mode::empty())
                .map_err(std::io::Error::from)?;
        }
        Ok(File::from(descriptor))
    }

    fn child(parent: &File, name: &str, flags: OFlags) -> Result<File> {
        Ok(File::from(
            openat(parent, name, flags, Mode::empty()).map_err(std::io::Error::from)?,
        ))
    }

    fn optional_child(parent: &File, name: &str, flags: OFlags) -> Result<Option<File>> {
        match child(parent, name, flags) {
            Ok(file) => Ok(Some(file)),
            Err(super::super::MaterialError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn identity(file: &File) -> Result<String> {
        let metadata = file.metadata()?;
        Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
    }

    fn same_file(left: &File, right: &File) -> Result<bool> {
        Ok(identity(left)? == identity(right)?)
    }

    fn stored_limit() -> u64 {
        desktop_vault::encrypted_max_len(MAX_STATE_BYTES)
    }

    fn stamp(file: &File) -> Result<String> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > stored_limit() || metadata.nlink() != 1 {
            return Err(invalid(
                "unsafe, hard-linked or oversized material bindings",
            ));
        }
        Ok(format!(
            "{}:{}:{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec()
        ))
    }

    fn read_stable_bounded(file: &mut File, maximum: u64) -> Result<(Vec<u8>, String)> {
        let before = stamp(file)?;
        let mut bytes = Vec::new();
        (&mut *file).take(maximum + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > maximum || stamp(file)? != before {
            return Err(invalid("material metadata changed during read"));
        }
        Ok((bytes, before))
    }

    /// A generation observation for the ordinary workspace document. This does
    /// not create another writer or confer authority from a content hash alone.
    #[derive(Debug)]
    pub(crate) struct WorkspaceSnapshot {
        pub(crate) bytes: Vec<u8>,
        pub(crate) revision: String,
        root: File,
        sidecar: File,
        file: File,
    }

    impl WorkspaceSnapshot {
        pub(crate) fn open(path: &Path) -> Result<Option<Self>> {
            let root = directory(path)?;
            let sidecar = child(&root, ".loom", DIRECTORY)?;
            let Some(mut file) =
                optional_child(&root, crate::workspace_template::TEMPLATE_PATH, READ_FILE)?
            else {
                return Ok(None);
            };
            let (bytes, file_stamp) = read_stable_bounded(
                &mut file,
                crate::workspace_template::MAX_TEMPLATE_BYTES as u64,
            )?;
            let witness = format!("{}:{}:{file_stamp}", identity(&root)?, identity(&sidecar)?);
            let revision = digest(
                &[
                    b"loom.workspace-materials-generation.v1\0".as_slice(),
                    witness.as_bytes(),
                    b"\0",
                    &bytes,
                ]
                .concat(),
            );
            let snapshot = Self {
                bytes,
                revision,
                root,
                sidecar,
                file,
            };
            snapshot.ensure_paths(path)?;
            Ok(Some(snapshot))
        }

        fn ensure_paths(&self, path: &Path) -> Result<()> {
            let root = directory(path)?;
            let sidecar = child(&root, ".loom", DIRECTORY)?;
            let file = child(&root, crate::workspace_template::TEMPLATE_PATH, READ_FILE)?;
            if !same_file(&self.root, &root)?
                || !same_file(&self.sidecar, &sidecar)?
                || !same_file(&self.file, &file)?
            {
                return Err(invalid("workspace material metadata target was replaced"));
            }
            Ok(())
        }

        pub(crate) fn ensure_current(&self, path: &Path) -> Result<()> {
            self.ensure_paths(path)?;
            let current = Self::open(path)?
                .ok_or_else(|| invalid("workspace material metadata disappeared"))?;
            if current.revision != self.revision {
                return Err(invalid("workspace material metadata changed before commit"));
            }
            Ok(())
        }
    }

    /// Presence is enough to refuse the retired format. Never open, decode,
    /// archive or unlink its bytes, including a substituted special file.
    pub(crate) fn refuse_legacy_bindings(path: &Path) -> Result<()> {
        let root = directory(path)?;
        let sidecar = child(&root, ".loom", DIRECTORY)?;
        let Some(directory) = optional_child(&sidecar, "materials", DIRECTORY)? else {
            return Ok(());
        };
        match statat(&directory, NAME, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Err(invalid("retired material bindings are unsupported")),
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(error) => Err(std::io::Error::from(error).into()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::fs;
        use std::os::unix::fs::symlink;

        fn fixture() -> tempfile::TempDir {
            let parent = std::env::temp_dir().canonicalize().unwrap();
            let root = tempfile::tempdir_in(parent).unwrap();
            fs::create_dir(root.path().join(".loom")).unwrap();
            root
        }

        #[test]
        fn workspace_generation_rejects_same_bytes_replacement() -> Result<()> {
            let root = fixture();
            let path = root.path().join(crate::workspace_template::TEMPLATE_PATH);
            fs::write(&path, b"# Workspace\n")?;
            let snapshot = WorkspaceSnapshot::open(root.path())?.expect("workspace exists");
            fs::rename(&path, path.with_extension("saved"))?;
            fs::write(&path, b"# Workspace\n")?;
            assert!(snapshot.ensure_current(root.path()).is_err());
            let current =
                WorkspaceSnapshot::open(root.path())?.expect("replacement workspace exists");
            assert_eq!(current.bytes, snapshot.bytes);
            assert_ne!(current.revision, snapshot.revision);
            Ok(())
        }

        #[test]
        fn workspace_generation_refuses_symlinks_and_overlong_documents() -> Result<()> {
            let root = fixture();
            let outside = fixture();
            let target = outside.path().join("untouched.md");
            fs::write(&target, b"untouched")?;
            let path = root.path().join(crate::workspace_template::TEMPLATE_PATH);
            symlink(&target, &path)?;
            assert!(WorkspaceSnapshot::open(root.path()).is_err());
            assert_eq!(fs::read(&target)?, b"untouched");
            fs::remove_file(&path)?;
            fs::write(
                &path,
                vec![b'x'; crate::workspace_template::MAX_TEMPLATE_BYTES + 1],
            )?;
            assert!(WorkspaceSnapshot::open(root.path()).is_err());
            Ok(())
        }
    }
}

#[cfg(unix)]
pub(crate) use native::{WorkspaceSnapshot, refuse_legacy_bindings};

#[cfg(not(unix))]
pub(crate) fn refuse_legacy_bindings(_: &Path) -> Result<()> {
    Err(invalid(
        "material metadata admission is unsupported on this platform",
    ))
}
