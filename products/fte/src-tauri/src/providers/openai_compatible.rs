use async_trait::async_trait;
use futures::{StreamExt, stream::BoxStream};
use reqwest::Client;
use std::fmt;
use std::time::Duration;
use tracing::warn;

use crate::backend::{BackendCredentials, InferenceBackend};
use crate::providers::{
    Capability, ChatChunk, ChatRequest, ChatResponse, CompletionChunk, CompletionRequest,
    CompletionResponse,
};
use crate::providers::{
    completions::{CompletionEndpoint, CompletionProtocol, completion_chunks_from_response},
    spec::{ParameterPolicy, ProviderSpec, RequestMode},
    streaming,
};

pub struct OpenAiCompatibleProvider {
    spec: ProviderSpec,
    client: Client,
    completion_endpoint: Option<CompletionEndpoint>,
}

#[derive(Debug)]
struct ProviderHttpError {
    status: u16,
    message: String,
}

impl fmt::Display for ProviderHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderHttpError {}

pub fn upstream_http_status(error: &anyhow::Error) -> Option<u16> {
    error
        .downcast_ref::<ProviderHttpError>()
        .map(|error| error.status)
}

pub fn is_transport_timeout(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .is_some_and(reqwest::Error::is_timeout)
}

impl OpenAiCompatibleProvider {
    pub fn new(
        id: &'static str,
        name: &'static str,
        chat_endpoint: &'static str,
        capabilities: Vec<Capability>,
    ) -> Self {
        Self::from_spec(ProviderSpec::openai_compatible(
            id,
            name,
            chat_endpoint,
            capabilities,
        ))
    }

    pub fn from_spec(spec: ProviderSpec) -> Self {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(120))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_else(|error| {
                warn!("Could not build the hardened HTTP client: {error}");
                Client::new()
            });
        Self {
            spec,
            client,
            completion_endpoint: None,
        }
    }

    pub fn with_completion_endpoint(
        mut self,
        endpoint: &'static str,
        protocol: CompletionProtocol,
    ) -> Self {
        self.completion_endpoint = Some(CompletionEndpoint::new(endpoint, protocol));
        self
    }
}

#[async_trait]
impl InferenceBackend for OpenAiCompatibleProvider {
    fn id(&self) -> &str {
        self.spec.id()
    }

    fn name(&self) -> &str {
        self.spec.name()
    }

    fn capabilities(&self) -> &[Capability] {
        self.spec.capabilities()
    }

    async fn chat(
        &self,
        req: &ChatRequest,
        credentials: BackendCredentials<'_>,
        policy: &ParameterPolicy,
    ) -> anyhow::Result<ChatResponse> {
        let api_key = credentials.require_api_key(self.name())?;
        let prepared = self
            .spec
            .prepare_chat(req, RequestMode::NonStreaming, policy, api_key)?;
        let response = self
            .client
            .post(&prepared.url)
            .headers(prepared.headers)
            .json(&prepared.body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(upstream_error(response, self.name(), false).await);
        }

        self.spec
            .transform_chat_response(response.json::<serde_json::Value>().await?)
    }

    async fn chat_stream(
        &self,
        req: &ChatRequest,
        credentials: BackendCredentials<'_>,
        policy: &ParameterPolicy,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<ChatChunk>>> {
        let api_key = credentials.require_api_key(self.name())?;
        let prepared = self
            .spec
            .prepare_chat(req, RequestMode::Streaming, policy, api_key)?;
        let response = self
            .client
            .post(&prepared.url)
            .headers(prepared.headers)
            .json(&prepared.body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(upstream_error(response, self.name(), true).await);
        }

        Ok(streaming::chat_chunks_from_response(
            response,
            self.spec.stream_parser(),
        ))
    }

    fn supports_completions(&self) -> bool {
        self.completion_endpoint.is_some()
    }

    async fn complete(
        &self,
        req: &CompletionRequest,
        credentials: BackendCredentials<'_>,
    ) -> anyhow::Result<CompletionResponse> {
        let api_key = credentials.require_api_key(self.name())?;
        let endpoint = self.completion_endpoint.ok_or_else(|| {
            anyhow::anyhow!("{} does not support native text completions", self.name())
        })?;
        let body = endpoint.request_body(req, false)?;
        let response = self
            .client
            .post(endpoint.url)
            .headers(self.spec.headers(api_key)?)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(upstream_error(response, self.name(), false).await);
        }

        endpoint.response(response.json::<serde_json::Value>().await?)
    }

    async fn complete_stream(
        &self,
        req: &CompletionRequest,
        credentials: BackendCredentials<'_>,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<CompletionChunk>>> {
        let api_key = credentials.require_api_key(self.name())?;
        let endpoint = self.completion_endpoint.ok_or_else(|| {
            anyhow::anyhow!(
                "{} does not support native text completion streaming",
                self.name()
            )
        })?;
        let body = endpoint.request_body(req, true)?;
        let response = self
            .client
            .post(endpoint.url)
            .headers(self.spec.headers(api_key)?)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(upstream_error(response, self.name(), true).await);
        }

        Ok(completion_chunks_from_response(response, endpoint.protocol))
    }
}

async fn upstream_error(
    response: reqwest::Response,
    provider_name: &str,
    streaming: bool,
) -> anyhow::Error {
    const MAX_ERROR_BODY_BYTES: usize = 32 * 1024;

    let status = response.status();
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    let mut truncated = false;

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => {
                let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(body.len());
                if chunk.len() > remaining {
                    body.extend_from_slice(&chunk[..remaining]);
                    truncated = true;
                    break;
                }
                body.extend_from_slice(&chunk);
            }
            Err(error) => {
                return anyhow::Error::new(ProviderHttpError {
                    status: status.as_u16(),
                    message: format!(
                        "{provider_name} API error ({status}); its error body could not be read: {error}"
                    ),
                });
            }
        }
    }

    let body = String::from_utf8_lossy(&body);
    let suffix = if truncated { "… [truncated]" } else { "" };
    let request_kind = if streaming { " streaming" } else { "" };
    anyhow::Error::new(ProviderHttpError {
        status: status.as_u16(),
        message: format!("{provider_name}{request_kind} API error ({status}): {body}{suffix}"),
    })
}
