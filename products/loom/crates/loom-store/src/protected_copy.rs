//! Explicit, no-clobber conversion of current project storage into a separate
//! encrypted copy. The source is never rewritten or deleted. Unknown private
//! formats are refused rather than copied as misleadingly protected plaintext.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};

use desktop_vault::ProjectVault;
use loom_types::{BlobId, CommandId, DocumentId, RevisionId};
use rusqlite::params;
use serde::Serialize;

use crate::file_io::{
    atomic_replace_private, read_bounded_no_follow, rename_if_absent, sync_parent,
};
use crate::paths::ensure_private_directory;
use crate::{ProjectStore, Result, StoreError};

const MAX_FILES: usize = 100_000;
const MAX_DEPTH: usize = 32;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct ProtectedCopy {
    pub destination: PathBuf,
    pub protected_files: usize,
    pub readable_files: usize,
    pub source_preserved: bool,
}

impl ProjectStore {
    /// Copy current-format private data into a newly keyed project. Ordinary
    /// manuscripts, authored configuration, public assets and manuscript
    /// recovery captures remain readable. Existing plaintext source remains.
    pub fn export_encrypted_copy(&self, destination: impl AsRef<Path>) -> Result<ProtectedCopy> {
        self.protected_copy_using(destination.as_ref(), ProjectVault::initialize)
    }

    #[cfg(test)]
    fn protected_copy_with_key(&self, destination: &Path, key: [u8; 32]) -> Result<ProtectedCopy> {
        self.protected_copy_using(destination, |root| {
            ProjectVault::initialize_with_key(root, key)
        })
    }

    fn protected_copy_using(
        &self,
        destination: &Path,
        create_vault: impl FnOnce(&Path) -> std::result::Result<ProjectVault, desktop_vault::VaultError>,
    ) -> Result<ProtectedCopy> {
        crate::paths::ensure_private_storage_supported()?;
        self.require_copy_quiescence()?;
        let parent = destination
            .parent()
            .ok_or_else(|| refused("Choose a new destination folder"))?
            .canonicalize()?;
        if parent.starts_with(&self.root) {
            return Err(refused(
                "The protected copy must be outside its source project",
            ));
        }
        let name = destination
            .file_name()
            .ok_or_else(|| refused("Choose a new destination folder"))?;
        let destination = parent.join(name);
        if destination.try_exists()? || fs::symlink_metadata(&destination).is_ok() {
            return Err(refused(
                "The destination already exists; nothing was overwritten",
            ));
        }
        let files = inventory(&self.root)?;
        let owners = self.copy_payload_owners()?;
        for path in owners.drafts.keys() {
            if files.binary_search(path).is_err() {
                return Err(refused(
                    "The committed draft slot is missing; the source was preserved",
                ));
            }
        }
        // Validate all private format owners before prompting for a credential.
        for relative in &files {
            let _ = payload_kind(relative, &owners)?;
        }

        let stage = parent.join(format!(
            ".mine-protected-copy-{}",
            loom_types::CommandId::new()
        ));
        create_stage(&stage)?;
        let result = (|| {
            ensure_private_directory(&stage.join(".loom"))?;
            let vault = create_vault(&stage)?;
            let mut payloads = self.copy_payloads(&stage, &vault, &files, &owners)?;
            self.export_keyed_database(&stage, &vault)?;
            payloads.protected_files += 1;
            let copied = Self::open_for_protected_copy_with_vault(&stage, vault)?;
            self.verify_copied_store(&copied, &owners)?;
            drop(copied);
            if inventory(&self.root)? != files {
                return Err(refused("The source project changed during copying; retry"));
            }
            for (relative, expected) in payloads.hashes {
                let bytes = read_bounded_no_follow(
                    &self.root.join(relative),
                    desktop_vault::encrypted_max_len(MAX_FILE_BYTES),
                )?;
                if BlobId::digest(&bytes) != expected {
                    return Err(refused("The source project changed during copying; retry"));
                }
            }
            self.require_copy_quiescence()?;
            if !rename_if_absent(&stage, &destination)? {
                return Err(refused(
                    "The destination appeared during copying; nothing was overwritten",
                ));
            }
            sync_parent(&destination)?;
            Ok(ProtectedCopy {
                destination,
                protected_files: payloads.protected_files,
                readable_files: payloads.readable_files,
                source_preserved: true,
            })
        })();
        // Only this randomly named stage is ours. Never clean up the source or
        // the published destination, even when directory sync reports failure.
        if stage.is_dir() {
            let _ = fs::remove_dir_all(&stage);
        }
        result
    }

    fn verify_copied_store(&self, copied: &Self, owners: &CopyOwners) -> Result<()> {
        // Schema, identities and every referenced CAS payload must survive
        // exactly; the copy owns a separate store lease during validation.
        if copied.manifest() != self.manifest() || copied.counts()? != self.counts()? {
            return Err(refused("Protected copy metadata differs from its source"));
        }
        let ids = self
            .connection
            .prepare("SELECT blob_id FROM blobs ORDER BY blob_id")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in ids {
            let id: BlobId = id
                .parse()
                .map_err(|_| refused("Invalid source blob identity"))?;
            copied.read_blob_bounded(id, MAX_FILE_BYTES)?;
        }
        for draft in owners.drafts.values() {
            let restored = copied
                .load_transient_draft(&draft.document_path)?
                .ok_or_else(|| refused("The copied draft is missing"))?;
            if restored.document_id != draft.document_id
                || restored.version != draft.version
                || restored.blob_id != draft.blob_id
                || restored.source_revision_id != draft.source_revision_id
            {
                return Err(refused("The copied draft identity differs from its source"));
            }
        }
        Ok(())
    }

    fn copy_payloads(
        &self,
        stage: &Path,
        vault: &ProjectVault,
        files: &[String],
        owners: &CopyOwners,
    ) -> Result<CopiedPayloads> {
        let mut copied = CopiedPayloads::default();
        for relative in files {
            let kind = payload_kind(relative, owners)?;
            if matches!(kind, PayloadKind::Skip) {
                continue;
            }
            let source = self.root.join(relative);
            let stored =
                read_bounded_no_follow(&source, desktop_vault::encrypted_max_len(MAX_FILE_BYTES))?;
            copied
                .hashes
                .insert(relative.clone(), BlobId::digest(&stored));
            let destination = stage.join(relative);
            create_copy_parents(stage, relative)?;
            match kind {
                PayloadKind::Protected(namespace) => {
                    let plain = if let Some(source_vault) = &self.vault {
                        source_vault.open_bytes(&namespace, &stored, MAX_FILE_BYTES)?
                    } else {
                        stored
                    };
                    if let Some(draft) = owners.drafts.get(relative) {
                        validate_draft_bytes(draft, &plain)?;
                    }
                    if let Some(id) = namespace.strip_prefix("loom/blob/") {
                        let expected: BlobId =
                            id.parse().map_err(|_| refused("Invalid CAS identity"))?;
                        let actual = BlobId::digest(&plain);
                        if actual != expected {
                            return Err(StoreError::CorruptBlob {
                                path: source,
                                expected,
                                actual,
                            });
                        }
                    }
                    crate::private_io::write(Some(vault), &destination, &namespace, &plain)?;
                    // Verify the authenticated bytes through the destination
                    // codec before making the project visible at its name.
                    if crate::private_io::read(
                        Some(vault),
                        &destination,
                        &namespace,
                        MAX_FILE_BYTES,
                    )? != plain
                    {
                        return Err(refused("Protected copy verification failed"));
                    }
                    copied.protected_files += 1;
                }
                PayloadKind::Readable => {
                    if stored.len() as u64 > MAX_FILE_BYTES {
                        return Err(refused("Copy file exceeds its limit"));
                    }
                    if let Some(expected) = owners.recovery.get(relative)
                        && BlobId::digest(&stored) != *expected
                    {
                        return Err(refused(
                            "A manuscript recovery capture differs from its recorded identity",
                        ));
                    }
                    atomic_replace_private(&destination, &stored)?;
                    copied.readable_files += 1;
                }
                PayloadKind::Skip => unreachable!(),
            }
        }
        Ok(copied)
    }

    fn require_copy_quiescence(&self) -> Result<()> {
        for sql in [
            "SELECT count(*) FROM visible_file_outbox WHERE state = 'pending'",
            "SELECT count(*) FROM document_rename_operations WHERE state IN ('prepared','captured')",
            "SELECT count(*) FROM document_delete_operations WHERE state IN ('prepared','captured')",
        ] {
            if self
                .connection
                .query_row(sql, [], |row| row.get::<_, i64>(0))?
                != 0
            {
                return Err(refused(
                    "Resolve pending manuscript recovery before making a protected copy",
                ));
            }
        }
        Ok(())
    }

    fn copy_payload_owners(&self) -> Result<CopyOwners> {
        let mut owners = CopyOwners::default();
        let mut statement = self
            .connection
            .prepare("SELECT document_id FROM documents")?;
        for row in statement.query_map([], |row| row.get::<_, String>(0))? {
            let id: DocumentId = row?
                .parse()
                .map_err(|_| refused("Invalid document identity"))?;
            for slot in [0, 1] {
                owners
                    .disposable_drafts
                    .insert(format!(".loom/drafts/{id}.{slot}.draft"));
            }
            owners.ensure_bounded()?;
        }
        let mut statement = self.connection.prepare(
            "SELECT td.document_id, td.storage_slot, td.draft_version, td.draft_blob_id,
                    td.source_revision_id, d.relative_path
             FROM transient_drafts td JOIN documents d ON d.document_id = td.document_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u8>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (id, slot, version, blob, revision, document_path) = row?;
            let version = u64::try_from(version).map_err(|_| refused("Invalid draft version"))?;
            if version == 0 || slot > 1 {
                return Err(refused("Invalid draft slot or version"));
            }
            let id: DocumentId = id
                .parse()
                .map_err(|_| refused("Invalid draft document identity"))?;
            let blob = blob
                .parse()
                .map_err(|_| refused("Invalid draft blob identity"))?;
            let source_revision_id = revision
                .parse()
                .map_err(|_| refused("Invalid draft revision identity"))?;
            let path = format!(".loom/drafts/{id}.{slot}.draft");
            owners.drafts.insert(
                path,
                DraftProof {
                    namespace: crate::draft::draft_namespace(id, slot, version, blob),
                    document_id: id,
                    document_path,
                    source_revision_id,
                    version,
                    blob_id: blob,
                },
            );
            owners.ensure_bounded()?;
        }
        self.copy_recovery_owners(&mut owners)?;
        Ok(owners)
    }

    fn copy_recovery_owners(&self, owners: &mut CopyOwners) -> Result<()> {
        let mut statement = self.connection.prepare(
            "SELECT outbox_id, expected_visible_blob_id FROM visible_file_outbox
             WHERE state = 'completed' AND expected_visible_blob_id IS NOT NULL",
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })? {
            let (id, blob) = row?;
            if id <= 0 {
                return Err(refused("Invalid outbox identity"));
            }
            owners.recovery.insert(
                format!(".loom/backups/outbox/{id}.previous"),
                blob.parse()
                    .map_err(|_| refused("Invalid recovery blob identity"))?,
            );
            owners.ensure_bounded()?;
        }
        let mut statement = self.connection.prepare(
            "SELECT operation_id, blob_id FROM document_rename_operations WHERE state IN ('committed', 'aborted')"
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })? {
            let (id, blob) = row?;
            let id: CommandId = id
                .parse()
                .map_err(|_| refused("Invalid rename operation identity"))?;
            let blob: BlobId = blob
                .parse()
                .map_err(|_| refused("Invalid recovery blob identity"))?;
            for suffix in ["capture", "anchor"] {
                owners
                    .recovery
                    .insert(format!(".loom/renames/{id}.{suffix}"), blob);
            }
            owners.ensure_bounded()?;
        }
        let mut statement = self.connection.prepare(
            "SELECT command_id, document_id, revision_id, blob_id, recovery_file_name
             FROM document_delete_operations WHERE state IN ('committed', 'aborted')",
        )?;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })? {
            let (command, document, revision, blob, name) = row?;
            let command: CommandId = command
                .parse()
                .map_err(|_| refused("Invalid deletion identity"))?;
            let document: DocumentId = document
                .parse()
                .map_err(|_| refused("Invalid deletion document"))?;
            let revision: RevisionId = revision
                .parse()
                .map_err(|_| refused("Invalid deletion revision"))?;
            let blob: BlobId = blob
                .parse()
                .map_err(|_| refused("Invalid recovery blob identity"))?;
            if name != format!("{command}.{document}.{revision}.{blob}.document") {
                return Err(refused("Invalid deletion recovery filename"));
            }
            owners
                .recovery
                .insert(format!(".loom/deleted/{name}"), blob);
            owners.ensure_bounded()?;
        }
        Ok(())
    }

    fn export_keyed_database(&self, stage: &Path, vault: &ProjectVault) -> Result<()> {
        let path = stage.join(".loom/loom.sqlite3");
        crate::file_io::create_private_file_if_absent(&path)?;
        vault.with_database_key(|key| {
            self.connection.execute(
                "ATTACH DATABASE ?1 AS mine_protected KEY ?2",
                params![path.to_string_lossy(), key],
            )
        })?;
        let exported = (|| {
            self.connection
                .execute_batch("SELECT sqlcipher_export('mine_protected');")?;
            for pragma in ["application_id", "user_version"] {
                let value: u32 = self
                    .connection
                    .pragma_query_value(None, pragma, |row| row.get(0))?;
                self.connection
                    .pragma_update(Some("mine_protected"), pragma, value)?;
            }
            Ok::<_, rusqlite::Error>(())
        })();
        let detached = self
            .connection
            .execute_batch("DETACH DATABASE mine_protected");
        exported?;
        detached?;
        File::open(path)?.sync_all()?;
        Ok(())
    }
}

#[derive(Default)]
struct CopiedPayloads {
    hashes: BTreeMap<String, BlobId>,
    protected_files: usize,
    readable_files: usize,
}

#[derive(Default)]
struct CopyOwners {
    drafts: BTreeMap<String, DraftProof>,
    disposable_drafts: BTreeSet<String>,
    recovery: BTreeMap<String, BlobId>,
}

impl CopyOwners {
    fn ensure_bounded(&self) -> Result<()> {
        if self.drafts.len() + self.disposable_drafts.len() + self.recovery.len() > 3 * MAX_FILES {
            return Err(refused("Copy ownership inventory exceeds its limit"));
        }
        Ok(())
    }
}

struct DraftProof {
    namespace: String,
    document_id: DocumentId,
    document_path: String,
    source_revision_id: RevisionId,
    version: u64,
    blob_id: BlobId,
}

fn validate_draft_bytes(draft: &DraftProof, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > crate::MAX_DOCUMENT_BYTES
        || BlobId::digest(bytes) != draft.blob_id
        || std::str::from_utf8(bytes).is_err()
    {
        return Err(refused(
            "The committed draft bytes do not match their recorded identity",
        ));
    }
    Ok(())
}

fn create_copy_parents(stage: &Path, relative: &str) -> Result<()> {
    let path = Path::new(relative);
    let parent = path.parent().ok_or_else(|| refused("Invalid copy path"))?;
    let mut directory = stage.to_path_buf();
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return Err(refused("Invalid copy path"));
        };
        directory.push(name);
        ensure_private_directory(&directory)?;
    }
    Ok(())
}
enum PayloadKind {
    Skip,
    Readable,
    Protected(String),
}

fn payload_kind(relative: &str, owners: &CopyOwners) -> Result<PayloadKind> {
    if !relative.starts_with(".loom/") {
        return Ok(PayloadKind::Readable);
    }
    if let Some(draft) = owners.drafts.get(relative) {
        return Ok(PayloadKind::Protected(draft.namespace.clone()));
    }
    if matches!(
        relative,
        ".loom/loom.sqlite3"
            | ".loom/loom.sqlite3-wal"
            | ".loom/loom.sqlite3-shm"
            | ".loom/session.lock"
            | ".loom/vault.json"
    ) || owners.disposable_drafts.contains(relative)
    {
        // Only the committed draft slot is authoritative. The other slot is
        // disposable crash staging and is never imported as semantic history.
        return Ok(PayloadKind::Skip);
    }
    if relative == ".loom/project.json" {
        return Ok(PayloadKind::Protected(
            crate::store::MANIFEST_NAMESPACE.into(),
        ));
    }
    if let Some(path) = relative.strip_prefix(".loom/blobs/sha256/") {
        let parts: Vec<_> = path.split('/').collect();
        if parts.len() == 2 && parts[0].len() == 2 && parts[1].len() == 62 {
            let id: BlobId = parts
                .concat()
                .parse()
                .map_err(|_| refused("Invalid CAS path"))?;
            return Ok(PayloadKind::Protected(format!("loom/blob/{id}")));
        }
    }
    if relative == ".loom/co-writers.json"
        || relative == ".loom/attachments/document-context.json"
        || known_attachment_payload(relative)
        || known_function_receipt(relative)
    {
        return Ok(PayloadKind::Protected(relative.into()));
    }
    if owners.recovery.contains_key(relative) {
        return Ok(PayloadKind::Readable);
    }
    Err(refused(&format!(
        "Unknown private file format: {relative}; the source was preserved"
    )))
}

fn canonical_blob_name(name: &str) -> bool {
    name.parse::<BlobId>()
        .is_ok_and(|id| id.to_string() == name)
}

fn known_attachment_payload(relative: &str) -> bool {
    if let Some(name) = relative.strip_prefix(".loom/attachments/objects/") {
        return canonical_blob_name(name);
    }
    relative
        .strip_prefix(".loom/attachments/manifests/")
        .and_then(|name| name.strip_suffix(".json"))
        .is_some_and(|name| canonical_blob_name(name.strip_prefix("source-").unwrap_or(name)))
}

fn known_function_receipt(relative: &str) -> bool {
    relative
        .strip_prefix(".loom/function-runs/")
        .and_then(|name| {
            name.strip_suffix(".started.json")
                .or_else(|| name.strip_suffix(".finished.json"))
        })
        .is_some_and(|name| {
            name.parse::<CommandId>()
                .is_ok_and(|id| id.to_string() == name)
        })
}

fn inventory(root: &Path) -> Result<Vec<String>> {
    let mut stack = vec![(root.to_path_buf(), 0)];
    let mut files = Vec::new();
    let mut count = 0;
    let mut total = 0_u64;
    while let Some((directory, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(refused("Copy directory depth exceeds its limit"));
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            count += 1;
            if count > MAX_FILES {
                return Err(refused("Copy file count exceeds its limit"));
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_symlink() {
                return Err(StoreError::SymbolicLink(path));
            }
            if metadata.is_dir() {
                stack.push((path, depth + 1));
            } else if metadata.is_file() {
                total = total
                    .checked_add(metadata.len())
                    .ok_or_else(|| refused("Copy size overflow"))?;
                if metadata.len() > desktop_vault::encrypted_max_len(MAX_FILE_BYTES)
                    || total > MAX_TOTAL_BYTES
                {
                    return Err(refused(
                        "Copy exceeds its 512 MiB per-file or 16 GiB total limit",
                    ));
                }
                files.push(relative_name(root, &path)?);
            } else {
                return Err(StoreError::NotRegularFile(path));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn relative_name(root: &Path, path: &Path) -> Result<String> {
    path.strip_prefix(root)
        .ok()
        .and_then(Path::to_str)
        .map(str::to_owned)
        .ok_or_else(|| refused("Copy path is not project-relative UTF-8"))
}

fn create_stage(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

fn refused(message: &str) -> StoreError {
    StoreError::CorruptDatabase(message.into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use loom_document::DocumentContent;

    fn source_project(root: &Path, encrypted: bool) -> ProjectStore {
        if encrypted {
            fs::create_dir_all(root.join(".loom")).unwrap();
            let vault = ProjectVault::initialize_with_key(root, [19; 32]).unwrap();
            ProjectStore::initialize_with_vault(root, "Private project name", vault)
                .unwrap()
                .0
        } else {
            ProjectStore::initialize(root, "Private project name")
                .unwrap()
                .0
        }
    }

    const VISIBLE: &str = "Ordinary writing\r\n尾  ";

    fn source_with_draft(
        root: &Path,
        encrypted: bool,
    ) -> (ProjectStore, crate::TransientDraft, RevisionId) {
        let mut source = source_project(root, encrypted);
        let saved = source
            .create_document_if_absent(
                "chapters/deep/page.md",
                DocumentContent::Prose(VISIBLE.into()),
                "import",
            )
            .unwrap();
        let first = source
            .upsert_transient_draft(
                "chapters/deep/page.md",
                saved.revision_id,
                0,
                DocumentContent::Prose("Earlier unsent draft".into()),
            )
            .unwrap();
        let draft = source
            .upsert_transient_draft(
                "chapters/deep/page.md",
                saved.revision_id,
                first.draft.version,
                DocumentContent::Prose("Exact current unsent draft\r\n尾  ".into()),
            )
            .unwrap()
            .draft;
        (source, draft, saved.revision_id)
    }

    #[test]
    fn nested_copy_preserves_drafts_cas_and_each_private_namespace_from_either_source_format() {
        for encrypted in [false, true] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("source");
            let (source, draft, revision) = source_with_draft(&root, encrypted);
            let visible = VISIBLE;
            let evidence = b"Private unselected result and evidence";
            let blob = source.put_blob(evidence).unwrap();
            let receipt = b"{\"private\":\"A private response\"}";
            let attachment_id = BlobId::digest(receipt);
            let receipt_paths = [
                format!(".loom/function-runs/{}.finished.json", CommandId::new()),
                ".loom/co-writers.json".into(),
                ".loom/attachments/document-context.json".into(),
                format!(".loom/attachments/manifests/{attachment_id}.json"),
                format!(".loom/attachments/manifests/source-{attachment_id}.json"),
                format!(".loom/attachments/objects/{attachment_id}"),
            ];
            let mut source_bytes = BTreeMap::new();
            for relative in &receipt_paths {
                create_copy_parents(&root, relative).unwrap();
                crate::private_io::write(
                    source.vault.as_ref(),
                    &root.join(relative),
                    relative,
                    receipt,
                )
                .unwrap();
                source_bytes.insert(relative, fs::read(root.join(relative)).unwrap());
            }
            fs::write(
                root.join(".mine.toml"),
                "# Exact authored configuration\r\n",
            )
            .unwrap();
            let destination = parent.path().join("protected");
            let report = source
                .protected_copy_with_key(&destination, [72; 32])
                .unwrap();
            assert!(report.source_preserved);
            assert_eq!(
                fs::read(destination.join("chapters/deep/page.md")).unwrap(),
                visible.as_bytes()
            );
            assert_eq!(
                fs::read(root.join(".mine.toml")).unwrap(),
                fs::read(destination.join(".mine.toml")).unwrap()
            );
            let vault = ProjectVault::open_with_key(&destination, [72; 32])
                .unwrap()
                .unwrap();
            for relative in &receipt_paths {
                assert_eq!(
                    fs::read(root.join(relative)).unwrap(),
                    source_bytes[relative]
                );
                let stored = fs::read(destination.join(relative)).unwrap();
                assert!(!stored.windows(receipt.len()).any(|bytes| bytes == receipt));
                assert_eq!(vault.open_bytes(relative, &stored, 1024).unwrap(), receipt);
                assert!(
                    vault
                        .open_bytes(".loom/a-different-owner", &stored, 1024)
                        .is_err()
                );
            }
            let copied =
                ProjectStore::open_for_protected_copy_with_vault(&destination, vault).unwrap();
            assert_eq!(copied.counts().unwrap(), source.counts().unwrap());
            assert_eq!(copied.manifest(), source.manifest());
            assert_eq!(
                copied.read_document("chapters/deep/page.md").unwrap().text,
                visible
            );
            assert_eq!(
                copied.reconstruct_revision(revision).unwrap(),
                visible.as_bytes()
            );
            assert_eq!(copied.read_blob_bounded(blob, 1024).unwrap(), evidence);
            assert_eq!(
                copied
                    .load_transient_draft("chapters/deep/page.md")
                    .unwrap()
                    .unwrap(),
                draft
            );
            assert_eq!(
                source
                    .load_transient_draft("chapters/deep/page.md")
                    .unwrap()
                    .unwrap(),
                draft
            );
            assert_eq!(
                fs::read_dir(destination.join(".loom/drafts"))
                    .unwrap()
                    .count(),
                1
            );
            assert_eq!(fs::read_dir(root.join(".loom/drafts")).unwrap().count(), 2);
        }
    }

    #[test]
    fn destination_race_and_source_symlinks_never_clobber_or_publish() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("source");
        let source = source_project(&root, false);
        let destination = parent.path().join("protected");
        let result = source.protected_copy_using(&destination, |stage| {
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("keep.txt"), b"Competing destination").unwrap();
            ProjectVault::initialize_with_key(stage, [74; 32])
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read(destination.join("keep.txt")).unwrap(),
            b"Competing destination"
        );
        assert!(!destination.join(".loom").exists());
        assert!(
            source
                .protected_copy_with_key(&destination, [74; 32])
                .is_err()
        );
        assert!(
            source
                .protected_copy_with_key(&root.join("nested"), [74; 32])
                .is_err()
        );
        std::os::unix::fs::symlink(destination.join("keep.txt"), root.join("linked.txt")).unwrap();
        let other = parent.path().join("other");
        assert!(source.protected_copy_with_key(&other, [74; 32]).is_err());
        assert!(!other.exists());
        assert!(
            root.join("linked.txt")
                .symlink_metadata()
                .unwrap()
                .is_symlink()
        );
    }

    #[test]
    fn unowned_private_files_cannot_hide_in_readable_recovery_or_disposable_draft_directories() {
        for relative in [
            ".loom/unknown-private-format",
            ".loom/backups/secret.txt",
            ".loom/backups/outbox/1.previous",
            ".loom/deleted/secret.document",
            ".loom/renames/secret.capture",
            ".loom/drafts/secret.txt",
            ".loom/function-runs/not-a-receipt.json",
            ".loom/attachments/objects/not-a-blob",
            ".loom/attachments/manifests/unknown.json",
        ] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("source");
            let source = source_project(&root, false);
            create_copy_parents(&root, relative).unwrap();
            fs::write(root.join(relative), b"never silently copy or discard").unwrap();
            let destination = parent.path().join("protected");
            assert!(
                source
                    .protected_copy_with_key(&destination, [73; 32])
                    .is_err(),
                "{relative}"
            );
            assert!(!destination.exists());
            assert_eq!(
                fs::read(root.join(relative)).unwrap(),
                b"never silently copy or discard"
            );
        }
    }

    #[test]
    fn pending_outbox_refuses_without_project_recovery_or_publication() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("source");
        let mut source = source_project(&root, false);
        let saved = source
            .create_document_if_absent(
                "page.md",
                DocumentContent::Prose("Saved text".into()),
                "fixture",
            )
            .unwrap();
        source.connection.execute("UPDATE visible_file_outbox SET state='pending', completed_at_ms=NULL WHERE revision_id=?1", [saved.revision_id.to_string()]).unwrap();
        fs::write(root.join("page.md"), b"Uncheckpointed external text").unwrap();
        let destination = parent.path().join("protected");
        assert!(
            source
                .protected_copy_with_key(&destination, [73; 32])
                .is_err()
        );
        assert!(!destination.exists());
        assert_eq!(source.pending_outbox_count().unwrap(), 1);
        assert_eq!(
            fs::read(root.join("page.md")).unwrap(),
            b"Uncheckpointed external text"
        );
    }

    #[test]
    fn missing_or_corrupt_active_drafts_are_refused_and_never_recreated_in_source() {
        for missing in [false, true] {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("source");
            let mut source = source_project(&root, false);
            let saved = source
                .create_document_if_absent(
                    "page.md",
                    DocumentContent::Prose("Saved text".into()),
                    "fixture",
                )
                .unwrap();
            let draft = source
                .upsert_transient_draft(
                    "page.md",
                    saved.revision_id,
                    0,
                    DocumentContent::Prose("Unsent draft".into()),
                )
                .unwrap()
                .draft;
            let slot = root.join(format!(".loom/drafts/{}.1.draft", draft.document_id));
            if missing {
                fs::remove_dir_all(root.join(".loom/drafts")).unwrap();
            } else {
                fs::write(&slot, b"Corrupted slot bytes").unwrap();
            }
            let destination = parent.path().join("protected");
            assert!(
                source
                    .protected_copy_with_key(&destination, [73; 32])
                    .is_err()
            );
            assert!(!destination.exists());
            assert!(!parent.path().read_dir().unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".mine-protected-copy-")
            }));
            if missing {
                assert!(!root.join(".loom/drafts").exists());
            } else {
                assert_eq!(fs::read(&slot).unwrap(), b"Corrupted slot bytes");
            }
        }
    }

    #[test]
    fn deleted_manuscript_capture_remains_readable_and_owned_by_the_same_sql_identity() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("source");
        let mut source = source_project(&root, false);
        let saved = source
            .create_document_if_absent(
                "page.md",
                DocumentContent::Prose("Deleted manuscript text".into()),
                "fixture",
            )
            .unwrap();
        let document = source.read_document("page.md").unwrap();
        source
            .delete_document_file_idempotent(
                CommandId::new(),
                document.document_id,
                saved.revision_id,
                saved.blob_id,
            )
            .unwrap();
        let capture: String = source
            .connection
            .query_row(
                "SELECT recovery_file_name FROM document_deletions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let destination = parent.path().join("protected");
        source
            .protected_copy_with_key(&destination, [73; 32])
            .unwrap();
        assert_eq!(
            fs::read(destination.join(".loom/deleted").join(&capture)).unwrap(),
            b"Deleted manuscript text"
        );
        assert_eq!(
            fs::read(root.join(".loom/deleted").join(&capture)).unwrap(),
            b"Deleted manuscript text"
        );
    }
}
