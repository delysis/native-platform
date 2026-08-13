#![forbid(unsafe_code)]

//! Optional, permissioned Tauri 2 embedding surface for
//! `information-native-kit`.
//!
//! The application constructs the product-neutral [`InformationHost`] and
//! injects it into [`Builder`]. The plugin does not choose storage paths,
//! catalogues, mounted backends, or filesystem-picker policy.
//!
//! Generic search, read, and lookup IPC is local-UI-only. Model retrieval is
//! available solely through the typed `information_query` command, which the
//! host evaluates with model-context purpose under a separate permission set.

use chrono::Utc;
use information_native_catalog::{CatalogSearchQuery, PlanRequest, SearchTextMode};
use information_native_host::{
    CatalogHit, HostError, HostStatus, InformationHost, InformationToolCall, InformationToolResult,
    MountInstallationOptions,
};
use information_native_retrieval::{
    BackendDescriptor, BackendReadResult, LookupRequest, ReadRequest,
};
use information_native_store::{
    ExternalRegistrationRequest, RemovalKind, RemovalPlan, StoreSnapshot,
};
use information_native_types::{
    ArtifactId, ErrorClass, EvidenceLocator, EvidenceSet, ExternalAccessMode, ExternalRegistration,
    FormatKind, InformationCapability, InformationError, InformationQuery, InstallPlan,
    InstallReceipt, InstallSelection, InstallationId, Provenance, QueryBudget, QueryFilters,
    QueryId, QuerySyntax, ReleaseId, RepresentationFormat, RepresentationId, ResourceId,
    ResourceKind, RetrievalPurpose, RetrievalTarget, RightsStatement, UsePolicy,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{Manager, Runtime, State};

/// Configures the plugin with an application-owned information host.
pub struct Builder {
    host: Arc<InformationHost>,
    external_path_grants: Option<Arc<dyn ExternalPathGrantResolver>>,
}

impl Builder {
    #[must_use]
    pub fn new(host: Arc<InformationHost>) -> Self {
        Self {
            host,
            external_path_grants: None,
        }
    }

    /// Supply an application-owned, preferably one-time resolver for opaque
    /// native picker grants. Raw filesystem paths are never accepted over IPC.
    #[must_use]
    pub fn external_path_grants(mut self, resolver: Arc<dyn ExternalPathGrantResolver>) -> Self {
        self.external_path_grants = Some(resolver);
        self
    }

    #[must_use]
    pub fn build<R: Runtime>(self) -> TauriPlugin<R> {
        let host = self.host;
        let external_path_grants = self.external_path_grants;
        PluginBuilder::new("information-native")
            .invoke_handler(tauri::generate_handler![
                information_status,
                information_catalog_search,
                information_installed,
                information_resolve_install_plan,
                information_query,
                information_search,
                information_read,
                information_lookup,
                information_register_external,
                information_install,
                information_mount_installation,
                information_plan_removal,
            ])
            .setup(move |app, _api| {
                app.manage(InformationPluginState {
                    host,
                    external_path_grants,
                });
                Ok(())
            })
            .build()
    }
}

/// Construct the plugin from an application-owned information host.
#[must_use]
pub fn init<R: Runtime>(host: Arc<InformationHost>) -> TauriPlugin<R> {
    Builder::new(host).build()
}

struct InformationPluginState {
    host: Arc<InformationHost>,
    external_path_grants: Option<Arc<dyn ExternalPathGrantResolver>>,
}

/// Product-specific authority that exchanges an opaque native picker grant for
/// exactly one already-authorized path. Implementations should consume grants
/// once and bind them to the requesting app/window where practical.
pub trait ExternalPathGrantResolver: Send + Sync + 'static {
    fn consume_grant(&self, grant: &str) -> Result<PathBuf, InformationError>;
}

/// Access to the same host injected into the Tauri plugin.
pub trait InformationNativeExt<R: Runtime> {
    fn information_native(&self) -> Arc<InformationHost>;
}

impl<R: Runtime, T: Manager<R>> InformationNativeExt<R> for T {
    fn information_native(&self) -> Arc<InformationHost> {
        Arc::clone(&self.state::<InformationPluginState>().host)
    }
}

/// Serializable catalogue search input for Tauri IPC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CatalogSearchRequest {
    pub text: String,
    pub text_mode: CatalogTextMode,
    pub kinds: BTreeSet<ResourceKind>,
    pub languages: Vec<String>,
    pub subjects: Vec<String>,
    pub formats: BTreeSet<FormatKind>,
    pub capabilities: BTreeSet<InformationCapability>,
    #[serde(default = "default_catalog_limit")]
    pub limit: usize,
}

impl Default for CatalogSearchRequest {
    fn default() -> Self {
        Self {
            text: String::new(),
            text_mode: CatalogTextMode::default(),
            kinds: BTreeSet::new(),
            languages: Vec::new(),
            subjects: Vec::new(),
            formats: BTreeSet::new(),
            capabilities: BTreeSet::new(),
            limit: default_catalog_limit(),
        }
    }
}

impl From<CatalogSearchRequest> for CatalogSearchQuery {
    fn from(request: CatalogSearchRequest) -> Self {
        Self {
            text: request.text,
            text_mode: request.text_mode.into(),
            kinds: request.kinds,
            languages: request.languages,
            subjects: request.subjects,
            formats: request.formats,
            capabilities: request.capabilities,
            limit: request.limit,
        }
    }
}

const fn default_catalog_limit() -> usize {
    100
}

/// Catalogue text matching semantics exposed over IPC.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogTextMode {
    #[default]
    AllTerms,
    AnyTerm,
}

impl From<CatalogTextMode> for SearchTextMode {
    fn from(mode: CatalogTextMode) -> Self {
        match mode {
            CatalogTextMode::AllTerms => Self::AllTerms,
            CatalogTextMode::AnyTerm => Self::AnyTerm,
        }
    }
}

/// Search input for the local-UI IPC surface. The retrieval purpose is not an
/// IPC field; conversion always assigns [`RetrievalPurpose::LocalUi`]. Model
/// callers must use the `information_query` command under its separate permission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LocalSearchRequest {
    pub schema: String,
    pub query_id: QueryId,
    pub text: String,
    pub syntax: QuerySyntax,
    #[serde(default)]
    pub targets: Vec<RetrievalTarget>,
    #[serde(default)]
    pub resources: Vec<ResourceId>,
    #[serde(default)]
    pub representations: Vec<RepresentationId>,
    pub filters: QueryFilters,
    pub budget: QueryBudget,
}

impl From<LocalSearchRequest> for InformationQuery {
    fn from(request: LocalSearchRequest) -> Self {
        Self {
            schema: request.schema,
            query_id: request.query_id,
            text: request.text,
            syntax: request.syntax,
            purpose: RetrievalPurpose::LocalUi,
            targets: request.targets,
            resources: request.resources,
            representations: request.representations,
            filters: request.filters,
            budget: request.budget,
        }
    }
}

/// Direct-read input for the local-UI IPC surface. Purpose is fixed during
/// conversion and cannot be supplied by the IPC caller.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LocalReadRequest {
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub locator: EvidenceLocator,
    pub max_context_chars: u32,
    pub timeout_ms: u64,
}

impl From<LocalReadRequest> for ReadRequest {
    fn from(request: LocalReadRequest) -> Self {
        Self {
            resource_id: request.resource_id,
            release_id: request.release_id,
            representation_id: request.representation_id,
            purpose: RetrievalPurpose::LocalUi,
            locator: request.locator,
            max_context_chars: request.max_context_chars,
            timeout_ms: request.timeout_ms,
        }
    }
}

/// Record-lookup input for the local-UI IPC surface. Purpose is fixed during
/// conversion and cannot be supplied by the IPC caller.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalLookupRequest {
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub collection: Option<String>,
    pub key: String,
    pub max_context_chars: u32,
    pub timeout_ms: u64,
}

impl From<LocalLookupRequest> for LookupRequest {
    fn from(request: LocalLookupRequest) -> Self {
        Self {
            resource_id: request.resource_id,
            release_id: request.release_id,
            representation_id: request.representation_id,
            purpose: RetrievalPurpose::LocalUi,
            collection: request.collection,
            key: request.key,
            max_context_chars: request.max_context_chars,
            timeout_ms: request.timeout_ms,
        }
    }
}

/// Serializable input for resolving an exact install plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct InstallPlanRequest {
    pub installation_id: InstallationId,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    #[serde(default)]
    pub selection: InstallSelection,
    #[serde(default)]
    pub mirror_choices: BTreeMap<ArtifactId, String>,
    pub available_bytes_observed: Option<u64>,
}

impl From<InstallPlanRequest> for PlanRequest {
    fn from(request: InstallPlanRequest) -> Self {
        Self {
            installation_id: request.installation_id,
            resource_id: request.resource_id,
            release_id: request.release_id,
            representation_id: request.representation_id,
            selection: request.selection,
            mirror_choices: request.mirror_choices,
            available_bytes_observed: request.available_bytes_observed,
            created_at: Utc::now(),
        }
    }
}

/// Serializable input for binding one durable installation into the live
/// retrieval router. This accepts only an installation id already known to the
/// managed store; it does not register an arbitrary filesystem path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MountInstallationRequest {
    pub installation_id: InstallationId,
}

/// Serializable input for registering a caller-authorized external resource.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalRegistrationInput {
    pub installation_id: InstallationId,
    pub resource_id: ResourceId,
    pub release_id: ReleaseId,
    pub representation_id: RepresentationId,
    pub format: RepresentationFormat,
    /// Opaque token issued by an application-owned native file picker.
    pub path_grant: String,
    pub access_mode: ExternalAccessMode,
    pub provenance: Provenance,
    #[serde(default)]
    pub rights: Vec<RightsStatement>,
    #[serde(default)]
    pub use_policy: UsePolicy,
}

impl ExternalRegistrationInput {
    fn into_request(self, absolute_path: PathBuf) -> ExternalRegistrationRequest {
        ExternalRegistrationRequest {
            installation_id: self.installation_id,
            resource_id: self.resource_id,
            release_id: self.release_id,
            representation_id: self.representation_id,
            format: self.format,
            absolute_path,
            access_mode: self.access_mode,
            provenance: self.provenance,
            rights: self.rights,
            use_policy: self.use_policy,
        }
    }
}

/// Ready managed packages and external read-only registrations.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct InstalledResources {
    pub managed: Vec<InstallReceipt>,
    pub external: Vec<ExternalRegistration>,
}

impl From<StoreSnapshot> for InstalledResources {
    fn from(snapshot: StoreSnapshot) -> Self {
        Self {
            managed: snapshot.managed,
            external: snapshot.external,
        }
    }
}

/// Removal target classification. No Tauri command executes this plan.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemovalTargetKind {
    ManagedPackage,
    ExternalRegistrationOnly,
}

impl From<RemovalKind> for RemovalTargetKind {
    fn from(kind: RemovalKind) -> Self {
        match kind {
            RemovalKind::ManagedPackage => Self::ManagedPackage,
            RemovalKind::ExternalRegistrationOnly => Self::ExternalRegistrationOnly,
        }
    }
}

/// Read-only description of what a future, separately authorized removal
/// action would affect.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemovalPlanResponse {
    pub installation_id: InstallationId,
    pub kind: RemovalTargetKind,
    pub managed_relative_paths: Vec<String>,
    pub external_source_preserved: Option<PathBuf>,
    pub observed_package_bytes: u64,
    pub requires_explicit_confirmation: bool,
}

impl From<RemovalPlan> for RemovalPlanResponse {
    fn from(plan: RemovalPlan) -> Self {
        Self {
            installation_id: plan.installation_id,
            kind: plan.kind.into(),
            managed_relative_paths: plan.managed_relative_paths,
            external_source_preserved: plan.external_source_preserved,
            observed_package_bytes: plan.observed_package_bytes,
            requires_explicit_confirmation: plan.requires_explicit_confirmation,
        }
    }
}

#[tauri::command]
async fn information_status(
    state: State<'_, InformationPluginState>,
) -> Result<HostStatus, InformationError> {
    run_host_blocking(Arc::clone(&state.host), "read host status", |host| {
        host.status()
    })
    .await
}

#[tauri::command]
async fn information_catalog_search(
    request: CatalogSearchRequest,
    state: State<'_, InformationPluginState>,
) -> Result<Vec<CatalogHit>, InformationError> {
    let query = CatalogSearchQuery::from(request);
    run_host_blocking(Arc::clone(&state.host), "search catalogue", move |host| {
        host.search_catalog(&query)
    })
    .await
}

#[tauri::command]
async fn information_installed(
    state: State<'_, InformationPluginState>,
) -> Result<InstalledResources, InformationError> {
    run_host_blocking(
        Arc::clone(&state.host),
        "list installed resources",
        |host| host.installed().map(InstalledResources::from),
    )
    .await
}

#[tauri::command]
async fn information_resolve_install_plan(
    request: InstallPlanRequest,
    state: State<'_, InformationPluginState>,
) -> Result<InstallPlan, InformationError> {
    let request = PlanRequest::from(request);
    run_host_blocking(
        Arc::clone(&state.host),
        "resolve install plan",
        move |host| host.resolve_install_plan(request),
    )
    .await
}

/// Execute one of the host's typed, model-facing information tool calls.
#[tauri::command]
async fn information_query(
    call: InformationToolCall,
    state: State<'_, InformationPluginState>,
) -> Result<InformationToolResult, InformationError> {
    run_host_blocking(
        Arc::clone(&state.host),
        "execute information query",
        move |host| host.execute_tool(call),
    )
    .await
}

#[tauri::command]
async fn information_search(
    query: LocalSearchRequest,
    state: State<'_, InformationPluginState>,
) -> Result<EvidenceSet, InformationError> {
    let query = InformationQuery::from(query);
    run_host_blocking(
        Arc::clone(&state.host),
        "search offline resources",
        move |host| host.search(&query),
    )
    .await
}

#[tauri::command]
async fn information_read(
    request: LocalReadRequest,
    state: State<'_, InformationPluginState>,
) -> Result<BackendReadResult, InformationError> {
    let request = ReadRequest::from(request);
    run_host_blocking(
        Arc::clone(&state.host),
        "read offline resource",
        move |host| host.read(&request),
    )
    .await
}

#[tauri::command]
async fn information_lookup(
    request: LocalLookupRequest,
    state: State<'_, InformationPluginState>,
) -> Result<BackendReadResult, InformationError> {
    let request = LookupRequest::from(request);
    run_host_blocking(
        Arc::clone(&state.host),
        "look up offline resource record",
        move |host| host.lookup(&request),
    )
    .await
}

#[tauri::command]
async fn information_register_external(
    request: ExternalRegistrationInput,
    state: State<'_, InformationPluginState>,
) -> Result<ExternalRegistration, InformationError> {
    if request.path_grant.trim().is_empty() || request.path_grant.len() > 512 {
        return Err(InformationError::new(
            ErrorClass::InvalidInput,
            "information_path_grant_invalid",
            "external path grant must contain between 1 and 512 bytes",
        ));
    }
    let resolver = state.external_path_grants.clone().ok_or_else(|| {
        InformationError::new(
            ErrorClass::Permission,
            "information_path_grants_unavailable",
            "the application did not configure native external-path grants",
        )
    })?;
    let grant = request.path_grant.clone();
    run_host_blocking(
        Arc::clone(&state.host),
        "register external resource",
        move |host| {
            let path = resolver.consume_grant(&grant)?;
            host.register_external(&request.into_request(path))
        },
    )
    .await
}

#[tauri::command]
async fn information_install(
    plan: InstallPlan,
    state: State<'_, InformationPluginState>,
) -> Result<InstallReceipt, InformationError> {
    run_host_blocking(
        Arc::clone(&state.host),
        "install information resource",
        move |host| host.install(&plan),
    )
    .await
}

#[tauri::command]
async fn information_mount_installation(
    request: MountInstallationRequest,
    state: State<'_, InformationPluginState>,
) -> Result<BackendDescriptor, InformationError> {
    let MountInstallationRequest { installation_id } = request;
    run_host_blocking(
        Arc::clone(&state.host),
        "mount information resource",
        move |host| host.mount_installation(&installation_id, MountInstallationOptions::default()),
    )
    .await
}

#[tauri::command]
async fn information_plan_removal(
    installation_id: InstallationId,
    state: State<'_, InformationPluginState>,
) -> Result<RemovalPlanResponse, InformationError> {
    run_host_blocking(
        Arc::clone(&state.host),
        "plan information resource removal",
        move |host| {
            host.plan_removal(&installation_id)
                .map(RemovalPlanResponse::from)
        },
    )
    .await
}

async fn run_host_blocking<T, F>(
    host: Arc<InformationHost>,
    operation: &'static str,
    task: F,
) -> Result<T, InformationError>
where
    T: Send + 'static,
    F: FnOnce(&InformationHost) -> Result<T, HostError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || task(&host))
        .await
        .map_err(|_join_error| worker_failed(operation))?
        .map_err(map_host_error)
}

fn map_host_error(error: HostError) -> InformationError {
    error.as_information_error()
}

fn worker_failed(operation: &str) -> InformationError {
    InformationError::new(
        ErrorClass::Internal,
        "information_worker_failed",
        format!("{operation} worker did not complete"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn catalogue_request_preserves_filters_and_uses_bounded_default() {
        let request = CatalogSearchRequest {
            text: "local wisdom".to_string(),
            kinds: BTreeSet::from([ResourceKind::TextCorpus]),
            capabilities: BTreeSet::from([InformationCapability::LexicalSearch]),
            ..CatalogSearchRequest::default()
        };
        let query = CatalogSearchQuery::from(request);

        assert_eq!(query.text, "local wisdom");
        assert_eq!(query.text_mode, SearchTextMode::AllTerms);
        assert_eq!(query.limit, 100);
        assert!(query.kinds.contains(&ResourceKind::TextCorpus));
        assert!(
            query
                .capabilities
                .contains(&InformationCapability::LexicalSearch)
        );
    }

    #[test]
    fn catalogue_json_uses_the_same_bounded_default() -> TestResult {
        let request: CatalogSearchRequest = serde_json::from_str(r#"{"text":"wisdom"}"#)?;
        assert_eq!(request.limit, 100);
        assert_eq!(request.text_mode, CatalogTextMode::AllTerms);
        Ok(())
    }

    #[test]
    fn host_errors_cross_ipc_as_information_errors() -> TestResult {
        let error = map_host_error(HostError::BackendRegistryUnavailable);
        assert_eq!(
            error.code,
            "information_backend_registry_unavailable".to_string()
        );
        assert_eq!(error.class, ErrorClass::ResourceBusy);
        assert!(error.retryable);
        let encoded = serde_json::to_value(error)?;
        assert_eq!(encoded["class"], "resource_busy");
        Ok(())
    }

    #[test]
    fn worker_failures_do_not_expose_join_details() {
        let error = worker_failed("search offline resources");
        assert_eq!(error.code, "information_worker_failed".to_string());
        assert_eq!(error.class, ErrorClass::Internal);
        assert_eq!(
            error.safe_message,
            "search offline resources worker did not complete".to_string()
        );
    }

    #[test]
    fn local_ipc_requests_cannot_select_model_context() -> TestResult {
        let resource_id = ResourceId::parse("local.test-resource")?;
        let release_id = ReleaseId::parse("observed")?;
        let representation_id = RepresentationId::parse("sqlite")?;
        let target = RetrievalTarget {
            resource_id: resource_id.clone(),
            release_id: release_id.clone(),
            representation_id: representation_id.clone(),
        };
        let search_request = LocalSearchRequest {
            schema: information_native_types::QUERY_SCHEMA.to_string(),
            query_id: QueryId::new(),
            text: "attention".to_string(),
            syntax: QuerySyntax::NaturalTerms,
            targets: vec![target],
            resources: vec![resource_id.clone()],
            representations: vec![representation_id.clone()],
            filters: QueryFilters::default(),
            budget: QueryBudget::default(),
        };
        let read_request = LocalReadRequest {
            resource_id: resource_id.clone(),
            release_id: release_id.clone(),
            representation_id: representation_id.clone(),
            locator: EvidenceLocator::Record {
                collection: Some("records".to_string()),
                key: "1".to_string(),
            },
            max_context_chars: 1_000,
            timeout_ms: 5_000,
        };
        let lookup_request = LocalLookupRequest {
            resource_id,
            release_id,
            representation_id,
            collection: Some("records".to_string()),
            key: "1".to_string(),
            max_context_chars: 1_000,
            timeout_ms: 5_000,
        };

        let search = InformationQuery::from(search_request.clone());
        let read = ReadRequest::from(read_request.clone());
        let lookup = LookupRequest::from(lookup_request.clone());
        assert_eq!(search.purpose, RetrievalPurpose::LocalUi);
        assert_eq!(read.purpose, RetrievalPurpose::LocalUi);
        assert_eq!(lookup.purpose, RetrievalPurpose::LocalUi);

        let mut encoded_search = serde_json::to_value(search_request)?;
        let mut encoded_read = serde_json::to_value(read_request)?;
        let mut encoded_lookup = serde_json::to_value(lookup_request)?;
        assert!(encoded_search.get("purpose").is_none());
        assert!(encoded_read.get("purpose").is_none());
        assert!(encoded_lookup.get("purpose").is_none());

        encoded_search["purpose"] = serde_json::json!("model_context");
        encoded_read["purpose"] = serde_json::json!("model_context");
        encoded_lookup["purpose"] = serde_json::json!("model_context");
        assert!(serde_json::from_value::<LocalSearchRequest>(encoded_search).is_err());
        assert!(serde_json::from_value::<LocalReadRequest>(encoded_read).is_err());
        assert!(serde_json::from_value::<LocalLookupRequest>(encoded_lookup).is_err());
        Ok(())
    }

    #[test]
    fn local_and_model_permission_sets_are_disjoint() {
        const DEFAULT_PERMISSION: &str = include_str!("../permissions/default.toml");
        const GENERATED_REFERENCE: &str = include_str!("../permissions/autogenerated/reference.md");
        const LOCAL_UI_PERMISSION: &str = include_str!("../permissions/local-ui-query.toml");
        const MODEL_PERMISSION: &str = include_str!("../permissions/model-query.toml");

        assert!(DEFAULT_PERMISSION.contains("allow-information-status"));
        assert_eq!(DEFAULT_PERMISSION.matches("allow-information-").count(), 1);

        for permission in [
            "allow-information-search",
            "allow-information-read",
            "allow-information-lookup",
        ] {
            assert!(LOCAL_UI_PERMISSION.contains(permission));
            assert!(!MODEL_PERMISSION.contains(permission));
        }
        assert!(!LOCAL_UI_PERMISSION.contains("allow-information-query"));
        assert!(MODEL_PERMISSION.contains("allow-information-query"));
        assert!(!LOCAL_UI_PERMISSION.contains("allow-information-installed"));
        assert!(!MODEL_PERMISSION.contains("allow-information-installed"));
        assert!(GENERATED_REFERENCE.contains("`information-native:local-ui-query`"));
        assert!(GENERATED_REFERENCE.contains("`information-native:model-query`"));
        assert!(!GENERATED_REFERENCE.contains("`information-native:query`"));
    }

    #[test]
    fn mount_request_defaults_to_private_model_context_denied() -> TestResult {
        let request: MountInstallationRequest = serde_json::from_value(serde_json::json!({
            "installation_id": "community-archive"
        }))?;

        assert_eq!(
            request.installation_id,
            InstallationId::parse("community-archive")?
        );

        let mut encoded = serde_json::to_value(request)?;
        encoded["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<MountInstallationRequest>(encoded).is_err());

        assert!(
            serde_json::from_value::<MountInstallationRequest>(serde_json::json!({
                "installation_id": "community-archive",
                "options": { "allow_private_model_context": true }
            }))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn mount_permission_is_narrow_and_standalone() {
        const ACQUIRE_PERMISSION: &str = include_str!("../permissions/acquire.toml");
        const DEFAULT_PERMISSION: &str = include_str!("../permissions/default.toml");
        const GENERATED_REFERENCE: &str = include_str!("../permissions/autogenerated/reference.md");
        const LOCAL_UI_PERMISSION: &str = include_str!("../permissions/local-ui-query.toml");
        const MODEL_PERMISSION: &str = include_str!("../permissions/model-query.toml");
        const MOUNT_PERMISSION: &str = include_str!("../permissions/mount.toml");

        const COMMAND_PERMISSION: &str = "allow-information-mount-installation";
        assert!(MOUNT_PERMISSION.contains(COMMAND_PERMISSION));
        assert_eq!(MOUNT_PERMISSION.matches("allow-information-").count(), 1);
        for broader_permission in [
            ACQUIRE_PERMISSION,
            DEFAULT_PERMISSION,
            LOCAL_UI_PERMISSION,
            MODEL_PERMISSION,
        ] {
            assert!(!broader_permission.contains(COMMAND_PERMISSION));
        }
        assert!(GENERATED_REFERENCE.contains("`information-native:mount`"));
        assert!(
            GENERATED_REFERENCE
                .contains("`information-native:allow-information-mount-installation`")
        );
    }

    #[test]
    fn external_registration_accepts_only_opaque_native_path_grants() -> TestResult {
        let value = serde_json::json!({
            "installation_id": "external-fixture",
            "resource_id": "local.fixture",
            "release_id": "observed",
            "representation_id": "sqlite",
            "format": {
                "kind": "sqlite_fts5",
                "profile": "alexandria.blocks.v1",
                "media_type": "application/vnd.sqlite3"
            },
            "path_grant": "picker-grant:one-time-token",
            "access_mode": "live_read_only",
            "provenance": {
                "publisher": "Fixture",
                "source_uri": "fixture://picker",
                "upstream_record_id": null,
                "source_inputs": [],
                "transformation": null,
                "metadata": {}
            }
        });
        let request: ExternalRegistrationInput = serde_json::from_value(value.clone())?;
        assert_eq!(request.path_grant, "picker-grant:one-time-token");
        let encoded = serde_json::to_value(request)?;
        assert!(encoded.get("absolute_path").is_none());

        let mut raw_path = value;
        raw_path
            .as_object_mut()
            .ok_or_else(|| std::io::Error::other("fixture was not an object"))?
            .remove("path_grant");
        raw_path["absolute_path"] = serde_json::json!("/etc/passwd");
        assert!(serde_json::from_value::<ExternalRegistrationInput>(raw_path).is_err());
        Ok(())
    }
}
