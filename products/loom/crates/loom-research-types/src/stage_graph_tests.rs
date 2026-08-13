use loom_types::BlobId;
use serde_json::json;

use crate::*;

fn canonical_stage_specs() -> Vec<FrozenStageSpec> {
    let ids = std::array::from_fn::<_, MAX_TRIAL_STAGES, _>(|_| StageId::new());
    let dependencies: [Vec<StageId>; MAX_TRIAL_STAGES] = [
        vec![],
        vec![ids[0]],
        vec![ids[0], ids[1]],
        vec![ids[0], ids[2]],
        vec![ids[0], ids[1], ids[2], ids[3]],
        vec![ids[4]],
        vec![ids[5]],
        vec![ids[6]],
        vec![ids[7]],
        vec![ids[8]],
        vec![ids[9]],
        vec![ids[9], ids[10]],
    ];
    FrozenTrialStage::ALL
        .into_iter()
        .enumerate()
        .map(|(index, stage)| {
            FrozenStageSpec::new(
                ids[index],
                stage,
                BlobId::digest(format!("frozen-stage-{stage:?}").as_bytes()),
                dependencies[index].clone(),
            )
            .expect("canonical stage specification")
        })
        .collect()
}

fn canonical_stage_graph() -> StageGraph {
    let stages = canonical_stage_specs();
    let output = stages.last().expect("archive stage").id();
    StageGraph::new(StageGraphId::new(), stages, output).expect("canonical frozen trial graph")
}

#[test]
fn frozen_trial_graph_contains_every_stage_and_round_trips() {
    let graph = canonical_stage_graph();
    assert_eq!(graph.stages().len(), MAX_TRIAL_STAGES);
    for (spec, expected) in graph.stages().iter().zip(FrozenTrialStage::ALL) {
        assert_eq!(spec.stage(), expected);
        assert_eq!(graph.stage(spec.id()), Some(spec));
    }
    assert_eq!(
        graph.stage(graph.output()).map(FrozenStageSpec::stage),
        Some(FrozenTrialStage::Archive)
    );

    let encoded = serde_json::to_vec(&graph).expect("serialize stage graph");
    let restored: StageGraph = serde_json::from_slice(&encoded).expect("validated deserialize");
    assert_eq!(restored, graph);
}

#[test]
fn stage_graph_rejects_missing_reordered_or_duplicate_kinds() {
    let mut missing = canonical_stage_specs();
    let output = missing.last().expect("archive").id();
    missing.remove(3);
    assert_eq!(
        StageGraph::new(StageGraphId::new(), missing, output).unwrap_err(),
        StageGraphError::IncompleteStageSet
    );

    let mut reordered = canonical_stage_specs();
    reordered.swap(1, 2);
    let output = reordered.last().expect("archive").id();
    assert!(matches!(
        StageGraph::new(StageGraphId::new(), reordered, output),
        Err(StageGraphError::InvalidStageOrder { .. })
    ));

    let mut duplicate = canonical_stage_specs();
    let first_id = duplicate[0].id();
    duplicate[1] = FrozenStageSpec::new(
        duplicate[1].id(),
        FrozenTrialStage::FreezeInputs,
        duplicate[1].spec_fingerprint(),
        vec![first_id],
    )
    .expect("locally bounded stage");
    let output = duplicate.last().expect("archive").id();
    assert_eq!(
        StageGraph::new(StageGraphId::new(), duplicate, output).unwrap_err(),
        StageGraphError::DuplicateStageKind(FrozenTrialStage::FreezeInputs)
    );

    let mut duplicate_id = canonical_stage_specs();
    let repeated = duplicate_id[0].id();
    duplicate_id[1] = FrozenStageSpec::new(
        repeated,
        FrozenTrialStage::BacktranslateMask,
        duplicate_id[1].spec_fingerprint(),
        vec![repeated],
    )
    .expect("locally bounded stage");
    let output = duplicate_id.last().expect("archive").id();
    assert_eq!(
        StageGraph::new(StageGraphId::new(), duplicate_id, output).unwrap_err(),
        StageGraphError::DuplicateStageId(repeated)
    );
}

#[test]
fn stage_dependencies_are_exact_ordered_and_acyclic() {
    let mut wrong = canonical_stage_specs();
    let plan_id = wrong[2].id();
    wrong[3] = FrozenStageSpec::new(
        wrong[3].id(),
        FrozenTrialStage::Retrieve,
        wrong[3].spec_fingerprint(),
        vec![plan_id],
    )
    .expect("locally valid dependency vector");
    let output = wrong.last().expect("archive").id();
    assert!(matches!(
        StageGraph::new(StageGraphId::new(), wrong, output),
        Err(StageGraphError::InvalidDependencies { .. })
    ));

    let mut forward = canonical_stage_specs();
    let archive_id = forward[11].id();
    forward[0] = FrozenStageSpec::new(
        forward[0].id(),
        FrozenTrialStage::FreezeInputs,
        forward[0].spec_fingerprint(),
        vec![archive_id],
    )
    .expect("locally valid dependency vector");
    let output = forward.last().expect("archive").id();
    assert!(matches!(
        StageGraph::new(StageGraphId::new(), forward, output),
        Err(StageGraphError::MissingOrForwardDependency { .. })
    ));

    let stage = StageId::new();
    assert_eq!(
        FrozenStageSpec::new(
            stage,
            FrozenTrialStage::Plan,
            BlobId::digest(b"duplicate dependency"),
            vec![
                StageId::new(),
                StageId::new(),
                StageId::new(),
                StageId::new(),
                StageId::new()
            ],
        )
        .unwrap_err(),
        StageGraphError::Bound(BoundError::TooMany {
            actual: 5,
            maximum: MAX_STAGE_DEPENDENCIES,
        })
    );

    let dependency = StageId::new();
    assert_eq!(
        FrozenStageSpec::new(
            stage,
            FrozenTrialStage::Plan,
            BlobId::digest(b"duplicate dependency"),
            vec![dependency, dependency],
        )
        .unwrap_err(),
        StageGraphError::DuplicateDependency(stage)
    );
}

#[test]
fn only_the_archive_stage_can_be_the_graph_output() {
    let stages = canonical_stage_specs();
    assert_eq!(
        StageGraph::new(StageGraphId::new(), stages.clone(), stages[10].id()).unwrap_err(),
        StageGraphError::InvalidOutput(stages[10].id())
    );
}

#[test]
fn stage_graph_serde_rejects_unknown_fields_missing_dependencies_and_bad_hashes() {
    let graph = canonical_stage_graph();
    let mut unknown = serde_json::to_value(&graph).expect("graph value");
    unknown["adaptive_search_mutation"] = json!(true);
    assert!(serde_json::from_value::<StageGraph>(unknown).is_err());

    let mut missing = serde_json::to_value(&graph).expect("graph value");
    missing["stages"][5]
        .as_object_mut()
        .expect("stage object")
        .remove("dependencies");
    assert!(serde_json::from_value::<StageGraph>(missing).is_err());

    let mut malformed = serde_json::to_value(&graph).expect("graph value");
    malformed["stages"][0]["spec_fingerprint"] = json!("not-a-sha256");
    assert!(serde_json::from_value::<StageGraph>(malformed).is_err());
}

#[test]
fn stage_graph_serde_rejects_a_declared_cycle_before_construction() {
    let graph = canonical_stage_graph();
    let mut value = serde_json::to_value(&graph).expect("graph value");
    let archive_id = value["stages"][11]["id"].clone();
    value["stages"][0]["dependencies"] = json!([archive_id]);
    assert!(serde_json::from_value::<StageGraph>(value).is_err());
}
