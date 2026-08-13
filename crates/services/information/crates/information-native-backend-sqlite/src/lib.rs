#![forbid(unsafe_code)]

//! Strict read-only retrieval for Alexandria-compatible SQLite/FTS5 corpora.
//!
//! The profile is compiled here: catalogue metadata cannot supply table or
//! column identifiers. Every operation opens the canonical file read-only,
//! starts one read transaction, applies a progress deadline, and compares file
//! identity before and after the transaction. Immutable mode additionally uses
//! SQLite's `immutable=1` URI and rejects a non-empty sibling WAL.

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

const MAX_CONTEXT_RADIUS: u8 = 8;
const MAX_SNIPPET_CHARS: u32 = 4_096;
const MAX_ID_CHARS: usize = 8_192;
const MAX_THEME_VALUES: usize = 64;
const MAX_QUERY_TERMS: usize = 64;
const MAX_FILTER_VALUES_PER_LIST: usize = 128;
const MAX_FIELD_FILTERS: usize = 16;
const MAX_SQLITE_BINDS: usize = 512;
const PROFILE_NAME: &str = "alexandria.blocks.v1";

static NEXT_BACKEND_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct AlexandriaBackendConfig {
    pub backend_id: String,
    pub label: String,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub path: PathBuf,
    pub access_mode: ExternalAccessMode,
    pub publisher: String,
    /// Optional expected whole-file digest. Immutable opening hashes the file
    /// once and rejects a mismatch; live mode rejects static digest claims.
    pub verified_source_sha256: Option<String>,
    /// Optional registration identity associated with a previously verified
    /// digest. Immutable opening checks every available identity field and
    /// still rehashes once; live mode rejects static identity claims.
    pub verified_source_identity: Option<SourceIdentity>,
    pub rights: Vec<RightsStatement>,
    pub use_policy: UsePolicy,
    pub context_radius: u8,
    pub max_snippet_chars: u32,
}

impl AlexandriaBackendConfig {
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
            context_radius: 1,
            max_snippet_chars: 700,
        }
    }

    fn validate(&self) -> Result<(), InformationError> {
        if self.backend_id.trim().is_empty()
            || self.label.trim().is_empty()
            || self.publisher.trim().is_empty()
        {
            return Err(input_error(
                "invalid_sqlite_backend_config",
                "backend id, label, and publisher must not be empty",
            ));
        }
        if self.context_radius > MAX_CONTEXT_RADIUS
            || self.max_snippet_chars == 0
            || self.max_snippet_chars > MAX_SNIPPET_CHARS
        {
            return Err(input_error(
                "invalid_sqlite_backend_config",
                "context radius or snippet budget is outside the supported bounds",
            ));
        }
        if let Some(digest) = &self.verified_source_sha256 {
            let normalized = digest
                .strip_prefix("sha256:")
                .map_or(digest.as_str(), |value| value);
            if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(input_error(
                    "invalid_verified_source_sha256",
                    "verified SQLite source digest is not a SHA-256 value",
                ));
            }
            if self.access_mode == ExternalAccessMode::LiveReadOnly {
                return Err(input_error(
                    "live_sqlite_static_digest_forbidden",
                    "live SQLite sources cannot advertise a static whole-file digest",
                ));
            }
        }
        if let Some(identity) = &self.verified_source_identity {
            identity.validate().map_err(|error| {
                input_error(
                    "invalid_verified_source_identity",
                    format!("verified SQLite source identity is invalid: {error}"),
                )
            })?;
            if identity.sha256.is_none() || identity.modified_unix_nanos.is_none() {
                return Err(input_error(
                    "incomplete_verified_source_identity",
                    "verified SQLite source identity requires a digest and modification timestamp",
                ));
            }
            #[cfg(unix)]
            if identity.device.is_none() || identity.inode.is_none() {
                return Err(input_error(
                    "incomplete_verified_source_identity",
                    "verified SQLite source identity requires device and inode on this platform",
                ));
            }
            if self.access_mode == ExternalAccessMode::LiveReadOnly {
                return Err(input_error(
                    "live_sqlite_static_identity_forbidden",
                    "live SQLite sources cannot advertise a static verified identity",
                ));
            }
            if self
                .verified_source_sha256
                .as_ref()
                .zip(identity.sha256.as_ref())
                .is_some_and(|(left, right)| {
                    normalize_sha256(left.clone()) != normalize_sha256(right.clone())
                })
            {
                return Err(input_error(
                    "conflicting_verified_source_digest",
                    "verified SQLite digest fields disagree",
                ));
            }
        }
        for statement in &self.rights {
            statement.validate().map_err(|error| {
                input_error(
                    "invalid_sqlite_rights_statement",
                    format!("SQLite rights statement is invalid: {error}"),
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
                "sqlite_source_metadata_failed",
                format!("cannot inspect SQLite source metadata: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(integrity_error(
                "sqlite_source_is_symlink",
                "canonical SQLite source unexpectedly resolves to a symbolic link",
            ));
        }
        if !metadata.is_file() {
            return Err(input_error(
                "sqlite_source_not_file",
                "SQLite source path is not a regular file",
            ));
        }
        Self::from_metadata(&metadata)
    }

    fn from_metadata(metadata: &Metadata) -> Result<Self, InformationError> {
        if !metadata.is_file() {
            return Err(input_error(
                "sqlite_source_not_file",
                "SQLite source path is not a regular file",
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
                "sqlite_sidecar_is_symlink",
                format!("SQLite {label} sidecar must not be a symbolic link"),
            )),
            Ok(metadata) if !metadata.is_file() => Err(integrity_error(
                "sqlite_sidecar_not_file",
                format!("SQLite {label} sidecar must be a regular file"),
            )),
            Ok(metadata) => Ok(Self::Present(FileIdentity::from_metadata(&metadata)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::Absent),
            Err(error) => Err(io_error(
                "sqlite_sidecar_metadata_failed",
                format!("cannot inspect SQLite {label} sidecar metadata: {error}"),
            )),
        }
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

    fn is_nonempty(&self) -> bool {
        matches!(self, Self::Present(identity) if identity.bytes > 0)
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
                "sqlite_snapshot_nonempty_wal",
                "sidecar-free SQLite snapshots reject a non-empty sibling WAL; checkpoint or copy the database first",
            ));
        }
        if self.journal.is_nonempty() {
            return Err(integrity_error(
                "sqlite_snapshot_nonempty_journal",
                "sidecar-free SQLite snapshots reject a non-empty sibling rollback journal",
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
    data_version: i64,
    schema_version: i64,
    user_version: i64,
    application_id: i64,
    page_count: i64,
    freelist_count: i64,
    journal_mode: String,
}

impl SqliteSnapshotFacts {
    fn observe(connection: &Connection) -> Result<Self, InformationError> {
        Ok(Self {
            data_version: pragma_i64(connection, "data_version")?,
            schema_version: pragma_i64(connection, "schema_version")?,
            user_version: pragma_i64(connection, "user_version")?,
            application_id: pragma_i64(connection, "application_id")?,
            page_count: pragma_i64(connection, "page_count")?,
            freelist_count: pragma_i64(connection, "freelist_count")?,
            journal_mode: connection
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .map_err(map_sqlite_error)?,
        })
    }

    fn update_hasher(&self, hasher: &mut Sha256) {
        for value in [
            self.data_version,
            self.schema_version,
            self.user_version,
            self.application_id,
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

pub struct AlexandriaBackend {
    descriptor: BackendDescriptor,
    path: PathBuf,
    access_mode: ExternalAccessMode,
    publisher: String,
    immutable_source_sha256: Option<String>,
    rights: Vec<RightsStatement>,
    use_policy: UsePolicy,
    context_radius: u8,
    max_snippet_chars: u32,
    initial_identity: FileIdentity,
    instance_id: u64,
    operation_sequence: AtomicU64,
}

impl AlexandriaBackend {
    pub fn open(config: AlexandriaBackendConfig) -> Result<Self, InformationError> {
        config.validate()?;
        let path = fs::canonicalize(&config.path).map_err(|error| {
            io_error(
                "sqlite_source_canonicalize_failed",
                format!("cannot resolve SQLite source path: {error}"),
            )
        })?;
        let initial_identity = FileIdentity::observe(&path)?;
        SidecarSet::observe(&path)?.ensure_snapshot_safe()?;
        if let Some(expected_identity) = &config.verified_source_identity {
            verify_registered_identity(&initial_identity, expected_identity)?;
        }
        let expected_sha256 = config
            .verified_source_identity
            .as_ref()
            .and_then(|identity| identity.sha256.clone())
            .or_else(|| config.verified_source_sha256.clone())
            .map(normalize_sha256);
        let immutable_source_sha256 = match config.access_mode {
            ExternalAccessMode::LiveReadOnly => None,
            ExternalAccessMode::ImmutableReadOnly => {
                let observed = hash_immutable_source(&path, &initial_identity)?;
                if expected_sha256
                    .as_ref()
                    .is_some_and(|expected| expected != &observed)
                {
                    return Err(integrity_error(
                        "immutable_sqlite_digest_mismatch",
                        "immutable SQLite source does not match its expected SHA-256 digest",
                    ));
                }
                Some(observed)
            }
        };
        let use_policy = intersect_rights_into_use_policy(config.use_policy, &config.rights);
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
            use_policy,
            context_radius: config.context_radius,
            max_snippet_chars: config.max_snippet_chars,
            initial_identity,
            instance_id: NEXT_BACKEND_INSTANCE_ID.fetch_add(1, Ordering::Relaxed),
            operation_sequence: AtomicU64::new(1),
        };
        backend.with_connection(5_000, |_connection, _fingerprint| Ok(()))?;
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

    #[must_use]
    pub fn source_fingerprint(&self) -> String {
        match &self.immutable_source_sha256 {
            Some(digest) => format!("sha256:{digest}"),
            None => {
                let mut hasher = Sha256::new();
                hasher.update(b"information-native.sqlite-live-reference.v1\0");
                hasher.update(self.instance_id.to_le_bytes());
                hasher.update(self.next_operation_sequence().to_le_bytes());
                hasher.update(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_nanos())
                        .to_le_bytes(),
                );
                self.initial_identity.update_hasher(&mut hasher);
                format!(
                    "volatile-live-reference-v1:{}",
                    hex::encode(hasher.finalize())
                )
            }
        }
    }

    fn operation_fingerprint(
        &self,
        identity: &FileIdentity,
        sidecars: &SidecarSet,
        facts: &SqliteSnapshotFacts,
    ) -> String {
        match &self.immutable_source_sha256 {
            Some(digest) => format!("sha256:{digest}"),
            None => {
                let mut hasher = Sha256::new();
                hasher.update(b"information-native.sqlite-live-snapshot.v1\0");
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
                format!(
                    "volatile-sqlite-snapshot-v1:{}",
                    hex::encode(hasher.finalize())
                )
            }
        }
    }

    fn next_operation_sequence(&self) -> u64 {
        self.operation_sequence.fetch_add(1, Ordering::Relaxed)
    }

    fn ensure_purpose_allowed(
        &self,
        policy: UsePolicy,
        purpose: RetrievalPurpose,
    ) -> Result<(), InformationError> {
        if policy.permission_for(purpose) != UsePermission::Allowed {
            let mut error = InformationError::new(
                ErrorClass::Permission,
                "retrieval_purpose_not_permitted",
                "the requested retrieval purpose is not explicitly allowed by the resource use policy",
            );
            error.resource_id = Some(self.descriptor.resource_id.clone());
            error.representation_id = Some(self.descriptor.representation_id.clone());
            return Err(error);
        }
        Ok(())
    }

    fn with_connection<T>(
        &self,
        timeout_ms: u64,
        operation: impl FnOnce(&Connection, &str) -> Result<T, InformationError>,
    ) -> Result<T, InformationError> {
        let before = FileIdentity::observe(&self.path)?;
        let before_sidecars = SidecarSet::observe(&self.path)?;
        before_sidecars.ensure_snapshot_safe()?;
        if self.access_mode == ExternalAccessMode::ImmutableReadOnly
            && before != self.initial_identity
        {
            return Err(integrity_error(
                "immutable_sqlite_identity_changed",
                "immutable SQLite source identity changed after registration",
            ));
        }

        let connection = open_read_only_connection(&self.path, timeout_ms)?;
        connection
            .execute_batch("BEGIN DEFERRED TRANSACTION")
            .map_err(map_sqlite_error)?;
        let result = (|| {
            let connected_identity = FileIdentity::observe(&self.path)?;
            if connected_identity != before {
                return Err(integrity_error(
                    "sqlite_identity_changed_before_read",
                    "SQLite source identity changed while opening a read snapshot",
                ));
            }
            validate_alexandria_schema(&connection)?;
            let facts = SqliteSnapshotFacts::observe(&connection)?;
            let snapshot_sidecars = SidecarSet::observe(&self.path)?;
            snapshot_sidecars.ensure_snapshot_safe()?;
            if snapshot_sidecars != before_sidecars {
                return Err(integrity_error(
                    "sqlite_sidecar_changed",
                    "SQLite sidecar identity changed while opening a read snapshot",
                ));
            }
            let fingerprint =
                self.operation_fingerprint(&connected_identity, &snapshot_sidecars, &facts);
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
        let after_sidecars = SidecarSet::observe(&self.path)?;
        after_sidecars.ensure_snapshot_safe()?;
        if before != after {
            return Err(integrity_error(
                "sqlite_identity_changed_during_read",
                "SQLite source identity changed during a read operation; results were discarded",
            ));
        }
        if before_sidecars != after_sidecars {
            return Err(integrity_error(
                "sqlite_sidecar_changed",
                "SQLite sidecar identity changed during a read operation",
            ));
        }
        result
    }

    fn search_connection(
        &self,
        connection: &Connection,
        source_fingerprint: &str,
        query: &InformationQuery,
    ) -> Result<BackendSearchResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, query.purpose)?;
        query.validate().map_err(|error| {
            input_error(
                "invalid_query",
                format!("query contract is invalid: {error}"),
            )
        })?;
        validate_query_for_backend(query, &self.descriptor)?;
        let started = Instant::now();
        let (candidates, mut warnings) = query_candidates(&query.text, query.syntax)?;
        let limit = query.budget.max_hits.min(query.budget.max_hits_per_backend);
        let fetch_limit = i64::from(limit).saturating_add(1);
        let mut selected_hits = None;
        let mut first_empty = None;
        let mut query_error_count = 0_usize;

        for (index, candidate) in candidates.iter().enumerate() {
            match run_fts_query(
                connection,
                &candidate.query,
                query,
                fetch_limit,
                self.max_snippet_chars,
            ) {
                Ok(hits) if hits.is_empty() => {
                    if first_empty.is_none() {
                        first_empty = Some(hits);
                    }
                }
                Ok(hits) => {
                    if index > 0 {
                        warnings.push(format!(
                            "used {} query fallback after earlier candidates produced no usable hits",
                            candidate.label
                        ));
                    }
                    selected_hits = Some(hits);
                    break;
                }
                Err(error) => {
                    query_error_count = query_error_count.saturating_add(1);
                    warnings.push(format!(
                        "{} FTS query candidate failed: {}",
                        candidate.label,
                        safe_sqlite_summary(&error)
                    ));
                }
            }
        }

        let mut raw_hits = if let Some(hits) = selected_hits {
            hits
        } else if let Some(hits) = first_empty {
            hits
        } else {
            return Err(InformationError::new(
                ErrorClass::InvalidInput,
                "all_fts_queries_failed",
                "all FTS query candidates were rejected by SQLite",
            ));
        };

        let mut complete = query_error_count == 0;
        let target_len = limit as usize;
        if raw_hits.len() > target_len {
            raw_hits.truncate(target_len);
            complete = false;
            warnings.push("per-backend hit budget truncated additional matches".to_string());
        }

        let mut hits = Vec::with_capacity(raw_hits.len());
        let mut remaining_chars = query.budget.max_context_chars as usize;
        let raw_count = raw_hits.len();
        for (index, raw) in raw_hits.into_iter().enumerate() {
            let remaining_hits = raw_count.saturating_sub(index).max(1);
            let share = remaining_chars / remaining_hits;
            let (snippet, snippet_truncated) =
                truncate_chars(&raw.snippet, share.min(self.max_snippet_chars as usize));
            let snippet_chars = snippet.chars().count();
            let context_budget = share.saturating_sub(snippet_chars);
            let (context, context_truncated) = fetch_context(
                connection,
                &raw.doc_id,
                raw.block_index,
                self.context_radius,
                context_budget,
            )?;
            let context_chars = context.chars().count();
            remaining_chars =
                remaining_chars.saturating_sub(snippet_chars.saturating_add(context_chars));
            let (themes, themes_truncated) = fetch_themes(connection, &raw.block_id)?;
            if snippet_truncated || context_truncated || themes_truncated {
                complete = false;
            }
            let rank = u32::try_from(index.saturating_add(1)).map_err(|_| {
                integrity_error(
                    "sqlite_rank_overflow",
                    "SQLite result rank exceeded supported bounds",
                )
            })?;
            hits.push(self.block_evidence(
                raw,
                themes,
                snippet,
                context,
                rank,
                ScoreSemantics::LowerIsBetter,
                source_fingerprint,
                query.purpose,
            )?);
        }
        if !complete {
            warnings.push(
                "one or more result, context, theme, or query limits made this response partial"
                    .to_string(),
            );
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
        source_fingerprint: &str,
        request: &ReadRequest,
    ) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        validate_direct_read_budget(request.max_context_chars, request.timeout_ms)?;
        validate_read_identity(
            &self.descriptor,
            &request.resource_id,
            &request.release_id,
            &request.representation_id,
        )?;
        let EvidenceLocator::SqliteBlock {
            block_id,
            doc_id,
            block_index,
            ..
        } = &request.locator
        else {
            return Err(InformationError::new(
                ErrorClass::Unsupported,
                "unsupported_sqlite_locator",
                "Alexandria reads require a sqlite_block locator",
            ));
        };
        validate_lookup_key(block_id)?;
        let started = Instant::now();
        let raw = load_block(connection, block_id, self.max_snippet_chars)?;
        if &raw.doc_id != doc_id || raw.block_index != *block_index {
            return Err(integrity_error(
                "sqlite_locator_mismatch",
                "sqlite_block locator fields do not identify the same stored block",
            ));
        }
        self.read_raw_block(
            connection,
            source_fingerprint,
            raw,
            request.max_context_chars,
            started,
            request.purpose,
        )
    }

    fn lookup_connection(
        &self,
        connection: &Connection,
        source_fingerprint: &str,
        request: &LookupRequest,
    ) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        validate_direct_read_budget(request.max_context_chars, request.timeout_ms)?;
        validate_read_identity(
            &self.descriptor,
            &request.resource_id,
            &request.release_id,
            &request.representation_id,
        )?;
        match request.collection.as_deref() {
            None | Some("block") | Some("blocks") => {
                validate_lookup_key(&request.key)?;
                let started = Instant::now();
                let raw = load_block(connection, &request.key, self.max_snippet_chars)?;
                self.read_raw_block(
                    connection,
                    source_fingerprint,
                    raw,
                    request.max_context_chars,
                    started,
                    request.purpose,
                )
            }
            Some(_) => Err(InformationError::new(
                ErrorClass::Unsupported,
                "unsupported_sqlite_collection",
                "Alexandria record lookup supports only the blocks collection",
            )),
        }
    }

    fn read_raw_block(
        &self,
        connection: &Connection,
        source_fingerprint: &str,
        raw: RawHit,
        max_context_chars: u32,
        started: Instant,
        purpose: RetrievalPurpose,
    ) -> Result<BackendReadResult, InformationError> {
        let snippet_budget = (max_context_chars as usize).min(self.max_snippet_chars as usize);
        let (snippet, snippet_truncated) = truncate_chars(&raw.snippet, snippet_budget);
        let context_budget = (max_context_chars as usize).saturating_sub(snippet.chars().count());
        let (context, context_truncated) = fetch_context(
            connection,
            &raw.doc_id,
            raw.block_index,
            self.context_radius,
            context_budget,
        )?;
        let (themes, themes_truncated) = fetch_themes(connection, &raw.block_id)?;
        let complete = !(snippet_truncated || context_truncated || themes_truncated);
        let warnings = if complete {
            Vec::new()
        } else {
            vec!["read text or theme metadata was truncated to its budget".to_string()]
        };
        let hit = self.block_evidence(
            raw,
            themes,
            snippet,
            context,
            1,
            ScoreSemantics::RankOnly,
            source_fingerprint,
            purpose,
        )?;
        Ok(BackendReadResult {
            complete,
            warnings,
            hit,
            elapsed_ms: elapsed_millis(started),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn block_evidence(
        &self,
        raw: RawHit,
        themes: Vec<ThemeHit>,
        snippet: String,
        context: String,
        rank: u32,
        score_semantics: ScoreSemantics,
        source_fingerprint: &str,
        purpose: RetrievalPurpose,
    ) -> Result<EvidenceHit, InformationError> {
        validate_row_identity(&raw)?;
        if raw.source_uri.trim().is_empty() {
            return Err(integrity_error(
                "sqlite_empty_source_uri",
                "Alexandria document has an empty source URI",
            ));
        }
        let mut metadata = BTreeMap::new();
        metadata.insert("profile".to_string(), json!(PROFILE_NAME));
        metadata.insert("block_type".to_string(), json!(raw.block_type));
        metadata.insert("tradition_tag".to_string(), json!(raw.tradition_tag));
        metadata.insert("date_range".to_string(), json!(raw.date_range));
        metadata.insert(
            "language_original".to_string(),
            json!(raw.language_original),
        );
        metadata.insert(
            "language_translation".to_string(),
            json!(raw.language_translation),
        );
        metadata.insert("canonical_path".to_string(), json!(raw.canonical_path));
        metadata.insert("rights_status".to_string(), json!(&raw.rights_status));
        metadata.insert(
            "themes".to_string(),
            JsonValue::Array(
                themes
                    .into_iter()
                    .map(|theme| {
                        json!({
                            "theme_tag": theme.theme_tag,
                            "matched_term": theme.matched_term,
                            "controversy_risk": theme.controversy_risk,
                        })
                    })
                    .collect(),
            ),
        );
        metadata.insert(
            "excerpt_hash_basis".to_string(),
            json!("information_native_types::evidence_text_sha256 over exact snippet and context"),
        );
        metadata.insert(
            "access_mode".to_string(),
            json!(access_mode_name(self.access_mode)),
        );
        let excerpt_sha256 = evidence_text_sha256(&snippet, &context);
        let mut rights = self.rights.clone();
        let status = raw
            .rights_status
            .as_deref()
            .map(str::trim)
            .filter(|status| !status.is_empty())
            .ok_or_else(|| {
                integrity_error(
                    "sqlite_empty_rights_status",
                    "Alexandria document has an empty rights status",
                )
            })?;
        let classified_rights = classify_rights_status(status);
        rights.push(RightsStatement {
            scope: format!("document:{}", raw.doc_id),
            expression: status.to_string(),
            license_url: classified_rights.license_url,
            license_text_sha256: None,
            attribution: Some(self.publisher.clone()),
            obligations: classified_rights.obligations,
            redistribution: classified_rights.redistribution,
        });
        let hit_use_policy = intersect_rights_into_use_policy(self.use_policy, &rights);
        self.ensure_purpose_allowed(hit_use_policy, purpose)?;
        let evidence_id = format!(
            "{}:{}:{}:{}",
            self.descriptor.resource_id,
            self.descriptor.release_id,
            self.descriptor.representation_id,
            raw.block_id
        );
        let score = if score_semantics == ScoreSemantics::RankOnly {
            1.0
        } else {
            raw.score
        };
        let hit = EvidenceHit {
            evidence_id,
            resource_id: self.descriptor.resource_id.clone(),
            release_id: self.descriptor.release_id.clone(),
            representation_id: self.descriptor.representation_id.clone(),
            backend_id: self.descriptor.backend_id.clone(),
            rank,
            score: EvidenceScore {
                value: score,
                semantics: score_semantics,
                fused_value: None,
            },
            title: raw.title,
            creator: raw.creator,
            snippet,
            context,
            excerpt_sha256,
            source_fingerprint: Some(source_fingerprint.to_string()),
            document_id: Some(raw.doc_id.clone()),
            passage_id: Some(raw.block_id.clone()),
            locator: EvidenceLocator::SqliteBlock {
                block_id: raw.block_id,
                doc_id: raw.doc_id.clone(),
                block_index: raw.block_index,
                location_path: raw.location_path,
            },
            source_uri: Some(raw.source_uri.clone()),
            provenance: Provenance {
                publisher: self.publisher.clone(),
                source_uri: raw.source_uri,
                upstream_record_id: Some(raw.doc_id),
                source_inputs: vec![format!("sqlite-source-{source_fingerprint}")],
                transformation: Some(
                    "read-only Alexandria SQLite/FTS5 retrieval with bounded context".to_string(),
                ),
                metadata: BTreeMap::from([
                    ("profile".to_string(), json!(PROFILE_NAME)),
                    (
                        "source_fingerprint_kind".to_string(),
                        json!(source_fingerprint_kind(source_fingerprint)),
                    ),
                ]),
            },
            rights,
            use_policy: hit_use_policy,
            metadata,
        };
        hit.validate().map_err(|error| {
            integrity_error(
                "invalid_sqlite_evidence",
                format!("generated SQLite evidence failed validation: {error}"),
            )
        })?;
        Ok(hit)
    }
}

impl ResourceBackend for AlexandriaBackend {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    fn health(&self) -> BackendHealth {
        match self.with_connection(1_000, |connection, _identity| {
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
        self.ensure_purpose_allowed(self.use_policy, query.purpose)?;
        query.validate().map_err(|error| {
            input_error(
                "invalid_query",
                format!("query contract is invalid: {error}"),
            )
        })?;
        validate_query_for_backend(query, &self.descriptor)?;
        self.with_connection(query.budget.timeout_ms, |connection, fingerprint| {
            self.search_connection(connection, fingerprint, query)
        })
    }

    fn read(&self, request: &ReadRequest) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        validate_direct_read_budget(request.max_context_chars, request.timeout_ms)?;
        self.with_connection(request.timeout_ms, |connection, fingerprint| {
            self.read_connection(connection, fingerprint, request)
        })
    }

    fn lookup(&self, request: &LookupRequest) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        validate_direct_read_budget(request.max_context_chars, request.timeout_ms)?;
        self.with_connection(request.timeout_ms, |connection, fingerprint| {
            self.lookup_connection(connection, fingerprint, request)
        })
    }
}

fn open_read_only_connection(path: &Path, timeout_ms: u64) -> Result<Connection, InformationError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut uri = Url::from_file_path(path).map_err(|()| {
        input_error(
            "sqlite_path_not_file_uri",
            "SQLite source path cannot be represented as a file URI",
        )
    })?;
    uri.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
    let connection = Connection::open_with_flags(uri.as_str(), flags | OpenFlags::SQLITE_OPEN_URI)
        .map_err(map_sqlite_error)?;
    connection
        .set_limit(
            Limit::SQLITE_LIMIT_VARIABLE_NUMBER,
            i32::try_from(MAX_SQLITE_BINDS).map_err(|_| {
                integrity_error(
                    "sqlite_bind_limit_overflow",
                    "Alexandria bind limit exceeds SQLite integer bounds",
                )
            })?,
        )
        .map_err(map_sqlite_error)?;
    if connection
        .limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER)
        .map_err(map_sqlite_error)?
        < i32::try_from(MAX_SQLITE_BINDS).unwrap_or(i32::MAX)
    {
        return Err(InformationError::new(
            ErrorClass::Unsupported,
            "sqlite_bind_limit_too_low",
            "SQLite runtime cannot support the Alexandria profile bind limit",
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
    let trusted_schema = connection
        .pragma_query_value(None, "trusted_schema", |row| row.get::<_, i64>(0))
        .map_err(map_sqlite_error)?;
    let query_only = connection
        .pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))
        .map_err(map_sqlite_error)?;
    if trusted_schema != 0 || query_only != 1 {
        return Err(integrity_error(
            "sqlite_read_only_pragmas_failed",
            "SQLite did not retain trusted_schema=OFF and query_only=ON",
        ));
    }
    let now = Instant::now();
    let deadline = now
        .checked_add(Duration::from_millis(timeout_ms))
        .map_or(now, |value| value);
    connection
        .progress_handler(1_000, Some(move || Instant::now() >= deadline))
        .map_err(map_sqlite_error)?;
    Ok(connection)
}

#[cfg(test)]
fn sibling_wal_path(path: &Path) -> Result<PathBuf, InformationError> {
    sibling_sidecar_path(path, "-wal")
}

fn sibling_sidecar_path(path: &Path, suffix: &str) -> Result<PathBuf, InformationError> {
    let Some(file_name) = path.file_name() else {
        return Err(input_error(
            "sqlite_path_has_no_file_name",
            "SQLite source path has no file name",
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
            "verified_sqlite_identity_mismatch",
            "SQLite source no longer matches the identity bound to its verified digest",
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
            "immutable_sqlite_hash_open_failed",
            format!("cannot open immutable SQLite source for verification: {error}"),
        )
    })?;
    let opened_identity = FileIdentity::from_metadata(&file.metadata().map_err(|error| {
        io_error(
            "immutable_sqlite_hash_metadata_failed",
            format!("cannot inspect opened immutable SQLite source: {error}"),
        )
    })?)?;
    if &opened_identity != expected_identity {
        return Err(integrity_error(
            "immutable_sqlite_identity_changed_before_hash",
            "immutable SQLite identity changed before digest verification",
        ));
    }

    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut hasher = Sha256::new();
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            io_error(
                "immutable_sqlite_hash_read_failed",
                format!("cannot hash immutable SQLite source: {error}"),
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
                "immutable_sqlite_hash_metadata_failed",
                format!("cannot re-inspect opened immutable SQLite source: {error}"),
            )
        })?)?;
    let path_after = FileIdentity::observe(path)?;
    if opened_identity != handle_after || &path_after != expected_identity {
        return Err(integrity_error(
            "immutable_sqlite_identity_changed_during_hash",
            "immutable SQLite identity changed during digest verification",
        ));
    }
    Ok(hex::encode(hasher.finalize()))
}

fn validate_alexandria_schema(connection: &Connection) -> Result<(), InformationError> {
    require_table_columns(
        connection,
        "documents",
        &[
            "doc_id",
            "title",
            "author_normalized",
            "author_attributed",
            "tradition_tag",
            "date_range",
            "language_original",
            "language_translation",
            "source_uri",
            "rights_status",
            "genre",
            "canonical_path",
        ],
    )?;
    require_table_columns(
        connection,
        "blocks",
        &[
            "block_id",
            "doc_id",
            "block_index",
            "block_type",
            "text",
            "location_path",
        ],
    )?;
    require_table_columns(connection, "blocks_fts", &["block_id", "doc_id", "text"])?;
    require_table_columns(
        connection,
        "block_theme_hits",
        &[
            "doc_id",
            "block_id",
            "theme_tag",
            "matched_term",
            "controversy_risk",
        ],
    )?;
    let fts_sql_value = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            ["blocks_fts"],
            |row| row.get::<_, Option<String>>(0),
        )
        .map_err(map_sqlite_error)?;
    let fts_sql = option_string_or_empty(fts_sql_value).to_ascii_lowercase();
    if !fts_sql.contains("virtual table") || !fts_sql.contains("using fts5") {
        return Err(integrity_error(
            "sqlite_profile_invalid_fts",
            "blocks_fts is not an FTS5 virtual table",
        ));
    }
    for required_index in ["idx_blocks_doc_idx", "idx_theme_hits_block"] {
        let exists = connection
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1",
                [required_index],
                |_row| Ok(()),
            )
            .optional()
            .map_err(map_sqlite_error)?
            .is_some();
        if !exists {
            return Err(integrity_error(
                "sqlite_profile_missing_index",
                format!("Alexandria profile is missing required index {required_index}"),
            ));
        }
    }
    Ok(())
}

fn require_table_columns(
    connection: &Connection,
    table: &'static str,
    required: &[&str],
) -> Result<(), InformationError> {
    let sql = match table {
        "documents" => "PRAGMA table_info(documents)",
        "blocks" => "PRAGMA table_info(blocks)",
        "blocks_fts" => "PRAGMA table_info(blocks_fts)",
        "block_theme_hits" => "PRAGMA table_info(block_theme_hits)",
        _ => {
            return Err(integrity_error(
                "sqlite_profile_internal_table",
                "backend attempted to inspect an unknown profile table",
            ));
        }
    };
    let mut statement = connection.prepare(sql).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(map_sqlite_error)?;
    let mut columns = BTreeSet::new();
    for row in rows {
        columns.insert(row.map_err(map_sqlite_error)?);
    }
    let missing = required
        .iter()
        .filter(|column| !columns.contains(**column))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(integrity_error(
            "sqlite_profile_missing_columns",
            format!(
                "Alexandria table {table} is missing columns: {}",
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}

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
    if !query.resources.is_empty() && !query.resources.contains(&descriptor.resource_id) {
        return Err(input_error(
            "query_resource_mismatch",
            "query resource selection excludes this backend",
        ));
    }
    if !query.representations.is_empty()
        && !query
            .representations
            .contains(&descriptor.representation_id)
    {
        return Err(input_error(
            "query_representation_mismatch",
            "query representation selection excludes this backend",
        ));
    }
    if query.filters.spatial.is_some()
        || query.filters.temporal_start.is_some()
        || query.filters.temporal_end.is_some()
    {
        return Err(InformationError::new(
            ErrorClass::Unsupported,
            "sqlite_filter_unsupported",
            "Alexandria does not advertise spatial or temporal filtering",
        ));
    }
    for (name, values) in [
        ("languages", &query.filters.languages),
        ("subjects", &query.filters.subjects),
        ("document_ids", &query.filters.document_ids),
    ] {
        if values.len() > MAX_FILTER_VALUES_PER_LIST {
            return Err(input_error(
                "sqlite_filter_value_limit_exceeded",
                format!(
                    "Alexandria filter {name} exceeds the profile cap of {MAX_FILTER_VALUES_PER_LIST} values"
                ),
            ));
        }
    }
    if query.filters.fields.len() > MAX_FIELD_FILTERS {
        return Err(input_error(
            "sqlite_field_filter_limit_exceeded",
            format!(
                "Alexandria field filters exceed the profile cap of {MAX_FIELD_FILTERS} entries"
            ),
        ));
    }
    let bind_count = 3_usize
        .checked_add(query.filters.document_ids.len())
        .and_then(|count| {
            query
                .filters
                .languages
                .len()
                .checked_mul(2)
                .and_then(|languages| count.checked_add(languages))
        })
        .and_then(|count| count.checked_add(query.filters.subjects.len()))
        .and_then(|count| count.checked_add(query.filters.fields.len()))
        .ok_or_else(|| {
            input_error(
                "sqlite_filter_bind_overflow",
                "Alexandria filter bind accounting overflowed",
            )
        })?;
    if bind_count > MAX_SQLITE_BINDS {
        return Err(input_error(
            "sqlite_filter_bind_limit_exceeded",
            format!(
                "Alexandria query requires {bind_count} binds, exceeding the profile cap of {MAX_SQLITE_BINDS}"
            ),
        ));
    }
    let supported_fields = BTreeSet::from([
        "author",
        "block_type",
        "controversy_risk",
        "genre",
        "source_uri",
        "theme_tag",
        "tradition_tag",
    ]);
    if let Some(field) = query
        .filters
        .fields
        .keys()
        .find(|field| !supported_fields.contains(field.as_str()))
    {
        return Err(InformationError::new(
            ErrorClass::Unsupported,
            "sqlite_field_filter_unsupported",
            format!("Alexandria does not support the requested field filter: {field}"),
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
            "invalid_read_budget",
            "read budget is outside the supported bounds",
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
            "read_identity_mismatch",
            "read request does not target this backend identity",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct QueryCandidate {
    label: &'static str,
    query: String,
}

fn query_candidates(
    input: &str,
    syntax: QuerySyntax,
) -> Result<(Vec<QueryCandidate>, Vec<String>), InformationError> {
    let (terms, terms_truncated) = safe_query_terms(input);
    let exact = exact_phrase(input);
    let all = join_terms(&terms, " AND ");
    let any = join_terms(&terms, " OR ");
    let mut candidates = match syntax {
        QuerySyntax::NaturalTerms => vec![
            QueryCandidate {
                label: "exact phrase",
                query: exact,
            },
            QueryCandidate {
                label: "all terms",
                query: all,
            },
            QueryCandidate {
                label: "any terms",
                query: any,
            },
        ],
        QuerySyntax::ExactPhrase => vec![QueryCandidate {
            label: "exact phrase",
            query: exact,
        }],
        QuerySyntax::AllTerms => vec![QueryCandidate {
            label: "all terms",
            query: all,
        }],
        QuerySyntax::AnyTerms => vec![QueryCandidate {
            label: "any terms",
            query: any,
        }],
        QuerySyntax::BackendNative => vec![
            QueryCandidate {
                label: "backend-native",
                query: input.to_string(),
            },
            QueryCandidate {
                label: "escaped all-terms",
                query: all,
            },
            QueryCandidate {
                label: "escaped any-terms",
                query: any,
            },
        ],
    };
    let mut seen = BTreeSet::new();
    candidates.retain(|candidate| {
        !candidate.query.trim().is_empty() && seen.insert(candidate.query.clone())
    });
    if candidates.is_empty() {
        return Err(input_error(
            "fts_query_has_no_terms",
            "query contains no searchable terms",
        ));
    }
    let warnings = if terms_truncated {
        vec![format!(
            "query tokenization was bounded to {MAX_QUERY_TERMS} searchable terms"
        )]
    } else {
        Vec::new()
    };
    Ok((candidates, warnings))
}

fn exact_phrase(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
}

fn safe_query_terms(input: &str) -> (Vec<String>, bool) {
    let mut all = input
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>();
    let truncated = all.len() > MAX_QUERY_TERMS;
    all.truncate(MAX_QUERY_TERMS);
    (all, truncated)
}

fn join_terms(terms: &[String], operator: &str) -> String {
    terms.join(operator)
}

#[derive(Debug)]
struct RawHit {
    block_id: String,
    doc_id: String,
    block_index: i64,
    block_type: String,
    location_path: Option<String>,
    title: String,
    creator: Option<String>,
    tradition_tag: Option<String>,
    date_range: Option<String>,
    language_original: Option<String>,
    language_translation: Option<String>,
    source_uri: String,
    canonical_path: Option<String>,
    rights_status: Option<String>,
    score: f64,
    snippet: String,
}

fn run_fts_query(
    connection: &Connection,
    fts_query: &str,
    query: &InformationQuery,
    limit: i64,
    max_snippet_chars: u32,
) -> rusqlite::Result<Vec<RawHit>> {
    let mut sql = String::from(
        r#"
        SELECT
            b.block_id,
            b.doc_id,
            b.block_index,
            substr(b.block_type, 1, 512),
            substr(b.location_path, 1, 4096),
            substr(d.title, 1, 2048),
            substr(COALESCE(d.author_normalized, d.author_attributed), 1, 1024),
            substr(d.tradition_tag, 1, 512),
            substr(d.date_range, 1, 512),
            substr(d.language_original, 1, 512),
            substr(d.language_translation, 1, 512),
            substr(d.source_uri, 1, 8192),
            substr(d.canonical_path, 1, 4096),
            substr(d.rights_status, 1, 512),
            bm25(blocks_fts) AS native_score,
            substr(snippet(blocks_fts, 2, '[', ']', ' ... ', 32), 1, ?)
        FROM blocks_fts
        JOIN blocks b ON b.block_id = blocks_fts.block_id
        JOIN documents d ON d.doc_id = b.doc_id
        WHERE blocks_fts MATCH ?
          AND length(b.block_id) BETWEEN 1 AND 8192
          AND length(b.doc_id) BETWEEN 1 AND 8192
        "#,
    );
    let mut values = vec![
        SqlValue::Integer(i64::from(max_snippet_chars).saturating_add(1)),
        SqlValue::Text(fts_query.to_string()),
    ];
    add_in_filter(
        &mut sql,
        &mut values,
        "b.doc_id",
        &query.filters.document_ids,
    );
    if !query.filters.languages.is_empty() {
        let placeholders = placeholders(query.filters.languages.len());
        sql.push_str(&format!(
            "\nAND (d.language_original IN ({placeholders}) OR d.language_translation IN ({placeholders}))"
        ));
        push_text_values(&mut values, &query.filters.languages);
        push_text_values(&mut values, &query.filters.languages);
    }
    if !query.filters.subjects.is_empty() {
        sql.push_str(&format!(
            "\nAND EXISTS (SELECT 1 FROM block_theme_hits subject_hits WHERE subject_hits.block_id = b.block_id AND subject_hits.theme_tag IN ({}))",
            placeholders(query.filters.subjects.len())
        ));
        push_text_values(&mut values, &query.filters.subjects);
    }
    for (field, value) in &query.filters.fields {
        match field.as_str() {
            "author" => sql.push_str("\nAND COALESCE(d.author_normalized, d.author_attributed) = ?"),
            "block_type" => sql.push_str("\nAND b.block_type = ?"),
            "genre" => sql.push_str("\nAND d.genre = ?"),
            "source_uri" => sql.push_str("\nAND d.source_uri = ?"),
            "tradition_tag" => sql.push_str("\nAND d.tradition_tag = ?"),
            "theme_tag" => sql.push_str("\nAND EXISTS (SELECT 1 FROM block_theme_hits theme_filter WHERE theme_filter.block_id = b.block_id AND theme_filter.theme_tag = ?)"),
            "controversy_risk" => sql.push_str("\nAND EXISTS (SELECT 1 FROM block_theme_hits risk_filter WHERE risk_filter.block_id = b.block_id AND risk_filter.controversy_risk = ?)"),
            _ => return Err(rusqlite::Error::InvalidParameterName(field.clone())),
        }
        values.push(SqlValue::Text(value.clone()));
    }
    sql.push_str("\nORDER BY native_score ASC, b.block_id ASC LIMIT ?");
    values.push(SqlValue::Integer(limit));
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), raw_hit_from_row)?;
    rows.collect()
}

fn raw_hit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawHit> {
    Ok(RawHit {
        block_id: row.get(0)?,
        doc_id: row.get(1)?,
        block_index: row.get(2)?,
        block_type: row.get(3)?,
        location_path: row.get(4)?,
        title: row.get(5)?,
        creator: row.get(6)?,
        tradition_tag: row.get(7)?,
        date_range: row.get(8)?,
        language_original: row.get(9)?,
        language_translation: row.get(10)?,
        source_uri: row.get(11)?,
        canonical_path: row.get(12)?,
        rights_status: row.get(13)?,
        score: row.get(14)?,
        snippet: option_string_or_empty(row.get::<_, Option<String>>(15)?),
    })
}

fn load_block(
    connection: &Connection,
    block_id: &str,
    max_snippet_chars: u32,
) -> Result<RawHit, InformationError> {
    connection
        .query_row(
            r#"
            SELECT
                b.block_id,
                b.doc_id,
                b.block_index,
                substr(b.block_type, 1, 512),
                substr(b.location_path, 1, 4096),
                substr(d.title, 1, 2048),
                substr(COALESCE(d.author_normalized, d.author_attributed), 1, 1024),
                substr(d.tradition_tag, 1, 512),
                substr(d.date_range, 1, 512),
                substr(d.language_original, 1, 512),
                substr(d.language_translation, 1, 512),
                substr(d.source_uri, 1, 8192),
                substr(d.canonical_path, 1, 4096),
                substr(d.rights_status, 1, 512),
                1.0,
                substr(b.text, 1, ?2)
            FROM blocks b
            JOIN documents d ON d.doc_id = b.doc_id
            WHERE b.block_id = ?1
              AND length(b.block_id) BETWEEN 1 AND 8192
              AND length(b.doc_id) BETWEEN 1 AND 8192
            "#,
            params![block_id, i64::from(max_snippet_chars).saturating_add(1)],
            raw_hit_from_row,
        )
        .optional()
        .map_err(map_sqlite_error)?
        .ok_or_else(|| {
            let mut error = InformationError::new(
                ErrorClass::NotFound,
                "sqlite_block_not_found",
                "Alexandria block was not found",
            );
            error.retryable = false;
            error
        })
}

fn add_in_filter(sql: &mut String, values: &mut Vec<SqlValue>, column: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    sql.push_str(&format!(
        "\nAND {column} IN ({})",
        placeholders(items.len())
    ));
    push_text_values(values, items);
}

fn push_text_values(values: &mut Vec<SqlValue>, items: &[String]) {
    values.extend(items.iter().cloned().map(SqlValue::Text));
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

#[derive(Debug)]
struct ThemeHit {
    theme_tag: String,
    matched_term: String,
    controversy_risk: String,
}

fn fetch_themes(
    connection: &Connection,
    block_id: &str,
) -> Result<(Vec<ThemeHit>, bool), InformationError> {
    let limit = i64::try_from(MAX_THEME_VALUES.saturating_add(1)).map_err(|_| {
        integrity_error(
            "theme_limit_overflow",
            "theme metadata limit exceeded SQLite bounds",
        )
    })?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT
                substr(theme_tag, 1, 512),
                substr(matched_term, 1, 512),
                substr(controversy_risk, 1, 512)
            FROM block_theme_hits
            WHERE block_id = ?1
            ORDER BY theme_tag, matched_term, controversy_risk
            LIMIT ?2
            "#,
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(params![block_id, limit], |row| {
            Ok(ThemeHit {
                theme_tag: row.get(0)?,
                matched_term: row.get(1)?,
                controversy_risk: row.get(2)?,
            })
        })
        .map_err(map_sqlite_error)?;
    let mut themes = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_sqlite_error)?;
    let truncated = themes.len() > MAX_THEME_VALUES;
    themes.truncate(MAX_THEME_VALUES);
    Ok((themes, truncated))
}

fn fetch_context(
    connection: &Connection,
    doc_id: &str,
    block_index: i64,
    radius: u8,
    max_chars: usize,
) -> Result<(String, bool), InformationError> {
    if max_chars == 0 {
        return Ok((String::new(), radius > 0));
    }
    let start = block_index.saturating_sub(i64::from(radius));
    let end = block_index.saturating_add(i64::from(radius));
    let sqlite_text_limit = i64::try_from(max_chars.saturating_add(1)).map_err(|_| {
        integrity_error(
            "context_budget_overflow",
            "context budget exceeded SQLite bounds",
        )
    })?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT
                b.block_id,
                b.block_index,
                substr(b.location_path, 1, 4096),
                substr(b.text, 1, ?4)
            FROM blocks b
            WHERE b.doc_id = ?1
              AND b.block_index BETWEEN ?2 AND ?3
              AND length(b.block_id) BETWEEN 1 AND 8192
            ORDER BY b.block_index, b.block_id
            "#,
        )
        .map_err(map_sqlite_error)?;
    let mut rows = statement
        .query(params![doc_id, start, end, sqlite_text_limit])
        .map_err(map_sqlite_error)?;
    let mut context = String::new();
    let mut truncated = false;
    while let Some(row) = rows.next().map_err(map_sqlite_error)? {
        let block_id: String = row.get(0).map_err(map_sqlite_error)?;
        let current_index: i64 = row.get(1).map_err(map_sqlite_error)?;
        let location: Option<String> = row.get(2).map_err(map_sqlite_error)?;
        let text: String = row.get(3).map_err(map_sqlite_error)?;
        let separator = if context.is_empty() { "" } else { "\n\n" };
        let label = match location {
            Some(value) if !value.is_empty() => {
                format!("[{block_id}; index={current_index}; location={value}]\n")
            }
            _ => format!("[{block_id}; index={current_index}]\n"),
        };
        let used = context.chars().count();
        let available = max_chars.saturating_sub(used);
        let combined = format!("{separator}{label}{text}");
        let (part, part_truncated) = truncate_chars(&combined, available);
        context.push_str(&part);
        if part_truncated {
            truncated = true;
            break;
        }
    }
    if context.chars().count() >= max_chars {
        truncated = true;
    }
    Ok((context, truncated))
}

fn truncate_chars(input: &str, limit: usize) -> (String, bool) {
    let mut chars = input.chars();
    let output = chars.by_ref().take(limit).collect::<String>();
    let truncated = chars.next().is_some();
    (output, truncated)
}

#[allow(clippy::unnecessary_option_map_or_else)]
fn option_string_or_empty(value: Option<String>) -> String {
    value.map_or_else(String::new, |present| present)
}

fn validate_lookup_key(value: &str) -> Result<(), InformationError> {
    let count = value.chars().count();
    if count == 0 || count > MAX_ID_CHARS {
        return Err(input_error(
            "invalid_sqlite_lookup_key",
            "SQLite lookup key is empty or exceeds 8192 characters",
        ));
    }
    Ok(())
}

fn validate_row_identity(raw: &RawHit) -> Result<(), InformationError> {
    if raw.block_id.is_empty()
        || raw.doc_id.is_empty()
        || raw.block_id.chars().count() > MAX_ID_CHARS
        || raw.doc_id.chars().count() > MAX_ID_CHARS
        || raw.block_index < 0
        || raw.title.trim().is_empty()
        || !raw.score.is_finite()
    {
        return Err(integrity_error(
            "invalid_sqlite_evidence_row",
            "Alexandria row has invalid identifiers, metadata, or score",
        ));
    }
    Ok(())
}

fn normalize_sha256(digest: String) -> String {
    match digest.strip_prefix("sha256:") {
        Some(value) => value.to_ascii_lowercase(),
        None => digest.to_ascii_lowercase(),
    }
}

fn source_fingerprint_kind(fingerprint: &str) -> &'static str {
    if fingerprint.starts_with("sha256:") {
        "verified whole-file SHA-256"
    } else if fingerprint.starts_with("volatile-sqlite-snapshot-v1:") {
        "volatile per-operation SQLite snapshot identity; not a content digest"
    } else {
        "volatile live reference; not a content digest"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClassifiedDocumentRights {
    redistribution: RedistributionPolicy,
    license_url: Option<String>,
    obligations: Vec<String>,
}

fn classify_rights_status(status: &str) -> ClassifiedDocumentRights {
    let normalized = normalize_rights_status(status);
    let license_url = creative_commons_license_url(status);
    let redistribution = match normalized.as_str() {
        "private" | "private_donor_archive" | "private_use_only" => {
            RedistributionPolicy::PrivateUseOnly
        }
        "no_redistribution" | "redistribution_forbidden" | "all_rights_reserved" => {
            RedistributionPolicy::Forbidden
        }
        "public_domain"
        | "public_domain_hathitrust_anna_torrent"
        | "public_domain_project_gutenberg"
        | "public_domain_or_open_source_cache"
        | "public_domain_or_open_internet_archive"
        | "open_public_ia_text_file_not_marked_private"
        | "public_or_open_ia_text_open_public_ia_text_file_not_marked_private"
        | "public_or_open_ia_text_internet_archive_gutenberg_collection"
        | "public_or_open_ia_text_ia_rights_status_not_in_copyright"
        | "public_or_open_ia_text_ia_rights_status_this_book_is_not_in_copyright"
        | "public_or_open_ia_text_ia_rights_status_public_domain" => RedistributionPolicy::Allowed,
        value if is_publication_cutoff_status(value) => RedistributionPolicy::Allowed,
        value if is_creative_commons_public_domain_status(value) => RedistributionPolicy::Allowed,
        value if is_creative_commons_license_status(value) => {
            RedistributionPolicy::AllowedWithObligations
        }
        _ => RedistributionPolicy::Unknown,
    };
    let obligations = if redistribution == RedistributionPolicy::AllowedWithObligations {
        vec![
            "preserve the source license terms".to_string(),
            "provide source attribution when redistributing excerpts".to_string(),
        ]
    } else {
        Vec::new()
    };
    ClassifiedDocumentRights {
        redistribution,
        license_url,
        obligations,
    }
}

fn normalize_rights_status(status: &str) -> String {
    let mut normalized = String::with_capacity(status.len());
    let mut separator = false;
    for byte in status.trim().bytes() {
        if byte.is_ascii_alphanumeric() {
            normalized.push(char::from(byte.to_ascii_lowercase()));
            separator = false;
        } else if !separator && !normalized.is_empty() {
            normalized.push('_');
            separator = true;
        }
    }
    while normalized.ends_with('_') {
        normalized.pop();
    }
    normalized
}

fn is_publication_cutoff_status(normalized: &str) -> bool {
    normalized
        .strip_prefix("public_or_open_ia_text_published_")
        .and_then(|value| value.strip_suffix("_at_or_before_1930"))
        .is_some_and(|year| {
            (3..=4).contains(&year.len()) && year.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn ia_license_token(normalized: &str) -> Option<&str> {
    normalized.strip_prefix("public_or_open_ia_text_ia_license_")
}

fn is_creative_commons_public_domain_status(normalized: &str) -> bool {
    ia_license_token(normalized).is_some_and(|license| {
        ["http_", "https_"].iter().any(|scheme| {
            license.strip_prefix(scheme).is_some_and(|rest| {
                rest.starts_with("creativecommons_org_publicdomain_")
                    || rest.starts_with("creativecommons_org_licenses_publicdomain_")
            })
        })
    })
}

fn is_creative_commons_license_status(normalized: &str) -> bool {
    ia_license_token(normalized).is_some_and(|license| {
        ["http_", "https_"].iter().any(|scheme| {
            license
                .strip_prefix(scheme)
                .is_some_and(|rest| rest.starts_with("creativecommons_org_licenses_"))
        })
    })
}

fn creative_commons_license_url(status: &str) -> Option<String> {
    let (_, candidate) = status.split_once("ia_license:")?;
    let parsed = Url::parse(candidate.trim()).ok()?;
    if matches!(parsed.scheme(), "http" | "https")
        && parsed
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("creativecommons.org"))
    {
        Some(parsed.to_string())
    } else {
        None
    }
}

fn intersect_rights_into_use_policy(
    mut policy: UsePolicy,
    rights: &[RightsStatement],
) -> UsePolicy {
    for statement in rights {
        let rights_permission = match statement.redistribution {
            RedistributionPolicy::Allowed | RedistributionPolicy::AllowedWithObligations => {
                UsePermission::Allowed
            }
            RedistributionPolicy::PrivateUseOnly | RedistributionPolicy::Forbidden => {
                UsePermission::Forbidden
            }
            RedistributionPolicy::Unknown => UsePermission::Unknown,
        };
        policy.excerpt_export = intersect_use_permission(policy.excerpt_export, rights_permission);
        policy.redistribution = intersect_use_permission(policy.redistribution, rights_permission);
        if statement.redistribution == RedistributionPolicy::AllowedWithObligations {
            policy.attribution_required = true;
        }
    }
    policy
}

fn intersect_use_permission(left: UsePermission, right: UsePermission) -> UsePermission {
    match (left, right) {
        (UsePermission::Forbidden, _) | (_, UsePermission::Forbidden) => UsePermission::Forbidden,
        (UsePermission::Allowed, UsePermission::Allowed) => UsePermission::Allowed,
        _ => UsePermission::Unknown,
    }
}

fn access_mode_name(mode: ExternalAccessMode) -> &'static str {
    match mode {
        ExternalAccessMode::LiveReadOnly => "live_read_only",
        ExternalAccessMode::ImmutableReadOnly => "immutable_read_only",
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).map_or(u64::MAX, |value| value)
}

fn safe_sqlite_summary(error: &rusqlite::Error) -> &'static str {
    match error {
        rusqlite::Error::SqliteFailure(code, _) if code.code == ErrorCode::OperationInterrupted => {
            "query deadline elapsed"
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) =>
        {
            "database is busy"
        }
        _ => "invalid FTS syntax or backend query failure",
    }
}

fn map_sqlite_error(error: rusqlite::Error) -> InformationError {
    match &error {
        rusqlite::Error::SqliteFailure(code, _) if code.code == ErrorCode::OperationInterrupted => {
            InformationError::new(
                ErrorClass::ResourceBusy,
                "sqlite_query_timeout",
                "SQLite query deadline elapsed",
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
                "sqlite_resource_busy",
                "SQLite source is busy",
            )
            .retryable(true)
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(code.code, ErrorCode::PermissionDenied | ErrorCode::ReadOnly) =>
        {
            InformationError::new(
                ErrorClass::Permission,
                "sqlite_permission_denied",
                "SQLite denied the read-only operation",
            )
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            ) =>
        {
            integrity_error(
                "sqlite_source_invalid",
                "SQLite source is corrupt or not a database",
            )
        }
        _ => InformationError::new(
            ErrorClass::Backend,
            "sqlite_backend_error",
            format!("SQLite backend operation failed: {error}"),
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
        QUERY_SCHEMA, QueryBudget, QueryFilters, QueryId, RetrievalTarget, UsePermission,
    };
    use std::error::Error;
    use std::io::Write;
    use std::sync::Arc;

    const FIXTURE_FINGERPRINT: &str = "volatile-sqlite-snapshot-v1:fixture-operation";

    fn fixture_ids() -> Result<(ResourceId, ReleaseId, RepresentationId), Box<dyn Error>> {
        Ok((
            ResourceId::parse("fixture-resource")?,
            ReleaseId::parse("fixture-release")?,
            RepresentationId::parse("fixture-representation")?,
        ))
    }

    fn fixture_config(path: impl Into<PathBuf>) -> Result<AlexandriaBackendConfig, Box<dyn Error>> {
        let (resource, release, representation) = fixture_ids()?;
        Ok(AlexandriaBackendConfig::new(
            "fixture-sqlite",
            "Fixture Alexandria",
            resource,
            release,
            representation,
            path,
            ExternalAccessMode::LiveReadOnly,
            "Fixture Publisher",
        ))
    }

    fn fixture_query(text: &str, syntax: QuerySyntax) -> Result<InformationQuery, Box<dyn Error>> {
        let (resource_id, release_id, representation_id) = fixture_ids()?;
        Ok(InformationQuery {
            schema: QUERY_SCHEMA.to_string(),
            query_id: QueryId::new(),
            text: text.to_string(),
            syntax,
            purpose: RetrievalPurpose::LocalUi,
            targets: vec![RetrievalTarget {
                resource_id,
                release_id,
                representation_id,
            }],
            resources: Vec::new(),
            representations: Vec::new(),
            filters: QueryFilters::default(),
            budget: QueryBudget {
                max_hits: 5,
                max_hits_per_backend: 5,
                max_backends: 1,
                max_context_chars: 1_500,
                timeout_ms: 5_000,
            },
        })
    }

    fn create_fixture(connection: &Connection) -> rusqlite::Result<()> {
        connection.execute_batch(
            r#"
            CREATE TABLE documents (
                doc_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                author_normalized TEXT,
                author_attributed TEXT,
                tradition_tag TEXT NOT NULL,
                date_range TEXT,
                language_original TEXT,
                language_translation TEXT,
                translator TEXT,
                editor TEXT,
                edition TEXT,
                source_uri TEXT NOT NULL,
                rights_status TEXT NOT NULL,
                genre TEXT,
                file_ext TEXT NOT NULL,
                ingest_status TEXT NOT NULL,
                block_count INTEGER NOT NULL,
                text_chars INTEGER NOT NULL,
                canonical_path TEXT
            );
            CREATE TABLE blocks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                block_id TEXT UNIQUE NOT NULL,
                doc_id TEXT NOT NULL REFERENCES documents(doc_id),
                block_index INTEGER NOT NULL,
                block_type TEXT NOT NULL,
                text TEXT NOT NULL,
                char_start INTEGER,
                char_end INTEGER,
                location_path TEXT
            );
            CREATE INDEX idx_blocks_doc_idx ON blocks(doc_id, block_index);
            CREATE VIRTUAL TABLE blocks_fts USING fts5(
                block_id UNINDEXED,
                doc_id UNINDEXED,
                text,
                tokenize='unicode61'
            );
            CREATE TABLE block_theme_hits (
                hit_id INTEGER PRIMARY KEY AUTOINCREMENT,
                doc_id TEXT NOT NULL,
                block_id TEXT NOT NULL,
                theme_tag TEXT NOT NULL,
                matched_term TEXT NOT NULL,
                controversy_risk TEXT NOT NULL
            );
            CREATE INDEX idx_theme_hits_block ON block_theme_hits(block_id);
            INSERT INTO documents (
                doc_id, title, author_normalized, tradition_tag,
                language_translation, source_uri, rights_status, genre,
                file_ext, ingest_status, block_count, text_chars, canonical_path
            ) VALUES (
                'D1', 'Fixture Treatise', 'A. Mystic', 'Catholic', 'English',
                'fixture://treatise', 'public_domain', 'spirituality', 'txt',
                'ok', 3, 180, 'fixture/treatise.txt'
            );
            INSERT INTO blocks (block_id, doc_id, block_index, block_type, text, location_path)
            VALUES
                ('D1:B000001', 'D1', 1, 'paragraph', 'The soul recollects before God in humble prayer.', 'chapter 1'),
                ('D1:B000002', 'D1', 2, 'paragraph', 'The prayer of quiet gathers the powers for contemplation.', 'chapter 1'),
                ('D1:B000003', 'D1', 3, 'paragraph', 'Charity is the form of union.', 'chapter 1');
            INSERT INTO blocks_fts (block_id, doc_id, text)
                SELECT block_id, doc_id, text FROM blocks;
            INSERT INTO block_theme_hits (doc_id, block_id, theme_tag, matched_term, controversy_risk)
            VALUES ('D1', 'D1:B000002', 'contemplation', 'prayer of quiet', 'low');
            "#,
        )
    }

    fn create_file_fixture(path: &Path) -> Result<(), Box<dyn Error>> {
        let connection = Connection::open(path)?;
        create_fixture(&connection)?;
        drop(connection);
        Ok(())
    }

    fn file_sha256(path: &Path) -> Result<String, Box<dyn Error>> {
        let mut hasher = Sha256::new();
        hasher.update(fs::read(path)?);
        Ok(hex::encode(hasher.finalize()))
    }

    fn memory_backend() -> Result<AlexandriaBackend, Box<dyn Error>> {
        let config = fixture_config("fixture-memory.db")?;
        Ok(AlexandriaBackend {
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
                ]),
                use_policy: config.use_policy,
            },
            path: PathBuf::from("fixture-memory.db"),
            access_mode: ExternalAccessMode::LiveReadOnly,
            publisher: config.publisher,
            immutable_source_sha256: None,
            rights: Vec::new(),
            use_policy: config.use_policy,
            context_radius: 1,
            max_snippet_chars: 700,
            initial_identity: FileIdentity {
                bytes: 1,
                modified_unix_nanos: Some(1),
                #[cfg(unix)]
                device: 1,
                #[cfg(unix)]
                inode: 1,
                #[cfg(unix)]
                changed_unix_seconds: 1,
                #[cfg(unix)]
                changed_subsec_nanos: 1,
            },
            instance_id: 1,
            operation_sequence: AtomicU64::new(1),
        })
    }

    #[test]
    fn in_memory_fixture_searches_with_stable_citations_and_budget() -> Result<(), Box<dyn Error>> {
        let connection = Connection::open_in_memory()?;
        create_fixture(&connection)?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "query_only", true)?;
        validate_alexandria_schema(&connection)?;
        let backend = memory_backend()?;
        let query = fixture_query("prayer quiet", QuerySyntax::NaturalTerms)?;
        let result = backend.search_connection(&connection, FIXTURE_FINGERPRINT, &query)?;
        assert!(!result.hits.is_empty());
        let hit = result
            .hits
            .first()
            .ok_or_else(|| std::io::Error::other("fixture evidence was unexpectedly empty"))?;
        assert_eq!(hit.passage_id.as_deref(), Some("D1:B000002"));
        assert_eq!(hit.use_policy, UsePolicy::default());
        assert_eq!(
            hit.excerpt_sha256,
            evidence_text_sha256(&hit.snippet, &hit.context)
        );
        hit.validate()?;
        assert!(hit.context.chars().count() + hit.snippet.chars().count() <= 1_500);
        assert!(matches!(hit.locator, EvidenceLocator::SqliteBlock { .. }));
        Ok(())
    }

    #[test]
    fn backend_native_syntax_failure_uses_escaped_fallback() -> Result<(), Box<dyn Error>> {
        let connection = Connection::open_in_memory()?;
        create_fixture(&connection)?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "query_only", true)?;
        let backend = memory_backend()?;
        let query = fixture_query("quiet OR (", QuerySyntax::BackendNative)?;
        let result = backend.search_connection(&connection, FIXTURE_FINGERPRINT, &query)?;
        assert_eq!(result.hits.len(), 1);
        assert!(!result.complete);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("candidate failed"))
        );
        Ok(())
    }

    #[test]
    fn field_filters_are_bound_values_not_sql() -> Result<(), Box<dyn Error>> {
        let connection = Connection::open_in_memory()?;
        create_fixture(&connection)?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "query_only", true)?;
        let backend = memory_backend()?;
        let mut query = fixture_query("prayer", QuerySyntax::AnyTerms)?;
        query.filters.fields.insert(
            "tradition_tag".to_string(),
            "Catholic' OR 1=1 --".to_string(),
        );
        let result = backend.search_connection(&connection, FIXTURE_FINGERPRINT, &query)?;
        assert!(result.hits.is_empty());
        Ok(())
    }

    #[test]
    fn block_read_and_lookup_preserve_locator_and_hash() -> Result<(), Box<dyn Error>> {
        let connection = Connection::open_in_memory()?;
        create_fixture(&connection)?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "query_only", true)?;
        let backend = memory_backend()?;
        let (resource_id, release_id, representation_id) = fixture_ids()?;
        let read = backend.read_connection(
            &connection,
            FIXTURE_FINGERPRINT,
            &ReadRequest {
                resource_id: resource_id.clone(),
                release_id: release_id.clone(),
                representation_id: representation_id.clone(),
                purpose: RetrievalPurpose::LocalUi,
                locator: EvidenceLocator::SqliteBlock {
                    block_id: "D1:B000002".to_string(),
                    doc_id: "D1".to_string(),
                    block_index: 2,
                    location_path: Some("chapter 1".to_string()),
                },
                max_context_chars: 1_000,
                timeout_ms: 5_000,
            },
        )?;
        assert_eq!(
            read.hit.excerpt_sha256,
            evidence_text_sha256(&read.hit.snippet, &read.hit.context)
        );
        read.hit.validate()?;
        assert_eq!(read.hit.passage_id.as_deref(), Some("D1:B000002"));

        let lookup = backend.lookup_connection(
            &connection,
            FIXTURE_FINGERPRINT,
            &LookupRequest {
                resource_id,
                release_id,
                representation_id,
                purpose: RetrievalPurpose::LocalUi,
                collection: Some("blocks".to_string()),
                key: "D1:B000002".to_string(),
                max_context_chars: 1_000,
                timeout_ms: 5_000,
            },
        )?;
        assert_eq!(lookup.hit.locator, read.hit.locator);
        assert_eq!(lookup.hit.excerpt_sha256, read.hit.excerpt_sha256);
        lookup.hit.validate()?;
        Ok(())
    }

    #[test]
    fn private_document_rights_forbid_hit_redistribution() -> Result<(), Box<dyn Error>> {
        let connection = Connection::open_in_memory()?;
        create_fixture(&connection)?;
        connection.execute(
            "UPDATE documents SET rights_status = ?1 WHERE doc_id = ?2",
            params!["private donor archive", "D1"],
        )?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "query_only", true)?;
        let backend = memory_backend()?;
        let query = fixture_query("prayer quiet", QuerySyntax::NaturalTerms)?;
        let result = backend.search_connection(&connection, FIXTURE_FINGERPRINT, &query)?;
        let hit = result.hits.first().ok_or_else(|| {
            std::io::Error::other("fixture query did not return private document evidence")
        })?;
        assert_eq!(hit.use_policy.redistribution, UsePermission::Forbidden);
        assert!(hit.rights.iter().any(|statement| {
            statement.scope == "document:D1"
                && statement.expression == "private donor archive"
                && statement.redistribution == RedistributionPolicy::PrivateUseOnly
        }));
        Ok(())
    }

    #[test]
    fn exact_public_status_with_private_word_remains_exportable() -> Result<(), Box<dyn Error>> {
        let connection = Connection::open_in_memory()?;
        create_fixture(&connection)?;
        connection.execute(
            "UPDATE documents SET rights_status = ?1 WHERE doc_id = ?2",
            params!["open_public_ia_text_file_not_marked_private", "D1"],
        )?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "query_only", true)?;
        let mut backend = memory_backend()?;
        backend.use_policy.excerpt_export = UsePermission::Allowed;
        backend.use_policy.redistribution = UsePermission::Allowed;
        backend.descriptor.use_policy = backend.use_policy;
        let mut query = fixture_query("prayer quiet", QuerySyntax::NaturalTerms)?;
        query.purpose = RetrievalPurpose::ExcerptExport;
        let result = backend.search_connection(&connection, FIXTURE_FINGERPRINT, &query)?;
        let hit = result.hits.first().ok_or_else(|| {
            std::io::Error::other("fixture query did not return public document evidence")
        })?;
        assert_eq!(hit.use_policy.excerpt_export, UsePermission::Allowed);
        assert!(hit.rights.iter().any(|statement| {
            statement.expression == "open_public_ia_text_file_not_marked_private"
                && statement.redistribution == RedistributionPolicy::Allowed
        }));
        hit.validate()?;
        Ok(())
    }

    #[test]
    fn unknown_document_rights_fail_closed_for_export() -> Result<(), Box<dyn Error>> {
        let connection = Connection::open_in_memory()?;
        create_fixture(&connection)?;
        connection.execute(
            "UPDATE documents SET rights_status = ?1 WHERE doc_id = ?2",
            params!["custom status not in the vocabulary", "D1"],
        )?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "query_only", true)?;
        let mut backend = memory_backend()?;
        backend.use_policy.excerpt_export = UsePermission::Allowed;
        backend.descriptor.use_policy = backend.use_policy;
        let mut query = fixture_query("prayer quiet", QuerySyntax::NaturalTerms)?;
        query.purpose = RetrievalPurpose::ExcerptExport;
        let error = backend
            .search_connection(&connection, FIXTURE_FINGERPRINT, &query)
            .err()
            .ok_or_else(|| std::io::Error::other("unknown rights unexpectedly allowed export"))?;
        assert_eq!(error.class, ErrorClass::Permission);
        assert_eq!(error.code, "retrieval_purpose_not_permitted");
        Ok(())
    }

    #[test]
    fn corpus_rights_restrictions_are_intersected_into_backend_policy() -> Result<(), Box<dyn Error>>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.db");
        create_file_fixture(&path)?;
        let mut config = fixture_config(&path)?;
        config.use_policy.excerpt_export = UsePermission::Allowed;
        config.use_policy.redistribution = UsePermission::Allowed;
        config.rights.push(RightsStatement {
            scope: "corpus".to_string(),
            expression: "all_rights_reserved".to_string(),
            license_url: None,
            license_text_sha256: None,
            attribution: Some("Fixture Publisher".to_string()),
            obligations: Vec::new(),
            redistribution: RedistributionPolicy::Forbidden,
        });
        let backend = AlexandriaBackend::open(config)?;
        assert_eq!(
            backend.descriptor().use_policy.excerpt_export,
            UsePermission::Forbidden
        );
        assert_eq!(
            backend.descriptor().use_policy.redistribution,
            UsePermission::Forbidden
        );
        Ok(())
    }

    #[test]
    fn direct_backend_calls_fail_closed_for_unknown_purpose_permission()
    -> Result<(), Box<dyn Error>> {
        let connection = Connection::open_in_memory()?;
        create_fixture(&connection)?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "query_only", true)?;
        let backend = memory_backend()?;
        let mut query = fixture_query("prayer", QuerySyntax::AnyTerms)?;
        query.purpose = RetrievalPurpose::ModelContext;
        let search_error = backend
            .search_connection(&connection, FIXTURE_FINGERPRINT, &query)
            .err()
            .ok_or_else(|| {
                std::io::Error::other("direct model-context search unexpectedly succeeded")
            })?;
        assert_eq!(search_error.code, "retrieval_purpose_not_permitted");

        let (resource_id, release_id, representation_id) = fixture_ids()?;
        let read_error = backend
            .read_connection(
                &connection,
                FIXTURE_FINGERPRINT,
                &ReadRequest {
                    resource_id: resource_id.clone(),
                    release_id: release_id.clone(),
                    representation_id: representation_id.clone(),
                    purpose: RetrievalPurpose::ModelContext,
                    locator: EvidenceLocator::SqliteBlock {
                        block_id: "D1:B000002".to_string(),
                        doc_id: "D1".to_string(),
                        block_index: 2,
                        location_path: None,
                    },
                    max_context_chars: 1_000,
                    timeout_ms: 5_000,
                },
            )
            .err()
            .ok_or_else(|| {
                std::io::Error::other("direct model-context read unexpectedly succeeded")
            })?;
        assert_eq!(read_error.code, "retrieval_purpose_not_permitted");

        let lookup_error = backend
            .lookup_connection(
                &connection,
                FIXTURE_FINGERPRINT,
                &LookupRequest {
                    resource_id,
                    release_id,
                    representation_id,
                    purpose: RetrievalPurpose::ModelContext,
                    collection: Some("blocks".to_string()),
                    key: "D1:B000002".to_string(),
                    max_context_chars: 1_000,
                    timeout_ms: 5_000,
                },
            )
            .err()
            .ok_or_else(|| {
                std::io::Error::other("direct model-context lookup unexpectedly succeeded")
            })?;
        assert_eq!(lookup_error.code, "retrieval_purpose_not_permitted");
        Ok(())
    }

    #[test]
    fn alexandria_filter_caps_apply_before_sql_preparation() -> Result<(), Box<dyn Error>> {
        let connection = Connection::open_in_memory()?;
        create_fixture(&connection)?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "query_only", true)?;
        let backend = memory_backend()?;

        let mut query = fixture_query("prayer", QuerySyntax::AnyTerms)?;
        query.filters.document_ids = (0..=MAX_FILTER_VALUES_PER_LIST)
            .map(|index| format!("document-{index}"))
            .collect();
        let value_cap_error = backend
            .search_connection(&connection, FIXTURE_FINGERPRINT, &query)
            .err()
            .ok_or_else(|| std::io::Error::other("oversized filter unexpectedly succeeded"))?;
        assert_eq!(value_cap_error.code, "sqlite_filter_value_limit_exceeded");

        let mut query = fixture_query("prayer", QuerySyntax::AnyTerms)?;
        query.filters.document_ids = vec!["D1".to_string(); MAX_FILTER_VALUES_PER_LIST];
        query.filters.languages = vec!["English".to_string(); MAX_FILTER_VALUES_PER_LIST];
        query.filters.subjects = vec!["prayer".to_string(); MAX_FILTER_VALUES_PER_LIST];
        query
            .filters
            .fields
            .insert("author".to_string(), "A. Mystic".to_string());
        let bind_cap_error = backend
            .search_connection(&connection, FIXTURE_FINGERPRINT, &query)
            .err()
            .ok_or_else(|| std::io::Error::other("excessive binds unexpectedly succeeded"))?;
        assert_eq!(bind_cap_error.code, "sqlite_filter_bind_limit_exceeded");
        Ok(())
    }

    #[test]
    fn live_mode_rejects_static_digest_claims() -> Result<(), Box<dyn Error>> {
        let mut config = fixture_config("source-does-not-need-to-exist.db")?;
        config.verified_source_sha256 = Some("00".repeat(32));
        let error = AlexandriaBackend::open(config).err().ok_or_else(|| {
            std::io::Error::other("live backend unexpectedly accepted a static digest")
        })?;
        assert_eq!(error.code, "live_sqlite_static_digest_forbidden");
        Ok(())
    }

    #[test]
    fn immutable_mode_verifies_and_binds_whole_file_digest() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.db");
        create_file_fixture(&path)?;
        let expected = file_sha256(&path)?;

        let mut wrong_config = fixture_config(&path)?;
        wrong_config.access_mode = ExternalAccessMode::ImmutableReadOnly;
        wrong_config.verified_source_sha256 = Some("00".repeat(32));
        let error = AlexandriaBackend::open(wrong_config)
            .err()
            .ok_or_else(|| std::io::Error::other("wrong immutable digest unexpectedly matched"))?;
        assert_eq!(error.code, "immutable_sqlite_digest_mismatch");

        let mut stale_identity = FileIdentity::observe(&path)?.source_identity();
        stale_identity.sha256 = Some(format!("sha256:{expected}"));
        stale_identity.bytes = stale_identity.bytes.saturating_add(1);
        let mut stale_config = fixture_config(&path)?;
        stale_config.access_mode = ExternalAccessMode::ImmutableReadOnly;
        stale_config.verified_source_identity = Some(stale_identity);
        let error = AlexandriaBackend::open(stale_config).err().ok_or_else(|| {
            std::io::Error::other("stale immutable identity unexpectedly matched")
        })?;
        assert_eq!(error.code, "verified_sqlite_identity_mismatch");

        let mut verified_identity = FileIdentity::observe(&path)?.source_identity();
        verified_identity.sha256 = Some(format!("sha256:{expected}"));
        let mut config = fixture_config(&path)?;
        config.access_mode = ExternalAccessMode::ImmutableReadOnly;
        config.verified_source_identity = Some(verified_identity);
        let backend = AlexandriaBackend::open(config)?;
        assert_eq!(
            backend.source_identity().sha256.as_deref(),
            Some(format!("sha256:{expected}").as_str())
        );
        assert_eq!(backend.source_fingerprint(), format!("sha256:{expected}"));
        backend.source_identity().validate()?;
        Ok(())
    }

    #[test]
    fn every_mode_rejects_nonempty_wal() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.db");
        let connection = Connection::open(&path)?;
        create_fixture(&connection)?;
        drop(connection);
        let wal = sibling_wal_path(&path)?;
        fs::write(wal, b"not empty")?;
        for access_mode in [
            ExternalAccessMode::LiveReadOnly,
            ExternalAccessMode::ImmutableReadOnly,
        ] {
            let mut config = fixture_config(&path)?;
            config.access_mode = access_mode;
            let error = AlexandriaBackend::open(config).err().ok_or_else(|| {
                std::io::Error::other("snapshot backend unexpectedly accepted a non-empty WAL")
            })?;
            assert_eq!(error.code, "sqlite_snapshot_nonempty_wal");
        }
        Ok(())
    }

    #[test]
    fn live_mode_rejects_pending_wal_instead_of_writing_sidecars() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.db");
        let writer = Connection::open(&path)?;
        create_fixture(&writer)?;
        writer.pragma_update(None, "journal_mode", "WAL")?;
        writer.pragma_update(None, "wal_autocheckpoint", 0)?;
        writer.execute(
            "UPDATE documents SET edition = ?1 WHERE doc_id = ?2",
            params!["live", "D1"],
        )?;
        let wal = sibling_wal_path(&path)?;
        assert!(fs::metadata(&wal)?.len() > 0);
        let error = AlexandriaBackend::open(fixture_config(&path)?)
            .err()
            .ok_or_else(|| std::io::Error::other("live backend accepted a pending WAL"))?;
        assert_eq!(error.code, "sqlite_snapshot_nonempty_wal");
        drop(writer);
        Ok(())
    }

    #[test]
    fn live_mode_creates_no_sqlite_sidecars() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.db");
        create_file_fixture(&path)?;
        let wal = sibling_sidecar_path(&path, "-wal")?;
        let shm = sibling_sidecar_path(&path, "-shm")?;
        let journal = sibling_sidecar_path(&path, "-journal")?;
        assert!(!wal.exists() && !shm.exists() && !journal.exists());
        let backend = AlexandriaBackend::open(fixture_config(&path)?)?;
        let result = backend.search(&fixture_query("prayer", QuerySyntax::AnyTerms)?)?;
        assert!(!result.hits.is_empty());
        assert!(!wal.exists() && !shm.exists() && !journal.exists());
        Ok(())
    }

    #[test]
    fn live_mode_uses_distinct_volatile_snapshot_fingerprints() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.db");
        create_file_fixture(&path)?;
        let backend = AlexandriaBackend::open(fixture_config(&path)?)?;
        let first_reference = backend.source_fingerprint();
        let second_reference = backend.source_fingerprint();
        assert!(first_reference.starts_with("volatile-live-reference-v1:"));
        assert_ne!(first_reference, second_reference);

        let query = fixture_query("prayer", QuerySyntax::AnyTerms)?;
        let first = backend.search(&query)?;
        let second = backend.search(&query)?;
        let first_fingerprint = first
            .hits
            .first()
            .and_then(|hit| hit.source_fingerprint.as_deref())
            .ok_or_else(|| std::io::Error::other("first search had no fingerprint"))?;
        let second_fingerprint = second
            .hits
            .first()
            .and_then(|hit| hit.source_fingerprint.as_deref())
            .ok_or_else(|| std::io::Error::other("second search had no fingerprint"))?;
        assert!(first_fingerprint.starts_with("volatile-sqlite-snapshot-v1:"));
        assert_ne!(first_fingerprint, second_fingerprint);
        Ok(())
    }

    #[test]
    fn live_mode_rejects_non_file_sidecars() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.db");
        create_file_fixture(&path)?;
        fs::create_dir(sibling_sidecar_path(&path, "-journal")?)?;
        let error = AlexandriaBackend::open(fixture_config(&path)?)
            .err()
            .ok_or_else(|| std::io::Error::other("non-file sidecar unexpectedly accepted"))?;
        assert_eq!(error.code, "sqlite_sidecar_not_file");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn live_mode_rejects_symlink_sidecars() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.db");
        create_file_fixture(&path)?;
        let target = directory.path().join("other-wal");
        fs::write(&target, b"not a SQLite WAL")?;
        symlink(&target, sibling_wal_path(&path)?)?;
        let error = AlexandriaBackend::open(fixture_config(&path)?)
            .err()
            .ok_or_else(|| std::io::Error::other("symlink sidecar unexpectedly accepted"))?;
        assert_eq!(error.code, "sqlite_sidecar_is_symlink");
        Ok(())
    }

    #[test]
    fn live_mode_discards_results_if_main_file_identity_changes() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.db");
        let writer = Connection::open(&path)?;
        create_fixture(&writer)?;
        drop(writer);
        let backend = AlexandriaBackend::open(fixture_config(&path)?)?;
        let error = backend
            .with_connection(5_000, |_connection, _identity| {
                let mut file = fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .map_err(|io| io_error("fixture_append_failed", io.to_string()))?;
                file.write_all(&[0])
                    .map_err(|io| io_error("fixture_append_failed", io.to_string()))?;
                Ok(())
            })
            .err()
            .ok_or_else(|| {
                std::io::Error::other("identity-changing operation unexpectedly succeeded")
            })?;
        assert_eq!(error.code, "sqlite_identity_changed_during_read");
        Ok(())
    }

    #[test]
    fn forbidden_local_search_is_a_permission_error() -> Result<(), Box<dyn Error>> {
        let mut backend = memory_backend()?;
        backend.use_policy.local_search = UsePermission::Forbidden;
        backend.descriptor.use_policy.local_search = UsePermission::Forbidden;
        let query = fixture_query("prayer", QuerySyntax::AnyTerms)?;
        let error = backend
            .search(&query)
            .err()
            .ok_or_else(|| std::io::Error::other("forbidden backend unexpectedly searched"))?;
        assert_eq!(error.class, ErrorClass::Permission);
        assert_eq!(error.code, "retrieval_purpose_not_permitted");
        Ok(())
    }

    #[test]
    #[ignore = "set INFORMATION_NATIVE_ALEXANDRIA_DB to run against a real compatible corpus"]
    fn real_alexandria_smoke() -> Result<(), Box<dyn Error>> {
        let Some(path) = std::env::var_os("INFORMATION_NATIVE_ALEXANDRIA_DB") else {
            return Ok(());
        };
        let config = fixture_config(PathBuf::from(path))?;
        let backend = Arc::new(AlexandriaBackend::open(config)?);
        let query = fixture_query("prayer of quiet", QuerySyntax::NaturalTerms)?;
        let result = backend.search(&query)?;
        assert!(!result.hits.is_empty());
        Ok(())
    }
}
