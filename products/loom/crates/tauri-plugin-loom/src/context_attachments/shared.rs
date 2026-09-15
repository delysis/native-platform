//! Shared manuscripts read a separate attachment cache. Naming a private
//! import's hash grants no access to its bytes, display label, or provenance.
use super::*;

// Projection and on-demand inspection share one owner. A quota check followed
// by an immutable installation cannot race another shared-cache writer.
static CACHE_WRITE: Mutex<()> = Mutex::new(());
const MAX_DERIVED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 4096;

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Scope {
    schema: u32,
    cabal: String,
    private_documents: BTreeSet<String>,
    public_documents: BTreeSet<String>,
    recovery_paths: BTreeSet<String>,
}

pub(crate) fn configure(
    project_root: &Path,
    cabal: uuid::Uuid,
    mut private_documents: BTreeSet<String>,
    mut public_documents: BTreeSet<String>,
) -> Result<PathBuf, ContextAttachmentError> {
    let previous = read_scope(project_root)?;
    let mut recovery_paths = BTreeSet::new();
    if let Some(previous) = &previous {
        if previous.cabal != cabal.to_string() {
            return Err(ContextAttachmentError::ContextInvalid);
        }
        public_documents.extend(previous.public_documents.iter().cloned());
        recovery_paths.clone_from(&previous.recovery_paths);
    }
    private_documents.retain(|id| !public_documents.contains(id));
    let scope = Scope {
        schema: 1,
        cabal: cabal.to_string(),
        private_documents,
        public_documents,
        recovery_paths,
    };
    if previous.as_ref() != Some(&scope) {
        write_scope(project_root, &scope)?;
    }
    shared_root(project_root, cabal)
}

fn write_scope(project_root: &Path, scope: &Scope) -> Result<(), ContextAttachmentError> {
    let bytes = serde_json::to_vec(&scope)?;
    if bytes.len() > 1024 * 1024 {
        return Err(ContextAttachmentError::ContextLimit);
    }
    let path = attachment_root(project_root)?.join("sharing.json");
    replace_atomically(&path, &bytes)
}

pub(crate) fn recovery_paths(
    project_root: &Path,
) -> Result<BTreeSet<String>, ContextAttachmentError> {
    Ok(read_scope(project_root)?
        .map(|scope| scope.recovery_paths)
        .unwrap_or_default())
}

/// Reserve the namespace before creating the recovery file. A crash between
/// creation and registration must not grant its peer-authored references
/// access to private imports. Recorded document IDs retain this scope on rename.
pub(crate) fn reserve_recovery(
    project_root: &Path,
    path: String,
) -> Result<(), ContextAttachmentError> {
    let mut scope = read_scope(project_root)?.ok_or(ContextAttachmentError::ContextInvalid)?;
    if scope.recovery_paths.insert(path) {
        write_scope(project_root, &scope)?;
    }
    Ok(())
}

pub(crate) fn inline_root(
    project_root: &Path,
    document_id: &str,
) -> Result<PathBuf, ContextAttachmentError> {
    match read_scope(project_root)? {
        Some(scope) if !scope.private_documents.contains(document_id) => shared_root(
            project_root,
            scope
                .cabal
                .parse()
                .map_err(|_| ContextAttachmentError::ContextInvalid)?,
        ),
        _ => Ok(project_root.into()),
    }
}

pub(crate) fn original_for_document(
    project_root: &Path,
    document_id: &str,
    manuscript: &str,
    id: &str,
) -> Result<PathBuf, ContextAttachmentError> {
    let contexts = read_contexts(project_root)?;
    if authoritative_context_ids(&contexts, document_id)
        .iter()
        .any(|selected| selected == id)
    {
        return original_path(project_root, id);
    }
    if !manuscript_selects_attachment(manuscript, id) {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let root = inline_root(project_root, document_id)?;
    if let Some(value) = published(&root, id)? {
        let _ = read_object(&root, id, value.asset.byte_count)?;
        return Ok(attachment_root(&root)?.join("objects").join(id));
    }
    original_path(&root, id)
}

fn read_scope(project_root: &Path) -> Result<Option<Scope>, ContextAttachmentError> {
    let path = attachment_root(project_root)?.join("sharing.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let scope: Scope = serde_json::from_slice(&bytes)?;
    if scope.schema != 1 || scope.cabal.parse::<uuid::Uuid>().is_err() {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    Ok(Some(scope))
}

fn shared_root(project_root: &Path, cabal: uuid::Uuid) -> Result<PathBuf, ContextAttachmentError> {
    let base = project_root.join(".loom/cabal-media");
    let root = base.join(cabal.to_string());
    for path in [&base, &root] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(ContextAttachmentError::ContextInvalid),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(path)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(root)
}

/// Only local authoring calls this. A remote projection must never resolve
/// new references against the private attachment registry.
pub(crate) fn locally_added(
    project_root: &Path,
    before: &str,
    after: &str,
) -> Result<Vec<(String, Vec<u8>)>, ContextAttachmentError> {
    let old: BTreeSet<_> = inline_attachment_ids(before)
        .into_iter()
        .chain(inline_media_ids(before))
        .collect();
    let added: BTreeSet<_> = inline_attachment_ids(after)
        .into_iter()
        .chain(inline_media_ids(after))
        .filter(|id| !old.contains(id))
        .collect();
    if added.len() > MAX_CONTEXT_ATTACHMENTS {
        return Err(ContextAttachmentError::ContextLimit);
    }
    let mut result = Vec::new();
    let mut bytes = 0_u64;
    for id in added {
        let (name, data) = if let Some(manifest) = read_manifest_if_present(project_root, &id)? {
            let data = read_object(project_root, &id, manifest.attachment.byte_count)?;
            (manifest.attachment.file_name, data)
        } else if let Some(name) = crate::attachments::inline_image_assets(after)
            .into_iter()
            .find(|name| name.starts_with(&id))
        {
            let image = crate::attachments::read_image_asset(project_root, &name)
                .map_err(|error| ContextAttachmentError::Processing(error.to_string()))?;
            (name, image.bytes)
        } else {
            continue;
        };
        bytes = bytes
            .checked_add(data.len() as u64)
            .ok_or(ContextAttachmentError::SourceSize)?;
        if bytes > MAX_ATTACHMENT_BYTES {
            return Err(ContextAttachmentError::SourceSize);
        }
        result.push((name, data));
    }
    Ok(result)
}

pub(crate) fn read_image(
    root: &Path,
    name: &str,
) -> Result<crate::attachments::LoadedImageAsset, ContextAttachmentError> {
    if !crate::attachments::is_canonical_image_asset_file_name(name) {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let Some(value) = published(root, &name[..64])? else {
        return crate::attachments::read_image_asset(root, name)
            .map_err(|error| ContextAttachmentError::Processing(error.to_string()));
    };
    if value.asset.byte_count > 16 * 1024 * 1024 {
        return Err(ContextAttachmentError::SourceSize);
    }
    let bytes = read_object(root, &name[..64], value.asset.byte_count)?;
    crate::attachments::validate_image_asset(name, bytes)
        .map_err(|error| ContextAttachmentError::Processing(error.to_string()))
}

/// Local image drops retain ordinary relative paths. Inspect their exact
/// original bytes through the same native media pipeline as context imports.
pub(super) fn prepare_inline_images(
    root: &Path,
    markdown: &str,
) -> Result<(), ContextAttachmentError> {
    let names = crate::attachments::inline_image_assets(markdown);
    if names.len() > MAX_CONTEXT_ATTACHMENTS {
        return Err(ContextAttachmentError::ContextLimit);
    }
    for name in names {
        let id = &name[..64];
        if read_manifest_if_present(root, id)?.is_some() || published(root, id)?.is_some() {
            continue;
        }
        let image = read_image(root, &name)?;
        let provided = ProvidedAttachment::read_bounded(
            name,
            None,
            &mut Cursor::new(image.bytes),
            16 * 1024 * 1024,
        )
        .map_err(|error| ContextAttachmentError::Processing(error.safe_message))?;
        import_provided(root, provided)?;
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Published {
    schema: u32,
    cabal: uuid::Uuid,
    asset: loom_cabal::AssetDescriptor,
}

fn published(root: &Path, id: &str) -> Result<Option<Published>, ContextAttachmentError> {
    if !is_sha256(id) {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let path = attachment_root(root)?
        .join("manifests")
        .join(format!("{id}.cabal.json"));
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let value: Published = serde_json::from_slice(&bytes)?;
    if value.schema != 1
        || value.asset.sha256 != id
        || value.asset.byte_count == 0
        || value.asset.byte_count > MAX_ATTACHMENT_BYTES
        || value.asset.name.trim().is_empty()
        || value.asset.name.len() > 512
        || value.asset.name.chars().any(char::is_control)
        || root.file_name().and_then(|name| name.to_str()) != Some(value.cabal.to_string().as_str())
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    Ok(Some(value))
}

pub(crate) fn has_public_copy(root: &Path, id: &str) -> Result<bool, ContextAttachmentError> {
    let Some(value) = published(root, id)? else {
        return Ok(false);
    };
    let path = attachment_root(root)?.join("objects").join(id);
    Ok(fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() == value.asset.byte_count
    }))
}

pub(crate) fn retain_public(
    root: &Path,
    cabal: uuid::Uuid,
    asset: &loom_cabal::AssetDescriptor,
    bytes: &[u8],
) -> Result<(), ContextAttachmentError> {
    let _owner = CACHE_WRITE
        .lock()
        .map_err(|_| ContextAttachmentError::ContextInvalid)?;
    if bytes.len() as u64 != asset.byte_count
        || bytes.is_empty()
        || bytes.len() as u64 > MAX_ATTACHMENT_BYTES
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    let object = attachment_root(root)?.join("objects").join(&asset.sha256);
    if let Ok(metadata) = fs::symlink_metadata(&object)
        && (!metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != asset.byte_count)
    {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    install_object(root, &asset.sha256, bytes)?;
    let path = attachment_root(root)?
        .join("manifests")
        .join(format!("{}.cabal.json", asset.sha256));
    install_immutable(
        &path,
        &serde_json::to_vec(&Published {
            schema: 1,
            cabal,
            asset: asset.clone(),
        })?,
    )
}

/// Inspect only when a preview or generation actually needs this published
/// file. A malformed attachment cannot stall unrelated document projections.
pub(super) fn inspect_published(root: &Path, id: &str) -> Result<bool, ContextAttachmentError> {
    let _owner = CACHE_WRITE
        .lock()
        .map_err(|_| ContextAttachmentError::ContextInvalid)?;
    // Another reader may have completed inspection while this one waited.
    if read_manifest_if_present(root, id)?.is_some() {
        return Ok(true);
    }
    let Some(value) = published(root, id)? else {
        return Ok(false);
    };
    let bytes = read_object(root, id, value.asset.byte_count)?;
    let provided = ProvidedAttachment::read_bounded(
        value.asset.name,
        None,
        &mut Cursor::new(bytes),
        MAX_ATTACHMENT_BYTES,
    )
    .map_err(|error| ContextAttachmentError::Processing(error.safe_message))?;
    let mut budget = CacheBudget::read(root)?;
    let attachment =
        import_provided_using(root, provided, |path, bytes| budget.retain(path, bytes))?;
    if attachment.id != id {
        return Err(ContextAttachmentError::ContextInvalid);
    }
    record_import_origin_using(
        root,
        &serde_json::json!({
            "schema": "loom.cabal-attachment.v1", "cabal": value.cabal,
            "source_sha256": id, "source_bytes": value.asset.byte_count,
            "integrity": "sha256-verified", "human_reviewed": false,
        }),
        |path, bytes| budget.retain(path, bytes),
    )?;
    Ok(true)
}

/// Original bytes have their own 256 MiB cabal reservation. Extracted text,
/// converted media, receipts, and abandoned temporary files consume this
/// separate budget, so processing cannot use up room reserved for downloads.
struct CacheBudget {
    files: BTreeSet<PathBuf>,
    derived_bytes: u64,
}

impl CacheBudget {
    fn read(root: &Path) -> Result<Self, ContextAttachmentError> {
        let cache = attachment_root(root)?;
        let mut result = Self {
            files: BTreeSet::new(),
            derived_bytes: 0,
        };
        for directory in ["objects", "manifests"] {
            for entry in fs::read_dir(cache.join(directory))? {
                let entry = entry?;
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path)?;
                if result.files.len() >= MAX_CACHE_FILES
                    || !metadata.is_file()
                    || metadata.file_type().is_symlink()
                {
                    return Err(ContextAttachmentError::ContextInvalid);
                }
                let original = if directory == "objects" {
                    match entry.file_name().to_str().filter(|name| is_sha256(name)) {
                        Some(id) => published(root, id)?.is_some(),
                        None => false,
                    }
                } else if let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix(".cabal.json"))
                {
                    published(root, id)?.is_some()
                } else {
                    false
                };
                if !original {
                    result.derived_bytes = result
                        .derived_bytes
                        .checked_add(metadata.len())
                        .ok_or(ContextAttachmentError::ContextLimit)?;
                }
                result.files.insert(path);
            }
        }
        if result.derived_bytes > MAX_DERIVED_BYTES {
            return Err(ContextAttachmentError::ContextLimit);
        }
        Ok(result)
    }

    fn retain(&mut self, path: &Path, bytes: &[u8]) -> Result<(), ContextAttachmentError> {
        if self.files.contains(path) {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != bytes.len() as u64
            {
                return Err(ContextAttachmentError::ContextInvalid);
            }
            return install_immutable(path, bytes);
        }
        let next = self
            .derived_bytes
            .checked_add(bytes.len() as u64)
            .ok_or(ContextAttachmentError::ContextLimit)?;
        if next > MAX_DERIVED_BYTES || self.files.len() >= MAX_CACHE_FILES {
            return Err(ContextAttachmentError::ContextLimit);
        }
        install_immutable(path, bytes)?;
        self.files.insert(path.to_path_buf());
        self.derived_bytes = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_intent_survives_reopen_and_renaming_cannot_widen_attachment_authority() {
        let project = tempfile::tempdir().unwrap();
        let cabal = uuid::Uuid::new_v4();
        let document = loom_types::DocumentId::new().to_string();
        let public = configure(project.path(), cabal, BTreeSet::new(), BTreeSet::new()).unwrap();
        let path = format!(
            "Recovery/Cabal-{}-{}.md",
            uuid::Uuid::new_v4(),
            "a".repeat(64)
        );
        reserve_recovery(project.path(), path.clone()).unwrap();
        assert!(recovery_paths(project.path()).unwrap().contains(&path));
        // Before registration, an unknown recovered document is already public.
        assert_eq!(inline_root(project.path(), &document).unwrap(), public);
        configure(
            project.path(),
            cabal,
            BTreeSet::new(),
            BTreeSet::from([document.clone()]),
        )
        .unwrap();
        // A later private path is not permission to reinterpret a peer's hashes.
        configure(
            project.path(),
            cabal,
            BTreeSet::from([document.clone()]),
            BTreeSet::new(),
        )
        .unwrap();
        assert_eq!(inline_root(project.path(), &document).unwrap(), public);
        assert!(recovery_paths(project.path()).unwrap().contains(&path));
    }

    #[test]
    fn a_full_processing_cache_rejects_new_derivatives_but_still_receives_originals() {
        let project = tempfile::tempdir().unwrap();
        let cabal = uuid::Uuid::new_v4();
        let root = configure(project.path(), cabal, BTreeSet::new(), BTreeSet::new()).unwrap();
        let objects = attachment_root(&root).unwrap().join("objects");
        // Sparse fixture exercises the actual durable accounting without
        // allocating hundreds of MiB or bypassing the production limit.
        File::create(objects.join("retained-processing-output"))
            .unwrap()
            .set_len(MAX_DERIVED_BYTES)
            .unwrap();
        let mut budget = CacheBudget::read(&root).unwrap();
        assert!(
            budget
                .retain(&objects.join("new-output"), b"overflow")
                .is_err()
        );
        assert!(!objects.join("new-output").exists());
        let bytes = b"A newly received original remains available.";
        let descriptor = loom_cabal::AssetDescriptor {
            sha256: format!("{:x}", Sha256::digest(bytes)),
            name: "new.txt".into(),
            byte_count: bytes.len() as u64,
        };
        retain_public(&root, cabal, &descriptor, bytes).unwrap();
        assert_eq!(
            read_object(&root, &descriptor.sha256, descriptor.byte_count).unwrap(),
            bytes
        );
        assert!(inspect_published(&root, &descriptor.sha256).is_err());
        assert!(
            !attachment_root(&root)
                .unwrap()
                .join("manifests")
                .join(format!("{}.json", descriptor.sha256))
                .exists()
        );
        assert_eq!(
            fs::metadata(objects.join("retained-processing-output"))
                .unwrap()
                .len(),
            MAX_DERIVED_BYTES
        );
    }
}
