use std::collections::BTreeSet;

use loom_research_types::TrialCaseId;
use loom_search::UnitScore;
use loom_types::{ArtifactId, BlobId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const CONFIRMATORY_N_VALUES: [usize; 6] = [1, 2, 4, 8, 16, 32];
pub const MAX_N_CURVE_POOL: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NCurvePoint {
    n: usize,
    ordered_pool: Vec<ArtifactId>,
    selected_occurrence_id: ArtifactId,
    selected_quality: Option<UnitScore>,
    cumulative_writer_tokens: u64,
    cumulative_wall_time_ms: u64,
}

impl NCurvePoint {
    pub fn new(
        ordered_pool: Vec<ArtifactId>,
        selected_occurrence_id: ArtifactId,
        selected_quality: Option<UnitScore>,
        cumulative_writer_tokens: u64,
        cumulative_wall_time_ms: u64,
    ) -> Result<Self, NCurveError> {
        let n = ordered_pool.len();
        if !CONFIRMATORY_N_VALUES.contains(&n) {
            return Err(NCurveError::UnsupportedN(n));
        }
        let unique = ordered_pool.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != ordered_pool.len() {
            return Err(NCurveError::DuplicateOccurrence);
        }
        if !unique.contains(&selected_occurrence_id) {
            return Err(NCurveError::SelectionOutsidePool);
        }
        if cumulative_writer_tokens == 0 || cumulative_wall_time_ms == 0 {
            return Err(NCurveError::EmptyCost);
        }
        Ok(Self {
            n,
            ordered_pool,
            selected_occurrence_id,
            selected_quality,
            cumulative_writer_tokens,
            cumulative_wall_time_ms,
        })
    }

    pub const fn n(&self) -> usize {
        self.n
    }

    pub fn ordered_pool(&self) -> &[ArtifactId] {
        &self.ordered_pool
    }

    pub const fn selected_occurrence_id(&self) -> ArtifactId {
        self.selected_occurrence_id
    }

    pub const fn selected_quality(&self) -> Option<UnitScore> {
        self.selected_quality
    }

    pub const fn cumulative_writer_tokens(&self) -> u64 {
        self.cumulative_writer_tokens
    }

    pub const fn cumulative_wall_time_ms(&self) -> u64 {
        self.cumulative_wall_time_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NestedNCurve {
    case_id: TrialCaseId,
    treatment_fingerprint: BlobId,
    selection_policy_fingerprint: BlobId,
    points: Vec<NCurvePoint>,
    curve_fingerprint: BlobId,
}

impl NestedNCurve {
    pub fn new(
        case_id: TrialCaseId,
        treatment_fingerprint: BlobId,
        selection_policy_fingerprint: BlobId,
        points: Vec<NCurvePoint>,
    ) -> Result<Self, NCurveError> {
        if points.is_empty() || points.len() > CONFIRMATORY_N_VALUES.len() {
            return Err(NCurveError::InvalidPointCount(points.len()));
        }
        for (index, point) in points.iter().enumerate() {
            if point.n != CONFIRMATORY_N_VALUES[index] {
                return Err(NCurveError::NonCanonicalNOrder);
            }
            if index > 0 {
                let previous = &points[index - 1];
                if !point.ordered_pool.starts_with(&previous.ordered_pool) {
                    return Err(NCurveError::PoolIsNotNested);
                }
                if point.cumulative_writer_tokens < previous.cumulative_writer_tokens
                    || point.cumulative_wall_time_ms < previous.cumulative_wall_time_ms
                {
                    return Err(NCurveError::NonMonotonicCost);
                }
            }
        }
        let curve_fingerprint = fingerprint_curve(
            case_id,
            treatment_fingerprint,
            selection_policy_fingerprint,
            &points,
        );
        Ok(Self {
            case_id,
            treatment_fingerprint,
            selection_policy_fingerprint,
            points,
            curve_fingerprint,
        })
    }

    pub const fn case_id(&self) -> TrialCaseId {
        self.case_id
    }

    pub const fn treatment_fingerprint(&self) -> BlobId {
        self.treatment_fingerprint
    }

    pub const fn selection_policy_fingerprint(&self) -> BlobId {
        self.selection_policy_fingerprint
    }

    pub fn points(&self) -> &[NCurvePoint] {
        &self.points
    }

    pub const fn curve_fingerprint(&self) -> BlobId {
        self.curve_fingerprint
    }

    pub const fn is_complete_confirmatory_curve(&self) -> bool {
        self.points.len() == CONFIRMATORY_N_VALUES.len()
    }
}

fn fingerprint_curve(
    case_id: TrialCaseId,
    treatment: BlobId,
    selection: BlobId,
    points: &[NCurvePoint],
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/nested-n-curve/v1\0");
    digest.update(case_id.as_ulid().to_bytes());
    digest.update(treatment.as_bytes());
    digest.update(selection.as_bytes());
    digest.update((points.len() as u64).to_be_bytes());
    for point in points {
        digest.update((point.n as u64).to_be_bytes());
        for occurrence in &point.ordered_pool {
            digest.update(occurrence.as_ulid().to_bytes());
        }
        digest.update(point.selected_occurrence_id.as_ulid().to_bytes());
        match point.selected_quality {
            Some(score) => {
                digest.update([1]);
                digest.update(score.millionths().to_be_bytes());
            }
            None => digest.update([0]),
        }
        digest.update(point.cumulative_writer_tokens.to_be_bytes());
        digest.update(point.cumulative_wall_time_ms.to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum NCurveError {
    #[error("N={0} is not one of the confirmatory nested values")]
    UnsupportedN(usize),
    #[error("an N-curve pool repeats an occurrence")]
    DuplicateOccurrence,
    #[error("an N-curve selection is not present in its pool")]
    SelectionOutsidePool,
    #[error("an N-curve point must charge nonzero writer tokens and wall time")]
    EmptyCost,
    #[error("N-curve point count {0} is outside 1..=6")]
    InvalidPointCount(usize),
    #[error("N-curve points are not the canonical prefix of [1,2,4,8,16,32]")]
    NonCanonicalNOrder,
    #[error("a larger N-curve pool is not an exact ordered extension of its predecessor")]
    PoolIsNotNested,
    #[error("cumulative N-curve cost decreased")]
    NonMonotonicCost,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(pool: &[ArtifactId], tokens: u64) -> NCurvePoint {
        NCurvePoint::new(pool.to_vec(), pool[0], Some(UnitScore::ONE), tokens, tokens)
            .expect("point")
    }

    #[test]
    fn accepts_only_exact_nested_prefixes() {
        let candidates = (0..4).map(|_| ArtifactId::new()).collect::<Vec<_>>();
        let curve = NestedNCurve::new(
            TrialCaseId::new(),
            BlobId::digest(b"treatment"),
            BlobId::digest(b"selection"),
            vec![
                point(&candidates[..1], 10),
                point(&candidates[..2], 20),
                point(&candidates[..4], 40),
            ],
        )
        .expect("nested curve");
        assert_eq!(curve.points().len(), 3);

        let mut permuted = candidates[..2].to_vec();
        permuted.swap(0, 1);
        assert!(matches!(
            NestedNCurve::new(
                TrialCaseId::new(),
                BlobId::digest(b"treatment"),
                BlobId::digest(b"selection"),
                vec![point(&candidates[..1], 10), point(&permuted, 20)],
            ),
            Err(NCurveError::PoolIsNotNested)
        ));
    }

    #[test]
    fn selected_candidate_must_come_from_the_pool() {
        let candidate = ArtifactId::new();
        assert!(matches!(
            NCurvePoint::new(vec![candidate], ArtifactId::new(), None, 1, 1),
            Err(NCurveError::SelectionOutsidePool)
        ));
    }
}
