#![forbid(unsafe_code)]

//! Transport-neutral retrieval policy and backend contracts.
//!
//! This crate has no filesystem, network, SQLite, process, Tauri, or model
//! authority. Backends return ranked evidence; federation converts only ranks
//! to reciprocal-rank-fusion values. A backend's native score is retained for
//! inspection but is never compared with another backend's score.

use information_native_types::{
    EVIDENCE_SCHEMA, ErrorClass, EvidenceHit, EvidenceLocator, EvidenceSet, InformationCapability,
    InformationError, InformationQuery, MAX_RETRIEVAL_TIMEOUT_MS, ReleaseId, RepresentationId,
    ResourceId, RetrievalPurpose, UsePermission, UsePolicy,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

const DEFAULT_RRF_K: u32 = 60;
const HARD_MAX_ROUTED_BACKENDS: usize = 64;
const HARD_MAX_BACKEND_HITS: usize = 10_000;
const HARD_MAX_BACKEND_WARNINGS: usize = 256;

/// Exact identity and truthful capabilities of one mounted backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BackendDescriptor {
    pub backend_id: String,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub label: String,
    pub capabilities: BTreeSet<InformationCapability>,
    pub use_policy: UsePolicy,
}

impl BackendDescriptor {
    fn validate(&self) -> Result<(), InformationError> {
        if self.backend_id.trim().is_empty() || self.label.trim().is_empty() {
            return Err(invalid_input(
                "invalid_backend_descriptor",
                "backend descriptor identifiers and labels must not be empty",
            ));
        }
        if self.capabilities.is_empty() {
            return Err(invalid_input(
                "invalid_backend_descriptor",
                "backend descriptor must advertise at least one capability",
            ));
        }
        Ok(())
    }

    fn routing_key(&self) -> (&str, &str, &str, &str) {
        (
            self.resource_id.as_str(),
            self.release_id.as_str(),
            self.representation_id.as_str(),
            &self.backend_id,
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackendHealthStatus {
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BackendHealth {
    pub status: BackendHealthStatus,
    pub message: String,
}

impl BackendHealth {
    #[must_use]
    pub fn ready() -> Self {
        Self {
            status: BackendHealthStatus::Ready,
            message: "ready".to_string(),
        }
    }

    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: BackendHealthStatus::Unavailable,
            message: message.into(),
        }
    }
}

/// A backend-local response. `hits` must be in backend-native rank order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BackendSearchResult {
    pub complete: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub hits: Vec<EvidenceHit>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BackendReadResult {
    pub complete: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub hit: EvidenceHit,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub purpose: RetrievalPurpose,
    pub locator: EvidenceLocator,
    pub max_context_chars: u32,
    pub timeout_ms: u64,
}

impl ReadRequest {
    fn validate(&self) -> Result<(), InformationError> {
        validate_read_budget(self.max_context_chars, self.timeout_ms)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LookupRequest {
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub purpose: RetrievalPurpose,
    pub collection: Option<String>,
    pub key: String,
    pub max_context_chars: u32,
    pub timeout_ms: u64,
}

impl LookupRequest {
    fn validate(&self) -> Result<(), InformationError> {
        if self.key.trim().is_empty() || self.key.chars().count() > 8_192 {
            return Err(invalid_input(
                "invalid_lookup_key",
                "lookup key must contain between 1 and 8192 characters",
            ));
        }
        if self
            .collection
            .as_ref()
            .is_some_and(|collection| collection.trim().is_empty())
        {
            return Err(invalid_input(
                "invalid_lookup_collection",
                "lookup collection must not be empty when supplied",
            ));
        }
        validate_read_budget(self.max_context_chars, self.timeout_ms)
    }
}

fn validate_read_budget(max_context_chars: u32, timeout_ms: u64) -> Result<(), InformationError> {
    if max_context_chars == 0
        || max_context_chars > 2_000_000
        || timeout_ms == 0
        || timeout_ms > MAX_RETRIEVAL_TIMEOUT_MS
    {
        return Err(invalid_input(
            "invalid_read_budget",
            "read budget is outside the supported bounds",
        ));
    }
    Ok(())
}

/// Synchronous backend boundary suitable for native hosts and blocking worker
/// pools. Implementations must not invoke models or construct prompts.
pub trait ResourceBackend: Send + Sync {
    fn descriptor(&self) -> &BackendDescriptor;

    fn health(&self) -> BackendHealth;

    fn search(&self, query: &InformationQuery) -> Result<BackendSearchResult, InformationError> {
        let _ = query;
        Err(unsupported_operation(self.descriptor(), "lexical search"))
    }

    fn read(&self, request: &ReadRequest) -> Result<BackendReadResult, InformationError> {
        let _ = request;
        Err(unsupported_operation(self.descriptor(), "article read"))
    }

    fn lookup(&self, request: &LookupRequest) -> Result<BackendReadResult, InformationError> {
        let _ = request;
        Err(unsupported_operation(self.descriptor(), "record lookup"))
    }
}

fn unsupported_operation(descriptor: &BackendDescriptor, operation: &str) -> InformationError {
    let mut error = InformationError::new(
        ErrorClass::Unsupported,
        "backend_operation_unsupported",
        format!(
            "backend {} does not support {operation}",
            descriptor.backend_id
        ),
    );
    error.resource_id = Some(descriptor.resource_id.clone());
    error.representation_id = Some(descriptor.representation_id.clone());
    error
}

/// Deterministic capability router and bounded rank-fusion policy.
#[derive(Default)]
pub struct RetrievalRouter {
    backends: Vec<Arc<dyn ResourceBackend>>,
}

impl RetrievalRouter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_backends(
        backends: impl IntoIterator<Item = Arc<dyn ResourceBackend>>,
    ) -> Result<Self, InformationError> {
        let mut router = Self::new();
        for backend in backends {
            router.register(backend)?;
        }
        Ok(router)
    }

    pub fn register(&mut self, backend: Arc<dyn ResourceBackend>) -> Result<(), InformationError> {
        backend.descriptor().validate()?;
        if self
            .backends
            .iter()
            .any(|candidate| candidate.descriptor().backend_id == backend.descriptor().backend_id)
        {
            return Err(invalid_input(
                "duplicate_backend",
                format!(
                    "backend id {} is already registered",
                    backend.descriptor().backend_id
                ),
            ));
        }
        self.backends.push(backend);
        self.backends.sort_by(|left, right| {
            left.descriptor()
                .routing_key()
                .cmp(&right.descriptor().routing_key())
        });
        Ok(())
    }

    #[must_use]
    pub fn descriptors(&self) -> Vec<BackendDescriptor> {
        self.backends
            .iter()
            .map(|backend| backend.descriptor().clone())
            .collect()
    }

    #[must_use]
    pub fn health(&self) -> Vec<(BackendDescriptor, BackendHealth)> {
        self.backends
            .iter()
            .map(|backend| (backend.descriptor().clone(), backend.health()))
            .collect()
    }

    pub fn search(&self, query: &InformationQuery) -> Result<EvidenceSet, InformationError> {
        query.validate().map_err(|error| {
            invalid_input(
                "invalid_query",
                format!("query contract is invalid: {error}"),
            )
        })?;
        let started = Instant::now();
        let required_capabilities = required_capabilities(query);
        let mut eligible = self
            .backends
            .iter()
            .filter(|backend| descriptor_matches_query(backend.descriptor(), query))
            .filter(|backend| {
                required_capabilities
                    .iter()
                    .all(|capability| backend.descriptor().capabilities.contains(capability))
            })
            .collect::<Vec<_>>();
        eligible.sort_by(|left, right| {
            left.descriptor()
                .routing_key()
                .cmp(&right.descriptor().routing_key())
        });

        if eligible.is_empty() {
            return Err(unsupported_query(query, &required_capabilities));
        }

        let denied_backend_ids = eligible
            .iter()
            .filter(|backend| {
                backend
                    .descriptor()
                    .use_policy
                    .permission_for(query.purpose)
                    != UsePermission::Allowed
            })
            .map(|backend| backend.descriptor().backend_id.clone())
            .collect::<Vec<_>>();
        let permitted = eligible
            .iter()
            .filter(|backend| {
                backend
                    .descriptor()
                    .use_policy
                    .permission_for(query.purpose)
                    == UsePermission::Allowed
            })
            .copied()
            .collect::<Vec<_>>();
        if permitted.is_empty() {
            return Err(InformationError::new(
                ErrorClass::Permission,
                "retrieval_purpose_not_permitted",
                "the requested retrieval purpose is not explicitly allowed by any selected resource policy",
            ));
        }
        eligible = permitted;

        let mut complete = true;
        let mut warnings = BTreeSet::new();
        for target in &query.targets {
            let covered = eligible.iter().any(|backend| {
                let descriptor = backend.descriptor();
                descriptor.resource_id == target.resource_id
                    && descriptor.release_id == target.release_id
                    && descriptor.representation_id == target.representation_id
            });
            if !covered {
                complete = false;
                warnings.insert(format!(
                    "exact target {}/{}/{} has no capable, permitted backend",
                    target.resource_id, target.release_id, target.representation_id
                ));
            }
        }
        for resource in &query.resources {
            if !eligible
                .iter()
                .any(|backend| backend.descriptor().resource_id == *resource)
            {
                complete = false;
                warnings.insert(format!(
                    "requested resource {resource} has no capable, permitted backend"
                ));
            }
        }
        if !denied_backend_ids.is_empty() {
            complete = false;
            warnings.insert(format!(
                "retrieval-purpose policy excluded backends: {}",
                denied_backend_ids.join(", ")
            ));
        }
        let requested_backends = usize::from(query.budget.max_backends);
        let maximum_backends = requested_backends.min(HARD_MAX_ROUTED_BACKENDS);
        if requested_backends > HARD_MAX_ROUTED_BACKENDS
            && eligible.len() > HARD_MAX_ROUTED_BACKENDS
        {
            complete = false;
            warnings.insert(format!(
                "hard safety limit bounded routing to {HARD_MAX_ROUTED_BACKENDS} backends"
            ));
        }
        if eligible.len() > maximum_backends {
            complete = false;
            warnings.insert(format!(
                "backend budget selected {maximum_backends} of {} eligible backends",
                eligible.len()
            ));
            eligible.truncate(maximum_backends);
        }

        let mut ranked_lists = Vec::new();
        for backend in eligible {
            if elapsed_millis(started) >= query.budget.timeout_ms {
                complete = false;
                warnings.insert(format!(
                    "query timeout budget elapsed before backend {} started",
                    backend.descriptor().backend_id
                ));
                continue;
            }

            let backend_id = backend.descriptor().backend_id.clone();
            match backend.health() {
                BackendHealth {
                    status: BackendHealthStatus::Unavailable,
                    message,
                } => {
                    complete = false;
                    warnings.insert(format!("backend {backend_id} unavailable: {message}"));
                    continue;
                }
                BackendHealth {
                    status: BackendHealthStatus::Degraded,
                    message,
                } => {
                    warnings.insert(format!("backend {backend_id} degraded: {message}"));
                }
                BackendHealth {
                    status: BackendHealthStatus::Ready,
                    ..
                } => {}
            }

            let elapsed = elapsed_millis(started);
            let remaining_ms = query.budget.timeout_ms.saturating_sub(elapsed);
            if remaining_ms == 0 {
                complete = false;
                warnings.insert(format!(
                    "query soft deadline elapsed before backend {backend_id} started"
                ));
                continue;
            }
            let mut backend_query = query.clone();
            backend_query.budget.timeout_ms = remaining_ms;
            match backend.search(&backend_query) {
                Ok(mut response) => {
                    if elapsed_millis(started) > query.budget.timeout_ms {
                        complete = false;
                        warnings.insert(format!(
                            "backend {backend_id} exceeded the cooperative soft deadline; its results were discarded"
                        ));
                        continue;
                    }
                    if response.hits.len() > HARD_MAX_BACKEND_HITS
                        || response.warnings.len() > HARD_MAX_BACKEND_WARNINGS
                    {
                        complete = false;
                        warnings.insert(format!(
                            "backend {backend_id} violated the bounded response contract"
                        ));
                        continue;
                    }
                    if !response.complete {
                        complete = false;
                    }
                    for warning in response.warnings {
                        warnings.insert(format!("backend {backend_id}: {warning}"));
                    }
                    response.hits.sort_by(|left, right| {
                        left.rank
                            .cmp(&right.rank)
                            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
                    });
                    let per_backend_limit =
                        query.budget.max_hits_per_backend.min(query.budget.max_hits) as usize;
                    if response.hits.len() > per_backend_limit {
                        response.hits.truncate(per_backend_limit);
                        complete = false;
                        warnings.insert(format!(
                            "backend {backend_id}: hit list exceeded the per-backend budget"
                        ));
                    }
                    match validate_backend_hits(backend.descriptor(), &response.hits, query.purpose)
                    {
                        Ok(()) => ranked_lists.push((backend_id, response.hits)),
                        Err(error) => {
                            complete = false;
                            warnings.insert(format!(
                                "backend {backend_id} returned invalid evidence: {}",
                                error.safe_message
                            ));
                        }
                    }
                }
                Err(error) => {
                    complete = false;
                    warnings.insert(format!(
                        "backend {backend_id} failed [{}]: {}",
                        error.code, error.safe_message
                    ));
                }
            }
        }

        if elapsed_millis(started) > query.budget.timeout_ms {
            complete = false;
            warnings.insert("query timeout budget was exceeded".to_string());
        }

        let mut hits = fuse_ranked_lists(ranked_lists, DEFAULT_RRF_K);
        if hits.len() > query.budget.max_hits as usize {
            hits.truncate(query.budget.max_hits as usize);
            complete = false;
            warnings.insert("global hit budget truncated fused evidence".to_string());
        }
        for (index, hit) in hits.iter_mut().enumerate() {
            hit.rank = usize_to_u32_saturated(index.saturating_add(1));
        }

        let result = EvidenceSet {
            schema: EVIDENCE_SCHEMA.to_string(),
            query_id: query.query_id.clone(),
            complete,
            warnings: warnings.into_iter().collect(),
            hits,
            elapsed_ms: elapsed_millis(started),
        };
        result.validate(&query.budget).map_err(|error| {
            InformationError::new(
                ErrorClass::Internal,
                "invalid_federated_evidence",
                format!("federated evidence failed contract validation: {error}"),
            )
        })?;
        Ok(result)
    }

    pub fn read(&self, request: &ReadRequest) -> Result<BackendReadResult, InformationError> {
        request.validate()?;
        let started = Instant::now();
        let result = self.route_single(
            &request.resource_id,
            &request.release_id,
            &request.representation_id,
            request.purpose,
            InformationCapability::ArticleRead,
            |backend| {
                let result = backend.read(request)?;
                validate_backend_read_result(
                    backend.descriptor(),
                    &result,
                    request.purpose,
                    request.max_context_chars,
                )?;
                if result.hit.locator != request.locator {
                    return Err(InformationError::new(
                        ErrorClass::Integrity,
                        "backend_read_locator_mismatch",
                        "backend read result locator does not match the requested locator",
                    ));
                }
                Ok(result)
            },
        )?;
        enforce_soft_deadline(started, request.timeout_ms)?;
        Ok(result)
    }

    pub fn lookup(&self, request: &LookupRequest) -> Result<BackendReadResult, InformationError> {
        request.validate()?;
        let started = Instant::now();
        let result = self.route_single(
            &request.resource_id,
            &request.release_id,
            &request.representation_id,
            request.purpose,
            InformationCapability::RecordLookup,
            |backend| {
                let result = backend.lookup(request)?;
                validate_backend_read_result(
                    backend.descriptor(),
                    &result,
                    request.purpose,
                    request.max_context_chars,
                )?;
                if !locator_matches_lookup(
                    &result.hit.locator,
                    request.collection.as_deref(),
                    &request.key,
                ) {
                    return Err(InformationError::new(
                        ErrorClass::Integrity,
                        "backend_lookup_key_mismatch",
                        "backend lookup result locator does not match the requested key",
                    ));
                }
                Ok(result)
            },
        )?;
        enforce_soft_deadline(started, request.timeout_ms)?;
        Ok(result)
    }

    fn route_single<F>(
        &self,
        resource_id: &ResourceId,
        release_id: &ReleaseId,
        representation_id: &RepresentationId,
        purpose: RetrievalPurpose,
        capability: InformationCapability,
        operation: F,
    ) -> Result<BackendReadResult, InformationError>
    where
        F: Fn(&Arc<dyn ResourceBackend>) -> Result<BackendReadResult, InformationError>,
    {
        let identity_matches = |backend: &&Arc<dyn ResourceBackend>| {
            let descriptor = backend.descriptor();
            &descriptor.resource_id == resource_id
                && &descriptor.release_id == release_id
                && &descriptor.representation_id == representation_id
                && descriptor.capabilities.contains(&capability)
        };
        let matching = self
            .backends
            .iter()
            .filter(identity_matches)
            .collect::<Vec<_>>();
        if !matching.is_empty()
            && matching.iter().all(|backend| {
                backend.descriptor().use_policy.permission_for(purpose) != UsePermission::Allowed
            })
        {
            return Err(InformationError::new(
                ErrorClass::Permission,
                "retrieval_purpose_not_permitted",
                "the selected resource policy does not explicitly allow this retrieval purpose",
            ));
        }
        let candidates = matching.into_iter().filter(|backend| {
            backend.descriptor().use_policy.permission_for(purpose) == UsePermission::Allowed
        });
        let mut failures = Vec::new();
        let mut found = false;
        for backend in candidates {
            found = true;
            let health = backend.health();
            if health.status == BackendHealthStatus::Unavailable {
                failures.push(format!(
                    "{} unavailable: {}",
                    backend.descriptor().backend_id,
                    health.message
                ));
                continue;
            }
            match operation(backend) {
                Ok(result) => return Ok(result),
                Err(error) => failures.push(format!(
                    "{} [{}]: {}",
                    backend.descriptor().backend_id,
                    error.code,
                    error.safe_message
                )),
            }
        }
        if !found {
            return Err(InformationError::new(
                ErrorClass::Unsupported,
                "no_capable_backend",
                "no selected backend supports the requested operation",
            ));
        }
        Err(InformationError::new(
            ErrorClass::Backend,
            "all_backends_failed",
            format!("all matching backends failed: {}", failures.join("; ")),
        ))
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).map_or(u64::MAX, |value| value)
}

fn usize_to_u32_saturated(value: usize) -> u32 {
    u32::try_from(value).map_or(u32::MAX, |converted| converted)
}

fn descriptor_matches_query(descriptor: &BackendDescriptor, query: &InformationQuery) -> bool {
    (query.targets.is_empty()
        || query.targets.iter().any(|target| {
            target.resource_id == descriptor.resource_id
                && target.release_id == descriptor.release_id
                && target.representation_id == descriptor.representation_id
        }))
        && (query.resources.is_empty() || query.resources.contains(&descriptor.resource_id))
        && (query.representations.is_empty()
            || query
                .representations
                .contains(&descriptor.representation_id))
}

fn required_capabilities(query: &InformationQuery) -> BTreeSet<InformationCapability> {
    let mut capabilities = BTreeSet::from([InformationCapability::LexicalSearch]);
    if query.filters.spatial.is_some() {
        capabilities.insert(InformationCapability::SpatialFilter);
    }
    if query.filters.temporal_start.is_some() || query.filters.temporal_end.is_some() {
        capabilities.insert(InformationCapability::TemporalFilter);
    }
    capabilities
}

fn unsupported_query(
    query: &InformationQuery,
    capabilities: &BTreeSet<InformationCapability>,
) -> InformationError {
    let requested_scope = if query.resources.is_empty() && query.representations.is_empty() {
        "the registered resources".to_string()
    } else {
        "the selected resource representations".to_string()
    };
    InformationError::new(
        ErrorClass::Unsupported,
        "no_capable_backend",
        format!(
            "no backend in {requested_scope} supports all required capabilities: {capabilities:?}"
        ),
    )
}

fn validate_backend_hits(
    descriptor: &BackendDescriptor,
    hits: &[EvidenceHit],
    purpose: RetrievalPurpose,
) -> Result<(), InformationError> {
    for hit in hits {
        if hit.resource_id != descriptor.resource_id
            || hit.release_id != descriptor.release_id
            || hit.representation_id != descriptor.representation_id
            || hit.backend_id != descriptor.backend_id
            || hit.rank == 0
            || !hit.score.value.is_finite()
        {
            return Err(InformationError::new(
                ErrorClass::Integrity,
                "backend_evidence_identity_mismatch",
                "backend evidence identity, rank, or score does not match its descriptor",
            ));
        }
        if hit.use_policy.permission_for(purpose) != UsePermission::Allowed {
            return Err(InformationError::new(
                ErrorClass::Permission,
                "backend_evidence_purpose_not_permitted",
                "backend evidence policy does not explicitly allow the requested retrieval purpose",
            ));
        }
    }
    Ok(())
}

fn validate_backend_read_result(
    descriptor: &BackendDescriptor,
    result: &BackendReadResult,
    purpose: RetrievalPurpose,
    max_context_chars: u32,
) -> Result<(), InformationError> {
    if result.warnings.len() > HARD_MAX_BACKEND_WARNINGS {
        return Err(InformationError::new(
            ErrorClass::Integrity,
            "backend_warning_limit_exceeded",
            "backend read result exceeded the warning limit",
        ));
    }
    validate_backend_hits(descriptor, std::slice::from_ref(&result.hit), purpose)?;
    result.hit.validate().map_err(|error| {
        InformationError::new(
            ErrorClass::Integrity,
            "backend_evidence_contract_invalid",
            format!("backend evidence failed contract validation: {error}"),
        )
    })?;
    let observed = result
        .hit
        .snippet
        .chars()
        .count()
        .saturating_add(result.hit.context.chars().count());
    let observed = u32::try_from(observed).map_err(|_| {
        InformationError::new(
            ErrorClass::Integrity,
            "backend_text_budget_overflow",
            "backend text length cannot be represented by the contract",
        )
    })?;
    if observed > max_context_chars {
        return Err(InformationError::new(
            ErrorClass::Integrity,
            "backend_text_budget_exceeded",
            "backend read result exceeded the requested text budget",
        ));
    }
    Ok(())
}

fn locator_matches_lookup(
    locator: &EvidenceLocator,
    requested_collection: Option<&str>,
    key: &str,
) -> bool {
    match locator {
        EvidenceLocator::SqliteBlock { block_id, .. } => {
            requested_collection.is_none() && block_id == key
        }
        EvidenceLocator::Record {
            collection,
            key: observed,
        } => collection.as_deref() == requested_collection && observed == key,
        EvidenceLocator::ZimArticle { internal_path, .. } => {
            requested_collection.is_none() && internal_path == key
        }
        EvidenceLocator::OvertureFeature { gers_id } => {
            requested_collection.is_none() && gers_id == key
        }
        _ => false,
    }
}

fn enforce_soft_deadline(started: Instant, timeout_ms: u64) -> Result<(), InformationError> {
    if elapsed_millis(started) > timeout_ms {
        return Err(InformationError::new(
            ErrorClass::Backend,
            "cooperative_soft_deadline_exceeded",
            "backend operation exceeded its cooperative soft deadline; results were discarded",
        )
        .retryable(true));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EvidenceDedupeKey {
    resource_id: String,
    release_id: String,
    representation_id: String,
    document_id: String,
    passage_id: String,
    source_fingerprint: String,
    locator_and_policy: String,
}

impl EvidenceDedupeKey {
    fn from_hit(hit: &EvidenceHit) -> Self {
        let document_id = match &hit.document_id {
            Some(value) => value.clone(),
            None => hit.evidence_id.clone(),
        };
        let passage_id = match &hit.passage_id {
            Some(value) => value.clone(),
            None => hit.evidence_id.clone(),
        };
        Self {
            resource_id: hit.resource_id.to_string(),
            release_id: hit.release_id.to_string(),
            representation_id: hit.representation_id.to_string(),
            document_id,
            passage_id,
            source_fingerprint: hit.source_fingerprint.clone().unwrap_or_default(),
            locator_and_policy: serde_json::to_string(&(
                &hit.locator,
                &hit.provenance,
                &hit.rights,
                hit.use_policy,
                &hit.excerpt_sha256,
            ))
            .unwrap_or_else(|_| format!("unserializable:{}", hit.evidence_id)),
        }
    }
}

struct FusedCandidate {
    hit: EvidenceHit,
    fused_value: f64,
    best_source_rank: u32,
    contributing_backends: BTreeSet<String>,
}

fn fuse_ranked_lists(
    ranked_lists: Vec<(String, Vec<EvidenceHit>)>,
    rrf_k: u32,
) -> Vec<EvidenceHit> {
    let mut fused = BTreeMap::<EvidenceDedupeKey, FusedCandidate>::new();
    for (backend_id, hits) in ranked_lists {
        for (position, mut hit) in hits.into_iter().enumerate() {
            let source_rank = usize_to_u32_saturated(position.saturating_add(1));
            hit.metadata
                .entry("source_rank".to_string())
                .or_insert_with(|| JsonValue::from(source_rank));
            let denominator = f64::from(rrf_k.saturating_add(source_rank));
            let contribution = if denominator > 0.0 {
                1.0 / denominator
            } else {
                0.0
            };
            let key = EvidenceDedupeKey::from_hit(&hit);
            if let Some(candidate) = fused.get_mut(&key) {
                if candidate.contributing_backends.insert(backend_id.clone()) {
                    candidate.fused_value += contribution;
                }
                if source_rank < candidate.best_source_rank
                    || (source_rank == candidate.best_source_rank
                        && hit.backend_id < candidate.hit.backend_id)
                {
                    hit.score.fused_value = Some(candidate.fused_value);
                    candidate.hit = hit;
                    candidate.best_source_rank = source_rank;
                }
            } else {
                hit.score.fused_value = Some(contribution);
                fused.insert(
                    key,
                    FusedCandidate {
                        hit,
                        fused_value: contribution,
                        best_source_rank: source_rank,
                        contributing_backends: BTreeSet::from([backend_id.clone()]),
                    },
                );
            }
        }
    }

    let mut candidates = fused.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .fused_value
            .total_cmp(&left.fused_value)
            .then_with(|| left.best_source_rank.cmp(&right.best_source_rank))
            .then_with(|| left.hit.resource_id.cmp(&right.hit.resource_id))
            .then_with(|| left.hit.document_id.cmp(&right.hit.document_id))
            .then_with(|| left.hit.passage_id.cmp(&right.hit.passage_id))
            .then_with(|| left.hit.evidence_id.cmp(&right.hit.evidence_id))
    });
    candidates
        .into_iter()
        .map(|mut candidate| {
            candidate.hit.score.fused_value = Some(candidate.fused_value);
            candidate.hit.metadata.insert(
                "fusion".to_string(),
                json!({
                    "method": "reciprocal_rank_fusion",
                    "rrf_k": rrf_k,
                    "contributing_backends": candidate
                        .contributing_backends
                        .into_iter()
                        .collect::<Vec<_>>(),
                }),
            );
            candidate.hit
        })
        .collect()
}

fn invalid_input(code: &'static str, message: impl Into<String>) -> InformationError {
    InformationError::new(ErrorClass::InvalidInput, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use information_native_types::{
        EVIDENCE_SCHEMA, EvidenceScore, QUERY_SCHEMA, QueryBudget, QueryFilters, QueryId,
        QuerySyntax, RightsStatement, ScoreSemantics, UsePolicy,
    };
    use std::error::Error;

    #[derive(Clone)]
    struct FakeBackend {
        descriptor: BackendDescriptor,
        health: BackendHealth,
        response: Result<BackendSearchResult, InformationError>,
    }

    #[derive(Clone)]
    struct FakeReadBackend {
        descriptor: BackendDescriptor,
        hit: EvidenceHit,
    }

    impl ResourceBackend for FakeReadBackend {
        fn descriptor(&self) -> &BackendDescriptor {
            &self.descriptor
        }

        fn health(&self) -> BackendHealth {
            BackendHealth::ready()
        }

        fn read(&self, _request: &ReadRequest) -> Result<BackendReadResult, InformationError> {
            Ok(BackendReadResult {
                complete: true,
                warnings: Vec::new(),
                hit: self.hit.clone(),
                elapsed_ms: 0,
            })
        }

        fn lookup(&self, _request: &LookupRequest) -> Result<BackendReadResult, InformationError> {
            Ok(BackendReadResult {
                complete: true,
                warnings: Vec::new(),
                hit: self.hit.clone(),
                elapsed_ms: 0,
            })
        }
    }

    impl ResourceBackend for FakeBackend {
        fn descriptor(&self) -> &BackendDescriptor {
            &self.descriptor
        }

        fn health(&self) -> BackendHealth {
            self.health.clone()
        }

        fn search(
            &self,
            _query: &InformationQuery,
        ) -> Result<BackendSearchResult, InformationError> {
            self.response.clone()
        }
    }

    fn id<T>(
        value: &str,
        parse: impl FnOnce(String) -> Result<T, information_native_types::ContractError>,
    ) -> Result<T, Box<dyn Error>> {
        Ok(parse(value.to_string())?)
    }

    fn descriptor(
        backend_id: &str,
        representation: &str,
    ) -> Result<BackendDescriptor, Box<dyn Error>> {
        Ok(BackendDescriptor {
            backend_id: backend_id.to_string(),
            resource_id: id("resource", ResourceId::parse)?,
            release_id: id("release", ReleaseId::parse)?,
            representation_id: id(representation, RepresentationId::parse)?,
            label: backend_id.to_string(),
            capabilities: BTreeSet::from([InformationCapability::LexicalSearch]),
            use_policy: UsePolicy {
                attribution_required: false,
                ..UsePolicy::default()
            },
        })
    }

    fn hit(
        descriptor: &BackendDescriptor,
        evidence: &str,
        document: &str,
        passage: &str,
        rank: u32,
        raw_score: f64,
        semantics: ScoreSemantics,
    ) -> EvidenceHit {
        EvidenceHit {
            evidence_id: evidence.to_string(),
            resource_id: descriptor.resource_id.clone(),
            release_id: descriptor.release_id.clone(),
            representation_id: descriptor.representation_id.clone(),
            backend_id: descriptor.backend_id.clone(),
            rank,
            score: EvidenceScore {
                value: raw_score,
                semantics,
                fused_value: None,
            },
            title: document.to_string(),
            creator: None,
            snippet: passage.to_string(),
            context: String::new(),
            excerpt_sha256: information_native_types::evidence_text_sha256(passage, ""),
            source_fingerprint: None,
            document_id: Some(document.to_string()),
            passage_id: Some(passage.to_string()),
            locator: EvidenceLocator::Record {
                collection: None,
                key: passage.to_string(),
            },
            source_uri: None,
            provenance: information_native_types::Provenance {
                publisher: "fixture".to_string(),
                source_uri: "fixture://source".to_string(),
                upstream_record_id: None,
                source_inputs: Vec::new(),
                transformation: None,
                metadata: BTreeMap::new(),
            },
            rights: Vec::<RightsStatement>::new(),
            use_policy: UsePolicy {
                attribution_required: false,
                ..UsePolicy::default()
            },
            metadata: BTreeMap::new(),
        }
    }

    fn query(max_hits: u32, max_backends: u16) -> Result<InformationQuery, Box<dyn Error>> {
        Ok(InformationQuery {
            schema: QUERY_SCHEMA.to_string(),
            query_id: QueryId::new(),
            text: "quiet prayer".to_string(),
            syntax: QuerySyntax::NaturalTerms,
            purpose: RetrievalPurpose::LocalUi,
            targets: Vec::new(),
            resources: Vec::new(),
            representations: Vec::new(),
            filters: QueryFilters::default(),
            budget: QueryBudget {
                max_hits,
                max_hits_per_backend: 10,
                max_backends,
                max_context_chars: 10_000,
                timeout_ms: 10_000,
            },
        })
    }

    #[test]
    fn fusion_uses_rank_not_raw_backend_score() -> Result<(), Box<dyn Error>> {
        let first = descriptor("a", "rep-a")?;
        let second = descriptor("b", "rep-b")?;
        let a_hits = vec![
            hit(
                &first,
                "a:x",
                "doc-x",
                "p-x",
                1,
                10_000.0,
                ScoreSemantics::HigherIsBetter,
            ),
            hit(
                &first,
                "a:y",
                "doc-y",
                "p-y",
                2,
                9_999.0,
                ScoreSemantics::HigherIsBetter,
            ),
        ];
        let b_hits = vec![
            hit(
                &second,
                "b:x",
                "doc-x",
                "p-x",
                1,
                -0.001,
                ScoreSemantics::LowerIsBetter,
            ),
            hit(
                &second,
                "b:z",
                "doc-z",
                "p-z",
                2,
                -1_000_000.0,
                ScoreSemantics::LowerIsBetter,
            ),
        ];
        let router = RetrievalRouter::from_backends([
            Arc::new(FakeBackend {
                descriptor: first,
                health: BackendHealth::ready(),
                response: Ok(BackendSearchResult {
                    complete: true,
                    warnings: Vec::new(),
                    hits: a_hits,
                    elapsed_ms: 1,
                }),
            }) as Arc<dyn ResourceBackend>,
            Arc::new(FakeBackend {
                descriptor: second,
                health: BackendHealth::ready(),
                response: Ok(BackendSearchResult {
                    complete: true,
                    warnings: Vec::new(),
                    hits: b_hits,
                    elapsed_ms: 1,
                }),
            }) as Arc<dyn ResourceBackend>,
        ])?;
        let evidence = router.search(&query(10, 8)?)?;
        assert_eq!(evidence.schema, EVIDENCE_SCHEMA);
        assert_eq!(evidence.hits.len(), 4);
        let first = evidence
            .hits
            .first()
            .ok_or_else(|| std::io::Error::other("fused evidence was unexpectedly empty"))?;
        assert_eq!(first.passage_id.as_deref(), Some("p-x"));
        assert!(first.score.fused_value.is_some_and(|score| score > 0.016));
        Ok(())
    }

    #[test]
    fn fusion_order_preserves_rank_strength() -> Result<(), Box<dyn Error>> {
        let source = descriptor("a", "rep-a")?;
        let response = BackendSearchResult {
            complete: true,
            warnings: Vec::new(),
            hits: vec![
                hit(
                    &source,
                    "1",
                    "doc-a",
                    "a1",
                    1,
                    1.0,
                    ScoreSemantics::RankOnly,
                ),
                hit(
                    &source,
                    "2",
                    "doc-a",
                    "a2",
                    2,
                    1.0,
                    ScoreSemantics::RankOnly,
                ),
                hit(
                    &source,
                    "3",
                    "doc-b",
                    "b1",
                    3,
                    1.0,
                    ScoreSemantics::RankOnly,
                ),
            ],
            elapsed_ms: 1,
        };
        let router = RetrievalRouter::from_backends([Arc::new(FakeBackend {
            descriptor: source,
            health: BackendHealth::ready(),
            response: Ok(response),
        }) as Arc<dyn ResourceBackend>])?;
        let evidence = router.search(&query(10, 8)?)?;
        let passages = evidence
            .hits
            .iter()
            .filter_map(|hit| hit.passage_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(passages, vec!["a1", "a2", "b1"]);
        Ok(())
    }

    #[test]
    fn unavailable_and_budget_skips_are_explicitly_partial() -> Result<(), Box<dyn Error>> {
        let first = descriptor("a", "rep-a")?;
        let second = descriptor("b", "rep-b")?;
        let router = RetrievalRouter::from_backends([
            Arc::new(FakeBackend {
                descriptor: first,
                health: BackendHealth::unavailable("offline"),
                response: Ok(BackendSearchResult {
                    complete: true,
                    warnings: Vec::new(),
                    hits: Vec::new(),
                    elapsed_ms: 0,
                }),
            }) as Arc<dyn ResourceBackend>,
            Arc::new(FakeBackend {
                descriptor: second,
                health: BackendHealth::ready(),
                response: Ok(BackendSearchResult {
                    complete: true,
                    warnings: Vec::new(),
                    hits: Vec::new(),
                    elapsed_ms: 0,
                }),
            }) as Arc<dyn ResourceBackend>,
        ])?;
        let evidence = router.search(&query(10, 1)?)?;
        assert!(!evidence.complete);
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("selected 1 of 2"))
        );
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("unavailable"))
        );
        Ok(())
    }

    #[test]
    fn forbidden_policy_fails_before_backend_search() -> Result<(), Box<dyn Error>> {
        let mut source = descriptor("denied", "rep-denied")?;
        source.use_policy.local_search = UsePermission::Forbidden;
        let router = RetrievalRouter::from_backends([Arc::new(FakeBackend {
            descriptor: source,
            health: BackendHealth::ready(),
            response: Ok(BackendSearchResult {
                complete: true,
                warnings: Vec::new(),
                hits: Vec::new(),
                elapsed_ms: 0,
            }),
        }) as Arc<dyn ResourceBackend>])?;
        let error = router.search(&query(10, 1)?).err().ok_or_else(|| {
            std::io::Error::other("forbidden router unexpectedly executed search")
        })?;
        assert_eq!(error.class, ErrorClass::Permission);
        assert_eq!(error.code, "retrieval_purpose_not_permitted");
        Ok(())
    }

    #[test]
    fn model_context_requires_explicit_policy_and_exact_target() -> Result<(), Box<dyn Error>> {
        let mut source = descriptor("model", "rep-model")?;
        let mut response = BackendSearchResult {
            complete: true,
            warnings: Vec::new(),
            hits: vec![hit(
                &source,
                "model:block",
                "doc",
                "block",
                1,
                1.0,
                ScoreSemantics::RankOnly,
            )],
            elapsed_ms: 0,
        };
        let mut denied_query = query(10, 1)?;
        denied_query.purpose = RetrievalPurpose::ModelContext;
        denied_query.targets = vec![information_native_types::RetrievalTarget {
            resource_id: source.resource_id.clone(),
            release_id: source.release_id.clone(),
            representation_id: source.representation_id.clone(),
        }];
        let denied = RetrievalRouter::from_backends([Arc::new(FakeBackend {
            descriptor: source.clone(),
            health: BackendHealth::ready(),
            response: Ok(response.clone()),
        }) as Arc<dyn ResourceBackend>])?;
        let error = denied
            .search(&denied_query)
            .err()
            .ok_or_else(|| std::io::Error::other("unknown model permission was accepted"))?;
        assert_eq!(error.class, ErrorClass::Permission);

        source.use_policy.model_context = UsePermission::Allowed;
        for hit in &mut response.hits {
            hit.use_policy.model_context = UsePermission::Allowed;
        }
        let allowed = RetrievalRouter::from_backends([Arc::new(FakeBackend {
            descriptor: source,
            health: BackendHealth::ready(),
            response: Ok(response),
        }) as Arc<dyn ResourceBackend>])?;
        assert_eq!(allowed.search(&denied_query)?.hits.len(), 1);
        Ok(())
    }

    #[test]
    fn search_rejects_hit_policy_narrower_than_descriptor() -> Result<(), Box<dyn Error>> {
        let mut source = descriptor("adversarial-search", "rep-search")?;
        source.use_policy.model_context = UsePermission::Allowed;
        let denied_hit = hit(
            &source,
            "adversarial:block",
            "doc",
            "block",
            1,
            1.0,
            ScoreSemantics::RankOnly,
        );
        assert_eq!(
            source
                .use_policy
                .permission_for(RetrievalPurpose::ModelContext),
            UsePermission::Allowed
        );
        assert_ne!(
            denied_hit
                .use_policy
                .permission_for(RetrievalPurpose::ModelContext),
            UsePermission::Allowed
        );

        let mut request = query(10, 1)?;
        request.purpose = RetrievalPurpose::ModelContext;
        request.targets = vec![information_native_types::RetrievalTarget {
            resource_id: source.resource_id.clone(),
            release_id: source.release_id.clone(),
            representation_id: source.representation_id.clone(),
        }];
        let router = RetrievalRouter::from_backends([Arc::new(FakeBackend {
            descriptor: source,
            health: BackendHealth::ready(),
            response: Ok(BackendSearchResult {
                complete: true,
                warnings: Vec::new(),
                hits: vec![denied_hit],
                elapsed_ms: 0,
            }),
        }) as Arc<dyn ResourceBackend>])?;

        let evidence = router.search(&request)?;
        assert!(evidence.hits.is_empty());
        assert!(!evidence.complete);
        assert!(evidence.warnings.iter().any(|warning| {
            warning.contains("backend evidence policy does not explicitly allow")
        }));
        Ok(())
    }

    #[test]
    fn read_and_lookup_reject_hit_policy_narrower_than_descriptor() -> Result<(), Box<dyn Error>> {
        let mut source = descriptor("adversarial-read", "rep-read")?;
        source
            .capabilities
            .insert(InformationCapability::ArticleRead);
        source
            .capabilities
            .insert(InformationCapability::RecordLookup);
        source.use_policy.model_context = UsePermission::Allowed;
        let denied_hit = hit(
            &source,
            "adversarial:block",
            "doc",
            "block",
            1,
            1.0,
            ScoreSemantics::RankOnly,
        );
        let router = RetrievalRouter::from_backends([Arc::new(FakeReadBackend {
            descriptor: source.clone(),
            hit: denied_hit,
        }) as Arc<dyn ResourceBackend>])?;

        let read_error = router
            .read(&ReadRequest {
                resource_id: source.resource_id.clone(),
                release_id: source.release_id.clone(),
                representation_id: source.representation_id.clone(),
                purpose: RetrievalPurpose::ModelContext,
                locator: EvidenceLocator::Record {
                    collection: None,
                    key: "block".to_string(),
                },
                max_context_chars: 1_000,
                timeout_ms: 1_000,
            })
            .expect_err("read unexpectedly returned evidence denied by its hit policy");
        assert_eq!(read_error.code, "all_backends_failed");
        assert!(
            read_error
                .safe_message
                .contains("backend_evidence_purpose_not_permitted")
        );

        let lookup_error = router
            .lookup(&LookupRequest {
                resource_id: source.resource_id,
                release_id: source.release_id,
                representation_id: source.representation_id,
                purpose: RetrievalPurpose::ModelContext,
                collection: Some("blocks".to_string()),
                key: "block".to_string(),
                max_context_chars: 1_000,
                timeout_ms: 1_000,
            })
            .expect_err("lookup unexpectedly returned evidence denied by its hit policy");
        assert_eq!(lookup_error.code, "all_backends_failed");
        assert!(
            lookup_error
                .safe_message
                .contains("backend_evidence_purpose_not_permitted")
        );
        Ok(())
    }

    #[test]
    fn descriptors_health_read_and_lookup_are_deterministic() -> Result<(), Box<dyn Error>> {
        let mut source = descriptor("reader", "rep-reader")?;
        source
            .capabilities
            .insert(InformationCapability::ArticleRead);
        source
            .capabilities
            .insert(InformationCapability::RecordLookup);
        let source_hit = hit(
            &source,
            "reader:block",
            "doc",
            "block",
            1,
            1.0,
            ScoreSemantics::RankOnly,
        );
        let router = RetrievalRouter::from_backends([Arc::new(FakeReadBackend {
            descriptor: source.clone(),
            hit: source_hit,
        }) as Arc<dyn ResourceBackend>])?;
        assert_eq!(router.descriptors(), vec![source.clone()]);
        let health = router.health();
        let first_health = health
            .first()
            .ok_or_else(|| std::io::Error::other("router returned no health descriptor"))?;
        assert_eq!(first_health.1.status, BackendHealthStatus::Ready);

        let read = router.read(&ReadRequest {
            resource_id: source.resource_id.clone(),
            release_id: source.release_id.clone(),
            representation_id: source.representation_id.clone(),
            purpose: RetrievalPurpose::LocalUi,
            locator: EvidenceLocator::Record {
                collection: None,
                key: "block".to_string(),
            },
            max_context_chars: 1_000,
            timeout_ms: 1_000,
        })?;
        assert_eq!(read.hit.passage_id.as_deref(), Some("block"));

        let lookup = router.lookup(&LookupRequest {
            resource_id: source.resource_id,
            release_id: source.release_id,
            representation_id: source.representation_id,
            purpose: RetrievalPurpose::LocalUi,
            collection: None,
            key: "block".to_string(),
            max_context_chars: 1_000,
            timeout_ms: 1_000,
        })?;
        assert_eq!(lookup.hit.evidence_id, read.hit.evidence_id);
        Ok(())
    }

    #[test]
    fn lookup_matching_includes_the_requested_collection() {
        let locator = EvidenceLocator::Record {
            collection: Some("messages".to_string()),
            key: "42".to_string(),
        };
        assert!(locator_matches_lookup(&locator, Some("messages"), "42"));
        assert!(!locator_matches_lookup(&locator, Some("accounts"), "42"));
        assert!(!locator_matches_lookup(&locator, None, "42"));
    }

    #[test]
    fn direct_read_and_lookup_timeouts_have_a_hard_ceiling() -> Result<(), Box<dyn Error>> {
        let source = descriptor("timeout", "rep-timeout")?;
        let read = ReadRequest {
            resource_id: source.resource_id.clone(),
            release_id: source.release_id.clone(),
            representation_id: source.representation_id.clone(),
            purpose: RetrievalPurpose::LocalUi,
            locator: EvidenceLocator::Record {
                collection: None,
                key: "key".to_string(),
            },
            max_context_chars: 1,
            timeout_ms: MAX_RETRIEVAL_TIMEOUT_MS + 1,
        };
        assert!(read.validate().is_err());
        let lookup = LookupRequest {
            resource_id: source.resource_id,
            release_id: source.release_id,
            representation_id: source.representation_id,
            purpose: RetrievalPurpose::LocalUi,
            collection: None,
            key: "key".to_string(),
            max_context_chars: 1,
            timeout_ms: MAX_RETRIEVAL_TIMEOUT_MS + 1,
        };
        assert!(lookup.validate().is_err());
        Ok(())
    }
}
