use fte_backend_llama::{BACKEND_ID, LlamaNativeBackend};
use fte_types::{
    BackendLocation, BackendRequest, CacheMode, CacheOutcome, CachePolicy, ContentBlock,
    DeadlinePolicy, GatewayBackend, GatewayRequest, GenerationInput, InputItem, MessageRole,
    ModelSelector, RequestId, ResolvedRoute, ResponseFormat, RoutingPolicy, SamplingOptions,
    StoragePolicy, StreamPolicy, ToolPolicy,
};
use llama_native_host::{NativeHost, NativeHostConfig};
use llama_native_types::{NativeDevice, NativeModelConfig};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

fn request(model_id: &str, input: GenerationInput, cache: CachePolicy) -> BackendRequest {
    let request_id = RequestId::new();
    BackendRequest {
        route: ResolvedRoute {
            backend_id: BACKEND_ID.to_string(),
            model_id: model_id.to_string(),
            display_name: model_id.to_string(),
            location: BackendLocation::LocalEmbedded,
            catalog_version: "real-test".to_string(),
        },
        request: GatewayRequest {
            request_id,
            client_id: "real-adapter-test".to_string(),
            model: ModelSelector::ExactRoute {
                backend_id: BACKEND_ID.to_string(),
                model_id: model_id.to_string(),
            },
            input,
            sampling: SamplingOptions {
                max_output_tokens: Some(12),
                temperature: Some(0.0),
                ..SamplingOptions::default()
            },
            cache,
            response_format: ResponseFormat::Text,
            tools: Vec::new(),
            tool_policy: ToolPolicy::default(),
            routing: RoutingPolicy::default(),
            storage: StoragePolicy::default(),
            deadline: DeadlinePolicy::default(),
            stream: StreamPolicy::default(),
            provider_extensions: BTreeMap::new(),
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires MOM_LLAMA_MODEL_PATH pointing at a real local GGUF"]
async fn real_chat_completion_and_stable_prefix_hit_are_in_process() {
    let Some(model_path) = std::env::var_os("MOM_LLAMA_MODEL_PATH").map(PathBuf::from) else {
        eprintln!("MOM_LLAMA_MODEL_PATH is not set; real adapter proof skipped");
        return;
    };
    let host = Arc::new(NativeHost::new(NativeHostConfig::default()));
    let backend = LlamaNativeBackend::new_borrowed(Arc::clone(&host));
    let mut config = NativeModelConfig::local(model_path);
    config.device = NativeDevice::Cpu;
    config.context_tokens = 4096;
    config.batch_tokens = 256;
    let model_id = config.model_id.clone();
    backend.register_model(config).expect("load real GGUF");

    let stable_system = (0..24)
        .map(|index| {
            format!(
                "Stable persona rule {index}: be precise, concise, and distinguish evidence from uncertainty."
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let chat_input = GenerationInput::Chat {
        items: vec![
            InputItem::Message {
                id: Some("persona-system".to_string()),
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: stable_system,
                }],
            },
            InputItem::Message {
                id: Some("host-message".to_string()),
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "Reply with exactly one short greeting.".to_string(),
                }],
            },
        ],
    };
    let cache = CachePolicy {
        mode: CacheMode::StablePrefix,
        stable_prefix_items: Some(1),
        owner_namespace: Some("persona:real-adapter".to_string()),
        owner_version: Some("1".to_string()),
        ..CachePolicy::default()
    };

    let cold = backend
        .execute(request(&model_id, chat_input.clone(), cache.clone()))
        .await
        .expect("admit cold chat")
        .final_response()
        .await
        .expect("complete cold chat");
    assert!(cold.usage.real_local_inference);
    assert_eq!(
        cold.usage.cache.as_ref().map(|receipt| receipt.outcome),
        Some(CacheOutcome::Miss)
    );

    let warm = backend
        .execute(request(&model_id, chat_input, cache))
        .await
        .expect("admit warm chat")
        .final_response()
        .await
        .expect("complete warm chat");
    assert!(warm.usage.real_local_inference);
    assert_eq!(
        warm.usage.cache.as_ref().map(|receipt| receipt.outcome),
        Some(CacheOutcome::Hit)
    );

    let completion = backend
        .execute(request(
            &model_id,
            GenerationInput::Completion {
                prompts: vec![fte_types::CompletionPrompt::Text {
                    text: "  The exact completion starts".to_string(),
                    add_bos: false,
                }],
            },
            CachePolicy {
                mode: CacheMode::Disabled,
                ..CachePolicy::default()
            },
        ))
        .await
        .expect("admit exact completion")
        .final_response()
        .await
        .expect("complete exact completion");
    assert!(completion.usage.real_local_inference);

    assert_eq!(
        host.slots().len(),
        1,
        "chat, warm-prefix chat, and Completion must reuse one resident model"
    );
    backend
        .shutdown()
        .await
        .expect("the FTE adapter must drain its requests");
    assert_eq!(
        host.slots().len(),
        1,
        "adapter shutdown must not unload its borrowed native host"
    );
    let joined = host.shutdown_for_process_exit();
    assert_eq!(joined.joined_worker_count(), 1);
    assert!(joined.belongs_to(&host));
}
