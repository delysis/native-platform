use std::collections::BTreeSet;

use loom_types::ArtifactId;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{EvidenceError, EvidenceRef, validate_evidence};

pub const MAX_HARD_GATES: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    Abstain,
    Fail,
    Pass,
}

/// One immutable gate observation. Evidence is mandatory even for abstention,
/// so "unknown" remains inspectable rather than becoming a silent pass.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HardGateObservation {
    observation_id: ArtifactId,
    candidate_occurrence_id: ArtifactId,
    gate_id: ArtifactId,
    outcome: GateOutcome,
    evidence: Vec<EvidenceRef>,
}

impl HardGateObservation {
    pub fn new(
        observation_id: ArtifactId,
        candidate_occurrence_id: ArtifactId,
        gate_id: ArtifactId,
        outcome: GateOutcome,
        evidence: Vec<EvidenceRef>,
    ) -> Result<Self, ValidationError> {
        validate_evidence(&evidence)?;
        Ok(Self {
            observation_id,
            candidate_occurrence_id,
            gate_id,
            outcome,
            evidence,
        })
    }

    pub const fn observation_id(&self) -> ArtifactId {
        self.observation_id
    }

    pub const fn candidate_occurrence_id(&self) -> ArtifactId {
        self.candidate_occurrence_id
    }

    pub const fn gate_id(&self) -> ArtifactId {
        self.gate_id
    }

    pub const fn outcome(&self) -> GateOutcome {
        self.outcome
    }

    pub fn evidence(&self) -> &[EvidenceRef] {
        &self.evidence
    }
}

impl<'de> Deserialize<'de> for HardGateObservation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireObservation {
            observation_id: ArtifactId,
            candidate_occurrence_id: ArtifactId,
            gate_id: ArtifactId,
            outcome: GateOutcome,
            evidence: Vec<EvidenceRef>,
        }

        let wire = WireObservation::deserialize(deserializer)?;
        Self::new(
            wire.observation_id,
            wire.candidate_occurrence_id,
            wire.gate_id,
            wire.outcome,
            wire.evidence,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HardValidationDecision {
    Abstained,
    Eligible,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HardValidationReport {
    candidate_occurrence_id: ArtifactId,
    decision: HardValidationDecision,
    observations: Vec<HardGateObservation>,
}

impl HardValidationReport {
    pub const fn candidate_occurrence_id(&self) -> ArtifactId {
        self.candidate_occurrence_id
    }

    pub const fn decision(&self) -> HardValidationDecision {
        self.decision
    }

    pub fn observations(&self) -> &[HardGateObservation] {
        &self.observations
    }
}

pub fn aggregate_hard_gates(
    mut observations: Vec<HardGateObservation>,
) -> Result<HardValidationReport, ValidationError> {
    if observations.is_empty() || observations.len() > MAX_HARD_GATES {
        return Err(ValidationError::InvalidGateCount(observations.len()));
    }

    let candidate = observations[0].candidate_occurrence_id;
    let mut gate_ids = BTreeSet::new();
    let mut observation_ids = BTreeSet::new();
    for observation in &observations {
        if observation.candidate_occurrence_id != candidate {
            return Err(ValidationError::MixedCandidates {
                expected: candidate,
                actual: observation.candidate_occurrence_id,
            });
        }
        if !gate_ids.insert(observation.gate_id) {
            return Err(ValidationError::DuplicateGate(observation.gate_id));
        }
        if !observation_ids.insert(observation.observation_id) {
            return Err(ValidationError::DuplicateObservation(
                observation.observation_id,
            ));
        }
    }

    observations.sort_by_key(|observation| observation.gate_id);
    let decision = if observations
        .iter()
        .any(|observation| observation.outcome == GateOutcome::Fail)
    {
        HardValidationDecision::Rejected
    } else if observations
        .iter()
        .any(|observation| observation.outcome == GateOutcome::Abstain)
    {
        HardValidationDecision::Abstained
    } else {
        HardValidationDecision::Eligible
    };

    Ok(HardValidationReport {
        candidate_occurrence_id: candidate,
        decision,
        observations,
    })
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ValidationError {
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error("hard-gate count {0} is outside 1..={MAX_HARD_GATES}")]
    InvalidGateCount(usize),
    #[error("gate observations mix candidates: expected {expected}, found {actual}")]
    MixedCandidates {
        expected: ArtifactId,
        actual: ArtifactId,
    },
    #[error("gate {0} occurs more than once")]
    DuplicateGate(ArtifactId),
    #[error("observation {0} occurs more than once")]
    DuplicateObservation(ArtifactId),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(candidate: ArtifactId, outcome: GateOutcome) -> HardGateObservation {
        HardGateObservation::new(
            ArtifactId::new(),
            candidate,
            ArtifactId::new(),
            outcome,
            vec![EvidenceRef::artifact(ArtifactId::new())],
        )
        .expect("valid observation")
    }

    #[test]
    fn abstention_is_not_treated_as_a_pass() {
        let candidate = ArtifactId::new();
        let report = aggregate_hard_gates(vec![
            observation(candidate, GateOutcome::Pass),
            observation(candidate, GateOutcome::Abstain),
        ])
        .expect("valid report");
        assert_eq!(report.decision(), HardValidationDecision::Abstained);
    }

    #[test]
    fn failure_dominates_abstention_and_preserves_both() {
        let candidate = ArtifactId::new();
        let report = aggregate_hard_gates(vec![
            observation(candidate, GateOutcome::Abstain),
            observation(candidate, GateOutcome::Fail),
        ])
        .expect("valid report");
        assert_eq!(report.decision(), HardValidationDecision::Rejected);
        assert_eq!(report.observations().len(), 2);
    }

    #[test]
    fn observations_require_inspectable_evidence() {
        assert!(matches!(
            HardGateObservation::new(
                ArtifactId::new(),
                ArtifactId::new(),
                ArtifactId::new(),
                GateOutcome::Abstain,
                Vec::new(),
            ),
            Err(ValidationError::Evidence(EvidenceError::InvalidCount(0)))
        ));
    }

    #[test]
    fn observation_round_trip_revalidates_evidence() {
        let observation = observation(ArtifactId::new(), GateOutcome::Pass);
        let encoded = serde_json::to_string(&observation).expect("serialize");
        let decoded: HardGateObservation = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, observation);
    }
}
