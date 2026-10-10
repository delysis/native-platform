//! Native app-private selected-scope grants and opaque continuation leaves.
use super::{
    CollectionDefinition, CollectionError, CollectionHead, CollectionIdentity, Deserialize, Path,
    ProjectStore, Result, SCHEMA, Serialize, WRITE_LOCK, bounded, digest, identity, invalid,
    storage, valid_collection_id, valid_hash, validate_head,
};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    schema: String,
    project_id: String,
    project_root: PathBuf,
    collection_id: String,
    scope_fingerprint: String,
    provider: String,
    principal: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Continuation {
    schema: String,
    project_id: String,
    project_root: PathBuf,
    identity: CollectionIdentity,
    refresh_id: String,
    page_number: u32,
    next_cursor: Option<String>,
}
fn private_root(store: &ProjectStore, root: &Path) -> Result<PathBuf> {
    if !root.is_absolute() || root.starts_with(store.root()) {
        return Err(invalid(
            "Collection grants require native app-private storage.",
        ));
    }
    let parent = root
        .parent()
        .ok_or_else(|| invalid("Invalid native grant directory."))?;
    for ancestor in parent.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid("Native grant directories cannot be symlinks."));
        }
    }
    storage::directory(root)?;
    let root = fs::canonicalize(root)?;
    if root.starts_with(fs::canonicalize(store.root())?) {
        return Err(invalid(
            "Acquisition grants cannot live inside a workspace.",
        ));
    }
    Ok(root)
}
fn project_root(store: &ProjectStore) -> Result<PathBuf> {
    Ok(fs::canonicalize(store.root())?)
}
fn workspace_key(store: &ProjectStore, id: &str) -> Result<String> {
    if !valid_collection_id(id) {
        return Err(invalid("Invalid collection ID."));
    }
    Ok(digest(&serde_json::to_vec(&(
        store.manifest().project_id.to_string(),
        project_root(store)?,
        id,
    ))?))
}
fn grant_path(store: &ProjectStore, root: &Path, id: &str) -> Result<PathBuf> {
    Ok(private_root(store, root)?.join(format!("grant-{}.json", workspace_key(store, id)?)))
}
fn load_grant(store: &ProjectStore, root: &Path, id: &str) -> Result<Grant> {
    let bytes = match storage::read(&grant_path(store, root, id)?) {
        Ok(bytes) => bytes,
        Err(CollectionError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CollectionError::NeedsAuthorization);
        }
        Err(error) => return Err(error),
    };
    let grant: Grant = serde_json::from_slice(&bytes)?;
    if grant.schema != SCHEMA
        || grant.collection_id != id
        || grant.project_id != store.manifest().project_id.to_string()
        || grant.project_root != project_root(store)?
        || !valid_hash(&grant.scope_fingerprint)
        || !matches!(grant.provider.as_str(), "drive" | "gmail")
        || !bounded(&grant.principal, 1024)
    {
        return Err(CollectionError::NeedsAuthorization);
    }
    Ok(grant)
}
fn validate_grant_identity(
    store: &ProjectStore,
    root: &Path,
    identity: &CollectionIdentity,
) -> Result<()> {
    let grant = load_grant(store, root, &identity.collection_id)?;
    if grant.scope_fingerprint != identity.scope_fingerprint
        || grant.provider != identity.provider
        || digest(grant.principal.as_bytes()) != identity.principal_fingerprint
    {
        return Err(CollectionError::NeedsAuthorization);
    }
    Ok(())
}
pub(crate) fn save_grant(
    store: &ProjectStore,
    root: &Path,
    definition: &CollectionDefinition,
    principal: &str,
) -> Result<()> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("Collection write lock unavailable."))?;
    let identity = identity(definition, principal)?;
    let grant = Grant {
        schema: SCHEMA.into(),
        project_id: store.manifest().project_id.to_string(),
        project_root: project_root(store)?,
        collection_id: identity.collection_id,
        scope_fingerprint: identity.scope_fingerprint,
        provider: identity.provider,
        principal: principal.into(),
    };
    storage::replace(
        &grant_path(store, root, &definition.id)?,
        &storage::encode(&grant)?,
    )
}
pub(crate) fn require_grant(
    store: &ProjectStore,
    root: &Path,
    definition: &CollectionDefinition,
) -> Result<String> {
    let grant = load_grant(store, root, &definition.id)?;
    let identity = identity(definition, &grant.principal)?;
    if identity.scope_fingerprint != grant.scope_fingerprint || identity.provider != grant.provider
    {
        return Err(CollectionError::NeedsAuthorization);
    }
    Ok(grant.principal)
}
pub(crate) fn revoke_grant(store: &ProjectStore, root: &Path, id: &str) -> Result<()> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("Collection write lock unavailable."))?;
    let path = grant_path(store, root, id)?;
    match storage::read(&path) {
        Ok(_) => {
            fs::remove_file(&path)?;
        }
        Err(CollectionError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
pub(super) fn save_continuation(
    store: &ProjectStore,
    root: &Path,
    head: &CollectionHead,
    next_cursor: Option<String>,
) -> Result<String> {
    let checkpoint = &head.checkpoint;
    validate_grant_identity(store, root, &checkpoint.identity)?;
    let continuation = Continuation {
        schema: SCHEMA.into(),
        project_id: store.manifest().project_id.to_string(),
        project_root: project_root(store)?,
        identity: checkpoint.identity.clone(),
        refresh_id: checkpoint.refresh_id.clone(),
        page_number: checkpoint.pages_completed,
        next_cursor,
    };
    let bytes = storage::encode(&continuation)?;
    let id = digest(&bytes);
    let path = private_root(store, root)?.join(format!(
        "continuation-{}-{id}.json",
        workspace_key(store, &checkpoint.identity.collection_id)?
    ));
    storage::install(&path, &bytes)?;
    Ok(id)
}
pub(crate) fn load_continuation(
    store: &ProjectStore,
    root: &Path,
    head: &CollectionHead,
) -> Result<Option<String>> {
    let checkpoint = &head.checkpoint;
    validate_head(head, &checkpoint.identity.collection_id)?;
    validate_grant_identity(store, root, &checkpoint.identity)?;
    let Some(id) = &checkpoint.continuation_id else {
        return Ok(None);
    };
    let path = private_root(store, root)?.join(format!(
        "continuation-{}-{id}.json",
        workspace_key(store, &checkpoint.identity.collection_id)?
    ));
    let bytes = storage::read(&path)?;
    if digest(&bytes) != *id {
        return Err(invalid("Private continuation changed."));
    }
    let continuation: Continuation = serde_json::from_slice(&bytes)?;
    let expected_page = if checkpoint.page_open {
        checkpoint.pages_completed
    } else {
        checkpoint
            .pages_completed
            .checked_sub(1)
            .ok_or_else(|| invalid("Invalid continuation page."))?
    };
    if continuation.schema != SCHEMA
        || continuation.project_id != store.manifest().project_id.to_string()
        || continuation.project_root != project_root(store)?
        || continuation.identity != checkpoint.identity
        || continuation.refresh_id != checkpoint.refresh_id
        || continuation.page_number != expected_page
        || continuation
            .next_cursor
            .as_ref()
            .is_some_and(|cursor| !bounded(cursor, 16 * 1024))
        || continuation.next_cursor.is_some() != checkpoint.has_next_page
    {
        return Err(invalid(
            "Private continuation does not match this checkpoint.",
        ));
    }
    Ok(continuation.next_cursor)
}
