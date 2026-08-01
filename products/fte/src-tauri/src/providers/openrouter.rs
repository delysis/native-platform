use crate::providers::completions::CompletionProtocol;
use crate::providers::openai_compatible::OpenAiCompatibleProvider;
use crate::providers::spec::ProviderSpec;
use crate::providers::Capability;

const OPENROUTER_CHAT_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const OPENROUTER_COMPLETIONS_ENDPOINT: &str = "https://openrouter.ai/api/v1/completions";

pub fn provider() -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::from_spec(
        ProviderSpec::openai_compatible(
            "openrouter",
            "OpenRouter",
            OPENROUTER_CHAT_ENDPOINT,
            vec![Capability::Streaming, Capability::Vision, Capability::Tools],
        )
        .with_static_header("http-referer", "https://free-token-energy.local")
        .with_static_header("x-title", "Free Token Energy"),
    )
    .with_completion_endpoint(OPENROUTER_COMPLETIONS_ENDPOINT, CompletionProtocol::OpenAi)
}
