//! Owned durable form of the common document. Storage protection is selected by
//! its repository, never by the document's current layout.
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    BranchIndex, Document, DocumentError, HistoryError, HistoryNode, MAX_DOCUMENT_BYTES, PartKind,
};

const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Message,
    Artifact,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReference {
    pub kind: SourceKind,
    /// An occurrence ID, never a content digest.
    pub occurrence_id: String,
    pub start_byte: u64,
    pub end_byte: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentLineage {
    pub revision: Option<RevisionLineage>,
    pub source: Option<SourceLineage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionLineage {
    pub revision_id: String,
    pub parent_revision_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLineage {
    pub document_id: String,
    pub part_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "storage",
    content = "text",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PartContent {
    Inline(String),
    /// Resolve the exact source occurrence and byte range through the owning
    /// repository. No file path or arbitrary read authority is present here.
    Source,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotPart<Metadata> {
    pub id: String,
    /// Conversation-context parentage, not edit-revision parentage.
    pub parent_id: Option<String>,
    pub kind: PartKind,
    pub source: SourceReference,
    pub content: PartContent,
    pub metadata: Metadata,
}

impl<Metadata> HistoryNode for SnapshotPart<Metadata> {
    type Id = String;
    fn id(&self) -> &String {
        &self.id
    }
    fn parent_id(&self) -> Option<&String> {
        self.parent_id.as_ref()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DocumentSelection {
    Sequence,
    Branch {
        head: String,
    },
    /// Preserve a document's explicit default selection policy.
    LatestBranch,
}

/// One owned serializable aggregate for writing and conversation. Header and part metadata
/// stay typed by the owner; common text/source/role/lineage has only one codec.
/// Private fields prevent mutation after validation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocumentSnapshot<Header, Metadata> {
    schema_version: u32,
    document_id: String,
    selection: DocumentSelection,
    lineage: DocumentLineage,
    metadata: Header,
    parts: Vec<SnapshotPart<Metadata>>,
}

impl<Header: Serialize, Metadata: Serialize> DocumentSnapshot<Header, Metadata> {
    pub fn new(
        document_id: String,
        selection: DocumentSelection,
        lineage: DocumentLineage,
        metadata: Header,
        parts: Vec<SnapshotPart<Metadata>>,
    ) -> Result<Self, SnapshotError> {
        let snapshot = Self {
            schema_version: SCHEMA_VERSION,
            document_id,
            selection,
            lineage,
            metadata,
            parts,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
    pub fn document_id(&self) -> &str {
        &self.document_id
    }
    pub const fn selection(&self) -> &DocumentSelection {
        &self.selection
    }
    pub const fn lineage(&self) -> &DocumentLineage {
        &self.lineage
    }
    pub const fn metadata(&self) -> &Header {
        &self.metadata
    }
    pub fn parts(&self) -> &[SnapshotPart<Metadata>] {
        &self.parts
    }
    fn validate(&self) -> Result<(), SnapshotError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(SnapshotError::Schema);
        }
        validate_id(&self.document_id)?;
        if let Some(revision) = &self.lineage.revision {
            validate_id(&revision.revision_id)?;
            if let Some(parent) = &revision.parent_revision_id {
                validate_id(parent)?;
                if parent == &revision.revision_id {
                    return Err(SnapshotError::Lineage);
                }
            }
        }
        if let Some(source) = &self.lineage.source {
            validate_id(&source.document_id)?;
            if let Some(part) = &source.part_id {
                validate_id(part)?;
            }
        }
        let index = BranchIndex::new(&self.parts)?;
        if let DocumentSelection::Branch { head } = &self.selection {
            index.path(Some(head))?;
        }
        let mut metadata_writer = MetadataBudget(0);
        serde_json::to_writer(&mut metadata_writer, &self.metadata)?;
        let mut length = 0_u64;
        for part in &self.parts {
            serde_json::to_writer(&mut metadata_writer, &part.metadata)?;
            validate_id(&part.id)?;
            validate_id(&part.source.occurrence_id)?;
            let bytes = part
                .source
                .end_byte
                .checked_sub(part.source.start_byte)
                .ok_or(SnapshotError::SourceRange)?;
            if let PartContent::Inline(text) = &part.content
                && u64::try_from(text.len()).ok() != Some(bytes)
            {
                return Err(SnapshotError::SourceRange);
            }
            length = length
                .checked_add(bytes)
                .filter(|len| *len <= MAX_DOCUMENT_BYTES as u64)
                .ok_or(DocumentError::ByteBudget)?;
        }
        Ok(())
    }

    /// Resolve into the same content aggregate used by writing and chat. This
    /// explicit callback is the only place source bytes can be acquired.
    pub fn resolve<E>(
        &self,
        mut resolve: impl FnMut(&SourceReference) -> Result<Vec<u8>, E>,
    ) -> Result<Document<'static, SourceReference, Metadata>, E>
    where
        Metadata: Clone,
        E: From<SnapshotError>,
    {
        let mut document = Document::new();
        let index = BranchIndex::new(&self.parts).map_err(SnapshotError::from)?;
        let parts = match &self.selection {
            DocumentSelection::Sequence => self.parts.iter().collect(),
            DocumentSelection::Branch { head } => {
                index.path(Some(head)).map_err(SnapshotError::from)?
            }
            DocumentSelection::LatestBranch => index
                .path(self.parts.last().map(|part| &part.id))
                .map_err(SnapshotError::from)?,
        };
        for part in parts {
            match &part.content {
                PartContent::Inline(text) => {
                    // Inline text already represents the selected source range.
                    document
                        .push_owned_part(
                            part.source.clone(),
                            part.source.start_byte..part.source.end_byte,
                            text.clone(),
                            part.kind,
                            part.metadata.clone(),
                        )
                        .map_err(SnapshotError::from)?;
                }
                PartContent::Source => {
                    let bytes = resolve(&part.source)?;
                    document
                        .push_slice(
                            part.source.clone(),
                            &bytes,
                            part.source.start_byte..part.source.end_byte,
                            part.kind,
                            part.metadata.clone(),
                        )
                        .map_err(SnapshotError::from)?;
                }
            }
        }
        Ok(document)
    }
}

impl<'de, Header: Deserialize<'de> + Serialize, Metadata: Deserialize<'de> + Serialize>
    Deserialize<'de> for DocumentSnapshot<Header, Metadata>
{
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Stored<Header, Metadata> {
            schema_version: u32,
            document_id: String,
            selection: DocumentSelection,
            lineage: DocumentLineage,
            metadata: Header,
            parts: Vec<SnapshotPart<Metadata>>,
        }
        let stored = Stored::deserialize(deserializer)?;
        let snapshot = Self {
            schema_version: stored.schema_version,
            document_id: stored.document_id,
            selection: stored.selection,
            lineage: stored.lineage,
            metadata: stored.metadata,
            parts: stored.parts,
        };
        snapshot.validate().map_err(serde::de::Error::custom)?;
        Ok(snapshot)
    }
}

struct MetadataBudget(usize);
impl std::io::Write for MetadataBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|length| *length <= 16 * 1024 * 1024)
            .ok_or_else(|| std::io::Error::other("document metadata exceeds 16 MiB"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_id(id: &str) -> Result<(), SnapshotError> {
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return Err(SnapshotError::Identity);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("document metadata encoding or budget failed: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("unsupported document snapshot schema")]
    Schema,
    #[error("invalid document occurrence identity")]
    Identity,
    #[error("document revision is its own parent")]
    Lineage,
    #[error("document source range is reversed or disagrees with inline text")]
    SourceRange,
    #[error(transparent)]
    Content(#[from] DocumentError),
    #[error(transparent)]
    History(#[from] HistoryError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageRole;
    use serde_json::{Value, json};

    fn part(id: &str, parent: Option<&str>, text: &str) -> SnapshotPart<Value> {
        SnapshotPart {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            kind: PartKind::Message(MessageRole::Assistant),
            source: SourceReference {
                kind: SourceKind::Message,
                occurrence_id: id.into(),
                start_byte: 0,
                end_byte: text.len() as u64,
            },
            content: PartContent::Inline(text.into()),
            metadata: json!({"receipt_id": "original-receipt"}),
        }
    }

    fn snapshot() -> DocumentSnapshot<Value, Value> {
        DocumentSnapshot::new(
            "document-1".into(),
            DocumentSelection::Branch {
                head: "selected".into(),
            },
            DocumentLineage::default(),
            json!({"title": "Private document"}),
            vec![
                part("root", None, "café\r\n"),
                part("hidden", Some("root"), "alternative"),
                part("selected", Some("root"), "chosen"),
            ],
        )
        .expect("valid snapshot")
    }

    #[test]
    fn durable_round_trip_preserves_all_branches_but_resolves_only_selected_bytes() {
        let original = snapshot();
        let encoded = serde_json::to_vec(&original).expect("valid snapshot fixture");
        let decoded: DocumentSnapshot<Value, Value> =
            serde_json::from_slice(&encoded).expect("valid snapshot fixture");
        assert_eq!(decoded, original);
        assert_eq!(decoded.parts().len(), 3);
        let selected = decoded
            .resolve::<SnapshotError>(|_| panic!("inline document requires no authority"))
            .expect("valid snapshot fixture");
        assert_eq!(selected.text(), "café\r\nchosen");
        assert_eq!(
            selected.parts()[1].metadata()["receipt_id"],
            "original-receipt"
        );
    }

    #[test]
    fn durable_decoder_rejects_schema_identity_head_range_and_hidden_corruption() {
        let original = serde_json::to_value(snapshot()).expect("valid snapshot fixture");
        let mutations = [
            ("/schema_version", json!(9)),
            ("/document_id", json!("x".repeat(513))),
            ("/selection/head", json!("missing")),
            ("/parts/1/parent_id", json!("hidden")),
            ("/parts/1/id", json!("")),
            ("/parts/1/source/occurrence_id", json!("bad\nidentity")),
            ("/parts/1/source/end_byte", json!(1)),
        ];
        for (pointer, replacement) in mutations {
            let mut value = original.clone();
            *value.pointer_mut(pointer).expect("valid snapshot fixture") = replacement;
            assert!(
                serde_json::from_value::<DocumentSnapshot<Value, Value>>(value).is_err(),
                "{pointer}"
            );
        }
    }

    #[test]
    fn source_resolution_checks_exact_utf8_range_and_retains_occurrence_identity() {
        let mut part = part("part", None, "é");
        part.content = PartContent::Source;
        part.source.kind = SourceKind::Artifact;
        part.source.start_byte = 1;
        part.source.end_byte = 3;
        let snapshot = DocumentSnapshot::new(
            "document".into(),
            DocumentSelection::Sequence,
            DocumentLineage::default(),
            (),
            vec![part],
        )
        .expect("valid snapshot fixture");
        let document = snapshot
            .resolve::<SnapshotError>(|source| {
                assert_eq!(source.occurrence_id, "part");
                Ok("aéb".as_bytes().to_vec())
            })
            .expect("valid snapshot fixture");
        assert_eq!(document.text(), "é");
        assert_eq!(document.parts()[0].source_range(), 1..3);
        assert!(
            snapshot
                .resolve::<SnapshotError>(|_| Ok("éab".as_bytes().to_vec()))
                .is_err()
        );
    }

    #[test]
    fn durable_metadata_is_bounded_independently_of_text() {
        let result = DocumentSnapshot::new(
            "document".into(),
            DocumentSelection::Sequence,
            DocumentLineage::default(),
            "x".repeat(16 * 1024 * 1024),
            Vec::<SnapshotPart<()>>::new(),
        );
        assert!(matches!(result, Err(SnapshotError::Metadata(_))));
    }
}
