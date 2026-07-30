use serde::Serialize;

use crate::providers::{spec::ParameterPolicy, Capability};
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
    pub parameter_policy: ParameterPolicy,
    pub quota: QuotaSpec,
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
        parameter_policy: parameter_policy_for_provider(provider_id),
        quota,
    }
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
