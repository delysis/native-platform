//! Shared workspaces have one CRDT owner and ordinary Markdown projections.
//! Projection attempts use the existing immutable save receipts to settle
//! crashes between the CRDT commit and the visible file commit.
use super::*;
use fs2::FileExt;
use loom_cabal::compute::{
    ComputeExecutor, ComputeGrant, ComputeGrantStatus, ComputeHost, ComputeModel,
};
use loom_cabal::{
    Cabal, Create, DocumentView, Edit, EditResult, Identity, Invitation, MetadataEdit, Network,
    NetworkMode, PeerStatus, Roster, TextKind,
};
use std::io::Write;
use std::sync::OnceLock;
use uuid::Uuid;

#[path = "cabal_compute_requests.rs"]
pub(crate) mod requesting;
pub(crate) use requesting::{
    compute_job_cancel, compute_job_check, compute_job_get, compute_job_prepare,
    compute_job_submit, compute_jobs, compute_peer_offers,
};

type Shared = Arc<Mutex<Cabal>>;

#[derive(Debug, Default)]
pub(crate) struct CabalService {
    profile: tokio::sync::Mutex<Option<Profile>>,
    closed: AtomicBool,
    compute_executor: OnceLock<Arc<dyn ComputeExecutor>>,
}

#[derive(Debug)]
struct Profile {
    directory: PathBuf,
    identity: Identity,
    network: Arc<Network>,
    cabals: BTreeMap<Uuid, Shared>,
    bindings: BTreeMap<PathBuf, Uuid>,
    compute: Option<Arc<ComputeHost>>,
    compute_problem: Option<String>,
    compute_client: Option<Arc<Mutex<loom_cabal::compute::ComputeClient>>>,
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
    local_id: Option<DocumentId>,
    path: String,
    base: DocumentView,
    creation: CommandId,
    relocation: Option<String>,
    pending: Option<ProjectionAttempt>,
    metadata: Option<MetadataEdit>,
    removal: Option<RemovalAttempt>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProjectionAttempt {
    view: DocumentView,
    path: String,
    command: CommandId,
    revision: RevisionId,
    blob: BlobId,
    kind: DocumentKind,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RemovalAttempt {
    command: CommandId,
    document: DocumentId,
    revision: RevisionId,
    blob: BlobId,
}

#[derive(Debug, Serialize)]
struct ProjectionProblem {
    document_id: Option<String>,
    name: String,
    message: String,
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
    deleted_document_ids: Vec<String>,
    problems: Vec<ProjectionProblem>,
    read_only: bool,
    orphaned_changes: usize,
    removed_documents: usize,
}

#[derive(Debug, Serialize)]
struct SharedDocument {
    shared: DocumentView,
    local: OpenDocument,
}

#[derive(Debug, Serialize)]
pub(crate) struct RecoveryCopies {
    paths: Vec<String>,
    draft_path: Option<String>,
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
    pub(super) fn set_compute_executor(&self, executor: Arc<dyn ComputeExecutor>) {
        self.compute_executor
            .set(executor)
            .expect("one native compute executor per cabal service");
    }

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
        let (compute, compute_problem) = if directory.join("compute/compute.db").is_file() {
            match self.compute_executor.get() {
                Some(executor) => {
                    match network.host_compute(&directory.join("compute"), executor.clone()) {
                        Ok(host) => (Some(host), None),
                        Err(error) => (None, Some(error.to_string())),
                    }
                }
                None => (
                    None,
                    Some("The native compute executor is unavailable.".into()),
                ),
            }
        } else {
            (None, None)
        };
        *slot = Some(Profile {
            directory: directory.into(),
            identity,
            network,
            cabals,
            bindings,
            compute,
            compute_problem,
            compute_client: None,
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

    async fn workspace_root(&self, directory: &Path, id: Uuid) -> Result<PathBuf, IpcFailure> {
        if self.profile.lock().await.is_none() && !directory.join("index.json").is_file() {
            return Err(failure("Join this cabal with an invitation first."));
        }
        self.start(directory).await?;
        let slot = self.profile.lock().await;
        let root = slot
            .as_ref()
            .and_then(|profile| profile.bindings.iter().find(|(_, bound)| **bound == id))
            .map(|(root, _)| root.clone())
            .ok_or_else(|| failure("Join this cabal with an invitation first."))?;
        if !root.is_dir() {
            return Err(failure("This cabal's saved folder is unavailable."));
        }
        Ok(root)
    }

    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        let mut slot = self.profile.lock().await;
        if let Some(profile) = slot.take()
            && let Err(error) = profile.network.shutdown().await
        {
            eprintln!("Loom cabal shutdown preserved an unfinished operation: {error}");
        }
    }

    async fn compute_binding(
        &self,
        directory: &Path,
        root: &Path,
        create: bool,
    ) -> Result<Option<ComputeBinding>, IpcFailure> {
        if self.bound(directory, root).await?.is_none() {
            return Ok(None);
        }
        let mut slot = self.profile.lock().await;
        let profile = slot.as_mut().ok_or_else(|| failure("Cabals are closing"))?;
        let cabal = profile
            .bindings
            .get(root)
            .and_then(|id| profile.cabals.get(id))
            .cloned()
            .ok_or_else(|| failure("This folder is not a cabal"))?;
        if create && profile.compute.is_none() {
            let executor = self
                .compute_executor
                .get()
                .ok_or_else(|| failure("Native compute is unavailable"))?;
            let host = profile
                .network
                .host_compute(&directory.join("compute"), executor.clone())
                .map_err(failure)?;
            profile.compute = Some(host);
            profile.compute_problem = None;
        }
        Ok(Some(ComputeBinding {
            cabal,
            host: profile.compute.clone(),
            problem: profile.compute_problem.clone(),
        }))
    }
}

struct ComputeBinding {
    cabal: Shared,
    host: Option<Arc<ComputeHost>>,
    problem: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ComputeHostSnapshot {
    model: Option<ComputeModel>,
    idle: bool,
    grants: Vec<ComputeGrantStatus>,
    problem: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComputeGrantRequest {
    id: Uuid,
    member_key: String,
    roster_hash: String,
    model_fingerprint: String,
    max_output_tokens: u32,
    max_seconds: u32,
    jobs: u32,
}

#[tauri::command]
pub(crate) async fn compute_host_snapshot(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<Option<ComputeHostSnapshot>, IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    let Some(binding) = state
        .cabals
        .compute_binding(&directory(&state)?, &root, false)
        .await?
    else {
        return Ok(None);
    };
    let cabal_id = binding
        .cabal
        .lock()
        .map_err(|_| failure("Cabal owner stopped"))?
        .id();
    let model = loaded_model_for_state(&state)
        .ok()
        .and_then(|model| crate::peer_compute::model_claim(&model).ok());
    let stopped = binding.host.as_ref().is_some_and(|host| host.is_stopped());
    let grants = match binding.host {
        Some(host) => host
            .grant_statuses()
            .map_err(failure)?
            .into_iter()
            .filter(|item| item.grant.cabal == cabal_id)
            .collect(),
        None => Vec::new(),
    };
    Ok(Some(ComputeHostSnapshot {
        idle: model.is_some()
            && !stopped
            && state.peer_compute.idle()
            && state
                .generations
                .active_local_branch_count()
                .map_err(failure)?
                == 0,
        model,
        grants,
        problem: binding.problem.or_else(|| {
            stopped.then(|| "Compute sharing stopped. Restart Loom to resume it.".into())
        }),
    }))
}

#[tauri::command]
pub(crate) async fn compute_grant(
    project_id: String,
    session_id: String,
    request: ComputeGrantRequest,
    state: State<'_, PluginState>,
) -> Result<ComputeGrant, IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    let model = crate::peer_compute::model_claim(&loaded_model_for_state(&state)?)
        .map_err(|_| failure("Choose a verified text-completion model before sharing compute."))?;
    if model.fingerprint != request.model_fingerprint {
        return Err(failure(
            "The selected model changed. Review the grant again.",
        ));
    }
    let Some(binding) = state
        .cabals
        .compute_binding(&directory(&state)?, &root, true)
        .await?
    else {
        return Err(failure("Share this workspace before granting compute."));
    };
    let grant = {
        let cabal = binding
            .cabal
            .lock()
            .map_err(|_| failure("Cabal owner stopped"))?;
        if cabal.roster().hash().map_err(failure)? != request.roster_hash {
            return Err(failure("Cabal membership changed. Review the grant again."));
        }
        ComputeGrant {
            id: request.id,
            cabal: cabal.id(),
            epoch: cabal.roster().payload.epoch,
            peer: request.member_key.parse().map_err(failure)?,
            model,
            max_output_tokens: request.max_output_tokens,
            max_seconds: request.max_seconds,
            jobs: request.jobs,
        }
    };
    // The cabal mutex is released before the compute owner rechecks membership.
    binding
        .host
        .ok_or_else(|| failure("Compute sharing is unavailable"))?
        .grant(grant.clone())
        .map_err(failure)?;
    Ok(grant)
}

#[tauri::command]
pub(crate) async fn compute_revoke(
    project_id: String,
    session_id: String,
    grant_id: Uuid,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    let Some(binding) = state
        .cabals
        .compute_binding(&directory(&state)?, &root, true)
        .await?
    else {
        return Ok(());
    };
    let Some(host) = binding.host else {
        return Ok(());
    };
    let cabal = binding
        .cabal
        .lock()
        .map_err(|_| failure("Cabal owner stopped"))?
        .id();
    if let Some(grant) = host
        .grants()
        .map_err(failure)?
        .iter()
        .find(|grant| grant.id == grant_id)
        && grant.cabal != cabal
    {
        return Err(failure("This compute grant belongs to another workspace."));
    }
    host.revoke(grant_id).map_err(failure)
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
    protected_document_id: Option<String>,
    state: State<'_, PluginState>,
) -> Result<Option<CabalSnapshot>, IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    let Some((cabal, network)) = state.cabals.bound(&directory(&state)?, &root).await? else {
        return Ok(None);
    };
    let mut session = lock_session(&state)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    let mut cabal = cabal.lock().map_err(|_| failure("Cabal owner stopped"))?;
    let protected = protected_document_id
        .map(|id| id.parse::<DocumentId>())
        .transpose()
        .map_err(|_| failure("Invalid protected document identity"))?;
    let (documents, deleted_document_ids, problems) =
        project_workspace(store, &mut cabal, protected)?;
    Ok(Some(CabalSnapshot {
        id: cabal.id(),
        name: cabal.roster().payload.name.clone(),
        roster: cabal.roster().clone(),
        roster_hash: cabal.roster().hash().map_err(failure)?,
        my_key: cabal.identity().public_key().to_string(),
        peers: network.status().borrow().clone(),
        documents,
        deleted_document_ids,
        problems,
        read_only: !cabal.is_member(cabal.identity().public_key()),
        orphaned_changes: cabal.orphaned_changes().map_err(failure)?,
        removed_documents: cabal
            .views()
            .map_err(failure)?
            .iter()
            .filter(|view| view.deleted)
            .count(),
    }))
}

#[tauri::command]
pub(crate) async fn cabal_workspace(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<Option<loom_signal_protocol::Workspace>, IpcFailure> {
    ensure_application_running(&state, "reading a cabal workspace")?;
    let root = root_for(&state, &project_id, &session_id)?;
    let Some((cabal, _)) = state.cabals.bound(&directory(&state)?, &root).await? else {
        return Ok(None);
    };
    let cabal = cabal.lock().map_err(|_| failure("Cabal owner stopped"))?;
    Ok(Some(loom_signal_protocol::Workspace {
        id: cabal.id(),
        title: cabal.roster().payload.name.clone(),
    }))
}

#[tauri::command]
pub(crate) async fn cabal_open(
    workspace_id: Uuid,
    state: State<'_, PluginState>,
) -> Result<Option<String>, IpcFailure> {
    ensure_application_running(&state, "opening a cabal workspace")?;
    let root = state
        .cabals
        .workspace_root(&directory(&state)?, workspace_id)
        .await?;
    let _admission = lock_application_admission(&state, "opening a cabal workspace")?;
    prepare_project_folder(&state, Some(root))
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
            capture_document(store, &mut cabal, source)?;
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
        project_workspace(&mut store, &mut cabal, None)?;
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
    edit: Option<Edit>,
    state: State<'_, PluginState>,
) -> Result<RecoveryCopies, IpcFailure> {
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
    let draft_path = edit
        .as_ref()
        .map(|edit| recovery_path(edit.document, &edit.text));
    let paths = recover_documents(store, &cabal, edit)?;
    Ok(RecoveryCopies { paths, draft_path })
}

fn recovery_path(id: Uuid, text: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
    format!("Recovery/Cabal-{id}-{digest}.md")
}

fn recover_documents(
    store: &mut ProjectStore,
    cabal: &Cabal,
    edit: Option<Edit>,
) -> Result<Vec<String>, IpcFailure> {
    let mut views = cabal.orphaned_documents().map_err(failure)?;
    views.extend(
        cabal
            .views()
            .map_err(failure)?
            .into_iter()
            .filter(|view| view.deleted),
    );
    if !cabal.is_member(cabal.identity().public_key()) {
        for mut view in cabal.views().map_err(failure)? {
            if let Some(projection) = cabal
                .local_record::<Projection>(&projection_key(view.id))
                .map_err(failure)?
                && let Some(id) = projection.local_id
                && !store.document_is_deleted(id).map_err(IpcFailure::store)?
                && let Some(registered) =
                    store.registered_document(id).map_err(IpcFailure::store)?
            {
                let snapshot = store
                    .reconciliation_snapshot(&registered.relative_path)
                    .map_err(IpcFailure::store)?;
                view.text = snapshot
                    .visible
                    .map_or(snapshot.base_text, |visible| visible.text);
            }
            views.push(view);
        }
    }
    if let Some(edit) = edit {
        if u64::try_from(edit.text.len()).map_err(failure)? > loom_store::MAX_DOCUMENT_BYTES {
            return Err(failure(
                "The recovery copy exceeds the manuscript size limit",
            ));
        }
        let mut view = cabal.view(edit.document).map_err(failure)?;
        view.text = edit.text;
        views.push(view);
    }
    let mut paths = BTreeSet::new();
    for view in views {
        let path = recovery_path(view.id, &view.text);
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
                    DocumentContent::from_visible(visible_kind(view.kind), view.text.into_bytes())
                        .map_err(failure)?,
                    "recover cabal writing",
                )
                .map_err(IpcFailure::store)?;
        }
        paths.insert(path);
    }
    Ok(paths.into_iter().collect())
}

fn projection_key(id: Uuid) -> String {
    format!("projection:{id}")
}

fn shareable(path: &str) -> bool {
    !path.starts_with("Runs/") && !path.starts_with("Recovery/") && !path.starts_with('.')
}

fn shared_kind(kind: DocumentKind) -> Result<TextKind, IpcFailure> {
    match kind {
        DocumentKind::Prose => Ok(TextKind::Prose),
        DocumentKind::Verse => Ok(TextKind::Verse),
        DocumentKind::Hybrid => Err(failure(
            "Share hybrid writing after exporting its text as prose or verse",
        )),
    }
}

fn visible_kind(kind: TextKind) -> DocumentKind {
    match kind {
        TextKind::Prose => DocumentKind::Prose,
        TextKind::Verse => DocumentKind::Verse,
    }
}

impl Projection {
    fn new(base: DocumentView, path: String, local_id: Option<DocumentId>) -> Self {
        Self {
            local_id,
            path,
            base,
            creation: CommandId::new(),
            relocation: None,
            pending: None,
            metadata: None,
            removal: None,
        }
    }
    fn save(&self, cabal: &Cabal) -> Result<(), IpcFailure> {
        cabal
            .set_local_record(&projection_key(self.base.id), self)
            .map_err(failure)
    }
}

fn capture_document(
    store: &ProjectStore,
    cabal: &mut Cabal,
    source: &loom_store::DocumentSummary,
) -> Result<(), IpcFailure> {
    let key = format!("capture:{}", source.document_id);
    let create = if let Some(create) = cabal.local_record::<Create>(&key).map_err(failure)? {
        create
    } else {
        let loaded = store
            .read_document(&source.relative_path)
            .map_err(IpcFailure::store)?;
        let create = Create {
            document: Uuid::new_v4(),
            client: Uuid::new_v4(),
            name: source.relative_path.clone(),
            kind: shared_kind(loaded.kind)?,
            text: loaded.text,
        };
        // Record the exact source and identity before committing a CRDT change.
        // A lost response must not turn one local file into two shared documents.
        cabal.set_local_record(&key, &create).map_err(failure)?;
        create
    };
    let result = cabal.create_document_idempotent(&create).map_err(failure)?;
    if cabal
        .local_record::<Projection>(&projection_key(create.document))
        .map_err(failure)?
        .is_none()
    {
        let base = DocumentView {
            id: create.document,
            name: create.name.clone(),
            kind: create.kind,
            deleted: false,
            text: create.text,
            heads: result.local_heads,
        };
        Projection::new(base, create.name, Some(source.document_id)).save(cabal)?;
    }
    Ok(())
}

fn settle_file(
    store: &mut ProjectStore,
    cabal: &Cabal,
    projection: &mut Projection,
) -> Result<(), IpcFailure> {
    let Some(attempt) = projection.pending.clone() else {
        return Ok(());
    };
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
    projection.save(cabal)
}

fn reconcile_local_metadata(
    store: &mut ProjectStore,
    cabal: &mut Cabal,
    view: &DocumentView,
) -> Result<(), IpcFailure> {
    let Some(mut projection) = cabal
        .local_record::<Projection>(&projection_key(view.id))
        .map_err(failure)?
    else {
        return Ok(());
    };
    settle_file(store, cabal, &mut projection)?;
    if projection.local_id.is_none() {
        projection = projection_for(store, cabal, view, &projection.path)?;
    }
    let id = projection
        .local_id
        .ok_or_else(|| failure("Missing local document identity"))?;
    let registered = store
        .registered_document(id)
        .map_err(IpcFailure::store)?
        .ok_or_else(|| failure("A shared document lost its local registration"))?;
    // A file move can commit before its cabal bookkeeping. Settle that intent
    // before deciding whether a different path was an independent human rename.
    if let Some(target) = projection.relocation.clone()
        && registered.relative_path != projection.path
    {
        projection.path = target;
        projection.relocation = None;
        projection.save(cabal)?;
    }
    let removed = store.document_is_deleted(id).map_err(IpcFailure::store)?;
    if projection.metadata.is_none() {
        let renamed = registered.relative_path != projection.path;
        if !renamed && (!removed || projection.base.deleted || projection.removal.is_some()) {
            return Ok(());
        }
        projection.metadata = Some(MetadataEdit {
            document: view.id,
            client: Uuid::new_v4(),
            basis: projection.base.heads.clone(),
            name: if renamed {
                registered.relative_path.clone()
            } else {
                projection.base.name.clone()
            },
            deleted: removed || projection.base.deleted,
        });
        projection.save(cabal)?;
    }
    let attempt = projection
        .metadata
        .as_ref()
        .ok_or_else(|| failure("Missing document action"))?;
    let result = cabal.edit_metadata(attempt).map_err(failure)?;
    projection.base.name.clone_from(&attempt.name);
    projection.base.deleted = attempt.deleted;
    projection.base.heads = result.local_heads;
    projection.path = registered.relative_path;
    projection.metadata = None;
    projection.save(cabal)
}

fn collision_path(view: &DocumentView, index: usize) -> String {
    let (parent, file) = view.name.rsplit_once('/').unwrap_or(("", &view.name));
    let (stem, extension) = file
        .rsplit_once('.')
        .filter(|(_, extension)| extension.len() <= 32)
        .map_or((file, String::new()), |(stem, ext)| {
            (stem, format!(".{ext}"))
        });
    let tag = view.id.simple().to_string();
    let counter = if index == 0 {
        String::new()
    } else {
        format!("-{index}")
    };
    let suffix = format!(" ~{}{counter}", &tag[..8]);
    let available = 1024_usize
        .saturating_sub(parent.len() + usize::from(!parent.is_empty()))
        .min(255);
    let room = available.saturating_sub(suffix.len() + extension.len());
    if room == 0 {
        return format!("Conflicts/{tag}{counter}.md");
    }
    let mut end = stem.len().min(room);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    let file = format!("{}{suffix}{extension}", &stem[..end]);
    if parent.is_empty() {
        file
    } else {
        format!("{parent}/{file}")
    }
}

fn namespace(views: &[DocumentView]) -> Result<BTreeMap<Uuid, String>, IpcFailure> {
    use loom_store::document_path_reservation_key as key;
    let mut counts = BTreeMap::<String, usize>::new();
    for view in views {
        *counts.entry(key(&view.name)).or_default() += 1;
    }
    let mut assigned = BTreeSet::new();
    let mut paths = BTreeMap::new();
    for view in views {
        let mut path = view.name.clone();
        if counts[&key(&path)] > 1 {
            let mut selected = None;
            for index in 0..=views.len() {
                let candidate = collision_path(view, index);
                if !counts.contains_key(&key(&candidate)) && !assigned.contains(&key(&candidate)) {
                    selected = Some(candidate);
                    break;
                }
            }
            path = selected
                .ok_or_else(|| failure("Shared filename collision could not be resolved"))?;
        }
        assigned.insert(key(&path));
        paths.insert(view.id, path);
    }
    Ok(paths)
}

type WorkspaceProjection = (Vec<SharedDocument>, Vec<String>, Vec<ProjectionProblem>);

fn project_workspace(
    store: &mut ProjectStore,
    cabal: &mut Cabal,
    protected: Option<DocumentId>,
) -> Result<WorkspaceProjection, IpcFailure> {
    store
        .reconcile_document_lifecycle()
        .map_err(IpcFailure::store)?;
    store.discover_documents().map_err(IpcFailure::store)?;
    let mut problems = Vec::new();
    let mut blocked = BTreeSet::new();
    let mut claimed = BTreeSet::new();
    let member = cabal.is_member(cabal.identity().public_key());
    for view in cabal.views().map_err(failure)? {
        let projection = cabal
            .local_record::<Projection>(&projection_key(view.id))
            .map_err(failure)?;
        let local = projection.and_then(|record| record.local_id);
        if member && let Err(error) = reconcile_local_metadata(store, cabal, &view) {
            blocked.insert(view.id);
            problems.push(ProjectionProblem {
                document_id: local.map(|id| id.to_string()),
                name: view.name,
                message: error.message,
            });
        }
        claimed.extend(
            cabal
                .local_record::<Projection>(&projection_key(view.id))
                .map_err(failure)?
                .and_then(|record| record.local_id),
        );
    }
    if member {
        for source in store.list_documents().map_err(IpcFailure::store)? {
            if shareable(&source.relative_path)
                && !claimed.contains(&source.document_id)
                && let Err(error) = capture_document(store, cabal, &source)
            {
                problems.push(ProjectionProblem {
                    document_id: Some(source.document_id.to_string()),
                    name: source.relative_path,
                    message: error.message,
                });
            }
        }
    }
    let views = cabal.views().map_err(failure)?;
    let paths = namespace(&views)?;
    let mut documents = Vec::new();
    let mut deleted = Vec::new();
    for view in views {
        if blocked.contains(&view.id) {
            continue;
        }
        let projection = cabal
            .local_record::<Projection>(&projection_key(view.id))
            .map_err(failure)?;
        let local = projection.and_then(|record| record.local_id);
        match project_document_at(
            store,
            cabal,
            view.id,
            &paths[&view.id],
            protected.is_some() && protected == local,
        ) {
            Ok(Some(document)) => documents.push(document),
            Ok(None) => {
                if let Some(record) = cabal
                    .local_record::<Projection>(&projection_key(view.id))
                    .map_err(failure)?
                    && let Some(id) = record.local_id
                {
                    deleted.push(id.to_string());
                }
            }
            Err(error) => problems.push(ProjectionProblem {
                document_id: local.map(|id| id.to_string()),
                name: view.name,
                message: error.message,
            }),
        }
    }
    Ok((documents, deleted, problems))
}

fn projection_for(
    store: &mut ProjectStore,
    cabal: &Cabal,
    view: &DocumentView,
    path: &str,
) -> Result<Projection, IpcFailure> {
    let mut projection = if let Some(record) = cabal
        .local_record::<Projection>(&projection_key(view.id))
        .map_err(failure)?
    {
        record
    } else {
        if store
            .document_path_is_reserved(path)
            .map_err(IpcFailure::store)?
        {
            return Err(failure(format!(
                "{path} already exists outside this cabal; its contents were preserved"
            )));
        }
        let projection = Projection::new(view.clone(), path.into(), None);
        projection.save(cabal)?;
        projection
    };
    if projection.local_id.is_none() {
        let outcome = store
            .create_document_idempotent(
                projection.creation,
                &projection.path,
                DocumentContent::from_visible(
                    visible_kind(projection.base.kind),
                    projection.base.text.as_bytes().to_vec(),
                )
                .map_err(failure)?,
                "cabal document joined",
            )
            .map_err(IpcFailure::store)?;
        if !matches!(outcome.visible_projection, VisibleProjectionState::Applied) {
            return Err(failure(
                "The shared document's initial file is still settling",
            ));
        }
        projection.local_id = Some(
            store
                .document_for_revision(outcome.save.revision_id)
                .map_err(IpcFailure::store)?
                .document_id,
        );
        projection.save(cabal)?;
    }
    Ok(projection)
}

fn project_document(
    store: &mut ProjectStore,
    cabal: &mut Cabal,
    id: Uuid,
) -> Result<SharedDocument, IpcFailure> {
    let paths = namespace(&cabal.views().map_err(failure)?)?;
    project_document_at(store, cabal, id, &paths[&id], true)?
        .ok_or_else(|| failure("This shared document was removed"))
}

fn relocate(
    store: &mut ProjectStore,
    cabal: &Cabal,
    projection: &mut Projection,
    target: &str,
) -> Result<(), IpcFailure> {
    projection.relocation = Some(target.into());
    projection.save(cabal)?;
    let mut authority = store
        .open_document_file(&projection.path)
        .map_err(IpcFailure::store)?;
    store
        .rename_document_to_path(&mut authority, target, &title_for_path(target))
        .map_err(IpcFailure::store)?;
    projection.path = target.into();
    projection.relocation = None;
    projection.save(cabal)
}

/// A filename swap has no free endpoint. Move only another owned projection
/// that is itself leaving this path, retaining an ordinary recovery copy and
/// the same durable move intent used for its final destination.
fn release_shared_target(
    store: &mut ProjectStore,
    cabal: &Cabal,
    id: Uuid,
    target: &str,
) -> Result<(), IpcFailure> {
    use loom_store::document_path_reservation_key as key;
    let views = cabal.views().map_err(failure)?;
    let paths = namespace(&views)?;
    for view in views {
        if view.id == id || key(&paths[&view.id]) == key(target) {
            continue;
        }
        let Some(mut other) = cabal
            .local_record::<Projection>(&projection_key(view.id))
            .map_err(failure)?
        else {
            continue;
        };
        let Some(local) = other.local_id else {
            continue;
        };
        let Some(registered) = store
            .registered_document(local)
            .map_err(IpcFailure::store)?
        else {
            continue;
        };
        if key(&registered.relative_path) != key(target)
            || store
                .document_is_deleted(local)
                .map_err(IpcFailure::store)?
        {
            continue;
        }
        settle_file(store, cabal, &mut other)?;
        store
            .import_external_changes_if_uncontested(
                &registered.relative_path,
                "shared filename move",
            )
            .map_err(IpcFailure::store)?;
        let temporary = format!("Recovery/Cabal moves/{}.md", view.id);
        if store
            .document_path_is_reserved(&temporary)
            .map_err(IpcFailure::store)?
        {
            return Err(failure(
                "A shared filename move has a recovery copy that needs attention",
            ));
        }
        relocate(store, cabal, &mut other, &temporary)?;
        break;
    }
    Ok(())
}

// Keep the durable intent, filesystem action, and acknowledgement in order.
#[allow(clippy::too_many_lines)]
fn project_document_at(
    store: &mut ProjectStore,
    cabal: &mut Cabal,
    id: Uuid,
    path: &str,
    protected: bool,
) -> Result<Option<SharedDocument>, IpcFailure> {
    let mut view = cabal.view(id).map_err(failure)?;
    let mut projection = projection_for(store, cabal, &view, path)?;
    settle_file(store, cabal, &mut projection)?;
    let local = projection
        .local_id
        .ok_or_else(|| failure("Shared document registration disappeared"))?;
    if store
        .document_is_deleted(local)
        .map_err(IpcFailure::store)?
    {
        if !view.deleted {
            return Err(failure(
                "Recover the removed shared document as a new document",
            ));
        }
        projection.base = view;
        projection.removal = None;
        projection.save(cabal)?;
        return Ok(None);
    }
    let registered = store
        .registered_document(local)
        .map_err(IpcFailure::store)?
        .ok_or_else(|| failure("Shared document registration disappeared"))?;
    store
        .import_external_changes_if_uncontested(
            &registered.relative_path,
            "external edit to a shared document",
        )
        .map_err(IpcFailure::store)?;
    let mut loaded = store
        .read_document(&registered.relative_path)
        .map_err(IpcFailure::store)?;
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
            ..projection.base
        };
        projection.save(cabal)?;
        view = result.merged;
    }
    if registered.relative_path != path {
        release_shared_target(store, cabal, id, path)?;
        relocate(store, cabal, &mut projection, path)?;
        loaded = store.read_document(path).map_err(IpcFailure::store)?;
    }
    if loaded.text != view.text {
        let attempt = ProjectionAttempt {
            view: view.clone(),
            path: path.into(),
            command: CommandId::new(),
            revision: loaded.revision_id,
            blob: loaded.blob_id,
            kind: loaded.kind,
        };
        projection.pending = Some(attempt.clone());
        projection.save(cabal)?;
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
        loaded = store.read_document(path).map_err(IpcFailure::store)?;
    }
    projection.base = view.clone();
    projection.pending = None;
    projection.save(cabal)?;
    if view.deleted && !protected {
        let attempt = projection.removal.clone().unwrap_or(RemovalAttempt {
            command: CommandId::new(),
            document: local,
            revision: loaded.revision_id,
            blob: loaded.blob_id,
        });
        projection.removal = Some(attempt.clone());
        projection.save(cabal)?;
        store
            .delete_document_file_idempotent(
                attempt.command,
                attempt.document,
                attempt.revision,
                attempt.blob,
            )
            .map_err(IpcFailure::store)?;
        projection.removal = None;
        projection.save(cabal)?;
        return Ok(None);
    }
    let summary = store
        .registered_document(local)
        .map_err(IpcFailure::store)?
        .ok_or_else(|| failure("Shared document registration disappeared"))?;
    Ok(Some(SharedDocument {
        shared: view,
        local: open_document_from(loaded, None, summary.display_title),
    }))
}

fn save_projection(
    store: &mut ProjectStore,
    attempt: &ProjectionAttempt,
) -> Result<VisibleProjectionState, IpcFailure> {
    let outcome = store
        .save_document_if_source_idempotent(
            attempt.command,
            &attempt.path,
            DocumentContent::from_visible(attempt.kind, attempt.view.text.as_bytes().to_vec())
                .map_err(failure)?,
            "cabal human edits",
            attempt.revision,
            attempt.blob,
        )
        .map_err(IpcFailure::store)?;
    Ok(outcome.visible_projection)
}

// ProjectStore uses anchored filesystem capabilities, currently Unix-only.
// The non-Unix fail-closed behavior is covered in loom-store.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn compute_commands_bind_exact_model_membership_and_workspace() {
        let temporary = tempfile::tempdir().expect("fixture");
        let (store, mut cabal, _) = fixture(temporary.path());
        let root = store.root().to_owned();
        let project = store.manifest().project_id.to_string();
        let session_id = CommandId::new().to_string();
        let identity = cabal.identity().clone();
        let peer = Identity::generate().expect("peer");
        let invite = cabal.invite(identity.public_key().into()).expect("invite");
        cabal
            .admit(&invite.token, peer.public_key(), "Friend")
            .expect("admit");
        let model = crate::tests::test_loaded_model(Path::new("metadata-only.gguf"), "Test model");
        let model_claim = crate::peer_compute::model_claim(&model).expect("metadata claim");
        let request = ComputeGrantRequest {
            id: Uuid::new_v4(),
            member_key: peer.public_key().to_string(),
            roster_hash: cabal.roster().hash().expect("membership hash"),
            model_fingerprint: model_claim.fingerprint,
            max_output_tokens: 16,
            max_seconds: 120,
            jobs: 1,
        };
        let id = cabal.id();
        let cabal = Arc::new(Mutex::new(cabal));
        let network = Arc::new(
            Network::start(&identity, NetworkMode::Local)
                .await
                .expect("local endpoint"),
        );
        network.add(cabal.clone()).expect("current membership");
        let state = PluginState::with_app_local_data_root(
            Some(temporary.path().join("app-data")),
            true,
            BuildModelPolicy::default(),
        );
        {
            let mut session = state.session.lock().expect("session");
            session.phase = SessionPhase::Open;
            session.active_session_id = Some(session_id.parse().expect("session ID"));
            session.store = Some(store);
        }
        *state.model.lock().expect("model registry") = ModelRegistry::Loaded(Box::new(model));
        let profile_root = directory(&state).expect("profile location");
        *state.cabals.profile.lock().await = Some(Profile {
            directory: profile_root.clone(),
            identity,
            network,
            cabals: BTreeMap::from([(id, cabal.clone())]),
            bindings: BTreeMap::from([(root, id)]),
            compute: None,
            compute_problem: None,
            compute_client: None,
            _lease: File::create(temporary.path().join("lease")).expect("lease"),
        });
        let app = tauri::test::mock_app();
        assert!(app.manage(state));
        let state = app.state::<PluginState>();
        let snapshot = compute_host_snapshot(project.clone(), session_id.clone(), state.clone())
            .await
            .expect("snapshot")
            .expect("cabal");
        assert!(snapshot.grants.is_empty());
        assert!(
            !profile_root.join("compute").exists(),
            "reading a snapshot cannot configure sharing"
        );
        let mut changed_model = request.clone();
        changed_model.model_fingerprint = "00".repeat(32);
        assert!(
            compute_grant(
                project.clone(),
                session_id.clone(),
                changed_model,
                state.clone()
            )
            .await
            .is_err()
        );
        assert!(!profile_root.join("compute").exists());
        let mut withdrawn = request.clone();
        withdrawn.id = Uuid::new_v4();
        compute_revoke(
            project.clone(),
            session_id.clone(),
            withdrawn.id,
            state.clone(),
        )
        .await
        .expect("withdraw uncertain grant before a host exists");
        assert!(
            compute_grant(
                project.clone(),
                session_id.clone(),
                withdrawn,
                state.clone()
            )
            .await
            .is_err(),
            "late first admission must remain revoked"
        );
        let grant = compute_grant(
            project.clone(),
            session_id.clone(),
            request.clone(),
            state.clone(),
        )
        .await
        .expect("explicit grant");
        assert_eq!(grant.id, request.id);
        assert_eq!(
            compute_grant(
                project.clone(),
                session_id.clone(),
                request.clone(),
                state.clone()
            )
            .await
            .expect("exact retry"),
            grant
        );
        let mut outsider = request.clone();
        outsider.id = Uuid::new_v4();
        outsider.member_key = Identity::generate()
            .expect("outsider")
            .public_key()
            .to_string();
        assert!(
            compute_grant(project.clone(), session_id.clone(), outsider, state.clone())
                .await
                .is_err()
        );
        assert!(
            compute_grant(
                project.clone(),
                CommandId::new().to_string(),
                request.clone(),
                state.clone()
            )
            .await
            .is_err()
        );
        let snapshot = compute_host_snapshot(project.clone(), session_id.clone(), state.clone())
            .await
            .expect("snapshot")
            .expect("cabal");
        assert_eq!(
            snapshot.grants,
            vec![ComputeGrantStatus {
                jobs_remaining: grant.jobs,
                grant,
                current: true
            }]
        );
        cabal
            .lock()
            .expect("cabal")
            .revoke(peer.public_key())
            .expect("revoke membership");
        assert!(
            compute_grant(
                project.clone(),
                session_id.clone(),
                request.clone(),
                state.clone()
            )
            .await
            .is_err(),
            "old UI membership cannot restore access"
        );
        compute_revoke(
            project.clone(),
            session_id.clone(),
            request.id,
            state.clone(),
        )
        .await
        .expect("stop sharing");
        compute_revoke(
            project.clone(),
            session_id.clone(),
            request.id,
            state.clone(),
        )
        .await
        .expect("exact revocation retry");
        assert!(
            compute_host_snapshot(project, session_id, state.clone())
                .await
                .expect("snapshot")
                .expect("cabal")
                .grants
                .is_empty()
        );
        state.cabals.shutdown().await;
        assert!(
            !temporary
                .path()
                .join("app-data/peer-compute-writing")
                .exists(),
            "granting alone never runs a model"
        );
    }

    #[tokio::test]
    async fn public_workspace_ids_open_only_existing_profile_bindings() {
        let directory = tempfile::tempdir().expect("directory");
        let profile_root = directory.path().join("profile");
        let service = CabalService::default();
        assert!(
            service
                .workspace_root(&profile_root, Uuid::new_v4())
                .await
                .is_err()
        );
        assert!(
            !profile_root.exists(),
            "an unknown link cannot initialize a networking profile"
        );
        let (store, cabal, _) = fixture(directory.path());
        let id = cabal.id();
        let root = store.root().to_owned();
        let identity = Identity::generate().expect("identity");
        let network = Arc::new(
            Network::start(&identity, NetworkMode::Local)
                .await
                .expect("network"),
        );
        *service.profile.lock().await = Some(Profile {
            directory: profile_root.clone(),
            identity,
            network,
            cabals: BTreeMap::from([(id, Arc::new(Mutex::new(cabal)))]),
            bindings: BTreeMap::from([(root.clone(), id)]),
            compute: None,
            compute_problem: None,
            compute_client: None,
            _lease: File::create(directory.path().join("lease")).expect("lease"),
        });
        assert_eq!(
            service
                .workspace_root(&profile_root, id)
                .await
                .expect("registered workspace"),
            root
        );
        assert!(
            service
                .workspace_root(&profile_root, Uuid::new_v4())
                .await
                .is_err()
        );
        drop(store);
        let moved = directory.path().join("moved-writing");
        std::fs::rename(&root, &moved).expect("move the folder");
        assert!(service.workspace_root(&profile_root, id).await.is_err());
        assert!(
            !root.exists(),
            "opening a bookmark cannot recreate a missing workspace"
        );
        service.shutdown().await;
    }

    #[test]
    fn revocation_recovery_preserves_file_and_unsent_editor_versions_privately() {
        let directory = tempfile::tempdir().expect("directory");
        let (_, mut alice, _) = fixture(directory.path());
        let verse = alice
            .create_document_with_kind("Rain.md", "Rain\r\n  falls\r\n", TextKind::Verse)
            .expect("verse");
        let bob_key = Identity::generate().expect("Bob");
        let invitation = alice
            .invite(alice.identity().public_key().into())
            .expect("invitation");
        let roster = alice
            .admit(&invitation.token, bob_key.public_key(), "Bob")
            .expect("admit");
        let mut bob =
            Cabal::import(&directory.path().join("bob.db"), bob_key, roster).expect("Bob's cabal");
        bob.apply(alice.missing(&BTreeSet::new()).expect("changes"))
            .expect("initial sync");
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("bob-writing"), "Bob").expect("project");
        project_workspace(&mut store, &mut bob, None).expect("initial files");
        alice
            .revoke(bob.identity().public_key())
            .expect("remove Bob");
        bob.accept_roster(alice.roster().clone())
            .expect("membership notice");
        std::fs::write(
            store.root().join("Rain.md"),
            "Rain\r\n  falls outside Loom\r\n",
        )
        .expect("offline file");
        let edit = Edit {
            document: verse.id,
            client: Uuid::new_v4(),
            basis: verse.heads,
            text: "Rain\r\n  falls in the editor\r\n".into(),
        };
        assert!(
            bob.edit(&edit).is_err(),
            "revocation does not grant shared write authority"
        );
        let before = bob.hashes().expect("hashes");
        let paths =
            recover_documents(&mut store, &bob, Some(edit.clone())).expect("private copies");
        let draft = store
            .read_document(recovery_path(edit.document, &edit.text))
            .expect("editor copy");
        let external = store
            .read_document(recovery_path(
                edit.document,
                "Rain\r\n  falls outside Loom\r\n",
            ))
            .expect("file copy");
        assert_eq!(draft.text, edit.text);
        assert_eq!(draft.kind, DocumentKind::Verse);
        assert_eq!(external.text, "Rain\r\n  falls outside Loom\r\n");
        assert!(paths.iter().all(|path| !shareable(path)));
        assert_eq!(
            recover_documents(&mut store, &bob, Some(edit)).expect("lost recovery reply"),
            paths
        );
        assert_eq!(bob.hashes().expect("no shared changes"), before);
    }

    #[test]
    fn maximal_colliding_names_cannot_block_the_shared_namespace() {
        let directory = tempfile::tempdir().expect("directory");
        let (_, mut cabal, _) = fixture(directory.path());
        let long_extension = format!("a.{}", "b".repeat(253));
        for _ in 0..2 {
            cabal
                .create_document(&long_extension, "words")
                .expect("valid maximum component");
        }
        let long_path = format!("{0}/{0}/{0}/{0}", "a".repeat(255));
        for _ in 0..2 {
            cabal
                .create_document(&long_path, "words")
                .expect("valid maximum path");
        }
        let paths = namespace(&cabal.views().expect("views")).expect("bounded namespace");
        let unique: BTreeSet<_> = paths
            .values()
            .map(|path| loom_store::document_path_reservation_key(path))
            .collect();
        assert_eq!(unique.len(), paths.len());
        for path in paths.values() {
            assert!(path.len() <= 1024);
            assert!(path.split('/').all(|part| part.len() <= 255));
        }
    }

    #[test]
    fn shared_filename_swaps_finish_without_clobbering_either_manuscript() {
        let directory = tempfile::tempdir().expect("directory");
        let (mut store, mut cabal, first) = fixture(directory.path());
        let second = cabal
            .create_document("Other.md", "Another manuscript")
            .expect("second");
        project_workspace(&mut store, &mut cabal, None).expect("project second");
        for (view, name) in [(&first, "Other.md"), (&second, "Garden.md")] {
            cabal
                .edit_metadata(&MetadataEdit {
                    document: view.id,
                    client: Uuid::new_v4(),
                    basis: view.heads.clone(),
                    name: name.into(),
                    deleted: false,
                })
                .expect("shared rename");
        }
        let (documents, _, problems) =
            project_workspace(&mut store, &mut cabal, None).expect("swap");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(documents.len(), 2);
        assert_eq!(
            store.read_document("Other.md").expect("first body").text,
            first.text
        );
        assert_eq!(
            store.read_document("Garden.md").expect("second body").text,
            second.text
        );
        let hashes = cabal.hashes().expect("hashes");
        project_workspace(&mut store, &mut cabal, None).expect("poll");
        assert_eq!(cabal.hashes().expect("hashes"), hashes);
    }

    #[test]
    fn lost_creation_reply_and_later_rename_keep_one_shared_identity() {
        let directory = tempfile::tempdir().expect("directory");
        let (mut store, mut cabal, original) = fixture(directory.path());
        let mut projection = cabal
            .local_record::<Projection>(&projection_key(original.id))
            .expect("record")
            .expect("projection");
        let id = projection.local_id.expect("local identity");
        // The file receipt committed, but the association reply never arrived.
        projection.local_id = None;
        projection.save(&cabal).expect("interrupted association");
        let mut authority = store.open_document_file(&original.name).expect("authority");
        store
            .rename_document_to_path(&mut authority, "Poems/Spring.md", "Spring")
            .expect("rename");
        let (documents, _, problems) =
            project_workspace(&mut store, &mut cabal, Some(id)).expect("recover");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].local.summary.document_id, id.to_string());
        assert_eq!(documents[0].shared.name, "Poems/Spring.md");
        assert_eq!(cabal.views().expect("views").len(), 1);
        assert!(!store.root().join(&original.name).exists());
    }

    #[test]
    fn a_remote_removal_merges_local_words_and_waits_for_the_open_editor() {
        let directory = tempfile::tempdir().expect("directory");
        let (mut store, mut cabal, original) = fixture(directory.path());
        let loaded = store.read_document(&original.name).expect("local");
        cabal
            .edit_metadata(&MetadataEdit {
                document: original.id,
                client: Uuid::new_v4(),
                basis: original.heads,
                name: "Poems/Garden.md".into(),
                deleted: true,
            })
            .expect("remote removal");
        std::fs::write(
            store.root().join(&original.name),
            "First\n\nLast, still writing\n",
        )
        .expect("offline words");
        let (documents, deleted, problems) =
            project_workspace(&mut store, &mut cabal, Some(loaded.document_id)).expect("project");
        assert!(problems.is_empty(), "{problems:?}");
        assert!(deleted.is_empty());
        assert!(documents[0].shared.deleted);
        assert_eq!(documents[0].shared.text, "First\n\nLast, still writing\n");
        assert!(store.root().join("Poems/Garden.md").exists());
        let hashes = cabal.hashes().expect("hashes");
        project_workspace(&mut store, &mut cabal, Some(loaded.document_id)).expect("still open");
        assert_eq!(
            cabal.hashes().expect("hashes"),
            hashes,
            "a protected file is not an undelete"
        );
        let (documents, deleted, problems) =
            project_workspace(&mut store, &mut cabal, None).expect("closed editor");
        assert!(problems.is_empty(), "{problems:?}");
        assert!(documents.is_empty());
        assert_eq!(deleted, vec![loaded.document_id.to_string()]);
        assert!(!store.root().join("Poems/Garden.md").exists());
        assert_eq!(
            cabal
                .view(original.id)
                .expect("retained shared writing")
                .text,
            "First\n\nLast, still writing\n"
        );
        assert_eq!(
            store
                .reconstruct_revision(loaded.revision_id)
                .expect("immutable original"),
            loaded.text.as_bytes()
        );
    }

    #[test]
    fn a_local_rename_and_an_unseen_remote_edit_merge_once() {
        let directory = tempfile::tempdir().expect("directory");
        let (mut store, mut cabal, original) = fixture(directory.path());
        let loaded = store.read_document(&original.name).expect("local");
        let mut authority = store.open_document_file(&original.name).expect("authority");
        store
            .rename_document_to_path(&mut authority, "Poems/Spring.md", "Spring")
            .expect("rename");
        cabal
            .edit(&Edit {
                document: original.id,
                client: Uuid::new_v4(),
                basis: original.heads,
                text: "First together\n\nLast\n".into(),
            })
            .expect("remote text");
        let (documents, _, problems) =
            project_workspace(&mut store, &mut cabal, Some(loaded.document_id)).expect("merge");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(documents[0].shared.name, "Poems/Spring.md");
        assert_eq!(documents[0].local.text, "First together\n\nLast\n");
        assert_eq!(
            documents[0].local.summary.document_id,
            loaded.document_id.to_string()
        );
        let hashes = cabal.hashes().expect("hashes");
        project_workspace(&mut store, &mut cabal, None).expect("poll");
        assert_eq!(cabal.hashes().expect("hashes"), hashes);
    }

    #[test]
    fn colliding_shared_names_have_portable_distinct_files() {
        let directory = tempfile::tempdir().expect("directory");
        let (mut store, mut cabal, _) = fixture(directory.path());
        cabal
            .create_document_with_kind("garden.md", "Different words\n", TextKind::Verse)
            .expect("other author");
        let (documents, _, problems) =
            project_workspace(&mut store, &mut cabal, None).expect("namespace");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(documents.len(), 2);
        assert_ne!(
            loom_store::document_path_reservation_key(&documents[0].local.summary.relative_path),
            loom_store::document_path_reservation_key(&documents[1].local.summary.relative_path)
        );
        assert!(
            documents
                .iter()
                .any(|doc| doc.local.text == "Different words\n"
                    && doc.local.summary.kind == DocumentKind::Verse)
        );
        assert!(
            documents
                .iter()
                .any(|doc| doc.local.text == "First\n\nLast\n")
        );
        let hashes = cabal.hashes().expect("hashes");
        project_workspace(&mut store, &mut cabal, None).expect("poll");
        assert_eq!(
            cabal.hashes().expect("hashes"),
            hashes,
            "projection aliases are not human renames"
        );
    }

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
            path: original.name.clone(),
            command: CommandId::new(),
            revision: source.revision_id,
            blob: source.blob_id,
            kind: source.kind,
        };
        let mut projection = cabal
            .local_record::<Projection>(&projection_key(original.id))
            .expect("record")
            .expect("projection");
        projection.pending = Some(attempt.clone());
        projection.save(&cabal).expect("intent");
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
