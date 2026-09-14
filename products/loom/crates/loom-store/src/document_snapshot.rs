//! Durable shared document snapshots inside immutable revision artifact metadata.
//! SQL segment rows remain lookup indexes and are checked against the snapshot.
use loom_types::{ArtifactId, ContributionKind, DocumentId, DocumentKind, RevisionId};
use rusqlite::params;
use serde_json::Value;
use workspace_document::{
    DocumentLineage, DocumentSelection, DocumentSnapshot, PartContent, PartKind, RevisionLineage,
    SnapshotPart, SourceKind, SourceReference,
};

use crate::provenance::StoredSegment;
use crate::{ProjectStore, Result, StoreError};

pub(crate) type RevisionSnapshot = DocumentSnapshot<DocumentKind, ContributionKind>;

#[derive(Clone, Copy)]
pub(crate) struct RevisionIdentity {
    pub document_id: DocumentId,
    pub revision_id: RevisionId,
    pub parent_revision_id: Option<RevisionId>,
    pub kind: DocumentKind,
}

pub(crate) fn seal(
    mut metadata: Value,
    identity: RevisionIdentity,
    segments: &[StoredSegment],
) -> Result<Value> {
    let parts = segments
        .iter()
        .enumerate()
        .map(|(index, segment)| SnapshotPart {
            id: format!("{}:{index}", identity.revision_id),
            parent_id: None,
            kind: PartKind::Text,
            source: SourceReference {
                kind: SourceKind::Artifact,
                occurrence_id: segment.artifact_id.to_string(),
                start_byte: segment.start,
                end_byte: segment.end,
            },
            content: PartContent::Source,
            metadata: segment.contribution,
        })
        .collect();
    let snapshot = DocumentSnapshot::new(
        identity.document_id.to_string(),
        DocumentSelection::Sequence,
        DocumentLineage {
            revision: Some(RevisionLineage {
                revision_id: identity.revision_id.to_string(),
                parent_revision_id: identity.parent_revision_id.map(|id| id.to_string()),
            }),
            source: None,
        },
        identity.kind,
        parts,
    )?;
    metadata
        .as_object_mut()
        .ok_or_else(|| StoreError::CorruptDatabase("revision metadata is not an object".into()))?
        .insert("document_snapshot".into(), serde_json::to_value(snapshot)?);
    Ok(metadata)
}

pub(crate) fn single_segment(
    artifact_id: ArtifactId,
    len: u64,
    contribution: ContributionKind,
) -> Vec<StoredSegment> {
    if len == 0 {
        Vec::new()
    } else {
        vec![StoredSegment {
            artifact_id,
            start: 0,
            end: len,
            contribution,
        }]
    }
}

pub(crate) fn load(store: &ProjectStore, revision_id: RevisionId) -> Result<RevisionSnapshot> {
    let row: (String, Option<String>, String, String) = store.connection.query_row(
        "SELECT r.document_id, r.parent_revision_id, d.document_kind, a.metadata_json FROM revisions r JOIN documents d ON d.document_id = r.document_id JOIN artifacts a ON a.artifact_id = r.artifact_id WHERE r.revision_id = ?1",
        params![revision_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let mut metadata: Value = serde_json::from_str(&row.3)?;
    let value = metadata.as_object_mut().and_then(|metadata| metadata.remove("document_snapshot"))
        .ok_or_else(|| StoreError::CorruptDatabase("revision is missing its required document snapshot; preserve the project and copy its ordinary UTF-8 manuscript before an explicit import into a new project".into()))?;
    let snapshot: RevisionSnapshot = serde_json::from_value(value)?;
    let revision = snapshot.lineage().revision.as_ref().ok_or_else(|| {
        StoreError::CorruptDatabase("document snapshot has no revision lineage".into())
    })?;
    if snapshot.selection() != &DocumentSelection::Sequence
        || snapshot.document_id() != row.0
        || revision.revision_id != revision_id.to_string()
        || revision.parent_revision_id != row.1
        || snapshot.metadata().as_str() != row.2
        || snapshot.lineage().source.is_some()
    {
        return Err(StoreError::CorruptDatabase(
            "document snapshot disagrees with revision identity".into(),
        ));
    }
    let segments = store.load_revision_segment_index(revision_id)?;
    if snapshot.parts().len() != segments.len() {
        return Err(StoreError::CorruptDatabase(
            "document snapshot disagrees with its segment index".into(),
        ));
    }
    for (index, (part, segment)) in snapshot.parts().iter().zip(&segments).enumerate() {
        if part.id != format!("{revision_id}:{index}")
            || part.parent_id.is_some()
            || part.kind != PartKind::Text
            || part.content != PartContent::Source
            || part.source.kind != SourceKind::Artifact
            || part.source.occurrence_id != segment.artifact_id.to_string()
            || part.source.start_byte != segment.start
            || part.source.end_byte != segment.end
            || part.metadata != segment.contribution
        {
            return Err(StoreError::CorruptDatabase(
                "document snapshot disagrees with its source index".into(),
            ));
        }
    }
    Ok(snapshot)
}

/// Imported source metadata is retained without interpreting it as authority.
/// Consumers can decode their own typed metadata after validating the source.
pub type ImportedDocumentSnapshot = DocumentSnapshot<Value, Value>;

pub(crate) fn parse_import(json: &str) -> Result<ImportedDocumentSnapshot> {
    if json.len() as u64 > crate::MAX_DOCUMENT_BYTES {
        return Err(StoreError::DocumentTooLarge {
            actual_bytes: json.len() as u64,
            max_bytes: crate::MAX_DOCUMENT_BYTES,
        });
    }
    let snapshot: ImportedDocumentSnapshot = serde_json::from_str(json)?;
    if snapshot
        .parts()
        .iter()
        .any(|part| matches!(part.content, PartContent::Source))
    {
        return Err(StoreError::CorruptDatabase("imported document must be self-contained; external source references need an explicit bundle importer".into()));
    }
    Ok(snapshot)
}

impl ProjectStore {
    /// Retrieve the exact complete source of an explicit snapshot import.
    /// This does not restore credentials, tool grants, or referenced attachments.
    pub fn imported_document_snapshot(
        &self,
        revision_id: RevisionId,
    ) -> Result<Option<ImportedDocumentSnapshot>> {
        let metadata: String = self.connection.query_row(
            "SELECT a.metadata_json FROM revisions r JOIN artifacts a ON a.artifact_id = r.artifact_id WHERE r.revision_id = ?1",
            params![revision_id.to_string()], |row| row.get(0),
        )?;
        let metadata: Value = serde_json::from_str(&metadata)?;
        let Some(value) = metadata.get("source_document_snapshot_blob_id") else {
            return Ok(None);
        };
        let blob_id: loom_types::BlobId = serde_json::from_value(value.clone())?;
        let bytes = self.read_blob(blob_id)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            StoreError::CorruptDatabase("source document snapshot is not UTF-8".into())
        })?;
        Ok(Some(parse_import(text)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_document::DocumentContent;
    use serde_json::json;
    use workspace_document::MessageRole;

    fn imported() -> ImportedDocumentSnapshot {
        let messages = [
            ("u", None, MessageRole::User, "café\r\n"),
            (
                "hidden",
                Some("u"),
                MessageRole::Assistant,
                "hidden alternative",
            ),
            ("a", Some("u"), MessageRole::Assistant, "selected answer"),
        ];
        DocumentSnapshot::new("source-chat".into(), DocumentSelection::Branch {head: "a".into()}, DocumentLineage::default(), json!({"title": "private", "tool_grants": ["never-execute"]}), messages.into_iter().map(|(id, parent, role, text)| SnapshotPart {
            id: id.into(), parent_id: parent.map(str::to_owned), kind: PartKind::Message(role),
            source: SourceReference { kind: SourceKind::Message, occurrence_id: id.into(), start_byte: 0, end_byte: text.len() as u64 },
            content: PartContent::Inline(text.into()), metadata: json!({"reasoning_content": "private reasoning", "receipt_id": "source-receipt", "attachment_ids": ["source-attachment"]}),
        }).collect()).expect("snapshot store fixture")
    }

    #[test]
    fn explicit_import_retains_full_source_and_reopens_durable_writing_projection() {
        let dir = tempfile::tempdir().expect("snapshot store fixture");
        let (mut store, _) =
            ProjectStore::initialize(dir.path(), "Import").expect("snapshot store fixture");
        let original = imported();
        let source = serde_json::to_string(&original).expect("snapshot store fixture");
        let saved = store
            .import_document_snapshot_if_absent("transcript.md", &source, "explicit import")
            .expect("snapshot store fixture");
        let expected = original
            .resolve::<StoreError>(|_| panic!("inline source"))
            .expect("snapshot store fixture")
            .transcript()
            .expect("snapshot store fixture");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("transcript.md"))
                .expect("snapshot store fixture"),
            expected
        );
        assert!(!expected.contains("hidden alternative"));
        assert!(!expected.contains("private reasoning"));
        assert_eq!(
            store
                .imported_document_snapshot(saved.revision_id)
                .expect("snapshot store fixture"),
            Some(original.clone())
        );
        assert!(
            store
                .revision_provenance(saved.revision_id)
                .expect("snapshot store fixture")
                .segments
                .iter()
                .all(|part| part.contribution == ContributionKind::Source)
        );
        let durable = load(&store, saved.revision_id).expect("snapshot store fixture");
        assert!(
            durable
                .parts()
                .iter()
                .all(|part| part.content == PartContent::Source)
        );
        assert_eq!(
            durable
                .lineage()
                .revision
                .as_ref()
                .expect("snapshot store fixture")
                .revision_id,
            saved.revision_id.to_string()
        );
        drop(store);
        let store = ProjectStore::open(dir.path()).expect("snapshot store fixture");
        assert_eq!(
            store
                .reconstruct_revision(saved.revision_id)
                .expect("snapshot store fixture"),
            expected.as_bytes()
        );
        assert_eq!(
            store
                .imported_document_snapshot(saved.revision_id)
                .expect("snapshot store fixture"),
            Some(original)
        );
    }

    #[test]
    fn invalid_or_external_import_cannot_create_visible_or_semantic_content() {
        let dir = tempfile::tempdir().expect("snapshot store fixture");
        let (mut store, _) =
            ProjectStore::initialize(dir.path(), "Import").expect("snapshot store fixture");
        let mut value = serde_json::to_value(imported()).expect("snapshot store fixture");
        value["parts"][0]["content"] = json!({"storage": "source"});
        assert!(
            store
                .import_document_snapshot_if_absent("no.md", &value.to_string(), "explicit")
                .is_err()
        );
        assert!(
            store
                .list_documents()
                .expect("snapshot store fixture")
                .is_empty()
        );
        assert!(!dir.path().join("no.md").exists());
    }

    #[test]
    fn import_rendering_uses_typed_parts_instead_of_confusing_selection_with_content() {
        let dir = tempfile::tempdir().expect("snapshot store fixture");
        let (mut store, _) =
            ProjectStore::initialize(dir.path(), "Import").expect("snapshot store fixture");
        let mut source = serde_json::to_value(imported()).expect("snapshot store fixture");
        source["selection"] = json!({"kind": "sequence"});
        store
            .import_document_snapshot_if_absent("sequence.md", &source.to_string(), "explicit")
            .expect("sequence of typed messages");
        let sequence =
            std::fs::read_to_string(dir.path().join("sequence.md")).expect("sequence transcript");
        assert!(sequence.starts_with("## User\ncafé\r\n"));
        assert!(sequence.contains("## Assistant\nhidden alternative"));

        source["selection"] = json!({"kind": "branch", "head": "a"});
        for part in source["parts"].as_array_mut().expect("parts") {
            part["kind"] = json!({"kind": "text"});
        }
        store
            .import_document_snapshot_if_absent("writing.md", &source.to_string(), "explicit")
            .expect("branch of exact writing parts");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("writing.md")).expect("selected writing"),
            "café\r\nselected answer"
        );
    }

    #[test]
    fn snapshot_and_index_disagreement_refuses_reconstruction_and_source_edit() {
        let dir = tempfile::tempdir().expect("snapshot store fixture");
        let (mut store, _) =
            ProjectStore::initialize(dir.path(), "Integrity").expect("snapshot store fixture");
        let saved = store
            .create_document_if_absent(
                "story.md",
                DocumentContent::Prose("original".into()),
                "create",
            )
            .expect("snapshot store fixture");
        store
            .connection
            .execute_batch("DROP TRIGGER revision_segments_are_immutable_update")
            .expect("snapshot store fixture");
        store
            .connection
            .execute(
                "UPDATE revision_segments SET end_byte = 2 WHERE revision_id = ?1",
                [saved.revision_id.to_string()],
            )
            .expect("snapshot store fixture");
        assert!(store.reconstruct_revision(saved.revision_id).is_err());
        assert!(
            store
                .save_document_if_source(
                    "story.md",
                    DocumentContent::Prose("edited".into()),
                    "edit",
                    saved.revision_id,
                    saved.blob_id
                )
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("story.md")).expect("snapshot store fixture"),
            "original"
        );
    }

    #[test]
    fn missing_snapshot_refusal_preserves_original_manuscript_and_metadata() {
        let dir = tempfile::tempdir().expect("snapshot store fixture");
        let (mut store, _) =
            ProjectStore::initialize(dir.path(), "Old format").expect("snapshot store fixture");
        let saved = store
            .create_document_if_absent(
                "story.md",
                DocumentContent::Prose("  exact\r\n".into()),
                "create",
            )
            .expect("snapshot store fixture");
        store
            .connection
            .execute_batch("DROP TRIGGER artifacts_are_immutable_update")
            .expect("snapshot store fixture");
        store
            .connection
            .execute(
                "UPDATE artifacts SET metadata_json = '{}' WHERE artifact_id = ?1",
                [saved.artifact_id.to_string()],
            )
            .expect("snapshot store fixture");
        let error = store
            .reconstruct_revision(saved.revision_id)
            .expect_err("reject previous revision format")
            .to_string();
        assert!(error.contains("preserve the project"));
        let metadata: String = store
            .connection
            .query_row(
                "SELECT metadata_json FROM artifacts WHERE artifact_id = ?1",
                [saved.artifact_id.to_string()],
                |row| row.get(0),
            )
            .expect("snapshot store fixture");
        assert_eq!(metadata, "{}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("story.md")).expect("snapshot store fixture"),
            "  exact\r\n"
        );
    }

    #[test]
    fn previous_store_format_is_rejected_before_recovery_or_semantic_mutation() {
        let dir = tempfile::tempdir().expect("snapshot store fixture");
        let (mut store, _) = ProjectStore::initialize(dir.path(), "Previous format")
            .expect("snapshot store fixture");
        store
            .create_document_if_absent(
                "story.md",
                DocumentContent::Prose("  exact\r\n".into()),
                "create",
            )
            .expect("snapshot store fixture");
        store
            .connection
            .pragma_update(None, "user_version", 15)
            .expect("previous format fixture");
        drop(store);
        let database = dir.path().join(".loom").join(crate::store::DATABASE_FILE);
        let before = std::fs::read(&database).expect("database bytes");
        assert!(matches!(
            ProjectStore::open(dir.path()),
            Err(StoreError::UnsupportedSchema {
                found: 15,
                supported: 16
            })
        ));
        assert_eq!(
            std::fs::read(&database).expect("preserved database"),
            before
        );
        assert_eq!(
            std::fs::read(dir.path().join("story.md")).expect("preserved manuscript"),
            b"  exact\r\n"
        );
    }
}
