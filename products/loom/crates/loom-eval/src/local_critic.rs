//! Strict consumption of live, structured local-critic responses.

use std::fmt;

#[cfg(test)]
use loom_inference::local_critic::CriticConstraintKind;
use loom_inference::local_critic::{
    CriticWorkerLineage, VerifiedCriticResponse, VerifiedCriticResponseEvidence,
};
use loom_inference::{ControllerCandidateBinding, CriticEvaluationTask};
use loom_research_types::NonEmptyByteRange;
use loom_types::{ArtifactId, BlobId};
use serde::Serialize;
use thiserror::Error;

use crate::{
    BlindPairAssignment, CandidateEvidenceSource, ValidatedBlindPairJudgment,
    ValidatedCriterionObservation, parse_and_validate_blind_pair_judgment,
    parse_and_validate_criterion_claim,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalCriticEvidenceClass {
    LiveInProcessStructuredCritic,
}

#[derive(Clone, Copy)]
pub struct LocalCriterionContext<'a> {
    pub evaluation_attempt_id: ArtifactId,
    pub criterion_key: &'a str,
    pub candidate: CandidateEvidenceSource<'a>,
    pub candidate_range: NonEmptyByteRange,
    pub evaluation_packet_fingerprint: BlobId,
    pub rubric_fingerprint: BlobId,
    pub constraint_fingerprint: BlobId,
}

impl fmt::Debug for LocalCriterionContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalCriterionContext")
            .field("evaluation_attempt_id", &self.evaluation_attempt_id)
            .field(
                "criterion_key_blob",
                &BlobId::digest(self.criterion_key.as_bytes()),
            )
            .field("candidate", &self.candidate)
            .field("candidate_range", &self.candidate_range)
            .field(
                "evaluation_packet_fingerprint",
                &self.evaluation_packet_fingerprint,
            )
            .field("rubric_fingerprint", &self.rubric_fingerprint)
            .field("constraint_fingerprint", &self.constraint_fingerprint)
            .finish()
    }
}

#[derive(Clone, Copy)]
pub struct LocalBlindPairContext<'a> {
    pub evaluation_attempt_id: ArtifactId,
    pub assignment: &'a BlindPairAssignment,
    pub rubric_fingerprint: BlobId,
    pub constraint_fingerprint: BlobId,
}

impl fmt::Debug for LocalBlindPairContext<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalBlindPairContext")
            .field("evaluation_attempt_id", &self.evaluation_attempt_id)
            .field(
                "evaluation_packet_fingerprint",
                &self.assignment.packet().packet_fingerprint(),
            )
            .field("rubric_fingerprint", &self.rubric_fingerprint)
            .field("constraint_fingerprint", &self.constraint_fingerprint)
            .finish()
    }
}

pub struct LocalCriterionEvaluation {
    response: VerifiedCriticResponseEvidence,
    observation: ValidatedCriterionObservation,
    lineage: Option<CriticWorkerLineage>,
}

impl fmt::Debug for LocalCriterionEvaluation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalCriterionEvaluation")
            .field("source", &self.source())
            .field(
                "response_fingerprint",
                &self.response.verification_fingerprint(),
            )
            .field("observation", &self.observation)
            .field("has_worker_lineage", &self.lineage.is_some())
            .finish()
    }
}

impl LocalCriterionEvaluation {
    pub const fn source(&self) -> LocalCriticEvidenceClass {
        LocalCriticEvidenceClass::LiveInProcessStructuredCritic
    }

    pub const fn observation(&self) -> &ValidatedCriterionObservation {
        &self.observation
    }

    pub const fn response(&self) -> &VerifiedCriticResponseEvidence {
        &self.response
    }

    pub fn take_lineage(&mut self) -> Option<CriticWorkerLineage> {
        self.lineage.take()
    }
}

pub struct LocalBlindPairEvaluation {
    response: VerifiedCriticResponseEvidence,
    judgment: ValidatedBlindPairJudgment,
    lineage: Option<CriticWorkerLineage>,
}

impl fmt::Debug for LocalBlindPairEvaluation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalBlindPairEvaluation")
            .field("source", &self.source())
            .field(
                "response_fingerprint",
                &self.response.verification_fingerprint(),
            )
            .field("judgment", &self.judgment)
            .field("has_worker_lineage", &self.lineage.is_some())
            .finish()
    }
}

impl LocalBlindPairEvaluation {
    pub const fn source(&self) -> LocalCriticEvidenceClass {
        LocalCriticEvidenceClass::LiveInProcessStructuredCritic
    }

    pub const fn judgment(&self) -> &ValidatedBlindPairJudgment {
        &self.judgment
    }

    pub const fn response(&self) -> &VerifiedCriticResponseEvidence {
        &self.response
    }

    pub fn take_lineage(&mut self) -> Option<CriticWorkerLineage> {
        self.lineage.take()
    }
}

pub fn consume_local_criterion_response(
    response: VerifiedCriticResponse,
    context: LocalCriterionContext<'_>,
) -> Result<LocalCriterionEvaluation, LocalCriticAdapterError> {
    validate_candidate_bytes(context.candidate, context.candidate_range)?;
    validate_criterion_binding(response.evidence(), context)?;
    let (response, lineage) = response.into_parts();
    let observation = parse_and_validate_criterion_claim(
        context.criterion_key,
        context.candidate,
        response.raw_output(),
    );
    Ok(LocalCriterionEvaluation {
        response,
        observation,
        lineage,
    })
}

pub fn consume_local_blind_pair_response(
    response: VerifiedCriticResponse,
    context: LocalBlindPairContext<'_>,
) -> Result<LocalBlindPairEvaluation, LocalCriticAdapterError> {
    validate_blind_binding(response.evidence(), &context)?;
    let (response, lineage) = response.into_parts();
    let judgment =
        parse_and_validate_blind_pair_judgment(context.assignment, response.raw_output());
    Ok(LocalBlindPairEvaluation {
        response,
        judgment,
        lineage,
    })
}

fn validate_candidate_bytes(
    source: CandidateEvidenceSource<'_>,
    range: NonEmptyByteRange,
) -> Result<(), LocalCriticAdapterError> {
    if std::str::from_utf8(source.utf8).is_err()
        || BlobId::digest(source.utf8) != source.blob_id
        || range.checked_str(source.utf8).is_err()
    {
        return Err(LocalCriticAdapterError::CandidateBindingMismatch);
    }
    Ok(())
}

fn validate_criterion_binding(
    response: &VerifiedCriticResponseEvidence,
    context: LocalCriterionContext<'_>,
) -> Result<(), LocalCriticAdapterError> {
    let CriticEvaluationTask::Criterion { criterion_key } = response.prompt().task() else {
        return Err(LocalCriticAdapterError::TaskMismatch);
    };
    if criterion_key != context.criterion_key
        || response.prompt().specification().evaluation_attempt_id()
            != context.evaluation_attempt_id
        || response.prompt().evaluation_packet_fingerprint()
            != context.evaluation_packet_fingerprint
        || response.prompt().rubric_fingerprint() != context.rubric_fingerprint
        || response.constraint_fingerprint() != context.constraint_fingerprint
        || response.prompt().candidates().len() != 1
    {
        return Err(LocalCriticAdapterError::PromptBindingMismatch);
    }
    let expected = CandidateBindingFact {
        occurrence_id: context.candidate.occurrence_id,
        blob_id: context.candidate.blob_id,
        range: context.candidate_range,
    };
    validate_candidate_binding(response.prompt().candidates()[0], expected)
}

fn validate_blind_binding(
    response: &VerifiedCriticResponseEvidence,
    context: &LocalBlindPairContext<'_>,
) -> Result<(), LocalCriticAdapterError> {
    if response.prompt().task() != &CriticEvaluationTask::BlindPair {
        return Err(LocalCriticAdapterError::TaskMismatch);
    }
    if response.prompt().evaluation_packet_fingerprint()
        != context.assignment.packet().packet_fingerprint()
        || response.prompt().specification().evaluation_attempt_id()
            != context.evaluation_attempt_id
        || response.prompt().rubric_fingerprint() != context.rubric_fingerprint
        || response.constraint_fingerprint() != context.constraint_fingerprint
        || response.prompt().candidates().len() != 2
    {
        return Err(LocalCriticAdapterError::PromptBindingMismatch);
    }
    let expected_ids = context.assignment.ordered_candidate_bindings();
    let packet_candidates = [
        context.assignment.packet().first(),
        context.assignment.packet().second(),
    ];
    for (index, packet) in packet_candidates.into_iter().enumerate() {
        let bytes = packet.text().as_bytes();
        let range = NonEmptyByteRange::new(0, bytes.len() as u64)
            .map_err(|_| LocalCriticAdapterError::CandidateBindingMismatch)?;
        let expected = CandidateBindingFact {
            occurrence_id: expected_ids[index].0,
            blob_id: expected_ids[index].1,
            range,
        };
        if BlobId::digest(bytes) != expected.blob_id {
            return Err(LocalCriticAdapterError::CandidateBindingMismatch);
        }
        validate_candidate_binding(response.prompt().candidates()[index], expected)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct CandidateBindingFact {
    occurrence_id: ArtifactId,
    blob_id: BlobId,
    range: NonEmptyByteRange,
}

fn validate_candidate_binding(
    actual: ControllerCandidateBinding,
    expected: CandidateBindingFact,
) -> Result<(), LocalCriticAdapterError> {
    validate_candidate_binding_facts(
        CandidateBindingFact {
            occurrence_id: actual.occurrence_id(),
            blob_id: actual.blob_id(),
            range: actual.range(),
        },
        expected,
    )
}

fn validate_candidate_binding_facts(
    actual: CandidateBindingFact,
    expected: CandidateBindingFact,
) -> Result<(), LocalCriticAdapterError> {
    if actual.occurrence_id != expected.occurrence_id
        || actual.blob_id != expected.blob_id
        || actual.range != expected.range
    {
        return Err(LocalCriticAdapterError::CandidateBindingMismatch);
    }
    Ok(())
}

/// Generic closed schema for a criterion card. Evidence remains untrusted
/// until the adapter checks every range and quote against candidate bytes.
pub fn criterion_claim_json_schema() -> String {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["criterion_key", "outcome", "evidence"],
        "properties": {
            "criterion_key": {"type": "string", "minLength": 1, "maxLength": 128},
            "outcome": {
                "type": "object",
                "additionalProperties": false,
                "required": ["outcome"],
                "properties": {
                    "outcome": {"type": "string", "enum": ["abstain", "score"]},
                    "score": {"type": "integer", "minimum": 0, "maximum": 1_000_000}
                }
            },
            "evidence": {
                "type": "array",
                "maxItems": 64,
                "items": evidence_span_schema()
            }
        }
    })
    .to_string()
}

pub fn blind_pair_claim_json_schema() -> String {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "evidence"],
        "properties": {
            "verdict": {
                "type": "object",
                "additionalProperties": false,
                "required": ["verdict"],
                "properties": {
                    "verdict": {"type": "string", "enum": ["winner", "tie", "abstain"]},
                    "winner_label": {"type": "string"}
                }
            },
            "evidence": {
                "type": "array",
                "maxItems": 128,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["label", "range", "quote"],
                    "properties": {
                        "label": {"type": "string", "minLength": 12, "maxLength": 12},
                        "range": range_schema(),
                        "quote": {"type": "string", "minLength": 1, "maxLength": 16384}
                    }
                }
            }
        }
    })
    .to_string()
}

fn evidence_span_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["candidate_occurrence_id", "candidate_blob_id", "range", "quote"],
        "properties": {
            "candidate_occurrence_id": {"type": "string"},
            "candidate_blob_id": {"type": "string", "minLength": 64, "maxLength": 64},
            "range": range_schema(),
            "quote": {"type": "string", "minLength": 1, "maxLength": 16384}
        }
    })
}

fn range_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["start", "end"],
        "properties": {
            "start": {"type": "integer", "minimum": 0},
            "end": {"type": "integer", "minimum": 1}
        }
    })
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum LocalCriticAdapterError {
    #[error("verified critic response has the wrong evaluation task")]
    TaskMismatch,
    #[error("verified critic prompt, rubric, packet, or constraint binding changed")]
    PromptBindingMismatch,
    #[error("verified critic candidate occurrence, blob, range, or bytes changed")]
    CandidateBindingMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_text_under_another_occurrence_is_not_the_same_candidate() {
        let bytes = b"Exact candidate text.";
        let actual = CandidateBindingFact {
            occurrence_id: ArtifactId::new(),
            blob_id: BlobId::digest(bytes),
            range: NonEmptyByteRange::new(0, bytes.len() as u64).expect("range"),
        };
        let expected = CandidateBindingFact {
            occurrence_id: ArtifactId::new(),
            blob_id: BlobId::digest(bytes),
            range: NonEmptyByteRange::new(0, bytes.len() as u64).expect("range"),
        };
        assert_eq!(
            validate_candidate_binding_facts(actual, expected),
            Err(LocalCriticAdapterError::CandidateBindingMismatch)
        );
    }

    #[test]
    fn schemas_are_closed_at_the_root_and_evidence_levels() {
        for schema in [
            criterion_claim_json_schema(),
            blind_pair_claim_json_schema(),
        ] {
            let value: serde_json::Value = serde_json::from_str(&schema).expect("schema JSON");
            assert_eq!(value["additionalProperties"], false);
            assert!(schema.contains("\"additionalProperties\":false"));
        }
    }

    #[test]
    fn malformed_and_extra_criterion_json_abstain_without_repair() {
        let bytes = b"Candidate sentence.";
        let source = CandidateEvidenceSource {
            occurrence_id: ArtifactId::new(),
            blob_id: BlobId::digest(bytes),
            utf8: bytes,
        };
        for malformed in [
            br#"{"criterion_key":"continuity","outcome":{"outcome":"score","score":500000},"evidence":[],"extra":true}"#.as_slice(),
            br#"{"criterion_key":"continuity"}"#.as_slice(),
            b"not-json".as_slice(),
        ] {
            assert!(matches!(
                parse_and_validate_criterion_claim("continuity", source, malformed),
                ValidatedCriterionObservation::Abstained { .. }
            ));
        }
    }

    #[test]
    fn constraint_kind_is_a_closed_local_critic_fact() {
        assert_ne!(CriticConstraintKind::JsonSchema, CriticConstraintKind::Gbnf);
    }
}
