use std::{collections::BTreeSet, fmt};

use loom_research_types::NonEmptyByteRange;
use loom_search::UnitScore;
use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

pub const MAX_EVALUATION_QUOTE_BYTES: usize = 16 * 1024;
pub const MAX_EVIDENCE_SPANS_PER_OBSERVATION: usize = 64;

/// Bounded exact text quoted by a judge.
///
/// Newlines and tabs are ordinary literary evidence. NUL and other control
/// bytes are rejected because they cannot occur in a normal JSON quotation and
/// make review packets ambiguous.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct EvidenceQuote(String);

impl EvidenceQuote {
    pub fn new(value: impl Into<String>) -> Result<Self, EvidenceValidationError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_EVALUATION_QUOTE_BYTES {
            return Err(EvidenceValidationError::InvalidQuoteLength(value.len()));
        }
        if value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(EvidenceValidationError::ProhibitedQuoteControl);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for EvidenceQuote {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvidenceQuote")
            .field("byte_len", &self.0.len())
            .field("blob_id", &BlobId::digest(self.0.as_bytes()))
            .finish()
    }
}

impl<'de> Deserialize<'de> for EvidenceQuote {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Untrusted evidence returned by a judge.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpanClaim {
    pub candidate_occurrence_id: ArtifactId,
    pub candidate_blob_id: BlobId,
    pub range: NonEmptyByteRange,
    pub quote: EvidenceQuote,
}

/// A quotation checked byte-for-byte against one exact candidate.
///
/// It is reconstructible data, not a live capability. Deserialization is
/// intentionally absent because candidate bytes are required to validate it.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct ValidatedEvidenceSpan {
    candidate_occurrence_id: ArtifactId,
    candidate_blob_id: BlobId,
    range: NonEmptyByteRange,
    quote_blob_id: BlobId,
}

impl ValidatedEvidenceSpan {
    #[cfg(test)]
    pub(crate) const fn for_test(
        candidate_occurrence_id: ArtifactId,
        candidate_blob_id: BlobId,
        range: NonEmptyByteRange,
        quote_blob_id: BlobId,
    ) -> Self {
        Self {
            candidate_occurrence_id,
            candidate_blob_id,
            range,
            quote_blob_id,
        }
    }

    pub const fn candidate_occurrence_id(&self) -> ArtifactId {
        self.candidate_occurrence_id
    }

    pub const fn candidate_blob_id(&self) -> BlobId {
        self.candidate_blob_id
    }

    pub const fn range(&self) -> NonEmptyByteRange {
        self.range
    }

    pub const fn quote_blob_id(&self) -> BlobId {
        self.quote_blob_id
    }
}

/// Exact bytes and occurrence identity supplied by the harness, never by a
/// judge response.
#[derive(Clone, Copy)]
pub struct CandidateEvidenceSource<'a> {
    pub occurrence_id: ArtifactId,
    pub blob_id: BlobId,
    pub utf8: &'a [u8],
}

impl fmt::Debug for CandidateEvidenceSource<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CandidateEvidenceSource")
            .field("occurrence_id", &self.occurrence_id)
            .field("blob_id", &self.blob_id)
            .field("byte_len", &self.utf8.len())
            .finish()
    }
}

pub fn validate_evidence_spans(
    source: CandidateEvidenceSource<'_>,
    claims: &[EvidenceSpanClaim],
) -> Result<Vec<ValidatedEvidenceSpan>, EvidenceValidationError> {
    if claims.is_empty() || claims.len() > MAX_EVIDENCE_SPANS_PER_OBSERVATION {
        return Err(EvidenceValidationError::InvalidSpanCount(claims.len()));
    }
    if std::str::from_utf8(source.utf8).is_err() {
        return Err(EvidenceValidationError::CandidateNotUtf8);
    }
    if BlobId::digest(source.utf8) != source.blob_id {
        return Err(EvidenceValidationError::CandidateBlobMismatch);
    }

    let mut unique = BTreeSet::new();
    let mut validated = Vec::with_capacity(claims.len());
    for (index, claim) in claims.iter().enumerate() {
        if claim.candidate_occurrence_id != source.occurrence_id {
            return Err(EvidenceValidationError::OccurrenceMismatch { index });
        }
        if claim.candidate_blob_id != source.blob_id {
            return Err(EvidenceValidationError::EvidenceBlobMismatch { index });
        }
        let quoted = claim
            .range
            .checked_str(source.utf8)
            .map_err(|_| EvidenceValidationError::InvalidRange { index })?;
        if quoted.as_bytes() != claim.quote.as_str().as_bytes() {
            return Err(EvidenceValidationError::QuotationMismatch { index });
        }
        let quote_blob_id = BlobId::digest(claim.quote.as_str().as_bytes());
        let key = (claim.range.start(), claim.range.end(), quote_blob_id);
        if !unique.insert(key) {
            return Err(EvidenceValidationError::DuplicateSpan { index });
        }
        validated.push(ValidatedEvidenceSpan {
            candidate_occurrence_id: source.occurrence_id,
            candidate_blob_id: source.blob_id,
            range: claim.range,
            quote_blob_id,
        });
    }
    Ok(validated)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "score")]
pub enum CriterionClaimOutcome {
    Abstain,
    Score(UnitScore),
}

/// Closed judge payload after structured parsing but before evidence checking.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionObservationClaim {
    pub criterion_key: String,
    pub outcome: CriterionClaimOutcome,
    pub evidence: Vec<EvidenceSpanClaim>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationAbstention {
    Explicit,
    PromptInjectionSuspected,
    CriterionMismatch,
    InvalidEvidence,
    MalformedStructuredResponse,
}

/// Evidence-checked criterion result. Invalid evidence is an abstention; the
/// harness never searches for, rewrites, or relocates the judge's quotation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "disposition")]
pub enum ValidatedCriterionObservation {
    Scored {
        criterion_key: String,
        score: UnitScore,
        evidence: Vec<ValidatedEvidenceSpan>,
    },
    Abstained {
        expected_criterion_key: String,
        reason: EvaluationAbstention,
    },
}

pub fn validate_criterion_claim(
    expected_criterion_key: &str,
    source: CandidateEvidenceSource<'_>,
    claim: &CriterionObservationClaim,
) -> ValidatedCriterionObservation {
    if claim.criterion_key != expected_criterion_key {
        return ValidatedCriterionObservation::Abstained {
            expected_criterion_key: expected_criterion_key.to_owned(),
            reason: EvaluationAbstention::CriterionMismatch,
        };
    }
    let CriterionClaimOutcome::Score(score) = claim.outcome else {
        return ValidatedCriterionObservation::Abstained {
            expected_criterion_key: expected_criterion_key.to_owned(),
            reason: EvaluationAbstention::Explicit,
        };
    };
    match validate_evidence_spans(source, &claim.evidence) {
        Ok(evidence) => ValidatedCriterionObservation::Scored {
            criterion_key: expected_criterion_key.to_owned(),
            score,
            evidence,
        },
        Err(_) => ValidatedCriterionObservation::Abstained {
            expected_criterion_key: expected_criterion_key.to_owned(),
            reason: EvaluationAbstention::InvalidEvidence,
        },
    }
}

/// Turns malformed JSON into an explicit abstention without attempting a
/// tolerant parse or repairing any field.
pub fn parse_and_validate_criterion_claim(
    expected_criterion_key: &str,
    source: CandidateEvidenceSource<'_>,
    response_json: &[u8],
) -> ValidatedCriterionObservation {
    let Ok(claim) = serde_json::from_slice::<CriterionObservationClaim>(response_json) else {
        return ValidatedCriterionObservation::Abstained {
            expected_criterion_key: expected_criterion_key.to_owned(),
            reason: EvaluationAbstention::MalformedStructuredResponse,
        };
    };
    validate_criterion_claim(expected_criterion_key, source, &claim)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EvidenceValidationError {
    #[error("evidence quote length {0} is outside 1..={MAX_EVALUATION_QUOTE_BYTES}")]
    InvalidQuoteLength(usize),
    #[error("evidence quote contains a prohibited control character")]
    ProhibitedQuoteControl,
    #[error("evidence span count {0} is outside 1..={MAX_EVIDENCE_SPANS_PER_OBSERVATION}")]
    InvalidSpanCount(usize),
    #[error("candidate bytes are not UTF-8")]
    CandidateNotUtf8,
    #[error("candidate bytes differ from their blob ID")]
    CandidateBlobMismatch,
    #[error("evidence span {index} references another occurrence")]
    OccurrenceMismatch { index: usize },
    #[error("evidence span {index} references another candidate blob")]
    EvidenceBlobMismatch { index: usize },
    #[error("evidence span {index} is not a valid UTF-8 range")]
    InvalidRange { index: usize },
    #[error("evidence span {index} quotation differs from exact candidate bytes")]
    QuotationMismatch { index: usize },
    #[error("evidence span {index} duplicates an earlier span")]
    DuplicateSpan { index: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(bytes: &[u8], occurrence_id: ArtifactId) -> CandidateEvidenceSource<'_> {
        CandidateEvidenceSource {
            occurrence_id,
            blob_id: BlobId::digest(bytes),
            utf8: bytes,
        }
    }

    fn claim(
        bytes: &[u8],
        occurrence_id: ArtifactId,
        start: u64,
        end: u64,
        quote: &str,
    ) -> EvidenceSpanClaim {
        EvidenceSpanClaim {
            candidate_occurrence_id: occurrence_id,
            candidate_blob_id: BlobId::digest(bytes),
            range: NonEmptyByteRange::new(start, end).expect("fixture range"),
            quote: EvidenceQuote::new(quote).expect("fixture quote"),
        }
    }

    #[test]
    fn exact_quote_and_utf8_boundaries_are_required() {
        let bytes = "Mara touched the door.\nIt answered.".as_bytes();
        let occurrence = ArtifactId::new();
        let exact = claim(bytes, occurrence, 0, 4, "Mara");
        let checked =
            validate_evidence_spans(source(bytes, occurrence), &[exact]).expect("exact evidence");
        assert_eq!(checked[0].range(), NonEmptyByteRange::new(0, 4).unwrap());

        let altered = claim(bytes, occurrence, 0, 4, "Mary");
        assert!(matches!(
            validate_evidence_spans(source(bytes, occurrence), &[altered]),
            Err(EvidenceValidationError::QuotationMismatch { index: 0 })
        ));
    }

    #[test]
    fn malformed_or_inexact_judge_output_becomes_abstention() {
        let bytes = b"The key turned.";
        let occurrence = ArtifactId::new();
        let malformed = parse_and_validate_criterion_claim(
            "continuity",
            source(bytes, occurrence),
            br#"{"criterion_key":"continuity"}"#,
        );
        assert!(matches!(
            malformed,
            ValidatedCriterionObservation::Abstained {
                reason: EvaluationAbstention::MalformedStructuredResponse,
                ..
            }
        ));

        let inexact = CriterionObservationClaim {
            criterion_key: "continuity".into(),
            outcome: CriterionClaimOutcome::Score(
                UnitScore::from_millionths(800_000).expect("score"),
            ),
            evidence: vec![claim(bytes, occurrence, 0, 3, "Tea")],
        };
        assert!(matches!(
            validate_criterion_claim("continuity", source(bytes, occurrence), &inexact),
            ValidatedCriterionObservation::Abstained {
                reason: EvaluationAbstention::InvalidEvidence,
                ..
            }
        ));
    }

    #[test]
    fn cross_candidate_evidence_is_rejected() {
        let bytes = b"A clean sentence.";
        let occurrence = ArtifactId::new();
        let mut evidence = claim(bytes, occurrence, 0, 1, "A");
        evidence.candidate_occurrence_id = ArtifactId::new();
        assert!(matches!(
            validate_evidence_spans(source(bytes, occurrence), &[evidence]),
            Err(EvidenceValidationError::OccurrenceMismatch { index: 0 })
        ));
    }
}
