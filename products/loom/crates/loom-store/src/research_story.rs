//! Exact diagnostic persistence for story graphs and evidence-grounded state.

use loom_research_types::{
    ExplicitStoryStateField, GroundedFactCollection, StoryGraph, StoryGraphId, StoryState,
    StoryStateId,
};
use loom_types::{BlobId, ProjectId, RevisionId, now_unix_ms};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{ProjectStore, ResearchExecutionRecordKind, Result, StoreError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticStoryGraph {
    story_graph_id: StoryGraphId,
    graph_fingerprint: BlobId,
    record_fingerprint: BlobId,
}

impl PersistedDiagnosticStoryGraph {
    pub const fn story_graph_id(self) -> StoryGraphId {
        self.story_graph_id
    }

    pub const fn graph_fingerprint(self) -> BlobId {
        self.graph_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PersistedDiagnosticStoryState {
    story_state_id: StoryStateId,
    state_fingerprint: BlobId,
    record_fingerprint: BlobId,
    source_revision_id: RevisionId,
    source_blob_id: BlobId,
}

impl PersistedDiagnosticStoryState {
    pub const fn story_state_id(self) -> StoryStateId {
        self.story_state_id
    }

    pub const fn state_fingerprint(self) -> BlobId {
        self.state_fingerprint
    }

    pub const fn record_fingerprint(self) -> BlobId {
        self.record_fingerprint
    }

    pub const fn source_revision_id(self) -> RevisionId {
        self.source_revision_id
    }

    pub const fn source_blob_id(self) -> BlobId {
        self.source_blob_id
    }
}

impl ProjectStore {
    /// Persist one validated `StoryGraph` as nonauthorizing diagnostic evidence.
    pub fn persist_diagnostic_story_graph(
        &mut self,
        graph: &StoryGraph,
    ) -> Result<PersistedDiagnosticStoryGraph> {
        let canonical = serde_json::to_vec(graph)?;
        let graph_fingerprint = BlobId::digest(&canonical);
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::StoryGraph,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_story_graphs(
                story_graph_id, project_id, graph_fingerprint,
                node_count, relation_count, record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                graph.id().to_string(),
                self.manifest.project_id.to_string(),
                graph_fingerprint.to_string(),
                checked_count(graph.nodes().len(), "story graph node count")?,
                checked_count(graph.relations().len(), "story graph relation count")?,
                record.fingerprint().to_string(),
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_story_graphs
             WHERE story_graph_id = ?1 AND project_id = ?2
               AND graph_fingerprint = ?3 AND node_count = ?4
               AND relation_count = ?5 AND record_fingerprint = ?6",
            params![
                graph.id().to_string(),
                self.manifest.project_id.to_string(),
                graph_fingerprint.to_string(),
                checked_count(graph.nodes().len(), "story graph node count")?,
                checked_count(graph.relations().len(), "story graph relation count")?,
                record.fingerprint().to_string(),
            ],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Err(StoreError::InvalidResearchDiagnostic(
                "story graph ID conflicts with persisted graph evidence".into(),
            ));
        }
        transaction.commit()?;
        Ok(PersistedDiagnosticStoryGraph {
            story_graph_id: graph.id(),
            graph_fingerprint,
            record_fingerprint: record.fingerprint(),
        })
    }

    /// Persist one source-verified `StoryState`. Unknown fields remain explicit;
    /// at least one grounded field or grounded absence is required.
    pub fn persist_diagnostic_story_state(
        &mut self,
        graph: &StoryGraph,
        state: &StoryState,
        source_revision_id: RevisionId,
    ) -> Result<PersistedDiagnosticStoryState> {
        state.validate_against_graph(graph).map_err(invalid_story)?;
        let graph_fingerprint = BlobId::digest(&serde_json::to_vec(graph)?);
        ensure_persisted_graph(
            &self.connection,
            self.manifest.project_id,
            graph.id(),
            graph_fingerprint,
        )?;
        let source_blob_id = revision_blob_id(&self.connection, source_revision_id)?;
        let source_bytes = self.read_blob(source_blob_id)?;
        state
            .verify_source(source_revision_id, &source_bytes)
            .map_err(invalid_story)?;
        let fact_count = grounded_state_fact_count(state)?;
        let canonical = serde_json::to_vec(state)?;
        let state_fingerprint = BlobId::digest(&canonical);
        let record = self.persist_research_execution_record(
            ResearchExecutionRecordKind::StoryState,
            &canonical,
        )?;
        let created_at_ms = now_unix_ms();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO research_story_states(
                story_state_id, story_graph_id, anchor_story_node_id,
                source_revision_id, source_blob_id, fact_count,
                state_fingerprint, record_fingerprint, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                state.id().to_string(),
                state.story_graph_id().to_string(),
                state.at_node().to_string(),
                source_revision_id.to_string(),
                source_blob_id.to_string(),
                fact_count,
                state_fingerprint.to_string(),
                record.fingerprint().to_string(),
                created_at_ms,
            ],
        )?;
        let exact: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM research_story_states
             WHERE story_state_id = ?1 AND story_graph_id = ?2
               AND anchor_story_node_id = ?3 AND source_revision_id = ?4
               AND source_blob_id = ?5 AND fact_count = ?6
               AND state_fingerprint = ?7 AND record_fingerprint = ?8",
            params![
                state.id().to_string(),
                state.story_graph_id().to_string(),
                state.at_node().to_string(),
                source_revision_id.to_string(),
                source_blob_id.to_string(),
                fact_count,
                state_fingerprint.to_string(),
                record.fingerprint().to_string(),
            ],
            |row| row.get(0),
        )?;
        if exact != 1 {
            return Err(StoreError::InvalidResearchDiagnostic(
                "story state ID conflicts with persisted state evidence".into(),
            ));
        }
        transaction.commit()?;
        Ok(PersistedDiagnosticStoryState {
            story_state_id: state.id(),
            state_fingerprint,
            record_fingerprint: record.fingerprint(),
            source_revision_id,
            source_blob_id,
        })
    }
}

fn ensure_persisted_graph(
    connection: &rusqlite::Connection,
    project_id: ProjectId,
    graph_id: StoryGraphId,
    graph_fingerprint: BlobId,
) -> Result<()> {
    let exact: i64 = connection.query_row(
        "SELECT COUNT(*) FROM research_story_graphs
         WHERE story_graph_id = ?1 AND project_id = ?2 AND graph_fingerprint = ?3",
        params![
            graph_id.to_string(),
            project_id.to_string(),
            graph_fingerprint.to_string(),
        ],
        |row| row.get(0),
    )?;
    if exact != 1 {
        return Err(StoreError::InvalidResearchDiagnostic(
            "story state graph is absent or differs from persisted graph evidence".into(),
        ));
    }
    Ok(())
}

fn revision_blob_id(connection: &rusqlite::Connection, revision_id: RevisionId) -> Result<BlobId> {
    let encoded = connection
        .query_row(
            "SELECT artifact.blob_id
             FROM revisions revision
             JOIN artifacts artifact ON artifact.artifact_id = revision.artifact_id
             WHERE revision.revision_id = ?1",
            [revision_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::InvalidResearchDiagnostic(
                "story state source revision is not present in this project".into(),
            )
        })?;
    encoded.parse().map_err(|error| {
        StoreError::CorruptDatabase(format!(
            "invalid story-state source blob fingerprint: {error}"
        ))
    })
}

fn grounded_state_fact_count(state: &StoryState) -> Result<i64> {
    let explicit = [state.chronology(), state.location(), state.point_of_view()]
        .into_iter()
        .filter(|field| matches!(field, ExplicitStoryStateField::Grounded { .. }))
        .count();
    let collections = [
        state.physical_configuration(),
        state.character_knowledge(),
        state.character_conditions(),
        state.world_facts(),
        state.unresolved_promises(),
        state.voice_constraints(),
        state.possible_next_actions(),
    ];
    let grounded = collections
        .into_iter()
        .map(grounded_collection_count)
        .try_fold(explicit, usize::checked_add)
        .ok_or_else(|| invalid_story("story-state grounded fact count overflow"))?;
    if grounded == 0 || grounded > 4_096 {
        return Err(invalid_story(
            "story state must contain 1..=4096 grounded facts or grounded absences",
        ));
    }
    checked_count(grounded, "story state fact count")
}

fn grounded_collection_count(collection: &GroundedFactCollection) -> usize {
    match collection.known_facts() {
        Some(facts) if !facts.is_empty() => facts.len(),
        Some(_) if collection.known_empty_evidence().is_some() => 1,
        Some(_) | None => 0,
    }
}

fn checked_count(value: usize, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        StoreError::InvalidResearchDiagnostic(format!("{field} exceeds SQLite's integer domain"))
    })
}

fn invalid_story(error: impl std::fmt::Display) -> StoreError {
    StoreError::InvalidResearchDiagnostic(format!("invalid story evidence: {error}"))
}

#[cfg(test)]
mod tests {
    use loom_document::DocumentContent;
    use loom_research_types::{
        ExplicitStoryStateField, GroundedFactCollection, NonEmptyByteRange, StateEvidenceSpan,
        StoryNode, StoryNodeId, StoryNodeKind, StoryNodeName,
    };
    use tempfile::tempdir;

    use super::*;

    fn story_graph() -> (StoryGraph, StoryNodeId) {
        let book = StoryNodeId::new();
        let chapter = StoryNodeId::new();
        let scene = StoryNodeId::new();
        let nodes = vec![
            StoryNode::new(
                book,
                StoryNodeKind::Book,
                None,
                0,
                StoryNodeName::new("Book").expect("book name"),
            ),
            StoryNode::new(
                chapter,
                StoryNodeKind::Chapter,
                Some(book),
                0,
                StoryNodeName::new("Chapter").expect("chapter name"),
            ),
            StoryNode::new(
                scene,
                StoryNodeKind::Scene,
                Some(chapter),
                0,
                StoryNodeName::new("Scene").expect("scene name"),
            ),
        ];
        (
            StoryGraph::new(StoryGraphId::new(), nodes, vec![]).expect("story graph"),
            scene,
        )
    }

    fn unknown(reason: &str) -> GroundedFactCollection {
        GroundedFactCollection::unknown(reason).expect("explicit unknown")
    }

    fn source_bound_state(
        graph: &StoryGraph,
        at_node: StoryNodeId,
        state_id: StoryStateId,
        revision_id: RevisionId,
        source_blob_id: BlobId,
    ) -> StoryState {
        let evidence = StateEvidenceSpan::new(
            revision_id,
            source_blob_id,
            NonEmptyByteRange::new(0, 4).expect("source range"),
        )
        .expect("bounded evidence");
        StoryState::new(
            state_id,
            graph.id(),
            at_node,
            ExplicitStoryStateField::grounded("Mara is present", vec![evidence])
                .expect("grounded chronology"),
            ExplicitStoryStateField::unknown("Location is not established")
                .expect("unknown location"),
            unknown("Physical configuration is not established"),
            unknown("Knowledge is not established"),
            unknown("Conditions are not established"),
            unknown("World facts are not established"),
            unknown("Promises are not established"),
            ExplicitStoryStateField::unknown("Point of view is not established")
                .expect("unknown point of view"),
            unknown("Voice is not established"),
            unknown("Next actions are not established"),
        )
        .expect("source-bound story state")
    }

    #[test]
    fn story_graph_and_state_are_exact_source_bound_idempotent_diagnostics() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) = ProjectStore::initialize(directory.path(), "story").expect("store");
        store
            .save_document(
                "manuscript/source.md",
                DocumentContent::Prose("Mara waits by the door.".into()),
                "story source",
            )
            .expect("save source");
        let source = store
            .read_document("manuscript/source.md")
            .expect("read source");
        let (graph, scene) = story_graph();
        let first_graph = store
            .persist_diagnostic_story_graph(&graph)
            .expect("persist graph");
        let second_graph = store
            .persist_diagnostic_story_graph(&graph)
            .expect("repeat graph");
        assert_eq!(first_graph, second_graph);
        assert_eq!(
            first_graph.graph_fingerprint(),
            BlobId::digest(&serde_json::to_vec(&graph).expect("canonical graph"))
        );

        let state = source_bound_state(
            &graph,
            scene,
            StoryStateId::new(),
            source.revision_id,
            source.blob_id,
        );
        let first_state = store
            .persist_diagnostic_story_state(&graph, &state, source.revision_id)
            .expect("persist state");
        let second_state = store
            .persist_diagnostic_story_state(&graph, &state, source.revision_id)
            .expect("repeat state");
        assert_eq!(first_state, second_state);
        assert_eq!(first_state.source_blob_id(), source.blob_id);
        assert_eq!(
            first_state.state_fingerprint(),
            BlobId::digest(&serde_json::to_vec(&state).expect("canonical state"))
        );
        assert_eq!(
            first_state.record_fingerprint(),
            first_state.state_fingerprint(),
            "the execution record must contain the exact canonical state bytes"
        );

        let row: (String, String, i64) = store
            .connection
            .query_row(
                "SELECT source_revision_id, source_blob_id, fact_count
                 FROM research_story_states WHERE story_state_id = ?1",
                [state.id().to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("persisted state row");
        assert_eq!(row.0, source.revision_id.to_string());
        assert_eq!(row.1, source.blob_id.to_string());
        assert_eq!(row.2, 1);
    }

    #[test]
    fn story_state_rejects_unpersisted_graph_wrong_source_and_identity_conflicts() {
        let directory = tempdir().expect("temporary project");
        let (mut store, _) = ProjectStore::initialize(directory.path(), "story").expect("store");
        store
            .save_document(
                "manuscript/source.md",
                DocumentContent::Prose("Mara waits by the door.".into()),
                "story source",
            )
            .expect("save source");
        let source = store
            .read_document("manuscript/source.md")
            .expect("read source");
        let (graph, scene) = story_graph();
        let state_id = StoryStateId::new();
        let valid = source_bound_state(&graph, scene, state_id, source.revision_id, source.blob_id);
        assert!(matches!(
            store.persist_diagnostic_story_state(&graph, &valid, source.revision_id),
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));

        store
            .persist_diagnostic_story_graph(&graph)
            .expect("persist graph");
        let wrong_source = source_bound_state(
            &graph,
            scene,
            StoryStateId::new(),
            source.revision_id,
            BlobId::digest(b"substituted source"),
        );
        assert!(matches!(
            store.persist_diagnostic_story_state(&graph, &wrong_source, source.revision_id),
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));

        store
            .persist_diagnostic_story_state(&graph, &valid, source.revision_id)
            .expect("persist valid state");
        let conflicting =
            source_bound_state(&graph, scene, state_id, source.revision_id, source.blob_id);
        let mut value = serde_json::to_value(conflicting).expect("state JSON");
        value["chronology"]["value"]["value"] = serde_json::json!("A different claim");
        let conflicting: StoryState = serde_json::from_value(value).expect("valid altered state");
        assert!(matches!(
            store.persist_diagnostic_story_state(&graph, &conflicting, source.revision_id),
            Err(StoreError::InvalidResearchDiagnostic(_))
        ));
    }
}
