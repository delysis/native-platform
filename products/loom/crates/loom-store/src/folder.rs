use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use loom_types::DocumentKind;

use crate::{ProjectStore, Result, StoreError};

const MAX_FOLDER_ENTRIES: usize = 50_000;
const MAX_FOLDER_DEPTH: usize = 64;

impl ProjectStore {
    /// Opens writing in place. Only the hidden history sidecar is created;
    /// discovered documents retain their paths and exact visible bytes.
    pub fn open_folder(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_folder_using(path.as_ref(), false, |root, name| {
            Ok(Self::initialize(root, name)?.0)
        })
    }

    /// Encrypt a newly created sidecar; open an existing project's declared
    /// storage policy without silently rewriting its history.
    pub fn open_folder_encrypted(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_folder_using(path.as_ref(), true, |root, name| {
            Ok(Self::initialize_encrypted(root, name)?.0)
        })
    }

    fn open_folder_using(
        path: &Path,
        resume_empty_initialization: bool,
        initialize: impl FnOnce(&Path, &str) -> Result<Self>,
    ) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Err(StoreError::SymbolicLink(path.to_owned()));
        }
        if !metadata.is_dir() {
            return Err(StoreError::NotDirectory(path.to_owned()));
        }
        let existing = path.join(".loom").try_exists()?;
        let resumable = existing
            && resume_empty_initialization
            && crate::store::is_initialization_skeleton(path)?;
        let mut store = if existing && !resumable {
            // A payload-bearing sidecar may contain irreplaceable drafts and
            // history. Only an empty initialization skeleton can be resumed.
            Self::open(path)?
        } else {
            let root = path.canonicalize()?;
            let name = root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Writing");
            initialize(&root, name)?
        };
        store.recover()?;
        store.discover_documents()?;
        Ok(store)
    }

    /// Registers newly discovered text files, without resurrecting tombstones
    /// or changing already registered documents. Call after filesystem hints.
    pub fn discover_documents(&mut self) -> Result<usize> {
        self.reconcile_document_lifecycle()?;
        let paths = writing_files(self.root())?;
        let registered: BTreeSet<String> = self
            .connection
            .prepare("SELECT relative_path FROM documents")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut adopted = 0;
        let mut warnings = Vec::new();
        for path in paths {
            let Some(relative) = path.to_str() else {
                return Err(StoreError::NonUtf8Path(path));
            };
            if registered.contains(relative) || self.document_path_is_reserved(relative)? {
                continue;
            }
            match self.adopt_visible_document_if_absent(
                relative,
                DocumentKind::Prose,
                "Open writing file",
            ) {
                Ok(_) => adopted += 1,
                Err(StoreError::ExternalVisibleInvalidUtf8(_)) => {
                    warnings.push(format!("{relative}: not UTF-8 text"));
                }
                Err(StoreError::DocumentTooLarge { .. }) => {
                    warnings.push(format!("{relative}: exceeds the document size limit"));
                }
                Err(error) => return Err(error),
            }
        }
        self.folder_warnings = warnings;
        Ok(adopted)
    }

    pub fn folder_warnings(&self) -> &[String] {
        &self.folder_warnings
    }
}

fn writing_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![(PathBuf::new(), 0)];
    let mut files = Vec::new();
    let mut visited = 0;
    while let Some((relative, depth)) = pending.pop() {
        for entry in fs::read_dir(root.join(&relative))? {
            let entry = entry?;
            visited += 1;
            if visited > MAX_FOLDER_ENTRIES || depth > MAX_FOLDER_DEPTH {
                return Err(StoreError::FolderTooLarge {
                    path: root.join(relative),
                    max_entries: MAX_FOLDER_ENTRIES,
                    max_depth: MAX_FOLDER_DEPTH,
                });
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') || matches!(name, "node_modules" | "target") {
                continue;
            }
            let kind = entry.file_type()?;
            let path = relative.join(name);
            if kind.is_dir() {
                pending.push((path, depth + 1));
            } else if kind.is_file() && is_writing_file(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn is_writing_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["md", "markdown", "txt"]
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

#[cfg(all(test, unix))]
mod tests {
    use loom_document::DocumentContent;

    use super::*;

    #[test]
    fn encrypted_folder_retries_denied_initial_unlock_without_replacing_lease() {
        use std::os::unix::fs::MetadataExt as _;

        let root = tempfile::tempdir().unwrap();
        let manuscript = b"Exact readable manuscript.\r\n";
        fs::write(root.path().join("Notes.md"), manuscript).unwrap();
        let denied = ProjectStore::open_folder_using(root.path(), true, |root, name| {
            Ok(
                ProjectStore::initialize_encrypted_using(root, name.into(), |_| {
                    Err(desktop_vault::VaultError::Locked)
                })?
                .0,
            )
        });
        assert!(matches!(
            denied,
            Err(StoreError::Vault(desktop_vault::VaultError::Locked))
        ));
        assert!(crate::store::is_initialization_skeleton(root.path()).unwrap());
        let lease = root.path().join(".loom/session.lock");
        let lease_inode = fs::metadata(&lease).unwrap().ino();
        let store = ProjectStore::open_folder_using(root.path(), true, |root, name| {
            Ok(
                ProjectStore::initialize_encrypted_using(root, name.into(), |root| {
                    desktop_vault::ProjectVault::initialize_with_key(root, [92; 32])
                })?
                .0,
            )
        })
        .unwrap();
        assert!(store.vault().is_some());
        assert_eq!(store.list_documents().unwrap().len(), 1);
        assert_eq!(fs::metadata(&lease).unwrap().ino(), lease_inode);
        assert_eq!(fs::read(root.path().join("Notes.md")).unwrap(), manuscript);
        drop(store);
        assert!(
            ProjectStore::open_folder_encrypted(root.path())
                .unwrap()
                .vault()
                .is_some()
        );
    }

    #[test]
    fn encrypted_folder_never_restarts_partial_or_unknown_private_payloads() {
        for relative in [
            "project.json",
            "loom.sqlite3",
            "vault.json",
            "blobs/sha256/saved",
            "backups/previous",
            "unknown",
        ] {
            let root = tempfile::tempdir().unwrap();
            let _ = ProjectStore::initialize_encrypted_using(root.path(), "Writing".into(), |_| {
                Err(desktop_vault::VaultError::Locked)
            });
            let path = root.path().join(".loom").join(relative);
            fs::write(&path, b"irreplaceable").unwrap();
            let called = std::cell::Cell::new(false);
            assert!(
                ProjectStore::open_folder_using(root.path(), true, |_, _| {
                    called.set(true);
                    panic!("must not initialize a payload-bearing sidecar")
                })
                .is_err()
            );
            assert!(!called.get());
            assert_eq!(fs::read(path).unwrap(), b"irreplaceable");
        }
    }

    #[test]
    fn resumed_encrypted_initialization_rechecks_payloads_under_the_lease() {
        let root = tempfile::tempdir().unwrap();
        let _ = ProjectStore::initialize_encrypted_using(root.path(), "Writing".into(), |_| {
            Err(desktop_vault::VaultError::Locked)
        });
        let result = ProjectStore::open_folder_using(root.path(), true, |root, name| {
            // Simulate a payload arriving after folder routing admitted the
            // skeleton but before the initializer acquired its lease.
            fs::write(root.join(".loom/loom.sqlite3"), b"do not replace")?;
            Ok(
                ProjectStore::initialize_encrypted_using(root, name.into(), |_| {
                    panic!("must reject the changed skeleton before acquiring credentials")
                })?
                .0,
            )
        });
        assert!(matches!(result, Err(StoreError::NotAProject(_))));
        assert_eq!(
            fs::read(root.path().join(".loom/loom.sqlite3")).unwrap(),
            b"do not replace"
        );
        assert!(!root.path().join(".loom/vault.json").exists());
        assert!(!root.path().join(".loom/project.json").exists());
    }

    #[test]
    fn ordinary_folder_opens_edits_and_reopens_without_rewriting_or_moving_files() {
        let root = tempfile::tempdir().unwrap();
        let original = b"# My writing\r\n\r\nExact bytes.  \r\n";
        fs::write(root.path().join("Notes.md"), original).unwrap();
        fs::create_dir(root.path().join("chapters")).unwrap();
        fs::write(root.path().join("chapters/One.txt"), "Chapter one.").unwrap();
        fs::write(root.path().join("image.png"), [0, 255, 0]).unwrap();
        let mut store = ProjectStore::open_folder(root.path()).unwrap();
        assert_eq!(store.list_documents().unwrap().len(), 2);
        assert_eq!(fs::read(root.path().join("Notes.md")).unwrap(), original);
        assert!(!root.path().join("manuscript").exists());
        let id = store.read_document("Notes.md").unwrap().document_id;
        store
            .save_document(
                "Notes.md",
                DocumentContent::Prose("Edited writing.\n".into()),
                "edit",
            )
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("Notes.md")).unwrap(),
            "Edited writing.\n"
        );
        drop(store);
        fs::write(root.path().join("Later.MD"), "Added outside Loom.").unwrap();
        let mut store = ProjectStore::open_folder(root.path()).unwrap();
        assert_eq!(store.list_documents().unwrap().len(), 3);
        assert_eq!(store.read_document("Notes.md").unwrap().document_id, id);
        let mut authority = store.open_document_file("Notes.md").unwrap();
        store.rename_document(&mut authority, "Renamed").unwrap();
        assert_eq!(store.read_document("Renamed.md").unwrap().document_id, id);
        assert!(!root.path().join("Notes.md").exists());
        let loaded = store.read_document("Renamed.md").unwrap();
        store
            .delete_document_file_idempotent(
                loom_types::CommandId::new(),
                loaded.document_id,
                loaded.revision_id,
                loaded.blob_id,
            )
            .unwrap();
        fs::write(root.path().join("Renamed.md"), "External recreation.").unwrap();
        assert_eq!(store.discover_documents().unwrap(), 0);
        assert_eq!(store.list_documents().unwrap().len(), 2);
        assert_eq!(
            fs::read_to_string(root.path().join("Renamed.md")).unwrap(),
            "External recreation."
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovery_ignores_hidden_generated_and_symlinked_content() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("Secret.md"), "private").unwrap();
        symlink(outside.path(), root.path().join("linked")).unwrap();
        symlink(
            outside.path().join("Secret.md"),
            root.path().join("Secret.md"),
        )
        .unwrap();
        for directory in [".git", "target", "node_modules"] {
            fs::create_dir(root.path().join(directory)).unwrap();
            fs::write(root.path().join(directory).join("Ignore.md"), "generated").unwrap();
        }
        fs::write(root.path().join("Visible.md"), "writing").unwrap();
        let mut store = ProjectStore::open_folder(root.path()).unwrap();
        assert_eq!(store.list_documents().unwrap().len(), 1);
        assert_eq!(store.discover_documents().unwrap(), 0);
    }

    #[test]
    fn unsupported_text_does_not_block_open_or_refresh_of_readable_writing() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("Notes.md"), "Readable writing.").unwrap();
        let invalid = [0xff, 0xfe, b'A', 0];
        fs::write(root.path().join("Export.txt"), invalid).unwrap();
        let mut store = ProjectStore::open_folder(root.path()).unwrap();
        assert_eq!(store.list_documents().unwrap().len(), 1);
        assert_eq!(store.folder_warnings(), ["Export.txt: not UTF-8 text"]);
        assert_eq!(fs::read(root.path().join("Export.txt")).unwrap(), invalid);
        fs::write(root.path().join("New.md"), "New writing.").unwrap();
        assert_eq!(store.discover_documents().unwrap(), 1);
        assert_eq!(store.list_documents().unwrap().len(), 2);
        fs::write(
            root.path().join("Export.txt"),
            "Replaced externally with UTF-8.",
        )
        .unwrap();
        assert_eq!(store.discover_documents().unwrap(), 1);
        assert!(store.folder_warnings().is_empty());
        assert_eq!(store.list_documents().unwrap().len(), 3);
    }
}
