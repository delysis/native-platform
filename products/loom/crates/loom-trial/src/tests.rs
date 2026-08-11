use loom_inference::BaseWriterBinding;
use loom_research_types::{
    CallScope, CampaignId, CompiledBaseCompletionPrompt, CompiledManifest,
    CompletionPromptBlockRole, CompletionPromptTail, ExactPromptBlockBytes, ExactPromptSource,
    FrozenBaseCompletionPrompt, FrozenCompletionPromptBlock, FrozenStageSpec, FrozenTrialStage,
    ManifestDocument, NonEmptyByteRange, PromptBlockWitness, PromptSourceRange, PromptTopology,
    StageAttemptId, StageGraph, StageGraphId, StageId, TrialCaseId, TrialRunId, TrialRunOrigin,
    TrialRunRecord, compile_manifest,
};
use loom_store::{
    FrozenCampaignPersistence, FrozenStagePersistence, FrozenTrialPersistence, ProjectStore,
    ResearchBudgetMaximum, StandaloneTrialRunPersistence, StoreError,
};
use loom_types::{BlobId, ProjectId, RevisionId};
use tempfile::tempdir;

use super::*;

const MODEL_HASH: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TOKENIZER_HASH: &str = "2222222222222222222222222222222222222222222222222222222222222222";

type TopologyShape<'a> = (
    PromptTopology,
    &'a str,
    &'a [(CompletionPromptBlockRole, bool)],
);

struct Fixture {
    project_id: ProjectId,
    campaign_id: CampaignId,
    case_id: TrialCaseId,
    campaign: CompiledManifest,
    binding: BaseWriterBinding,
    graph: StageGraph,
    prompt: CompiledBaseCompletionPrompt,
    budget: TrialBudgetLimits,
}

impl Fixture {
    fn new() -> Self {
        Self::new_for_project(ProjectId::new())
    }

    fn new_for_project(project_id: ProjectId) -> Self {
        let campaign_id = CampaignId::new();
        let case_id = TrialCaseId::new();
        let graph = stage_graph();
        let binding_manifest =
            compile_manifest(model_manifest().as_bytes()).expect("model manifest");
        let binding = BaseWriterBinding::compile(&binding_manifest, "writer").expect("binding");
        let campaign_source = campaign_manifest(binding_manifest.artifact_hash().as_blob_id(), "");
        let campaign = compile_manifest(campaign_source.as_bytes()).expect("campaign");
        let treatment_fingerprint = match campaign.document() {
            ManifestDocument::Campaign(campaign) => {
                fingerprint_treatment(&campaign.treatments()[0])
            }
            _ => panic!("campaign fixture"),
        };
        let generate_stage_id = graph
            .stages()
            .iter()
            .find(|stage| stage.stage() == FrozenTrialStage::Generate)
            .expect("generate stage")
            .id();
        let prompt = compiled_prompt(
            project_id,
            campaign_id,
            case_id,
            generate_stage_id,
            treatment_fingerprint,
        );
        Self {
            project_id,
            campaign_id,
            case_id,
            campaign,
            binding,
            graph,
            prompt,
            budget: TrialBudgetLimits::new(100, 100, 10, 1_000).expect("budget"),
        }
    }

    fn inputs(&self) -> FrozenTrialInputs<'_> {
        let treatment = match self.campaign.document() {
            ManifestDocument::Campaign(campaign) => &campaign.treatments()[0],
            _ => panic!("campaign fixture"),
        };
        FrozenTrialInputs {
            project_id: self.project_id,
            project_input_fingerprint: BlobId::digest(b"frozen project state"),
            campaign_id: self.campaign_id,
            case_id: self.case_id,
            campaign_manifest: &self.campaign,
            campaign_case_key: "case-01",
            treatment_key: "direct",
            stage_graph: &self.graph,
            compiled_prompt: &self.prompt,
            prompt_topology_lease: PromptTopologyVerifier::verify(treatment, &self.prompt)
                .expect("verified prompt topology"),
            model_binding: &self.binding,
            stage_budget_maxima: stage_budget_maxima(
                &self.graph,
                32,
                PromptTopology::ExactDirectContinuation,
            ),
            budget: self.budget,
        }
    }

    fn spec(&self) -> FrozenTrialSpec {
        FrozenTrialSpec::compile(self.inputs()).expect("frozen trial")
    }
}

#[test]
fn store_session_binds_trial_subject_record_project_and_live_exclusivity() {
    let directory = tempdir().expect("temporary project");
    let (mut store, _) = ProjectStore::initialize(directory.path(), "trial lease")
        .expect("initialize project store");
    let fixture = Fixture::new_for_project(store.manifest().project_id);
    let spec = fixture.spec();
    let canonical = spec
        .canonical_record_bytes()
        .expect("canonical trial record");
    let trial_run_id = persist_trial(
        &mut store,
        &spec,
        &fixture.graph,
        spec.fingerprint(),
        &canonical,
    );
    let lease = store
        .acquire_trial_run_session(trial_run_id)
        .expect("matching trial lease");
    let lease_fingerprint = lease.lease_fingerprint();
    let journal = TrialJournal::new(spec.clone(), fixture.graph.clone(), &store, lease)
        .expect("live trial journal");
    assert_eq!(journal.store_lease_fingerprint(), lease_fingerprint);
    assert!(matches!(
        store.acquire_trial_run_session(trial_run_id),
        Err(StoreError::ResearchSessionAlreadyActive { .. })
    ));
    drop(journal);
    let reacquired = store
        .acquire_trial_run_session(trial_run_id)
        .expect("journal drop releases exact trial");
    drop(reacquired);
}

#[test]
fn durable_trial_resume_releases_reserved_attempt_before_retry_authority() {
    let directory = tempdir().expect("temporary project");
    let project_path = directory.path().to_path_buf();
    let (mut store, _) =
        ProjectStore::initialize(&project_path, "reserved recovery").expect("project store");
    let fixture = Fixture::new_for_project(store.manifest().project_id);
    let spec = fixture.spec();
    let canonical = spec.canonical_record_bytes().expect("canonical trial");
    let trial_run_id = persist_trial(
        &mut store,
        &spec,
        &fixture.graph,
        spec.fingerprint(),
        &canonical,
    );
    let lease = store
        .acquire_trial_run_session(trial_run_id)
        .expect("initial lease");
    let first_lease = lease.lease_fingerprint();
    let mut journal = TrialJournal::new(spec.clone(), fixture.graph.clone(), &store, lease)
        .expect("durable trial");
    let stage = &fixture.graph.stages()[0];
    let first_attempt = StageAttemptId::new();
    let reservation = spec
        .stage_budget_maximum(stage.id())
        .expect("stage maximum");
    let permit = journal
        .reserve_stage(stage.id(), first_attempt, reservation)
        .expect("durable reservation");
    drop(permit);
    drop(journal);
    drop(store);

    let reopened = ProjectStore::open(&project_path).expect("reopen project");
    let lease = reopened
        .acquire_trial_run_session(trial_run_id)
        .expect("fresh lease");
    assert_ne!(lease.lease_fingerprint(), first_lease);
    let mut resumed = TrialJournal::resume(spec.clone(), fixture.graph.clone(), &reopened, lease)
        .expect("resume and release pre-crash reservation");
    assert_eq!(
        resumed.attempt_status(first_attempt),
        Some(StageAttemptStatus::Abandoned)
    );
    assert_eq!(resumed.snapshot().active_attempt_count(), 0);
    assert_eq!(
        resumed.snapshot().budget().reserved(),
        BudgetAmount::default()
    );
    assert_eq!(
        resumed.snapshot().budget().charged(),
        BudgetAmount::default()
    );

    let retry = StageAttemptId::new();
    let _retry_permit = resumed
        .reserve_stage(stage.id(), retry, reservation)
        .expect("retry after durable abandonment");
    assert!(matches!(
        resumed.events().last().expect("retry event").kind(),
        TrialEventKind::AttemptReserved {
            attempt_ordinal: 2,
            ..
        }
    ));
}

#[test]
fn durable_trial_resume_interrupts_running_attempt_at_full_charge() {
    let directory = tempdir().expect("temporary project");
    let project_path = directory.path().to_path_buf();
    let (mut store, _) =
        ProjectStore::initialize(&project_path, "running recovery").expect("project store");
    let fixture = Fixture::new_for_project(store.manifest().project_id);
    let spec = fixture.spec();
    let canonical = spec.canonical_record_bytes().expect("canonical trial");
    let trial_run_id = persist_trial(
        &mut store,
        &spec,
        &fixture.graph,
        spec.fingerprint(),
        &canonical,
    );
    let lease = store
        .acquire_trial_run_session(trial_run_id)
        .expect("initial lease");
    let mut journal = TrialJournal::new(spec.clone(), fixture.graph.clone(), &store, lease)
        .expect("durable trial");
    let stage = &fixture.graph.stages()[0];
    let attempt = StageAttemptId::new();
    let reservation = spec
        .stage_budget_maximum(stage.id())
        .expect("stage maximum");
    let permit = journal
        .reserve_stage(stage.id(), attempt, reservation)
        .expect("reserve");
    let command = journal.start_reserved(permit).expect("durable start");
    drop(command);
    drop(journal);
    drop(store);

    let reopened = ProjectStore::open(&project_path).expect("reopen project");
    let lease = reopened
        .acquire_trial_run_session(trial_run_id)
        .expect("fresh lease");
    let resumed = TrialJournal::resume(spec, fixture.graph, &reopened, lease)
        .expect("resume and interrupt pre-crash call");
    assert_eq!(
        resumed.attempt_status(attempt),
        Some(StageAttemptStatus::Interrupted)
    );
    assert_eq!(resumed.snapshot().active_attempt_count(), 0);
    assert_eq!(
        resumed.snapshot().budget().reserved(),
        BudgetAmount::default()
    );
    assert_eq!(resumed.snapshot().budget().charged(), reservation);
    assert!(matches!(
        resumed.events().last().expect("recovery event").kind(),
        TrialEventKind::AttemptFinished {
            terminal: AttemptTerminal::Interrupted { .. },
            actual_charge,
            terminal_evidence: StageTerminalEvidence::ConservativeInterruption { .. },
            ..
        } if actual_charge == reservation
    ));
}

#[test]
fn trial_session_rejects_relabel_fake_record_and_cross_project_laundering() {
    let directory = tempdir().expect("temporary project");
    let (mut store, _) =
        ProjectStore::initialize(directory.path(), "relabel").expect("project store");
    let fixture = Fixture::new_for_project(store.manifest().project_id);
    let spec = fixture.spec();
    let canonical = spec
        .canonical_record_bytes()
        .expect("canonical trial record");
    let relabeled = BlobId::digest(b"relabeled trial subject");
    let relabeled_run_id = persist_trial(&mut store, &spec, &fixture.graph, relabeled, &canonical);
    let lease = store
        .acquire_trial_run_session(relabeled_run_id)
        .expect("persisted relabel claim");
    assert!(matches!(
        TrialJournal::new(spec.clone(), fixture.graph.clone(), &store, lease),
        Err(TrialError::SessionNormalizedSnapshotMismatch)
    ));

    let directory = tempdir().expect("temporary project");
    let (mut store, _) =
        ProjectStore::initialize(directory.path(), "fake record").expect("project store");
    let fixture = Fixture::new_for_project(store.manifest().project_id);
    let spec = fixture.spec();
    let fake_record_run_id = persist_trial(
        &mut store,
        &spec,
        &fixture.graph,
        spec.fingerprint(),
        b"fake trial canonical record",
    );
    let lease = store
        .acquire_trial_run_session(fake_record_run_id)
        .expect("persisted fake record claim");
    assert!(matches!(
        TrialJournal::new(spec, fixture.graph, &store, lease),
        Err(TrialError::SessionNormalizedSnapshotMismatch)
    ));

    let directory = tempdir().expect("temporary project");
    let (mut store, _) =
        ProjectStore::initialize(directory.path(), "wrong project").expect("project store");
    let fixture = Fixture::new();
    let spec = fixture.spec();
    assert_ne!(spec.project_id(), store.manifest().project_id);
    let canonical = spec
        .canonical_record_bytes()
        .expect("canonical trial record");
    let cross_project_run_id = persist_trial(
        &mut store,
        &spec,
        &fixture.graph,
        spec.fingerprint(),
        &canonical,
    );
    let lease = store
        .acquire_trial_run_session(cross_project_run_id)
        .expect("cross-project persisted claim");
    assert!(matches!(
        TrialJournal::new(spec, fixture.graph, &store, lease),
        Err(TrialError::SessionLeaseMismatch)
    ));
}

#[test]
fn trial_session_rejects_exact_record_with_laundered_stage_rows() {
    for laundering in [
        StageLaundering::Maximum,
        StageLaundering::IdentityAndDependencies,
    ] {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) =
            ProjectStore::initialize(directory.path(), "stage laundering").expect("project store");
        let fixture = Fixture::new_for_project(store.manifest().project_id);
        let spec = fixture.spec();
        let canonical = spec
            .canonical_record_bytes()
            .expect("canonical trial record");
        let trial_run_id = persist_trial_with_stage_laundering(
            &mut store,
            &spec,
            &fixture.graph,
            spec.fingerprint(),
            &canonical,
            laundering,
        );
        let lease = store
            .acquire_trial_run_session(trial_run_id)
            .expect("persisted laundered normalized rows");
        assert!(matches!(
            TrialJournal::new(spec, fixture.graph, &store, lease),
            Err(TrialError::SessionNormalizedSnapshotMismatch)
        ));
    }
}

#[test]
fn spec_binds_exact_campaign_prompt_treatment_graph_model_and_budget() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let rebuilt = fixture.spec();

    assert_eq!(spec, rebuilt);
    assert_eq!(spec.project_id(), fixture.project_id);
    assert_eq!(
        spec.project_input_fingerprint(),
        BlobId::digest(b"frozen project state")
    );
    assert_eq!(spec.campaign_id(), fixture.campaign_id);
    assert_eq!(spec.case_id(), fixture.case_id);
    assert_eq!(
        spec.prompt_content_fingerprint(),
        fixture.prompt.content_fingerprint()
    );
    assert_eq!(
        spec.exact_prompt_blob_id(),
        BlobId::digest(fixture.prompt.exact_bytes())
    );
    assert_eq!(
        spec.model_binding_fingerprint(),
        fixture.binding.fingerprint()
    );
    assert_eq!(
        spec.stage_graph_fingerprint(),
        fingerprint_stage_graph(&fixture.graph)
    );
    assert_eq!(spec.declared_writer_token_maximum(), 32);
    assert_eq!(spec.expected_writer_call_count(), 2);
    assert!(matches!(
        crate::runtime::verify_single_writer_call_spec(&spec),
        Err(TrialRuntimeError::UnsupportedWriterCallCount(2))
    ));
    assert_eq!(spec.budget(), fixture.budget);

    let debug = format!("{spec:?}");
    assert!(!debug.contains("private opening paragraph"));
}

#[test]
fn frozen_trial_identity_is_stable_across_fresh_prompt_attempts() {
    let fixture = Fixture::new();
    let original = fixture.spec();
    let rebound = rebind_direct_prompt_attempt(&fixture.prompt, StageAttemptId::new());
    assert_ne!(fixture.prompt.fingerprint(), rebound.fingerprint());
    assert_eq!(
        fixture.prompt.content_fingerprint(),
        rebound.content_fingerprint()
    );

    let treatment = match fixture.campaign.document() {
        ManifestDocument::Campaign(campaign) => &campaign.treatments()[0],
        _ => panic!("campaign fixture"),
    };
    let mut inputs = fixture.inputs();
    inputs.compiled_prompt = &rebound;
    inputs.prompt_topology_lease =
        PromptTopologyVerifier::verify(treatment, &rebound).expect("rebound direct topology");
    let retried = FrozenTrialSpec::compile(inputs).expect("same frozen content trial");

    assert_eq!(retried, original);
    assert_eq!(retried.fingerprint(), original.fingerprint());
}

#[test]
fn spec_rejects_cross_case_cross_campaign_and_underfunded_substitution() {
    let fixture = Fixture::new();
    let mut inputs = fixture.inputs();
    inputs.case_id = TrialCaseId::new();
    assert_eq!(
        FrozenTrialSpec::compile(inputs),
        Err(TrialSpecError::PromptCaseMismatch)
    );

    let mut inputs = fixture.inputs();
    inputs.campaign_id = CampaignId::new();
    assert_eq!(
        FrozenTrialSpec::compile(inputs),
        Err(TrialSpecError::PromptCampaignMismatch)
    );

    let mut inputs = fixture.inputs();
    inputs.budget = TrialBudgetLimits::new(31, 100, 10, 1_000).expect("bounded budget");
    assert_eq!(
        FrozenTrialSpec::compile(inputs),
        Err(TrialSpecError::InsufficientTrialWriterBudget {
            required: 32,
            available: 31,
        })
    );
}

#[test]
fn frozen_stage_maxima_require_exact_graph_coverage_valid_shapes_and_total_budget() {
    let fixture = Fixture::new();

    let mut inputs = fixture.inputs();
    inputs
        .stage_budget_maxima
        .push(inputs.stage_budget_maxima[0]);
    assert_eq!(
        FrozenTrialSpec::compile(inputs),
        Err(TrialSpecError::DuplicateStageBudget(
            fixture.graph.stages()[0].id()
        ))
    );

    let mut inputs = fixture.inputs();
    let missing = inputs.stage_budget_maxima.pop().expect("stage maximum");
    assert_eq!(
        FrozenTrialSpec::compile(inputs),
        Err(TrialSpecError::MissingStageBudget(missing.stage_id()))
    );

    let mut inputs = fixture.inputs();
    let unknown = StageId::new();
    inputs
        .stage_budget_maxima
        .push(FrozenStageBudgetMaximum::new(
            unknown,
            BudgetAmount::new(0, 0, 0, 1).expect("bounded maximum"),
        ));
    assert_eq!(
        FrozenTrialSpec::compile(inputs),
        Err(TrialSpecError::UnknownStageBudget(unknown))
    );

    let mut inputs = fixture.inputs();
    let generate = fixture
        .graph
        .stages()
        .iter()
        .find(|stage| stage.stage() == FrozenTrialStage::Generate)
        .expect("Generate stage");
    let maximum = inputs
        .stage_budget_maxima
        .iter_mut()
        .find(|entry| entry.stage_id() == generate.id())
        .expect("Generate maximum");
    *maximum = FrozenStageBudgetMaximum::new(
        generate.id(),
        BudgetAmount::new(31, 0, 0, 100).expect("bounded maximum"),
    );
    assert_eq!(
        FrozenTrialSpec::compile(inputs),
        Err(TrialSpecError::InvalidStageBudget(
            FrozenTrialStage::Generate
        ))
    );

    let mut inputs = fixture.inputs();
    inputs.budget = TrialBudgetLimits::new(100, 100, 10, 299).expect("bounded trial budget");
    assert_eq!(
        FrozenTrialSpec::compile(inputs),
        Err(TrialSpecError::StageBudgetsExceedTrial)
    );
}

#[test]
fn reservation_must_equal_the_frozen_stage_maximum_before_state_changes() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let stage = fixture.graph.stages()[0].clone();
    let mut journal = live_journal(spec.clone(), fixture.graph);
    let before = journal.snapshot();
    let attempt_id = StageAttemptId::new();

    for wall_time_ms in [9, 11] {
        let substituted = BudgetAmount::new(0, 0, 0, wall_time_ms).expect("bounded maximum");
        assert!(matches!(
            journal.reserve_stage(stage.id(), attempt_id, substituted),
            Err(TrialError::InvalidReservation(rejected)) if rejected == stage.stage()
        ));
        assert_eq!(journal.snapshot(), before);
        assert!(journal.attempt_status(attempt_id).is_none());
    }

    let exact = spec
        .stage_budget_maximum(stage.id())
        .expect("frozen stage maximum");
    let permit = journal
        .reserve_stage(stage.id(), attempt_id, exact)
        .expect("exact maximum reserves");
    assert_eq!(permit.attempt_id(), attempt_id);
    assert_eq!(
        journal.attempt_status(attempt_id),
        Some(StageAttemptStatus::Reserved)
    );
}

#[test]
fn deterministic_verifier_issues_six_topologies_and_keeps_graph_raw_closed() {
    let fixture = Fixture::new();
    let variants: &[TopologyShape<'_>] = &[
        (
            PromptTopology::ExactDirectContinuation,
            "exact_direct_continuation",
            &[],
        ),
        (
            PromptTopology::NaturalBookfrontContinuation,
            "natural_bookfront_continuation",
            &[
                (CompletionPromptBlockRole::Bookfront, false),
                (CompletionPromptBlockRole::Bookfront, false),
            ],
        ),
        (
            PromptTopology::EventLedgerOperatorPair,
            "event_ledger_operator_pair",
            &[
                (CompletionPromptBlockRole::OperatorDemonstration, true),
                (CompletionPromptBlockRole::OperatorDemonstration, false),
            ],
        ),
        (
            PromptTopology::NearestProjectAnchor,
            "nearest_project_anchor",
            &[(CompletionPromptBlockRole::ProjectAnchor, false)],
        ),
        (
            PromptTopology::RawSceneApprenticeship,
            "raw_scene_apprenticeship",
            &[
                (CompletionPromptBlockRole::SourceApprenticeship, false),
                (CompletionPromptBlockRole::SourceApprenticeship, false),
            ],
        ),
        (
            PromptTopology::StagedMovementAssembly,
            "staged_movement_assembly",
            &[
                (CompletionPromptBlockRole::StoryState, true),
                (CompletionPromptBlockRole::MovementContract, true),
            ],
        ),
    ];
    for (declared_topology, manifest_name, shape) in variants {
        let model_artifact = fixture.binding.manifest_fingerprint().as_blob_id();
        let source = campaign_manifest_with_topology(model_artifact, "", manifest_name);
        let campaign = compile_manifest(source.as_bytes()).expect("topology campaign");
        let treatment = match campaign.document() {
            ManifestDocument::Campaign(campaign) => &campaign.treatments()[0],
            _ => panic!("campaign fixture"),
        };
        let prompt = compiled_prompt_with_shape(&fixture, fingerprint_treatment(treatment), shape);
        let _ = PromptTopologyVerifier::verify(treatment, &prompt)
            .unwrap_or_else(|error| panic!("{declared_topology:?} failed: {error}"));
    }

    let source = campaign_manifest_with_topology(
        fixture.binding.manifest_fingerprint().as_blob_id(),
        "",
        "graph_raw_paired_apprenticeship",
    );
    let campaign = compile_manifest(source.as_bytes()).expect("graph campaign");
    let treatment = match campaign.document() {
        ManifestDocument::Campaign(campaign) => &campaign.treatments()[0],
        _ => panic!("campaign fixture"),
    };
    let prompt = compiled_prompt_with_shape(
        &fixture,
        fingerprint_treatment(treatment),
        &[
            (CompletionPromptBlockRole::StoryState, true),
            (CompletionPromptBlockRole::SourceApprenticeship, false),
        ],
    );
    assert!(matches!(
        PromptTopologyVerifier::verify(treatment, &prompt),
        Err(PromptTopologyVerificationError::AcceptedBacktranslationRequired)
    ));
    let accepted = AcceptedBacktranslationDemonstrationLease::diagnostic_for_tests(
        fingerprint_treatment(treatment),
        prompt.content_fingerprint(),
    );
    let _ = PromptTopologyVerifier::verify_graph_raw(treatment, &prompt, accepted)
        .expect("accepted graph-to-raw topology");
}

#[test]
fn every_topology_rejects_a_treatment_bound_prompt_with_the_wrong_shape() {
    let fixture = Fixture::new();
    let variants = [
        (
            PromptTopology::ExactDirectContinuation,
            "exact_direct_continuation",
        ),
        (
            PromptTopology::NaturalBookfrontContinuation,
            "natural_bookfront_continuation",
        ),
        (
            PromptTopology::EventLedgerOperatorPair,
            "event_ledger_operator_pair",
        ),
        (
            PromptTopology::NearestProjectAnchor,
            "nearest_project_anchor",
        ),
        (
            PromptTopology::RawSceneApprenticeship,
            "raw_scene_apprenticeship",
        ),
        (
            PromptTopology::GraphRawPairedApprenticeship,
            "graph_raw_paired_apprenticeship",
        ),
        (
            PromptTopology::StagedMovementAssembly,
            "staged_movement_assembly",
        ),
    ];
    for (topology, manifest_name) in variants {
        let source = campaign_manifest_with_topology(
            fixture.binding.manifest_fingerprint().as_blob_id(),
            "",
            manifest_name,
        );
        let campaign = compile_manifest(source.as_bytes()).expect("shape campaign");
        let treatment = match campaign.document() {
            ManifestDocument::Campaign(campaign) => &campaign.treatments()[0],
            _ => panic!("campaign fixture"),
        };
        let wrong_shape: &[(CompletionPromptBlockRole, bool)] =
            if topology == PromptTopology::ExactDirectContinuation {
                &[(CompletionPromptBlockRole::SourceApprenticeship, false)]
            } else {
                &[]
            };
        let prompt =
            compiled_prompt_with_shape(&fixture, fingerprint_treatment(treatment), wrong_shape);
        let error = if topology == PromptTopology::GraphRawPairedApprenticeship {
            let accepted = AcceptedBacktranslationDemonstrationLease::diagnostic_for_tests(
                fingerprint_treatment(treatment),
                prompt.content_fingerprint(),
            );
            PromptTopologyVerifier::verify_graph_raw(treatment, &prompt, accepted)
                .expect_err("wrong graph shape")
        } else {
            PromptTopologyVerifier::verify(treatment, &prompt).expect_err("wrong prompt shape")
        };
        assert_eq!(
            error,
            PromptTopologyVerificationError::ShapeMismatch(topology)
        );
    }
}

#[test]
fn topology_verifier_rejects_relabel_without_recompiling_the_prompt() {
    let fixture = Fixture::new();
    let source = campaign_manifest_with_topology(
        fixture.binding.manifest_fingerprint().as_blob_id(),
        "",
        "natural_bookfront_continuation",
    );
    let campaign = compile_manifest(source.as_bytes()).expect("relabel campaign");
    let relabeled = match campaign.document() {
        ManifestDocument::Campaign(campaign) => &campaign.treatments()[0],
        _ => panic!("campaign fixture"),
    };
    assert!(matches!(
        PromptTopologyVerifier::verify(relabeled, &fixture.prompt),
        Err(PromptTopologyVerificationError::TreatmentMismatch)
    ));
}

#[test]
fn exact_manifest_source_occurrence_changes_trial_even_when_semantics_match() {
    let fixture = Fixture::new();
    let model_artifact = fixture.binding.manifest_fingerprint().as_blob_id();
    let alternate_source = campaign_manifest(model_artifact, "# retained operator note\n");
    let alternate = compile_manifest(alternate_source.as_bytes()).expect("alternate campaign");
    assert_eq!(alternate.artifact_hash(), fixture.campaign.artifact_hash());
    assert_ne!(alternate.source_hash(), fixture.campaign.source_hash());

    let mut inputs = fixture.inputs();
    inputs.campaign_manifest = &alternate;
    let alternate_spec = FrozenTrialSpec::compile(inputs).expect("alternate spec");
    assert_ne!(alternate_spec.fingerprint(), fixture.spec().fingerprint());
    assert_eq!(
        alternate_spec.campaign_manifest_fingerprint(),
        fixture.spec().campaign_manifest_fingerprint()
    );
}

#[test]
fn complete_fixed_graph_replays_to_the_same_state() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let stages = fixture.graph.stages().to_vec();
    let mut journal = live_journal(spec.clone(), fixture.graph.clone());

    for stage in stages {
        let attempt_id = StageAttemptId::new();
        let (reservation, charge) = budgets_for(stage.stage(), &spec);
        let permit = journal
            .reserve_stage(stage.id(), attempt_id, reservation)
            .expect("reserve in dependency order");
        let command = journal.start_reserved(permit).expect("start");
        assert_eq!(command.stage_spec_fingerprint(), stage.spec_fingerprint());
        if stage.stage() == FrozenTrialStage::Generate {
            assert_eq!(
                command.prompt_content_fingerprint(),
                Some(spec.prompt_content_fingerprint())
            );
            assert_eq!(
                command.model_binding_fingerprint(),
                Some(spec.model_binding_fingerprint())
            );
        }
        let output = expected_output(&spec, stage.stage(), stage.id());
        let terminal = success_lease(&command, output, charge);
        journal.finish(command, terminal).expect("finish");
    }
    let completion = journal
        .completion_lease_for_tests(BlobId::digest(b"durable archive and current head"))
        .expect("completion lease");
    let completed = journal.complete(completion).expect("close complete");
    assert_eq!(journal.snapshot().status(), TrialStatus::Completed);
    assert_eq!(journal.snapshot().successful_stage_count(), 12);
    assert_eq!(journal.snapshot().active_attempt_count(), 0);

    let encoded = serde_json::to_vec(journal.events()).expect("event JSON");
    assert!(!String::from_utf8_lossy(&encoded).contains("private opening paragraph"));
    assert_eq!(completed.trial_fingerprint(), spec.fingerprint());
    assert_eq!(
        completed.trial_journal_fingerprint(),
        journal.snapshot().last_event_fingerprint()
    );
    let events: TrialEventBatch = serde_json::from_slice(&encoded).expect("bounded events");
    let replayed =
        TrialJournal::replay_batch(&spec, &fixture.graph, &events).expect("strict replay");
    assert_eq!(replayed.snapshot(), journal.snapshot());
}

#[test]
fn retry_is_a_new_attempt_and_reusing_an_attempt_id_fails() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let first_stage = fixture.graph.stages()[0].clone();
    let second_stage = fixture.graph.stages()[1].clone();
    let mut journal = live_journal(spec.clone(), fixture.graph);
    succeed_stage(&mut journal, &spec, &first_stage);

    let first_attempt = StageAttemptId::new();
    let reservation = spec
        .stage_budget_maximum(second_stage.id())
        .expect("second-stage maximum");
    let first_permit = journal
        .reserve_stage(second_stage.id(), first_attempt, reservation)
        .expect("first attempt");
    let command = journal.start_reserved(first_permit).expect("start");
    let terminal = failed_lease(
        &command,
        BlobId::digest(b"first failure"),
        BudgetAmount::new(0, 0, 0, 2).expect("charge"),
    );
    journal.finish(command, terminal).expect("failed terminal");
    assert_eq!(
        journal
            .reserve_stage(second_stage.id(), first_attempt, reservation)
            .map(|_| ()),
        Err(TrialError::DuplicateAttemptId(first_attempt))
    );

    let retry_attempt = StageAttemptId::new();
    let retry_permit = journal
        .reserve_stage(second_stage.id(), retry_attempt, reservation)
        .expect("new retry");
    let retry = journal.start_reserved(retry_permit).expect("retry start");
    assert_eq!(retry.attempt_ordinal(), 2);
    let terminal = success_lease(
        &retry,
        BlobId::digest(b"retry output"),
        BudgetAmount::new(0, 0, 0, 3).expect("charge"),
    );
    journal.finish(retry, terminal).expect("retry success");
    assert_eq!(
        journal.attempt_status(first_attempt),
        Some(StageAttemptStatus::Failed)
    );
    assert_eq!(
        journal.attempt_status(retry_attempt),
        Some(StageAttemptStatus::Succeeded)
    );
}

#[test]
fn generate_retry_uses_a_fresh_attempt_inside_the_same_frozen_trial() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let stages = fixture.graph.stages().to_vec();
    let mut journal = live_journal(spec.clone(), fixture.graph);
    for stage in stages.iter().take(5) {
        succeed_stage(&mut journal, &spec, stage);
    }

    let generate_stage = &stages[5];
    let reservation = BudgetAmount::new(spec.declared_writer_token_maximum(), 0, 0, 100)
        .expect("writer reservation");
    let first_attempt = StageAttemptId::new();
    let generate_permit = journal
        .reserve_stage(generate_stage.id(), first_attempt, reservation)
        .expect("first Generate reservation");
    let command = journal
        .start_reserved(generate_permit)
        .expect("start Generate");
    let terminal = failed_lease(
        &command,
        BlobId::digest(b"retryable backend failure"),
        BudgetAmount::new(1, 0, 0, 10).expect("failed-call charge"),
    );
    journal
        .finish(command, terminal)
        .expect("record failed Generate");

    let retry_attempt = StageAttemptId::new();
    let retry_permit = journal
        .reserve_stage(generate_stage.id(), retry_attempt, reservation)
        .expect("fresh retry remains inside immutable trial");
    let retry = journal
        .start_reserved(retry_permit)
        .expect("start fresh Generate retry");
    assert_eq!(retry.attempt_ordinal(), 2);
    let terminal = success_lease(
        &retry,
        BlobId::digest(b"verified retry output"),
        BudgetAmount::new(1, 0, 0, 10).expect("retry charge"),
    );
    journal.finish(retry, terminal).expect("finish retry");
    assert_eq!(
        journal.attempt_status(first_attempt),
        Some(StageAttemptStatus::Failed)
    );
    assert_eq!(
        journal.attempt_status(retry_attempt),
        Some(StageAttemptStatus::Succeeded)
    );
}

#[test]
fn commands_require_reservation_and_dependencies_are_not_searchable() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let late_stage = fixture.graph.stages()[4].id();
    let mut journal = live_journal(spec.clone(), fixture.graph);
    assert!(matches!(
        journal.reserve_stage(
            late_stage,
            StageAttemptId::new(),
            BudgetAmount::new(0, 0, 0, 10).expect("reservation")
        ),
        Err(TrialError::DependencyNotSatisfied { .. })
    ));
}

#[test]
fn overrun_is_rejected_without_mutating_the_running_attempt() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let first_stage = fixture.graph.stages()[0].clone();
    let reservation = spec
        .stage_budget_maximum(first_stage.id())
        .expect("frozen stage maximum");
    let mut journal = live_journal(spec, fixture.graph);
    let attempt = StageAttemptId::new();
    let permit = journal
        .reserve_stage(first_stage.id(), attempt, reservation)
        .expect("reserve");
    let command = journal.start_reserved(permit).expect("start");
    let before = journal.snapshot();
    let terminal = success_lease(
        &command,
        journal.spec().fingerprint(),
        BudgetAmount::new(0, 0, 0, reservation.wall_time_ms() + 1).expect("charge"),
    );
    assert_eq!(
        journal.finish(command, terminal),
        Err(TrialError::Budget(BudgetError::ChargeExceedsReservation))
    );
    assert_eq!(journal.snapshot(), before);
}

#[test]
fn interruption_consumes_the_command_and_charges_the_full_reservation() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let first_stage = fixture.graph.stages()[0].clone();
    let reservation = spec
        .stage_budget_maximum(first_stage.id())
        .expect("frozen stage maximum");
    let mut journal = live_journal(spec, fixture.graph);
    let attempt = StageAttemptId::new();
    let permit = journal
        .reserve_stage(first_stage.id(), attempt, reservation)
        .expect("reserve");
    let command = journal.start_reserved(permit).expect("start");
    let diagnostic = BlobId::digest(b"lost receipt");
    journal
        .reconcile_interrupted(command, diagnostic)
        .expect("conservative recovery");
    assert_eq!(journal.snapshot().budget().charged(), reservation);
    assert_eq!(
        journal.attempt_status(attempt),
        Some(StageAttemptStatus::Interrupted)
    );
    assert!(matches!(
        journal.events().last().expect("interruption event").kind(),
        TrialEventKind::AttemptFinished {
            terminal: AttemptTerminal::Interrupted {
                diagnostic_fingerprint: terminal_diagnostic,
            },
            terminal_evidence: StageTerminalEvidence::ConservativeInterruption {
                diagnostic_fingerprint: evidence_diagnostic,
            },
            ..
        } if terminal_diagnostic == diagnostic && evidence_diagnostic == diagnostic
    ));
}

#[test]
fn adversarial_event_mutation_and_reordering_fail_replay() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let first_stage = fixture.graph.stages()[0].clone();
    let mut journal = live_journal(spec.clone(), fixture.graph.clone());
    succeed_stage(&mut journal, &spec, &first_stage);
    let original = serde_json::to_value(journal.events()).expect("JSON");

    let mut changed_charge = original.clone();
    changed_charge[1]["kind"]["reservation"]["wall_time_ms"] = serde_json::json!(999);
    let events: TrialEventBatch =
        serde_json::from_value(changed_charge).expect("shape remains valid");
    assert!(matches!(
        TrialJournal::replay_batch(&spec, &fixture.graph, &events),
        Err(TrialError::EventFingerprint)
    ));

    let mut changed_link = original.clone();
    changed_link[1]["previous_event_fingerprint"] =
        serde_json::json!(BlobId::digest(b"wrong link"));
    let events: TrialEventBatch =
        serde_json::from_value(changed_link).expect("shape remains valid");
    assert!(matches!(
        TrialJournal::replay_batch(&spec, &fixture.graph, &events),
        Err(TrialError::PreviousEventFingerprint)
    ));

    let batch: TrialEventBatch = serde_json::from_value(original).expect("events");
    let mut events = batch.into_events();
    events.swap(1, 2);
    assert!(matches!(
        TrialJournal::replay(&spec, &fixture.graph, &events),
        Err(TrialError::EventSequence { .. })
    ));
}

#[test]
fn wrong_graph_and_wrong_compile_prompt_output_fail_closed() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    assert!(matches!(
        TrialJournal::diagnostic_for_tests(spec.clone(), stage_graph()),
        Err(TrialError::StageGraphMismatch)
    ));

    let stages = fixture.graph.stages().to_vec();
    let mut journal = live_journal(spec.clone(), fixture.graph);
    for stage in stages.iter().take(4) {
        succeed_stage(&mut journal, &spec, stage);
    }
    let compile_stage = &stages[4];
    let attempt = StageAttemptId::new();
    let reservation = spec
        .stage_budget_maximum(compile_stage.id())
        .expect("compile-stage maximum");
    let permit = journal
        .reserve_stage(compile_stage.id(), attempt, reservation)
        .expect("reserve compile");
    let command = journal.start_reserved(permit).expect("start compile");
    let terminal = success_lease(
        &command,
        BlobId::digest(b"substituted prompt"),
        BudgetAmount::new(0, 0, 0, 1).expect("charge"),
    );
    assert_eq!(
        journal.finish(command, terminal),
        Err(TrialError::ExpectedOutputFingerprint)
    );
}

#[test]
fn session_permits_and_terminal_leases_cannot_cross_live_journals() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let graph = fixture.graph;
    let first_stage = graph.stages()[0].clone();
    let attempt_id = StageAttemptId::new();
    let reservation = spec
        .stage_budget_maximum(first_stage.id())
        .expect("frozen stage maximum");

    let mut first = live_journal(spec.clone(), graph.clone());
    let mut second = live_journal(spec.clone(), graph.clone());
    let first_permit = first
        .reserve_stage(first_stage.id(), attempt_id, reservation)
        .expect("first reservation");
    let second_permit = second
        .reserve_stage(first_stage.id(), attempt_id, reservation)
        .expect("second reservation");
    assert!(matches!(
        second.start_reserved(first_permit),
        Err(TrialError::ReservationPermitMismatch)
    ));
    let second_command = second
        .start_reserved(second_permit)
        .expect("own permit starts");

    let mut third = live_journal(spec.clone(), graph.clone());
    let third_permit = third
        .reserve_stage(first_stage.id(), attempt_id, reservation)
        .expect("third reservation");
    let third_command = third.start_reserved(third_permit).expect("third command");
    let third_terminal = success_lease(
        &third_command,
        spec.fingerprint(),
        BudgetAmount::new(0, 0, 0, 1).expect("charge"),
    );
    assert_eq!(
        second.finish(second_command, third_terminal),
        Err(TrialError::TerminalLeaseMismatch)
    );

    third
        .reconcile_interrupted(third_command, BlobId::digest(b"test cleanup"))
        .expect("third command remains affine and valid for its journal");
}

#[test]
fn terminal_event_derives_output_charge_and_live_evidence_from_the_consumed_lease() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let first_stage = fixture.graph.stages()[0].clone();
    let reservation = spec
        .stage_budget_maximum(first_stage.id())
        .expect("frozen stage maximum");
    let mut journal = live_journal(spec.clone(), fixture.graph);
    let attempt_id = StageAttemptId::new();
    let charge = BudgetAmount::new(0, 0, 0, 3).expect("charge");
    let output = spec.fingerprint();
    let live_evidence = BlobId::digest(b"exact backend/store terminal receipt");
    let permit = journal
        .reserve_stage(first_stage.id(), attempt_id, reservation)
        .expect("reserve");
    let command = journal.start_reserved(permit).expect("start");
    let terminal = VerifiedStageTerminalLease::diagnostic_for_tests(
        &command,
        AttemptTerminal::Succeeded {
            output_fingerprint: output,
        },
        charge,
        live_evidence,
    );
    journal.finish(command, terminal).expect("accept terminal");

    assert_eq!(journal.snapshot().budget().charged(), charge);
    assert!(matches!(
        journal.events().last().expect("terminal event").kind(),
        TrialEventKind::AttemptFinished {
            attempt_id: observed_attempt,
            terminal: AttemptTerminal::Succeeded { output_fingerprint },
            actual_charge,
            terminal_evidence: StageTerminalEvidence::VerifiedLive {
                receipt_fingerprint,
            },
            ..
        } if observed_attempt == attempt_id
            && output_fingerprint == output
            && actual_charge == charge
            && receipt_fingerprint == live_evidence
    ));
}

#[test]
fn reserved_permit_can_only_be_abandoned_before_dispatch() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let first_stage = fixture.graph.stages()[0].clone();
    let reservation = spec
        .stage_budget_maximum(first_stage.id())
        .expect("frozen stage maximum");
    let mut journal = live_journal(spec, fixture.graph);
    let attempt_id = StageAttemptId::new();
    let permit = journal
        .reserve_stage(first_stage.id(), attempt_id, reservation)
        .expect("reserve");
    assert_eq!(journal.snapshot().budget().reserved(), reservation);
    journal
        .abandon_reserved(permit, BlobId::digest(b"pre-dispatch abandonment"))
        .expect("abandon");
    assert_eq!(
        journal.snapshot().budget().reserved(),
        BudgetAmount::default()
    );
    assert_eq!(
        journal.snapshot().budget().charged(),
        BudgetAmount::default()
    );
    assert_eq!(
        journal.attempt_status(attempt_id),
        Some(StageAttemptStatus::Abandoned)
    );
}

#[test]
fn completion_requires_exact_archive_output_terminal_and_current_head() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let archive_stage_id = fixture.graph.output();
    let mut journal = live_journal(spec.clone(), fixture.graph.clone());
    run_to_archive(&mut journal, &spec, &fixture.graph);
    let charged = journal.snapshot().budget().charged();
    let request = journal
        .completion_request()
        .expect("archive completion request");
    let expected_archive_output =
        expected_output(&spec, FrozenTrialStage::Archive, archive_stage_id);
    assert_eq!(
        request.archive_output_fingerprint(),
        expected_archive_output
    );
    assert_eq!(
        request.current_event_fingerprint(),
        journal.snapshot().last_event_fingerprint()
    );

    let mut stale = journal
        .completion_lease_for_tests(BlobId::digest(b"store archive proof"))
        .expect("completion lease");
    stale.current_event_fingerprint = BlobId::digest(b"stale journal head");
    assert!(matches!(
        journal.complete(stale),
        Err(TrialError::CompletionLeaseMismatch)
    ));
    assert_eq!(journal.snapshot().status(), TrialStatus::Running);

    let completion_evidence = BlobId::digest(b"fresh exact archive proof");
    let completion = journal
        .completion_lease_for_tests(completion_evidence)
        .expect("fresh completion lease");
    let completed = journal
        .complete(completion)
        .expect("complete exact archive");
    assert_eq!(completed.trial_fingerprint(), spec.fingerprint());
    assert_eq!(
        completed.archive_output_fingerprint(),
        expected_archive_output
    );
    assert_eq!(
        completed.archive_terminal_event_fingerprint(),
        request.archive_terminal_event_fingerprint()
    );
    assert_eq!(completed.actual_charge(), charged);
    assert_eq!(
        completed.live_completion_evidence_fingerprint(),
        completion_evidence
    );
    assert_eq!(
        completed.trial_journal_fingerprint(),
        journal.snapshot().last_event_fingerprint()
    );
}

#[test]
fn replay_wire_input_is_bounded_and_rejects_unknown_claim_fields() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let journal = live_journal(spec, fixture.graph);
    let prepared = journal.events()[0].clone();
    assert_eq!(
        TrialEventBatch::new(vec![prepared.clone(); MAX_TRIAL_EVENTS + 1]),
        Err(TrialError::EventLimit)
    );
    let encoded = serde_json::to_vec(&vec![prepared; MAX_TRIAL_EVENTS + 1])
        .expect("oversized event claim JSON");
    assert!(serde_json::from_slice::<TrialEventBatch>(&encoded).is_err());

    let mut unknown = serde_json::to_value(&journal.events()[0]).expect("event value");
    unknown["kind"]["caller_asserted_live"] = serde_json::json!(true);
    assert!(serde_json::from_value::<TrialEvent>(unknown).is_err());
}

#[test]
fn direct_continuation_runtime_commits_only_exact_typed_dependencies() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let stages = fixture.graph.stages().to_vec();
    let mut journal = live_journal(spec.clone(), fixture.graph);

    let freeze = start_stage_command(&mut journal, &spec, &stages[0]);
    let frozen = execute_freeze_inputs(freeze, &spec)
        .expect("exact frozen inputs")
        .commit(&mut journal)
        .expect("commit frozen inputs");
    let mask = start_stage_command(&mut journal, &spec, &stages[1]);
    let mask = execute_direct_backtranslate_mask(mask, &spec, &frozen)
        .expect("explicit no-mask stage")
        .commit(&mut journal)
        .expect("commit no-mask stage");
    let plan = start_stage_command(&mut journal, &spec, &stages[2]);
    let plan = execute_direct_plan(plan, &spec, &frozen, &mask)
        .expect("explicit no-plan stage")
        .commit(&mut journal)
        .expect("commit no-plan stage");
    let retrieve = start_stage_command(&mut journal, &spec, &stages[3]);
    let retrieval = execute_direct_retrieve(retrieve, &spec, &frozen, &plan)
        .expect("explicit no-retrieval stage")
        .commit(&mut journal)
        .expect("commit no-retrieval stage");
    let compile = start_stage_command(&mut journal, &spec, &stages[4]);
    let _compiled = execute_compile_prompt(
        compile,
        &spec,
        &frozen,
        &mask,
        &plan,
        &retrieval,
        &fixture.prompt,
    )
    .expect("exact compiled prompt")
    .commit(&mut journal)
    .expect("commit compiled prompt");

    assert_eq!(journal.snapshot().successful_stage_count(), 5);
    assert_eq!(
        journal.snapshot().budget().charged(),
        BudgetAmount::default()
    );
    assert_eq!(
        journal.snapshot().active_attempt_count(),
        0,
        "Generate remains undispatched until live writer authority exists"
    );
}

#[test]
fn direct_runtime_rejects_cross_session_dependency_and_wrong_prompt() {
    let fixture = Fixture::new();
    let spec = fixture.spec();
    let stages = fixture.graph.stages().to_vec();

    let mut first = live_journal(spec.clone(), fixture.graph.clone());
    let first_freeze = start_stage_command(&mut first, &spec, &stages[0]);
    let first_frozen = execute_freeze_inputs(first_freeze, &spec)
        .expect("first frozen inputs")
        .commit(&mut first)
        .expect("commit first frozen inputs");

    let mut second = live_journal(spec.clone(), fixture.graph.clone());
    let second_freeze = start_stage_command(&mut second, &spec, &stages[0]);
    let second_frozen = execute_freeze_inputs(second_freeze, &spec)
        .expect("second frozen inputs")
        .commit(&mut second)
        .expect("commit second frozen inputs");
    let second_mask_command = start_stage_command(&mut second, &spec, &stages[1]);
    assert!(matches!(
        execute_direct_backtranslate_mask(second_mask_command, &spec, &first_frozen),
        Err(TrialRuntimeError::InputAuthorityMismatch)
    ));

    let mut third = live_journal(spec.clone(), fixture.graph);
    let freeze = start_stage_command(&mut third, &spec, &stages[0]);
    let frozen = execute_freeze_inputs(freeze, &spec)
        .expect("third frozen inputs")
        .commit(&mut third)
        .expect("commit third frozen inputs");
    let mask = start_stage_command(&mut third, &spec, &stages[1]);
    let mask = execute_direct_backtranslate_mask(mask, &spec, &frozen)
        .expect("third no-mask")
        .commit(&mut third)
        .expect("commit third no-mask");
    let plan = start_stage_command(&mut third, &spec, &stages[2]);
    let plan = execute_direct_plan(plan, &spec, &frozen, &mask)
        .expect("third no-plan")
        .commit(&mut third)
        .expect("commit third no-plan");
    let retrieve = start_stage_command(&mut third, &spec, &stages[3]);
    let retrieval = execute_direct_retrieve(retrieve, &spec, &frozen, &plan)
        .expect("third no-retrieval")
        .commit(&mut third)
        .expect("commit third no-retrieval");
    let compile = start_stage_command(&mut third, &spec, &stages[4]);
    let other = Fixture::new();
    assert!(matches!(
        execute_compile_prompt(
            compile,
            &spec,
            &frozen,
            &mask,
            &plan,
            &retrieval,
            &other.prompt,
        ),
        Err(TrialRuntimeError::PromptMismatch)
    ));

    drop(second_frozen);
}

#[test]
fn verified_writer_accounting_rejects_mismatch_and_over_budget() {
    let exact = BlobId::digest(b"exact live call");
    let reservation = BudgetAmount::new(8, 0, 0, 20).expect("reservation");
    let facts = WriterRuntimeFacts {
        completion_tokens: 7,
        generated_token_count: 7,
        duration_ms: 19,
        verification_fingerprint: exact,
        call_verification_fingerprint: exact,
    };
    assert_eq!(
        verified_writer_charge(facts, reservation).expect("exact charge"),
        BudgetAmount::new(7, 0, 0, 19).expect("charge")
    );
    assert!(matches!(
        verified_writer_charge(
            WriterRuntimeFacts {
                generated_token_count: 6,
                ..facts
            },
            reservation,
        ),
        Err(TrialRuntimeError::RuntimeChargeMismatch)
    ));
    assert!(matches!(
        verified_writer_charge(
            WriterRuntimeFacts {
                completion_tokens: 9,
                generated_token_count: 9,
                ..facts
            },
            reservation,
        ),
        Err(TrialRuntimeError::ChargeExceedsReservation)
    ));
    assert!(matches!(
        verified_writer_charge(
            WriterRuntimeFacts {
                duration_ms: 21,
                ..facts
            },
            reservation,
        ),
        Err(TrialRuntimeError::ChargeExceedsReservation)
    ));
}

fn start_stage_command(
    journal: &mut TrialJournal,
    spec: &FrozenTrialSpec,
    stage: &FrozenStageSpec,
) -> StageCommand {
    let attempt = StageAttemptId::new();
    let reservation = spec
        .stage_budget_maximum(stage.id())
        .expect("stage maximum");
    let permit = journal
        .reserve_stage(stage.id(), attempt, reservation)
        .expect("reserve stage");
    journal.start_reserved(permit).expect("start stage")
}

fn succeed_stage(journal: &mut TrialJournal, spec: &FrozenTrialSpec, stage: &FrozenStageSpec) {
    let attempt = StageAttemptId::new();
    let (reservation, charge) = budgets_for(stage.stage(), spec);
    let permit = journal
        .reserve_stage(stage.id(), attempt, reservation)
        .expect("reserve");
    let command = journal.start_reserved(permit).expect("start");
    let terminal = success_lease(
        &command,
        expected_output(spec, stage.stage(), stage.id()),
        charge,
    );
    journal.finish(command, terminal).expect("success");
}

fn run_to_archive(journal: &mut TrialJournal, spec: &FrozenTrialSpec, graph: &StageGraph) {
    for stage in graph.stages() {
        succeed_stage(journal, spec, stage);
    }
}

fn live_journal(spec: FrozenTrialSpec, graph: StageGraph) -> TrialJournal {
    TrialJournal::diagnostic_for_tests(spec, graph).expect("live trial journal")
}

fn persist_trial(
    store: &mut ProjectStore,
    spec: &FrozenTrialSpec,
    graph: &StageGraph,
    claimed_subject: BlobId,
    canonical_record_bytes: &[u8],
) -> TrialRunId {
    persist_trial_with_stage_laundering(
        store,
        spec,
        graph,
        claimed_subject,
        canonical_record_bytes,
        StageLaundering::None,
    )
}

#[derive(Clone, Copy, Debug)]
enum StageLaundering {
    None,
    Maximum,
    IdentityAndDependencies,
}

fn persist_trial_with_stage_laundering(
    store: &mut ProjectStore,
    spec: &FrozenTrialSpec,
    graph: &StageGraph,
    claimed_subject: BlobId,
    canonical_record_bytes: &[u8],
    laundering: StageLaundering,
) -> TrialRunId {
    let trial_maximum = research_budget_from_limits(spec.budget());
    store
        .persist_frozen_campaign(FrozenCampaignPersistence {
            campaign_id: spec.campaign_id(),
            campaign_fingerprint: BlobId::digest(b"supporting persisted trial campaign"),
            project_id: store.manifest().project_id,
            manifest_source_bytes: b"supporting campaign manifest",
            manifest_fingerprint: spec.campaign_manifest_fingerprint(),
            project_input_fingerprint: spec.project_input_fingerprint(),
            seed: 0,
            maximum: trial_maximum,
            canonical_record_bytes: b"supporting frozen campaign record",
        })
        .expect("persist supporting campaign");

    let stage_records = graph
        .stages()
        .iter()
        .map(|stage| canonical_stage_record_bytes(stage).expect("canonical stage record"))
        .collect::<Vec<_>>();
    let mut persisted_stage_ids = graph
        .stages()
        .iter()
        .map(FrozenStageSpec::id)
        .collect::<Vec<_>>();
    if matches!(laundering, StageLaundering::IdentityAndDependencies) {
        persisted_stage_ids[0] = StageId::new();
    }
    let persisted_dependencies = graph
        .stages()
        .iter()
        .map(|stage| {
            stage
                .dependencies()
                .iter()
                .map(|dependency| {
                    let index = graph
                        .stages()
                        .iter()
                        .position(|candidate| candidate.id() == *dependency)
                        .expect("known dependency");
                    persisted_stage_ids[index]
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let stages = graph
        .stages()
        .iter()
        .zip(&stage_records)
        .enumerate()
        .map(|(index, (stage, record))| {
            let mut maximum = spec
                .stage_budget_maximum(stage.id())
                .expect("frozen stage maximum");
            if index == 0 && matches!(laundering, StageLaundering::Maximum) {
                maximum = BudgetAmount::new(0, 0, 0, maximum.wall_time_ms() + 1)
                    .expect("laundered bounded maximum");
            }
            FrozenStagePersistence {
                stage_id: persisted_stage_ids[index],
                stage: stage.stage(),
                stage_spec_fingerprint: stage.spec_fingerprint(),
                maximum: research_budget_from_amount(maximum),
                dependencies: &persisted_dependencies[index],
                canonical_record_bytes: record,
            }
        })
        .collect::<Vec<_>>();
    store
        .persist_frozen_trial(FrozenTrialPersistence {
            campaign_id: spec.campaign_id(),
            trial_fingerprint: claimed_subject,
            trial_case_id: spec.case_id(),
            treatment_fingerprint: spec.treatment_fingerprint(),
            prompt_content_fingerprint: spec.prompt_content_fingerprint(),
            model_binding_fingerprint: spec.model_binding_fingerprint(),
            expected_writer_call_count: spec.expected_writer_call_count(),
            declared_writer_token_maximum: spec.declared_writer_token_maximum(),
            maximum: trial_maximum,
            canonical_record_bytes,
            stages: &stages,
        })
        .expect("persist frozen trial claim");

    let trial_run_id = TrialRunId::new();
    let run_record = TrialRunRecord::new(trial_run_id, claimed_subject, TrialRunOrigin::Standalone);
    let run_record_bytes = run_record.canonical_bytes().expect("canonical trial run");
    store
        .persist_standalone_trial_run(StandaloneTrialRunPersistence {
            trial_run_id,
            trial_fingerprint: claimed_subject,
            canonical_record_bytes: &run_record_bytes,
        })
        .expect("persist standalone trial run");
    trial_run_id
}

fn research_budget_from_limits(limits: TrialBudgetLimits) -> ResearchBudgetMaximum {
    ResearchBudgetMaximum {
        writer_tokens: limits.writer_tokens(),
        controller_tokens: limits.controller_tokens(),
        evaluations: limits.evaluations(),
        wall_time_ms: limits.wall_time_ms(),
    }
}

fn research_budget_from_amount(amount: BudgetAmount) -> ResearchBudgetMaximum {
    ResearchBudgetMaximum {
        writer_tokens: amount.writer_tokens(),
        controller_tokens: amount.controller_tokens(),
        evaluations: amount.evaluations(),
        wall_time_ms: amount.wall_time_ms(),
    }
}

fn success_lease(
    command: &StageCommand,
    output_fingerprint: BlobId,
    actual_charge: BudgetAmount,
) -> VerifiedStageTerminalLease {
    VerifiedStageTerminalLease::diagnostic_for_tests(
        command,
        AttemptTerminal::Succeeded { output_fingerprint },
        actual_charge,
        BlobId::digest(b"test-only verified successful terminal"),
    )
}

fn failed_lease(
    command: &StageCommand,
    diagnostic_fingerprint: BlobId,
    actual_charge: BudgetAmount,
) -> VerifiedStageTerminalLease {
    VerifiedStageTerminalLease::diagnostic_for_tests(
        command,
        AttemptTerminal::Failed {
            diagnostic_fingerprint,
        },
        actual_charge,
        BlobId::digest(b"test-only verified failed terminal"),
    )
}

fn budgets_for(stage: FrozenTrialStage, spec: &FrozenTrialSpec) -> (BudgetAmount, BudgetAmount) {
    let reservation = stage_budget_maximum(
        stage,
        spec.declared_writer_token_maximum(),
        spec.prompt_topology(),
    );
    let charge = match TrialWorkClass::for_stage(stage) {
        TrialWorkClass::Pure => BudgetAmount::new(0, 0, 0, 1).expect("charge"),
        TrialWorkClass::OptionalController
            if spec.prompt_topology() == PromptTopology::ExactDirectContinuation =>
        {
            BudgetAmount::new(0, 0, 0, 2).expect("charge")
        }
        TrialWorkClass::OptionalController => BudgetAmount::new(0, 3, 0, 2).expect("charge"),
        TrialWorkClass::Writer => BudgetAmount::new(7, 0, 0, 20).expect("charge"),
        TrialWorkClass::Evaluation => BudgetAmount::new(0, 4, 1, 20).expect("charge"),
    };
    (reservation, charge)
}

fn stage_budget_maxima(
    graph: &StageGraph,
    declared_writer_tokens: u64,
    prompt_topology: PromptTopology,
) -> Vec<FrozenStageBudgetMaximum> {
    graph
        .stages()
        .iter()
        .map(|stage| {
            FrozenStageBudgetMaximum::new(
                stage.id(),
                stage_budget_maximum(stage.stage(), declared_writer_tokens, prompt_topology),
            )
        })
        .collect()
}

fn stage_budget_maximum(
    stage: FrozenTrialStage,
    declared_writer_tokens: u64,
    prompt_topology: PromptTopology,
) -> BudgetAmount {
    match TrialWorkClass::for_stage(stage) {
        TrialWorkClass::Pure => BudgetAmount::new(0, 0, 0, 10).expect("reservation"),
        TrialWorkClass::OptionalController
            if prompt_topology == PromptTopology::ExactDirectContinuation =>
        {
            BudgetAmount::new(0, 0, 0, 10).expect("reservation")
        }
        TrialWorkClass::OptionalController => BudgetAmount::new(0, 10, 0, 10).expect("reservation"),
        TrialWorkClass::Writer => {
            BudgetAmount::new(declared_writer_tokens, 0, 0, 100).expect("reservation")
        }
        TrialWorkClass::Evaluation => BudgetAmount::new(0, 10, 1, 100).expect("reservation"),
    }
}

fn expected_output(spec: &FrozenTrialSpec, stage: FrozenTrialStage, stage_id: StageId) -> BlobId {
    match stage {
        FrozenTrialStage::FreezeInputs => spec.fingerprint(),
        FrozenTrialStage::CompilePrompt => spec.prompt_content_fingerprint(),
        FrozenTrialStage::BacktranslateMask
        | FrozenTrialStage::Plan
        | FrozenTrialStage::Retrieve
        | FrozenTrialStage::Generate
        | FrozenTrialStage::Admit
        | FrozenTrialStage::Assemble
        | FrozenTrialStage::Gate
        | FrozenTrialStage::Evaluate
        | FrozenTrialStage::Describe
        | FrozenTrialStage::Archive => BlobId::digest(&stage_id.as_ulid().to_bytes()),
    }
}

fn stage_graph() -> StageGraph {
    let ids = FrozenTrialStage::ALL.map(|_| StageId::new());
    let dependency_indices: [&[usize]; 12] = [
        &[],
        &[0],
        &[0, 1],
        &[0, 2],
        &[0, 1, 2, 3],
        &[4],
        &[5],
        &[6],
        &[7],
        &[8],
        &[9],
        &[9, 10],
    ];
    let stages = FrozenTrialStage::ALL
        .into_iter()
        .enumerate()
        .map(|(index, stage)| {
            FrozenStageSpec::new(
                ids[index],
                stage,
                BlobId::digest(&[u8::try_from(index).expect("twelve stage indices fit u8")]),
                dependency_indices[index]
                    .iter()
                    .map(|dependency| ids[*dependency])
                    .collect(),
            )
            .expect("stage")
        })
        .collect();
    StageGraph::new(StageGraphId::new(), stages, ids[11]).expect("graph")
}

fn compiled_prompt_with_shape(
    fixture: &Fixture,
    treatment_fingerprint: BlobId,
    shape: &[(CompletionPromptBlockRole, bool)],
) -> CompiledBaseCompletionPrompt {
    let mut owned_sources = Vec::with_capacity(shape.len() + 1);
    let mut blocks = Vec::with_capacity(shape.len());
    let mut exact_prompt_bytes = Vec::new();
    for (index, (role, transformed)) in shape.iter().copied().enumerate() {
        let bytes = format!("topology block {index}\n\n").into_bytes();
        let revision_id = RevisionId::new();
        let source_blob_id = BlobId::digest(&bytes);
        let range = NonEmptyByteRange::new(0, bytes.len() as u64).expect("block range");
        let source = PromptSourceRange::new(revision_id, source_blob_id, range)
            .expect("prompt source range");
        let witness = if transformed {
            PromptBlockWitness::transformation(
                vec![source],
                BlobId::digest(b"test transformation recipe"),
                BlobId::digest(b"test transformation receipt"),
                BlobId::digest(&bytes),
            )
            .expect("transformation witness")
        } else {
            PromptBlockWitness::exact_source(source)
        };
        blocks.push(
            FrozenCompletionPromptBlock::new(
                role,
                ExactPromptBlockBytes::new(bytes.clone()).expect("block bytes"),
                witness,
            )
            .expect("prompt block"),
        );
        exact_prompt_bytes.extend_from_slice(&bytes);
        owned_sources.push((revision_id, bytes));
    }

    let tail_bytes = b"private opening paragraph".to_vec();
    let tail_revision = RevisionId::new();
    let tail = CompletionPromptTail::live_manuscript(
        tail_revision,
        BlobId::digest(&tail_bytes),
        NonEmptyByteRange::new(0, tail_bytes.len() as u64).expect("tail range"),
    )
    .expect("tail");
    exact_prompt_bytes.extend_from_slice(&tail_bytes);
    owned_sources.push((tail_revision, tail_bytes));
    let exact_sources = owned_sources
        .iter()
        .map(|(revision_id, bytes)| ExactPromptSource::new(*revision_id, bytes))
        .collect::<Vec<_>>();
    let generate_stage_id = fixture
        .graph
        .stages()
        .iter()
        .find(|stage| stage.stage() == FrozenTrialStage::Generate)
        .expect("generate stage")
        .id();
    FrozenBaseCompletionPrompt::new(
        fixture.project_id,
        CallScope::new(
            fixture.campaign_id,
            generate_stage_id,
            StageAttemptId::new(),
            fixture.case_id,
        ),
        treatment_fingerprint,
        blocks,
        tail,
    )
    .expect("prompt specification")
    .compile(exact_prompt_bytes, &exact_sources)
    .expect("compiled shaped prompt")
}

fn compiled_prompt(
    project_id: ProjectId,
    campaign_id: CampaignId,
    case_id: TrialCaseId,
    generate_stage_id: StageId,
    treatment_fingerprint: BlobId,
) -> CompiledBaseCompletionPrompt {
    let source = b"private opening paragraph";
    let revision_id = RevisionId::new();
    let tail = CompletionPromptTail::live_manuscript(
        revision_id,
        BlobId::digest(source),
        NonEmptyByteRange::new(0, source.len() as u64).expect("range"),
    )
    .expect("tail");
    FrozenBaseCompletionPrompt::new(
        project_id,
        CallScope::new(
            campaign_id,
            generate_stage_id,
            StageAttemptId::new(),
            case_id,
        ),
        treatment_fingerprint,
        Vec::new(),
        tail,
    )
    .expect("prompt spec")
    .compile(
        source.to_vec(),
        &[ExactPromptSource::new(revision_id, source)],
    )
    .expect("compiled prompt")
}

fn rebind_direct_prompt_attempt(
    prompt: &CompiledBaseCompletionPrompt,
    attempt_id: StageAttemptId,
) -> CompiledBaseCompletionPrompt {
    let specification = prompt.specification();
    assert!(specification.preceding_blocks().is_empty());
    let tail = specification.tail();
    let revision_id = tail
        .source_revision_id()
        .expect("direct prompt has a manuscript tail");
    let scope = specification.scope();
    FrozenBaseCompletionPrompt::new(
        specification.project_id(),
        CallScope::new(
            scope.campaign_id(),
            scope.stage_id(),
            attempt_id,
            scope.case_id(),
        ),
        specification.treatment_recipe_fingerprint(),
        Vec::new(),
        tail,
    )
    .expect("fresh retry prompt specification")
    .compile(
        prompt.exact_bytes().to_vec(),
        &[ExactPromptSource::new(revision_id, prompt.exact_bytes())],
    )
    .expect("source-bound retry prompt")
}

fn model_manifest() -> String {
    format!(
        r#"format = "loom.model-bindings.v1"
name = "test-models"
description = "Pinned test artifacts"

[[bindings]]
id = "writer"
role = "base_writer"
model_sha256 = "{MODEL_HASH}"
model_bytes = 1024
tokenizer_sha256 = "{TOKENIZER_HASH}"
architecture = "test"
context_tokens = 1024
capabilities = ["completion", "logits"]
adapters = []
"#
    )
}

fn campaign_manifest(model_bindings: BlobId, prefix: &str) -> String {
    campaign_manifest_with_topology(model_bindings, prefix, "exact_direct_continuation")
}

fn campaign_manifest_with_topology(
    model_bindings: BlobId,
    prefix: &str,
    prompt_topology: &str,
) -> String {
    format!(
        r#"{prefix}format = "loom.campaign.v1"
name = "frozen-test"
description = "One immutable trial"
seed = 42
selection = "sealed_comparison"

[core_pack]
format = "loom.core-pack.v1"
artifact_sha256 = "{MODEL_HASH}"

[genre_pack]
format = "loom.genre-pack.v1"
artifact_sha256 = "{TOKENIZER_HASH}"

[model_bindings]
format = "loom.model-bindings.v1"
artifact_sha256 = "{model_bindings}"

[budget]
max_writer_tokens = 1000
max_controller_tokens = 1000
max_evaluations = 100

[[cases]]
id = "case-01"
genre_function = "suspense"
source_sha256 = "{MODEL_HASH}"
max_context_tokens = 512

[[treatments]]
id = "direct"
prompt_topology = "{prompt_topology}"
samples_per_case = 2
max_output_tokens = 16
control_parameters = {{}}

[treatments.sampler]
temperature = 0.8
top_k = 40
top_p = 0.95
min_p = 0.05
typical_p = 1.0
repetition_penalty = 1.05
"#
    )
}

#[cfg(feature = "real-native-tests")]
mod real_native_runtime {
    use std::{env, fs, path::PathBuf, sync::Mutex};

    use llama_native_engine::NativeModelOwner;
    use llama_native_types::{NativeDevice, NativeModelConfig, SamplingConfig};
    use loom_inference::{
        BaseWriterBackend,
        native_llama::{BaseWriterCaseSpec, NativeLlamaWriter},
    };
    use loom_research_types::ModelCallId;

    use super::*;

    const REAL_MODEL_ENV: &str = "MOM_LLAMA_MODEL_PATH";
    const QWEN_SHA256: &str = "9acfc1e001311f34b4252001b626f2e466d592a42065f66571bff3790d4e1b14";
    const QWEN_BYTES: u64 = 484_220_320;
    static REAL_MODEL_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    #[ignore = "requires MOM_LLAMA_MODEL_PATH to name the pinned Qwen 0.6B GGUF"]
    fn real_qwen_direct_trial_charges_verified_generation_and_commits_exact_scope() {
        let _guard = REAL_MODEL_LOCK.lock().expect("real trial model lock");
        let model_path = real_model_path();
        let metadata = fs::metadata(&model_path).expect("pinned Qwen GGUF must be readable");
        assert_eq!(metadata.len(), QWEN_BYTES, "Qwen byte length changed");

        let directory = tempfile::tempdir().expect("temporary real trial project");
        let (mut store, _) = ProjectStore::initialize(directory.path(), "real direct trial")
            .expect("initialize real trial store");
        let (spec, graph, prompt, binding) = build_real_trial(&store);
        let canonical = spec.canonical_record_bytes().expect("canonical real trial");
        let trial_run_id = persist_trial(&mut store, &spec, &graph, spec.fingerprint(), &canonical);
        let lease = store
            .acquire_trial_run_session(trial_run_id)
            .expect("real trial lease");
        let mut journal = TrialJournal::new(spec.clone(), graph.clone(), &store, lease)
            .expect("durable real trial journal");
        let compiled = commit_real_direct_prefix(&mut journal, &spec, &graph, &prompt);

        let generate_stage = &graph.stages()[5];
        let first_attempt = StageAttemptId::new();
        let first =
            start_stage_command_with_attempt(&mut journal, &spec, generate_stage, first_attempt);
        let failed_charge = BudgetAmount::new(0, 0, 0, 1).expect("bounded failed charge");
        let failure = failed_lease(
            &first,
            BlobId::digest(b"real-test pre-dispatch retryable failure"),
            failed_charge,
        );
        journal
            .finish(first, failure)
            .expect("record first Generate failure");

        let retry_attempt = StageAttemptId::new();
        let retry_prompt = rebind_direct_prompt_attempt(&prompt, retry_attempt);
        assert_eq!(
            retry_prompt.content_fingerprint(),
            spec.prompt_content_fingerprint()
        );
        assert_ne!(retry_prompt.fingerprint(), prompt.fingerprint());
        let generate =
            start_stage_command_with_attempt(&mut journal, &spec, generate_stage, retry_attempt);
        let (owner, outcome) = run_real_inference(model_path, binding, retry_prompt);
        let expected_charge = verified_real_charge(&outcome);
        let _generated = execute_generate(generate, &spec, &compiled, &outcome)
            .expect("bind freshly scoped real retry")
            .commit(&mut journal)
            .expect("commit exact real retry");

        assert_eq!(journal.snapshot().successful_stage_count(), 6);
        assert_eq!(
            journal.snapshot().budget().charged(),
            failed_charge
                .checked_add(expected_charge)
                .expect("total charge")
        );
        assert_eq!(
            journal.attempt_status(first_attempt),
            Some(StageAttemptStatus::Failed)
        );
        assert_eq!(
            journal.attempt_status(retry_attempt),
            Some(StageAttemptStatus::Succeeded)
        );
        owner
            .shutdown_joined()
            .expect("join real trial worker before teardown");
    }

    fn build_real_trial(
        store: &ProjectStore,
    ) -> (
        FrozenTrialSpec,
        StageGraph,
        CompiledBaseCompletionPrompt,
        BaseWriterBinding,
    ) {
        let project_id = store.manifest().project_id;
        let campaign_id = CampaignId::new();
        let case_id = TrialCaseId::new();
        let graph = stage_graph();
        let binding_manifest =
            compile_manifest(real_model_manifest().as_bytes()).expect("Qwen binding manifest");
        let binding =
            BaseWriterBinding::compile(&binding_manifest, "writer").expect("Qwen binding");
        let campaign = compile_manifest(
            real_campaign_manifest(binding_manifest.artifact_hash().as_blob_id()).as_bytes(),
        )
        .expect("real direct N=1 campaign");
        let treatment = match campaign.document() {
            ManifestDocument::Campaign(campaign) => &campaign.treatments()[0],
            _ => panic!("real campaign fixture"),
        };
        let treatment_fingerprint = fingerprint_treatment(treatment);
        let generate_stage = graph
            .stages()
            .iter()
            .find(|stage| stage.stage() == FrozenTrialStage::Generate)
            .expect("Generate stage");
        let prompt = compiled_prompt(
            project_id,
            campaign_id,
            case_id,
            generate_stage.id(),
            treatment_fingerprint,
        );
        let budget = TrialBudgetLimits::new(2, 10, 1, 30_000).expect("real trial budget");
        let spec = FrozenTrialSpec::compile(FrozenTrialInputs {
            project_id,
            project_input_fingerprint: BlobId::digest(b"real direct frozen project input"),
            campaign_id,
            case_id,
            campaign_manifest: &campaign,
            campaign_case_key: "case-01",
            treatment_key: "direct",
            stage_graph: &graph,
            compiled_prompt: &prompt,
            prompt_topology_lease: PromptTopologyVerifier::verify(treatment, &prompt)
                .expect("exact direct prompt topology"),
            model_binding: &binding,
            stage_budget_maxima: real_stage_budget_maxima(&graph, 2),
            budget,
        })
        .expect("frozen real direct trial");
        assert_eq!(spec.expected_writer_call_count(), 1);
        assert_eq!(spec.declared_writer_token_maximum(), 2);
        (spec, graph, prompt, binding)
    }

    fn commit_real_direct_prefix(
        journal: &mut TrialJournal,
        spec: &FrozenTrialSpec,
        graph: &StageGraph,
        prompt: &CompiledBaseCompletionPrompt,
    ) -> CompiledPromptStageOutput {
        let stages = graph.stages();
        let freeze = start_stage_command(journal, spec, &stages[0]);
        let frozen = execute_freeze_inputs(freeze, spec)
            .expect("freeze exact real inputs")
            .commit(journal)
            .expect("commit real inputs");
        let mask = start_stage_command(journal, spec, &stages[1]);
        let mask = execute_direct_backtranslate_mask(mask, spec, &frozen)
            .expect("real explicit no-mask")
            .commit(journal)
            .expect("commit real no-mask");
        let plan = start_stage_command(journal, spec, &stages[2]);
        let plan = execute_direct_plan(plan, spec, &frozen, &mask)
            .expect("real explicit no-plan")
            .commit(journal)
            .expect("commit real no-plan");
        let retrieve = start_stage_command(journal, spec, &stages[3]);
        let retrieval = execute_direct_retrieve(retrieve, spec, &frozen, &plan)
            .expect("real explicit no-retrieval")
            .commit(journal)
            .expect("commit real no-retrieval");
        let compile = start_stage_command(journal, spec, &stages[4]);
        execute_compile_prompt(compile, spec, &frozen, &mask, &plan, &retrieval, prompt)
            .expect("compile exact real prompt")
            .commit(journal)
            .expect("commit exact real prompt")
    }

    fn run_real_inference(
        model_path: PathBuf,
        binding: BaseWriterBinding,
        prompt: CompiledBaseCompletionPrompt,
    ) -> (NativeModelOwner, loom_inference::VerifiedInferenceOutcome) {
        let mut model = NativeModelConfig::local(model_path);
        model.model_id = "loom-real-direct-trial-qwen".to_owned();
        model.expected_model_sha256 = Some(QWEN_SHA256.to_owned());
        model.device = NativeDevice::Cpu;
        model.context_tokens = 512;
        model.batch_tokens = 128;
        model.max_sequences = 1;
        model.gpu_layers = 0;
        let owner = NativeModelOwner::load(model).expect("load pinned Qwen in-process");
        let writer = NativeLlamaWriter::new(owner.handle());
        let prepared = writer
            .prepare_completion(binding, prompt)
            .expect("prepare exact real completion");
        let sampling = SamplingConfig {
            seed: 91_337,
            temperature: 0.7,
            top_k: 20,
            top_p: 0.9,
            min_p: 0.05,
            repeat_penalty: 1.05,
            max_tokens: 2,
            ..SamplingConfig::default()
        };
        let ticket = writer
            .start(
                prepared,
                vec![
                    BaseWriterCaseSpec::new(ModelCallId::new(), sampling)
                        .expect("one bounded real case"),
                ],
            )
            .expect("start real direct generation");
        let outcome = writer
            .wait(ticket)
            .expect("verify real owner-worker output");
        (owner, outcome)
    }

    fn verified_real_charge(outcome: &loom_inference::VerifiedInferenceOutcome) -> BudgetAmount {
        let call = match &outcome {
            loom_inference::VerifiedInferenceOutcome::Admitted(envelope) => envelope
                .completed_calls()
                .next()
                .expect("one completed real call"),
            loom_inference::VerifiedInferenceOutcome::DiagnosticOnly(_) => {
                panic!("real completion unexpectedly became diagnostic-only")
            }
        };
        BudgetAmount::new(
            call.runtime_charge().completion_tokens(),
            0,
            0,
            u64::try_from(call.runtime_charge().duration_ms())
                .expect("real duration fits trial budget domain"),
        )
        .expect("bounded real charge")
    }

    fn real_stage_budget_maxima(
        graph: &StageGraph,
        declared_writer_tokens: u64,
    ) -> Vec<FrozenStageBudgetMaximum> {
        graph
            .stages()
            .iter()
            .map(|stage| {
                let maximum = if stage.stage() == FrozenTrialStage::Generate {
                    BudgetAmount::new(declared_writer_tokens, 0, 0, 20_000)
                        .expect("real writer maximum")
                } else {
                    stage_budget_maximum(
                        stage.stage(),
                        declared_writer_tokens,
                        PromptTopology::ExactDirectContinuation,
                    )
                };
                FrozenStageBudgetMaximum::new(stage.id(), maximum)
            })
            .collect()
    }

    fn start_stage_command_with_attempt(
        journal: &mut TrialJournal,
        spec: &FrozenTrialSpec,
        stage: &FrozenStageSpec,
        attempt_id: StageAttemptId,
    ) -> StageCommand {
        let reservation = spec
            .stage_budget_maximum(stage.id())
            .expect("stage maximum");
        let permit = journal
            .reserve_stage(stage.id(), attempt_id, reservation)
            .expect("reserve exact attempt");
        journal.start_reserved(permit).expect("start exact attempt")
    }

    fn real_model_path() -> PathBuf {
        env::var_os(REAL_MODEL_ENV).map_or_else(
            || panic!("{REAL_MODEL_ENV} must name pinned Qwen {QWEN_SHA256}"),
            PathBuf::from,
        )
    }

    fn real_model_manifest() -> String {
        format!(
            r#"format = "loom.model-bindings.v1"
name = "qwen-real-trial"
description = "Pinned small base writer for frozen-trial execution proof"

[[bindings]]
id = "writer"
role = "base_writer"
model_sha256 = "{QWEN_SHA256}"
model_bytes = {QWEN_BYTES}
tokenizer_sha256 = "{QWEN_SHA256}"
architecture = "qwen3"
context_tokens = 512
capabilities = ["completion"]
adapters = []
"#
        )
    }

    fn real_campaign_manifest(model_bindings: BlobId) -> String {
        format!(
            r#"format = "loom.campaign.v1"
name = "real-direct-n1"
description = "One exact small-model retry trial"
seed = 91337
selection = "sealed_comparison"

[core_pack]
format = "loom.core-pack.v1"
artifact_sha256 = "{MODEL_HASH}"

[genre_pack]
format = "loom.genre-pack.v1"
artifact_sha256 = "{TOKENIZER_HASH}"

[model_bindings]
format = "loom.model-bindings.v1"
artifact_sha256 = "{model_bindings}"

[budget]
max_writer_tokens = 2
max_controller_tokens = 10
max_evaluations = 1

[[cases]]
id = "case-01"
genre_function = "suspense"
source_sha256 = "{MODEL_HASH}"
max_context_tokens = 512

[[treatments]]
id = "direct"
prompt_topology = "exact_direct_continuation"
samples_per_case = 1
max_output_tokens = 2
control_parameters = {{}}

[treatments.sampler]
temperature = 0.7
top_k = 20
top_p = 0.9
min_p = 0.05
typical_p = 1.0
repetition_penalty = 1.05
"#
        )
    }
}
