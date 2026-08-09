use std::collections::{BTreeMap, BTreeSet};

use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::retrieval::{ExactExcerpt, ExactExcerptIdentity, UnitScore};
use crate::text::{
    TextLimitError, contains_word_sequence, intersection_and_union, normalize_words, shingle_hashes,
};

pub const MAX_ANTI_COPY_CANDIDATE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ANTI_COPY_PROMPT_EXCERPTS: usize = 4_096;
pub const MAX_ANTI_COPY_PROMPT_BYTES: usize = 64 * 1024 * 1024;
pub const MIN_EXACT_SHINGLE_WORDS: usize = 4;
pub const MAX_EXACT_SHINGLE_WORDS: usize = 64;
pub const DEFAULT_EXACT_SHINGLE_WORDS: usize = 12;
pub const MIN_FUZZY_SHINGLE_WORDS: usize = 2;
pub const MAX_FUZZY_SHINGLE_WORDS: usize = 16;
pub const DEFAULT_FUZZY_SHINGLE_WORDS: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct AntiCopyConfig {
    exact_shingle_words: usize,
    fuzzy_shingle_words: usize,
    fuzzy_threshold: UnitScore,
    semantic_threshold: UnitScore,
}

impl AntiCopyConfig {
    pub const fn new(
        exact_shingle_words: usize,
        fuzzy_shingle_words: usize,
        fuzzy_threshold: UnitScore,
        semantic_threshold: UnitScore,
    ) -> Result<Self, AntiCopyError> {
        if exact_shingle_words < MIN_EXACT_SHINGLE_WORDS
            || exact_shingle_words > MAX_EXACT_SHINGLE_WORDS
        {
            return Err(AntiCopyError::InvalidExactShingleWidth {
                shingle_words: exact_shingle_words,
            });
        }
        if fuzzy_shingle_words < MIN_FUZZY_SHINGLE_WORDS
            || fuzzy_shingle_words > MAX_FUZZY_SHINGLE_WORDS
        {
            return Err(AntiCopyError::InvalidFuzzyShingleWidth {
                shingle_words: fuzzy_shingle_words,
            });
        }
        Ok(Self {
            exact_shingle_words,
            fuzzy_shingle_words,
            fuzzy_threshold,
            semantic_threshold,
        })
    }

    pub const fn exact_shingle_words(self) -> usize {
        self.exact_shingle_words
    }

    pub const fn fuzzy_shingle_words(self) -> usize {
        self.fuzzy_shingle_words
    }

    pub const fn fuzzy_threshold(self) -> UnitScore {
        self.fuzzy_threshold
    }

    pub const fn semantic_threshold(self) -> UnitScore {
        self.semantic_threshold
    }
}

impl Default for AntiCopyConfig {
    fn default() -> Self {
        Self {
            exact_shingle_words: DEFAULT_EXACT_SHINGLE_WORDS,
            fuzzy_shingle_words: DEFAULT_FUZZY_SHINGLE_WORDS,
            fuzzy_threshold: UnitScore::from_validated(600_000),
            semantic_threshold: UnitScore::from_validated(850_000),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExternalSemanticSimilarity {
    pub excerpt: ExactExcerptIdentity,
    pub evidence_artifact_id: ArtifactId,
    pub similarity: UnitScore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExactCopySignals {
    pub raw_entire_excerpt_substring: bool,
    pub normalized_entire_excerpt_word_sequence: bool,
    pub exact_shingle_words: usize,
    pub shared_exact_contiguous_shingles: usize,
    pub exact_contiguous_shingle_match: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FuzzyCopySignals {
    pub fuzzy_shingle_words: usize,
    pub candidate_unique_shingles: usize,
    pub excerpt_unique_shingles: usize,
    pub shared_unique_shingles: usize,
    pub normalized_shingle_jaccard: Option<UnitScore>,
    pub excerpt_shingle_containment: Option<UnitScore>,
    pub threshold_exceeded: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AntiCopyEvidence {
    pub excerpt: ExactExcerptIdentity,
    pub exact: ExactCopySignals,
    pub fuzzy: FuzzyCopySignals,
    /// Never synthesized. `None` means no external semantic scorer supplied a
    /// value for this exact excerpt occurrence.
    pub external_semantic_similarity: Option<UnitScore>,
    pub external_semantic_evidence_artifact_id: Option<ArtifactId>,
    pub semantic_threshold_exceeded: bool,
    pub suspect: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AntiCopyReport {
    pub candidate_hash: BlobId,
    /// One item per exact prompt excerpt, in prompt order.
    pub evidence: Vec<AntiCopyEvidence>,
    pub any_suspect: bool,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AntiCopyError {
    #[error("candidate must contain 1 to {MAX_ANTI_COPY_CANDIDATE_BYTES} UTF-8 bytes")]
    InvalidCandidateSize,
    #[error("prompt excerpt count exceeds {MAX_ANTI_COPY_PROMPT_EXCERPTS}")]
    TooManyPromptExcerpts,
    #[error("prompt excerpts exceed the {MAX_ANTI_COPY_PROMPT_BYTES}-byte limit")]
    PromptExcerptsTooLarge,
    #[error(
        "exact shingle width {shingle_words} is outside {MIN_EXACT_SHINGLE_WORDS}..={MAX_EXACT_SHINGLE_WORDS}"
    )]
    InvalidExactShingleWidth { shingle_words: usize },
    #[error(
        "fuzzy shingle width {shingle_words} is outside {MIN_FUZZY_SHINGLE_WORDS}..={MAX_FUZZY_SHINGLE_WORDS}"
    )]
    InvalidFuzzyShingleWidth { shingle_words: usize },
    #[error("duplicate exact prompt excerpt identity")]
    DuplicatePromptExcerpt,
    #[error("duplicate external semantic score for one exact excerpt")]
    DuplicateSemanticScore,
    #[error("external semantic score count exceeds the exact prompt excerpt count")]
    TooManySemanticScores,
    #[error("external semantic score does not bind an exact prompt excerpt")]
    UnknownSemanticExcerpt,
    #[error("normalized text exceeds a bounded analysis limit")]
    TextAnalysisLimit,
    #[error("anti-copy input arithmetic overflowed")]
    InputOverflow,
}

pub fn analyze_anti_copy(
    candidate: &str,
    prompt_excerpts: &[ExactExcerpt],
    external_semantic_scores: &[ExternalSemanticSimilarity],
    config: AntiCopyConfig,
) -> Result<AntiCopyReport, AntiCopyError> {
    validate_inputs(candidate, prompt_excerpts, config)?;
    let prompt_identities = prompt_excerpts
        .iter()
        .map(ExactExcerpt::identity)
        .collect::<BTreeSet<_>>();
    let semantic = semantic_scores(external_semantic_scores, &prompt_identities)?;
    let candidate_words = normalize_words(candidate).map_err(map_text_limit)?;
    let candidate_exact_shingles =
        shingle_hashes(&candidate_words, config.exact_shingle_words).map_err(map_text_limit)?;
    let candidate_fuzzy_shingles =
        shingle_hashes(&candidate_words, config.fuzzy_shingle_words).map_err(map_text_limit)?;

    let mut evidence = Vec::with_capacity(prompt_excerpts.len());
    let mut any_suspect = false;
    for excerpt in prompt_excerpts {
        let excerpt_words = normalize_words(excerpt.text()).map_err(map_text_limit)?;
        let excerpt_exact_shingles =
            shingle_hashes(&excerpt_words, config.exact_shingle_words).map_err(map_text_limit)?;
        let (shared_exact, _) =
            intersection_and_union(&candidate_exact_shingles, &excerpt_exact_shingles);
        let excerpt_fuzzy_shingles =
            shingle_hashes(&excerpt_words, config.fuzzy_shingle_words).map_err(map_text_limit)?;
        let (shared_fuzzy, fuzzy_union) =
            intersection_and_union(&candidate_fuzzy_shingles, &excerpt_fuzzy_shingles);
        let jaccard = UnitScore::from_ratio(shared_fuzzy, fuzzy_union);
        let containment = UnitScore::from_ratio(shared_fuzzy, excerpt_fuzzy_shingles.len());
        let threshold_exceeded = [jaccard, containment]
            .into_iter()
            .flatten()
            .max()
            .is_some_and(|score| score >= config.fuzzy_threshold);
        let exact = ExactCopySignals {
            raw_entire_excerpt_substring: candidate.contains(excerpt.text()),
            normalized_entire_excerpt_word_sequence: contains_word_sequence(
                &candidate_words,
                &excerpt_words,
            ),
            exact_shingle_words: config.exact_shingle_words,
            shared_exact_contiguous_shingles: shared_exact,
            exact_contiguous_shingle_match: shared_exact > 0,
        };
        let semantic_evidence = semantic.get(&excerpt.identity()).copied();
        let semantic_score = semantic_evidence.map(|evidence| evidence.similarity);
        let semantic_threshold_exceeded =
            semantic_score.is_some_and(|score| score >= config.semantic_threshold);
        let suspect = exact.raw_entire_excerpt_substring
            || exact.normalized_entire_excerpt_word_sequence
            || exact.exact_contiguous_shingle_match
            || threshold_exceeded
            || semantic_threshold_exceeded;
        any_suspect |= suspect;
        evidence.push(AntiCopyEvidence {
            excerpt: excerpt.identity(),
            exact,
            fuzzy: FuzzyCopySignals {
                fuzzy_shingle_words: config.fuzzy_shingle_words,
                candidate_unique_shingles: candidate_fuzzy_shingles.len(),
                excerpt_unique_shingles: excerpt_fuzzy_shingles.len(),
                shared_unique_shingles: shared_fuzzy,
                normalized_shingle_jaccard: jaccard,
                excerpt_shingle_containment: containment,
                threshold_exceeded,
            },
            external_semantic_similarity: semantic_score,
            external_semantic_evidence_artifact_id: semantic_evidence
                .map(|evidence| evidence.evidence_artifact_id),
            semantic_threshold_exceeded,
            suspect,
        });
    }
    Ok(AntiCopyReport {
        candidate_hash: BlobId::digest(candidate.as_bytes()),
        evidence,
        any_suspect,
    })
}

fn validate_inputs(
    candidate: &str,
    prompt_excerpts: &[ExactExcerpt],
    config: AntiCopyConfig,
) -> Result<(), AntiCopyError> {
    if candidate.is_empty() || candidate.len() > MAX_ANTI_COPY_CANDIDATE_BYTES {
        return Err(AntiCopyError::InvalidCandidateSize);
    }
    if prompt_excerpts.len() > MAX_ANTI_COPY_PROMPT_EXCERPTS {
        return Err(AntiCopyError::TooManyPromptExcerpts);
    }
    if config.exact_shingle_words < MIN_EXACT_SHINGLE_WORDS
        || config.exact_shingle_words > MAX_EXACT_SHINGLE_WORDS
    {
        return Err(AntiCopyError::InvalidExactShingleWidth {
            shingle_words: config.exact_shingle_words,
        });
    }
    if config.fuzzy_shingle_words < MIN_FUZZY_SHINGLE_WORDS
        || config.fuzzy_shingle_words > MAX_FUZZY_SHINGLE_WORDS
    {
        return Err(AntiCopyError::InvalidFuzzyShingleWidth {
            shingle_words: config.fuzzy_shingle_words,
        });
    }
    let mut bytes = 0_usize;
    let mut identities = BTreeSet::new();
    for excerpt in prompt_excerpts {
        bytes = bytes
            .checked_add(excerpt.text().len())
            .ok_or(AntiCopyError::InputOverflow)?;
        if bytes > MAX_ANTI_COPY_PROMPT_BYTES {
            return Err(AntiCopyError::PromptExcerptsTooLarge);
        }
        if !identities.insert(excerpt.identity()) {
            return Err(AntiCopyError::DuplicatePromptExcerpt);
        }
    }
    Ok(())
}

fn semantic_scores(
    supplied: &[ExternalSemanticSimilarity],
    prompt_identities: &BTreeSet<ExactExcerptIdentity>,
) -> Result<BTreeMap<ExactExcerptIdentity, ExternalSemanticSimilarity>, AntiCopyError> {
    if supplied.len() > prompt_identities.len() {
        return Err(AntiCopyError::TooManySemanticScores);
    }
    let mut scores = BTreeMap::new();
    for supplied in supplied {
        if !prompt_identities.contains(&supplied.excerpt) {
            return Err(AntiCopyError::UnknownSemanticExcerpt);
        }
        if scores.insert(supplied.excerpt, *supplied).is_some() {
            return Err(AntiCopyError::DuplicateSemanticScore);
        }
    }
    Ok(scores)
}

fn map_text_limit(_: TextLimitError) -> AntiCopyError {
    AntiCopyError::TextAnalysisLimit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(byte: u8) -> BlobId {
        BlobId::from_bytes([byte; 32])
    }

    fn excerpt(id: u8, text: &str) -> ExactExcerpt {
        ExactExcerpt::new(
            ArtifactId::new(),
            blob(id),
            crate::SourceByteRange::new(0, u64::try_from(text.len()).unwrap()).unwrap(),
            text,
        )
        .unwrap()
    }

    #[test]
    fn every_prompt_excerpt_gets_inspectable_evidence_without_fake_semantics() {
        let prompts = [
            excerpt(1, "the copper bell sounded"),
            excerpt(2, "snow crossed the courtyard"),
        ];
        let report = analyze_anti_copy(
            "At noon, the copper bell sounded twice.",
            &prompts,
            &[],
            AntiCopyConfig::default(),
        )
        .unwrap();
        assert_eq!(report.evidence.len(), prompts.len());
        assert!(
            report.evidence[0]
                .exact
                .normalized_entire_excerpt_word_sequence
        );
        assert!(report.evidence[0].suspect);
        assert!(!report.evidence[1].suspect);
        assert!(
            report
                .evidence
                .iter()
                .all(|item| item.external_semantic_similarity.is_none())
        );
        assert!(
            report
                .evidence
                .iter()
                .all(|item| item.external_semantic_evidence_artifact_id.is_none())
        );
    }

    #[test]
    fn unicode_normalization_detects_composed_and_decomposed_copying() {
        let prompt = excerpt(1, "CAFÉ au lait");
        let report = analyze_anti_copy(
            "She ordered cafe\u{301} au lait.",
            &[prompt],
            &[],
            AntiCopyConfig::default(),
        )
        .unwrap();
        let evidence = report.evidence[0];
        assert!(!evidence.exact.raw_entire_excerpt_substring);
        assert!(evidence.exact.normalized_entire_excerpt_word_sequence);
        assert!(evidence.suspect);
    }

    #[test]
    fn semantic_scores_are_external_exact_occurrence_evidence_only() {
        let prompt = excerpt(1, "a river under glass");
        let other = excerpt(2, "a different source");
        let score = UnitScore::new(900_000).unwrap();
        let evidence_artifact_id = ArtifactId::new();
        let report = analyze_anti_copy(
            "unrelated candidate words",
            std::slice::from_ref(&prompt),
            &[ExternalSemanticSimilarity {
                excerpt: prompt.identity(),
                evidence_artifact_id,
                similarity: score,
            }],
            AntiCopyConfig::default(),
        )
        .unwrap();
        assert_eq!(report.evidence[0].external_semantic_similarity, Some(score));
        assert_eq!(
            report.evidence[0].external_semantic_evidence_artifact_id,
            Some(evidence_artifact_id)
        );
        assert!(report.evidence[0].semantic_threshold_exceeded);
        assert_eq!(
            analyze_anti_copy(
                "unrelated candidate words",
                &[prompt],
                &[ExternalSemanticSimilarity {
                    excerpt: other.identity(),
                    evidence_artifact_id: ArtifactId::new(),
                    similarity: score,
                }],
                AntiCopyConfig::default(),
            ),
            Err(AntiCopyError::UnknownSemanticExcerpt)
        );

        let prompts = [excerpt(3, "third prompt"), excerpt(4, "fourth prompt")];
        let duplicate = ExternalSemanticSimilarity {
            excerpt: prompts[0].identity(),
            evidence_artifact_id: ArtifactId::new(),
            similarity: score,
        };
        assert_eq!(
            analyze_anti_copy(
                "unrelated candidate words",
                &prompts,
                &[duplicate, duplicate],
                AntiCopyConfig::default(),
            ),
            Err(AntiCopyError::DuplicateSemanticScore)
        );
    }

    #[test]
    fn fuzzy_shingles_are_inspectable_and_thresholded() {
        let config =
            AntiCopyConfig::new(12, 2, UnitScore::new(400_000).unwrap(), UnitScore::ONE).unwrap();
        let report = analyze_anti_copy(
            "alpha beta gamma changed ending",
            &[excerpt(1, "alpha beta gamma delta ending")],
            &[],
            config,
        )
        .unwrap();
        let fuzzy = report.evidence[0].fuzzy;
        assert_eq!(fuzzy.fuzzy_shingle_words, 2);
        assert_eq!(fuzzy.excerpt_unique_shingles, 4);
        assert_eq!(fuzzy.shared_unique_shingles, 2);
        assert_eq!(
            fuzzy.normalized_shingle_jaccard,
            Some(UnitScore::new(333_333).unwrap())
        );
        assert_eq!(
            fuzzy.excerpt_shingle_containment,
            Some(UnitScore::new(500_000).unwrap())
        );
        assert!(fuzzy.threshold_exceeded);
        assert!(report.any_suspect);
    }

    #[test]
    fn default_exact_span_catches_twelve_copied_words_from_a_long_excerpt() {
        let source = excerpt(
            1,
            "amber birch cedar dogwood elm fir granite hemlock iron juniper kiln linden maple nettle oak pine quartz rowan spruce thistle umber violet willow xylem yarrow zinc",
        );
        let candidate = "She recalled elm fir granite hemlock iron juniper kiln linden maple nettle oak pine before the names dissolved.";
        let report =
            analyze_anti_copy(candidate, &[source], &[], AntiCopyConfig::default()).unwrap();
        let evidence = report.evidence[0];
        assert!(!evidence.exact.raw_entire_excerpt_substring);
        assert!(!evidence.exact.normalized_entire_excerpt_word_sequence);
        assert_eq!(
            evidence.exact.exact_shingle_words,
            DEFAULT_EXACT_SHINGLE_WORDS
        );
        assert!(evidence.exact.shared_exact_contiguous_shingles >= 1);
        assert!(evidence.exact.exact_contiguous_shingle_match);
        assert!(evidence.suspect);
    }

    #[test]
    fn default_does_not_flag_one_shared_ordinary_three_word_phrase() {
        let source = excerpt(
            1,
            "amber bells rang in the morning before winter crossed the valley and every shutter opened toward the pale eastern ridge",
        );
        let candidate =
            "We left quietly in the morning and never discussed the map or the weather again.";
        let report =
            analyze_anti_copy(candidate, &[source], &[], AntiCopyConfig::default()).unwrap();
        let evidence = report.evidence[0];
        assert!(!evidence.exact.normalized_entire_excerpt_word_sequence);
        assert_eq!(evidence.exact.shared_exact_contiguous_shingles, 0);
        assert_eq!(evidence.fuzzy.shared_unique_shingles, 0);
        assert!(!evidence.fuzzy.threshold_exceeded);
        assert!(!evidence.suspect);
        assert!(!report.any_suspect);
    }
}
