//! Research-only interventions, isolated from the resident generation context.
//! An evaluated control is not a promoted deployment control or a trait claim.

use super::residual_training::{decode_token, validate_architecture};
use super::*;
use llama_native_types::{
    RESIDUAL_EVALUATION_INTERVENTION, RESIDUAL_EVALUATION_SAMPLING, ResidualEvaluationCaseOutput,
    ResidualEvaluationOutput, ResidualEvaluationRequest, ResidualEvaluationTermination,
    compose_residual_control, residual_control_stack_norm,
};

/// Move-only evidence of a completed operation on one native owner.
///
/// ```compile_fail
/// use llama_native_engine::VerifiedResidualEvaluation;
/// fn duplicate(value: &VerifiedResidualEvaluation) -> VerifiedResidualEvaluation {
///     value.clone()
/// }
/// ```
pub struct VerifiedResidualEvaluation {
    output: ResidualEvaluationOutput,
    worker_identity: Arc<WorkerIdentity>,
}

impl std::fmt::Debug for VerifiedResidualEvaluation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedResidualEvaluation")
            .field("request_sha256", &self.output.request_sha256)
            .finish_non_exhaustive()
    }
}

impl VerifiedResidualEvaluation {
    pub(super) fn from_worker(
        output: ResidualEvaluationOutput,
        worker_identity: Arc<WorkerIdentity>,
    ) -> Self {
        Self {
            output,
            worker_identity,
        }
    }

    #[must_use]
    pub const fn output(&self) -> &ResidualEvaluationOutput {
        &self.output
    }

    #[must_use]
    pub fn belongs_to_joined_model(&self, joined: &JoinedNativeModel) -> bool {
        Arc::ptr_eq(&self.worker_identity, &joined.worker_identity)
    }
}

#[derive(Debug)]
pub struct ResidualEvaluationTicket {
    pub request_id: String,
    result: Receiver<NativeResult<VerifiedResidualEvaluation>>,
    control: Arc<ActiveRequest>,
}

impl ResidualEvaluationTicket {
    pub fn cancel(&self) {
        self.control.cancel_all();
    }

    pub fn wait(self) -> NativeResult<VerifiedResidualEvaluation> {
        self.result.recv().map_err(|_| {
            NativeError::new(
                NativeErrorCode::WorkerStopped,
                "residual evaluation worker stopped",
            )
        })?
    }

    pub fn wait_timeout(
        self,
        timeout: Duration,
    ) -> NativeResult<WaitOutcome<Self, VerifiedResidualEvaluation>> {
        match self.result.recv_timeout(timeout) {
            Ok(result) => result.map(WaitOutcome::Ready),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(WaitOutcome::TimedOut(self)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "residual evaluation worker disconnected",
            )),
        }
    }
}

impl Drop for ResidualEvaluationTicket {
    fn drop(&mut self) {
        self.control.cancel_all();
    }
}

impl NativeModelHandle {
    /// Evaluate arbitrary norm-bounded controls for research, without promoting
    /// them to the controlled-generation API or changing resident model state.
    pub fn evaluate_residual(
        &self,
        request: ResidualEvaluationRequest,
    ) -> NativeResult<ResidualEvaluationTicket> {
        request.validate()?;
        if request.model_id != self.status().model_id {
            return Err(invalid(
                "residual evaluation model_id differs from resident model",
            ));
        }
        let request_id = request.request_id.clone();
        let cancellation = Arc::new(AtomicBool::new(false));
        let (result, result_rx) = bounded(1);
        let control = self.inner.admit_command(
            request_id.clone(),
            RequestClass::ResidualEvaluation,
            RequestControls::ResidualEvaluation {
                cancellation: Arc::clone(&cancellation),
            },
            |request_lease| WorkerCommand::EvaluateResidual {
                request,
                result,
                cancellation,
                request_lease,
            },
            "submitting residual evaluation",
        )?;
        Ok(ResidualEvaluationTicket {
            request_id,
            result: result_rx,
            control,
        })
    }
}

fn invalid(message: &str) -> NativeError {
    NativeError::new(NativeErrorCode::InvalidConfig, message)
}

fn cancelled(cancellation: &AtomicBool) -> NativeResult<()> {
    if cancellation.load(Ordering::Acquire) {
        Err(NativeError::new(
            NativeErrorCode::Cancelled,
            "residual evaluation cancelled",
        ))
    } else {
        Ok(())
    }
}

fn prepare_prefix(
    context: &mut LlamaContext<'_>,
    prefix: &[i32],
    control: Option<(&[f32], u32, u32)>,
    cancellation: &AtomicBool,
) -> NativeResult<()> {
    context
        .control_vector_clear()
        .map_err(|_| invalid("clear evaluation control failed"))?;
    context.clear_kv_cache();
    let last = prefix.len() - 1;
    for (position, &token) in prefix[..last].iter().enumerate() {
        decode_token(context, token, position, cancellation)?;
    }
    if let Some((values, first, last)) = control {
        context
            .control_vector_set(values, first, last)
            .map_err(|_| invalid("apply evaluation control failed"))?;
    }
    decode_token(context, prefix[last], last, cancellation)
}

/// Stable log-softmax over the entire vocabulary; no top-k approximation.
fn log_distribution(logits: &[f32], vocabulary: usize) -> NativeResult<Vec<f64>> {
    if vocabulary == 0
        || vocabulary > llama_native_types::MAX_RESIDUAL_TRAINING_VOCABULARY
        || logits.len() != vocabulary
        || logits.iter().any(|v| !v.is_finite())
    {
        return Err(invalid("invalid residual evaluation logits or vocabulary"));
    }
    let max = f64::from(logits.iter().copied().fold(f32::NEG_INFINITY, f32::max));
    let log_sum = logits
        .iter()
        .map(|&v| (f64::from(v) - max).exp())
        .sum::<f64>()
        .ln();
    Ok(logits
        .iter()
        .map(|&v| f64::from(v) - max - log_sum)
        .collect())
}

fn token_metrics(
    reference: &[f32],
    controlled: &[f32],
    vocabulary: usize,
    token: i32,
) -> NativeResult<(f64, f64)> {
    if token < 0 || token as usize >= vocabulary {
        return Err(invalid("invalid evaluation target token"));
    }
    let reference = log_distribution(reference, vocabulary)?;
    let controlled = log_distribution(controlled, vocabulary)?;
    let kl = reference
        .iter()
        .zip(&controlled)
        .map(|(&r, &c)| r.exp() * (r - c))
        .sum::<f64>();
    if !kl.is_finite() || kl < -1e-10 {
        return Err(invalid("invalid reference-to-control KL"));
    }
    Ok((controlled[token as usize], kl.max(0.0)))
}

fn greedy_token(logits: &[f32], vocabulary: usize) -> NativeResult<LlamaToken> {
    if vocabulary == 0
        || vocabulary > llama_native_types::MAX_RESIDUAL_TRAINING_VOCABULARY
        || logits.len() != vocabulary
        || logits.iter().any(|v| !v.is_finite())
    {
        return Err(invalid("invalid greedy evaluation logits or vocabulary"));
    }
    // Keep the first index on a tie. No sampling state or seed dependence.
    let mut best = 0;
    for index in 1..vocabulary {
        if logits[index] > logits[best] {
            best = index;
        }
    }
    Ok(LlamaToken(
        i32::try_from(best).map_err(|_| invalid("token index overflow"))?,
    ))
}

pub(super) fn execute(
    config: &NativeModelConfig,
    backend: &LlamaBackend,
    model: &LlamaModel,
    fingerprint: &ModelFingerprint,
    request: &ResidualEvaluationRequest,
    cancellation: &AtomicBool,
) -> NativeResult<ResidualEvaluationOutput> {
    cancelled(cancellation)?;
    request.validate()?;
    if request.expected_model_sha256 != fingerprint.model_sha256 {
        return Err(invalid(
            "residual profile expected model digest differs from loaded model",
        ));
    }
    validate_architecture(
        &model
            .meta_val_str("general.architecture")
            .unwrap_or_default(),
    )?;
    let context_tokens = config
        .context_tokens
        .min(model.n_ctx_train())
        .clamp(512, 4096);
    let width =
        usize::try_from(model.n_embd()).map_err(|_| invalid("invalid residual model width"))?;
    let vocabulary = usize::try_from(model.n_vocab()).map_err(|_| invalid("invalid vocabulary"))?;
    // Reject ALL malformed or oversized cases before allocating contexts or
    // producing partial measurements, including shared-layer norm cross-terms.
    request.validate_for_model(model.n_layer(), width, vocabulary, context_tokens as usize)?;
    let mut execution_config = config.clone();
    execution_config.max_sequences = 1;
    execution_config.batch_tokens = execution_config.batch_tokens.min(context_tokens);
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
        .map_err(|_| invalid("create residual evaluation context failed"))?;
    // Reference is only required by teacher-forcing rows. Avoid a second KV
    // allocation for generation-only requests.
    let mut reference = if request
        .cases
        .iter()
        .any(|case| !case.continuation.is_empty())
    {
        Some(
            model
                .new_context(
                    backend,
                    generation_context_params(&execution_config, context_tokens),
                )
                .map_err(|_| invalid("create residual reference context failed"))?,
        )
    } else {
        None
    };
    let layers: Vec<_> = request
        .profiles
        .iter()
        .flat_map(|p| p.layers.iter().copied())
        .collect();
    let first = *layers
        .iter()
        .min()
        .ok_or_else(|| invalid("missing control layer"))?;
    let last = *layers
        .iter()
        .max()
        .ok_or_else(|| invalid("missing control layer"))?;
    let mut cases = Vec::with_capacity(request.cases.len());
    for case in &request.cases {
        cancelled(cancellation)?;
        let values = compose_residual_control(request, case, model.n_layer(), width)?;
        let actual_stack_norm = residual_control_stack_norm(request, case)?;
        // Canonical disabled path for exact zero, including cancelling profiles.
        let control = if actual_stack_norm == 0.0 {
            None
        } else {
            Some((values.as_slice(), first, last))
        };
        let (mean_continuation_logprob, mean_reference_kl) = if case.continuation.is_empty() {
            (None, None)
        } else {
            let reference = reference
                .as_mut()
                .ok_or_else(|| invalid("reference context missing"))?;
            prepare_prefix(reference, &case.prefix, None, cancellation)?;
            prepare_prefix(&mut context, &case.prefix, control, cancellation)?;
            let mut likelihood = 0.0;
            let mut kl = 0.0;
            for (index, &token) in case.continuation.iter().enumerate() {
                cancelled(cancellation)?;
                let (lp, delta) = token_metrics(
                    reference.get_logits_ith(0),
                    context.get_logits_ith(0),
                    vocabulary,
                    token,
                )?;
                likelihood += lp;
                kl += delta;
                if index + 1 < case.continuation.len() {
                    let position = case.prefix.len() + index;
                    decode_token(reference, token, position, cancellation)?;
                    decode_token(&mut context, token, position, cancellation)?;
                }
            }
            let count = case.continuation.len() as f64;
            (Some(likelihood / count), Some(kl / count))
        };
        let mut generated_token_ids = Vec::with_capacity(case.maximum_new_tokens as usize);
        let mut generated_bytes = Vec::new();
        let mut termination = ResidualEvaluationTermination::NotRequested;
        if case.maximum_new_tokens != 0 {
            // Forced continuation is not leaked into the free-generation prefix.
            prepare_prefix(&mut context, &case.prefix, control, cancellation)?;
            termination = ResidualEvaluationTermination::TokenLimit;
            for index in 0..case.maximum_new_tokens as usize {
                cancelled(cancellation)?;
                let token = greedy_token(context.get_logits_ith(0), vocabulary)?;
                if model.is_eog_token(token) {
                    termination = ResidualEvaluationTermination::Eog;
                    break;
                }
                let piece = generated_token_piece(model, token)
                    .map_err(|_| invalid("residual generated token bytes could not be decoded"))?;
                generated_bytes.extend_from_slice(&piece);
                generated_token_ids.push(token.0);
                if index + 1 < case.maximum_new_tokens as usize {
                    decode_token(
                        &mut context,
                        token.0,
                        case.prefix.len() + index,
                        cancellation,
                    )?;
                }
            }
        }
        context
            .control_vector_clear()
            .map_err(|_| invalid("clear evaluation control failed"))?;
        cases.push(ResidualEvaluationCaseOutput {
            id: case.id.clone(),
            coefficients: case.coefficients.clone(),
            actual_stack_norm,
            generated_token_ids,
            generated_bytes,
            mean_continuation_logprob,
            mean_reference_kl,
            target_token_count: case.continuation.len() as u32,
            termination,
        });
    }
    cancelled(cancellation)?;
    let output = ResidualEvaluationOutput {
        request_sha256: request.sha256()?,
        model_fingerprint: fingerprint.clone(),
        execution_fingerprint,
        sampling_method: RESIDUAL_EVALUATION_SAMPLING.into(),
        intervention_semantics: RESIDUAL_EVALUATION_INTERVENTION.into(),
        profile_sha256: request
            .profiles
            .iter()
            .map(|p| p.sha256())
            .collect::<NativeResult<_>>()?,
        cases,
    };
    output.validate_for_request(request)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_kl_uses_reference_direction_and_entire_distribution() {
        let (lp, kl) = token_metrics(&[0.0, 0.0], &[0.0, 2.0], 2, 1).expect("finite logits");
        assert!((lp - (1.0_f64 / (1.0 + (-2.0_f64).exp())).ln()).abs() < 1e-12);
        let expected = 0.5
            * ((0.5_f64 / (1.0 / (1.0 + 2.0_f64.exp()))).ln()
                + (0.5_f64 / (1.0 / (1.0 + (-2.0_f64).exp()))).ln());
        assert!((kl - expected).abs() < 1e-12);
        let (_, reverse) =
            token_metrics(&[0.0, 2.0], &[0.0, 0.0], 2, 1).expect("finite reverse logits");
        assert!((kl - reverse).abs() > 0.01);
    }

    #[test]
    fn identity_and_extreme_finite_logits_remain_valid() {
        assert_eq!(
            token_metrics(&[3.0, -2.0], &[3.0, -2.0], 2, 0)
                .expect("finite identical logits")
                .1,
            0.0
        );
        let (lp, kl) = token_metrics(&[f32::MAX, -f32::MAX], &[-f32::MAX, f32::MAX], 2, 0)
            .expect("extreme finite logits");
        assert!(lp.is_finite() && kl.is_finite() && kl > 1e38);
        assert_eq!(
            greedy_token(&[1.0, 1.0, -2.0], 3).expect("finite tied logits"),
            LlamaToken(0)
        );
    }

    #[test]
    fn invalid_logits_and_targets_fail_closed() {
        for logits in [
            vec![],
            vec![f32::NAN, 0.0],
            vec![f32::INFINITY, 0.0],
            vec![0.0],
        ] {
            assert!(greedy_token(&logits, 2).is_err());
            assert!(token_metrics(&logits, &[0.0, 0.0], 2, 0).is_err());
        }
        assert!(token_metrics(&[0.0], &[0.0], 1, -1).is_err());
        assert!(token_metrics(&[0.0], &[0.0], 1, 1).is_err());
    }
}
