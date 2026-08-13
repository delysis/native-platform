use async_trait::async_trait;
use fte_store::SecretResolver;
use fte_types::{
    BackendDescriptor, BackendLocation, BackendReadiness, BackendRequest, CancelTarget,
    CompletionPrompt, ErrorClass, GatewayBackend, GatewayError, GatewayEvent, GatewayResponse,
    GatewayTicket, GatewayUsage, GenerationInput, ModelDescriptor, RequestId, ResolvedRoute,
    TerminalStatus, TicketCancellation,
};
use futures::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_ERROR_BODY_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedProtocol {
    OpenAi,
    OpenAiCompatible,
    Anthropic,
    Gemini,
}

#[derive(Debug, Clone)]
pub enum HostedAuth {
    Bearer,
    Header { name: String, prefix: String },
}

#[derive(Debug, Clone, Default)]
pub struct HostedEndpoints {
    pub responses: Option<String>,
    pub chat_completions: Option<String>,
    pub completions: Option<String>,
    pub messages: Option<String>,
    pub count_tokens: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HostedProviderConfig {
    pub id: String,
    pub display_name: String,
    pub protocol: HostedProtocol,
    pub secret_id: String,
    pub auth: HostedAuth,
    pub endpoints: HostedEndpoints,
    pub static_headers: BTreeMap<String, String>,
    pub models: Vec<ModelDescriptor>,
    pub catalog_version: String,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl HostedProviderConfig {
    #[must_use]
    pub fn openai(
        id: impl Into<String>,
        display_name: impl Into<String>,
        secret_id: impl Into<String>,
        models: Vec<ModelDescriptor>,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            protocol: HostedProtocol::OpenAi,
            secret_id: secret_id.into(),
            auth: HostedAuth::Bearer,
            endpoints: HostedEndpoints {
                responses: Some("https://api.openai.com/v1/responses".to_string()),
                chat_completions: Some("https://api.openai.com/v1/chat/completions".to_string()),
                completions: Some("https://api.openai.com/v1/completions".to_string()),
                messages: None,
                count_tokens: None,
            },
            static_headers: BTreeMap::new(),
            models,
            catalog_version: "configured".to_string(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(10 * 60),
        }
    }

    #[must_use]
    pub fn anthropic(
        id: impl Into<String>,
        display_name: impl Into<String>,
        secret_id: impl Into<String>,
        models: Vec<ModelDescriptor>,
    ) -> Self {
        let mut static_headers = BTreeMap::new();
        static_headers.insert("anthropic-version".to_string(), "2023-06-01".to_string());
        Self {
            id: id.into(),
            display_name: display_name.into(),
            protocol: HostedProtocol::Anthropic,
            secret_id: secret_id.into(),
            auth: HostedAuth::Header {
                name: "x-api-key".to_string(),
                prefix: String::new(),
            },
            endpoints: HostedEndpoints {
                messages: Some("https://api.anthropic.com/v1/messages".to_string()),
                count_tokens: Some(
                    "https://api.anthropic.com/v1/messages/count_tokens".to_string(),
                ),
                ..HostedEndpoints::default()
            },
            static_headers,
            models,
            catalog_version: "configured".to_string(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(10 * 60),
        }
    }

    #[must_use]
    pub fn openai_compatible(
        id: impl Into<String>,
        display_name: impl Into<String>,
        secret_id: impl Into<String>,
        chat_completions: impl Into<String>,
        models: Vec<ModelDescriptor>,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            protocol: HostedProtocol::OpenAiCompatible,
            secret_id: secret_id.into(),
            auth: HostedAuth::Bearer,
            endpoints: HostedEndpoints {
                chat_completions: Some(chat_completions.into()),
                ..HostedEndpoints::default()
            },
            static_headers: BTreeMap::new(),
            models,
            catalog_version: "configured".to_string(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(10 * 60),
        }
    }

    #[must_use]
    pub fn gemini(
        id: impl Into<String>,
        display_name: impl Into<String>,
        secret_id: impl Into<String>,
        models: Vec<ModelDescriptor>,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            protocol: HostedProtocol::Gemini,
            secret_id: secret_id.into(),
            auth: HostedAuth::Header {
                name: "x-goog-api-key".to_string(),
                prefix: String::new(),
            },
            endpoints: HostedEndpoints {
                messages: Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
                count_tokens: Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
                ..HostedEndpoints::default()
            },
            static_headers: BTreeMap::new(),
            models,
            catalog_version: "configured".to_string(),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(10 * 60),
        }
    }
}

pub struct HostedProviderBackend {
    config: HostedProviderConfig,
    client: reqwest::Client,
    secrets: Arc<dyn SecretResolver>,
    credential: Mutex<Option<String>>,
    activity: Arc<HostedActivity>,
}

struct HostedActivityState {
    accepting: bool,
    active: HashMap<RequestId, CancellationToken>,
}

impl Default for HostedActivityState {
    fn default() -> Self {
        Self {
            accepting: true,
            active: HashMap::new(),
        }
    }
}

#[derive(Default)]
struct HostedActivity {
    state: Mutex<HostedActivityState>,
    changed: Notify,
}

impl HostedActivity {
    fn register(self: &Arc<Self>, request_id: &RequestId) -> Result<HostedOperation, GatewayError> {
        let mut state = self.lock_state();
        if !state.accepting {
            return Err(GatewayError::unavailable(
                request_id,
                "provider_quiescing",
                "the hosted provider is draining and no longer accepts requests",
            ));
        }
        if state.active.contains_key(request_id) {
            return Err(GatewayError::invalid_request(
                request_id,
                "provider_request_id_active",
                "that request ID is already active on the hosted provider",
            ));
        }
        let cancellation = CancellationToken::new();
        state
            .active
            .insert(request_id.clone(), cancellation.clone());
        Ok(HostedOperation {
            activity: Arc::clone(self),
            request_id: request_id.clone(),
            cancellation,
        })
    }

    fn is_accepting(&self) -> bool {
        self.lock_state().accepting
    }

    fn cancel(&self, request_id: &RequestId) -> usize {
        self.lock_state()
            .active
            .get(request_id)
            .cloned()
            .map(|token| {
                token.cancel();
                1
            })
            .unwrap_or_default()
    }

    async fn shutdown(&self) -> Result<(), GatewayError> {
        let tokens = {
            let mut state = self.lock_state();
            state.accepting = false;
            state.active.values().cloned().collect::<Vec<_>>()
        };
        for token in tokens {
            token.cancel();
        }
        loop {
            let changed = self.changed.notified();
            let drained = self.lock_state().active.is_empty();
            if drained {
                return Ok(());
            }
            changed.await;
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, HostedActivityState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct HostedOperation {
    activity: Arc<HostedActivity>,
    request_id: RequestId,
    cancellation: CancellationToken,
}

impl HostedOperation {
    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

impl Drop for HostedOperation {
    fn drop(&mut self) {
        let mut state = self.activity.lock_state();
        state.active.remove(&self.request_id);
        self.activity.changed.notify_waiters();
    }
}

impl std::fmt::Debug for HostedProviderBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostedProviderBackend")
            .field("id", &self.config.id)
            .field("protocol", &self.config.protocol)
            .finish_non_exhaustive()
    }
}

impl HostedProviderBackend {
    pub fn new(
        config: HostedProviderConfig,
        secrets: Arc<dyn SecretResolver>,
    ) -> Result<Self, GatewayError> {
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .map_err(|error| provider_internal(&config.id, "provider_client_invalid", error))?;
        Ok(Self {
            config,
            client,
            secrets,
            credential: Mutex::new(None),
            activity: Arc::new(HostedActivity::default()),
        })
    }

    fn credential(&self, request_id: &RequestId) -> Result<String, GatewayError> {
        if let Some(secret) = self
            .credential
            .lock()
            .map_err(|error| {
                provider_internal(&self.config.id, "provider_credential_state_failed", error)
            })?
            .clone()
        {
            return Ok(secret);
        }
        let secret = self
            .secrets
            .resolve(&self.config.secret_id)?
            .filter(|secret| !secret.trim().is_empty())
            .ok_or_else(|| GatewayError {
                code: "provider_credential_missing".to_string(),
                class: ErrorClass::Authentication,
                retryable: false,
                http_status: 401,
                request_id: request_id.clone(),
                provider: Some(self.config.id.clone()),
                safe_detail: format!(
                    "{} credentials are not configured",
                    self.config.display_name
                ),
            })?;
        *self.credential.lock().map_err(|error| {
            provider_internal(&self.config.id, "provider_credential_state_failed", error)
        })? = Some(secret.clone());
        Ok(secret)
    }

    fn headers(
        &self,
        secret: &str,
        request: &fte_types::GatewayRequest,
    ) -> Result<HeaderMap, GatewayError> {
        let request_id = &request.request_id;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        match &self.config.auth {
            HostedAuth::Bearer => {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|error| {
                        provider_request_error(request_id, &self.config.id, error)
                    })?,
                );
            }
            HostedAuth::Header { name, prefix } => {
                let name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|error| provider_request_error(request_id, &self.config.id, error))?;
                let value = HeaderValue::from_str(&format!("{prefix}{secret}"))
                    .map_err(|error| provider_request_error(request_id, &self.config.id, error))?;
                headers.insert(name, value);
            }
        }
        for (name, value) in &self.config.static_headers {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|error| provider_request_error(request_id, &self.config.id, error))?,
                HeaderValue::from_str(value)
                    .map_err(|error| provider_request_error(request_id, &self.config.id, error))?,
            );
        }
        if self.config.protocol == HostedProtocol::Anthropic {
            apply_anthropic_request_headers(&mut headers, request)?;
        }
        Ok(headers)
    }

    fn prepare(&self, request: &BackendRequest) -> Result<PreparedHostedRequest, GatewayError> {
        match self.config.protocol {
            HostedProtocol::OpenAi => prepare_openai(&self.config, request),
            HostedProtocol::OpenAiCompatible => prepare_openai_compatible(&self.config, request),
            HostedProtocol::Anthropic => prepare_anthropic(&self.config, request),
            HostedProtocol::Gemini => prepare_gemini(&self.config, request),
        }
    }
}

#[async_trait]
impl GatewayBackend for HostedProviderBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: self.config.id.clone(),
            display_name: self.config.display_name.clone(),
            location: BackendLocation::Hosted,
            models: self.config.models.clone(),
        }
    }

    fn readiness(&self) -> BackendReadiness {
        if !self.activity.is_accepting() {
            return BackendReadiness::Unavailable {
                reason: "the hosted provider is draining".to_string(),
            };
        }
        match self.credential(&RequestId::new()) {
            Ok(_) => BackendReadiness::Ready,
            Err(error) => BackendReadiness::NotConfigured {
                reason: error.safe_detail,
            },
        }
    }

    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
        let request_id = request.request.request_id.clone();
        let secret = self.credential(&request_id)?;
        let prepared = self.prepare(&request)?;
        let operation = self.activity.register(&request_id)?;
        let token = operation.cancellation();
        let response = tokio::select! {
            () = token.cancelled() => {
                return Err(cancelled_error(&request_id, &self.config.id));
            }
            response = self.client
                .post(&prepared.url)
                .headers(self.headers(&secret, &request.request)?)
                .json(&prepared.body)
                .send() => response.map_err(|error| map_transport_error(&request_id, &self.config.id, error))?,
        };
        if !response.status().is_success() {
            return Err(tokio::select! {
                () = token.cancelled() => cancelled_error(&request_id, &self.config.id),
                error = map_http_error(&request_id, &self.config.id, response) => error,
            });
        }

        let capacity = request
            .request
            .stream
            .event_capacity
            .unwrap_or(fte_types::DEFAULT_EVENT_CAPACITY)
            .clamp(32, 4096);
        let (event_tx, event_rx) = mpsc::channel(capacity);
        let (final_tx, final_rx) = oneshot::channel();
        let terminal = Arc::new(AtomicBool::new(false));
        let response_id = format!("resp_{}", Uuid::new_v4());
        event_tx
            .try_send(GatewayEvent::ResponseCreated {
                request_id: request_id.clone(),
                response_id: response_id.clone(),
                route: request.route.clone(),
            })
            .map_err(|_| {
                GatewayError::unavailable(
                    &request_id,
                    "gateway_event_channel_closed",
                    "the event consumer closed before the hosted request started",
                )
            })?;
        let terminal_permit = event_tx.clone().reserve_owned().await.map_err(|_| {
            GatewayError::unavailable(
                &request_id,
                "gateway_event_channel_closed",
                "the event consumer closed before terminal capacity was reserved",
            )
        })?;

        let cancellation: Arc<dyn TicketCancellation> = Arc::new(HostedCancellation {
            token: token.clone(),
        });
        let protocol = prepared.protocol;
        let request_id_for_task = request_id.clone();
        let route = request.route;
        let backend_id_for_task = route.backend_id.clone();
        let previous_response_id = request.request.storage.previous_response_id;
        let terminal_for_task = Arc::clone(&terminal);
        tokio::spawn(async move {
            let _operation = operation;
            let result = if prepared.streaming {
                consume_provider_stream(
                    response,
                    ProviderStreamRequest {
                        protocol,
                        request_id: request_id_for_task.clone(),
                        response_id: response_id.clone(),
                        route,
                        previous_response_id,
                        events: event_tx.clone(),
                        cancellation: token.clone(),
                    },
                )
                .await
            } else {
                tokio::select! {
                    () = token.cancelled() => {
                        Err(cancelled_error(&request_id_for_task, &backend_id_for_task))
                    }
                    result = consume_provider_json(
                        response,
                        ProviderJsonRequest {
                            protocol,
                            request_id: request_id_for_task.clone(),
                            response_id,
                            route,
                            previous_response_id,
                            events: event_tx.clone(),
                            cancellation: token.clone(),
                        },
                    ) => result,
                }
            };
            let terminal_event = terminal_event(&request_id_for_task, &result);
            enqueue_reserved_terminal(terminal_permit, terminal_event, &terminal_for_task);
            let _ = final_tx.send(result);
        });

        Ok(GatewayTicket::new(
            request_id,
            event_rx,
            final_rx,
            cancellation,
            terminal,
        ))
    }

    async fn count_tokens(&self, request: BackendRequest) -> Result<GatewayUsage, GatewayError> {
        if !matches!(
            self.config.protocol,
            HostedProtocol::Anthropic | HostedProtocol::Gemini
        ) {
            return Err(capability_error(
                &request.request.request_id,
                &self.config.id,
                "hosted_token_count_unavailable",
                "this hosted provider has no exact token-count endpoint configured",
            ));
        }
        let request_id = request.request.request_id.clone();
        let secret = self.credential(&request_id)?;
        let operation = self.activity.register(&request_id)?;
        let cancellation = operation.cancellation();
        let (url, body) = match self.config.protocol {
            HostedProtocol::Anthropic => (
                required_endpoint(
                    &request,
                    self.config.endpoints.count_tokens.as_deref(),
                    "Anthropic token counting",
                )?,
                anthropic_count_body(&request)?,
            ),
            HostedProtocol::Gemini => (
                gemini_url(
                    &request,
                    self.config.endpoints.count_tokens.as_deref(),
                    "countTokens",
                    false,
                )?,
                gemini_count_body(&request)?,
            ),
            HostedProtocol::OpenAi | HostedProtocol::OpenAiCompatible => {
                return Err(capability_error(
                    &request_id,
                    &self.config.id,
                    "hosted_token_count_unavailable",
                    "this hosted provider has no exact token-count endpoint configured",
                ));
            }
        };
        let response = tokio::select! {
            () = cancellation.cancelled() => {
                return Err(cancelled_error(&request_id, &self.config.id));
            }
            response = self
                .client
                .post(url)
                .headers(self.headers(&secret, &request.request)?)
                .json(&body)
                .send() => response.map_err(|error| map_transport_error(&request_id, &self.config.id, error))?,
        };
        if !response.status().is_success() {
            return Err(tokio::select! {
                () = cancellation.cancelled() => cancelled_error(&request_id, &self.config.id),
                error = map_http_error(&request_id, &self.config.id, response) => error,
            });
        }
        let value = tokio::select! {
            () = cancellation.cancelled() => {
                return Err(cancelled_error(&request_id, &self.config.id));
            }
            value = response.json::<Value>() => value
                .map_err(|error| map_transport_error(&request_id, &self.config.id, error))?,
        };
        Ok(GatewayUsage {
            input_tokens: value
                .get("input_tokens")
                .or_else(|| value.get("totalTokens"))
                .and_then(Value::as_u64),
            provenance: fte_types::UsageProvenance::Exact,
            selected_route: Some(request.route),
            ..GatewayUsage::default()
        })
    }

    fn cancel(&self, request_id: &RequestId, _target: CancelTarget) -> usize {
        self.activity.cancel(request_id)
    }

    async fn shutdown(&self) -> Result<(), GatewayError> {
        self.activity.shutdown().await
    }
}

fn terminal_event(
    request_id: &RequestId,
    result: &Result<GatewayResponse, GatewayError>,
) -> GatewayEvent {
    match result {
        Ok(response) if response.status == TerminalStatus::Completed => GatewayEvent::Completed {
            request_id: request_id.clone(),
            response: Box::new(response.clone()),
        },
        Ok(response) => GatewayEvent::Cancelled {
            request_id: request_id.clone(),
            usage: response.usage.clone(),
        },
        Err(error) if error.class == ErrorClass::Cancelled => GatewayEvent::Cancelled {
            request_id: request_id.clone(),
            usage: GatewayUsage::default(),
        },
        Err(error) => GatewayEvent::Failed {
            request_id: request_id.clone(),
            error: error.clone(),
        },
    }
}

fn enqueue_reserved_terminal(
    permit: mpsc::OwnedPermit<GatewayEvent>,
    event: GatewayEvent,
    terminal_observed: &AtomicBool,
) {
    permit.send(event);
    terminal_observed.store(true, Ordering::Release);
}

async fn send_provider_event(
    events: &mpsc::Sender<GatewayEvent>,
    cancellation: &CancellationToken,
    request_id: &RequestId,
    provider: &str,
    event: GatewayEvent,
) -> Result<(), GatewayError> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(cancelled_error(request_id, provider)),
        result = events.send(event) => result.map_err(|_| GatewayError::unavailable(
            request_id,
            "gateway_event_channel_closed",
            "the hosted event consumer closed before the request completed",
        )),
    }
}

#[derive(Clone, Copy)]
enum WireProtocol {
    OpenAiResponses,
    OpenAiChat,
    OpenAiCompletion,
    AnthropicMessages,
    GeminiGenerateContent,
}

struct PreparedHostedRequest {
    url: String,
    body: Value,
    protocol: WireProtocol,
    streaming: bool,
}

struct HostedCancellation {
    token: CancellationToken,
}

impl TicketCancellation for HostedCancellation {
    fn cancel(&self, _target: CancelTarget) -> usize {
        self.token.cancel();
        1
    }
}

fn prepare_openai(
    config: &HostedProviderConfig,
    request: &BackendRequest,
) -> Result<PreparedHostedRequest, GatewayError> {
    match &request.request.input {
        GenerationInput::Completion { .. } => prepare_openai_completion(config, request),
        GenerationInput::Chat { .. } => Ok(PreparedHostedRequest {
            url: required_endpoint(
                request,
                config.endpoints.responses.as_deref(),
                "OpenAI Responses",
            )?,
            body: openai_responses_body(request)?,
            protocol: WireProtocol::OpenAiResponses,
            streaming: request.request.stream.enabled,
        }),
        GenerationInput::FillInMiddle { .. } => Err(capability_error(
            &request.request.request_id,
            &config.id,
            "hosted_fim_unsupported",
            "this hosted route has no verified FIM token contract",
        )),
    }
}

fn prepare_openai_compatible(
    config: &HostedProviderConfig,
    request: &BackendRequest,
) -> Result<PreparedHostedRequest, GatewayError> {
    match &request.request.input {
        GenerationInput::Completion { .. } => prepare_openai_completion(config, request),
        GenerationInput::Chat { .. } => Ok(PreparedHostedRequest {
            url: required_endpoint(
                request,
                config.endpoints.chat_completions.as_deref(),
                "Chat Completions",
            )?,
            body: openai_chat_body(request)?,
            protocol: WireProtocol::OpenAiChat,
            streaming: request.request.stream.enabled,
        }),
        GenerationInput::FillInMiddle { .. } => Err(capability_error(
            &request.request.request_id,
            &config.id,
            "hosted_fim_unsupported",
            "this hosted route has no verified FIM token contract",
        )),
    }
}

fn prepare_openai_completion(
    config: &HostedProviderConfig,
    request: &BackendRequest,
) -> Result<PreparedHostedRequest, GatewayError> {
    Ok(PreparedHostedRequest {
        url: required_endpoint(
            request,
            config.endpoints.completions.as_deref(),
            "Completions",
        )?,
        body: openai_completion_body(request)?,
        protocol: WireProtocol::OpenAiCompletion,
        streaming: request.request.stream.enabled,
    })
}

fn prepare_anthropic(
    config: &HostedProviderConfig,
    request: &BackendRequest,
) -> Result<PreparedHostedRequest, GatewayError> {
    if !matches!(request.request.input, GenerationInput::Chat { .. }) {
        return Err(capability_error(
            &request.request.request_id,
            &config.id,
            "anthropic_chat_required",
            "Anthropic Messages accepts canonical chat Items only",
        ));
    }
    Ok(PreparedHostedRequest {
        url: required_endpoint(
            request,
            config.endpoints.messages.as_deref(),
            "Anthropic Messages",
        )?,
        body: anthropic_body(request)?,
        protocol: WireProtocol::AnthropicMessages,
        streaming: request.request.stream.enabled,
    })
}

fn prepare_gemini(
    config: &HostedProviderConfig,
    request: &BackendRequest,
) -> Result<PreparedHostedRequest, GatewayError> {
    if !matches!(request.request.input, GenerationInput::Chat { .. }) {
        return Err(capability_error(
            &request.request.request_id,
            &config.id,
            "gemini_chat_required",
            "Gemini GenerateContent accepts canonical chat Items only",
        ));
    }
    let streaming = request.request.stream.enabled;
    Ok(PreparedHostedRequest {
        url: gemini_url(
            request,
            config.endpoints.messages.as_deref(),
            if streaming {
                "streamGenerateContent"
            } else {
                "generateContent"
            },
            streaming,
        )?,
        body: gemini_body(request)?,
        protocol: WireProtocol::GeminiGenerateContent,
        streaming,
    })
}

fn gemini_url(
    request: &BackendRequest,
    base: Option<&str>,
    method: &str,
    streaming: bool,
) -> Result<String, GatewayError> {
    let base = required_endpoint(request, base, "Gemini GenerateContent")?;
    let model = request
        .route
        .model_id
        .strip_prefix("models/")
        .unwrap_or(&request.route.model_id);
    Ok(format!(
        "{}/models/{model}:{method}{}",
        base.trim_end_matches('/'),
        if streaming { "?alt=sse" } else { "" }
    ))
}

fn required_endpoint(
    request: &BackendRequest,
    endpoint: Option<&str>,
    surface: &str,
) -> Result<String, GatewayError> {
    endpoint.map(ToString::to_string).ok_or_else(|| {
        capability_error(
            &request.request.request_id,
            &request.route.backend_id,
            "provider_surface_unavailable",
            &format!("{surface} is not configured for this route"),
        )
    })
}

fn openai_responses_body(request: &BackendRequest) -> Result<Value, GatewayError> {
    let GenerationInput::Chat { items } = &request.request.input else {
        return Err(capability_error(
            &request.request.request_id,
            &request.route.backend_id,
            "responses_items_required",
            "Responses requires canonical Items",
        ));
    };
    let mut body = Map::new();
    body.insert("model".to_string(), json!(request.route.model_id));
    body.insert(
        "input".to_string(),
        Value::Array(
            items
                .iter()
                .map(openai_input_item)
                .collect::<Result<Vec<_>, _>>()?,
        ),
    );
    body.insert("stream".to_string(), json!(request.request.stream.enabled));
    body.insert(
        "store".to_string(),
        json!(request.request.storage.store_response),
    );
    if let Some(previous) = &request.request.storage.previous_response_id {
        body.insert("previous_response_id".to_string(), json!(previous));
    }
    insert_openai_responses_sampling(&mut body, request)?;
    insert_openai_responses_tools(&mut body, request)?;
    insert_response_format(&mut body, &request.request.response_format);
    if let Some(key) = &request.request.cache.provider_key {
        body.insert("prompt_cache_key".to_string(), json!(key));
    }
    if let Some(ttl) = request.request.cache.provider_ttl {
        match ttl {
            fte_types::ProviderCacheTtl::TwentyFourHours => {
                body.insert("prompt_cache_retention".to_string(), json!("24h"));
            }
            fte_types::ProviderCacheTtl::FiveMinutes | fte_types::ProviderCacheTtl::OneHour => {
                return Err(capability_error(
                    &request.request.request_id,
                    &request.route.backend_id,
                    "openai_cache_ttl_unsupported",
                    "OpenAI prompt cache retention supports only 24h on this surface",
                ));
            }
        }
    }
    validate_provider_extensions(
        request,
        &[
            "openai.reasoning",
            "openai.include",
            "openai.metadata",
            "openai.service_tier",
            "openai.parallel_tool_calls",
        ],
    )?;
    for name in [
        "reasoning",
        "include",
        "metadata",
        "service_tier",
        "parallel_tool_calls",
    ] {
        if let Some(value) = request
            .request
            .provider_extensions
            .get(&format!("openai.{name}"))
        {
            body.insert(name.to_string(), value.clone());
        }
    }
    Ok(Value::Object(body))
}

fn openai_chat_body(request: &BackendRequest) -> Result<Value, GatewayError> {
    let GenerationInput::Chat { items } = &request.request.input else {
        return Err(capability_error(
            &request.request.request_id,
            &request.route.backend_id,
            "chat_items_required",
            "Chat Completions requires canonical chat Items",
        ));
    };
    let mut body = Map::new();
    body.insert("model".to_string(), json!(request.route.model_id));
    body.insert(
        "messages".to_string(),
        Value::Array(openai_chat_messages(items, &request.request.request_id)?),
    );
    body.insert("stream".to_string(), json!(request.request.stream.enabled));
    if request.request.stream.enabled {
        body.insert("stream_options".to_string(), json!({"include_usage":true}));
    }
    insert_openai_legacy_sampling(&mut body, request, "max_tokens")?;
    insert_openai_chat_tools(&mut body, request)?;
    validate_provider_extensions(request, &["openai.parallel_tool_calls"])?;
    if let Some(value) = request
        .request
        .provider_extensions
        .get("openai.parallel_tool_calls")
    {
        body.insert("parallel_tool_calls".to_string(), value.clone());
    }
    if !matches!(
        request.request.response_format,
        fte_types::ResponseFormat::Text
    ) {
        body.insert(
            "response_format".to_string(),
            openai_chat_response_format(&request.request.response_format),
        );
    }
    Ok(Value::Object(body))
}

fn openai_completion_body(request: &BackendRequest) -> Result<Value, GatewayError> {
    let GenerationInput::Completion { prompts } = &request.request.input else {
        return Err(capability_error(
            &request.request.request_id,
            &request.route.backend_id,
            "completion_prompt_required",
            "Completions requires exact canonical completion prompts",
        ));
    };
    let prompt = completion_prompt_json(prompts, &request.request.request_id)?;
    let mut body = Map::new();
    body.insert("model".to_string(), json!(request.route.model_id));
    body.insert("prompt".to_string(), prompt);
    body.insert("stream".to_string(), json!(request.request.stream.enabled));
    insert_openai_legacy_sampling(&mut body, request, "max_tokens")?;
    Ok(Value::Object(body))
}

fn completion_prompt_json(
    prompts: &[CompletionPrompt],
    request_id: &RequestId,
) -> Result<Value, GatewayError> {
    match prompts {
        [
            CompletionPrompt::Text {
                text,
                add_bos: false,
            },
        ] => Ok(json!(text)),
        values
            if values
                .iter()
                .all(|value| matches!(value, CompletionPrompt::Text { add_bos: false, .. })) =>
        {
            Ok(json!(
                values
                    .iter()
                    .filter_map(|value| match value {
                        CompletionPrompt::Text { text, .. } => Some(text),
                        CompletionPrompt::Tokens { .. } => None,
                    })
                    .collect::<Vec<_>>()
            ))
        }
        [CompletionPrompt::Tokens { token_ids }] => Ok(json!(token_ids)),
        values
            if values
                .iter()
                .all(|value| matches!(value, CompletionPrompt::Tokens { .. })) =>
        {
            Ok(json!(
                values
                    .iter()
                    .filter_map(|value| match value {
                        CompletionPrompt::Tokens { token_ids } => Some(token_ids),
                        CompletionPrompt::Text { .. } => None,
                    })
                    .collect::<Vec<_>>()
            ))
        }
        values
            if values
                .iter()
                .any(|value| matches!(value, CompletionPrompt::Text { add_bos: true, .. })) =>
        {
            Err(capability_error(
                request_id,
                "openai-compatible",
                "completion_bos_policy_unsupported",
                "hosted completion APIs cannot guarantee caller-directed BOS insertion",
            ))
        }
        _ => Err(capability_error(
            request_id,
            "openai-compatible",
            "completion_prompt_batch_mixed",
            "completion batches cannot mix text and token prompts",
        )),
    }
}

fn anthropic_body(request: &BackendRequest) -> Result<Value, GatewayError> {
    let GenerationInput::Chat { items } = &request.request.input else {
        return Err(capability_error(
            &request.request.request_id,
            &request.route.backend_id,
            "anthropic_items_required",
            "Anthropic Messages requires canonical Items",
        ));
    };
    let (system, messages) = anthropic_messages_from_items(items, &request.request.request_id)?;
    let mut body = Map::new();
    body.insert("model".to_string(), json!(request.route.model_id));
    body.insert(
        "max_tokens".to_string(),
        json!(request.request.sampling.max_output_tokens.unwrap_or(1024)),
    );
    body.insert("messages".to_string(), Value::Array(messages));
    body.insert("stream".to_string(), json!(request.request.stream.enabled));
    if !system.is_empty() {
        body.insert("system".to_string(), Value::Array(system));
    }
    insert_anthropic_sampling(&mut body, request)?;
    if !request.request.tools.is_empty() {
        body.insert(
            "tools".to_string(),
            json!(
                request
                    .request
                    .tools
                    .iter()
                    .map(|tool| json!({
                        "name":tool.name,
                        "description":tool.description,
                        "input_schema":tool.input_schema
                    }))
                    .collect::<Vec<_>>()
            ),
        );
        let choice = match request.request.tool_policy.execution {
            fte_types::ToolExecutionPolicy::Deny => json!({"type":"none"}),
            fte_types::ToolExecutionPolicy::ClientOnly | fte_types::ToolExecutionPolicy::Ask => {
                json!({"type":"auto"})
            }
            fte_types::ToolExecutionPolicy::AllowGateway => {
                return Err(capability_error(
                    &request.request.request_id,
                    &request.route.backend_id,
                    "gateway_tool_execution_not_bound",
                    "hosted tool execution requires an explicit gateway-owned tool adapter",
                ));
            }
        };
        body.insert("tool_choice".to_string(), choice);
    }
    apply_anthropic_cache_breakpoints(
        &mut body,
        &request.request.cache.provider_breakpoints,
        &request.request.request_id,
    )?;
    validate_provider_extensions(
        request,
        &[
            "anthropic.thinking",
            "anthropic.metadata",
            "anthropic.service_tier",
            "anthropic.beta",
        ],
    )?;
    for name in ["thinking", "metadata", "service_tier"] {
        if let Some(value) = request
            .request
            .provider_extensions
            .get(&format!("anthropic.{name}"))
        {
            body.insert(name.to_string(), value.clone());
        }
    }
    Ok(Value::Object(body))
}

fn anthropic_count_body(request: &BackendRequest) -> Result<Value, GatewayError> {
    validate_provider_extensions(
        request,
        &[
            "anthropic.thinking",
            "anthropic.metadata",
            "anthropic.service_tier",
            "anthropic.beta",
        ],
    )?;
    let GenerationInput::Chat { items } = &request.request.input else {
        return Err(capability_error(
            &request.request.request_id,
            &request.route.backend_id,
            "anthropic_items_required",
            "Anthropic token counting requires canonical chat Items",
        ));
    };
    let (system, messages) = anthropic_messages_from_items(items, &request.request.request_id)?;
    let mut body = Map::new();
    body.insert("model".to_string(), json!(request.route.model_id));
    body.insert("messages".to_string(), Value::Array(messages));
    if !system.is_empty() {
        body.insert("system".to_string(), Value::Array(system));
    }
    if !request.request.tools.is_empty() {
        body.insert(
            "tools".to_string(),
            anthropic_tools_value(&request.request.tools),
        );
    }
    apply_anthropic_cache_breakpoints(
        &mut body,
        &request.request.cache.provider_breakpoints,
        &request.request.request_id,
    )?;
    Ok(Value::Object(body))
}

fn gemini_body(request: &BackendRequest) -> Result<Value, GatewayError> {
    let GenerationInput::Chat { items } = &request.request.input else {
        return Err(capability_error(
            &request.request.request_id,
            &request.route.backend_id,
            "gemini_items_required",
            "Gemini GenerateContent requires canonical chat Items",
        ));
    };
    let mut system_parts = Vec::new();
    let mut contents = Vec::new();
    let mut call_names = BTreeMap::<String, String>::new();
    for item in items {
        match item {
            fte_types::InputItem::Message {
                role: fte_types::MessageRole::System | fte_types::MessageRole::Developer,
                content,
                ..
            } => system_parts.extend(
                content
                    .iter()
                    .map(gemini_content)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            fte_types::InputItem::Message { role, content, .. } => contents.push(json!({
                "role": if *role == fte_types::MessageRole::Assistant { "model" } else { "user" },
                "parts": content.iter().map(gemini_content).collect::<Result<Vec<_>,_>>()?
            })),
            fte_types::InputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                call_names.insert(call_id.clone(), name.clone());
                let part = json!({"functionCall":{"name":name,"args":arguments}});
                if !append_to_last_role(&mut contents, "model", "parts", part.clone()) {
                    contents.push(json!({"role":"model","parts":[part]}));
                }
            }
            fte_types::InputItem::FunctionResult {
                call_id, output, ..
            } => {
                let Some(name) = call_names.get(call_id) else {
                    return Err(capability_error(
                        &request.request.request_id,
                        &request.route.backend_id,
                        "gemini_tool_result_name_missing",
                        "Gemini tool results require the matching function call in canonical history",
                    ));
                };
                contents.push(json!({
                    "role":"user",
                    "parts":[{"functionResponse":{"name":name,"response":{"content":content_text(output)}}}]
                }));
            }
            fte_types::InputItem::Reasoning {
                opaque_continuation,
                ..
            } => {
                let Some(Value::Object(opaque)) = opaque_continuation else {
                    return Err(capability_error(
                        &request.request.request_id,
                        &request.route.backend_id,
                        "gemini_reasoning_continuity_missing",
                        "Gemini thought continuation requires its provider thought signature",
                    ));
                };
                contents.push(Value::Object(opaque.clone()));
            }
            fte_types::InputItem::ProviderOpaque { provider, item } if provider == "gemini" => {
                contents.push(item.clone());
            }
            fte_types::InputItem::ProviderOpaque { .. } => {
                return Err(capability_error(
                    &request.request.request_id,
                    &request.route.backend_id,
                    "provider_state_incompatible",
                    "provider-opaque continuation belongs to another provider",
                ));
            }
        }
    }
    let sampling = &request.request.sampling;
    reject_sampling_fields(
        request,
        [
            sampling.min_p.map(|_| "min_p"),
            sampling.seed.map(|_| "seed"),
            sampling.presence_penalty.map(|_| "presence_penalty"),
            sampling.frequency_penalty.map(|_| "frequency_penalty"),
        ],
        "Gemini GenerateContent",
    )?;
    let mut generation = Map::new();
    if let Some(value) = sampling.max_output_tokens {
        generation.insert("maxOutputTokens".to_string(), json!(value));
    }
    if let Some(value) = sampling.temperature {
        generation.insert("temperature".to_string(), json!(value));
    }
    if let Some(value) = sampling.top_p {
        generation.insert("topP".to_string(), json!(value));
    }
    if let Some(value) = sampling.top_k {
        generation.insert("topK".to_string(), json!(value));
    }
    if !sampling.stop.is_empty() {
        generation.insert("stopSequences".to_string(), json!(sampling.stop));
    }
    validate_provider_extensions(
        request,
        &[
            "gemini.thinkingConfig",
            "gemini.toolConfig",
            "gemini.safetySettings",
            "gemini.cachedContent",
        ],
    )?;
    if let Some(value) = request
        .request
        .provider_extensions
        .get("gemini.thinkingConfig")
    {
        generation.insert("thinkingConfig".to_string(), value.clone());
    }
    match &request.request.response_format {
        fte_types::ResponseFormat::Text => {}
        fte_types::ResponseFormat::JsonObject => {
            generation.insert("responseMimeType".to_string(), json!("application/json"));
        }
        fte_types::ResponseFormat::JsonSchema { schema, .. } => {
            generation.insert("responseMimeType".to_string(), json!("application/json"));
            generation.insert("responseJsonSchema".to_string(), schema.clone());
        }
    }
    let mut body = Map::new();
    body.insert("contents".to_string(), Value::Array(contents));
    if !system_parts.is_empty() {
        body.insert(
            "systemInstruction".to_string(),
            json!({"parts":system_parts}),
        );
    }
    if !generation.is_empty() {
        body.insert("generationConfig".to_string(), Value::Object(generation));
    }
    if !request.request.tools.is_empty() {
        let declarations = request
            .request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name":tool.name,
                    "description":tool.description,
                    "parameters":tool.input_schema
                })
            })
            .collect::<Vec<_>>();
        body.insert(
            "tools".to_string(),
            json!([{"functionDeclarations":declarations}]),
        );
        if let Some(tool_config) = request.request.provider_extensions.get("gemini.toolConfig") {
            body.insert("toolConfig".to_string(), tool_config.clone());
        } else {
            let mode = match request.request.tool_policy.execution {
                fte_types::ToolExecutionPolicy::Deny => "NONE",
                fte_types::ToolExecutionPolicy::ClientOnly
                | fte_types::ToolExecutionPolicy::Ask => "AUTO",
                fte_types::ToolExecutionPolicy::AllowGateway => {
                    return Err(capability_error(
                        &request.request.request_id,
                        &request.route.backend_id,
                        "gateway_tool_execution_not_bound",
                        "hosted tool execution requires an explicit gateway-owned tool adapter",
                    ));
                }
            };
            body.insert(
                "toolConfig".to_string(),
                json!({"functionCallingConfig":{"mode":mode}}),
            );
        }
    } else if let Some(tool_config) = request.request.provider_extensions.get("gemini.toolConfig") {
        body.insert("toolConfig".to_string(), tool_config.clone());
    }
    for name in ["safetySettings", "cachedContent"] {
        if let Some(value) = request
            .request
            .provider_extensions
            .get(&format!("gemini.{name}"))
        {
            body.insert(name.to_string(), value.clone());
        }
    }
    Ok(Value::Object(body))
}

fn gemini_count_body(request: &BackendRequest) -> Result<Value, GatewayError> {
    let body = gemini_body(request)?;
    let Some(full) = body.as_object() else {
        return Err(capability_error(
            &request.request.request_id,
            &request.route.backend_id,
            "gemini_count_body_invalid",
            "Gemini token counting could not construct a prompt body",
        ));
    };
    let mut count = Map::new();
    for key in ["contents", "systemInstruction", "tools"] {
        if let Some(value) = full.get(key) {
            count.insert(key.to_string(), value.clone());
        }
    }
    Ok(Value::Object(count))
}

fn gemini_content(block: &fte_types::ContentBlock) -> Result<Value, GatewayError> {
    match block {
        fte_types::ContentBlock::Text { text } => Ok(json!({"text":text})),
        fte_types::ContentBlock::Image { source, .. }
        | fte_types::ContentBlock::Audio { source, .. }
        | fte_types::ContentBlock::Document { source, .. } => match source {
            fte_types::MediaSource::Bytes {
                mime_type,
                data_base64,
            } => Ok(json!({"inlineData":{"mimeType":mime_type,"data":data_base64}})),
            fte_types::MediaSource::Url { url } => Ok(json!({"fileData":{"fileUri":url}})),
            fte_types::MediaSource::FileId { file_id } => {
                Ok(json!({"fileData":{"fileUri":file_id}}))
            }
        },
        fte_types::ContentBlock::Thinking { text, signature } => Ok(json!({
            "text":text,
            "thought":true,
            "thoughtSignature":signature
        })),
        fte_types::ContentBlock::RedactedThinking { data } => Ok(json!({
            "thought":true,
            "thoughtSignature":data
        })),
    }
}

fn openai_input_item(item: &fte_types::InputItem) -> Result<Value, GatewayError> {
    let request_id = RequestId::new();
    match item {
        fte_types::InputItem::Message { role, content, .. } => Ok(json!({
            "type":"message",
            "role":role_name(*role),
            "content":content.iter().map(openai_input_content).collect::<Result<Vec<_>,_>>()?,
        })),
        fte_types::InputItem::FunctionCall {
            id,
            call_id,
            name,
            arguments,
        } => Ok(json!({
            "id":id,
            "type":"function_call",
            "call_id":call_id,
            "name":name,
            "arguments":serde_json::to_string(arguments).map_err(|error| provider_request_error(&request_id,"openai",error))?,
        })),
        fte_types::InputItem::FunctionResult {
            id,
            call_id,
            output,
            ..
        } => Ok(json!({
            "id":id,
            "type":"function_call_output",
            "call_id":call_id,
            "output":content_text(output),
        })),
        fte_types::InputItem::Reasoning {
            id,
            summary,
            opaque_continuation,
        } => Ok(json!({
            "id":id,
            "type":"reasoning",
            "summary":summary.iter().map(|text|json!({"type":"summary_text","text":text})).collect::<Vec<_>>(),
            "encrypted_content":opaque_continuation,
        })),
        fte_types::InputItem::ProviderOpaque { provider, item } if provider == "openai" => {
            Ok(item.clone())
        }
        fte_types::InputItem::ProviderOpaque { .. } => Err(capability_error(
            &request_id,
            "openai",
            "provider_state_incompatible",
            "provider-opaque continuation belongs to another provider",
        )),
    }
}

fn openai_input_content(block: &fte_types::ContentBlock) -> Result<Value, GatewayError> {
    match block {
        fte_types::ContentBlock::Text { text } => Ok(json!({"type":"input_text","text":text})),
        fte_types::ContentBlock::Image { source, detail } => {
            Ok(json!({"type":"input_image","image_url":media_url(source),"detail":detail}))
        }
        fte_types::ContentBlock::Audio { source, format } => Ok(json!({
            "type":"input_audio",
            "input_audio":{"data":media_data(source)?,"format":format}
        })),
        fte_types::ContentBlock::Document { source, title } => Ok(json!({
            "type":"input_file",
            "file_data":media_data(source)?,
            "filename":title
        })),
        fte_types::ContentBlock::Thinking { .. }
        | fte_types::ContentBlock::RedactedThinking { .. } => Err(capability_error(
            &RequestId::new(),
            "openai",
            "reasoning_content_position_invalid",
            "reasoning continuation must use a typed reasoning Item",
        )),
    }
}

fn append_to_last_role(
    messages: &mut [Value],
    role: &str,
    array_field: &str,
    value: Value,
) -> bool {
    let Some(last) = messages.last_mut().and_then(Value::as_object_mut) else {
        return false;
    };
    if last.get("role").and_then(Value::as_str) != Some(role) {
        return false;
    }
    match last.get_mut(array_field) {
        Some(Value::Array(values)) => values.push(value),
        Some(_) => return false,
        None => {
            last.insert(array_field.to_string(), Value::Array(vec![value]));
        }
    }
    true
}

fn openai_chat_messages(
    items: &[fte_types::InputItem],
    request_id: &RequestId,
) -> Result<Vec<Value>, GatewayError> {
    let mut messages = Vec::new();
    for item in items {
        match item {
            fte_types::InputItem::Message { role, content, .. } => messages.push(json!({
                "role":role_name(*role),
                "content":content.iter().map(openai_chat_content).collect::<Result<Vec<_>,_>>()?,
            })),
            fte_types::InputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let call = json!({"id":call_id,"type":"function","function":{"name":name,"arguments":serde_json::to_string(arguments).map_err(|error|provider_request_error(request_id,"openai-compatible",error))?}});
                if !append_to_last_role(&mut messages, "assistant", "tool_calls", call.clone()) {
                    messages.push(json!({
                        "role":"assistant",
                        "content":Value::Null,
                        "tool_calls":[call],
                    }));
                }
            }
            fte_types::InputItem::FunctionResult {
                call_id, output, ..
            } => messages.push(json!({
                "role":"tool","tool_call_id":call_id,"content":content_text(output),
            })),
            fte_types::InputItem::Reasoning { .. }
            | fte_types::InputItem::ProviderOpaque { .. } => {
                return Err(capability_error(
                    request_id,
                    "openai-compatible",
                    "provider_continuation_unsupported",
                    "this Chat Completions route cannot preserve provider reasoning continuity",
                ));
            }
        }
    }
    Ok(messages)
}

fn openai_chat_content(block: &fte_types::ContentBlock) -> Result<Value, GatewayError> {
    match block {
        fte_types::ContentBlock::Text { text } => Ok(json!({"type":"text","text":text})),
        fte_types::ContentBlock::Image { source, detail } => Ok(json!({
            "type":"image_url","image_url":{"url":media_url(source),"detail":detail}
        })),
        _ => Err(capability_error(
            &RequestId::new(),
            "openai-compatible",
            "chat_content_unsupported",
            "this Chat Completions route cannot preserve the requested content block",
        )),
    }
}

fn anthropic_messages_from_items(
    items: &[fte_types::InputItem],
    request_id: &RequestId,
) -> Result<(Vec<Value>, Vec<Value>), GatewayError> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for item in items {
        match item {
            fte_types::InputItem::Message {
                role: fte_types::MessageRole::System | fte_types::MessageRole::Developer,
                content,
                ..
            } => system.extend(
                content
                    .iter()
                    .map(anthropic_content)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            fte_types::InputItem::Message { role, content, .. } => messages.push(json!({
                "role":role_name(*role),
                "content":content.iter().map(anthropic_content).collect::<Result<Vec<_>,_>>()?
            })),
            fte_types::InputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let block =
                    json!({"type":"tool_use","id":call_id,"name":name,"input":arguments});
                if !append_to_last_role(
                    &mut messages,
                    "assistant",
                    "content",
                    block.clone(),
                ) {
                    messages.push(json!({"role":"assistant","content":[block]}));
                }
            }
            fte_types::InputItem::FunctionResult {
                call_id,
                output,
                is_error,
                ..
            } => messages.push(json!({
                "role":"user",
                "content":[{"type":"tool_result","tool_use_id":call_id,"content":content_text(output),"is_error":is_error}]
            })),
            fte_types::InputItem::Reasoning {
                summary,
                opaque_continuation,
                ..
            } => {
                if let Some(Value::Object(opaque)) = opaque_continuation
                    && let (Some(Value::String(thinking)), Some(Value::String(signature))) =
                        (opaque.get("thinking"), opaque.get("signature"))
                {
                    messages.push(json!({
                        "role":"assistant",
                        "content":[{"type":"thinking","thinking":thinking,"signature":signature}]
                    }));
                } else if !summary.is_empty() {
                    return Err(capability_error(
                        request_id,
                        "anthropic",
                        "anthropic_reasoning_continuity_missing",
                        "Anthropic thinking continuation requires its provider signature",
                    ));
                }
            }
            fte_types::InputItem::ProviderOpaque { provider, item }
                if provider == "anthropic" => messages.push(item.clone()),
            fte_types::InputItem::ProviderOpaque { .. } => {
                return Err(capability_error(
                    request_id,
                    "anthropic",
                    "provider_state_incompatible",
                    "provider-opaque continuation belongs to another provider",
                ));
            }
        }
    }
    Ok((system, messages))
}

fn anthropic_content(block: &fte_types::ContentBlock) -> Result<Value, GatewayError> {
    match block {
        fte_types::ContentBlock::Text { text } => Ok(json!({"type":"text","text":text})),
        fte_types::ContentBlock::Image { source, .. } => {
            Ok(json!({"type":"image","source":anthropic_media(source)}))
        }
        fte_types::ContentBlock::Document { source, title } => {
            Ok(json!({"type":"document","source":anthropic_media(source),"title":title}))
        }
        fte_types::ContentBlock::Thinking { text, signature } => {
            Ok(json!({"type":"thinking","thinking":text,"signature":signature}))
        }
        fte_types::ContentBlock::RedactedThinking { data } => {
            Ok(json!({"type":"redacted_thinking","data":data}))
        }
        fte_types::ContentBlock::Audio { .. } => Err(capability_error(
            &RequestId::new(),
            "anthropic",
            "anthropic_audio_unsupported",
            "Anthropic Messages does not accept this canonical audio block",
        )),
    }
}

fn insert_openai_responses_sampling(
    body: &mut Map<String, Value>,
    request: &BackendRequest,
) -> Result<(), GatewayError> {
    let sampling = &request.request.sampling;
    reject_sampling_fields(
        request,
        [
            sampling.top_k.map(|_| "top_k"),
            sampling.min_p.map(|_| "min_p"),
            sampling.seed.map(|_| "seed"),
            (!sampling.stop.is_empty()).then_some("stop"),
            sampling.presence_penalty.map(|_| "presence_penalty"),
            sampling.frequency_penalty.map(|_| "frequency_penalty"),
        ],
        "OpenAI Responses",
    )?;
    if let Some(max) = sampling.max_output_tokens {
        body.insert("max_output_tokens".to_string(), json!(max));
    }
    if let Some(value) = sampling.temperature {
        body.insert("temperature".to_string(), json!(value));
    }
    if let Some(value) = sampling.top_p {
        body.insert("top_p".to_string(), json!(value));
    }
    Ok(())
}

fn insert_openai_legacy_sampling(
    body: &mut Map<String, Value>,
    request: &BackendRequest,
    max_tokens_key: &str,
) -> Result<(), GatewayError> {
    let sampling = &request.request.sampling;
    reject_sampling_fields(
        request,
        [
            sampling.top_k.map(|_| "top_k"),
            sampling.min_p.map(|_| "min_p"),
        ],
        "OpenAI-compatible legacy generation",
    )?;
    if let Some(max) = sampling.max_output_tokens {
        body.insert(max_tokens_key.to_string(), json!(max));
    }
    if let Some(value) = sampling.temperature {
        body.insert("temperature".to_string(), json!(value));
    }
    if let Some(value) = sampling.top_p {
        body.insert("top_p".to_string(), json!(value));
    }
    if let Some(value) = sampling.seed {
        body.insert("seed".to_string(), json!(value));
    }
    if !sampling.stop.is_empty() {
        body.insert("stop".to_string(), json!(sampling.stop));
    }
    if let Some(value) = sampling.presence_penalty {
        body.insert("presence_penalty".to_string(), json!(value));
    }
    if let Some(value) = sampling.frequency_penalty {
        body.insert("frequency_penalty".to_string(), json!(value));
    }
    Ok(())
}

fn insert_anthropic_sampling(
    body: &mut Map<String, Value>,
    request: &BackendRequest,
) -> Result<(), GatewayError> {
    let sampling = &request.request.sampling;
    reject_sampling_fields(
        request,
        [
            sampling.min_p.map(|_| "min_p"),
            sampling.seed.map(|_| "seed"),
            sampling.presence_penalty.map(|_| "presence_penalty"),
            sampling.frequency_penalty.map(|_| "frequency_penalty"),
        ],
        "Anthropic Messages",
    )?;
    if !matches!(
        request.request.response_format,
        fte_types::ResponseFormat::Text
    ) {
        return Err(capability_error(
            &request.request.request_id,
            &request.route.backend_id,
            "anthropic_response_format_unsupported",
            "this Anthropic Messages adapter cannot guarantee the requested response format",
        ));
    }
    if let Some(value) = sampling.temperature {
        body.insert("temperature".to_string(), json!(value));
    }
    if let Some(value) = sampling.top_p {
        body.insert("top_p".to_string(), json!(value));
    }
    if let Some(value) = sampling.top_k {
        body.insert("top_k".to_string(), json!(value));
    }
    if !sampling.stop.is_empty() {
        body.insert("stop_sequences".to_string(), json!(sampling.stop));
    }
    Ok(())
}

fn reject_sampling_fields<const N: usize>(
    request: &BackendRequest,
    fields: [Option<&str>; N],
    surface: &str,
) -> Result<(), GatewayError> {
    let unsupported = fields.into_iter().flatten().collect::<Vec<_>>();
    if unsupported.is_empty() {
        return Ok(());
    }
    Err(capability_error(
        &request.request.request_id,
        &request.route.backend_id,
        "sampling_parameter_unsupported",
        &format!(
            "{surface} cannot preserve these requested sampling parameters: {}",
            unsupported.join(", ")
        ),
    ))
}

fn insert_openai_responses_tools(
    body: &mut Map<String, Value>,
    request: &BackendRequest,
) -> Result<(), GatewayError> {
    insert_tool_policy(body, request)?;
    if !request.request.tools.is_empty() {
        body.insert(
            "tools".to_string(),
            json!(
                request
                    .request
                    .tools
                    .iter()
                    .map(|tool| json!({
                        "type":"function",
                        "name":tool.name,
                        "description":tool.description,
                        "parameters":tool.input_schema,
                        "strict":tool.strict
                    }))
                    .collect::<Vec<_>>()
            ),
        );
    }
    Ok(())
}

fn insert_openai_chat_tools(
    body: &mut Map<String, Value>,
    request: &BackendRequest,
) -> Result<(), GatewayError> {
    insert_tool_policy(body, request)?;
    if !request.request.tools.is_empty() {
        body.insert(
            "tools".to_string(),
            json!(
                request
                    .request
                    .tools
                    .iter()
                    .map(|tool| json!({
                        "type":"function",
                        "function":{
                            "name":tool.name,
                            "description":tool.description,
                            "parameters":tool.input_schema,
                            "strict":tool.strict
                        }
                    }))
                    .collect::<Vec<_>>()
            ),
        );
    }
    Ok(())
}

fn insert_tool_policy(
    body: &mut Map<String, Value>,
    request: &BackendRequest,
) -> Result<(), GatewayError> {
    if request.request.tools.is_empty() {
        return Ok(());
    }
    let choice = match request.request.tool_policy.execution {
        fte_types::ToolExecutionPolicy::Deny => json!("none"),
        fte_types::ToolExecutionPolicy::ClientOnly | fte_types::ToolExecutionPolicy::Ask => {
            json!("auto")
        }
        fte_types::ToolExecutionPolicy::AllowGateway => {
            return Err(capability_error(
                &request.request.request_id,
                &request.route.backend_id,
                "gateway_tool_execution_not_bound",
                "hosted tool execution requires an explicit gateway-owned tool adapter",
            ));
        }
    };
    body.insert("tool_choice".to_string(), choice);
    Ok(())
}

fn anthropic_tools_value(tools: &[fte_types::ToolDefinition]) -> Value {
    json!(
        tools
            .iter()
            .map(|tool| json!({
                "name":tool.name,
                "description":tool.description,
                "input_schema":tool.input_schema
            }))
            .collect::<Vec<_>>()
    )
}

fn insert_response_format(body: &mut Map<String, Value>, format: &fte_types::ResponseFormat) {
    if !matches!(format, fte_types::ResponseFormat::Text) {
        body.insert(
            "text".to_string(),
            json!({"format":openai_response_format(format)}),
        );
    }
}

fn openai_response_format(format: &fte_types::ResponseFormat) -> Value {
    match format {
        fte_types::ResponseFormat::Text => json!({"type":"text"}),
        fte_types::ResponseFormat::JsonObject => json!({"type":"json_object"}),
        fte_types::ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => json!({"type":"json_schema","name":name,"schema":schema,"strict":strict}),
    }
}

fn openai_chat_response_format(format: &fte_types::ResponseFormat) -> Value {
    match format {
        fte_types::ResponseFormat::Text => json!({"type":"text"}),
        fte_types::ResponseFormat::JsonObject => json!({"type":"json_object"}),
        fte_types::ResponseFormat::JsonSchema {
            name,
            schema,
            strict,
        } => json!({
            "type":"json_schema",
            "json_schema":{"name":name,"schema":schema,"strict":strict}
        }),
    }
}

fn apply_anthropic_cache_breakpoints(
    body: &mut Map<String, Value>,
    breakpoints: &[fte_types::ProviderCacheBreakpoint],
    request_id: &RequestId,
) -> Result<(), GatewayError> {
    for breakpoint in breakpoints {
        let ttl = match breakpoint
            .ttl
            .unwrap_or(fte_types::ProviderCacheTtl::FiveMinutes)
        {
            fte_types::ProviderCacheTtl::FiveMinutes => "5m",
            fte_types::ProviderCacheTtl::OneHour => "1h",
            fte_types::ProviderCacheTtl::TwentyFourHours => {
                return Err(capability_error(
                    request_id,
                    "anthropic",
                    "anthropic_cache_ttl_unsupported",
                    "Anthropic cache TTL cannot be 24h",
                ));
            }
        };
        let mut path = breakpoint.path.split('.');
        let root = path.next().unwrap_or_default();
        let root_index = path.next().and_then(|value| value.parse::<usize>().ok());
        let content_marker = path.next();
        let content_index = path.next().and_then(|value| value.parse::<usize>().ok());
        let target = match (root, root_index, content_marker, content_index) {
            ("system", Some(index), None, None) => body
                .get_mut("system")
                .and_then(Value::as_array_mut)
                .and_then(|values| values.get_mut(index)),
            ("tools", Some(index), None, None) => body
                .get_mut("tools")
                .and_then(Value::as_array_mut)
                .and_then(|values| values.get_mut(index)),
            ("messages", Some(message), Some("content"), Some(content)) => body
                .get_mut("messages")
                .and_then(Value::as_array_mut)
                .and_then(|values| values.get_mut(message))
                .and_then(|message| message.get_mut("content"))
                .and_then(Value::as_array_mut)
                .and_then(|values| values.get_mut(content)),
            _ => None,
        }
        .ok_or_else(|| {
            capability_error(
                request_id,
                "anthropic",
                "anthropic_cache_breakpoint_unmappable",
                "a cache breakpoint could not be mapped without changing message semantics",
            )
        })?;
        target
            .as_object_mut()
            .ok_or_else(|| {
                capability_error(
                    request_id,
                    "anthropic",
                    "anthropic_cache_breakpoint_invalid",
                    "an Anthropic cache breakpoint did not address a content block",
                )
            })?
            .insert(
                "cache_control".to_string(),
                json!({"type":"ephemeral","ttl":ttl}),
            );
    }
    Ok(())
}

fn role_name(role: fte_types::MessageRole) -> &'static str {
    match role {
        fte_types::MessageRole::System => "system",
        fte_types::MessageRole::Developer => "developer",
        fte_types::MessageRole::User => "user",
        fte_types::MessageRole::Assistant => "assistant",
        fte_types::MessageRole::Tool => "tool",
    }
}

fn media_url(source: &fte_types::MediaSource) -> String {
    match source {
        fte_types::MediaSource::Url { url } => url.clone(),
        fte_types::MediaSource::Bytes {
            mime_type,
            data_base64,
        } => format!("data:{mime_type};base64,{data_base64}"),
        fte_types::MediaSource::FileId { file_id } => file_id.clone(),
    }
}

fn media_data(source: &fte_types::MediaSource) -> Result<String, GatewayError> {
    match source {
        fte_types::MediaSource::Bytes { data_base64, .. } => Ok(data_base64.clone()),
        fte_types::MediaSource::FileId { file_id } => Ok(file_id.clone()),
        fte_types::MediaSource::Url { .. } => Err(capability_error(
            &RequestId::new(),
            "provider",
            "inline_media_required",
            "this provider field requires inline data or a provider file ID",
        )),
    }
}

fn anthropic_media(source: &fte_types::MediaSource) -> Value {
    match source {
        fte_types::MediaSource::Bytes {
            mime_type,
            data_base64,
        } => json!({"type":"base64","media_type":mime_type,"data":data_base64}),
        fte_types::MediaSource::Url { url } => json!({"type":"url","url":url}),
        fte_types::MediaSource::FileId { file_id } => {
            json!({"type":"file","file_id":file_id})
        }
    }
}

fn content_text(content: &[fte_types::ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            fte_types::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct ProviderJsonRequest {
    protocol: WireProtocol,
    request_id: RequestId,
    response_id: String,
    route: ResolvedRoute,
    previous_response_id: Option<String>,
    events: mpsc::Sender<GatewayEvent>,
    cancellation: CancellationToken,
}

async fn consume_provider_json(
    response: reqwest::Response,
    request: ProviderJsonRequest,
) -> Result<GatewayResponse, GatewayError> {
    let ProviderJsonRequest {
        protocol,
        request_id,
        response_id,
        route,
        previous_response_id,
        events,
        cancellation,
    } = request;
    let value = response
        .json::<Value>()
        .await
        .map_err(|error| map_transport_error(&request_id, &route.backend_id, error))?;
    let response = parse_provider_response(
        protocol,
        &value,
        &request_id,
        &response_id,
        route,
        previous_response_id,
    )?;
    emit_completed_lifecycle(
        &events,
        &cancellation,
        &request_id,
        &response.route.backend_id,
        &response,
    )
    .await?;
    Ok(response)
}

struct ProviderStreamRequest {
    protocol: WireProtocol,
    request_id: RequestId,
    response_id: String,
    route: ResolvedRoute,
    previous_response_id: Option<String>,
    events: mpsc::Sender<GatewayEvent>,
    cancellation: CancellationToken,
}

async fn consume_provider_stream(
    response: reqwest::Response,
    request: ProviderStreamRequest,
) -> Result<GatewayResponse, GatewayError> {
    let ProviderStreamRequest {
        protocol,
        request_id,
        response_id,
        route,
        previous_response_id,
        events,
        cancellation,
    } = request;
    let mut bytes = response.bytes_stream();
    let mut parser = SseParser::default();
    let mut state = ProviderStreamState::new(
        protocol,
        request_id.clone(),
        response_id,
        route,
        previous_response_id,
        cancellation.clone(),
    );
    let started_at = tokio::time::Instant::now();
    loop {
        let next = tokio::select! {
            () = cancellation.cancelled() => {
                return Err(cancelled_error(&request_id, &state.route.backend_id));
            }
            value = tokio::time::timeout(Duration::from_secs(120), bytes.next()) => value,
        };
        let chunk = match next {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(error))) => {
                return Err(map_transport_error(
                    &request_id,
                    &state.route.backend_id,
                    error,
                ));
            }
            Ok(None) => break,
            Err(_) => {
                return Err(timeout_error(
                    &request_id,
                    &state.route.backend_id,
                    "provider_stream_idle_timeout",
                    "the hosted provider stream was idle for too long",
                ));
            }
        };
        if started_at.elapsed() > Duration::from_secs(60 * 60) {
            return Err(timeout_error(
                &request_id,
                &state.route.backend_id,
                "provider_stream_total_timeout",
                "the hosted provider stream exceeded its total deadline",
            ));
        }
        for frame in parser.push(&chunk)? {
            if state.consume_frame(frame, &events).await? {
                return state.finish(&events).await;
            }
        }
    }
    for frame in parser.finish()? {
        if state.consume_frame(frame, &events).await? {
            return state.finish(&events).await;
        }
    }
    state.finish(&events).await
}

#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
}

struct SseFrame {
    event: Option<String>,
    data: String,
}

impl SseParser {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseFrame>, GatewayError> {
        self.buffer.extend_from_slice(bytes);
        self.take(false)
    }

    fn finish(&mut self) -> Result<Vec<SseFrame>, GatewayError> {
        self.take(true)
    }

    fn take(&mut self, finish: bool) -> Result<Vec<SseFrame>, GatewayError> {
        let mut frames = Vec::new();
        while let Some(index) = self.buffer.windows(2).position(|window| window == b"\n\n") {
            let bytes = self.buffer.drain(..index + 2).collect::<Vec<_>>();
            if let Some(frame) = parse_sse_frame(&bytes[..index])? {
                frames.push(frame);
            }
        }
        if finish && !self.buffer.is_empty() {
            let bytes = std::mem::take(&mut self.buffer);
            if let Some(frame) = parse_sse_frame(&bytes)? {
                frames.push(frame);
            }
        }
        Ok(frames)
    }
}

fn parse_sse_frame(bytes: &[u8]) -> Result<Option<SseFrame>, GatewayError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| provider_request_error(&RequestId::new(), "provider", error))?;
    let mut event = None;
    let mut data = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        }
        if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start().to_string());
        }
    }
    Ok((!data.is_empty()).then(|| SseFrame {
        event,
        data: data.join("\n"),
    }))
}

struct ProviderStreamState {
    protocol: WireProtocol,
    request_id: RequestId,
    response_id: String,
    route: ResolvedRoute,
    previous_response_id: Option<String>,
    outputs: Vec<fte_types::OutputItem>,
    text: BTreeMap<usize, String>,
    function_arguments: BTreeMap<usize, String>,
    reasoning: BTreeMap<usize, String>,
    gemini_signatures: BTreeMap<usize, Value>,
    gemini_text_outputs: HashMap<usize, usize>,
    gemini_reasoning_outputs: HashMap<usize, usize>,
    gemini_function_outputs: HashMap<(usize, usize), usize>,
    usage: GatewayUsage,
    provider_terminal: Option<GatewayResponse>,
    cancellation: CancellationToken,
}

impl ProviderStreamState {
    fn new(
        protocol: WireProtocol,
        request_id: RequestId,
        response_id: String,
        route: ResolvedRoute,
        previous_response_id: Option<String>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            protocol,
            request_id,
            response_id,
            route,
            previous_response_id,
            outputs: Vec::new(),
            text: BTreeMap::new(),
            function_arguments: BTreeMap::new(),
            reasoning: BTreeMap::new(),
            gemini_signatures: BTreeMap::new(),
            gemini_text_outputs: HashMap::new(),
            gemini_reasoning_outputs: HashMap::new(),
            gemini_function_outputs: HashMap::new(),
            usage: GatewayUsage::default(),
            provider_terminal: None,
            cancellation,
        }
    }

    async fn emit(
        &self,
        events: &mpsc::Sender<GatewayEvent>,
        event: GatewayEvent,
    ) -> Result<(), GatewayError> {
        send_provider_event(
            events,
            &self.cancellation,
            &self.request_id,
            &self.route.backend_id,
            event,
        )
        .await
    }

    async fn consume_frame(
        &mut self,
        frame: SseFrame,
        events: &mpsc::Sender<GatewayEvent>,
    ) -> Result<bool, GatewayError> {
        if frame.data == "[DONE]" {
            return Ok(true);
        }
        let value: Value = serde_json::from_str(&frame.data).map_err(|error| {
            provider_request_error(&self.request_id, &self.route.backend_id, error)
        })?;
        match self.protocol {
            WireProtocol::OpenAiResponses => {
                self.consume_openai_responses_event(value, events).await
            }
            WireProtocol::OpenAiChat | WireProtocol::OpenAiCompletion => {
                self.consume_openai_chunk(value, events).await
            }
            WireProtocol::AnthropicMessages => {
                self.consume_anthropic_event(frame.event.as_deref(), value, events)
                    .await
            }
            WireProtocol::GeminiGenerateContent => self.consume_gemini_event(value, events).await,
        }
    }

    async fn consume_openai_responses_event(
        &mut self,
        value: Value,
        events: &mpsc::Sender<GatewayEvent>,
    ) -> Result<bool, GatewayError> {
        match value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "response.output_item.added" => {
                if let (Some(index), Some(item)) =
                    (usize_field(&value, "output_index"), value.get("item"))
                {
                    let item = parse_openai_output_item(item)?;
                    ensure_output(&mut self.outputs, index, item.clone());
                    self.emit(
                        events,
                        GatewayEvent::OutputItemAdded {
                            request_id: self.request_id.clone(),
                            output_index: index,
                            item,
                        },
                    )
                    .await?;
                }
            }
            "response.content_part.added" => {
                if let (Some(output_index), Some(content_index), Some(part)) = (
                    usize_field(&value, "output_index"),
                    usize_field(&value, "content_index"),
                    value.get("part").and_then(parse_openai_content),
                ) {
                    self.emit(
                        events,
                        GatewayEvent::ContentPartAdded {
                            request_id: self.request_id.clone(),
                            output_index,
                            content_index,
                            part,
                        },
                    )
                    .await?;
                }
            }
            "response.output_text.delta" => {
                if let (Some(output_index), Some(content_index), Some(delta)) = (
                    usize_field(&value, "output_index"),
                    usize_field(&value, "content_index"),
                    value.get("delta").and_then(Value::as_str),
                ) {
                    self.text.entry(output_index).or_default().push_str(delta);
                    self.emit(
                        events,
                        GatewayEvent::TextDelta {
                            request_id: self.request_id.clone(),
                            output_index,
                            content_index,
                            delta: delta.to_string(),
                        },
                    )
                    .await?;
                }
            }
            "response.reasoning_summary_text.delta" => {
                if let (Some(output_index), Some(summary_index), Some(delta)) = (
                    usize_field(&value, "output_index"),
                    usize_field(&value, "summary_index"),
                    value.get("delta").and_then(Value::as_str),
                ) {
                    self.emit(
                        events,
                        GatewayEvent::ReasoningSummaryDelta {
                            request_id: self.request_id.clone(),
                            output_index,
                            summary_index,
                            delta: delta.to_string(),
                        },
                    )
                    .await?;
                }
            }
            "response.function_call_arguments.delta" => {
                if let (Some(output_index), Some(delta)) = (
                    usize_field(&value, "output_index"),
                    value.get("delta").and_then(Value::as_str),
                ) {
                    self.function_arguments
                        .entry(output_index)
                        .or_default()
                        .push_str(delta);
                    self.emit(
                        events,
                        GatewayEvent::FunctionArgumentsDelta {
                            request_id: self.request_id.clone(),
                            output_index,
                            delta: delta.to_string(),
                        },
                    )
                    .await?;
                }
            }
            "response.content_part.done" => {
                if let (Some(output_index), Some(content_index), Some(part)) = (
                    usize_field(&value, "output_index"),
                    usize_field(&value, "content_index"),
                    value.get("part").and_then(parse_openai_content),
                ) {
                    self.emit(
                        events,
                        GatewayEvent::ContentPartCompleted {
                            request_id: self.request_id.clone(),
                            output_index,
                            content_index,
                            part,
                        },
                    )
                    .await?;
                }
            }
            "response.output_item.done" => {
                if let (Some(index), Some(item)) =
                    (usize_field(&value, "output_index"), value.get("item"))
                {
                    let item = parse_openai_output_item(item)?;
                    ensure_output(&mut self.outputs, index, item.clone());
                    self.emit(
                        events,
                        GatewayEvent::OutputItemCompleted {
                            request_id: self.request_id.clone(),
                            output_index: index,
                            item,
                        },
                    )
                    .await?;
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(response) = value.get("response") {
                    self.provider_terminal = Some(parse_openai_responses(
                        response,
                        &self.request_id,
                        &self.response_id,
                        self.route.clone(),
                        self.previous_response_id.clone(),
                    )?);
                    return Ok(true);
                }
            }
            "error" => {
                return Err(provider_event_error(
                    &self.request_id,
                    &self.route.backend_id,
                    &value,
                ));
            }
            _ => {}
        }
        Ok(false)
    }

    async fn consume_openai_chunk(
        &mut self,
        value: Value,
        events: &mpsc::Sender<GatewayEvent>,
    ) -> Result<bool, GatewayError> {
        if let Some(usage) = value.get("usage") {
            self.usage = usage_from_openai(usage, self.route.clone());
        }
        if let Some(choices) = value.get("choices").and_then(Value::as_array) {
            for choice in choices {
                let index = choice
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as usize;
                let delta = match self.protocol {
                    WireProtocol::OpenAiChat => choice
                        .get("delta")
                        .and_then(|delta| delta.get("content"))
                        .and_then(Value::as_str),
                    _ => choice.get("text").and_then(Value::as_str),
                };
                if let Some(delta) = delta {
                    self.text.entry(index).or_default().push_str(delta);
                    self.emit(
                        events,
                        GatewayEvent::TextDelta {
                            request_id: self.request_id.clone(),
                            output_index: index,
                            content_index: 0,
                            delta: delta.to_string(),
                        },
                    )
                    .await?;
                }
            }
        }
        Ok(false)
    }

    async fn consume_anthropic_event(
        &mut self,
        event: Option<&str>,
        value: Value,
        events: &mpsc::Sender<GatewayEvent>,
    ) -> Result<bool, GatewayError> {
        match event
            .or_else(|| value.get("type").and_then(Value::as_str))
            .unwrap_or_default()
        {
            "message_start" => {
                if let Some(usage) = value.pointer("/message/usage") {
                    self.usage = usage_from_anthropic(usage, self.route.clone());
                }
            }
            "content_block_start" => {
                let index = usize_field(&value, "index").unwrap_or_default();
                if let Some(block) = value.get("content_block") {
                    let item = parse_anthropic_output_item(block, index)?;
                    ensure_output(&mut self.outputs, index, item.clone());
                    self.emit(
                        events,
                        GatewayEvent::OutputItemAdded {
                            request_id: self.request_id.clone(),
                            output_index: index,
                            item,
                        },
                    )
                    .await?;
                }
            }
            "content_block_delta" => {
                let index = usize_field(&value, "index").unwrap_or_default();
                let delta = value.get("delta").unwrap_or(&Value::Null);
                match delta
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            self.text.entry(index).or_default().push_str(text);
                            self.emit(
                                events,
                                GatewayEvent::TextDelta {
                                    request_id: self.request_id.clone(),
                                    output_index: index,
                                    content_index: 0,
                                    delta: text.to_string(),
                                },
                            )
                            .await?;
                        }
                    }
                    "thinking_delta" => {
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            self.emit(
                                events,
                                GatewayEvent::ReasoningSummaryDelta {
                                    request_id: self.request_id.clone(),
                                    output_index: index,
                                    summary_index: 0,
                                    delta: text.to_string(),
                                },
                            )
                            .await?;
                        }
                    }
                    "input_json_delta" => {
                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                            self.function_arguments
                                .entry(index)
                                .or_default()
                                .push_str(partial);
                            self.emit(
                                events,
                                GatewayEvent::FunctionArgumentsDelta {
                                    request_id: self.request_id.clone(),
                                    output_index: index,
                                    delta: partial.to_string(),
                                },
                            )
                            .await?;
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(usage) = value.get("usage") {
                    merge_anthropic_usage(&mut self.usage, usage);
                }
            }
            "message_stop" => return Ok(true),
            "error" => {
                return Err(provider_event_error(
                    &self.request_id,
                    &self.route.backend_id,
                    &value,
                ));
            }
            _ => {}
        }
        Ok(false)
    }

    async fn consume_gemini_event(
        &mut self,
        value: Value,
        events: &mpsc::Sender<GatewayEvent>,
    ) -> Result<bool, GatewayError> {
        if let Some(usage) = value.get("usageMetadata") {
            self.usage = usage_from_gemini(usage, self.route.clone());
        }
        for (candidate_index, candidate) in value
            .get("candidates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let parts = candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten();
            for (part_index, part) in parts.enumerate() {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        let output_index = *self
                            .gemini_reasoning_outputs
                            .entry(candidate_index)
                            .or_insert_with(|| {
                                let output_index = self.outputs.len();
                                self.outputs.push(fte_types::OutputItem::Reasoning {
                                    id: format!("rs_{}", Uuid::new_v4()),
                                    summary: Vec::new(),
                                    opaque_continuation: None,
                                });
                                output_index
                            });
                        self.reasoning
                            .entry(output_index)
                            .or_default()
                            .push_str(text);
                        if let Some(signature) = part
                            .get("thoughtSignature")
                            .or_else(|| part.get("thought_signature"))
                        {
                            self.gemini_signatures
                                .insert(output_index, signature.clone());
                        }
                        self.emit(
                            events,
                            GatewayEvent::ReasoningSummaryDelta {
                                request_id: self.request_id.clone(),
                                output_index,
                                summary_index: 0,
                                delta: text.to_string(),
                            },
                        )
                        .await?;
                    } else {
                        let output_index = *self
                            .gemini_text_outputs
                            .entry(candidate_index)
                            .or_insert_with(|| {
                                let output_index = self.outputs.len();
                                self.outputs.push(fte_types::OutputItem::Message {
                                    id: format!("msg_{}", Uuid::new_v4()),
                                    role: fte_types::MessageRole::Assistant,
                                    content: Vec::new(),
                                });
                                output_index
                            });
                        self.text.entry(output_index).or_default().push_str(text);
                        self.emit(
                            events,
                            GatewayEvent::TextDelta {
                                request_id: self.request_id.clone(),
                                output_index,
                                content_index: 0,
                                delta: text.to_string(),
                            },
                        )
                        .await?;
                    }
                }
                if let Some(call) = part.get("functionCall") {
                    let output_index = *self
                        .gemini_function_outputs
                        .entry((candidate_index, part_index))
                        .or_insert_with(|| {
                            let output_index = self.outputs.len();
                            self.outputs.push(fte_types::OutputItem::FunctionCall {
                                id: format!("fc_{}", Uuid::new_v4()),
                                call_id: format!("call_{}", Uuid::new_v4()),
                                name: call
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("function")
                                    .to_string(),
                                arguments: Value::Object(Map::new()),
                            });
                            output_index
                        });
                    let arguments = call
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Map::new()));
                    if let Some(fte_types::OutputItem::FunctionCall {
                        arguments: stored, ..
                    }) = self.outputs.get_mut(output_index)
                    {
                        *stored = arguments.clone();
                    }
                    self.emit(
                        events,
                        GatewayEvent::FunctionArgumentsDelta {
                            request_id: self.request_id.clone(),
                            output_index,
                            delta: arguments.to_string(),
                        },
                    )
                    .await?;
                }
            }
        }
        Ok(false)
    }

    async fn finish(
        mut self,
        events: &mpsc::Sender<GatewayEvent>,
    ) -> Result<GatewayResponse, GatewayError> {
        if let Some(response) = self.provider_terminal.take() {
            return Ok(response);
        }
        if self.outputs.is_empty() {
            for (index, text) in &self.text {
                ensure_output(
                    &mut self.outputs,
                    *index,
                    fte_types::OutputItem::Message {
                        id: format!("msg_{}", Uuid::new_v4()),
                        role: fte_types::MessageRole::Assistant,
                        content: vec![fte_types::ContentBlock::Text { text: text.clone() }],
                    },
                );
            }
        }
        for (index, text) in self.text {
            if let Some(fte_types::OutputItem::Message { content, .. }) =
                self.outputs.get_mut(index)
            {
                *content = vec![fte_types::ContentBlock::Text { text }];
            }
        }
        for (index, arguments) in self.function_arguments {
            if let Some(fte_types::OutputItem::FunctionCall {
                arguments: stored, ..
            }) = self.outputs.get_mut(index)
            {
                *stored = serde_json::from_str(&arguments).map_err(|error| {
                    provider_request_error(&self.request_id, &self.route.backend_id, error)
                })?;
            }
        }
        for (index, reasoning) in self.reasoning {
            if let Some(fte_types::OutputItem::Reasoning {
                summary,
                opaque_continuation,
                ..
            }) = self.outputs.get_mut(index)
            {
                *summary = vec![reasoning.clone()];
                *opaque_continuation = self.gemini_signatures.get(&index).map(|signature| {
                    json!({
                        "role":"model",
                        "parts":[{
                            "text":reasoning,
                            "thought":true,
                            "thoughtSignature":signature
                        }]
                    })
                });
            }
        }
        let response = GatewayResponse {
            id: self.response_id,
            request_id: self.request_id.clone(),
            model: self.route.model_id.clone(),
            route: self.route,
            output: self.outputs,
            usage: self.usage,
            status: TerminalStatus::Completed,
            previous_response_id: self.previous_response_id,
        };
        emit_completed_lifecycle(
            events,
            &self.cancellation,
            &self.request_id,
            &response.route.backend_id,
            &response,
        )
        .await?;
        Ok(response)
    }
}

fn parse_provider_response(
    protocol: WireProtocol,
    value: &Value,
    request_id: &RequestId,
    response_id: &str,
    route: ResolvedRoute,
    previous: Option<String>,
) -> Result<GatewayResponse, GatewayError> {
    match protocol {
        WireProtocol::OpenAiResponses => {
            parse_openai_responses(value, request_id, response_id, route, previous)
        }
        WireProtocol::OpenAiChat => {
            parse_openai_chat(value, request_id, response_id, route, previous)
        }
        WireProtocol::OpenAiCompletion => {
            parse_openai_completion(value, request_id, response_id, route, previous)
        }
        WireProtocol::AnthropicMessages => {
            parse_anthropic(value, request_id, response_id, route, previous)
        }
        WireProtocol::GeminiGenerateContent => {
            parse_gemini(value, request_id, response_id, route, previous)
        }
    }
}

fn parse_openai_responses(
    value: &Value,
    request_id: &RequestId,
    response_id: &str,
    route: ResolvedRoute,
    previous: Option<String>,
) -> Result<GatewayResponse, GatewayError> {
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            provider_response_error(request_id, &route.backend_id, "Responses output is missing")
        })?
        .iter()
        .map(parse_openai_output_item)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(GatewayResponse {
        id: response_id.to_string(),
        request_id: request_id.clone(),
        model: value
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&route.model_id)
            .to_string(),
        route: route.clone(),
        output,
        usage: usage_from_openai(value.get("usage").unwrap_or(&Value::Null), route),
        status: if value.get("status").and_then(Value::as_str) == Some("cancelled") {
            TerminalStatus::Cancelled
        } else {
            TerminalStatus::Completed
        },
        previous_response_id: previous,
    })
}

fn parse_openai_chat(
    value: &Value,
    request_id: &RequestId,
    response_id: &str,
    route: ResolvedRoute,
    previous: Option<String>,
) -> Result<GatewayResponse, GatewayError> {
    let choices = value
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            provider_response_error(request_id, &route.backend_id, "Chat choices are missing")
        })?;
    let output = choices
        .iter()
        .map(|choice| fte_types::OutputItem::Message {
            id: format!("msg_{}", Uuid::new_v4()),
            role: fte_types::MessageRole::Assistant,
            content: vec![fte_types::ContentBlock::Text {
                text: choice
                    .pointer("/message/content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }],
        })
        .collect();
    Ok(hosted_response(
        value,
        request_id,
        response_id,
        route,
        previous,
        output,
    ))
}

fn parse_openai_completion(
    value: &Value,
    request_id: &RequestId,
    response_id: &str,
    route: ResolvedRoute,
    previous: Option<String>,
) -> Result<GatewayResponse, GatewayError> {
    let choices = value
        .get("choices")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            provider_response_error(
                request_id,
                &route.backend_id,
                "Completion choices are missing",
            )
        })?;
    let output = choices
        .iter()
        .map(|choice| fte_types::OutputItem::Message {
            id: format!("msg_{}", Uuid::new_v4()),
            role: fte_types::MessageRole::Assistant,
            content: vec![fte_types::ContentBlock::Text {
                text: choice
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }],
        })
        .collect();
    Ok(hosted_response(
        value,
        request_id,
        response_id,
        route,
        previous,
        output,
    ))
}

fn hosted_response(
    value: &Value,
    request_id: &RequestId,
    response_id: &str,
    route: ResolvedRoute,
    previous: Option<String>,
    output: Vec<fte_types::OutputItem>,
) -> GatewayResponse {
    GatewayResponse {
        id: response_id.to_string(),
        request_id: request_id.clone(),
        model: value
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&route.model_id)
            .to_string(),
        route: route.clone(),
        output,
        usage: usage_from_openai(value.get("usage").unwrap_or(&Value::Null), route),
        status: TerminalStatus::Completed,
        previous_response_id: previous,
    }
}

fn parse_anthropic(
    value: &Value,
    request_id: &RequestId,
    response_id: &str,
    route: ResolvedRoute,
    previous: Option<String>,
) -> Result<GatewayResponse, GatewayError> {
    let output = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            provider_response_error(
                request_id,
                &route.backend_id,
                "Anthropic content is missing",
            )
        })?
        .iter()
        .enumerate()
        .map(|(index, block)| parse_anthropic_output_item(block, index))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(GatewayResponse {
        id: response_id.to_string(),
        request_id: request_id.clone(),
        model: value
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&route.model_id)
            .to_string(),
        route: route.clone(),
        output,
        usage: usage_from_anthropic(value.get("usage").unwrap_or(&Value::Null), route),
        status: TerminalStatus::Completed,
        previous_response_id: previous,
    })
}

fn parse_gemini(
    value: &Value,
    request_id: &RequestId,
    response_id: &str,
    route: ResolvedRoute,
    previous: Option<String>,
) -> Result<GatewayResponse, GatewayError> {
    let candidates = value
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            provider_response_error(
                request_id,
                &route.backend_id,
                "Gemini candidates are missing",
            )
        })?;
    let mut output = Vec::new();
    for candidate in candidates {
        let mut text = String::new();
        for part in candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(value) = part.get("text").and_then(Value::as_str) {
                if part.get("thought").and_then(Value::as_bool) == Some(true) {
                    let signature = part
                        .get("thoughtSignature")
                        .or_else(|| part.get("thought_signature"))
                        .cloned();
                    output.push(fte_types::OutputItem::Reasoning {
                        id: format!("rs_{}", Uuid::new_v4()),
                        summary: vec![value.to_string()],
                        opaque_continuation: signature.map(|signature| {
                            json!({
                                "role":"model",
                                "parts":[{
                                    "text":value,
                                    "thought":true,
                                    "thoughtSignature":signature
                                }]
                            })
                        }),
                    });
                } else {
                    text.push_str(value);
                }
            }
            if let Some(call) = part.get("functionCall") {
                output.push(fte_types::OutputItem::FunctionCall {
                    id: format!("fc_{}", Uuid::new_v4()),
                    call_id: format!("call_{}", Uuid::new_v4()),
                    name: call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("function")
                        .to_string(),
                    arguments: call
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Map::new())),
                });
            }
        }
        if !text.is_empty() {
            output.push(fte_types::OutputItem::Message {
                id: format!("msg_{}", Uuid::new_v4()),
                role: fte_types::MessageRole::Assistant,
                content: vec![fte_types::ContentBlock::Text { text }],
            });
        }
    }
    Ok(GatewayResponse {
        id: response_id.to_string(),
        request_id: request_id.clone(),
        model: route.model_id.clone(),
        route: route.clone(),
        output,
        usage: usage_from_gemini(value.get("usageMetadata").unwrap_or(&Value::Null), route),
        status: TerminalStatus::Completed,
        previous_response_id: previous,
    })
}

fn parse_openai_output_item(value: &Value) -> Result<fte_types::OutputItem, GatewayError> {
    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "message" => Ok(fte_types::OutputItem::Message {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("message")
                .to_string(),
            role: fte_types::MessageRole::Assistant,
            content: value
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(parse_openai_content)
                .collect(),
        }),
        "function_call" => Ok(fte_types::OutputItem::FunctionCall {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("function")
                .to_string(),
            call_id: value
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            arguments: value
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|arguments| serde_json::from_str(arguments).ok())
                .unwrap_or_else(|| json!({})),
        }),
        "reasoning" => Ok(fte_types::OutputItem::Reasoning {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("reasoning")
                .to_string(),
            summary: value
                .get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect(),
            opaque_continuation: value.get("encrypted_content").cloned(),
        }),
        _ => Err(provider_response_error(
            &RequestId::new(),
            "openai",
            "OpenAI returned an unsupported output Item",
        )),
    }
}

fn parse_openai_content(value: &Value) -> Option<fte_types::ContentBlock> {
    match value.get("type").and_then(Value::as_str)? {
        "output_text" => Some(fte_types::ContentBlock::Text {
            text: value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }),
        "reasoning_text" => Some(fte_types::ContentBlock::Thinking {
            text: value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            signature: value
                .get("signature")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        }),
        _ => None,
    }
}

fn parse_anthropic_output_item(
    value: &Value,
    index: usize,
) -> Result<fte_types::OutputItem, GatewayError> {
    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "text" => Ok(fte_types::OutputItem::Message {
            id: format!("msg_{index}"),
            role: fte_types::MessageRole::Assistant,
            content: vec![fte_types::ContentBlock::Text {
                text: value
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }],
        }),
        "tool_use" => Ok(fte_types::OutputItem::FunctionCall {
            id: format!("tool_{index}"),
            call_id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            arguments: value.get("input").cloned().unwrap_or_else(|| json!({})),
        }),
        "thinking" => Ok(fte_types::OutputItem::Reasoning {
            id: format!("reasoning_{index}"),
            summary: vec![
                value
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ],
            opaque_continuation: Some(json!({
                "thinking":value.get("thinking"),
                "signature":value.get("signature")
            })),
        }),
        "redacted_thinking" => Ok(fte_types::OutputItem::Reasoning {
            id: format!("reasoning_{index}"),
            summary: Vec::new(),
            opaque_continuation: Some(json!({"redacted_thinking":value.get("data")})),
        }),
        _ => Err(provider_response_error(
            &RequestId::new(),
            "anthropic",
            "Anthropic returned an unsupported content block",
        )),
    }
}

async fn emit_completed_lifecycle(
    events: &mpsc::Sender<GatewayEvent>,
    cancellation: &CancellationToken,
    request_id: &RequestId,
    provider: &str,
    response: &GatewayResponse,
) -> Result<(), GatewayError> {
    for (output_index, item) in response.output.iter().enumerate() {
        send_provider_event(
            events,
            cancellation,
            request_id,
            provider,
            GatewayEvent::OutputItemAdded {
                request_id: request_id.clone(),
                output_index,
                item: item.clone(),
            },
        )
        .await?;
        if let fte_types::OutputItem::Message { content, .. } = item {
            for (content_index, part) in content.iter().enumerate() {
                send_provider_event(
                    events,
                    cancellation,
                    request_id,
                    provider,
                    GatewayEvent::ContentPartAdded {
                        request_id: request_id.clone(),
                        output_index,
                        content_index,
                        part: part.clone(),
                    },
                )
                .await?;
                send_provider_event(
                    events,
                    cancellation,
                    request_id,
                    provider,
                    GatewayEvent::ContentPartCompleted {
                        request_id: request_id.clone(),
                        output_index,
                        content_index,
                        part: part.clone(),
                    },
                )
                .await?;
            }
        }
        send_provider_event(
            events,
            cancellation,
            request_id,
            provider,
            GatewayEvent::OutputItemCompleted {
                request_id: request_id.clone(),
                output_index,
                item: item.clone(),
            },
        )
        .await?;
    }
    send_provider_event(
        events,
        cancellation,
        request_id,
        provider,
        GatewayEvent::UsageUpdated {
            request_id: request_id.clone(),
            usage: response.usage.clone(),
        },
    )
    .await
}

fn ensure_output(
    outputs: &mut Vec<fte_types::OutputItem>,
    index: usize,
    item: fte_types::OutputItem,
) {
    while outputs.len() <= index {
        outputs.push(fte_types::OutputItem::Message {
            id: format!("msg_{}", Uuid::new_v4()),
            role: fte_types::MessageRole::Assistant,
            content: Vec::new(),
        });
    }
    outputs[index] = item;
}

fn usize_field(value: &Value, name: &str) -> Option<usize> {
    value
        .get(name)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
}

fn usage_from_openai(value: &Value, route: ResolvedRoute) -> GatewayUsage {
    let input = value
        .get("input_tokens")
        .or_else(|| value.get("prompt_tokens"))
        .and_then(Value::as_u64);
    let output = value
        .get("output_tokens")
        .or_else(|| value.get("completion_tokens"))
        .and_then(Value::as_u64);
    let cached = value
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64);
    GatewayUsage {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: value
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
        cache_read_tokens: cached,
        cache_write_tokens: None,
        provenance: fte_types::UsageProvenance::Exact,
        selected_route: Some(route),
        cache: Some(fte_types::CacheReceipt {
            tier: fte_types::CacheTier::ProviderNative,
            outcome: if cached.unwrap_or_default() > 0 {
                fte_types::CacheOutcome::Hit
            } else {
                fte_types::CacheOutcome::Miss
            },
            reason: None,
        }),
        ..GatewayUsage::default()
    }
}

fn usage_from_anthropic(value: &Value, route: ResolvedRoute) -> GatewayUsage {
    let cached = value.get("cache_read_input_tokens").and_then(Value::as_u64);
    GatewayUsage {
        input_tokens: value.get("input_tokens").and_then(Value::as_u64),
        output_tokens: value.get("output_tokens").and_then(Value::as_u64),
        cache_read_tokens: cached,
        cache_write_tokens: value
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64),
        provenance: fte_types::UsageProvenance::Exact,
        selected_route: Some(route),
        cache: Some(fte_types::CacheReceipt {
            tier: fte_types::CacheTier::ProviderNative,
            outcome: if cached.unwrap_or_default() > 0 {
                fte_types::CacheOutcome::Hit
            } else {
                fte_types::CacheOutcome::Miss
            },
            reason: None,
        }),
        ..GatewayUsage::default()
    }
}

fn usage_from_gemini(value: &Value, route: ResolvedRoute) -> GatewayUsage {
    GatewayUsage {
        input_tokens: value.get("promptTokenCount").and_then(Value::as_u64),
        output_tokens: value
            .get("candidatesTokenCount")
            .or_else(|| value.get("totalCandidatesTokenCount"))
            .and_then(Value::as_u64),
        reasoning_tokens: value.get("thoughtsTokenCount").and_then(Value::as_u64),
        cache_read_tokens: value.get("cachedContentTokenCount").and_then(Value::as_u64),
        cache_write_tokens: None,
        provenance: fte_types::UsageProvenance::Exact,
        selected_route: Some(route),
        cache: None,
        ..GatewayUsage::default()
    }
}

fn merge_anthropic_usage(usage: &mut GatewayUsage, value: &Value) {
    if let Some(output) = value.get("output_tokens").and_then(Value::as_u64) {
        usage.output_tokens = Some(output);
    }
    if let Some(input) = value.get("input_tokens").and_then(Value::as_u64) {
        usage.input_tokens = Some(input);
    }
}

async fn map_http_error(
    request_id: &RequestId,
    provider: &str,
    response: reqwest::Response,
) -> GatewayError {
    let status = response.status();
    let mut body = response.bytes_stream();
    let mut read = 0_usize;
    while let Some(Ok(chunk)) = body.next().await {
        read = read.saturating_add(chunk.len());
        if read >= MAX_ERROR_BODY_BYTES {
            break;
        }
    }
    let class = match status.as_u16() {
        401 => ErrorClass::Authentication,
        403 => ErrorClass::Authorization,
        429 => ErrorClass::RateLimit,
        400..=499 => ErrorClass::InvalidRequest,
        _ => ErrorClass::Provider,
    };
    GatewayError {
        code: "provider_http_error".to_string(),
        class,
        retryable: status.as_u16() == 429 || status.is_server_error(),
        http_status: status.as_u16(),
        request_id: request_id.clone(),
        provider: Some(provider.to_string()),
        safe_detail: format!("the hosted provider returned HTTP {status}"),
    }
}

fn map_transport_error(
    request_id: &RequestId,
    provider: &str,
    error: reqwest::Error,
) -> GatewayError {
    GatewayError {
        code: if error.is_timeout() {
            "provider_timeout"
        } else {
            "provider_transport_error"
        }
        .to_string(),
        class: if error.is_timeout() {
            ErrorClass::Timeout
        } else {
            ErrorClass::Unavailable
        },
        retryable: true,
        http_status: if error.is_timeout() { 504 } else { 503 },
        request_id: request_id.clone(),
        provider: Some(provider.to_string()),
        safe_detail: if error.is_timeout() {
            "the hosted provider request timed out"
        } else {
            "the hosted provider connection failed"
        }
        .to_string(),
    }
}

fn provider_request_error(
    request_id: &RequestId,
    provider: &str,
    error: impl std::fmt::Display,
) -> GatewayError {
    GatewayError {
        code: "provider_request_invalid".to_string(),
        class: ErrorClass::InvalidRequest,
        retryable: false,
        http_status: 400,
        request_id: request_id.clone(),
        provider: Some(provider.to_string()),
        safe_detail: format!("the provider request could not be encoded: {error}"),
    }
}

fn provider_response_error(request_id: &RequestId, provider: &str, detail: &str) -> GatewayError {
    GatewayError {
        code: "provider_response_invalid".to_string(),
        class: ErrorClass::Provider,
        retryable: false,
        http_status: 502,
        request_id: request_id.clone(),
        provider: Some(provider.to_string()),
        safe_detail: detail.to_string(),
    }
}

fn provider_event_error(request_id: &RequestId, provider: &str, value: &Value) -> GatewayError {
    GatewayError {
        code: value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("provider_stream_error")
            .to_string(),
        class: ErrorClass::Provider,
        retryable: false,
        http_status: 502,
        request_id: request_id.clone(),
        provider: Some(provider.to_string()),
        safe_detail: "the hosted provider stream returned an error".to_string(),
    }
}

fn capability_error(
    request_id: &RequestId,
    provider: &str,
    code: &str,
    detail: &str,
) -> GatewayError {
    GatewayError {
        code: code.to_string(),
        class: ErrorClass::Capability,
        retryable: false,
        http_status: 400,
        request_id: request_id.clone(),
        provider: Some(provider.to_string()),
        safe_detail: detail.to_string(),
    }
}

fn cancelled_error(request_id: &RequestId, provider: &str) -> GatewayError {
    GatewayError {
        code: "request_cancelled".to_string(),
        class: ErrorClass::Cancelled,
        retryable: false,
        http_status: 499,
        request_id: request_id.clone(),
        provider: Some(provider.to_string()),
        safe_detail: "the hosted request was cancelled".to_string(),
    }
}

fn timeout_error(request_id: &RequestId, provider: &str, code: &str, detail: &str) -> GatewayError {
    GatewayError {
        code: code.to_string(),
        class: ErrorClass::Timeout,
        retryable: true,
        http_status: 504,
        request_id: request_id.clone(),
        provider: Some(provider.to_string()),
        safe_detail: detail.to_string(),
    }
}

fn provider_internal(provider: &str, code: &str, error: impl std::fmt::Display) -> GatewayError {
    GatewayError {
        code: code.to_string(),
        class: ErrorClass::Internal,
        retryable: false,
        http_status: 500,
        request_id: RequestId::new(),
        provider: Some(provider.to_string()),
        safe_detail: format!("hosted provider state failed: {error}"),
    }
}

fn validate_provider_extensions(
    request: &BackendRequest,
    allowed: &[&str],
) -> Result<(), GatewayError> {
    if let Some(name) = request
        .request
        .provider_extensions
        .keys()
        .find(|name| !allowed.contains(&name.as_str()))
    {
        return Err(GatewayError::invalid_request(
            &request.request.request_id,
            "hosted_provider_extension_unsupported",
            &format!("the selected hosted protocol does not accept provider extension {name}"),
        ));
    }
    Ok(())
}

fn apply_anthropic_request_headers(
    headers: &mut HeaderMap,
    request: &fte_types::GatewayRequest,
) -> Result<(), GatewayError> {
    let Some(beta) = request.provider_extensions.get("anthropic.beta") else {
        return Ok(());
    };
    let Some(beta) = beta.as_str() else {
        return Err(GatewayError::invalid_request(
            &request.request_id,
            "anthropic_beta_invalid",
            "anthropic_beta must be a string",
        ));
    };
    headers.insert(
        HeaderName::from_static("anthropic-beta"),
        HeaderValue::from_str(beta)
            .map_err(|error| provider_request_error(&request.request_id, "anthropic", error))?,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "unstable-w1-vertical-tests")]
    mod w1_source_tree {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/w1_source_tree.rs"
        ));
    }

    use super::*;
    use fte_types::{
        CachePolicy, DeadlinePolicy, GenerationInput, ModelSelector, PrivacyPolicy, ResponseFormat,
        RoutingPolicy, SamplingOptions, StoragePolicy, StreamPolicy, ToolPolicy,
    };
    #[cfg(feature = "unstable-w1-vertical-tests")]
    use platform_vertical_fixtures_v0::{
        EquivalenceProjectionV0, ObservationEnvelopeV0, VerticalFixtureManifestV0, sha256_identity,
        validate_baseline, validate_manifest,
    };

    #[cfg(feature = "unstable-w1-vertical-tests")]
    const W1_CORPUS_BYTES: &[u8] =
        include_bytes!("../../../tests/fixtures/w1/v0/fte-hosted-loopback.json");
    #[cfg(feature = "unstable-w1-vertical-tests")]
    const W1_MANIFEST_BYTES: &[u8] =
        include_bytes!("../../../tests/fixtures/w1/v0/fte-hosted-loopback.manifest.json");
    #[cfg(feature = "unstable-w1-vertical-tests")]
    const W1_HOSTED_PROJECTION_BYTES: &[u8] =
        include_bytes!("../../../tests/fixtures/w1/v0/fte-hosted-projection.json");

    fn request(input: GenerationInput) -> BackendRequest {
        BackendRequest {
            request: fte_types::GatewayRequest {
                request_id: RequestId::new(),
                client_id: "test".to_string(),
                model: ModelSelector::ExactModel {
                    model_id: "model".to_string(),
                },
                input,
                sampling: SamplingOptions::default(),
                response_format: ResponseFormat::Text,
                tools: Vec::new(),
                tool_policy: ToolPolicy::default(),
                cache: CachePolicy::default(),
                routing: RoutingPolicy {
                    privacy: PrivacyPolicy::HostedAllowed,
                    ..RoutingPolicy::default()
                },
                storage: StoragePolicy::default(),
                deadline: DeadlinePolicy::default(),
                stream: StreamPolicy::default(),
                provider_extensions: BTreeMap::new(),
            },
            route: ResolvedRoute {
                backend_id: "provider".to_string(),
                model_id: "model".to_string(),
                display_name: "Model".to_string(),
                location: BackendLocation::Hosted,
                catalog_version: "test".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn hosted_shutdown_cancels_and_waits_for_registered_operations() {
        let activity = Arc::new(HostedActivity::default());
        let request_id = RequestId("active-request".to_string());
        let operation = activity
            .register(&request_id)
            .expect("register hosted operation");
        let cancellation = operation.cancellation();
        let activity_for_shutdown = Arc::clone(&activity);
        let shutdown = tokio::spawn(async move {
            activity_for_shutdown
                .shutdown()
                .await
                .expect("drain hosted operations");
        });

        tokio::task::yield_now().await;
        assert!(cancellation.is_cancelled());
        assert!(!shutdown.is_finished());
        let error = match activity.register(&RequestId::new()) {
            Ok(_) => panic!("quiescing providers must reject new work"),
            Err(error) => error,
        };
        assert_eq!(error.code, "provider_quiescing");

        drop(operation);
        shutdown.await.expect("shutdown task");
        activity
            .shutdown()
            .await
            .expect("shutdown is idempotent after drain");
    }

    #[tokio::test]
    async fn hosted_shutdown_interrupts_an_undrained_event_send() {
        let activity = Arc::new(HostedActivity::default());
        let request_id = RequestId("blocked-event-send".to_string());
        let operation = activity
            .register(&request_id)
            .expect("register hosted operation");
        let cancellation = operation.cancellation();
        let (events, _undrained) = mpsc::channel(1);
        events
            .send(GatewayEvent::Warning {
                request_id: request_id.clone(),
                code: "fill_channel".to_string(),
                message: "fill the bounded channel".to_string(),
            })
            .await
            .expect("fill event channel");

        let request_for_send = request_id.clone();
        let blocked_send = tokio::spawn(async move {
            let _operation = operation;
            send_provider_event(
                &events,
                &cancellation,
                &request_for_send,
                "provider",
                GatewayEvent::Warning {
                    request_id: request_for_send.clone(),
                    code: "blocked".to_string(),
                    message: "this send must be interrupted by shutdown".to_string(),
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!blocked_send.is_finished());

        tokio::time::timeout(Duration::from_secs(1), activity.shutdown())
            .await
            .expect("shutdown must interrupt a full event channel")
            .expect("shutdown hosted activity");
        let error = blocked_send
            .await
            .expect("blocked send task")
            .expect_err("shutdown cancellation must interrupt the send");
        assert_eq!(error.code, "request_cancelled");
    }

    #[tokio::test]
    async fn hosted_terminal_reservation_survives_a_full_event_channel() {
        let request_id = RequestId("reserved-terminal".to_string());
        let (events, mut receiver) = mpsc::channel(2);
        let terminal_permit = events
            .clone()
            .reserve_owned()
            .await
            .expect("reserve terminal capacity");
        events
            .send(GatewayEvent::Warning {
                request_id: request_id.clone(),
                code: "fill_channel".to_string(),
                message: "consume every ordinary permit".to_string(),
            })
            .await
            .expect("fill ordinary event capacity");
        let terminal_observed = AtomicBool::new(false);
        let cancelled = Err(cancelled_error(&request_id, "provider"));
        enqueue_reserved_terminal(
            terminal_permit,
            terminal_event(&request_id, &cancelled),
            &terminal_observed,
        );
        drop(events);

        let mut terminal_count = 0;
        while let Some(event) = receiver.recv().await {
            terminal_count += usize::from(event.is_terminal());
        }
        assert_eq!(terminal_count, 1);
        assert!(terminal_observed.load(Ordering::Acquire));
    }

    #[test]
    fn responses_keep_typed_items_and_previous_response_affinity() {
        let mut request = request(GenerationInput::Chat {
            items: vec![fte_types::InputItem::FunctionCall {
                id: None,
                call_id: "call_1".to_string(),
                name: "weather".to_string(),
                arguments: json!({"city":"Boston"}),
            }],
        });
        request.request.storage.previous_response_id = Some("resp_previous".to_string());
        let body = openai_responses_body(&request).expect("body");
        assert_eq!(body["previous_response_id"], "resp_previous");
        assert_eq!(body["input"][0]["type"], "function_call");
    }

    #[test]
    fn raw_completion_whitespace_and_tokens_are_not_reencoded() {
        let text_request = request(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: "  exact\n".to_string(),
                add_bos: false,
            }],
        });
        let body = openai_completion_body(&text_request).expect("body");
        assert_eq!(body["prompt"], "  exact\n");
        let token_request = request(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Tokens {
                token_ids: vec![1, 2, 3],
            }],
        });
        let body = openai_completion_body(&token_request).expect("body");
        assert_eq!(body["prompt"], json!([1, 2, 3]));
    }

    #[cfg(feature = "unstable-w1-vertical-tests")]
    fn w1_hosted_request_bodies(fixture: &Value) -> (Value, Value, Value) {
        let chat_text = fixture["hosted"]["chat"]["text"]
            .as_str()
            .expect("chat fixture text");
        let chat = request(GenerationInput::Chat {
            items: vec![fte_types::InputItem::Message {
                id: None,
                role: fte_types::MessageRole::User,
                content: vec![fte_types::ContentBlock::Text {
                    text: chat_text.to_string(),
                }],
            }],
        });
        let chat_body = openai_chat_body(&chat).expect("fixture chat body");

        let raw_text = fixture["hosted"]["raw_text"]["prompt"]
            .as_str()
            .expect("raw text fixture");
        let raw = request(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: raw_text.to_string(),
                add_bos: false,
            }],
        });
        let raw_body = openai_completion_body(&raw).expect("fixture raw body");

        let token_ids = fixture["hosted"]["raw_tokens"]["prompt"]
            .as_array()
            .expect("token fixture")
            .iter()
            .map(|value| {
                i32::try_from(value.as_i64().expect("integer token"))
                    .expect("token fixture must fit i32")
            })
            .collect();
        let raw_tokens = request(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Tokens { token_ids }],
        });
        let token_body = openai_completion_body(&raw_tokens).expect("fixture token body");

        (chat_body, raw_body, token_body)
    }

    #[cfg(feature = "unstable-w1-vertical-tests")]
    fn digest_json(bytes: &[u8]) -> Value {
        serde_json::to_value(sha256_identity("fact", bytes).digest).expect("digest JSON")
    }

    #[cfg(feature = "unstable-w1-vertical-tests")]
    fn validate_w1_hosted_projection(
        fixture: &Value,
        chat_body: &Value,
        raw_body: &Value,
        token_body: &Value,
    ) {
        let manifest: VerticalFixtureManifestV0 =
            serde_json::from_slice(W1_MANIFEST_BYTES).expect("W1 vertical manifest");
        validate_manifest(&manifest).expect("valid W1 vertical manifest");
        let case = manifest
            .cases
            .iter()
            .find(|case| case.case_id == "hosted.contract")
            .expect("hosted contract case");
        let input = &case.inputs[0].identity;
        assert_eq!(
            sha256_identity(input.id.clone(), W1_CORPUS_BYTES),
            *input,
            "manifest must authenticate the complete hosted corpus"
        );

        assert_eq!(
            sha256_identity(
                case.source.production_tree.id.clone(),
                w1_source_tree::BYTES
            ),
            case.source.production_tree,
            "manifest must authenticate the complete production source descriptor"
        );
        w1_source_tree::verify();

        let response = &fixture["hosted"]["response_projection"];
        let stream_bytes = response["stream_frames"]
            .as_array()
            .expect("stream frames")
            .iter()
            .map(|frame| frame.as_str().expect("stream frame"))
            .collect::<String>();
        let corpus_identity = serde_json::to_value(input).expect("corpus identity JSON");
        let actual_projection: EquivalenceProjectionV0 = serde_json::from_value(json!({
            "ordered_events": [{
                "sequence": 0,
                "operation_id": "fte.hosted.contract",
                "attempt_id": "fixture.attempt.1",
                "correlation_id": "fixture.request.w1",
                "kind": "completed",
                "payload": corpus_identity,
            }],
            "durable_state": [{
                "state_id": "hosted.fixture.corpus",
                "schema_id": "delysis.w1.fte.hosted.corpus.v0",
                "before": corpus_identity,
                "after": corpus_identity,
                "disposition": "unchanged",
            }],
            "lifecycle": [{
                "operation_id": "fte.hosted.contract",
                "attempt_id": "fixture.attempt.1",
                "correlation_id": "fixture.request.w1",
                "terminal": "completed",
                "released": true,
            }],
            "ownership": {
                "active_operations": 0,
                "retained_tasks": 0,
                "expected_workers": 0,
                "joined_workers": 0,
            },
            "output_facts": {
                "chat_request_digest": {"kind": "digest", "value": digest_json(&serde_json::to_vec(chat_body).expect("chat bytes"))},
                "raw_text_request_digest": {"kind": "digest", "value": digest_json(&serde_json::to_vec(raw_body).expect("raw bytes"))},
                "raw_token_request_digest": {"kind": "digest", "value": digest_json(&serde_json::to_vec(token_body).expect("token bytes"))},
                "chat_response_digest": {"kind": "digest", "value": digest_json(&serde_json::to_vec(&response["chat"]).expect("chat response bytes"))},
                "completion_response_digest": {"kind": "digest", "value": digest_json(&serde_json::to_vec(&response["completion"]).expect("completion response bytes"))},
                "stream_frames_digest": {"kind": "digest", "value": digest_json(stream_bytes.as_bytes())},
                "error_fixture_digest": {"kind": "digest", "value": digest_json(&serde_json::to_vec(&response["error"]).expect("error bytes"))},
                "request_identity_digest": {"kind": "digest", "value": digest_json(response["request_id"].as_str().expect("request ID").as_bytes())},
                "credential_required": {"kind": "boolean", "value": false},
                "hosted_request_sent": {"kind": "boolean", "value": false},
                "raw_completion_distinct": {"kind": "boolean", "value": true},
                "request_identity_bound": {"kind": "boolean", "value": true},
                "unsafe_error_redacted": {"kind": "boolean", "value": true},
            },
            "fail_closed_facts": [
                "no credential was resolved",
                "no hosted network request was sent",
                "unsupported raw completion mutation was rejected",
                "provider error detail did not escape",
            ],
        }))
        .expect("actual hosted projection");
        let expected_projection: EquivalenceProjectionV0 =
            serde_json::from_slice(W1_HOSTED_PROJECTION_BYTES).expect("expected projection");
        assert_eq!(
            actual_projection, expected_projection,
            "hosted projection drift"
        );
        let observation: ObservationEnvelopeV0 = serde_json::from_value(json!({
            "schema": "delysis.vertical_observation.v0",
            "vertical_id": "fte_hosted_fixture_loopback",
            "case_id": case.case_id,
            "implementation_revision": case.source.commit,
            "observed_prerequisites": [],
            "evidence": {
                "schema": "delysis.evidence_claim.v0",
                "tier": "reproducible",
                "threat_model": "deterministic hosted protocol translation only; no credential or hosted request",
                "exact_source": case.source.production_tree.digest,
                "exact_runtime_or_artifact": input.digest,
                "execution_kind": "fixture",
                "omitted_claims": manifest.omitted_claims,
                "negative_evidence": [],
            },
            "projection": actual_projection,
        }))
        .expect("hosted observation");
        validate_baseline(
            &manifest,
            &case.case_id,
            W1_HOSTED_PROJECTION_BYTES,
            &[],
            &observation,
        )
        .expect("hosted production behavior matches authenticated W1 projection");
    }

    #[cfg(feature = "unstable-w1-vertical-tests")]
    #[test]
    fn w1_hosted_fixture_preserves_chat_and_raw_without_live_authority() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/w1/v0/fte-hosted-loopback.json"
        ))
        .expect("parse W1 hosted fixture");
        assert_eq!(fixture["execution"]["kind"], "deterministic_fixture");
        assert_eq!(fixture["execution"]["network"], "loopback_only");
        assert_eq!(fixture["execution"]["hosted_request_sent"], false);
        assert_eq!(fixture["execution"]["credential_required"], false);

        let (chat_body, raw_body, token_body) = w1_hosted_request_bodies(&fixture);
        assert_eq!(chat_body, fixture["hosted"]["chat"]["expected_body"]);

        let raw_text = fixture["hosted"]["raw_text"]["prompt"]
            .as_str()
            .expect("raw text fixture");
        assert_eq!(raw_body, fixture["hosted"]["raw_text"]["expected_body"]);
        assert_eq!(token_body, fixture["hosted"]["raw_tokens"]["expected_body"]);

        let add_bos = request(GenerationInput::Completion {
            prompts: vec![CompletionPrompt::Text {
                text: raw_text.to_string(),
                add_bos: true,
            }],
        });
        assert_eq!(
            openai_completion_body(&add_bos)
                .expect_err("unsupported prompt mutation must fail")
                .code,
            fixture["hosted"]["unsupported_add_bos_error"]
        );
    }

    #[cfg(feature = "unstable-w1-vertical-tests")]
    #[tokio::test]
    async fn w1_hosted_fixture_projects_responses_streams_errors_and_request_identity() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/w1/v0/fte-hosted-loopback.json"
        ))
        .expect("parse W1 hosted fixture");
        let projection = &fixture["hosted"]["response_projection"];
        let request_id = RequestId(
            projection["request_id"]
                .as_str()
                .expect("request ID fixture")
                .to_string(),
        );
        let response_id = projection["response_id"]
            .as_str()
            .expect("response ID fixture");
        let route = request(GenerationInput::Chat { items: Vec::new() }).route;

        let chat = parse_provider_response(
            WireProtocol::OpenAiChat,
            &projection["chat"],
            &request_id,
            response_id,
            route.clone(),
            None,
        )
        .expect("fixture chat response");
        assert_eq!(chat.request_id, request_id);
        assert_eq!(chat.id, response_id);
        let [fte_types::OutputItem::Message { content, .. }] = chat.output.as_slice() else {
            panic!("fixture chat response must contain one message")
        };
        assert_eq!(
            content,
            &[fte_types::ContentBlock::Text {
                text: "exact chat output".to_string()
            }]
        );

        let completion = parse_provider_response(
            WireProtocol::OpenAiCompletion,
            &projection["completion"],
            &request_id,
            response_id,
            route.clone(),
            None,
        )
        .expect("fixture completion response");
        assert_eq!(completion.request_id, request_id);
        assert_eq!(completion.id, response_id);
        let [fte_types::OutputItem::Message { content, .. }] = completion.output.as_slice() else {
            panic!("fixture completion response must contain one message")
        };
        assert_eq!(
            content,
            &[fte_types::ContentBlock::Text {
                text: " exact raw output\n".to_string()
            }]
        );

        let (events, mut receiver) = mpsc::channel(16);
        let mut state = ProviderStreamState::new(
            WireProtocol::OpenAiCompletion,
            request_id.clone(),
            response_id.to_string(),
            route,
            None,
            CancellationToken::new(),
        );
        let mut parser = SseParser::default();
        let mut finished = false;
        for bytes in projection["stream_frames"]
            .as_array()
            .expect("stream frame fixtures")
        {
            for frame in parser
                .push(bytes.as_str().expect("stream fixture bytes").as_bytes())
                .expect("parse stream fixture")
            {
                finished |= state
                    .consume_frame(frame, &events)
                    .await
                    .expect("consume stream fixture");
            }
        }
        assert!(finished, "[DONE] must terminate the fixture stream");
        drop(events);
        let mut deltas = String::new();
        while let Some(event) = receiver.recv().await {
            if let GatewayEvent::TextDelta {
                request_id: event_request,
                delta,
                ..
            } = event
            {
                assert_eq!(event_request, request_id);
                deltas.push_str(&delta);
            }
        }
        assert_eq!(deltas, projection["stream_text"]);

        let error = provider_event_error(&request_id, "provider", &projection["error"]);
        assert_eq!(error.request_id, request_id);
        assert_eq!(error.code, projection["expected_error_code"]);
        assert_eq!(error.safe_detail, projection["expected_safe_detail"]);
        assert!(!error.safe_detail.contains("must never escape"));

        let (chat_body, raw_body, token_body) = w1_hosted_request_bodies(&fixture);
        validate_w1_hosted_projection(&fixture, &chat_body, &raw_body, &token_body);
    }

    #[test]
    fn fragmented_sse_is_reassembled_without_json_loss() {
        let mut parser = SseParser::default();
        assert!(
            parser
                .push(b"event: response.output_text.delta\ndata: {\"delta\":\"")
                .expect("first fragment")
                .is_empty()
        );
        let frames = parser.push(b"hi\"}\n\n").expect("second fragment");
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].event.as_deref(),
            Some("response.output_text.delta")
        );
        assert_eq!(frames[0].data, "{\"delta\":\"hi\"}");
    }

    #[test]
    fn provider_specific_sampling_is_never_silently_dropped() {
        let mut responses = request(GenerationInput::Chat { items: Vec::new() });
        responses.request.sampling.top_k = Some(40);
        let error = openai_responses_body(&responses).expect_err("top_k must be rejected");
        assert_eq!(error.code, "sampling_parameter_unsupported");
        assert!(error.safe_detail.contains("top_k"));

        let mut anthropic = request(GenerationInput::Chat { items: Vec::new() });
        anthropic.request.sampling.top_k = Some(40);
        anthropic.request.sampling.stop = vec!["done".to_string()];
        let body = anthropic_body(&anthropic).expect("Anthropic sampling");
        assert_eq!(body["top_k"], 40);
        assert_eq!(body["stop_sequences"], json!(["done"]));
        assert!(body.get("stop").is_none());

        anthropic.request.sampling.seed = Some(7);
        let error = anthropic_body(&anthropic).expect_err("seed must be rejected");
        assert_eq!(error.code, "sampling_parameter_unsupported");
        assert!(error.safe_detail.contains("seed"));
    }

    #[test]
    fn openai_tool_schemas_remain_native_to_each_surface() {
        let mut request = request(GenerationInput::Chat { items: Vec::new() });
        request.request.tools.push(fte_types::ToolDefinition {
            name: "weather".to_string(),
            description: Some("Weather".to_string()),
            input_schema: json!({"type":"object"}),
            strict: true,
            owner: fte_types::ToolOwner::Client,
        });
        let responses = openai_responses_body(&request).expect("Responses body");
        assert_eq!(responses["tools"][0]["name"], "weather");
        assert!(responses["tools"][0].get("function").is_none());

        let chat = openai_chat_body(&request).expect("Chat body");
        assert_eq!(chat["tools"][0]["function"]["name"], "weather");
        assert!(chat["tools"][0].get("name").is_none());
    }

    #[test]
    fn anthropic_count_body_contains_only_prompt_shaping_fields() {
        let mut request = request(GenerationInput::Chat {
            items: vec![fte_types::InputItem::Message {
                id: None,
                role: fte_types::MessageRole::User,
                content: vec![fte_types::ContentBlock::Text {
                    text: "Count me".to_string(),
                }],
            }],
        });
        request.request.sampling.temperature = Some(0.2);
        request.request.stream.enabled = true;
        let body = anthropic_count_body(&request).expect("count body");
        assert_eq!(body["messages"][0]["content"][0]["text"], "Count me");
        assert!(body.get("stream").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn gemini_count_body_contains_only_prompt_shaping_fields() {
        let mut request = request(GenerationInput::Chat {
            items: vec![fte_types::InputItem::Message {
                id: None,
                role: fte_types::MessageRole::User,
                content: vec![fte_types::ContentBlock::Text {
                    text: "Count me".to_string(),
                }],
            }],
        });
        request.request.sampling.temperature = Some(0.2);
        request.request.stream.enabled = true;
        request.request.provider_extensions.insert(
            "gemini.cachedContent".to_string(),
            json!("cachedContents/unsafe"),
        );
        let body = gemini_count_body(&request).expect("count body");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "Count me");
        assert!(body.get("generationConfig").is_none());
        assert!(body.get("toolConfig").is_none());
        assert!(body.get("cachedContent").is_none());
    }

    #[test]
    fn anthropic_beta_is_a_validated_header_and_never_a_body_field() {
        let mut request = request(GenerationInput::Chat {
            items: vec![fte_types::InputItem::Message {
                id: None,
                role: fte_types::MessageRole::User,
                content: vec![fte_types::ContentBlock::Text {
                    text: "hello".to_string(),
                }],
            }],
        });
        request.request.provider_extensions.insert(
            "anthropic.beta".to_string(),
            json!("interleaved-thinking-2025-05-14"),
        );
        let body = anthropic_body(&request).expect("Anthropic body");
        assert!(body.get("beta").is_none());
        assert!(body.get("anthropic_beta").is_none());

        let mut headers = HeaderMap::new();
        apply_anthropic_request_headers(&mut headers, &request.request)
            .expect("Anthropic request headers");
        assert_eq!(
            headers
                .get("anthropic-beta")
                .expect("beta header")
                .to_str()
                .expect("valid header"),
            "interleaved-thinking-2025-05-14"
        );
    }

    #[test]
    fn gemini_extensions_keep_generation_and_top_level_placement() {
        let mut request = request(GenerationInput::Chat {
            items: vec![fte_types::InputItem::Message {
                id: None,
                role: fte_types::MessageRole::User,
                content: vec![fte_types::ContentBlock::Text {
                    text: "hello".to_string(),
                }],
            }],
        });
        request.request.provider_extensions.extend([
            (
                "gemini.thinkingConfig".to_string(),
                json!({"thinkingBudget":512}),
            ),
            (
                "gemini.safetySettings".to_string(),
                json!([{"category":"HARM_CATEGORY_HATE_SPEECH","threshold":"BLOCK_NONE"}]),
            ),
            (
                "gemini.cachedContent".to_string(),
                json!("cachedContents/stable"),
            ),
        ]);

        let body = gemini_body(&request).expect("Gemini body");
        assert_eq!(
            body["generationConfig"]["thinkingConfig"],
            json!({"thinkingBudget":512})
        );
        assert!(body.get("thinkingConfig").is_none());
        assert_eq!(body["cachedContent"], "cachedContents/stable");
        assert!(body["safetySettings"].is_array());
    }

    #[test]
    fn hosted_protocols_reject_foreign_provider_extensions() {
        let mut request = request(GenerationInput::Chat { items: Vec::new() });
        request
            .request
            .provider_extensions
            .insert("gemini.cachedContent".to_string(), json!("foreign"));
        let error = anthropic_body(&request).expect_err("foreign extension must fail");
        assert_eq!(error.code, "hosted_provider_extension_unsupported");
    }

    #[test]
    fn assistant_text_and_multiple_calls_stay_in_one_provider_turn() {
        let items = vec![
            fte_types::InputItem::Message {
                id: None,
                role: fte_types::MessageRole::Assistant,
                content: vec![fte_types::ContentBlock::Text {
                    text: "Checking.".to_string(),
                }],
            },
            fte_types::InputItem::FunctionCall {
                id: None,
                call_id: "call_1".to_string(),
                name: "first".to_string(),
                arguments: json!({}),
            },
            fte_types::InputItem::FunctionCall {
                id: None,
                call_id: "call_2".to_string(),
                name: "second".to_string(),
                arguments: json!({}),
            },
        ];

        let openai = openai_chat_messages(&items, &RequestId::new()).expect("OpenAI messages");
        assert_eq!(openai.len(), 1);
        assert_eq!(openai[0]["tool_calls"].as_array().expect("calls").len(), 2);

        let (_, anthropic) =
            anthropic_messages_from_items(&items, &RequestId::new()).expect("Anthropic messages");
        assert_eq!(anthropic.len(), 1);
        assert_eq!(
            anthropic[0]["content"].as_array().expect("content").len(),
            3
        );

        let request = request(GenerationInput::Chat { items });
        let gemini = gemini_body(&request).expect("Gemini body");
        assert_eq!(gemini["contents"].as_array().expect("contents").len(), 1);
        assert_eq!(
            gemini["contents"][0]["parts"]
                .as_array()
                .expect("parts")
                .len(),
            3
        );
    }
}
