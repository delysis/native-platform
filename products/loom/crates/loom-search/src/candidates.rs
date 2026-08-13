use std::collections::{BTreeMap, BTreeSet};

use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::UnitScore;

pub const MAX_CANDIDATE_OCCURRENCES: usize = 4_096;
pub const MAX_SEMANTIC_OBSERVATIONS: usize = 100_000;

/// One causal occurrence and the identity of its exact bytes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidateOccurrence {
    pub occurrence_id: ArtifactId,
    pub content_blob_id: BlobId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExactContentGroup {
    content_blob_id: BlobId,
    occurrence_ids: Vec<ArtifactId>,
}

impl ExactContentGroup {
    pub const fn content_blob_id(&self) -> BlobId {
        self.content_blob_id
    }

    pub fn occurrence_ids(&self) -> &[ArtifactId] {
        &self.occurrence_ids
    }
}

/// Groups exact bytes while retaining every causal occurrence identity.
pub fn deduplicate_exact_content(
    candidates: &[CandidateOccurrence],
) -> Result<Vec<ExactContentGroup>, CandidateSetError> {
    validate_candidate_count(candidates.len())?;

    let mut seen_occurrences = BTreeSet::new();
    let mut by_content = BTreeMap::<BlobId, Vec<ArtifactId>>::new();
    for candidate in candidates {
        if !seen_occurrences.insert(candidate.occurrence_id) {
            return Err(CandidateSetError::DuplicateOccurrence(
                candidate.occurrence_id,
            ));
        }
        by_content
            .entry(candidate.content_blob_id)
            .or_default()
            .push(candidate.occurrence_id);
    }

    Ok(by_content
        .into_iter()
        .map(|(content_blob_id, mut occurrence_ids)| {
            occurrence_ids.sort_unstable();
            ExactContentGroup {
                content_blob_id,
                occurrence_ids,
            }
        })
        .collect())
}

/// A similarity supplied by a caller that owns the embedding or comparison
/// method. Loom Search never synthesizes a value for a missing pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SemanticSimilarityObservation {
    lower_content_blob_id: BlobId,
    upper_content_blob_id: BlobId,
    similarity: UnitScore,
}

impl SemanticSimilarityObservation {
    pub fn new(
        left_content_blob_id: BlobId,
        right_content_blob_id: BlobId,
        similarity: UnitScore,
    ) -> Result<Self, CandidateSetError> {
        if left_content_blob_id == right_content_blob_id {
            return Err(CandidateSetError::SelfSimilarity(left_content_blob_id));
        }
        let (lower_content_blob_id, upper_content_blob_id) =
            if left_content_blob_id < right_content_blob_id {
                (left_content_blob_id, right_content_blob_id)
            } else {
                (right_content_blob_id, left_content_blob_id)
            };
        Ok(Self {
            lower_content_blob_id,
            upper_content_blob_id,
            similarity,
        })
    }

    pub const fn content_pair(&self) -> (BlobId, BlobId) {
        (self.lower_content_blob_id, self.upper_content_blob_id)
    }

    pub const fn similarity(&self) -> UnitScore {
        self.similarity
    }
}

impl<'de> Deserialize<'de> for SemanticSimilarityObservation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireObservation {
            lower_content_blob_id: BlobId,
            upper_content_blob_id: BlobId,
            similarity: UnitScore,
        }

        let wire = WireObservation::deserialize(deserializer)?;
        Self::new(
            wire.lower_content_blob_id,
            wire.upper_content_blob_id,
            wire.similarity,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SemanticCluster {
    content_blob_ids: Vec<BlobId>,
    occurrence_ids: Vec<ArtifactId>,
}

impl SemanticCluster {
    pub fn content_blob_ids(&self) -> &[BlobId] {
        &self.content_blob_ids
    }

    pub fn occurrence_ids(&self) -> &[ArtifactId] {
        &self.occurrence_ids
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SemanticClustering {
    exact_groups: Vec<ExactContentGroup>,
    clusters: Vec<SemanticCluster>,
    supplied_pair_count: u64,
    missing_pair_count: u64,
}

impl SemanticClustering {
    pub fn exact_groups(&self) -> &[ExactContentGroup] {
        &self.exact_groups
    }

    pub fn clusters(&self) -> &[SemanticCluster] {
        &self.clusters
    }

    pub const fn supplied_pair_count(&self) -> u64 {
        self.supplied_pair_count
    }

    pub const fn missing_pair_count(&self) -> u64 {
        self.missing_pair_count
    }
}

/// Stable single-link clustering over only the explicitly supplied similarities.
/// An absent pair stays absent and is reported in `missing_pair_count`.
pub fn cluster_candidates(
    candidates: &[CandidateOccurrence],
    similarities: &[SemanticSimilarityObservation],
    threshold: UnitScore,
) -> Result<SemanticClustering, CandidateSetError> {
    if similarities.len() > MAX_SEMANTIC_OBSERVATIONS {
        return Err(CandidateSetError::TooManySemanticObservations(
            similarities.len(),
        ));
    }

    let exact_groups = deduplicate_exact_content(candidates)?;
    let content_ids: Vec<_> = exact_groups
        .iter()
        .map(ExactContentGroup::content_blob_id)
        .collect();
    let content_indexes: BTreeMap<_, _> = content_ids
        .iter()
        .copied()
        .enumerate()
        .map(|(index, content_id)| (content_id, index))
        .collect();

    let mut supplied_pairs = BTreeSet::new();
    let mut ordered_similarities = similarities.to_vec();
    ordered_similarities.sort_unstable_by_key(SemanticSimilarityObservation::content_pair);
    for observation in &ordered_similarities {
        let pair = observation.content_pair();
        if !content_indexes.contains_key(&pair.0) {
            return Err(CandidateSetError::UnknownContent(pair.0));
        }
        if !content_indexes.contains_key(&pair.1) {
            return Err(CandidateSetError::UnknownContent(pair.1));
        }
        if !supplied_pairs.insert(pair) {
            return Err(CandidateSetError::DuplicateSemanticPair {
                lower: pair.0,
                upper: pair.1,
            });
        }
    }

    let mut union_find = UnionFind::new(content_ids.len());
    for observation in &ordered_similarities {
        if observation.similarity >= threshold {
            let pair = observation.content_pair();
            union_find.union(content_indexes[&pair.0], content_indexes[&pair.1]);
        }
    }

    let mut members = BTreeMap::<usize, Vec<usize>>::new();
    for index in 0..content_ids.len() {
        let root = union_find.find(index);
        members.entry(root).or_default().push(index);
    }

    let clusters = members
        .into_values()
        .map(|indexes| {
            let content_blob_ids: Vec<_> =
                indexes.iter().map(|index| content_ids[*index]).collect();
            let mut occurrence_ids: Vec<_> = indexes
                .iter()
                .flat_map(|index| exact_groups[*index].occurrence_ids.iter().copied())
                .collect();
            occurrence_ids.sort_unstable();
            SemanticCluster {
                content_blob_ids,
                occurrence_ids,
            }
        })
        .collect();

    let content_count =
        u64::try_from(content_ids.len()).map_err(|_| CandidateSetError::PairCountOverflow)?;
    let possible_pair_count = content_count
        .checked_mul(content_count.saturating_sub(1))
        .and_then(|product| product.checked_div(2))
        .ok_or(CandidateSetError::PairCountOverflow)?;
    let supplied_pair_count =
        u64::try_from(supplied_pairs.len()).map_err(|_| CandidateSetError::PairCountOverflow)?;
    let missing_pair_count = possible_pair_count
        .checked_sub(supplied_pair_count)
        .ok_or(CandidateSetError::PairCountOverflow)?;

    Ok(SemanticClustering {
        exact_groups,
        clusters,
        supplied_pair_count,
        missing_pair_count,
    })
}

fn validate_candidate_count(count: usize) -> Result<(), CandidateSetError> {
    if count == 0 || count > MAX_CANDIDATE_OCCURRENCES {
        return Err(CandidateSetError::InvalidCandidateCount(count));
    }
    Ok(())
}

struct UnionFind {
    parents: Vec<usize>,
}

impl UnionFind {
    fn new(len: usize) -> Self {
        Self {
            parents: (0..len).collect(),
        }
    }

    fn find(&mut self, index: usize) -> usize {
        let parent = self.parents[index];
        if parent == index {
            return index;
        }
        let root = self.find(parent);
        self.parents[index] = root;
        root
    }

    fn union(&mut self, left: usize, right: usize) {
        let left_root = self.find(left);
        let right_root = self.find(right);
        if left_root == right_root {
            return;
        }
        let (lower, upper) = if left_root < right_root {
            (left_root, right_root)
        } else {
            (right_root, left_root)
        };
        self.parents[upper] = lower;
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CandidateSetError {
    #[error("candidate count {0} is outside 1..={MAX_CANDIDATE_OCCURRENCES}")]
    InvalidCandidateCount(usize),
    #[error("candidate occurrence {0} occurs more than once")]
    DuplicateOccurrence(ArtifactId),
    #[error("semantic observation count {0} exceeds {MAX_SEMANTIC_OBSERVATIONS}")]
    TooManySemanticObservations(usize),
    #[error("semantic similarity cannot compare content {0} with itself")]
    SelfSimilarity(BlobId),
    #[error("semantic similarity references unknown content {0}")]
    UnknownContent(BlobId),
    #[error("semantic similarity for {lower} and {upper} occurs more than once")]
    DuplicateSemanticPair { lower: BlobId, upper: BlobId },
    #[error("semantic pair accounting overflowed")]
    PairCountOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(bytes: &[u8]) -> CandidateOccurrence {
        CandidateOccurrence {
            occurrence_id: ArtifactId::new(),
            content_blob_id: BlobId::digest(bytes),
        }
    }

    #[test]
    fn exact_dedup_keeps_distinct_occurrence_identities() {
        let first = candidate(b"same bytes");
        let second = candidate(b"same bytes");
        let groups = deduplicate_exact_content(&[first, second]).expect("dedup");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].occurrence_ids().len(), 2);
        assert!(groups[0].occurrence_ids().contains(&first.occurrence_id));
        assert!(groups[0].occurrence_ids().contains(&second.occurrence_id));
    }

    #[test]
    fn missing_semantic_values_are_reported_not_fabricated() {
        let first = candidate(b"first");
        let second = candidate(b"second");
        let third = candidate(b"third");
        let similarity = SemanticSimilarityObservation::new(
            first.content_blob_id,
            second.content_blob_id,
            UnitScore::from_millionths(900_000).expect("score"),
        )
        .expect("pair");
        let clustering = cluster_candidates(
            &[third, second, first],
            &[similarity],
            UnitScore::from_millionths(800_000).expect("threshold"),
        )
        .expect("clustering");
        assert_eq!(clustering.clusters().len(), 2);
        assert_eq!(clustering.supplied_pair_count(), 1);
        assert_eq!(clustering.missing_pair_count(), 2);
    }

    #[test]
    fn clustering_is_stable_under_input_permutation() {
        let first = candidate(b"first");
        let second = candidate(b"second");
        let third = candidate(b"third");
        let observations = [
            SemanticSimilarityObservation::new(
                first.content_blob_id,
                second.content_blob_id,
                UnitScore::ONE,
            )
            .expect("first pair"),
            SemanticSimilarityObservation::new(
                second.content_blob_id,
                third.content_blob_id,
                UnitScore::ONE,
            )
            .expect("second pair"),
        ];
        let forward = cluster_candidates(&[first, second, third], &observations, UnitScore::ONE)
            .expect("forward");
        let reverse = cluster_candidates(
            &[third, second, first],
            &[observations[1], observations[0]],
            UnitScore::ONE,
        )
        .expect("reverse");
        assert_eq!(forward, reverse);
    }

    #[test]
    fn semantic_observation_round_trip_preserves_normalized_pair() {
        let observation = SemanticSimilarityObservation::new(
            BlobId::digest(b"right"),
            BlobId::digest(b"left"),
            UnitScore::from_millionths(500_000).expect("score"),
        )
        .expect("observation");
        let encoded = serde_json::to_string(&observation).expect("serialize");
        let decoded: SemanticSimilarityObservation =
            serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, observation);
    }
}
