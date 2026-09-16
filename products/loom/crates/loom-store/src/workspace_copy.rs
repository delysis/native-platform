//! Explicit file copies into visible workspace folders. Originals are only read;
//! publication uses the same durable no-clobber primitive as document creation.
use std::path::Path;

use loom_types::DocumentKind;

use crate::file_io::{BoundedNoFollowFile, atomic_install_if_absent};
use crate::paths::{ensure_document_parent, normalize_document_path};
use crate::{ProjectStore, Result, StoreError};

pub struct PreparedWorkspaceCopy {
    relative_path: String,
    bytes: Vec<u8>,
    writing: bool,
}

impl std::fmt::Debug for PreparedWorkspaceCopy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedWorkspaceCopy")
            .field("relative_path", &self.relative_path)
            .field("byte_count", &self.bytes.len())
            .field("writing", &self.writing)
            .finish()
    }
}

#[derive(Debug)]
pub struct WorkspaceCopyOutcome {
    pub relative_path: String,
    pub registration_warning: Option<String>,
}

impl PreparedWorkspaceCopy {
    /// Destination is an exact project-relative filename, never a directory
    /// chosen by file contents. Read and conversion can run outside store locks.
    pub fn read(source: &Path, destination: &Path, max_bytes: u64) -> Result<Self> {
        let relative_path = normalize_document_path(destination)?;
        let mut file = BoundedNoFollowFile::open(source, max_bytes)?;
        let bytes = file.read()?;
        file.ensure_path_binding()?;
        let writing = destination
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown" | "txt")
            })
            && std::str::from_utf8(&bytes).is_ok();
        Ok(Self {
            relative_path,
            bytes,
            writing,
        })
    }

    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub const fn is_writing(&self) -> bool {
        self.writing
    }

    /// Never replace a visible file or a registered/tombstoned document. If
    /// registration fails after installation, the exact copy remains on disk
    /// and normal folder discovery can adopt it; it is never deleted on error.
    pub fn publish(self, store: &mut ProjectStore) -> Result<WorkspaceCopyOutcome> {
        if store.document_path_is_reserved(&self.relative_path)? {
            return Err(StoreError::DocumentAlreadyExists(self.relative_path));
        }
        let destination = ensure_document_parent(store.root(), &self.relative_path)?;
        if !atomic_install_if_absent(&destination, &self.bytes)? {
            return Err(StoreError::VisibleFileAlreadyExists(self.relative_path));
        }
        let registration_warning = if self.writing {
            store
                .adopt_visible_document_if_absent(
                    &self.relative_path,
                    DocumentKind::Prose,
                    "Copy file into workspace",
                )
                .err()
                .map(|error| {
                    format!("The file was copied, but could not be opened as writing: {error}")
                })
        } else {
            None
        };
        Ok(WorkspaceCopyOutcome {
            relative_path: self.relative_path,
            registration_warning,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn copies_exact_bytes_registers_writing_and_refuses_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.md");
        let bytes = "雪\r\n\r\n# Unchanged\n".as_bytes();
        fs::write(&source, bytes).unwrap();
        let mut store = ProjectStore::initialize(root.path().join("project"), "Copy")
            .unwrap()
            .0;
        let destination = Path::new("Notes/source.md");
        PreparedWorkspaceCopy::read(&source, destination, 1024)
            .unwrap()
            .publish(&mut store)
            .unwrap();
        assert_eq!(
            store.read_document(destination).unwrap().text.as_bytes(),
            bytes
        );
        assert_eq!(fs::read(&source).unwrap(), bytes);
        fs::write(&source, "Replacement").unwrap();
        assert!(
            PreparedWorkspaceCopy::read(&source, destination, 1024)
                .unwrap()
                .publish(&mut store)
                .is_err()
        );
        assert_eq!(fs::read(store.root().join(destination)).unwrap(), bytes);
        let binary = root.path().join("scan.bin");
        fs::write(&binary, [0, 255, 1]).unwrap();
        PreparedWorkspaceCopy::read(&binary, Path::new("Sources/scan.bin"), 1024)
            .unwrap()
            .publish(&mut store)
            .unwrap();
        assert_eq!(
            fs::read(store.root().join("Sources/scan.bin")).unwrap(),
            [0, 255, 1]
        );
        assert_eq!(store.list_documents().unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_oversized_sources_traversal_and_destination_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.txt");
        fs::write(&source, b"unchanged").unwrap();
        assert!(PreparedWorkspaceCopy::read(&source, Path::new("safe.txt"), 2).is_err());
        for destination in ["../escape.txt", ".loom/source.txt", "/absolute.txt"] {
            assert!(PreparedWorkspaceCopy::read(&source, Path::new(destination), 1024).is_err());
        }
        let mut store = ProjectStore::initialize(root.path().join("project"), "Copy")
            .unwrap()
            .0;
        std::os::unix::fs::symlink(root.path(), store.root().join("Linked")).unwrap();
        let prepared =
            PreparedWorkspaceCopy::read(&source, Path::new("Linked/copied.txt"), 1024).unwrap();
        assert!(prepared.publish(&mut store).is_err());
        assert!(!root.path().join("copied.txt").exists());
        let linked_source = root.path().join("link.txt");
        std::os::unix::fs::symlink(&source, &linked_source).unwrap();
        assert!(PreparedWorkspaceCopy::read(&linked_source, Path::new("copy.txt"), 1024).is_err());
    }

    #[test]
    fn registration_failure_reports_the_preserved_visible_copy() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.txt");
        fs::write(&source, "retained writing").unwrap();
        let mut store = ProjectStore::initialize(root.path().join("project"), "Copy")
            .unwrap()
            .0;
        store.connection.execute_batch("CREATE TRIGGER refuse_test_document BEFORE INSERT ON documents BEGIN SELECT RAISE(ABORT, 'test registration failure'); END;").unwrap();
        let result = PreparedWorkspaceCopy::read(&source, Path::new("source.txt"), 1024)
            .unwrap()
            .publish(&mut store)
            .unwrap();
        assert_eq!(result.relative_path, "source.txt");
        assert!(
            result
                .registration_warning
                .unwrap()
                .contains("file was copied")
        );
        assert_eq!(
            fs::read(store.root().join("source.txt")).unwrap(),
            b"retained writing"
        );
        assert_eq!(fs::read(source).unwrap(), b"retained writing");
        assert!(store.list_documents().unwrap().is_empty());
    }
}
