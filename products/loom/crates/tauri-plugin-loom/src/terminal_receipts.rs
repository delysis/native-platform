//! Immutable JSON receipts, published only after their complete bytes are durable.

use std::path::{Path, PathBuf};

use super::IpcFailure;

const MAX_BYTES: usize = 512 * 1024;

pub(super) fn directory(root: &Path) -> Result<PathBuf, IpcFailure> {
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    return Ok(unix::Directory::open(root)?.path);
    #[cfg(not(all(unix, not(any(target_os = "redox", target_os = "espidf")))))]
    {
        let _ = root;
        Err(failure("Receipt storage is unsupported on this platform."))
    }
}

pub(super) fn write(root: &Path, id: &str, finished: bool, bytes: &[u8]) -> Result<(), IpcFailure> {
    let name = file_name(id, finished)?;
    validate_bytes(bytes)?;
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    return unix::write(root, &name, bytes, || Ok(()));
    #[cfg(not(all(unix, not(any(target_os = "redox", target_os = "espidf")))))]
    {
        let _ = (root, name);
        Err(failure("Receipt storage is unsupported on this platform."))
    }
}

pub(super) fn read(root: &Path, id: &str, finished: bool) -> Result<Option<Vec<u8>>, IpcFailure> {
    let name = file_name(id, finished)?;
    #[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
    return unix::Directory::open(root)?.read(&name);
    #[cfg(not(all(unix, not(any(target_os = "redox", target_os = "espidf")))))]
    {
        let _ = (root, name);
        Err(failure("Receipt storage is unsupported on this platform."))
    }
}

fn file_name(id: &str, finished: bool) -> Result<String, IpcFailure> {
    let parsed: loom_types::CommandId = id
        .parse()
        .map_err(|_| failure("Invalid receipt identifier."))?;
    if parsed.to_string() != id {
        return Err(failure(
            "Receipt identifiers must use canonical ULID spelling.",
        ));
    }
    Ok(format!(
        "{id}.{}.json",
        if finished { "finished" } else { "started" }
    ))
}

fn validate_bytes(bytes: &[u8]) -> Result<(), IpcFailure> {
    if bytes.len() > MAX_BYTES {
        return Err(failure("Function receipt exceeds 512 KiB."));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| failure("Function receipt is not valid JSON."))?;
    if !value.is_object() {
        return Err(failure("Function receipt must be a JSON object."));
    }
    Ok(())
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("terminal_receipt_invalid", message, false)
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
mod unix {
    use std::fs::File;
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::MetadataExt as _;

    use rustix::fs::{AtFlags, Mode, OFlags, linkat, mkdirat, open, openat, unlinkat};
    use rustix::io::Errno;

    use super::{IpcFailure, MAX_BYTES, Path, PathBuf, failure, validate_bytes};

    const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    const READ_FLAGS: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::CLOEXEC);

    #[derive(Debug)]
    pub(super) struct Directory {
        root: File,
        metadata: File,
        runs: File,
        pub(super) path: PathBuf,
    }

    impl Directory {
        pub(super) fn open(root: &Path) -> Result<Self, IpcFailure> {
            // Root is the already-open project's authority. Each private child
            // is opened relative to a retained descriptor, never a checked path.
            let root_file =
                File::from(open(root, DIRECTORY_FLAGS, Mode::empty()).map_err(io_failure)?);
            let metadata = File::from(
                openat(&root_file, ".loom", DIRECTORY_FLAGS, Mode::empty()).map_err(io_failure)?,
            );
            match mkdirat(&metadata, "function-runs", Mode::RWXU) {
                Ok(()) => metadata.sync_all().map_err(io_failure)?,
                Err(Errno::EXIST) => {}
                Err(error) => return Err(io_failure(error)),
            }
            let runs = File::from(
                openat(&metadata, "function-runs", DIRECTORY_FLAGS, Mode::empty())
                    .map_err(io_failure)?,
            );
            let directory = Self {
                root: root_file,
                metadata,
                runs,
                path: root.join(".loom/function-runs"),
            };
            directory.ensure_binding()?;
            Ok(directory)
        }

        fn ensure_binding(&self) -> Result<(), IpcFailure> {
            let metadata = File::from(
                openat(&self.root, ".loom", DIRECTORY_FLAGS, Mode::empty()).map_err(io_failure)?,
            );
            let runs = File::from(
                openat(&metadata, "function-runs", DIRECTORY_FLAGS, Mode::empty())
                    .map_err(io_failure)?,
            );
            if !same_file(&metadata, &self.metadata)? || !same_file(&runs, &self.runs)? {
                return Err(failure(
                    "Function receipt directory changed during the operation.",
                ));
            }
            Ok(())
        }

        pub(super) fn read(&self, name: &str) -> Result<Option<Vec<u8>>, IpcFailure> {
            self.ensure_binding()?;
            let file = match openat(&self.runs, name, READ_FLAGS, Mode::empty()) {
                Ok(file) => File::from(file),
                Err(Errno::NOENT) => return Ok(None),
                Err(error) => return Err(io_failure(error)),
            };
            let metadata = file.metadata().map_err(io_failure)?;
            if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
                return Err(failure("Function receipt is not a bounded regular file."));
            }
            let mut bytes = Vec::new();
            (&file)
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(io_failure)?;
            validate_bytes(&bytes)?;
            let current = File::from(
                openat(&self.runs, name, READ_FLAGS, Mode::empty()).map_err(io_failure)?,
            );
            if !same_file(&file, &current)? {
                return Err(failure("Function receipt changed during the read."));
            }
            self.ensure_binding()?;
            Ok(Some(bytes))
        }
    }

    pub(super) fn write(
        root: &Path,
        name: &str,
        bytes: &[u8],
        before_publish: impl FnOnce() -> Result<(), IpcFailure>,
    ) -> Result<(), IpcFailure> {
        let directory = Directory::open(root)?;
        if let Some(existing) = directory.read(name)? {
            return verify_collision(&directory, &existing, bytes);
        }
        let temporary = format!(".pending-{}", loom_types::CommandId::new());
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut file = File::from(
            openat(
                &directory.runs,
                temporary.as_str(),
                flags,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(io_failure)?,
        );
        let result = (|| {
            file.write_all(bytes).map_err(io_failure)?;
            file.sync_all().map_err(io_failure)?;
            before_publish()?;
            directory.ensure_binding()?;
            match linkat(
                &directory.runs,
                temporary.as_str(),
                &directory.runs,
                name,
                AtFlags::empty(),
            ) {
                Ok(()) => Ok(()),
                Err(Errno::EXIST) => {
                    let existing = directory.read(name)?.ok_or_else(|| {
                        failure("Function receipt disappeared during publication.")
                    })?;
                    verify_collision(&directory, &existing, bytes)
                }
                Err(error) => Err(io_failure(error)),
            }
        })();
        // A crash before publication can leave only a hidden staging file. A
        // crash after publication leaves a complete receipt. Cleanup failure
        // must not invalidate a successful, immutable publication.
        let _ = unlinkat(&directory.runs, temporary.as_str(), AtFlags::empty());
        directory.runs.sync_all().map_err(io_failure)?;
        result?;
        directory.ensure_binding()
    }

    fn verify_collision(
        directory: &Directory,
        existing: &[u8],
        expected: &[u8],
    ) -> Result<(), IpcFailure> {
        if existing != expected {
            return Err(failure(
                "An immutable function receipt already exists with different bytes.",
            ));
        }
        directory.runs.sync_all().map_err(io_failure)
    }

    fn same_file(left: &File, right: &File) -> Result<bool, IpcFailure> {
        let left = left.metadata().map_err(io_failure)?;
        let right = right.metadata().map_err(io_failure)?;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }

    fn io_failure(error: impl std::fmt::Display) -> IpcFailure {
        failure(format!("Function receipt storage failed: {error}"))
    }
}

#[cfg(all(test, unix, not(any(target_os = "redox", target_os = "espidf"))))]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("project root");
        fs::create_dir(root.path().join(".loom")).expect("project metadata");
        root
    }

    #[test]
    fn receipts_are_atomic_idempotent_and_never_clobbered() {
        let root = fixture();
        let id = loom_types::CommandId::new().to_string();
        let bytes = br#"{"status":"running"}"#;
        write(root.path(), &id, false, bytes).expect("publish");
        write(root.path(), &id, false, bytes).expect("identical retry");
        assert!(write(root.path(), &id, false, br#"{"status":"different"}"#).is_err());
        assert_eq!(
            read(root.path(), &id, false).expect("read").as_deref(),
            Some(bytes.as_slice())
        );
        assert!(
            read(root.path(), &id, true)
                .expect("unpublished phase")
                .is_none()
        );
        assert_eq!(
            fs::read_dir(directory(root.path()).expect("directory"))
                .expect("list")
                .count(),
            1
        );
    }

    #[test]
    fn interrupted_staging_never_exposes_a_partial_receipt() {
        let root = fixture();
        let id = loom_types::CommandId::new().to_string();
        let name = file_name(&id, false).expect("receipt name");
        let result = unix::write(root.path(), &name, br#"{"complete":true}"#, || {
            Err(failure("injected publication failure"))
        });
        assert!(result.is_err());
        assert!(
            read(root.path(), &id, false)
                .expect("no partial receipt")
                .is_none()
        );
        assert_eq!(
            fs::read_dir(directory(root.path()).expect("directory"))
                .expect("list")
                .count(),
            0
        );
        write(root.path(), &id, false, br#"{"complete":true}"#).expect("retry after failure");
    }

    #[test]
    fn replacing_the_directory_during_staging_cannot_redirect_publication() {
        let root = fixture();
        let outside = tempfile::tempdir().expect("outside directory");
        let id = loom_types::CommandId::new().to_string();
        let name = file_name(&id, false).expect("receipt name");
        let runs = directory(root.path()).expect("runs directory");
        let retained = root.path().join("retained-runs");
        let result = unix::write(root.path(), &name, b"{}", || {
            fs::rename(&runs, &retained).expect("replace directory after staging");
            symlink(outside.path(), &runs).expect("redirect visible directory");
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read_dir(outside.path())
                .expect("outside untouched")
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(retained)
                .expect("anchored staging cleaned")
                .count(),
            0
        );
    }

    #[test]
    fn invalid_or_oversized_bytes_cannot_be_published_or_read() {
        let root = fixture();
        let id = loom_types::CommandId::new().to_string();
        assert!(write(root.path(), &id, false, b"{partial").is_err());
        assert!(write(root.path(), &id, false, &vec![b' '; MAX_BYTES + 1]).is_err());
        assert!(write(root.path(), "../escape", false, b"{}").is_err());
        let path = directory(root.path())
            .expect("directory")
            .join(file_name(&id, false).expect("name"));
        fs::write(&path, b"{old partial").expect("malformed existing receipt");
        assert!(read(root.path(), &id, false).is_err());
        assert!(write(root.path(), &id, false, b"{}").is_err());
        assert_eq!(fs::read(path).expect("original bytes"), b"{old partial");
    }

    #[test]
    fn symlink_receipts_and_private_directories_are_rejected() {
        let root = fixture();
        let outside = tempfile::tempdir().expect("outside directory");
        let id = loom_types::CommandId::new().to_string();
        let outside_file = outside.path().join("receipt.json");
        fs::write(&outside_file, b"{}").expect("outside receipt");
        let runs = directory(root.path()).expect("directory");
        let receipt = runs.join(file_name(&id, false).expect("name"));
        symlink(&outside_file, &receipt).expect("receipt symlink");
        assert!(read(root.path(), &id, false).is_err());
        assert!(write(root.path(), &id, false, b"{}").is_err());
        fs::remove_file(receipt).expect("remove symlink");
        fs::remove_dir(&runs).expect("remove runs directory");
        symlink(outside.path(), &runs).expect("directory symlink");
        assert!(directory(root.path()).is_err());
        fs::remove_file(runs).expect("remove directory symlink");
        fs::remove_dir(root.path().join(".loom")).expect("remove metadata");
        symlink(outside.path(), root.path().join(".loom")).expect("metadata symlink");
        assert!(directory(root.path()).is_err());
        assert_eq!(fs::read(outside_file).expect("outside unchanged"), b"{}");
    }
}
