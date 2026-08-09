use loom_types::{BlobId, RevisionId};
use serde_json::{Value, json};

use crate::*;

fn name(value: &str) -> StoryNodeName {
    StoryNodeName::new(value).expect("bounded node name")
}

fn description(value: &str) -> StoryRelationDescription {
    StoryRelationDescription::new(value).expect("bounded relation description")
}

fn valid_story_graph() -> (StoryGraph, [StoryNodeId; 5]) {
    let ids = std::array::from_fn(|_| StoryNodeId::new());
    let nodes = vec![
        StoryNode::new(ids[0], StoryNodeKind::Book, None, 0, name("Book")),
        StoryNode::new(
            ids[1],
            StoryNodeKind::Chapter,
            Some(ids[0]),
            0,
            name("Chapter"),
        ),
        StoryNode::new(ids[2], StoryNodeKind::Scene, Some(ids[1]), 0, name("Scene")),
        StoryNode::new(
            ids[3],
            StoryNodeKind::Movement,
            Some(ids[2]),
            0,
            name("Movement"),
        ),
        StoryNode::new(ids[4], StoryNodeKind::Beat, Some(ids[3]), 0, name("Beat")),
    ];
    let relations = vec![
        StoryRelation::new(
            StoryRelationId::new(),
            StoryRelationKind::Causal,
            ids[2],
            ids[3],
            description("The scene pressure causes the movement."),
        )
        .expect("non-self relation"),
        StoryRelation::new(
            StoryRelationId::new(),
            StoryRelationKind::Temporal,
            ids[3],
            ids[4],
            description("The beat follows the movement."),
        )
        .expect("non-self relation"),
    ];
    (
        StoryGraph::new(StoryGraphId::new(), nodes, relations).expect("valid story graph"),
        ids,
    )
}

fn evidence(start: u64, end: u64) -> StateEvidenceSpan {
    StateEvidenceSpan::new(
        RevisionId::new(),
        BlobId::digest(format!("source-{start}-{end}").as_bytes()),
        NonEmptyByteRange::new(start, end).expect("nonempty evidence range"),
    )
    .expect("bounded evidence")
}

fn fact(subject: &str, assertion: &str, offset: u64) -> GroundedStoryStateFact {
    GroundedStoryStateFact::new(
        StoryStateFactId::new(),
        subject,
        assertion,
        vec![evidence(offset, offset + 3)],
    )
    .expect("grounded fact")
}

fn known(facts: Vec<GroundedStoryStateFact>) -> GroundedFactCollection {
    GroundedFactCollection::known(facts).expect("valid known fact collection")
}

fn unknown_facts(reason: &str) -> GroundedFactCollection {
    GroundedFactCollection::unknown(reason).expect("valid unknown fact collection")
}

fn known_empty(offset: u64) -> GroundedFactCollection {
    GroundedFactCollection::known_empty(vec![evidence(offset, offset + 3)])
        .expect("known emptiness has exact source evidence")
}

fn valid_story_state(graph: &StoryGraph, at_node: StoryNodeId) -> StoryState {
    StoryState::new(
        StoryStateId::new(),
        graph.id(),
        at_node,
        ExplicitStoryStateField::grounded("Minutes after midnight", vec![evidence(0, 3)])
            .expect("grounded chronology"),
        ExplicitStoryStateField::grounded("The locked observatory", vec![evidence(3, 6)])
            .expect("grounded location"),
        known(vec![fact("Mara", "stands beside the sealed hatch", 6)]),
        known(vec![fact("Mara", "knows the outer alarm was disabled", 9)]),
        known(vec![fact("Mara", "is short of breath", 12)]),
        known(vec![fact("observatory", "has only one visible exit", 15)]),
        known(vec![fact("alarm", "must be explained before dawn", 18)]),
        ExplicitStoryStateField::grounded(
            "Close third person through Mara",
            vec![evidence(21, 24)],
        )
        .expect("grounded point of view"),
        known(vec![fact(
            "narration",
            "keeps perceptions concrete and immediate",
            24,
        )]),
        known(vec![fact(
            "Mara",
            "can inspect the hatch without crossing the room",
            27,
        )]),
    )
    .expect("valid explicit state")
}

#[test]
fn hierarchy_and_semantic_graph_round_trip_without_conflating_edges() {
    let (graph, ids) = valid_story_graph();
    assert_eq!(graph.nodes()[0].kind(), StoryNodeKind::Book);
    assert_eq!(graph.nodes()[4].parent_id(), Some(ids[3]));
    assert_eq!(graph.relations()[0].kind(), StoryRelationKind::Causal);
    assert_eq!(graph.relations()[1].kind(), StoryRelationKind::Temporal);

    let encoded = serde_json::to_vec(&graph).expect("serialize graph");
    let decoded: StoryGraph = serde_json::from_slice(&encoded).expect("validated deserialize");
    assert_eq!(decoded, graph);
}

#[test]
fn hierarchy_rejects_wrong_parent_kind_forward_parent_and_sibling_gaps() {
    let book = StoryNodeId::new();
    let chapter = StoryNodeId::new();
    let scene = StoryNodeId::new();
    let wrong_parent = StoryGraph::new(
        StoryGraphId::new(),
        vec![
            StoryNode::new(book, StoryNodeKind::Book, None, 0, name("Book")),
            StoryNode::new(scene, StoryNodeKind::Scene, Some(book), 0, name("Scene")),
        ],
        vec![],
    )
    .expect_err("scene cannot be parented by book");
    assert!(matches!(
        wrong_parent,
        StoryGraphError::InvalidParentKind { .. }
    ));

    let forward = StoryGraph::new(
        StoryGraphId::new(),
        vec![
            StoryNode::new(book, StoryNodeKind::Book, None, 0, name("Book")),
            StoryNode::new(scene, StoryNodeKind::Scene, Some(chapter), 0, name("Scene")),
            StoryNode::new(
                chapter,
                StoryNodeKind::Chapter,
                Some(book),
                0,
                name("Chapter"),
            ),
        ],
        vec![],
    )
    .expect_err("parents must precede children");
    assert!(matches!(
        forward,
        StoryGraphError::MissingOrForwardParent { .. }
    ));

    let gap = StoryGraph::new(
        StoryGraphId::new(),
        vec![
            StoryNode::new(book, StoryNodeKind::Book, None, 0, name("Book")),
            StoryNode::new(
                chapter,
                StoryNodeKind::Chapter,
                Some(book),
                1,
                name("Chapter"),
            ),
        ],
        vec![],
    )
    .expect_err("sibling order starts at zero and is contiguous");
    assert!(matches!(gap, StoryGraphError::InvalidSiblingOrder { .. }));
}

#[test]
fn semantic_cycle_across_relation_kinds_and_duplicate_edges_fail_closed() {
    let (graph, ids) = valid_story_graph();
    let mut relations = graph.relations().to_vec();
    relations.push(
        StoryRelation::new(
            StoryRelationId::new(),
            StoryRelationKind::Reveal,
            ids[4],
            ids[2],
            description("A later reveal cannot make the earlier scene depend on it."),
        )
        .expect("non-self relation"),
    );
    assert_eq!(
        StoryGraph::new(graph.id(), graph.nodes().to_vec(), relations).unwrap_err(),
        StoryGraphError::SemanticCycle
    );

    let relation = graph.relations()[0].clone();
    let duplicate = StoryRelation::new(
        StoryRelationId::new(),
        relation.kind(),
        relation.from(),
        relation.to(),
        description("Same typed endpoints are still a duplicate."),
    )
    .expect("non-self relation");
    assert!(matches!(
        StoryGraph::new(
            graph.id(),
            graph.nodes().to_vec(),
            vec![relation, duplicate]
        ),
        Err(StoryGraphError::DuplicateRelationEndpoints { .. })
    ));
}

#[test]
fn graph_serde_rejects_unknown_fields_self_edges_and_a_second_book() {
    let (graph, _) = valid_story_graph();
    let mut unknown = serde_json::to_value(&graph).expect("graph value");
    unknown["invented"] = json!(true);
    assert!(serde_json::from_value::<StoryGraph>(unknown).is_err());

    let mut self_edge = serde_json::to_value(&graph).expect("graph value");
    self_edge["relations"][0]["to"] = self_edge["relations"][0]["from"].clone();
    assert!(serde_json::from_value::<StoryGraph>(self_edge).is_err());

    let mut nodes = graph.nodes().to_vec();
    nodes.push(StoryNode::new(
        StoryNodeId::new(),
        StoryNodeKind::Book,
        None,
        0,
        name("Second book"),
    ));
    assert_eq!(
        StoryGraph::new(StoryGraphId::new(), nodes, vec![]).unwrap_err(),
        StoryGraphError::InvalidBookRoot
    );
}

#[test]
fn story_state_requires_every_field_and_preserves_explicit_grounding() {
    let (graph, ids) = valid_story_graph();
    let state = valid_story_state(&graph, ids[3]);
    state
        .validate_against_graph(&graph)
        .expect("movement may anchor state");
    assert_eq!(state.character_knowledge().known_facts().unwrap().len(), 1);
    assert_eq!(
        state.possible_next_actions().known_facts().unwrap().len(),
        1
    );

    let value = serde_json::to_value(&state).expect("state value");
    let restored: StoryState = serde_json::from_value(value.clone()).expect("state round trip");
    assert_eq!(restored, state);

    for field in [
        "chronology",
        "location",
        "physical_configuration",
        "character_knowledge",
        "character_conditions",
        "world_facts",
        "unresolved_promises",
        "point_of_view",
        "voice_constraints",
        "possible_next_actions",
    ] {
        let mut missing = value.clone();
        missing.as_object_mut().expect("state object").remove(field);
        assert!(
            serde_json::from_value::<StoryState>(missing).is_err(),
            "missing {field} must not receive an invented default"
        );
    }
}

#[test]
fn unknown_scalar_state_is_explicit_while_grounded_values_require_evidence() {
    let unknown = ExplicitStoryStateField::unknown("The excerpt does not establish the date")
        .expect("explicit unknown");
    let encoded = serde_json::to_value(&unknown).expect("unknown field value");
    assert_eq!(encoded["status"], "unknown");
    assert!(encoded.get("reason").is_some());

    assert!(GroundedStateText::new("Unfounded claim", vec![]).is_err());
    let span = evidence(0, 2);
    assert_eq!(
        GroundedStateText::new("Duplicated support", vec![span, span]).unwrap_err(),
        StoryStateError::DuplicateEvidence
    );
    assert!(
        StateEvidenceSpan::new(
            RevisionId::new(),
            BlobId::digest(b"oversized"),
            NonEmptyByteRange::new(0, MAX_SOURCE_BYTES as u64 + 1).expect("nonempty range"),
        )
        .is_err()
    );
}

#[test]
fn every_fact_category_distinguishes_known_empty_from_unknown() {
    let established_empty = known_empty(0);
    let unknown = unknown_facts("The source does not establish anyone's knowledge");

    assert_eq!(established_empty.known_facts(), Some([].as_slice()));
    assert_eq!(established_empty.known_empty_evidence().unwrap().len(), 1);
    assert!(established_empty.unknown_reason().is_none());
    assert!(unknown.known_facts().is_none());
    assert_eq!(
        unknown
            .unknown_reason()
            .map(StoryStateUnknownReason::as_str),
        Some("The source does not establish anyone's knowledge")
    );

    let known_value = serde_json::to_value(&established_empty).expect("known-empty value");
    assert_eq!(known_value["status"], "known_empty");
    assert!(known_value.get("facts").is_none());
    assert_eq!(known_value["evidence"].as_array().unwrap().len(), 1);
    let unknown_value = serde_json::to_value(&unknown).expect("unknown value");
    assert_eq!(unknown_value["status"], "unknown");
    assert!(unknown_value.get("facts").is_none());
    assert_eq!(
        serde_json::from_value::<GroundedFactCollection>(unknown_value)
            .expect("unknown round trip"),
        unknown
    );

    assert_eq!(
        GroundedFactCollection::known(vec![]).unwrap_err(),
        StoryStateError::Bound(BoundError::Empty)
    );
    assert!(
        serde_json::from_value::<GroundedFactCollection>(json!({
            "status": "known_non_empty",
            "facts": []
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<GroundedFactCollection>(json!({
            "status": "known_empty",
            "evidence": []
        }))
        .is_err()
    );
}

#[test]
fn evidence_verification_binds_revision_blob_and_utf8_range() {
    let source = "AéZ".as_bytes();
    let revision = RevisionId::new();
    let valid = StateEvidenceSpan::new(
        revision,
        BlobId::digest(source),
        NonEmptyByteRange::new(1, 3).expect("range around one UTF-8 character"),
    )
    .expect("valid evidence reference");
    assert_eq!(
        valid
            .verified_source_text(revision, source)
            .expect("exact source evidence"),
        "é"
    );
    GroundedStateText::new("The middle character is accented", vec![valid])
        .expect("grounding")
        .verify_source(revision, source)
        .expect("grounding verifies every evidence span");
    GroundedFactCollection::known_empty(vec![valid])
        .expect("grounded absence")
        .verify_source(revision, source)
        .expect("known emptiness verifies its absence witness");

    assert!(matches!(
        valid.verified_source_text(RevisionId::new(), source),
        Err(StoryStateError::EvidenceRevisionMismatch { .. })
    ));

    let wrong_blob = StateEvidenceSpan::new(
        revision,
        BlobId::digest(b"different source"),
        NonEmptyByteRange::new(1, 3).expect("nonempty range"),
    )
    .expect("structurally valid evidence reference");
    assert!(matches!(
        wrong_blob.verified_source_text(revision, source),
        Err(StoryStateError::EvidenceBlobMismatch { .. })
    ));

    let out_of_range = StateEvidenceSpan::new(
        revision,
        BlobId::digest(source),
        NonEmptyByteRange::new(1, 8).expect("nonempty structural range"),
    )
    .expect("range is below the global source limit");
    assert!(matches!(
        out_of_range.verified_source_text(revision, source),
        Err(StoryStateError::Range(RangeError::OutOfBounds { .. }))
    ));

    let split_utf8 = StateEvidenceSpan::new(
        revision,
        BlobId::digest(source),
        NonEmptyByteRange::new(2, 3).expect("nonempty structural range"),
    )
    .expect("range is below the global source limit");
    assert_eq!(
        split_utf8
            .verified_source_text(revision, source)
            .unwrap_err(),
        StoryStateError::Range(RangeError::SplitsUtf8 { offset: 2 })
    );
}

#[test]
fn story_state_rejects_duplicate_facts_and_wrong_graph_or_anchor() {
    let (graph, ids) = valid_story_graph();
    let duplicate = fact("Mara", "knows the door is locked", 0);
    let result = StoryState::new(
        StoryStateId::new(),
        graph.id(),
        ids[2],
        ExplicitStoryStateField::unknown("Not yet established").expect("explicit unknown"),
        ExplicitStoryStateField::unknown("Not yet established").expect("explicit unknown"),
        known(vec![duplicate.clone()]),
        known(vec![duplicate]),
        unknown_facts("Not yet established"),
        known_empty(30),
        unknown_facts("Not yet established"),
        ExplicitStoryStateField::unknown("Not yet established").expect("explicit unknown"),
        known_empty(33),
        unknown_facts("Not yet established"),
    );
    assert!(matches!(result, Err(StoryStateError::DuplicateFact(_))));

    let state = valid_story_state(&graph, ids[3]);
    let (other, _) = valid_story_graph();
    assert!(matches!(
        state.validate_against_graph(&other),
        Err(StoryStateError::WrongStoryGraph { .. })
    ));

    let book_state = valid_story_state(&graph, ids[0]);
    assert_eq!(
        book_state.validate_against_graph(&graph).unwrap_err(),
        StoryStateError::InvalidAnchorKind(StoryNodeKind::Book)
    );
}

#[test]
fn story_state_serde_rejects_unvalidated_evidence_and_unknown_fields() {
    let (graph, ids) = valid_story_graph();
    let state = valid_story_state(&graph, ids[4]);
    let mut value = serde_json::to_value(state).expect("state value");
    value["invented_default"] = Value::Bool(true);
    assert!(serde_json::from_value::<StoryState>(value).is_err());

    let invalid = json!({
        "value": "unsupported",
        "evidence": []
    });
    assert!(serde_json::from_value::<GroundedStateText>(invalid).is_err());
}
