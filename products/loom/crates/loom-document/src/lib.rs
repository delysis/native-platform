#![forbid(unsafe_code)]

use loom_types::{ArtifactId, ByteRange, DocumentKind};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod merge;

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
                text: canonicalize_prose(&text),
            }]),
            DocumentKind::Prose => Self::Prose(canonicalize_prose(&text)),
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

    pub fn project_visible(&self) -> Result<VisibleProjection, DocumentError> {
        match self {
            Self::Prose(markdown) => Ok(VisibleProjection {
                bytes: canonicalize_prose(markdown).into_bytes(),
                hybrid_blocks: Vec::new(),
            }),
            Self::Verse(text) => Ok(VisibleProjection {
                bytes: text.as_bytes().to_vec(),
                hybrid_blocks: Vec::new(),
            }),
            Self::Hybrid(blocks) => {
                let mut visible = String::new();
                let mut metadata = Vec::with_capacity(blocks.len());
                for block in blocks {
                    let start = visible.len();
                    match block.kind {
                        HybridBlockKind::Prose => {
                            visible.push_str(&canonicalize_prose(&block.text));
                        }
                        HybridBlockKind::Verse => visible.push_str(&block.text),
                    }
                    let end = visible.len();
                    metadata.push(HybridBlockProjection {
                        kind: block.kind,
                        byte_range: ByteRange {
                            start: usize_to_u64(start)?,
                            end: usize_to_u64(end)?,
                        },
                    });
                }
                Ok(VisibleProjection {
                    bytes: visible.into_bytes(),
                    hybrid_blocks: metadata,
                })
            }
        }
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
    let mut projected = Vec::new();
    for slice in slices {
        let bytes = resolve(slice.artifact_id)?;
        let start = usize::try_from(slice.range.start).map_err(|_| DocumentError::RangeTooLarge)?;
        let end = usize::try_from(slice.range.end).map_err(|_| DocumentError::RangeTooLarge)?;
        let selected = bytes
            .get(start..end)
            .ok_or(DocumentError::RangeOutsideArtifact {
                artifact_id: slice.artifact_id,
                range: slice.range,
                byte_len: bytes.len(),
            })?;
        if std::str::from_utf8(selected).is_err() {
            return Err(DocumentError::RangeSplitsUtf8 {
                artifact_id: slice.artifact_id,
                range: slice.range,
            });
        }
        projected.extend_from_slice(selected);
    }
    Ok(projected)
}

pub fn canonicalize_prose(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }

    let mut canonical = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            canonical.push('\n');
        } else {
            canonical.push(character);
        }
    }
    canonical
}

fn usize_to_u64(value: usize) -> Result<u64, DocumentError> {
    u64::try_from(value).map_err(|_| DocumentError::RangeTooLarge)
}

#[derive(Debug, Error)]
pub enum DocumentError {
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
    fn prose_projection_canonicalizes_line_endings() {
        let content = DocumentContent::Prose("one\r\ntwo\rthree\n".into());
        assert_eq!(
            content.project_visible().expect("project prose").bytes,
            b"one\ntwo\nthree\n"
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
        assert_eq!(projection.bytes, b"a\n  b");
        assert_eq!(
            projection.hybrid_blocks[0].byte_range,
            ByteRange { start: 0, end: 2 }
        );
        assert_eq!(
            projection.hybrid_blocks[1].byte_range,
            ByteRange { start: 2, end: 5 }
        );
    }
}
