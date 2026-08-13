use std::{collections::BTreeSet, fmt};

use loom_research_types::NonEmptyByteRange;
use loom_types::{ArtifactId, BlobId};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    CandidateEvidenceSource, EvidenceQuote, EvidenceSpanClaim, ValidatedEvidenceSpan,
    validate_evidence_spans,
};

pub const MAX_BLIND_CRITERIA: usize = 128;
pub const MAX_CRITERION_KEY_BYTES: usize = 64;
const BLIND_LABEL_ALPHABET: &[u8; 16] = b"abcdefghijklmnop";

#[derive(Clone, Copy)]
pub struct BlindCandidateInput<'a> {
    pub occurrence_id: ArtifactId,
    pub blob_id: BlobId,
    pub utf8: &'a [u8],
}

impl fmt::Debug for BlindCandidateInput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlindCandidateInput")
            .field("occurrence_id", &self.occurrence_id)
            .field("blob_id", &self.blob_id)
            .field("byte_len", &self.utf8.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlindCandidateLabel([u8; 12]);

impl BlindCandidateLabel {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("blind labels are lowercase ASCII")
    }
}

impl fmt::Display for BlindCandidateLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for BlindCandidateLabel {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for BlindCandidateLabel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let bytes = value.as_bytes();
        if bytes.len() != 12 || bytes.iter().any(|byte| !(b'a'..=b'p').contains(byte)) {
            return Err(serde::de::Error::custom("invalid blind candidate label"));
        }
        let mut label = [0_u8; 12];
        label.copy_from_slice(bytes);
        Ok(Self(label))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlindCandidatePacket {
    label: BlindCandidateLabel,
    text: String,
}

impl BlindCandidatePacket {
    pub const fn label(&self) -> BlindCandidateLabel {
        self.label
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Judge-facing packet. Candidate occurrence IDs, blob IDs, treatment names,
/// seeds, ranks, and generation metadata are absent.
#[derive(Clone, Eq, PartialEq)]
pub struct BlindPairPacket {
    task_id: ArtifactId,
    first: BlindCandidatePacket,
    second: BlindCandidatePacket,
    criterion_order: Vec<String>,
    packet_fingerprint: BlobId,
}

impl fmt::Debug for BlindPairPacket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlindPairPacket")
            .field("task_id", &self.task_id)
            .field("first_label", &self.first.label)
            .field("first_byte_len", &self.first.text.len())
            .field("second_label", &self.second.label)
            .field("second_byte_len", &self.second.text.len())
            .field("criterion_count", &self.criterion_order.len())
            .field("packet_fingerprint", &self.packet_fingerprint)
            .finish()
    }
}

impl BlindPairPacket {
    pub const fn task_id(&self) -> ArtifactId {
        self.task_id
    }

    pub const fn first(&self) -> &BlindCandidatePacket {
        &self.first
    }

    pub const fn second(&self) -> &BlindCandidatePacket {
        &self.second
    }

    pub fn criterion_order(&self) -> &[String] {
        &self.criterion_order
    }

    pub const fn packet_fingerprint(&self) -> BlobId {
        self.packet_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CandidateBinding {
    label: BlindCandidateLabel,
    occurrence_id: ArtifactId,
    blob_id: BlobId,
}

/// Private mapping retained by the harness. It is intentionally separate from
/// the packet passed to a judge.
#[derive(Debug)]
pub struct BlindPairAssignment {
    packet: BlindPairPacket,
    first_binding: CandidateBinding,
    second_binding: CandidateBinding,
    mapping_fingerprint: BlobId,
}

impl BlindPairAssignment {
    pub const fn packet(&self) -> &BlindPairPacket {
        &self.packet
    }

    pub const fn mapping_fingerprint(&self) -> BlobId {
        self.mapping_fingerprint
    }

    #[cfg(feature = "local-critic")]
    pub(crate) const fn ordered_candidate_bindings(&self) -> [(ArtifactId, BlobId); 2] {
        [
            (self.first_binding.occurrence_id, self.first_binding.blob_id),
            (
                self.second_binding.occurrence_id,
                self.second_binding.blob_id,
            ),
        ]
    }

    pub fn resolve_label<'a>(
        &self,
        label: BlindCandidateLabel,
        first_bytes: &'a [u8],
        second_bytes: &'a [u8],
    ) -> Result<CandidateEvidenceSource<'a>, BlindAssignmentError> {
        let (binding, bytes) = if label == self.first_binding.label {
            (self.first_binding, first_bytes)
        } else if label == self.second_binding.label {
            (self.second_binding, second_bytes)
        } else {
            return Err(BlindAssignmentError::UnknownLabel);
        };
        if BlobId::digest(bytes) != binding.blob_id {
            return Err(BlindAssignmentError::CandidateBytesChanged);
        }
        Ok(CandidateEvidenceSource {
            occurrence_id: binding.occurrence_id,
            blob_id: binding.blob_id,
            utf8: bytes,
        })
    }

    fn source_for_label(
        &self,
        label: BlindCandidateLabel,
    ) -> Result<CandidateEvidenceSource<'_>, BlindAssignmentError> {
        if label == self.first_binding.label {
            Ok(CandidateEvidenceSource {
                occurrence_id: self.first_binding.occurrence_id,
                blob_id: self.first_binding.blob_id,
                utf8: self.packet.first.text.as_bytes(),
            })
        } else if label == self.second_binding.label {
            Ok(CandidateEvidenceSource {
                occurrence_id: self.second_binding.occurrence_id,
                blob_id: self.second_binding.blob_id,
                utf8: self.packet.second.text.as_bytes(),
            })
        } else {
            Err(BlindAssignmentError::UnknownLabel)
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BlindEvidenceSpanClaim {
    pub label: BlindCandidateLabel,
    pub range: NonEmptyByteRange,
    pub quote: EvidenceQuote,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "verdict", content = "winner_label")]
pub enum BlindPairVerdictClaim {
    Winner(BlindCandidateLabel),
    Tie,
    Abstain,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BlindPairJudgmentClaim {
    pub verdict: BlindPairVerdictClaim,
    pub evidence: Vec<BlindEvidenceSpanClaim>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlindPairAbstention {
    Explicit,
    PromptInjectionSuspected,
    MalformedStructuredResponse,
    InvalidLabelOrEvidence,
    MissingBilateralEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "disposition")]
pub enum ValidatedBlindPairJudgment {
    Winner {
        winner_occurrence_id: ArtifactId,
        evidence: Vec<ValidatedEvidenceSpan>,
    },
    Tie {
        evidence: Vec<ValidatedEvidenceSpan>,
    },
    Abstained {
        reason: BlindPairAbstention,
    },
}

pub fn parse_and_validate_blind_pair_judgment(
    assignment: &BlindPairAssignment,
    response_json: &[u8],
) -> ValidatedBlindPairJudgment {
    let Ok(claim) = serde_json::from_slice::<BlindPairJudgmentClaim>(response_json) else {
        return ValidatedBlindPairJudgment::Abstained {
            reason: BlindPairAbstention::MalformedStructuredResponse,
        };
    };
    validate_blind_pair_judgment(assignment, &claim)
}

pub fn validate_blind_pair_judgment(
    assignment: &BlindPairAssignment,
    claim: &BlindPairJudgmentClaim,
) -> ValidatedBlindPairJudgment {
    if claim.verdict == BlindPairVerdictClaim::Abstain {
        return ValidatedBlindPairJudgment::Abstained {
            reason: BlindPairAbstention::Explicit,
        };
    }
    let mut resolved = Vec::with_capacity(claim.evidence.len());
    let mut cited_labels = BTreeSet::new();
    for blind in &claim.evidence {
        let Ok(source) = assignment.source_for_label(blind.label) else {
            return ValidatedBlindPairJudgment::Abstained {
                reason: BlindPairAbstention::InvalidLabelOrEvidence,
            };
        };
        let exact = EvidenceSpanClaim {
            candidate_occurrence_id: source.occurrence_id,
            candidate_blob_id: source.blob_id,
            range: blind.range,
            quote: blind.quote.clone(),
        };
        let Ok(mut validated) = validate_evidence_spans(source, &[exact]) else {
            return ValidatedBlindPairJudgment::Abstained {
                reason: BlindPairAbstention::InvalidLabelOrEvidence,
            };
        };
        cited_labels.insert(blind.label);
        resolved.append(&mut validated);
    }
    if cited_labels.len() != 2 {
        return ValidatedBlindPairJudgment::Abstained {
            reason: BlindPairAbstention::MissingBilateralEvidence,
        };
    }
    match claim.verdict {
        BlindPairVerdictClaim::Winner(label) => {
            let Ok(winner) = assignment.source_for_label(label) else {
                return ValidatedBlindPairJudgment::Abstained {
                    reason: BlindPairAbstention::InvalidLabelOrEvidence,
                };
            };
            ValidatedBlindPairJudgment::Winner {
                winner_occurrence_id: winner.occurrence_id,
                evidence: resolved,
            }
        }
        BlindPairVerdictClaim::Tie => ValidatedBlindPairJudgment::Tie { evidence: resolved },
        BlindPairVerdictClaim::Abstain => unreachable!("explicit abstention returned above"),
    }
}

#[derive(Debug)]
pub struct BlindPairSpec<'a> {
    pub task_id: ArtifactId,
    pub left: BlindCandidateInput<'a>,
    pub right: BlindCandidateInput<'a>,
    pub reverse_candidates: bool,
    pub criterion_order: Vec<String>,
}

pub fn build_blind_pair(
    spec: BlindPairSpec<'_>,
) -> Result<BlindPairAssignment, BlindAssignmentError> {
    validate_candidate(spec.left)?;
    validate_candidate(spec.right)?;
    if spec.left.occurrence_id == spec.right.occurrence_id
        || spec.left.blob_id == spec.right.blob_id
    {
        return Err(BlindAssignmentError::SelfComparison);
    }
    validate_criterion_order(&spec.criterion_order)?;

    let first_label = derive_label(spec.task_id, 0);
    let second_label = derive_label(spec.task_id, 1);
    let (first_input, second_input) = if spec.reverse_candidates {
        (spec.right, spec.left)
    } else {
        (spec.left, spec.right)
    };
    let first_binding = CandidateBinding {
        label: first_label,
        occurrence_id: first_input.occurrence_id,
        blob_id: first_input.blob_id,
    };
    let second_binding = CandidateBinding {
        label: second_label,
        occurrence_id: second_input.occurrence_id,
        blob_id: second_input.blob_id,
    };
    let first = BlindCandidatePacket {
        label: first_label,
        text: std::str::from_utf8(first_input.utf8)
            .expect("candidate validation checked UTF-8")
            .to_owned(),
    };
    let second = BlindCandidatePacket {
        label: second_label,
        text: std::str::from_utf8(second_input.utf8)
            .expect("candidate validation checked UTF-8")
            .to_owned(),
    };
    let packet_fingerprint =
        fingerprint_packet(spec.task_id, &first, &second, &spec.criterion_order);
    let mapping_fingerprint = fingerprint_mapping(
        packet_fingerprint,
        first_binding,
        second_binding,
        spec.reverse_candidates,
    );
    Ok(BlindPairAssignment {
        packet: BlindPairPacket {
            task_id: spec.task_id,
            first,
            second,
            criterion_order: spec.criterion_order,
            packet_fingerprint,
        },
        first_binding,
        second_binding,
        mapping_fingerprint,
    })
}

fn validate_candidate(candidate: BlindCandidateInput<'_>) -> Result<(), BlindAssignmentError> {
    if candidate.utf8.is_empty() || std::str::from_utf8(candidate.utf8).is_err() {
        return Err(BlindAssignmentError::CandidateNotText);
    }
    if BlobId::digest(candidate.utf8) != candidate.blob_id {
        return Err(BlindAssignmentError::CandidateBytesChanged);
    }
    Ok(())
}

fn validate_criterion_order(criteria: &[String]) -> Result<(), BlindAssignmentError> {
    if criteria.is_empty() || criteria.len() > MAX_BLIND_CRITERIA {
        return Err(BlindAssignmentError::InvalidCriterionCount(criteria.len()));
    }
    let mut unique = BTreeSet::new();
    for criterion in criteria {
        if criterion.is_empty()
            || criterion.len() > MAX_CRITERION_KEY_BYTES
            || criterion.chars().any(char::is_control)
        {
            return Err(BlindAssignmentError::InvalidCriterionKey);
        }
        if !unique.insert(criterion) {
            return Err(BlindAssignmentError::DuplicateCriterion);
        }
    }
    Ok(())
}

fn derive_label(task_id: ArtifactId, slot: u8) -> BlindCandidateLabel {
    let mut digest = Sha256::new();
    digest.update(b"loom/blind-candidate-label/v1\0");
    digest.update(task_id.as_ulid().to_bytes());
    digest.update([slot]);
    let digest = digest.finalize();
    let mut label = [0_u8; 12];
    for (index, byte) in label.iter_mut().enumerate() {
        *byte = BLIND_LABEL_ALPHABET[usize::from(digest[index] & 0x0f)];
    }
    BlindCandidateLabel(label)
}

fn fingerprint_packet(
    task_id: ArtifactId,
    first: &BlindCandidatePacket,
    second: &BlindCandidatePacket,
    criteria: &[String],
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/blind-pair-packet/v1\0");
    digest.update(task_id.as_ulid().to_bytes());
    update_len_bytes(&mut digest, first.label.as_str().as_bytes());
    update_len_bytes(&mut digest, first.text.as_bytes());
    update_len_bytes(&mut digest, second.label.as_str().as_bytes());
    update_len_bytes(&mut digest, second.text.as_bytes());
    digest.update((criteria.len() as u64).to_be_bytes());
    for criterion in criteria {
        update_len_bytes(&mut digest, criterion.as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_mapping(
    packet: BlobId,
    first: CandidateBinding,
    second: CandidateBinding,
    reversed: bool,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/blind-pair-mapping/v1\0");
    digest.update(packet.as_bytes());
    for binding in [first, second] {
        update_len_bytes(&mut digest, binding.label.as_str().as_bytes());
        digest.update(binding.occurrence_id.as_ulid().to_bytes());
        digest.update(binding.blob_id.as_bytes());
    }
    digest.update([u8::from(reversed)]);
    BlobId::from_bytes(digest.finalize().into())
}

fn update_len_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum BlindAssignmentError {
    #[error("blind comparison candidates must be distinct occurrences and bytes")]
    SelfComparison,
    #[error("blind comparison candidate is empty or not UTF-8")]
    CandidateNotText,
    #[error("blind comparison candidate bytes differ from their blob ID")]
    CandidateBytesChanged,
    #[error("blind criterion count {0} is outside 1..={MAX_BLIND_CRITERIA}")]
    InvalidCriterionCount(usize),
    #[error("blind criterion key is empty, oversized, or contains a control character")]
    InvalidCriterionKey,
    #[error("blind criterion order contains a duplicate")]
    DuplicateCriterion,
    #[error("blind response references an unknown candidate label")]
    UnknownLabel,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(bytes: &[u8]) -> BlindCandidateInput<'_> {
        BlindCandidateInput {
            occurrence_id: ArtifactId::new(),
            blob_id: BlobId::digest(bytes),
            utf8: bytes,
        }
    }

    #[test]
    fn reversal_changes_mapping_but_packet_discloses_no_candidate_ids() {
        let task = ArtifactId::new();
        let left = candidate(b"Left manuscript.");
        let right = candidate(b"Right manuscript.");
        let forward = build_blind_pair(BlindPairSpec {
            task_id: task,
            left,
            right,
            reverse_candidates: false,
            criterion_order: vec!["continuity".into(), "voice".into()],
        })
        .expect("forward assignment");
        let reverse = build_blind_pair(BlindPairSpec {
            task_id: task,
            left,
            right,
            reverse_candidates: true,
            criterion_order: vec!["continuity".into(), "voice".into()],
        })
        .expect("reverse assignment");

        assert_eq!(forward.packet().first().text(), "Left manuscript.");
        assert_eq!(reverse.packet().first().text(), "Right manuscript.");
        assert_ne!(forward.mapping_fingerprint(), reverse.mapping_fingerprint());
        let debug = format!("{:?}", forward.packet());
        assert!(!debug.contains("Left manuscript"));
        assert!(!debug.contains(&left.occurrence_id.to_string()));
    }

    #[test]
    fn label_resolution_rechecks_exact_candidate_bytes() {
        let left = candidate(b"First.");
        let right = candidate(b"Second.");
        let assignment = build_blind_pair(BlindPairSpec {
            task_id: ArtifactId::new(),
            left,
            right,
            reverse_candidates: false,
            criterion_order: vec!["precision".into()],
        })
        .expect("assignment");
        let resolved = assignment
            .resolve_label(assignment.packet().first().label(), left.utf8, right.utf8)
            .expect("known label");
        assert_eq!(resolved.occurrence_id, left.occurrence_id);
        assert!(matches!(
            assignment.resolve_label(assignment.packet().first().label(), b"altered", right.utf8),
            Err(BlindAssignmentError::CandidateBytesChanged)
        ));
    }

    #[test]
    fn blind_pair_judgment_requires_exact_bilateral_evidence() {
        let left = candidate(b"First manuscript.");
        let right = candidate(b"Second manuscript.");
        let assignment = build_blind_pair(BlindPairSpec {
            task_id: ArtifactId::new(),
            left,
            right,
            reverse_candidates: false,
            criterion_order: vec!["continuity".into()],
        })
        .expect("assignment");
        let first_label = assignment.packet().first().label();
        let second_label = assignment.packet().second().label();
        let claim = BlindPairJudgmentClaim {
            verdict: BlindPairVerdictClaim::Winner(first_label),
            evidence: vec![
                BlindEvidenceSpanClaim {
                    label: first_label,
                    range: NonEmptyByteRange::new(0, 5).expect("range"),
                    quote: EvidenceQuote::new("First").expect("quote"),
                },
                BlindEvidenceSpanClaim {
                    label: second_label,
                    range: NonEmptyByteRange::new(0, 6).expect("range"),
                    quote: EvidenceQuote::new("Second").expect("quote"),
                },
            ],
        };
        assert!(matches!(
            validate_blind_pair_judgment(&assignment, &claim),
            ValidatedBlindPairJudgment::Winner {
                winner_occurrence_id,
                ..
            } if winner_occurrence_id == left.occurrence_id
        ));

        let one_sided = BlindPairJudgmentClaim {
            verdict: BlindPairVerdictClaim::Tie,
            evidence: vec![claim.evidence[0].clone()],
        };
        assert!(matches!(
            validate_blind_pair_judgment(&assignment, &one_sided),
            ValidatedBlindPairJudgment::Abstained {
                reason: BlindPairAbstention::MissingBilateralEvidence
            }
        ));
    }
}
