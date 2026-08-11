//! Strict compatibility codecs at the Free Token Energy protocol edge.
//!
//! The canonical gateway never stores a Chat-shaped catch-all. Each supported
//! public protocol is parsed into typed fields here and unsupported behavior is
//! rejected before routing.

use fte_types::{
    CacheMode, CachePolicy, CacheRequirement, CompletionPrompt, ContentBlock, DeadlinePolicy,
    GatewayError, GatewayEvent, GatewayRequest, GatewayResponse, GatewayUsage, GenerationInput,
    InputItem, MessageRole, ModelSelector, OutputItem, PrivacyPolicy, ProviderCacheBreakpoint,
    ProviderCacheTtl, RequestId, ResponseFormat, RouteProfile, RoutingPolicy, SamplingOptions,
    StoragePolicy, StreamPolicy, ToolDefinition, ToolExecutionPolicy, ToolOwner, ToolPolicy,
    UsageProvenance,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const DEFAULT_CLIENT_ID: &str = "loopback";

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StopValue {
    One(String),
    Many(Vec<String>),
}

impl StopValue {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(value) => vec![value],
            Self::Many(values) => values,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum CompletionPromptValue {
    Text(String),
    TextBatch(Vec<String>),
    Tokens(Vec<i32>),
    TokenBatch(Vec<Vec<i32>>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiCompletionRequest {
    pub model: String,
    pub prompt: CompletionPromptValue,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub seed: Option<u32>,
    pub stop: Option<StopValue>,
    #[serde(default)]
    pub stream: bool,
    pub user: Option<String>,
    pub n: Option<u32>,
    pub best_of: Option<u32>,
    pub echo: Option<bool>,
    pub logprobs: Option<u32>,
    pub logit_bias: Option<BTreeMap<String, f32>>,
    pub suffix: Option<String>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
}

impl OpenAiCompletionRequest {
    pub fn into_gateway(self, defaults: EdgeDefaults) -> Result<GatewayRequest, GatewayError> {
        let request_id = RequestId::new();
        reject_if(
            &request_id,
            self.n.is_some_and(|value| value != 1),
            "completion_n_unsupported",
            "only n=1 is supported",
        )?;
        reject_if(
            &request_id,
            self.best_of.is_some(),
            "completion_best_of_unsupported",
            "best_of is not supported",
        )?;
        reject_if(
            &request_id,
            self.echo.unwrap_or(false),
            "completion_echo_unsupported",
            "echo is not supported",
        )?;
        reject_if(
            &request_id,
            self.logprobs.is_some(),
            "completion_logprobs_unsupported",
            "logprobs are not supported",
        )?;
        reject_if(
            &request_id,
            self.logit_bias.is_some(),
            "completion_logit_bias_unsupported",
            "logit_bias is not supported",
        )?;
        reject_if(
            &request_id,
            self.suffix.is_some(),
            "completion_suffix_unsupported",
            "suffix is FIM behavior and is not accepted as direct completion",
        )?;
        let prompts = match self.prompt {
            CompletionPromptValue::Text(text) => vec![CompletionPrompt::Text {
                text,
                add_bos: false,
            }],
            CompletionPromptValue::TextBatch(values) => values
                .into_iter()
                .map(|text| CompletionPrompt::Text {
                    text,
                    add_bos: false,
                })
                .collect(),
            CompletionPromptValue::Tokens(token_ids) => {
                vec![CompletionPrompt::Tokens { token_ids }]
            }
            CompletionPromptValue::TokenBatch(values) => values
                .into_iter()
                .map(|token_ids| CompletionPrompt::Tokens { token_ids })
                .collect(),
        };
        Ok(base_request(
            request_id,
            self.user.unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string()),
            self.model,
            GenerationInput::Completion { prompts },
            SamplingOptions {
                max_output_tokens: self.max_tokens,
                temperature: self.temperature,
                top_p: self.top_p,
                seed: self.seed,
                stop: self.stop.map(StopValue::into_vec).unwrap_or_default(),
                presence_penalty: self.presence_penalty,
                frequency_penalty: self.frequency_penalty,
                ..SamplingOptions::default()
            },
            self.stream,
            defaults,
        ))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiChatRequest {
    pub model: String,
    pub messages: Vec<OpenAiMessage>,
    pub max_tokens: Option<u32>,
    pub max_completion_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub seed: Option<u32>,
    pub stop: Option<StopValue>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub stream: bool,
    pub user: Option<String>,
    pub n: Option<u32>,
    pub tools: Option<Vec<OpenAiFunctionTool>>,
    pub tool_choice: Option<Value>,
    pub response_format: Option<OpenAiResponseFormat>,
    pub parallel_tool_calls: Option<bool>,
    pub store: Option<bool>,
}

impl OpenAiChatRequest {
    pub fn into_gateway(self, defaults: EdgeDefaults) -> Result<GatewayRequest, GatewayError> {
        let request_id = RequestId::new();
        reject_if(
            &request_id,
            self.n.is_some_and(|value| value != 1),
            "chat_n_unsupported",
            "only n=1 is supported",
        )?;
        reject_if(
            &request_id,
            self.max_tokens.is_some() && self.max_completion_tokens.is_some(),
            "chat_token_limit_ambiguous",
            "provide max_tokens or max_completion_tokens, not both",
        )?;
        let tools: Vec<ToolDefinition> = self
            .tools
            .unwrap_or_default()
            .into_iter()
            .map(OpenAiFunctionTool::into_canonical)
            .collect();
        let tool_policy = parse_openai_tool_choice(self.tool_choice.as_ref(), !tools.is_empty())?;
        let mut request = base_request(
            request_id,
            self.user.unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string()),
            self.model,
            GenerationInput::Chat {
                items: self
                    .messages
                    .into_iter()
                    .map(OpenAiMessage::into_items)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect(),
            },
            SamplingOptions {
                max_output_tokens: self.max_completion_tokens.or(self.max_tokens),
                temperature: self.temperature,
                top_p: self.top_p,
                seed: self.seed,
                stop: self.stop.map(StopValue::into_vec).unwrap_or_default(),
                presence_penalty: self.presence_penalty,
                frequency_penalty: self.frequency_penalty,
                ..SamplingOptions::default()
            },
            self.stream,
            defaults,
        );
        request.tools = tools;
        request.tool_policy = tool_policy;
        request.response_format = self
            .response_format
            .map(OpenAiResponseFormat::into_canonical)
            .unwrap_or_default();
        request.storage.store_response = self.store.unwrap_or(false);
        if self.parallel_tool_calls == Some(false) {
            request
                .provider_extensions
                .insert("openai.parallel_tool_calls".to_string(), Value::Bool(false));
        }
        Ok(request)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiMessage {
    pub role: String,
    pub content: Option<OpenAiContent>,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
}

impl OpenAiMessage {
    fn into_items(self) -> Result<Vec<InputItem>, GatewayError> {
        let request_id = RequestId::new();
        if self.tool_call_id.is_some() && self.tool_calls.is_some() {
            return Err(GatewayError::invalid_request(
                &request_id,
                "chat_tool_message_ambiguous",
                "a Chat Completions message cannot be both a tool result and an assistant tool call",
            ));
        }
        if let Some(call_id) = self.tool_call_id {
            reject_if(
                &request_id,
                call_id.trim().is_empty(),
                "chat_tool_result_identity_invalid",
                "tool-call results require a non-empty call ID",
            )?;
            if self.role != "tool" {
                return Err(GatewayError::invalid_request(
                    &request_id,
                    "chat_tool_result_role_invalid",
                    "tool_call_id is valid only on a tool-role message",
                ));
            }
            let output = match self.content {
                Some(content) => content.into_blocks()?,
                None => Vec::new(),
            };
            return Ok(vec![InputItem::FunctionResult {
                id: None,
                call_id,
                output,
                is_error: false,
            }]);
        }
        if let Some(tool_calls) = self.tool_calls {
            reject_if(
                &request_id,
                tool_calls.is_empty(),
                "chat_tool_calls_empty",
                "assistant tool_calls must contain at least one function call",
            )?;
            if self.role != "assistant" {
                return Err(GatewayError::invalid_request(
                    &request_id,
                    "chat_tool_call_role_invalid",
                    "tool_calls is valid only on an assistant-role message",
                ));
            }
            let mut items = Vec::with_capacity(tool_calls.len().saturating_add(1));
            if let Some(content) = self.content {
                let content = content.into_blocks()?;
                if !content.is_empty() {
                    items.push(InputItem::Message {
                        id: None,
                        role: MessageRole::Assistant,
                        content,
                    });
                }
            }
            for tool_call in tool_calls {
                items.push(tool_call.into_item(&request_id)?);
            }
            return Ok(items);
        }
        let role = parse_role(&request_id, &self.role)?;
        let _ = self.name;
        let content = match self.content {
            Some(content) => content.into_blocks()?,
            None => Vec::new(),
        };
        Ok(vec![InputItem::Message {
            id: None,
            role,
            content,
        }])
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

impl OpenAiContent {
    fn into_blocks(self) -> Result<Vec<ContentBlock>, GatewayError> {
        match self {
            Self::Text(text) => Ok(vec![ContentBlock::Text { text }]),
            Self::Parts(parts) => parts
                .into_iter()
                .map(OpenAiContentPart::into_block)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum OpenAiContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "input_text")]
    InputText { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: OpenAiImageUrl },
}

impl OpenAiContentPart {
    fn into_block(self) -> Result<ContentBlock, GatewayError> {
        match self {
            Self::Text { text } | Self::InputText { text } => Ok(ContentBlock::Text { text }),
            Self::ImageUrl { image_url } => Ok(ContentBlock::Image {
                source: fte_types::MediaSource::Url { url: image_url.url },
                detail: image_url.detail,
            }),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiImageUrl {
    pub url: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAiFunctionCall,
}

impl OpenAiToolCall {
    fn into_item(self, request_id: &RequestId) -> Result<InputItem, GatewayError> {
        reject_if(
            request_id,
            self.kind != "function",
            "chat_tool_call_type_unsupported",
            "Chat Completions tool-call history supports only function calls",
        )?;
        reject_if(
            request_id,
            self.id.trim().is_empty() || self.function.name.trim().is_empty(),
            "chat_tool_call_identity_invalid",
            "tool-call history requires non-empty call and function names",
        )?;
        let arguments = serde_json::from_str(&self.function.arguments).map_err(|_| {
            GatewayError::invalid_request(
                request_id,
                "chat_tool_call_arguments_invalid",
                "tool-call arguments must be valid JSON",
            )
        })?;
        Ok(InputItem::FunctionCall {
            id: None,
            call_id: self.id,
            name: self.function.name,
            arguments,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiFunctionTool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: OpenAiFunctionDefinition,
}

impl OpenAiFunctionTool {
    fn into_canonical(self) -> ToolDefinition {
        ToolDefinition {
            name: self.function.name,
            description: self.function.description,
            input_schema: self.function.parameters,
            strict: self.function.strict.unwrap_or(false),
            owner: ToolOwner::Client,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiFunctionDefinition {
    pub name: String,
    pub description: Option<String>,
    #[serde(default = "empty_object")]
    pub parameters: Value,
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiResponseFormat {
    Text,
    JsonObject,
    JsonSchema { json_schema: OpenAiJsonSchema },
}

impl OpenAiResponseFormat {
    fn into_canonical(self) -> ResponseFormat {
        match self {
            Self::Text => ResponseFormat::Text,
            Self::JsonObject => ResponseFormat::JsonObject,
            Self::JsonSchema { json_schema } => ResponseFormat::JsonSchema {
                name: json_schema.name,
                schema: json_schema.schema,
                strict: json_schema.strict.unwrap_or(false),
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiJsonSchema {
    pub name: String,
    pub description: Option<String>,
    pub schema: Value,
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiResponsesRequest {
    pub model: String,
    pub input: OpenAiResponsesInput,
    pub instructions: Option<String>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    #[serde(default)]
    pub stream: bool,
    pub store: Option<bool>,
    pub previous_response_id: Option<String>,
    pub tools: Option<Vec<OpenAiResponsesTool>>,
    pub tool_choice: Option<Value>,
    pub parallel_tool_calls: Option<bool>,
    pub text: Option<OpenAiTextConfig>,
    pub reasoning: Option<Value>,
    pub truncation: Option<String>,
    pub include: Option<Vec<String>>,
    pub metadata: Option<BTreeMap<String, String>>,
    pub prompt_cache_key: Option<String>,
    pub prompt_cache_retention: Option<String>,
    pub service_tier: Option<String>,
    pub user: Option<String>,
}

impl OpenAiResponsesRequest {
    pub fn into_gateway(self, defaults: EdgeDefaults) -> Result<GatewayRequest, GatewayError> {
        let request_id = RequestId::new();
        reject_if(
            &request_id,
            self.truncation
                .as_deref()
                .is_some_and(|value| value != "disabled"),
            "responses_truncation_unsupported",
            "automatic truncation is not implemented; use disabled and trim canonical Items explicitly",
        )?;
        let mut items = self.input.into_items()?;
        if let Some(instructions) = self.instructions {
            items.insert(
                0,
                InputItem::Message {
                    id: None,
                    role: MessageRole::Developer,
                    content: vec![ContentBlock::Text { text: instructions }],
                },
            );
        }
        let tools = self
            .tools
            .unwrap_or_default()
            .into_iter()
            .map(OpenAiResponsesTool::into_canonical)
            .collect::<Result<Vec<_>, _>>()?;
        let mut request = base_request(
            request_id,
            self.user.unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string()),
            self.model,
            GenerationInput::Chat { items },
            SamplingOptions {
                max_output_tokens: self.max_output_tokens,
                temperature: self.temperature,
                top_p: self.top_p,
                ..SamplingOptions::default()
            },
            self.stream,
            defaults,
        );
        request.storage = StoragePolicy {
            store_response: self.store.unwrap_or(true),
            previous_response_id: self.previous_response_id,
        };
        request.tools = tools;
        request.tool_policy =
            parse_openai_tool_choice(self.tool_choice.as_ref(), !request.tools.is_empty())?;
        if let Some(text) = self.text {
            request.response_format = text.format.unwrap_or_default().into_canonical();
        }
        if let Some(key) = self.prompt_cache_key {
            request.cache.mode = CacheMode::ProviderNative;
            request.cache.provider_key = Some(key);
        }
        if let Some(retention) = self.prompt_cache_retention {
            request.cache.provider_ttl = Some(match retention.as_str() {
                "24h" => ProviderCacheTtl::TwentyFourHours,
                _ => {
                    return Err(GatewayError::invalid_request(
                        &request.request_id,
                        "responses_cache_retention_unsupported",
                        "the requested prompt cache retention is unsupported",
                    ));
                }
            });
        }
        if let Some(reasoning) = self.reasoning {
            request
                .provider_extensions
                .insert("openai.reasoning".to_string(), reasoning);
        }
        if let Some(include) = self.include {
            request
                .provider_extensions
                .insert("openai.include".to_string(), json!(include));
        }
        if let Some(metadata) = self.metadata {
            request
                .provider_extensions
                .insert("openai.metadata".to_string(), json!(metadata));
        }
        if let Some(service_tier) = self.service_tier {
            request
                .provider_extensions
                .insert("openai.service_tier".to_string(), json!(service_tier));
        }
        if let Some(parallel) = self.parallel_tool_calls {
            request.provider_extensions.insert(
                "openai.parallel_tool_calls".to_string(),
                Value::Bool(parallel),
            );
        }
        Ok(request)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum OpenAiResponsesInput {
    Text(String),
    Items(Vec<OpenAiResponseInputItem>),
}

impl OpenAiResponsesInput {
    fn into_items(self) -> Result<Vec<InputItem>, GatewayError> {
        match self {
            Self::Text(text) => Ok(vec![InputItem::Message {
                id: None,
                role: MessageRole::User,
                content: vec![ContentBlock::Text { text }],
            }]),
            Self::Items(items) => items
                .into_iter()
                .map(OpenAiResponseInputItem::into_canonical)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum OpenAiResponseInputItem {
    #[serde(rename = "message")]
    Message {
        id: Option<String>,
        role: String,
        content: OpenAiContent,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        id: Option<String>,
        call_id: String,
        name: String,
        arguments: String,
    },
    #[serde(rename = "function_call_output")]
    FunctionCallOutput {
        id: Option<String>,
        call_id: String,
        output: String,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        id: Option<String>,
        #[serde(default)]
        summary: Vec<OpenAiReasoningSummary>,
        encrypted_content: Option<String>,
    },
}

impl OpenAiResponseInputItem {
    fn into_canonical(self) -> Result<InputItem, GatewayError> {
        let request_id = RequestId::new();
        match self {
            Self::Message { id, role, content } => Ok(InputItem::Message {
                id,
                role: parse_role(&request_id, &role)?,
                content: content.into_blocks()?,
            }),
            Self::FunctionCall {
                id,
                call_id,
                name,
                arguments,
            } => Ok(InputItem::FunctionCall {
                id,
                call_id,
                name,
                arguments: serde_json::from_str(&arguments).map_err(|_| {
                    GatewayError::invalid_request(
                        &request_id,
                        "responses_function_arguments_invalid",
                        "function call arguments must be valid JSON",
                    )
                })?,
            }),
            Self::FunctionCallOutput {
                id,
                call_id,
                output,
            } => Ok(InputItem::FunctionResult {
                id,
                call_id,
                output: vec![ContentBlock::Text { text: output }],
                is_error: false,
            }),
            Self::Reasoning {
                id,
                summary,
                encrypted_content,
            } => Ok(InputItem::Reasoning {
                id,
                summary: summary.into_iter().map(|value| value.text).collect(),
                opaque_continuation: encrypted_content
                    .map(|content| json!({"encrypted_content": content})),
            }),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiReasoningSummary {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum OpenAiResponsesTool {
    #[serde(rename = "function")]
    Function {
        name: String,
        description: Option<String>,
        parameters: Option<Value>,
        strict: Option<bool>,
    },
}

impl OpenAiResponsesTool {
    fn into_canonical(self) -> Result<ToolDefinition, GatewayError> {
        match self {
            Self::Function {
                name,
                description,
                parameters,
                strict,
            } => Ok(ToolDefinition {
                name,
                description,
                input_schema: parameters.unwrap_or_else(empty_object),
                strict: strict.unwrap_or(false),
                owner: ToolOwner::Client,
            }),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiTextConfig {
    pub format: Option<OpenAiTextFormat>,
    pub verbosity: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiTextFormat {
    #[default]
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        description: Option<String>,
        schema: Value,
        strict: Option<bool>,
    },
}

impl OpenAiTextFormat {
    fn into_canonical(self) -> ResponseFormat {
        match self {
            Self::Text => ResponseFormat::Text,
            Self::JsonObject => ResponseFormat::JsonObject,
            Self::JsonSchema {
                name,
                schema,
                strict,
                ..
            } => ResponseFormat::JsonSchema {
                name,
                schema,
                strict: strict.unwrap_or(false),
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicMessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<AnthropicMessage>,
    pub system: Option<AnthropicSystem>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<i32>,
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    #[serde(default)]
    pub stream: bool,
    pub tools: Option<Vec<AnthropicTool>>,
    pub tool_choice: Option<AnthropicToolChoice>,
    pub thinking: Option<Value>,
    pub metadata: Option<Value>,
    pub service_tier: Option<String>,
}

impl AnthropicMessagesRequest {
    pub fn into_gateway(self, defaults: EdgeDefaults) -> Result<GatewayRequest, GatewayError> {
        let request_id = RequestId::new();
        let cache_breakpoints = anthropic_cache_breakpoints(&self, &request_id)?;
        let mut items = Vec::new();
        if let Some(system) = self.system {
            items.push(InputItem::Message {
                id: None,
                role: MessageRole::System,
                content: system.into_blocks(),
            });
        }
        for message in self.messages {
            items.extend(message.into_items()?);
        }
        let tools = self
            .tools
            .unwrap_or_default()
            .into_iter()
            .map(AnthropicTool::into_canonical)
            .collect();
        let mut request = base_request(
            request_id,
            DEFAULT_CLIENT_ID.to_string(),
            self.model,
            GenerationInput::Chat { items },
            SamplingOptions {
                max_output_tokens: Some(self.max_tokens),
                temperature: self.temperature,
                top_p: self.top_p,
                top_k: self.top_k,
                stop: self.stop_sequences,
                ..SamplingOptions::default()
            },
            self.stream,
            defaults,
        );
        request.tools = tools;
        request.tool_policy = parse_anthropic_tool_choice(self.tool_choice.as_ref());
        if !cache_breakpoints.is_empty() {
            request.cache.mode = CacheMode::ProviderNative;
            request.cache.requirement = CacheRequirement::Required;
            request.cache.provider_breakpoints = cache_breakpoints;
        }
        if let Some(thinking) = self.thinking {
            request
                .provider_extensions
                .insert("anthropic.thinking".to_string(), thinking);
        }
        if let Some(metadata) = self.metadata {
            request
                .provider_extensions
                .insert("anthropic.metadata".to_string(), metadata);
        }
        if let Some(tier) = self.service_tier {
            request
                .provider_extensions
                .insert("anthropic.service_tier".to_string(), json!(tier));
        }
        Ok(request)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AnthropicSystem {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

impl AnthropicSystem {
    fn into_blocks(self) -> Vec<ContentBlock> {
        match self {
            Self::Text(text) => vec![ContentBlock::Text { text }],
            Self::Blocks(blocks) => blocks
                .into_iter()
                .filter_map(AnthropicContentBlock::into_system_block)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicContent,
}

impl AnthropicMessage {
    fn into_items(self) -> Result<Vec<InputItem>, GatewayError> {
        let request_id = RequestId::new();
        let role = parse_role(&request_id, &self.role)?;
        match self.content {
            AnthropicContent::Text(text) => Ok(vec![InputItem::Message {
                id: None,
                role,
                content: vec![ContentBlock::Text { text }],
            }]),
            AnthropicContent::Blocks(blocks) => {
                let mut message_blocks = Vec::new();
                let mut items = Vec::new();
                for block in blocks {
                    match block {
                        AnthropicContentBlock::Text {
                            text,
                            cache_control,
                        } => {
                            if cache_control.is_some() {
                                // Cache breakpoints are preserved on the request below via
                                // provider extensions once a hosted Anthropic route is selected.
                            }
                            message_blocks.push(ContentBlock::Text { text });
                        }
                        AnthropicContentBlock::Image { source } => {
                            message_blocks.push(ContentBlock::Image {
                                source: source.into_media_source(),
                                detail: None,
                            });
                        }
                        AnthropicContentBlock::ToolUse { id, name, input } => {
                            items.push(InputItem::FunctionCall {
                                id: None,
                                call_id: id,
                                name,
                                arguments: input,
                            });
                        }
                        AnthropicContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                            ..
                        } => {
                            items.push(InputItem::FunctionResult {
                                id: None,
                                call_id: tool_use_id,
                                output: content.into_blocks(),
                                is_error: is_error.unwrap_or(false),
                            });
                        }
                        AnthropicContentBlock::Thinking {
                            thinking,
                            signature,
                        } => {
                            message_blocks.push(ContentBlock::Thinking {
                                text: thinking,
                                signature,
                            });
                        }
                        AnthropicContentBlock::RedactedThinking { data } => {
                            message_blocks.push(ContentBlock::RedactedThinking { data });
                        }
                    }
                }
                if !message_blocks.is_empty() {
                    items.insert(
                        0,
                        InputItem::Message {
                            id: None,
                            role,
                            content: message_blocks,
                        },
                    );
                }
                Ok(items)
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

impl AnthropicContent {
    fn into_blocks(self) -> Vec<ContentBlock> {
        match self {
            Self::Text(text) => vec![ContentBlock::Text { text }],
            Self::Blocks(blocks) => blocks
                .into_iter()
                .filter_map(AnthropicContentBlock::into_system_block)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    Text {
        text: String,
        cache_control: Option<AnthropicCacheControl>,
    },
    Image {
        source: AnthropicMediaSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: AnthropicContent,
        is_error: Option<bool>,
        cache_control: Option<AnthropicCacheControl>,
    },
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
}

impl AnthropicContentBlock {
    fn into_system_block(self) -> Option<ContentBlock> {
        match self {
            Self::Text { text, .. } => Some(ContentBlock::Text { text }),
            Self::Image { source } => Some(ContentBlock::Image {
                source: source.into_media_source(),
                detail: None,
            }),
            Self::Thinking {
                thinking,
                signature,
            } => Some(ContentBlock::Thinking {
                text: thinking,
                signature,
            }),
            Self::RedactedThinking { data } => Some(ContentBlock::RedactedThinking { data }),
            Self::ToolUse { .. } | Self::ToolResult { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicMediaSource {
    #[serde(rename = "type")]
    pub kind: String,
    pub media_type: String,
    pub data: String,
}

impl AnthropicMediaSource {
    fn into_media_source(self) -> fte_types::MediaSource {
        fte_types::MediaSource::Bytes {
            mime_type: self.media_type,
            data_base64: self.data,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicCacheControl {
    #[serde(rename = "type")]
    pub kind: String,
    pub ttl: Option<String>,
}

fn anthropic_cache_breakpoints(
    request: &AnthropicMessagesRequest,
    request_id: &RequestId,
) -> Result<Vec<ProviderCacheBreakpoint>, GatewayError> {
    let mut breakpoints = Vec::new();
    if let Some(AnthropicSystem::Blocks(blocks)) = &request.system {
        for (index, block) in blocks.iter().enumerate() {
            if let Some(control) = block.cache_control() {
                breakpoints.push(parse_anthropic_cache_control(
                    request_id,
                    format!("system.{index}"),
                    control,
                )?);
            }
        }
    }
    for (message_index, message) in request.messages.iter().enumerate() {
        if let AnthropicContent::Blocks(blocks) = &message.content {
            for (content_index, block) in blocks.iter().enumerate() {
                if let Some(control) = block.cache_control() {
                    breakpoints.push(parse_anthropic_cache_control(
                        request_id,
                        format!("messages.{message_index}.content.{content_index}"),
                        control,
                    )?);
                }
            }
        }
    }
    if let Some(tools) = &request.tools {
        for (index, tool) in tools.iter().enumerate() {
            if let Some(control) = &tool.cache_control {
                breakpoints.push(parse_anthropic_cache_control(
                    request_id,
                    format!("tools.{index}"),
                    control,
                )?);
            }
        }
    }
    if breakpoints.len() > 4 {
        return Err(GatewayError::invalid_request(
            request_id,
            "anthropic_cache_breakpoint_limit_exceeded",
            "Anthropic requests may contain at most four explicit cache breakpoints",
        ));
    }
    Ok(breakpoints)
}

fn parse_anthropic_cache_control(
    request_id: &RequestId,
    path: String,
    control: &AnthropicCacheControl,
) -> Result<ProviderCacheBreakpoint, GatewayError> {
    if control.kind != "ephemeral" {
        return Err(GatewayError::invalid_request(
            request_id,
            "anthropic_cache_control_type_unsupported",
            "Anthropic cache_control.type must be ephemeral",
        ));
    }
    let ttl = match control.ttl.as_deref() {
        None | Some("5m") => Some(ProviderCacheTtl::FiveMinutes),
        Some("1h") => Some(ProviderCacheTtl::OneHour),
        Some(_) => {
            return Err(GatewayError::invalid_request(
                request_id,
                "anthropic_cache_ttl_unsupported",
                "Anthropic cache_control.ttl must be 5m or 1h",
            ));
        }
    };
    Ok(ProviderCacheBreakpoint { path, ttl })
}

impl AnthropicContentBlock {
    fn cache_control(&self) -> Option<&AnthropicCacheControl> {
        match self {
            Self::Text { cache_control, .. } | Self::ToolResult { cache_control, .. } => {
                cache_control.as_ref()
            }
            Self::Image { .. }
            | Self::ToolUse { .. }
            | Self::Thinking { .. }
            | Self::RedactedThinking { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub cache_control: Option<AnthropicCacheControl>,
}

impl AnthropicTool {
    fn into_canonical(self) -> ToolDefinition {
        ToolDefinition {
            name: self.name,
            description: self.description,
            input_schema: self.input_schema,
            strict: false,
            owner: ToolOwner::Client,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicToolChoice {
    Auto {
        disable_parallel_tool_use: Option<bool>,
    },
    Any {
        disable_parallel_tool_use: Option<bool>,
    },
    Tool {
        name: String,
        disable_parallel_tool_use: Option<bool>,
    },
    None,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicCountTokensRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    pub system: Option<AnthropicSystem>,
    pub tools: Option<Vec<AnthropicTool>>,
    pub thinking: Option<Value>,
}

impl AnthropicCountTokensRequest {
    pub fn into_gateway(self, defaults: EdgeDefaults) -> Result<GatewayRequest, GatewayError> {
        let request = AnthropicMessagesRequest {
            model: self.model,
            max_tokens: 1,
            messages: self.messages,
            system: self.system,
            temperature: None,
            top_p: None,
            top_k: None,
            stop_sequences: Vec::new(),
            stream: false,
            tools: self.tools,
            tool_choice: None,
            thinking: self.thinking,
            metadata: None,
            service_tier: None,
        };
        request.into_gateway(defaults)
    }
}

#[derive(Debug, Clone)]
pub struct EdgeDefaults {
    pub privacy: PrivacyPolicy,
    pub profile: RouteProfile,
}

impl Default for EdgeDefaults {
    fn default() -> Self {
        Self {
            privacy: PrivacyPolicy::LocalOnly,
            profile: RouteProfile::LocalOnly,
        }
    }
}

fn base_request(
    request_id: RequestId,
    client_id: String,
    model: String,
    input: GenerationInput,
    sampling: SamplingOptions,
    stream: bool,
    defaults: EdgeDefaults,
) -> GatewayRequest {
    let selector = match model.split_once('/') {
        Some((backend_id, model_id)) if !backend_id.is_empty() && !model_id.is_empty() => {
            ModelSelector::ExactRoute {
                backend_id: backend_id.to_string(),
                model_id: model_id.to_string(),
            }
        }
        _ if matches!(
            model.as_str(),
            "local-only" | "hosted-only" | "prefer-local" | "auto"
        ) =>
        {
            ModelSelector::Profile { name: model }
        }
        _ => ModelSelector::ExactModel { model_id: model },
    };
    GatewayRequest {
        request_id,
        client_id,
        model: selector,
        input,
        sampling,
        response_format: ResponseFormat::Text,
        tools: Vec::new(),
        tool_policy: ToolPolicy::default(),
        cache: CachePolicy {
            mode: CacheMode::Adaptive,
            requirement: CacheRequirement::Optional,
            ..CachePolicy::default()
        },
        routing: RoutingPolicy {
            privacy: defaults.privacy,
            profile: defaults.profile,
            ..RoutingPolicy::default()
        },
        storage: StoragePolicy::default(),
        deadline: DeadlinePolicy::default(),
        stream: StreamPolicy {
            enabled: stream,
            event_capacity: None,
            latency_sensitive: true,
        },
        provider_extensions: BTreeMap::new(),
    }
}

fn parse_role(request_id: &RequestId, role: &str) -> Result<MessageRole, GatewayError> {
    match role {
        "system" => Ok(MessageRole::System),
        "developer" => Ok(MessageRole::Developer),
        "user" => Ok(MessageRole::User),
        "assistant" => Ok(MessageRole::Assistant),
        "tool" => Ok(MessageRole::Tool),
        _ => Err(GatewayError::invalid_request(
            request_id,
            "message_role_invalid",
            "message role is unsupported",
        )),
    }
}

fn parse_openai_tool_choice(
    choice: Option<&Value>,
    has_tools: bool,
) -> Result<ToolPolicy, GatewayError> {
    let request_id = RequestId::new();
    let execution = match choice {
        None if has_tools => ToolExecutionPolicy::ClientOnly,
        None => ToolExecutionPolicy::Deny,
        Some(Value::String(value)) if value == "none" => ToolExecutionPolicy::Deny,
        Some(Value::String(value)) if matches!(value.as_str(), "auto" | "required") => {
            ToolExecutionPolicy::ClientOnly
        }
        Some(Value::Object(_)) => ToolExecutionPolicy::ClientOnly,
        Some(_) => {
            return Err(GatewayError::invalid_request(
                &request_id,
                "tool_choice_invalid",
                "tool_choice is malformed",
            ));
        }
    };
    Ok(ToolPolicy {
        execution,
        max_turns: None,
    })
}

fn parse_anthropic_tool_choice(choice: Option<&AnthropicToolChoice>) -> ToolPolicy {
    ToolPolicy {
        execution: match choice {
            Some(AnthropicToolChoice::None) | None => ToolExecutionPolicy::Deny,
            Some(_) => ToolExecutionPolicy::ClientOnly,
        },
        max_turns: None,
    }
}

fn reject_if(
    request_id: &RequestId,
    condition: bool,
    code: &str,
    detail: &str,
) -> Result<(), GatewayError> {
    if condition {
        Err(GatewayError::invalid_request(request_id, code, detail))
    } else {
        Ok(())
    }
}

fn empty_object() -> Value {
    json!({})
}

#[must_use]
pub fn openai_responses_json(response: &GatewayResponse) -> Value {
    json!({
        "id": response.id,
        "object": "response",
        "status": match response.status {
            fte_types::TerminalStatus::Completed => "completed",
            fte_types::TerminalStatus::Cancelled => "cancelled",
            fte_types::TerminalStatus::Failed => "failed",
        },
        "model": response.model,
        "previous_response_id": response.previous_response_id,
        "output": response.output.iter().map(openai_output_item).collect::<Vec<_>>(),
        "usage": openai_usage(&response.usage),
        "x_free_token_energy": {
            "backend": response.route.backend_id,
            "location": response.route.location,
            "real_local_inference": response.usage.real_local_inference,
        }
    })
}

fn openai_output_item(item: &OutputItem) -> Value {
    match item {
        OutputItem::Message { id, role, content } => json!({
            "id": id,
            "type": "message",
            "status": "completed",
            "role": match role { MessageRole::Assistant => "assistant", _ => "assistant" },
            "content": content.iter().filter_map(|block| match block {
                ContentBlock::Text { text } => Some(json!({"type":"output_text","text":text,"annotations":[]})),
                _ => None,
            }).collect::<Vec<_>>()
        }),
        OutputItem::FunctionCall {
            id,
            call_id,
            name,
            arguments,
        } => json!({
            "id": id,
            "type": "function_call",
            "status": "completed",
            "call_id": call_id,
            "name": name,
            "arguments": serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string()),
        }),
        OutputItem::Reasoning {
            id,
            summary,
            opaque_continuation,
        } => json!({
            "id": id,
            "type": "reasoning",
            "summary": summary.iter().map(|text| json!({"type":"summary_text","text":text})).collect::<Vec<_>>(),
            "encrypted_content": opaque_continuation,
        }),
    }
}

fn openai_output_content_part(part: &ContentBlock) -> Option<Value> {
    match part {
        ContentBlock::Text { text } => {
            Some(json!({"type":"output_text","text":text,"annotations":[]}))
        }
        ContentBlock::Thinking { text, signature } => {
            Some(json!({"type":"reasoning_text","text":text,"signature":signature}))
        }
        ContentBlock::RedactedThinking { data } => {
            Some(json!({"type":"redacted_reasoning","data":data}))
        }
        ContentBlock::Image { .. } | ContentBlock::Audio { .. } | ContentBlock::Document { .. } => {
            None
        }
    }
}

#[must_use]
pub fn openai_response_event(event: &GatewayEvent) -> Option<(String, Value)> {
    OpenAiResponsesStreamEncoder::default()
        .encode(event)
        .map(|event| (event.event, event.data))
}

#[derive(Debug, Default)]
pub struct OpenAiResponsesStreamEncoder {
    next_sequence_number: u64,
    item_ids: BTreeMap<usize, String>,
    terminal: bool,
}

impl OpenAiResponsesStreamEncoder {
    pub fn encode(&mut self, event: &GatewayEvent) -> Option<SseEvent> {
        if self.terminal {
            return None;
        }
        let (name, mut data) = match event {
            GatewayEvent::ResponseCreated {
                request_id,
                response_id,
                route,
            } => (
                "response.created".to_string(),
                json!({"type":"response.created","response":{"id":response_id,"object":"response","status":"in_progress","model":route.model_id},"request_id":request_id}),
            ),
            GatewayEvent::OutputItemAdded {
                output_index, item, ..
            } => {
                self.item_ids
                    .insert(*output_index, output_item_id(item).to_string());
                let mut encoded = openai_output_item(item);
                if let Some(object) = encoded.as_object_mut() {
                    object.insert("status".to_string(), json!("in_progress"));
                }
                (
                    "response.output_item.added".to_string(),
                    json!({"type":"response.output_item.added","output_index":output_index,"item":encoded}),
                )
            }
            GatewayEvent::ContentPartAdded {
                output_index,
                content_index,
                part,
                ..
            } => {
                let part = openai_output_content_part(part)?;
                (
                    "response.content_part.added".to_string(),
                    json!({"type":"response.content_part.added","item_id":self.item_id(*output_index),"output_index":output_index,"content_index":content_index,"part":part}),
                )
            }
            GatewayEvent::TextDelta {
                output_index,
                content_index,
                delta,
                ..
            } => (
                "response.output_text.delta".to_string(),
                json!({"type":"response.output_text.delta","item_id":self.item_id(*output_index),"output_index":output_index,"content_index":content_index,"delta":delta}),
            ),
            GatewayEvent::ReasoningSummaryDelta {
                output_index,
                summary_index,
                delta,
                ..
            } => (
                "response.reasoning_summary_text.delta".to_string(),
                json!({"type":"response.reasoning_summary_text.delta","item_id":self.item_id(*output_index),"output_index":output_index,"summary_index":summary_index,"delta":delta}),
            ),
            GatewayEvent::FunctionArgumentsDelta {
                output_index,
                delta,
                ..
            } => (
                "response.function_call_arguments.delta".to_string(),
                json!({"type":"response.function_call_arguments.delta","item_id":self.item_id(*output_index),"output_index":output_index,"delta":delta}),
            ),
            GatewayEvent::ContentPartCompleted {
                output_index,
                content_index,
                part,
                ..
            } => {
                let part = openai_output_content_part(part)?;
                (
                    "response.content_part.done".to_string(),
                    json!({"type":"response.content_part.done","item_id":self.item_id(*output_index),"output_index":output_index,"content_index":content_index,"part":part}),
                )
            }
            GatewayEvent::OutputItemCompleted {
                output_index, item, ..
            } => {
                self.item_ids
                    .insert(*output_index, output_item_id(item).to_string());
                (
                    "response.output_item.done".to_string(),
                    json!({"type":"response.output_item.done","output_index":output_index,"item":openai_output_item(item)}),
                )
            }
            GatewayEvent::Completed { response, .. } => {
                self.terminal = true;
                (
                    "response.completed".to_string(),
                    json!({"type":"response.completed","response":openai_responses_json(response)}),
                )
            }
            GatewayEvent::Cancelled { request_id, usage } => {
                self.terminal = true;
                (
                    "response.incomplete".to_string(),
                    json!({"type":"response.incomplete","request_id":request_id,"usage":openai_usage(usage)}),
                )
            }
            GatewayEvent::Failed { error, .. } => {
                self.terminal = true;
                (
                    "error".to_string(),
                    json!({"type":"error","code":error.code,"message":error.safe_detail}),
                )
            }
            GatewayEvent::Warning { code, message, .. } => (
                "response.warning".to_string(),
                json!({"type":"response.warning","code":code,"message":message}),
            ),
            GatewayEvent::UsageUpdated { .. } => return None,
        };
        if let Some(object) = data.as_object_mut() {
            object.insert(
                "sequence_number".to_string(),
                json!(self.next_sequence_number),
            );
        }
        self.next_sequence_number += 1;
        Some(SseEvent { event: name, data })
    }

    fn item_id(&self, output_index: usize) -> &str {
        self.item_ids.get(&output_index).map_or("", String::as_str)
    }
}

fn output_item_id(item: &OutputItem) -> &str {
    match item {
        OutputItem::Message { id, .. }
        | OutputItem::FunctionCall { id, .. }
        | OutputItem::Reasoning { id, .. } => id,
    }
}

#[must_use]
pub fn openai_chat_json(response: &GatewayResponse) -> Value {
    let text = response_text(response);
    let tool_calls = response
        .output
        .iter()
        .filter_map(|item| match item {
            OutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => Some(json!({
                "id":call_id,
                "type":"function",
                "function":{
                    "name":name,
                    "arguments":arguments.to_string(),
                }
            })),
            OutputItem::Message { .. } | OutputItem::Reasoning { .. } => None,
        })
        .collect::<Vec<_>>();
    let mut message = json!({"role":"assistant","content":text});
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    let finish_reason = if message.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    json!({
        "id": format!("chatcmpl_{}", response.id),
        "object": "chat.completion",
        "model": response.model,
        "choices": [{"index":0,"message":message,"finish_reason":finish_reason}],
        "usage": openai_legacy_usage(&response.usage),
        "x_free_token_energy": {"backend":response.route.backend_id}
    })
}

#[must_use]
pub fn openai_completion_json(response: &GatewayResponse) -> Value {
    let choices = response
        .output
        .iter()
        .enumerate()
        .map(|(index, item)| json!({"index":index,"text":output_item_text(item),"finish_reason":"stop","logprobs":Value::Null}))
        .collect::<Vec<_>>();
    json!({
        "id": format!("cmpl_{}", response.id),
        "object":"text_completion",
        "model":response.model,
        "choices":choices,
        "usage":openai_legacy_usage(&response.usage),
        "x_free_token_energy":{"backend":response.route.backend_id}
    })
}

#[must_use]
pub fn anthropic_message_json(response: &GatewayResponse) -> Value {
    let content = response
        .output
        .iter()
        .flat_map(anthropic_output_blocks)
        .collect::<Vec<_>>();
    let stop_reason = if response
        .output
        .iter()
        .any(|item| matches!(item, OutputItem::FunctionCall { .. }))
    {
        "tool_use"
    } else if response.status == fte_types::TerminalStatus::Completed {
        "end_turn"
    } else {
        "stop_sequence"
    };
    json!({
        "id": format!("msg_{}", response.id),
        "type":"message",
        "role":"assistant",
        "model":response.model,
        "content":content,
        "stop_reason":stop_reason,
        "stop_sequence":Value::Null,
        "usage":anthropic_usage(&response.usage),
        "x_free_token_energy":{"backend":response.route.backend_id}
    })
}

fn anthropic_output_blocks(item: &OutputItem) -> Vec<Value> {
    match item {
        OutputItem::Message { content, .. } => content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(json!({"type":"text","text":text})),
                ContentBlock::Thinking { text, signature } => Some(json!({
                    "type":"thinking",
                    "thinking":text,
                    "signature":signature,
                })),
                ContentBlock::RedactedThinking { data } => {
                    Some(json!({"type":"redacted_thinking","data":data}))
                }
                ContentBlock::Image { .. }
                | ContentBlock::Audio { .. }
                | ContentBlock::Document { .. } => None,
            })
            .collect(),
        OutputItem::FunctionCall {
            call_id,
            name,
            arguments,
            ..
        } => vec![json!({
            "type":"tool_use",
            "id":call_id,
            "name":name,
            "input":arguments,
        })],
        OutputItem::Reasoning { summary, .. } => {
            vec![json!({"type":"thinking","thinking":summary.join("\n"),"signature":Value::Null})]
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SseEvent {
    pub event: String,
    pub data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AnthropicBlockKey {
    Content(usize, usize),
    Item(usize),
}

#[derive(Debug)]
pub struct AnthropicStreamEncoder {
    input_tokens: u64,
    next_block_index: usize,
    blocks: BTreeMap<AnthropicBlockKey, usize>,
    stopped_blocks: BTreeSet<usize>,
    delta_blocks: BTreeSet<usize>,
    items: BTreeMap<usize, OutputItem>,
    tool_use: bool,
    terminal: bool,
}

impl AnthropicStreamEncoder {
    #[must_use]
    pub fn new(input_tokens: u64) -> Self {
        Self {
            input_tokens,
            next_block_index: 0,
            blocks: BTreeMap::new(),
            stopped_blocks: BTreeSet::new(),
            delta_blocks: BTreeSet::new(),
            items: BTreeMap::new(),
            tool_use: false,
            terminal: false,
        }
    }

    pub fn encode(&mut self, event: &GatewayEvent) -> Vec<SseEvent> {
        if self.terminal {
            return Vec::new();
        }
        let mut events = Vec::new();
        match event {
            GatewayEvent::ResponseCreated {
                response_id, route, ..
            } => {
                events.push(SseEvent {
                    event: "message_start".to_string(),
                    data: json!({"type":"message_start","message":{"id":format!("msg_{response_id}"),"type":"message","role":"assistant","model":route.model_id,"content":[],"stop_reason":Value::Null,"stop_sequence":Value::Null,"usage":{"input_tokens":self.input_tokens,"output_tokens":0}}}),
                });
            }
            GatewayEvent::OutputItemAdded {
                output_index, item, ..
            } => {
                self.items.insert(*output_index, item.clone());
                match item {
                    OutputItem::FunctionCall { call_id, name, .. } => {
                        self.tool_use = true;
                        self.ensure_block(
                            AnthropicBlockKey::Item(*output_index),
                            json!({"type":"tool_use","id":call_id,"name":name,"input":{}}),
                            &mut events,
                        );
                    }
                    OutputItem::Reasoning { .. } => {
                        self.ensure_block(
                            AnthropicBlockKey::Item(*output_index),
                            json!({"type":"thinking","thinking":""}),
                            &mut events,
                        );
                    }
                    OutputItem::Message { .. } => {}
                }
            }
            GatewayEvent::ContentPartAdded {
                output_index,
                content_index,
                part,
                ..
            } => {
                if let Some(block) = anthropic_content_block_start(part) {
                    self.ensure_block(
                        AnthropicBlockKey::Content(*output_index, *content_index),
                        block,
                        &mut events,
                    );
                }
            }
            GatewayEvent::TextDelta {
                output_index,
                content_index,
                delta,
                ..
            } => {
                let index = self.ensure_block(
                    AnthropicBlockKey::Content(*output_index, *content_index),
                    json!({"type":"text","text":""}),
                    &mut events,
                );
                self.delta_blocks.insert(index);
                events.push(SseEvent {
                    event: "content_block_delta".to_string(),
                    data: json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":delta}}),
                });
            }
            GatewayEvent::ReasoningSummaryDelta {
                output_index,
                delta,
                ..
            } => {
                let index = self.ensure_block(
                    AnthropicBlockKey::Item(*output_index),
                    json!({"type":"thinking","thinking":""}),
                    &mut events,
                );
                self.delta_blocks.insert(index);
                events.push(SseEvent {
                    event: "content_block_delta".to_string(),
                    data: json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":delta}}),
                });
            }
            GatewayEvent::FunctionArgumentsDelta {
                output_index,
                delta,
                ..
            } => {
                let block = self
                    .items
                    .get(output_index)
                    .and_then(|item| match item {
                        OutputItem::FunctionCall { call_id, name, .. } => {
                            Some(json!({"type":"tool_use","id":call_id,"name":name,"input":{}}))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| json!({"type":"tool_use","id":"","name":"","input":{}}));
                self.tool_use = true;
                let index =
                    self.ensure_block(AnthropicBlockKey::Item(*output_index), block, &mut events);
                self.delta_blocks.insert(index);
                events.push(SseEvent {
                    event: "content_block_delta".to_string(),
                    data: json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":delta}}),
                });
            }
            GatewayEvent::ContentPartCompleted {
                output_index,
                content_index,
                part,
                ..
            } => self.complete_content_part(
                AnthropicBlockKey::Content(*output_index, *content_index),
                part,
                &mut events,
            ),
            GatewayEvent::OutputItemCompleted {
                output_index, item, ..
            } => self.complete_output_item(*output_index, item, &mut events),
            GatewayEvent::Completed { response, .. } => {
                self.finish_content_blocks(&mut events);
                events.push(SseEvent {
                    event: "message_delta".to_string(),
                    data: json!({"type":"message_delta","delta":{"stop_reason":if self.tool_use {"tool_use"} else {"end_turn"},"stop_sequence":Value::Null},"usage":anthropic_usage(&response.usage)}),
                });
                events.push(SseEvent {
                    event: "message_stop".to_string(),
                    data: json!({"type":"message_stop"}),
                });
                self.terminal = true;
            }
            GatewayEvent::Cancelled { usage, .. } => {
                self.finish_content_blocks(&mut events);
                events.push(SseEvent {
                    event: "message_delta".to_string(),
                    data: json!({"type":"message_delta","delta":{"stop_reason":"stop_sequence","stop_sequence":Value::Null},"usage":anthropic_usage(usage)}),
                });
                events.push(SseEvent {
                    event: "message_stop".to_string(),
                    data: json!({"type":"message_stop"}),
                });
                self.terminal = true;
            }
            GatewayEvent::Failed { error, .. } => {
                events.push(SseEvent {
                    event: "error".to_string(),
                    data: json!({"type":"error","error":{"type":"api_error","message":error.safe_detail,"code":error.code}}),
                });
                self.terminal = true;
            }
            GatewayEvent::UsageUpdated { .. } | GatewayEvent::Warning { .. } => {}
        }
        events
    }

    fn ensure_block(
        &mut self,
        key: AnthropicBlockKey,
        content_block: Value,
        events: &mut Vec<SseEvent>,
    ) -> usize {
        if let Some(index) = self.blocks.get(&key) {
            return *index;
        }
        let index = self.next_block_index;
        self.next_block_index += 1;
        self.blocks.insert(key, index);
        events.push(SseEvent {
            event: "content_block_start".to_string(),
            data: json!({"type":"content_block_start","index":index,"content_block":content_block}),
        });
        index
    }

    fn complete_content_part(
        &mut self,
        key: AnthropicBlockKey,
        part: &ContentBlock,
        events: &mut Vec<SseEvent>,
    ) {
        let Some(start) = anthropic_content_block_start(part) else {
            return;
        };
        let index = self.ensure_block(key, start, events);
        if !self.delta_blocks.contains(&index) {
            match part {
                ContentBlock::Text { text } if !text.is_empty() => events.push(SseEvent {
                    event: "content_block_delta".to_string(),
                    data: json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}}),
                }),
                ContentBlock::Thinking { text, .. } if !text.is_empty() => {
                    events.push(SseEvent {
                        event: "content_block_delta".to_string(),
                        data: json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":text}}),
                    });
                }
                _ => {}
            }
        }
        if let ContentBlock::Thinking {
            signature: Some(signature),
            ..
        } = part
        {
            events.push(SseEvent {
                event: "content_block_delta".to_string(),
                data: json!({"type":"content_block_delta","index":index,"delta":{"type":"signature_delta","signature":signature}}),
            });
        }
        self.stop_block(index, events);
    }

    fn complete_output_item(
        &mut self,
        output_index: usize,
        item: &OutputItem,
        events: &mut Vec<SseEvent>,
    ) {
        match item {
            OutputItem::Message { content, .. } => {
                for (content_index, part) in content.iter().enumerate() {
                    self.complete_content_part(
                        AnthropicBlockKey::Content(output_index, content_index),
                        part,
                        events,
                    );
                }
            }
            OutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                self.tool_use = true;
                let index = self.ensure_block(
                    AnthropicBlockKey::Item(output_index),
                    json!({"type":"tool_use","id":call_id,"name":name,"input":{}}),
                    events,
                );
                if !self.delta_blocks.contains(&index) {
                    events.push(SseEvent {
                        event: "content_block_delta".to_string(),
                        data: json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string())}}),
                    });
                }
                self.stop_block(index, events);
            }
            OutputItem::Reasoning { summary, .. } => {
                let index = self.ensure_block(
                    AnthropicBlockKey::Item(output_index),
                    json!({"type":"thinking","thinking":""}),
                    events,
                );
                if !self.delta_blocks.contains(&index) && !summary.is_empty() {
                    events.push(SseEvent {
                        event: "content_block_delta".to_string(),
                        data: json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":summary.join("\n")}}),
                    });
                }
                self.stop_block(index, events);
            }
        }
    }

    fn stop_block(&mut self, index: usize, events: &mut Vec<SseEvent>) {
        if self.stopped_blocks.insert(index) {
            events.push(SseEvent {
                event: "content_block_stop".to_string(),
                data: json!({"type":"content_block_stop","index":index}),
            });
        }
    }

    fn finish_content_blocks(&mut self, events: &mut Vec<SseEvent>) {
        let mut indices = self.blocks.values().copied().collect::<Vec<_>>();
        indices.sort_unstable();
        indices.dedup();
        for index in indices {
            self.stop_block(index, events);
        }
    }
}

fn anthropic_content_block_start(part: &ContentBlock) -> Option<Value> {
    match part {
        ContentBlock::Text { .. } => Some(json!({"type":"text","text":""})),
        ContentBlock::Thinking { .. } => Some(json!({"type":"thinking","thinking":""})),
        ContentBlock::RedactedThinking { data } => {
            Some(json!({"type":"redacted_thinking","data":data}))
        }
        ContentBlock::Image { .. } | ContentBlock::Audio { .. } | ContentBlock::Document { .. } => {
            None
        }
    }
}

fn response_text(response: &GatewayResponse) -> String {
    response
        .output
        .iter()
        .map(output_item_text)
        .collect::<Vec<_>>()
        .join("")
}

fn output_item_text(item: &OutputItem) -> String {
    match item {
        OutputItem::Message { content, .. } => content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

fn openai_usage(usage: &GatewayUsage) -> Value {
    let input = usage.input_tokens.unwrap_or_default();
    let output = usage.output_tokens.unwrap_or_default();
    json!({
        "input_tokens":input,
        "output_tokens":output,
        "total_tokens":input + output,
        "input_tokens_details":{"cached_tokens":usage.cache_read_tokens.unwrap_or_default()},
        "output_tokens_details":{"reasoning_tokens":usage.reasoning_tokens.unwrap_or_default()},
        "x_usage_provenance": match usage.provenance {
            UsageProvenance::Exact => "exact",
            UsageProvenance::Estimated => "estimated",
            UsageProvenance::Unknown => "unknown",
        }
    })
}

fn openai_legacy_usage(usage: &GatewayUsage) -> Value {
    let prompt = usage.input_tokens.unwrap_or_default();
    let completion = usage.output_tokens.unwrap_or_default();
    json!({
        "prompt_tokens":prompt,
        "completion_tokens":completion,
        "total_tokens":prompt + completion,
        "prompt_tokens_details":{"cached_tokens":usage.cache_read_tokens.unwrap_or_default()},
        "completion_tokens_details":{"reasoning_tokens":usage.reasoning_tokens.unwrap_or_default()},
        "x_usage_provenance": match usage.provenance {
            UsageProvenance::Exact => "exact",
            UsageProvenance::Estimated => "estimated",
            UsageProvenance::Unknown => "unknown",
        }
    })
}

fn anthropic_usage(usage: &GatewayUsage) -> Value {
    json!({
        "input_tokens":usage.input_tokens.unwrap_or_default(),
        "output_tokens":usage.output_tokens.unwrap_or_default(),
        "cache_creation_input_tokens":usage.cache_write_tokens.unwrap_or_default(),
        "cache_read_input_tokens":usage.cache_read_tokens.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fte_types::{BackendLocation, ResolvedRoute};

    #[test]
    fn openai_surfaces_use_their_protocol_specific_usage_keys() {
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
            request_id,
            model: route.model_id.clone(),
            route,
            output: Vec::new(),
            usage: GatewayUsage {
                input_tokens: Some(7),
                output_tokens: Some(3),
                provenance: UsageProvenance::Exact,
                ..GatewayUsage::default()
            },
            status: fte_types::TerminalStatus::Completed,
            previous_response_id: None,
        };

        let responses = openai_responses_json(&response);
        assert_eq!(responses["usage"]["input_tokens"], 7);
        assert_eq!(responses["usage"]["output_tokens"], 3);
        assert!(responses["usage"].get("prompt_tokens").is_none());

        for legacy in [
            openai_chat_json(&response),
            openai_completion_json(&response),
        ] {
            assert_eq!(legacy["usage"]["prompt_tokens"], 7);
            assert_eq!(legacy["usage"]["completion_tokens"], 3);
            assert!(legacy["usage"].get("input_tokens").is_none());
        }
    }

    #[test]
    fn legacy_completion_preserves_exact_prompt_forms() {
        let request: OpenAiCompletionRequest = serde_json::from_value(json!({
            "model":"local/model",
            "prompt":[[1,2],[3,4]],
            "max_tokens":8
        }))
        .expect("decode completion");
        let gateway = request
            .into_gateway(EdgeDefaults::default())
            .expect("canonical request");
        let GenerationInput::Completion { prompts } = gateway.input else {
            panic!("must remain completion input");
        };
        assert_eq!(prompts.len(), 2);
        assert!(matches!(prompts[0], CompletionPrompt::Tokens { .. }));
    }

    #[test]
    fn unknown_fields_are_rejected_instead_of_dropped() {
        let error = serde_json::from_value::<OpenAiResponsesRequest>(json!({
            "model":"auto",
            "input":"hello",
            "imaginary_parameter":true
        }))
        .expect_err("unknown field must fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn responses_function_items_remain_typed_items() {
        let request: OpenAiResponsesRequest = serde_json::from_value(json!({
            "model":"auto",
            "input":[
                {"type":"function_call","call_id":"call_1","name":"weather","arguments":"{\"city\":\"Boston\"}"},
                {"type":"function_call_output","call_id":"call_1","output":"sunny"}
            ]
        }))
        .expect("decode response request");
        let gateway = request
            .into_gateway(EdgeDefaults::default())
            .expect("canonical request");
        let GenerationInput::Chat { items } = gateway.input else {
            panic!("responses Items become canonical chat Items");
        };
        assert!(matches!(items[0], InputItem::FunctionCall { .. }));
        assert!(matches!(items[1], InputItem::FunctionResult { .. }));
    }

    #[test]
    fn openai_chat_json_preserves_tool_calls_for_replay() {
        let route = ResolvedRoute {
            backend_id: "provider".to_string(),
            model_id: "model".to_string(),
            display_name: "Model".to_string(),
            location: BackendLocation::Hosted,
            catalog_version: "test".to_string(),
        };
        let response = GatewayResponse {
            id: "resp".to_string(),
            request_id: RequestId::new(),
            model: route.model_id.clone(),
            route,
            output: vec![
                OutputItem::Message {
                    id: "message_1".to_string(),
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "Checking.".to_string(),
                    }],
                },
                OutputItem::FunctionCall {
                    id: "item_1".to_string(),
                    call_id: "call_1".to_string(),
                    name: "lookup".to_string(),
                    arguments: json!({"term":"FTE"}),
                },
            ],
            usage: GatewayUsage::default(),
            status: fte_types::TerminalStatus::Completed,
            previous_response_id: None,
        };

        let body = openai_chat_json(&response);
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(body["choices"][0]["message"]["content"], "Checking.");
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"term\":\"FTE\"}"
        );
    }

    #[test]
    fn chat_assistant_tool_history_remains_typed_items() {
        let request: OpenAiChatRequest = serde_json::from_value(json!({
            "model":"auto",
            "messages":[
                {
                    "role":"assistant",
                    "content":"I will check.",
                    "tool_calls":[{
                        "id":"call_1",
                        "type":"function",
                        "function":{"name":"weather","arguments":"{\"city\":\"Boston\"}"}
                    }]
                },
                {"role":"tool","tool_call_id":"call_1","content":"sunny"}
            ]
        }))
        .expect("decode Chat Completions request");
        let gateway = request
            .into_gateway(EdgeDefaults::default())
            .expect("canonical request");
        let GenerationInput::Chat { items } = gateway.input else {
            panic!("chat messages become canonical chat Items");
        };
        assert!(matches!(items[0], InputItem::Message { .. }));
        assert!(matches!(items[1], InputItem::FunctionCall { .. }));
        assert!(matches!(items[2], InputItem::FunctionResult { .. }));
        let InputItem::FunctionCall {
            call_id,
            name,
            arguments,
            ..
        } = &items[1]
        else {
            unreachable!()
        };
        assert_eq!(call_id, "call_1");
        assert_eq!(name, "weather");
        assert_eq!(arguments, &json!({"city":"Boston"}));
    }

    #[test]
    fn chat_tool_history_rejects_invalid_role_kind_and_arguments() {
        for message in [
            json!({
                "role":"user",
                "content":"not an assistant",
                "tool_calls":[{"id":"call_1","type":"function","function":{"name":"tool","arguments":"{}"}}]
            }),
            json!({
                "role":"assistant",
                "content":null,
                "tool_calls":[{"id":"call_1","type":"custom","function":{"name":"tool","arguments":"{}"}}]
            }),
            json!({
                "role":"assistant",
                "content":null,
                "tool_calls":[{"id":"call_1","type":"function","function":{"name":"tool","arguments":"not-json"}}]
            }),
        ] {
            let request: OpenAiChatRequest = serde_json::from_value(json!({
                "model":"auto",
                "messages":[message]
            }))
            .expect("strict message shape");
            assert!(request.into_gateway(EdgeDefaults::default()).is_err());
        }
    }

    #[test]
    fn openai_twenty_four_hour_cache_retention_is_not_downgraded() {
        let request: OpenAiResponsesRequest = serde_json::from_value(json!({
            "model":"hosted/openai",
            "input":"hello",
            "prompt_cache_key":"stable",
            "prompt_cache_retention":"24h"
        }))
        .expect("decode Responses request");
        let gateway = request
            .into_gateway(EdgeDefaults::default())
            .expect("canonical request");
        assert_eq!(
            gateway.cache.provider_ttl,
            Some(ProviderCacheTtl::TwentyFourHours)
        );
    }

    #[test]
    fn anthropic_cache_breakpoints_keep_order_path_and_ttl() {
        let request: AnthropicMessagesRequest = serde_json::from_value(json!({
            "model":"hosted/anthropic",
            "max_tokens":64,
            "system":[{"type":"text","text":"stable system","cache_control":{"type":"ephemeral","ttl":"1h"}}],
            "messages":[{"role":"user","content":[{"type":"text","text":"hello","cache_control":{"type":"ephemeral"}}]}]
        }))
        .expect("decode Anthropic request");
        let gateway = request
            .into_gateway(EdgeDefaults::default())
            .expect("canonical request");
        assert_eq!(gateway.cache.mode, CacheMode::ProviderNative);
        assert_eq!(gateway.cache.requirement, CacheRequirement::Required);
        assert_eq!(gateway.cache.provider_breakpoints.len(), 2);
        assert_eq!(gateway.cache.provider_breakpoints[0].path, "system.0");
        assert_eq!(
            gateway.cache.provider_breakpoints[0].ttl,
            Some(ProviderCacheTtl::OneHour)
        );
        assert_eq!(
            gateway.cache.provider_breakpoints[1].path,
            "messages.0.content.0"
        );
        assert_eq!(
            gateway.cache.provider_breakpoints[1].ttl,
            Some(ProviderCacheTtl::FiveMinutes)
        );
    }

    #[test]
    fn anthropic_stream_order_is_message_block_delta_block_message() {
        let request_id = RequestId::new();
        let route = ResolvedRoute {
            backend_id: "local".to_string(),
            model_id: "model".to_string(),
            display_name: "Model".to_string(),
            location: BackendLocation::LocalEmbedded,
            catalog_version: "test".to_string(),
        };
        let mut encoder = AnthropicStreamEncoder::new(7);
        let mut names = Vec::new();
        let start = encoder.encode(&GatewayEvent::ResponseCreated {
            request_id: request_id.clone(),
            response_id: "resp".to_string(),
            route: route.clone(),
        });
        assert_eq!(
            start[0].data["message"]["usage"]["input_tokens"],
            json!(7),
            "Anthropic message_start must use the exact preflight count"
        );
        names.extend(start.into_iter().map(|event| event.event));
        names.extend(
            encoder
                .encode(&GatewayEvent::TextDelta {
                    request_id: request_id.clone(),
                    output_index: 0,
                    content_index: 0,
                    delta: "hi".to_string(),
                })
                .into_iter()
                .map(|event| event.event),
        );
        let response = GatewayResponse {
            id: "resp".to_string(),
            request_id: request_id.clone(),
            model: "model".to_string(),
            route,
            output: vec![],
            usage: GatewayUsage::default(),
            status: fte_types::TerminalStatus::Completed,
            previous_response_id: None,
        };
        names.extend(
            encoder
                .encode(&GatewayEvent::Completed {
                    request_id,
                    response: Box::new(response),
                })
                .into_iter()
                .map(|event| event.event),
        );
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
    }

    #[test]
    fn anthropic_stream_preserves_thinking_and_tool_use_blocks() {
        let request_id = RequestId::new();
        let route = ResolvedRoute {
            backend_id: "anthropic".to_string(),
            model_id: "claude-test".to_string(),
            display_name: "Claude test".to_string(),
            location: BackendLocation::Hosted,
            catalog_version: "test".to_string(),
        };
        let reasoning = OutputItem::Reasoning {
            id: "reasoning_1".to_string(),
            summary: vec!["check evidence".to_string()],
            opaque_continuation: None,
        };
        let function = OutputItem::FunctionCall {
            id: "tool_1".to_string(),
            call_id: "call_1".to_string(),
            name: "lookup".to_string(),
            arguments: json!({"term":"cache"}),
        };
        let response = GatewayResponse {
            id: "resp".to_string(),
            request_id: request_id.clone(),
            model: route.model_id.clone(),
            route: route.clone(),
            output: vec![reasoning.clone(), function.clone()],
            usage: GatewayUsage {
                input_tokens: Some(9),
                output_tokens: Some(4),
                provenance: UsageProvenance::Exact,
                ..GatewayUsage::default()
            },
            status: fte_types::TerminalStatus::Completed,
            previous_response_id: None,
        };
        let mut encoder = AnthropicStreamEncoder::new(9);
        let events = [
            GatewayEvent::ResponseCreated {
                request_id: request_id.clone(),
                response_id: "resp".to_string(),
                route,
            },
            GatewayEvent::OutputItemAdded {
                request_id: request_id.clone(),
                output_index: 0,
                item: reasoning.clone(),
            },
            GatewayEvent::ReasoningSummaryDelta {
                request_id: request_id.clone(),
                output_index: 0,
                summary_index: 0,
                delta: "check evidence".to_string(),
            },
            GatewayEvent::OutputItemCompleted {
                request_id: request_id.clone(),
                output_index: 0,
                item: reasoning,
            },
            GatewayEvent::OutputItemAdded {
                request_id: request_id.clone(),
                output_index: 1,
                item: function.clone(),
            },
            GatewayEvent::FunctionArgumentsDelta {
                request_id: request_id.clone(),
                output_index: 1,
                delta: "{\"term\":\"cache\"}".to_string(),
            },
            GatewayEvent::OutputItemCompleted {
                request_id: request_id.clone(),
                output_index: 1,
                item: function,
            },
            GatewayEvent::Completed {
                request_id,
                response: Box::new(response.clone()),
            },
        ]
        .iter()
        .flat_map(|event| encoder.encode(event))
        .collect::<Vec<_>>();

        let delta_types = events
            .iter()
            .filter(|event| event.event == "content_block_delta")
            .filter_map(|event| event.data.pointer("/delta/type").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(delta_types, vec!["thinking_delta", "input_json_delta"]);
        assert_eq!(
            events
                .iter()
                .find(|event| event.event == "message_delta")
                .and_then(|event| event.data.pointer("/delta/stop_reason"))
                .and_then(Value::as_str),
            Some("tool_use")
        );

        let message = anthropic_message_json(&response);
        assert_eq!(message.pointer("/content/0/type"), Some(&json!("thinking")));
        assert_eq!(message.pointer("/content/1/type"), Some(&json!("tool_use")));
        assert_eq!(message["stop_reason"], "tool_use");
    }

    #[test]
    fn responses_stream_keeps_sequence_item_identity_and_one_terminal() {
        let request_id = RequestId::new();
        let route = ResolvedRoute {
            backend_id: "local".to_string(),
            model_id: "model".to_string(),
            display_name: "Model".to_string(),
            location: BackendLocation::LocalEmbedded,
            catalog_version: "test".to_string(),
        };
        let item = OutputItem::Message {
            id: "msg_1".to_string(),
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
        };
        let response = GatewayResponse {
            id: "resp_1".to_string(),
            request_id: request_id.clone(),
            model: "model".to_string(),
            route: route.clone(),
            output: vec![item.clone()],
            usage: GatewayUsage::default(),
            status: fte_types::TerminalStatus::Completed,
            previous_response_id: None,
        };
        let events = [
            GatewayEvent::ResponseCreated {
                request_id: request_id.clone(),
                response_id: "resp_1".to_string(),
                route,
            },
            GatewayEvent::OutputItemAdded {
                request_id: request_id.clone(),
                output_index: 0,
                item: item.clone(),
            },
            GatewayEvent::ContentPartAdded {
                request_id: request_id.clone(),
                output_index: 0,
                content_index: 0,
                part: ContentBlock::Text {
                    text: String::new(),
                },
            },
            GatewayEvent::TextDelta {
                request_id: request_id.clone(),
                output_index: 0,
                content_index: 0,
                delta: "hi".to_string(),
            },
            GatewayEvent::ContentPartCompleted {
                request_id: request_id.clone(),
                output_index: 0,
                content_index: 0,
                part: ContentBlock::Text {
                    text: "hi".to_string(),
                },
            },
            GatewayEvent::OutputItemCompleted {
                request_id: request_id.clone(),
                output_index: 0,
                item,
            },
            GatewayEvent::Completed {
                request_id: request_id.clone(),
                response: Box::new(response),
            },
        ];
        let mut encoder = OpenAiResponsesStreamEncoder::default();
        let encoded = events
            .iter()
            .filter_map(|event| encoder.encode(event))
            .collect::<Vec<_>>();
        assert_eq!(encoded.len(), 7);
        for (sequence, event) in encoded.iter().enumerate() {
            assert_eq!(
                event.data["sequence_number"],
                json!(u64::try_from(sequence).expect("small fixture"))
            );
        }
        assert_eq!(encoded[3].data["item_id"], "msg_1");
        assert_eq!(
            encoded.last().map(|event| event.event.as_str()),
            Some("response.completed")
        );
        assert!(
            encoder
                .encode(&GatewayEvent::Failed {
                    request_id,
                    error: GatewayError::unavailable(
                        &RequestId::new(),
                        "late_failure",
                        "late failure",
                    ),
                })
                .is_none(),
            "a response stream may emit exactly one terminal event"
        );
    }
}
