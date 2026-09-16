//! Opt-in end-to-end training evidence. Uses only an explicitly supplied local
//! GGUF and authored non-personal contrasts; never downloads models or text.
use llama_native_engine::{NativeModelHandle, NativeModelOwner};
use llama_native_types::{
    CompletionPrompt, GenerationInput, GenerationRequest, NativeErrorCode, NativeModelConfig,
    ResidualTrainingPair, ResidualTrainingRequest, SamplingConfig, SpecialTokenPolicy,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

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
    let path = std::env::var("LLAMA_RESIDUAL_MODEL")?;
    let mut config = NativeModelConfig::local(path.into());
    config.model_id = "residual-test".into();
    config.context_tokens = 512;
    config.batch_tokens = 64;
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
    trained.output().validate()?;
    assert!(trained.output().train_final.loss <= trained.output().train_baseline.loss);
    assert_eq!(trained.output().history.len(), 2);

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
    assert_eq!(joined.expected_worker_count(), joined.joined_worker_count());
    if let Ok(path) = std::env::var("LLAMA_RESIDUAL_TEST_OUTPUT") {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        serde_json::to_writer_pretty(file, trained.output())?;
    }
    eprintln!(
        "native residual training: train loss {} -> {}; heldout loss {} -> {}; gains {:?}; no-op delta {}; holdout invariance, resident isolation, cancellation, and owner join verified",
        trained.output().train_baseline.loss,
        trained.output().train_final.loss,
        trained.output().validation_baseline.loss,
        trained.output().validation_final.loss,
        trained.output().gains,
        trained.output().no_op_max_logprob_delta
    );
    Ok(())
}
