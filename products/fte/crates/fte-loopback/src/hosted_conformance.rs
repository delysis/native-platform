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
    let (status, text, _) =
        hosted_exchange_captured(protocol, content_type, bytes, path, body).await;
    (status, text)
}

async fn hosted_exchange_captured(
    protocol: &str,
    content_type: &str,
    bytes: Vec<u8>,
    path: &str,
    body: Value,
) -> (StatusCode, String, Vec<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider listener");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("hosted conformance fixture")
    );
    let content_type = content_type.to_owned();
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_for_provider = Arc::clone(&captured);
    let provider =
        axum::Router::new().fallback(move |uri: axum::http::Uri, Json(body): Json<Value>| {
            let bytes = bytes.clone();
            let content_type = content_type.clone();
            let captured = Arc::clone(&captured_for_provider);
            async move {
                captured
                    .lock()
                    .expect("captured provider requests")
                    .push(body);
                if uri.path().ends_with("/count_tokens") {
                    (
                        [(header::CONTENT_TYPE, "application/json".to_string())],
                        br#"{"input_tokens":4}"#.to_vec(),
                    )
                } else if uri.path().ends_with(":countTokens") {
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
            modalities: vec![fte_types::Modality::Text, fte_types::Modality::Image],
            prompt_forms: vec![PromptForm::Chat, PromptForm::Completion],
            streaming: true,
            tools: true,
            provider_cache: true,
            ..Default::default()
        },
        context_tokens: None,
        max_output_tokens: None,
        quota: Default::default(),
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
    config.endpoints.count_tokens = Some(format!("{url}/count_tokens"));
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
    let captured = captured.lock().expect("captured provider requests").clone();
    (status, text, captured)
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
async fn public_tool_selection_survives_without_gateway_execution_authority() {
    for (protocol, path) in [
        ("chat", "/v1/chat/completions"),
        ("responses", "/v1/responses"),
        ("anthropic", "/v1/messages"),
    ] {
        for choice in ["omitted", "none", "auto", "required", "named"] {
            let (mut body, provider) = match protocol {
                "chat" => (
                    json!({"model":"fixture-model","messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"lookup","parameters":{}}}]}),
                    json!({"choices":[{"index":0,"message":{"content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":1}}),
                ),
                "responses" => (
                    json!({"model":"fixture-model","input":"hello","store":false,"tools":[{"type":"function","name":"lookup","parameters":{}}]}),
                    json!({"status":"completed","output":[],"usage":{"input_tokens":4,"output_tokens":1}}),
                ),
                _ => (
                    json!({"model":"fixture-model","max_tokens":16,"messages":[{"role":"user","content":"hello"}],"tools":[{"name":"lookup","input_schema":{}}]}),
                    json!({"content":[{"type":"text","text":"answer"}],"stop_reason":"end_turn","usage":{"input_tokens":4,"output_tokens":1}}),
                ),
            };
            let expected = match (protocol, choice) {
                ("anthropic", "omitted" | "auto") => json!({"type":"auto"}),
                ("anthropic", "none") => json!({"type":"none"}),
                ("anthropic", "required") => json!({"type":"any","disable_parallel_tool_use":true}),
                ("anthropic", "named") => {
                    json!({"type":"tool","name":"lookup","disable_parallel_tool_use":false})
                }
                ("chat", "named") => json!({"type":"function","function":{"name":"lookup"}}),
                ("responses", "named") => json!({"type":"function","name":"lookup"}),
                (_, "omitted") => json!("auto"),
                (_, choice) => json!(choice),
            };
            if choice != "omitted" {
                body["tool_choice"] = expected.clone();
            }
            let (status, text, captured) = hosted_exchange_captured(
                protocol,
                "application/json",
                provider.to_string().into_bytes(),
                path,
                body,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{protocol}/{choice}: {text}");
            assert_eq!(captured.len(), 1);
            assert_eq!(captured[0]["tool_choice"], expected, "{protocol}/{choice}");
        }
    }
}

#[tokio::test]
async fn malformed_or_undeclared_tool_choices_fail_before_hosted_dispatch() {
    for choice in [
        json!({"anything":"ignored"}),
        json!({"type":"function","function":{"name":"missing"}}),
        json!({"type":"function","function":{"name":"lookup","extra":true}}),
    ] {
        let (status, text, captured) = hosted_exchange_captured("chat", "application/json", b"{}".to_vec(), "/v1/chat/completions", json!({"model":"fixture-model","messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function","function":{"name":"lookup","parameters":{}}}],"tool_choice":choice})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert!(captured.is_empty());
    }
}

#[tokio::test]
async fn anthropic_mixed_history_keeps_order_media_and_cache_targets_in_both_bodies() {
    let provider = json!({"content":[{"type":"text","text":"answer"}],"stop_reason":"end_turn","usage":{"input_tokens":4,"output_tokens":1}});
    let (status, text, captured) = hosted_exchange_captured(
        "anthropic", "application/json", provider.to_string().into_bytes(), "/v1/messages",
        json!({
            "model":"fixture-model", "max_tokens":16, "stream":true,
            "system":[{"type":"text","text":"system","cache_control":{"type":"ephemeral","ttl":"1h"}}],
            "messages":[
                {"role":"assistant","content":[
                    {"type":"text","text":"A"},
                    {"type":"tool_use","id":"call_1","name":"lookup","input":{"n":1}},
                    {"type":"text","text":"B","cache_control":{"type":"ephemeral","ttl":"1h"}}
                ]},
                {"role":"assistant","content":[
                    {"type":"text","text":"C","cache_control":{"type":"ephemeral"}},
                    {"type":"tool_use","id":"call_2","name":"lookup","input":{"n":2}},
                    {"type":"text","text":"D"}
                ]},
                {"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"call_1","is_error":true,"cache_control":{"type":"ephemeral"},"content":[
                        {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQ=="}},
                        {"type":"text","text":"result"}
                    ]},
                    {"type":"tool_result","tool_use_id":"call_2","content":"second"},
                    {"type":"text","text":"E"}
                ]}
            ]
        }),
    ).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        captured.len(),
        2,
        "one count request and one generation request"
    );
    assert_eq!(captured[0]["messages"], captured[1]["messages"]);
    assert_eq!(captured[0]["system"], captured[1]["system"]);
    for body in &captured {
        let assistant = body["messages"][0]["content"]
            .as_array()
            .expect("assistant blocks");
        assert_eq!(assistant.len(), 6);
        assert_eq!(assistant[0]["text"], "A");
        assert_eq!(assistant[1]["id"], "call_1");
        assert_eq!(assistant[2]["text"], "B");
        assert_eq!(assistant[2]["cache_control"]["ttl"], "1h");
        assert_eq!(assistant[3]["text"], "C");
        assert_eq!(assistant[3]["cache_control"]["ttl"], "5m");
        assert_eq!(assistant[4]["id"], "call_2");
        assert_eq!(assistant[5]["text"], "D");
        let user = body["messages"][1]["content"]
            .as_array()
            .expect("user blocks");
        assert_eq!(user.len(), 3);
        assert_eq!(user[0]["tool_use_id"], "call_1");
        assert_eq!(user[0]["is_error"], true);
        assert_eq!(user[0]["cache_control"]["ttl"], "5m");
        assert_eq!(user[0]["content"][0]["source"]["data"], "AQ==");
        assert_eq!(user[0]["content"][1]["text"], "result");
        assert_eq!(user[1]["tool_use_id"], "call_2");
        assert_eq!(user[2]["text"], "E");
    }
}

#[tokio::test]
async fn unsupported_known_fields_fail_before_hosted_dispatch() {
    for (path, body, code) in [
        (
            "/v1/chat/completions",
            json!({"model":"fixture-model","messages":[{"role":"user","content":"hello","name":"participant"}]}),
            "chat_participant_name_unsupported",
        ),
        (
            "/v1/chat/completions",
            json!({"model":"fixture-model","messages":[],"tools":[{"type":"other","function":{"name":"lookup","parameters":{}}}]}),
            "chat_tool_definition_type_unsupported",
        ),
        (
            "/v1/responses",
            json!({"model":"fixture-model","input":"hello","text":{"verbosity":"high"}}),
            "responses_text_verbosity_unsupported",
        ),
        (
            "/v1/responses",
            json!({"model":"fixture-model","input":"hello","text":{"format":{"type":"json_schema","name":"answer","description":"must not vanish","schema":{}}}}),
            "json_schema_description_unsupported",
        ),
    ] {
        let (status, text, captured) =
            hosted_exchange_captured("responses", "application/json", b"{}".to_vec(), path, body)
                .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        let error: Value = serde_json::from_str(&text).expect("typed public field error");
        assert_eq!(error["error"]["code"], code);
        assert!(
            captured.is_empty(),
            "unsupported field reached provider: {captured:?}"
        );
    }
}

#[tokio::test]
async fn hosted_json_anthropic_stream_never_emits_a_delta_after_block_stop() {
    let provider = json!({
        "content":[
            {"type":"text","text":"answer"},
            {"type":"thinking","thinking":"check","signature":"sig_exact"},
            {"type":"tool_use","id":"call_1","name":"lookup","input":{"term":"日本語"}}
        ],
        "stop_reason":"tool_use",
        "usage":{"input_tokens":4,"output_tokens":3}
    });
    let (status, text) = hosted_exchange(
        "anthropic", "application/json", provider.to_string().into_bytes(), "/v1/messages",
        json!({"model":"fixture-model","messages":[{"role":"user","content":"hello"}],"max_tokens":16,"stream":true}),
    ).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let values = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str::<Value>(line).expect("Anthropic SSE JSON"))
        .collect::<Vec<_>>();
    let mut stopped = std::collections::BTreeSet::new();
    let mut deltas = Vec::new();
    for value in &values {
        match value["type"].as_str() {
            Some("content_block_delta") => {
                let index = value["index"].as_u64().expect("block index");
                assert!(
                    !stopped.contains(&index),
                    "delta after stopped block: {text}"
                );
                deltas.push(value["delta"].clone());
            }
            Some("content_block_stop") => {
                assert!(
                    stopped.insert(value["index"].as_u64().expect("block index")),
                    "duplicate block stop: {text}"
                );
            }
            Some("error") => panic!("unexpected stream failure: {text}"),
            _ => {}
        }
    }
    assert_eq!(stopped.len(), 3);
    assert_eq!(
        deltas
            .iter()
            .filter(|delta| delta["text"] == "answer")
            .count(),
        1
    );
    assert_eq!(
        deltas
            .iter()
            .filter(|delta| delta["signature"] == "sig_exact")
            .count(),
        1
    );
    assert!(
        deltas
            .iter()
            .any(|delta| delta["partial_json"] == "{\"term\":\"日本語\"}"),
        "{text}"
    );
    assert_eq!(
        values.last().expect("terminal message")["type"],
        "message_stop"
    );
}

#[tokio::test]
async fn hosted_usage_overflow_has_an_unavailable_public_total() {
    let provider = json!({"choices":[{"index":0,"message":{"content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":u64::MAX,"completion_tokens":1}});
    for stream in [false, true] {
        let (status, text) = hosted_exchange(
            "chat",
            "application/json",
            provider.to_string().into_bytes(),
            "/v1/chat/completions",
            chat_request(stream),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        if stream {
            let terminal = text
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|value| value["choices"][0]["finish_reason"] == "stop")
                .expect("terminal completion frame");
            assert!(terminal.get("usage").is_none(), "{text}");
        } else {
            let value: Value = serde_json::from_str(&text).expect("public JSON response");
            assert!(value.get("usage").is_none(), "{text}");
        }
    }
    let provider = json!({"status":"completed","output":[],"usage":{"input_tokens":u64::MAX,"output_tokens":1}});
    let (status, text) = hosted_exchange(
        "responses",
        "application/json",
        provider.to_string().into_bytes(),
        "/v1/responses",
        json!({"model":"fixture-model","input":"hello","store":false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let response: Value = serde_json::from_str(&text).expect("public response");
    assert_eq!(response["usage"], Value::Null);
}

#[tokio::test]
async fn hosted_responses_reasoning_replay_keeps_the_wire_string() {
    for encrypted in ["", "opaque_日本語🔒"] {
        let provider = json!({"status":"completed","output":[{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":encrypted}]});
        let (status, text, captured) = hosted_exchange_captured(
            "responses",
            "application/json",
            provider.to_string().into_bytes(),
            "/v1/responses",
            json!({"model":"fixture-model","input":[{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":encrypted}],"store":false}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0]["input"][0]["encrypted_content"], encrypted);
        let value: Value = serde_json::from_str(&text).expect("public reasoning response");
        assert_eq!(value["output"][0]["encrypted_content"], encrypted);
    }
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
            modalities: vec![fte_types::Modality::Text],
            prompt_forms: vec![PromptForm::Chat],
            streaming: true,
            tools: true,
            ..Default::default()
        },
        context_tokens: None,
        max_output_tokens: None,
        quota: Default::default(),
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

// A new Gateway and store binding for each turn; the provider listener remains real HTTP.
fn continuation_state(
    url: &str,
    protocol: &str,
    store: Arc<dyn fte_types::ResponseStore>,
) -> AppState {
    let mut state = tests::regression_state(store);
    let gateway = Arc::new(Gateway::new(fte_router::GatewayDefaults::default()));
    gateway
        .bind_response_store(Arc::clone(&state.store))
        .expect("owned response store");
    let models = ["fixture-model", "other-model"]
        .into_iter()
        .map(|id| ModelDescriptor {
            id: id.into(),
            aliases: vec![],
            display_name: id.into(),
            backend_id: "fixture".into(),
            location: BackendLocation::Hosted,
            capabilities: ModelCapabilities {
                modalities: vec![fte_types::Modality::Text],
                prompt_forms: vec![PromptForm::Chat],
                streaming: true,
                ..Default::default()
            },
            context_tokens: None,
            max_output_tokens: None,
            quota: Default::default(),
            observed: Default::default(),
        })
        .collect();
    let mut config = if protocol == "responses" {
        HostedProviderConfig::openai("fixture", "Fixture", "fixture", models)
    } else {
        HostedProviderConfig::openai_compatible("fixture", "Fixture", "fixture", url, models)
    };
    config.endpoints.messages = Some(url.into());
    config.endpoints.responses = Some(url.into());
    gateway
        .register_backend(Arc::new(
            HostedProviderBackend::new(config, Arc::new(Secrets)).expect("real hosted adapter"),
        ))
        .expect("configured route");
    state.gateway = gateway;
    state.edge_defaults.privacy = fte_types::PrivacyPolicy::HostedAllowed;
    state.edge_defaults.profile = fte_types::RouteProfile::Auto;
    state
}

fn public_response_value(text: &str, streaming: bool) -> Value {
    if !streaming {
        return serde_json::from_str(text).expect("public JSON response");
    }
    let events = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .collect::<Vec<_>>();
    let completed = events
        .iter()
        .filter(|value| value["type"] == "response.completed")
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 1, "{text}");
    let response = completed[0]["response"].clone();
    let created = events
        .iter()
        .find(|value| value["type"] == "response.created")
        .expect("local created identity");
    assert_eq!(created["response"]["id"], response["id"]);
    response
}

async fn continuation_round_trip(streaming: bool, reopen: bool) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider listener");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("provider address")
    );
    let captured = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let provider_capture = Arc::clone(&captured);
    let provider = axum::Router::new().fallback(move |Json(body): Json<Value>| {
        let captured = Arc::clone(&provider_capture);
        async move {
            let turn = {
                let mut requests = captured.lock().expect("actual provider requests");
                requests.push(body.clone()); requests.len()
            };
            let response = json!({"id":format!("upstream_{turn}"),"model":"fixture-model","status":"completed","output":[{"type":"message","id":format!("msg_{turn}"),"role":"assistant","content":[{"type":"output_text","text":format!("turn {turn}")}]}],"usage":{"input_tokens":4,"output_tokens":2}});
            if body["stream"] == true {
                ([(header::CONTENT_TYPE,"text/event-stream")], sse(&[json!({"type":"response.completed","response":response})]))
            } else {
                ([(header::CONTENT_TYPE,"application/json")], response.to_string().into_bytes())
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
            .expect("provider fixture");
    });
    let directory = std::env::temp_dir().join(format!(
        "fte-continuation-{}",
        fte_types::RequestId::new().0
    ));
    std::fs::create_dir(&directory).expect("owned store directory");
    let path = directory.join("responses.sqlite");
    let store = Arc::new(SqliteStore::open(&path).expect("fresh response store"));
    let state = continuation_state(&url, "responses", store);
    let first = router(state.clone(), 8192)
        .oneshot(tests::http_request(
            "/v1/responses",
            json!({"model":"fixture-model","input":"first","store":true,"stream":streaming}),
        ))
        .await
        .expect("first public response");
    assert_eq!(first.status(), StatusCode::OK);
    let first = public_response_value(&tests::response_body(first).await, streaming);
    let local_id = first["id"].as_str().expect("local public ID").to_owned();
    assert_ne!(local_id, "upstream_1");
    assert!(first.get("provider_response_id").is_none());
    let state = if reopen {
        state
            .gateway
            .shutdown()
            .await
            .expect("first gateway drained");
        drop(state);
        // Reusing the same route IDs with a Chat adapter does not authorize
        // forwarding an existing Responses handle to that protocol.
        let chat = continuation_state(
            &url,
            "chat",
            Arc::new(SqliteStore::open(&path).expect("store for changed adapter")),
        );
        let rejected = router(chat.clone(), 8192).oneshot(tests::http_request("/v1/responses", json!({"model":"fixture-model","input":"changed adapter","store":false,"previous_response_id":local_id}))).await.expect("changed adapter refusal");
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        let error: Value =
            serde_json::from_str(&tests::response_body(rejected).await).expect("adapter error");
        assert_eq!(error["error"]["code"], "provider_continuation_unsupported");
        assert_eq!(
            captured.lock().expect("no changed adapter dispatch").len(),
            1
        );
        chat.gateway
            .shutdown()
            .await
            .expect("changed adapter drained");
        drop(chat);
        continuation_state(
            &url,
            "responses",
            Arc::new(SqliteStore::open(&path).expect("current response store reopened")),
        )
    } else {
        state
    };
    let second = router(state.clone(), 8192).oneshot(tests::http_request("/v1/responses", json!({"model":"fixture-model","input":"second","store":true,"previous_response_id":local_id,"stream":streaming}))).await.expect("second public response");
    assert_eq!(second.status(), StatusCode::OK);
    let second = public_response_value(&tests::response_body(second).await, streaming);
    assert_eq!(second["previous_response_id"], local_id);
    assert_ne!(second["id"], "upstream_2");
    assert!(second.get("provider_response_id").is_none());
    {
        let requests = captured.lock().expect("captured two turns");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].get("previous_response_id").is_none());
        assert_eq!(requests[1]["previous_response_id"], "upstream_1");
        assert_eq!(requests[1]["model"], "fixture-model");
    }
    // IDs from provider traffic are never local continuation authority.
    for body in [
        json!({"model":"fixture-model","input":"unknown","store":false,"previous_response_id":"upstream_1"}),
        json!({"model":"other-model","input":"route drift","store":false,"previous_response_id":local_id}),
        json!({"model":"fixture-model","input":"forged","store":false,"provider_previous_response_id":"upstream_1"}),
    ] {
        let result = router(state.clone(), 8192)
            .oneshot(tests::http_request("/v1/responses", body))
            .await
            .expect("rejected public continuation");
        assert!(!result.status().is_success());
        let _ = tests::response_body(result).await;
        assert_eq!(captured.lock().expect("no rejected dispatch").len(), 2);
    }
    // Even if another current stored record carries the same local ID,
    // its route cannot replace the already witnessed producing owner.
    let original = state
        .store
        .get(&local_id)
        .expect("stored owner")
        .expect("retained response");
    for backend in [false, true] {
        let mut wrong = original.clone();
        if backend {
            wrong.route.backend_id = "wrong-backend".into();
        } else {
            wrong.route.model_id = "other-model".into();
        }
        state.store.put(&wrong).expect("wrong-owner fixture");
        let response = router(state.clone(), 8192).oneshot(tests::http_request("/v1/responses", json!({"model":"auto","input":"changed owner","store":false,"previous_response_id":local_id}))).await.expect("source identity refusal");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let value: Value =
            serde_json::from_str(&tests::response_body(response).await).expect("source error");
        assert_eq!(value["error"]["code"], "previous_response_source_mismatch");
        assert_eq!(captured.lock().expect("no wrong-owner dispatch").len(), 2);
    }
    state
        .store
        .put(&original)
        .expect("restore exact fixture owner");
    // Retained storage deletion must invalidate the still-cached handle.
    assert!(
        state
            .store
            .delete(&local_id)
            .expect("delete retained fixture")
    );
    let deleted = router(state.clone(), 8192).oneshot(tests::http_request("/v1/responses", json!({"model":"fixture-model","input":"deleted","store":false,"previous_response_id":local_id}))).await.expect("deleted continuation refusal");
    assert_eq!(deleted.status(), StatusCode::BAD_REQUEST);
    let value: Value =
        serde_json::from_str(&tests::response_body(deleted).await).expect("deleted error");
    assert_eq!(value["error"]["code"], "previous_response_unavailable");
    assert_eq!(captured.lock().expect("no deleted dispatch").len(), 2);
    state.gateway.shutdown().await.expect("gateway drained");
    drop(state);
    stop.send(()).expect("stop fixture");
    server.await.expect("fixture joined");
    std::fs::remove_dir_all(directory).expect("remove owned store files");
}

#[tokio::test]
async fn hosted_continuation_json_in_process() {
    continuation_round_trip(false, false).await;
}

#[tokio::test]
async fn hosted_continuation_json_after_reopen() {
    continuation_round_trip(false, true).await;
}

#[tokio::test]
async fn hosted_continuation_stream_in_process() {
    continuation_round_trip(true, false).await;
}

#[tokio::test]
async fn hosted_continuation_stream_after_reopen() {
    continuation_round_trip(true, true).await;
}

#[tokio::test]
async fn hosted_continuation_refuses_missing_and_chat_handles_before_dispatch() {
    for protocol in ["responses", "chat"] {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("provider listener");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("provider address")
        );
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider_calls = Arc::clone(&calls);
        let provider = axum::Router::new().fallback(move || {
            let calls = Arc::clone(&provider_calls);
            async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                // A Chat completion ID must never become a Responses handle.
                Json(if protocol == "chat" {
                    json!({"id":"chatcmpl_upstream","choices":[{"index":0,"message":{"content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":4,"completion_tokens":2}})
                } else {
                    json!({"status":"completed","output":[],"usage":{"input_tokens":4,"output_tokens":2}})
                })
            }
        });
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, provider)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .expect("provider fixture");
        });
        let state = continuation_state(
            &url,
            protocol,
            Arc::new(SqliteStore::in_memory().expect("store")),
        );
        let first = router(state.clone(), 8192)
            .oneshot(tests::http_request(
                "/v1/responses",
                json!({"model":"fixture-model","input":"first","store":true}),
            ))
            .await
            .expect("first public response");
        assert_eq!(first.status(), StatusCode::OK);
        let first = public_response_value(&tests::response_body(first).await, false);
        let result = router(state.clone(), 8192).oneshot(tests::http_request("/v1/responses", json!({"model":"fixture-model","input":"second","store":false,"previous_response_id":first["id"]}))).await.expect("missing handle refusal");
        assert_eq!(result.status(), StatusCode::BAD_REQUEST);
        let result: Value =
            serde_json::from_str(&tests::response_body(result).await).expect("error response");
        assert_eq!(result["error"]["code"], "previous_response_handle_missing");
        assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 1);
        state.gateway.shutdown().await.expect("gateway drained");
        stop.send(()).expect("stop fixture");
        server.await.expect("fixture joined");
    }
}

#[tokio::test]
async fn native_continuation_requires_explicit_items_without_a_provider_handle() {
    let state = tests::regression_state(Arc::new(SqliteStore::in_memory().expect("native store")));
    let first = router(state.clone(), 8192)
        .oneshot(tests::http_request(
            "/v1/responses",
            json!({"model":"test-model","input":"first","store":true}),
        ))
        .await
        .expect("native first response");
    assert_eq!(first.status(), StatusCode::OK);
    let first = public_response_value(&tests::response_body(first).await, false);
    let result = router(state.clone(), 8192).oneshot(tests::http_request("/v1/responses", json!({"model":"test-model","input":"second","store":false,"previous_response_id":first["id"]}))).await.expect("native continuation refusal");
    assert_eq!(result.status(), StatusCode::BAD_REQUEST);
    let result: Value =
        serde_json::from_str(&tests::response_body(result).await).expect("native error");
    assert_eq!(result["error"]["code"], "previous_response_handle_missing");
    state
        .gateway
        .shutdown()
        .await
        .expect("native gateway drained");
}

async fn require_anthropic_ordinary_replay(content: Value) {
    // These opaque values are protocol sentinels, not provider-issued crypto.
    let provider = json!({
        "content":content,
        "stop_reason":"tool_use",
        "usage":{"input_tokens":4,"output_tokens":3}
    });
    let tools = json!([{"name":"lookup","input_schema":{"type":"object"}}]);
    let user = json!({"role":"user","content":"hello"});
    let (status, text, first_capture) = hosted_exchange_captured(
        "anthropic",
        "application/json",
        provider.to_string().into_bytes(),
        "/v1/messages",
        json!({"model":"fixture-model","max_tokens":16,"messages":[user],"tools":tools}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let response: Value = serde_json::from_str(&text).expect("ordinary Messages response");
    let returned = response["content"].clone();
    let result = json!({
        "role":"user",
        "content":[{"type":"tool_result","tool_use_id":"call_1","content":"found"}]
    });
    let (replay_status, replay_text, replay_capture) = hosted_exchange_captured(
        "anthropic",
        "application/json",
        br#"{"content":[{"type":"text","text":"done"}],"stop_reason":"end_turn","usage":{"input_tokens":9,"output_tokens":1}}"#.to_vec(),
        "/v1/messages",
        json!({
            "model":"fixture-model","max_tokens":16,"tools":tools,
            "messages":[user,{"role":"assistant","content":returned},result]
        }),
    )
    .await;
    assert_eq!(
        replay_status,
        StatusCode::OK,
        "ordinary Anthropic replay: {}",
        json!({
            "first_response_text": &text,
            "first_provider_requests": &first_capture,
            "replay_status": replay_status.as_u16(),
            "replay_response_text": &replay_text,
            "replay_provider_requests": &replay_capture,
        })
    );
    assert_eq!(first_capture.len(), 1);
    assert_eq!(replay_capture.len(), 1);
    assert_eq!(
        response["content"], content,
        "ordinary response lost block state"
    );
    assert_eq!(
        replay_capture[0]["messages"][1]["content"], content,
        "assistant replay changed block order or opaque bytes"
    );
    assert_eq!(
        replay_capture[0]["messages"][2]["content"][0],
        json!({
            "type":"tool_result","tool_use_id":"call_1",
            "content":[{"type":"text","text":"found"}],"is_error":false
        })
    );
    assert_eq!(response["stop_reason"], "tool_use");
    assert_eq!(response["usage"]["input_tokens"], 4);
    assert_eq!(response["usage"]["output_tokens"], 3);
}

#[tokio::test]
async fn hosted_anthropic_ordinary_signed_thinking_replays_unchanged() {
    require_anthropic_ordinary_replay(json!([
        {"type":"thinking","thinking":"Check 日本語.\nKeep order.","signature":"fixture_sig_日本語+A/=="},
        {"type":"text","text":"Looking up."},
        {"type":"tool_use","id":"call_1","name":"lookup","input":{"term":"日本語"}}
    ]))
    .await;
}

#[tokio::test]
async fn hosted_anthropic_ordinary_redacted_and_signed_blocks_replay_in_order() {
    require_anthropic_ordinary_replay(json!([
        {"type":"redacted_thinking","data":"fixture_cipher_日本語/+=one"},
        {"type":"thinking","thinking":"Visible continuation.","signature":"fixture_sig_two/=="},
        {"type":"redacted_thinking","data":"fixture_cipher_two/+=two"},
        {"type":"text","text":"Looking up."},
        {"type":"tool_use","id":"call_1","name":"lookup","input":{"term":"日本語"}}
    ]))
    .await;
}

#[tokio::test]
async fn hosted_anthropic_ordinary_text_and_tool_replay_remains_unchanged() {
    require_anthropic_ordinary_replay(json!([
        {"type":"text","text":"Looking up."},
        {"type":"tool_use","id":"call_1","name":"lookup","input":{"term":"日本語"}}
    ]))
    .await;
}

#[tokio::test]
async fn hosted_anthropic_canonical_reasoning_replays_ordinary_producer_state() {
    use fte_types::{BackendRequest, GatewayBackend, GenerationInput, InputItem, ResolvedRoute};

    // No cryptographic authority is claimed for the fixture signature/data.
    let content = json!([
        {"type":"redacted_thinking","data":"fixture_canonical_cipher_日本語/+=one"},
        {"type":"thinking","thinking":"Check 日本語.\nKeep order.","signature":"fixture_canonical_sig+A/=="},
        {"type":"redacted_thinking","data":"fixture_canonical_cipher_two/+=two"},
        {"type":"tool_use","id":"call_1","name":"lookup","input":{"term":"日本語"}}
    ]);
    let bytes = json!({
        "content":content,"stop_reason":"tool_use",
        "usage":{"input_tokens":4,"output_tokens":3}
    })
    .to_string()
    .into_bytes();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("canonical replay provider listener");
    let endpoint = format!(
        "http://{}",
        listener.local_addr().expect("canonical replay address")
    );
    let captured = Arc::new(Mutex::new(Vec::new()));
    let provider_capture = Arc::clone(&captured);
    let provider = axum::Router::new().fallback(move |Json(body): Json<Value>| {
        let captured = Arc::clone(&provider_capture);
        let bytes = bytes.clone();
        async move {
            captured
                .lock()
                .expect("canonical replay capture")
                .push(body);
            ([(header::CONTENT_TYPE, "application/json")], bytes)
        }
    });
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, provider)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .expect("canonical replay provider");
    });
    let mut config = HostedProviderConfig::anthropic("fixture", "Fixture", "fixture", Vec::new());
    config.endpoints = fte_providers::HostedEndpoints {
        messages: Some(endpoint),
        ..Default::default()
    };
    let backend =
        HostedProviderBackend::new(config, Arc::new(Secrets)).expect("canonical replay backend");
    let initial: AnthropicMessagesRequest = serde_json::from_value(json!({
        "model":"fixture-model","max_tokens":16,
        "messages":[{"role":"user","content":"hello"}],
        "tools":[{"name":"lookup","input_schema":{"type":"object"}}]
    }))
    .expect("ordinary initial request");
    let mut request = initial
        .into_gateway(EdgeDefaults {
            privacy: fte_types::PrivacyPolicy::HostedAllowed,
            profile: fte_types::RouteProfile::Auto,
        })
        .expect("canonical initial request");
    let route = ResolvedRoute {
        backend_id: "fixture".into(),
        model_id: "fixture-model".into(),
        display_name: "Fixture".into(),
        location: BackendLocation::Hosted,
        catalog_version: "fixture".into(),
    };
    let response = backend
        .execute(BackendRequest {
            request: request.clone(),
            route: route.clone(),
            provider_previous_response_id: None,
        })
        .await
        .expect("ordinary producer ticket")
        .final_response()
        .await
        .expect("ordinary producer response");
    // Reconstruct public canonical input from the actual producer's output.
    // No hand-authored replacement for the opaque state is injected.
    let replay: Vec<InputItem> = serde_json::from_value(
        serde_json::to_value(&response.output).expect("canonical output bytes"),
    )
    .expect("canonical replay input");
    let GenerationInput::Chat { items } = &mut request.input else {
        panic!("initial request must remain Chat");
    };
    items.extend(replay);
    items.push(InputItem::FunctionResult {
        id: None,
        call_id: "call_1".into(),
        output: vec![fte_types::ContentBlock::Text {
            text: "found".into(),
        }],
        is_error: false,
    });
    request.request_id = RequestId::new();
    let replayed = backend
        .execute(BackendRequest {
            request: request.clone(),
            route: route.clone(),
            provider_previous_response_id: None,
        })
        .await
        .expect("canonical replay ticket")
        .final_response()
        .await
        .expect("canonical replay response");
    let mut precedence = request.clone();
    precedence.request_id = RequestId::new();
    let GenerationInput::Chat { items } = &mut precedence.input else {
        panic!("canonical replay must remain Chat");
    };
    let mut mixed_signed_state = false;
    for item in items {
        if let InputItem::Reasoning {
            opaque_continuation: Some(Value::Object(opaque)),
            ..
        } = item
            && opaque.contains_key("thinking")
        {
            opaque.insert("redacted_thinking".into(), json!("fixture_mixed_ignored"));
            mixed_signed_state = true;
        }
    }
    assert!(
        mixed_signed_state,
        "actual producer must supply signed state"
    );
    backend
        .execute(BackendRequest {
            request: precedence,
            route: route.clone(),
            provider_previous_response_id: None,
        })
        .await
        .expect("existing mixed-state signed precedence")
        .final_response()
        .await
        .expect("mixed-state response");
    let mut refusal_codes = Vec::new();
    for (item, code) in [
        (
            InputItem::Reasoning {
                id: None,
                summary: vec!["unsigned".into()],
                opaque_continuation: None,
            },
            "anthropic_reasoning_continuity_missing",
        ),
        (
            InputItem::Reasoning {
                id: None,
                summary: vec!["not a native redacted summary".into()],
                opaque_continuation: Some(json!({"redacted_thinking":"fixture_refused"})),
            },
            "anthropic_reasoning_continuity_missing",
        ),
        (
            InputItem::ProviderOpaque {
                provider: "gemini".into(),
                item: json!({"role":"assistant","content":[]}),
            },
            "provider_state_incompatible",
        ),
    ] {
        let mut refused = request.clone();
        refused.request_id = RequestId::new();
        refused.input = GenerationInput::Chat { items: vec![item] };
        let error = match backend
            .execute(BackendRequest {
                request: refused,
                route: route.clone(),
                provider_previous_response_id: None,
            })
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("unsupported continuation must refuse before dispatch"),
        };
        refusal_codes.push((error.code, code));
    }
    backend.shutdown().await.expect("canonical replay drain");
    stop.send(()).expect("canonical provider shutdown");
    server.await.expect("canonical provider joined");
    let captured = captured.lock().expect("canonical replay capture").clone();
    assert_eq!(captured.len(), 3);
    for (actual, expected) in refusal_codes {
        assert_eq!(actual, expected);
    }
    assert_eq!(
        captured[1]["messages"][1]["content"], content,
        "canonical replay lost producer-created reasoning or changed block order"
    );
    assert_eq!(
        captured[1]["messages"][2]["content"][0]["tool_use_id"],
        "call_1"
    );
    assert_eq!(
        captured[2]["messages"][1]["content"]
            .as_array()
            .expect("mixed-state assistant blocks")
            .iter()
            .find(|block| block["type"] == "thinking"),
        Some(&content[1]),
        "existing signed branch must retain precedence over extra redacted state"
    );
    assert_eq!(replayed.usage.input_tokens, Some(4));
    assert_eq!(replayed.usage.output_tokens, Some(3));
    assert_eq!(replayed.output.len(), response.output.len());
    assert!(matches!(
        &response.output[0],
        fte_types::OutputItem::Reasoning {
            summary,
            opaque_continuation: Some(opaque),
            ..
        } if summary.is_empty()
            && opaque["redacted_thinking"] == "fixture_canonical_cipher_日本語/+=one"
    ));
}

#[tokio::test]
async fn hosted_chat_refusal_history_roundtrips_without_losing_designation() {
    let ordinary = json!({"choices":[{"index":0,"message":{"role":"assistant","content":"Ordinary 日本語."},"finish_reason":"stop"}]});
    let (status, text, captured) = hosted_exchange_captured(
        "chat",
        "application/json",
        ordinary.to_string().into_bytes(),
        "/v1/chat/completions",
        chat_request(false),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let ordinary_response: Value = serde_json::from_str(&text).expect("ordinary response");
    let ordinary_message = ordinary_response["choices"][0]["message"].clone();
    assert_eq!(ordinary_message, ordinary["choices"][0]["message"]);
    assert_eq!(captured.len(), 1);
    let ordinary_history = json!({"model":"fixture-model","messages":[
        {"role":"user","content":"hello"}, ordinary_message, {"role":"user","content":"follow up"}
    ]});
    let (status, text, captured) = hosted_exchange_captured(
        "chat",
        "application/json",
        ordinary.to_string().into_bytes(),
        "/v1/chat/completions",
        ordinary_history,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        captured[0]["messages"][1],
        json!({
            "role":"assistant","content":[{"type":"text","text":"Ordinary 日本語."}]
        })
    );

    let fixture: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/chat-refusal.json"))
            .expect("shared provider/UI refusal fixture");
    for refusal in [
        fixture["choices"][0]["message"]["refusal"]
            .as_str()
            .expect("refusal"),
        "",
    ] {
        let mut provider = fixture.clone();
        provider["choices"][0]["message"]["refusal"] = json!(refusal);
        let (first_status, first_text, first_capture) = hosted_exchange_captured(
            "chat",
            "application/json",
            provider.to_string().into_bytes(),
            "/v1/chat/completions",
            chat_request(false),
        )
        .await;
        assert_eq!(first_status, StatusCode::OK, "{first_text}");
        let first: Value =
            serde_json::from_str(&first_text).expect("real hosted refusal projection");
        let returned = first["choices"][0]["message"].clone();
        assert_eq!(returned, provider["choices"][0]["message"]);
        assert_eq!(first["choices"][0]["finish_reason"], "stop");
        assert_eq!(first_capture.len(), 1);
        let history = json!({"model":"fixture-model","messages":[
            {"role":"user","content":"hello"}, returned, {"role":"user","content":"follow up"}
        ]});
        let (replay_status, replay_text, replay_capture) = hosted_exchange_captured(
            "chat",
            "application/json",
            ordinary.to_string().into_bytes(),
            "/v1/chat/completions",
            history.clone(),
        )
        .await;
        assert_eq!(
            replay_status,
            StatusCode::OK,
            "refusal history: {}",
            json!({
                "first_response":first_text,"first_provider_requests":first_capture,
                "replay_status":replay_status.as_u16(),"replay_response":replay_text,
                "replay_provider_requests":replay_capture
            })
        );
        assert_eq!(replay_capture.len(), 1);
        assert_eq!(replay_capture[0]["messages"][1], returned);
        let canonical = serde_json::from_value::<fte_protocols::OpenAiChatRequest>(history.clone())
            .expect("strict public Chat history")
            .into_gateway(fte_protocols::EdgeDefaults::default())
            .expect("typed refusal history");
        assert_eq!(
            serde_json::to_value(&canonical.input).expect("canonical history")["items"][1],
            json!({
                "type":"message","id":null,"role":"assistant",
                "content":[{"type":"refusal","text":refusal}]
            })
        );
        for (protocol, code) in [
            ("responses", "responses_refusal_history_unsupported"),
            ("anthropic", "refusal_history_unsupported"),
            ("gemini", "refusal_history_unsupported"),
        ] {
            let (status, text, captured) = hosted_exchange_captured(
                protocol,
                "application/json",
                b"{}".to_vec(),
                "/v1/chat/completions",
                history.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{protocol}: {text}");
            let error: Value = serde_json::from_str(&text).expect("typed unsupported history");
            assert_eq!(error["error"]["code"], code);
            assert!(
                captured.is_empty(),
                "unsupported history dispatched: {captured:?}"
            );
        }
    }
}

#[tokio::test]
async fn chat_refusal_history_preserves_ordinary_controls_and_refuses_unsupported_shapes() {
    let answer = br#"{"choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}]}"#.to_vec();
    let tool = json!({"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"term\":\"日本語\"}"}});
    for message in [
        json!({"role":"assistant","content":"ordinary","refusal":null}),
        json!({"role":"assistant","content":null}),
        json!({"role":"assistant","content":null,"tool_calls":[tool.clone()]}),
    ] {
        let (status, text, captured) = hosted_exchange_captured(
            "chat",
            "application/json",
            answer.clone(),
            "/v1/chat/completions",
            json!({"model":"fixture-model","messages":[message.clone()]}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "ordinary history: {message}: {text}"
        );
        assert_eq!(captured.len(), 1);
        if message.get("tool_calls").is_some() {
            assert_eq!(
                captured[0]["messages"][0]["tool_calls"],
                json!([tool.clone()])
            );
        } else if message["content"].is_null() {
            assert_eq!(captured[0]["messages"][0]["content"], json!([]));
        } else {
            assert_eq!(
                captured[0]["messages"][0]["content"],
                json!([{"type":"text","text":"ordinary"}])
            );
        }
        assert!(captured[0]["messages"][0].get("refusal").is_none());
    }
    for (message, expected) in [
        (
            json!({"role":"user","content":null,"refusal":"no"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"role":"tool","tool_call_id":"call_1","content":null,"refusal":"no"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"role":"assistant","content":"ordinary","refusal":"no"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"role":"assistant","content":null,"refusal":"no","tool_calls":[tool]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"role":"assistant","content":[{"type":"refusal","refusal":"no"}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"role":"assistant","content":[{"type":"refusal","refusal":"one"},{"type":"refusal","refusal":"two"}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"role":"assistant","content":null,"refusal":7}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"role":"assistant","content":null,"refusal":"no","unsupported":true}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"role":"assistant","content":null,"refusal":"no","encrypted_content":"must not vanish"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, text, captured) = hosted_exchange_captured(
            "chat",
            "application/json",
            answer.clone(),
            "/v1/chat/completions",
            json!({"model":"fixture-model","messages":[message.clone()]}),
        )
        .await;
        assert_eq!(status, expected, "unsupported history: {message}: {text}");
        assert!(captured.is_empty());
    }
    let prompt = "\n  raw tail\t";
    let (status, text, captured) = hosted_exchange_captured(
        "chat",
        "application/json",
        br#"{"choices":[{"index":0,"text":" continuation\n","finish_reason":"stop"}]}"#.to_vec(),
        "/v1/completions",
        json!({"model":"fixture-model","prompt":prompt}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(captured[0]["prompt"], prompt);
    let completion: Value = serde_json::from_str(&text).expect("raw completion");
    assert_eq!(completion["choices"][0]["text"], " continuation\n");
    let (status, text, captured) = hosted_exchange_captured(
        "chat", "application/json", answer, "/v1/completions",
        json!({"model":"fixture-model","prompt":prompt,"messages":[{"role":"assistant","refusal":"no"}]}),
    ).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{text}");
    assert!(captured.is_empty());
}

#[tokio::test]
async fn hosted_gemini_count_keeps_plain_contents_and_wraps_system_context() {
    let ordinary = json!({
        "model":"fixture-model",
        "messages":[{"role":"user","content":"Count this ordinary prompt."}]
    });
    let (status, text, captured) = hosted_exchange_captured(
        "gemini",
        "application/json",
        Vec::new(),
        "/v1/messages/count_tokens",
        ordinary.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("count result")["input_tokens"],
        4
    );
    let contents = json!([{"role":"user","parts":[{"text":"Count this ordinary prompt."}]}]);
    assert_eq!(captured, vec![json!({"contents":contents})]);

    let mut with_system = ordinary;
    with_system["system"] = json!("系統 instruction ∆");
    let (status, text, captured) = hosted_exchange_captured(
        "gemini",
        "application/json",
        Vec::new(),
        "/v1/messages/count_tokens",
        with_system,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("count result")["input_tokens"],
        4
    );
    assert_eq!(
        captured,
        vec![json!({
            "generateContentRequest":{
                "model":"models/fixture-model",
                "contents":contents,
                "systemInstruction":{"parts":[{"text":"系統 instruction ∆"}]}
            }
        })],
        "only the documented nested envelope may carry system context"
    );
}

#[tokio::test]
async fn hosted_gemini_count_keeps_plain_contents_and_wraps_tool_context() {
    let ordinary = json!({
        "model":"fixture-model",
        "messages":[{"role":"user","content":"Count this ordinary prompt."}]
    });
    let (status, text, captured) = hosted_exchange_captured(
        "gemini",
        "application/json",
        Vec::new(),
        "/v1/messages/count_tokens",
        ordinary.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("count result")["input_tokens"],
        4
    );
    let contents = json!([{"role":"user","parts":[{"text":"Count this ordinary prompt."}]}]);
    assert_eq!(captured, vec![json!({"contents":contents})]);

    let mut with_tools = ordinary;
    with_tools["tools"] = json!([{
        "name":"lookup", "description":"Look up an owned record.",
        "input_schema":{"type":"object","properties":{"name":{"type":"string"}}}
    }]);
    let (status, text, captured) = hosted_exchange_captured(
        "gemini",
        "application/json",
        Vec::new(),
        "/v1/messages/count_tokens",
        with_tools,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("count result")["input_tokens"],
        4
    );
    assert_eq!(
        captured,
        vec![json!({
            "generateContentRequest":{
                "model":"models/fixture-model",
                "contents":contents,
                "tools":[{"functionDeclarations":[{
                    "name":"lookup", "description":"Look up an owned record.",
                    "parameters":{"type":"object","properties":{"name":{"type":"string"}}}
                }]}]
            }
        })],
        "only the documented nested envelope may carry function declarations"
    );
}

#[tokio::test]
async fn hosted_anthropic_count_preserves_direct_thinking_and_strict_admission() {
    let plain = json!({
        "model":"fixture-model",
        "messages":[{"role":"user","content":"count me"}]
    });
    let (status, text, captured) = hosted_exchange_captured(
        "anthropic",
        "application/json",
        b"{}".to_vec(),
        "/v1/messages/count_tokens",
        plain.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        serde_json::from_str::<Value>(&text).expect("count response")["input_tokens"],
        4
    );
    assert_eq!(captured.len(), 1);
    let plain_count = captured[0].clone();
    assert_eq!(
        plain_count,
        json!({
            "model":"fixture-model",
            "messages":[{"role":"user","content":[{"type":"text","text":"count me"}]}]
        })
    );
    for field in ["tool_choice", "disable_parallel_tool_use"] {
        let mut unsupported = plain.clone();
        unsupported[field] = if field == "tool_choice" {
            json!({"type":"auto"})
        } else {
            json!(true)
        };
        let (status, text, captured) = hosted_exchange_captured(
            "anthropic",
            "application/json",
            b"{}".to_vec(),
            "/v1/messages/count_tokens",
            unsupported,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}: {text}");
        assert!(
            captured.is_empty(),
            "unsupported direct-count field was dispatched: {captured:?}"
        );
    }
    for thinking in [
        json!({"type":"enabled","budget_tokens":1024}),
        json!({"type":"adaptive"}),
        json!({"type":"disabled"}),
    ] {
        let mut body = plain.clone();
        body["thinking"] = thinking.clone();
        let (status, text, captured) = hosted_exchange_captured(
            "anthropic",
            "application/json",
            b"{}".to_vec(),
            "/v1/messages/count_tokens",
            body,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{thinking}: {text}");
        let mut expected = plain_count.clone();
        expected["thinking"] = thinking;
        assert_eq!(
            captured,
            vec![expected],
            "direct count lost admitted thinking: {text}"
        );
    }
}

#[tokio::test]
async fn hosted_anthropic_count_preflight_keeps_generation_tools_and_choice() {
    let provider = json!({
        "content":[{"type":"text","text":"answer"}],"stop_reason":"end_turn",
        "usage":{"input_tokens":4,"output_tokens":1}
    });
    let plain = json!({
        "model":"fixture-model","max_tokens":16,"stream":true,"temperature":0.5,
        "messages":[{"role":"user","content":"hello"}]
    });
    let (status, text, captured) = hosted_exchange_captured(
        "anthropic",
        "application/json",
        provider.to_string().into_bytes(),
        "/v1/messages",
        plain.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(
        captured.len(),
        2,
        "ordinary count and generation: {captured:?}"
    );
    assert_eq!(captured[0]["messages"], captured[1]["messages"]);
    assert!(captured[0].get("tool_choice").is_none());
    for field in ["max_tokens", "stream", "temperature"] {
        assert!(captured[0].get(field).is_none(), "count included {field}");
        assert_eq!(captured[1][field], plain[field]);
    }
    for choice in [
        None,
        Some(json!({"type":"none"})),
        Some(json!({"type":"auto","disable_parallel_tool_use":false})),
        Some(json!({"type":"any","disable_parallel_tool_use":true})),
        Some(json!({"type":"tool","name":"lookup","disable_parallel_tool_use":false})),
    ] {
        let expected = choice.clone().unwrap_or_else(|| json!({"type":"auto"}));
        let mut body = plain.clone();
        body["tools"] = json!([{"name":"lookup","input_schema":{"type":"object"}}]);
        if let Some(choice) = choice {
            body["tool_choice"] = choice;
        }
        let (status, text, captured) = hosted_exchange_captured(
            "anthropic",
            "application/json",
            provider.to_string().into_bytes(),
            "/v1/messages",
            body,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{expected}: {text}");
        assert_eq!(captured.len(), 2, "count and generation: {captured:?}");
        assert_eq!(
            captured[1]["tool_choice"], expected,
            "generation choice: {captured:?}"
        );
        assert_eq!(
            captured[0]["tool_choice"], expected,
            "count choice: {captured:?}"
        );
        let mut count_projection = captured[1].clone();
        let fields = count_projection.as_object_mut().expect("generation body");
        for field in ["max_tokens", "stream", "temperature"] {
            fields.remove(field);
        }
        assert_eq!(
            captured[0], count_projection,
            "count and generation prompt diverged: {text}"
        );
    }
}

// C-GEMINI-STATE: these are fixed loopback transport fixtures, not model acceptance.
async fn gemini_signed_state_result(
    gateway: &Gateway,
    request: fte_types::GatewayRequest,
) -> (Vec<GatewayEvent>, Result<GatewayResponse, GatewayError>) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut ticket = gateway
            .execute(request)
            .await
            .expect("admitted fixture request");
        let mut events = Vec::new();
        while let Some(event) = ticket.events.recv().await {
            let terminal = event.is_terminal();
            events.push(event);
            if terminal {
                break;
            }
        }
        (events, ticket.final_response().await)
    })
    .await
    .expect("bounded public gateway result; timeout is not a signature witness")
}

async fn gemini_signed_state_round_trip(sse_response: bool, stream_request: bool) {
    use fte_types::{InputItem, ModelSelector, ResponseStore};
    use std::collections::VecDeque;
    use std::time::Duration;

    let ordinary = json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"ordinary café"}]},"finishReason":"STOP"}]});
    let calls = json!({"candidates":[{"index":0,"content":{"role":"model","parts":[
        {"functionCall":{"id":"call-paris","name":"weather","args":{"city":"Paris","index":999}}},
        {"functionCall":{"id":"call-london","name":"weather","args":{"city":"London","index":7}}}
    ]},"finishReason":"STOP"}]});
    let replies = Arc::new(std::sync::Mutex::new(VecDeque::<Vec<Value>>::new()));
    let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, Value)>::new()));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("owned fixture listener");
    let url = format!("http://{}", listener.local_addr().expect("fixture address"));
    let provider_replies = Arc::clone(&replies);
    let provider_capture = Arc::clone(&captured);
    let fallback = ordinary.clone();
    let provider =
        axum::Router::new().fallback(move |uri: axum::http::Uri, Json(body): Json<Value>| {
            let replies = Arc::clone(&provider_replies);
            let captured = Arc::clone(&provider_capture);
            let fallback = fallback.clone();
            async move {
                captured
                    .lock()
                    .expect("captured requests")
                    .push((uri.to_string(), body));
                let frames = replies
                    .lock()
                    .expect("fixed responses")
                    .pop_front()
                    .unwrap_or_else(|| vec![fallback]);
                let bytes = if sse_response {
                    // Separate SSE events remain separate even when TCP coalesces writes.
                    frames
                        .iter()
                        .map(|frame| format!("data: {frame}\r\n\r\n"))
                        .collect::<String>()
                        .into_bytes()
                } else {
                    assert_eq!(frames.len(), 1, "JSON fixture is one complete response");
                    frames[0].to_string().into_bytes()
                };
                let chunks = async_stream::stream! {
                    // Deliberately split UTF-8, field names and CRLF in the server body.
                    // This does not assert reqwest's eventual network chunk boundaries.
                    for byte in bytes {
                        tokio::task::yield_now().await;
                        yield Ok::<_, std::convert::Infallible>(vec![byte]);
                    }
                };
                Response::builder()
                    .header(
                        header::CONTENT_TYPE,
                        if sse_response {
                            "text/event-stream"
                        } else {
                            "application/json"
                        },
                    )
                    .body(Body::from_stream(chunks))
                    .expect("fixed fragmented provider body")
            }
        });
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, provider)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .expect("owned provider fixture");
    });
    let directory = std::env::temp_dir().join(format!(
        "fte-gemini-signed-state-{}",
        fte_types::RequestId::new().0
    ));
    std::fs::create_dir(&directory).expect("fresh owned fixture directory");
    let path = directory.join("responses.sqlite");
    let make_gateway = |store: Arc<SqliteStore>| {
        let gateway = Gateway::new(fte_router::GatewayDefaults::default());
        gateway
            .bind_response_store(store)
            .expect("one current store owner");
        let mut config = HostedProviderConfig::gemini(
            "fixture",
            "Fixture",
            "fixture",
            vec![fixture_model("fixture")],
        );
        config.endpoints.messages = Some(url.clone());
        config.endpoints.count_tokens = None;
        config.connect_timeout = Duration::from_secs(2);
        config.request_timeout = Duration::from_secs(3);
        gateway
            .register_backend(Arc::new(
                HostedProviderBackend::new(config, Arc::new(Secrets)).expect("real hosted decoder"),
            ))
            .expect("exact fixture route");
        gateway
    };
    let decode = |body: Value| {
        let wire: fte_protocols::OpenAiResponsesRequest =
            serde_json::from_value(body).expect("public Responses request decoder");
        let mut request = wire
            .into_gateway(fte_protocols::EdgeDefaults {
                privacy: fte_types::PrivacyPolicy::HostedAllowed,
                profile: fte_types::RouteProfile::Auto,
            })
            .expect("public canonical request");
        // HTTP model names are ExactModel, not a fabricated backend/model syntax.
        // The embedding caller explicitly chooses an exact route for this journey.
        request.model = ModelSelector::ExactRoute {
            backend_id: "fixture".into(),
            model_id: "fixture-model".into(),
        };
        request.deadline.total_ms = Some(4_000);
        request
    };
    let tools_body = |input: Value| {
        json!({
            "model":"fixture-model", "input":input, "store":true, "stream":stream_request,
            "tools":[{"type":"function","name":"weather","parameters":{"type":"object"}}],
            "tool_choice":{"type":"function","name":"weather"}
        })
    };
    let store = Arc::new(SqliteStore::open(&path).expect("current fresh schema"));
    let gateway = make_gateway(Arc::clone(&store));

    // Ordinary success is an actual prerequisite, not a hand-built canonical oracle.
    replies
        .lock()
        .expect("reply queue")
        .push_back(vec![ordinary.clone()]);
    let (events, result) = gemini_signed_state_result(
        &gateway,
        decode(json!({
            "model":"fixture-model","input":"ordinary","store":true,"stream":stream_request
        })),
    )
    .await;
    let plain = result.expect("ordinary unsigned generation must work first");
    assert_eq!(
        fte_protocols::openai_responses_json(&plain)["output"][0]["content"][0]["text"],
        "ordinary café"
    );
    assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
    assert_eq!(
        store.get(&plain.id).expect("ordinary stored").as_ref(),
        Some(&plain)
    );

    // Unsigned tool designation, IDs, argument meaning, order and client authority.
    replies
        .lock()
        .expect("reply queue")
        .push_back(vec![calls.clone()]);
    let tool_request = decode(tools_body(json!("tools")));
    assert_eq!(
        tool_request.tool_policy.selection,
        fte_types::ToolSelection::Named {
            name: "weather".into()
        }
    );
    assert!(matches!(
        tool_request.tool_policy.execution,
        fte_types::ToolExecutionPolicy::ClientOnly
    ));
    let (_, result) = gemini_signed_state_result(&gateway, tool_request).await;
    let first = result.expect("ordinary unsigned tools must work before signed cases");
    assert_eq!(first.output.len(), 2);
    assert_eq!(first.output_groups.len(), 1);
    assert_eq!(first.output_groups[0].output_indices, vec![0, 1]);
    assert_eq!(
        first.output_groups[0].finish_reason,
        fte_types::FinishReason::ToolCalls
    );
    for (item, (id, city, index)) in first
        .output
        .iter()
        .zip([("call-paris", "Paris", 999), ("call-london", "London", 7)])
    {
        match item {
            fte_types::OutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                assert_eq!(call_id, id);
                assert_eq!(name, "weather");
                assert_eq!(arguments, &json!({"city":city,"index":index}));
            }
            _ => panic!("function designation was replaced by another output kind"),
        }
    }
    let canonical = serde_json::to_vec(&first).expect("canonical serialization");
    let decoded: GatewayResponse =
        serde_json::from_slice(&canonical).expect("current canonical reader");
    assert_eq!(decoded, first);
    assert_eq!(
        store.get(&first.id).expect("stored tools").as_ref(),
        Some(&first)
    );
    gateway
        .shutdown()
        .await
        .expect("first gateway fully drained");
    drop(gateway);
    drop(store);

    // Real close/reopen of the current database: no migration or alternate reader.
    let store = Arc::new(SqliteStore::open(&path).expect("reopen the same current schema"));
    let reopened = store
        .get(&first.id)
        .expect("reopened lookup")
        .expect("retained tool response");
    assert_eq!(
        serde_json::to_vec(&reopened).expect("reopened canonical bytes"),
        canonical
    );
    assert_eq!(
        store.get(&plain.id).expect("old ordinary row").as_ref(),
        Some(&plain)
    );
    let gateway = make_gateway(Arc::clone(&store));
    let public = fte_protocols::openai_responses_json(&reopened);
    let mut history = vec![json!({"type":"message","role":"user","content":"tools"})];
    history.extend(
        public["output"]
            .as_array()
            .expect("actual public output")
            .iter()
            .cloned(),
    );
    history.push(
        json!({"type":"function_call_output","call_id":"call-paris","output":"Paris result"}),
    );
    history.push(
        json!({"type":"function_call_output","call_id":"call-london","output":"London result"}),
    );
    let mut replay = decode(tools_body(json!(history)));
    replay.model = ModelSelector::ExactRoute {
        backend_id: reopened.route.backend_id.clone(),
        model_id: reopened.route.model_id.clone(),
    };
    replies
        .lock()
        .expect("reply queue")
        .push_back(vec![ordinary.clone()]);
    let (_, result) = gemini_signed_state_result(&gateway, replay).await;
    result.expect("explicit unsigned history replays through public decoder after reopen");
    {
        let requests = captured.lock().expect("actual transport requests");
        assert_eq!(requests.len(), 3);
        let expected_path = if stream_request {
            "/models/fixture-model:streamGenerateContent?alt=sse"
        } else {
            "/models/fixture-model:generateContent"
        };
        assert!(requests.iter().all(|(path, _)| path == expected_path));
        let request = &requests[2].1;
        assert_eq!(request["contents"][1]["role"], "model");
        assert_eq!(
            request["contents"][1]["parts"],
            calls["candidates"][0]["content"]["parts"]
        );
        assert_eq!(
            request["contents"][2]["parts"][0]["functionResponse"]["id"],
            "call-paris"
        );
        assert_eq!(
            request["contents"][3]["parts"][0]["functionResponse"]["id"],
            "call-london"
        );
        assert_eq!(
            request["toolConfig"]["functionCallingConfig"]["mode"],
            "ANY"
        );
        assert_eq!(
            request["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
            json!(["weather"])
        );
        assert!(request.get("previous_response_id").is_none());
        assert!(!request.to_string().contains("thoughtSignature"));
    }
    let mut previous = decode(json!({"model":"fixture-model","input":"next","store":true,
        "previous_response_id":first.id}));
    assert_eq!(
        gateway
            .execute(previous.clone())
            .await
            .expect_err("Gemini has no provider handle")
            .code,
        "previous_response_handle_missing"
    );
    previous.storage.previous_response_id = None;
    previous.routing.privacy = fte_types::PrivacyPolicy::LocalOnly;
    assert_eq!(
        gateway
            .execute(previous)
            .await
            .expect_err("privacy still rejects hosted")
            .class,
        fte_types::ErrorClass::Privacy
    );
    let mut foreign = decode(json!({"model":"fixture-model","input":"foreign","store":false}));
    foreign.input = fte_types::GenerationInput::Chat {
        items: vec![InputItem::ProviderOpaque {
            provider: "anthropic".into(),
            item: json!({"role":"model","parts":[{"text":"foreign"}]}),
        }],
    };
    assert_eq!(
        gateway
            .execute(foreign)
            .await
            .expect_err("foreign opaque provider state")
            .code,
        "provider_state_incompatible"
    );
    assert_eq!(
        captured
            .lock()
            .expect("negative controls never dispatch")
            .len(),
        3
    );

    // The signature-looking string inside args is user data, not Part metadata.
    let mut arguments_only = calls.clone();
    arguments_only["candidates"][0]["content"]["parts"][0]["functionCall"]["args"]["thoughtSignature"] =
        json!("argument-data");
    replies
        .lock()
        .expect("reply queue")
        .push_back(vec![arguments_only]);
    let (_, result) =
        gemini_signed_state_result(&gateway, decode(tools_body(json!("arguments")))).await;
    let argument_response = result.expect("signature-named arguments are ordinary unsigned calls");
    match &argument_response.output[0] {
        fte_types::OutputItem::FunctionCall { arguments, .. } => {
            assert_eq!(arguments["thoughtSignature"], "argument-data")
        }
        _ => panic!("argument-data tool must remain a function call"),
    }

    // Unsigned thought summaries remain ordinary output, not invented signed replay.
    replies
        .lock()
        .expect("reply queue")
        .push_back(vec![json!({"candidates":[{
            "index":0,"content":{"role":"model","parts":[
                {"thought":true,"text":"unsigned summary"},{"text":"ordinary answer"}
            ]},"finishReason":"STOP"
        }]})]);
    let (_, result) =
        gemini_signed_state_result(&gateway, decode(tools_body(json!("unsigned thought")))).await;
    let unsigned_thought = result.expect("unsigned thought summaries remain supported");
    assert!(unsigned_thought.output.iter().any(|item| matches!(item,
        fte_types::OutputItem::Reasoning { summary, opaque_continuation: None, .. }
        if summary == &vec!["unsigned summary".to_string()]
    )));

    // The admission and insertion owners still carry selection, not execution.
    for (choice, mode) in [
        (json!("none"), "NONE"),
        (json!("auto"), "AUTO"),
        (json!("required"), "ANY"),
    ] {
        let mut body = tools_body(json!("selection control"));
        body["tool_choice"] = choice;
        replies
            .lock()
            .expect("reply queue")
            .push_back(vec![ordinary.clone()]);
        let request = decode(body);
        assert!(matches!(
            request.tool_policy.execution,
            fte_types::ToolExecutionPolicy::ClientOnly
        ));
        let (_, result) = gemini_signed_state_result(&gateway, request).await;
        result.expect("ordinary selection mode");
        let requests = captured.lock().expect("selection captured");
        let request = &requests.last().expect("selection request").1;
        assert_eq!(request["toolConfig"]["functionCallingConfig"]["mode"], mode);
    }

    // Each case exercises the same public decoder, hosted producer and store owner.
    // Collect failures until after cleanup so Before exposes every signed shape.
    let mut cases = Vec::<(&str, Vec<Value>)>::new();
    for (label, part) in [
        (
            "signed-call",
            json!({"functionCall":{"id":"signed-id","name":"weather","args":{"city":"Paris"}},"thoughtSignature":"c2lnbmVkLXNlbnRpbmVs"}),
        ),
        (
            "signed-nonthought-text",
            json!({"text":"signed text","thoughtSignature":"c2lnbmVkLXNlbnRpbmVs"}),
        ),
        (
            "signed-thought-text",
            json!({"text":"thought summary","thought":true,"thoughtSignature":"c2lnbmVkLXNlbnRpbmVs"}),
        ),
        (
            "signature-only",
            json!({"thoughtSignature":"c2lnbmVkLXNlbnRpbmVs"}),
        ),
        (
            "empty-text-signature",
            json!({"text":"","thoughtSignature":"c2lnbmVkLXNlbnRpbmVs"}),
        ),
        (
            "snake-signature",
            json!({"functionCall":{"name":"weather","args":{}},"thought_signature":"c2lnbmVkLXNlbnRpbmVs"}),
        ),
        (
            "null-signature",
            json!({"functionCall":{"name":"weather","args":{}},"thoughtSignature":null}),
        ),
        (
            "empty-signature",
            json!({"functionCall":{"name":"weather","args":{}},"thoughtSignature":""}),
        ),
    ] {
        cases.push((label, vec![json!({"candidates":[{"index":0,"content":{"role":"model","parts":[part]},"finishReason":"STOP"}]})]));
    }
    let mut mixed = calls.clone();
    mixed["candidates"][0]["content"]["parts"][1]["thoughtSignature"] =
        json!("c2lnbmVkLXNlbnRpbmVs");
    cases.push(("unsigned-sibling-before-signed", vec![mixed]));
    let mut candidates = ordinary.clone();
    candidates["candidates"].as_array_mut().expect("candidate array").push(json!({
        "index":7,"content":{"role":"model","parts":[{"text":"alternate","thoughtSignature":"c2lnbmVkLXNlbnRpbmVs"}]},"finishReason":"STOP"
    }));
    cases.push(("signed-alternate-candidate", vec![candidates]));
    if sse_response {
        cases.push(("late-signature", vec![
            json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"provisional café"}]}}]}),
            json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"","thoughtSignature":"c2lnbmVkLXNlbnRpbmVs"}]},"finishReason":"STOP"}]})
        ]));
        cases.push((
            "late-call-signature",
            vec![
                json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{
                    "functionCall":{"id":"late-call","name":"weather","args":{"city":"Paris"}}
                }]}}]}),
                json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{
                "functionCall":{"id":"late-call","name":"weather","args":{"city":"Paris"}},
                "thoughtSignature":"c2lnbmVkLXNlbnRpbmVs"
            }]},"finishReason":"STOP"}]}),
            ],
        ));
    }
    let mut violations = Vec::new();
    for (label, frames) in cases {
        let before_dispatch = captured.lock().expect("request count").len();
        let producer_parts = frames[0]["candidates"][0]["content"]["parts"].clone();
        replies.lock().expect("reply queue").push_back(frames);
        let request = decode(tools_body(json!(label)));
        let request_id = request.request_id.clone();
        let (events, result) = gemini_signed_state_result(&gateway, request).await;
        let failed = match &result {
            Err(error) => {
                error.class == fte_types::ErrorClass::Provider
                    && error.code == "gemini_signed_part_unsupported"
                    && error.request_id == request_id
                    && error.provider.as_deref() == Some("fixture")
                    && !error.retryable
                    && error.http_status == 502
                    && !error.safe_detail.contains("c2lnbmVkLXNlbnRpbmVs")
            }
            Ok(_) => false,
        };
        if !failed {
            violations.push(format!(
                "{label}: signed output was not refused: {result:?}"
            ));
        }
        if events.iter().filter(|event| event.is_terminal()).count() != 1
            || !matches!(events.last(), Some(GatewayEvent::Failed { error, .. }) if error.code == "gemini_signed_part_unsupported")
        {
            violations.push(format!("{label}: expected exactly one failed terminal"));
        }
        let id = events
            .iter()
            .find_map(|event| match event {
                GatewayEvent::ResponseCreated { response_id, .. } => Some(response_id.as_str()),
                _ => None,
            })
            .expect("public created response identity");
        if store.get(id).expect("failed response lookup").is_some() {
            violations.push(format!("{label}: lossy response reached durable history"));
        }
        if !label.starts_with("late-")
            && events.iter().any(|event| {
                matches!(
                    event,
                    GatewayEvent::OutputItemAdded { .. }
                        | GatewayEvent::TextDelta { .. }
                        | GatewayEvent::ReasoningSummaryDelta { .. }
                        | GatewayEvent::FunctionArgumentsDelta { .. }
                )
            })
        {
            violations.push(format!(
                "{label}: frame was partially normalized before refusal"
            ));
        }
        if label == "late-signature"
            && !events.iter().any(|event| {
                matches!(event,
                    GatewayEvent::TextDelta { delta, .. } if delta == "provisional café"
                )
            })
        {
            violations.push(format!("{label}: missing ordinary provisional progress"));
        }
        if events.iter().any(|event| {
            matches!(
                event,
                GatewayEvent::OutputItemCompleted { .. }
                    | GatewayEvent::ContentPartCompleted { .. }
                    | GatewayEvent::Completed { .. }
            )
        }) {
            violations.push(format!(
                "{label}: refused output became authoritative completion"
            ));
        }
        if label == "late-call-signature" {
            let additions = events.iter().filter(|event| matches!(event,
                GatewayEvent::OutputItemAdded { item: fte_types::OutputItem::FunctionCall { call_id, .. }, .. }
                if call_id == "late-call"
            )).count();
            let arguments = events
                .iter()
                .filter(|event| {
                    matches!(event,
                        GatewayEvent::FunctionArgumentsDelta { delta, .. }
                        if delta == &json!({"city":"Paris"}).to_string()
                    )
                })
                .count();
            if additions != 1 || arguments != 1 {
                violations.push(format!(
                    "{label}: prior call progress missing or signed frame changed it"
                ));
            }
            let mut encoder = fte_protocols::OpenAiResponsesStreamEncoder::default();
            let wire = events
                .iter()
                .filter_map(|event| encoder.encode(event))
                .collect::<Vec<_>>();
            if !wire.iter().any(|event| {
                event.event == "response.output_item.added"
                    && event.data["item"]["call_id"] == "late-call"
                    && event.data["item"]["status"] == "in_progress"
            }) || !wire.last().is_some_and(|event| {
                event.event == "error" && event.data["code"] == "gemini_signed_part_unsupported"
            }) || wire.iter().any(|event| {
                event.event.ends_with(".done")
                    || matches!(
                        event.event.as_str(),
                        "response.completed" | "response.incomplete"
                    )
            }) {
                violations.push(format!(
                    "{label}: public stream accepted provisional call as done"
                ));
            }
        }
        if captured.lock().expect("single dispatch").len() != before_dispatch + 1 {
            violations.push(format!("{label}: unexpected retry or missing dispatch"));
        }
        if label == "signed-call"
            && let Ok(response) = &result
        {
            // Preserve a genuine Before diagnostic through the stored public history.
            // Refused After results never enter this branch or invent a replay Item.
            let retained = store
                .get(&response.id)
                .expect("accepted signed row lookup")
                .expect("accepted signed response stored by the real publication owner");
            assert_eq!(&retained, response);
            let public = fte_protocols::openai_responses_json(&retained);
            let mut history = vec![json!({"type":"message","role":"user","content":"signed-call"})];
            history.extend(
                public["output"]
                    .as_array()
                    .expect("produced public history")
                    .iter()
                    .cloned(),
            );
            history.push(json!({"type":"function_call_output","call_id":"signed-id","output":"Paris result"}));
            replies
                .lock()
                .expect("reply queue")
                .push_back(vec![ordinary.clone()]);
            let (_, replayed) =
                gemini_signed_state_result(&gateway, decode(tools_body(json!(history)))).await;
            replayed.expect("ordinary local response to accepted history replay");
            let requests = captured
                .lock()
                .expect("actual signed history replay capture");
            let replay_parts =
                &requests.last().expect("signed history replay request").1["contents"][1]["parts"];
            violations.push(format!(
                "{label}: accepted producer Parts={producer_parts}; stored canonical={retained:?}; replay Parts={replay_parts}; producer equality={}",
                replay_parts == &producer_parts
            ));
        }
    }
    // Prior successful rows survive every refusal without rewrite or repair.
    assert_eq!(
        store.get(&first.id).expect("retained original").as_ref(),
        Some(&first)
    );
    assert_eq!(
        store.get(&plain.id).expect("retained ordinary").as_ref(),
        Some(&plain)
    );
    gateway
        .shutdown()
        .await
        .expect("final gateway fully drained");
    drop(gateway);
    drop(store);
    let _ = stop.send(());
    match tokio::time::timeout(Duration::from_secs(3), &mut server).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => violations.push(format!("provider fixture join failed: {error}")),
        Err(_) => {
            server.abort();
            let _ = server.await;
            violations.push("provider fixture did not drain within its cleanup bound".into());
        }
    }
    std::fs::remove_dir_all(directory).expect("remove only the owned synthetic store");
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[tokio::test]
async fn gemini_signed_state_json_public_history_and_reopen() {
    gemini_signed_state_round_trip(false, false).await;
}

#[tokio::test]
async fn gemini_signed_state_fragmented_sse_public_history_and_reopen() {
    gemini_signed_state_round_trip(true, true).await;
}

#[tokio::test]
async fn gemini_signed_state_json_fallback_public_history_and_reopen() {
    gemini_signed_state_round_trip(false, true).await;
}
