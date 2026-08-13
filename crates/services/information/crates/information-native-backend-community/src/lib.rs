#![forbid(unsafe_code)]

//! Strict, compiled, read-only retrieval for Community Archive SQLite files.
//!
//! The profile in this crate is intentionally not configurable: table names,
//! columns, FTS options, lookup vocabularies, and supporting indexes are all
//! checked against the schema currently used by the canonical Community
//! Archive. Every operation opens a fresh read-only connection, enables
//! `query_only`, disables `trusted_schema`, installs a progress deadline, and
//! checks the source identity before and after its read transaction. Both
//! access modes use an immutable URI so SQLite cannot create or mutate source
//! sidecars. `LiveReadOnly` means that identity is rebound for each operation;
//! it still requires a quiescent source with no non-empty WAL or journal.

use information_native_retrieval::{
    BackendDescriptor, BackendHealth, BackendHealthStatus, BackendReadResult, BackendSearchResult,
    LookupRequest, ReadRequest, ResourceBackend,
};
use information_native_types::{
    ErrorClass, EvidenceHit, EvidenceLocator, EvidenceScore, ExternalAccessMode,
    InformationCapability, InformationError, InformationQuery, Provenance, QuerySyntax,
    RedistributionPolicy, ReleaseId, RepresentationId, ResourceId, RetrievalPurpose,
    RightsStatement, ScoreSemantics, SourceIdentity, UsePermission, UsePolicy,
    evidence_text_sha256,
};
use rusqlite::ffi::ErrorCode;
use rusqlite::limits::Limit;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, params_from_iter};
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

pub const PROFILE_NAME: &str = "community-archive.messages.v28";
pub const PROFILE_USER_VERSION: i64 = 28;

const MAX_SNIPPET_CHARS: u32 = 4_096;
const MAX_QUERY_TERMS: usize = 64;
const MAX_FIELD_FILTERS: usize = 16;
const MAX_DOCUMENT_FILTERS: usize = 256;
const MAX_SQLITE_BINDS: usize = 384;
const MAX_SOURCE_OBSERVATIONS: usize = 128;
const MAX_LOOKUP_KEY_CHARS: usize = 8_192;

static NEXT_BACKEND_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

/// Configuration for one Community Archive database.
///
/// `use_policy` is intersected with the compiled private-use ceiling. It can
/// remove authority but cannot enable excerpt export or redistribution.
/// Private messages also deny model context unless both `model_context` is
/// `Allowed` and `allow_private_model_context` is explicitly true.
#[derive(Debug, Clone)]
pub struct CommunityArchiveBackendConfig {
    pub backend_id: String,
    pub label: String,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub path: PathBuf,
    pub access_mode: ExternalAccessMode,
    pub publisher: String,
    /// Optional expected whole-file digest. Only immutable mode accepts it.
    pub verified_source_sha256: Option<String>,
    /// Optional registration identity bound to a verified whole-file digest.
    pub verified_source_identity: Option<SourceIdentity>,
    /// Source-wide rights statements applied in addition to the mandatory
    /// per-record unknown/private-use statement.
    pub rights: Vec<RightsStatement>,
    pub use_policy: UsePolicy,
    pub allow_private_model_context: bool,
    pub max_snippet_chars: u32,
}

impl CommunityArchiveBackendConfig {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backend_id: impl Into<String>,
        label: impl Into<String>,
        resource_id: ResourceId,
        release_id: ReleaseId,
        representation_id: RepresentationId,
        path: impl Into<PathBuf>,
        access_mode: ExternalAccessMode,
        publisher: impl Into<String>,
    ) -> Self {
        Self {
            backend_id: backend_id.into(),
            label: label.into(),
            resource_id,
            release_id,
            representation_id,
            path: path.into(),
            access_mode,
            publisher: publisher.into(),
            verified_source_sha256: None,
            verified_source_identity: None,
            rights: Vec::new(),
            use_policy: UsePolicy::default(),
            allow_private_model_context: false,
            max_snippet_chars: 900,
        }
    }

    fn validate(&self) -> Result<(), InformationError> {
        if self.backend_id.trim().is_empty()
            || self.label.trim().is_empty()
            || self.publisher.trim().is_empty()
        {
            return Err(input_error(
                "invalid_community_backend_config",
                "backend id, label, and publisher must not be empty",
            ));
        }
        if self.max_snippet_chars == 0 || self.max_snippet_chars > MAX_SNIPPET_CHARS {
            return Err(input_error(
                "invalid_community_backend_config",
                "snippet budget is outside the supported bounds",
            ));
        }
        if let Some(digest) = &self.verified_source_sha256 {
            validate_sha256_config(digest)?;
            if self.access_mode == ExternalAccessMode::LiveReadOnly {
                return Err(input_error(
                    "live_community_static_digest_forbidden",
                    "live Community Archive sources cannot advertise a static whole-file digest",
                ));
            }
        }
        if let Some(identity) = &self.verified_source_identity {
            identity.validate().map_err(|error| {
                input_error(
                    "invalid_verified_community_source_identity",
                    format!("verified Community Archive identity is invalid: {error}"),
                )
            })?;
            if identity.sha256.is_none() || identity.modified_unix_nanos.is_none() {
                return Err(input_error(
                    "incomplete_verified_community_source_identity",
                    "verified Community Archive identity requires a digest and modification timestamp",
                ));
            }
            #[cfg(unix)]
            if identity.device.is_none() || identity.inode.is_none() {
                return Err(input_error(
                    "incomplete_verified_community_source_identity",
                    "verified Community Archive identity requires device and inode on this platform",
                ));
            }
            if self.access_mode == ExternalAccessMode::LiveReadOnly {
                return Err(input_error(
                    "live_community_static_identity_forbidden",
                    "live Community Archive sources cannot advertise a static verified identity",
                ));
            }
            if self
                .verified_source_sha256
                .as_ref()
                .zip(identity.sha256.as_ref())
                .is_some_and(|(left, right)| normalize_sha256(left) != normalize_sha256(right))
            {
                return Err(input_error(
                    "conflicting_verified_community_source_digest",
                    "verified Community Archive digest fields disagree",
                ));
            }
        }
        for statement in &self.rights {
            statement.validate().map_err(|error| {
                input_error(
                    "invalid_community_rights_statement",
                    format!("Community Archive rights statement is invalid: {error}"),
                )
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    bytes: u64,
    modified_unix_nanos: Option<u128>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_unix_seconds: i64,
    #[cfg(unix)]
    changed_subsec_nanos: i64,
}

impl FileIdentity {
    fn observe(path: &Path) -> Result<Self, InformationError> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            io_error(
                "community_source_metadata_failed",
                format!("cannot inspect Community Archive metadata: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(integrity_error(
                "community_source_is_symlink",
                "canonical Community Archive source is a symbolic link",
            ));
        }
        Self::from_metadata(&metadata)
    }

    fn from_metadata(metadata: &Metadata) -> Result<Self, InformationError> {
        if !metadata.is_file() {
            return Err(input_error(
                "community_source_not_file",
                "Community Archive source is not a regular file",
            ));
        }
        let modified_unix_nanos = metadata.modified().ok().and_then(system_time_nanos);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                bytes: metadata.len(),
                modified_unix_nanos,
                device: metadata.dev(),
                inode: metadata.ino(),
                changed_unix_seconds: metadata.ctime(),
                changed_subsec_nanos: metadata.ctime_nsec(),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                bytes: metadata.len(),
                modified_unix_nanos,
            })
        }
    }

    fn source_identity(&self) -> SourceIdentity {
        SourceIdentity {
            bytes: self.bytes,
            modified_unix_nanos: self.modified_unix_nanos,
            #[cfg(unix)]
            device: Some(self.device),
            #[cfg(not(unix))]
            device: None,
            #[cfg(unix)]
            inode: Some(self.inode),
            #[cfg(not(unix))]
            inode: None,
            sha256: None,
        }
    }

    fn update_hasher(&self, hasher: &mut Sha256) {
        hasher.update(self.bytes.to_le_bytes());
        match self.modified_unix_nanos {
            Some(value) => {
                hasher.update([1]);
                hasher.update(value.to_le_bytes());
            }
            None => hasher.update([0]),
        }
        #[cfg(unix)]
        {
            hasher.update(self.device.to_le_bytes());
            hasher.update(self.inode.to_le_bytes());
            hasher.update(self.changed_unix_seconds.to_le_bytes());
            hasher.update(self.changed_subsec_nanos.to_le_bytes());
        }
    }
}

fn system_time_nanos(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|value| value.as_nanos())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SidecarIdentity {
    Absent,
    Present(FileIdentity),
}

impl SidecarIdentity {
    fn observe(path: &Path, label: &'static str) -> Result<Self, InformationError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Err(integrity_error(
                "community_sidecar_is_symlink",
                format!("SQLite {label} sidecar is a symbolic link"),
            )),
            Ok(metadata) if !metadata.is_file() => Err(integrity_error(
                "community_sidecar_not_file",
                format!("SQLite {label} sidecar is not a regular file"),
            )),
            Ok(metadata) => Ok(Self::Present(FileIdentity::from_metadata(&metadata)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::Absent),
            Err(error) => Err(io_error(
                "community_sidecar_metadata_failed",
                format!("cannot inspect SQLite {label} sidecar: {error}"),
            )),
        }
    }

    fn is_nonempty(&self) -> bool {
        matches!(self, Self::Present(identity) if identity.bytes > 0)
    }

    fn update_hasher(&self, label: &[u8], hasher: &mut Sha256) {
        hasher.update(label);
        match self {
            Self::Absent => hasher.update([0]),
            Self::Present(identity) => {
                hasher.update([1]);
                identity.update_hasher(hasher);
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SidecarSet {
    wal: SidecarIdentity,
    shm: SidecarIdentity,
    journal: SidecarIdentity,
}

impl SidecarSet {
    fn observe(path: &Path) -> Result<Self, InformationError> {
        Ok(Self {
            wal: SidecarIdentity::observe(&sibling_sidecar_path(path, "-wal")?, "WAL")?,
            shm: SidecarIdentity::observe(&sibling_sidecar_path(path, "-shm")?, "SHM")?,
            journal: SidecarIdentity::observe(
                &sibling_sidecar_path(path, "-journal")?,
                "rollback journal",
            )?,
        })
    }

    fn ensure_snapshot_safe(&self) -> Result<(), InformationError> {
        if self.wal.is_nonempty() {
            return Err(integrity_error(
                "community_nonempty_wal",
                "write-free Community Archive snapshots reject a non-empty sibling WAL",
            ));
        }
        if self.journal.is_nonempty() {
            return Err(integrity_error(
                "community_nonempty_journal",
                "write-free Community Archive snapshots reject a non-empty sibling rollback journal",
            ));
        }
        Ok(())
    }

    fn update_hasher(&self, hasher: &mut Sha256) {
        self.wal.update_hasher(b"wal\0", hasher);
        self.shm.update_hasher(b"shm\0", hasher);
        self.journal.update_hasher(b"journal\0", hasher);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SqliteSnapshotFacts {
    schema_version: i64,
    user_version: i64,
    page_count: i64,
    freelist_count: i64,
    journal_mode: String,
}

impl SqliteSnapshotFacts {
    fn observe(connection: &Connection) -> Result<Self, InformationError> {
        Ok(Self {
            schema_version: pragma_i64(connection, "schema_version")?,
            user_version: pragma_i64(connection, "user_version")?,
            page_count: pragma_i64(connection, "page_count")?,
            freelist_count: pragma_i64(connection, "freelist_count")?,
            journal_mode: connection
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .map_err(map_sqlite_error)?,
        })
    }

    fn update_hasher(&self, hasher: &mut Sha256) {
        for value in [
            self.schema_version,
            self.user_version,
            self.page_count,
            self.freelist_count,
        ] {
            hasher.update(value.to_le_bytes());
        }
        hasher.update(self.journal_mode.len().to_le_bytes());
        hasher.update(self.journal_mode.as_bytes());
    }
}

fn pragma_i64(connection: &Connection, pragma: &str) -> Result<i64, InformationError> {
    connection
        .pragma_query_value(None, pragma, |row| row.get(0))
        .map_err(map_sqlite_error)
}

/// Read-only backend for the compiled Community Archive v28 profile.
pub struct CommunityArchiveBackend {
    descriptor: BackendDescriptor,
    path: PathBuf,
    access_mode: ExternalAccessMode,
    publisher: String,
    immutable_source_sha256: Option<String>,
    rights: Vec<RightsStatement>,
    allow_private_model_context: bool,
    max_snippet_chars: u32,
    initial_identity: FileIdentity,
    initial_sidecars: SidecarSet,
    instance_id: u64,
    operation_sequence: AtomicU64,
}

impl CommunityArchiveBackend {
    pub fn open(config: CommunityArchiveBackendConfig) -> Result<Self, InformationError> {
        config.validate()?;
        let path = fs::canonicalize(&config.path).map_err(|error| {
            io_error(
                "community_source_canonicalize_failed",
                format!("cannot resolve Community Archive source: {error}"),
            )
        })?;
        let initial_identity = FileIdentity::observe(&path)?;
        let initial_sidecars = SidecarSet::observe(&path)?;
        initial_sidecars.ensure_snapshot_safe()?;
        if let Some(expected_identity) = &config.verified_source_identity {
            verify_registered_identity(&initial_identity, expected_identity)?;
        }
        let expected_sha256 = config
            .verified_source_identity
            .as_ref()
            .and_then(|identity| identity.sha256.as_deref())
            .or(config.verified_source_sha256.as_deref())
            .map(normalize_sha256);
        let immutable_source_sha256 = match config.access_mode {
            ExternalAccessMode::LiveReadOnly => None,
            ExternalAccessMode::ImmutableReadOnly => {
                let observed = hash_immutable_source(&path, &initial_identity)?;
                let sidecars_after_hash = SidecarSet::observe(&path)?;
                sidecars_after_hash.ensure_snapshot_safe()?;
                if sidecars_after_hash != initial_sidecars {
                    return Err(integrity_error(
                        "immutable_community_sidecar_changed_during_hash",
                        "Community Archive sidecars changed during whole-file verification",
                    ));
                }
                if expected_sha256
                    .as_ref()
                    .is_some_and(|expected| expected != &observed)
                {
                    return Err(integrity_error(
                        "immutable_community_digest_mismatch",
                        "immutable Community Archive does not match its expected SHA-256 digest",
                    ));
                }
                Some(observed)
            }
        };
        let use_policy = intersect_rights_into_use_policy(
            intersect_policy(config.use_policy, profile_policy_ceiling()),
            &config.rights,
        );
        let backend = Self {
            descriptor: BackendDescriptor {
                backend_id: config.backend_id,
                resource_id: config.resource_id,
                release_id: config.release_id,
                representation_id: config.representation_id,
                label: config.label,
                capabilities: BTreeSet::from([
                    InformationCapability::LexicalSearch,
                    InformationCapability::ArticleRead,
                    InformationCapability::RecordLookup,
                    InformationCapability::RandomAccess,
                ]),
                use_policy,
            },
            path,
            access_mode: config.access_mode,
            publisher: config.publisher,
            immutable_source_sha256,
            rights: config.rights,
            allow_private_model_context: config.allow_private_model_context,
            max_snippet_chars: config.max_snippet_chars,
            initial_identity,
            initial_sidecars,
            instance_id: NEXT_BACKEND_INSTANCE_ID.fetch_add(1, Ordering::Relaxed),
            operation_sequence: AtomicU64::new(1),
        };
        backend.with_connection(5_000, |_connection, _snapshot| Ok(()))?;
        Ok(backend)
    }

    #[must_use]
    pub fn source_identity(&self) -> SourceIdentity {
        let mut identity = self.initial_identity.source_identity();
        identity.sha256 = self
            .immutable_source_sha256
            .as_ref()
            .map(|digest| format!("sha256:{digest}"));
        identity
    }

    /// Return the verified immutable digest or a volatile live reference.
    #[must_use]
    pub fn source_fingerprint(&self) -> String {
        match &self.immutable_source_sha256 {
            Some(digest) => format!("sha256:{digest}"),
            None => {
                let mut hasher = Sha256::new();
                hasher.update(b"information-native.community-archive-live-reference.v1\0");
                hasher.update(self.instance_id.to_le_bytes());
                hasher.update(self.next_operation_sequence().to_le_bytes());
                hasher.update(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_nanos())
                        .to_le_bytes(),
                );
                self.initial_identity.update_hasher(&mut hasher);
                format!("volatile-community-reference-v1:{:x}", hasher.finalize())
            }
        }
    }

    fn next_operation_sequence(&self) -> u64 {
        self.operation_sequence.fetch_add(1, Ordering::Relaxed)
    }

    fn snapshot_fingerprint(
        &self,
        identity: &FileIdentity,
        sidecars: &SidecarSet,
        facts: &SqliteSnapshotFacts,
    ) -> String {
        let mut hasher = Sha256::new();
        match &self.immutable_source_sha256 {
            Some(digest) => format!("sha256:{digest}"),
            None => {
                hasher.update(b"information-native.community-archive-live-snapshot.v1\0");
                hasher.update(self.instance_id.to_le_bytes());
                hasher.update(self.next_operation_sequence().to_le_bytes());
                hasher.update(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_nanos())
                        .to_le_bytes(),
                );
                identity.update_hasher(&mut hasher);
                sidecars.update_hasher(&mut hasher);
                facts.update_hasher(&mut hasher);
                format!("volatile-sqlite-live-snapshot-v1:{:x}", hasher.finalize())
            }
        }
    }

    fn ensure_purpose_allowed(
        &self,
        policy: UsePolicy,
        purpose: RetrievalPurpose,
    ) -> Result<(), InformationError> {
        if policy.permission_for(purpose) == UsePermission::Allowed {
            return Ok(());
        }
        let mut error = InformationError::new(
            ErrorClass::Permission,
            "retrieval_purpose_not_permitted",
            "the requested purpose is not explicitly allowed by the effective use policy",
        );
        error.resource_id = Some(self.descriptor.resource_id.clone());
        error.representation_id = Some(self.descriptor.representation_id.clone());
        Err(error)
    }

    fn with_connection<T>(
        &self,
        timeout_ms: u64,
        operation: impl FnOnce(&Connection, &str) -> Result<T, InformationError>,
    ) -> Result<T, InformationError> {
        let before = FileIdentity::observe(&self.path)?;
        let before_sidecars = SidecarSet::observe(&self.path)?;
        before_sidecars.ensure_snapshot_safe()?;
        if self.access_mode == ExternalAccessMode::ImmutableReadOnly {
            if before != self.initial_identity {
                return Err(integrity_error(
                    "immutable_community_identity_changed",
                    "immutable Community Archive identity changed after mounting",
                ));
            }
            if before_sidecars != self.initial_sidecars {
                return Err(integrity_error(
                    "immutable_community_sidecar_changed",
                    "immutable Community Archive sidecars changed after mounting",
                ));
            }
        }

        let connection = open_read_only_connection(&self.path, self.access_mode, timeout_ms)?;
        connection
            .execute_batch("BEGIN DEFERRED TRANSACTION")
            .map_err(map_sqlite_error)?;
        let result = (|| {
            let connected_identity = FileIdentity::observe(&self.path)?;
            if connected_identity != before {
                return Err(integrity_error(
                    "community_identity_changed_before_read",
                    "Community Archive identity changed while opening a read snapshot",
                ));
            }
            validate_community_schema(&connection)?;
            let facts = SqliteSnapshotFacts::observe(&connection)?;
            let snapshot_sidecars = SidecarSet::observe(&self.path)?;
            snapshot_sidecars.ensure_snapshot_safe()?;
            if snapshot_sidecars != before_sidecars {
                return Err(integrity_error(
                    "community_sidecar_changed",
                    "Community Archive sidecars changed during snapshot setup",
                ));
            }
            let fingerprint =
                self.snapshot_fingerprint(&connected_identity, &snapshot_sidecars, &facts);
            operation(&connection, &fingerprint)
        })();
        let transaction_result = if result.is_ok() {
            connection.execute_batch("COMMIT")
        } else {
            connection.execute_batch("ROLLBACK")
        };
        if let Err(error) = transaction_result {
            return Err(map_sqlite_error(error));
        }

        let after = FileIdentity::observe(&self.path)?;
        if before != after {
            return Err(integrity_error(
                "community_identity_changed_during_read",
                "Community Archive identity changed during a read; results were discarded",
            ));
        }
        let after_sidecars = SidecarSet::observe(&self.path)?;
        after_sidecars.ensure_snapshot_safe()?;
        if after_sidecars != before_sidecars {
            return Err(integrity_error(
                "community_sidecar_changed",
                "Community Archive sidecars changed during a read",
            ));
        }
        result
    }

    fn search_connection(
        &self,
        connection: &Connection,
        snapshot_fingerprint: &str,
        query: &InformationQuery,
    ) -> Result<BackendSearchResult, InformationError> {
        self.ensure_purpose_allowed(self.descriptor.use_policy, query.purpose)?;
        query.validate().map_err(|error| {
            input_error(
                "invalid_query",
                format!("query contract is invalid: {error}"),
            )
        })?;
        validate_query_for_backend(query, &self.descriptor)?;
        validate_query_filters(query)?;

        let started = Instant::now();
        let fts_query = compile_fts_query(&query.text, query.syntax)?;
        let limit = query.budget.max_hits.min(query.budget.max_hits_per_backend);
        let fetch_limit = i64::from(limit).saturating_add(1);
        let raw_hits = run_fts_query(
            connection,
            &fts_query,
            query,
            fetch_limit,
            self.max_snippet_chars,
        )
        .map_err(map_search_sqlite_error)?;

        let mut complete = raw_hits.len() <= limit as usize;
        let mut warnings = Vec::new();
        let mut hits = Vec::with_capacity(limit as usize);
        let mut remaining_chars = query.budget.max_context_chars as usize;
        let mut policy_omissions = 0_usize;
        for raw in raw_hits {
            if hits.len() >= limit as usize {
                complete = false;
                break;
            }
            let provenance = load_message_provenance(connection, &raw)?;
            let policy = self.policy_for_record(provenance.private);
            if policy.permission_for(query.purpose) != UsePermission::Allowed {
                policy_omissions = policy_omissions.saturating_add(1);
                complete = false;
                continue;
            }
            let remaining_slots = (limit as usize).saturating_sub(hits.len()).max(1);
            let share = remaining_chars / remaining_slots;
            let (snippet, snippet_truncated) =
                truncate_chars(&raw.text, share.min(self.max_snippet_chars as usize));
            remaining_chars = remaining_chars.saturating_sub(snippet.chars().count());
            if snippet_truncated {
                complete = false;
            }
            let rank = u32::try_from(hits.len().saturating_add(1)).map_err(|_| {
                integrity_error(
                    "community_rank_overflow",
                    "Community Archive result rank exceeded supported bounds",
                )
            })?;
            hits.push(self.message_evidence(
                raw,
                provenance,
                policy,
                snippet,
                rank,
                ScoreSemantics::LowerIsBetter,
                snapshot_fingerprint,
            )?);
        }
        if policy_omissions > 0 {
            warnings.push(
                "one or more matches were omitted by per-record privacy or use policy".to_string(),
            );
        }
        if !complete {
            warnings.push("result or text limits made this response partial".to_string());
        }
        warnings.sort();
        warnings.dedup();
        Ok(BackendSearchResult {
            complete,
            warnings,
            hits,
            elapsed_ms: elapsed_millis(started),
        })
    }

    fn read_connection(
        &self,
        connection: &Connection,
        snapshot_fingerprint: &str,
        request: &ReadRequest,
    ) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.descriptor.use_policy, request.purpose)?;
        validate_direct_read_budget(request.max_context_chars, request.timeout_ms)?;
        validate_read_identity(
            &self.descriptor,
            &request.resource_id,
            &request.release_id,
            &request.representation_id,
        )?;
        let EvidenceLocator::Record { collection, key } = &request.locator else {
            return Err(InformationError::new(
                ErrorClass::Unsupported,
                "unsupported_community_locator",
                "Community Archive reads require a record locator",
            ));
        };
        validate_collection(collection.as_deref())?;
        let locator = parse_record_key(key)?;
        self.read_locator(
            connection,
            snapshot_fingerprint,
            &locator,
            request.max_context_chars,
            request.purpose,
        )
    }

    fn lookup_connection(
        &self,
        connection: &Connection,
        snapshot_fingerprint: &str,
        request: &LookupRequest,
    ) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.descriptor.use_policy, request.purpose)?;
        validate_direct_read_budget(request.max_context_chars, request.timeout_ms)?;
        validate_read_identity(
            &self.descriptor,
            &request.resource_id,
            &request.release_id,
            &request.representation_id,
        )?;
        validate_collection(request.collection.as_deref())?;
        let locator = parse_record_key(&request.key)?;
        self.read_locator(
            connection,
            snapshot_fingerprint,
            &locator,
            request.max_context_chars,
            request.purpose,
        )
    }

    fn read_locator(
        &self,
        connection: &Connection,
        snapshot_fingerprint: &str,
        locator: &RecordKey,
        max_context_chars: u32,
        purpose: RetrievalPurpose,
    ) -> Result<BackendReadResult, InformationError> {
        let started = Instant::now();
        let raw = load_message(connection, locator, max_context_chars)?;
        locator.verify(&raw)?;
        let provenance = load_message_provenance(connection, &raw)?;
        let policy = self.policy_for_record(provenance.private);
        self.ensure_purpose_allowed(policy, purpose)?;
        let (snippet, truncated) = truncate_chars(&raw.text, max_context_chars as usize);
        let hit = self.message_evidence(
            raw,
            provenance,
            policy,
            snippet,
            1,
            ScoreSemantics::RankOnly,
            snapshot_fingerprint,
        )?;
        Ok(BackendReadResult {
            complete: !truncated,
            warnings: if truncated {
                vec!["message text was truncated to the read budget".to_string()]
            } else {
                Vec::new()
            },
            hit,
            elapsed_ms: elapsed_millis(started),
        })
    }

    fn policy_for_record(&self, private: bool) -> UsePolicy {
        let mut policy = self.descriptor.use_policy;
        if private {
            if !self.allow_private_model_context {
                policy.model_context = UsePermission::Forbidden;
            }
            policy.excerpt_export = UsePermission::Forbidden;
            policy.redistribution = UsePermission::Forbidden;
        }
        policy
    }

    #[allow(clippy::too_many_arguments)]
    fn message_evidence(
        &self,
        raw: RawMessage,
        source: MessageProvenance,
        use_policy: UsePolicy,
        snippet: String,
        rank: u32,
        semantics: ScoreSemantics,
        snapshot_fingerprint: &str,
    ) -> Result<EvidenceHit, InformationError> {
        validate_raw_message(&raw)?;
        let key = canonical_record_key(&raw);
        let document_id = upstream_record_id(&raw);
        let logical_uri = format!("community-archive://messages/rowid/{}", raw.rowid);
        let mut rights = self.rights.clone();
        rights.push(RightsStatement {
            scope: format!("message:rowid:{}", raw.rowid),
            expression: if source.private {
                "private archive record; copyright and reuse rights otherwise unknown".to_string()
            } else {
                "copyright and reuse rights unknown; private-use retrieval only".to_string()
            },
            license_url: None,
            license_text_sha256: None,
            attribution: Some(self.publisher.clone()),
            obligations: Vec::new(),
            redistribution: RedistributionPolicy::PrivateUseOnly,
        });
        let use_policy = intersect_rights_into_use_policy(use_policy, &rights);
        let source_records = source
            .observations
            .iter()
            .map(|observation| {
                json!({
                    "source_collection": observation.source_collection,
                    "source_record_hash": observation.source_record_hash,
                    "source_ordinal": observation.source_ordinal,
                    "private_archive": observation.private,
                })
            })
            .collect::<Vec<_>>();
        let mut metadata = BTreeMap::from([
            ("profile".to_string(), json!(PROFILE_NAME)),
            ("message_rowid".to_string(), json!(raw.rowid.to_string())),
            ("message_kind".to_string(), json!(&raw.kind)),
            (
                "source_collection".to_string(),
                json!(&raw.source_collection),
            ),
            ("created_at".to_string(), json!(&raw.created_at)),
            ("private_archive".to_string(), json!(source.private)),
            (
                "source_records".to_string(),
                JsonValue::Array(source_records.clone()),
            ),
            (
                "excerpt_hash_basis".to_string(),
                json!(
                    "information_native_types::evidence_text_sha256 over exact snippet and context"
                ),
            ),
            (
                "access_mode".to_string(),
                json!(access_mode_name(self.access_mode)),
            ),
        ]);
        insert_optional_string(
            &mut metadata,
            "tweet_id",
            raw.tweet_id.map(|id| id.to_string()),
        );
        insert_optional_string(&mut metadata, "note_id", raw.note_id.clone());
        insert_optional_string(
            &mut metadata,
            "conversation_id",
            raw.conversation_id.map(|id| id.to_string()),
        );
        insert_optional_string(
            &mut metadata,
            "thread_id",
            raw.thread_id.map(|id| id.to_string()),
        );

        let creator = raw
            .username
            .as_ref()
            .map(|username| format!("@{username}"))
            .or_else(|| raw.display_name.clone());
        let title = match (&creator, &raw.created_at) {
            (Some(creator), Some(created_at)) => format!("{creator} — {created_at}"),
            (Some(creator), None) => creator.clone(),
            (None, Some(created_at)) => format!("Community Archive message — {created_at}"),
            (None, None) => format!("Community Archive message {}", raw.rowid),
        };
        let score = if semantics == ScoreSemantics::RankOnly {
            1.0
        } else {
            raw.score
        };
        let context = String::new();
        let hit = EvidenceHit {
            evidence_id: format!(
                "{}:{}:{}:message-rowid:{}",
                self.descriptor.resource_id,
                self.descriptor.release_id,
                self.descriptor.representation_id,
                raw.rowid
            ),
            resource_id: self.descriptor.resource_id.clone(),
            release_id: self.descriptor.release_id.clone(),
            representation_id: self.descriptor.representation_id.clone(),
            backend_id: self.descriptor.backend_id.clone(),
            rank,
            score: EvidenceScore {
                value: score,
                semantics,
                fused_value: None,
            },
            title,
            creator,
            snippet: snippet.clone(),
            context: context.clone(),
            excerpt_sha256: evidence_text_sha256(&snippet, &context),
            source_fingerprint: Some(source.source_fingerprint.clone()),
            document_id: Some(document_id.clone()),
            passage_id: Some(format!("message-rowid:{}", raw.rowid)),
            locator: EvidenceLocator::Record {
                collection: Some("messages".to_string()),
                key,
            },
            source_uri: Some(logical_uri.clone()),
            provenance: Provenance {
                publisher: self.publisher.clone(),
                source_uri: logical_uri,
                upstream_record_id: Some(document_id),
                source_inputs: source
                    .hashes
                    .iter()
                    .map(|digest| format!("sha256:{digest}"))
                    .collect(),
                transformation: Some(
                    "read-only Community Archive SQLite/FTS5 record retrieval".to_string(),
                ),
                metadata: BTreeMap::from([
                    ("profile".to_string(), json!(PROFILE_NAME)),
                    (
                        "snapshot_fingerprint".to_string(),
                        json!(snapshot_fingerprint),
                    ),
                    (
                        "source_records".to_string(),
                        JsonValue::Array(source_records),
                    ),
                ]),
            },
            rights,
            use_policy,
            metadata,
        };
        hit.validate().map_err(|error| {
            integrity_error(
                "invalid_community_evidence",
                format!("generated Community Archive evidence failed validation: {error}"),
            )
        })?;
        Ok(hit)
    }
}

impl ResourceBackend for CommunityArchiveBackend {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    fn health(&self) -> BackendHealth {
        match self.with_connection(1_000, |connection, _snapshot| {
            connection
                .query_row("SELECT 1", [], |_row| Ok(()))
                .map_err(map_sqlite_error)
        }) {
            Ok(()) => BackendHealth::ready(),
            Err(error) => BackendHealth {
                status: BackendHealthStatus::Unavailable,
                message: format!("{}: {}", error.code, error.safe_message),
            },
        }
    }

    fn search(&self, query: &InformationQuery) -> Result<BackendSearchResult, InformationError> {
        self.ensure_purpose_allowed(self.descriptor.use_policy, query.purpose)?;
        query.validate().map_err(|error| {
            input_error(
                "invalid_query",
                format!("query contract is invalid: {error}"),
            )
        })?;
        validate_query_for_backend(query, &self.descriptor)?;
        self.with_connection(query.budget.timeout_ms, |connection, snapshot| {
            self.search_connection(connection, snapshot, query)
        })
    }

    fn read(&self, request: &ReadRequest) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.descriptor.use_policy, request.purpose)?;
        validate_direct_read_budget(request.max_context_chars, request.timeout_ms)?;
        self.with_connection(request.timeout_ms, |connection, snapshot| {
            self.read_connection(connection, snapshot, request)
        })
    }

    fn lookup(&self, request: &LookupRequest) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.descriptor.use_policy, request.purpose)?;
        validate_direct_read_budget(request.max_context_chars, request.timeout_ms)?;
        self.with_connection(request.timeout_ms, |connection, snapshot| {
            self.lookup_connection(connection, snapshot, request)
        })
    }
}

fn profile_policy_ceiling() -> UsePolicy {
    UsePolicy {
        local_search: UsePermission::Allowed,
        model_context: UsePermission::Allowed,
        excerpt_export: UsePermission::Forbidden,
        redistribution: UsePermission::Forbidden,
        attribution_required: true,
    }
}

fn intersect_policy(left: UsePolicy, right: UsePolicy) -> UsePolicy {
    UsePolicy {
        local_search: intersect_permission(left.local_search, right.local_search),
        model_context: intersect_permission(left.model_context, right.model_context),
        excerpt_export: intersect_permission(left.excerpt_export, right.excerpt_export),
        redistribution: intersect_permission(left.redistribution, right.redistribution),
        attribution_required: left.attribution_required || right.attribution_required,
    }
}

fn intersect_rights_into_use_policy(
    mut policy: UsePolicy,
    rights: &[RightsStatement],
) -> UsePolicy {
    for statement in rights {
        let permission = match statement.redistribution {
            RedistributionPolicy::Allowed | RedistributionPolicy::AllowedWithObligations => {
                UsePermission::Allowed
            }
            RedistributionPolicy::PrivateUseOnly | RedistributionPolicy::Forbidden => {
                UsePermission::Forbidden
            }
            RedistributionPolicy::Unknown => UsePermission::Unknown,
        };
        policy.excerpt_export = intersect_permission(policy.excerpt_export, permission);
        policy.redistribution = intersect_permission(policy.redistribution, permission);
        if statement.redistribution == RedistributionPolicy::AllowedWithObligations {
            policy.attribution_required = true;
        }
    }
    policy
}

fn intersect_permission(left: UsePermission, right: UsePermission) -> UsePermission {
    match (left, right) {
        (UsePermission::Forbidden, _) | (_, UsePermission::Forbidden) => UsePermission::Forbidden,
        (UsePermission::Allowed, UsePermission::Allowed) => UsePermission::Allowed,
        _ => UsePermission::Unknown,
    }
}

fn open_read_only_connection(
    path: &Path,
    _access_mode: ExternalAccessMode,
    timeout_ms: u64,
) -> Result<Connection, InformationError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    // Ordinary SQLite read-only WAL locking can write the shared-memory
    // sidecar. This backend has no authority to write anywhere in the source
    // directory, so even live operations use a short-lived immutable URI and
    // are discarded unless main-file and sidecar identities remain unchanged.
    let mut uri = Url::from_file_path(path).map_err(|()| {
        input_error(
            "community_path_not_file_uri",
            "Community Archive path cannot be represented as a file URI",
        )
    })?;
    uri.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
    let connection = Connection::open_with_flags(uri.as_str(), flags | OpenFlags::SQLITE_OPEN_URI)
        .map_err(map_sqlite_error)?;
    let bind_limit = i32::try_from(MAX_SQLITE_BINDS).map_err(|_| {
        integrity_error(
            "community_bind_limit_overflow",
            "compiled Community Archive bind limit exceeds SQLite bounds",
        )
    })?;
    connection.set_limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER, bind_limit);
    if connection.limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER) < bind_limit {
        return Err(InformationError::new(
            ErrorClass::Unsupported,
            "community_bind_limit_too_low",
            "SQLite runtime cannot support the compiled Community Archive bind limit",
        ));
    }
    connection
        .busy_timeout(Duration::from_millis(timeout_ms.min(30_000)))
        .map_err(map_sqlite_error)?;
    connection
        .pragma_update(None, "trusted_schema", false)
        .map_err(map_sqlite_error)?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(map_sqlite_error)?;
    let trusted_schema = pragma_i64(&connection, "trusted_schema")?;
    let query_only = pragma_i64(&connection, "query_only")?;
    if trusted_schema != 0 || query_only != 1 {
        return Err(integrity_error(
            "community_read_only_pragmas_failed",
            "SQLite did not retain trusted_schema=OFF and query_only=ON",
        ));
    }
    let now = Instant::now();
    let deadline = now
        .checked_add(Duration::from_millis(timeout_ms))
        .unwrap_or(now);
    connection.progress_handler(1_000, Some(move || Instant::now() >= deadline));
    Ok(connection)
}

fn sibling_sidecar_path(path: &Path, suffix: &str) -> Result<PathBuf, InformationError> {
    let Some(file_name) = path.file_name() else {
        return Err(input_error(
            "community_path_has_no_file_name",
            "Community Archive path has no file name",
        ));
    };
    let mut sidecar_name = OsString::from(file_name);
    sidecar_name.push(suffix);
    Ok(path.with_file_name(sidecar_name))
}

fn verify_registered_identity(
    observed: &FileIdentity,
    expected: &SourceIdentity,
) -> Result<(), InformationError> {
    let matches = observed.bytes == expected.bytes
        && observed.modified_unix_nanos == expected.modified_unix_nanos
        && {
            #[cfg(unix)]
            {
                expected.device == Some(observed.device) && expected.inode == Some(observed.inode)
            }
            #[cfg(not(unix))]
            {
                expected.device.is_none() && expected.inode.is_none()
            }
        };
    if !matches {
        return Err(integrity_error(
            "verified_community_identity_mismatch",
            "Community Archive no longer matches the identity bound to its verified digest",
        ));
    }
    Ok(())
}

fn hash_immutable_source(
    path: &Path,
    expected_identity: &FileIdentity,
) -> Result<String, InformationError> {
    let file = File::open(path).map_err(|error| {
        io_error(
            "immutable_community_hash_open_failed",
            format!("cannot open immutable Community Archive for verification: {error}"),
        )
    })?;
    let opened_identity = FileIdentity::from_metadata(&file.metadata().map_err(|error| {
        io_error(
            "immutable_community_hash_metadata_failed",
            format!("cannot inspect opened Community Archive: {error}"),
        )
    })?)?;
    if &opened_identity != expected_identity {
        return Err(integrity_error(
            "immutable_community_identity_changed_before_hash",
            "immutable Community Archive identity changed before digest verification",
        ));
    }

    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut hasher = Sha256::new();
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            io_error(
                "immutable_community_hash_read_failed",
                format!("cannot hash immutable Community Archive: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let handle_after =
        FileIdentity::from_metadata(&reader.get_ref().metadata().map_err(|error| {
            io_error(
                "immutable_community_hash_metadata_failed",
                format!("cannot re-inspect opened Community Archive: {error}"),
            )
        })?)?;
    let path_after = FileIdentity::observe(path)?;
    if opened_identity != handle_after || &path_after != expected_identity {
        return Err(integrity_error(
            "immutable_community_identity_changed_during_hash",
            "immutable Community Archive identity changed during digest verification",
        ));
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_sha256_config(value: &str) -> Result<(), InformationError> {
    let normalized = value.strip_prefix("sha256:").unwrap_or(value);
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(input_error(
            "invalid_verified_community_source_sha256",
            "verified Community Archive digest is not a SHA-256 value",
        ));
    }
    Ok(())
}

fn normalize_sha256(value: &str) -> String {
    value
        .strip_prefix("sha256:")
        .unwrap_or(value)
        .to_ascii_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ColumnSpec {
    name: String,
    declared_type: String,
    not_null: i64,
    default_value: Option<String>,
    primary_key_position: i64,
    hidden: i64,
}

impl ColumnSpec {
    fn expected(
        name: &'static str,
        declared_type: &'static str,
        not_null: i64,
        default_value: Option<&'static str>,
        primary_key_position: i64,
        hidden: i64,
    ) -> Self {
        Self {
            name: name.to_string(),
            declared_type: declared_type.to_string(),
            not_null,
            default_value: default_value.map(str::to_string),
            primary_key_position,
            hidden,
        }
    }
}

fn validate_community_schema(connection: &Connection) -> Result<(), InformationError> {
    let user_version = pragma_i64(connection, "user_version")?;
    if user_version != PROFILE_USER_VERSION {
        return Err(integrity_error(
            "community_profile_user_version_mismatch",
            format!(
                "Community Archive user_version is {user_version}; compiled profile requires {PROFILE_USER_VERSION}"
            ),
        ));
    }
    validate_table(
        connection,
        "messages",
        &messages_columns(),
        EXPECTED_MESSAGES_SQL,
    )?;
    validate_table(
        connection,
        "messages_fts",
        &messages_fts_columns(),
        EXPECTED_MESSAGES_FTS_SQL,
    )?;
    validate_table(
        connection,
        "accounts",
        &accounts_columns(),
        EXPECTED_ACCOUNTS_SQL,
    )?;
    validate_table(
        connection,
        "message_kinds",
        &message_kinds_columns(),
        EXPECTED_MESSAGE_KINDS_SQL,
    )?;
    validate_table(
        connection,
        "source_collections",
        &source_collections_columns(),
        EXPECTED_SOURCE_COLLECTIONS_SQL,
    )?;
    validate_table(
        connection,
        "archives",
        &archives_columns(),
        EXPECTED_ARCHIVES_SQL,
    )?;
    validate_table(
        connection,
        "message_sources",
        &message_sources_columns(),
        EXPECTED_MESSAGE_SOURCES_SQL,
    )?;
    validate_schema_object(
        connection,
        "view",
        "messages_fts_content",
        EXPECTED_MESSAGES_FTS_CONTENT_SQL,
    )?;
    validate_schema_object(
        connection,
        "index",
        "idx_messages_tweet_id",
        "CREATE INDEX idx_messages_tweet_id ON messages(tweet_id)",
    )?;
    validate_schema_object(
        connection,
        "index",
        "idx_message_sources_message",
        "CREATE INDEX idx_message_sources_message ON message_sources(message_rowid)",
    )?;
    validate_lookup_rows(connection)?;
    Ok(())
}

fn validate_table(
    connection: &Connection,
    table: &'static str,
    expected_columns: &[ColumnSpec],
    expected_sql: &'static str,
) -> Result<(), InformationError> {
    let pragma = match table {
        "messages" => "PRAGMA table_xinfo(messages)",
        "messages_fts" => "PRAGMA table_xinfo(messages_fts)",
        "accounts" => "PRAGMA table_xinfo(accounts)",
        "message_kinds" => "PRAGMA table_xinfo(message_kinds)",
        "source_collections" => "PRAGMA table_xinfo(source_collections)",
        "archives" => "PRAGMA table_xinfo(archives)",
        "message_sources" => "PRAGMA table_xinfo(message_sources)",
        _ => {
            return Err(integrity_error(
                "community_profile_internal_table",
                "backend attempted to inspect an uncompiled table",
            ));
        }
    };
    let mut statement = connection.prepare(pragma).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok(ColumnSpec {
                name: row.get(1)?,
                declared_type: row.get(2)?,
                not_null: row.get(3)?,
                default_value: row.get(4)?,
                primary_key_position: row.get(5)?,
                hidden: row.get(6)?,
            })
        })
        .map_err(map_sqlite_error)?;
    let observed = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_sqlite_error)?;
    if observed != expected_columns {
        return Err(integrity_error(
            "community_profile_columns_mismatch",
            format!("Community Archive table {table} does not exactly match the compiled profile"),
        ));
    }
    validate_schema_object(connection, "table", table, expected_sql)
}

fn validate_schema_object(
    connection: &Connection,
    object_type: &'static str,
    name: &'static str,
    expected_sql: &'static str,
) -> Result<(), InformationError> {
    let observed = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = ?1 AND name = ?2",
            params![object_type, name],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_sqlite_error)?
        .flatten()
        .ok_or_else(|| {
            integrity_error(
                "community_profile_object_missing",
                format!("Community Archive is missing compiled {object_type} {name}"),
            )
        })?;
    if compact_sql(&observed) != compact_sql(expected_sql) {
        return Err(integrity_error(
            "community_profile_sql_mismatch",
            format!("Community Archive {object_type} {name} does not match the compiled profile"),
        ));
    }
    Ok(())
}

fn compact_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_ascii_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn validate_lookup_rows(connection: &Connection) -> Result<(), InformationError> {
    let kinds = query_i64_string_pairs(
        connection,
        "SELECT kind_id, name FROM message_kinds ORDER BY kind_id",
    )?;
    let expected_kinds = vec![
        (1, "tweet".to_string()),
        (2, "community_tweet".to_string()),
        (3, "note_tweet".to_string()),
        (4, "liked_tweet".to_string()),
    ];
    if kinds != expected_kinds {
        return Err(integrity_error(
            "community_profile_message_kinds_mismatch",
            "Community Archive message_kinds do not match the compiled vocabulary",
        ));
    }
    let collections = query_i64_string_pairs(
        connection,
        "SELECT source_collection_id, name FROM source_collections ORDER BY source_collection_id",
    )?;
    let expected_collections = vec![
        (1, "tweets".to_string()),
        (2, "community-tweet".to_string()),
        (3, "note-tweet".to_string()),
        (4, "like".to_string()),
        (5, "enriched-tweet".to_string()),
    ];
    if collections != expected_collections {
        return Err(integrity_error(
            "community_profile_source_collections_mismatch",
            "Community Archive source_collections do not match the compiled vocabulary",
        ));
    }
    Ok(())
}

fn query_i64_string_pairs(
    connection: &Connection,
    sql: &'static str,
) -> Result<Vec<(i64, String)>, InformationError> {
    let mut statement = connection.prepare(sql).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(map_sqlite_error)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_sqlite_error)
}

fn messages_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::expected("rowid", "INTEGER", 0, None, 1, 0),
        ColumnSpec::expected("tweet_id", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("note_id", "TEXT", 0, None, 0, 0),
        ColumnSpec::expected("kind_id", "INTEGER", 1, None, 0, 0),
        ColumnSpec::expected("source_collection_id", "INTEGER", 1, None, 0, 0),
        ColumnSpec::expected("archive_rowid", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("owner_account_rowid", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("created_at", "TEXT", 0, None, 0, 0),
        ColumnSpec::expected("text", "TEXT", 1, Some("''"), 0, 0),
        ColumnSpec::expected("lang_id", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("conversation_id", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("thread_id", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("depth", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("reply_to_tweet_id", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("reply_to_user_id", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("reply_to_username", "TEXT", 0, None, 0, 0),
        ColumnSpec::expected("quoted_tweet_id", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("retweeted_tweet_id", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("liked_by_account_rowid", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("community_id", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("favorite_count", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("retweet_count", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("flags", "INTEGER", 1, Some("0"), 0, 0),
        ColumnSpec::expected("source_record_hash", "TEXT", 0, None, 0, 0),
        ColumnSpec::expected("source_ordinal", "INTEGER", 0, None, 0, 0),
    ]
}

fn messages_fts_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::expected("text", "", 0, None, 0, 0),
        ColumnSpec::expected("username", "", 0, None, 0, 0),
        ColumnSpec::expected("account_display_name", "", 0, None, 0, 0),
        ColumnSpec::expected("messages_fts", "", 0, None, 0, 1),
        ColumnSpec::expected("rank", "", 0, None, 0, 1),
    ]
}

fn accounts_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::expected("account_rowid", "INTEGER", 0, None, 1, 0),
        ColumnSpec::expected("twitter_id", "INTEGER", 1, None, 0, 0),
        ColumnSpec::expected("username", "TEXT", 0, None, 0, 0),
        ColumnSpec::expected("display_name", "TEXT", 0, None, 0, 0),
        ColumnSpec::expected("created_at", "TEXT", 0, None, 0, 0),
        ColumnSpec::expected("created_via", "TEXT", 0, None, 0, 0),
        ColumnSpec::expected("num_tweets", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("num_following", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("num_followers", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("num_likes", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("updated_at", "TEXT", 0, None, 0, 0),
    ]
}

fn message_kinds_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::expected("kind_id", "INTEGER", 0, None, 1, 0),
        ColumnSpec::expected("name", "TEXT", 1, None, 0, 0),
    ]
}

fn source_collections_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::expected("source_collection_id", "INTEGER", 0, None, 1, 0),
        ColumnSpec::expected("name", "TEXT", 1, None, 0, 0),
    ]
}

fn archives_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::expected("archive_rowid", "INTEGER", 0, None, 1, 0),
        ColumnSpec::expected("path", "TEXT", 1, None, 0, 0),
        ColumnSpec::expected("owner_account_rowid", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("upload_start", "TEXT", 0, None, 0, 0),
        ColumnSpec::expected("upload_end", "TEXT", 0, None, 0, 0),
        ColumnSpec::expected("upload_likes", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("keep_private", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("imported_at", "TEXT", 1, Some("CURRENT_TIMESTAMP"), 0, 0),
        ColumnSpec::expected("message_count", "INTEGER", 1, Some("0"), 0, 0),
        ColumnSpec::expected("like_count", "INTEGER", 1, Some("0"), 0, 0),
    ]
}

fn message_sources_columns() -> Vec<ColumnSpec> {
    vec![
        ColumnSpec::expected("message_rowid", "INTEGER", 1, None, 1, 0),
        ColumnSpec::expected("archive_rowid", "INTEGER", 0, None, 2, 0),
        ColumnSpec::expected("source_collection_id", "INTEGER", 0, None, 0, 0),
        ColumnSpec::expected("source_record_hash", "TEXT", 1, None, 3, 0),
        ColumnSpec::expected("source_ordinal", "INTEGER", 0, None, 4, 0),
        ColumnSpec::expected("observed_at", "TEXT", 1, Some("CURRENT_TIMESTAMP"), 0, 0),
    ]
}

const EXPECTED_MESSAGES_SQL: &str = r#"
CREATE TABLE messages (
  rowid INTEGER PRIMARY KEY,
  tweet_id INTEGER,
  note_id TEXT,
  kind_id INTEGER NOT NULL,
  source_collection_id INTEGER NOT NULL,
  archive_rowid INTEGER,
  owner_account_rowid INTEGER,
  created_at TEXT,
  text TEXT NOT NULL DEFAULT '',
  lang_id INTEGER,
  conversation_id INTEGER,
  thread_id INTEGER,
  depth INTEGER,
  reply_to_tweet_id INTEGER,
  reply_to_user_id INTEGER,
  reply_to_username TEXT,
  quoted_tweet_id INTEGER,
  retweeted_tweet_id INTEGER,
  liked_by_account_rowid INTEGER,
  community_id INTEGER,
  favorite_count INTEGER,
  retweet_count INTEGER,
  flags INTEGER NOT NULL DEFAULT 0,
  source_record_hash TEXT,
  source_ordinal INTEGER
)
"#;

const EXPECTED_MESSAGES_FTS_SQL: &str = r#"
CREATE VIRTUAL TABLE messages_fts USING fts5(
  text,
  username,
  account_display_name,
  content='messages_fts_content',
  content_rowid='rowid',
  tokenize='porter unicode61'
)
"#;

const EXPECTED_ACCOUNTS_SQL: &str = r#"
CREATE TABLE accounts (
  account_rowid INTEGER PRIMARY KEY,
  twitter_id INTEGER NOT NULL UNIQUE,
  username TEXT,
  display_name TEXT,
  created_at TEXT,
  created_via TEXT,
  num_tweets INTEGER,
  num_following INTEGER,
  num_followers INTEGER,
  num_likes INTEGER,
  updated_at TEXT
)
"#;

const EXPECTED_MESSAGE_KINDS_SQL: &str = r#"
CREATE TABLE message_kinds (
  kind_id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE
)
"#;

const EXPECTED_SOURCE_COLLECTIONS_SQL: &str = r#"
CREATE TABLE source_collections (
  source_collection_id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE
)
"#;

const EXPECTED_ARCHIVES_SQL: &str = r#"
CREATE TABLE archives (
  archive_rowid INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  owner_account_rowid INTEGER,
  upload_start TEXT,
  upload_end TEXT,
  upload_likes INTEGER,
  keep_private INTEGER,
  imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  message_count INTEGER NOT NULL DEFAULT 0,
  like_count INTEGER NOT NULL DEFAULT 0
)
"#;

const EXPECTED_MESSAGE_SOURCES_SQL: &str = r#"
CREATE TABLE message_sources (
  message_rowid INTEGER NOT NULL,
  archive_rowid INTEGER,
  source_collection_id INTEGER,
  source_record_hash TEXT NOT NULL,
  source_ordinal INTEGER,
  observed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY(message_rowid, archive_rowid, source_record_hash, source_ordinal)
)
"#;

const EXPECTED_MESSAGES_FTS_CONTENT_SQL: &str = r#"
CREATE VIEW messages_fts_content AS
SELECT
  m.rowid AS rowid,
  m.text AS text,
  COALESCE(owner.username, '') AS username,
  COALESCE(owner.display_name, '') AS account_display_name
FROM messages m
LEFT JOIN accounts owner ON owner.account_rowid = m.owner_account_rowid
"#;

fn validate_query_for_backend(
    query: &InformationQuery,
    descriptor: &BackendDescriptor,
) -> Result<(), InformationError> {
    if !query.targets.is_empty()
        && !query.targets.iter().any(|target| {
            target.resource_id == descriptor.resource_id
                && target.release_id == descriptor.release_id
                && target.representation_id == descriptor.representation_id
        })
    {
        return Err(input_error(
            "query_target_mismatch",
            "query exact targets exclude this backend",
        ));
    }
    if !query.resources.is_empty()
        && !query
            .resources
            .iter()
            .any(|resource| resource == &descriptor.resource_id)
    {
        return Err(input_error(
            "query_resource_mismatch",
            "query resource filters exclude this backend",
        ));
    }
    if !query.representations.is_empty()
        && !query
            .representations
            .iter()
            .any(|representation| representation == &descriptor.representation_id)
    {
        return Err(input_error(
            "query_representation_mismatch",
            "query representation filters exclude this backend",
        ));
    }
    Ok(())
}

fn validate_query_filters(query: &InformationQuery) -> Result<(), InformationError> {
    if !query.filters.languages.is_empty()
        || !query.filters.subjects.is_empty()
        || query.filters.spatial.is_some()
        || query.filters.temporal_start.is_some()
        || query.filters.temporal_end.is_some()
    {
        return Err(InformationError::new(
            ErrorClass::Unsupported,
            "community_filter_unsupported",
            "Community Archive does not advertise language, subject, spatial, or temporal filters",
        ));
    }
    if query.filters.document_ids.len() > MAX_DOCUMENT_FILTERS
        || query.filters.fields.len() > MAX_FIELD_FILTERS
    {
        return Err(input_error(
            "community_filter_limit_exceeded",
            "Community Archive filter count exceeds the compiled profile cap",
        ));
    }
    for key in &query.filters.document_ids {
        let parsed = parse_record_key(key)?;
        if matches!(
            parsed,
            RecordKey::RowAndTweet { .. } | RecordKey::RowAndNote { .. }
        ) {
            return Err(input_error(
                "community_document_filter_key_unsupported",
                "document filters accept rowid, tweet_id, or note_id keys, not compound locators",
            ));
        }
    }
    for (field, value) in &query.filters.fields {
        match field.as_str() {
            "kind" | "source_collection" | "username" | "note_id" => {}
            "tweet_id" => {
                parse_positive_i64("tweet_id", value)?;
            }
            _ => {
                return Err(InformationError::new(
                    ErrorClass::Unsupported,
                    "community_field_filter_unsupported",
                    format!("Community Archive does not support field filter {field}"),
                ));
            }
        }
    }
    let bind_count = 3_usize
        .checked_add(query.filters.document_ids.len())
        .and_then(|count| count.checked_add(query.filters.fields.len()))
        .ok_or_else(|| {
            input_error(
                "community_bind_accounting_overflow",
                "Community Archive bind accounting overflowed",
            )
        })?;
    if bind_count > MAX_SQLITE_BINDS {
        return Err(input_error(
            "community_bind_limit_exceeded",
            "Community Archive query exceeds the compiled SQLite bind cap",
        ));
    }
    Ok(())
}

fn compile_fts_query(text: &str, syntax: QuerySyntax) -> Result<String, InformationError> {
    if text.contains('\0') {
        return Err(input_error(
            "community_query_contains_nul",
            "Community Archive query text contains a NUL character",
        ));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(input_error(
            "community_query_empty",
            "Community Archive query text is empty",
        ));
    }
    if syntax == QuerySyntax::BackendNative {
        return Ok(trimmed.to_string());
    }
    if syntax == QuerySyntax::ExactPhrase {
        return Ok(quote_fts_phrase(trimmed));
    }
    let terms = trimmed.split_whitespace().collect::<Vec<_>>();
    if terms.is_empty() || terms.len() > MAX_QUERY_TERMS {
        return Err(input_error(
            "community_query_term_limit",
            "Community Archive query term count is outside the compiled profile cap",
        ));
    }
    let quoted = terms.into_iter().map(quote_fts_phrase).collect::<Vec<_>>();
    let operator = if syntax == QuerySyntax::AnyTerms {
        " OR "
    } else {
        " AND "
    };
    Ok(quoted.join(operator))
}

fn quote_fts_phrase(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[derive(Debug)]
struct RawMessage {
    rowid: i64,
    tweet_id: Option<i64>,
    note_id: Option<String>,
    kind: String,
    source_collection: String,
    archive_rowid: Option<i64>,
    joined_archive_rowid: Option<i64>,
    keep_private: Option<i64>,
    username: Option<String>,
    display_name: Option<String>,
    created_at: Option<String>,
    conversation_id: Option<i64>,
    thread_id: Option<i64>,
    source_record_hash: Option<String>,
    source_ordinal: Option<i64>,
    score: f64,
    text: String,
}

fn run_fts_query(
    connection: &Connection,
    fts_query: &str,
    query: &InformationQuery,
    limit: i64,
    max_snippet_chars: u32,
) -> rusqlite::Result<Vec<RawMessage>> {
    let mut sql = String::from(
        r#"
        SELECT
            m.rowid,
            m.tweet_id,
            substr(m.note_id, 1, 512),
            substr(k.name, 1, 128),
            substr(sc.name, 1, 128),
            m.archive_rowid,
            primary_archive.archive_rowid,
            primary_archive.keep_private,
            substr(owner.username, 1, 512),
            substr(owner.display_name, 1, 512),
            substr(m.created_at, 1, 128),
            m.conversation_id,
            m.thread_id,
            substr(m.source_record_hash, 1, 129),
            m.source_ordinal,
            bm25(messages_fts) AS native_score,
            substr(snippet(messages_fts, 0, '[', ']', ' ... ', 32), 1, ?)
        FROM messages_fts
        JOIN messages m ON m.rowid = messages_fts.rowid
        JOIN message_kinds k ON k.kind_id = m.kind_id
        JOIN source_collections sc ON sc.source_collection_id = m.source_collection_id
        LEFT JOIN accounts owner ON owner.account_rowid = m.owner_account_rowid
        LEFT JOIN archives primary_archive ON primary_archive.archive_rowid = m.archive_rowid
        WHERE messages_fts MATCH ?
        "#,
    );
    let mut values = vec![
        SqlValue::Integer(i64::from(max_snippet_chars).saturating_add(1)),
        SqlValue::Text(fts_query.to_string()),
    ];
    add_document_filters(&mut sql, &mut values, &query.filters.document_ids)?;
    for (field, value) in &query.filters.fields {
        match field.as_str() {
            "kind" => sql.push_str("\nAND k.name = ?"),
            "source_collection" => sql.push_str("\nAND sc.name = ?"),
            "username" => sql.push_str("\nAND owner.username = ?"),
            "note_id" => sql.push_str("\nAND m.note_id = ?"),
            "tweet_id" => {
                sql.push_str("\nAND m.tweet_id = ?");
                values.push(SqlValue::Integer(value.parse::<i64>().map_err(|_| {
                    rusqlite::Error::InvalidParameterName(field.clone())
                })?));
                continue;
            }
            _ => return Err(rusqlite::Error::InvalidParameterName(field.clone())),
        }
        values.push(SqlValue::Text(value.clone()));
    }
    sql.push_str("\nORDER BY native_score ASC, m.rowid ASC LIMIT ?");
    values.push(SqlValue::Integer(limit));
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), raw_message_from_row)?;
    rows.collect()
}

fn add_document_filters(
    sql: &mut String,
    values: &mut Vec<SqlValue>,
    keys: &[String],
) -> rusqlite::Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut rowids = Vec::new();
    let mut tweet_ids = Vec::new();
    let mut note_ids = Vec::new();
    for key in keys {
        match parse_record_key(key)
            .map_err(|_| rusqlite::Error::InvalidParameterName(key.clone()))?
        {
            RecordKey::Rowid(rowid) => rowids.push(rowid),
            RecordKey::TweetId(tweet_id) => tweet_ids.push(tweet_id),
            RecordKey::NoteId(note_id) => note_ids.push(note_id),
            RecordKey::RowAndTweet { .. } | RecordKey::RowAndNote { .. } => {
                return Err(rusqlite::Error::InvalidParameterName(key.clone()));
            }
        }
    }
    let mut clauses = Vec::new();
    if !rowids.is_empty() {
        clauses.push(format!("m.rowid IN ({})", placeholders(rowids.len())));
        values.extend(rowids.into_iter().map(SqlValue::Integer));
    }
    if !tweet_ids.is_empty() {
        clauses.push(format!("m.tweet_id IN ({})", placeholders(tweet_ids.len())));
        values.extend(tweet_ids.into_iter().map(SqlValue::Integer));
    }
    if !note_ids.is_empty() {
        clauses.push(format!("m.note_id IN ({})", placeholders(note_ids.len())));
        values.extend(note_ids.into_iter().map(SqlValue::Text));
    }
    sql.push_str("\nAND (");
    sql.push_str(&clauses.join(" OR "));
    sql.push(')');
    Ok(())
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

fn raw_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawMessage> {
    Ok(RawMessage {
        rowid: row.get(0)?,
        tweet_id: row.get(1)?,
        note_id: row.get(2)?,
        kind: row.get(3)?,
        source_collection: row.get(4)?,
        archive_rowid: row.get(5)?,
        joined_archive_rowid: row.get(6)?,
        keep_private: row.get(7)?,
        username: row.get(8)?,
        display_name: row.get(9)?,
        created_at: row.get(10)?,
        conversation_id: row.get(11)?,
        thread_id: row.get(12)?,
        source_record_hash: row.get(13)?,
        source_ordinal: row.get(14)?,
        score: row.get(15)?,
        text: option_string_or_empty(row.get::<_, Option<String>>(16)?),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordKey {
    Rowid(i64),
    TweetId(i64),
    NoteId(String),
    RowAndTweet { rowid: i64, tweet_id: i64 },
    RowAndNote { rowid: i64, note_id: String },
}

impl RecordKey {
    fn verify(&self, raw: &RawMessage) -> Result<(), InformationError> {
        let matches = match self {
            Self::Rowid(rowid) => raw.rowid == *rowid,
            Self::TweetId(tweet_id) => raw.tweet_id == Some(*tweet_id),
            Self::NoteId(note_id) => raw.note_id.as_deref() == Some(note_id),
            Self::RowAndTweet { rowid, tweet_id } => {
                raw.rowid == *rowid && raw.tweet_id == Some(*tweet_id)
            }
            Self::RowAndNote { rowid, note_id } => {
                raw.rowid == *rowid && raw.note_id.as_deref() == Some(note_id)
            }
        };
        if matches {
            Ok(())
        } else {
            Err(integrity_error(
                "community_locator_identity_mismatch",
                "record locator identity fields do not identify the same message",
            ))
        }
    }
}

fn parse_record_key(key: &str) -> Result<RecordKey, InformationError> {
    let key = key.trim();
    let char_count = key.chars().count();
    if char_count == 0 || char_count > MAX_LOOKUP_KEY_CHARS {
        return Err(input_error(
            "invalid_community_record_key",
            "Community Archive record key is empty or exceeds 8192 characters",
        ));
    }
    if let Some((rowid, tweet_id)) = key
        .strip_prefix("rowid:")
        .and_then(|rest| rest.split_once(";tweet_id:"))
    {
        return Ok(RecordKey::RowAndTweet {
            rowid: parse_positive_i64("rowid", rowid)?,
            tweet_id: parse_positive_i64("tweet_id", tweet_id)?,
        });
    }
    if let Some((rowid, note_id)) = key
        .strip_prefix("rowid:")
        .and_then(|rest| rest.split_once(";note_id:"))
    {
        validate_note_id(note_id)?;
        return Ok(RecordKey::RowAndNote {
            rowid: parse_positive_i64("rowid", rowid)?,
            note_id: note_id.to_string(),
        });
    }
    if let Some(value) = key.strip_prefix("rowid:") {
        return Ok(RecordKey::Rowid(parse_positive_i64("rowid", value)?));
    }
    if let Some(value) = key.strip_prefix("tweet_id:") {
        return Ok(RecordKey::TweetId(parse_positive_i64("tweet_id", value)?));
    }
    if let Some(value) = key.strip_prefix("note_id:") {
        validate_note_id(value)?;
        return Ok(RecordKey::NoteId(value.to_string()));
    }
    Err(input_error(
        "invalid_community_record_key",
        "Community Archive record key must use rowid:, tweet_id:, or note_id:",
    ))
}

fn parse_positive_i64(label: &'static str, value: &str) -> Result<i64, InformationError> {
    let parsed = value.parse::<i64>().map_err(|_| {
        input_error(
            "invalid_community_numeric_key",
            format!("Community Archive {label} is not a positive 64-bit integer"),
        )
    })?;
    if parsed <= 0 {
        return Err(input_error(
            "invalid_community_numeric_key",
            format!("Community Archive {label} is not a positive 64-bit integer"),
        ));
    }
    Ok(parsed)
}

fn validate_note_id(value: &str) -> Result<(), InformationError> {
    if value.is_empty()
        || value.len() > 512
        || value.contains(';')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':'))
    {
        return Err(input_error(
            "invalid_community_note_id",
            "Community Archive note_id is malformed",
        ));
    }
    Ok(())
}

fn canonical_record_key(raw: &RawMessage) -> String {
    match (raw.tweet_id, raw.note_id.as_deref()) {
        (Some(tweet_id), _) => format!("rowid:{};tweet_id:{tweet_id}", raw.rowid),
        (None, Some(note_id)) => format!("rowid:{};note_id:{note_id}", raw.rowid),
        (None, None) => format!("rowid:{}", raw.rowid),
    }
}

fn upstream_record_id(raw: &RawMessage) -> String {
    match (raw.tweet_id, raw.note_id.as_deref()) {
        (Some(tweet_id), _) => format!("tweet:{tweet_id}"),
        (None, Some(note_id)) => format!("note:{note_id}"),
        (None, None) => format!("message-rowid:{}", raw.rowid),
    }
}

fn validate_collection(collection: Option<&str>) -> Result<(), InformationError> {
    if matches!(collection, None | Some("messages")) {
        Ok(())
    } else {
        Err(InformationError::new(
            ErrorClass::Unsupported,
            "unsupported_community_collection",
            "Community Archive record lookup supports only the messages collection",
        ))
    }
}

fn load_message(
    connection: &Connection,
    locator: &RecordKey,
    max_context_chars: u32,
) -> Result<RawMessage, InformationError> {
    let text_limit = i64::from(max_context_chars).saturating_add(1);
    match locator {
        RecordKey::Rowid(rowid)
        | RecordKey::RowAndTweet { rowid, .. }
        | RecordKey::RowAndNote { rowid, .. } => load_message_by_predicate(
            connection,
            "m.rowid = ?1",
            SqlValue::Integer(*rowid),
            text_limit,
            false,
        ),
        RecordKey::TweetId(tweet_id) => load_message_by_predicate(
            connection,
            "m.tweet_id = ?1",
            SqlValue::Integer(*tweet_id),
            text_limit,
            true,
        ),
        RecordKey::NoteId(note_id) => load_message_by_predicate(
            connection,
            "m.note_id = ?1",
            SqlValue::Text(note_id.clone()),
            text_limit,
            true,
        ),
    }
}

fn load_message_by_predicate(
    connection: &Connection,
    predicate: &'static str,
    key: SqlValue,
    text_limit: i64,
    detect_ambiguity: bool,
) -> Result<RawMessage, InformationError> {
    let sql = format!(
        r#"
        SELECT
            m.rowid,
            m.tweet_id,
            substr(m.note_id, 1, 512),
            substr(k.name, 1, 128),
            substr(sc.name, 1, 128),
            m.archive_rowid,
            primary_archive.archive_rowid,
            primary_archive.keep_private,
            substr(owner.username, 1, 512),
            substr(owner.display_name, 1, 512),
            substr(m.created_at, 1, 128),
            m.conversation_id,
            m.thread_id,
            substr(m.source_record_hash, 1, 129),
            m.source_ordinal,
            1.0,
            substr(m.text, 1, ?2)
        FROM messages m
        JOIN message_kinds k ON k.kind_id = m.kind_id
        JOIN source_collections sc ON sc.source_collection_id = m.source_collection_id
        LEFT JOIN accounts owner ON owner.account_rowid = m.owner_account_rowid
        LEFT JOIN archives primary_archive ON primary_archive.archive_rowid = m.archive_rowid
        WHERE {predicate}
        ORDER BY m.rowid
        LIMIT 2
        "#
    );
    let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(params![key, text_limit], raw_message_from_row)
        .map_err(map_sqlite_error)?;
    let mut messages = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_sqlite_error)?;
    if messages.is_empty() {
        return Err(InformationError::new(
            ErrorClass::NotFound,
            "community_message_not_found",
            "Community Archive message was not found",
        ));
    }
    if detect_ambiguity && messages.len() > 1 {
        return Err(InformationError::new(
            ErrorClass::InvalidInput,
            "community_message_key_ambiguous",
            "Community Archive key matches multiple messages; use a rowid locator",
        ));
    }
    Ok(messages.remove(0))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceObservation {
    source_collection: String,
    source_record_hash: String,
    source_ordinal: Option<i64>,
    private: bool,
}

#[derive(Debug)]
struct MessageProvenance {
    observations: Vec<SourceObservation>,
    hashes: Vec<String>,
    source_fingerprint: String,
    private: bool,
}

fn load_message_provenance(
    connection: &Connection,
    raw: &RawMessage,
) -> Result<MessageProvenance, InformationError> {
    if raw.archive_rowid.is_some() && raw.joined_archive_rowid != raw.archive_rowid {
        return Err(integrity_error(
            "community_message_archive_missing",
            "message references an archive row that is not present",
        ));
    }
    let direct_private = raw
        .archive_rowid
        .is_some_and(|_| raw.keep_private.unwrap_or(1) != 0);
    let mut observations = BTreeSet::new();
    if let Some(digest) = raw.source_record_hash.as_deref() {
        let digest = validate_source_record_hash(digest)?;
        if raw.source_ordinal.is_some_and(|ordinal| ordinal < 0) {
            return Err(integrity_error(
                "community_source_ordinal_invalid",
                "message source ordinal is negative",
            ));
        }
        observations.insert(SourceObservation {
            source_collection: raw.source_collection.clone(),
            source_record_hash: digest,
            source_ordinal: raw.source_ordinal,
            private: direct_private,
        });
    }

    let limit = i64::try_from(MAX_SOURCE_OBSERVATIONS.saturating_add(1)).map_err(|_| {
        integrity_error(
            "community_source_limit_overflow",
            "source observation limit exceeds SQLite bounds",
        )
    })?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT
                ms.archive_rowid,
                source_archive.archive_rowid,
                source_archive.keep_private,
                ms.source_collection_id,
                substr(source_collection.name, 1, 128),
                substr(ms.source_record_hash, 1, 129),
                ms.source_ordinal
            FROM message_sources ms
            LEFT JOIN archives source_archive
              ON source_archive.archive_rowid = ms.archive_rowid
            LEFT JOIN source_collections source_collection
              ON source_collection.source_collection_id = ms.source_collection_id
            WHERE ms.message_rowid = ?1
            ORDER BY ms.source_record_hash, ms.archive_rowid, ms.source_ordinal
            LIMIT ?2
            "#,
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(params![raw.rowid, limit], |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(map_sqlite_error)?;
    let source_rows = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_sqlite_error)?;
    if source_rows.len() > MAX_SOURCE_OBSERVATIONS {
        return Err(integrity_error(
            "community_source_observation_limit",
            "message has more source observations than the compiled provenance cap",
        ));
    }
    for (
        archive_rowid,
        joined_archive_rowid,
        keep_private,
        source_collection_id,
        source_collection,
        digest,
        source_ordinal,
    ) in source_rows
    {
        if archive_rowid.is_some() && joined_archive_rowid != archive_rowid {
            return Err(integrity_error(
                "community_source_archive_missing",
                "message source references an archive row that is not present",
            ));
        }
        if source_collection_id.is_some() && source_collection.is_none() {
            return Err(integrity_error(
                "community_source_collection_missing",
                "message source references a source collection that is not present",
            ));
        }
        if source_ordinal.is_some_and(|ordinal| ordinal < 0) {
            return Err(integrity_error(
                "community_source_ordinal_invalid",
                "message source ordinal is negative",
            ));
        }
        observations.insert(SourceObservation {
            source_collection: source_collection.unwrap_or_else(|| "unspecified".to_string()),
            source_record_hash: validate_source_record_hash(&digest)?,
            source_ordinal,
            private: archive_rowid.is_some_and(|_| keep_private.unwrap_or(1) != 0),
        });
    }
    if observations.is_empty() {
        return Err(integrity_error(
            "community_source_provenance_missing",
            "message has no source record hash provenance",
        ));
    }
    let observations = observations.into_iter().collect::<Vec<_>>();
    let hashes = observations
        .iter()
        .map(|observation| observation.source_record_hash.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let private = direct_private || observations.iter().any(|observation| observation.private);
    let source_fingerprint = aggregate_source_hashes(&hashes);
    Ok(MessageProvenance {
        observations,
        hashes,
        source_fingerprint,
        private,
    })
}

fn validate_source_record_hash(value: &str) -> Result<String, InformationError> {
    let normalized = value
        .strip_prefix("sha256:")
        .unwrap_or(value)
        .to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(integrity_error(
            "community_source_record_hash_invalid",
            "message source record hash is not a SHA-256 digest",
        ));
    }
    Ok(normalized)
}

fn aggregate_source_hashes(hashes: &[String]) -> String {
    if let [only] = hashes {
        return format!("sha256:{only}");
    }
    let mut hasher = Sha256::new();
    hasher.update(b"information-native.community-source-record-set.v1\0");
    for digest in hashes {
        hasher.update(digest.len().to_le_bytes());
        hasher.update(digest.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn validate_raw_message(raw: &RawMessage) -> Result<(), InformationError> {
    if raw.rowid <= 0
        || raw.tweet_id.is_some_and(|value| value <= 0)
        || raw.note_id.as_ref().is_some_and(|value| value.is_empty())
        || raw.kind.trim().is_empty()
        || raw.source_collection.trim().is_empty()
        || raw.source_ordinal.is_some_and(|value| value < 0)
        || !raw.score.is_finite()
    {
        return Err(integrity_error(
            "community_message_row_invalid",
            "Community Archive message contains invalid identity, vocabulary, or score fields",
        ));
    }
    Ok(())
}

fn validate_direct_read_budget(
    max_context_chars: u32,
    timeout_ms: u64,
) -> Result<(), InformationError> {
    if max_context_chars == 0 || max_context_chars > 2_000_000 || timeout_ms == 0 {
        return Err(input_error(
            "invalid_community_read_budget",
            "Community Archive read budget is outside the supported bounds",
        ));
    }
    Ok(())
}

fn validate_read_identity(
    descriptor: &BackendDescriptor,
    resource_id: &ResourceId,
    release_id: &ReleaseId,
    representation_id: &RepresentationId,
) -> Result<(), InformationError> {
    if &descriptor.resource_id != resource_id
        || &descriptor.release_id != release_id
        || &descriptor.representation_id != representation_id
    {
        return Err(input_error(
            "community_read_identity_mismatch",
            "read identity does not address this Community Archive backend",
        ));
    }
    Ok(())
}

fn insert_optional_string(
    metadata: &mut BTreeMap<String, JsonValue>,
    key: &'static str,
    value: Option<String>,
) {
    if let Some(value) = value {
        metadata.insert(key.to_string(), json!(value));
    }
}

fn truncate_chars(input: &str, limit: usize) -> (String, bool) {
    let mut chars = input.chars();
    let output = chars.by_ref().take(limit).collect::<String>();
    let truncated = chars.next().is_some();
    (output, truncated)
}

fn option_string_or_empty(value: Option<String>) -> String {
    value.unwrap_or_default()
}

fn access_mode_name(mode: ExternalAccessMode) -> &'static str {
    match mode {
        ExternalAccessMode::LiveReadOnly => "live_read_only",
        ExternalAccessMode::ImmutableReadOnly => "immutable_read_only",
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn map_search_sqlite_error(error: rusqlite::Error) -> InformationError {
    match &error {
        rusqlite::Error::SqliteFailure(code, _) if code.code == ErrorCode::OperationInterrupted => {
            map_sqlite_error(error)
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) =>
        {
            map_sqlite_error(error)
        }
        _ => input_error(
            "community_fts_query_rejected",
            "Community Archive FTS5 rejected the compiled query",
        ),
    }
}

fn map_sqlite_error(error: rusqlite::Error) -> InformationError {
    match &error {
        rusqlite::Error::SqliteFailure(code, _) if code.code == ErrorCode::OperationInterrupted => {
            InformationError::new(
                ErrorClass::ResourceBusy,
                "community_query_timeout",
                "Community Archive query deadline elapsed",
            )
            .retryable(true)
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) =>
        {
            InformationError::new(
                ErrorClass::ResourceBusy,
                "community_resource_busy",
                "Community Archive is busy",
            )
            .retryable(true)
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(code.code, ErrorCode::PermissionDenied | ErrorCode::ReadOnly) =>
        {
            InformationError::new(
                ErrorClass::Permission,
                "community_permission_denied",
                "SQLite denied the read-only Community Archive operation",
            )
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            ) =>
        {
            integrity_error(
                "community_source_invalid",
                "Community Archive source is corrupt or not a SQLite database",
            )
        }
        _ => InformationError::new(
            ErrorClass::Backend,
            "community_backend_error",
            "Community Archive SQLite operation failed",
        ),
    }
}

fn input_error(code: &'static str, message: impl Into<String>) -> InformationError {
    InformationError::new(ErrorClass::InvalidInput, code, message)
}

fn integrity_error(code: &'static str, message: impl Into<String>) -> InformationError {
    InformationError::new(ErrorClass::Integrity, code, message)
}

fn io_error(code: &'static str, message: impl Into<String>) -> InformationError {
    InformationError::new(ErrorClass::Io, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use information_native_types::{
        QUERY_SCHEMA, QueryBudget, QueryFilters, QueryId, RetrievalTarget,
    };
    use std::error::Error;
    use tempfile::TempDir;

    const PRIVATE_ARCHIVE_PATH: &str = "/private/source/secret-twitter-archive.zip";

    fn fixture() -> Result<(TempDir, PathBuf), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("community.sqlite");
        let connection = Connection::open(&path)?;
        let schema = format!(
            r#"
            PRAGMA user_version = {PROFILE_USER_VERSION};
            {EXPECTED_ACCOUNTS_SQL};
            {EXPECTED_ARCHIVES_SQL};
            {EXPECTED_MESSAGE_KINDS_SQL};
            {EXPECTED_SOURCE_COLLECTIONS_SQL};
            {EXPECTED_MESSAGES_SQL};
            {EXPECTED_MESSAGE_SOURCES_SQL};
            {EXPECTED_MESSAGES_FTS_CONTENT_SQL};
            {EXPECTED_MESSAGES_FTS_SQL};
            CREATE INDEX idx_messages_tweet_id ON messages(tweet_id);
            CREATE INDEX idx_message_sources_message ON message_sources(message_rowid);
            "#
        );
        connection.execute_batch(&schema)?;
        connection.execute_batch(
            r#"
            INSERT INTO message_kinds(kind_id, name) VALUES
              (1, 'tweet'),
              (2, 'community_tweet'),
              (3, 'note_tweet'),
              (4, 'liked_tweet');
            INSERT INTO source_collections(source_collection_id, name) VALUES
              (1, 'tweets'),
              (2, 'community-tweet'),
              (3, 'note-tweet'),
              (4, 'like'),
              (5, 'enriched-tweet');
            INSERT INTO accounts(
              account_rowid, twitter_id, username, display_name
            ) VALUES (1, 9001, 'example', 'Example Person');
            "#,
        )?;
        connection.execute(
            "INSERT INTO archives(archive_rowid, path, keep_private) VALUES (1, ?1, 1)",
            [PRIVATE_ARCHIVE_PATH],
        )?;
        connection.execute(
            "INSERT INTO archives(archive_rowid, path, keep_private) VALUES (2, ?1, 0)",
            ["/public/source/archive.zip"],
        )?;
        let private_hash = "a".repeat(64);
        let public_hash = "b".repeat(64);
        connection.execute(
            r#"
            INSERT INTO messages(
              rowid, tweet_id, kind_id, source_collection_id, archive_rowid,
              owner_account_rowid, created_at, text, conversation_id, thread_id,
              source_record_hash, source_ordinal
            ) VALUES (
              1, 101, 1, 1, 1, 1, '2025-01-01T00:00:00.000Z',
              'quiet contemplation and practical attention', 101, 101, ?1, 0
            )
            "#,
            [&private_hash],
        )?;
        connection.execute(
            r#"
            INSERT INTO messages(
              rowid, tweet_id, kind_id, source_collection_id, archive_rowid,
              owner_account_rowid, created_at, text, source_record_hash, source_ordinal
            ) VALUES (
              2, 202, 1, 1, 2, 1, '2025-01-02T00:00:00.000Z',
              'public contemplation record', ?1, 1
            )
            "#,
            [&public_hash],
        )?;
        connection.execute(
            r#"
            INSERT INTO message_sources(
              message_rowid, archive_rowid, source_collection_id,
              source_record_hash, source_ordinal
            ) VALUES (1, 1, 1, ?1, 0)
            "#,
            [&private_hash],
        )?;
        connection.execute(
            r#"
            INSERT INTO message_sources(
              message_rowid, archive_rowid, source_collection_id,
              source_record_hash, source_ordinal
            ) VALUES (2, 2, 1, ?1, 1)
            "#,
            [&public_hash],
        )?;
        connection.execute_batch(
            r#"
            INSERT INTO messages_fts(rowid, text, username, account_display_name)
            SELECT m.rowid, m.text, COALESCE(a.username, ''), COALESCE(a.display_name, '')
            FROM messages m
            LEFT JOIN accounts a ON a.account_rowid = m.owner_account_rowid;
            "#,
        )?;
        drop(connection);
        Ok((temporary, path))
    }

    fn config(path: impl Into<PathBuf>) -> Result<CommunityArchiveBackendConfig, Box<dyn Error>> {
        Ok(CommunityArchiveBackendConfig::new(
            "community-fixture",
            "Community fixture",
            ResourceId::parse("community.fixture")?,
            ReleaseId::parse("community.fixture.v28")?,
            RepresentationId::parse("community.fixture.sqlite")?,
            path,
            ExternalAccessMode::ImmutableReadOnly,
            "Fixture archive owner",
        ))
    }

    fn target(backend: &CommunityArchiveBackend) -> RetrievalTarget {
        RetrievalTarget {
            resource_id: backend.descriptor.resource_id.clone(),
            release_id: backend.descriptor.release_id.clone(),
            representation_id: backend.descriptor.representation_id.clone(),
        }
    }

    fn query(
        backend: &CommunityArchiveBackend,
        text: &str,
        purpose: RetrievalPurpose,
    ) -> InformationQuery {
        InformationQuery {
            schema: QUERY_SCHEMA.to_string(),
            query_id: QueryId::new(),
            text: text.to_string(),
            syntax: QuerySyntax::NaturalTerms,
            purpose,
            targets: vec![target(backend)],
            resources: Vec::new(),
            representations: Vec::new(),
            filters: QueryFilters::default(),
            budget: QueryBudget {
                max_hits: 10,
                max_hits_per_backend: 10,
                max_backends: 1,
                max_context_chars: 4_096,
                timeout_ms: 5_000,
            },
        }
    }

    fn read_request(
        backend: &CommunityArchiveBackend,
        purpose: RetrievalPurpose,
        locator: EvidenceLocator,
    ) -> ReadRequest {
        ReadRequest {
            resource_id: backend.descriptor.resource_id.clone(),
            release_id: backend.descriptor.release_id.clone(),
            representation_id: backend.descriptor.representation_id.clone(),
            purpose,
            locator,
            max_context_chars: 4_096,
            timeout_ms: 5_000,
        }
    }

    fn lookup_request(
        backend: &CommunityArchiveBackend,
        purpose: RetrievalPurpose,
        key: &str,
    ) -> LookupRequest {
        LookupRequest {
            resource_id: backend.descriptor.resource_id.clone(),
            release_id: backend.descriptor.release_id.clone(),
            representation_id: backend.descriptor.representation_id.clone(),
            purpose,
            collection: Some("messages".to_string()),
            key: key.to_string(),
            max_context_chars: 4_096,
            timeout_ms: 5_000,
        }
    }

    #[test]
    fn search_read_and_lookup_preserve_stable_identity_and_provenance() -> Result<(), Box<dyn Error>>
    {
        let (_temporary, path) = fixture()?;
        let backend = CommunityArchiveBackend::open(config(&path)?)?;
        let result = backend.search(&query(
            &backend,
            "quiet contemplation",
            RetrievalPurpose::LocalUi,
        ))?;
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        assert_eq!(
            hit.locator,
            EvidenceLocator::Record {
                collection: Some("messages".to_string()),
                key: "rowid:1;tweet_id:101".to_string(),
            }
        );
        assert_eq!(
            hit.source_fingerprint,
            Some(format!("sha256:{}", "a".repeat(64)))
        );
        assert_eq!(
            hit.excerpt_sha256,
            evidence_text_sha256(&hit.snippet, &hit.context)
        );
        let serialized = serde_json::to_string(hit)?;
        assert!(!serialized.contains(PRIVATE_ARCHIVE_PATH));
        assert!(!serialized.contains("/private/source"));

        let read = backend.read(&read_request(
            &backend,
            RetrievalPurpose::LocalUi,
            hit.locator.clone(),
        ))?;
        assert_eq!(
            read.hit.snippet,
            "quiet contemplation and practical attention"
        );
        assert_eq!(read.hit.locator, hit.locator);

        let lookup = backend.lookup(&lookup_request(
            &backend,
            RetrievalPurpose::LocalUi,
            "tweet_id:101",
        ))?;
        assert_eq!(lookup.hit.locator, hit.locator);
        assert_eq!(
            lookup.hit.provenance.source_inputs,
            vec![format!("sha256:{}", "a".repeat(64))]
        );
        Ok(())
    }

    #[test]
    fn private_records_require_dedicated_model_opt_in_and_never_export()
    -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let mut denied_config = config(&path)?;
        denied_config.use_policy.model_context = UsePermission::Allowed;
        let denied_backend = CommunityArchiveBackend::open(denied_config)?;

        let local = denied_backend.lookup(&lookup_request(
            &denied_backend,
            RetrievalPurpose::LocalUi,
            "rowid:1",
        ))?;
        let denied = denied_backend
            .read(&read_request(
                &denied_backend,
                RetrievalPurpose::ModelContext,
                local.hit.locator.clone(),
            ))
            .err()
            .ok_or("private model read unexpectedly succeeded")?;
        assert_eq!(denied.class, ErrorClass::Permission);
        let model_search = denied_backend.search(&query(
            &denied_backend,
            "quiet",
            RetrievalPurpose::ModelContext,
        ))?;
        assert!(model_search.hits.is_empty());
        assert!(!model_search.complete);

        let mut allowed_config = config(&path)?;
        allowed_config.use_policy.model_context = UsePermission::Allowed;
        allowed_config.allow_private_model_context = true;
        let allowed_backend = CommunityArchiveBackend::open(allowed_config)?;
        let allowed = allowed_backend.lookup(&lookup_request(
            &allowed_backend,
            RetrievalPurpose::ModelContext,
            "rowid:1",
        ))?;
        assert_eq!(allowed.hit.use_policy.model_context, UsePermission::Allowed);
        assert_eq!(
            allowed.hit.use_policy.excerpt_export,
            UsePermission::Forbidden
        );
        assert_eq!(
            allowed.hit.use_policy.redistribution,
            UsePermission::Forbidden
        );
        assert_eq!(
            allowed.hit.rights[0].redistribution,
            RedistributionPolicy::PrivateUseOnly
        );
        let serialized = serde_json::to_string(&allowed.hit)?;
        assert!(!serialized.contains(PRIVATE_ARCHIVE_PATH));

        let export = allowed_backend
            .lookup(&lookup_request(
                &allowed_backend,
                RetrievalPurpose::ExcerptExport,
                "rowid:1",
            ))
            .err()
            .ok_or("private export unexpectedly succeeded")?;
        assert_eq!(export.class, ErrorClass::Permission);
        Ok(())
    }

    #[test]
    fn any_private_source_observation_makes_the_record_private() -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let connection = Connection::open(&path)?;
        let additional_hash = "c".repeat(64);
        connection.execute(
            r#"
            INSERT INTO message_sources(
              message_rowid, archive_rowid, source_collection_id,
              source_record_hash, source_ordinal
            ) VALUES (2, 1, 5, ?1, 2)
            "#,
            [&additional_hash],
        )?;
        drop(connection);

        let mut fixture_config = config(&path)?;
        fixture_config.use_policy.model_context = UsePermission::Allowed;
        let backend = CommunityArchiveBackend::open(fixture_config)?;
        let model = backend.search(&query(
            &backend,
            "public contemplation",
            RetrievalPurpose::ModelContext,
        ))?;
        assert!(model.hits.is_empty());
        let local = backend.lookup(&lookup_request(
            &backend,
            RetrievalPurpose::LocalUi,
            "rowid:2",
        ))?;
        assert_eq!(
            local.hit.metadata.get("private_archive"),
            Some(&json!(true))
        );
        assert_eq!(local.hit.provenance.source_inputs.len(), 2);
        assert!(!serde_json::to_string(&local.hit)?.contains(PRIVATE_ARCHIVE_PATH));
        Ok(())
    }

    #[test]
    fn caller_policy_can_only_remove_profile_authority() -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let mut fixture_config = config(&path)?;
        fixture_config.use_policy = UsePolicy {
            local_search: UsePermission::Forbidden,
            model_context: UsePermission::Allowed,
            excerpt_export: UsePermission::Allowed,
            redistribution: UsePermission::Allowed,
            attribution_required: false,
        };
        fixture_config.allow_private_model_context = true;
        let backend = CommunityArchiveBackend::open(fixture_config)?;
        assert_eq!(
            backend.descriptor.use_policy.local_search,
            UsePermission::Forbidden
        );
        assert_eq!(
            backend.descriptor.use_policy.excerpt_export,
            UsePermission::Forbidden
        );
        assert_eq!(
            backend.descriptor.use_policy.redistribution,
            UsePermission::Forbidden
        );
        assert!(backend.descriptor.use_policy.attribution_required);
        let denied = backend
            .lookup(&lookup_request(
                &backend,
                RetrievalPurpose::LocalUi,
                "rowid:1",
            ))
            .err()
            .ok_or("forbidden local lookup unexpectedly succeeded")?;
        assert_eq!(denied.class, ErrorClass::Permission);
        Ok(())
    }

    #[test]
    fn immutable_registration_identity_and_digest_are_independently_verified()
    -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let observed = FileIdentity::observe(&path)?;
        let digest = hash_immutable_source(&path, &observed)?;
        let mut registered_identity = observed.source_identity();
        registered_identity.sha256 = Some(format!("sha256:{digest}"));

        let mut verified_config = config(&path)?;
        verified_config.verified_source_sha256 = Some(digest.clone());
        verified_config.verified_source_identity = Some(registered_identity.clone());
        let backend = CommunityArchiveBackend::open(verified_config)?;
        assert_eq!(backend.source_identity(), registered_identity);
        assert_eq!(backend.source_fingerprint(), format!("sha256:{digest}"));

        let mut wrong_digest_config = config(&path)?;
        wrong_digest_config.verified_source_sha256 = Some("0".repeat(64));
        let wrong_digest = CommunityArchiveBackend::open(wrong_digest_config)
            .err()
            .ok_or("incorrect registration digest unexpectedly mounted")?;
        assert_eq!(wrong_digest.code, "immutable_community_digest_mismatch");

        let mut stale_identity = registered_identity;
        stale_identity.bytes = stale_identity.bytes.saturating_add(1);
        let mut stale_config = config(&path)?;
        stale_config.verified_source_identity = Some(stale_identity);
        let stale = CommunityArchiveBackend::open(stale_config)
            .err()
            .ok_or("stale registration identity unexpectedly mounted")?;
        assert_eq!(stale.code, "verified_community_identity_mismatch");
        Ok(())
    }

    #[test]
    fn live_mode_rejects_static_registration_claims() -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let observed = FileIdentity::observe(&path)?;
        let digest = hash_immutable_source(&path, &observed)?;

        let mut digest_config = config(&path)?;
        digest_config.access_mode = ExternalAccessMode::LiveReadOnly;
        digest_config.verified_source_sha256 = Some(digest.clone());
        let digest_error = CommunityArchiveBackend::open(digest_config)
            .err()
            .ok_or("live static digest unexpectedly accepted")?;
        assert_eq!(digest_error.code, "live_community_static_digest_forbidden");

        let mut identity = observed.source_identity();
        identity.sha256 = Some(format!("sha256:{digest}"));
        let mut identity_config = config(&path)?;
        identity_config.access_mode = ExternalAccessMode::LiveReadOnly;
        identity_config.verified_source_identity = Some(identity);
        let identity_error = CommunityArchiveBackend::open(identity_config)
            .err()
            .ok_or("live static identity unexpectedly accepted")?;
        assert_eq!(
            identity_error.code,
            "live_community_static_identity_forbidden"
        );
        Ok(())
    }

    #[test]
    fn source_wide_rights_are_retained_and_only_tighten_policy() -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let source_rights = RightsStatement {
            scope: "community-archive-source".to_string(),
            expression: "operator forbids redistribution".to_string(),
            license_url: None,
            license_text_sha256: None,
            attribution: Some("Fixture archive owner".to_string()),
            obligations: Vec::new(),
            redistribution: RedistributionPolicy::Forbidden,
        };
        let mut fixture_config = config(&path)?;
        fixture_config.rights = vec![source_rights.clone()];
        fixture_config.use_policy.excerpt_export = UsePermission::Allowed;
        fixture_config.use_policy.redistribution = UsePermission::Allowed;
        let backend = CommunityArchiveBackend::open(fixture_config)?;
        assert_eq!(
            backend.descriptor.use_policy.excerpt_export,
            UsePermission::Forbidden
        );
        assert_eq!(
            backend.descriptor.use_policy.redistribution,
            UsePermission::Forbidden
        );
        let hit = backend.lookup(&lookup_request(
            &backend,
            RetrievalPurpose::LocalUi,
            "rowid:1",
        ))?;
        assert_eq!(hit.hit.rights.first(), Some(&source_rights));
        assert_eq!(hit.hit.rights.len(), 2);
        assert_eq!(hit.hit.use_policy.excerpt_export, UsePermission::Forbidden);
        assert_eq!(hit.hit.use_policy.redistribution, UsePermission::Forbidden);
        Ok(())
    }

    #[test]
    fn exact_schema_profile_rejects_added_columns() -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let connection = Connection::open(&path)?;
        connection.execute_batch("ALTER TABLE messages ADD COLUMN unexpected TEXT;")?;
        drop(connection);
        let error = CommunityArchiveBackend::open(config(&path)?)
            .err()
            .ok_or("schema drift unexpectedly mounted")?;
        assert_eq!(error.code, "community_profile_columns_mismatch");
        Ok(())
    }

    #[test]
    fn immutable_mode_rejects_nonempty_wal_and_later_identity_change() -> Result<(), Box<dyn Error>>
    {
        let (_temporary, path) = fixture()?;
        let wal = sibling_sidecar_path(&path, "-wal")?;
        fs::write(&wal, b"not a valid empty WAL")?;
        let wal_error = CommunityArchiveBackend::open(config(&path)?)
            .err()
            .ok_or("non-empty WAL unexpectedly mounted immutable")?;
        assert_eq!(wal_error.code, "community_nonempty_wal");
        let mut live_config = config(&path)?;
        live_config.access_mode = ExternalAccessMode::LiveReadOnly;
        let live_wal_error = CommunityArchiveBackend::open(live_config)
            .err()
            .ok_or("non-empty WAL unexpectedly mounted live")?;
        assert_eq!(live_wal_error.code, "community_nonempty_wal");
        fs::remove_file(&wal)?;

        let backend = CommunityArchiveBackend::open(config(&path)?)?;
        let connection = Connection::open(&path)?;
        connection
            .execute_batch("CREATE INDEX fixture_identity_change ON messages(created_at);")?;
        drop(connection);
        let health = backend.health();
        assert_eq!(health.status, BackendHealthStatus::Unavailable);
        assert!(
            health
                .message
                .contains("immutable_community_identity_changed")
        );
        Ok(())
    }

    #[test]
    fn live_mode_discards_results_when_file_identity_changes() -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let mut fixture_config = config(&path)?;
        fixture_config.access_mode = ExternalAccessMode::LiveReadOnly;
        let backend = CommunityArchiveBackend::open(fixture_config)?;
        let changed_size = fs::metadata(&path)?.len().saturating_add(1);
        let error = backend
            .with_connection(5_000, |_connection, _snapshot| {
                let file = fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .map_err(|error| {
                        io_error(
                            "fixture_raw_open_failed",
                            format!("cannot open fixture for identity test: {error}"),
                        )
                    })?;
                file.set_len(changed_size).map_err(|error| {
                    io_error(
                        "fixture_raw_resize_failed",
                        format!("cannot resize fixture for identity test: {error}"),
                    )
                })?;
                Ok(())
            })
            .err()
            .ok_or("live identity change unexpectedly accepted")?;
        assert_eq!(error.code, "community_identity_changed_during_read");
        Ok(())
    }

    #[test]
    fn live_reads_leave_main_file_and_source_sidecars_untouched() -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let before = FileIdentity::observe(&path)?;
        let before_sidecars = SidecarSet::observe(&path)?;
        let mut fixture_config = config(&path)?;
        fixture_config.access_mode = ExternalAccessMode::LiveReadOnly;
        let backend = CommunityArchiveBackend::open(fixture_config)?;
        let result = backend.search(&query(&backend, "quiet", RetrievalPurpose::LocalUi))?;
        assert_eq!(result.hits.len(), 1);
        assert_eq!(FileIdentity::observe(&path)?, before);
        assert_eq!(SidecarSet::observe(&path)?, before_sidecars);
        assert_eq!(before_sidecars.wal, SidecarIdentity::Absent);
        assert_eq!(before_sidecars.shm, SidecarIdentity::Absent);
        assert_eq!(before_sidecars.journal, SidecarIdentity::Absent);
        Ok(())
    }

    #[test]
    fn connection_enforces_pragmas_bind_cap_and_progress_deadline() -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let connection =
            open_read_only_connection(&path, ExternalAccessMode::ImmutableReadOnly, 1)?;
        assert_eq!(pragma_i64(&connection, "query_only")?, 1);
        assert_eq!(pragma_i64(&connection, "trusted_schema")?, 0);
        assert_eq!(
            connection.limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER),
            i32::try_from(MAX_SQLITE_BINDS)?
        );
        let interrupted = connection
            .query_row(
                r#"
                WITH RECURSIVE count(value) AS (
                  VALUES(0)
                  UNION ALL
                  SELECT value + 1 FROM count WHERE value < 100000000
                )
                SELECT sum(value) FROM count
                "#,
                [],
                |row| row.get::<_, i64>(0),
            )
            .err()
            .ok_or("recursive query unexpectedly outran the one millisecond deadline")?;
        let mapped = map_sqlite_error(interrupted);
        assert_eq!(mapped.code, "community_query_timeout");
        Ok(())
    }

    #[test]
    fn tweet_lookup_fails_closed_when_the_key_is_ambiguous() -> Result<(), Box<dyn Error>> {
        let (_temporary, path) = fixture()?;
        let connection = Connection::open(&path)?;
        let duplicate_hash = "c".repeat(64);
        connection.execute(
            r#"
            INSERT INTO messages(
              rowid, tweet_id, kind_id, source_collection_id, archive_rowid,
              owner_account_rowid, text, source_record_hash, source_ordinal
            ) VALUES (3, 101, 4, 4, 1, 1, 'duplicate liked record', ?1, 2)
            "#,
            [&duplicate_hash],
        )?;
        connection.execute(
            r#"
            INSERT INTO message_sources(
              message_rowid, archive_rowid, source_collection_id,
              source_record_hash, source_ordinal
            ) VALUES (3, 1, 4, ?1, 2)
            "#,
            [&duplicate_hash],
        )?;
        connection.execute(
            "INSERT INTO messages_fts(rowid, text, username, account_display_name) VALUES (3, 'duplicate liked record', 'example', 'Example Person')",
            [],
        )?;
        drop(connection);
        let backend = CommunityArchiveBackend::open(config(&path)?)?;
        let error = backend
            .lookup(&lookup_request(
                &backend,
                RetrievalPurpose::LocalUi,
                "tweet_id:101",
            ))
            .err()
            .ok_or("ambiguous tweet id unexpectedly resolved")?;
        assert_eq!(error.code, "community_message_key_ambiguous");
        let exact = backend.lookup(&lookup_request(
            &backend,
            RetrievalPurpose::LocalUi,
            "rowid:1;tweet_id:101",
        ))?;
        assert_eq!(exact.hit.passage_id.as_deref(), Some("message-rowid:1"));
        Ok(())
    }

    #[test]
    #[ignore = "set COMMUNITY_ARCHIVE_SQLITE to run against a compatible v28 archive"]
    fn real_community_archive_smoke() -> Result<(), Box<dyn Error>> {
        let Some(path) = std::env::var_os("COMMUNITY_ARCHIVE_SQLITE").map(PathBuf::from) else {
            return Ok(());
        };
        let mut smoke_config = CommunityArchiveBackendConfig::new(
            "community-real-smoke",
            "Canonical Community Archive",
            ResourceId::parse("community.archive")?,
            ReleaseId::parse("community.archive.v28")?,
            RepresentationId::parse("community.archive.sqlite")?,
            &path,
            ExternalAccessMode::LiveReadOnly,
            "Community Archive contributors",
        );
        smoke_config.max_snippet_chars = 512;
        let backend = CommunityArchiveBackend::open(smoke_config)?;
        let mut smoke_query = query(&backend, "contemplation", RetrievalPurpose::LocalUi);
        smoke_query.budget.max_hits = 1;
        smoke_query.budget.max_hits_per_backend = 1;
        smoke_query.budget.max_context_chars = 1_024;
        smoke_query.budget.timeout_ms = 30_000;
        let result = backend.search(&smoke_query)?;
        assert!(!result.hits.is_empty());
        let serialized = serde_json::to_string(&result)?;
        assert!(!serialized.contains(path.to_string_lossy().as_ref()));
        for hit in result.hits {
            hit.validate()?;
        }
        Ok(())
    }
}
