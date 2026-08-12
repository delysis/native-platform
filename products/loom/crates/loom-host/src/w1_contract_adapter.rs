use super::*;
use platform_contract_testkit::compositional_lifecycle::{
    WaiterControlAdapter, run_waiter_control_suite,
};
use platform_contract_testkit::contracts::privacy::PRIVACY_POLICY_SCHEMA_V0;
use platform_contract_testkit::contracts::{
    DataHandlingV0, DataTierV0, LoggingPolicyV0, NetworkPolicyV0, PayloadRedactionV0,
    PrivacyDecisionV0, PrivacyDenialV0, PrivacyPolicyV0, ProviderId, RedactionStateV0,
    RoutePrivacyContextV0, RouteTargetV0,
};
use platform_contract_testkit::{
    AcceptanceError, AttemptIdentity, LifecycleCoverageManifest, LifecycleImplementation,
    LifecycleInvariant, OperationPhase, OperationSnapshot, TerminalRecord, WaitObservation,
};

enum LoomInteractiveLifecycle {}

impl LifecycleImplementation for LoomInteractiveLifecycle {
    const PRODUCT: &'static str = "loom";
    const IMPLEMENTATION: &'static str = "interactive-generation-registry";
}

/// Test-only view of facts owned by Loom's production generation registry.
///
/// This deliberately does not implement `OperationModelAdapter`.
/// `GenerationRegistry` owns route reservation, cancellation routing and
/// release after durable terminal persistence. It does not own queue/running
/// transitions, terminal or progress publication, retained tasks, worker
/// joins, or a global quiesce phase. Supplying those fields here would create
/// a shadow lifecycle instead of testing Loom.
#[derive(Debug)]
struct LoomRegistryContractAdapter {
    registry: Arc<GenerationRegistry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoomRegistryFacts {
    active_branches: usize,
    session_active: bool,
}

impl LoomRegistryContractAdapter {
    fn new(max_active_branches: usize) -> Self {
        Self {
            registry: Arc::new(
                GenerationRegistry::new(max_active_branches)
                    .expect("contract test capacity is valid"),
            ),
        }
    }

    fn register(
        &self,
        identity: GenerationFamilyIdentity,
        branches: Vec<(GenerationRunId, BranchId)>,
        cancellation: Arc<dyn BranchCancellation>,
    ) -> Result<(), GenerationRegistryError> {
        self.registry.register(GenerationFamilyRegistration {
            identity,
            branches,
            cancellation,
        })
    }

    fn facts(
        &self,
        project_id: ProjectId,
        session_id: CommandId,
    ) -> Result<LoomRegistryFacts, GenerationRegistryError> {
        Ok(LoomRegistryFacts {
            active_branches: self.registry.active_branch_count()?,
            session_active: self.registry.has_active_session(project_id, session_id)?,
        })
    }
}

#[derive(Debug, Default)]
struct RecordingCancellation {
    branches: Mutex<Vec<BranchId>>,
}

impl RecordingCancellation {
    fn cancelled(&self) -> Vec<BranchId> {
        self.branches
            .lock()
            .expect("recording cancellation lock")
            .clone()
    }
}

impl BranchCancellation for RecordingCancellation {
    fn cancel_branch(&self, branch_id: BranchId) -> bool {
        self.branches
            .lock()
            .expect("recording cancellation lock")
            .push(branch_id);
        true
    }
}

#[derive(Clone, Debug)]
struct LoomWaiterAdapter {
    registry: Arc<GenerationRegistry>,
}

#[derive(Debug)]
struct LoomWaitTicket {
    project: ProjectId,
    session: CommandId,
    run: GenerationRunId,
}

#[derive(Debug)]
struct LoomWaitLease {
    request_id: String,
    run_id: GenerationRunId,
    branch_id: BranchId,
    cancellation: Arc<RecordingCancellation>,
}

impl LoomWaiterAdapter {
    fn attempt_identity(lease: &LoomWaitLease) -> AttemptIdentity {
        let bytes = lease.branch_id.as_ulid().to_bytes();
        AttemptIdentity {
            operation_id: lease.request_id.clone(),
            attempt_id: lease.branch_id.to_string(),
            sequence: u64::from_be_bytes(bytes[..8].try_into().expect("eight-byte prefix")),
        }
    }
}

impl WaiterControlAdapter for LoomWaiterAdapter {
    type Implementation = LoomInteractiveLifecycle;
    type Error = GenerationRegistryError;
    type Ticket = LoomWaitTicket;
    type Lease = LoomWaitLease;

    fn deterministic() -> Self {
        Self {
            registry: Arc::new(GenerationRegistry::new(1).expect("one live branch")),
        }
    }

    fn start(&self, operation_id: &str) -> Result<(Self::Ticket, Self::Lease), Self::Error> {
        let project_id = ProjectId::new();
        let session_id = CommandId::new();
        let run_id = GenerationRunId::new();
        let branch_id = BranchId::new();
        let cancellation = Arc::new(RecordingCancellation::default());
        let authority: Arc<dyn BranchCancellation> = cancellation.clone();
        self.registry.register(GenerationFamilyRegistration {
            identity: GenerationFamilyIdentity {
                request_id: operation_id.to_owned(),
                project_id,
                session_id,
                document_id: DocumentId::new(),
            },
            branches: vec![(run_id, branch_id)],
            cancellation: authority,
        })?;
        Ok((
            LoomWaitTicket {
                project: project_id,
                session: session_id,
                run: run_id,
            },
            LoomWaitLease {
                request_id: operation_id.to_owned(),
                run_id,
                branch_id,
                cancellation,
            },
        ))
    }

    fn snapshot(&self, lease: &Self::Lease) -> Option<OperationSnapshot> {
        let route = self.registry.route_for_run(lease.run_id).ok()??;
        if route.branch_id != lease.branch_id || route.identity.request_id != lease.request_id {
            return None;
        }
        Some(OperationSnapshot {
            identity: Self::attempt_identity(lease),
            phase: OperationPhase::Running,
            cancellation_requested: lease.cancellation.cancelled().contains(&lease.branch_id),
            authoritative_terminal: None::<TerminalRecord>,
            final_projection: None,
            progress_projection: Vec::new(),
        })
    }

    fn waiter_timeout(&self, ticket: &Self::Ticket) -> Result<WaitObservation, Self::Error> {
        if self.registry.route_for_run(ticket.run)?.is_none() {
            return Err(GenerationRegistryError::RunNotActive(ticket.run));
        }
        if self
            .registry
            .wait_for_session_idle(ticket.project, ticket.session, Duration::ZERO)?
        {
            return Err(GenerationRegistryError::CorruptRegistry);
        }
        Ok(WaitObservation::TimedOut)
    }

    fn request_cancel(&self, ticket: &Self::Ticket) -> Result<(), Self::Error> {
        if !self
            .registry
            .cancel_run(ticket.project, ticket.session, ticket.run)?
        {
            return Err(GenerationRegistryError::CorruptRegistry);
        }
        Ok(())
    }

    fn finish_cancelled(&self, lease: &Self::Lease) -> Result<(), Self::Error> {
        self.registry
            .complete_family(&lease.request_id)?
            .ok_or_else(|| GenerationRegistryError::RequestNotActive(lease.request_id.clone()))?;
        Ok(())
    }
}

#[test]
fn real_registry_satisfies_waiter_control_suite_without_shadow_lifecycle_state() {
    let evidence = run_waiter_control_suite::<LoomWaiterAdapter>("generation-registry");
    assert_eq!(evidence.product(), "loom");
    assert_eq!(evidence.implementation(), "interactive-generation-registry");
    assert_eq!(evidence.component(), "generation-registry");
    assert_eq!(evidence.suite(), "waiter-control");
    assert_eq!(
        evidence.invariants().collect::<Vec<_>>(),
        [LifecycleInvariant::WaiterTimeoutIsObservational]
    );
    assert!(matches!(
        LifecycleCoverageManifest::<LoomInteractiveLifecycle>::accept([evidence]),
        Err(AcceptanceError::MissingSuites(_))
    ));
}

fn local_only_contract(policy: &loom_types::BuildModelPolicy) -> PrivacyPolicyV0 {
    assert_eq!(
        policy.as_v1().inference_boundary(),
        loom_types::InferenceBoundary::LocalOnly,
        "Loom policy no longer has the local-only boundary this adapter binds"
    );
    assert_eq!(
        policy.as_v1().hosted_fallback(),
        loom_types::HostedFallback::Forbidden,
        "Loom policy unexpectedly permits hosted fallback"
    );
    PrivacyPolicyV0 {
        schema: PRIVACY_POLICY_SCHEMA_V0.to_owned(),
        network: NetworkPolicyV0::Deny,
        data_handling: DataHandlingV0::LocalOnly,
        allowed_provider_ids: Vec::new(),
        allowed_hosted_data_tiers: Vec::new(),
        payload_redaction: PayloadRedactionV0::LocalOnly,
        logging: LoggingPolicyV0::Disabled,
    }
}

#[test]
fn real_registry_reservation_cancellation_and_release_bind_owned_facts() {
    let adapter = LoomRegistryContractAdapter::new(2);
    let project_id = ProjectId::new();
    let session_id = CommandId::new();
    let run_id = GenerationRunId::new();
    let branch_id = BranchId::new();
    let identity = GenerationFamilyIdentity {
        request_id: "w1-loom-generation".to_owned(),
        project_id,
        session_id,
        document_id: DocumentId::new(),
    };
    let cancellation = Arc::new(RecordingCancellation::default());
    let authority: Arc<dyn BranchCancellation> = cancellation.clone();

    adapter
        .register(identity.clone(), vec![(run_id, branch_id)], authority)
        .expect("real registry admits one family");
    assert_eq!(
        adapter.facts(project_id, session_id).expect("owned facts"),
        LoomRegistryFacts {
            active_branches: 1,
            session_active: true,
        }
    );
    let route = adapter
        .registry
        .route_for_run(run_id)
        .expect("real route lookup")
        .expect("active route");
    assert_eq!(route.identity, identity);
    assert_eq!(route.run_id, run_id);
    assert_eq!(route.branch_id, branch_id);

    assert_eq!(
        adapter
            .register(
                identity.clone(),
                vec![(GenerationRunId::new(), BranchId::new())],
                Arc::new(RecordingCancellation::default()),
            )
            .expect_err("duplicate live request identity must fail"),
        GenerationRegistryError::DuplicateRequest(identity.request_id.clone())
    );
    assert_eq!(
        adapter
            .facts(project_id, session_id)
            .expect("no partial admission"),
        LoomRegistryFacts {
            active_branches: 1,
            session_active: true,
        }
    );

    assert_eq!(
        adapter
            .registry
            .cancel_session(project_id, session_id)
            .expect("route cancellation through production registry"),
        vec![run_id]
    );
    assert_eq!(cancellation.cancelled(), vec![branch_id]);
    assert_eq!(
        adapter.facts(project_id, session_id).expect("cancel facts"),
        LoomRegistryFacts {
            active_branches: 1,
            session_active: true,
        },
        "cancellation must not invent terminal persistence or release"
    );
    assert!(
        !adapter
            .registry
            .wait_for_session_idle(project_id, session_id, Duration::ZERO)
            .expect("bounded busy observation")
    );

    assert_eq!(
        adapter
            .registry
            .complete_family(&identity.request_id)
            .expect("release persisted family"),
        Some(identity)
    );
    assert_eq!(
        adapter
            .facts(project_id, session_id)
            .expect("released facts"),
        LoomRegistryFacts {
            active_branches: 0,
            session_active: false,
        }
    );
    assert!(
        adapter
            .registry
            .wait_for_session_idle(project_id, session_id, Duration::ZERO)
            .expect("idle after release")
    );
}

#[test]
fn real_registry_exposes_persistence_failure_without_forging_a_terminal() {
    let adapter = LoomRegistryContractAdapter::new(1);
    let project_id = ProjectId::new();
    let session_id = CommandId::new();
    let request_id = "w1-persistence-failure";
    let run_id = GenerationRunId::new();
    let branch_id = BranchId::new();
    adapter
        .registry
        .reserve(
            GenerationFamilyIdentity {
                request_id: request_id.to_owned(),
                project_id,
                session_id,
                document_id: DocumentId::new(),
            },
            vec![(run_id, branch_id)],
        )
        .expect("reserve before backend attachment");
    adapter
        .registry
        .mark_terminal_persistence_failure(request_id, "durable write failed")
        .expect("record production persistence failure");

    let failures = adapter
        .registry
        .terminal_persistence_failures(project_id, session_id)
        .expect("read production failure facts");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].identity.request_id, request_id);
    assert_eq!(failures[0].runs, vec![(run_id, branch_id)]);
    assert_eq!(failures[0].error, "durable write failed");
    assert!(
        !adapter
            .registry
            .wait_for_session_idle(project_id, session_id, Duration::from_secs(1))
            .expect("failure returns control to repair path")
    );
    assert_eq!(
        adapter
            .facts(project_id, session_id)
            .expect("failure facts"),
        LoomRegistryFacts {
            active_branches: 1,
            session_active: true,
        }
    );
}

#[test]
fn every_real_loom_model_policy_maps_to_the_local_only_privacy_contract() {
    for policy in [
        loom_types::BuildModelPolicy::none_v1(),
        loom_types::BuildModelPolicy::writer_gemma4_base_v1(),
        loom_types::BuildModelPolicy::writer_gemma4_base_v2(),
    ] {
        let contract = local_only_contract(&policy);
        contract.validate().expect("valid W1 local-only envelope");
        assert_eq!(contract.network, NetworkPolicyV0::Deny);
        assert_eq!(contract.data_handling, DataHandlingV0::LocalOnly);
        assert!(contract.allowed_provider_ids.is_empty());
        assert!(contract.allowed_hosted_data_tiers.is_empty());
        assert_eq!(contract.logging, LoggingPolicyV0::Disabled);

        assert_eq!(
            contract.decide(&RoutePrivacyContextV0 {
                target: RouteTargetV0::Local,
                data_tier: DataTierV0::Private,
                redaction: RedactionStateV0::NotApplied,
            }),
            PrivacyDecisionV0::Allowed
        );
        assert_eq!(
            contract.decide(&RoutePrivacyContextV0 {
                target: RouteTargetV0::Hosted {
                    provider_id: ProviderId::new("provider.unconfigured")
                        .expect("opaque provider ID"),
                },
                data_tier: DataTierV0::Public,
                redaction: RedactionStateV0::Applied,
            }),
            PrivacyDecisionV0::Denied(PrivacyDenialV0::LocalOnlyBoundary)
        );
        assert_eq!(
            contract.decide(&RoutePrivacyContextV0 {
                target: RouteTargetV0::Unknown,
                data_tier: DataTierV0::Public,
                redaction: RedactionStateV0::Unknown,
            }),
            PrivacyDecisionV0::Denied(PrivacyDenialV0::UnknownRoute)
        );
    }
}
