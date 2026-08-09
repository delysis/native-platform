use std::{collections::BTreeSet, marker::PhantomData};

use loom_types::{ArtifactId, BlobId};
use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore, SeedableRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    Adam, DatasetError, DatasetPartition, EmbeddingRole, FrozenEmbedding, LeakageGroups,
    MAX_LEARNING_EXAMPLES, OptimizerError, PartitionedExample, SplitAudit,
    audit_group_disjoint_splits,
};

pub const REWARD_ENSEMBLE_HEADS: usize = 5;
pub const MAX_REWARD_EPOCHS: u32 = 512;
pub const MAX_CALIBRATION_EPOCHS: u32 = 2_048;
pub const MAX_REWARD_PARTITION_EXAMPLES: usize = 65_535;
pub const MIN_EVALUATOR_HUMAN_PAIRS: usize = 300;
pub const MIN_EVALUATOR_HUMAN_GROUPS: usize = 75;
pub const MIN_ACTIVE_SHELF_HUMAN_PAIRS: usize = 1_000;
pub const MIN_ACTIVE_SHELF_HUMAN_GROUPS: usize = 200;

const DATASET_DOMAIN: &[u8] = b"loom/reward-pair-dataset/v1\0";
const CONFIG_DOMAIN: &[u8] = b"loom/reward-training-config/v1\0";
const MODEL_DOMAIN: &[u8] = b"loom/final-embedding-reward-ensemble/v1\0";
const PARAMETER_DOMAIN: &[u8] = b"loom/reward-trained-parameters/v1\0";
const OOD_DOMAIN: &[u8] = b"loom/reward-ood-distribution/v1\0";
const HEAD_SEED_DOMAIN: &[u8] = b"loom/reward-head-seed/v1\0";
const INITIAL_WEIGHT_SCALE: f32 = 0.01;
const MIN_OOD_SCALE: f32 = 1.0e-6;
const REWARD_HEAD_DIVISOR: f32 = 5.0;

mod sealed {
    pub trait Sealed {}
}

/// Type-level role for a final-embedding reward ensemble.
pub trait RewardRole: sealed::Sealed + Clone + Copy + std::fmt::Debug + Eq + PartialEq {
    const ROLE: RewardHeadRole;
    const EMBEDDING_ROLE: EmbeddingRole;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Causal;

impl sealed::Sealed for Causal {}
impl RewardRole for Causal {
    const ROLE: RewardHeadRole = RewardHeadRole::Causal;
    const EMBEDDING_ROLE: EmbeddingRole = EmbeddingRole::FinalSegment;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Literary;

impl sealed::Sealed for Literary {}
impl RewardRole for Literary {
    const ROLE: RewardHeadRole = RewardHeadRole::Literary;
    const EMBEDDING_ROLE: EmbeddingRole = EmbeddingRole::FinalSegment;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanValue;

impl sealed::Sealed for PlanValue {}
impl RewardRole for PlanValue {
    const ROLE: RewardHeadRole = RewardHeadRole::PlanValue;
    const EMBEDDING_ROLE: EmbeddingRole = EmbeddingRole::Plan;
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardHeadRole {
    Causal,
    Literary,
    PlanValue,
}

impl RewardHeadRole {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Causal => 0,
            Self::Literary => 1,
            Self::PlanValue => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceLabel {
    Left,
    Right,
    Tie,
}

impl PreferenceLabel {
    const fn tag(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Tie => 2,
        }
    }

    const fn target(self) -> [f32; 3] {
        match self {
            Self::Left => [1.0, 0.0, 0.0],
            Self::Right => [0.0, 1.0, 0.0],
            Self::Tie => [0.0, 0.0, 1.0],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceSource {
    /// The caller declares that an external receipt records a human label.
    /// This crate does not verify that declaration.
    ClaimedHuman,
    FrontierCritic {
        critic_fingerprint: BlobId,
    },
}

impl PreferenceSource {
    const fn tag(self) -> u8 {
        match self {
            Self::ClaimedHuman => 0,
            Self::FrontierCritic { .. } => 1,
        }
    }
}

/// Exact label-provenance declaration.
///
/// Frontier labels remain distinguishable forever and never contribute to
/// human activation thresholds. These constructors bind caller-supplied
/// receipt fingerprints; this crate does not verify human presence. A trusted
/// store must verify that evidence before using the derived threshold status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PreferenceEvidence {
    source: PreferenceSource,
    receipt_fingerprint: BlobId,
}

impl PreferenceEvidence {
    /// Records a caller-declared human-label receipt fingerprint.
    ///
    /// The name is intentionally explicit: constructing this value does not
    /// verify user presence, label contents, or receipt storage. A trusted
    /// store must replay those facts before activating a learned evaluator.
    pub const fn claimed_human(receipt_fingerprint: BlobId) -> Self {
        Self {
            source: PreferenceSource::ClaimedHuman,
            receipt_fingerprint,
        }
    }

    pub const fn frontier(critic_fingerprint: BlobId, receipt_fingerprint: BlobId) -> Self {
        Self {
            source: PreferenceSource::FrontierCritic { critic_fingerprint },
            receipt_fingerprint,
        }
    }

    pub const fn source(self) -> PreferenceSource {
        self.source
    }

    pub const fn receipt_fingerprint(self) -> BlobId {
        self.receipt_fingerprint
    }
}

#[derive(Clone, Debug)]
pub struct RewardPairExampleInput {
    pub example_id: ArtifactId,
    pub partition: DatasetPartition,
    pub groups: LeakageGroups,
    pub left: FrozenEmbedding,
    pub right: FrozenEmbedding,
    pub preference: PreferenceLabel,
    pub evidence: PreferenceEvidence,
}

#[derive(Clone, Debug)]
struct RewardPairExample {
    example_id: ArtifactId,
    partition: DatasetPartition,
    groups: LeakageGroups,
    left: FrozenEmbedding,
    right: FrozenEmbedding,
    preference: PreferenceLabel,
    evidence: PreferenceEvidence,
}

impl RewardPairExample {
    fn compile<R: RewardRole>(input: RewardPairExampleInput) -> Result<Self, RewardError> {
        require_role::<R>(&input.left)?;
        require_role::<R>(&input.right)?;
        if input.left.occurrence_id() == input.right.occurrence_id() {
            return Err(RewardError::RepeatedEmbeddingOccurrence);
        }
        if input.left.dimension() != input.right.dimension() {
            return Err(RewardError::EmbeddingDimensionMismatch {
                expected: input.left.dimension(),
                actual: input.right.dimension(),
            });
        }
        if input.left.model_fingerprint() != input.right.model_fingerprint()
            || input.left.tokenizer_fingerprint() != input.right.tokenizer_fingerprint()
        {
            return Err(RewardError::EmbeddingSpaceMismatch);
        }
        Ok(Self {
            example_id: input.example_id,
            partition: input.partition,
            groups: input.groups,
            left: input.left,
            right: input.right,
            preference: input.preference,
            evidence: input.evidence,
        })
    }

    fn partition_record(&self) -> PartitionedExample {
        PartitionedExample::new(self.example_id, self.partition, self.groups)
    }
}

/// Data-volume disposition only. It never grants benchmark, promotion, or
/// manuscript authority, and is trustworthy only when the input label receipts
/// were independently verified.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LabelComposition {
    pub(crate) claimed_human_training_pairs: usize,
    pub(crate) claimed_human_training_groups: usize,
    pub(crate) claimed_human_validation_pairs: usize,
    pub(crate) claimed_human_calibration_pairs: usize,
    pub(crate) frontier_pairs: usize,
}

impl LabelComposition {
    pub const fn claimed_human_training_pairs(self) -> usize {
        self.claimed_human_training_pairs
    }

    pub const fn claimed_human_training_groups(self) -> usize {
        self.claimed_human_training_groups
    }

    pub const fn claimed_human_validation_pairs(self) -> usize {
        self.claimed_human_validation_pairs
    }

    pub const fn claimed_human_calibration_pairs(self) -> usize {
        self.claimed_human_calibration_pairs
    }

    pub const fn frontier_pairs(self) -> usize {
        self.frontier_pairs
    }
}

/// Canonical, role-typed reward-pair evidence.
#[derive(Clone, Debug)]
pub struct RewardDataset<R: RewardRole> {
    examples: Vec<RewardPairExample>,
    embedding_dimension: usize,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    split_audit: SplitAudit,
    label_composition: LabelComposition,
    fingerprint: BlobId,
    role: PhantomData<R>,
}

impl<R: RewardRole> RewardDataset<R> {
    pub fn compile(inputs: Vec<RewardPairExampleInput>) -> Result<Self, RewardError> {
        if inputs.is_empty() || inputs.len() > MAX_LEARNING_EXAMPLES {
            return Err(RewardError::Dataset(DatasetError::InvalidExampleCount(
                inputs.len(),
            )));
        }
        let mut examples = inputs
            .into_iter()
            .map(RewardPairExample::compile::<R>)
            .collect::<Result<Vec<_>, _>>()?;
        examples.sort_unstable_by_key(|example| example.example_id);
        let bindings = validate_reward_bindings(&examples)?;
        let split_audit = validate_reward_split(&examples)?;
        let label_composition = label_composition(&examples);
        let fingerprint = fingerprint_dataset::<R>(
            &examples,
            bindings.dimension,
            bindings.model_fingerprint,
            bindings.tokenizer_fingerprint,
            split_audit,
            label_composition,
        );
        Ok(Self {
            examples,
            embedding_dimension: bindings.dimension,
            model_fingerprint: bindings.model_fingerprint,
            tokenizer_fingerprint: bindings.tokenizer_fingerprint,
            split_audit,
            label_composition,
            fingerprint,
            role: PhantomData,
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

    pub const fn label_composition(&self) -> LabelComposition {
        self.label_composition
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Copy)]
struct RewardDatasetBindings {
    dimension: usize,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
}

fn validate_reward_bindings(
    examples: &[RewardPairExample],
) -> Result<RewardDatasetBindings, RewardError> {
    let first = &examples[0];
    let bindings = RewardDatasetBindings {
        dimension: first.left.dimension(),
        model_fingerprint: first.left.model_fingerprint(),
        tokenizer_fingerprint: first.left.tokenizer_fingerprint(),
    };
    let mut occurrences = BTreeSet::new();
    for example in examples {
        if example.left.dimension() != bindings.dimension {
            return Err(RewardError::EmbeddingDimensionMismatch {
                expected: bindings.dimension,
                actual: example.left.dimension(),
            });
        }
        if example.left.model_fingerprint() != bindings.model_fingerprint
            || example.left.tokenizer_fingerprint() != bindings.tokenizer_fingerprint
        {
            return Err(RewardError::EmbeddingSpaceMismatch);
        }
        if !occurrences.insert(example.left.occurrence_id())
            || !occurrences.insert(example.right.occurrence_id())
        {
            return Err(RewardError::RepeatedEmbeddingOccurrence);
        }
    }
    Ok(bindings)
}

fn validate_reward_split(examples: &[RewardPairExample]) -> Result<SplitAudit, RewardError> {
    let partition_records = examples
        .iter()
        .map(RewardPairExample::partition_record)
        .collect::<Vec<_>>();
    let split_audit = audit_group_disjoint_splits(&partition_records)?;
    let counts = [
        split_audit.train_examples(),
        split_audit.validation_examples(),
        split_audit.calibration_examples(),
    ];
    if counts.contains(&0) {
        return Err(RewardError::MissingRequiredPartition);
    }
    if let Some(count) = counts
        .into_iter()
        .find(|count| *count > MAX_REWARD_PARTITION_EXAMPLES)
    {
        return Err(RewardError::PartitionTooLarge(count));
    }
    Ok(split_audit)
}

fn label_composition(examples: &[RewardPairExample]) -> LabelComposition {
    let is_human =
        |example: &&RewardPairExample| example.evidence.source == PreferenceSource::ClaimedHuman;
    let human_training = examples
        .iter()
        .filter(is_human)
        .filter(|example| example.partition == DatasetPartition::Train)
        .collect::<Vec<_>>();
    let mut group_axes: [BTreeSet<BlobId>; 5] = std::array::from_fn(|_| BTreeSet::new());
    for example in &human_training {
        for (index, (_, group)) in example.groups.axes().into_iter().enumerate() {
            group_axes[index].insert(group);
        }
    }
    let human_training_groups = group_axes.iter().map(BTreeSet::len).min().unwrap_or(0);
    let human_validation_pairs = examples
        .iter()
        .filter(is_human)
        .filter(|example| example.partition == DatasetPartition::Validation)
        .count();
    let human_calibration_pairs = examples
        .iter()
        .filter(is_human)
        .filter(|example| example.partition == DatasetPartition::Calibration)
        .count();
    LabelComposition {
        claimed_human_training_pairs: human_training.len(),
        claimed_human_training_groups: human_training_groups,
        claimed_human_validation_pairs: human_validation_pairs,
        claimed_human_calibration_pairs: human_calibration_pairs,
        frontier_pairs: examples.len()
            - human_training.len()
            - human_validation_pairs
            - human_calibration_pairs,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RewardTrainingConfig {
    seed: [u8; 32],
    epochs: u32,
    learning_rate: f32,
    calibration_epochs: u32,
    calibration_learning_rate: f32,
    initial_tie_logit: f32,
    ood_z_threshold: f32,
    fingerprint: BlobId,
}

impl RewardTrainingConfig {
    pub fn new(input: RewardTrainingConfigInput) -> Result<Self, RewardError> {
        if input.epochs == 0 || input.epochs > MAX_REWARD_EPOCHS {
            return Err(RewardError::InvalidEpochCount(input.epochs));
        }
        if input.calibration_epochs == 0 || input.calibration_epochs > MAX_CALIBRATION_EPOCHS {
            return Err(RewardError::InvalidCalibrationEpochCount(
                input.calibration_epochs,
            ));
        }
        if !valid_learning_rate(input.learning_rate)
            || !valid_learning_rate(input.calibration_learning_rate)
        {
            return Err(RewardError::InvalidLearningRate);
        }
        if !input.initial_tie_logit.is_finite()
            || !(-10.0..=10.0).contains(&input.initial_tie_logit)
        {
            return Err(RewardError::InvalidTieLogit);
        }
        if !input.ood_z_threshold.is_finite() || !(1.0..=100.0).contains(&input.ood_z_threshold) {
            return Err(RewardError::InvalidOodThreshold);
        }
        let fingerprint = fingerprint_config(&input);
        Ok(Self {
            seed: input.seed,
            epochs: input.epochs,
            learning_rate: input.learning_rate,
            calibration_epochs: input.calibration_epochs,
            calibration_learning_rate: input.calibration_learning_rate,
            initial_tie_logit: input.initial_tie_logit,
            ood_z_threshold: input.ood_z_threshold,
            fingerprint,
        })
    }

    pub const fn fingerprint(self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RewardTrainingConfigInput {
    pub seed: [u8; 32],
    pub epochs: u32,
    pub learning_rate: f32,
    pub calibration_epochs: u32,
    pub calibration_learning_rate: f32,
    pub initial_tie_logit: f32,
    pub ood_z_threshold: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LinearRewardHead {
    weights: Vec<f32>,
    tie_logit: f32,
}

impl LinearRewardHead {
    pub(crate) fn from_validated_parts(
        weights: Vec<f32>,
        tie_logit: f32,
    ) -> Result<Self, RewardError> {
        if weights.is_empty()
            || weights.iter().any(|value| !value.is_finite())
            || !tie_logit.is_finite()
        {
            return Err(RewardError::InvalidHeadShape);
        }
        Ok(Self { weights, tie_logit })
    }

    fn score(&self, values: &[f32]) -> f32 {
        dot(&self.weights, values)
    }

    pub(crate) fn weights(&self) -> &[f32] {
        &self.weights
    }

    pub(crate) const fn tie_logit(&self) -> f32 {
        self.tie_logit
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FrozenOodDistribution {
    mean: Vec<f32>,
    scale: Vec<f32>,
    threshold: f32,
    fingerprint: BlobId,
}

impl FrozenOodDistribution {
    fn fit<R: RewardRole>(dataset: &RewardDataset<R>, threshold: f32) -> Result<Self, RewardError> {
        let training = dataset
            .examples
            .iter()
            .filter(|example| example.partition == DatasetPartition::Train)
            .flat_map(|example| [&example.left, &example.right])
            .collect::<Vec<_>>();
        if training.is_empty() {
            return Err(RewardError::MissingRequiredPartition);
        }
        let mut mean = vec![0.0_f32; dataset.embedding_dimension];
        for embedding in &training {
            for (sum, value) in mean.iter_mut().zip(embedding.values()) {
                *sum += *value;
            }
        }
        let training_pairs = f32::from(
            u16::try_from(training.len() / 2)
                .map_err(|_| RewardError::PartitionTooLarge(training.len() / 2))?,
        );
        let count = training_pairs * 2.0;
        for value in &mut mean {
            *value /= count;
        }
        let mut variance = vec![0.0_f32; dataset.embedding_dimension];
        for embedding in &training {
            for ((sum, value), average) in variance.iter_mut().zip(embedding.values()).zip(&mean) {
                let residual = *value - *average;
                *sum += residual * residual;
            }
        }
        let scale = variance
            .into_iter()
            .map(|value| (value / count).sqrt().max(MIN_OOD_SCALE))
            .collect::<Vec<_>>();
        Self::from_validated_parts(mean, scale, threshold)
    }

    fn distance(&self, values: &[f32]) -> f32 {
        values
            .iter()
            .zip(&self.mean)
            .zip(&self.scale)
            .map(|((value, mean), scale)| ((*value - *mean) / *scale).abs())
            .fold(0.0_f32, f32::max)
    }

    pub(crate) fn from_validated_parts(
        mean: Vec<f32>,
        scale: Vec<f32>,
        threshold: f32,
    ) -> Result<Self, RewardError> {
        if mean.is_empty()
            || mean.len() != scale.len()
            || mean.iter().any(|value| !value.is_finite())
            || scale
                .iter()
                .any(|value| !value.is_finite() || *value <= 0.0)
            || !threshold.is_finite()
            || !(1.0..=100.0).contains(&threshold)
        {
            return Err(RewardError::InvalidOodDistribution);
        }
        let fingerprint = fingerprint_ood(&mean, &scale, threshold);
        Ok(Self {
            mean,
            scale,
            threshold,
            fingerprint,
        })
    }

    pub(crate) fn mean(&self) -> &[f32] {
        &self.mean
    }

    pub(crate) fn scale(&self) -> &[f32] {
        &self.scale
    }

    pub(crate) const fn threshold(&self) -> f32 {
        self.threshold
    }

    pub(crate) const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimedHumanThreshold {
    ExploratoryOnly,
    DeclaredCalibrationVolumeMet,
    DeclaredActiveShelfVolumeMet,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FinalEmbeddingRewardEnsemble<R: RewardRole> {
    heads: [LinearRewardHead; REWARD_ENSEMBLE_HEADS],
    calibration_temperature: f32,
    ood: FrozenOodDistribution,
    embedding_dimension: usize,
    model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    dataset_fingerprint: BlobId,
    training_fingerprint: BlobId,
    trained_parameters_fingerprint: BlobId,
    label_composition: LabelComposition,
    claimed_human_threshold: ClaimedHumanThreshold,
    fingerprint: BlobId,
    role: PhantomData<R>,
}

pub(crate) struct RewardModelParts {
    pub(crate) heads: [LinearRewardHead; REWARD_ENSEMBLE_HEADS],
    pub(crate) calibration_temperature: f32,
    pub(crate) ood: FrozenOodDistribution,
    pub(crate) embedding_dimension: usize,
    pub(crate) model_fingerprint: BlobId,
    pub(crate) tokenizer_fingerprint: BlobId,
    pub(crate) dataset_fingerprint: BlobId,
    pub(crate) training_fingerprint: BlobId,
    pub(crate) label_composition: LabelComposition,
    pub(crate) claimed_human_threshold: ClaimedHumanThreshold,
    pub(crate) expected_parameters_fingerprint: BlobId,
}

impl<R: RewardRole> FinalEmbeddingRewardEnsemble<R> {
    pub fn train(
        dataset: &RewardDataset<R>,
        config: RewardTrainingConfig,
    ) -> Result<Self, RewardError> {
        let train_examples = dataset
            .examples
            .iter()
            .filter(|example| example.partition == DatasetPartition::Train)
            .collect::<Vec<_>>();
        let calibration_examples = dataset
            .examples
            .iter()
            .filter(|example| example.partition == DatasetPartition::Calibration)
            .collect::<Vec<_>>();
        if train_examples.is_empty() || calibration_examples.is_empty() {
            return Err(RewardError::MissingRequiredPartition);
        }
        let mut trained = Vec::with_capacity(REWARD_ENSEMBLE_HEADS);
        for head_index in 0..REWARD_ENSEMBLE_HEADS {
            trained.push(train_one_head(
                dataset.embedding_dimension,
                &train_examples,
                config,
                head_index,
            )?);
        }
        let heads: [LinearRewardHead; REWARD_ENSEMBLE_HEADS] = trained
            .try_into()
            .map_err(|_| RewardError::InvalidHeadCount)?;
        let trained_parameters_fingerprint = fingerprint_parameters(&heads);
        let calibration_temperature = calibrate_temperature(
            &heads,
            &calibration_examples,
            config.calibration_epochs,
            config.calibration_learning_rate,
        )?;
        let ood = FrozenOodDistribution::fit(dataset, config.ood_z_threshold)?;
        let claimed_human_threshold = claimed_human_threshold_for(dataset.label_composition);
        Self::from_validated_parts(RewardModelParts {
            heads,
            calibration_temperature,
            ood,
            embedding_dimension: dataset.embedding_dimension,
            model_fingerprint: dataset.model_fingerprint,
            tokenizer_fingerprint: dataset.tokenizer_fingerprint,
            dataset_fingerprint: dataset.fingerprint,
            training_fingerprint: config.fingerprint,
            label_composition: dataset.label_composition,
            claimed_human_threshold,
            expected_parameters_fingerprint: trained_parameters_fingerprint,
        })
    }

    pub(crate) fn from_validated_parts(parts: RewardModelParts) -> Result<Self, RewardError> {
        if parts.embedding_dimension == 0
            || parts.embedding_dimension > crate::MAX_EMBEDDING_DIMENSIONS
            || parts.heads.iter().any(|head| {
                head.weights.len() != parts.embedding_dimension
                    || head.weights.iter().any(|value| !value.is_finite())
                    || !head.tie_logit.is_finite()
            })
        {
            return Err(RewardError::InvalidHeadShape);
        }
        if !parts.calibration_temperature.is_finite()
            || !(0.01..=100.0).contains(&parts.calibration_temperature)
        {
            return Err(RewardError::InvalidCalibrationTemperature);
        }
        if parts.ood.mean.len() != parts.embedding_dimension {
            return Err(RewardError::InvalidOodDistribution);
        }
        if parts.claimed_human_threshold != claimed_human_threshold_for(parts.label_composition) {
            return Err(RewardError::ThresholdDoesNotMatchDeclaredHumanEvidence);
        }
        let trained_parameters_fingerprint = fingerprint_parameters(&parts.heads);
        if trained_parameters_fingerprint != parts.expected_parameters_fingerprint {
            return Err(RewardError::ParameterFingerprintMismatch);
        }
        let fingerprint = fingerprint_model::<R>(&parts, trained_parameters_fingerprint);
        Ok(Self {
            heads: parts.heads,
            calibration_temperature: parts.calibration_temperature,
            ood: parts.ood,
            embedding_dimension: parts.embedding_dimension,
            model_fingerprint: parts.model_fingerprint,
            tokenizer_fingerprint: parts.tokenizer_fingerprint,
            dataset_fingerprint: parts.dataset_fingerprint,
            training_fingerprint: parts.training_fingerprint,
            trained_parameters_fingerprint,
            label_composition: parts.label_composition,
            claimed_human_threshold: parts.claimed_human_threshold,
            fingerprint,
            role: PhantomData,
        })
    }

    pub fn score(&self, embedding: &FrozenEmbedding) -> Result<RewardAssessment, RewardError> {
        require_role::<R>(embedding)?;
        self.validate_embedding(embedding)?;
        let distance = self.ood.distance(embedding.values());
        if distance > self.ood.threshold {
            return Ok(RewardAssessment::Abstain(OodAbstention {
                distance,
                threshold: self.ood.threshold,
                distribution_fingerprint: self.ood.fingerprint,
            }));
        }
        let mut head_scores = [0.0_f32; REWARD_ENSEMBLE_HEADS];
        for (output, head) in head_scores.iter_mut().zip(&self.heads) {
            *output = head.score(embedding.values());
        }
        let mean = head_scores.iter().sum::<f32>() / REWARD_HEAD_DIVISOR;
        if !mean.is_finite() {
            return Err(RewardError::NonFiniteScore);
        }
        Ok(RewardAssessment::Score(RewardScore { head_scores, mean }))
    }

    pub fn compare(
        &self,
        left: &FrozenEmbedding,
        right: &FrozenEmbedding,
    ) -> Result<RewardComparison, RewardError> {
        match (self.score(left)?, self.score(right)?) {
            (RewardAssessment::Abstain(reason), _) | (_, RewardAssessment::Abstain(reason)) => {
                Ok(RewardComparison::Abstain(reason))
            }
            (RewardAssessment::Score(_), RewardAssessment::Score(_)) => {
                let mut probabilities = [0.0_f32; 3];
                for head in &self.heads {
                    let delta = head.score(left.values()) - head.score(right.values());
                    let prediction = davidson_probabilities(
                        delta / self.calibration_temperature,
                        head.tie_logit / self.calibration_temperature,
                    )?;
                    for (sum, value) in probabilities.iter_mut().zip(prediction) {
                        *sum += value / REWARD_HEAD_DIVISOR;
                    }
                }
                Ok(RewardComparison::Probabilities(probabilities))
            }
        }
    }

    fn validate_embedding(&self, embedding: &FrozenEmbedding) -> Result<(), RewardError> {
        if embedding.dimension() != self.embedding_dimension {
            return Err(RewardError::EmbeddingDimensionMismatch {
                expected: self.embedding_dimension,
                actual: embedding.dimension(),
            });
        }
        if embedding.model_fingerprint() != self.model_fingerprint
            || embedding.tokenizer_fingerprint() != self.tokenizer_fingerprint
        {
            return Err(RewardError::EmbeddingSpaceMismatch);
        }
        Ok(())
    }

    pub const fn role(&self) -> RewardHeadRole {
        R::ROLE
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

    pub const fn trained_parameters_fingerprint(&self) -> BlobId {
        self.trained_parameters_fingerprint
    }

    pub const fn label_composition(&self) -> LabelComposition {
        self.label_composition
    }

    /// Returns only the volume threshold implied by caller-declared label
    /// sources. It is not verified human activation authority.
    pub const fn claimed_human_threshold(&self) -> ClaimedHumanThreshold {
        self.claimed_human_threshold
    }

    pub const fn calibration_temperature(&self) -> f32 {
        self.calibration_temperature
    }

    pub const fn fingerprint(&self) -> BlobId {
        self.fingerprint
    }

    pub(crate) fn heads(&self) -> &[LinearRewardHead; REWARD_ENSEMBLE_HEADS] {
        &self.heads
    }

    pub(crate) fn ood(&self) -> &FrozenOodDistribution {
        &self.ood
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RewardScore {
    pub head_scores: [f32; REWARD_ENSEMBLE_HEADS],
    pub mean: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OodAbstention {
    pub distance: f32,
    pub threshold: f32,
    pub distribution_fingerprint: BlobId,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RewardAssessment {
    Score(RewardScore),
    Abstain(OodAbstention),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RewardComparison {
    Probabilities([f32; 3]),
    Abstain(OodAbstention),
}

fn train_one_head(
    dimension: usize,
    examples: &[&RewardPairExample],
    config: RewardTrainingConfig,
    head_index: usize,
) -> Result<LinearRewardHead, RewardError> {
    let mut rng = ChaCha20Rng::from_seed(derive_head_seed(config.seed, head_index));
    let mut parameters = (0..dimension)
        .map(|_| seeded_weight(&mut rng))
        .chain(std::iter::once(config.initial_tie_logit))
        .collect::<Vec<_>>();
    let mut adam = Adam::new(parameters.len()).map_err(RewardError::optimizer)?;
    let mut order = (0..examples.len()).collect::<Vec<_>>();
    for _ in 0..config.epochs {
        deterministic_shuffle(&mut order, &mut rng);
        for &example_index in &order {
            let (_, gradient) = reward_loss_and_gradient(&parameters, examples[example_index])?;
            adam.update(&mut parameters, &gradient, config.learning_rate)
                .map_err(RewardError::optimizer)?;
        }
    }
    let tie_logit = parameters.pop().ok_or(RewardError::InvalidHeadShape)?;
    Ok(LinearRewardHead {
        weights: parameters,
        tie_logit,
    })
}

fn reward_loss_and_gradient(
    parameters: &[f32],
    example: &RewardPairExample,
) -> Result<(f32, Vec<f32>), RewardError> {
    let dimension = example.left.dimension();
    if parameters.len() != dimension + 1 {
        return Err(RewardError::InvalidHeadShape);
    }
    let (weights, tie) = parameters.split_at(dimension);
    let difference = example
        .left
        .values()
        .iter()
        .zip(example.right.values())
        .map(|(left, right)| *left - *right)
        .collect::<Vec<_>>();
    let delta = dot(weights, &difference);
    let (loss, delta_gradient, tie_gradient) =
        davidson_loss_gradient(delta, tie[0], example.preference)?;
    let mut gradient = difference
        .iter()
        .map(|value| delta_gradient * *value)
        .collect::<Vec<_>>();
    gradient.push(tie_gradient);
    if gradient.iter().any(|value| !value.is_finite()) {
        return Err(RewardError::NonFiniteLoss);
    }
    Ok((loss, gradient))
}

fn davidson_loss_gradient(
    delta: f32,
    tie_logit: f32,
    preference: PreferenceLabel,
) -> Result<(f32, f32, f32), RewardError> {
    let probabilities = davidson_probabilities(delta, tie_logit)?;
    let target = preference.target();
    let selected = match preference {
        PreferenceLabel::Left => probabilities[0],
        PreferenceLabel::Right => probabilities[1],
        PreferenceLabel::Tie => probabilities[2],
    };
    let loss = -selected.ln();
    let delta_gradient = 0.5 * ((probabilities[0] - target[0]) - (probabilities[1] - target[1]));
    let tie_gradient = probabilities[2] - target[2];
    if loss.is_finite() && delta_gradient.is_finite() && tie_gradient.is_finite() {
        Ok((loss, delta_gradient, tie_gradient))
    } else {
        Err(RewardError::NonFiniteLoss)
    }
}

fn davidson_probabilities(delta: f32, tie_logit: f32) -> Result<[f32; 3], RewardError> {
    if !delta.is_finite() || !tie_logit.is_finite() {
        return Err(RewardError::NonFiniteScore);
    }
    let logits = [delta * 0.5, delta * -0.5, tie_logit];
    let maximum = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exponentials = logits.map(|value| (value - maximum).exp());
    let denominator: f32 = exponentials.iter().sum();
    if !denominator.is_finite() || denominator <= 0.0 {
        return Err(RewardError::NonFiniteScore);
    }
    Ok(exponentials.map(|value| value / denominator))
}

fn calibrate_temperature(
    heads: &[LinearRewardHead; REWARD_ENSEMBLE_HEADS],
    examples: &[&RewardPairExample],
    epochs: u32,
    learning_rate: f32,
) -> Result<f32, RewardError> {
    if examples.is_empty() {
        return Err(RewardError::MissingRequiredPartition);
    }
    let mut log_temperature = vec![0.0_f32];
    let mut adam = Adam::new(1).map_err(RewardError::optimizer)?;
    for _ in 0..epochs {
        let (_, gradient) =
            calibration_loss_and_log_temperature_gradient(heads, examples, log_temperature[0])?;
        adam.update(&mut log_temperature, &[gradient], learning_rate)
            .map_err(RewardError::optimizer)?;
        log_temperature[0] = log_temperature[0].clamp(0.01_f32.ln(), 100.0_f32.ln());
    }
    let temperature = log_temperature[0].exp();
    if temperature.is_finite() {
        Ok(temperature)
    } else {
        Err(RewardError::InvalidCalibrationTemperature)
    }
}

fn calibration_loss_and_log_temperature_gradient(
    heads: &[LinearRewardHead; REWARD_ENSEMBLE_HEADS],
    examples: &[&RewardPairExample],
    log_temperature: f32,
) -> Result<(f32, f32), RewardError> {
    if examples.is_empty() || !log_temperature.is_finite() {
        return Err(RewardError::InvalidCalibrationTemperature);
    }
    let temperature = log_temperature.exp();
    if !temperature.is_finite() || temperature <= 0.0 {
        return Err(RewardError::InvalidCalibrationTemperature);
    }
    let mut loss = 0.0_f32;
    let mut gradient = 0.0_f32;
    for example in examples {
        let target = example.preference.target();
        for head in heads {
            let delta = head.score(example.left.values()) - head.score(example.right.values());
            let scaled_logits = [
                delta * 0.5 / temperature,
                delta * -0.5 / temperature,
                head.tie_logit / temperature,
            ];
            let probabilities = softmax_three(scaled_logits)?;
            loss -= probabilities
                .iter()
                .zip(target)
                .filter(|(_, expected)| *expected > 0.0)
                .map(|(probability, expected)| expected * probability.ln())
                .sum::<f32>();
            gradient -= probabilities
                .iter()
                .zip(target)
                .zip(scaled_logits)
                .map(|((probability, expected), logit)| (*probability - expected) * logit)
                .sum::<f32>();
        }
    }
    let calibration_count = f32::from(
        u16::try_from(examples.len())
            .map_err(|_| RewardError::PartitionTooLarge(examples.len()))?,
    );
    let divisor = calibration_count * REWARD_HEAD_DIVISOR;
    loss /= divisor;
    gradient /= divisor;
    if loss.is_finite() && gradient.is_finite() {
        Ok((loss, gradient))
    } else {
        Err(RewardError::NonFiniteLoss)
    }
}

fn softmax_three(logits: [f32; 3]) -> Result<[f32; 3], RewardError> {
    let maximum = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exponentials = logits.map(|value| (value - maximum).exp());
    let denominator: f32 = exponentials.iter().sum();
    if !denominator.is_finite() || denominator <= 0.0 {
        return Err(RewardError::NonFiniteScore);
    }
    Ok(exponentials.map(|value| value / denominator))
}

fn claimed_human_threshold_for(composition: LabelComposition) -> ClaimedHumanThreshold {
    if composition.claimed_human_training_pairs >= MIN_ACTIVE_SHELF_HUMAN_PAIRS
        && composition.claimed_human_training_groups >= MIN_ACTIVE_SHELF_HUMAN_GROUPS
        && composition.claimed_human_calibration_pairs > 0
    {
        ClaimedHumanThreshold::DeclaredActiveShelfVolumeMet
    } else if composition.claimed_human_training_pairs >= MIN_EVALUATOR_HUMAN_PAIRS
        && composition.claimed_human_training_groups >= MIN_EVALUATOR_HUMAN_GROUPS
        && composition.claimed_human_calibration_pairs > 0
    {
        ClaimedHumanThreshold::DeclaredCalibrationVolumeMet
    } else {
        ClaimedHumanThreshold::ExploratoryOnly
    }
}

fn valid_learning_rate(value: f32) -> bool {
    value.is_finite() && (1.0e-8..=1.0).contains(&value)
}

fn require_role<R: RewardRole>(embedding: &FrozenEmbedding) -> Result<(), RewardError> {
    if embedding.role() == R::EMBEDDING_ROLE {
        Ok(())
    } else {
        Err(RewardError::EmbeddingRoleMismatch {
            expected: R::EMBEDDING_ROLE,
            actual: embedding.role(),
        })
    }
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

fn derive_head_seed(base: [u8; 32], head_index: usize) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(HEAD_SEED_DOMAIN);
    digest.update(base);
    digest.update((head_index as u64).to_be_bytes());
    digest.finalize().into()
}

fn deterministic_shuffle(values: &mut [usize], rng: &mut ChaCha20Rng) {
    for upper in (1..values.len()).rev() {
        let bound = u32::try_from(upper + 1).expect("reward partition is bounded below u32::MAX");
        let index = usize::try_from(rng.next_u32() % bound).expect("u32 fits supported usize");
        values.swap(upper, index);
    }
}

fn fingerprint_dataset<R: RewardRole>(
    examples: &[RewardPairExample],
    dimension: usize,
    model: BlobId,
    tokenizer: BlobId,
    split_audit: SplitAudit,
    composition: LabelComposition,
) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(DATASET_DOMAIN);
    digest.update([R::ROLE.tag()]);
    digest.update((dimension as u64).to_be_bytes());
    digest.update(model.as_bytes());
    digest.update(tokenizer.as_bytes());
    digest.update(split_audit.fingerprint().as_bytes());
    digest.update((composition.claimed_human_training_pairs as u64).to_be_bytes());
    digest.update((composition.claimed_human_training_groups as u64).to_be_bytes());
    digest.update((composition.claimed_human_validation_pairs as u64).to_be_bytes());
    digest.update((composition.claimed_human_calibration_pairs as u64).to_be_bytes());
    digest.update((composition.frontier_pairs as u64).to_be_bytes());
    digest.update((examples.len() as u64).to_be_bytes());
    for example in examples {
        digest.update(example.example_id.as_ulid().to_bytes());
        digest.update([example.partition.tag()]);
        digest.update(example.groups.fingerprint().as_bytes());
        digest.update(example.left.fingerprint().as_bytes());
        digest.update(example.right.fingerprint().as_bytes());
        digest.update([example.preference.tag(), example.evidence.source.tag()]);
        if let PreferenceSource::FrontierCritic { critic_fingerprint } = example.evidence.source {
            digest.update(critic_fingerprint.as_bytes());
        }
        digest.update(example.evidence.receipt_fingerprint.as_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_config(input: &RewardTrainingConfigInput) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(CONFIG_DOMAIN);
    digest.update(input.seed);
    digest.update(input.epochs.to_be_bytes());
    digest.update(input.learning_rate.to_bits().to_be_bytes());
    digest.update(input.calibration_epochs.to_be_bytes());
    digest.update(input.calibration_learning_rate.to_bits().to_be_bytes());
    digest.update(input.initial_tie_logit.to_bits().to_be_bytes());
    digest.update(input.ood_z_threshold.to_bits().to_be_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

pub(crate) fn fingerprint_parameters(heads: &[LinearRewardHead; REWARD_ENSEMBLE_HEADS]) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(PARAMETER_DOMAIN);
    for head in heads {
        for weight in &head.weights {
            digest.update(weight.to_bits().to_be_bytes());
        }
        digest.update(head.tie_logit.to_bits().to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_ood(mean: &[f32], scale: &[f32], threshold: f32) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(OOD_DOMAIN);
    digest.update((mean.len() as u64).to_be_bytes());
    for value in mean.iter().chain(scale) {
        digest.update(value.to_bits().to_be_bytes());
    }
    digest.update(threshold.to_bits().to_be_bytes());
    BlobId::from_bytes(digest.finalize().into())
}

fn fingerprint_model<R: RewardRole>(parts: &RewardModelParts, parameters: BlobId) -> BlobId {
    let mut digest = Sha256::new();
    digest.update(MODEL_DOMAIN);
    digest.update([R::ROLE.tag()]);
    digest.update((parts.embedding_dimension as u64).to_be_bytes());
    digest.update(parts.model_fingerprint.as_bytes());
    digest.update(parts.tokenizer_fingerprint.as_bytes());
    digest.update(parts.dataset_fingerprint.as_bytes());
    digest.update(parts.training_fingerprint.as_bytes());
    digest.update(parameters.as_bytes());
    digest.update(parts.calibration_temperature.to_bits().to_be_bytes());
    digest.update(parts.ood.fingerprint.as_bytes());
    digest.update((parts.label_composition.claimed_human_training_pairs as u64).to_be_bytes());
    digest.update((parts.label_composition.claimed_human_training_groups as u64).to_be_bytes());
    digest.update((parts.label_composition.claimed_human_validation_pairs as u64).to_be_bytes());
    digest.update((parts.label_composition.claimed_human_calibration_pairs as u64).to_be_bytes());
    digest.update((parts.label_composition.frontier_pairs as u64).to_be_bytes());
    digest.update([match parts.claimed_human_threshold {
        ClaimedHumanThreshold::ExploratoryOnly => 0,
        ClaimedHumanThreshold::DeclaredCalibrationVolumeMet => 1,
        ClaimedHumanThreshold::DeclaredActiveShelfVolumeMet => 2,
    }]);
    for head in &parts.heads {
        digest.update(head.tie_logit.to_bits().to_be_bytes());
    }
    BlobId::from_bytes(digest.finalize().into())
}

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum RewardError {
    #[error(transparent)]
    Dataset(#[from] DatasetError),
    #[error("embedding role mismatch: expected {expected:?}, got {actual:?}")]
    EmbeddingRoleMismatch {
        expected: EmbeddingRole,
        actual: EmbeddingRole,
    },
    #[error("the same embedding occurrence appears on both sides of a reward pair")]
    RepeatedEmbeddingOccurrence,
    #[error("embedding dimensions differ: expected {expected}, got {actual}")]
    EmbeddingDimensionMismatch { expected: usize, actual: usize },
    #[error("embeddings do not share exact model and tokenizer fingerprints")]
    EmbeddingSpaceMismatch,
    #[error("reward datasets require train, validation, and calibration partitions")]
    MissingRequiredPartition,
    #[error("reward partition has {0} examples; maximum is {MAX_REWARD_PARTITION_EXAMPLES}")]
    PartitionTooLarge(usize),
    #[error("reward epoch count {0} is outside 1..={MAX_REWARD_EPOCHS}")]
    InvalidEpochCount(u32),
    #[error("calibration epoch count {0} is outside 1..={MAX_CALIBRATION_EPOCHS}")]
    InvalidCalibrationEpochCount(u32),
    #[error("learning rate is outside the finite range 1e-8..=1")]
    InvalidLearningRate,
    #[error("initial Davidson tie logit is outside the finite range -10..=10")]
    InvalidTieLogit,
    #[error("OOD z threshold is outside the finite range 1..=100")]
    InvalidOodThreshold,
    #[error("reward ensemble must contain exactly five heads")]
    InvalidHeadCount,
    #[error("reward head shape or values are invalid")]
    InvalidHeadShape,
    #[error("calibration temperature is outside the finite range 0.01..=100")]
    InvalidCalibrationTemperature,
    #[error("frozen OOD distribution is invalid")]
    InvalidOodDistribution,
    #[error("persisted evaluator status does not match human label evidence")]
    ThresholdDoesNotMatchDeclaredHumanEvidence,
    #[error("trained parameter fingerprint does not match exact head values")]
    ParameterFingerprintMismatch,
    #[error("reward loss or gradient is non-finite")]
    NonFiniteLoss,
    #[error("reward score is non-finite")]
    NonFiniteScore,
    #[error("deterministic Adam rejected the reward update")]
    Optimizer,
}

impl RewardError {
    const fn optimizer(_: OptimizerError) -> Self {
        Self::Optimizer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FrozenEmbeddingInput;

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
        FrozenEmbedding::new(FrozenEmbeddingInput {
            occurrence_id: ArtifactId::new(),
            source_blob_id: BlobId::digest(label),
            model_fingerprint: BlobId::digest(b"embedding-model"),
            tokenizer_fingerprint: BlobId::digest(b"tokenizer"),
            extraction_fingerprint: BlobId::digest(&[label, b"extraction"].concat()),
            role,
            input_token_count: 2,
            values: values.to_vec(),
        })
        .expect("embedding")
    }

    fn pair(
        partition: DatasetPartition,
        label: &[u8],
        preference: PreferenceLabel,
        source: PreferenceEvidence,
    ) -> RewardPairExampleInput {
        RewardPairExampleInput {
            example_id: ArtifactId::new(),
            partition,
            groups: groups(label),
            left: embedding(
                EmbeddingRole::FinalSegment,
                &[0.2, -0.1],
                &[label, b"left"].concat(),
            ),
            right: embedding(
                EmbeddingRole::FinalSegment,
                &[-0.1, 0.3],
                &[label, b"right"].concat(),
            ),
            preference,
            evidence: source,
        }
    }

    fn config() -> RewardTrainingConfig {
        RewardTrainingConfig::new(RewardTrainingConfigInput {
            seed: [9; 32],
            epochs: 3,
            learning_rate: 0.01,
            calibration_epochs: 8,
            calibration_learning_rate: 0.01,
            initial_tie_logit: 0.0,
            ood_z_threshold: 5.0,
        })
        .expect("config")
    }

    fn dataset(calibration_preference: PreferenceLabel) -> RewardDataset<Literary> {
        RewardDataset::compile(vec![
            pair(
                DatasetPartition::Train,
                b"train",
                PreferenceLabel::Left,
                PreferenceEvidence::claimed_human(BlobId::digest(b"human-train")),
            ),
            pair(
                DatasetPartition::Validation,
                b"validation",
                PreferenceLabel::Right,
                PreferenceEvidence::frontier(
                    BlobId::digest(b"critic"),
                    BlobId::digest(b"frontier-validation"),
                ),
            ),
            pair(
                DatasetPartition::Calibration,
                b"calibration",
                calibration_preference,
                PreferenceEvidence::claimed_human(BlobId::digest(b"human-calibration")),
            ),
        ])
        .expect("dataset")
    }

    #[test]
    fn davidson_tie_math_is_exact_at_symmetric_logits() {
        let probabilities = davidson_probabilities(0.0, 2.0_f32.ln()).expect("probabilities");
        assert!((probabilities[0] - 0.25).abs() < 1.0e-6);
        assert!((probabilities[1] - 0.25).abs() < 1.0e-6);
        assert!((probabilities[2] - 0.5).abs() < 1.0e-6);
        let (loss, delta_gradient, tie_gradient) =
            davidson_loss_gradient(0.0, 2.0_f32.ln(), PreferenceLabel::Tie).expect("loss");
        assert!((loss - 2.0_f32.ln()).abs() < 1.0e-6);
        assert!(delta_gradient.abs() < 1.0e-6);
        assert!((tie_gradient + 0.5).abs() < 1.0e-6);
    }

    #[test]
    fn reward_gradient_matches_finite_differences() {
        let compiled = RewardPairExample::compile::<Literary>(pair(
            DatasetPartition::Train,
            b"gradient",
            PreferenceLabel::Left,
            PreferenceEvidence::claimed_human(BlobId::digest(b"receipt")),
        ))
        .expect("pair");
        let parameters = vec![0.2, -0.3, 0.1];
        let (_, analytic) = reward_loss_and_gradient(&parameters, &compiled).expect("gradient");
        let epsilon = 1.0e-3;
        for index in 0..parameters.len() {
            let mut plus = parameters.clone();
            plus[index] += epsilon;
            let plus_loss = reward_loss_and_gradient(&plus, &compiled).expect("plus").0;
            let mut minus = parameters.clone();
            minus[index] -= epsilon;
            let minus_loss = reward_loss_and_gradient(&minus, &compiled)
                .expect("minus")
                .0;
            let numeric = (plus_loss - minus_loss) / (2.0 * epsilon);
            assert!((numeric - analytic[index]).abs() < 1.0e-4);
        }
    }

    #[test]
    fn calibration_gradient_matches_finite_differences() {
        let compiled = RewardPairExample::compile::<Literary>(pair(
            DatasetPartition::Calibration,
            b"calibration-gradient",
            PreferenceLabel::Tie,
            PreferenceEvidence::claimed_human(BlobId::digest(b"calibration-receipt")),
        ))
        .expect("pair");
        let heads = std::array::from_fn(|index| {
            let index = f32::from(u16::try_from(index).expect("five heads"));
            LinearRewardHead::from_validated_parts(
                vec![0.2 + index * 0.03, -0.1 + index * 0.01],
                -0.2 + index * 0.05,
            )
            .expect("head")
        });
        let examples = [&compiled];
        let log_temperature = 0.3_f32;
        let (_, analytic) =
            calibration_loss_and_log_temperature_gradient(&heads, &examples, log_temperature)
                .expect("analytic gradient");
        let epsilon = 1.0e-3_f32;
        let plus = calibration_loss_and_log_temperature_gradient(
            &heads,
            &examples,
            log_temperature + epsilon,
        )
        .expect("plus")
        .0;
        let minus = calibration_loss_and_log_temperature_gradient(
            &heads,
            &examples,
            log_temperature - epsilon,
        )
        .expect("minus")
        .0;
        let numeric = (plus - minus) / (2.0 * epsilon);
        assert!(
            (numeric - analytic).abs() < 1.0e-4,
            "numeric={numeric}, analytic={analytic}"
        );
    }

    #[test]
    fn reward_training_is_reproducible_and_calibration_never_changes_weights() {
        let left_labels = dataset(PreferenceLabel::Left);
        let right_labels = dataset(PreferenceLabel::Right);
        let first = FinalEmbeddingRewardEnsemble::train(&left_labels, config()).expect("train");
        let repeated = FinalEmbeddingRewardEnsemble::train(&left_labels, config()).expect("train");
        let changed_calibration =
            FinalEmbeddingRewardEnsemble::train(&right_labels, config()).expect("train");
        assert_eq!(first, repeated);
        assert_eq!(
            first.trained_parameters_fingerprint(),
            changed_calibration.trained_parameters_fingerprint()
        );
        assert_eq!(
            first.ood().fingerprint(),
            changed_calibration.ood().fingerprint()
        );
        assert_ne!(
            first.calibration_temperature().to_bits(),
            changed_calibration.calibration_temperature().to_bits()
        );
        assert_eq!(
            first.claimed_human_threshold(),
            ClaimedHumanThreshold::ExploratoryOnly
        );
        assert_eq!(first.label_composition().frontier_pairs(), 1);
    }

    #[test]
    fn frozen_training_distribution_causes_ood_abstention() {
        let dataset = dataset(PreferenceLabel::Tie);
        let model = FinalEmbeddingRewardEnsemble::train(&dataset, config()).expect("train");
        let outlier = embedding(EmbeddingRole::FinalSegment, &[100.0, -100.0], b"outlier");
        assert!(matches!(
            model.score(&outlier).expect("assessment"),
            RewardAssessment::Abstain(_)
        ));
    }

    #[test]
    fn declared_thresholds_count_only_claimed_human_labels() {
        assert_eq!(
            claimed_human_threshold_for(LabelComposition {
                claimed_human_training_pairs: 299,
                claimed_human_training_groups: 100,
                claimed_human_validation_pairs: 0,
                claimed_human_calibration_pairs: 1,
                frontier_pairs: 10_000,
            }),
            ClaimedHumanThreshold::ExploratoryOnly
        );
        assert_eq!(
            claimed_human_threshold_for(LabelComposition {
                claimed_human_training_pairs: 300,
                claimed_human_training_groups: 75,
                claimed_human_validation_pairs: 0,
                claimed_human_calibration_pairs: 1,
                frontier_pairs: 0,
            }),
            ClaimedHumanThreshold::DeclaredCalibrationVolumeMet
        );
        assert_eq!(
            claimed_human_threshold_for(LabelComposition {
                claimed_human_training_pairs: 1_000,
                claimed_human_training_groups: 200,
                claimed_human_validation_pairs: 0,
                claimed_human_calibration_pairs: 1,
                frontier_pairs: 0,
            }),
            ClaimedHumanThreshold::DeclaredActiveShelfVolumeMet
        );
        assert_eq!(
            claimed_human_threshold_for(LabelComposition {
                claimed_human_training_pairs: 1_000,
                claimed_human_training_groups: 200,
                claimed_human_validation_pairs: 0,
                claimed_human_calibration_pairs: 0,
                frontier_pairs: 100,
            }),
            ClaimedHumanThreshold::ExploratoryOnly
        );
    }

    #[test]
    fn preference_source_reclassification_changes_exact_dataset_identity() {
        let receipt = BlobId::digest(b"same external receipt");
        let human_inputs = vec![
            pair(
                DatasetPartition::Train,
                b"source-train",
                PreferenceLabel::Left,
                PreferenceEvidence::claimed_human(receipt),
            ),
            pair(
                DatasetPartition::Validation,
                b"source-validation",
                PreferenceLabel::Right,
                PreferenceEvidence::frontier(
                    BlobId::digest(b"critic"),
                    BlobId::digest(b"frontier-validation"),
                ),
            ),
            pair(
                DatasetPartition::Calibration,
                b"source-calibration",
                PreferenceLabel::Tie,
                PreferenceEvidence::claimed_human(BlobId::digest(b"human-calibration")),
            ),
        ];
        let mut frontier_inputs = human_inputs.clone();
        frontier_inputs[0].evidence =
            PreferenceEvidence::frontier(BlobId::digest(b"critic"), receipt);
        let human = RewardDataset::<Literary>::compile(human_inputs).expect("human declaration");
        let frontier =
            RewardDataset::<Literary>::compile(frontier_inputs).expect("frontier declaration");
        assert_ne!(human.fingerprint(), frontier.fingerprint());
        assert_eq!(human.label_composition().claimed_human_training_pairs(), 1);
        assert_eq!(
            frontier.label_composition().claimed_human_training_pairs(),
            0
        );
        assert_eq!(human.label_composition().frontier_pairs(), 1);
        assert_eq!(frontier.label_composition().frontier_pairs(), 2);
    }

    #[test]
    fn causal_and_literary_heads_have_separate_type_bound_identities() {
        let inputs = vec![
            pair(
                DatasetPartition::Train,
                b"role-train",
                PreferenceLabel::Left,
                PreferenceEvidence::claimed_human(BlobId::digest(b"role-train-receipt")),
            ),
            pair(
                DatasetPartition::Validation,
                b"role-validation",
                PreferenceLabel::Right,
                PreferenceEvidence::claimed_human(BlobId::digest(b"role-validation-receipt")),
            ),
            pair(
                DatasetPartition::Calibration,
                b"role-calibration",
                PreferenceLabel::Tie,
                PreferenceEvidence::claimed_human(BlobId::digest(b"role-calibration-receipt")),
            ),
        ];
        let causal = RewardDataset::<Causal>::compile(inputs.clone()).expect("causal dataset");
        let literary = RewardDataset::<Literary>::compile(inputs).expect("literary dataset");
        assert_ne!(causal.fingerprint(), literary.fingerprint());
        let causal = FinalEmbeddingRewardEnsemble::train(&causal, config()).expect("causal head");
        let literary =
            FinalEmbeddingRewardEnsemble::train(&literary, config()).expect("literary head");
        assert_eq!(causal.role(), RewardHeadRole::Causal);
        assert_eq!(literary.role(), RewardHeadRole::Literary);
        assert_ne!(causal.fingerprint(), literary.fingerprint());
    }

    #[test]
    fn plan_value_role_rejects_final_segment_embeddings() {
        let result = RewardDataset::<PlanValue>::compile(vec![
            pair(
                DatasetPartition::Train,
                b"plan-train",
                PreferenceLabel::Left,
                PreferenceEvidence::claimed_human(BlobId::digest(b"plan-train-receipt")),
            ),
            pair(
                DatasetPartition::Validation,
                b"plan-validation",
                PreferenceLabel::Left,
                PreferenceEvidence::claimed_human(BlobId::digest(b"plan-validation-receipt")),
            ),
            pair(
                DatasetPartition::Calibration,
                b"plan-calibration",
                PreferenceLabel::Left,
                PreferenceEvidence::claimed_human(BlobId::digest(b"plan-calibration-receipt")),
            ),
        ]);
        assert!(matches!(
            result,
            Err(RewardError::EmbeddingRoleMismatch {
                expected: EmbeddingRole::Plan,
                actual: EmbeddingRole::FinalSegment,
            })
        ));
    }
}
