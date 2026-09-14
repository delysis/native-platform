#![forbid(unsafe_code)]

use loom_types::{ArtifactId, ByteRange, DocumentKind};
use serde::{Deserialize, Serialize};
use thiserror::Error;
pub use workspace_document::{Document, DocumentPart, MessageRole, PartKind};

mod merge;
pub mod neural_functions;

pub use neural_functions::{
    DocumentReference, NeuralCommand, NeuralExpression, NeuralSyntaxError, document_references,
    parse_neural_command, render_base_function_prompt,
};

pub use merge::{
    DEFAULT_MERGE_BUDGET, MergeBudget, MergeBudgetMetric, MergeConflict, MergeConflictKind,
    MergeConflictSpan, MergeError, MergeOutcome, three_way_merge, three_way_merge_with_budget,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "content", rename_all = "snake_case")]
pub enum DocumentContent {
    Hybrid(Vec<HybridBlock>),
    Prose(String),
    Verse(String),
}

impl DocumentContent {
    pub fn from_visible(kind: DocumentKind, bytes: Vec<u8>) -> Result<Self, DocumentError> {
        let text = String::from_utf8(bytes)?;
        Ok(match kind {
            DocumentKind::Hybrid => Self::Hybrid(vec![HybridBlock {
                kind: HybridBlockKind::Prose,
                text,
            }]),
            DocumentKind::Prose => Self::Prose(text),
            DocumentKind::Verse => Self::Verse(text),
        })
    }

    pub const fn kind(&self) -> DocumentKind {
        match self {
            Self::Hybrid(_) => DocumentKind::Hybrid,
            Self::Prose(_) => DocumentKind::Prose,
            Self::Verse(_) => DocumentKind::Verse,
        }
    }

    /// The same logical content aggregate used for chat transcripts. Layout is
    /// a projection; changing it never normalizes the author's stored bytes.
    pub fn document(&self) -> Result<Document<'_, usize, ()>, DocumentError> {
        let mut document = Document::new();
        match self {
            Self::Prose(text) => document.push_text(0, text, PartKind::Prose, ())?,
            Self::Verse(text) => document.push_text(0, text, PartKind::Verse, ())?,
            Self::Hybrid(blocks) => {
                for (index, block) in blocks.iter().enumerate() {
                    let kind = match block.kind {
                        HybridBlockKind::Prose => PartKind::Prose,
                        HybridBlockKind::Verse => PartKind::Verse,
                    };
                    document.push_text(index, &block.text, kind, ())?;
                }
            }
        }
        Ok(document)
    }

    pub fn project_visible(&self) -> Result<VisibleProjection, DocumentError> {
        let document = self.document()?;
        let mut offset = 0;
        let hybrid_blocks = if matches!(self, Self::Hybrid(_)) {
            document
                .parts()
                .iter()
                .map(|part| {
                    let start = offset;
                    offset += part.text().len() as u64;
                    HybridBlockProjection {
                        kind: match part.kind() {
                            PartKind::Verse => HybridBlockKind::Verse,
                            _ => HybridBlockKind::Prose,
                        },
                        byte_range: ByteRange { start, end: offset },
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(VisibleProjection {
            bytes: document.text().into_bytes(),
            hybrid_blocks,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HybridBlockKind {
    Prose,
    Verse,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HybridBlock {
    pub kind: HybridBlockKind,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisibleProjection {
    pub bytes: Vec<u8>,
    pub hybrid_blocks: Vec<HybridBlockProjection>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HybridBlockProjection {
    pub kind: HybridBlockKind,
    pub byte_range: ByteRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactSlice {
    pub artifact_id: ArtifactId,
    pub range: ByteRange,
}

pub fn project_artifact_slices<'a, F>(
    slices: &[ArtifactSlice],
    mut resolve: F,
) -> Result<Vec<u8>, DocumentError>
where
    F: FnMut(ArtifactId) -> Result<&'a [u8], DocumentError>,
{
    let mut document = Document::new();
    for slice in slices {
        let bytes = resolve(slice.artifact_id)?;
        document
            .push_slice(
                slice.artifact_id,
                bytes,
                slice.range.start..slice.range.end,
                PartKind::Prose,
                (),
            )
            .map_err(|error| match error {
                workspace_document::DocumentError::InvalidRange => {
                    DocumentError::RangeOutsideArtifact {
                        artifact_id: slice.artifact_id,
                        range: slice.range,
                        byte_len: bytes.len(),
                    }
                }
                workspace_document::DocumentError::InvalidUtf8 => DocumentError::RangeSplitsUtf8 {
                    artifact_id: slice.artifact_id,
                    range: slice.range,
                },
                error => DocumentError::Content(error),
            })?;
    }
    Ok(document.text().into_bytes())
}

#[derive(Debug, Error)]
pub enum DocumentError {
    #[error(transparent)]
    Content(#[from] workspace_document::DocumentError),
    #[error("document is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    #[error("document is too large to represent with 64-bit byte ranges")]
    RangeTooLarge,
    #[error("range {range:?} is outside artifact {artifact_id} with {byte_len} bytes")]
    RangeOutsideArtifact {
        artifact_id: ArtifactId,
        range: ByteRange,
        byte_len: usize,
    },
    #[error("range {range:?} splits a UTF-8 code point in artifact {artifact_id}")]
    RangeSplitsUtf8 {
        artifact_id: ArtifactId,
        range: ByteRange,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_projection_preserves_authored_utf8_and_line_endings() {
        let original = "# café\r\n\r\none\rtwo\n\u{301}\t";
        let content =
            DocumentContent::from_visible(DocumentKind::Prose, original.as_bytes().to_vec())
                .expect("valid prose");
        assert_eq!(content, DocumentContent::Prose(original.into()));
        assert_eq!(
            content.project_visible().expect("project prose").bytes,
            original.as_bytes()
        );
    }

    #[test]
    fn verse_projection_preserves_every_byte() {
        let original = "  first\r\n\r\nsecond  \n\u{301}\t";
        let content =
            DocumentContent::from_visible(DocumentKind::Verse, original.as_bytes().to_vec())
                .expect("valid verse");
        assert_eq!(
            content.project_visible().expect("project verse").bytes,
            original.as_bytes()
        );
    }

    #[test]
    fn slice_projection_rejects_half_a_unicode_scalar() {
        let id = ArtifactId::new();
        let bytes = "aéz".as_bytes();
        let error = project_artifact_slices(
            &[ArtifactSlice {
                artifact_id: id,
                range: ByteRange { start: 1, end: 2 },
            }],
            |_| Ok(bytes),
        )
        .expect_err("range should split UTF-8");
        assert!(matches!(error, DocumentError::RangeSplitsUtf8 { .. }));
    }

    #[test]
    fn hybrid_projection_records_exact_block_ranges() {
        let content = DocumentContent::Hybrid(vec![
            HybridBlock {
                kind: HybridBlockKind::Prose,
                text: "a\r\n".into(),
            },
            HybridBlock {
                kind: HybridBlockKind::Verse,
                text: "  b".into(),
            },
        ]);
        let projection = content.project_visible().expect("project hybrid");
        assert_eq!(projection.bytes, b"a\r\n  b");
        assert_eq!(
            projection.hybrid_blocks[0].byte_range,
            ByteRange { start: 0, end: 3 }
        );
        assert_eq!(
            projection.hybrid_blocks[1].byte_range,
            ByteRange { start: 3, end: 6 }
        );
    }
}
