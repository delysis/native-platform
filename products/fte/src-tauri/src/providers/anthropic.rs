use crate::providers::openai_compatible::OpenAiCompatibleProvider;
use crate::providers::spec::ProviderSpec;
use crate::providers::Capability;

const ANTHROPIC_MESSAGES_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

pub fn provider() -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::from_spec(ProviderSpec::anthropic(
        "anthropic",
        "Anthropic",
        ANTHROPIC_MESSAGES_ENDPOINT,
        vec![
            Capability::Streaming,
            Capability::Tools,
            Capability::Vision,
            Capability::LongContext,
        ],
    ))
}
