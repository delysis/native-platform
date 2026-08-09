use std::ffi::OsString;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use fs4::TryLockError;
use reqwest::StatusCode;
use reqwest::header::{
    ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, HeaderMap, RANGE,
};
use reqwest::redirect;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _, SeekFrom};
use tokio::sync::watch;

use crate::is_gguf_path;

/// Hard library ceiling for one model download (one tebibyte).
pub const MAX_MODEL_DOWNLOAD_BYTES: u64 = 1 << 40;
/// Smallest accepted interval between byte-progress reports.
pub const MIN_PROGRESS_INTERVAL_BYTES: u64 = 64 * 1024;
/// Default byte-progress interval.
pub const DEFAULT_PROGRESS_INTERVAL_BYTES: u64 = 4 * 1024 * 1024;
/// Largest accepted interval between byte-progress reports.
pub const MAX_PROGRESS_INTERVAL_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum number of HTTPS redirect hops.
pub const MAX_REDIRECTS: usize = 5;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const HASH_BUFFER_BYTES: usize = 1024 * 1024;
const GGUF_MAGIC: [u8; 4] = *b"GGUF";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub fn from_hex(value: &str) -> Result<Self, DownloadError> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(DownloadError::InvalidSha256);
        }
        let mut bytes = [0_u8; 32];
        hex::decode_to_slice(value, &mut bytes).map_err(|_| DownloadError::InvalidSha256)?;
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl FromStr for Sha256Digest {
    type Err = DownloadError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_hex(value)
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_hex(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct GgufDownloadRequest {
    pub url: String,
    pub target_path: PathBuf,
    pub expected_sha256: Sha256Digest,
    pub max_bytes: u64,
    pub expected_bytes: Option<u64>,
    pub progress_interval_bytes: u64,
}

impl fmt::Debug for GgufDownloadRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GgufDownloadRequest")
            .field("url", &"<redacted HTTPS URL>")
            .field("target_path", &self.target_path)
            .field("expected_sha256", &self.expected_sha256)
            .field("max_bytes", &self.max_bytes)
            .field("expected_bytes", &self.expected_bytes)
            .field("progress_interval_bytes", &self.progress_interval_bytes)
            .finish()
    }
}

impl GgufDownloadRequest {
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        target_path: impl Into<PathBuf>,
        expected_sha256: Sha256Digest,
        max_bytes: u64,
    ) -> Self {
        Self {
            url: url.into(),
            target_path: target_path.into(),
            expected_sha256,
            max_bytes,
            expected_bytes: None,
            progress_interval_bytes: DEFAULT_PROGRESS_INTERVAL_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadPhase {
    InspectingExisting,
    HashingPartial,
    Downloading,
    Verifying,
    Installing,
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DownloadProgress {
    pub phase: DownloadPhase,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub resumed_from_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DownloadControl {
    #[default]
    Continue,
    Cancel,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadDisposition {
    ReusedExisting,
    DownloadedFresh,
    DownloadedResumed { resumed_from_bytes: u64 },
    DownloadedAfterRestart { discarded_partial_bytes: u64 },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GgufDownloadResult {
    pub target_path: PathBuf,
    pub bytes: u64,
    pub sha256: Sha256Digest,
    pub disposition: DownloadDisposition,
    pub partial_removed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DownloadCancellation {
    inner: Arc<CancellationInner>,
}

#[derive(Debug)]
struct CancellationInner {
    sender: watch::Sender<bool>,
}

impl Default for CancellationInner {
    fn default() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }
}

impl DownloadCancellation {
    pub fn cancel(&self) {
        self.inner.sender.send_replace(true);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.inner.sender.borrow()
    }

    async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut receiver = self.inner.sender.subscribe();
        if *receiver.borrow_and_update() {
            return;
        }
        while receiver.changed().await.is_ok() {
            if *receiver.borrow_and_update() {
                return;
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("download URL is invalid")]
    InvalidUrl,
    #[error("model downloads require an HTTPS URL")]
    HttpsRequired,
    #[error("model download URLs may not contain embedded credentials")]
    CredentialsInUrl,
    #[error("download URL path must name a .gguf file")]
    SourceNotGguf,
    #[error("download target must name a .gguf file")]
    TargetNotGguf,
    #[error("download target must have an existing parent directory")]
    MissingTargetParent,
    #[error("SHA-256 must be exactly 64 hexadecimal characters")]
    InvalidSha256,
    #[error("maximum download size must be between 1 and {MAX_MODEL_DOWNLOAD_BYTES} bytes")]
    InvalidMaximumBytes,
    #[error("expected byte count must be positive and no greater than the maximum")]
    InvalidExpectedBytes,
    #[error(
        "progress interval must be between {MIN_PROGRESS_INTERVAL_BYTES} and {MAX_PROGRESS_INTERVAL_BYTES} bytes"
    )]
    InvalidProgressInterval,
    #[error("{role} path is a symbolic link: {path}")]
    SymlinkPath { role: &'static str, path: PathBuf },
    #[error("{role} path is not a regular file: {path}")]
    NonRegularPath { role: &'static str, path: PathBuf },
    #[error("another download is using the partial file")]
    PartialFileBusy,
    #[error("partial file changed while it was being opened")]
    PartialFileChanged,
    #[error("partial file has {bytes} bytes, exceeding the allowed {limit} bytes")]
    PartialTooLarge { bytes: u64, limit: u64 },
    #[error(
        "existing target does not have the expected byte count: expected {expected}, got {actual}"
    )]
    ExistingSizeMismatch { expected: u64, actual: u64 },
    #[error("existing target does not have the expected SHA-256 digest")]
    ExistingHashMismatch,
    #[error("downloaded file does not have the expected SHA-256 digest")]
    DownloadHashMismatch,
    #[error("file does not begin with the GGUF magic bytes")]
    InvalidGgufMagic,
    #[error("download was cancelled")]
    Cancelled,
    #[error("could not construct the HTTPS client")]
    ClientConfiguration,
    #[error("HTTPS connection failed")]
    NetworkConnect,
    #[error("HTTPS operation timed out")]
    NetworkTimeout,
    #[error("HTTPS response body failed")]
    NetworkBody,
    #[error("HTTPS request failed")]
    NetworkRequest,
    #[error("redirect was rejected because it downgraded HTTPS or exceeded the hop limit")]
    RedirectRejected,
    #[error("server returned unexpected HTTP status {status}")]
    HttpStatus { status: u16 },
    #[error("server returned a non-identity content encoding")]
    EncodedResponse,
    #[error("server returned an invalid Content-Length header")]
    InvalidContentLength,
    #[error("server returned an invalid Content-Range header")]
    InvalidContentRange,
    #[error("server range response starts at {actual}, not requested offset {expected}")]
    RangeStartMismatch { expected: u64, actual: u64 },
    #[error("server range response is internally inconsistent")]
    InconsistentRange,
    #[error("server reports a total of {reported} bytes, but {expected} were required")]
    RemoteSizeMismatch { expected: u64, reported: u64 },
    #[error("response would exceed the {limit}-byte download limit")]
    DownloadTooLarge { limit: u64 },
    #[error("download ended at {actual} bytes, but {expected} were required")]
    DownloadSizeMismatch { expected: u64, actual: u64 },
    #[error("server rejected the resume range and did not report the complete remote size")]
    UnsatisfiedRange,
    #[error("target appeared during installation and does not match the requested model")]
    TargetRaceMismatch,
    #[error("could not {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl DownloadError {
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::PartialFileBusy
            | Self::PartialFileChanged
            | Self::NetworkConnect
            | Self::NetworkTimeout
            | Self::NetworkBody
            | Self::NetworkRequest => true,
            Self::HttpStatus { status } => matches!(*status, 408 | 425 | 429) || *status >= 500,
            _ => false,
        }
    }

    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

#[derive(Debug)]
struct ValidatedRequest {
    url: reqwest::Url,
    partial_path: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ParsedContentRange {
    Satisfied {
        start: u64,
        end: u64,
        total: Option<u64>,
    },
    Unsatisfied {
        total: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResponsePlan {
    Continue { total: Option<u64>, restart: bool },
    PartialAlreadyComplete,
}

#[derive(Debug)]
struct PartialState {
    file: tokio::fs::File,
    initial_bytes: u64,
    hasher: Sha256,
}

#[derive(Debug)]
struct TransferState {
    file: tokio::fs::File,
    response: reqwest::Response,
    hasher: Sha256,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    resumed_from_bytes: u64,
    disposition: DownloadDisposition,
}

#[derive(Clone, Copy, Debug)]
struct CompletedDownload {
    bytes: u64,
    digest: Sha256Digest,
    disposition: DownloadDisposition,
}

struct DownloadSession<'a, F> {
    request: &'a GgufDownloadRequest,
    cancellation: &'a DownloadCancellation,
    progress: F,
}

/// Downloads and atomically installs one verified GGUF.
///
/// The target is never overwritten. A sibling `<target>.part` file is resumed
/// when the server honors the exact byte range, and is retained on all
/// pre-install failures. The callback receives phase changes plus byte reports
/// no more frequently than `progress_interval_bytes` while response data flows.
pub async fn download_gguf<F>(
    request: &GgufDownloadRequest,
    cancellation: &DownloadCancellation,
    progress: F,
) -> Result<GgufDownloadResult, DownloadError>
where
    F: FnMut(DownloadProgress) -> DownloadControl,
{
    DownloadSession {
        request,
        cancellation,
        progress,
    }
    .run()
    .await
}

/// Validates a download request without opening a network connection or
/// creating its partial file.
pub fn validate_gguf_download_request(request: &GgufDownloadRequest) -> Result<(), DownloadError> {
    validate_request(request).map(|_| ())
}

impl<F> DownloadSession<'_, F>
where
    F: FnMut(DownloadProgress) -> DownloadControl,
{
    async fn run(mut self) -> Result<GgufDownloadResult, DownloadError> {
        let validated = validate_request(self.request)?;
        ensure_not_cancelled(self.cancellation)?;
        if path_exists(&self.request.target_path, "target")? {
            self.reuse_existing(&validated).await
        } else {
            self.download_missing(&validated).await
        }
    }

    async fn reuse_existing(
        &mut self,
        validated: &ValidatedRequest,
    ) -> Result<GgufDownloadResult, DownloadError> {
        self.emit(DownloadProgress {
            phase: DownloadPhase::InspectingExisting,
            downloaded_bytes: 0,
            total_bytes: self.request.expected_bytes,
            resumed_from_bytes: 0,
        })?;
        let (bytes, digest) = hash_regular_path(
            &self.request.target_path,
            "target",
            self.request.max_bytes,
            self.cancellation,
        )
        .await?;
        validate_existing(self.request, bytes, digest)?;
        verify_gguf_magic_path(&self.request.target_path, "target").await?;
        self.emit_complete(bytes, 0);
        Ok(GgufDownloadResult {
            target_path: self.request.target_path.clone(),
            bytes,
            sha256: digest,
            disposition: DownloadDisposition::ReusedExisting,
            partial_removed: !path_exists(&validated.partial_path, "partial")?,
        })
    }

    async fn download_missing(
        &mut self,
        validated: &ValidatedRequest,
    ) -> Result<GgufDownloadResult, DownloadError> {
        let mut partial = self.prepare_partial(&validated.partial_path).await?;
        if self.request.expected_bytes == Some(partial.initial_bytes) && partial.initial_bytes > 0 {
            return self
                .finish_known_partial(&validated.partial_path, partial)
                .await;
        }
        partial
            .file
            .seek(SeekFrom::End(0))
            .await
            .map_err(|source| io_error("seek partial file", &validated.partial_path, source))?;
        let response = self
            .send_request(&validated.url, partial.initial_bytes)
            .await?;
        reject_encoded_response(response.headers())?;
        let plan = plan_response(
            response.status(),
            response.headers(),
            partial.initial_bytes,
            self.request,
        )?;
        if plan == ResponsePlan::PartialAlreadyComplete {
            return self
                .finish_known_partial(&validated.partial_path, partial)
                .await;
        }
        let transfer = self
            .prepare_transfer(partial, response, plan, &validated.partial_path)
            .await?;
        self.stream_transfer(transfer, &validated.partial_path)
            .await
    }

    async fn prepare_partial(&mut self, path: &Path) -> Result<PartialState, DownloadError> {
        let file = open_locked_partial(path)?;
        let initial_bytes = file
            .metadata()
            .map_err(|source| io_error("inspect partial file", path, source))?
            .len();
        validate_partial_size(self.request, initial_bytes)?;
        let mut file = tokio::fs::File::from_std(file);
        self.emit(DownloadProgress {
            phase: DownloadPhase::HashingPartial,
            downloaded_bytes: initial_bytes,
            total_bytes: self.request.expected_bytes,
            resumed_from_bytes: initial_bytes,
        })?;
        let (hasher, hashed_bytes) = hash_open_file(&mut file, path, self.cancellation).await?;
        if hashed_bytes != initial_bytes {
            return Err(DownloadError::PartialFileChanged);
        }
        Ok(PartialState {
            file,
            initial_bytes,
            hasher,
        })
    }

    async fn finish_known_partial(
        &mut self,
        path: &Path,
        mut partial: PartialState,
    ) -> Result<GgufDownloadResult, DownloadError> {
        let completed = CompletedDownload {
            bytes: partial.initial_bytes,
            digest: finalize_digest(&partial.hasher),
            disposition: DownloadDisposition::DownloadedResumed {
                resumed_from_bytes: partial.initial_bytes,
            },
        };
        validate_download(self.request, completed.bytes, completed.digest)?;
        verify_gguf_magic_open(&mut partial.file, path).await?;
        self.install(path, &mut partial.file, completed).await
    }

    async fn send_request(
        &self,
        url: &reqwest::Url,
        resume_bytes: u64,
    ) -> Result<reqwest::Response, DownloadError> {
        let client = build_client()?;
        let mut builder = client.get(url.clone()).header(ACCEPT_ENCODING, "identity");
        if resume_bytes > 0 {
            builder = builder.header(RANGE, format!("bytes={resume_bytes}-"));
        }
        let response = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => return Err(DownloadError::Cancelled),
            response = builder.send() => response.map_err(|error| map_request_error(&error))?,
        };
        if response.url().scheme() != "https" {
            return Err(DownloadError::RedirectRejected);
        }
        Ok(response)
    }

    async fn prepare_transfer(
        &self,
        mut partial: PartialState,
        response: reqwest::Response,
        plan: ResponsePlan,
        path: &Path,
    ) -> Result<TransferState, DownloadError> {
        let ResponsePlan::Continue { total, restart } = plan else {
            unreachable!("complete response plan is handled before transfer preparation");
        };
        let (downloaded_bytes, resumed_from_bytes, disposition) = if restart {
            partial
                .file
                .set_len(0)
                .await
                .map_err(|source| io_error("truncate partial file", path, source))?;
            partial
                .file
                .seek(SeekFrom::Start(0))
                .await
                .map_err(|source| io_error("seek partial file", path, source))?;
            partial.hasher = Sha256::new();
            (
                0,
                0,
                DownloadDisposition::DownloadedAfterRestart {
                    discarded_partial_bytes: partial.initial_bytes,
                },
            )
        } else if partial.initial_bytes > 0 {
            (
                partial.initial_bytes,
                partial.initial_bytes,
                DownloadDisposition::DownloadedResumed {
                    resumed_from_bytes: partial.initial_bytes,
                },
            )
        } else {
            (0, 0, DownloadDisposition::DownloadedFresh)
        };
        Ok(TransferState {
            file: partial.file,
            response,
            hasher: partial.hasher,
            downloaded_bytes,
            total_bytes: total,
            resumed_from_bytes,
            disposition,
        })
    }

    async fn stream_transfer(
        &mut self,
        mut transfer: TransferState,
        path: &Path,
    ) -> Result<GgufDownloadResult, DownloadError> {
        self.emit(DownloadProgress {
            phase: DownloadPhase::Downloading,
            downloaded_bytes: transfer.downloaded_bytes,
            total_bytes: transfer.total_bytes.or(self.request.expected_bytes),
            resumed_from_bytes: transfer.resumed_from_bytes,
        })?;
        let mut next_progress = transfer
            .downloaded_bytes
            .saturating_add(self.request.progress_interval_bytes);
        loop {
            let chunk = tokio::select! {
                biased;
                () = self.cancellation.cancelled() => return Err(DownloadError::Cancelled),
                chunk = transfer.response.chunk() => {
                    chunk.map_err(|error| map_request_error(&error))?
                },
            };
            let Some(chunk) = chunk else { break };
            let chunk_bytes =
                u64::try_from(chunk.len()).map_err(|_| DownloadError::DownloadTooLarge {
                    limit: self.request.max_bytes,
                })?;
            let next_bytes = transfer.downloaded_bytes.checked_add(chunk_bytes).ok_or(
                DownloadError::DownloadTooLarge {
                    limit: self.request.max_bytes,
                },
            )?;
            enforce_transfer_bound(self.request, next_bytes)?;
            transfer
                .file
                .write_all(&chunk)
                .await
                .map_err(|source| io_error("write partial file", path, source))?;
            transfer.hasher.update(&chunk);
            transfer.downloaded_bytes = next_bytes;
            if transfer.downloaded_bytes >= next_progress {
                self.emit(DownloadProgress {
                    phase: DownloadPhase::Downloading,
                    downloaded_bytes: transfer.downloaded_bytes,
                    total_bytes: transfer.total_bytes.or(self.request.expected_bytes),
                    resumed_from_bytes: transfer.resumed_from_bytes,
                })?;
                next_progress = transfer
                    .downloaded_bytes
                    .saturating_add(self.request.progress_interval_bytes);
            }
        }
        self.finish_transfer(transfer, path).await
    }

    async fn finish_transfer(
        &mut self,
        mut transfer: TransferState,
        path: &Path,
    ) -> Result<GgufDownloadResult, DownloadError> {
        if let Some(total) = transfer.total_bytes
            && transfer.downloaded_bytes != total
        {
            return Err(DownloadError::DownloadSizeMismatch {
                expected: total,
                actual: transfer.downloaded_bytes,
            });
        }
        let completed = CompletedDownload {
            bytes: transfer.downloaded_bytes,
            digest: finalize_digest(&transfer.hasher),
            disposition: transfer.disposition,
        };
        validate_download(self.request, completed.bytes, completed.digest)?;
        verify_gguf_magic_open(&mut transfer.file, path).await?;
        self.install(path, &mut transfer.file, completed).await
    }

    async fn install(
        &mut self,
        partial_path: &Path,
        partial: &mut tokio::fs::File,
        completed: CompletedDownload,
    ) -> Result<GgufDownloadResult, DownloadError> {
        self.emit(DownloadProgress {
            phase: DownloadPhase::Verifying,
            downloaded_bytes: completed.bytes,
            total_bytes: Some(completed.bytes),
            resumed_from_bytes: disposition_resume(completed.disposition),
        })?;
        partial
            .sync_all()
            .await
            .map_err(|source| io_error("synchronize partial file", partial_path, source))?;
        let (disk_hasher, disk_bytes) =
            hash_open_file(partial, partial_path, self.cancellation).await?;
        let disk_digest = finalize_digest(&disk_hasher);
        validate_download(self.request, disk_bytes, disk_digest)?;
        if disk_bytes != completed.bytes || disk_digest != completed.digest {
            return Err(DownloadError::DownloadHashMismatch);
        }
        verify_gguf_magic_open(partial, partial_path).await?;
        ensure_not_cancelled(self.cancellation)?;
        validate_open_path(partial, partial_path, "partial").await?;
        self.emit(DownloadProgress {
            phase: DownloadPhase::Installing,
            downloaded_bytes: completed.bytes,
            total_bytes: Some(completed.bytes),
            resumed_from_bytes: disposition_resume(completed.disposition),
        })?;
        let disposition = match std::fs::hard_link(partial_path, &self.request.target_path) {
            Ok(()) => {
                // The no-clobber link operates on a path, not the locked file
                // handle. Re-read the installed target before acknowledging it
                // so a same-size path replacement cannot inherit the verified
                // digest from another inode.
                self.validate_install_race(completed).await?;
                completed.disposition
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.validate_install_race(completed).await?;
                DownloadDisposition::ReusedExisting
            }
            Err(source) => {
                return Err(io_error(
                    "atomically install completed model",
                    &self.request.target_path,
                    source,
                ));
            }
        };
        let partial_removed = std::fs::remove_file(partial_path).is_ok();
        self.emit_complete(completed.bytes, disposition_resume(disposition));
        Ok(GgufDownloadResult {
            target_path: self.request.target_path.clone(),
            bytes: completed.bytes,
            sha256: completed.digest,
            disposition,
            partial_removed,
        })
    }

    async fn validate_install_race(
        &self,
        completed: CompletedDownload,
    ) -> Result<(), DownloadError> {
        let (target_bytes, target_digest) = hash_regular_path(
            &self.request.target_path,
            "target",
            self.request.max_bytes,
            self.cancellation,
        )
        .await?;
        if target_bytes != completed.bytes || target_digest != completed.digest {
            return Err(DownloadError::TargetRaceMismatch);
        }
        Ok(())
    }

    fn emit(&mut self, event: DownloadProgress) -> Result<(), DownloadError> {
        ensure_not_cancelled(self.cancellation)?;
        if (self.progress)(event) == DownloadControl::Cancel {
            return Err(DownloadError::Cancelled);
        }
        ensure_not_cancelled(self.cancellation)
    }

    fn emit_complete(&mut self, bytes: u64, resumed_from_bytes: u64) {
        // Completion wins the narrow race after the no-clobber install commit.
        // A late cancellation must not report failure after the target exists.
        let _ = (self.progress)(DownloadProgress {
            phase: DownloadPhase::Complete,
            downloaded_bytes: bytes,
            total_bytes: Some(bytes),
            resumed_from_bytes,
        });
    }
}

const fn disposition_resume(disposition: DownloadDisposition) -> u64 {
    match disposition {
        DownloadDisposition::DownloadedResumed { resumed_from_bytes } => resumed_from_bytes,
        _ => 0,
    }
}

fn validate_request(request: &GgufDownloadRequest) -> Result<ValidatedRequest, DownloadError> {
    let url = reqwest::Url::parse(&request.url).map_err(|_| DownloadError::InvalidUrl)?;
    if url.scheme() != "https" {
        return Err(DownloadError::HttpsRequired);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(DownloadError::CredentialsInUrl);
    }
    if !url.path().to_ascii_lowercase().ends_with(".gguf") {
        return Err(DownloadError::SourceNotGguf);
    }
    if !is_gguf_path(&request.target_path) {
        return Err(DownloadError::TargetNotGguf);
    }
    let parent = request
        .target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(DownloadError::MissingTargetParent)?;
    let parent_metadata =
        std::fs::metadata(parent).map_err(|_| DownloadError::MissingTargetParent)?;
    if !parent_metadata.is_dir() {
        return Err(DownloadError::MissingTargetParent);
    }
    if request.max_bytes == 0 || request.max_bytes > MAX_MODEL_DOWNLOAD_BYTES {
        return Err(DownloadError::InvalidMaximumBytes);
    }
    if request
        .expected_bytes
        .is_some_and(|bytes| bytes == 0 || bytes > request.max_bytes)
    {
        return Err(DownloadError::InvalidExpectedBytes);
    }
    if !(MIN_PROGRESS_INTERVAL_BYTES..=MAX_PROGRESS_INTERVAL_BYTES)
        .contains(&request.progress_interval_bytes)
    {
        return Err(DownloadError::InvalidProgressInterval);
    }
    let file_name = request
        .target_path
        .file_name()
        .ok_or(DownloadError::TargetNotGguf)?;
    let mut partial_name = OsString::from(file_name);
    partial_name.push(".part");
    Ok(ValidatedRequest {
        url,
        partial_path: request.target_path.with_file_name(partial_name),
    })
}

fn build_client() -> Result<reqwest::Client, DownloadError> {
    let redirects = redirect::Policy::custom(|attempt| {
        if attempt.url().scheme() != "https"
            || !attempt.url().username().is_empty()
            || attempt.url().password().is_some()
            || attempt.previous().len() > MAX_REDIRECTS
        {
            attempt.error("unsafe or excessive model download redirect")
        } else {
            attempt.follow()
        }
    });
    reqwest::Client::builder()
        .redirect(redirects)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .map_err(|_| DownloadError::ClientConfiguration)
}

fn map_request_error(error: &reqwest::Error) -> DownloadError {
    if error.is_redirect() {
        DownloadError::RedirectRejected
    } else if error.is_timeout() {
        DownloadError::NetworkTimeout
    } else if error.is_connect() {
        DownloadError::NetworkConnect
    } else if error.is_body() {
        DownloadError::NetworkBody
    } else {
        DownloadError::NetworkRequest
    }
}

fn plan_response(
    status: StatusCode,
    headers: &HeaderMap,
    resume_bytes: u64,
    request: &GgufDownloadRequest,
) -> Result<ResponsePlan, DownloadError> {
    let content_length = parse_content_length(headers)?;
    if resume_bytes == 0 {
        if status == StatusCode::OK {
            if headers.contains_key(CONTENT_RANGE) {
                return Err(DownloadError::InvalidContentRange);
            }
            preflight_length(request, 0, content_length)?;
            return Ok(ResponsePlan::Continue {
                total: content_length,
                restart: false,
            });
        }
        if status == StatusCode::PARTIAL_CONTENT {
            let ParsedContentRange::Satisfied { start, end, total } =
                parse_required_range(headers)?
            else {
                return Err(DownloadError::InvalidContentRange);
            };
            if start != 0 {
                return Err(DownloadError::RangeStartMismatch {
                    expected: 0,
                    actual: start,
                });
            }
            validate_range_length(start, end, content_length)?;
            validate_reported_total(request, total)?;
            preflight_length(request, 0, content_length)?;
            return Ok(ResponsePlan::Continue {
                total,
                restart: false,
            });
        }
        return Err(DownloadError::HttpStatus {
            status: status.as_u16(),
        });
    }

    if status == StatusCode::OK {
        if headers.contains_key(CONTENT_RANGE) {
            return Err(DownloadError::InvalidContentRange);
        }
        preflight_length(request, 0, content_length)?;
        return Ok(ResponsePlan::Continue {
            total: content_length,
            restart: true,
        });
    }
    if status == StatusCode::PARTIAL_CONTENT {
        let ParsedContentRange::Satisfied { start, end, total } = parse_required_range(headers)?
        else {
            return Err(DownloadError::InvalidContentRange);
        };
        if start != resume_bytes {
            return Err(DownloadError::RangeStartMismatch {
                expected: resume_bytes,
                actual: start,
            });
        }
        validate_range_length(start, end, content_length)?;
        validate_reported_total(request, total)?;
        preflight_length(request, resume_bytes, content_length)?;
        return Ok(ResponsePlan::Continue {
            total,
            restart: false,
        });
    }
    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        let ParsedContentRange::Unsatisfied { total } = parse_required_range(headers)? else {
            return Err(DownloadError::InvalidContentRange);
        };
        if total == Some(resume_bytes) {
            validate_reported_total(request, total)?;
            return Ok(ResponsePlan::PartialAlreadyComplete);
        }
        return Err(DownloadError::UnsatisfiedRange);
    }
    Err(DownloadError::HttpStatus {
        status: status.as_u16(),
    })
}

fn parse_required_range(headers: &HeaderMap) -> Result<ParsedContentRange, DownloadError> {
    let value = headers
        .get(CONTENT_RANGE)
        .ok_or(DownloadError::InvalidContentRange)?
        .to_str()
        .map_err(|_| DownloadError::InvalidContentRange)?;
    parse_content_range(value).ok_or(DownloadError::InvalidContentRange)
}

fn parse_content_range(value: &str) -> Option<ParsedContentRange> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    if range == "*" {
        return Some(ParsedContentRange::Unsatisfied { total });
    }
    let (start, end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    if start > end || total.is_some_and(|total| end >= total) {
        return None;
    }
    Some(ParsedContentRange::Satisfied { start, end, total })
}

fn parse_content_length(headers: &HeaderMap) -> Result<Option<u64>, DownloadError> {
    headers
        .get(CONTENT_LENGTH)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse().ok())
                .ok_or(DownloadError::InvalidContentLength)
        })
        .transpose()
}

fn reject_encoded_response(headers: &HeaderMap) -> Result<(), DownloadError> {
    if headers.get(CONTENT_ENCODING).is_some_and(|value| {
        value
            .to_str()
            .map_or(true, |value| !value.eq_ignore_ascii_case("identity"))
    }) {
        return Err(DownloadError::EncodedResponse);
    }
    Ok(())
}

fn validate_range_length(
    start: u64,
    end: u64,
    content_length: Option<u64>,
) -> Result<(), DownloadError> {
    let range_length = end
        .checked_sub(start)
        .and_then(|length| length.checked_add(1))
        .ok_or(DownloadError::InconsistentRange)?;
    if content_length.is_some_and(|length| length != range_length) {
        return Err(DownloadError::InconsistentRange);
    }
    Ok(())
}

fn validate_reported_total(
    request: &GgufDownloadRequest,
    total: Option<u64>,
) -> Result<(), DownloadError> {
    if let Some(total) = total {
        if total > request.max_bytes {
            return Err(DownloadError::DownloadTooLarge {
                limit: request.max_bytes,
            });
        }
        if let Some(expected) = request.expected_bytes
            && total != expected
        {
            return Err(DownloadError::RemoteSizeMismatch {
                expected,
                reported: total,
            });
        }
    }
    Ok(())
}

fn preflight_length(
    request: &GgufDownloadRequest,
    current: u64,
    remaining: Option<u64>,
) -> Result<(), DownloadError> {
    let Some(remaining) = remaining else {
        return Ok(());
    };
    let total = current
        .checked_add(remaining)
        .ok_or(DownloadError::DownloadTooLarge {
            limit: request.max_bytes,
        })?;
    enforce_transfer_bound(request, total)
}

fn enforce_transfer_bound(request: &GgufDownloadRequest, bytes: u64) -> Result<(), DownloadError> {
    let limit = request.expected_bytes.unwrap_or(request.max_bytes);
    if bytes > limit {
        return Err(DownloadError::DownloadTooLarge { limit });
    }
    Ok(())
}

fn validate_partial_size(request: &GgufDownloadRequest, bytes: u64) -> Result<(), DownloadError> {
    let limit = request.expected_bytes.unwrap_or(request.max_bytes);
    if bytes > limit {
        return Err(DownloadError::PartialTooLarge { bytes, limit });
    }
    Ok(())
}

fn validate_existing(
    request: &GgufDownloadRequest,
    bytes: u64,
    digest: Sha256Digest,
) -> Result<(), DownloadError> {
    if let Some(expected) = request.expected_bytes
        && bytes != expected
    {
        return Err(DownloadError::ExistingSizeMismatch {
            expected,
            actual: bytes,
        });
    }
    if digest != request.expected_sha256 {
        return Err(DownloadError::ExistingHashMismatch);
    }
    Ok(())
}

fn validate_download(
    request: &GgufDownloadRequest,
    bytes: u64,
    digest: Sha256Digest,
) -> Result<(), DownloadError> {
    if let Some(expected) = request.expected_bytes
        && bytes != expected
    {
        return Err(DownloadError::DownloadSizeMismatch {
            expected,
            actual: bytes,
        });
    }
    if digest != request.expected_sha256 {
        return Err(DownloadError::DownloadHashMismatch);
    }
    Ok(())
}

fn path_exists(path: &Path, role: &'static str) -> Result<bool, DownloadError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_regular_metadata(path, role, &metadata)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("inspect path", path, source)),
    }
}

fn validate_regular_metadata(
    path: &Path,
    role: &'static str,
    metadata: &std::fs::Metadata,
) -> Result<(), DownloadError> {
    if metadata.file_type().is_symlink() {
        return Err(DownloadError::SymlinkPath {
            role,
            path: path.to_path_buf(),
        });
    }
    if !metadata.is_file() {
        return Err(DownloadError::NonRegularPath {
            role,
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn open_locked_partial(path: &Path) -> Result<File, DownloadError> {
    let file = match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_regular_metadata(path, "partial", &metadata)?;
            open_existing_partial(path)?
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let metadata = std::fs::symlink_metadata(path)
                        .map_err(|source| io_error("reinspect partial file", path, source))?;
                    validate_regular_metadata(path, "partial", &metadata)?;
                    open_existing_partial(path)?
                }
                Err(source) => return Err(io_error("create partial file", path, source)),
            }
        }
        Err(source) => return Err(io_error("inspect partial file", path, source)),
    };
    match <File as fs4::FileExt>::try_lock(&file) {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(DownloadError::PartialFileBusy),
        Err(TryLockError::Error(source)) => {
            return Err(io_error("lock partial file", path, source));
        }
    }
    let file_metadata = file
        .metadata()
        .map_err(|source| io_error("inspect partial file", path, source))?;
    if !file_metadata.is_file() {
        return Err(DownloadError::NonRegularPath {
            role: "partial",
            path: path.to_path_buf(),
        });
    }
    let path_metadata = std::fs::symlink_metadata(path)
        .map_err(|source| io_error("reinspect partial file", path, source))?;
    validate_regular_metadata(path, "partial", &path_metadata)?;
    if path_metadata.len() != file_metadata.len() {
        return Err(DownloadError::PartialFileChanged);
    }
    Ok(file)
}

fn open_existing_partial(path: &Path) -> Result<File, DownloadError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|source| io_error("open partial file", path, source))
}

async fn validate_open_path(
    file: &tokio::fs::File,
    path: &Path,
    role: &'static str,
) -> Result<(), DownloadError> {
    let file_metadata = file
        .metadata()
        .await
        .map_err(|source| io_error("inspect open file", path, source))?;
    if !file_metadata.is_file() {
        return Err(DownloadError::NonRegularPath {
            role,
            path: path.to_path_buf(),
        });
    }
    let path_metadata = std::fs::symlink_metadata(path)
        .map_err(|source| io_error("reinspect file path", path, source))?;
    validate_regular_metadata(path, role, &path_metadata)?;
    if file_metadata.len() != path_metadata.len() {
        return Err(DownloadError::PartialFileChanged);
    }
    Ok(())
}

async fn hash_regular_path(
    path: &Path,
    role: &'static str,
    max_bytes: u64,
    cancellation: &DownloadCancellation,
) -> Result<(u64, Sha256Digest), DownloadError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| io_error("inspect file", path, source))?;
    validate_regular_metadata(path, role, &metadata)?;
    if metadata.len() > max_bytes {
        return Err(DownloadError::DownloadTooLarge { limit: max_bytes });
    }
    let file = File::open(path).map_err(|source| io_error("open file", path, source))?;
    let mut file = tokio::fs::File::from_std(file);
    let (hasher, bytes) = hash_open_file(&mut file, path, cancellation).await?;
    Ok((bytes, finalize_digest(&hasher)))
}

async fn hash_open_file(
    file: &mut tokio::fs::File,
    path: &Path,
    cancellation: &DownloadCancellation,
) -> Result<(Sha256, u64), DownloadError> {
    file.seek(SeekFrom::Start(0))
        .await
        .map_err(|source| io_error("seek file", path, source))?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    loop {
        let read_result = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(DownloadError::Cancelled),
            result = file.read(&mut buffer) => result,
        };
        let read = read_result.map_err(|source| io_error("read file", path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes
            .checked_add(
                u64::try_from(read).map_err(|_| DownloadError::DownloadTooLarge {
                    limit: MAX_MODEL_DOWNLOAD_BYTES,
                })?,
            )
            .ok_or(DownloadError::DownloadTooLarge {
                limit: MAX_MODEL_DOWNLOAD_BYTES,
            })?;
    }
    Ok((hasher, bytes))
}

fn finalize_digest(hasher: &Sha256) -> Sha256Digest {
    Sha256Digest(hasher.clone().finalize().into())
}

async fn verify_gguf_magic_path(path: &Path, role: &'static str) -> Result<(), DownloadError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| io_error("inspect file", path, source))?;
    validate_regular_metadata(path, role, &metadata)?;
    let file = File::open(path).map_err(|source| io_error("open file", path, source))?;
    let mut file = tokio::fs::File::from_std(file);
    verify_gguf_magic_open(&mut file, path).await
}

async fn verify_gguf_magic_open(
    file: &mut tokio::fs::File,
    path: &Path,
) -> Result<(), DownloadError> {
    file.seek(SeekFrom::Start(0))
        .await
        .map_err(|source| io_error("seek file", path, source))?;
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic)
        .await
        .map_err(|source| io_error("read GGUF header", path, source))?;
    if magic != GGUF_MAGIC {
        return Err(DownloadError::InvalidGgufMagic);
    }
    Ok(())
}

fn ensure_not_cancelled(cancellation: &DownloadCancellation) -> Result<(), DownloadError> {
    if cancellation.is_cancelled() {
        Err(DownloadError::Cancelled)
    } else {
        Ok(())
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> DownloadError {
    DownloadError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn model_bytes() -> Vec<u8> {
        b"GGUFdeterministic-test-model".to_vec()
    }

    fn digest(bytes: &[u8]) -> Sha256Digest {
        Sha256Digest(Sha256::digest(bytes).into())
    }

    fn request(target: PathBuf, bytes: &[u8]) -> GgufDownloadRequest {
        let mut request = GgufDownloadRequest::new(
            "https://models.example.test/model.gguf",
            target,
            digest(bytes),
            1024 * 1024,
        );
        request.expected_bytes = Some(u64::try_from(bytes.len()).unwrap());
        request
    }

    #[test]
    fn sha256_digest_requires_canonical_width() {
        let value = "ab".repeat(32);
        let parsed = Sha256Digest::from_hex(&value).unwrap();
        assert_eq!(parsed.to_string(), value);
        let encoded = serde_json::to_string(&parsed).unwrap();
        assert_eq!(encoded, format!("\"{value}\""));
        assert_eq!(
            serde_json::from_str::<Sha256Digest>(&encoded).unwrap(),
            parsed
        );
        assert!(matches!(
            Sha256Digest::from_hex("ab"),
            Err(DownloadError::InvalidSha256)
        ));
        assert!(matches!(
            Sha256Digest::from_hex(&"zz".repeat(32)),
            Err(DownloadError::InvalidSha256)
        ));
    }

    #[test]
    fn retryability_is_narrow_and_explicit() {
        assert!(DownloadError::NetworkTimeout.is_retryable());
        assert!(DownloadError::PartialFileBusy.is_retryable());
        assert!(DownloadError::HttpStatus { status: 429 }.is_retryable());
        assert!(DownloadError::HttpStatus { status: 503 }.is_retryable());
        assert!(!DownloadError::HttpStatus { status: 404 }.is_retryable());
        assert!(!DownloadError::DownloadHashMismatch.is_retryable());
        assert!(DownloadError::Cancelled.is_cancelled());
        assert!(!DownloadError::NetworkTimeout.is_cancelled());
    }

    #[test]
    fn validates_https_gguf_paths_and_bounds() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = model_bytes();
        let mut value = request(directory.path().join("model.gguf"), &bytes);
        assert!(validate_request(&value).is_ok());

        value.url = "http://models.example.test/model.gguf".to_string();
        assert!(matches!(
            validate_request(&value),
            Err(DownloadError::HttpsRequired)
        ));
        value.url = "https://models.example.test/model.bin".to_string();
        assert!(matches!(
            validate_request(&value),
            Err(DownloadError::SourceNotGguf)
        ));
        value.url = "https://models.example.test/model.gguf".to_string();
        value.target_path = directory.path().join("model.bin");
        assert!(matches!(
            validate_request(&value),
            Err(DownloadError::TargetNotGguf)
        ));
        value.target_path = directory.path().join("model.gguf");
        value.max_bytes = 0;
        assert!(matches!(
            validate_request(&value),
            Err(DownloadError::InvalidMaximumBytes)
        ));

        value.max_bytes = 1024 * 1024;
        value.target_path = directory.path().join("model.gguf");
        value.url = "https://secret@models.example.test/model.gguf".to_string();
        assert!(matches!(
            validate_request(&value),
            Err(DownloadError::CredentialsInUrl)
        ));
        assert!(!format!("{value:?}").contains("secret"));
    }

    #[test]
    fn parses_and_rejects_content_ranges_deterministically() {
        assert_eq!(
            parse_content_range("bytes 10-19/20"),
            Some(ParsedContentRange::Satisfied {
                start: 10,
                end: 19,
                total: Some(20),
            })
        );
        assert_eq!(
            parse_content_range("bytes */20"),
            Some(ParsedContentRange::Unsatisfied { total: Some(20) })
        );
        assert_eq!(parse_content_range("bytes 20-19/20"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
    }

    #[test]
    fn resume_response_must_start_at_exact_partial_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = model_bytes();
        let value = request(directory.path().join("model.gguf"), &bytes);
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_RANGE, "bytes 4-27/28".parse().unwrap());
        headers.insert(CONTENT_LENGTH, "24".parse().unwrap());
        assert_eq!(
            plan_response(StatusCode::PARTIAL_CONTENT, &headers, 4, &value).unwrap(),
            ResponsePlan::Continue {
                total: Some(28),
                restart: false,
            }
        );
        assert!(matches!(
            plan_response(StatusCode::PARTIAL_CONTENT, &headers, 3, &value),
            Err(DownloadError::RangeStartMismatch {
                expected: 3,
                actual: 4
            })
        ));
    }

    #[test]
    fn ignored_range_selects_safe_restart() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = model_bytes();
        let value = request(directory.path().join("model.gguf"), &bytes);
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_LENGTH, "28".parse().unwrap());
        assert_eq!(
            plan_response(StatusCode::OK, &headers, 4, &value).unwrap(),
            ResponsePlan::Continue {
                total: Some(28),
                restart: true,
            }
        );
    }

    #[tokio::test]
    async fn matching_existing_target_is_reused_without_network() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("model.gguf");
        let bytes = model_bytes();
        std::fs::write(&target, &bytes).unwrap();
        let result = download_gguf(
            &request(target.clone(), &bytes),
            &DownloadCancellation::default(),
            |_| DownloadControl::Continue,
        )
        .await
        .unwrap();
        assert_eq!(result.disposition, DownloadDisposition::ReusedExisting);
        assert_eq!(result.target_path, target);
        assert_eq!(result.bytes, 28);
    }

    #[tokio::test]
    async fn complete_partial_installs_without_network_and_without_clobber() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("model.gguf");
        let partial = directory.path().join("model.gguf.part");
        let bytes = model_bytes();
        std::fs::write(&partial, &bytes).unwrap();
        let result = download_gguf(
            &request(target.clone(), &bytes),
            &DownloadCancellation::default(),
            |_| DownloadControl::Continue,
        )
        .await
        .unwrap();
        assert_eq!(
            result.disposition,
            DownloadDisposition::DownloadedResumed {
                resumed_from_bytes: 28
            }
        );
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert!(!partial.exists());
        assert!(result.partial_removed);
    }

    #[tokio::test]
    async fn cancellation_preserves_complete_partial() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("model.gguf");
        let partial = directory.path().join("model.gguf.part");
        let bytes = model_bytes();
        std::fs::write(&partial, &bytes).unwrap();
        let cancellation = DownloadCancellation::default();
        cancellation.cancel();
        let result = download_gguf(&request(target, &bytes), &cancellation, |_| {
            DownloadControl::Continue
        })
        .await;
        assert!(matches!(result, Err(DownloadError::Cancelled)));
        assert_eq!(std::fs::read(partial).unwrap(), bytes);
    }

    #[tokio::test]
    async fn hash_failure_preserves_complete_partial() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("model.gguf");
        let partial = directory.path().join("model.gguf.part");
        let bytes = model_bytes();
        std::fs::write(&partial, &bytes).unwrap();
        let mut value = request(target.clone(), &bytes);
        value.expected_sha256 = digest(b"different bytes");
        let result = download_gguf(&value, &DownloadCancellation::default(), |_| {
            DownloadControl::Continue
        })
        .await;
        assert!(matches!(result, Err(DownloadError::DownloadHashMismatch)));
        assert_eq!(std::fs::read(partial).unwrap(), bytes);
        assert!(!target.exists());
    }

    #[test]
    fn partial_lock_contention_is_typed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model.gguf.part");
        let first = open_locked_partial(&path).unwrap();
        assert!(matches!(
            open_locked_partial(&path),
            Err(DownloadError::PartialFileBusy)
        ));
        drop(first);
        assert!(open_locked_partial(&path).is_ok());
    }

    #[test]
    fn install_primitive_never_clobbers_existing_target() {
        let directory = tempfile::tempdir().unwrap();
        let partial = directory.path().join("model.gguf.part");
        let target = directory.path().join("model.gguf");
        File::create(&partial).unwrap().write_all(b"new").unwrap();
        std::fs::write(&target, b"old").unwrap();
        let error = std::fs::hard_link(&partial, &target).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(target).unwrap(), b"old");
    }

    #[tokio::test]
    async fn installed_target_is_cold_verified_even_when_size_matches() {
        let directory = tempfile::tempdir().unwrap();
        let expected = model_bytes();
        let target = directory.path().join("model.gguf");
        let mut replacement = expected.clone();
        replacement[4] ^= 0xff;
        std::fs::write(&target, replacement).unwrap();
        let request = request(target, &expected);
        let session = DownloadSession {
            request: &request,
            cancellation: &DownloadCancellation::default(),
            progress: |_| DownloadControl::Continue,
        };
        let result = session
            .validate_install_race(CompletedDownload {
                bytes: u64::try_from(expected.len()).unwrap(),
                digest: digest(&expected),
                disposition: DownloadDisposition::DownloadedFresh,
            })
            .await;
        assert!(matches!(result, Err(DownloadError::TargetRaceMismatch)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symbolic_link_target_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let bytes = model_bytes();
        let real = directory.path().join("real.gguf");
        let target = directory.path().join("model.gguf");
        std::fs::write(&real, &bytes).unwrap();
        symlink(&real, &target).unwrap();
        let result = download_gguf(
            &request(target, &bytes),
            &DownloadCancellation::default(),
            |_| DownloadControl::Continue,
        )
        .await;
        assert!(matches!(result, Err(DownloadError::SymlinkPath { .. })));
    }
}
