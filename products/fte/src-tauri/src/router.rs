use chrono::{TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::backend::{
    BackendCredentials, BackendKind, BackendReadiness, BackendRegistry, CredentialRequirement,
    InferenceBackend,
};
use crate::catalog::{ModelCatalogEntry, PromptSemantics, default_model_catalog};
use crate::db::{Database, ProviderLogSummary};
use crate::eval_store::EvalStore;
use crate::providers::{
    Capability, ChatChunk, ChatRequest, ChatResponse, CompletionChunk, CompletionRequest,
    CompletionResponse, spec::ParameterPolicy,
};
use crate::rate_limiter::QuotaTracker;
use async_stream::try_stream;
use futures::{StreamExt, stream::BoxStream};
use tracing::{info, warn};

pub struct Router {
    backends: BackendRegistry,
    quota_tracker: Arc<QuotaTracker>,
    eval_store: Arc<EvalStore>,
    db: Arc<Database>,
    model_catalog: Vec<ModelCatalogEntry>,
}

#[derive(Debug, Clone)]
pub struct RouteResult {
    pub provider_id: String,
    pub public_model_id: String,
    pub provider_model_id: String,
    pub parameter_policy: ParameterPolicy,
    pub quota_tracked: bool,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct ProviderModelStatus {
    pub public_model_id: String,
    pub provider_model_id: String,
    pub display_name: String,
    pub headroom: Option<f64>,
    pub supports_chat_completions: bool,
    pub supports_text_completions: bool,
    pub prompt_semantics: Option<PromptSemantics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub id: String,
    pub name: String,
    pub backend_kind: BackendKind,
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

impl Router {
    pub fn new(
        quota_tracker: Arc<QuotaTracker>,
        eval_store: Arc<EvalStore>,
        db: Arc<Database>,
    ) -> Self {
        Self::with_catalog(quota_tracker, eval_store, db, default_model_catalog())
    }

    pub fn with_catalog(
        quota_tracker: Arc<QuotaTracker>,
        eval_store: Arc<EvalStore>,
        db: Arc<Database>,
        model_catalog: Vec<ModelCatalogEntry>,
    ) -> Self {
        for entry in &model_catalog {
            quota_tracker.register_model(
                entry.provider_id.clone(),
                entry.provider_model_id.clone(),
                entry.quota.windows(),
            );
        }

        let router = Self {
            backends: BackendRegistry::default(),
            quota_tracker,
            eval_store,
            db,
            model_catalog,
        };
        router.restore_quota_events();
        router
    }

    fn restore_quota_events(&self) {
        let since = Utc::now().timestamp() - 86_400;
        match self.db.get_quota_events_since(since) {
            Ok(events) => {
                for event in events {
                    let Some(occurred_at) = Utc.timestamp_opt(event.occurred_at, 0).single() else {
                        warn!("Ignoring quota event with invalid timestamp");
                        continue;
                    };
                    self.quota_tracker.restore_event(
                        &event.provider_id,
                        &event.model_id,
                        occurred_at,
                        event.request_count,
                        event.tokens,
                    );
                }
            }
            Err(error) => warn!("Could not restore persisted quota events: {error}"),
        }
    }

    pub fn add_backend(&mut self, backend: Box<dyn InferenceBackend>) -> anyhow::Result<()> {
        for entry in self
            .model_catalog
            .iter()
            .filter(|entry| entry.provider_id == backend.id())
        {
            validate_model_route(backend.as_ref(), entry)?;
        }
        self.backends.register(backend)
    }

    /// Adds runtime-discovered model routes for an already registered backend.
    ///
    /// This is primarily intended for explicitly selected local GGUF models. It remains a
    /// startup-time operation so route selection can use an immutable catalog snapshot.
    pub fn add_model_routes(
        &mut self,
        backend_id: &str,
        entries: Vec<ModelCatalogEntry>,
    ) -> anyhow::Result<()> {
        let backend = self.backends.get(backend_id).ok_or_else(|| {
            anyhow::anyhow!("Cannot add model routes for unregistered backend '{backend_id}'.")
        })?;

        let mut route_keys = self
            .model_catalog
            .iter()
            .map(|entry| {
                (
                    entry.provider_id.clone(),
                    entry.public_model_id.clone(),
                    entry.provider_model_id.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        for entry in &entries {
            validate_model_route(backend, entry)?;
            let key = (
                entry.provider_id.clone(),
                entry.public_model_id.clone(),
                entry.provider_model_id.clone(),
            );
            if !route_keys.insert(key) {
                return Err(anyhow::anyhow!(
                    "Model route '{}/{}' is already registered.",
                    entry.provider_id,
                    entry.provider_model_id
                ));
            }
        }

        for entry in &entries {
            self.quota_tracker.register_model(
                entry.provider_id.clone(),
                entry.provider_model_id.clone(),
                entry.quota.windows(),
            );
        }
        self.model_catalog.extend(entries);
        Ok(())
    }

    pub fn supports_provider(&self, provider_id: &str) -> bool {
        self.backends.contains(provider_id)
            && self
                .model_catalog
                .iter()
                .any(|entry| entry.provider_id == provider_id)
    }

    pub fn credential_requirement(&self, backend_id: &str) -> Option<CredentialRequirement> {
        self.backends
            .get(backend_id)
            .map(InferenceBackend::credential_requirement)
    }

    pub fn public_models(&self) -> Vec<PublicModel> {
        let mut models: BTreeMap<String, PublicModelAccumulator> = BTreeMap::new();

        for entry in &self.model_catalog {
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

        let mut public_models: Vec<PublicModel> = models
            .into_iter()
            .map(|(id, model)| PublicModel {
                id,
                display_name: model.display_name,
                providers: model.providers.into_iter().collect(),
                supports_chat_completions: model.supports_chat_completions,
                supports_text_completions: model.supports_text_completions,
                prompt_semantics: model.prompt_semantics.into_iter().collect(),
            })
            .collect();

        let all_providers = self
            .model_catalog
            .iter()
            .map(|entry| entry.provider_name.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        public_models.insert(
            0,
            PublicModel {
                id: "auto".to_string(),
                display_name: "Automatic best available route".to_string(),
                providers: all_providers,
                supports_chat_completions: self
                    .model_catalog
                    .iter()
                    .any(|entry| entry.chat_completions),
                supports_text_completions: self
                    .model_catalog
                    .iter()
                    .any(|entry| entry.text_completions.is_some()),
                prompt_semantics: self
                    .model_catalog
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

    pub fn provider_statuses(&self) -> anyhow::Result<Vec<ProviderStatus>> {
        let summaries = self.db.get_provider_log_summaries()?;
        let mut by_provider: BTreeMap<String, (String, Vec<&ModelCatalogEntry>)> = BTreeMap::new();

        for entry in &self.model_catalog {
            by_provider
                .entry(entry.provider_id.clone())
                .or_insert_with(|| (entry.provider_name.clone(), Vec::new()))
                .1
                .push(entry);
        }

        let mut statuses = Vec::new();
        for (provider_id, (provider_name, entries)) in by_provider {
            let backend = self.backends.get(&provider_id);
            let readiness = self.backend_readiness(backend)?;
            let configured = backend.is_some() && readiness.configuration_satisfied();
            let summary = summaries.get(&provider_id).cloned().unwrap_or_default();

            let models: Vec<ProviderModelStatus> = entries
                .iter()
                .map(|entry| ProviderModelStatus {
                    public_model_id: entry.public_model_id.clone(),
                    provider_model_id: entry.provider_model_id.clone(),
                    display_name: entry.display_name.clone(),
                    headroom: entry.quota.has_documented_limit().then(|| {
                        self.quota_tracker
                            .headroom(&entry.provider_id, &entry.provider_model_id)
                    }),
                    supports_chat_completions: entry.chat_completions,
                    supports_text_completions: entry.text_completions.is_some(),
                    prompt_semantics: entry
                        .text_completions
                        .as_ref()
                        .map(|support| support.prompt_semantics),
                })
                .collect();

            let headroom = models
                .iter()
                .filter_map(|model| model.headroom)
                .reduce(f64::max);
            let status = provider_status(readiness, headroom, &summary);

            statuses.push(ProviderStatus {
                id: provider_id,
                name: provider_name,
                backend_kind: backend.map_or(BackendKind::Unknown, InferenceBackend::kind),
                credential_required: backend.is_some_and(|backend| {
                    backend.credential_requirement() == CredentialRequirement::ApiKey
                }),
                configured,
                status,
                model_count: models.len(),
                text_completion_model_count: models
                    .iter()
                    .filter(|model| model.supports_text_completions)
                    .count(),
                headroom: headroom.map(|value| (value * 100.0).round() / 100.0),
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

    pub fn global_headroom_percent(&self) -> anyhow::Result<Option<f64>> {
        let mut headrooms = Vec::new();

        for entry in &self.model_catalog {
            if self
                .backend_readiness(self.backends.get(&entry.provider_id))?
                .is_ready()
                && entry.quota.has_documented_limit()
            {
                headrooms.push(
                    self.quota_tracker
                        .headroom(&entry.provider_id, &entry.provider_model_id),
                );
            }
        }

        if headrooms.is_empty() {
            return Ok(None);
        }

        let average = headrooms.iter().sum::<f64>() / headrooms.len() as f64;
        Ok(Some((average * 10000.0).round() / 100.0))
    }

    pub async fn route(&self, req: &ChatRequest, task_hint: &str) -> anyhow::Result<RouteResult> {
        validate_chat_request(req)?;

        let mut best_route: Option<RouteResult> = None;
        let mut max_score = -1.0;
        let mut saw_model_candidate = false;
        let mut saw_chat_candidate = false;
        let mut saw_missing_credential = false;
        let mut saw_unready_candidate = false;
        let mut saw_ready_candidate = false;
        let mut saw_capable_candidate = false;
        let required_capabilities = required_capabilities(req);
        let provider_summaries = self.db.get_provider_log_summaries()?;

        for entry in &self.model_catalog {
            if !entry.matches_requested_model(&req.model) {
                continue;
            }
            saw_model_candidate = true;

            if !entry.chat_completions {
                continue;
            }
            saw_chat_candidate = true;

            let Some(backend) = self.backends.get(&entry.provider_id) else {
                saw_unready_candidate = true;
                continue;
            };

            if required_capabilities.iter().any(|capability| {
                !backend.capabilities().contains(capability)
                    || !entry.capabilities.contains(capability)
            }) {
                continue;
            }
            saw_capable_candidate = true;

            match self.backend_readiness(Some(backend))? {
                BackendReadiness::Ready => saw_ready_candidate = true,
                BackendReadiness::MissingCredential => {
                    saw_missing_credential = true;
                    continue;
                }
                BackendReadiness::NotConfigured
                | BackendReadiness::Loading
                | BackendReadiness::Unavailable => {
                    saw_unready_candidate = true;
                    continue;
                }
            }

            let headroom = self
                .quota_tracker
                .headroom(&entry.provider_id, &entry.provider_model_id);
            if headroom <= 0.0 {
                continue;
            }

            let score = self.route_score(
                entry,
                task_hint,
                headroom,
                provider_summaries.get(&entry.provider_id),
            );

            info!(
                "Scored {}/{} as {}: {}",
                entry.provider_id, entry.provider_model_id, entry.public_model_id, score
            );

            if score > max_score {
                max_score = score;
                best_route = Some(RouteResult {
                    provider_id: entry.provider_id.clone(),
                    public_model_id: entry.public_model_id.clone(),
                    provider_model_id: entry.provider_model_id.clone(),
                    parameter_policy: entry.parameter_policy.clone(),
                    quota_tracked: entry.quota.has_enforced_limit(),
                    score,
                });
            }
        }

        best_route.ok_or_else(|| {
            if !saw_model_candidate {
                anyhow::anyhow!(
                    "Unknown model '{}'. Use /v1/models or model 'auto'.",
                    req.model
                )
            } else if !saw_chat_candidate {
                anyhow::anyhow!("Model '{}' does not support chat completions.", req.model)
            } else if !saw_capable_candidate {
                anyhow::anyhow!(
                    "No provider for '{}' supports every requested capability.",
                    req.model
                )
            } else if saw_missing_credential && !saw_ready_candidate && !saw_unready_candidate {
                anyhow::anyhow!(
                    "No API key is saved for a provider that can serve '{}'.",
                    req.model
                )
            } else if !saw_ready_candidate {
                anyhow::anyhow!(
                    "No configured inference backend for '{}' is ready.",
                    req.model
                )
            } else {
                anyhow::anyhow!(
                    "All configured providers for '{}' are out of quota or unavailable.",
                    req.model
                )
            }
        })
    }

    pub async fn route_completion(
        &self,
        req: &CompletionRequest,
        task_hint: &str,
    ) -> anyhow::Result<RouteResult> {
        validate_completion_request(req)?;

        let mut best_route: Option<RouteResult> = None;
        let mut max_score = -1.0;
        let mut saw_model_candidate = false;
        let mut saw_completion_candidate = false;
        let mut saw_compatible_candidate = false;
        let mut saw_missing_credential = false;
        let mut saw_unready_candidate = false;
        let mut saw_ready_candidate = false;
        let mut incompatible = BTreeSet::new();
        let provider_summaries = self.db.get_provider_log_summaries()?;

        for entry in &self.model_catalog {
            if !entry.matches_requested_model(&req.model) {
                continue;
            }
            saw_model_candidate = true;

            let Some(completion_support) = &entry.text_completions else {
                continue;
            };
            let Some(backend) = self.backends.get(&entry.provider_id) else {
                saw_unready_candidate = true;
                continue;
            };
            if !backend.supports_completions() {
                continue;
            }
            saw_completion_candidate = true;

            if !completion_support.supports(req) {
                incompatible.extend(completion_support.incompatibilities(req));
                continue;
            }
            if req.stream
                && (!backend.capabilities().contains(&Capability::Streaming)
                    || !entry.capabilities.contains(&Capability::Streaming))
            {
                incompatible.insert("stream".to_string());
                continue;
            }
            saw_compatible_candidate = true;

            match self.backend_readiness(Some(backend))? {
                BackendReadiness::Ready => saw_ready_candidate = true,
                BackendReadiness::MissingCredential => {
                    saw_missing_credential = true;
                    continue;
                }
                BackendReadiness::NotConfigured
                | BackendReadiness::Loading
                | BackendReadiness::Unavailable => {
                    saw_unready_candidate = true;
                    continue;
                }
            }

            let headroom = self
                .quota_tracker
                .headroom(&entry.provider_id, &entry.provider_model_id);
            if headroom <= 0.0 {
                continue;
            }

            let score = self.route_score(
                entry,
                task_hint,
                headroom,
                provider_summaries.get(&entry.provider_id),
            );

            info!(
                "Scored native completion route {}/{} as {}: {}",
                entry.provider_id, entry.provider_model_id, entry.public_model_id, score
            );

            if score > max_score {
                max_score = score;
                best_route = Some(RouteResult {
                    provider_id: entry.provider_id.clone(),
                    public_model_id: entry.public_model_id.clone(),
                    provider_model_id: entry.provider_model_id.clone(),
                    parameter_policy: entry.parameter_policy.clone(),
                    quota_tracked: entry.quota.has_enforced_limit(),
                    score,
                });
            }
        }

        best_route.ok_or_else(|| {
            if !saw_model_candidate {
                anyhow::anyhow!(
                    "Unknown model '{}'. Use /v1/models or model 'auto'.",
                    req.model
                )
            } else if !saw_completion_candidate {
                anyhow::anyhow!(
                    "Model '{}' does not support native text completions.",
                    req.model
                )
            } else if !saw_compatible_candidate {
                anyhow::anyhow!(
                    "No native text completion route for '{}' accepts: {}.",
                    req.model,
                    incompatible.into_iter().collect::<Vec<_>>().join(", ")
                )
            } else if saw_missing_credential && !saw_ready_candidate && !saw_unready_candidate {
                anyhow::anyhow!(
                    "No API key is saved for a native text completion provider that can serve '{}'.",
                    req.model
                )
            } else if !saw_ready_candidate {
                anyhow::anyhow!(
                    "No configured native text completion backend for '{}' is ready.",
                    req.model
                )
            } else {
                anyhow::anyhow!(
                    "All configured native text completion providers for '{}' are out of quota or unavailable.",
                    req.model
                )
            }
        })
    }

    pub async fn chat(&self, req: &ChatRequest, task_hint: &str) -> anyhow::Result<ChatResponse> {
        let route = self.route(req, task_hint).await?;
        self.reserve_request(&route)?;
        let backend = self
            .backends
            .get(&route.provider_id)
            .ok_or_else(|| anyhow::anyhow!("Inference backend disappeared during routing"))?;
        let credential = self.backend_credentials(backend)?;

        let mut provider_req = req.clone();
        provider_req.model = route.provider_model_id.clone();
        provider_req.stream = false;
        provider_req.stream_options = None;

        let start = std::time::Instant::now();
        let result = backend
            .chat(
                &provider_req,
                credential.as_credentials(),
                &route.parameter_policy,
            )
            .await;
        let latency = elapsed_millis(start);

        match result {
            Ok(mut response) => {
                let total_tokens = response
                    .usage
                    .as_ref()
                    .map(|usage| usage.total_tokens)
                    .unwrap_or(0);

                self.record_tokens(&route, total_tokens);
                self.log_request(
                    &route.provider_id,
                    &route.public_model_id,
                    total_tokens,
                    latency,
                    200,
                );

                response.model = Some(route.public_model_id);
                if response.object.is_none() {
                    response.object = Some("chat.completion".to_string());
                }
                if response.created.is_none() {
                    response.created = Some(current_unix_timestamp());
                }

                Ok(response)
            }
            Err(error) => {
                self.log_request(
                    &route.provider_id,
                    &route.public_model_id,
                    0,
                    latency,
                    request_error_status(&error),
                );
                Err(error)
            }
        }
    }

    pub async fn complete(
        &self,
        req: &CompletionRequest,
        task_hint: &str,
    ) -> anyhow::Result<CompletionResponse> {
        let route = self.route_completion(req, task_hint).await?;
        self.reserve_request(&route)?;
        let backend = self
            .backends
            .get(&route.provider_id)
            .ok_or_else(|| anyhow::anyhow!("Inference backend disappeared during routing"))?;
        let credential = self.backend_credentials(backend)?;

        let mut provider_req = req.clone();
        provider_req.model = route.provider_model_id.clone();
        provider_req.stream = false;
        provider_req.stream_options = None;

        let start = std::time::Instant::now();
        let result = backend
            .complete(&provider_req, credential.as_credentials())
            .await;
        let latency = elapsed_millis(start);

        match result {
            Ok(mut response) => {
                let total_tokens = response.total_tokens().unwrap_or_default();
                self.record_tokens(&route, total_tokens);
                self.log_request(
                    &route.provider_id,
                    &route.public_model_id,
                    total_tokens,
                    latency,
                    200,
                );
                response.normalize(&route.public_model_id, current_unix_timestamp());
                Ok(response)
            }
            Err(error) => {
                self.log_request(
                    &route.provider_id,
                    &route.public_model_id,
                    0,
                    latency,
                    request_error_status(&error),
                );
                Err(error)
            }
        }
    }

    pub async fn chat_stream(
        &self,
        req: &ChatRequest,
        task_hint: &str,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<ChatChunk>>> {
        let route = self.route(req, task_hint).await?;
        self.reserve_request(&route)?;
        let backend = self
            .backends
            .get(&route.provider_id)
            .ok_or_else(|| anyhow::anyhow!("Inference backend disappeared during routing"))?;
        let credential = self.backend_credentials(backend)?;

        let mut provider_req = req.clone();
        provider_req.model = route.provider_model_id.clone();
        provider_req.stream = true;

        let start = std::time::Instant::now();
        let upstream = match backend
            .chat_stream(
                &provider_req,
                credential.as_credentials(),
                &route.parameter_policy,
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                self.log_request(
                    &route.provider_id,
                    &route.public_model_id,
                    0,
                    elapsed_millis(start),
                    request_error_status(&error),
                );
                return Err(error);
            }
        };
        let quota_tracker = self.quota_tracker.clone();
        let db = self.db.clone();
        let route_for_stream = route.clone();
        let created = current_unix_timestamp();

        let stream = try_stream! {
            let mut upstream = upstream;
            let mut audit = StreamAudit::new(
                quota_tracker,
                db,
                route_for_stream.clone(),
                start,
            );

            while let Some(next_chunk) = upstream.next().await {
                match next_chunk {
                    Ok(mut chunk) => {
                        if let Some(chunk_total) = chunk.total_tokens() {
                            audit.total_tokens = chunk_total;
                        }
                        chunk.normalize(&route_for_stream.public_model_id, created);
                        yield chunk;
                    }
                    Err(error) => {
                        audit.finish(502);
                        Err(error)?;
                    }
                }
            }

            audit.finish(200);
        };

        Ok(Box::pin(stream))
    }

    pub async fn complete_stream(
        &self,
        req: &CompletionRequest,
        task_hint: &str,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<CompletionChunk>>> {
        let route = self.route_completion(req, task_hint).await?;
        self.reserve_request(&route)?;
        let backend = self
            .backends
            .get(&route.provider_id)
            .ok_or_else(|| anyhow::anyhow!("Inference backend disappeared during routing"))?;
        let credential = self.backend_credentials(backend)?;

        let mut provider_req = req.clone();
        provider_req.model = route.provider_model_id.clone();
        provider_req.stream = true;

        let start = std::time::Instant::now();
        let upstream = match backend
            .complete_stream(&provider_req, credential.as_credentials())
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                self.log_request(
                    &route.provider_id,
                    &route.public_model_id,
                    0,
                    elapsed_millis(start),
                    request_error_status(&error),
                );
                return Err(error);
            }
        };
        let quota_tracker = self.quota_tracker.clone();
        let db = self.db.clone();
        let route_for_stream = route.clone();
        let created = current_unix_timestamp();

        let stream = try_stream! {
            let mut upstream = upstream;
            let mut audit = StreamAudit::new(
                quota_tracker,
                db,
                route_for_stream.clone(),
                start,
            );

            while let Some(next_chunk) = upstream.next().await {
                match next_chunk {
                    Ok(mut chunk) => {
                        if let Some(chunk_total) = chunk.total_tokens() {
                            audit.total_tokens = chunk_total;
                        }
                        chunk.normalize(&route_for_stream.public_model_id, created);
                        yield chunk;
                    }
                    Err(error) => {
                        audit.finish(502);
                        Err(error)?;
                    }
                }
            }

            audit.finish(200);
        };

        Ok(Box::pin(stream))
    }

    fn backend_readiness(
        &self,
        backend: Option<&dyn InferenceBackend>,
    ) -> anyhow::Result<BackendReadiness> {
        let Some(backend) = backend else {
            return Ok(BackendReadiness::Unavailable);
        };
        let runtime = backend.runtime_readiness();
        if !runtime.is_ready() {
            return Ok(runtime);
        }
        if backend.credential_requirement() == CredentialRequirement::ApiKey
            && !self.db.has_api_key(backend.id())?
        {
            return Ok(BackendReadiness::MissingCredential);
        }
        Ok(BackendReadiness::Ready)
    }

    fn backend_credentials(
        &self,
        backend: &dyn InferenceBackend,
    ) -> anyhow::Result<ResolvedBackendCredentials> {
        match backend.credential_requirement() {
            CredentialRequirement::NotRequired => Ok(ResolvedBackendCredentials::None),
            CredentialRequirement::ApiKey => self
                .db
                .get_api_key(backend.id())?
                .map(ResolvedBackendCredentials::ApiKey)
                .ok_or_else(|| anyhow::anyhow!("No API key found for backend {}", backend.id())),
        }
    }

    fn route_score(
        &self,
        entry: &ModelCatalogEntry,
        task_hint: &str,
        headroom: f64,
        provider_summary: Option<&ProviderLogSummary>,
    ) -> f64 {
        let eval_scores = self.eval_store.get_score(&entry.public_model_id);
        let quality_score = match task_hint {
            "coding" => eval_scores.coding,
            "reasoning" => eval_scores.reasoning,
            "creative" => eval_scores.creative,
            _ => eval_scores.general,
        };
        let declared_capabilities = entry.capabilities.len()
            + usize::from(entry.chat_completions)
            + usize::from(entry.text_completions.is_some());
        let capability_score = declared_capabilities as f64 / (Capability::ALL.len() + 2) as f64;
        let latency_score = observed_latency_score(provider_summary);
        let headroom_score = if entry.quota.has_documented_limit() {
            headroom
        } else {
            0.5
        };
        (0.35 * headroom_score)
            + (0.30 * quality_score)
            + (0.20 * capability_score)
            + (0.15 * latency_score)
    }

    fn reserve_request(&self, route: &RouteResult) -> anyhow::Result<()> {
        if !route.quota_tracked {
            return Ok(());
        }
        let now = Utc::now();
        if !self
            .quota_tracker
            .try_record_request(&route.provider_id, &route.provider_model_id, now)
        {
            return Err(anyhow::anyhow!(
                "Quota was exhausted while reserving '{}'; retry after the active window resets.",
                route.public_model_id
            ));
        }

        if let Err(error) = self.db.record_quota_event(
            &route.provider_id,
            &route.provider_model_id,
            now.timestamp(),
            1,
            0,
        ) {
            warn!("Could not persist request quota event: {error}");
        }
        Ok(())
    }

    fn record_tokens(&self, route: &RouteResult, tokens: u32) {
        record_tokens(&self.quota_tracker, &self.db, route, tokens);
    }

    fn log_request(
        &self,
        provider_id: &str,
        public_model_id: &str,
        tokens: u32,
        latency_ms: u64,
        status: i32,
    ) {
        if let Err(error) =
            self.db
                .log_request(provider_id, public_model_id, tokens, latency_ms, status)
        {
            warn!("Could not persist request log: {error}");
        }
    }
}

enum ResolvedBackendCredentials {
    None,
    ApiKey(String),
}

impl ResolvedBackendCredentials {
    fn as_credentials(&self) -> BackendCredentials<'_> {
        match self {
            Self::None => BackendCredentials::None,
            Self::ApiKey(value) => BackendCredentials::ApiKey(value),
        }
    }
}

struct StreamAudit {
    quota_tracker: Arc<QuotaTracker>,
    db: Arc<Database>,
    route: RouteResult,
    started_at: std::time::Instant,
    total_tokens: u32,
    finished: bool,
}

impl StreamAudit {
    fn new(
        quota_tracker: Arc<QuotaTracker>,
        db: Arc<Database>,
        route: RouteResult,
        started_at: std::time::Instant,
    ) -> Self {
        Self {
            quota_tracker,
            db,
            route,
            started_at,
            total_tokens: 0,
            finished: false,
        }
    }

    fn finish(&mut self, status: i32) {
        if self.finished {
            return;
        }
        if self.total_tokens > 0 {
            record_tokens(
                &self.quota_tracker,
                &self.db,
                &self.route,
                self.total_tokens,
            );
        }
        if let Err(error) = self.db.log_request(
            &self.route.provider_id,
            &self.route.public_model_id,
            self.total_tokens,
            elapsed_millis(self.started_at),
            status,
        ) {
            warn!("Could not persist streaming request log: {error}");
        }
        self.finished = true;
    }
}

impl Drop for StreamAudit {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(499);
        }
    }
}

fn record_tokens(quota_tracker: &QuotaTracker, db: &Database, route: &RouteResult, tokens: u32) {
    if tokens == 0 || !route.quota_tracked {
        return;
    }
    let now = Utc::now();
    quota_tracker.record_tokens(&route.provider_id, &route.provider_model_id, tokens, now);
    if let Err(error) = db.record_quota_event(
        &route.provider_id,
        &route.provider_model_id,
        now.timestamp(),
        0,
        tokens,
    ) {
        warn!("Could not persist token quota event: {error}");
    }
}

fn request_error_status(error: &anyhow::Error) -> i32 {
    crate::providers::openai_compatible::upstream_http_status(error)
        .map(i32::from)
        .unwrap_or_else(|| {
            if crate::providers::openai_compatible::is_transport_timeout(error) {
                504
            } else {
                502
            }
        })
}

fn validate_model_route(
    backend: &dyn InferenceBackend,
    entry: &ModelCatalogEntry,
) -> anyhow::Result<()> {
    if entry.provider_id != backend.id() {
        return Err(anyhow::anyhow!(
            "Model route '{}' belongs to backend '{}', not '{}'.",
            entry.provider_model_id,
            entry.provider_id,
            backend.id()
        ));
    }
    for (field, value) in [
        ("public_model_id", entry.public_model_id.as_str()),
        ("provider_model_id", entry.provider_model_id.as_str()),
        ("display_name", entry.display_name.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "Model route for backend '{}' has an empty {field}.",
                backend.id()
            ));
        }
    }
    if !entry.chat_completions && entry.text_completions.is_none() {
        return Err(anyhow::anyhow!(
            "Model route '{}/{}' declares no generation surface.",
            entry.provider_id,
            entry.provider_model_id
        ));
    }
    if entry
        .capabilities
        .iter()
        .any(|capability| !backend.capabilities().contains(capability))
    {
        return Err(anyhow::anyhow!(
            "Model route '{}/{}' declares a capability its backend does not support.",
            entry.provider_id,
            entry.provider_model_id
        ));
    }
    if let Some(completion) = &entry.text_completions {
        if !backend.supports_completions() {
            return Err(anyhow::anyhow!(
                "Model route '{}/{}' declares text completions but its backend does not support them.",
                entry.provider_id,
                entry.provider_model_id
            ));
        }
        if completion.prompt_types.is_empty() {
            return Err(anyhow::anyhow!(
                "Model route '{}/{}' declares text completions without a supported prompt type.",
                entry.provider_id,
                entry.provider_model_id
            ));
        }
    }
    Ok(())
}

fn provider_status(
    readiness: BackendReadiness,
    headroom: Option<f64>,
    summary: &ProviderLogSummary,
) -> String {
    if !readiness.is_ready() {
        readiness.status().to_string()
    } else if headroom.is_some_and(|value| value <= 0.0) || summary.last_status_code == Some(429) {
        "quota_exhausted".to_string()
    } else if summary
        .last_status_code
        .map(|status| status >= 400)
        .unwrap_or(false)
    {
        "upstream_error".to_string()
    } else {
        "ready".to_string()
    }
}

fn observed_latency_score(summary: Option<&ProviderLogSummary>) -> f64 {
    let Some(summary) = summary.filter(|summary| summary.request_count > 0) else {
        return 0.5;
    };
    1_000.0 / (1_000.0 + summary.avg_latency_ms as f64)
}

fn validate_chat_request(req: &ChatRequest) -> anyhow::Result<()> {
    if req.model.trim().is_empty() {
        return Err(anyhow::anyhow!("model is required"));
    }
    if req.messages.is_empty() {
        return Err(anyhow::anyhow!("messages must contain at least one item"));
    }
    if req.messages.len() > 1_000 {
        return Err(anyhow::anyhow!("messages exceeds the limit of 1000 items"));
    }
    for (index, message) in req.messages.iter().enumerate() {
        if !matches!(
            message.role.as_str(),
            "system" | "developer" | "user" | "assistant" | "tool"
        ) {
            return Err(anyhow::anyhow!(
                "messages[{index}].role '{}' is unsupported",
                message.role
            ));
        }
        if message.content.is_null() && message.extra.get("tool_calls").is_none() {
            return Err(anyhow::anyhow!(
                "messages[{index}].content must not be null"
            ));
        }
    }
    if let Some(temperature) = req.temperature
        && (!temperature.is_finite() || !(0.0..=2.0).contains(&temperature))
    {
        return Err(anyhow::anyhow!("temperature must be between 0 and 2"));
    }
    if req.max_tokens == Some(0) {
        return Err(anyhow::anyhow!("max_tokens must be greater than zero"));
    }
    Ok(())
}

fn validate_completion_request(req: &CompletionRequest) -> anyhow::Result<()> {
    if req.model.trim().is_empty() {
        return Err(anyhow::anyhow!("model is required"));
    }
    let prompt_count = req.prompt.item_count();
    if prompt_count == 0 {
        return Err(anyhow::anyhow!("prompt must contain at least one item"));
    }
    if prompt_count > 2_048 {
        return Err(anyhow::anyhow!("prompt exceeds the limit of 2048 items"));
    }
    if let Some(temperature) = req.temperature
        && (!temperature.is_finite() || !(0.0..=2.0).contains(&temperature))
    {
        return Err(anyhow::anyhow!("temperature must be between 0 and 2"));
    }
    if req.max_tokens == Some(0) {
        return Err(anyhow::anyhow!("max_tokens must be greater than zero"));
    }
    if req.stream_options.is_some() && !req.stream {
        return Err(anyhow::anyhow!(
            "stream_options may only be set when stream is true"
        ));
    }
    for parameter in ["n", "best_of"] {
        if let Some(value) = req.extra.get(parameter).filter(|value| !value.is_null()) {
            let number = value
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("{parameter} must be a positive integer"))?;
            if number == 0 {
                return Err(anyhow::anyhow!("{parameter} must be a positive integer"));
            }
        }
    }
    Ok(())
}

fn required_capabilities(req: &ChatRequest) -> Vec<Capability> {
    let mut required = Vec::new();
    if req.stream {
        required.push(Capability::Streaming);
    }
    if req
        .extra
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
    {
        required.push(Capability::Tools);
    }
    if req
        .messages
        .iter()
        .any(|message| value_requires_vision(&message.content))
    {
        required.push(Capability::Vision);
    }
    required
}

fn value_requires_vision(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(value_requires_vision),
        Value::Object(object) => {
            matches!(
                object.get("type").and_then(Value::as_str),
                Some("image" | "image_url" | "input_image" | "document")
            ) || object.contains_key("image_url")
                || object.contains_key("inlineData")
                || object.contains_key("fileData")
                || object.values().any(value_requires_vision)
        }
        _ => false,
    }
}

fn elapsed_millis(started_at: std::time::Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn current_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::{StreamExt, stream::BoxStream};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DATABASE_ID: AtomicU64 = AtomicU64::new(1);

    use crate::catalog::{PromptSemantics, QuotaSpec, TextCompletionSupport};
    use crate::providers::{
        ChatChoice, ChatChunk, ChatMessage, ChatUsage, CompletionChoice, CompletionPrompt,
        CompletionPromptKind, CompletionUsage,
    };

    #[tokio::test]
    async fn route_selects_configured_catalog_provider() {
        let (_db, _quota, mut router) = test_router(vec![
            test_catalog_entry("unkeyed", "test-model", "provider-a-model", 0.5),
            test_catalog_entry("keyed", "test-model", "provider-b-model", 0.9),
        ]);
        router
            .add_backend(Box::new(MockProvider::new("unkeyed")))
            .unwrap();
        router
            .add_backend(Box::new(MockProvider::new("keyed")))
            .unwrap();
        router.db.save_api_key("keyed", "test-key").unwrap();

        let route = router
            .route(&chat_request("test-model"), "general")
            .await
            .unwrap();

        assert_eq!(route.provider_id, "keyed");
        assert_eq!(route.provider_model_id, "provider-b-model");
        assert_eq!(route.public_model_id, "test-model");
    }

    #[tokio::test]
    async fn credentialless_local_backend_registers_models_and_serves_raw_completions() {
        let seen_credentials = Arc::new(Mutex::new(Vec::new()));
        let (db, _quota, mut router) = test_router(Vec::new());
        router
            .add_backend(Box::new(MockLocalCompletionBackend {
                readiness: BackendReadiness::Ready,
                seen_credentials: seen_credentials.clone(),
            }))
            .unwrap();
        router
            .add_model_routes(
                "llama-native",
                vec![local_completion_catalog_entry("local/test-model")],
            )
            .unwrap();

        let response = router
            .complete(&completion_request("local/test-model"), "general")
            .await
            .unwrap();

        assert_eq!(response.model.as_deref(), Some("local/test-model"));
        assert_eq!(seen_credentials.lock().unwrap().as_slice(), ["none"]);
        let status = router.provider_statuses().unwrap().remove(0);
        assert_eq!(status.backend_kind, BackendKind::LocalEmbedded);
        assert!(!status.credential_required);
        assert!(status.configured);
        assert_eq!(status.status, "ready");
        assert!(db.get_quota_events_since(0).unwrap().is_empty());
    }

    #[tokio::test]
    async fn local_backend_runtime_readiness_is_independent_of_credentials() {
        let (_db, _quota, mut router) = test_router(Vec::new());
        router
            .add_backend(Box::new(MockLocalCompletionBackend {
                readiness: BackendReadiness::Loading,
                seen_credentials: Arc::new(Mutex::new(Vec::new())),
            }))
            .unwrap();
        router
            .add_model_routes(
                "llama-native",
                vec![local_completion_catalog_entry("local/test-model")],
            )
            .unwrap();

        let error = router
            .route_completion(&completion_request("local/test-model"), "general")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("backend") && error.contains("ready"));
        let status = router.provider_statuses().unwrap().remove(0);
        assert_eq!(status.status, "loading");
        assert!(status.configured);
    }

    #[test]
    fn runtime_model_routes_must_match_a_registered_backend() {
        let (_db, _quota, mut router) = test_router(Vec::new());
        let error = router
            .add_model_routes(
                "llama-native",
                vec![local_completion_catalog_entry("local/test-model")],
            )
            .unwrap_err()
            .to_string();

        assert!(error.contains("unregistered backend"));
    }

    #[tokio::test]
    async fn route_carries_catalog_parameter_policy() {
        let mut entry = test_catalog_entry("provider", "test-model", "provider-model", 0.9);
        entry.parameter_policy = ParameterPolicy::mistral();
        let (_db, _quota, mut router) = test_router(vec![entry]);
        router
            .add_backend(Box::new(MockProvider::new("provider")))
            .unwrap();
        router.db.save_api_key("provider", "test-key").unwrap();

        let route = router
            .route(&chat_request("test-model"), "general")
            .await
            .unwrap();

        assert!(
            route
                .parameter_policy
                .rename_parameters
                .iter()
                .any(|rename| rename.from == "seed" && rename.to == "random_seed")
        );
    }

    #[tokio::test]
    async fn chat_rewrites_model_for_provider_and_logs_public_model() {
        let sent_models = Arc::new(Mutex::new(Vec::new()));
        let (db, _quota, mut router) = test_router(vec![test_catalog_entry(
            "provider",
            "public-model",
            "provider-model",
            0.8,
        )]);
        router
            .add_backend(Box::new(MockProvider::with_sent_models(
                "provider",
                sent_models.clone(),
            )))
            .unwrap();
        db.save_api_key("provider", "test-key").unwrap();

        let response = router
            .chat(&chat_request("public-model"), "general")
            .await
            .unwrap();

        assert_eq!(response.model.as_deref(), Some("public-model"));
        assert_eq!(sent_models.lock().unwrap().as_slice(), ["provider-model"]);

        let logs = db.get_recent_logs(10).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].provider_id, "provider");
        assert_eq!(logs[0].model_id, "public-model");
        assert_eq!(logs[0].tokens_used, 11);
        assert_eq!(logs[0].status_code, 200);
    }

    #[tokio::test]
    async fn chat_stream_normalizes_chunks_and_logs_when_drained() {
        let sent_models = Arc::new(Mutex::new(Vec::new()));
        let (db, _quota, mut router) = test_router(vec![ModelCatalogEntry {
            capabilities: vec![Capability::Streaming],
            ..test_catalog_entry("provider", "public-model", "provider-model", 0.8)
        }]);
        router
            .add_backend(Box::new(MockProvider::with_stream(
                "provider",
                sent_models.clone(),
                vec![ChatChunk {
                    id: "chunk-test".to_string(),
                    object: None,
                    created: None,
                    model: None,
                    choices: Vec::new(),
                    usage: Some(ChatUsage {
                        prompt_tokens: 3,
                        completion_tokens: 4,
                        total_tokens: 7,
                    }),
                }],
            )))
            .unwrap();
        db.save_api_key("provider", "test-key").unwrap();

        let mut request = chat_request("public-model");
        request.stream = true;

        let chunks: Vec<ChatChunk> = router
            .chat_stream(&request, "general")
            .await
            .unwrap()
            .map(|result| result.unwrap())
            .collect()
            .await;

        assert_eq!(sent_models.lock().unwrap().as_slice(), ["provider-model"]);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].model.as_deref(), Some("public-model"));
        assert_eq!(chunks[0].object.as_deref(), Some("chat.completion.chunk"));

        let logs = db.get_recent_logs(10).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].model_id, "public-model");
        assert_eq!(logs[0].tokens_used, 7);
        assert_eq!(logs[0].status_code, 200);
    }

    #[tokio::test]
    async fn completion_route_excludes_chat_only_models() {
        let chat_only = test_catalog_entry("chat", "shared-model", "chat-model", 0.8);
        let completion_entry =
            test_completion_catalog_entry("completion", "shared-model", "completion-model");
        let (db, _quota, mut router) = test_router(vec![chat_only, completion_entry]);
        router
            .add_backend(Box::new(MockProvider::new("chat")))
            .unwrap();
        router
            .add_backend(Box::new(MockCompletionProvider::new(
                "completion",
                Arc::new(Mutex::new(Vec::new())),
                Vec::new(),
            )))
            .unwrap();
        db.save_api_key("chat", "test-key").unwrap();
        db.save_api_key("completion", "test-key").unwrap();

        let route = router
            .route_completion(&completion_request("shared-model"), "general")
            .await
            .unwrap();

        assert_eq!(route.provider_id, "completion");
        assert_eq!(route.provider_model_id, "completion-model");
    }

    #[tokio::test]
    async fn completion_preserves_token_batches_and_all_choices() {
        let sent_requests = Arc::new(Mutex::new(Vec::new()));
        let (db, _quota, mut router) = test_router(vec![test_completion_catalog_entry(
            "completion",
            "public-model",
            "provider-model",
        )]);
        router
            .add_backend(Box::new(MockCompletionProvider::new(
                "completion",
                sent_requests.clone(),
                Vec::new(),
            )))
            .unwrap();
        db.save_api_key("completion", "test-key").unwrap();
        let mut request = completion_request("public-model");
        request.prompt = CompletionPrompt::TokenBatches(vec![vec![1, 2], vec![3, 4]]);

        let response = router.complete(&request, "general").await.unwrap();

        let sent = sent_requests.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].model, "provider-model");
        assert_eq!(sent[0].prompt, request.prompt);
        assert_eq!(response.model.as_deref(), Some("public-model"));
        assert_eq!(response.object.as_deref(), Some("text_completion"));
        assert_eq!(response.choices.len(), 2);
        assert!(response.choices[0].logprobs.is_some());
    }

    #[tokio::test]
    async fn completion_route_rejects_unsupported_parameters() {
        let mut entry =
            test_completion_catalog_entry("completion", "public-model", "provider-model");
        entry
            .text_completions
            .as_mut()
            .unwrap()
            .supported_parameters = vec!["max_tokens".to_string()];
        let (db, _quota, mut router) = test_router(vec![entry]);
        router
            .add_backend(Box::new(MockCompletionProvider::new(
                "completion",
                Arc::new(Mutex::new(Vec::new())),
                Vec::new(),
            )))
            .unwrap();
        db.save_api_key("completion", "test-key").unwrap();
        let mut request = completion_request("public-model");
        request.extra.insert("n".to_string(), serde_json::json!(2));

        let error = router
            .route_completion(&request, "general")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("accepts: n"));
    }

    #[tokio::test]
    async fn completion_stream_normalizes_chunks_and_records_usage() {
        let sent_requests = Arc::new(Mutex::new(Vec::new()));
        let (db, _quota, mut router) = test_router(vec![test_completion_catalog_entry(
            "completion",
            "public-model",
            "provider-model",
        )]);
        router
            .add_backend(Box::new(MockCompletionProvider::new(
                "completion",
                sent_requests.clone(),
                vec![completion_response("stream-1")],
            )))
            .unwrap();
        db.save_api_key("completion", "test-key").unwrap();
        let mut request = completion_request("public-model");
        request.stream = true;

        let chunks: Vec<CompletionChunk> = router
            .complete_stream(&request, "general")
            .await
            .unwrap()
            .map(|result| result.unwrap())
            .collect()
            .await;

        assert_eq!(sent_requests.lock().unwrap()[0].model, "provider-model");
        assert_eq!(chunks[0].model.as_deref(), Some("public-model"));
        assert_eq!(chunks[0].object.as_deref(), Some("text_completion"));
        assert_eq!(db.get_recent_logs(10).unwrap()[0].tokens_used, 5);
    }

    #[tokio::test]
    async fn route_rejects_exhausted_quota() {
        let (db, quota, mut router) = test_router(vec![ModelCatalogEntry {
            quota: QuotaSpec {
                rpm: 1,
                rpd: 1,
                tpm: 1,
                tpd: 1,
                documented: true,
            },
            ..test_catalog_entry("provider", "public-model", "provider-model", 0.8)
        }]);
        router
            .add_backend(Box::new(MockProvider::new("provider")))
            .unwrap();
        db.save_api_key("provider", "test-key").unwrap();
        assert!(quota.try_record_request("provider", "provider-model", Utc::now()));

        let error = router
            .route(&chat_request("public-model"), "general")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("out of quota") || error.contains("unavailable"));
    }

    #[test]
    fn persisted_quota_events_are_restored_on_startup() {
        let path = test_database_path("quota-restore");
        let db = Arc::new(Database::new(path).unwrap());
        let entry = test_catalog_entry("provider", "public-model", "provider-model", 0.8);
        db.record_quota_event("provider", "provider-model", Utc::now().timestamp(), 1, 0)
            .unwrap();
        let quota = Arc::new(QuotaTracker::new());

        let _router =
            Router::with_catalog(quota.clone(), Arc::new(EvalStore::new()), db, vec![entry]);

        let headroom = quota.headroom("provider", "provider-model");
        assert!((headroom - 0.9).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn route_rejects_tools_on_a_model_without_tool_capability() {
        let (db, _quota, mut router) = test_router(vec![ModelCatalogEntry {
            capabilities: vec![Capability::Streaming],
            ..test_catalog_entry("provider", "public-model", "provider-model", 0.8)
        }]);
        router
            .add_backend(Box::new(MockProvider::new("provider")))
            .unwrap();
        db.save_api_key("provider", "test-key").unwrap();
        let mut request = chat_request("public-model");
        request.extra.insert(
            "tools".to_string(),
            serde_json::json!([{"type": "function"}]),
        );

        let error = router
            .route(&request, "general")
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("requested capability"));
    }

    #[tokio::test]
    async fn dropped_stream_is_logged_as_client_disconnect() {
        let (db, _quota, mut router) = test_router(vec![ModelCatalogEntry {
            capabilities: vec![Capability::Streaming],
            ..test_catalog_entry("provider", "public-model", "provider-model", 0.8)
        }]);
        router
            .add_backend(Box::new(MockProvider::with_stream(
                "provider",
                Arc::new(Mutex::new(Vec::new())),
                vec![ChatChunk {
                    id: "chunk-test".to_string(),
                    object: None,
                    created: None,
                    model: None,
                    choices: Vec::new(),
                    usage: None,
                }],
            )))
            .unwrap();
        db.save_api_key("provider", "test-key").unwrap();
        let mut request = chat_request("public-model");
        request.stream = true;

        let mut stream = router.chat_stream(&request, "general").await.unwrap();
        assert!(stream.next().await.is_some());
        drop(stream);

        let logs = db.get_recent_logs(10).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].status_code, 499);
    }

    fn test_router(catalog: Vec<ModelCatalogEntry>) -> (Arc<Database>, Arc<QuotaTracker>, Router) {
        let path = test_database_path("router");
        let _ = std::fs::remove_file(&path);

        let db = Arc::new(Database::new(path).unwrap());
        let quota = Arc::new(QuotaTracker::new());
        let router = Router::with_catalog(
            quota.clone(),
            Arc::new(EvalStore::new()),
            db.clone(),
            catalog,
        );
        (db, quota, router)
    }

    fn test_database_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "free-token-energy-{label}-test-{}-{}-{}.sqlite",
            std::process::id(),
            TEST_DATABASE_ID.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn test_catalog_entry(
        provider_id: &str,
        public_model_id: &str,
        provider_model_id: &str,
        _latency_score: f64,
    ) -> ModelCatalogEntry {
        ModelCatalogEntry {
            provider_id: provider_id.to_string(),
            provider_name: provider_id.to_string(),
            public_model_id: public_model_id.to_string(),
            provider_model_id: provider_model_id.to_string(),
            display_name: public_model_id.to_string(),
            capabilities: vec![Capability::Tools],
            chat_completions: true,
            text_completions: None,
            parameter_policy: ParameterPolicy::openai_compatible(),
            quota: QuotaSpec {
                rpm: 10,
                rpd: 100,
                tpm: 1000,
                tpd: 10000,
                documented: true,
            },
        }
    }

    fn test_completion_catalog_entry(
        provider_id: &str,
        public_model_id: &str,
        provider_model_id: &str,
    ) -> ModelCatalogEntry {
        ModelCatalogEntry {
            capabilities: vec![Capability::Streaming],
            chat_completions: false,
            text_completions: Some(TextCompletionSupport {
                prompt_semantics: PromptSemantics::DirectContinuation,
                prompt_types: vec![
                    CompletionPromptKind::Text,
                    CompletionPromptKind::Texts,
                    CompletionPromptKind::Tokens,
                    CompletionPromptKind::TokenBatches,
                ],
                supported_parameters: vec![
                    "stream".to_string(),
                    "temperature".to_string(),
                    "max_tokens".to_string(),
                    "n".to_string(),
                ],
            }),
            ..test_catalog_entry(provider_id, public_model_id, provider_model_id, 0.8)
        }
    }

    fn local_completion_catalog_entry(public_model_id: &str) -> ModelCatalogEntry {
        ModelCatalogEntry {
            provider_id: "llama-native".to_string(),
            provider_name: "Local llama.cpp".to_string(),
            public_model_id: public_model_id.to_string(),
            provider_model_id: "model-fingerprint".to_string(),
            display_name: "Selected local GGUF".to_string(),
            capabilities: Vec::new(),
            chat_completions: false,
            text_completions: Some(TextCompletionSupport {
                prompt_semantics: PromptSemantics::DirectContinuation,
                prompt_types: vec![CompletionPromptKind::Text, CompletionPromptKind::Tokens],
                supported_parameters: vec!["temperature".to_string(), "max_tokens".to_string()],
            }),
            parameter_policy: ParameterPolicy::openai_compatible(),
            quota: QuotaSpec {
                rpm: u32::MAX,
                rpd: u32::MAX,
                tpm: u32::MAX,
                tpd: u32::MAX,
                documented: false,
            },
        }
    }

    fn chat_request(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.to_string(),
            messages: vec![ChatMessage::text("user", "hello")],
            stream: false,
            stream_options: None,
            temperature: None,
            max_tokens: None,
            extra: serde_json::Map::new(),
        }
    }

    fn completion_request(model: &str) -> CompletionRequest {
        CompletionRequest {
            model: model.to_string(),
            prompt: CompletionPrompt::Text("Once upon a time".to_string()),
            stream: false,
            stream_options: None,
            temperature: None,
            max_tokens: Some(32),
            extra: serde_json::Map::new(),
        }
    }

    fn completion_response(id: &str) -> CompletionResponse {
        CompletionResponse {
            id: id.to_string(),
            object: None,
            created: None,
            model: None,
            choices: vec![
                CompletionChoice {
                    text: " continued".to_string(),
                    index: 0,
                    logprobs: Some(serde_json::json!({"tokens": [" continued"]})),
                    finish_reason: Some("stop".to_string()),
                    extra: serde_json::Map::new(),
                },
                CompletionChoice {
                    text: " alternative".to_string(),
                    index: 1,
                    logprobs: None,
                    finish_reason: Some("length".to_string()),
                    extra: serde_json::Map::new(),
                },
            ],
            usage: Some(CompletionUsage {
                prompt_tokens: 2,
                completion_tokens: 3,
                total_tokens: 5,
                extra: serde_json::Map::new(),
            }),
            extra: serde_json::Map::new(),
        }
    }

    struct MockProvider {
        id: &'static str,
        sent_models: Arc<Mutex<Vec<String>>>,
        stream_chunks: Vec<ChatChunk>,
    }

    impl MockProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                sent_models: Arc::new(Mutex::new(Vec::new())),
                stream_chunks: Vec::new(),
            }
        }

        fn with_sent_models(id: &'static str, sent_models: Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                id,
                sent_models,
                stream_chunks: Vec::new(),
            }
        }

        fn with_stream(
            id: &'static str,
            sent_models: Arc<Mutex<Vec<String>>>,
            stream_chunks: Vec<ChatChunk>,
        ) -> Self {
            Self {
                id,
                sent_models,
                stream_chunks,
            }
        }
    }

    #[async_trait]
    impl InferenceBackend for MockProvider {
        fn id(&self) -> &str {
            self.id
        }

        fn name(&self) -> &str {
            self.id
        }

        fn capabilities(&self) -> &[Capability] {
            &[Capability::Streaming, Capability::Tools]
        }

        async fn chat(
            &self,
            req: &ChatRequest,
            _credentials: BackendCredentials<'_>,
            _policy: &ParameterPolicy,
        ) -> anyhow::Result<ChatResponse> {
            self.sent_models.lock().unwrap().push(req.model.clone());
            Ok(ChatResponse {
                id: "chatcmpl-test".to_string(),
                object: None,
                created: None,
                model: None,
                choices: vec![ChatChoice {
                    message: ChatMessage::text("assistant", "ok"),
                    finish_reason: Some("stop".to_string()),
                }],
                usage: Some(ChatUsage {
                    prompt_tokens: 5,
                    completion_tokens: 6,
                    total_tokens: 11,
                }),
            })
        }

        async fn chat_stream(
            &self,
            req: &ChatRequest,
            _credentials: BackendCredentials<'_>,
            _policy: &ParameterPolicy,
        ) -> anyhow::Result<BoxStream<'static, anyhow::Result<ChatChunk>>> {
            self.sent_models.lock().unwrap().push(req.model.clone());
            Ok(Box::pin(futures::stream::iter(
                self.stream_chunks.clone().into_iter().map(Ok),
            )))
        }
    }

    struct MockCompletionProvider {
        id: &'static str,
        sent_requests: Arc<Mutex<Vec<CompletionRequest>>>,
        stream_chunks: Vec<CompletionChunk>,
    }

    struct MockLocalCompletionBackend {
        readiness: BackendReadiness,
        seen_credentials: Arc<Mutex<Vec<&'static str>>>,
    }

    #[async_trait]
    impl InferenceBackend for MockLocalCompletionBackend {
        fn id(&self) -> &str {
            "llama-native"
        }

        fn name(&self) -> &str {
            "Local llama.cpp"
        }

        fn capabilities(&self) -> &[Capability] {
            &[]
        }

        fn kind(&self) -> BackendKind {
            BackendKind::LocalEmbedded
        }

        fn credential_requirement(&self) -> CredentialRequirement {
            CredentialRequirement::NotRequired
        }

        fn runtime_readiness(&self) -> BackendReadiness {
            self.readiness
        }

        async fn chat(
            &self,
            _req: &ChatRequest,
            _credentials: BackendCredentials<'_>,
            _policy: &ParameterPolicy,
        ) -> anyhow::Result<ChatResponse> {
            Err(anyhow::anyhow!("chat is not supported by this fixture"))
        }

        async fn chat_stream(
            &self,
            _req: &ChatRequest,
            _credentials: BackendCredentials<'_>,
            _policy: &ParameterPolicy,
        ) -> anyhow::Result<BoxStream<'static, anyhow::Result<ChatChunk>>> {
            Err(anyhow::anyhow!("chat is not supported by this fixture"))
        }

        fn supports_completions(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: &CompletionRequest,
            credentials: BackendCredentials<'_>,
        ) -> anyhow::Result<CompletionResponse> {
            let credential_kind = match credentials {
                BackendCredentials::None => "none",
                BackendCredentials::ApiKey(_) => "api_key",
            };
            self.seen_credentials.lock().unwrap().push(credential_kind);
            Ok(completion_response("cmpl-local"))
        }
    }

    impl MockCompletionProvider {
        fn new(
            id: &'static str,
            sent_requests: Arc<Mutex<Vec<CompletionRequest>>>,
            stream_chunks: Vec<CompletionChunk>,
        ) -> Self {
            Self {
                id,
                sent_requests,
                stream_chunks,
            }
        }
    }

    #[async_trait]
    impl InferenceBackend for MockCompletionProvider {
        fn id(&self) -> &str {
            self.id
        }

        fn name(&self) -> &str {
            self.id
        }

        fn capabilities(&self) -> &[Capability] {
            &[Capability::Streaming]
        }

        async fn chat(
            &self,
            _req: &ChatRequest,
            _credentials: BackendCredentials<'_>,
            _policy: &ParameterPolicy,
        ) -> anyhow::Result<ChatResponse> {
            Err(anyhow::anyhow!("chat is not supported by this fixture"))
        }

        async fn chat_stream(
            &self,
            _req: &ChatRequest,
            _credentials: BackendCredentials<'_>,
            _policy: &ParameterPolicy,
        ) -> anyhow::Result<BoxStream<'static, anyhow::Result<ChatChunk>>> {
            Err(anyhow::anyhow!("chat is not supported by this fixture"))
        }

        fn supports_completions(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            req: &CompletionRequest,
            _credentials: BackendCredentials<'_>,
        ) -> anyhow::Result<CompletionResponse> {
            self.sent_requests.lock().unwrap().push(req.clone());
            Ok(completion_response("cmpl-test"))
        }

        async fn complete_stream(
            &self,
            req: &CompletionRequest,
            _credentials: BackendCredentials<'_>,
        ) -> anyhow::Result<BoxStream<'static, anyhow::Result<CompletionChunk>>> {
            self.sent_requests.lock().unwrap().push(req.clone());
            Ok(Box::pin(futures::stream::iter(
                self.stream_chunks.clone().into_iter().map(Ok),
            )))
        }
    }
}
