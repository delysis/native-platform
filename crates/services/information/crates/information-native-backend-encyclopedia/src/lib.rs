#![forbid(unsafe_code)]

//! Strict read-only retrieval for the compiled local encyclopedia profile.
//!
//! This adapter accepts one exact `articles` schema and one exact external-
//! content `articles_fts` definition. SQL identifiers are compiled into this
//! crate. Queries bind all caller-controlled values. Every operation uses a
//! immutable-transport read transaction, a SQLite progress deadline, and
//! before/after source identity checks. Live mode permits the main file to
//! change between operations but never consumes pending WAL/journal state.
//! Retrieved text is untrusted evidence.

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
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

/// Compiled schema/profile identifier emitted in evidence metadata.
pub const ENCYCLOPEDIA_PROFILE_NAME: &str = "encyclopedia.articles.v1";
/// Stable collection used by every article [`EvidenceLocator::Record`].
pub const ENCYCLOPEDIA_ARTICLE_COLLECTION: &str = "articles";

const PROFILE_NAME: &str = ENCYCLOPEDIA_PROFILE_NAME;
const ARTICLE_COLLECTION: &str = ENCYCLOPEDIA_ARTICLE_COLLECTION;
const MAX_SNIPPET_CHARS: u32 = 4_096;
const MAX_QUERY_TERMS: usize = 64;
const MAX_FILTER_VALUES_PER_LIST: usize = 128;
const MAX_FIELD_FILTERS: usize = 8;
const MAX_SQLITE_BINDS: usize = 512;
const MAX_ID_CHARS: usize = 32;
const MAX_ORIGIN_CHARS: usize = 512;
const MAX_TITLE_CHARS: usize = 2_048;
const MAX_SHORT_METADATA_CHARS: usize = 1_024;
const MAX_AUTHORS_CHARS: usize = 4_096;
const MAX_SOURCE_CONTAINER_CHARS: usize = 8_192;
const MAX_LONG_METADATA_CHARS: usize = 16_384;
const SEARCH_POLICY_OVERSAMPLE_FACTOR: u32 = 4;
const MAX_SEARCH_POLICY_CANDIDATES: u32 = 4_000;

static NEXT_BACKEND_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

/// Configuration for one mounted encyclopedia database.
#[derive(Debug, Clone)]
pub struct EncyclopediaBackendConfig {
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
    /// Optional registration identity bound to a verified digest.
    pub verified_source_identity: Option<SourceIdentity>,
    /// Source-wide statements applied in addition to per-origin rights.
    pub rights: Vec<RightsStatement>,
    /// Source-wide ceiling intersected with the mandatory per-origin policy.
    pub use_policy: UsePolicy,
    pub max_snippet_chars: u32,
}

impl EncyclopediaBackendConfig {
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
            use_policy: UsePolicy {
                local_search: UsePermission::Allowed,
                model_context: UsePermission::Forbidden,
                excerpt_export: UsePermission::Forbidden,
                redistribution: UsePermission::Forbidden,
                attribution_required: false,
            },
            max_snippet_chars: 700,
        }
    }

    fn validate(&self) -> Result<(), InformationError> {
        if self.backend_id.trim().is_empty()
            || self.label.trim().is_empty()
            || self.publisher.trim().is_empty()
        {
            return Err(input_error(
                "invalid_encyclopedia_backend_config",
                "backend id, label, and publisher must not be empty",
            ));
        }
        if self.max_snippet_chars == 0 || self.max_snippet_chars > MAX_SNIPPET_CHARS {
            return Err(input_error(
                "invalid_encyclopedia_backend_config",
                "snippet budget is outside the supported bounds",
            ));
        }
        if let Some(digest) = &self.verified_source_sha256 {
            validate_sha256(digest)?;
            if self.access_mode == ExternalAccessMode::LiveReadOnly {
                return Err(input_error(
                    "live_encyclopedia_static_digest_forbidden",
                    "live encyclopedia sources cannot advertise a static whole-file digest",
                ));
            }
        }
        if let Some(identity) = &self.verified_source_identity {
            identity.validate().map_err(|error| {
                input_error(
                    "invalid_verified_source_identity",
                    format!("verified encyclopedia source identity is invalid: {error}"),
                )
            })?;
            if identity.sha256.is_none() || identity.modified_unix_nanos.is_none() {
                return Err(input_error(
                    "incomplete_verified_source_identity",
                    "verified source identity requires a digest and modification timestamp",
                ));
            }
            #[cfg(unix)]
            if identity.device.is_none() || identity.inode.is_none() {
                return Err(input_error(
                    "incomplete_verified_source_identity",
                    "verified source identity requires device and inode on this platform",
                ));
            }
            if self.access_mode == ExternalAccessMode::LiveReadOnly {
                return Err(input_error(
                    "live_encyclopedia_static_identity_forbidden",
                    "live encyclopedia sources cannot advertise static verified identity",
                ));
            }
            if self
                .verified_source_sha256
                .as_ref()
                .zip(identity.sha256.as_ref())
                .is_some_and(|(left, right)| normalize_sha256(left) != normalize_sha256(right))
            {
                return Err(input_error(
                    "conflicting_verified_source_digest",
                    "verified source digest fields disagree",
                ));
            }
        }
        for statement in &self.rights {
            statement.validate().map_err(|error| {
                input_error(
                    "invalid_encyclopedia_rights_statement",
                    format!("encyclopedia rights statement is invalid: {error}"),
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
                "encyclopedia_source_metadata_failed",
                format!("cannot inspect encyclopedia source metadata: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(integrity_error(
                "encyclopedia_source_is_symlink",
                "canonical encyclopedia source unexpectedly resolves to a symbolic link",
            ));
        }
        Self::from_metadata(&metadata)
    }

    fn from_metadata(metadata: &Metadata) -> Result<Self, InformationError> {
        if !metadata.is_file() {
            return Err(input_error(
                "encyclopedia_source_not_file",
                "encyclopedia source path is not a regular file",
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
        .map(|duration| duration.as_nanos())
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
                "encyclopedia_sidecar_is_symlink",
                format!("encyclopedia {label} sidecar must not be a symbolic link"),
            )),
            Ok(metadata) if !metadata.is_file() => Err(integrity_error(
                "encyclopedia_sidecar_not_file",
                format!("encyclopedia {label} sidecar must be a regular file"),
            )),
            Ok(metadata) => Ok(Self::Present(FileIdentity::from_metadata(&metadata)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::Absent),
            Err(error) => Err(io_error(
                "encyclopedia_sidecar_metadata_failed",
                format!("cannot inspect encyclopedia {label} sidecar metadata: {error}"),
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
            wal: SidecarIdentity::observe(&sidecar_path(path, "-wal")?, "WAL")?,
            shm: SidecarIdentity::observe(&sidecar_path(path, "-shm")?, "SHM")?,
            journal: SidecarIdentity::observe(
                &sidecar_path(path, "-journal")?,
                "rollback journal",
            )?,
        })
    }

    fn ensure_no_pending_writes(&self) -> Result<(), InformationError> {
        if self.wal.is_nonempty() {
            return Err(integrity_error(
                "encyclopedia_nonempty_wal",
                "encyclopedia reads reject a non-empty sibling WAL",
            ));
        }
        if self.journal.is_nonempty() {
            return Err(integrity_error(
                "encyclopedia_nonempty_journal",
                "encyclopedia reads reject a non-empty sibling rollback journal",
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

/// A mounted, compiled encyclopedia backend.
#[derive(Debug)]
pub struct EncyclopediaBackend {
    descriptor: BackendDescriptor,
    path: PathBuf,
    source_uri: String,
    access_mode: ExternalAccessMode,
    publisher: String,
    immutable_source_sha256: Option<String>,
    rights: Vec<RightsStatement>,
    use_policy: UsePolicy,
    max_snippet_chars: u32,
    initial_identity: FileIdentity,
    instance_id: u64,
    operation_sequence: AtomicU64,
}

impl EncyclopediaBackend {
    pub fn open(config: EncyclopediaBackendConfig) -> Result<Self, InformationError> {
        config.validate()?;
        let path = fs::canonicalize(&config.path).map_err(|error| {
            io_error(
                "encyclopedia_source_canonicalize_failed",
                format!("cannot resolve encyclopedia source path: {error}"),
            )
        })?;
        let source_uri = Url::from_file_path(&path)
            .map_err(|()| {
                input_error(
                    "encyclopedia_path_not_file_uri",
                    "encyclopedia source path cannot be represented as a file URI",
                )
            })?
            .to_string();
        let initial_identity = FileIdentity::observe(&path)?;
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
                SidecarSet::observe(&path)?.ensure_no_pending_writes()?;
                let observed = hash_immutable_source(&path, &initial_identity)?;
                if expected_sha256
                    .as_ref()
                    .is_some_and(|expected| expected != &observed)
                {
                    return Err(integrity_error(
                        "immutable_encyclopedia_digest_mismatch",
                        "immutable encyclopedia source does not match its expected SHA-256 digest",
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
            source_uri,
            access_mode: config.access_mode,
            publisher: config.publisher,
            immutable_source_sha256,
            rights: config.rights,
            use_policy,
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

    /// Return the immutable digest or a deliberately volatile live reference.
    #[must_use]
    pub fn source_fingerprint(&self) -> String {
        match &self.immutable_source_sha256 {
            Some(digest) => format!("sha256:{digest}"),
            None => {
                let mut hasher = Sha256::new();
                hasher.update(b"information-native.encyclopedia-live-reference.v1\0");
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
                    "volatile-encyclopedia-reference-v1:{}",
                    hex::encode(hasher.finalize())
                )
            }
        }
    }

    fn next_operation_sequence(&self) -> u64 {
        self.operation_sequence.fetch_add(1, Ordering::Relaxed)
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
                hasher.update(b"information-native.encyclopedia-live-snapshot.v1\0");
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
                    "volatile-encyclopedia-snapshot-v1:{}",
                    hex::encode(hasher.finalize())
                )
            }
        }
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
                "the requested retrieval purpose is not explicitly allowed by the encyclopedia use policy",
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
        before_sidecars.ensure_no_pending_writes()?;
        if self.access_mode == ExternalAccessMode::ImmutableReadOnly
            && before != self.initial_identity
        {
            return Err(integrity_error(
                "immutable_encyclopedia_identity_changed",
                "immutable encyclopedia source identity changed after registration",
            ));
        }

        let connection = open_read_only_connection(&self.path, self.access_mode, timeout_ms)?;
        connection
            .execute_batch("BEGIN DEFERRED TRANSACTION")
            .map_err(map_sqlite_error)?;
        let result = (|| {
            let connected_identity = FileIdentity::observe(&self.path)?;
            if connected_identity != before {
                return Err(integrity_error(
                    "encyclopedia_identity_changed_before_read",
                    "encyclopedia identity changed while opening a read snapshot",
                ));
            }
            validate_encyclopedia_schema(&connection)?;
            let facts = SqliteSnapshotFacts::observe(&connection)?;
            let snapshot_sidecars = SidecarSet::observe(&self.path)?;
            snapshot_sidecars.ensure_no_pending_writes()?;
            if snapshot_sidecars != before_sidecars {
                return Err(integrity_error(
                    "encyclopedia_sidecar_changed",
                    "encyclopedia sidecar identity changed while opening a read snapshot",
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
        if before != after {
            return Err(integrity_error(
                "encyclopedia_identity_changed_during_read",
                "encyclopedia source identity changed during a read; results were discarded",
            ));
        }
        after_sidecars.ensure_no_pending_writes()?;
        if before_sidecars != after_sidecars {
            return Err(integrity_error(
                "encyclopedia_sidecar_changed",
                "encyclopedia sidecar identity changed during a read",
            ));
        }
        result
    }
}

impl EncyclopediaBackend {
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
        let target_len = limit as usize;
        let candidate_cap = search_policy_candidate_cap(limit);
        let fetch_limit = i64::from(candidate_cap).saturating_add(1);
        let mut selected_hits = None;
        let mut successful_query_count = 0_usize;
        let mut query_error_count = 0_usize;
        let mut examined_count = 0_usize;
        let mut unknown_origin_count = 0_usize;
        let mut purpose_denied_count = 0_usize;
        let mut oversample_exhausted_count = 0_usize;
        let mut hit_budget_truncated = false;

        for (index, candidate) in candidates.iter().enumerate() {
            match run_fts_query(
                connection,
                &candidate.query,
                query,
                fetch_limit,
                self.max_snippet_chars,
            ) {
                Ok(raw_hits) => {
                    successful_query_count = successful_query_count.saturating_add(1);
                    let oversample_exhausted = raw_hits.len() > candidate_cap as usize;
                    if oversample_exhausted {
                        oversample_exhausted_count = oversample_exhausted_count.saturating_add(1);
                    }

                    let mut allowed_hits = Vec::with_capacity(target_len);
                    for raw in raw_hits.into_iter().take(candidate_cap as usize) {
                        examined_count = examined_count.saturating_add(1);
                        match self.search_candidate_disposition(&raw, query.purpose)? {
                            SearchCandidateDisposition::Allowed => {
                                if allowed_hits.len() < target_len {
                                    allowed_hits.push(raw);
                                } else {
                                    hit_budget_truncated = true;
                                }
                            }
                            SearchCandidateDisposition::UnknownOrigin => {
                                unknown_origin_count = unknown_origin_count.saturating_add(1);
                            }
                            SearchCandidateDisposition::PurposeDenied => {
                                purpose_denied_count = purpose_denied_count.saturating_add(1);
                            }
                        }
                    }
                    if !allowed_hits.is_empty() {
                        if index > 0 {
                            warnings.push(format!(
                                "used {} query fallback after earlier candidates produced no usable hits",
                                candidate.label
                            ));
                        }
                        selected_hits = Some(allowed_hits);
                        break;
                    }
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

        if successful_query_count == 0 {
            return Err(input_error(
                "all_encyclopedia_fts_queries_failed",
                "all encyclopedia FTS query candidates were rejected by SQLite",
            ));
        }
        let raw_hits = selected_hits.unwrap_or_default();

        let skipped_count = unknown_origin_count.saturating_add(purpose_denied_count);
        let mut complete = query_error_count == 0
            && skipped_count == 0
            && oversample_exhausted_count == 0
            && !hit_budget_truncated;
        if skipped_count > 0 {
            warnings.push(format!(
                "origin policy examined {examined_count} ranked candidates and skipped {skipped_count} ({unknown_origin_count} unknown origin; {purpose_denied_count} not permitted for {}); returned {} allowed hits",
                retrieval_purpose_name(query.purpose),
                raw_hits.len(),
            ));
        }
        if oversample_exhausted_count > 0 {
            warnings.push(format!(
                "bounded origin-policy over-sample reached {candidate_cap} candidates in {oversample_exhausted_count} FTS query candidates; additional policy-allowed matches may exist"
            ));
        }
        if hit_budget_truncated {
            warnings.push(
                "per-backend hit budget truncated additional policy-allowed matches".to_string(),
            );
        }

        let raw_count = raw_hits.len();
        let mut remaining_chars = query.budget.max_context_chars as usize;
        let mut hits = Vec::with_capacity(raw_count);
        for (index, mut raw) in raw_hits.into_iter().enumerate() {
            let remaining_hits = raw_count.saturating_sub(index).max(1);
            let share = remaining_chars / remaining_hits;
            let (snippet, snippet_truncated) = truncate_chars(
                &raw.evidence_text,
                share.min(self.max_snippet_chars as usize),
            );
            remaining_chars = remaining_chars.saturating_sub(snippet.chars().count());
            raw.evidence_text.clear();
            let rank = u32::try_from(index.saturating_add(1)).map_err(|_| {
                integrity_error(
                    "encyclopedia_rank_overflow",
                    "encyclopedia result rank exceeded supported bounds",
                )
            })?;
            let metadata_truncated = raw.metadata_truncated;
            let hit = self.article_evidence(
                raw,
                snippet,
                String::new(),
                rank,
                ScoreSemantics::LowerIsBetter,
                source_fingerprint,
                query.purpose,
            )?;
            if snippet_truncated || metadata_truncated {
                complete = false;
            }
            hits.push(hit);
        }
        if !complete {
            warnings.push(
                "one or more query, hit, origin-policy, evidence-text, or metadata limits made this response partial"
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

    fn search_candidate_disposition(
        &self,
        raw: &RawArticle,
        purpose: RetrievalPurpose,
    ) -> Result<SearchCandidateDisposition, InformationError> {
        validate_raw_article(raw)?;
        let OriginPolicy {
            rights: origin_rights,
            use_policy: origin_use_policy,
            ..
        } = match classify_origin(raw) {
            Ok(policy) => policy,
            Err(error) if error.code == "encyclopedia_unknown_origin" => {
                return Ok(SearchCandidateDisposition::UnknownOrigin);
            }
            Err(error) => return Err(error),
        };
        let mut rights = self.rights.clone();
        rights.push(origin_rights);
        let mut policy = intersect_use_policy(self.use_policy, origin_use_policy);
        policy = intersect_rights_into_use_policy(policy, &rights);
        if policy.permission_for(purpose) == UsePermission::Allowed {
            Ok(SearchCandidateDisposition::Allowed)
        } else {
            Ok(SearchCandidateDisposition::PurposeDenied)
        }
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
        let EvidenceLocator::Record { collection, key } = &request.locator else {
            return Err(InformationError::new(
                ErrorClass::Unsupported,
                "unsupported_encyclopedia_locator",
                "encyclopedia reads require an articles record locator",
            ));
        };
        if collection.as_deref() != Some(ARTICLE_COLLECTION) {
            return Err(InformationError::new(
                ErrorClass::Unsupported,
                "unsupported_encyclopedia_collection",
                "encyclopedia record locators must name the articles collection",
            ));
        }
        let id = parse_article_id(key)?;
        self.read_article(
            connection,
            source_fingerprint,
            id,
            request.max_context_chars,
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
        if request
            .collection
            .as_deref()
            .is_some_and(|collection| collection != ARTICLE_COLLECTION)
        {
            return Err(InformationError::new(
                ErrorClass::Unsupported,
                "unsupported_encyclopedia_collection",
                "encyclopedia lookup supports only the articles collection",
            ));
        }
        let id = parse_article_id(&request.key)?;
        self.read_article(
            connection,
            source_fingerprint,
            id,
            request.max_context_chars,
            request.purpose,
        )
    }

    fn read_article(
        &self,
        connection: &Connection,
        source_fingerprint: &str,
        id: i64,
        max_context_chars: u32,
        purpose: RetrievalPurpose,
    ) -> Result<BackendReadResult, InformationError> {
        let started = Instant::now();
        let mut raw = load_article(connection, id, max_context_chars)?;
        let (context, text_truncated) =
            truncate_chars(&raw.evidence_text, max_context_chars as usize);
        raw.evidence_text.clear();
        let metadata_truncated = raw.metadata_truncated;
        let hit = self.article_evidence(
            raw,
            String::new(),
            context,
            1,
            ScoreSemantics::RankOnly,
            source_fingerprint,
            purpose,
        )?;
        let complete = !(text_truncated || metadata_truncated);
        let warnings = if complete {
            Vec::new()
        } else {
            vec!["article text or metadata was truncated to a compiled budget".to_string()]
        };
        Ok(BackendReadResult {
            complete,
            warnings,
            hit,
            elapsed_ms: elapsed_millis(started),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn article_evidence(
        &self,
        raw: RawArticle,
        snippet: String,
        context: String,
        rank: u32,
        score_semantics: ScoreSemantics,
        source_fingerprint: &str,
        purpose: RetrievalPurpose,
    ) -> Result<EvidenceHit, InformationError> {
        validate_raw_article(&raw)?;
        let OriginPolicy {
            class_name,
            rights: origin_rights,
            use_policy: origin_use_policy,
        } = classify_origin(&raw)?;
        let mut rights = self.rights.clone();
        rights.push(origin_rights);
        let mut hit_use_policy = intersect_use_policy(self.use_policy, origin_use_policy);
        hit_use_policy = intersect_rights_into_use_policy(hit_use_policy, &rights);
        self.ensure_purpose_allowed(hit_use_policy, purpose)?;

        let article_id = raw.id.to_string();
        let title = preferred_title(&raw, &article_id);
        let mut metadata = BTreeMap::new();
        metadata.insert("profile".to_string(), json!(PROFILE_NAME));
        metadata.insert("collection".to_string(), json!(ARTICLE_COLLECTION));
        metadata.insert("origin".to_string(), json!(&raw.origin));
        metadata.insert("origin_class".to_string(), json!(class_name));
        metadata.insert("headword".to_string(), json!(&raw.headword));
        metadata.insert("category".to_string(), json!(&raw.category));
        metadata.insert("edition".to_string(), json!(&raw.edition));
        metadata.insert("revision".to_string(), json!(raw.revision));
        metadata.insert("authors".to_string(), json!(&raw.authors));
        metadata.insert("source_container".to_string(), json!(&raw.source_container));
        metadata.insert("bibliography".to_string(), json!(&raw.bibliography));
        metadata.insert("media_credits".to_string(), json!(&raw.media_credits));
        metadata.insert(
            "metadata_truncated".to_string(),
            json!(raw.metadata_truncated),
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
        let evidence_id = format!(
            "{}:{}:{}:{}",
            self.descriptor.resource_id,
            self.descriptor.release_id,
            self.descriptor.representation_id,
            article_id
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
            title,
            creator: nonempty(raw.authors.clone()),
            snippet,
            context,
            excerpt_sha256,
            source_fingerprint: Some(source_fingerprint.to_string()),
            document_id: Some(article_id.clone()),
            passage_id: None,
            locator: EvidenceLocator::Record {
                collection: Some(ARTICLE_COLLECTION.to_string()),
                key: article_id.clone(),
            },
            source_uri: Some(self.source_uri.clone()),
            provenance: Provenance {
                publisher: self.publisher.clone(),
                source_uri: self.source_uri.clone(),
                upstream_record_id: Some(article_id),
                source_inputs: vec![format!("sqlite-source-{source_fingerprint}")],
                transformation: Some(
                    "read-only compiled encyclopedia SQLite/FTS5 retrieval with bounded text"
                        .to_string(),
                ),
                metadata: BTreeMap::from([
                    ("profile".to_string(), json!(PROFILE_NAME)),
                    ("raw_origin".to_string(), json!(&raw.origin)),
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
                "invalid_encyclopedia_evidence",
                format!("generated encyclopedia evidence failed validation: {error}"),
            )
        })?;
        Ok(hit)
    }
}

impl ResourceBackend for EncyclopediaBackend {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    fn health(&self) -> BackendHealth {
        match self.with_connection(1_000, |connection, _fingerprint| {
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
        let started = Instant::now();
        let result = self.with_connection(query.budget.timeout_ms, |connection, fingerprint| {
            self.search_connection(connection, fingerprint, query)
        })?;
        enforce_soft_deadline(started, query.budget.timeout_ms)?;
        Ok(result)
    }

    fn read(&self, request: &ReadRequest) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        validate_direct_read_budget(request.max_context_chars, request.timeout_ms)?;
        let started = Instant::now();
        let result = self.with_connection(request.timeout_ms, |connection, fingerprint| {
            self.read_connection(connection, fingerprint, request)
        })?;
        enforce_soft_deadline(started, request.timeout_ms)?;
        Ok(result)
    }

    fn lookup(&self, request: &LookupRequest) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        validate_direct_read_budget(request.max_context_chars, request.timeout_ms)?;
        let started = Instant::now();
        let result = self.with_connection(request.timeout_ms, |connection, fingerprint| {
            self.lookup_connection(connection, fingerprint, request)
        })?;
        enforce_soft_deadline(started, request.timeout_ms)?;
        Ok(result)
    }
}

fn open_read_only_connection(
    path: &Path,
    _access_mode: ExternalAccessMode,
    timeout_ms: u64,
) -> Result<Connection, InformationError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    // Even live mode uses SQLite's immutable transport for each individual
    // operation. The backend reopens and re-identifies the file each time, so
    // live mode still observes main-file changes between operations. Rejecting
    // pending sidecars keeps immutable transport truthful and prevents SQLite
    // from materializing WAL/SHM files beside the canonical source.
    let mut uri = Url::from_file_path(path).map_err(|()| {
        input_error(
            "encyclopedia_path_not_file_uri",
            "encyclopedia source path cannot be represented as a file URI",
        )
    })?;
    uri.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
    let connection = Connection::open_with_flags(uri.as_str(), flags | OpenFlags::SQLITE_OPEN_URI)
        .map_err(map_sqlite_error)?;
    connection.set_limit(
        Limit::SQLITE_LIMIT_VARIABLE_NUMBER,
        i32::try_from(MAX_SQLITE_BINDS).map_err(|_| {
            integrity_error(
                "encyclopedia_bind_limit_overflow",
                "compiled encyclopedia bind limit exceeds SQLite integer bounds",
            )
        })?,
    );
    if connection.limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER)
        < i32::try_from(MAX_SQLITE_BINDS).unwrap_or(i32::MAX)
    {
        return Err(InformationError::new(
            ErrorClass::Unsupported,
            "encyclopedia_bind_limit_too_low",
            "SQLite runtime cannot support the compiled encyclopedia bind limit",
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
            "encyclopedia_read_only_pragmas_failed",
            "SQLite did not retain trusted_schema=OFF and query_only=ON",
        ));
    }
    let now = Instant::now();
    let deadline = now
        .checked_add(Duration::from_millis(timeout_ms))
        .map_or(now, |value| value);
    connection.progress_handler(1_000, Some(move || Instant::now() >= deadline));
    Ok(connection)
}

fn sidecar_path(path: &Path, suffix: &str) -> Result<PathBuf, InformationError> {
    let Some(file_name) = path.file_name() else {
        return Err(input_error(
            "encyclopedia_path_has_no_file_name",
            "encyclopedia source path has no file name",
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
            "verified_encyclopedia_identity_mismatch",
            "encyclopedia source no longer matches the identity bound to its verified digest",
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
            "immutable_encyclopedia_hash_open_failed",
            format!("cannot open immutable encyclopedia source for verification: {error}"),
        )
    })?;
    let opened_identity = FileIdentity::from_metadata(&file.metadata().map_err(|error| {
        io_error(
            "immutable_encyclopedia_hash_metadata_failed",
            format!("cannot inspect opened immutable encyclopedia source: {error}"),
        )
    })?)?;
    if &opened_identity != expected_identity {
        return Err(integrity_error(
            "immutable_encyclopedia_identity_changed_before_hash",
            "immutable encyclopedia identity changed before digest verification",
        ));
    }

    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut hasher = Sha256::new();
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            io_error(
                "immutable_encyclopedia_hash_read_failed",
                format!("cannot hash immutable encyclopedia source: {error}"),
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
                "immutable_encyclopedia_hash_metadata_failed",
                format!("cannot re-inspect opened immutable encyclopedia source: {error}"),
            )
        })?)?;
    let path_after = FileIdentity::observe(path)?;
    if opened_identity != handle_after || &path_after != expected_identity {
        return Err(integrity_error(
            "immutable_encyclopedia_identity_changed_during_hash",
            "immutable encyclopedia identity changed during digest verification",
        ));
    }
    Ok(hex::encode(hasher.finalize()))
}

#[derive(Debug, PartialEq, Eq)]
struct ColumnSpec {
    name: String,
    declared_type: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key_position: i64,
    hidden: i64,
}

fn validate_encyclopedia_schema(connection: &Connection) -> Result<(), InformationError> {
    const ARTICLE_COLUMNS: &[(&str, &str, bool, i64, i64)] = &[
        ("id", "INTEGER", false, 1, 0),
        ("origin", "TEXT", true, 0, 0),
        ("source_id", "INTEGER", false, 0, 0),
        ("title", "TEXT", false, 0, 0),
        ("headword", "TEXT", false, 0, 0),
        ("category", "TEXT", false, 0, 0),
        ("edition", "TEXT", false, 0, 0),
        ("revision", "INTEGER", false, 0, 0),
        ("markup", "TEXT", false, 0, 0),
        ("plain_text", "TEXT", false, 0, 0),
        ("source_container", "TEXT", false, 0, 0),
        ("authors", "TEXT", false, 0, 0),
        ("related", "TEXT", false, 0, 0),
        ("bibliography", "TEXT", false, 0, 0),
        ("media_credits", "TEXT", false, 0, 0),
        ("plain_text_raw", "TEXT", false, 0, 0),
        ("text_cleanup_version", "TEXT", false, 0, 0),
    ];
    const FTS_COLUMNS: &[(&str, &str, bool, i64, i64)] = &[
        ("title", "", false, 0, 0),
        ("plain_text", "", false, 0, 0),
        ("articles_fts", "", false, 0, 1),
        ("rank", "", false, 0, 1),
    ];

    require_exact_columns(connection, "articles", ARTICLE_COLUMNS)?;
    require_exact_columns(connection, "articles_fts", FTS_COLUMNS)?;
    require_table_shape(connection, "articles", "table", 17, false, false)?;
    require_table_shape(connection, "articles_fts", "virtual", 4, false, false)?;

    let fts_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            ["articles_fts"],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_sqlite_error)?
        .flatten()
        .ok_or_else(|| {
            integrity_error(
                "encyclopedia_profile_missing_fts",
                "articles_fts is missing from the encyclopedia profile",
            )
        })?;
    let normalized = strip_ascii_whitespace(&fts_sql).to_ascii_lowercase();
    let expected = "createvirtualtablearticles_ftsusingfts5(title,plain_text,content='articles',content_rowid='id')";
    if normalized != expected {
        return Err(integrity_error(
            "encyclopedia_profile_invalid_fts",
            "articles_fts does not exactly match the compiled external-content FTS5 profile",
        ));
    }
    Ok(())
}

fn require_exact_columns(
    connection: &Connection,
    table: &'static str,
    expected: &[(&str, &str, bool, i64, i64)],
) -> Result<(), InformationError> {
    let sql = match table {
        "articles" => "PRAGMA table_xinfo(articles)",
        "articles_fts" => "PRAGMA table_xinfo(articles_fts)",
        _ => {
            return Err(integrity_error(
                "encyclopedia_profile_internal_table",
                "backend attempted to inspect an unknown profile table",
            ));
        }
    };
    let mut statement = connection.prepare(sql).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok(ColumnSpec {
                name: row.get(1)?,
                declared_type: row.get(2)?,
                not_null: row.get::<_, i64>(3)? != 0,
                default_value: row.get(4)?,
                primary_key_position: row.get(5)?,
                hidden: row.get(6)?,
            })
        })
        .map_err(map_sqlite_error)?;
    let observed = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    let expected = expected
        .iter()
        .map(
            |(name, declared_type, not_null, primary_key_position, hidden)| ColumnSpec {
                name: (*name).to_string(),
                declared_type: (*declared_type).to_string(),
                not_null: *not_null,
                default_value: None,
                primary_key_position: *primary_key_position,
                hidden: *hidden,
            },
        )
        .collect::<Vec<_>>();
    if observed != expected {
        return Err(integrity_error(
            "encyclopedia_profile_column_mismatch",
            format!("{table} columns do not exactly match the compiled encyclopedia profile"),
        ));
    }
    Ok(())
}

fn require_table_shape(
    connection: &Connection,
    table: &'static str,
    expected_type: &'static str,
    expected_columns: i64,
    expected_without_rowid: bool,
    expected_strict: bool,
) -> Result<(), InformationError> {
    let observed = connection
        .query_row(
            "SELECT type, ncol, wr, strict FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
            [table],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite_error)?;
    if observed
        != Some((
            expected_type.to_string(),
            expected_columns,
            expected_without_rowid,
            expected_strict,
        ))
    {
        return Err(integrity_error(
            "encyclopedia_profile_table_shape_mismatch",
            format!("{table} does not have the compiled encyclopedia table shape"),
        ));
    }
    Ok(())
}

fn strip_ascii_whitespace(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect()
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
            "query exact targets exclude this encyclopedia backend",
        ));
    }
    if !query.resources.is_empty() && !query.resources.contains(&descriptor.resource_id) {
        return Err(input_error(
            "query_resource_mismatch",
            "query resource selection excludes this encyclopedia backend",
        ));
    }
    if !query.representations.is_empty()
        && !query
            .representations
            .contains(&descriptor.representation_id)
    {
        return Err(input_error(
            "query_representation_mismatch",
            "query representation selection excludes this encyclopedia backend",
        ));
    }
    if !query.filters.languages.is_empty()
        || query.filters.spatial.is_some()
        || query.filters.temporal_start.is_some()
        || query.filters.temporal_end.is_some()
    {
        return Err(InformationError::new(
            ErrorClass::Unsupported,
            "encyclopedia_filter_unsupported",
            "the encyclopedia profile does not support language, spatial, or temporal filters",
        ));
    }
    for (name, values) in [
        ("subjects", &query.filters.subjects),
        ("document_ids", &query.filters.document_ids),
    ] {
        if values.len() > MAX_FILTER_VALUES_PER_LIST {
            return Err(input_error(
                "encyclopedia_filter_value_limit_exceeded",
                format!(
                    "encyclopedia filter {name} exceeds the compiled cap of {MAX_FILTER_VALUES_PER_LIST} values"
                ),
            ));
        }
    }
    for document_id in &query.filters.document_ids {
        parse_article_id(document_id)?;
    }
    if query.filters.fields.len() > MAX_FIELD_FILTERS {
        return Err(input_error(
            "encyclopedia_field_filter_limit_exceeded",
            format!(
                "encyclopedia field filters exceed the compiled cap of {MAX_FIELD_FILTERS} entries"
            ),
        ));
    }
    let supported_fields = BTreeSet::from([
        "authors",
        "category",
        "edition",
        "headword",
        "origin",
        "revision",
        "source_container",
        "title",
    ]);
    if let Some(field) = query
        .filters
        .fields
        .keys()
        .find(|field| !supported_fields.contains(field.as_str()))
    {
        return Err(InformationError::new(
            ErrorClass::Unsupported,
            "encyclopedia_field_filter_unsupported",
            format!("encyclopedia does not support the requested field filter: {field}"),
        ));
    }
    if let Some(revision) = query.filters.fields.get("revision") {
        revision.parse::<i64>().map_err(|_| {
            input_error(
                "invalid_encyclopedia_revision_filter",
                "encyclopedia revision filters must be signed 64-bit integers",
            )
        })?;
    }
    let bind_count = 3_usize
        .checked_add(query.filters.document_ids.len())
        .and_then(|count| count.checked_add(query.filters.subjects.len()))
        .and_then(|count| count.checked_add(query.filters.fields.len()))
        .ok_or_else(|| {
            input_error(
                "encyclopedia_filter_bind_overflow",
                "encyclopedia bind accounting overflowed",
            )
        })?;
    if bind_count > MAX_SQLITE_BINDS {
        return Err(input_error(
            "encyclopedia_filter_bind_limit_exceeded",
            format!(
                "encyclopedia query requires {bind_count} binds, exceeding the compiled cap of {MAX_SQLITE_BINDS}"
            ),
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
            "read request does not target this encyclopedia backend identity",
        ));
    }
    Ok(())
}

fn parse_article_id(value: &str) -> Result<i64, InformationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_ID_CHARS || trimmed != value {
        return Err(input_error(
            "invalid_encyclopedia_article_id",
            "article id must be a bounded positive decimal integer",
        ));
    }
    let id = trimmed.parse::<i64>().map_err(|_| {
        input_error(
            "invalid_encyclopedia_article_id",
            "article id must be a bounded positive decimal integer",
        )
    })?;
    if id <= 0 {
        return Err(input_error(
            "invalid_encyclopedia_article_id",
            "article id must be a bounded positive decimal integer",
        ));
    }
    if id.to_string() != value {
        return Err(input_error(
            "invalid_encyclopedia_article_id",
            "article id must use its canonical positive decimal representation",
        ));
    }
    Ok(id)
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
    let all = join_terms(&terms, "AND");
    let any = join_terms(&terms, "OR");
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
            "encyclopedia_fts_query_has_no_terms",
            "query contains no searchable encyclopedia terms",
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
    let mut terms = input
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>();
    let truncated = terms.len() > MAX_QUERY_TERMS;
    terms.truncate(MAX_QUERY_TERMS);
    (terms, truncated)
}

fn join_terms(terms: &[String], operator: &str) -> String {
    terms.join(&format!(" {operator} "))
}

#[derive(Debug)]
struct RawArticle {
    id: i64,
    origin: String,
    title: Option<String>,
    headword: Option<String>,
    category: Option<String>,
    edition: Option<String>,
    revision: Option<i64>,
    authors: Option<String>,
    source_container: Option<String>,
    bibliography: Option<String>,
    media_credits: Option<String>,
    score: f64,
    evidence_text: String,
    origin_truncated: bool,
    metadata_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchCandidateDisposition {
    Allowed,
    UnknownOrigin,
    PurposeDenied,
}

fn search_policy_candidate_cap(limit: u32) -> u32 {
    limit
        .saturating_mul(SEARCH_POLICY_OVERSAMPLE_FACTOR)
        .min(MAX_SEARCH_POLICY_CANDIDATES)
        .max(limit)
}

fn validate_raw_article(raw: &RawArticle) -> Result<(), InformationError> {
    if raw.id <= 0 || raw.origin.trim().is_empty() {
        return Err(integrity_error(
            "encyclopedia_invalid_article_identity",
            "encyclopedia article has an invalid id or empty origin",
        ));
    }
    if raw.origin_truncated {
        return Err(integrity_error(
            "encyclopedia_origin_too_long",
            "encyclopedia article origin exceeds the compiled rights-classification bound",
        ));
    }
    if !raw.score.is_finite() {
        return Err(integrity_error(
            "encyclopedia_invalid_score",
            "encyclopedia article has a non-finite score",
        ));
    }
    Ok(())
}

fn run_fts_query(
    connection: &Connection,
    fts_query: &str,
    query: &InformationQuery,
    limit: i64,
    max_snippet_chars: u32,
) -> rusqlite::Result<Vec<RawArticle>> {
    let mut sql = String::from(
        r#"
        SELECT
            a.id,
            substr(a.origin, 1, 513),
            substr(a.title, 1, 2049),
            substr(a.headword, 1, 2049),
            substr(a.category, 1, 1025),
            substr(a.edition, 1, 1025),
            a.revision,
            substr(a.authors, 1, 4097),
            substr(a.source_container, 1, 8193),
            substr(a.bibliography, 1, 16385),
            substr(a.media_credits, 1, 16385),
            bm25(articles_fts) AS native_score,
            substr(snippet(articles_fts, -1, '[', ']', ' ... ', 32), 1, ?)
        FROM articles_fts
        JOIN articles a ON a.id = articles_fts.rowid
        WHERE articles_fts MATCH ?
          AND a.id > 0
        "#,
    );
    let mut values = vec![
        SqlValue::Integer(i64::from(max_snippet_chars).saturating_add(1)),
        SqlValue::Text(fts_query.to_string()),
    ];
    if !query.filters.document_ids.is_empty() {
        sql.push_str(&format!(
            "\nAND a.id IN ({})",
            placeholders(query.filters.document_ids.len())
        ));
        for id in &query.filters.document_ids {
            let parsed = id
                .parse::<i64>()
                .map_err(|_| rusqlite::Error::InvalidParameterName("document_ids".to_string()))?;
            values.push(SqlValue::Integer(parsed));
        }
    }
    add_text_in_filter(&mut sql, &mut values, "a.category", &query.filters.subjects);
    for (field, value) in &query.filters.fields {
        match field.as_str() {
            "authors" => sql.push_str("\nAND a.authors = ?"),
            "category" => sql.push_str("\nAND a.category = ?"),
            "edition" => sql.push_str("\nAND a.edition = ?"),
            "headword" => sql.push_str("\nAND a.headword = ?"),
            "origin" => sql.push_str("\nAND a.origin = ?"),
            "source_container" => sql.push_str("\nAND a.source_container = ?"),
            "title" => sql.push_str("\nAND a.title = ?"),
            "revision" => {
                sql.push_str("\nAND a.revision = ?");
                let revision = value
                    .parse::<i64>()
                    .map_err(|_| rusqlite::Error::InvalidParameterName(field.clone()))?;
                values.push(SqlValue::Integer(revision));
                continue;
            }
            _ => return Err(rusqlite::Error::InvalidParameterName(field.clone())),
        }
        values.push(SqlValue::Text(value.clone()));
    }
    sql.push_str("\nORDER BY native_score ASC, a.id ASC LIMIT ?");
    values.push(SqlValue::Integer(limit));
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values.iter()), raw_article_from_row)?;
    rows.collect()
}

fn raw_article_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawArticle> {
    let (origin, origin_truncated) = bound_required(row.get(1)?, MAX_ORIGIN_CHARS);
    let (title, title_truncated) = bound_optional(row.get(2)?, MAX_TITLE_CHARS);
    let (headword, headword_truncated) = bound_optional(row.get(3)?, MAX_TITLE_CHARS);
    let (category, category_truncated) = bound_optional(row.get(4)?, MAX_SHORT_METADATA_CHARS);
    let (edition, edition_truncated) = bound_optional(row.get(5)?, MAX_SHORT_METADATA_CHARS);
    let (authors, authors_truncated) = bound_optional(row.get(7)?, MAX_AUTHORS_CHARS);
    let (source_container, source_container_truncated) =
        bound_optional(row.get(8)?, MAX_SOURCE_CONTAINER_CHARS);
    let (bibliography, bibliography_truncated) =
        bound_optional(row.get(9)?, MAX_LONG_METADATA_CHARS);
    let (media_credits, media_credits_truncated) =
        bound_optional(row.get(10)?, MAX_LONG_METADATA_CHARS);
    Ok(RawArticle {
        id: row.get(0)?,
        origin,
        title,
        headword,
        category,
        edition,
        revision: row.get(6)?,
        authors,
        source_container,
        bibliography,
        media_credits,
        score: row.get(11)?,
        evidence_text: row.get::<_, Option<String>>(12)?.unwrap_or_default(),
        origin_truncated,
        metadata_truncated: origin_truncated
            || title_truncated
            || headword_truncated
            || category_truncated
            || edition_truncated
            || authors_truncated
            || source_container_truncated
            || bibliography_truncated
            || media_credits_truncated,
    })
}

fn load_article(
    connection: &Connection,
    id: i64,
    max_context_chars: u32,
) -> Result<RawArticle, InformationError> {
    connection
        .query_row(
            r#"
            SELECT
                a.id,
                substr(a.origin, 1, 513),
                substr(a.title, 1, 2049),
                substr(a.headword, 1, 2049),
                substr(a.category, 1, 1025),
                substr(a.edition, 1, 1025),
                a.revision,
                substr(a.authors, 1, 4097),
                substr(a.source_container, 1, 8193),
                substr(a.bibliography, 1, 16385),
                substr(a.media_credits, 1, 16385),
                1.0,
                substr(a.plain_text, 1, ?2)
            FROM articles a
            WHERE a.id = ?1
              AND a.id > 0
            "#,
            params![id, i64::from(max_context_chars).saturating_add(1)],
            raw_article_from_row,
        )
        .optional()
        .map_err(map_sqlite_error)?
        .ok_or_else(|| {
            let mut error = InformationError::new(
                ErrorClass::NotFound,
                "encyclopedia_article_not_found",
                "encyclopedia article was not found",
            );
            error.retryable = false;
            error
        })
}

fn add_text_in_filter(
    sql: &mut String,
    values: &mut Vec<SqlValue>,
    column: &str,
    items: &[String],
) {
    if items.is_empty() {
        return;
    }
    sql.push_str(&format!(
        "\nAND {column} IN ({})",
        placeholders(items.len())
    ));
    values.extend(items.iter().cloned().map(SqlValue::Text));
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

fn bound_required(value: String, limit: usize) -> (String, bool) {
    truncate_chars(&value, limit)
}

fn bound_optional(value: Option<String>, limit: usize) -> (Option<String>, bool) {
    match value {
        Some(value) => {
            let (bounded, truncated) = truncate_chars(&value, limit);
            (Some(bounded), truncated)
        }
        None => (None, false),
    }
}

fn truncate_chars(input: &str, limit: usize) -> (String, bool) {
    let mut characters = input.chars();
    let bounded = characters.by_ref().take(limit).collect::<String>();
    let truncated = characters.next().is_some();
    (bounded, truncated)
}

fn preferred_title(raw: &RawArticle, article_id: &str) -> String {
    nonempty(raw.title.clone())
        .or_else(|| nonempty(raw.headword.clone()))
        .unwrap_or_else(|| format!("Encyclopedia article {article_id}"))
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|candidate| !candidate.trim().is_empty())
}

struct OriginPolicy {
    class_name: &'static str,
    rights: RightsStatement,
    use_policy: UsePolicy,
}

fn classify_origin(raw: &RawArticle) -> Result<OriginPolicy, InformationError> {
    let normalized = raw.origin.trim().to_ascii_lowercase();
    if origin_has_prefix(&normalized, "britannica")
        || origin_has_prefix(&normalized, "encarta")
        || origin_has_prefix(&normalized, "local")
    {
        let owner = if origin_has_prefix(&normalized, "britannica") {
            "Encyclopaedia Britannica"
        } else if origin_has_prefix(&normalized, "encarta") {
            "Microsoft Encarta"
        } else {
            "local rights holder"
        };
        return Ok(OriginPolicy {
            class_name: "copyrighted_private_local",
            rights: RightsStatement {
                scope: format!("articles origin {:?}", raw.origin),
                expression: format!(
                    "Copyrighted encyclopedia text attributed to {owner}; this backend authorizes private local retrieval only and does not grant redistribution rights"
                ),
                license_url: None,
                license_text_sha256: None,
                attribution: Some(raw.origin.clone()),
                obligations: vec![
                    "Keep retrieved source text in private local use".to_string(),
                    "Do not redistribute source text without separate permission".to_string(),
                ],
                redistribution: RedistributionPolicy::PrivateUseOnly,
            },
            use_policy: UsePolicy {
                local_search: UsePermission::Allowed,
                model_context: UsePermission::Forbidden,
                excerpt_export: UsePermission::Forbidden,
                redistribution: UsePermission::Forbidden,
                attribution_required: false,
            },
        });
    }
    if origin_has_prefix(&normalized, "wikipedia") {
        let attribution = wikipedia_attribution(raw);
        return Ok(OriginPolicy {
            class_name: "wikipedia_cc_by_sa",
            rights: RightsStatement {
                scope: format!("articles origin {:?}", raw.origin),
                expression: "Wikipedia-derived article text under Creative Commons Attribution-ShareAlike 4.0"
                    .to_string(),
                license_url: Some(
                    "https://creativecommons.org/licenses/by-sa/4.0/".to_string(),
                ),
                license_text_sha256: None,
                attribution: Some(attribution),
                obligations: vec![
                    "Preserve attribution to Wikipedia contributors".to_string(),
                    "Distribute adaptations under CC BY-SA 4.0".to_string(),
                    "Indicate material changes".to_string(),
                ],
                redistribution: RedistributionPolicy::AllowedWithObligations,
            },
            use_policy: UsePolicy {
                local_search: UsePermission::Allowed,
                model_context: UsePermission::Allowed,
                excerpt_export: UsePermission::Allowed,
                redistribution: UsePermission::Allowed,
                attribution_required: true,
            },
        });
    }
    let mut error = InformationError::new(
        ErrorClass::Permission,
        "encyclopedia_unknown_origin",
        "article origin is not recognized by the compiled rights policy; retrieval failed closed",
    );
    error.retryable = false;
    Err(error)
}

fn origin_has_prefix(origin: &str, prefix: &str) -> bool {
    origin == prefix
        || origin
            .strip_prefix(prefix)
            .and_then(|suffix| suffix.chars().next())
            .is_some_and(|character| {
                character.is_ascii_whitespace() || matches!(character, '-' | '_' | ':' | '/')
            })
}

fn wikipedia_attribution(raw: &RawArticle) -> String {
    let title = nonempty(raw.title.clone())
        .or_else(|| nonempty(raw.headword.clone()))
        .unwrap_or_else(|| format!("article {}", raw.id));
    let contributors =
        nonempty(raw.authors.clone()).unwrap_or_else(|| "Wikipedia contributors".to_string());
    match raw.revision {
        Some(revision) => format!("{contributors}, {title}, revision {revision}"),
        None => format!("{contributors}, {title}"),
    }
}

fn intersect_use_policy(left: UsePolicy, right: UsePolicy) -> UsePolicy {
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

fn access_mode_name(mode: ExternalAccessMode) -> &'static str {
    match mode {
        ExternalAccessMode::LiveReadOnly => "live_read_only",
        ExternalAccessMode::ImmutableReadOnly => "immutable_read_only",
    }
}

fn retrieval_purpose_name(purpose: RetrievalPurpose) -> &'static str {
    match purpose {
        RetrievalPurpose::LocalUi => "local_ui",
        RetrievalPurpose::ModelContext => "model_context",
        RetrievalPurpose::ExcerptExport => "excerpt_export",
    }
}

fn source_fingerprint_kind(fingerprint: &str) -> &'static str {
    if fingerprint.starts_with("sha256:") {
        "sha256"
    } else {
        "volatile_live_snapshot"
    }
}

fn normalize_sha256(value: &str) -> String {
    value
        .strip_prefix("sha256:")
        .unwrap_or(value)
        .to_ascii_lowercase()
}

fn validate_sha256(value: &str) -> Result<(), InformationError> {
    let normalized = value.strip_prefix("sha256:").unwrap_or(value);
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(input_error(
            "invalid_verified_source_sha256",
            "verified encyclopedia source digest is not a SHA-256 value",
        ));
    }
    Ok(())
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).map_or(u64::MAX, |value| value)
}

fn enforce_soft_deadline(started: Instant, timeout_ms: u64) -> Result<(), InformationError> {
    if elapsed_millis(started) >= timeout_ms {
        return Err(InformationError::new(
            ErrorClass::ResourceBusy,
            "encyclopedia_soft_deadline_exceeded",
            "encyclopedia operation exceeded its cooperative soft deadline; results were discarded",
        )
        .retryable(true));
    }
    Ok(())
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
                "encyclopedia_query_timeout",
                "encyclopedia SQLite query deadline elapsed",
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
                "encyclopedia_resource_busy",
                "encyclopedia SQLite source is busy",
            )
            .retryable(true)
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(code.code, ErrorCode::PermissionDenied | ErrorCode::ReadOnly) =>
        {
            InformationError::new(
                ErrorClass::Permission,
                "encyclopedia_permission_denied",
                "SQLite denied the read-only encyclopedia operation",
            )
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            ) =>
        {
            integrity_error(
                "encyclopedia_source_invalid",
                "encyclopedia source is corrupt or not a database",
            )
        }
        _ => InformationError::new(
            ErrorClass::Backend,
            "encyclopedia_backend_error",
            format!("encyclopedia SQLite operation failed: {error}"),
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

    fn create_fixture() -> Result<(TempDir, PathBuf), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("encyclopedia.db");
        let connection = Connection::open(&path)?;
        connection.execute_batch(
            r#"
            CREATE TABLE articles (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                origin TEXT NOT NULL,
                source_id INTEGER,
                title TEXT,
                headword TEXT,
                category TEXT,
                edition TEXT,
                revision INTEGER,
                markup TEXT,
                plain_text TEXT,
                source_container TEXT,
                authors TEXT,
                related TEXT,
                bibliography TEXT,
                media_credits TEXT,
                plain_text_raw TEXT,
                text_cleanup_version TEXT
            );
            CREATE VIRTUAL TABLE articles_fts USING fts5(
                title,
                plain_text,
                content='articles',
                content_rowid='id'
            );
            INSERT INTO articles (
                id, origin, source_id, title, headword, category, edition,
                revision, markup, plain_text, source_container, authors,
                related, bibliography, media_credits, plain_text_raw,
                text_cleanup_version
            ) VALUES
                (1, 'Britannica 2015', 10, 'Quantum mechanics', 'Quantum mechanics',
                 'physics', 'ultimate', NULL, NULL,
                 'Quantum mechanics describes matter at microscopic scales.',
                 'work/britannica/article.zip', 'A. Physicist', NULL,
                 'A bounded bibliography', NULL, NULL, 'v1'),
                (2, 'Wikipedia', 11, 'Quantum field theory', 'Quantum field theory',
                 'physics', 'online', 42, NULL,
                 'Quantum field theory combines quantum mechanics and special relativity.',
                 'work/wikipedia/pages.xml', 'Wikipedia contributors', NULL,
                 NULL, 'Image credit under a compatible license', NULL, 'v1'),
                (3, 'Mystery Corpus', 12, 'Unknown provenance', NULL,
                 'unknown', NULL, NULL, NULL,
                 'Quantum claims with no recognized rights provenance.',
                 'work/mystery.bin', NULL, NULL, NULL, NULL, NULL, 'v1'),
                (4, 'Encarta 2009', 13, NULL, 'Relativity',
                 'physics', 'std', 100, NULL,
                 'Relativity concerns measurements of space and time.',
                 'work/encarta/content.akc', NULL, NULL, NULL, NULL, NULL, 'v1'),
                (5, 'Britannica 2015', 14, 'Policy sentinel', 'Policy sentinel',
                 'policy', 'ultimate', NULL, NULL,
                 'Policy sentinel text shared by ranked fixture candidates.',
                 'work/britannica/policy.zip', 'Britannica editor', NULL,
                 NULL, NULL, NULL, 'v1'),
                (6, 'Mystery Corpus', 15, 'Policy sentinel', 'Policy sentinel',
                 'policy', NULL, NULL, NULL,
                 'Policy sentinel text shared by ranked fixture candidates.',
                 'work/mystery-policy.bin', NULL, NULL, NULL, NULL, NULL, 'v1'),
                (7, 'Wikipedia', 16, 'Policy sentinel', 'Policy sentinel',
                 'policy', 'online', 84, NULL,
                 'Policy sentinel text shared by ranked fixture candidates.',
                 'work/wikipedia/policy.xml', 'Wikipedia contributors', NULL,
                 NULL, NULL, NULL, 'v1'),
                (8, 'Wikipedia', 17, 'Policy sentinel', 'Policy sentinel',
                 'policy', 'online', 85, NULL,
                 'Policy sentinel text shared by ranked fixture candidates.',
                 'work/wikipedia/policy-2.xml', 'Wikipedia contributors', NULL,
                 NULL, NULL, NULL, 'v1');
            INSERT INTO articles_fts(articles_fts) VALUES('rebuild');
            "#,
        )?;
        drop(connection);
        Ok((directory, path))
    }

    fn fixture_config(path: &Path) -> Result<EncyclopediaBackendConfig, Box<dyn Error>> {
        Ok(EncyclopediaBackendConfig::new(
            "fixture-encyclopedia",
            "Fixture encyclopedia",
            ResourceId::parse("encyclopedia.fixture")?,
            ReleaseId::parse("fixture-v1")?,
            RepresentationId::parse("fixture-sqlite")?,
            path,
            ExternalAccessMode::LiveReadOnly,
            "Fixture publisher",
        ))
    }

    fn fixture_query(text: &str) -> Result<InformationQuery, Box<dyn Error>> {
        let resource_id = ResourceId::parse("encyclopedia.fixture")?;
        let release_id = ReleaseId::parse("fixture-v1")?;
        let representation_id = RepresentationId::parse("fixture-sqlite")?;
        Ok(InformationQuery {
            schema: QUERY_SCHEMA.to_string(),
            query_id: QueryId::new(),
            text: text.to_string(),
            syntax: QuerySyntax::NaturalTerms,
            purpose: RetrievalPurpose::LocalUi,
            targets: vec![RetrievalTarget {
                resource_id: resource_id.clone(),
                release_id,
                representation_id: representation_id.clone(),
            }],
            resources: vec![resource_id],
            representations: vec![representation_id],
            filters: QueryFilters::default(),
            budget: QueryBudget {
                max_hits: 10,
                max_hits_per_backend: 10,
                max_backends: 1,
                max_context_chars: 4_000,
                timeout_ms: 5_000,
            },
        })
    }

    fn lookup_request(
        id: &str,
        purpose: RetrievalPurpose,
        max_context_chars: u32,
    ) -> Result<LookupRequest, Box<dyn Error>> {
        Ok(LookupRequest {
            resource_id: ResourceId::parse("encyclopedia.fixture")?,
            release_id: ReleaseId::parse("fixture-v1")?,
            representation_id: RepresentationId::parse("fixture-sqlite")?,
            purpose,
            collection: Some(ARTICLE_COLLECTION.to_string()),
            key: id.to_string(),
            max_context_chars,
            timeout_ms: 5_000,
        })
    }

    #[test]
    fn search_emits_stable_record_and_private_copyright_policy() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let backend = EncyclopediaBackend::open(fixture_config(&path)?)?;
        let mut query = fixture_query("quantum mechanics")?;
        query
            .filters
            .fields
            .insert("origin".to_string(), "Britannica 2015".to_string());
        let result = backend.search(&query)?;
        assert_eq!(result.hits.len(), 1);
        let hit = &result.hits[0];
        assert_eq!(hit.document_id.as_deref(), Some("1"));
        assert_eq!(hit.creator.as_deref(), Some("A. Physicist"));
        assert_eq!(hit.metadata.get("origin"), Some(&json!("Britannica 2015")));
        assert_eq!(
            hit.metadata.get("bibliography"),
            Some(&json!("A bounded bibliography"))
        );
        assert_eq!(
            hit.locator,
            EvidenceLocator::Record {
                collection: Some("articles".to_string()),
                key: "1".to_string(),
            }
        );
        assert_eq!(hit.use_policy.local_search, UsePermission::Allowed);
        assert_eq!(hit.use_policy.model_context, UsePermission::Forbidden);
        assert_eq!(hit.use_policy.excerpt_export, UsePermission::Forbidden);
        assert_eq!(hit.use_policy.redistribution, UsePermission::Forbidden);
        assert!(
            hit.rights
                .iter()
                .any(|rights| rights.redistribution == RedistributionPolicy::PrivateUseOnly)
        );
        assert_eq!(
            hit.excerpt_sha256,
            evidence_text_sha256(&hit.snippet, &hit.context)
        );
        hit.validate()?;
        Ok(())
    }

    #[test]
    fn lookup_and_read_obey_text_budget_and_locator_identity() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let backend = EncyclopediaBackend::open(fixture_config(&path)?)?;
        let lookup = backend.lookup(&lookup_request("1", RetrievalPurpose::LocalUi, 12)?)?;
        assert!(!lookup.complete);
        assert_eq!(lookup.hit.context.chars().count(), 12);
        assert_eq!(lookup.hit.snippet, "");
        assert_eq!(
            lookup.hit.excerpt_sha256,
            evidence_text_sha256("", &lookup.hit.context)
        );

        let read = backend.read(&ReadRequest {
            resource_id: ResourceId::parse("encyclopedia.fixture")?,
            release_id: ReleaseId::parse("fixture-v1")?,
            representation_id: RepresentationId::parse("fixture-sqlite")?,
            purpose: RetrievalPurpose::LocalUi,
            locator: lookup.hit.locator.clone(),
            max_context_chars: 2_000,
            timeout_ms: 5_000,
        })?;
        assert!(read.complete);
        assert!(read.hit.context.contains("microscopic scales"));

        let error = backend
            .read(&ReadRequest {
                resource_id: ResourceId::parse("encyclopedia.fixture")?,
                release_id: ReleaseId::parse("fixture-v1")?,
                representation_id: RepresentationId::parse("fixture-sqlite")?,
                purpose: RetrievalPurpose::LocalUi,
                locator: EvidenceLocator::Record {
                    collection: None,
                    key: "1".to_string(),
                },
                max_context_chars: 2_000,
                timeout_ms: 5_000,
            })
            .expect_err("collection-less read locator must fail");
        assert_eq!(error.code, "unsupported_encyclopedia_collection");
        Ok(())
    }

    #[test]
    fn wikipedia_policy_requires_attribution_and_share_alike() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let mut config = fixture_config(&path)?;
        config.use_policy = UsePolicy {
            local_search: UsePermission::Allowed,
            model_context: UsePermission::Allowed,
            excerpt_export: UsePermission::Allowed,
            redistribution: UsePermission::Allowed,
            attribution_required: false,
        };
        let backend = EncyclopediaBackend::open(config)?;
        let result = backend.lookup(&lookup_request(
            "2",
            RetrievalPurpose::ExcerptExport,
            2_000,
        )?)?;
        assert!(result.hit.use_policy.attribution_required);
        assert_eq!(result.hit.use_policy.excerpt_export, UsePermission::Allowed);
        let rights = result
            .hit
            .rights
            .iter()
            .find(|rights| rights.redistribution == RedistributionPolicy::AllowedWithObligations)
            .ok_or("missing Wikipedia rights")?;
        assert_eq!(
            rights.license_url.as_deref(),
            Some("https://creativecommons.org/licenses/by-sa/4.0/")
        );
        assert!(
            rights
                .attribution
                .as_deref()
                .is_some_and(|value| value.contains("revision 42"))
        );
        assert_eq!(result.hit.metadata.get("origin"), Some(&json!("Wikipedia")));
        result.hit.validate()?;
        Ok(())
    }

    #[test]
    fn model_context_search_skips_denied_candidates_before_allowed_wikipedia_hit()
    -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let mut config = fixture_config(&path)?;
        config.use_policy = UsePolicy {
            local_search: UsePermission::Allowed,
            model_context: UsePermission::Allowed,
            excerpt_export: UsePermission::Allowed,
            redistribution: UsePermission::Allowed,
            attribution_required: false,
        };
        let backend = EncyclopediaBackend::open(config)?;
        let mut query = fixture_query("policy sentinel")?;
        query.syntax = QuerySyntax::ExactPhrase;
        query.purpose = RetrievalPurpose::ModelContext;
        query.budget.max_hits = 2;
        query.budget.max_hits_per_backend = 2;
        query.budget.max_context_chars = 1_000;

        let result = backend.search(&query)?;

        assert!(!result.complete);
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.hits[0].document_id.as_deref(), Some("7"));
        assert_eq!(result.hits[1].document_id.as_deref(), Some("8"));
        assert_eq!(result.hits[0].rank, 1);
        assert_eq!(result.hits[1].rank, 2);
        assert_eq!(
            result.hits[0].metadata.get("origin"),
            Some(&json!("Wikipedia"))
        );
        assert!(
            result
                .hits
                .iter()
                .map(|hit| hit.snippet.chars().count() + hit.context.chars().count())
                .sum::<usize>()
                <= query.budget.max_context_chars as usize
        );
        let warning = result.warnings.join("\n");
        assert!(warning.contains("examined 4 ranked candidates and skipped 2"));
        assert!(warning.contains("1 unknown origin"));
        assert!(warning.contains("1 not permitted for model_context"));
        assert!(warning.contains("returned 2 allowed hits"));

        let copyrighted = backend
            .lookup(&lookup_request("5", RetrievalPurpose::ModelContext, 2_000)?)
            .expect_err("direct copyrighted lookup must remain fail-closed");
        assert_eq!(copyrighted.code, "retrieval_purpose_not_permitted");
        let unknown = backend
            .lookup(&lookup_request("6", RetrievalPurpose::ModelContext, 2_000)?)
            .expect_err("direct unknown-origin lookup must remain fail-closed");
        assert_eq!(unknown.code, "encyclopedia_unknown_origin");
        Ok(())
    }

    #[test]
    fn copyrighted_origin_ceiling_cannot_be_overridden() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let mut config = fixture_config(&path)?;
        config.use_policy = UsePolicy {
            local_search: UsePermission::Allowed,
            model_context: UsePermission::Allowed,
            excerpt_export: UsePermission::Allowed,
            redistribution: UsePermission::Allowed,
            attribution_required: false,
        };
        let backend = EncyclopediaBackend::open(config)?;
        let error = backend
            .lookup(&lookup_request(
                "1",
                RetrievalPurpose::ExcerptExport,
                2_000,
            )?)
            .expect_err("copyrighted article export must fail");
        assert_eq!(error.code, "retrieval_purpose_not_permitted");
        Ok(())
    }

    #[test]
    fn unknown_origin_fails_closed() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let backend = EncyclopediaBackend::open(fixture_config(&path)?)?;
        let error = backend
            .lookup(&lookup_request("3", RetrievalPurpose::LocalUi, 2_000)?)
            .expect_err("unknown origin must fail closed");
        assert_eq!(error.class, ErrorClass::Permission);
        assert_eq!(error.code, "encyclopedia_unknown_origin");
        Ok(())
    }

    #[test]
    fn exact_schema_rejects_extra_article_column() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let connection = Connection::open(&path)?;
        connection.execute_batch("ALTER TABLE articles ADD COLUMN unexpected TEXT")?;
        drop(connection);
        let error = EncyclopediaBackend::open(fixture_config(&path)?)
            .expect_err("schema extension must fail exact validation");
        assert_eq!(error.code, "encyclopedia_profile_column_mismatch");
        Ok(())
    }

    #[test]
    fn filters_are_compiled_bounded_and_bound_as_values() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let backend = EncyclopediaBackend::open(fixture_config(&path)?)?;
        let mut query = fixture_query("quantum")?;
        query
            .filters
            .fields
            .insert("origin".to_string(), "' OR 1=1 --".to_string());
        assert!(backend.search(&query)?.hits.is_empty());

        let mut unsupported = fixture_query("quantum")?;
        unsupported.filters.languages.push("en".to_string());
        let error = backend
            .search(&unsupported)
            .expect_err("language filter must fail");
        assert_eq!(error.code, "encyclopedia_filter_unsupported");

        let mut oversized = fixture_query("quantum")?;
        oversized.filters.document_ids = (1..=129).map(|id| id.to_string()).collect();
        let error = backend
            .search(&oversized)
            .expect_err("compiled filter cap must fail");
        assert_eq!(error.code, "encyclopedia_filter_value_limit_exceeded");
        Ok(())
    }

    #[test]
    fn connection_enforces_query_only_and_trusted_schema_off() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let connection = open_read_only_connection(&path, ExternalAccessMode::LiveReadOnly, 5_000)?;
        let query_only: i64 =
            connection.pragma_query_value(None, "query_only", |row| row.get(0))?;
        let trusted_schema: i64 =
            connection.pragma_query_value(None, "trusted_schema", |row| row.get(0))?;
        assert_eq!(query_only, 1);
        assert_eq!(trusted_schema, 0);
        assert!(
            connection
                .execute("INSERT INTO articles(origin) VALUES ('Wikipedia')", [])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn live_operations_create_no_source_sidecars() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let wal = sidecar_path(&path, "-wal")?;
        let shm = sidecar_path(&path, "-shm")?;
        let journal = sidecar_path(&path, "-journal")?;
        assert!(!wal.exists());
        assert!(!shm.exists());
        assert!(!journal.exists());

        let backend = EncyclopediaBackend::open(fixture_config(&path)?)?;
        assert_eq!(backend.health().status, BackendHealthStatus::Ready);
        let mut query = fixture_query("quantum")?;
        query
            .filters
            .fields
            .insert("origin".to_string(), "Britannica 2015".to_string());
        backend.search(&query)?;
        backend.lookup(&lookup_request("1", RetrievalPurpose::LocalUi, 2_000)?)?;
        drop(backend);

        assert!(!wal.exists());
        assert!(!shm.exists());
        assert!(!journal.exists());
        Ok(())
    }

    #[test]
    fn progress_handler_interrupts_expired_work() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let connection = open_read_only_connection(&path, ExternalAccessMode::LiveReadOnly, 0)?;
        let error = connection
            .query_row(
                "WITH RECURSIVE counter(value) AS (VALUES(1) UNION ALL SELECT value + 1 FROM counter WHERE value < 1000000) SELECT sum(value) FROM counter",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect_err("expired progress deadline must interrupt work");
        let mapped = map_sqlite_error(error);
        assert_eq!(mapped.code, "encyclopedia_query_timeout");
        assert!(mapped.retryable);
        Ok(())
    }

    #[test]
    fn immutable_mode_hashes_source_and_rejects_nonempty_wal() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let mut config = fixture_config(&path)?;
        config.access_mode = ExternalAccessMode::ImmutableReadOnly;
        let backend = EncyclopediaBackend::open(config)?;
        let digest = backend
            .source_identity()
            .sha256
            .ok_or("immutable source identity has no digest")?;
        assert!(digest.starts_with("sha256:"));
        assert_eq!(digest.len(), "sha256:".len() + 64);

        let wal_path = sidecar_path(&path, "-wal")?;
        fs::write(&wal_path, b"not empty")?;
        let live_error = EncyclopediaBackend::open(fixture_config(&path)?)
            .expect_err("live mode must reject a non-empty WAL");
        assert_eq!(live_error.code, "encyclopedia_nonempty_wal");
        let mut rejected = fixture_config(&path)?;
        rejected.access_mode = ExternalAccessMode::ImmutableReadOnly;
        let error = EncyclopediaBackend::open(rejected)
            .expect_err("immutable mode must reject a non-empty WAL");
        assert_eq!(error.code, "encyclopedia_nonempty_wal");
        Ok(())
    }

    #[test]
    fn immutable_mode_rejects_identity_change_after_open() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let mut config = fixture_config(&path)?;
        config.access_mode = ExternalAccessMode::ImmutableReadOnly;
        let backend = EncyclopediaBackend::open(config)?;

        let connection = Connection::open(&path)?;
        connection.execute("UPDATE articles SET edition = 'changed' WHERE id = 1", [])?;
        drop(connection);

        let error = backend
            .lookup(&lookup_request("1", RetrievalPurpose::LocalUi, 2_000)?)
            .expect_err("immutable backend must reject changed file identity");
        assert_eq!(error.code, "immutable_encyclopedia_identity_changed");
        Ok(())
    }

    #[test]
    fn live_fingerprints_are_volatile_not_static_digests() -> Result<(), Box<dyn Error>> {
        let (_directory, path) = create_fixture()?;
        let backend = EncyclopediaBackend::open(fixture_config(&path)?)?;
        let first = backend.source_fingerprint();
        let second = backend.source_fingerprint();
        assert!(first.starts_with("volatile-encyclopedia-reference-v1:"));
        assert!(second.starts_with("volatile-encyclopedia-reference-v1:"));
        assert_ne!(first, second);
        assert!(backend.source_identity().sha256.is_none());
        Ok(())
    }

    #[test]
    #[ignore = "set INFORMATION_NATIVE_ENCYCLOPEDIA_DB to run against a compatible database"]
    fn real_database_read_only_smoke() -> Result<(), Box<dyn Error>> {
        let Some(path) = std::env::var_os("INFORMATION_NATIVE_ENCYCLOPEDIA_DB") else {
            return Ok(());
        };
        let path = Path::new(&path);
        let identity_before = FileIdentity::observe(path)?;
        let backend = EncyclopediaBackend::open(EncyclopediaBackendConfig::new(
            "local-encyclopedia",
            "Local encyclopedia",
            ResourceId::parse("encyclopedia.local")?,
            ReleaseId::parse("local-current")?,
            RepresentationId::parse("encyclopedia-sqlite")?,
            path,
            ExternalAccessMode::LiveReadOnly,
            "Local encyclopedia import",
        ))?;
        let query = InformationQuery {
            schema: QUERY_SCHEMA.to_string(),
            query_id: QueryId::new(),
            text: "quantum mechanics".to_string(),
            syntax: QuerySyntax::ExactPhrase,
            purpose: RetrievalPurpose::LocalUi,
            targets: vec![RetrievalTarget {
                resource_id: ResourceId::parse("encyclopedia.local")?,
                release_id: ReleaseId::parse("local-current")?,
                representation_id: RepresentationId::parse("encyclopedia-sqlite")?,
            }],
            resources: Vec::new(),
            representations: Vec::new(),
            filters: QueryFilters::default(),
            budget: QueryBudget {
                max_hits: 2,
                max_hits_per_backend: 2,
                max_backends: 1,
                max_context_chars: 2_000,
                timeout_ms: 10_000,
            },
        };
        let result = backend.search(&query)?;
        assert!(!result.hits.is_empty());
        assert!(result.hits.iter().all(|hit| {
            matches!(
                hit.locator,
                EvidenceLocator::Record {
                    collection: Some(ref collection),
                    ..
                } if collection == ARTICLE_COLLECTION
            )
        }));
        let first_id = result.hits[0]
            .document_id
            .as_deref()
            .ok_or("real search hit has no article id")?;
        let lookup = LookupRequest {
            resource_id: ResourceId::parse("encyclopedia.local")?,
            release_id: ReleaseId::parse("local-current")?,
            representation_id: RepresentationId::parse("encyclopedia-sqlite")?,
            purpose: RetrievalPurpose::LocalUi,
            collection: Some(ARTICLE_COLLECTION.to_string()),
            key: first_id.to_string(),
            max_context_chars: 4_000,
            timeout_ms: 10_000,
        };
        backend.lookup(&lookup)?.hit.validate()?;
        assert_eq!(FileIdentity::observe(path)?, identity_before);
        Ok(())
    }
}
