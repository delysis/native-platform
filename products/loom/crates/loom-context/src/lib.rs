#![forbid(unsafe_code)]

use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod anti_copy;
mod retrieval;
mod text;

pub use anti_copy::{
    AntiCopyConfig, AntiCopyError, AntiCopyEvidence, AntiCopyReport, DEFAULT_EXACT_SHINGLE_WORDS,
    DEFAULT_FUZZY_SHINGLE_WORDS, ExactCopySignals, ExternalSemanticSimilarity, FuzzyCopySignals,
    MAX_ANTI_COPY_CANDIDATE_BYTES, MAX_ANTI_COPY_PROMPT_BYTES, MAX_ANTI_COPY_PROMPT_EXCERPTS,
    MAX_EXACT_SHINGLE_WORDS, MAX_FUZZY_SHINGLE_WORDS, MIN_EXACT_SHINGLE_WORDS,
    MIN_FUZZY_SHINGLE_WORDS, analyze_anti_copy,
};
pub use retrieval::{
    DiversifiedExcerpt, DiversifiedSelection, ExactExcerpt, ExactExcerptIdentity, ExactTokenCount,
    HybridScoreEvidence, MAX_CRAFT_TAG_BYTES, MAX_CRAFT_TAGS, MAX_EXCERPT_BYTES,
    MAX_EXCERPT_TOKENS, MAX_RETRIEVAL_CANDIDATES, MAX_RETRIEVAL_INPUT_BYTES,
    MAX_RETRIEVAL_SHINGLES, MAX_SELECTED_EXCERPTS, MAX_SELECTION_BYTES, MAX_SELECTION_TOKENS,
    RankingWeights, RetrievalCandidate, RetrievalError, RetrievalQuery, SCORE_SCALE,
    SelectionBudget, SourceByteRange, UnitScore, select_diversified,
};

pub const MAX_PROMPT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PROMPT_PARTS: usize = 4_096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptPartKind {
    Demonstration,
    ManuscriptHistory,
    ManuscriptLiveBoundary,
    RetrievedExcerpt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptPart {
    pub kind: PromptPartKind,
    pub bytes: Vec<u8>,
    pub source_artifact_id: Option<ArtifactId>,
    pub source_blob_id: Option<BlobId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompletionPromptRecipe {
    pub recipe_artifact_id: ArtifactId,
    pub ordered_parts: Vec<PromptPart>,
}

impl CompletionPromptRecipe {
    pub fn assemble_exact_completion(&self) -> Result<Vec<u8>, ContextError> {
        if self.ordered_parts.is_empty() || self.ordered_parts.len() > MAX_PROMPT_PARTS {
            return Err(ContextError::InvalidPartCount {
                count: self.ordered_parts.len(),
                max: MAX_PROMPT_PARTS,
            });
        }
        if self.ordered_parts.last().map(|part| part.kind)
            != Some(PromptPartKind::ManuscriptLiveBoundary)
        {
            return Err(ContextError::LiveBoundaryMustBeLast);
        }

        let total = self.ordered_parts.iter().try_fold(0_usize, |total, part| {
            total
                .checked_add(part.bytes.len())
                .ok_or(ContextError::PromptTooLarge {
                    bytes: usize::MAX,
                    max: MAX_PROMPT_BYTES,
                })
        })?;
        if total > MAX_PROMPT_BYTES {
            return Err(ContextError::PromptTooLarge {
                bytes: total,
                max: MAX_PROMPT_BYTES,
            });
        }

        let mut assembled = Vec::with_capacity(total);
        for part in &self.ordered_parts {
            assembled.extend_from_slice(&part.bytes);
        }
        Ok(assembled)
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ContextError {
    #[error("prompt has {count} parts; expected 1 to {max}")]
    InvalidPartCount { count: usize, max: usize },
    #[error("the exact live manuscript boundary must be the final prompt part")]
    LiveBoundaryMustBeLast,
    #[error("prompt has {bytes} bytes; limit is {max}")]
    PromptTooLarge { bytes: usize, max: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(kind: PromptPartKind, text: &str) -> PromptPart {
        PromptPart {
            kind,
            bytes: text.as_bytes().to_vec(),
            source_artifact_id: None,
            source_blob_id: None,
        }
    }

    #[test]
    fn raw_completion_ends_on_exact_manuscript_bytes() {
        let recipe = CompletionPromptRecipe {
            recipe_artifact_id: ArtifactId::new(),
            ordered_parts: vec![
                part(PromptPartKind::RetrievedExcerpt, "example\n\n"),
                part(PromptPartKind::ManuscriptLiveBoundary, "She opened"),
            ],
        };
        assert_eq!(
            recipe.assemble_exact_completion().expect("assemble prompt"),
            b"example\n\nShe opened"
        );
    }

    #[test]
    fn control_bytes_after_boundary_are_rejected() {
        let recipe = CompletionPromptRecipe {
            recipe_artifact_id: ArtifactId::new(),
            ordered_parts: vec![
                part(PromptPartKind::ManuscriptLiveBoundary, "She opened"),
                part(PromptPartKind::Demonstration, "<continue>"),
            ],
        };
        assert_eq!(
            recipe.assemble_exact_completion(),
            Err(ContextError::LiveBoundaryMustBeLast)
        );
    }
}
