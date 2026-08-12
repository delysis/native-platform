use super::*;
use crate::secrets::SecretStoreError;
use async_trait::async_trait;
use fte_types::{
    BackendDescriptor, BackendRequest, CancelTarget, ContentBlock, GatewayBackend, GatewayRequest,
    GatewayResponse, GatewayTicket, GatewayUsage, GenerationInput, InputItem, MessageRole,
    ModelCapabilities, OutputItem, ResolvedRoute, SamplingOptions, StoragePolicy, StreamPolicy,
    TerminalStatus, TicketCancellation, ToolPolicy,
};
use platform_vertical_fixtures_v0::{
    EquivalenceProjectionV0, ObservationEnvelopeV0, VerticalFixtureManifestV0, sha256_identity,
    validate_baseline, validate_manifest,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::sync::{Semaphore, mpsc, oneshot};

const BASELINE_COMMIT: &str = "797500060047ccd10f9810fb4d5c8f374e00eb08";
const MANIFEST_BYTES: &[u8] =
    include_bytes!("../../tests/fixtures/w1/v0/fte-quit-relaunch.manifest.json");
const INPUT_BYTES: &[u8] =
    include_bytes!("../../tests/fixtures/w1/v0/fte-quit-relaunch-input-v1.json");
const PROJECTION_BYTES: &[u8] =
    include_bytes!("../../tests/fixtures/w1/v0/fte-quit-relaunch-projection.json");
const SOURCE_BYTES: &[u8] =
    include_bytes!("../../tests/fixtures/w1/v0/fte-quit-relaunch-production-tree-2db2d45.json");
static DATABASE_ID: AtomicU64 = AtomicU64::new(1);

struct FixtureDirectory {
    path: PathBuf,
}

impl FixtureDirectory {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "fte-w1-quit-relaunch-{}-{}",
                std::process::id(),
                DATABASE_ID.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create isolated fixture directory: {error}"),
            }
        }
    }

    fn database_path(&self) -> PathBuf {
        self.path.join("gateway.db")
    }
}

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).expect("remove isolated fixture directory");
    }
}

#[derive(Deserialize)]
struct FixtureInput {
    first_runtime_id: String,
    fresh_runtime_id: String,
    active_request_id: String,
    rejected_request_id: String,
    fresh_request_id: String,
    backend_id: String,
    model_id: String,
    durable_profile_key: String,
    durable_profile_value: String,
    expected_worker_ids: Vec<String>,
}

#[derive(Deserialize)]
struct SourceDescriptor {
    schema: String,
    repository_id: String,
    commit: String,
    prefixes: Vec<SourcePrefix>,
    git_blobs: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct SourcePrefix {
    path: String,
    boundary: String,
    sha256: String,
    byte_len: u64,
}

#[derive(Default)]
struct CountingCredentialStore {
    reads: AtomicUsize,
    writes: AtomicUsize,
    deletes: AtomicUsize,
}

impl CountingCredentialStore {
    fn accesses(&self) -> usize {
        self.reads.load(Ordering::Acquire)
            + self.writes.load(Ordering::Acquire)
            + self.deletes.load(Ordering::Acquire)
    }
}

impl CredentialStore for CountingCredentialStore {
    fn write(&self, _provider_id: &str, _secret: &[u8]) -> Result<(), SecretStoreError> {
        self.writes.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn read(&self, _provider_id: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        self.reads.fetch_add(1, Ordering::AcqRel);
        Ok(None)
    }

    fn delete(&self, _provider_id: &str) -> Result<bool, SecretStoreError> {
        self.deletes.fetch_add(1, Ordering::AcqRel);
        Ok(false)
    }
}

struct NoopTicketCancellation;

impl TicketCancellation for NoopTicketCancellation {
    fn cancel(&self, _target: CancelTarget) -> usize {
        0
    }
}

type FinalSender = oneshot::Sender<Result<GatewayResponse, GatewayError>>;

struct QuitControlledBackend {
    completion: Mutex<Option<FinalSender>>,
    cancel_seen: Semaphore,
    cancellations: AtomicUsize,
}

impl QuitControlledBackend {
    fn new() -> Self {
        Self {
            completion: Mutex::new(None),
            cancel_seen: Semaphore::new(0),
            cancellations: AtomicUsize::new(0),
        }
    }

    fn release_cancelled(&self, request_id: RequestId) {
        let completion = self
            .completion
            .lock()
            .expect("fixture completion")
            .take()
            .expect("active fixture request");
        completion
            .send(Err(GatewayError {
                code: "fixture_quit_cancelled".to_string(),
                class: fte_types::ErrorClass::Cancelled,
                retryable: false,
                http_status: 499,
                request_id,
                provider: None,
                safe_detail: "the deterministic fixture released after quit cancellation"
                    .to_string(),
            }))
            .expect("publish authoritative cancellation terminal");
    }
}

#[async_trait]
impl GatewayBackend for QuitControlledBackend {
    fn descriptor(&self) -> BackendDescriptor {
        fixture_descriptor()
    }

    fn readiness(&self) -> BackendReadiness {
        BackendReadiness::Ready
    }

    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
        let (_event_tx, event_rx) = mpsc::channel(1);
        let (final_tx, final_rx) = oneshot::channel();
        *self.completion.lock().expect("fixture completion") = Some(final_tx);
        Ok(GatewayTicket::new(
            request.request.request_id,
            event_rx,
            final_rx,
            Arc::new(NoopTicketCancellation),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ))
    }

    fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
        self.cancellations.fetch_add(1, Ordering::AcqRel);
        self.cancel_seen.add_permits(1);
        1
    }
}

struct SuccessfulBackend;

#[async_trait]
impl GatewayBackend for SuccessfulBackend {
    fn descriptor(&self) -> BackendDescriptor {
        fixture_descriptor()
    }

    fn readiness(&self) -> BackendReadiness {
        BackendReadiness::Ready
    }

    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
        let (event_tx, event_rx) = mpsc::channel(1);
        drop(event_tx);
        let (final_tx, final_rx) = oneshot::channel();
        let response = completed_response(request.request.request_id.clone(), request.route);
        final_tx
            .send(Ok(response))
            .expect("publish fresh-owner result");
        Ok(GatewayTicket::new(
            request.request.request_id,
            event_rx,
            final_rx,
            Arc::new(NoopTicketCancellation),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ))
    }

    fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
        0
    }
}

fn fixture_descriptor() -> BackendDescriptor {
    BackendDescriptor {
        id: "w1-deterministic-owner".to_string(),
        display_name: "W1 deterministic owner".to_string(),
        location: BackendLocation::LocalEmbedded,
        models: vec![ModelDescriptor {
            id: "w1-deterministic-model".to_string(),
            aliases: Vec::new(),
            display_name: "W1 deterministic model".to_string(),
            backend_id: "w1-deterministic-owner".to_string(),
            location: BackendLocation::LocalEmbedded,
            capabilities: ModelCapabilities {
                prompt_forms: vec![PromptForm::Chat],
                modalities: Vec::new(),
                tools: false,
                structured_output: false,
                reasoning: false,
                streaming: false,
                provider_cache: false,
            },
            context_tokens: Some(256),
            max_output_tokens: Some(16),
            observed: RouteObservations::default(),
        }],
    }
}

fn fixture_request(request_id: &str, input: &FixtureInput) -> GatewayRequest {
    GatewayRequest {
        request_id: RequestId(request_id.to_string()),
        client_id: "w1-quit-relaunch".to_string(),
        model: ModelSelector::ExactRoute {
            backend_id: input.backend_id.clone(),
            model_id: input.model_id.clone(),
        },
        input: GenerationInput::Chat {
            items: vec![InputItem::Message {
                id: None,
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "deterministic lifecycle probe".to_string(),
                }],
            }],
        },
        sampling: SamplingOptions::default(),
        response_format: fte_types::ResponseFormat::Text,
        tools: Vec::new(),
        tool_policy: ToolPolicy::default(),
        cache: fte_types::CachePolicy::default(),
        routing: fte_types::RoutingPolicy::default(),
        storage: StoragePolicy::default(),
        deadline: fte_types::DeadlinePolicy::default(),
        stream: StreamPolicy::default(),
        provider_extensions: BTreeMap::new(),
    }
}

fn completed_response(request_id: RequestId, route: ResolvedRoute) -> GatewayResponse {
    GatewayResponse {
        id: "w1-fresh-response".to_string(),
        request_id,
        model: route.model_id.clone(),
        route,
        output: vec![OutputItem::Message {
            id: "w1-fresh-message".to_string(),
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text {
                text: "fresh owner ready".to_string(),
            }],
        }],
        usage: GatewayUsage::default(),
        status: TerminalStatus::Completed,
        previous_response_id: None,
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_owned()
}

fn git_output(repository: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository)
        .output()
        .expect("execute source identity command");
    assert!(
        output.status.success(),
        "source identity command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn production_prefix(bytes: &[u8]) -> &[u8] {
    let boundary = b"\n#[cfg(test)]";
    let position = bytes
        .windows(boundary.len())
        .position(|window| window == boundary)
        .expect("production/test source boundary");
    &bytes[..position + 1]
}

fn verify_production_source() {
    let descriptor: SourceDescriptor =
        serde_json::from_slice(SOURCE_BYTES).expect("source descriptor");
    assert_eq!(descriptor.schema, "delysis.production_source_roots.v0");
    assert_eq!(descriptor.repository_id, "delysis/free-token-energy");
    assert_eq!(descriptor.commit, BASELINE_COMMIT);
    let repository = repository_root();
    assert!(
        Command::new("git")
            .args(["merge-base", "--is-ancestor", BASELINE_COMMIT, "HEAD"])
            .current_dir(&repository)
            .status()
            .expect("execute ancestry check")
            .success(),
        "fixture commit must descend from baseline"
    );

    for prefix in descriptor.prefixes {
        assert_eq!(prefix.boundary, "first_cfg_test");
        let current = std::fs::read(repository.join(&prefix.path)).expect("read source");
        let baseline = git_output(
            &repository,
            &["show", &format!("{}:{}", descriptor.commit, prefix.path)],
        );
        for bytes in [production_prefix(&current), production_prefix(&baseline)] {
            let identity = sha256_identity("fte.quit_relaunch.production.prefix", bytes);
            assert_eq!(identity.digest.hex, prefix.sha256);
            assert_eq!(identity.length, prefix.byte_len);
        }
    }

    for (path, expected_oid) in descriptor.git_blobs {
        let working_tree = git_output(&repository, &["hash-object", "--no-filters", "--", &path]);
        assert_eq!(
            String::from_utf8(working_tree).unwrap().trim(),
            expected_oid
        );
        for revision in [&descriptor.commit, "HEAD"] {
            let actual = git_output(&repository, &["rev-parse", &format!("{revision}:{path}")]);
            assert_eq!(String::from_utf8(actual).unwrap().trim(), expected_oid);
        }
    }
}

#[tokio::test]
async fn w1_product_owner_quit_relaunch_is_quiescent_and_fresh() {
    let manifest: VerticalFixtureManifestV0 =
        serde_json::from_slice(MANIFEST_BYTES).expect("parse quit/relaunch manifest");
    validate_manifest(&manifest).expect("valid quit/relaunch manifest");
    let case = &manifest.cases[0];
    let input: FixtureInput = serde_json::from_slice(INPUT_BYTES).expect("parse fixture input");
    assert_eq!(case.source.commit, BASELINE_COMMIT);
    assert_eq!(
        sha256_identity(case.source.production_tree.id.clone(), SOURCE_BYTES),
        case.source.production_tree
    );
    assert_eq!(
        sha256_identity(case.inputs[0].identity.id.clone(), INPUT_BYTES),
        case.inputs[0].identity
    );
    assert_eq!(
        sha256_identity(case.expected_projection.id.clone(), PROJECTION_BYTES),
        case.expected_projection
    );
    verify_production_source();

    let credentials = Arc::new(CountingCredentialStore::default());
    let store: Arc<dyn CredentialStore> = credentials.clone();
    let fixture_directory = FixtureDirectory::new();
    let database_path = fixture_directory.database_path();
    let database = Arc::new(Database::new(database_path.clone()).expect("product database"));
    database
        .save_profile_field(&input.durable_profile_key, &input.durable_profile_value)
        .expect("persist product profile state");
    let owner = Arc::new(
        GatewayRuntimeOwner::new_with_store_and_runtime_id(
            store,
            RequestId(input.first_runtime_id.clone()),
        )
        .expect("first owner"),
    );
    owner
        .bind_database(Arc::clone(&database))
        .expect("bind product database");
    let controlled = Arc::new(QuitControlledBackend::new());
    owner
        .gateway()
        .register_backend(controlled.clone())
        .expect("register deterministic backend");
    let first_gateway = owner.gateway();
    let ticket = first_gateway
        .execute(fixture_request(&input.active_request_id, &input))
        .await
        .expect("accept active work");
    let terminal = tokio::spawn(async move { ticket.final_response().await });

    let shutdown_owner = Arc::clone(&owner);
    let shutdown = tokio::spawn(async move { shutdown_owner.shutdown_with_report().await });
    controlled
        .cancel_seen
        .acquire()
        .await
        .expect("observe cancellation")
        .forget();
    assert!(
        !shutdown.is_finished(),
        "shutdown receipt cannot precede authoritative backend final release"
    );
    assert_eq!(controlled.cancellations.load(Ordering::Acquire), 1);
    let rejected = first_gateway
        .execute(fixture_request(&input.rejected_request_id, &input))
        .await
        .expect_err("quiescing owner must reject new work");
    assert_eq!(rejected.code, "gateway_quiescing");

    controlled.release_cancelled(RequestId(input.active_request_id.clone()));
    let cancelled = terminal
        .await
        .expect("terminal task")
        .expect_err("quit cancels active work");
    assert_eq!(cancelled.code, "fixture_quit_cancelled");
    let first_report = shutdown.await.expect("shutdown task");
    assert_eq!(first_report.runtime_id.0, input.first_runtime_id);
    first_report
        .gateway
        .result
        .clone()
        .expect("first Gateway shutdown");
    assert_eq!(
        first_report.gateway.expected_worker_ids,
        input.expected_worker_ids
    );
    let mut first_joined_worker_ids = first_report.gateway.joined_worker_ids.clone();
    first_joined_worker_ids.sort_unstable();
    assert_eq!(
        first_joined_worker_ids,
        first_report.gateway.expected_worker_ids
    );
    assert_eq!(first_report.gateway.retained_tasks, 0);
    assert!(first_report.native_host_joined);
    assert_eq!(first_gateway.status().active_requests, 0);
    assert_eq!(
        first_gateway.status().lifecycle,
        fte_types::GatewayLifecycle::Closed
    );

    drop(owner);
    drop(database);
    let reopened = Arc::new(Database::new(database_path).expect("reopen product database"));
    assert_eq!(
        reopened
            .get_profile_field(&input.durable_profile_key)
            .expect("read durable profile"),
        Some(input.durable_profile_value.clone())
    );
    let store: Arc<dyn CredentialStore> = credentials.clone();
    let fresh_owner = GatewayRuntimeOwner::new_with_store_and_runtime_id(
        store,
        RequestId(input.fresh_runtime_id.clone()),
    )
    .expect("fresh owner");
    fresh_owner
        .bind_database(reopened)
        .expect("bind reopened product database");
    fresh_owner
        .gateway()
        .register_backend(Arc::new(SuccessfulBackend))
        .expect("register fresh deterministic backend");
    let fresh_gateway = fresh_owner.gateway();
    assert!(!Arc::ptr_eq(&first_gateway, &fresh_gateway));
    let response = fresh_gateway
        .execute(fixture_request(&input.fresh_request_id, &input))
        .await
        .expect("fresh owner accepts work")
        .final_response()
        .await
        .expect("fresh owner completes work");
    assert_eq!(response.status, TerminalStatus::Completed);
    assert_eq!(response.request_id.0, input.fresh_request_id);
    let fresh_report = fresh_owner.shutdown_with_report().await;
    assert_eq!(fresh_report.runtime_id.0, input.fresh_runtime_id);
    fresh_report
        .gateway
        .result
        .clone()
        .expect("fresh Gateway shutdown");
    assert_eq!(
        fresh_report.gateway.expected_worker_ids,
        input.expected_worker_ids
    );
    let mut fresh_joined_worker_ids = fresh_report.gateway.joined_worker_ids.clone();
    fresh_joined_worker_ids.sort_unstable();
    assert_eq!(
        fresh_joined_worker_ids,
        fresh_report.gateway.expected_worker_ids
    );
    assert_eq!(fresh_report.gateway.retained_tasks, 0);
    assert!(fresh_report.native_host_joined);
    assert_eq!(credentials.accesses(), 0);
    drop(fresh_owner);

    let durable_identity = sha256_identity(
        "fte.runtime.profile.marker",
        input.durable_profile_value.as_bytes(),
    );
    assert_eq!(case.state_identities[0].baseline.identity, durable_identity);
    let joined_identity = sha256_identity(
        "fte.runtime.first.joined_worker_ids",
        format!(
            "{}\n{}",
            input.first_runtime_id,
            input.expected_worker_ids.join("\n")
        )
        .as_bytes(),
    );
    let fresh_joined_identity = sha256_identity(
        "fte.runtime.fresh.joined_worker_ids",
        format!(
            "{}\n{}",
            input.fresh_runtime_id,
            input.expected_worker_ids.join("\n")
        )
        .as_bytes(),
    );
    let projection: EquivalenceProjectionV0 = serde_json::from_value(json!({
        "ordered_events": [
            {"sequence": 0, "operation_id": input.active_request_id, "attempt_id": "owner.1.active", "correlation_id": "fte.quit_relaunch.w1", "kind": "started", "payload": null},
            {"sequence": 1, "operation_id": "fte.owner.1", "attempt_id": "owner.1.quit", "correlation_id": "fte.quit_relaunch.w1", "kind": "cancel_requested", "payload": null},
            {"sequence": 2, "operation_id": input.rejected_request_id, "attempt_id": "owner.1.rejected", "correlation_id": "fte.quit_relaunch.w1", "kind": "failed", "payload": null},
            {"sequence": 3, "operation_id": input.active_request_id, "attempt_id": "owner.1.active", "correlation_id": "fte.quit_relaunch.w1", "kind": "cancelled", "payload": null},
            {"sequence": 4, "operation_id": "fte.owner.1", "attempt_id": "owner.1.quit", "correlation_id": "fte.quit_relaunch.w1", "kind": "completed", "payload": joined_identity},
            {"sequence": 5, "operation_id": "fte.database.profile", "attempt_id": "owner.2.reopen", "correlation_id": "fte.quit_relaunch.w1", "kind": "completed", "payload": durable_identity},
            {"sequence": 6, "operation_id": input.fresh_request_id, "attempt_id": "owner.2.fresh", "correlation_id": "fte.quit_relaunch.w1", "kind": "completed", "payload": fresh_joined_identity}
        ],
        "durable_state": [{
            "state_id": "fte.runtime.profile",
            "schema_id": "fte.database.master_profile.v1",
            "before": durable_identity,
            "after": durable_identity,
            "disposition": "unchanged"
        }],
        "lifecycle": [
            {"operation_id": input.active_request_id, "attempt_id": "owner.1.active", "correlation_id": "fte.quit_relaunch.w1", "terminal": "cancelled", "released": true},
            {"operation_id": "fte.owner.1", "attempt_id": "owner.1.quit", "correlation_id": "fte.quit_relaunch.w1", "terminal": "completed", "released": true},
            {"operation_id": input.rejected_request_id, "attempt_id": "owner.1.rejected", "correlation_id": "fte.quit_relaunch.w1", "terminal": "failed", "released": true},
            {"operation_id": "fte.database.profile", "attempt_id": "owner.2.reopen", "correlation_id": "fte.quit_relaunch.w1", "terminal": "completed", "released": true},
            {"operation_id": input.fresh_request_id, "attempt_id": "owner.2.fresh", "correlation_id": "fte.quit_relaunch.w1", "terminal": "completed", "released": true}
        ],
        "ownership": {
            "active_operations": 0,
            "retained_tasks": first_report.gateway.retained_tasks + fresh_report.gateway.retained_tasks,
            "expected_workers": first_report.gateway.expected_worker_ids.len() + fresh_report.gateway.expected_worker_ids.len(),
            "joined_workers": first_report.gateway.joined_worker_ids.len() + fresh_report.gateway.joined_worker_ids.len()
        },
        "output_facts": {
            "admission_closed_during_quit": {"kind": "boolean", "value": true},
            "active_request_cancelled": {"kind": "boolean", "value": true},
            "authoritative_terminal_released_before_shutdown_receipt": {"kind": "boolean", "value": true},
            "first_runtime_id": {"kind": "text", "value": input.first_runtime_id},
            "fresh_runtime_id": {"kind": "text", "value": input.fresh_runtime_id},
            "first_exact_joined_worker_ids": {"kind": "digest", "value": joined_identity.digest},
            "fresh_exact_joined_worker_ids": {"kind": "digest", "value": fresh_joined_identity.digest},
            "native_owner_joined": {"kind": "boolean", "value": first_report.native_host_joined},
            "same_product_database_reopened": {"kind": "boolean", "value": true},
            "fresh_owner_distinct": {"kind": "boolean", "value": true},
            "fresh_owner_completed_work": {"kind": "boolean", "value": true},
            "fresh_owner_zero_retained_tasks": {"kind": "boolean", "value": fresh_report.gateway.retained_tasks == 0},
            "credential_store_accesses": {"kind": "integer", "value": credentials.accesses()},
            "hosted_network_requests": {"kind": "integer", "value": 0}
        },
        "fail_closed_facts": [
            "new admission was rejected after quit began",
            "shutdown receipt was not published before the authoritative cancellation terminal released",
            "every retained backend worker ID was joined before closure",
            "fresh work used a distinct Gateway runtime owner",
            "no hosted provider, API key, Keychain, or network path was entered"
        ]
    }))
    .expect("construct quit/relaunch projection");
    let expected: EquivalenceProjectionV0 =
        serde_json::from_slice(PROJECTION_BYTES).expect("parse expected projection");
    assert_eq!(projection, expected);
    let observation: ObservationEnvelopeV0 = serde_json::from_value(json!({
        "schema": "delysis.vertical_observation.v0",
        "vertical_id": manifest.vertical_id,
        "case_id": case.case_id,
        "implementation_revision": case.source.commit,
        "observed_prerequisites": [],
        "evidence": {
            "schema": "delysis.evidence_claim.v0",
            "tier": "reproducible",
            "threat_model": "product Gateway owner quit closes admission, cancels and releases active work, joins exact workers, then a distinct owner reopens product state and completes fresh work",
            "exact_source": case.source.production_tree.digest,
            "exact_runtime_or_artifact": case.inputs[0].identity.digest,
            "execution_kind": "fixture",
            "omitted_claims": manifest.omitted_claims,
            "negative_evidence": []
        },
        "projection": projection
    }))
    .expect("construct quit/relaunch observation");
    validate_baseline(
        &manifest,
        &case.case_id,
        PROJECTION_BYTES,
        &[],
        &observation,
    )
    .expect("central protocol accepts product-owner quit/relaunch evidence");
}
