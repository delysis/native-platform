#![forbid(unsafe_code)]

//! Versioned, transport-neutral contracts for offline information resources.
//!
//! These types grant no filesystem, network, process, SQLite, Tauri, or model
//! authority. Text carried in evidence values is untrusted source material.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use thiserror::Error;
use url::{Host, Url};
use uuid::Uuid;

pub const CATALOG_SCHEMA: &str = "information_native.catalog.v1";
pub const INSTALL_PLAN_SCHEMA: &str = "information_native.install_plan.v1";
pub const INSTALL_RECEIPT_SCHEMA: &str = "information_native.install_receipt.v1";
pub const EXTERNAL_REGISTRATION_SCHEMA: &str = "information_native.external_registration.v1";
pub const QUERY_SCHEMA: &str = "information_native.query.v1";
pub const EVIDENCE_SCHEMA: &str = "information_native.evidence.v1";
pub const MAX_RETRIEVAL_TIMEOUT_MS: u64 = 300_000;
pub const MAX_ACQUISITION_ATTEMPTS: usize = 128;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
                let value = value.into();
                validate_identifier(stringify!($name), &value)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ContractError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(ResourceId);
string_id!(ReleaseId);
string_id!(RepresentationId);
string_id!(ArtifactId);
string_id!(InstallationId);
string_id!(QueryId);

impl InstallationId {
    #[must_use]
    pub fn new() -> Self {
        Self::parse(Uuid::new_v4().to_string()).expect("UUID installation ids are always valid")
    }
}

impl Default for InstallationId {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryId {
    #[must_use]
    pub fn new() -> Self {
        Self::parse(Uuid::new_v4().to_string()).expect("UUID query ids are always valid")
    }
}

impl Default for QueryId {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_identifier(kind: &str, value: &str) -> Result<(), ContractError> {
    if value.is_empty()
        || value.len() > 192
        || value.starts_with('/')
        || value.ends_with('/')
        || value
            .split('/')
            .any(|segment| segment == "." || segment == ".." || segment.is_empty())
    {
        return Err(ContractError::InvalidIdentifier {
            kind: kind.to_string(),
            value: value.to_string(),
        });
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':' | b'@')
    }) {
        return Err(ContractError::InvalidIdentifier {
            kind: kind.to_string(),
            value: value.to_string(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InformationCatalog {
    pub schema: String,
    pub catalogue_id: String,
    pub generated_at: DateTime<Utc>,
    pub publisher: Publisher,
    /// Untrusted upstream declaration. Effective authority is established by
    /// the catalogue loader and is never deserialized from this field.
    pub declared_trust: CatalogTrust,
    #[serde(default)]
    pub resources: Vec<ResourceRecord>,
}

impl InformationCatalog {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_schema(CATALOG_SCHEMA, &self.schema)?;
        require_text("catalogue_id", &self.catalogue_id)?;
        self.publisher.validate()?;
        let mut resources = BTreeSet::new();
        for record in &self.resources {
            record.validate()?;
            if !resources.insert(&record.resource.id) {
                return Err(ContractError::DuplicateIdentifier(
                    record.resource.id.to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    pub name: String,
    pub homepage: Option<String>,
}

impl Publisher {
    fn validate(&self) -> Result<(), ContractError> {
        require_text("publisher.name", &self.name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogTrust {
    BuiltIn,
    Signed { key_id: String },
    LocallyApproved { approved_by: String },
    Unverified,
}

/// Durable evidence for the authority actually granted to a catalogue by the
/// host. This is deliberately separate from [`InformationCatalog::declared_trust`],
/// which is only an untrusted upstream declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogAuthority {
    Unverified {
        declared: CatalogTrust,
        catalog_sha256: String,
    },
    BuiltInPinned {
        catalog_sha256: String,
    },
    LocallyApproved {
        catalog_sha256: String,
        approved_by: String,
        approved_at: DateTime<Utc>,
    },
    /// A self-contained plan accepted through an explicit import boundary.
    /// The original fingerprint preserves what the caller supplied, but this
    /// variant grants no catalogue authority.
    DetachedUnverified {
        source_plan_sha256: String,
    },
}

impl CatalogAuthority {
    pub fn validate(&self) -> Result<(), ContractError> {
        let digest = match self {
            Self::Unverified { catalog_sha256, .. }
            | Self::BuiltInPinned { catalog_sha256 }
            | Self::LocallyApproved { catalog_sha256, .. } => catalog_sha256,
            Self::DetachedUnverified { source_plan_sha256 } => source_plan_sha256,
        };
        validate_sha256(digest)?;
        if let Self::LocallyApproved { approved_by, .. } = self {
            require_text("catalog_authority.approved_by", approved_by)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResourceRecord {
    pub resource: ResourceDescriptor,
    #[serde(default)]
    pub releases: Vec<ResourceRelease>,
}

impl ResourceRecord {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.resource.validate()?;
        if self.releases.is_empty() {
            return Err(ContractError::InvalidContract(format!(
                "resource {} has no releases",
                self.resource.id
            )));
        }
        let mut releases = BTreeSet::new();
        for release in &self.releases {
            release.validate()?;
            if !releases.insert(&release.id) {
                return Err(ContractError::DuplicateIdentifier(release.id.to_string()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceDescriptor {
    pub id: ResourceId,
    pub kind: ResourceKind,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub subjects: Vec<String>,
    pub homepage: Option<String>,
    #[serde(default)]
    pub extensions: BTreeMap<String, Value>,
}

impl ResourceDescriptor {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_identifier("resource_id", self.id.as_str())?;
        require_text("resource.title", &self.title)?;
        require_text("resource.summary", &self.summary)?;
        validate_short_values("resource.languages", &self.languages)?;
        validate_short_values("resource.subjects", &self.subjects)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Encyclopedia,
    TextCorpus,
    PublicationCollection,
    KnowledgeGraph,
    Map,
    Gazetteer,
    Bibliography,
    MediaArchive,
    WebArchive,
    StructuredDataset,
    Mixed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResourceRelease {
    pub id: ReleaseId,
    pub published_at: Option<DateTime<Utc>>,
    pub upstream_id: Option<String>,
    pub immutable: bool,
    pub provenance: Provenance,
    #[serde(default)]
    pub rights: Vec<RightsStatement>,
    #[serde(default)]
    pub default_use_policy: UsePolicy,
    #[serde(default)]
    pub representations: Vec<RepresentationDescriptor>,
}

impl ResourceRelease {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_identifier("release_id", self.id.as_str())?;
        self.provenance.validate()?;
        if self.rights.is_empty() {
            return Err(ContractError::InvalidContract(format!(
                "release {} has no rights statement",
                self.id
            )));
        }
        for statement in &self.rights {
            statement.validate()?;
        }
        self.default_use_policy.validate_with_rights(&self.rights)?;
        if self.representations.is_empty() {
            return Err(ContractError::InvalidContract(format!(
                "release {} has no representations",
                self.id
            )));
        }
        let mut representations = BTreeSet::new();
        for representation in &self.representations {
            representation.validate()?;
            if !representations.insert(&representation.id) {
                return Err(ContractError::DuplicateIdentifier(
                    representation.id.to_string(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub publisher: String,
    pub source_uri: String,
    pub upstream_record_id: Option<String>,
    #[serde(default)]
    pub source_inputs: Vec<String>,
    pub transformation: Option<String>,
    pub metadata: BTreeMap<String, Value>,
}

impl Provenance {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_text("provenance.publisher", &self.publisher)?;
        require_text("provenance.source_uri", &self.source_uri)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RightsStatement {
    pub scope: String,
    pub expression: String,
    pub license_url: Option<String>,
    pub license_text_sha256: Option<String>,
    pub attribution: Option<String>,
    #[serde(default)]
    pub obligations: Vec<String>,
    pub redistribution: RedistributionPolicy,
}

impl RightsStatement {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_text("rights.scope", &self.scope)?;
        require_text("rights.expression", &self.expression)?;
        if let Some(digest) = &self.license_text_sha256 {
            validate_sha256(digest)?;
        }
        if self.redistribution == RedistributionPolicy::AllowedWithObligations
            && self.obligations.is_empty()
        {
            return Err(ContractError::InvalidContract(
                "allowed-with-obligations rights require at least one obligation".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedistributionPolicy {
    Allowed,
    AllowedWithObligations,
    PrivateUseOnly,
    Unknown,
    Forbidden,
}

/// Operational policy is separate from copyright vocabulary. Unknown is not
/// permission: callers may still search a privately mounted source, while
/// export or redistribution remains blocked unless policy says otherwise.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct UsePolicy {
    pub local_search: UsePermission,
    pub model_context: UsePermission,
    pub excerpt_export: UsePermission,
    pub redistribution: UsePermission,
    pub attribution_required: bool,
}

impl Default for UsePolicy {
    fn default() -> Self {
        Self {
            local_search: UsePermission::Allowed,
            model_context: UsePermission::Unknown,
            excerpt_export: UsePermission::Unknown,
            redistribution: UsePermission::Unknown,
            attribution_required: true,
        }
    }
}

impl UsePolicy {
    #[must_use]
    pub fn permission_for(self, purpose: RetrievalPurpose) -> UsePermission {
        match purpose {
            RetrievalPurpose::LocalUi => self.local_search,
            RetrievalPurpose::ModelContext => {
                intersect_permission(self.local_search, self.model_context)
            }
            RetrievalPurpose::ExcerptExport => {
                intersect_permission(self.local_search, self.excerpt_export)
            }
        }
    }

    pub fn validate_with_rights(self, rights: &[RightsStatement]) -> Result<(), ContractError> {
        let redistribution_is_allowed = !rights.is_empty()
            && rights.iter().all(|statement| {
                matches!(
                    statement.redistribution,
                    RedistributionPolicy::Allowed | RedistributionPolicy::AllowedWithObligations
                )
            });
        if !redistribution_is_allowed && self.excerpt_export == UsePermission::Allowed {
            return Err(ContractError::InvalidContract(
                "use policy permits excerpt export beyond the rights redistribution ceiling"
                    .to_string(),
            ));
        }
        if !redistribution_is_allowed && self.redistribution == UsePermission::Allowed {
            return Err(ContractError::InvalidContract(
                "use policy permits redistribution beyond the rights redistribution ceiling"
                    .to_string(),
            ));
        }
        if rights.iter().any(|statement| {
            statement.redistribution == RedistributionPolicy::AllowedWithObligations
        }) && !self.attribution_required
        {
            return Err(ContractError::InvalidContract(
                "allowed-with-obligations rights require an attribution-required use policy"
                    .to_string(),
            ));
        }
        if self.attribution_required
            && !rights.iter().any(|statement| {
                statement
                    .attribution
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
                    || statement
                        .license_url
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
            })
        {
            return Err(ContractError::InvalidContract(
                "attribution-required policy has no attribution text or license URI".to_string(),
            ));
        }
        Ok(())
    }
}

fn intersect_permission(left: UsePermission, right: UsePermission) -> UsePermission {
    match (left, right) {
        (UsePermission::Forbidden, _) | (_, UsePermission::Forbidden) => UsePermission::Forbidden,
        (UsePermission::Allowed, UsePermission::Allowed) => UsePermission::Allowed,
        _ => UsePermission::Unknown,
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsePermission {
    Allowed,
    Forbidden,
    #[default]
    Unknown,
}

/// The audience/operation for which source text is being materialized.
/// Unknown permission always fails closed at retrieval boundaries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalPurpose {
    LocalUi,
    ModelContext,
    ExcerptExport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RepresentationDescriptor {
    pub id: RepresentationId,
    pub format: RepresentationFormat,
    #[serde(default)]
    pub capabilities: BTreeSet<InformationCapability>,
    pub coverage: CoverageDescriptor,
    pub subset_support: SubsetSupport,
    pub expected_installed_bytes: u64,
    #[serde(default)]
    pub artifacts: Vec<ArtifactDescriptor>,
    pub runtime: RuntimeRequirement,
}

impl RepresentationDescriptor {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_identifier("representation_id", self.id.as_str())?;
        if self.capabilities.is_empty() {
            return Err(ContractError::InvalidContract(format!(
                "representation {} has no capabilities",
                self.id
            )));
        }
        self.coverage.validate()?;
        if self.artifacts.is_empty() {
            return Err(ContractError::InvalidContract(format!(
                "representation {} has no artifacts",
                self.id
            )));
        }
        let mut artifacts = BTreeSet::new();
        let mut portable_file_names = BTreeSet::new();
        for artifact in &self.artifacts {
            artifact.validate()?;
            if !artifacts.insert(&artifact.id) {
                return Err(ContractError::DuplicateIdentifier(artifact.id.to_string()));
            }
            if !portable_file_names.insert(artifact.file_name.to_ascii_lowercase()) {
                return Err(ContractError::InvalidContract(format!(
                    "representation {} has an ASCII case-insensitive artifact file-name collision: {:?}",
                    self.id, artifact.file_name
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepresentationFormat {
    pub kind: FormatKind,
    pub profile: Option<String>,
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum FormatKind {
    AlexandriaSqlite,
    SqliteFts5,
    Zim,
    OsmPbf,
    GeoParquet,
    PmTiles,
    FlatGeobuf,
    GeoJsonSequence,
    GeoPackage,
    Parquet,
    JsonLines,
    Csv,
    Epub,
    Warc,
    Hdt,
    Directory,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum InformationCapability {
    LexicalSearch,
    ArticleRead,
    RecordLookup,
    SpatialFilter,
    TemporalFilter,
    GraphTraversal,
    TileRead,
    MediaRead,
    RandomAccess,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct CoverageDescriptor {
    pub languages: Vec<String>,
    pub subjects: Vec<String>,
    pub spatial: Option<BoundingBox>,
    pub temporal_start: Option<DateTime<Utc>>,
    pub temporal_end: Option<DateTime<Utc>>,
    pub records: Option<u64>,
}

impl CoverageDescriptor {
    fn validate(&self) -> Result<(), ContractError> {
        validate_short_values("coverage.languages", &self.languages)?;
        validate_short_values("coverage.subjects", &self.subjects)?;
        if let Some(spatial) = self.spatial {
            spatial.validate()?;
        }
        if self
            .temporal_start
            .zip(self.temporal_end)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(ContractError::InvalidContract(
                "coverage temporal_start is after temporal_end".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BoundingBox {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl BoundingBox {
    pub fn validate(self) -> Result<(), ContractError> {
        let finite = [self.west, self.south, self.east, self.north]
            .into_iter()
            .all(f64::is_finite);
        if !finite
            || !(-180.0..=180.0).contains(&self.west)
            || !(-180.0..=180.0).contains(&self.east)
            || !(-90.0..=90.0).contains(&self.south)
            || !(-90.0..=90.0).contains(&self.north)
            || self.west >= self.east
            || self.south >= self.north
        {
            return Err(ContractError::InvalidBoundingBox);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SubsetSupport {
    pub bounding_box: bool,
    pub languages: bool,
    pub subjects: bool,
    pub feature_types: Vec<String>,
    pub columns: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeRequirement {
    BuiltIn {
        backend: String,
    },
    RegisteredBackend {
        backend: String,
    },
    ExternalProgram {
        program: String,
        minimum_version: Option<String>,
    },
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDescriptor {
    pub id: ArtifactId,
    pub role: ArtifactRole,
    pub file_name: String,
    pub media_type: String,
    pub expected_bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub mirrors: Vec<ArtifactMirror>,
}

impl ArtifactDescriptor {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_identifier("artifact_id", self.id.as_str())?;
        validate_file_name(&self.file_name)?;
        require_text("artifact.media_type", &self.media_type)?;
        validate_sha256(&self.sha256)?;
        if self.mirrors.is_empty() {
            return Err(ContractError::InvalidContract(format!(
                "artifact {} has no mirrors",
                self.id
            )));
        }
        for mirror in &self.mirrors {
            validate_durable_fetch_uri("artifact.mirror.uri", &mirror.uri)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    Primary,
    SearchIndex,
    Metadata,
    Attribution,
    Changelog,
    Bridge,
    Signature,
    Auxiliary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMirror {
    pub uri: String,
    pub priority: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct InstallSelection {
    pub bounding_box: Option<BoundingBox>,
    pub languages: Vec<String>,
    pub subjects: Vec<String>,
    pub feature_types: Vec<String>,
    pub columns: Vec<String>,
}

/// Frozen catalogue facts for exactly the resource representation selected by
/// an install plan. Runtime and provenance facts survive catalogue updates and
/// are available without consulting the network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResolvedResource {
    pub resource: ResourceDescriptor,
    pub release_id: ReleaseId,
    pub published_at: Option<DateTime<Utc>>,
    pub upstream_id: Option<String>,
    pub immutable: bool,
    pub provenance: Provenance,
    #[serde(default)]
    pub rights: Vec<RightsStatement>,
    pub use_policy: UsePolicy,
    pub representation: RepresentationDescriptor,
}

impl ResolvedResource {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.resource.validate()?;
        validate_identifier("release_id", self.release_id.as_str())?;
        self.provenance.validate()?;
        if self.rights.is_empty() {
            return Err(ContractError::InvalidContract(
                "resolved resource has no rights statement".to_string(),
            ));
        }
        for statement in &self.rights {
            statement.validate()?;
        }
        self.use_policy.validate_with_rights(&self.rights)?;
        self.representation.validate()
    }
}

impl InstallSelection {
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.bounding_box.is_none()
            && self.languages.is_empty()
            && self.subjects.is_empty()
            && self.feature_types.is_empty()
            && self.columns.is_empty()
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        if let Some(bbox) = self.bounding_box {
            bbox.validate()?;
        }
        validate_short_values("selection.languages", &self.languages)?;
        validate_short_values("selection.subjects", &self.subjects)?;
        validate_short_values("selection.feature_types", &self.feature_types)?;
        validate_short_values("selection.columns", &self.columns)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InstallPlan {
    pub schema: String,
    pub installation_id: InstallationId,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub format: RepresentationFormat,
    pub selection: InstallSelection,
    pub catalog_authority: CatalogAuthority,
    pub resolved: ResolvedResource,
    pub rights: Vec<RightsStatement>,
    pub use_policy: UsePolicy,
    pub artifacts: Vec<PlannedArtifact>,
    pub total_download_bytes: u64,
    pub expected_installed_bytes: u64,
    pub available_bytes_observed: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub plan_sha256: String,
}

impl InstallPlan {
    /// Fingerprint the canonical JSON form with the fingerprint field replaced
    /// by 64 ASCII zeroes. Keeping this algorithm in the contract crate prevents
    /// planners and stores from drifting apart.
    pub fn compute_plan_sha256(&self) -> Result<String, ContractError> {
        let mut canonical = self.clone();
        canonical.plan_sha256 = "0".repeat(64);
        let encoded = serde_json::to_vec(&canonical)
            .map_err(|error| ContractError::Serialization(error.to_string()))?;
        Ok(format!("{:x}", Sha256::digest(encoded)))
    }

    pub fn refresh_plan_sha256(&mut self) -> Result<(), ContractError> {
        self.plan_sha256 = self.compute_plan_sha256()?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        require_schema(INSTALL_PLAN_SCHEMA, &self.schema)?;
        self.catalog_authority.validate()?;
        self.resolved.validate()?;
        self.selection.validate()?;
        validate_sha256(&self.plan_sha256)?;
        if self.compute_plan_sha256()? != self.plan_sha256 {
            return Err(ContractError::InvalidContract(
                "install plan fingerprint does not match its contents".to_string(),
            ));
        }
        if self.artifacts.is_empty() {
            return Err(ContractError::InvalidContract(
                "install plan has no artifacts".to_string(),
            ));
        }
        if self.resource_id != self.resolved.resource.id
            || self.release_id != self.resolved.release_id
            || self.representation_id != self.resolved.representation.id
            || self.format != self.resolved.representation.format
            || self.rights != self.resolved.rights
            || self.use_policy != self.resolved.use_policy
        {
            return Err(ContractError::InvalidContract(
                "install plan identity or policy disagrees with its frozen resource".to_string(),
            ));
        }
        if !self.selection.is_full() {
            return Err(ContractError::InvalidContract(
                "install plan contains an unmaterialized subset selection".to_string(),
            ));
        }
        let mut portable_file_names = BTreeSet::new();
        let mut planned_artifact_ids = BTreeSet::new();
        let total = self.artifacts.iter().try_fold(0_u64, |sum, artifact| {
            artifact.validate()?;
            if !planned_artifact_ids.insert(&artifact.artifact_id) {
                return Err(ContractError::DuplicateIdentifier(
                    artifact.artifact_id.to_string(),
                ));
            }
            if !portable_file_names.insert(artifact.file_name.to_ascii_lowercase()) {
                return Err(ContractError::InvalidContract(format!(
                    "install plan has an ASCII case-insensitive artifact file-name collision: {:?}",
                    artifact.file_name
                )));
            }
            sum.checked_add(artifact.expected_bytes)
                .ok_or(ContractError::IntegerOverflow)
        })?;
        if total != self.total_download_bytes {
            return Err(ContractError::InvalidContract(format!(
                "install plan total {0} does not equal artifact total {total}",
                self.total_download_bytes
            )));
        }
        if self.expected_installed_bytes != self.resolved.representation.expected_installed_bytes
            || self.artifacts.len() != self.resolved.representation.artifacts.len()
        {
            return Err(ContractError::InvalidContract(
                "install plan artifact set or installed size disagrees with its frozen representation"
                    .to_string(),
            ));
        }
        let declared_artifacts = self
            .resolved
            .representation
            .artifacts
            .iter()
            .map(|artifact| (artifact.id.clone(), artifact))
            .collect::<BTreeMap<_, _>>();
        for artifact in &self.artifacts {
            let declared = declared_artifacts
                .get(&artifact.artifact_id)
                .ok_or_else(|| {
                    ContractError::InvalidContract(format!(
                        "planned artifact {} is absent from the frozen representation",
                        artifact.artifact_id
                    ))
                })?;
            let planned_uri = Url::parse(&artifact.source_uri).map_err(|_| {
                ContractError::InvalidContract(format!(
                    "planned artifact {} has an invalid source URI",
                    artifact.artifact_id
                ))
            })?;
            if artifact.file_name != declared.file_name
                || artifact.expected_bytes != declared.expected_bytes
                || !sha256_equal(&artifact.sha256, &declared.sha256)
                || !declared
                    .mirrors
                    .iter()
                    .any(|mirror| Url::parse(&mirror.uri).is_ok_and(|uri| uri == planned_uri))
            {
                return Err(ContractError::InvalidContract(format!(
                    "planned artifact {} disagrees with its frozen representation",
                    artifact.artifact_id
                )));
            }
        }
        if self
            .available_bytes_observed
            .is_some_and(|available| available < self.expected_installed_bytes)
        {
            return Err(ContractError::InsufficientDiskSpace);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PlannedArtifact {
    pub artifact_id: ArtifactId,
    pub file_name: String,
    pub source_uri: String,
    pub expected_bytes: u64,
    pub sha256: String,
}

impl PlannedArtifact {
    fn validate(&self) -> Result<(), ContractError> {
        validate_file_name(&self.file_name)?;
        validate_durable_fetch_uri("planned_artifact.source_uri", &self.source_uri)?;
        validate_sha256(&self.sha256)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallationState {
    Planned,
    Fetching,
    Verifying,
    Activating,
    Ready,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InstallReceipt {
    pub schema: String,
    pub installation_id: InstallationId,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub format: RepresentationFormat,
    pub catalog_authority: CatalogAuthority,
    pub resolved: ResolvedResource,
    pub plan_sha256: String,
    pub state: InstallationState,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Whether a network acquisition was actually attempted. `None` means a
    /// terminal failure occurred at a boundary where that fact cannot be
    /// reconstructed honestly.
    pub network_attempted: Option<bool>,
    /// True only when at least one completed, digest-verified acquisition used
    /// HTTP(S). Partial transfers are reported separately.
    pub network_used: bool,
    /// Bytes represented by completed, digest-verified acquisition records.
    pub downloaded_bytes: u64,
    /// Bytes left in staging without a completed acquisition record after a
    /// failed or cancelled install.
    pub unverified_staged_bytes: u64,
    pub installed_relative_path: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<InstalledArtifact>,
    #[serde(default)]
    pub acquisitions: Vec<ArtifactAcquisition>,
    pub failure: Option<InformationError>,
}

impl InstallReceipt {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_schema(INSTALL_RECEIPT_SCHEMA, &self.schema)?;
        self.catalog_authority.validate()?;
        self.resolved.validate()?;
        validate_sha256(&self.plan_sha256)?;
        if self.resource_id != self.resolved.resource.id
            || self.release_id != self.resolved.release_id
            || self.representation_id != self.resolved.representation.id
            || self.format != self.resolved.representation.format
        {
            return Err(ContractError::InvalidContract(
                "install receipt identity disagrees with its frozen resource".to_string(),
            ));
        }
        let terminal_success = self.state == InstallationState::Ready;
        let terminal_failure = matches!(
            self.state,
            InstallationState::Failed | InstallationState::Cancelled
        );
        if terminal_success && (self.failure.is_some() || self.installed_relative_path.is_none())
            || terminal_failure
                && (self.failure.is_none() || self.installed_relative_path.is_some())
            || !terminal_success && !terminal_failure
        {
            return Err(ContractError::InvalidContract(
                "durable receipts must be terminal ready, failed, or cancelled states".to_string(),
            ));
        }
        let finished_at = self.finished_at.ok_or_else(|| {
            ContractError::InvalidContract(
                "durable terminal receipt must record its finish time".to_string(),
            )
        })?;
        if finished_at < self.started_at {
            return Err(ContractError::InvalidContract(
                "receipt finish time precedes its start time".to_string(),
            ));
        }
        let planned_artifacts = self
            .resolved
            .representation
            .artifacts
            .iter()
            .map(|artifact| (artifact.id.clone(), artifact))
            .collect::<BTreeMap<_, _>>();
        let mut acquisition_ids = BTreeSet::new();
        for acquisition in &self.acquisitions {
            acquisition.validate()?;
            if acquisition.finished_at < self.started_at
                || acquisition.finished_at > finished_at
                || acquisition
                    .started_at
                    .is_some_and(|started| started < self.started_at || started > finished_at)
            {
                return Err(ContractError::InvalidContract(
                    "receipt timestamps do not contain every acquisition".to_string(),
                ));
            }
            if acquisition.attempts.iter().any(|attempt| {
                attempt.started_at < self.started_at
                    || attempt
                        .finished_at
                        .is_some_and(|attempt_end| attempt_end > finished_at)
            }) {
                return Err(ContractError::InvalidContract(
                    "receipt timestamps do not contain every network attempt".to_string(),
                ));
            }
            if !acquisition_ids.insert(&acquisition.artifact_id) {
                return Err(ContractError::DuplicateIdentifier(
                    acquisition.artifact_id.to_string(),
                ));
            }
            let planned = planned_artifacts
                .get(&acquisition.artifact_id)
                .ok_or_else(|| {
                    ContractError::InvalidContract(format!(
                        "receipt acquisition references unplanned artifact {}",
                        acquisition.artifact_id
                    ))
                })?;
            if acquisition.verified_bytes != planned.expected_bytes
                || !sha256_equal(&acquisition.sha256, &planned.sha256)
            {
                return Err(ContractError::InvalidContract(format!(
                    "receipt acquisition for {} disagrees with the frozen representation",
                    acquisition.artifact_id
                )));
            }
        }
        let observed_network = self.acquisitions.iter().any(|acquisition| {
            matches!(
                acquisition.transport,
                AcquisitionTransport::Http | AcquisitionTransport::Https
            )
        });
        let observed_bytes = self
            .acquisitions
            .iter()
            .try_fold(0_u64, |sum, acquisition| {
                sum.checked_add(acquisition.verified_bytes)
                    .ok_or(ContractError::IntegerOverflow)
            })?;
        if observed_network != self.network_used || observed_bytes != self.downloaded_bytes {
            return Err(ContractError::InvalidContract(
                "receipt acquisition accounting is inconsistent".to_string(),
            ));
        }
        let planned_bytes =
            self.resolved
                .representation
                .artifacts
                .iter()
                .try_fold(0_u64, |sum, artifact| {
                    sum.checked_add(artifact.expected_bytes)
                        .ok_or(ContractError::IntegerOverflow)
                })?;
        let staged_accounting = self
            .downloaded_bytes
            .checked_add(self.unverified_staged_bytes)
            .ok_or(ContractError::IntegerOverflow)?;
        if staged_accounting > planned_bytes {
            return Err(ContractError::InvalidContract(
                "receipt accounts for more staged bytes than the frozen representation".to_string(),
            ));
        }
        if self.network_used && self.network_attempted != Some(true) {
            return Err(ContractError::InvalidContract(
                "verified network use requires an affirmative attempt fact".to_string(),
            ));
        }
        if terminal_success
            && (self.artifacts.len() != self.resolved.representation.artifacts.len()
                || self.acquisitions.len() != self.artifacts.len())
        {
            return Err(ContractError::InvalidContract(
                "ready receipt requires one installed and acquisition record per planned artifact"
                    .to_string(),
            ));
        }
        if terminal_success {
            if self.network_attempted != Some(self.network_used)
                || self.unverified_staged_bytes != 0
            {
                return Err(ContractError::InvalidContract(
                    "ready receipt has incomplete network or staging accounting".to_string(),
                ));
            }
            let mut installed_ids = BTreeSet::new();
            for installed in &self.artifacts {
                validate_sha256(&installed.sha256)?;
                require_text("installed_artifact.relative_path", &installed.relative_path)?;
                if !installed_ids.insert(&installed.artifact_id) {
                    return Err(ContractError::DuplicateIdentifier(
                        installed.artifact_id.to_string(),
                    ));
                }
                let planned = planned_artifacts
                    .get(&installed.artifact_id)
                    .ok_or_else(|| {
                        ContractError::InvalidContract(format!(
                            "installed receipt references unplanned artifact {}",
                            installed.artifact_id
                        ))
                    })?;
                let acquisition = self
                    .acquisitions
                    .iter()
                    .find(|candidate| candidate.artifact_id == installed.artifact_id)
                    .ok_or_else(|| {
                        ContractError::InvalidContract(
                            "ready receipt artifact has no acquisition record".to_string(),
                        )
                    })?;
                if installed.bytes != planned.expected_bytes
                    || !sha256_equal(&installed.sha256, &planned.sha256)
                    || acquisition.verified_bytes != installed.bytes
                    || !sha256_equal(&acquisition.sha256, &installed.sha256)
                {
                    return Err(ContractError::InvalidContract(
                        "ready receipt artifact, acquisition, and frozen representation disagree"
                            .to_string(),
                    ));
                }
            }
        } else if !self.artifacts.is_empty() {
            return Err(ContractError::InvalidContract(
                "failed or cancelled receipt cannot claim installed artifacts".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstalledArtifact {
    pub artifact_id: ArtifactId,
    pub relative_path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAcquisition {
    pub artifact_id: ArtifactId,
    pub transport: AcquisitionTransport,
    pub requested_uri: Option<String>,
    pub final_uri: Option<String>,
    pub final_peer_address: Option<String>,
    #[serde(default)]
    pub redirect_chain: Vec<AcquisitionRedirect>,
    /// Ordered source contacts that contributed bytes or were superseded by
    /// a later restart while producing this verified artifact.
    pub attempts: Vec<AcquisitionAttempt>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: DateTime<Utc>,
    pub resumed_bytes: u64,
    pub verified_bytes: u64,
    pub sha256: String,
}

impl ArtifactAcquisition {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_identifier("acquisition.artifact_id", self.artifact_id.as_str())?;
        validate_sha256(&self.sha256)?;
        if self
            .started_at
            .is_some_and(|started| started > self.finished_at)
        {
            return Err(ContractError::InvalidContract(
                "acquisition finished before it started".to_string(),
            ));
        }
        if self.resumed_bytes > self.verified_bytes
            || self.redirect_chain.len() > 32
            || self.attempts.len() > MAX_ACQUISITION_ATTEMPTS
        {
            return Err(ContractError::InvalidContract(
                "acquisition resume or redirect accounting is invalid".to_string(),
            ));
        }
        for redirect in &self.redirect_chain {
            redirect.validate()?;
        }
        match self.transport {
            AcquisitionTransport::PreexistingStage => {
                if self.requested_uri.is_some()
                    || self.final_uri.is_some()
                    || self.final_peer_address.is_some()
                    || !self.redirect_chain.is_empty()
                    || !self.attempts.is_empty()
                    || self.started_at.is_some()
                {
                    return Err(ContractError::InvalidContract(
                        "preexisting staged bytes cannot claim source transport facts".to_string(),
                    ));
                }
            }
            AcquisitionTransport::File => {
                require_acquisition_uri(&self.requested_uri, "file")?;
                require_acquisition_uri(&self.final_uri, "file")?;
                if self.final_peer_address.is_some()
                    || !self.redirect_chain.is_empty()
                    || self.attempts.len() != 1
                    || self.started_at.is_none()
                {
                    return Err(ContractError::InvalidContract(
                        "file acquisition has invalid network or timing facts".to_string(),
                    ));
                }
                let attempt = &self.attempts[0];
                attempt.validate(AcquisitionTransport::File, self.verified_bytes)?;
                if attempt.byte_start != 0
                    || attempt.byte_end != self.verified_bytes
                    || self.requested_uri.as_deref() != Some(attempt.requested_uri.as_str())
                    || self.final_uri.as_deref() != Some(attempt.final_uri.as_str())
                {
                    return Err(ContractError::InvalidContract(
                        "file acquisition summary disagrees with its source attempt".to_string(),
                    ));
                }
            }
            AcquisitionTransport::Http | AcquisitionTransport::Https => {
                let requested_scheme = if self.transport == AcquisitionTransport::Http {
                    "http"
                } else {
                    "https"
                };
                require_acquisition_uri(&self.requested_uri, requested_scheme)?;
                let final_uri = self.final_uri.as_deref().ok_or_else(|| {
                    ContractError::InvalidContract(
                        "network acquisition has no final URI".to_string(),
                    )
                })?;
                if !final_uri.starts_with("http://") && !final_uri.starts_with("https://") {
                    return Err(ContractError::InvalidContract(
                        "network acquisition final URI is not HTTP(S)".to_string(),
                    ));
                }
                if self
                    .final_peer_address
                    .as_deref()
                    .is_none_or(|peer| peer.trim().is_empty())
                    || self.started_at.is_none()
                    || self.attempts.is_empty()
                {
                    return Err(ContractError::InvalidContract(
                        "network acquisition lacks peer or timing attestation".to_string(),
                    ));
                }
                let mut prior_end = None;
                for (index, attempt) in self.attempts.iter().enumerate() {
                    attempt.validate(self.transport, self.verified_bytes)?;
                    if index == 0 && attempt.byte_start != 0
                        || index > 0
                            && attempt.byte_start != 0
                            && prior_end != Some(attempt.byte_start)
                    {
                        return Err(ContractError::InvalidContract(
                            "network acquisition attempt byte ranges are discontinuous".to_string(),
                        ));
                    }
                    prior_end = Some(attempt.byte_end);
                }
                let last = self.attempts.last().ok_or_else(|| {
                    ContractError::InvalidContract(
                        "network acquisition has no attempt history".to_string(),
                    )
                })?;
                if last.byte_end != self.verified_bytes
                    || self.requested_uri.as_deref() != Some(last.requested_uri.as_str())
                    || self.final_uri.as_deref() != Some(last.final_uri.as_str())
                    || self.final_peer_address.as_deref() != last.final_peer_address.as_deref()
                    || self.redirect_chain != last.redirect_chain
                {
                    return Err(ContractError::InvalidContract(
                        "network acquisition summary disagrees with its final attempt".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionAttempt {
    pub requested_uri: String,
    pub final_uri: String,
    pub final_peer_address: Option<String>,
    #[serde(default)]
    pub redirect_chain: Vec<AcquisitionRedirect>,
    pub byte_start: u64,
    pub byte_end: u64,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl AcquisitionAttempt {
    fn validate(
        &self,
        transport: AcquisitionTransport,
        verified_bytes: u64,
    ) -> Result<(), ContractError> {
        if self.byte_start > self.byte_end
            || self.byte_end > verified_bytes
            || self.redirect_chain.len() > 32
            || self
                .finished_at
                .is_some_and(|finished| finished < self.started_at)
        {
            return Err(ContractError::InvalidContract(
                "network acquisition attempt range, redirect, or timing is invalid".to_string(),
            ));
        }
        if transport == AcquisitionTransport::File {
            let requested = validate_file_acquisition_url(
                "acquisition.attempt.requested_uri",
                &self.requested_uri,
            )?;
            let final_uri =
                validate_file_acquisition_url("acquisition.attempt.final_uri", &self.final_uri)?;
            if requested != final_uri
                || self.final_peer_address.is_some()
                || !self.redirect_chain.is_empty()
            {
                return Err(ContractError::InvalidContract(
                    "file acquisition attempt contains network source facts".to_string(),
                ));
            }
            return Ok(());
        }
        let expected_requested_scheme = if transport == AcquisitionTransport::Http {
            "http"
        } else if transport == AcquisitionTransport::Https {
            "https"
        } else {
            return Err(ContractError::InvalidContract(
                "preexisting bytes cannot contain source attempts".to_string(),
            ));
        };
        let mut current = validate_acquisition_url(
            "acquisition.attempt.requested_uri",
            &self.requested_uri,
            Some(expected_requested_scheme),
        )?;
        for redirect in &self.redirect_chain {
            redirect.validate()?;
            let from = validate_acquisition_url(
                "acquisition.attempt.redirect.from_uri",
                &redirect.from_uri,
                None,
            )?;
            let to = validate_acquisition_url(
                "acquisition.attempt.redirect.to_uri",
                &redirect.to_uri,
                None,
            )?;
            if from != current || current.scheme() == "https" && to.scheme() == "http" {
                return Err(ContractError::InvalidContract(
                    "network acquisition redirect chain is inconsistent".to_string(),
                ));
            }
            current = to;
        }
        let final_uri =
            validate_acquisition_url("acquisition.attempt.final_uri", &self.final_uri, None)?;
        if current != final_uri
            || self
                .final_peer_address
                .as_deref()
                .is_none_or(|peer| !is_ip_socket_address(peer))
        {
            return Err(ContractError::InvalidContract(
                "network acquisition final source or peer is invalid".to_string(),
            ));
        }
        Ok(())
    }
}

fn validate_file_acquisition_url(field: &str, value: &str) -> Result<Url, ContractError> {
    require_text(field, value)?;
    let url = Url::parse(value)
        .map_err(|_| ContractError::InvalidContract(format!("{field} is not an absolute URL")))?;
    if url.scheme() != "file"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ContractError::InvalidContract(format!(
            "{field} is not a durable, secret-free file URL"
        )));
    }
    Ok(url)
}

fn require_acquisition_uri(
    value: &Option<String>,
    expected_scheme: &str,
) -> Result<(), ContractError> {
    let value = value.as_deref().ok_or_else(|| {
        ContractError::InvalidContract("acquisition source URI is missing".to_string())
    })?;
    require_text("acquisition.source_uri", value)?;
    let expected_prefix = format!("{expected_scheme}://");
    if !value.starts_with(&expected_prefix) {
        return Err(ContractError::InvalidContract(format!(
            "acquisition source URI does not use {expected_scheme}"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionTransport {
    Https,
    Http,
    File,
    PreexistingStage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionRedirect {
    pub status: u16,
    pub from_uri: String,
    pub to_uri: String,
    pub peer_address: Option<String>,
}

impl AcquisitionRedirect {
    fn validate(&self) -> Result<(), ContractError> {
        if !matches!(self.status, 301 | 302 | 303 | 307 | 308) {
            return Err(ContractError::InvalidContract(
                "acquisition redirect status is not a redirect".to_string(),
            ));
        }
        require_text("acquisition.redirect.from_uri", &self.from_uri)?;
        require_text("acquisition.redirect.to_uri", &self.to_uri)?;
        if self
            .peer_address
            .as_deref()
            .is_none_or(|peer| !is_ip_socket_address(peer))
        {
            return Err(ContractError::InvalidContract(
                "acquisition redirect peer address is missing or invalid".to_string(),
            ));
        }
        Ok(())
    }
}

fn is_ip_socket_address(value: &str) -> bool {
    let Ok(url) = Url::parse(&format!("http://{value}")) else {
        return false;
    };
    url.username().is_empty()
        && url.password().is_none()
        && url.port().is_some()
        && matches!(url.host(), Some(Host::Ipv4(_) | Host::Ipv6(_)))
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == "/"
}

fn validate_acquisition_url(
    field: &str,
    value: &str,
    expected_scheme: Option<&str>,
) -> Result<Url, ContractError> {
    require_text(field, value)?;
    let url = Url::parse(value)
        .map_err(|_| ContractError::InvalidContract(format!("{field} is not an absolute URL")))?;
    if !matches!(url.scheme(), "http" | "https")
        || expected_scheme.is_some_and(|expected| url.scheme() != expected)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ContractError::InvalidContract(format!(
            "{field} is not a durable, secret-free HTTP(S) URL"
        )));
    }
    Ok(url)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAccessMode {
    LiveReadOnly,
    ImmutableReadOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalRegistration {
    pub schema: String,
    pub installation_id: InstallationId,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub format: RepresentationFormat,
    pub absolute_path: String,
    pub access_mode: ExternalAccessMode,
    pub identity: SourceIdentity,
    pub provenance: Provenance,
    #[serde(default)]
    pub rights: Vec<RightsStatement>,
    #[serde(default)]
    pub use_policy: UsePolicy,
    pub registered_at: DateTime<Utc>,
}

impl ExternalRegistration {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_schema(EXTERNAL_REGISTRATION_SCHEMA, &self.schema)?;
        require_text("external.absolute_path", &self.absolute_path)?;
        self.identity.validate()?;
        self.provenance.validate()?;
        for statement in &self.rights {
            statement.validate()?;
        }
        self.use_policy.validate_with_rights(&self.rights)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub bytes: u64,
    pub modified_unix_nanos: Option<u128>,
    #[serde(default)]
    pub device: Option<u64>,
    #[serde(default)]
    pub inode: Option<u64>,
    pub sha256: Option<String>,
}

impl SourceIdentity {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.device.is_some() != self.inode.is_some() {
            return Err(ContractError::InvalidContract(
                "source identity device and inode must be supplied together".to_string(),
            ));
        }
        if let Some(digest) = &self.sha256 {
            validate_sha256(digest)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InformationQuery {
    pub schema: String,
    pub query_id: QueryId,
    pub text: String,
    pub syntax: QuerySyntax,
    pub purpose: RetrievalPurpose,
    #[serde(default)]
    pub targets: Vec<RetrievalTarget>,
    #[serde(default)]
    pub resources: Vec<ResourceId>,
    #[serde(default)]
    pub representations: Vec<RepresentationId>,
    pub filters: QueryFilters,
    pub budget: QueryBudget,
}

impl InformationQuery {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_schema(QUERY_SCHEMA, &self.schema)?;
        require_text("query.text", &self.text)?;
        if self.text.chars().count() > 8_192 {
            return Err(ContractError::LimitExceeded(
                "query text exceeds 8192 characters".to_string(),
            ));
        }
        self.filters.validate()?;
        validate_id_list("query.resources", &self.resources)?;
        validate_id_list("query.representations", &self.representations)?;
        validate_id_list("query.targets", &self.targets)?;
        for resource in &self.resources {
            validate_identifier("query.resource_id", resource.as_str())?;
        }
        for representation in &self.representations {
            validate_identifier("query.representation_id", representation.as_str())?;
        }
        for target in &self.targets {
            target.validate()?;
        }
        let mut aggregate_bytes = self.text.len();
        for value in self
            .filters
            .languages
            .iter()
            .chain(&self.filters.subjects)
            .chain(&self.filters.document_ids)
        {
            aggregate_bytes = aggregate_bytes
                .checked_add(value.len())
                .ok_or(ContractError::IntegerOverflow)?;
        }
        for (key, value) in &self.filters.fields {
            aggregate_bytes = aggregate_bytes
                .checked_add(key.len())
                .and_then(|sum| sum.checked_add(value.len()))
                .ok_or(ContractError::IntegerOverflow)?;
        }
        if aggregate_bytes > 1_048_576 {
            return Err(ContractError::LimitExceeded(
                "query and filter text exceeds one MiB".to_string(),
            ));
        }
        if self.purpose == RetrievalPurpose::ModelContext && self.targets.is_empty() {
            return Err(ContractError::InvalidContract(
                "model-context retrieval requires at least one exact target".to_string(),
            ));
        }
        self.budget.validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct RetrievalTarget {
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
}

impl RetrievalTarget {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_identifier("target.resource_id", self.resource_id.as_str())?;
        validate_identifier("target.release_id", self.release_id.as_str())?;
        validate_identifier("target.representation_id", self.representation_id.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuerySyntax {
    #[default]
    NaturalTerms,
    ExactPhrase,
    AllTerms,
    AnyTerms,
    BackendNative,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct QueryFilters {
    pub languages: Vec<String>,
    pub subjects: Vec<String>,
    pub document_ids: Vec<String>,
    pub spatial: Option<BoundingBox>,
    pub temporal_start: Option<DateTime<Utc>>,
    pub temporal_end: Option<DateTime<Utc>>,
    pub fields: BTreeMap<String, String>,
}

impl QueryFilters {
    fn validate(&self) -> Result<(), ContractError> {
        validate_short_values("filters.languages", &self.languages)?;
        validate_short_values("filters.subjects", &self.subjects)?;
        validate_short_values("filters.document_ids", &self.document_ids)?;
        if self.document_ids.len() > 256 {
            return Err(ContractError::LimitExceeded(
                "filters.document_ids has more than 256 entries".to_string(),
            ));
        }
        if self.fields.len() > 128 {
            return Err(ContractError::LimitExceeded(
                "filters.fields has more than 128 entries".to_string(),
            ));
        }
        for (key, value) in &self.fields {
            require_text("filters.fields key", key)?;
            require_text("filters.fields value", value)?;
            if key.len() > 128 || value.len() > 2_048 {
                return Err(ContractError::LimitExceeded(
                    "filters.fields contains an oversized key or value".to_string(),
                ));
            }
        }
        if let Some(spatial) = self.spatial {
            spatial.validate()?;
        }
        if self
            .temporal_start
            .zip(self.temporal_end)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(ContractError::InvalidContract(
                "query temporal_start is after temporal_end".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct QueryBudget {
    pub max_hits: u32,
    pub max_hits_per_backend: u32,
    pub max_backends: u16,
    pub max_context_chars: u32,
    pub timeout_ms: u64,
}

impl Default for QueryBudget {
    fn default() -> Self {
        Self {
            max_hits: 20,
            max_hits_per_backend: 10,
            max_backends: 8,
            max_context_chars: 24_000,
            timeout_ms: 10_000,
        }
    }
}

impl QueryBudget {
    fn validate(&self) -> Result<(), ContractError> {
        if self.max_hits == 0
            || self.max_hits_per_backend == 0
            || self.max_backends == 0
            || self.max_context_chars == 0
            || self.timeout_ms == 0
            || self.timeout_ms > MAX_RETRIEVAL_TIMEOUT_MS
            || self.max_hits > 1_000
            || self.max_context_chars > 2_000_000
        {
            return Err(ContractError::InvalidBudget);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSet {
    pub schema: String,
    pub query_id: QueryId,
    pub complete: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub hits: Vec<EvidenceHit>,
    pub elapsed_ms: u64,
}

impl EvidenceSet {
    pub fn validate(&self, budget: &QueryBudget) -> Result<(), ContractError> {
        require_schema(EVIDENCE_SCHEMA, &self.schema)?;
        if self.hits.len() > budget.max_hits as usize {
            return Err(ContractError::LimitExceeded(
                "evidence hit count exceeds query budget".to_string(),
            ));
        }
        let mut ids = BTreeSet::new();
        let mut chars = 0_u32;
        for hit in &self.hits {
            hit.validate()?;
            if !ids.insert(&hit.evidence_id) {
                return Err(ContractError::DuplicateIdentifier(hit.evidence_id.clone()));
            }
            let hit_chars = hit
                .snippet
                .chars()
                .count()
                .saturating_add(hit.context.chars().count());
            let hit_chars = u32::try_from(hit_chars).map_err(|_| ContractError::IntegerOverflow)?;
            chars = chars
                .checked_add(hit_chars)
                .ok_or(ContractError::IntegerOverflow)?;
        }
        if chars > budget.max_context_chars {
            return Err(ContractError::LimitExceeded(
                "evidence text exceeds query context budget".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceHit {
    pub evidence_id: String,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub backend_id: String,
    pub rank: u32,
    pub score: EvidenceScore,
    pub title: String,
    pub creator: Option<String>,
    pub snippet: String,
    pub context: String,
    pub excerpt_sha256: String,
    pub source_fingerprint: Option<String>,
    pub document_id: Option<String>,
    pub passage_id: Option<String>,
    pub locator: EvidenceLocator,
    pub source_uri: Option<String>,
    pub provenance: Provenance,
    #[serde(default)]
    pub rights: Vec<RightsStatement>,
    #[serde(default)]
    pub use_policy: UsePolicy,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl EvidenceHit {
    pub fn validate(&self) -> Result<(), ContractError> {
        require_text("evidence_id", &self.evidence_id)?;
        require_text("evidence.backend_id", &self.backend_id)?;
        require_text("evidence.title", &self.title)?;
        if self.rank == 0 || !self.score.value.is_finite() {
            return Err(ContractError::InvalidContract(
                "evidence rank and score are invalid".to_string(),
            ));
        }
        validate_sha256(&self.excerpt_sha256)?;
        if !self
            .excerpt_sha256
            .strip_prefix("sha256:")
            .unwrap_or(&self.excerpt_sha256)
            .eq_ignore_ascii_case(&evidence_text_sha256(&self.snippet, &self.context))
        {
            return Err(ContractError::InvalidContract(
                "evidence text digest does not match snippet and context".to_string(),
            ));
        }
        if let Some(fingerprint) = &self.source_fingerprint {
            require_text("evidence.source_fingerprint", fingerprint)?;
        }
        self.provenance.validate()?;
        for statement in &self.rights {
            statement.validate()?;
        }
        self.use_policy.validate_with_rights(&self.rights)?;
        self.locator.validate()
    }
}

/// Hash all source text returned in an evidence envelope. Length prefixes make
/// the `(snippet, context)` boundary unambiguous. Display metadata and labels
/// are intentionally outside this digest.
#[must_use]
pub fn evidence_text_sha256(snippet: &str, context: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"information-native.evidence-text.v1\0");
    let snippet_len = u64::try_from(snippet.len()).map_or(u64::MAX, |value| value);
    hasher.update(snippet_len.to_le_bytes());
    hasher.update(snippet.as_bytes());
    let context_len = u64::try_from(context.len()).map_or(u64::MAX, |value| value);
    hasher.update(context_len.to_le_bytes());
    hasher.update(context.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceScore {
    pub value: f64,
    pub semantics: ScoreSemantics,
    pub fused_value: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScoreSemantics {
    HigherIsBetter,
    LowerIsBetter,
    RankOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceLocator {
    SqliteBlock {
        block_id: String,
        doc_id: String,
        block_index: i64,
        location_path: Option<String>,
    },
    ZimArticle {
        archive_uuid: Option<String>,
        internal_path: String,
    },
    Page {
        page: u32,
        section: Option<String>,
    },
    MediaTime {
        start_seconds: f64,
        end_seconds: Option<f64>,
    },
    SpatialFeature {
        feature_id: String,
        bounding_box: Option<BoundingBox>,
    },
    OsmElement {
        element_type: String,
        element_id: u64,
        version: Option<u32>,
    },
    OvertureFeature {
        gers_id: String,
    },
    Record {
        collection: Option<String>,
        key: String,
    },
    WebArchiveRecord {
        record_id: String,
        warc_file: String,
        offset: u64,
        length: u64,
    },
}

impl EvidenceLocator {
    fn validate(&self) -> Result<(), ContractError> {
        match self {
            Self::SqliteBlock {
                block_id, doc_id, ..
            } => {
                require_text("locator.block_id", block_id)?;
                require_text("locator.doc_id", doc_id)
            }
            Self::ZimArticle { internal_path, .. } => {
                require_text("locator.internal_path", internal_path)
            }
            Self::Page { page, .. } if *page == 0 => Err(ContractError::InvalidContract(
                "page locators are one-based".to_string(),
            )),
            Self::MediaTime {
                start_seconds,
                end_seconds,
            } if !start_seconds.is_finite()
                || *start_seconds < 0.0
                || end_seconds.is_some_and(|end| !end.is_finite() || end < *start_seconds) =>
            {
                Err(ContractError::InvalidContract(
                    "media time locator is invalid".to_string(),
                ))
            }
            Self::SpatialFeature {
                feature_id,
                bounding_box,
            } => {
                require_text("locator.feature_id", feature_id)?;
                if let Some(bbox) = bounding_box {
                    bbox.validate()?;
                }
                Ok(())
            }
            Self::OsmElement { element_type, .. } => {
                require_text("locator.element_type", element_type)
            }
            Self::OvertureFeature { gers_id } => require_text("locator.gers_id", gers_id),
            Self::Record { key, .. } => require_text("locator.key", key),
            Self::WebArchiveRecord {
                record_id,
                warc_file,
                length,
                ..
            } => {
                require_text("locator.record_id", record_id)?;
                require_text("locator.warc_file", warc_file)?;
                if *length == 0 {
                    return Err(ContractError::InvalidContract(
                        "web archive record length is zero".to_string(),
                    ));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    InvalidInput,
    NotFound,
    Unsupported,
    Integrity,
    Permission,
    ResourceBusy,
    Network,
    Io,
    Backend,
    Internal,
}

#[derive(Debug, Clone, Error, Serialize, Deserialize, PartialEq, Eq)]
#[error("{safe_message}")]
#[serde(deny_unknown_fields)]
pub struct InformationError {
    pub code: String,
    pub class: ErrorClass,
    pub safe_message: String,
    pub retryable: bool,
    pub resource_id: Option<ResourceId>,
    pub representation_id: Option<RepresentationId>,
}

impl InformationError {
    #[must_use]
    pub fn new(
        class: ErrorClass,
        code: impl Into<String>,
        safe_message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            class,
            safe_message: safe_message.into(),
            retryable: false,
            resource_id: None,
            representation_id: None,
        }
    }

    #[must_use]
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("schema mismatch: expected {expected}, got {actual}")]
    SchemaMismatch { expected: String, actual: String },
    #[error("invalid {kind} identifier: {value:?}")]
    InvalidIdentifier { kind: String, value: String },
    #[error("duplicate identifier: {0}")]
    DuplicateIdentifier(String),
    #[error("required field is empty: {0}")]
    EmptyField(String),
    #[error("invalid SHA-256 digest: {0}")]
    InvalidSha256(String),
    #[error("invalid file name: {0:?}")]
    InvalidFileName(String),
    #[error("invalid bounding box")]
    InvalidBoundingBox,
    #[error("invalid query budget")]
    InvalidBudget,
    #[error("insufficient disk space")]
    InsufficientDiskSpace,
    #[error("integer overflow")]
    IntegerOverflow,
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),
    #[error("invalid contract: {0}")]
    InvalidContract(String),
    #[error("contract serialization failed: {0}")]
    Serialization(String),
}

fn require_schema(expected: &str, actual: &str) -> Result<(), ContractError> {
    if expected != actual {
        return Err(ContractError::SchemaMismatch {
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}

fn require_text(field: &str, value: &str) -> Result<(), ContractError> {
    if value.trim().is_empty() {
        return Err(ContractError::EmptyField(field.to_string()));
    }
    Ok(())
}

fn validate_short_values(field: &str, values: &[String]) -> Result<(), ContractError> {
    if values.len() > 1_024 {
        return Err(ContractError::LimitExceeded(format!(
            "{field} has more than 1024 entries"
        )));
    }
    for value in values {
        require_text(field, value)?;
        if value.len() > 512 {
            return Err(ContractError::LimitExceeded(format!(
                "{field} contains a value longer than 512 bytes"
            )));
        }
    }
    Ok(())
}

fn validate_id_list<T>(field: &str, values: &[T]) -> Result<(), ContractError> {
    if values.len() > 1_024 {
        return Err(ContractError::LimitExceeded(format!(
            "{field} has more than 1024 entries"
        )));
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), ContractError> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ContractError::InvalidSha256(value.to_string()));
    }
    Ok(())
}

fn sha256_equal(left: &str, right: &str) -> bool {
    left.strip_prefix("sha256:")
        .unwrap_or(left)
        .eq_ignore_ascii_case(right.strip_prefix("sha256:").unwrap_or(right))
}

fn validate_durable_fetch_uri(field: &str, value: &str) -> Result<(), ContractError> {
    require_text(field, value)?;
    let uri = Url::parse(value).map_err(|_| {
        ContractError::InvalidContract(format!(
            "{field} must be an absolute http, https, or file URI"
        ))
    })?;
    if !matches!(uri.scheme(), "http" | "https" | "file") {
        return Err(ContractError::InvalidContract(format!(
            "{field} has an unsupported URI scheme"
        )));
    }
    if !uri.username().is_empty() || uri.password().is_some() {
        return Err(ContractError::InvalidContract(format!(
            "{field} cannot persist URI credentials"
        )));
    }
    if uri.query().is_some() || uri.fragment().is_some() {
        return Err(ContractError::InvalidContract(format!(
            "{field} cannot persist query parameters or fragments; use a credential-free immutable mirror"
        )));
    }
    Ok(())
}

fn validate_file_name(value: &str) -> Result<(), ContractError> {
    let stem = value.split('.').next().unwrap_or(value);
    let windows_reserved = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", "conin$",
        "conout$",
    ];
    if value.is_empty()
        || value.len() > 255
        || value == "."
        || value == ".."
        || value.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
        || value.ends_with([' ', '.'])
        || windows_reserved
            .iter()
            .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return Err(ContractError::InvalidFileName(value.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_and_paths_are_restricted() {
        assert!(ResourceId::parse("org.kiwix.wikipedia/en").is_ok());
        assert!(ResourceId::parse("../../escape").is_err());
        assert!(validate_file_name("payload.zim").is_ok());
        assert!(validate_file_name("../payload.zim").is_err());
    }

    #[test]
    fn identifier_deserialization_cannot_bypass_validation() {
        macro_rules! assert_invalid_json_id {
            ($id_type:ty) => {
                assert!(serde_json::from_str::<$id_type>(r#""../../escape""#).is_err());
            };
        }
        assert_invalid_json_id!(ResourceId);
        assert_invalid_json_id!(ReleaseId);
        assert_invalid_json_id!(RepresentationId);
        assert_invalid_json_id!(ArtifactId);
        assert_invalid_json_id!(InstallationId);
        assert_invalid_json_id!(QueryId);

        let query_id =
            QueryId::try_from("query:validated".to_string()).expect("valid query id was rejected");
        let encoded = serde_json::to_string(&query_id).expect("query id did not serialize");
        let decoded: QueryId =
            serde_json::from_str(&encoded).expect("valid query id did not deserialize");
        assert_eq!(decoded, query_id);
        assert_eq!(decoded.as_ref(), "query:validated");
        assert!(QueryId::try_from("query with spaces".to_string()).is_err());
    }

    #[test]
    fn artifact_file_names_use_a_portable_subset() {
        for invalid in [
            "bad<name",
            "bad>name",
            "bad:name",
            "bad\"name",
            "bad/name",
            "bad\\name",
            "bad|name",
            "bad?name",
            "bad*name",
            "bad\nname",
            "bad\u{7f}name",
            "payload.",
            "payload ",
            "NUL.txt",
            "conin$",
        ] {
            assert!(
                validate_file_name(invalid).is_err(),
                "non-portable artifact file name was accepted: {invalid:?}"
            );
        }
        assert!(validate_file_name("portable-payload_01.zim").is_ok());
    }

    #[test]
    fn durable_fetch_uris_never_persist_credentials_or_signed_queries() {
        assert!(validate_durable_fetch_uri("source", "HTTPS://EXAMPLE.COM/archive.zim").is_ok());
        for invalid in [
            "https://user:secret@example.com/archive.zim",
            "https://example.com/archive.zim?X-Amz-Signature=secret",
            "https://example.com/archive.zim#fragment",
        ] {
            assert!(validate_durable_fetch_uri("source", invalid).is_err());
        }
    }

    #[test]
    fn network_acquisition_history_is_contiguous_and_secret_free() -> Result<(), ContractError> {
        assert!(is_ip_socket_address("203.0.113.8:443"));
        assert!(is_ip_socket_address("[2001:db8::8]:443"));
        assert!(!is_ip_socket_address("example.test:443"));
        let started = Utc::now();
        let finished = started + chrono::Duration::seconds(1);
        let first = AcquisitionAttempt {
            requested_uri: "https://example.test/archive.zim".to_string(),
            final_uri: "https://cdn.example.test/archive.zim".to_string(),
            final_peer_address: Some("203.0.113.8:443".to_string()),
            redirect_chain: vec![AcquisitionRedirect {
                status: 302,
                from_uri: "https://example.test/archive.zim".to_string(),
                to_uri: "https://cdn.example.test/archive.zim".to_string(),
                peer_address: Some("203.0.113.7:443".to_string()),
            }],
            byte_start: 0,
            byte_end: 4,
            started_at: started,
            finished_at: None,
        };
        let second = AcquisitionAttempt {
            byte_start: 4,
            byte_end: 10,
            started_at: finished,
            finished_at: Some(finished),
            ..first.clone()
        };
        let mut acquisition = ArtifactAcquisition {
            artifact_id: ArtifactId::parse("payload")?,
            transport: AcquisitionTransport::Https,
            requested_uri: Some(second.requested_uri.clone()),
            final_uri: Some(second.final_uri.clone()),
            final_peer_address: second.final_peer_address.clone(),
            redirect_chain: second.redirect_chain.clone(),
            attempts: vec![first, second],
            started_at: Some(started),
            finished_at: finished,
            resumed_bytes: 4,
            verified_bytes: 10,
            sha256: "0".repeat(64),
        };
        acquisition.validate()?;

        acquisition.attempts[1].byte_start = 5;
        assert!(acquisition.validate().is_err());
        acquisition.attempts[1].byte_start = 4;
        acquisition.attempts[1].final_uri =
            "https://cdn.example.test/archive.zim?signature=secret".to_string();
        acquisition.final_uri = Some(acquisition.attempts[1].final_uri.clone());
        assert!(acquisition.validate().is_err());
        Ok(())
    }

    #[test]
    fn bounding_boxes_reject_non_finite_and_inverted_values() {
        let good = BoundingBox {
            west: -71.1,
            south: 42.3,
            east: -71.0,
            north: 42.4,
        };
        assert!(good.validate().is_ok());
        assert!(
            BoundingBox {
                east: -72.0,
                ..good
            }
            .validate()
            .is_err()
        );
        assert!(
            BoundingBox {
                north: f64::NAN,
                ..good
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn query_budget_has_hard_caps() {
        let query = InformationQuery {
            schema: QUERY_SCHEMA.to_string(),
            query_id: QueryId::new(),
            text: "infused contemplation".to_string(),
            syntax: QuerySyntax::NaturalTerms,
            purpose: RetrievalPurpose::LocalUi,
            targets: Vec::new(),
            resources: Vec::new(),
            representations: Vec::new(),
            filters: QueryFilters::default(),
            budget: QueryBudget::default(),
        };
        assert!(query.validate().is_ok());
        let mut excessive_timeout = query.clone();
        excessive_timeout.budget.timeout_ms = MAX_RETRIEVAL_TIMEOUT_MS + 1;
        assert_eq!(
            excessive_timeout.validate(),
            Err(ContractError::InvalidBudget)
        );
        let mut maximum_timeout = query.clone();
        maximum_timeout.budget.timeout_ms = MAX_RETRIEVAL_TIMEOUT_MS;
        assert!(maximum_timeout.validate().is_ok());
        let mut unbounded = query;
        unbounded.budget.max_hits = 1_001;
        assert_eq!(unbounded.validate(), Err(ContractError::InvalidBudget));
    }

    fn rights(redistribution: RedistributionPolicy) -> RightsStatement {
        RightsStatement {
            scope: "fixture".to_string(),
            expression: "fixture rights".to_string(),
            license_url: None,
            license_text_sha256: None,
            attribution: Some("Fixture custodian".to_string()),
            obligations: if redistribution == RedistributionPolicy::AllowedWithObligations {
                vec!["Preserve attribution".to_string()]
            } else {
                Vec::new()
            },
            redistribution,
        }
    }

    #[test]
    fn use_policy_cannot_exceed_rights_redistribution_ceiling() {
        let permissive = UsePolicy {
            local_search: UsePermission::Allowed,
            model_context: UsePermission::Allowed,
            excerpt_export: UsePermission::Allowed,
            redistribution: UsePermission::Allowed,
            attribution_required: true,
        };
        assert!(
            permissive
                .validate_with_rights(&[rights(RedistributionPolicy::Allowed)])
                .is_ok()
        );
        let excerpt_only = UsePolicy {
            redistribution: UsePermission::Forbidden,
            ..permissive
        };
        let redistribution_only = UsePolicy {
            excerpt_export: UsePermission::Forbidden,
            ..permissive
        };
        for ceiling in [
            RedistributionPolicy::Unknown,
            RedistributionPolicy::PrivateUseOnly,
            RedistributionPolicy::Forbidden,
        ] {
            assert!(
                excerpt_only
                    .validate_with_rights(&[rights(ceiling)])
                    .is_err(),
                "{ceiling:?} rights unexpectedly authorized excerpt export"
            );
            assert!(
                redistribution_only
                    .validate_with_rights(&[rights(ceiling)])
                    .is_err(),
                "{ceiling:?} rights unexpectedly authorized redistribution"
            );
        }
        assert!(permissive.validate_with_rights(&[]).is_err());
        assert!(
            permissive
                .validate_with_rights(&[
                    rights(RedistributionPolicy::Allowed),
                    rights(RedistributionPolicy::Unknown),
                ])
                .is_err()
        );

        let conservative = UsePolicy {
            excerpt_export: UsePermission::Unknown,
            redistribution: UsePermission::Forbidden,
            attribution_required: true,
            ..permissive
        };
        assert!(
            conservative
                .validate_with_rights(&[rights(RedistributionPolicy::PrivateUseOnly)])
                .is_ok()
        );
    }

    #[test]
    fn redistribution_obligations_require_attribution_policy() {
        let obligated_rights = [rights(RedistributionPolicy::AllowedWithObligations)];
        let unattributed = UsePolicy {
            local_search: UsePermission::Allowed,
            model_context: UsePermission::Allowed,
            excerpt_export: UsePermission::Allowed,
            redistribution: UsePermission::Allowed,
            attribution_required: false,
        };
        assert!(
            unattributed
                .validate_with_rights(&obligated_rights)
                .is_err()
        );
        assert!(
            UsePolicy {
                attribution_required: true,
                ..unattributed
            }
            .validate_with_rights(&obligated_rights)
            .is_ok()
        );
    }

    fn fixture_artifact(id: &str, file_name: &str, expected_bytes: u64) -> ArtifactDescriptor {
        ArtifactDescriptor {
            id: ArtifactId::parse(id).expect("fixture artifact id is valid"),
            role: ArtifactRole::Primary,
            file_name: file_name.to_string(),
            media_type: "application/octet-stream".to_string(),
            expected_bytes,
            sha256: "0".repeat(64),
            mirrors: vec![ArtifactMirror {
                uri: format!("file:///fixture/{file_name}"),
                priority: 1,
            }],
        }
    }

    fn fixture_representation(
        artifacts: Vec<ArtifactDescriptor>,
        expected_installed_bytes: u64,
    ) -> RepresentationDescriptor {
        RepresentationDescriptor {
            id: RepresentationId::parse("fixture-representation")
                .expect("fixture representation id is valid"),
            format: RepresentationFormat {
                kind: FormatKind::Directory,
                profile: Some("fixture".to_string()),
                media_type: None,
            },
            capabilities: BTreeSet::from([InformationCapability::RandomAccess]),
            coverage: CoverageDescriptor::default(),
            subset_support: SubsetSupport::default(),
            expected_installed_bytes,
            artifacts,
            runtime: RuntimeRequirement::None,
        }
    }

    #[test]
    fn representation_rejects_ascii_case_insensitive_file_name_collisions() {
        let representation = fixture_representation(
            vec![
                fixture_artifact("first", "Payload.ZIM", 1),
                fixture_artifact("second", "payload.zim", 1),
            ],
            2,
        );
        let error = representation
            .validate()
            .expect_err("portable artifact file-name collision was accepted");
        assert!(error.to_string().contains("case-insensitive"));
    }

    #[test]
    fn install_plan_rejects_ascii_case_insensitive_file_name_collisions() {
        let representation =
            fixture_representation(vec![fixture_artifact("source", "source.bin", 2)], 2);
        let resource_id = ResourceId::parse("fixture.resource").expect("valid fixture resource");
        let release_id = ReleaseId::parse("fixture-release").expect("valid fixture release");
        let rights = vec![rights(RedistributionPolicy::Unknown)];
        let use_policy = UsePolicy::default();
        let resolved = ResolvedResource {
            resource: ResourceDescriptor {
                id: resource_id.clone(),
                kind: ResourceKind::StructuredDataset,
                title: "Fixture resource".to_string(),
                summary: "Fixture resource for portable file-name validation".to_string(),
                languages: Vec::new(),
                subjects: Vec::new(),
                homepage: None,
                extensions: BTreeMap::new(),
            },
            release_id: release_id.clone(),
            published_at: None,
            upstream_id: None,
            immutable: true,
            provenance: Provenance {
                publisher: "Fixture publisher".to_string(),
                source_uri: "file:///fixture".to_string(),
                upstream_record_id: None,
                source_inputs: Vec::new(),
                transformation: None,
                metadata: BTreeMap::new(),
            },
            rights: rights.clone(),
            use_policy,
            representation: representation.clone(),
        };
        let mut plan = InstallPlan {
            schema: INSTALL_PLAN_SCHEMA.to_string(),
            installation_id: InstallationId::new(),
            resource_id,
            release_id,
            representation_id: representation.id.clone(),
            format: representation.format.clone(),
            selection: InstallSelection::default(),
            catalog_authority: CatalogAuthority::Unverified {
                declared: CatalogTrust::Unverified,
                catalog_sha256: "0".repeat(64),
            },
            resolved,
            rights,
            use_policy,
            artifacts: vec![
                PlannedArtifact {
                    artifact_id: ArtifactId::parse("first").expect("valid artifact id"),
                    file_name: "Payload.BIN".to_string(),
                    source_uri: "file:///fixture/Payload.BIN".to_string(),
                    expected_bytes: 1,
                    sha256: "1".repeat(64),
                },
                PlannedArtifact {
                    artifact_id: ArtifactId::parse("second").expect("valid artifact id"),
                    file_name: "payload.bin".to_string(),
                    source_uri: "file:///fixture/payload.bin".to_string(),
                    expected_bytes: 1,
                    sha256: "2".repeat(64),
                },
            ],
            total_download_bytes: 2,
            expected_installed_bytes: 2,
            available_bytes_observed: Some(2),
            created_at: Utc::now(),
            plan_sha256: "0".repeat(64),
        };
        plan.refresh_plan_sha256()
            .expect("fixture plan fingerprinting failed");
        let error = plan
            .validate()
            .expect_err("portable planned file-name collision was accepted");
        assert!(error.to_string().contains("case-insensitive"));
    }

    #[test]
    fn ready_receipt_cannot_hide_failure() {
        let resource_id = ResourceId::parse("test.resource").expect("valid resource id");
        let release_id = ReleaseId::parse("2026-08").expect("valid release id");
        let representation_id = RepresentationId::parse("fts").expect("valid representation id");
        let artifact_id = ArtifactId::parse("primary").expect("valid artifact id");
        let format = RepresentationFormat {
            kind: FormatKind::SqliteFts5,
            profile: Some("fixture".to_string()),
            media_type: Some("application/vnd.sqlite3".to_string()),
        };
        let policy = UsePolicy {
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
        let resolved = ResolvedResource {
            resource: ResourceDescriptor {
                id: resource_id.clone(),
                kind: ResourceKind::TextCorpus,
                title: "Fixture".to_string(),
                summary: "Fixture corpus".to_string(),
                languages: vec!["en".to_string()],
                subjects: vec!["test".to_string()],
                homepage: None,
                extensions: BTreeMap::new(),
            },
            release_id: release_id.clone(),
            published_at: None,
            upstream_id: None,
            immutable: true,
            provenance: Provenance {
                publisher: "Fixture".to_string(),
                source_uri: "file:///fixture".to_string(),
                upstream_record_id: None,
                source_inputs: Vec::new(),
                transformation: None,
                metadata: BTreeMap::new(),
            },
            rights,
            use_policy: policy,
            representation: RepresentationDescriptor {
                id: representation_id.clone(),
                format: format.clone(),
                capabilities: BTreeSet::from([InformationCapability::LexicalSearch]),
                coverage: CoverageDescriptor::default(),
                subset_support: SubsetSupport::default(),
                expected_installed_bytes: 1,
                artifacts: vec![ArtifactDescriptor {
                    id: artifact_id.clone(),
                    role: ArtifactRole::Primary,
                    file_name: "fixture.sqlite".to_string(),
                    media_type: "application/vnd.sqlite3".to_string(),
                    expected_bytes: 1,
                    sha256: "0".repeat(64),
                    mirrors: vec![ArtifactMirror {
                        uri: "file:///fixture".to_string(),
                        priority: 1,
                    }],
                }],
                runtime: RuntimeRequirement::None,
            },
        };
        let receipt_time = Utc::now();
        let receipt = InstallReceipt {
            schema: INSTALL_RECEIPT_SCHEMA.to_string(),
            installation_id: InstallationId::new(),
            resource_id,
            release_id,
            representation_id,
            format,
            catalog_authority: CatalogAuthority::Unverified {
                declared: CatalogTrust::Unverified,
                catalog_sha256: "0".repeat(64),
            },
            resolved,
            plan_sha256: "0".repeat(64),
            state: InstallationState::Ready,
            started_at: receipt_time,
            finished_at: Some(receipt_time),
            network_attempted: Some(false),
            network_used: false,
            downloaded_bytes: 1,
            unverified_staged_bytes: 0,
            installed_relative_path: Some("packages/test".to_string()),
            artifacts: vec![InstalledArtifact {
                artifact_id: artifact_id.clone(),
                relative_path: "packages/test/artifacts/fixture.sqlite".to_string(),
                bytes: 1,
                sha256: "0".repeat(64),
            }],
            acquisitions: vec![ArtifactAcquisition {
                artifact_id,
                transport: AcquisitionTransport::PreexistingStage,
                requested_uri: None,
                final_uri: None,
                final_peer_address: None,
                redirect_chain: Vec::new(),
                attempts: Vec::new(),
                started_at: None,
                finished_at: receipt_time,
                resumed_bytes: 0,
                verified_bytes: 1,
                sha256: "0".repeat(64),
            }],
            failure: None,
        };
        assert!(receipt.validate().is_ok());
        let mut wrong_installed_id = receipt.clone();
        wrong_installed_id.artifacts[0].artifact_id =
            ArtifactId::parse("other").expect("valid artifact id");
        assert!(wrong_installed_id.validate().is_err());

        let mut wrong_acquisition_id = receipt.clone();
        wrong_acquisition_id.acquisitions[0].artifact_id =
            ArtifactId::parse("other").expect("valid artifact id");
        assert!(wrong_acquisition_id.validate().is_err());

        let mut duplicate_installed = receipt.clone();
        let second_id = ArtifactId::parse("second").expect("valid artifact id");
        let mut second_descriptor =
            duplicate_installed.resolved.representation.artifacts[0].clone();
        second_descriptor.id = second_id.clone();
        second_descriptor.file_name = "fixture-2.sqlite".to_string();
        duplicate_installed
            .resolved
            .representation
            .artifacts
            .push(second_descriptor);
        duplicate_installed
            .resolved
            .representation
            .expected_installed_bytes = 2;
        let mut second_acquisition = duplicate_installed.acquisitions[0].clone();
        second_acquisition.artifact_id = second_id;
        duplicate_installed.acquisitions.push(second_acquisition);
        duplicate_installed
            .artifacts
            .push(duplicate_installed.artifacts[0].clone());
        duplicate_installed.downloaded_bytes = 2;
        assert!(duplicate_installed.validate().is_err());

        let mut duplicate_acquisition = receipt.clone();
        duplicate_acquisition.state = InstallationState::Failed;
        duplicate_acquisition.installed_relative_path = None;
        duplicate_acquisition.artifacts.clear();
        duplicate_acquisition.failure = Some(InformationError::new(
            ErrorClass::Integrity,
            "failed_install",
            "fixture failure",
        ));
        duplicate_acquisition
            .acquisitions
            .push(duplicate_acquisition.acquisitions[0].clone());
        assert!(duplicate_acquisition.validate().is_err());

        let mut forged = receipt;
        forged.failure = Some(InformationError::new(
            ErrorClass::Integrity,
            "bad_digest",
            "digest mismatch",
        ));
        assert!(forged.validate().is_err());
    }
}
