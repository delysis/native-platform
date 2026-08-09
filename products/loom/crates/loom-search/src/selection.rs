use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{MAX_CANDIDATE_OCCURRENCES, UnitScore};

const NOVELTY_SEED_DOMAIN: &[u8] = b"loom-native/seeded-novelty/v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidatePoint {
    pub occurrence_id: ArtifactId,
    pub content_blob_id: BlobId,
    pub quality: UnitScore,
    pub novelty: UnitScore,
}

impl CandidatePoint {
    pub const fn new(
        occurrence_id: ArtifactId,
        content_blob_id: BlobId,
        quality: UnitScore,
        novelty: UnitScore,
    ) -> Self {
        Self {
            occurrence_id,
            content_blob_id,
            quality,
            novelty,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SeededNoveltyReceipt {
    /// The exact caller-supplied seed. Persisting it makes selection replayable.
    pub seed: [u8; 32],
    pub draw: u64,
    pub total_weight: u64,
    /// False only when the quality champion dominated every other distinct
    /// content item and the explicit exploration slot used the full set.
    pub from_pareto_frontier: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct QualityNoveltySelection {
    frontier: Vec<CandidatePoint>,
    quality_choice: CandidatePoint,
    novelty_choice: Option<CandidatePoint>,
    novelty_receipt: Option<SeededNoveltyReceipt>,
}

impl QualityNoveltySelection {
    pub fn frontier(&self) -> &[CandidatePoint] {
        &self.frontier
    }

    pub const fn quality_choice(&self) -> CandidatePoint {
        self.quality_choice
    }

    pub const fn novelty_choice(&self) -> Option<CandidatePoint> {
        self.novelty_choice
    }

    pub const fn novelty_receipt(&self) -> Option<SeededNoveltyReceipt> {
        self.novelty_receipt
    }
}

/// Computes a deterministic two-objective Pareto frontier. Equal-scored causal
/// occurrences remain distinct; exact content identity is never substituted
/// for occurrence identity.
pub fn pareto_frontier(
    candidates: &[CandidatePoint],
) -> Result<Vec<CandidatePoint>, SelectionError> {
    validate_candidates(candidates)?;
    let mut frontier: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|candidate| !candidates.iter().any(|other| dominates(*other, *candidate)))
        .collect();
    frontier.sort_unstable_by_key(|candidate| candidate.occurrence_id);
    Ok(frontier)
}

const fn dominates(left: CandidatePoint, right: CandidatePoint) -> bool {
    left.quality.millionths() >= right.quality.millionths()
        && left.novelty.millionths() >= right.novelty.millionths()
        && (left.quality.millionths() > right.quality.millionths()
            || left.novelty.millionths() > right.novelty.millionths())
}

/// Keeps the deterministic quality champion and, whenever distinct bytes
/// exist, a separately seeded novelty choice. The novelty draw first uses the
/// Pareto frontier and falls back to the full set only to preserve that explicit
/// exploration slot. It is weighted by `novelty + 1` and cannot select another
/// occurrence of the quality choice's exact content.
pub fn select_quality_plus_seeded_novelty(
    candidates: &[CandidatePoint],
    seed: [u8; 32],
) -> Result<QualityNoveltySelection, SelectionError> {
    let frontier = pareto_frontier(candidates)?;
    let quality_choice = frontier
        .iter()
        .copied()
        .min_by(compare_quality_choice)
        .ok_or(SelectionError::InvalidCandidateCount(0))?;

    let frontier_pool = distinct_novelty_pool(&frontier, quality_choice.content_blob_id);
    let (novelty_pool, from_pareto_frontier) = if frontier_pool.is_empty() {
        (
            distinct_novelty_pool(candidates, quality_choice.content_blob_id),
            false,
        )
    } else {
        (frontier_pool, true)
    };
    let (novelty_choice, novelty_receipt) = if novelty_pool.is_empty() {
        (None, None)
    } else {
        let total_weight = novelty_pool.iter().try_fold(0_u64, |total, candidate| {
            total
                .checked_add(u64::from(candidate.novelty.millionths()) + 1)
                .ok_or(SelectionError::ArithmeticOverflow)
        })?;
        let draw = seeded_draw(seed, &novelty_pool)? % total_weight;
        let selected = weighted_choice(&novelty_pool, draw)?;
        (
            Some(selected),
            Some(SeededNoveltyReceipt {
                seed,
                draw,
                total_weight,
                from_pareto_frontier,
            }),
        )
    };

    Ok(QualityNoveltySelection {
        frontier,
        quality_choice,
        novelty_choice,
        novelty_receipt,
    })
}

fn validate_candidates(candidates: &[CandidatePoint]) -> Result<(), SelectionError> {
    if candidates.is_empty() || candidates.len() > MAX_CANDIDATE_OCCURRENCES {
        return Err(SelectionError::InvalidCandidateCount(candidates.len()));
    }
    let mut occurrence_ids = BTreeSet::new();
    for candidate in candidates {
        if !occurrence_ids.insert(candidate.occurrence_id) {
            return Err(SelectionError::DuplicateOccurrence(candidate.occurrence_id));
        }
    }
    Ok(())
}

fn compare_quality_choice(left: &CandidatePoint, right: &CandidatePoint) -> Ordering {
    right
        .quality
        .cmp(&left.quality)
        .then_with(|| right.novelty.cmp(&left.novelty))
        .then_with(|| left.occurrence_id.cmp(&right.occurrence_id))
}

fn compare_novelty_representative(left: CandidatePoint, right: CandidatePoint) -> Ordering {
    left.novelty
        .cmp(&right.novelty)
        .then_with(|| left.quality.cmp(&right.quality))
        .then_with(|| right.occurrence_id.cmp(&left.occurrence_id))
}

fn distinct_novelty_pool(
    frontier: &[CandidatePoint],
    excluded_content: BlobId,
) -> Vec<CandidatePoint> {
    let mut by_content = BTreeMap::<BlobId, CandidatePoint>::new();
    for candidate in frontier {
        if candidate.content_blob_id == excluded_content {
            continue;
        }
        by_content
            .entry(candidate.content_blob_id)
            .and_modify(|selected| {
                if compare_novelty_representative(*candidate, *selected).is_gt() {
                    *selected = *candidate;
                }
            })
            .or_insert(*candidate);
    }
    let mut pool: Vec<_> = by_content.into_values().collect();
    pool.sort_unstable_by_key(|candidate| candidate.occurrence_id);
    pool
}

fn seeded_draw(seed: [u8; 32], candidates: &[CandidatePoint]) -> Result<u64, SelectionError> {
    let candidate_bytes = candidates
        .len()
        .checked_mul(16 + BlobId::BYTE_LEN + 8)
        .ok_or(SelectionError::ArithmeticOverflow)?;
    let capacity = NOVELTY_SEED_DOMAIN
        .len()
        .checked_add(seed.len())
        .and_then(|base| base.checked_add(candidate_bytes))
        .ok_or(SelectionError::ArithmeticOverflow)?;
    let mut material = Vec::with_capacity(capacity);
    material.extend_from_slice(NOVELTY_SEED_DOMAIN);
    material.extend_from_slice(&seed);
    for candidate in candidates {
        material.extend_from_slice(&candidate.occurrence_id.as_ulid().to_bytes());
        material.extend_from_slice(candidate.content_blob_id.as_bytes());
        material.extend_from_slice(&candidate.quality.millionths().to_be_bytes());
        material.extend_from_slice(&candidate.novelty.millionths().to_be_bytes());
    }
    let digest = BlobId::digest(&material);
    let draw_bytes: [u8; 8] = digest.as_bytes()[..8]
        .try_into()
        .map_err(|_| SelectionError::ArithmeticOverflow)?;
    Ok(u64::from_be_bytes(draw_bytes))
}

fn weighted_choice(
    candidates: &[CandidatePoint],
    draw: u64,
) -> Result<CandidatePoint, SelectionError> {
    let mut cursor = draw;
    for candidate in candidates {
        let weight = u64::from(candidate.novelty.millionths()) + 1;
        if cursor < weight {
            return Ok(*candidate);
        }
        cursor = cursor
            .checked_sub(weight)
            .ok_or(SelectionError::ArithmeticOverflow)?;
    }
    Err(SelectionError::ArithmeticOverflow)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SelectionError {
    #[error("candidate count {0} is outside 1..={MAX_CANDIDATE_OCCURRENCES}")]
    InvalidCandidateCount(usize),
    #[error("candidate occurrence {0} occurs more than once")]
    DuplicateOccurrence(ArtifactId),
    #[error("selection arithmetic overflowed")]
    ArithmeticOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(value: u32) -> UnitScore {
        UnitScore::from_millionths(value).expect("score")
    }

    fn candidate(bytes: &[u8], quality: u32, novelty: u32) -> CandidatePoint {
        CandidatePoint::new(
            ArtifactId::new(),
            BlobId::digest(bytes),
            score(quality),
            score(novelty),
        )
    }

    #[test]
    fn frontier_keeps_quality_and_novelty_extremes() {
        let quality = candidate(b"quality", 900_000, 200_000);
        let novelty = candidate(b"novelty", 500_000, 900_000);
        let dominated = candidate(b"dominated", 400_000, 100_000);
        let frontier = pareto_frontier(&[quality, novelty, dominated]).expect("frontier");
        assert_eq!(frontier.len(), 2);
        assert!(frontier.contains(&quality));
        assert!(frontier.contains(&novelty));
        assert!(!frontier.contains(&dominated));
    }

    #[test]
    fn frontier_is_stable_under_input_permutation() {
        let first = candidate(b"first", 900_000, 200_000);
        let second = candidate(b"second", 500_000, 900_000);
        let third = candidate(b"third", 400_000, 100_000);
        let forward = pareto_frontier(&[first, second, third]).expect("forward");
        let reverse = pareto_frontier(&[third, second, first]).expect("reverse");
        assert_eq!(forward, reverse);
    }

    #[test]
    fn quality_and_novelty_slots_remain_distinct() {
        let quality = candidate(b"quality", 1_000_000, 100_000);
        let novelty = candidate(b"novelty", 800_000, 900_000);
        let selection =
            select_quality_plus_seeded_novelty(&[quality, novelty], [7; 32]).expect("selection");
        assert_eq!(selection.quality_choice(), quality);
        assert_eq!(selection.novelty_choice(), Some(novelty));
        assert!(selection.novelty_receipt().is_some());
    }

    #[test]
    fn exploration_slot_survives_a_single_point_frontier() {
        let quality = candidate(b"quality", 1_000_000, 1_000_000);
        let alternative = candidate(b"alternative", 500_000, 500_000);
        let selection = select_quality_plus_seeded_novelty(&[quality, alternative], [9; 32])
            .expect("selection");
        assert_eq!(selection.frontier(), &[quality]);
        assert_eq!(selection.novelty_choice(), Some(alternative));
        assert!(
            !selection
                .novelty_receipt()
                .expect("receipt")
                .from_pareto_frontier
        );
    }

    #[test]
    fn duplicate_bytes_do_not_fill_both_slots() {
        let quality = candidate(b"same", 1_000_000, 100_000);
        let duplicate = candidate(b"same", 900_000, 900_000);
        let distinct = candidate(b"distinct", 800_000, 950_000);
        let selection =
            select_quality_plus_seeded_novelty(&[quality, duplicate, distinct], [3; 32])
                .expect("selection");
        assert_eq!(selection.quality_choice(), quality);
        assert_eq!(selection.novelty_choice(), Some(distinct));
    }

    #[test]
    fn duplicate_content_occurrences_remain_on_equal_frontier() {
        let first = candidate(b"same", 500_000, 500_000);
        let second = candidate(b"same", 500_000, 500_000);
        let frontier = pareto_frontier(&[first, second]).expect("frontier");
        assert_eq!(frontier.len(), 2);
        assert!(frontier.contains(&first));
        assert!(frontier.contains(&second));
    }

    #[test]
    fn seeded_choice_is_replayable_and_receipted() {
        let quality = candidate(b"quality", 1_000_000, 0);
        let first = candidate(b"first", 900_000, 800_000);
        let second = candidate(b"second", 800_000, 900_000);
        let seed = [42; 32];
        let forward =
            select_quality_plus_seeded_novelty(&[quality, first, second], seed).expect("forward");
        let permuted =
            select_quality_plus_seeded_novelty(&[second, quality, first], seed).expect("permuted");
        assert_eq!(forward, permuted);
        assert_eq!(forward.novelty_receipt().expect("receipt").seed, seed);
    }

    #[test]
    fn duplicate_occurrence_is_rejected_even_when_content_differs() {
        let first = candidate(b"first", 500_000, 500_000);
        let second = CandidatePoint::new(
            first.occurrence_id,
            BlobId::digest(b"second"),
            score(500_000),
            score(500_000),
        );
        assert_eq!(
            pareto_frontier(&[first, second]),
            Err(SelectionError::DuplicateOccurrence(first.occurrence_id))
        );
    }
}
