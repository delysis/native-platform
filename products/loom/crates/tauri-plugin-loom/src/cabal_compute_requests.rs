//! Requesting-device commands. Save intent before transmission; status recovery
//! never submits work and a lost reply never becomes a fabricated final state.
use super::*;
use loom_cabal::compute::{
    ClientJob, ClientRequest, ComputeClient, ComputeRejection, ComputeReply, ComputeStatus,
};

type Client = Arc<Mutex<ComputeClient>>;

struct Binding {
    cabal: Shared,
    network: Arc<Network>,
    client: Option<Client>,
}

impl CabalService {
    async fn request_binding(
        &self,
        directory: &Path,
        root: &Path,
        create: bool,
    ) -> Result<Binding, IpcFailure> {
        self.bound(directory, root)
            .await?
            .ok_or_else(|| failure("This workspace is not a cabal."))?;
        let mut slot = self.profile.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(failure("Cabals are closing."));
        }
        let profile = slot
            .as_mut()
            .ok_or_else(|| failure("Cabals are closing."))?;
        let cabal = profile
            .bindings
            .get(root)
            .and_then(|id| profile.cabals.get(id))
            .cloned()
            .ok_or_else(|| failure("This folder is not a cabal."))?;
        let client_root = profile.directory.join("compute-requests");
        // A read may reopen an existing ledger, but cannot create an empty one.
        // Treat nonordinary existing paths as errors in ComputeClient::open.
        if profile.compute_client.is_none() && (create || client_root.symlink_metadata().is_ok()) {
            let client = if create {
                ComputeClient::open(&client_root, profile.identity.public_key())
            } else {
                ComputeClient::open_existing(&client_root, profile.identity.public_key())
            }
            .map_err(failure)?;
            profile.compute_client = Some(Arc::new(Mutex::new(client)));
        }
        Ok(Binding {
            cabal,
            network: profile.network.clone(),
            client: profile.compute_client.clone(),
        })
    }
}

impl Binding {
    fn client(&self) -> Result<std::sync::MutexGuard<'_, ComputeClient>, IpcFailure> {
        self.client
            .as_ref()
            .ok_or_else(|| failure("No peer jobs have been prepared."))?
            .lock()
            .map_err(|_| failure("Peer request storage stopped."))
    }

    fn job(&self, id: Uuid) -> Result<ClientJob, IpcFailure> {
        let cabal = self
            .cabal
            .lock()
            .map_err(|_| failure("Cabal owner stopped."))?
            .id();
        let job = self
            .client()?
            .get(id)
            .map_err(failure)?
            .ok_or_else(|| failure("This peer job has not been prepared."))?;
        if job.request.grant.cabal != cabal {
            return Err(failure("This peer job belongs to another workspace."));
        }
        Ok(job)
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct PeerOffers {
    host: String,
    roster_hash: String,
    grants: Vec<ComputeGrant>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum JobDelivery {
    Stored,
    Receipt,
    Rejected { reason: ComputeRejection },
    Unconfirmed { message: String },
}

#[derive(Debug, Serialize)]
pub(crate) struct JobReply {
    job: ClientJob,
    delivery: JobDelivery,
}

#[derive(Debug, Serialize)]
pub(crate) struct JobSummary {
    id: Uuid,
    host: String,
    model: ComputeModel,
    cancel_requested: bool,
    state: &'static str,
    preview: String,
}

#[tauri::command]
pub(crate) async fn compute_peer_offers(
    project_id: String,
    session_id: String,
    host: String,
    state: State<'_, PluginState>,
) -> Result<PeerOffers, IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    let binding = state
        .cabals
        .request_binding(&directory(&state)?, &root, false)
        .await?;
    let key = host.parse().map_err(failure)?;
    let (id, peer, roster_hash, address) = {
        let cabal = binding
            .cabal
            .lock()
            .map_err(|_| failure("Cabal owner stopped."))?;
        let peer = cabal.identity().public_key();
        if key == peer || !cabal.is_member(key) || !cabal.is_member(peer) {
            return Err(failure("Choose a current friend in this cabal."));
        }
        (
            cabal.id(),
            peer,
            cabal.roster().hash().map_err(failure)?,
            cabal.peer_address(key).map_err(failure)?,
        )
    };
    let response = binding
        .network
        .compute_offers(address, id)
        .await
        .map_err(failure)?;
    // A delayed discovery result cannot cross a workspace or membership change.
    root_for(&state, &project_id, &session_id)?;
    let cabal = binding
        .cabal
        .lock()
        .map_err(|_| failure("Cabal owner stopped."))?;
    if cabal.roster().hash().map_err(failure)? != roster_hash {
        return Err(failure(
            "Cabal membership changed. Check the friend's models again.",
        ));
    }
    match response {
        ComputeReply::Offers { grants }
            if grants.iter().all(|grant| {
                grant.cabal == id
                    && grant.peer == peer
                    && grant.epoch == cabal.roster().payload.epoch
            }) =>
        {
            Ok(PeerOffers {
                host: key.to_string(),
                roster_hash,
                grants,
            })
        }
        ComputeReply::Rejected { reason } => Err(failure(format!(
            "The friend could not offer compute: {reason:?}"
        ))),
        _ => Err(failure("The friend returned unrelated compute offers.")),
    }
}

#[tauri::command]
pub(crate) async fn compute_job_prepare(
    project_id: String,
    session_id: String,
    request: ClientRequest,
    roster_hash: String,
    state: State<'_, PluginState>,
) -> Result<ClientJob, IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    // Resolve the shared workspace before creating request storage. The actual
    // prepare is rechecked under the session owner after this asynchronous step.
    let binding = state
        .cabals
        .request_binding(&directory(&state)?, &root, true)
        .await?;
    let _admission = lock_application_admission(&state, "a peer experiment")?;
    let mut session = lock_session(&state)?;
    require_bound_store(&mut session, &project_id, &session_id)?;
    let cabal = binding
        .cabal
        .lock()
        .map_err(|_| failure("Cabal owner stopped."))?;
    if request.grant.cabal != cabal.id() {
        return Err(failure("This model grant belongs to another workspace."));
    }
    let mut client = binding.client()?;
    // Exact retries recover durable intent even after membership changes.
    if client.get(request.id).map_err(failure)?.is_none()
        && (cabal.roster().hash().map_err(failure)? != roster_hash
            || request.grant.epoch != cabal.roster().payload.epoch
            || !cabal.is_member(request.host)
            || !cabal.is_member(cabal.identity().public_key()))
    {
        return Err(failure(
            "Cabal membership changed. Review the peer request again.",
        ));
    }
    client.prepare(request).map_err(failure)
}

#[tauri::command]
pub(crate) async fn compute_job_get(
    project_id: String,
    session_id: String,
    job_id: Uuid,
    state: State<'_, PluginState>,
) -> Result<ClientJob, IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    let binding = state
        .cabals
        .request_binding(&directory(&state)?, &root, false)
        .await?;
    root_for(&state, &project_id, &session_id)?;
    binding.job(job_id)
}

#[tauri::command]
pub(crate) async fn compute_jobs(
    project_id: String,
    session_id: String,
    state: State<'_, PluginState>,
) -> Result<Vec<JobSummary>, IpcFailure> {
    let root = root_for(&state, &project_id, &session_id)?;
    let binding = state
        .cabals
        .request_binding(&directory(&state)?, &root, false)
        .await?;
    root_for(&state, &project_id, &session_id)?;
    if binding.client.is_none() {
        return Ok(Vec::new());
    }
    let cabal = binding
        .cabal
        .lock()
        .map_err(|_| failure("Cabal owner stopped."))?
        .id();
    // The ledger bounds total history; only a small preview crosses the UI IPC.
    Ok(binding
        .client()?
        .jobs(cabal)
        .map_err(failure)?
        .into_iter()
        .map(|job| {
            let (state, preview) = match job.receipt.as_ref().map(|receipt| &receipt.payload.status)
            {
                None => ("unconfirmed", String::new()),
                Some(ComputeStatus::Accepted) => ("accepted", String::new()),
                Some(ComputeStatus::Running) => ("running", String::new()),
                Some(ComputeStatus::Cancelling { .. }) => ("cancelling", String::new()),
                Some(ComputeStatus::Cancelled { .. }) => ("cancelled", String::new()),
                Some(ComputeStatus::Failed { .. }) => ("failed", String::new()),
                Some(ComputeStatus::Interrupted) => ("interrupted", String::new()),
                Some(ComputeStatus::Completed { text }) => {
                    ("completed", text.chars().take(256).collect())
                }
            };
            JobSummary {
                id: job.request.id,
                host: job.request.host.to_string(),
                model: job.request.grant.model,
                cancel_requested: job.cancel_requested,
                state,
                preview,
            }
        })
        .collect())
}

#[derive(Clone, Copy)]
enum Action {
    Submit,
    Check,
    Cancel,
}

async fn exchange(
    project: &str,
    session: &str,
    id: Uuid,
    action: Action,
    state: &PluginState,
) -> Result<JobReply, IpcFailure> {
    let root = root_for(state, project, session)?;
    let binding = state
        .cabals
        .request_binding(&directory(state)?, &root, false)
        .await?;
    let (job, address) = {
        let _admission = lock_application_admission(state, "a peer job")?;
        let mut owner = lock_session(state)?;
        require_bound_store(&mut owner, project, session)?;
        let mut job = binding.job(id)?;
        if matches!(action, Action::Cancel) {
            job = binding.client()?.request_cancel(id).map_err(failure)?;
        }
        let address = binding
            .cabal
            .lock()
            .map_err(|_| failure("Cabal owner stopped."))?
            .peer_address(job.request.host)
            .map_err(failure)?;
        (job, address)
    };
    if job
        .receipt
        .as_ref()
        .is_some_and(|receipt| receipt.payload.status.is_terminal())
    {
        return Ok(JobReply {
            job,
            delivery: JobDelivery::Stored,
        });
    }
    let request = &job.request;
    let response = match action {
        Action::Check => binding.network.compute_status(address, id).await,
        // Persisted cancellation wins over any later explicit submission. A
        // submit already in flight is settled by the host's idempotent cancel.
        Action::Cancel | Action::Submit if job.cancel_requested => {
            binding
                .network
                .compute_cancel(address, id, request.grant.id, request.input.clone())
                .await
        }
        Action::Submit => {
            binding
                .network
                .compute_submit(address, id, request.grant.id, request.input.clone())
                .await
        }
        Action::Cancel => unreachable!("cancellation was persisted before dispatch"),
    };
    let delivery = match response {
        Ok(ComputeReply::Receipt { receipt }) => {
            binding.client()?.record(*receipt).map_err(failure)?;
            JobDelivery::Receipt
        }
        Ok(ComputeReply::Rejected { reason }) => JobDelivery::Rejected { reason },
        Ok(ComputeReply::Offers { .. }) => {
            return Err(failure("The friend returned offers for a job request."));
        }
        Err(error) => JobDelivery::Unconfirmed {
            message: error.to_string(),
        },
    };
    // A concurrent status or cancellation may have advanced this job. Return
    // the ledger's newest verified state, never the earlier request snapshot.
    root_for(state, project, session)?;
    Ok(JobReply {
        job: binding.job(id)?,
        delivery,
    })
}

#[tauri::command]
pub(crate) async fn compute_job_submit(
    project_id: String,
    session_id: String,
    job_id: Uuid,
    state: State<'_, PluginState>,
) -> Result<JobReply, IpcFailure> {
    exchange(&project_id, &session_id, job_id, Action::Submit, &state).await
}

#[tauri::command]
pub(crate) async fn compute_job_check(
    project_id: String,
    session_id: String,
    job_id: Uuid,
    state: State<'_, PluginState>,
) -> Result<JobReply, IpcFailure> {
    exchange(&project_id, &session_id, job_id, Action::Check, &state).await
}

#[tauri::command]
pub(crate) async fn compute_job_cancel(
    project_id: String,
    session_id: String,
    job_id: Uuid,
    state: State<'_, PluginState>,
) -> Result<JobReply, IpcFailure> {
    exchange(&project_id, &session_id, job_id, Action::Cancel, &state).await
}

#[cfg(all(test, unix))]
#[path = "cabal_compute_request_tests.rs"]
mod tests;
