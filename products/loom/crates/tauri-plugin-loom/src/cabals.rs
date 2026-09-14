//! Shared workspaces have one CRDT owner and ordinary Markdown projections.
//! Projection attempts use the existing immutable save receipts to settle
//! crashes between the CRDT commit and the visible file commit.
use super::*;
use fs2::FileExt;
use loom_cabal::{
    Cabal, DocumentView, Edit, EditResult, Identity, Invitation, Network, NetworkMode, PeerStatus,
    Roster,
};
use std::io::Write;
use uuid::Uuid;

type Shared = Arc<Mutex<Cabal>>;

#[derive(Debug, Default)]
pub(crate) struct CabalService {
    profile: tokio::sync::Mutex<Option<Profile>>,
    closed: AtomicBool,
}

#[derive(Debug)]
struct Profile {
    directory: PathBuf,
    identity: Identity,
    network: Arc<Network>,
    cabals: BTreeMap<Uuid, Shared>,
    bindings: BTreeMap<PathBuf, Uuid>,
    _lease: File,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema: u32,
    bindings: BTreeMap<PathBuf, Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Projection {
    base: DocumentView,
    pending: Option<ProjectionAttempt>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProjectionAttempt {
    view: DocumentView,
    command: CommandId,
    revision: RevisionId,
    blob: BlobId,
    kind: DocumentKind,
}

#[derive(Debug, Serialize)]
pub(crate) struct CabalSnapshot {
    id: Uuid,
    name: String,
    roster: Roster,
    roster_hash: String,
    my_key: String,
    peers: Vec<PeerStatus>,
    documents: Vec<SharedDocument>,
    orphaned_changes: usize,
}

#[derive(Debug, Serialize)]
struct SharedDocument {
    shared: DocumentView,
    local: OpenDocument,
}

#[derive(Debug, Serialize)]
pub(crate) struct CabalEditReply {
    local_heads: Vec<String>,
    shared: DocumentView,
    local: OpenDocument,
}

fn failure(error: impl std::fmt::Display) -> IpcFailure {
    IpcFailure::new("cabal_failed", error.to_string(), false)
}
fn directory(state: &PluginState) -> Result<PathBuf, IpcFailure> {
    state
        .app_local_data_root
        .as_ref()
        .map(|root| root.join("cabals"))
        .ok_or_else(|| failure("Cabal storage is unavailable"))
}

impl CabalService {
    async fn start(&self, directory: &Path) -> Result<(), IpcFailure> {
        let mut slot = self.profile.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(failure("Cabals are closing"));
        }
        if slot.is_some() {
            return Ok(());
        }
        std::fs::create_dir_all(directory).map_err(failure)?;
        let lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("profile.lock"))
            .map_err(failure)?;
        lease
            .try_lock_exclusive()
            .map_err(|_| failure("Another Loom process owns this cabal profile"))?;
        let identity = Identity::open(directory).map_err(failure)?;
        let bindings = match std::fs::read(directory.join("index.json")) {
            Ok(bytes) => {
                if bytes.len() > 65536 {
                    return Err(failure("Cabal index exceeds limit"));
                }
                let index: Index = serde_json::from_slice(&bytes).map_err(failure)?;
                if index.schema != 1 || index.bindings.len() > 16 {
                    return Err(failure("Unsupported cabal index"));
                }
                index.bindings
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(failure(error)),
        };
        let mut cabals = BTreeMap::new();
        for id in bindings.values() {
            if cabals.contains_key(id) {
                continue;
            }
            let cabal = Arc::new(Mutex::new(
                Cabal::open(&directory.join(format!("{id}.db")), identity.clone())
                    .map_err(failure)?,
            ));
            cabals.insert(*id, cabal);
        }
        let network = Arc::new(
            Network::start(&identity, NetworkMode::Internet)
                .await
                .map_err(failure)?,
        );
        for cabal in cabals.values() {
            if let Err(error) = network.add(cabal.clone()) {
                let _ = network.shutdown().await;
                return Err(failure(error));
            }
        }
        *slot = Some(Profile {
            directory: directory.into(),
            identity,
            network,
            cabals,
            bindings,
            _lease: lease,
        });
        Ok(())
    }

    async fn bound(
        &self,
        directory: &Path,
        root: &Path,
    ) -> Result<Option<(Shared, Arc<Network>)>, IpcFailure> {
        if self.profile.lock().await.is_none() && !directory.join("index.json").is_file() {
            return Ok(None);
        }
        self.start(directory).await?;
        let slot = self.profile.lock().await;
        let profile = slot
            .as_ref()
            .ok_or_else(|| failure("Cabal profile is closed"))?;
        Ok(profile
            .bindings
            .get(root)
            .and_then(|id| profile.cabals.get(id))
            .map(|cabal| (cabal.clone(), profile.network.clone())))
    }

    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(profile) = self.profile.lock().await.take() {
            let _ = profile.network.shutdown().await;
        }
    }
}

impl Profile {
    fn save_index(&self) -> Result<(), IpcFailure> {
        let mut file = atomic_write_file::AtomicWriteFile::open(self.directory.join("index.json"))
            .map_err(failure)?;
        file.write_all(
            &serde_json::to_vec(&Index {
                schema: 1,
                bindings: self.bindings.clone(),
            })
            .map_err(failure)?,
        )
        .map_err(failure)?;
        file.commit().map_err(failure)
    }
}

fn root_for(state: &PluginState, project: &str, session: &str) -> Result<PathBuf, IpcFailure> {
    let mut owner = lock_session(state)?;
    Ok(require_bound_store(&mut owner, project, session)?
        .root()
        .to_owned())
}

#[tauri::command]
pub(crate) async fn cabal_snapshot(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<Option<CabalSnapshot>, IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    let Some((cabal, network)) = state.cabals.bound(&directory(&state)?, &root).await? else {
        return Ok(None);
    };
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let mut cabal = cabal.lock().map_err(|_| failure("Cabal owner stopped"))?;
    let mut documents = Vec::new();
    for view in cabal.views().map_err(failure)? {
        documents.push(project_document(store, &mut cabal, view.id)?);
    }
    Ok(Some(CabalSnapshot {
        id: cabal.id(),
        name: cabal.roster().payload.name.clone(),
        roster: cabal.roster().clone(),
        roster_hash: cabal.roster().hash().map_err(failure)?,
        my_key: cabal.identity().public_key().to_string(),
        peers: network.status().borrow().clone(),
        documents,
        orphaned_changes: cabal.orphaned_changes().map_err(failure)?,
    }))
}

#[tauri::command]
pub(crate) async fn cabal_share(
    project_id: String,
    session_id: String,
    name: String,
    display_name: String,
    state: State<'_, PluginState>,
) -> Result<String, IpcFailure> {
    ensure_application_running(&state, "cabal sharing")?;
    let root = root_for(&state, &project_id, &session_id)?;
    let directory = directory(&state)?;
    state.cabals.start(&directory).await?;
    let mut slot = state.cabals.profile.lock().await;
    let profile = slot
        .as_mut()
        .ok_or_else(|| failure("Cabal profile is closed"))?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let cabal = if let Some(id) = profile.bindings.get(&root) {
        profile
            .cabals
            .get(id)
            .cloned()
            .ok_or_else(|| failure("Cabal binding is unavailable"))?
    } else {
        if profile.bindings.len() >= 16 {
            return Err(failure("The cabal profile is full"));
        }
        // Create under a private temporary name, then publish its stable identity.
        let temporary = directory.join(format!("{}.db", Uuid::new_v4()));
        let mut cabal = Cabal::create(&temporary, profile.identity.clone(), &name, &display_name)
            .map_err(failure)?;
        let sources = store.list_documents().map_err(IpcFailure::store)?;
        for source in sources
            .iter()
            .filter(|source| shareable(&source.relative_path))
        {
            let loaded = store
                .read_document(&source.relative_path)
                .map_err(IpcFailure::store)?;
            let view = cabal
                .create_document(&source.relative_path, &loaded.text)
                .map_err(failure)?;
            cabal
                .set_local_record(
                    &projection_key(view.id),
                    &Projection {
                        base: view,
                        pending: None,
                    },
                )
                .map_err(failure)?;
        }
        let id = cabal.id();
        drop(cabal);
        std::fs::rename(&temporary, directory.join(format!("{id}.db"))).map_err(failure)?;
        let cabal = Arc::new(Mutex::new(
            Cabal::open(
                &directory.join(format!("{id}.db")),
                profile.identity.clone(),
            )
            .map_err(failure)?,
        ));
        profile.network.add(cabal.clone()).map_err(failure)?;
        profile.cabals.insert(id, cabal.clone());
        profile.bindings.insert(root, id);
        profile.save_index()?;
        cabal
    };
    cabal
        .lock()
        .map_err(|_| failure("Cabal owner stopped"))?
        .invite(profile.network.address())
        .and_then(|invitation| invitation.encode())
        .map_err(failure)
}

#[tauri::command]
pub(crate) async fn cabal_join(
    invitation: String,
    display_name: String,
    state: State<'_, PluginState>,
) -> Result<Option<String>, IpcFailure> {
    ensure_application_running(&state, "joining a cabal")?;
    let invitation = Invitation::decode(&invitation).map_err(failure)?;
    let directory = directory(&state)?;
    state.cabals.start(&directory).await?;
    let network = state
        .cabals
        .profile
        .lock()
        .await
        .as_ref()
        .ok_or_else(|| failure("Cabal profile is closed"))?
        .network
        .clone();
    let roster = network
        .join(&invitation, &display_name)
        .await
        .map_err(failure)?;
    let root = {
        let mut slot = state.cabals.profile.lock().await;
        let profile = slot
            .as_mut()
            .ok_or_else(|| failure("Cabal profile is closed"))?;
        if let Some((root, _)) = profile
            .bindings
            .iter()
            .find(|(_, id)| **id == invitation.cabal)
        {
            root.clone()
        } else {
            let root = directory
                .join("workspaces")
                .join(invitation.cabal.to_string());
            std::fs::create_dir_all(&root).map_err(failure)?;
            let root = root.canonicalize().map_err(failure)?;
            let path = directory.join(format!("{}.db", invitation.cabal));
            let cabal = if path.exists() {
                Cabal::open(&path, profile.identity.clone())
            } else {
                Cabal::import(&path, profile.identity.clone(), roster)
            }
            .map_err(failure)?;
            cabal.remember_peer(&invitation.owner).map_err(failure)?;
            let cabal = Arc::new(Mutex::new(cabal));
            profile.network.add(cabal.clone()).map_err(failure)?;
            profile.cabals.insert(invitation.cabal, cabal);
            profile.bindings.insert(root.clone(), invitation.cabal);
            profile.save_index()?;
            root
        }
    };
    let cabal = state
        .cabals
        .profile
        .lock()
        .await
        .as_ref()
        .and_then(|profile| profile.cabals.get(&invitation.cabal))
        .cloned()
        .ok_or_else(|| failure("Cabal is unavailable"))?;
    // A bounded initial pull gives the new project real documents before its
    // editor is attached; the supervisor continues the remaining catch-up.
    network
        .sync_now(cabal.clone(), invitation.owner.id)
        .await
        .map_err(failure)?;
    {
        let mut store = ProjectStore::open_folder(&root).map_err(IpcFailure::store)?;
        let mut cabal = cabal.lock().map_err(|_| failure("Cabal owner stopped"))?;
        for view in cabal.views().map_err(failure)? {
            project_document(&mut store, &mut cabal, view.id)?;
        }
    }
    let _admission = lock_application_admission(&state, "opening a cabal")?;
    prepare_project_folder(&state, Some(root))
}

#[tauri::command]
pub(crate) async fn cabal_edit(
    project_id: String,
    session_id: String,
    edit: Edit,
    state: State<'_, PluginState>,
) -> Result<CabalEditReply, IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    let (cabal, _) = state
        .cabals
        .bound(&directory(&state)?, &root)
        .await?
        .ok_or_else(|| failure("This workspace is not a cabal"))?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let mut cabal = cabal.lock().map_err(|_| failure("Cabal owner stopped"))?;
    let EditResult {
        local_heads,
        merged,
    } = cabal.edit(&edit).map_err(failure)?;
    let projected = project_document(store, &mut cabal, merged.id)?;
    Ok(CabalEditReply {
        local_heads,
        shared: projected.shared,
        local: projected.local,
    })
}

#[tauri::command]
pub(crate) async fn cabal_revoke(
    project_id: String,
    session_id: String,
    member_key: String,
    roster_hash: String,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    ensure_application_running(&state, "cabal membership")?;
    let root = root_for(&state, &project_id, &session_id)?;
    let (shared, _) = state
        .cabals
        .bound(&directory(&state)?, &root)
        .await?
        .ok_or_else(|| failure("This workspace is not a cabal"))?;
    let mut session = lock_session(&state)?;
    require_bound_store(&mut session, &project_id, &session_id)?;
    let mut cabal = shared.lock().map_err(|_| failure("Cabal owner stopped"))?;
    if cabal.roster().hash().map_err(failure)? != roster_hash {
        return Err(failure(
            "Membership changed. Review the current members before removing someone.",
        ));
    }
    let key = member_key
        .parse()
        .map_err(|_| failure("Invalid member key"))?;
    cabal.revoke(key).map_err(failure)
}

#[tauri::command]
pub(crate) async fn cabal_recover(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<Vec<String>, IpcFailure> {
    ensure_application_running(&state, "cabal recovery")?;
    let root = root_for(&state, &project_id, &session_id)?;
    let (shared, _) = state
        .cabals
        .bound(&directory(&state)?, &root)
        .await?
        .ok_or_else(|| failure("This workspace is not a cabal"))?;
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let cabal = shared.lock().map_err(|_| failure("Cabal owner stopped"))?;
    let mut paths = Vec::new();
    for view in cabal.orphaned_documents().map_err(failure)? {
        let digest = format!("{:x}", Sha256::digest(view.text.as_bytes()));
        let path = format!("Recovery/Cabal-{}-{}.md", view.id, &digest[..16]);
        if let Some(existing) = store
            .list_documents()
            .map_err(IpcFailure::store)?
            .iter()
            .find(|item| item.relative_path == path)
        {
            let text = store
                .read_document(&existing.relative_path)
                .map_err(IpcFailure::store)?
                .text;
            if text != view.text {
                return Err(failure(
                    "A previous recovery copy was edited. Preserve it before recovering this version again.",
                ));
            }
        } else {
            store
                .create_document_if_absent(
                    &path,
                    DocumentContent::Prose(view.text),
                    "recover orphaned cabal edits",
                )
                .map_err(IpcFailure::store)?;
        }
        paths.push(path);
    }
    Ok(paths)
}

fn projection_key(id: Uuid) -> String {
    format!("projection:{id}")
}

fn shareable(path: &str) -> bool {
    !path.starts_with("Runs/") && !path.starts_with("Recovery/") && !path.starts_with('.')
}

fn projection_for(
    store: &ProjectStore,
    cabal: &Cabal,
    view: &DocumentView,
) -> Result<Projection, IpcFailure> {
    let key = projection_key(view.id);
    if let Some(projection) = cabal.local_record(&key).map_err(failure)? {
        return Ok(projection);
    }
    if store
        .list_documents()
        .map_err(IpcFailure::store)?
        .iter()
        .any(|item| item.relative_path == view.name)
    {
        return Err(failure(format!(
            "{} already exists outside this cabal; its contents were preserved",
            view.name
        )));
    }
    let projection = Projection {
        base: view.clone(),
        pending: None,
    };
    cabal.set_local_record(&key, &projection).map_err(failure)?;
    Ok(projection)
}

fn project_document(
    store: &mut ProjectStore,
    cabal: &mut Cabal,
    id: Uuid,
) -> Result<SharedDocument, IpcFailure> {
    let mut view = cabal.view(id).map_err(failure)?;
    if !shareable(&view.name)
        || Path::new(&view.name)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(failure(
            "A shared document must name an ordinary workspace file",
        ));
    }
    let key = projection_key(id);
    let mut projection = projection_for(store, cabal, &view)?;
    if !store
        .list_documents()
        .map_err(IpcFailure::store)?
        .iter()
        .any(|item| item.relative_path == view.name)
    {
        store
            .create_document_if_absent(
                &view.name,
                DocumentContent::from_visible(
                    DocumentKind::Prose,
                    projection.base.text.as_bytes().to_vec(),
                )
                .map_err(failure)?,
                "cabal document joined",
            )
            .map_err(IpcFailure::store)?;
    }
    if let Some(attempt) = projection.pending.clone() {
        // The exact immutable receipt settles whether a previous file write
        // committed. Never guess from the current file or repeat an edit.
        match save_projection(store, &attempt) {
            Ok(VisibleProjectionState::Applied) => projection.base = attempt.view,
            Ok(VisibleProjectionState::PendingConflict { .. }) => (),
            Ok(VisibleProjectionState::PendingRetry { error, .. }) => return Err(failure(error)),
            Err(error)
                if matches!(
                    error.code,
                    "source_revision_conflict" | "source_blob_conflict" | "external_file_conflict"
                ) => {}
            Err(error) => return Err(error),
        }
        projection.pending = None;
        cabal.set_local_record(&key, &projection).map_err(failure)?;
    }
    store
        .import_external_changes_if_uncontested(&view.name, "external edit to a shared document")
        .map_err(IpcFailure::store)?;
    let mut loaded = store.read_document(&view.name).map_err(IpcFailure::store)?;
    if loaded.text != projection.base.text && loaded.text != view.text {
        let result = cabal
            .edit(&Edit {
                document: id,
                client: Uuid::new_v4(),
                basis: projection.base.heads.clone(),
                text: loaded.text.clone(),
            })
            .map_err(failure)?;
        projection.base = DocumentView {
            heads: result.local_heads,
            text: loaded.text.clone(),
            ..view.clone()
        };
        cabal.set_local_record(&key, &projection).map_err(failure)?;
        view = result.merged;
    }
    if loaded.text != view.text {
        let attempt = ProjectionAttempt {
            view: view.clone(),
            command: CommandId::new(),
            revision: loaded.revision_id,
            blob: loaded.blob_id,
            kind: loaded.kind,
        };
        projection.pending = Some(attempt.clone());
        cabal.set_local_record(&key, &projection).map_err(failure)?;
        if !matches!(
            save_projection(store, &attempt)?,
            VisibleProjectionState::Applied
        ) {
            return Err(IpcFailure::new(
                "cabal_projection_pending",
                "The shared edit is durable; its Markdown file is still settling.",
                true,
            ));
        }
        loaded = store.read_document(&view.name).map_err(IpcFailure::store)?;
    }
    projection.base = view.clone();
    projection.pending = None;
    cabal.set_local_record(&key, &projection).map_err(failure)?;
    let summary = store
        .registered_document(loaded.document_id)
        .map_err(IpcFailure::store)?
        .ok_or_else(|| failure("Shared document registration disappeared"))?;
    Ok(SharedDocument {
        shared: view,
        local: open_document_from(loaded, None, summary.display_title),
    })
}

fn save_projection(
    store: &mut ProjectStore,
    attempt: &ProjectionAttempt,
) -> Result<VisibleProjectionState, IpcFailure> {
    let outcome = store
        .save_document_if_source_idempotent(
            attempt.command,
            &attempt.view.name,
            DocumentContent::from_visible(attempt.kind, attempt.view.text.as_bytes().to_vec())
                .map_err(failure)?,
            "cabal human edits",
            attempt.revision,
            attempt.blob,
        )
        .map_err(IpcFailure::store)?;
    Ok(outcome.visible_projection)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(directory: &Path) -> (ProjectStore, Cabal, DocumentView) {
        let (mut store, _) =
            ProjectStore::initialize(directory.join("writing"), "Writing").expect("project");
        let mut cabal = Cabal::create(
            &directory.join("cabal.db"),
            Identity::generate().expect("key"),
            "Friends",
            "Alice",
        )
        .expect("cabal");
        let view = cabal
            .create_document("Garden.md", "First\n\nLast\n")
            .expect("document");
        project_document(&mut store, &mut cabal, view.id).expect("first projection");
        (store, cabal, view)
    }

    #[test]
    fn crdt_commit_before_file_write_recovers_without_duplicate_changes() {
        let directory = tempfile::tempdir().expect("directory");
        let (store, mut cabal, view) = fixture(directory.path());
        let original = store.read_document(&view.name).expect("original");
        cabal
            .edit(&Edit {
                document: view.id,
                client: Uuid::new_v4(),
                basis: view.heads,
                text: "First, with friends\n\nLast\n".into(),
            })
            .expect("durable change");
        let hashes = cabal.hashes().expect("hashes");
        let identity = cabal.identity().clone();
        drop(cabal);
        drop(store);
        let mut store =
            ProjectStore::open(directory.path().join("writing")).expect("reopen project");
        let mut cabal =
            Cabal::open(&directory.path().join("cabal.db"), identity).expect("reopen cabal");
        let projected =
            project_document(&mut store, &mut cabal, view.id).expect("recover projection");
        assert_eq!(projected.local.text, "First, with friends\n\nLast\n");
        assert_eq!(cabal.hashes().expect("hashes"), hashes);
        assert_eq!(
            store
                .reconstruct_revision(original.revision_id)
                .expect("immutable source"),
            original.text.as_bytes()
        );
    }

    #[test]
    fn committed_file_write_is_settled_by_its_exact_receipt() {
        let directory = tempfile::tempdir().expect("directory");
        let (mut store, mut cabal, original) = fixture(directory.path());
        let source = store.read_document(&original.name).expect("source");
        let edited = cabal
            .edit(&Edit {
                document: original.id,
                client: Uuid::new_v4(),
                basis: original.heads.clone(),
                text: "A committed shared edit".into(),
            })
            .expect("edit");
        let attempt = ProjectionAttempt {
            view: edited.merged,
            command: CommandId::new(),
            revision: source.revision_id,
            blob: source.blob_id,
            kind: source.kind,
        };
        cabal
            .set_local_record(
                &projection_key(original.id),
                &Projection {
                    base: original.clone(),
                    pending: Some(attempt.clone()),
                },
            )
            .expect("intent");
        assert_eq!(
            save_projection(&mut store, &attempt).expect("save"),
            VisibleProjectionState::Applied
        );
        let committed = store.read_document(&original.name).expect("saved");
        let projected =
            project_document(&mut store, &mut cabal, original.id).expect("settle receipt");
        assert_eq!(
            projected.local.summary.revision_id,
            Some(committed.revision_id.to_string())
        );
        assert_eq!(cabal.hashes().expect("hashes").len(), 2);
    }

    #[test]
    fn an_external_markdown_edit_merges_with_unprojected_remote_text() {
        let directory = tempfile::tempdir().expect("directory");
        let (mut store, mut cabal, view) = fixture(directory.path());
        cabal
            .edit(&Edit {
                document: view.id,
                client: Uuid::new_v4(),
                basis: view.heads,
                text: "First together\n\nLast\n".into(),
            })
            .expect("remote side");
        std::fs::write(
            store.root().join(&view.name),
            "First\n\nLast outside Loom\n",
        )
        .expect("external edit");
        let projected = project_document(&mut store, &mut cabal, view.id).expect("merge");
        assert_eq!(
            projected.local.text,
            "First together\n\nLast outside Loom\n"
        );
        let hashes = cabal.hashes().expect("hashes");
        project_document(&mut store, &mut cabal, view.id).expect("unchanged projection");
        assert_eq!(cabal.hashes().expect("hashes"), hashes);
    }
}
