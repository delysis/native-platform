#![forbid(unsafe_code)]

//! The sole network-authority boundary for information-native-kit.
//!
//! Acquisition is deny-by-default. Network requests may only reach globally
//! routable addresses unless a caller explicitly grants a broader scope, and a
//! `file:` URI is rejected unless its canonical path is beneath a caller-granted
//! canonical root. Artifact bytes are streamed into private staging files and
//! become usable only after their declared length and SHA-256 both match.

use information_native_types::PlannedArtifact;
use reqwest::header::{
    ACCEPT_ENCODING, ACCEPT_RANGES, CONTENT_ENCODING, CONTENT_RANGE, ETAG, HeaderValue, IF_RANGE,
    LAST_MODIFIED, LOCATION, RANGE,
};
use reqwest::redirect::Policy;
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use url::{Host, Url};

#[cfg(unix)]
use fs2::FileExt;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

const STREAM_BUFFER_BYTES: usize = 64 * 1024;
const DEFAULT_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESOLVED_ADDRESSES: usize = 32;
const MAX_ATTESTED_REDIRECTS: usize = 32;
const MAX_RESUME_SIDECAR_BYTES: u64 = 64 * 1024;
const RESUME_SIDECAR_VERSION: u32 = 2;
const MAX_SOURCE_ATTESTATIONS: usize = 128;
const MAX_SIDECAR_TEMP_ATTEMPTS: usize = 128;
const MAX_SIDECAR_DIRECTORY_ENTRIES: usize = 4_096;

static NEXT_SIDECAR_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// Backwards-compatible client configuration.
///
/// `request_timeout` is the hard total budget for one catalogue or artifact
/// transfer, including all redirect hops. Use [`AcquireClient::new_with_timeouts`]
/// to choose a distinct read-idle timeout.
#[derive(Debug, Clone)]
pub struct AcquireConfig {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_redirects: usize,
    pub user_agent: String,
}

impl Default for AcquireConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            request_timeout: Duration::from_secs(30 * 60),
            max_redirects: 5,
            user_agent: format!("information-native-acquire/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Independent read-idle and total-transfer bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferTimeouts {
    /// Maximum time without receiving another response-body frame.
    pub read_idle_timeout: Duration,
    /// Maximum elapsed time for all redirects and response bytes.
    pub total_transfer_timeout: Duration,
}

impl TransferTimeouts {
    #[must_use]
    pub const fn new(read_idle_timeout: Duration, total_transfer_timeout: Duration) -> Self {
        Self {
            read_idle_timeout,
            total_transfer_timeout,
        }
    }
}

/// The network address space an acquisition may reach.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NetworkScope {
    /// Permit only addresses classified as globally routable by this crate.
    #[default]
    PublicInternetOnly,
    /// Permit any IP address, including loopback and private networks.
    ///
    /// This is an explicit capability for controlled local services and tests.
    AnyAddress,
}

/// A directory capability canonicalized at grant time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalFileRoot {
    path: PathBuf,
}

impl CanonicalFileRoot {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, AcquireError> {
        let path = fs::canonicalize(path.as_ref()).map_err(file_policy_io)?;
        let metadata = fs::metadata(&path).map_err(file_policy_io)?;
        if !metadata.is_dir() {
            return Err(AcquireError::FileRootNotDirectory(path));
        }
        Ok(Self { path })
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }
}

/// Explicit authority granted to URI acquisition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcquisitionPolicy {
    network_scope: NetworkScope,
    file_roots: Vec<CanonicalFileRoot>,
}

impl AcquisitionPolicy {
    #[must_use]
    pub const fn restricted() -> Self {
        Self {
            network_scope: NetworkScope::PublicInternetOnly,
            file_roots: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_network_scope(mut self, network_scope: NetworkScope) -> Self {
        self.network_scope = network_scope;
        self
    }

    #[must_use]
    pub fn with_file_root(mut self, root: CanonicalFileRoot) -> Self {
        if !self.file_roots.contains(&root) {
            self.file_roots.push(root);
        }
        self
    }

    pub fn grant_file_root(&mut self, path: impl AsRef<Path>) -> Result<&mut Self, AcquireError> {
        let root = CanonicalFileRoot::new(path)?;
        if !self.file_roots.contains(&root) {
            self.file_roots.push(root);
        }
        Ok(self)
    }

    fn authorize_file_uri(&self, path: &Path) -> Result<PathBuf, AcquireError> {
        if self.file_roots.is_empty() {
            return Err(AcquireError::FileUriForbidden);
        }
        let canonical = fs::canonicalize(path).map_err(file_policy_io)?;
        if self
            .file_roots
            .iter()
            .any(|root| canonical.starts_with(root.as_path()))
        {
            Ok(canonical)
        } else {
            Err(AcquireError::FileOutsideGrantedRoots(canonical))
        }
    }

    fn open_file_uri(&self, path: &Path) -> Result<File, AcquireError> {
        let canonical = self.authorize_file_uri(path)?;
        let metadata = fs::symlink_metadata(&canonical).map_err(file_policy_io)?;
        if !metadata.file_type().is_file() {
            return Err(AcquireError::InvalidFileUri);
        }
        let file = File::open(&canonical).map_err(source_io)?;
        if !open_file_matches_path(&file, &canonical)? {
            return Err(AcquireError::FileSourceIdentityChanged);
        }
        let recanonical = fs::canonicalize(&canonical).map_err(file_policy_io)?;
        if recanonical != canonical
            || !self
                .file_roots
                .iter()
                .any(|root| recanonical.starts_with(root.as_path()))
        {
            return Err(AcquireError::FileSourceIdentityChanged);
        }
        Ok(file)
    }

    fn permits_address(&self, address: IpAddr) -> bool {
        self.network_scope == NetworkScope::AnyAddress || is_publicly_routable(address)
    }
}

/// Durable retry behavior for an artifact staging path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ResumePolicy {
    #[default]
    Disabled,
    /// Preserve a safe partial artifact and bind it to this durable sidecar.
    /// This mode is currently available only on Unix, where safe standard
    /// library file identity is sufficient for replacement-aware cleanup.
    Durable { sidecar_path: PathBuf },
}

impl ResumePolicy {
    #[must_use]
    pub fn durable(sidecar_path: impl Into<PathBuf>) -> Self {
        Self::Durable {
            sidecar_path: sidecar_path.into(),
        }
    }

    fn sidecar_path(&self) -> Option<&Path> {
        match self {
            Self::Disabled => None,
            Self::Durable { sidecar_path } => Some(sidecar_path),
        }
    }
}

fn ensure_durable_resume_supported(resume: &ResumePolicy) -> Result<(), AcquireError> {
    #[cfg(not(unix))]
    if matches!(resume, ResumePolicy::Durable { .. }) {
        return Err(AcquireError::DurableResumeUnsupportedOnPlatform);
    }
    #[cfg(unix)]
    let _resume = resume;
    Ok(())
}

/// Policy and retry controls for artifact acquisition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactFetchOptions {
    pub acquisition_policy: AcquisitionPolicy,
    pub resume: ResumePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferPhase {
    Starting,
    Resuming,
    Downloading,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferProgress {
    pub phase: TransferPhase,
    pub downloaded_bytes: u64,
    pub expected_bytes: u64,
    pub resumed_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressControl {
    Continue,
    Cancel,
}

/// A synchronous callback invoked between received chunks.
///
/// Returning [`ProgressControl::Cancel`] prevents validation or publication.
/// Cancellation of a pending DNS lookup, connect, or read occurs at the next
/// configured timeout boundary.
pub trait ProgressCallback {
    fn on_progress(&mut self, progress: TransferProgress) -> ProgressControl;
}

impl<F> ProgressCallback for F
where
    F: FnMut(TransferProgress) -> ProgressControl,
{
    fn on_progress(&mut self, progress: TransferProgress) -> ProgressControl {
        self(progress)
    }
}

#[derive(Debug)]
struct IgnoreProgress;

impl ProgressCallback for IgnoreProgress {
    fn on_progress(&mut self, _progress: TransferProgress) -> ProgressControl {
        ProgressControl::Continue
    }
}

#[derive(Debug)]
pub struct AcquireClient {
    config: AcquireConfig,
    timeouts: TransferTimeouts,
}

impl AcquireClient {
    pub fn new(config: AcquireConfig) -> Result<Self, AcquireError> {
        let read_idle_timeout = DEFAULT_READ_IDLE_TIMEOUT.min(config.request_timeout);
        let total_transfer_timeout = config.request_timeout;
        Self::new_with_timeouts(
            config,
            TransferTimeouts::new(read_idle_timeout, total_transfer_timeout),
        )
    }

    pub fn new_with_timeouts(
        config: AcquireConfig,
        timeouts: TransferTimeouts,
    ) -> Result<Self, AcquireError> {
        if config.connect_timeout.is_zero()
            || config.request_timeout.is_zero()
            || timeouts.read_idle_timeout.is_zero()
            || timeouts.total_transfer_timeout.is_zero()
        {
            return Err(AcquireError::InvalidConfig(
                "connect, read-idle, legacy request, and total timeouts must be greater than zero"
                    .to_string(),
            ));
        }
        if config.user_agent.trim().is_empty() {
            return Err(AcquireError::InvalidConfig(
                "user agent cannot be empty".to_string(),
            ));
        }
        if config.max_redirects > MAX_ATTESTED_REDIRECTS {
            return Err(AcquireError::InvalidConfig(format!(
                "redirect limit cannot exceed {MAX_ATTESTED_REDIRECTS}"
            )));
        }
        Ok(Self { config, timeouts })
    }

    pub fn with_defaults() -> Result<Self, AcquireError> {
        Self::new(AcquireConfig::default())
    }

    /// Fetch exactly the artifact described by a validated install plan.
    pub fn fetch_planned_artifact(
        &self,
        artifact: &PlannedArtifact,
        staging_path: &Path,
        max_bytes: u64,
    ) -> Result<VerifiedFetch, AcquireError> {
        let mut progress = IgnoreProgress;
        self.fetch_planned_artifact_with_options(
            artifact,
            staging_path,
            max_bytes,
            &ArtifactFetchOptions::default(),
            &mut progress,
        )
    }

    pub fn fetch_planned_artifact_with_options(
        &self,
        artifact: &PlannedArtifact,
        staging_path: &Path,
        max_bytes: u64,
        options: &ArtifactFetchOptions,
        progress: &mut dyn ProgressCallback,
    ) -> Result<VerifiedFetch, AcquireError> {
        self.fetch_artifact_with_options(
            &artifact.source_uri,
            staging_path,
            artifact.expected_bytes,
            &artifact.sha256,
            max_bytes,
            options,
            progress,
        )
    }

    /// Fetch an HTTP(S) URI under the restrictive default policy.
    ///
    /// `file:` remains parseable for compatibility, but is denied here because
    /// the default policy grants no canonical file roots.
    pub fn fetch_artifact(
        &self,
        source_uri: &str,
        staging_path: &Path,
        expected_bytes: u64,
        expected_sha256: &str,
        max_bytes: u64,
    ) -> Result<VerifiedFetch, AcquireError> {
        let mut progress = IgnoreProgress;
        self.fetch_artifact_with_options(
            source_uri,
            staging_path,
            expected_bytes,
            expected_sha256,
            max_bytes,
            &ArtifactFetchOptions::default(),
            &mut progress,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fetch_artifact_with_options(
        &self,
        source_uri: &str,
        staging_path: &Path,
        expected_bytes: u64,
        expected_sha256: &str,
        max_bytes: u64,
        options: &ArtifactFetchOptions,
        progress: &mut dyn ProgressCallback,
    ) -> Result<VerifiedFetch, AcquireError> {
        validate_expectation(expected_bytes, expected_sha256, max_bytes)?;
        match parse_source_uri(source_uri)? {
            FetchSource::Http(url) => self.fetch_http_artifact(
                source_uri,
                url,
                staging_path,
                expected_bytes,
                expected_sha256,
                max_bytes,
                options,
                progress,
            ),
            FetchSource::File { path, uri } => {
                if options.resume != ResumePolicy::Disabled {
                    return Err(AcquireError::ResumeUnsupportedForFile);
                }
                let source = options.acquisition_policy.open_file_uri(&path)?;
                let attestation = SourceAttestation::direct(uri);
                self.fetch_open_file_inner(
                    source,
                    staging_path,
                    expected_bytes,
                    expected_sha256,
                    max_bytes,
                    Some(attestation),
                    unix_time_millis()?,
                    progress,
                )
            }
        }
    }

    /// Explicit path variant for applications that already hold path authority.
    pub fn fetch_file_artifact(
        &self,
        source_path: &Path,
        staging_path: &Path,
        expected_bytes: u64,
        expected_sha256: &str,
        max_bytes: u64,
    ) -> Result<VerifiedFetch, AcquireError> {
        let mut progress = IgnoreProgress;
        self.fetch_file_artifact_with_progress(
            source_path,
            staging_path,
            expected_bytes,
            expected_sha256,
            max_bytes,
            &mut progress,
        )
    }

    pub fn fetch_file_artifact_with_progress(
        &self,
        source_path: &Path,
        staging_path: &Path,
        expected_bytes: u64,
        expected_sha256: &str,
        max_bytes: u64,
        progress: &mut dyn ProgressCallback,
    ) -> Result<VerifiedFetch, AcquireError> {
        validate_expectation(expected_bytes, expected_sha256, max_bytes)?;
        self.fetch_file_inner(
            source_path,
            staging_path,
            expected_bytes,
            expected_sha256,
            max_bytes,
            None,
            progress,
        )
    }

    /// Fetch untrusted catalogue bytes under the restrictive default policy.
    pub fn fetch_catalogue(
        &self,
        source_uri: &str,
        max_bytes: u64,
    ) -> Result<CatalogueBytes, AcquireError> {
        self.fetch_catalogue_with_policy(source_uri, max_bytes, &AcquisitionPolicy::default())
    }

    pub fn fetch_catalogue_with_policy(
        &self,
        source_uri: &str,
        max_bytes: u64,
        policy: &AcquisitionPolicy,
    ) -> Result<CatalogueBytes, AcquireError> {
        validate_nonzero_limit(max_bytes)?;
        match parse_source_uri(source_uri)? {
            FetchSource::Http(url) => {
                let deadline = transfer_deadline(self.timeouts.total_transfer_timeout)?;
                self.run_network(
                    self.fetch_catalogue_http(source_uri, url, max_bytes, policy, deadline),
                )
            }
            FetchSource::File { path, uri } => {
                let mut file = policy.open_file_uri(&path)?;
                if file
                    .metadata()
                    .ok()
                    .is_some_and(|metadata| metadata.len() > max_bytes)
                {
                    return Err(AcquireError::LimitExceeded { max_bytes });
                }
                Ok(CatalogueBytes {
                    bytes: read_bounded(&mut file, max_bytes)?,
                    network_used: false,
                    final_source_uri: Some(uri.clone()),
                    redirects: 0,
                    source_attestation: Some(SourceAttestation::direct(uri)),
                })
            }
        }
    }

    pub fn fetch_catalogue_file(
        &self,
        source_path: &Path,
        max_bytes: u64,
    ) -> Result<CatalogueBytes, AcquireError> {
        validate_nonzero_limit(max_bytes)?;
        let mut file = File::open(source_path).map_err(source_io)?;
        if file
            .metadata()
            .ok()
            .is_some_and(|metadata| metadata.len() > max_bytes)
        {
            return Err(AcquireError::LimitExceeded { max_bytes });
        }
        Ok(CatalogueBytes {
            bytes: read_bounded(&mut file, max_bytes)?,
            network_used: false,
            final_source_uri: None,
            redirects: 0,
            source_attestation: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch_file_inner(
        &self,
        source_path: &Path,
        staging_path: &Path,
        expected_bytes: u64,
        expected_sha256: &str,
        max_bytes: u64,
        attestation: Option<SourceAttestation>,
        progress: &mut dyn ProgressCallback,
    ) -> Result<VerifiedFetch, AcquireError> {
        let started_at_unix_ms = unix_time_millis()?;
        let source = File::open(source_path).map_err(source_io)?;
        self.fetch_open_file_inner(
            source,
            staging_path,
            expected_bytes,
            expected_sha256,
            max_bytes,
            attestation,
            started_at_unix_ms,
            progress,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch_open_file_inner(
        &self,
        mut source: File,
        staging_path: &Path,
        expected_bytes: u64,
        expected_sha256: &str,
        max_bytes: u64,
        attestation: Option<SourceAttestation>,
        started_at_unix_ms: u64,
        progress: &mut dyn ProgressCallback,
    ) -> Result<VerifiedFetch, AcquireError> {
        if let Ok(metadata) = source.metadata()
            && metadata.len() != expected_bytes
        {
            return Err(AcquireError::ContentLengthMismatch {
                declared: metadata.len(),
                expected: expected_bytes,
            });
        }
        stream_verified_file(
            &mut source,
            staging_path,
            expected_bytes,
            expected_sha256,
            max_bytes,
            attestation,
            started_at_unix_ms,
            progress,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch_http_artifact(
        &self,
        requested_uri: &str,
        initial_url: Url,
        staging_path: &Path,
        expected_bytes: u64,
        expected_sha256: &str,
        max_bytes: u64,
        options: &ArtifactFetchOptions,
        progress: &mut dyn ProgressCallback,
    ) -> Result<VerifiedFetch, AcquireError> {
        validate_artifact_url_metadata(&initial_url)?;
        let started_at_unix_ms = unix_time_millis()?;
        let expected_digest = canonical_sha256(expected_sha256)?;
        let mut prepared = PreparedTransfer::open(
            requested_uri,
            staging_path,
            expected_bytes,
            &expected_digest,
            &options.resume,
        )?;

        // An empty durable partial carries no useful bytes. Start a full
        // request instead of issuing a meaningless `Range: bytes=0-`.
        if prepared.offset == 0 && prepared.validator.is_some() {
            prepared.files.disable_resume()?;
            prepared.validator = None;
        }

        if prepared.offset == expected_bytes {
            let actual_digest = hex::encode(prepared.hasher.clone().finalize());
            if actual_digest != expected_digest {
                prepared.files.discard();
                return Err(AcquireError::DigestMismatch {
                    expected: expected_digest,
                    actual: actual_digest,
                });
            }
            if prepared.source_attestations.is_empty() {
                return Err(AcquireError::InvalidResumeState(
                    "completed partial lacks source attestation history".to_string(),
                ));
            }
            prepared.files.complete()?;
            let _control = progress.on_progress(TransferProgress {
                phase: TransferPhase::Complete,
                downloaded_bytes: expected_bytes,
                expected_bytes,
                resumed_bytes: expected_bytes,
            });
            return verified_from_attestations(
                expected_bytes,
                expected_digest,
                prepared.source_attestations,
                started_at_unix_ms,
                expected_bytes,
            );
        }

        let deadline = transfer_deadline(self.timeouts.total_transfer_timeout)?;
        let result = self.run_network(self.fetch_http_artifact_async(
            requested_uri,
            initial_url,
            expected_bytes,
            &expected_digest,
            max_bytes,
            options,
            progress,
            deadline,
            &mut prepared,
            started_at_unix_ms,
        ));
        if result.is_err() {
            prepared.settle_failed_attempt(requested_uri, expected_bytes, &expected_digest)?;
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn fetch_http_artifact_async(
        &self,
        requested_uri: &str,
        initial_url: Url,
        expected_bytes: u64,
        expected_digest: &str,
        max_bytes: u64,
        options: &ArtifactFetchOptions,
        progress: &mut dyn ProgressCallback,
        deadline: Instant,
        prepared: &mut PreparedTransfer,
        started_at_unix_ms: u64,
    ) -> Result<VerifiedFetch, AcquireError> {
        let mut resumed_bytes = prepared.offset;
        notify_progress(
            progress,
            TransferProgress {
                phase: if resumed_bytes == 0 {
                    TransferPhase::Starting
                } else {
                    TransferPhase::Resuming
                },
                downloaded_bytes: resumed_bytes,
                expected_bytes,
                resumed_bytes,
            },
        )?;

        let range = prepared.validator.as_ref().map(|validator| RangeRequest {
            offset: prepared.offset,
            validator,
        });
        let mut opened = self
            .open_http(
                requested_uri,
                initial_url,
                &options.acquisition_policy,
                deadline,
                range,
                false,
            )
            .await?;

        if prepared.offset == 0 {
            require_fresh_response(&opened.response, expected_bytes)?;
            prepared.begin_attestation(opened.attestation.clone(), 0, started_at_unix_ms)?;
            if options.resume.sidecar_path().is_some()
                && let Some(validator) = resumable_validator(&opened.response)
                && accepts_byte_ranges(&opened.response)
            {
                prepared.validator = Some(validator);
                let state =
                    prepared.resume_state(requested_uri, expected_bytes, expected_digest)?;
                prepared.files.enable_resume(&state)?;
            }
        } else if opened.response.status() == StatusCode::PARTIAL_CONTENT {
            validate_resumed_response(
                &opened.response,
                prepared.offset,
                expected_bytes,
                prepared
                    .validator
                    .as_ref()
                    .ok_or(AcquireError::InvalidResumeState(
                        "partial has no HTTP validator".to_string(),
                    ))?,
            )?;
            if let Err(error) = prepared.begin_attestation(
                opened.attestation.clone(),
                prepared.offset,
                started_at_unix_ms,
            ) {
                prepared.files.discard();
                return Err(error);
            }
            let state = prepared.resume_state(requested_uri, expected_bytes, expected_digest)?;
            if let Err(error) = prepared.files.rewrite_resume(&state) {
                prepared.files.discard();
                return Err(error);
            }
        } else if opened.response.status() == StatusCode::OK {
            require_fresh_response(&opened.response, expected_bytes)?;
            prepared.files.reset_staging()?;
            prepared.offset = 0;
            resumed_bytes = 0;
            prepared.hasher = Sha256::new();
            prepared.begin_attestation(opened.attestation.clone(), 0, started_at_unix_ms)?;

            if let Some(validator) = resumable_validator(&opened.response)
                && accepts_byte_ranges(&opened.response)
            {
                prepared.validator = Some(validator);
                let state =
                    prepared.resume_state(requested_uri, expected_bytes, expected_digest)?;
                if let Err(error) = prepared.files.rewrite_resume(&state) {
                    prepared.files.discard();
                    return Err(error);
                }
            } else {
                prepared.files.disable_resume()?;
                prepared.validator = None;
            }
        } else {
            return Err(AcquireError::HttpStatus(opened.response.status().as_u16()));
        }

        let mut received = prepared.offset;
        while let Some(chunk) = opened.response.chunk().await.map_err(network_error)? {
            let chunk_len =
                u64::try_from(chunk.len()).map_err(|_| AcquireError::IntegerOverflow)?;
            received = received
                .checked_add(chunk_len)
                .ok_or(AcquireError::IntegerOverflow)?;
            if received > max_bytes {
                prepared.files.discard();
                return Err(AcquireError::LimitExceeded { max_bytes });
            }
            if received > expected_bytes {
                prepared.files.discard();
                return Err(AcquireError::LengthMismatch {
                    expected: expected_bytes,
                    actual: received,
                });
            }
            prepared.hasher.update(&chunk);
            prepared.files.write_all(&chunk)?;
            notify_progress(
                progress,
                TransferProgress {
                    phase: TransferPhase::Downloading,
                    downloaded_bytes: received,
                    expected_bytes,
                    resumed_bytes,
                },
            )?;
        }

        if received != expected_bytes {
            return Err(AcquireError::LengthMismatch {
                expected: expected_bytes,
                actual: received,
            });
        }
        let actual_digest = hex::encode(prepared.hasher.clone().finalize());
        if actual_digest != expected_digest {
            prepared.files.discard();
            return Err(AcquireError::DigestMismatch {
                expected: expected_digest.to_string(),
                actual: actual_digest,
            });
        }
        let finished_at_unix_ms = unix_time_millis()?;
        prepared.finish_active_attestation(received, finished_at_unix_ms)?;
        prepared.files.complete()?;
        let _control = progress.on_progress(TransferProgress {
            phase: TransferPhase::Complete,
            downloaded_bytes: received,
            expected_bytes,
            resumed_bytes,
        });
        verified_from_attestations_at(
            received,
            actual_digest,
            std::mem::take(&mut prepared.source_attestations),
            started_at_unix_ms,
            finished_at_unix_ms,
            resumed_bytes,
        )
    }

    async fn fetch_catalogue_http(
        &self,
        requested_uri: &str,
        initial_url: Url,
        max_bytes: u64,
        policy: &AcquisitionPolicy,
        deadline: Instant,
    ) -> Result<CatalogueBytes, AcquireError> {
        let mut opened = self
            .open_http(requested_uri, initial_url, policy, deadline, None, true)
            .await?;
        if !opened.response.status().is_success() {
            return Err(AcquireError::HttpStatus(opened.response.status().as_u16()));
        }
        if opened.response.status() == StatusCode::PARTIAL_CONTENT {
            return Err(AcquireError::UnexpectedPartialResponse);
        }
        if opened
            .response
            .content_length()
            .is_some_and(|declared| declared > max_bytes)
        {
            return Err(AcquireError::LimitExceeded { max_bytes });
        }
        let mut bytes = Vec::with_capacity(
            usize::try_from(max_bytes.min(1024 * 1024))
                .map_err(|_| AcquireError::IntegerOverflow)?,
        );
        let mut received = 0_u64;
        while let Some(chunk) = opened.response.chunk().await.map_err(network_error)? {
            received = received
                .checked_add(u64::try_from(chunk.len()).map_err(|_| AcquireError::IntegerOverflow)?)
                .ok_or(AcquireError::IntegerOverflow)?;
            if received > max_bytes {
                return Err(AcquireError::LimitExceeded { max_bytes });
            }
            bytes.extend_from_slice(&chunk);
        }
        let attestation = opened.attestation;
        Ok(CatalogueBytes {
            bytes,
            network_used: true,
            final_source_uri: Some(attestation.final_uri.clone()),
            redirects: attestation.redirect_chain.len(),
            source_attestation: Some(attestation),
        })
    }

    async fn open_http<'a>(
        &self,
        requested_uri: &str,
        initial_url: Url,
        policy: &AcquisitionPolicy,
        deadline: Instant,
        range: Option<RangeRequest<'a>>,
        allow_query_and_fragment: bool,
    ) -> Result<OpenedHttp, AcquireError> {
        let mut current = initial_url;
        let mut redirect_chain = Vec::new();
        loop {
            validate_http_url(&current)?;
            if !allow_query_and_fragment {
                validate_artifact_url_metadata(&current)?;
            }
            let destination = resolve_destination(
                &current,
                policy,
                deadline,
                self.timeouts.total_transfer_timeout,
            )
            .await?;
            let client = self.client_for_destination(&destination)?;
            let remaining = remaining_transfer_time(deadline)?;
            let mut request = client
                .get(current.clone())
                .header(ACCEPT_ENCODING, "identity")
                .timeout(remaining);
            if let Some(range) = range {
                request = request
                    .header(RANGE, format!("bytes={}-", range.offset))
                    .header(IF_RANGE, range.validator.as_header_value()?);
            }
            let response = request.send().await.map_err(network_error)?;
            let peer = validate_connected_peer(&response, &destination, policy)?;

            if is_followable_redirect(response.status()) {
                if redirect_chain.len() >= self.config.max_redirects {
                    return Err(AcquireError::TooManyRedirects {
                        max_redirects: self.config.max_redirects,
                    });
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .ok_or(AcquireError::RedirectMissingLocation)?
                    .to_str()
                    .map_err(|_| AcquireError::InvalidRedirectLocation)?;
                let next = current
                    .join(location)
                    .map_err(|_| AcquireError::InvalidRedirectLocation)?;
                validate_redirect(&current, &next)?;
                if !allow_query_and_fragment {
                    validate_artifact_url_metadata(&next)?;
                }
                redirect_chain.push(RedirectAttestation {
                    status: response.status().as_u16(),
                    from_uri: current.to_string(),
                    to_uri: next.to_string(),
                    peer_address: peer,
                });
                current = next;
                continue;
            }
            if response.status().is_redirection() {
                return Err(AcquireError::HttpStatus(response.status().as_u16()));
            }
            reject_encoded_response(&response)?;
            let attestation = SourceAttestation {
                requested_uri: requested_uri.to_string(),
                redirect_chain,
                final_uri: current.to_string(),
                final_peer_address: peer,
            };
            return Ok(OpenedHttp {
                response,
                attestation,
            });
        }
    }

    fn client_for_destination(
        &self,
        destination: &ResolvedDestination,
    ) -> Result<Client, AcquireError> {
        let mut builder = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(self.config.connect_timeout)
            .read_timeout(self.timeouts.read_idle_timeout)
            .user_agent(&self.config.user_agent);
        if let Some(host) = &destination.dns_host {
            builder = builder.resolve_to_addrs(host, &destination.addresses);
        }
        builder.build().map_err(network_error)
    }

    fn run_network<T, F>(&self, future: F) -> Result<T, AcquireError>
    where
        F: Future<Output = Result<T, AcquireError>>,
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| AcquireError::Runtime(error.to_string()))?;
        let result = runtime.block_on(async {
            tokio::time::timeout(self.timeouts.total_transfer_timeout, future)
                .await
                .map_err(|_| AcquireError::TotalTransferTimeout {
                    timeout: self.timeouts.total_transfer_timeout,
                })?
        });
        // Tokio's resolver may occupy a blocking worker after its future is
        // cancelled. Never let runtime destruction extend the public deadline.
        runtime.shutdown_timeout(Duration::ZERO);
        result
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedirectAttestation {
    pub status: u16,
    pub from_uri: String,
    pub to_uri: String,
    pub peer_address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAttestation {
    pub requested_uri: String,
    pub redirect_chain: Vec<RedirectAttestation>,
    pub final_uri: String,
    pub final_peer_address: String,
}

impl SourceAttestation {
    fn direct(uri: String) -> Self {
        Self {
            requested_uri: uri.clone(),
            redirect_chain: Vec::new(),
            final_uri: uri,
            final_peer_address: String::new(),
        }
    }
}

/// One source contact that supplied a half-open byte range during a verified fetch.
///
/// `byte_start..byte_end` describes bytes written to the staging artifact by this
/// attempt. A missing finish time means the process ended before it could durably
/// close the attempt; its byte end is reconstructed from the identity-bound
/// staging file on the next resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAttemptAttestation {
    pub source: SourceAttestation,
    pub byte_start: u64,
    pub byte_end: u64,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: Option<u64>,
}

fn validate_attestation_history(
    history: &[SourceAttemptAttestation],
    requested_uri: &str,
    expected_bytes: u64,
) -> Result<(), AcquireError> {
    if history.is_empty() || history.len() > MAX_SOURCE_ATTESTATIONS {
        return Err(AcquireError::InvalidResumeState(
            "source attempt history is empty or exceeds its limit".to_string(),
        ));
    }
    let mut prior_end = None;
    for (index, attempt) in history.iter().enumerate() {
        if (index == 0 && attempt.byte_start != 0)
            || attempt.byte_start > attempt.byte_end
            || attempt.byte_end > expected_bytes
        {
            return Err(AcquireError::InvalidResumeState(
                "source attempt byte range is invalid".to_string(),
            ));
        }
        if index > 0 && attempt.byte_start != 0 && prior_end != Some(attempt.byte_start) {
            return Err(AcquireError::InvalidResumeState(
                "source attempt byte ranges are discontinuous".to_string(),
            ));
        }
        if attempt.started_at_unix_ms == 0
            || attempt
                .finished_at_unix_ms
                .is_some_and(|finished| finished < attempt.started_at_unix_ms)
        {
            return Err(AcquireError::InvalidResumeState(
                "source attempt timing is invalid".to_string(),
            ));
        }
        validate_persisted_source(&attempt.source, requested_uri)?;
        prior_end = Some(attempt.byte_end);
    }
    Ok(())
}

fn validate_persisted_source(
    attestation: &SourceAttestation,
    requested_uri: &str,
) -> Result<(), AcquireError> {
    if attestation.requested_uri != requested_uri
        || attestation.redirect_chain.len() > MAX_ATTESTED_REDIRECTS
    {
        return Err(AcquireError::InvalidResumeState(
            "source attestation request or redirect count is invalid".to_string(),
        ));
    }
    let mut current = Url::parse(requested_uri).map_err(|_| {
        AcquireError::InvalidResumeState("source attestation request URI is invalid".to_string())
    })?;
    validate_http_url(&current).map_err(|_| {
        AcquireError::InvalidResumeState(
            "source attestation request URI is not HTTP(S)".to_string(),
        )
    })?;
    validate_artifact_url_metadata(&current).map_err(|_| {
        AcquireError::InvalidResumeState(
            "source attestation request URI contains private URL metadata".to_string(),
        )
    })?;
    for redirect in &attestation.redirect_chain {
        let status = StatusCode::from_u16(redirect.status).map_err(|_| {
            AcquireError::InvalidResumeState(
                "source attestation redirect status is invalid".to_string(),
            )
        })?;
        let from = Url::parse(&redirect.from_uri).map_err(|_| {
            AcquireError::InvalidResumeState(
                "source attestation redirect origin is invalid".to_string(),
            )
        })?;
        let to = Url::parse(&redirect.to_uri).map_err(|_| {
            AcquireError::InvalidResumeState(
                "source attestation redirect target is invalid".to_string(),
            )
        })?;
        if !is_followable_redirect(status)
            || from != current
            || validate_redirect(&from, &to).is_err()
            || validate_artifact_url_metadata(&to).is_err()
            || redirect.peer_address.parse::<SocketAddr>().is_err()
        {
            return Err(AcquireError::InvalidResumeState(
                "source attestation redirect chain is inconsistent".to_string(),
            ));
        }
        current = to;
    }
    let final_uri = Url::parse(&attestation.final_uri).map_err(|_| {
        AcquireError::InvalidResumeState("source attestation final URI is invalid".to_string())
    })?;
    if final_uri != current
        || validate_artifact_url_metadata(&final_uri).is_err()
        || attestation
            .final_peer_address
            .parse::<SocketAddr>()
            .is_err()
    {
        return Err(AcquireError::InvalidResumeState(
            "source attestation final source is inconsistent".to_string(),
        ));
    }
    Ok(())
}

fn reconcile_interrupted_attestation(
    history: &mut [SourceAttemptAttestation],
    staging_bytes: u64,
) -> Result<(), AcquireError> {
    let last = history.last_mut().ok_or_else(|| {
        AcquireError::InvalidResumeState("source attempt history is empty".to_string())
    })?;
    if last.finished_at_unix_ms.is_none() {
        if staging_bytes < last.byte_start {
            return Err(AcquireError::InvalidResumeState(
                "interrupted source attempt exceeds staging bytes".to_string(),
            ));
        }
        last.byte_end = staging_bytes;
    } else if last.byte_end != staging_bytes {
        return Err(AcquireError::InvalidResumeState(
            "source attempt history does not match staging bytes".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFetch {
    pub bytes: u64,
    pub sha256: String,
    pub network_used: bool,
    pub final_source_uri: Option<String>,
    pub redirects: usize,
    /// The last source contact, retained for backwards compatibility.
    pub source_attestation: Option<SourceAttestation>,
    /// Ordered source contacts for all durable attempts that contributed or
    /// were superseded while producing the verified artifact.
    pub source_attestations: Vec<SourceAttemptAttestation>,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub resumed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueBytes {
    pub bytes: Vec<u8>,
    pub network_used: bool,
    pub final_source_uri: Option<String>,
    pub redirects: usize,
    pub source_attestation: Option<SourceAttestation>,
}

#[derive(Debug, Error)]
pub enum AcquireError {
    #[error("invalid acquisition configuration: {0}")]
    InvalidConfig(String),
    #[error("source URI is invalid")]
    InvalidSourceUri,
    #[error("source URI scheme is not supported: {0}")]
    UnsupportedScheme(String),
    #[error("credentials embedded in source URIs are forbidden")]
    CredentialsForbidden,
    #[error("artifact HTTP source and redirects cannot contain a query or fragment")]
    ArtifactQueryOrFragmentForbidden,
    #[error("file URI is invalid for this platform")]
    InvalidFileUri,
    #[error("file URI access requires an explicit canonical-root grant")]
    FileUriForbidden,
    #[error("file URI resolved outside every granted root: {0}")]
    FileOutsideGrantedRoots(PathBuf),
    #[error("file URI source changed identity while it was being opened")]
    FileSourceIdentityChanged,
    #[error("granted file root is not a directory: {0}")]
    FileRootNotDirectory(PathBuf),
    #[error("file policy check failed: {0}")]
    FilePolicyIo(#[source] io::Error),
    #[error("network destination has no host")]
    MissingNetworkHost,
    #[error("network destination has no usable port")]
    MissingNetworkPort,
    #[error("network destination is forbidden by policy: {address}")]
    NetworkDestinationForbidden { address: IpAddr },
    #[error("DNS resolution failed for {host}: {message}")]
    DnsResolution { host: String, message: String },
    #[error("DNS resolution for {host} returned no addresses")]
    DnsResolutionEmpty { host: String },
    #[error("DNS resolution for {host} exceeded the address limit ({max_addresses})")]
    TooManyResolvedAddresses { host: String, max_addresses: usize },
    #[error("HTTP response did not expose its connected peer address")]
    PeerAddressUnavailable,
    #[error("connected peer {peer} was not among the pinned DNS results")]
    PeerAddressMismatch { peer: IpAddr },
    #[error("network request failed: {0}")]
    Network(String),
    #[error("HTTP response status was {0}")]
    HttpStatus(u16),
    #[error("redirect response did not contain a Location header")]
    RedirectMissingLocation,
    #[error("redirect Location header is invalid")]
    InvalidRedirectLocation,
    #[error("redirect to URI scheme {0:?} is forbidden")]
    RedirectSchemeForbidden(String),
    #[error("HTTPS-to-HTTP redirect is forbidden")]
    HttpsDowngradeRedirect,
    #[error("redirect limit exceeded ({max_redirects})")]
    TooManyRedirects { max_redirects: usize },
    #[error("encoded HTTP response is forbidden for digest-pinned bytes")]
    EncodedResponseForbidden,
    #[error("unexpected partial response to a request without a byte range")]
    UnexpectedPartialResponse,
    #[error("resumed response did not honor the requested byte range")]
    InvalidContentRange,
    #[error("resumed response validator did not match its durable sidecar")]
    ResumeValidatorMismatch,
    #[error("source declared {declared} bytes but {expected} were expected")]
    ContentLengthMismatch { declared: u64, expected: u64 },
    #[error("byte limit exceeded ({max_bytes})")]
    LimitExceeded { max_bytes: u64 },
    #[error("artifact length mismatch: expected {expected}, received {actual}")]
    LengthMismatch { expected: u64, actual: u64 },
    #[error("expected SHA-256 digest is invalid")]
    InvalidExpectedDigest,
    #[error("artifact SHA-256 mismatch: expected {expected}, received {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("staging path already exists")]
    StagingPathExists,
    #[error("resume sidecar path already exists without its staging file")]
    OrphanedResumeSidecar,
    #[error("staging file exists without its resume sidecar")]
    MissingResumeSidecar,
    #[error("resume is only supported for HTTP(S) artifacts")]
    ResumeUnsupportedForFile,
    #[error(
        "durable resume is unavailable on this platform because safe file identity is unavailable"
    )]
    DurableResumeUnsupportedOnPlatform,
    #[error("resume sidecar and staging paths must differ")]
    ResumePathCollision,
    #[error("durable resume staging and sidecar must share one canonical directory")]
    ResumePathsDifferentDirectories,
    #[error("durable resume directory must be owned by this user and private (0700)")]
    UnsafeResumeDirectory,
    #[error("durable resume directory is already in use")]
    ResumeDirectoryBusy,
    #[error("resume state is invalid: {0}")]
    InvalidResumeState(String),
    #[error("resume state does not describe this source, length, and digest")]
    ResumeStateMismatch,
    #[error("staging or sidecar path changed identity before safe cleanup")]
    CleanupIdentityChanged,
    #[error("staging path is not a private regular file")]
    UnsafeStagingFile,
    #[error("source read failed: {0}")]
    SourceIo(#[source] io::Error),
    #[error("staging write failed: {0}")]
    StagingIo(#[source] io::Error),
    #[error("transfer cancelled after {downloaded_bytes} bytes")]
    Cancelled { downloaded_bytes: u64 },
    #[error("total transfer timeout of {timeout:?} expired")]
    TotalTransferTimeout { timeout: Duration },
    #[error("failed to start the HTTP runtime: {0}")]
    Runtime(String),
    #[error("system clock cannot produce a receipt timestamp: {0}")]
    SystemClock(String),
    #[error("integer overflow while accounting for bytes")]
    IntegerOverflow,
}

#[derive(Debug)]
struct OpenedHttp {
    response: Response,
    attestation: SourceAttestation,
}

#[derive(Debug)]
enum FetchSource {
    Http(Url),
    File { path: PathBuf, uri: String },
}

#[derive(Debug)]
struct ResolvedDestination {
    dns_host: Option<String>,
    addresses: Vec<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum HttpValidator {
    StrongEtag(String),
    LastModified(String),
}

impl HttpValidator {
    fn as_header_value(&self) -> Result<HeaderValue, AcquireError> {
        let value = match self {
            Self::StrongEtag(value) | Self::LastModified(value) => value,
        };
        HeaderValue::from_str(value).map_err(|_| {
            AcquireError::InvalidResumeState("validator is not a valid HTTP header".to_string())
        })
    }

    fn matches_response(&self, response: &Response) -> bool {
        match self {
            Self::StrongEtag(expected) => response
                .headers()
                .get(ETAG)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|actual| actual == expected),
            Self::LastModified(expected) => response
                .headers()
                .get(LAST_MODIFIED)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|actual| actual == expected),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResumeSidecar {
    version: u32,
    requested_uri: String,
    expected_bytes: u64,
    expected_sha256: String,
    staging_identity: FileIdentity,
    validator: HttpValidator,
    source_attestations: Vec<SourceAttemptAttestation>,
}

#[derive(Debug, Clone, Copy)]
struct RangeRequest<'a> {
    offset: u64,
    validator: &'a HttpValidator,
}

#[derive(Debug)]
struct ResumeDirectoryLease {
    _directory: File,
}

#[derive(Debug)]
struct DurableResumePaths {
    staging: PathBuf,
    sidecar: PathBuf,
    lease: ResumeDirectoryLease,
}

impl DurableResumePaths {
    #[cfg(unix)]
    fn acquire(staging_path: &Path, sidecar_path: &Path) -> Result<Self, AcquireError> {
        let staging_name = staging_path
            .file_name()
            .ok_or(AcquireError::ResumePathCollision)?;
        let sidecar_name = sidecar_path
            .file_name()
            .ok_or(AcquireError::ResumePathCollision)?;
        if staging_name == sidecar_name {
            return Err(AcquireError::ResumePathCollision);
        }
        let staging_parent = canonical_parent(staging_path)?;
        let sidecar_parent = canonical_parent(sidecar_path)?;
        if staging_parent != sidecar_parent {
            return Err(AcquireError::ResumePathsDifferentDirectories);
        }
        let directory = rustix::fs::open(
            &staging_parent,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
        .map_err(staging_io)?;
        let metadata = directory.metadata().map_err(staging_io)?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.permissions().mode() & 0o700 != 0o700
        {
            return Err(AcquireError::UnsafeResumeDirectory);
        }
        directory.try_lock_exclusive().map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                AcquireError::ResumeDirectoryBusy
            } else {
                staging_io(error)
            }
        })?;
        let current = fs::symlink_metadata(&staging_parent).map_err(staging_io)?;
        if !current.is_dir() || current.dev() != metadata.dev() || current.ino() != metadata.ino() {
            return Err(AcquireError::CleanupIdentityChanged);
        }
        Ok(Self {
            staging: staging_parent.join(staging_name),
            sidecar: staging_parent.join(sidecar_name),
            lease: ResumeDirectoryLease {
                _directory: directory,
            },
        })
    }

    #[cfg(not(unix))]
    fn acquire(_staging_path: &Path, _sidecar_path: &Path) -> Result<Self, AcquireError> {
        Err(AcquireError::DurableResumeUnsupportedOnPlatform)
    }
}

fn canonical_parent(path: &Path) -> Result<PathBuf, AcquireError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::canonicalize(parent).map_err(staging_io)
}

#[derive(Debug)]
struct PreparedTransfer {
    files: TransferFiles,
    offset: u64,
    hasher: Sha256,
    validator: Option<HttpValidator>,
    source_attestations: Vec<SourceAttemptAttestation>,
    active_attestation: Option<usize>,
}

impl PreparedTransfer {
    fn fresh(staging_path: &Path) -> Result<Self, AcquireError> {
        Self::fresh_with_lease(staging_path, None, None)
    }

    fn fresh_with_lease(
        staging_path: &Path,
        sidecar_path: Option<PathBuf>,
        lease: Option<ResumeDirectoryLease>,
    ) -> Result<Self, AcquireError> {
        Ok(Self {
            files: TransferFiles::fresh(staging_path, sidecar_path, lease)?,
            offset: 0,
            hasher: Sha256::new(),
            validator: None,
            source_attestations: Vec::new(),
            active_attestation: None,
        })
    }

    fn open(
        requested_uri: &str,
        staging_path: &Path,
        expected_bytes: u64,
        expected_sha256: &str,
        resume: &ResumePolicy,
    ) -> Result<Self, AcquireError> {
        ensure_durable_resume_supported(resume)?;
        let Some(sidecar_path) = resume.sidecar_path() else {
            return Self::fresh(staging_path);
        };
        let paths = DurableResumePaths::acquire(staging_path, sidecar_path)?;
        let staging_path = paths.staging;
        let sidecar_path = paths.sidecar;
        let mut lease = Some(paths.lease);
        let staging_exists = path_exists(&staging_path)?;
        let sidecar_exists = path_exists(&sidecar_path)?;
        match (staging_exists, sidecar_exists) {
            (false, false) => {
                Self::fresh_with_lease(&staging_path, Some(sidecar_path), lease.take())
            }
            (true, false) => {
                let mut staging = StagingFile::open_existing(&staging_path)?;
                staging.remove_now()?;
                Self::fresh_with_lease(&staging_path, Some(sidecar_path), lease.take())
            }
            (false, true) => {
                let mut sidecar = SidecarFile::open(&sidecar_path)?;
                sidecar.remove_now()?;
                Self::fresh_with_lease(&staging_path, Some(sidecar_path), lease.take())
            }
            (true, true) => {
                let sidecar = SidecarFile::open(&sidecar_path)?;
                let staging = StagingFile::open_existing(&staging_path)?;
                if sidecar.identity == staging.identity {
                    return Err(AcquireError::ResumePathCollision);
                }
                let mut files = TransferFiles::resuming(
                    staging,
                    sidecar,
                    lease.take().ok_or_else(|| {
                        AcquireError::InvalidResumeState(
                            "durable resume directory lease is missing".to_string(),
                        )
                    })?,
                );
                let state = match files.read_resume_state() {
                    Ok(state) => state,
                    Err(AcquireError::InvalidResumeState(_)) => {
                        return Self::fresh_after_invalid_resume(files, &staging_path);
                    }
                    Err(error) => return Err(error),
                };
                let state_mismatches = state.version != RESUME_SIDECAR_VERSION
                    || state.requested_uri != requested_uri
                    || state.expected_bytes != expected_bytes
                    || state.expected_sha256 != expected_sha256
                    || state.staging_identity != files.staging.identity;
                if state_mismatches
                    || state.validator.as_header_value().is_err()
                    || validate_attestation_history(
                        &state.source_attestations,
                        requested_uri,
                        expected_bytes,
                    )
                    .is_err()
                {
                    return Self::fresh_after_invalid_resume(files, &staging_path);
                }
                let (offset, hasher) = match files.staging.hash_existing(expected_bytes) {
                    Ok(existing) => existing,
                    Err(error @ AcquireError::LengthMismatch { .. }) => {
                        files.discard();
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                };
                let mut source_attestations = state.source_attestations;
                if reconcile_interrupted_attestation(&mut source_attestations, offset).is_err() {
                    return Self::fresh_after_invalid_resume(files, &staging_path);
                }
                Ok(Self {
                    files,
                    offset,
                    hasher,
                    validator: Some(state.validator),
                    source_attestations,
                    active_attestation: None,
                })
            }
        }
    }

    fn fresh_after_invalid_resume(
        files: TransferFiles,
        staging_path: &Path,
    ) -> Result<Self, AcquireError> {
        let files = files.recover_fresh(staging_path)?;
        Ok(Self {
            files,
            offset: 0,
            hasher: Sha256::new(),
            validator: None,
            source_attestations: Vec::new(),
            active_attestation: None,
        })
    }

    fn begin_attestation(
        &mut self,
        source: SourceAttestation,
        byte_start: u64,
        started_at_unix_ms: u64,
    ) -> Result<(), AcquireError> {
        if self.active_attestation.is_some() {
            return Err(AcquireError::InvalidResumeState(
                "source attempt is already active".to_string(),
            ));
        }
        if self.source_attestations.len() >= MAX_SOURCE_ATTESTATIONS {
            return Err(AcquireError::InvalidResumeState(
                "source attempt history exceeds its limit".to_string(),
            ));
        }
        self.source_attestations.push(SourceAttemptAttestation {
            source,
            byte_start,
            byte_end: byte_start,
            started_at_unix_ms,
            finished_at_unix_ms: None,
        });
        self.active_attestation = Some(self.source_attestations.len() - 1);
        Ok(())
    }

    fn finish_active_attestation(
        &mut self,
        byte_end: u64,
        finished_at_unix_ms: u64,
    ) -> Result<(), AcquireError> {
        let Some(index) = self.active_attestation.take() else {
            return Ok(());
        };
        let attestation = self.source_attestations.get_mut(index).ok_or_else(|| {
            AcquireError::InvalidResumeState("active source attempt is missing".to_string())
        })?;
        if byte_end < attestation.byte_start || finished_at_unix_ms < attestation.started_at_unix_ms
        {
            return Err(AcquireError::InvalidResumeState(
                "source attempt has invalid byte or time bounds".to_string(),
            ));
        }
        attestation.byte_end = byte_end;
        attestation.finished_at_unix_ms = Some(finished_at_unix_ms);
        Ok(())
    }

    fn resume_state(
        &self,
        requested_uri: &str,
        expected_bytes: u64,
        expected_sha256: &str,
    ) -> Result<ResumeSidecar, AcquireError> {
        Ok(ResumeSidecar {
            version: RESUME_SIDECAR_VERSION,
            requested_uri: requested_uri.to_string(),
            expected_bytes,
            expected_sha256: expected_sha256.to_string(),
            staging_identity: self.files.staging.identity,
            validator: self.validator.clone().ok_or_else(|| {
                AcquireError::InvalidResumeState(
                    "durable source attempt has no HTTP validator".to_string(),
                )
            })?,
            source_attestations: self.source_attestations.clone(),
        })
    }

    fn settle_failed_attempt(
        &mut self,
        requested_uri: &str,
        expected_bytes: u64,
        expected_sha256: &str,
    ) -> Result<(), AcquireError> {
        if self.active_attestation.is_none() {
            return self.files.settle_failure();
        }
        let byte_end = self.files.staging.len()?;
        if byte_end > 0 && self.files.has_sidecar() {
            self.files.staging.sync_partial()?;
        }
        self.finish_active_attestation(byte_end, unix_time_millis()?)?;
        if byte_end > 0 && self.files.has_sidecar() {
            let state = self.resume_state(requested_uri, expected_bytes, expected_sha256)?;
            if let Err(error) = self.files.rewrite_resume(&state) {
                self.files.discard();
                return Err(error);
            }
        }
        self.files.settle_failure()
    }
}

#[derive(Debug)]
struct TransferFiles {
    staging: StagingFile,
    sidecar: Option<SidecarFile>,
    resume_sidecar_path: Option<PathBuf>,
    retain_partial: bool,
    lease: Option<ResumeDirectoryLease>,
}

impl TransferFiles {
    fn fresh(
        staging_path: &Path,
        resume_sidecar_path: Option<PathBuf>,
        lease: Option<ResumeDirectoryLease>,
    ) -> Result<Self, AcquireError> {
        Ok(Self {
            staging: StagingFile::create(staging_path)?,
            sidecar: None,
            resume_sidecar_path,
            retain_partial: false,
            lease,
        })
    }

    fn resuming(
        mut staging: StagingFile,
        mut sidecar: SidecarFile,
        lease: ResumeDirectoryLease,
    ) -> Self {
        staging.keep = false;
        sidecar.keep = false;
        let resume_sidecar_path = sidecar.path.clone();
        Self {
            staging,
            sidecar: Some(sidecar),
            resume_sidecar_path: Some(resume_sidecar_path),
            retain_partial: true,
            lease: Some(lease),
        }
    }

    fn enable_resume(&mut self, state: &ResumeSidecar) -> Result<(), AcquireError> {
        if self.sidecar.is_some() {
            return Err(AcquireError::InvalidResumeState(
                "resume sidecar already open".to_string(),
            ));
        }
        let sidecar_path = self.resume_sidecar_path.as_deref().ok_or_else(|| {
            AcquireError::InvalidResumeState(
                "durable resume has no canonical sidecar path".to_string(),
            )
        })?;
        self.sidecar = Some(SidecarFile::create(sidecar_path, state)?);
        self.retain_partial = true;
        Ok(())
    }

    fn rewrite_resume(&mut self, state: &ResumeSidecar) -> Result<(), AcquireError> {
        let sidecar = self
            .sidecar
            .as_mut()
            .ok_or(AcquireError::MissingResumeSidecar)?;
        sidecar.replace_state(state)?;
        self.retain_partial = true;
        Ok(())
    }

    fn read_resume_state(&mut self) -> Result<ResumeSidecar, AcquireError> {
        self.sidecar
            .as_mut()
            .ok_or(AcquireError::MissingResumeSidecar)?
            .read_state()
    }

    fn has_sidecar(&self) -> bool {
        self.sidecar.is_some()
    }

    fn recover_fresh(mut self, staging_path: &Path) -> Result<Self, AcquireError> {
        let lease = self.lease.take();
        let resume_sidecar_path = self.resume_sidecar_path.take();
        self.staging.ensure_current()?;
        self.sidecar
            .as_ref()
            .ok_or(AcquireError::MissingResumeSidecar)?
            .ensure_current()?;

        if let Some(mut sidecar) = self.sidecar.take() {
            sidecar.remove_now()?;
        }
        self.staging.remove_now()?;
        self.retain_partial = false;
        Self::fresh(staging_path, resume_sidecar_path, lease)
    }

    fn disable_resume(&mut self) -> Result<(), AcquireError> {
        self.retain_partial = false;
        if let Some(mut sidecar) = self.sidecar.take() {
            sidecar.remove_now()?;
        }
        Ok(())
    }

    fn reset_staging(&mut self) -> Result<(), AcquireError> {
        self.staging.file.set_len(0).map_err(staging_io)?;
        self.staging
            .file
            .seek(SeekFrom::Start(0))
            .map_err(staging_io)?;
        self.staging.file.sync_all().map_err(staging_io)?;
        Ok(())
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), AcquireError> {
        self.staging.file.write_all(bytes).map_err(staging_io)
    }

    fn discard(&mut self) {
        self.retain_partial = false;
        self.staging.keep = false;
        if let Some(sidecar) = &mut self.sidecar {
            sidecar.keep = false;
        }
    }

    fn settle_failure(&mut self) -> Result<(), AcquireError> {
        let partial_bytes = self.staging.file.metadata().map_err(staging_io)?.len();
        if partial_bytes == 0 {
            self.discard();
        } else if self.retain_partial
            && let Err(error) = self.staging.sync_partial()
        {
            self.discard();
            return Err(error);
        }
        Ok(())
    }

    fn complete(&mut self) -> Result<(), AcquireError> {
        self.staging.finish()?;
        self.retain_partial = false;
        if let Some(sidecar) = &mut self.sidecar {
            sidecar.remove_now()?;
        }
        Ok(())
    }
}

impl Drop for TransferFiles {
    fn drop(&mut self) {
        if self.retain_partial {
            self.staging.keep = true;
            if let Some(sidecar) = &mut self.sidecar {
                sidecar.keep = true;
            }
        }
    }
}

#[derive(Debug)]
struct StagingFile {
    path: PathBuf,
    file: File,
    identity: FileIdentity,
    keep: bool,
}

impl StagingFile {
    fn create(path: &Path) -> Result<Self, AcquireError> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(path).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                AcquireError::StagingPathExists
            } else {
                staging_io(error)
            }
        })?;
        let identity = FileIdentity::from_file(&file)?;
        let staging = Self {
            path: path.to_path_buf(),
            file,
            identity,
            keep: false,
        };
        enforce_private_regular_file(&staging.file)?;
        sync_parent_directory(path)?;
        Ok(staging)
    }

    fn open_existing(path: &Path) -> Result<Self, AcquireError> {
        let file = open_existing_private_file(path, true)?;
        enforce_private_regular_file(&file)?;
        let identity = FileIdentity::from_file(&file)?;
        if !identity.matches_path(path)? {
            return Err(AcquireError::CleanupIdentityChanged);
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
            identity,
            keep: true,
        })
    }

    fn hash_existing(&mut self, expected_bytes: u64) -> Result<(u64, Sha256), AcquireError> {
        self.file.seek(SeekFrom::Start(0)).map_err(staging_io)?;
        let mut hasher = Sha256::new();
        let mut received = 0_u64;
        let mut buffer = [0_u8; STREAM_BUFFER_BYTES];
        loop {
            let read = self.file.read(&mut buffer).map_err(staging_io)?;
            if read == 0 {
                break;
            }
            received = received
                .checked_add(u64::try_from(read).map_err(|_| AcquireError::IntegerOverflow)?)
                .ok_or(AcquireError::IntegerOverflow)?;
            if received > expected_bytes {
                self.keep = false;
                return Err(AcquireError::LengthMismatch {
                    expected: expected_bytes,
                    actual: received,
                });
            }
            hasher.update(&buffer[..read]);
        }
        self.file.seek(SeekFrom::End(0)).map_err(staging_io)?;
        Ok((received, hasher))
    }

    fn len(&self) -> Result<u64, AcquireError> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(staging_io)
    }

    fn sync_partial(&mut self) -> Result<(), AcquireError> {
        self.file.flush().map_err(staging_io)?;
        self.file.sync_all().map_err(staging_io)
    }

    fn finish(&mut self) -> Result<(), AcquireError> {
        self.sync_partial()?;
        sync_parent_directory(&self.path)?;
        self.keep = true;
        Ok(())
    }

    fn ensure_current(&self) -> Result<(), AcquireError> {
        if self.identity.matches_path(&self.path)? {
            Ok(())
        } else {
            Err(AcquireError::CleanupIdentityChanged)
        }
    }

    fn remove_now(&mut self) -> Result<(), AcquireError> {
        self.ensure_current()?;
        fs::remove_file(&self.path).map_err(staging_io)?;
        self.keep = true;
        sync_parent_directory(&self.path)
    }
}

impl Drop for StagingFile {
    fn drop(&mut self) {
        if !self.keep && self.identity.matches_path(&self.path).unwrap_or(false) {
            let _ignored = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug)]
struct SidecarFile {
    path: PathBuf,
    file: File,
    identity: FileIdentity,
    keep: bool,
}

impl SidecarFile {
    fn create(path: &Path, state: &ResumeSidecar) -> Result<Self, AcquireError> {
        let encoded = encode_resume_sidecar(state)?;
        PrivateSidecarTemp::create(path, &encoded)?.publish_new(path)
    }

    fn open(path: &Path) -> Result<Self, AcquireError> {
        let file = open_existing_private_file(path, false)?;
        enforce_private_file_mode(&file)?;
        let identity = FileIdentity::from_file(&file)?;
        if !identity.matches_path(path)? {
            return Err(AcquireError::CleanupIdentityChanged);
        }
        #[cfg(unix)]
        if file.metadata().map_err(staging_io)?.nlink() != 1 {
            cleanup_published_temp_link(path, identity)?;
        }
        #[cfg(not(unix))]
        cleanup_published_temp_link(path, identity)?;
        enforce_private_regular_file(&file)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            identity,
            keep: true,
        })
    }

    fn read_state(&mut self) -> Result<ResumeSidecar, AcquireError> {
        let metadata = self.file.metadata().map_err(staging_io)?;
        if metadata.len() == 0 || metadata.len() > MAX_RESUME_SIDECAR_BYTES {
            return Err(AcquireError::InvalidResumeState(
                "sidecar is empty or exceeds its byte limit".to_string(),
            ));
        }
        self.file.seek(SeekFrom::Start(0)).map_err(staging_io)?;
        serde_json::from_reader(Read::by_ref(&mut self.file)).map_err(|error| {
            AcquireError::InvalidResumeState(format!("sidecar JSON is invalid: {error}"))
        })
    }

    fn replace_state(&mut self, state: &ResumeSidecar) -> Result<(), AcquireError> {
        let encoded = encode_resume_sidecar(state)?;
        let replacement = PrivateSidecarTemp::create(&self.path, &encoded)?;
        let path = self.path.clone();
        self.ensure_current()?;
        let published = replacement.publish_replace(&path, self.identity)?;
        *self = published;
        Ok(())
    }

    fn ensure_current(&self) -> Result<(), AcquireError> {
        if self.identity.matches_path(&self.path)? {
            Ok(())
        } else {
            Err(AcquireError::CleanupIdentityChanged)
        }
    }

    fn remove_now(&mut self) -> Result<(), AcquireError> {
        if let Err(error) = self.ensure_current() {
            self.keep = true;
            return Err(error);
        }
        fs::remove_file(&self.path).map_err(staging_io)?;
        self.keep = true;
        sync_parent_directory(&self.path)?;
        Ok(())
    }
}

fn encode_resume_sidecar(state: &ResumeSidecar) -> Result<Vec<u8>, AcquireError> {
    let mut encoded = serde_json::to_vec(state)
        .map_err(|error| AcquireError::StagingIo(io::Error::other(error)))?;
    encoded.push(b'\n');
    let bytes = u64::try_from(encoded.len()).map_err(|_| AcquireError::IntegerOverflow)?;
    if bytes > MAX_RESUME_SIDECAR_BYTES {
        return Err(AcquireError::InvalidResumeState(
            "encoded sidecar exceeds its byte limit".to_string(),
        ));
    }
    Ok(encoded)
}

#[derive(Debug)]
struct PrivateSidecarTemp {
    path: PathBuf,
    file: Option<File>,
    identity: FileIdentity,
    keep: bool,
}

impl PrivateSidecarTemp {
    fn create(target_path: &Path, encoded: &[u8]) -> Result<Self, AcquireError> {
        for _attempt in 0..MAX_SIDECAR_TEMP_ATTEMPTS {
            let path = resume_temp_path(target_path)?;
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let file = match options.open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(staging_io(error)),
            };
            let identity = FileIdentity::from_file(&file)?;
            let mut temp = Self {
                path,
                file: Some(file),
                identity,
                keep: false,
            };
            enforce_private_regular_file(
                temp.file.as_ref().ok_or_else(|| {
                    staging_io(io::Error::other("resume temp file is unavailable"))
                })?,
            )?;
            let file = temp
                .file
                .as_mut()
                .ok_or_else(|| staging_io(io::Error::other("resume temp file is unavailable")))?;
            file.write_all(encoded).map_err(staging_io)?;
            file.flush().map_err(staging_io)?;
            file.sync_all().map_err(staging_io)?;
            return Ok(temp);
        }
        Err(AcquireError::InvalidResumeState(
            "could not allocate a private resume sidecar temp file".to_string(),
        ))
    }

    fn publish_new(mut self, target_path: &Path) -> Result<SidecarFile, AcquireError> {
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| staging_io(io::Error::other("resume temp file is unavailable")))?
            .try_clone()
            .map_err(staging_io)?;
        fs::hard_link(&self.path, target_path).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                AcquireError::OrphanedResumeSidecar
            } else {
                staging_io(error)
            }
        })?;
        if let Err(error) = sync_parent_directory(target_path) {
            let _cleanup_result = remove_identity_bound_path(target_path, self.identity);
            return Err(error);
        }
        if !self.identity.matches_path(target_path)? {
            return Err(AcquireError::CleanupIdentityChanged);
        }

        // The published link is durable. Temp cleanup is best-effort and
        // identity-bound; failure must not invalidate the usable sidecar.
        let _temp_cleanup = self.remove_now();
        Ok(SidecarFile {
            path: target_path.to_path_buf(),
            file,
            identity: self.identity,
            keep: false,
        })
    }

    fn publish_replace(
        mut self,
        target_path: &Path,
        expected_identity: FileIdentity,
    ) -> Result<SidecarFile, AcquireError> {
        if !expected_identity.matches_path(target_path)? {
            return Err(AcquireError::CleanupIdentityChanged);
        }
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| staging_io(io::Error::other("resume temp file is unavailable")))?
            .try_clone()
            .map_err(staging_io)?;
        fs::rename(&self.path, target_path).map_err(staging_io)?;
        self.keep = true;
        sync_parent_directory(target_path)?;
        if !self.identity.matches_path(target_path)? {
            return Err(AcquireError::CleanupIdentityChanged);
        }
        Ok(SidecarFile {
            path: target_path.to_path_buf(),
            file,
            identity: self.identity,
            keep: false,
        })
    }

    fn remove_now(&mut self) -> Result<(), AcquireError> {
        remove_identity_bound_path(&self.path, self.identity)?;
        self.keep = true;
        Ok(())
    }
}

impl Drop for PrivateSidecarTemp {
    fn drop(&mut self) {
        if !self.keep && self.identity.matches_path(&self.path).unwrap_or(false) {
            let _ignored = fs::remove_file(&self.path);
        }
    }
}

fn resume_temp_path(target_path: &Path) -> Result<PathBuf, AcquireError> {
    let file_name = target_path.file_name().ok_or_else(|| {
        AcquireError::InvalidResumeState("resume sidecar path has no final file name".to_string())
    })?;
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(
        ".information-native-tmp-{}-{}",
        std::process::id(),
        NEXT_SIDECAR_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let parent = target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(parent.join(temp_name))
}

#[cfg(unix)]
fn cleanup_published_temp_link(
    target_path: &Path,
    identity: FileIdentity,
) -> Result<(), AcquireError> {
    let file_name = target_path.file_name().ok_or_else(|| {
        AcquireError::InvalidResumeState("resume sidecar path has no final file name".to_string())
    })?;
    let mut prefix = Vec::with_capacity(file_name.as_bytes().len().saturating_add(32));
    prefix.push(b'.');
    prefix.extend_from_slice(file_name.as_bytes());
    prefix.extend_from_slice(b".information-native-tmp-");
    let parent = target_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut removed = false;
    for (index, entry) in fs::read_dir(parent).map_err(staging_io)?.enumerate() {
        if index >= MAX_SIDECAR_DIRECTORY_ENTRIES {
            return Err(AcquireError::InvalidResumeState(
                "resume sidecar directory exceeds its recovery scan limit".to_string(),
            ));
        }
        let entry = entry.map_err(staging_io)?;
        if entry.file_name().as_bytes().starts_with(&prefix)
            && identity.matches_path(&entry.path())?
        {
            fs::remove_file(entry.path()).map_err(staging_io)?;
            removed = true;
        }
    }
    if removed {
        sync_parent_directory(target_path)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn cleanup_published_temp_link(
    _target_path: &Path,
    _identity: FileIdentity,
) -> Result<(), AcquireError> {
    Err(AcquireError::DurableResumeUnsupportedOnPlatform)
}

fn remove_identity_bound_path(path: &Path, identity: FileIdentity) -> Result<(), AcquireError> {
    if !identity.matches_path(path)? {
        return Err(AcquireError::CleanupIdentityChanged);
    }
    fs::remove_file(path).map_err(staging_io)?;
    sync_parent_directory(path)
}

impl Drop for SidecarFile {
    fn drop(&mut self) {
        if !self.keep && self.identity.matches_path(&self.path).unwrap_or(false) {
            let _ignored = fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl FileIdentity {
    fn from_file(file: &File) -> Result<Self, AcquireError> {
        let metadata = file.metadata().map_err(staging_io)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    fn matches_path(self, path: &Path) -> Result<bool, AcquireError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => Ok(metadata.file_type().is_file()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(staging_io(error)),
        }
    }
}

#[cfg(not(unix))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct FileIdentity;

#[cfg(not(unix))]
impl FileIdentity {
    fn from_file(_file: &File) -> Result<Self, AcquireError> {
        Ok(Self)
    }

    fn matches_path(self, _path: &Path) -> Result<bool, AcquireError> {
        // The safe standard library does not expose a stable file identity on
        // every platform. Fail closed by retaining the path instead of risking
        // deletion of a replacement file.
        Ok(false)
    }
}

fn parse_source_uri(source_uri: &str) -> Result<FetchSource, AcquireError> {
    let url = Url::parse(source_uri).map_err(|_| AcquireError::InvalidSourceUri)?;
    match url.scheme() {
        "http" | "https" => {
            validate_http_url(&url)?;
            Ok(FetchSource::Http(url))
        }
        "file" => {
            if url.query().is_some() || url.fragment().is_some() {
                return Err(AcquireError::InvalidFileUri);
            }
            let uri = url.to_string();
            let path = url
                .to_file_path()
                .map_err(|()| AcquireError::InvalidFileUri)?;
            Ok(FetchSource::File { path, uri })
        }
        scheme => Err(AcquireError::UnsupportedScheme(scheme.to_string())),
    }
}

fn validate_http_url(url: &Url) -> Result<(), AcquireError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(AcquireError::UnsupportedScheme(url.scheme().to_string()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AcquireError::CredentialsForbidden);
    }
    if url.host().is_none() {
        return Err(AcquireError::MissingNetworkHost);
    }
    Ok(())
}

fn validate_artifact_url_metadata(url: &Url) -> Result<(), AcquireError> {
    if url.query().is_some() || url.fragment().is_some() {
        Err(AcquireError::ArtifactQueryOrFragmentForbidden)
    } else {
        Ok(())
    }
}

fn validate_redirect(current: &Url, next: &Url) -> Result<(), AcquireError> {
    if !matches!(next.scheme(), "http" | "https") {
        return Err(AcquireError::RedirectSchemeForbidden(
            next.scheme().to_string(),
        ));
    }
    if current.scheme() == "https" && next.scheme() == "http" {
        return Err(AcquireError::HttpsDowngradeRedirect);
    }
    validate_http_url(next)
}

fn is_followable_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

async fn resolve_destination(
    url: &Url,
    policy: &AcquisitionPolicy,
    deadline: Instant,
    total_timeout: Duration,
) -> Result<ResolvedDestination, AcquireError> {
    let port = url
        .port_or_known_default()
        .ok_or(AcquireError::MissingNetworkPort)?;
    let host = url.host().ok_or(AcquireError::MissingNetworkHost)?;
    match host {
        Host::Ipv4(address) => {
            validate_address(IpAddr::V4(address), policy)?;
            Ok(ResolvedDestination {
                dns_host: None,
                addresses: vec![SocketAddr::new(IpAddr::V4(address), port)],
            })
        }
        Host::Ipv6(address) => {
            validate_address(IpAddr::V6(address), policy)?;
            Ok(ResolvedDestination {
                dns_host: None,
                addresses: vec![SocketAddr::new(IpAddr::V6(address), port)],
            })
        }
        Host::Domain(domain) => {
            let host = domain.to_string();
            let remaining = remaining_transfer_time(deadline)?;
            let resolved = tokio::time::timeout(
                remaining,
                tokio::net::lookup_host((domain.to_string(), port)),
            )
            .await
            .map_err(|_| AcquireError::TotalTransferTimeout {
                timeout: total_timeout,
            })?
            .map_err(|error| AcquireError::DnsResolution {
                host: host.clone(),
                message: error.to_string(),
            })?;
            let mut addresses = Vec::new();
            let mut seen = HashSet::new();
            for address in resolved {
                validate_address(address.ip(), policy)?;
                if seen.insert(address) {
                    if addresses.len() >= MAX_RESOLVED_ADDRESSES {
                        return Err(AcquireError::TooManyResolvedAddresses {
                            host,
                            max_addresses: MAX_RESOLVED_ADDRESSES,
                        });
                    }
                    addresses.push(address);
                }
            }
            if addresses.is_empty() {
                return Err(AcquireError::DnsResolutionEmpty { host });
            }
            Ok(ResolvedDestination {
                dns_host: Some(domain.to_string()),
                addresses,
            })
        }
    }
}

fn validate_address(address: IpAddr, policy: &AcquisitionPolicy) -> Result<(), AcquireError> {
    if policy.permits_address(address) {
        Ok(())
    } else {
        Err(AcquireError::NetworkDestinationForbidden { address })
    }
}

fn validate_connected_peer(
    response: &Response,
    destination: &ResolvedDestination,
    policy: &AcquisitionPolicy,
) -> Result<String, AcquireError> {
    let peer = response
        .remote_addr()
        .ok_or(AcquireError::PeerAddressUnavailable)?;
    validate_address(peer.ip(), policy)?;
    if !destination
        .addresses
        .iter()
        .any(|resolved| resolved.ip() == peer.ip())
    {
        return Err(AcquireError::PeerAddressMismatch { peer: peer.ip() });
    }
    Ok(peer.to_string())
}

fn is_publicly_routable(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, d] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && c == 0 && d != 9 && d != 10)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    let globally_allocated = (segments[0] & 0xe000) == 0x2000;
    let ietf_special = segments[0] == 0x2001
        && (segments[1] == 0
            || (segments[1] == 2 && segments[2] == 0)
            || segments[1] == 0x0db8
            || (0x20..=0x3f).contains(&segments[1]));
    let six_to_four = segments[0] == 0x2002;
    let documentation_3fff = segments[0] == 0x3fff && (segments[1] & 0xf000) == 0;
    globally_allocated && !ietf_special && !six_to_four && !documentation_3fff
}

fn reject_encoded_response(response: &Response) -> Result<(), AcquireError> {
    for value in response.headers().get_all(CONTENT_ENCODING) {
        let value = value
            .to_str()
            .map_err(|_| AcquireError::EncodedResponseForbidden)?;
        if !value.trim().eq_ignore_ascii_case("identity") {
            return Err(AcquireError::EncodedResponseForbidden);
        }
    }
    Ok(())
}

fn require_fresh_response(response: &Response, expected_bytes: u64) -> Result<(), AcquireError> {
    if !response.status().is_success() {
        return Err(AcquireError::HttpStatus(response.status().as_u16()));
    }
    if response.status() == StatusCode::PARTIAL_CONTENT {
        return Err(AcquireError::UnexpectedPartialResponse);
    }
    if let Some(declared) = response.content_length()
        && declared != expected_bytes
    {
        return Err(AcquireError::ContentLengthMismatch {
            declared,
            expected: expected_bytes,
        });
    }
    Ok(())
}

fn validate_resumed_response(
    response: &Response,
    offset: u64,
    expected_bytes: u64,
    validator: &HttpValidator,
) -> Result<(), AcquireError> {
    if !validator.matches_response(response) {
        return Err(AcquireError::ResumeValidatorMismatch);
    }
    let value = response
        .headers()
        .get(CONTENT_RANGE)
        .ok_or(AcquireError::InvalidContentRange)?
        .to_str()
        .map_err(|_| AcquireError::InvalidContentRange)?;
    let expected_end = expected_bytes
        .checked_sub(1)
        .ok_or(AcquireError::InvalidContentRange)?;
    let (start, end, total) = parse_content_range(value)?;
    if start != offset || end != expected_end || total != expected_bytes {
        return Err(AcquireError::InvalidContentRange);
    }
    let remaining = expected_bytes
        .checked_sub(offset)
        .ok_or(AcquireError::InvalidContentRange)?;
    if let Some(declared) = response.content_length()
        && declared != remaining
    {
        return Err(AcquireError::ContentLengthMismatch {
            declared,
            expected: remaining,
        });
    }
    Ok(())
}

fn parse_content_range(value: &str) -> Result<(u64, u64, u64), AcquireError> {
    let value = value
        .strip_prefix("bytes ")
        .ok_or(AcquireError::InvalidContentRange)?;
    let (range, total) = value
        .split_once('/')
        .ok_or(AcquireError::InvalidContentRange)?;
    let (start, end) = range
        .split_once('-')
        .ok_or(AcquireError::InvalidContentRange)?;
    let start = start
        .parse::<u64>()
        .map_err(|_| AcquireError::InvalidContentRange)?;
    let end = end
        .parse::<u64>()
        .map_err(|_| AcquireError::InvalidContentRange)?;
    let total = total
        .parse::<u64>()
        .map_err(|_| AcquireError::InvalidContentRange)?;
    if start > end {
        return Err(AcquireError::InvalidContentRange);
    }
    Ok((start, end, total))
}

fn resumable_validator(response: &Response) -> Option<HttpValidator> {
    if let Some(value) = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_strong_etag(value))
    {
        return Some(HttpValidator::StrongEtag(value.to_string()));
    }
    response
        .headers()
        .get(LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .map(|value| HttpValidator::LastModified(value.to_string()))
}

fn is_strong_etag(value: &str) -> bool {
    let value = value.trim();
    value.len() >= 2
        && value.starts_with('"')
        && value.ends_with('"')
        && !value
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("w/"))
}

fn accepts_byte_ranges(response: &Response) -> bool {
    response
        .headers()
        .get_all(ACCEPT_RANGES)
        .iter()
        .any(|value| {
            value.to_str().ok().is_some_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("bytes"))
            })
        })
}

fn validate_expectation(
    expected_bytes: u64,
    expected_sha256: &str,
    max_bytes: u64,
) -> Result<(), AcquireError> {
    validate_nonzero_limit(max_bytes)?;
    if expected_bytes > max_bytes {
        return Err(AcquireError::LimitExceeded { max_bytes });
    }
    canonical_sha256(expected_sha256).map(|_| ())
}

fn validate_nonzero_limit(max_bytes: u64) -> Result<(), AcquireError> {
    if max_bytes == 0 {
        return Err(AcquireError::InvalidConfig(
            "byte limit must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

fn canonical_sha256(value: &str) -> Result<String, AcquireError> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AcquireError::InvalidExpectedDigest);
    }
    Ok(digest.to_ascii_lowercase())
}

#[allow(clippy::too_many_arguments)]
fn stream_verified_file(
    source: &mut dyn Read,
    staging_path: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
    max_bytes: u64,
    attestation: Option<SourceAttestation>,
    started_at_unix_ms: u64,
    progress: &mut dyn ProgressCallback,
) -> Result<VerifiedFetch, AcquireError> {
    let expected_digest = canonical_sha256(expected_sha256)?;
    let mut staging = StagingFile::create(staging_path)?;
    let mut hasher = Sha256::new();
    let mut received = 0_u64;
    notify_progress(
        progress,
        TransferProgress {
            phase: TransferPhase::Starting,
            downloaded_bytes: 0,
            expected_bytes,
            resumed_bytes: 0,
        },
    )?;
    let mut buffer = [0_u8; STREAM_BUFFER_BYTES];
    loop {
        let read = source.read(&mut buffer).map_err(source_io)?;
        if read == 0 {
            break;
        }
        received = received
            .checked_add(u64::try_from(read).map_err(|_| AcquireError::IntegerOverflow)?)
            .ok_or(AcquireError::IntegerOverflow)?;
        if received > max_bytes {
            return Err(AcquireError::LimitExceeded { max_bytes });
        }
        if received > expected_bytes {
            return Err(AcquireError::LengthMismatch {
                expected: expected_bytes,
                actual: received,
            });
        }
        hasher.update(&buffer[..read]);
        staging
            .file
            .write_all(&buffer[..read])
            .map_err(staging_io)?;
        notify_progress(
            progress,
            TransferProgress {
                phase: TransferPhase::Downloading,
                downloaded_bytes: received,
                expected_bytes,
                resumed_bytes: 0,
            },
        )?;
    }
    if received != expected_bytes {
        return Err(AcquireError::LengthMismatch {
            expected: expected_bytes,
            actual: received,
        });
    }
    let actual_digest = hex::encode(hasher.finalize());
    if actual_digest != expected_digest {
        return Err(AcquireError::DigestMismatch {
            expected: expected_digest,
            actual: actual_digest,
        });
    }
    staging.finish()?;
    let _control = progress.on_progress(TransferProgress {
        phase: TransferPhase::Complete,
        downloaded_bytes: received,
        expected_bytes,
        resumed_bytes: 0,
    });
    let final_source_uri = attestation
        .as_ref()
        .map(|attestation| attestation.final_uri.clone());
    let redirects = attestation
        .as_ref()
        .map_or(0, |attestation| attestation.redirect_chain.len());
    let finished_at_unix_ms = unix_time_millis()?;
    let source_attestations = attestation
        .clone()
        .map(|source| SourceAttemptAttestation {
            source,
            byte_start: 0,
            byte_end: received,
            started_at_unix_ms,
            finished_at_unix_ms: Some(finished_at_unix_ms),
        })
        .into_iter()
        .collect();
    Ok(VerifiedFetch {
        bytes: received,
        sha256: actual_digest,
        network_used: false,
        final_source_uri,
        redirects,
        source_attestation: attestation,
        source_attestations,
        started_at_unix_ms,
        finished_at_unix_ms,
        resumed_bytes: 0,
    })
}

fn verified_from_attestations(
    bytes: u64,
    sha256: String,
    source_attestations: Vec<SourceAttemptAttestation>,
    started_at_unix_ms: u64,
    resumed_bytes: u64,
) -> Result<VerifiedFetch, AcquireError> {
    verified_from_attestations_at(
        bytes,
        sha256,
        source_attestations,
        started_at_unix_ms,
        unix_time_millis()?,
        resumed_bytes,
    )
}

fn verified_from_attestations_at(
    bytes: u64,
    sha256: String,
    source_attestations: Vec<SourceAttemptAttestation>,
    started_at_unix_ms: u64,
    finished_at_unix_ms: u64,
    resumed_bytes: u64,
) -> Result<VerifiedFetch, AcquireError> {
    let attestation = source_attestations
        .last()
        .map(|attempt| attempt.source.clone())
        .ok_or_else(|| {
            AcquireError::InvalidResumeState(
                "verified network fetch has no source attempt history".to_string(),
            )
        })?;
    Ok(VerifiedFetch {
        bytes,
        sha256,
        network_used: true,
        final_source_uri: Some(attestation.final_uri.clone()),
        redirects: attestation.redirect_chain.len(),
        source_attestation: Some(attestation),
        source_attestations,
        started_at_unix_ms,
        finished_at_unix_ms,
        resumed_bytes,
    })
}

fn notify_progress(
    callback: &mut dyn ProgressCallback,
    progress: TransferProgress,
) -> Result<(), AcquireError> {
    if callback.on_progress(progress) == ProgressControl::Cancel {
        Err(AcquireError::Cancelled {
            downloaded_bytes: progress.downloaded_bytes,
        })
    } else {
        Ok(())
    }
}

fn read_bounded(source: &mut dyn Read, max_bytes: u64) -> Result<Vec<u8>, AcquireError> {
    let initial_capacity =
        usize::try_from(max_bytes.min(1024 * 1024)).map_err(|_| AcquireError::IntegerOverflow)?;
    let mut bytes = Vec::with_capacity(initial_capacity);
    let mut received = 0_u64;
    let mut buffer = [0_u8; STREAM_BUFFER_BYTES];
    loop {
        let read = source.read(&mut buffer).map_err(source_io)?;
        if read == 0 {
            break;
        }
        received = received
            .checked_add(u64::try_from(read).map_err(|_| AcquireError::IntegerOverflow)?)
            .ok_or(AcquireError::IntegerOverflow)?;
        if received > max_bytes {
            return Err(AcquireError::LimitExceeded { max_bytes });
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    Ok(bytes)
}

fn enforce_private_regular_file(file: &File) -> Result<(), AcquireError> {
    enforce_private_file_mode(file)?;
    #[cfg(unix)]
    if file.metadata().map_err(staging_io)?.nlink() != 1 {
        return Err(AcquireError::UnsafeStagingFile);
    }
    Ok(())
}

fn enforce_private_file_mode(file: &File) -> Result<(), AcquireError> {
    let metadata = file.metadata().map_err(staging_io)?;
    if !metadata.is_file() {
        return Err(AcquireError::UnsafeStagingFile);
    }
    #[cfg(unix)]
    if metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.permissions().mode() & 0o600 != 0o600
    {
        return Err(AcquireError::UnsafeStagingFile);
    }
    Ok(())
}

#[cfg(unix)]
fn open_existing_private_file(path: &Path, writable: bool) -> Result<File, AcquireError> {
    let access = if writable {
        rustix::fs::OFlags::RDWR
    } else {
        rustix::fs::OFlags::RDONLY
    };
    match rustix::fs::open(
        path,
        access | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    ) {
        Ok(file) => Ok(File::from(file)),
        Err(rustix::io::Errno::LOOP) => Err(AcquireError::UnsafeStagingFile),
        Err(error) => Err(staging_io(io::Error::from(error))),
    }
}

#[cfg(not(unix))]
fn open_existing_private_file(path: &Path, writable: bool) -> Result<File, AcquireError> {
    OpenOptions::new()
        .read(true)
        .write(writable)
        .open(path)
        .map_err(staging_io)
}

fn open_file_matches_path(file: &File, path: &Path) -> Result<bool, AcquireError> {
    let opened = file.metadata().map_err(file_policy_io)?;
    let current = fs::symlink_metadata(path).map_err(file_policy_io)?;
    if !opened.is_file() || !current.file_type().is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        Ok(opened.dev() == current.dev() && opened.ino() == current.ino())
    }
    #[cfg(not(unix))]
    {
        // A second canonicalization in `open_file_uri` is the strongest
        // identity recheck available in portable safe std.
        Ok(true)
    }
}

fn path_exists(path: &Path) -> Result<bool, AcquireError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(staging_io(error)),
    }
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), AcquireError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(staging_io)
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<(), AcquireError> {
    Ok(())
}

fn transfer_deadline(timeout: Duration) -> Result<Instant, AcquireError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or(AcquireError::InvalidConfig(
            "total transfer timeout exceeds the monotonic clock".to_string(),
        ))
}

fn remaining_transfer_time(deadline: Instant) -> Result<Duration, AcquireError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(AcquireError::TotalTransferTimeout {
            timeout: Duration::ZERO,
        })
    } else {
        Ok(remaining)
    }
}

fn network_error(error: reqwest::Error) -> AcquireError {
    AcquireError::Network(error.without_url().to_string())
}

fn source_io(error: io::Error) -> AcquireError {
    AcquireError::SourceIo(error)
}

fn staging_io(error: io::Error) -> AcquireError {
    AcquireError::StagingIo(error)
}

fn file_policy_io(error: io::Error) -> AcquireError {
    AcquireError::FilePolicyIo(error)
}

fn unix_time_millis() -> Result<u64, AcquireError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AcquireError::SystemClock(error.to_string()))?;
    u64::try_from(duration.as_millis()).map_err(|_| AcquireError::IntegerOverflow)
}

#[cfg(test)]
mod tests;
