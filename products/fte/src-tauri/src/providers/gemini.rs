use crate::providers::Capability;
use crate::providers::openai_compatible::OpenAiCompatibleProvider;
use crate::providers::spec::ProviderSpec;

const GEMINI_API_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

pub fn provider() -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::from_spec(ProviderSpec::gemini(
        "gemini",
        "Google Gemini",
        GEMINI_API_BASE_URL,
        vec![
            Capability::Streaming,
            Capability::Tools,
            Capability::Vision,
            Capability::LongContext,
        ],
    ))
}
