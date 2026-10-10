//! Frozen-model residual learning on the existing model owner worker.
//!
//! Only temporary contexts are mutated. The serializable output is research
//! evidence; the move-only completion additionally binds the owning worker.

use super::residual_training_math as math;
use super::*;
use llama_cpp_2::context::hidden_states::HiddenStateCaptureConfig;
use llama_native_types::{
    MAX_RESIDUAL_NO_OP_LOGPROB_DELTA, RESIDUAL_INTERVENTION_SEMANTICS, ResidualPooling,
    ResidualTrainingMetrics, ResidualTrainingOutput, ResidualTrainingPair, ResidualTrainingRequest,
};

/// A completed native training operation, not a claim of scientific validity.
///
/// ```compile_fail
/// use llama_native_engine::VerifiedResidualTraining;
/// fn clone_training(value: &VerifiedResidualTraining) -> VerifiedResidualTraining {
///     value.clone()
/// }
/// ```
pub struct VerifiedResidualTraining {
    output: ResidualTrainingOutput,
    worker_identity: Arc<WorkerIdentity>,
}

impl std::fmt::Debug for VerifiedResidualTraining {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedResidualTraining")
            .field("request_sha256", &self.output.request_sha256)
            .finish_non_exhaustive()
    }
}

impl VerifiedResidualTraining {
    pub(super) fn from_worker(
        output: ResidualTrainingOutput,
        worker_identity: Arc<WorkerIdentity>,
    ) -> Self {
        Self {
            output,
            worker_identity,
        }
    }

    #[must_use]
    pub const fn output(&self) -> &ResidualTrainingOutput {
        &self.output
    }

    #[must_use]
    pub fn belongs_to_joined_model(&self, joined: &JoinedNativeModel) -> bool {
        Arc::ptr_eq(&self.worker_identity, &joined.worker_identity)
    }
}

#[derive(Debug)]
pub struct ResidualTrainingTicket {
    pub request_id: String,
    result: Receiver<NativeResult<VerifiedResidualTraining>>,
    control: Arc<ActiveRequest>,
}

impl ResidualTrainingTicket {
    pub fn cancel(&self) {
        self.control.cancel_all();
    }

    pub fn wait(self) -> NativeResult<VerifiedResidualTraining> {
        self.result.recv().map_err(|error| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                format!("residual training worker stopped: {error}"),
            )
        })?
    }

    pub fn wait_timeout(
        self,
        timeout: Duration,
    ) -> NativeResult<WaitOutcome<Self, VerifiedResidualTraining>> {
        match self.result.recv_timeout(timeout) {
            Ok(result) => result.map(WaitOutcome::Ready),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(WaitOutcome::TimedOut(self)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "residual training worker disconnected",
            )),
        }
    }
}

impl Drop for ResidualTrainingTicket {
    fn drop(&mut self) {
        self.control.cancel_all();
    }
}

impl NativeModelHandle {
    /// Fit contrast-initialized residual directions and bounded layer gains.
    /// No gradients through model weights or resident generation cache are used.
    pub fn train_residual(
        &self,
        request: ResidualTrainingRequest,
    ) -> NativeResult<ResidualTrainingTicket> {
        request.validate()?;
        if request.model_id != self.status().model_id {
            return Err(invalid(
                "residual training model_id differs from the resident model",
            ));
        }
        let request_id = request.request_id.clone();
        let cancellation = Arc::new(AtomicBool::new(false));
        let (result, result_rx) = bounded(1);
        let control = self.inner.admit_command(
            request_id.clone(),
            RequestClass::ResidualTraining,
            RequestControls::ResidualTraining {
                cancellation: Arc::clone(&cancellation),
            },
            |request_lease| WorkerCommand::TrainResidual {
                request,
                result,
                cancellation,
                request_lease,
            },
            "submitting residual training",
        )?;
        Ok(ResidualTrainingTicket {
            request_id,
            result: result_rx,
            control,
        })
    }
}

fn invalid(message: impl Into<String>) -> NativeError {
    NativeError::new(NativeErrorCode::InvalidConfig, message)
}

fn check_cancelled(cancellation: &AtomicBool) -> NativeResult<()> {
    if cancellation.load(Ordering::Acquire) {
        Err(NativeError::new(
            NativeErrorCode::Cancelled,
            "residual training cancelled",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn decode_token(
    context: &mut LlamaContext<'_>,
    token: i32,
    position: usize,
    cancellation: &AtomicBool,
) -> NativeResult<()> {
    check_cancelled(cancellation)?;
    let mut batch = LlamaBatch::new(1, 1);
    batch
        .add(
            LlamaToken(token),
            i32::try_from(position).map_err(|_| invalid("position overflow"))?,
            &[0],
            true,
        )
        .map_err(|error| invalid(format!("invalid residual training batch: {error}")))?;
    context
        .decode(&mut batch)
        .map_err(|error| native_decode_error("residual training", error))
}

/// Prefix logits are unused. Keep the intervention/capture boundary out of
/// these batches, and bound submitted chunk size independently of resident
/// batch capacity. Cancellation is cooperative, not a wall-clock guarantee.
/// Zero outputs is the same convention as native KV prefill.
pub(super) fn decode_unmodified_prefix(
    context: &mut LlamaContext<'_>,
    tokens: &[i32],
    cancellation: &AtomicBool,
) -> NativeResult<()> {
    let chunk_size = (context.n_batch().max(1) as usize).min(64);
    for (index, chunk) in tokens.chunks(chunk_size).enumerate() {
        check_cancelled(cancellation)?;
        let mut batch = LlamaBatch::new(chunk.len(), 1);
        for (offset, &token) in chunk.iter().enumerate() {
            batch
                .add(
                    LlamaToken(token),
                    i32::try_from(index * chunk_size + offset)
                        .map_err(|_| invalid("position overflow"))?,
                    &[0],
                    false,
                )
                .map_err(|error| invalid(format!("invalid residual prefix batch: {error}")))?;
        }
        context
            .decode(&mut batch)
            .map_err(|error| native_decode_error("residual prefix", error))?;
    }
    check_cancelled(cancellation)
}

fn capture_start(
    pooling: ResidualPooling,
    prefix_len: usize,
    pole_len: usize,
) -> NativeResult<usize> {
    let total = prefix_len
        .checked_add(pole_len)
        .ok_or_else(|| invalid("capture length overflow"))?;
    if prefix_len == 0
        || pole_len == 0
        || total > llama_native_types::MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS
    {
        return Err(invalid("invalid capture prefix or pole length"));
    }
    Ok(match pooling {
        ResidualPooling::TerminalToken => total - 1,
        ResidualPooling::ResponseSpanMean | ResidualPooling::TerminalTokenMatched => prefix_len,
    })
}

fn capture_pole(
    context: &mut LlamaContext<'_>,
    prefix: &[i32],
    pole: &[i32],
    pooling: ResidualPooling,
    layers: &[u32],
    width: usize,
    cancellation: &AtomicBool,
) -> NativeResult<Vec<Vec<f64>>> {
    context
        .control_vector_clear()
        .map_err(|error| invalid(format!("clear residual control: {error}")))?;
    context.clear_kv_cache();
    context
        .set_hidden_state_capture_enabled(false)
        .map_err(|error| invalid(format!("disable capture: {error}")))?;
    // Preserve V2 terminal decode shape exactly. Both V3 modes prefill only
    // the neutral prefix and capture/validate every singleton pole token.
    let tokens: Vec<_> = prefix.iter().chain(pole).copied().collect();
    let capture_start = capture_start(pooling, prefix.len(), pole.len())?;
    decode_unmodified_prefix(context, &tokens[..capture_start], cancellation)?;
    context
        .set_hidden_state_capture_enabled(true)
        .map_err(|error| invalid(format!("enable pole capture: {error}")))?;
    pool_captured_tokens(
        &tokens[capture_start..],
        capture_start,
        layers.len(),
        width,
        pooling,
        cancellation,
        |token, position| {
            decode_token(context, token, position, cancellation)?;
            let states = context
                .take_hidden_states()
                .map_err(|error| invalid(format!("capture residual states: {error}")))?;
            if states.len() != layers.len()
                || states
                    .iter()
                    .zip(layers)
                    .any(|(state, layer)| state.layer != *layer)
            {
                return Err(invalid("captured layer identity or order mismatch"));
            }
            Ok(states.into_iter().map(|state| state.values).collect())
        },
    )
}

fn pool_captured_tokens(
    tokens: &[i32],
    start_position: usize,
    layers: usize,
    width: usize,
    pooling: ResidualPooling,
    cancellation: &AtomicBool,
    mut capture: impl FnMut(i32, usize) -> NativeResult<Vec<Vec<f32>>>,
) -> NativeResult<Vec<Vec<f64>>> {
    let mut accumulator = math::PoleAccumulator::new(layers, width, tokens.len(), pooling)?;
    for (offset, &token) in tokens.iter().enumerate() {
        check_cancelled(cancellation)?;
        let position = start_position
            .checked_add(offset)
            .ok_or_else(|| invalid("capture position overflow"))?;
        let states = capture(token, position)?;
        check_cancelled(cancellation)?;
        accumulator.push(&states)?;
    }
    accumulator.finish()
}

// The callback has no holdout input. Only training representations contribute
// to axes, and each pole mean and each pair has equal weight.
fn fit_directions(
    request: &ResidualTrainingRequest,
    width: usize,
    mut capture: impl FnMut(&[i32], &[i32], ResidualPooling) -> NativeResult<Vec<Vec<f64>>>,
) -> NativeResult<Vec<Vec<f32>>> {
    request.validate()?;
    if !(1..=llama_native_types::MAX_RESIDUAL_TRAINING_DIMENSIONS).contains(&width)
        || width
            .checked_mul(request.layers.len())
            .is_none_or(|n| n > llama_native_types::MAX_RESIDUAL_CAPTURE_VALUES)
    {
        return Err(invalid("invalid direction accumulator width"));
    }
    let mut means = vec![vec![0.0_f64; width]; request.layers.len()];
    for pair in &request.train {
        let positive = capture(&pair.prefix, &pair.positive, request.pooling)?;
        let negative = capture(&pair.prefix, &pair.negative, request.pooling)?;
        if positive.len() != means.len() || negative.len() != means.len() {
            return Err(invalid("captured layer count mismatch"));
        }
        for ((mean, pos), neg) in means.iter_mut().zip(positive).zip(negative) {
            if pos.len() != width
                || neg.len() != width
                || pos.iter().chain(&neg).any(|v| !v.is_finite())
            {
                return Err(invalid("captured width or finite-value mismatch"));
            }
            for ((sum, pos), neg) in mean.iter_mut().zip(pos).zip(neg) {
                *sum += (pos - neg) / request.train.len() as f64;
            }
        }
    }
    means
        .iter()
        .map(|mean| math::normalize_direction(mean))
        .collect()
}

#[derive(Clone, Copy)]
struct Control<'a> {
    values: &'a [f32],
    first: u32,
    last: u32,
}

fn control<'a>(values: &'a [f32], layers: &[u32]) -> NativeResult<Control<'a>> {
    Ok(Control {
        values,
        first: *layers
            .iter()
            .min()
            .ok_or_else(|| invalid("missing control layers"))?,
        last: *layers
            .iter()
            .max()
            .ok_or_else(|| invalid("missing control layers"))?,
    })
}

fn score_tokens(
    context: &mut LlamaContext<'_>,
    prefix: &[i32],
    pole: &[i32],
    control: Option<Control<'_>>,
    cancellation: &AtomicBool,
) -> NativeResult<Vec<f64>> {
    context
        .control_vector_clear()
        .map_err(|error| invalid(format!("clear residual control: {error}")))?;
    context.clear_kv_cache();
    let last = prefix.len() - 1;
    decode_unmodified_prefix(context, &prefix[..last], cancellation)?;
    // All cached prefix tokens precede the intervention. From the final prefix
    // token onward singleton decode makes the intervention position explicit.
    if let Some(control) = control {
        context
            .control_vector_set(control.values, control.first, control.last)
            .map_err(|error| invalid(format!("apply residual control: {error}")))?;
    }
    decode_token(context, prefix[last], last, cancellation)?;
    let mut scores = Vec::with_capacity(pole.len());
    for (index, &token) in pole.iter().enumerate() {
        check_cancelled(cancellation)?;
        scores.push(math::selected_token_log_probability(
            context.get_logits_ith(0),
            token,
            context.model.n_vocab() as usize,
        )?);
        if index + 1 < pole.len() {
            decode_token(context, token, prefix.len() + index, cancellation)?;
        }
    }
    context
        .control_vector_clear()
        .map_err(|error| invalid(format!("clear residual control: {error}")))?;
    Ok(scores)
}

fn score_pole(
    context: &mut LlamaContext<'_>,
    prefix: &[i32],
    pole: &[i32],
    control: Option<Control<'_>>,
    cancellation: &AtomicBool,
) -> NativeResult<f64> {
    let scores = score_tokens(context, prefix, pole, control, cancellation)?;
    Ok(scores.iter().sum::<f64>() / scores.len() as f64)
}

pub(super) fn validate_architecture(architecture: &str) -> NativeResult<()> {
    // Decoder flags alone also admit diffusion models whose decode falls back
    // to stateless encode. Expand only after validating causal KV semantics.
    if architecture != "gemma4" {
        return Err(NativeError::new(
            NativeErrorCode::UnsupportedParameter,
            "residual training currently supports only Gemma 4 causal decoder models",
        ));
    }
    Ok(())
}

fn control_buffer(
    width: usize,
    model_layers: usize,
    layers: &[u32],
    directions: &[Vec<f32>],
    gains: &[f32],
    sign: f32,
) -> NativeResult<Vec<f32>> {
    let count = width
        .checked_mul(model_layers - 1)
        .ok_or_else(|| invalid("control buffer size overflow"))?;
    if count > 16_777_216 {
        return Err(invalid("control buffer exceeds 64 MiB"));
    }
    let mut buffer = vec![0.0; count];
    for ((&layer, direction), &gain) in layers.iter().zip(directions).zip(gains) {
        let start = (layer as usize - 1) * width;
        for (out, &value) in buffer[start..start + width].iter_mut().zip(direction) {
            *out = value * gain * sign;
            if !out.is_finite() {
                return Err(invalid("nonfinite control payload"));
            }
        }
    }
    Ok(buffer)
}

struct Objective<'a, 'model> {
    context: &'a mut LlamaContext<'model>,
    request: &'a ResidualTrainingRequest,
    directions: &'a [Vec<f32>],
    cancellation: &'a AtomicBool,
}

impl Objective<'_, '_> {
    fn evaluate(
        &mut self,
        pairs: &[ResidualTrainingPair],
        gains: &[f32],
    ) -> NativeResult<ResidualTrainingMetrics> {
        let no_control = gains.iter().all(|gain| *gain == 0.0);
        let width = usize::try_from(self.context.model.n_embd())
            .map_err(|_| invalid("invalid residual width"))?;
        let layers = self.context.model.n_layer() as usize;
        let positive = control_buffer(
            width,
            layers,
            &self.request.layers,
            self.directions,
            gains,
            1.0,
        )?;
        let negative = control_buffer(
            width,
            layers,
            &self.request.layers,
            self.directions,
            gains,
            -1.0,
        )?;
        let mut log_probabilities = Vec::with_capacity(pairs.len());
        for pair in pairs {
            let pos = if no_control {
                None
            } else {
                Some(control(&positive, &self.request.layers)?)
            };
            let neg = if no_control {
                None
            } else {
                Some(control(&negative, &self.request.layers)?)
            };
            log_probabilities.push([
                score_pole(
                    self.context,
                    &pair.prefix,
                    &pair.positive,
                    pos,
                    self.cancellation,
                )?,
                score_pole(
                    self.context,
                    &pair.prefix,
                    &pair.negative,
                    pos,
                    self.cancellation,
                )?,
                score_pole(
                    self.context,
                    &pair.prefix,
                    &pair.positive,
                    neg,
                    self.cancellation,
                )?,
                score_pole(
                    self.context,
                    &pair.prefix,
                    &pair.negative,
                    neg,
                    self.cancellation,
                )?,
            ]);
        }
        math::metrics_from_log_probs(
            &log_probabilities,
            gains,
            self.request.l2_penalty,
            self.request.margin,
        )
    }
}

pub(super) fn execute(
    config: &NativeModelConfig,
    backend: &LlamaBackend,
    model: &LlamaModel,
    fingerprint: &ModelFingerprint,
    request: &ResidualTrainingRequest,
    cancellation: &AtomicBool,
) -> NativeResult<ResidualTrainingOutput> {
    check_cancelled(cancellation)?;
    validate_architecture(
        &model
            .meta_val_str("general.architecture")
            .unwrap_or_default(),
    )?;
    // Request sequences are capped at 4096; never duplicate a long-chat KV
    // allocation merely because the resident generation context is larger.
    let context_tokens = config.context_tokens.min(model.n_ctx_train()).clamp(
        512,
        llama_native_types::MAX_RESIDUAL_TRAINING_SEQUENCE_TOKENS as u32,
    );
    request.validate_for_model(
        model.n_layer(),
        usize::try_from(model.n_vocab()).map_err(|_| invalid("invalid vocabulary"))?,
        context_tokens as usize,
    )?;
    let width = usize::try_from(model.n_embd()).map_err(|_| invalid("invalid residual width"))?;
    request.validate_control_vector_shape(model.n_layer(), width)?;
    if width > llama_cpp_2::context::hidden_states::MAX_CAPTURE_WIDTH {
        return Err(invalid(
            "residual training model exceeds capture width bounds",
        ));
    }
    let mut execution_config = config.clone();
    execution_config.max_sequences = 1;
    let params = generation_context_params(&execution_config, context_tokens);
    let (rope_config_sha256, kv_layout_sha256) = context_fingerprints(&params);
    let mut execution_fingerprint = fingerprint.clone();
    execution_fingerprint.context_tokens = params.n_ctx().map_or(0, NonZeroU32::get);
    execution_fingerprint.batch_tokens = params.n_batch();
    execution_fingerprint.max_sequences = 1;
    execution_fingerprint.rope_config_sha256 = rope_config_sha256;
    execution_fingerprint.kv_layout_sha256 = kv_layout_sha256;
    let mut context = model
        .new_context(backend, params)
        .map_err(|error| invalid(format!("create training scorer: {error}")))?;
    let zero = vec![0.0; width * (model.n_layer() as usize - 1)];
    let first = &request.train[0];
    let mut no_op_max_logprob_delta = 0.0_f64;
    for pole in [&first.positive, &first.negative] {
        let baseline = score_tokens(&mut context, &first.prefix, pole, None, cancellation)?;
        let zero_control = Some(control(&zero, &request.layers)?);
        // Repeated explicit zero plus a cleared replay catch graph/reset drift.
        for active in [zero_control, zero_control, None] {
            let scores = score_tokens(&mut context, &first.prefix, pole, active, cancellation)?;
            for (baseline, score) in baseline.iter().zip(scores) {
                no_op_max_logprob_delta = no_op_max_logprob_delta.max((baseline - score).abs());
            }
        }
    }
    if no_op_max_logprob_delta > MAX_RESIDUAL_NO_OP_LOGPROB_DELTA {
        return Err(invalid(format!(
            "zero-control maximum token log-probability delta {no_op_max_logprob_delta:.9} exceeds {MAX_RESIDUAL_NO_OP_LOGPROB_DELTA}"
        )));
    }
    let mut capture = model
        .new_context_with_hidden_state_capture(
            backend,
            generation_context_params(&execution_config, context_tokens),
            HiddenStateCaptureConfig::new(request.layers.clone()),
        )
        .map_err(|error| invalid(format!("create residual capture context: {error}")))?;
    let directions = fit_directions(request, width, |prefix, pole, pooling| {
        capture_pole(
            &mut capture,
            prefix,
            pole,
            pooling,
            &request.layers,
            width,
            cancellation,
        )
    })?;
    drop(capture);
    let mut objective = Objective {
        context: &mut context,
        request,
        directions: &directions,
        cancellation,
    };
    let history = math::coordinate_search(
        request.layers.len(),
        request.search_steps,
        request.initial_step,
        request.maximum_norm,
        |gains| objective.evaluate(&request.train, gains),
    )?;
    let train_baseline = history
        .first()
        .ok_or_else(|| invalid("training history is empty"))?
        .metrics;
    let last = history
        .last()
        .ok_or_else(|| invalid("training history is empty"))?;
    let gains = last.gains.clone();
    let best = last.metrics;
    // Holdout scoring starts only after directions, gains, and history are frozen.
    let validation_baseline = objective.evaluate(&request.validation, &vec![0.0; gains.len()])?;
    let validation_final = objective.evaluate(&request.validation, &gains)?;
    check_cancelled(cancellation)?;
    let output = ResidualTrainingOutput {
        request_sha256: request.sha256()?,
        pooling: request.pooling,
        model_fingerprint: fingerprint.clone(),
        execution_fingerprint,
        no_op_max_logprob_delta,
        training_method: request.pooling.training_method().into(),
        intervention_semantics: RESIDUAL_INTERVENTION_SEMANTICS.into(),
        layers: request.layers.clone(),
        directions,
        gains,
        history,
        train_baseline,
        train_final: best,
        validation_baseline,
        validation_final,
    };
    output.validate_for_request(request)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_schedule_excludes_prefix_and_single_token_matches_terminal() {
        let cancel = AtomicBool::new(false);
        let prefix = [999, 998, 997];
        for pole in [vec![10], vec![10, 20]] {
            let tokens: Vec<_> = prefix.iter().chain(&pole).copied().collect();
            let mut schedules = Vec::new();
            for pooling in [
                ResidualPooling::TerminalToken,
                ResidualPooling::TerminalTokenMatched,
                ResidualPooling::ResponseSpanMean,
            ] {
                let start = capture_start(pooling, prefix.len(), pole.len()).expect("boundary");
                let mut calls = Vec::new();
                let result = pool_captured_tokens(
                    &tokens[start..],
                    start,
                    1,
                    1,
                    pooling,
                    &cancel,
                    |token, position| {
                        assert!(position >= prefix.len());
                        assert!(token < 100, "prefix states must never be captured");
                        calls.push((token, position));
                        Ok(vec![vec![token as f32]])
                    },
                )
                .expect("mean");
                let expected = match pooling {
                    ResidualPooling::TerminalToken | ResidualPooling::TerminalTokenMatched => {
                        f64::from(*pole.last().expect("pole"))
                    }
                    ResidualPooling::ResponseSpanMean => {
                        pole.iter().map(|&v| f64::from(v)).sum::<f64>() / pole.len() as f64
                    }
                };
                assert_eq!(result, [vec![expected]]);
                assert_eq!(
                    calls,
                    tokens[start..]
                        .iter()
                        .enumerate()
                        .map(|(i, &t)| (t, start + i))
                        .collect::<Vec<_>>()
                );
                if pole.len() == 1 {
                    assert_eq!(result, [vec![10.0]]);
                }
                schedules.push((tokens[..start].to_vec(), calls));
            }
            assert_eq!(
                schedules[1], schedules[2],
                "matched and mean share exact prefill and every capture call"
            );
            if pole.len() == 1 {
                assert_eq!(schedules[0], schedules[1]);
            }
        }
        for (prefix, pole) in [(0, 1), (1, 0), (usize::MAX, 1), (4096, 1)] {
            assert!(capture_start(ResidualPooling::ResponseSpanMean, prefix, pole).is_err());
        }
    }

    #[test]
    fn pole_stream_checks_cancellation_and_propagates_capture_failure() {
        let cancel = AtomicBool::new(true);
        let mut calls = 0;
        let error = pool_captured_tokens(
            &[10, 20],
            3,
            1,
            1,
            ResidualPooling::ResponseSpanMean,
            &cancel,
            |_, _| {
                calls += 1;
                Ok(vec![vec![1.0]])
            },
        )
        .expect_err("cancel before capture");
        assert_eq!(error.code, NativeErrorCode::Cancelled);
        assert_eq!(calls, 0);
        cancel.store(false, Ordering::Release);
        let error = pool_captured_tokens(
            &[10, 20],
            3,
            1,
            1,
            ResidualPooling::TerminalTokenMatched,
            &cancel,
            |_, _| {
                calls += 1;
                cancel.store(true, Ordering::Release);
                Ok(vec![vec![1.0]])
            },
        )
        .expect_err("cancel during capture");
        assert_eq!(error.code, NativeErrorCode::Cancelled);
        assert_eq!(calls, 1);
        cancel.store(false, Ordering::Release);
        assert!(
            pool_captured_tokens(
                &[10, 20],
                3,
                1,
                1,
                ResidualPooling::TerminalTokenMatched,
                &cancel,
                |_, _| Err(invalid("capture failed"))
            )
            .is_err()
        );
        assert!(
            pool_captured_tokens(
                &[10, 20],
                usize::MAX,
                1,
                1,
                ResidualPooling::ResponseSpanMean,
                &cancel,
                |_, _| Ok(vec![vec![1.0]])
            )
            .is_err()
        );
    }

    #[test]
    fn fitting_weights_poles_and_pairs_equally_and_never_captures_holdout() {
        let mut request = ResidualTrainingRequest {
            request_id: "fit".into(),
            model_id: "model".into(),
            pooling: ResidualPooling::ResponseSpanMean,
            layers: vec![1],
            train: vec![
                ResidualTrainingPair {
                    id: "a".into(),
                    prefix: vec![1],
                    positive: vec![10, 10],
                    negative: vec![20],
                },
                ResidualTrainingPair {
                    id: "b".into(),
                    prefix: vec![2],
                    positive: vec![30],
                    negative: vec![40, 40, 40],
                },
            ],
            validation: vec![ResidualTrainingPair {
                id: "holdout".into(),
                prefix: vec![99],
                positive: vec![50],
                negative: vec![60],
            }],
            search_steps: 0,
            initial_step: 0.25,
            maximum_norm: 1.0,
            l2_penalty: 0.0,
            margin: 1.0,
        };
        let cancel = AtomicBool::new(false);
        let run = |request: &ResidualTrainingRequest| {
            let mut calls = Vec::new();
            let directions = fit_directions(request, 2, |prefix, pole, pooling| {
                assert!(prefix[0] < 99, "holdout cannot enter fitting");
                calls.push((prefix.to_vec(), pole.to_vec(), pooling));
                let start = capture_start(pooling, prefix.len(), pole.len())?;
                let tokens: Vec<_> = prefix.iter().chain(pole).copied().collect();
                pool_captured_tokens(
                    &tokens[start..],
                    start,
                    1,
                    2,
                    pooling,
                    &cancel,
                    |token, _| {
                        let state = match token {
                            10 => vec![6.0, 0.0],
                            20 => vec![0.0, 2.0],
                            30 => vec![0.0, 6.0],
                            40 => vec![2.0, 0.0],
                            _ => panic!("unexpected captured token"),
                        };
                        Ok(vec![state])
                    },
                )
            })
            .expect("directions");
            (directions, calls)
        };
        for pooling in [
            ResidualPooling::TerminalToken,
            ResidualPooling::TerminalTokenMatched,
            ResidualPooling::ResponseSpanMean,
        ] {
            request.pooling = pooling;
            let (directions, calls) = run(&request);
            assert_eq!(calls.len(), 4);
            // Pair differences are [6,-2] and [-2,6], not token-length weights
            // or unit pole vectors. Equal pair averaging gives [2,2].
            let expected = std::f32::consts::FRAC_1_SQRT_2;
            assert_eq!(directions, [vec![expected, expected]]);
            let mut changed = request.clone();
            changed.validation[0].prefix = vec![100; 3];
            changed.validation[0].positive = vec![70; 4];
            assert_ne!(
                request.sha256().expect("hash"),
                changed.sha256().expect("changed hash")
            );
            assert_eq!((directions, calls), run(&changed));
        }
    }

    #[test]
    fn trainer_rejects_unqualified_and_stateless_architectures() {
        assert!(validate_architecture("gemma4").is_ok());
        for architecture in ["", "llada", "llada2", "rnd1", "llama", "gemma3"] {
            assert_eq!(
                validate_architecture(architecture)
                    .expect_err("unsupported architecture")
                    .code,
                NativeErrorCode::UnsupportedParameter
            );
        }
    }

    #[test]
    fn control_rows_follow_post_block_indices_without_widening_layer_range() {
        let layers = [3, 1];
        let directions = [vec![1.0, 0.0], vec![0.0, 1.0]];
        let positive = control_buffer(2, 5, &layers, &directions, &[2.0, 3.0], 1.0)
            .expect("valid control shape");
        assert_eq!(positive, [0.0, 3.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0]);
        let negative = control_buffer(2, 5, &layers, &directions, &[2.0, 3.0], -1.0)
            .expect("valid negative control");
        assert!(positive.iter().zip(negative).all(|(a, b)| *a == -b));
        let bounded = control(&positive, &layers).expect("bounded control");
        assert_eq!((bounded.first, bounded.last), (1, 3));
        let singleton = control(&positive, &[3]).expect("single layer");
        assert_eq!((singleton.first, singleton.last), (3, 3));
    }
}
