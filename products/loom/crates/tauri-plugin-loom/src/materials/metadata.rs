//! One fixed mutable bindings file, not a general filesystem API. Callers hold
//! the material `WRITE_LOCK` and a `ProjectStore` lease. No renderer path reaches
//! this owner. The current JSON schema and immutable sources live elsewhere.
use std::path::Path;

#[cfg(unix)]
use super::{MAX_STATE_BYTES, digest};
use super::{Result, invalid};

#[cfg(unix)]
mod native {
    use std::fs::File;
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Component;

    use rustix::fs::{AtFlags, Mode, OFlags, open, openat, renameat, unlinkat};

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

    #[derive(Debug)]
    pub(crate) struct Snapshot {
        pub(crate) bytes: Vec<u8>,
        pub(crate) revision: String,
        // Retaining descriptors prevents inode reuse within a commit.
        root: File,
        sidecar: File,
        directory: File,
        file: File,
    }

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

    fn stamp(file: &File) -> Result<String> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_STATE_BYTES || metadata.nlink() != 1 {
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

    fn read_stable(file: &mut File) -> Result<(Vec<u8>, String)> {
        let before = stamp(file)?;
        let mut bytes = Vec::new();
        (&mut *file)
            .take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_STATE_BYTES || stamp(file)? != before {
            return Err(invalid("material metadata changed during read"));
        }
        Ok((bytes, before))
    }

    impl Snapshot {
        pub(crate) fn open(path: &Path) -> Result<Option<Self>> {
            // A missing project/sidecar is not an empty material collection.
            let root = directory(path)?;
            let sidecar = child(&root, ".loom", DIRECTORY)?;
            let Some(directory) = optional_child(&sidecar, "materials", DIRECTORY)? else {
                return Ok(None);
            };
            // NONBLOCK prevents a substituted FIFO/device from hanging a read.
            let Some(mut file) = optional_child(&directory, NAME, READ_FILE)? else {
                return Ok(None);
            };
            let (bytes, file_stamp) = read_stable(&mut file)?;
            let witness = format!(
                "{}:{}:{}:{file_stamp}",
                identity(&root)?,
                identity(&sidecar)?,
                identity(&directory)?
            );
            let revision = digest(&[witness.as_bytes(), b"\0", &bytes].concat());
            let value = Self {
                bytes,
                revision,
                root,
                sidecar,
                directory,
                file,
            };
            value.ensure_paths(path)?;
            Ok(Some(value))
        }

        fn ensure_paths(&self, path: &Path) -> Result<()> {
            let root = directory(path)?;
            let sidecar = child(&root, ".loom", DIRECTORY)?;
            let directory = child(&sidecar, "materials", DIRECTORY)?;
            let file = child(&directory, NAME, READ_FILE)?;
            if !same_file(&self.root, &root)?
                || !same_file(&self.sidecar, &sidecar)?
                || !same_file(&self.directory, &directory)?
                || !same_file(&self.file, &file)?
            {
                return Err(invalid("material metadata target was replaced"));
            }
            Ok(())
        }

        pub(crate) fn ensure_current(&self, root: &Path) -> Result<()> {
            self.ensure_paths(root)?;
            let current =
                Self::open(root)?.ok_or_else(|| invalid("material metadata disappeared"))?;
            if current.revision != self.revision {
                return Err(invalid("material metadata changed before commit"));
            }
            Ok(())
        }

        pub(crate) fn replace(&self, root: &Path, bytes: &[u8]) -> Result<Self> {
            self.replace_before_commit(root, bytes, || Ok(()))
        }

        fn replace_before_commit(
            &self,
            root: &Path,
            bytes: &[u8],
            before_commit: impl FnOnce() -> Result<()>,
        ) -> Result<Self> {
            if bytes.len() as u64 > MAX_STATE_BYTES {
                return Err(invalid("workspace material metadata limit reached"));
            }
            self.ensure_current(root)?;
            let temporary = format!(".bindings-{}.pending", loom_types::CommandId::new());
            let descriptor = openat(
                &self.directory,
                temporary.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(std::io::Error::from)?;
            let mut file = File::from(descriptor);
            let result = (|| {
                file.write_all(bytes)?;
                file.sync_all()?;
                before_commit()?;
                self.ensure_current(root)?;
                let mut prepared = child(&self.directory, temporary.as_str(), READ_FILE)?;
                let (prepared_bytes, _) = read_stable(&mut prepared)?;
                if !same_file(&file, &prepared)? || prepared_bytes != bytes {
                    return Err(invalid("prepared material metadata was replaced"));
                }
                // One atomic replacement in the held private directory. The
                // project lease/WRITE_LOCK serialize authorized metadata writers;
                // no promise of CAS against an uncooperative private-store writer.
                renameat(&self.directory, temporary.as_str(), &self.directory, NAME)
                    .map_err(std::io::Error::from)?;
                self.directory.sync_all()?;
                let result = Self::open(root)?
                    .ok_or_else(|| invalid("committed material metadata disappeared"))?;
                if !same_file(&file, &result.file)? || result.bytes != bytes {
                    return Err(invalid(
                        "material metadata commit needs a fresh observation",
                    ));
                }
                Ok(result)
            })();
            // Ordinary failures clean only our still-owned private staging file.
            // Unknown replacements are retained. A process crash can leave a
            // hidden orphan, which is never read as committed metadata/evidence.
            if let Ok(pending) = child(&self.directory, temporary.as_str(), READ_FILE)
                && same_file(&file, &pending).unwrap_or(false)
            {
                let _ = unlinkat(&self.directory, temporary.as_str(), AtFlags::empty());
            }
            result
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
            fs::create_dir_all(root.path().join(".loom/materials")).unwrap();
            fs::write(
                root.path().join(".loom/materials/bindings.json"),
                b"original",
            )
            .unwrap();
            root
        }

        #[test]
        fn replaced_same_bytes_and_in_place_edits_are_stale() {
            let root = fixture();
            let snapshot = Snapshot::open(root.path()).unwrap().unwrap();
            let path = root.path().join(".loom/materials/bindings.json");
            fs::rename(&path, path.with_extension("saved")).unwrap();
            fs::write(&path, b"original").unwrap();
            assert!(snapshot.replace(root.path(), b"renamed").is_err());
            assert_eq!(fs::read(&path).unwrap(), b"original");
            let snapshot = Snapshot::open(root.path()).unwrap().unwrap();
            fs::write(&path, b"external").unwrap();
            assert!(snapshot.replace(root.path(), b"renamed").is_err());
            assert_eq!(fs::read(&path).unwrap(), b"external");
        }

        #[test]
        fn commit_failure_and_racing_replacement_preserve_old_and_foreign_bytes() {
            for replace in [false, true] {
                let root = fixture();
                let snapshot = Snapshot::open(root.path()).unwrap().unwrap();
                let path = root.path().join(".loom/materials/bindings.json");
                assert!(
                    snapshot
                        .replace_before_commit(root.path(), b"renamed", || {
                            if !replace {
                                return Err(invalid("injected pre-commit failure"));
                            }
                            fs::rename(&path, path.with_extension("saved"))?;
                            fs::write(&path, b"foreign")?;
                            Ok(())
                        })
                        .is_err()
                );
                assert_eq!(
                    fs::read(path).unwrap(),
                    if replace {
                        &b"foreign"[..]
                    } else {
                        &b"original"[..]
                    }
                );
            }
        }

        #[test]
        fn symlink_and_parent_replacement_never_write_the_destination() {
            for component in [".loom", ".loom/materials", ".loom/materials/bindings.json"] {
                let root = fixture();
                let outside = fixture();
                let snapshot = Snapshot::open(root.path()).unwrap().unwrap();
                let path = root.path().join(component);
                fs::rename(&path, path.with_extension("saved")).unwrap();
                symlink(outside.path().join(component), &path).unwrap();
                assert!(snapshot.replace(root.path(), b"renamed").is_err());
                assert_eq!(
                    fs::read(outside.path().join(".loom/materials/bindings.json")).unwrap(),
                    b"original"
                );
            }
        }

        #[test]
        fn directory_swap_during_prepare_rejects_commit_without_redirecting_writes() {
            let root = fixture();
            let outside = fixture();
            let snapshot = Snapshot::open(root.path()).unwrap().unwrap();
            let parent = root.path().join(".loom/materials");
            assert!(
                snapshot
                    .replace_before_commit(root.path(), b"renamed", || {
                        fs::rename(&parent, parent.with_extension("saved"))?;
                        symlink(outside.path().join(".loom/materials"), &parent)?;
                        Ok(())
                    })
                    .is_err()
            );
            assert_eq!(
                fs::read(outside.path().join(".loom/materials/bindings.json")).unwrap(),
                b"original"
            );
            assert_eq!(
                fs::read(parent.with_extension("saved").join("bindings.json")).unwrap(),
                b"original"
            );
        }

        #[test]
        fn replaced_prepared_file_and_hardlinked_bindings_fail_closed() {
            let root = fixture();
            let snapshot = Snapshot::open(root.path()).unwrap().unwrap();
            assert!(
                snapshot
                    .replace_before_commit(root.path(), b"renamed", || {
                        for entry in fs::read_dir(root.path().join(".loom/materials"))? {
                            let entry = entry?;
                            if entry.file_name().to_string_lossy().ends_with(".pending") {
                                fs::rename(entry.path(), entry.path().with_extension("saved"))?;
                                fs::write(entry.path(), b"foreign")?;
                            }
                        }
                        Ok(())
                    })
                    .is_err()
            );
            let path = root.path().join(".loom/materials/bindings.json");
            assert_eq!(fs::read(&path).unwrap(), b"original");
            fs::hard_link(&path, root.path().join("outside-link")).unwrap();
            assert!(Snapshot::open(root.path()).is_err());
            assert_eq!(
                fs::read(root.path().join("outside-link")).unwrap(),
                b"original"
            );
        }

        #[test]
        fn commit_is_readable_on_reopen_and_old_observations_cannot_replay() {
            let root = fixture();
            let snapshot = Snapshot::open(root.path()).unwrap().unwrap();
            let committed = snapshot.replace(root.path(), b"renamed").unwrap();
            assert_ne!(snapshot.revision, committed.revision);
            assert!(snapshot.replace(root.path(), b"another name").is_err());
            drop(committed);
            assert_eq!(
                Snapshot::open(root.path()).unwrap().unwrap().bytes,
                b"renamed"
            );
        }
    }
}

#[cfg(unix)]
pub(crate) use native::Snapshot;

#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct Snapshot {
    pub(crate) bytes: Vec<u8>,
    pub(crate) revision: String,
}
#[cfg(not(unix))]
impl Snapshot {
    pub(crate) fn open(_: &Path) -> Result<Option<Self>> {
        Err(invalid(
            "material metadata mutation is unsupported on this platform",
        ))
    }
    pub(crate) fn ensure_current(&self, _: &Path) -> Result<()> {
        Err(invalid(
            "material metadata mutation is unsupported on this platform",
        ))
    }
    pub(crate) fn replace(&self, _: &Path, _: &[u8]) -> Result<Self> {
        Err(invalid(
            "material metadata mutation is unsupported on this platform",
        ))
    }
}
