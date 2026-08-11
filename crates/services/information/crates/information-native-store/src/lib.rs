#![forbid(unsafe_code)]

//! Local staging, atomic activation, and read-only external registration.
//!
//! This crate deliberately has no network client or process authority. Callers
//! put bytes into paths returned by [`ManagedStore::prepare_install`]. The store
//! independently checks every declared length and SHA-256 digest immediately
//! before and after the same-filesystem rename that activates a package.
//!
//! Install plan fingerprints are SHA-256 over the compact `serde_json` encoding
//! of the complete [`InstallPlan`] after replacing `plan_sha256` with exactly 64
//! ASCII zeroes. [`compute_plan_sha256`] delegates to the contract crate's
//! canonical implementation so planners and stores cannot drift.

use chrono::{DateTime, Utc};
use fs2::FileExt;
use information_native_types::{
    AcquisitionTransport, ArtifactAcquisition, ArtifactId, ContractError,
    EXTERNAL_REGISTRATION_SCHEMA, ExternalAccessMode, ExternalRegistration, INSTALL_RECEIPT_SCHEMA,
    InstallPlan, InstallReceipt, InstallationId, InstallationState, InstalledArtifact, Provenance,
    ReleaseId, RepresentationFormat, RepresentationId, ResourceId, RightsStatement, SourceIdentity,
    UsePolicy,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

const REGISTRY_EVENT_SCHEMA: &str = "information_native.store_event.v1";
// v2 moves acquisition facts out of the mutable manifest and into immutable,
// per-artifact journal entries. Reusing the v1 tag would make the on-disk
// recovery contract ambiguous across upgrades.
const STAGING_SCHEMA: &str = "information_native.staging.v2";
const ACQUISITION_JOURNAL_SCHEMA: &str = "information_native.acquisition_journal.v1";
const HASH_BUFFER_BYTES: usize = 1024 * 1024;
const MINIMUM_FREE_SPACE_RESERVE: u64 = 64 * 1024 * 1024;
static PROCESS_LOCKS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();

/// Failures returned by local store operations.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("contract rejected by the store: {0}")]
    Contract(#[from] ContractError),
    #[error("could not {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not encode or decode {context}: {source}")]
    Json {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("managed path is unsafe: {path} ({reason})")]
    UnsafePath { path: PathBuf, reason: &'static str },
    #[error("the managed store is already locked by another operation")]
    StoreBusy,
    #[error("installation {0} is already being acquired or activated")]
    InstallationBusy(InstallationId),
    #[error("install plan fingerprint mismatch: expected {expected}, observed {observed}")]
    PlanFingerprintMismatch { expected: String, observed: String },
    #[error("installation identifier is invalid for managed state: {0:?}")]
    InvalidInstallationId(String),
    #[error("installation {0} is already ready")]
    AlreadyInstalled(InstallationId),
    #[error("installation {0} has an activated package but no ready receipt; rerun activation")]
    ActivationIncomplete(InstallationId),
    #[error("installation {0} has not been staged")]
    StageNotFound(InstallationId),
    #[error("staged plan does not match the requested install plan")]
    StagedPlanMismatch,
    #[error("staged artifact set is invalid: {0}")]
    InvalidArtifactSet(String),
    #[error("artifact {artifact_id} has {observed} bytes; expected {expected}")]
    ArtifactSizeMismatch {
        artifact_id: String,
        expected: u64,
        observed: u64,
    },
    #[error("artifact {artifact_id} SHA-256 mismatch: expected {expected}, observed {observed}")]
    ArtifactDigestMismatch {
        artifact_id: String,
        expected: String,
        observed: String,
    },
    #[error("source changed while its identity was being observed: {0}")]
    SourceChanged(PathBuf),
    #[error("source is not a regular file: {0}")]
    SourceNotFile(PathBuf),
    #[error("immutable registration is blocked by a non-empty SQLite WAL: {0}")]
    NonEmptySqliteWal(PathBuf),
    #[error("immutable registration is blocked by a non-empty SQLite rollback journal: {0}")]
    NonEmptySqliteJournal(PathBuf),
    #[error("external source must be outside the managed store: {0}")]
    ExternalInsideManagedRoot(PathBuf),
    #[error("registration conflicts with existing installation {0}")]
    RegistrationConflict(InstallationId),
    #[error("registry is corrupt: {0}")]
    RegistryCorrupt(String),
    #[error("installation was not found: {0}")]
    InstallationNotFound(InstallationId),
    #[error("arithmetic overflow while accounting for local bytes")]
    IntegerOverflow,
    #[error("managed store has {available} bytes available; installation requires {required}")]
    InsufficientDiskSpace { available: u64, required: u64 },
}

impl StoreError {
    fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Transfer facts supplied by the acquisition authority. Logical payload bytes
/// are derived independently from the verified staged files by the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferSummary {
    pub started_at: DateTime<Utc>,
    pub acquisitions: Vec<ArtifactAcquisition>,
}

impl TransferSummary {
    #[must_use]
    pub fn for_plan(plan: &InstallPlan, started_at: DateTime<Utc>, _network_used: bool) -> Self {
        Self {
            started_at,
            acquisitions: plan
                .artifacts
                .iter()
                .map(|artifact| ArtifactAcquisition {
                    artifact_id: artifact.artifact_id.clone(),
                    transport: AcquisitionTransport::PreexistingStage,
                    requested_uri: None,
                    final_uri: None,
                    final_peer_address: None,
                    redirect_chain: Vec::new(),
                    attempts: Vec::new(),
                    started_at: None,
                    finished_at: Utc::now(),
                    resumed_bytes: 0,
                    verified_bytes: artifact.expected_bytes,
                    sha256: normalize_digest(&artifact.sha256),
                })
                .collect(),
        }
    }
}

/// One safe destination for an artifact declared by an install plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedArtifactTarget {
    pub artifact_id: information_native_types::ArtifactId,
    pub path: PathBuf,
    pub expected_bytes: u64,
    pub sha256: String,
}

/// An idempotently prepared, non-ready installation directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedInstall {
    pub installation_id: InstallationId,
    /// Durable start of this staging attempt. Reused across process restarts so
    /// terminal receipt timestamps cover every persisted acquisition.
    pub prepared_at: DateTime<Utc>,
    pub directory: PathBuf,
    pub artifacts: Vec<StagedArtifactTarget>,
}

impl PreparedInstall {
    #[must_use]
    pub fn artifact_target(
        &self,
        artifact_id: &information_native_types::ArtifactId,
    ) -> Option<&StagedArtifactTarget> {
        self.artifacts
            .iter()
            .find(|target| &target.artifact_id == artifact_id)
    }
}

/// Input for registering a caller-granted external file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRegistrationRequest {
    pub installation_id: InstallationId,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub format: RepresentationFormat,
    pub absolute_path: PathBuf,
    pub access_mode: ExternalAccessMode,
    pub provenance: Provenance,
    pub rights: Vec<RightsStatement>,
    pub use_policy: UsePolicy,
}

/// How much identity work to do for an external source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityStrength {
    /// Record length and modification time without hashing a potentially huge
    /// live database.
    Metadata,
    /// Record length, modification time, and SHA-256 while rejecting changes
    /// observed during the hashing pass.
    Sha256,
}

/// Result of comparing a source after a bounded operation with its earlier
/// identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityCheck {
    pub before: SourceIdentity,
    pub after: SourceIdentity,
    pub unchanged: bool,
}

/// A managed installation or external registration currently visible.
#[derive(Debug, Clone, PartialEq)]
pub enum RegisteredInstallation {
    Managed(Box<InstallReceipt>),
    External(Box<ExternalRegistration>),
}

impl RegisteredInstallation {
    #[must_use]
    pub fn installation_id(&self) -> &InstallationId {
        match self {
            Self::Managed(receipt) => &receipt.installation_id,
            Self::External(registration) => &registration.installation_id,
        }
    }
}

/// Durable store entries as of one locked read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoreSnapshot {
    pub managed: Vec<InstallReceipt>,
    pub external: Vec<ExternalRegistration>,
}

/// Result of an explicit full-byte verification. Ordinary listing performs a
/// cheap structural/size check; callers must request this before first use or
/// whenever the package identity changes.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FullVerification {
    pub installation_id: InstallationId,
    pub plan_sha256: String,
    pub verified_at: DateTime<Utc>,
    pub logical_bytes: u64,
    pub artifacts: Vec<InstalledArtifact>,
}

/// A non-ready directory retained after preparation or an interrupted
/// activation. These entries are never returned by [`ManagedStore::list`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialInstallState {
    Staged,
    ActivatedUnregistered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialInstall {
    pub installation_id: InstallationId,
    pub plan_sha256: String,
    pub state: PartialInstallState,
    pub absolute_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalKind {
    ManagedPackage,
    ExternalRegistrationOnly,
}

/// A read-only description of what an explicit removal command would affect.
/// Creating a plan never removes or unregisters anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalPlan {
    pub installation_id: InstallationId,
    pub kind: RemovalKind,
    pub managed_relative_paths: Vec<String>,
    pub external_source_preserved: Option<PathBuf>,
    pub observed_package_bytes: u64,
    pub requires_explicit_confirmation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct StagingManifest {
    schema: String,
    prepared_at: DateTime<Utc>,
    plan: InstallPlan,
    #[serde(default)]
    acquisitions: Vec<ArtifactAcquisition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcquisitionJournalEntry {
    schema: String,
    acquisition: ArtifactAcquisition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryEvent<T> {
    schema: String,
    revision: u64,
    recorded_at: DateTime<Utc>,
    value: T,
}

trait RegistryRecord {
    fn installation_id(&self) -> &InstallationId;
    fn validate_record(&self) -> Result<(), ContractError>;
}

impl RegistryRecord for InstallReceipt {
    fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }

    fn validate_record(&self) -> Result<(), ContractError> {
        self.validate()
    }
}

impl RegistryRecord for ExternalRegistration {
    fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }

    fn validate_record(&self) -> Result<(), ContractError> {
        self.validate()
    }
}

/// Filesystem authority for one local information resource root.
#[derive(Debug, Clone)]
pub struct ManagedStore {
    root: PathBuf,
}

impl ManagedStore {
    /// Create or open a managed root and its fixed internal layout.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let requested = root.as_ref();
        if requested.as_os_str().is_empty() {
            return Err(StoreError::UnsafePath {
                path: requested.to_path_buf(),
                reason: "empty managed root",
            });
        }
        create_private_dir_all(requested)?;
        reject_symlink(requested)?;
        let metadata = fs::metadata(requested)
            .map_err(|error| StoreError::io("inspect managed root", requested, error))?;
        if !metadata.is_dir() {
            return Err(StoreError::UnsafePath {
                path: requested.to_path_buf(),
                reason: "managed root is not a directory",
            });
        }
        let root = fs::canonicalize(requested)
            .map_err(|error| StoreError::io("canonicalize managed root", requested, error))?;
        let store = Self { root };
        enforce_private_directory(&store.root)?;
        store.ensure_layout()?;
        Ok(store)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Hold an installation-scoped cross-process lease across transfer,
    /// journaling, and activation. This prevents a competing process from
    /// mistaking an in-flight artifact for abandoned recovery state.
    pub fn acquire_install_lease(&self, plan: &InstallPlan) -> Result<InstallLease, StoreError> {
        validate_store_plan(plan)?;
        {
            let _store_lock = self.try_lock()?;
            self.ensure_layout()?;
        }
        let path = self
            .leases_root()
            .join(format!("{}.lock", installation_key(&plan.installation_id)?));
        let process_lock = ProcessLock::try_acquire(&path)
            .ok_or_else(|| StoreError::InstallationBusy(plan.installation_id.clone()))?;
        if path_exists(&path)? {
            reject_symlink(&path)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        set_private_create_mode(&mut options);
        let file = options
            .open(&path)
            .map_err(|error| StoreError::io("open installation lease", &path, error))?;
        let metadata = file
            .metadata()
            .map_err(|error| StoreError::io("inspect installation lease", &path, error))?;
        if !metadata.is_file() {
            return Err(StoreError::UnsafePath {
                path,
                reason: "installation lease is not a regular file",
            });
        }
        enforce_private_file(&path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(InstallLease {
                file,
                installation_id: plan.installation_id.clone(),
                _process_lock: process_lock,
            }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                Err(StoreError::InstallationBusy(plan.installation_id.clone()))
            }
            Err(error) => Err(StoreError::io("lock installation lease", &path, error)),
        }
    }

    /// Validate a plan and prepare its private staging directory. Repeating the
    /// call returns the same destinations without truncating partial artifacts.
    pub fn prepare_install(&self, plan: &InstallPlan) -> Result<PreparedInstall, StoreError> {
        validate_store_plan(plan)?;
        let _lock = self.try_lock()?;
        self.ensure_layout()?;
        if self
            .read_managed_receipt(&plan.installation_id)?
            .is_some_and(|receipt| receipt.state == InstallationState::Ready)
        {
            return Err(StoreError::AlreadyInstalled(plan.installation_id.clone()));
        }
        if self
            .read_external_registration(&plan.installation_id)?
            .is_some()
        {
            return Err(StoreError::RegistrationConflict(
                plan.installation_id.clone(),
            ));
        }

        let key = installation_key(&plan.installation_id)?;
        let stage = self.staging_root().join(&key);
        let package = self.packages_root().join(&key);
        if path_exists(&package)? {
            return Err(StoreError::ActivationIncomplete(
                plan.installation_id.clone(),
            ));
        }
        if path_exists(&stage)? {
            let manifest = read_staging_manifest(&stage)?;
            if manifest.plan != *plan {
                return Err(StoreError::StagedPlanMismatch);
            }
            cleanup_acquisition_temporaries(&stage.join("acquisitions"))?;
            return prepared_install(plan, stage, manifest.prepared_at);
        }

        let required = plan
            .total_download_bytes
            .max(plan.expected_installed_bytes)
            .checked_add(MINIMUM_FREE_SPACE_RESERVE)
            .ok_or(StoreError::IntegerOverflow)?;
        let available = fs2::available_space(&self.root).map_err(|error| {
            StoreError::io("inspect managed store free space", &self.root, error)
        })?;
        if available < required {
            return Err(StoreError::InsufficientDiskSpace {
                available,
                required,
            });
        }

        let temporary = self
            .staging_root()
            .join(format!(".prepare-{}", Uuid::new_v4()));
        create_private_dir(&temporary)?;
        let mut temporary_guard = TemporaryDirectoryGuard::new(temporary.clone());
        let artifacts = temporary.join("artifacts");
        create_private_dir(&artifacts)?;
        let acquisitions = temporary.join("acquisitions");
        create_private_dir(&acquisitions)?;
        let prepared_at = Utc::now();
        let manifest = StagingManifest {
            schema: STAGING_SCHEMA.to_string(),
            prepared_at,
            plan: plan.clone(),
            acquisitions: Vec::new(),
        };
        let manifest_path = temporary.join("stage.json");
        write_new_json(&manifest_path, &manifest, "staging manifest")?;
        sync_directory(&temporary)?;
        fs::rename(&temporary, &stage)
            .map_err(|error| StoreError::io("publish staging directory", &stage, error))?;
        temporary_guard.disarm();
        sync_directory(&self.staging_root())?;
        prepared_install(plan, stage, prepared_at)
    }

    /// Recheck staged artifacts, atomically rename the complete directory into
    /// `packages`, recheck it there, and publish a ready receipt.
    ///
    /// If the process previously stopped after the rename but before the
    /// receipt event, this method recognizes the package and safely completes
    /// the interrupted activation.
    pub fn activate(
        &self,
        plan: &InstallPlan,
        transfer: TransferSummary,
    ) -> Result<InstallReceipt, StoreError> {
        validate_store_plan(plan)?;
        let _lock = self.try_lock()?;
        self.ensure_layout()?;
        self.activate_locked(plan, transfer)
    }

    /// Finish the narrow crash window in which a verified staging directory
    /// was renamed into `packages` but its ready receipt was not published.
    /// Returns `None` when no activated package exists.
    pub fn recover_interrupted_activation(
        &self,
        plan: &InstallPlan,
    ) -> Result<Option<InstallReceipt>, StoreError> {
        validate_store_plan(plan)?;
        let _lock = self.try_lock()?;
        self.ensure_layout()?;
        let key = installation_key(&plan.installation_id)?;
        let package = self.packages_root().join(key);
        if !path_exists(&package)? {
            return Ok(None);
        }
        let manifest = read_staging_manifest(&package)?;
        if manifest.plan != *plan {
            return Err(StoreError::StagedPlanMismatch);
        }
        let started_at = manifest
            .acquisitions
            .iter()
            .flat_map(|acquisition| [acquisition.started_at, Some(acquisition.finished_at)])
            .flatten()
            .fold(manifest.prepared_at, std::cmp::min);
        self.activate_locked(
            plan,
            TransferSummary {
                started_at,
                acquisitions: Vec::new(),
            },
        )
        .map(Some)
    }

    fn activate_locked(
        &self,
        plan: &InstallPlan,
        transfer: TransferSummary,
    ) -> Result<InstallReceipt, StoreError> {
        if let Some(receipt) = self.read_managed_receipt(&plan.installation_id)? {
            if receipt.state == InstallationState::Ready {
                ensure_receipt_matches_plan(&receipt, plan)?;
                return Ok(receipt);
            }
            ensure_non_ready_receipt_matches_plan(&receipt, plan)?;
        }
        if self
            .read_external_registration(&plan.installation_id)?
            .is_some()
        {
            return Err(StoreError::RegistrationConflict(
                plan.installation_id.clone(),
            ));
        }

        let key = installation_key(&plan.installation_id)?;
        let stage = self.staging_root().join(&key);
        let package = self.packages_root().join(&key);
        let stage_exists = path_exists(&stage)?;
        let package_exists = path_exists(&package)?;
        match (stage_exists, package_exists) {
            (true, false) => {
                for acquisition in &transfer.acquisitions {
                    record_staged_acquisition_locked(plan, acquisition, &stage)?;
                }
                verify_package_directory(&stage, plan)?;
                fs::rename(&stage, &package)
                    .map_err(|error| StoreError::io("activate staged package", &package, error))?;
                // Persist the destination before the source removal. If the
                // second sync is interrupted, recovery may see both names but
                // must never prefer a durable state with neither name.
                sync_directory(&self.packages_root())?;
                sync_directory(&self.staging_root())?;
            }
            (false, true) => {}
            (false, false) => {
                return Err(StoreError::StageNotFound(plan.installation_id.clone()));
            }
            (true, true) => {
                return Err(StoreError::RegistryCorrupt(format!(
                    "installation {} exists in both staging and packages",
                    plan.installation_id
                )));
            }
        }

        let verified = verify_package_directory(&package, plan)?;
        let manifest_acquisitions = read_staging_manifest(&package)?.acquisitions;
        let acquisitions = if manifest_acquisitions.is_empty() {
            transfer.acquisitions
        } else {
            if !transfer.acquisitions.is_empty() && transfer.acquisitions != manifest_acquisitions {
                return Err(StoreError::InvalidArtifactSet(
                    "supplied acquisition records disagree with the staged journal".to_string(),
                ));
            }
            manifest_acquisitions
        };
        let verified_logical_bytes = verified.iter().try_fold(0_u64, |sum, artifact| {
            sum.checked_add(artifact.bytes)
                .ok_or(StoreError::IntegerOverflow)
        })?;
        validate_acquisition_records(plan, &verified, &acquisitions)?;
        let network_used = acquisitions.iter().any(|acquisition| {
            matches!(
                acquisition.transport,
                AcquisitionTransport::Http | AcquisitionTransport::Https
            )
        });
        let finished_at = acquisitions
            .iter()
            .map(|acquisition| acquisition.finished_at)
            .fold(Utc::now(), std::cmp::max);
        let receipt = InstallReceipt {
            schema: INSTALL_RECEIPT_SCHEMA.to_string(),
            installation_id: plan.installation_id.clone(),
            resource_id: plan.resource_id.clone(),
            release_id: plan.release_id.clone(),
            representation_id: plan.representation_id.clone(),
            format: plan.format.clone(),
            catalog_authority: plan.catalog_authority.clone(),
            resolved: plan.resolved.clone(),
            plan_sha256: plan.plan_sha256.clone(),
            state: InstallationState::Ready,
            started_at: transfer.started_at,
            finished_at: Some(finished_at),
            network_attempted: Some(network_used),
            network_used,
            downloaded_bytes: verified_logical_bytes,
            unverified_staged_bytes: 0,
            installed_relative_path: Some(format!("packages/{key}")),
            artifacts: verified
                .into_iter()
                .map(|artifact| InstalledArtifact {
                    artifact_id: artifact.artifact_id,
                    relative_path: format!("packages/{key}/artifacts/{}", artifact.file_name),
                    bytes: artifact.bytes,
                    sha256: artifact.sha256,
                })
                .collect(),
            acquisitions,
            failure: None,
        };
        receipt.validate()?;
        self.append_managed_event(&receipt)?;
        Ok(receipt)
    }

    /// Persist an inspectable failed or cancelled receipt without making an
    /// installation ready. A later successful activation appends a new event.
    pub fn record_non_ready_receipt(&self, receipt: &InstallReceipt) -> Result<(), StoreError> {
        receipt.validate()?;
        if receipt.state == InstallationState::Ready {
            return Err(StoreError::RegistryCorrupt(
                "record_non_ready_receipt received a ready receipt".to_string(),
            ));
        }
        validate_installation_id(&receipt.installation_id)?;
        let _lock = self.try_lock()?;
        if self
            .read_managed_receipt(&receipt.installation_id)?
            .is_some_and(|existing| existing.state == InstallationState::Ready)
        {
            return Err(StoreError::AlreadyInstalled(
                receipt.installation_id.clone(),
            ));
        }
        if self
            .read_external_registration(&receipt.installation_id)?
            .is_some()
        {
            return Err(StoreError::RegistrationConflict(
                receipt.installation_id.clone(),
            ));
        }
        self.append_managed_event(receipt)
    }

    /// Persist one verified acquisition attestation beside a staged plan. The
    /// journal survives a crash between transfer completion and activation.
    pub fn record_staged_acquisition(
        &self,
        plan: &InstallPlan,
        acquisition: &ArtifactAcquisition,
    ) -> Result<(), StoreError> {
        validate_store_plan(plan)?;
        acquisition.validate()?;
        let _lock = self.try_lock()?;
        let key = installation_key(&plan.installation_id)?;
        let stage = self.staging_root().join(key);
        record_staged_acquisition_locked(plan, acquisition, &stage)
    }

    pub fn staged_acquisitions(
        &self,
        plan: &InstallPlan,
    ) -> Result<Vec<ArtifactAcquisition>, StoreError> {
        validate_store_plan(plan)?;
        let _lock = self.try_lock()?;
        let stage = self
            .staging_root()
            .join(installation_key(&plan.installation_id)?);
        let manifest = read_staging_manifest(&stage)?;
        if manifest.plan != *plan {
            return Err(StoreError::StagedPlanMismatch);
        }
        Ok(manifest.acquisitions)
    }

    /// Register a canonical external file. Immutable registrations are hashed
    /// and reject a non-empty sibling `<database>-wal` both before and after
    /// hashing. Live registrations retain metadata identity but permit later
    /// changes and do not claim immutability.
    pub fn register_external(
        &self,
        request: &ExternalRegistrationRequest,
    ) -> Result<ExternalRegistration, StoreError> {
        validate_installation_id(&request.installation_id)?;
        if !request.absolute_path.is_absolute() {
            return Err(StoreError::UnsafePath {
                path: request.absolute_path.clone(),
                reason: "external registration path is not absolute",
            });
        }
        let canonical = fs::canonicalize(&request.absolute_path).map_err(|error| {
            StoreError::io(
                "canonicalize external source",
                &request.absolute_path,
                error,
            )
        })?;
        if canonical.starts_with(&self.root) {
            return Err(StoreError::ExternalInsideManagedRoot(canonical));
        }

        let strength = match request.access_mode {
            ExternalAccessMode::LiveReadOnly => IdentityStrength::Metadata,
            ExternalAccessMode::ImmutableReadOnly => IdentityStrength::Sha256,
        };
        let sqlite_snapshot = matches!(
            request.format.kind,
            information_native_types::FormatKind::AlexandriaSqlite
                | information_native_types::FormatKind::SqliteFts5
                | information_native_types::FormatKind::GeoPackage
        );
        if request.access_mode == ExternalAccessMode::ImmutableReadOnly && sqlite_snapshot {
            reject_nonempty_sqlite_sidecars(&canonical)?;
        }
        let identity = capture_source_identity(&canonical, strength)?;
        if request.access_mode == ExternalAccessMode::ImmutableReadOnly && sqlite_snapshot {
            reject_nonempty_sqlite_sidecars(&canonical)?;
        }
        let registration = ExternalRegistration {
            schema: EXTERNAL_REGISTRATION_SCHEMA.to_string(),
            installation_id: request.installation_id.clone(),
            resource_id: request.resource_id.clone(),
            release_id: request.release_id.clone(),
            representation_id: request.representation_id.clone(),
            format: request.format.clone(),
            absolute_path: canonical
                .to_str()
                .ok_or_else(|| StoreError::UnsafePath {
                    path: canonical.clone(),
                    reason: "external registration path is not valid UTF-8",
                })?
                .to_string(),
            access_mode: request.access_mode,
            identity,
            provenance: request.provenance.clone(),
            rights: request.rights.clone(),
            use_policy: request.use_policy,
            registered_at: Utc::now(),
        };
        registration.validate()?;

        let _lock = self.try_lock()?;
        if self
            .read_managed_receipt(&registration.installation_id)?
            .is_some()
        {
            return Err(StoreError::RegistrationConflict(
                registration.installation_id.clone(),
            ));
        }
        if let Some(existing) = self.read_external_registration(&registration.installation_id)? {
            if existing.absolute_path == registration.absolute_path
                && existing.access_mode == registration.access_mode
                && existing.resource_id == registration.resource_id
                && existing.release_id == registration.release_id
                && existing.representation_id == registration.representation_id
                && existing.format == registration.format
                && existing.provenance == registration.provenance
                && existing.rights == registration.rights
                && existing.use_policy == registration.use_policy
                && existing.identity == registration.identity
            {
                return Ok(existing);
            }
            return Err(StoreError::RegistrationConflict(
                registration.installation_id.clone(),
            ));
        }
        self.append_external_event(&registration)?;
        Ok(registration)
    }

    /// List ready managed installations and external registrations. Failed
    /// receipts and unregistered packages are intentionally excluded.
    pub fn list(&self) -> Result<StoreSnapshot, StoreError> {
        let _lock = self.try_lock()?;
        let mut managed = self.read_all_managed_receipts()?;
        managed.retain(|receipt| receipt.state == InstallationState::Ready);
        for receipt in &managed {
            self.validate_ready_package_record(receipt)?;
        }
        let external = self.read_all_external_registrations()?;
        Ok(StoreSnapshot { managed, external })
    }

    /// List the latest durable receipt for every managed installation id,
    /// including failed and cancelled attempts. Use [`Self::receipt_history`]
    /// for the complete append-only audit trail of one installation.
    pub fn list_receipts(&self) -> Result<Vec<InstallReceipt>, StoreError> {
        let _lock = self.try_lock()?;
        self.read_all_managed_receipts()
    }

    /// Return every durable receipt event for one installation in revision
    /// order. Temporary files from interrupted writes are ignored.
    pub fn receipt_history(
        &self,
        installation_id: &InstallationId,
    ) -> Result<Vec<InstallReceipt>, StoreError> {
        validate_installation_id(installation_id)?;
        let _lock = self.try_lock()?;
        let entry = self
            .installations_registry_root()
            .join(installation_key(installation_id)?);
        if !path_exists(&entry)? {
            return Ok(Vec::new());
        }
        ensure_real_directory(&entry)?;
        let mut events = Vec::new();
        for directory_entry in read_directory(&entry)? {
            let path = directory_entry.path();
            let Some(revision) = revision_from_path(&path)? else {
                continue;
            };
            let event: RegistryEvent<InstallReceipt> =
                read_json(&path, "managed installation receipt history")?;
            if event.schema != REGISTRY_EVENT_SCHEMA || event.revision != revision {
                return Err(StoreError::RegistryCorrupt(format!(
                    "registry event {} has an invalid schema or revision",
                    path.display()
                )));
            }
            event.value.validate()?;
            if event.value.installation_id != *installation_id {
                return Err(StoreError::RegistryCorrupt(format!(
                    "registry event {} contains the wrong installation id",
                    path.display()
                )));
            }
            events.push((revision, event.value));
        }
        events.sort_by_key(|(revision, _)| *revision);
        Ok(events.into_iter().map(|(_, receipt)| receipt).collect())
    }

    /// Rehash every artifact in one ready managed package and compare it with
    /// the frozen plan. This is intentionally explicit because archives may be
    /// tens or hundreds of gigabytes.
    pub fn verify_full(
        &self,
        installation_id: &InstallationId,
    ) -> Result<FullVerification, StoreError> {
        validate_installation_id(installation_id)?;
        let _lock = self.try_lock()?;
        let receipt = self
            .read_managed_receipt(installation_id)?
            .filter(|receipt| receipt.state == InstallationState::Ready)
            .ok_or_else(|| StoreError::InstallationNotFound(installation_id.clone()))?;
        let relative = receipt.installed_relative_path.as_deref().ok_or_else(|| {
            StoreError::RegistryCorrupt(format!(
                "ready receipt {} has no installed path",
                installation_id
            ))
        })?;
        let package = self.resolve_managed_relative_path(relative)?;
        let manifest = read_staging_manifest(&package)?;
        ensure_receipt_matches_plan(&receipt, &manifest.plan)?;
        let verified = verify_package_directory(&package, &manifest.plan)?;
        let logical_bytes = verified.iter().try_fold(0_u64, |sum, artifact| {
            sum.checked_add(artifact.bytes)
                .ok_or(StoreError::IntegerOverflow)
        })?;
        let key = installation_key(installation_id)?;
        Ok(FullVerification {
            installation_id: installation_id.clone(),
            plan_sha256: manifest.plan.plan_sha256,
            verified_at: Utc::now(),
            logical_bytes,
            artifacts: verified
                .into_iter()
                .map(|artifact| InstalledArtifact {
                    artifact_id: artifact.artifact_id,
                    relative_path: format!("packages/{key}/artifacts/{}", artifact.file_name),
                    bytes: artifact.bytes,
                    sha256: artifact.sha256,
                })
                .collect(),
        })
    }

    /// Read the latest managed receipt, including a non-ready failure receipt.
    pub fn get_receipt(
        &self,
        installation_id: &InstallationId,
    ) -> Result<Option<InstallReceipt>, StoreError> {
        validate_installation_id(installation_id)?;
        let _lock = self.try_lock()?;
        self.read_managed_receipt(installation_id)
    }

    /// Load the exact plan embedded in a ready managed package. This preserves
    /// format, rights, use policy, selection, and provenance-driving source
    /// URIs across process restarts without consulting a mutable catalogue.
    pub fn get_installed_plan(
        &self,
        installation_id: &InstallationId,
    ) -> Result<Option<InstallPlan>, StoreError> {
        validate_installation_id(installation_id)?;
        let _lock = self.try_lock()?;
        let Some(receipt) = self.read_managed_receipt(installation_id)? else {
            return Ok(None);
        };
        if receipt.state != InstallationState::Ready {
            return Ok(None);
        }
        self.validate_ready_package_record(&receipt)?;
        let relative = receipt.installed_relative_path.as_deref().ok_or_else(|| {
            StoreError::RegistryCorrupt(format!(
                "ready receipt {} has no installed path",
                installation_id
            ))
        })?;
        let package = self.resolve_managed_relative_path(relative)?;
        Ok(Some(read_staging_manifest(&package)?.plan))
    }

    /// Resolve one receipt-validated managed artifact to its canonical path.
    /// The returned path is guaranteed to remain beneath the managed root at
    /// the instant of this locked lookup.
    pub fn managed_artifact_path(
        &self,
        installation_id: &InstallationId,
        artifact_id: &ArtifactId,
    ) -> Result<PathBuf, StoreError> {
        validate_installation_id(installation_id)?;
        let _lock = self.try_lock()?;
        let receipt = self
            .read_managed_receipt(installation_id)?
            .filter(|receipt| receipt.state == InstallationState::Ready)
            .ok_or_else(|| StoreError::InstallationNotFound(installation_id.clone()))?;
        self.validate_ready_package_record(&receipt)?;
        let artifact = receipt
            .artifacts
            .iter()
            .find(|artifact| &artifact.artifact_id == artifact_id)
            .ok_or_else(|| {
                StoreError::RegistryCorrupt(format!(
                    "ready receipt {} has no artifact {}",
                    installation_id, artifact_id
                ))
            })?;
        self.resolve_managed_relative_path(&artifact.relative_path)
    }

    /// Look up a visible installation by its durable installation identifier.
    pub fn get(
        &self,
        installation_id: &InstallationId,
    ) -> Result<Option<RegisteredInstallation>, StoreError> {
        validate_installation_id(installation_id)?;
        let _lock = self.try_lock()?;
        if let Some(receipt) = self.read_managed_receipt(installation_id)?
            && receipt.state == InstallationState::Ready
        {
            self.validate_ready_package_record(&receipt)?;
            return Ok(Some(RegisteredInstallation::Managed(Box::new(receipt))));
        }
        Ok(self
            .read_external_registration(installation_id)?
            .map(Box::new)
            .map(RegisteredInstallation::External))
    }

    /// Discover cleanly staged and atomically activated-but-unregistered
    /// packages. Hidden temporary preparation files are ignored and never
    /// treated as valid state.
    pub fn list_partial_installs(&self) -> Result<Vec<PartialInstall>, StoreError> {
        let _lock = self.try_lock()?;
        let ready = self
            .read_all_managed_receipts()?
            .into_iter()
            .filter(|receipt| receipt.state == InstallationState::Ready)
            .map(|receipt| receipt.installation_id)
            .collect::<BTreeSet<_>>();
        let mut partial = Vec::new();
        self.collect_partial_from_directory(
            &self.staging_root(),
            PartialInstallState::Staged,
            &ready,
            &mut partial,
        )?;
        self.collect_partial_from_directory(
            &self.packages_root(),
            PartialInstallState::ActivatedUnregistered,
            &ready,
            &mut partial,
        )?;
        partial.sort_by(|left, right| left.installation_id.cmp(&right.installation_id));
        Ok(partial)
    }

    /// Describe removal scope without deleting or unregistering anything.
    pub fn plan_removal(
        &self,
        installation_id: &InstallationId,
    ) -> Result<RemovalPlan, StoreError> {
        validate_installation_id(installation_id)?;
        let _lock = self.try_lock()?;
        if let Some(receipt) = self.read_managed_receipt(installation_id)? {
            if receipt.state != InstallationState::Ready {
                return Err(StoreError::InstallationNotFound(installation_id.clone()));
            }
            let relative = receipt.installed_relative_path.clone().ok_or_else(|| {
                StoreError::RegistryCorrupt(format!(
                    "ready receipt {} has no installed path",
                    installation_id
                ))
            })?;
            let absolute = self.resolve_managed_relative_path(&relative)?;
            let observed_package_bytes = directory_bytes(&absolute)?;
            let registry_path = format!(
                "registry/installations/{}",
                installation_key(installation_id)?
            );
            return Ok(RemovalPlan {
                installation_id: installation_id.clone(),
                kind: RemovalKind::ManagedPackage,
                managed_relative_paths: vec![relative, registry_path],
                external_source_preserved: None,
                observed_package_bytes,
                requires_explicit_confirmation: true,
            });
        }
        if let Some(registration) = self.read_external_registration(installation_id)? {
            return Ok(RemovalPlan {
                installation_id: installation_id.clone(),
                kind: RemovalKind::ExternalRegistrationOnly,
                managed_relative_paths: vec![format!(
                    "registry/externals/{}",
                    installation_key(installation_id)?
                )],
                external_source_preserved: Some(PathBuf::from(registration.absolute_path)),
                observed_package_bytes: 0,
                requires_explicit_confirmation: true,
            });
        }
        Err(StoreError::InstallationNotFound(installation_id.clone()))
    }

    fn ensure_layout(&self) -> Result<(), StoreError> {
        ensure_real_directory(&self.root)?;
        for directory in [
            self.staging_root(),
            self.packages_root(),
            self.registry_root(),
            self.installations_registry_root(),
            self.externals_registry_root(),
            self.leases_root(),
        ] {
            if !path_exists(&directory)? {
                create_private_dir(&directory)?;
            }
            ensure_real_directory(&directory)?;
            enforce_private_directory(&directory)?;
        }
        Ok(())
    }

    fn try_lock(&self) -> Result<StoreLock, StoreError> {
        let path = self.root.join("store.lock");
        let process_lock = ProcessLock::try_acquire(&path).ok_or(StoreError::StoreBusy)?;
        if path_exists(&path)? {
            reject_symlink(&path)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        set_private_create_mode(&mut options);
        let file = options
            .open(&path)
            .map_err(|error| StoreError::io("open store lock", &path, error))?;
        let metadata = file
            .metadata()
            .map_err(|error| StoreError::io("inspect store lock", &path, error))?;
        if !metadata.is_file() {
            return Err(StoreError::UnsafePath {
                path,
                reason: "store lock is not a regular file",
            });
        }
        enforce_private_file(&path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(StoreLock {
                file,
                _process_lock: process_lock,
            }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Err(StoreError::StoreBusy),
            Err(error) => Err(StoreError::io("lock managed store", &path, error)),
        }
    }

    fn staging_root(&self) -> PathBuf {
        self.root.join("staging")
    }

    fn packages_root(&self) -> PathBuf {
        self.root.join("packages")
    }

    fn registry_root(&self) -> PathBuf {
        self.root.join("registry")
    }

    fn installations_registry_root(&self) -> PathBuf {
        self.registry_root().join("installations")
    }

    fn externals_registry_root(&self) -> PathBuf {
        self.registry_root().join("externals")
    }

    fn leases_root(&self) -> PathBuf {
        self.root.join("leases")
    }

    fn append_managed_event(&self, receipt: &InstallReceipt) -> Result<(), StoreError> {
        self.append_registry_event(
            &self.installations_registry_root(),
            &receipt.installation_id,
            receipt,
            "managed installation receipt",
        )
    }

    fn append_external_event(&self, registration: &ExternalRegistration) -> Result<(), StoreError> {
        self.append_registry_event(
            &self.externals_registry_root(),
            &registration.installation_id,
            registration,
            "external registration",
        )
    }

    fn append_registry_event<T: Serialize>(
        &self,
        category_root: &Path,
        installation_id: &InstallationId,
        value: &T,
        context: &'static str,
    ) -> Result<(), StoreError> {
        let entry = category_root.join(installation_key(installation_id)?);
        if !path_exists(&entry)? {
            create_private_dir(&entry)?;
            sync_directory(category_root)?;
        }
        ensure_real_directory(&entry)?;
        let revision = next_revision(&entry)?;
        let event = RegistryEvent {
            schema: REGISTRY_EVENT_SCHEMA.to_string(),
            revision,
            recorded_at: Utc::now(),
            value,
        };
        let final_path = entry.join(format!("{revision:020}.json"));
        let temporary = entry.join(format!(".{revision:020}-{}.tmp", Uuid::new_v4()));
        let mut temporary_guard = TemporaryFileGuard::new(temporary.clone());
        write_new_json(&temporary, &event, context)?;
        fs::rename(&temporary, &final_path)
            .map_err(|error| StoreError::io("publish registry event", &final_path, error))?;
        temporary_guard.disarm();
        sync_directory(&entry)
    }

    fn read_managed_receipt(
        &self,
        installation_id: &InstallationId,
    ) -> Result<Option<InstallReceipt>, StoreError> {
        self.read_latest_registry_event(
            &self.installations_registry_root(),
            installation_id,
            "managed installation receipt",
        )
    }

    fn read_external_registration(
        &self,
        installation_id: &InstallationId,
    ) -> Result<Option<ExternalRegistration>, StoreError> {
        self.read_latest_registry_event(
            &self.externals_registry_root(),
            installation_id,
            "external registration",
        )
    }

    fn read_latest_registry_event<T: for<'de> Deserialize<'de> + RegistryRecord>(
        &self,
        category_root: &Path,
        installation_id: &InstallationId,
        context: &'static str,
    ) -> Result<Option<T>, StoreError> {
        let entry = category_root.join(installation_key(installation_id)?);
        if !path_exists(&entry)? {
            return Ok(None);
        }
        ensure_real_directory(&entry)?;
        let Some((revision, path)) = latest_revision_path(&entry)? else {
            return Ok(None);
        };
        let event: RegistryEvent<T> = read_json(&path, context)?;
        if event.schema != REGISTRY_EVENT_SCHEMA || event.revision != revision {
            return Err(StoreError::RegistryCorrupt(format!(
                "registry event {} has an invalid schema or revision",
                path.display()
            )));
        }
        event.value.validate_record()?;
        if event.value.installation_id() != installation_id {
            return Err(StoreError::RegistryCorrupt(format!(
                "registry event {} contains installation {} instead of {}",
                path.display(),
                event.value.installation_id(),
                installation_id
            )));
        }
        Ok(Some(event.value))
    }

    fn read_all_managed_receipts(&self) -> Result<Vec<InstallReceipt>, StoreError> {
        let mut values: Vec<InstallReceipt> =
            self.read_all_registry_values(&self.installations_registry_root(), "managed receipt")?;
        values.sort_by(|left, right| left.installation_id.cmp(&right.installation_id));
        Ok(values)
    }

    fn read_all_external_registrations(&self) -> Result<Vec<ExternalRegistration>, StoreError> {
        let mut values: Vec<ExternalRegistration> = self
            .read_all_registry_values(&self.externals_registry_root(), "external registration")?;
        values.sort_by(|left, right| left.installation_id.cmp(&right.installation_id));
        Ok(values)
    }

    fn read_all_registry_values<T: for<'de> Deserialize<'de> + RegistryRecord>(
        &self,
        category_root: &Path,
        context: &'static str,
    ) -> Result<Vec<T>, StoreError> {
        ensure_real_directory(category_root)?;
        let mut values = Vec::new();
        for entry in read_directory(category_root)? {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|error| StoreError::io("inspect registry entry", &entry.path(), error))?;
            if file_type.is_symlink() || !file_type.is_dir() {
                return Err(StoreError::RegistryCorrupt(format!(
                    "unexpected registry entry {}",
                    entry.path().display()
                )));
            }
            let Some((revision, path)) = latest_revision_path(&entry.path())? else {
                continue;
            };
            let event: RegistryEvent<T> = read_json(&path, context)?;
            if event.schema != REGISTRY_EVENT_SCHEMA || event.revision != revision {
                return Err(StoreError::RegistryCorrupt(format!(
                    "registry event {} has an invalid schema or revision",
                    path.display()
                )));
            }
            event.value.validate_record()?;
            let expected_key = installation_key(event.value.installation_id())?;
            if entry.file_name().to_str() != Some(expected_key.as_str()) {
                return Err(StoreError::RegistryCorrupt(format!(
                    "registry entry {} does not match installation {}",
                    entry.path().display(),
                    event.value.installation_id()
                )));
            }
            values.push(event.value);
        }
        Ok(values)
    }

    fn collect_partial_from_directory(
        &self,
        directory: &Path,
        state: PartialInstallState,
        ready: &BTreeSet<InstallationId>,
        output: &mut Vec<PartialInstall>,
    ) -> Result<(), StoreError> {
        ensure_real_directory(directory)?;
        for entry in read_directory(directory)? {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let file_type = entry.file_type().map_err(|error| {
                StoreError::io("inspect partial installation", &entry.path(), error)
            })?;
            if file_type.is_symlink() || !file_type.is_dir() {
                return Err(StoreError::RegistryCorrupt(format!(
                    "unexpected managed package entry {}",
                    entry.path().display()
                )));
            }
            let manifest = read_staging_manifest(&entry.path())?;
            let expected_key = installation_key(&manifest.plan.installation_id)?;
            if entry.file_name().to_str() != Some(expected_key.as_str()) {
                return Err(StoreError::RegistryCorrupt(format!(
                    "managed package key does not match manifest at {}",
                    entry.path().display()
                )));
            }
            if !ready.contains(&manifest.plan.installation_id) {
                output.push(PartialInstall {
                    installation_id: manifest.plan.installation_id,
                    plan_sha256: manifest.plan.plan_sha256,
                    state,
                    absolute_path: entry.path(),
                });
            }
        }
        Ok(())
    }

    fn resolve_managed_relative_path(&self, relative: &str) -> Result<PathBuf, StoreError> {
        let relative_path = Path::new(relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(StoreError::UnsafePath {
                path: relative_path.to_path_buf(),
                reason: "registry path is not a clean relative path",
            });
        }
        let joined = self.root.join(relative_path);
        let canonical = fs::canonicalize(&joined)
            .map_err(|error| StoreError::io("canonicalize managed package", &joined, error))?;
        if !canonical.starts_with(&self.root) {
            return Err(StoreError::UnsafePath {
                path: canonical,
                reason: "registry path escapes managed root",
            });
        }
        Ok(canonical)
    }

    fn validate_ready_package_record(&self, receipt: &InstallReceipt) -> Result<(), StoreError> {
        let relative = receipt.installed_relative_path.as_deref().ok_or_else(|| {
            StoreError::RegistryCorrupt(format!(
                "ready receipt {} has no installed path",
                receipt.installation_id
            ))
        })?;
        let expected = format!("packages/{}", installation_key(&receipt.installation_id)?);
        if relative != expected {
            return Err(StoreError::RegistryCorrupt(format!(
                "ready receipt {} points to unexpected path {:?}",
                receipt.installation_id, relative
            )));
        }
        let absolute = self.resolve_managed_relative_path(relative)?;
        let manifest = read_staging_manifest(&absolute)?;
        ensure_receipt_matches_plan(receipt, &manifest.plan)?;
        verify_package_structure(&absolute, &manifest.plan)
    }
}

/// RAII guard for one installation's transfer/activation critical section.
pub struct InstallLease {
    file: File,
    installation_id: InstallationId,
    _process_lock: ProcessLock,
}

impl InstallLease {
    #[must_use]
    pub fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }
}

struct StoreLock {
    file: File,
    _process_lock: ProcessLock,
}

#[derive(Debug)]
struct ProcessLock {
    path: PathBuf,
}

impl ProcessLock {
    fn try_acquire(path: &Path) -> Option<Self> {
        let mut held = PROCESS_LOCKS
            .get_or_init(|| Mutex::new(BTreeSet::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.insert(path.to_path_buf()).then(|| Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let mut held = PROCESS_LOCKS
            .get_or_init(|| Mutex::new(BTreeSet::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.remove(&self.path);
    }
}

struct TemporaryDirectoryGuard {
    path: PathBuf,
    armed: bool,
}

impl TemporaryDirectoryGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryDirectoryGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ignored = fs::remove_dir_all(&self.path);
        }
    }
}

struct TemporaryFileGuard {
    path: PathBuf,
    armed: bool,
}

impl TemporaryFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ignored = fs::remove_file(&self.path);
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ignored = FileExt::unlock(&self.file);
    }
}

impl Drop for InstallLease {
    fn drop(&mut self) {
        let _ignored = FileExt::unlock(&self.file);
    }
}

#[derive(Debug)]
struct VerifiedArtifact {
    artifact_id: information_native_types::ArtifactId,
    file_name: String,
    bytes: u64,
    sha256: String,
}

/// Compute the canonical install plan fingerprint documented by this crate.
pub fn compute_plan_sha256(plan: &InstallPlan) -> Result<String, StoreError> {
    Ok(plan.compute_plan_sha256()?)
}

/// Observe the identity of a regular file. SHA-256 mode compares metadata from
/// before and after the read and fails if the source changed during the pass.
pub fn capture_source_identity(
    path: impl AsRef<Path>,
    strength: IdentityStrength,
) -> Result<SourceIdentity, StoreError> {
    let path = path.as_ref();
    reject_symlink(path)?;
    let before = fs::metadata(path)
        .map_err(|error| StoreError::io("inspect source identity", path, error))?;
    if !before.is_file() {
        return Err(StoreError::SourceNotFile(path.to_path_buf()));
    }
    let before_identity = metadata_identity(&before, None)?;
    if strength == IdentityStrength::Metadata {
        return Ok(before_identity);
    }

    let (bytes, digest, file_metadata) = hash_file(path)?;
    let after = fs::metadata(path)
        .map_err(|error| StoreError::io("reinspect source identity", path, error))?;
    let file_identity = metadata_identity(&file_metadata, None)?;
    let after_identity = metadata_identity(&after, None)?;
    if before_identity != file_identity || file_identity != after_identity || bytes != after.len() {
        return Err(StoreError::SourceChanged(path.to_path_buf()));
    }
    Ok(SourceIdentity {
        bytes,
        modified_unix_nanos: after_identity.modified_unix_nanos,
        device: after_identity.device,
        inode: after_identity.inode,
        sha256: Some(digest),
    })
}

/// Reobserve a file and compare it with a before snapshot. Callers use this
/// around immutable retrieval to detect source replacement or mutation.
pub fn check_source_identity(
    path: impl AsRef<Path>,
    before: &SourceIdentity,
) -> Result<IdentityCheck, StoreError> {
    before.validate()?;
    let strength = if before.sha256.is_some() {
        IdentityStrength::Sha256
    } else {
        IdentityStrength::Metadata
    };
    let after = capture_source_identity(path, strength)?;
    Ok(IdentityCheck {
        before: before.clone(),
        unchanged: identities_equal(before, &after),
        after,
    })
}

/// Reject an immutable SQLite snapshot if its sibling `-wal` contains bytes.
/// The check is format-agnostic and therefore safe to call for any immutable
/// regular file; non-SQLite files almost never have such a sibling.
pub fn reject_nonempty_sqlite_wal(path: impl AsRef<Path>) -> Result<(), StoreError> {
    let path = path.as_ref();
    let mut wal_name: OsString = path.as_os_str().to_os_string();
    wal_name.push("-wal");
    let wal = PathBuf::from(wal_name);
    match fs::symlink_metadata(&wal) {
        Ok(metadata)
            if !metadata.file_type().is_symlink() && metadata.is_file() && metadata.len() == 0 =>
        {
            Ok(())
        }
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
            Err(StoreError::NonEmptySqliteWal(wal))
        }
        Ok(_) => Err(StoreError::NonEmptySqliteWal(wal)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::io("inspect SQLite WAL", &wal, error)),
    }
}

/// Reject pending SQLite transaction state that an immutable main-file hash
/// would omit. Empty WAL files are harmless; SHM files contain coordination
/// state rather than database pages and are deliberately ignored.
pub fn reject_nonempty_sqlite_sidecars(path: impl AsRef<Path>) -> Result<(), StoreError> {
    let path = path.as_ref();
    reject_nonempty_sqlite_wal(path)?;
    let mut journal_name: OsString = path.as_os_str().to_os_string();
    journal_name.push("-journal");
    let journal = PathBuf::from(journal_name);
    match fs::symlink_metadata(&journal) {
        Ok(metadata)
            if !metadata.file_type().is_symlink() && metadata.is_file() && metadata.len() == 0 =>
        {
            Ok(())
        }
        Ok(_) => Err(StoreError::NonEmptySqliteJournal(journal)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::io(
            "inspect SQLite rollback journal",
            &journal,
            error,
        )),
    }
}

fn validate_store_plan(plan: &InstallPlan) -> Result<(), StoreError> {
    let observed = compute_plan_sha256(plan)?;
    if !digest_equal(&plan.plan_sha256, &observed) {
        return Err(StoreError::PlanFingerprintMismatch {
            expected: normalize_digest(&plan.plan_sha256),
            observed,
        });
    }
    plan.validate()?;
    validate_installation_id(&plan.installation_id)?;
    let mut artifact_ids = BTreeSet::new();
    let mut file_names = BTreeSet::new();
    for artifact in &plan.artifacts {
        if !artifact_ids.insert(&artifact.artifact_id) {
            return Err(StoreError::InvalidArtifactSet(format!(
                "duplicate artifact id {}",
                artifact.artifact_id
            )));
        }
        if !file_names.insert(&artifact.file_name) {
            return Err(StoreError::InvalidArtifactSet(format!(
                "duplicate artifact file name {:?}",
                artifact.file_name
            )));
        }
    }
    Ok(())
}

fn validate_installation_id(installation_id: &InstallationId) -> Result<(), StoreError> {
    let value = installation_id.as_str();
    if value.is_empty()
        || value.len() > 192
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'@')
        })
        || value == "."
        || value == ".."
    {
        return Err(StoreError::InvalidInstallationId(value.to_string()));
    }
    Ok(())
}

fn installation_key(installation_id: &InstallationId) -> Result<String, StoreError> {
    validate_installation_id(installation_id)?;
    Ok(hex::encode(Sha256::digest(
        installation_id.as_str().as_bytes(),
    )))
}

fn acquisition_journal_name(artifact_id: &ArtifactId) -> String {
    format!(
        "{}.json",
        hex::encode(Sha256::digest(artifact_id.as_str().as_bytes()))
    )
}

fn record_staged_acquisition_locked(
    plan: &InstallPlan,
    acquisition: &ArtifactAcquisition,
    stage: &Path,
) -> Result<(), StoreError> {
    let manifest = read_staging_manifest(stage)?;
    if manifest.plan != *plan {
        return Err(StoreError::StagedPlanMismatch);
    }
    let planned = plan
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == acquisition.artifact_id)
        .ok_or_else(|| {
            StoreError::InvalidArtifactSet(format!(
                "acquisition references unplanned artifact {}",
                acquisition.artifact_id
            ))
        })?;
    let artifact_path = stage.join("artifacts").join(&planned.file_name);
    reject_symlink(&artifact_path)?;
    let metadata = fs::metadata(&artifact_path).map_err(|error| {
        StoreError::io("inspect acquired staged artifact", &artifact_path, error)
    })?;
    if !metadata.is_file() {
        return Err(StoreError::SourceNotFile(artifact_path));
    }
    if metadata.len() != planned.expected_bytes {
        return Err(StoreError::ArtifactSizeMismatch {
            artifact_id: acquisition.artifact_id.to_string(),
            expected: planned.expected_bytes,
            observed: metadata.len(),
        });
    }
    if metadata.len() != acquisition.verified_bytes
        || !digest_equal(&acquisition.sha256, &planned.sha256)
    {
        return Err(StoreError::InvalidArtifactSet(format!(
            "acquisition record for {} does not match staged bytes",
            acquisition.artifact_id
        )));
    }
    if let Some(existing) = manifest
        .acquisitions
        .iter()
        .find(|existing| existing.artifact_id == acquisition.artifact_id)
    {
        if existing == acquisition {
            return Ok(());
        }
        return Err(StoreError::InvalidArtifactSet(format!(
            "staged acquisition for {} conflicts with its journal",
            acquisition.artifact_id
        )));
    }
    let journal = stage.join("acquisitions");
    ensure_real_directory(&journal)?;
    cleanup_acquisition_temporaries(&journal)?;
    let final_path = journal.join(acquisition_journal_name(&acquisition.artifact_id));
    if path_exists(&final_path)? {
        return Err(StoreError::InvalidArtifactSet(format!(
            "staged acquisition for {} conflicts with an unreadable journal entry",
            acquisition.artifact_id
        )));
    }
    let temporary = journal.join(format!(".journal-{}.tmp", Uuid::new_v4()));
    let mut temporary_guard = TemporaryFileGuard::new(temporary.clone());
    write_new_json(
        &temporary,
        &AcquisitionJournalEntry {
            schema: ACQUISITION_JOURNAL_SCHEMA.to_string(),
            acquisition: acquisition.clone(),
        },
        "staging acquisition journal",
    )?;
    fs::rename(&temporary, &final_path).map_err(|error| {
        StoreError::io("publish staging acquisition journal", &final_path, error)
    })?;
    temporary_guard.disarm();
    sync_directory(&journal)
}

fn cleanup_acquisition_temporaries(directory: &Path) -> Result<(), StoreError> {
    ensure_real_directory(directory)?;
    let mut removed = false;
    for entry in read_directory(directory)? {
        let name = entry.file_name().into_string().map_err(|_| {
            StoreError::InvalidArtifactSet(format!(
                "non-UTF-8 acquisition journal entry in {}",
                directory.display()
            ))
        })?;
        if !name.starts_with(".journal-") || !name.ends_with(".tmp") {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            StoreError::io("inspect acquisition journal temporary", &path, error)
        })?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(StoreError::UnsafePath {
                path,
                reason: "acquisition journal temporary is not a regular file",
            });
        }
        fs::remove_file(&path).map_err(|error| {
            StoreError::io("remove acquisition journal temporary", &path, error)
        })?;
        removed = true;
    }
    if removed {
        sync_directory(directory)?;
    }
    Ok(())
}

fn prepared_install(
    plan: &InstallPlan,
    directory: PathBuf,
    prepared_at: DateTime<Utc>,
) -> Result<PreparedInstall, StoreError> {
    let artifacts_dir = directory.join("artifacts");
    ensure_real_directory(&artifacts_dir)?;
    let artifacts = plan
        .artifacts
        .iter()
        .map(|artifact| StagedArtifactTarget {
            artifact_id: artifact.artifact_id.clone(),
            path: artifacts_dir.join(&artifact.file_name),
            expected_bytes: artifact.expected_bytes,
            sha256: normalize_digest(&artifact.sha256),
        })
        .collect();
    Ok(PreparedInstall {
        installation_id: plan.installation_id.clone(),
        prepared_at,
        directory,
        artifacts,
    })
}

fn read_staging_manifest(directory: &Path) -> Result<StagingManifest, StoreError> {
    ensure_real_directory(directory)?;
    let path = directory.join("stage.json");
    reject_symlink(&path)?;
    let mut manifest: StagingManifest = read_json(&path, "staging manifest")?;
    if manifest.schema != STAGING_SCHEMA {
        return Err(StoreError::RegistryCorrupt(format!(
            "staging manifest {} has unknown schema {:?}",
            path.display(),
            manifest.schema
        )));
    }
    validate_store_plan(&manifest.plan)?;
    let journal = directory.join("acquisitions");
    ensure_real_directory(&journal)?;
    let mut expected_names = manifest
        .plan
        .artifacts
        .iter()
        .map(|artifact| {
            (
                acquisition_journal_name(&artifact.artifact_id),
                artifact.artifact_id.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for entry in read_directory(&journal)? {
        let name = entry.file_name().into_string().map_err(|_| {
            StoreError::InvalidArtifactSet(format!(
                "non-UTF-8 acquisition journal entry in {}",
                journal.display()
            ))
        })?;
        if name.starts_with(".journal-") && name.ends_with(".tmp") {
            continue;
        }
        let expected = expected_names.remove(&name).ok_or_else(|| {
            StoreError::InvalidArtifactSet(format!("unexpected acquisition journal entry {name:?}"))
        })?;
        let file_type = entry.file_type().map_err(|error| {
            StoreError::io("inspect acquisition journal entry", &entry.path(), error)
        })?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(StoreError::UnsafePath {
                path: entry.path(),
                reason: "acquisition journal entry is not a regular file",
            });
        }
        enforce_private_file(&entry.path())?;
        let journal_entry: AcquisitionJournalEntry =
            read_json(&entry.path(), "staging acquisition journal")?;
        if journal_entry.schema != ACQUISITION_JOURNAL_SCHEMA
            || journal_entry.acquisition.artifact_id != expected
        {
            return Err(StoreError::InvalidArtifactSet(format!(
                "acquisition journal entry {name:?} has the wrong identity"
            )));
        }
        journal_entry.acquisition.validate()?;
        if manifest
            .acquisitions
            .iter()
            .any(|existing| existing.artifact_id == expected)
        {
            return Err(StoreError::InvalidArtifactSet(format!(
                "duplicate acquisition journal entry for {expected}"
            )));
        }
        manifest.acquisitions.push(journal_entry.acquisition);
    }
    manifest
        .acquisitions
        .sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    Ok(manifest)
}

fn verify_package_directory(
    directory: &Path,
    plan: &InstallPlan,
) -> Result<Vec<VerifiedArtifact>, StoreError> {
    let manifest = read_staging_manifest(directory)?;
    if manifest.plan != *plan {
        return Err(StoreError::StagedPlanMismatch);
    }
    ensure_exact_directory_entries(directory, &["acquisitions", "artifacts", "stage.json"])?;
    verify_acquisition_journal_directory(directory, plan)?;
    let artifacts_dir = directory.join("artifacts");
    ensure_real_directory(&artifacts_dir)?;
    let expected_names = plan
        .artifacts
        .iter()
        .map(|artifact| artifact.file_name.as_str())
        .collect::<Vec<_>>();
    ensure_exact_directory_entries(&artifacts_dir, &expected_names)?;

    let mut verified = Vec::with_capacity(plan.artifacts.len());
    for artifact in &plan.artifacts {
        let path = artifacts_dir.join(&artifact.file_name);
        reject_symlink(&path)?;
        enforce_private_file(&path)?;
        let (bytes, digest, _) = hash_file(&path)?;
        if bytes != artifact.expected_bytes {
            return Err(StoreError::ArtifactSizeMismatch {
                artifact_id: artifact.artifact_id.to_string(),
                expected: artifact.expected_bytes,
                observed: bytes,
            });
        }
        if !digest_equal(&artifact.sha256, &digest) {
            return Err(StoreError::ArtifactDigestMismatch {
                artifact_id: artifact.artifact_id.to_string(),
                expected: normalize_digest(&artifact.sha256),
                observed: digest,
            });
        }
        verified.push(VerifiedArtifact {
            artifact_id: artifact.artifact_id.clone(),
            file_name: artifact.file_name.clone(),
            bytes,
            sha256: digest,
        });
    }
    Ok(verified)
}

fn verify_package_structure(directory: &Path, plan: &InstallPlan) -> Result<(), StoreError> {
    ensure_exact_directory_entries(directory, &["acquisitions", "artifacts", "stage.json"])?;
    verify_acquisition_journal_directory(directory, plan)?;
    let artifacts_dir = directory.join("artifacts");
    ensure_real_directory(&artifacts_dir)?;
    let expected_names = plan
        .artifacts
        .iter()
        .map(|artifact| artifact.file_name.as_str())
        .collect::<Vec<_>>();
    ensure_exact_directory_entries(&artifacts_dir, &expected_names)?;
    for artifact in &plan.artifacts {
        let path = artifacts_dir.join(&artifact.file_name);
        reject_symlink(&path)?;
        let metadata = fs::metadata(&path)
            .map_err(|error| StoreError::io("inspect managed artifact", &path, error))?;
        if !metadata.is_file() {
            return Err(StoreError::SourceNotFile(path));
        }
        if metadata.len() != artifact.expected_bytes {
            return Err(StoreError::ArtifactSizeMismatch {
                artifact_id: artifact.artifact_id.to_string(),
                expected: artifact.expected_bytes,
                observed: metadata.len(),
            });
        }
    }
    Ok(())
}

fn verify_acquisition_journal_directory(
    directory: &Path,
    plan: &InstallPlan,
) -> Result<(), StoreError> {
    let acquisitions = directory.join("acquisitions");
    ensure_real_directory(&acquisitions)?;
    let expected = plan
        .artifacts
        .iter()
        .map(|artifact| acquisition_journal_name(&artifact.artifact_id))
        .collect::<Vec<_>>();
    let expected = expected.iter().map(String::as_str).collect::<Vec<_>>();
    ensure_exact_directory_entries(&acquisitions, &expected)
}

fn validate_acquisition_records(
    plan: &InstallPlan,
    verified: &[VerifiedArtifact],
    acquisitions: &[ArtifactAcquisition],
) -> Result<(), StoreError> {
    if acquisitions.len() != plan.artifacts.len() || verified.len() != plan.artifacts.len() {
        return Err(StoreError::InvalidArtifactSet(
            "acquisition record count does not match the install plan".to_string(),
        ));
    }
    let mut observed = BTreeSet::new();
    for acquisition in acquisitions {
        acquisition.validate()?;
        if !observed.insert(&acquisition.artifact_id) {
            return Err(StoreError::InvalidArtifactSet(format!(
                "duplicate acquisition record for {}",
                acquisition.artifact_id
            )));
        }
        let planned = plan
            .artifacts
            .iter()
            .find(|artifact| artifact.artifact_id == acquisition.artifact_id)
            .ok_or_else(|| {
                StoreError::InvalidArtifactSet(format!(
                    "acquisition record references unplanned artifact {}",
                    acquisition.artifact_id
                ))
            })?;
        let observed_artifact = verified
            .iter()
            .find(|artifact| artifact.artifact_id == acquisition.artifact_id)
            .ok_or_else(|| {
                StoreError::InvalidArtifactSet(format!(
                    "verified artifact {} has no acquisition record",
                    acquisition.artifact_id
                ))
            })?;
        if acquisition.verified_bytes != planned.expected_bytes
            || acquisition.verified_bytes != observed_artifact.bytes
            || !digest_equal(&acquisition.sha256, &planned.sha256)
            || !digest_equal(&acquisition.sha256, &observed_artifact.sha256)
        {
            return Err(StoreError::InvalidArtifactSet(format!(
                "acquisition record for {} disagrees with verified bytes",
                acquisition.artifact_id
            )));
        }
    }
    Ok(())
}

fn ensure_receipt_matches_plan(
    receipt: &InstallReceipt,
    plan: &InstallPlan,
) -> Result<(), StoreError> {
    receipt.validate()?;
    if receipt.state != InstallationState::Ready
        || receipt.installation_id != plan.installation_id
        || receipt.resource_id != plan.resource_id
        || receipt.release_id != plan.release_id
        || receipt.representation_id != plan.representation_id
        || receipt.format != plan.format
        || !digest_equal(&receipt.plan_sha256, &plan.plan_sha256)
    {
        return Err(StoreError::RegistrationConflict(
            plan.installation_id.clone(),
        ));
    }
    if receipt.downloaded_bytes != plan.total_download_bytes
        || receipt.artifacts.len() != plan.artifacts.len()
    {
        return Err(StoreError::RegistryCorrupt(format!(
            "ready receipt {} has artifact accounting inconsistent with its plan",
            receipt.installation_id
        )));
    }
    let key = installation_key(&receipt.installation_id)?;
    let mut observed = BTreeSet::new();
    for installed in &receipt.artifacts {
        if !observed.insert(&installed.artifact_id) {
            return Err(StoreError::RegistryCorrupt(format!(
                "ready receipt {} repeats artifact {}",
                receipt.installation_id, installed.artifact_id
            )));
        }
        let planned = plan
            .artifacts
            .iter()
            .find(|artifact| artifact.artifact_id == installed.artifact_id)
            .ok_or_else(|| {
                StoreError::RegistryCorrupt(format!(
                    "ready receipt {} contains unplanned artifact {}",
                    receipt.installation_id, installed.artifact_id
                ))
            })?;
        let expected_path = format!("packages/{key}/artifacts/{}", planned.file_name);
        if installed.relative_path != expected_path
            || installed.bytes != planned.expected_bytes
            || !digest_equal(&installed.sha256, &planned.sha256)
        {
            return Err(StoreError::RegistryCorrupt(format!(
                "ready receipt {} has invalid accounting for artifact {}",
                receipt.installation_id, installed.artifact_id
            )));
        }
    }
    Ok(())
}

fn ensure_non_ready_receipt_matches_plan(
    receipt: &InstallReceipt,
    plan: &InstallPlan,
) -> Result<(), StoreError> {
    receipt.validate()?;
    if receipt.state == InstallationState::Ready
        || receipt.installation_id != plan.installation_id
        || receipt.resource_id != plan.resource_id
        || receipt.release_id != plan.release_id
        || receipt.representation_id != plan.representation_id
        || receipt.format != plan.format
        || !digest_equal(&receipt.plan_sha256, &plan.plan_sha256)
    {
        return Err(StoreError::RegistrationConflict(
            plan.installation_id.clone(),
        ));
    }
    Ok(())
}

fn ensure_exact_directory_entries(directory: &Path, expected: &[&str]) -> Result<(), StoreError> {
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    for entry in read_directory(directory)? {
        let name = entry.file_name().into_string().map_err(|_| {
            StoreError::InvalidArtifactSet(format!("non-UTF-8 entry in {}", directory.display()))
        })?;
        if !observed.insert(name.clone()) {
            return Err(StoreError::InvalidArtifactSet(format!(
                "duplicate directory entry {name:?}"
            )));
        }
        let file_type = entry.file_type().map_err(|error| {
            StoreError::io("inspect package directory entry", &entry.path(), error)
        })?;
        if file_type.is_symlink() {
            return Err(StoreError::UnsafePath {
                path: entry.path(),
                reason: "symlink inside managed package",
            });
        }
    }
    let observed_refs = observed.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if observed_refs != expected {
        return Err(StoreError::InvalidArtifactSet(format!(
            "directory {} contains {:?}; expected {:?}",
            directory.display(),
            observed_refs,
            expected
        )));
    }
    Ok(())
}

fn metadata_identity(
    metadata: &fs::Metadata,
    sha256: Option<String>,
) -> Result<SourceIdentity, StoreError> {
    let modified_unix_nanos = metadata.modified().ok().and_then(system_time_unix_nanos);
    let identity = SourceIdentity {
        bytes: metadata.len(),
        modified_unix_nanos,
        device: metadata_device(metadata),
        inode: metadata_inode(metadata),
        sha256,
    };
    identity.validate()?;
    Ok(identity)
}

#[cfg(unix)]
fn metadata_device(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev())
}

#[cfg(not(unix))]
fn metadata_device(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

#[cfg(unix)]
fn metadata_inode(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.ino())
}

#[cfg(not(unix))]
fn metadata_inode(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

fn system_time_unix_nanos(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|value| value.as_nanos())
}

fn hash_file(path: &Path) -> Result<(u64, String, fs::Metadata), StoreError> {
    reject_symlink(path)?;
    let file =
        File::open(path).map_err(|error| StoreError::io("open file for hashing", path, error))?;
    let metadata_before = file
        .metadata()
        .map_err(|error| StoreError::io("inspect file before hashing", path, error))?;
    if !metadata_before.is_file() {
        return Err(StoreError::SourceNotFile(path.to_path_buf()));
    }
    let before = metadata_identity(&metadata_before, None)?;
    let mut reader = BufReader::with_capacity(HASH_BUFFER_BYTES, file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    let mut bytes = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| StoreError::io("read file for hashing", path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        let read = u64::try_from(read).map_err(|_| StoreError::IntegerOverflow)?;
        bytes = bytes.checked_add(read).ok_or(StoreError::IntegerOverflow)?;
    }
    let metadata_after = reader
        .get_ref()
        .metadata()
        .map_err(|error| StoreError::io("inspect file after hashing", path, error))?;
    let after = metadata_identity(&metadata_after, None)?;
    if before != after || bytes != after.bytes {
        return Err(StoreError::SourceChanged(path.to_path_buf()));
    }
    Ok((bytes, hex::encode(hasher.finalize()), metadata_after))
}

fn identities_equal(left: &SourceIdentity, right: &SourceIdentity) -> bool {
    left.bytes == right.bytes
        && left.modified_unix_nanos == right.modified_unix_nanos
        && left.device == right.device
        && left.inode == right.inode
        && match (&left.sha256, &right.sha256) {
            (Some(left), Some(right)) => digest_equal(left, right),
            (None, None) => true,
            _ => false,
        }
}

fn digest_equal(left: &str, right: &str) -> bool {
    normalize_digest(left).eq_ignore_ascii_case(&normalize_digest(right))
}

fn normalize_digest(value: &str) -> String {
    value
        .strip_prefix("sha256:")
        .unwrap_or(value)
        .to_ascii_lowercase()
}

fn write_new_json<T: Serialize>(
    path: &Path,
    value: &T,
    context: &'static str,
) -> Result<(), StoreError> {
    let mut encoded =
        serde_json::to_vec_pretty(value).map_err(|source| StoreError::Json { context, source })?;
    encoded.push(b'\n');
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_private_create_mode(&mut options);
    let mut file = options
        .open(path)
        .map_err(|error| StoreError::io("create JSON file", path, error))?;
    file.write_all(&encoded)
        .map_err(|error| StoreError::io("write JSON file", path, error))?;
    file.sync_all()
        .map_err(|error| StoreError::io("sync JSON file", path, error))
}

fn create_private_dir_all(path: &Path) -> Result<(), StoreError> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|error| StoreError::io("create private directory tree", path, error))?;
    enforce_private_directory(path)
}

fn create_private_dir(path: &Path) -> Result<(), StoreError> {
    let mut builder = DirBuilder::new();
    builder.recursive(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|error| StoreError::io("create private directory", path, error))?;
    enforce_private_directory(path)
}

#[cfg(unix)]
fn enforce_private_directory(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StoreError::io("inspect private directory", path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "private store path is not a real directory",
        });
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| StoreError::io("restrict directory permissions", path, error))
}

#[cfg(not(unix))]
fn enforce_private_directory(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(unix)]
fn enforce_private_file(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| StoreError::io("restrict file permissions", path, error))
}

#[cfg(not(unix))]
fn enforce_private_file(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_create_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_create_mode(_options: &mut OpenOptions) {}

fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    context: &'static str,
) -> Result<T, StoreError> {
    reject_symlink(path)?;
    let file = File::open(path).map_err(|error| StoreError::io("open JSON file", path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| StoreError::io("inspect JSON file", path, error))?;
    if !metadata.is_file() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "JSON path is not a regular file",
        });
    }
    serde_json::from_reader(BufReader::new(file))
        .map_err(|source| StoreError::Json { context, source })
}

fn next_revision(directory: &Path) -> Result<u64, StoreError> {
    let next = latest_revision_path(directory)?.map_or(Ok(1_u64), |(revision, _)| {
        revision.checked_add(1).ok_or(StoreError::IntegerOverflow)
    })?;
    Ok(next)
}

fn latest_revision_path(directory: &Path) -> Result<Option<(u64, PathBuf)>, StoreError> {
    let mut latest: Option<(u64, PathBuf)> = None;
    for entry in read_directory(directory)? {
        let Some(revision) = revision_from_path(&entry.path())? else {
            continue;
        };
        let file_type = entry
            .file_type()
            .map_err(|error| StoreError::io("inspect registry event", &entry.path(), error))?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(StoreError::RegistryCorrupt(format!(
                "registry event is not a regular file: {}",
                entry.path().display()
            )));
        }
        if latest
            .as_ref()
            .is_none_or(|(current, _)| revision > *current)
        {
            latest = Some((revision, entry.path()));
        }
    }
    Ok(latest)
}

fn revision_from_path(path: &Path) -> Result<Option<u64>, StoreError> {
    let name = path
        .file_name()
        .ok_or_else(|| StoreError::RegistryCorrupt("registry event has no file name".to_string()))?
        .to_string_lossy();
    if name.starts_with('.') {
        return Ok(None);
    }
    let Some(revision_text) = name.strip_suffix(".json") else {
        return Err(StoreError::RegistryCorrupt(format!(
            "unexpected registry event file {}",
            path.display()
        )));
    };
    if revision_text.len() != 20 || !revision_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(StoreError::RegistryCorrupt(format!(
            "invalid registry revision file {}",
            path.display()
        )));
    }
    revision_text.parse::<u64>().map(Some).map_err(|_| {
        StoreError::RegistryCorrupt(format!("invalid registry revision file {}", path.display()))
    })
}

fn directory_bytes(path: &Path) -> Result<u64, StoreError> {
    reject_symlink(path)?;
    let metadata = fs::metadata(path)
        .map_err(|error| StoreError::io("inspect removal target", path, error))?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "removal target is neither a file nor directory",
        });
    }
    let mut total = 0_u64;
    for entry in read_directory(path)? {
        let file_type = entry.file_type().map_err(|error| {
            StoreError::io("inspect removal target entry", &entry.path(), error)
        })?;
        if file_type.is_symlink() {
            return Err(StoreError::UnsafePath {
                path: entry.path(),
                reason: "symlink in removal target",
            });
        }
        let bytes = directory_bytes(&entry.path())?;
        total = total
            .checked_add(bytes)
            .ok_or(StoreError::IntegerOverflow)?;
    }
    Ok(total)
}

fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, StoreError> {
    let entries = fs::read_dir(path)
        .map_err(|error| StoreError::io("read directory", path, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StoreError::io("read directory entry", path, error))?;
    Ok(entries)
}

fn path_exists(path: &Path) -> Result<bool, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StoreError::io("inspect path", path, error)),
    }
}

fn reject_symlink(path: &Path) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| StoreError::io("inspect path without following links", path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "symbolic links are not accepted",
        });
    }
    Ok(())
}

fn ensure_real_directory(path: &Path) -> Result<(), StoreError> {
    reject_symlink(path)?;
    let metadata =
        fs::metadata(path).map_err(|error| StoreError::io("inspect directory", path, error))?;
    if !metadata.is_dir() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            reason: "expected a directory",
        });
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), StoreError> {
    let directory =
        File::open(path).map_err(|error| StoreError::io("open directory for sync", path, error))?;
    directory
        .sync_all()
        .map_err(|error| StoreError::io("sync directory", path, error))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use information_native_types::{
        ArtifactDescriptor, ArtifactId, ArtifactMirror, ArtifactRole, CatalogAuthority,
        CatalogTrust, CoverageDescriptor, ErrorClass, FormatKind, INSTALL_PLAN_SCHEMA,
        InformationCapability, InformationError, InstallSelection, PlannedArtifact,
        RedistributionPolicy, RepresentationDescriptor, ResolvedResource, ResourceDescriptor,
        ResourceKind, RightsStatement, RuntimeRequirement, SubsetSupport,
    };
    use std::error::Error;
    use tempfile::TempDir;

    fn digest(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn plan_with_bytes(bytes: &[u8]) -> Result<InstallPlan, StoreError> {
        let expected_bytes = u64::try_from(bytes.len()).map_err(|_| StoreError::IntegerOverflow)?;
        let resource_id = ResourceId::parse("test.resource")?;
        let release_id = ReleaseId::parse("2026-08")?;
        let representation_id = RepresentationId::parse("fts")?;
        let format = RepresentationFormat {
            kind: FormatKind::SqliteFts5,
            profile: Some("test-v1".to_string()),
            media_type: Some("application/vnd.sqlite3".to_string()),
        };
        let use_policy = UsePolicy {
            attribution_required: false,
            ..UsePolicy::default()
        };
        let rights = vec![RightsStatement {
            scope: "fixture".to_string(),
            expression: "unknown".to_string(),
            license_url: None,
            license_text_sha256: None,
            attribution: None,
            obligations: Vec::new(),
            redistribution: RedistributionPolicy::Unknown,
        }];
        let provenance = Provenance {
            publisher: "Fixture".to_string(),
            source_uri: "file:///source/library.sqlite".to_string(),
            upstream_record_id: None,
            source_inputs: Vec::new(),
            transformation: None,
            metadata: std::collections::BTreeMap::new(),
        };
        let artifact = ArtifactDescriptor {
            id: ArtifactId::parse("primary")?,
            role: ArtifactRole::Primary,
            file_name: "library.sqlite".to_string(),
            media_type: "application/vnd.sqlite3".to_string(),
            expected_bytes,
            sha256: digest(bytes),
            mirrors: vec![ArtifactMirror {
                uri: "file:///source/library.sqlite".to_string(),
                priority: 1,
            }],
        };
        let mut plan = InstallPlan {
            schema: INSTALL_PLAN_SCHEMA.to_string(),
            installation_id: InstallationId::parse("11111111-1111-4111-8111-111111111111")?,
            resource_id: resource_id.clone(),
            release_id: release_id.clone(),
            representation_id: representation_id.clone(),
            format: format.clone(),
            catalog_authority: CatalogAuthority::Unverified {
                declared: CatalogTrust::Unverified,
                catalog_sha256: "0".repeat(64),
            },
            resolved: ResolvedResource {
                resource: ResourceDescriptor {
                    id: resource_id,
                    kind: ResourceKind::TextCorpus,
                    title: "Fixture".to_string(),
                    summary: "Fixture corpus".to_string(),
                    languages: vec!["en".to_string()],
                    subjects: vec!["test".to_string()],
                    homepage: None,
                    extensions: std::collections::BTreeMap::new(),
                },
                release_id,
                published_at: None,
                upstream_id: None,
                immutable: true,
                provenance,
                rights: rights.clone(),
                use_policy,
                representation: RepresentationDescriptor {
                    id: representation_id,
                    format: format.clone(),
                    capabilities: std::collections::BTreeSet::from([
                        InformationCapability::LexicalSearch,
                    ]),
                    coverage: CoverageDescriptor::default(),
                    subset_support: SubsetSupport::default(),
                    expected_installed_bytes: expected_bytes,
                    artifacts: vec![artifact],
                    runtime: RuntimeRequirement::None,
                },
            },
            selection: InstallSelection::default(),
            rights,
            use_policy,
            artifacts: vec![PlannedArtifact {
                artifact_id: ArtifactId::parse("primary")?,
                file_name: "library.sqlite".to_string(),
                source_uri: "file:///source/library.sqlite".to_string(),
                expected_bytes,
                sha256: digest(bytes),
            }],
            total_download_bytes: expected_bytes,
            expected_installed_bytes: expected_bytes,
            available_bytes_observed: None,
            created_at: DateTime::parse_from_rfc3339("2026-08-08T12:00:00Z")
                .map_err(|error| StoreError::RegistryCorrupt(error.to_string()))?
                .with_timezone(&Utc),
            plan_sha256: "0".repeat(64),
        };
        plan.refresh_plan_sha256()?;
        Ok(plan)
    }

    fn write_staged(prepared: &PreparedInstall, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
        let target = prepared
            .artifacts
            .first()
            .ok_or_else(|| io::Error::other("test plan has no artifact target"))?;
        fs::write(&target.path, bytes)?;
        Ok(())
    }

    #[test]
    fn plan_fingerprint_uses_zeroed_field_compact_json() -> Result<(), Box<dyn Error>> {
        let plan = plan_with_bytes(b"abc")?;
        let mut zeroed = plan.clone();
        zeroed.plan_sha256 = "0".repeat(64);
        let independently_encoded = serde_json::to_vec(&zeroed)?;
        assert_eq!(
            plan.plan_sha256,
            hex::encode(Sha256::digest(independently_encoded))
        );
        assert_eq!(compute_plan_sha256(&plan)?, plan.plan_sha256);
        Ok(())
    }

    #[test]
    fn installation_lease_spans_the_transfer_critical_section() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("managed"))?;
        let plan = plan_with_bytes(b"lease fixture")?;
        let lease = store.acquire_install_lease(&plan)?;
        assert_eq!(lease.installation_id(), &plan.installation_id);
        assert!(matches!(
            store.acquire_install_lease(&plan),
            Err(StoreError::InstallationBusy(id)) if id == plan.installation_id
        ));
        drop(lease);
        let reacquired = store.acquire_install_lease(&plan)?;
        assert_eq!(reacquired.installation_id(), &plan.installation_id);
        Ok(())
    }

    #[test]
    fn partial_stage_is_not_listed_as_ready() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let plan = plan_with_bytes(b"complete payload")?;
        let prepared = store.prepare_install(&plan)?;
        write_staged(&prepared, b"partial")?;

        assert!(store.list()?.managed.is_empty());
        let partial = store.list_partial_installs()?;
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].state, PartialInstallState::Staged);
        assert_eq!(partial[0].installation_id, plan.installation_id);
        Ok(())
    }

    #[test]
    fn activation_rejects_size_and_digest_then_receipts_exact_bytes() -> Result<(), Box<dyn Error>>
    {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let expected = b"verified archive bytes";
        let plan = plan_with_bytes(expected)?;
        let prepared = store.prepare_install(&plan)?;
        write_staged(&prepared, b"short")?;
        let transfer = TransferSummary::for_plan(&plan, Utc::now(), false);
        assert!(matches!(
            store.activate(&plan, transfer.clone()),
            Err(StoreError::ArtifactSizeMismatch { .. })
        ));
        assert!(store.list()?.managed.is_empty());

        write_staged(&prepared, b"wrong digest payload!!")?;
        assert!(matches!(
            store.activate(&plan, transfer.clone()),
            Err(StoreError::ArtifactDigestMismatch { .. })
                | Err(StoreError::ArtifactSizeMismatch { .. })
        ));
        write_staged(&prepared, expected)?;
        let receipt = store.activate(&plan, transfer)?;
        assert_eq!(receipt.state, InstallationState::Ready);
        assert_eq!(receipt.downloaded_bytes, plan.total_download_bytes);
        assert_eq!(receipt.artifacts.len(), 1);
        assert_eq!(receipt.artifacts[0].bytes, plan.artifacts[0].expected_bytes);
        assert_eq!(receipt.artifacts[0].sha256, plan.artifacts[0].sha256);
        assert_eq!(store.list()?.managed, vec![receipt]);
        assert_eq!(store.get_installed_plan(&plan.installation_id)?, Some(plan));
        Ok(())
    }

    #[test]
    fn activation_recovers_crash_after_atomic_rename() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let payload = b"recoverable package";
        let plan = plan_with_bytes(payload)?;
        let prepared = store.prepare_install(&plan)?;
        write_staged(&prepared, payload)?;
        let transfer = TransferSummary::for_plan(&plan, Utc::now(), true);
        store.record_staged_acquisition(&plan, &transfer.acquisitions[0])?;
        let key = installation_key(&plan.installation_id)?;
        let activated = store.packages_root().join(key);
        fs::rename(&prepared.directory, &activated)?;

        assert!(store.list()?.managed.is_empty());
        let partial = store.list_partial_installs()?;
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].state, PartialInstallState::ActivatedUnregistered);

        let receipt = store
            .recover_interrupted_activation(&plan)?
            .ok_or_else(|| io::Error::other("activated package was not recovered"))?;
        assert_eq!(receipt.state, InstallationState::Ready);
        assert!(store.list_partial_installs()?.is_empty());
        assert_eq!(store.list()?.managed, vec![receipt]);
        Ok(())
    }

    #[test]
    fn acquisition_journal_is_append_only_and_cleans_interrupted_temporaries()
    -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let payload = b"journaled payload";
        let plan = plan_with_bytes(payload)?;
        let prepared = store.prepare_install(&plan)?;
        write_staged(&prepared, payload)?;
        let stage_manifest = prepared.directory.join("stage.json");
        let original_manifest = fs::read(&stage_manifest)?;
        let transfer = TransferSummary::for_plan(&plan, prepared.prepared_at, false);
        store.record_staged_acquisition(&plan, &transfer.acquisitions[0])?;
        store.record_staged_acquisition(&plan, &transfer.acquisitions[0])?;
        assert_eq!(fs::read(&stage_manifest)?, original_manifest);
        let journal = prepared.directory.join("acquisitions");
        assert_eq!(read_directory(&journal)?.len(), 1);

        let abandoned = journal.join(".journal-abandoned.tmp");
        write_new_json(&abandoned, &transfer.acquisitions[0], "test temporary")?;
        assert!(abandoned.exists());
        let resumed = store.prepare_install(&plan)?;
        assert_eq!(resumed.directory, prepared.directory);
        assert!(!abandoned.exists());
        assert_eq!(store.staged_acquisitions(&plan)?, transfer.acquisitions);
        Ok(())
    }

    #[test]
    fn full_verification_detects_same_length_tampering() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let payload = b"original";
        let plan = plan_with_bytes(payload)?;
        let prepared = store.prepare_install(&plan)?;
        write_staged(&prepared, payload)?;
        store.activate(&plan, TransferSummary::for_plan(&plan, Utc::now(), false))?;
        assert_eq!(
            store.verify_full(&plan.installation_id)?.logical_bytes,
            u64::try_from(payload.len())?
        );
        let key = installation_key(&plan.installation_id)?;
        let installed = store
            .packages_root()
            .join(key)
            .join("artifacts/library.sqlite");
        fs::write(installed, b"tampered")?;
        assert_eq!(store.list()?.managed.len(), 1);
        assert!(matches!(
            store.verify_full(&plan.installation_id),
            Err(StoreError::ArtifactDigestMismatch { .. })
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn managed_state_is_private_on_unix() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;

        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let plan = plan_with_bytes(b"private")?;
        let prepared = store.prepare_install(&plan)?;
        write_staged(&prepared, b"private")?;
        store.activate(&plan, TransferSummary::for_plan(&plan, Utc::now(), false))?;
        assert_eq!(fs::metadata(store.root())?.permissions().mode() & 0o077, 0);
        let key = installation_key(&plan.installation_id)?;
        let package = store.packages_root().join(key);
        assert_eq!(fs::metadata(&package)?.permissions().mode() & 0o077, 0);
        assert_eq!(
            fs::metadata(package.join("stage.json"))?
                .permissions()
                .mode()
                & 0o077,
            0
        );
        assert_eq!(
            fs::metadata(package.join("artifacts/library.sqlite"))?
                .permissions()
                .mode()
                & 0o077,
            0
        );
        Ok(())
    }

    #[test]
    fn traversal_and_unexpected_files_fail_closed() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let mut plan = plan_with_bytes(b"payload")?;
        plan.artifacts[0].file_name = "../escape".to_string();
        plan.plan_sha256 = compute_plan_sha256(&plan)?;
        assert!(matches!(
            store.prepare_install(&plan),
            Err(StoreError::Contract(ContractError::InvalidFileName(_)))
        ));
        assert!(!temporary.path().join("escape").exists());

        let plan = plan_with_bytes(b"payload")?;
        let prepared = store.prepare_install(&plan)?;
        write_staged(&prepared, b"payload")?;
        fs::write(prepared.directory.join("artifacts/unplanned"), b"surprise")?;
        assert!(matches!(
            store.activate(&plan, TransferSummary::for_plan(&plan, Utc::now(), false)),
            Err(StoreError::InvalidArtifactSet(_))
        ));
        assert!(store.list()?.managed.is_empty());
        Ok(())
    }

    #[test]
    fn immutable_external_rejects_wal_and_live_identity_is_distinct() -> Result<(), Box<dyn Error>>
    {
        let temporary = TempDir::new()?;
        let source = temporary.path().join("library.sqlite");
        fs::write(&source, b"sqlite snapshot")?;
        let wal = temporary.path().join("library.sqlite-wal");
        fs::write(&wal, b"pending transaction")?;
        let store = ManagedStore::open(temporary.path().join("managed"))?;
        let base = ExternalRegistrationRequest {
            installation_id: InstallationId::parse("external-live")?,
            resource_id: ResourceId::parse("test.external")?,
            release_id: ReleaseId::parse("snapshot")?,
            representation_id: RepresentationId::parse("sqlite")?,
            format: RepresentationFormat {
                kind: FormatKind::SqliteFts5,
                profile: Some("test-v1".to_string()),
                media_type: Some("application/vnd.sqlite3".to_string()),
            },
            absolute_path: source.clone(),
            access_mode: ExternalAccessMode::LiveReadOnly,
            provenance: Provenance {
                publisher: "Fixture".to_string(),
                source_uri: "file:///fixture/library.sqlite".to_string(),
                upstream_record_id: None,
                source_inputs: Vec::new(),
                transformation: None,
                metadata: std::collections::BTreeMap::new(),
            },
            rights: vec![RightsStatement {
                scope: "fixture".to_string(),
                expression: "unknown".to_string(),
                license_url: None,
                license_text_sha256: None,
                attribution: None,
                obligations: Vec::new(),
                redistribution: RedistributionPolicy::Unknown,
            }],
            use_policy: UsePolicy {
                attribution_required: false,
                ..UsePolicy::default()
            },
        };
        let live = store.register_external(&base)?;
        assert!(live.identity.sha256.is_none());

        let immutable = ExternalRegistrationRequest {
            installation_id: InstallationId::parse("external-immutable")?,
            access_mode: ExternalAccessMode::ImmutableReadOnly,
            ..base
        };
        assert!(matches!(
            store.register_external(&immutable),
            Err(StoreError::NonEmptySqliteWal(_))
        ));
        fs::write(&wal, b"")?;
        let journal = temporary.path().join("library.sqlite-journal");
        fs::write(&journal, b"pending rollback")?;
        assert!(matches!(
            store.register_external(&immutable),
            Err(StoreError::NonEmptySqliteJournal(_))
        ));
        fs::write(&journal, b"")?;
        let immutable = store.register_external(&immutable)?;
        assert_eq!(immutable.identity.sha256, Some(digest(b"sqlite snapshot")));
        assert_eq!(store.list()?.external.len(), 2);
        Ok(())
    }

    #[test]
    fn before_after_identity_detects_mutation() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let source = temporary.path().join("source.bin");
        fs::write(&source, b"before")?;
        let before = capture_source_identity(&source, IdentityStrength::Sha256)?;
        fs::write(&source, b"after!")?;
        let check = check_source_identity(&source, &before)?;
        assert!(!check.unchanged);
        assert_ne!(check.before.sha256, check.after.sha256);
        Ok(())
    }

    #[test]
    fn registry_temp_event_is_ignored_and_removal_is_only_a_plan() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let payload = b"installed";
        let plan = plan_with_bytes(payload)?;
        let prepared = store.prepare_install(&plan)?;
        write_staged(&prepared, payload)?;
        let receipt = store.activate(&plan, TransferSummary::for_plan(&plan, Utc::now(), false))?;
        let key = installation_key(&plan.installation_id)?;
        let registry_entry = store.installations_registry_root().join(key);
        fs::write(registry_entry.join(".interrupted.tmp"), b"partial JSON")?;
        assert_eq!(store.list()?.managed, vec![receipt.clone()]);

        let plan = store.plan_removal(&plan.installation_id)?;
        assert_eq!(plan.kind, RemovalKind::ManagedPackage);
        assert!(plan.requires_explicit_confirmation);
        let installed = receipt
            .installed_relative_path
            .ok_or_else(|| io::Error::other("ready receipt had no path"))?;
        assert!(store.root().join(installed).exists());
        assert_eq!(store.list()?.managed.len(), 1);
        Ok(())
    }

    #[test]
    fn non_ready_receipt_remains_inspectable_but_not_visible() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let plan = plan_with_bytes(b"failed")?;
        let receipt = InstallReceipt {
            schema: INSTALL_RECEIPT_SCHEMA.to_string(),
            installation_id: plan.installation_id.clone(),
            resource_id: plan.resource_id.clone(),
            release_id: plan.release_id.clone(),
            representation_id: plan.representation_id.clone(),
            format: plan.format.clone(),
            catalog_authority: plan.catalog_authority.clone(),
            resolved: plan.resolved.clone(),
            plan_sha256: plan.plan_sha256.clone(),
            state: InstallationState::Failed,
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            network_attempted: Some(false),
            network_used: false,
            downloaded_bytes: 0,
            unverified_staged_bytes: 0,
            installed_relative_path: None,
            artifacts: Vec::new(),
            acquisitions: Vec::new(),
            failure: Some(InformationError::new(
                ErrorClass::Io,
                "staging_interrupted",
                "staging did not complete",
            )),
        };
        store.record_non_ready_receipt(&receipt)?;
        assert!(store.list()?.managed.is_empty());
        assert!(store.get(&plan.installation_id)?.is_none());
        assert_eq!(
            store.get_receipt(&plan.installation_id)?,
            Some(receipt.clone())
        );
        assert_eq!(store.list_receipts()?, vec![receipt]);

        let prepared = store.prepare_install(&plan)?;
        write_staged(&prepared, b"failed")?;
        let ready = store.activate(&plan, TransferSummary::for_plan(&plan, Utc::now(), false))?;
        assert_eq!(store.list()?.managed, vec![ready.clone()]);
        assert_eq!(store.list_receipts()?, vec![ready.clone()]);
        let history = store.receipt_history(&plan.installation_id)?;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].state, InstallationState::Failed);
        assert_eq!(history[1], ready);
        let key = installation_key(&plan.installation_id)?;
        let event_count = read_directory(&store.installations_registry_root().join(key))?
            .into_iter()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == "json")
            })
            .count();
        assert_eq!(event_count, 2);
        Ok(())
    }

    #[test]
    fn receipt_bytes_are_derived_from_verified_files() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let plan = plan_with_bytes(b"payload")?;
        let prepared = store.prepare_install(&plan)?;
        write_staged(&prepared, b"payload")?;
        let transfer = TransferSummary {
            started_at: Utc::now(),
            acquisitions: TransferSummary::for_plan(&plan, Utc::now(), true).acquisitions,
        };
        let receipt = store.activate(&plan, transfer)?;
        assert_eq!(receipt.downloaded_bytes, plan.total_download_bytes);
        assert_eq!(store.list()?.managed.len(), 1);
        Ok(())
    }

    #[test]
    fn competing_mutation_fails_fast_on_store_lock() -> Result<(), Box<dyn Error>> {
        let temporary = TempDir::new()?;
        let store = ManagedStore::open(temporary.path().join("store"))?;
        let plan = plan_with_bytes(b"payload")?;
        let _held = store.try_lock()?;
        assert!(matches!(
            store.prepare_install(&plan),
            Err(StoreError::StoreBusy)
        ));
        Ok(())
    }
}
