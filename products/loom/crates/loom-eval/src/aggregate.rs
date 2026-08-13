use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::TrialCaseId;
use loom_search::UnitScore;
use loom_types::ArtifactId;
use serde::Serialize;
use thiserror::Error;

use crate::ValidatedCriterionObservation;

pub const MAX_EVALUATORS_PER_ENSEMBLE: usize = 32;
pub const MAX_CLUSTERED_CASES: usize = 4_096;
pub const MAX_CELLS_PER_CASE: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvaluatorScorecard {
    evaluator_id: ArtifactId,
    candidate_occurrence_id: ArtifactId,
    observations: Vec<ValidatedCriterionObservation>,
}

impl EvaluatorScorecard {
    pub fn new(
        evaluator_id: ArtifactId,
        candidate_occurrence_id: ArtifactId,
        observations: Vec<ValidatedCriterionObservation>,
    ) -> Result<Self, AggregateError> {
        if observations.is_empty() {
            return Err(AggregateError::EmptyScorecard);
        }
        let mut keys = BTreeSet::new();
        for observation in &observations {
            let key = observation_key(observation);
            if !keys.insert(key) {
                return Err(AggregateError::DuplicateCriterion);
            }
            if let ValidatedCriterionObservation::Scored { evidence, .. } = observation
                && evidence
                    .iter()
                    .any(|span| span.candidate_occurrence_id() != candidate_occurrence_id)
            {
                return Err(AggregateError::MixedCandidateEvidence);
            }
        }
        Ok(Self {
            evaluator_id,
            candidate_occurrence_id,
            observations,
        })
    }

    pub const fn evaluator_id(&self) -> ArtifactId {
        self.evaluator_id
    }

    pub const fn candidate_occurrence_id(&self) -> ArtifactId {
        self.candidate_occurrence_id
    }

    pub fn observations(&self) -> &[ValidatedCriterionObservation] {
        &self.observations
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "disposition")]
pub enum PessimisticCriterionAggregate {
    Scored {
        criterion_key: String,
        score: UnitScore,
    },
    Abstained {
        criterion_key: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PessimisticEnsemble {
    candidate_occurrence_id: ArtifactId,
    evaluator_ids: Vec<ArtifactId>,
    criteria: Vec<PessimisticCriterionAggregate>,
}

impl PessimisticEnsemble {
    pub const fn candidate_occurrence_id(&self) -> ArtifactId {
        self.candidate_occurrence_id
    }

    pub fn evaluator_ids(&self) -> &[ArtifactId] {
        &self.evaluator_ids
    }

    pub fn criteria(&self) -> &[PessimisticCriterionAggregate] {
        &self.criteria
    }

    pub fn is_complete(&self) -> bool {
        self.criteria
            .iter()
            .all(|criterion| matches!(criterion, PessimisticCriterionAggregate::Scored { .. }))
    }
}

pub fn aggregate_pessimistic_ensemble(
    scorecards: &[EvaluatorScorecard],
) -> Result<PessimisticEnsemble, AggregateError> {
    if scorecards.is_empty() || scorecards.len() > MAX_EVALUATORS_PER_ENSEMBLE {
        return Err(AggregateError::InvalidEvaluatorCount(scorecards.len()));
    }
    let candidate = scorecards[0].candidate_occurrence_id;
    let expected_keys = scorecard_keys(&scorecards[0]);
    let mut evaluator_ids = BTreeSet::new();
    for scorecard in scorecards {
        if scorecard.candidate_occurrence_id != candidate {
            return Err(AggregateError::MixedCandidates);
        }
        if scorecard_keys(scorecard) != expected_keys {
            return Err(AggregateError::MixedCriterionSets);
        }
        if !evaluator_ids.insert(scorecard.evaluator_id) {
            return Err(AggregateError::DuplicateEvaluator);
        }
    }

    let mut criteria = Vec::with_capacity(expected_keys.len());
    for key in expected_keys {
        let mut minimum = UnitScore::ONE;
        let mut abstained = false;
        for scorecard in scorecards {
            let observation = scorecard
                .observations
                .iter()
                .find(|observation| observation_key(observation) == key)
                .expect("validated criterion sets are equal");
            match observation {
                ValidatedCriterionObservation::Scored { score, .. } => {
                    minimum = minimum.min(*score);
                }
                ValidatedCriterionObservation::Abstained { .. } => abstained = true,
            }
        }
        criteria.push(if abstained {
            PessimisticCriterionAggregate::Abstained { criterion_key: key }
        } else {
            PessimisticCriterionAggregate::Scored {
                criterion_key: key,
                score: minimum,
            }
        });
    }
    Ok(PessimisticEnsemble {
        candidate_occurrence_id: candidate,
        evaluator_ids: evaluator_ids.into_iter().collect(),
        criteria,
    })
}

fn scorecard_keys(scorecard: &EvaluatorScorecard) -> BTreeSet<String> {
    scorecard.observations.iter().map(observation_key).collect()
}

fn observation_key(observation: &ValidatedCriterionObservation) -> String {
    match observation {
        ValidatedCriterionObservation::Scored { criterion_key, .. } => criterion_key.clone(),
        ValidatedCriterionObservation::Abstained {
            expected_criterion_key,
            ..
        } => expected_criterion_key.clone(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedCellOutcome {
    BaselineWin,
    ContenderWin,
    Tie,
    Abstain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PairedCellObservation {
    pub cell_id: ArtifactId,
    pub case_id: TrialCaseId,
    pub outcome: PairedCellOutcome,
}

/// A case-clustered 95% interval. All cells for one case are collapsed before
/// uncertainty is computed, so candidate-order reversals and repeated judges
/// do not masquerade as independent examples.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ClusteredWinInterval {
    point_estimate: f64,
    lower_95: f64,
    upper_95: f64,
    case_count: usize,
    cell_count: usize,
    abstention_count: usize,
}

impl ClusteredWinInterval {
    pub const fn point_estimate(self) -> f64 {
        self.point_estimate
    }

    pub const fn lower_95(self) -> f64 {
        self.lower_95
    }

    pub const fn upper_95(self) -> f64 {
        self.upper_95
    }

    pub const fn case_count(self) -> usize {
        self.case_count
    }

    pub const fn cell_count(self) -> usize {
        self.cell_count
    }

    pub const fn abstention_count(self) -> usize {
        self.abstention_count
    }
}

pub fn case_clustered_win_interval(
    observations: &[PairedCellObservation],
) -> Result<ClusteredWinInterval, AggregateError> {
    if observations.is_empty() {
        return Err(AggregateError::NoPairedObservations);
    }
    let mut cells = BTreeSet::new();
    let mut by_case = BTreeMap::<TrialCaseId, Vec<PairedCellOutcome>>::new();
    let mut abstentions = 0_usize;
    for observation in observations {
        if !cells.insert(observation.cell_id) {
            return Err(AggregateError::DuplicateCell);
        }
        let outcomes = by_case.entry(observation.case_id).or_default();
        if outcomes.len() == MAX_CELLS_PER_CASE {
            return Err(AggregateError::TooManyCellsForCase);
        }
        outcomes.push(observation.outcome);
        if observation.outcome == PairedCellOutcome::Abstain {
            abstentions += 1;
        }
    }
    if by_case.len() < 2 || by_case.len() > MAX_CLUSTERED_CASES {
        return Err(AggregateError::InvalidClusterCount(by_case.len()));
    }

    let case_scores = by_case
        .values()
        .map(|outcomes| {
            let total = outcomes
                .iter()
                .map(|outcome| match outcome {
                    PairedCellOutcome::ContenderWin => 1.0_f64,
                    PairedCellOutcome::Tie => 0.5,
                    PairedCellOutcome::BaselineWin | PairedCellOutcome::Abstain => 0.0,
                })
                .sum::<f64>();
            let count = u32::try_from(outcomes.len()).expect("bounded cells per case fit in u32");
            total / f64::from(count)
        })
        .collect::<Vec<_>>();
    let count = case_scores.len();
    let count_u32 = u32::try_from(count).map_err(|_| AggregateError::ClusterCountOverflow)?;
    let count_float = f64::from(count_u32);
    let mean = case_scores.iter().sum::<f64>() / count_float;
    let squared_error = case_scores
        .iter()
        .map(|score| (score - mean).powi(2))
        .sum::<f64>();
    let degrees_of_freedom = count_u32 - 1;
    let sample_variance = squared_error / f64::from(degrees_of_freedom);
    let standard_error = (sample_variance / count_float).sqrt();
    let critical = student_t_975(count - 1);
    let margin = critical * standard_error;
    Ok(ClusteredWinInterval {
        point_estimate: mean,
        lower_95: (mean - margin).clamp(0.0, 1.0),
        upper_95: (mean + margin).clamp(0.0, 1.0),
        case_count: count,
        cell_count: observations.len(),
        abstention_count: abstentions,
    })
}

// Two-sided 95% Student t critical values for df 1..=30. Above 30 we retain
// the df=30 value rather than dropping to the 1.96 normal limit: 1.96 is
// anti-conservative for every finite sample, while 2.042 is a safe monotone
// upper bound for all larger degrees of freedom.
const T_975: [f64; 30] = [
    12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.160,
    2.145, 2.131, 2.120, 2.110, 2.101, 2.093, 2.086, 2.080, 2.074, 2.069, 2.064, 2.060, 2.056,
    2.052, 2.048, 2.045, 2.042,
];

fn student_t_975(degrees_of_freedom: usize) -> f64 {
    T_975
        .get(degrees_of_freedom.saturating_sub(1))
        .copied()
        .unwrap_or(T_975[T_975.len() - 1])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontierWinDecision {
    Pass,
    OverallPointEstimateTooLow,
    OverallLowerBoundTooLow,
    GenrePointEstimateTooLow,
}

pub fn evaluate_frontier_win_rule(
    overall: ClusteredWinInterval,
    genre_point_estimates: &[f64],
) -> FrontierWinDecision {
    if overall.point_estimate <= 0.55 {
        FrontierWinDecision::OverallPointEstimateTooLow
    } else if overall.lower_95 <= 0.5 {
        FrontierWinDecision::OverallLowerBoundTooLow
    } else if genre_point_estimates
        .iter()
        .any(|estimate| !estimate.is_finite() || *estimate < 0.5)
    {
        FrontierWinDecision::GenrePointEstimateTooLow
    } else {
        FrontierWinDecision::Pass
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AggregateError {
    #[error("an evaluator scorecard cannot be empty")]
    EmptyScorecard,
    #[error("an evaluator scorecard contains a duplicate criterion")]
    DuplicateCriterion,
    #[error("a scored observation cites another candidate")]
    MixedCandidateEvidence,
    #[error("evaluator count {0} is outside 1..={MAX_EVALUATORS_PER_ENSEMBLE}")]
    InvalidEvaluatorCount(usize),
    #[error("ensemble scorecards reference different candidates")]
    MixedCandidates,
    #[error("ensemble scorecards use different criterion sets")]
    MixedCriterionSets,
    #[error("ensemble contains the same evaluator more than once")]
    DuplicateEvaluator,
    #[error("paired evaluation has no observations")]
    NoPairedObservations,
    #[error("paired evaluation repeats a cell ID")]
    DuplicateCell,
    #[error("paired evaluation exceeds {MAX_CELLS_PER_CASE} cells for one case")]
    TooManyCellsForCase,
    #[error("case-cluster count {0} is outside 2..={MAX_CLUSTERED_CASES}")]
    InvalidClusterCount(usize),
    #[error("case-cluster count cannot be represented")]
    ClusterCountOverflow,
}

#[cfg(test)]
mod tests {
    use loom_research_types::{NonEmptyByteRange, TrialCaseId};
    use loom_types::BlobId;

    use crate::ValidatedEvidenceSpan;

    use super::*;

    fn scored(candidate: ArtifactId, key: &str, millionths: u32) -> ValidatedCriterionObservation {
        ValidatedCriterionObservation::Scored {
            criterion_key: key.into(),
            score: UnitScore::from_millionths(millionths).expect("score"),
            evidence: vec![ValidatedEvidenceSpan::for_test(
                candidate,
                BlobId::digest(b"candidate"),
                NonEmptyByteRange::new(0, 1).expect("range"),
                BlobId::digest(b"c"),
            )],
        }
    }

    #[test]
    fn ensemble_uses_minimum_and_never_reweights_abstention() {
        let candidate = ArtifactId::new();
        let first = EvaluatorScorecard::new(
            ArtifactId::new(),
            candidate,
            vec![scored(candidate, "voice", 900_000)],
        )
        .expect("first");
        let second = EvaluatorScorecard::new(
            ArtifactId::new(),
            candidate,
            vec![scored(candidate, "voice", 600_000)],
        )
        .expect("second");
        let aggregate = aggregate_pessimistic_ensemble(&[first, second]).expect("aggregate");
        assert_eq!(
            aggregate.criteria(),
            &[PessimisticCriterionAggregate::Scored {
                criterion_key: "voice".into(),
                score: UnitScore::from_millionths(600_000).unwrap(),
            }]
        );
    }

    #[test]
    fn repeated_cells_are_averaged_within_case_and_abstention_is_pessimistic() {
        let first_case = TrialCaseId::new();
        let second_case = TrialCaseId::new();
        let interval = case_clustered_win_interval(&[
            PairedCellObservation {
                cell_id: ArtifactId::new(),
                case_id: first_case,
                outcome: PairedCellOutcome::ContenderWin,
            },
            PairedCellObservation {
                cell_id: ArtifactId::new(),
                case_id: first_case,
                outcome: PairedCellOutcome::Tie,
            },
            PairedCellObservation {
                cell_id: ArtifactId::new(),
                case_id: second_case,
                outcome: PairedCellOutcome::Abstain,
            },
        ])
        .expect("interval");
        assert_eq!(interval.case_count(), 2);
        assert_eq!(interval.cell_count(), 3);
        assert_eq!(interval.abstention_count(), 1);
        assert!((interval.point_estimate() - 0.375).abs() < f64::EPSILON);
    }

    #[test]
    fn confirmation_thresholds_are_strict() {
        let interval = ClusteredWinInterval {
            point_estimate: 0.56,
            lower_95: 0.51,
            upper_95: 0.70,
            case_count: 30,
            cell_count: 120,
            abstention_count: 0,
        };
        assert_eq!(
            evaluate_frontier_win_rule(interval, &[0.5, 0.7]),
            FrontierWinDecision::Pass
        );
        assert_eq!(
            evaluate_frontier_win_rule(interval, &[0.49]),
            FrontierWinDecision::GenrePointEstimateTooLow
        );
    }

    #[test]
    fn finite_large_samples_never_use_the_narrower_normal_limit() {
        for degrees_of_freedom in [30, 31, MAX_CLUSTERED_CASES - 1] {
            assert!((student_t_975(degrees_of_freedom) - 2.042).abs() < f64::EPSILON);
        }
        assert!(student_t_975(31) > 1.96);
    }
}
