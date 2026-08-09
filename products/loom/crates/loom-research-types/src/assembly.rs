use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use loom_types::{BlobId, CommandId, RevisionId};

use crate::{
    BoundError, BoundedText, ByteRange, CallError, CandidateAssemblyId, CandidateProjectionId,
    ExactCallEvidence, GeneratedSpanOccurrenceId, GeneratedSpanOccurrenceRecord,
    MAX_ASSEMBLY_BYTES, MAX_ASSEMBLY_EVIDENCE_BYTES, MAX_ASSEMBLY_EVIDENCE_TOKENS,
    MAX_ASSEMBLY_PARTS, MixedAuthorshipAssemblyId, NonEmptyBoundedVec, OperationGraph,
    OperationGraphError, PipelineEligibility, PipelineIneligibility, PipelineOperationKind,
    RangeError,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinBefore {
    None,
    Space,
    LineBreak,
    ParagraphBreak,
}

impl JoinBefore {
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            Self::None => b"",
            Self::Space => b" ",
            Self::LineBreak => b"\n",
            Self::ParagraphBreak => b"\n\n",
        }
    }

    const fn domain_tag(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Space => 1,
            Self::LineBreak => 2,
            Self::ParagraphBreak => 3,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AssemblyPartRecord {
    join_before: JoinBefore,
    span: GeneratedSpanOccurrenceRecord,
}

impl AssemblyPartRecord {
    pub const fn new(join_before: JoinBefore, span: GeneratedSpanOccurrenceRecord) -> Self {
        Self { join_before, span }
    }

    pub const fn join_before(&self) -> JoinBefore {
        self.join_before
    }

    pub const fn span(&self) -> &GeneratedSpanOccurrenceRecord {
        &self.span
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssemblyPartRecordWire {
    join_before: JoinBefore,
    span: GeneratedSpanOccurrenceRecord,
}

impl<'de> Deserialize<'de> for AssemblyPartRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AssemblyPartRecordWire::deserialize(deserializer)?;
        Ok(Self::new(wire.join_before, wire.span))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AssemblyReconstructionWitness {
    part_order_fingerprint: BlobId,
    graph_fingerprint: BlobId,
    assembled_blob_id: BlobId,
    assembled_byte_len: u64,
}

impl AssemblyReconstructionWitness {
    pub const fn part_order_fingerprint(&self) -> BlobId {
        self.part_order_fingerprint
    }

    pub const fn graph_fingerprint(&self) -> BlobId {
        self.graph_fingerprint
    }

    pub const fn assembled_blob_id(&self) -> BlobId {
        self.assembled_blob_id
    }

    pub const fn assembled_byte_len(&self) -> u64 {
        self.assembled_byte_len
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssemblyReconstructionWitnessWire {
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    part_order_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    graph_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    assembled_blob_id: BlobId,
    assembled_byte_len: u64,
}

impl<'de> Deserialize<'de> for AssemblyReconstructionWitness {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AssemblyReconstructionWitnessWire::deserialize(deserializer)?;
        if wire.assembled_byte_len == 0 || wire.assembled_byte_len > MAX_ASSEMBLY_BYTES as u64 {
            return Err(serde::de::Error::custom(
                AssemblyError::AssemblyLengthOutOfBounds {
                    actual: usize::try_from(wire.assembled_byte_len).unwrap_or(usize::MAX),
                    maximum: MAX_ASSEMBLY_BYTES,
                },
            ));
        }
        Ok(Self {
            part_order_fingerprint: wire.part_order_fingerprint,
            graph_fingerprint: wire.graph_fingerprint,
            assembled_blob_id: wire.assembled_blob_id,
            assembled_byte_len: wire.assembled_byte_len,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CandidateAssemblyRecord {
    id: CandidateAssemblyId,
    parts: NonEmptyBoundedVec<AssemblyPartRecord, MAX_ASSEMBLY_PARTS>,
    operation_graph: OperationGraph,
    witness: AssemblyReconstructionWitness,
}

impl CandidateAssemblyRecord {
    pub fn new(
        id: CandidateAssemblyId,
        parts: Vec<AssemblyPartRecord>,
        evidence: &[ExactCallEvidence<'_>],
    ) -> Result<Self, AssemblyError> {
        let parts = NonEmptyBoundedVec::new(parts)?;
        validate_parts(&parts)?;
        validate_evidence_coverage(&parts, evidence)?;
        let assembled = reconstruct_parts(&parts, evidence)?;
        validate_assembled_bytes(&assembled)?;
        let spans = parts
            .iter()
            .map(|part| part.span.clone())
            .collect::<Vec<_>>();
        let operation_graph = OperationGraph::for_assembly_record(id, &spans)?;
        let witness = AssemblyReconstructionWitness {
            part_order_fingerprint: fingerprint_part_order(id, &parts),
            graph_fingerprint: operation_graph.fingerprint(),
            assembled_blob_id: BlobId::digest(&assembled),
            assembled_byte_len: assembled.len() as u64,
        };
        Ok(Self {
            id,
            parts,
            operation_graph,
            witness,
        })
    }

    pub const fn id(&self) -> CandidateAssemblyId {
        self.id
    }

    pub fn parts(&self) -> &[AssemblyPartRecord] {
        &self.parts
    }

    pub const fn operation_graph(&self) -> &OperationGraph {
        &self.operation_graph
    }

    pub const fn witness(&self) -> &AssemblyReconstructionWitness {
        &self.witness
    }

    pub fn declared_pipeline_eligibility(&self) -> PipelineEligibility {
        self.operation_graph.pipeline_eligibility()
    }

    pub fn reconstruct(
        &self,
        evidence: &[ExactCallEvidence<'_>],
    ) -> Result<Vec<u8>, AssemblyError> {
        self.validate_static()?;
        validate_evidence_coverage(&self.parts, evidence)?;
        let assembled = reconstruct_parts(&self.parts, evidence)?;
        validate_assembled_bytes(&assembled)?;
        if assembled.len() as u64 != self.witness.assembled_byte_len
            || BlobId::digest(&assembled) != self.witness.assembled_blob_id
        {
            return Err(AssemblyError::ReconstructionWitnessMismatch);
        }
        Ok(assembled)
    }

    fn validate_static(&self) -> Result<(), AssemblyError> {
        validate_parts(&self.parts)?;
        if fingerprint_part_order(self.id, &self.parts) != self.witness.part_order_fingerprint
            || self.operation_graph.fingerprint() != self.witness.graph_fingerprint
        {
            return Err(AssemblyError::ReconstructionWitnessMismatch);
        }
        validate_assembly_graph(self.id, &self.parts, &self.operation_graph)?;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateAssemblyRecordWire {
    id: CandidateAssemblyId,
    parts: NonEmptyBoundedVec<AssemblyPartRecord, MAX_ASSEMBLY_PARTS>,
    operation_graph: OperationGraph,
    witness: AssemblyReconstructionWitness,
}

impl<'de> Deserialize<'de> for CandidateAssemblyRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CandidateAssemblyRecordWire::deserialize(deserializer)?;
        let assembly = Self {
            id: wire.id,
            parts: wire.parts,
            operation_graph: wire.operation_graph,
            witness: wire.witness,
        };
        assembly
            .validate_static()
            .map_err(serde::de::Error::custom)?;
        Ok(assembly)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProjectionWitness {
    binding_fingerprint: BlobId,
    source_blob_id: BlobId,
    assembly_blob_id: BlobId,
    resulting_blob_id: BlobId,
    resulting_byte_len: u64,
}

impl ProjectionWitness {
    pub const fn binding_fingerprint(&self) -> BlobId {
        self.binding_fingerprint
    }

    pub const fn source_blob_id(&self) -> BlobId {
        self.source_blob_id
    }

    pub const fn assembly_blob_id(&self) -> BlobId {
        self.assembly_blob_id
    }

    pub const fn resulting_blob_id(&self) -> BlobId {
        self.resulting_blob_id
    }

    pub const fn resulting_byte_len(&self) -> u64 {
        self.resulting_byte_len
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionWitnessWire {
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    binding_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    source_blob_id: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    assembly_blob_id: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    resulting_blob_id: BlobId,
    resulting_byte_len: u64,
}

impl<'de> Deserialize<'de> for ProjectionWitness {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ProjectionWitnessWire::deserialize(deserializer)?;
        if wire.resulting_byte_len == 0
            || wire.resulting_byte_len > crate::MAX_SOURCE_BYTES as u64 + MAX_ASSEMBLY_BYTES as u64
        {
            return Err(serde::de::Error::custom(
                AssemblyError::ProjectedOutputTooLarge,
            ));
        }
        Ok(Self {
            binding_fingerprint: wire.binding_fingerprint,
            source_blob_id: wire.source_blob_id,
            assembly_blob_id: wire.assembly_blob_id,
            resulting_blob_id: wire.resulting_blob_id,
            resulting_byte_len: wire.resulting_byte_len,
        })
    }
}

/// Application of an assembly to a pinned source range. Source bytes are never
/// copied into this artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CandidateProjectionRecord {
    id: CandidateProjectionId,
    assembly_id: CandidateAssemblyId,
    source_revision_id: RevisionId,
    source_blob_id: BlobId,
    target_range: ByteRange,
    operation_graph: OperationGraph,
    witness: ProjectionWitness,
}

impl CandidateProjectionRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: CandidateProjectionId,
        assembly: &CandidateAssemblyRecord,
        source_revision_id: RevisionId,
        source_blob_id: BlobId,
        source_bytes: &[u8],
        target_range: ByteRange,
        evidence: &[ExactCallEvidence<'_>],
    ) -> Result<Self, AssemblyError> {
        validate_source(source_blob_id, source_bytes, target_range)?;
        let assembled = assembly.reconstruct(evidence)?;
        let resulting = apply_bytes(source_bytes, target_range, &assembled)?;
        let operation_graph = assembly.operation_graph.with_projection(id)?;
        let resulting_blob_id = BlobId::digest(&resulting);
        let binding_fingerprint = fingerprint_projection_binding(
            id,
            assembly.id,
            source_revision_id,
            source_blob_id,
            target_range,
            assembly.witness.assembled_blob_id,
            resulting_blob_id,
            resulting.len() as u64,
            operation_graph.fingerprint(),
        );
        let witness = ProjectionWitness {
            binding_fingerprint,
            source_blob_id,
            assembly_blob_id: assembly.witness.assembled_blob_id,
            resulting_blob_id,
            resulting_byte_len: resulting.len() as u64,
        };
        Ok(Self {
            id,
            assembly_id: assembly.id,
            source_revision_id,
            source_blob_id,
            target_range,
            operation_graph,
            witness,
        })
    }

    pub const fn id(&self) -> CandidateProjectionId {
        self.id
    }

    pub const fn assembly_id(&self) -> CandidateAssemblyId {
        self.assembly_id
    }

    pub const fn source_revision_id(&self) -> RevisionId {
        self.source_revision_id
    }

    pub const fn source_blob_id(&self) -> BlobId {
        self.source_blob_id
    }

    pub const fn target_range(&self) -> ByteRange {
        self.target_range
    }

    pub const fn operation_graph(&self) -> &OperationGraph {
        &self.operation_graph
    }

    pub const fn witness(&self) -> &ProjectionWitness {
        &self.witness
    }

    pub fn apply(
        &self,
        assembly: &CandidateAssemblyRecord,
        source_bytes: &[u8],
        evidence: &[ExactCallEvidence<'_>],
    ) -> Result<Vec<u8>, AssemblyError> {
        self.validate_static()?;
        if assembly.id != self.assembly_id {
            return Err(AssemblyError::AssemblyIdMismatch);
        }
        validate_source(self.source_blob_id, source_bytes, self.target_range)?;
        if self.witness.source_blob_id != self.source_blob_id
            || self.witness.assembly_blob_id != assembly.witness.assembled_blob_id
        {
            return Err(AssemblyError::ProjectionWitnessMismatch);
        }
        let expected_graph = assembly.operation_graph.with_projection(self.id)?;
        if self.operation_graph != expected_graph {
            return Err(AssemblyError::ProjectionGraphMismatch);
        }
        let assembled = assembly.reconstruct(evidence)?;
        let resulting = apply_bytes(source_bytes, self.target_range, &assembled)?;
        if resulting.len() as u64 != self.witness.resulting_byte_len
            || BlobId::digest(&resulting) != self.witness.resulting_blob_id
        {
            return Err(AssemblyError::ProjectionWitnessMismatch);
        }
        Ok(resulting)
    }

    fn validate_static(&self) -> Result<(), AssemblyError> {
        if self.witness.source_blob_id != self.source_blob_id {
            return Err(AssemblyError::ProjectionWitnessMismatch);
        }
        let output = self
            .operation_graph
            .nodes()
            .iter()
            .find(|node| node.id() == self.operation_graph.output())
            .ok_or(AssemblyError::ProjectionGraphMismatch)?;
        if !matches!(
            output.kind(),
            PipelineOperationKind::Project { projection_id } if *projection_id == self.id
        ) || output.inputs().len() != 1
        {
            return Err(AssemblyError::ProjectionGraphMismatch);
        }
        let assembly_node = self
            .operation_graph
            .nodes()
            .iter()
            .find(|node| node.id() == output.inputs()[0])
            .ok_or(AssemblyError::ProjectionGraphMismatch)?;
        if !matches!(
            assembly_node.kind(),
            PipelineOperationKind::Assemble { assembly_id } if *assembly_id == self.assembly_id
        ) {
            return Err(AssemblyError::ProjectionGraphMismatch);
        }
        let expected_binding = fingerprint_projection_binding(
            self.id,
            self.assembly_id,
            self.source_revision_id,
            self.source_blob_id,
            self.target_range,
            self.witness.assembly_blob_id,
            self.witness.resulting_blob_id,
            self.witness.resulting_byte_len,
            self.operation_graph.fingerprint(),
        );
        if self.witness.binding_fingerprint != expected_binding {
            return Err(AssemblyError::ProjectionWitnessMismatch);
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateProjectionRecordWire {
    id: CandidateProjectionId,
    assembly_id: CandidateAssemblyId,
    #[serde(deserialize_with = "crate::bounded::deserialize_revision_id")]
    source_revision_id: RevisionId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    source_blob_id: BlobId,
    target_range: ByteRange,
    operation_graph: OperationGraph,
    witness: ProjectionWitness,
}

impl<'de> Deserialize<'de> for CandidateProjectionRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CandidateProjectionRecordWire::deserialize(deserializer)?;
        let projection = Self {
            id: wire.id,
            assembly_id: wire.assembly_id,
            source_revision_id: wire.source_revision_id,
            source_blob_id: wire.source_blob_id,
            target_range: wire.target_range,
            operation_graph: wire.operation_graph,
            witness: wire.witness,
        };
        projection
            .validate_static()
            .map_err(serde::de::Error::custom)?;
        Ok(projection)
    }
}

/// An inspectable human/instruct/critic/literal transformation lane. A
/// promotion layer may replay it under `PromotionAuthority`, but this record
/// itself confers no authority and can never support a base-writer-only claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MixedAuthorshipAssemblyRecord {
    id: MixedAuthorshipAssemblyId,
    output_blob_id: BlobId,
    output_byte_len: u64,
    operation_graph: OperationGraph,
}

impl MixedAuthorshipAssemblyRecord {
    pub fn new(
        id: MixedAuthorshipAssemblyId,
        exact_output: &[u8],
        operation_graph: OperationGraph,
    ) -> Result<Self, AssemblyError> {
        validate_assembled_bytes(exact_output)?;
        if matches!(
            operation_graph.pipeline_eligibility(),
            PipelineEligibility::DeclaredBaseWriterOnly
        ) {
            return Err(AssemblyError::MixedAssemblyRequiresMixedEvidence);
        }
        let output_blob_id = BlobId::digest(exact_output);
        validate_mixed_output_graph(output_blob_id, &operation_graph, false, None)?;
        Ok(Self {
            id,
            output_blob_id,
            output_byte_len: exact_output.len() as u64,
            operation_graph,
        })
    }

    pub fn new_from_call_output(
        id: MixedAuthorshipAssemblyId,
        exact_output: &[u8],
        operation_graph: OperationGraph,
        evidence: &ExactCallEvidence<'_>,
    ) -> Result<Self, AssemblyError> {
        validate_assembled_bytes(exact_output)?;
        if exact_output != evidence.raw_output() {
            return Err(AssemblyError::MixedOutputGraphMismatch);
        }
        evidence
            .call()
            .completed()?
            .verify_exact(evidence.raw_output(), evidence.token_ids())?;
        let output_blob_id = BlobId::digest(exact_output);
        validate_mixed_output_graph(
            output_blob_id,
            &operation_graph,
            true,
            Some(evidence.call()),
        )?;
        Ok(Self {
            id,
            output_blob_id,
            output_byte_len: exact_output.len() as u64,
            operation_graph,
        })
    }

    pub const fn id(&self) -> MixedAuthorshipAssemblyId {
        self.id
    }

    pub const fn output_blob_id(&self) -> BlobId {
        self.output_blob_id
    }

    pub const fn output_byte_len(&self) -> u64 {
        self.output_byte_len
    }

    pub const fn operation_graph(&self) -> &OperationGraph {
        &self.operation_graph
    }

    pub fn declared_pipeline_eligibility(&self) -> PipelineEligibility {
        self.operation_graph.pipeline_eligibility()
    }

    pub fn verify_output(&self, exact_output: &[u8]) -> Result<(), AssemblyError> {
        validate_assembled_bytes(exact_output)?;
        if exact_output.len() as u64 != self.output_byte_len
            || BlobId::digest(exact_output) != self.output_blob_id
        {
            return Err(AssemblyError::ReconstructionWitnessMismatch);
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MixedAuthorshipAssemblyRecordWire {
    id: MixedAuthorshipAssemblyId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    output_blob_id: BlobId,
    output_byte_len: u64,
    operation_graph: OperationGraph,
}

impl<'de> Deserialize<'de> for MixedAuthorshipAssemblyRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = MixedAuthorshipAssemblyRecordWire::deserialize(deserializer)?;
        if wire.output_byte_len == 0 || wire.output_byte_len > MAX_ASSEMBLY_BYTES as u64 {
            return Err(serde::de::Error::custom(
                AssemblyError::AssemblyLengthOutOfBounds {
                    actual: usize::try_from(wire.output_byte_len).unwrap_or(usize::MAX),
                    maximum: MAX_ASSEMBLY_BYTES,
                },
            ));
        }
        if matches!(
            wire.operation_graph.pipeline_eligibility(),
            PipelineEligibility::DeclaredBaseWriterOnly
        ) {
            return Err(serde::de::Error::custom(
                AssemblyError::MixedAssemblyRequiresMixedEvidence,
            ));
        }
        validate_mixed_output_graph(wire.output_blob_id, &wire.operation_graph, true, None)
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            id: wire.id,
            output_blob_id: wire.output_blob_id,
            output_byte_len: wire.output_byte_len,
            operation_graph: wire.operation_graph,
        })
    }
}

pub type PromotionActor = BoundedText<128>;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserPresenceKind {
    EditorGesture,
    CliInteractiveConfirmation,
    NativeDialogConfirmation,
    HumanReviewSubmission,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UserPresenceEvidence {
    kind: UserPresenceKind,
    session_fingerprint: BlobId,
    event_receipt_blob_id: BlobId,
    monotonic_event_index: u64,
    occurred_at_ms: i64,
}

impl UserPresenceEvidence {
    pub fn new(
        kind: UserPresenceKind,
        session_fingerprint: BlobId,
        event_receipt_blob_id: BlobId,
        monotonic_event_index: u64,
        occurred_at_ms: i64,
    ) -> Result<Self, AssemblyError> {
        if monotonic_event_index == 0 || occurred_at_ms <= 0 {
            return Err(AssemblyError::InvalidUserPresence);
        }
        Ok(Self {
            kind,
            session_fingerprint,
            event_receipt_blob_id,
            monotonic_event_index,
            occurred_at_ms,
        })
    }

    pub const fn kind(&self) -> UserPresenceKind {
        self.kind
    }

    pub const fn event_receipt_blob_id(&self) -> BlobId {
        self.event_receipt_blob_id
    }

    pub const fn session_fingerprint(&self) -> BlobId {
        self.session_fingerprint
    }

    pub const fn monotonic_event_index(&self) -> u64 {
        self.monotonic_event_index
    }

    pub const fn occurred_at_ms(&self) -> i64 {
        self.occurred_at_ms
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserPresenceEvidenceWire {
    kind: UserPresenceKind,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    session_fingerprint: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    event_receipt_blob_id: BlobId,
    monotonic_event_index: u64,
    occurred_at_ms: i64,
}

impl<'de> Deserialize<'de> for UserPresenceEvidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = UserPresenceEvidenceWire::deserialize(deserializer)?;
        Self::new(
            wire.kind,
            wire.session_fingerprint,
            wire.event_receipt_blob_id,
            wire.monotonic_event_index,
            wire.occurred_at_ms,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Structural promotion-authority record. There is no caller-set
/// `human_confirmed` boolean: actor, pinned source, command occurrence and
/// user-presence receipt must all be present. The store still verifies that
/// receipt before promotion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PromotionAuthority {
    actor: PromotionActor,
    source_revision_id: RevisionId,
    source_blob_id: BlobId,
    command_id: CommandId,
    user_presence: UserPresenceEvidence,
}

impl PromotionAuthority {
    pub const fn new(
        actor: PromotionActor,
        source_revision_id: RevisionId,
        source_blob_id: BlobId,
        command_id: CommandId,
        user_presence: UserPresenceEvidence,
    ) -> Self {
        Self {
            actor,
            source_revision_id,
            source_blob_id,
            command_id,
            user_presence,
        }
    }

    pub const fn actor(&self) -> &PromotionActor {
        &self.actor
    }

    pub const fn source_revision_id(&self) -> RevisionId {
        self.source_revision_id
    }

    pub const fn source_blob_id(&self) -> BlobId {
        self.source_blob_id
    }

    pub const fn command_id(&self) -> CommandId {
        self.command_id
    }

    pub const fn user_presence(&self) -> &UserPresenceEvidence {
        &self.user_presence
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromotionAuthorityWire {
    actor: PromotionActor,
    #[serde(deserialize_with = "crate::bounded::deserialize_revision_id")]
    source_revision_id: RevisionId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    source_blob_id: BlobId,
    #[serde(deserialize_with = "crate::bounded::deserialize_command_id")]
    command_id: CommandId,
    user_presence: UserPresenceEvidence,
}

impl<'de> Deserialize<'de> for PromotionAuthority {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PromotionAuthorityWire::deserialize(deserializer)?;
        Ok(Self::new(
            wire.actor,
            wire.source_revision_id,
            wire.source_blob_id,
            wire.command_id,
            wire.user_presence,
        ))
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AssemblyError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error(transparent)]
    Call(#[from] CallError),
    #[error(transparent)]
    Graph(#[from] OperationGraphError),
    #[error(transparent)]
    Range(#[from] RangeError),
    #[error("first assembly part must have JoinBefore::None")]
    FirstPartHasSeparator,
    #[error("assembly repeats span occurrence {0}")]
    DuplicateSpan(GeneratedSpanOccurrenceId),
    #[error("assembly repeats model call occurrence")]
    DuplicateModelCall,
    #[error("exact call evidence is missing, duplicated, or contains unrelated calls")]
    ExactEvidenceCoverageMismatch,
    #[error("aggregate raw-output or token evidence exceeds the assembly replay budget")]
    EvidenceBudgetExceeded,
    #[error("assembly has {actual} bytes; allowed range is 1..={maximum}")]
    AssemblyLengthOutOfBounds { actual: usize, maximum: usize },
    #[error("assembly bytes are not valid UTF-8")]
    InvalidAssemblyUtf8,
    #[error("assembly reconstruction witness does not match")]
    ReconstructionWitnessMismatch,
    #[error("candidate assembly record operation graph does not exactly describe its parts")]
    AssemblyGraphMismatch,
    #[error("source manuscript blob does not match pinned bytes")]
    SourceBlobMismatch,
    #[error("candidate projection references a different assembly")]
    AssemblyIdMismatch,
    #[error("candidate projection witness does not match")]
    ProjectionWitnessMismatch,
    #[error("candidate projection operation graph does not match")]
    ProjectionGraphMismatch,
    #[error("projected output exceeds its bound")]
    ProjectedOutputTooLarge,
    #[error("mixed-authorship assembly requires a text-affecting ineligible operation")]
    MixedAssemblyRequiresMixedEvidence,
    #[error("mixed-authorship output is not bound to the graph's terminal text operation")]
    MixedOutputGraphMismatch,
    #[error("instruct-editor or critic output requires exact model-call evidence")]
    MixedOutputRequiresCallEvidence,
    #[error("user-presence evidence requires a positive event index and timestamp")]
    InvalidUserPresence,
}

fn validate_parts(
    parts: &NonEmptyBoundedVec<AssemblyPartRecord, MAX_ASSEMBLY_PARTS>,
) -> Result<(), AssemblyError> {
    if parts[0].join_before != JoinBefore::None {
        return Err(AssemblyError::FirstPartHasSeparator);
    }
    let mut span_ids = BTreeSet::new();
    let mut call_ids = BTreeSet::new();
    for part in parts.iter() {
        if !span_ids.insert(part.span.id()) {
            return Err(AssemblyError::DuplicateSpan(part.span.id()));
        }
        if !call_ids.insert(part.span.call_id()) {
            return Err(AssemblyError::DuplicateModelCall);
        }
    }
    Ok(())
}

fn validate_evidence_coverage(
    parts: &[AssemblyPartRecord],
    evidence: &[ExactCallEvidence<'_>],
) -> Result<(), AssemblyError> {
    if evidence.len() != parts.len() {
        return Err(AssemblyError::ExactEvidenceCoverageMismatch);
    }
    let evidence_ids = evidence
        .iter()
        .map(|item| item.call().id())
        .collect::<BTreeSet<_>>();
    if evidence_ids.len() != evidence.len()
        || parts
            .iter()
            .any(|part| !evidence_ids.contains(&part.span.call_id()))
    {
        return Err(AssemblyError::ExactEvidenceCoverageMismatch);
    }
    let mut raw_bytes = 0_usize;
    let mut token_ids = 0_usize;
    for item in evidence {
        raw_bytes = raw_bytes
            .checked_add(item.raw_output().len())
            .ok_or(AssemblyError::EvidenceBudgetExceeded)?;
        token_ids = token_ids
            .checked_add(item.token_ids().len())
            .ok_or(AssemblyError::EvidenceBudgetExceeded)?;
        if raw_bytes > MAX_ASSEMBLY_EVIDENCE_BYTES || token_ids > MAX_ASSEMBLY_EVIDENCE_TOKENS {
            return Err(AssemblyError::EvidenceBudgetExceeded);
        }
    }
    Ok(())
}

fn reconstruct_parts(
    parts: &[AssemblyPartRecord],
    evidence: &[ExactCallEvidence<'_>],
) -> Result<Vec<u8>, AssemblyError> {
    let mut output = Vec::new();
    for part in parts {
        let exact = evidence
            .iter()
            .find(|item| item.call().id() == part.span.call_id())
            .ok_or(AssemblyError::ExactEvidenceCoverageMismatch)?;
        part.span.verify_exact(exact)?;
        output.extend_from_slice(part.join_before.bytes());
        output.extend_from_slice(part.span.displayed_str(exact)?.as_bytes());
        if output.len() > MAX_ASSEMBLY_BYTES {
            return Err(AssemblyError::AssemblyLengthOutOfBounds {
                actual: output.len(),
                maximum: MAX_ASSEMBLY_BYTES,
            });
        }
    }
    Ok(output)
}

fn validate_assembled_bytes(bytes: &[u8]) -> Result<(), AssemblyError> {
    if bytes.is_empty() || bytes.len() > MAX_ASSEMBLY_BYTES {
        return Err(AssemblyError::AssemblyLengthOutOfBounds {
            actual: bytes.len(),
            maximum: MAX_ASSEMBLY_BYTES,
        });
    }
    let _ = std::str::from_utf8(bytes).map_err(|_| AssemblyError::InvalidAssemblyUtf8)?;
    Ok(())
}

fn fingerprint_part_order(
    assembly_id: CandidateAssemblyId,
    parts: &[AssemblyPartRecord],
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/candidate-assembly-parts/v1\0");
    digest.update(assembly_id.as_ulid().to_bytes());
    digest.update((parts.len() as u64).to_be_bytes());
    for part in parts {
        digest.update([part.join_before.domain_tag()]);
        digest.update(part.span.id().as_ulid().to_bytes());
        digest.update(part.span.call_id().as_ulid().to_bytes());
        digest.update(part.span.extraction_receipt().fingerprint().as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn validate_assembly_graph(
    assembly_id: CandidateAssemblyId,
    parts: &[AssemblyPartRecord],
    graph: &OperationGraph,
) -> Result<(), AssemblyError> {
    if graph.nodes().len() != parts.len() * 2 + 1 {
        return Err(AssemblyError::AssemblyGraphMismatch);
    }
    let output = graph
        .nodes()
        .iter()
        .find(|node| node.id() == graph.output())
        .ok_or(AssemblyError::AssemblyGraphMismatch)?;
    if !matches!(
        output.kind(),
        PipelineOperationKind::Assemble { assembly_id: graph_id } if *graph_id == assembly_id
    ) || output.inputs().len() != parts.len()
    {
        return Err(AssemblyError::AssemblyGraphMismatch);
    }
    for (part, extract_id) in parts.iter().zip(output.inputs()) {
        let extract = graph
            .nodes()
            .iter()
            .find(|node| node.id() == *extract_id)
            .ok_or(AssemblyError::AssemblyGraphMismatch)?;
        if !matches!(
            extract.kind(),
            PipelineOperationKind::ExtractSpan { occurrence_id } if *occurrence_id == part.span.id()
        ) || extract.inputs().len() != 1
        {
            return Err(AssemblyError::AssemblyGraphMismatch);
        }
        let call = graph
            .nodes()
            .iter()
            .find(|node| node.id() == extract.inputs()[0])
            .ok_or(AssemblyError::AssemblyGraphMismatch)?;
        if !matches!(
            call.kind(),
            PipelineOperationKind::ModelCall { call_id, evidence_class }
                if *call_id == part.span.call_id()
                    && *evidence_class == part.span.evidence_class()
        ) {
            return Err(AssemblyError::AssemblyGraphMismatch);
        }
    }
    Ok(())
}

fn validate_source(
    source_blob_id: BlobId,
    source_bytes: &[u8],
    target_range: ByteRange,
) -> Result<(), AssemblyError> {
    let _ = crate::range::validate_source_utf8(source_bytes)?;
    if BlobId::digest(source_bytes) != source_blob_id {
        return Err(AssemblyError::SourceBlobMismatch);
    }
    let _ = target_range.checked_slice(source_bytes)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn fingerprint_projection_binding(
    projection_id: CandidateProjectionId,
    assembly_id: CandidateAssemblyId,
    source_revision_id: RevisionId,
    source_blob_id: BlobId,
    target_range: ByteRange,
    assembly_blob_id: BlobId,
    resulting_blob_id: BlobId,
    resulting_byte_len: u64,
    operation_graph_fingerprint: BlobId,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(b"loom/candidate-projection/v1\0");
    digest.update(projection_id.as_ulid().to_bytes());
    digest.update(assembly_id.as_ulid().to_bytes());
    digest.update(source_revision_id.as_ulid().to_bytes());
    digest.update(source_blob_id.as_bytes());
    digest.update(target_range.start().to_be_bytes());
    digest.update(target_range.end().to_be_bytes());
    digest.update(assembly_blob_id.as_bytes());
    digest.update(resulting_blob_id.as_bytes());
    digest.update(resulting_byte_len.to_be_bytes());
    digest.update(operation_graph_fingerprint.as_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn apply_bytes(
    source_bytes: &[u8],
    target_range: ByteRange,
    assembled: &[u8],
) -> Result<Vec<u8>, AssemblyError> {
    let range_len =
        usize::try_from(target_range.len()).map_err(|_| AssemblyError::ProjectedOutputTooLarge)?;
    let range_start = usize::try_from(target_range.start())
        .map_err(|_| AssemblyError::ProjectedOutputTooLarge)?;
    let range_end =
        usize::try_from(target_range.end()).map_err(|_| AssemblyError::ProjectedOutputTooLarge)?;
    let resulting_len = source_bytes
        .len()
        .checked_sub(range_len)
        .and_then(|length| length.checked_add(assembled.len()))
        .ok_or(AssemblyError::ProjectedOutputTooLarge)?;
    if resulting_len > crate::MAX_SOURCE_BYTES + MAX_ASSEMBLY_BYTES {
        return Err(AssemblyError::ProjectedOutputTooLarge);
    }
    let mut result = Vec::with_capacity(resulting_len);
    result.extend_from_slice(&source_bytes[..range_start]);
    result.extend_from_slice(assembled);
    result.extend_from_slice(&source_bytes[range_end..]);
    Ok(result)
}

fn validate_mixed_output_graph(
    output_blob_id: BlobId,
    graph: &OperationGraph,
    allow_call_claim: bool,
    exact_call: Option<&crate::ModelCall>,
) -> Result<(), AssemblyError> {
    let output = graph
        .nodes()
        .iter()
        .find(|node| node.id() == graph.output())
        .ok_or(AssemblyError::MixedOutputGraphMismatch)?;
    match output.kind() {
        PipelineOperationKind::HumanTransformation { content_blob_id }
        | PipelineOperationKind::CodexText { content_blob_id }
        | PipelineOperationKind::FixtureText { content_blob_id }
        | PipelineOperationKind::HistoricalText { content_blob_id }
        | PipelineOperationKind::LiteralText { content_blob_id } => {
            if *content_blob_id != output_blob_id {
                return Err(AssemblyError::MixedOutputGraphMismatch);
            }
        }
        PipelineOperationKind::InstructEditorTransformation {
            call_id,
            output_blob_id: claimed_output,
        } => {
            if !allow_call_claim {
                return Err(AssemblyError::MixedOutputRequiresCallEvidence);
            }
            if *claimed_output != output_blob_id {
                return Err(AssemblyError::MixedOutputGraphMismatch);
            }
            if let Some(call) = exact_call
                && (call.id() != *call_id
                    || call.evidence_class() != crate::CallEvidenceClass::LiveInstructEditorClaim)
            {
                return Err(AssemblyError::MixedOutputGraphMismatch);
            }
        }
        PipelineOperationKind::CriticText {
            call_id,
            output_blob_id: claimed_output,
        } => {
            if !allow_call_claim {
                return Err(AssemblyError::MixedOutputRequiresCallEvidence);
            }
            if *claimed_output != output_blob_id {
                return Err(AssemblyError::MixedOutputGraphMismatch);
            }
            if let Some(call) = exact_call
                && (call.id() != *call_id
                    || !matches!(
                        call.evidence_class(),
                        crate::CallEvidenceClass::LiveLocalCriticClaim
                            | crate::CallEvidenceClass::LiveCodexCriticClaim
                    ))
            {
                return Err(AssemblyError::MixedOutputGraphMismatch);
            }
        }
        PipelineOperationKind::ModelCall { .. }
        | PipelineOperationKind::ExtractSpan { .. }
        | PipelineOperationKind::Assemble { .. }
        | PipelineOperationKind::Project { .. } => {
            return Err(AssemblyError::MixedOutputGraphMismatch);
        }
    }
    Ok(())
}

pub fn diagnostic_reason_labels(eligibility: &PipelineEligibility) -> Vec<&'static str> {
    let PipelineEligibility::Ineligible { reasons } = eligibility else {
        return Vec::new();
    };
    reasons
        .iter()
        .map(|reason| match reason {
            PipelineIneligibility::NonBaseWriterCall(_) => "non_base_writer_call",
            PipelineIneligibility::HumanText => "human_text",
            PipelineIneligibility::InstructEditorText => "instruct_editor_text",
            PipelineIneligibility::CriticText => "critic_text",
            PipelineIneligibility::CodexText => "codex_text",
            PipelineIneligibility::FixtureText => "fixture_text",
            PipelineIneligibility::HistoricalText => "historical_text",
            PipelineIneligibility::LiteralText => "literal_text",
        })
        .collect()
}
