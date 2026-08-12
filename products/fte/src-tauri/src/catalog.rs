use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Capability {
    Vision,
    Tools,
    LongContext,
    Streaming,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionPromptKind {
    Text,
    Texts,
    Tokens,
    TokenBatches,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuotaSpec {
    pub rpm: u32,
    pub rpd: u32,
    pub tpm: u32,
    pub tpd: u32,
    pub documented: bool,
}

impl QuotaSpec {
    pub fn has_documented_limit(&self) -> bool {
        self.documented
    }

    /// Whether dispatch must reserve a finite local quota window.
    /// Unknown and inherently unmetered backends use neutral routing headroom without invented
    /// limits, while request activity remains available through the ordinary request log.
    pub fn has_enforced_limit(&self) -> bool {
        self.documented
            && [self.rpm, self.rpd, self.tpm, self.tpd]
                .into_iter()
                .any(|limit| limit < u32::MAX)
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
        chat_completions: true,
        text_completions: text_completion_support(provider_id, provider_model_id),
        quota,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_enforcement_requires_a_documented_finite_limit() {
        assert!(!unknown_quota().has_enforced_limit());
        assert!(documented_quota(10, u32::MAX, u32::MAX, u32::MAX).has_enforced_limit());
        assert!(
            !QuotaSpec {
                rpm: 10,
                rpd: u32::MAX,
                tpm: u32::MAX,
                tpd: u32::MAX,
                documented: false,
            }
            .has_enforced_limit()
        );
    }

    #[test]
    fn provider_completion_matrix_is_explicit_in_the_catalog() {
        let catalog = default_model_catalog();
        let find = |provider: &str, model: &str| {
            catalog
                .iter()
                .find(|entry| entry.provider_id == provider && entry.provider_model_id == model)
                .unwrap()
        };

        assert!(
            find("openrouter", "openrouter/free")
                .text_completions
                .is_none()
        );
        assert!(
            find("groq", "llama-3.3-70b-versatile")
                .text_completions
                .is_none()
        );
        assert!(
            find("gemini", "gemini-2.5-flash")
                .text_completions
                .is_none()
        );
        assert!(
            find("mistral", "mistral-small-latest")
                .text_completions
                .is_none()
        );
        assert!(
            find("nvidia", "meta/llama-3.1-70b-instruct")
                .text_completions
                .is_none()
        );

        assert!(
            find("anthropic", "claude-sonnet-5")
                .text_completions
                .is_none()
        );
        assert!(catalog.iter().all(|entry| {
            entry
                .text_completions
                .as_ref()
                .is_none_or(|support| support.prompt_semantics != PromptSemantics::FillInMiddle)
        }));
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
