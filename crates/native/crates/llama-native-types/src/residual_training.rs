//! Product-neutral residual direction fitting contracts. Public, mutable DTOs
//! must be validated at every execution boundary, including after deserialization.
//! Transport byte limits must be enforced before deserializing untrusted data.

use crate::{ModelFingerprint, NativeError, invalid_config, validate_id, validate_sha256};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const MAX_RESIDUAL_TRAINING_PAIRS: usize = 128;
pub const MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS: usize = 4_096;
pub const MAX_RESIDUAL_TRAINING_TOTAL_TOKENS: usize = 32_768;
pub const MAX_RESIDUAL_TRAINING_LAYERS: usize = 16;
pub const MAX_RESIDUAL_TRAINING_MODEL_LAYERS: u32 = 4_096;
pub const MAX_RESIDUAL_TRAINING_DIMENSIONS: usize = 65_536;
pub const MAX_RESIDUAL_CONTROL_VECTOR_VALUES: usize = 16_777_216;
pub const MAX_RESIDUAL_CAPTURE_VALUES: usize = 1_048_576;
pub const MAX_RESIDUAL_TRAINING_VOCABULARY: usize = 1_048_576;
pub const MAX_RESIDUAL_TRAINING_SEARCH_STEPS: u32 = 64;
pub const MAX_RESIDUAL_TRAINING_NORM: f32 = 100.0;
pub const MAX_RESIDUAL_TRAINING_PENALTY: f32 = 100.0;
pub const MAX_RESIDUAL_TRAINING_MARGIN: f32 = 100.0;
/// Conservative total token decodes across extraction, search, held-out scoring,
/// and the explicit zero-control check. This is not an elapsed-time guarantee.
pub const MAX_RESIDUAL_TRAINING_TOKEN_EVALUATIONS: u64 = 16_777_216;
pub const MAX_RESIDUAL_NO_OP_LOGPROB_DELTA: f64 = 1e-5;
/// Absolute loss improvement required before accepting a gain update. The
/// factor of four allows for the two log-probabilities in each signed margin
/// and noise in both candidate and incumbent evaluations. This is a
/// computational acceptance floor, not statistical confidence or a guarantee
/// that every runtime evaluation's numerical error is bounded by the no-op probe.
pub const MIN_RESIDUAL_LOSS_IMPROVEMENT: f64 = 4.0 * MAX_RESIDUAL_NO_OP_LOGPROB_DELTA;
/// Existing terminal-token V2 specification; not the span-mean method.
pub const RESIDUAL_TRAINING_METHOD: &str =
    "paired_mean_direction_bounded_coordinate_search_prefix_batches_max64_v2";
pub const RESIDUAL_SPAN_MEAN_TRAINING_METHOD: &str =
    "paired_response_span_mean_direction_bounded_coordinate_search_prefix_batches_max64_v3";
pub const RESIDUAL_MATCHED_TERMINAL_TRAINING_METHOD: &str =
    "paired_terminal_token_matched_direction_bounded_coordinate_search_prefix_batches_max64_v3";
pub const RESIDUAL_INTERVENTION_SEMANTICS: &str =
    "signed_post_block_residual_addition_from_last_prefix_token_v1";

/// Extraction only; likelihood scoring and gain optimization are unchanged.
/// Required on requests and receipts: legacy JSON is not silently upgraded.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResidualPooling {
    /// One post-block state after decoding the final pole token (V2 reference).
    TerminalToken,
    /// V3 ablation: capture/validate every pole singleton, retain only the last.
    /// Prefix prefill and all decode/capture calls match `ResponseSpanMean`.
    TerminalTokenMatched,
    /// Arithmetic mean of post-block states after each pole token, no prefix.
    ResponseSpanMean,
}

impl ResidualPooling {
    #[must_use]
    pub const fn training_method(self) -> &'static str {
        match self {
            Self::TerminalToken => RESIDUAL_TRAINING_METHOD,
            Self::TerminalTokenMatched => RESIDUAL_MATCHED_TERMINAL_TRAINING_METHOD,
            Self::ResponseSpanMean => RESIDUAL_SPAN_MEAN_TRAINING_METHOD,
        }
    }

    const fn hash_tag(self) -> u8 {
        match self {
            Self::TerminalToken => 0,
            Self::TerminalTokenMatched => 2,
            Self::ResponseSpanMean => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResidualTrainingPair {
    pub id: String,
    pub prefix: Vec<i32>,
    pub positive: Vec<i32>,
    pub negative: Vec<i32>,
}

impl ResidualTrainingPair {
    pub fn validate(&self) -> Result<(), NativeError> {
        validate_id("residual pair id", &self.id)?;
        for tokens in [&self.prefix, &self.positive, &self.negative] {
            if tokens.is_empty()
                || tokens.len() > MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS
                || tokens.iter().any(|&token| token < 0)
            {
                return Err(invalid_config(
                    "residual sequences must be nonempty, bounded, and contain nonnegative token IDs",
                ));
            }
        }
        if self.prefix.len() + self.positive.len() > MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS
            || self.prefix.len() + self.negative.len() > MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS
            || self.positive == self.negative
        {
            return Err(invalid_config(
                "residual poles must differ and each full sequence must fit the token cap",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResidualTrainingRequest {
    pub request_id: String,
    pub model_id: String,
    pub pooling: ResidualPooling,
    /// Zero-based post-block indices. Only `1..=n_layers - 2` is supported.
    /// Order is preserved and defines the coordinate-search order.
    pub layers: Vec<u32>,
    pub train: Vec<ResidualTrainingPair>,
    pub validation: Vec<ResidualTrainingPair>,
    /// Complete coordinate sweeps; zero explicitly requests the no-op baseline.
    pub search_steps: u32,
    pub initial_step: f32,
    /// L2 radius of the vector of per-layer scalar gains.
    pub maximum_norm: f32,
    pub l2_penalty: f32,
    pub margin: f32,
}

impl ResidualTrainingRequest {
    pub fn validate(&self) -> Result<(), NativeError> {
        validate_id("residual request_id", &self.request_id)?;
        validate_id("residual model_id", &self.model_id)?;
        validate_layers(&self.layers)?;
        validate_residual_search_parameters(
            self.search_steps,
            self.initial_step,
            self.maximum_norm,
        )?;
        validate_residual_objective_parameters(self.l2_penalty, self.margin)?;
        let mut ids = BTreeSet::new();
        let mut pair_content = BTreeSet::new();
        let mut training_sequences = BTreeSet::new();
        let mut total_tokens = 0;
        for (is_validation, pairs) in [(false, &self.train), (true, &self.validation)] {
            let minimum = if is_validation { 1 } else { 2 };
            if pairs.len() < minimum || pairs.len() > MAX_RESIDUAL_TRAINING_PAIRS {
                return Err(invalid_config(
                    "residual training needs 2..=128 pairs; validation needs 1..=128",
                ));
            }
            for pair in pairs {
                pair.validate()?;
                total_tokens += pair.prefix.len() + pair.positive.len() + pair.negative.len();
                if total_tokens > MAX_RESIDUAL_TRAINING_TOTAL_TOKENS {
                    return Err(invalid_config("residual training total token cap exceeded"));
                }
                if !ids.insert(pair.id.as_str()) {
                    return Err(invalid_config("residual pair IDs must be globally unique"));
                }
                let (first, second) = if pair.positive < pair.negative {
                    (&pair.positive, &pair.negative)
                } else {
                    (&pair.negative, &pair.positive)
                };
                if !pair_content.insert((&pair.prefix, first, second)) {
                    return Err(invalid_config(
                        "duplicate or pole-swapped residual pair content",
                    ));
                }
                for pole in [&pair.positive, &pair.negative] {
                    let sequence: Vec<i32> = pair.prefix.iter().chain(pole).copied().collect();
                    if is_validation {
                        if training_sequences.contains(&sequence) {
                            return Err(invalid_config(
                                "train and validation share a full token sequence",
                            ));
                        }
                    } else {
                        training_sequences.insert(sequence);
                    }
                }
            }
        }
        if self.token_evaluation_estimate() > MAX_RESIDUAL_TRAINING_TOKEN_EVALUATIONS {
            return Err(invalid_config(
                "residual training token evaluation budget exceeded",
            ));
        }
        Ok(())
    }

    /// Includes two signed evaluations per pole, two trials per coordinate,
    /// baseline, final held-out evaluation, direction extraction, and eight
    /// first-pair pole evaluations for no-control/zero-control/replay checking.
    /// Validation is never included in optimizer evaluations. Does not estimate
    /// architecture-dependent FLOPs or bound transport deserialization memory.
    pub fn estimated_token_evaluations(&self) -> Result<u64, NativeError> {
        self.validate()?;
        Ok(self.token_evaluation_estimate())
    }

    /// Live owner-side memory admission: dense cvec storage is at most 64 MiB
    /// and one selected-layer f32 capture is at most 4 MiB. Pole and pair means
    /// are accumulated incrementally in at most three selected-layer f64 buffers
    /// (8 MiB each), never all training-token captures. Context/backend RAM and
    /// the wrapper's own f32 snapshot storage are additional.
    pub fn validate_control_vector_shape(
        &self,
        n_layers: u32,
        width: usize,
    ) -> Result<(), NativeError> {
        self.validate()?;
        if !(3..=MAX_RESIDUAL_TRAINING_MODEL_LAYERS).contains(&n_layers)
            || self.layers.iter().any(|&layer| layer > n_layers - 2)
            || !(1..=MAX_RESIDUAL_TRAINING_DIMENSIONS).contains(&width)
            || width
                .checked_mul(n_layers.saturating_sub(1) as usize)
                .is_none_or(|n| n > MAX_RESIDUAL_CONTROL_VECTOR_VALUES)
            || width
                .checked_mul(self.layers.len())
                .is_none_or(|n| n > MAX_RESIDUAL_CAPTURE_VALUES)
        {
            return Err(invalid_config(
                "residual control vector or selected capture exceeds memory bounds",
            ));
        }
        Ok(())
    }

    fn token_evaluation_estimate(&self) -> u64 {
        let cost = |pair: &ResidualTrainingPair| {
            2 * pair.prefix.len() as u64 + pair.positive.len() as u64 + pair.negative.len() as u64
        };
        let train: u64 = self.train.iter().map(cost).sum();
        let validation: u64 = self.validation.iter().map(cost).sum();
        let evaluations = 1 + 2 * self.layers.len() as u64 * u64::from(self.search_steps);
        train + 2 * train * evaluations + 4 * validation + 4 * cost(&self.train[0])
    }

    /// Owner-thread admission must call this before extracting any hidden states.
    pub fn validate_for_model(
        &self,
        n_layers: u32,
        vocabulary_size: usize,
        context_tokens: usize,
    ) -> Result<(), NativeError> {
        self.validate()?;
        if !(3..=MAX_RESIDUAL_TRAINING_MODEL_LAYERS).contains(&n_layers)
            || !(1..=MAX_RESIDUAL_TRAINING_VOCABULARY).contains(&vocabulary_size)
            || context_tokens == 0
            || self.layers.iter().any(|&layer| layer > n_layers - 2)
        {
            return Err(invalid_config(
                "invalid residual model dimensions or unsupported post-block layer",
            ));
        }
        for pair in self.train.iter().chain(&self.validation) {
            if pair.prefix.len() + pair.positive.len() > context_tokens
                || pair.prefix.len() + pair.negative.len() > context_tokens
                || pair
                    .prefix
                    .iter()
                    .chain(&pair.positive)
                    .chain(&pair.negative)
                    .any(|&token| token as usize >= vocabulary_size)
            {
                return Err(invalid_config(
                    "residual token sequence exceeds model context or vocabulary",
                ));
            }
        }
        Ok(())
    }

    /// Canonical, domain-separated, length-delimited digest including partition
    /// membership, order, IDs, token IDs, and exact float bits. Not JSON-dependent.
    pub fn sha256(&self) -> Result<String, NativeError> {
        self.validate()?;
        let mut hash = Sha256::new();
        hash.update(b"llama-native.residual-training-request.v2\0");
        hash.update([self.pooling.hash_tag()]);
        hash_bytes(&mut hash, self.request_id.as_bytes());
        hash_bytes(&mut hash, self.model_id.as_bytes());
        hash.update((self.layers.len() as u64).to_le_bytes());
        for layer in &self.layers {
            hash.update(layer.to_le_bytes());
        }
        for pairs in [&self.train, &self.validation] {
            hash.update((pairs.len() as u64).to_le_bytes());
            for pair in pairs {
                hash_bytes(&mut hash, pair.id.as_bytes());
                for tokens in [&pair.prefix, &pair.positive, &pair.negative] {
                    hash.update((tokens.len() as u64).to_le_bytes());
                    for token in tokens {
                        hash.update(token.to_le_bytes());
                    }
                }
            }
        }
        hash.update(self.search_steps.to_le_bytes());
        for value in [
            self.initial_step,
            self.maximum_norm,
            self.l2_penalty,
            self.margin,
        ] {
            hash.update(value.to_bits().to_le_bytes());
        }
        Ok(format!("{:x}", hash.finalize()))
    }
}

fn hash_bytes(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResidualTrainingMetrics {
    /// Mean signed softplus margin loss plus `l2_penalty * sum(gain^2)`.
    pub loss: f64,
    /// Mean `logp(positive | +g) - logp(negative | +g)`.
    pub positive_margin: f64,
    /// Mean `logp(negative | -g) - logp(positive | -g)`.
    pub negative_margin: f64,
    /// Half the sum of the two mean margins.
    pub signed_utility: f64,
    /// Fraction of pairs for which BOTH signed margins are strictly positive.
    /// Ties do not count as agreement, including at the no-op baseline.
    pub polarity_agreement: f64,
    pub pair_count: u32,
}

impl ResidualTrainingMetrics {
    pub fn validate(&self) -> Result<(), NativeError> {
        if [
            self.loss,
            self.positive_margin,
            self.negative_margin,
            self.signed_utility,
            self.polarity_agreement,
        ]
        .iter()
        .any(|v| !v.is_finite())
            || self.loss < 0.0
            || !(0.0..=1.0).contains(&self.polarity_agreement)
            || self.pair_count == 0
            || self.pair_count as usize > MAX_RESIDUAL_TRAINING_PAIRS
        {
            return Err(invalid_config("invalid residual training metrics"));
        }
        let expected = self.positive_margin * 0.5 + self.negative_margin * 0.5;
        if (expected - self.signed_utility).abs() > 1e-10 * expected.abs().max(1.0) {
            return Err(invalid_config(
                "residual signed utility disagrees with margins",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResidualTrainingStep {
    /// Zero is the baseline; subsequent entries are completed sweeps.
    pub step: u32,
    pub gains: Vec<f32>,
    pub metrics: ResidualTrainingMetrics,
}

impl ResidualTrainingStep {
    pub fn validate(&self) -> Result<(), NativeError> {
        if self.step > MAX_RESIDUAL_TRAINING_SEARCH_STEPS {
            return Err(invalid_config("residual history step exceeds search cap"));
        }
        validate_gains(&self.gains, MAX_RESIDUAL_TRAINING_NORM)?;
        self.metrics.validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResidualTrainingOutput {
    pub request_sha256: String,
    /// Bound to both the request digest and the mode-specific method label.
    pub pooling: ResidualPooling,
    pub model_fingerprint: ModelFingerprint,
    /// Actual temporary execution context, distinct from resident model config.
    pub execution_fingerprint: ModelFingerprint,
    /// Maximum absolute per-target-token log-probability drift on the first
    /// training pair: no control, explicit zero twice, then cleared replay.
    /// A bounded diagnostic on those tokens, not full-vocabulary equivalence.
    pub no_op_max_logprob_delta: f64,
    pub training_method: String,
    pub intervention_semantics: String,
    pub layers: Vec<u32>,
    /// Unit axes from equal-weight paired differences of training-only pole
    /// representations under `pooling`, captured with control disabled.
    pub directions: Vec<Vec<f32>>,
    pub gains: Vec<f32>,
    pub history: Vec<ResidualTrainingStep>,
    pub train_baseline: ResidualTrainingMetrics,
    pub train_final: ResidualTrainingMetrics,
    pub validation_baseline: ResidualTrainingMetrics,
    pub validation_final: ResidualTrainingMetrics,
}

impl ResidualTrainingOutput {
    pub fn validate(&self) -> Result<(), NativeError> {
        validate_sha256("residual request_sha256", &self.request_sha256)?;
        for fingerprint in [&self.model_fingerprint, &self.execution_fingerprint] {
            for (name, value) in [
                ("binding_version", &fingerprint.binding_version),
                ("build_id", &fingerprint.build_id),
                ("backend", &fingerprint.backend),
            ] {
                validate_id(name, value)?;
            }
            for (name, value) in [
                ("chat_template_sha256", &fingerprint.chat_template_sha256),
                ("rope_config_sha256", &fingerprint.rope_config_sha256),
                ("kv_layout_sha256", &fingerprint.kv_layout_sha256),
            ] {
                validate_sha256(name, value)?;
            }
            if let Some(digest) = &fingerprint.multimodal_projector_sha256 {
                validate_sha256("multimodal_projector_sha256", digest)?;
            }
        }
        validate_id(
            "residual fingerprint model_id",
            &self.model_fingerprint.model_id,
        )?;
        validate_sha256(
            "residual model_sha256",
            &self.model_fingerprint.model_sha256,
        )?;
        validate_sha256(
            "residual tokenizer_sha256",
            &self.model_fingerprint.tokenizer_sha256,
        )?;
        validate_id(
            "residual execution model_id",
            &self.execution_fingerprint.model_id,
        )?;
        validate_sha256(
            "residual execution model_sha256",
            &self.execution_fingerprint.model_sha256,
        )?;
        validate_sha256(
            "residual execution tokenizer_sha256",
            &self.execution_fingerprint.tokenizer_sha256,
        )?;
        if self.execution_fingerprint.model_id != self.model_fingerprint.model_id
            || self.execution_fingerprint.model_size != self.model_fingerprint.model_size
            || self.execution_fingerprint.binding_version != self.model_fingerprint.binding_version
            || self.execution_fingerprint.build_id != self.model_fingerprint.build_id
            || self.execution_fingerprint.backend != self.model_fingerprint.backend
            || self.execution_fingerprint.chat_template_sha256
                != self.model_fingerprint.chat_template_sha256
            || self.execution_fingerprint.multimodal_projector_sha256
                != self.model_fingerprint.multimodal_projector_sha256
            || self.execution_fingerprint.model_sha256 != self.model_fingerprint.model_sha256
            || self.execution_fingerprint.tokenizer_sha256
                != self.model_fingerprint.tokenizer_sha256
            || !self.no_op_max_logprob_delta.is_finite()
            || !(0.0..=MAX_RESIDUAL_NO_OP_LOGPROB_DELTA).contains(&self.no_op_max_logprob_delta)
        {
            return Err(invalid_config(
                "residual execution identity or zero-control equivalence check failed",
            ));
        }
        if self.training_method != self.pooling.training_method()
            || self.intervention_semantics != RESIDUAL_INTERVENTION_SEMANTICS
        {
            return Err(invalid_config(
                "unsupported residual training method or intervention semantics",
            ));
        }
        validate_layers(&self.layers)?;
        validate_gains(&self.gains, MAX_RESIDUAL_TRAINING_NORM)?;
        if self.directions.len() != self.layers.len() || self.gains.len() != self.layers.len() {
            return Err(invalid_config(
                "residual layer, direction, and gain counts differ",
            ));
        }
        let dimensions = self.directions[0].len();
        for direction in &self.directions {
            if direction.is_empty()
                || direction.len() > MAX_RESIDUAL_TRAINING_DIMENSIONS
                || direction.len() != dimensions
                || direction.iter().any(|v| !v.is_finite())
            {
                return Err(invalid_config(
                    "invalid residual direction dimensions or values",
                ));
            }
            let squared_norm: f64 = direction.iter().map(|&v| f64::from(v).powi(2)).sum();
            if (squared_norm - 1.0).abs() > 1e-5 {
                return Err(invalid_config("residual directions must be unit axes"));
            }
        }
        for metrics in [
            self.train_baseline,
            self.train_final,
            self.validation_baseline,
            self.validation_final,
        ] {
            metrics.validate()?;
        }
        if self.train_baseline.pair_count < 2
            || self.train_baseline.pair_count != self.train_final.pair_count
            || self.validation_baseline.pair_count != self.validation_final.pair_count
            || self.history.is_empty()
            || self.history.len() > MAX_RESIDUAL_TRAINING_SEARCH_STEPS as usize + 1
        {
            return Err(invalid_config(
                "invalid residual history length or partition counts",
            ));
        }
        let mut previous_loss = self.train_baseline.loss;
        let mut previous_gains = vec![0.0; self.layers.len()];
        for (index, step) in self.history.iter().enumerate() {
            step.validate()?;
            if step.step as usize != index
                || step.gains.len() != self.layers.len()
                || step.metrics.pair_count != self.train_baseline.pair_count
                || step.metrics.loss > previous_loss
                || (step.metrics.loss == previous_loss && step.gains != previous_gains)
            {
                return Err(invalid_config(
                    "residual history is not a bounded monotone search from no-op",
                ));
            }
            previous_loss = step.metrics.loss;
            previous_gains.clone_from(&step.gains);
        }
        let first = &self.history[0];
        let last = &self.history[self.history.len() - 1];
        if first.gains.iter().any(|&v| v != 0.0)
            || first.metrics != self.train_baseline
            || last.gains != self.gains
            || last.metrics != self.train_final
        {
            return Err(invalid_config(
                "residual output disagrees with baseline or final history",
            ));
        }
        Ok(())
    }

    /// Bind the receipt to the exact request and its tighter gain/search limits.
    pub fn validate_for_request(
        &self,
        request: &ResidualTrainingRequest,
    ) -> Result<(), NativeError> {
        self.validate()?;
        if self.request_sha256 != request.sha256()?
            || self.pooling != request.pooling
            || self.layers != request.layers
            || self.model_fingerprint.model_id != request.model_id
            || self.train_baseline.pair_count as usize != request.train.len()
            || self.validation_baseline.pair_count as usize != request.validation.len()
            || self.history.len() != request.search_steps as usize + 1
        {
            return Err(invalid_config("residual output does not match its request"));
        }
        for step in &self.history {
            validate_gains(&step.gains, request.maximum_norm)?;
        }
        Ok(())
    }
}

fn validate_layers(layers: &[u32]) -> Result<(), NativeError> {
    if layers.is_empty()
        || layers.len() > MAX_RESIDUAL_TRAINING_LAYERS
        || layers
            .iter()
            .any(|&layer| layer == 0 || layer > MAX_RESIDUAL_TRAINING_MODEL_LAYERS - 2)
        || layers.iter().collect::<BTreeSet<_>>().len() != layers.len()
    {
        return Err(invalid_config(
            "residual layers must be nonempty, bounded, unique, interior post-block indices",
        ));
    }
    Ok(())
}

pub fn validate_residual_search_parameters(
    steps: u32,
    initial_step: f32,
    maximum_norm: f32,
) -> Result<(), NativeError> {
    if steps > MAX_RESIDUAL_TRAINING_SEARCH_STEPS
        || !initial_step.is_finite()
        || initial_step <= 0.0
        || initial_step > MAX_RESIDUAL_TRAINING_NORM
        || !maximum_norm.is_finite()
        || !(0.0..=MAX_RESIDUAL_TRAINING_NORM).contains(&maximum_norm)
    {
        return Err(invalid_config(
            "invalid residual search steps, initial step, or L2 radius",
        ));
    }
    Ok(())
}

pub fn validate_residual_objective_parameters(
    l2_penalty: f32,
    margin: f32,
) -> Result<(), NativeError> {
    if !l2_penalty.is_finite()
        || !(0.0..=MAX_RESIDUAL_TRAINING_PENALTY).contains(&l2_penalty)
        || !margin.is_finite()
        || !(0.0..=MAX_RESIDUAL_TRAINING_MARGIN).contains(&margin)
    {
        return Err(invalid_config("invalid residual L2 penalty or margin"));
    }
    Ok(())
}

fn validate_gains(gains: &[f32], maximum_norm: f32) -> Result<(), NativeError> {
    if gains.is_empty()
        || gains.len() > MAX_RESIDUAL_TRAINING_LAYERS
        || gains.iter().any(|v| !v.is_finite())
    {
        return Err(invalid_config("invalid residual gains"));
    }
    let norm = gains
        .iter()
        .map(|&v| f64::from(v).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm > f64::from(maximum_norm) {
        return Err(invalid_config("residual gains exceed L2 radius"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ResidualTrainingRequest {
        ResidualTrainingRequest {
            request_id: "request".into(),
            model_id: "model".into(),
            pooling: ResidualPooling::TerminalToken,
            layers: vec![1, 2],
            train: vec![
                ResidualTrainingPair {
                    id: "train".into(),
                    prefix: vec![1],
                    positive: vec![2, 3],
                    negative: vec![4],
                },
                ResidualTrainingPair {
                    id: "train-2".into(),
                    prefix: vec![2],
                    positive: vec![3],
                    negative: vec![4],
                },
            ],
            validation: vec![ResidualTrainingPair {
                id: "validation".into(),
                prefix: vec![5],
                positive: vec![6],
                negative: vec![7],
            }],
            search_steps: 2,
            initial_step: 0.5,
            maximum_norm: 1.0,
            l2_penalty: 0.01,
            margin: 1.0,
        }
    }

    #[test]
    fn partition_leakage_rejects_ids_aliases_swaps_and_resegmentation() {
        let original = request();
        original.validate().expect("valid request");
        let mut value = original.clone();
        value.validation[0].id = value.train[0].id.clone();
        assert!(value.validate().is_err());
        for swap in [false, true] {
            let mut value = original.clone();
            value.validation[0] = value.train[0].clone();
            value.validation[0].id = "alias".into();
            if swap {
                let pair = &mut value.validation[0];
                std::mem::swap(&mut pair.positive, &mut pair.negative);
            }
            assert!(value.validate().is_err());
        }
        let mut value = original.clone();
        value.validation[0].prefix = vec![1, 2];
        value.validation[0].positive = vec![3];
        assert!(
            value.validate().is_err(),
            "prefix boundary cannot hide a shared sequence"
        );
        let mut value = original;
        let mut alias = value.train[0].clone();
        alias.id = "alias".into();
        value.train.push(alias);
        assert!(value.validate().is_err());
    }

    #[test]
    fn finite_bounds_and_model_admission() {
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 101.0] {
            for field in 0..4 {
                let mut value = request();
                match field {
                    0 => value.initial_step = invalid,
                    1 => value.maximum_norm = invalid,
                    2 => value.l2_penalty = invalid,
                    _ => value.margin = invalid,
                }
                assert!(value.validate().is_err());
            }
        }
        let valid = request();
        valid
            .validate_for_model(4, 8, 3)
            .expect("boundary dimensions");
        for (layers, vocabulary, context) in [(3, 8, 3), (4, 7, 3), (4, 8, 2), (0, 0, 0)] {
            assert!(
                valid
                    .validate_for_model(layers, vocabulary, context)
                    .is_err()
            );
        }
        let mut value = request();
        value.layers = vec![1, 1];
        assert!(value.validate().is_err());
        value.layers = vec![0];
        assert!(value.validate().is_err());
        value = request();
        value.train[0].positive = value.train[0].negative.clone();
        assert!(value.validate().is_err());
        value = request();
        value.train[0].prefix.clear();
        assert!(value.validate().is_err());
        value = request();
        value.train[0].negative = vec![-1];
        assert!(value.validate().is_err());
        value = request();
        value.train[0].prefix = vec![1; MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS];
        assert!(value.validate().is_err());
        value = request();
        value.search_steps = MAX_RESIDUAL_TRAINING_SEARCH_STEPS + 1;
        assert!(value.validate().is_err());
        value = request();
        value.train = vec![value.train[0].clone(); MAX_RESIDUAL_TRAINING_PAIRS + 1];
        assert!(value.validate().is_err());
        value = request();
        value.search_steps = 0;
        value.maximum_norm = 0.0;
        value.validate().expect("explicit no-op allowed");
    }

    #[test]
    fn aggregate_token_cap_and_serde_digest() {
        let mut value = request();
        value.train = (0..9)
            .map(|index| ResidualTrainingPair {
                id: format!("pair-{index}"),
                prefix: vec![10 + index; 4_094],
                positive: vec![20],
                negative: vec![21],
            })
            .collect();
        assert!(value.validate().is_err());
        let original = request();
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: ResidualTrainingRequest = serde_json::from_str(&json).expect("deserialize");
        decoded.validate().expect("validate decoded DTO");
        assert_eq!(original, decoded);
        assert_eq!(
            original.sha256().expect("hash"),
            decoded.sha256().expect("hash")
        );
        let mut changed = original.clone();
        changed.margin += 0.1;
        assert_ne!(
            original.sha256().expect("hash"),
            changed.sha256().expect("hash")
        );
        changed = original.clone();
        changed.train.swap(0, 1);
        assert_ne!(
            original.sha256().expect("hash"),
            changed.sha256().expect("hash")
        );
        changed = original.clone();
        changed.train[0].positive.push(22);
        assert_ne!(
            original.sha256().expect("hash"),
            changed.sha256().expect("hash")
        );
    }

    #[test]
    fn pooling_is_required_and_bound_to_request_and_receipt() {
        let terminal = request();
        let mut span = terminal.clone();
        span.pooling = ResidualPooling::ResponseSpanMean;
        let mut matched = terminal.clone();
        matched.pooling = ResidualPooling::TerminalTokenMatched;
        let digests: BTreeSet<_> = [&terminal, &matched, &span]
            .into_iter()
            .map(|r| r.sha256().expect("hash"))
            .collect();
        assert_eq!(
            digests.len(),
            3,
            "all pooling modes must have distinct hashes"
        );
        assert_ne!(
            matched.pooling.training_method(),
            span.pooling.training_method()
        );
        assert_ne!(
            terminal.sha256().expect("terminal hash"),
            span.sha256().expect("span hash")
        );
        assert_eq!(
            terminal.estimated_token_evaluations().expect("cost"),
            span.estimated_token_evaluations().expect("cost")
        );
        for request in [&terminal, &matched, &span] {
            let encoded = serde_json::to_value(request).expect("encode");
            let roundtrip: ResidualTrainingRequest =
                serde_json::from_value(encoded.clone()).expect("explicit mode");
            assert_eq!(&roundtrip, request);
            let mut missing = encoded.clone();
            missing.as_object_mut().expect("object").remove("pooling");
            assert!(serde_json::from_value::<ResidualTrainingRequest>(missing).is_err());
            let mut unknown = encoded;
            unknown["pooling"] = serde_json::json!("all_sequence_mean");
            assert!(serde_json::from_value::<ResidualTrainingRequest>(unknown).is_err());
            let mut receipt = output();
            receipt.pooling = request.pooling;
            receipt.training_method = request.pooling.training_method().into();
            receipt.request_sha256 = request.sha256().expect("request hash");
            receipt
                .validate_for_request(request)
                .expect("matching mode receipt");
            for other in [&terminal, &matched, &span] {
                if other.pooling != request.pooling {
                    assert!(receipt.validate_for_request(other).is_err());
                }
            }
        }
        let original = output();
        let mut changed = original.clone();
        changed.pooling = span.pooling;
        assert!(
            changed.validate().is_err(),
            "terminal method cannot describe span pooling"
        );
        changed.training_method = span.pooling.training_method().into();
        assert!(
            changed.validate_for_request(&terminal).is_err(),
            "mode mismatch despite original request hash"
        );
        assert!(
            changed.validate_for_request(&span).is_err(),
            "request hash still bound to terminal mode"
        );
        changed.request_sha256 = span.sha256().expect("span hash");
        changed
            .validate_for_request(&span)
            .expect("consistent span receipt");
        let mut missing = serde_json::to_value(&original).expect("encode");
        missing.as_object_mut().expect("object").remove("pooling");
        assert!(serde_json::from_value::<ResidualTrainingOutput>(missing).is_err());
        let mut legacy_method = original;
        legacy_method.training_method = "paired_mean_direction_bounded_coordinate_search_v1".into();
        assert!(
            legacy_method.validate().is_err(),
            "V1 evidence is not current V2 execution"
        );
    }

    #[test]
    fn workload_and_live_width_are_bounded() {
        let mut value = request();
        value
            .validate_control_vector_shape(4, 4)
            .expect("small shape");
        assert!(value.validate_control_vector_shape(4, 0).is_err());
        assert!(value.validate_control_vector_shape(4, usize::MAX).is_err());
        assert!(value.validate_control_vector_shape(4_096, 65_536).is_err());
        assert!(value.validate_control_vector_shape(2, 4).is_err());
        let cost = value.estimated_token_evaluations().expect("cost");
        // Train cost = 5+4, holdout = 4, nine optimizer evaluations,
        // and four first-pair evaluations for the no-op/reset probe.
        assert_eq!(cost, 9 + 2 * 9 * 9 + 4 * 4 + 4 * 5);
        value.layers = (1..=16).collect();
        value.search_steps = 64;
        value.train[0].prefix = vec![1; 4_094];
        assert!(
            value.validate().is_err(),
            "multiplicative compute budget is capped"
        );
        value = request();
        value.train.pop();
        assert!(value.validate().is_err(), "at least two training pairs");
    }

    fn output() -> ResidualTrainingOutput {
        let request = request();
        let fingerprint = ModelFingerprint {
            model_id: "model".into(),
            model_size: 1,
            model_sha256: "a".repeat(64),
            tokenizer_sha256: "b".repeat(64),
            chat_template_sha256: "c".repeat(64),
            multimodal_projector_sha256: None,
            binding_version: "test".into(),
            build_id: "test".into(),
            backend: "fixture".into(),
            context_tokens: 8,
            batch_tokens: 1,
            max_sequences: 1,
            rope_config_sha256: "d".repeat(64),
            kv_layout_sha256: "e".repeat(64),
        };
        let train = ResidualTrainingMetrics {
            loss: 1.0,
            positive_margin: 0.0,
            negative_margin: 0.0,
            signed_utility: 0.0,
            polarity_agreement: 0.0,
            pair_count: 2,
        };
        let validation = ResidualTrainingMetrics {
            pair_count: 1,
            ..train
        };
        ResidualTrainingOutput {
            request_sha256: request.sha256().expect("hash"),
            pooling: request.pooling,
            model_fingerprint: fingerprint.clone(),
            execution_fingerprint: fingerprint,
            no_op_max_logprob_delta: 0.0,
            training_method: RESIDUAL_TRAINING_METHOD.into(),
            intervention_semantics: RESIDUAL_INTERVENTION_SEMANTICS.into(),
            layers: request.layers,
            directions: vec![vec![1.0, 0.0], vec![0.0, 1.0]],
            gains: vec![0.0, 0.0],
            history: (0..=request.search_steps)
                .map(|step| ResidualTrainingStep {
                    step,
                    gains: vec![0.0, 0.0],
                    metrics: train,
                })
                .collect(),
            train_baseline: train,
            train_final: train,
            validation_baseline: validation,
            validation_final: validation,
        }
    }

    #[test]
    fn output_receipt_roundtrip_and_validation() {
        let original = output();
        original
            .validate_for_request(&request())
            .expect("valid receipt");
        let decoded: ResidualTrainingOutput =
            serde_json::from_str(&serde_json::to_string(&original).expect("encode"))
                .expect("decode");
        decoded.validate().expect("validated decoded receipt");
        assert_eq!(original, decoded);
        for invalid in [f64::NAN, -1.0, 1e-4] {
            let mut value = original.clone();
            value.no_op_max_logprob_delta = invalid;
            assert!(value.validate().is_err());
        }
        let mut value = original.clone();
        value.execution_fingerprint.model_sha256 = "f".repeat(64);
        assert!(value.validate().is_err());
        value = original.clone();
        value.directions[0] = vec![0.0, 0.0];
        assert!(value.validate().is_err());
        value = original.clone();
        value.directions[0][0] = f32::NAN;
        assert!(value.validate().is_err());
        value = original.clone();
        value.history.clear();
        assert!(value.validate().is_err());
        value = original.clone();
        value.history.pop();
        assert!(
            value.validate_for_request(&request()).is_err(),
            "fixed sweep history cannot be truncated"
        );
        value = original.clone();
        value.execution_fingerprint.build_id = "x".repeat(257);
        assert!(value.validate().is_err());
        value = original.clone();
        value.history[0].step = 1;
        assert!(value.validate().is_err());
        value = original.clone();
        value.gains[0] = 1.0;
        assert!(value.validate().is_err());
        value = original.clone();
        value.train_final.signed_utility = 1.0;
        assert!(value.validate().is_err());
        value = original.clone();
        value.request_sha256 = "f".repeat(64);
        assert!(value.validate_for_request(&request()).is_err());
        value = original.clone();
        value.history.push(ResidualTrainingStep {
            step: 3,
            gains: vec![0.5, 0.0],
            metrics: original.train_baseline,
        });
        value.gains = vec![0.5, 0.0];
        assert!(
            value.validate().is_err(),
            "equal-loss history cannot move off no-op"
        );
    }
}
