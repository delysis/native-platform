use std::collections::{BTreeMap, BTreeSet};

use loom_types::{ArtifactId, BlobId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const MAX_EMBEDDING_DIMENSIONS: usize = 65_536;
pub const MAX_EMBEDDING_INPUT_TOKENS: u32 = 1_048_576;
pub const MAX_LEARNING_EXAMPLES: usize = 1_000_000;

const EMBEDDING_DOMAIN: &[u8] = b"loom/frozen-embedding/v1\0";
const GROUPS_DOMAIN: &[u8] = b"loom/leakage-groups/v1\0";
const SPLIT_AUDIT_DOMAIN: &[u8] = b"loom/group-disjoint-split-audit/v1\0";

/// The role a frozen embedding played when it was extracted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingRole {
    Prefix,
    Continuation,
    FinalSegment,
    Plan,
}

impl EmbeddingRole {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Prefix => 0,
            Self::Continuation => 1,
            Self::FinalSegment => 2,
            Self::Plan => 3,
        }
    }
}

/// Receipt-referencing inputs used to construct one frozen embedding.
///
/// The caller supplies the evidence fingerprints. This crate validates shape,
/// finiteness, and exact-bit identity but does not verify backend receipts or
/// grant inference authority.
#[derive(Clone, Debug)]
pub struct FrozenEmbeddingInput {
    pub occurrence_id: ArtifactId,
    pub source_blob_id: BlobId,
    pub model_fingerprint: BlobId,
    pub tokenizer_fingerprint: BlobId,
    pub extraction_fingerprint: BlobId,
    pub role: EmbeddingRole,
    pub input_token_count: u32,
    pub values: Vec<f32>,
}

/// Finite, immutable embedding values bound to exact extraction evidence.
///
/// Deserialization is deliberately unavailable. A dataset owner rebuilds this
/// value from its independently checked embedding receipt, recomputing the
/// exact-bit fingerprint instead of trusting persisted derived fields. The
/// value remains non-authorizing even when its receipt was checked elsewhere.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FrozenEmbedding {
    occurrence_id: ArtifactId,
    source_blob_id: BlobId,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    extraction_fingerprint: BlobId,
    role: EmbeddingRole,
    input_token_count: u32,
    values: Vec<f32>,
    fingerprint: BlobId,
}

impl FrozenEmbedding {
    pub fn new(input: FrozenEmbeddingInput) -> Result<Self, DatasetError> {
        if input.values.is_empty() || input.values.len() > MAX_EMBEDDING_DIMENSIONS {
            return Err(DatasetError::InvalidEmbeddingDimension(input.values.len()));
        }
        if input.values.iter().any(|value| !value.is_finite()) {
            return Err(DatasetError::NonFiniteEmbedding);
        }
        if input.input_token_count == 0 || input.input_token_count > MAX_EMBEDDING_INPUT_TOKENS {
            return Err(DatasetError::InvalidEmbeddingTokenCount(
                input.input_token_count,
            ));
        }
        let fingerprint = fingerprint_embedding(&input);
        Ok(Self {
            occurrence_id: input.occurrence_id,
            source_blob_id: input.source_blob_id,
            model_fingerprint: input.model_fingerprint,
            tokenizer_fingerprint: input.tokenizer_fingerprint,
            extraction_fingerprint: input.extraction_fingerprint,
            role: input.role,
            input_token_count: input.input_token_count,
            values: input.values,
            fingerprint,
        })
    }

    pub const fn occurrence_id(&self) -> ArtifactId {
        self.occurrence_id
    }

    pub const fn source_blob_id(&self) -> BlobId {
        self.source_blob_id
    }

    pub const fn model_fingerprint(&self) -> BlobId {
        self.model_fingerprint
    }

    pub const fn tokenizer_fingerprint(&self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn extraction_fingerprint(&self) -> BlobId {
        self.extraction_fingerprint
    }

    pub const fn role(&self) -> EmbeddingRole {
        self.role
    }

    pub const fn input_token_count(&self) -> u32 {
        self.input_token_count
    }

    pub fn values(&self) -> &[f32] {
        &self.values
    }

    pub const fn dimension(&self) -> usize {
        self.values.len()
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

/// All group identities capable of leaking literary or prompt-family signal.
/// Every identity must remain in exactly one dataset partition.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct LeakageGroups {
    project_ancestry: BlobId,
    sibling_pool: BlobId,
    author_work: BlobId,
    prompt_family: BlobId,
    near_duplicate_cluster: BlobId,
    fingerprint: BlobId,
}

impl LeakageGroups {
    pub fn new(
        project_ancestry: BlobId,
        sibling_pool: BlobId,
        author_work: BlobId,
        prompt_family: BlobId,
        near_duplicate_cluster: BlobId,
    ) -> Self {
        let fingerprint = fingerprint_groups([
            project_ancestry,
            sibling_pool,
            author_work,
            prompt_family,
            near_duplicate_cluster,
        ]);
        Self {
            project_ancestry,
            sibling_pool,
            author_work,
            prompt_family,
            near_duplicate_cluster,
            fingerprint,
        }
    }

    pub const fn project_ancestry(self) -> BlobId {
        self.project_ancestry
    }

    pub const fn sibling_pool(self) -> BlobId {
        self.sibling_pool
    }

    pub const fn author_work(self) -> BlobId {
        self.author_work
    }

    pub const fn prompt_family(self) -> BlobId {
        self.prompt_family
    }

    pub const fn near_duplicate_cluster(self) -> BlobId {
        self.near_duplicate_cluster
    }

    pub const fn fingerprint(self) -> BlobId {
        self.fingerprint
    }

    pub(crate) fn axes(self) -> [(GroupAxis, BlobId); 5] {
        [
            (GroupAxis::ProjectAncestry, self.project_ancestry),
            (GroupAxis::SiblingPool, self.sibling_pool),
            (GroupAxis::AuthorWork, self.author_work),
            (GroupAxis::PromptFamily, self.prompt_family),
            (GroupAxis::NearDuplicateCluster, self.near_duplicate_cluster),
        ]
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetPartition {
    Train,
    Validation,
    Calibration,
}

impl DatasetPartition {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Train => 0,
            Self::Validation => 1,
            Self::Calibration => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum GroupAxis {
    ProjectAncestry,
    SiblingPool,
    AuthorWork,
    PromptFamily,
    NearDuplicateCluster,
}

/// Minimal grouping declaration shared by learned-head datasets.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PartitionedExample {
    example_id: ArtifactId,
    partition: DatasetPartition,
    groups: LeakageGroups,
}

impl PartitionedExample {
    pub const fn new(
        example_id: ArtifactId,
        partition: DatasetPartition,
        groups: LeakageGroups,
    ) -> Self {
        Self {
            example_id,
            partition,
            groups,
        }
    }

    pub const fn example_id(self) -> ArtifactId {
        self.example_id
    }

    pub const fn partition(self) -> DatasetPartition {
        self.partition
    }

    pub const fn groups(self) -> LeakageGroups {
        self.groups
    }
}

/// Canonical evidence that all declared leakage axes are partition-disjoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct SplitAudit {
    fingerprint: BlobId,
    train_examples: usize,
    validation_examples: usize,
    calibration_examples: usize,
    distinct_groups: usize,
}

impl SplitAudit {
    pub const fn fingerprint(self) -> BlobId {
        self.fingerprint
    }

    pub const fn train_examples(self) -> usize {
        self.train_examples
    }

    pub const fn validation_examples(self) -> usize {
        self.validation_examples
    }

    pub const fn calibration_examples(self) -> usize {
        self.calibration_examples
    }

    pub const fn distinct_groups(self) -> usize {
        self.distinct_groups
    }
}

/// Validates every required group axis and fingerprints the canonical audit.
///
/// Input ordering does not affect the fingerprint. This function does not
/// require all three partitions because some projection datasets do not use a
/// calibration partition; consumers that need calibration must require it.
pub fn audit_group_disjoint_splits(
    examples: &[PartitionedExample],
) -> Result<SplitAudit, DatasetError> {
    if examples.is_empty() || examples.len() > MAX_LEARNING_EXAMPLES {
        return Err(DatasetError::InvalidExampleCount(examples.len()));
    }
    let mut example_ids = BTreeSet::new();
    let mut assignments = BTreeMap::<(GroupAxis, BlobId), DatasetPartition>::new();
    let mut canonical = examples.to_vec();
    canonical.sort_unstable_by_key(|example| example.example_id);

    let mut partition_counts = [0_usize; 3];
    for example in &canonical {
        if !example_ids.insert(example.example_id) {
            return Err(DatasetError::DuplicateExample(example.example_id));
        }
        partition_counts[usize::from(example.partition.tag())] += 1;
        for (axis, group) in example.groups.axes() {
            if let Some(existing) = assignments.insert((axis, group), example.partition)
                && existing != example.partition
            {
                return Err(DatasetError::GroupLeakage {
                    axis,
                    group,
                    first: existing,
                    second: example.partition,
                });
            }
        }
    }

    let mut digest = Sha256::new();
    digest.update(SPLIT_AUDIT_DOMAIN);
    digest.update((canonical.len() as u64).to_be_bytes());
    for example in canonical {
        digest.update(example.example_id.as_ulid().to_bytes());
        digest.update([example.partition.tag()]);
        digest.update(example.groups.fingerprint().as_bytes());
    }
    Ok(SplitAudit {
        fingerprint: BlobId::from_bytes(digest.finalize().into()),
        train_examples: partition_counts[0],
        validation_examples: partition_counts[1],
        calibration_examples: partition_counts[2],
        distinct_groups: assignments.len(),
    })
}

fn fingerprint_embedding(input: &FrozenEmbeddingInput) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(EMBEDDING_DOMAIN);
    digest.update(input.occurrence_id.as_ulid().to_bytes());
    digest.update(input.source_blob_id.as_bytes());
    digest.update(input.model_fingerprint.as_bytes());
    digest.update(input.tokenizer_fingerprint.as_bytes());
    digest.update(input.extraction_fingerprint.as_bytes());
    digest.update([input.role.tag()]);
    digest.update(input.input_token_count.to_be_bytes());
    digest.update((input.values.len() as u64).to_be_bytes());
    for value in &input.values {
        digest.update(value.to_bits().to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_groups(values: [BlobId; 5]) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(GROUPS_DOMAIN);
    for value in values {
        digest.update(value.as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum DatasetError {
    #[error("embedding dimension {0} is outside 1..={MAX_EMBEDDING_DIMENSIONS}")]
    InvalidEmbeddingDimension(usize),
    #[error("embedding contains a non-finite value")]
    NonFiniteEmbedding,
    #[error("embedding input token count {0} is outside 1..={MAX_EMBEDDING_INPUT_TOKENS}")]
    InvalidEmbeddingTokenCount(u32),
    #[error("learning example count {0} is outside 1..={MAX_LEARNING_EXAMPLES}")]
    InvalidExampleCount(usize),
    #[error("learning example {0} occurs more than once")]
    DuplicateExample(ArtifactId),
    #[error("{axis:?} group {group} crosses {first:?} and {second:?} partitions")]
    GroupLeakage {
        axis: GroupAxis,
        group: BlobId,
        first: DatasetPartition,
        second: DatasetPartition,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(label: &[u8]) -> LeakageGroups {
        LeakageGroups::new(
            BlobId::digest(&[label, b"project"].concat()),
            BlobId::digest(&[label, b"sibling"].concat()),
            BlobId::digest(&[label, b"work"].concat()),
            BlobId::digest(&[label, b"prompt"].concat()),
            BlobId::digest(&[label, b"duplicate"].concat()),
        )
    }

    fn embedding(values: Vec<f32>) -> Result<FrozenEmbedding, DatasetError> {
        FrozenEmbedding::new(FrozenEmbeddingInput {
            occurrence_id: ArtifactId::new(),
            source_blob_id: BlobId::digest(b"source"),
            model_fingerprint: BlobId::digest(b"model"),
            tokenizer_fingerprint: BlobId::digest(b"tokenizer"),
            extraction_fingerprint: BlobId::digest(b"extraction"),
            role: EmbeddingRole::Prefix,
            input_token_count: 2,
            values,
        })
    }

    #[test]
    fn embeddings_reject_nonfinite_values_and_bind_exact_bits() {
        let base = embedding(vec![0.0, -0.0]).expect("embedding");
        let changed = FrozenEmbedding::new(FrozenEmbeddingInput {
            occurrence_id: base.occurrence_id(),
            source_blob_id: base.source_blob_id(),
            model_fingerprint: base.model_fingerprint(),
            tokenizer_fingerprint: base.tokenizer_fingerprint(),
            extraction_fingerprint: base.extraction_fingerprint(),
            role: base.role(),
            input_token_count: base.input_token_count(),
            values: vec![-0.0, -0.0],
        })
        .expect("embedding");
        assert_ne!(base.fingerprint(), changed.fingerprint());
        assert_eq!(
            embedding(vec![f32::NAN]),
            Err(DatasetError::NonFiniteEmbedding)
        );
    }

    #[test]
    fn every_group_axis_is_disjoint_and_audit_is_order_independent() {
        let train =
            PartitionedExample::new(ArtifactId::new(), DatasetPartition::Train, groups(b"train"));
        let calibration = PartitionedExample::new(
            ArtifactId::new(),
            DatasetPartition::Calibration,
            groups(b"calibration"),
        );
        let forward = audit_group_disjoint_splits(&[train, calibration]).expect("disjoint");
        let reverse = audit_group_disjoint_splits(&[calibration, train]).expect("disjoint");
        assert_eq!(forward.fingerprint(), reverse.fingerprint());
        assert_eq!(forward.train_examples(), 1);
        assert_eq!(forward.calibration_examples(), 1);

        let leaking = LeakageGroups::new(
            calibration.groups().project_ancestry(),
            calibration.groups().sibling_pool(),
            calibration.groups().author_work(),
            train.groups().prompt_family(),
            calibration.groups().near_duplicate_cluster(),
        );
        assert!(matches!(
            audit_group_disjoint_splits(&[
                train,
                PartitionedExample::new(
                    calibration.example_id(),
                    DatasetPartition::Validation,
                    leaking,
                ),
            ]),
            Err(DatasetError::GroupLeakage {
                axis: GroupAxis::PromptFamily,
                ..
            })
        ));
    }
}
