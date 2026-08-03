use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;

use crate::providers::spec::ParameterPolicy;
use crate::providers::{
    Capability, ChatChunk, ChatRequest, ChatResponse, CompletionChunk, CompletionRequest,
    CompletionResponse,
};

/// Describes where inference is executed without leaking transport details into routing policy.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    RemoteApi,
    LocalEmbedded,
    LocalService,
    Unknown,
}

/// Declares whether the gateway must resolve a provider API key before dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialRequirement {
    ApiKey,
    NotRequired,
}

/// Runtime readiness is deliberately separate from credentials and quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendReadiness {
    Ready,
    MissingCredential,
    NotConfigured,
    Loading,
    Unavailable,
}

impl BackendReadiness {
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    pub const fn configuration_satisfied(self) -> bool {
        !matches!(self, Self::MissingCredential | Self::NotConfigured)
    }

    pub const fn status(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::MissingCredential => "needs_key",
            Self::NotConfigured => "not_configured",
            Self::Loading => "loading",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Credentials resolved by the application shell. Local backends receive `None`.
#[derive(Clone, Copy)]
pub enum BackendCredentials<'a> {
    None,
    ApiKey(&'a str),
}

impl fmt::Debug for BackendCredentials<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("BackendCredentials::None"),
            Self::ApiKey(_) => formatter.write_str("BackendCredentials::ApiKey([REDACTED])"),
        }
    }
}

impl<'a> BackendCredentials<'a> {
    pub fn require_api_key(self, backend_name: &str) -> anyhow::Result<&'a str> {
        match self {
            Self::ApiKey(value) if !value.is_empty() => Ok(value),
            Self::ApiKey(_) | Self::None => Err(anyhow::anyhow!(
                "No API key is available for {backend_name}."
            )),
        }
    }
}

/// A transport-neutral inference target. Remote providers and local runtimes implement the same
/// generation surface while declaring different readiness and credential requirements.
#[async_trait]
pub trait InferenceBackend: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn capabilities(&self) -> &[Capability];

    fn kind(&self) -> BackendKind {
        BackendKind::RemoteApi
    }

    fn credential_requirement(&self) -> CredentialRequirement {
        CredentialRequirement::ApiKey
    }

    /// Reports a non-blocking engine or transport snapshot only. The router resolves credentials
    /// separately; loading work and filesystem inspection belong outside route selection.
    fn runtime_readiness(&self) -> BackendReadiness {
        BackendReadiness::Ready
    }

    async fn chat(
        &self,
        req: &ChatRequest,
        credentials: BackendCredentials<'_>,
        policy: &ParameterPolicy,
    ) -> anyhow::Result<ChatResponse>;

    async fn chat_stream(
        &self,
        req: &ChatRequest,
        credentials: BackendCredentials<'_>,
        policy: &ParameterPolicy,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<ChatChunk>>>;

    fn supports_completions(&self) -> bool {
        false
    }

    async fn complete(
        &self,
        _req: &CompletionRequest,
        _credentials: BackendCredentials<'_>,
    ) -> anyhow::Result<CompletionResponse> {
        Err(anyhow::anyhow!(
            "{} does not support native text completions",
            self.name()
        ))
    }

    async fn complete_stream(
        &self,
        _req: &CompletionRequest,
        _credentials: BackendCredentials<'_>,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<CompletionChunk>>> {
        Err(anyhow::anyhow!(
            "{} does not support native text completion streaming",
            self.name()
        ))
    }
}

#[derive(Default)]
pub struct BackendRegistry {
    backends: HashMap<String, Box<dyn InferenceBackend>>,
}

impl BackendRegistry {
    pub fn register(&mut self, backend: Box<dyn InferenceBackend>) -> anyhow::Result<()> {
        let raw_id = backend.id();
        let id = raw_id.trim();
        if id.is_empty() {
            return Err(anyhow::anyhow!("Inference backend ID must not be empty."));
        }
        if id != raw_id
            || !id.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '-' | '_' | '.')
            })
        {
            return Err(anyhow::anyhow!(
                "Inference backend ID '{raw_id}' must use lowercase ASCII letters, digits, '.', '-' or '_' without surrounding whitespace."
            ));
        }
        if self.backends.contains_key(id) {
            return Err(anyhow::anyhow!(
                "Inference backend '{id}' is already registered."
            ));
        }
        self.backends.insert(id.to_string(), backend);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&dyn InferenceBackend> {
        self.backends.get(id).map(Box::as_ref)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.backends.contains_key(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_debug_output_is_redacted() {
        let rendered = format!("{:?}", BackendCredentials::ApiKey("secret-value"));
        assert!(rendered.contains("REDACTED"));
        assert!(!rendered.contains("secret-value"));
    }

    #[test]
    fn registry_rejects_duplicate_ids_without_replacing_the_backend() {
        let mut registry = BackendRegistry::default();
        registry.register(Box::new(TestBackend("local"))).unwrap();

        let error = registry
            .register(Box::new(TestBackend("local")))
            .unwrap_err()
            .to_string();

        assert!(error.contains("already registered"));
        assert_eq!(registry.get("local").unwrap().name(), "local");
    }

    struct TestBackend(&'static str);

    #[async_trait]
    impl InferenceBackend for TestBackend {
        fn id(&self) -> &str {
            self.0
        }

        fn name(&self) -> &str {
            self.0
        }

        fn capabilities(&self) -> &[Capability] {
            &[]
        }

        async fn chat(
            &self,
            _req: &ChatRequest,
            _credentials: BackendCredentials<'_>,
            _policy: &ParameterPolicy,
        ) -> anyhow::Result<ChatResponse> {
            Err(anyhow::anyhow!("not used"))
        }

        async fn chat_stream(
            &self,
            _req: &ChatRequest,
            _credentials: BackendCredentials<'_>,
            _policy: &ParameterPolicy,
        ) -> anyhow::Result<BoxStream<'static, anyhow::Result<ChatChunk>>> {
            Err(anyhow::anyhow!("not used"))
        }
    }
}
