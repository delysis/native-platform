//! Named source bindings. Workspace metadata is never filesystem authority.
//! Libraries are read-only capabilities granted by an explicit native selection;
//! evidence is retained as immutable bytes independently of the live source.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use atomic_write_file::AtomicWriteFile;
use information_native_backend_sqlite::{AlexandriaBackend, AlexandriaBackendConfig};
use information_native_retrieval::{ReadRequest, ResourceBackend};
use information_native_types::{
    EvidenceHit, ExternalAccessMode, InformationQuery, QUERY_SCHEMA, QueryBudget, QueryFilters,
    QueryId, QuerySyntax, ReleaseId, RepresentationId, ResourceId, RetrievalPurpose,
    RetrievalTarget, UsePermission,
};
use loom_store::ProjectStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::context_attachments::{self, ContextAttachmentPresentation};

mod folders;
mod grants;
pub(crate) use folders::{FolderRetrieval, FolderScanBudget, search_folder};
pub(crate) use grants::{forget_selected_grant, persist_selected_grant, restore_selected_grants};

const SCHEMA: &str = "loom.materials.v1";
const MAX_BINDINGS: usize = 4096;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_EVIDENCE_BYTES: u64 = 70 * 1024 * 1024;
const MAX_QUERY_BYTES: usize = 4096;
const MAX_HITS: u32 = 12;
const MAX_CONTEXT_CHARS: u32 = 24_000;
static WRITE_LOCK: Mutex<()> = Mutex::new(());
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct LibraryGrant {
    backend: AlexandriaBackend,
    path: PathBuf,
    version: String,
}
type LibraryGrants = BTreeMap<(PathBuf, String, String), Arc<LibraryGrant>>;
static LIBRARY_GRANTS: OnceLock<Mutex<LibraryGrants>> = OnceLock::new();

#[derive(Debug, Error)]
pub(crate) enum MaterialError {
    #[error("material storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("material metadata failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("source preparation failed: {0}")]
    Attachment(#[from] context_attachments::ContextAttachmentError),
    #[error("{0}")]
    Invalid(String),
    #[error("material was not found: {0}")]
    NotFound(String),
    #[error("more than one material has this name; use its qualified reference: {0}")]
    Ambiguous(String),
    #[error("select this library again to authorize local access: {0}")]
    NeedsAuthorization(String),
}
type Result<T> = std::result::Result<T, MaterialError>;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MaterialKind {
    Attachment,
    Library,
    Folder,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaterialEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) reference: String,
    pub(crate) kind: MaterialKind,
    pub(crate) pinned: bool,
    pub(crate) available: bool,
    pub(crate) source_path: Option<String>,
    pub(crate) attachment_id: Option<String>,
    #[serde(default)]
    pub(crate) workspace_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    id: String,
    name: String,
    pinned: bool,
    source: Source,
    #[serde(default)]
    workspace_path: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    Attachment { attachment_id: String },
    Library { path: PathBuf },
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Bindings {
    schema: String,
    items: Vec<Binding>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaterialEvidence {
    pub(crate) complete: bool,
    pub(crate) warnings: Vec<String>,
    pub(crate) id: String,
    pub(crate) reference: String,
    pub(crate) material_id: String,
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) source_revision: String,
    pub(crate) text_sha256: String,
    pub(crate) locator: Value,
    /// Full Information evidence envelope, including rights, provenance, and
    /// exact snippet/context bytes. None only for retained attachment text.
    pub(crate) source_evidence: Option<EvidenceHit>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct MaterialRead {
    pub(crate) material: MaterialEntry,
    pub(crate) text: String,
    pub(crate) complete: bool,
    pub(crate) warnings: Vec<String>,
    pub(crate) source_revision: String,
    pub(crate) evidence: Vec<MaterialEvidence>,
    pub(crate) presentation: Option<ContextAttachmentPresentation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct MaterialAdmission {
    pub(crate) material: MaterialEntry,
    pub(crate) source_revision: String,
    pub(crate) text: Option<String>,
    pub(crate) complete: bool,
}

/// Freeze a reference for a run without copying a book into a discarded value.
/// Large sources remain structured references until bounded retrieval consumes
/// them. No source excerpt is fabricated and no evidence is published here.
pub(crate) fn admit(
    store: &ProjectStore,
    id: &str,
    max_text_bytes: usize,
) -> Result<MaterialAdmission> {
    let binding = binding(store, id)?;
    let material = entry(store, &binding)?;
    match binding.source {
        Source::Library { .. } => Ok(MaterialAdmission {
            material,
            source_revision: grants()
                .lock()
                .map_err(|_| invalid("library capability lock poisoned"))?
                .get(&grant_key(store, id))
                .map(|grant| grant.version.clone())
                .unwrap_or_default(),
            text: None,
            complete: false,
        }),
        Source::Attachment { attachment_id } => {
            let mut source = context_attachments::describe_source(store.root(), &attachment_id)?;
            let text = if source.text_bytes <= u64::try_from(max_text_bytes).unwrap_or(u64::MAX) {
                let (verified, text) =
                    context_attachments::read_source(store.root(), &attachment_id)?;
                if verified.source_revision != source.source_revision {
                    return Err(invalid("source changed during reference admission"));
                }
                source = verified;
                Some(text)
            } else {
                None
            };
            Ok(MaterialAdmission {
                material,
                source_revision: source.source_revision,
                text,
                complete: source.coverage_complete,
            })
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct MaterialSearch {
    pub(crate) source_revision: String,
    pub(crate) material: MaterialEntry,
    pub(crate) query: String,
    pub(crate) hits: Vec<MaterialEvidence>,
    /// Completeness of this bounded retrieval, never a claim of corpus coverage.
    pub(crate) complete: bool,
    pub(crate) warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) folder: Option<FolderRetrieval>,
}

fn grant_key(store: &ProjectStore, id: &str) -> (PathBuf, String, String) {
    (
        store.root().to_path_buf(),
        store.manifest().project_id.to_string(),
        id.to_owned(),
    )
}

fn grants() -> &'static Mutex<LibraryGrants> {
    LIBRARY_GRANTS.get_or_init(Mutex::default)
}
fn invalid(message: impl Into<String>) -> MaterialError {
    MaterialError::Invalid(message.into())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn binding_id(source: &Source) -> Result<String> {
    Ok(format!("material-{}", digest(&serde_json::to_vec(source)?)))
}
fn qualified(binding: &Binding) -> String {
    format!("materials/{}#{}", binding.name, &binding.id[9..21])
}
fn reference(name: &str) -> Result<String> {
    Ok(format!("@{}", serde_json::to_string(name)?))
}
fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
        return Err(invalid("material name must contain 1–512 printable bytes"));
    }
    Ok(())
}
fn entry(store: &ProjectStore, binding: &Binding) -> Result<MaterialEntry> {
    let (kind, source_path, attachment_id, available) = match &binding.source {
        Source::Attachment { attachment_id } => (
            MaterialKind::Attachment,
            None,
            Some(attachment_id.clone()),
            true,
        ),
        Source::Library { path } => (
            MaterialKind::Library,
            Some(path.to_string_lossy().into_owned()),
            None,
            grants()
                .lock()
                .map_err(|_| invalid("library capability lock poisoned"))?
                .contains_key(&grant_key(store, &binding.id)),
        ),
    };
    Ok(MaterialEntry {
        id: binding.id.clone(),
        name: binding.name.clone(),
        reference: reference(&qualified(binding))?,
        kind,
        pinned: binding.pinned,
        available,
        source_path,
        attachment_id,
        workspace_path: binding.workspace_path.clone(),
    })
}

pub(crate) fn list(store: &ProjectStore) -> Result<Vec<MaterialEntry>> {
    read_bindings(store)?
        .items
        .iter()
        .map(|binding| entry(store, binding))
        .collect()
}
pub(crate) fn is_material_id(name: &str) -> bool {
    name.strip_prefix("material-").is_some_and(valid_hash)
}
pub(crate) fn resolve_optional(store: &ProjectStore, name: &str) -> Result<Option<MaterialEntry>> {
    match resolve(store, name) {
        Ok(entry) => Ok(Some(entry)),
        Err(MaterialError::NotFound(_)) => Ok(None),
        Err(error) => Err(error),
    }
}
pub(crate) fn resolve(store: &ProjectStore, name: &str) -> Result<MaterialEntry> {
    let bindings = read_bindings(store)?;
    if is_material_id(name) {
        return bindings
            .items
            .iter()
            .find(|binding| binding.id == name)
            .map(|binding| entry(store, binding))
            .transpose()?
            .ok_or_else(|| MaterialError::NotFound(name.into()));
    }

    let mut matches: Vec<_> = bindings
        .items
        .iter()
        .filter(|binding| binding.id == name || qualified(binding) == name)
        .collect();
    if matches.is_empty() {
        matches = bindings
            .items
            .iter()
            .filter(|binding| binding.workspace_path.as_deref() == Some(name))
            .collect();
    }
    if matches.is_empty() {
        matches = bindings.items.iter().filter(|binding| matches!(&binding.source, Source::Library { path } if path.to_str() == Some(name))).collect();
    }
    if matches.is_empty() {
        matches = bindings
            .items
            .iter()
            .filter(|binding| binding.name == name)
            .collect();
    }

    match matches.as_slice() {
        [binding] => entry(store, binding),
        [] => Err(MaterialError::NotFound(name.into())),
        _ => Err(MaterialError::Ambiguous(name.into())),
    }
}
fn binding(store: &ProjectStore, id: &str) -> Result<Binding> {
    read_bindings(store)?
        .items
        .into_iter()
        .find(|binding| binding.id == id)
        .ok_or_else(|| MaterialError::NotFound(id.into()))
}
fn save_binding(store: &ProjectStore, source: Source, name: &str) -> Result<Binding> {
    save_binding_at_path(store, source, name, None)
}

fn binding_identity(source: &Source, workspace_path: Option<&str>) -> Result<String> {
    match workspace_path {
        None => binding_id(source),
        Some(path) => {
            validate_workspace_path(path)?;
            if !matches!(source, Source::Attachment { .. }) {
                return Err(invalid(
                    "Only copied attachments have workspace placements.",
                ));
            }
            Ok(format!(
                "material-{}",
                digest(&serde_json::to_vec(&(
                    "loom.workspace-source.v1",
                    source,
                    path
                ))?)
            ))
        }
    }
}

fn save_binding_at_path(
    store: &ProjectStore,
    source: Source,
    name: &str,
    workspace_path: Option<&str>,
) -> Result<Binding> {
    validate_name(name)?;
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("material write lock poisoned"))?;
    let mut bindings = read_bindings(store)?;
    let id = binding_identity(&source, workspace_path)?;
    if let Some(existing) = bindings.items.iter().find(|binding| binding.id == id) {
        return Ok(existing.clone());
    }
    if bindings.items.len() >= MAX_BINDINGS {
        return Err(invalid("workspace material limit reached"));
    }
    let binding = Binding {
        id,
        name: name.to_owned(),
        pinned: false,
        source,
        workspace_path: workspace_path.map(str::to_owned),
    };
    bindings.items.push(binding.clone());
    write_bindings(store, &bindings)?;
    Ok(binding)
}
pub(crate) fn bind_attachment(
    store: &ProjectStore,
    attachment_id: &str,
    name: Option<&str>,
) -> Result<MaterialEntry> {
    let source = context_attachments::describe_source(store.root(), attachment_id)?;
    let binding = save_binding(
        store,
        Source::Attachment {
            attachment_id: attachment_id.into(),
        },
        name.unwrap_or(&source.file_name),
    )?;
    entry(store, &binding)
}

/// Each workspace placement has its own binding while retaining shared bytes.
pub(crate) fn bind_workspace_attachment(
    store: &ProjectStore,
    attachment_id: &str,
    relative_path: &str,
) -> Result<MaterialEntry> {
    validate_workspace_path(relative_path)?;
    context_attachments::describe_source(store.root(), attachment_id)?;
    let binding = save_binding_at_path(
        store,
        Source::Attachment {
            attachment_id: attachment_id.into(),
        },
        relative_path.rsplit('/').next().unwrap_or(relative_path),
        Some(relative_path),
    )?;
    entry(store, &binding)
}

fn validate_workspace_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.'))
    {
        return Err(invalid(
            "Use an ordinary project-relative workspace file path.",
        ));
    }
    Ok(())
}

/// Only call after a native file selection (or a separately authenticated
/// application-owned grant). Workspace JSON paths never call this function.
pub(crate) fn add_library(
    store: &ProjectStore,
    selected_path: &Path,
    name: Option<&str>,
) -> Result<MaterialEntry> {
    let metadata = fs::symlink_metadata(selected_path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("select an ordinary SQLite file"));
    }
    let path = fs::canonicalize(selected_path)?;
    let name = name.map_or_else(
        || {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        },
        str::to_owned,
    );
    validate_name(&name)?;
    let version = file_version(&path)?;
    let source = Source::Library { path: path.clone() };
    let id = binding_id(&source)?;
    let mut config = AlexandriaBackendConfig::new(
        &id,
        &name,
        ResourceId::parse(&id).map_err(|e| invalid(e.to_string()))?,
        ReleaseId::parse("local").map_err(|e| invalid(e.to_string()))?,
        RepresentationId::parse("alexandria-blocks-v1").map_err(|e| invalid(e.to_string()))?,
        path.clone(),
        ExternalAccessMode::LiveReadOnly,
        "Local source",
    );
    // Selection authorizes local use only. No redistribution or remote/model
    // service is authorized here. Corpus rights can further narrow permission.
    config.use_policy.model_context = UsePermission::Allowed;
    config.context_radius = 0;
    config.max_snippet_chars = 4096;
    let backend = AlexandriaBackend::open(config)
        .map_err(|e| invalid(format!("{}: {}", e.code, e.safe_message)))?;
    if file_version(&path)? != version {
        return Err(invalid("library changed while being selected"));
    }
    let backend = Arc::new(LibraryGrant {
        backend,
        path,
        version,
    });
    let binding = save_binding(store, source, &name)?;
    grants()
        .lock()
        .map_err(|_| invalid("library capability lock poisoned"))?
        .insert(grant_key(store, &binding.id), backend);
    entry(store, &binding)
}
/// Register an explicitly selected source and save native access together. A
/// failed private-state write rolls back the newly exposed binding/capability.
pub(crate) fn add_library_persisted(
    store: &ProjectStore,
    selected: &Path,
    grant_root: Option<&Path>,
) -> Result<MaterialEntry> {
    let source = Source::Library {
        path: fs::canonicalize(selected)?,
    };
    let id = binding_id(&source)?;
    let existed = read_bindings(store)?
        .items
        .iter()
        .any(|binding| binding.id == id);
    let previous = grants()
        .lock()
        .map_err(|_| invalid("library capability lock poisoned"))?
        .get(&grant_key(store, &id))
        .cloned();
    let entry = add_library(store, selected, None)?;
    if let Some(root) = grant_root
        && let Err(error) = persist_selected_grant(store, root, &entry.id)
    {
        let mut active = grants()
            .lock()
            .map_err(|_| invalid("library capability lock poisoned"))?;
        if let Some(previous) = previous {
            active.insert(grant_key(store, &id), previous);
        } else {
            active.remove(&grant_key(store, &id));
        }
        drop(active);
        if !existed {
            remove(store, &id)?;
        }
        return Err(error);
    }
    Ok(entry)
}

pub(crate) fn set_pinned(store: &ProjectStore, id: &str, pinned: bool) -> Result<MaterialEntry> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("material write lock poisoned"))?;
    let mut bindings = read_bindings(store)?;
    let binding = bindings
        .items
        .iter_mut()
        .find(|b| b.id == id)
        .ok_or_else(|| MaterialError::NotFound(id.into()))?;
    binding.pinned = pinned;
    let result = entry(store, binding)?;
    write_bindings(store, &bindings)?;
    Ok(result)
}
pub(crate) fn remove(store: &ProjectStore, id: &str) -> Result<()> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| invalid("material write lock poisoned"))?;
    let mut bindings = read_bindings(store)?;
    let previous = bindings.items.len();
    bindings.items.retain(|b| b.id != id);
    if previous == bindings.items.len() {
        return Err(MaterialError::NotFound(id.into()));
    }
    write_bindings(store, &bindings)?;
    grants()
        .lock()
        .map_err(|_| invalid("library capability lock poisoned"))?
        .remove(&grant_key(store, id));
    // Retained attachments and evidence are user data, not binding metadata.
    Ok(())
}

pub(crate) fn read(store: &ProjectStore, id: &str) -> Result<MaterialRead> {
    let binding = binding(store, id)?;
    let material = entry(store, &binding)?;
    match binding.source {
        Source::Library { .. } => Ok(MaterialRead {
            material,
            text: String::new(),
            complete: false,
            warnings: Vec::new(),
            source_revision: grants()
                .lock()
                .map_err(|_| invalid("library capability lock poisoned"))?
                .get(&grant_key(store, id))
                .map(|grant| grant.version.clone())
                .unwrap_or_default(),
            evidence: Vec::new(),
            presentation: None,
        }),
        Source::Attachment { attachment_id } => {
            let (presentation, text) =
                context_attachments::read_source(store.root(), &attachment_id)?;
            let evidence = retain_evidence(
                store,
                MaterialEvidence {
                    complete: presentation.coverage_complete,
                    warnings: presentation.warnings.clone(),
                    id: String::new(),
                    reference: String::new(),
                    material_id: id.into(),
                    title: material.name.clone(),
                    text_sha256: digest(text.as_bytes()),
                    text: text.clone(),
                    source_revision: presentation.source_revision.clone(),
                    locator: json!({"kind":"attachment_text", "attachment_id": attachment_id, "start_byte":0, "end_byte":text.len(), "pdf_pages":presentation.pdf_pages}),
                    source_evidence: None,
                },
            )?;
            Ok(MaterialRead {
                material,
                text,
                complete: presentation.coverage_complete,
                warnings: presentation.warnings.clone(),
                source_revision: presentation.source_revision.clone(),
                evidence: vec![evidence],
                presentation: Some(presentation),
            })
        }
    }
}

pub(crate) fn native_media(
    store: &ProjectStore,
    id: &str,
) -> Result<Vec<llama_native_types::MediaInput>> {
    match binding(store, id)?.source {
        Source::Attachment { attachment_id } => Ok(context_attachments::source_native_media(
            store.root(),
            &attachment_id,
        )?),
        Source::Library { .. } => Ok(Vec::new()),
    }
}

pub(crate) fn search(store: &ProjectStore, id: &str, query: &str) -> Result<MaterialSearch> {
    if query.trim().is_empty() || query.len() > MAX_QUERY_BYTES {
        return Err(invalid("search needs 1–4096 bytes of text"));
    }
    let binding = binding(store, id)?;
    let material = entry(store, &binding)?;
    match binding.source {
        Source::Library { .. } => search_library(store, material, query),
        Source::Attachment { attachment_id } => {
            search_attachment(store, material, &attachment_id, query)
        }
    }
}
fn library_query(backend: &AlexandriaBackend, text: &str) -> InformationQuery {
    InformationQuery {
        schema: QUERY_SCHEMA.into(),
        query_id: QueryId::new(),
        text: text.into(),
        syntax: QuerySyntax::NaturalTerms,
        purpose: RetrievalPurpose::ModelContext,
        targets: vec![RetrievalTarget {
            resource_id: backend.descriptor().resource_id.clone(),
            release_id: backend.descriptor().release_id.clone(),
            representation_id: backend.descriptor().representation_id.clone(),
        }],
        resources: Vec::new(),
        representations: Vec::new(),
        filters: QueryFilters::default(),
        budget: QueryBudget {
            max_hits: MAX_HITS,
            max_hits_per_backend: MAX_HITS,
            max_backends: 1,
            max_context_chars: MAX_CONTEXT_CHARS,
            timeout_ms: 1500,
        },
    }
}

fn search_library(
    store: &ProjectStore,
    material: MaterialEntry,
    text: &str,
) -> Result<MaterialSearch> {
    let backend = grants()
        .lock()
        .map_err(|_| invalid("library capability lock poisoned"))?
        .get(&grant_key(store, &material.id))
        .cloned()
        .ok_or_else(|| MaterialError::NeedsAuthorization(material.name.clone()))?;
    let query = library_query(&backend.backend, text);
    if file_version(&backend.path)? != backend.version {
        return Err(invalid(
            "library changed; select it again to use its new version",
        ));
    }
    let mut result = backend
        .backend
        .search(&query)
        .map_err(|e| invalid(format!("{}: {}", e.code, e.safe_message)))?;
    if file_version(&backend.path)? != backend.version {
        return Err(invalid("library changed during search"));
    }
    let mut hits = Vec::with_capacity(result.hits.len());
    let share = MAX_CONTEXT_CHARS / u32::try_from(result.hits.len()).unwrap_or(MAX_HITS).max(1);
    let started = std::time::Instant::now();
    for found in result.hits {
        found.validate().map_err(|e| invalid(e.to_string()))?;
        let remaining = 1500_u64
            .saturating_sub(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        if remaining == 0 {
            result.complete = false;
            result
                .warnings
                .push("Passage reading reached its time budget.".into());
            break;
        }
        let read = backend
            .backend
            .read(&ReadRequest {
                resource_id: found.resource_id,
                release_id: found.release_id,
                representation_id: found.representation_id,
                purpose: RetrievalPurpose::ModelContext,
                locator: found.locator,
                max_context_chars: share.min(4096),
                timeout_ms: remaining,
            })
            .map_err(|e| invalid(format!("{}: {}", e.code, e.safe_message)))?;
        result.complete &= read.complete;
        result.warnings.extend(read.warnings.clone());
        // A direct read returns an exact source prefix; search snippets contain
        // FTS highlighting and must never masquerade as source bytes.
        let hit = read.hit;
        hit.validate().map_err(|e| invalid(e.to_string()))?;
        let text = hit.snippet.clone();
        hits.push(retain_evidence(
            store,
            MaterialEvidence {
                complete: read.complete,
                warnings: read.warnings,
                id: String::new(),
                reference: String::new(),
                material_id: material.id.clone(),
                title: hit.title.clone(),
                text_sha256: digest(text.as_bytes()),
                text,
                source_revision: backend.version.clone(),
                locator: serde_json::to_value(&hit.locator)?,
                source_evidence: Some(hit),
            },
        )?);
    }
    if file_version(&backend.path)? != backend.version {
        return Err(invalid("library changed while reading passages"));
    }
    result.warnings.sort();
    result.warnings.dedup();
    Ok(MaterialSearch {
        source_revision: backend.version.clone(),
        material,
        query: text.into(),
        hits,
        complete: result.complete,
        warnings: result.warnings,
        folder: None,
    })
}
fn search_attachment(
    store: &ProjectStore,
    material: MaterialEntry,
    attachment_id: &str,
    query: &str,
) -> Result<MaterialSearch> {
    let (presentation, source) = context_attachments::read_source(store.root(), attachment_id)?;
    let selection = select_passages(
        &source,
        query,
        MAX_HITS as usize,
        MAX_CONTEXT_CHARS as usize,
    );
    let complete = presentation.coverage_complete && selection.complete;
    let mut hits = Vec::new();
    for passage in selection.passages {
        let text = &source[passage.range.clone()];
        hits.push(retain_evidence(store, MaterialEvidence {
            complete: presentation.coverage_complete && passage.complete,
            warnings: if passage.complete { presentation.warnings.clone() } else { vec!["This is a bounded passage from the source.".into()] },
            id: String::new(), reference: String::new(), material_id: material.id.clone(), title: material.name.clone(),
            text: text.into(), text_sha256: digest(text.as_bytes()), source_revision: presentation.source_revision.clone(),
            locator: json!({"kind":"attachment_text", "attachment_id":attachment_id,"start_byte":passage.range.start,"end_byte":passage.range.end,
                "pdf_pages":presentation.pdf_pages.iter().filter(|page| page.start_byte < passage.range.end && page.end_byte > passage.range.start).collect::<Vec<_>>() }), source_evidence: None,
        })?);
    }
    let mut warnings = presentation.warnings;
    if !complete {
        warnings.push("The result contains bounded passages, not the complete source.".into());
    }
    Ok(MaterialSearch {
        source_revision: presentation.source_revision,
        material,
        query: query.into(),
        complete,
        hits,
        warnings,
        folder: None,
    })
}

struct TextPassage {
    range: std::ops::Range<usize>,
    complete: bool,
}

struct PassageSelection {
    passages: Vec<TextPassage>,
    complete: bool,
    result_limit_reached: bool,
}

/// Literal UTF-8 slices shared by imported-source and registered-folder search.
fn select_passages(
    source: &str,
    query: &str,
    max_hits: usize,
    mut remaining: usize,
) -> PassageSelection {
    let terms = query
        .split_whitespace()
        .take(64)
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    let mut selection = PassageSelection {
        passages: Vec::new(),
        complete: true,
        result_limit_reached: false,
    };
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let folded = line.to_lowercase();
        let found = terms
            .iter()
            .filter_map(|term| folded.find(term).map(|at| (at, term.len())))
            .min_by_key(|(at, _)| *at);
        if let Some((at, term_bytes)) = found {
            if selection.passages.len() == max_hits || remaining == 0 {
                selection.complete = false;
                selection.result_limit_reached = true;
                break;
            }
            let mut folded_offset = 0;
            let original_at = line
                .char_indices()
                .find_map(|(index, ch)| {
                    let width: usize = ch.to_lowercase().map(char::len_utf8).sum();
                    let found = (at < folded_offset + width).then_some(index);
                    folded_offset += width;
                    found
                })
                .unwrap_or(0);
            let mut start = original_at.saturating_sub(256);
            while !line.is_char_boundary(start) {
                start -= 1;
            }
            let width = 2048_usize
                .max(term_bytes.saturating_mul(4).saturating_add(256))
                .min(remaining);
            let mut end = (start + width).min(line.len());
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            if end <= original_at {
                selection.complete = false;
                selection.result_limit_reached = true;
                break;
            }
            let complete = start == 0 && end == line.len();
            selection.complete &= complete;
            remaining -= end - start;
            selection.passages.push(TextPassage {
                range: offset + start..offset + end,
                complete,
            });
        }
        offset += line.len();
    }
    selection
}

fn evidence_payload(evidence: &MaterialEvidence) -> Result<Vec<u8>> {
    let mut payload = evidence.clone();
    payload.id.clear();
    payload.reference.clear();
    Ok(serde_json::to_vec(&payload)?)
}
fn retain_evidence(
    store: &ProjectStore,
    mut evidence: MaterialEvidence,
) -> Result<MaterialEvidence> {
    let bytes = evidence_payload(&evidence)?;
    if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
        return Err(invalid("source evidence exceeds retained evidence limit"));
    }
    evidence.id = digest(&bytes);
    evidence.reference = reference(&format!("evidence/{}", evidence.id))?;
    let path = storage(store)?
        .join("evidence")
        .join(format!("{}.json", evidence.id));
    install_evidence(&path, &bytes)?;
    Ok(evidence)
}
pub(crate) fn read_evidence(
    store: &ProjectStore,
    material_id: &str,
    evidence_id: &str,
) -> Result<MaterialEvidence> {
    let evidence = load_evidence(store, evidence_id)?;
    if !material_id.is_empty() && evidence.material_id != material_id {
        return Err(invalid("evidence belongs to another source"));
    }
    Ok(evidence)
}
pub(crate) fn resolve_evidence_reference(
    store: &ProjectStore,
    name: &str,
) -> Result<Option<MaterialEvidence>> {
    name.strip_prefix("evidence/")
        .map(|id| load_evidence(store, id))
        .transpose()
}
fn load_evidence(store: &ProjectStore, id: &str) -> Result<MaterialEvidence> {
    if !valid_hash(id) {
        return Err(invalid("invalid evidence identity"));
    }
    let bytes = read_safe(
        &storage(store)?.join("evidence").join(format!("{id}.json")),
        MAX_EVIDENCE_BYTES,
    )?;
    if digest(&bytes) != id {
        return Err(invalid("retained evidence identity mismatch"));
    }
    let mut evidence: MaterialEvidence = serde_json::from_slice(&bytes)?;
    if digest(evidence.text.as_bytes()) != evidence.text_sha256 {
        return Err(invalid("retained evidence text mismatch"));
    }
    evidence.id = id.into();
    evidence.reference = reference(&format!("evidence/{id}"))?;
    Ok(evidence)
}

fn storage(store: &ProjectStore) -> Result<PathBuf> {
    let root = store.root().join(".loom").join("materials");
    for path in [
        store.root().join(".loom"),
        root.clone(),
        root.join("evidence"),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
            Ok(_) => return Err(invalid("material storage must be ordinary directories")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&path)?,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(root)
}
fn read_safe(path: &Path, max: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max {
        return Err(invalid("unsafe or oversized material metadata"));
    }
    let file = File::open(path)?;
    let identity = same_file::Handle::from_file(file.try_clone()?)?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max
        || same_file::Handle::from_path(path)? != identity
        || fs::symlink_metadata(path)?.file_type().is_symlink()
    {
        return Err(invalid("material metadata changed during read"));
    }
    Ok(bytes)
}
fn read_bindings(store: &ProjectStore) -> Result<Bindings> {
    let path = storage(store)?.join("bindings.json");
    let bytes = match read_safe(&path, MAX_STATE_BYTES) {
        Ok(bytes) => bytes,
        Err(MaterialError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Bindings {
                schema: SCHEMA.into(),
                items: Vec::new(),
            });
        }
        Err(e) => return Err(e),
    };
    let bindings: Bindings = serde_json::from_slice(&bytes)?;
    if bindings.schema != SCHEMA || bindings.items.len() > MAX_BINDINGS {
        return Err(invalid("unsupported material bindings"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for binding in &bindings.items {
        validate_name(&binding.name)?;
        if binding.id != binding_identity(&binding.source, binding.workspace_path.as_deref())?
            || !ids.insert(&binding.id)
        {
            return Err(invalid("material binding identity mismatch"));
        }
        match &binding.source {
            Source::Attachment { attachment_id } if !valid_hash(attachment_id) => {
                return Err(invalid("invalid attachment identity"));
            }
            Source::Library { path } if !path.is_absolute() => {
                return Err(invalid("library path is not absolute"));
            }
            _ => {}
        }
    }
    Ok(bindings)
}
fn write_bindings(store: &ProjectStore, bindings: &Bindings) -> Result<()> {
    let bytes = serde_json::to_vec(bindings)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid("workspace material metadata limit reached"));
    }
    let path = storage(store)?.join("bindings.json");
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err(invalid("unsafe material bindings file"));
    }
    let mut file = AtomicWriteFile::open(&path)?;
    file.write_all(&bytes)?;
    file.commit()?;
    Ok(())
}

// A registration pins the local file identity, not a claim of a whole-file
// digest. Exact returned bytes and the backend transaction fingerprint are
// retained in each immutable evidence envelope.
fn file_version(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("library is no longer an ordinary file"));
    }
    let mut identity = format!("{}:{:?}", metadata.len(), metadata.modified()?);
    #[cfg(unix)]
    {
        use std::fmt::Write as _;
        use std::os::unix::fs::MetadataExt as _;
        let _ = write!(
            identity,
            ":{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec()
        );
    }
    Ok(format!(
        "local-file-identity-v1:{}",
        digest(identity.as_bytes())
    ))
}

fn install_evidence(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.try_exists()? {
        if read_safe(path, MAX_EVIDENCE_BYTES)? != bytes {
            return Err(invalid("retained evidence identity mismatch"));
        }
        return Ok(());
    }
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_extension(format!("{}.{sequence}.tmp", std::process::id()));
    let mut created = false;
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        created = true;
        file.write_all(bytes)?;
        file.sync_all()?;
        match fs::hard_link(&temp, path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if read_safe(path, MAX_EVIDENCE_BYTES)? != bytes {
                    return Err(invalid("retained evidence identity mismatch"));
                }
            }
            Err(error) => return Err(error.into()),
        }
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if created {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project() -> (tempfile::TempDir, ProjectStore) {
        let temp = tempfile::tempdir().unwrap();
        let (store, _) = ProjectStore::initialize(temp.path().join("Writing"), "Writing").unwrap();
        (temp, store)
    }
    pub(super) fn database(path: &Path) {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute_batch(include_str!("materials/alexandria-fixture.sql"))
            .unwrap();
    }
    fn source(store: &ProjectStore, text: &str, name: &str) -> MaterialEntry {
        let path = store.root().join("fixture.txt");
        fs::write(&path, text).unwrap();
        let source = context_attachments::import_path(store.root(), &path).unwrap();
        bind_attachment(store, &source.id, Some(name)).unwrap()
    }
    #[cfg(unix)]
    #[test]
    fn workspace_copies_preserve_distinct_placements_and_shared_original_identity() {
        let (_temp, store) = project();
        let original = source(&store, "One shared original.", "Original");
        let attachment_id = original.attachment_id.as_deref().unwrap();
        let first =
            bind_workspace_attachment(&store, attachment_id, "Research/source.txt").unwrap();
        let second = bind_workspace_attachment(&store, attachment_id, "Drafts/source.txt").unwrap();
        assert_ne!(first.id, second.id);
        assert_ne!(first.id, original.id);
        assert_eq!(first.attachment_id, second.attachment_id);
        assert_eq!(first.workspace_path.as_deref(), Some("Research/source.txt"));
        assert_eq!(second.workspace_path.as_deref(), Some("Drafts/source.txt"));
        assert_eq!(resolve(&store, "Research/source.txt").unwrap().id, first.id);
        assert_eq!(resolve(&store, "Drafts/source.txt").unwrap().id, second.id);
        assert_eq!(
            bind_workspace_attachment(&store, attachment_id, "Research/source.txt")
                .unwrap()
                .id,
            first.id
        );
        assert_eq!(list(&store).unwrap().len(), 3);
        for path in [
            "../source.txt",
            "/source.txt",
            ".loom/source.txt",
            "Notes/.hidden",
            "Notes//source.txt",
            "Notes\\source.txt",
        ] {
            assert!(
                bind_workspace_attachment(&store, attachment_id, path).is_err(),
                "accepted {path}"
            );
        }
    }
    #[test]
    fn admitting_large_source_does_not_publish_or_load_a_full_text_value() {
        let (_temp, store) = project();
        let entry = source(&store, &"words ".repeat(20_000), "Book");
        let admission = admit(&store, &entry.id, 65_536).unwrap();
        assert!(admission.text.is_none());
        assert!(admission.complete);
        assert!(
            fs::read_dir(storage(&store).unwrap().join("evidence"))
                .unwrap()
                .next()
                .is_none()
        );
        let small = source(&store, "Short text.", "Note");
        assert!(
            admit(&store, &small.id, 65_536)
                .unwrap()
                .text
                .unwrap()
                .contains("Short text.")
        );
    }
    #[test]
    fn names_fail_on_ambiguity_qualified_names_remain_exact() {
        let (_temp, store) = project();
        assert!(list(&store).unwrap().is_empty());
        let first = source(&store, "First source", "Research");
        let second = source(&store, "Second source", "Research");
        assert!(matches!(
            resolve(&store, "Research"),
            Err(MaterialError::Ambiguous(_))
        ));
        for entry in [first, second] {
            let reference: String = serde_json::from_str(&entry.reference[1..]).unwrap();
            assert_eq!(resolve(&store, &reference).unwrap().id, entry.id);
        }
    }
    #[test]
    fn removing_binding_preserves_source_and_exact_retained_evidence() {
        let (_temp, store) = project();
        let entry = source(&store, "A line with café 🦉.\r\nSecond line.\n", "Notes");
        let read = read(&store, &entry.id).unwrap();
        let before = read.evidence[0].clone();
        set_pinned(&store, &entry.id, true).unwrap();
        assert!(list(&store).unwrap()[0].pinned);
        remove(&store, &entry.id).unwrap();
        assert!(list(&store).unwrap().is_empty());
        assert_eq!(
            read_evidence(&store, &entry.id, &before.id).unwrap().text,
            read.text
        );
        assert!(
            context_attachments::original_path(
                store.root(),
                entry.attachment_id.as_deref().unwrap()
            )
            .unwrap()
            .is_file()
        );
    }
    #[test]
    fn sqlite_search_preserves_source_and_retains_evidence_after_source_change() {
        let (temp, store) = project();
        let path = temp.path().join("library.sqlite3");
        database(&path);
        let original = fs::read(&path).unwrap();
        let entry = add_library(&store, &path, Some("Library")).unwrap();
        let result = search(&store, &entry.id, "prayer").unwrap();
        assert!(!result.hits.is_empty());
        let hit = &result.hits[0];
        assert!(hit.source_evidence.is_some());
        assert!(!hit.text.contains("[prayer]"));
        assert!(!hit.text.starts_with("[D1:"));
        assert!(
            original
                .windows(hit.text.len())
                .any(|window| window == hit.text.as_bytes())
        );
        assert_eq!(hit.locator["kind"], "sqlite_block");
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("sqlite3-wal").exists());
        assert!(!path.with_extension("sqlite3-shm").exists());
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute("UPDATE documents SET title = 'Changed'", [])
            .unwrap();
        drop(connection);
        assert!(search(&store, &entry.id, "prayer").is_err());
        assert_eq!(
            read_evidence(&store, &entry.id, &hit.id).unwrap().text,
            hit.text
        );
    }
    #[test]
    fn workspace_metadata_cannot_replay_local_file_authority() {
        let (temp, store) = project();
        let path = temp.path().join("library.sqlite3");
        database(&path);
        let entry = add_library(&store, &path, None).unwrap();
        grants()
            .lock()
            .unwrap()
            .remove(&grant_key(&store, &entry.id));
        assert!(!list(&store).unwrap()[0].available);
        assert!(matches!(
            search(&store, &entry.id, "prayer"),
            Err(MaterialError::NeedsAuthorization(_))
        ));
        assert!(add_library(&store, &path, None).unwrap().available);
    }
    #[test]
    fn nonempty_wal_is_rejected_without_creating_a_binding() {
        let (temp, store) = project();
        let path = temp.path().join("library.sqlite3");
        database(&path);
        fs::write(path.with_extension("sqlite3-wal"), b"uncheckpointed").unwrap();
        assert!(add_library(&store, &path, None).is_err());
        assert!(list(&store).unwrap().is_empty());
    }
    #[test]
    fn attachment_search_preserves_unicode_offsets_across_chunk_boundaries() {
        let (_temp, store) = project();
        let text = format!("{}Kelvin \u{212a}elvin café 🦉 finish", "x".repeat(2045));
        let entry = source(&store, &text, "Unicode");
        let result = search(&store, &entry.id, "kelvin").unwrap();
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        let start = usize::try_from(hit.locator["start_byte"].as_u64().unwrap()).unwrap();
        let end = usize::try_from(hit.locator["end_byte"].as_u64().unwrap()).unwrap();
        let retained = read(&store, &entry.id).unwrap();
        assert_eq!(hit.text, retained.text[start..end]);
        assert!(hit.text.contains("Kelvin"));
        assert!(hit.text.contains("\u{212a}elvin"));
    }
    #[test]
    fn retained_evidence_tampering_is_not_reinterpreted_as_source() {
        let (_temp, store) = project();
        let entry = source(&store, "Unchanged original", "Notes");
        let evidence = read(&store, &entry.id).unwrap().evidence.remove(0);
        fs::write(
            storage(&store)
                .unwrap()
                .join("evidence")
                .join(format!("{}.json", evidence.id)),
            b"{}",
        )
        .unwrap();
        assert!(read_evidence(&store, &entry.id, &evidence.id).is_err());
    }
}
