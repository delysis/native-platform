//! Frozen-model residual learning on the existing model owner worker.
//!
//! Only temporary contexts are mutated. The serializable output is research
//! evidence; the move-only completion additionally binds the owning worker.

use super::residual_training_math as math;
use super::*;
use llama_cpp_2::context::hidden_states::HiddenStateCaptureConfig;
use llama_native_types::{
    MAX_RESIDUAL_NO_OP_LOGPROB_DELTA, RESIDUAL_INTERVENTION_SEMANTICS, RESIDUAL_TRAINING_METHOD,
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

fn capture_pole(
    context: &mut LlamaContext<'_>,
    prefix: &[i32],
    pole: &[i32],
    cancellation: &AtomicBool,
) -> NativeResult<Vec<Vec<f32>>> {
    context
        .control_vector_clear()
        .map_err(|error| invalid(format!("clear residual control: {error}")))?;
    context.clear_kv_cache();
    context
        .set_hidden_state_capture_enabled(false)
        .map_err(|error| invalid(format!("disable capture: {error}")))?;
    let last = prefix.len() + pole.len() - 1;
    for (position, token) in prefix.iter().chain(pole).enumerate() {
        if position == last {
            context
                .set_hidden_state_capture_enabled(true)
                .map_err(|error| invalid(format!("enable final-token capture: {error}")))?;
        }
        decode_token(context, *token, position, cancellation)?;
    }
    let states = context
        .take_hidden_states()
        .map_err(|error| invalid(format!("capture residual states: {error}")))?;
    Ok(states.into_iter().map(|state| state.values).collect())
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
    for (position, &token) in prefix[..last].iter().enumerate() {
        decode_token(context, token, position, cancellation)?;
    }
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
    let mut means = vec![vec![0.0_f64; width]; request.layers.len()];
    for pair in &request.train {
        let positive = capture_pole(&mut capture, &pair.prefix, &pair.positive, cancellation)?;
        let negative = capture_pole(&mut capture, &pair.prefix, &pair.negative, cancellation)?;
        if positive.len() != means.len() || negative.len() != means.len() {
            return Err(invalid("captured layer count mismatch"));
        }
        for ((mean, pos), neg) in means.iter_mut().zip(positive).zip(negative) {
            if pos.len() != width || neg.len() != width {
                return Err(invalid("captured width mismatch"));
            }
            for ((sum, pos), neg) in mean.iter_mut().zip(pos).zip(neg) {
                *sum += (f64::from(pos) - f64::from(neg)) / request.train.len() as f64;
            }
        }
    }
    drop(capture);
    let directions = means
        .iter()
        .map(|mean| math::normalize_direction(mean))
        .collect::<NativeResult<Vec<_>>>()?;
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
    // Holdout reads start only after directions, gains, and history are frozen.
    let validation_baseline = objective.evaluate(&request.validation, &vec![0.0; gains.len()])?;
    let validation_final = objective.evaluate(&request.validation, &gains)?;
    check_cancelled(cancellation)?;
    let output = ResidualTrainingOutput {
        request_sha256: request.sha256()?,
        model_fingerprint: fingerprint.clone(),
        execution_fingerprint,
        no_op_max_logprob_delta,
        training_method: RESIDUAL_TRAINING_METHOD.into(),
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
