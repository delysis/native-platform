use std::fs;
use std::path::{Path, PathBuf};

use desktop_vault::ProjectVault;
use loom_document::DocumentContent;
use loom_types::BlobId;
use rusqlite::Connection;

use crate::{ProjectStore, StoreError};

const WRAPPING_KEY: [u8; 32] = [73; 32];
const PUBLIC_TEXT: &str = "ordinary readable manuscript café\r\n";
const PRIVATE_DRAFT: &str = "private unsent draft 8179 4e9c café\r\n";
const PRIVATE_REASON: &str = "private revision metadata 73b2 960f";
const PRIVATE_NAME: &str = "private project name 2f39 8421";
const PRIVATE_EVIDENCE: &[u8] = b"private generation evidence f8c0 413d";

fn encrypted_fixture() -> (tempfile::TempDir, ProjectStore) {
    let directory = tempfile::tempdir().expect("fixture directory");
    crate::paths::ensure_private_directory(&directory.path().join(".loom"))
        .expect("private fixture directory");
    let vault = ProjectVault::initialize_with_key(directory.path(), WRAPPING_KEY)
        .expect("explicit fixture key");
    let (store, _) = ProjectStore::initialize_with_vault(directory.path(), PRIVATE_NAME, vault)
        .expect("encrypted project");
    (directory, store)
}

fn reopened_vault(root: &Path) -> ProjectVault {
    ProjectVault::open_with_key(root, WRAPPING_KEY)
        .expect("explicit unlock")
        .expect("retained vault")
}

fn private_files(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.join(".loom")];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).expect("private directory") {
            let entry = entry.expect("private entry");
            let kind = entry.file_type().expect("entry kind");
            assert!(!kind.is_symlink(), "fixture contains no symbolic links");
            if kind.is_dir() {
                pending.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    files
}

fn assert_private_payloads_are_sealed(root: &Path) {
    for path in private_files(root) {
        let bytes = fs::read(&path).expect("private file");
        for private in [
            PUBLIC_TEXT.as_bytes(),
            PRIVATE_DRAFT.as_bytes(),
            PRIVATE_REASON.as_bytes(),
            PRIVATE_NAME.as_bytes(),
            PRIVATE_EVIDENCE,
        ] {
            assert!(
                !bytes.windows(private.len()).any(|window| window == private),
                "plaintext payload in {}",
                path.display(),
            );
        }
    }
}

#[test]
fn encrypted_history_manifest_blobs_drafts_and_wal_reopen_with_plain_manuscript() {
    let (directory, mut store) = encrypted_fixture();
    let saved = store
        .create_document_if_absent(
            "story.md",
            DocumentContent::Prose(PUBLIC_TEXT.into()),
            PRIVATE_REASON,
        )
        .expect("create readable writing");
    let evidence = store.put_blob(PRIVATE_EVIDENCE).expect("private evidence");
    let draft = store
        .upsert_transient_draft(
            "story.md",
            saved.revision_id,
            0,
            DocumentContent::Prose(PRIVATE_DRAFT.into()),
        )
        .expect("private draft");
    let database = directory.path().join(".loom/loom.sqlite3");
    let wal = directory.path().join(".loom/loom.sqlite3-wal");
    assert!(
        wal.exists(),
        "inspect the live encrypted WAL, not just checkpointed DB"
    );
    assert!(
        !fs::read(&database)
            .expect("database bytes")
            .starts_with(b"SQLite format 3\0")
    );
    assert_eq!(
        fs::read(directory.path().join("story.md")).expect("ordinary manuscript"),
        PUBLIC_TEXT.as_bytes()
    );
    assert_private_payloads_are_sealed(directory.path());
    // SQL constraints are still enforced by the engine in the encrypted store.
    assert!(
        store
            .connection
            .execute("UPDATE artifacts SET metadata_json = '{}'", [])
            .is_err()
    );
    assert!(
        store
            .connection
            .execute("DELETE FROM revision_segments", [])
            .is_err()
    );
    drop(store);
    let mut store =
        ProjectStore::open_with_vault(directory.path(), reopened_vault(directory.path()))
            .expect("encrypted reopen");
    assert_eq!(store.manifest().name, PRIVATE_NAME);
    assert_eq!(
        store
            .reconstruct_revision(saved.revision_id)
            .expect("history"),
        PUBLIC_TEXT.as_bytes()
    );
    assert_eq!(
        store.read_blob(evidence).expect("private evidence"),
        PRIVATE_EVIDENCE
    );
    assert_eq!(
        store
            .load_transient_draft("story.md")
            .expect("draft")
            .expect("retained")
            .text,
        PRIVATE_DRAFT
    );
    assert!(
        store
            .clear_transient_draft("story.md", draft.draft.version)
            .expect("clear draft")
    );
    store
        .save_document_if_source(
            "story.md",
            DocumentContent::Prose("next readable version".into()),
            "edit",
            saved.revision_id,
            saved.blob_id,
        )
        .expect("source-bound edit");
    assert_private_payloads_are_sealed(directory.path());
    assert_eq!(
        fs::read(directory.path().join("story.md")).expect("edited manuscript"),
        b"next readable version"
    );
}

#[test]
fn wrong_key_and_plain_sqlite_cannot_read_or_replace_encrypted_history() {
    let (directory, mut store) = encrypted_fixture();
    store
        .create_document_if_absent(
            "story.md",
            DocumentContent::Prose(PUBLIC_TEXT.into()),
            PRIVATE_REASON,
        )
        .expect("writing");
    drop(store);
    let database = directory.path().join(".loom/loom.sqlite3");
    let before = fs::read(&database).expect("database bytes");
    assert!(ProjectVault::open_with_key(directory.path(), [74; 32]).is_err());
    let unkeyed = Connection::open(&database).expect("connection does not yet read pages");
    assert!(
        unkeyed
            .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row
                .get::<_, i64>(0))
            .is_err()
    );
    drop(unkeyed);
    let wrong_key = Connection::open(&database).expect("connection");
    wrong_key
        .pragma_update(None, "key", "deliberately incorrect fixture key")
        .expect("key assignment alone is not authentication");
    assert!(
        wrong_key
            .query_row("SELECT count(*) FROM sqlite_schema", [], |row| row
                .get::<_, i64>(0))
            .is_err()
    );
    drop(wrong_key);
    assert_eq!(fs::read(&database).expect("preserved database"), before);
    assert_eq!(
        fs::read(directory.path().join("story.md")).expect("preserved manuscript"),
        PUBLIC_TEXT.as_bytes()
    );
    assert!(
        ProjectStore::open_with_vault(directory.path(), reopened_vault(directory.path())).is_ok()
    );
}

fn blob_path(root: &Path, blob: BlobId) -> PathBuf {
    let digest = blob.to_hex();
    root.join(".loom/blobs/sha256")
        .join(&digest[..2])
        .join(&digest[2..])
}

#[test]
fn modified_and_cross_namespace_private_files_fail_authentication() {
    let (directory, mut store) = encrypted_fixture();
    let saved = store
        .create_document_if_absent(
            "story.md",
            DocumentContent::Prose(PUBLIC_TEXT.into()),
            "create",
        )
        .expect("writing");
    let evidence = store.put_blob(PRIVATE_EVIDENCE).expect("private evidence");
    let evidence_path = blob_path(directory.path(), evidence);
    let encrypted_evidence = fs::read(&evidence_path).expect("sealed evidence");
    let writing_blob_path = blob_path(directory.path(), saved.blob_id);
    fs::write(
        &evidence_path,
        fs::read(&writing_blob_path).expect("other namespace"),
    )
    .expect("swap ciphertext fixture");
    assert!(matches!(
        store.read_blob(evidence),
        Err(StoreError::Vault(_))
    ));
    let mut tampered = encrypted_evidence;
    *tampered.last_mut().expect("envelope byte") ^= 1;
    fs::write(&evidence_path, tampered).expect("tampered fixture");
    assert!(matches!(
        store.read_blob(evidence),
        Err(StoreError::Vault(_))
    ));
    store
        .upsert_transient_draft(
            "story.md",
            saved.revision_id,
            0,
            DocumentContent::Prose(PRIVATE_DRAFT.into()),
        )
        .expect("draft");
    let draft_path = fs::read_dir(directory.path().join(".loom/drafts"))
        .expect("draft directory")
        .next()
        .expect("draft slot")
        .expect("slot")
        .path();
    fs::write(
        draft_path,
        fs::read(writing_blob_path).expect("other namespace"),
    )
    .expect("swap encrypted draft");
    assert!(matches!(
        store.load_transient_draft("story.md"),
        Err(StoreError::Vault(_))
    ));
    assert_eq!(
        fs::read(directory.path().join("story.md")).expect("untouched manuscript"),
        PUBLIC_TEXT.as_bytes()
    );
}

#[test]
fn tampered_manifest_or_database_refuses_recovery_without_touching_manuscript() {
    for target in ["project.json", "loom.sqlite3"] {
        let (directory, mut store) = encrypted_fixture();
        store
            .create_document_if_absent(
                "story.md",
                DocumentContent::Prose(PUBLIC_TEXT.into()),
                "create",
            )
            .expect("writing");
        drop(store);
        let path = directory.path().join(".loom").join(target);
        let mut bytes = fs::read(&path).expect("encrypted bytes");
        let offset = if target == "loom.sqlite3" {
            100
        } else {
            bytes.len() - 1
        };
        bytes[offset] ^= 1;
        fs::write(&path, &bytes).expect("tamper fixture");
        assert!(
            ProjectStore::open_with_vault(directory.path(), reopened_vault(directory.path()))
                .is_err()
        );
        assert_eq!(fs::read(path).expect("preserved rejected bytes"), bytes);
        assert_eq!(
            fs::read(directory.path().join("story.md")).expect("preserved manuscript"),
            PUBLIC_TEXT.as_bytes()
        );
    }
}

#[test]
fn plaintext_initialization_cannot_downgrade_an_existing_vault() {
    let directory = tempfile::tempdir().expect("directory");
    crate::paths::ensure_private_directory(&directory.path().join(".loom"))
        .expect("private fixture directory");
    let _vault =
        ProjectVault::initialize_with_key(directory.path(), WRAPPING_KEY).expect("explicit key");
    assert!(ProjectStore::initialize(directory.path(), PRIVATE_NAME).is_err());
    assert!(!directory.path().join(".loom/project.json").exists());
    assert!(!directory.path().join(".loom/loom.sqlite3").exists());
}

#[test]
fn encrypted_outbox_recovers_semantic_commit_after_projection_failure() {
    let (directory, mut store) = encrypted_fixture();
    let saved = store
        .create_document_if_absent(
            "story.md",
            DocumentContent::Prose(PUBLIC_TEXT.into()),
            "create",
        )
        .expect("writing");
    let changed = store
        .save_document_if_source_idempotent_with_boundary(
            loom_types::CommandId::new(),
            "story.md",
            DocumentContent::Prose("recovered ordinary manuscript".into()),
            PRIVATE_REASON,
            saved.revision_id,
            saved.blob_id,
            |_| {
                Err(StoreError::Io(std::io::Error::other(
                    "interrupted projection fixture",
                )))
            },
        )
        .expect("semantic commit is retained");
    assert!(matches!(
        changed.visible_projection,
        crate::VisibleProjectionState::PendingRetry { .. }
    ));
    assert_eq!(
        fs::read(directory.path().join("story.md")).expect("original writing"),
        PUBLIC_TEXT.as_bytes()
    );
    assert_private_payloads_are_sealed(directory.path());
    drop(store);
    let mut store =
        ProjectStore::open_with_vault(directory.path(), reopened_vault(directory.path()))
            .expect("encrypted reopen");
    store.recover().expect("recover existing outbox owner");
    assert_eq!(
        fs::read(directory.path().join("story.md")).expect("recovered writing"),
        b"recovered ordinary manuscript"
    );
    assert_eq!(
        store
            .reconstruct_revision(changed.save.revision_id)
            .expect("retained exact revision"),
        b"recovered ordinary manuscript"
    );
    assert_private_payloads_are_sealed(directory.path());
}

#[test]
fn encrypted_engine_retains_the_main_sqlite_wal_reset_fix() {
    // libsqlite3-sys 0.37.0 bundled stock SQLite 3.51.3 but SQLCipher 3.50.4.
    // Feature unification must not silently downgrade the workspace database.
    assert!(rusqlite::version_number() >= 3_051_003);
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let version: String = connection
        .pragma_query_value(None, "cipher_version", |row| row.get(0))
        .unwrap();
    assert!(!version.is_empty());
}

#[test]
fn protected_copy_admission_preserves_committed_wal_and_refuses_rollback_recovery() {
    let (directory, mut store) = encrypted_fixture();
    store
        .create_document_if_absent(
            "story.md",
            DocumentContent::Prose(PUBLIC_TEXT.into()),
            PRIVATE_REASON,
        )
        .unwrap();
    store
        .connection
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
    drop(store);
    let root = directory.path();
    let database = root.join(".loom/loom.sqlite3");
    let wal = root.join(".loom/loom.sqlite3-wal");
    let database_before = fs::read(&database).unwrap();
    let wal_before = fs::read(&wal).unwrap();
    assert!(wal_before.len() > 32);
    let source =
        ProjectStore::open_for_protected_copy_with_vault(root, reopened_vault(root)).unwrap();
    assert_eq!(source.read_document("story.md").unwrap().text, PUBLIC_TEXT);
    drop(source);
    assert_eq!(fs::read(&database).unwrap(), database_before);
    assert_eq!(fs::read(&wal).unwrap(), wal_before);

    let rollback = root.join(".loom/loom.sqlite3-journal");
    let marker = b"unresolved rollback journal must not be consumed";
    fs::write(&rollback, marker).unwrap();
    assert!(ProjectStore::open_for_protected_copy_with_vault(root, reopened_vault(root)).is_err());
    assert_eq!(fs::read(&rollback).unwrap(), marker);
    assert_eq!(fs::read(database).unwrap(), database_before);
    assert_eq!(fs::read(wal).unwrap(), wal_before);
}
