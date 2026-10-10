//! Durable shared document snapshots inside immutable revision artifact metadata.
//! Current-main revisions derive the same view from their authoritative segments.
//! When present, durable snapshots must agree with the immutable SQL records.
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
const REVISION_FORMAT: &str = "workspace-document.v1";

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
    let snapshot = from_segments(identity, segments)?;
    let object = metadata
        .as_object_mut()
        .ok_or_else(|| StoreError::CorruptDatabase("revision metadata is not an object".into()))?;
    object.insert("revision_format".into(), REVISION_FORMAT.into());
    object.insert("document_snapshot".into(), serde_json::to_value(snapshot)?);
    Ok(metadata)
}

fn from_segments(
    identity: RevisionIdentity,
    segments: &[StoredSegment],
) -> Result<RevisionSnapshot> {
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
    Ok(DocumentSnapshot::new(
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
    )?)
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
    let metadata = metadata
        .as_object_mut()
        .ok_or_else(|| StoreError::CorruptDatabase("revision metadata is not an object".into()))?;
    let declared_format = metadata.get("revision_format");
    if declared_format.is_some_and(|format| format.as_str() != Some(REVISION_FORMAT)) {
        return Err(StoreError::CorruptDatabase(
            "unsupported revision format".into(),
        ));
    }
    let snapshot_required = declared_format.is_some();
    let segments = store.load_revision_segment_index(revision_id)?;
    let Some(value) = metadata.remove("document_snapshot") else {
        if snapshot_required {
            return Err(StoreError::CorruptDatabase(
                "revision is missing its declared document snapshot".into(),
            ));
        }
        // Schema-15 revisions have one existing immutable authority. Adapt it
        // in memory; never rewrite old artifacts or backfill another store.
        return from_segments(
            RevisionIdentity {
                document_id: row.0.parse().map_err(|error| {
                    StoreError::CorruptDatabase(format!("invalid document identity: {error}"))
                })?,
                revision_id,
                parent_revision_id: row.1.as_deref().map(str::parse).transpose().map_err(
                    |error| {
                        StoreError::CorruptDatabase(format!("invalid parent revision: {error}"))
                    },
                )?,
                kind: row.2.parse().map_err(|error| {
                    StoreError::CorruptDatabase(format!("invalid document kind: {error}"))
                })?,
            },
            &segments,
        );
    };
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

// These fixtures exercise the Unix-only private project storage boundary.
// Portable document schema validation remains in workspace-document's tests.
#[cfg(all(test, unix))]
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
    fn refused_snapshot_destination_never_materializes_private_source_evidence() {
        for (target, reason) in [
            ("../outside.md", "import".to_owned()),
            ("registered.md", "import".to_owned()),
            ("visible.md", "import".to_owned()),
            ("new.md", "x".repeat(4097)),
        ] {
            let dir = tempfile::tempdir().expect("snapshot store fixture");
            let (mut store, _) =
                ProjectStore::initialize(dir.path(), "Admission").expect("snapshot store fixture");
            store
                .create_document_if_absent(
                    "registered.md",
                    DocumentContent::Prose("existing writing".into()),
                    "fixture",
                )
                .expect("registered fixture");
            std::fs::write(dir.path().join("visible.md"), "external writing")
                .expect("visible fixture");
            let source = serde_json::to_string(&imported()).expect("snapshot fixture");
            let source_id = loom_types::BlobId::digest(source.as_bytes());
            let before = store.counts().expect("before counts");
            assert!(
                store
                    .import_document_snapshot_if_absent(target, &source, reason)
                    .is_err()
            );
            assert_eq!(store.counts().expect("after counts"), before);
            assert!(
                matches!(
                    store.read_blob(source_id),
                    Err(StoreError::MissingBlob { .. })
                ),
                "refused target {target} retained private source bytes"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("registered.md")).expect("original"),
                "existing writing"
            );
            assert_eq!(
                std::fs::read_to_string(dir.path().join("visible.md")).expect("external"),
                "external writing"
            );
        }
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

    /// Reproduce origin/main 95c446b's `adopt_visible_document_if_absent` records.
    /// This inserts the main-format rows directly, with all immutable triggers
    /// enabled. It never manufactures a snapshot and then strips it afterward.
    fn main_revision_fixture(root: &std::path::Path, text: &str) -> crate::SaveOutcome {
        use loom_types::{CommandKind, OperationId};
        let (mut store, _) = ProjectStore::initialize(root, "Current main").expect("fixture");
        let blob_id = store.put_blob(text.as_bytes()).expect("fixture blob");
        std::fs::write(root.join("story.md"), text).expect("fixture manuscript");
        let document_id = DocumentId::new();
        let artifact_id = ArtifactId::new();
        let operation_id = OperationId::new();
        let revision_id = RevisionId::new();
        let byte_len = i64::try_from(text.len()).expect("bounded fixture");
        let metadata = serde_json::json!({
            "workflow": "adopt_visible_document", "source": "existing_visible_file",
            "relative_path": "story.md", "reason": "current main fixture",
            "source_blob_id": blob_id,
        })
        .to_string();
        let transaction = store.connection.transaction().expect("fixture transaction");
        transaction
            .execute(
                "INSERT INTO blobs VALUES (?1, ?2, 'application/octet-stream', 1)",
                params![blob_id.to_string(), byte_len],
            )
            .expect("main blob row");
        transaction.execute(
            "INSERT INTO documents(document_id, relative_path, document_kind, created_at_ms) VALUES (?1, 'story.md', 'prose', 1)",
            [document_id.to_string()],
        ).expect("main document row");
        transaction.execute(
            "INSERT INTO artifacts VALUES (?1, ?2, 'human_contribution', 'text/markdown; charset=utf-8', ?3, 1)",
            params![artifact_id.to_string(), blob_id.to_string(), metadata],
        ).expect("main artifact row");
        transaction
            .execute(
                "INSERT INTO operations VALUES (?1, 'import', ?2, 1)",
                params![operation_id.to_string(), metadata],
            )
            .expect("main operation row");
        transaction
            .execute(
                "INSERT INTO operation_outputs VALUES (?1, 0, ?2)",
                params![operation_id.to_string(), artifact_id.to_string()],
            )
            .expect("main output row");
        transaction
            .execute(
                "INSERT INTO revisions VALUES (?1, ?2, NULL, ?3, 'current main fixture', 1)",
                params![
                    revision_id.to_string(),
                    document_id.to_string(),
                    artifact_id.to_string()
                ],
            )
            .expect("main revision row");
        if byte_len != 0 {
            transaction
                .execute(
                    "INSERT INTO revision_segments VALUES (?1, 0, ?2, 0, ?3, 'human')",
                    params![revision_id.to_string(), artifact_id.to_string(), byte_len],
                )
                .expect("main source segment");
        }
        transaction.execute(
            "INSERT INTO visible_file_outbox(revision_id, relative_path, target_blob_id, expected_visible_blob_id, state, created_at_ms, completed_at_ms) VALUES (?1, 'story.md', ?2, ?2, 'completed', 1, 1)",
            params![revision_id.to_string(), blob_id.to_string()],
        ).expect("main outbox row");
        transaction.commit().expect("main records");
        let receipt = store.new_receipt(
            CommandKind::Import,
            1,
            None,
            &[artifact_id],
            &[operation_id],
            &[revision_id],
        );
        store.persist_receipt(&receipt).expect("main receipt");
        // Pin the actual opening boundary, independently of this build's value.
        store
            .connection
            .pragma_update(None, "user_version", 15)
            .expect("main schema");
        crate::SaveOutcome {
            blob_id,
            artifact_id,
            operation_id,
            revision_id,
            receipt,
        }
    }

    fn revision_metadata(store: &ProjectStore, artifact_id: ArtifactId) -> String {
        store
            .connection
            .query_row(
                "SELECT metadata_json FROM artifacts WHERE artifact_id = ?1",
                [artifact_id.to_string()],
                |row| row.get(0),
            )
            .expect("revision metadata")
    }

    #[test]
    fn current_main_store_opens_edits_and_reopens_without_rewriting_history() {
        for original in ["  café\r\nexact  ", ""] {
            let dir = tempfile::tempdir().expect("fixture directory");
            let saved = main_revision_fixture(dir.path(), original);
            let mut store = ProjectStore::open(dir.path()).expect("open current-main store");
            assert_eq!(crate::CURRENT_STORE_SCHEMA_VERSION, 15);
            let before = revision_metadata(&store, saved.artifact_id);
            assert!(!before.contains("document_snapshot"));
            assert_eq!(
                store
                    .reconstruct_revision(saved.revision_id)
                    .expect("original"),
                original.as_bytes()
            );
            let provenance = store
                .revision_provenance(saved.revision_id)
                .expect("original provenance");
            let edited = format!("{original}added é\r\n");
            let changed = store
                .save_document_if_source(
                    "story.md",
                    DocumentContent::Prose(edited.clone()),
                    "edit main document",
                    saved.revision_id,
                    saved.blob_id,
                )
                .expect("edit current-main revision")
                .save;
            assert_eq!(revision_metadata(&store, saved.artifact_id), before);
            assert_eq!(
                load(&store, changed.revision_id)
                    .expect("new snapshot")
                    .lineage()
                    .revision
                    .as_ref()
                    .expect("lineage")
                    .parent_revision_id,
                Some(saved.revision_id.to_string())
            );
            let new_metadata: Value =
                serde_json::from_str(&revision_metadata(&store, changed.artifact_id))
                    .expect("metadata");
            assert_eq!(new_metadata["revision_format"], REVISION_FORMAT);
            assert!(new_metadata["document_snapshot"].is_object());
            drop(store);
            let store = ProjectStore::open(dir.path()).expect("reopen mixed revisions");
            assert_eq!(
                store
                    .reconstruct_revision(saved.revision_id)
                    .expect("old revision"),
                original.as_bytes()
            );
            assert_eq!(
                store
                    .reconstruct_revision(changed.revision_id)
                    .expect("new revision"),
                edited.as_bytes()
            );
            assert_eq!(
                std::fs::read(dir.path().join("story.md")).expect("visible writing"),
                edited.as_bytes()
            );
            assert_eq!(revision_metadata(&store, saved.artifact_id), before);
            assert_eq!(
                store
                    .revision_provenance(saved.revision_id)
                    .expect("unchanged provenance"),
                provenance
            );
            assert_eq!(
                store
                    .load_receipt(saved.receipt.command_id)
                    .expect("retained receipt"),
                Some(saved.receipt)
            );
        }
    }

    #[test]
    fn malformed_or_missing_declared_snapshot_never_falls_back_to_segment_rows() {
        for replacement in [
            serde_json::json!({ "revision_format": REVISION_FORMAT }),
            serde_json::json!({ "document_snapshot": null }),
            serde_json::json!({ "document_snapshot": {} }),
            serde_json::json!({ "revision_format": "unknown-format" }),
        ] {
            let dir = tempfile::tempdir().expect("fixture directory");
            let (mut store, _) =
                ProjectStore::initialize(dir.path(), "Integrity").expect("fixture");
            let saved = store
                .create_document_if_absent(
                    "story.md",
                    DocumentContent::Prose("original".into()),
                    "create",
                )
                .expect("fixture");
            store
                .connection
                .execute_batch("DROP TRIGGER artifacts_are_immutable_update")
                .expect("tamper fixture");
            store
                .connection
                .execute(
                    "UPDATE artifacts SET metadata_json = ?1 WHERE artifact_id = ?2",
                    params![replacement.to_string(), saved.artifact_id.to_string()],
                )
                .expect("tampered metadata");
            assert!(
                store.reconstruct_revision(saved.revision_id).is_err(),
                "{replacement}"
            );
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
                std::fs::read(dir.path().join("story.md")).expect("preserved writing"),
                b"original"
            );
            assert_eq!(
                revision_metadata(&store, saved.artifact_id),
                replacement.to_string()
            );
        }
    }

    #[test]
    fn current_main_segments_still_reject_corrupt_source_ranges() {
        for end_byte in [1, 999] {
            let dir = tempfile::tempdir().expect("fixture directory");
            let saved = main_revision_fixture(dir.path(), "é exact");
            let mut store = ProjectStore::open(dir.path()).expect("main fixture");
            store
                .connection
                .execute_batch("DROP TRIGGER revision_segments_are_immutable_update")
                .expect("tamper fixture");
            store
                .connection
                .execute(
                    "UPDATE revision_segments SET end_byte = ?1 WHERE revision_id = ?2",
                    params![end_byte, saved.revision_id.to_string()],
                )
                .expect("tampered source");
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
                std::fs::read(dir.path().join("story.md")).expect("preserved writing"),
                "é exact".as_bytes()
            );
        }
    }
}
