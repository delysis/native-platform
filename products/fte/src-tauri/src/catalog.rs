use serde::Serialize;

use crate::providers::{
    spec::ParameterPolicy, Capability, CompletionPromptKind, CompletionRequest,
};
use crate::rate_limiter::QuotaWindows;

#[derive(Debug, Clone, Serialize)]
pub struct QuotaSpec {
    pub rpm: u32,
    pub rpd: u32,
    pub tpm: u32,
    pub tpd: u32,
    pub documented: bool,
}

impl QuotaSpec {
    pub fn windows(&self) -> QuotaWindows {
        QuotaWindows::new(self.rpm, self.rpd, self.tpm, self.tpd)
    }

    pub fn has_documented_limit(&self) -> bool {
        self.documented
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelCatalogEntry {
    pub provider_id: String,
    pub provider_name: String,
    pub public_model_id: String,
    pub provider_model_id: String,
    pub display_name: String,
    pub capabilities: Vec<Capability>,
    pub chat_completions: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_completions: Option<TextCompletionSupport>,
    pub parameter_policy: ParameterPolicy,
    pub quota: QuotaSpec,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TextCompletionSupport {
    pub prompt_semantics: PromptSemantics,
    pub prompt_types: Vec<CompletionPromptKind>,
    pub supported_parameters: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PromptSemantics {
    DirectContinuation,
    FillInMiddle,
    ProviderNativeUnverified,
    LegacyPromptProtocol,
}

impl TextCompletionSupport {
    pub fn supports(&self, request: &CompletionRequest) -> bool {
        self.prompt_types.contains(&request.prompt.kind())
            && request.requested_parameters().iter().all(|parameter| {
                self.supported_parameters
                    .iter()
                    .any(|item| item == parameter)
            })
    }

    pub fn incompatibilities(&self, request: &CompletionRequest) -> Vec<String> {
        let mut incompatible = Vec::new();
        if !self.prompt_types.contains(&request.prompt.kind()) {
            incompatible.push(format!("prompt type {:?}", request.prompt.kind()));
        }
        incompatible.extend(
            request
                .requested_parameters()
                .into_iter()
                .filter(|parameter| {
                    !self
                        .supported_parameters
                        .iter()
                        .any(|supported| supported == parameter)
                })
                .map(ToString::to_string),
        );
        incompatible
    }
}

impl ModelCatalogEntry {
    pub fn matches_requested_model(&self, requested_model: &str) -> bool {
        let requested = requested_model.trim();
        requested.eq_ignore_ascii_case("auto")
            || requested.eq_ignore_ascii_case("best")
            || requested.eq_ignore_ascii_case("free/auto")
            || requested == self.public_model_id
            || requested == self.provider_model_id
    }
}

pub fn default_model_catalog() -> Vec<ModelCatalogEntry> {
    vec![
        // OpenRouter's free router follows the provider's live free-model
        // catalog, avoiding hard-coded model slugs that disappear frequently.
        model(
            "openrouter",
            "OpenRouter",
            "openrouter-free",
            "openrouter/free",
            "OpenRouter Free Models Router",
            vec![Capability::Streaming, Capability::Tools, Capability::Vision],
            documented_quota(10, 50, u32::MAX, u32::MAX),
        ),
        model(
            "groq",
            "Groq Cloud",
            "llama-3.3-70b-versatile",
            "llama-3.3-70b-versatile",
            "Llama 3.3 70B Versatile",
            vec![Capability::Streaming, Capability::Tools],
            documented_quota(30, 1_000, 12_000, 100_000),
        ),
        model(
            "groq",
            "Groq Cloud",
            "llama-3.1-8b-instant",
            "llama-3.1-8b-instant",
            "Llama 3.1 8B Instant",
            vec![Capability::Streaming, Capability::Tools],
            documented_quota(30, 14_400, 6_000, 500_000),
        ),
        model(
            "anthropic",
            "Anthropic",
            "claude-opus-5",
            "claude-opus-5",
            "Claude Opus 5",
            rich_capabilities(),
            unknown_quota(),
        ),
        model(
            "anthropic",
            "Anthropic",
            "claude-sonnet-5",
            "claude-sonnet-5",
            "Claude Sonnet 5",
            rich_capabilities(),
            unknown_quota(),
        ),
        model(
            "anthropic",
            "Anthropic",
            "claude-haiku-4.5",
            "claude-haiku-4-5-20251001",
            "Claude Haiku 4.5",
            rich_capabilities(),
            unknown_quota(),
        ),
        model(
            "gemini",
            "Google Gemini",
            "gemini-2.5-pro",
            "gemini-2.5-pro",
            "Gemini 2.5 Pro",
            rich_capabilities(),
            unknown_quota(),
        ),
        model(
            "gemini",
            "Google Gemini",
            "gemini-2.5-flash",
            "gemini-2.5-flash",
            "Gemini 2.5 Flash",
            rich_capabilities(),
            unknown_quota(),
        ),
        model(
            "gemini",
            "Google Gemini",
            "gemini-2.5-flash-lite",
            "gemini-2.5-flash-lite",
            "Gemini 2.5 Flash-Lite",
            rich_capabilities(),
            unknown_quota(),
        ),
        model(
            "mistral",
            "Mistral AI",
            "mistral-small-latest",
            "mistral-small-latest",
            "Mistral Small",
            vec![Capability::Streaming, Capability::Tools],
            unknown_quota(),
        ),
        completion_only_model(
            model(
                "mistral",
                "Mistral AI",
                "codestral-latest",
                "codestral-latest",
                "Codestral",
                vec![Capability::Streaming],
                unknown_quota(),
            ),
            TextCompletionSupport::mistral_fim(),
        ),
        model(
            "nvidia",
            "NVIDIA NIM",
            "llama-3.1-70b-instruct",
            "meta/llama-3.1-70b-instruct",
            "Llama 3.1 70B Instruct",
            vec![Capability::Streaming, Capability::Tools],
            unknown_quota(),
        ),
        model(
            "cerebras",
            "Cerebras",
            "gpt-oss-120b",
            "gpt-oss-120b",
            "GPT OSS 120B",
            vec![Capability::Streaming, Capability::Tools],
            documented_quota(5, u32::MAX, 30_000, 1_000_000),
        ),
    ]
}

fn rich_capabilities() -> Vec<Capability> {
    vec![
        Capability::Streaming,
        Capability::Tools,
        Capability::Vision,
        Capability::LongContext,
    ]
}

fn documented_quota(rpm: u32, rpd: u32, tpm: u32, tpd: u32) -> QuotaSpec {
    QuotaSpec {
        rpm,
        rpd,
        tpm,
        tpd,
        documented: true,
    }
}

fn unknown_quota() -> QuotaSpec {
    QuotaSpec {
        rpm: u32::MAX,
        rpd: u32::MAX,
        tpm: u32::MAX,
        tpd: u32::MAX,
        documented: false,
    }
}

fn model(
    provider_id: &str,
    provider_name: &str,
    public_model_id: &str,
    provider_model_id: &str,
    display_name: &str,
    capabilities: Vec<Capability>,
    quota: QuotaSpec,
) -> ModelCatalogEntry {
    ModelCatalogEntry {
        provider_id: provider_id.to_string(),
        provider_name: provider_name.to_string(),
        public_model_id: public_model_id.to_string(),
        provider_model_id: provider_model_id.to_string(),
        display_name: display_name.to_string(),
        capabilities,
        parameter_policy: parameter_policy_for_provider(provider_id),
        chat_completions: true,
        text_completions: text_completion_support(provider_id, provider_model_id),
        quota,
    }
}

fn completion_only_model(
    mut entry: ModelCatalogEntry,
    text_completions: TextCompletionSupport,
) -> ModelCatalogEntry {
    entry.chat_completions = false;
    entry.text_completions = Some(text_completions);
    entry
}

fn text_completion_support(
    provider_id: &str,
    _provider_model_id: &str,
) -> Option<TextCompletionSupport> {
    match provider_id {
        "cerebras" => Some(TextCompletionSupport::cerebras()),
        _ => None,
    }
}

impl TextCompletionSupport {
    fn mistral_fim() -> Self {
        Self {
            prompt_semantics: PromptSemantics::FillInMiddle,
            prompt_types: vec![CompletionPromptKind::Text],
            supported_parameters: parameters(&[
                "stream",
                "temperature",
                "max_tokens",
                "metadata",
                "min_tokens",
                "prompt_cache_key",
                "seed",
                "stop",
                "suffix",
                "top_p",
            ]),
        }
    }

    fn cerebras() -> Self {
        Self {
            prompt_semantics: PromptSemantics::DirectContinuation,
            prompt_types: all_prompt_types(),
            supported_parameters: parameters(&[
                "stream",
                "temperature",
                "max_tokens",
                "echo",
                "grammar_root",
                "logprobs",
                "min_tokens",
                "prompt_cache_key",
                "return_raw_tokens",
                "seed",
                "stop",
                "top_p",
                "user",
            ]),
        }
    }
}

fn all_prompt_types() -> Vec<CompletionPromptKind> {
    vec![
        CompletionPromptKind::Text,
        CompletionPromptKind::Texts,
        CompletionPromptKind::Tokens,
        CompletionPromptKind::TokenBatches,
    ]
}

fn parameters(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn parameter_policy_for_provider(provider_id: &str) -> ParameterPolicy {
    match provider_id {
        "anthropic" => ParameterPolicy::anthropic(),
        "gemini" => ParameterPolicy::gemini(),
        "mistral" => ParameterPolicy::mistral(),
        "nvidia" | "cerebras" => ParameterPolicy::max_completion_tokens_to_max_tokens(),
        _ => ParameterPolicy::openai_compatible(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_completion_matrix_is_explicit_in_the_catalog() {
        let catalog = default_model_catalog();
        let find = |provider: &str, model: &str| {
            catalog
                .iter()
                .find(|entry| entry.provider_id == provider && entry.provider_model_id == model)
                .unwrap()
        };

        assert!(find("openrouter", "openrouter/free")
            .text_completions
            .is_none());
        assert!(find("groq", "llama-3.3-70b-versatile")
            .text_completions
            .is_none());
        assert!(find("gemini", "gemini-2.5-flash")
            .text_completions
            .is_none());
        assert!(find("mistral", "mistral-small-latest")
            .text_completions
            .is_none());
        assert!(find("nvidia", "meta/llama-3.1-70b-instruct")
            .text_completions
            .is_none());

        assert!(find("anthropic", "claude-sonnet-5")
            .text_completions
            .is_none());
        assert_eq!(
            find("mistral", "codestral-latest")
                .text_completions
                .as_ref()
                .unwrap()
                .prompt_semantics,
            PromptSemantics::FillInMiddle
        );
        assert!(!find("mistral", "codestral-latest").chat_completions);
        assert_eq!(
            find("cerebras", "gpt-oss-120b")
                .text_completions
                .as_ref()
                .unwrap()
                .prompt_semantics,
            PromptSemantics::DirectContinuation
        );
    }
}
