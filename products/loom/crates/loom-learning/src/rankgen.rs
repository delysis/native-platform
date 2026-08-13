use std::collections::BTreeSet;

use loom_types::{ArtifactId, BlobId};
use ndarray::Array2;
use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore, SeedableRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    Adam, DatasetError, DatasetPartition, FrozenEmbedding, LeakageGroups, MAX_LEARNING_EXAMPLES,
    OptimizerError, PartitionedExample, SplitAudit, audit_group_disjoint_splits,
};

pub const RANKGEN_PROJECTION_RANK: usize = 64;
pub const MAX_RANKGEN_NEGATIVES: usize = 64;
pub const MAX_RANKGEN_EPOCHS: u32 = 512;
pub const MAX_RANKGEN_TRAIN_EXAMPLES: usize = 65_535;

const DATASET_DOMAIN: &[u8] = b"loom/rankgen-dataset/v1\0";
const CONFIG_DOMAIN: &[u8] = b"loom/rankgen-training-config/v1\0";
const MODEL_DOMAIN: &[u8] = b"loom/rankgen-projection-head/v1\0";
const NEGATIVE_EVIDENCE_DOMAIN: &[u8] = b"loom/rankgen-negative-evidence/v1\0";
const INITIAL_WEIGHT_SCALE: f32 = 0.02;

/// The two exact context geometries supported by the projection ranker.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RankGenVariant {
    Local256x128,
    ChapterTransition1024x256,
}

impl RankGenVariant {
    pub const fn prefix_tokens(self) -> u16 {
        match self {
            Self::Local256x128 => 256,
            Self::ChapterTransition1024x256 => 1_024,
        }
    }

    pub const fn continuation_tokens(self) -> u16 {
        match self {
            Self::Local256x128 => 128,
            Self::ChapterTransition1024x256 => 256,
        }
    }

    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Local256x128 => 0,
            Self::ChapterTransition1024x256 => 1,
        }
    }
}

/// Required hard-negative provenance class.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RankGenNegativeClass {
    SameWork,
    Sibling,
    Generated,
    Temporal,
    EntityCorruption,
}

impl RankGenNegativeClass {
    const ALL: [Self; 5] = [
        Self::SameWork,
        Self::Sibling,
        Self::Generated,
        Self::Temporal,
        Self::EntityCorruption,
    ];

    const fn tag(self) -> u8 {
        match self {
            Self::SameWork => 0,
            Self::Sibling => 1,
            Self::Generated => 2,
            Self::Temporal => 3,
            Self::EntityCorruption => 4,
        }
    }
}

/// Exact, non-authorizing derivation declaration for one hard negative.
///
/// The fingerprint is expected to identify a separately checked receipt. This
/// crate cannot verify the receipt, but it does require the declared relation
/// to hold over the exact anchor and candidate occurrences before the example
/// can enter a dataset.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RankGenNegativeDerivation {
    SameWork {
        receipt_fingerprint: BlobId,
    },
    Sibling {
        receipt_fingerprint: BlobId,
    },
    Generated {
        generation_receipt_fingerprint: BlobId,
    },
    Temporal {
        receipt_fingerprint: BlobId,
        anchor_position_fingerprint: BlobId,
        candidate_position_fingerprint: BlobId,
    },
    EntityCorruption {
        transformation_receipt_fingerprint: BlobId,
    },
}

impl RankGenNegativeDerivation {
    const fn class(self) -> RankGenNegativeClass {
        match self {
            Self::SameWork { .. } => RankGenNegativeClass::SameWork,
            Self::Sibling { .. } => RankGenNegativeClass::Sibling,
            Self::Generated { .. } => RankGenNegativeClass::Generated,
            Self::Temporal { .. } => RankGenNegativeClass::Temporal,
            Self::EntityCorruption { .. } => RankGenNegativeClass::EntityCorruption,
        }
    }

    const fn receipt_fingerprint(self) -> BlobId {
        match self {
            Self::SameWork {
                receipt_fingerprint,
            }
            | Self::Sibling {
                receipt_fingerprint,
            }
            | Self::Temporal {
                receipt_fingerprint,
                ..
            } => receipt_fingerprint,
            Self::Generated {
                generation_receipt_fingerprint,
            } => generation_receipt_fingerprint,
            Self::EntityCorruption {
                transformation_receipt_fingerprint,
            } => transformation_receipt_fingerprint,
        }
    }
}

/// Content-addressed proof declaration binding a negative class to exact
/// source and occurrence relationships. It remains training evidence only.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RankGenNegativeEvidence {
    derivation: RankGenNegativeDerivation,
    anchor_occurrence_id: ArtifactId,
    anchor_source_blob_id: BlobId,
    anchor_embedding_fingerprint: BlobId,
    candidate_occurrence_id: ArtifactId,
    candidate_source_blob_id: BlobId,
    candidate_embedding_fingerprint: BlobId,
    relation_group_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl RankGenNegativeEvidence {
    pub fn derive(
        derivation: RankGenNegativeDerivation,
        prefix: &FrozenEmbedding,
        positive: &FrozenEmbedding,
        example_groups: LeakageGroups,
        candidate: &FrozenEmbedding,
        candidate_groups: LeakageGroups,
    ) -> Result<Self, RankGenError> {
        let class = derivation.class();
        let anchor = if class == RankGenNegativeClass::Generated {
            prefix
        } else {
            positive
        };
        if anchor.occurrence_id() == candidate.occurrence_id() {
            return Err(RankGenError::RepeatedEmbeddingOccurrence);
        }
        let relation_group_fingerprint = match derivation {
            RankGenNegativeDerivation::SameWork { .. }
            | RankGenNegativeDerivation::Temporal { .. } => require_negative_group_relation(
                class,
                example_groups.author_work(),
                candidate_groups.author_work(),
            )?,
            RankGenNegativeDerivation::Sibling { .. } => require_negative_group_relation(
                class,
                example_groups.sibling_pool(),
                candidate_groups.sibling_pool(),
            )?,
            RankGenNegativeDerivation::Generated { .. } => require_negative_group_relation(
                class,
                example_groups.prompt_family(),
                candidate_groups.prompt_family(),
            )?,
            RankGenNegativeDerivation::EntityCorruption { .. } => {
                if positive.source_blob_id() == candidate.source_blob_id() {
                    return Err(RankGenError::EntityCorruptionSourceUnchanged);
                }
                require_negative_group_relation(
                    class,
                    example_groups.near_duplicate_cluster(),
                    candidate_groups.near_duplicate_cluster(),
                )?
            }
        };
        if let RankGenNegativeDerivation::Temporal {
            anchor_position_fingerprint,
            candidate_position_fingerprint,
            ..
        } = derivation
            && anchor_position_fingerprint == candidate_position_fingerprint
        {
            return Err(RankGenError::TemporalPositionsNotDistinct);
        }
        let anchor_occurrence_id = anchor.occurrence_id();
        let anchor_source_blob_id = anchor.source_blob_id();
        let anchor_embedding_fingerprint = anchor.fingerprint();
        let candidate_occurrence_id = candidate.occurrence_id();
        let candidate_source_blob_id = candidate.source_blob_id();
        let candidate_embedding_fingerprint = candidate.fingerprint();
        let fingerprint = fingerprint_negative_evidence(&NegativeEvidenceFingerprintInput {
            derivation,
            anchor_occurrence_id,
            anchor_source_blob_id,
            anchor_embedding_fingerprint,
            candidate_occurrence_id,
            candidate_source_blob_id,
            candidate_embedding_fingerprint,
            relation_group_fingerprint,
        });
        Ok(Self {
            derivation,
            anchor_occurrence_id,
            anchor_source_blob_id,
            anchor_embedding_fingerprint,
            candidate_occurrence_id,
            candidate_source_blob_id,
            candidate_embedding_fingerprint,
            relation_group_fingerprint,
            fingerprint,
        })
    }

    pub const fn class(self) -> RankGenNegativeClass {
        self.derivation.class()
    }

    pub const fn receipt_fingerprint(self) -> BlobId {
        self.derivation.receipt_fingerprint()
    }

    pub const fn fingerprint(self) -> BlobId {
        self.fingerprint
    }

    fn verify_binding(
        self,
        prefix: &FrozenEmbedding,
        positive: &FrozenEmbedding,
        example_groups: LeakageGroups,
        candidate: &FrozenEmbedding,
        candidate_groups: LeakageGroups,
    ) -> Result<(), RankGenError> {
        let rebuilt = Self::derive(
            self.derivation,
            prefix,
            positive,
            example_groups,
            candidate,
            candidate_groups,
        )?;
        if rebuilt == self {
            Ok(())
        } else {
            Err(RankGenError::NegativeEvidenceBindingMismatch)
        }
    }
}

#[derive(Clone, Debug)]
pub struct RankGenNegativeInput {
    pub evidence: RankGenNegativeEvidence,
    pub embedding: FrozenEmbedding,
    pub groups: LeakageGroups,
}

#[derive(Clone, Debug)]
struct RankGenNegative {
    evidence: RankGenNegativeEvidence,
    embedding: FrozenEmbedding,
    groups: LeakageGroups,
}

#[derive(Clone, Debug)]
pub struct RankGenExampleInput {
    pub example_id: ArtifactId,
    pub partition: DatasetPartition,
    pub groups: LeakageGroups,
    pub prefix: FrozenEmbedding,
    pub positive: FrozenEmbedding,
    pub negatives: Vec<RankGenNegativeInput>,
}

#[derive(Clone, Debug)]
struct RankGenExample {
    example_id: ArtifactId,
    partition: DatasetPartition,
    groups: LeakageGroups,
    prefix: FrozenEmbedding,
    positive: FrozenEmbedding,
    negatives: Vec<RankGenNegative>,
}

impl RankGenExample {
    fn compile(input: RankGenExampleInput) -> Result<Self, RankGenError> {
        require_role(&input.prefix, crate::EmbeddingRole::Prefix)?;
        require_role(&input.positive, crate::EmbeddingRole::Continuation)?;
        if input.negatives.len() < RankGenNegativeClass::ALL.len()
            || input.negatives.len() > MAX_RANKGEN_NEGATIVES
        {
            return Err(RankGenError::InvalidNegativeCount(input.negatives.len()));
        }
        let mut classes = BTreeSet::new();
        let mut occurrences =
            BTreeSet::from([input.prefix.occurrence_id(), input.positive.occurrence_id()]);
        if occurrences.len() != 2 {
            return Err(RankGenError::RepeatedEmbeddingOccurrence);
        }
        let mut negatives = Vec::with_capacity(input.negatives.len());
        for negative in input.negatives {
            require_role(&negative.embedding, crate::EmbeddingRole::Continuation)?;
            negative.evidence.verify_binding(
                &input.prefix,
                &input.positive,
                input.groups,
                &negative.embedding,
                negative.groups,
            )?;
            classes.insert(negative.evidence.class());
            if !occurrences.insert(negative.embedding.occurrence_id()) {
                return Err(RankGenError::RepeatedEmbeddingOccurrence);
            }
            negatives.push(RankGenNegative {
                evidence: negative.evidence,
                embedding: negative.embedding,
                groups: negative.groups,
            });
        }
        for class in RankGenNegativeClass::ALL {
            if !classes.contains(&class) {
                return Err(RankGenError::MissingNegativeClass(class));
            }
        }
        negatives.sort_unstable_by_key(|negative| {
            (
                negative.evidence.class(),
                negative.embedding.occurrence_id(),
                negative.embedding.fingerprint(),
                negative.evidence.fingerprint(),
            )
        });
        let example = Self {
            example_id: input.example_id,
            partition: input.partition,
            groups: input.groups,
            prefix: input.prefix,
            positive: input.positive,
            negatives,
        };
        example.validate_bindings()?;
        example.validate_distinct_continuation_sources()?;
        Ok(example)
    }

    fn validate_bindings(&self) -> Result<(), RankGenError> {
        let expected_dimension = self.prefix.dimension();
        let expected_model = self.prefix.model_fingerprint();
        let expected_tokenizer = self.prefix.tokenizer_fingerprint();
        for embedding in std::iter::once(&self.positive)
            .chain(self.negatives.iter().map(|negative| &negative.embedding))
        {
            if embedding.dimension() != expected_dimension {
                return Err(RankGenError::EmbeddingDimensionMismatch {
                    expected: expected_dimension,
                    actual: embedding.dimension(),
                });
            }
            if embedding.model_fingerprint() != expected_model
                || embedding.tokenizer_fingerprint() != expected_tokenizer
            {
                return Err(RankGenError::EmbeddingSpaceMismatch);
            }
        }
        Ok(())
    }

    fn validate_distinct_continuation_sources(&self) -> Result<(), RankGenError> {
        let mut sources = BTreeSet::from([self.positive.source_blob_id()]);
        for negative in &self.negatives {
            if !sources.insert(negative.embedding.source_blob_id()) {
                return Err(RankGenError::RepeatedContinuationSource(
                    negative.embedding.source_blob_id(),
                ));
            }
        }
        Ok(())
    }

    fn partition_record(&self) -> PartitionedExample {
        PartitionedExample::new(self.example_id, self.partition, self.groups)
    }
}

/// Canonical, group-disjoint `RankGen` training evidence.
#[derive(Clone, Debug)]
pub struct RankGenDataset {
    examples: Vec<RankGenExample>,
    embedding_dimension: usize,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    split_audit: SplitAudit,
    fingerprint: BlobId,
}

impl RankGenDataset {
    pub fn compile(inputs: Vec<RankGenExampleInput>) -> Result<Self, RankGenError> {
        if inputs.is_empty() || inputs.len() > MAX_LEARNING_EXAMPLES {
            return Err(RankGenError::Dataset(DatasetError::InvalidExampleCount(
                inputs.len(),
            )));
        }
        let mut examples = inputs
            .into_iter()
            .map(RankGenExample::compile)
            .collect::<Result<Vec<_>, _>>()?;
        examples.sort_unstable_by_key(|example| example.example_id);

        let first = &examples[0];
        let embedding_dimension = first.prefix.dimension();
        let model_fingerprint = first.prefix.model_fingerprint();
        let tokenizer_fingerprint = first.prefix.tokenizer_fingerprint();
        for example in &examples {
            if example.prefix.dimension() != embedding_dimension {
                return Err(RankGenError::EmbeddingDimensionMismatch {
                    expected: embedding_dimension,
                    actual: example.prefix.dimension(),
                });
            }
            if example.prefix.model_fingerprint() != model_fingerprint
                || example.prefix.tokenizer_fingerprint() != tokenizer_fingerprint
            {
                return Err(RankGenError::EmbeddingSpaceMismatch);
            }
        }
        let mut occurrences = BTreeSet::new();
        for example in &examples {
            for embedding in std::iter::once(&example.prefix)
                .chain(std::iter::once(&example.positive))
                .chain(example.negatives.iter().map(|negative| &negative.embedding))
            {
                if !occurrences.insert(embedding.occurrence_id()) {
                    return Err(RankGenError::RepeatedEmbeddingOccurrence);
                }
            }
        }

        let mut partition_records = Vec::new();
        for example in &examples {
            partition_records.push(example.partition_record());
            for negative in &example.negatives {
                partition_records.push(PartitionedExample::new(
                    negative.embedding.occurrence_id(),
                    example.partition,
                    negative.groups,
                ));
            }
        }
        let split_audit = audit_group_disjoint_splits(&partition_records)?;
        if split_audit.train_examples() == 0 || split_audit.validation_examples() == 0 {
            return Err(RankGenError::MissingTrainOrValidationPartition);
        }
        let fingerprint = fingerprint_dataset(
            &examples,
            embedding_dimension,
            model_fingerprint,
            tokenizer_fingerprint,
            split_audit,
        );
        Ok(Self {
            examples,
            embedding_dimension,
            model_fingerprint,
            tokenizer_fingerprint,
            split_audit,
            fingerprint,
        })
    }

    pub const fn embedding_dimension(&self) -> usize {
        self.embedding_dimension
    }

    pub const fn model_fingerprint(&self) -> BlobId {
        self.model_fingerprint
    }

    pub const fn tokenizer_fingerprint(&self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn split_audit(&self) -> SplitAudit {
        self.split_audit
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RankGenTrainingConfig {
    variant: RankGenVariant,
    seed: [u8; 32],
    epochs: u32,
    learning_rate: f32,
    temperature: f32,
    fingerprint: BlobId,
}

impl RankGenTrainingConfig {
    pub fn new(
        variant: RankGenVariant,
        seed: [u8; 32],
        epochs: u32,
        learning_rate: f32,
        temperature: f32,
    ) -> Result<Self, RankGenError> {
        if epochs == 0 || epochs > MAX_RANKGEN_EPOCHS {
            return Err(RankGenError::InvalidEpochCount(epochs));
        }
        if !learning_rate.is_finite() || !(1.0e-8..=1.0).contains(&learning_rate) {
            return Err(RankGenError::InvalidLearningRate);
        }
        if !temperature.is_finite() || !(1.0e-4..=100.0).contains(&temperature) {
            return Err(RankGenError::InvalidTemperature);
        }
        let fingerprint = fingerprint_config(variant, seed, epochs, learning_rate, temperature);
        Ok(Self {
            variant,
            seed,
            epochs,
            learning_rate,
            temperature,
            fingerprint,
        })
    }

    pub const fn variant(self) -> RankGenVariant {
        self.variant
    }

    pub const fn epochs(self) -> u32 {
        self.epochs
    }

    pub const fn learning_rate(self) -> f32 {
        self.learning_rate
    }

    pub const fn temperature(self) -> f32 {
        self.temperature
    }

    pub const fn fingerprint(self) -> BlobId {
        self.fingerprint
    }
}

/// A rank-64 dual projection head. It is scoring evidence only and carries no
/// promotion, admission, benchmark, or writer authority.
#[derive(Clone, Debug, PartialEq)]
pub struct RankGenProjectionHead {
    variant: RankGenVariant,
    embedding_dimension: usize,
    prefix_projection: Array2<f32>,
    continuation_projection: Array2<f32>,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    dataset_fingerprint: BlobId,
    training_fingerprint: BlobId,
    fingerprint: BlobId,
}

impl RankGenProjectionHead {
    pub fn train(
        dataset: &RankGenDataset,
        config: RankGenTrainingConfig,
    ) -> Result<Self, RankGenError> {
        let train_examples = dataset
            .examples
            .iter()
            .filter(|example| example.partition == DatasetPartition::Train)
            .collect::<Vec<_>>();
        if train_examples.is_empty() || train_examples.len() > MAX_RANKGEN_TRAIN_EXAMPLES {
            return Err(RankGenError::InvalidTrainingExampleCount(
                train_examples.len(),
            ));
        }
        validate_variant_windows(&dataset.examples, config.variant)?;
        let parameter_count = RANKGEN_PROJECTION_RANK
            .checked_mul(dataset.embedding_dimension)
            .and_then(|count| count.checked_mul(2))
            .ok_or(RankGenError::ParameterCountOverflow)?;
        let mut rng = ChaCha20Rng::from_seed(config.seed);
        let mut parameters = (0..parameter_count)
            .map(|_| seeded_weight(&mut rng))
            .collect::<Vec<_>>();
        let mut adam = Adam::new(parameter_count).map_err(RankGenError::optimizer)?;
        for _ in 0..config.epochs {
            let (_, gradient) = rankgen_loss_and_gradient(
                &parameters,
                dataset.embedding_dimension,
                &train_examples,
                config.temperature,
            )?;
            adam.update(&mut parameters, &gradient, config.learning_rate)
                .map_err(RankGenError::optimizer)?;
        }
        let matrix_size = RANKGEN_PROJECTION_RANK * dataset.embedding_dimension;
        let prefix_projection = Array2::from_shape_vec(
            (RANKGEN_PROJECTION_RANK, dataset.embedding_dimension),
            parameters[..matrix_size].to_vec(),
        )
        .map_err(|_| RankGenError::ParameterShape)?;
        let continuation_projection = Array2::from_shape_vec(
            (RANKGEN_PROJECTION_RANK, dataset.embedding_dimension),
            parameters[matrix_size..].to_vec(),
        )
        .map_err(|_| RankGenError::ParameterShape)?;
        Self::from_validated_parts(
            config.variant,
            prefix_projection,
            continuation_projection,
            dataset.model_fingerprint,
            dataset.tokenizer_fingerprint,
            dataset.fingerprint,
            config.fingerprint,
        )
    }

    pub(crate) fn from_validated_parts(
        variant: RankGenVariant,
        prefix_projection: Array2<f32>,
        continuation_projection: Array2<f32>,
        model_fingerprint: BlobId,
        tokenizer_fingerprint: BlobId,
        dataset_fingerprint: BlobId,
        training_fingerprint: BlobId,
    ) -> Result<Self, RankGenError> {
        let prefix_shape = prefix_projection.shape();
        let continuation_shape = continuation_projection.shape();
        if prefix_shape.len() != 2
            || prefix_shape[0] != RANKGEN_PROJECTION_RANK
            || prefix_shape != continuation_shape
            || prefix_shape[1] == 0
            || prefix_shape[1] > crate::MAX_EMBEDDING_DIMENSIONS
        {
            return Err(RankGenError::ParameterShape);
        }
        if prefix_projection.iter().any(|value| !value.is_finite())
            || continuation_projection
                .iter()
                .any(|value| !value.is_finite())
        {
            return Err(RankGenError::NonFiniteParameter);
        }
        let embedding_dimension = prefix_shape[1];
        let fingerprint = fingerprint_model(&RankGenFingerprintInput {
            variant,
            dimension: embedding_dimension,
            prefix: &prefix_projection,
            continuation: &continuation_projection,
            model: model_fingerprint,
            tokenizer: tokenizer_fingerprint,
            dataset: dataset_fingerprint,
            training: training_fingerprint,
        });
        Ok(Self {
            variant,
            embedding_dimension,
            prefix_projection,
            continuation_projection,
            model_fingerprint,
            tokenizer_fingerprint,
            dataset_fingerprint,
            training_fingerprint,
            fingerprint,
        })
    }

    pub fn score(
        &self,
        prefix: &FrozenEmbedding,
        continuation: &FrozenEmbedding,
    ) -> Result<f32, RankGenError> {
        require_role(prefix, crate::EmbeddingRole::Prefix)?;
        require_role(continuation, crate::EmbeddingRole::Continuation)?;
        self.validate_embedding(prefix)?;
        self.validate_embedding(continuation)?;
        let prefix = project(&self.prefix_projection, prefix.values());
        let continuation = project(&self.continuation_projection, continuation.values());
        let score = dot(&prefix, &continuation);
        if score.is_finite() {
            Ok(score)
        } else {
            Err(RankGenError::NonFiniteScore)
        }
    }

    fn validate_embedding(&self, embedding: &FrozenEmbedding) -> Result<(), RankGenError> {
        if embedding.dimension() != self.embedding_dimension {
            return Err(RankGenError::EmbeddingDimensionMismatch {
                expected: self.embedding_dimension,
                actual: embedding.dimension(),
            });
        }
        if embedding.model_fingerprint() != self.model_fingerprint
            || embedding.tokenizer_fingerprint() != self.tokenizer_fingerprint
        {
            return Err(RankGenError::EmbeddingSpaceMismatch);
        }
        Ok(())
    }

    pub const fn variant(&self) -> RankGenVariant {
        self.variant
    }

    pub const fn embedding_dimension(&self) -> usize {
        self.embedding_dimension
    }

    pub const fn model_fingerprint(&self) -> BlobId {
        self.model_fingerprint
    }

    pub const fn tokenizer_fingerprint(&self) -> BlobId {
        self.tokenizer_fingerprint
    }

    pub const fn dataset_fingerprint(&self) -> BlobId {
        self.dataset_fingerprint
    }

    pub const fn training_fingerprint(&self) -> BlobId {
        self.training_fingerprint
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub(crate) fn prefix_projection(&self) -> &Array2<f32> {
        &self.prefix_projection
    }

    pub(crate) fn continuation_projection(&self) -> &Array2<f32> {
        &self.continuation_projection
    }
}

fn rankgen_loss_and_gradient(
    parameters: &[f32],
    dimension: usize,
    examples: &[&RankGenExample],
    temperature: f32,
) -> Result<(f32, Vec<f32>), RankGenError> {
    let matrix_size = RANKGEN_PROJECTION_RANK
        .checked_mul(dimension)
        .ok_or(RankGenError::ParameterCountOverflow)?;
    if parameters.len() != matrix_size * 2 || examples.is_empty() {
        return Err(RankGenError::ParameterShape);
    }
    let (prefix_weights, continuation_weights) = parameters.split_at(matrix_size);
    let mut gradient = vec![0.0_f32; parameters.len()];
    let mut total_loss = 0.0_f32;
    for example in examples {
        let prefix_projection = project_flat(prefix_weights, dimension, example.prefix.values());
        let continuations = std::iter::once(&example.positive)
            .chain(example.negatives.iter().map(|negative| &negative.embedding))
            .collect::<Vec<_>>();
        let continuation_projections = continuations
            .iter()
            .map(|embedding| project_flat(continuation_weights, dimension, embedding.values()))
            .collect::<Vec<_>>();
        let logits = continuation_projections
            .iter()
            .map(|continuation| dot(&prefix_projection, continuation) / temperature)
            .collect::<Vec<_>>();
        let max_logit = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let exponentials = logits
            .iter()
            .map(|logit| (*logit - max_logit).exp())
            .collect::<Vec<_>>();
        let denominator: f32 = exponentials.iter().sum();
        if !denominator.is_finite() || denominator <= 0.0 {
            return Err(RankGenError::NonFiniteLoss);
        }
        total_loss += denominator.ln() + max_logit - logits[0];

        for (continuation_index, ((embedding, projected), exponential)) in continuations
            .iter()
            .zip(&continuation_projections)
            .zip(&exponentials)
            .enumerate()
        {
            let target = if continuation_index == 0 { 1.0 } else { 0.0 };
            let derivative = (*exponential / denominator - target) / temperature;
            for rank in 0..RANKGEN_PROJECTION_RANK {
                let prefix_factor = derivative * projected[rank];
                let continuation_factor = derivative * prefix_projection[rank];
                let row = rank * dimension;
                for column in 0..dimension {
                    gradient[row + column] += prefix_factor * example.prefix.values()[column];
                    gradient[matrix_size + row + column] +=
                        continuation_factor * embedding.values()[column];
                }
            }
        }
    }
    let divisor = f32::from(
        u16::try_from(examples.len())
            .map_err(|_| RankGenError::InvalidTrainingExampleCount(examples.len()))?,
    );
    total_loss /= divisor;
    for value in &mut gradient {
        *value /= divisor;
    }
    if !total_loss.is_finite() || gradient.iter().any(|value| !value.is_finite()) {
        return Err(RankGenError::NonFiniteLoss);
    }
    Ok((total_loss, gradient))
}

fn project(matrix: &Array2<f32>, embedding: &[f32]) -> Vec<f32> {
    let dimension = embedding.len();
    let weights = matrix
        .as_slice()
        .expect("owned standard-layout projection matrix");
    project_flat(weights, dimension, embedding)
}

fn project_flat(weights: &[f32], dimension: usize, embedding: &[f32]) -> Vec<f32> {
    let mut output = vec![0.0_f32; RANKGEN_PROJECTION_RANK];
    for (rank, output_value) in output.iter_mut().enumerate() {
        let row = rank * dimension;
        for column in 0..dimension {
            *output_value = weights[row + column].mul_add(embedding[column], *output_value);
        }
    }
    output
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .fold(0.0_f32, |sum, (left, right)| left.mul_add(*right, sum))
}

fn seeded_weight(rng: &mut ChaCha20Rng) -> f32 {
    let sample = u16::try_from(rng.next_u32() >> 16).expect("shifted u32 always fits u16");
    let unit = f32::from(sample) / f32::from(u16::MAX);
    (unit * 2.0 - 1.0) * INITIAL_WEIGHT_SCALE
}

fn require_role(
    embedding: &FrozenEmbedding,
    expected: crate::EmbeddingRole,
) -> Result<(), RankGenError> {
    if embedding.role() == expected {
        Ok(())
    } else {
        Err(RankGenError::EmbeddingRoleMismatch {
            expected,
            actual: embedding.role(),
        })
    }
}

fn validate_variant_windows(
    examples: &[RankGenExample],
    variant: RankGenVariant,
) -> Result<(), RankGenError> {
    let max_prefix = u32::from(variant.prefix_tokens());
    let max_continuation = u32::from(variant.continuation_tokens());
    for example in examples {
        if example.prefix.input_token_count() > max_prefix {
            return Err(RankGenError::EmbeddingWindowTooLarge {
                role: crate::EmbeddingRole::Prefix,
                maximum: max_prefix,
                actual: example.prefix.input_token_count(),
            });
        }
        for continuation in std::iter::once(&example.positive)
            .chain(example.negatives.iter().map(|negative| &negative.embedding))
        {
            if continuation.input_token_count() > max_continuation {
                return Err(RankGenError::EmbeddingWindowTooLarge {
                    role: crate::EmbeddingRole::Continuation,
                    maximum: max_continuation,
                    actual: continuation.input_token_count(),
                });
            }
        }
    }
    Ok(())
}

fn fingerprint_dataset(
    examples: &[RankGenExample],
    dimension: usize,
    model: BlobId,
    tokenizer: BlobId,
    split_audit: SplitAudit,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(DATASET_DOMAIN);
    digest.update((dimension as u64).to_be_bytes());
    digest.update(model.as_bytes());
    digest.update(tokenizer.as_bytes());
    digest.update(split_audit.fingerprint().as_bytes());
    digest.update((examples.len() as u64).to_be_bytes());
    for example in examples {
        digest.update(example.example_id.as_ulid().to_bytes());
        digest.update([example.partition.tag()]);
        digest.update(example.groups.fingerprint().as_bytes());
        digest.update(example.prefix.fingerprint().as_bytes());
        digest.update(example.positive.fingerprint().as_bytes());
        digest.update((example.negatives.len() as u64).to_be_bytes());
        for negative in &example.negatives {
            digest.update([negative.evidence.class().tag()]);
            digest.update(negative.evidence.fingerprint().as_bytes());
            digest.update(negative.embedding.fingerprint().as_bytes());
            digest.update(negative.groups.fingerprint().as_bytes());
        }
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_config(
    variant: RankGenVariant,
    seed: [u8; 32],
    epochs: u32,
    learning_rate: f32,
    temperature: f32,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(CONFIG_DOMAIN);
    digest.update([variant.tag()]);
    digest.update(seed);
    digest.update(epochs.to_be_bytes());
    digest.update(learning_rate.to_bits().to_be_bytes());
    digest.update(temperature.to_bits().to_be_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

struct RankGenFingerprintInput<'a> {
    variant: RankGenVariant,
    dimension: usize,
    prefix: &'a Array2<f32>,
    continuation: &'a Array2<f32>,
    model: BlobId,
    tokenizer: BlobId,
    dataset: BlobId,
    training: BlobId,
}

fn fingerprint_model(input: &RankGenFingerprintInput<'_>) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(MODEL_DOMAIN);
    digest.update([input.variant.tag()]);
    digest.update((input.dimension as u64).to_be_bytes());
    digest.update(input.model.as_bytes());
    digest.update(input.tokenizer.as_bytes());
    digest.update(input.dataset.as_bytes());
    digest.update(input.training.as_bytes());
    for parameter in input.prefix.iter().chain(input.continuation.iter()) {
        digest.update(parameter.to_bits().to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn require_negative_group_relation(
    class: RankGenNegativeClass,
    anchor: BlobId,
    candidate: BlobId,
) -> Result<BlobId, RankGenError> {
    if anchor == candidate {
        Ok(anchor)
    } else {
        Err(RankGenError::NegativeRelationshipMismatch(class))
    }
}

struct NegativeEvidenceFingerprintInput {
    derivation: RankGenNegativeDerivation,
    anchor_occurrence_id: ArtifactId,
    anchor_source_blob_id: BlobId,
    anchor_embedding_fingerprint: BlobId,
    candidate_occurrence_id: ArtifactId,
    candidate_source_blob_id: BlobId,
    candidate_embedding_fingerprint: BlobId,
    relation_group_fingerprint: BlobId,
}

fn fingerprint_negative_evidence(input: &NegativeEvidenceFingerprintInput) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(NEGATIVE_EVIDENCE_DOMAIN);
    digest.update([input.derivation.class().tag()]);
    digest.update(input.anchor_occurrence_id.as_ulid().to_bytes());
    digest.update(input.anchor_source_blob_id.as_bytes());
    digest.update(input.anchor_embedding_fingerprint.as_bytes());
    digest.update(input.candidate_occurrence_id.as_ulid().to_bytes());
    digest.update(input.candidate_source_blob_id.as_bytes());
    digest.update(input.candidate_embedding_fingerprint.as_bytes());
    digest.update(input.relation_group_fingerprint.as_bytes());
    digest.update(input.derivation.receipt_fingerprint().as_bytes());
    if let RankGenNegativeDerivation::Temporal {
        anchor_position_fingerprint,
        candidate_position_fingerprint,
        ..
    } = input.derivation
    {
        digest.update(anchor_position_fingerprint.as_bytes());
        digest.update(candidate_position_fingerprint.as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum RankGenError {
    #[error(transparent)]
    Dataset(#[from] DatasetError),
    #[error("RankGen requires 5..={MAX_RANKGEN_NEGATIVES} negatives, got {0}")]
    InvalidNegativeCount(usize),
    #[error("RankGen example is missing {0:?} negative evidence")]
    MissingNegativeClass(RankGenNegativeClass),
    #[error("{0:?} negative does not satisfy its exact source-group relationship")]
    NegativeRelationshipMismatch(RankGenNegativeClass),
    #[error("RankGen negative evidence does not bind the supplied source occurrence")]
    NegativeEvidenceBindingMismatch,
    #[error("temporal negative anchor and candidate positions must differ")]
    TemporalPositionsNotDistinct,
    #[error("entity-corruption negative must have different source bytes from its anchor")]
    EntityCorruptionSourceUnchanged,
    #[error("an embedding occurrence is reused within a RankGen example")]
    RepeatedEmbeddingOccurrence,
    #[error("continuation source {0} is reused as more than one InfoNCE target")]
    RepeatedContinuationSource(BlobId),
    #[error("embedding role mismatch: expected {expected:?}, got {actual:?}")]
    EmbeddingRoleMismatch {
        expected: crate::EmbeddingRole,
        actual: crate::EmbeddingRole,
    },
    #[error("embedding dimensions differ: expected {expected}, got {actual}")]
    EmbeddingDimensionMismatch { expected: usize, actual: usize },
    #[error("embeddings do not share exact model and tokenizer fingerprints")]
    EmbeddingSpaceMismatch,
    #[error("{role:?} embedding has {actual} input tokens; treatment maximum is {maximum}")]
    EmbeddingWindowTooLarge {
        role: crate::EmbeddingRole,
        maximum: u32,
        actual: u32,
    },
    #[error("RankGen dataset requires non-empty train and validation partitions")]
    MissingTrainOrValidationPartition,
    #[error("RankGen training example count {0} is outside 1..={MAX_RANKGEN_TRAIN_EXAMPLES}")]
    InvalidTrainingExampleCount(usize),
    #[error("RankGen epoch count {0} is outside 1..={MAX_RANKGEN_EPOCHS}")]
    InvalidEpochCount(u32),
    #[error("RankGen learning rate is outside the finite range 1e-8..=1")]
    InvalidLearningRate,
    #[error("RankGen temperature is outside the finite range 1e-4..=100")]
    InvalidTemperature,
    #[error("RankGen parameter count overflowed")]
    ParameterCountOverflow,
    #[error("RankGen parameter shape is invalid")]
    ParameterShape,
    #[error("RankGen parameter is non-finite")]
    NonFiniteParameter,
    #[error("RankGen loss or gradient is non-finite")]
    NonFiniteLoss,
    #[error("RankGen score is non-finite")]
    NonFiniteScore,
    #[error("deterministic Adam rejected the RankGen update")]
    Optimizer,
}

impl RankGenError {
    const fn optimizer(_: OptimizerError) -> Self {
        Self::Optimizer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EmbeddingRole, FrozenEmbeddingInput};

    fn groups(label: &[u8]) -> LeakageGroups {
        LeakageGroups::new(
            BlobId::digest(&[label, b"project"].concat()),
            BlobId::digest(&[label, b"sibling"].concat()),
            BlobId::digest(&[label, b"work"].concat()),
            BlobId::digest(&[label, b"prompt"].concat()),
            BlobId::digest(&[label, b"duplicate"].concat()),
        )
    }

    fn embedding(role: EmbeddingRole, values: &[f32], label: &[u8]) -> FrozenEmbedding {
        embedding_with_tokens(role, values, label, 2)
    }

    fn embedding_with_tokens(
        role: EmbeddingRole,
        values: &[f32],
        label: &[u8],
        input_token_count: u32,
    ) -> FrozenEmbedding {
        embedding_with_source_and_tokens(
            role,
            values,
            label,
            BlobId::digest(label),
            input_token_count,
        )
    }

    fn embedding_with_source_and_tokens(
        role: EmbeddingRole,
        values: &[f32],
        label: &[u8],
        source_blob_id: BlobId,
        input_token_count: u32,
    ) -> FrozenEmbedding {
        FrozenEmbedding::new(FrozenEmbeddingInput {
            occurrence_id: ArtifactId::new(),
            source_blob_id,
            model_fingerprint: BlobId::digest(b"embedding-model"),
            tokenizer_fingerprint: BlobId::digest(b"tokenizer"),
            extraction_fingerprint: BlobId::digest(&[label, b"extraction"].concat()),
            role,
            input_token_count,
            values: values.to_vec(),
        })
        .expect("embedding")
    }

    fn example(partition: DatasetPartition, label: &[u8]) -> RankGenExampleInput {
        example_with_windows(partition, label, 2, 2)
    }

    fn example_with_windows(
        partition: DatasetPartition,
        label: &[u8],
        prefix_tokens: u32,
        continuation_tokens: u32,
    ) -> RankGenExampleInput {
        let example_groups = groups(&[label, b"main"].concat());
        let prefix = embedding_with_tokens(
            EmbeddingRole::Prefix,
            &[0.4, -0.2],
            &[label, b"prefix"].concat(),
            prefix_tokens,
        );
        let positive = embedding_with_tokens(
            EmbeddingRole::Continuation,
            &[0.3, -0.1],
            &[label, b"positive"].concat(),
            continuation_tokens,
        );
        let negatives = RankGenNegativeClass::ALL
            .iter()
            .enumerate()
            .map(|(index, class)| {
                let small_index = u8::try_from(index).expect("five negative classes");
                let value = f32::from(small_index);
                let negative_label = [label, &[small_index]].concat();
                let embedding = embedding_with_tokens(
                    EmbeddingRole::Continuation,
                    &[value * 0.07 - 0.2, value * -0.03 + 0.1],
                    &negative_label,
                    continuation_tokens,
                );
                let negative_groups = negative_groups(*class, example_groups, &negative_label);
                let evidence = RankGenNegativeEvidence::derive(
                    derivation(*class, &negative_label),
                    &prefix,
                    &positive,
                    example_groups,
                    &embedding,
                    negative_groups,
                )
                .expect("valid negative relationship");
                RankGenNegativeInput {
                    evidence,
                    embedding,
                    groups: negative_groups,
                }
            })
            .collect();
        RankGenExampleInput {
            example_id: ArtifactId::new(),
            partition,
            groups: example_groups,
            prefix,
            positive,
            negatives,
        }
    }

    fn negative_groups(
        class: RankGenNegativeClass,
        anchor: LeakageGroups,
        label: &[u8],
    ) -> LeakageGroups {
        let fresh = groups(label);
        match class {
            RankGenNegativeClass::SameWork | RankGenNegativeClass::Temporal => LeakageGroups::new(
                fresh.project_ancestry(),
                fresh.sibling_pool(),
                anchor.author_work(),
                fresh.prompt_family(),
                fresh.near_duplicate_cluster(),
            ),
            RankGenNegativeClass::Sibling => LeakageGroups::new(
                fresh.project_ancestry(),
                anchor.sibling_pool(),
                fresh.author_work(),
                fresh.prompt_family(),
                fresh.near_duplicate_cluster(),
            ),
            RankGenNegativeClass::Generated => LeakageGroups::new(
                fresh.project_ancestry(),
                fresh.sibling_pool(),
                fresh.author_work(),
                anchor.prompt_family(),
                fresh.near_duplicate_cluster(),
            ),
            RankGenNegativeClass::EntityCorruption => LeakageGroups::new(
                fresh.project_ancestry(),
                fresh.sibling_pool(),
                fresh.author_work(),
                fresh.prompt_family(),
                anchor.near_duplicate_cluster(),
            ),
        }
    }

    fn derivation(class: RankGenNegativeClass, label: &[u8]) -> RankGenNegativeDerivation {
        let receipt = BlobId::digest(&[label, b"negative-receipt"].concat());
        match class {
            RankGenNegativeClass::SameWork => RankGenNegativeDerivation::SameWork {
                receipt_fingerprint: receipt,
            },
            RankGenNegativeClass::Sibling => RankGenNegativeDerivation::Sibling {
                receipt_fingerprint: receipt,
            },
            RankGenNegativeClass::Generated => RankGenNegativeDerivation::Generated {
                generation_receipt_fingerprint: receipt,
            },
            RankGenNegativeClass::Temporal => RankGenNegativeDerivation::Temporal {
                receipt_fingerprint: receipt,
                anchor_position_fingerprint: BlobId::digest(&[label, b"anchor-position"].concat()),
                candidate_position_fingerprint: BlobId::digest(
                    &[label, b"candidate-position"].concat(),
                ),
            },
            RankGenNegativeClass::EntityCorruption => RankGenNegativeDerivation::EntityCorruption {
                transformation_receipt_fingerprint: receipt,
            },
        }
    }

    fn dataset() -> RankGenDataset {
        RankGenDataset::compile(vec![
            example(DatasetPartition::Train, b"train"),
            example(DatasetPartition::Validation, b"validation"),
        ])
        .expect("dataset")
    }

    #[test]
    fn training_is_bit_reproducible_and_uses_fixed_rank() {
        let dataset = dataset();
        let config =
            RankGenTrainingConfig::new(RankGenVariant::Local256x128, [7; 32], 4, 0.01, 0.5)
                .expect("config");
        let first = RankGenProjectionHead::train(&dataset, config).expect("train");
        let second = RankGenProjectionHead::train(&dataset, config).expect("train");
        assert_eq!(first, second);
        assert_eq!(first.prefix_projection().shape(), &[64, 2]);
        assert_eq!(first.variant().prefix_tokens(), 256);
        assert_eq!(first.variant().continuation_tokens(), 128);
    }

    #[test]
    fn analytic_infonce_gradient_matches_finite_differences() {
        let compiled = RankGenExample::compile(example(DatasetPartition::Train, b"gradient"))
            .expect("example");
        let examples = vec![&compiled];
        let dimension = 2;
        let parameter_count = RANKGEN_PROJECTION_RANK * dimension * 2;
        let parameters = (0..parameter_count)
            .map(|index| {
                f32::from(u16::try_from(index).expect("small parameter vector")) * 0.000_1 - 0.01
            })
            .collect::<Vec<_>>();
        let (_, analytic) =
            rankgen_loss_and_gradient(&parameters, dimension, &examples, 0.7).expect("gradient");
        let epsilon = 1.0e-3_f32;
        for index in [0, 37, RANKGEN_PROJECTION_RANK * dimension + 3] {
            let mut plus = parameters.clone();
            plus[index] += epsilon;
            let plus_loss = rankgen_loss_and_gradient(&plus, dimension, &examples, 0.7)
                .expect("plus")
                .0;
            let mut minus = parameters.clone();
            minus[index] -= epsilon;
            let minus_loss = rankgen_loss_and_gradient(&minus, dimension, &examples, 0.7)
                .expect("minus")
                .0;
            let numeric = (plus_loss - minus_loss) / (2.0 * epsilon);
            assert!(
                (numeric - analytic[index]).abs() < 2.0e-4,
                "index={index}, numeric={numeric}, analytic={}",
                analytic[index]
            );
        }
    }

    #[test]
    fn every_required_negative_class_is_enforced() {
        let mut incomplete = example(DatasetPartition::Train, b"incomplete");
        incomplete.negatives.pop();
        assert_eq!(
            RankGenExample::compile(incomplete).expect_err("missing class"),
            RankGenError::InvalidNegativeCount(4)
        );
    }

    #[test]
    fn all_five_negative_classes_share_one_infonce_denominator() {
        let compiled =
            RankGenExample::compile(example(DatasetPartition::Train, b"infonce-denominator"))
                .expect("example");
        let dimension = compiled.prefix.dimension();
        let parameters = vec![0.0_f32; RANKGEN_PROJECTION_RANK * dimension * 2];
        let (full_loss, _) = rankgen_loss_and_gradient(&parameters, dimension, &[&compiled], 1.0)
            .expect("full InfoNCE");
        assert!((full_loss - 6.0_f32.ln()).abs() < 1.0e-6);

        for class in RankGenNegativeClass::ALL {
            let mut without_class = compiled.clone();
            without_class
                .negatives
                .retain(|negative| negative.evidence.class() != class);
            let (reduced_loss, _) =
                rankgen_loss_and_gradient(&parameters, dimension, &[&without_class], 1.0)
                    .expect("reduced InfoNCE");
            assert!(
                (reduced_loss - 5.0_f32.ln()).abs() < 1.0e-6,
                "class {class:?} was not represented exactly once"
            );
        }
    }

    #[test]
    fn negative_evidence_cannot_be_transplanted_to_another_occurrence() {
        let mut input = example(DatasetPartition::Train, b"evidence-transplant");
        input.negatives[0].embedding = embedding(
            EmbeddingRole::Continuation,
            &[-0.25, 0.15],
            b"evidence-transplant-replacement",
        );
        assert_eq!(
            RankGenExample::compile(input).expect_err("transplanted evidence"),
            RankGenError::NegativeEvidenceBindingMismatch
        );
    }

    #[test]
    fn one_arbitrary_source_cannot_be_relabelled_as_five_negatives() {
        let example_groups = groups(b"relabel-main");
        let prefix = embedding(EmbeddingRole::Prefix, &[0.4, -0.2], b"relabel-prefix");
        let positive = embedding(
            EmbeddingRole::Continuation,
            &[0.3, -0.1],
            b"relabel-positive",
        );
        let shared_source = BlobId::digest(b"one arbitrary negative source");
        let negatives = RankGenNegativeClass::ALL
            .iter()
            .enumerate()
            .map(|(index, class)| {
                let index = u8::try_from(index).expect("five classes");
                let label = [b"relabel-negative".as_slice(), &[index]].concat();
                let candidate = embedding_with_source_and_tokens(
                    EmbeddingRole::Continuation,
                    &[-0.2, 0.1],
                    &label,
                    shared_source,
                    2,
                );
                let evidence = RankGenNegativeEvidence::derive(
                    derivation(*class, &label),
                    &prefix,
                    &positive,
                    example_groups,
                    &candidate,
                    example_groups,
                )
                .expect("surface relationships can all be declared");
                RankGenNegativeInput {
                    evidence,
                    embedding: candidate,
                    groups: example_groups,
                }
            })
            .collect();
        let input = RankGenExampleInput {
            example_id: ArtifactId::new(),
            partition: DatasetPartition::Train,
            groups: example_groups,
            prefix,
            positive,
            negatives,
        };
        assert_eq!(
            RankGenExample::compile(input).expect_err("duplicate source relabeling"),
            RankGenError::RepeatedContinuationSource(shared_source)
        );
    }

    #[test]
    fn dataset_fingerprint_is_canonical_across_input_and_negative_order() {
        let train = example(DatasetPartition::Train, b"canonical-train");
        let validation = example(DatasetPartition::Validation, b"canonical-validation");
        let forward = RankGenDataset::compile(vec![train.clone(), validation.clone()])
            .expect("forward dataset");
        let mut reversed_train = train;
        reversed_train.negatives.reverse();
        let mut reversed_validation = validation;
        reversed_validation.negatives.reverse();
        let reverse = RankGenDataset::compile(vec![reversed_validation, reversed_train])
            .expect("reverse dataset");
        assert_eq!(forward.fingerprint(), reverse.fingerprint());
    }

    #[test]
    fn local_and_chapter_variants_enforce_their_exact_window_maxima() {
        let local = RankGenTrainingConfig::new(RankGenVariant::Local256x128, [1; 32], 1, 0.01, 0.5)
            .expect("local config");
        let local_boundary = RankGenDataset::compile(vec![
            example_with_windows(DatasetPartition::Train, b"local-train", 256, 128),
            example_with_windows(DatasetPartition::Validation, b"local-validation", 256, 128),
        ])
        .expect("local boundary dataset");
        RankGenProjectionHead::train(&local_boundary, local).expect("exact local maxima");

        let local_continuation_overflow = RankGenDataset::compile(vec![
            example_with_windows(DatasetPartition::Train, b"local-overflow", 256, 129),
            example_with_windows(
                DatasetPartition::Validation,
                b"local-overflow-validation",
                256,
                128,
            ),
        ])
        .expect("local overflow dataset");
        assert!(matches!(
            RankGenProjectionHead::train(&local_continuation_overflow, local),
            Err(RankGenError::EmbeddingWindowTooLarge {
                role: EmbeddingRole::Continuation,
                maximum: 128,
                actual: 129,
            })
        ));

        let chapter = RankGenTrainingConfig::new(
            RankGenVariant::ChapterTransition1024x256,
            [1; 32],
            1,
            0.01,
            0.5,
        )
        .expect("chapter config");
        let chapter_boundary = RankGenDataset::compile(vec![
            example_with_windows(DatasetPartition::Train, b"chapter-train", 1_024, 256),
            example_with_windows(
                DatasetPartition::Validation,
                b"chapter-validation",
                1_024,
                256,
            ),
        ])
        .expect("chapter boundary dataset");
        RankGenProjectionHead::train(&chapter_boundary, chapter).expect("exact chapter maxima");

        let chapter_prefix_overflow = RankGenDataset::compile(vec![
            example_with_windows(
                DatasetPartition::Train,
                b"chapter-prefix-overflow",
                1_025,
                256,
            ),
            example_with_windows(
                DatasetPartition::Validation,
                b"chapter-prefix-overflow-validation",
                1_024,
                256,
            ),
        ])
        .expect("chapter prefix overflow dataset");
        assert!(matches!(
            RankGenProjectionHead::train(&chapter_prefix_overflow, chapter),
            Err(RankGenError::EmbeddingWindowTooLarge {
                role: EmbeddingRole::Prefix,
                maximum: 1_024,
                actual: 1_025,
            })
        ));

        let chapter_continuation_overflow = RankGenDataset::compile(vec![
            example_with_windows(
                DatasetPartition::Train,
                b"chapter-continuation-overflow",
                1_024,
                257,
            ),
            example_with_windows(
                DatasetPartition::Validation,
                b"chapter-continuation-overflow-validation",
                1_024,
                256,
            ),
        ])
        .expect("chapter continuation overflow dataset");
        assert!(matches!(
            RankGenProjectionHead::train(&chapter_continuation_overflow, chapter),
            Err(RankGenError::EmbeddingWindowTooLarge {
                role: EmbeddingRole::Continuation,
                maximum: 256,
                actual: 257,
            })
        ));
    }
}
