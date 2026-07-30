use crate::providers::{ChatChunk, ChatRequest};
use crate::router::Router as TokenRouter;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderName, HeaderValue, Request, Response, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
    Json, Router as AxumRouter,
};
use bytes::Bytes;
use futures::StreamExt;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Weak};
use tokio::sync::{oneshot, Mutex};
use tracing::{info, warn};

const MAX_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;
const MIN_USER_PORT: u16 = 1024;

#[derive(Debug, Clone, Serialize)]
pub struct ProxyStatus {
    pub running: bool,
    pub port: Option<u16>,
}

#[derive(Default)]
struct ProxyRuntimeState {
    port: Option<u16>,
    generation: u64,
    shutdown: Option<oneshot::Sender<()>>,
}

pub struct ProxyManager {
    router: Arc<TokenRouter>,
    state: Mutex<ProxyRuntimeState>,
}

impl ProxyManager {
    pub fn new(router: Arc<TokenRouter>) -> Arc<Self> {
        Arc::new(Self {
            router,
            state: Mutex::new(ProxyRuntimeState::default()),
        })
    }

    pub async fn restart(self: &Arc<Self>, port: u16) -> anyhow::Result<ProxyStatus> {
        if port < MIN_USER_PORT {
            return Err(anyhow::anyhow!(
                "Port must be between {MIN_USER_PORT} and {}.",
                u16::MAX
            ));
        }

        let mut state = self.state.lock().await;
        if state.port == Some(port) && state.shutdown.is_some() {
            return Ok(ProxyStatus {
                running: true,
                port: Some(port),
            });
        }

        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|error| anyhow::anyhow!("Could not listen on {address}: {error}"))?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        if let Some(previous_shutdown) = state.shutdown.take() {
            let _ = previous_shutdown.send(());
        }
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        state.port = Some(port);
        state.shutdown = Some(shutdown_tx);
        drop(state);

        let app = openai_app(self.router.clone());
        let manager = Arc::downgrade(self);
        tokio::spawn(async move {
            info!("Local API proxy listening on http://{address}");
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
            if let Err(error) = result {
                warn!("Local API proxy stopped with an error: {error}");
            }
            clear_proxy_state(manager, generation).await;
        });

        Ok(ProxyStatus {
            running: true,
            port: Some(port),
        })
    }

    pub async fn status(&self) -> ProxyStatus {
        let state = self.state.lock().await;
        ProxyStatus {
            running: state.shutdown.is_some(),
            port: state.port,
        }
    }
}

async fn clear_proxy_state(manager: Weak<ProxyManager>, generation: u64) {
    let Some(manager) = manager.upgrade() else {
        return;
    };
    let mut state = manager.state.lock().await;
    if state.generation == generation {
        state.port = None;
        state.shutdown = None;
    }
}

pub fn openai_app(router: Arc<TokenRouter>) -> AxumRouter {
    AxumRouter::new()
        .route("/v1/models", get(list_models))
        .route("/v1/completions", post(completions))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses))
        .route("/v1/messages", post(anthropic_messages))
        .route("/v1beta/models/{model_method}", post(gemini_dispatch))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn(security_headers))
        .with_state(router)
}

async fn security_headers(request: Request<Body>, next: Next) -> Response<Body> {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    response
}

#[derive(Debug, Serialize)]
pub struct ModelsResponse {
    object: &'static str,
    data: Vec<ModelResponse>,
}

#[derive(Debug, Serialize)]
pub struct ModelResponse {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: String,
}

#[derive(Debug, Serialize)]
pub struct OpenAiErrorResponse {
    error: OpenAiError,
}

#[derive(Debug, Serialize)]
pub struct OpenAiError {
    message: String,
    #[serde(rename = "type")]
    error_type: &'static str,
    param: Option<String>,
    code: String,
}

async fn list_models(State(router): State<Arc<TokenRouter>>) -> Json<ModelsResponse> {
    Json(models_response(&router))
}

async fn chat_completions(
    State(router): State<Arc<TokenRouter>>,
    Json(req): Json<ChatRequest>,
) -> Response<Body> {
    if req.model.trim().is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "model is required",
            Some("model"),
            "missing_required_parameter",
        )
        .into_response();
    }
    if req.messages.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "messages must contain at least one item",
            Some("messages"),
            "invalid_request_error",
        )
        .into_response();
    }

    if req.stream {
        return match router.chat_stream(&req, "default").await {
            Ok(stream) => sse_response(stream),
            Err(e) => api_error(status_for_error(&e), &e.to_string(), None, "gateway_error")
                .into_response(),
        };
    }

    match router.chat(&req, "default").await {
        Ok(res) => Json(res).into_response(),
        Err(e) => {
            api_error(status_for_error(&e), &e.to_string(), None, "gateway_error").into_response()
        }
    }
}

async fn completions(
    State(router): State<Arc<TokenRouter>>,
    Json(body): Json<Value>,
) -> Response<Body> {
    let req = match chat_request_from_completion(&body) {
        Ok(req) => req,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                &error.to_string(),
                None,
                "invalid_request_error",
            )
            .into_response();
        }
    };

    if req.stream {
        return match router.chat_stream(&req, "default").await {
            Ok(stream) => completion_sse_response(stream),
            Err(error) => api_error(
                status_for_error(&error),
                &error.to_string(),
                None,
                "gateway_error",
            )
            .into_response(),
        };
    }

    match router.chat(&req, "default").await {
        Ok(res) => Json(completion_response_from_chat(&body, res)).into_response(),
        Err(error) => api_error(
            status_for_error(&error),
            &error.to_string(),
            None,
            "gateway_error",
        )
        .into_response(),
    }
}

async fn responses(
    State(router): State<Arc<TokenRouter>>,
    Json(body): Json<Value>,
) -> Response<Body> {
    let req = match chat_request_from_response_create(&body) {
        Ok(req) => req,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                &error.to_string(),
                None,
                "invalid_request_error",
            )
            .into_response();
        }
    };

    if req.stream {
        return match router.chat_stream(&req, "default").await {
            Ok(stream) => responses_sse_response(stream),
            Err(error) => api_error(
                status_for_error(&error),
                &error.to_string(),
                None,
                "gateway_error",
            )
            .into_response(),
        };
    }

    match router.chat(&req, "default").await {
        Ok(res) => Json(response_create_from_chat(&body, res)).into_response(),
        Err(error) => api_error(
            status_for_error(&error),
            &error.to_string(),
            None,
            "gateway_error",
        )
        .into_response(),
    }
}

async fn anthropic_messages(
    State(router): State<Arc<TokenRouter>>,
    Json(body): Json<Value>,
) -> Response<Body> {
    let req = match chat_request_from_anthropic(&body) {
        Ok(req) => req,
        Err(error) => {
            return anthropic_error(StatusCode::BAD_REQUEST, &error.to_string()).into_response();
        }
    };

    if req.stream {
        return match router.chat_stream(&req, "default").await {
            Ok(stream) => anthropic_sse_response(stream),
            Err(error) => {
                anthropic_error(status_for_error(&error), &error.to_string()).into_response()
            }
        };
    }

    match router.chat(&req, "default").await {
        Ok(res) => Json(anthropic_response_from_chat(res)).into_response(),
        Err(error) => anthropic_error(status_for_error(&error), &error.to_string()).into_response(),
    }
}

async fn gemini_generate_content(
    State(router): State<Arc<TokenRouter>>,
    Path(model): Path<String>,
    Json(body): Json<Value>,
) -> Response<Body> {
    let req = match chat_request_from_gemini(&model, &body, false) {
        Ok(req) => req,
        Err(error) => {
            return google_error(StatusCode::BAD_REQUEST, &error.to_string()).into_response();
        }
    };

    match router.chat(&req, "default").await {
        Ok(res) => Json(gemini_response_from_chat(res)).into_response(),
        Err(error) => google_error(status_for_error(&error), &error.to_string()).into_response(),
    }
}

async fn gemini_dispatch(
    State(router): State<Arc<TokenRouter>>,
    Path(model_method): Path<String>,
    Json(body): Json<Value>,
) -> Response<Body> {
    if let Some(model) = model_method.strip_suffix(":generateContent") {
        return gemini_generate_content(State(router), Path(model.to_string()), Json(body)).await;
    }
    if let Some(model) = model_method.strip_suffix(":streamGenerateContent") {
        return gemini_stream_generate_content(State(router), Path(model.to_string()), Json(body))
            .await;
    }

    google_error(StatusCode::NOT_FOUND, "Unknown Gemini model method").into_response()
}

async fn gemini_stream_generate_content(
    State(router): State<Arc<TokenRouter>>,
    Path(model): Path<String>,
    Json(body): Json<Value>,
) -> Response<Body> {
    let req = match chat_request_from_gemini(&model, &body, true) {
        Ok(req) => req,
        Err(error) => {
            return google_error(StatusCode::BAD_REQUEST, &error.to_string()).into_response();
        }
    };

    match router.chat_stream(&req, "default").await {
        Ok(stream) => gemini_sse_response(stream),
        Err(error) => google_error(status_for_error(&error), &error.to_string()).into_response(),
    }
}

fn models_response(router: &TokenRouter) -> ModelsResponse {
    let data = router
        .public_models()
        .into_iter()
        .map(|model| ModelResponse {
            id: model.id,
            object: "model",
            created: 0,
            owned_by: model.providers.join(","),
        })
        .collect();

    ModelsResponse {
        object: "list",
        data,
    }
}

fn status_for_error(error: &anyhow::Error) -> StatusCode {
    if let Some(status) = crate::providers::openai_compatible::upstream_http_status(error) {
        return StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    }
    if crate::providers::openai_compatible::is_transport_timeout(error) {
        return StatusCode::GATEWAY_TIMEOUT;
    }

    let message = error.to_string();
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("unknown model")
        || normalized.contains("model is required")
        || normalized.contains("messages must contain")
        || normalized.contains("streaming responses")
        || normalized.contains("requested capability")
        || normalized.contains("supports every requested capability")
    {
        StatusCode::BAD_REQUEST
    } else if normalized.contains("out of quota")
        || normalized.contains("quota was exhausted")
        || normalized.contains("request quota")
    {
        StatusCode::TOO_MANY_REQUESTS
    } else if normalized.contains("no api key") {
        StatusCode::SERVICE_UNAVAILABLE
    } else if normalized.contains("timed out") {
        StatusCode::GATEWAY_TIMEOUT
    } else {
        StatusCode::BAD_GATEWAY
    }
}

fn api_error(
    status: StatusCode,
    message: &str,
    param: Option<&str>,
    code: &str,
) -> (StatusCode, Json<OpenAiErrorResponse>) {
    (
        status,
        Json(OpenAiErrorResponse {
            error: OpenAiError {
                message: message.to_string(),
                error_type: "invalid_request_error",
                param: param.map(|value| value.to_string()),
                code: code.to_string(),
            },
        }),
    )
}

fn anthropic_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": message
            }
        })),
    )
}

fn google_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({
            "error": {
                "code": status.as_u16(),
                "message": message,
                "status": "INVALID_ARGUMENT"
            }
        })),
    )
}

fn sse_response(
    stream: futures::stream::BoxStream<'static, anyhow::Result<ChatChunk>>,
) -> Response<Body> {
    let body_stream = stream
        .map(|chunk_result| match chunk_result {
            Ok(chunk) => chat_chunk_sse_event(&chunk),
            Err(error) => {
                let error_body = OpenAiErrorResponse {
                    error: OpenAiError {
                        message: error.to_string(),
                        error_type: "gateway_stream_error",
                        param: None,
                        code: "gateway_stream_error".to_string(),
                    },
                };
                serde_json::to_string(&error_body)
                    .map(|json| Bytes::from(format!("data: {json}\n\n")))
                    .map_err(anyhow::Error::from)
            }
        })
        .chain(futures::stream::once(async {
            Ok::<Bytes, anyhow::Error>(Bytes::from_static(b"data: [DONE]\n\n"))
        }));

    sse_body_response(body_stream)
}

fn chat_chunk_sse_event(chunk: &ChatChunk) -> anyhow::Result<Bytes> {
    let json = serde_json::to_string(chunk)?;
    Ok(Bytes::from(format!("data: {json}\n\n")))
}

fn completion_sse_response(
    stream: futures::stream::BoxStream<'static, anyhow::Result<ChatChunk>>,
) -> Response<Body> {
    let body_stream = stream
        .map(|chunk_result| match chunk_result {
            Ok(chunk) => completion_chunk_sse_event(&chunk),
            Err(error) => serde_json::to_string(&json!({
                "error": {
                    "message": error.to_string(),
                    "type": "gateway_stream_error"
                }
            }))
            .map(|json| Bytes::from(format!("data: {json}\n\n")))
            .map_err(anyhow::Error::from),
        })
        .chain(futures::stream::once(async {
            Ok::<Bytes, anyhow::Error>(Bytes::from_static(b"data: [DONE]\n\n"))
        }));

    sse_body_response(body_stream)
}

fn responses_sse_response(
    stream: futures::stream::BoxStream<'static, anyhow::Result<ChatChunk>>,
) -> Response<Body> {
    let body_stream = futures::stream::once(async {
        Ok::<Bytes, anyhow::Error>(Bytes::from_static(
            b"event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
        ))
    })
    .chain(stream.map(|chunk_result| {
        match chunk_result {
            Ok(chunk) => responses_chunk_sse_event(&chunk),
            Err(error) => serde_json::to_string(&json!({
                "type": "error",
                "error": {
                    "message": error.to_string(),
                    "type": "gateway_stream_error"
                }
            }))
            .map(|json| Bytes::from(format!("event: error\ndata: {json}\n\n")))
            .map_err(anyhow::Error::from),
        }
    }))
    .chain(futures::stream::once(async {
        Ok::<Bytes, anyhow::Error>(Bytes::from_static(
            b"event: response.completed\ndata: {\"type\":\"response.completed\"}\n\n",
        ))
    }));

    sse_body_response(body_stream)
}

fn anthropic_sse_response(
    stream: futures::stream::BoxStream<'static, anyhow::Result<ChatChunk>>,
) -> Response<Body> {
    let body_stream = futures::stream::once(async {
        Ok::<Bytes, anyhow::Error>(Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_gateway\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"gateway\",\"stop_reason\":null,\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\n",
        ))
    })
    .chain(futures::stream::once(async {
        Ok::<Bytes, anyhow::Error>(Bytes::from_static(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        ))
    }))
    .chain(stream.map(|chunk_result| match chunk_result {
        Ok(chunk) => anthropic_chunk_sse_event(&chunk),
        Err(error) => serde_json::to_string(&json!({
            "type": "error",
            "error": {
                "type": "gateway_stream_error",
                "message": error.to_string()
            }
        }))
        .map(|json| Bytes::from(format!("event: error\ndata: {json}\n\n")))
        .map_err(anyhow::Error::from),
    }))
    .chain(futures::stream::once(async {
        Ok::<Bytes, anyhow::Error>(Bytes::from_static(
            b"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ))
    }));

    sse_body_response(body_stream)
}

fn gemini_sse_response(
    stream: futures::stream::BoxStream<'static, anyhow::Result<ChatChunk>>,
) -> Response<Body> {
    let body_stream = stream.map(|chunk_result| match chunk_result {
        Ok(chunk) => gemini_chunk_sse_event(&chunk),
        Err(error) => serde_json::to_string(&json!({
            "error": {
                "message": error.to_string(),
                "status": "INTERNAL"
            }
        }))
        .map(|json| Bytes::from(format!("data: {json}\n\n")))
        .map_err(anyhow::Error::from),
    });

    sse_body_response(body_stream)
}

fn sse_body_response<S>(body_stream: S) -> Response<Body>
where
    S: futures::Stream<Item = anyhow::Result<Bytes>> + Send + 'static,
{
    let mut response = Response::new(Body::from_stream(body_stream));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

fn completion_chunk_sse_event(chunk: &ChatChunk) -> anyhow::Result<Bytes> {
    let text = chunk
        .choices
        .first()
        .and_then(|choice| choice.delta.content.clone())
        .unwrap_or_default();
    let json = serde_json::to_string(&json!({
        "id": chunk.id,
        "object": "text_completion",
        "created": chunk.created.unwrap_or_default(),
        "model": chunk.model.clone().unwrap_or_else(|| "auto".to_string()),
        "choices": [{
            "text": text,
            "index": 0,
            "finish_reason": chunk.choices.first().and_then(|choice| choice.finish_reason.clone())
        }],
        "usage": chunk.usage
    }))?;
    Ok(Bytes::from(format!("data: {json}\n\n")))
}

fn responses_chunk_sse_event(chunk: &ChatChunk) -> anyhow::Result<Bytes> {
    let text = chunk
        .choices
        .first()
        .and_then(|choice| choice.delta.content.clone())
        .unwrap_or_default();
    if text.is_empty() {
        return Ok(Bytes::from_static(b""));
    }
    let json = serde_json::to_string(&json!({
        "type": "response.output_text.delta",
        "delta": text
    }))?;
    Ok(Bytes::from(format!(
        "event: response.output_text.delta\ndata: {json}\n\n"
    )))
}

fn anthropic_chunk_sse_event(chunk: &ChatChunk) -> anyhow::Result<Bytes> {
    let choice = chunk.choices.first();
    if let Some(text) = choice.and_then(|choice| choice.delta.content.clone()) {
        let json = serde_json::to_string(&json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "text_delta",
                "text": text
            }
        }))?;
        return Ok(Bytes::from(format!(
            "event: content_block_delta\ndata: {json}\n\n"
        )));
    }

    if let Some(usage) = &chunk.usage {
        let json = serde_json::to_string(&json!({
            "type": "message_delta",
            "delta": {},
            "usage": {
                "output_tokens": usage.completion_tokens
            }
        }))?;
        return Ok(Bytes::from(format!(
            "event: message_delta\ndata: {json}\n\n"
        )));
    }

    Ok(Bytes::from_static(b""))
}

fn gemini_chunk_sse_event(chunk: &ChatChunk) -> anyhow::Result<Bytes> {
    let text = chunk
        .choices
        .first()
        .and_then(|choice| choice.delta.content.clone())
        .unwrap_or_default();
    let finish_reason = chunk
        .choices
        .first()
        .and_then(|choice| choice.finish_reason.clone())
        .map(|reason| match reason.as_str() {
            "stop" => "STOP".to_string(),
            "length" => "MAX_TOKENS".to_string(),
            "content_filter" => "SAFETY".to_string(),
            other => other.to_string(),
        });
    let json = serde_json::to_string(&json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": if text.is_empty() { Vec::<Value>::new() } else { vec![json!({ "text": text })] }
            },
            "finishReason": finish_reason
        }],
        "usageMetadata": chunk.usage.as_ref().map(|usage| json!({
            "promptTokenCount": usage.prompt_tokens,
            "candidatesTokenCount": usage.completion_tokens,
            "totalTokenCount": usage.total_tokens
        }))
    }))?;
    Ok(Bytes::from(format!("data: {json}\n\n")))
}

fn chat_request_from_completion(body: &Value) -> anyhow::Result<ChatRequest> {
    let model = required_string(body, "model")?;
    let prompt = body.get("prompt").map(value_to_text).unwrap_or_default();
    let max_tokens = optional_u32(body.get("max_tokens"), "max_tokens")?;
    let mut extra = Map::new();
    copy_value(body, &mut extra, "stop", "stop");
    copy_value(body, &mut extra, "top_p", "top_p");
    copy_value(
        body,
        &mut extra,
        "max_completion_tokens",
        "max_completion_tokens",
    );

    Ok(ChatRequest {
        model,
        messages: vec![crate::providers::ChatMessage::text("user", &prompt)],
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        stream_options: None,
        temperature: body
            .get("temperature")
            .and_then(Value::as_f64)
            .map(|value| value as f32),
        max_tokens,
        extra,
    })
}

fn completion_response_from_chat(
    request: &Value,
    response: crate::providers::ChatResponse,
) -> Value {
    let content = response
        .choices
        .first()
        .map(|choice| choice.message.content_text())
        .unwrap_or_default();
    json!({
        "id": response.id,
        "object": "text_completion",
        "created": response.created.unwrap_or_default(),
        "model": request.get("model").cloned().or(response.model.map(Value::String)).unwrap_or(Value::String("auto".to_string())),
        "choices": [{
            "text": content,
            "index": 0,
            "finish_reason": response.choices.first().and_then(|choice| choice.finish_reason.clone())
        }],
        "usage": response.usage
    })
}

fn chat_request_from_response_create(body: &Value) -> anyhow::Result<ChatRequest> {
    let model = required_string(body, "model")?;
    let max_tokens = optional_u32(
        body.get("max_output_tokens")
            .or_else(|| body.get("max_tokens")),
        "max_output_tokens",
    )?;
    let mut messages = Vec::new();
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str) {
        messages.push(crate::providers::ChatMessage::text(
            "developer",
            instructions,
        ));
    }

    match body.get("input") {
        Some(Value::String(input)) => {
            messages.push(crate::providers::ChatMessage::text("user", input));
        }
        Some(Value::Array(items)) => {
            messages.extend(items.iter().filter_map(response_input_item_to_chat_message));
        }
        Some(other) => {
            messages.push(crate::providers::ChatMessage::text(
                "user",
                &other.to_string(),
            ));
        }
        None => return Err(anyhow::anyhow!("input is required")),
    }

    let mut extra = Map::new();
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        extra.insert(
            "tools".to_string(),
            Value::Array(
                tools
                    .iter()
                    .filter_map(response_tool_to_chat_tool)
                    .collect(),
            ),
        );
    }
    copy_value(body, &mut extra, "tool_choice", "tool_choice");
    copy_value(body, &mut extra, "reasoning", "reasoning");
    if let Some(text) = body.get("text") {
        extra.insert(
            "response_format".to_string(),
            text.get("format")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "text" })),
        );
    }

    Ok(ChatRequest {
        model,
        messages,
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        stream_options: None,
        temperature: body
            .get("temperature")
            .and_then(Value::as_f64)
            .map(|value| value as f32),
        max_tokens,
        extra,
    })
}

fn response_create_from_chat(request: &Value, response: crate::providers::ChatResponse) -> Value {
    let message = response.choices.first().map(|choice| &choice.message);
    let output_text = message
        .map(|message| message.content_text())
        .unwrap_or_default();
    let mut output = Vec::new();
    if !output_text.is_empty() {
        output.push(json!({
            "id": "msg_0",
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": output_text,
                "annotations": []
            }]
        }));
    }
    if let Some(tool_calls) = message
        .and_then(|message| message.extra.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for tool_call in tool_calls {
            let function = tool_call.get("function").unwrap_or(tool_call);
            output.push(json!({
                "type": "function_call",
                "id": tool_call.get("id").cloned().unwrap_or_else(|| Value::String(format!("fc_{}", output.len()))),
                "call_id": tool_call.get("id").cloned().unwrap_or_else(|| Value::String(format!("call_{}", output.len()))),
                "name": function.get("name").cloned().unwrap_or(Value::String("tool".to_string())),
                "arguments": function.get("arguments").cloned().unwrap_or(Value::String("{}".to_string()))
            }));
        }
    }

    json!({
        "id": response.id,
        "object": "response",
        "created_at": response.created.unwrap_or_default(),
        "status": "completed",
        "model": request.get("model").cloned().or(response.model.map(Value::String)).unwrap_or(Value::String("auto".to_string())),
        "output": output,
        "usage": response.usage
    })
}

fn chat_request_from_anthropic(body: &Value) -> anyhow::Result<ChatRequest> {
    let model = required_string(body, "model")?;
    let max_tokens = optional_u32(body.get("max_tokens"), "max_tokens")?;
    let mut messages = Vec::new();

    if let Some(system) = body.get("system") {
        messages.push(crate::providers::ChatMessage {
            role: "system".to_string(),
            content: anthropic_content_to_openai_content(system),
            extra: Map::new(),
        });
    }

    for item in body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("messages is required"))?
    {
        let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
        let content = item.get("content").cloned().unwrap_or(Value::Null);
        messages.push(crate::providers::ChatMessage {
            role: if role == "assistant" {
                "assistant".to_string()
            } else {
                "user".to_string()
            },
            content: anthropic_content_to_openai_content(&content),
            extra: Map::new(),
        });
    }

    let mut extra = Map::new();
    copy_value(body, &mut extra, "tools", "tools");
    copy_value(body, &mut extra, "tool_choice", "tool_choice");
    copy_value(body, &mut extra, "thinking", "thinking");
    if let Some(stop_sequences) = body.get("stop_sequences") {
        extra.insert("stop".to_string(), stop_sequences.clone());
    }
    copy_value(body, &mut extra, "top_p", "top_p");
    copy_value(body, &mut extra, "top_k", "top_k");

    Ok(ChatRequest {
        model,
        messages,
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        stream_options: None,
        temperature: body
            .get("temperature")
            .and_then(Value::as_f64)
            .map(|value| value as f32),
        max_tokens,
        extra,
    })
}

fn anthropic_response_from_chat(response: crate::providers::ChatResponse) -> Value {
    let choice = response.choices.first();
    let message = choice.map(|choice| &choice.message);
    let mut content = Vec::new();
    if let Some(text) = message
        .map(|message| message.content_text())
        .filter(|text| !text.is_empty())
    {
        content.push(json!({ "type": "text", "text": text }));
    }
    if let Some(tool_calls) = message
        .and_then(|message| message.extra.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for tool_call in tool_calls {
            let function = tool_call.get("function").unwrap_or(tool_call);
            content.push(json!({
                "type": "tool_use",
                "id": tool_call.get("id").and_then(Value::as_str).unwrap_or("tool_use"),
                "name": function.get("name").and_then(Value::as_str).unwrap_or("tool"),
                "input": function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                    .unwrap_or_else(|| json!({}))
            }));
        }
    }

    json!({
        "id": response.id,
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": response.model.unwrap_or_else(|| "auto".to_string()),
        "stop_reason": choice.and_then(|choice| choice.finish_reason.clone()).map(openai_finish_to_anthropic),
        "stop_sequence": null,
        "usage": response.usage.map(|usage| json!({
            "input_tokens": usage.prompt_tokens,
            "output_tokens": usage.completion_tokens
        })).unwrap_or_else(|| json!({
            "input_tokens": 0,
            "output_tokens": 0
        }))
    })
}

fn chat_request_from_gemini(
    model: &str,
    body: &Value,
    stream: bool,
) -> anyhow::Result<ChatRequest> {
    let mut messages = Vec::new();

    if let Some(system) = body.get("systemInstruction") {
        messages.push(crate::providers::ChatMessage {
            role: "system".to_string(),
            content: gemini_parts_to_openai_content(system.get("parts").unwrap_or(system)),
            extra: Map::new(),
        });
    }

    for item in body
        .get("contents")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("contents is required"))?
    {
        let role = match item.get("role").and_then(Value::as_str) {
            Some("model") => "assistant",
            _ => "user",
        };
        messages.push(crate::providers::ChatMessage {
            role: role.to_string(),
            content: gemini_parts_to_openai_content(
                item.get("parts").unwrap_or(&Value::Array(Vec::new())),
            ),
            extra: Map::new(),
        });
    }

    let generation_config = body.get("generationConfig");
    let max_tokens = optional_u32(
        generation_config.and_then(|config| config.get("maxOutputTokens")),
        "generationConfig.maxOutputTokens",
    )?;
    let mut extra = Map::new();
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        extra.insert("tools".to_string(), Value::Array(tools.clone()));
    }
    copy_value(body, &mut extra, "toolConfig", "tool_config");
    copy_value(body, &mut extra, "safetySettings", "safety_settings");
    copy_value(body, &mut extra, "cachedContent", "cached_content");
    if let Some(thinking_config) = generation_config.and_then(|config| config.get("thinkingConfig"))
    {
        extra.insert("thinking_config".to_string(), thinking_config.clone());
    }

    Ok(ChatRequest {
        model: model.strip_prefix("models/").unwrap_or(model).to_string(),
        messages,
        stream,
        stream_options: None,
        temperature: generation_config
            .and_then(|config| config.get("temperature"))
            .and_then(Value::as_f64)
            .map(|value| value as f32),
        max_tokens,
        extra,
    })
}

fn gemini_response_from_chat(response: crate::providers::ChatResponse) -> Value {
    let choice = response.choices.first();
    let message = choice.map(|choice| &choice.message);
    json!({
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{
                    "text": message.map(|message| message.content_text()).unwrap_or_default()
                }]
            },
            "finishReason": choice.and_then(|choice| choice.finish_reason.clone()).map(openai_finish_to_gemini)
        }],
        "usageMetadata": response.usage.map(|usage| json!({
            "promptTokenCount": usage.prompt_tokens,
            "candidatesTokenCount": usage.completion_tokens,
            "totalTokenCount": usage.total_tokens
        }))
    })
}

fn response_input_item_to_chat_message(item: &Value) -> Option<crate::providers::ChatMessage> {
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    match item_type {
        "message" => Some(crate::providers::ChatMessage {
            role: item
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_string(),
            content: response_content_to_openai_content(item.get("content")?),
            extra: Map::new(),
        }),
        "function_call_output" => {
            let mut extra = Map::new();
            copy_value(item, &mut extra, "call_id", "tool_call_id");
            Some(crate::providers::ChatMessage {
                role: "tool".to_string(),
                content: item
                    .get("output")
                    .cloned()
                    .unwrap_or(Value::String(String::new())),
                extra,
            })
        }
        _ => None,
    }
}

fn response_tool_to_chat_tool(tool: &Value) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    Some(json!({
        "type": "function",
        "function": {
            "name": tool.get("name")?,
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
            "strict": tool.get("strict").cloned().unwrap_or(Value::Null)
        }
    }))
}

fn anthropic_content_to_openai_content(content: &Value) -> Value {
    match content {
        Value::String(_) => content.clone(),
        Value::Array(blocks) => Value::Array(
            blocks
                .iter()
                .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                    Some("text") => Some(json!({
                        "type": "text",
                        "text": block.get("text").and_then(Value::as_str).unwrap_or_default()
                    })),
                    Some("image") | Some("document") => Some(block.clone()),
                    Some("tool_use") | Some("thinking") | Some("redacted_thinking") => {
                        Some(block.clone())
                    }
                    Some("tool_result") => Some(json!({
                        "type": "text",
                        "text": value_to_text(block.get("content").unwrap_or(&Value::Null))
                    })),
                    _ => None,
                })
                .collect(),
        ),
        _ => Value::String(value_to_text(content)),
    }
}

fn gemini_parts_to_openai_content(parts: &Value) -> Value {
    match parts {
        Value::Array(parts) => Value::Array(
            parts
                .iter()
                .map(|part| {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        json!({
                            "type": "text",
                            "text": text
                        })
                    } else {
                        part.clone()
                    }
                })
                .collect(),
        ),
        _ => Value::String(value_to_text(parts)),
    }
}

fn response_content_to_openai_content(content: &Value) -> Value {
    match content {
        Value::String(_) => content.clone(),
        Value::Array(parts) => Value::Array(
            parts
                .iter()
                .filter_map(|part| {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        Some(json!({ "type": "text", "text": text }))
                    } else if let Some(text) = part.get("type").and_then(Value::as_str) {
                        if text == "input_text" || text == "output_text" {
                            Some(json!({
                                "type": "text",
                                "text": part.get("text").and_then(Value::as_str).unwrap_or_default()
                            }))
                        } else {
                            Some(part.clone())
                        }
                    } else {
                        None
                    }
                })
                .collect(),
        ),
        _ => Value::String(value_to_text(content)),
    }
}

fn required_string(body: &Value, key: &str) -> anyhow::Result<String> {
    body.get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow::anyhow!("{key} is required"))
}

fn optional_u32(value: Option<&Value>, key: &str) -> anyhow::Result<Option<u32>> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let number = value
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("{key} must be a non-negative integer"))?;
    Ok(Some(u32::try_from(number).map_err(|_| {
        anyhow::anyhow!("{key} exceeds the maximum supported value")
    })?))
}

fn copy_value(from: &Value, to: &mut Map<String, Value>, source_key: &str, target_key: &str) {
    if let Some(value) = from.get(source_key) {
        to.insert(target_key.to_string(), value.clone());
    }
}

fn value_to_text(value: &Value) -> String {
    crate::providers::text_from_value(value)
}

fn openai_finish_to_anthropic(reason: String) -> String {
    match reason.as_str() {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        "content_filter" => "refusal",
        other => other,
    }
    .to_string()
}

fn openai_finish_to_gemini(reason: String) -> String {
    match reason.as_str() {
        "stop" => "STOP",
        "length" => "MAX_TOKENS",
        "content_filter" => "SAFETY",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::eval_store::EvalStore;
    use crate::providers::{ChatChunkChoice, ChatDelta};
    use crate::rate_limiter::QuotaTracker;

    #[test]
    fn model_list_uses_openai_list_shape() {
        let router = test_router("model-list");
        let response = models_response(&router);
        let json = serde_json::to_value(response).unwrap();

        assert_eq!(json["object"], "list");
        assert!(json["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["object"] == "model" && item["id"] == "llama-3.1-70b-instruct" }));
    }

    #[test]
    fn api_router_constructs_with_gemini_method_paths() {
        let _app = openai_app(Arc::new(test_router("route-construction")));
    }

    #[test]
    fn request_errors_use_openai_error_shape() {
        let (_status, Json(error)) = api_error(
            StatusCode::BAD_REQUEST,
            "model is required",
            Some("model"),
            "missing_required_parameter",
        );
        let json = serde_json::to_value(error).unwrap();

        assert_eq!(json["error"]["message"], "model is required");
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(json["error"]["param"], "model");
        assert_eq!(json["error"]["code"], "missing_required_parameter");
    }

    #[test]
    fn gateway_errors_map_to_actionable_http_statuses() {
        assert_eq!(
            status_for_error(&anyhow::anyhow!("Request quota was exhausted")),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            status_for_error(&anyhow::anyhow!("No API key configured")),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status_for_error(&anyhow::anyhow!("upstream timed out")),
            StatusCode::GATEWAY_TIMEOUT
        );
    }

    #[test]
    fn numeric_request_fields_reject_overflow_and_negative_values() {
        assert!(chat_request_from_completion(&json!({
            "model": "auto",
            "prompt": "hello",
            "max_tokens": u64::MAX
        }))
        .is_err());
        assert!(chat_request_from_completion(&json!({
            "model": "auto",
            "prompt": "hello",
            "max_tokens": -1
        }))
        .is_err());
    }

    #[test]
    fn stream_chunks_are_encoded_as_sse_data_events() {
        let chunk = ChatChunk {
            id: "chunk-test".to_string(),
            object: Some("chat.completion.chunk".to_string()),
            created: Some(1),
            model: Some("auto".to_string()),
            choices: vec![ChatChunkChoice {
                index: 0,
                delta: ChatDelta {
                    role: None,
                    content: Some("hi".to_string()),
                    extra: serde_json::Map::new(),
                },
                finish_reason: None,
            }],
            usage: None,
        };

        let bytes = chat_chunk_sse_event(&chunk).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.starts_with("data: {"));
        assert!(text.contains("\"object\":\"chat.completion.chunk\""));
        assert!(text.ends_with("\n\n"));
    }

    #[test]
    fn anthropic_request_conversion_maps_system_and_messages() {
        let req = chat_request_from_anthropic(&json!({
            "model": "claude-sonnet-4-20250514",
            "system": "system rules",
            "messages": [{ "role": "user", "content": "hello" }],
            "max_tokens": 128
        }))
        .unwrap();

        assert_eq!(req.model, "claude-sonnet-4-20250514");
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[1].role, "user");
        assert_eq!(req.max_tokens, Some(128));
    }

    #[test]
    fn gemini_request_conversion_maps_contents() {
        let req = chat_request_from_gemini(
            "gemini-2.5-flash",
            &json!({
                "systemInstruction": { "parts": [{ "text": "system rules" }] },
                "contents": [{
                    "role": "user",
                    "parts": [{ "text": "hello" }]
                }],
                "generationConfig": { "maxOutputTokens": 128 }
            }),
            false,
        )
        .unwrap();

        assert_eq!(req.model, "gemini-2.5-flash");
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(req.messages[1].role, "user");
        assert_eq!(req.max_tokens, Some(128));
    }

    fn test_router(name: &str) -> TokenRouter {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "free-token-energy-api-test-{}-{}.sqlite",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        TokenRouter::new(
            Arc::new(QuotaTracker::new()),
            Arc::new(EvalStore::new()),
            Arc::new(Database::new(path).unwrap()),
        )
    }
}
