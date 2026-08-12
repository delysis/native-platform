//! Sole modern Gateway assembly and read-only desktop projection.

use crate::catalog::{Capability, ModelCatalogEntry, PromptSemantics, default_model_catalog};
use crate::db::{Database, LocalModelConfiguration, ProviderLogSummary};
use crate::secrets::{CredentialStore, OsCredentialStore};
use fte_backend_llama::{BACKEND_ID as NATIVE_BACKEND_ID, LlamaNativeBackend};
use fte_protocols::{
    EdgeDefaults, OpenAiChatRequest, OpenAiCompletionRequest, openai_chat_json,
    openai_completion_json,
};
use fte_providers::{HostedProviderBackend, HostedProviderConfig};
use fte_router::{Gateway, GatewayDefaults, GatewayShutdownReport};
use fte_store::SecretResolver;
use fte_types::{
    BackendLocation, BackendReadiness, BackendSnapshot, GatewayError, Modality, ModelCapabilities,
    ModelDescriptor, ModelSelector, PrivacyPolicy, PromptForm, RequestId, RouteObservations,
    RouteProfile,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

const LOCAL_MODEL_ID: &str = "local/default";
const LOCAL_MODEL_DISPLAY_NAME: &str = "Local GGUF";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PublicModel {
    pub id: String,
    pub display_name: String,
    pub providers: Vec<String>,
    pub supports_chat_completions: bool,
    pub supports_text_completions: bool,
    pub prompt_semantics: Vec<PromptSemantics>,
}

#[derive(Default)]
struct PublicModelAccumulator {
    display_name: String,
    providers: BTreeSet<String>,
    supports_chat_completions: bool,
    supports_text_completions: bool,
    prompt_semantics: BTreeSet<PromptSemantics>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProviderModelStatus {
    pub public_model_id: String,
    pub provider_model_id: String,
    pub display_name: String,
    pub headroom: Option<f64>,
    pub supports_chat_completions: bool,
    pub supports_text_completions: bool,
    pub prompt_semantics: Option<PromptSemantics>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProviderStatus {
    pub id: String,
    pub name: String,
    pub backend_kind: ProviderBackendKind,
    pub credential_required: bool,
    pub configured: bool,
    pub status: String,
    pub model_count: usize,
    pub text_completion_model_count: usize,
    pub headroom: Option<f64>,
    pub total_tokens: u64,
    pub avg_latency_ms: u64,
    pub request_count: u64,
    pub last_request_at: Option<String>,
    pub last_status_code: Option<i32>,
    pub models: Vec<ProviderModelStatus>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderBackendKind {
    RemoteApi,
    LocalEmbedded,
    LocalService,
    Unknown,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalModelStatus {
    pub state: LocalModelState,
    pub display_name: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalModelState {
    NotConfigured,
    Ready,
    Invalid,
}

impl LocalModelStatus {
    fn not_configured() -> Self {
        Self {
            state: LocalModelState::NotConfigured,
            display_name: None,
            detail: "Choose a local GGUF file to enable private on-device inference.".to_string(),
        }
    }

    fn ready(path: &Path) -> Self {
        Self {
            state: LocalModelState::Ready,
            display_name: model_file_name(path),
            detail: "The model selection is saved locally and restored at startup.".to_string(),
        }
    }

    fn invalid(path: &Path, error: &anyhow::Error) -> Self {
        Self {
            state: LocalModelState::Invalid,
            display_name: model_file_name(path),
            detail: format!("The saved model cannot be used: {error}"),
        }
    }
}

pub struct StoreSecretResolver {
    store: Arc<dyn CredentialStore>,
}

impl StoreSecretResolver {
    fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self { store }
    }
}

impl SecretResolver for StoreSecretResolver {
    fn resolve(&self, provider: &str) -> Result<Option<String>, GatewayError> {
        self.store
            .read(provider)
            .map_err(|error| GatewayError {
                code: "secret_resolver_read_failed".to_string(),
                class: fte_types::ErrorClass::Internal,
                retryable: true,
                http_status: 503,
                request_id: RequestId::new(),
                provider: Some(provider.to_string()),
                safe_detail: format!(
                    "the saved credential for {provider} could not be read: {error}"
                ),
            })?
            .map(|secret| {
                String::from_utf8(secret).map_err(|_| GatewayError {
                    code: "secret_resolver_encoding_invalid".to_string(),
                    class: fte_types::ErrorClass::Internal,
                    retryable: false,
                    http_status: 500,
                    request_id: RequestId::new(),
                    provider: Some(provider.to_string()),
                    safe_detail: format!("the saved credential for {provider} is not UTF-8"),
                })
            })
            .transpose()
    }
}

pub struct GatewayRuntimeOwner {
    gateway: Arc<Gateway>,
    native_host: Arc<llama_native_host::NativeHost>,
    native_backend: Arc<LlamaNativeBackend>,
    native_shutdown: Mutex<Option<llama_native_host::ProcessExitJoinedNativeHost>>,
    credential_store: Arc<dyn CredentialStore>,
    database: RwLock<Option<Arc<Database>>>,
    local_model_configuration_lock: Mutex<()>,
    local_model_status: RwLock<LocalModelStatus>,
    catalog: Vec<ModelCatalogEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayRuntimeShutdownReport {
    pub gateway: GatewayShutdownReport,
    pub native_host_joined: bool,
}

impl GatewayRuntimeOwner {
    pub fn new() -> Result<Self, GatewayError> {
        Self::new_with_store(Arc::new(OsCredentialStore::new()))
    }

    pub(crate) fn new_with_store(
        credential_store: Arc<dyn CredentialStore>,
    ) -> Result<Self, GatewayError> {
        let gateway = Arc::new(Gateway::new(GatewayDefaults {
            catalog_version: "free-token-energy-desktop-v2".to_string(),
        }));
        let secrets: Arc<dyn SecretResolver> =
            Arc::new(StoreSecretResolver::new(Arc::clone(&credential_store)));
        let catalog = default_model_catalog();
        register_hosted_backends(&gateway, secrets, &catalog)?;
        let native_host = Arc::new(llama_native_host::NativeHost::new(
            llama_native_host::NativeHostConfig::default(),
        ));
        let native_backend = Arc::new(LlamaNativeBackend::new_borrowed(Arc::clone(&native_host)));
        gateway.register_backend(native_backend.clone())?;
        Ok(Self {
            gateway,
            native_host,
            native_backend,
            native_shutdown: Mutex::new(None),
            credential_store,
            database: RwLock::new(None),
            local_model_configuration_lock: Mutex::new(()),
            local_model_status: RwLock::new(LocalModelStatus::not_configured()),
            catalog,
        })
    }

    pub fn gateway(&self) -> Arc<Gateway> {
        Arc::clone(&self.gateway)
    }

    pub fn configure_local_model(
        &self,
        model_path: impl AsRef<Path>,
        expected_sha256: Option<String>,
    ) -> anyhow::Result<String> {
        let _configuration_guard = self
            .local_model_configuration_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let database = self.database()?;
        let (model_path, native_config, saved_configuration) =
            local_model_configuration(model_path.as_ref(), expected_sha256)?;
        let previous_configuration = database.get_local_model_configuration()?;
        database.save_local_model_configuration(&saved_configuration)?;
        if let Err(error) = self.native_backend.configure_model(native_config) {
            let rollback = match previous_configuration {
                Some(ref previous) => database.save_local_model_configuration(previous),
                None => database.delete_local_model_configuration(),
            };
            if let Err(rollback_error) = rollback {
                self.set_local_model_status(LocalModelStatus::invalid(
                    &model_path,
                    &anyhow::anyhow!(
                        "configuration failed ({error}); database rollback also failed ({rollback_error})"
                    ),
                ))?;
                anyhow::bail!(
                    "local model configuration failed: {error}; database rollback failed: {rollback_error}"
                );
            }
            return Err(error.into());
        }
        self.set_local_model_status(LocalModelStatus::ready(&model_path))?;
        Ok(LOCAL_MODEL_ID.to_string())
    }

    pub fn restore_local_model_configuration(&self) -> anyhow::Result<LocalModelStatus> {
        let _configuration_guard = self
            .local_model_configuration_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let database = self.database()?;
        let Some(saved) = database.get_local_model_configuration()? else {
            let status = LocalModelStatus::not_configured();
            self.set_local_model_status(status.clone())?;
            return Ok(status);
        };
        let saved_path = PathBuf::from(&saved.model_path);
        match local_model_configuration(&saved_path, saved.expected_sha256) {
            Ok((canonical_path, native_config, _)) => {
                if let Err(error) = self.native_backend.configure_model(native_config) {
                    let error = anyhow::Error::from(error);
                    let status = LocalModelStatus::invalid(&saved_path, &error);
                    self.set_local_model_status(status.clone())?;
                    return Ok(status);
                }
                let status = LocalModelStatus::ready(&canonical_path);
                self.set_local_model_status(status.clone())?;
                Ok(status)
            }
            Err(error) => {
                let status = LocalModelStatus::invalid(&saved_path, &error);
                self.set_local_model_status(status.clone())?;
                Ok(status)
            }
        }
    }

    pub fn local_model_status(&self) -> anyhow::Result<LocalModelStatus> {
        self.local_model_status
            .read()
            .map_err(|_| anyhow::anyhow!("the local model status is unavailable"))
            .map(|status| status.clone())
    }

    fn set_local_model_status(&self, status: LocalModelStatus) -> anyhow::Result<()> {
        *self
            .local_model_status
            .write()
            .map_err(|_| anyhow::anyhow!("the local model status is unavailable"))? = status;
        Ok(())
    }

    #[must_use]
    pub fn shutdown_native_for_process_exit(&self) -> bool {
        let mut shutdown = self
            .native_shutdown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if shutdown.is_none() {
            *shutdown = Some(self.native_host.shutdown_for_process_exit());
        }
        shutdown
            .as_ref()
            .is_some_and(|fact| fact.belongs_to(&self.native_host))
    }

    /// Drains the Gateway before joining the application-owned native host.
    /// The returned report is suitable for process-exit diagnostics and
    /// acceptance evidence; callers do not need access to either owner.
    pub async fn shutdown_with_report(&self) -> GatewayRuntimeShutdownReport {
        let gateway = self.gateway.shutdown_with_report().await;
        let native_host_joined = self.shutdown_native_for_process_exit();
        GatewayRuntimeShutdownReport {
            gateway,
            native_host_joined,
        }
    }

    pub fn bind_database(&self, database: Arc<Database>) -> Result<(), GatewayError> {
        let mut current = self.database.write().map_err(|_| {
            GatewayError::unavailable(
                &RequestId::new(),
                "database_binding_failed",
                "the desktop metadata database binding is unavailable",
            )
        })?;
        if current.is_some() {
            return Err(GatewayError::invalid_request(
                &RequestId::new(),
                "database_already_bound",
                "the desktop metadata database was already initialized",
            ));
        }
        *current = Some(database);
        Ok(())
    }

    pub fn credential_store(&self) -> Arc<dyn CredentialStore> {
        Arc::clone(&self.credential_store)
    }

    fn database(&self) -> anyhow::Result<Arc<Database>> {
        self.database
            .read()
            .map_err(|_| anyhow::anyhow!("the desktop database binding is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the desktop database is not bound"))
    }

    #[must_use]
    pub fn supports_provider(&self, provider_id: &str) -> bool {
        self.catalog
            .iter()
            .any(|entry| entry.provider_id == provider_id)
    }

    pub fn save_credential(&self, provider_id: &str, secret: &str) -> anyhow::Result<()> {
        self.credential_store
            .write(provider_id, secret.as_bytes())?;
        let stored = self.credential_store.read(provider_id)?;
        anyhow::ensure!(
            stored.as_deref() == Some(secret.as_bytes()),
            "OS credential readback did not exactly match the requested value"
        );
        Ok(())
    }

    pub fn delete_credential(&self, provider_id: &str) -> anyhow::Result<bool> {
        self.credential_store
            .delete(provider_id)
            .map_err(Into::into)
    }

    pub async fn chat(&self, request: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let (requested_model, mut canonical) = canonical_chat_request(request, &self.catalog)?;
        self.apply_route_policy(&requested_model, &mut canonical);
        let started = std::time::Instant::now();
        let result = self.gateway.execute(canonical).await;
        let response = match result {
            Ok(ticket) => ticket.final_response().await,
            Err(error) => Err(error),
        };
        match response {
            Ok(response) => {
                self.log_gateway_result(
                    &response.route.backend_id,
                    &requested_model,
                    response
                        .usage
                        .input_tokens
                        .unwrap_or_default()
                        .saturating_add(response.usage.output_tokens.unwrap_or_default()),
                    started.elapsed(),
                    200,
                );
                let mut json = openai_chat_json(&response);
                json["model"] = serde_json::Value::String(requested_model);
                Ok(json)
            }
            Err(error) => {
                self.log_gateway_result(
                    error.provider.as_deref().unwrap_or("gateway"),
                    &requested_model,
                    0,
                    started.elapsed(),
                    i32::from(error.http_status),
                );
                Err(error.into())
            }
        }
    }

    pub async fn complete(&self, request: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let (requested_model, mut canonical) = canonical_completion_request(request)?;
        self.apply_route_policy(&requested_model, &mut canonical);
        let started = std::time::Instant::now();
        let result = self.gateway.execute(canonical).await;
        let response = match result {
            Ok(ticket) => ticket.final_response().await,
            Err(error) => Err(error),
        };
        match response {
            Ok(response) => {
                self.log_gateway_result(
                    &response.route.backend_id,
                    &requested_model,
                    response
                        .usage
                        .input_tokens
                        .unwrap_or_default()
                        .saturating_add(response.usage.output_tokens.unwrap_or_default()),
                    started.elapsed(),
                    200,
                );
                let mut json = openai_completion_json(&response);
                json["model"] = serde_json::Value::String(requested_model);
                Ok(json)
            }
            Err(error) => {
                self.log_gateway_result(
                    error.provider.as_deref().unwrap_or("gateway"),
                    &requested_model,
                    0,
                    started.elapsed(),
                    i32::from(error.http_status),
                );
                Err(error.into())
            }
        }
    }

    fn log_gateway_result(
        &self,
        provider_id: &str,
        model_id: &str,
        tokens: u64,
        elapsed: std::time::Duration,
        status: i32,
    ) {
        if let Ok(database) = self.database() {
            let _ = database.log_request(
                provider_id,
                model_id,
                u32::try_from(tokens).unwrap_or(u32::MAX),
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                status,
            );
        }
    }

    fn apply_route_policy(&self, requested_model: &str, request: &mut fte_types::GatewayRequest) {
        if self
            .gateway
            .models()
            .iter()
            .any(|model| model.backend_id == NATIVE_BACKEND_ID && model.id == requested_model)
        {
            request.routing.privacy = PrivacyPolicy::LocalOnly;
            request.routing.profile = RouteProfile::LocalOnly;
        }
    }

    #[must_use]
    pub fn public_models(&self) -> Vec<PublicModel> {
        let mut models = public_models_from_catalog(&self.catalog);
        models.extend(
            self.gateway
                .models()
                .into_iter()
                .filter(|model| model.backend_id == NATIVE_BACKEND_ID)
                .map(|model| PublicModel {
                    id: model.id,
                    display_name: LOCAL_MODEL_DISPLAY_NAME.to_string(),
                    providers: vec!["Local llama.cpp".to_string()],
                    supports_chat_completions: model
                        .capabilities
                        .prompt_forms
                        .contains(&PromptForm::Chat),
                    supports_text_completions: model
                        .capabilities
                        .prompt_forms
                        .contains(&PromptForm::Completion),
                    prompt_semantics: model
                        .capabilities
                        .prompt_forms
                        .contains(&PromptForm::Completion)
                        .then_some(PromptSemantics::DirectContinuation)
                        .into_iter()
                        .collect(),
                }),
        );
        models
    }

    pub fn provider_statuses(&self) -> anyhow::Result<Vec<ProviderStatus>> {
        let database = self.database()?;
        let summaries = database.get_provider_log_summaries()?;
        let snapshots = self
            .gateway
            .backend_snapshots()
            .into_iter()
            .map(|snapshot| (snapshot.descriptor.id.clone(), snapshot))
            .collect::<BTreeMap<_, _>>();
        let mut by_provider: BTreeMap<String, (String, Vec<&ModelCatalogEntry>)> = BTreeMap::new();
        for entry in &self.catalog {
            by_provider
                .entry(entry.provider_id.clone())
                .or_insert_with(|| (entry.provider_name.clone(), Vec::new()))
                .1
                .push(entry);
        }

        let mut statuses = by_provider
            .into_iter()
            .map(|(provider_id, (provider_name, entries))| {
                let snapshot = snapshots.get(&provider_id);
                let readiness = snapshot
                    .map(|item| item.readiness.clone())
                    .unwrap_or_else(|| BackendReadiness::Unavailable {
                        reason: "the provider backend is not registered".to_string(),
                    });
                let summary = summaries.get(&provider_id).cloned().unwrap_or_default();
                let models = provider_models(&provider_id, &entries, snapshot)?;
                let headroom = models
                    .iter()
                    .filter_map(|model| model.headroom)
                    .reduce(f64::max);
                let configured = snapshot.is_some()
                    && !matches!(readiness, BackendReadiness::NotConfigured { .. });
                Ok(ProviderStatus {
                    id: provider_id,
                    name: provider_name,
                    backend_kind: snapshot.map_or(ProviderBackendKind::Unknown, |item| {
                        backend_kind(item.descriptor.location)
                    }),
                    credential_required: snapshot
                        .is_some_and(|item| item.descriptor.location == BackendLocation::Hosted),
                    configured,
                    status: provider_status(&readiness, headroom, &summary).to_string(),
                    model_count: models.len(),
                    text_completion_model_count: models
                        .iter()
                        .filter(|model| model.supports_text_completions)
                        .count(),
                    headroom,
                    total_tokens: summary.total_tokens,
                    avg_latency_ms: summary.avg_latency_ms,
                    request_count: summary.request_count,
                    last_request_at: summary.last_request_at,
                    last_status_code: summary.last_status_code,
                    models,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        if let Some(snapshot) = snapshots.get(NATIVE_BACKEND_ID) {
            let summary = summaries
                .get(NATIVE_BACKEND_ID)
                .cloned()
                .unwrap_or_default();
            let models = snapshot
                .descriptor
                .models
                .iter()
                .map(|model| ProviderModelStatus {
                    public_model_id: model.id.clone(),
                    provider_model_id: model.id.clone(),
                    display_name: LOCAL_MODEL_DISPLAY_NAME.to_string(),
                    headroom: None,
                    supports_chat_completions: model
                        .capabilities
                        .prompt_forms
                        .contains(&PromptForm::Chat),
                    supports_text_completions: model
                        .capabilities
                        .prompt_forms
                        .contains(&PromptForm::Completion),
                    prompt_semantics: model
                        .capabilities
                        .prompt_forms
                        .contains(&PromptForm::Completion)
                        .then_some(PromptSemantics::DirectContinuation),
                })
                .collect::<Vec<_>>();
            statuses.push(ProviderStatus {
                id: NATIVE_BACKEND_ID.to_string(),
                name: "Local llama.cpp".to_string(),
                backend_kind: ProviderBackendKind::LocalEmbedded,
                credential_required: false,
                configured: !models.is_empty(),
                status: match snapshot.readiness {
                    BackendReadiness::NotConfigured { .. } => "needs_model",
                    _ => provider_status(&snapshot.readiness, None, &summary),
                }
                .to_string(),
                model_count: models.len(),
                text_completion_model_count: models
                    .iter()
                    .filter(|model| model.supports_text_completions)
                    .count(),
                headroom: None,
                total_tokens: summary.total_tokens,
                avg_latency_ms: summary.avg_latency_ms,
                request_count: summary.request_count,
                last_request_at: summary.last_request_at,
                last_status_code: summary.last_status_code,
                models,
            });
        }
        Ok(statuses)
    }

    pub fn global_headroom_percent(&self) -> anyhow::Result<f64> {
        Ok(self
            .provider_statuses()?
            .into_iter()
            .filter_map(|provider| provider.headroom)
            .reduce(f64::max)
            .unwrap_or(0.0)
            * 100.0)
    }
}

fn hosted_defaults() -> EdgeDefaults {
    EdgeDefaults {
        privacy: PrivacyPolicy::HostedOnly,
        profile: RouteProfile::HostedOnly,
    }
}

fn validated_gguf_path(path: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(path.is_absolute(), "local model path must be absolute");
    anyhow::ensure!(path.is_file(), "local model path must name a regular file");
    anyhow::ensure!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf")),
        "local model path must have a .gguf extension"
    );
    let canonical = path.canonicalize()?;
    let mut header = [0_u8; 4];
    std::fs::File::open(&canonical)?.read_exact(&mut header)?;
    anyhow::ensure!(
        &header == b"GGUF",
        "local model file does not have a GGUF header"
    );
    Ok(canonical)
}

fn local_model_configuration(
    path: &Path,
    expected_sha256: Option<String>,
) -> anyhow::Result<(
    PathBuf,
    llama_native_types::NativeModelConfig,
    LocalModelConfiguration,
)> {
    let model_path = validated_gguf_path(path)?;
    if let Some(digest) = expected_sha256.as_deref() {
        anyhow::ensure!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "expected_sha256 must be 64 lowercase hexadecimal characters"
        );
    }
    let persisted_path = model_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("local model path must be valid UTF-8"))?
        .to_string();
    let mut native_config = llama_native_types::NativeModelConfig::local(model_path.clone());
    native_config.model_id = LOCAL_MODEL_ID.to_string();
    native_config.expected_model_sha256 = expected_sha256.clone();
    Ok((
        model_path,
        native_config,
        LocalModelConfiguration {
            model_path: persisted_path,
            expected_sha256,
        },
    ))
}

fn model_file_name(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToString::to_string)
}

fn canonical_chat_request(
    mut request: serde_json::Value,
    catalog: &[ModelCatalogEntry],
) -> anyhow::Result<(String, fte_types::GatewayRequest)> {
    let object = request
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("desktop chat request must be a JSON object"))?;
    anyhow::ensure!(
        !object
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        "desktop streaming must use the Gateway event API"
    );
    let requested_model = required_model(object)?;
    let provider_fields = DesktopProviderFields::take(object)?;
    let provider = if provider_fields.is_empty() {
        None
    } else {
        Some(exact_catalog_provider(&requested_model, catalog)?)
    };
    let mut canonical =
        serde_json::from_value::<OpenAiChatRequest>(request)?.into_gateway(hosted_defaults())?;
    provider_fields.apply(provider, &mut canonical)?;
    canonical.model = desktop_model_selector(&requested_model);
    Ok((requested_model, canonical))
}

#[derive(Default)]
struct DesktopProviderFields {
    top_k: Option<i32>,
    anthropic_thinking: Option<serde_json::Map<String, serde_json::Value>>,
    anthropic_metadata: Option<serde_json::Map<String, serde_json::Value>>,
    anthropic_beta: Option<String>,
    gemini_thinking_config: Option<serde_json::Map<String, serde_json::Value>>,
    gemini_tool_config: Option<serde_json::Map<String, serde_json::Value>>,
    gemini_safety_settings: Option<Vec<serde_json::Value>>,
    gemini_cached_content: Option<String>,
}

impl DesktopProviderFields {
    fn take(extra: &mut serde_json::Map<String, serde_json::Value>) -> anyhow::Result<Self> {
        Ok(Self {
            top_k: take_typed(extra, "top_k")?,
            anthropic_thinking: take_typed(extra, "thinking")?,
            anthropic_metadata: take_typed(extra, "metadata")?,
            anthropic_beta: take_typed(extra, "anthropic_beta")?,
            gemini_thinking_config: take_typed(extra, "thinking_config")?,
            gemini_tool_config: take_typed(extra, "tool_config")?,
            gemini_safety_settings: take_typed(extra, "safety_settings")?,
            gemini_cached_content: take_typed(extra, "cached_content")?,
        })
    }

    fn is_empty(&self) -> bool {
        self.top_k.is_none()
            && self.anthropic_thinking.is_none()
            && self.anthropic_metadata.is_none()
            && self.anthropic_beta.is_none()
            && self.gemini_thinking_config.is_none()
            && self.gemini_tool_config.is_none()
            && self.gemini_safety_settings.is_none()
            && self.gemini_cached_content.is_none()
    }

    fn apply(
        self,
        provider: Option<&str>,
        request: &mut fte_types::GatewayRequest,
    ) -> anyhow::Result<()> {
        let provider = provider.unwrap_or_default();
        let has_anthropic = self.anthropic_thinking.is_some()
            || self.anthropic_metadata.is_some()
            || self.anthropic_beta.is_some();
        let has_gemini = self.gemini_thinking_config.is_some()
            || self.gemini_tool_config.is_some()
            || self.gemini_safety_settings.is_some()
            || self.gemini_cached_content.is_some();
        anyhow::ensure!(
            !(has_anthropic && has_gemini),
            "Anthropic and Gemini provider fields cannot be mixed in one request"
        );
        if has_anthropic {
            anyhow::ensure!(
                provider == "anthropic",
                "Anthropic fields require an exact Anthropic model"
            );
        }
        if has_gemini {
            anyhow::ensure!(
                provider == "gemini",
                "Gemini fields require an exact Gemini model"
            );
        }
        if self.top_k.is_some() {
            anyhow::ensure!(
                matches!(provider, "anthropic" | "gemini"),
                "top_k requires an exact Anthropic or Gemini model"
            );
        }
        request.sampling.top_k = self.top_k;
        insert_extension(
            request,
            "anthropic.thinking",
            self.anthropic_thinking.map(serde_json::Value::Object),
        );
        insert_extension(
            request,
            "anthropic.metadata",
            self.anthropic_metadata.map(serde_json::Value::Object),
        );
        insert_extension(
            request,
            "anthropic.beta",
            self.anthropic_beta.map(serde_json::Value::String),
        );
        insert_extension(
            request,
            "gemini.thinkingConfig",
            self.gemini_thinking_config.map(serde_json::Value::Object),
        );
        insert_extension(
            request,
            "gemini.toolConfig",
            self.gemini_tool_config.map(serde_json::Value::Object),
        );
        insert_extension(
            request,
            "gemini.safetySettings",
            self.gemini_safety_settings.map(serde_json::Value::Array),
        );
        insert_extension(
            request,
            "gemini.cachedContent",
            self.gemini_cached_content.map(serde_json::Value::String),
        );
        Ok(())
    }
}

fn take_typed<T: DeserializeOwned>(
    extra: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> anyhow::Result<Option<T>> {
    extra
        .remove(name)
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|error| anyhow::anyhow!("invalid {name} provider field: {error}"))
        })
        .transpose()
}

fn insert_extension(
    request: &mut fte_types::GatewayRequest,
    name: &str,
    value: Option<serde_json::Value>,
) {
    if let Some(value) = value {
        request.provider_extensions.insert(name.to_string(), value);
    }
}

fn exact_catalog_provider<'a>(
    model: &str,
    catalog: &'a [ModelCatalogEntry],
) -> anyhow::Result<&'a str> {
    let providers = catalog
        .iter()
        .filter(|entry| entry.public_model_id == model || entry.provider_model_id == model)
        .map(|entry| entry.provider_id.as_str())
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        providers.len() == 1,
        "provider-specific fields require one exact catalog model"
    );
    Ok(providers.into_iter().next().expect("one provider checked"))
}

fn canonical_completion_request(
    request: serde_json::Value,
) -> anyhow::Result<(String, fte_types::GatewayRequest)> {
    let object = request
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("desktop completion request must be a JSON object"))?;
    anyhow::ensure!(
        !object
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        "desktop streaming must use the Gateway event API"
    );
    let requested_model = required_model(object)?;
    let mut canonical = serde_json::from_value::<OpenAiCompletionRequest>(request)?
        .into_gateway(hosted_defaults())?;
    canonical.model = desktop_model_selector(&requested_model);
    Ok((requested_model, canonical))
}

fn required_model(object: &serde_json::Map<String, serde_json::Value>) -> anyhow::Result<String> {
    object
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow::anyhow!("desktop request requires a nonempty string model"))
}

fn desktop_model_selector(model: &str) -> ModelSelector {
    if matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "auto" | "best" | "free/auto"
    ) {
        ModelSelector::Profile {
            name: "auto".to_string(),
        }
    } else {
        ModelSelector::ExactModel {
            model_id: model.trim().to_string(),
        }
    }
}

fn register_hosted_backends(
    gateway: &Arc<Gateway>,
    secrets: Arc<dyn SecretResolver>,
    product_catalog: &[ModelCatalogEntry],
) -> Result<(), GatewayError> {
    let mut catalog = product_catalog.iter().cloned().fold(
        BTreeMap::<String, Vec<ModelCatalogEntry>>::new(),
        |mut map, item| {
            map.entry(item.provider_id.clone()).or_default().push(item);
            map
        },
    );

    if let Some(entries) = catalog.remove("anthropic") {
        register(
            gateway,
            HostedProviderConfig::anthropic(
                "anthropic",
                "Anthropic",
                "anthropic",
                descriptors("anthropic", entries, true, true),
            ),
            Arc::clone(&secrets),
        )?;
    }

    if let Some(entries) = catalog.remove("gemini") {
        let mut models = descriptors("gemini", entries, true, false);
        for model in &mut models {
            model.capabilities.structured_output = true;
        }
        register(
            gateway,
            HostedProviderConfig::gemini("gemini", "Google Gemini", "gemini", models),
            Arc::clone(&secrets),
        )?;
    }

    for (id, name, chat, completion) in [
        (
            "openrouter",
            "OpenRouter",
            "https://openrouter.ai/api/v1/chat/completions",
            Some("https://openrouter.ai/api/v1/completions"),
        ),
        (
            "groq",
            "Groq Cloud",
            "https://api.groq.com/openai/v1/chat/completions",
            None,
        ),
        (
            "mistral",
            "Mistral AI",
            "https://api.mistral.ai/v1/chat/completions",
            None,
        ),
        (
            "nvidia",
            "NVIDIA NIM",
            "https://integrate.api.nvidia.com/v1/chat/completions",
            None,
        ),
        (
            "cerebras",
            "Cerebras",
            "https://api.cerebras.ai/v1/chat/completions",
            Some("https://api.cerebras.ai/v1/completions"),
        ),
    ] {
        let Some(entries) = catalog.remove(id) else {
            continue;
        };
        let mut config = HostedProviderConfig::openai_compatible(
            id,
            name,
            id,
            chat,
            descriptors(id, entries, false, false),
        );
        config.endpoints.completions = completion.map(ToString::to_string);
        if id == "openrouter" {
            config.static_headers.insert(
                "http-referer".to_string(),
                "https://free-token-energy.local".to_string(),
            );
            config
                .static_headers
                .insert("x-title".to_string(), "Free Token Energy".to_string());
        }
        register(gateway, config, Arc::clone(&secrets))?;
    }
    if !catalog.is_empty() {
        return Err(GatewayError::invalid_request(
            &RequestId::new(),
            "desktop_catalog_backend_missing",
            "the desktop catalog contains a provider with no modern backend",
        ));
    }
    Ok(())
}

pub(crate) fn public_models_from_catalog(catalog: &[ModelCatalogEntry]) -> Vec<PublicModel> {
    let mut models: BTreeMap<String, PublicModelAccumulator> = BTreeMap::new();
    for entry in catalog {
        let model = models
            .entry(entry.public_model_id.clone())
            .or_insert_with(|| PublicModelAccumulator {
                display_name: entry.display_name.clone(),
                ..PublicModelAccumulator::default()
            });
        model.providers.insert(entry.provider_name.clone());
        model.supports_chat_completions |= entry.chat_completions;
        if let Some(completion) = &entry.text_completions {
            model.supports_text_completions = true;
            model.prompt_semantics.insert(completion.prompt_semantics);
        }
    }
    let mut public_models = models
        .into_iter()
        .map(|(id, model)| PublicModel {
            id,
            display_name: model.display_name,
            providers: model.providers.into_iter().collect(),
            supports_chat_completions: model.supports_chat_completions,
            supports_text_completions: model.supports_text_completions,
            prompt_semantics: model.prompt_semantics.into_iter().collect(),
        })
        .collect::<Vec<_>>();
    public_models.insert(
        0,
        PublicModel {
            id: "auto".to_string(),
            display_name: "Automatic best available route".to_string(),
            providers: catalog
                .iter()
                .map(|entry| entry.provider_name.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            supports_chat_completions: catalog.iter().any(|entry| entry.chat_completions),
            supports_text_completions: catalog.iter().any(|entry| entry.text_completions.is_some()),
            prompt_semantics: catalog
                .iter()
                .filter_map(|entry| {
                    entry
                        .text_completions
                        .as_ref()
                        .map(|support| support.prompt_semantics)
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        },
    );
    public_models
}

fn provider_models(
    provider_id: &str,
    entries: &[&ModelCatalogEntry],
    snapshot: Option<&BackendSnapshot>,
) -> anyhow::Result<Vec<ProviderModelStatus>> {
    let models = entries
        .iter()
        .map(|entry| {
            let descriptor = snapshot.and_then(|snapshot| {
                snapshot
                    .descriptor
                    .models
                    .iter()
                    .find(|model| model.id == entry.provider_model_id)
            });
            ProviderModelStatus {
                public_model_id: entry.public_model_id.clone(),
                provider_model_id: entry.provider_model_id.clone(),
                display_name: descriptor.map_or_else(
                    || entry.display_name.clone(),
                    |model| model.display_name.clone(),
                ),
                headroom: descriptor.and_then(|model| model.observed.quota_headroom),
                supports_chat_completions: entry.chat_completions,
                supports_text_completions: entry.text_completions.is_some(),
                prompt_semantics: entry
                    .text_completions
                    .as_ref()
                    .map(|support| support.prompt_semantics),
            }
        })
        .collect::<Vec<_>>();
    if snapshot.is_some_and(|snapshot| {
        snapshot.descriptor.id != provider_id
            || models.iter().any(|model| {
                !snapshot
                    .descriptor
                    .models
                    .iter()
                    .any(|item| item.id == model.provider_model_id)
            })
    }) {
        anyhow::bail!("modern gateway descriptor diverged from the product catalog");
    }
    Ok(models)
}

const fn backend_kind(location: BackendLocation) -> ProviderBackendKind {
    match location {
        BackendLocation::Hosted => ProviderBackendKind::RemoteApi,
        BackendLocation::LocalEmbedded => ProviderBackendKind::LocalEmbedded,
    }
}

fn provider_status(
    readiness: &BackendReadiness,
    headroom: Option<f64>,
    summary: &ProviderLogSummary,
) -> &'static str {
    match readiness {
        BackendReadiness::NotConfigured { .. } => "needs_key",
        BackendReadiness::Loading => "loading",
        BackendReadiness::Unavailable { .. } => "unavailable",
        BackendReadiness::Ready if headroom.is_some_and(|value| value <= 0.0) => "quota_exhausted",
        BackendReadiness::Ready if summary.last_status_code == Some(429) => "quota_exhausted",
        BackendReadiness::Ready if summary.last_status_code.is_some_and(|status| status >= 400) => {
            "upstream_error"
        }
        BackendReadiness::Ready => "ready",
    }
}

fn register(
    gateway: &Gateway,
    config: HostedProviderConfig,
    secrets: Arc<dyn SecretResolver>,
) -> Result<(), GatewayError> {
    gateway.register_backend(Arc::new(HostedProviderBackend::new(config, secrets)?))
}

fn descriptors(
    backend_id: &str,
    entries: Vec<ModelCatalogEntry>,
    reasoning: bool,
    provider_cache: bool,
) -> Vec<ModelDescriptor> {
    entries
        .into_iter()
        .map(|entry| {
            let mut prompt_forms = Vec::new();
            if entry.chat_completions {
                prompt_forms.push(PromptForm::Chat);
            }
            if entry.text_completions.as_ref().is_some_and(|completion| {
                matches!(
                    completion.prompt_semantics,
                    PromptSemantics::DirectContinuation | PromptSemantics::LegacyPromptProtocol
                )
            }) {
                prompt_forms.push(PromptForm::Completion);
            }
            let vision = entry.capabilities.contains(&Capability::Vision);
            ModelDescriptor {
                id: entry.provider_model_id,
                aliases: vec![entry.public_model_id],
                display_name: entry.display_name,
                backend_id: backend_id.to_string(),
                location: BackendLocation::Hosted,
                capabilities: ModelCapabilities {
                    prompt_forms,
                    modalities: if vision {
                        vec![Modality::Text, Modality::Image]
                    } else {
                        vec![Modality::Text]
                    },
                    tools: entry.capabilities.contains(&Capability::Tools),
                    structured_output: false,
                    reasoning,
                    streaming: entry.capabilities.contains(&Capability::Streaming),
                    provider_cache,
                },
                context_tokens: None,
                max_output_tokens: None,
                observed: RouteObservations::default(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretStoreError;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeCredentialStore {
        values: Mutex<BTreeMap<String, Vec<u8>>>,
    }

    impl CredentialStore for FakeCredentialStore {
        fn write(&self, provider_id: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
            self.values
                .lock()
                .expect("credential fixture")
                .insert(provider_id.to_string(), secret.to_vec());
            Ok(())
        }

        fn read(&self, provider_id: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
            Ok(self
                .values
                .lock()
                .expect("credential fixture")
                .get(provider_id)
                .cloned())
        }

        fn delete(&self, provider_id: &str) -> Result<bool, SecretStoreError> {
            Ok(self
                .values
                .lock()
                .expect("credential fixture")
                .remove(provider_id)
                .is_some())
        }
    }

    #[test]
    fn desktop_gateway_registers_exactly_the_catalog_backends() {
        let runtime = GatewayRuntimeOwner::new().expect("gateway");
        let backends = runtime
            .gateway()
            .backend_snapshots()
            .into_iter()
            .map(|snapshot| snapshot.descriptor.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            backends,
            BTreeSet::from([
                "anthropic".to_string(),
                "cerebras".to_string(),
                "gemini".to_string(),
                "groq".to_string(),
                "llama-native".to_string(),
                "mistral".to_string(),
                "nvidia".to_string(),
                "openrouter".to_string(),
            ])
        );
    }

    #[tokio::test]
    async fn runtime_shutdown_reports_every_owned_worker_and_native_join() {
        let runtime = GatewayRuntimeOwner::new().expect("gateway");
        let report = runtime.shutdown_with_report().await;
        report.gateway.result.expect("Gateway shutdown");
        assert_eq!(
            report.gateway.expected_worker_ids,
            vec![
                "backend-shutdown:anthropic",
                "backend-shutdown:cerebras",
                "backend-shutdown:gemini",
                "backend-shutdown:groq",
                "backend-shutdown:llama-native",
                "backend-shutdown:mistral",
                "backend-shutdown:nvidia",
                "backend-shutdown:openrouter",
            ]
        );
        assert_eq!(
            report.gateway.joined_worker_ids,
            report.gateway.expected_worker_ids
        );
        assert_eq!(report.gateway.retained_tasks, 0);
        assert!(report.native_host_joined);
    }

    #[test]
    fn desktop_native_backend_uses_the_shared_gateway_and_local_only_route() {
        let runtime = GatewayRuntimeOwner::new().expect("gateway");
        runtime
            .bind_database(test_database("native-shared-route"))
            .expect("bind database");
        let plugin_gateway = runtime.gateway();
        assert!(Arc::ptr_eq(&runtime.gateway, &plugin_gateway));
        let initial = plugin_gateway
            .backend_snapshots()
            .into_iter()
            .find(|snapshot| snapshot.descriptor.id == NATIVE_BACKEND_ID)
            .expect("native backend");
        assert!(matches!(
            initial.readiness,
            BackendReadiness::NotConfigured { .. }
        ));

        let path = std::env::temp_dir().join(format!(
            "free-token-energy-native-route-{}-{}.gguf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        ));
        write_test_gguf(&path);
        let model_id = runtime
            .configure_local_model(&path, None)
            .expect("configure local model");
        assert_eq!(model_id, LOCAL_MODEL_ID);
        assert!(runtime.public_models().iter().any(|model| {
            model.id == LOCAL_MODEL_ID && model.display_name == LOCAL_MODEL_DISPLAY_NAME
        }));

        let request = serde_json::json!({
            "model":LOCAL_MODEL_ID,
            "messages":[{"role":"user","content":"local only"}],
            "max_tokens":1
        });
        let (_, mut canonical) =
            canonical_chat_request(request, &runtime.catalog).expect("canonical local request");
        runtime.apply_route_policy(LOCAL_MODEL_ID, &mut canonical);
        assert_eq!(canonical.routing.privacy, PrivacyPolicy::LocalOnly);
        assert_eq!(canonical.routing.profile, RouteProfile::LocalOnly);
        std::fs::remove_file(path).expect("remove GGUF fixture");

        assert!(runtime.shutdown_native_for_process_exit());
        assert!(runtime.shutdown_native_for_process_exit());
    }

    #[tokio::test]
    #[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
    async fn real_desktop_local_route_runs_in_process_and_joins_on_shutdown() {
        let model_path = std::env::var_os("MOM_LLAMA_MODEL_PATH")
            .map(PathBuf::from)
            .expect("MOM_LLAMA_MODEL_PATH");
        let runtime = GatewayRuntimeOwner::new().expect("gateway");
        runtime
            .bind_database(test_database("real-local-route"))
            .expect("bind database");
        runtime
            .configure_local_model(model_path, None)
            .expect("configure local GGUF");
        let request = serde_json::json!({
            "model":LOCAL_MODEL_ID,
            "messages":[{
                "role":"user",
                "content":"Reply with the single word ready."
            }],
            "temperature":0.0,
            "max_tokens":8
        });
        let (_, mut canonical) =
            canonical_chat_request(request, &runtime.catalog).expect("canonical local request");
        runtime.apply_route_policy(LOCAL_MODEL_ID, &mut canonical);
        let ticket = runtime
            .gateway
            .execute(canonical)
            .await
            .expect("start local inference");
        let response = ticket.final_response().await.expect("local inference");
        assert_eq!(response.route.backend_id, NATIVE_BACKEND_ID);
        assert!(!response.output.is_empty());
        runtime.gateway.shutdown().await.expect("drain Gateway");
        assert!(runtime.shutdown_native_for_process_exit());
    }

    #[test]
    fn local_model_configuration_restores_through_a_new_gateway_owner() {
        let database_path = test_database_path("local-model-restart");
        let model_path = test_gguf_path("local-model-restart");
        write_test_gguf(&model_path);
        {
            let database = Arc::new(Database::new(database_path.clone()).expect("database"));
            let runtime = GatewayRuntimeOwner::new().expect("gateway");
            runtime.bind_database(database).expect("bind database");
            runtime
                .configure_local_model(&model_path, Some("a".repeat(64)))
                .expect("configure local model");
            assert_eq!(
                runtime.local_model_status().unwrap().state,
                LocalModelState::Ready
            );
            assert!(runtime.shutdown_native_for_process_exit());
        }

        let reopened = Arc::new(Database::new(database_path).expect("reopen database"));
        let restarted = GatewayRuntimeOwner::new().expect("restarted gateway");
        restarted
            .bind_database(Arc::clone(&reopened))
            .expect("bind reopened database");
        let status = restarted
            .restore_local_model_configuration()
            .expect("restore local model");
        assert_eq!(status.state, LocalModelState::Ready);
        assert_eq!(
            status.display_name.as_deref(),
            model_path.file_name().and_then(|v| v.to_str())
        );
        assert!(
            restarted
                .public_models()
                .iter()
                .any(|model| model.id == LOCAL_MODEL_ID)
        );
        assert!(reopened.get_local_model_configuration().unwrap().is_some());
        assert!(restarted.shutdown_native_for_process_exit());
        std::fs::remove_file(model_path).expect("remove GGUF fixture");
    }

    #[test]
    fn invalid_replacement_does_not_overwrite_working_local_model_configuration() {
        let database = test_database("local-model-invalid-replacement");
        let runtime = GatewayRuntimeOwner::new().expect("gateway");
        runtime
            .bind_database(Arc::clone(&database))
            .expect("bind database");
        let model_path = test_gguf_path("valid-model");
        write_test_gguf(&model_path);
        runtime
            .configure_local_model(&model_path, None)
            .expect("configure valid local model");
        let saved = database
            .get_local_model_configuration()
            .unwrap()
            .expect("saved configuration");

        let missing_path = test_gguf_path("missing-replacement");
        assert!(runtime.configure_local_model(missing_path, None).is_err());
        assert_eq!(
            database.get_local_model_configuration().unwrap(),
            Some(saved)
        );
        assert_eq!(
            runtime.local_model_status().unwrap().state,
            LocalModelState::Ready
        );

        assert!(runtime.shutdown_native_for_process_exit());
        std::fs::remove_file(model_path).expect("remove GGUF fixture");
    }

    #[test]
    fn missing_saved_model_restores_as_invalid_without_deleting_the_selection() {
        let database = test_database("local-model-missing-restore");
        let missing_path = test_gguf_path("missing-saved-model");
        let saved = LocalModelConfiguration {
            model_path: missing_path.to_string_lossy().into_owned(),
            expected_sha256: None,
        };
        database
            .save_local_model_configuration(&saved)
            .expect("seed saved model");
        let runtime = GatewayRuntimeOwner::new().expect("gateway");
        runtime
            .bind_database(Arc::clone(&database))
            .expect("bind database");

        let status = runtime
            .restore_local_model_configuration()
            .expect("restore missing model state");
        assert_eq!(status.state, LocalModelState::Invalid);
        assert!(status.detail.contains("regular file"));
        assert!(
            !runtime
                .public_models()
                .iter()
                .any(|model| model.id == LOCAL_MODEL_ID)
        );
        assert_eq!(
            database.get_local_model_configuration().unwrap(),
            Some(saved)
        );
        assert!(runtime.shutdown_native_for_process_exit());
    }

    #[tokio::test]
    async fn native_configuration_failure_rolls_back_the_persisted_replacement() {
        let database = test_database("local-model-configure-rollback");
        let runtime = GatewayRuntimeOwner::new().expect("gateway");
        runtime
            .bind_database(Arc::clone(&database))
            .expect("bind database");
        let first_path = test_gguf_path("rollback-first");
        let replacement_path = test_gguf_path("rollback-replacement");
        write_test_gguf(&first_path);
        write_test_gguf(&replacement_path);
        runtime
            .configure_local_model(&first_path, None)
            .expect("configure first model");
        let first = database
            .get_local_model_configuration()
            .unwrap()
            .expect("first persisted model");
        runtime.gateway.shutdown().await.expect("shutdown Gateway");

        assert!(
            runtime
                .configure_local_model(&replacement_path, None)
                .is_err()
        );
        assert_eq!(
            database.get_local_model_configuration().unwrap(),
            Some(first)
        );
        assert_eq!(
            runtime.local_model_status().unwrap().state,
            LocalModelState::Ready
        );
        assert!(runtime.shutdown_native_for_process_exit());
        std::fs::remove_file(first_path).expect("remove first fixture");
        std::fs::remove_file(replacement_path).expect("remove replacement fixture");
    }

    #[test]
    fn desktop_chat_edge_preserves_public_alias_and_message_content() {
        let request = serde_json::json!({
            "model":"vendor/public-model",
            "messages":[{"role":"user","content":"exact text"}],
            "temperature":0.25,
            "max_tokens":17
        });

        let (requested_model, canonical) =
            canonical_chat_request(request, &default_model_catalog())
                .expect("canonical desktop chat");

        assert_eq!(requested_model, "vendor/public-model");
        assert_eq!(
            canonical.model,
            ModelSelector::ExactModel {
                model_id: "vendor/public-model".to_string()
            }
        );
        assert_eq!(canonical.sampling.max_output_tokens, Some(17));
        assert_eq!(canonical.sampling.temperature, Some(0.25));
        let fte_types::GenerationInput::Chat { items } = canonical.input else {
            panic!("chat edge must produce canonical chat input");
        };
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn desktop_anthropic_edge_preserves_only_typed_provider_fields() {
        let request = serde_json::json!({
            "model":"claude-sonnet-5",
            "messages":[{"role":"user","content":"hello"}],
            "top_k":23,
            "metadata":{"user_id":"stable-user"},
            "thinking":{"type":"enabled","budget_tokens":1024},
            "anthropic_beta":"interleaved-thinking-2025-05-14"
        });

        let (_, canonical) = canonical_chat_request(request, &default_model_catalog())
            .expect("canonical Anthropic request");
        assert_eq!(canonical.sampling.top_k, Some(23));
        assert_eq!(
            canonical.provider_extensions,
            BTreeMap::from([
                (
                    "anthropic.beta".to_string(),
                    serde_json::json!("interleaved-thinking-2025-05-14"),
                ),
                (
                    "anthropic.metadata".to_string(),
                    serde_json::json!({"user_id":"stable-user"}),
                ),
                (
                    "anthropic.thinking".to_string(),
                    serde_json::json!({"type":"enabled","budget_tokens":1024}),
                ),
            ])
        );
    }

    #[test]
    fn desktop_gemini_edge_preserves_nested_provider_fields_and_tool_history() {
        let request = serde_json::json!({
            "model":"gemini-2.5-flash",
            "messages":[
                {
                    "role":"assistant",
                    "content":"checking",
                    "tool_calls":[{
                        "id":"call_1",
                        "type":"function",
                        "function":{"name":"lookup","arguments":"{\"term\":\"FTE\"}"}
                    }]
                },
                {"role":"tool","tool_call_id":"call_1","content":"found"}
            ],
            "top_k":40,
            "thinking_config":{"thinkingBudget":512},
            "tool_config":{"functionCallingConfig":{"mode":"AUTO"}},
            "safety_settings":[{"category":"HARM_CATEGORY_HATE_SPEECH","threshold":"BLOCK_NONE"}],
            "cached_content":"cachedContents/stable"
        });

        let (_, canonical) = canonical_chat_request(request, &default_model_catalog())
            .expect("canonical Gemini request");
        assert_eq!(canonical.sampling.top_k, Some(40));
        assert_eq!(
            canonical.provider_extensions["gemini.thinkingConfig"],
            serde_json::json!({"thinkingBudget":512})
        );
        assert_eq!(
            canonical.provider_extensions["gemini.cachedContent"],
            serde_json::json!("cachedContents/stable")
        );
        let fte_types::GenerationInput::Chat { items } = canonical.input else {
            panic!("chat edge must produce canonical chat input");
        };
        assert!(matches!(items[0], fte_types::InputItem::Message { .. }));
        assert!(matches!(
            items[1],
            fte_types::InputItem::FunctionCall { .. }
        ));
        assert!(matches!(
            items[2],
            fte_types::InputItem::FunctionResult { .. }
        ));
    }

    #[test]
    fn desktop_provider_fields_fail_closed_for_ambiguous_or_unknown_models() {
        for model in ["auto", "not-in-catalog"] {
            let request = serde_json::json!({
                "model":model,
                "messages":[{"role":"user","content":"hello"}],
                "thinking":{"type":"enabled","budget_tokens":1024}
            });
            assert!(canonical_chat_request(request, &default_model_catalog()).is_err());
        }
    }

    #[test]
    fn desktop_completion_edge_preserves_exact_token_batches() {
        let request = serde_json::json!({
            "model":"public-completion",
            "prompt":[[1,2,3],[8,13]],
            "max_tokens":9
        });

        let (_, canonical) =
            canonical_completion_request(request).expect("canonical desktop completion");

        let fte_types::GenerationInput::Completion { prompts } = canonical.input else {
            panic!("completion edge must produce canonical completion input");
        };
        assert_eq!(
            prompts,
            vec![
                fte_types::CompletionPrompt::Tokens {
                    token_ids: vec![1, 2, 3]
                },
                fte_types::CompletionPrompt::Tokens {
                    token_ids: vec![8, 13]
                }
            ]
        );
        assert_eq!(canonical.sampling.max_output_tokens, Some(9));
    }

    #[test]
    fn public_model_catalog_matches_the_promoted_golden_fixture() {
        let database = test_database("model-golden");
        let runtime = GatewayRuntimeOwner::new().expect("gateway");
        runtime
            .bind_database(Arc::clone(&database))
            .expect("bind database");

        assert_eq!(
            runtime
                .public_models()
                .into_iter()
                .map(|model| (model.id, model.supports_text_completions))
                .collect::<Vec<_>>(),
            vec![
                ("auto".to_string(), true),
                ("claude-haiku-4.5".to_string(), false),
                ("claude-opus-5".to_string(), false),
                ("claude-sonnet-5".to_string(), false),
                ("gemini-2.5-flash".to_string(), false),
                ("gemini-2.5-flash-lite".to_string(), false),
                ("gemini-2.5-pro".to_string(), false),
                ("gpt-oss-120b".to_string(), true),
                ("llama-3.1-70b-instruct".to_string(), false),
                ("llama-3.1-8b-instant".to_string(), false),
                ("llama-3.3-70b-versatile".to_string(), false),
                ("mistral-small-latest".to_string(), false),
                ("openrouter-free".to_string(), false),
            ]
        );
    }

    #[test]
    fn provider_inventory_matches_the_promoted_golden_fixture() {
        let database = test_database("provider-golden");
        let store = Arc::new(FakeCredentialStore::default());
        store
            .write("anthropic", b"fixture-secret")
            .expect("save OS credential fixture");
        let runtime = GatewayRuntimeOwner::new_with_store(store).expect("gateway");
        runtime
            .bind_database(Arc::clone(&database))
            .expect("bind database");

        let modern = runtime.provider_statuses().expect("modern providers");
        assert_eq!(
            modern
                .iter()
                .map(|provider| (
                    provider.id.as_str(),
                    provider.configured,
                    provider.status.as_str(),
                    provider.model_count,
                    provider.text_completion_model_count,
                ))
                .collect::<Vec<_>>(),
            vec![
                ("anthropic", true, "ready", 3, 0),
                ("cerebras", false, "needs_key", 1, 1),
                ("gemini", false, "needs_key", 3, 0),
                ("groq", false, "needs_key", 2, 0),
                ("mistral", false, "needs_key", 1, 0),
                ("nvidia", false, "needs_key", 1, 0),
                ("openrouter", false, "needs_key", 1, 0),
                (NATIVE_BACKEND_ID, false, "needs_model", 0, 0),
            ]
        );
        assert!(modern.iter().all(|provider| {
            provider.total_tokens == 0
                && provider.avg_latency_ms == 0
                && provider.request_count == 0
                && provider.last_request_at.is_none()
                && provider.last_status_code.is_none()
        }));
    }

    fn test_database(label: &str) -> Arc<Database> {
        Arc::new(Database::new(test_database_path(label)).expect("test database"))
    }

    fn test_database_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "free-token-energy-gateway-runtime-{label}-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        ))
    }

    fn test_gguf_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "free-token-energy-{label}-{}-{}.gguf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        ))
    }

    fn write_test_gguf(path: &Path) {
        std::fs::write(path, b"GGUF").expect("write minimal GGUF fixture");
    }
}
