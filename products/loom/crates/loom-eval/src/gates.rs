use std::collections::BTreeSet;

use loom_types::{ArtifactId, BlobId};
use serde::Serialize;
use thiserror::Error;

use crate::ValidatedEvidenceSpan;

pub const CORE_HARD_GATE_COUNT: usize = 5;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreHardGate {
    Provenance,
    Assembly,
    Format,
    StoryState,
    AntiCopy,
}

impl CoreHardGate {
    pub const ALL: [Self; CORE_HARD_GATE_COUNT] = [
        Self::Provenance,
        Self::Assembly,
        Self::Format,
        Self::StoryState,
        Self::AntiCopy,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HardGateOutcome {
    Pass,
    Fail,
    Abstain,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HardGateEvidence {
    Receipt {
        artifact_id: ArtifactId,
        blob_id: BlobId,
    },
    CandidateSpans {
        spans: Vec<ValidatedEvidenceSpan>,
    },
}

impl HardGateEvidence {
    fn is_empty(&self) -> bool {
        match self {
            Self::Receipt { .. } => false,
            Self::CandidateSpans { spans } => spans.is_empty(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CoreHardGateObservation {
    observation_id: ArtifactId,
    candidate_occurrence_id: ArtifactId,
    gate: CoreHardGate,
    outcome: HardGateOutcome,
    evidence: HardGateEvidence,
}

impl CoreHardGateObservation {
    pub fn new(
        observation_id: ArtifactId,
        candidate_occurrence_id: ArtifactId,
        gate: CoreHardGate,
        outcome: HardGateOutcome,
        evidence: HardGateEvidence,
    ) -> Result<Self, HardGateError> {
        if evidence.is_empty() {
            return Err(HardGateError::EmptyEvidence);
        }
        if let HardGateEvidence::CandidateSpans { spans } = &evidence
            && spans
                .iter()
                .any(|span| span.candidate_occurrence_id() != candidate_occurrence_id)
        {
            return Err(HardGateError::MixedCandidateEvidence);
        }
        Ok(Self {
            observation_id,
            candidate_occurrence_id,
            gate,
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

    pub const fn gate(&self) -> CoreHardGate {
        self.gate
    }

    pub const fn outcome(&self) -> HardGateOutcome {
        self.outcome
    }

    pub const fn evidence(&self) -> &HardGateEvidence {
        &self.evidence
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Outcome of reducing caller-supplied hard-gate observations.
///
/// This is deliberately diagnostic. `AllClaimsPass` means only that the
/// supplied observation set was complete and every supplied outcome was
/// `Pass`; it is not a campaign, archive, benchmark, or promotion capability.
pub enum DiagnosticCoreHardGateDecision {
    AllClaimsPass,
    Rejected,
    Abstained,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
/// Replayable reduction of a complete set of hard-gate observations.
///
/// The observations may have been produced by a trusted live verifier, but
/// this value does not retain that verifier's affine authority. A persisted or
/// reconstructed report therefore cannot qualify a candidate.
pub struct DiagnosticCoreHardGateReport {
    candidate_occurrence_id: ArtifactId,
    decision: DiagnosticCoreHardGateDecision,
    observations: Vec<CoreHardGateObservation>,
}

impl DiagnosticCoreHardGateReport {
    pub const fn candidate_occurrence_id(&self) -> ArtifactId {
        self.candidate_occurrence_id
    }

    pub const fn decision(&self) -> DiagnosticCoreHardGateDecision {
        self.decision
    }

    pub fn observations(&self) -> &[CoreHardGateObservation] {
        &self.observations
    }
}

/// Reduce a complete observation set for inspection and persistence.
///
/// The function name and return type intentionally say `diagnostic`: public
/// observation constructors are useful for importing evaluator claims, but
/// they are not a source of live verification authority.
pub fn evaluate_diagnostic_core_hard_gates(
    mut observations: Vec<CoreHardGateObservation>,
) -> Result<DiagnosticCoreHardGateReport, HardGateError> {
    if observations.len() != CORE_HARD_GATE_COUNT {
        return Err(HardGateError::IncompleteCoreGateSet(observations.len()));
    }
    let candidate = observations[0].candidate_occurrence_id;
    let mut observation_ids = BTreeSet::new();
    let mut gates = BTreeSet::new();
    for observation in &observations {
        if observation.candidate_occurrence_id != candidate {
            return Err(HardGateError::MixedCandidates);
        }
        if !observation_ids.insert(observation.observation_id) {
            return Err(HardGateError::DuplicateObservation);
        }
        if !gates.insert(observation.gate) {
            return Err(HardGateError::DuplicateGate);
        }
    }
    if gates != CoreHardGate::ALL.into_iter().collect() {
        return Err(HardGateError::IncompleteCoreGateSet(gates.len()));
    }
    observations.sort_unstable_by_key(CoreHardGateObservation::gate);
    let decision = if observations
        .iter()
        .any(|observation| observation.outcome == HardGateOutcome::Fail)
    {
        DiagnosticCoreHardGateDecision::Rejected
    } else if observations
        .iter()
        .any(|observation| observation.outcome == HardGateOutcome::Abstain)
    {
        DiagnosticCoreHardGateDecision::Abstained
    } else {
        DiagnosticCoreHardGateDecision::AllClaimsPass
    };
    Ok(DiagnosticCoreHardGateReport {
        candidate_occurrence_id: candidate,
        decision,
        observations,
    })
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum HardGateError {
    #[error("hard-gate evidence cannot be empty")]
    EmptyEvidence,
    #[error("hard-gate text evidence cites another candidate")]
    MixedCandidateEvidence,
    #[error("core hard-gate set has {0} entries instead of exactly {CORE_HARD_GATE_COUNT}")]
    IncompleteCoreGateSet(usize),
    #[error("core hard-gate observations reference different candidates")]
    MixedCandidates,
    #[error("core hard-gate set repeats an observation ID")]
    DuplicateObservation,
    #[error("core hard-gate set repeats a gate")]
    DuplicateGate,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(
        candidate: ArtifactId,
        gate: CoreHardGate,
        outcome: HardGateOutcome,
    ) -> CoreHardGateObservation {
        CoreHardGateObservation::new(
            ArtifactId::new(),
            candidate,
            gate,
            outcome,
            HardGateEvidence::Receipt {
                artifact_id: ArtifactId::new(),
                blob_id: BlobId::digest(b"receipt"),
            },
        )
        .expect("observation")
    }

    #[test]
    fn complete_all_pass_set_is_only_a_diagnostic_claim() {
        let candidate = ArtifactId::new();
        let report = evaluate_diagnostic_core_hard_gates(
            CoreHardGate::ALL
                .into_iter()
                .map(|gate| observation(candidate, gate, HardGateOutcome::Pass))
                .collect(),
        )
        .expect("complete gates");
        assert_eq!(
            report.decision(),
            DiagnosticCoreHardGateDecision::AllClaimsPass
        );
    }

    #[test]
    fn failure_dominates_abstention_and_missing_gate_never_passes() {
        let candidate = ArtifactId::new();
        let mut observations = CoreHardGate::ALL
            .into_iter()
            .map(|gate| observation(candidate, gate, HardGateOutcome::Pass))
            .collect::<Vec<_>>();
        observations[0] = observation(
            candidate,
            CoreHardGate::Provenance,
            HardGateOutcome::Abstain,
        );
        observations[1] = observation(candidate, CoreHardGate::Assembly, HardGateOutcome::Fail);
        assert_eq!(
            evaluate_diagnostic_core_hard_gates(observations)
                .expect("complete gates")
                .decision(),
            DiagnosticCoreHardGateDecision::Rejected
        );

        assert!(matches!(
            evaluate_diagnostic_core_hard_gates(vec![observation(
                candidate,
                CoreHardGate::Provenance,
                HardGateOutcome::Pass
            )]),
            Err(HardGateError::IncompleteCoreGateSet(1))
        ));
    }
}
