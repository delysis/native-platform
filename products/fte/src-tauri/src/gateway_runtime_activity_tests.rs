use super::*;
use fte_types::{
    BackendDescriptor, BackendRequest, CancelTarget, GatewayBackend, GatewayResponse,
    GatewayTicket, GatewayUsage, RequestCancellation, TerminalStatus, TicketCancellation,
};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{Notify, oneshot};

type FinalSenders =
    Arc<Mutex<BTreeMap<RequestId, oneshot::Sender<Result<GatewayResponse, GatewayError>>>>>;

#[derive(Default)]
struct FixtureBackend {
    pending: FinalSenders,
    deferred: AtomicBool,
    panic_next: AtomicBool,
    defer_setup: AtomicBool,
    calls: AtomicUsize,
    entered: Notify,
    release_setup: Notify,
}

struct FixtureCancellation {
    pending: FinalSenders,
    id: RequestId,
}
impl TicketCancellation for FixtureCancellation {
    fn cancel(&self, _target: CancelTarget) -> usize {
        cancel_fixture(&self.pending, &self.id)
    }
}
fn cancel_fixture(pending: &FinalSenders, id: &RequestId) -> usize {
    let Some(send) = pending.lock().expect("fixture senders").remove(id) else {
        return 0;
    };
    let mut error =
        GatewayError::invalid_request(id, "fixture_cancelled", "fixture cancellation acknowledged");
    error.class = fte_types::ErrorClass::Cancelled;
    error.http_status = 499;
    error.provider = Some("activity-fixture".into());
    let _ = send.send(Err(error));
    1
}
#[async_trait::async_trait]
impl GatewayBackend for FixtureBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "activity-fixture".into(),
            display_name: "fixture".into(),
            location: BackendLocation::Hosted,
            models: vec![ModelDescriptor {
                id: "activity-model".into(),
                aliases: vec![],
                display_name: "fixture".into(),
                backend_id: "activity-fixture".into(),
                location: BackendLocation::Hosted,
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
        if self.panic_next.swap(false, Ordering::SeqCst) {
            panic!("injected backend worker panic");
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        let id = request.request.request_id;
        let (events, receive_events) = tokio::sync::mpsc::channel(1);
        drop(events);
        let (send, receive) = oneshot::channel();
        if self.deferred.load(Ordering::SeqCst) {
            self.pending
                .lock()
                .expect("pending")
                .insert(id.clone(), send);
        } else {
            send.send(Ok(GatewayResponse {
                id: format!("fixture-{}", id.0),
                request_id: id.clone(),
                model: request.route.model_id.clone(),
                route: request.route,
                output: vec![],
                usage: GatewayUsage::default(),
                status: TerminalStatus::Completed,
                previous_response_id: None,
            }))
            .expect("fixture final");
        }
        self.entered.notify_one();
        if self.defer_setup.load(Ordering::SeqCst) {
            self.release_setup.notified().await;
        }
        Ok(GatewayTicket::new(
            id.clone(),
            receive_events,
            receive,
            Arc::new(FixtureCancellation {
                pending: Arc::clone(&self.pending),
                id,
            }),
            Arc::new(AtomicBool::new(false)),
        ))
    }
    fn cancel(&self, id: &RequestId, _target: CancelTarget) -> usize {
        let count = cancel_fixture(&self.pending, id);
        self.release_setup.notify_one();
        count
    }
}

fn fixture() -> (Arc<GatewayRuntimeOwner>, Arc<Database>, Arc<FixtureBackend>) {
    let owner = Arc::new(
        GatewayRuntimeOwner::new_with_store(Arc::new(super::tests::FakeCredentialStore::default()))
            .expect("owner"),
    );
    let database = super::tests::test_database(&format!("activity-{}", RequestId::new().0));
    owner
        .bind_database(Arc::clone(&database))
        .expect("database");
    let backend = Arc::new(FixtureBackend::default());
    owner
        .gateway
        .register_backend(backend.clone())
        .expect("backend");
    (owner, database, backend)
}
fn chat() -> serde_json::Value {
    serde_json::json!({"model":"activity-model","messages":[{"role":"user","content":"private prompt must not be logged"}]})
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(5), future)
        .await
        .expect("bounded fixture")
}

#[tokio::test]
async fn desktop_logs_once_with_unknown_usage_and_without_private_prompt() {
    let (owner, database, _) = fixture();
    owner.chat(chat()).await.expect("desktop request");
    let logs = database.get_recent_logs(10).expect("logs");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].provider_id, "activity-fixture");
    assert_eq!(logs[0].model_id, "activity-model");
    assert!(logs[0].tokens_used.is_none());
    assert_eq!(logs[0].status_code, 200);
    assert_eq!(
        database
            .get_global_log_summary()
            .expect("summary")
            .request_count,
        1
    );
    assert!(
        !serde_json::to_string(&logs)
            .expect("json")
            .contains("private prompt")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn authenticated_loopback_logs_once_with_unknown_usage() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (owner, database, _) = fixture();
    let token_directory =
        std::env::temp_dir().join(format!("fte-activity-token-{}", RequestId::new().0));
    let token_path = token_directory.join("token");
    let mut config = fte_loopback::LoopbackConfig::app_private(token_path.clone());
    config.edge_defaults = hosted_defaults();
    let server = fte_loopback::LoopbackServer::start(
        owner.gateway(),
        Arc::new(fte_store::SqliteStore::in_memory().expect("response store")),
        config,
    )
    .await
    .expect("loopback start");
    let token = std::fs::read_to_string(&token_path).expect("local token");
    let body = chat().to_string();
    let mut socket = tokio::net::TcpStream::connect(server.addresses()[0])
        .await
        .expect("connect");
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        token.trim(),
        body.len(),
        body
    );
    socket
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut response = Vec::new();
    bounded(socket.read_to_end(&mut response))
        .await
        .expect("read response");
    assert!(
        response.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&response)
    );
    let logs = database.get_recent_logs(10).expect("logs");
    assert_eq!(logs.len(), 1);
    assert!(logs.iter().all(|log| log.provider_id == "activity-fixture"
        && log.model_id == "activity-model"
        && log.tokens_used.is_none()
        && log.status_code == 200));
    assert_eq!(
        database
            .get_global_log_summary()
            .expect("summary")
            .request_count,
        1
    );
    let json = serde_json::to_string(&logs).expect("json");
    assert!(!json.contains("private prompt"));
    assert!(!json.contains(token.trim()));
    server.shutdown().await;
    std::fs::remove_file(token_path).expect("remove token");
    std::fs::remove_dir(token_directory).expect("remove token directory");
}

#[tokio::test]
async fn playground_stop_before_spawn_prevents_dispatch_and_allows_next_request() {
    let (owner, database, backend) = fixture();
    let id = owner.start_playground(chat(), "chat").expect("start");
    assert!(owner.cancel_playground(&id).expect("stop"));
    let error = bounded(owner.wait_playground(&id))
        .await
        .expect_err("cancelled");
    assert!(error.contains("request_cancelled"));
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        database.get_recent_logs(10).expect("logs")[0].status_code,
        499
    );
    let next = owner.start_playground(chat(), "chat").expect("restart");
    assert!(!owner.cancel_playground(&id).expect("stale stop"));
    bounded(owner.wait_playground(&next))
        .await
        .expect("next completed");
    assert_eq!(database.get_recent_logs(10).expect("logs").len(), 2);
}

#[tokio::test]
async fn playground_stop_reaches_registered_backend_during_setup() {
    let (owner, database, backend) = fixture();
    backend.deferred.store(true, Ordering::SeqCst);
    backend.defer_setup.store(true, Ordering::SeqCst);
    let id = owner.start_playground(chat(), "chat").expect("start");
    bounded(backend.entered.notified()).await;
    assert!(owner.cancel_playground(&id).expect("stop"));
    assert!(
        bounded(owner.wait_playground(&id))
            .await
            .expect_err("cancelled")
            .contains("fixture_cancelled")
    );
    assert!(backend.pending.lock().expect("pending").is_empty());
    assert_eq!(database.get_recent_logs(10).expect("logs").len(), 1);
    assert_eq!(
        database.get_recent_logs(10).expect("logs")[0].status_code,
        499
    );
}

#[tokio::test]
async fn queued_stop_does_not_dispatch_and_dropped_ticket_still_logs_once() {
    let (owner, database, backend) = fixture();
    backend.deferred.store(true, Ordering::SeqCst);
    owner
        .gateway
        .set_backend_concurrency("activity-fixture", 1)
        .expect("one slot");
    let (_, first) = canonical_chat_request(chat(), &owner.catalog).expect("canonical");
    let ticket = owner.gateway.execute(first).await.expect("first admitted");
    let (_, second) = canonical_chat_request(chat(), &owner.catalog).expect("canonical");
    let cancellation = Arc::new(RequestCancellation::default());
    let second = owner
        .gateway
        .execute_with_cancellation(second, Arc::clone(&cancellation));
    let mut second = std::pin::pin!(second);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(second.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    cancellation.cancel();
    assert_eq!(
        bounded(second).await.expect_err("queue cancelled").class,
        fte_types::ErrorClass::Cancelled
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    drop(ticket);
    bounded(async {
        while database.get_recent_logs(10).expect("logs").len() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        database
            .get_recent_logs(10)
            .expect("logs")
            .iter()
            .all(|log| log.status_code == 499)
    );
}

#[tokio::test]
async fn unresolved_failure_records_one_outcome_without_logging_untrusted_model_text() {
    let (owner, database, _) = fixture();
    let mut request = chat();
    request["model"] = serde_json::json!("untrusted text in an unknown model field");
    owner.chat(request).await.expect_err("no route");
    let logs = database.get_recent_logs(10).expect("logs");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].provider_id, "gateway");
    assert_eq!(logs[0].model_id, "unresolved");
    assert_eq!(logs[0].status_code, 503);
    assert_eq!(logs[0].tokens_used, None);
}

#[tokio::test]
async fn raw_completion_stop_uses_the_same_pre_dispatch_authority() {
    let (owner, database, backend) = fixture();
    let id = owner
        .start_playground(
            serde_json::json!({"model":"activity-model", "prompt":"private raw prompt"}),
            "completion",
        )
        .expect("start raw completion");
    assert!(owner.cancel_playground(&id).expect("stop"));
    assert!(
        bounded(owner.wait_playground(&id))
            .await
            .expect_err("cancelled")
            .contains("request_cancelled")
    );
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert_eq!(database.get_recent_logs(10).expect("logs").len(), 1);
}

#[test]
fn aggregate_usage_retains_known_sum_and_counts_missing_observations() {
    let (_, database, _) = fixture();
    database
        .log_request("activity-fixture", "activity-model", Some(7), 3, 200)
        .expect("known");
    database
        .log_request("activity-fixture", "activity-model", None, 3, 200)
        .expect("unknown");
    let global = database.get_global_log_summary().expect("global");
    assert_eq!(global.total_tokens, 7);
    assert_eq!(global.unknown_usage_requests, 1);
    let providers = database.get_provider_log_summaries().expect("providers");
    assert_eq!(providers["activity-fixture"].total_tokens, 7);
    assert_eq!(providers["activity-fixture"].unknown_usage_requests, 1);
}

#[tokio::test]
async fn panicked_playground_worker_clears_waited_slot_with_typed_failure() {
    let (owner, _, backend) = fixture();
    backend.panic_next.store(true, Ordering::SeqCst);
    let id = owner
        .start_playground(chat(), "chat")
        .expect("start panic fixture");
    let error = bounded(owner.wait_playground(&id))
        .await
        .expect_err("worker failure");
    let next = owner
        .start_playground(chat(), "chat")
        .expect("failed worker must release slot");
    assert!(error.starts_with("playground_worker_stopped:"), "{error}");
    bounded(owner.wait_playground(&next))
        .await
        .expect("replacement completes");
}

#[tokio::test]
async fn closed_worker_can_be_replaced_before_old_waiter_cleanup_without_clearing_new_identity() {
    let (owner, _, backend) = fixture();
    backend.panic_next.store(true, Ordering::SeqCst);
    let id = owner
        .start_playground(chat(), "chat")
        .expect("start panic fixture");
    let mut old_wait = std::pin::pin!(owner.wait_playground(&id));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(old_wait.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    bounded(async {
        loop {
            if owner
                .playground
                .lock()
                .expect("slot")
                .as_ref()
                .expect("pending")
                .result
                .has_changed()
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        !owner
            .cancel_playground(&id)
            .expect("finished worker cannot accept cancellation")
    );
    backend.deferred.store(true, Ordering::SeqCst);
    let next = owner
        .start_playground(chat(), "chat")
        .expect("closed slot can be replaced without waiter");
    assert!(
        old_wait
            .await
            .expect_err("old failure")
            .starts_with("playground_worker_stopped:")
    );
    assert_eq!(
        owner
            .playground
            .lock()
            .expect("slot")
            .as_ref()
            .expect("new slot retained")
            .id
            .0,
        next
    );
    assert!(owner.cancel_playground(&next).expect("new cancellation"));
    bounded(owner.wait_playground(&next))
        .await
        .expect_err("replacement cancelled");
}
