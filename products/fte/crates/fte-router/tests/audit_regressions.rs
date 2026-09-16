#![forbid(unsafe_code)]

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
    use fte_protocols::{EdgeDefaults, OpenAiChatRequest};
    use fte_providers::{HostedAuth, HostedProviderBackend, HostedProviderConfig};
    use fte_router::{Gateway, GatewayDefaults};
    use fte_store::SecretResolver;
    use fte_types::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::Duration;

    fn descriptor() -> BackendDescriptor {
        BackendDescriptor {
            id: "fixture".into(),
            display_name: "Synthetic fixture".into(),
            location: BackendLocation::Hosted,
            models: vec![ModelDescriptor {
                id: "fixture-model".into(),
                aliases: vec![],
                display_name: "Synthetic model".into(),
                backend_id: "fixture".into(),
                location: BackendLocation::Hosted,
                capabilities: ModelCapabilities {
                    prompt_forms: vec![PromptForm::Chat],
                    modalities: vec![Modality::Text],
                    streaming: true,
                    ..Default::default()
                },
                context_tokens: Some(4096),
                max_output_tokens: Some(512),
                quota: fte_types::QuotaLimits::default(),
                observed: RouteObservations::default(),
            }],
        }
    }

    fn request(model: &str) -> GatewayRequest {
        serde_json::from_value::<OpenAiChatRequest>(json!({
            "model": model,
            "messages": [{"role":"user", "content":"synthetic audit input"}],
            "max_tokens": 8
        }))
        .expect("fixture parse")
        .into_gateway(EdgeDefaults {
            privacy: PrivacyPolicy::HostedAllowed,
            profile: RouteProfile::Auto,
        })
        .expect("fixture conversion")
    }

    struct Probe {
        descriptor: BackendDescriptor,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl GatewayBackend for Probe {
        fn descriptor(&self) -> BackendDescriptor {
            self.descriptor.clone()
        }
        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }
        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(GatewayError::invalid_request(
                &request.request.request_id,
                "audit_backend_reached",
                "synthetic probe reached",
            ))
        }
        fn cancel(&self, _: &RequestId, _: CancelTarget) -> usize {
            0
        }
    }

    async fn dispatch_count(request: GatewayRequest, descriptor: BackendDescriptor) -> usize {
        let calls = Arc::new(AtomicUsize::new(0));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(Arc::new(Probe {
                descriptor,
                calls: calls.clone(),
            }))
            .expect("register probe");
        let result = gateway.execute(request).await;
        assert!(result.is_err(), "probe always returns an error");
        gateway.shutdown().await.expect("shutdown");
        calls.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn explicit_local_privacy_blocks_hosted_baseline() {
        let mut request = request("auto");
        request.routing.privacy = PrivacyPolicy::LocalOnly;
        assert_eq!(dispatch_count(request, descriptor()).await, 0);
    }

    #[tokio::test]
    async fn local_only_model_selector_must_not_dispatch_hosted() {
        assert_eq!(
            dispatch_count(request("local-only"), descriptor()).await,
            0,
            "the local-only model selector dispatched to a hosted backend"
        );
    }

    #[tokio::test]
    async fn exhausted_quota_must_not_dispatch() {
        for limit in [None, Some(10)] {
            let mut descriptor = descriptor();
            descriptor.models[0].observed.quota_headroom = Some(0.0);
            descriptor.models[0].quota.requests_per_minute = limit;
            assert_eq!(
                dispatch_count(request("auto"), descriptor).await,
                0,
                "local limit {limit:?} must not override an observed exhausted quota"
            );
        }
    }

    #[tokio::test]
    async fn image_request_must_not_reach_text_only_backend() {
        let mut request = request("auto");
        request.input = GenerationInput::Chat {
            items: vec![InputItem::Message {
                id: None,
                role: MessageRole::User,
                content: vec![ContentBlock::Image {
                    source: MediaSource::Bytes {
                        mime_type: "image/png".into(),
                        data_base64: "AA==".into(),
                    },
                    detail: None,
                }],
            }],
        };
        assert_eq!(
            dispatch_count(request, descriptor()).await,
            0,
            "the router admitted an image request to a text-only backend"
        );
    }

    struct MutableSecrets(RwLock<Option<String>>);
    impl SecretResolver for MutableSecrets {
        fn resolve(&self, _: &str) -> Result<Option<String>, GatewayError> {
            Ok(self.0.read().expect("fixture secret lock").clone())
        }
    }

    struct Fixture {
        gateway: Gateway,
        backend: Arc<HostedProviderBackend>,
        secrets: Arc<MutableSecrets>,
        observed_headers: Arc<Mutex<Vec<String>>>,
        server: tokio::task::JoinHandle<()>,
    }

    async fn respond(
        State(headers): State<Arc<Mutex<Vec<String>>>>,
        incoming: HeaderMap,
    ) -> Json<Value> {
        headers.lock().expect("fixture header lock").push(
            incoming
                .get("authorization")
                .expect("fixture auth")
                .to_str()
                .expect("fixture auth string")
                .to_owned(),
        );
        Json(json!({
            "id":"fixture-response", "object":"chat.completion", "model":"fixture-model",
            "choices":[{"index":0,"message":{"role":"assistant","content":"synthetic output"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}
        }))
    }

    async fn fixture() -> Fixture {
        fixture_with_limits(QuotaLimits::default()).await
    }

    async fn fixture_with_limits(limits: QuotaLimits) -> Fixture {
        let observed_headers = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local fixture listener");
        let address = listener.local_addr().expect("local address");
        let app = Router::new()
            .route("/chat", post(respond))
            .with_state(observed_headers.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("fixture serve");
        });
        let secrets = Arc::new(MutableSecrets(RwLock::new(Some(
            "synthetic-old-key".into(),
        ))));
        let mut config = HostedProviderConfig::openai_compatible(
            "fixture",
            "Synthetic fixture",
            "fixture-secret",
            format!("http://{address}/chat"),
            descriptor().models,
        );
        config.models[0].quota = limits;
        config.request_timeout = Duration::from_secs(2);
        let backend =
            Arc::new(HostedProviderBackend::new(config, secrets.clone()).expect("hosted adapter"));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway
            .register_backend(backend.clone())
            .expect("register adapter");
        assert!(
            backend.readiness().is_ready(),
            "prime credential via ordinary status read"
        );
        Fixture {
            gateway,
            backend,
            secrets,
            observed_headers,
            server,
        }
    }

    #[tokio::test]
    async fn credential_replacement_must_affect_next_request() {
        let fixture = fixture().await;
        *fixture.secrets.0.write().expect("replace fixture secret") =
            Some("synthetic-new-key".into());
        fixture
            .gateway
            .execute(request("fixture-model"))
            .await
            .expect("execute")
            .final_response()
            .await
            .expect("response");
        fixture.gateway.shutdown().await.expect("shutdown");
        fixture.server.abort();
        let headers = fixture.observed_headers.lock().expect("headers");
        assert_eq!(
            headers.as_slice(),
            ["Bearer synthetic-new-key"],
            "adapter sent the old cached credential after replacement"
        );
    }

    #[tokio::test]
    async fn direct_gateway_persists_before_publishing_completion_and_restores_after_restart() {
        let fixture = fixture().await;
        let directory = tempfile::tempdir().expect("temporary store directory");
        let path = directory.path().join("responses.db");
        let store = Arc::new(fte_store::SqliteStore::open(&path).expect("store"));
        fixture
            .gateway
            .bind_response_store(store.clone())
            .expect("bind store");
        let mut first = request("fixture-model");
        first.storage.store_response = true;
        let mut ticket = fixture.gateway.execute(first).await.expect("execute");
        let mut completed_id = None;
        while let Some(event) = ticket.events.recv().await {
            if let GatewayEvent::Completed { response, .. } = event {
                assert!(store.get(&response.id).expect("stored response").is_some());
                completed_id = Some(response.id);
                break;
            }
        }
        let response = ticket.final_response().await.expect("final response");
        assert_eq!(completed_id.as_deref(), Some(response.id.as_str()));
        fixture.gateway.shutdown().await.expect("shutdown");
        fixture.server.abort();

        // A fresh router has no volatile affinity. Reopening its injected store
        // must pin the continuation before it calls the original route.
        let reopened = Gateway::new(GatewayDefaults::default());
        reopened
            .bind_response_store(Arc::new(
                fte_store::SqliteStore::open(&path).expect("reopen on disk"),
            ))
            .expect("rebind on new router");
        let calls = Arc::new(AtomicUsize::new(0));
        reopened
            .register_backend(Arc::new(Probe {
                descriptor: descriptor(),
                calls: calls.clone(),
            }))
            .expect("register original route");
        let mut next = request("auto");
        next.storage.previous_response_id = Some(response.id);
        let error = reopened.execute(next).await.expect_err("probe error");
        assert_eq!(error.code, "audit_backend_reached");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        reopened.shutdown().await.expect("shutdown reopened");
    }

    struct FailingStore;
    impl ResponseStore for FailingStore {
        fn put(&self, response: &GatewayResponse) -> Result<(), GatewayError> {
            Err(GatewayError::unavailable(
                &response.request_id,
                "fixture_store_failure",
                "synthetic store failure",
            ))
        }
        fn get(&self, _: &str) -> Result<Option<GatewayResponse>, GatewayError> {
            Ok(None)
        }
        fn delete(&self, _: &str) -> Result<bool, GatewayError> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn storage_failure_emits_failure_instead_of_successful_stream_completion() {
        let fixture = fixture().await;
        fixture
            .gateway
            .bind_response_store(Arc::new(FailingStore))
            .expect("bind failing store");
        let mut request = request("fixture-model");
        request.storage.store_response = true;
        let mut ticket = fixture.gateway.execute(request).await.expect("execute");
        let mut terminal_count = 0;
        while let Some(event) = ticket.events.recv().await {
            assert!(!matches!(event, GatewayEvent::Completed { .. }));
            if let GatewayEvent::Failed { error, .. } = event {
                assert_eq!(error.code, "fixture_store_failure");
                terminal_count += 1;
            }
        }
        assert_eq!(terminal_count, 1);
        assert_eq!(
            ticket
                .final_response()
                .await
                .expect_err("storage must fail")
                .code,
            "fixture_store_failure"
        );
        assert_eq!(fixture.gateway.status().active_requests, 0);
        fixture.gateway.shutdown().await.expect("shutdown");
        fixture.server.abort();
    }

    #[tokio::test]
    async fn declared_quota_is_enforced_after_successful_provider_accounting() {
        let fixture = fixture_with_limits(QuotaLimits {
            requests_per_day: Some(1),
            ..Default::default()
        })
        .await;
        fixture
            .gateway
            .execute(request("fixture-model"))
            .await
            .expect("first")
            .final_response()
            .await
            .expect("first result");
        assert!(
            fixture
                .gateway
                .execute(request("fixture-model"))
                .await
                .is_err()
        );
        assert_eq!(fixture.observed_headers.lock().expect("headers").len(), 1);
        assert_eq!(
            fixture.gateway.models()[0].observed.quota_headroom,
            None,
            "local allowance must not become a provider observation"
        );
        fixture.gateway.shutdown().await.expect("shutdown");
        fixture.server.abort();
    }

    #[tokio::test]
    async fn credential_deletion_must_block_next_request() {
        let fixture = fixture().await;
        *fixture.secrets.0.write().expect("delete fixture secret") = None;
        let ready_after_delete = fixture.backend.readiness().is_ready();
        let result = fixture.gateway.execute(request("fixture-model")).await;
        if let Ok(ticket) = result {
            let _ = ticket.final_response().await;
        }
        fixture.gateway.shutdown().await.expect("shutdown");
        fixture.server.abort();
        let sent = fixture.observed_headers.lock().expect("headers").len();
        assert_eq!(
            (ready_after_delete, sent),
            (false, 0),
            "deleting the resolver credential left the route ready and authorized a new HTTP request"
        );
    }

    #[tokio::test]
    async fn custom_api_key_must_not_follow_cross_origin_redirect() {
        let received = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("sink bind");
        let sink_url = format!("http://{}/sink", sink.local_addr().expect("sink address"));
        let sink_received = received.clone();
        let sink_app = Router::new().route(
            "/sink",
            post(move |headers: HeaderMap| {
                let received = sink_received.clone();
                async move {
                    if let Some(key) = headers.get("x-api-key") {
                        received
                            .lock()
                            .expect("header lock")
                            .push(key.to_str().expect("key").into());
                    }
                    Json(json!({"choices":[],"usage":{}}))
                }
            }),
        );
        let sink_task = tokio::spawn(async move {
            axum::serve(sink, sink_app).await.expect("sink serve");
        });
        let redirect = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("redirect bind");
        let source_url = format!(
            "http://{}/chat",
            redirect.local_addr().expect("redirect address")
        );
        let redirect_app = Router::new().route(
            "/chat",
            post(move || {
                let sink_url = sink_url.clone();
                async move { axum::response::Redirect::temporary(&sink_url) }
            }),
        );
        let redirect_task = tokio::spawn(async move {
            axum::serve(redirect, redirect_app)
                .await
                .expect("redirect serve");
        });
        let secrets = Arc::new(MutableSecrets(RwLock::new(Some(
            "synthetic-header-key".into(),
        ))));
        let mut config = HostedProviderConfig::openai_compatible(
            "fixture",
            "Synthetic fixture",
            "fixture-secret",
            source_url,
            descriptor().models,
        );
        config.auth = HostedAuth::Header {
            name: "x-api-key".into(),
            prefix: String::new(),
        };
        config.request_timeout = Duration::from_secs(2);
        let backend = Arc::new(HostedProviderBackend::new(config, secrets).expect("backend"));
        let gateway = Gateway::new(GatewayDefaults::default());
        gateway.register_backend(backend).expect("register");
        if let Ok(ticket) = gateway.execute(request("fixture-model")).await {
            let _ = ticket.final_response().await;
        }
        gateway.shutdown().await.expect("shutdown");
        redirect_task.abort();
        sink_task.abort();
        assert!(
            received.lock().expect("received").is_empty(),
            "custom provider credential reached a different origin via HTTP 307"
        );
    }

    struct BlockingSetup {
        calls: AtomicUsize,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    #[async_trait]
    impl GatewayBackend for BlockingSetup {
        fn descriptor(&self) -> BackendDescriptor {
            descriptor()
        }
        fn readiness(&self) -> BackendReadiness {
            BackendReadiness::Ready
        }
        async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Err(GatewayError::invalid_request(
                &request.request.request_id,
                "audit_setup_done",
                "fixture",
            ))
        }
        fn cancel(&self, _: &RequestId, _: CancelTarget) -> usize {
            0
        }
    }

    #[tokio::test]
    async fn cancelling_queued_request_must_prevent_later_dispatch() {
        let gateway = Arc::new(Gateway::new(GatewayDefaults::default()));
        let backend = Arc::new(BlockingSetup {
            calls: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        gateway.register_backend(backend.clone()).expect("register");
        gateway
            .set_backend_concurrency("fixture", 1)
            .expect("one slot");
        let first_gateway = gateway.clone();
        let first =
            tokio::spawn(async move { first_gateway.execute(request("fixture-model")).await });
        backend.entered.notified().await;
        let queued_request = request("fixture-model");
        let queued_id = queued_request.request_id.clone();
        let queued = gateway.execute(queued_request);
        tokio::pin!(queued);
        // Poll exactly once into the semaphore wait; no timing assumption is needed.
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(queued.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let cancelled = gateway.cancel(&queued_id, CancelTarget::Request);
        backend.release.notify_one();
        let _ = first.await.expect("first task");
        let _ = queued.await;
        gateway.shutdown().await.expect("shutdown");
        assert_eq!(
            (cancelled, backend.calls.load(Ordering::SeqCst)),
            (1, 1),
            "queued request was invisible to cancellation and dispatched after the slot became free"
        );
    }
}
