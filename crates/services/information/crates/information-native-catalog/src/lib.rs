#![forbid(unsafe_code)]

//! Pure catalogue and source-registry policy: validation, normalized
//! discovery, search, and exact install-plan resolution.
//!
//! This crate deliberately has no filesystem or network APIs. Callers obtain
//! bytes elsewhere, then pass those bytes here for validation and planning.

mod metalink;
mod opds;
mod source_registry;
mod stac;

pub use metalink::{
    DEFAULT_MAX_METALINK_BYTES, MetalinkFile, MetalinkMirror, parse_metalink4,
    parse_metalink4_with_limit,
};
pub use opds::{
    KiwixOpdsEntry, KiwixOpdsFeed, OpdsLink, parse_kiwix_opds, parse_kiwix_opds_with_limit,
};
pub use source_registry::{
    DEFAULT_MAX_SOURCE_REGISTRY_BYTES, SOURCE_REGISTRY_SCHEMA, SourceRegistry, SourceRegistryEntry,
    SourceRegistryKind, SourceRegistrySupport, SourceRegistryTrust, parse_source_registry,
    parse_source_registry_with_limit,
};
pub use stac::{
    StacAsset, StacDocument, StacLink, StacObjectType, parse_stac_document,
    parse_stac_document_with_limit,
};

use chrono::{DateTime, Utc};
use information_native_types::{
    ArtifactId, CatalogAuthority, ContractError, FormatKind, INSTALL_PLAN_SCHEMA,
    InformationCapability, InformationCatalog, InstallPlan, InstallSelection, InstallationId,
    PlannedArtifact, ReleaseId, RepresentationDescriptor, RepresentationId, ResolvedResource,
    ResourceId, ResourceKind, ResourceRecord, ResourceRelease,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use url::Url;

/// A defensive parser ceiling for a single normalized catalogue document.
pub const DEFAULT_MAX_CATALOG_BYTES: usize = 64 * 1024 * 1024;
/// A defensive parser ceiling for a single provider discovery response.
pub const DEFAULT_MAX_DISCOVERY_BYTES: usize = 32 * 1024 * 1024;
const MAX_PROVIDER_URI_BYTES: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalogue input is {actual} bytes, exceeding the {limit}-byte limit")]
    InputTooLarge { actual: usize, limit: usize },
    #[error("catalogue JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("source registry input is {actual} bytes, exceeding the {limit}-byte limit")]
    SourceRegistryInputTooLarge { actual: usize, limit: usize },
    #[error("source registry JSON is invalid: {0}")]
    SourceRegistryJson(#[source] serde_json::Error),
    #[error("source registry contract is invalid: {0}")]
    InvalidSourceRegistry(String),
    #[error("catalogue contract is invalid: {0}")]
    Contract(#[from] ContractError),
    #[error("{entity} was not found: {id}")]
    NotFound { entity: &'static str, id: String },
    #[error("install selection is invalid: {0}")]
    InvalidSelection(String),
    #[error("mirror selection for artifact {artifact_id} is invalid: {reason}")]
    InvalidMirror { artifact_id: String, reason: String },
    #[error("catalogue source URI is invalid: {0}")]
    InvalidSourceUri(String),
    #[error("catalogue arithmetic overflow")]
    IntegerOverflow,
    #[error("provider document is invalid: {0}")]
    Provider(String),
    #[error("catalogue digest mismatch: expected {expected}, observed {observed}")]
    DigestMismatch { expected: String, observed: String },
    #[error("install plan does not match this catalogue: {0}")]
    InvalidPlan(String),
}

/// Provider-neutral discovery metadata. It is intentionally weaker than an
/// [`InformationCatalog`]: an upstream listing can be useful without carrying
/// the exact digest required for installation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRecord {
    pub provider: DiscoveryProvider,
    pub upstream_id: String,
    pub title: String,
    pub summary: String,
    pub kind: ResourceKind,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub subjects: Vec<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub landing_uri: Option<String>,
    #[serde(default)]
    pub assets: Vec<DiscoveryAsset>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

impl DiscoveryRecord {
    /// True only when every discovered asset already has the facts required by
    /// an exact install plan. Metalink descriptors are not treated as payloads.
    #[must_use]
    pub fn has_exact_install_metadata(&self) -> bool {
        !self.assets.is_empty()
            && self.assets.iter().all(|asset| {
                asset.expected_bytes.is_some()
                    && asset.sha256.is_some()
                    && !asset.requires_resolution
            })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryProvider {
    KiwixOpds,
    Stac,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryAsset {
    pub key: String,
    pub href: String,
    pub media_type: Option<String>,
    pub expected_bytes: Option<u64>,
    pub sha256: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    /// True for an object such as a Kiwix `.meta4` file which must be resolved
    /// into payload mirrors and a digest before it can enter an install plan.
    pub requires_resolution: bool,
}

/// A validated catalogue plus a deterministic in-memory search index.
#[derive(Debug, Clone)]
pub struct CatalogIndex {
    catalog: InformationCatalog,
    authority: CatalogAuthority,
    search_documents: Vec<SearchDocument>,
}

impl CatalogIndex {
    /// Build an index from caller-provided typed data. Any serialized trust
    /// claim remains only a declaration; this constructor never grants it.
    pub fn new(catalog: InformationCatalog) -> Result<Self, CatalogError> {
        let bytes = serde_json::to_vec(&catalog)?;
        let authority = CatalogAuthority::Unverified {
            declared: catalog.declared_trust.clone(),
            catalog_sha256: sha256_hex(&bytes),
        };
        Self::new_with_authority(catalog, authority)
    }

    fn new_with_authority(
        catalog: InformationCatalog,
        authority: CatalogAuthority,
    ) -> Result<Self, CatalogError> {
        catalog.validate()?;
        authority.validate()?;
        validate_catalogue_sources(&catalog)?;
        let search_documents = catalog
            .resources
            .iter()
            .map(SearchDocument::from_record)
            .collect();
        Ok(Self {
            catalog,
            authority,
            search_documents,
        })
    }

    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, CatalogError> {
        Self::from_json_slice_with_limit(bytes, DEFAULT_MAX_CATALOG_BYTES)
    }

    pub fn from_json_slice_with_limit(
        bytes: &[u8],
        max_bytes: usize,
    ) -> Result<Self, CatalogError> {
        let catalog = load_catalog_json_with_limit(bytes, max_bytes)?;
        let authority = CatalogAuthority::Unverified {
            declared: catalog.declared_trust.clone(),
            catalog_sha256: sha256_hex(bytes),
        };
        Self::new_with_authority(catalog, authority)
    }

    /// Grant built-in authority only after exact comparison with a digest
    /// pinned in host code or another independently trusted configuration.
    pub fn from_pinned_json_slice(
        bytes: &[u8],
        expected_sha256: &str,
    ) -> Result<Self, CatalogError> {
        if bytes.len() > DEFAULT_MAX_CATALOG_BYTES {
            return Err(CatalogError::InputTooLarge {
                actual: bytes.len(),
                limit: DEFAULT_MAX_CATALOG_BYTES,
            });
        }
        let expected = canonical_sha256(expected_sha256);
        let observed = sha256_hex(bytes);
        if expected != observed {
            return Err(CatalogError::DigestMismatch { expected, observed });
        }
        let catalog = load_catalog_json_with_limit(bytes, DEFAULT_MAX_CATALOG_BYTES)?;
        Self::new_with_authority(
            catalog,
            CatalogAuthority::BuiltInPinned {
                catalog_sha256: observed,
            },
        )
    }

    /// Record a deliberate local approval of these exact bytes. The approving
    /// identity comes from the host authority boundary, never from the file.
    pub fn from_locally_approved_json_slice(
        bytes: &[u8],
        approved_by: impl Into<String>,
        approved_at: DateTime<Utc>,
    ) -> Result<Self, CatalogError> {
        let catalog = load_catalog_json_with_limit(bytes, DEFAULT_MAX_CATALOG_BYTES)?;
        Self::new_with_authority(
            catalog,
            CatalogAuthority::LocallyApproved {
                catalog_sha256: sha256_hex(bytes),
                approved_by: approved_by.into(),
                approved_at,
            },
        )
    }

    #[must_use]
    pub fn catalog(&self) -> &InformationCatalog {
        &self.catalog
    }

    #[must_use]
    pub fn authority(&self) -> &CatalogAuthority {
        &self.authority
    }

    #[must_use]
    pub fn into_catalog(self) -> InformationCatalog {
        self.catalog
    }

    #[must_use]
    pub fn resource(&self, id: &ResourceId) -> Option<&ResourceRecord> {
        self.catalog
            .resources
            .iter()
            .find(|record| record.resource.id == *id)
    }

    /// Search uses normalized Unicode lowercase terms and deterministic
    /// relevance ordering. Structured filters are applied before scoring.
    pub fn search<'index>(
        &'index self,
        query: &CatalogSearchQuery,
    ) -> Result<Vec<CatalogSearchHit<'index>>, CatalogError> {
        if query.text.chars().count() > 8_192 {
            return Err(CatalogError::InvalidSelection(
                "search text exceeds 8192 characters".to_string(),
            ));
        }
        if query.limit > CatalogSearchQuery::MAX_LIMIT {
            return Err(CatalogError::InvalidSelection(format!(
                "search limit {} exceeds {}",
                query.limit,
                CatalogSearchQuery::MAX_LIMIT
            )));
        }
        validate_search_values("languages", &query.languages)?;
        validate_search_values("subjects", &query.subjects)?;

        let terms = normalized_terms(&query.text);
        let phrase = terms.join(" ");
        let requested_languages = normalized_values(&query.languages);
        let requested_subjects = normalized_values(&query.subjects);
        let mut hits = Vec::new();

        for (record, document) in self.catalog.resources.iter().zip(&self.search_documents) {
            if !query.kinds.is_empty() && !query.kinds.contains(&record.resource.kind) {
                continue;
            }
            let matching_representations = matching_representations(
                document,
                query,
                &requested_languages,
                &requested_subjects,
            );
            if matching_representations.is_empty() {
                continue;
            }

            let matched_terms = terms
                .iter()
                .filter(|term| document.all_terms.contains(*term))
                .count();
            let text_matches = terms.is_empty()
                || match query.text_mode {
                    SearchTextMode::AllTerms => matched_terms == terms.len(),
                    SearchTextMode::AnyTerm => matched_terms > 0,
                };
            if !text_matches {
                continue;
            }

            let relevance = document.score(&terms, &phrase);
            hits.push(CatalogSearchHit {
                record,
                relevance,
                matching_representations,
            });
        }

        hits.sort_by_key(|hit| {
            (
                Reverse(hit.relevance),
                normalize_text(&hit.record.resource.title),
                hit.record.resource.id.as_str().to_string(),
            )
        });
        hits.truncate(query.limit);
        Ok(hits)
    }

    /// Resolve a resource/release/representation into exact, ordered bytes.
    /// Every caller-controlled mirror must be one of the representation's
    /// declared mirrors. Otherwise the lowest `(priority, URI)` pair wins.
    pub fn resolve_install_plan(&self, request: PlanRequest) -> Result<InstallPlan, CatalogError> {
        InstallationId::parse(request.installation_id.as_str())?;
        let record = self
            .resource(&request.resource_id)
            .ok_or_else(|| CatalogError::NotFound {
                entity: "resource",
                id: request.resource_id.to_string(),
            })?;
        let release = record
            .releases
            .iter()
            .find(|release| release.id == request.release_id)
            .ok_or_else(|| CatalogError::NotFound {
                entity: "release",
                id: request.release_id.to_string(),
            })?;
        let representation = release
            .representations
            .iter()
            .find(|representation| representation.id == request.representation_id)
            .ok_or_else(|| CatalogError::NotFound {
                entity: "representation",
                id: request.representation_id.to_string(),
            })?;

        let selection = canonicalize_selection(request.selection)?;
        if !selection.is_full() {
            return Err(CatalogError::InvalidSelection(
                "subset materialization is not implemented; install the full representation"
                    .to_string(),
            ));
        }

        let declared_ids: BTreeSet<&ArtifactId> = representation
            .artifacts
            .iter()
            .map(|artifact| &artifact.id)
            .collect();
        for artifact_id in request.mirror_choices.keys() {
            if !declared_ids.contains(artifact_id) {
                return Err(CatalogError::InvalidMirror {
                    artifact_id: artifact_id.to_string(),
                    reason: "artifact is not part of the selected representation".to_string(),
                });
            }
        }

        let mut declared_artifacts: Vec<_> = representation.artifacts.iter().collect();
        declared_artifacts
            .sort_by_key(|artifact| (artifact.id.as_str().to_string(), artifact.file_name.clone()));

        let mut artifacts = Vec::with_capacity(declared_artifacts.len());
        let mut total_download_bytes = 0_u64;
        for artifact in declared_artifacts {
            let chosen_uri = if let Some(choice) = request.mirror_choices.get(&artifact.id) {
                let canonical_choice = validate_fetch_uri(choice)?.to_string();
                if !artifact.mirrors.iter().any(|mirror| {
                    validate_fetch_uri(&mirror.uri)
                        .is_ok_and(|declared| declared.as_str() == canonical_choice)
                }) {
                    return Err(CatalogError::InvalidMirror {
                        artifact_id: artifact.id.to_string(),
                        reason: "chosen URI is not a declared mirror".to_string(),
                    });
                }
                canonical_choice
            } else {
                artifact
                    .mirrors
                    .iter()
                    .min_by(|left, right| {
                        (left.priority, left.uri.as_str())
                            .cmp(&(right.priority, right.uri.as_str()))
                    })
                    .map(|mirror| validate_fetch_uri(&mirror.uri).map(|uri| uri.to_string()))
                    .ok_or_else(|| CatalogError::InvalidMirror {
                        artifact_id: artifact.id.to_string(),
                        reason: "artifact has no mirrors".to_string(),
                    })??
            };
            total_download_bytes = total_download_bytes
                .checked_add(artifact.expected_bytes)
                .ok_or(CatalogError::IntegerOverflow)?;
            artifacts.push(PlannedArtifact {
                artifact_id: artifact.id.clone(),
                file_name: artifact.file_name.clone(),
                source_uri: chosen_uri,
                expected_bytes: artifact.expected_bytes,
                sha256: canonical_sha256(&artifact.sha256),
            });
        }

        let mut plan = InstallPlan {
            schema: INSTALL_PLAN_SCHEMA.to_string(),
            installation_id: request.installation_id,
            resource_id: record.resource.id.clone(),
            release_id: release.id.clone(),
            representation_id: representation.id.clone(),
            format: representation.format.clone(),
            selection,
            catalog_authority: self.authority.clone(),
            resolved: freeze_resource(record, release, representation),
            rights: release.rights.clone(),
            use_policy: release.default_use_policy,
            artifacts,
            total_download_bytes,
            expected_installed_bytes: representation.expected_installed_bytes,
            available_bytes_observed: request.available_bytes_observed,
            created_at: request.created_at,
            plan_sha256: "0".repeat(64),
        };
        plan.refresh_plan_sha256()?;
        plan.validate()?;
        Ok(plan)
    }

    /// Rebind a caller-provided plan to this exact validated catalogue before
    /// any authority-bearing host operation. A self-consistent plan is not
    /// sufficient: every frozen fact, artifact, and selected mirror must still
    /// be declared by this catalogue under the same host-granted authority.
    pub fn validate_resolved_plan(&self, plan: &InstallPlan) -> Result<(), CatalogError> {
        plan.validate()
            .map_err(|error| CatalogError::InvalidPlan(error.to_string()))?;
        if plan.catalog_authority != self.authority {
            return Err(CatalogError::InvalidPlan(
                "catalogue authority does not match".to_string(),
            ));
        }
        if !plan.selection.is_full() {
            return Err(CatalogError::InvalidPlan(
                "subset selections are not materialized".to_string(),
            ));
        }

        let record = self
            .resource(&plan.resource_id)
            .ok_or_else(|| CatalogError::InvalidPlan("resource is not declared".to_string()))?;
        let release = record
            .releases
            .iter()
            .find(|release| release.id == plan.release_id)
            .ok_or_else(|| CatalogError::InvalidPlan("release is not declared".to_string()))?;
        let representation = release
            .representations
            .iter()
            .find(|representation| representation.id == plan.representation_id)
            .ok_or_else(|| {
                CatalogError::InvalidPlan("representation is not declared".to_string())
            })?;

        if plan.resolved != freeze_resource(record, release, representation)
            || plan.format != representation.format
            || plan.rights != release.rights
            || plan.use_policy != release.default_use_policy
            || plan.expected_installed_bytes != representation.expected_installed_bytes
        {
            return Err(CatalogError::InvalidPlan(
                "frozen resource, release, representation, or policy differs".to_string(),
            ));
        }
        if plan.artifacts.len() != representation.artifacts.len() {
            return Err(CatalogError::InvalidPlan(
                "planned artifact set differs".to_string(),
            ));
        }

        let declared: BTreeMap<_, _> = representation
            .artifacts
            .iter()
            .map(|artifact| (&artifact.id, artifact))
            .collect();
        let mut planned_ids = BTreeSet::new();
        for artifact in &plan.artifacts {
            if !planned_ids.insert(&artifact.artifact_id) {
                return Err(CatalogError::InvalidPlan(format!(
                    "artifact {} is duplicated",
                    artifact.artifact_id
                )));
            }
            let descriptor = declared.get(&artifact.artifact_id).ok_or_else(|| {
                CatalogError::InvalidPlan(format!(
                    "artifact {} is not declared",
                    artifact.artifact_id
                ))
            })?;
            if artifact.file_name != descriptor.file_name
                || artifact.expected_bytes != descriptor.expected_bytes
                || canonical_sha256(&artifact.sha256) != canonical_sha256(&descriptor.sha256)
            {
                return Err(CatalogError::InvalidPlan(format!(
                    "artifact {} metadata differs",
                    artifact.artifact_id
                )));
            }
            if !descriptor.mirrors.iter().any(|mirror| {
                validate_fetch_uri(&mirror.uri)
                    .is_ok_and(|declared| declared.as_str() == artifact.source_uri)
            }) {
                return Err(CatalogError::InvalidPlan(format!(
                    "artifact {} source URI is not a declared mirror",
                    artifact.artifact_id
                )));
            }
        }
        Ok(())
    }
}

fn freeze_resource(
    record: &ResourceRecord,
    release: &ResourceRelease,
    representation: &RepresentationDescriptor,
) -> ResolvedResource {
    ResolvedResource {
        resource: record.resource.clone(),
        release_id: release.id.clone(),
        published_at: release.published_at,
        upstream_id: release.upstream_id.clone(),
        immutable: release.immutable,
        provenance: release.provenance.clone(),
        rights: release.rights.clone(),
        use_policy: release.default_use_policy,
        representation: representation.clone(),
    }
}

pub fn load_catalog_json(bytes: &[u8]) -> Result<InformationCatalog, CatalogError> {
    load_catalog_json_with_limit(bytes, DEFAULT_MAX_CATALOG_BYTES)
}

pub fn load_catalog_json_with_limit(
    bytes: &[u8],
    max_bytes: usize,
) -> Result<InformationCatalog, CatalogError> {
    if bytes.len() > max_bytes {
        return Err(CatalogError::InputTooLarge {
            actual: bytes.len(),
            limit: max_bytes,
        });
    }
    let catalog: InformationCatalog = serde_json::from_slice(bytes)?;
    catalog.validate()?;
    validate_catalogue_sources(&catalog)?;
    Ok(catalog)
}

/// Convenience wrapper for callers that do not retain a search index.
pub fn resolve_install_plan(
    catalog: InformationCatalog,
    request: PlanRequest,
) -> Result<InstallPlan, CatalogError> {
    CatalogIndex::new(catalog)?.resolve_install_plan(request)
}

#[derive(Debug, Clone)]
pub struct PlanRequest {
    pub installation_id: InstallationId,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub selection: InstallSelection,
    /// Exact source URI overrides by artifact. Unknown artifacts or undeclared
    /// URIs fail closed.
    pub mirror_choices: BTreeMap<ArtifactId, String>,
    pub available_bytes_observed: Option<u64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CatalogSearchQuery {
    pub text: String,
    pub text_mode: SearchTextMode,
    pub kinds: BTreeSet<ResourceKind>,
    pub languages: Vec<String>,
    pub subjects: Vec<String>,
    /// Any listed format is acceptable.
    pub formats: BTreeSet<FormatKind>,
    /// A matching representation must provide every listed capability.
    pub capabilities: BTreeSet<InformationCapability>,
    pub limit: usize,
}

impl CatalogSearchQuery {
    pub const MAX_LIMIT: usize = 10_000;
}

impl Default for CatalogSearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            text_mode: SearchTextMode::AllTerms,
            kinds: BTreeSet::new(),
            languages: Vec::new(),
            subjects: Vec::new(),
            formats: BTreeSet::new(),
            capabilities: BTreeSet::new(),
            limit: 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTextMode {
    AllTerms,
    AnyTerm,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct CatalogRepresentationMatch {
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
}

#[derive(Debug, Clone)]
pub struct CatalogSearchHit<'catalog> {
    pub record: &'catalog ResourceRecord,
    pub relevance: u32,
    /// Every concrete release and representation that independently satisfies
    /// all structured representation filters in the query.
    pub matching_representations: Vec<CatalogRepresentationMatch>,
}

#[derive(Debug, Clone)]
struct SearchDocument {
    title: String,
    summary: String,
    all_terms: BTreeSet<String>,
    representations: Vec<RepresentationSearchDocument>,
}

#[derive(Debug, Clone)]
struct RepresentationSearchDocument {
    matched: CatalogRepresentationMatch,
    format: FormatKind,
    capabilities: BTreeSet<InformationCapability>,
    languages: BTreeSet<String>,
    subjects: BTreeSet<String>,
}

impl SearchDocument {
    fn from_record(record: &ResourceRecord) -> Self {
        let title = normalize_text(&record.resource.title);
        let summary = normalize_text(&record.resource.summary);
        let mut all_text = format!(
            "{} {} {} {}",
            record.resource.id.as_str(),
            record.resource.title,
            record.resource.summary,
            record.resource.subjects.join(" ")
        );
        for release in &record.releases {
            for representation in &release.representations {
                all_text.push(' ');
                all_text.push_str(representation.id.as_str());
                if let Some(profile) = &representation.format.profile {
                    all_text.push(' ');
                    all_text.push_str(profile);
                }
            }
        }
        let all_terms = normalized_terms(&all_text).into_iter().collect();
        let mut representations = record
            .releases
            .iter()
            .flat_map(|release| {
                release
                    .representations
                    .iter()
                    .map(|representation| RepresentationSearchDocument {
                        matched: CatalogRepresentationMatch {
                            release_id: release.id.clone(),
                            representation_id: representation.id.clone(),
                        },
                        format: representation.format.kind,
                        capabilities: representation.capabilities.clone(),
                        languages: normalized_values(
                            if representation.coverage.languages.is_empty() {
                                &record.resource.languages
                            } else {
                                &representation.coverage.languages
                            },
                        ),
                        subjects: normalized_values(
                            if representation.coverage.subjects.is_empty() {
                                &record.resource.subjects
                            } else {
                                &representation.coverage.subjects
                            },
                        ),
                    })
            })
            .collect::<Vec<_>>();
        representations.sort_by(|left, right| left.matched.cmp(&right.matched));
        Self {
            title,
            summary,
            all_terms,
            representations,
        }
    }

    fn score(&self, terms: &[String], phrase: &str) -> u32 {
        if terms.is_empty() {
            return 0;
        }
        let mut score = 0_u32;
        if self.title == phrase {
            score = score.saturating_add(1_000);
        } else if self.title.starts_with(phrase) {
            score = score.saturating_add(500);
        } else if self.title.contains(phrase) {
            score = score.saturating_add(250);
        } else if self.summary.contains(phrase) {
            score = score.saturating_add(100);
        }
        for term in terms {
            if self.title.split_whitespace().any(|word| word == term) {
                score = score.saturating_add(40);
            } else if self.title.contains(term) {
                score = score.saturating_add(20);
            }
            if self.summary.contains(term) {
                score = score.saturating_add(5);
            }
        }
        score
    }
}

fn matching_representations(
    document: &SearchDocument,
    query: &CatalogSearchQuery,
    requested_languages: &BTreeSet<String>,
    requested_subjects: &BTreeSet<String>,
) -> Vec<CatalogRepresentationMatch> {
    document
        .representations
        .iter()
        .filter(|representation| {
            requested_languages.is_subset(&representation.languages)
                && requested_subjects.is_subset(&representation.subjects)
                && (query.formats.is_empty() || query.formats.contains(&representation.format))
                && query.capabilities.is_subset(&representation.capabilities)
        })
        .map(|representation| representation.matched.clone())
        .collect()
}

fn validate_catalogue_sources(catalog: &InformationCatalog) -> Result<(), CatalogError> {
    for record in &catalog.resources {
        for release in &record.releases {
            for representation in &release.representations {
                for artifact in &representation.artifacts {
                    for mirror in &artifact.mirrors {
                        validate_fetch_uri(&mirror.uri).map_err(|error| {
                            CatalogError::InvalidMirror {
                                artifact_id: artifact.id.to_string(),
                                reason: error.to_string(),
                            }
                        })?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_fetch_uri(uri: &str) -> Result<Url, CatalogError> {
    let parsed = Url::parse(uri).map_err(|_| {
        CatalogError::InvalidSourceUri(
            "source must be an absolute http, https, or file URI".to_string(),
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https" | "file") {
        return Err(CatalogError::InvalidSourceUri(format!(
            "unsupported URI scheme {:?}",
            parsed.scheme()
        )));
    }
    if matches!(parsed.scheme(), "http" | "https")
        && (!parsed.username().is_empty() || parsed.password().is_some())
    {
        return Err(CatalogError::InvalidSourceUri(
            "credentials in source URIs are forbidden".to_string(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(CatalogError::InvalidSourceUri(
            "durable install source URIs cannot contain query parameters or fragments".to_string(),
        ));
    }
    Ok(parsed)
}

fn canonicalize_selection(
    mut selection: InstallSelection,
) -> Result<InstallSelection, CatalogError> {
    selection.validate()?;
    selection.languages = canonicalize_values("languages", selection.languages, true)?;
    selection.subjects = canonicalize_values("subjects", selection.subjects, false)?;
    selection.feature_types = canonicalize_values("feature_types", selection.feature_types, false)?;
    selection.columns = canonicalize_values("columns", selection.columns, false)?;
    Ok(selection)
}

fn canonicalize_values(
    field: &str,
    values: Vec<String>,
    lowercase: bool,
) -> Result<Vec<String>, CatalogError> {
    let mut normalized = BTreeSet::new();
    let mut canonical = Vec::with_capacity(values.len());
    for value in values {
        let trimmed = value.trim();
        let key = normalize_text(trimmed);
        if !normalized.insert(key) {
            return Err(CatalogError::InvalidSelection(format!(
                "{field} contains duplicate value {trimmed:?}"
            )));
        }
        canonical.push(if lowercase {
            trimmed.to_lowercase()
        } else {
            trimmed.to_string()
        });
    }
    canonical.sort_by_key(|value| (normalize_text(value), value.clone()));
    Ok(canonical)
}

fn canonical_sha256(value: &str) -> String {
    value
        .strip_prefix("sha256:")
        .unwrap_or(value)
        .to_ascii_lowercase()
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn normalize_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut needs_space = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            if needs_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character);
            needs_space = false;
        } else {
            needs_space = true;
        }
    }
    normalized
}

fn normalized_terms(value: &str) -> Vec<String> {
    normalize_text(value)
        .split_whitespace()
        .map(ToString::to_string)
        .collect()
}

fn normalized_values(values: &[String]) -> BTreeSet<String> {
    values.iter().map(|value| normalize_text(value)).collect()
}

fn validate_search_values(field: &str, values: &[String]) -> Result<(), CatalogError> {
    if values.len() > 1_024 {
        return Err(CatalogError::InvalidSelection(format!(
            "search {field} contains more than 1024 values"
        )));
    }
    for value in values {
        if value.trim().is_empty() || value.len() > 512 {
            return Err(CatalogError::InvalidSelection(format!(
                "search {field} contains an empty or oversized value"
            )));
        }
    }
    Ok(())
}

pub(crate) fn resolve_provider_href(base: &Url, href: &str) -> Result<String, CatalogError> {
    if href.len() > MAX_PROVIDER_URI_BYTES {
        return Err(CatalogError::Provider(format!(
            "provider link exceeds {MAX_PROVIDER_URI_BYTES} bytes"
        )));
    }
    validate_provider_source_uri(base)?;
    let resolved = base.join(href).map_err(|_| {
        CatalogError::Provider("provider link is not a valid URI reference".to_string())
    })?;
    validate_provider_source_uri(&resolved)?;
    Ok(resolved.to_string())
}

pub(crate) fn validate_provider_source_uri(uri: &Url) -> Result<(), CatalogError> {
    if uri.as_str().len() > MAX_PROVIDER_URI_BYTES {
        return Err(CatalogError::Provider(format!(
            "provider URI exceeds {MAX_PROVIDER_URI_BYTES} bytes"
        )));
    }
    if !matches!(uri.scheme(), "http" | "https" | "file" | "s3") {
        return Err(CatalogError::Provider(format!(
            "provider link uses unsupported scheme {:?}",
            uri.scheme()
        )));
    }
    if matches!(uri.scheme(), "http" | "https")
        && (!uri.username().is_empty() || uri.password().is_some())
    {
        return Err(CatalogError::Provider(
            "provider URI cannot contain embedded credentials".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use information_native_types::{
        ArtifactDescriptor, ArtifactMirror, ArtifactRole, BoundingBox, CATALOG_SCHEMA,
        CatalogTrust, CoverageDescriptor, FormatKind, Provenance, Publisher, RedistributionPolicy,
        RepresentationFormat, ResourceDescriptor, ResourceRelease, RightsStatement,
        RuntimeRequirement, SubsetSupport, UsePolicy,
    };

    fn fixture_catalog() -> Result<InformationCatalog, CatalogError> {
        let resource_id = ResourceId::parse("org.example.world")?;
        let release_id = ReleaseId::parse("2026-08")?;
        let representation_id = RepresentationId::parse("geoparquet")?;
        let artifact_id = ArtifactId::parse("data")?;
        Ok(InformationCatalog {
            schema: CATALOG_SCHEMA.to_string(),
            catalogue_id: "test".to_string(),
            generated_at: Utc
                .with_ymd_and_hms(2026, 8, 8, 12, 0, 0)
                .single()
                .ok_or_else(|| CatalogError::Provider("invalid test date".to_string()))?,
            publisher: Publisher {
                name: "Example".to_string(),
                homepage: None,
            },
            declared_trust: CatalogTrust::BuiltIn,
            resources: vec![ResourceRecord {
                resource: ResourceDescriptor {
                    id: resource_id,
                    kind: ResourceKind::Map,
                    title: "World Places".to_string(),
                    summary: "A compact global places corpus".to_string(),
                    languages: vec!["en".to_string(), "fr".to_string()],
                    subjects: vec!["places".to_string()],
                    homepage: None,
                    extensions: BTreeMap::new(),
                },
                releases: vec![ResourceRelease {
                    id: release_id,
                    published_at: None,
                    upstream_id: None,
                    immutable: true,
                    provenance: Provenance {
                        publisher: "Example".to_string(),
                        source_uri: "https://example.invalid/catalog".to_string(),
                        upstream_record_id: None,
                        source_inputs: Vec::new(),
                        transformation: None,
                        metadata: BTreeMap::new(),
                    },
                    rights: vec![RightsStatement {
                        scope: "dataset".to_string(),
                        expression: "CC0-1.0".to_string(),
                        license_url: None,
                        license_text_sha256: None,
                        attribution: None,
                        obligations: Vec::new(),
                        redistribution: RedistributionPolicy::Allowed,
                    }],
                    default_use_policy: UsePolicy {
                        attribution_required: false,
                        ..UsePolicy::default()
                    },
                    representations: vec![RepresentationDescriptor {
                        id: representation_id,
                        format: RepresentationFormat {
                            kind: FormatKind::GeoParquet,
                            profile: None,
                            media_type: Some("application/vnd.apache.parquet".to_string()),
                        },
                        capabilities: BTreeSet::from([
                            InformationCapability::RecordLookup,
                            InformationCapability::SpatialFilter,
                        ]),
                        coverage: CoverageDescriptor {
                            languages: vec!["en".to_string(), "fr".to_string()],
                            subjects: vec!["places".to_string()],
                            spatial: Some(BoundingBox {
                                west: -180.0,
                                south: -90.0,
                                east: 180.0,
                                north: 90.0,
                            }),
                            ..CoverageDescriptor::default()
                        },
                        subset_support: SubsetSupport {
                            bounding_box: true,
                            languages: true,
                            subjects: true,
                            feature_types: vec!["place".to_string()],
                            columns: true,
                        },
                        expected_installed_bytes: 12,
                        artifacts: vec![ArtifactDescriptor {
                            id: artifact_id,
                            role: ArtifactRole::Primary,
                            file_name: "places.parquet".to_string(),
                            media_type: "application/vnd.apache.parquet".to_string(),
                            expected_bytes: 12,
                            sha256: format!("sha256:{}", "a".repeat(64)),
                            mirrors: vec![
                                ArtifactMirror {
                                    uri: "https://b.example.invalid/places".to_string(),
                                    priority: 10,
                                },
                                ArtifactMirror {
                                    uri: "https://a.example.invalid/places".to_string(),
                                    priority: 10,
                                },
                            ],
                        }],
                        runtime: RuntimeRequirement::None,
                    }],
                }],
            }],
        })
    }

    fn plan_request() -> Result<PlanRequest, CatalogError> {
        Ok(PlanRequest {
            installation_id: InstallationId::parse("install-1")?,
            resource_id: ResourceId::parse("org.example.world")?,
            release_id: ReleaseId::parse("2026-08")?,
            representation_id: RepresentationId::parse("geoparquet")?,
            selection: InstallSelection::default(),
            mirror_choices: BTreeMap::new(),
            available_bytes_observed: Some(100),
            created_at: Utc
                .with_ymd_and_hms(2026, 8, 8, 12, 0, 0)
                .single()
                .ok_or_else(|| CatalogError::Provider("invalid test date".to_string()))?,
        })
    }

    #[test]
    fn exact_plans_are_stable_and_choose_a_deterministic_mirror() -> Result<(), CatalogError> {
        let index = CatalogIndex::new(fixture_catalog()?)?;
        let first = index.resolve_install_plan(plan_request()?)?;
        let second = index.resolve_install_plan(plan_request()?)?;
        assert_eq!(first, second);
        assert_eq!(
            first.artifacts[0].source_uri,
            "https://a.example.invalid/places"
        );
        assert_eq!(first.artifacts[0].sha256, "a".repeat(64));
        assert!(first.selection.is_full());
        assert!(matches!(
            first.catalog_authority,
            CatalogAuthority::Unverified { .. }
        ));
        assert_eq!(first.resolved.provenance.publisher, "Example");
        first.validate()?;
        Ok(())
    }

    #[test]
    fn plans_canonicalize_declared_and_selected_uri_spellings() -> Result<(), CatalogError> {
        let mut catalog = fixture_catalog()?;
        catalog.resources[0].releases[0].representations[0].artifacts[0].mirrors[0].uri =
            "HTTPS://A.EXAMPLE.INVALID/places".to_string();
        let index = CatalogIndex::new(catalog)?;
        let mut request = plan_request()?;
        request.mirror_choices.insert(
            ArtifactId::parse("data")?,
            "https://a.example.invalid/places".to_string(),
        );
        let plan = index.resolve_install_plan(request)?;
        assert_eq!(
            plan.artifacts[0].source_uri,
            "https://a.example.invalid/places"
        );
        index.validate_resolved_plan(&plan)?;
        Ok(())
    }

    #[test]
    fn undeclared_mirrors_and_unsupported_subsets_fail_closed() -> Result<(), CatalogError> {
        let index = CatalogIndex::new(fixture_catalog()?)?;
        let mut request = plan_request()?;
        request.mirror_choices.insert(
            ArtifactId::parse("data")?,
            "https://evil.invalid/payload".to_string(),
        );
        assert!(matches!(
            index.resolve_install_plan(request),
            Err(CatalogError::InvalidMirror { .. })
        ));

        let mut request = plan_request()?;
        request.selection.languages = vec!["de".to_string()];
        assert!(matches!(
            index.resolve_install_plan(request),
            Err(CatalogError::InvalidSelection(_))
        ));

        let mut request = plan_request()?;
        request.selection.languages = vec!["en".to_string()];
        assert!(matches!(
            index.resolve_install_plan(request),
            Err(CatalogError::InvalidSelection(_))
        ));
        Ok(())
    }

    #[test]
    fn every_non_full_install_selection_fails_closed() -> Result<(), CatalogError> {
        let index = CatalogIndex::new(fixture_catalog()?)?;
        let selections = [
            InstallSelection {
                bounding_box: Some(BoundingBox {
                    west: -10.0,
                    south: -10.0,
                    east: 10.0,
                    north: 10.0,
                }),
                ..InstallSelection::default()
            },
            InstallSelection {
                languages: vec!["en".to_string()],
                ..InstallSelection::default()
            },
            InstallSelection {
                subjects: vec!["places".to_string()],
                ..InstallSelection::default()
            },
            InstallSelection {
                feature_types: vec!["place".to_string()],
                ..InstallSelection::default()
            },
            InstallSelection {
                columns: vec!["name".to_string()],
                ..InstallSelection::default()
            },
        ];

        for selection in selections {
            let mut request = plan_request()?;
            request.selection = selection;
            assert!(matches!(
                index.resolve_install_plan(request),
                Err(CatalogError::InvalidSelection(message))
                    if message.contains("subset materialization is not implemented")
            ));
        }
        Ok(())
    }

    #[test]
    fn normalized_search_combines_text_and_capability_filters() -> Result<(), CatalogError> {
        let index = CatalogIndex::new(fixture_catalog()?)?;
        let query = CatalogSearchQuery {
            text: "WORLD places".to_string(),
            languages: vec!["EN".to_string()],
            capabilities: BTreeSet::from([InformationCapability::SpatialFilter]),
            ..CatalogSearchQuery::default()
        };
        let hits = index.search(&query)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.resource.id.as_str(), "org.example.world");
        assert_eq!(
            hits[0].matching_representations,
            vec![CatalogRepresentationMatch {
                release_id: ReleaseId::parse("2026-08")?,
                representation_id: RepresentationId::parse("geoparquet")?,
            }]
        );
        Ok(())
    }

    #[test]
    fn structured_search_filters_match_one_concrete_representation() -> Result<(), CatalogError> {
        let mut catalog = fixture_catalog()?;
        let release = &mut catalog.resources[0].releases[0];
        release.representations[0].coverage.languages = vec!["en".to_string()];

        let mut zim = release.representations[0].clone();
        zim.id = RepresentationId::parse("zim")?;
        zim.format = RepresentationFormat {
            kind: FormatKind::Zim,
            profile: None,
            media_type: Some("application/x-zim".to_string()),
        };
        zim.capabilities = BTreeSet::from([InformationCapability::ArticleRead]);
        zim.coverage.languages = vec!["fr".to_string()];
        zim.coverage.subjects = vec!["history".to_string()];
        zim.artifacts[0].file_name = "places.zim".to_string();
        zim.artifacts[0].media_type = "application/x-zim".to_string();
        release.representations.push(zim);

        let index = CatalogIndex::new(catalog)?;
        let cross_representation = CatalogSearchQuery {
            languages: vec!["fr".to_string()],
            formats: BTreeSet::from([FormatKind::GeoParquet]),
            ..CatalogSearchQuery::default()
        };
        assert!(index.search(&cross_representation)?.is_empty());

        let split_axes = CatalogSearchQuery {
            languages: vec!["en".to_string()],
            subjects: vec!["history".to_string()],
            ..CatalogSearchQuery::default()
        };
        assert!(index.search(&split_axes)?.is_empty());

        let exact = CatalogSearchQuery {
            languages: vec!["fr".to_string()],
            subjects: vec!["history".to_string()],
            formats: BTreeSet::from([FormatKind::Zim]),
            capabilities: BTreeSet::from([InformationCapability::ArticleRead]),
            ..CatalogSearchQuery::default()
        };
        let hits = index.search(&exact)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].matching_representations,
            vec![CatalogRepresentationMatch {
                release_id: ReleaseId::parse("2026-08")?,
                representation_id: RepresentationId::parse("zim")?,
            }]
        );
        Ok(())
    }

    #[test]
    fn declared_trust_never_grants_typed_or_json_authority() -> Result<(), CatalogError> {
        for declared_trust in [
            CatalogTrust::BuiltIn,
            CatalogTrust::Signed {
                key_id: "publisher-key".to_string(),
            },
        ] {
            let mut catalog = fixture_catalog()?;
            catalog.declared_trust = declared_trust.clone();
            let bytes = serde_json::to_vec(&catalog)?;
            let expected = CatalogAuthority::Unverified {
                declared: declared_trust,
                catalog_sha256: sha256_hex(&bytes),
            };

            let typed = CatalogIndex::new(catalog)?;
            assert_eq!(typed.authority(), &expected);
            let json = CatalogIndex::from_json_slice(&bytes)?;
            assert_eq!(json.authority(), &expected);
        }
        Ok(())
    }

    #[test]
    fn pinned_authority_requires_the_exact_catalogue_digest() -> Result<(), CatalogError> {
        let bytes = serde_json::to_vec(&fixture_catalog()?)?;
        let observed = sha256_hex(&bytes);
        let mismatch = CatalogIndex::from_pinned_json_slice(&bytes, &"0".repeat(64));
        assert!(matches!(
            mismatch,
            Err(CatalogError::DigestMismatch {
                expected,
                observed: actual
            }) if expected == "0".repeat(64) && actual == observed
        ));

        let pinned = CatalogIndex::from_pinned_json_slice(
            &bytes,
            &format!("sha256:{}", observed.to_ascii_uppercase()),
        )?;
        assert_eq!(
            pinned.authority(),
            &CatalogAuthority::BuiltInPinned {
                catalog_sha256: observed,
            }
        );
        Ok(())
    }

    #[test]
    fn local_approval_is_explicit_and_bound_to_exact_bytes() -> Result<(), CatalogError> {
        let mut catalog = fixture_catalog()?;
        catalog.declared_trust = CatalogTrust::Signed {
            key_id: "untrusted-declaration".to_string(),
        };
        let bytes = serde_json::to_vec(&catalog)?;
        let approved_at = Utc
            .with_ymd_and_hms(2026, 8, 8, 13, 0, 0)
            .single()
            .ok_or_else(|| CatalogError::Provider("invalid test date".to_string()))?;
        let approved = CatalogIndex::from_locally_approved_json_slice(
            &bytes,
            "operator@example.invalid",
            approved_at,
        )?;
        assert_eq!(
            approved.authority(),
            &CatalogAuthority::LocallyApproved {
                catalog_sha256: sha256_hex(&bytes),
                approved_by: "operator@example.invalid".to_string(),
                approved_at,
            }
        );

        assert!(matches!(
            CatalogIndex::from_json_slice(&bytes)?.authority(),
            CatalogAuthority::Unverified {
                declared: CatalogTrust::Signed { .. },
                ..
            }
        ));
        assert!(matches!(
            CatalogIndex::from_locally_approved_json_slice(&bytes, " ", approved_at),
            Err(CatalogError::Contract(_))
        ));
        Ok(())
    }

    #[test]
    fn resolved_plan_validation_rejects_forged_catalogue_facts() -> Result<(), CatalogError> {
        let index = CatalogIndex::new(fixture_catalog()?)?;
        let plan = index.resolve_install_plan(plan_request()?)?;
        index.validate_resolved_plan(&plan)?;

        let mut wrong_authority = plan.clone();
        wrong_authority.catalog_authority = CatalogAuthority::BuiltInPinned {
            catalog_sha256: "b".repeat(64),
        };
        wrong_authority.refresh_plan_sha256()?;
        assert!(matches!(
            index.validate_resolved_plan(&wrong_authority),
            Err(CatalogError::InvalidPlan(_))
        ));

        let mut wrong_source = plan.clone();
        wrong_source.artifacts[0].source_uri = "https://evil.invalid/payload".to_string();
        wrong_source.refresh_plan_sha256()?;
        assert!(matches!(
            index.validate_resolved_plan(&wrong_source),
            Err(CatalogError::InvalidPlan(_))
        ));

        let mut wrong_metadata = plan.clone();
        wrong_metadata.artifacts[0].expected_bytes += 1;
        wrong_metadata.total_download_bytes += 1;
        wrong_metadata.refresh_plan_sha256()?;
        assert!(matches!(
            index.validate_resolved_plan(&wrong_metadata),
            Err(CatalogError::InvalidPlan(_))
        ));

        let mut wrong_frozen_resource = plan;
        wrong_frozen_resource.resolved.resource.summary = "forged summary".to_string();
        wrong_frozen_resource.refresh_plan_sha256()?;
        assert!(matches!(
            index.validate_resolved_plan(&wrong_frozen_resource),
            Err(CatalogError::InvalidPlan(_))
        ));
        Ok(())
    }

    #[test]
    fn json_loader_enforces_byte_limit_before_parsing() {
        let result = load_catalog_json_with_limit(br#"{}"#, 1);
        assert!(matches!(result, Err(CatalogError::InputTooLarge { .. })));
    }
}
