//! Opt-in end-to-end training evidence. Uses only an explicitly supplied local
//! GGUF and authored non-personal contrasts; never downloads models or text.
use llama_native_engine::{NativeModelHandle, NativeModelOwner, WaitOutcome};
use llama_native_types::{
    CompletionPrompt, GenerationInput, GenerationRequest, NativeErrorCode, NativeModelConfig,
    ResidualControlProfile, ResidualEvaluationCase, ResidualEvaluationRequest, ResidualPooling,
    ResidualTrainingPair, ResidualTrainingRequest, SamplingConfig, SpecialTokenPolicy,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn write_pooling_json(
    dir: &std::path::Path,
    name: &str,
    value: &impl serde::Serialize,
) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(name))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

fn compare_directions(
    left: &llama_native_types::ResidualTrainingOutput,
    right: &llama_native_types::ResidualTrainingOutput,
) -> Result<serde_json::Value> {
    if left.layers != right.layers || left.directions.len() != right.directions.len() {
        return Err("pooling comparison shape differs".into());
    }
    let mut rows = Vec::new();
    for ((&layer, left), right) in left
        .layers
        .iter()
        .zip(&left.directions)
        .zip(&right.directions)
    {
        if left.len() != right.len() {
            return Err("pooling comparison width differs".into());
        }
        let dot: f64 = left
            .iter()
            .zip(right)
            .map(|(&a, &b)| f64::from(a) * f64::from(b))
            .sum();
        let norm = |v: &[f32]| v.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>().sqrt();
        let cosine = dot / (norm(left) * norm(right));
        if !cosine.is_finite() {
            return Err("nonfinite direction comparison".into());
        }
        let max_delta = left
            .iter()
            .zip(right)
            .map(|(&a, &b)| (f64::from(a) - f64::from(b)).abs())
            .fold(0.0_f64, f64::max);
        rows.push(
            serde_json::json!({"layer": layer, "cosine": cosine, "max_absolute_delta": max_delta}),
        );
    }
    Ok(serde_json::json!(rows))
}

/// Eight zero-search fits, no generation or residual evaluation grid. The two
/// original smoke training pairs are exposed development data, not confirmation.
#[test]
#[ignore = "requires LLAMA_RESIDUAL_MODEL and LLAMA_RESIDUAL_POOLING_OUTPUT_DIR"]
fn local_pooling_matched_schedule_and_single_token() -> Result<()> {
    let path = std::env::var("LLAMA_RESIDUAL_MODEL")?;
    let out = std::path::PathBuf::from(std::env::var("LLAMA_RESIDUAL_POOLING_OUTPUT_DIR")?);
    let timeout_seconds: u64 = std::env::var("LLAMA_RESIDUAL_POOLING_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "300".into())
        .parse()?;
    if !(1..=3600).contains(&timeout_seconds) {
        return Err("invalid pooling timeout".into());
    }
    let layer: u32 = std::env::var("LLAMA_RESIDUAL_LAYER")
        .unwrap_or_else(|_| "20".into())
        .parse()?;
    let mut config = NativeModelConfig::local(path.into());
    config.model_id = "residual-pooling-test".into();
    config.context_tokens = 512;
    config.batch_tokens = 64;
    config.max_sequences = 1;
    config.gpu_layers = -1;
    std::fs::create_dir(&out)?;
    let mut hashes = std::collections::BTreeMap::new();
    hashes.insert(
        "config.json".to_string(),
        write_pooling_json(
            &out,
            "config.json",
            &serde_json::json!({
                "config": config, "timeout_seconds_per_fit": timeout_seconds, "fit_count": 8,
                "search_steps": 0, "role": "exposed_smoke_runtime_only_not_controller_evidence"
            }),
        )?,
    );
    let owner = NativeModelOwner::load(config)?;
    let handle = owner.handle();
    let work = (|| -> Result<_> {
        let base = ResidualTrainingRequest {
            request_id: "pooling".into(),
            model_id: "residual-pooling-test".into(),
            pooling: ResidualPooling::TerminalToken,
            layers: vec![layer],
            train: vec![
                pair(
                    &handle,
                    "train-gathering",
                    "At the gathering, she starts conversations with several visitors.",
                    "At the gathering, she quietly watches from a corner.",
                )?,
                pair(
                    &handle,
                    "train-lunch",
                    "During lunch, she invites colleagues to share her table.",
                    "During lunch, she chooses a table where she sits alone.",
                )?,
            ],
            validation: vec![pair(
                &handle,
                "heldout-weekend",
                "On weekends, she often joins group activities.",
                "On weekends, she often chooses solitary activities.",
            )?],
            search_steps: 0,
            initial_step: 0.25,
            maximum_norm: 0.5,
            l2_penalty: 0.001,
            margin: 0.1,
        };
        let changed_holdout = pair(
            &handle,
            "heldout-break",
            "During breaks, she seeks lively conversation with coworkers.",
            "During breaks, she seeks a quiet place away from coworkers.",
        )?;
        let modes = [
            ("terminal", ResidualPooling::TerminalToken),
            ("matched", ResidualPooling::TerminalTokenMatched),
            ("mean", ResidualPooling::ResponseSpanMean),
        ];
        let mut completed = Vec::new();
        for variant in ["full", "single", "holdout"] {
            for (name, mode) in modes {
                if variant == "holdout" && mode == ResidualPooling::TerminalToken {
                    continue;
                }
                let mut request = base.clone();
                request.request_id = format!("pooling-{variant}-{name}");
                request.pooling = mode;
                if variant == "single" {
                    for pair in request.train.iter_mut().chain(&mut request.validation) {
                        pair.positive.truncate(1);
                        pair.negative.truncate(1);
                    }
                } else if variant == "holdout" {
                    request.validation = vec![changed_holdout.clone()];
                }
                request.validate()?;
                if request
                    .train
                    .iter()
                    .chain(&request.validation)
                    .any(|p| p.prefix.len() + p.positive.len().max(p.negative.len()) > 64)
                {
                    return Err("pooling smoke sequence exceeds fixed 64-token budget".into());
                }
                let request_name = format!("{variant}-{name}-request.json");
                hashes.insert(
                    request_name.clone(),
                    write_pooling_json(&out, &request_name, &request)?,
                );
                let ticket = handle.train_residual(request.clone())?;
                let verified =
                    match ticket.wait_timeout(std::time::Duration::from_secs(timeout_seconds))? {
                        WaitOutcome::Ready(value) => value,
                        WaitOutcome::TimedOut(pending) => {
                            pending.cancel();
                            return Err("pooling fit timed out; cancellation requested".into());
                        }
                    };
                let output = verified.output();
                output.validate_for_request(&request)?;
                if output.gains.iter().any(|&v| v != 0.0)
                    || output.train_baseline != output.train_final
                    || output.validation_baseline != output.validation_final
                {
                    return Err("zero-search pooling fit changed its baseline".into());
                }
                let output_name = format!("{variant}-{name}-output.json");
                hashes.insert(
                    output_name.clone(),
                    write_pooling_json(&out, &output_name, output)?,
                );
                eprintln!(
                    "pooling smoke completed {variant}-{name}; token budget {}",
                    request.estimated_token_evaluations()?
                );
                completed.push(verified);
            }
        }
        for other in [4, 5] {
            if completed[3].output().directions != completed[other].output().directions {
                return Err("single-token pooling directions differ across schedules".into());
            }
        }
        for (original, changed) in [(1, 6), (2, 7)] {
            let (original, changed) = (completed[original].output(), completed[changed].output());
            if original.directions != changed.directions
                || original.gains != changed.gains
                || original.history != changed.history
            {
                return Err("holdout changed fitted pooling results".into());
            }
        }
        Ok(completed)
    })();
    // Join on ordinary work errors as well as success. Timeout is cooperative;
    // model loading and worker joining do not have a hard wall-clock deadline.
    let joined = owner.shutdown_joined()?;
    let completed = work?;
    if joined.expected_worker_count() != joined.joined_worker_count()
        || completed
            .iter()
            .any(|v| !v.belongs_to_joined_model(&joined))
    {
        return Err("pooling owner join verification failed".into());
    }
    let comparison = serde_json::json!({
        "role": "audit_only_exposed_smoke_not_controller_qualification", "fit_count": completed.len(),
        "matched_vs_mean": compare_directions(completed[1].output(), completed[2].output())?,
        "reference_v2_vs_matched_v3": compare_directions(completed[0].output(), completed[1].output())?,
        "single_token_directions_exactly_equal_all_modes": true, "holdout_isolation_matched_and_mean": true,
        "joined_owner_verified": true, "expected_workers": joined.expected_worker_count(), "joined_workers": joined.joined_worker_count(),
        "artifact_sha256": hashes
    });
    let comparison_hash = write_pooling_json(&out, "comparison.json", &comparison)?;
    eprintln!("pooling smoke passed; comparison SHA-256 {comparison_hash}");
    Ok(())
}

fn tokens(handle: &NativeModelHandle, text: &str) -> Result<Vec<i32>> {
    let prepared = handle.prepare_input(GenerationInput::Completion {
        prompts: vec![CompletionPrompt::Text {
            text: text.into(),
            special_tokens: SpecialTokenPolicy::AddBosParseSpecial,
        }],
    })?;
    Ok(prepared
        .first()
        .ok_or("missing prepared prompt")?
        .token_ids
        .clone())
}

fn pair(
    handle: &NativeModelHandle,
    id: &str,
    positive: &str,
    negative: &str,
) -> Result<ResidualTrainingPair> {
    let positive = tokens(handle, positive)?;
    let negative = tokens(handle, negative)?;
    if positive.len() < 2 || negative.len() < 2 || positive[0] != negative[0] {
        return Err("test requires a shared leading BOS token".into());
    }
    let shared = positive
        .iter()
        .zip(&negative)
        .take_while(|(a, b)| a == b)
        .count();
    if shared == positive.len() || shared == negative.len() {
        return Err("test poles must diverge after a shared prefix".into());
    }
    Ok(ResidualTrainingPair {
        id: id.into(),
        prefix: positive[..shared].to_vec(),
        positive: positive[shared..].to_vec(),
        negative: negative[shared..].to_vec(),
    })
}

fn ordinary_generation(handle: &NativeModelHandle, request_id: &str) -> Result<Vec<i32>> {
    let output = handle
        .generate(GenerationRequest {
            request_id: request_id.into(),
            model_id: "residual-test".into(),
            input: GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Text {
                    text: "The room went quiet.".into(),
                    special_tokens: SpecialTokenPolicy::AddBosParseSpecial,
                }],
            },
            sampling: SamplingConfig {
                seed: 20260916,
                temperature: 0.0,
                max_tokens: 8,
                ..SamplingConfig::default()
            },
            media: vec![],
            cached_prefix: None,
        })?
        .wait()?;
    let first = output.first().ok_or("missing generation output")?;
    assert!(first.real_engine_invoked);
    Ok(first.generated_token_ids.clone())
}

#[test]
#[ignore = "requires LLAMA_RESIDUAL_MODEL pointing to a local GGUF"]
fn local_training_holdout_isolation_and_join() -> Result<()> {
    // The test's reference default is explicit in the constructed request.
    // Unknown values fail before any model is loaded.
    let pooling = match std::env::var("LLAMA_RESIDUAL_POOLING").as_deref() {
        Ok("terminal_token") | Err(std::env::VarError::NotPresent) => {
            ResidualPooling::TerminalToken
        }
        Ok("response_span_mean") => ResidualPooling::ResponseSpanMean,
        Ok("terminal_token_matched") => ResidualPooling::TerminalTokenMatched,
        _ => return Err("invalid LLAMA_RESIDUAL_POOLING".into()),
    };
    let path = std::env::var("LLAMA_RESIDUAL_MODEL")?;
    let mut config = NativeModelConfig::local(path.into());
    config.model_id = "residual-test".into();
    config.context_tokens = 512;
    config.batch_tokens = std::env::var("LLAMA_RESIDUAL_BATCH")
        .unwrap_or_else(|_| "64".into())
        .parse()?;
    config.max_sequences = 1;
    let owner = NativeModelOwner::load(config)?;
    let handle = owner.handle();
    let before = ordinary_generation(&handle, "before-training")?;
    let layer: u32 = std::env::var("LLAMA_RESIDUAL_LAYER")
        .unwrap_or_else(|_| "20".into())
        .parse()?;
    let request = ResidualTrainingRequest {
        request_id: "native-residual-fit".into(),
        model_id: "residual-test".into(),
        pooling,
        layers: vec![layer],
        train: vec![
            pair(
                &handle,
                "train-gathering",
                "At the gathering, she starts conversations with several visitors.",
                "At the gathering, she quietly watches from a corner.",
            )?,
            pair(
                &handle,
                "train-lunch",
                "During lunch, she invites colleagues to share her table.",
                "During lunch, she chooses a table where she sits alone.",
            )?,
        ],
        validation: vec![
            pair(
                &handle,
                "heldout-weekend",
                "On weekends, she often joins group activities.",
                "On weekends, she often chooses solitary activities.",
            )?,
            pair(
                &handle,
                "heldout-break",
                "During breaks, she seeks lively conversation with coworkers.",
                "During breaks, she seeks a quiet place away from coworkers.",
            )?,
        ],
        search_steps: 1,
        initial_step: 0.25,
        maximum_norm: 0.5,
        l2_penalty: 0.001,
        margin: 0.1,
    };
    if let Ok(path) = std::env::var("LLAMA_RESIDUAL_TEST_REQUEST") {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        serde_json::to_writer_pretty(file, &request)?;
    }
    let trained = handle.train_residual(request.clone())?.wait()?;
    trained.output().validate_for_request(&request)?;
    assert_eq!(trained.output().pooling, pooling);
    assert!(trained.output().train_final.loss <= trained.output().train_baseline.loss);
    assert_eq!(trained.output().history.len(), 2);

    let axis = ResidualControlProfile {
        id: "axis".into(),
        source_sha256: trained.output().request_sha256.clone(),
        layers: trained.output().layers.clone(),
        directions: trained.output().directions.clone(),
    };
    let mut duplicate = axis.clone();
    duplicate.id = "same-axis".into();
    let heldout = &request.validation[0];
    let evaluation = ResidualEvaluationRequest {
        request_id: "composed-evaluation".into(),
        model_id: request.model_id.clone(),
        expected_model_sha256: trained.output().model_fingerprint.model_sha256.clone(),
        profiles: vec![axis, duplicate],
        maximum_control_norm: 0.6,
        cases: [
            ("zero", [0.0, 0.0]),
            ("positive", [0.25, 0.0]),
            ("negative", [-0.25, 0.0]),
            ("cancelled-sum", [0.25, -0.25]),
        ]
        .into_iter()
        .map(|(id, coefficients)| ResidualEvaluationCase {
            id: id.into(),
            prefix: heldout.prefix.clone(),
            continuation: heldout.positive.clone(),
            coefficients: coefficients.to_vec(),
            maximum_new_tokens: 8,
            seed: 20260916,
        })
        .collect(),
    };
    let evaluated = handle.evaluate_residual(evaluation.clone())?.wait()?;
    evaluated.output().validate_for_request(&evaluation)?;
    let zero = &evaluated.output().cases[0];
    let cancelled_sum = &evaluated.output().cases[3];
    assert!(zero.mean_reference_kl.expect("forced targets have KL") <= 1e-8);
    assert_eq!(zero.generated_token_ids, cancelled_sum.generated_token_ids);
    assert_eq!(zero.generated_bytes, cancelled_sum.generated_bytes);
    assert_eq!(
        zero.mean_continuation_logprob,
        cancelled_sum.mean_continuation_logprob
    );
    assert_eq!(zero.mean_reference_kl, cancelled_sum.mean_reference_kl);
    assert_eq!(cancelled_sum.actual_stack_norm, 0.0);
    for row in &evaluated.output().cases[1..3] {
        assert!((row.actual_stack_norm - 0.25).abs() < 1e-6);
        assert!(row.mean_reference_kl.expect("forced targets have KL") >= 0.0);
    }
    let ordinary_zero = handle
        .generate(GenerationRequest {
            request_id: "ordinary-zero-reference".into(),
            model_id: request.model_id.clone(),
            input: GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Tokens {
                    token_ids: heldout.prefix.clone(),
                }],
            },
            sampling: SamplingConfig {
                temperature: 0.0,
                max_tokens: 8,
                ..SamplingConfig::default()
            },
            media: vec![],
            cached_prefix: None,
        })?
        .wait()?;
    assert_eq!(
        ordinary_zero[0].generated_token_ids,
        zero.generated_token_ids
    );
    assert_eq!(ordinary_zero[0].text.as_bytes(), zero.generated_bytes);
    // Changing forced targets cannot enter free-generation history. Reversing
    // case order also exercises clearing a nonzero control before a zero row.
    let mut reversed = evaluation.clone();
    reversed.request_id = "reverse-and-replay".into();
    reversed.cases.reverse();
    for case in &mut reversed.cases {
        case.continuation = heldout.negative.clone();
    }
    let replay = handle.evaluate_residual(reversed)?.wait()?;
    for row in &replay.output().cases {
        let original = evaluated
            .output()
            .cases
            .iter()
            .find(|value| value.id == row.id)
            .expect("same case set");
        assert_eq!(row.generated_token_ids, original.generated_token_ids);
        assert_eq!(row.generated_bytes, original.generated_bytes);
    }
    let mut forced_only = evaluation.clone();
    forced_only.request_id = "forced-only".into();
    forced_only.cases = vec![forced_only.cases[1].clone()];
    forced_only.cases[0].maximum_new_tokens = 0;
    let forced = handle.evaluate_residual(forced_only)?.wait()?;
    assert_eq!(
        forced.output().cases[0].mean_continuation_logprob,
        evaluated.output().cases[1].mean_continuation_logprob
    );
    assert_eq!(
        forced.output().cases[0].mean_reference_kl,
        evaluated.output().cases[1].mean_reference_kl
    );
    let long_prefix = tokens(
        &handle,
        &"The archivist copied the same sentence into the notebook. ".repeat(18),
    )?;
    assert!(long_prefix.len() > 129 && long_prefix.len() + heldout.positive.len() < 512);
    let mut chunk_boundary = evaluation.clone();
    chunk_boundary.request_id = "multi-chunk-prefix".into();
    chunk_boundary.cases = vec![
        chunk_boundary.cases[0].clone(),
        chunk_boundary.cases[3].clone(),
    ];
    for case in &mut chunk_boundary.cases {
        case.prefix = long_prefix.clone();
    }
    let chunked = handle.evaluate_residual(chunk_boundary)?.wait()?;
    assert_eq!(chunked.output().cases[0].mean_reference_kl, Some(0.0));
    assert_eq!(
        chunked.output().cases[0].generated_token_ids,
        chunked.output().cases[1].generated_token_ids
    );
    assert_eq!(
        chunked.output().cases[0].mean_continuation_logprob,
        chunked.output().cases[1].mean_continuation_logprob
    );
    let mut capture_boundaries = request.clone();
    capture_boundaries.request_id = "multi-chunk-capture".into();
    capture_boundaries.search_steps = 0;
    let context_a = "The archivist copied a line from the notebook. ".repeat(14);
    let context_b = "The archivist copied a line from the notebook. ".repeat(20);
    capture_boundaries.train = vec![
        pair(
            &handle,
            "capture-long-a",
            &format!("{context_a}At lunch, she joins a table of coworkers."),
            &format!("{context_a}At lunch, she finds a table away from coworkers."),
        )?,
        pair(
            &handle,
            "capture-long-b",
            &format!("{context_b}At the party, she starts a conversation."),
            &format!("{context_b}At the party, she waits for a conversation."),
        )?,
    ];
    assert!(
        capture_boundaries
            .train
            .iter()
            .all(|pair| pair.prefix.len() > 129)
    );
    assert_ne!(
        capture_boundaries.train[0].prefix.len(),
        capture_boundaries.train[1].prefix.len()
    );
    let captured = handle.train_residual(capture_boundaries)?.wait()?;
    captured.output().validate()?;
    assert!(
        captured
            .output()
            .directions
            .iter()
            .flatten()
            .all(|value| value.is_finite())
    );
    let mut wrong_model = evaluation.clone();
    wrong_model.request_id = "wrong-model-evaluation".into();
    wrong_model.expected_model_sha256 = "0".repeat(64);
    assert_eq!(
        handle
            .evaluate_residual(wrong_model)?
            .wait()
            .expect_err("model mismatch must fail")
            .code,
        NativeErrorCode::InvalidConfig
    );
    let mut cancellation_probe = evaluation.clone();
    cancellation_probe.request_id = "cancel-evaluation".into();
    let ticket = handle.evaluate_residual(cancellation_probe)?;
    let cancellation_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while handle.status().active_sequences == 0 {
        assert!(
            std::time::Instant::now() < cancellation_deadline,
            "evaluation never entered Running"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    ticket.cancel();
    assert_eq!(
        ticket
            .wait()
            .expect_err("cancelled evaluation must fail")
            .code,
        NativeErrorCode::Cancelled
    );

    let mut holdout_changed = request.clone();
    holdout_changed.request_id = "native-residual-fit-new-holdout".into();
    holdout_changed.validation = vec![pair(
        &handle,
        "new-heldout",
        "She prefers discussing ideas with a large group.",
        "She prefers considering ideas on her own.",
    )?];
    let replicated = handle.train_residual(holdout_changed)?.wait()?;
    assert_eq!(trained.output().directions, replicated.output().directions);
    assert_eq!(trained.output().gains, replicated.output().gains);
    assert_eq!(trained.output().history, replicated.output().history);

    for (id, steps, radius) in [("zero-steps", 0, 0.5), ("zero-radius", 1, 0.0)] {
        let mut zero = request.clone();
        zero.request_id = id.into();
        zero.search_steps = steps;
        zero.maximum_norm = radius;
        let result = handle.train_residual(zero)?.wait()?;
        assert!(result.output().gains.iter().all(|gain| *gain == 0.0));
        assert_eq!(result.output().train_baseline, result.output().train_final);
        assert_eq!(
            result.output().validation_baseline,
            result.output().validation_final
        );
    }

    let after = ordinary_generation(&handle, "after-training")?;
    assert_eq!(before, after, "training altered resident generation state");
    let mut cancelled_request = request;
    cancelled_request.request_id = "cancel-native-residual".into();
    let ticket = handle.train_residual(cancelled_request)?;
    ticket.cancel();
    let error = ticket
        .wait()
        .expect_err("cancelled training must not return a model");
    assert_eq!(error.code, NativeErrorCode::Cancelled);
    let joined = owner.shutdown_joined()?;
    assert!(trained.belongs_to_joined_model(&joined));
    assert!(replicated.belongs_to_joined_model(&joined));
    assert!(evaluated.belongs_to_joined_model(&joined));
    assert!(replay.belongs_to_joined_model(&joined));
    assert!(forced.belongs_to_joined_model(&joined));
    assert!(chunked.belongs_to_joined_model(&joined));
    assert!(captured.belongs_to_joined_model(&joined));
    assert_eq!(joined.expected_worker_count(), joined.joined_worker_count());
    if let Ok(path) = std::env::var("LLAMA_RESIDUAL_TEST_OUTPUT") {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        serde_json::to_writer_pretty(file, trained.output())?;
    }
    if let Ok(path) = std::env::var("LLAMA_RESIDUAL_EVALUATION_OUTPUT") {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        serde_json::to_writer_pretty(file, evaluated.output())?;
    }
    eprintln!(
        "native residual training ({:?}): train loss {} -> {}; heldout loss {} -> {}; gains {:?}; no-op delta {}; holdout invariance, resident isolation, cancellation, and owner join verified",
        pooling,
        trained.output().train_baseline.loss,
        trained.output().train_final.loss,
        trained.output().validation_baseline.loss,
        trained.output().validation_final.loss,
        trained.output().gains,
        trained.output().no_op_max_logprob_delta
    );
    Ok(())
}
