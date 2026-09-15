use loom_cabal::{
    Cabal, Identity, Network, NetworkMode, Result,
    compute::{
        ComputeCancellation, ComputeExecutor, ComputeFailure, ComputeGrant, ComputeHost,
        ComputeInput, ComputeModel, ComputeRejection, ComputeReply, ComputeStatus, HostComputeJob,
        MAX_COMPUTE_TEXT_BYTES, RemoteJobReceipt,
    },
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug)]
struct Controlled {
    calls: AtomicUsize,
    panic: AtomicBool,
    started: Notify,
    cancelled: Notify,
    release: Semaphore,
    output: String,
    inputs: Mutex<Vec<ComputeInput>>,
}

impl Controlled {
    fn new(output: String) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            panic: AtomicBool::new(false),
            started: Notify::new(),
            cancelled: Notify::new(),
            release: Semaphore::new(0),
            output,
            inputs: Mutex::new(Vec::new()),
        })
    }
}

#[derive(Debug)]
struct Executor(Arc<Controlled>);

impl ComputeExecutor for Executor {
    fn execute(
        &self,
        job: HostComputeJob,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<String, ComputeFailure>> + Send>> {
        let owner = self.0.clone();
        Box::pin(async move {
            owner.calls.fetch_add(1, Ordering::SeqCst);
            owner
                .inputs
                .lock()
                .expect("record fixture input")
                .push(job.input.clone());
            owner.started.notify_one();
            assert!(
                !owner.panic.load(Ordering::SeqCst),
                "controlled adapter panic"
            );
            assert_eq!(job.input.prompt, "A small garden 🌱");
            tokio::select! {
                permit = owner.release.acquire() => permit.expect("owner release").forget(),
                () = cancel.cancelled() => {
                    owner.cancelled.notify_one();
                    // Deliberately hostile cancellation: the adapter has seen
                    // it but its native worker has not yet joined.
                    owner.release.acquire().await.expect("owner release").forget();
                }
            }
            Ok(owner.output.clone())
        })
    }
}

#[tokio::test]
async fn adapter_panic_is_a_durable_failure_and_the_next_job_has_a_new_identity() -> Result<()> {
    let pair = Pair::new("after the panic", 2, 10).await?;
    pair.host.grant(pair.grant.clone())?;
    pair.executor.panic.store(true, Ordering::SeqCst);
    let failed = Uuid::new_v4();
    receipt(pair.submit(failed).await?);
    assert_eq!(
        pair.wait_terminal(failed).await?.payload.status,
        ComputeStatus::Failed {
            failure: ComputeFailure::WorkerPanicked
        }
    );
    pair.executor.panic.store(false, Ordering::SeqCst);
    pair.executor.release.add_permits(1);
    let next = Uuid::new_v4();
    receipt(pair.submit(next).await?);
    assert_eq!(
        pair.wait_terminal(next).await?.payload.status,
        ComputeStatus::Completed {
            text: "after the panic".into()
        }
    );
    assert_eq!(pair.executor.calls.load(Ordering::SeqCst), 2);
    pair.close().await
}

struct Pair {
    directory: tempfile::TempDir,
    identity: Identity,
    cabal: Arc<Mutex<Cabal>>,
    network: Arc<Network>,
    peer: Network,
    host: Arc<ComputeHost>,
    executor: Arc<Controlled>,
    grant: ComputeGrant,
}

impl Pair {
    async fn new(output: &str, jobs: u32, seconds: u32) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let identity = Identity::generate()?;
        let peer_identity = Identity::generate()?;
        let mut cabal = Cabal::create(
            &directory.path().join("cabal.db"),
            identity.clone(),
            "Garden",
            "Alice",
        )?;
        let invitation = cabal.invite(identity.public_key().into())?;
        cabal.admit(&invitation.token, peer_identity.public_key(), "Bob")?;
        let grant = ComputeGrant {
            id: Uuid::new_v4(),
            cabal: cabal.id(),
            epoch: cabal.roster().payload.epoch,
            peer: peer_identity.public_key(),
            model: ComputeModel {
                media: Vec::new(),
                fingerprint: "ab".repeat(32),
                name: "Host test model".into(),
            },
            max_output_tokens: 128,
            max_seconds: seconds,
            jobs,
        };
        let cabal = Arc::new(Mutex::new(cabal));
        let network = Arc::new(Network::start(&identity, NetworkMode::Direct {}).await?);
        network.add(cabal.clone())?;
        let peer = Network::start(&peer_identity, NetworkMode::Direct {}).await?;
        let executor = Controlled::new(output.into());
        let host = network.host_compute(
            &directory.path().join("compute"),
            Arc::new(Executor(executor.clone())),
        )?;
        Ok(Self {
            directory,
            identity,
            cabal,
            network,
            peer,
            host,
            executor,
            grant,
        })
    }

    async fn submit(&self, job: Uuid) -> Result<ComputeReply> {
        self.peer
            .compute_submit(self.network.address(), job, self.grant.id, input())
            .await
    }

    async fn wait_terminal(&self, job: Uuid) -> Result<RemoteJobReceipt> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let value = receipt(
                    self.peer
                        .compute_status(self.network.address(), job)
                        .await?,
                );
                if value.payload.status.is_terminal() {
                    return Ok(value);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("job settled")
    }

    async fn close(self) -> Result<()> {
        self.network.shutdown().await?;
        self.peer.shutdown().await
    }
}

fn input() -> ComputeInput {
    ComputeInput {
        media: Vec::new(),
        prompt: "A small garden 🌱".into(),
        max_output_tokens: 128,
        seed: 4,
    }
}

#[tokio::test]
async fn media_crosses_quic_with_exact_retries_and_unsupported_modalities_spend_no_grant()
-> Result<()> {
    use loom_cabal::compute::{ComputeMedia, ComputeMediaFormat, ComputeModality};
    let mut pair = Pair::new("opaque media transport fixture", 2, 10).await?;
    pair.host.grant(pair.grant.clone())?;
    let mut media_input = input();
    // Opaque bytes deliberately exercise the wire/storage bound, not decoding.
    // Native image/audio validation is covered by the product adapter tests.
    media_input.media = vec![ComputeMedia::new(
        ComputeMediaFormat::Png,
        &vec![42; 4 * 1024 * 1024],
    )?];
    let id = Uuid::new_v4();
    rejected(
        pair.peer
            .compute_submit(
                pair.network.address(),
                id,
                pair.grant.id,
                media_input.clone(),
            )
            .await?,
        ComputeRejection::InvalidRequest,
    );
    assert_eq!(pair.executor.calls.load(Ordering::SeqCst), 0);
    assert_eq!(pair.host.grant_statuses()?[0].jobs_remaining, 2);
    pair.grant.id = Uuid::new_v4();
    pair.grant.model.media = vec![ComputeModality::Image];
    pair.host.grant(pair.grant.clone())?;
    pair.executor.release.add_permits(1);
    receipt(
        pair.peer
            .compute_submit(
                pair.network.address(),
                id,
                pair.grant.id,
                media_input.clone(),
            )
            .await?,
    );
    let terminal = pair.wait_terminal(id).await?;
    assert_eq!(
        pair.executor
            .inputs
            .lock()
            .expect("received input")
            .as_slice(),
        &[media_input.clone()]
    );
    let retry = receipt(
        pair.peer
            .compute_submit(
                pair.network.address(),
                id,
                pair.grant.id,
                media_input.clone(),
            )
            .await?,
    );
    assert_eq!(retry.hash()?, terminal.hash()?);
    media_input.media[0] = ComputeMedia::new(ComputeMediaFormat::Png, b"different input")?;
    rejected(
        pair.peer
            .compute_submit(pair.network.address(), id, pair.grant.id, media_input)
            .await?,
        ComputeRejection::MismatchedRetry,
    );
    assert_eq!(pair.executor.calls.load(Ordering::SeqCst), 1);
    pair.close().await
}

fn receipt(reply: ComputeReply) -> RemoteJobReceipt {
    match reply {
        ComputeReply::Receipt { receipt } => *receipt,
        other => panic!("expected receipt, got {other:?}"),
    }
}

fn rejected(reply: ComputeReply, reason: ComputeRejection) {
    assert!(
        matches!(reply, ComputeReply::Rejected { reason: actual } if actual == reason),
        "{reply:?}"
    );
}

async fn notified(notify: &Notify) {
    tokio::time::timeout(Duration::from_secs(5), notify.notified())
        .await
        .expect("worker observation");
}

#[tokio::test]
async fn cancelling_before_admission_prevents_a_delayed_submission_from_executing() -> Result<()> {
    let pair = Pair::new("must not run", 1, 10).await?;
    pair.host.grant(pair.grant.clone())?;
    let job = Uuid::new_v4();
    let cancelled = receipt(
        pair.peer
            .compute_cancel(pair.network.address(), job, pair.grant.id, input())
            .await?,
    );
    assert!(matches!(
        cancelled.payload.status,
        ComputeStatus::Cancelled { .. }
    ));
    assert_eq!(receipt(pair.submit(job).await?).hash()?, cancelled.hash()?);
    assert_eq!(pair.executor.calls.load(Ordering::SeqCst), 0);
    assert_eq!(pair.host.grant_statuses()?[0].jobs_remaining, 0);
    pair.close().await
}

#[cfg(unix)]
#[tokio::test]
async fn requesting_device_restart_recovers_lost_replies_and_preserves_cancellation_intent()
-> Result<()> {
    use loom_cabal::compute::{ClientRequest, ComputeClient};
    let pair = Pair::new("discard cancelled output", 1, 10).await?;
    pair.host.grant(pair.grant.clone())?;
    let directory = pair.directory.path().join("caller");
    let mut client = ComputeClient::open(&directory, pair.peer.address().id)?;
    let request = ClientRequest {
        id: Uuid::new_v4(),
        host: pair.network.address().id,
        grant: pair.grant.clone(),
        input: input(),
    };
    client.prepare(request.clone())?;
    let _lost_reply = pair.submit(request.id).await?;
    notified(&pair.executor.started).await;
    drop(client);
    let mut client = ComputeClient::open(&directory, pair.peer.address().id)?;
    assert!(
        client
            .get(request.id)?
            .expect("saved job")
            .receipt
            .is_none()
    );
    let observed = receipt(
        pair.peer
            .compute_status(pair.network.address(), request.id)
            .await?,
    );
    client.record(observed)?;
    client.request_cancel(request.id)?;
    drop(client);
    let mut client = ComputeClient::open(&directory, pair.peer.address().id)?;
    let saved = client.get(request.id)?.expect("cancel intent");
    assert!(saved.cancel_requested);
    let cancelling = receipt(
        pair.peer
            .compute_cancel(
                pair.network.address(),
                saved.request.id,
                saved.request.grant.id,
                saved.request.input,
            )
            .await?,
    );
    client.record(cancelling)?;
    notified(&pair.executor.cancelled).await;
    pair.executor.release.add_permits(1);
    let terminal = pair.wait_terminal(request.id).await?;
    client.record(terminal.clone())?;
    let retried = receipt(pair.submit(request.id).await?);
    assert_eq!(retried.hash()?, terminal.hash()?);
    assert_eq!(
        client.record(retried)?.receipt.expect("terminal").hash()?,
        terminal.hash()?
    );
    assert_eq!(
        pair.executor.calls.load(Ordering::SeqCst),
        1,
        "only the original reviewed submission executes"
    );
    assert_eq!(client.jobs(pair.grant.cabal)?.len(), 1);
    pair.close().await
}

#[tokio::test]
async fn explicit_grant_and_authenticated_job_retries_run_once_over_quic() -> Result<()> {
    let pair = Pair::new("A host assertion", 1, 10).await?;
    let job = Uuid::new_v4();
    rejected(pair.submit(job).await?, ComputeRejection::Denied);
    let mut stale_grant = pair.grant.clone();
    stale_grant.epoch += 1;
    assert!(pair.host.grant(stale_grant).is_err());
    let mut outsider_grant = pair.grant.clone();
    outsider_grant.peer = Identity::generate()?.public_key();
    assert!(pair.host.grant(outsider_grant).is_err());
    pair.host.grant(pair.grant.clone())?;
    assert_eq!(pair.host.grant_statuses()?[0].jobs_remaining, 1);
    let offers = pair
        .peer
        .compute_offers(pair.network.address(), pair.grant.cabal)
        .await?;
    assert!(
        matches!(offers, ComputeReply::Offers { grants } if grants == vec![pair.grant.clone()])
    );
    let accepted = receipt(pair.submit(job).await?);
    accepted.verify()?;
    assert_eq!(accepted.signer, pair.identity.public_key());
    assert_eq!(
        accepted.payload.request_fingerprint,
        input().fingerprint(pair.grant.id)?
    );
    notified(&pair.executor.started).await;
    let retried = receipt(pair.submit(job).await?);
    assert_eq!(retried.payload.job, job);
    let status = pair.host.grant_statuses()?.remove(0);
    assert_eq!(
        status.jobs_remaining, 0,
        "an exact retry spends no extra job"
    );
    assert!(status.current);
    assert!(
        matches!(pair.peer.compute_offers(pair.network.address(), pair.grant.cabal).await?,
        ComputeReply::Offers { grants } if grants.is_empty()),
        "exhausted grants cannot advertise new work"
    );
    let mut changed = input();
    changed.seed += 1;
    rejected(
        pair.peer
            .compute_submit(pair.network.address(), job, pair.grant.id, changed)
            .await?,
        ComputeRejection::MismatchedRetry,
    );
    rejected(pair.submit(Uuid::new_v4()).await?, ComputeRejection::Busy);
    let stranger = Network::start(&Identity::generate()?, NetworkMode::Direct {}).await?;
    rejected(
        stranger.compute_status(pair.network.address(), job).await?,
        ComputeRejection::Denied,
    );
    rejected(
        stranger
            .compute_cancel(pair.network.address(), job, pair.grant.id, input())
            .await?,
        ComputeRejection::Denied,
    );
    rejected(
        stranger
            .compute_submit(
                pair.network.address(),
                Uuid::new_v4(),
                pair.grant.id,
                input(),
            )
            .await?,
        ComputeRejection::Denied,
    );
    pair.executor.release.add_permits(1);
    let completed = pair.wait_terminal(job).await?;
    assert_eq!(
        completed.payload.status,
        ComputeStatus::Completed {
            text: "A host assertion".into()
        }
    );
    assert_eq!(completed.payload.revision, 2);
    assert_eq!(receipt(pair.submit(job).await?).hash()?, completed.hash()?);
    rejected(
        pair.submit(Uuid::new_v4()).await?,
        ComputeRejection::Exhausted,
    );
    assert_eq!(pair.executor.calls.load(Ordering::SeqCst), 1);
    stranger.shutdown().await?;
    pair.close().await
}

#[tokio::test]
async fn network_shutdown_cancels_work_and_every_shutdown_observer_waits_for_joining() -> Result<()>
{
    let pair = Pair::new("discard after shutdown", 1, 120).await?;
    pair.host.grant(pair.grant.clone())?;
    let job = Uuid::new_v4();
    receipt(pair.submit(job).await?);
    notified(&pair.executor.started).await;
    let first = pair.network.clone();
    let first = tokio::spawn(async move { first.shutdown().await });
    let second = pair.network.clone();
    let second = tokio::spawn(async move { second.shutdown().await });
    notified(&pair.executor.cancelled).await;
    assert!(!first.is_finished() && !second.is_finished());
    assert_eq!(
        receipt(
            pair.peer
                .compute_status(pair.network.address(), job)
                .await?
        )
        .payload
        .status,
        ComputeStatus::Cancelling {
            reason: ComputeCancellation::HostStopping
        }
    );
    pair.executor.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .expect("first owner joined")
        .expect("shutdown task")?;
    tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("second owner joined")
        .expect("shutdown task")?;
    assert!(
        pair.network
            .host_compute(
                &pair.directory.path().join("too-late"),
                Arc::new(Executor(pair.executor.clone()))
            )
            .is_err()
    );
    pair.peer.shutdown().await
}

#[tokio::test]
async fn cancellation_does_not_release_capacity_or_claim_join_before_worker_returns() -> Result<()>
{
    let pair = Pair::new("discard after cancellation", 2, 10).await?;
    pair.host.grant(pair.grant.clone())?;
    let job = Uuid::new_v4();
    receipt(pair.submit(job).await?);
    notified(&pair.executor.started).await;
    let cancelling = receipt(
        pair.peer
            .compute_cancel(pair.network.address(), job, pair.grant.id, input())
            .await?,
    );
    assert_eq!(
        cancelling.payload.status,
        ComputeStatus::Cancelling {
            reason: ComputeCancellation::Requested
        }
    );
    notified(&pair.executor.cancelled).await;
    rejected(pair.submit(Uuid::new_v4()).await?, ComputeRejection::Busy);
    assert_eq!(
        receipt(
            pair.peer
                .compute_status(pair.network.address(), job)
                .await?
        )
        .payload
        .status,
        cancelling.payload.status
    );
    pair.executor.release.add_permits(1);
    let terminal = pair.wait_terminal(job).await?;
    assert_eq!(
        terminal.payload.status,
        ComputeStatus::Cancelled {
            reason: ComputeCancellation::Requested
        }
    );
    assert_eq!(terminal.payload.revision, 3);
    assert_eq!(receipt(pair.submit(job).await?).hash()?, terminal.hash()?);
    assert_eq!(pair.executor.calls.load(Ordering::SeqCst), 1);
    pair.close().await
}

#[tokio::test]
async fn revoking_grant_and_membership_cancels_owned_execution() -> Result<()> {
    for membership in [false, true] {
        let pair = Pair::new("discard revoked output", 2, 10).await?;
        pair.host.grant(pair.grant.clone())?;
        let job = Uuid::new_v4();
        receipt(pair.submit(job).await?);
        notified(&pair.executor.started).await;
        if membership {
            pair.cabal
                .lock()
                .expect("cabal owner")
                .revoke(pair.grant.peer)?;
        } else {
            pair.host.revoke(pair.grant.id)?;
            assert!(
                pair.host.grant(pair.grant.clone()).is_err(),
                "revoked grant cannot be restored by retry"
            );
        }
        notified(&pair.executor.cancelled).await;
        let current = receipt(
            pair.peer
                .compute_status(pair.network.address(), job)
                .await?,
        );
        assert_eq!(
            current.payload.status,
            ComputeStatus::Cancelling {
                reason: ComputeCancellation::GrantRevoked
            }
        );
        pair.executor.release.add_permits(1);
        assert_eq!(
            pair.wait_terminal(job).await?.payload.status,
            ComputeStatus::Cancelled {
                reason: ComputeCancellation::GrantRevoked
            }
        );
        rejected(pair.submit(Uuid::new_v4()).await?, ComputeRejection::Denied);
        pair.close().await?;
    }
    Ok(())
}

#[tokio::test]
async fn deadline_and_network_shutdown_wait_for_actual_worker_settlement() -> Result<()> {
    let pair = Pair::new("too late", 2, 1).await?;
    pair.host.grant(pair.grant.clone())?;
    let job = Uuid::new_v4();
    receipt(pair.submit(job).await?);
    notified(&pair.executor.started).await;
    notified(&pair.executor.cancelled).await;
    assert_eq!(
        receipt(
            pair.peer
                .compute_status(pair.network.address(), job)
                .await?
        )
        .payload
        .status,
        ComputeStatus::Cancelling {
            reason: ComputeCancellation::TimeLimit
        }
    );
    let network = pair.network.clone();
    let shutdown = tokio::spawn(async move { network.shutdown().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !shutdown.is_finished(),
        "shutdown cannot detach a native worker"
    );
    pair.executor.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .expect("joined shutdown")
        .expect("shutdown task")?;
    assert_eq!(pair.executor.calls.load(Ordering::SeqCst), 1);
    pair.peer.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn completed_job_survives_host_restart_and_same_device_reconnect() -> Result<()> {
    let pair = Pair::new("durable answer", 1, 10).await?;
    pair.host.grant(pair.grant.clone())?;
    let job = Uuid::new_v4();
    receipt(pair.submit(job).await?);
    notified(&pair.executor.started).await;
    pair.executor.release.add_permits(1);
    let completed = pair.wait_terminal(job).await?;
    pair.network.shutdown().await?;
    let Pair {
        directory,
        identity,
        cabal,
        network,
        peer,
        host,
        executor,
        grant,
    } = pair;
    drop(network);
    drop(host);
    let restarted = Network::start(&identity, NetworkMode::Direct {}).await?;
    restarted.add(cabal)?;
    let host = restarted.host_compute(
        &directory.path().join("compute"),
        Arc::new(Executor(executor.clone())),
    )?;
    assert_eq!(host.grants()?, vec![grant.clone()]);
    let retried = receipt(
        peer.compute_submit(restarted.address(), job, grant.id, input())
            .await?,
    );
    assert_eq!(completed.hash()?, retried.hash()?);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    restarted.shutdown().await?;
    peer.shutdown().await
}

#[tokio::test]
async fn malformed_and_over_budget_inputs_do_not_execute_and_oversized_output_fails() -> Result<()>
{
    let pair = Pair::new(&"x".repeat(MAX_COMPUTE_TEXT_BYTES + 1), 1, 10).await?;
    pair.host.grant(pair.grant.clone())?;
    let mut over_budget = input();
    over_budget.max_output_tokens = 129;
    rejected(
        pair.peer
            .compute_submit(
                pair.network.address(),
                Uuid::new_v4(),
                pair.grant.id,
                over_budget,
            )
            .await?,
        ComputeRejection::InvalidRequest,
    );
    let mut large = input();
    large.prompt = "x".repeat(MAX_COMPUTE_TEXT_BYTES + 1);
    rejected(
        pair.peer
            .compute_submit(pair.network.address(), Uuid::new_v4(), pair.grant.id, large)
            .await?,
        ComputeRejection::InvalidRequest,
    );
    assert_eq!(pair.executor.calls.load(Ordering::SeqCst), 0);
    let job = Uuid::new_v4();
    receipt(pair.submit(job).await?);
    notified(&pair.executor.started).await;
    pair.executor.release.add_permits(1);
    assert_eq!(
        pair.wait_terminal(job).await?.payload.status,
        ComputeStatus::Failed {
            failure: ComputeFailure::InvalidOutput
        }
    );
    pair.close().await
}
