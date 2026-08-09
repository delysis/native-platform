use std::collections::{BTreeMap, BTreeSet};

use loom_types::ArtifactId;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{EvidenceError, EvidenceRef, SCORE_SCALE, UnitScore, validate_evidence};

pub const MAX_RUBRIC_CRITERIA: usize = 64;
pub const MAX_CRITERION_WEIGHT: u32 = SCORE_SCALE;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct CriterionWeight(u32);

impl CriterionWeight {
    pub const fn new(value: u32) -> Result<Self, RubricError> {
        if value == 0 || value > MAX_CRITERION_WEIGHT {
            return Err(RubricError::InvalidWeight(value));
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for CriterionWeight {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RubricCriterion {
    criterion_id: ArtifactId,
    weight: CriterionWeight,
}

impl RubricCriterion {
    pub const fn new(criterion_id: ArtifactId, weight: CriterionWeight) -> Self {
        Self {
            criterion_id,
            weight,
        }
    }

    pub const fn criterion_id(self) -> ArtifactId {
        self.criterion_id
    }

    pub const fn weight(self) -> CriterionWeight {
        self.weight
    }
}

/// A product-neutral rubric. Criterion prose and versioning live in the
/// referenced artifacts; this type owns only stable identities and weights.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Rubric {
    rubric_id: ArtifactId,
    criteria: Vec<RubricCriterion>,
}

impl Rubric {
    pub fn new(
        rubric_id: ArtifactId,
        mut criteria: Vec<RubricCriterion>,
    ) -> Result<Self, RubricError> {
        validate_criteria(&criteria)?;
        criteria.sort_unstable_by_key(|criterion| criterion.criterion_id);
        Ok(Self {
            rubric_id,
            criteria,
        })
    }

    pub const fn rubric_id(&self) -> ArtifactId {
        self.rubric_id
    }

    pub fn criteria(&self) -> &[RubricCriterion] {
        &self.criteria
    }
}

impl<'de> Deserialize<'de> for Rubric {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireRubric {
            rubric_id: ArtifactId,
            criteria: Vec<RubricCriterion>,
        }

        let wire = WireRubric::deserialize(deserializer)?;
        Self::new(wire.rubric_id, wire.criteria).map_err(serde::de::Error::custom)
    }
}

fn validate_criteria(criteria: &[RubricCriterion]) -> Result<(), RubricError> {
    if criteria.is_empty() || criteria.len() > MAX_RUBRIC_CRITERIA {
        return Err(RubricError::InvalidCriterionCount(criteria.len()));
    }
    let mut ids = BTreeSet::new();
    for criterion in criteria {
        if !ids.insert(criterion.criterion_id) {
            return Err(RubricError::DuplicateCriterion(criterion.criterion_id));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "score")]
pub enum CriterionOutcome {
    Abstain,
    Score(UnitScore),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RubricObservation {
    observation_id: ArtifactId,
    evaluator_id: ArtifactId,
    candidate_occurrence_id: ArtifactId,
    criterion_id: ArtifactId,
    outcome: CriterionOutcome,
    evidence: Vec<EvidenceRef>,
}

impl RubricObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        observation_id: ArtifactId,
        evaluator_id: ArtifactId,
        candidate_occurrence_id: ArtifactId,
        criterion_id: ArtifactId,
        outcome: CriterionOutcome,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, RubricError> {
        validate_evidence(&evidence)?;
        Ok(Self {
            observation_id,
            evaluator_id,
            candidate_occurrence_id,
            criterion_id,
            outcome,
            evidence,
        })
    }

    pub const fn observation_id(&self) -> ArtifactId {
        self.observation_id
    }

    pub const fn evaluator_id(&self) -> ArtifactId {
        self.evaluator_id
    }

    pub const fn candidate_occurrence_id(&self) -> ArtifactId {
        self.candidate_occurrence_id
    }

    pub const fn criterion_id(&self) -> ArtifactId {
        self.criterion_id
    }

    pub const fn outcome(&self) -> CriterionOutcome {
        self.outcome
    }

    pub fn evidence(&self) -> &[EvidenceRef] {
        &self.evidence
    }
}

impl<'de> Deserialize<'de> for RubricObservation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireObservation {
            observation_id: ArtifactId,
            evaluator_id: ArtifactId,
            candidate_occurrence_id: ArtifactId,
            criterion_id: ArtifactId,
            outcome: CriterionOutcome,
            evidence: Vec<EvidenceRef>,
        }

        let wire = WireObservation::deserialize(deserializer)?;
        Self::new(
            wire.observation_id,
            wire.evaluator_id,
            wire.candidate_occurrence_id,
            wire.criterion_id,
            wire.outcome,
            wire.evidence,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RubricScorecard {
    rubric_id: ArtifactId,
    evaluator_id: ArtifactId,
    candidate_occurrence_id: ArtifactId,
    observations: Vec<RubricObservation>,
    weighted_score: Option<UnitScore>,
    abstained_criterion_ids: Vec<ArtifactId>,
}

impl RubricScorecard {
    pub const fn rubric_id(&self) -> ArtifactId {
        self.rubric_id
    }

    pub const fn evaluator_id(&self) -> ArtifactId {
        self.evaluator_id
    }

    pub const fn candidate_occurrence_id(&self) -> ArtifactId {
        self.candidate_occurrence_id
    }

    pub fn observations(&self) -> &[RubricObservation] {
        &self.observations
    }

    /// `None` means at least one criterion was explicitly abstained from. An
    /// abstention is never converted to zero or silently reweighted away.
    pub const fn weighted_score(&self) -> Option<UnitScore> {
        self.weighted_score
    }

    pub fn abstained_criterion_ids(&self) -> &[ArtifactId] {
        &self.abstained_criterion_ids
    }
}

pub fn score_rubric(
    rubric: &Rubric,
    mut observations: Vec<RubricObservation>,
) -> Result<RubricScorecard, RubricError> {
    if observations.len() != rubric.criteria.len() {
        return Err(RubricError::ObservationCount {
            expected: rubric.criteria.len(),
            actual: observations.len(),
        });
    }

    let first = observations.first().ok_or(RubricError::ObservationCount {
        expected: rubric.criteria.len(),
        actual: 0,
    })?;
    let evaluator_id = first.evaluator_id;
    let candidate_occurrence_id = first.candidate_occurrence_id;

    let criteria_by_id: BTreeMap<_, _> = rubric
        .criteria
        .iter()
        .map(|criterion| (criterion.criterion_id, criterion.weight))
        .collect();
    let mut criterion_ids = BTreeSet::new();
    let mut observation_ids = BTreeSet::new();
    for observation in &observations {
        validate_observation_identity(observation, evaluator_id, candidate_occurrence_id)?;
        if !criteria_by_id.contains_key(&observation.criterion_id) {
            return Err(RubricError::UnknownCriterion(observation.criterion_id));
        }
        if !criterion_ids.insert(observation.criterion_id) {
            return Err(RubricError::DuplicateCriterionObservation(
                observation.criterion_id,
            ));
        }
        if !observation_ids.insert(observation.observation_id) {
            return Err(RubricError::DuplicateObservation(
                observation.observation_id,
            ));
        }
    }

    observations.sort_unstable_by_key(|observation| observation.criterion_id);
    let mut weighted_sum = 0_u64;
    let mut total_weight = 0_u64;
    let mut abstained_criterion_ids = Vec::new();
    for observation in &observations {
        let weight = u64::from(criteria_by_id[&observation.criterion_id].get());
        total_weight = total_weight
            .checked_add(weight)
            .ok_or(RubricError::ArithmeticOverflow)?;
        match observation.outcome {
            CriterionOutcome::Abstain => {
                abstained_criterion_ids.push(observation.criterion_id);
            }
            CriterionOutcome::Score(score) => {
                let weighted = weight
                    .checked_mul(u64::from(score.millionths()))
                    .ok_or(RubricError::ArithmeticOverflow)?;
                weighted_sum = weighted_sum
                    .checked_add(weighted)
                    .ok_or(RubricError::ArithmeticOverflow)?;
            }
        }
    }

    let weighted_score = if abstained_criterion_ids.is_empty() {
        let score = weighted_sum
            .checked_div(total_weight)
            .ok_or(RubricError::ArithmeticOverflow)?;
        Some(
            UnitScore::from_millionths(
                u32::try_from(score).map_err(|_| RubricError::ArithmeticOverflow)?,
            )
            .map_err(|_| RubricError::ArithmeticOverflow)?,
        )
    } else {
        None
    };

    Ok(RubricScorecard {
        rubric_id: rubric.rubric_id,
        evaluator_id,
        candidate_occurrence_id,
        observations,
        weighted_score,
        abstained_criterion_ids,
    })
}

fn validate_observation_identity(
    observation: &RubricObservation,
    evaluator_id: ArtifactId,
    candidate_occurrence_id: ArtifactId,
) -> Result<(), RubricError> {
    if observation.evaluator_id != evaluator_id {
        return Err(RubricError::MixedEvaluators {
            expected: evaluator_id,
            actual: observation.evaluator_id,
        });
    }
    if observation.candidate_occurrence_id != candidate_occurrence_id {
        return Err(RubricError::MixedCandidates {
            expected: candidate_occurrence_id,
            actual: observation.candidate_occurrence_id,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum RubricError {
    #[error("criterion weight {0} is outside 1..={MAX_CRITERION_WEIGHT}")]
    InvalidWeight(u32),
    #[error("rubric criterion count {0} is outside 1..={MAX_RUBRIC_CRITERIA}")]
    InvalidCriterionCount(usize),
    #[error("rubric criterion {0} occurs more than once")]
    DuplicateCriterion(ArtifactId),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error("expected {expected} rubric observations, found {actual}")]
    ObservationCount { expected: usize, actual: usize },
    #[error("rubric observations mix evaluators: expected {expected}, found {actual}")]
    MixedEvaluators {
        expected: ArtifactId,
        actual: ArtifactId,
    },
    #[error("rubric observations mix candidates: expected {expected}, found {actual}")]
    MixedCandidates {
        expected: ArtifactId,
        actual: ArtifactId,
    },
    #[error("rubric observation references unknown criterion {0}")]
    UnknownCriterion(ArtifactId),
    #[error("criterion {0} has more than one observation")]
    DuplicateCriterionObservation(ArtifactId),
    #[error("observation {0} occurs more than once")]
    DuplicateObservation(ArtifactId),
    #[error("rubric score arithmetic overflowed")]
    ArithmeticOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> Vec<EvidenceRef> {
        vec![EvidenceRef::artifact(ArtifactId::new())]
    }

    fn observation(
        evaluator_id: ArtifactId,
        candidate_id: ArtifactId,
        criterion_id: ArtifactId,
        score: u32,
    ) -> RubricObservation {
        RubricObservation::new(
            ArtifactId::new(),
            evaluator_id,
            candidate_id,
            criterion_id,
            CriterionOutcome::Score(UnitScore::from_millionths(score).expect("score")),
            evidence(),
        )
        .expect("observation")
    }

    #[test]
    fn integer_weighting_is_exact_and_permutation_invariant() {
        let light =
            RubricCriterion::new(ArtifactId::new(), CriterionWeight::new(1).expect("weight"));
        let heavy =
            RubricCriterion::new(ArtifactId::new(), CriterionWeight::new(3).expect("weight"));
        let rubric = Rubric::new(ArtifactId::new(), vec![heavy, light]).expect("rubric");
        let evaluator = ArtifactId::new();
        let candidate = ArtifactId::new();
        let first = observation(evaluator, candidate, light.criterion_id(), 200_000);
        let second = observation(evaluator, candidate, heavy.criterion_id(), 1_000_000);

        let forward =
            score_rubric(&rubric, vec![first.clone(), second.clone()]).expect("forward score");
        let reverse = score_rubric(&rubric, vec![second, first]).expect("reverse score");
        assert_eq!(forward, reverse);
        assert_eq!(
            forward.weighted_score(),
            Some(UnitScore::from_millionths(800_000).expect("score"))
        );
    }

    #[test]
    fn abstention_is_preserved_and_never_reweighted() {
        let criterion =
            RubricCriterion::new(ArtifactId::new(), CriterionWeight::new(1).expect("weight"));
        let rubric = Rubric::new(ArtifactId::new(), vec![criterion]).expect("rubric");
        let observation = RubricObservation::new(
            ArtifactId::new(),
            ArtifactId::new(),
            ArtifactId::new(),
            criterion.criterion_id(),
            CriterionOutcome::Abstain,
            evidence(),
        )
        .expect("observation");
        let scorecard = score_rubric(&rubric, vec![observation]).expect("scorecard");
        assert_eq!(scorecard.weighted_score(), None);
        assert_eq!(
            scorecard.abstained_criterion_ids(),
            &[criterion.criterion_id()]
        );
    }

    #[test]
    fn rejects_missing_and_duplicate_criterion_observations() {
        let first =
            RubricCriterion::new(ArtifactId::new(), CriterionWeight::new(1).expect("weight"));
        let second =
            RubricCriterion::new(ArtifactId::new(), CriterionWeight::new(1).expect("weight"));
        let rubric = Rubric::new(ArtifactId::new(), vec![first, second]).expect("rubric");
        let evaluator = ArtifactId::new();
        let candidate = ArtifactId::new();
        let observation = observation(evaluator, candidate, first.criterion_id(), 500_000);
        assert!(matches!(
            score_rubric(&rubric, vec![observation.clone()]),
            Err(RubricError::ObservationCount { .. })
        ));
        assert!(matches!(
            score_rubric(&rubric, vec![observation.clone(), observation]),
            Err(RubricError::DuplicateCriterionObservation(_))
        ));
    }

    #[test]
    fn deserialize_enforces_weight_and_unique_criteria() {
        let criterion_id = ArtifactId::new();
        let json = format!(
            r#"{{"rubric_id":"{}","criteria":[{{"criterion_id":"{}","weight":1}},{{"criterion_id":"{}","weight":1}}]}}"#,
            ArtifactId::new(),
            criterion_id,
            criterion_id
        );
        assert!(serde_json::from_str::<Rubric>(&json).is_err());
        assert!(serde_json::from_str::<CriterionWeight>("0").is_err());
    }

    #[test]
    fn observation_round_trip_revalidates_evidence() {
        let observation = observation(
            ArtifactId::new(),
            ArtifactId::new(),
            ArtifactId::new(),
            500_000,
        );
        let encoded = serde_json::to_string(&observation).expect("serialize");
        let decoded: RubricObservation = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, observation);
    }
}
