use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
};

use loom_research_types::{ManifestKey, TrialCaseId};
use loom_types::BlobId;

use super::*;

const CASES_PER_FUNCTION: usize = 6;

#[derive(Clone)]
struct Fixture {
    source: Vec<u8>,
    baseline: FrozenDirectContinuationN1Baseline,
    contenders: Vec<FrozenCandidateProfile>,
    cases: Vec<BenchmarkCaseBinding>,
    budgets: Vec<ArmBudgetAllocation>,
    review: FrontierReviewBinding,
}

impl Fixture {
    fn inputs(&self) -> BenchmarkSealInputs {
        BenchmarkSealInputs {
            baseline: self.baseline.clone(),
            contenders: self.contenders.clone(),
            cases: self.cases.clone(),
            arm_budgets: self.budgets.clone(),
            review: self.review.clone(),
        }
    }

    fn compile(&self) -> (BenchmarkSeal, AssignmentLabelMap) {
        BenchmarkSeal::compile(&self.source, self.inputs()).expect("valid benchmark fixture")
    }
}

fn key(value: impl Into<String>) -> ManifestKey {
    ManifestKey::new(value).expect("bounded manifest key")
}

fn hash(value: impl AsRef<[u8]>) -> BlobId {
    BlobId::digest(value.as_ref())
}

fn candidate(id: &str, selected_n: u8, review_model: BlobId, salt: &str) -> FrozenCandidateProfile {
    let components = HarnessProfileComponents::new(HarnessProfileComponentInputs {
        models: vec![ProfileModelBinding::new(
            ProfileModelRole::BaseWriter,
            hash(format!("model-{salt}")),
            hash(format!("tokenizer-{salt}")),
            hash(format!("adapters-{salt}")),
        )],
        prompt_fingerprint: hash(format!("prompt-{salt}")),
        sampler_fingerprint: hash(format!("sampler-{salt}")),
        control_fingerprint: hash(format!("control-{salt}")),
        ranker_fingerprint: hash(format!("ranker-{salt}")),
        search_fingerprint: hash(format!("search-{salt}")),
        evaluator_fingerprints: vec![review_model, hash(format!("local-eval-{salt}"))],
        corpus_fingerprints: vec![hash(format!("corpus-{salt}"))],
        selected_n,
    })
    .expect("valid profile components");
    FrozenCandidateProfile::new(key(id), components)
}

fn baseline(review_model: BlobId) -> FrozenDirectContinuationN1Baseline {
    let candidate = candidate("direct-baseline", 1, review_model, "baseline");
    let lease = VerifiedDirectContinuationBaselineLease::for_test(
        candidate.fingerprint(),
        hash("exact-direct-continuation-treatment"),
        hash("prompt-treatment-verifier-receipt"),
    );
    FrozenDirectContinuationN1Baseline::from_verified(candidate, lease)
        .expect("verified direct-continuation N=1 baseline")
}

fn fixture(contender_count: usize) -> Fixture {
    let review_model = hash("frontier-model");
    let baseline = baseline(review_model);
    let contenders = (0..contender_count)
        .map(|index| {
            candidate(
                &format!("contender-{index}"),
                if index % 2 == 0 { 8 } else { 16 },
                review_model,
                &format!("contender-{index}"),
            )
        })
        .collect::<Vec<_>>();
    let mut cases = Vec::new();
    let mut function_case_ids = Vec::new();
    for function in BuiltInGenreFunction::ALL {
        let mut ids = Vec::new();
        for case_index in 0..CASES_PER_FUNCTION {
            let name = format!("{}-{case_index:02}", function.id());
            ids.push(name.clone());
            let identity = format!("{}-{case_index}", function.id());
            cases.push(BenchmarkCaseBinding::new(
                key(name),
                TrialCaseId::new(),
                function,
                hash(format!("work-{identity}")),
                hash(format!("ancestry-{identity}")),
                hash(format!("source-{identity}")),
            ));
        }
        function_case_ids.push((function, ids));
    }
    let budget = ArmBudget::new(2_000_000, 2_000_000, 100_000, 100_000, 100_000_000)
        .expect("valid equal arm budget");
    let budgets = std::iter::once(ArmBudgetAllocation::new(baseline.fingerprint(), budget))
        .chain(
            contenders
                .iter()
                .map(|profile| ArmBudgetAllocation::new(profile.fingerprint(), budget)),
        )
        .collect::<Vec<_>>();
    let review = FrontierReviewBinding::pinned(
        key(REQUIRED_FRONTIER_MODEL),
        review_model,
        hash("frontier-review-protocol"),
        hash("codex-cli-binary"),
        hash("codex-cli-version"),
    )
    .expect("exact pinned frontier reviewer");
    let mut source = format!(
        "format = \"loom.benchmark.v1\"\nname = \"sealed-confirmation\"\ndescription = \"Five function nested-N confirmation\"\nseed = 938475\nnested_n = [1, 2, 4, 8, 16, 32]\n\n[campaign]\nformat = \"loom.campaign.v1\"\nartifact_sha256 = \"{}\"\n\n[review]\nfrontier_model = \"{}\"\nfresh_runs = 3\norder_permutation_cells = 4\n",
        hash("frozen-campaign"),
        REQUIRED_FRONTIER_MODEL,
    );
    for contender in &contenders {
        writeln!(
            source,
            "\n[[contenders]]\nid = \"{}\"\nprofile_sha256 = \"{}\"",
            contender.id(),
            contender.fingerprint(),
        )
        .expect("writing to String cannot fail");
    }
    for (function, ids) in function_case_ids {
        let quoted_ids = ids
            .iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            source,
            "\n[[functions]]\nid = \"{}\"\ncase_ids = [{quoted_ids}]",
            function.id(),
        )
        .expect("writing to String cannot fail");
    }
    Fixture {
        source: source.into_bytes(),
        baseline,
        contenders,
        cases,
        budgets,
        review,
    }
}

#[test]
fn crate_dependency_boundary_is_headless_and_search_free() {
    let manifest = include_str!("../Cargo.toml");
    for forbidden in [
        "loom-campaign",
        "loom-search",
        "loom-store",
        "loom-inference",
        "loom-backend",
        "tauri",
        "tokio",
        "subprocess",
    ] {
        assert!(
            !manifest.lines().any(|line| line.starts_with(forbidden)),
            "forbidden dependency {forbidden}",
        );
    }
}

#[test]
fn frozen_candidates_bind_all_components_without_review_authority() {
    let review = hash("review");
    let first = candidate("profile", 8, review, "a");
    let second = candidate("profile", 8, review, "b");
    assert_ne!(
        first.components().fingerprint(),
        second.components().fingerprint(),
    );
    assert_ne!(first.fingerprint(), second.fingerprint());

    let bad_n = HarnessProfileComponents::new(HarnessProfileComponentInputs {
        models: vec![ProfileModelBinding::new(
            ProfileModelRole::BaseWriter,
            hash("model"),
            hash("tokenizer"),
            hash("adapter"),
        )],
        prompt_fingerprint: hash("prompt"),
        sampler_fingerprint: hash("sampler"),
        control_fingerprint: hash("control"),
        ranker_fingerprint: hash("ranker"),
        search_fingerprint: hash("search"),
        evaluator_fingerprints: vec![review],
        corpus_fingerprints: vec![hash("corpus")],
        selected_n: 3,
    });
    assert!(matches!(bad_n, Err(ProfileError::UnsupportedSelectedN(3))));

    let n8 = candidate("not-a-baseline", 8, review, "n8");
    let n8_lease = VerifiedDirectContinuationBaselineLease::for_test(
        n8.fingerprint(),
        hash("direct-treatment"),
        hash("verifier"),
    );
    assert!(matches!(
        FrozenDirectContinuationN1Baseline::from_verified(n8, n8_lease),
        Err(ProfileError::BaselineMustUseN1),
    ));

    let n1 = candidate("baseline", 1, review, "n1");
    let wrong_lease = VerifiedDirectContinuationBaselineLease::for_test(
        hash("another-candidate"),
        hash("direct-treatment"),
        hash("verifier"),
    );
    assert!(matches!(
        FrozenDirectContinuationN1Baseline::from_verified(n1, wrong_lease),
        Err(ProfileError::BaselineLeaseMismatch),
    ));
}

#[test]
fn seal_is_deterministic_complete_and_keeps_mapping_separate() {
    let fixture = fixture(2);
    let (first, first_map) = fixture.compile();
    let (second, second_map) = fixture.compile();
    let expected = 2
        * BuiltInGenreFunction::ALL.len()
        * CASES_PER_FUNCTION
        * CONFIRMATORY_N_VALUES.len()
        * usize::from(FRESH_FRONTIER_RUNS)
        * usize::from(ORDER_PERMUTATION_CELLS);
    assert_eq!(first.assignments().len(), expected);
    assert_eq!(first.fingerprint(), second.fingerprint());
    assert_eq!(first.assignments(), second.assignments());
    assert_eq!(first_map, second_map);
    assert_eq!(first.exact_manifest_source(), fixture.source);
    first.verify_integrity().expect("self-verifying seal");
    first
        .verify_mapping(&first_map)
        .expect("separate exact label map");

    for assignment in first.assignments() {
        assert_eq!(
            first.assignment(assignment.assignment_id()),
            Some(assignment),
        );
        assert_eq!(
            first_map
                .entry(assignment.assignment_id())
                .map(AssignmentLabelMapEntry::assignment_id),
            Some(assignment.assignment_id()),
        );
    }

    let visible = serde_json::to_value(first.assignments()).expect("serialize assignments");
    let visible_text = visible.to_string();
    assert!(!visible_text.contains("profile_fingerprint"));
    assert!(!visible_text.contains(&fixture.baseline.fingerprint().to_string()));
    for contender in &fixture.contenders {
        assert!(!visible_text.contains(&contender.fingerprint().to_string()));
    }
    assert_eq!(
        first
            .assignments()
            .iter()
            .map(BlindedBenchmarkAssignment::n)
            .collect::<BTreeSet<_>>(),
        CONFIRMATORY_N_VALUES.into_iter().collect(),
    );
    assert_eq!(
        first
            .assignments()
            .iter()
            .map(|assignment| assignment.permutation_cell().index())
            .collect::<BTreeSet<_>>(),
        (0..ORDER_PERMUTATION_CELLS).collect(),
    );

    let valid_label = format!("\"{}\"", first.assignments()[0].left_label().as_str());
    assert!(serde_json::from_str::<OpaqueCandidateLabel>(&valid_label).is_ok());
    assert!(serde_json::from_str::<OpaqueCandidateLabel>("\"candidate-NOT-HEX\"").is_err());
}

#[test]
fn seal_rejects_protocol_drift_split_leakage_unpinned_review_and_unequal_budgets() {
    let fixture = fixture(1);
    let mut wrong_n = fixture.clone();
    wrong_n.source = String::from_utf8(wrong_n.source)
        .expect("UTF-8 fixture")
        .replace("[1, 2, 4, 8, 16, 32]", "[1, 2, 4, 8, 16]")
        .into_bytes();
    assert!(matches!(
        BenchmarkSeal::compile(&wrong_n.source, wrong_n.inputs()),
        Err(BenchmarkSealError::NonCanonicalNestedN),
    ));

    let mut collision = fixture.clone();
    let first = collision.cases[0].clone();
    let second = collision.cases[1].clone();
    collision.cases[1] = BenchmarkCaseBinding::new(
        second.manifest_id().clone(),
        second.case_id(),
        second.function(),
        first.work_fingerprint(),
        second.project_ancestry_fingerprint(),
        second.source_blob_id(),
    );
    assert!(matches!(
        BenchmarkSeal::compile(&collision.source, collision.inputs()),
        Err(BenchmarkSealError::CaseSplitCollision),
    ));

    let mut unequal = fixture.clone();
    unequal.budgets[1] = ArmBudgetAllocation::new(
        unequal.budgets[1].profile_fingerprint(),
        ArmBudget::new(2_000_001, 2_000_000, 100_000, 100_000, 100_000_000).expect("valid budget"),
    );
    assert!(matches!(
        BenchmarkSeal::compile(&unequal.source, unequal.inputs()),
        Err(BenchmarkSealError::UnequalArmBudgets),
    ));

    assert!(matches!(
        FrontierReviewBinding::pinned(
            key("gpt-5-6-sol"),
            hash("model"),
            hash("protocol"),
            hash("cli"),
            hash("version"),
        ),
        Err(BenchmarkSealError::ReviewModelNotPinned),
    ));
}

fn assert_evaluator_bindings_reject_tamper(
    seal: &BenchmarkSeal,
    mapping: &AssignmentLabelMap,
    snapshot: &VerifiedBenchmarkRunJournalRecord,
) {
    for field in [
        "evaluator_prepared_pair_fingerprint",
        "exact_prompt_blob_id",
        "output_schema_blob_id",
    ] {
        let mut packet_tamper = serde_json::to_value(snapshot).expect("serialize JSON value");
        packet_tamper["events"][0]["run"]["packet"][field] =
            serde_json::to_value(hash(field)).expect("serialize hash");
        let record: VerifiedBenchmarkRunJournalRecord =
            serde_json::from_value(packet_tamper).expect("diagnostic outer record");
        assert!(matches!(
            DiagnosticBenchmarkRunJournal::replay(seal, mapping, record),
            Err(RunJournalError::FrontierPacketFingerprintMismatch),
        ));
    }
    for field in [
        "fresh_challenge_fingerprint",
        "evaluator_judgment_fingerprint",
    ] {
        let mut run_tamper = serde_json::to_value(snapshot).expect("serialize JSON value");
        run_tamper["events"][0]["run"][field] =
            serde_json::to_value(hash(field)).expect("serialize hash");
        let record: VerifiedBenchmarkRunJournalRecord =
            serde_json::from_value(run_tamper).expect("diagnostic outer record");
        assert!(matches!(
            DiagnosticBenchmarkRunJournal::replay(seal, mapping, record),
            Err(RunJournalError::VerifiedRunFingerprintMismatch),
        ));
    }
}

#[test]
fn verified_journal_is_append_only_while_serialized_replay_is_diagnostic() {
    let fixture = fixture(1);
    let (seal, mapping) = fixture.compile();
    let assignment = &seal.assignments()[0];
    let mut journal = QualificationBenchmarkRunJournal::from_verified_seal(
        &seal,
        &mapping,
        VerifiedBenchmarkSealLease::for_test(&seal),
    )
    .expect("verified seal admission");

    let too_early = VerifiedBenchmarkRunLease::supplemental_for_test(
        &seal,
        &mapping,
        assignment,
        SupplementalRunReason::FailureAnalysis,
        b"too-early",
    );
    assert!(matches!(
        journal.append_verified(&seal, &mapping, too_early),
        Err(RunJournalError::SupplementalBeforePrimary),
    ));

    journal
        .append_verified(
            &seal,
            &mapping,
            VerifiedBenchmarkRunLease::primary_for_test(
                &seal,
                &mapping,
                assignment,
                BlindedPairOutcome::Tie,
                (true, true, true),
                (true, true, true),
            ),
        )
        .expect("verified primary run");
    let snapshot = journal.snapshot().expect("strict snapshot");
    let encoded = serde_json::to_vec(&snapshot).expect("serialize snapshot");
    let decoded: VerifiedBenchmarkRunJournalRecord =
        serde_json::from_slice(&encoded).expect("strict self-hashed record");
    let diagnostic = DiagnosticBenchmarkRunJournal::replay(&seal, &mapping, decoded.clone())
        .expect("diagnostic replay");
    assert_eq!(diagnostic.record().fingerprint(), snapshot.fingerprint());

    let revalidated = QualificationBenchmarkRunJournal::revalidate(
        &seal,
        &mapping,
        &decoded,
        VerifiedBenchmarkSealLease::for_test(&seal),
        VerifiedJournalRevalidationLease::for_test(&decoded),
    )
    .expect("external revalidation restores qualification authority");
    assert_eq!(revalidated.chain_head(), journal.chain_head());

    let mut event_tamper = serde_json::to_value(&snapshot).expect("serialize JSON value");
    event_tamper["events"][0]["event_hash"] =
        serde_json::to_value(hash("forged-event")).expect("serialize hash");
    assert!(serde_json::from_value::<VerifiedBenchmarkRunJournalRecord>(event_tamper).is_err(),);

    let mut pool_tamper = serde_json::to_value(&snapshot).expect("serialize JSON value");
    pool_tamper["events"][0]["run"]["packet"]["pair"]["contender_pool"]["selected_projection_fingerprints"]
        [0] = serde_json::to_value(hash("outside-prefix")).expect("serialize hash");
    let self_hashed_but_semantically_tampered: VerifiedBenchmarkRunJournalRecord =
        serde_json::from_value(pool_tamper).expect("outer event hash cannot inspect nested fields");
    assert!(matches!(
        DiagnosticBenchmarkRunJournal::replay(
            &seal,
            &mapping,
            self_hashed_but_semantically_tampered,
        ),
        Err(RunJournalError::NestedPoolFingerprintMismatch),
    ));

    assert_evaluator_bindings_reject_tamper(&seal, &mapping, &snapshot);

    journal
        .append_verified(
            &seal,
            &mapping,
            VerifiedBenchmarkRunLease::supplemental_for_test(
                &seal,
                &mapping,
                assignment,
                SupplementalRunReason::JudgeDisagreement,
                b"after-primary",
            ),
        )
        .expect("supplemental run after primary");
    assert_eq!(journal.snapshot().expect("snapshot").events().len(), 2);
}

#[test]
fn rejected_binding_conflicts_leave_the_entire_qualification_journal_unchanged() {
    let fixture = fixture(1);
    let (seal, mapping) = fixture.compile();
    let first = &seal.assignments()[0];
    let same_case_and_n = seal
        .assignments()
        .iter()
        .filter(|assignment| assignment.case_id() == first.case_id() && assignment.n() == first.n())
        .collect::<Vec<_>>();
    assert!(
        same_case_and_n.len() >= 3,
        "fixture needs three review cells"
    );
    let different_n = seal
        .assignments()
        .iter()
        .find(|assignment| assignment.case_id() == first.case_id() && assignment.n() != first.n())
        .expect("fixture needs another N for the same case");
    let mut journal = QualificationBenchmarkRunJournal::from_verified_seal(
        &seal,
        &mapping,
        VerifiedBenchmarkSealLease::for_test(&seal),
    )
    .expect("verified seal admission");
    journal
        .append_verified(
            &seal,
            &mapping,
            VerifiedBenchmarkRunLease::primary_for_test(
                &seal,
                &mapping,
                same_case_and_n[0],
                BlindedPairOutcome::Tie,
                (true, true, true),
                (true, true, true),
            ),
        )
        .expect("seed primary");

    for (assignment, mutation, expected) in [
        (
            same_case_and_n[1],
            TestRunBindingMutation::Pool,
            RunJournalError::NestedPoolChanged,
        ),
        (
            same_case_and_n[2],
            TestRunBindingMutation::CandidatePair,
            RunJournalError::CandidatePairChanged,
        ),
        (
            different_n,
            TestRunBindingMutation::Baseline,
            RunJournalError::BaselineCandidateChanged,
        ),
    ] {
        let before = journal.exact_state_for_test();
        let rejected = journal.append_verified(
            &seal,
            &mapping,
            VerifiedBenchmarkRunLease::conflicting_primary_for_test(
                &seal, &mapping, assignment, mutation,
            ),
        );
        assert_eq!(rejected, Err(expected));
        assert_eq!(journal.exact_state_for_test(), before);

        journal
            .append_verified(
                &seal,
                &mapping,
                VerifiedBenchmarkRunLease::primary_for_test(
                    &seal,
                    &mapping,
                    assignment,
                    BlindedPairOutcome::Tie,
                    (true, true, true),
                    (true, true, true),
                ),
            )
            .expect("fresh verified lease succeeds after rejection");
    }

    assert_eq!(journal.snapshot().expect("snapshot").events().len(), 4);
}

#[derive(Clone, Copy)]
enum RelativeTestOutcome {
    BaselineWin,
    ContenderWin,
    Tie,
}

#[derive(Clone, Copy)]
struct TestRunDisposition {
    outcome: RelativeTestOutcome,
    contender_checks: (bool, bool, bool),
}

fn fill_primary_matrix(
    seal: &BenchmarkSeal,
    mapping: &AssignmentLabelMap,
    dispositions: &BTreeMap<BlobId, TestRunDisposition>,
) -> QualificationBenchmarkRunJournal {
    let mut journal = QualificationBenchmarkRunJournal::from_verified_seal(
        seal,
        mapping,
        VerifiedBenchmarkSealLease::for_test(seal),
    )
    .expect("verified seal admission");
    for assignment in seal.assignments() {
        let entry = mapping
            .entry(assignment.assignment_id())
            .expect("mapped assignment");
        let contender = [entry.left_arm(), entry.right_arm()]
            .into_iter()
            .find_map(|arm| match arm {
                ProfileArm::Contender {
                    profile_fingerprint,
                } => Some(profile_fingerprint),
                ProfileArm::Baseline { .. } => None,
            })
            .expect("contender arm");
        let disposition = dispositions
            .get(&contender)
            .copied()
            .unwrap_or(TestRunDisposition {
                outcome: RelativeTestOutcome::ContenderWin,
                contender_checks: (true, true, true),
            });
        let contender_is_left = matches!(entry.left_arm(), ProfileArm::Contender { .. });
        let outcome = match (disposition.outcome, contender_is_left) {
            (RelativeTestOutcome::ContenderWin, true)
            | (RelativeTestOutcome::BaselineWin, false) => BlindedPairOutcome::LeftWin,
            (RelativeTestOutcome::ContenderWin, false)
            | (RelativeTestOutcome::BaselineWin, true) => BlindedPairOutcome::RightWin,
            (RelativeTestOutcome::Tie, _) => BlindedPairOutcome::Tie,
        };
        journal
            .append_verified(
                seal,
                mapping,
                VerifiedBenchmarkRunLease::primary_for_test(
                    seal,
                    mapping,
                    assignment,
                    outcome,
                    disposition.contender_checks,
                    (true, true, true),
                ),
            )
            .expect("verified primary matrix run");
    }
    assert!(journal.primary_is_complete());
    journal
}

fn finalist_evidence(
    seal: &BenchmarkSeal,
    contenders: &[FrozenCandidateProfile],
    writer_costs: &[u64],
    defective_index: Option<usize>,
) -> (
    Vec<VerifiedCloseReadLease>,
    Vec<VerifiedProfileComputeCostLease>,
) {
    let close_reads = contenders
        .iter()
        .enumerate()
        .map(|(index, profile)| {
            let findings = if Some(index) == defective_index {
                vec![CloseReadFinding::for_test(
                    CloseReadDefect::RewardHackedPolish,
                    true,
                    true,
                    hash("close-read-evidence"),
                )]
            } else {
                Vec::new()
            };
            VerifiedCloseReadLease::for_test(seal, profile, findings)
                .expect("verified close-read lease")
        })
        .collect::<Vec<_>>();
    let costs = contenders
        .iter()
        .zip(writer_costs)
        .map(|(profile, writer_tokens)| {
            VerifiedProfileComputeCostLease::for_test(
                seal,
                profile,
                test_charges(*writer_tokens, 0, 0, 0, 1),
            )
            .expect("verified charge-derived cost")
        })
        .collect::<Vec<_>>();
    (close_reads, costs)
}

fn finalist_dispositions(
    contenders: &[FrozenCandidateProfile],
) -> BTreeMap<BlobId, TestRunDisposition> {
    contenders
        .iter()
        .enumerate()
        .map(|(index, contender)| {
            let disposition = if index == 4 {
                TestRunDisposition {
                    outcome: RelativeTestOutcome::Tie,
                    contender_checks: (false, false, false),
                }
            } else {
                TestRunDisposition {
                    outcome: RelativeTestOutcome::ContenderWin,
                    contender_checks: (true, true, true),
                }
            };
            (contender.fingerprint(), disposition)
        })
        .collect()
}

fn assert_finalist_policy(selection: &FinalistSelection, fixture: &Fixture) {
    let dominated = selection
        .assessments()
        .iter()
        .find(|assessment| assessment.profile_fingerprint() == fixture.contenders[0].fingerprint())
        .expect("expensive assessment");
    assert!(dominated.blockers().iter().any(|blocker| matches!(
        blocker,
        FinalistBlocker::DominatedByCheaperEqualQuality { profile_fingerprint }
            if *profile_fingerprint == fixture.contenders[1].fingerprint()
    )));

    let defective = selection
        .assessments()
        .iter()
        .find(|assessment| assessment.profile_fingerprint() == fixture.contenders[4].fingerprint())
        .expect("defective assessment");
    assert!(
        defective
            .blockers()
            .contains(&FinalistBlocker::IncompleteProvenance)
    );
    assert!(
        defective
            .blockers()
            .contains(&FinalistBlocker::IncompleteAssembly)
    );
    assert!(
        defective
            .blockers()
            .contains(&FinalistBlocker::OverallPointNotAbove55)
    );
    assert!(
        defective
            .blockers()
            .contains(&FinalistBlocker::RecurringCloseReadDefect)
    );

    assert_eq!(selection.roles().len(), MAX_FINALISTS);
    assert_eq!(selection.reviewed_profiles().len(), MAX_FINALISTS);
    assert_eq!(
        selection
            .roles()
            .iter()
            .map(|assignment| assignment.role())
            .collect::<BTreeSet<_>>(),
        FrontierProfileRole::ALL.into_iter().collect(),
    );
    assert_eq!(
        selection
            .roles()
            .iter()
            .map(|assignment| assignment.candidate_profile_fingerprint())
            .collect::<BTreeSet<_>>()
            .len(),
        MAX_FINALISTS,
    );
    assert!(selection.reviewed_profiles().iter().all(|profile| {
        profile.finalist_selection_fingerprint() == selection.fingerprint()
            && profile.seal_fingerprint() == selection.seal_fingerprint()
            && profile.journal_chain_head() == selection.journal_chain_head()
            && profile.journal_record_fingerprint() == selection.journal_record_fingerprint()
    }));
}

fn assert_test_human_confirmation(seal: &BenchmarkSeal, selection: &FinalistSelection) {
    let provisional = selection.reviewed_profiles()[0].clone();
    let envelope = EncryptedHumanLabelArchive::archive(
        seal.fingerprint(),
        hash("human-label-schema"),
        hash("human-key"),
        selection.fingerprint(),
        HumanLabelEncryptionAlgorithm::XChaCha20Poly1305V1,
        vec![7; 24],
        vec![9; 64],
    )
    .expect("structurally valid diagnostic ciphertext");
    let adjudication = HumanAdjudicationArchiveRecord::archive(
        seal.fingerprint(),
        selection.fingerprint(),
        provisional.fingerprint(),
        envelope.fingerprint(),
        hash("three-reader-protocol"),
        vec![hash("group-a"), hash("group-b"), hash("group-c")],
        hash("adjudication-receipt"),
    )
    .expect("diagnostic adjudication archive");
    let confirmation_lease =
        VerifiedHumanConfirmationLease::for_test(&provisional, &envelope, &adjudication);
    let confirmed = HumanConfirmedProfile::from_verified(
        provisional.clone(),
        &envelope,
        &adjudication,
        confirmation_lease,
    )
    .expect("test-only crypto verifier lease confirms exact profile");
    assert_eq!(confirmed.provisional(), &provisional);
    assert_eq!(
        confirmed.profile_fingerprint(),
        provisional.candidate().fingerprint()
    );
    assert_eq!(confirmed.benchmark_seal_fingerprint(), seal.fingerprint());
    assert_eq!(
        confirmed.encrypted_label_archive_fingerprint(),
        envelope.fingerprint()
    );
    assert_eq!(
        confirmed.adjudication_archive_fingerprint(),
        adjudication.fingerprint()
    );
    confirmed
        .validate_confirmation_evidence(&envelope, &adjudication)
        .expect("confirmation remains bound to exact typed evidence");
    let substituted_envelope = EncryptedHumanLabelArchive::archive(
        seal.fingerprint(),
        hash("human-label-schema"),
        hash("human-key"),
        selection.fingerprint(),
        HumanLabelEncryptionAlgorithm::XChaCha20Poly1305V1,
        vec![7; 24],
        vec![8; 64],
    )
    .expect("second structurally valid envelope");
    assert!(matches!(
        confirmed.validate_confirmation_evidence(&substituted_envelope, &adjudication),
        Err(HumanConfirmationError::ConfirmationEvidenceMismatch)
    ));
    assert_ne!(confirmed.fingerprint(), provisional.fingerprint());
    let encoded = serde_json::to_vec(&envelope).expect("serialize diagnostic envelope");
    assert!(!String::from_utf8_lossy(&encoded).contains("plaintext"));
    let decoded: EncryptedHumanLabelArchive =
        serde_json::from_slice(&encoded).expect("strict envelope replay");
    assert_eq!(decoded, envelope);
}

fn assert_supplemental_does_not_change_metrics(
    seal: &BenchmarkSeal,
    mapping: &AssignmentLabelMap,
    journal: &mut QualificationBenchmarkRunJournal,
    fixture: &Fixture,
    selection: &FinalistSelection,
) {
    let writer_costs = [100, 75, 85, 90, 110];
    let before_metrics = selection
        .assessments()
        .iter()
        .map(|assessment| assessment.metrics().clone())
        .collect::<Vec<_>>();
    let first_assignment = &seal.assignments()[0];
    journal
        .append_verified(
            seal,
            mapping,
            VerifiedBenchmarkRunLease::supplemental_for_test(
                seal,
                mapping,
                first_assignment,
                SupplementalRunReason::FailureAnalysis,
                b"late-diagnostic",
            ),
        )
        .expect("supplemental diagnostic run");
    let (close_reads, costs) = finalist_evidence(seal, &fixture.contenders, &writer_costs, Some(4));
    let after = evaluate_finalists(seal, mapping, journal, close_reads, costs)
        .expect("supplemental run cannot change primary metrics");
    assert_eq!(
        before_metrics,
        after
            .assessments()
            .iter()
            .map(|assessment| assessment.metrics().clone())
            .collect::<Vec<_>>(),
    );
}

#[test]
fn qualification_selects_three_explicit_nondominated_roles_and_blocks_defects() {
    let fixture = fixture(5);
    let (seal, mapping) = fixture.compile();
    let dispositions = finalist_dispositions(&fixture.contenders);
    let mut journal = fill_primary_matrix(&seal, &mapping, &dispositions);
    let writer_costs = [100, 75, 85, 90, 110];
    let (close_reads, costs) =
        finalist_evidence(&seal, &fixture.contenders, &writer_costs, Some(4));
    let selection = evaluate_finalists(&seal, &mapping, &journal, close_reads, costs)
        .expect("complete verified finalist selection");
    assert_finalist_policy(&selection, &fixture);
    assert_test_human_confirmation(&seal, &selection);
    assert_supplemental_does_not_change_metrics(
        &seal,
        &mapping,
        &mut journal,
        &fixture,
        &selection,
    );
}

#[test]
fn costs_rates_intervals_and_human_archives_fail_closed() {
    let fixture = fixture(1);
    let (seal, _) = fixture.compile();
    assert!(matches!(
        VerifiedProfileComputeCostLease::for_test(
            &seal,
            &fixture.contenders[0],
            test_charges(0, 0, 0, 0, 0),
        ),
        Err(FinalistError::InvalidVerifiedCost),
    ));
    assert_eq!(
        boolean_rate_for_test(std::iter::repeat_n(true, 100_000)).expect("large rate arithmetic"),
        RateMillionths::ONE,
    );
    for degrees_of_freedom in [30, 31, 10_000] {
        assert!((student_t_for_test(degrees_of_freedom) - 2.042).abs() < f64::EPSILON);
    }

    assert!(matches!(
        EncryptedHumanLabelArchive::archive(
            seal.fingerprint(),
            hash("schema"),
            hash("key"),
            hash("aad"),
            HumanLabelEncryptionAlgorithm::XChaCha20Poly1305V1,
            vec![0; 12],
            vec![0; 64],
        ),
        Err(HumanConfirmationError::InvalidXChaChaNonce),
    ));
    assert!(matches!(
        EncryptedHumanLabelArchive::archive(
            seal.fingerprint(),
            hash("schema"),
            hash("key"),
            hash("aad"),
            HumanLabelEncryptionAlgorithm::AgeX25519V1,
            vec![1],
            vec![2],
        ),
        Err(HumanConfirmationError::InvalidAgeNonce),
    ));
    assert!(matches!(
        HumanAdjudicationArchiveRecord::archive(
            seal.fingerprint(),
            hash("selection"),
            hash("profile"),
            hash("envelope"),
            hash("protocol"),
            vec![hash("group"), hash("group"), hash("other")],
            hash("receipt"),
        ),
        Err(HumanConfirmationError::DuplicateReviewerGroup),
    ));
}

#[test]
fn baseline_loss_variant_is_exercised() {
    let fixture = fixture(1);
    let (seal, mapping) = fixture.compile();
    let dispositions = BTreeMap::from([(
        fixture.contenders[0].fingerprint(),
        TestRunDisposition {
            outcome: RelativeTestOutcome::BaselineWin,
            contender_checks: (true, true, true),
        },
    )]);
    let journal = fill_primary_matrix(&seal, &mapping, &dispositions);
    let (close_reads, costs) = finalist_evidence(&seal, &fixture.contenders, &[100], None);
    let selection = evaluate_finalists(&seal, &mapping, &journal, close_reads, costs)
        .expect("loss remains diagnostic");
    assert_eq!(
        selection.assessments()[0].metrics().overall().point(),
        RateMillionths::ZERO,
    );
    assert!(selection.reviewed_profiles().is_empty());
}
