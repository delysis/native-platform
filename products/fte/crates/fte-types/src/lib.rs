//! Protocol-neutral contracts shared by the Free Token Energy gateway, its
//! protocol edges, hosted providers, and embedded local runtimes.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

pub const DEFAULT_EVENT_CAPACITY: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(transparent)]
pub struct RequestId(pub String);

impl RequestId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelSelector {
    ExactRoute {
        backend_id: String,
        model_id: String,
    },
    ExactModel {
        model_id: String,
    },
    Profile {
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GatewayRequest {
    pub request_id: RequestId,
    pub client_id: String,
    pub model: ModelSelector,
    pub input: GenerationInput,
    #[serde(default)]
    pub sampling: SamplingOptions,
    #[serde(default)]
    pub response_format: ResponseFormat,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub tool_policy: ToolPolicy,
    #[serde(default)]
    pub cache: CachePolicy,
    #[serde(default)]
    pub routing: RoutingPolicy,
    #[serde(default)]
    pub storage: StoragePolicy,
    #[serde(default)]
    pub deadline: DeadlinePolicy,
    #[serde(default)]
    pub stream: StreamPolicy,
    #[serde(default)]
    pub provider_extensions: BTreeMap<String, Value>,
}

impl GatewayRequest {
    pub fn validate(&self) -> Result<(), GatewayError> {
        if self.client_id.trim().is_empty() {
            return Err(GatewayError::invalid_request(
                &self.request_id,
                "client_id_empty",
                "client_id must not be empty",
            ));
        }
        if self.sampling.max_output_tokens == Some(0) {
            return Err(GatewayError::invalid_request(
                &self.request_id,
                "max_output_tokens_invalid",
                "max_output_tokens must be greater than zero",
            ));
        }
        if let GenerationInput::Completion { prompts } = &self.input
            && prompts.is_empty()
        {
            return Err(GatewayError::invalid_request(
                &self.request_id,
                "completion_prompts_empty",
                "at least one completion prompt is required",
            ));
        }
        if let GenerationInput::Chat { items } = &self.input
            && items.is_empty()
        {
            return Err(GatewayError::invalid_request(
                &self.request_id,
                "chat_items_empty",
                "at least one chat item is required",
            ));
        }
        if self.deadline.total_ms == Some(0) {
            return Err(GatewayError::invalid_request(
                &self.request_id,
                "deadline_invalid",
                "total deadline must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GenerationInput {
    Chat { items: Vec<InputItem> },
    Completion { prompts: Vec<CompletionPrompt> },
    FillInMiddle { prefix: String, suffix: String },
}

impl GenerationInput {
    #[must_use]
    pub const fn prompt_form(&self) -> PromptForm {
        match self {
            Self::Chat { .. } => PromptForm::Chat,
            Self::Completion { .. } => PromptForm::Completion,
            Self::FillInMiddle { .. } => PromptForm::FillInMiddle,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PromptForm {
    Chat,
    Completion,
    FillInMiddle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionPrompt {
    Text {
        text: String,
        #[serde(default)]
        add_bos: bool,
    },
    Tokens {
        token_ids: Vec<i32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    Message {
        id: Option<String>,
        role: MessageRole,
        content: Vec<ContentBlock>,
    },
    FunctionCall {
        id: Option<String>,
        call_id: String,
        name: String,
        arguments: Value,
    },
    FunctionResult {
        id: Option<String>,
        call_id: String,
        output: Vec<ContentBlock>,
        #[serde(default)]
        is_error: bool,
    },
    Reasoning {
        id: Option<String>,
        summary: Vec<String>,
        opaque_continuation: Option<Value>,
    },
    ProviderOpaque {
        provider: String,
        item: Value,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source: MediaSource,
        detail: Option<String>,
    },
    Audio {
        source: MediaSource,
        format: Option<String>,
    },
    Document {
        source: MediaSource,
        title: Option<String>,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaSource {
    Bytes {
        mime_type: String,
        data_base64: String,
    },
    Url {
        url: String,
    },
    FileId {
        file_id: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SamplingOptions {
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<i32>,
    pub min_p: Option<f32>,
    pub seed: Option<u32>,
    #[serde(default)]
    pub stop: Vec<String>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    #[default]
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: Value,
        strict: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub owner: ToolOwner,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOwner {
    #[default]
    Client,
    Gateway,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolPolicy {
    #[serde(default)]
    pub execution: ToolExecutionPolicy,
    pub max_turns: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionPolicy {
    #[default]
    ClientOnly,
    Ask,
    AllowGateway,
    Deny,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachePolicy {
    #[serde(default)]
    pub mode: CacheMode,
    #[serde(default)]
    pub requirement: CacheRequirement,
    /// Number of leading canonical chat items that form the reusable prefix.
    ///
    /// The remaining items are request-specific and are never written into a
    /// stable prefix pack. `StablePrefix` requires this boundary explicitly;
    /// other local cache modes default to the complete submitted chat.
    pub stable_prefix_items: Option<usize>,
    pub owner_namespace: Option<String>,
    pub owner_version: Option<String>,
    pub provider_key: Option<String>,
    pub provider_ttl: Option<ProviderCacheTtl>,
    #[serde(default)]
    pub provider_breakpoints: Vec<ProviderCacheBreakpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderCacheBreakpoint {
    /// Protocol-native, stable path such as `system.0` or
    /// `messages.2.content.1`.
    pub path: String,
    pub ttl: Option<ProviderCacheTtl>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    Disabled,
    Memory,
    Persistent,
    StablePrefix,
    ProviderNative,
    #[default]
    Adaptive,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheRequirement {
    #[default]
    Optional,
    Required,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCacheTtl {
    FiveMinutes,
    OneHour,
    TwentyFourHours,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingPolicy {
    #[serde(default)]
    pub privacy: PrivacyPolicy,
    #[serde(default)]
    pub profile: RouteProfile,
    /// Retained for wire compatibility with early gateway-v2 clients.
    ///
    /// The router uses the product's fixed, documented ranking policy; request
    /// callers cannot change routing weights or introduce unmeasured signals.
    #[serde(default)]
    pub weights: RoutingWeights,
    #[serde(default)]
    pub retry_before_output: bool,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            privacy: PrivacyPolicy::LocalOnly,
            profile: RouteProfile::LocalOnly,
            weights: RoutingWeights::default(),
            retry_before_output: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyPolicy {
    #[default]
    LocalOnly,
    HostedAllowed,
    HostedOnly,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteProfile {
    #[default]
    LocalOnly,
    HostedOnly,
    PreferLocal,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingWeights {
    pub quality: f64,
    pub cost: f64,
    pub latency: f64,
    pub quota: f64,
    pub cache_warmth: f64,
}

impl Default for RoutingWeights {
    fn default() -> Self {
        Self {
            quality: 0.30,
            cost: 0.20,
            latency: 0.20,
            quota: 0.20,
            cache_warmth: 0.10,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoragePolicy {
    #[serde(default)]
    pub store_response: bool,
    pub previous_response_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeadlinePolicy {
    pub queue_ms: Option<u64>,
    pub model_load_ms: Option<u64>,
    pub connect_ms: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub idle_stream_ms: Option<u64>,
    pub total_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamPolicy {
    #[serde(default)]
    pub enabled: bool,
    pub event_capacity: Option<usize>,
    #[serde(default)]
    pub latency_sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackendRequest {
    pub request: GatewayRequest,
    pub route: ResolvedRoute,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedRoute {
    pub backend_id: String,
    pub model_id: String,
    pub display_name: String,
    pub location: BackendLocation,
    pub catalog_version: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackendLocation {
    LocalEmbedded,
    Hosted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackendDescriptor {
    pub id: String,
    pub display_name: String,
    pub location: BackendLocation,
    pub models: Vec<ModelDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelDescriptor {
    pub id: String,
    pub display_name: String,
    pub backend_id: String,
    pub location: BackendLocation,
    pub capabilities: ModelCapabilities,
    pub context_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub observed: RouteObservations,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub prompt_forms: Vec<PromptForm>,
    #[serde(default)]
    pub modalities: Vec<Modality>,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub structured_output: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub provider_cache: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Text,
    Image,
    Audio,
    Document,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RouteObservations {
    pub quality: Option<f64>,
    pub cost_per_million_input_tokens: Option<f64>,
    pub latency_ms: Option<u64>,
    pub quota_headroom: Option<f64>,
    pub cache_warmth: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BackendReadiness {
    Ready,
    Loading,
    NotConfigured { reason: String },
    Unavailable { reason: String },
}

impl BackendReadiness {
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GatewayResponse {
    pub id: String,
    pub request_id: RequestId,
    pub model: String,
    pub route: ResolvedRoute,
    pub output: Vec<OutputItem>,
    pub usage: GatewayUsage,
    pub status: TerminalStatus,
    pub previous_response_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputItem {
    Message {
        id: String,
        role: MessageRole,
        content: Vec<ContentBlock>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: Value,
    },
    Reasoning {
        id: String,
        summary: Vec<String>,
        opaque_continuation: Option<Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GatewayEvent {
    ResponseCreated {
        request_id: RequestId,
        response_id: String,
        route: ResolvedRoute,
    },
    OutputItemAdded {
        request_id: RequestId,
        output_index: usize,
        item: OutputItem,
    },
    ContentPartAdded {
        request_id: RequestId,
        output_index: usize,
        content_index: usize,
        part: ContentBlock,
    },
    ContentPartCompleted {
        request_id: RequestId,
        output_index: usize,
        content_index: usize,
        part: ContentBlock,
    },
    OutputItemCompleted {
        request_id: RequestId,
        output_index: usize,
        item: OutputItem,
    },
    TextDelta {
        request_id: RequestId,
        output_index: usize,
        content_index: usize,
        delta: String,
    },
    ReasoningSummaryDelta {
        request_id: RequestId,
        output_index: usize,
        summary_index: usize,
        delta: String,
    },
    FunctionArgumentsDelta {
        request_id: RequestId,
        output_index: usize,
        delta: String,
    },
    UsageUpdated {
        request_id: RequestId,
        usage: GatewayUsage,
    },
    Warning {
        request_id: RequestId,
        code: String,
        message: String,
    },
    Completed {
        request_id: RequestId,
        response: Box<GatewayResponse>,
    },
    Cancelled {
        request_id: RequestId,
        usage: GatewayUsage,
    },
    Failed {
        request_id: RequestId,
        error: GatewayError,
    },
}

impl GatewayEvent {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Cancelled { .. } | Self::Failed { .. }
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GatewayUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    #[serde(default)]
    pub provenance: UsageProvenance,
    pub queue_ms: Option<u64>,
    pub model_load_ms: Option<u64>,
    pub time_to_first_token_ms: Option<u64>,
    pub total_ms: Option<u64>,
    pub selected_route: Option<ResolvedRoute>,
    pub cache: Option<CacheReceipt>,
    #[serde(default)]
    pub real_local_inference: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageProvenance {
    Exact,
    Estimated,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheReceipt {
    pub tier: CacheTier,
    pub outcome: CacheOutcome,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheTier {
    ResidentSequence,
    MemoryPrefix,
    PersistentPrefix,
    StablePrefixPack,
    ProviderNative,
    None,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheOutcome {
    Hit,
    Miss,
    Rejected,
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayError {
    pub code: String,
    pub class: ErrorClass,
    pub retryable: bool,
    pub http_status: u16,
    pub request_id: RequestId,
    pub provider: Option<String>,
    pub safe_detail: String,
}

impl GatewayError {
    #[must_use]
    pub fn invalid_request(request_id: &RequestId, code: &str, detail: &str) -> Self {
        Self {
            code: code.to_string(),
            class: ErrorClass::InvalidRequest,
            retryable: false,
            http_status: 400,
            request_id: request_id.clone(),
            provider: None,
            safe_detail: detail.to_string(),
        }
    }

    #[must_use]
    pub fn unavailable(request_id: &RequestId, code: &str, detail: &str) -> Self {
        Self {
            code: code.to_string(),
            class: ErrorClass::Unavailable,
            retryable: true,
            http_status: 503,
            request_id: request_id.clone(),
            provider: None,
            safe_detail: detail.to_string(),
        }
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.safe_detail)
    }
}

impl std::error::Error for GatewayError {}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    Authentication,
    Authorization,
    InvalidRequest,
    Capability,
    Privacy,
    Quota,
    RateLimit,
    Timeout,
    Cancelled,
    Unavailable,
    Provider,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelTarget {
    Request,
    Output(usize),
}

pub trait TicketCancellation: Send + Sync {
    fn cancel(&self, target: CancelTarget) -> usize;
}

pub struct GatewayTicket {
    pub request_id: RequestId,
    pub events: mpsc::Receiver<GatewayEvent>,
    final_response: Option<oneshot::Receiver<Result<GatewayResponse, GatewayError>>>,
    cancellation: Arc<dyn TicketCancellation>,
    terminal_observed: Arc<AtomicBool>,
    cancel_on_drop: bool,
}

impl fmt::Debug for GatewayTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GatewayTicket")
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

impl GatewayTicket {
    #[must_use]
    pub fn new(
        request_id: RequestId,
        events: mpsc::Receiver<GatewayEvent>,
        final_response: oneshot::Receiver<Result<GatewayResponse, GatewayError>>,
        cancellation: Arc<dyn TicketCancellation>,
        terminal_observed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            request_id,
            events,
            final_response: Some(final_response),
            cancellation,
            terminal_observed,
            cancel_on_drop: true,
        }
    }

    /// Keeps an opaque router admission guard alive until the authoritative
    /// backend result resolves. Dropping a consumer cancels the request, but
    /// it does not falsely report the backend task as drained before that task
    /// has actually terminated.
    #[must_use]
    pub fn with_admission_lease(mut self, lease: Box<dyn Send>) -> Self {
        let request_id = self.request_id.clone();
        let Some(upstream_final) = self.final_response.take() else {
            return self;
        };
        let (final_tx, final_rx) = oneshot::channel();
        tokio::spawn(async move {
            let result = upstream_final.await.unwrap_or_else(|_| {
                Err(GatewayError::unavailable(
                    &request_id,
                    "backend_result_channel_closed",
                    "the backend stopped before returning an authoritative result",
                ))
            });
            drop(lease);
            let _ = final_tx.send(result);
        });
        self.final_response = Some(final_rx);
        self
    }

    /// Applies request deadlines to the ticket itself, so embedded Rust and
    /// Tauri consumers receive the same timeout behavior as HTTP clients.
    ///
    /// `elapsed` is time already spent validating, queuing, selecting, and
    /// starting the backend. First-output and total deadlines are measured
    /// from the original gateway call, not from creation of this wrapper.
    #[must_use]
    pub fn with_deadlines(
        mut self,
        policy: DeadlinePolicy,
        elapsed: Duration,
        event_capacity: usize,
    ) -> Self {
        if policy.first_token_ms.is_none()
            && policy.idle_stream_ms.is_none()
            && policy.total_ms.is_none()
        {
            return self;
        }

        let request_id = self.request_id.clone();
        let Some(mut upstream_final) = self.final_response.take() else {
            return self;
        };
        let (placeholder_tx, placeholder_rx) = mpsc::channel(1);
        drop(placeholder_tx);
        let mut upstream_events = std::mem::replace(&mut self.events, placeholder_rx);
        let cancellation = Arc::clone(&self.cancellation);
        self.cancel_on_drop = false;

        let capacity = event_capacity.clamp(32, 4096);
        let (event_tx, event_rx) = mpsc::channel(capacity);
        let (final_tx, final_rx) = oneshot::channel();
        let terminal_observed = Arc::new(AtomicBool::new(false));
        let terminal_for_task = Arc::clone(&terminal_observed);
        let request_for_task = request_id.clone();
        let cancellation_for_task = Arc::clone(&cancellation);

        tokio::spawn(async move {
            let Ok(terminal_permit) = event_tx.clone().reserve_owned().await else {
                let _ = cancellation_for_task.cancel(CancelTarget::Request);
                return;
            };
            let mut terminal_permit = Some(terminal_permit);
            let now = tokio::time::Instant::now();
            let first_deadline = remaining_deadline(now, policy.first_token_ms, elapsed);
            let total_deadline = remaining_deadline(now, policy.total_ms, elapsed);
            let idle_duration = policy.idle_stream_ms.map(Duration::from_millis);
            let mut idle_deadline = idle_duration.map(|duration| now + duration);
            let mut first_output_observed = false;
            let mut upstream_terminal_observed = false;
            let mut upstream_events_closed = false;

            loop {
                let deadline = next_ticket_deadline(
                    first_deadline.filter(|_| !first_output_observed),
                    idle_deadline.filter(|_| !upstream_terminal_observed),
                    total_deadline,
                );
                tokio::select! {
                    event = upstream_events.recv(), if !upstream_events_closed && !upstream_terminal_observed => {
                        let Some(event) = event else {
                            upstream_events_closed = true;
                            continue;
                        };
                        if event_is_output_progress(&event) {
                            first_output_observed = true;
                        }
                        if let Some(duration) = idle_duration {
                            idle_deadline = Some(tokio::time::Instant::now() + duration);
                        }
                        let is_terminal = event.is_terminal();
                        if is_terminal {
                            enqueue_reserved_terminal(
                                &mut terminal_permit,
                                event,
                                &terminal_for_task,
                            );
                            upstream_terminal_observed = true;
                            continue;
                        }
                        let send_deadline = next_ticket_deadline(
                            first_deadline.filter(|_| !first_output_observed),
                            idle_deadline.filter(|_| !upstream_terminal_observed),
                            total_deadline,
                        );
                        tokio::select! {
                            result = event_tx.send(event) => {
                                if result.is_err() {
                                    let _ = cancellation_for_task.cancel(CancelTarget::Request);
                                    return;
                                }
                            }
                            () = sleep_until_optional(send_deadline), if send_deadline.is_some() => {
                                let error = ticket_deadline_error(
                                    &request_for_task,
                                    first_output_observed,
                                    first_deadline,
                                    total_deadline,
                                );
                                let _ = cancellation_for_task.cancel(CancelTarget::Request);
                                enqueue_reserved_terminal(
                                    &mut terminal_permit,
                                    GatewayEvent::Failed {
                                        request_id: request_for_task.clone(),
                                        error: error.clone(),
                                    },
                                    &terminal_for_task,
                                );
                                let _ = final_tx.send(Err(error));
                                return;
                            }
                        }
                    }
                    result = &mut upstream_final => {
                        let result = result.unwrap_or_else(|_| {
                            Err(GatewayError::unavailable(
                                &request_for_task,
                                "backend_result_channel_closed",
                                "the backend stopped before returning an authoritative result",
                            ))
                        });
                        if !upstream_terminal_observed {
                            let terminal = terminal_event_from_result(&request_for_task, &result);
                            enqueue_reserved_terminal(
                                &mut terminal_permit,
                                terminal,
                                &terminal_for_task,
                            );
                        }
                        let _ = final_tx.send(result);
                        return;
                    }
                    () = sleep_until_optional(deadline), if deadline.is_some() => {
                        let error = ticket_deadline_error(
                            &request_for_task,
                            first_output_observed,
                            first_deadline,
                            total_deadline,
                        );
                        let _ = cancellation_for_task.cancel(CancelTarget::Request);
                        if !upstream_terminal_observed {
                            enqueue_reserved_terminal(
                                &mut terminal_permit,
                                GatewayEvent::Failed {
                                    request_id: request_for_task.clone(),
                                    error: error.clone(),
                                },
                                &terminal_for_task,
                            );
                        }
                        let _ = final_tx.send(Err(error));
                        return;
                    }
                }
            }
        });

        Self {
            request_id,
            events: event_rx,
            final_response: Some(final_rx),
            cancellation,
            terminal_observed,
            cancel_on_drop: true,
        }
    }

    pub fn cancel(&self, target: CancelTarget) -> usize {
        self.cancellation.cancel(target)
    }

    pub async fn final_response(mut self) -> Result<GatewayResponse, GatewayError> {
        let Some(mut final_response) = self.final_response.take() else {
            return Err(GatewayError::unavailable(
                &self.request_id,
                "backend_result_already_consumed",
                "the authoritative backend result was already consumed",
            ));
        };
        loop {
            tokio::select! {
                result = &mut final_response => {
                    return result.unwrap_or_else(|_| {
                        Err(GatewayError::unavailable(
                            &self.request_id,
                            "backend_result_channel_closed",
                            "the backend stopped before returning an authoritative result",
                        ))
                    });
                }
                event = self.events.recv() => {
                    if event.is_none() {
                        return final_response.await.unwrap_or_else(|_| {
                            Err(GatewayError::unavailable(
                                &self.request_id,
                                "backend_result_channel_closed",
                                "the backend stopped before returning an authoritative result",
                            ))
                        });
                    }
                }
            }
        }
    }

    #[must_use]
    pub fn terminal_observed(&self) -> bool {
        self.terminal_observed.load(Ordering::Acquire)
    }
}

impl Drop for GatewayTicket {
    fn drop(&mut self) {
        if self.cancel_on_drop && !self.terminal_observed.load(Ordering::Acquire) {
            let _ = self.cancellation.cancel(CancelTarget::Request);
        }
    }
}

fn remaining_deadline(
    now: tokio::time::Instant,
    configured_ms: Option<u64>,
    elapsed: Duration,
) -> Option<tokio::time::Instant> {
    configured_ms
        .map(|configured_ms| now + Duration::from_millis(configured_ms).saturating_sub(elapsed))
}

fn next_ticket_deadline(
    first: Option<tokio::time::Instant>,
    idle: Option<tokio::time::Instant>,
    total: Option<tokio::time::Instant>,
) -> Option<tokio::time::Instant> {
    [first, idle, total].into_iter().flatten().min()
}

async fn sleep_until_optional(deadline: Option<tokio::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    }
}

fn enqueue_reserved_terminal(
    permit: &mut Option<mpsc::OwnedPermit<GatewayEvent>>,
    event: GatewayEvent,
    terminal_observed: &AtomicBool,
) {
    if let Some(permit) = permit.take() {
        permit.send(event);
        terminal_observed.store(true, Ordering::Release);
    }
}

fn ticket_deadline_error(
    request_id: &RequestId,
    first_output_observed: bool,
    first_deadline: Option<tokio::time::Instant>,
    total_deadline: Option<tokio::time::Instant>,
) -> GatewayError {
    let now = tokio::time::Instant::now();
    let (code, detail) = if total_deadline.is_some_and(|value| now >= value) {
        (
            "request_total_deadline_exceeded",
            "the request exceeded its total deadline",
        )
    } else if !first_output_observed && first_deadline.is_some_and(|value| now >= value) {
        (
            "first_output_deadline_exceeded",
            "the selected backend did not produce output before the first-output deadline",
        )
    } else {
        (
            "stream_idle_deadline_exceeded",
            "the selected backend stream was idle for too long",
        )
    };
    ticket_timeout(request_id, code, detail)
}

fn event_is_output_progress(event: &GatewayEvent) -> bool {
    matches!(
        event,
        GatewayEvent::OutputItemAdded { .. }
            | GatewayEvent::ContentPartAdded { .. }
            | GatewayEvent::ContentPartCompleted { .. }
            | GatewayEvent::OutputItemCompleted { .. }
            | GatewayEvent::TextDelta { .. }
            | GatewayEvent::ReasoningSummaryDelta { .. }
            | GatewayEvent::FunctionArgumentsDelta { .. }
            | GatewayEvent::Completed { .. }
    )
}

fn terminal_event_from_result(
    request_id: &RequestId,
    result: &Result<GatewayResponse, GatewayError>,
) -> GatewayEvent {
    match result {
        Ok(response) if response.status == TerminalStatus::Completed => GatewayEvent::Completed {
            request_id: request_id.clone(),
            response: Box::new(response.clone()),
        },
        Ok(response) => GatewayEvent::Cancelled {
            request_id: request_id.clone(),
            usage: response.usage.clone(),
        },
        Err(error) if error.class == ErrorClass::Cancelled => GatewayEvent::Cancelled {
            request_id: request_id.clone(),
            usage: GatewayUsage::default(),
        },
        Err(error) => GatewayEvent::Failed {
            request_id: request_id.clone(),
            error: error.clone(),
        },
    }
}

fn ticket_timeout(request_id: &RequestId, code: &str, detail: &str) -> GatewayError {
    GatewayError {
        code: code.to_string(),
        class: ErrorClass::Timeout,
        retryable: true,
        http_status: 504,
        request_id: request_id.clone(),
        provider: None,
        safe_detail: detail.to_string(),
    }
}

#[async_trait]
pub trait GatewayBackend: Send + Sync {
    fn descriptor(&self) -> BackendDescriptor;
    fn readiness(&self) -> BackendReadiness;
    async fn execute(&self, request: BackendRequest) -> Result<GatewayTicket, GatewayError>;
    async fn count_tokens(&self, request: BackendRequest) -> Result<GatewayUsage, GatewayError> {
        Err(GatewayError {
            code: "token_counting_unsupported".to_string(),
            class: ErrorClass::Capability,
            retryable: false,
            http_status: 400,
            request_id: request.request.request_id,
            provider: Some(request.route.backend_id),
            safe_detail: "the selected route does not expose exact token counting".to_string(),
        })
    }
    fn cancel(&self, request_id: &RequestId, target: CancelTarget) -> usize;
    /// Stops admission owned by this backend, cancels every backend request,
    /// and does not return until all bridge/provider tasks have terminated.
    ///
    /// This is a request-drain boundary only. A backend borrowing an
    /// application-owned native host must not destroy or permanently close
    /// that host here; process-lifetime host ownership belongs to the embedding
    /// application.
    async fn shutdown(&self) -> Result<(), GatewayError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GatewayLifecycle {
    #[default]
    Running,
    Quiescing,
    Closed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GatewayStatus {
    pub backend_count: usize,
    pub ready_backend_count: usize,
    pub active_requests: usize,
    #[serde(default)]
    pub lifecycle: GatewayLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shutdown_error: Option<GatewayError>,
    pub loopback: Option<LoopbackStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopbackStatus {
    pub enabled: bool,
    pub addresses: Vec<String>,
    pub token_path: Option<String>,
}

pub fn duration_ms(value: Option<u64>) -> Option<Duration> {
    value.map(Duration::from_millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct NoopCancellation;

    impl TicketCancellation for NoopCancellation {
        fn cancel(&self, _target: CancelTarget) -> usize {
            0
        }
    }

    struct CountingCancellation(Arc<AtomicUsize>);

    impl TicketCancellation for CountingCancellation {
        fn cancel(&self, _target: CancelTarget) -> usize {
            self.0.fetch_add(1, Ordering::AcqRel);
            1
        }
    }

    #[test]
    fn request_validation_rejects_empty_chat_before_inference() {
        let request = GatewayRequest {
            request_id: RequestId::new(),
            client_id: "test".to_string(),
            model: ModelSelector::Profile {
                name: "local-only".to_string(),
            },
            input: GenerationInput::Chat { items: Vec::new() },
            sampling: SamplingOptions::default(),
            response_format: ResponseFormat::Text,
            tools: Vec::new(),
            tool_policy: ToolPolicy::default(),
            cache: CachePolicy::default(),
            routing: RoutingPolicy::default(),
            storage: StoragePolicy::default(),
            deadline: DeadlinePolicy::default(),
            stream: StreamPolicy::default(),
            provider_extensions: BTreeMap::new(),
        };
        assert_eq!(
            request.validate().map_err(|error| error.code),
            Err("chat_items_empty".to_string())
        );
    }

    #[test]
    fn only_terminal_events_are_classified_as_terminal() {
        let request_id = RequestId::new();
        let warning = GatewayEvent::Warning {
            request_id,
            code: "notice".to_string(),
            message: "not terminal".to_string(),
        };
        assert!(!warning.is_terminal());
    }

    #[tokio::test]
    async fn final_response_drains_a_bounded_event_stream() {
        let request_id = RequestId::new();
        let route = ResolvedRoute {
            backend_id: "local".to_string(),
            model_id: "model".to_string(),
            display_name: "Model".to_string(),
            location: BackendLocation::LocalEmbedded,
            catalog_version: "test".to_string(),
        };
        let response = GatewayResponse {
            id: "resp".to_string(),
            request_id: request_id.clone(),
            model: route.model_id.clone(),
            route,
            output: Vec::new(),
            usage: GatewayUsage::default(),
            status: TerminalStatus::Completed,
            previous_response_id: None,
        };
        let (event_tx, event_rx) = mpsc::channel(1);
        let (final_tx, final_rx) = oneshot::channel();
        let terminal = Arc::new(AtomicBool::new(false));
        let terminal_for_task = Arc::clone(&terminal);
        let request_for_task = request_id.clone();
        let response_for_task = response.clone();
        tokio::spawn(async move {
            for index in 0..64 {
                event_tx
                    .send(GatewayEvent::Warning {
                        request_id: request_for_task.clone(),
                        code: format!("warning_{index}"),
                        message: "bounded lifecycle event".to_string(),
                    })
                    .await
                    .expect("ticket must drain events");
            }
            terminal_for_task.store(true, Ordering::Release);
            event_tx
                .send(GatewayEvent::Completed {
                    request_id: request_for_task,
                    response: Box::new(response_for_task.clone()),
                })
                .await
                .expect("send terminal");
            final_tx.send(Ok(response_for_task)).expect("send result");
        });
        let ticket = GatewayTicket::new(
            request_id,
            event_rx,
            final_rx,
            Arc::new(NoopCancellation),
            terminal,
        );
        assert_eq!(
            ticket.final_response().await.expect("final response"),
            response
        );
    }

    #[tokio::test]
    async fn first_output_deadline_cancels_and_returns_one_typed_terminal() {
        let request_id = RequestId::new();
        let (_event_tx, event_rx) = mpsc::channel(1);
        let (_final_tx, final_rx) = oneshot::channel();
        let cancellations = Arc::new(AtomicUsize::new(0));
        let mut ticket = GatewayTicket::new(
            request_id,
            event_rx,
            final_rx,
            Arc::new(CountingCancellation(Arc::clone(&cancellations))),
            Arc::new(AtomicBool::new(false)),
        )
        .with_deadlines(
            DeadlinePolicy {
                first_token_ms: Some(5),
                ..DeadlinePolicy::default()
            },
            Duration::ZERO,
            32,
        );

        let event = ticket.events.recv().await.expect("timeout terminal event");
        let GatewayEvent::Failed { error, .. } = event else {
            panic!("first-output timeout must be a failed terminal event");
        };
        assert_eq!(error.code, "first_output_deadline_exceeded");
        assert!(ticket.events.recv().await.is_none());
        assert!(ticket.terminal_observed());
        assert_eq!(cancellations.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn full_event_channel_preserves_exactly_one_timeout_terminal() {
        let request_id = RequestId::new();
        let (event_tx, event_rx) = mpsc::channel(64);
        let (_final_tx, final_rx) = oneshot::channel();
        let cancellations = Arc::new(AtomicUsize::new(0));
        let request_for_events = request_id.clone();
        tokio::spawn(async move {
            for index in 0..64 {
                if event_tx
                    .send(GatewayEvent::Warning {
                        request_id: request_for_events.clone(),
                        code: format!("warning_{index}"),
                        message: "fill the downstream event channel".to_string(),
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        let mut ticket = GatewayTicket::new(
            request_id,
            event_rx,
            final_rx,
            Arc::new(CountingCancellation(Arc::clone(&cancellations))),
            Arc::new(AtomicBool::new(false)),
        )
        .with_deadlines(
            DeadlinePolicy {
                total_ms: Some(10),
                ..DeadlinePolicy::default()
            },
            Duration::ZERO,
            32,
        );

        tokio::time::sleep(Duration::from_millis(30)).await;
        let events = tokio::time::timeout(Duration::from_secs(1), async {
            let mut events = Vec::new();
            while let Some(event) = ticket.events.recv().await {
                events.push(event);
            }
            events
        })
        .await
        .expect("the timed-out wrapper must close its event stream");
        let terminals = events.iter().filter(|event| event.is_terminal()).count();
        assert_eq!(terminals, 1);
        assert!(events.iter().any(|event| matches!(
            event,
            GatewayEvent::Failed { error, .. }
                if error.code == "request_total_deadline_exceeded"
        )));
        assert!(ticket.terminal_observed());
        assert_eq!(cancellations.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn idle_deadline_applies_after_a_stream_lifecycle_event() {
        let request_id = RequestId::new();
        let route = ResolvedRoute {
            backend_id: "local".to_string(),
            model_id: "model".to_string(),
            display_name: "Model".to_string(),
            location: BackendLocation::LocalEmbedded,
            catalog_version: "test".to_string(),
        };
        let (event_tx, event_rx) = mpsc::channel(1);
        let (_final_tx, final_rx) = oneshot::channel();
        let cancellations = Arc::new(AtomicUsize::new(0));
        event_tx
            .send(GatewayEvent::ResponseCreated {
                request_id: request_id.clone(),
                response_id: "resp".to_string(),
                route,
            })
            .await
            .expect("seed lifecycle event");
        let mut ticket = GatewayTicket::new(
            request_id,
            event_rx,
            final_rx,
            Arc::new(CountingCancellation(Arc::clone(&cancellations))),
            Arc::new(AtomicBool::new(false)),
        )
        .with_deadlines(
            DeadlinePolicy {
                idle_stream_ms: Some(5),
                ..DeadlinePolicy::default()
            },
            Duration::ZERO,
            32,
        );

        assert!(matches!(
            ticket.events.recv().await,
            Some(GatewayEvent::ResponseCreated { .. })
        ));
        let event = ticket.events.recv().await.expect("idle timeout terminal");
        let GatewayEvent::Failed { error, .. } = event else {
            panic!("idle timeout must be a failed terminal event");
        };
        assert_eq!(error.code, "stream_idle_deadline_exceeded");
        assert_eq!(cancellations.load(Ordering::Acquire), 1);
    }
}
