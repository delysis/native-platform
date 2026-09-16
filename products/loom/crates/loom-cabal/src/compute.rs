//! Whole, explicitly granted model jobs. These receipts are remote assertions;
//! a signature authenticates the host, not its model or execution environment.
mod batching;
mod client;
mod media;
mod retention;
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

pub use client::{ClientJob, ClientRequest, ComputeClient};
pub use media::{
    ComputeMedia, ComputeMediaFormat, ComputeModality, MAX_COMPUTE_FRAME_BYTES,
    MAX_COMPUTE_MEDIA_BYTES, MAX_COMPUTE_MEDIA_OBJECTS,
};
pub use wire::Response as ComputeReply;
pub(crate) use wire::{Handler, Request, Response, request};
pub(crate) const ALPN: &[u8] = b"app.delysis.loom/compute/4";
const STORAGE_VERSION: i64 = 4;
pub const MAX_COMPUTE_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_COMPUTE_BATCH_JOBS: usize = 4;
const MAX_OUTPUT_TOKENS: u32 = 2048;
const MAX_JOB_SECONDS: u32 = 120;
const MAX_GRANT_JOBS: u32 = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeModel {
    /// Digest of the host's exact verified model configuration. Never a path.
    pub fingerprint: String,
    pub name: String,
    /// Modalities attested by the host's verified model and loaded projector.
    /// Text completion is required for every offer.
    pub media: Vec<ComputeModality>,
}

impl ComputeModel {
    fn validate(&self) -> Result<()> {
        if !valid_digest(&self.fingerprint)
            || self.name.is_empty()
            || self.name.len() > 160
            || self.name.chars().any(char::is_control)
            || self.media.len() > 2
            || self.media.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(Error::Invalid("Invalid compute model"));
        }
        Ok(())
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ComputeGrantStatus {
    pub grant: ComputeGrant,
    pub jobs_remaining: u32,
    pub current: bool,
}

impl ComputeGrant {
    fn validate(&self) -> Result<()> {
        self.model.validate()?;
        if self.id.is_nil()
            || self.cabal.is_nil()
            || !(1..=MAX_OUTPUT_TOKENS).contains(&self.max_output_tokens)
            || !(1..=MAX_JOB_SECONDS).contains(&self.max_seconds)
            || !(1..=MAX_GRANT_JOBS).contains(&self.jobs)
        {
            return Err(Error::Invalid("Invalid compute grant"));
        }
        Ok(())
    }
}

/// Completion with references resolved and exact media retained by the requester.
/// No expression, filename, or local-tool authority crosses this boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeInput {
    pub prompt: String,
    pub format: ComputePromptFormat,
    pub max_output_tokens: u32,
    pub seed: u32,
    pub media: Vec<ComputeMedia>,
}

/// Explicit model framing, bound to the exact request and every retry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputePromptFormat {
    Raw,
    Function,
}

impl ComputeInput {
    fn validate(&self) -> Result<()> {
        if self.prompt.len() > MAX_COMPUTE_TEXT_BYTES
            || !(1..=MAX_OUTPUT_TOKENS).contains(&self.max_output_tokens)
        {
            return Err(Error::Invalid("Compute input exceeds limits"));
        }
        media::validate(&self.media)
    }

    pub fn validate_for_model(&self, model: &ComputeModel) -> Result<()> {
        if self
            .media
            .iter()
            .any(|item| !model.media.contains(&item.format.modality()))
        {
            return Err(Error::Invalid(
                "The friend's model does not accept every image or audio input",
            ));
        }
        self.validate()
    }

    pub fn fingerprint(&self, grant: Uuid) -> Result<String> {
        self.validate()?;
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&(
            "loom_compute_input_v3",
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

impl RemoteJobRecord {
    /// Shared structural checks for wire replies and both durable ledgers.
    /// These do not establish that the host performed the claimed execution.
    fn validate(&self) -> Result<()> {
        self.model.validate()?;
        let revision_valid = match self.status {
            ComputeStatus::Accepted => self.revision == 0,
            ComputeStatus::Running => self.revision == 1,
            ComputeStatus::Cancelling { .. } => (1..=2).contains(&self.revision),
            ComputeStatus::Cancelled { .. } => (2..=3).contains(&self.revision),
            ComputeStatus::Completed { .. } | ComputeStatus::Failed { .. } => self.revision == 2,
            ComputeStatus::Interrupted => (1..=3).contains(&self.revision),
        };
        if self.job.is_nil()
            || self.grant.is_nil()
            || !valid_digest(&self.request_fingerprint)
            || self.created_at_ms > self.recorded_at_ms
            || !revision_valid
            || matches!(&self.status, ComputeStatus::Completed { text } if text.len() > MAX_COMPUTE_TEXT_BYTES)
        {
            return Err(Error::Invalid("Invalid remote compute result"));
        }
        Ok(())
    }
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

pub type ComputeFuture =
    Pin<Box<dyn Future<Output = std::result::Result<String, ComputeFailure>> + Send>>;
pub type ComputeBatchFuture = Pin<Box<dyn Future<Output = Vec<ComputeBatchOutput>> + Send>>;

/// A transient scheduling envelope, never a replacement for a durable job.
#[derive(Clone, Debug)]
pub struct ComputeBatchJob {
    pub job: HostComputeJob,
    pub cancel: CancellationToken,
}

#[derive(Debug)]
pub struct ComputeBatchOutput {
    pub peer: PublicKey,
    pub job: Uuid,
    pub result: std::result::Result<String, ComputeFailure>,
}

/// The adapter owns and joins native work, including after cancellation.
/// Batch-aware adapters must cancel individual cases, not their siblings.
pub trait ComputeExecutor: std::fmt::Debug + Send + Sync + 'static {
    fn available(&self, _model: &ComputeModel) -> bool {
        true
    }

    /// Static collection only. This does not promise continuous admission.
    fn batch_limit(&self) -> usize {
        1
    }

    /// The host also enforces identical cabal, membership epoch and exact model.
    /// The adapter must enforce its prompt/media/cache compatibility boundary.
    fn batch_compatible(&self, _first: &HostComputeJob, _next: &HostComputeJob) -> bool {
        false
    }

    fn execute(&self, job: HostComputeJob, cancel: CancellationToken) -> ComputeFuture;

    fn execute_batch(&self, mut jobs: Vec<ComputeBatchJob>) -> ComputeBatchFuture {
        if jobs.len() != 1 {
            return Box::pin(async move {
                jobs.into_iter()
                    .map(|item| ComputeBatchOutput {
                        peer: item.job.peer,
                        job: item.job.id,
                        result: Err(ComputeFailure::InputUnsupported),
                    })
                    .collect()
            });
        }
        let item = jobs.remove(0);
        let peer = item.job.peer;
        let job = item.job.id;
        let work = self.execute(item.job, item.cancel);
        Box::pin(async move {
            vec![ComputeBatchOutput {
                peer,
                job,
                result: work.await,
            }]
        })
    }
}

#[derive(Clone, Debug)]
pub struct HostComputeJob {
    pub id: Uuid,
    pub peer: PublicKey,
    pub grant: ComputeGrant,
    pub input: ComputeInput,
}

struct PendingComputeJob {
    job: HostComputeJob,
    admitted_at: tokio::time::Instant,
}

type Authority = Arc<dyn Fn(&ComputeGrant) -> bool + Send + Sync>;

struct HostState {
    ledger: Ledger,
    active: Vec<HostComputeJob>,
    collecting: bool,
    closed: bool,
    failure: Option<String>,
}

pub struct ComputeHost {
    state: Arc<Mutex<HostState>>,
    authority: Authority,
    executor: Arc<dyn ComputeExecutor>,
    pending: mpsc::Sender<PendingComputeJob>,
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
            active: Vec::new(),
            collecting: false,
            closed: false,
            failure: None,
        }));
        let (pending, receiver) = mpsc::channel(MAX_COMPUTE_BATCH_JOBS);
        let stop = CancellationToken::new();
        let worker = tokio::spawn(batching::supervise(
            state.clone(),
            authority.clone(),
            executor.clone(),
            receiver,
            stop.clone(),
        ));
        Ok(Arc::new(Self {
            state,
            authority,
            executor,
            pending,
            stop,
            worker: tokio::sync::Mutex::new(Some(worker)),
        }))
    }

    /// Called only by the local host after explicit peer and model selection.
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
        if grant.is_nil() {
            return Err(Error::Invalid("Invalid compute grant identity"));
        }
        let result = (|| {
            let mut state = self.lock()?;
            state.ledger.revoke(grant)?;
            for job in state
                .active
                .clone()
                .into_iter()
                .filter(|job| job.grant.id == grant)
            {
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

    pub fn grant_statuses(&self) -> Result<Vec<ComputeGrantStatus>> {
        let state = self.lock()?;
        state
            .ledger
            .grants()?
            .into_iter()
            .map(|grant| {
                Ok(ComputeGrantStatus {
                    jobs_remaining: state.ledger.remaining_jobs(&grant)?,
                    current: (self.authority)(&grant),
                    grant,
                })
            })
            .collect()
    }

    pub fn stop(&self) {
        self.stop.cancel();
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.is_cancelled()
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.stop();
        // Retain the handle across an abandoned or timed-out shutdown future.
        let mut slot = self.worker.lock().await;
        if let Some(worker) = slot.as_mut() {
            let result = worker
                .await
                .map_err(|_| Error::Invalid("Compute supervisor stopped unexpectedly"))
                .and_then(|result| result);
            *slot = None;
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
        let cancel = matches!(&request, Request::Cancel { .. });
        // Exact retries and retrieval precede admission, even after revocation.
        match request {
            Request::Status { job } => Ok(receipt_response(state.ledger.get(peer, job)?)),
            Request::Offers { cabal } => {
                if state.closed || self.stop.is_cancelled() {
                    return Ok(Response::Rejected {
                        reason: ComputeRejection::Stopped,
                    });
                }
                let mut grants = Vec::new();
                for grant in state.ledger.grants()? {
                    if grant.peer == peer
                        && grant.cabal == cabal
                        && (self.authority)(&grant)
                        && self.executor.available(&grant.model)
                        && state.ledger.has_capacity(&grant, None)?
                    {
                        grants.push(grant);
                    }
                }
                Ok(Response::Offers { grants })
            }
            Request::Submit { job, grant, input } | Request::Cancel { job, grant, input } => {
                let reject = |reason| Ok(Response::Rejected { reason });
                if job.is_nil() {
                    return reject(ComputeRejection::InvalidRequest);
                }
                if let Some(existing) = state.ledger.get(peer, job)? {
                    let Ok(fingerprint) = input.fingerprint(grant) else {
                        return reject(ComputeRejection::InvalidRequest);
                    };
                    return if existing.payload.request_fingerprint == fingerprint {
                        Ok(receipt_response(if cancel {
                            state
                                .ledger
                                .cancel(peer, job, ComputeCancellation::Requested)?
                        } else {
                            Some(existing)
                        }))
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
                if input.max_output_tokens > grant.max_output_tokens
                    || input.validate_for_model(&grant.model).is_err()
                {
                    return reject(ComputeRejection::InvalidRequest);
                }
                let fingerprint = input.fingerprint(grant.id)?;
                let pending = HostComputeJob {
                    id: job,
                    peer,
                    grant,
                    input,
                };
                if !cancel {
                    let limit = self.executor.batch_limit().clamp(1, MAX_COMPUTE_BATCH_JOBS);
                    let compatible = state.active.first().is_none_or(|first| {
                        state.collecting
                            && first.grant.cabal == pending.grant.cabal
                            && first.grant.epoch == pending.grant.epoch
                            && first.grant.model == pending.grant.model
                            && self.executor.batch_compatible(first, &pending)
                    });
                    if state.active.len() >= limit
                        || state.active.iter().filter(|item| item.peer == peer).count() >= 2
                        || !compatible
                        || !self.executor.available(&pending.grant.model)
                    {
                        return reject(ComputeRejection::Busy);
                    }
                }
                if !state
                    .ledger
                    .has_capacity(&pending.grant, Some(&pending.input))?
                {
                    return reject(ComputeRejection::Exhausted);
                }
                let admitted_at = tokio::time::Instant::now();
                let receipt = state.ledger.accept(&pending, fingerprint)?;
                if cancel {
                    state
                        .ledger
                        .cancel(peer, job, ComputeCancellation::Requested)?;
                    let receipt = state.ledger.transition(
                        peer,
                        job,
                        ComputeStatus::Cancelled {
                            reason: ComputeCancellation::Requested,
                        },
                    )?;
                    return Ok(Response::Receipt {
                        receipt: Box::new(receipt),
                    });
                }
                if state.active.is_empty() {
                    state.collecting = true;
                }
                state.active.push(pending.clone());
                if self
                    .pending
                    .try_send(PendingComputeJob {
                        job: pending,
                        admitted_at,
                    })
                    .is_err()
                {
                    state.closed = true;
                    self.stop.cancel();
                    state
                        .ledger
                        .transition(peer, job, ComputeStatus::Interrupted)?;
                    state.active.retain(|item| item.peer != peer || item.id != job);
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

fn now_ms() -> Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Invalid("Compute clock is unavailable"))?
            .as_millis(),
    )
    .map_err(|_| Error::Invalid("Compute clock overflow"))
}

#[cfg(test)]
#[path = "compute/record_tests.rs"]
mod record_tests;

#[cfg(test)]
#[path = "compute/batch_tests.rs"]
mod batch_tests;
