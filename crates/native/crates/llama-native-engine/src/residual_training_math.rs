//! Pure residual-axis fitting and bounded derivative-free scalar gain search.
//! No model weights are updated; this is not autograd or full-model training.
//! The owner supplies explicitly pooled hidden states from training poles
//! and teacher-forced mean token log-probabilities. Validation data must never
//! enter direction fitting or the search evaluator.

use llama_native_types::{
    MAX_RESIDUAL_CAPTURE_VALUES, MAX_RESIDUAL_TRAINING_DIMENSIONS, MAX_RESIDUAL_TRAINING_LAYERS,
    MAX_RESIDUAL_TRAINING_NORM, MAX_RESIDUAL_TRAINING_PAIRS, MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS,
    MAX_RESIDUAL_TRAINING_VOCABULARY, MIN_RESIDUAL_LOSS_IMPROVEMENT, NativeError, NativeErrorCode,
    ResidualPooling, ResidualTrainingMetrics, ResidualTrainingStep,
    validate_residual_objective_parameters, validate_residual_search_parameters,
};

/// Relative numerical guard for large losses, used in addition to the public
/// absolute computational acceptance floor [`MIN_RESIDUAL_LOSS_IMPROVEMENT`].
pub const RESIDUAL_IMPROVEMENT_TOLERANCE: f64 = 1e-12;

fn invalid(message: &str) -> NativeError {
    NativeError::new(NativeErrorCode::InvalidConfig, message)
}

/// One bounded pole accumulator, independent of pole length in retained memory.
/// All captures share shape/finiteness/count validation. Mean mode sums raw f32
/// states in f64 then divides by token count; terminal modes retain only the last
/// state. Unit normalization happens only after equal-weight pair averaging.
pub(super) struct PoleAccumulator {
    sums: Vec<Vec<f64>>,
    expected_tokens: usize,
    tokens: usize,
    pooling: ResidualPooling,
}

impl PoleAccumulator {
    pub(super) fn new(
        layers: usize,
        width: usize,
        expected_tokens: usize,
        pooling: ResidualPooling,
    ) -> Result<Self, NativeError> {
        if !(1..=MAX_RESIDUAL_TRAINING_LAYERS).contains(&layers)
            || !(1..=MAX_RESIDUAL_TRAINING_DIMENSIONS).contains(&width)
            || !(1..=MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS).contains(&expected_tokens)
            || (pooling == ResidualPooling::TerminalToken && expected_tokens != 1)
            || width
                .checked_mul(layers)
                .is_none_or(|n| n > MAX_RESIDUAL_CAPTURE_VALUES)
        {
            return Err(invalid(
                "invalid residual pole mean dimensions or token count",
            ));
        }
        Ok(Self {
            sums: vec![vec![0.0; width]; layers],
            expected_tokens,
            tokens: 0,
            pooling,
        })
    }

    pub(super) fn push(&mut self, states: &[Vec<f32>]) -> Result<(), NativeError> {
        if self.tokens >= self.expected_tokens
            || states.len() != self.sums.len()
            || states.iter().zip(&self.sums).any(|(state, sum)| {
                state.len() != sum.len() || state.iter().any(|v| !v.is_finite())
            })
        {
            return Err(invalid(
                "invalid residual pole capture shape, values, or count",
            ));
        }
        for (sum, state) in self.sums.iter_mut().zip(states) {
            for (sum, &value) in sum.iter_mut().zip(state) {
                match self.pooling {
                    ResidualPooling::ResponseSpanMean => *sum += f64::from(value),
                    ResidualPooling::TerminalToken | ResidualPooling::TerminalTokenMatched => {
                        *sum = f64::from(value)
                    }
                }
            }
        }
        // At most 4096 finite f32 terms cannot overflow f64.
        self.tokens += 1;
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<Vec<Vec<f64>>, NativeError> {
        if self.tokens != self.expected_tokens {
            return Err(invalid("incomplete residual pole captures"));
        }
        if self.pooling == ResidualPooling::ResponseSpanMean {
            for value in self.sums.iter_mut().flatten() {
                *value /= self.tokens as f64;
            }
        }
        Ok(self.sums)
    }
}

/// Stable raw-model selected-token log-softmax. The expected vocabulary must
/// come from the loaded model; token IDs are checked before any indexing.
/// Uses f64 even for subtraction so opposite finite f32 extremes remain valid.
pub fn selected_token_log_probability(
    logits: &[f32],
    token: i32,
    vocabulary_size: usize,
) -> Result<f64, NativeError> {
    if !(1..=MAX_RESIDUAL_TRAINING_VOCABULARY).contains(&vocabulary_size)
        || logits.len() != vocabulary_size
        || token < 0
        || token as usize >= vocabulary_size
        || logits.iter().any(|value| !value.is_finite())
    {
        return Err(invalid(
            "invalid residual logits, vocabulary, or selected token",
        ));
    }
    let maximum = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let maximum = f64::from(maximum);
    let exponential_sum: f64 = logits.iter().map(|&v| (f64::from(v) - maximum).exp()).sum();
    Ok((f64::from(logits[token as usize]) - maximum) - exponential_sum.ln())
}

/// Unit axis from equal-weight paired positive-minus-negative final residual
/// states for ONE layer. Supply training rows only, in corresponding order.
/// Differences and accumulation use f64; poles are not normalized separately.
#[cfg(test)]
pub fn mean_direction(
    positive: &[Vec<f32>],
    negative: &[Vec<f32>],
) -> Result<Vec<f32>, NativeError> {
    if positive.is_empty()
        || positive.len() > MAX_RESIDUAL_TRAINING_PAIRS
        || positive.len() != negative.len()
    {
        return Err(invalid("invalid residual hidden-state pair count"));
    }
    let width = positive[0].len();
    if width == 0 || width > MAX_RESIDUAL_TRAINING_DIMENSIONS {
        return Err(invalid("invalid residual hidden-state width"));
    }
    let mut mean = vec![0.0_f64; width];
    for (positive, negative) in positive.iter().zip(negative) {
        if positive.len() != width
            || negative.len() != width
            || positive.iter().chain(negative).any(|v| !v.is_finite())
        {
            return Err(invalid(
                "residual hidden states must be finite and equally sized",
            ));
        }
        for ((sum, &positive), &negative) in mean.iter_mut().zip(positive).zip(negative) {
            *sum += f64::from(positive) - f64::from(negative);
        }
    }
    for value in &mut mean {
        *value /= positive.len() as f64;
    }
    normalize_direction(&mean)
}

/// Scale before squaring, handling both tiny and extreme finite f64 inputs.
pub fn normalize_direction(values: &[f64]) -> Result<Vec<f32>, NativeError> {
    if values.is_empty()
        || values.len() > MAX_RESIDUAL_TRAINING_DIMENSIONS
        || values.iter().any(|v| !v.is_finite())
    {
        return Err(invalid("invalid residual direction values or width"));
    }
    let scale = values.iter().map(|v| v.abs()).fold(0.0, f64::max);
    if scale == 0.0 {
        return Err(invalid("zero-norm residual direction"));
    }
    let scaled_norm = values
        .iter()
        .map(|v| (v / scale).powi(2))
        .sum::<f64>()
        .sqrt();
    Ok(values
        .iter()
        .map(|v| ((v / scale) / scaled_norm) as f32)
        .collect())
}

pub fn softplus(value: f64) -> Result<f64, NativeError> {
    if !value.is_finite() {
        return Err(invalid("softplus input must be finite"));
    }
    Ok(value.max(0.0) + (-value.abs()).exp().ln_1p())
}

/// Rows contain mean continuation-token log-probabilities in the fixed order
/// `[positive_under_plus, negative_under_plus, positive_under_minus,
/// negative_under_minus]`. Prefix tokens are excluded from each mean.
/// Each pole and each pair receive equal weight, regardless of token length.
///
/// Per-pair margins are `m+ = row[0]-row[1]`, `m- = row[3]-row[2]`.
/// Loss is `mean((softplus(margin-m+) + softplus(margin-m-))/2)
/// + l2_penalty * ||gains||^2`. Agreement requires BOTH margins to be > 0.
pub fn metrics_from_log_probs(
    rows: &[[f64; 4]],
    gains: &[f32],
    l2_penalty: f32,
    margin: f32,
) -> Result<ResidualTrainingMetrics, NativeError> {
    validate_residual_objective_parameters(l2_penalty, margin)?;
    let norm_squared = gain_norm_squared(gains)?;
    if norm_squared > f64::from(MAX_RESIDUAL_TRAINING_NORM).powi(2)
        || rows.is_empty()
        || rows.len() > MAX_RESIDUAL_TRAINING_PAIRS
        || rows.iter().flatten().any(|v| !v.is_finite() || *v > 0.0)
    {
        return Err(invalid(
            "invalid residual log-probability rows or gain norm",
        ));
    }
    let mut positive_margin = 0.0;
    let mut negative_margin = 0.0;
    let mut loss = 0.0;
    let mut agreements = 0_u32;
    // Divide before accumulating to avoid overflowing sums of finite means.
    let count = rows.len() as f64;
    for row in rows {
        let positive = row[0] - row[1];
        let negative = row[3] - row[2];
        positive_margin += positive / count;
        negative_margin += negative / count;
        loss += (softplus(f64::from(margin) - positive)? * 0.5
            + softplus(f64::from(margin) - negative)? * 0.5)
            / count;
        agreements += u32::from(positive > 0.0 && negative > 0.0);
    }
    let metrics = ResidualTrainingMetrics {
        loss: loss + f64::from(l2_penalty) * norm_squared,
        positive_margin,
        negative_margin,
        signed_utility: positive_margin * 0.5 + negative_margin * 0.5,
        polarity_agreement: f64::from(agreements) / count,
        pair_count: rows.len() as u32,
    };
    metrics.validate()?;
    Ok(metrics)
}

fn gain_norm_squared(gains: &[f32]) -> Result<f64, NativeError> {
    if gains.is_empty()
        || gains.len() > MAX_RESIDUAL_TRAINING_LAYERS
        || gains.iter().any(|v| !v.is_finite())
    {
        return Err(invalid(
            "residual gains must be finite, nonempty, and bounded in count",
        ));
    }
    Ok(gains.iter().map(|&v| f64::from(v).powi(2)).sum())
}

/// Project the entire per-layer gain vector, not individual coordinates.
/// Unit axes in distinct blocks mean this is the whole stacked-vector L2 bound.
/// A rounding correction keeps the returned f32 vector inside the f64 radius.
pub fn project_gains(gains: &mut [f32], maximum_norm: f32) -> Result<(), NativeError> {
    let projected = projected_gains(gains, maximum_norm)?;
    gains.copy_from_slice(&projected);
    Ok(())
}

fn projected_gains(gains: &[f32], maximum_norm: f32) -> Result<Vec<f32>, NativeError> {
    if !maximum_norm.is_finite() || !(0.0..=MAX_RESIDUAL_TRAINING_NORM).contains(&maximum_norm) {
        return Err(invalid("invalid residual gain projection radius"));
    }
    let norm = gain_norm_squared(gains)?.sqrt();
    if maximum_norm == 0.0 {
        return Ok(vec![0.0; gains.len()]);
    }
    if norm <= f64::from(maximum_norm) {
        return Ok(gains.to_vec());
    }
    let scale = f64::from(maximum_norm) / norm;
    let mut projected: Vec<f32> = gains
        .iter()
        .map(|&v| (f64::from(v) * scale) as f32)
        .collect();
    if gain_norm_squared(&projected)? > f64::from(maximum_norm).powi(2) {
        // One inward representable step per component also handles subnormals.
        for value in &mut projected {
            if *value != 0.0 {
                *value = f32::from_bits(value.to_bits() - 1);
            }
        }
    }
    if gain_norm_squared(&projected)? > f64::from(maximum_norm).powi(2) {
        return Err(invalid("residual gain projection could not satisfy radius"));
    }
    Ok(projected)
}

/// Accept only finite, strictly lower loss with improvement exceeding both the
/// no-op-drift-derived absolute floor and the relative numerical guard. This
/// computational rule does not establish statistical confidence.
pub fn strictly_improves(candidate: f64, incumbent: f64) -> bool {
    let threshold = MIN_RESIDUAL_LOSS_IMPROVEMENT
        .max(RESIDUAL_IMPROVEMENT_TOLERANCE * incumbent.abs().max(1.0));
    candidate.is_finite()
        && incumbent.is_finite()
        && candidate < incumbent
        && incumbent - candidate > threshold
}

/// Fixed-count deterministic coordinate sweeps, from a zero-gain baseline.
/// The callback must evaluate TRAINING ONLY with identical teacher forcing and
/// objective parameters on every invocation. It may propagate cancellation.
/// Only `metrics.loss` is consulted for selection; held-out scoring is external.
///
/// For each coordinate, trial -step then +step relative to its current value;
/// both are projected and compared with strict improvement. Equal candidates
/// preserve the first winner. Halve step only after a sweep with no improvement.
/// At most `1 + 2 * layer_count * search_steps` callback invocations are made.
/// The result has exactly `search_steps + 1` entries, including no-op baseline.
pub fn coordinate_search(
    layer_count: usize,
    search_steps: u32,
    initial_step: f32,
    maximum_norm: f32,
    mut evaluate: impl FnMut(&[f32]) -> Result<ResidualTrainingMetrics, NativeError>,
) -> Result<Vec<ResidualTrainingStep>, NativeError> {
    validate_residual_search_parameters(search_steps, initial_step, maximum_norm)?;
    if layer_count == 0 || layer_count > MAX_RESIDUAL_TRAINING_LAYERS {
        return Err(invalid("invalid residual coordinate count"));
    }
    let mut gains = vec![0.0; layer_count];
    let mut metrics = evaluate(&gains)?;
    metrics.validate()?;
    let pair_count = metrics.pair_count;
    let mut history = Vec::with_capacity(search_steps as usize + 1);
    history.push(ResidualTrainingStep {
        step: 0,
        gains: gains.clone(),
        metrics,
    });
    let mut step_size = initial_step;
    for step in 1..=search_steps {
        let mut improved = false;
        for coordinate in 0..layer_count {
            let origin = gains.clone();
            for sign in [-1.0_f32, 1.0] {
                let mut candidate = origin.clone();
                candidate[coordinate] += sign * step_size;
                project_gains(&mut candidate, maximum_norm)?;
                if candidate == gains {
                    continue;
                }
                let candidate_metrics = evaluate(&candidate)?;
                candidate_metrics.validate()?;
                if candidate_metrics.pair_count != pair_count {
                    return Err(invalid(
                        "residual search evaluator changed training pair count",
                    ));
                }
                if strictly_improves(candidate_metrics.loss, metrics.loss) {
                    gains = candidate;
                    metrics = candidate_metrics;
                    improved = true;
                }
            }
        }
        if !improved {
            step_size *= 0.5;
        }
        history.push(ResidualTrainingStep {
            step,
            gains: gains.clone(),
            metrics,
        });
    }
    Ok(history)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}");
    }

    fn metric(loss: f64) -> ResidualTrainingMetrics {
        ResidualTrainingMetrics {
            loss,
            positive_margin: 0.0,
            negative_margin: 0.0,
            signed_utility: 0.0,
            polarity_agreement: 0.0,
            pair_count: 2,
        }
    }

    #[test]
    fn pole_mean_is_raw_f64_arithmetic_not_per_token_normalization() {
        let mut mean =
            PoleAccumulator::new(2, 2, 3, ResidualPooling::ResponseSpanMean).expect("shape");
        for states in [
            vec![vec![16_777_216.0, 6.0], vec![3.0, 0.0]],
            vec![vec![1.0, 0.0], vec![0.0, 6.0]],
            vec![vec![-16_777_216.0, 0.0], vec![0.0, 0.0]],
        ] {
            mean.push(&states).expect("capture");
        }
        let mean = mean.finish().expect("complete");
        assert_eq!(mean, [vec![1.0 / 3.0, 2.0], vec![1.0, 2.0]]);
        let state = vec![vec![f32::MAX, -f32::MAX, f32::MIN_POSITIVE]];
        let mut one =
            PoleAccumulator::new(1, 3, 1, ResidualPooling::ResponseSpanMean).expect("single");
        one.push(&state).expect("finite extremes");
        assert_eq!(
            one.finish().expect("mean")[0],
            state[0].iter().copied().map(f64::from).collect::<Vec<_>>()
        );
        let mut many = PoleAccumulator::new(
            1,
            1,
            MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS,
            ResidualPooling::ResponseSpanMean,
        )
        .expect("bounded");
        for _ in 0..MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS {
            many.push(&[vec![f32::MAX]]).expect("capture");
        }
        assert_eq!(
            many.finish().expect("finite sum"),
            [vec![f64::from(f32::MAX)]]
        );
    }

    #[test]
    fn pole_mean_rejects_missing_extra_nonfinite_or_misshapen_states() {
        for (layers, width, count) in [
            (0, 1, 1),
            (1, 0, 1),
            (17, 1, 1),
            (1, 1, 0),
            (1, 1, 4097),
            (1, usize::MAX, 1),
        ] {
            assert!(
                PoleAccumulator::new(layers, width, count, ResidualPooling::ResponseSpanMean)
                    .is_err()
            );
        }
        assert!(
            PoleAccumulator::new(1, 1, 1, ResidualPooling::ResponseSpanMean)
                .expect("valid")
                .finish()
                .is_err()
        );
        let mut mean =
            PoleAccumulator::new(1, 2, 1, ResidualPooling::ResponseSpanMean).expect("shape");
        for states in [
            vec![],
            vec![vec![1.0]],
            vec![vec![1.0, 2.0]; 2],
            vec![vec![f32::NAN, 1.0]],
            vec![vec![1.0, f32::INFINITY]],
        ] {
            assert!(mean.push(&states).is_err());
        }
        mean.push(&[vec![2.0, 4.0]])
            .expect("bad states did not change mean");
        assert!(mean.push(&[vec![2.0, 4.0]]).is_err());
        assert_eq!(mean.finish().expect("one token"), [vec![2.0, 4.0]]);
    }

    #[test]
    fn matched_terminal_validates_discarded_states_and_retains_only_last() {
        let mut accumulator =
            PoleAccumulator::new(1, 2, 2, ResidualPooling::TerminalTokenMatched).expect("shape");
        assert!(accumulator.push(&[vec![f32::NAN, 1.0]]).is_err());
        assert!(accumulator.push(&[vec![1.0]]).is_err());
        accumulator
            .push(&[vec![f32::MAX, -f32::MAX]])
            .expect("first");
        accumulator.push(&[vec![3.0, 4.0]]).expect("last");
        assert_eq!(accumulator.finish().expect("terminal"), [vec![3.0, 4.0]]);
        assert!(PoleAccumulator::new(1, 1, 2, ResidualPooling::TerminalToken).is_err());
        let mut incomplete =
            PoleAccumulator::new(1, 1, 2, ResidualPooling::TerminalTokenMatched).expect("shape");
        incomplete.push(&[vec![1.0]]).expect("first");
        assert!(incomplete.finish().is_err());
    }

    #[test]
    fn selected_log_softmax_is_stable_and_checks_vocabulary() {
        close(
            selected_token_log_probability(&[1.0, 1.0], 0, 2).expect("valid"),
            -2.0_f64.ln(),
        );
        close(
            selected_token_log_probability(&[10_000.0, 9_999.0], 1, 2).expect("valid"),
            -1.0 - (-1.0_f64).exp().ln_1p(),
        );
        let extremes =
            selected_token_log_probability(&[-f32::MAX, f32::MAX], 0, 2).expect("finite extremes");
        assert!(extremes.is_finite());
        assert_eq!(extremes, -2.0 * f64::from(f32::MAX));
        for (values, token, size) in [
            (vec![], 0, 0),
            (vec![0.0], -1, 1),
            (vec![0.0], 1, 1),
            (vec![0.0], 0, 2),
            (vec![f32::NAN], 0, 1),
            (vec![f32::INFINITY], 0, 1),
            (vec![f32::NEG_INFINITY], 0, 1),
        ] {
            assert!(selected_token_log_probability(&values, token, size).is_err());
        }
    }

    #[test]
    fn mean_axis_uses_paired_training_differences_and_rejects_zero() {
        let axis = mean_direction(
            &[vec![3.0, 2.0], vec![1.0, 4.0]],
            &[vec![1.0, 1.0], vec![1.0, 1.0]],
        )
        .expect("axis");
        assert!((axis[0] - 1.0 / 5.0_f32.sqrt()).abs() < 1e-6);
        assert!((axis[1] - 2.0 / 5.0_f32.sqrt()).abs() < 1e-6);
        assert!(mean_direction(&[vec![1.0], vec![-1.0]], &[vec![0.0], vec![0.0]]).is_err());
        assert!(mean_direction(&[vec![1.0]], &[]).is_err());
        assert!(mean_direction(&[vec![1.0]], &[vec![1.0, 2.0]]).is_err());
        assert!(mean_direction(&[vec![f32::NAN]], &[vec![1.0]]).is_err());
        assert!(normalize_direction(&[]).is_err());
        assert!(normalize_direction(&[0.0]).is_err());
        assert!(normalize_direction(&[f64::INFINITY]).is_err());
        assert_eq!(normalize_direction(&[f64::MAX]).expect("extreme"), [1.0]);
        assert_eq!(
            normalize_direction(&[f64::from_bits(1)]).expect("subnormal"),
            [1.0]
        );
        assert_eq!(
            mean_direction(&[vec![f32::MAX]], &[vec![-f32::MAX]]).expect("difference in f64"),
            [1.0]
        );
    }

    #[test]
    fn signed_metrics_match_hand_calculation() {
        let result =
            metrics_from_log_probs(&[[-1.0, -3.0, -4.0, -1.0]], &[0.5], 2.0, 1.0).expect("metrics");
        close(result.positive_margin, 2.0);
        close(result.negative_margin, 3.0);
        close(result.signed_utility, 2.5);
        close(
            result.loss,
            ((-1.0_f64).exp().ln_1p() + (-2.0_f64).exp().ln_1p()) * 0.5 + 0.5,
        );
        assert_eq!(result.polarity_agreement, 1.0);
        let baseline = metrics_from_log_probs(&[[-1.0, -3.0, -1.0, -3.0]], &[0.0], 0.0, 1.0)
            .expect("baseline");
        assert_eq!(baseline.signed_utility, 0.0);
        assert_eq!(baseline.polarity_agreement, 0.0);
        let contrary = metrics_from_log_probs(&[[-3.0, -1.0, -1.0, -4.0]], &[0.5], 2.0, 1.0)
            .expect("contrary");
        assert!(contrary.loss > result.loss);
        assert_eq!(contrary.signed_utility, -2.5);
        let ties = metrics_from_log_probs(&[[-1.0; 4]], &[0.0], 0.0, 0.0).expect("ties");
        close(ties.loss, 2.0_f64.ln());
        assert_eq!(ties.polarity_agreement, 0.0);
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.01] {
            assert!(
                metrics_from_log_probs(&[[invalid, -1.0, -1.0, -1.0]], &[0.0], 0.0, 1.0).is_err()
            );
        }
        assert!(metrics_from_log_probs(&[], &[0.0], 0.0, 1.0).is_err());
        assert!(metrics_from_log_probs(&[[-1.0; 4]], &[f32::NAN], 0.0, 1.0).is_err());
    }

    #[test]
    fn softplus_extremes_are_stable() {
        assert_eq!(softplus(1e300).expect("large"), 1e300);
        assert_eq!(softplus(-1e300).expect("small"), 0.0);
        close(softplus(0.0).expect("zero"), 2.0_f64.ln());
        assert!(softplus(f64::NAN).is_err());
    }

    #[test]
    fn projection_bounds_whole_stack_including_rounding() {
        let projected = projected_gains(&[3.0, -4.0], 1.0).expect("project");
        assert!((projected[0] - 0.6).abs() < 1e-6);
        assert!((projected[1] + 0.8).abs() < 1e-6);
        assert!(gain_norm_squared(&projected).expect("norm") <= 1.0);
        assert_eq!(
            projected_gains(&[3.0, -4.0], 0.0).expect("zero"),
            [0.0, 0.0]
        );
        assert_eq!(
            projected_gains(&[0.1, 0.2], 1.0).expect("interior"),
            [0.1, 0.2]
        );
        for radius in [1.0, 0.001, f32::MIN_POSITIVE, f32::from_bits(1)] {
            for width in 1..=MAX_RESIDUAL_TRAINING_LAYERS {
                let gains = vec![f32::MAX; width];
                let projected = projected_gains(&gains, radius).expect("extreme projection");
                assert!(gain_norm_squared(&projected).expect("norm") <= f64::from(radius).powi(2));
            }
        }
        assert!(projected_gains(&[], 1.0).is_err());
        assert!(projected_gains(&[f32::NAN], 1.0).is_err());
        assert!(projected_gains(&[1.0], f32::INFINITY).is_err());
        assert!(projected_gains(&[1.0], -1.0).is_err());
    }

    #[test]
    fn search_ties_and_numerical_noise_preserve_no_op() {
        let mut calls = Vec::new();
        let history = coordinate_search(1, 2, 1.0, 2.0, |gains| {
            calls.push(gains.to_vec());
            Ok(metric(1.0))
        })
        .expect("search");
        assert_eq!(
            calls,
            [vec![0.0], vec![-1.0], vec![1.0], vec![-0.5], vec![0.5]]
        );
        assert!(history.iter().all(|step| step.gains == [0.0]));
        assert_eq!(history.len(), 3);
        assert!(!strictly_improves(1.0 - 1e-13, 1.0));
        assert!(!strictly_improves(1.0 - 1e-10, 1.0));
        assert!(!strictly_improves(f64::NAN, 1.0));
        assert!(!strictly_improves(0.0, f64::INFINITY));
        let history = coordinate_search(1, 1, 1.0, 1.0, |gains| {
            Ok(metric(if gains[0] == 0.0 { 2.0 } else { 1.0 }))
        })
        .expect("symmetric");
        assert_eq!(
            history[1].gains,
            [-1.0],
            "equal candidate losses preserve the first winner"
        );
    }

    #[test]
    fn improvement_requires_absolute_floor_and_relative_tolerance() {
        let floor = MIN_RESIDUAL_LOSS_IMPROVEMENT;
        assert_eq!(
            floor,
            4.0 * llama_native_types::MAX_RESIDUAL_NO_OP_LOGPROB_DELTA
        );
        assert!(!strictly_improves(0.0, floor * 0.5));
        assert!(
            !strictly_improves(0.0, floor),
            "exact boundary is not strict improvement"
        );
        assert!(strictly_improves(0.0, f64::from_bits(floor.to_bits() + 1)));
        assert!(
            !strictly_improves(1e12 - 0.5, 1e12),
            "relative guard still applies"
        );
        assert!(strictly_improves(1e12 - 2.0, 1e12));
        assert!(!strictly_improves(1.0, 1.0));
        assert!(!strictly_improves(2.0, 1.0));
    }

    #[test]
    fn search_preserves_no_op_below_drift_floor_and_accepts_clear_improvement() {
        for (improvement, expected_gain) in [
            (MIN_RESIDUAL_LOSS_IMPROVEMENT * 0.5, 0.0),
            (MIN_RESIDUAL_LOSS_IMPROVEMENT * 2.0, -1.0),
        ] {
            let history = coordinate_search(1, 2, 1.0, 1.0, |gains| {
                Ok(metric(if gains[0] == 0.0 {
                    1.0
                } else {
                    1.0 - improvement
                }))
            })
            .expect("search");
            assert_eq!(history.len(), 3);
            assert_eq!(history[1].gains, [expected_gain]);
            assert_eq!(history[2].gains, [expected_gain]);
        }
    }

    #[test]
    fn search_is_deterministic_bounded_monotone_and_baseline_first() {
        let run = || {
            coordinate_search(2, 4, 0.5, 1.0, |gains| {
                assert!(gain_norm_squared(gains).expect("norm") <= 1.0);
                Ok(metric(
                    (f64::from(gains[0]) - 0.5).powi(2) + (f64::from(gains[1]) + 0.5).powi(2),
                ))
            })
            .expect("search")
        };
        let history = run();
        assert_eq!(history, run());
        assert_eq!(history.len(), 5);
        assert_eq!(history[0].gains, [0.0, 0.0]);
        assert_eq!(history[4].gains, [0.5, -0.5]);
        assert_eq!(history[4].metrics.loss, 0.0);
        assert!(
            history
                .windows(2)
                .all(|w| w[1].metrics.loss <= w[0].metrics.loss)
        );
        let mut calls = 0;
        let zero = coordinate_search(2, 3, 0.5, 0.0, |_| {
            calls += 1;
            Ok(metric(1.0))
        })
        .expect("zero radius");
        assert_eq!(calls, 1);
        assert_eq!(zero.len(), 4);
        let zero = coordinate_search(1, 0, 0.5, 1.0, |_| Ok(metric(1.0))).expect("zero sweeps");
        assert_eq!(zero.len(), 1);
    }

    #[test]
    fn search_rejects_invalid_evaluations_and_propagates_errors() {
        assert!(coordinate_search(0, 1, 1.0, 1.0, |_| Ok(metric(1.0))).is_err());
        assert!(coordinate_search(1, 65, 1.0, 1.0, |_| Ok(metric(1.0))).is_err());
        assert!(coordinate_search(1, 1, f32::NAN, 1.0, |_| Ok(metric(1.0))).is_err());
        assert!(coordinate_search(1, 1, 1.0, 1.0, |_| Ok(metric(f64::NAN))).is_err());
        assert!(coordinate_search(1, 1, 1.0, 1.0, |_| Err(invalid("cancelled"))).is_err());
        assert!(
            coordinate_search(1, 1, 1.0, 1.0, |gains| {
                let mut result = metric(1.0);
                if gains[0] != 0.0 {
                    result.pair_count = 1;
                }
                Ok(result)
            })
            .is_err()
        );
    }
}
