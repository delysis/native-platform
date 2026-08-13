#![forbid(unsafe_code)]

//! Compiled, read-only retrieval for Alexandria's Scripture-reference index.
//!
//! The profile is deliberately narrow: it knows only
//! `scripture_passages`, `scripture_citation_occurrences`, and
//! `scripture_sources`. SQL identifiers are not configurable. Search is a
//! bounded literal exact/prefix match over `normalized_reference`; this
//! database has no FTS table, so every returned score is [`ScoreSemantics::RankOnly`].
//! Occurrence context is untrusted source evidence and is never interpreted as
//! an instruction.
//!
//! The default policy permits local UI retrieval only. The source index does
//! not carry rights declarations, so default rights are unknown and excerpt
//! export/redistribution are not granted. A caller may replace both the rights
//! statements and use policy, but the two are validated and intersected before
//! the backend opens.
//!
//! Both access-mode variants use `mode=ro&immutable=1` for each operation so
//! SQLite never creates or participates in WAL/SHM state. Consequently, this
//! backend requires a quiescent source: a non-empty WAL or rollback journal is
//! rejected, and all observed sidecar identities must remain unchanged.

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

pub const OCCURRENCE_COLLECTION: &str = "scripture_citation_occurrences";
pub const PASSAGE_COLLECTION: &str = "scripture_passages";
pub const SCRIPTURE_PROFILE_NAME: &str = "alexandria.scripture-references.v1";

const PROFILE_NAME: &str = SCRIPTURE_PROFILE_NAME;
const MAX_REFERENCE_QUERY_CHARS: usize = 512;
const MAX_MATCHING_PASSAGES: usize = 256;
const MAX_FILTER_VALUES: usize = 128;
const MAX_SQLITE_BINDS: usize = 512;
const MAX_SQLITE_VALUE_BYTES: i32 = 4 * 1024 * 1024;
const MAX_IDENTIFIER_CHARS: usize = 8_192;
const MAX_METADATA_CHARS: usize = 16_384;
const MAX_SNIPPET_CHARS: u32 = 4_096;

static NEXT_BACKEND_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

/// Configuration for the compiled Scripture-reference profile.
#[derive(Debug, Clone)]
pub struct ScriptureBackendConfig {
    pub backend_id: String,
    pub label: String,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub path: PathBuf,
    pub access_mode: ExternalAccessMode,
    pub publisher: String,
    /// Whole-file digest accepted only for immutable mode.
    pub verified_source_sha256: Option<String>,
    /// Registered file identity accepted only for immutable mode. A digest and
    /// modification timestamp are required; Unix also requires device/inode.
    pub verified_source_identity: Option<SourceIdentity>,
    pub rights: Vec<RightsStatement>,
    pub use_policy: UsePolicy,
    pub max_snippet_chars: u32,
}

impl ScriptureBackendConfig {
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
        let publisher = publisher.into();
        Self {
            backend_id: backend_id.into(),
            label: label.into(),
            resource_id,
            release_id,
            representation_id,
            path: path.into(),
            access_mode,
            publisher: publisher.clone(),
            verified_source_sha256: None,
            verified_source_identity: None,
            rights: vec![RightsStatement {
                scope: "Scripture-reference index and cited source excerpts".to_string(),
                expression:
                    "rights are not recorded by bible_refs.db; redistribution is not granted"
                        .to_string(),
                license_url: None,
                license_text_sha256: None,
                attribution: Some(publisher),
                obligations: Vec::new(),
                redistribution: RedistributionPolicy::Unknown,
            }],
            use_policy: UsePolicy {
                local_search: UsePermission::Allowed,
                model_context: UsePermission::Unknown,
                excerpt_export: UsePermission::Forbidden,
                redistribution: UsePermission::Forbidden,
                attribution_required: true,
            },
            max_snippet_chars: 512,
        }
    }

    fn validate(&self) -> Result<(), InformationError> {
        if self.backend_id.trim().is_empty()
            || self.label.trim().is_empty()
            || self.publisher.trim().is_empty()
        {
            return Err(input_error(
                "invalid_scripture_backend_config",
                "backend id, label, and publisher must not be empty",
            ));
        }
        if self.max_snippet_chars == 0 || self.max_snippet_chars > MAX_SNIPPET_CHARS {
            return Err(input_error(
                "invalid_scripture_backend_config",
                "snippet budget is outside the compiled profile bounds",
            ));
        }
        if self.rights.is_empty() {
            return Err(input_error(
                "scripture_rights_required",
                "the Scripture backend requires an explicit rights statement",
            ));
        }
        for statement in &self.rights {
            statement.validate().map_err(|error| {
                input_error(
                    "invalid_scripture_rights_statement",
                    format!("Scripture rights statement is invalid: {error}"),
                )
            })?;
        }
        self.use_policy
            .validate_with_rights(&self.rights)
            .map_err(|error| {
                input_error(
                    "incoherent_scripture_use_policy",
                    format!("Scripture use policy is incoherent with its rights: {error}"),
                )
            })?;
        if let Some(digest) = &self.verified_source_sha256 {
            validate_sha256_value(digest)?;
            if self.access_mode == ExternalAccessMode::LiveReadOnly {
                return Err(input_error(
                    "live_scripture_static_digest_forbidden",
                    "live Scripture indexes cannot advertise a static whole-file digest",
                ));
            }
        }
        if let Some(identity) = &self.verified_source_identity {
            identity.validate().map_err(|error| {
                input_error(
                    "invalid_verified_scripture_identity",
                    format!("verified Scripture identity is invalid: {error}"),
                )
            })?;
            if identity.sha256.is_none() || identity.modified_unix_nanos.is_none() {
                return Err(input_error(
                    "incomplete_verified_scripture_identity",
                    "verified Scripture identity requires a digest and modification timestamp",
                ));
            }
            #[cfg(unix)]
            if identity.device.is_none() || identity.inode.is_none() {
                return Err(input_error(
                    "incomplete_verified_scripture_identity",
                    "verified Scripture identity requires device and inode on Unix",
                ));
            }
            if self.access_mode == ExternalAccessMode::LiveReadOnly {
                return Err(input_error(
                    "live_scripture_static_identity_forbidden",
                    "live Scripture indexes cannot advertise a static verified identity",
                ));
            }
            if self
                .verified_source_sha256
                .as_ref()
                .zip(identity.sha256.as_ref())
                .is_some_and(|(left, right)| normalize_sha256(left) != normalize_sha256(right))
            {
                return Err(input_error(
                    "conflicting_verified_scripture_digest",
                    "verified Scripture digest fields disagree",
                ));
            }
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
                "scripture_source_metadata_failed",
                format!("cannot inspect Scripture index metadata: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(integrity_error(
                "scripture_source_is_symlink",
                "canonical Scripture index unexpectedly resolves to a symbolic link",
            ));
        }
        Self::from_metadata(&metadata)
    }

    fn from_metadata(metadata: &Metadata) -> Result<Self, InformationError> {
        if !metadata.is_file() {
            return Err(input_error(
                "scripture_source_not_file",
                "Scripture index path is not a regular file",
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
                "scripture_sidecar_is_symlink",
                format!("Scripture index {label} sidecar must not be a symbolic link"),
            )),
            Ok(metadata) if !metadata.is_file() => Err(integrity_error(
                "scripture_sidecar_not_file",
                format!("Scripture index {label} sidecar must be a regular file"),
            )),
            Ok(metadata) => Ok(Self::Present(FileIdentity::from_metadata(&metadata)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::Absent),
            Err(error) => Err(io_error(
                "scripture_sidecar_metadata_failed",
                format!("cannot inspect Scripture index {label} sidecar: {error}"),
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

    fn ensure_immutable_safe(&self) -> Result<(), InformationError> {
        if self.wal.is_nonempty() {
            return Err(integrity_error(
                "scripture_nonempty_wal",
                "sidecar-free Scripture retrieval rejects a non-empty sibling WAL",
            ));
        }
        if self.journal.is_nonempty() {
            return Err(integrity_error(
                "scripture_nonempty_journal",
                "sidecar-free Scripture retrieval rejects a non-empty rollback journal",
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
struct SnapshotFacts {
    data_version: i64,
    schema_version: i64,
    user_version: i64,
    application_id: i64,
    page_count: i64,
    freelist_count: i64,
    journal_mode: String,
}

impl SnapshotFacts {
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
        hasher.update(self.data_version.to_le_bytes());
        hasher.update(self.schema_version.to_le_bytes());
        hasher.update(self.user_version.to_le_bytes());
        hasher.update(self.application_id.to_le_bytes());
        hasher.update(self.page_count.to_le_bytes());
        hasher.update(self.freelist_count.to_le_bytes());
        hasher.update(self.journal_mode.as_bytes());
    }
}

/// Read-only backend for the compiled Scripture-reference schema.
pub struct ScriptureBackend {
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

impl ScriptureBackend {
    pub fn open(config: ScriptureBackendConfig) -> Result<Self, InformationError> {
        config.validate()?;
        let path = fs::canonicalize(&config.path).map_err(|error| {
            io_error(
                "scripture_source_canonicalize_failed",
                format!("cannot resolve Scripture index path: {error}"),
            )
        })?;
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
                SidecarSet::observe(&path)?.ensure_immutable_safe()?;
                let observed = hash_immutable_source(&path, &initial_identity)?;
                if expected_sha256
                    .as_ref()
                    .is_some_and(|expected| expected != &observed)
                {
                    return Err(integrity_error(
                        "immutable_scripture_digest_mismatch",
                        "immutable Scripture index does not match its expected SHA-256",
                    ));
                }
                Some(observed)
            }
        };
        let source_uri = Url::from_file_path(&path)
            .map_err(|()| {
                input_error(
                    "scripture_path_not_file_uri",
                    "Scripture index path cannot be represented as a file URI",
                )
            })?
            .to_string();
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

    fn ensure_purpose_allowed(
        &self,
        policy: UsePolicy,
        purpose: RetrievalPurpose,
    ) -> Result<(), InformationError> {
        if policy.permission_for(purpose) != UsePermission::Allowed {
            let mut error = InformationError::new(
                ErrorClass::Permission,
                "retrieval_purpose_not_permitted",
                "the requested retrieval purpose is not explicitly allowed by the Scripture use policy",
            );
            error.resource_id = Some(self.descriptor.resource_id.clone());
            error.representation_id = Some(self.descriptor.representation_id.clone());
            return Err(error);
        }
        Ok(())
    }

    fn next_operation_sequence(&self) -> u64 {
        self.operation_sequence.fetch_add(1, Ordering::Relaxed)
    }

    fn operation_fingerprint(
        &self,
        identity: &FileIdentity,
        sidecars: &SidecarSet,
        facts: &SnapshotFacts,
    ) -> String {
        if let Some(digest) = &self.immutable_source_sha256 {
            return format!("sha256:{digest}");
        }
        let mut hasher = Sha256::new();
        hasher.update(b"information-native.scripture-live-snapshot.v1\0");
        hasher.update(self.instance_id.to_le_bytes());
        hasher.update(self.next_operation_sequence().to_le_bytes());
        identity.update_hasher(&mut hasher);
        sidecars.update_hasher(&mut hasher);
        facts.update_hasher(&mut hasher);
        format!(
            "volatile-scripture-snapshot-v1:{}",
            hex::encode(hasher.finalize())
        )
    }

    fn with_connection<T>(
        &self,
        timeout_ms: u64,
        operation: impl FnOnce(&Connection, &str) -> Result<T, InformationError>,
    ) -> Result<T, InformationError> {
        if timeout_ms == 0 {
            return Err(input_error(
                "invalid_scripture_timeout",
                "Scripture retrieval timeout must be non-zero",
            ));
        }
        let before = FileIdentity::observe(&self.path)?;
        let before_sidecars = SidecarSet::observe(&self.path)?;
        before_sidecars.ensure_immutable_safe()?;
        if self.access_mode == ExternalAccessMode::ImmutableReadOnly
            && before != self.initial_identity
        {
            return Err(integrity_error(
                "immutable_scripture_identity_changed",
                "immutable Scripture index identity changed after registration",
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
                    "scripture_identity_changed_before_read",
                    "Scripture index identity changed while opening a read snapshot",
                ));
            }
            validate_scripture_schema(&connection)?;
            let facts = SnapshotFacts::observe(&connection)?;
            let snapshot_sidecars = SidecarSet::observe(&self.path)?;
            snapshot_sidecars.ensure_immutable_safe()?;
            if snapshot_sidecars != before_sidecars {
                return Err(integrity_error(
                    "scripture_sidecar_changed",
                    "Scripture sidecars changed while opening a read snapshot",
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
                "scripture_identity_changed_during_read",
                "Scripture index identity changed during retrieval; results were discarded",
            ));
        }
        after_sidecars.ensure_immutable_safe()?;
        if before_sidecars != after_sidecars {
            return Err(integrity_error(
                "scripture_sidecar_changed",
                "Scripture sidecars changed during retrieval",
            ));
        }
        result
    }

    fn search_connection(
        &self,
        connection: &Connection,
        index_fingerprint: &str,
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
        let reference = query.text.trim();
        let reference_chars = reference.chars().count();
        if reference_chars == 0 || reference_chars > MAX_REFERENCE_QUERY_CHARS {
            return Err(input_error(
                "invalid_scripture_reference_query",
                format!(
                    "normalized-reference search requires 1 to {MAX_REFERENCE_QUERY_CHARS} characters"
                ),
            ));
        }
        let mode = match query.syntax {
            QuerySyntax::NaturalTerms => ReferenceMatchMode::Prefix,
            QuerySyntax::ExactPhrase => ReferenceMatchMode::Exact,
            QuerySyntax::AllTerms | QuerySyntax::AnyTerms | QuerySyntax::BackendNative => {
                return Err(InformationError::new(
                    ErrorClass::Unsupported,
                    "scripture_query_syntax_unsupported",
                    "Scripture search supports only literal natural_terms prefix and exact_phrase matching over normalized_reference",
                ));
            }
        };
        let mut passages = load_matching_passages(connection, reference, mode)?;
        let passages_truncated = passages.len() > MAX_MATCHING_PASSAGES;
        passages.truncate(MAX_MATCHING_PASSAGES);

        let limit = query.budget.max_hits.min(query.budget.max_hits_per_backend) as usize;
        let fetch_limit = limit.saturating_add(1);
        let per_hit_text_budget = (query.budget.max_context_chars as usize)
            .checked_div(limit)
            .unwrap_or(0);
        let context_fetch_chars = per_hit_text_budget.saturating_add(1);
        let mut raw_hits = Vec::new();
        let mut occurrence_overflow = false;
        for passage in passages {
            if raw_hits.len() >= fetch_limit {
                occurrence_overflow = true;
                break;
            }
            let remaining = fetch_limit.saturating_sub(raw_hits.len());
            let mut occurrences = load_occurrences_for_passage(
                connection,
                passage,
                &query.filters.document_ids,
                remaining,
                context_fetch_chars,
            )?;
            if occurrences.len() == remaining
                && raw_hits.len().saturating_add(remaining) >= fetch_limit
            {
                occurrence_overflow = true;
            }
            raw_hits.append(&mut occurrences);
        }
        let hits_truncated = raw_hits.len() > limit;
        raw_hits.truncate(limit);

        let mut complete = !(passages_truncated || hits_truncated || occurrence_overflow);
        let mut warnings = vec![
            "normalized-reference search uses bounded literal SQL matching without FTS; ranks are deterministic and not relevance scores"
                .to_string(),
        ];
        if passages_truncated {
            warnings.push(format!(
                "normalized-reference matches were capped at {MAX_MATCHING_PASSAGES} passages"
            ));
        }
        if hits_truncated || occurrence_overflow {
            warnings.push("the per-backend hit budget omitted additional occurrences".to_string());
        }

        let mut hits = Vec::with_capacity(raw_hits.len());
        let mut remaining_chars = query.budget.max_context_chars as usize;
        let raw_count = raw_hits.len();
        for (index, raw) in raw_hits.into_iter().enumerate() {
            let remaining_hits = raw_count.saturating_sub(index).max(1);
            let share = remaining_chars / remaining_hits;
            let (snippet, snippet_truncated) = truncate_chars(
                &raw.raw_reference,
                share.min(self.max_snippet_chars as usize),
            );
            let context_budget = share.saturating_sub(snippet.chars().count());
            let (context, context_truncated) = truncate_chars(
                raw.context_text.as_deref().unwrap_or_default(),
                context_budget,
            );
            remaining_chars = remaining_chars.saturating_sub(
                snippet
                    .chars()
                    .count()
                    .saturating_add(context.chars().count()),
            );
            if snippet_truncated || context_truncated || raw.metadata_truncated {
                complete = false;
            }
            let rank = u32::try_from(index.saturating_add(1)).map_err(|_| {
                integrity_error(
                    "scripture_rank_overflow",
                    "Scripture result rank exceeded supported bounds",
                )
            })?;
            hits.push(self.occurrence_evidence(
                raw,
                snippet,
                context,
                rank,
                index_fingerprint,
                query.purpose,
            )?);
        }
        if !complete {
            warnings.push(
                "one or more result, text, or metadata bounds made this response partial"
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
        index_fingerprint: &str,
        request: &ReadRequest,
    ) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        validate_direct_read_request(
            &self.descriptor,
            &request.resource_id,
            &request.release_id,
            &request.representation_id,
            request.max_context_chars,
            request.timeout_ms,
        )?;
        let EvidenceLocator::Record { collection, key } = &request.locator else {
            return Err(InformationError::new(
                ErrorClass::Unsupported,
                "scripture_locator_unsupported",
                "Scripture article reads require a record locator",
            ));
        };
        self.read_record(
            connection,
            index_fingerprint,
            collection.as_deref(),
            key,
            request.max_context_chars,
            request.purpose,
        )
    }

    fn lookup_connection(
        &self,
        connection: &Connection,
        index_fingerprint: &str,
        request: &LookupRequest,
    ) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        validate_direct_read_request(
            &self.descriptor,
            &request.resource_id,
            &request.release_id,
            &request.representation_id,
            request.max_context_chars,
            request.timeout_ms,
        )?;
        self.read_record(
            connection,
            index_fingerprint,
            request.collection.as_deref(),
            &request.key,
            request.max_context_chars,
            request.purpose,
        )
    }

    fn read_record(
        &self,
        connection: &Connection,
        index_fingerprint: &str,
        collection: Option<&str>,
        key: &str,
        max_context_chars: u32,
        purpose: RetrievalPurpose,
    ) -> Result<BackendReadResult, InformationError> {
        let started = Instant::now();
        match collection {
            None | Some("occurrence") | Some("occurrences") | Some(OCCURRENCE_COLLECTION) => {
                let occurrence_id = parse_positive_record_id(key, "occurrence")?;
                let raw = load_occurrence_by_id(
                    connection,
                    occurrence_id,
                    max_context_chars as usize + 1,
                )?;
                let snippet_budget =
                    (max_context_chars as usize).min(self.max_snippet_chars as usize);
                let (snippet, snippet_truncated) =
                    truncate_chars(&raw.raw_reference, snippet_budget);
                let context_budget =
                    (max_context_chars as usize).saturating_sub(snippet.chars().count());
                let (context, context_truncated) = truncate_chars(
                    raw.context_text.as_deref().unwrap_or_default(),
                    context_budget,
                );
                let complete = !(snippet_truncated || context_truncated || raw.metadata_truncated);
                let hit =
                    self.occurrence_evidence(raw, snippet, context, 1, index_fingerprint, purpose)?;
                Ok(BackendReadResult {
                    complete,
                    warnings: if complete {
                        Vec::new()
                    } else {
                        vec![
                            "occurrence text or metadata was truncated to its read budget"
                                .to_string(),
                        ]
                    },
                    hit,
                    elapsed_ms: elapsed_millis(started),
                })
            }
            Some("passage") | Some("passages") | Some(PASSAGE_COLLECTION) => {
                let passage_id = parse_positive_record_id(key, "passage")?;
                let passage = load_passage_by_id(connection, passage_id)?;
                let (hit, complete) = self.passage_evidence(
                    passage,
                    max_context_chars as usize,
                    index_fingerprint,
                    purpose,
                )?;
                Ok(BackendReadResult {
                    complete,
                    warnings: if complete {
                        Vec::new()
                    } else {
                        vec!["passage reference was truncated to its read budget".to_string()]
                    },
                    hit,
                    elapsed_ms: elapsed_millis(started),
                })
            }
            Some(_) => Err(InformationError::new(
                ErrorClass::Unsupported,
                "scripture_collection_unsupported",
                format!(
                    "Scripture lookup supports only {OCCURRENCE_COLLECTION} and {PASSAGE_COLLECTION}"
                ),
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn occurrence_evidence(
        &self,
        raw: OccurrenceRecord,
        snippet: String,
        context: String,
        rank: u32,
        index_fingerprint: &str,
        purpose: RetrievalPurpose,
    ) -> Result<EvidenceHit, InformationError> {
        raw.validate()?;
        self.ensure_purpose_allowed(self.use_policy, purpose)?;
        let ambiguity_flags = parse_json_array(
            &raw.ambiguity_flags_json,
            "scripture_invalid_ambiguity_flags",
        )?;
        let ambiguity_evidence = raw
            .ambiguity_evidence_json
            .as_deref()
            .map(|value| parse_json(value, "scripture_invalid_ambiguity_evidence"))
            .transpose()?;
        let effective_doc_id = raw
            .alexandria_doc_id
            .clone()
            .or_else(|| raw.source_alexandria_doc_id.clone());
        let source_content_fingerprint = raw
            .content_fingerprint
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string);
        let mut source_inputs = vec![format!("scripture-index-{index_fingerprint}")];
        if let Some(fingerprint) = &source_content_fingerprint {
            source_inputs.push(format!("source-content-{fingerprint}"));
        }
        let excerpt_sha256 = evidence_text_sha256(&snippet, &context);
        let title = raw
            .source_title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| raw.source_uri.clone());
        let creator = raw
            .source_author
            .clone()
            .filter(|value| !value.trim().is_empty());
        let mut metadata = BTreeMap::from([
            ("profile".to_string(), json!(PROFILE_NAME)),
            ("record_kind".to_string(), json!("scripture_occurrence")),
            ("occurrence_id".to_string(), json!(raw.occurrence_id)),
            ("source_id".to_string(), json!(raw.source_id)),
            ("passage_id".to_string(), json!(raw.passage.passage_id)),
            ("occurrence_key".to_string(), json!(raw.occurrence_key)),
            ("raw_reference".to_string(), json!(raw.raw_reference)),
            (
                "normalized_reference".to_string(),
                json!(raw.passage.normalized_reference),
            ),
            ("book_key".to_string(), json!(raw.passage.book_key)),
            ("book_name".to_string(), json!(raw.passage.book_name)),
            ("book_ordinal".to_string(), json!(raw.passage.book_ordinal)),
            (
                "start_chapter".to_string(),
                json!(raw.passage.start_chapter),
            ),
            ("start_verse".to_string(), json!(raw.passage.start_verse)),
            ("end_chapter".to_string(), json!(raw.passage.end_chapter)),
            ("end_verse".to_string(), json!(raw.passage.end_verse)),
            ("confidence".to_string(), json!(raw.confidence)),
            (
                "occurrence_parser_version".to_string(),
                json!(raw.occurrence_parser_version),
            ),
            ("detection_method".to_string(), json!(raw.detection_method)),
            ("detection_tier".to_string(), json!(raw.detection_tier)),
            ("review_status".to_string(), json!(raw.review_status)),
            ("ambiguity_flags".to_string(), ambiguity_flags),
            (
                "ambiguity_evidence".to_string(),
                ambiguity_evidence.unwrap_or(JsonValue::Null),
            ),
            (
                "alexandria_block_id".to_string(),
                json!(raw.alexandria_block_id),
            ),
            (
                "alexandria_doc_id".to_string(),
                json!(raw.alexandria_doc_id),
            ),
            (
                "source_alexandria_doc_id".to_string(),
                json!(raw.source_alexandria_doc_id),
            ),
            ("location_path".to_string(), json!(raw.location_path)),
            ("chapter_title".to_string(), json!(raw.chapter_title)),
            ("reference_start".to_string(), json!(raw.reference_start)),
            ("reference_end".to_string(), json!(raw.reference_end)),
            ("rebuild_token".to_string(), json!(raw.rebuild_token)),
            ("created_at".to_string(), json!(raw.created_at)),
            ("updated_at".to_string(), json!(raw.updated_at)),
            ("source_kind".to_string(), json!(raw.source_kind)),
            (
                "source_content_fingerprint".to_string(),
                json!(source_content_fingerprint),
            ),
            (
                "source_parser_version".to_string(),
                json!(raw.source_parser_version),
            ),
            (
                "source_first_indexed_at".to_string(),
                json!(raw.first_indexed_at),
            ),
            (
                "source_last_indexed_at".to_string(),
                json!(raw.last_indexed_at),
            ),
            (
                "index_snapshot_fingerprint".to_string(),
                json!(index_fingerprint),
            ),
            (
                "excerpt_hash_basis".to_string(),
                json!(
                    "information_native_types::evidence_text_sha256 over exact snippet and context"
                ),
            ),
            (
                "metadata_truncated".to_string(),
                json!(raw.metadata_truncated),
            ),
        ]);
        metadata.insert(
            "source_content_fingerprint_present".to_string(),
            json!(source_content_fingerprint.is_some()),
        );
        let evidence_id = format!(
            "{}:{}:{}:occurrence:{}",
            self.descriptor.resource_id,
            self.descriptor.release_id,
            self.descriptor.representation_id,
            raw.occurrence_id
        );
        let hit = EvidenceHit {
            evidence_id,
            resource_id: self.descriptor.resource_id.clone(),
            release_id: self.descriptor.release_id.clone(),
            representation_id: self.descriptor.representation_id.clone(),
            backend_id: self.descriptor.backend_id.clone(),
            rank,
            score: EvidenceScore {
                value: f64::from(rank),
                semantics: ScoreSemantics::RankOnly,
                fused_value: None,
            },
            title,
            creator,
            snippet,
            context,
            excerpt_sha256,
            source_fingerprint: source_content_fingerprint,
            document_id: effective_doc_id,
            passage_id: Some(raw.passage.passage_id.to_string()),
            locator: EvidenceLocator::Record {
                collection: Some(OCCURRENCE_COLLECTION.to_string()),
                key: raw.occurrence_id.to_string(),
            },
            source_uri: Some(raw.source_uri.clone()),
            provenance: Provenance {
                publisher: self.publisher.clone(),
                source_uri: raw.source_uri,
                upstream_record_id: Some(raw.occurrence_key),
                source_inputs,
                transformation: Some(
                    "read-only join of compiled Scripture passage, occurrence, and source tables"
                        .to_string(),
                ),
                metadata: BTreeMap::from([
                    ("profile".to_string(), json!(PROFILE_NAME)),
                    ("index_source_uri".to_string(), json!(self.source_uri)),
                    (
                        "index_snapshot_fingerprint".to_string(),
                        json!(index_fingerprint),
                    ),
                    (
                        "content_fingerprint".to_string(),
                        json!(raw.content_fingerprint),
                    ),
                ]),
            },
            rights: self.rights.clone(),
            use_policy: self.use_policy,
            metadata,
        };
        hit.validate().map_err(|error| {
            integrity_error(
                "invalid_scripture_evidence",
                format!("generated Scripture evidence failed validation: {error}"),
            )
        })?;
        Ok(hit)
    }

    fn passage_evidence(
        &self,
        passage: PassageRecord,
        max_context_chars: usize,
        index_fingerprint: &str,
        purpose: RetrievalPurpose,
    ) -> Result<(EvidenceHit, bool), InformationError> {
        passage.validate()?;
        self.ensure_purpose_allowed(self.use_policy, purpose)?;
        let snippet_budget = max_context_chars.min(self.max_snippet_chars as usize);
        let (snippet, truncated) = truncate_chars(&passage.normalized_reference, snippet_budget);
        let context = String::new();
        let hit = EvidenceHit {
            evidence_id: format!(
                "{}:{}:{}:passage:{}",
                self.descriptor.resource_id,
                self.descriptor.release_id,
                self.descriptor.representation_id,
                passage.passage_id
            ),
            resource_id: self.descriptor.resource_id.clone(),
            release_id: self.descriptor.release_id.clone(),
            representation_id: self.descriptor.representation_id.clone(),
            backend_id: self.descriptor.backend_id.clone(),
            rank: 1,
            score: EvidenceScore {
                value: 1.0,
                semantics: ScoreSemantics::RankOnly,
                fused_value: None,
            },
            title: format!("{} — {}", passage.book_name, passage.normalized_reference),
            creator: None,
            excerpt_sha256: evidence_text_sha256(&snippet, &context),
            snippet,
            context,
            source_fingerprint: Some(index_fingerprint.to_string()),
            document_id: None,
            passage_id: Some(passage.passage_id.to_string()),
            locator: EvidenceLocator::Record {
                collection: Some(PASSAGE_COLLECTION.to_string()),
                key: passage.passage_id.to_string(),
            },
            source_uri: Some(self.source_uri.clone()),
            provenance: Provenance {
                publisher: self.publisher.clone(),
                source_uri: self.source_uri.clone(),
                upstream_record_id: Some(format!("passage:{}", passage.passage_id)),
                source_inputs: vec![format!("scripture-index-{index_fingerprint}")],
                transformation: Some(
                    "read-only canonical passage record from scripture_passages".to_string(),
                ),
                metadata: BTreeMap::from([
                    ("profile".to_string(), json!(PROFILE_NAME)),
                    (
                        "index_snapshot_fingerprint".to_string(),
                        json!(index_fingerprint),
                    ),
                ]),
            },
            rights: self.rights.clone(),
            use_policy: self.use_policy,
            metadata: BTreeMap::from([
                ("profile".to_string(), json!(PROFILE_NAME)),
                ("record_kind".to_string(), json!("scripture_passage")),
                ("passage_id".to_string(), json!(passage.passage_id)),
                ("book_key".to_string(), json!(passage.book_key)),
                ("book_name".to_string(), json!(passage.book_name)),
                ("book_ordinal".to_string(), json!(passage.book_ordinal)),
                ("start_chapter".to_string(), json!(passage.start_chapter)),
                ("start_verse".to_string(), json!(passage.start_verse)),
                ("end_chapter".to_string(), json!(passage.end_chapter)),
                ("end_verse".to_string(), json!(passage.end_verse)),
                (
                    "normalized_reference".to_string(),
                    json!(passage.normalized_reference),
                ),
                (
                    "index_snapshot_fingerprint".to_string(),
                    json!(index_fingerprint),
                ),
            ]),
        };
        hit.validate().map_err(|error| {
            integrity_error(
                "invalid_scripture_evidence",
                format!("generated passage evidence failed validation: {error}"),
            )
        })?;
        Ok((hit, !truncated))
    }
}

impl ResourceBackend for ScriptureBackend {
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
        self.with_connection(query.budget.timeout_ms, |connection, fingerprint| {
            self.search_connection(connection, fingerprint, query)
        })
    }

    fn read(&self, request: &ReadRequest) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        self.with_connection(request.timeout_ms, |connection, fingerprint| {
            self.read_connection(connection, fingerprint, request)
        })
    }

    fn lookup(&self, request: &LookupRequest) -> Result<BackendReadResult, InformationError> {
        self.ensure_purpose_allowed(self.use_policy, request.purpose)?;
        self.with_connection(request.timeout_ms, |connection, fingerprint| {
            self.lookup_connection(connection, fingerprint, request)
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum ReferenceMatchMode {
    Exact,
    Prefix,
}

#[derive(Debug, Clone)]
struct PassageRecord {
    passage_id: i64,
    book_key: String,
    book_name: String,
    book_ordinal: i64,
    start_chapter: i64,
    start_verse: i64,
    end_chapter: i64,
    end_verse: i64,
    normalized_reference: String,
}

impl PassageRecord {
    fn validate(&self) -> Result<(), InformationError> {
        let valid_range = self.end_chapter > self.start_chapter
            || (self.end_chapter == self.start_chapter && self.end_verse >= self.start_verse);
        let valid_verse_shape = (self.start_verse == 0 && self.end_verse == 0)
            || (self.start_verse > 0 && self.end_verse > 0);
        if self.passage_id <= 0
            || self.book_key.trim().is_empty()
            || self.book_name.trim().is_empty()
            || self.normalized_reference.trim().is_empty()
            || self.book_ordinal <= 0
            || self.start_chapter <= 0
            || self.end_chapter <= 0
            || self.start_verse < 0
            || self.end_verse < 0
            || !valid_range
            || !valid_verse_shape
        {
            return Err(integrity_error(
                "invalid_scripture_passage_row",
                "Scripture passage row violates the compiled profile invariants",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct OccurrenceRecord {
    occurrence_id: i64,
    source_id: i64,
    occurrence_key: String,
    raw_reference: String,
    confidence: f64,
    occurrence_parser_version: String,
    detection_method: String,
    detection_tier: i64,
    review_status: String,
    ambiguity_flags_json: String,
    ambiguity_evidence_json: Option<String>,
    alexandria_block_id: Option<String>,
    alexandria_doc_id: Option<String>,
    location_path: Option<String>,
    chapter_title: Option<String>,
    context_text: Option<String>,
    reference_start: Option<i64>,
    reference_end: Option<i64>,
    rebuild_token: String,
    created_at: String,
    updated_at: String,
    source_uri: String,
    source_kind: String,
    source_title: Option<String>,
    source_author: Option<String>,
    content_fingerprint: Option<String>,
    source_alexandria_doc_id: Option<String>,
    source_parser_version: String,
    first_indexed_at: String,
    last_indexed_at: String,
    passage: PassageRecord,
    metadata_truncated: bool,
}

impl OccurrenceRecord {
    fn validate(&self) -> Result<(), InformationError> {
        self.passage.validate()?;
        let detection_valid = matches!(
            (self.detection_method.as_str(), self.detection_tier),
            ("explicit_syntax", 1) | ("quotation_alignment", 2) | ("semantic_allusion", 3)
        );
        let review_valid = matches!(
            self.review_status.as_str(),
            "unreviewed" | "accepted" | "needs_review" | "rejected"
        );
        let span_valid = match (self.reference_start, self.reference_end) {
            (None, None) => true,
            (Some(start), Some(end)) => start >= 0 && end >= start,
            _ => false,
        };
        if self.occurrence_id <= 0
            || self.source_id <= 0
            || self.occurrence_key.trim().is_empty()
            || self.raw_reference.trim().is_empty()
            || !self.confidence.is_finite()
            || !(0.0..=1.0).contains(&self.confidence)
            || self.occurrence_parser_version.trim().is_empty()
            || self.rebuild_token.trim().is_empty()
            || self.created_at.trim().is_empty()
            || self.updated_at.trim().is_empty()
            || self.source_uri.trim().is_empty()
            || self.source_kind.trim().is_empty()
            || self.source_parser_version.trim().is_empty()
            || self.first_indexed_at.trim().is_empty()
            || self.last_indexed_at.trim().is_empty()
            || !detection_valid
            || !review_valid
            || !span_valid
        {
            return Err(integrity_error(
                "invalid_scripture_occurrence_row",
                "Scripture occurrence/source row violates the compiled profile invariants",
            ));
        }
        for value in [
            Some(self.occurrence_key.as_str()),
            self.alexandria_block_id.as_deref(),
            self.alexandria_doc_id.as_deref(),
            self.source_alexandria_doc_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.chars().count() > MAX_IDENTIFIER_CHARS {
                return Err(integrity_error(
                    "oversized_scripture_identifier",
                    "Scripture occurrence contains an oversized identifier",
                ));
            }
        }
        Ok(())
    }
}

fn load_matching_passages(
    connection: &Connection,
    reference: &str,
    mode: ReferenceMatchMode,
) -> Result<Vec<PassageRecord>, InformationError> {
    let limit = i64::try_from(MAX_MATCHING_PASSAGES.saturating_add(1)).map_err(|_| {
        integrity_error(
            "scripture_passage_limit_overflow",
            "compiled passage candidate limit exceeds SQLite bounds",
        )
    })?;
    let (sql, pattern) = match mode {
        ReferenceMatchMode::Exact => (
            r#"
            SELECT passage_id, book_key, book_name, book_ordinal,
                   start_chapter, start_verse, end_chapter, end_verse,
                   normalized_reference
            FROM scripture_passages
            WHERE normalized_reference = ?1 COLLATE NOCASE
            ORDER BY book_ordinal, start_chapter, start_verse,
                     end_chapter, end_verse, passage_id
            LIMIT ?2
            "#,
            reference.to_string(),
        ),
        ReferenceMatchMode::Prefix => (
            r#"
            SELECT passage_id, book_key, book_name, book_ordinal,
                   start_chapter, start_verse, end_chapter, end_verse,
                   normalized_reference
            FROM scripture_passages
            WHERE normalized_reference LIKE ?1 ESCAPE '\' COLLATE NOCASE
            ORDER BY CASE WHEN normalized_reference = ?2 COLLATE NOCASE THEN 0 ELSE 1 END,
                     book_ordinal, start_chapter, start_verse,
                     end_chapter, end_verse, passage_id
            LIMIT ?3
            "#,
            format!("{}%", escape_like_literal(reference)),
        ),
    };
    let mut statement = connection.prepare(sql).map_err(map_sqlite_error)?;
    let rows = match mode {
        ReferenceMatchMode::Exact => statement
            .query_map(params![pattern, limit], passage_from_row)
            .map_err(map_sqlite_error)?,
        ReferenceMatchMode::Prefix => statement
            .query_map(params![pattern, reference, limit], passage_from_row)
            .map_err(map_sqlite_error)?,
    };
    let passages = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_sqlite_error)?;
    for passage in &passages {
        passage.validate()?;
    }
    Ok(passages)
}

fn passage_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PassageRecord> {
    Ok(PassageRecord {
        passage_id: row.get(0)?,
        book_key: row.get(1)?,
        book_name: row.get(2)?,
        book_ordinal: row.get(3)?,
        start_chapter: row.get(4)?,
        start_verse: row.get(5)?,
        end_chapter: row.get(6)?,
        end_verse: row.get(7)?,
        normalized_reference: row.get(8)?,
    })
}

fn load_passage_by_id(
    connection: &Connection,
    passage_id: i64,
) -> Result<PassageRecord, InformationError> {
    let passage = connection
        .query_row(
            r#"
            SELECT passage_id, book_key, book_name, book_ordinal,
                   start_chapter, start_verse, end_chapter, end_verse,
                   normalized_reference
            FROM scripture_passages
            WHERE passage_id = ?1
            "#,
            [passage_id],
            passage_from_row,
        )
        .optional()
        .map_err(map_sqlite_error)?
        .ok_or_else(|| {
            not_found_error(
                "scripture_passage_not_found",
                "Scripture passage was not found",
            )
        })?;
    passage.validate()?;
    Ok(passage)
}

fn load_occurrences_for_passage(
    connection: &Connection,
    passage: PassageRecord,
    document_ids: &[String],
    limit: usize,
    context_chars: usize,
) -> Result<Vec<OccurrenceRecord>, InformationError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut sql = occurrence_select_sql();
    sql.push_str("\nWHERE o.passage_id = ?");
    let context_limit = sqlite_substr_limit(context_chars)?;
    let mut values = vec![
        SqlValue::Integer(context_limit),
        SqlValue::Integer(passage.passage_id),
    ];
    if !document_ids.is_empty() {
        sql.push_str(&format!(
            "\nAND COALESCE(o.alexandria_doc_id, s.alexandria_doc_id) IN ({})",
            placeholders(document_ids.len())
        ));
        values.extend(document_ids.iter().cloned().map(SqlValue::Text));
    }
    sql.push_str("\nORDER BY o.occurrence_id LIMIT ?");
    values.push(SqlValue::Integer(i64::try_from(limit).map_err(|_| {
        input_error(
            "scripture_hit_limit_overflow",
            "Scripture hit limit exceeds SQLite bounds",
        )
    })?));
    let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(params_from_iter(values.iter()), |row| {
            occurrence_from_row(row, passage.clone(), context_chars)
        })
        .map_err(map_sqlite_error)?;
    let occurrences = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_sqlite_error)?;
    for occurrence in &occurrences {
        occurrence.validate()?;
    }
    Ok(occurrences)
}

fn load_occurrence_by_id(
    connection: &Connection,
    occurrence_id: i64,
    context_chars: usize,
) -> Result<OccurrenceRecord, InformationError> {
    let mut sql = occurrence_select_sql();
    sql.push_str("\nWHERE o.occurrence_id = ?2");
    let context_limit = sqlite_substr_limit(context_chars)?;
    let occurrence = connection
        .query_row(&sql, params![context_limit, occurrence_id], |row| {
            let passage = passage_from_occurrence_row(row)?;
            occurrence_from_row(row, passage, context_chars)
        })
        .optional()
        .map_err(map_sqlite_error)?
        .ok_or_else(|| {
            not_found_error(
                "scripture_occurrence_not_found",
                "Scripture occurrence was not found",
            )
        })?;
    occurrence.validate()?;
    Ok(occurrence)
}

fn occurrence_select_sql() -> String {
    r#"
        SELECT
            substr(o.occurrence_key, 1, 8193),
            substr(o.raw_reference, 1, 16385),
            o.confidence,
            substr(o.parser_version, 1, 16385),
            substr(o.detection_method, 1, 128),
            o.detection_tier,
            substr(o.review_status, 1, 128),
            substr(o.ambiguity_flags_json, 1, 16385),
            substr(o.ambiguity_evidence_json, 1, 16385),
            substr(o.alexandria_block_id, 1, 8193),
            substr(o.alexandria_doc_id, 1, 8193),
            substr(o.location_path, 1, 16385),
            substr(o.chapter_title, 1, 16385),
            substr(o.context_text, 1, ?1),
            o.reference_start,
            o.reference_end,
            substr(o.rebuild_token, 1, 16385),
            substr(o.created_at, 1, 256),
            substr(o.updated_at, 1, 256),
            o.occurrence_id,
            o.source_id,
            o.passage_id,
            substr(s.source_uri, 1, 16385),
            substr(s.source_kind, 1, 16385),
            substr(s.source_title, 1, 16385),
            substr(s.source_author, 1, 16385),
            substr(s.content_fingerprint, 1, 16385),
            substr(s.alexandria_doc_id, 1, 8193),
            substr(s.parser_version, 1, 16385),
            substr(s.first_indexed_at, 1, 256),
            substr(s.last_indexed_at, 1, 256),
            p.passage_id,
            substr(p.book_key, 1, 16385),
            substr(p.book_name, 1, 16385),
            p.book_ordinal,
            p.start_chapter,
            p.start_verse,
            p.end_chapter,
            p.end_verse,
            substr(p.normalized_reference, 1, 16385)
        FROM scripture_citation_occurrences o
        JOIN scripture_sources s ON s.source_id = o.source_id
        JOIN scripture_passages p ON p.passage_id = o.passage_id
    "#
    .to_string()
}

fn passage_from_occurrence_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PassageRecord> {
    Ok(PassageRecord {
        passage_id: row.get(31)?,
        book_key: row.get(32)?,
        book_name: row.get(33)?,
        book_ordinal: row.get(34)?,
        start_chapter: row.get(35)?,
        start_verse: row.get(36)?,
        end_chapter: row.get(37)?,
        end_verse: row.get(38)?,
        normalized_reference: row.get(39)?,
    })
}

fn occurrence_from_row(
    row: &rusqlite::Row<'_>,
    passage: PassageRecord,
    context_chars: usize,
) -> rusqlite::Result<OccurrenceRecord> {
    let occurrence_passage_id: i64 = row.get(21)?;
    if occurrence_passage_id != passage.passage_id {
        return Err(rusqlite::Error::IntegralValueOutOfRange(
            21,
            occurrence_passage_id,
        ));
    }
    let mut metadata_truncated = false;
    let occurrence_key =
        bounded_required(row.get(0)?, MAX_IDENTIFIER_CHARS, &mut metadata_truncated);
    let raw_reference = bounded_required(row.get(1)?, MAX_METADATA_CHARS, &mut metadata_truncated);
    let ambiguity_flags_json =
        bounded_required(row.get(7)?, MAX_METADATA_CHARS, &mut metadata_truncated);
    let ambiguity_evidence_json =
        bounded_optional(row.get(8)?, MAX_METADATA_CHARS, &mut metadata_truncated);
    let context_text = bounded_optional(row.get(13)?, context_chars, &mut metadata_truncated);
    Ok(OccurrenceRecord {
        occurrence_id: row.get(19)?,
        source_id: row.get(20)?,
        occurrence_key,
        raw_reference,
        confidence: row.get(2)?,
        occurrence_parser_version: bounded_required(
            row.get(3)?,
            MAX_METADATA_CHARS,
            &mut metadata_truncated,
        ),
        detection_method: row.get(4)?,
        detection_tier: row.get(5)?,
        review_status: row.get(6)?,
        ambiguity_flags_json,
        ambiguity_evidence_json,
        alexandria_block_id: bounded_optional(
            row.get(9)?,
            MAX_IDENTIFIER_CHARS,
            &mut metadata_truncated,
        ),
        alexandria_doc_id: bounded_optional(
            row.get(10)?,
            MAX_IDENTIFIER_CHARS,
            &mut metadata_truncated,
        ),
        location_path: bounded_optional(row.get(11)?, MAX_METADATA_CHARS, &mut metadata_truncated),
        chapter_title: bounded_optional(row.get(12)?, MAX_METADATA_CHARS, &mut metadata_truncated),
        context_text,
        reference_start: row.get(14)?,
        reference_end: row.get(15)?,
        rebuild_token: bounded_required(row.get(16)?, MAX_METADATA_CHARS, &mut metadata_truncated),
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
        source_uri: bounded_required(row.get(22)?, MAX_METADATA_CHARS, &mut metadata_truncated),
        source_kind: bounded_required(row.get(23)?, MAX_METADATA_CHARS, &mut metadata_truncated),
        source_title: bounded_optional(row.get(24)?, MAX_METADATA_CHARS, &mut metadata_truncated),
        source_author: bounded_optional(row.get(25)?, MAX_METADATA_CHARS, &mut metadata_truncated),
        content_fingerprint: bounded_optional(
            row.get(26)?,
            MAX_METADATA_CHARS,
            &mut metadata_truncated,
        ),
        source_alexandria_doc_id: bounded_optional(
            row.get(27)?,
            MAX_IDENTIFIER_CHARS,
            &mut metadata_truncated,
        ),
        source_parser_version: bounded_required(
            row.get(28)?,
            MAX_METADATA_CHARS,
            &mut metadata_truncated,
        ),
        first_indexed_at: row.get(29)?,
        last_indexed_at: row.get(30)?,
        passage,
        metadata_truncated,
    })
}

fn bounded_required(value: String, limit: usize, truncated: &mut bool) -> String {
    let (value, was_truncated) = truncate_chars(&value, limit);
    *truncated |= was_truncated;
    value
}

fn bounded_optional(value: Option<String>, limit: usize, truncated: &mut bool) -> Option<String> {
    value.map(|value| bounded_required(value, limit, truncated))
}

fn sqlite_substr_limit(chars: usize) -> Result<i64, InformationError> {
    i64::try_from(chars.saturating_add(1)).map_err(|_| {
        input_error(
            "scripture_context_budget_overflow",
            "Scripture context budget exceeds SQLite bounds",
        )
    })
}

fn escape_like_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
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
            "query exact targets exclude this Scripture backend",
        ));
    }
    if !query.resources.is_empty() && !query.resources.contains(&descriptor.resource_id) {
        return Err(input_error(
            "query_resource_mismatch",
            "query resource selection excludes this Scripture backend",
        ));
    }
    if !query.representations.is_empty()
        && !query
            .representations
            .contains(&descriptor.representation_id)
    {
        return Err(input_error(
            "query_representation_mismatch",
            "query representation selection excludes this Scripture backend",
        ));
    }
    if !query.filters.languages.is_empty()
        || !query.filters.subjects.is_empty()
        || query.filters.spatial.is_some()
        || query.filters.temporal_start.is_some()
        || query.filters.temporal_end.is_some()
        || !query.filters.fields.is_empty()
    {
        return Err(InformationError::new(
            ErrorClass::Unsupported,
            "scripture_filter_unsupported",
            "the compiled Scripture profile supports only document_ids filtering",
        ));
    }
    if query.filters.document_ids.len() > MAX_FILTER_VALUES {
        return Err(input_error(
            "scripture_filter_value_limit_exceeded",
            format!("Scripture document_ids filtering is capped at {MAX_FILTER_VALUES} values"),
        ));
    }
    let bind_count = query
        .filters
        .document_ids
        .len()
        .checked_add(4)
        .ok_or_else(|| {
            input_error(
                "scripture_filter_bind_overflow",
                "Scripture bind accounting overflowed",
            )
        })?;
    if bind_count > MAX_SQLITE_BINDS {
        return Err(input_error(
            "scripture_filter_bind_limit_exceeded",
            "Scripture query exceeds the compiled SQLite bind limit",
        ));
    }
    Ok(())
}

fn validate_direct_read_request(
    descriptor: &BackendDescriptor,
    resource_id: &ResourceId,
    release_id: &ReleaseId,
    representation_id: &RepresentationId,
    max_context_chars: u32,
    timeout_ms: u64,
) -> Result<(), InformationError> {
    if descriptor.resource_id != *resource_id
        || descriptor.release_id != *release_id
        || descriptor.representation_id != *representation_id
    {
        return Err(input_error(
            "scripture_read_identity_mismatch",
            "read request does not target this Scripture backend identity",
        ));
    }
    if max_context_chars == 0 || max_context_chars > 2_000_000 || timeout_ms == 0 {
        return Err(input_error(
            "invalid_scripture_read_budget",
            "Scripture read budget is outside supported bounds",
        ));
    }
    Ok(())
}

fn parse_positive_record_id(value: &str, kind: &str) -> Result<i64, InformationError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > 20
        || !trimmed.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(input_error(
            "invalid_scripture_record_key",
            format!("Scripture {kind} key must be a positive decimal integer"),
        ));
    }
    let parsed = trimmed.parse::<i64>().map_err(|_| {
        input_error(
            "invalid_scripture_record_key",
            format!("Scripture {kind} key exceeds integer bounds"),
        )
    })?;
    if parsed <= 0 || parsed.to_string() != trimmed {
        return Err(input_error(
            "invalid_scripture_record_key",
            format!("Scripture {kind} key is not in canonical positive-integer form"),
        ));
    }
    Ok(parsed)
}

fn parse_json(value: &str, code: &'static str) -> Result<JsonValue, InformationError> {
    serde_json::from_str(value).map_err(|_| {
        integrity_error(
            code,
            "Scripture occurrence contains invalid bounded JSON metadata",
        )
    })
}

fn parse_json_array(value: &str, code: &'static str) -> Result<JsonValue, InformationError> {
    let parsed = parse_json(value, code)?;
    if !parsed.is_array() {
        return Err(integrity_error(
            code,
            "Scripture ambiguity flags metadata is not a JSON array",
        ));
    }
    Ok(parsed)
}

fn truncate_chars(input: &str, limit: usize) -> (String, bool) {
    let mut chars = input.chars();
    let output = chars.by_ref().take(limit).collect::<String>();
    let truncated = chars.next().is_some();
    (output, truncated)
}

fn open_read_only_connection(
    path: &Path,
    _access_mode: ExternalAccessMode,
    timeout_ms: u64,
) -> Result<Connection, InformationError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut uri = Url::from_file_path(path).map_err(|()| {
        input_error(
            "scripture_path_not_file_uri",
            "Scripture index path cannot be represented as a file URI",
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
                    "scripture_bind_limit_overflow",
                    "compiled bind limit exceeds SQLite integer bounds",
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
            "scripture_sqlite_bind_limit_too_low",
            "SQLite runtime cannot support the compiled Scripture bind limit",
        ));
    }
    connection
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_SQLITE_VALUE_BYTES)
        .map_err(map_sqlite_error)?;
    if connection
        .limit(Limit::SQLITE_LIMIT_LENGTH)
        .map_err(map_sqlite_error)?
        < MAX_SQLITE_VALUE_BYTES
    {
        return Err(InformationError::new(
            ErrorClass::Unsupported,
            "scripture_sqlite_value_limit_too_low",
            "SQLite runtime cannot support the compiled Scripture value limit",
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
    let database_read_only = connection.is_readonly("main").map_err(map_sqlite_error)?;
    if trusted_schema != 0 || query_only != 1 || !database_read_only {
        return Err(integrity_error(
            "scripture_read_only_guards_failed",
            "SQLite did not retain trusted_schema=OFF, query_only=ON, and a read-only main database",
        ));
    }
    let now = Instant::now();
    let deadline = now
        .checked_add(Duration::from_millis(timeout_ms))
        .unwrap_or(now);
    connection
        .progress_handler(1_000, Some(move || Instant::now() >= deadline))
        .map_err(map_sqlite_error)?;
    Ok(connection)
}

fn pragma_i64(connection: &Connection, name: &'static str) -> Result<i64, InformationError> {
    connection
        .pragma_query_value(None, name, |row| row.get(0))
        .map_err(map_sqlite_error)
}

fn sidecar_path(path: &Path, suffix: &str) -> Result<PathBuf, InformationError> {
    let Some(file_name) = path.file_name() else {
        return Err(input_error(
            "scripture_path_has_no_file_name",
            "Scripture index path has no file name",
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
            "verified_scripture_identity_mismatch",
            "Scripture index no longer matches the identity bound to its verified digest",
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
            "immutable_scripture_hash_open_failed",
            format!("cannot open immutable Scripture index for hashing: {error}"),
        )
    })?;
    let opened_identity = FileIdentity::from_metadata(&file.metadata().map_err(|error| {
        io_error(
            "immutable_scripture_hash_metadata_failed",
            format!("cannot inspect immutable Scripture file handle: {error}"),
        )
    })?)?;
    if &opened_identity != expected_identity {
        return Err(integrity_error(
            "immutable_scripture_identity_changed_before_hash",
            "immutable Scripture identity changed before digest verification",
        ));
    }
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut hasher = Sha256::new();
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            io_error(
                "immutable_scripture_hash_read_failed",
                format!("cannot hash immutable Scripture index: {error}"),
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
                "immutable_scripture_hash_metadata_failed",
                format!("cannot re-inspect immutable Scripture file handle: {error}"),
            )
        })?)?;
    let path_after = FileIdentity::observe(path)?;
    if opened_identity != handle_after || &path_after != expected_identity {
        return Err(integrity_error(
            "immutable_scripture_identity_changed_during_hash",
            "immutable Scripture identity changed during digest verification",
        ));
    }
    Ok(hex::encode(hasher.finalize()))
}

fn validate_sha256_value(value: &str) -> Result<(), InformationError> {
    let normalized = value.strip_prefix("sha256:").unwrap_or(value);
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(input_error(
            "invalid_verified_scripture_sha256",
            "verified Scripture digest is not a SHA-256 value",
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

const EXPECTED_PASSAGES_SQL: &str = r#"
CREATE TABLE scripture_passages (
    passage_id INTEGER PRIMARY KEY,
    book_key TEXT NOT NULL COLLATE NOCASE
        CHECK (length(trim(book_key)) > 0),
    book_name TEXT NOT NULL
        CHECK (length(trim(book_name)) > 0),
    book_ordinal INTEGER NOT NULL CHECK (book_ordinal > 0),
    start_chapter INTEGER NOT NULL CHECK (start_chapter > 0),
    start_verse INTEGER NOT NULL CHECK (start_verse >= 0),
    end_chapter INTEGER NOT NULL CHECK (end_chapter > 0),
    end_verse INTEGER NOT NULL CHECK (end_verse >= 0),
    normalized_reference TEXT NOT NULL
        CHECK (length(trim(normalized_reference)) > 0),
    CHECK (
        end_chapter > start_chapter OR
        (end_chapter = start_chapter AND end_verse >= start_verse)
    ),
    CHECK (
        (start_verse = 0 AND end_verse = 0) OR
        (start_verse > 0 AND end_verse > 0)
    ),
    UNIQUE (
        book_key, start_chapter, start_verse, end_chapter, end_verse
    )
)
"#;

const EXPECTED_OCCURRENCES_SQL: &str = r#"
CREATE TABLE scripture_citation_occurrences (
    occurrence_id INTEGER PRIMARY KEY,
    source_id INTEGER NOT NULL REFERENCES scripture_sources(source_id)
        ON DELETE CASCADE,
    passage_id INTEGER NOT NULL REFERENCES scripture_passages(passage_id)
        ON DELETE RESTRICT,
    occurrence_key TEXT NOT NULL
        CHECK (length(trim(occurrence_key)) > 0),
    raw_reference TEXT NOT NULL
        CHECK (length(trim(raw_reference)) > 0),
    confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    parser_version TEXT NOT NULL
        CHECK (length(trim(parser_version)) > 0),
    detection_method TEXT NOT NULL DEFAULT 'explicit_syntax'
        CHECK (detection_method IN (
            'explicit_syntax', 'quotation_alignment', 'semantic_allusion'
        )),
    detection_tier INTEGER NOT NULL DEFAULT 1
        CHECK (detection_tier IN (1, 2, 3)),
    review_status TEXT NOT NULL DEFAULT 'unreviewed'
        CHECK (review_status IN (
            'unreviewed', 'accepted', 'needs_review', 'rejected'
        )),
    ambiguity_flags_json TEXT NOT NULL DEFAULT '[]'
        CHECK (
            json_valid(ambiguity_flags_json)
            AND json_type(ambiguity_flags_json) = 'array'
        ),
    ambiguity_evidence_json TEXT
        CHECK (
            ambiguity_evidence_json IS NULL
            OR json_valid(ambiguity_evidence_json)
        ),
    alexandria_block_id TEXT,
    alexandria_doc_id TEXT,
    location_path TEXT,
    chapter_title TEXT,
    context_text TEXT,
    reference_start INTEGER,
    reference_end INTEGER,
    rebuild_token TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT
        (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT
        (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (
        (reference_start IS NULL AND reference_end IS NULL) OR
        (reference_start IS NOT NULL AND reference_end IS NOT NULL
         AND reference_start >= 0 AND reference_end >= reference_start)
    ),
    CHECK (
        (detection_method = 'explicit_syntax' AND detection_tier = 1) OR
        (detection_method = 'quotation_alignment' AND detection_tier = 2) OR
        (detection_method = 'semantic_allusion' AND detection_tier = 3)
    ),
    UNIQUE (source_id, occurrence_key)
)
"#;

const EXPECTED_SOURCES_SQL: &str = r#"
CREATE TABLE scripture_sources (
    source_id INTEGER PRIMARY KEY,
    source_uri TEXT NOT NULL COLLATE BINARY UNIQUE
        CHECK (length(trim(source_uri)) > 0),
    source_kind TEXT NOT NULL
        CHECK (length(trim(source_kind)) > 0),
    source_title TEXT,
    source_author TEXT,
    content_fingerprint TEXT,
    alexandria_doc_id TEXT,
    parser_version TEXT NOT NULL
        CHECK (length(trim(parser_version)) > 0),
    first_indexed_at TEXT NOT NULL DEFAULT
        (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_indexed_at TEXT NOT NULL DEFAULT
        (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
)
"#;

fn validate_scripture_schema(connection: &Connection) -> Result<(), InformationError> {
    for (table, expected) in [
        ("scripture_passages", EXPECTED_PASSAGES_SQL),
        ("scripture_citation_occurrences", EXPECTED_OCCURRENCES_SQL),
        ("scripture_sources", EXPECTED_SOURCES_SQL),
    ] {
        let actual = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(map_sqlite_error)?
            .flatten()
            .ok_or_else(|| {
                integrity_error(
                    "scripture_profile_missing_table",
                    format!("compiled Scripture profile is missing table {table}"),
                )
            })?;
        if normalize_schema_sql(&actual) != normalize_schema_sql(expected) {
            return Err(integrity_error(
                "scripture_profile_schema_mismatch",
                format!("table {table} does not exactly match the compiled schema"),
            ));
        }
    }
    for (index, expected_columns) in [
        (
            "idx_scripture_passages_exact_range",
            &[
                "book_key",
                "start_chapter",
                "start_verse",
                "end_chapter",
                "end_verse",
            ][..],
        ),
        (
            "idx_scripture_occurrences_passage",
            &["passage_id", "source_id"][..],
        ),
        (
            "idx_scripture_occurrences_block",
            &["alexandria_block_id"][..],
        ),
        ("idx_scripture_occurrences_doc", &["alexandria_doc_id"][..]),
        ("idx_scripture_occurrences_confidence", &["confidence"][..]),
        (
            "idx_scripture_occurrences_eligibility",
            &["detection_tier", "review_status", "confidence"][..],
        ),
        (
            "idx_scripture_sources_alexandria_doc",
            &["alexandria_doc_id"][..],
        ),
    ] {
        require_index_columns(connection, index, expected_columns)?;
    }
    Ok(())
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn require_index_columns(
    connection: &Connection,
    index: &'static str,
    expected: &[&str],
) -> Result<(), InformationError> {
    let sql = match index {
        "idx_scripture_passages_exact_range" => {
            "PRAGMA index_info(idx_scripture_passages_exact_range)"
        }
        "idx_scripture_occurrences_passage" => {
            "PRAGMA index_info(idx_scripture_occurrences_passage)"
        }
        "idx_scripture_occurrences_block" => "PRAGMA index_info(idx_scripture_occurrences_block)",
        "idx_scripture_occurrences_doc" => "PRAGMA index_info(idx_scripture_occurrences_doc)",
        "idx_scripture_occurrences_confidence" => {
            "PRAGMA index_info(idx_scripture_occurrences_confidence)"
        }
        "idx_scripture_occurrences_eligibility" => {
            "PRAGMA index_info(idx_scripture_occurrences_eligibility)"
        }
        "idx_scripture_sources_alexandria_doc" => {
            "PRAGMA index_info(idx_scripture_sources_alexandria_doc)"
        }
        _ => {
            return Err(integrity_error(
                "scripture_profile_internal_index",
                "backend attempted to inspect an unknown compiled index",
            ));
        }
    };
    let mut statement = connection.prepare(sql).map_err(map_sqlite_error)?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(2))
        .map_err(map_sqlite_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(map_sqlite_error)?;
    if columns != expected {
        return Err(integrity_error(
            "scripture_profile_index_mismatch",
            format!("compiled Scripture index {index} is missing or has different columns"),
        ));
    }
    Ok(())
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
        policy.excerpt_export = intersect_permission(policy.excerpt_export, rights_permission);
        policy.redistribution = intersect_permission(policy.redistribution, rights_permission);
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

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn map_sqlite_error(error: rusqlite::Error) -> InformationError {
    match &error {
        rusqlite::Error::SqliteFailure(code, _) if code.code == ErrorCode::OperationInterrupted => {
            InformationError::new(
                ErrorClass::ResourceBusy,
                "scripture_query_timeout",
                "Scripture SQLite query deadline elapsed",
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
                "scripture_resource_busy",
                "Scripture SQLite source is busy",
            )
            .retryable(true)
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(code.code, ErrorCode::PermissionDenied | ErrorCode::ReadOnly) =>
        {
            InformationError::new(
                ErrorClass::Permission,
                "scripture_sqlite_permission_denied",
                "SQLite denied the read-only Scripture operation",
            )
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            ) =>
        {
            integrity_error(
                "scripture_source_invalid",
                "Scripture source is corrupt or not a SQLite database",
            )
        }
        rusqlite::Error::QueryReturnedNoRows => not_found_error(
            "scripture_record_not_found",
            "Scripture record was not found",
        ),
        _ => InformationError::new(
            ErrorClass::Backend,
            "scripture_sqlite_backend_error",
            format!("Scripture SQLite operation failed: {error}"),
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

fn not_found_error(code: &'static str, message: impl Into<String>) -> InformationError {
    InformationError::new(ErrorClass::NotFound, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use information_native_types::{
        QUERY_SCHEMA, QueryBudget, QueryFilters, QueryId, RetrievalTarget,
    };
    use std::error::Error;

    fn fixture_ids() -> Result<(ResourceId, ReleaseId, RepresentationId), Box<dyn Error>> {
        Ok((
            ResourceId::parse("fixture-scripture")?,
            ReleaseId::parse("fixture-release")?,
            RepresentationId::parse("fixture-scripture-sqlite")?,
        ))
    }

    fn fixture_config(path: impl Into<PathBuf>) -> Result<ScriptureBackendConfig, Box<dyn Error>> {
        let (resource_id, release_id, representation_id) = fixture_ids()?;
        Ok(ScriptureBackendConfig::new(
            "fixture-scripture-backend",
            "Fixture Scripture References",
            resource_id,
            release_id,
            representation_id,
            path,
            ExternalAccessMode::LiveReadOnly,
            "Fixture Corpus Operator",
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
                max_hits: 10,
                max_hits_per_backend: 10,
                max_backends: 1,
                max_context_chars: 4_000,
                timeout_ms: 5_000,
            },
        })
    }

    fn create_fixture(connection: &Connection) -> rusqlite::Result<()> {
        connection.execute_batch(EXPECTED_PASSAGES_SQL)?;
        connection.execute_batch(EXPECTED_SOURCES_SQL)?;
        connection.execute_batch(EXPECTED_OCCURRENCES_SQL)?;
        connection.execute_batch(
            r#"
            CREATE INDEX idx_scripture_passages_exact_range
                ON scripture_passages(
                    book_key, start_chapter, start_verse, end_chapter, end_verse
                );
            CREATE INDEX idx_scripture_occurrences_passage
                ON scripture_citation_occurrences(passage_id, source_id);
            CREATE INDEX idx_scripture_occurrences_block
                ON scripture_citation_occurrences(alexandria_block_id);
            CREATE INDEX idx_scripture_occurrences_doc
                ON scripture_citation_occurrences(alexandria_doc_id);
            CREATE INDEX idx_scripture_occurrences_confidence
                ON scripture_citation_occurrences(confidence DESC);
            CREATE INDEX idx_scripture_occurrences_eligibility
                ON scripture_citation_occurrences(
                    detection_tier, review_status, confidence DESC
                );
            CREATE INDEX idx_scripture_sources_alexandria_doc
                ON scripture_sources(alexandria_doc_id);

            INSERT INTO scripture_passages (
                passage_id, book_key, book_name, book_ordinal,
                start_chapter, start_verse, end_chapter, end_verse,
                normalized_reference
            ) VALUES
                (1, 'John', 'John', 43, 3, 16, 3, 16, 'John.3.16'),
                (2, 'John', 'John', 43, 3, 16, 3, 17, 'John.3.16-John.3.17'),
                (3, 'Gen', 'Genesis', 1, 1, 1, 1, 1, 'Gen.1.1');

            INSERT INTO scripture_sources (
                source_id, source_uri, source_kind, source_title, source_author,
                content_fingerprint, alexandria_doc_id, parser_version
            ) VALUES
                (10, 'fixture://source/one', 'epub', 'Fixture Homily', 'A. Writer',
                 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                 'DOC-1', 'source-parser-v1'),
                (11, 'fixture://source/two', 'text', 'Second Source', NULL,
                 NULL, 'DOC-2', 'source-parser-v1');

            INSERT INTO scripture_citation_occurrences (
                occurrence_id, source_id, passage_id, occurrence_key,
                raw_reference, confidence, parser_version, detection_method,
                detection_tier, review_status, ambiguity_flags_json,
                ambiguity_evidence_json, alexandria_block_id, alexandria_doc_id,
                location_path, chapter_title, context_text,
                reference_start, reference_end, rebuild_token
            ) VALUES
                (100, 10, 1, 'chapter-1:20:29', 'John 3:16', 0.99,
                 'occurrence-parser-v2', 'explicit_syntax', 1, 'accepted',
                 '["abbreviation"]', '{"candidate_count":1}', 'BLOCK-1', 'DOC-1',
                 'chapter/1', 'Love', 'For God so loved the world.', 20, 29, 'fixture-r1'),
                (101, 11, 1, 'line-4:0:9', 'Jn 3:16', 0.82,
                 'occurrence-parser-v2', 'quotation_alignment', 2, 'needs_review',
                 '[]', NULL, 'BLOCK-2', NULL, 'line/4', NULL,
                 'The cited line appears in a second source.', 0, 9, 'fixture-r1'),
                (102, 10, 2, 'chapter-2:10:24', 'John 3:16-17', 0.97,
                 'occurrence-parser-v2', 'explicit_syntax', 1, 'unreviewed',
                 '[]', NULL, 'BLOCK-3', 'DOC-1', 'chapter/2', 'Witness',
                 'The neighboring verses are cited together.', 10, 24, 'fixture-r1');
            "#,
        )
    }

    fn create_file_fixture(path: &Path) -> Result<(), Box<dyn Error>> {
        let connection = Connection::open(path)?;
        create_fixture(&connection)?;
        drop(connection);
        Ok(())
    }

    fn fixture_backend() -> Result<(tempfile::TempDir, ScriptureBackend), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("scripture.db");
        create_file_fixture(&path)?;
        let backend = ScriptureBackend::open(fixture_config(&path)?)?;
        Ok((directory, backend))
    }

    #[test]
    fn prefix_search_returns_rank_only_occurrences_with_provenance() -> Result<(), Box<dyn Error>> {
        let (_directory, backend) = fixture_backend()?;
        let result = backend.search(&fixture_query("John.3.16", QuerySyntax::NaturalTerms)?)?;
        assert_eq!(result.hits.len(), 3);
        assert!(result.hits.iter().all(|hit| {
            hit.score.semantics == ScoreSemantics::RankOnly
                && matches!(
                    &hit.locator,
                    EvidenceLocator::Record { collection: Some(collection), .. }
                        if collection == OCCURRENCE_COLLECTION
                )
        }));
        let first = result
            .hits
            .first()
            .ok_or_else(|| std::io::Error::other("fixture search returned no first hit"))?;
        assert_eq!(first.title, "Fixture Homily");
        assert_eq!(first.creator.as_deref(), Some("A. Writer"));
        assert_eq!(first.document_id.as_deref(), Some("DOC-1"));
        assert_eq!(first.passage_id.as_deref(), Some("1"));
        assert_eq!(first.source_uri.as_deref(), Some("fixture://source/one"));
        assert_eq!(
            first.source_fingerprint.as_deref(),
            Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(first.metadata["raw_reference"], json!("John 3:16"));
        assert_eq!(first.metadata["normalized_reference"], json!("John.3.16"));
        assert_eq!(first.metadata["alexandria_block_id"], json!("BLOCK-1"));
        assert_eq!(first.metadata["detection_method"], json!("explicit_syntax"));
        assert_eq!(first.metadata["review_status"], json!("accepted"));
        assert_eq!(
            first.excerpt_sha256,
            evidence_text_sha256(&first.snippet, &first.context)
        );
        first.validate()?;
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("without FTS"))
        );
        Ok(())
    }

    #[test]
    fn exact_search_does_not_return_longer_passage() -> Result<(), Box<dyn Error>> {
        let (_directory, backend) = fixture_backend()?;
        let result = backend.search(&fixture_query("John.3.16", QuerySyntax::ExactPhrase)?)?;
        assert_eq!(result.hits.len(), 2);
        assert!(
            result
                .hits
                .iter()
                .all(|hit| hit.metadata["normalized_reference"] == json!("John.3.16"))
        );
        Ok(())
    }

    #[test]
    fn occurrence_lookup_and_record_read_round_trip() -> Result<(), Box<dyn Error>> {
        let (_directory, backend) = fixture_backend()?;
        let (resource_id, release_id, representation_id) = fixture_ids()?;
        let lookup = backend.lookup(&LookupRequest {
            resource_id: resource_id.clone(),
            release_id: release_id.clone(),
            representation_id: representation_id.clone(),
            purpose: RetrievalPurpose::LocalUi,
            collection: Some(OCCURRENCE_COLLECTION.to_string()),
            key: "100".to_string(),
            max_context_chars: 1_000,
            timeout_ms: 5_000,
        })?;
        let read = backend.read(&ReadRequest {
            resource_id,
            release_id,
            representation_id,
            purpose: RetrievalPurpose::LocalUi,
            locator: lookup.hit.locator.clone(),
            max_context_chars: 1_000,
            timeout_ms: 5_000,
        })?;
        assert_eq!(read.hit.locator, lookup.hit.locator);
        assert_eq!(read.hit.snippet, lookup.hit.snippet);
        assert_eq!(read.hit.context, lookup.hit.context);
        assert_eq!(read.hit.excerpt_sha256, lookup.hit.excerpt_sha256);
        read.hit.validate()?;
        Ok(())
    }

    #[test]
    fn passage_lookup_returns_stable_passage_record_locator() -> Result<(), Box<dyn Error>> {
        let (_directory, backend) = fixture_backend()?;
        let (resource_id, release_id, representation_id) = fixture_ids()?;
        let result = backend.lookup(&LookupRequest {
            resource_id,
            release_id,
            representation_id,
            purpose: RetrievalPurpose::LocalUi,
            collection: Some(PASSAGE_COLLECTION.to_string()),
            key: "1".to_string(),
            max_context_chars: 1_000,
            timeout_ms: 5_000,
        })?;
        assert_eq!(result.hit.passage_id.as_deref(), Some("1"));
        assert_eq!(
            result.hit.metadata["normalized_reference"],
            json!("John.3.16")
        );
        assert!(matches!(
            result.hit.locator,
            EvidenceLocator::Record { collection: Some(ref collection), ref key }
                if collection == PASSAGE_COLLECTION && key == "1"
        ));
        result.hit.validate()?;
        Ok(())
    }

    #[test]
    fn default_policy_fails_closed_outside_local_ui() -> Result<(), Box<dyn Error>> {
        let (_directory, backend) = fixture_backend()?;
        assert_eq!(
            backend.descriptor().use_policy.redistribution,
            UsePermission::Forbidden
        );
        let mut query = fixture_query("John.3.16", QuerySyntax::ExactPhrase)?;
        query.purpose = RetrievalPurpose::ModelContext;
        let error = backend
            .search(&query)
            .err()
            .ok_or_else(|| std::io::Error::other("model context unexpectedly succeeded"))?;
        assert_eq!(error.class, ErrorClass::Permission);
        assert_eq!(error.code, "retrieval_purpose_not_permitted");
        Ok(())
    }

    #[test]
    fn literal_prefix_escapes_like_wildcards() -> Result<(), Box<dyn Error>> {
        let (_directory, backend) = fixture_backend()?;
        let result = backend.search(&fixture_query("John.%", QuerySyntax::NaturalTerms)?)?;
        assert!(result.hits.is_empty());
        Ok(())
    }

    #[test]
    fn schema_drift_is_rejected_before_retrieval() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("drifted.db");
        let connection = Connection::open(&path)?;
        create_fixture(&connection)?;
        connection.execute_batch("DROP INDEX idx_scripture_occurrences_passage")?;
        drop(connection);
        let error = ScriptureBackend::open(fixture_config(&path)?)
            .err()
            .ok_or_else(|| std::io::Error::other("drifted schema unexpectedly opened"))?;
        assert_eq!(error.code, "scripture_profile_index_mismatch");
        Ok(())
    }

    #[test]
    fn source_file_identity_is_unchanged_by_fixture_retrieval() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("identity.db");
        create_file_fixture(&path)?;
        let before = FileIdentity::observe(&path)?;
        let backend = ScriptureBackend::open(fixture_config(&path)?)?;
        let _result = backend.search(&fixture_query("John.3", QuerySyntax::NaturalTerms)?)?;
        let after = FileIdentity::observe(&path)?;
        assert_eq!(before, after);
        Ok(())
    }

    #[test]
    fn both_access_modes_create_no_sqlite_sidecars() -> Result<(), Box<dyn Error>> {
        for (index, access_mode) in [
            ExternalAccessMode::LiveReadOnly,
            ExternalAccessMode::ImmutableReadOnly,
        ]
        .into_iter()
        .enumerate()
        {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join(format!("sidecar-free-{index}.db"));
            create_file_fixture(&path)?;
            let before = SidecarSet::observe(&path)?;
            assert_eq!(
                before,
                SidecarSet {
                    wal: SidecarIdentity::Absent,
                    shm: SidecarIdentity::Absent,
                    journal: SidecarIdentity::Absent,
                }
            );
            let mut config = fixture_config(&path)?;
            config.access_mode = access_mode;
            let backend = ScriptureBackend::open(config)?;
            let _result = backend.search(&fixture_query("John.3", QuerySyntax::NaturalTerms)?)?;
            assert_eq!(before, SidecarSet::observe(&path)?);
        }
        Ok(())
    }

    #[test]
    fn both_access_modes_reject_nonempty_wal() -> Result<(), Box<dyn Error>> {
        for (index, access_mode) in [
            ExternalAccessMode::LiveReadOnly,
            ExternalAccessMode::ImmutableReadOnly,
        ]
        .into_iter()
        .enumerate()
        {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join(format!("pending-wal-{index}.db"));
            create_file_fixture(&path)?;
            fs::write(sidecar_path(&path, "-wal")?, b"pending transaction")?;
            let mut config = fixture_config(&path)?;
            config.access_mode = access_mode;
            let error = ScriptureBackend::open(config)
                .err()
                .ok_or_else(|| std::io::Error::other("non-empty WAL unexpectedly opened"))?;
            assert_eq!(error.code, "scripture_nonempty_wal");
        }
        Ok(())
    }

    #[test]
    #[ignore = "set INFORMATION_NATIVE_SCRIPTURE_DB to run against a compatible index"]
    fn real_scripture_index_read_only_smoke() -> Result<(), Box<dyn Error>> {
        let Some(path) = std::env::var_os("INFORMATION_NATIVE_SCRIPTURE_DB") else {
            return Ok(());
        };
        let path = Path::new(&path);
        let before = FileIdentity::observe(path)?;
        let before_sidecars = SidecarSet::observe(path)?;
        let (resource_id, release_id, representation_id) = (
            ResourceId::parse("alexandria-scripture")?,
            ReleaseId::parse("local-index")?,
            RepresentationId::parse("bible-refs-sqlite")?,
        );
        let backend = ScriptureBackend::open(ScriptureBackendConfig::new(
            "alexandria-scripture-local",
            "Alexandria Scripture References",
            resource_id.clone(),
            release_id.clone(),
            representation_id.clone(),
            path,
            ExternalAccessMode::LiveReadOnly,
            "Local Alexandria Corpus",
        ))?;
        assert_eq!(backend.health().status, BackendHealthStatus::Ready);
        let lookup = backend.lookup(&LookupRequest {
            resource_id,
            release_id,
            representation_id,
            purpose: RetrievalPurpose::LocalUi,
            collection: Some(OCCURRENCE_COLLECTION.to_string()),
            key: "1".to_string(),
            max_context_chars: 2_000,
            timeout_ms: 10_000,
        })?;
        lookup.hit.validate()?;
        assert_eq!(lookup.hit.score.semantics, ScoreSemantics::RankOnly);
        assert_eq!(before, FileIdentity::observe(path)?);
        assert_eq!(before_sidecars, SidecarSet::observe(path)?);
        Ok(())
    }
}
