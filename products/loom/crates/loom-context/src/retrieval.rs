use std::cmp::Ordering;
use std::collections::BTreeSet;

use loom_types::{ArtifactId, BlobId, RevisionId};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::text::{
    TextLimitError, intersection_and_union, normalize_tag, normalize_words, shingle_hashes,
};

pub const SCORE_SCALE: u32 = 1_000_000;
pub const MAX_EXCERPT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_RETRIEVAL_CANDIDATES: usize = 4_096;
pub const MAX_RETRIEVAL_INPUT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_CRAFT_TAGS: usize = 64;
pub const MAX_CRAFT_TAG_BYTES: usize = 128;
pub const MAX_SELECTED_EXCERPTS: usize = 256;
pub const MAX_SELECTION_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_SELECTION_TOKENS: u64 = 4_000_000;
pub const MAX_EXCERPT_TOKENS: u32 = 1_000_000;
pub const MAX_RETRIEVAL_SHINGLES: usize = 500_000;

const DIVERSITY_SHINGLE_WORDS: usize = 3;
const SELECTION_FINGERPRINT_DOMAIN: &[u8] = b"loom/diversified-retrieval-selection/v1\0";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct UnitScore(u32);

impl UnitScore {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(SCORE_SCALE);

    pub const fn new(millionths: u32) -> Result<Self, RetrievalError> {
        if millionths > SCORE_SCALE {
            return Err(RetrievalError::ScoreOutOfRange { millionths });
        }
        Ok(Self(millionths))
    }

    pub const fn millionths(self) -> u32 {
        self.0
    }

    pub(crate) fn from_ratio(numerator: usize, denominator: usize) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        let numerator = u64::try_from(numerator).ok()?;
        let denominator = u64::try_from(denominator).ok()?;
        let scaled = numerator.saturating_mul(u64::from(SCORE_SCALE)) / denominator;
        u32::try_from(scaled).ok().map(Self)
    }

    pub(crate) const fn from_validated(millionths: u32) -> Self {
        Self(millionths)
    }
}

impl<'de> Deserialize<'de> for UnitScore {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let millionths = u32::deserialize(deserializer)?;
        Self::new(millionths).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceByteRange {
    start: u64,
    end_exclusive: u64,
}

impl SourceByteRange {
    pub const fn new(start: u64, end_exclusive: u64) -> Result<Self, RetrievalError> {
        if end_exclusive <= start {
            return Err(RetrievalError::InvalidByteRange {
                start,
                end_exclusive,
            });
        }
        Ok(Self {
            start,
            end_exclusive,
        })
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn end_exclusive(self) -> u64 {
        self.end_exclusive
    }

    pub const fn len(self) -> u64 {
        self.end_exclusive - self.start
    }

    pub const fn is_empty(self) -> bool {
        false
    }
}

impl<'de> Deserialize<'de> for SourceByteRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireRange {
            start: u64,
            end_exclusive: u64,
        }
        let wire = WireRange::deserialize(deserializer)?;
        Self::new(wire.start, wire.end_exclusive).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactExcerptIdentity {
    pub source_artifact_id: ArtifactId,
    pub source_revision_id: RevisionId,
    pub source_blob_id: BlobId,
    pub source_range: SourceByteRange,
    pub excerpt_hash: BlobId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExactExcerpt {
    identity: ExactExcerptIdentity,
    text: String,
}

impl ExactExcerpt {
    pub fn new(
        source_artifact_id: ArtifactId,
        source_revision_id: RevisionId,
        source_blob_id: BlobId,
        source_range: SourceByteRange,
        text: impl Into<String>,
    ) -> Result<Self, RetrievalError> {
        let text = text.into();
        validate_excerpt_text(source_range, &text)?;
        let identity = ExactExcerptIdentity {
            source_artifact_id,
            source_revision_id,
            source_blob_id,
            source_range,
            excerpt_hash: BlobId::digest(text.as_bytes()),
        };
        Ok(Self { identity, text })
    }

    pub const fn identity(&self) -> ExactExcerptIdentity {
        self.identity
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub const fn byte_len(&self) -> u64 {
        self.identity.source_range.len()
    }
}

impl<'de> Deserialize<'de> for ExactExcerpt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireExcerpt {
            identity: ExactExcerptIdentity,
            text: String,
        }
        let wire = WireExcerpt::deserialize(deserializer)?;
        let excerpt = Self::new(
            wire.identity.source_artifact_id,
            wire.identity.source_revision_id,
            wire.identity.source_blob_id,
            wire.identity.source_range,
            wire.text,
        )
        .map_err(serde::de::Error::custom)?;
        if excerpt.identity.excerpt_hash != wire.identity.excerpt_hash {
            return Err(serde::de::Error::custom(
                RetrievalError::ExcerptHashMismatch,
            ));
        }
        Ok(excerpt)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ExactTokenCount {
    tokenizer_fingerprint: BlobId,
    excerpt_hash: BlobId,
    tokens: u32,
}

impl ExactTokenCount {
    pub const fn new(
        tokenizer_fingerprint: BlobId,
        excerpt_hash: BlobId,
        tokens: u32,
    ) -> Result<Self, RetrievalError> {
        if tokens == 0 || tokens > MAX_EXCERPT_TOKENS {
            return Err(RetrievalError::InvalidTokenCount { tokens });
        }
        Ok(Self {
            tokenizer_fingerprint,
            excerpt_hash,
            tokens,
        })
    }

    pub const fn tokenizer_fingerprint(self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn tokens(self) -> u32 {
        self.tokens
    }

    pub const fn excerpt_hash(self) -> BlobId {
        self.excerpt_hash
    }
}

impl<'de> Deserialize<'de> for ExactTokenCount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireTokenCount {
            tokenizer_fingerprint: BlobId,
            excerpt_hash: BlobId,
            tokens: u32,
        }
        let wire = WireTokenCount::deserialize(deserializer)?;
        Self::new(wire.tokenizer_fingerprint, wire.excerpt_hash, wire.tokens)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RetrievalCandidate {
    excerpt: ExactExcerpt,
    exact_token_count: ExactTokenCount,
    lexical_score: Option<UnitScore>,
    embedding_score: Option<UnitScore>,
    craft_tags: Vec<String>,
}

impl RetrievalCandidate {
    pub fn new(
        excerpt: ExactExcerpt,
        exact_token_count: ExactTokenCount,
        lexical_score: Option<UnitScore>,
        embedding_score: Option<UnitScore>,
        craft_tags: impl IntoIterator<Item = String>,
    ) -> Result<Self, RetrievalError> {
        let craft_tags = canonical_tags(craft_tags)?;
        if exact_token_count.excerpt_hash != excerpt.identity.excerpt_hash {
            return Err(RetrievalError::TokenCountExcerptMismatch);
        }
        Ok(Self {
            excerpt,
            exact_token_count,
            lexical_score,
            embedding_score,
            craft_tags,
        })
    }

    pub const fn excerpt(&self) -> &ExactExcerpt {
        &self.excerpt
    }

    pub const fn exact_token_count(&self) -> ExactTokenCount {
        self.exact_token_count
    }

    pub const fn lexical_score(&self) -> Option<UnitScore> {
        self.lexical_score
    }

    pub const fn embedding_score(&self) -> Option<UnitScore> {
        self.embedding_score
    }

    pub fn craft_tags(&self) -> &[String] {
        &self.craft_tags
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RankingWeights {
    lexical: UnitScore,
    embedding: UnitScore,
    craft_tags: UnitScore,
    diversity_penalty: UnitScore,
}

impl RankingWeights {
    pub const fn new(
        lexical: UnitScore,
        embedding: UnitScore,
        craft_tags: UnitScore,
        diversity_penalty: UnitScore,
    ) -> Result<Self, RetrievalError> {
        if lexical.millionths() == 0 && embedding.millionths() == 0 && craft_tags.millionths() == 0
        {
            return Err(RetrievalError::ZeroRelevanceWeights);
        }
        Ok(Self {
            lexical,
            embedding,
            craft_tags,
            diversity_penalty,
        })
    }

    pub const fn lexical(self) -> UnitScore {
        self.lexical
    }

    pub const fn embedding(self) -> UnitScore {
        self.embedding
    }

    pub const fn craft_tags(self) -> UnitScore {
        self.craft_tags
    }

    pub const fn diversity_penalty(self) -> UnitScore {
        self.diversity_penalty
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RetrievalQuery {
    craft_tags: Vec<String>,
    weights: RankingWeights,
}

impl RetrievalQuery {
    pub fn new(
        craft_tags: impl IntoIterator<Item = String>,
        weights: RankingWeights,
    ) -> Result<Self, RetrievalError> {
        Ok(Self {
            craft_tags: canonical_tags(craft_tags)?,
            weights,
        })
    }

    pub fn craft_tags(&self) -> &[String] {
        &self.craft_tags
    }

    pub const fn weights(&self) -> RankingWeights {
        self.weights
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SelectionBudget {
    tokenizer_fingerprint: BlobId,
    max_tokens: u64,
    max_bytes: usize,
    max_excerpts: usize,
}

impl SelectionBudget {
    pub const fn new(
        tokenizer_fingerprint: BlobId,
        max_tokens: u64,
        max_bytes: usize,
        max_excerpts: usize,
    ) -> Result<Self, RetrievalError> {
        if max_tokens == 0 || max_tokens > MAX_SELECTION_TOKENS {
            return Err(RetrievalError::InvalidTokenBudget { max_tokens });
        }
        if max_bytes == 0 || max_bytes > MAX_SELECTION_BYTES {
            return Err(RetrievalError::InvalidByteBudget { max_bytes });
        }
        if max_excerpts == 0 || max_excerpts > MAX_SELECTED_EXCERPTS {
            return Err(RetrievalError::InvalidCountBudget { max_excerpts });
        }
        Ok(Self {
            tokenizer_fingerprint,
            max_tokens,
            max_bytes,
            max_excerpts,
        })
    }

    pub const fn tokenizer_fingerprint(self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn max_tokens(self) -> u64 {
        self.max_tokens
    }

    pub const fn max_bytes(self) -> usize {
        self.max_bytes
    }

    pub const fn max_excerpts(self) -> usize {
        self.max_excerpts
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HybridScoreEvidence {
    pub lexical: Option<UnitScore>,
    pub embedding: Option<UnitScore>,
    pub craft_tags: Option<UnitScore>,
    /// Sum of weights for the `Some` signals. Missing signals are excluded,
    /// never replaced with zero-valued evidence.
    pub available_weight_millionths: u32,
    pub relevance: UnitScore,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiversifiedExcerpt {
    candidate: RetrievalCandidate,
    score_evidence: HybridScoreEvidence,
    maximum_selected_similarity: Option<UnitScore>,
    mmr_millionths: i64,
    /// Zero is the first (strongest) MMR choice. Prompt order is reversed, so
    /// this strongest choice is nearest the live manuscript boundary.
    selection_rank: usize,
}

impl DiversifiedExcerpt {
    pub const fn candidate(&self) -> &RetrievalCandidate {
        &self.candidate
    }

    pub const fn score_evidence(&self) -> HybridScoreEvidence {
        self.score_evidence
    }

    pub const fn maximum_selected_similarity(&self) -> Option<UnitScore> {
        self.maximum_selected_similarity
    }

    pub const fn mmr_millionths(&self) -> i64 {
        self.mmr_millionths
    }

    pub const fn selection_rank(&self) -> usize {
        self.selection_rank
    }
}

/// Replayable result of the deterministic selector.
///
/// This is intentionally not deserializable and all fields are private. A
/// stored score vector or reordered excerpt list cannot recreate a live
/// selector result; persistence is diagnostic and selection must rerun from
/// its bounded candidate pool.
///
/// ```compile_fail
/// use loom_context::DiversifiedSelection;
/// fn requires_deserialize<T: serde::de::DeserializeOwned>() {}
/// requires_deserialize::<DiversifiedSelection>();
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiversifiedSelection {
    /// Prompt order: weaker selections first, strongest selection last.
    prompt_order: Vec<DiversifiedExcerpt>,
    total_tokens: u64,
    total_bytes: usize,
    query: RetrievalQuery,
    budget: SelectionBudget,
    selection_fingerprint: BlobId,
}

impl DiversifiedSelection {
    pub fn prompt_order(&self) -> &[DiversifiedExcerpt] {
        &self.prompt_order
    }

    pub const fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    pub const fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub const fn query(&self) -> &RetrievalQuery {
        &self.query
    }

    pub const fn budget(&self) -> SelectionBudget {
        self.budget
    }

    pub const fn selection_fingerprint(&self) -> BlobId {
        self.selection_fingerprint
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RetrievalError {
    #[error("score {millionths} exceeds {SCORE_SCALE} millionths")]
    ScoreOutOfRange { millionths: u32 },
    #[error("source byte range {start}..{end_exclusive} is empty or reversed")]
    InvalidByteRange { start: u64, end_exclusive: u64 },
    #[error("excerpt must contain 1 to {MAX_EXCERPT_BYTES} UTF-8 bytes")]
    InvalidExcerptSize,
    #[error("excerpt byte length does not equal its source byte range")]
    ExcerptRangeLengthMismatch,
    #[error("serialized excerpt hash does not match its exact UTF-8 bytes")]
    ExcerptHashMismatch,
    #[error("exact token count {tokens} is outside the supported range")]
    InvalidTokenCount { tokens: u32 },
    #[error("exact token count is bound to a different excerpt hash")]
    TokenCountExcerptMismatch,
    #[error("craft tag count exceeds {MAX_CRAFT_TAGS}")]
    TooManyCraftTags,
    #[error("craft tag is empty after normalization")]
    EmptyCraftTag,
    #[error("craft tag exceeds {MAX_CRAFT_TAG_BYTES} normalized UTF-8 bytes")]
    CraftTagTooLong,
    #[error("at least one relevance weight must be positive")]
    ZeroRelevanceWeights,
    #[error("token budget {max_tokens} is outside the supported range")]
    InvalidTokenBudget { max_tokens: u64 },
    #[error("byte budget {max_bytes} is outside the supported range")]
    InvalidByteBudget { max_bytes: usize },
    #[error("excerpt-count budget {max_excerpts} is outside the supported range")]
    InvalidCountBudget { max_excerpts: usize },
    #[error("candidate count {count} exceeds {MAX_RETRIEVAL_CANDIDATES}")]
    TooManyCandidates { count: usize },
    #[error("candidate excerpts exceed the {MAX_RETRIEVAL_INPUT_BYTES}-byte input limit")]
    CandidateInputTooLarge,
    #[error("candidate text analysis exceeds the {MAX_RETRIEVAL_SHINGLES}-shingle limit")]
    CandidateAnalysisTooLarge,
    #[error("duplicate exact excerpt identity in retrieval candidates")]
    DuplicateExcerpt,
    #[error("candidate token count uses a different tokenizer fingerprint")]
    TokenizerMismatch,
    #[error("candidate has no supplied ranking evidence with a positive weight")]
    MissingRankingEvidence,
    #[error("normalized text exceeds a bounded analysis limit")]
    TextAnalysisLimit,
    #[error("selection budget arithmetic overflowed")]
    BudgetOverflow,
    #[error("ranking weight arithmetic overflowed")]
    RankingWeightOverflow,
}

#[derive(Debug)]
struct PreparedCandidate {
    index: usize,
    identity: ExactExcerptIdentity,
    score_evidence: HybridScoreEvidence,
    shingles: BTreeSet<BlobId>,
}

#[derive(Clone, Copy, Debug)]
struct ChosenCandidate {
    prepared_index: usize,
    similarity: Option<UnitScore>,
    mmr_millionths: i64,
}

pub fn select_diversified(
    candidates: &[RetrievalCandidate],
    query: &RetrievalQuery,
    budget: SelectionBudget,
) -> Result<DiversifiedSelection, RetrievalError> {
    validate_candidates(candidates, budget)?;
    let mut prepared = Vec::with_capacity(candidates.len());
    let mut total_shingles = 0_usize;
    for (index, candidate) in candidates.iter().enumerate() {
        let candidate = prepare_candidate(index, candidate, query)?;
        total_shingles = total_shingles
            .checked_add(candidate.shingles.len())
            .ok_or(RetrievalError::CandidateAnalysisTooLarge)?;
        if total_shingles > MAX_RETRIEVAL_SHINGLES {
            return Err(RetrievalError::CandidateAnalysisTooLarge);
        }
        prepared.push(candidate);
    }

    let mut remaining = (0..prepared.len()).collect::<BTreeSet<_>>();
    let mut chosen = Vec::new();
    let mut total_tokens = 0_u64;
    let mut total_bytes = 0_usize;
    while chosen.len() < budget.max_excerpts {
        let next = best_fitting_candidate(
            &remaining,
            &prepared,
            candidates,
            &chosen,
            query.weights,
            budget,
            total_tokens,
            total_bytes,
        )?;
        let Some(next) = next else { break };
        let candidate = &candidates[prepared[next.prepared_index].index];
        total_tokens = total_tokens
            .checked_add(u64::from(candidate.exact_token_count.tokens))
            .ok_or(RetrievalError::BudgetOverflow)?;
        total_bytes = total_bytes
            .checked_add(candidate.excerpt.text.len())
            .ok_or(RetrievalError::BudgetOverflow)?;
        remaining.remove(&next.prepared_index);
        chosen.push(next);
    }

    let mut prompt_order = chosen
        .iter()
        .enumerate()
        .map(|(selection_rank, chosen)| {
            let prepared = &prepared[chosen.prepared_index];
            DiversifiedExcerpt {
                candidate: candidates[prepared.index].clone(),
                score_evidence: prepared.score_evidence,
                maximum_selected_similarity: chosen.similarity,
                mmr_millionths: chosen.mmr_millionths,
                selection_rank,
            }
        })
        .collect::<Vec<_>>();
    prompt_order.reverse();
    let selection_fingerprint = fingerprint_selection(
        candidates,
        query,
        budget,
        &prompt_order,
        total_tokens,
        total_bytes,
    );
    Ok(DiversifiedSelection {
        prompt_order,
        total_tokens,
        total_bytes,
        query: query.clone(),
        budget,
        selection_fingerprint,
    })
}

fn fingerprint_selection(
    candidates: &[RetrievalCandidate],
    query: &RetrievalQuery,
    budget: SelectionBudget,
    prompt_order: &[DiversifiedExcerpt],
    total_tokens: u64,
    total_bytes: usize,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(SELECTION_FINGERPRINT_DOMAIN);
    digest.update((query.craft_tags.len() as u64).to_be_bytes());
    for tag in &query.craft_tags {
        digest.update((tag.len() as u64).to_be_bytes());
        digest.update(tag.as_bytes());
    }
    for score in [
        query.weights.lexical,
        query.weights.embedding,
        query.weights.craft_tags,
        query.weights.diversity_penalty,
    ] {
        digest.update(score.millionths().to_be_bytes());
    }
    digest.update(budget.tokenizer_fingerprint.as_bytes());
    digest.update(budget.max_tokens.to_be_bytes());
    digest.update((budget.max_bytes as u64).to_be_bytes());
    digest.update((budget.max_excerpts as u64).to_be_bytes());
    let mut canonical_candidates = candidates.iter().collect::<Vec<_>>();
    canonical_candidates.sort_unstable_by_key(|candidate| candidate.excerpt.identity);
    digest.update((canonical_candidates.len() as u64).to_be_bytes());
    for candidate in canonical_candidates {
        update_candidate_digest(&mut digest, candidate);
    }
    digest.update(total_tokens.to_be_bytes());
    digest.update((total_bytes as u64).to_be_bytes());
    digest.update((prompt_order.len() as u64).to_be_bytes());
    for item in prompt_order {
        digest.update(item.candidate.excerpt.identity.excerpt_hash.as_bytes());
        update_optional_score(&mut digest, item.score_evidence.lexical);
        update_optional_score(&mut digest, item.score_evidence.embedding);
        update_optional_score(&mut digest, item.score_evidence.craft_tags);
        digest.update(
            item.score_evidence
                .available_weight_millionths
                .to_be_bytes(),
        );
        digest.update(item.score_evidence.relevance.millionths().to_be_bytes());
        update_optional_score(&mut digest, item.maximum_selected_similarity);
        digest.update(item.mmr_millionths.to_be_bytes());
        digest.update((item.selection_rank as u64).to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn update_candidate_digest(digest: &mut Sha256, candidate: &RetrievalCandidate) {
    let identity = candidate.excerpt.identity;
    digest.update(identity.source_artifact_id.as_ulid().to_bytes());
    digest.update(identity.source_revision_id.as_ulid().to_bytes());
    digest.update(identity.source_blob_id.as_bytes());
    digest.update(identity.source_range.start.to_be_bytes());
    digest.update(identity.source_range.end_exclusive.to_be_bytes());
    digest.update(identity.excerpt_hash.as_bytes());
    digest.update(candidate.exact_token_count.tokenizer_fingerprint.as_bytes());
    digest.update(candidate.exact_token_count.excerpt_hash.as_bytes());
    digest.update(candidate.exact_token_count.tokens.to_be_bytes());
    update_optional_score(digest, candidate.lexical_score);
    update_optional_score(digest, candidate.embedding_score);
    digest.update((candidate.craft_tags.len() as u64).to_be_bytes());
    for tag in &candidate.craft_tags {
        digest.update((tag.len() as u64).to_be_bytes());
        digest.update(tag.as_bytes());
    }
}

fn update_optional_score(digest: &mut Sha256, score: Option<UnitScore>) {
    match score {
        Some(score) => {
            digest.update([1]);
            digest.update(score.millionths().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn validate_excerpt_text(range: SourceByteRange, text: &str) -> Result<(), RetrievalError> {
    if text.is_empty() || text.len() > MAX_EXCERPT_BYTES {
        return Err(RetrievalError::InvalidExcerptSize);
    }
    let bytes = u64::try_from(text.len()).map_err(|_| RetrievalError::InvalidExcerptSize)?;
    if range.len() != bytes {
        return Err(RetrievalError::ExcerptRangeLengthMismatch);
    }
    Ok(())
}

fn canonical_tags(tags: impl IntoIterator<Item = String>) -> Result<Vec<String>, RetrievalError> {
    let mut canonical = BTreeSet::new();
    for (index, tag) in tags.into_iter().enumerate() {
        if index == MAX_CRAFT_TAGS {
            return Err(RetrievalError::TooManyCraftTags);
        }
        let tag = normalize_tag(&tag, MAX_CRAFT_TAG_BYTES).map_err(|error| match error {
            TextLimitError::NormalizedBytes => RetrievalError::CraftTagTooLong,
            TextLimitError::Words | TextLimitError::Shingles => RetrievalError::TextAnalysisLimit,
        })?;
        if tag.is_empty() {
            return Err(RetrievalError::EmptyCraftTag);
        }
        canonical.insert(tag);
    }
    Ok(canonical.into_iter().collect())
}

fn validate_candidates(
    candidates: &[RetrievalCandidate],
    budget: SelectionBudget,
) -> Result<(), RetrievalError> {
    if candidates.len() > MAX_RETRIEVAL_CANDIDATES {
        return Err(RetrievalError::TooManyCandidates {
            count: candidates.len(),
        });
    }
    let mut total_bytes = 0_usize;
    let mut identities = BTreeSet::new();
    for candidate in candidates {
        total_bytes = total_bytes
            .checked_add(candidate.excerpt.text.len())
            .ok_or(RetrievalError::CandidateInputTooLarge)?;
        if total_bytes > MAX_RETRIEVAL_INPUT_BYTES {
            return Err(RetrievalError::CandidateInputTooLarge);
        }
        if !identities.insert(candidate.excerpt.identity) {
            return Err(RetrievalError::DuplicateExcerpt);
        }
        if candidate.exact_token_count.tokenizer_fingerprint != budget.tokenizer_fingerprint {
            return Err(RetrievalError::TokenizerMismatch);
        }
    }
    Ok(())
}

fn prepare_candidate(
    index: usize,
    candidate: &RetrievalCandidate,
    query: &RetrievalQuery,
) -> Result<PreparedCandidate, RetrievalError> {
    let craft_tags = tag_overlap(&query.craft_tags, &candidate.craft_tags);
    let (relevance, available_weight_millionths) = weighted_relevance(
        candidate.lexical_score,
        candidate.embedding_score,
        craft_tags,
        query.weights,
    )?;
    let evidence = HybridScoreEvidence {
        lexical: candidate.lexical_score,
        embedding: candidate.embedding_score,
        craft_tags,
        available_weight_millionths,
        relevance,
    };
    let words = normalize_words(&candidate.excerpt.text).map_err(map_text_limit)?;
    let shingles = shingle_hashes(&words, DIVERSITY_SHINGLE_WORDS).map_err(map_text_limit)?;
    Ok(PreparedCandidate {
        index,
        identity: candidate.excerpt.identity,
        score_evidence: evidence,
        shingles,
    })
}

fn tag_overlap(query: &[String], candidate: &[String]) -> Option<UnitScore> {
    if query.is_empty() {
        return None;
    }
    let query = query.iter().collect::<BTreeSet<_>>();
    let candidate = candidate.iter().collect::<BTreeSet<_>>();
    UnitScore::from_ratio(query.intersection(&candidate).count(), query.len())
}

fn weighted_relevance(
    lexical: Option<UnitScore>,
    embedding: Option<UnitScore>,
    craft_tags: Option<UnitScore>,
    weights: RankingWeights,
) -> Result<(UnitScore, u32), RetrievalError> {
    let signals = [
        (lexical, weights.lexical),
        (embedding, weights.embedding),
        (craft_tags, weights.craft_tags),
    ];
    let mut weighted_sum = 0_u64;
    let mut available_weight = 0_u32;
    for (score, weight) in signals {
        if let Some(score) = score
            && weight.millionths() > 0
        {
            weighted_sum = weighted_sum
                .checked_add(u64::from(score.millionths()) * u64::from(weight.millionths()))
                .ok_or(RetrievalError::RankingWeightOverflow)?;
            available_weight = available_weight
                .checked_add(weight.millionths())
                .ok_or(RetrievalError::RankingWeightOverflow)?;
        }
    }
    if available_weight == 0 {
        return Err(RetrievalError::MissingRankingEvidence);
    }
    let relevance = u32::try_from(weighted_sum / u64::from(available_weight)).map_err(|_| {
        RetrievalError::ScoreOutOfRange {
            millionths: u32::MAX,
        }
    })?;
    Ok((UnitScore::new(relevance)?, available_weight))
}

#[allow(clippy::too_many_arguments)]
fn best_fitting_candidate(
    remaining: &BTreeSet<usize>,
    prepared: &[PreparedCandidate],
    candidates: &[RetrievalCandidate],
    chosen: &[ChosenCandidate],
    weights: RankingWeights,
    budget: SelectionBudget,
    total_tokens: u64,
    total_bytes: usize,
) -> Result<Option<ChosenCandidate>, RetrievalError> {
    let mut best = None;
    for &prepared_index in remaining {
        let candidate = &candidates[prepared[prepared_index].index];
        if !fits_budget(candidate, budget, total_tokens, total_bytes)? {
            continue;
        }
        let similarity = maximum_similarity(prepared_index, prepared, chosen);
        let penalty = similarity.map_or(0_i64, |similarity| {
            i64::from(similarity.millionths()) * i64::from(weights.diversity_penalty.millionths())
                / i64::from(SCORE_SCALE)
        });
        let proposal = ChosenCandidate {
            prepared_index,
            similarity,
            mmr_millionths: i64::from(
                prepared[prepared_index]
                    .score_evidence
                    .relevance
                    .millionths(),
            ) - penalty,
        };
        if best.is_none_or(|current| better_choice(proposal, current, prepared)) {
            best = Some(proposal);
        }
    }
    Ok(best)
}

fn fits_budget(
    candidate: &RetrievalCandidate,
    budget: SelectionBudget,
    total_tokens: u64,
    total_bytes: usize,
) -> Result<bool, RetrievalError> {
    let tokens = total_tokens
        .checked_add(u64::from(candidate.exact_token_count.tokens))
        .ok_or(RetrievalError::BudgetOverflow)?;
    let bytes = total_bytes
        .checked_add(candidate.excerpt.text.len())
        .ok_or(RetrievalError::BudgetOverflow)?;
    Ok(tokens <= budget.max_tokens && bytes <= budget.max_bytes)
}

fn maximum_similarity(
    candidate_index: usize,
    prepared: &[PreparedCandidate],
    chosen: &[ChosenCandidate],
) -> Option<UnitScore> {
    chosen
        .iter()
        .filter_map(|chosen| {
            let (intersection, union) = intersection_and_union(
                &prepared[candidate_index].shingles,
                &prepared[chosen.prepared_index].shingles,
            );
            UnitScore::from_ratio(intersection, union)
        })
        .max()
}

fn better_choice(
    proposal: ChosenCandidate,
    current: ChosenCandidate,
    prepared: &[PreparedCandidate],
) -> bool {
    proposal
        .mmr_millionths
        .cmp(&current.mmr_millionths)
        .then_with(|| {
            prepared[proposal.prepared_index]
                .score_evidence
                .relevance
                .cmp(&prepared[current.prepared_index].score_evidence.relevance)
        })
        .then_with(|| {
            prepared[current.prepared_index]
                .identity
                .cmp(&prepared[proposal.prepared_index].identity)
        })
        == Ordering::Greater
}

fn map_text_limit(_: TextLimitError) -> RetrievalError {
    RetrievalError::TextAnalysisLimit
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
            RevisionId::new(),
            blob(id),
            SourceByteRange::new(10, 10 + u64::try_from(text.len()).unwrap()).unwrap(),
            text,
        )
        .unwrap()
    }

    fn candidate(id: u8, text: &str, lexical: u32) -> RetrievalCandidate {
        let excerpt = excerpt(id, text);
        RetrievalCandidate::new(
            excerpt.clone(),
            ExactTokenCount::new(
                blob(99),
                excerpt.identity().excerpt_hash,
                u32::try_from(text.len()).unwrap(),
            )
            .unwrap(),
            Some(UnitScore::new(lexical).unwrap()),
            None,
            Vec::new(),
        )
        .unwrap()
    }

    fn query(diversity: u32) -> RetrievalQuery {
        RetrievalQuery::new(
            Vec::new(),
            RankingWeights::new(
                UnitScore::ONE,
                UnitScore::ZERO,
                UnitScore::ZERO,
                UnitScore::new(diversity).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn budget(count: usize, bytes: usize, tokens: u64) -> SelectionBudget {
        SelectionBudget::new(blob(99), tokens, bytes, count).unwrap()
    }

    #[test]
    fn exact_excerpt_binds_range_and_hash_and_rejects_tampering() {
        let value = excerpt(1, "café");
        assert_eq!(value.byte_len(), 5);
        assert_eq!(
            value.identity().excerpt_hash,
            BlobId::digest("café".as_bytes())
        );
        let mut json = serde_json::to_value(&value).unwrap();
        json["identity"]["excerpt_hash"] = serde_json::Value::String(blob(8).to_string());
        assert!(serde_json::from_value::<ExactExcerpt>(json).is_err());
        assert_eq!(
            ExactExcerpt::new(
                ArtifactId::new(),
                RevisionId::new(),
                blob(1),
                SourceByteRange::new(0, 4).unwrap(),
                "café"
            ),
            Err(RetrievalError::ExcerptRangeLengthMismatch)
        );
        assert_eq!(
            RetrievalCandidate::new(
                value,
                ExactTokenCount::new(blob(99), blob(42), 2).unwrap(),
                Some(UnitScore::ONE),
                None,
                Vec::new(),
            ),
            Err(RetrievalError::TokenCountExcerptMismatch)
        );
    }

    #[test]
    fn absent_embedding_evidence_remains_absent() {
        let selection = select_diversified(
            &[candidate(1, "one two three", 700_000)],
            &query(0),
            budget(1, 100, 100),
        )
        .unwrap();
        let evidence = selection.prompt_order()[0].score_evidence();
        assert_eq!(evidence.lexical, Some(UnitScore::new(700_000).unwrap()));
        assert_eq!(evidence.embedding, None);
        assert_eq!(evidence.craft_tags, None);
        assert_eq!(evidence.available_weight_millionths, SCORE_SCALE);
        assert_eq!(evidence.relevance, UnitScore::new(700_000).unwrap());
    }

    #[test]
    fn explicit_craft_tags_are_normalized_evidence_not_a_fabricated_model_score() {
        let excerpt = excerpt(1, "one two three");
        let candidate = RetrievalCandidate::new(
            excerpt.clone(),
            ExactTokenCount::new(blob(99), excerpt.identity().excerpt_hash, 3).unwrap(),
            None,
            None,
            vec!["  LYRICAL  ".to_string()],
        )
        .unwrap();
        let query = RetrievalQuery::new(
            vec!["lyrical".to_string(), "refrain".to_string()],
            RankingWeights::new(
                UnitScore::ZERO,
                UnitScore::ZERO,
                UnitScore::ONE,
                UnitScore::ZERO,
            )
            .unwrap(),
        )
        .unwrap();
        let selection = select_diversified(&[candidate], &query, budget(1, 100, 100)).unwrap();
        let evidence = selection.prompt_order()[0].score_evidence();
        assert_eq!(evidence.lexical, None);
        assert_eq!(evidence.embedding, None);
        assert_eq!(evidence.craft_tags, Some(UnitScore::new(500_000).unwrap()));
        assert_eq!(evidence.relevance, UnitScore::new(500_000).unwrap());
    }

    #[test]
    fn selection_is_input_order_invariant_and_strongest_is_nearest_boundary() {
        let candidates = vec![
            candidate(1, "red green blue", 900_000),
            candidate(2, "oak ash elm", 700_000),
            candidate(3, "moon tide salt", 800_000),
        ];
        let forward = select_diversified(&candidates, &query(0), budget(3, 1_000, 1_000)).unwrap();
        let reversed = select_diversified(
            &candidates.iter().cloned().rev().collect::<Vec<_>>(),
            &query(0),
            budget(3, 1_000, 1_000),
        )
        .unwrap();
        let identities = |selection: &DiversifiedSelection| {
            selection
                .prompt_order()
                .iter()
                .map(|item| item.candidate().excerpt().identity())
                .collect::<Vec<_>>()
        };
        assert_eq!(identities(&forward), identities(&reversed));
        assert_eq!(
            forward.selection_fingerprint(),
            reversed.selection_fingerprint(),
            "candidate input order is not semantic"
        );
        assert_eq!(
            forward.prompt_order().last().unwrap().selection_rank(),
            0,
            "strongest choice must be nearest the live boundary"
        );
        assert_eq!(
            forward
                .prompt_order()
                .last()
                .unwrap()
                .score_evidence()
                .relevance,
            UnitScore::new(900_000).unwrap()
        );
    }

    #[test]
    fn selection_fingerprint_commits_to_the_complete_audition_pool() {
        let chosen = candidate(1, "red green blue", 900_000);
        let first = select_diversified(
            &[chosen.clone(), candidate(2, "oak ash elm", 100_000)],
            &query(0),
            budget(1, 1_000, 1_000),
        )
        .unwrap();
        let second = select_diversified(
            &[chosen, candidate(3, "moon tide salt", 100_000)],
            &query(0),
            budget(1, 1_000, 1_000),
        )
        .unwrap();
        assert_eq!(
            first.prompt_order()[0].candidate().excerpt().text(),
            second.prompt_order()[0].candidate().excerpt().text()
        );
        assert_ne!(
            first.selection_fingerprint(),
            second.selection_fingerprint(),
            "unselected candidates are causal inputs and must remain in provenance"
        );
    }

    #[test]
    fn diversity_penalty_prefers_a_different_excerpt() {
        let candidates = vec![
            candidate(1, "one two three four five", 900_000),
            candidate(2, "one two three four six", 890_000),
            candidate(3, "cedar rain copper window", 800_000),
        ];
        let selection =
            select_diversified(&candidates, &query(SCORE_SCALE), budget(2, 1_000, 1_000)).unwrap();
        let chosen = selection
            .prompt_order()
            .iter()
            .map(|item| item.candidate().excerpt().identity().source_blob_id)
            .collect::<BTreeSet<_>>();
        assert!(chosen.contains(&blob(1)));
        assert!(chosen.contains(&blob(3)));
        assert!(!chosen.contains(&blob(2)));
    }

    #[test]
    fn every_selection_respects_all_exact_budgets() {
        let candidates = (1_u8..=12)
            .map(|id| {
                candidate(
                    id,
                    &format!("word{id} river stone"),
                    500_000 + u32::from(id),
                )
            })
            .collect::<Vec<_>>();
        for count in 1..=5 {
            for bytes in 16..=80 {
                for tokens in 16..=80 {
                    let selection = select_diversified(
                        &candidates,
                        &query(250_000),
                        budget(count, bytes, tokens),
                    )
                    .unwrap();
                    assert!(selection.prompt_order().len() <= count);
                    assert!(selection.total_bytes() <= bytes);
                    assert!(selection.total_tokens() <= tokens);
                    let recomputed_bytes = selection
                        .prompt_order()
                        .iter()
                        .map(|item| item.candidate().excerpt().text().len())
                        .sum::<usize>();
                    let recomputed_tokens = selection
                        .prompt_order()
                        .iter()
                        .map(|item| u64::from(item.candidate().exact_token_count().tokens()))
                        .sum::<u64>();
                    assert_eq!(selection.total_bytes(), recomputed_bytes);
                    assert_eq!(selection.total_tokens(), recomputed_tokens);
                }
            }
        }
    }
}
