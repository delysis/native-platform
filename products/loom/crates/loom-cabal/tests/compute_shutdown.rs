//! Exercise shutdown through the public host and authenticated QUIC boundary.
//! The executor is a lifecycle fixture, not evidence of native model execution.
use loom_cabal::{
    Cabal, Identity, Network, NetworkMode, Result,
    compute::{
        ComputeCancellation, ComputeExecutor, ComputeFailure, ComputeGrant, ComputeHost,
        ComputeInput, ComputeModel, ComputePromptFormat, ComputeReply, ComputeStatus,
        HostComputeJob,
    },
};
use std::{
    future::{Future, poll_fn},
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug)]
struct ParkedExecutor {
    started: Arc<Notify>,
    cancelled: Arc<Notify>,
    exited: Arc<Notify>,
    release: Arc<Semaphore>,
}

impl ComputeExecutor for ParkedExecutor {
    fn execute(
        &self,
        _job: HostComputeJob,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<String, ComputeFailure>> + Send>> {
        let started = self.started.clone();
        let cancelled = self.cancelled.clone();
        let exited = self.exited.clone();
        let release = self.release.clone();
        Box::pin(async move {
            started.notify_one();
            cancel.cancelled().await;
            cancelled.notify_one();
            // Observing cancellation is deliberately not the same as joining.
            release.acquire().await.expect("release executor").forget();
            exited.notify_one();
            Ok("must not become successful output".into())
        })
    }
}

async fn observed(signal: &Notify) {
    tokio::time::timeout(Duration::from_secs(5), signal.notified())
        .await
        .expect("executor observation");
}

async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

#[tokio::test]
async fn abandoned_shutdown_cannot_detach_the_worker_or_make_retry_report_drained() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let identity = Identity::generate()?;
    let peer_identity = Identity::generate()?;
    let mut cabal = Cabal::create(
        &directory.path().join("cabal.db"),
        identity.clone(),
        "Shutdown regression",
        "Host",
    )?;
    let invitation = cabal.invite(identity.public_key().into())?;
    cabal.admit(&invitation.token, peer_identity.public_key(), "Requester")?;
    let grant = ComputeGrant {
        id: Uuid::new_v4(),
        cabal: cabal.id(),
        epoch: cabal.roster().payload.epoch,
        peer: peer_identity.public_key(),
        model: ComputeModel {
            fingerprint: "ab".repeat(32),
            name: "Lifecycle fixture".into(),
            media: Vec::new(),
        },
        max_output_tokens: 16,
        max_seconds: 120,
        jobs: 1,
    };
    let network = Network::start(&identity, NetworkMode::Direct {}).await?;
    network.add(Arc::new(Mutex::new(cabal)))?;
    let peer = Network::start(&peer_identity, NetworkMode::Direct {}).await?;
    let executor = Arc::new(ParkedExecutor {
        started: Arc::new(Notify::new()),
        cancelled: Arc::new(Notify::new()),
        exited: Arc::new(Notify::new()),
        release: Arc::new(Semaphore::new(0)),
    });
    let host: Arc<ComputeHost> =
        network.host_compute(&directory.path().join("compute"), executor.clone())?;
    host.grant(grant.clone())?;
    let job = Uuid::new_v4();
    let response = peer
        .compute_submit(
            network.address(),
            job,
            grant.id,
            ComputeInput {
                prompt: "An explicitly requested job".into(),
                format: ComputePromptFormat::Raw,
                max_output_tokens: 16,
                seed: 1,
                media: Vec::new(),
            },
        )
        .await?;
    assert!(matches!(response, ComputeReply::Receipt { .. }));
    observed(&executor.started).await;

    let mut abandoned = Box::pin(host.shutdown());
    assert!(poll_once(abandoned.as_mut()).await.is_pending());
    observed(&executor.cancelled).await;
    // Simulate timeout/caller cancellation while the adapter still owns work.
    drop(abandoned);

    let mut retry = Box::pin(host.shutdown());
    let observation = poll_once(retry.as_mut()).await;
    let retained_ownership = observation.is_pending();
    // Release even on regression so the test never leaves the fixture parked.
    executor.release.add_permits(1);
    match observation {
        Poll::Pending => tokio::time::timeout(Duration::from_secs(5), retry)
            .await
            .expect("shutdown joins released executor")?,
        Poll::Ready(result) => result?,
    }
    observed(&executor.exited).await;
    if retained_ownership {
        let response = peer.compute_status(network.address(), job).await?;
        let ComputeReply::Receipt { receipt } = response else {
            panic!("expected the durable shutdown receipt");
        };
        assert_eq!(
            receipt.payload.status,
            ComputeStatus::Cancelled {
                reason: ComputeCancellation::HostStopping,
            }
        );
        // Completed shutdown remains idempotent, including its durable outcome.
        host.shutdown().await?;
    }
    network.shutdown().await?;
    peer.shutdown().await?;
    assert!(
        retained_ownership,
        "shutdown returned while the cancelled executor had not joined"
    );
    Ok(())
}
