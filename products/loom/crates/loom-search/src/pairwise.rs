use std::collections::BTreeSet;

use loom_types::ArtifactId;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{EvidenceError, EvidenceRef, MAX_RUBRIC_CRITERIA, validate_evidence};

pub const MAX_PAIRWISE_OBSERVATIONS: usize = 1_024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairwiseVerdict {
    Abstain,
    First,
    Second,
    Tie,
}

/// One judge's result, retaining both candidate presentation order and rubric
/// criterion order. `First` and `Second` are deliberately positional here;
/// aggregation normalizes them to candidate identities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PairwiseObservation {
    observation_id: ArtifactId,
    evaluator_id: ArtifactId,
    rubric_id: ArtifactId,
    first_candidate_id: ArtifactId,
    second_candidate_id: ArtifactId,
    criterion_order: Vec<ArtifactId>,
    verdict: PairwiseVerdict,
    evidence: Vec<EvidenceRef>,
}

impl PairwiseObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        observation_id: ArtifactId,
        evaluator_id: ArtifactId,
        rubric_id: ArtifactId,
        first_candidate_id: ArtifactId,
        second_candidate_id: ArtifactId,
        criterion_order: Vec<ArtifactId>,
        verdict: PairwiseVerdict,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, PairwiseError> {
        if first_candidate_id == second_candidate_id {
            return Err(PairwiseError::SelfComparison(first_candidate_id));
        }
        validate_criterion_order(&criterion_order)?;
        validate_evidence(&evidence)?;
        Ok(Self {
            observation_id,
            evaluator_id,
            rubric_id,
            first_candidate_id,
            second_candidate_id,
            criterion_order,
            verdict,
            evidence,
        })
    }

    pub const fn observation_id(&self) -> ArtifactId {
        self.observation_id
    }

    pub const fn evaluator_id(&self) -> ArtifactId {
        self.evaluator_id
    }

    pub const fn rubric_id(&self) -> ArtifactId {
        self.rubric_id
    }

    pub const fn first_candidate_id(&self) -> ArtifactId {
        self.first_candidate_id
    }

    pub const fn second_candidate_id(&self) -> ArtifactId {
        self.second_candidate_id
    }

    pub fn criterion_order(&self) -> &[ArtifactId] {
        &self.criterion_order
    }

    pub const fn verdict(&self) -> PairwiseVerdict {
        self.verdict
    }

    pub fn evidence(&self) -> &[EvidenceRef] {
        &self.evidence
    }

    fn candidate_pair(&self) -> (ArtifactId, ArtifactId) {
        ordered_pair(self.first_candidate_id, self.second_candidate_id)
    }

    fn normalized_vote(&self) -> NormalizedPairwiseVote {
        match self.verdict {
            PairwiseVerdict::Abstain => NormalizedPairwiseVote::Abstain,
            PairwiseVerdict::Tie => NormalizedPairwiseVote::Tie,
            PairwiseVerdict::First => NormalizedPairwiseVote::Winner(self.first_candidate_id),
            PairwiseVerdict::Second => NormalizedPairwiseVote::Winner(self.second_candidate_id),
        }
    }
}

impl<'de> Deserialize<'de> for PairwiseObservation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireObservation {
            observation_id: ArtifactId,
            evaluator_id: ArtifactId,
            rubric_id: ArtifactId,
            first_candidate_id: ArtifactId,
            second_candidate_id: ArtifactId,
            criterion_order: Vec<ArtifactId>,
            verdict: PairwiseVerdict,
            evidence: Vec<EvidenceRef>,
        }

        let wire = WireObservation::deserialize(deserializer)?;
        Self::new(
            wire.observation_id,
            wire.evaluator_id,
            wire.rubric_id,
            wire.first_candidate_id,
            wire.second_candidate_id,
            wire.criterion_order,
            wire.verdict,
            wire.evidence,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn validate_criterion_order(criterion_order: &[ArtifactId]) -> Result<(), PairwiseError> {
    if criterion_order.is_empty() || criterion_order.len() > MAX_RUBRIC_CRITERIA {
        return Err(PairwiseError::InvalidCriterionCount(criterion_order.len()));
    }
    let mut criterion_ids = BTreeSet::new();
    for criterion_id in criterion_order {
        if !criterion_ids.insert(*criterion_id) {
            return Err(PairwiseError::DuplicateCriterion(*criterion_id));
        }
    }
    Ok(())
}

fn ordered_pair(left: ArtifactId, right: ArtifactId) -> (ArtifactId, ArtifactId) {
    if left < right {
        (left, right)
    } else {
        (right, left)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "vote", content = "candidate_id")]
pub enum NormalizedPairwiseVote {
    Abstain,
    Tie,
    Winner(ArtifactId),
}

impl NormalizedPairwiseVote {
    const fn decisive_winner(self) -> Option<ArtifactId> {
        match self {
            Self::Winner(candidate_id) => Some(candidate_id),
            Self::Abstain | Self::Tie => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PairwiseVoteCounts {
    pub lower_candidate_wins: u32,
    pub upper_candidate_wins: u32,
    pub ties: u32,
    pub abstentions: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairwiseDisposition {
    Abstained,
    Disputed,
    PreferLowerCandidate,
    PreferUpperCandidate,
    Tied,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PairwiseDisagreement {
    /// At least one decisive vote exists for each candidate.
    pub decisive_candidate_conflict: bool,
    /// Paired observations by one evaluator changed the normalized winner when
    /// candidate presentation order was reversed and rubric order was fixed.
    pub reversal_conflict_pairs: u32,
    /// Paired observations by one evaluator changed the normalized winner when
    /// rubric order changed and candidate presentation order was fixed.
    pub rubric_permutation_conflict_pairs: u32,
    pub distinct_rubric_orders: u32,
    pub contains_tie: bool,
    pub contains_abstention: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PairwiseReport {
    rubric_id: ArtifactId,
    lower_candidate_id: ArtifactId,
    upper_candidate_id: ArtifactId,
    observations: Vec<PairwiseObservation>,
    normalized_votes: Vec<NormalizedPairwiseVote>,
    counts: PairwiseVoteCounts,
    disposition: PairwiseDisposition,
    disagreement: PairwiseDisagreement,
}

impl PairwiseReport {
    pub const fn rubric_id(&self) -> ArtifactId {
        self.rubric_id
    }

    pub const fn candidate_pair(&self) -> (ArtifactId, ArtifactId) {
        (self.lower_candidate_id, self.upper_candidate_id)
    }

    pub fn observations(&self) -> &[PairwiseObservation] {
        &self.observations
    }

    pub fn normalized_votes(&self) -> &[NormalizedPairwiseVote] {
        &self.normalized_votes
    }

    pub const fn counts(&self) -> PairwiseVoteCounts {
        self.counts
    }

    pub const fn disposition(&self) -> PairwiseDisposition {
        self.disposition
    }

    pub const fn disagreement(&self) -> PairwiseDisagreement {
        self.disagreement
    }
}

pub fn aggregate_pairwise(
    mut observations: Vec<PairwiseObservation>,
) -> Result<PairwiseReport, PairwiseError> {
    if observations.is_empty() || observations.len() > MAX_PAIRWISE_OBSERVATIONS {
        return Err(PairwiseError::InvalidObservationCount(observations.len()));
    }

    let first = &observations[0];
    let expected_pair = first.candidate_pair();
    let rubric_id = first.rubric_id;
    let expected_criterion_set: BTreeSet<_> = first.criterion_order.iter().copied().collect();
    let mut observation_ids = BTreeSet::new();
    for observation in &observations {
        if observation.candidate_pair() != expected_pair {
            return Err(PairwiseError::MixedCandidatePairs {
                expected_lower: expected_pair.0,
                expected_upper: expected_pair.1,
                actual_lower: observation.candidate_pair().0,
                actual_upper: observation.candidate_pair().1,
            });
        }
        if observation.rubric_id != rubric_id {
            return Err(PairwiseError::MixedRubrics {
                expected: rubric_id,
                actual: observation.rubric_id,
            });
        }
        let actual_criterion_set: BTreeSet<_> =
            observation.criterion_order.iter().copied().collect();
        if actual_criterion_set != expected_criterion_set {
            return Err(PairwiseError::MixedCriterionSets);
        }
        if !observation_ids.insert(observation.observation_id) {
            return Err(PairwiseError::DuplicateObservation(
                observation.observation_id,
            ));
        }
    }

    observations.sort_unstable_by_key(|observation| observation.observation_id);
    let normalized_votes: Vec<_> = observations
        .iter()
        .map(PairwiseObservation::normalized_vote)
        .collect();
    let counts = count_votes(&normalized_votes, expected_pair)?;
    let disposition = disposition(counts);
    let disagreement = inspect_disagreement(&observations, &normalized_votes, counts)?;

    Ok(PairwiseReport {
        rubric_id,
        lower_candidate_id: expected_pair.0,
        upper_candidate_id: expected_pair.1,
        observations,
        normalized_votes,
        counts,
        disposition,
        disagreement,
    })
}

fn count_votes(
    votes: &[NormalizedPairwiseVote],
    candidate_pair: (ArtifactId, ArtifactId),
) -> Result<PairwiseVoteCounts, PairwiseError> {
    let mut counts = PairwiseVoteCounts {
        lower_candidate_wins: 0,
        upper_candidate_wins: 0,
        ties: 0,
        abstentions: 0,
    };
    for vote in votes {
        match vote {
            NormalizedPairwiseVote::Abstain => {
                counts.abstentions = checked_increment(counts.abstentions)?;
            }
            NormalizedPairwiseVote::Tie => {
                counts.ties = checked_increment(counts.ties)?;
            }
            NormalizedPairwiseVote::Winner(candidate_id) if *candidate_id == candidate_pair.0 => {
                counts.lower_candidate_wins = checked_increment(counts.lower_candidate_wins)?;
            }
            NormalizedPairwiseVote::Winner(candidate_id) if *candidate_id == candidate_pair.1 => {
                counts.upper_candidate_wins = checked_increment(counts.upper_candidate_wins)?;
            }
            NormalizedPairwiseVote::Winner(candidate_id) => {
                return Err(PairwiseError::InvalidNormalizedWinner(*candidate_id));
            }
        }
    }
    Ok(counts)
}

fn checked_increment(value: u32) -> Result<u32, PairwiseError> {
    value
        .checked_add(1)
        .ok_or(PairwiseError::ArithmeticOverflow)
}

const fn disposition(counts: PairwiseVoteCounts) -> PairwiseDisposition {
    if counts.lower_candidate_wins > 0 && counts.upper_candidate_wins > 0 {
        PairwiseDisposition::Disputed
    } else if counts.lower_candidate_wins > 0 {
        PairwiseDisposition::PreferLowerCandidate
    } else if counts.upper_candidate_wins > 0 {
        PairwiseDisposition::PreferUpperCandidate
    } else if counts.ties > 0 {
        PairwiseDisposition::Tied
    } else {
        PairwiseDisposition::Abstained
    }
}

fn inspect_disagreement(
    observations: &[PairwiseObservation],
    votes: &[NormalizedPairwiseVote],
    counts: PairwiseVoteCounts,
) -> Result<PairwiseDisagreement, PairwiseError> {
    let distinct_rubric_orders = observations
        .iter()
        .map(|observation| observation.criterion_order.clone())
        .collect::<BTreeSet<_>>()
        .len();
    let mut reversal_conflict_pairs = 0_u32;
    let mut rubric_permutation_conflict_pairs = 0_u32;
    for left_index in 0..observations.len() {
        for right_index in (left_index + 1)..observations.len() {
            let left = &observations[left_index];
            let right = &observations[right_index];
            if left.evaluator_id != right.evaluator_id {
                continue;
            }
            let Some(left_winner) = votes[left_index].decisive_winner() else {
                continue;
            };
            let Some(right_winner) = votes[right_index].decisive_winner() else {
                continue;
            };
            if left_winner == right_winner {
                continue;
            }

            let candidate_order_reversed = left.first_candidate_id == right.second_candidate_id
                && left.second_candidate_id == right.first_candidate_id;
            if candidate_order_reversed && left.criterion_order == right.criterion_order {
                reversal_conflict_pairs = checked_increment(reversal_conflict_pairs)?;
            }

            let candidate_order_fixed = left.first_candidate_id == right.first_candidate_id
                && left.second_candidate_id == right.second_candidate_id;
            if candidate_order_fixed && left.criterion_order != right.criterion_order {
                rubric_permutation_conflict_pairs =
                    checked_increment(rubric_permutation_conflict_pairs)?;
            }
        }
    }

    Ok(PairwiseDisagreement {
        decisive_candidate_conflict: counts.lower_candidate_wins > 0
            && counts.upper_candidate_wins > 0,
        reversal_conflict_pairs,
        rubric_permutation_conflict_pairs,
        distinct_rubric_orders: u32::try_from(distinct_rubric_orders)
            .map_err(|_| PairwiseError::ArithmeticOverflow)?,
        contains_tie: counts.ties > 0,
        contains_abstention: counts.abstentions > 0,
    })
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PairwiseError {
    #[error("a candidate cannot be compared with itself: {0}")]
    SelfComparison(ArtifactId),
    #[error("criterion count {0} is outside 1..={MAX_RUBRIC_CRITERIA}")]
    InvalidCriterionCount(usize),
    #[error("criterion {0} occurs more than once in a presentation")]
    DuplicateCriterion(ArtifactId),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error("pairwise observation count {0} is outside 1..={MAX_PAIRWISE_OBSERVATIONS}")]
    InvalidObservationCount(usize),
    #[error(
        "pairwise observations mix candidate pairs: expected ({expected_lower}, {expected_upper}), found ({actual_lower}, {actual_upper})"
    )]
    MixedCandidatePairs {
        expected_lower: ArtifactId,
        expected_upper: ArtifactId,
        actual_lower: ArtifactId,
        actual_upper: ArtifactId,
    },
    #[error("pairwise observations mix rubrics: expected {expected}, found {actual}")]
    MixedRubrics {
        expected: ArtifactId,
        actual: ArtifactId,
    },
    #[error("pairwise observations do not use the same criterion set")]
    MixedCriterionSets,
    #[error("pairwise observation {0} occurs more than once")]
    DuplicateObservation(ArtifactId),
    #[error("normalized winner {0} is not in the candidate pair")]
    InvalidNormalizedWinner(ArtifactId),
    #[error("pairwise accounting overflowed")]
    ArithmeticOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> Vec<EvidenceRef> {
        vec![EvidenceRef::artifact(ArtifactId::new())]
    }

    #[allow(clippy::too_many_arguments)]
    fn observation(
        evaluator: ArtifactId,
        rubric: ArtifactId,
        first: ArtifactId,
        second: ArtifactId,
        criteria: &[ArtifactId],
        verdict: PairwiseVerdict,
    ) -> PairwiseObservation {
        PairwiseObservation::new(
            ArtifactId::new(),
            evaluator,
            rubric,
            first,
            second,
            criteria.to_vec(),
            verdict,
            evidence(),
        )
        .expect("observation")
    }

    #[test]
    fn candidate_order_reversal_normalizes_the_same_winner() {
        let evaluator = ArtifactId::new();
        let rubric = ArtifactId::new();
        let criterion = ArtifactId::new();
        let left = ArtifactId::new();
        let right = ArtifactId::new();
        let report = aggregate_pairwise(vec![
            observation(
                evaluator,
                rubric,
                left,
                right,
                &[criterion],
                PairwiseVerdict::First,
            ),
            observation(
                evaluator,
                rubric,
                right,
                left,
                &[criterion],
                PairwiseVerdict::Second,
            ),
        ])
        .expect("report");
        assert!(!report.disagreement().decisive_candidate_conflict);
        assert_eq!(report.disagreement().reversal_conflict_pairs, 0);
        assert!(matches!(
            report.disposition(),
            PairwiseDisposition::PreferLowerCandidate | PairwiseDisposition::PreferUpperCandidate
        ));
    }

    #[test]
    fn first_position_bias_is_exposed_as_reversal_conflict() {
        let evaluator = ArtifactId::new();
        let rubric = ArtifactId::new();
        let criterion = ArtifactId::new();
        let left = ArtifactId::new();
        let right = ArtifactId::new();
        let report = aggregate_pairwise(vec![
            observation(
                evaluator,
                rubric,
                left,
                right,
                &[criterion],
                PairwiseVerdict::First,
            ),
            observation(
                evaluator,
                rubric,
                right,
                left,
                &[criterion],
                PairwiseVerdict::First,
            ),
        ])
        .expect("report");
        assert_eq!(report.disposition(), PairwiseDisposition::Disputed);
        assert!(report.disagreement().decisive_candidate_conflict);
        assert_eq!(report.disagreement().reversal_conflict_pairs, 1);
    }

    #[test]
    fn rubric_permutation_is_retained_and_conflict_is_exposed() {
        let evaluator = ArtifactId::new();
        let rubric = ArtifactId::new();
        let first_criterion = ArtifactId::new();
        let second_criterion = ArtifactId::new();
        let left = ArtifactId::new();
        let right = ArtifactId::new();
        let report = aggregate_pairwise(vec![
            observation(
                evaluator,
                rubric,
                left,
                right,
                &[first_criterion, second_criterion],
                PairwiseVerdict::First,
            ),
            observation(
                evaluator,
                rubric,
                left,
                right,
                &[second_criterion, first_criterion],
                PairwiseVerdict::Second,
            ),
        ])
        .expect("report");
        assert_eq!(report.disagreement().distinct_rubric_orders, 2);
        assert_eq!(report.disagreement().rubric_permutation_conflict_pairs, 1);
    }

    #[test]
    fn abstention_is_neither_a_tie_nor_a_preference() {
        let report = aggregate_pairwise(vec![observation(
            ArtifactId::new(),
            ArtifactId::new(),
            ArtifactId::new(),
            ArtifactId::new(),
            &[ArtifactId::new()],
            PairwiseVerdict::Abstain,
        )])
        .expect("report");
        assert_eq!(report.disposition(), PairwiseDisposition::Abstained);
        assert_eq!(report.counts().abstentions, 1);
        assert_eq!(report.counts().ties, 0);
        assert!(report.disagreement().contains_abstention);
    }

    #[test]
    fn every_pairwise_outcome_requires_evidence() {
        assert!(matches!(
            PairwiseObservation::new(
                ArtifactId::new(),
                ArtifactId::new(),
                ArtifactId::new(),
                ArtifactId::new(),
                ArtifactId::new(),
                vec![ArtifactId::new()],
                PairwiseVerdict::Abstain,
                Vec::new(),
            ),
            Err(PairwiseError::Evidence(EvidenceError::InvalidCount(0)))
        ));
    }

    #[test]
    fn criterion_set_changes_are_rejected_not_compared() {
        let evaluator = ArtifactId::new();
        let rubric = ArtifactId::new();
        let left = ArtifactId::new();
        let right = ArtifactId::new();
        let observations = vec![
            observation(
                evaluator,
                rubric,
                left,
                right,
                &[ArtifactId::new()],
                PairwiseVerdict::First,
            ),
            observation(
                evaluator,
                rubric,
                left,
                right,
                &[ArtifactId::new()],
                PairwiseVerdict::First,
            ),
        ];
        assert_eq!(
            aggregate_pairwise(observations),
            Err(PairwiseError::MixedCriterionSets)
        );
    }

    #[test]
    fn observation_round_trip_preserves_both_presentations() {
        let original = observation(
            ArtifactId::new(),
            ArtifactId::new(),
            ArtifactId::new(),
            ArtifactId::new(),
            &[ArtifactId::new(), ArtifactId::new()],
            PairwiseVerdict::Tie,
        );
        let encoded = serde_json::to_string(&original).expect("serialize");
        let decoded: PairwiseObservation = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, original);
    }
}
