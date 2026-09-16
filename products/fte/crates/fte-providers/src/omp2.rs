//! Selected OMP² adaptations; see third-party/omp2/import.json and its MIT notice.
//! Upstream input tokens exclude cache reads. FTE's input tokens are inclusive;
//! retain that contract and preserve absent counters instead of inventing zeros.
use crate::{HostedAuth, HostedProviderConfig};
use fte_types::{
    CacheOutcome, CacheReceipt, CacheTier, GatewayUsage, ResolvedRoute, UsageProvenance,
};
use fte_types::{GatewayError, ModelDescriptor, RequestId};
use serde_json::Value;

const PROVIDERS: &str = include_str!(
    "../../../../../third-party/omp2/upstream/fixtures/llm-oracle/catalog/providers.toml"
);
const COMPAT: &str = include_str!(
    "../../../../../third-party/omp2/upstream/fixtures/llm-oracle/catalog-policy/compat-profiles.json"
);

/// Reviewed Chat wire constraints. Only the pinned catalog factory populates
/// these profiles; a user-chosen provider name does not select upstream policy.
#[derive(Debug, Clone)]
pub struct OpenAiChatCompatibility {
    usage_in_streaming: bool,
    penalties: bool,
    tool_choice: bool,
    tool_strict_mode: String,
    max_tokens_field: String,
}

pub(crate) fn chat_body(
    config: &HostedProviderConfig,
    request: &fte_types::BackendRequest,
    mut body: Value,
) -> Result<Value, GatewayError> {
    let Some(profile) = config.chat_compatibility.get(&request.route.model_id) else {
        return Ok(body);
    };
    let unsupported = |detail| GatewayError {
        code: "omp2_wire_feature_unsupported".into(),
        class: fte_types::ErrorClass::Capability,
        retryable: false,
        http_status: 422,
        request_id: request.request.request_id.clone(),
        provider: Some(config.id.clone()),
        safe_detail: detail,
    };
    let object = body
        .as_object_mut()
        .ok_or_else(|| unsupported("invalid chat body".into()))?;
    if !profile.usage_in_streaming {
        object.remove("stream_options");
    }
    if !profile.penalties
        && (object.contains_key("frequency_penalty") || object.contains_key("presence_penalty"))
    {
        return Err(unsupported(
            "the reviewed provider profile does not support penalties".into(),
        ));
    }
    if !profile.tool_choice && object.contains_key("tool_choice") {
        return Err(unsupported(
            "the reviewed model profile does not support tool choice".into(),
        ));
    }
    if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            let strict = tool
                .pointer("/function/strict")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if (profile.tool_strict_mode == "none" && strict)
                || (profile.tool_strict_mode == "all_strict" && !strict)
            {
                return Err(unsupported(
                    "tool strictness does not match the reviewed provider contract".into(),
                ));
            }
            if profile.tool_strict_mode == "none"
                && let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut)
            {
                function.remove("strict");
            }
        }
    }
    if let Some(tokens) = object.remove("max_tokens") {
        object.insert(profile.max_tokens_field.clone(), tokens);
    }
    Ok(body)
}

const IMPORT: &str = include_str!("../../../../../third-party/omp2/import.json");

/// Immutable source identity for embedding applications' route receipts.
pub fn omp2_catalog_version() -> Result<String, GatewayError> {
    let error = || {
        GatewayError::invalid_request(
            &RequestId::new(),
            "omp2_manifest_invalid",
            "the embedded provider source manifest is invalid",
        )
    };
    let manifest: Value = serde_json::from_str(IMPORT).map_err(|_| error())?;
    let revision = manifest
        .get("revision")
        .and_then(Value::as_str)
        .ok_or_else(error)?;
    if revision.len() != 40
        || !revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(error());
    }
    Ok(format!("omp2:{revision}"))
}

impl HostedProviderConfig {
    /// Builds one reviewed API-key profile from the pinned OMP² catalog.
    /// Models, credentials and registration authority remain caller-owned.
    /// No environment lookup, OAuth flow or network discovery takes place.
    pub fn from_omp2(
        id: &str,
        display_name: impl Into<String>,
        secret_id: impl Into<String>,
        models: Vec<ModelDescriptor>,
    ) -> Result<Self, GatewayError> {
        let error = || {
            GatewayError::invalid_request(
                &RequestId::new(),
                "omp2_profile_unavailable",
                "the pinned upstream profile has no reviewed FTE adapter",
            )
        };
        if !matches!(
            id,
            "openai"
                | "anthropic"
                | "gemini"
                | "groq"
                | "openrouter"
                | "mistral"
                | "nvidia"
                | "cerebras"
        ) {
            return Err(error());
        }
        let catalog = toml::from_str::<toml::Value>(PROVIDERS).map_err(|_| error())?;
        let source_id = if id == "gemini" { "google" } else { id };
        let profile = catalog
            .get("providers")
            .and_then(|p| p.get(source_id))
            .ok_or_else(error)?;
        let base = profile
            .get("base_url")
            .and_then(toml::Value::as_str)
            .ok_or_else(error)?;
        let url = reqwest::Url::parse(base).map_err(|_| error())?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(error());
        }
        let base = base.trim_end_matches('/');
        let name = display_name.into();
        let secret = secret_id.into();
        let transport = profile
            .get("transport")
            .and_then(toml::Value::as_str)
            .ok_or_else(error)?;
        let mut config = match (id, transport) {
            ("openai", "open-ai-responses") => {
                let mut config = Self::openai(id, name, secret, models);
                config.endpoints.responses = Some(format!("{base}/responses"));
                config.endpoints.chat_completions = Some(format!("{base}/chat/completions"));
                // Raw completion is our explicit extension, not an OMP capability.
                config.endpoints.completions = Some(format!("{base}/completions"));
                config
            }
            ("anthropic", "anthropic-messages") => {
                let mut config = Self::anthropic(id, name, secret, models);
                config.endpoints.messages = Some(format!("{base}/v1/messages"));
                config.endpoints.count_tokens = Some(format!("{base}/v1/messages/count_tokens"));
                let version = profile
                    .get("headers")
                    .and_then(|h| h.get("anthropic-version"))
                    .and_then(toml::Value::as_str)
                    .ok_or_else(error)?;
                config
                    .static_headers
                    .insert("anthropic-version".into(), version.into());
                config
            }
            ("gemini", "google-gen-ai") => {
                let mut config = Self::gemini(id, name, secret, models);
                config.endpoints.messages = Some(base.into());
                config.endpoints.count_tokens = Some(base.into());
                config
            }
            (_, "open-ai-chat") => Self::openai_compatible(
                id,
                name,
                secret,
                format!("{base}/chat/completions"),
                models,
            ),
            _ => return Err(error()),
        };
        let auth = profile.get("auth").ok_or_else(error)?;
        config.auth = match auth.get("type").and_then(toml::Value::as_str) {
            Some("bearer") => HostedAuth::Bearer,
            Some("header") => HostedAuth::Header {
                name: auth
                    .get("name")
                    .and_then(toml::Value::as_str)
                    .ok_or_else(error)?
                    .into(),
                prefix: String::new(),
            },
            // Deliberate local difference: keep the API key out of URL logs.
            Some("query")
                if id == "gemini"
                    && auth.get("param").and_then(toml::Value::as_str) == Some("key") =>
            {
                HostedAuth::Header {
                    name: "x-goog-api-key".into(),
                    prefix: String::new(),
                }
            }
            _ => return Err(error()),
        };
        if transport == "open-ai-chat" {
            let profiles: Value = serde_json::from_str(COMPAT).map_err(|_| error())?;
            let profiles = profiles
                .get("profiles")
                .and_then(Value::as_array)
                .ok_or_else(error)?;
            let compat = profile.get("compat");
            let flag = |name| {
                compat
                    .and_then(|c| c.get(name))
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(true)
            };
            let field = |name, default: &str| {
                compat
                    .and_then(|c| c.get(name))
                    .and_then(toml::Value::as_str)
                    .unwrap_or(default)
                    .to_owned()
            };
            for model in &config.models {
                let mut wire = OpenAiChatCompatibility {
                    usage_in_streaming: flag("usage_in_streaming"),
                    penalties: flag("penalties"),
                    tool_choice: true,
                    tool_strict_mode: field("tool_strict_mode", "optional"),
                    max_tokens_field: field("max_tokens_field", "max_completion_tokens"),
                };
                if !matches!(
                    wire.max_tokens_field.as_str(),
                    "max_tokens" | "max_completion_tokens"
                ) || !matches!(
                    wire.tool_strict_mode.as_str(),
                    "optional" | "none" | "all_strict"
                ) {
                    return Err(error());
                }
                let qualified = format!("{source_id}/{}", model.id);
                for profile in profiles {
                    if profile
                        .get("models")
                        .and_then(Value::as_array)
                        .is_some_and(|models| {
                            models
                                .iter()
                                .any(|model| model.as_str() == Some(&qualified))
                        })
                    {
                        let shape = profile.get("shape").ok_or_else(error)?;
                        if let Some(value) = shape
                            .get("wire/supports_usage_in_streaming")
                            .and_then(Value::as_bool)
                        {
                            wire.usage_in_streaming = value;
                        }
                        if let Some(value) = shape
                            .get("wire/supports_tool_choice")
                            .and_then(Value::as_bool)
                        {
                            wire.tool_choice = value;
                        }
                    }
                }
                config.chat_compatibility.insert(model.id.clone(), wire);
            }
        }
        config.catalog_version = omp2_catalog_version()?;
        Ok(config)
    }
}

fn counter(value: &Value, paths: &[&str]) -> Option<u64> {
    paths
        .iter()
        .filter_map(|path| value.pointer(path).and_then(Value::as_u64))
        .max()
}

pub(crate) fn usage(value: &Value, route: ResolvedRoute) -> GatewayUsage {
    let mut result = GatewayUsage::default();
    merge_usage(&mut result, value, route);
    result
}

pub(crate) fn merge_usage(result: &mut GatewayUsage, value: &Value, route: ResolvedRoute) {
    let input = counter(value, &["/input_tokens", "/prompt_tokens"]);
    let output = counter(value, &["/output_tokens", "/completion_tokens"]);
    let reasoning = counter(
        value,
        &[
            "/output_tokens_details/reasoning_tokens",
            "/completion_tokens_details/reasoning_tokens",
        ],
    );
    let cached = counter(
        value,
        &[
            "/input_tokens_details/cached_tokens",
            "/prompt_tokens_details/cached_tokens",
            "/cached_tokens",
            "/prompt_cache_hit_tokens",
            "/cachedContentTokenCount",
        ],
    );
    let mut written = counter(
        value,
        &[
            "/input_tokens_details/cache_write_tokens",
            "/prompt_tokens_details/cache_write_tokens",
        ],
    );
    if value
        .get("prompt_cache_hit_tokens")
        .and_then(Value::as_u64)
        .is_some_and(|count| count > 0)
    {
        written = written.max(counter(value, &["/prompt_cache_miss_tokens"]));
    }
    result.input_tokens = input.or(result.input_tokens);
    result.output_tokens = output.or(result.output_tokens);
    result.reasoning_tokens = reasoning.or(result.reasoning_tokens);
    result.cache_read_tokens = cached.or(result.cache_read_tokens);
    result.cache_write_tokens = written.or(result.cache_write_tokens);
    if [input, output, reasoning, cached, written]
        .iter()
        .any(Option::is_some)
    {
        result.provenance = UsageProvenance::Exact;
    }
    result.selected_route = Some(route);
    // Unknown cache usage is not a measured miss.
    if let Some(count) = result.cache_read_tokens {
        result.cache = Some(CacheReceipt {
            tier: CacheTier::ProviderNative,
            outcome: if count > 0 {
                CacheOutcome::Hit
            } else {
                CacheOutcome::Miss
            },
            reason: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fte_types::BackendLocation;
    use serde_json::json;

    fn route() -> ResolvedRoute {
        ResolvedRoute {
            backend_id: "fixture".into(),
            model_id: "model".into(),
            display_name: "Model".into(),
            location: BackendLocation::Hosted,
            catalog_version: "test".into(),
        }
    }

    #[test]
    fn upstream_split_usage_preserves_counts_and_does_not_double_count_cache() {
        let fixture = include_str!(
            "../../../../../third-party/omp2/upstream/fixtures/llm-oracle/openai/chat/stream.parity.sse"
        );
        let mut result = GatewayUsage::default();
        for line in fixture
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
        {
            if let Ok(value) = serde_json::from_str::<Value>(line)
                && let Some(usage) = value.get("usage")
            {
                merge_usage(&mut result, usage, route());
            }
        }
        assert_eq!(result.input_tokens, Some(10));
        assert_eq!(result.output_tokens, Some(4));
        assert_eq!(result.reasoning_tokens, Some(2));
        assert_eq!(result.cache_read_tokens, Some(6));
        assert_eq!(result.cache_write_tokens, Some(2));
        merge_usage(&mut result, &json!({"completion_tokens":5}), route());
        assert_eq!(result.reasoning_tokens, Some(2));
        assert_eq!(result.input_tokens, Some(10));
    }

    #[test]
    fn missing_usage_stays_unknown_and_explicit_zero_is_preserved() {
        let mut result = usage(&Value::Null, route());
        assert_eq!(result.provenance, UsageProvenance::Unknown);
        assert!(result.cache.is_none());
        merge_usage(
            &mut result,
            &json!({"prompt_tokens":0,"completion_tokens":0,"cached_tokens":0}),
            route(),
        );
        assert_eq!(result.input_tokens, Some(0));
        assert_eq!(
            result.cache.expect("valid test fixture").outcome,
            CacheOutcome::Miss
        );
    }

    #[test]
    fn cache_aliases_are_alternatives_not_additive() {
        let result = usage(
            &json!({"prompt_tokens":20,"cached_tokens":8,"prompt_cache_hit_tokens":12,"prompt_cache_miss_tokens":3,"cachedContentTokenCount":10}),
            route(),
        );
        assert_eq!(result.input_tokens, Some(20));
        assert_eq!(result.cache_read_tokens, Some(12));
        assert_eq!(result.cache_write_tokens, Some(3));
    }

    #[test]
    fn catalog_selects_reviewed_endpoints_without_enabling_all_upstream_routes() {
        for id in [
            "openai",
            "anthropic",
            "gemini",
            "groq",
            "openrouter",
            "mistral",
            "nvidia",
            "cerebras",
        ] {
            let config =
                HostedProviderConfig::from_omp2(id, "Test", "explicit-secret-handle", Vec::new())
                    .expect("valid test fixture");
            assert_eq!(config.secret_id, "explicit-secret-handle");
            assert!(config.models.is_empty());
            assert!(config.catalog_version.starts_with("omp2:"));
        }
        assert!(
            HostedProviderConfig::from_omp2("github-copilot", "Test", "handle", Vec::new())
                .is_err()
        );
        let gemini = HostedProviderConfig::from_omp2("gemini", "Test", "handle", Vec::new())
            .expect("valid test fixture");
        assert!(matches!(gemini.auth, HostedAuth::Header { name, .. } if name == "x-goog-api-key"));
        assert!(
            !gemini
                .endpoints
                .messages
                .expect("valid test fixture")
                .contains('?')
        );
        let groq = HostedProviderConfig::from_omp2("groq", "Test", "handle", Vec::new())
            .expect("valid test fixture");
        assert_eq!(
            groq.endpoints.chat_completions.as_deref(),
            Some("https://api.groq.com/openai/v1/chat/completions")
        );
        assert!(groq.endpoints.completions.is_none());
    }
}
