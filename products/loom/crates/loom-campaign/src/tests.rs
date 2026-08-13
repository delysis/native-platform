use std::collections::BTreeMap;

use loom_research_types::{
    CampaignId, FrozenStageSpec, FrozenTrialStage, ManifestKey, StageGraph, StageGraphId, StageId,
    TrialCaseId, compile_manifest,
};
use loom_search::UnitScore;
use loom_store::{
    FrozenCampaignPersistence, FrozenCampaignTopologyPersistence,
    FrozenCampaignTrialTopologyPersistence, FrozenStagePersistence, FrozenTrialPersistence,
    ProjectStore, ResearchBudgetMaximum, StoreError,
};
use loom_trial::{BudgetAmount, TrialBudgetLimits, canonical_stage_record_bytes};
use loom_types::{ArtifactId, BlobId, ProjectId};
use tempfile::tempdir;

use super::*;
use crate::authority::{DiagnosticEvaluatedCandidateInput, DiagnosticPressureCurveInput};
use crate::spec::DiagnosticTrialInput;

const MODEL_HASH: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const TOKENIZER_HASH: &str = "2222222222222222222222222222222222222222222222222222222222222222";

struct CampaignFixture {
    spec: FrozenCampaignSpec,
    case_id: TrialCaseId,
    trials: Vec<BlobId>,
    treatments: Vec<BlobId>,
}

impl CampaignFixture {
    fn new(trial_count: usize, dependency_chain: bool) -> Self {
        Self::new_for_project(trial_count, dependency_chain, ProjectId::new())
    }

    fn new_for_project(trial_count: usize, dependency_chain: bool, project_id: ProjectId) -> Self {
        Self::new_for_project_with_controller(trial_count, dependency_chain, project_id, 100)
    }

    fn new_for_project_with_controller(
        trial_count: usize,
        dependency_chain: bool,
        project_id: ProjectId,
        controller_tokens: u64,
    ) -> Self {
        let campaign_id = CampaignId::new();
        let mut campaign_source = campaign_manifest(BlobId::digest(b"model bindings"));
        if controller_tokens == 0 {
            campaign_source = campaign_source
                .replace("max_controller_tokens = 1000", "max_controller_tokens = 0");
        }
        let campaign = compile_manifest(campaign_source.as_bytes()).expect("campaign");
        let case_id = TrialCaseId::new();
        let trials = (0..trial_count)
            .map(|index| BlobId::digest(format!("frozen trial {index}").as_bytes()))
            .collect::<Vec<_>>();
        let treatments = (0..trial_count)
            .map(|index| BlobId::digest(format!("frozen treatment {index}").as_bytes()))
            .collect::<Vec<_>>();
        let inputs = trials
            .iter()
            .enumerate()
            .map(|(index, trial_fingerprint)| DiagnosticTrialInput {
                trial_fingerprint: *trial_fingerprint,
                case_id,
                treatment_fingerprint: treatments[index],
                budget: TrialBudgetLimits::new(100, controller_tokens, 10, 1_000).expect("budget"),
                dependencies: if dependency_chain && index > 0 {
                    vec![trials[index - 1]]
                } else {
                    Vec::new()
                },
            })
            .collect();
        let spec = FrozenCampaignSpec::diagnostic_for_tests(
            campaign_id,
            project_id,
            BlobId::digest(b"frozen project state"),
            &campaign,
            10_000,
            inputs,
        )
        .expect("frozen campaign");
        Self {
            spec,
            case_id,
            trials,
            treatments,
        }
    }

    fn factorial(&self) -> BlockedFactorialPlan {
        let temperature = key("temperature");
        let topology = key("topology");
        let arms = self
            .trials
            .iter()
            .enumerate()
            .map(|(index, trial)| {
                FactorialArm::new(
                    *trial,
                    self.treatments[index],
                    vec![
                        FactorSetting::new(
                            temperature.clone(),
                            BlobId::digest(format!("level {index}").as_bytes()),
                        ),
                        FactorSetting::new(topology.clone(), BlobId::digest(b"direct")),
                    ],
                )
                .expect("arm")
            })
            .collect::<Vec<_>>();
        BlockedFactorialPlan::new(
            BlobId::digest(b"case block"),
            temperature,
            arms[0].clone(),
            arms[1..].to_vec(),
        )
        .expect("factorial")
    }

    fn node(&self, trial: BlobId) -> &FrozenCampaignTrial {
        self.spec.trial(trial).expect("fixture trial")
    }
}

#[test]
fn store_session_binds_campaign_subject_record_project_and_live_exclusivity() {
    let directory = tempdir().expect("temporary project");
    let (mut store, _) = ProjectStore::initialize(directory.path(), "campaign lease")
        .expect("initialize project store");
    let fixture = CampaignFixture::new_for_project(2, true, store.manifest().project_id);
    let canonical = fixture
        .spec
        .canonical_record_bytes()
        .expect("canonical campaign record");
    persist_campaign(
        &mut store,
        &fixture.spec,
        fixture.spec.fingerprint(),
        &canonical,
    );
    let lease = store
        .acquire_campaign_session(fixture.spec.fingerprint())
        .expect("matching campaign lease");
    let lease_fingerprint = lease.lease_fingerprint();
    let journal = CampaignJournal::new(fixture.spec.clone(), &store, lease).expect("live campaign");
    assert_eq!(journal.store_lease_fingerprint(), lease_fingerprint);
    assert!(matches!(
        store.acquire_campaign_session(fixture.spec.fingerprint()),
        Err(StoreError::ResearchSessionAlreadyActive { .. })
    ));
    drop(journal);
    let reacquired = store
        .acquire_campaign_session(fixture.spec.fingerprint())
        .expect("journal drop releases exact campaign");
    drop(reacquired);
}

#[test]
fn durable_campaign_resume_releases_reserved_trial_before_retry() {
    let directory = tempdir().expect("temporary project");
    let project_path = directory.path().to_path_buf();
    let (mut store, _) =
        ProjectStore::initialize(&project_path, "campaign reserved recovery").expect("project");
    let fixture = CampaignFixture::new_for_project(2, false, store.manifest().project_id);
    let canonical = fixture
        .spec
        .canonical_record_bytes()
        .expect("campaign record");
    persist_campaign(
        &mut store,
        &fixture.spec,
        fixture.spec.fingerprint(),
        &canonical,
    );
    let lease = store
        .acquire_campaign_session(fixture.spec.fingerprint())
        .expect("campaign lease");
    let first_lease = lease.lease_fingerprint();
    let mut journal =
        CampaignJournal::new(fixture.spec.clone(), &store, lease).expect("durable campaign");
    journal.start().expect("start");
    journal
        .schedule_blocked_factorial(&fixture.factorial())
        .expect("schedule trials");
    let order = scheduled_order(&journal);
    let attempt = TrialAttemptId::new();
    let permit = journal
        .reserve_trial(order[0], attempt)
        .expect("durable reservation");
    drop(permit);
    drop(journal);
    drop(store);

    let reopened = ProjectStore::open(&project_path).expect("reopen project");
    let lease = reopened
        .acquire_campaign_session(fixture.spec.fingerprint())
        .expect("fresh lease");
    assert_ne!(lease.lease_fingerprint(), first_lease);
    let mut resumed = CampaignJournal::resume(fixture.spec.clone(), &reopened, lease)
        .expect("resume and release reservation");
    assert_eq!(
        resumed.attempt_status(attempt),
        Some(CampaignTrialAttemptStatus::Released)
    );
    assert_eq!(resumed.snapshot().active_attempt_count(), 0);
    assert_eq!(
        resumed.snapshot().budget().reserved(),
        CampaignBudgetAmount::default()
    );
    assert_eq!(
        resumed.snapshot().budget().charged(),
        CampaignBudgetAmount::default()
    );

    let _retry_permit = resumed
        .reserve_trial(order[0], TrialAttemptId::new())
        .expect("retry after durable release");
    assert!(matches!(
        resumed.events().last().expect("retry event").kind(),
        CampaignEventKind::TrialReserved {
            attempt_ordinal: 2,
            ..
        }
    ));
}

#[test]
fn durable_campaign_resume_reissues_dispatch_once_then_interrupts_at_full_charge() {
    let directory = tempdir().expect("temporary project");
    let project_path = directory.path().to_path_buf();
    let (mut store, _) =
        ProjectStore::initialize(&project_path, "campaign dispatch recovery").expect("project");
    let fixture = CampaignFixture::new_for_project(2, false, store.manifest().project_id);
    let canonical = fixture
        .spec
        .canonical_record_bytes()
        .expect("campaign record");
    persist_campaign(
        &mut store,
        &fixture.spec,
        fixture.spec.fingerprint(),
        &canonical,
    );
    let lease = store
        .acquire_campaign_session(fixture.spec.fingerprint())
        .expect("campaign lease");
    let mut journal =
        CampaignJournal::new(fixture.spec.clone(), &store, lease).expect("durable campaign");
    journal.start().expect("start");
    journal
        .schedule_blocked_factorial(&fixture.factorial())
        .expect("schedule trials");
    let order = scheduled_order(&journal);
    let attempt = TrialAttemptId::new();
    let reserved = journal.reserve_trial(order[0], attempt).expect("reserve");
    let dispatched = journal.dispatch_reserved(reserved).expect("dispatch");
    let reservation = fixture.node(order[0]).budget_maximum();
    drop(dispatched);
    drop(journal);
    drop(store);

    let reopened = ProjectStore::open(&project_path).expect("reopen project");
    let lease = reopened
        .acquire_campaign_session(fixture.spec.fingerprint())
        .expect("fresh lease");
    let mut resumed =
        CampaignJournal::resume(fixture.spec, &reopened, lease).expect("resume dispatched trial");
    assert_eq!(
        resumed.attempt_status(attempt),
        Some(CampaignTrialAttemptStatus::Dispatched)
    );
    assert_eq!(resumed.snapshot().active_attempt_count(), 1);
    assert_eq!(resumed.snapshot().budget().reserved(), reservation);
    assert_eq!(
        resumed.snapshot().budget().charged(),
        CampaignBudgetAmount::default()
    );

    let mut recovered = resumed.take_recovered_dispatches();
    assert_eq!(recovered.len(), 1);
    assert!(resumed.take_recovered_dispatches().is_empty());
    resumed
        .reconcile_interrupted(
            recovered.pop().expect("one recovered dispatch"),
            BlobId::digest(b"recovered executor did not produce terminal evidence"),
        )
        .expect("conservative interruption");
    assert_eq!(
        resumed.attempt_status(attempt),
        Some(CampaignTrialAttemptStatus::Interrupted)
    );
    assert_eq!(resumed.snapshot().active_attempt_count(), 0);
    assert_eq!(
        resumed.snapshot().budget().reserved(),
        CampaignBudgetAmount::default()
    );
    assert_eq!(resumed.snapshot().budget().charged(), reservation);
    assert!(matches!(
        resumed.events().last().expect("recovery event").kind(),
        CampaignEventKind::TrialFinished {
            terminal: CampaignTrialTerminal::Interrupted { .. },
            actual_charge,
            ..
        } if *actual_charge == reservation
    ));
}

#[test]
fn direct_continuation_with_zero_controller_budget_persists_and_resumes() {
    let directory = tempdir().expect("temporary project");
    let project_path = directory.path().to_path_buf();
    let (mut store, _) =
        ProjectStore::initialize(&project_path, "controller-free campaign").expect("project");
    let fixture =
        CampaignFixture::new_for_project_with_controller(2, false, store.manifest().project_id, 0);
    let canonical = fixture
        .spec
        .canonical_record_bytes()
        .expect("campaign record");
    persist_campaign(
        &mut store,
        &fixture.spec,
        fixture.spec.fingerprint(),
        &canonical,
    );
    let lease = store
        .acquire_campaign_session(fixture.spec.fingerprint())
        .expect("campaign lease");
    let mut journal =
        CampaignJournal::new(fixture.spec.clone(), &store, lease).expect("durable campaign");
    journal.start().expect("start");
    journal
        .schedule_blocked_factorial(&fixture.factorial())
        .expect("schedule trials");
    let trial = scheduled_order(&journal)[0];
    let attempt = TrialAttemptId::new();
    let permit = journal
        .reserve_trial(trial, attempt)
        .expect("zero-controller reserve");
    assert_eq!(fixture.node(trial).budget_maximum().controller_tokens(), 0);
    assert!(matches!(
        journal.events().last().expect("reservation event").kind(),
        CampaignEventKind::TrialReserved { reservation, .. }
            if reservation.controller_tokens() == 0
    ));
    drop(permit);
    drop(journal);
    drop(store);

    let reopened = ProjectStore::open(&project_path).expect("reopen project");
    let lease = reopened
        .acquire_campaign_session(fixture.spec.fingerprint())
        .expect("fresh lease");
    let resumed = CampaignJournal::resume(fixture.spec, &reopened, lease)
        .expect("resume controller-free campaign");
    assert_eq!(
        resumed.attempt_status(attempt),
        Some(CampaignTrialAttemptStatus::Released)
    );
    assert_eq!(
        resumed.snapshot().budget().reserved(),
        CampaignBudgetAmount::default()
    );
    assert_eq!(
        resumed.snapshot().budget().charged(),
        CampaignBudgetAmount::default()
    );
}

#[test]
fn campaign_session_rejects_relabel_fake_record_and_cross_project_laundering() {
    let directory = tempdir().expect("temporary project");
    let (mut store, _) =
        ProjectStore::initialize(directory.path(), "relabel").expect("project store");
    let fixture = CampaignFixture::new_for_project(2, false, store.manifest().project_id);
    let canonical = fixture
        .spec
        .canonical_record_bytes()
        .expect("canonical campaign record");
    let relabeled = BlobId::digest(b"relabeled campaign subject");
    persist_campaign(&mut store, &fixture.spec, relabeled, &canonical);
    let lease = store
        .acquire_campaign_session(relabeled)
        .expect("persisted relabel claim");
    assert!(matches!(
        CampaignJournal::new(fixture.spec.clone(), &store, lease),
        Err(CampaignError::SessionLeaseMismatch)
    ));

    let directory = tempdir().expect("temporary project");
    let (mut store, _) =
        ProjectStore::initialize(directory.path(), "fake record").expect("project store");
    let fixture = CampaignFixture::new_for_project(2, false, store.manifest().project_id);
    persist_campaign(
        &mut store,
        &fixture.spec,
        fixture.spec.fingerprint(),
        b"fake campaign canonical record",
    );
    let lease = store
        .acquire_campaign_session(fixture.spec.fingerprint())
        .expect("persisted fake record claim");
    assert!(matches!(
        CampaignJournal::new(fixture.spec.clone(), &store, lease),
        Err(CampaignError::SessionRecordMismatch)
    ));

    let directory = tempdir().expect("temporary project");
    let (mut store, _) =
        ProjectStore::initialize(directory.path(), "wrong project").expect("project store");
    let fixture = CampaignFixture::new(2, false);
    assert_ne!(fixture.spec.project_id(), store.manifest().project_id);
    let canonical = fixture
        .spec
        .canonical_record_bytes()
        .expect("canonical campaign record");
    persist_campaign(
        &mut store,
        &fixture.spec,
        fixture.spec.fingerprint(),
        &canonical,
    );
    let lease = store
        .acquire_campaign_session(fixture.spec.fingerprint())
        .expect("cross-project persisted claim");
    assert!(matches!(
        CampaignJournal::new(fixture.spec, &store, lease),
        Err(CampaignError::SessionLeaseMismatch)
    ));
}

#[test]
fn campaign_session_rejects_exact_record_with_laundered_normalized_rows() {
    for laundering in [
        CampaignLaundering::CampaignMaximum,
        CampaignLaundering::TrialMaximum,
        CampaignLaundering::Dependencies,
    ] {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) = ProjectStore::initialize(directory.path(), "campaign laundering")
            .expect("project store");
        let fixture = CampaignFixture::new_for_project(2, true, store.manifest().project_id);
        let canonical = fixture
            .spec
            .canonical_record_bytes()
            .expect("canonical campaign record");
        persist_campaign_with_laundering(
            &mut store,
            &fixture.spec,
            fixture.spec.fingerprint(),
            &canonical,
            laundering,
        );
        let lease = store
            .acquire_campaign_session(fixture.spec.fingerprint())
            .expect("persisted laundered campaign rows");
        assert!(matches!(
            CampaignJournal::new(fixture.spec, &store, lease),
            Err(CampaignError::SessionNormalizedSnapshotMismatch)
        ));
    }
}

#[test]
fn frozen_campaign_binds_seed_source_trials_dependencies_and_budget() {
    let fixture = CampaignFixture::new(3, true);
    assert_eq!(fixture.spec.seed(), 42);
    assert_eq!(
        fixture.spec.project_input_fingerprint(),
        BlobId::digest(b"frozen project state")
    );
    assert_eq!(fixture.spec.trials().len(), 3);
    assert_eq!(
        fixture.node(fixture.trials[2]).dependencies(),
        &[fixture.trials[1]]
    );
    assert_eq!(fixture.spec.budget_limits().writer_tokens(), 1_000);
}

#[test]
fn session_reservation_dispatch_terminal_and_replay_are_strictly_separated() {
    let fixture = CampaignFixture::new(2, false);
    let (mut journal, order) = running_journal(&fixture);
    let charge = amount(10, 2, 1, 50);
    let attempt = complete_trial(&mut journal, order[0], charge);
    assert_eq!(
        journal.attempt_status(attempt),
        Some(CampaignTrialAttemptStatus::Completed)
    );
    assert_eq!(journal.snapshot().budget().charged(), charge);

    let batch = CampaignEventBatch::new(journal.events().to_vec()).expect("bounded batch");
    let encoded = serde_json::to_vec(&batch).expect("events JSON");
    let decoded: CampaignEventBatch = serde_json::from_slice(&encoded).expect("bounded decode");
    let checked = CampaignJournal::replay_batch(&fixture.spec, &decoded).expect("strict replay");
    assert_eq!(checked.snapshot(), journal.snapshot());
    assert_eq!(checked.snapshot().status(), CampaignStatus::Running);
}

#[test]
fn replay_rejects_tampering_reordering_and_search_claim_mutation() {
    let fixture = CampaignFixture::new(2, false);
    let (journal, _) = running_journal(&fixture);

    let mut value = serde_json::to_value(journal.events()).expect("JSON value");
    value[1]["sequence"] = serde_json::json!(99);
    let tampered: Vec<CampaignEvent> = serde_json::from_value(value).expect("wire shape");
    assert!(matches!(
        CampaignJournal::replay(&fixture.spec, &tampered),
        Err(CampaignError::EventSequence { .. })
    ));

    let mut reordered = journal.events().to_vec();
    reordered.swap(1, 2);
    assert!(CampaignJournal::replay(&fixture.spec, &reordered).is_err());

    let mut value = serde_json::to_value(journal.events()).expect("JSON value");
    value[2]["kind"]["receipt"]["effect"]["seed"] = serde_json::json!(7);
    let tampered: Vec<CampaignEvent> = serde_json::from_value(value).expect("wire shape");
    assert!(matches!(
        CampaignJournal::replay(&fixture.spec, &tampered),
        Err(
            CampaignError::Decision(SearchDecisionError::FingerprintMismatch)
                | CampaignError::EventFingerprint
        )
    ));
}

#[test]
fn replayed_claims_cannot_recover_live_dispatch_authority() {
    let fixture = CampaignFixture::new(2, false);
    let (mut journal, order) = running_journal(&fixture);
    let permit = journal
        .reserve_trial(order[0], TrialAttemptId::new())
        .expect("reserve");
    let permit = journal.dispatch_reserved(permit).expect("dispatch");
    let mut restored =
        CampaignJournal::restore_for_tests(fixture.spec.clone(), journal.events().to_vec())
            .expect("diagnostic restore");
    assert_eq!(
        restored.reconcile_interrupted(permit, BlobId::digest(b"replayed claim")),
        Err(CampaignError::ExecutionPermitMismatch)
    );
}

#[test]
fn frozen_seeded_order_is_dependency_safe_and_enforced() {
    let fixture = CampaignFixture::new(4, true);
    let (mut journal, order) = running_journal(&fixture);
    assert_eq!(order, fixture.trials);
    assert!(matches!(
        journal.reserve_trial(order[1], TrialAttemptId::new()),
        Err(CampaignError::TrialReservationOutOfOrder { expected, actual })
            if expected == order[0] && actual == order[1]
    ));
    complete_trial(&mut journal, order[0], amount(1, 1, 1, 1));
    assert_eq!(
        journal.trial_scheduling_status(order[1]),
        Ok(TrialSchedulingStatus::Runnable)
    );
}

#[test]
fn retry_requires_fresh_identity_and_terminal_charge_comes_only_from_live_lease() {
    let fixture = CampaignFixture::new(2, false);
    let (mut journal, order) = running_journal(&fixture);
    let first_id = TrialAttemptId::new();
    let reserved = journal.reserve_trial(order[0], first_id).expect("reserve");
    let dispatched = journal.dispatch_reserved(reserved).expect("dispatch");
    let terminal = VerifiedTrialTerminalLease::diagnostic_for_tests(
        &dispatched,
        CampaignTrialTerminal::Failed {
            diagnostic_fingerprint: BlobId::digest(b"failure"),
        },
        amount(1, 1, 0, 1),
        BlobId::digest(b"live failed terminal"),
    );
    journal
        .accept_trial_terminal(terminal)
        .expect("failed terminal");
    assert!(matches!(
        journal.reserve_trial(order[0], first_id),
        Err(CampaignError::DuplicateAttemptId(id)) if id == first_id
    ));
    let retry = journal
        .reserve_trial(order[0], TrialAttemptId::new())
        .expect("fresh retry");
    assert!(matches!(
        journal.events().last().expect("reservation").kind(),
        CampaignEventKind::TrialReserved {
            attempt_ordinal: 2,
            ..
        }
    ));
    journal
        .abandon_reserved(retry, BlobId::digest(b"test cleanup"))
        .expect("pre-dispatch release");
}

#[test]
fn permits_are_bound_to_exact_session_attempt_and_event_head() {
    let fixture = CampaignFixture::new(2, false);
    let (mut journal, order) = running_journal(&fixture);
    let first = journal
        .reserve_trial(order[0], TrialAttemptId::new())
        .expect("first reserve");
    let first = journal.dispatch_reserved(first).expect("first dispatch");
    let second = journal
        .reserve_trial(order[1], TrialAttemptId::new())
        .expect("second reserve");
    let second = journal.dispatch_reserved(second).expect("second dispatch");
    let mut wrong_terminal = VerifiedTrialTerminalLease::diagnostic_for_tests(
        &first,
        CampaignTrialTerminal::Completed {
            trial_journal_fingerprint: BlobId::digest(b"terminal"),
        },
        amount(1, 1, 1, 1),
        BlobId::digest(b"live terminal"),
    );
    wrong_terminal.attempt_id = second.attempt_id();
    assert_eq!(
        journal.accept_trial_terminal(wrong_terminal),
        Err(CampaignError::TrialTerminalLeaseMismatch)
    );
}

#[test]
fn interruption_consumes_dispatch_authority_and_charges_the_full_maximum() {
    let fixture = CampaignFixture::new(2, false);
    let (mut journal, order) = running_journal(&fixture);
    let maximum = fixture.node(order[0]).budget_maximum();
    let permit = journal
        .reserve_trial(order[0], TrialAttemptId::new())
        .expect("reserve");
    let permit = journal.dispatch_reserved(permit).expect("dispatch");
    journal
        .reconcile_interrupted(permit, BlobId::digest(b"crash"))
        .expect("conservative reconciliation");
    assert_eq!(journal.snapshot().budget().charged(), maximum);
    assert_eq!(
        journal.snapshot().budget().reserved(),
        CampaignBudgetAmount::default()
    );
}

#[test]
fn halving_is_pessimistic_seeded_and_stateful_across_rungs() {
    let fixture = CampaignFixture::new(4, false);
    let (mut journal, order) = running_journal(&fixture);
    let charge = amount(10, 2, 1, 50);
    for trial in &order {
        complete_trial(&mut journal, *trial, charge);
    }
    let outcomes = [
        HalvingOutcome::Scored {
            score: score(900_000),
        },
        HalvingOutcome::Scored {
            score: UnitScore::ZERO,
        },
        HalvingOutcome::Abstained,
        HalvingOutcome::HardGateRejected,
    ];
    let leases = order
        .iter()
        .zip(outcomes)
        .map(|(trial, outcome)| {
            evaluated_lease(EvaluatedLeaseInput {
                journal: &journal,
                fixture: &fixture,
                purpose: EvaluationLeasePurpose::Halving { rung: 1 },
                trial: *trial,
                occurrence_id: ArtifactId::new(),
                outcome,
                quality: match outcome {
                    HalvingOutcome::Scored { score } => score,
                    HalvingOutcome::Abstained | HalvingOutcome::HardGateRejected => UnitScore::ZERO,
                },
                actual_charge: charge,
                descriptors: Vec::new(),
                coverage_fingerprint: BlobId::digest(b"common evaluation coverage"),
            })
        })
        .collect();
    let first = journal
        .apply_successive_halving(1, leases, 2)
        .expect("first rung");
    assert!(matches!(
        first.survivors()[0].outcome(),
        HalvingOutcome::Scored { .. }
    ));
    assert!(matches!(
        first.survivors()[1].outcome(),
        HalvingOutcome::Scored { .. }
    ));
    assert_eq!(journal.snapshot().eliminated_trial_count(), 2);
    let eliminated = first.eliminated()[0].trial_fingerprint();
    assert!(matches!(
        journal.reserve_trial(eliminated, TrialAttemptId::new()),
        Err(CampaignError::TrialEliminated(trial)) if trial == eliminated
    ));

    let coverage = BlobId::digest(b"common evaluation coverage");
    let second_leases = first
        .survivors()
        .iter()
        .map(|candidate| {
            evaluated_lease(EvaluatedLeaseInput {
                journal: &journal,
                fixture: &fixture,
                purpose: EvaluationLeasePurpose::Halving { rung: 2 },
                trial: candidate.trial_fingerprint(),
                occurrence_id: ArtifactId::new(),
                outcome: HalvingOutcome::Scored {
                    score: score(800_000),
                },
                quality: score(800_000),
                actual_charge: charge,
                descriptors: Vec::new(),
                coverage_fingerprint: coverage,
            })
        })
        .collect();
    let second = journal
        .apply_successive_halving(2, second_leases, 1)
        .expect("second rung");
    assert_eq!(second.round(), 2);
}

#[test]
fn halving_rejects_coverage_and_prior_survivor_drift() {
    let fixture = CampaignFixture::new(3, false);
    let (mut journal, order) = running_journal(&fixture);
    let charge = amount(2, 2, 1, 10);
    for trial in &order {
        complete_trial(&mut journal, *trial, charge);
    }
    let leases = order
        .iter()
        .enumerate()
        .map(|(index, trial)| {
            evaluated_lease(EvaluatedLeaseInput {
                journal: &journal,
                fixture: &fixture,
                purpose: EvaluationLeasePurpose::Halving { rung: 1 },
                trial: *trial,
                occurrence_id: ArtifactId::new(),
                outcome: HalvingOutcome::Scored {
                    score: score(600_000),
                },
                quality: score(600_000),
                actual_charge: charge,
                descriptors: Vec::new(),
                coverage_fingerprint: BlobId::digest(format!("coverage {index}").as_bytes()),
            })
        })
        .collect();
    assert_eq!(
        journal.apply_successive_halving(1, leases, 1),
        Err(CampaignError::UnequalEvaluationCoverage)
    );
}

#[test]
fn scheduler_terminal_derives_halving_and_archive_obligations_from_live_state() {
    let fixture = CampaignFixture::new(3, false);
    let (mut journal, order) = running_journal(&fixture);
    let charge = amount(2, 0, 1, 10);
    for trial in &order {
        complete_trial(&mut journal, *trial, charge);
    }
    let coverage = BlobId::digest(b"terminal halving coverage");
    let first_leases = order
        .iter()
        .map(|trial| {
            evaluated_lease(EvaluatedLeaseInput {
                journal: &journal,
                fixture: &fixture,
                purpose: EvaluationLeasePurpose::Halving { rung: 1 },
                trial: *trial,
                occurrence_id: ArtifactId::new(),
                outcome: HalvingOutcome::Scored {
                    score: score(700_000),
                },
                quality: score(700_000),
                actual_charge: charge,
                descriptors: Vec::new(),
                coverage_fingerprint: coverage,
            })
        })
        .collect();
    let first = journal
        .apply_successive_halving(1, first_leases, 2)
        .expect("first halving rung");
    assert!(matches!(
        journal.verify_scheduler_terminal(),
        Err(CampaignError::RequiredWorkRemaining)
    ));
    let second_leases = first
        .survivors()
        .iter()
        .map(|candidate| {
            evaluated_lease(EvaluatedLeaseInput {
                journal: &journal,
                fixture: &fixture,
                purpose: EvaluationLeasePurpose::Halving { rung: 2 },
                trial: candidate.trial_fingerprint(),
                occurrence_id: candidate.occurrence_id(),
                outcome: HalvingOutcome::Scored {
                    score: score(710_000),
                },
                quality: score(710_000),
                actual_charge: charge,
                descriptors: Vec::new(),
                coverage_fingerprint: coverage,
            })
        })
        .collect();
    journal
        .apply_successive_halving(2, second_leases, 1)
        .expect("final halving rung");
    journal
        .verify_scheduler_terminal()
        .expect("one survivor leaves no halving work");

    let archive_fixture = CampaignFixture::new(2, false);
    let (mut archive_journal, archive_order) = running_journal(&archive_fixture);
    for trial in &archive_order {
        complete_trial(&mut archive_journal, *trial, charge);
    }
    let axis = DescriptorAxis::new(key("voice"), 4).expect("axis");
    let archive = MapElitesArchive::empty(archive_fixture.spec.fingerprint(), vec![axis.clone()])
        .expect("empty archive");
    archive_journal
        .initialize_archive(&archive)
        .expect("initialize archive");
    assert!(matches!(
        archive_journal.verify_scheduler_terminal(),
        Err(CampaignError::RequiredWorkRemaining)
    ));
    let candidate = archive_lease(&ArchiveLeaseInput {
        journal: &archive_journal,
        fixture: &archive_fixture,
        archive: &archive,
        trial: archive_order[0],
        occurrence_id: ArtifactId::new(),
        content: b"archive terminal candidate",
        quality: score(700_000),
        actual_charge: charge,
        axis: &axis,
    });
    archive_journal
        .consider_archive_candidate(&archive, candidate)
        .expect("advance archive");
    archive_journal
        .verify_scheduler_terminal()
        .expect("advanced archive has no invisible pending work");
}

#[test]
fn pressure_continue_blocks_completion_until_exact_next_step_stops() {
    let fixture = CampaignFixture::new(4, false);
    let (mut journal, order) = running_journal(&fixture);
    let charge = amount(1, 1, 1, 10);
    for trial in &order {
        complete_trial(&mut journal, *trial, charge);
    }
    let family = BlobId::digest(b"N pressure family");
    let policy = PressurePolicy::new(score(10_000), 10, 8).expect("policy");
    let first = VerifiedPressureCurveLease::diagnostic_for_tests(DiagnosticPressureCurveInput {
        campaign_fingerprint: fixture.spec.fingerprint(),
        session_id: journal.session_id_for_tests(),
        family_fingerprint: family,
        affected_trials: order[..2].to_vec(),
        policy,
        points: vec![
            PressurePoint::new(1, Some(score(500_000)), 0, compute_units(charge)).expect("point"),
            PressurePoint::new(2, Some(score(510_000)), 0, compute_units(charge) * 2)
                .expect("point"),
        ],
        execution_charges: order[..2].iter().map(|trial| (*trial, charge)).collect(),
        prior_decision_fingerprint: None,
    });
    assert_eq!(
        journal
            .apply_pressure_curve(first)
            .expect("continue")
            .action(),
        PressureAction::Continue { next_level: 3 }
    );
    assert!(matches!(
        journal.verify_scheduler_terminal(),
        Err(CampaignError::RequiredWorkRemaining)
    ));
    let prior = last_decision_fingerprint(&journal);
    let stop = VerifiedPressureCurveLease::diagnostic_for_tests(DiagnosticPressureCurveInput {
        campaign_fingerprint: fixture.spec.fingerprint(),
        session_id: journal.session_id_for_tests(),
        family_fingerprint: family,
        affected_trials: order.clone(),
        policy,
        points: vec![
            PressurePoint::new(2, Some(score(510_000)), 0, compute_units(charge) * 2)
                .expect("point"),
            PressurePoint::new(3, None, 0, compute_units(charge) * 4).expect("point"),
        ],
        execution_charges: order.iter().map(|trial| (*trial, charge)).collect(),
        prior_decision_fingerprint: Some(prior),
    });
    assert!(matches!(
        journal.apply_pressure_curve(stop).expect("stop").action(),
        PressureAction::Stop { .. }
    ));
    assert_eq!(journal.snapshot().stopped_trial_count(), 4);
    let terminal = journal
        .verify_scheduler_terminal()
        .expect("live terminal state");
    journal.complete(terminal).expect("verified closure");
    assert_eq!(journal.snapshot().status(), CampaignStatus::Completed);
}

#[test]
fn exact_nested_n_pool_is_live_evidence_bound() {
    let fixture = CampaignFixture::new(2, false);
    let (mut journal, order) = running_journal(&fixture);
    complete_trial(&mut journal, order[0], amount(1, 1, 1, 10));
    let occurrences = (0..32).map(|_| ArtifactId::new()).collect::<Vec<_>>();
    let lease = VerifiedNestedPoolLease::diagnostic_for_tests(
        fixture.spec.fingerprint(),
        journal.session_id_for_tests(),
        fixture.case_id,
        fixture.node(order[0]).treatment_fingerprint(),
        occurrences.clone(),
    );
    let plan = journal.record_nested_pool(lease).expect("nested pool");
    assert_eq!(
        plan.pools().iter().map(NestedPool::n).collect::<Vec<_>>(),
        vec![1, 2, 4, 8, 16, 32]
    );
    for pool in plan.pools() {
        assert_eq!(pool.ordered_occurrences(), &occurrences[..pool.n()]);
    }
    assert_ne!(plan.exact_boundaries_fingerprint(), plan.fingerprint());
}

#[test]
fn map_elites_is_parent_bound_pareto_bounded_and_append_only() {
    let fixture = CampaignFixture::new(2, false);
    let (mut journal, order) = running_journal(&fixture);
    let expensive = amount(10, 10, 1, 100);
    let cheap = amount(1, 1, 1, 10);
    complete_trial(&mut journal, order[0], expensive);
    complete_trial(&mut journal, order[1], cheap);
    let axis = DescriptorAxis::new(key("pacing"), 4).expect("axis");
    let mut archive = MapElitesArchive::empty(fixture.spec.fingerprint(), vec![axis.clone()])
        .expect("empty archive");
    journal.initialize_archive(&archive).expect("initialize");

    let first_occurrence = ArtifactId::new();
    let first = archive_lease(&ArchiveLeaseInput {
        journal: &journal,
        fixture: &fixture,
        archive: &archive,
        trial: order[0],
        occurrence_id: first_occurrence,
        content: b"first",
        quality: score(500_000),
        actual_charge: expensive,
        axis: &axis,
    });
    archive = journal
        .consider_archive_candidate(&archive, first)
        .expect("first update")
        .into_snapshot();
    assert_eq!(archive.generation(), 1);

    let second_occurrence = ArtifactId::new();
    let second = archive_lease(&ArchiveLeaseInput {
        journal: &journal,
        fixture: &fixture,
        archive: &archive,
        trial: order[1],
        occurrence_id: second_occurrence,
        content: b"second",
        quality: score(500_000),
        actual_charge: cheap,
        axis: &axis,
    });
    let update = journal
        .consider_archive_candidate(&archive, second)
        .expect("cheaper dominates");
    assert_eq!(
        update.decision().kind(),
        ArchiveDecisionKind::ReplacedDominated
    );
    archive = update.into_snapshot();
    assert_eq!(archive.generation(), 2);
    assert_eq!(archive.seen_occurrences().count(), 2);
    assert_eq!(archive.global_pareto().len(), 1);
    assert_eq!(
        archive.global_pareto()[0].occurrence_id(),
        second_occurrence
    );
    assert!(archive.occurrence_commitment(first_occurrence).is_some());

    let duplicate = archive_lease(&ArchiveLeaseInput {
        journal: &journal,
        fixture: &fixture,
        archive: &archive,
        trial: order[0],
        occurrence_id: first_occurrence,
        content: b"first",
        quality: score(500_000),
        actual_charge: expensive,
        axis: &axis,
    });
    let event_count = journal.events().len();
    let duplicate = journal
        .consider_archive_candidate(&archive, duplicate)
        .expect("idempotent duplicate");
    assert_eq!(
        duplicate.decision().kind(),
        ArchiveDecisionKind::AlreadyPresent
    );
    assert_eq!(journal.events().len(), event_count);
}

#[test]
fn archive_rejects_changed_evidence_for_a_seen_occurrence() {
    let fixture = CampaignFixture::new(2, false);
    let (mut journal, order) = running_journal(&fixture);
    let charge = amount(1, 1, 1, 10);
    complete_trial(&mut journal, order[0], charge);
    let axis = DescriptorAxis::new(key("tension"), 2).expect("axis");
    let root =
        MapElitesArchive::empty(fixture.spec.fingerprint(), vec![axis.clone()]).expect("root");
    journal.initialize_archive(&root).expect("initialize");
    let occurrence = ArtifactId::new();
    let first = archive_lease(&ArchiveLeaseInput {
        journal: &journal,
        fixture: &fixture,
        archive: &root,
        trial: order[0],
        occurrence_id: occurrence,
        content: b"original",
        quality: score(500_000),
        actual_charge: charge,
        axis: &axis,
    });
    let archive = journal
        .consider_archive_candidate(&root, first)
        .expect("insert")
        .into_snapshot();
    let relabeled = archive_lease(&ArchiveLeaseInput {
        journal: &journal,
        fixture: &fixture,
        archive: &archive,
        trial: order[0],
        occurrence_id: occurrence,
        content: b"changed",
        quality: score(500_000),
        actual_charge: charge,
        axis: &axis,
    });
    assert_eq!(
        journal.consider_archive_candidate(&archive, relabeled),
        Err(CampaignError::Archive(ArchiveError::OccurrenceConflict(
            occurrence
        )))
    );
}

#[test]
fn completion_requires_exact_current_head_and_no_required_work() {
    let fixture = CampaignFixture::new(2, false);
    let (mut journal, order) = running_journal(&fixture);
    complete_trial(&mut journal, order[0], amount(1, 1, 1, 10));
    assert!(matches!(
        journal.verify_scheduler_terminal(),
        Err(CampaignError::RequiredWorkRemaining)
    ));
    complete_trial(&mut journal, order[1], amount(1, 1, 1, 10));
    let stale = journal
        .verify_scheduler_terminal()
        .expect("terminal before new decision");
    journal
        .request_pause(BlobId::digest(b"stale terminal lease check"))
        .expect("new event");
    assert_eq!(
        journal.complete(stale),
        Err(CampaignError::SchedulerTerminalLeaseMismatch)
    );
    journal.acknowledge_paused().expect("paused");
    journal.resume_running().expect("running again");
    let terminal = journal
        .verify_scheduler_terminal()
        .expect("current terminal state");
    journal.complete(terminal).expect("current terminal lease");
}

#[test]
fn pure_halving_and_factorial_math_are_input_order_stable() {
    let first = HalvingCandidate::new(
        BlobId::digest(b"a"),
        ArtifactId::new(),
        HalvingOutcome::Scored {
            score: score(500_000),
        },
    );
    let second = HalvingCandidate::new(
        BlobId::digest(b"b"),
        ArtifactId::new(),
        HalvingOutcome::Scored {
            score: score(500_000),
        },
    );
    let forward =
        SuccessiveHalvingDecision::decide(42, 1, vec![first, second], 1).expect("forward");
    let reverse =
        SuccessiveHalvingDecision::decide(42, 1, vec![second, first], 1).expect("reverse");
    assert_eq!(forward, reverse);
    assert!(forward.tie_broken_at_cutoff());

    let temperature = key("temperature");
    let topology = key("topology");
    let baseline = arm(
        b"base trial",
        b"base treatment",
        &[(temperature.clone(), b"0.8"), (topology.clone(), b"direct")],
    );
    let confounded = arm(
        b"confounded trial",
        b"confounded treatment",
        &[(temperature.clone(), b"1.1"), (topology, b"bookfront")],
    );
    assert_eq!(
        BlockedFactorialPlan::new(
            BlobId::digest(b"case block"),
            temperature,
            baseline,
            vec![confounded],
        ),
        Err(FactorialError::ConfoundedArm { differing_axes: 2 })
    );
}

fn persist_campaign(
    store: &mut ProjectStore,
    spec: &FrozenCampaignSpec,
    claimed_subject: BlobId,
    canonical_record_bytes: &[u8],
) {
    persist_campaign_with_laundering(
        store,
        spec,
        claimed_subject,
        canonical_record_bytes,
        CampaignLaundering::None,
    );
}

#[derive(Clone, Copy, Debug)]
enum CampaignLaundering {
    None,
    CampaignMaximum,
    TrialMaximum,
    Dependencies,
}

fn persist_campaign_with_laundering(
    store: &mut ProjectStore,
    spec: &FrozenCampaignSpec,
    claimed_subject: BlobId,
    canonical_record_bytes: &[u8],
    laundering: CampaignLaundering,
) {
    let mut manifest_source = campaign_manifest(BlobId::digest(b"model bindings"));
    if spec.budget_limits().controller_tokens() == 0 {
        manifest_source =
            manifest_source.replace("max_controller_tokens = 1000", "max_controller_tokens = 0");
    }
    let maximum = spec.budget_limits();
    let mut persisted_maximum = ResearchBudgetMaximum {
        writer_tokens: maximum.writer_tokens(),
        controller_tokens: maximum.controller_tokens(),
        evaluations: maximum.evaluations(),
        wall_time_ms: maximum.wall_time_ms(),
    };
    if matches!(laundering, CampaignLaundering::CampaignMaximum) {
        persisted_maximum.wall_time_ms += 1;
    }
    store
        .persist_frozen_campaign(FrozenCampaignPersistence {
            campaign_id: spec.campaign_id(),
            campaign_fingerprint: claimed_subject,
            project_id: store.manifest().project_id,
            manifest_source_bytes: manifest_source.as_bytes(),
            manifest_fingerprint: spec.manifest_fingerprint(),
            project_input_fingerprint: spec.project_input_fingerprint(),
            seed: spec.seed(),
            maximum: persisted_maximum,
            canonical_record_bytes,
        })
        .expect("persist frozen campaign claim");

    for (index, trial) in spec.trials().iter().enumerate() {
        persist_campaign_trial(store, spec, trial, index, laundering);
    }
    let dependency_lists = spec
        .trials()
        .iter()
        .map(|trial| {
            if matches!(laundering, CampaignLaundering::Dependencies) {
                Vec::new()
            } else {
                trial.dependencies().to_vec()
            }
        })
        .collect::<Vec<_>>();
    let topology = spec
        .trials()
        .iter()
        .zip(&dependency_lists)
        .map(
            |(trial, dependencies)| FrozenCampaignTrialTopologyPersistence {
                trial_fingerprint: trial.trial_fingerprint(),
                dependencies,
            },
        )
        .collect::<Vec<_>>();
    store
        .persist_frozen_campaign_topology(FrozenCampaignTopologyPersistence {
            campaign_id: spec.campaign_id(),
            campaign_fingerprint: claimed_subject,
            trials: &topology,
        })
        .expect("persist complete campaign topology");
}

fn persist_campaign_trial(
    store: &mut ProjectStore,
    campaign: &FrozenCampaignSpec,
    trial: &FrozenCampaignTrial,
    index: usize,
    laundering: CampaignLaundering,
) {
    let graph = campaign_trial_stage_graph(trial.trial_fingerprint());
    let stage_records = graph
        .stages()
        .iter()
        .map(|stage| canonical_stage_record_bytes(stage).expect("canonical stage record"))
        .collect::<Vec<_>>();
    let stages = graph
        .stages()
        .iter()
        .zip(&stage_records)
        .map(|(stage, record)| FrozenStagePersistence {
            stage_id: stage.id(),
            stage: stage.stage(),
            stage_spec_fingerprint: stage.spec_fingerprint(),
            maximum: research_budget_from_trial_stage(
                stage.stage(),
                trial.budget_maximum().controller_tokens() > 0,
            ),
            dependencies: stage.dependencies(),
            canonical_record_bytes: record,
        })
        .collect::<Vec<_>>();
    let maximum = trial.budget_maximum();
    let mut persisted_maximum = research_budget_from_campaign_amount(maximum);
    if index == 0 && matches!(laundering, CampaignLaundering::TrialMaximum) {
        persisted_maximum.wall_time_ms -= 1;
    }
    let record = format!(
        "{{\"format\":\"diagnostic.campaign-trial.v1\",\"trial\":\"{}\"}}",
        trial.trial_fingerprint()
    );
    store
        .persist_frozen_trial(FrozenTrialPersistence {
            campaign_id: campaign.campaign_id(),
            trial_fingerprint: trial.trial_fingerprint(),
            trial_case_id: trial.case_id(),
            treatment_fingerprint: trial.treatment_fingerprint(),
            prompt_content_fingerprint: BlobId::digest(
                format!("prompt {}", trial.trial_fingerprint()).as_bytes(),
            ),
            model_binding_fingerprint: BlobId::digest(
                format!("model {}", trial.trial_fingerprint()).as_bytes(),
            ),
            expected_writer_call_count: 1,
            declared_writer_token_maximum: 1,
            maximum: persisted_maximum,
            canonical_record_bytes: record.as_bytes(),
            stages: &stages,
        })
        .expect("persist campaign trial node");
}

const fn research_budget_from_campaign_amount(
    amount: CampaignBudgetAmount,
) -> ResearchBudgetMaximum {
    ResearchBudgetMaximum {
        writer_tokens: amount.writer_tokens(),
        controller_tokens: amount.controller_tokens(),
        evaluations: amount.evaluations(),
        wall_time_ms: amount.wall_time_ms(),
    }
}

fn research_budget_from_trial_stage(
    stage: FrozenTrialStage,
    controller_enabled: bool,
) -> ResearchBudgetMaximum {
    let amount = match stage {
        FrozenTrialStage::BacktranslateMask | FrozenTrialStage::Plan => {
            BudgetAmount::new(0, u64::from(controller_enabled), 0, 1)
                .expect("optional controller maximum")
        }
        FrozenTrialStage::Generate => BudgetAmount::new(1, 0, 0, 1).expect("writer maximum"),
        FrozenTrialStage::Evaluate => {
            BudgetAmount::new(0, u64::from(controller_enabled), 1, 1).expect("evaluation maximum")
        }
        FrozenTrialStage::FreezeInputs
        | FrozenTrialStage::Retrieve
        | FrozenTrialStage::CompilePrompt
        | FrozenTrialStage::Admit
        | FrozenTrialStage::Assemble
        | FrozenTrialStage::Gate
        | FrozenTrialStage::Describe
        | FrozenTrialStage::Archive => BudgetAmount::new(0, 0, 0, 1).expect("pure maximum"),
    };
    ResearchBudgetMaximum {
        writer_tokens: amount.writer_tokens(),
        controller_tokens: amount.controller_tokens(),
        evaluations: amount.evaluations(),
        wall_time_ms: amount.wall_time_ms(),
    }
}

fn campaign_trial_stage_graph(trial_fingerprint: BlobId) -> StageGraph {
    let mut ids = BTreeMap::new();
    for stage in FrozenTrialStage::ALL {
        ids.insert(stage, StageId::new());
    }
    let stages = FrozenTrialStage::ALL
        .into_iter()
        .map(|stage| {
            let dependencies = canonical_stage_dependencies(stage)
                .iter()
                .map(|dependency| ids[dependency])
                .collect::<Vec<_>>();
            let mut evidence = Vec::with_capacity(33);
            evidence.extend_from_slice(trial_fingerprint.as_bytes());
            evidence.push(stage as u8);
            FrozenStageSpec::new(ids[&stage], stage, BlobId::digest(&evidence), dependencies)
                .expect("canonical stage")
        })
        .collect::<Vec<_>>();
    StageGraph::new(StageGraphId::new(), stages, ids[&FrozenTrialStage::Archive])
        .expect("canonical stage graph")
}

const fn canonical_stage_dependencies(stage: FrozenTrialStage) -> &'static [FrozenTrialStage] {
    match stage {
        FrozenTrialStage::FreezeInputs => &[],
        FrozenTrialStage::BacktranslateMask => &[FrozenTrialStage::FreezeInputs],
        FrozenTrialStage::Plan => &[
            FrozenTrialStage::FreezeInputs,
            FrozenTrialStage::BacktranslateMask,
        ],
        FrozenTrialStage::Retrieve => &[FrozenTrialStage::FreezeInputs, FrozenTrialStage::Plan],
        FrozenTrialStage::CompilePrompt => &[
            FrozenTrialStage::FreezeInputs,
            FrozenTrialStage::BacktranslateMask,
            FrozenTrialStage::Plan,
            FrozenTrialStage::Retrieve,
        ],
        FrozenTrialStage::Generate => &[FrozenTrialStage::CompilePrompt],
        FrozenTrialStage::Admit => &[FrozenTrialStage::Generate],
        FrozenTrialStage::Assemble => &[FrozenTrialStage::Admit],
        FrozenTrialStage::Gate => &[FrozenTrialStage::Assemble],
        FrozenTrialStage::Evaluate => &[FrozenTrialStage::Gate],
        FrozenTrialStage::Describe => &[FrozenTrialStage::Evaluate],
        FrozenTrialStage::Archive => &[FrozenTrialStage::Evaluate, FrozenTrialStage::Describe],
    }
}

fn running_journal(fixture: &CampaignFixture) -> (CampaignJournal, Vec<BlobId>) {
    let mut journal = CampaignJournal::diagnostic_for_tests(fixture.spec.clone()).expect("journal");
    journal.start().expect("start");
    journal
        .schedule_blocked_factorial(&fixture.factorial())
        .expect("schedule factorial");
    let order = scheduled_order(&journal);
    (journal, order)
}

fn scheduled_order(journal: &CampaignJournal) -> Vec<BlobId> {
    match journal.events().last().expect("decision").kind() {
        CampaignEventKind::SearchDecisionRecorded { receipt } => match receipt.effect() {
            SearchDecisionEffect::BlockedFactorialScheduled {
                seeded_trial_order, ..
            } => seeded_trial_order.to_vec(),
            _ => panic!("unexpected search effect"),
        },
        _ => panic!("unexpected campaign event"),
    }
}

fn complete_trial(
    journal: &mut CampaignJournal,
    trial: BlobId,
    charge: CampaignBudgetAmount,
) -> TrialAttemptId {
    let attempt = TrialAttemptId::new();
    let permit = journal
        .reserve_trial(trial, attempt)
        .expect("reserve maximum");
    let permit = journal.dispatch_reserved(permit).expect("dispatch");
    let terminal = VerifiedTrialTerminalLease::diagnostic_for_tests(
        &permit,
        CampaignTrialTerminal::Completed {
            trial_journal_fingerprint: BlobId::digest(b"terminal trial journal"),
        },
        charge,
        BlobId::digest(b"live terminal evidence"),
    );
    journal.accept_trial_terminal(terminal).expect("terminal");
    attempt
}

struct EvaluatedLeaseInput<'a> {
    journal: &'a CampaignJournal,
    fixture: &'a CampaignFixture,
    purpose: EvaluationLeasePurpose,
    trial: BlobId,
    occurrence_id: ArtifactId,
    outcome: HalvingOutcome,
    quality: UnitScore,
    actual_charge: CampaignBudgetAmount,
    descriptors: Vec<(ManifestKey, UnitScore)>,
    coverage_fingerprint: BlobId,
}

fn evaluated_lease(input: EvaluatedLeaseInput<'_>) -> VerifiedEvaluatedCandidateLease {
    let node = input.fixture.node(input.trial);
    VerifiedEvaluatedCandidateLease::diagnostic_for_tests(DiagnosticEvaluatedCandidateInput {
        campaign_fingerprint: input.fixture.spec.fingerprint(),
        session_id: input.journal.session_id_for_tests(),
        purpose: input.purpose,
        trial_fingerprint: input.trial,
        case_id: node.case_id(),
        treatment_fingerprint: node.treatment_fingerprint(),
        occurrence_id: input.occurrence_id,
        content_blob_id: BlobId::digest(input.occurrence_id.to_string().as_bytes()),
        coverage_fingerprint: input.coverage_fingerprint,
        actual_charge: input.actual_charge,
        outcome: input.outcome,
        quality: input.quality,
        descriptors: input.descriptors,
    })
}

struct ArchiveLeaseInput<'a> {
    journal: &'a CampaignJournal,
    fixture: &'a CampaignFixture,
    archive: &'a MapElitesArchive,
    trial: BlobId,
    occurrence_id: ArtifactId,
    content: &'a [u8],
    quality: UnitScore,
    actual_charge: CampaignBudgetAmount,
    axis: &'a DescriptorAxis,
}

fn archive_lease(input: &ArchiveLeaseInput<'_>) -> VerifiedEvaluatedCandidateLease {
    let node = input.fixture.node(input.trial);
    let mut lease =
        VerifiedEvaluatedCandidateLease::diagnostic_for_tests(DiagnosticEvaluatedCandidateInput {
            campaign_fingerprint: input.fixture.spec.fingerprint(),
            session_id: input.journal.session_id_for_tests(),
            purpose: EvaluationLeasePurpose::Archive {
                parent_fingerprint: input.archive.fingerprint(),
            },
            trial_fingerprint: input.trial,
            case_id: node.case_id(),
            treatment_fingerprint: node.treatment_fingerprint(),
            occurrence_id: input.occurrence_id,
            content_blob_id: BlobId::digest(input.content),
            coverage_fingerprint: BlobId::digest(b"archive coverage"),
            actual_charge: input.actual_charge,
            outcome: HalvingOutcome::Scored {
                score: input.quality,
            },
            quality: input.quality,
            descriptors: vec![(input.axis.id().clone(), score(500_000))],
        });
    lease.evaluation_receipt_fingerprint = BlobId::digest(input.content);
    lease
}

fn last_decision_fingerprint(journal: &CampaignJournal) -> BlobId {
    match journal.events().last().expect("decision event").kind() {
        CampaignEventKind::SearchDecisionRecorded { receipt } => receipt.fingerprint(),
        _ => panic!("expected decision event"),
    }
}

fn amount(
    writer_tokens: u64,
    controller_tokens: u64,
    evaluations: u32,
    wall_time_ms: u64,
) -> CampaignBudgetAmount {
    CampaignBudgetAmount::new(writer_tokens, controller_tokens, evaluations, wall_time_ms)
        .expect("bounded amount")
}

fn score(value: u32) -> UnitScore {
    UnitScore::from_millionths(value).expect("bounded score")
}

fn key(value: &str) -> ManifestKey {
    ManifestKey::new(value).expect("bounded key")
}

fn arm(trial: &[u8], treatment: &[u8], settings: &[(ManifestKey, &[u8])]) -> FactorialArm {
    FactorialArm::new(
        BlobId::digest(trial),
        BlobId::digest(treatment),
        settings
            .iter()
            .map(|(axis, value)| FactorSetting::new(axis.clone(), BlobId::digest(value)))
            .collect(),
    )
    .expect("factorial arm")
}

fn campaign_manifest(model_bindings: BlobId) -> String {
    format!(
        r#"format = "loom.campaign.v1"
name = "campaign-test"
description = "Immutable exploratory trial occurrences"
seed = 42
selection = "successive_halving"

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
prompt_topology = "exact_direct_continuation"
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
