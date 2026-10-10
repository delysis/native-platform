//! Opt-in qualification through the production resident worker, not a fake executor.
//! This proves one native host only; it is not physical-network qualification.
use llama_native_engine::{NativeModelOwner, WaitOutcome};
use llama_native_types::{
    CompletionPrompt, GenerationBatchRequest, GenerationCase, GenerationInput, GenerationState,
    NativeDevice, NativeModelConfig, SamplingConfig, SpecialTokenPolicy,
};
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn request(model_id: &str, id: &str, tokens: Vec<i32>, maximum: u32) -> GenerationBatchRequest {
    GenerationBatchRequest {
        request_id: id.to_owned(),
        model_id: model_id.to_owned(),
        media: Vec::new(),
        first_word_choices: None,
        cases: vec![GenerationCase {
            // Deliberately equal across owners: identity must not be case-id-only.
            case_id: "answer".to_owned(),
            input: GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Tokens { token_ids: tokens }],
            },
            sampling: SamplingConfig {
                seed: 91,
                temperature: 0.0,
                max_tokens: maximum,
                ..SamplingConfig::default()
            },
            cached_prefix: None,
        }],
    }
}

#[test]
#[ignore = "requires MOM_LLAMA_MODEL_PATH, MOM_LLAMA_MODEL_SHA256, and a real local GGUF"]
fn real_independent_requests_share_decode_and_cancel_separately() -> Result<()> {
    let mut config = NativeModelConfig::local(std::env::var("MOM_LLAMA_MODEL_PATH")?.into());
    let expected_model = std::env::var("MOM_LLAMA_MODEL_SHA256")?;
    config.expected_model_sha256 = Some(expected_model.clone());
    config.device = NativeDevice::Cpu;
    config.context_tokens = 4096;
    config.batch_tokens = 128;
    config.max_sequences = 4;
    let owner = NativeModelOwner::load(config)?;
    let first_handle = owner.handle();
    let second_handle = owner.handle();
    assert!(first_handle.is_same_worker(&second_handle));
    let fingerprint = first_handle
        .status()
        .fingerprint
        .expect("resident fingerprint");
    assert_eq!(fingerprint.model_sha256, expected_model);

    let prompts = first_handle.prepare_input(GenerationInput::Completion {
        prompts: vec![
            CompletionPrompt::Text {
                text: "Continue this numbered list to one thousand, without commentary:\n1. one\n2. two\n3.".to_owned(),
                special_tokens: SpecialTokenPolicy::AddBosParseSpecial,
            },
            CompletionPrompt::Text {
                text: "The moon rose above the quiet harbor, and the old sailor".to_owned(),
                special_tokens: SpecialTokenPolicy::AddBosParseSpecial,
            },
        ],
    })?;
    let first_request = request(
        &fingerprint.model_id,
        "owner-a",
        prompts[0].token_ids.clone(),
        1024,
    );
    let second_request = request(
        &fingerprint.model_id,
        "owner-b",
        prompts[1].token_ids.clone(),
        64,
    );
    let isolated = first_handle
        .generate_batch(request(
            &fingerprint.model_id,
            "isolated-b",
            prompts[1].token_ids.clone(),
            64,
        ))?
        .wait_verified()?;
    let observer = first_handle.observe_cooperative_batches()?;
    let first = first_handle.generate_cooperative(first_request.clone())?;
    let deadline = Instant::now() + Duration::from_secs(120);

    // Do not submit B until an actual successful native decode has consumed an
    // output token for A. A shared prefill or two queued commands is insufficient.
    let first_decode = loop {
        assert!(
            Instant::now() < deadline,
            "first native decode did not arrive"
        );
        if let Some(sample) = observer.receive_timeout(Duration::from_millis(100))?
            && sample
                .members()
                .iter()
                .any(|member| member.request_id() == "owner-a" && member.decode_tokens() > 0)
        {
            break sample.ordinal();
        }
    };
    let second = second_handle.generate_cooperative(second_request.clone())?;
    let shared = loop {
        assert!(
            Instant::now() < deadline,
            "late request never shared a native decode"
        );
        if let Some(sample) = observer.receive_timeout(Duration::from_millis(100))? {
            let has = |id| {
                sample
                    .members()
                    .iter()
                    .any(|member| member.request_id() == id && member.decode_tokens() > 0)
            };
            if has("owner-a") && has("owner-b") {
                assert!(sample.ordinal() > first_decode);
                break sample;
            }
        }
    };
    assert_eq!(
        observer.dropped_samples(),
        0,
        "incomplete trace cannot qualify execution"
    );
    assert!(first.cancel_branch("answer"));

    let first = match first.wait_verified_timeout(Duration::from_secs(120))? {
        WaitOutcome::Ready(value) => value,
        WaitOutcome::TimedOut(ticket) => {
            ticket.cancel_all();
            return Err("first native owner did not settle after cancellation".into());
        }
    };
    let second = match second.wait_verified_timeout(Duration::from_secs(120))? {
        WaitOutcome::Ready(value) => value,
        WaitOutcome::TimedOut(ticket) => {
            ticket.cancel_all();
            return Err("second native owner did not settle independently".into());
        }
    };
    assert_eq!(first.request(), &first_request);
    assert_eq!(second.request(), &second_request);
    assert_eq!(first.outputs()[0].state, GenerationState::Cancelled);
    assert_eq!(second.outputs()[0].state, GenerationState::Completed);
    assert!(!second.outputs()[0].generated_token_ids.is_empty());
    assert_eq!(
        second.outputs()[0].generated_token_ids,
        isolated.outputs()[0].generated_token_ids
    );
    assert_eq!(second.outputs()[0].text, isolated.outputs()[0].text);
    for seal in [&first, &second] {
        assert_eq!(seal.model_fingerprint(), &fingerprint);
        assert_eq!(seal.outputs().len(), 1);
        assert_eq!(seal.outputs()[0].input_index, 0);
        assert_eq!(seal.outputs()[0].metrics.shared_prefix_tokens, 0);
        assert_eq!(seal.outputs()[0].metrics.cache.resident_prefix_tokens, 0);
        assert_eq!(
            seal.outputs()[0].metrics.cache.batch_shared_prefix_tokens,
            0
        );
        let trace = &seal.token_piece_traces()[0];
        assert_eq!(
            trace.cumulative_boundaries().len(),
            seal.outputs()[0].generated_token_ids.len() + 1
        );
        assert!(seal.events().iter().all(|event| {
            event.request_id == seal.request().request_id
                && event.input_index == 0
                && event.sequence_id == 0
        }));
    }

    // Settling a request must release only its own sequence, not the resident.
    let third_request = request(
        &fingerprint.model_id,
        "owner-c",
        prompts[1].token_ids.clone(),
        8,
    );
    let third = second_handle
        .generate_cooperative(third_request)?
        .wait_verified()?;
    assert_eq!(third.outputs()[0].state, GenerationState::Completed);
    assert_eq!(
        second_handle.status().fingerprint.as_ref(),
        Some(&fingerprint)
    );
    let joined = owner.shutdown_joined()?;
    assert!(joined.belongs_to(&first_handle));
    assert_eq!(joined.expected_worker_count(), 1);
    assert_eq!(joined.joined_worker_count(), 1);
    println!(
        "cooperative native qualification: model={} build={} first_decode={} shared_decode={} members={:?}; physical_network=false",
        fingerprint.model_sha256,
        fingerprint.build_id,
        first_decode,
        shared.ordinal(),
        shared.members()
    );
    Ok(())
}
