//! Hardened, optional REST/SSE loopback edge.

use async_stream::stream;
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use fte_protocols::{
    AnthropicCountTokensRequest, AnthropicMessagesRequest, AnthropicStreamEncoder, EdgeDefaults,
    OpenAiChatRequest, OpenAiCompletionRequest, OpenAiResponsesRequest,
    OpenAiResponsesStreamEncoder, anthropic_message_json, openai_chat_json, openai_completion_json,
    openai_responses_json,
};
use fte_router::Gateway;
use fte_store::ResponseStore;
use fte_types::{
    CancelTarget, GatewayError, GatewayEvent, GatewayResponse, GatewayUsage, RequestId,
};
use rand::random;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::convert::Infallible;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path as FilePath, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

const LISTENER_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct LoopbackConfig {
    pub port: u16,
    pub token_path: PathBuf,
    pub allowed_origins: BTreeSet<String>,
    pub max_request_bytes: usize,
    pub max_header_bytes: usize,
    pub max_concurrent_requests: usize,
    pub stream_keep_alive: Duration,
    pub stream_idle_timeout: Duration,
    pub stream_total_timeout: Duration,
    pub edge_defaults: EdgeDefaults,
}

impl LoopbackConfig {
    #[must_use]
    pub fn app_private(token_path: PathBuf) -> Self {
        Self {
            port: 0,
            token_path,
            allowed_origins: BTreeSet::new(),
            max_request_bytes: 8 * 1024 * 1024,
            max_header_bytes: 32 * 1024,
            max_concurrent_requests: 16,
            stream_keep_alive: Duration::from_secs(15),
            stream_idle_timeout: Duration::from_secs(120),
            stream_total_timeout: Duration::from_hours(1),
            edge_defaults: EdgeDefaults::default(),
        }
    }
}

#[derive(Clone)]
struct AppState {
    gateway: Arc<Gateway>,
    store: Arc<dyn ResponseStore>,
    token: Arc<RwLock<String>>,
    allowed_origins: Arc<BTreeSet<String>>,
    concurrency: Arc<Semaphore>,
    active_responses: Arc<Mutex<HashMap<String, RequestId>>>,
    edge_defaults: EdgeDefaults,
    keep_alive: Duration,
    max_header_bytes: usize,
    stream_idle_timeout: Duration,
    stream_total_timeout: Duration,
}

pub struct LoopbackServer {
    addresses: Vec<SocketAddr>,
    token: Arc<RwLock<String>>,
    token_path: PathBuf,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for LoopbackServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoopbackServer")
            .field("addresses", &self.addresses)
            .finish_non_exhaustive()
    }
}

impl LoopbackServer {
    pub async fn start(
        gateway: Arc<Gateway>,
        store: Arc<dyn ResponseStore>,
        config: LoopbackConfig,
    ) -> Result<Self, GatewayError> {
        let token_value = load_or_create_token(&config.token_path)?;
        let token = Arc::new(RwLock::new(token_value));
        let state = AppState {
            gateway,
            store,
            token: Arc::clone(&token),
            allowed_origins: Arc::new(config.allowed_origins),
            concurrency: Arc::new(Semaphore::new(config.max_concurrent_requests.max(1))),
            active_responses: Arc::new(Mutex::new(HashMap::new())),
            edge_defaults: config.edge_defaults,
            keep_alive: config.stream_keep_alive,
            max_header_bytes: config.max_header_bytes,
            stream_idle_timeout: config.stream_idle_timeout,
            stream_total_timeout: config.stream_total_timeout,
        };

        let ipv4 = TcpListener::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            config.port,
        ))
        .await
        .map_err(loopback_error)?;
        let port = ipv4.local_addr().map_err(loopback_error)?.port();
        let ipv6 = TcpListener::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port))
            .await
            .ok();
        let mut listeners = vec![ipv4];
        if let Some(listener) = ipv6 {
            listeners.push(listener);
        }
        let addresses = listeners
            .iter()
            .filter_map(|listener| listener.local_addr().ok())
            .collect::<Vec<_>>();
        let (shutdown, _) = watch::channel(false);
        let mut tasks = Vec::new();
        for listener in listeners {
            let app = router(state.clone(), config.max_request_bytes);
            let mut shutdown_rx = shutdown.subscribe();
            tasks.push(tokio::spawn(async move {
                let result = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        while !*shutdown_rx.borrow() {
                            if shutdown_rx.changed().await.is_err() {
                                break;
                            }
                        }
                    })
                    .await;
                if let Err(error) = result {
                    eprintln!("Free Token Energy loopback listener stopped: {error}");
                }
            }));
        }
        Ok(Self {
            addresses,
            token,
            token_path: config.token_path,
            shutdown,
            tasks,
        })
    }

    #[must_use]
    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }

    #[must_use]
    pub fn token_path(&self) -> &FilePath {
        &self.token_path
    }

    pub fn rotate_token(&self) -> Result<String, GatewayError> {
        let token = random_token();
        write_token_atomic(&self.token_path, &token)?;
        *self.token.write().map_err(lock_error)? = token.clone();
        Ok(token)
    }

    pub async fn shutdown(self) {
        self.shutdown_with_grace(LISTENER_SHUTDOWN_GRACE).await;
    }

    async fn shutdown_with_grace(self, grace: Duration) {
        let _ = self.shutdown.send(true);
        let deadline = Instant::now() + grace;
        for mut task in self.tasks {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if tokio::time::timeout(remaining, &mut task).await.is_err() {
                // Axum graceful shutdown intentionally waits for live
                // connections. A client that stops reading an SSE response
                // can otherwise hold process exit forever. After the bounded
                // grace period, drop that listener and every connection it
                // owns, then await the aborted task so no listener work is
                // left detached.
                task.abort();
                let _ = task.await;
            }
        }
    }
}

fn router(state: AppState, max_request_bytes: usize) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/models", get(models))
        .route("/v1/completions", post(completions))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route(
            "/v1/responses/{id}",
            get(get_response).delete(delete_response),
        )
        .route("/v1/responses/{id}/cancel", post(cancel_response))
        .route("/v1/messages", post(anthropic_messages))
        .route("/v1/messages/count_tokens", post(anthropic_count_tokens))
        .layer(DefaultBodyLimit::max(max_request_bytes))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            security_guard,
        ))
        .with_state(state)
}

async fn security_guard(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !host_allowed(request.headers()) {
        return ApiError::simple(
            StatusCode::BAD_REQUEST,
            "invalid_host",
            "Host must be a loopback IP literal",
        )
        .into_response();
    }
    if header_bytes(request.headers()) > state.max_header_bytes {
        return ApiError::simple(
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "headers_too_large",
            "Request headers exceed the local gateway limit",
        )
        .into_response();
    }
    if !origin_allowed(request.headers(), &state.allowed_origins) {
        return ApiError::simple(
            StatusCode::FORBIDDEN,
            "origin_forbidden",
            "Origin is not allowlisted",
        )
        .into_response();
    }
    let expected = match state.token.read() {
        Ok(token) => token.clone(),
        Err(_) => {
            return ApiError::simple(
                StatusCode::INTERNAL_SERVER_ERROR,
                "token_state_unavailable",
                "Loopback authentication state is unavailable",
            )
            .into_response();
        }
    };
    if !authenticated(request.headers(), &expected) {
        return ApiError::simple(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "A valid local gateway token is required",
        )
        .into_response();
    }
    let permit = match Arc::clone(&state.concurrency).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return ApiError::simple(
                StatusCode::TOO_MANY_REQUESTS,
                "concurrency_limit",
                "The local gateway concurrency limit is reached",
            )
            .into_response();
        }
    };
    hold_concurrency_permit(next.run(request).await, permit)
}

fn hold_concurrency_permit(response: Response, permit: OwnedSemaphorePermit) -> Response {
    let (parts, body) = response.into_parts();
    let guarded = stream! {
        let _permit = permit;
        let mut data = body.into_data_stream();
        while let Some(chunk) = futures::StreamExt::next(&mut data).await {
            yield chunk;
        }
    };
    Response::from_parts(parts, Body::from_stream(guarded))
}

fn header_bytes(headers: &HeaderMap) -> usize {
    headers
        .iter()
        .map(|(name, value)| name.as_str().len().saturating_add(value.as_bytes().len()))
        .sum()
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"status":"ok","gateway":state.gateway.status()}))
}

async fn models(State(state): State<AppState>) -> Json<Value> {
    let data = state
        .gateway
        .models()
        .into_iter()
        .map(|model| {
            json!({
                "id": model.id,
                "object":"model",
                "owned_by":model.backend_id,
                "x_free_token_energy":{
                    "location":model.location,
                    "capabilities":model.capabilities,
                    "context_tokens":model.context_tokens,
                }
            })
        })
        .collect::<Vec<_>>();
    Json(json!({"object":"list","data":data}))
}

async fn completions(
    State(state): State<AppState>,
    Json(request): Json<OpenAiCompletionRequest>,
) -> Result<Response, ApiError> {
    let stream_requested = request.stream;
    let gateway_request = request.into_gateway(state.edge_defaults.clone())?;
    let ticket = state.gateway.execute(gateway_request).await?;
    if stream_requested {
        Ok(openai_stream_response(
            ticket,
            StreamFlavor::Completion,
            state,
        ))
    } else {
        let response = await_response(ticket).await?;
        Ok(Json(openai_completion_json(&response)).into_response())
    }
}

async fn chat_completions(
    State(state): State<AppState>,
    Json(request): Json<OpenAiChatRequest>,
) -> Result<Response, ApiError> {
    let stream_requested = request.stream;
    let gateway_request = request.into_gateway(state.edge_defaults.clone())?;
    let ticket = state.gateway.execute(gateway_request).await?;
    if stream_requested {
        Ok(openai_stream_response(ticket, StreamFlavor::Chat, state))
    } else {
        let response = await_response(ticket).await?;
        Ok(Json(openai_chat_json(&response)).into_response())
    }
}

async fn responses(
    State(state): State<AppState>,
    Json(request): Json<OpenAiResponsesRequest>,
) -> Result<Response, ApiError> {
    let stream_requested = request.stream;
    let gateway_request = request.into_gateway(state.edge_defaults.clone())?;
    restore_previous_response_affinity(&state, &gateway_request)?;
    let should_store = gateway_request.storage.store_response;
    let ticket = state.gateway.execute(gateway_request).await?;
    if stream_requested {
        Ok(openai_responses_stream(ticket, state, should_store))
    } else {
        let response = await_response(ticket).await?;
        persist_response(&state, &response, should_store)?;
        Ok(Json(openai_responses_json(&response)).into_response())
    }
}

async fn get_response(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let response = state.store.get(&id)?.ok_or_else(|| {
        ApiError::simple(
            StatusCode::NOT_FOUND,
            "response_not_found",
            "Stored response not found",
        )
    })?;
    Ok(Json(openai_responses_json(&response)))
}

async fn delete_response(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !state.store.delete(&id)? {
        return Err(ApiError::simple(
            StatusCode::NOT_FOUND,
            "response_not_found",
            "Stored response not found",
        ));
    }
    Ok(Json(
        json!({"id":id,"object":"response.deleted","deleted":true}),
    ))
}

async fn cancel_response(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let request_id = state
        .active_responses
        .lock()
        .map_err(|_| {
            ApiError::simple(
                StatusCode::INTERNAL_SERVER_ERROR,
                "active_state_unavailable",
                "Active request state is unavailable",
            )
        })?
        .get(&id)
        .cloned()
        .or_else(|| {
            state
                .store
                .get(&id)
                .ok()
                .flatten()
                .map(|response| response.request_id)
        })
        .ok_or_else(|| {
            ApiError::simple(
                StatusCode::NOT_FOUND,
                "response_not_found",
                "Response not found",
            )
        })?;
    let cancelled = state.gateway.cancel(&request_id, CancelTarget::Request);
    Ok(Json(
        json!({"id":id,"object":"response","status":if cancelled > 0 {"cancelling"} else {"completed"}}),
    ))
}

async fn anthropic_messages(
    State(state): State<AppState>,
    Json(request): Json<AnthropicMessagesRequest>,
) -> Result<Response, ApiError> {
    let stream_requested = request.stream;
    let gateway_request = request.into_gateway(state.edge_defaults.clone())?;
    if stream_requested {
        let usage = state.gateway.count_tokens(gateway_request.clone()).await?;
        let input_tokens = require_exact_input_tokens(&usage)?;
        let ticket = state.gateway.execute(gateway_request).await?;
        Ok(anthropic_stream(ticket, state, input_tokens))
    } else {
        let ticket = state.gateway.execute(gateway_request).await?;
        let response = await_response(ticket).await?;
        require_exact_input_tokens(&response.usage)?;
        require_exact_output_tokens(&response.usage)?;
        Ok(Json(anthropic_message_json(&response)).into_response())
    }
}

async fn anthropic_count_tokens(
    State(state): State<AppState>,
    Json(request): Json<AnthropicCountTokensRequest>,
) -> Result<Json<Value>, ApiError> {
    let request = request.into_gateway(state.edge_defaults.clone())?;
    let usage = state.gateway.count_tokens(request).await?;
    let input_tokens = require_exact_input_tokens(&usage)?;
    Ok(Json(json!({
        "input_tokens":input_tokens,
        "x_usage_provenance":"exact"
    })))
}

fn require_exact_input_tokens(usage: &GatewayUsage) -> Result<u64, ApiError> {
    if usage.provenance != fte_types::UsageProvenance::Exact {
        return Err(ApiError::simple(
            StatusCode::UNPROCESSABLE_ENTITY,
            "exact_input_usage_unavailable",
            "The selected route cannot provide exact Anthropic input token accounting",
        ));
    }
    usage.input_tokens.ok_or_else(|| {
        ApiError::simple(
            StatusCode::UNPROCESSABLE_ENTITY,
            "exact_input_usage_unavailable",
            "The selected route returned no exact Anthropic input token count",
        )
    })
}

fn require_exact_output_tokens(usage: &GatewayUsage) -> Result<u64, ApiError> {
    if usage.provenance != fte_types::UsageProvenance::Exact {
        return Err(ApiError::simple(
            StatusCode::UNPROCESSABLE_ENTITY,
            "exact_output_usage_unavailable",
            "The selected route cannot provide exact Anthropic output token accounting",
        ));
    }
    usage.output_tokens.ok_or_else(|| {
        ApiError::simple(
            StatusCode::UNPROCESSABLE_ENTITY,
            "exact_output_usage_unavailable",
            "The selected route returned no exact Anthropic output token count",
        )
    })
}

async fn await_response(
    mut ticket: fte_types::GatewayTicket,
) -> Result<GatewayResponse, GatewayError> {
    while let Some(event) = ticket.events.recv().await {
        if event.is_terminal() {
            break;
        }
    }
    ticket.final_response().await
}

#[derive(Clone, Copy)]
enum StreamFlavor {
    Chat,
    Completion,
}

enum StreamPoll {
    Event(Box<GatewayEvent>),
    Closed,
    IdleTimeout,
    TotalTimeout,
}

async fn next_stream_event(
    events: &mut mpsc::Receiver<GatewayEvent>,
    started_at: Instant,
    idle_timeout: Duration,
    total_timeout: Duration,
) -> StreamPoll {
    let elapsed = started_at.elapsed();
    if elapsed >= total_timeout {
        return StreamPoll::TotalTimeout;
    }
    let remaining = total_timeout.saturating_sub(elapsed);
    let wait = idle_timeout.min(remaining);
    match tokio::time::timeout(wait, events.recv()).await {
        Ok(Some(event)) => StreamPoll::Event(Box::new(event)),
        Ok(None) => StreamPoll::Closed,
        Err(_) if remaining <= idle_timeout => StreamPoll::TotalTimeout,
        Err(_) => StreamPoll::IdleTimeout,
    }
}

fn stream_timeout_payload(poll: &StreamPoll) -> Value {
    let (code, message) = match poll {
        StreamPoll::IdleTimeout => (
            "stream_idle_timeout",
            "The local gateway stream was idle for too long",
        ),
        StreamPoll::TotalTimeout => (
            "stream_total_timeout",
            "The local gateway stream exceeded its total lifetime",
        ),
        StreamPoll::Event(_) | StreamPoll::Closed => (
            "stream_closed",
            "The local gateway stream closed unexpectedly",
        ),
    };
    json!({"type":"gateway_error","code":code,"message":message})
}

fn openai_stream_response(
    mut ticket: fte_types::GatewayTicket,
    flavor: StreamFlavor,
    state: AppState,
) -> Response {
    let keep_alive = state.keep_alive;
    let idle_timeout = state.stream_idle_timeout;
    let total_timeout = state.stream_total_timeout;
    let event_stream = stream! {
        let started_at = Instant::now();
        let mut active_response_id = None;
        loop {
            let poll = next_stream_event(&mut ticket.events, started_at, idle_timeout, total_timeout).await;
            let event = match poll {
                StreamPoll::Event(event) => *event,
                StreamPoll::Closed => break,
                StreamPoll::IdleTimeout | StreamPoll::TotalTimeout => {
                    yield Ok::<Event, Infallible>(Event::default().data(json!({"error":stream_timeout_payload(&poll)}).to_string()));
                    yield Ok(Event::default().data("[DONE]"));
                    break;
                }
            };
            match event {
                GatewayEvent::ResponseCreated { response_id, request_id, .. } => {
                    if let Ok(mut active) = state.active_responses.lock() {
                        active.insert(response_id.clone(), request_id);
                    }
                    active_response_id = Some(response_id.clone());
                    if matches!(flavor, StreamFlavor::Chat) {
                        yield Ok::<Event, Infallible>(Event::default().data(json!({
                            "id":format!("chatcmpl_{response_id}"),
                            "object":"chat.completion.chunk",
                            "choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":Value::Null}]
                        }).to_string()));
                    }
                }
                GatewayEvent::TextDelta { delta, output_index, .. } => {
                    let value = match flavor {
                        StreamFlavor::Chat => json!({
                            "object":"chat.completion.chunk",
                            "choices":[{"index":output_index,"delta":{"content":delta},"finish_reason":Value::Null}]
                        }),
                        StreamFlavor::Completion => json!({
                            "object":"text_completion",
                            "choices":[{"index":output_index,"text":delta,"finish_reason":Value::Null,"logprobs":Value::Null}]
                        }),
                    };
                    yield Ok(Event::default().data(value.to_string()));
                }
                GatewayEvent::Completed { response, .. } => {
                    let value = match flavor {
                        StreamFlavor::Chat => json!({
                            "id":format!("chatcmpl_{}",response.id),
                            "object":"chat.completion.chunk",
                            "model":response.model,
                            "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
                            "usage":response.usage,
                        }),
                        StreamFlavor::Completion => json!({
                            "id":format!("cmpl_{}",response.id),
                            "object":"text_completion",
                            "model":response.model,
                            "choices":[{"index":0,"text":"","finish_reason":"stop","logprobs":Value::Null}],
                            "usage":response.usage,
                        }),
                    };
                    yield Ok(Event::default().data(value.to_string()));
                    yield Ok(Event::default().data("[DONE]"));
                    break;
                }
                GatewayEvent::Cancelled { .. } | GatewayEvent::Failed { .. } => {
                    yield Ok(Event::default().data("[DONE]"));
                    break;
                }
                _ => {}
            }
        }
        if let Some(response_id) = active_response_id
            && let Ok(mut active) = state.active_responses.lock()
        {
            active.remove(&response_id);
        }
    };
    Sse::new(event_stream)
        .keep_alive(KeepAlive::new().interval(keep_alive))
        .into_response()
}

fn openai_responses_stream(
    mut ticket: fte_types::GatewayTicket,
    state: AppState,
    should_store: bool,
) -> Response {
    let keep_alive = state.keep_alive;
    let idle_timeout = state.stream_idle_timeout;
    let total_timeout = state.stream_total_timeout;
    let event_stream = stream! {
        let mut encoder = OpenAiResponsesStreamEncoder::default();
        let started_at = Instant::now();
        let mut active_response_id = None;
        loop {
            let poll = next_stream_event(&mut ticket.events, started_at, idle_timeout, total_timeout).await;
            let event = match poll {
                StreamPoll::Event(event) => *event,
                StreamPoll::Closed => break,
                StreamPoll::IdleTimeout | StreamPoll::TotalTimeout => {
                    let payload = stream_timeout_payload(&poll);
                    yield Ok::<Event, Infallible>(Event::default().event("error").data(payload.to_string()));
                    break;
                }
            };
            if let GatewayEvent::ResponseCreated { response_id, request_id, .. } = &event
                && let Ok(mut active) = state.active_responses.lock()
            {
                active.insert(response_id.clone(), request_id.clone());
                active_response_id = Some(response_id.clone());
            }
            if let GatewayEvent::Completed { response, .. } = &event {
                let _ = persist_response(&state, response, should_store);
                if let Ok(mut active) = state.active_responses.lock() {
                    active.remove(&response.id);
                }
            }
            if let Some(encoded) = encoder.encode(&event) {
                yield Ok::<Event, Infallible>(Event::default().event(encoded.event).data(encoded.data.to_string()));
            }
            if event.is_terminal() {
                break;
            }
        }
        if let Some(response_id) = active_response_id
            && let Ok(mut active) = state.active_responses.lock()
        {
            active.remove(&response_id);
        }
    };
    Sse::new(event_stream)
        .keep_alive(KeepAlive::new().interval(keep_alive))
        .into_response()
}

fn anthropic_stream(
    mut ticket: fte_types::GatewayTicket,
    state: AppState,
    input_tokens: u64,
) -> Response {
    let keep_alive = state.keep_alive;
    let idle_timeout = state.stream_idle_timeout;
    let total_timeout = state.stream_total_timeout;
    let event_stream = stream! {
        let mut encoder = AnthropicStreamEncoder::new(input_tokens);
        let started_at = Instant::now();
        loop {
            let poll = next_stream_event(&mut ticket.events, started_at, idle_timeout, total_timeout).await;
            let event = match poll {
                StreamPoll::Event(event) => *event,
                StreamPoll::Closed => break,
                StreamPoll::IdleTimeout | StreamPoll::TotalTimeout => {
                    let payload = stream_timeout_payload(&poll);
                    yield Ok::<Event, Infallible>(Event::default().event("error").data(json!({"type":"error","error":payload}).to_string()));
                    break;
                }
            };
            for encoded in encoder.encode(&event) {
                yield Ok::<Event, Infallible>(Event::default().event(encoded.event).data(encoded.data.to_string()));
            }
            if event.is_terminal() {
                break;
            }
        }
    };
    Sse::new(event_stream)
        .keep_alive(KeepAlive::new().interval(keep_alive))
        .into_response()
}

fn persist_response(
    state: &AppState,
    response: &GatewayResponse,
    should_store: bool,
) -> Result<(), GatewayError> {
    state
        .gateway
        .record_response_affinity(&response.id, &response.route)?;
    if should_store {
        state.store.put(response)?;
    }
    Ok(())
}

fn restore_previous_response_affinity(
    state: &AppState,
    request: &fte_types::GatewayRequest,
) -> Result<(), GatewayError> {
    let Some(previous_response_id) = request.storage.previous_response_id.as_deref() else {
        return Ok(());
    };
    if let Some(previous) = state.store.get(previous_response_id)? {
        state
            .gateway
            .record_response_affinity(previous_response_id, &previous.route)?;
    }
    Ok(())
}

fn authenticated(headers: &HeaderMap, expected: &str) -> bool {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let anthropic = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok());
    bearer.is_some_and(|candidate| constant_time_token_eq(candidate, expected))
        || anthropic.is_some_and(|candidate| constant_time_token_eq(candidate, expected))
}

fn constant_time_token_eq(candidate: &str, expected: &str) -> bool {
    if candidate.len() != expected.len() {
        return false;
    }
    candidate
        .as_bytes()
        .iter()
        .zip(expected.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn host_allowed(headers: &HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value == "127.0.0.1"
                || value.starts_with("127.0.0.1:")
                || value == "[::1]"
                || value.starts_with("[::1]:")
        })
}

fn origin_allowed(headers: &HeaderMap, allowlist: &BTreeSet<String>) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|origin| allowlist.contains(origin))
}

fn load_or_create_token(path: &FilePath) -> Result<String, GatewayError> {
    if let Ok(value) = fs::read_to_string(path) {
        verify_private_token_path(path)?;
        let value = value.trim().to_string();
        if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(value);
        }
        return Err(loopback_error(
            "the existing loopback token file is invalid",
        ));
    }
    if let Some(parent) = path.parent() {
        ensure_private_directory(parent)?;
    }
    let token = random_token();
    write_token_atomic(path, &token)?;
    Ok(token)
}

fn write_token_atomic(path: &FilePath, token: &str) -> Result<(), GatewayError> {
    let parent = path
        .parent()
        .ok_or_else(|| loopback_error("the loopback token path has no private parent directory"))?;
    ensure_private_directory(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| loopback_error("the loopback token file name is invalid"))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", random_token()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(loopback_error)?;
        file.write_all(token.as_bytes()).map_err(loopback_error)?;
        file.sync_all().map_err(loopback_error)?;
        fs::rename(&temporary, path).map_err(loopback_error)?;
        sync_directory(parent)?;
        verify_private_token_path(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn ensure_private_directory(path: &FilePath) -> Result<(), GatewayError> {
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::fs::PermissionsExt;
    if !path.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path).map_err(loopback_error)?;
    }
    let mode = fs::metadata(path)
        .map_err(loopback_error)?
        .permissions()
        .mode();
    if mode & 0o022 != 0 {
        return Err(loopback_error(
            "the loopback token directory is writable by another user",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_directory(path: &FilePath) -> Result<(), GatewayError> {
    fs::create_dir_all(path).map_err(loopback_error)
}

#[cfg(unix)]
fn verify_private_token_path(path: &FilePath) -> Result<(), GatewayError> {
    use std::os::unix::fs::PermissionsExt;
    if fs::metadata(path)
        .map_err(loopback_error)?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err(loopback_error(
            "the loopback token file permissions are not private",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_private_token_path(_path: &FilePath) -> Result<(), GatewayError> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &FilePath) -> Result<(), GatewayError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(loopback_error)
}

#[cfg(not(unix))]
fn sync_directory(_path: &FilePath) -> Result<(), GatewayError> {
    Ok(())
}

fn random_token() -> String {
    random::<[u8; 32]>()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn loopback_error(error: impl std::fmt::Display) -> GatewayError {
    GatewayError {
        code: "loopback_error".to_string(),
        class: fte_types::ErrorClass::Internal,
        retryable: false,
        http_status: 500,
        request_id: RequestId::new(),
        provider: None,
        safe_detail: format!("loopback service failed: {error}"),
    }
}

fn lock_error<T>(_error: std::sync::PoisonError<T>) -> GatewayError {
    loopback_error("loopback authentication state is unavailable")
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: String,
    message: String,
}

impl ApiError {
    fn simple(status: StatusCode, code: &str, message: &str) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: message.to_string(),
        }
    }
}

impl From<GatewayError> for ApiError {
    fn from(error: GatewayError) -> Self {
        Self {
            status: StatusCode::from_u16(error.http_status)
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code: error.code,
            message: error.safe_detail,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error":{"type":"gateway_error","code":self.code,"message":self.message}})),
        )
            .into_response()
    }
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
    use async_trait::async_trait;
    use axum::http::HeaderValue;
    use fte_store::SqliteStore;
    use fte_types::{
        BackendDescriptor, BackendLocation, BackendReadiness, BackendRequest, CancelTarget,
        ContentBlock, GatewayBackend, GatewayResponse, GatewayTicket, GatewayUsage, MessageRole,
        ModelCapabilities, ModelDescriptor, OutputItem, PromptForm, RouteObservations,
        TerminalStatus, TicketCancellation, UsageProvenance,
    };
    #[cfg(feature = "unstable-w1-vertical-tests")]
    use platform_vertical_fixtures_v0::{
        EquivalenceProjectionV0, ObservationEnvelopeV0, VerticalFixtureManifestV0, sha256_identity,
        validate_baseline, validate_manifest,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::{mpsc, oneshot};

    #[cfg(feature = "unstable-w1-vertical-tests")]
    const W1_CORPUS_BYTES: &[u8] =
        include_bytes!("../../../tests/fixtures/w1/v0/fte-hosted-loopback.json");
    #[cfg(feature = "unstable-w1-vertical-tests")]
    const W1_MANIFEST_BYTES: &[u8] =
        include_bytes!("../../../tests/fixtures/w1/v0/fte-hosted-loopback.manifest.json");
    #[cfg(feature = "unstable-w1-vertical-tests")]
    const W1_LOOPBACK_PROJECTION_BYTES: &[u8] =
        include_bytes!("../../../tests/fixtures/w1/v0/fte-loopback-projection.json");

    fn w1_hosted_loopback_fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/w1/v0/fte-hosted-loopback.json"
        ))
        .expect("parse W1 hosted/loopback fixture")
    }

    #[cfg(feature = "unstable-w1-vertical-tests")]
    fn digest_json(bytes: &[u8]) -> Value {
        serde_json::to_value(sha256_identity("fact", bytes).digest).expect("digest JSON")
    }

    #[cfg(feature = "unstable-w1-vertical-tests")]
    struct LoopbackObservation<'a> {
        unauthorized_status: u16,
        rebound_status: u16,
        concurrent_status: u16,
        stream_body: &'a str,
        chat: &'a Value,
        completion: &'a Value,
        stored_response_id: &'a str,
        stored_response_reopened: bool,
        continuation: &'a Value,
    }

    #[cfg(feature = "unstable-w1-vertical-tests")]
    fn validate_w1_loopback_projection(fixture: &Value, actual: LoopbackObservation<'_>) {
        let manifest: VerticalFixtureManifestV0 =
            serde_json::from_slice(W1_MANIFEST_BYTES).expect("W1 vertical manifest");
        validate_manifest(&manifest).expect("valid W1 vertical manifest");
        let case = manifest
            .cases
            .iter()
            .find(|case| case.case_id == "loopback.roundtrip")
            .expect("loopback case");
        let input = &case.inputs[0].identity;
        assert_eq!(
            sha256_identity(input.id.clone(), W1_CORPUS_BYTES),
            *input,
            "manifest must authenticate the complete loopback corpus"
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

        let requests = serde_json::to_vec(&fixture["loopback"]["requests"])
            .expect("loopback request corpus bytes");
        let stored_identity = sha256_identity(
            "response.store.after_restart",
            actual.stored_response_id.as_bytes(),
        );
        let stored_identity_json =
            serde_json::to_value(&stored_identity).expect("stored identity JSON");
        let expected_events = fixture["loopback"]["expected"]["stream_events"]
            .as_array()
            .expect("stream events");
        let stream_completed = expected_events.iter().all(|event| {
            actual.stream_body.contains(&format!(
                "event: {}",
                event.as_str().expect("stream event name")
            ))
        });
        let chat_roundtrip = actual.chat["object"] == "chat.completion"
            && actual.chat["choices"][0]["message"]["content"] == "hello";
        let raw_roundtrip = actual.completion["object"] == "text_completion"
            && actual.completion["choices"][0]["text"] == "hello";
        let continuation_after_restart = actual.continuation["previous_response_id"]
            == actual.stored_response_id
            && actual.continuation["id"]
                == fixture["loopback"]["expected"]["continuation_response_id"];
        let actual_projection: EquivalenceProjectionV0 = serde_json::from_value(json!({
            "ordered_events": [{
                "sequence": 0,
                "operation_id": "fte.loopback.roundtrip",
                "attempt_id": "loopback.attempt.1",
                "correlation_id": "loopback.restart.w1",
                "kind": "completed",
                "payload": stored_identity_json,
            }],
            "durable_state": [{
                "state_id": "response.store",
                "schema_id": "fte.store.responses.v1",
                "before": null,
                "after": stored_identity_json,
                "disposition": "created",
            }],
            "lifecycle": [{
                "operation_id": "fte.loopback.roundtrip",
                "attempt_id": "loopback.attempt.1",
                "correlation_id": "loopback.restart.w1",
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
                "request_corpus_digest": {"kind": "digest", "value": digest_json(&requests)},
                "unauthorized_status": {"kind": "integer", "value": actual.unauthorized_status},
                "rebound_status": {"kind": "integer", "value": actual.rebound_status},
                "concurrent_status": {"kind": "integer", "value": actual.concurrent_status},
                "chat_roundtrip": {"kind": "boolean", "value": chat_roundtrip},
                "raw_completion_roundtrip": {"kind": "boolean", "value": raw_roundtrip},
                "responses_stream_completed": {"kind": "boolean", "value": stream_completed},
                "stored_response_reopened": {"kind": "boolean", "value": actual.stored_response_reopened},
                "continuation_after_restart": {"kind": "boolean", "value": continuation_after_restart},
                "loopback_only": {"kind": "boolean", "value": true},
            },
            "fail_closed_facts": [
                "missing bearer token was rejected",
                "dns rebinding host was rejected",
                "concurrency limit rejected overlapping work",
                "no hosted provider or credential path was entered",
            ],
        }))
        .expect("actual loopback projection");
        let observation: ObservationEnvelopeV0 = serde_json::from_value(json!({
            "schema": "delysis.vertical_observation.v0",
            "vertical_id": "fte_hosted_fixture_loopback",
            "case_id": case.case_id,
            "implementation_revision": case.source.commit,
            "observed_prerequisites": [],
            "evidence": {
                "schema": "delysis.evidence_claim.v0",
                "tier": "reproducible",
                "threat_model": "authenticated loopback socket using production protocol, storage, restart, and security paths with a deterministic backend",
                "exact_source": case.source.production_tree.digest,
                "exact_runtime_or_artifact": input.digest,
                "execution_kind": "fixture",
                "omitted_claims": manifest.omitted_claims,
                "negative_evidence": [],
            },
            "projection": actual_projection,
        }))
        .expect("loopback observation");
        validate_baseline(
            &manifest,
            &case.case_id,
            W1_LOOPBACK_PROJECTION_BYTES,
            &[],
            &observation,
        )
        .expect("loopback production behavior matches authenticated W1 projection");
    }

    struct NoopCancellation;

    impl TicketCancellation for NoopCancellation {
        fn cancel(&self, _target: CancelTarget) -> usize {
            0
        }
    }

    struct StreamingTestBackend;

    struct FloodingTestBackend;

    #[async_trait]
    impl GatewayBackend for StreamingTestBackend {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: "test-local".to_string(),
                display_name: "Test local".to_string(),
                location: BackendLocation::LocalEmbedded,
                models: vec![ModelDescriptor {
                    id: "test-model".to_string(),
                    aliases: Vec::new(),
                    display_name: "Test model".to_string(),
                    backend_id: "test-local".to_string(),
                    location: BackendLocation::LocalEmbedded,
                    capabilities: ModelCapabilities {
                        prompt_forms: vec![PromptForm::Chat, PromptForm::Completion],
                        modalities: vec![],
                        tools: false,
                        structured_output: false,
                        reasoning: false,
                        streaming: true,
                        provider_cache: false,
                    },
                    context_tokens: Some(4096),
                    max_output_tokens: Some(512),
                    observed: RouteObservations::default(),
                }],
            }
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            let request_id = request.request.request_id.clone();
            let route = request.route;
            let previous_response_id = request.request.storage.previous_response_id;
            let response_id = if previous_response_id.is_some() {
                "resp_socket_continuation"
            } else {
                "resp_socket_test"
            }
            .to_string();
            let item = OutputItem::Message {
                id: "msg_socket_test".to_string(),
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: "hello".to_string(),
                }],
            };
            let response = GatewayResponse {
                id: response_id.clone(),
                request_id: request_id.clone(),
                model: route.model_id.clone(),
                route: route.clone(),
                output: vec![item.clone()],
                usage: GatewayUsage {
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    provenance: UsageProvenance::Exact,
                    selected_route: Some(route.clone()),
                    real_local_inference: true,
                    ..GatewayUsage::default()
                },
                status: TerminalStatus::Completed,
                previous_response_id,
            };
            let (event_tx, event_rx) = mpsc::channel(16);
            let (final_tx, final_rx) = oneshot::channel();
            let terminal = Arc::new(AtomicBool::new(false));
            let terminal_for_task = Arc::clone(&terminal);
            let ticket_request_id = request_id.clone();
            tokio::spawn(async move {
                let _ = event_tx
                    .send(GatewayEvent::ResponseCreated {
                        request_id: request_id.clone(),
                        response_id,
                        route,
                    })
                    .await;
                tokio::time::sleep(Duration::from_millis(100)).await;
                let _ = event_tx
                    .send(GatewayEvent::OutputItemAdded {
                        request_id: request_id.clone(),
                        output_index: 0,
                        item: item.clone(),
                    })
                    .await;
                let _ = event_tx
                    .send(GatewayEvent::TextDelta {
                        request_id: request_id.clone(),
                        output_index: 0,
                        content_index: 0,
                        delta: "hello".to_string(),
                    })
                    .await;
                let _ = event_tx
                    .send(GatewayEvent::OutputItemCompleted {
                        request_id: request_id.clone(),
                        output_index: 0,
                        item,
                    })
                    .await;
                terminal_for_task.store(true, Ordering::Release);
                let _ = event_tx
                    .send(GatewayEvent::Completed {
                        request_id,
                        response: Box::new(response.clone()),
                    })
                    .await;
                let _ = final_tx.send(Ok(response));
            });
            Ok(GatewayTicket::new(
                ticket_request_id,
                event_rx,
                final_rx,
                Arc::new(NoopCancellation),
                terminal,
            ))
        }

        async fn count_tokens(
            &self,
            request: BackendRequest,
        ) -> Result<GatewayUsage, GatewayError> {
            Ok(GatewayUsage {
                input_tokens: Some(7),
                provenance: UsageProvenance::Exact,
                selected_route: Some(request.route),
                ..GatewayUsage::default()
            })
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            0
        }
    }

    #[async_trait]
    impl GatewayBackend for FloodingTestBackend {
        fn descriptor(&self) -> BackendDescriptor {
            StreamingTestBackend.descriptor()
        }

        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }

        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            let request_id = request.request.request_id.clone();
            let route = request.route;
            let response_id = "resp_stalled_client".to_string();
            let item = OutputItem::Message {
                id: "msg_stalled_client".to_string(),
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: "x".repeat(8 * 1024 * 1024),
                }],
            };
            let response = GatewayResponse {
                id: response_id.clone(),
                request_id: request_id.clone(),
                model: route.model_id.clone(),
                route: route.clone(),
                output: vec![item.clone()],
                usage: GatewayUsage {
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    provenance: UsageProvenance::Exact,
                    selected_route: Some(route.clone()),
                    ..GatewayUsage::default()
                },
                status: TerminalStatus::Completed,
                previous_response_id: None,
            };
            let (event_tx, event_rx) = mpsc::channel(8);
            let (final_tx, final_rx) = oneshot::channel();
            let terminal = Arc::new(AtomicBool::new(false));
            let terminal_for_task = Arc::clone(&terminal);
            let ticket_request_id = request_id.clone();
            tokio::spawn(async move {
                for event in [
                    GatewayEvent::ResponseCreated {
                        request_id: request_id.clone(),
                        response_id,
                        route,
                    },
                    GatewayEvent::OutputItemAdded {
                        request_id: request_id.clone(),
                        output_index: 0,
                        item: item.clone(),
                    },
                    GatewayEvent::TextDelta {
                        request_id: request_id.clone(),
                        output_index: 0,
                        content_index: 0,
                        delta: "x".repeat(8 * 1024 * 1024),
                    },
                    GatewayEvent::OutputItemCompleted {
                        request_id: request_id.clone(),
                        output_index: 0,
                        item,
                    },
                    GatewayEvent::Completed {
                        request_id,
                        response: Box::new(response.clone()),
                    },
                ] {
                    if event_tx.send(event).await.is_err() {
                        return;
                    }
                }
                terminal_for_task.store(true, Ordering::Release);
                let _ = final_tx.send(Ok(response));
            });
            Ok(GatewayTicket::new(
                ticket_request_id,
                event_rx,
                final_rx,
                Arc::new(NoopCancellation),
                terminal,
            ))
        }

        async fn count_tokens(
            &self,
            request: BackendRequest,
        ) -> Result<GatewayUsage, GatewayError> {
            Ok(GatewayUsage {
                input_tokens: Some(1),
                provenance: UsageProvenance::Exact,
                selected_route: Some(request.route),
                ..GatewayUsage::default()
            })
        }

        fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
            0
        }
    }

    #[test]
    fn authentication_accepts_only_the_local_token_in_sdk_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        assert!(authenticated(&headers, "secret"));
        assert!(!authenticated(&headers, "other"));
        headers.remove(header::AUTHORIZATION);
        headers.insert("x-api-key", HeaderValue::from_static("secret"));
        assert!(authenticated(&headers, "secret"));
    }

    #[test]
    fn dns_rebinding_hosts_and_unlisted_origins_are_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("evil.example:1337"));
        assert!(!host_allowed(&headers));
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:1337"));
        assert!(host_allowed(&headers));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        assert!(!origin_allowed(&headers, &BTreeSet::new()));
    }

    #[test]
    fn generated_tokens_are_256_bit_hex_values() {
        let token = random_token();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn token_creation_and_rotation_are_private_atomic_replacements() {
        let directory =
            std::env::temp_dir().join(format!("fte-loopback-token-{}", random::<u64>()));
        let path = directory.join("token");
        let first = load_or_create_token(&path).expect("create token");
        let second = random_token();
        write_token_atomic(&path, &second).expect("rotate token");
        assert_ne!(first, second);
        assert_eq!(
            fs::read_to_string(&path).expect("read rotated token"),
            second
        );
        assert_eq!(
            fs::read_dir(&directory)
                .expect("read token directory")
                .count(),
            1,
            "atomic replacement must not leave temporary token files"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path)
                    .expect("token metadata")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
            assert_eq!(
                fs::metadata(&directory)
                    .expect("directory metadata")
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
        fs::remove_dir_all(directory).expect("remove token directory");
    }

    #[cfg(unix)]
    #[test]
    fn token_creation_rejects_a_shared_parent_without_changing_its_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory =
            std::env::temp_dir().join(format!("fte-loopback-shared-{}", random::<u64>()));
        fs::create_dir(&directory).expect("create shared directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777))
            .expect("make fixture directory shared");
        let before = fs::metadata(&directory)
            .expect("shared directory metadata")
            .permissions()
            .mode()
            & 0o777;

        let error = write_token_atomic(&directory.join("token"), &random_token())
            .expect_err("shared token parent must be rejected");
        assert!(error.safe_detail.contains("writable by another user"));
        let after = fs::metadata(&directory)
            .expect("shared directory metadata after rejection")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(after, before, "token creation must not chmod its parent");
        assert!(!directory.join("token").exists());
        fs::remove_dir_all(directory).expect("remove shared directory");
    }

    #[tokio::test]
    async fn real_loopback_socket_enforces_security_stream_state_and_storage() {
        let fixture = w1_hosted_loopback_fixture();
        assert_eq!(fixture["execution"]["network"], "loopback_only");
        assert_eq!(fixture["execution"]["hosted_request_sent"], false);
        assert_eq!(fixture["execution"]["credential_required"], false);
        let gateway = Arc::new(Gateway::new(fte_router::GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(StreamingTestBackend))
            .expect("register backend");
        let token_directory =
            std::env::temp_dir().join(format!("fte-loopback-test-{}", random::<u64>()));
        let token_path = token_directory.join("token");
        let database_path =
            std::env::temp_dir().join(format!("fte-loopback-store-{}.sqlite3", random::<u64>()));
        let store = Arc::new(SqliteStore::open(&database_path).expect("file-backed store"));
        let mut config = LoopbackConfig::app_private(token_path.clone());
        config.max_concurrent_requests = 1;
        let server = LoopbackServer::start(gateway, store.clone(), config.clone())
            .await
            .expect("start loopback");
        let address = server
            .addresses()
            .iter()
            .find(|address| address.is_ipv4())
            .copied()
            .expect("IPv4 listener");
        assert!(address.ip().is_loopback());
        let base = format!("http://{address}");
        let token = fs::read_to_string(&token_path).expect("read token");
        let client = reqwest::Client::new();

        let unauthorized = client
            .get(format!("{base}/healthz"))
            .send()
            .await
            .expect("unauthorized request");
        let unauthorized_status = unauthorized.status().as_u16();
        assert_eq!(
            u64::from(unauthorized_status),
            fixture["loopback"]["expected"]["unauthorized_status"]
                .as_u64()
                .expect("unauthorized status fixture")
        );

        let rebound = client
            .get(format!("{base}/healthz"))
            .bearer_auth(&token)
            .header(header::HOST, "evil.example")
            .send()
            .await
            .expect("rebound request");
        let rebound_status = rebound.status().as_u16();
        assert_eq!(
            u64::from(rebound_status),
            fixture["loopback"]["expected"]["rebound_status"]
                .as_u64()
                .expect("rebound status fixture")
        );

        let stream = client
            .post(format!("{base}/v1/responses"))
            .bearer_auth(&token)
            .json(&fixture["loopback"]["requests"]["responses"])
            .send()
            .await
            .expect("Responses stream");
        assert_eq!(stream.status(), StatusCode::OK);

        let while_streaming = client
            .get(format!("{base}/healthz"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("concurrent request");
        let concurrent_status = while_streaming.status().as_u16();
        assert_eq!(
            u64::from(concurrent_status),
            fixture["loopback"]["expected"]["concurrent_status"]
                .as_u64()
                .expect("concurrent status fixture")
        );

        let body = stream.text().await.expect("read stream");
        for event in fixture["loopback"]["expected"]["stream_events"]
            .as_array()
            .expect("stream event fixtures")
        {
            assert!(
                body.contains(&format!(
                    "event: {}",
                    event.as_str().expect("stream event fixture")
                )),
                "missing fixture event {event}"
            );
        }
        assert!(body.contains("\"sequence_number\":0"));
        assert!(body.contains("\"sequence_number\":4"));
        assert!(body.contains("\"item_id\":\"msg_socket_test\""));

        let chat = client
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth(&token)
            .json(&fixture["loopback"]["requests"]["chat"])
            .send()
            .await
            .expect("Chat Completions request");
        assert_eq!(chat.status(), StatusCode::OK);
        let chat = chat.json::<Value>().await.expect("Chat Completions JSON");
        assert_eq!(chat["object"], "chat.completion");
        assert_eq!(chat["choices"][0]["message"]["content"], "hello");
        assert_eq!(chat["usage"]["prompt_tokens"], 1);
        assert_eq!(chat["usage"]["completion_tokens"], 1);

        let completion = client
            .post(format!("{base}/v1/completions"))
            .bearer_auth(&token)
            .json(&fixture["loopback"]["requests"]["completion"])
            .send()
            .await
            .expect("legacy Completions request");
        assert_eq!(completion.status(), StatusCode::OK);
        let completion = completion
            .json::<Value>()
            .await
            .expect("legacy Completions JSON");
        assert_eq!(completion["object"], "text_completion");
        assert_eq!(completion["choices"][0]["text"], "hello");

        let message = client
            .post(format!("{base}/v1/messages"))
            .header("x-api-key", &token)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model":"test-model",
                "max_tokens":64,
                "messages":[{"role":"user","content":"hello"}]
            }))
            .send()
            .await
            .expect("Anthropic Messages request");
        assert_eq!(message.status(), StatusCode::OK);
        let message = message.json::<Value>().await.expect("Messages JSON");
        assert_eq!(message["type"], "message");
        assert_eq!(message["role"], "assistant");
        assert_eq!(message["content"][0]["text"], "hello");
        assert_eq!(message["usage"]["input_tokens"], 1);
        assert_eq!(message["usage"]["output_tokens"], 1);

        let counted = client
            .post(format!("{base}/v1/messages/count_tokens"))
            .header("x-api-key", &token)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model":"test-model",
                "messages":[{"role":"user","content":"hello"}]
            }))
            .send()
            .await
            .expect("Anthropic token count request");
        assert_eq!(counted.status(), StatusCode::OK);
        assert_eq!(
            counted.json::<Value>().await.expect("token count JSON")["input_tokens"],
            7
        );

        let stored = client
            .get(format!("{base}/v1/responses/resp_socket_test"))
            .header("x-api-key", &token)
            .send()
            .await
            .expect("stored response");
        assert_eq!(stored.status(), StatusCode::OK);
        let stored = stored.json::<Value>().await.expect("stored JSON");
        assert_eq!(
            stored["id"],
            fixture["loopback"]["expected"]["stored_response_id"]
        );
        server.shutdown().await;
        drop(store);

        let restarted_gateway = Arc::new(Gateway::new(fte_router::GatewayDefaults::default()));
        restarted_gateway
            .register_backend(Arc::new(StreamingTestBackend))
            .expect("register backend after restart");
        let reopened_store = Arc::new(
            SqliteStore::open(&database_path).expect("reopen file-backed store after shutdown"),
        );
        let restarted = LoopbackServer::start(restarted_gateway, reopened_store, config)
            .await
            .expect("restart loopback");
        let restarted_address = restarted
            .addresses()
            .iter()
            .find(|address| address.is_ipv4())
            .copied()
            .expect("restarted IPv4 listener");
        let restarted_base = format!("http://{restarted_address}");
        let reopened_stored = client
            .get(format!("{restarted_base}/v1/responses/resp_socket_test"))
            .header("x-api-key", &token)
            .send()
            .await
            .expect("stored response after database reopen");
        assert_eq!(reopened_stored.status(), StatusCode::OK);
        let reopened_stored = reopened_stored
            .json::<Value>()
            .await
            .expect("reopened stored response JSON");
        assert_eq!(reopened_stored, stored);
        #[cfg(feature = "unstable-w1-vertical-tests")]
        let stored_response_id = reopened_stored["id"]
            .as_str()
            .expect("reopened stored response ID");
        let continuation = client
            .post(format!("{restarted_base}/v1/responses"))
            .bearer_auth(&token)
            .json(&fixture["loopback"]["requests"]["continuation"])
            .send()
            .await
            .expect("continued stored response after restart");
        assert_eq!(continuation.status(), StatusCode::OK);
        let continuation = continuation
            .json::<Value>()
            .await
            .expect("continuation JSON");
        assert_eq!(
            continuation["id"],
            fixture["loopback"]["expected"]["continuation_response_id"]
        );
        assert_eq!(
            continuation["previous_response_id"],
            fixture["loopback"]["expected"]["stored_response_id"]
        );

        restarted.shutdown().await;
        #[cfg(feature = "unstable-w1-vertical-tests")]
        validate_w1_loopback_projection(
            &fixture,
            LoopbackObservation {
                unauthorized_status,
                rebound_status,
                concurrent_status,
                stream_body: &body,
                chat: &chat,
                completion: &completion,
                stored_response_id,
                stored_response_reopened: reopened_stored == stored,
                continuation: &continuation,
            },
        );
        let _ = fs::remove_dir_all(token_directory);
        let _ = fs::remove_file(&database_path);
        let _ = fs::remove_file(database_path.with_extension("sqlite3-shm"));
        let _ = fs::remove_file(database_path.with_extension("sqlite3-wal"));
    }

    #[tokio::test]
    async fn shutdown_aborts_a_listener_after_a_non_reading_sse_client_exhausts_grace() {
        let gateway = Arc::new(Gateway::new(fte_router::GatewayDefaults::default()));
        gateway
            .register_backend(Arc::new(FloodingTestBackend))
            .expect("register flooding backend");
        let store = Arc::new(SqliteStore::in_memory().expect("in-memory store"));
        let token_directory =
            std::env::temp_dir().join(format!("fte-loopback-stall-{}", random::<u64>()));
        let token_path = token_directory.join("token");
        let server = LoopbackServer::start(
            gateway,
            store,
            LoopbackConfig::app_private(token_path.clone()),
        )
        .await
        .expect("start loopback");
        let address = server
            .addresses()
            .iter()
            .find(|address| address.is_ipv4())
            .copied()
            .expect("IPv4 listener");
        let token = fs::read_to_string(&token_path).expect("read token");
        let response = reqwest::Client::new()
            .post(format!("http://{address}/v1/responses"))
            .bearer_auth(token)
            .json(&json!({
                "model":"test-model",
                "input":"produce a deliberately oversized stream",
                "stream":true
            }))
            .send()
            .await
            .expect("open Responses stream");
        assert_eq!(response.status(), StatusCode::OK);

        tokio::time::timeout(
            Duration::from_secs(1),
            server.shutdown_with_grace(Duration::from_millis(25)),
        )
        .await
        .expect("listener shutdown must be bounded even when the client never reads SSE data");

        drop(response);
        let _ = fs::remove_dir_all(token_directory);
    }
}
