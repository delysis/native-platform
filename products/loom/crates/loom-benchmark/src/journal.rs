//! Append-only confirmatory evidence with explicit verifier authority.
//!
//! Persisted records are diagnostic. Qualification requires move-only leases
//! issued by external store/inference/evaluator verifiers. This crate exposes
//! no production issuer until those adapters exist.

use std::collections::{BTreeMap, BTreeSet};

use loom_research_types::{BoundError, BoundedVec, TrialCaseId};
use loom_types::BlobId;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    AssignmentLabelMap, BenchmarkSeal, BenchmarkSealError, BlindedBenchmarkAssignment,
    CONFIRMATORY_N_VALUES, MAX_BENCHMARK_ASSIGNMENTS, OpaqueCandidateLabel, ProfileArm,
    VerifiedBenchmarkSealLease,
};

pub const MAX_BENCHMARK_RUN_EVENTS: usize = 1_500_000;
pub const NESTED_POOL_SIZE: usize = 32;

const CANDIDATE_EVIDENCE_DOMAIN: &[u8] = b"loom/verified-candidate-evidence/v1\0";
const NESTED_POOL_DOMAIN: &[u8] = b"loom/verified-nested-pool/v1\0";
const PAIR_CONTENT_DOMAIN: &[u8] = b"loom/benchmark-pair-content/v1\0";
const FRONTIER_PACKET_DOMAIN: &[u8] = b"loom/benchmark-frontier-packet/v1\0";
const VERIFIED_RUN_DOMAIN: &[u8] = b"loom/verified-benchmark-run/v1\0";
const RUN_EVENT_DOMAIN: &[u8] = b"loom/verified-benchmark-run-event/v1\0";
const JOURNAL_GENESIS_DOMAIN: &[u8] = b"loom/verified-benchmark-journal-genesis/v1\0";
const JOURNAL_RECORD_DOMAIN: &[u8] = b"loom/verified-benchmark-journal-record/v1\0";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupplementalRunReason {
    DiagnosticRetry,
    JudgeDisagreement,
    FailureAnalysis,
    HumanAudit,
}

impl SupplementalRunReason {
    const fn domain_tag(self) -> u8 {
        match self {
            Self::DiagnosticRetry => 0,
            Self::JudgeDisagreement => 1,
            Self::FailureAnalysis => 2,
            Self::HumanAudit => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "class", rename_all = "snake_case")]
pub enum BenchmarkRunClass {
    Primary,
    Supplemental { reason: SupplementalRunReason },
}

impl BenchmarkRunClass {
    pub const fn is_primary(self) -> bool {
        matches!(self, Self::Primary)
    }

    fn update_digest(self, digest: &mut Sha256) {
        match self {
            Self::Primary => digest.update([0]),
            Self::Supplemental { reason } => digest.update([1, reason.domain_tag()]),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlindedPairOutcome {
    LeftWin,
    RightWin,
    Tie,
    Abstain,
}

impl BlindedPairOutcome {
    const fn domain_tag(self) -> u8 {
        match self {
            Self::LeftWin => 0,
            Self::RightWin => 1,
            Self::Tie => 2,
            Self::Abstain => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkDisclosurePolicy {
    ManuscriptOnly,
}

impl BenchmarkDisclosurePolicy {
    const fn domain_tag(self) -> u8 {
        match self {
            Self::ManuscriptOnly => 0,
        }
    }
}

/// Persisted candidate facts. They are diagnostic after deserialization; only
/// a `VerifiedBenchmarkRunLease` gives them qualification authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateEvidenceRecord {
    profile_fingerprint: BlobId,
    projection_fingerprint: BlobId,
    assembly_fingerprint: BlobId,
    manuscript_blob_id: BlobId,
    inference_receipt_fingerprint: BlobId,
    provenance_receipt_fingerprint: BlobId,
    hard_gate_receipt_fingerprint: BlobId,
    provenance_verified: bool,
    assembly_verified: bool,
    hard_gate_passed: bool,
    fingerprint: BlobId,
}

impl CandidateEvidenceRecord {
    pub const fn profile_fingerprint(&self) -> BlobId {
        self.profile_fingerprint
    }

    pub const fn projection_fingerprint(&self) -> BlobId {
        self.projection_fingerprint
    }

    pub const fn provenance_verified(&self) -> bool {
        self.provenance_verified
    }

    pub const fn assembly_verified(&self) -> bool {
        self.assembly_verified
    }

    pub const fn hard_gate_passed(&self) -> bool {
        self.hard_gate_passed
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(CANDIDATE_EVIDENCE_DOMAIN);
        digest.update(self.profile_fingerprint.as_bytes());
        digest.update(self.projection_fingerprint.as_bytes());
        digest.update(self.assembly_fingerprint.as_bytes());
        digest.update(self.manuscript_blob_id.as_bytes());
        digest.update(self.inference_receipt_fingerprint.as_bytes());
        digest.update(self.provenance_receipt_fingerprint.as_bytes());
        digest.update(self.hard_gate_receipt_fingerprint.as_bytes());
        digest.update([
            u8::from(self.provenance_verified),
            u8::from(self.assembly_verified),
            u8::from(self.hard_gate_passed),
        ]);
        BlobId::from_bytes(digest.finalize().into())
    }

    fn validate(&self) -> Result<(), RunJournalError> {
        if self.compute_fingerprint() != self.fingerprint {
            return Err(RunJournalError::CandidateEvidenceFingerprintMismatch);
        }
        Ok(())
    }
}

/// One exact ordered 32-candidate pool and its selected prefix winners.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NestedPoolEvidenceRecord {
    profile_fingerprint: BlobId,
    case_id: TrialCaseId,
    ordered_projection_fingerprints: [BlobId; NESTED_POOL_SIZE],
    selected_projection_fingerprints: [BlobId; CONFIRMATORY_N_VALUES.len()],
    selection_receipt_fingerprints: [BlobId; CONFIRMATORY_N_VALUES.len()],
    pool_verifier_receipt_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl NestedPoolEvidenceRecord {
    pub const fn profile_fingerprint(&self) -> BlobId {
        self.profile_fingerprint
    }

    pub const fn case_id(&self) -> TrialCaseId {
        self.case_id
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub fn selected_for_n(&self, n: u8) -> Option<BlobId> {
        CONFIRMATORY_N_VALUES
            .iter()
            .position(|candidate| *candidate == n)
            .map(|index| self.selected_projection_fingerprints[index])
    }

    fn validate(&self) -> Result<(), RunJournalError> {
        if self.compute_fingerprint() != self.fingerprint {
            return Err(RunJournalError::NestedPoolFingerprintMismatch);
        }
        let mut unique = BTreeSet::new();
        if self
            .ordered_projection_fingerprints
            .iter()
            .any(|projection| !unique.insert(*projection))
        {
            return Err(RunJournalError::NestedPoolDuplicateCandidate);
        }
        for (index, n) in CONFIRMATORY_N_VALUES.iter().copied().enumerate() {
            let prefix = &self.ordered_projection_fingerprints[..usize::from(n)];
            if !prefix.contains(&self.selected_projection_fingerprints[index]) {
                return Err(RunJournalError::NestedPoolSelectionOutsidePrefix(n));
            }
        }
        Ok(())
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(NESTED_POOL_DOMAIN);
        digest.update(self.profile_fingerprint.as_bytes());
        digest.update(self.case_id.as_ulid().to_bytes());
        for projection in &self.ordered_projection_fingerprints {
            digest.update(projection.as_bytes());
        }
        for projection in &self.selected_projection_fingerprints {
            digest.update(projection.as_bytes());
        }
        for receipt in &self.selection_receipt_fingerprints {
            digest.update(receipt.as_bytes());
        }
        digest.update(self.pool_verifier_receipt_fingerprint.as_bytes());
        BlobId::from_bytes(digest.finalize().into())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidatePairEvidenceRecord {
    assignment_id: BlobId,
    case_id: TrialCaseId,
    case_binding_fingerprint: BlobId,
    left_label: OpaqueCandidateLabel,
    right_label: OpaqueCandidateLabel,
    left: CandidateEvidenceRecord,
    right: CandidateEvidenceRecord,
    contender_pool: NestedPoolEvidenceRecord,
    comparison_content_fingerprint: BlobId,
}

impl CandidatePairEvidenceRecord {
    pub const fn left(&self) -> &CandidateEvidenceRecord {
        &self.left
    }

    pub const fn right(&self) -> &CandidateEvidenceRecord {
        &self.right
    }

    pub const fn contender_pool(&self) -> &NestedPoolEvidenceRecord {
        &self.contender_pool
    }

    pub const fn comparison_content_fingerprint(&self) -> BlobId {
        self.comparison_content_fingerprint
    }

    fn validate(
        &self,
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        assignment: &BlindedBenchmarkAssignment,
    ) -> Result<(), RunJournalError> {
        self.left.validate()?;
        self.right.validate()?;
        self.contender_pool.validate()?;
        if self.assignment_id != assignment.assignment_id()
            || self.case_id != assignment.case_id()
            || self.left_label != *assignment.left_label()
            || self.right_label != *assignment.right_label()
        {
            return Err(RunJournalError::CandidatePairAssignmentMismatch);
        }
        let case = seal
            .cases()
            .iter()
            .find(|case| case.case_id() == assignment.case_id())
            .ok_or(RunJournalError::CandidatePairAssignmentMismatch)?;
        if self.case_binding_fingerprint != case.fingerprint() {
            return Err(RunJournalError::CandidatePairAssignmentMismatch);
        }
        let entry = mapping
            .entry(assignment.assignment_id())
            .ok_or(RunJournalError::MappingMismatch)?;
        if self.left.profile_fingerprint() != entry.left_arm().profile_fingerprint()
            || self.right.profile_fingerprint() != entry.right_arm().profile_fingerprint()
        {
            return Err(RunJournalError::CandidatePairArmMismatch);
        }
        let (baseline, contender) = if matches!(entry.left_arm(), ProfileArm::Baseline { .. }) {
            (&self.left, &self.right)
        } else {
            (&self.right, &self.left)
        };
        if baseline.profile_fingerprint() != seal.baseline().fingerprint()
            || self.contender_pool.profile_fingerprint() != contender.profile_fingerprint()
            || self.contender_pool.case_id() != assignment.case_id()
            || self.contender_pool.selected_for_n(assignment.n())
                != Some(contender.projection_fingerprint())
        {
            return Err(RunJournalError::NestedPoolBindingMismatch);
        }
        let expected = fingerprint_pair_content(
            assignment.case_id(),
            baseline.fingerprint(),
            contender.fingerprint(),
            self.contender_pool.fingerprint(),
        );
        if self.comparison_content_fingerprint != expected {
            return Err(RunJournalError::CandidatePairFingerprintMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrontierPacketEvidenceRecord {
    pair: CandidatePairEvidenceRecord,
    order_cell: u8,
    criterion_permutation_fingerprint: BlobId,
    disclosure_policy: BenchmarkDisclosurePolicy,
    evaluator_prepared_pair_fingerprint: BlobId,
    exact_prompt_blob_id: BlobId,
    exact_prompt_byte_len: u64,
    output_schema_blob_id: BlobId,
    output_schema_byte_len: u64,
    fingerprint: BlobId,
}

impl FrontierPacketEvidenceRecord {
    pub const fn pair(&self) -> &CandidatePairEvidenceRecord {
        &self.pair
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub const fn evaluator_prepared_pair_fingerprint(&self) -> BlobId {
        self.evaluator_prepared_pair_fingerprint
    }

    pub const fn exact_prompt_blob_id(&self) -> BlobId {
        self.exact_prompt_blob_id
    }

    pub const fn exact_prompt_byte_len(&self) -> u64 {
        self.exact_prompt_byte_len
    }

    pub const fn output_schema_blob_id(&self) -> BlobId {
        self.output_schema_blob_id
    }

    pub const fn output_schema_byte_len(&self) -> u64 {
        self.output_schema_byte_len
    }

    fn validate(
        &self,
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        assignment: &BlindedBenchmarkAssignment,
    ) -> Result<(), RunJournalError> {
        self.pair.validate(seal, mapping, assignment)?;
        if self.order_cell != assignment.permutation_cell().index()
            || self.criterion_permutation_fingerprint != assignment.permutation_seed()
            || self.disclosure_policy != BenchmarkDisclosurePolicy::ManuscriptOnly
            || self.exact_prompt_byte_len == 0
            || self.output_schema_byte_len == 0
        {
            return Err(RunJournalError::FrontierPacketMismatch);
        }
        if self.compute_fingerprint() != self.fingerprint {
            return Err(RunJournalError::FrontierPacketFingerprintMismatch);
        }
        Ok(())
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(FRONTIER_PACKET_DOMAIN);
        digest.update(self.pair.comparison_content_fingerprint.as_bytes());
        digest.update(self.pair.assignment_id.as_bytes());
        update_label(&self.pair.left_label, &mut digest);
        update_label(&self.pair.right_label, &mut digest);
        digest.update([self.order_cell, self.disclosure_policy.domain_tag()]);
        digest.update(self.criterion_permutation_fingerprint.as_bytes());
        digest.update(self.evaluator_prepared_pair_fingerprint.as_bytes());
        digest.update(self.exact_prompt_blob_id.as_bytes());
        digest.update(self.exact_prompt_byte_len.to_be_bytes());
        digest.update(self.output_schema_blob_id.as_bytes());
        digest.update(self.output_schema_byte_len.to_be_bytes());
        BlobId::from_bytes(digest.finalize().into())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedBenchmarkRunRecord {
    run_id: BlobId,
    assignment_id: BlobId,
    class: BenchmarkRunClass,
    requested_model_fingerprint: BlobId,
    observed_model_fingerprint: BlobId,
    evaluator_protocol_fingerprint: BlobId,
    cli_binary_fingerprint: BlobId,
    cli_version_fingerprint: BlobId,
    authentication_receipt_fingerprint: BlobId,
    fresh_session_fingerprint: BlobId,
    fresh_challenge_fingerprint: BlobId,
    invocation_receipt_fingerprint: BlobId,
    packet: FrontierPacketEvidenceRecord,
    raw_jsonl_blob_id: BlobId,
    final_output_blob_id: BlobId,
    structured_evidence_receipt_fingerprint: BlobId,
    criterion_score_vector_fingerprint: BlobId,
    evaluator_judgment_fingerprint: BlobId,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    outcome: BlindedPairOutcome,
    fingerprint: BlobId,
}

impl VerifiedBenchmarkRunRecord {
    pub const fn run_id(&self) -> BlobId {
        self.run_id
    }

    pub const fn assignment_id(&self) -> BlobId {
        self.assignment_id
    }

    pub const fn class(&self) -> BenchmarkRunClass {
        self.class
    }

    pub const fn requested_model_fingerprint(&self) -> BlobId {
        self.requested_model_fingerprint
    }

    pub const fn observed_model_fingerprint(&self) -> BlobId {
        self.observed_model_fingerprint
    }

    pub const fn evaluator_protocol_fingerprint(&self) -> BlobId {
        self.evaluator_protocol_fingerprint
    }

    pub const fn cli_binary_fingerprint(&self) -> BlobId {
        self.cli_binary_fingerprint
    }

    pub const fn cli_version_fingerprint(&self) -> BlobId {
        self.cli_version_fingerprint
    }

    pub const fn authentication_receipt_fingerprint(&self) -> BlobId {
        self.authentication_receipt_fingerprint
    }

    pub const fn fresh_session_fingerprint(&self) -> BlobId {
        self.fresh_session_fingerprint
    }

    pub const fn fresh_challenge_fingerprint(&self) -> BlobId {
        self.fresh_challenge_fingerprint
    }

    pub const fn invocation_receipt_fingerprint(&self) -> BlobId {
        self.invocation_receipt_fingerprint
    }

    pub const fn packet(&self) -> &FrontierPacketEvidenceRecord {
        &self.packet
    }

    pub const fn raw_jsonl_blob_id(&self) -> BlobId {
        self.raw_jsonl_blob_id
    }

    pub const fn final_output_blob_id(&self) -> BlobId {
        self.final_output_blob_id
    }

    pub const fn structured_evidence_receipt_fingerprint(&self) -> BlobId {
        self.structured_evidence_receipt_fingerprint
    }

    pub const fn criterion_score_vector_fingerprint(&self) -> BlobId {
        self.criterion_score_vector_fingerprint
    }

    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    pub const fn cached_input_tokens(&self) -> u64 {
        self.cached_input_tokens
    }

    pub const fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    pub const fn outcome(&self) -> BlindedPairOutcome {
        self.outcome
    }

    pub const fn evaluator_judgment_fingerprint(&self) -> BlobId {
        self.evaluator_judgment_fingerprint
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn validate(
        &self,
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
    ) -> Result<(), RunJournalError> {
        let assignment = seal
            .assignment(self.assignment_id)
            .ok_or(RunJournalError::UnknownAssignment(self.assignment_id))?;
        self.packet.validate(seal, mapping, assignment)?;
        if self.packet.pair.assignment_id != self.assignment_id
            || self.requested_model_fingerprint != seal.review().model_fingerprint()
            || self.observed_model_fingerprint != self.requested_model_fingerprint
            || self.evaluator_protocol_fingerprint != seal.review().evaluator_protocol_fingerprint()
            || self.cli_binary_fingerprint != seal.review().cli_binary_fingerprint()
            || self.cli_version_fingerprint != seal.review().cli_version_fingerprint()
            || self.input_tokens == 0
            || self.output_tokens == 0
            || self.cached_input_tokens > self.input_tokens
        {
            return Err(RunJournalError::VerifiedRunBindingMismatch);
        }
        if self.compute_fingerprint() != self.fingerprint {
            return Err(RunJournalError::VerifiedRunFingerprintMismatch);
        }
        Ok(())
    }

    fn compute_fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(VERIFIED_RUN_DOMAIN);
        digest.update(self.run_id.as_bytes());
        digest.update(self.assignment_id.as_bytes());
        self.class.update_digest(&mut digest);
        digest.update(self.requested_model_fingerprint.as_bytes());
        digest.update(self.observed_model_fingerprint.as_bytes());
        digest.update(self.evaluator_protocol_fingerprint.as_bytes());
        digest.update(self.cli_binary_fingerprint.as_bytes());
        digest.update(self.cli_version_fingerprint.as_bytes());
        digest.update(self.authentication_receipt_fingerprint.as_bytes());
        digest.update(self.fresh_session_fingerprint.as_bytes());
        digest.update(self.fresh_challenge_fingerprint.as_bytes());
        digest.update(self.invocation_receipt_fingerprint.as_bytes());
        digest.update(self.packet.fingerprint.as_bytes());
        digest.update(self.raw_jsonl_blob_id.as_bytes());
        digest.update(self.final_output_blob_id.as_bytes());
        digest.update(self.structured_evidence_receipt_fingerprint.as_bytes());
        digest.update(self.criterion_score_vector_fingerprint.as_bytes());
        digest.update(self.evaluator_judgment_fingerprint.as_bytes());
        digest.update(self.input_tokens.to_be_bytes());
        digest.update(self.cached_input_tokens.to_be_bytes());
        digest.update(self.output_tokens.to_be_bytes());
        digest.update([self.outcome.domain_tag()]);
        BlobId::from_bytes(digest.finalize().into())
    }
}

/// Move-only authority for one checked frontier run. There is no public or
/// deserializing constructor. The future eval-Codex bridge is the intended
/// production issuer.
///
/// ```compile_fail
/// use loom_benchmark::VerifiedBenchmarkRunLease;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<VerifiedBenchmarkRunLease>();
/// ```
#[must_use]
#[derive(Debug)]
pub struct VerifiedBenchmarkRunLease {
    record: VerifiedBenchmarkRunRecord,
}

impl VerifiedBenchmarkRunLease {
    fn into_record(self) -> VerifiedBenchmarkRunRecord {
        self.record
    }

    #[cfg(test)]
    pub(crate) fn primary_for_test(
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        assignment: &BlindedBenchmarkAssignment,
        outcome: BlindedPairOutcome,
        contender_checks: (bool, bool, bool),
        baseline_checks: (bool, bool, bool),
    ) -> Self {
        test_run_lease(&TestRunLeaseSpec {
            seal,
            mapping,
            assignment,
            class: BenchmarkRunClass::Primary,
            salt: b"primary",
            outcome,
            contender_checks,
            baseline_checks,
            binding_mutation: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn supplemental_for_test(
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        assignment: &BlindedBenchmarkAssignment,
        reason: SupplementalRunReason,
        salt: &[u8],
    ) -> Self {
        test_run_lease(&TestRunLeaseSpec {
            seal,
            mapping,
            assignment,
            class: BenchmarkRunClass::Supplemental { reason },
            salt,
            outcome: BlindedPairOutcome::Abstain,
            contender_checks: (true, true, true),
            baseline_checks: (true, true, true),
            binding_mutation: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn conflicting_primary_for_test(
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        assignment: &BlindedBenchmarkAssignment,
        mutation: TestRunBindingMutation,
    ) -> Self {
        test_run_lease(&TestRunLeaseSpec {
            seal,
            mapping,
            assignment,
            class: BenchmarkRunClass::Primary,
            salt: mutation.salt(),
            outcome: BlindedPairOutcome::Tie,
            contender_checks: (true, true, true),
            baseline_checks: (true, true, true),
            binding_mutation: Some(mutation),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedBenchmarkRunEvent {
    sequence: u64,
    previous_event_hash: BlobId,
    run: VerifiedBenchmarkRunRecord,
    event_hash: BlobId,
}

impl VerifiedBenchmarkRunEvent {
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub const fn run(&self) -> &VerifiedBenchmarkRunRecord {
        &self.run
    }

    pub const fn event_hash(&self) -> BlobId {
        self.event_hash
    }
}

/// Strictly replayable but non-authoritative snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VerifiedBenchmarkRunJournalRecord {
    seal_fingerprint: BlobId,
    label_mapping_fingerprint: BlobId,
    seal_verification_fingerprint: BlobId,
    assignment_ids: BoundedVec<BlobId, MAX_BENCHMARK_ASSIGNMENTS>,
    events: BoundedVec<VerifiedBenchmarkRunEvent, MAX_BENCHMARK_RUN_EVENTS>,
    chain_head: BlobId,
    fingerprint: BlobId,
}

impl VerifiedBenchmarkRunJournalRecord {
    pub const fn seal_fingerprint(&self) -> BlobId {
        self.seal_fingerprint
    }

    pub const fn chain_head(&self) -> BlobId {
        self.chain_head
    }

    pub fn events(&self) -> &[VerifiedBenchmarkRunEvent] {
        &self.events
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    fn validate_internal(&self) -> Result<(), RunJournalError> {
        let mut unique = BTreeSet::new();
        if self.assignment_ids.is_empty()
            || self
                .assignment_ids
                .iter()
                .any(|assignment| !unique.insert(*assignment))
        {
            return Err(RunJournalError::AssignmentSetMismatch);
        }
        validate_event_chain(self.seal_fingerprint, &self.events, self.chain_head)?;
        if fingerprint_journal_record(
            self.seal_fingerprint,
            self.label_mapping_fingerprint,
            self.seal_verification_fingerprint,
            &self.assignment_ids,
            self.chain_head,
        ) != self.fingerprint
        {
            return Err(RunJournalError::RecordFingerprintMismatch);
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifiedBenchmarkRunJournalRecordWire {
    seal_fingerprint: BlobId,
    label_mapping_fingerprint: BlobId,
    seal_verification_fingerprint: BlobId,
    assignment_ids: BoundedVec<BlobId, MAX_BENCHMARK_ASSIGNMENTS>,
    events: BoundedVec<VerifiedBenchmarkRunEvent, MAX_BENCHMARK_RUN_EVENTS>,
    chain_head: BlobId,
    fingerprint: BlobId,
}

impl<'de> Deserialize<'de> for VerifiedBenchmarkRunJournalRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = VerifiedBenchmarkRunJournalRecordWire::deserialize(deserializer)?;
        let record = Self {
            seal_fingerprint: wire.seal_fingerprint,
            label_mapping_fingerprint: wire.label_mapping_fingerprint,
            seal_verification_fingerprint: wire.seal_verification_fingerprint,
            assignment_ids: wire.assignment_ids,
            events: wire.events,
            chain_head: wire.chain_head,
            fingerprint: wire.fingerprint,
        };
        record
            .validate_internal()
            .map_err(serde::de::Error::custom)?;
        Ok(record)
    }
}

/// Successfully parsed persisted evidence. It can be inspected but never
/// qualifies a profile.
///
/// ```compile_fail
/// use loom_benchmark::{
///     evaluate_finalists, AssignmentLabelMap, BenchmarkSeal,
///     DiagnosticBenchmarkRunJournal,
/// };
/// fn cannot_qualify(
///     seal: &BenchmarkSeal,
///     mapping: &AssignmentLabelMap,
///     diagnostic: &DiagnosticBenchmarkRunJournal,
/// ) {
///     let _ = evaluate_finalists(seal, mapping, diagnostic, Vec::new(), Vec::new());
/// }
/// ```
#[derive(Debug)]
pub struct DiagnosticBenchmarkRunJournal {
    record: VerifiedBenchmarkRunJournalRecord,
}

impl DiagnosticBenchmarkRunJournal {
    pub fn replay(
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        record: VerifiedBenchmarkRunJournalRecord,
    ) -> Result<Self, RunJournalError> {
        validate_record_against_schedule(seal, mapping, &record)?;
        Ok(Self { record })
    }

    pub const fn record(&self) -> &VerifiedBenchmarkRunJournalRecord {
        &self.record
    }
}

/// Move-only proof that every persisted run was re-resolved and revalidated.
///
/// ```compile_fail
/// use loom_benchmark::VerifiedJournalRevalidationLease;
/// fn needs_clone<T: Clone>() {}
/// needs_clone::<VerifiedJournalRevalidationLease>();
/// ```
#[must_use]
#[derive(Debug)]
pub struct VerifiedJournalRevalidationLease {
    record_fingerprint: BlobId,
    authority: RevalidationLeaseAuthority,
}

#[derive(Debug)]
struct RevalidationLeaseAuthority;

impl VerifiedJournalRevalidationLease {
    #[cfg(test)]
    pub(crate) const fn for_test(record: &VerifiedBenchmarkRunJournalRecord) -> Self {
        Self {
            record_fingerprint: record.fingerprint(),
            authority: RevalidationLeaseAuthority,
        }
    }
}

#[derive(Debug)]
pub struct QualificationBenchmarkRunJournal {
    seal_fingerprint: BlobId,
    label_mapping_fingerprint: BlobId,
    seal_verification_fingerprint: BlobId,
    assignment_ids: Vec<BlobId>,
    assignments: BTreeSet<BlobId>,
    events: Vec<VerifiedBenchmarkRunEvent>,
    chain_head: BlobId,
    runs: BTreeMap<BlobId, VerifiedBenchmarkRunRecord>,
    primary_by_assignment: BTreeMap<BlobId, BlobId>,
    fresh_sessions: BTreeSet<BlobId>,
    fresh_challenges: BTreeSet<BlobId>,
    invocations: BTreeSet<BlobId>,
    final_outputs: BTreeSet<BlobId>,
    structured_evidence: BTreeSet<BlobId>,
    evaluator_judgments: BTreeSet<BlobId>,
    pool_by_profile_case: BTreeMap<(BlobId, TrialCaseId), BlobId>,
    comparison_by_profile_case_n: BTreeMap<(BlobId, TrialCaseId, u8), BlobId>,
    baseline_by_case: BTreeMap<TrialCaseId, BlobId>,
}

impl QualificationBenchmarkRunJournal {
    pub fn from_verified_seal(
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        lease: VerifiedBenchmarkSealLease,
    ) -> Result<Self, RunJournalError> {
        seal.verify_integrity()?;
        seal.verify_mapping(mapping)?;
        let verification = lease.into_record(seal)?;
        Ok(Self::empty(seal, mapping, verification.fingerprint()))
    }

    pub fn revalidate(
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        record: &VerifiedBenchmarkRunJournalRecord,
        seal_lease: VerifiedBenchmarkSealLease,
        run_lease: VerifiedJournalRevalidationLease,
    ) -> Result<Self, RunJournalError> {
        validate_record_against_schedule(seal, mapping, record)?;
        let VerifiedJournalRevalidationLease {
            record_fingerprint,
            authority: _authority,
        } = run_lease;
        if record_fingerprint != record.fingerprint() {
            return Err(RunJournalError::RevalidationLeaseMismatch);
        }
        let verification = seal_lease.into_record(seal)?;
        if verification.fingerprint() != record.seal_verification_fingerprint {
            return Err(RunJournalError::RevalidationLeaseMismatch);
        }
        let mut journal = Self::empty(seal, mapping, verification.fingerprint());
        for event in record.events.iter() {
            journal.apply_run(seal, mapping, event.run.clone())?;
            journal.push_existing_event(event.clone())?;
        }
        if journal.chain_head != record.chain_head {
            return Err(RunJournalError::ChainHeadMismatch);
        }
        Ok(journal)
    }

    pub fn append_verified(
        &mut self,
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        lease: VerifiedBenchmarkRunLease,
    ) -> Result<BlobId, RunJournalError> {
        self.verify_schedule(seal, mapping)?;
        let run = lease.into_record();
        let event = self.make_event(run.clone())?;
        self.apply_run(seal, mapping, run)?;
        let hash = event.event_hash;
        self.chain_head = hash;
        self.events.push(event);
        Ok(hash)
    }

    pub fn snapshot(&self) -> Result<VerifiedBenchmarkRunJournalRecord, RunJournalError> {
        let assignment_ids = BoundedVec::new(self.assignment_ids.clone())?;
        let events = BoundedVec::new(self.events.clone())?;
        let fingerprint = fingerprint_journal_record(
            self.seal_fingerprint,
            self.label_mapping_fingerprint,
            self.seal_verification_fingerprint,
            &assignment_ids,
            self.chain_head,
        );
        let record = VerifiedBenchmarkRunJournalRecord {
            seal_fingerprint: self.seal_fingerprint,
            label_mapping_fingerprint: self.label_mapping_fingerprint,
            seal_verification_fingerprint: self.seal_verification_fingerprint,
            assignment_ids,
            events,
            chain_head: self.chain_head,
            fingerprint,
        };
        record.validate_internal()?;
        Ok(record)
    }

    pub const fn chain_head(&self) -> BlobId {
        self.chain_head
    }

    pub const fn seal_fingerprint(&self) -> BlobId {
        self.seal_fingerprint
    }

    pub fn run(&self, run_id: BlobId) -> Option<&VerifiedBenchmarkRunRecord> {
        self.runs.get(&run_id)
    }

    pub fn primary_run_for_assignment(&self, assignment_id: BlobId) -> Option<BlobId> {
        self.primary_by_assignment.get(&assignment_id).copied()
    }

    pub fn primary_is_complete(&self) -> bool {
        self.primary_by_assignment.len() == self.assignments.len()
    }

    #[cfg(test)]
    pub(crate) fn exact_state_for_test(&self) -> QualificationJournalStateForTest {
        QualificationJournalStateForTest {
            seal_fingerprint: self.seal_fingerprint,
            label_mapping_fingerprint: self.label_mapping_fingerprint,
            seal_verification_fingerprint: self.seal_verification_fingerprint,
            assignment_ids: self.assignment_ids.clone(),
            assignments: self.assignments.clone(),
            events: self.events.clone(),
            chain_head: self.chain_head,
            runs: self.runs.clone(),
            primary_by_assignment: self.primary_by_assignment.clone(),
            fresh_sessions: self.fresh_sessions.clone(),
            fresh_challenges: self.fresh_challenges.clone(),
            invocations: self.invocations.clone(),
            final_outputs: self.final_outputs.clone(),
            structured_evidence: self.structured_evidence.clone(),
            evaluator_judgments: self.evaluator_judgments.clone(),
            pool_by_profile_case: self.pool_by_profile_case.clone(),
            comparison_by_profile_case_n: self.comparison_by_profile_case_n.clone(),
            baseline_by_case: self.baseline_by_case.clone(),
        }
    }

    fn empty(
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        seal_verification_fingerprint: BlobId,
    ) -> Self {
        let assignment_ids = seal
            .assignments()
            .iter()
            .map(BlindedBenchmarkAssignment::assignment_id)
            .collect::<Vec<_>>();
        let assignments = assignment_ids.iter().copied().collect();
        Self {
            seal_fingerprint: seal.fingerprint(),
            label_mapping_fingerprint: mapping.fingerprint(),
            seal_verification_fingerprint,
            assignment_ids,
            assignments,
            events: Vec::new(),
            chain_head: journal_genesis(seal.fingerprint()),
            runs: BTreeMap::new(),
            primary_by_assignment: BTreeMap::new(),
            fresh_sessions: BTreeSet::new(),
            fresh_challenges: BTreeSet::new(),
            invocations: BTreeSet::new(),
            final_outputs: BTreeSet::new(),
            structured_evidence: BTreeSet::new(),
            evaluator_judgments: BTreeSet::new(),
            pool_by_profile_case: BTreeMap::new(),
            comparison_by_profile_case_n: BTreeMap::new(),
            baseline_by_case: BTreeMap::new(),
        }
    }

    fn verify_schedule(
        &self,
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
    ) -> Result<(), RunJournalError> {
        if self.seal_fingerprint != seal.fingerprint()
            || self.label_mapping_fingerprint != mapping.fingerprint()
        {
            return Err(RunJournalError::ScheduleMismatch);
        }
        Ok(())
    }

    fn validate_new_run(&self, run: &VerifiedBenchmarkRunRecord) -> Result<(), RunJournalError> {
        if self.runs.contains_key(&run.run_id()) {
            return Err(RunJournalError::DuplicateRunId(run.run_id()));
        }
        if !self.assignments.contains(&run.assignment_id()) {
            return Err(RunJournalError::UnknownAssignment(run.assignment_id()));
        }
        if self.fresh_sessions.contains(&run.fresh_session_fingerprint)
            || self
                .fresh_challenges
                .contains(&run.fresh_challenge_fingerprint)
            || self
                .invocations
                .contains(&run.invocation_receipt_fingerprint)
            || self.final_outputs.contains(&run.final_output_blob_id)
            || self
                .structured_evidence
                .contains(&run.structured_evidence_receipt_fingerprint)
            || self
                .evaluator_judgments
                .contains(&run.evaluator_judgment_fingerprint)
        {
            return Err(RunJournalError::ReusedRunEvidence);
        }
        match run.class() {
            BenchmarkRunClass::Primary => {
                if self
                    .primary_by_assignment
                    .contains_key(&run.assignment_id())
                {
                    return Err(RunJournalError::DuplicatePrimaryAssignment);
                }
            }
            BenchmarkRunClass::Supplemental { .. } => {
                if !self
                    .primary_by_assignment
                    .contains_key(&run.assignment_id())
                {
                    return Err(RunJournalError::SupplementalBeforePrimary);
                }
            }
        }
        Ok(())
    }

    fn apply_run(
        &mut self,
        seal: &BenchmarkSeal,
        mapping: &AssignmentLabelMap,
        run: VerifiedBenchmarkRunRecord,
    ) -> Result<(), RunJournalError> {
        run.validate(seal, mapping)?;
        self.validate_new_run(&run)?;
        let assignment = seal
            .assignment(run.assignment_id())
            .ok_or(RunJournalError::UnknownAssignment(run.assignment_id()))?;
        let entry = mapping
            .entry(run.assignment_id())
            .ok_or(RunJournalError::MappingMismatch)?;
        let pair = run.packet.pair();
        let (baseline, contender) = if matches!(entry.left_arm(), ProfileArm::Baseline { .. }) {
            (pair.left(), pair.right())
        } else {
            (pair.right(), pair.left())
        };
        let pool_key = (contender.profile_fingerprint(), assignment.case_id());
        let pool_fingerprint = pair.contender_pool().fingerprint();
        let comparison_key = (
            contender.profile_fingerprint(),
            assignment.case_id(),
            assignment.n(),
        );
        let comparison_fingerprint = pair.comparison_content_fingerprint();
        let baseline_key = assignment.case_id();
        let baseline_fingerprint = baseline.fingerprint();

        // This is the transaction boundary for the in-memory journal. Every
        // fallible check must complete before any map, evidence set, or run
        // index changes. A rejected move-only lease therefore cannot poison the
        // journal that receives the next independently verified run.
        ensure_exact(
            &self.pool_by_profile_case,
            pool_key,
            pool_fingerprint,
            RunJournalError::NestedPoolChanged,
        )?;
        ensure_exact(
            &self.comparison_by_profile_case_n,
            comparison_key,
            comparison_fingerprint,
            RunJournalError::CandidatePairChanged,
        )?;
        ensure_exact(
            &self.baseline_by_case,
            baseline_key,
            baseline_fingerprint,
            RunJournalError::BaselineCandidateChanged,
        )?;

        self.pool_by_profile_case
            .entry(pool_key)
            .or_insert(pool_fingerprint);
        self.comparison_by_profile_case_n
            .entry(comparison_key)
            .or_insert(comparison_fingerprint);
        self.baseline_by_case
            .entry(baseline_key)
            .or_insert(baseline_fingerprint);
        if run.class().is_primary() {
            self.primary_by_assignment
                .insert(run.assignment_id(), run.run_id());
        }
        self.fresh_sessions.insert(run.fresh_session_fingerprint);
        self.fresh_challenges
            .insert(run.fresh_challenge_fingerprint);
        self.invocations.insert(run.invocation_receipt_fingerprint);
        self.final_outputs.insert(run.final_output_blob_id);
        self.structured_evidence
            .insert(run.structured_evidence_receipt_fingerprint);
        self.evaluator_judgments
            .insert(run.evaluator_judgment_fingerprint);
        self.runs.insert(run.run_id(), run);
        Ok(())
    }

    fn make_event(
        &self,
        run: VerifiedBenchmarkRunRecord,
    ) -> Result<VerifiedBenchmarkRunEvent, RunJournalError> {
        if self.events.len() == MAX_BENCHMARK_RUN_EVENTS {
            return Err(RunJournalError::TooManyEvents);
        }
        let sequence =
            u64::try_from(self.events.len()).map_err(|_| RunJournalError::EventSequenceOverflow)?;
        let event_hash = fingerprint_event(sequence, self.chain_head, run.fingerprint());
        Ok(VerifiedBenchmarkRunEvent {
            sequence,
            previous_event_hash: self.chain_head,
            run,
            event_hash,
        })
    }

    fn push_existing_event(
        &mut self,
        event: VerifiedBenchmarkRunEvent,
    ) -> Result<(), RunJournalError> {
        let expected =
            u64::try_from(self.events.len()).map_err(|_| RunJournalError::EventSequenceOverflow)?;
        if event.sequence != expected
            || event.previous_event_hash != self.chain_head
            || fingerprint_event(
                event.sequence,
                event.previous_event_hash,
                event.run.fingerprint(),
            ) != event.event_hash
        {
            return Err(RunJournalError::EventChainMismatch);
        }
        self.chain_head = event.event_hash;
        self.events.push(event);
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RunJournalError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error("benchmark seal validation failed: {0}")]
    Seal(String),
    #[error("benchmark seal, mapping, or journal mismatch")]
    ScheduleMismatch,
    #[error("run refers to unknown assignment {0}")]
    UnknownAssignment(BlobId),
    #[error("assignment label mapping is missing or mismatched")]
    MappingMismatch,
    #[error("candidate evidence fingerprint mismatch")]
    CandidateEvidenceFingerprintMismatch,
    #[error("nested pool fingerprint mismatch")]
    NestedPoolFingerprintMismatch,
    #[error("nested pool repeats a candidate")]
    NestedPoolDuplicateCandidate,
    #[error("nested pool selection for N={0} is outside that exact prefix")]
    NestedPoolSelectionOutsidePrefix(u8),
    #[error("candidate pair belongs to another assignment or case")]
    CandidatePairAssignmentMismatch,
    #[error("candidate pair arms do not match the blinded mapping")]
    CandidatePairArmMismatch,
    #[error("candidate pair does not match its nested pool selection")]
    NestedPoolBindingMismatch,
    #[error("candidate pair content fingerprint mismatch")]
    CandidatePairFingerprintMismatch,
    #[error("frontier packet does not match the sealed order/permutation cell")]
    FrontierPacketMismatch,
    #[error("frontier packet fingerprint mismatch")]
    FrontierPacketFingerprintMismatch,
    #[error("verified run model, CLI, protocol, packet, or usage mismatch")]
    VerifiedRunBindingMismatch,
    #[error("verified run fingerprint mismatch")]
    VerifiedRunFingerprintMismatch,
    #[error("run ID {0} is duplicated")]
    DuplicateRunId(BlobId),
    #[error("primary assignment is duplicated")]
    DuplicatePrimaryAssignment,
    #[error("supplemental run cannot precede the primary run")]
    SupplementalBeforePrimary,
    #[error("frontier session, challenge, invocation, output, evidence, or judgment was reused")]
    ReusedRunEvidence,
    #[error("nested pool changed across N or review cells")]
    NestedPoolChanged,
    #[error("candidate pair changed across review cells")]
    CandidatePairChanged,
    #[error("baseline candidate changed across comparisons for one case")]
    BaselineCandidateChanged,
    #[error("run event limit reached")]
    TooManyEvents,
    #[error("run event sequence cannot be represented")]
    EventSequenceOverflow,
    #[error("run event chain mismatch")]
    EventChainMismatch,
    #[error("run journal chain head mismatch")]
    ChainHeadMismatch,
    #[error("run journal assignment set mismatch")]
    AssignmentSetMismatch,
    #[error("run journal record fingerprint mismatch")]
    RecordFingerprintMismatch,
    #[error("journal revalidation lease does not match the exact persisted record")]
    RevalidationLeaseMismatch,
}

impl From<BenchmarkSealError> for RunJournalError {
    fn from(error: BenchmarkSealError) -> Self {
        Self::Seal(error.to_string())
    }
}

fn validate_record_against_schedule(
    seal: &BenchmarkSeal,
    mapping: &AssignmentLabelMap,
    record: &VerifiedBenchmarkRunJournalRecord,
) -> Result<(), RunJournalError> {
    record.validate_internal()?;
    seal.verify_integrity()?;
    seal.verify_mapping(mapping)?;
    let expected = seal
        .assignments()
        .iter()
        .map(BlindedBenchmarkAssignment::assignment_id)
        .collect::<Vec<_>>();
    if record.seal_fingerprint != seal.fingerprint()
        || record.label_mapping_fingerprint != mapping.fingerprint()
        || record.assignment_ids.as_ref() != expected
    {
        return Err(RunJournalError::AssignmentSetMismatch);
    }
    for event in record.events.iter() {
        event.run.validate(seal, mapping)?;
    }
    Ok(())
}

fn validate_event_chain(
    seal: BlobId,
    events: &[VerifiedBenchmarkRunEvent],
    expected_head: BlobId,
) -> Result<(), RunJournalError> {
    let mut head = journal_genesis(seal);
    let mut run_ids = BTreeSet::new();
    for (index, event) in events.iter().enumerate() {
        let sequence = u64::try_from(index).map_err(|_| RunJournalError::EventSequenceOverflow)?;
        if event.sequence != sequence
            || event.previous_event_hash != head
            || fingerprint_event(sequence, head, event.run.fingerprint()) != event.event_hash
            || !run_ids.insert(event.run.run_id())
        {
            return Err(RunJournalError::EventChainMismatch);
        }
        head = event.event_hash;
    }
    if head != expected_head {
        return Err(RunJournalError::ChainHeadMismatch);
    }
    Ok(())
}

fn ensure_exact<K: Ord + Copy>(
    map: &BTreeMap<K, BlobId>,
    key: K,
    value: BlobId,
    error: RunJournalError,
) -> Result<(), RunJournalError> {
    if map.get(&key).is_some_and(|existing| *existing != value) {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QualificationJournalStateForTest {
    seal_fingerprint: BlobId,
    label_mapping_fingerprint: BlobId,
    seal_verification_fingerprint: BlobId,
    assignment_ids: Vec<BlobId>,
    assignments: BTreeSet<BlobId>,
    events: Vec<VerifiedBenchmarkRunEvent>,
    chain_head: BlobId,
    runs: BTreeMap<BlobId, VerifiedBenchmarkRunRecord>,
    primary_by_assignment: BTreeMap<BlobId, BlobId>,
    fresh_sessions: BTreeSet<BlobId>,
    fresh_challenges: BTreeSet<BlobId>,
    invocations: BTreeSet<BlobId>,
    final_outputs: BTreeSet<BlobId>,
    structured_evidence: BTreeSet<BlobId>,
    evaluator_judgments: BTreeSet<BlobId>,
    pool_by_profile_case: BTreeMap<(BlobId, TrialCaseId), BlobId>,
    comparison_by_profile_case_n: BTreeMap<(BlobId, TrialCaseId, u8), BlobId>,
    baseline_by_case: BTreeMap<TrialCaseId, BlobId>,
}

fn fingerprint_pair_content(
    case_id: TrialCaseId,
    baseline: BlobId,
    contender: BlobId,
    pool: BlobId,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(PAIR_CONTENT_DOMAIN);
    digest.update(case_id.as_ulid().to_bytes());
    digest.update(baseline.as_bytes());
    digest.update(contender.as_bytes());
    digest.update(pool.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn journal_genesis(seal: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(JOURNAL_GENESIS_DOMAIN);
    digest.update(seal.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_event(sequence: u64, previous: BlobId, run: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(RUN_EVENT_DOMAIN);
    digest.update(sequence.to_be_bytes());
    digest.update(previous.as_bytes());
    digest.update(run.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_journal_record(
    seal: BlobId,
    mapping: BlobId,
    verification: BlobId,
    assignments: &[BlobId],
    head: BlobId,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(JOURNAL_RECORD_DOMAIN);
    digest.update(seal.as_bytes());
    digest.update(mapping.as_bytes());
    digest.update(verification.as_bytes());
    digest.update((assignments.len() as u64).to_be_bytes());
    for assignment in assignments {
        digest.update(assignment.as_bytes());
    }
    digest.update(head.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn update_label(label: &OpaqueCandidateLabel, digest: &mut Sha256) {
    digest.update((label.as_str().len() as u64).to_be_bytes());
    digest.update(label.as_str().as_bytes());
}

#[cfg(test)]
struct TestRunLeaseSpec<'a> {
    seal: &'a BenchmarkSeal,
    mapping: &'a AssignmentLabelMap,
    assignment: &'a BlindedBenchmarkAssignment,
    class: BenchmarkRunClass,
    salt: &'a [u8],
    outcome: BlindedPairOutcome,
    contender_checks: (bool, bool, bool),
    baseline_checks: (bool, bool, bool),
    binding_mutation: Option<TestRunBindingMutation>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TestRunBindingMutation {
    Pool,
    CandidatePair,
    Baseline,
}

#[cfg(test)]
impl TestRunBindingMutation {
    const fn salt(self) -> &'static [u8] {
        match self {
            Self::Pool => b"conflicting-pool",
            Self::CandidatePair => b"conflicting-candidate-pair",
            Self::Baseline => b"conflicting-baseline",
        }
    }
}

#[cfg(test)]
fn test_tagged(tag: &[u8], values: &[&[u8]]) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(tag);
    for value in values {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    BlobId::from_bytes(digest.finalize().into())
}

#[cfg(test)]
fn test_nested_pool(
    contender_profile: BlobId,
    assignment: &BlindedBenchmarkAssignment,
    variant: Option<&[u8]>,
) -> NestedPoolEvidenceRecord {
    let case_bytes = assignment.case_id().as_ulid().to_bytes();
    let mut ordered = [BlobId::digest(&[]); NESTED_POOL_SIZE];
    for (index, slot) in ordered.iter_mut().enumerate() {
        *slot = test_tagged(
            b"test-pool-candidate",
            &[
                contender_profile.as_bytes(),
                &case_bytes,
                &(index as u64).to_be_bytes(),
                variant.unwrap_or_default(),
            ],
        );
    }
    let selected = CONFIRMATORY_N_VALUES.map(|n| ordered[usize::from(n) - 1]);
    let selection_receipts = CONFIRMATORY_N_VALUES.map(|n| {
        test_tagged(
            b"test-selection-receipt",
            &[
                contender_profile.as_bytes(),
                &case_bytes,
                &[n],
                variant.unwrap_or_default(),
            ],
        )
    });
    let mut pool = NestedPoolEvidenceRecord {
        profile_fingerprint: contender_profile,
        case_id: assignment.case_id(),
        ordered_projection_fingerprints: ordered,
        selected_projection_fingerprints: selected,
        selection_receipt_fingerprints: selection_receipts,
        pool_verifier_receipt_fingerprint: test_tagged(
            b"test-pool-verifier",
            &[
                contender_profile.as_bytes(),
                &case_bytes,
                variant.unwrap_or_default(),
            ],
        ),
        fingerprint: BlobId::digest(&[]),
    };
    pool.fingerprint = pool.compute_fingerprint();
    pool
}

#[cfg(test)]
fn test_candidate(
    profile: BlobId,
    projection: BlobId,
    checks: (bool, bool, bool),
) -> CandidateEvidenceRecord {
    let tagged_projection = |tag: &[u8]| test_tagged(tag, &[projection.as_bytes()]);
    let mut record = CandidateEvidenceRecord {
        profile_fingerprint: profile,
        projection_fingerprint: projection,
        assembly_fingerprint: tagged_projection(b"test-assembly"),
        manuscript_blob_id: tagged_projection(b"test-manuscript"),
        inference_receipt_fingerprint: tagged_projection(b"test-inference"),
        provenance_receipt_fingerprint: tagged_projection(b"test-provenance"),
        hard_gate_receipt_fingerprint: tagged_projection(b"test-gates"),
        provenance_verified: checks.0,
        assembly_verified: checks.1,
        hard_gate_passed: checks.2,
        fingerprint: BlobId::digest(&[]),
    };
    record.fingerprint = record.compute_fingerprint();
    record
}

#[cfg(test)]
fn test_candidate_pair(spec: &TestRunLeaseSpec<'_>) -> CandidatePairEvidenceRecord {
    let entry = spec
        .mapping
        .entry(spec.assignment.assignment_id())
        .expect("test mapping");
    let contender_profile = [entry.left_arm(), entry.right_arm()]
        .into_iter()
        .find_map(|arm| match arm {
            ProfileArm::Contender {
                profile_fingerprint,
            } => Some(profile_fingerprint),
            ProfileArm::Baseline { .. } => None,
        })
        .expect("test contender arm");
    let pool_variant = matches!(spec.binding_mutation, Some(TestRunBindingMutation::Pool))
        .then_some(b"conflicting-pool".as_slice());
    let pool = test_nested_pool(contender_profile, spec.assignment, pool_variant);
    let contender_projection = pool
        .selected_for_n(spec.assignment.n())
        .expect("canonical test N");
    let case_bytes = spec.assignment.case_id().as_ulid().to_bytes();
    let baseline_projection = if matches!(
        spec.binding_mutation,
        Some(TestRunBindingMutation::Baseline)
    ) {
        test_tagged(b"test-conflicting-baseline-projection", &[&case_bytes])
    } else {
        test_tagged(b"test-baseline-projection", &[&case_bytes])
    };
    let baseline = test_candidate(
        spec.seal.baseline().fingerprint(),
        baseline_projection,
        spec.baseline_checks,
    );
    let mut contender = test_candidate(
        contender_profile,
        contender_projection,
        spec.contender_checks,
    );
    if matches!(
        spec.binding_mutation,
        Some(TestRunBindingMutation::CandidatePair)
    ) {
        contender.inference_receipt_fingerprint = test_tagged(
            b"test-conflicting-inference",
            &[spec.assignment.assignment_id().as_bytes()],
        );
        contender.fingerprint = contender.compute_fingerprint();
    }
    let (left, right) = if matches!(entry.left_arm(), ProfileArm::Baseline { .. }) {
        (baseline, contender)
    } else {
        (contender, baseline)
    };
    let pair_content = fingerprint_pair_content(
        spec.assignment.case_id(),
        if matches!(entry.left_arm(), ProfileArm::Baseline { .. }) {
            left.fingerprint()
        } else {
            right.fingerprint()
        },
        if matches!(entry.left_arm(), ProfileArm::Contender { .. }) {
            left.fingerprint()
        } else {
            right.fingerprint()
        },
        pool.fingerprint(),
    );
    let case = spec
        .seal
        .cases()
        .iter()
        .find(|case| case.case_id() == spec.assignment.case_id())
        .expect("test case");
    CandidatePairEvidenceRecord {
        assignment_id: spec.assignment.assignment_id(),
        case_id: spec.assignment.case_id(),
        case_binding_fingerprint: case.fingerprint(),
        left_label: spec.assignment.left_label().clone(),
        right_label: spec.assignment.right_label().clone(),
        left,
        right,
        contender_pool: pool,
        comparison_content_fingerprint: pair_content,
    }
}

#[cfg(test)]
fn test_run_lease(spec: &TestRunLeaseSpec<'_>) -> VerifiedBenchmarkRunLease {
    let pair = test_candidate_pair(spec);
    let mut packet = FrontierPacketEvidenceRecord {
        pair,
        order_cell: spec.assignment.permutation_cell().index(),
        criterion_permutation_fingerprint: spec.assignment.permutation_seed(),
        disclosure_policy: BenchmarkDisclosurePolicy::ManuscriptOnly,
        evaluator_prepared_pair_fingerprint: test_tagged(
            b"test-prepared-frontier-pair",
            &[spec.assignment.assignment_id().as_bytes()],
        ),
        exact_prompt_blob_id: test_tagged(
            b"test-prompt",
            &[spec.assignment.assignment_id().as_bytes()],
        ),
        exact_prompt_byte_len: 1_024,
        output_schema_blob_id: test_tagged(
            b"test-schema",
            &[spec.assignment.assignment_id().as_bytes()],
        ),
        output_schema_byte_len: 512,
        fingerprint: BlobId::digest(&[]),
    };
    packet.fingerprint = packet.compute_fingerprint();
    let run_id = test_tagged(
        b"test-run",
        &[spec.assignment.assignment_id().as_bytes(), spec.salt],
    );
    let mut record = VerifiedBenchmarkRunRecord {
        run_id,
        assignment_id: spec.assignment.assignment_id(),
        class: spec.class,
        requested_model_fingerprint: spec.seal.review().model_fingerprint(),
        observed_model_fingerprint: spec.seal.review().model_fingerprint(),
        evaluator_protocol_fingerprint: spec.seal.review().evaluator_protocol_fingerprint(),
        cli_binary_fingerprint: spec.seal.review().cli_binary_fingerprint(),
        cli_version_fingerprint: spec.seal.review().cli_version_fingerprint(),
        authentication_receipt_fingerprint: test_tagged(b"test-auth", &[run_id.as_bytes()]),
        fresh_session_fingerprint: test_tagged(b"test-session", &[run_id.as_bytes()]),
        fresh_challenge_fingerprint: test_tagged(b"test-challenge", &[run_id.as_bytes()]),
        invocation_receipt_fingerprint: test_tagged(b"test-invocation", &[run_id.as_bytes()]),
        packet,
        raw_jsonl_blob_id: test_tagged(b"test-jsonl", &[run_id.as_bytes()]),
        final_output_blob_id: test_tagged(b"test-final", &[run_id.as_bytes()]),
        structured_evidence_receipt_fingerprint: test_tagged(
            b"test-evidence",
            &[run_id.as_bytes()],
        ),
        criterion_score_vector_fingerprint: test_tagged(b"test-scores", &[run_id.as_bytes()]),
        evaluator_judgment_fingerprint: test_tagged(
            b"test-frontier-judgment",
            &[run_id.as_bytes()],
        ),
        input_tokens: 1_024,
        cached_input_tokens: 128,
        output_tokens: 256,
        outcome: spec.outcome,
        fingerprint: BlobId::digest(&[]),
    };
    record.fingerprint = record.compute_fingerprint();
    VerifiedBenchmarkRunLease { record }
}
