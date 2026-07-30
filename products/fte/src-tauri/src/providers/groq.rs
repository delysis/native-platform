use crate::providers::openai_compatible::OpenAiCompatibleProvider;
use crate::providers::spec::ProviderSpec;
use crate::providers::Capability;

const GROQ_CHAT_ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";

pub fn provider() -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::from_spec(ProviderSpec::openai_compatible(
        "groq",
        "Groq",
        GROQ_CHAT_ENDPOINT,
        vec![Capability::Streaming, Capability::Tools],
    ))
}
