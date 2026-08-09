use std::collections::{BTreeMap, BTreeSet, VecDeque};

use loom_types::{BlobId, RevisionId};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    BoundError, BoundedText, BoundedVec, MAX_SOURCE_BYTES, NonEmptyBoundedVec, NonEmptyByteRange,
    RangeError, StoryGraphId, StoryNodeId, StoryRelationId, StoryStateFactId, StoryStateId,
};

pub const MAX_STORY_NODES: usize = 16_384;
pub const MAX_STORY_RELATIONS: usize = 65_536;
pub const MAX_STORY_NODE_NAME_BYTES: usize = 512;
pub const MAX_STORY_RELATION_DESCRIPTION_BYTES: usize = 2_048;
pub const MAX_STORY_STATE_TEXT_BYTES: usize = 4_096;
pub const MAX_STORY_STATE_SUBJECT_BYTES: usize = 512;
pub const MAX_STORY_STATE_UNKNOWN_REASON_BYTES: usize = 1_024;
pub const MAX_STORY_STATE_EVIDENCE_SPANS: usize = 16;
pub const MAX_STORY_STATE_FACTS: usize = 4_096;

pub type StoryNodeName = BoundedText<MAX_STORY_NODE_NAME_BYTES>;
pub type StoryRelationDescription = BoundedText<MAX_STORY_RELATION_DESCRIPTION_BYTES>;
pub type StoryStateText = BoundedText<MAX_STORY_STATE_TEXT_BYTES>;
pub type StoryStateSubject = BoundedText<MAX_STORY_STATE_SUBJECT_BYTES>;
pub type StoryStateUnknownReason = BoundedText<MAX_STORY_STATE_UNKNOWN_REASON_BYTES>;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryNodeKind {
    Book,
    Part,
    Chapter,
    Scene,
    Movement,
    Beat,
}

impl StoryNodeKind {
    const fn permits_parent(self, parent: Self) -> bool {
        match self {
            Self::Book => false,
            Self::Part => matches!(parent, Self::Book),
            Self::Chapter => matches!(parent, Self::Book | Self::Part),
            Self::Scene => matches!(parent, Self::Chapter),
            Self::Movement => matches!(parent, Self::Scene),
            Self::Beat => matches!(parent, Self::Movement),
        }
    }

    const fn can_anchor_state(self) -> bool {
        matches!(
            self,
            Self::Chapter | Self::Scene | Self::Movement | Self::Beat
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StoryNode {
    id: StoryNodeId,
    kind: StoryNodeKind,
    parent_id: Option<StoryNodeId>,
    sibling_ordinal: u32,
    name: StoryNodeName,
}

impl StoryNode {
    pub const fn new(
        id: StoryNodeId,
        kind: StoryNodeKind,
        parent_id: Option<StoryNodeId>,
        sibling_ordinal: u32,
        name: StoryNodeName,
    ) -> Self {
        Self {
            id,
            kind,
            parent_id,
            sibling_ordinal,
            name,
        }
    }

    pub const fn id(&self) -> StoryNodeId {
        self.id
    }

    pub const fn kind(&self) -> StoryNodeKind {
        self.kind
    }

    pub const fn parent_id(&self) -> Option<StoryNodeId> {
        self.parent_id
    }

    pub const fn sibling_ordinal(&self) -> u32 {
        self.sibling_ordinal
    }

    pub const fn name(&self) -> &StoryNodeName {
        &self.name
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoryNodeWire {
    id: StoryNodeId,
    kind: StoryNodeKind,
    parent_id: Option<StoryNodeId>,
    sibling_ordinal: u32,
    name: StoryNodeName,
}

impl<'de> Deserialize<'de> for StoryNode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StoryNodeWire::deserialize(deserializer)?;
        Ok(Self::new(
            wire.id,
            wire.kind,
            wire.parent_id,
            wire.sibling_ordinal,
            wire.name,
        ))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryRelationKind {
    Causal,
    Temporal,
    Requirement,
    Reveal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StoryRelation {
    id: StoryRelationId,
    kind: StoryRelationKind,
    from: StoryNodeId,
    to: StoryNodeId,
    description: StoryRelationDescription,
}

impl StoryRelation {
    pub fn new(
        id: StoryRelationId,
        kind: StoryRelationKind,
        from: StoryNodeId,
        to: StoryNodeId,
        description: StoryRelationDescription,
    ) -> Result<Self, StoryGraphError> {
        if from == to {
            return Err(StoryGraphError::SelfRelation {
                relation: id,
                node: from,
            });
        }
        Ok(Self {
            id,
            kind,
            from,
            to,
            description,
        })
    }

    pub const fn id(&self) -> StoryRelationId {
        self.id
    }

    pub const fn kind(&self) -> StoryRelationKind {
        self.kind
    }

    pub const fn from(&self) -> StoryNodeId {
        self.from
    }

    pub const fn to(&self) -> StoryNodeId {
        self.to
    }

    pub const fn description(&self) -> &StoryRelationDescription {
        &self.description
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoryRelationWire {
    id: StoryRelationId,
    kind: StoryRelationKind,
    from: StoryNodeId,
    to: StoryNodeId,
    description: StoryRelationDescription,
}

impl<'de> Deserialize<'de> for StoryRelation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StoryRelationWire::deserialize(deserializer)?;
        Self::new(wire.id, wire.kind, wire.from, wire.to, wire.description)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StoryGraph {
    id: StoryGraphId,
    nodes: BoundedVec<StoryNode, MAX_STORY_NODES>,
    relations: BoundedVec<StoryRelation, MAX_STORY_RELATIONS>,
}

impl StoryGraph {
    pub fn new(
        id: StoryGraphId,
        nodes: Vec<StoryNode>,
        relations: Vec<StoryRelation>,
    ) -> Result<Self, StoryGraphError> {
        if nodes.is_empty() {
            return Err(StoryGraphError::Empty);
        }
        let nodes = BoundedVec::new(nodes)?;
        let relations = BoundedVec::new(relations)?;
        validate_story_hierarchy(&nodes)?;
        validate_story_relations(&nodes, &relations)?;
        Ok(Self {
            id,
            nodes,
            relations,
        })
    }

    pub const fn id(&self) -> StoryGraphId {
        self.id
    }

    pub fn nodes(&self) -> &[StoryNode] {
        &self.nodes
    }

    pub fn relations(&self) -> &[StoryRelation] {
        &self.relations
    }

    pub fn node(&self, id: StoryNodeId) -> Option<&StoryNode> {
        self.nodes.iter().find(|node| node.id == id)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoryGraphWire {
    id: StoryGraphId,
    nodes: BoundedVec<StoryNode, MAX_STORY_NODES>,
    relations: BoundedVec<StoryRelation, MAX_STORY_RELATIONS>,
}

impl<'de> Deserialize<'de> for StoryGraph {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StoryGraphWire::deserialize(deserializer)?;
        Self::new(
            wire.id,
            wire.nodes.into_inner(),
            wire.relations.into_inner(),
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StoryGraphError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error("story graph is empty")]
    Empty,
    #[error("story graph repeats node {0}")]
    DuplicateNode(StoryNodeId),
    #[error("story graph must contain exactly one Book root")]
    InvalidBookRoot,
    #[error("story node {node} refers to absent or forward parent {parent}")]
    MissingOrForwardParent {
        node: StoryNodeId,
        parent: StoryNodeId,
    },
    #[error("story node {node} of kind {kind:?} cannot have parent kind {parent_kind:?}")]
    InvalidParentKind {
        node: StoryNodeId,
        kind: StoryNodeKind,
        parent_kind: StoryNodeKind,
    },
    #[error("story node {node} has sibling ordinal {actual}; expected {expected}")]
    InvalidSiblingOrder {
        node: StoryNodeId,
        actual: u32,
        expected: u32,
    },
    #[error("story graph repeats relation {0}")]
    DuplicateRelation(StoryRelationId),
    #[error("story graph repeats a {kind:?} relation from {from} to {to}")]
    DuplicateRelationEndpoints {
        kind: StoryRelationKind,
        from: StoryNodeId,
        to: StoryNodeId,
    },
    #[error("story relation {relation} is a self-edge on {node}")]
    SelfRelation {
        relation: StoryRelationId,
        node: StoryNodeId,
    },
    #[error("story relation {relation} refers to absent node {node}")]
    MissingRelationNode {
        relation: StoryRelationId,
        node: StoryNodeId,
    },
    #[error("causal/temporal/requirement/reveal relations contain a cycle")]
    SemanticCycle,
}

fn validate_story_hierarchy(nodes: &[StoryNode]) -> Result<(), StoryGraphError> {
    let mut kinds = BTreeMap::new();
    let mut next_child_ordinal = BTreeMap::<StoryNodeId, u32>::new();
    let mut root = None;
    for node in nodes {
        if kinds.contains_key(&node.id) {
            return Err(StoryGraphError::DuplicateNode(node.id));
        }
        match (node.kind, node.parent_id) {
            (StoryNodeKind::Book, None) if node.sibling_ordinal == 0 && root.is_none() => {
                root = Some(node.id);
            }
            (StoryNodeKind::Book, _) | (_, None) => {
                return Err(StoryGraphError::InvalidBookRoot);
            }
            (kind, Some(parent)) => {
                let Some(parent_kind) = kinds.get(&parent).copied() else {
                    return Err(StoryGraphError::MissingOrForwardParent {
                        node: node.id,
                        parent,
                    });
                };
                if !kind.permits_parent(parent_kind) {
                    return Err(StoryGraphError::InvalidParentKind {
                        node: node.id,
                        kind,
                        parent_kind,
                    });
                }
                let expected = next_child_ordinal.entry(parent).or_default();
                if node.sibling_ordinal != *expected {
                    return Err(StoryGraphError::InvalidSiblingOrder {
                        node: node.id,
                        actual: node.sibling_ordinal,
                        expected: *expected,
                    });
                }
                *expected =
                    expected
                        .checked_add(1)
                        .ok_or(StoryGraphError::InvalidSiblingOrder {
                            node: node.id,
                            actual: node.sibling_ordinal,
                            expected: u32::MAX,
                        })?;
            }
        }
        kinds.insert(node.id, node.kind);
    }
    if root.is_none() {
        return Err(StoryGraphError::InvalidBookRoot);
    }
    Ok(())
}

fn validate_story_relations(
    nodes: &[StoryNode],
    relations: &[StoryRelation],
) -> Result<(), StoryGraphError> {
    let node_ids = nodes.iter().map(|node| node.id).collect::<BTreeSet<_>>();
    let mut relation_ids = BTreeSet::new();
    let mut endpoints = BTreeSet::new();
    let mut indegree = node_ids
        .iter()
        .copied()
        .map(|id| (id, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing = BTreeMap::<StoryNodeId, Vec<StoryNodeId>>::new();
    for relation in relations {
        if !relation_ids.insert(relation.id) {
            return Err(StoryGraphError::DuplicateRelation(relation.id));
        }
        if relation.from == relation.to {
            return Err(StoryGraphError::SelfRelation {
                relation: relation.id,
                node: relation.from,
            });
        }
        for node in [relation.from, relation.to] {
            if !node_ids.contains(&node) {
                return Err(StoryGraphError::MissingRelationNode {
                    relation: relation.id,
                    node,
                });
            }
        }
        if !endpoints.insert((relation.kind, relation.from, relation.to)) {
            return Err(StoryGraphError::DuplicateRelationEndpoints {
                kind: relation.kind,
                from: relation.from,
                to: relation.to,
            });
        }
        let degree =
            indegree
                .get_mut(&relation.to)
                .ok_or(StoryGraphError::MissingRelationNode {
                    relation: relation.id,
                    node: relation.to,
                })?;
        *degree = degree
            .checked_add(1)
            .ok_or(StoryGraphError::SemanticCycle)?;
        outgoing.entry(relation.from).or_default().push(relation.to);
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect::<VecDeque<_>>();
    let mut visited = 0_usize;
    while let Some(node) = ready.pop_front() {
        visited += 1;
        if let Some(destinations) = outgoing.get(&node) {
            for destination in destinations {
                let degree = indegree
                    .get_mut(destination)
                    .ok_or(StoryGraphError::SemanticCycle)?;
                *degree = degree
                    .checked_sub(1)
                    .ok_or(StoryGraphError::SemanticCycle)?;
                if *degree == 0 {
                    ready.push_back(*destination);
                }
            }
        }
    }
    if visited != nodes.len() {
        return Err(StoryGraphError::SemanticCycle);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct StateEvidenceSpan {
    source_revision_id: RevisionId,
    source_blob_id: BlobId,
    range: NonEmptyByteRange,
}

impl StateEvidenceSpan {
    pub fn new(
        source_revision_id: RevisionId,
        source_blob_id: BlobId,
        range: NonEmptyByteRange,
    ) -> Result<Self, StoryStateError> {
        if range.end() > MAX_SOURCE_BYTES as u64 {
            return Err(StoryStateError::EvidenceRangeTooLarge {
                end: range.end(),
                maximum: MAX_SOURCE_BYTES,
            });
        }
        Ok(Self {
            source_revision_id,
            source_blob_id,
            range,
        })
    }

    pub const fn source_revision_id(self) -> RevisionId {
        self.source_revision_id
    }

    pub const fn source_blob_id(self) -> BlobId {
        self.source_blob_id
    }

    pub const fn range(self) -> NonEmptyByteRange {
        self.range
    }

    /// Verifies this evidence reference against the exact source revision and bytes.
    ///
    /// The caller supplies the revision selected by its immutable source store. The
    /// span is admitted only when that revision matches, the bytes hash to the
    /// recorded blob, the complete source is bounded valid UTF-8, and the range is
    /// in bounds on UTF-8 boundaries.
    pub fn verified_source_text<'a>(
        &self,
        expected_revision_id: RevisionId,
        source_bytes: &'a [u8],
    ) -> Result<&'a str, StoryStateError> {
        if self.source_revision_id != expected_revision_id {
            return Err(StoryStateError::EvidenceRevisionMismatch {
                expected: expected_revision_id,
                actual: self.source_revision_id,
            });
        }
        let _ = crate::range::validate_source_utf8(source_bytes)?;
        let actual_blob_id = BlobId::digest(source_bytes);
        if self.source_blob_id != actual_blob_id {
            return Err(StoryStateError::EvidenceBlobMismatch {
                expected: self.source_blob_id,
                actual: actual_blob_id,
            });
        }
        Ok(self.range.checked_str(source_bytes)?)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateEvidenceSpanWire {
    #[serde(deserialize_with = "crate::bounded::deserialize_revision_id")]
    source_revision_id: RevisionId,
    #[serde(deserialize_with = "crate::bounded::deserialize_blob_id")]
    source_blob_id: BlobId,
    range: NonEmptyByteRange,
}

impl<'de> Deserialize<'de> for StateEvidenceSpan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StateEvidenceSpanWire::deserialize(deserializer)?;
        Self::new(wire.source_revision_id, wire.source_blob_id, wire.range)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GroundedStateText {
    value: StoryStateText,
    evidence: NonEmptyBoundedVec<StateEvidenceSpan, MAX_STORY_STATE_EVIDENCE_SPANS>,
}

impl GroundedStateText {
    pub fn new(
        value: impl Into<String>,
        evidence: Vec<StateEvidenceSpan>,
    ) -> Result<Self, StoryStateError> {
        let value = StoryStateText::new(value)?;
        let evidence = NonEmptyBoundedVec::new(evidence)?;
        validate_unique_evidence(&evidence)?;
        Ok(Self { value, evidence })
    }

    pub const fn value(&self) -> &StoryStateText {
        &self.value
    }

    pub fn evidence(&self) -> &[StateEvidenceSpan] {
        &self.evidence
    }

    pub fn verify_source(
        &self,
        expected_revision_id: RevisionId,
        source_bytes: &[u8],
    ) -> Result<(), StoryStateError> {
        for span in self.evidence.iter() {
            let excerpt = span.verified_source_text(expected_revision_id, source_bytes)?;
            if excerpt.is_empty() {
                return Err(StoryStateError::EmptyVerifiedEvidence);
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroundedStateTextWire {
    value: StoryStateText,
    evidence: NonEmptyBoundedVec<StateEvidenceSpan, MAX_STORY_STATE_EVIDENCE_SPANS>,
}

impl<'de> Deserialize<'de> for GroundedStateText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = GroundedStateTextWire::deserialize(deserializer)?;
        validate_unique_evidence(&wire.evidence).map_err(serde::de::Error::custom)?;
        Ok(Self {
            value: wire.value,
            evidence: wire.evidence,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "status", rename_all = "snake_case")]
pub enum ExplicitStoryStateField {
    Grounded { value: GroundedStateText },
    Unknown { reason: StoryStateUnknownReason },
}

impl ExplicitStoryStateField {
    pub fn grounded(
        value: impl Into<String>,
        evidence: Vec<StateEvidenceSpan>,
    ) -> Result<Self, StoryStateError> {
        Ok(Self::Grounded {
            value: GroundedStateText::new(value, evidence)?,
        })
    }

    pub fn unknown(reason: impl Into<String>) -> Result<Self, StoryStateError> {
        Ok(Self::Unknown {
            reason: StoryStateUnknownReason::new(reason)?,
        })
    }

    pub fn verify_source(
        &self,
        expected_revision_id: RevisionId,
        source_bytes: &[u8],
    ) -> Result<(), StoryStateError> {
        match self {
            Self::Grounded { value } => value.verify_source(expected_revision_id, source_bytes),
            Self::Unknown { .. } => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GroundedStoryStateFact {
    id: StoryStateFactId,
    subject: StoryStateSubject,
    assertion: GroundedStateText,
}

impl GroundedStoryStateFact {
    pub fn new(
        id: StoryStateFactId,
        subject: impl Into<String>,
        assertion: impl Into<String>,
        evidence: Vec<StateEvidenceSpan>,
    ) -> Result<Self, StoryStateError> {
        Ok(Self {
            id,
            subject: StoryStateSubject::new(subject)?,
            assertion: GroundedStateText::new(assertion, evidence)?,
        })
    }

    pub const fn id(&self) -> StoryStateFactId {
        self.id
    }

    pub const fn subject(&self) -> &StoryStateSubject {
        &self.subject
    }

    pub const fn assertion(&self) -> &GroundedStateText {
        &self.assertion
    }

    pub fn verify_source(
        &self,
        expected_revision_id: RevisionId,
        source_bytes: &[u8],
    ) -> Result<(), StoryStateError> {
        self.assertion
            .verify_source(expected_revision_id, source_bytes)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroundedStoryStateFactWire {
    id: StoryStateFactId,
    subject: StoryStateSubject,
    assertion: GroundedStateText,
}

impl<'de> Deserialize<'de> for GroundedStoryStateFact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = GroundedStoryStateFactWire::deserialize(deserializer)?;
        Ok(Self {
            id: wire.id,
            subject: wire.subject,
            assertion: wire.assertion,
        })
    }
}

/// An explicit state category. Established absence is a grounded claim, so a
/// known-empty collection carries nonempty source evidence of that absence.
/// It can never be smuggled in as a bare empty vector.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct GroundedFactCollection(GroundedFactCollectionStatus);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum GroundedFactCollectionStatus {
    KnownNonEmpty {
        facts: NonEmptyBoundedVec<GroundedStoryStateFact, MAX_STORY_STATE_FACTS>,
    },
    KnownEmpty {
        evidence: NonEmptyBoundedVec<StateEvidenceSpan, MAX_STORY_STATE_EVIDENCE_SPANS>,
    },
    Unknown {
        reason: StoryStateUnknownReason,
    },
}

impl GroundedFactCollection {
    pub fn known(facts: Vec<GroundedStoryStateFact>) -> Result<Self, StoryStateError> {
        validate_fact_collections([facts.as_slice()])?;
        Ok(Self(GroundedFactCollectionStatus::KnownNonEmpty {
            facts: NonEmptyBoundedVec::new(facts)?,
        }))
    }

    pub fn known_empty(evidence: Vec<StateEvidenceSpan>) -> Result<Self, StoryStateError> {
        let evidence = NonEmptyBoundedVec::new(evidence)?;
        validate_unique_evidence(&evidence)?;
        Ok(Self(GroundedFactCollectionStatus::KnownEmpty { evidence }))
    }

    pub fn unknown(reason: impl Into<String>) -> Result<Self, StoryStateError> {
        Ok(Self(GroundedFactCollectionStatus::Unknown {
            reason: StoryStateUnknownReason::new(reason)?,
        }))
    }

    pub fn known_facts(&self) -> Option<&[GroundedStoryStateFact]> {
        match &self.0 {
            GroundedFactCollectionStatus::KnownNonEmpty { facts } => Some(facts),
            GroundedFactCollectionStatus::KnownEmpty { .. } => Some(&[]),
            GroundedFactCollectionStatus::Unknown { .. } => None,
        }
    }

    pub fn known_empty_evidence(&self) -> Option<&[StateEvidenceSpan]> {
        match &self.0 {
            GroundedFactCollectionStatus::KnownEmpty { evidence } => Some(evidence),
            GroundedFactCollectionStatus::KnownNonEmpty { .. }
            | GroundedFactCollectionStatus::Unknown { .. } => None,
        }
    }

    pub const fn unknown_reason(&self) -> Option<&StoryStateUnknownReason> {
        match &self.0 {
            GroundedFactCollectionStatus::KnownNonEmpty { .. }
            | GroundedFactCollectionStatus::KnownEmpty { .. } => None,
            GroundedFactCollectionStatus::Unknown { reason } => Some(reason),
        }
    }

    pub fn verify_source(
        &self,
        expected_revision_id: RevisionId,
        source_bytes: &[u8],
    ) -> Result<(), StoryStateError> {
        match &self.0 {
            GroundedFactCollectionStatus::KnownNonEmpty { facts } => {
                for fact in facts.iter() {
                    fact.verify_source(expected_revision_id, source_bytes)?;
                }
            }
            GroundedFactCollectionStatus::KnownEmpty { evidence } => {
                for span in evidence.iter() {
                    let excerpt = span.verified_source_text(expected_revision_id, source_bytes)?;
                    if excerpt.is_empty() {
                        return Err(StoryStateError::EmptyVerifiedEvidence);
                    }
                }
            }
            GroundedFactCollectionStatus::Unknown { .. } => {}
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, tag = "status", rename_all = "snake_case")]
enum GroundedFactCollectionWire {
    KnownNonEmpty {
        facts: NonEmptyBoundedVec<GroundedStoryStateFact, MAX_STORY_STATE_FACTS>,
    },
    KnownEmpty {
        evidence: NonEmptyBoundedVec<StateEvidenceSpan, MAX_STORY_STATE_EVIDENCE_SPANS>,
    },
    Unknown {
        reason: StoryStateUnknownReason,
    },
}

impl<'de> Deserialize<'de> for GroundedFactCollection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match GroundedFactCollectionWire::deserialize(deserializer)? {
            GroundedFactCollectionWire::KnownNonEmpty { facts } => {
                Self::known(facts.into_inner()).map_err(serde::de::Error::custom)
            }
            GroundedFactCollectionWire::KnownEmpty { evidence } => {
                Self::known_empty(evidence.into_inner()).map_err(serde::de::Error::custom)
            }
            GroundedFactCollectionWire::Unknown { reason } => {
                Ok(Self(GroundedFactCollectionStatus::Unknown { reason }))
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StoryState {
    id: StoryStateId,
    story_graph_id: StoryGraphId,
    at_node: StoryNodeId,
    chronology: ExplicitStoryStateField,
    location: ExplicitStoryStateField,
    physical_configuration: GroundedFactCollection,
    character_knowledge: GroundedFactCollection,
    character_conditions: GroundedFactCollection,
    world_facts: GroundedFactCollection,
    unresolved_promises: GroundedFactCollection,
    point_of_view: ExplicitStoryStateField,
    voice_constraints: GroundedFactCollection,
    possible_next_actions: GroundedFactCollection,
}

#[allow(clippy::too_many_arguments)]
impl StoryState {
    pub fn new(
        id: StoryStateId,
        story_graph_id: StoryGraphId,
        at_node: StoryNodeId,
        chronology: ExplicitStoryStateField,
        location: ExplicitStoryStateField,
        physical_configuration: GroundedFactCollection,
        character_knowledge: GroundedFactCollection,
        character_conditions: GroundedFactCollection,
        world_facts: GroundedFactCollection,
        unresolved_promises: GroundedFactCollection,
        point_of_view: ExplicitStoryStateField,
        voice_constraints: GroundedFactCollection,
        possible_next_actions: GroundedFactCollection,
    ) -> Result<Self, StoryStateError> {
        validate_fact_collections([
            physical_configuration.known_facts().unwrap_or(&[]),
            character_knowledge.known_facts().unwrap_or(&[]),
            character_conditions.known_facts().unwrap_or(&[]),
            world_facts.known_facts().unwrap_or(&[]),
            unresolved_promises.known_facts().unwrap_or(&[]),
            voice_constraints.known_facts().unwrap_or(&[]),
            possible_next_actions.known_facts().unwrap_or(&[]),
        ])?;
        Ok(Self {
            id,
            story_graph_id,
            at_node,
            chronology,
            location,
            physical_configuration,
            character_knowledge,
            character_conditions,
            world_facts,
            unresolved_promises,
            point_of_view,
            voice_constraints,
            possible_next_actions,
        })
    }

    pub const fn id(&self) -> StoryStateId {
        self.id
    }

    pub const fn story_graph_id(&self) -> StoryGraphId {
        self.story_graph_id
    }

    pub const fn at_node(&self) -> StoryNodeId {
        self.at_node
    }

    pub const fn chronology(&self) -> &ExplicitStoryStateField {
        &self.chronology
    }

    pub const fn location(&self) -> &ExplicitStoryStateField {
        &self.location
    }

    pub const fn physical_configuration(&self) -> &GroundedFactCollection {
        &self.physical_configuration
    }

    pub const fn character_knowledge(&self) -> &GroundedFactCollection {
        &self.character_knowledge
    }

    pub const fn character_conditions(&self) -> &GroundedFactCollection {
        &self.character_conditions
    }

    pub const fn world_facts(&self) -> &GroundedFactCollection {
        &self.world_facts
    }

    pub const fn unresolved_promises(&self) -> &GroundedFactCollection {
        &self.unresolved_promises
    }

    pub const fn point_of_view(&self) -> &ExplicitStoryStateField {
        &self.point_of_view
    }

    pub const fn voice_constraints(&self) -> &GroundedFactCollection {
        &self.voice_constraints
    }

    pub const fn possible_next_actions(&self) -> &GroundedFactCollection {
        &self.possible_next_actions
    }

    pub fn validate_against_graph(&self, graph: &StoryGraph) -> Result<(), StoryStateError> {
        if self.story_graph_id != graph.id {
            return Err(StoryStateError::WrongStoryGraph {
                expected: self.story_graph_id,
                actual: graph.id,
            });
        }
        let node = graph
            .node(self.at_node)
            .ok_or(StoryStateError::MissingAnchorNode(self.at_node))?;
        if !node.kind.can_anchor_state() {
            return Err(StoryStateError::InvalidAnchorKind(node.kind));
        }
        Ok(())
    }

    pub fn verify_source(
        &self,
        expected_revision_id: RevisionId,
        source_bytes: &[u8],
    ) -> Result<(), StoryStateError> {
        self.chronology
            .verify_source(expected_revision_id, source_bytes)?;
        self.location
            .verify_source(expected_revision_id, source_bytes)?;
        self.physical_configuration
            .verify_source(expected_revision_id, source_bytes)?;
        self.character_knowledge
            .verify_source(expected_revision_id, source_bytes)?;
        self.character_conditions
            .verify_source(expected_revision_id, source_bytes)?;
        self.world_facts
            .verify_source(expected_revision_id, source_bytes)?;
        self.unresolved_promises
            .verify_source(expected_revision_id, source_bytes)?;
        self.point_of_view
            .verify_source(expected_revision_id, source_bytes)?;
        self.voice_constraints
            .verify_source(expected_revision_id, source_bytes)?;
        self.possible_next_actions
            .verify_source(expected_revision_id, source_bytes)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoryStateWire {
    id: StoryStateId,
    story_graph_id: StoryGraphId,
    at_node: StoryNodeId,
    chronology: ExplicitStoryStateField,
    location: ExplicitStoryStateField,
    physical_configuration: GroundedFactCollection,
    character_knowledge: GroundedFactCollection,
    character_conditions: GroundedFactCollection,
    world_facts: GroundedFactCollection,
    unresolved_promises: GroundedFactCollection,
    point_of_view: ExplicitStoryStateField,
    voice_constraints: GroundedFactCollection,
    possible_next_actions: GroundedFactCollection,
}

impl<'de> Deserialize<'de> for StoryState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StoryStateWire::deserialize(deserializer)?;
        Self::new(
            wire.id,
            wire.story_graph_id,
            wire.at_node,
            wire.chronology,
            wire.location,
            wire.physical_configuration,
            wire.character_knowledge,
            wire.character_conditions,
            wire.world_facts,
            wire.unresolved_promises,
            wire.point_of_view,
            wire.voice_constraints,
            wire.possible_next_actions,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StoryStateError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error(transparent)]
    Range(#[from] RangeError),
    #[error("evidence range end {end} exceeds maximum source bytes {maximum}")]
    EvidenceRangeTooLarge { end: u64, maximum: usize },
    #[error("evidence is bound to revision {actual}, expected {expected}")]
    EvidenceRevisionMismatch {
        expected: RevisionId,
        actual: RevisionId,
    },
    #[error("evidence bytes hash to {actual}, expected recorded blob {expected}")]
    EvidenceBlobMismatch { expected: BlobId, actual: BlobId },
    #[error("verified evidence unexpectedly resolved to an empty source slice")]
    EmptyVerifiedEvidence,
    #[error("grounding repeats an evidence span")]
    DuplicateEvidence,
    #[error("story state repeats fact id {0}")]
    DuplicateFact(StoryStateFactId),
    #[error("story state repeats subject/assertion text within one field")]
    DuplicateFactText,
    #[error("story state contains {actual} facts; maximum is {maximum}")]
    TooManyFacts { actual: usize, maximum: usize },
    #[error("story state is bound to graph {expected}, not graph {actual}")]
    WrongStoryGraph {
        expected: StoryGraphId,
        actual: StoryGraphId,
    },
    #[error("story state anchor node {0} is absent")]
    MissingAnchorNode(StoryNodeId),
    #[error("story state cannot anchor at hierarchy kind {0:?}")]
    InvalidAnchorKind(StoryNodeKind),
}

fn validate_unique_evidence(evidence: &[StateEvidenceSpan]) -> Result<(), StoryStateError> {
    let mut unique = BTreeSet::new();
    for span in evidence {
        let key = (
            span.source_revision_id,
            span.source_blob_id,
            span.range.start(),
            span.range.end(),
        );
        if !unique.insert(key) {
            return Err(StoryStateError::DuplicateEvidence);
        }
    }
    Ok(())
}

fn validate_fact_collections<const N: usize>(
    collections: [&[GroundedStoryStateFact]; N],
) -> Result<(), StoryStateError> {
    let total = collections.iter().try_fold(0_usize, |total, facts| {
        total
            .checked_add(facts.len())
            .ok_or(StoryStateError::TooManyFacts {
                actual: usize::MAX,
                maximum: MAX_STORY_STATE_FACTS,
            })
    })?;
    if total > MAX_STORY_STATE_FACTS {
        return Err(StoryStateError::TooManyFacts {
            actual: total,
            maximum: MAX_STORY_STATE_FACTS,
        });
    }
    let mut ids = BTreeSet::new();
    for facts in collections {
        let mut text = BTreeSet::new();
        for fact in facts {
            if !ids.insert(fact.id) {
                return Err(StoryStateError::DuplicateFact(fact.id));
            }
            if !text.insert((fact.subject.as_str(), fact.assertion.value.as_str())) {
                return Err(StoryStateError::DuplicateFactText);
            }
        }
    }
    Ok(())
}
