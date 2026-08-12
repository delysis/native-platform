//! Pure, bounded mathematics for controlled decoding.
//!
//! This module deliberately does not own model contexts or sampler state. It
//! validates finite logit slices and vocabulary identities, performs one
//! deterministic transformation, and returns ordinary Rust values for a later
//! owner-thread integration layer to consume.

use llama_native_types::{MAX_ABS_LOGIT_BIAS, MAX_TOP_N_SIGMA};
use std::error::Error;
use std::fmt::{Display, Formatter};

/// Largest vocabulary accepted by the control-math boundary.
pub const MAX_CONTROL_VOCAB_SIZE: usize = 1_048_576;
/// Largest fingerprint accepted by [`VocabularyIdentity`].
pub const MAX_FINGERPRINT_BYTES: usize = 256;
/// Largest sparse bias program accepted by [`apply_sparse_logit_bias`].
pub const MAX_SPARSE_BIASES: usize = 4_096;

const MAX_ABS_LOGIT: f32 = 1_000_000.0;
const MAX_GUIDANCE_SCALE: f32 = 100.0;
const MIN_POWER: f32 = f32::MIN_POSITIVE;
const MAX_POWER: f32 = 100.0;
const MIN_TEMPERATURE: f32 = 0.01;
const MAX_TEMPERATURE: f32 = 100.0;

/// A caller-visible failure at the controlled-decoding math boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlMathError {
    EmptyInput,
    InputTooLarge {
        len: usize,
        max: usize,
    },
    EmptyIdentity {
        field: &'static str,
    },
    IdentityTooLarge {
        field: &'static str,
        len: usize,
        max: usize,
    },
    InvalidVocabularySize {
        size: usize,
        max: usize,
    },
    DimensionMismatch {
        expected: usize,
        actual: usize,
    },
    TokenizerMismatch,
    VocabularyMismatch,
    NonFiniteLogit {
        index: usize,
    },
    LogitOutOfRange {
        index: usize,
        value: f32,
        max_abs: f32,
    },
    NonFiniteParameter {
        name: &'static str,
    },
    ParameterOutOfRange {
        name: &'static str,
        value: f32,
        min: f32,
        max: f32,
    },
    EmptySparseBias,
    TooManySparseBiases {
        len: usize,
        max: usize,
    },
    TokenOutOfRange {
        token_id: usize,
        vocabulary_size: usize,
    },
    DuplicateTokenBias {
        token_id: usize,
    },
    ArithmeticOverflow {
        index: usize,
    },
    DegenerateGuidedVariance,
}

impl Display for ControlMathError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyInput => formatter.write_str("logit input must not be empty"),
            Self::InputTooLarge { len, max } => {
                write!(formatter, "logit input length {len} exceeds limit {max}")
            }
            Self::EmptyIdentity { field } => write!(formatter, "{field} must not be empty"),
            Self::IdentityTooLarge {
                field,
                len,
                max,
            } => write!(formatter, "{field} length {len} exceeds limit {max}"),
            Self::InvalidVocabularySize { size, max } => {
                write!(formatter, "vocabulary size {size} is outside 1..={max}")
            }
            Self::DimensionMismatch { expected, actual } => write!(
                formatter,
                "logit dimension mismatch: expected {expected}, received {actual}"
            ),
            Self::TokenizerMismatch => {
                formatter.write_str("logit inputs use different tokenizer fingerprints")
            }
            Self::VocabularyMismatch => {
                formatter.write_str("logit inputs use different vocabulary fingerprints")
            }
            Self::NonFiniteLogit { index } => {
                write!(formatter, "logit at index {index} is not finite")
            }
            Self::LogitOutOfRange {
                index,
                value,
                max_abs,
            } => write!(
                formatter,
                "logit at index {index} has magnitude {value}, exceeding {max_abs}"
            ),
            Self::NonFiniteParameter { name } => {
                write!(formatter, "parameter {name} must be finite")
            }
            Self::ParameterOutOfRange {
                name,
                value,
                min,
                max,
            } => write!(
                formatter,
                "parameter {name}={value} is outside [{min}, {max}]"
            ),
            Self::EmptySparseBias => formatter.write_str("sparse logit bias must not be empty"),
            Self::TooManySparseBiases { len, max } => {
                write!(formatter, "sparse logit bias length {len} exceeds limit {max}")
            }
            Self::TokenOutOfRange {
                token_id,
                vocabulary_size,
            } => write!(
                formatter,
                "token {token_id} is outside vocabulary size {vocabulary_size}"
            ),
            Self::DuplicateTokenBias { token_id } => {
                write!(formatter, "token {token_id} has more than one sparse bias")
            }
            Self::ArithmeticOverflow { index } => {
                write!(formatter, "control arithmetic overflowed at index {index}")
            }
            Self::DegenerateGuidedVariance => formatter.write_str(
                "CFG rescaling cannot match a varying conditional distribution to constant guided logits",
            ),
        }
    }
}

impl Error for ControlMathError {}

/// Exact tokenizer and vocabulary identity required for cross-model arithmetic.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VocabularyIdentity {
    tokenizer_fingerprint: String,
    vocabulary_fingerprint: String,
    vocabulary_size: usize,
}

impl VocabularyIdentity {
    pub fn new(
        tokenizer_fingerprint: impl Into<String>,
        vocabulary_fingerprint: impl Into<String>,
        vocabulary_size: usize,
    ) -> Result<Self, ControlMathError> {
        let tokenizer_fingerprint = tokenizer_fingerprint.into();
        let vocabulary_fingerprint = vocabulary_fingerprint.into();
        validate_identity("tokenizer_fingerprint", &tokenizer_fingerprint)?;
        validate_identity("vocabulary_fingerprint", &vocabulary_fingerprint)?;
        if !(1..=MAX_CONTROL_VOCAB_SIZE).contains(&vocabulary_size) {
            return Err(ControlMathError::InvalidVocabularySize {
                size: vocabulary_size,
                max: MAX_CONTROL_VOCAB_SIZE,
            });
        }
        Ok(Self {
            tokenizer_fingerprint,
            vocabulary_fingerprint,
            vocabulary_size,
        })
    }

    pub fn tokenizer_fingerprint(&self) -> &str {
        &self.tokenizer_fingerprint
    }

    pub fn vocabulary_fingerprint(&self) -> &str {
        &self.vocabulary_fingerprint
    }

    pub fn vocabulary_size(&self) -> usize {
        self.vocabulary_size
    }
}

/// A finite, bounded logit slice tied to an exact vocabulary identity.
#[derive(Debug, Clone, Copy)]
pub struct ValidatedLogits<'a> {
    values: &'a [f32],
    vocabulary: &'a VocabularyIdentity,
}

impl<'a> ValidatedLogits<'a> {
    pub fn new(
        values: &'a [f32],
        vocabulary: &'a VocabularyIdentity,
    ) -> Result<Self, ControlMathError> {
        validate_logits(values)?;
        if values.len() != vocabulary.vocabulary_size {
            return Err(ControlMathError::DimensionMismatch {
                expected: vocabulary.vocabulary_size,
                actual: values.len(),
            });
        }
        Ok(Self { values, vocabulary })
    }

    pub fn values(self) -> &'a [f32] {
        self.values
    }

    pub fn vocabulary(self) -> &'a VocabularyIdentity {
        self.vocabulary
    }
}

/// A candidate mask kept separate from finite logits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateMask {
    allowed: Vec<bool>,
    retained: usize,
}

impl CandidateMask {
    pub fn as_slice(&self) -> &[bool] {
        &self.allowed
    }

    pub fn retained(&self) -> usize {
        self.retained
    }

    pub fn allows(&self, token_id: usize) -> bool {
        self.allowed.get(token_id).copied().unwrap_or(false)
    }

    fn from_allowed(allowed: Vec<bool>) -> Self {
        let retained = allowed.iter().filter(|allowed| **allowed).count();
        debug_assert!(retained > 0);
        Self { allowed, retained }
    }
}

/// Finite contrastive scores plus the expert plausibility mask.
#[derive(Debug, Clone, PartialEq)]
pub struct ContrastiveScores {
    scores: Vec<f32>,
    mask: CandidateMask,
}

impl ContrastiveScores {
    pub fn scores(&self) -> &[f32] {
        &self.scores
    }

    pub fn mask(&self) -> &CandidateMask {
        &self.mask
    }
}

/// Parameterization of the same power-family logit transform in two forms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PowerTemperature {
    /// Raise unnormalized token weights to this power.
    Power(f32),
    /// Divide logits by this temperature.
    Temperature(f32),
}

/// Combine conditional and unconditional logits as `u + scale * (c - u)`.
///
/// `rescale_mix`, when present, linearly blends the guided logits with a
/// distribution-equivalent, mean-preserving version whose standard deviation
/// matches the conditional logits. A value of zero disables rescaling and one
/// applies it fully.
pub fn classifier_free_guidance(
    conditional: ValidatedLogits<'_>,
    unconditional: ValidatedLogits<'_>,
    scale: f32,
    rescale_mix: Option<f32>,
) -> Result<Vec<f32>, ControlMathError> {
    validate_compatible(conditional, unconditional)?;
    validate_parameter("scale", scale, 0.0, MAX_GUIDANCE_SCALE)?;
    if let Some(mix) = rescale_mix {
        validate_parameter("rescale_mix", mix, 0.0, 1.0)?;
    }

    let mut guided = conditional
        .values
        .iter()
        .zip(unconditional.values)
        .enumerate()
        .map(|(index, (&conditional, &unconditional))| {
            finite_f32(
                f64::from(unconditional)
                    + f64::from(scale) * (f64::from(conditional) - f64::from(unconditional)),
                index,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mix = rescale_mix.unwrap_or(0.0);
    if mix == 0.0 {
        return Ok(guided);
    }

    let (_, conditional_std) = mean_and_population_std(conditional.values);
    let (guided_mean, guided_std) = mean_and_population_std(&guided);
    if guided_std == 0.0 {
        if conditional_std == 0.0 {
            return Ok(guided);
        }
        return Err(ControlMathError::DegenerateGuidedVariance);
    }
    let variance_ratio = conditional_std / guided_std;
    for (index, value) in guided.iter_mut().enumerate() {
        let raw = f64::from(*value);
        let matched = guided_mean + (raw - guided_mean) * variance_ratio;
        *value = finite_f32(
            (1.0 - f64::from(mix)) * raw + f64::from(mix) * matched,
            index,
        )?;
    }
    Ok(guided)
}

/// Apply expert/amateur contrastive decoding with an expert plausibility head.
///
/// Scores are `expert + weight * (expert - amateur)`. A token remains plausible
/// when its expert probability is at least `plausibility_ratio` times the
/// expert's most likely token probability. A zero ratio retains every token.
pub fn contrastive_expert_amateur(
    expert: ValidatedLogits<'_>,
    amateur: ValidatedLogits<'_>,
    plausibility_ratio: f32,
    amateur_weight: f32,
) -> Result<ContrastiveScores, ControlMathError> {
    validate_compatible(expert, amateur)?;
    validate_parameter("plausibility_ratio", plausibility_ratio, 0.0, 1.0)?;
    validate_parameter("amateur_weight", amateur_weight, 0.0, MAX_GUIDANCE_SCALE)?;

    let scores = expert
        .values
        .iter()
        .zip(amateur.values)
        .enumerate()
        .map(|(index, (&expert, &amateur))| {
            finite_f32(
                f64::from(expert)
                    + f64::from(amateur_weight) * (f64::from(expert) - f64::from(amateur)),
                index,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    let max_expert = expert
        .values
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let allowed = if plausibility_ratio == 0.0 {
        vec![true; expert.values.len()]
    } else {
        let threshold = f64::from(max_expert) + f64::from(plausibility_ratio).ln();
        expert
            .values
            .iter()
            .map(|value| f64::from(*value) >= threshold)
            .collect()
    };
    Ok(ContrastiveScores {
        scores,
        mask: CandidateMask::from_allowed(allowed),
    })
}

/// Apply DExperts/GenARM-style `base + strength * (expert - anti_expert)`.
pub fn linear_expert_arithmetic(
    base: ValidatedLogits<'_>,
    expert: ValidatedLogits<'_>,
    anti_expert: ValidatedLogits<'_>,
    strength: f32,
) -> Result<Vec<f32>, ControlMathError> {
    validate_compatible(base, expert)?;
    validate_compatible(base, anti_expert)?;
    validate_parameter("strength", strength, 0.0, MAX_GUIDANCE_SCALE)?;
    base.values
        .iter()
        .zip(expert.values)
        .zip(anti_expert.values)
        .enumerate()
        .map(|(index, ((&base, &expert), &anti_expert))| {
            finite_f32(
                f64::from(base)
                    + f64::from(strength) * (f64::from(expert) - f64::from(anti_expert)),
                index,
            )
        })
        .collect()
}

/// Apply a bounded sparse bias program without mutating the source logits.
pub fn apply_sparse_logit_bias(
    logits: ValidatedLogits<'_>,
    biases: &[(usize, f32)],
) -> Result<Vec<f32>, ControlMathError> {
    if biases.is_empty() {
        return Err(ControlMathError::EmptySparseBias);
    }
    if biases.len() > MAX_SPARSE_BIASES || biases.len() > logits.values.len() {
        return Err(ControlMathError::TooManySparseBiases {
            len: biases.len(),
            max: MAX_SPARSE_BIASES.min(logits.values.len()),
        });
    }
    let mut seen = vec![false; logits.values.len()];
    let mut output = logits.values.to_vec();
    for &(token_id, bias) in biases {
        if token_id >= output.len() {
            return Err(ControlMathError::TokenOutOfRange {
                token_id,
                vocabulary_size: output.len(),
            });
        }
        if seen[token_id] {
            return Err(ControlMathError::DuplicateTokenBias { token_id });
        }
        seen[token_id] = true;
        validate_parameter("bias", bias, -MAX_ABS_LOGIT_BIAS, MAX_ABS_LOGIT_BIAS)?;
        output[token_id] = finite_f32(f64::from(output[token_id]) + f64::from(bias), token_id)?;
    }
    Ok(output)
}

/// Apply power sampling or its equivalent temperature parameterization.
pub fn power_temperature_transform(
    logits: ValidatedLogits<'_>,
    transform: PowerTemperature,
) -> Result<Vec<f32>, ControlMathError> {
    let power = match transform {
        PowerTemperature::Power(power) => {
            validate_parameter("power", power, MIN_POWER, MAX_POWER)?;
            power
        }
        PowerTemperature::Temperature(temperature) => {
            validate_parameter("temperature", temperature, MIN_TEMPERATURE, MAX_TEMPERATURE)?;
            1.0 / temperature
        }
    };
    logits
        .values
        .iter()
        .enumerate()
        .map(|(index, value)| finite_f32(f64::from(*value) * f64::from(power), index))
        .collect()
}

/// Build the entropy-dependent eta-sampling mask from arXiv:2210.15191.
///
/// For normalized probabilities `p` with entropy `H(p)`, the cutoff is
/// `min(eta, sqrt(eta) * exp(-H(p)))`. Tokens whose probability is equal to or
/// above the cutoff are retained, matching the authors' reference
/// implementation, which removes only probabilities strictly below it.
pub fn eta_cutoff_mask(
    logits: ValidatedLogits<'_>,
    eta: f32,
) -> Result<CandidateMask, ControlMathError> {
    validate_parameter("eta", eta, 0.0, 1.0)?;
    if eta == 0.0 {
        return Ok(CandidateMask::from_allowed(vec![true; logits.values.len()]));
    }

    let max = logits
        .values
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let weights = logits
        .values
        .iter()
        .map(|value| (f64::from(*value) - f64::from(max)).exp())
        .collect::<Vec<_>>();
    let weight_sum = weights.iter().sum::<f64>();
    let probabilities = weights
        .iter()
        .map(|weight| weight / weight_sum)
        .collect::<Vec<_>>();
    let entropy = probabilities
        .iter()
        .filter(|probability| **probability > 0.0)
        .map(|probability| -probability * probability.ln())
        .sum::<f64>();
    let eta = f64::from(eta);
    let threshold = eta.min(eta.sqrt() * (-entropy).exp());
    let mut allowed = probabilities
        .iter()
        .map(|probability| *probability >= threshold)
        .collect::<Vec<_>>();
    ensure_argmax_retained(&mut allowed, logits.values);
    Ok(CandidateMask::from_allowed(allowed))
}

/// Keep logits within `n` population standard deviations of the maximum.
///
/// This is the top-n-sigma rule from arXiv:2411.07641:
/// `logit >= max(logits) - n * sigma(logits)`.
pub fn top_n_sigma_mask(
    logits: ValidatedLogits<'_>,
    n: f32,
) -> Result<CandidateMask, ControlMathError> {
    validate_parameter("n", n, 0.0, MAX_TOP_N_SIGMA)?;
    let (_, standard_deviation) = mean_and_population_std(logits.values);
    let max = logits
        .values
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let threshold = f64::from(max) - f64::from(n) * standard_deviation;
    let mut allowed = logits
        .values
        .iter()
        .map(|value| f64::from(*value) >= threshold)
        .collect::<Vec<_>>();
    ensure_argmax_retained(&mut allowed, logits.values);
    Ok(CandidateMask::from_allowed(allowed))
}

fn validate_identity(field: &'static str, value: &str) -> Result<(), ControlMathError> {
    if value.trim().is_empty() {
        return Err(ControlMathError::EmptyIdentity { field });
    }
    if value.len() > MAX_FINGERPRINT_BYTES {
        return Err(ControlMathError::IdentityTooLarge {
            field,
            len: value.len(),
            max: MAX_FINGERPRINT_BYTES,
        });
    }
    Ok(())
}

fn validate_logits(values: &[f32]) -> Result<(), ControlMathError> {
    if values.is_empty() {
        return Err(ControlMathError::EmptyInput);
    }
    if values.len() > MAX_CONTROL_VOCAB_SIZE {
        return Err(ControlMathError::InputTooLarge {
            len: values.len(),
            max: MAX_CONTROL_VOCAB_SIZE,
        });
    }
    for (index, value) in values.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(ControlMathError::NonFiniteLogit { index });
        }
        if value.abs() > MAX_ABS_LOGIT {
            return Err(ControlMathError::LogitOutOfRange {
                index,
                value,
                max_abs: MAX_ABS_LOGIT,
            });
        }
    }
    Ok(())
}

fn validate_parameter(
    name: &'static str,
    value: f32,
    min: f32,
    max: f32,
) -> Result<(), ControlMathError> {
    if !value.is_finite() {
        return Err(ControlMathError::NonFiniteParameter { name });
    }
    if value < min || value > max {
        return Err(ControlMathError::ParameterOutOfRange {
            name,
            value,
            min,
            max,
        });
    }
    Ok(())
}

fn validate_compatible(
    left: ValidatedLogits<'_>,
    right: ValidatedLogits<'_>,
) -> Result<(), ControlMathError> {
    if left.values.len() != right.values.len() {
        return Err(ControlMathError::DimensionMismatch {
            expected: left.values.len(),
            actual: right.values.len(),
        });
    }
    if left.vocabulary.tokenizer_fingerprint != right.vocabulary.tokenizer_fingerprint {
        return Err(ControlMathError::TokenizerMismatch);
    }
    if left.vocabulary.vocabulary_fingerprint != right.vocabulary.vocabulary_fingerprint
        || left.vocabulary.vocabulary_size != right.vocabulary.vocabulary_size
    {
        return Err(ControlMathError::VocabularyMismatch);
    }
    Ok(())
}

fn finite_f32(value: f64, index: usize) -> Result<f32, ControlMathError> {
    if !value.is_finite() || value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
        return Err(ControlMathError::ArithmeticOverflow { index });
    }
    let value = value as f32;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ControlMathError::ArithmeticOverflow { index })
    }
}

fn mean_and_population_std(values: &[f32]) -> (f64, f64) {
    let count = values.len() as f64;
    let mean = values.iter().map(|value| f64::from(*value)).sum::<f64>() / count;
    let variance = values
        .iter()
        .map(|value| {
            let centered = f64::from(*value) - mean;
            centered * centered
        })
        .sum::<f64>()
        / count;
    (mean, variance.sqrt())
}

fn ensure_argmax_retained(allowed: &mut [bool], logits: &[f32]) {
    if allowed.iter().any(|allowed| *allowed) {
        return;
    }
    if let Some((index, _)) = logits
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.total_cmp(right))
    {
        allowed[index] = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(size: usize) -> VocabularyIdentity {
        VocabularyIdentity::new("tokenizer-a", "vocabulary-a", size)
            .expect("test identity must be valid")
    }

    fn logits<'a>(values: &'a [f32], identity: &'a VocabularyIdentity) -> ValidatedLogits<'a> {
        ValidatedLogits::new(values, identity).expect("test logits must be valid")
    }

    fn assert_close(left: &[f32], right: &[f32]) {
        assert_eq!(left.len(), right.len());
        for (index, (left, right)) in left.iter().zip(right).enumerate() {
            assert!(
                (left - right).abs() <= 1.0e-5,
                "index {index}: {left} != {right}"
            );
        }
    }

    #[test]
    fn cfg_matches_hand_computation_and_limits() {
        let vocabulary = identity(2);
        let conditional_values = [1.0, 3.0];
        let unconditional_values = [0.0, 0.0];
        let conditional = logits(&conditional_values, &vocabulary);
        let unconditional = logits(&unconditional_values, &vocabulary);

        assert_eq!(
            classifier_free_guidance(conditional, unconditional, 0.0, None)
                .expect("scale zero must work"),
            unconditional_values
        );
        assert_eq!(
            classifier_free_guidance(conditional, unconditional, 1.0, None)
                .expect("scale one must work"),
            conditional_values
        );
        assert_eq!(
            classifier_free_guidance(conditional, unconditional, 2.0, None)
                .expect("guided arithmetic must work"),
            [2.0, 6.0]
        );
        assert_eq!(
            classifier_free_guidance(conditional, unconditional, 2.0, Some(1.0))
                .expect("full CFG rescale must work"),
            [3.0, 5.0]
        );
    }

    #[test]
    fn cfg_is_permutation_equivariant() {
        let vocabulary = identity(3);
        let conditional = [3.0, 1.0, 2.0];
        let unconditional = [0.0, -1.0, -2.0];
        let permuted_conditional = [2.0, 3.0, 1.0];
        let permuted_unconditional = [-2.0, 0.0, -1.0];
        let original = classifier_free_guidance(
            logits(&conditional, &vocabulary),
            logits(&unconditional, &vocabulary),
            1.5,
            Some(0.25),
        )
        .expect("original CFG must work");
        let permuted = classifier_free_guidance(
            logits(&permuted_conditional, &vocabulary),
            logits(&permuted_unconditional, &vocabulary),
            1.5,
            Some(0.25),
        )
        .expect("permuted CFG must work");
        assert_close(&permuted, &[original[2], original[0], original[1]]);
    }

    #[test]
    fn cfg_rejects_impossible_variance_rescale() {
        let vocabulary = identity(2);
        let conditional = [1.0, 3.0];
        let unconditional = [-1.0, -3.0];
        assert!(matches!(
            classifier_free_guidance(
                logits(&conditional, &vocabulary),
                logits(&unconditional, &vocabulary),
                0.5,
                Some(1.0),
            ),
            Err(ControlMathError::DegenerateGuidedVariance)
        ));
    }

    #[test]
    fn contrastive_scores_and_plausibility_are_hand_computed() {
        let vocabulary = identity(3);
        let expert_values = [2.0, 1.0, 0.0];
        let amateur_values = [1.0, 2.0, 0.0];
        let result = contrastive_expert_amateur(
            logits(&expert_values, &vocabulary),
            logits(&amateur_values, &vocabulary),
            0.5,
            1.0,
        )
        .expect("contrastive arithmetic must work");
        assert_eq!(result.scores(), &[3.0, 0.0, 0.0]);
        assert_eq!(result.mask().as_slice(), &[true, false, false]);

        let all = contrastive_expert_amateur(
            logits(&expert_values, &vocabulary),
            logits(&amateur_values, &vocabulary),
            0.0,
            0.0,
        )
        .expect("zero limits must work");
        assert_eq!(all.scores(), expert_values);
        assert_eq!(all.mask().retained(), 3);
    }

    #[test]
    fn contrastive_arithmetic_is_normalization_shift_invariant() {
        let vocabulary = identity(3);
        let expert = [2.0, 1.0, 0.0];
        let amateur = [0.5, 1.0, -1.0];
        let shifted_expert = [102.0, 101.0, 100.0];
        let shifted_amateur = [-49.5, -49.0, -51.0];
        let original = contrastive_expert_amateur(
            logits(&expert, &vocabulary),
            logits(&amateur, &vocabulary),
            0.25,
            1.5,
        )
        .expect("original contrastive arithmetic must work");
        let shifted = contrastive_expert_amateur(
            logits(&shifted_expert, &vocabulary),
            logits(&shifted_amateur, &vocabulary),
            0.25,
            1.5,
        )
        .expect("shifted contrastive arithmetic must work");
        assert_eq!(original.mask(), shifted.mask());
        let original_differences = [
            0.0,
            original.scores()[1] - original.scores()[0],
            original.scores()[2] - original.scores()[0],
        ];
        let shifted_differences = [
            0.0,
            shifted.scores()[1] - shifted.scores()[0],
            shifted.scores()[2] - shifted.scores()[0],
        ];
        assert_close(&original_differences, &shifted_differences);
    }

    #[test]
    fn linear_expert_arithmetic_matches_dexperts_formula() {
        let vocabulary = identity(2);
        let base_values = [1.0, 2.0];
        let expert_values = [3.0, 0.0];
        let anti_values = [1.0, 1.0];
        let output = linear_expert_arithmetic(
            logits(&base_values, &vocabulary),
            logits(&expert_values, &vocabulary),
            logits(&anti_values, &vocabulary),
            0.5,
        )
        .expect("linear expert arithmetic must work");
        assert_eq!(output, [2.0, 1.5]);
        assert_eq!(
            linear_expert_arithmetic(
                logits(&base_values, &vocabulary),
                logits(&expert_values, &vocabulary),
                logits(&anti_values, &vocabulary),
                0.0,
            )
            .expect("zero strength must work"),
            base_values
        );
    }

    #[test]
    fn sparse_bias_is_order_independent_and_rejects_duplicates() {
        let vocabulary = identity(3);
        let values = [1.0, 2.0, 3.0];
        let input = logits(&values, &vocabulary);
        assert_eq!(
            apply_sparse_logit_bias(input, &[(0, 0.5), (2, -1.0)]).expect("sparse bias must work"),
            [1.5, 2.0, 2.0]
        );
        assert_eq!(
            apply_sparse_logit_bias(input, &[(2, -1.0), (0, 0.5)])
                .expect("bias order must not matter"),
            [1.5, 2.0, 2.0]
        );
        assert!(matches!(
            apply_sparse_logit_bias(input, &[(0, 0.5), (0, 1.0)]),
            Err(ControlMathError::DuplicateTokenBias { token_id: 0 })
        ));
    }

    #[test]
    fn power_and_temperature_are_equivalent() {
        let vocabulary = identity(3);
        let values = [-2.0, 0.0, 3.0];
        let input = logits(&values, &vocabulary);
        let power = power_temperature_transform(input, PowerTemperature::Power(2.0))
            .expect("power transform must work");
        let temperature = power_temperature_transform(input, PowerTemperature::Temperature(0.5))
            .expect("temperature transform must work");
        assert_eq!(power, [-4.0, 0.0, 6.0]);
        assert_eq!(power, temperature);
        assert_eq!(
            power_temperature_transform(input, PowerTemperature::Power(1.0))
                .expect("unit power must work"),
            values
        );
    }

    #[test]
    fn eta_cutoff_matches_hand_computed_distributions() {
        let uniform_identity = identity(4);
        let uniform = [0.0; 4];
        let uniform_mask = eta_cutoff_mask(logits(&uniform, &uniform_identity), 0.5)
            .expect("uniform eta cutoff must work");
        assert_eq!(uniform_mask.as_slice(), &[true, true, true, true]);

        let peaked_identity = identity(3);
        let peaked = [0.8_f32.ln(), 0.1_f32.ln(), 0.1_f32.ln()];
        let peaked_mask = eta_cutoff_mask(logits(&peaked, &peaked_identity), 0.2)
            .expect("peaked eta cutoff must work");
        assert_eq!(peaked_mask.as_slice(), &[true, false, false]);
        assert_eq!(
            eta_cutoff_mask(logits(&peaked, &peaked_identity), 0.0)
                .expect("zero eta must retain all")
                .retained(),
            3
        );

        let shifted = [
            peaked[0] + 1_000.0,
            peaked[1] + 1_000.0,
            peaked[2] + 1_000.0,
        ];
        assert_eq!(
            eta_cutoff_mask(logits(&shifted, &peaked_identity), 0.2)
                .expect("eta cutoff must be normalization-shift invariant"),
            peaked_mask
        );
    }

    #[test]
    fn top_n_sigma_matches_formula_and_is_affine_invariant() {
        let vocabulary = identity(3);
        let values = [0.0, 1.0, 2.0];
        let mask =
            top_n_sigma_mask(logits(&values, &vocabulary), 1.0).expect("top-n-sigma must work");
        assert_eq!(mask.as_slice(), &[false, false, true]);
        assert_eq!(
            top_n_sigma_mask(logits(&values, &vocabulary), 0.0)
                .expect("zero sigma width must work")
                .as_slice(),
            &[false, false, true]
        );

        let affine = [10.0, 12.0, 14.0];
        assert_eq!(
            top_n_sigma_mask(logits(&affine, &vocabulary), 1.0)
                .expect("positive affine transform must work")
                .as_slice(),
            mask.as_slice()
        );
        let permuted = [2.0, 0.0, 1.0];
        assert_eq!(
            top_n_sigma_mask(logits(&permuted, &vocabulary), 1.0)
                .expect("permutation must work")
                .as_slice(),
            &[true, false, false]
        );
        assert_eq!(
            top_n_sigma_mask(logits(&values, &vocabulary), MAX_TOP_N_SIGMA)
                .expect("maximum sigma width must work")
                .retained(),
            3
        );
    }

    #[test]
    fn validation_rejects_empty_nonfinite_unbounded_and_bad_parameters() {
        let one = identity(1);
        assert!(matches!(
            ValidatedLogits::new(&[], &one),
            Err(ControlMathError::EmptyInput)
        ));
        assert!(matches!(
            ValidatedLogits::new(&[f32::NAN], &one),
            Err(ControlMathError::NonFiniteLogit { index: 0 })
        ));
        assert!(matches!(
            ValidatedLogits::new(&[f32::INFINITY], &one),
            Err(ControlMathError::NonFiniteLogit { index: 0 })
        ));
        assert!(matches!(
            VocabularyIdentity::new("tokenizer", "vocabulary", MAX_CONTROL_VOCAB_SIZE + 1),
            Err(ControlMathError::InvalidVocabularySize { .. })
        ));
        let oversized = vec![0.0; MAX_CONTROL_VOCAB_SIZE + 1];
        assert!(matches!(
            ValidatedLogits::new(&oversized, &one),
            Err(ControlMathError::InputTooLarge { .. })
        ));
        let values = [0.0];
        let input = logits(&values, &one);
        assert!(matches!(
            power_temperature_transform(input, PowerTemperature::Power(f32::NAN)),
            Err(ControlMathError::NonFiniteParameter { name: "power" })
        ));
        assert!(matches!(
            eta_cutoff_mask(input, -0.1),
            Err(ControlMathError::ParameterOutOfRange { name: "eta", .. })
        ));
        assert!(matches!(
            apply_sparse_logit_bias(input, &[]),
            Err(ControlMathError::EmptySparseBias)
        ));
    }

    #[test]
    fn every_parameter_boundary_rejects_nonfinite_values() {
        let vocabulary = identity(2);
        let values = [0.0, 1.0];
        let input = logits(&values, &vocabulary);
        assert!(matches!(
            classifier_free_guidance(input, input, f32::NAN, None),
            Err(ControlMathError::NonFiniteParameter { name: "scale" })
        ));
        assert!(matches!(
            classifier_free_guidance(input, input, 1.0, Some(f32::INFINITY)),
            Err(ControlMathError::NonFiniteParameter {
                name: "rescale_mix"
            })
        ));
        assert!(matches!(
            contrastive_expert_amateur(input, input, f32::NAN, 1.0),
            Err(ControlMathError::NonFiniteParameter {
                name: "plausibility_ratio"
            })
        ));
        assert!(matches!(
            contrastive_expert_amateur(input, input, 0.5, f32::NEG_INFINITY),
            Err(ControlMathError::NonFiniteParameter {
                name: "amateur_weight"
            })
        ));
        assert!(matches!(
            linear_expert_arithmetic(input, input, input, f32::NAN),
            Err(ControlMathError::NonFiniteParameter { name: "strength" })
        ));
        assert!(matches!(
            apply_sparse_logit_bias(input, &[(0, f32::INFINITY)]),
            Err(ControlMathError::NonFiniteParameter { name: "bias" })
        ));
        assert!(matches!(
            power_temperature_transform(input, PowerTemperature::Temperature(f32::NAN)),
            Err(ControlMathError::NonFiniteParameter {
                name: "temperature"
            })
        ));
        assert!(matches!(
            eta_cutoff_mask(input, f32::INFINITY),
            Err(ControlMathError::NonFiniteParameter { name: "eta" })
        ));
        assert!(matches!(
            top_n_sigma_mask(input, f32::NAN),
            Err(ControlMathError::NonFiniteParameter { name: "n" })
        ));
    }

    #[test]
    fn maximum_contract_values_remain_finite() {
        let vocabulary = identity(2);
        let positive = [MAX_ABS_LOGIT, -MAX_ABS_LOGIT];
        let negative = [-MAX_ABS_LOGIT, MAX_ABS_LOGIT];
        let positive = logits(&positive, &vocabulary);
        let negative = logits(&negative, &vocabulary);

        let cfg = classifier_free_guidance(positive, negative, MAX_GUIDANCE_SCALE, None)
            .expect("maximum bounded CFG must remain finite");
        assert!(cfg.iter().all(|value| value.is_finite()));

        let linear = linear_expert_arithmetic(positive, positive, negative, MAX_GUIDANCE_SCALE)
            .expect("maximum bounded linear arithmetic must remain finite");
        assert!(linear.iter().all(|value| value.is_finite()));

        let power = power_temperature_transform(positive, PowerTemperature::Power(MAX_POWER))
            .expect("maximum bounded power must remain finite");
        assert!(power.iter().all(|value| value.is_finite()));

        let biased = apply_sparse_logit_bias(positive, &[(0, MAX_ABS_LOGIT_BIAS)])
            .expect("maximum public sparse bias must remain valid");
        assert!(biased.iter().all(|value| value.is_finite()));

        assert_eq!(
            eta_cutoff_mask(positive, 1.0)
                .expect("extreme finite logits must have a stable eta mask")
                .retained(),
            1
        );
    }

    #[test]
    fn cross_model_arithmetic_rejects_dimension_tokenizer_and_vocabulary_mismatch() {
        let left_identity = identity(2);
        let short_identity = identity(1);
        let other_tokenizer =
            VocabularyIdentity::new("tokenizer-b", "vocabulary-a", 2).expect("valid identity");
        let other_vocabulary =
            VocabularyIdentity::new("tokenizer-a", "vocabulary-b", 2).expect("valid identity");
        let left_values = [1.0, 2.0];
        let short_values = [1.0];
        let left = logits(&left_values, &left_identity);

        assert!(matches!(
            classifier_free_guidance(left, logits(&short_values, &short_identity), 1.0, None),
            Err(ControlMathError::DimensionMismatch {
                expected: 2,
                actual: 1
            })
        ));
        assert!(matches!(
            classifier_free_guidance(left, logits(&left_values, &other_tokenizer), 1.0, None),
            Err(ControlMathError::TokenizerMismatch)
        ));
        assert!(matches!(
            classifier_free_guidance(left, logits(&left_values, &other_vocabulary), 1.0, None),
            Err(ControlMathError::VocabularyMismatch)
        ));
    }
}
