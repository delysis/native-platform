//! Wave 1 contract checks over the real gateway surfaces.
//!
//! FTE currently binds only the stable-shutdown ownership slice. Ten real
//! Gateway requests retain ten real backend tasks; the real Gateway shutdown
//! coordinator waits for those tasks and reports the exact backend-shutdown
//! worker identities it joined. The adapter does not claim the other ten
//! lifecycle suites or construct a complete lifecycle manifest.

use super::*;
use async_trait::async_trait;
use fte_types::{
    BackendReadiness, BackendRequest, CachePolicy, CancelTarget, ContentBlock, DeadlinePolicy,
    GatewayResponse, GatewayTicket, GatewayUsage, GenerationInput, InputItem, MessageRole,
    ModelCapabilities, ModelSelector, PromptForm, RouteObservations, RoutingPolicy,
    SamplingOptions, StoragePolicy, StreamPolicy, TerminalStatus, TicketCancellation, ToolPolicy,
};
use platform_contract_testkit::compositional_lifecycle::{
    ShutdownWitness, StableShutdownAdapter, run_stable_shutdown_suite,
};
use platform_contract_testkit::contracts::{
    self, CapabilityEntryV0, CapabilitySnapshotV0, ContentDigest, DataHandlingV0, DataTierV0,
    LoggingPolicyV0, NetworkPolicyV0, OperationId, PayloadRedactionV0, PrivacyDecisionV0,
    PrivacyDenialV0, PrivacyPolicyV0, ProviderId, Readiness, RedactionStateV0, RetryAdvice,
    RoutePrivacyContextV0, RouteTargetV0, ServiceErrorV0, ServiceId, TriState,
};
use platform_contract_testkit::{
    AttemptIdentity, ClosedFacts, LifecycleImplementation, LifecyclePhase, OperationPhase,
    OperationSnapshot, ShutdownOutcome, TerminalClass, TerminalRecord,
    validate_capability_snapshot_v0, validate_service_error_v0,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

enum FteGatewayLifecycle {}

impl LifecycleImplementation for FteGatewayLifecycle {
    const PRODUCT: &'static str = "free-token-energy";
    const IMPLEMENTATION: &'static str = "gateway-v2";
}

#[derive(Clone)]
struct GatewayStableShutdownAdapter {
    gateway: Arc<Gateway>,
    runtime: Arc<tokio::runtime::Runtime>,
    next_sequence: Arc<AtomicU64>,
}

struct ControlledGatewayOperation {
    request_id: RequestId,
    sequence: u64,
    ticket: Mutex<Option<GatewayTicket>>,
    complete: Mutex<Option<oneshot::Sender<()>>>,
    allow_exit: Arc<Semaphore>,
    shutdown_observed: Mutex<Option<mpsc::Receiver<()>>>,
}

struct ControlledGatewayBackend {
    descriptor: BackendDescriptor,
    complete: Mutex<Option<oneshot::Receiver<()>>>,
    allow_exit: Arc<Semaphore>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown_observed: Mutex<Option<mpsc::SyncSender<()>>>,
}

struct NoopCancellation;

impl TicketCancellation for NoopCancellation {
    fn cancel(&self, _target: CancelTarget) -> usize {
        0
    }
}

#[async_trait]
impl GatewayBackend for ControlledGatewayBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }

    fn readiness(&self) -> BackendReadiness {
        BackendReadiness::Ready
    }

    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
        let complete = self
            .complete
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &request.request.request_id,
                    "contract_backend_already_executed",
                    "the controlled contract backend accepts exactly one request",
                )
            })?;
        let request_id = request.request.request_id.clone();
        let route = request.route;
        let response = GatewayResponse {
            id: format!("contract-response-{request_id}"),
            request_id: request_id.clone(),
            model: route.model_id.clone(),
            route: route.clone(),
            output: Vec::new(),
            usage: GatewayUsage {
                selected_route: Some(route),
                ..GatewayUsage::default()
            },
            status: TerminalStatus::Completed,
            previous_response_id: None,
        };
        let (_events_tx, events_rx) = tokio::sync::mpsc::channel(1);
        let (final_tx, final_rx) = oneshot::channel();
        let terminal = Arc::new(AtomicBool::new(false));
        let terminal_for_task = Arc::clone(&terminal);
        let allow_exit = Arc::clone(&self.allow_exit);
        let task = tokio::spawn(async move {
            let _ = complete.await;
            terminal_for_task.store(true, Ordering::Release);
            let _ = final_tx.send(Ok(response));
            let _permit = allow_exit
                .acquire()
                .await
                .expect("worker-exit gate remains open");
        });
        *self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
        Ok(GatewayTicket::new(
            request_id,
            events_rx,
            final_rx,
            Arc::new(NoopCancellation),
            terminal,
        ))
    }

    fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
        0
    }

    async fn shutdown(&self) -> Result<(), GatewayError> {
        let task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            task.await.map_err(|error| GatewayError {
                code: "contract_backend_task_failed".to_owned(),
                class: ErrorClass::Internal,
                retryable: false,
                http_status: 500,
                request_id: RequestId::new(),
                provider: Some(self.descriptor.id.clone()),
                safe_detail: format!("controlled backend task failed: {error}"),
            })?;
        }
        if let Some(observed) = self
            .shutdown_observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = observed.send(());
        }
        Ok(())
    }
}

struct GatewayShutdownWitness {
    started: Mutex<Option<mpsc::Receiver<()>>>,
    result: Mutex<ShutdownWitnessResult>,
    thread: Option<thread::JoinHandle<()>>,
}

type ShutdownWitnessResult = (
    mpsc::Receiver<Result<ShutdownOutcome, GatewayError>>,
    Option<ShutdownOutcome>,
);

impl ShutdownWitness for GatewayShutdownWitness {
    type Error = String;

    fn wait_started(&self, timeout: Duration) -> Result<(), Self::Error> {
        self.started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| "shutdown start was already observed".to_owned())?
            .recv_timeout(timeout)
            .map_err(|error| format!("shutdown did not start before the witness deadline: {error}"))
    }

    fn try_complete(&self) -> Result<Option<ShutdownOutcome>, Self::Error> {
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.1.is_none() {
            match result.0.try_recv() {
                Ok(Ok(outcome)) => result.1 = Some(outcome),
                Ok(Err(error)) => return Err(error.to_string()),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err("shutdown result channel disconnected".to_owned());
                }
            }
        }
        Ok(result.1.clone())
    }

    fn wait(mut self, timeout: Duration) -> Result<ShutdownOutcome, Self::Error> {
        let outcome = {
            let mut result = self
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match result.1.take() {
                Some(outcome) => outcome,
                None => result
                    .0
                    .recv_timeout(timeout)
                    .map_err(|error| format!("shutdown did not finish before deadline: {error}"))?
                    .map_err(|error| error.to_string())?,
            }
        };
        self.thread
            .take()
            .ok_or_else(|| "shutdown thread was already joined".to_owned())?
            .join()
            .map_err(|_| "shutdown witness thread panicked".to_owned())?;
        Ok(outcome)
    }
}

impl GatewayStableShutdownAdapter {
    fn request(sequence: u64, backend_id: &str, model_id: &str) -> GatewayRequest {
        let mut request = local_request();
        request.client_id = format!("w1-stable-shutdown-{sequence}");
        request.model = ModelSelector::ExactRoute {
            backend_id: backend_id.to_owned(),
            model_id: model_id.to_owned(),
        };
        request
    }
}

impl StableShutdownAdapter for GatewayStableShutdownAdapter {
    type Implementation = FteGatewayLifecycle;
    type Error = GatewayError;
    type Operation = ControlledGatewayOperation;
    type ShutdownWitness = GatewayShutdownWitness;

    fn deterministic() -> Self {
        Self {
            gateway: Arc::new(Gateway::new(GatewayDefaults::default())),
            runtime: Arc::new(tokio::runtime::Runtime::new().expect("contract-test runtime")),
            next_sequence: Arc::new(AtomicU64::new(1)),
        }
    }

    fn start(&self, _operation_id: &str) -> Result<Self::Operation, Self::Error> {
        let sequence = self.next_sequence.fetch_add(1, Ordering::AcqRel);
        let backend_id = format!("w1-controlled-backend-{sequence}");
        let model_id = format!("{backend_id}-model");
        let (complete_tx, complete_rx) = oneshot::channel();
        let (shutdown_observed_tx, shutdown_observed_rx) = mpsc::sync_channel(0);
        let allow_exit = Arc::new(Semaphore::new(0));
        self.gateway
            .register_backend(Arc::new(ControlledGatewayBackend {
                descriptor: descriptor(&backend_id, BackendLocation::LocalEmbedded),
                complete: Mutex::new(Some(complete_rx)),
                allow_exit: Arc::clone(&allow_exit),
                task: Mutex::new(None),
                shutdown_observed: Mutex::new(Some(shutdown_observed_tx)),
            }))?;
        let request = Self::request(sequence, &backend_id, &model_id);
        let request_id = request.request_id.clone();
        let ticket = self.runtime.block_on(self.gateway.execute(request))?;
        Ok(ControlledGatewayOperation {
            request_id,
            sequence,
            ticket: Mutex::new(Some(ticket)),
            complete: Mutex::new(Some(complete_tx)),
            allow_exit,
            shutdown_observed: Mutex::new(Some(shutdown_observed_rx)),
        })
    }

    fn request_completed_release(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation
            .complete
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_completion_already_requested",
                    "completion may be requested only once",
                )
            })?
            .send(())
            .map_err(|_| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_completion_disconnected",
                    "the controlled backend stopped before completion",
                )
            })
    }

    fn wait_released(
        &self,
        operation: &Self::Operation,
        timeout: Duration,
    ) -> Result<OperationSnapshot, Self::Error> {
        let ticket = operation
            .ticket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_result_already_observed",
                    "the controlled result may be observed only once",
                )
            })?;
        let response = self
            .runtime
            .block_on(async move { tokio::time::timeout(timeout, ticket.final_response()).await })
            .map_err(|_| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_result_timeout",
                    "the controlled result did not arrive before the contract deadline",
                )
            })??;
        if response.status != TerminalStatus::Completed {
            return Err(GatewayError::unavailable(
                &operation.request_id,
                "contract_result_not_completed",
                "the controlled request did not complete successfully",
            ));
        }
        let identity = AttemptIdentity {
            operation_id: operation.request_id.to_string(),
            attempt_id: format!("gateway-attempt-{}", operation.sequence),
            sequence: operation.sequence,
        };
        let terminal = TerminalRecord {
            class: TerminalClass::Completed,
            sequence: operation.sequence,
        };
        Ok(OperationSnapshot {
            identity,
            phase: OperationPhase::Released,
            cancellation_requested: self.gateway.status().lifecycle != GatewayLifecycle::Running,
            authoritative_terminal: Some(terminal),
            final_projection: Some(terminal),
            progress_projection: Vec::new(),
        })
    }

    fn allow_worker_exit(&self, operation: &Self::Operation) -> Result<(), Self::Error> {
        operation.allow_exit.add_permits(1);
        operation
            .shutdown_observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_shutdown_already_observed",
                    "backend shutdown completion may be observed only once",
                )
            })?
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| {
                GatewayError::unavailable(
                    &operation.request_id,
                    "contract_backend_shutdown_timeout",
                    "the backend worker did not exit before the contract deadline",
                )
            })
    }

    fn begin_shutdown(&self) -> Self::ShutdownWitness {
        let runtime = Arc::clone(&self.runtime);
        let gateway = Arc::clone(&self.gateway);
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            runtime.block_on(async move {
                let shutdown_gateway = Arc::clone(&gateway);
                let shutdown =
                    tokio::spawn(async move { shutdown_gateway.shutdown_report().await });
                loop {
                    if gateway.status().lifecycle != GatewayLifecycle::Running {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                let _ = started_tx.send(());
                let result = match shutdown.await {
                    Ok(report) => report.result.map(|()| {
                        let status = gateway.status();
                        ShutdownOutcome {
                            facts: ClosedFacts {
                                lifecycle: lifecycle_phase(status.lifecycle),
                                active_operations: status.active_requests,
                                retained_tasks: 0,
                                expected_workers: report.expected_worker_ids.len(),
                                joined_workers: report.joined_worker_ids.len(),
                            },
                            expected_worker_ids: report.expected_worker_ids,
                            joined_worker_ids: report.joined_worker_ids,
                        }
                    }),
                    Err(error) => Err(GatewayError::unavailable(
                        &RequestId::new(),
                        "contract_shutdown_task_failed",
                        &format!("Gateway shutdown task failed: {error}"),
                    )),
                };
                let _ = result_tx.send(result);
            });
        });
        GatewayShutdownWitness {
            started: Mutex::new(Some(started_rx)),
            result: Mutex::new((result_rx, None)),
            thread: Some(thread),
        }
    }
}

fn lifecycle_phase(phase: GatewayLifecycle) -> LifecyclePhase {
    match phase {
        GatewayLifecycle::Running => LifecyclePhase::Running,
        GatewayLifecycle::Quiescing => LifecyclePhase::Quiescing,
        GatewayLifecycle::Closed => LifecyclePhase::Closed,
    }
}

struct SnapshotBackend {
    descriptor: BackendDescriptor,
    readiness: BackendReadiness,
}

#[async_trait]
impl GatewayBackend for SnapshotBackend {
    fn descriptor(&self) -> BackendDescriptor {
        self.descriptor.clone()
    }

    fn readiness(&self) -> BackendReadiness {
        self.readiness.clone()
    }

    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError> {
        Err(GatewayError::unavailable(
            &request.request.request_id,
            "fixture_not_executed",
            "contract fixture exposes inventory only",
        ))
    }

    fn cancel(&self, _request_id: &RequestId, _target: CancelTarget) -> usize {
        0
    }
}

fn descriptor(id: &str, location: BackendLocation) -> BackendDescriptor {
    BackendDescriptor {
        id: id.to_owned(),
        display_name: id.to_owned(),
        location,
        models: vec![ModelDescriptor {
            id: format!("{id}-model"),
            aliases: Vec::new(),
            display_name: id.to_owned(),
            backend_id: id.to_owned(),
            location,
            capabilities: ModelCapabilities {
                prompt_forms: vec![PromptForm::Chat],
                modalities: Vec::new(),
                tools: false,
                structured_output: false,
                reasoning: false,
                streaming: true,
                provider_cache: false,
            },
            context_tokens: Some(4_096),
            max_output_tokens: Some(512),
            observed: RouteObservations::default(),
        }],
    }
}

fn local_request() -> GatewayRequest {
    GatewayRequest {
        request_id: RequestId::new(),
        client_id: "w1-contract-test".to_owned(),
        model: ModelSelector::Profile {
            name: "local-only".to_owned(),
        },
        input: GenerationInput::Chat {
            items: vec![InputItem::Message {
                id: None,
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "inventory probe".to_owned(),
                }],
            }],
        },
        sampling: SamplingOptions::default(),
        response_format: ResponseFormat::Text,
        tools: Vec::new(),
        tool_policy: ToolPolicy::default(),
        cache: CachePolicy::default(),
        routing: RoutingPolicy::default(),
        storage: StoragePolicy::default(),
        deadline: DeadlinePolicy::default(),
        stream: StreamPolicy::default(),
        provider_extensions: BTreeMap::new(),
    }
}

fn local_only_contract_policy() -> PrivacyPolicyV0 {
    PrivacyPolicyV0 {
        schema: contracts::privacy::PRIVACY_POLICY_SCHEMA_V0.to_owned(),
        network: NetworkPolicyV0::Deny,
        data_handling: DataHandlingV0::LocalOnly,
        allowed_provider_ids: Vec::new(),
        allowed_hosted_data_tiers: Vec::new(),
        payload_redaction: PayloadRedactionV0::LocalOnly,
        logging: LoggingPolicyV0::Disabled,
    }
}

fn capability_snapshot(gateway: &Gateway) -> CapabilitySnapshotV0 {
    let backend_snapshots = gateway.backend_snapshots();
    let serialized = serde_json::to_vec(&backend_snapshots).expect("serialize gateway inventory");
    let digest = format!("{:x}", Sha256::digest(serialized));
    let entries = backend_snapshots
        .into_iter()
        .flat_map(|snapshot| {
            let readiness = match snapshot.readiness {
                BackendReadiness::Ready => Readiness::Ready,
                BackendReadiness::Loading => Readiness::Unknown,
                BackendReadiness::NotConfigured { .. } | BackendReadiness::Unavailable { .. } => {
                    Readiness::Unavailable
                }
            };
            let remediation = match &snapshot.readiness {
                BackendReadiness::Ready => None,
                BackendReadiness::Loading => {
                    Some("wait for backend loading to complete".to_owned())
                }
                BackendReadiness::NotConfigured { reason }
                | BackendReadiness::Unavailable { reason } => Some(reason.clone()),
            };
            snapshot.descriptor.models.into_iter().map(move |model| {
                let mut limits = BTreeMap::new();
                if let Some(context_tokens) = model.context_tokens {
                    limits.insert("context_tokens".to_owned(), u64::from(context_tokens));
                }
                if let Some(max_output_tokens) = model.max_output_tokens {
                    limits.insert("max_output_tokens".to_owned(), u64::from(max_output_tokens));
                }
                CapabilityEntryV0 {
                    operation: "chat".to_owned(),
                    backend_or_resource_id: format!("{}/{}", model.backend_id, model.id),
                    readiness,
                    limits,
                    network: if model.location == BackendLocation::Hosted {
                        TriState::Yes
                    } else {
                        TriState::No
                    },
                    privacy_eligible: if model.location == BackendLocation::LocalEmbedded {
                        TriState::Yes
                    } else {
                        TriState::No
                    },
                    evidence_source: "Gateway::backend_snapshots".to_owned(),
                    evidence_outcome: format!("{:?}", snapshot.readiness),
                    observed_at_unix_ms: None,
                    remediation: remediation.clone(),
                }
            })
        })
        .collect();
    CapabilitySnapshotV0 {
        schema: contracts::capability::CAPABILITY_SCHEMA_V0.to_owned(),
        snapshot_id: ContentDigest::sha256(digest).expect("SHA-256 digest must validate"),
        target: "fte-router-gateway".to_owned(),
        services: BTreeMap::from([(
            ServiceId::new("inference-gateway").expect("service ID must validate"),
            entries,
        )]),
        reports: Vec::new(),
    }
}

fn service_error(error: &GatewayError) -> ServiceErrorV0 {
    let class = match error.class {
        ErrorClass::Authentication | ErrorClass::Authorization => contracts::ErrorClass::Permission,
        ErrorClass::InvalidRequest => contracts::ErrorClass::InvalidRequest,
        ErrorClass::Capability => contracts::ErrorClass::Unsupported,
        ErrorClass::Privacy => contracts::ErrorClass::Privacy,
        ErrorClass::Quota | ErrorClass::RateLimit => contracts::ErrorClass::ResourceExhausted,
        ErrorClass::Timeout => contracts::ErrorClass::Timeout,
        ErrorClass::Cancelled => contracts::ErrorClass::Cancelled,
        ErrorClass::Unavailable | ErrorClass::Provider => contracts::ErrorClass::Unavailable,
        ErrorClass::Internal => contracts::ErrorClass::Internal,
    };
    ServiceErrorV0 {
        schema: contracts::error::SERVICE_ERROR_SCHEMA_V0.to_owned(),
        code: error.code.clone(),
        class,
        retry: if error.class == ErrorClass::Privacy || !error.retryable {
            RetryAdvice::Never
        } else {
            RetryAdvice::DifferentRoute
        },
        operation_id: Some(
            OperationId::new(error.request_id.to_string())
                .expect("FTE request IDs must be safe contract operation IDs"),
        ),
        service: ServiceId::new("inference-gateway").expect("service ID must validate"),
        safe_detail: error.safe_detail.clone(),
    }
}

#[test]
fn w1_contract_stable_shutdown_slice_uses_real_gateway_workers() {
    let evidence = run_stable_shutdown_suite::<GatewayStableShutdownAdapter>("gateway-supervisor");
    assert_eq!(evidence.product(), "free-token-energy");
    assert_eq!(evidence.implementation(), "gateway-v2");
    assert_eq!(evidence.suite(), "stable-shutdown");
    assert_eq!(evidence.invariants().count(), 2);
}

#[test]
fn w1_contract_local_only_privacy_matches_real_router_without_network() {
    let request = local_request();
    let local = descriptor("local-fixture", BackendLocation::LocalEmbedded);
    let hosted = descriptor("hosted-fixture", BackendLocation::Hosted);
    assert_eq!(
        candidate_allowed(&request, &local, &local.models[0], None),
        Ok(())
    );
    assert_eq!(
        candidate_allowed(&request, &hosted, &hosted.models[0], None),
        Err("privacy_local_only")
    );

    let policy = local_only_contract_policy();
    policy.validate().expect("local-only policy must validate");
    assert_eq!(
        policy.decide(&RoutePrivacyContextV0 {
            target: RouteTargetV0::Local,
            data_tier: DataTierV0::Private,
            redaction: RedactionStateV0::NotApplied,
        }),
        PrivacyDecisionV0::Allowed
    );
    assert_eq!(
        policy.decide(&RoutePrivacyContextV0 {
            target: RouteTargetV0::Hosted {
                provider_id: ProviderId::new("hosted-fixture").expect("provider ID must validate"),
            },
            data_tier: DataTierV0::Private,
            redaction: RedactionStateV0::NotApplied,
        }),
        PrivacyDecisionV0::Denied(PrivacyDenialV0::LocalOnlyBoundary)
    );
}

#[test]
fn w1_contract_capability_envelope_is_derived_from_gateway_inventory() {
    let gateway = Gateway::new(GatewayDefaults::default());
    gateway
        .register_backend(Arc::new(SnapshotBackend {
            descriptor: descriptor("local-fixture", BackendLocation::LocalEmbedded),
            readiness: BackendReadiness::Ready,
        }))
        .expect("register local inventory fixture");
    gateway
        .register_backend(Arc::new(SnapshotBackend {
            descriptor: descriptor("hosted-fixture", BackendLocation::Hosted),
            readiness: BackendReadiness::NotConfigured {
                reason: "fixture credential is absent".to_owned(),
            },
        }))
        .expect("register hosted inventory fixture");

    let snapshot = capability_snapshot(&gateway);
    validate_capability_snapshot_v0(&snapshot).expect("exact capability envelope must validate");
    let entries = snapshot.services.values().flatten().collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| {
        entry.backend_or_resource_id == "hosted-fixture/hosted-fixture-model"
            && entry.readiness == Readiness::Unavailable
            && entry.privacy_eligible == TriState::No
    }));
}

#[test]
fn w1_contract_service_error_wraps_real_privacy_rejection() {
    let gateway = Gateway::new(GatewayDefaults::default());
    gateway
        .register_backend(Arc::new(SnapshotBackend {
            descriptor: descriptor("hosted-fixture", BackendLocation::Hosted),
            readiness: BackendReadiness::Ready,
        }))
        .expect("register hosted fixture");
    let error = gateway
        .resolve(&local_request())
        .err()
        .expect("local-only routing must reject the hosted fixture");
    assert_eq!(error.class, ErrorClass::Privacy);

    let contract_error = service_error(&error);
    validate_service_error_v0(&contract_error).expect("exact service error must validate");
    assert_eq!(contract_error.class, contracts::ErrorClass::Privacy);
    assert_eq!(contract_error.retry, RetryAdvice::Never);
}
