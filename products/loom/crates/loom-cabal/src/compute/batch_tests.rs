//! Protocol ownership fixtures; these are not native decode qualification.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Notify, Semaphore};

#[derive(Debug)]
struct BatchFixture {
    calls: AtomicUsize,
    observed: Mutex<Vec<(PublicKey, Uuid)>>,
    started: Notify,
    cancelled: Notify,
    release: Semaphore,
    wait_for_cancel: bool,
}

impl BatchFixture {
    fn new(wait_for_cancel: bool) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            observed: Mutex::new(Vec::new()),
            started: Notify::new(),
            cancelled: Notify::new(),
            release: Semaphore::new(0),
            wait_for_cancel,
        })
    }
}

#[derive(Debug)]
struct FixtureExecutor(Arc<BatchFixture>);

impl ComputeExecutor for FixtureExecutor {
    fn batch_limit(&self) -> usize {
        4
    }

    fn batch_compatible(&self, first: &HostComputeJob, next: &HostComputeJob) -> bool {
        first.grant.cabal == next.grant.cabal
            && first.grant.epoch == next.grant.epoch
            && first.grant.model == next.grant.model
    }

    fn execute(&self, _job: HostComputeJob, _cancel: CancellationToken) -> ComputeFuture {
        panic!("the batch-aware executor must receive the batch envelope")
    }

    fn execute_batch(&self, jobs: Vec<ComputeBatchJob>) -> ComputeBatchFuture {
        let owner = self.0.clone();
        Box::pin(async move {
            owner.calls.fetch_add(1, Ordering::SeqCst);
            *owner.observed.lock().expect("observed batch") = jobs
                .iter()
                .map(|item| (item.job.peer, item.job.id))
                .collect();
            owner.started.notify_one();
            if owner.wait_for_cancel {
                jobs[0].cancel.cancelled().await;
                assert!(
                    !jobs[1].cancel.is_cancelled(),
                    "cancellation leaked to a sibling"
                );
                owner.cancelled.notify_one();
            }
            owner
                .release
                .acquire()
                .await
                .expect("release fixture")
                .forget();
            // Reversed completion order must still route by (peer, job), not
            // UUID alone or a vector's incidental position.
            jobs.into_iter()
                .rev()
                .map(|item| ComputeBatchOutput {
                    peer: item.job.peer,
                    job: item.job.id,
                    result: Ok(item.job.peer.to_string()),
                })
                .collect()
        })
    }
}

fn grant(peer: PublicKey, cabal: Uuid) -> ComputeGrant {
    ComputeGrant {
        id: Uuid::new_v4(),
        cabal,
        epoch: 1,
        peer,
        model: ComputeModel {
            fingerprint: "ab".repeat(32),
            name: "Batch ownership fixture".into(),
            media: Vec::new(),
        },
        max_output_tokens: 16,
        max_seconds: 120,
        jobs: 4,
    }
}

fn input() -> ComputeInput {
    ComputeInput {
        prompt: "An explicitly requested continuation".into(),
        format: ComputePromptFormat::Raw,
        max_output_tokens: 16,
        seed: 1,
        media: Vec::new(),
    }
}

fn submit(host: &ComputeHost, grant: &ComputeGrant, job: Uuid) -> Result<RemoteJobReceipt> {
    match host.respond(
        grant.peer,
        Request::Submit {
            job,
            grant: grant.id,
            input: input(),
        },
    )? {
        Response::Receipt { receipt } => Ok(*receipt),
        other => panic!("expected accepted job, got {other:?}"),
    }
}

fn status(host: &ComputeHost, grant: &ComputeGrant, job: Uuid) -> Result<RemoteJobReceipt> {
    match host.respond(grant.peer, Request::Status { job })? {
        Response::Receipt { receipt } => Ok(*receipt),
        other => panic!("expected durable job, got {other:?}"),
    }
}

async fn observed(notify: &Notify) {
    tokio::time::timeout(Duration::from_secs(5), notify.notified())
        .await
        .expect("bounded fixture observation");
}

async fn terminal(host: &ComputeHost, grant: &ComputeGrant, job: Uuid) -> Result<RemoteJobReceipt> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let receipt = status(host, grant, job)?;
            if receipt.payload.status.is_terminal() {
                return Ok(receipt);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("bounded terminal observation")
}

#[tokio::test]
async fn independent_peers_share_an_envelope_but_not_cancellation_or_identity() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let identity = Identity::generate()?;
    let fixture = BatchFixture::new(true);
    let host = ComputeHost::open(
        directory.path(),
        identity.clone(),
        Arc::new(|_| true),
        Arc::new(FixtureExecutor(fixture.clone())),
    )?;
    let cabal = Uuid::new_v4();
    let first = grant(Identity::generate()?.public_key(), cabal);
    let second = grant(Identity::generate()?.public_key(), cabal);
    host.grant(first.clone())?;
    host.grant(second.clone())?;
    // Deliberate UUID collision across requesters: the actual key is (peer, job).
    let job = Uuid::new_v4();
    submit(&host, &first, job)?;
    submit(&host, &second, job)?;
    observed(&fixture.started).await;
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *fixture.observed.lock().expect("batch"),
        vec![(first.peer, job), (second.peer, job)]
    );
    let _ = host.respond(
        first.peer,
        Request::Cancel {
            job,
            grant: first.id,
            input: input(),
        },
    )?;
    observed(&fixture.cancelled).await;
    assert!(matches!(
        status(&host, &first, job)?.payload.status,
        ComputeStatus::Cancelling { .. }
    ));
    assert_eq!(
        status(&host, &second, job)?.payload.status,
        ComputeStatus::Running
    );
    fixture.release.add_permits(1);
    let cancelled = terminal(&host, &first, job).await?;
    let completed = terminal(&host, &second, job).await?;
    assert_eq!(
        cancelled.payload.status,
        ComputeStatus::Cancelled {
            reason: ComputeCancellation::Requested
        }
    );
    assert_eq!(
        completed.payload.status,
        ComputeStatus::Completed {
            text: second.peer.to_string()
        }
    );
    assert_eq!(submit(&host, &first, job)?.hash()?, cancelled.hash()?);
    assert_eq!(submit(&host, &second, job)?.hash()?, completed.hash()?);
    assert!(
        host.grant_statuses()?
            .iter()
            .all(|item| item.jobs_remaining == 3)
    );
    host.shutdown().await?;
    drop(host);
    let reopened = ComputeHost::open(
        directory.path(),
        identity,
        Arc::new(|_| true),
        Arc::new(FixtureExecutor(fixture.clone())),
    )?;
    assert_eq!(submit(&reopened, &first, job)?.hash()?, cancelled.hash()?);
    assert_eq!(submit(&reopened, &second, job)?.hash()?, completed.hash()?);
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "restart replayed a settled batch"
    );
    reopened.shutdown().await
}

#[tokio::test]
async fn bounded_collection_rejects_cross_scope_and_excess_without_spending_grants() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let fixture = BatchFixture::new(false);
    let host = ComputeHost::open(
        directory.path(),
        Identity::generate()?,
        Arc::new(|_| true),
        Arc::new(FixtureExecutor(fixture.clone())),
    )?;
    let cabal = Uuid::new_v4();
    let grants = (0..5)
        .map(|_| Ok(grant(Identity::generate()?.public_key(), cabal)))
        .collect::<Result<Vec<_>>>()?;
    for grant in &grants {
        host.grant(grant.clone())?;
    }
    for grant in grants.iter().take(4) {
        submit(&host, grant, Uuid::new_v4())?;
    }
    let rejected = host.respond(
        grants[4].peer,
        Request::Submit {
            job: Uuid::new_v4(),
            grant: grants[4].id,
            input: input(),
        },
    )?;
    assert!(matches!(
        rejected,
        Response::Rejected {
            reason: ComputeRejection::Busy
        }
    ));
    assert_eq!(
        host.grant_statuses()?
            .iter()
            .find(|item| item.grant.id == grants[4].id)
            .expect("grant")
            .jobs_remaining,
        4
    );
    fixture.release.add_permits(1);
    observed(&fixture.started).await;
    host.shutdown().await?;

    let directory = tempfile::tempdir()?;
    let fixture = BatchFixture::new(false);
    let host = ComputeHost::open(
        directory.path(),
        Identity::generate()?,
        Arc::new(|_| true),
        Arc::new(FixtureExecutor(fixture.clone())),
    )?;
    let first = grant(Identity::generate()?.public_key(), cabal);
    let other = grant(Identity::generate()?.public_key(), Uuid::new_v4());
    host.grant(first.clone())?;
    host.grant(other.clone())?;
    submit(&host, &first, Uuid::new_v4())?;
    let rejected = host.respond(
        other.peer,
        Request::Submit {
            job: Uuid::new_v4(),
            grant: other.id,
            input: input(),
        },
    )?;
    assert!(matches!(
        rejected,
        Response::Rejected {
            reason: ComputeRejection::Busy
        }
    ));
    fixture.release.add_permits(1);
    host.shutdown().await
}

#[tokio::test]
async fn revoked_collected_job_is_not_dispatched_and_does_not_cancel_its_sibling() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let fixture = BatchFixture::new(false);
    let host = ComputeHost::open(
        directory.path(),
        Identity::generate()?,
        Arc::new(|_| true),
        Arc::new(FixtureExecutor(fixture.clone())),
    )?;
    let cabal = Uuid::new_v4();
    let first = grant(Identity::generate()?.public_key(), cabal);
    let second = grant(Identity::generate()?.public_key(), cabal);
    host.grant(first.clone())?;
    host.grant(second.clone())?;
    let first_job = Uuid::new_v4();
    let second_job = Uuid::new_v4();
    submit(&host, &first, first_job)?;
    submit(&host, &second, second_job)?;
    host.revoke(first.id)?;
    fixture.release.add_permits(1);
    assert_eq!(
        terminal(&host, &first, first_job).await?.payload.status,
        ComputeStatus::Cancelled {
            reason: ComputeCancellation::GrantRevoked
        }
    );
    assert_eq!(
        terminal(&host, &second, second_job).await?.payload.status,
        ComputeStatus::Completed {
            text: second.peer.to_string()
        }
    );
    assert_eq!(
        *fixture.observed.lock().expect("batch"),
        vec![(second.peer, second_job)]
    );
    host.shutdown().await
}
