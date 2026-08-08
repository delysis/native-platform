//! Reusable gateway assembly for the desktop application.

use crate::catalog::{ModelCatalogEntry, PromptSemantics, default_model_catalog};
use crate::db::Database;
use crate::providers::Capability;
use fte_providers::{HostedProviderBackend, HostedProviderConfig};
use fte_router::{Gateway, GatewayDefaults};
use fte_store::SecretResolver;
use fte_types::{
    BackendLocation, GatewayError, Modality, ModelCapabilities, ModelDescriptor, PromptForm,
    RequestId, RouteObservations,
};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

#[derive(Default)]
pub struct DeferredDatabaseSecrets {
    database: RwLock<Option<Arc<Database>>>,
}

impl DeferredDatabaseSecrets {
    pub fn bind(&self, database: Arc<Database>) -> Result<(), GatewayError> {
        let mut current = self.database.write().map_err(|_| {
            GatewayError::unavailable(
                &RequestId::new(),
                "secret_resolver_state_failed",
                "the hosted credential resolver is unavailable",
            )
        })?;
        if current.is_some() {
            return Err(GatewayError::invalid_request(
                &RequestId::new(),
                "secret_resolver_already_bound",
                "the hosted credential resolver was already initialized",
            ));
        }
        *current = Some(database);
        Ok(())
    }
}

impl SecretResolver for DeferredDatabaseSecrets {
    fn resolve(&self, provider: &str) -> Result<Option<String>, GatewayError> {
        let database = self.database.read().map_err(|_| {
            GatewayError::unavailable(
                &RequestId::new(),
                "secret_resolver_state_failed",
                "the hosted credential resolver is unavailable",
            )
        })?;
        let Some(database) = database.as_ref() else {
            return Ok(None);
        };
        database
            .get_api_key(provider)
            .map_err(|error| GatewayError {
                code: "secret_resolver_read_failed".to_string(),
                class: fte_types::ErrorClass::Internal,
                retryable: true,
                http_status: 503,
                request_id: RequestId::new(),
                provider: Some(provider.to_string()),
                safe_detail: format!(
                    "the saved credential for {provider} could not be read: {error}"
                ),
            })
    }
}

pub struct GatewayV2 {
    gateway: Arc<Gateway>,
    secrets: Arc<DeferredDatabaseSecrets>,
}

impl GatewayV2 {
    pub fn new() -> Result<Self, GatewayError> {
        let gateway = Arc::new(Gateway::new(GatewayDefaults {
            catalog_version: "free-token-energy-desktop-v2".to_string(),
        }));
        let secrets = Arc::new(DeferredDatabaseSecrets::default());
        register_hosted_backends(&gateway, secrets.clone())?;
        Ok(Self { gateway, secrets })
    }

    pub fn gateway(&self) -> Arc<Gateway> {
        Arc::clone(&self.gateway)
    }

    pub fn secrets(&self) -> Arc<DeferredDatabaseSecrets> {
        Arc::clone(&self.secrets)
    }
}

fn register_hosted_backends(
    gateway: &Arc<Gateway>,
    secrets: Arc<dyn SecretResolver>,
) -> Result<(), GatewayError> {
    let mut catalog = default_model_catalog().into_iter().fold(
        BTreeMap::<String, Vec<ModelCatalogEntry>>::new(),
        |mut map, item| {
            map.entry(item.provider_id.clone()).or_default().push(item);
            map
        },
    );

    let openai_models = vec![ModelDescriptor {
        id: "gpt-5.6-sol".to_string(),
        display_name: "GPT-5.6 Sol".to_string(),
        backend_id: "openai".to_string(),
        location: BackendLocation::Hosted,
        capabilities: ModelCapabilities {
            prompt_forms: vec![PromptForm::Chat],
            modalities: vec![Modality::Text, Modality::Image, Modality::Document],
            tools: true,
            structured_output: true,
            reasoning: true,
            streaming: true,
            provider_cache: true,
        },
        context_tokens: None,
        max_output_tokens: None,
        observed: RouteObservations::default(),
    }];
    register(
        gateway,
        HostedProviderConfig::openai("openai", "OpenAI", "openai", openai_models),
        Arc::clone(&secrets),
    )?;

    if let Some(entries) = catalog.remove("anthropic") {
        register(
            gateway,
            HostedProviderConfig::anthropic(
                "anthropic",
                "Anthropic",
                "anthropic",
                descriptors("anthropic", entries, true, true),
            ),
            Arc::clone(&secrets),
        )?;
    }

    if let Some(entries) = catalog.remove("gemini") {
        let mut models = descriptors("gemini", entries, true, false);
        for model in &mut models {
            model.capabilities.structured_output = true;
        }
        register(
            gateway,
            HostedProviderConfig::gemini("gemini", "Google Gemini", "gemini", models),
            Arc::clone(&secrets),
        )?;
    }

    for (id, name, chat, completion) in [
        (
            "openrouter",
            "OpenRouter",
            "https://openrouter.ai/api/v1/chat/completions",
            Some("https://openrouter.ai/api/v1/completions"),
        ),
        (
            "groq",
            "Groq Cloud",
            "https://api.groq.com/openai/v1/chat/completions",
            None,
        ),
        (
            "mistral",
            "Mistral AI",
            "https://api.mistral.ai/v1/chat/completions",
            None,
        ),
        (
            "nvidia",
            "NVIDIA NIM",
            "https://integrate.api.nvidia.com/v1/chat/completions",
            None,
        ),
        (
            "cerebras",
            "Cerebras",
            "https://api.cerebras.ai/v1/chat/completions",
            Some("https://api.cerebras.ai/v1/completions"),
        ),
    ] {
        let Some(entries) = catalog.remove(id) else {
            continue;
        };
        let mut config = HostedProviderConfig::openai_compatible(
            id,
            name,
            id,
            chat,
            descriptors(id, entries, false, false),
        );
        config.endpoints.completions = completion.map(ToString::to_string);
        if id == "openrouter" {
            config.static_headers.insert(
                "http-referer".to_string(),
                "https://free-token-energy.local".to_string(),
            );
            config
                .static_headers
                .insert("x-title".to_string(), "Free Token Energy".to_string());
        }
        register(gateway, config, Arc::clone(&secrets))?;
    }
    Ok(())
}

fn register(
    gateway: &Gateway,
    config: HostedProviderConfig,
    secrets: Arc<dyn SecretResolver>,
) -> Result<(), GatewayError> {
    gateway.register_backend(Arc::new(HostedProviderBackend::new(config, secrets)?))
}

fn descriptors(
    backend_id: &str,
    entries: Vec<ModelCatalogEntry>,
    reasoning: bool,
    provider_cache: bool,
) -> Vec<ModelDescriptor> {
    entries
        .into_iter()
        .map(|entry| {
            let mut prompt_forms = Vec::new();
            if entry.chat_completions {
                prompt_forms.push(PromptForm::Chat);
            }
            if entry.text_completions.as_ref().is_some_and(|completion| {
                matches!(
                    completion.prompt_semantics,
                    PromptSemantics::DirectContinuation | PromptSemantics::LegacyPromptProtocol
                )
            }) {
                prompt_forms.push(PromptForm::Completion);
            }
            let vision = entry.capabilities.contains(&Capability::Vision);
            ModelDescriptor {
                id: entry.provider_model_id,
                display_name: entry.display_name,
                backend_id: backend_id.to_string(),
                location: BackendLocation::Hosted,
                capabilities: ModelCapabilities {
                    prompt_forms,
                    modalities: if vision {
                        vec![Modality::Text, Modality::Image]
                    } else {
                        vec![Modality::Text]
                    },
                    tools: entry.capabilities.contains(&Capability::Tools),
                    structured_output: false,
                    reasoning,
                    streaming: entry.capabilities.contains(&Capability::Streaming),
                    provider_cache,
                },
                context_tokens: None,
                max_output_tokens: None,
                observed: RouteObservations::default(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_gateway_registers_protocol_native_and_compatible_backends() {
        let runtime = GatewayV2::new().expect("gateway");
        let models = runtime.gateway().models();
        for backend in [
            "openai",
            "anthropic",
            "gemini",
            "openrouter",
            "groq",
            "mistral",
            "nvidia",
            "cerebras",
        ] {
            assert!(
                models.iter().any(|model| model.backend_id == backend),
                "missing {backend}"
            );
        }
    }
}
