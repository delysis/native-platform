use std::collections::{BTreeMap, BTreeSet, HashMap};

use loom_types::BlobId;
use ndarray::Array2;
use safetensors::{Dtype, SafeTensors, tensor::TensorView};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{
    ClaimedHumanThreshold, FinalEmbeddingRewardEnsemble, FrozenOodDistribution, LabelComposition,
    LinearRewardHead, RANKGEN_PROJECTION_RANK, REWARD_ENSEMBLE_HEADS, RankGenError,
    RankGenProjectionHead, RankGenVariant, RewardError, RewardHeadRole, RewardModelParts,
    RewardRole,
};

pub const MAX_LEARNED_ARTIFACT_BYTES: usize = 512 * 1024 * 1024;

const METADATA_KEY: &str = "loom.manifest";
const RANKGEN_FORMAT: &str = "loom.rankgen-projection-head.v1";
const REWARD_FORMAT: &str = "loom.final-embedding-reward-ensemble.v1";

/// Artifact class included in an out-of-band exact import expectation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LearnedArtifactKind {
    RankGenProjection,
    FinalEmbeddingReward,
}

/// Out-of-band pins required before persisted learned bytes are interpreted.
///
/// This value has no deserializer. It should come from a frozen treatment or
/// model binding, not from the artifact it is intended to verify.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LearnedArtifactExpectation {
    artifact_fingerprint: BlobId,
    kind: LearnedArtifactKind,
    learned_model_fingerprint: BlobId,
    embedding_model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    dataset_fingerprint: BlobId,
    training_fingerprint: BlobId,
}

impl LearnedArtifactExpectation {
    pub const fn new(input: LearnedArtifactExpectationInput) -> Self {
        Self {
            artifact_fingerprint: input.artifact_fingerprint,
            kind: input.kind,
            learned_model_fingerprint: input.learned_model_fingerprint,
            embedding_model_fingerprint: input.embedding_model_fingerprint,
            tokenizer_fingerprint: input.tokenizer_fingerprint,
            dataset_fingerprint: input.dataset_fingerprint,
            training_fingerprint: input.training_fingerprint,
        }
    }

    pub const fn artifact_fingerprint(self) -> BlobId {
        self.artifact_fingerprint
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LearnedArtifactExpectationInput {
    pub artifact_fingerprint: BlobId,
    pub kind: LearnedArtifactKind,
    pub learned_model_fingerprint: BlobId,
    pub embedding_model_fingerprint: BlobId,
    pub tokenizer_fingerprint: BlobId,
    pub dataset_fingerprint: BlobId,
    pub training_fingerprint: BlobId,
}

/// Canonical safetensors bytes and their out-of-band verification pins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LearnedModelArtifact {
    bytes: Vec<u8>,
    expectation: LearnedArtifactExpectation,
}

impl LearnedModelArtifact {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub const fn expectation(&self) -> LearnedArtifactExpectation {
        self.expectation
    }
}

/// The only authority statement carried by a validated learned model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LearnedModelAuthority {
    ScoringEvidenceOnly,
}

/// A persistence-validated model that still cannot authorize research claims.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedNonAuthorizingModel<M> {
    model: M,
    artifact_fingerprint: BlobId,
}

impl<M> ValidatedNonAuthorizingModel<M> {
    pub const fn model(&self) -> &M {
        &self.model
    }

    pub const fn artifact_fingerprint(&self) -> BlobId {
        self.artifact_fingerprint
    }

    pub const fn authority(&self) -> LearnedModelAuthority {
        LearnedModelAuthority::ScoringEvidenceOnly
    }

    /// Extracting the model does not add authority; learned-head types expose
    /// scoring methods only.
    pub fn into_model(self) -> M {
        self.model
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RankGenArtifactManifest {
    format: String,
    variant: RankGenVariant,
    projection_rank: usize,
    embedding_dimension: usize,
    prefix_tokens: u16,
    continuation_tokens: u16,
    embedding_model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    dataset_fingerprint: BlobId,
    training_fingerprint: BlobId,
    learned_model_fingerprint: BlobId,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RewardArtifactManifest {
    format: String,
    role: RewardHeadRole,
    head_count: usize,
    embedding_dimension: usize,
    embedding_model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    dataset_fingerprint: BlobId,
    training_fingerprint: BlobId,
    trained_parameters_fingerprint: BlobId,
    learned_model_fingerprint: BlobId,
    calibration_temperature_bits: u32,
    ood_threshold_bits: u32,
    ood_fingerprint: BlobId,
    label_composition: LabelComposition,
    claimed_human_threshold: ClaimedHumanThreshold,
}

struct OwnedF32Tensor {
    name: String,
    shape: Vec<usize>,
    bytes: Vec<u8>,
}

impl OwnedF32Tensor {
    fn new(name: impl Into<String>, shape: Vec<usize>, values: &[f32]) -> Self {
        let mut bytes = Vec::with_capacity(std::mem::size_of_val(values));
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        Self {
            name: name.into(),
            shape,
            bytes,
        }
    }
}

impl RankGenProjectionHead {
    pub fn export_safetensors(&self) -> Result<LearnedModelArtifact, LearnedArtifactError> {
        let manifest = RankGenArtifactManifest {
            format: RANKGEN_FORMAT.to_owned(),
            variant: self.variant(),
            projection_rank: RANKGEN_PROJECTION_RANK,
            embedding_dimension: self.embedding_dimension(),
            prefix_tokens: self.variant().prefix_tokens(),
            continuation_tokens: self.variant().continuation_tokens(),
            embedding_model_fingerprint: self.model_fingerprint(),
            tokenizer_fingerprint: self.tokenizer_fingerprint(),
            dataset_fingerprint: self.dataset_fingerprint(),
            training_fingerprint: self.training_fingerprint(),
            learned_model_fingerprint: self.fingerprint(),
        };
        let prefix = self
            .prefix_projection()
            .as_slice()
            .ok_or(LearnedArtifactError::TensorLayout)?;
        let continuation = self
            .continuation_projection()
            .as_slice()
            .ok_or(LearnedArtifactError::TensorLayout)?;
        let shape = vec![RANKGEN_PROJECTION_RANK, self.embedding_dimension()];
        let bytes = serialize_artifact(
            &manifest,
            &[
                OwnedF32Tensor::new("continuation_projection", shape.clone(), continuation),
                OwnedF32Tensor::new("prefix_projection", shape, prefix),
            ],
        )?;
        artifact(
            bytes,
            LearnedArtifactKind::RankGenProjection,
            self.fingerprint(),
            self.model_fingerprint(),
            self.tokenizer_fingerprint(),
            self.dataset_fingerprint(),
            self.training_fingerprint(),
        )
    }

    pub fn import_safetensors(
        bytes: &[u8],
        expected: LearnedArtifactExpectation,
    ) -> Result<ValidatedNonAuthorizingModel<Self>, LearnedArtifactError> {
        verify_artifact_envelope(bytes, expected, LearnedArtifactKind::RankGenProjection)?;
        let tensors = SafeTensors::deserialize(bytes).map_err(LearnedArtifactError::safetensors)?;
        let manifest: RankGenArtifactManifest = read_manifest(bytes)?;
        verify_common_manifest(
            expected,
            manifest.embedding_model_fingerprint,
            manifest.tokenizer_fingerprint,
            manifest.dataset_fingerprint,
            manifest.training_fingerprint,
            manifest.learned_model_fingerprint,
        )?;
        if manifest.format != RANKGEN_FORMAT
            || manifest.projection_rank != RANKGEN_PROJECTION_RANK
            || manifest.embedding_dimension == 0
            || manifest.embedding_dimension > crate::MAX_EMBEDDING_DIMENSIONS
            || manifest.prefix_tokens != manifest.variant.prefix_tokens()
            || manifest.continuation_tokens != manifest.variant.continuation_tokens()
        {
            return Err(LearnedArtifactError::InvalidMetadata);
        }
        require_tensor_names(&tensors, &["continuation_projection", "prefix_projection"])?;
        let shape = [RANKGEN_PROJECTION_RANK, manifest.embedding_dimension];
        let prefix = decode_f32_tensor(&tensors, "prefix_projection", &shape)?;
        let continuation = decode_f32_tensor(&tensors, "continuation_projection", &shape)?;
        let prefix = Array2::from_shape_vec(
            (RANKGEN_PROJECTION_RANK, manifest.embedding_dimension),
            prefix,
        )
        .map_err(|_| LearnedArtifactError::InvalidTensorShape)?;
        let continuation = Array2::from_shape_vec(
            (RANKGEN_PROJECTION_RANK, manifest.embedding_dimension),
            continuation,
        )
        .map_err(|_| LearnedArtifactError::InvalidTensorShape)?;
        let model = Self::from_validated_parts(
            manifest.variant,
            prefix,
            continuation,
            manifest.embedding_model_fingerprint,
            manifest.tokenizer_fingerprint,
            manifest.dataset_fingerprint,
            manifest.training_fingerprint,
        )
        .map_err(LearnedArtifactError::rankgen)?;
        if model.fingerprint() != manifest.learned_model_fingerprint {
            return Err(LearnedArtifactError::LearnedModelFingerprintMismatch);
        }
        Ok(ValidatedNonAuthorizingModel {
            model,
            artifact_fingerprint: expected.artifact_fingerprint,
        })
    }
}

impl<R: RewardRole> FinalEmbeddingRewardEnsemble<R> {
    pub fn export_safetensors(&self) -> Result<LearnedModelArtifact, LearnedArtifactError> {
        let manifest = RewardArtifactManifest {
            format: REWARD_FORMAT.to_owned(),
            role: self.role(),
            head_count: REWARD_ENSEMBLE_HEADS,
            embedding_dimension: self.embedding_dimension(),
            embedding_model_fingerprint: self.model_fingerprint(),
            tokenizer_fingerprint: self.tokenizer_fingerprint(),
            dataset_fingerprint: self.dataset_fingerprint(),
            training_fingerprint: self.training_fingerprint(),
            trained_parameters_fingerprint: self.trained_parameters_fingerprint(),
            learned_model_fingerprint: self.fingerprint(),
            calibration_temperature_bits: self.calibration_temperature().to_bits(),
            ood_threshold_bits: self.ood().threshold().to_bits(),
            ood_fingerprint: self.ood().fingerprint(),
            label_composition: self.label_composition(),
            claimed_human_threshold: self.claimed_human_threshold(),
        };
        let mut tensors = Vec::with_capacity(REWARD_ENSEMBLE_HEADS + 4);
        for (index, head) in self.heads().iter().enumerate() {
            tensors.push(OwnedF32Tensor::new(
                format!("head_{index}_weights"),
                vec![self.embedding_dimension()],
                head.weights(),
            ));
        }
        let tie_logits = self
            .heads()
            .iter()
            .map(LinearRewardHead::tie_logit)
            .collect::<Vec<_>>();
        tensors.push(OwnedF32Tensor::new(
            "tie_logits",
            vec![REWARD_ENSEMBLE_HEADS],
            &tie_logits,
        ));
        tensors.push(OwnedF32Tensor::new(
            "calibration_temperature",
            vec![1],
            &[self.calibration_temperature()],
        ));
        tensors.push(OwnedF32Tensor::new(
            "ood_mean",
            vec![self.embedding_dimension()],
            self.ood().mean(),
        ));
        tensors.push(OwnedF32Tensor::new(
            "ood_scale",
            vec![self.embedding_dimension()],
            self.ood().scale(),
        ));
        let bytes = serialize_artifact(&manifest, &tensors)?;
        artifact(
            bytes,
            LearnedArtifactKind::FinalEmbeddingReward,
            self.fingerprint(),
            self.model_fingerprint(),
            self.tokenizer_fingerprint(),
            self.dataset_fingerprint(),
            self.training_fingerprint(),
        )
    }

    pub fn import_safetensors(
        bytes: &[u8],
        expected: LearnedArtifactExpectation,
    ) -> Result<ValidatedNonAuthorizingModel<Self>, LearnedArtifactError> {
        verify_artifact_envelope(bytes, expected, LearnedArtifactKind::FinalEmbeddingReward)?;
        let tensors = SafeTensors::deserialize(bytes).map_err(LearnedArtifactError::safetensors)?;
        let manifest: RewardArtifactManifest = read_manifest(bytes)?;
        verify_common_manifest(
            expected,
            manifest.embedding_model_fingerprint,
            manifest.tokenizer_fingerprint,
            manifest.dataset_fingerprint,
            manifest.training_fingerprint,
            manifest.learned_model_fingerprint,
        )?;
        if manifest.format != REWARD_FORMAT
            || manifest.role != R::ROLE
            || manifest.head_count != REWARD_ENSEMBLE_HEADS
            || manifest.embedding_dimension == 0
            || manifest.embedding_dimension > crate::MAX_EMBEDDING_DIMENSIONS
        {
            return Err(LearnedArtifactError::InvalidMetadata);
        }
        let mut expected_names = (0..REWARD_ENSEMBLE_HEADS)
            .map(|index| format!("head_{index}_weights"))
            .collect::<Vec<_>>();
        expected_names.extend([
            "calibration_temperature".to_owned(),
            "ood_mean".to_owned(),
            "ood_scale".to_owned(),
            "tie_logits".to_owned(),
        ]);
        require_owned_tensor_names(&tensors, &expected_names)?;
        let ties = decode_f32_tensor(&tensors, "tie_logits", &[REWARD_ENSEMBLE_HEADS])?;
        let mut heads = Vec::with_capacity(REWARD_ENSEMBLE_HEADS);
        for (index, tie_logit) in ties.into_iter().enumerate() {
            let weights = decode_f32_tensor(
                &tensors,
                &format!("head_{index}_weights"),
                &[manifest.embedding_dimension],
            )?;
            heads.push(
                LinearRewardHead::from_validated_parts(weights, tie_logit)
                    .map_err(LearnedArtifactError::reward)?,
            );
        }
        let heads: [LinearRewardHead; REWARD_ENSEMBLE_HEADS] = heads
            .try_into()
            .map_err(|_| LearnedArtifactError::InvalidTensorShape)?;
        let temperature = decode_f32_tensor(&tensors, "calibration_temperature", &[1])?[0];
        let mean = decode_f32_tensor(&tensors, "ood_mean", &[manifest.embedding_dimension])?;
        let scale = decode_f32_tensor(&tensors, "ood_scale", &[manifest.embedding_dimension])?;
        if temperature.to_bits() != manifest.calibration_temperature_bits {
            return Err(LearnedArtifactError::MetadataTensorMismatch);
        }
        let threshold = f32::from_bits(manifest.ood_threshold_bits);
        let ood = FrozenOodDistribution::from_validated_parts(mean, scale, threshold)
            .map_err(LearnedArtifactError::reward)?;
        if ood.fingerprint() != manifest.ood_fingerprint {
            return Err(LearnedArtifactError::MetadataTensorMismatch);
        }
        let model = Self::from_validated_parts(RewardModelParts {
            heads,
            calibration_temperature: temperature,
            ood,
            embedding_dimension: manifest.embedding_dimension,
            model_fingerprint: manifest.embedding_model_fingerprint,
            tokenizer_fingerprint: manifest.tokenizer_fingerprint,
            dataset_fingerprint: manifest.dataset_fingerprint,
            training_fingerprint: manifest.training_fingerprint,
            label_composition: manifest.label_composition,
            claimed_human_threshold: manifest.claimed_human_threshold,
            expected_parameters_fingerprint: manifest.trained_parameters_fingerprint,
        })
        .map_err(LearnedArtifactError::reward)?;
        if model.fingerprint() != manifest.learned_model_fingerprint {
            return Err(LearnedArtifactError::LearnedModelFingerprintMismatch);
        }
        Ok(ValidatedNonAuthorizingModel {
            model,
            artifact_fingerprint: expected.artifact_fingerprint,
        })
    }
}

fn artifact(
    bytes: Vec<u8>,
    kind: LearnedArtifactKind,
    learned_model_fingerprint: BlobId,
    embedding_model_fingerprint: BlobId,
    tokenizer_fingerprint: BlobId,
    dataset_fingerprint: BlobId,
    training_fingerprint: BlobId,
) -> Result<LearnedModelArtifact, LearnedArtifactError> {
    if bytes.is_empty() || bytes.len() > MAX_LEARNED_ARTIFACT_BYTES {
        return Err(LearnedArtifactError::InvalidArtifactSize(bytes.len()));
    }
    let artifact_fingerprint = BlobId::digest(&bytes);
    Ok(LearnedModelArtifact {
        bytes,
        expectation: LearnedArtifactExpectation::new(LearnedArtifactExpectationInput {
            artifact_fingerprint,
            kind,
            learned_model_fingerprint,
            embedding_model_fingerprint,
            tokenizer_fingerprint,
            dataset_fingerprint,
            training_fingerprint,
        }),
    })
}

fn serialize_artifact<T: Serialize>(
    manifest: &T,
    tensors: &[OwnedF32Tensor],
) -> Result<Vec<u8>, LearnedArtifactError> {
    let manifest = serde_json::to_string(manifest).map_err(LearnedArtifactError::json)?;
    let metadata = HashMap::from([(METADATA_KEY.to_owned(), manifest)]);
    let mut views = BTreeMap::new();
    for tensor in tensors {
        let view = TensorView::new(Dtype::F32, tensor.shape.clone(), &tensor.bytes)
            .map_err(LearnedArtifactError::safetensors)?;
        if views.insert(tensor.name.clone(), view).is_some() {
            return Err(LearnedArtifactError::DuplicateTensorName);
        }
    }
    let bytes =
        safetensors::serialize(views, Some(metadata)).map_err(LearnedArtifactError::safetensors)?;
    if bytes.len() > MAX_LEARNED_ARTIFACT_BYTES {
        return Err(LearnedArtifactError::InvalidArtifactSize(bytes.len()));
    }
    Ok(bytes)
}

fn read_manifest<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, LearnedArtifactError> {
    let (_, metadata) =
        SafeTensors::read_metadata(bytes).map_err(LearnedArtifactError::safetensors)?;
    let entries = metadata
        .metadata()
        .as_ref()
        .ok_or(LearnedArtifactError::MissingMetadata)?;
    if entries.len() != 1 {
        return Err(LearnedArtifactError::UnexpectedMetadata);
    }
    let manifest = entries
        .get(METADATA_KEY)
        .ok_or(LearnedArtifactError::MissingMetadata)?;
    serde_json::from_str(manifest).map_err(LearnedArtifactError::json)
}

fn verify_artifact_envelope(
    bytes: &[u8],
    expected: LearnedArtifactExpectation,
    kind: LearnedArtifactKind,
) -> Result<(), LearnedArtifactError> {
    if bytes.is_empty() || bytes.len() > MAX_LEARNED_ARTIFACT_BYTES {
        return Err(LearnedArtifactError::InvalidArtifactSize(bytes.len()));
    }
    if expected.kind != kind {
        return Err(LearnedArtifactError::ArtifactKindMismatch);
    }
    if BlobId::digest(bytes) != expected.artifact_fingerprint {
        return Err(LearnedArtifactError::ArtifactFingerprintMismatch);
    }
    Ok(())
}

fn verify_common_manifest(
    expected: LearnedArtifactExpectation,
    embedding_model: BlobId,
    tokenizer: BlobId,
    dataset: BlobId,
    training: BlobId,
    learned_model: BlobId,
) -> Result<(), LearnedArtifactError> {
    if expected.embedding_model_fingerprint != embedding_model
        || expected.tokenizer_fingerprint != tokenizer
        || expected.dataset_fingerprint != dataset
        || expected.training_fingerprint != training
        || expected.learned_model_fingerprint != learned_model
    {
        return Err(LearnedArtifactError::PinnedMetadataMismatch);
    }
    Ok(())
}

fn require_tensor_names(
    tensors: &SafeTensors<'_>,
    expected: &[&str],
) -> Result<(), LearnedArtifactError> {
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    let actual = tensors.names().into_iter().collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(LearnedArtifactError::UnexpectedTensorSet)
    }
}

fn require_owned_tensor_names(
    tensors: &SafeTensors<'_>,
    expected: &[String],
) -> Result<(), LearnedArtifactError> {
    let expected = expected.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let actual = tensors.names().into_iter().collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(LearnedArtifactError::UnexpectedTensorSet)
    }
}

fn decode_f32_tensor(
    tensors: &SafeTensors<'_>,
    name: &str,
    expected_shape: &[usize],
) -> Result<Vec<f32>, LearnedArtifactError> {
    let tensor = tensors
        .tensor(name)
        .map_err(LearnedArtifactError::safetensors)?;
    if tensor.dtype() != Dtype::F32 {
        return Err(LearnedArtifactError::InvalidTensorDtype);
    }
    if tensor.shape() != expected_shape {
        return Err(LearnedArtifactError::InvalidTensorShape);
    }
    let expected_elements = expected_shape
        .iter()
        .try_fold(1_usize, |product, dimension| {
            product.checked_mul(*dimension)
        })
        .ok_or(LearnedArtifactError::InvalidTensorShape)?;
    let expected_bytes = expected_elements
        .checked_mul(size_of::<f32>())
        .ok_or(LearnedArtifactError::InvalidTensorShape)?;
    if tensor.data().len() != expected_bytes {
        return Err(LearnedArtifactError::InvalidTensorShape);
    }
    let values = tensor
        .data()
        .chunks_exact(size_of::<f32>())
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect::<Vec<_>>();
    if values.iter().any(|value| !value.is_finite()) {
        return Err(LearnedArtifactError::NonFiniteTensor);
    }
    Ok(values)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum LearnedArtifactError {
    #[error("learned artifact byte length {0} is outside the accepted bound")]
    InvalidArtifactSize(usize),
    #[error("learned artifact SHA-256 does not match the out-of-band pin")]
    ArtifactFingerprintMismatch,
    #[error("learned artifact kind does not match the import entry point")]
    ArtifactKindMismatch,
    #[error("learned artifact metadata is absent")]
    MissingMetadata,
    #[error("learned artifact has unexpected metadata keys")]
    UnexpectedMetadata,
    #[error("learned artifact metadata is malformed or inconsistent")]
    InvalidMetadata,
    #[error("learned artifact metadata does not match frozen external pins")]
    PinnedMetadataMismatch,
    #[error("learned artifact contains an unexpected tensor set")]
    UnexpectedTensorSet,
    #[error("learned artifact repeats a tensor name")]
    DuplicateTensorName,
    #[error("learned tensor dtype is not F32")]
    InvalidTensorDtype,
    #[error("learned tensor shape is invalid")]
    InvalidTensorShape,
    #[error("learned tensor has a non-standard in-memory layout")]
    TensorLayout,
    #[error("learned tensor contains a non-finite value")]
    NonFiniteTensor,
    #[error("learned metadata and tensor values disagree")]
    MetadataTensorMismatch,
    #[error("reconstructed learned-model fingerprint differs from the manifest")]
    LearnedModelFingerprintMismatch,
    #[error("safetensors container validation failed")]
    Safetensors,
    #[error("canonical learned metadata JSON validation failed")]
    Json,
    #[error("RankGen model validation failed")]
    RankGen,
    #[error("reward model validation failed")]
    Reward,
}

impl LearnedArtifactError {
    fn safetensors(_: safetensors::SafeTensorError) -> Self {
        Self::Safetensors
    }

    fn json(_: serde_json::Error) -> Self {
        Self::Json
    }

    fn rankgen(_: RankGenError) -> Self {
        Self::RankGen
    }

    fn reward(_: RewardError) -> Self {
        Self::Reward
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClaimedHumanThreshold, Literary};

    fn rankgen_model() -> RankGenProjectionHead {
        let prefix = Array2::from_shape_fn((RANKGEN_PROJECTION_RANK, 2), |(row, column)| {
            f32::from(u16::try_from(row).expect("small row")) * 0.001
                + f32::from(u16::try_from(column).expect("small column")) * 0.01
        });
        let continuation = Array2::from_shape_fn((RANKGEN_PROJECTION_RANK, 2), |(row, column)| {
            f32::from(u16::try_from(row).expect("small row")) * -0.002
                + f32::from(u16::try_from(column).expect("small column")) * 0.005
        });
        RankGenProjectionHead::from_validated_parts(
            RankGenVariant::ChapterTransition1024x256,
            prefix,
            continuation,
            BlobId::digest(b"model"),
            BlobId::digest(b"tokenizer"),
            BlobId::digest(b"dataset"),
            BlobId::digest(b"training"),
        )
        .expect("model")
    }

    fn reward_model() -> FinalEmbeddingRewardEnsemble<Literary> {
        let heads = std::array::from_fn(|head| {
            let head = f32::from(u16::try_from(head).expect("five heads"));
            LinearRewardHead::from_validated_parts(
                vec![head * 0.1 + 0.2, head * -0.03],
                head * 0.02,
            )
            .expect("head")
        });
        let parameters = crate::reward::fingerprint_parameters(&heads);
        let ood = FrozenOodDistribution::from_validated_parts(vec![0.0, 0.0], vec![1.0, 2.0], 5.0)
            .expect("OOD");
        FinalEmbeddingRewardEnsemble::from_validated_parts(RewardModelParts {
            heads,
            calibration_temperature: 1.25,
            ood,
            embedding_dimension: 2,
            model_fingerprint: BlobId::digest(b"model"),
            tokenizer_fingerprint: BlobId::digest(b"tokenizer"),
            dataset_fingerprint: BlobId::digest(b"dataset"),
            training_fingerprint: BlobId::digest(b"training"),
            label_composition: LabelComposition {
                claimed_human_training_pairs: 10,
                claimed_human_training_groups: 5,
                claimed_human_validation_pairs: 0,
                claimed_human_calibration_pairs: 1,
                frontier_pairs: 20,
            },
            claimed_human_threshold: ClaimedHumanThreshold::ExploratoryOnly,
            expected_parameters_fingerprint: parameters,
        })
        .expect("model")
    }

    #[test]
    fn rankgen_export_is_bit_reproducible_and_roundtrips() {
        let model = rankgen_model();
        let first = model.export_safetensors().expect("export");
        let second = model.export_safetensors().expect("export");
        assert_eq!(first.bytes(), second.bytes());
        let imported =
            RankGenProjectionHead::import_safetensors(first.bytes(), first.expectation())
                .expect("import");
        assert_eq!(
            imported.authority(),
            LearnedModelAuthority::ScoringEvidenceOnly
        );
        assert_eq!(imported.model(), &model);
    }

    #[test]
    fn reward_export_is_bit_reproducible_and_roundtrips() {
        let model = reward_model();
        let first = model.export_safetensors().expect("export");
        let second = model.export_safetensors().expect("export");
        assert_eq!(first.bytes(), second.bytes());
        let imported = FinalEmbeddingRewardEnsemble::<Literary>::import_safetensors(
            first.bytes(),
            first.expectation(),
        )
        .expect("import");
        assert_eq!(imported.model(), &model);
    }

    #[test]
    fn byte_tampering_fails_before_container_interpretation() {
        let artifact = rankgen_model().export_safetensors().expect("export");
        let mut tampered = artifact.bytes().to_vec();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_eq!(
            RankGenProjectionHead::import_safetensors(&tampered, artifact.expectation())
                .expect_err("tampering"),
            LearnedArtifactError::ArtifactFingerprintMismatch
        );
    }

    #[test]
    fn shape_tampering_is_rejected_even_with_a_new_outer_digest() {
        let model = rankgen_model();
        let artifact = model.export_safetensors().expect("export");
        let tensors = SafeTensors::deserialize(artifact.bytes()).expect("container");
        let (_, metadata) = SafeTensors::read_metadata(artifact.bytes()).expect("metadata");
        let prefix = tensors.tensor("prefix_projection").expect("prefix");
        let continuation = tensors
            .tensor("continuation_projection")
            .expect("continuation");
        let wrong_prefix =
            TensorView::new(Dtype::F32, vec![32, 4], prefix.data()).expect("wrong-shaped view");
        let views = BTreeMap::from([
            ("continuation_projection".to_owned(), continuation),
            ("prefix_projection".to_owned(), wrong_prefix),
        ]);
        let malicious =
            safetensors::serialize(views, metadata.metadata().clone()).expect("malicious artifact");
        let old = artifact.expectation();
        let expectation = LearnedArtifactExpectation::new(LearnedArtifactExpectationInput {
            artifact_fingerprint: BlobId::digest(&malicious),
            kind: LearnedArtifactKind::RankGenProjection,
            learned_model_fingerprint: old.learned_model_fingerprint,
            embedding_model_fingerprint: old.embedding_model_fingerprint,
            tokenizer_fingerprint: old.tokenizer_fingerprint,
            dataset_fingerprint: old.dataset_fingerprint,
            training_fingerprint: old.training_fingerprint,
        });
        assert_eq!(
            RankGenProjectionHead::import_safetensors(&malicious, expectation)
                .expect_err("shape tampering"),
            LearnedArtifactError::InvalidTensorShape
        );
    }

    #[test]
    fn dtype_tampering_is_rejected_even_with_a_new_outer_digest() {
        let model = rankgen_model();
        let artifact = model.export_safetensors().expect("export");
        let tensors = SafeTensors::deserialize(artifact.bytes()).expect("container");
        let (_, metadata) = SafeTensors::read_metadata(artifact.bytes()).expect("metadata");
        let prefix = tensors.tensor("prefix_projection").expect("prefix");
        let continuation = tensors
            .tensor("continuation_projection")
            .expect("continuation");
        let wrong_prefix = TensorView::new(Dtype::U32, prefix.shape().to_vec(), prefix.data())
            .expect("wrong-typed view");
        let views = BTreeMap::from([
            ("continuation_projection".to_owned(), continuation),
            ("prefix_projection".to_owned(), wrong_prefix),
        ]);
        let malicious =
            safetensors::serialize(views, metadata.metadata().clone()).expect("malicious artifact");
        let expectation = repin(&malicious, artifact.expectation());
        assert_eq!(
            RankGenProjectionHead::import_safetensors(&malicious, expectation)
                .expect_err("dtype tampering"),
            LearnedArtifactError::InvalidTensorDtype
        );
    }

    #[test]
    fn frozen_training_fingerprint_cannot_be_swapped() {
        let artifact = rankgen_model().export_safetensors().expect("export");
        let old = artifact.expectation();
        let wrong = LearnedArtifactExpectation::new(LearnedArtifactExpectationInput {
            artifact_fingerprint: old.artifact_fingerprint,
            kind: old.kind,
            learned_model_fingerprint: old.learned_model_fingerprint,
            embedding_model_fingerprint: old.embedding_model_fingerprint,
            tokenizer_fingerprint: old.tokenizer_fingerprint,
            dataset_fingerprint: old.dataset_fingerprint,
            training_fingerprint: BlobId::digest(b"different training"),
        });
        assert_eq!(
            RankGenProjectionHead::import_safetensors(artifact.bytes(), wrong)
                .expect_err("training swap"),
            LearnedArtifactError::PinnedMetadataMismatch
        );
    }

    #[test]
    fn reward_role_and_inner_tensors_are_bound_by_exact_identity() {
        let artifact = reward_model().export_safetensors().expect("export");
        assert_eq!(
            FinalEmbeddingRewardEnsemble::<crate::Causal>::import_safetensors(
                artifact.bytes(),
                artifact.expectation(),
            )
            .expect_err("role transplant"),
            LearnedArtifactError::InvalidMetadata
        );

        let weight_tamper = mutate_first_f32(artifact.bytes(), "head_0_weights", 0.25);
        assert_eq!(
            FinalEmbeddingRewardEnsemble::<Literary>::import_safetensors(
                &weight_tamper,
                repin(&weight_tamper, artifact.expectation()),
            )
            .expect_err("parameter tamper"),
            LearnedArtifactError::Reward
        );

        let temperature_tamper =
            mutate_first_f32(artifact.bytes(), "calibration_temperature", 0.25);
        assert_eq!(
            FinalEmbeddingRewardEnsemble::<Literary>::import_safetensors(
                &temperature_tamper,
                repin(&temperature_tamper, artifact.expectation()),
            )
            .expect_err("calibration tamper"),
            LearnedArtifactError::MetadataTensorMismatch
        );

        let ood_tamper = mutate_first_f32(artifact.bytes(), "ood_scale", 0.25);
        assert_eq!(
            FinalEmbeddingRewardEnsemble::<Literary>::import_safetensors(
                &ood_tamper,
                repin(&ood_tamper, artifact.expectation()),
            )
            .expect_err("OOD tamper"),
            LearnedArtifactError::MetadataTensorMismatch
        );
    }

    #[test]
    fn claimed_human_volume_cannot_be_promoted_in_rewritten_metadata() {
        let artifact = reward_model().export_safetensors().expect("export");
        let tensors = SafeTensors::deserialize(artifact.bytes()).expect("container");
        let mut manifest: RewardArtifactManifest =
            read_manifest(artifact.bytes()).expect("manifest");
        manifest.claimed_human_threshold = ClaimedHumanThreshold::DeclaredActiveShelfVolumeMet;
        let manifest = serde_json::to_string(&manifest).expect("manifest JSON");
        let metadata = HashMap::from([(METADATA_KEY.to_owned(), manifest)]);
        let views = tensors
            .names()
            .into_iter()
            .map(|name| {
                let tensor = tensors.tensor(name).expect("tensor");
                (name.to_owned(), tensor)
            })
            .collect::<BTreeMap<_, _>>();
        let malicious = safetensors::serialize(views, Some(metadata)).expect("rewritten artifact");
        assert_eq!(
            FinalEmbeddingRewardEnsemble::<Literary>::import_safetensors(
                &malicious,
                repin(&malicious, artifact.expectation()),
            )
            .expect_err("claimed-human promotion"),
            LearnedArtifactError::Reward
        );
    }

    fn mutate_first_f32(bytes: &[u8], tensor_name: &str, addition: f32) -> Vec<u8> {
        let tensors = SafeTensors::deserialize(bytes).expect("container");
        let (_, metadata) = SafeTensors::read_metadata(bytes).expect("metadata");
        let mut owned = tensors
            .names()
            .into_iter()
            .map(|name| {
                let tensor = tensors.tensor(name).expect("tensor");
                (
                    name.to_owned(),
                    tensor.dtype(),
                    tensor.shape().to_vec(),
                    tensor.data().to_vec(),
                )
            })
            .collect::<Vec<_>>();
        let target = owned
            .iter_mut()
            .find(|(name, _, _, _)| name == tensor_name)
            .expect("target tensor");
        let first = f32::from_le_bytes(target.3[..size_of::<f32>()].try_into().expect("one f32"));
        target.3[..size_of::<f32>()].copy_from_slice(&(first + addition).to_le_bytes());
        let views = owned
            .iter()
            .map(|(name, dtype, shape, data)| {
                let view = TensorView::new(*dtype, shape.clone(), data).expect("tensor view");
                (name.clone(), view)
            })
            .collect::<BTreeMap<_, _>>();
        safetensors::serialize(views, metadata.metadata().clone()).expect("tampered artifact")
    }

    fn repin(bytes: &[u8], old: LearnedArtifactExpectation) -> LearnedArtifactExpectation {
        LearnedArtifactExpectation::new(LearnedArtifactExpectationInput {
            artifact_fingerprint: BlobId::digest(bytes),
            kind: old.kind,
            learned_model_fingerprint: old.learned_model_fingerprint,
            embedding_model_fingerprint: old.embedding_model_fingerprint,
            tokenizer_fingerprint: old.tokenizer_fingerprint,
            dataset_fingerprint: old.dataset_fingerprint,
            training_fingerprint: old.training_fingerprint,
        })
    }
}
