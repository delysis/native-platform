use super::*;

#[cfg(unix)]
use loom_cabal::compute::{
    ComputeCancellation, ComputeExecutor, ComputeFailure, ComputeInput, HostComputeJob,
};

#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct Executor(pub(crate) std::sync::atomic::AtomicUsize);

#[cfg(unix)]
impl ComputeExecutor for Executor {
    fn execute(
        &self,
        job: HostComputeJob,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, ComputeFailure>> + Send>>
    {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(format!("Remote assertion: {}", job.input.prompt)) })
    }
}

#[cfg(unix)]
pub(crate) struct Pair {
    pub(crate) temporary: tempfile::TempDir,
    pub(crate) app: tauri::App<tauri::test::MockRuntime>,
    pub(crate) host_network: Network,
    pub(crate) host: Arc<ComputeHost>,
    pub(crate) host_cabal: Shared,
    pub(crate) local_cabal: Shared,
    pub(crate) executor: Arc<Executor>,
    pub(crate) project: String,
    pub(crate) session: String,
    pub(crate) request: ClientRequest,
    pub(crate) roster_hash: String,
}

#[cfg(unix)]
impl Pair {
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn new() -> Self {
        let temporary = tempfile::tempdir().expect("fixture");
        let host_identity = Identity::generate().expect("host");
        let identity = Identity::generate().expect("requester");
        let mut host_cabal = Cabal::create(
            &temporary.path().join("host.db"),
            host_identity.clone(),
            "Garden",
            "Alice",
        )
        .expect("cabal");
        let invitation = host_cabal
            .invite(host_identity.public_key().into())
            .expect("invitation");
        host_cabal
            .admit(&invitation.token, identity.public_key(), "Bob")
            .expect("friend");
        let local_cabal = Arc::new(Mutex::new(
            Cabal::import(
                &temporary.path().join("caller.db"),
                identity.clone(),
                host_cabal.roster().clone(),
            )
            .expect("local cabal"),
        ));
        let id = host_cabal.id();
        let roster_hash = host_cabal.roster().hash().expect("membership");
        let host_cabal = Arc::new(Mutex::new(host_cabal));
        let host_network = Network::start(&host_identity, NetworkMode::Direct {})
            .await
            .expect("host network");
        host_network
            .add(host_cabal.clone())
            .expect("host membership");
        let network = Arc::new(
            Network::start(&identity, NetworkMode::Direct {})
                .await
                .expect("caller network"),
        );
        network.add(local_cabal.clone()).expect("caller membership");
        local_cabal
            .lock()
            .expect("cabal")
            .remember_peer(&host_network.address())
            .expect("host address");
        host_cabal
            .lock()
            .expect("cabal")
            .remember_peer(&network.address())
            .expect("caller address");
        let executor = Arc::new(Executor::default());
        let host = host_network
            .host_compute(&temporary.path().join("host-compute"), executor.clone())
            .expect("host owner");
        let grant = ComputeGrant {
            id: Uuid::new_v4(),
            cabal: id,
            epoch: 0,
            peer: identity.public_key(),
            model: ComputeModel {
                fingerprint: "ab".repeat(32),
                name: "Fixture model".into(),
            },
            max_output_tokens: 16,
            max_seconds: 10,
            jobs: 8,
        };
        let mut grant = grant;
        grant.epoch = host_cabal.lock().expect("cabal").roster().payload.epoch;
        host.grant(grant.clone()).expect("explicit grant");
        let request = ClientRequest {
            id: Uuid::new_v4(),
            host: host_identity.public_key(),
            grant,
            input: ComputeInput {
                prompt: "A garden 🌱 @literal".into(),
                seed: 3,
                max_output_tokens: 16,
            },
        };
        let state = PluginState::with_app_local_data_root(
            Some(temporary.path().join("app-data")),
            true,
            BuildModelPolicy::default(),
        );
        let (mut store, _) =
            ProjectStore::initialize(temporary.path().join("writing"), "Writing").expect("project");
        store
            .create_document_if_absent(
                "Draft.md",
                DocumentContent::Prose("My untouched writing.".into()),
                "fixture",
            )
            .expect("manuscript");
        let project = store.manifest().project_id.to_string();
        let session = CommandId::new().to_string();
        let root = store.root().to_owned();
        {
            let mut owner = state.session.lock().expect("session");
            owner.phase = SessionPhase::Open;
            owner.active_session_id = Some(session.parse().expect("session ID"));
            owner.store = Some(store);
        }
        *state.cabals.profile.lock().await = Some(Profile {
            directory: directory(&state).expect("profile"),
            identity,
            network,
            cabals: BTreeMap::from([(id, local_cabal.clone())]),
            bindings: BTreeMap::from([(root, id)]),
            compute: None,
            compute_problem: None,
            compute_client: None,
            _lease: File::create(temporary.path().join("lease")).expect("lease"),
        });
        let app = tauri::test::mock_app();
        assert!(app.manage(state));
        Self {
            temporary,
            app,
            host_network,
            host,
            host_cabal,
            local_cabal,
            executor,
            project,
            session,
            request,
            roster_hash,
        }
    }

    async fn prepare(&self, request: ClientRequest) -> Result<ClientJob, IpcFailure> {
        compute_job_prepare(
            self.project.clone(),
            self.session.clone(),
            request,
            self.roster_hash.clone(),
            self.app.state(),
        )
        .await
    }

    async fn check(&self) -> JobReply {
        compute_job_check(
            self.project.clone(),
            self.session.clone(),
            self.request.id,
            self.app.state(),
        )
        .await
        .expect("check")
    }

    pub(crate) async fn reopen_requester(&self) {
        self.app
            .state::<PluginState>()
            .cabals
            .profile
            .lock()
            .await
            .as_mut()
            .unwrap()
            .compute_client = None;
    }

    pub(crate) async fn close(self) {
        self.app.state::<PluginState>().cabals.shutdown().await;
        self.host_network.shutdown().await.expect("host shutdown");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn peer_jobs_cannot_be_read_or_dispatched_from_another_workspace() {
    let pair = Pair::new().await;
    pair.prepare(pair.request.clone()).await.expect("prepare");
    let state = pair.app.state::<PluginState>();
    let (other_store, _) =
        ProjectStore::initialize(pair.temporary.path().join("other-writing"), "Other")
            .expect("other project");
    let other_project = other_store.manifest().project_id.to_string();
    let other_session = CommandId::new().to_string();
    let other_root = other_store.root().to_owned();
    let other_cabal = Cabal::create(
        &pair.temporary.path().join("other-cabal.db"),
        Identity::generate().expect("identity"),
        "Other",
        "Bob",
    )
    .expect("other cabal");
    let other_id = other_cabal.id();
    {
        let mut slot = state.cabals.profile.lock().await;
        let profile = slot.as_mut().expect("profile");
        profile
            .cabals
            .insert(other_id, Arc::new(Mutex::new(other_cabal)));
        profile.bindings.insert(other_root, other_id);
    }
    {
        let mut session = state.session.lock().expect("session");
        session.store = Some(other_store);
        session.active_session_id = Some(other_session.parse().expect("session ID"));
    }
    assert!(
        compute_jobs(other_project.clone(), other_session.clone(), state.clone())
            .await
            .expect("scoped history")
            .is_empty()
    );
    assert!(
        compute_job_get(
            other_project.clone(),
            other_session.clone(),
            pair.request.id,
            state.clone()
        )
        .await
        .is_err()
    );
    assert!(
        compute_job_submit(
            other_project.clone(),
            other_session.clone(),
            pair.request.id,
            state.clone()
        )
        .await
        .is_err()
    );
    assert!(
        compute_job_cancel(other_project, other_session, pair.request.id, state)
            .await
            .is_err()
    );
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 0);
    {
        let slot = pair.app.state::<PluginState>();
        let owner = slot.cabals.profile.lock().await;
        let client = owner
            .as_ref()
            .expect("profile")
            .compute_client
            .as_ref()
            .expect("ledger")
            .lock()
            .expect("ledger owner");
        assert!(
            !client
                .get(pair.request.id)
                .expect("saved request")
                .expect("job")
                .cancel_requested
        );
    }
    pair.close().await;
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn native_request_commands_recover_exact_results_without_touching_the_manuscript() {
    let pair = Pair::new().await;
    let offers = compute_peer_offers(
        pair.project.clone(),
        pair.session.clone(),
        pair.request.host.to_string(),
        pair.app.state(),
    )
    .await
    .expect("offers");
    assert_eq!(offers.grants, vec![pair.request.grant.clone()]);
    assert_eq!(offers.roster_hash, pair.roster_hash);
    assert!(
        compute_jobs(pair.project.clone(), pair.session.clone(), pair.app.state())
            .await
            .expect("empty history")
            .is_empty()
    );
    let client_root = directory(&pair.app.state::<PluginState>())
        .expect("profile")
        .join("compute-requests");
    assert!(
        !client_root.exists(),
        "discovery and history cannot create request storage"
    );
    assert!(
        compute_job_check(
            pair.project.clone(),
            pair.session.clone(),
            pair.request.id,
            pair.app.state()
        )
        .await
        .is_err()
    );
    let prepared = pair.prepare(pair.request.clone()).await.expect("prepare");
    assert!(prepared.receipt.is_none());
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 0);
    let checked = pair.check().await;
    assert!(matches!(checked.delivery, JobDelivery::Rejected { .. }));
    assert!(
        checked.job.receipt.is_none(),
        "a rejected status is still an unconfirmed job"
    );
    assert_eq!(
        pair.executor.0.load(Ordering::SeqCst),
        0,
        "checking cannot submit"
    );
    let mut substitution = pair.request.clone();
    substitution.input.prompt.push_str("changed");
    assert!(pair.prepare(substitution).await.is_err());
    assert!(
        compute_job_submit(
            pair.project.clone(),
            CommandId::new().to_string(),
            pair.request.id,
            pair.app.state()
        )
        .await
        .is_err()
    );
    // Discard the admission reply and release the app's ledger owner, exactly
    // as a lost UI response followed by a fresh ledger owner would do.
    let _ = compute_job_submit(
        pair.project.clone(),
        pair.session.clone(),
        pair.request.id,
        pair.app.state(),
    )
    .await
    .expect("submit");
    pair.app
        .state::<PluginState>()
        .cabals
        .profile
        .lock()
        .await
        .as_mut()
        .expect("profile")
        .compute_client
        .take();
    let completed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let reply = pair.check().await;
            if reply
                .job
                .receipt
                .as_ref()
                .is_some_and(|r| r.payload.status.is_terminal())
            {
                break reply;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("completion");
    assert_eq!(
        completed
            .job
            .receipt
            .as_ref()
            .expect("receipt")
            .payload
            .status,
        ComputeStatus::Completed {
            text: "Remote assertion: A garden 🌱 @literal".into()
        }
    );
    let replay = compute_job_submit(
        pair.project.clone(),
        pair.session.clone(),
        pair.request.id,
        pair.app.state(),
    )
    .await
    .expect("retry");
    assert!(matches!(replay.delivery, JobDelivery::Stored));
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(pair.temporary.path().join("writing/Draft.md")).expect("writing"),
        "My untouched writing."
    );
    assert!(
        !pair.temporary.path().join("writing/Runs").exists(),
        "remote assertions cannot create local inference artifacts"
    );
    pair.close().await;
}

#[cfg(unix)]
#[tokio::test]
async fn native_cancellation_survives_reopen_and_wins_over_delayed_submission() {
    let pair = Pair::new().await;
    pair.prepare(pair.request.clone()).await.expect("prepare");
    let cancelled = compute_job_cancel(
        pair.project.clone(),
        pair.session.clone(),
        pair.request.id,
        pair.app.state(),
    )
    .await
    .expect("cancel");
    assert!(cancelled.job.cancel_requested);
    assert_eq!(
        cancelled.job.receipt.expect("receipt").payload.status,
        ComputeStatus::Cancelled {
            reason: ComputeCancellation::Requested
        }
    );
    pair.app
        .state::<PluginState>()
        .cabals
        .profile
        .lock()
        .await
        .as_mut()
        .expect("profile")
        .compute_client
        .take();
    let replay = compute_job_submit(
        pair.project.clone(),
        pair.session.clone(),
        pair.request.id,
        pair.app.state(),
    )
    .await
    .expect("delayed submit");
    assert!(matches!(replay.delivery, JobDelivery::Stored));
    assert!(replay.job.cancel_requested);
    assert_eq!(pair.executor.0.load(Ordering::SeqCst), 0);
    pair.host
        .revoke(pair.request.grant.id)
        .expect("revoke grant");
    pair.host_cabal
        .lock()
        .expect("host cabal")
        .revoke(pair.request.grant.peer)
        .expect("remove member");
    let roster = pair.host_cabal.lock().expect("host cabal").roster().clone();
    pair.local_cabal
        .lock()
        .expect("local cabal")
        .accept_roster(roster)
        .expect("new membership");
    assert!(
        pair.prepare(pair.request.clone()).await.is_ok(),
        "exact recovery survives membership changes"
    );
    let mut fresh = pair.request.clone();
    fresh.id = Uuid::new_v4();
    assert!(
        pair.prepare(fresh).await.is_err(),
        "new requests need current membership"
    );
    assert!(pair.check().await.job.receipt.is_some());
    pair.close().await;
}
