//! Whole, explicitly granted model jobs. These receipts are remote assertions;
//! a signature authenticates the host, not its model or execution environment.
mod store;
mod wire;

use std::{
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use iroh::PublicKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{Error, Identity, Result, Signed};
use store::Ledger;

pub use wire::Response as ComputeReply;
pub(crate) use wire::{Handler, Request, Response, request};
pub(crate) const ALPN: &[u8] = b"app.delysis.loom/compute/1";
pub const MAX_COMPUTE_TEXT_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_TOKENS: u32 = 2048;
const MAX_JOB_SECONDS: u32 = 120;
const MAX_GRANT_JOBS: u32 = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeModel {
    /// Digest of the host's exact verified model configuration. Never a path.
    pub fingerprint: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeGrant {
    pub id: Uuid,
    pub cabal: Uuid,
    pub epoch: u64,
    pub peer: PublicKey,
    pub model: ComputeModel,
    pub max_output_tokens: u32,
    pub max_seconds: u32,
    /// Lifetime budget. Retrying an existing job never spends another unit.
    pub jobs: u32,
}

impl ComputeGrant {
    fn validate(&self) -> Result<()> {
        if self.id.is_nil()
            || self.cabal.is_nil()
            || self.model.fingerprint.len() != 64
            || !self
                .model
                .fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || self.model.name.is_empty()
            || self.model.name.len() > 160
            || self.model.name.chars().any(char::is_control)
            || !(1..=MAX_OUTPUT_TOKENS).contains(&self.max_output_tokens)
            || !(1..=MAX_JOB_SECONDS).contains(&self.max_seconds)
            || !(1..=MAX_GRANT_JOBS).contains(&self.jobs)
        {
            return Err(Error::Invalid("Invalid compute grant"));
        }
        Ok(())
    }
}

/// Text-only completion, with all document references already resolved by the
/// requesting device. No expression, filename, or local-tool authority crosses
/// this boundary. Additional modalities need their own bounded input contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeInput {
    pub prompt: String,
    pub max_output_tokens: u32,
    pub seed: u32,
}

impl ComputeInput {
    fn validate(&self) -> Result<()> {
        if self.prompt.len() > MAX_COMPUTE_TEXT_BYTES
            || !(1..=MAX_OUTPUT_TOKENS).contains(&self.max_output_tokens)
        {
            return Err(Error::Invalid("Compute input exceeds limits"));
        }
        Ok(())
    }

    pub fn fingerprint(&self, grant: Uuid) -> Result<String> {
        self.validate()?;
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&(
            "loom_compute_input_v1",
            grant,
            self,
        ))?)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeCancellation {
    Requested,
    GrantRevoked,
    TimeLimit,
    HostStopping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeFailure {
    HostBusy,
    ModelUnavailable,
    InputUnsupported,
    ExecutionFailed,
    InvalidOutput,
    WorkerPanicked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputeStatus {
    Accepted,
    Running,
    Cancelling {
        reason: ComputeCancellation,
    },
    Completed {
        text: String,
    },
    Cancelled {
        reason: ComputeCancellation,
    },
    Failed {
        failure: ComputeFailure,
    },
    /// The prior host process stopped without a durable terminal receipt.
    /// This job is never automatically submitted to a model again.
    Interrupted,
}

impl ComputeStatus {
    pub fn is_terminal(&self) -> bool {
        !matches!(
            self,
            Self::Accepted | Self::Running | Self::Cancelling { .. }
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteJobRecord {
    kind: RemoteRecordKind,
    pub job: Uuid,
    pub peer: PublicKey,
    pub grant: Uuid,
    pub request_fingerprint: String,
    pub model: ComputeModel,
    pub revision: u32,
    pub created_at_ms: u64,
    pub recorded_at_ms: u64,
    pub status: ComputeStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum RemoteRecordKind {
    #[serde(rename = "loom_remote_execution_v1")]
    Execution,
}

pub type RemoteJobReceipt = Signed<RemoteJobRecord>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeRejection {
    Denied,
    Busy,
    InvalidRequest,
    MismatchedRetry,
    Exhausted,
    Stopped,
    Unavailable,
}

/// The host adapter must own and join its actual native model worker, including
/// after cancellation. Dropping a future is not worker shutdown. Local model
/// selection and idle admission belong to that adapter, never to peer input.
pub trait ComputeExecutor: std::fmt::Debug + Send + Sync + 'static {
    fn execute(
        &self,
        job: HostComputeJob,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<String, ComputeFailure>> + Send>>;
}

#[derive(Clone, Debug)]
pub struct HostComputeJob {
    pub id: Uuid,
    pub peer: PublicKey,
    pub grant: ComputeGrant,
    pub input: ComputeInput,
}

type Authority = Arc<dyn Fn(&ComputeGrant) -> bool + Send + Sync>;

struct HostState {
    ledger: Ledger,
    active: Option<HostComputeJob>,
    closed: bool,
    failure: Option<String>,
}

pub struct ComputeHost {
    state: Arc<Mutex<HostState>>,
    authority: Authority,
    pending: mpsc::Sender<HostComputeJob>,
    stop: CancellationToken,
    worker: tokio::sync::Mutex<Option<JoinHandle<Result<()>>>>,
}

impl std::fmt::Debug for ComputeHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputeHost").finish_non_exhaustive()
    }
}

impl ComputeHost {
    pub(crate) fn open(
        directory: &Path,
        identity: Identity,
        authority: Authority,
        executor: Arc<dyn ComputeExecutor>,
    ) -> Result<Arc<Self>> {
        let state = Arc::new(Mutex::new(HostState {
            ledger: Ledger::open(directory, identity)?,
            active: None,
            closed: false,
            failure: None,
        }));
        let (pending, receiver) = mpsc::channel(1);
        let stop = CancellationToken::new();
        let worker = tokio::spawn(supervise(
            state.clone(),
            authority.clone(),
            executor,
            receiver,
            stop.clone(),
        ));
        Ok(Arc::new(Self {
            state,
            authority,
            pending,
            stop,
            worker: tokio::sync::Mutex::new(Some(worker)),
        }))
    }

    /// Called only by the local host, after explicit user selection of a peer
    /// and verified model. Remote protocol requests have no grant operation.
    pub fn grant(&self, grant: ComputeGrant) -> Result<()> {
        grant.validate()?;
        let mut state = self.lock()?;
        if state.closed || self.stop.is_cancelled() || !(self.authority)(&grant) {
            return Err(Error::Invalid(
                "Compute grant is outside current membership",
            ));
        }
        state.ledger.grant(grant)
    }

    pub fn revoke(&self, grant: Uuid) -> Result<()> {
        let result = (|| {
            let mut state = self.lock()?;
            state.ledger.revoke(grant)?;
            if let Some(job) = state.active.clone().filter(|job| job.grant.id == grant) {
                state
                    .ledger
                    .cancel(job.peer, job.id, ComputeCancellation::GrantRevoked)?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.stop();
        }
        result
    }

    pub fn grants(&self) -> Result<Vec<ComputeGrant>> {
        self.lock()?.ledger.grants()
    }

    pub fn stop(&self) {
        self.stop.cancel();
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.stop();
        // Retain this lock through joining: a concurrent shutdown must wait too.
        let mut slot = self.worker.lock().await;
        if let Some(worker) = slot.take() {
            let result = worker
                .await
                .map_err(|_| Error::Invalid("Compute supervisor stopped unexpectedly"))
                .and_then(|result| result);
            if let Err(error) = result {
                self.lock()?.failure = Some(error.to_string());
            }
        }
        match &self.lock()?.failure {
            Some(failure) => Err(Error::Network(failure.clone())),
            None => Ok(()),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HostState>> {
        self.state
            .lock()
            .map_err(|_| Error::Invalid("Compute owner stopped"))
    }

    fn respond(&self, peer: PublicKey, request: Request) -> Result<Response> {
        let mut state = self.lock()?;
        // Retrieval/cancellation only touch this peer's existing jobs. They do
        // not require a continuing grant and can never start model execution.
        match request {
            Request::Status { job } => Ok(receipt_response(state.ledger.get(peer, job)?)),
            Request::Cancel { job } => Ok(receipt_response(state.ledger.cancel(
                peer,
                job,
                ComputeCancellation::Requested,
            )?)),
            Request::Offers { cabal } => {
                if state.closed || self.stop.is_cancelled() {
                    return Ok(Response::Rejected {
                        reason: ComputeRejection::Stopped,
                    });
                }
                Ok(Response::Offers {
                    grants: state
                        .ledger
                        .grants()?
                        .into_iter()
                        .filter(|grant| {
                            grant.peer == peer && grant.cabal == cabal && (self.authority)(grant)
                        })
                        .collect(),
                })
            }
            Request::Submit { job, grant, input } => {
                let reject = |reason| Ok(Response::Rejected { reason });
                if job.is_nil() || input.validate().is_err() {
                    return reject(ComputeRejection::InvalidRequest);
                }
                let fingerprint = input.fingerprint(grant)?;
                if let Some(existing) = state.ledger.get(peer, job)? {
                    return if existing.payload.request_fingerprint == fingerprint {
                        Ok(Response::Receipt {
                            receipt: Box::new(existing),
                        })
                    } else {
                        reject(ComputeRejection::MismatchedRetry)
                    };
                }
                if state.closed || self.stop.is_cancelled() {
                    return reject(ComputeRejection::Stopped);
                }
                let Some(grant) = state.ledger.find_grant(grant)? else {
                    return reject(ComputeRejection::Denied);
                };
                if grant.peer != peer || !(self.authority)(&grant) {
                    return reject(ComputeRejection::Denied);
                }
                if input.max_output_tokens > grant.max_output_tokens {
                    return reject(ComputeRejection::InvalidRequest);
                }
                if state.active.is_some() {
                    return reject(ComputeRejection::Busy);
                }
                if !state.ledger.has_capacity(&grant)? {
                    return reject(ComputeRejection::Exhausted);
                }
                let pending = HostComputeJob {
                    id: job,
                    peer,
                    grant,
                    input,
                };
                let receipt = state.ledger.accept(&pending, fingerprint)?;
                state.active = Some(pending.clone());
                if self.pending.try_send(pending).is_err() {
                    state.closed = true;
                    state
                        .ledger
                        .transition(peer, job, ComputeStatus::Interrupted)?;
                    return reject(ComputeRejection::Unavailable);
                }
                Ok(Response::Receipt {
                    receipt: Box::new(receipt),
                })
            }
        }
    }
}

impl Drop for ComputeHost {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn receipt_response(receipt: Option<RemoteJobReceipt>) -> Response {
    receipt.map_or(
        Response::Rejected {
            reason: ComputeRejection::Denied,
        },
        |receipt| Response::Receipt {
            receipt: Box::new(receipt),
        },
    )
}

async fn supervise(
    state: Arc<Mutex<HostState>>,
    authority: Authority,
    executor: Arc<dyn ComputeExecutor>,
    mut pending: mpsc::Receiver<HostComputeJob>,
    stop: CancellationToken,
) -> Result<()> {
    let outcome = loop {
        let job = tokio::select! {
            biased;
            value = pending.recv() => match value { Some(job) => job, None => break Ok(()) },
            () = stop.cancelled() => break Ok(()),
        };
        if let Err(error) = run_job(&state, &authority, executor.clone(), job, &stop).await {
            // A persistence failure must not open the slot or replay a model.
            stop.cancel();
            break Err(error);
        }
        if stop.is_cancelled() {
            break Ok(());
        }
    };
    let cleanup = (|| {
        let mut state = state
            .lock()
            .map_err(|_| Error::Invalid("Compute owner stopped"))?;
        state.closed = true;
        if let Some(job) = state.active.take() {
            state
                .ledger
                .transition(job.peer, job.id, ComputeStatus::Interrupted)?;
        }
        Ok(())
    })();
    outcome.and(cleanup)
}

fn cancellation(
    state: &mut HostState,
    authority: &Authority,
    job: &HostComputeJob,
    stop: &CancellationToken,
    expired: bool,
) -> Result<Option<ComputeCancellation>> {
    let reason = if stop.is_cancelled() {
        Some(ComputeCancellation::HostStopping)
    } else if state.ledger.find_grant(job.grant.id)?.is_none() || !authority(&job.grant) {
        Some(ComputeCancellation::GrantRevoked)
    } else if expired {
        Some(ComputeCancellation::TimeLimit)
    } else {
        None
    };
    let receipt = if let Some(reason) = reason {
        state.ledger.cancel(job.peer, job.id, reason)?
    } else {
        state.ledger.get(job.peer, job.id)?
    }
    .ok_or(Error::Invalid("Compute job disappeared"))?;
    Ok(match receipt.payload.status {
        ComputeStatus::Cancelling { reason } => Some(reason),
        _ => None,
    })
}

async fn run_job(
    state: &Mutex<HostState>,
    authority: &Authority,
    executor: Arc<dyn ComputeExecutor>,
    job: HostComputeJob,
    stop: &CancellationToken,
) -> Result<()> {
    {
        let mut state = state
            .lock()
            .map_err(|_| Error::Invalid("Compute owner stopped"))?;
        if let Some(reason) = cancellation(&mut state, authority, &job, stop, false)? {
            state
                .ledger
                .transition(job.peer, job.id, ComputeStatus::Cancelled { reason })?;
            state.active = None;
            return Ok(());
        }
        state
            .ledger
            .transition(job.peer, job.id, ComputeStatus::Running)?;
    }
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let worker_job = job.clone();
    // A separately joined task contains adapter panics. Cancellation never drops
    // this task: the adapter retains ownership until native worker joining ends.
    let mut worker = tokio::spawn(async move { executor.execute(worker_job, worker_cancel).await });
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(u64::from(job.grant.max_seconds));
    let mut persistence_failure = None;
    let result = loop {
        tokio::select! {
            result = &mut worker => break result,
            () = tokio::time::sleep(Duration::from_millis(50)) => {
                let observed = state.lock().map_err(|_| Error::Invalid("Compute owner stopped"))
                    .and_then(|mut state| cancellation(&mut state, authority, &job, stop, tokio::time::Instant::now() >= deadline));
                match observed {
                    Ok(Some(_)) => cancel.cancel(),
                    Ok(None) => (),
                    Err(error) => { cancel.cancel(); persistence_failure = Some(error); }
                }
            }
        }
    };
    // Even failed durable cancellation waits for the adapter to join first.
    if let Some(error) = persistence_failure {
        return Err(error);
    }
    let mut state = state
        .lock()
        .map_err(|_| Error::Invalid("Compute owner stopped"))?;
    let status = if let Some(reason) = cancellation(
        &mut state,
        authority,
        &job,
        stop,
        tokio::time::Instant::now() >= deadline,
    )? {
        ComputeStatus::Cancelled { reason }
    } else {
        match result {
            Ok(Ok(text)) if text.len() <= MAX_COMPUTE_TEXT_BYTES => {
                ComputeStatus::Completed { text }
            }
            Ok(Ok(_)) => ComputeStatus::Failed {
                failure: ComputeFailure::InvalidOutput,
            },
            Ok(Err(failure)) => ComputeStatus::Failed { failure },
            Err(_) => ComputeStatus::Failed {
                failure: ComputeFailure::WorkerPanicked,
            },
        }
    };
    state.ledger.transition(job.peer, job.id, status)?;
    state.active = None;
    Ok(())
}

fn now_ms() -> Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Invalid("Compute clock is unavailable"))?
            .as_millis(),
    )
    .map_err(|_| Error::Invalid("Compute clock overflow"))
}
