use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use ulid::Ulid;

use loom_types::BlobId;

use crate::{
    BoundError, BoundedVec, CallEvidenceClass, CandidateAssemblyId, CandidateProjectionId,
    GeneratedSpanOccurrenceId, GeneratedSpanOccurrenceRecord, MAX_OPERATION_EDGES,
    MAX_OPERATION_INPUTS, MAX_OPERATION_NODES, ModelCallId, PipelineOperationId,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "operation", rename_all = "snake_case")]
pub enum PipelineOperationKind {
    ModelCall {
        call_id: ModelCallId,
        evidence_class: CallEvidenceClass,
    },
    ExtractSpan {
        occurrence_id: GeneratedSpanOccurrenceId,
    },
    Assemble {
        assembly_id: CandidateAssemblyId,
    },
    Project {
        projection_id: CandidateProjectionId,
    },
    HumanTransformation {
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        content_blob_id: BlobId,
    },
    InstructEditorTransformation {
        call_id: ModelCallId,
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        output_blob_id: BlobId,
    },
    CriticText {
        call_id: ModelCallId,
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        output_blob_id: BlobId,
    },
    CodexText {
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        content_blob_id: BlobId,
    },
    FixtureText {
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        content_blob_id: BlobId,
    },
    HistoricalText {
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        content_blob_id: BlobId,
    },
    LiteralText {
        #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
        content_blob_id: BlobId,
    },
}

impl PipelineOperationKind {
    const fn domain_tag(&self) -> u8 {
        match self {
            Self::ModelCall { .. } => 0,
            Self::ExtractSpan { .. } => 1,
            Self::Assemble { .. } => 2,
            Self::Project { .. } => 3,
            Self::HumanTransformation { .. } => 4,
            Self::InstructEditorTransformation { .. } => 5,
            Self::CriticText { .. } => 6,
            Self::CodexText { .. } => 7,
            Self::FixtureText { .. } => 8,
            Self::HistoricalText { .. } => 9,
            Self::LiteralText { .. } => 10,
        }
    }

    fn ineligibility(&self) -> Option<PipelineIneligibility> {
        match self {
            Self::ModelCall { evidence_class, .. }
                if evidence_class.is_live_base_writer_claim() =>
            {
                None
            }
            Self::ModelCall { evidence_class, .. } => {
                Some(PipelineIneligibility::NonBaseWriterCall(*evidence_class))
            }
            Self::ExtractSpan { .. } | Self::Assemble { .. } | Self::Project { .. } => None,
            Self::HumanTransformation { .. } => Some(PipelineIneligibility::HumanText),
            Self::InstructEditorTransformation { .. } => {
                Some(PipelineIneligibility::InstructEditorText)
            }
            Self::CriticText { .. } => Some(PipelineIneligibility::CriticText),
            Self::CodexText { .. } => Some(PipelineIneligibility::CodexText),
            Self::FixtureText { .. } => Some(PipelineIneligibility::FixtureText),
            Self::HistoricalText { .. } => Some(PipelineIneligibility::HistoricalText),
            Self::LiteralText { .. } => Some(PipelineIneligibility::LiteralText),
        }
    }

    fn update_digest(&self, digest: &mut Sha256) {
        digest.update([self.domain_tag()]);
        match self {
            Self::ModelCall {
                call_id,
                evidence_class,
            } => {
                digest.update(call_id.as_ulid().to_bytes());
                digest.update([call_evidence_tag(*evidence_class)]);
            }
            Self::ExtractSpan { occurrence_id } => {
                digest.update(occurrence_id.as_ulid().to_bytes());
            }
            Self::Assemble { assembly_id } => {
                digest.update(assembly_id.as_ulid().to_bytes());
            }
            Self::Project { projection_id } => {
                digest.update(projection_id.as_ulid().to_bytes());
            }
            Self::HumanTransformation { content_blob_id }
            | Self::CodexText { content_blob_id }
            | Self::FixtureText { content_blob_id }
            | Self::HistoricalText { content_blob_id }
            | Self::LiteralText { content_blob_id } => digest.update(content_blob_id.as_bytes()),
            Self::InstructEditorTransformation {
                call_id,
                output_blob_id,
            }
            | Self::CriticText {
                call_id,
                output_blob_id,
            } => {
                digest.update(call_id.as_ulid().to_bytes());
                digest.update(output_blob_id.as_bytes());
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PipelineOperation {
    id: PipelineOperationId,
    kind: PipelineOperationKind,
    inputs: BoundedVec<PipelineOperationId, MAX_OPERATION_INPUTS>,
}

impl PipelineOperation {
    pub fn new(
        id: PipelineOperationId,
        kind: PipelineOperationKind,
        inputs: Vec<PipelineOperationId>,
    ) -> Result<Self, OperationGraphError> {
        let inputs = BoundedVec::new(inputs)?;
        let unique = inputs.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != inputs.len() {
            return Err(OperationGraphError::DuplicateInput(id));
        }
        Ok(Self { id, kind, inputs })
    }

    pub const fn id(&self) -> PipelineOperationId {
        self.id
    }

    pub const fn kind(&self) -> &PipelineOperationKind {
        &self.kind
    }

    pub fn inputs(&self) -> &[PipelineOperationId] {
        &self.inputs
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineOperationWire {
    id: PipelineOperationId,
    kind: PipelineOperationKind,
    inputs: BoundedVec<PipelineOperationId, MAX_OPERATION_INPUTS>,
}

impl<'de> Deserialize<'de> for PipelineOperation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PipelineOperationWire::deserialize(deserializer)?;
        Self::new(wire.id, wire.kind, wire.inputs.into_inner()).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperationGraph {
    nodes: BoundedVec<PipelineOperation, MAX_OPERATION_NODES>,
    output: PipelineOperationId,
}

impl OperationGraph {
    pub fn new(
        nodes: Vec<PipelineOperation>,
        output: PipelineOperationId,
    ) -> Result<Self, OperationGraphError> {
        if nodes.is_empty() {
            return Err(OperationGraphError::Empty);
        }
        let nodes = BoundedVec::new(nodes)?;
        let mut positions = BTreeMap::new();
        let mut edge_count = 0_usize;
        for (position, node) in nodes.iter().enumerate() {
            if positions.insert(node.id, position).is_some() {
                return Err(OperationGraphError::DuplicateNode(node.id));
            }
            for input in node.inputs.iter().copied() {
                let Some(input_position) = positions.get(&input).copied() else {
                    return Err(OperationGraphError::MissingOrForwardInput {
                        node: node.id,
                        input,
                    });
                };
                if input_position >= position {
                    return Err(OperationGraphError::MissingOrForwardInput {
                        node: node.id,
                        input,
                    });
                }
            }
            edge_count = edge_count
                .checked_add(node.inputs.len())
                .ok_or(BoundError::TooMany {
                    actual: usize::MAX,
                    maximum: MAX_OPERATION_EDGES,
                })?;
            if edge_count > MAX_OPERATION_EDGES {
                return Err(BoundError::TooMany {
                    actual: edge_count,
                    maximum: MAX_OPERATION_EDGES,
                }
                .into());
            }
            validate_cardinality(node, &nodes, &positions)?;
        }
        if !positions.contains_key(&output) {
            return Err(OperationGraphError::MissingOutput(output));
        }
        let graph = Self { nodes, output };
        graph.validate_complete()?;
        Ok(graph)
    }

    pub fn nodes(&self) -> &[PipelineOperation] {
        &self.nodes
    }

    pub const fn output(&self) -> PipelineOperationId {
        self.output
    }

    pub fn pipeline_eligibility(&self) -> PipelineEligibility {
        let mut reasons = Vec::new();
        for node in self.nodes.iter() {
            if let Some(reason) = node.kind.ineligibility()
                && !reasons.contains(&reason)
            {
                reasons.push(reason);
            }
        }
        if reasons.is_empty() {
            PipelineEligibility::DeclaredBaseWriterOnly
        } else {
            PipelineEligibility::Ineligible { reasons }
        }
    }

    pub fn fingerprint(&self) -> BlobId {
        let mut digest = Sha256::new();
        digest.update(b"loom/operation-graph/v1\0");
        digest.update((self.nodes.len() as u64).to_be_bytes());
        for node in self.nodes.iter() {
            digest.update(node.id.as_ulid().to_bytes());
            node.kind.update_digest(&mut digest);
            digest.update((node.inputs.len() as u64).to_be_bytes());
            for input in node.inputs.iter() {
                digest.update(input.as_ulid().to_bytes());
            }
        }
        digest.update(self.output.as_ulid().to_bytes());
        BlobId::from_bytes(digest.finalize().into())
    }

    pub(crate) fn for_assembly_record(
        assembly_id: CandidateAssemblyId,
        spans: &[GeneratedSpanOccurrenceRecord],
    ) -> Result<Self, OperationGraphError> {
        let mut nodes = Vec::with_capacity(spans.len() * 2 + 1);
        let mut assembly_inputs = Vec::with_capacity(spans.len());
        for (index, span) in spans.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| BoundError::TooMany {
                actual: spans.len(),
                maximum: MAX_OPERATION_INPUTS,
            })?;
            let call_node_id = derive_operation_id(
                assembly_id.as_ulid(),
                b"loom/assembly-call-operation/v1",
                index,
            );
            nodes.push(PipelineOperation::new(
                call_node_id,
                PipelineOperationKind::ModelCall {
                    call_id: span.call_id(),
                    evidence_class: span.evidence_class(),
                },
                Vec::new(),
            )?);
            let extract_node_id = derive_operation_id(
                assembly_id.as_ulid(),
                b"loom/assembly-extract-operation/v1",
                index,
            );
            nodes.push(PipelineOperation::new(
                extract_node_id,
                PipelineOperationKind::ExtractSpan {
                    occurrence_id: span.id(),
                },
                vec![call_node_id],
            )?);
            assembly_inputs.push(extract_node_id);
        }
        let output = derive_operation_id(
            assembly_id.as_ulid(),
            b"loom/assembly-output-operation/v1",
            0,
        );
        nodes.push(PipelineOperation::new(
            output,
            PipelineOperationKind::Assemble { assembly_id },
            assembly_inputs,
        )?);
        Self::new(nodes, output)
    }

    pub(crate) fn with_projection(
        &self,
        projection_id: CandidateProjectionId,
    ) -> Result<Self, OperationGraphError> {
        let mut nodes = self.nodes.to_vec();
        let output = derive_operation_id(
            projection_id.as_ulid(),
            b"loom/projection-output-operation/v1",
            0,
        );
        nodes.push(PipelineOperation::new(
            output,
            PipelineOperationKind::Project { projection_id },
            vec![self.output],
        )?);
        Self::new(nodes, output)
    }

    fn validate_complete(&self) -> Result<(), OperationGraphError> {
        let by_id = self
            .nodes
            .iter()
            .map(|node| (node.id, node))
            .collect::<BTreeMap<_, _>>();
        let mut reachable = BTreeSet::new();
        let mut stack = vec![self.output];
        while let Some(id) = stack.pop() {
            if reachable.insert(id) {
                let node = by_id
                    .get(&id)
                    .ok_or(OperationGraphError::MissingOutput(id))?;
                stack.extend(node.inputs.iter().copied());
            }
        }
        if reachable.len() != self.nodes.len() {
            return Err(OperationGraphError::Disconnected {
                reachable: reachable.len(),
                total: self.nodes.len(),
            });
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationGraphWire {
    nodes: BoundedVec<PipelineOperation, MAX_OPERATION_NODES>,
    output: PipelineOperationId,
}

impl<'de> Deserialize<'de> for OperationGraph {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = OperationGraphWire::deserialize(deserializer)?;
        Self::new(wire.nodes.into_inner(), wire.output).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PipelineEligibility {
    /// Every text-producing node declares base-writer provenance. This remains
    /// a claim until `loom-inference` consumes the native opaque generation
    /// seal and mints a `VerifiedInferenceEnvelope`.
    DeclaredBaseWriterOnly,
    Ineligible {
        reasons: Vec<PipelineIneligibility>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipelineIneligibility {
    NonBaseWriterCall(CallEvidenceClass),
    HumanText,
    InstructEditorText,
    CriticText,
    CodexText,
    FixtureText,
    HistoricalText,
    LiteralText,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum OperationGraphError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error("operation graph is empty")]
    Empty,
    #[error("duplicate operation node {0}")]
    DuplicateNode(PipelineOperationId),
    #[error("operation {0} repeats an input")]
    DuplicateInput(PipelineOperationId),
    #[error("operation {node} refers to absent or forward input {input}")]
    MissingOrForwardInput {
        node: PipelineOperationId,
        input: PipelineOperationId,
    },
    #[error("operation graph output {0} is absent")]
    MissingOutput(PipelineOperationId),
    #[error("operation graph has disconnected nodes: {reachable} of {total} reach the output")]
    Disconnected { reachable: usize, total: usize },
    #[error("operation {node} has invalid input cardinality for {kind}")]
    InvalidInputCardinality {
        node: PipelineOperationId,
        kind: &'static str,
    },
    #[error("operation {node} input {input} has the wrong kind")]
    InvalidInputKind {
        node: PipelineOperationId,
        input: PipelineOperationId,
    },
}

fn validate_cardinality(
    node: &PipelineOperation,
    nodes: &[PipelineOperation],
    positions: &BTreeMap<PipelineOperationId, usize>,
) -> Result<(), OperationGraphError> {
    let expected = match node.kind {
        PipelineOperationKind::ModelCall { .. }
        | PipelineOperationKind::CriticText { .. }
        | PipelineOperationKind::CodexText { .. }
        | PipelineOperationKind::FixtureText { .. }
        | PipelineOperationKind::HistoricalText { .. }
        | PipelineOperationKind::LiteralText { .. } => 0..=0,
        PipelineOperationKind::ExtractSpan { .. } | PipelineOperationKind::Project { .. } => 1..=1,
        PipelineOperationKind::Assemble { .. }
        | PipelineOperationKind::HumanTransformation { .. }
        | PipelineOperationKind::InstructEditorTransformation { .. } => 1..=MAX_OPERATION_INPUTS,
    };
    if !expected.contains(&node.inputs.len()) {
        return Err(OperationGraphError::InvalidInputCardinality {
            node: node.id,
            kind: operation_name(&node.kind),
        });
    }

    for input in node.inputs.iter() {
        let input_node =
            &nodes[*positions
                .get(input)
                .ok_or(OperationGraphError::MissingOrForwardInput {
                    node: node.id,
                    input: *input,
                })?];
        let valid = match node.kind {
            PipelineOperationKind::ExtractSpan { .. } => {
                matches!(input_node.kind, PipelineOperationKind::ModelCall { .. })
            }
            PipelineOperationKind::Assemble { .. } => {
                matches!(input_node.kind, PipelineOperationKind::ExtractSpan { .. })
            }
            PipelineOperationKind::Project { .. } => {
                matches!(input_node.kind, PipelineOperationKind::Assemble { .. })
            }
            _ => true,
        };
        if !valid {
            return Err(OperationGraphError::InvalidInputKind {
                node: node.id,
                input: *input,
            });
        }
    }
    Ok(())
}

const fn operation_name(kind: &PipelineOperationKind) -> &'static str {
    match kind {
        PipelineOperationKind::ModelCall { .. } => "model_call",
        PipelineOperationKind::ExtractSpan { .. } => "extract_span",
        PipelineOperationKind::Assemble { .. } => "assemble",
        PipelineOperationKind::Project { .. } => "project",
        PipelineOperationKind::HumanTransformation { .. } => "human_transformation",
        PipelineOperationKind::InstructEditorTransformation { .. } => {
            "instruct_editor_transformation"
        }
        PipelineOperationKind::CriticText { .. } => "critic_text",
        PipelineOperationKind::CodexText { .. } => "codex_text",
        PipelineOperationKind::FixtureText { .. } => "fixture_text",
        PipelineOperationKind::HistoricalText { .. } => "historical_text",
        PipelineOperationKind::LiteralText { .. } => "literal_text",
    }
}

const fn call_evidence_tag(class: CallEvidenceClass) -> u8 {
    match class {
        CallEvidenceClass::LiveBaseWriterClaim => 0,
        CallEvidenceClass::LiveInstructEditorClaim => 1,
        CallEvidenceClass::LiveLocalCriticClaim => 2,
        CallEvidenceClass::LiveCodexCriticClaim => 3,
        CallEvidenceClass::Fixture => 4,
        CallEvidenceClass::Mock => 5,
        CallEvidenceClass::HistoricalReceipt => 6,
    }
}

fn derive_operation_id(parent: Ulid, domain: &[u8], index: u32) -> PipelineOperationId {
    let parent_bytes = parent.to_bytes();
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(parent_bytes);
    digest.update(index.to_be_bytes());
    let digest = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&parent_bytes[..6]);
    bytes[6..].copy_from_slice(&digest[..10]);
    PipelineOperationId::from_ulid(Ulid::from_bytes(bytes))
}
