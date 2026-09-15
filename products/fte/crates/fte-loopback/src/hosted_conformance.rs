//! Local provider HTTP -> real hosted adapter -> router -> public HTTP codecs.
use super::*;
use fte_providers::{HostedProviderBackend, HostedProviderConfig};
use fte_store::{SecretResolver, SqliteStore};
use fte_types::{BackendLocation, ModelCapabilities, ModelDescriptor, PromptForm};
use tower::ServiceExt;

struct Secrets;
impl SecretResolver for Secrets {
    fn resolve(&self, _: &str) -> Result<Option<String>, GatewayError> {
        Ok(Some("fixture-only".into()))
    }
}

async fn hosted_exchange(
    protocol: &str,
    content_type: &str,
    bytes: Vec<u8>,
    path: &str,
    body: Value,
) -> (StatusCode, String) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider listener");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("hosted conformance fixture")
    );
    let content_type = content_type.to_owned();
    let provider = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let bytes = bytes.clone();
        let content_type = content_type.clone();
        async move {
            if uri.path().ends_with(":countTokens") {
                (
                    [(header::CONTENT_TYPE, "application/json".to_string())],
                    br#"{"totalTokens":4}"#.to_vec(),
                )
            } else {
                ([(header::CONTENT_TYPE, content_type)], bytes)
            }
        }
    });
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, provider)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .expect("hosted conformance fixture");
    });
    let gateway = Arc::new(Gateway::new(fte_router::GatewayDefaults::default()));
    let models = vec![ModelDescriptor {
        id: "fixture-model".into(),
        aliases: vec![],
        display_name: "Fixture".into(),
        backend_id: "fixture".into(),
        location: BackendLocation::Hosted,
        capabilities: ModelCapabilities {
            prompt_forms: vec![PromptForm::Chat, PromptForm::Completion],
            streaming: true,
            tools: true,
            ..Default::default()
        },
        context_tokens: None,
        max_output_tokens: None,
        observed: Default::default(),
    }];
    let mut config = match protocol {
        "responses" => HostedProviderConfig::openai("fixture", "Fixture", "fixture", models),
        "anthropic" => HostedProviderConfig::anthropic("fixture", "Fixture", "fixture", models),
        "gemini" => HostedProviderConfig::gemini("fixture", "Fixture", "fixture", models),
        _ => HostedProviderConfig::openai_compatible("fixture", "Fixture", "fixture", &url, models),
    };
    config.endpoints.messages = Some(url.clone());
    config.endpoints.responses = Some(url.clone());
    config.endpoints.count_tokens = Some(url.clone());
    config.endpoints.completions = Some(url);
    gateway
        .register_backend(Arc::new(
            HostedProviderBackend::new(config, Arc::new(Secrets))
                .expect("hosted conformance fixture"),
        ))
        .expect("hosted conformance fixture");
    let mut state = tests::regression_state(Arc::new(
        SqliteStore::in_memory().expect("hosted conformance fixture"),
    ));
    state.gateway = Arc::clone(&gateway);
    state.edge_defaults.privacy = fte_types::PrivacyPolicy::HostedAllowed;
    state.edge_defaults.profile = fte_types::RouteProfile::Auto;
    let response = router(state, 8192)
        .oneshot(tests::http_request(path, body))
        .await
        .expect("hosted conformance fixture");
    let status = response.status();
    let text = tests::response_body(response).await;
    gateway
        .shutdown()
        .await
        .expect("hosted conformance fixture");
    stop.send(()).expect("hosted conformance fixture");
    server.await.expect("hosted conformance fixture");
    (status, text)
}

fn chat_request(stream: bool) -> Value {
    json!({"model":"fixture-model","messages":[{"role":"user","content":"hello"}],"stream":stream})
}

fn sse(values: &[Value]) -> Vec<u8> {
    let mut text = values
        .iter()
        .map(|value| format!("data: {value}\n\n"))
        .collect::<String>();
    text.push_str("data: [DONE]\n\n");
    text.into_bytes()
}

#[tokio::test]
async fn hosted_chat_tools_survive_http_projection() {
    let provider = json!({"choices":[{"index":0,"message":{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"weather","arguments":"{\"city\":\"Paris\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":4,"completion_tokens":5}});
    let (status, text) = hosted_exchange(
        "chat",
        "application/json",
        provider.to_string().into_bytes(),
        "/v1/chat/completions",
        chat_request(false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let value: Value = serde_json::from_str(&text).expect("hosted conformance fixture");
    assert_eq!(value["choices"][0]["finish_reason"], "tool_calls", "{text}");
    assert_eq!(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Paris\"}"
    );
}

#[tokio::test]
async fn hosted_fragmented_tool_arguments_survive_stream_projection() {
    let body = sse(&[
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"weather","arguments":"{\"city\":"}}]},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
    ]);
    let (_, text) = hosted_exchange(
        "chat",
        "text/event-stream",
        body,
        "/v1/chat/completions",
        chat_request(true),
    )
    .await;
    let values = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    assert!(
        values.iter().any(
            |v| v["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]
                == "{\"city\":\"Paris\"}"
        ),
        "{text}"
    );
    assert!(text.contains("\"finish_reason\":\"tool_calls\""), "{text}");
}

#[tokio::test]
async fn hosted_responses_incomplete_is_not_completed() {
    let provider = json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","id":"m","content":[{"type":"output_text","text":"partial"}]}]});
    let (status, text) = hosted_exchange(
        "responses",
        "application/json",
        provider.to_string().into_bytes(),
        "/v1/responses",
        json!({"model":"fixture-model","input":"hello","store":false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let value: Value = serde_json::from_str(&text).expect("hosted conformance fixture");
    assert_eq!(value["status"], "incomplete", "{text}");
    assert_eq!(value["incomplete_details"]["reason"], "max_output_tokens");
}

#[tokio::test]
async fn hosted_partial_stream_eof_is_failure() {
    let body = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n".to_vec();
    let (_, text) = hosted_exchange(
        "chat",
        "text/event-stream",
        body,
        "/v1/chat/completions",
        chat_request(true),
    )
    .await;
    assert!(text.contains("provider_stream_incomplete"), "{text}");
    assert!(!text.contains("[DONE]"), "{text}");
}

#[tokio::test]
async fn hosted_choices_and_finish_reasons_survive_http_projection() {
    let body = json!({"choices":[{"index":0,"message":{"content":"complete"},"finish_reason":"stop"},{"index":1,"message":{"content":"partial"},"finish_reason":"length"}]});
    let (status, text) = hosted_exchange(
        "chat",
        "application/json",
        body.to_string().into_bytes(),
        "/v1/chat/completions",
        chat_request(false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let value: Value = serde_json::from_str(&text).expect("hosted conformance fixture");
    assert_eq!(
        value["choices"]
            .as_array()
            .expect("hosted conformance fixture")
            .len(),
        2,
        "{text}"
    );
    assert_eq!(value["choices"][0]["message"]["content"], "complete");
    assert_eq!(value["choices"][0]["finish_reason"], "stop");
    assert_eq!(value["choices"][1]["message"]["content"], "partial");
    assert_eq!(value["choices"][1]["finish_reason"], "length");
}

#[tokio::test]
async fn hosted_crlf_stream_uses_incremental_frames() {
    let body = sse(&[
        json!({"choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
    ]);
    let body = String::from_utf8(body)
        .expect("hosted conformance fixture")
        .replace('\n', "\r\n")
        .into_bytes();
    let (_, text) = hosted_exchange(
        "chat",
        "text/event-stream",
        body,
        "/v1/chat/completions",
        chat_request(true),
    )
    .await;
    assert!(text.contains("[DONE]"), "{text}");
    assert!(text.contains("hello"), "{text}");
}

fn fixture_model(id: &str) -> ModelDescriptor {
    ModelDescriptor {
        id: "fixture-model".into(),
        aliases: vec![],
        display_name: id.into(),
        backend_id: id.into(),
        location: BackendLocation::Hosted,
        capabilities: ModelCapabilities {
            prompt_forms: vec![PromptForm::Chat],
            streaming: true,
            tools: true,
            ..Default::default()
        },
        context_tokens: None,
        max_output_tokens: None,
        observed: Default::default(),
    }
}

struct CountFixture {
    backend: Arc<HostedProviderBackend>,
    stop: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
    counts: Arc<std::sync::atomic::AtomicUsize>,
}

async fn count_fixture(
    id: &str,
    count: u64,
    fail_execute: bool,
    count_gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
) -> CountFixture {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("hosted conformance fixture");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("hosted conformance fixture")
    );
    let label = id.to_owned();
    let counts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count_calls = Arc::clone(&counts);
    let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let label = label.clone();
        let gate = count_gate.clone();
        let calls = Arc::clone(&count_calls);
        async move {
            if uri.path() == "/count" {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if let Some((entered, release)) = gate { entered.notify_one(); release.notified().await; }
                return Json(json!({"input_tokens":count})).into_response();
            }
            if fail_execute { return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":{"message":"fixture unavailable"}}))).into_response(); }
            let values = [
                json!({"type":"message_start","message":{"usage":{"input_tokens":count,"output_tokens":0}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":label}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
                json!({"type":"message_stop"}),
            ];
            let body = values.iter().map(|value| format!("data: {value}\n\n")).collect::<String>();
            ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
        }
    });
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .expect("hosted conformance fixture");
    });
    let mut config = HostedProviderConfig::anthropic(id, id, "fixture", vec![fixture_model(id)]);
    config.endpoints.messages = Some(format!("{url}/messages"));
    config.endpoints.count_tokens = Some(format!("{url}/count"));
    CountFixture {
        backend: Arc::new(
            HostedProviderBackend::new(config, Arc::new(Secrets))
                .expect("hosted conformance fixture"),
        ),
        stop,
        server,
        counts,
    }
}

fn hosted_state(gateway: Arc<Gateway>) -> AppState {
    let mut state = tests::regression_state(Arc::new(
        SqliteStore::in_memory().expect("hosted conformance fixture"),
    ));
    state.gateway = gateway;
    state.edge_defaults.privacy = fte_types::PrivacyPolicy::HostedAllowed;
    state.edge_defaults.profile = fte_types::RouteProfile::Auto;
    state
}

fn anthropic_request() -> Value {
    json!({"model":"auto","messages":[{"role":"user","content":"hello"}],"max_tokens":16,"stream":true})
}

fn initial_input(text: &str) -> u64 {
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| {
            value
                .pointer("/message/usage/input_tokens")
                .and_then(Value::as_u64)
        })
        .expect("message_start exact input usage")
}

#[tokio::test]
async fn hosted_count_and_execution_keep_one_route_during_catalog_change() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let a = count_fixture(
        "z-original",
        10,
        false,
        Some((Arc::clone(&entered), Arc::clone(&release))),
    )
    .await;
    let b = count_fixture("a-new-preferred", 20, false, None).await;
    let gateway = Arc::new(Gateway::new(fte_router::GatewayDefaults::default()));
    gateway
        .register_backend(a.backend.clone())
        .expect("hosted conformance fixture");
    let app = router(hosted_state(Arc::clone(&gateway)), 8192);
    let request = tokio::spawn(async move {
        app.oneshot(tests::http_request("/v1/messages", anthropic_request()))
            .await
            .expect("hosted conformance fixture")
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("provider count entered");
    gateway
        .register_backend(b.backend.clone())
        .expect("hosted conformance fixture");
    release.notify_one();
    let text = tests::response_body(request.await.expect("hosted conformance fixture")).await;
    let served_original = text.contains("z-original");
    assert_eq!(
        initial_input(&text),
        if served_original { 10 } else { 20 },
        "initial usage must belong to actual serving route: {text}"
    );
    gateway
        .shutdown()
        .await
        .expect("hosted conformance fixture");
    a.stop.send(()).expect("hosted conformance fixture");
    b.stop.send(()).expect("hosted conformance fixture");
    a.server.await.expect("hosted conformance fixture");
    b.server.await.expect("hosted conformance fixture");
}

#[tokio::test]
async fn hosted_fallback_recounts_before_anthropic_message_start() {
    for fail_fallback in [false, true] {
        let a = count_fixture("a-first", 10, true, None).await;
        let b = count_fixture("b-fallback", 20, fail_fallback, None).await;
        let gateway = Arc::new(Gateway::new(fte_router::GatewayDefaults::default()));
        gateway
            .register_backend(a.backend.clone())
            .expect("hosted conformance fixture");
        gateway
            .register_backend(b.backend.clone())
            .expect("hosted conformance fixture");
        let state = hosted_state(Arc::clone(&gateway));
        let wire: AnthropicMessagesRequest =
            serde_json::from_value(anthropic_request()).expect("hosted conformance fixture");
        let mut request = wire
            .into_gateway(state.edge_defaults.clone())
            .expect("hosted conformance fixture");
        request.routing.retry_before_output = true;
        let result = gateway
            .execute_with_exact_input_count(
                request,
                Arc::new(fte_types::RequestCancellation::default()),
            )
            .await;
        if fail_fallback {
            assert!(result.is_err(), "no stream before a viable route");
        } else {
            let (ticket, usage) = result.expect("hosted conformance fixture");
            assert_eq!(
                usage
                    .selected_route
                    .as_ref()
                    .expect("hosted conformance fixture")
                    .backend_id,
                "b-fallback"
            );
            let text = tests::response_body(anthropic_stream(
                ticket,
                state,
                require_exact_input_tokens(&usage).expect("hosted conformance fixture"),
            ))
            .await;
            assert_eq!(initial_input(&text), 20, "{text}");
            assert!(text.contains("b-fallback"), "{text}");
        }
        assert_eq!(a.counts.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(b.counts.load(std::sync::atomic::Ordering::SeqCst), 1);
        gateway
            .shutdown()
            .await
            .expect("hosted conformance fixture");
        a.stop.send(()).expect("hosted conformance fixture");
        b.stop.send(()).expect("hosted conformance fixture");
        a.server.await.expect("hosted conformance fixture");
        b.server.await.expect("hosted conformance fixture");
    }
}

#[tokio::test]
async fn hosted_route_count_cancellation_prevents_generation() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let fixture = count_fixture(
        "fixture",
        10,
        false,
        Some((Arc::clone(&entered), Arc::clone(&release))),
    )
    .await;
    let gateway = Arc::new(Gateway::new(fte_router::GatewayDefaults::default()));
    gateway
        .register_backend(fixture.backend.clone())
        .expect("hosted conformance fixture");
    let wire: AnthropicMessagesRequest =
        serde_json::from_value(anthropic_request()).expect("hosted conformance fixture");
    let request = wire
        .into_gateway(hosted_state(Arc::clone(&gateway)).edge_defaults)
        .expect("hosted conformance fixture");
    let cancellation = Arc::new(fte_types::RequestCancellation::default());
    let running_gateway = Arc::clone(&gateway);
    let running_cancel = Arc::clone(&cancellation);
    let running = tokio::spawn(async move {
        running_gateway
            .execute_with_exact_input_count(request, running_cancel)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("hosted conformance fixture");
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .expect("count cancellation is bounded")
        .expect("hosted conformance fixture");
    assert!(matches!(result, Err(error) if error.class == fte_types::ErrorClass::Cancelled));
    release.notify_one();
    gateway
        .shutdown()
        .await
        .expect("hosted conformance fixture");
    fixture.stop.send(()).expect("hosted conformance fixture");
    fixture.server.await.expect("hosted conformance fixture");
}

#[tokio::test]
async fn hosted_transport_rejects_oversized_and_invalid_provider_results() {
    for (content_type, body, stream, code) in [
        (
            "text/event-stream",
            [b"data: ".as_slice(), &vec![b'x'; 1024 * 1024]].concat(),
            true,
            "provider_frame_too_large",
        ),
        (
            "text/event-stream",
            b"data: \xff\n\n".to_vec(),
            true,
            "provider_stream_invalid_utf8",
        ),
        (
            "application/json",
            vec![b' '; 8 * 1024 * 1024 + 1],
            false,
            "provider_result_too_large",
        ),
        (
            "application/json",
            vec![b' '; 8 * 1024 * 1024 + 1],
            true,
            "provider_result_too_large",
        ),
    ] {
        let (_, text) = hosted_exchange(
            "chat",
            content_type,
            body,
            "/v1/chat/completions",
            chat_request(stream),
        )
        .await;
        assert!(text.contains(code), "expected {code}: {text}");
        assert!(
            !text.contains("[DONE]"),
            "oversized/invalid output cannot complete: {text}"
        );
    }
}

#[tokio::test]
async fn hosted_stream_cancellation_while_waiting_for_frame_delimiter_drains() {
    use futures::StreamExt;
    let entered = Arc::new(tokio::sync::Notify::new());
    let provider_entered = Arc::clone(&entered);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("hosted conformance fixture");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("hosted conformance fixture")
    );
    let provider = Router::new().fallback(move || {
        let entered = Arc::clone(&provider_entered);
        async move {
            let body = stream! {
                yield Ok::<_, Infallible>("data: {\"choices\":");
                entered.notify_one();
                std::future::pending::<()>().await;
            };
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from_stream(body),
            )
        }
    });
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, provider)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .expect("hosted conformance fixture");
    });
    let config = HostedProviderConfig::openai_compatible(
        "fixture",
        "fixture",
        "fixture",
        url,
        vec![fixture_model("fixture")],
    );
    let gateway = Arc::new(Gateway::new(fte_router::GatewayDefaults::default()));
    gateway
        .register_backend(Arc::new(
            HostedProviderBackend::new(config, Arc::new(Secrets))
                .expect("hosted conformance fixture"),
        ))
        .expect("hosted conformance fixture");
    let app = router(hosted_state(Arc::clone(&gateway)), 8192);
    let response = app
        .clone()
        .oneshot(tests::http_request(
            "/v1/responses",
            json!({"model":"fixture-model","input":"hello","stream":true,"store":false}),
        ))
        .await
        .expect("hosted conformance fixture");
    let mut body = response.into_body().into_data_stream();
    let first = body
        .next()
        .await
        .expect("hosted conformance fixture")
        .expect("hosted conformance fixture");
    let first = std::str::from_utf8(&first).expect("hosted conformance fixture");
    let event: Value = first
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).expect("hosted conformance fixture"))
        .expect("hosted conformance fixture");
    let response_id = event["response"]["id"]
        .as_str()
        .expect("hosted conformance fixture");
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("hosted conformance fixture");
    let cancelled = app
        .oneshot(tests::http_request(
            &format!("/v1/responses/{response_id}/cancel"),
            Value::Null,
        ))
        .await
        .expect("hosted conformance fixture");
    assert_eq!(cancelled.status(), StatusCode::OK);
    let remaining = tokio::time::timeout(Duration::from_secs(2), async {
        let mut text = String::new();
        while let Some(chunk) = body.next().await {
            text.push_str(
                std::str::from_utf8(&chunk.expect("hosted conformance fixture"))
                    .expect("hosted conformance fixture"),
            );
        }
        text
    })
    .await
    .expect("delimiter wait cancelled");
    assert!(remaining.contains("response.incomplete"), "{remaining}");
    tokio::time::timeout(Duration::from_secs(2), gateway.shutdown())
        .await
        .expect("owned hosted work drained")
        .expect("hosted conformance fixture");
    stop.send(()).expect("hosted conformance fixture");
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("fixture connection closed")
        .expect("hosted conformance fixture");
}

#[tokio::test]
async fn hosted_limits_refusals_and_gemini_tools_keep_their_meaning() {
    let cases = [
        (
            "chat",
            json!({"choices":[{"index":0,"message":{"content":"partial"},"finish_reason":"length"}]}),
            "length",
        ),
        (
            "chat",
            json!({"choices":[{"index":0,"message":{"content":null,"refusal":"Cannot comply"},"finish_reason":"stop"}]}),
            "stop",
        ),
        (
            "gemini",
            json!({"candidates":[{"index":0,"content":{"parts":[{"text":"partial"}]},"finishReason":"MAX_TOKENS"}]}),
            "length",
        ),
        (
            "gemini",
            json!({"candidates":[{"index":0,"content":{"parts":[{"functionCall":{"id":"gemini-original-call","name":"weather","args":{"city":"Paris","index":999}}}]},"finishReason":"STOP"}]}),
            "tool_calls",
        ),
    ];
    for (protocol, provider, finish) in cases {
        for stream in [false, true] {
            let (_, text) = hosted_exchange(
                protocol,
                "application/json",
                provider.to_string().into_bytes(),
                "/v1/chat/completions",
                chat_request(stream),
            )
            .await;
            assert!(
                text.contains(&format!("\"finish_reason\":\"{finish}\"")),
                "{text}"
            );
            if provider.pointer("/choices/0/message/refusal").is_some() {
                assert!(text.contains("\"refusal\":\"Cannot comply\""), "{text}");
            }
            if finish == "tool_calls" {
                assert!(text.contains("weather"), "{text}");
                assert!(text.contains("gemini-original-call"), "{text}");
            }
        }
    }
}

#[tokio::test]
async fn hosted_anthropic_preserves_exact_stop_reason_and_sequence() {
    for reason in [
        "end_turn",
        "max_tokens",
        "model_context_window_exceeded",
        "stop_sequence",
        "refusal",
    ] {
        let provider = json!({"content":[{"type":"text","text":"answer"}],"stop_reason":reason,"stop_sequence":if reason == "stop_sequence" {json!("END")} else {Value::Null},"usage":{"input_tokens":4,"output_tokens":1}});
        let mut request = anthropic_request();
        request["stream"] = json!(false);
        let (status, text) = hosted_exchange(
            "anthropic",
            "application/json",
            provider.to_string().into_bytes(),
            "/v1/messages",
            request,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        let value: Value = serde_json::from_str(&text).expect("hosted conformance fixture");
        assert_eq!(value["stop_reason"], reason, "{text}");
        assert_eq!(value["stop_sequence"], provider["stop_sequence"]);
    }
}

#[tokio::test]
async fn hosted_stream_refusal_and_token_limit_do_not_become_plain_stops() {
    for refusal in [false, true] {
        let delta = if refusal {
            json!({"refusal":"Cannot comply"})
        } else {
            json!({"content":"partial"})
        };
        let reason = if refusal { "stop" } else { "length" };
        let body = sse(&[
            json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]}),
        ]);
        let (_, text) = hosted_exchange(
            "chat",
            "text/event-stream",
            body,
            "/v1/chat/completions",
            chat_request(true),
        )
        .await;
        assert!(text.contains("[DONE]"), "{text}");
        assert!(
            text.contains(&format!("\"finish_reason\":\"{reason}\"")),
            "{text}"
        );
        if refusal {
            assert!(text.contains("\"refusal\":\"Cannot comply\""), "{text}");
        }
    }
}

#[tokio::test]
async fn hosted_terminal_requires_all_choices_and_valid_tool_arguments() {
    let cases = [
        ("chat", "text/event-stream", sse(&[json!({"choices":[{"index":0,"delta":{"content":"complete"},"finish_reason":"stop"},{"index":1,"delta":{"content":"partial"},"finish_reason":null}]})]), "provider_stream_incomplete"),
        ("responses", "application/json", json!({"status":"completed","output":[{"type":"function_call","id":"fc_1","call_id":"call_1","name":"weather","arguments":"{"}]}).to_string().into_bytes(), "provider_tool_arguments_invalid"),
        ("chat", "application/json", json!({"choices":[{"message":{"content":"answer"},"finish_reason":"unknown-new-reason"}]}).to_string().into_bytes(), "provider_finish_reason_unsupported"),
    ];
    for (protocol, content_type, body, error) in cases {
        let (_, text) = hosted_exchange(
            protocol,
            content_type,
            body,
            "/v1/responses",
            json!({"model":"fixture-model","input":"hello","store":false,"stream":true}),
        )
        .await;
        assert!(text.contains(error), "{text}");
        assert!(!text.contains("event: response.completed"), "{text}");
    }
}

#[tokio::test]
async fn hosted_aggregate_tool_arguments_are_bounded_before_terminal() {
    let mut frames = vec![
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"weather","arguments":"{\"text\":\""}}]},"finish_reason":null}]}),
    ];
    let fragment = "x".repeat(512 * 1024);
    for _ in 0..17 {
        frames.push(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":fragment}}]},"finish_reason":null}]}));
    }
    let (_, text) = hosted_exchange(
        "chat",
        "text/event-stream",
        sse(&frames),
        "/v1/chat/completions",
        chat_request(true),
    )
    .await;
    assert!(text.contains("provider_result_too_large"), "{text}");
    assert!(
        !text.contains("[DONE]"),
        "oversized arguments cannot complete"
    );
}

#[tokio::test]
async fn hosted_interleaved_choices_keep_progress_and_terminal_identity() {
    let body = sse(&[
        json!({"choices":[{"index":1,"delta":{"content":"second"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"weather","arguments":"{}"}}]},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":"first"},"finish_reason":"tool_calls"},{"index":1,"delta":{},"finish_reason":"length"}]}),
    ]);
    let (_, text) = hosted_exchange(
        "chat",
        "text/event-stream",
        body,
        "/v1/chat/completions",
        chat_request(true),
    )
    .await;
    assert!(text.contains("[DONE]"), "{text}");
    let values = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok());
    let mut contents = HashMap::<u64, String>::new();
    let mut finishes = HashMap::<u64, String>::new();
    for value in values {
        assert!(
            value["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("chatcmpl")),
            "{value}"
        );
        assert_eq!(value["model"], "fixture-model", "{value}");
        for choice in value["choices"]
            .as_array()
            .expect("hosted conformance fixture")
        {
            let index = choice["index"]
                .as_u64()
                .expect("hosted conformance fixture");
            if let Some(content) = choice["delta"]["content"].as_str() {
                contents.entry(index).or_default().push_str(content);
            }
            if let Some(finish) = choice["finish_reason"].as_str() {
                finishes.insert(index, finish.into());
            }
        }
    }
    assert_eq!(
        contents.get(&0).map(String::as_str),
        Some("first"),
        "{text}"
    );
    assert_eq!(
        contents.get(&1).map(String::as_str),
        Some("second"),
        "{text}"
    );
    assert_eq!(finishes.get(&0).map(String::as_str), Some("tool_calls"));
    assert_eq!(finishes.get(&1).map(String::as_str), Some("length"));
}

#[tokio::test]
async fn hosted_anthropic_stream_rejects_alternative_candidates() {
    let provider = json!({"candidates":[
        {"index":0,"content":{"parts":[{"text":"first"}]},"finishReason":"STOP"},
        {"index":1,"content":{"parts":[{"text":"second"}]},"finishReason":"STOP"}
    ],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2}});
    let (_, text) = hosted_exchange(
        "gemini", "application/json", provider.to_string().into_bytes(),
        "/v1/messages",
        json!({"model":"fixture-model","messages":[{"role":"user","content":"hello"}],"max_tokens":10,"stream":true}),
    ).await;
    assert!(
        text.contains("provider_multiple_candidates_unsupported"),
        "{text}"
    );
    assert!(!text.contains("event: message_stop"), "{text}");
}
