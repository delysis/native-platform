//! Wave 1 contract checks over the real gateway surfaces.
//!
//! The lifecycle model is richer than FTE's public gateway API. This adapter
//! therefore runs only the generic empty-shutdown assertion. It deliberately
//! fails if a future test tries to infer reservations, attempt identities, or
//! progress snapshots that the gateway does not expose.

use super::*;
use async_trait::async_trait;
use fte_types::{
    BackendReadiness, CachePolicy, ContentBlock, DeadlinePolicy, GenerationInput, InputItem,
    MessageRole, ModelCapabilities, PromptForm, RouteObservations, RoutingPolicy, SamplingOptions,
    StoragePolicy, StreamPolicy, ToolPolicy,
};
use platform_contract_testkit::contracts::{
    self, CapabilityEntryV0, CapabilitySnapshotV0, ContentDigest, DataHandlingV0, DataTierV0,
    LoggingPolicyV0, NetworkPolicyV0, OperationId, PayloadRedactionV0, PrivacyDecisionV0,
    PrivacyDenialV0, PrivacyPolicyV0, ProviderId, Readiness, RedactionStateV0, RetryAdvice,
    RoutePrivacyContextV0, RouteTargetV0, ServiceErrorV0, ServiceId, TriState,
};
use platform_contract_testkit::lifecycle_suite::assert_repeated_shutdown_is_stable_and_empty;
use platform_contract_testkit::{
    ClosedFacts, LifecyclePhase, OperationModelAdapter, OperationSnapshot, Reservation,
    TerminalClass, TestConfig, WaitObservation, validate_capability_snapshot_v0,
    validate_service_error_v0,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::MutexGuard;

#[derive(Debug)]
enum UnsupportedOperation {}

#[derive(Debug)]
struct UnsupportedTicket;

#[derive(Clone, Debug)]
struct UnsupportedLease;

#[derive(Clone)]
struct EmptyGatewayShutdownAdapter {
    gateway: Arc<Gateway>,
    runtime: Arc<Mutex<tokio::runtime::Runtime>>,
}

impl EmptyGatewayShutdownAdapter {
    fn runtime(&self) -> MutexGuard<'_, tokio::runtime::Runtime> {
        self.runtime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn unsupported<T>() -> T {
        panic!("the FTE Gateway does not expose the contract operation-model surface")
    }
}

impl OperationModelAdapter for EmptyGatewayShutdownAdapter {
    type Error = UnsupportedOperation;
    type Ticket = UnsupportedTicket;
    type Lease = UnsupportedLease;

    fn deterministic(config: TestConfig) -> Self {
        assert_eq!(config, TestConfig::default());
        let runtime = tokio::runtime::Runtime::new().expect("construct contract-test runtime");
        Self {
            gateway: Arc::new(Gateway::new(GatewayDefaults::default())),
            runtime: Arc::new(Mutex::new(runtime)),
        }
    }

    fn reserve(
        &self,
        _operation_id: &str,
    ) -> Result<Reservation<Self::Ticket, Self::Lease>, Self::Error> {
        Self::unsupported()
    }

    fn ticket_identity(
        &self,
        _ticket: &Self::Ticket,
    ) -> platform_contract_testkit::AttemptIdentity {
        Self::unsupported()
    }

    fn lease_identity(&self, _lease: &Self::Lease) -> platform_contract_testkit::AttemptIdentity {
        Self::unsupported()
    }

    fn queue(&self, _lease: &Self::Lease) -> Result<(), Self::Error> {
        Self::unsupported()
    }

    fn start(&self, _lease: &Self::Lease) -> Result<(), Self::Error> {
        Self::unsupported()
    }

    fn publish_progress(&self, _lease: &Self::Lease, _sequence: u64) -> Result<(), Self::Error> {
        Self::unsupported()
    }

    fn request_cancel(&self, _ticket: &Self::Ticket) -> Result<(), Self::Error> {
        Self::unsupported()
    }

    fn consumer_drop(&self, _ticket: Self::Ticket) -> Result<(), Self::Error> {
        Self::unsupported()
    }

    fn waiter_timeout(&self, _ticket: &Self::Ticket) -> Result<WaitObservation, Self::Error> {
        Self::unsupported()
    }

    fn terminal(&self, _lease: &Self::Lease, _terminal: TerminalClass) -> Result<(), Self::Error> {
        Self::unsupported()
    }

    fn record_executor_panic(&self, _lease: &Self::Lease) -> Result<(), Self::Error> {
        Self::unsupported()
    }

    fn release(&self, _lease: &Self::Lease) -> Result<(), Self::Error> {
        Self::unsupported()
    }

    fn quiesce(&self) {
        Self::unsupported()
    }

    fn shutdown(&self) -> ClosedFacts {
        self.runtime()
            .block_on(self.gateway.shutdown())
            .expect("empty gateway shutdown must succeed");
        let status = self.gateway.status();
        ClosedFacts {
            lifecycle: lifecycle_phase(status.lifecycle),
            active_operations: status.active_requests,
            retained_tasks: 0,
            joined_workers: 0,
        }
    }

    fn lifecycle_phase(&self) -> LifecyclePhase {
        lifecycle_phase(self.gateway.status().lifecycle)
    }

    fn active_count(&self) -> usize {
        self.gateway.status().active_requests
    }

    fn retained_task_count(&self) -> usize {
        0
    }

    fn progress_capacity(&self) -> usize {
        fte_types::DEFAULT_EVENT_CAPACITY
    }

    fn current_snapshot(&self, _operation_id: &str) -> Option<OperationSnapshot> {
        None
    }

    fn lease_snapshot(&self, _lease: &Self::Lease) -> Option<OperationSnapshot> {
        None
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
fn w1_contract_generic_shutdown_subset_uses_real_gateway() {
    assert_repeated_shutdown_is_stable_and_empty::<EmptyGatewayShutdownAdapter>();
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
