use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::providers::streaming::StreamParserKind;
use crate::providers::{
    Capability, ChatChoice, ChatMessage, ChatRequest, ChatResponse, ChatUsage, text_from_value,
};

#[derive(Debug, Clone)]
pub struct ProviderSpec {
    id: &'static str,
    name: &'static str,
    capabilities: Vec<Capability>,
    auth: AuthScheme,
    url_shape: UrlShape,
    request_transform: RequestTransformKind,
    response_transform: ResponseTransformKind,
    stream_parser: StreamParserKind,
    static_headers: Vec<StaticHeader>,
}

#[derive(Debug, Clone)]
pub struct PreparedProviderRequest {
    pub url: String,
    pub headers: HeaderMap,
    pub body: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestMode {
    NonStreaming,
    Streaming,
}

#[derive(Debug, Clone)]
pub enum AuthScheme {
    Bearer,
    Header {
        name: &'static str,
        prefix: Option<&'static str>,
    },
    None,
}

#[derive(Debug, Clone)]
pub enum UrlShape {
    FixedChatCompletions { endpoint: &'static str },
    GeminiGenerateContent { base_url: &'static str },
}

#[derive(Debug, Clone)]
pub enum RequestTransformKind {
    OpenAiCompatible,
    AnthropicMessages,
    GeminiGenerateContent,
}

#[derive(Debug, Clone)]
pub enum ResponseTransformKind {
    OpenAiCompatible,
    AnthropicMessages,
    GeminiGenerateContent,
}

#[derive(Debug, Clone)]
pub struct StaticHeader {
    name: &'static str,
    value: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParameterPolicy {
    #[serde(default)]
    pub rename_parameters: Vec<ParameterRename>,
    #[serde(default)]
    pub drop_parameters: Vec<String>,
    #[serde(default = "default_include_usage_on_stream")]
    pub include_usage_on_stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParameterRename {
    pub from: String,
    pub to: String,
}

impl Default for ParameterPolicy {
    fn default() -> Self {
        Self::openai_compatible()
    }
}

impl ParameterPolicy {
    pub fn openai_compatible() -> Self {
        Self {
            rename_parameters: Vec::new(),
            drop_parameters: Vec::new(),
            include_usage_on_stream: true,
        }
    }

    pub fn mistral() -> Self {
        Self {
            rename_parameters: vec![
                ParameterRename::new("max_completion_tokens", "max_tokens"),
                ParameterRename::new("seed", "random_seed"),
            ],
            drop_parameters: strings(&[
                "parallel_tool_calls",
                "logit_bias",
                "presence_penalty",
                "frequency_penalty",
            ]),
            include_usage_on_stream: true,
        }
    }

    pub fn max_completion_tokens_to_max_tokens() -> Self {
        Self {
            rename_parameters: vec![ParameterRename::new("max_completion_tokens", "max_tokens")],
            drop_parameters: Vec::new(),
            include_usage_on_stream: true,
        }
    }

    pub fn anthropic() -> Self {
        Self {
            rename_parameters: Vec::new(),
            drop_parameters: strings(&[
                "n",
                "frequency_penalty",
                "presence_penalty",
                "logit_bias",
                "parallel_tool_calls",
                "response_format",
                "stream_options",
            ]),
            include_usage_on_stream: false,
        }
    }

    pub fn gemini() -> Self {
        Self {
            rename_parameters: Vec::new(),
            drop_parameters: strings(&[
                "n",
                "frequency_penalty",
                "presence_penalty",
                "logit_bias",
                "parallel_tool_calls",
                "stream_options",
            ]),
            include_usage_on_stream: false,
        }
    }

    pub fn apply(&self, body: &mut Value) {
        let Some(object) = body.as_object_mut() else {
            return;
        };

        for rename in &self.rename_parameters {
            if rename.from == rename.to {
                continue;
            }

            let Some(value) = object.remove(&rename.from) else {
                continue;
            };

            let should_insert = object
                .get(&rename.to)
                .map(|existing| existing.is_null())
                .unwrap_or(true);
            if should_insert {
                object.insert(rename.to.clone(), value);
            } else {
                object.insert(rename.from.clone(), value);
            }
        }

        for parameter in &self.drop_parameters {
            object.remove(parameter);
        }
    }
}

impl ParameterRename {
    pub fn new(from: &str, to: &str) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
        }
    }
}

impl ProviderSpec {
    fn new(
        identity: (&'static str, &'static str),
        capabilities: Vec<Capability>,
        auth: AuthScheme,
        url_shape: UrlShape,
        request_transform: RequestTransformKind,
        response_transform: ResponseTransformKind,
        stream_parser: StreamParserKind,
    ) -> Self {
        let (id, name) = identity;
        Self {
            id,
            name,
            capabilities,
            auth,
            url_shape,
            request_transform,
            response_transform,
            stream_parser,
            static_headers: Vec::new(),
        }
    }

    pub fn openai_compatible(
        id: &'static str,
        name: &'static str,
        chat_endpoint: &'static str,
        capabilities: Vec<Capability>,
    ) -> Self {
        Self::new(
            (id, name),
            capabilities,
            AuthScheme::Bearer,
            UrlShape::FixedChatCompletions {
                endpoint: chat_endpoint,
            },
            RequestTransformKind::OpenAiCompatible,
            ResponseTransformKind::OpenAiCompatible,
            StreamParserKind::OpenAiSse,
        )
    }

    pub fn anthropic(
        id: &'static str,
        name: &'static str,
        messages_endpoint: &'static str,
        capabilities: Vec<Capability>,
    ) -> Self {
        Self::new(
            (id, name),
            capabilities,
            AuthScheme::Header {
                name: "x-api-key",
                prefix: None,
            },
            UrlShape::FixedChatCompletions {
                endpoint: messages_endpoint,
            },
            RequestTransformKind::AnthropicMessages,
            ResponseTransformKind::AnthropicMessages,
            StreamParserKind::AnthropicSse,
        )
        .with_static_header("anthropic-version", "2023-06-01")
    }

    pub fn gemini(
        id: &'static str,
        name: &'static str,
        base_url: &'static str,
        capabilities: Vec<Capability>,
    ) -> Self {
        Self::new(
            (id, name),
            capabilities,
            AuthScheme::Header {
                name: "x-goog-api-key",
                prefix: None,
            },
            UrlShape::GeminiGenerateContent { base_url },
            RequestTransformKind::GeminiGenerateContent,
            ResponseTransformKind::GeminiGenerateContent,
            StreamParserKind::GeminiSse,
        )
    }

    pub fn with_static_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.static_headers.push(StaticHeader { name, value });
        self
    }

    pub fn id(&self) -> &str {
        self.id
    }

    pub fn name(&self) -> &str {
        self.name
    }

    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    pub fn stream_parser(&self) -> StreamParserKind {
        self.stream_parser
    }

    pub fn prepare_chat(
        &self,
        req: &ChatRequest,
        mode: RequestMode,
        policy: &ParameterPolicy,
        api_key: &str,
    ) -> anyhow::Result<PreparedProviderRequest> {
        let mut body = self.request_transform.transform_chat(req, mode, policy)?;
        policy.apply(&mut body);
        let mut headers = self.headers(api_key)?;
        apply_dynamic_headers(&mut headers, &mut body)?;

        Ok(PreparedProviderRequest {
            url: self.url_shape.chat_completions_url(req, mode),
            headers,
            body,
        })
    }

    pub fn transform_chat_response(&self, body: Value) -> anyhow::Result<ChatResponse> {
        self.response_transform.transform_chat_response(body)
    }

    pub(crate) fn headers(&self, api_key: &str) -> anyhow::Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        self.auth.apply(&mut headers, api_key)?;

        for header in &self.static_headers {
            headers.insert(
                HeaderName::from_bytes(header.name.as_bytes())?,
                HeaderValue::from_str(header.value)?,
            );
        }

        Ok(headers)
    }
}

impl AuthScheme {
    fn apply(&self, headers: &mut HeaderMap, api_key: &str) -> anyhow::Result<()> {
        match self {
            AuthScheme::Bearer => {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {api_key}"))?,
                );
            }
            AuthScheme::Header { name, prefix } => {
                let value = match prefix {
                    Some(prefix) => format!("{prefix}{api_key}"),
                    None => api_key.to_string(),
                };
                headers.insert(
                    HeaderName::from_bytes(name.as_bytes())?,
                    HeaderValue::from_str(&value)?,
                );
            }
            AuthScheme::None => {}
        }

        Ok(())
    }
}

impl UrlShape {
    fn chat_completions_url(&self, req: &ChatRequest, mode: RequestMode) -> String {
        match self {
            UrlShape::FixedChatCompletions { endpoint } => endpoint.to_string(),
            UrlShape::GeminiGenerateContent { base_url } => {
                let model = req.model.strip_prefix("models/").unwrap_or(&req.model);
                let method = match mode {
                    RequestMode::NonStreaming => "generateContent",
                    RequestMode::Streaming => "streamGenerateContent",
                };
                let suffix = match mode {
                    RequestMode::NonStreaming => String::new(),
                    RequestMode::Streaming => "?alt=sse".to_string(),
                };
                format!(
                    "{}/models/{}:{}{}",
                    base_url.trim_end_matches('/'),
                    model,
                    method,
                    suffix
                )
            }
        }
    }
}

impl RequestTransformKind {
    fn transform_chat(
        &self,
        req: &ChatRequest,
        mode: RequestMode,
        policy: &ParameterPolicy,
    ) -> anyhow::Result<Value> {
        match self {
            RequestTransformKind::OpenAiCompatible => {
                let mut body = serde_json::to_value(req)?;
                let object = body
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("chat request did not serialize as object"))?;

                object.insert(
                    "stream".to_string(),
                    Value::Bool(matches!(mode, RequestMode::Streaming)),
                );

                if matches!(mode, RequestMode::Streaming) && policy.include_usage_on_stream {
                    match object.get_mut("stream_options") {
                        Some(Value::Object(options)) => {
                            options
                                .entry("include_usage".to_string())
                                .or_insert(Value::Bool(true));
                        }
                        Some(_) | None => {
                            object.insert(
                                "stream_options".to_string(),
                                json!({
                                    "include_usage": true
                                }),
                            );
                        }
                    }
                }

                if matches!(mode, RequestMode::NonStreaming) {
                    object.remove("stream_options");
                }

                Ok(body)
            }
            RequestTransformKind::AnthropicMessages => anthropic_request_body(req, mode),
            RequestTransformKind::GeminiGenerateContent => gemini_request_body(req),
        }
    }
}

impl ResponseTransformKind {
    fn transform_chat_response(&self, body: Value) -> anyhow::Result<ChatResponse> {
        match self {
            ResponseTransformKind::OpenAiCompatible => Ok(serde_json::from_value(body)?),
            ResponseTransformKind::AnthropicMessages => anthropic_response_to_chat_response(body),
            ResponseTransformKind::GeminiGenerateContent => gemini_response_to_chat_response(body),
        }
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

fn default_include_usage_on_stream() -> bool {
    true
}

fn anthropic_request_body(req: &ChatRequest, mode: RequestMode) -> anyhow::Result<Value> {
    let mut system_blocks = Vec::new();
    let mut messages = Vec::new();

    for message in &req.messages {
        match message.role.as_str() {
            "system" | "developer" => {
                for block in anthropic_content_blocks(&message.content) {
                    system_blocks.push(block);
                }
            }
            "tool" => {
                messages.push(json!({
                    "role": "user",
                    "content": [anthropic_tool_result_block(message)]
                }));
            }
            "assistant" => {
                let mut content = anthropic_content_blocks(&message.content);
                content.extend(anthropic_assistant_extra_blocks(message));
                messages.push(json!({
                    "role": "assistant",
                    "content": content
                }));
            }
            _ => {
                messages.push(json!({
                    "role": "user",
                    "content": anthropic_content_blocks(&message.content)
                }));
            }
        }
    }

    let max_tokens = req
        .max_tokens
        .map(u64::from)
        .or_else(|| u64_extra(req, "max_tokens"))
        .or_else(|| u64_extra(req, "max_completion_tokens"))
        .unwrap_or(4096);

    let messages = merge_consecutive_anthropic_messages(messages);

    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(req.model.clone()));
    body.insert("messages".to_string(), Value::Array(messages));
    body.insert("max_tokens".to_string(), Value::from(max_tokens));
    body.insert(
        "stream".to_string(),
        Value::Bool(matches!(mode, RequestMode::Streaming)),
    );

    if !system_blocks.is_empty() {
        body.insert("system".to_string(), Value::Array(system_blocks));
    }

    if let Some(temperature) = req.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }

    // Anthropic recommends changing temperature or top_p, not both.
    if req.temperature.is_none() {
        copy_extra(req, &mut body, "top_p", "top_p");
    }
    copy_extra(req, &mut body, "top_k", "top_k");
    copy_extra(req, &mut body, "metadata", "metadata");
    copy_extra(req, &mut body, "thinking", "thinking");
    copy_extra(req, &mut body, "anthropic_beta", "anthropic_beta");

    if let Some(stop) = req.extra.get("stop") {
        body.insert("stop_sequences".to_string(), normalize_stop_sequences(stop));
    }

    if let Some(tools) = req.extra.get("tools").and_then(Value::as_array) {
        let converted = tools
            .iter()
            .filter_map(tool_to_anthropic)
            .collect::<Vec<_>>();
        if !converted.is_empty() {
            body.insert("tools".to_string(), Value::Array(converted));
        }
    }

    if let Some(tool_choice) = req.extra.get("tool_choice") {
        body.insert(
            "tool_choice".to_string(),
            anthropic_tool_choice(tool_choice),
        );
    }

    Ok(Value::Object(body))
}

fn gemini_request_body(req: &ChatRequest) -> anyhow::Result<Value> {
    let mut contents = Vec::new();
    let mut system_parts = Vec::new();

    for message in &req.messages {
        match message.role.as_str() {
            "system" | "developer" => {
                system_parts.extend(gemini_parts_from_content(&message.content));
            }
            "assistant" | "model" => {
                let mut parts = gemini_parts_from_content(&message.content);
                parts.extend(gemini_function_calls_from_message(message));
                contents.push(json!({
                    "role": "model",
                    "parts": parts
                }));
            }
            "tool" => {
                contents.push(json!({
                    "role": "user",
                    "parts": [gemini_function_response_from_tool_message(message)]
                }));
            }
            _ => {
                contents.push(json!({
                    "role": "user",
                    "parts": gemini_parts_from_content(&message.content)
                }));
            }
        }
    }

    let mut body = Map::new();
    body.insert("contents".to_string(), Value::Array(contents));

    if !system_parts.is_empty() {
        body.insert(
            "systemInstruction".to_string(),
            json!({
                "parts": system_parts
            }),
        );
    }

    let mut generation_config = Map::new();
    if let Some(max_tokens) = req.max_tokens {
        generation_config.insert("maxOutputTokens".to_string(), json!(max_tokens));
    }
    if let Some(temperature) = req.temperature {
        generation_config.insert("temperature".to_string(), json!(temperature));
    }
    copy_extra_to_map(req, &mut generation_config, "top_p", "topP");
    copy_extra_to_map(req, &mut generation_config, "top_k", "topK");
    copy_extra_to_map(req, &mut generation_config, "stop", "stopSequences");
    copy_extra_to_map(
        req,
        &mut generation_config,
        "thinking_config",
        "thinkingConfig",
    );
    if let Some(response_format) = req.extra.get("response_format")
        && response_format
            .get("type")
            .and_then(Value::as_str)
            .map(|kind| kind == "json_object" || kind == "json_schema")
            .unwrap_or(false)
    {
        generation_config.insert(
            "responseMimeType".to_string(),
            Value::String("application/json".to_string()),
        );
    }
    if !generation_config.is_empty() {
        body.insert(
            "generationConfig".to_string(),
            Value::Object(generation_config),
        );
    }

    if let Some(tools) = req.extra.get("tools").and_then(Value::as_array) {
        if tools
            .iter()
            .any(|tool| tool.get("functionDeclarations").is_some())
        {
            body.insert("tools".to_string(), Value::Array(tools.clone()));
        } else {
            let function_declarations = tools
                .iter()
                .filter_map(openai_tool_to_gemini_function_declaration)
                .collect::<Vec<_>>();
            if !function_declarations.is_empty() {
                body.insert(
                    "tools".to_string(),
                    json!([{ "functionDeclarations": function_declarations }]),
                );
            }
        }
    }

    if let Some(tool_choice) = req.extra.get("tool_choice") {
        body.insert("toolConfig".to_string(), gemini_tool_config(tool_choice));
    } else {
        copy_extra(req, &mut body, "tool_config", "toolConfig");
    }

    copy_extra(req, &mut body, "safety_settings", "safetySettings");
    copy_extra(req, &mut body, "cached_content", "cachedContent");

    Ok(Value::Object(body))
}

fn anthropic_response_to_chat_response(body: Value) -> anyhow::Result<ChatResponse> {
    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in body
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    text_parts.push(text.to_string());
                }
            }
            Some("thinking") => {
                if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                    reasoning_parts.push(text.to_string());
                }
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("tool_use")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let arguments = block
                    .get("input")
                    .map(|input| input.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments
                    }
                }));
            }
            _ => {}
        }
    }

    let mut message_extra = Map::new();
    if !tool_calls.is_empty() {
        message_extra.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    if !reasoning_parts.is_empty() {
        message_extra.insert(
            "reasoning_content".to_string(),
            Value::String(reasoning_parts.join("\n")),
        );
    }

    let usage = body.get("usage").map(|usage| {
        let prompt_tokens = [
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ]
        .iter()
        .filter_map(|key| usage.get(key).and_then(Value::as_u64))
        .map(saturating_u32)
        .fold(0, u32::saturating_add);
        let completion_tokens = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .map(saturating_u32)
            .unwrap_or_default();
        ChatUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
        }
    });

    Ok(ChatResponse {
        id: body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("anthropic-message")
            .to_string(),
        object: Some("chat.completion".to_string()),
        created: None,
        model: body
            .get("model")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        choices: vec![ChatChoice {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Value::String(text_parts.join("")),
                extra: message_extra,
            },
            finish_reason: body
                .get("stop_reason")
                .and_then(Value::as_str)
                .map(map_anthropic_finish_reason),
        }],
        usage,
    })
}

fn gemini_response_to_chat_response(body: Value) -> anyhow::Result<ChatResponse> {
    let candidate = body
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first());

    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut tool_calls = Vec::new();

    if let Some(parts) = candidate
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if part
                    .get("thought")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    reasoning_parts.push(text.to_string());
                } else {
                    text_parts.push(text.to_string());
                }
            }

            if let Some(function_call) = part.get("functionCall") {
                let name = function_call
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("function");
                let args = function_call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                tool_calls.push(json!({
                    "id": format!("call_{}", tool_calls.len()),
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": args.to_string()
                    }
                }));
            }
        }
    }

    let mut message_extra = Map::new();
    if !tool_calls.is_empty() {
        message_extra.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    if !reasoning_parts.is_empty() {
        message_extra.insert(
            "reasoning_content".to_string(),
            Value::String(reasoning_parts.join("\n")),
        );
    }

    let usage = body.get("usageMetadata").map(gemini_usage);

    Ok(ChatResponse {
        id: "gemini-message".to_string(),
        object: Some("chat.completion".to_string()),
        created: None,
        model: None,
        choices: vec![ChatChoice {
            message: ChatMessage {
                role: "assistant".to_string(),
                content: Value::String(text_parts.join("")),
                extra: message_extra,
            },
            finish_reason: candidate
                .and_then(|candidate| candidate.get("finishReason"))
                .and_then(Value::as_str)
                .map(map_gemini_finish_reason),
        }],
        usage,
    })
}

fn anthropic_content_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if let Some(text) = block.as_str() {
                    return Some(json!({ "type": "text", "text": text }));
                }
                if block.get("type").and_then(Value::as_str).is_some() {
                    return Some(block.clone());
                }
                None
            })
            .collect(),
        Value::String(text) => vec![json!({ "type": "text", "text": text })],
        Value::Null => Vec::new(),
        other => vec![json!({ "type": "text", "text": other.to_string() })],
    }
}

fn anthropic_tool_result_block(message: &ChatMessage) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": message
            .extra
            .get("tool_call_id")
            .or_else(|| message.extra.get("tool_use_id"))
            .and_then(Value::as_str)
            .unwrap_or("tool_use"),
        "content": text_from_value(&message.content)
    })
}

fn anthropic_assistant_extra_blocks(message: &ChatMessage) -> Vec<Value> {
    let mut blocks = Vec::new();

    if let Some(reasoning) = message
        .extra
        .get("reasoning_content")
        .or_else(|| message.extra.get("thinking"))
        .and_then(Value::as_str)
    {
        blocks.push(json!({
            "type": "thinking",
            "thinking": reasoning
        }));
    }

    if let Some(tool_calls) = message.extra.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            let Some(function) = tool_call.get("function") else {
                continue;
            };
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            let input = function
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                .unwrap_or_else(|| json!({}));
            blocks.push(json!({
                "type": "tool_use",
                "id": tool_call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("tool_use"),
                "name": name,
                "input": input
            }));
        }
    }

    blocks
}

fn gemini_parts_from_content(content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => vec![json!({ "text": text })],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if let Some(text) = part.as_str() {
                    return Some(json!({ "text": text }));
                }
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    return Some(json!({ "text": text }));
                }
                if part.get("inlineData").is_some()
                    || part.get("fileData").is_some()
                    || part.get("functionCall").is_some()
                    || part.get("functionResponse").is_some()
                {
                    return Some(part.clone());
                }
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    return part
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|text| json!({ "text": text }));
                }
                None
            })
            .collect(),
        Value::Null => Vec::new(),
        other => vec![json!({ "text": other.to_string() })],
    }
}

fn gemini_function_calls_from_message(message: &ChatMessage) -> Vec<Value> {
    message
        .extra
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool_call| {
            let function = tool_call.get("function")?;
            let name = function.get("name").and_then(Value::as_str)?;
            let args = function
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                .unwrap_or_else(|| json!({}));
            Some(json!({
                "functionCall": {
                    "name": name,
                    "args": args
                }
            }))
        })
        .collect()
}

fn gemini_function_response_from_tool_message(message: &ChatMessage) -> Value {
    let name = message
        .extra
        .get("name")
        .or_else(|| message.extra.get("tool_name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let response = serde_json::from_str::<Value>(&text_from_value(&message.content))
        .unwrap_or_else(|_| json!({ "content": text_from_value(&message.content) }));
    json!({
        "functionResponse": {
            "name": name,
            "response": response
        }
    })
}

fn tool_to_anthropic(tool: &Value) -> Option<Value> {
    let function = tool.get("function").unwrap_or(tool);
    let name = function.get("name")?.clone();
    let mut object = Map::new();
    object.insert("name".to_string(), name);
    if let Some(description) = function.get("description") {
        object.insert("description".to_string(), description.clone());
    }
    object.insert(
        "input_schema".to_string(),
        function
            .get("parameters")
            .or_else(|| function.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
    );
    Some(Value::Object(object))
}

fn merge_consecutive_anthropic_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_string();
        let content = message
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if let Some(previous) = merged.last_mut()
            && previous.get("role").and_then(Value::as_str) == Some(role.as_str())
            && let Some(previous_content) =
                previous.get_mut("content").and_then(Value::as_array_mut)
        {
            previous_content.extend(content);
            continue;
        }

        merged.push(json!({
            "role": role,
            "content": content
        }));
    }

    merged
}

fn apply_dynamic_headers(headers: &mut HeaderMap, body: &mut Value) -> anyhow::Result<()> {
    let Some(object) = body.as_object_mut() else {
        return Ok(());
    };

    if let Some(beta) = object.remove("anthropic_beta")
        && let Some(beta) = beta.as_str()
    {
        headers.insert(
            HeaderName::from_static("anthropic-beta"),
            HeaderValue::from_str(beta)?,
        );
    }

    Ok(())
}

fn openai_tool_to_gemini_function_declaration(tool: &Value) -> Option<Value> {
    let function = tool.get("function").unwrap_or(tool);
    let name = function.get("name")?.clone();
    let mut object = Map::new();
    object.insert("name".to_string(), name);
    if let Some(description) = function.get("description") {
        object.insert("description".to_string(), description.clone());
    }
    if let Some(parameters) = function.get("parameters") {
        object.insert("parameters".to_string(), parameters.clone());
    }
    Some(Value::Object(object))
}

fn anthropic_tool_choice(tool_choice: &Value) -> Value {
    match tool_choice {
        Value::String(choice) if choice == "auto" => json!({ "type": "auto" }),
        Value::String(choice) if choice == "none" => json!({ "type": "none" }),
        Value::String(choice) if choice == "required" => json!({ "type": "any" }),
        Value::Object(object) => {
            if let Some(name) = object
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
            {
                json!({ "type": "tool", "name": name })
            } else {
                tool_choice.clone()
            }
        }
        _ => tool_choice.clone(),
    }
}

fn gemini_tool_config(tool_choice: &Value) -> Value {
    let mode = match tool_choice.as_str() {
        Some("none") => "NONE",
        Some("required") => "ANY",
        _ => "AUTO",
    };

    let mut config = json!({
        "functionCallingConfig": {
            "mode": mode
        }
    });

    if let Some(name) = tool_choice
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
    {
        config["functionCallingConfig"]["mode"] = Value::String("ANY".to_string());
        config["functionCallingConfig"]["allowedFunctionNames"] = json!([name]);
    }

    config
}

fn normalize_stop_sequences(stop: &Value) -> Value {
    match stop {
        Value::String(stop) => json!([stop]),
        Value::Array(_) => stop.clone(),
        _ => Value::Null,
    }
}

fn copy_extra(req: &ChatRequest, body: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = req.extra.get(from) {
        body.insert(to.to_string(), value.clone());
    }
}

fn copy_extra_to_map(req: &ChatRequest, body: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = req.extra.get(from) {
        body.insert(to.to_string(), value.clone());
    }
}

fn u64_extra(req: &ChatRequest, key: &str) -> Option<u64> {
    req.extra.get(key).and_then(Value::as_u64)
}

fn map_anthropic_finish_reason(reason: &str) -> String {
    match reason {
        "end_turn" | "stop_sequence" => "stop",
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        other => other,
    }
    .to_string()
}

fn map_gemini_finish_reason(reason: &str) -> String {
    match reason {
        "STOP" => "stop",
        "MAX_TOKENS" => "length",
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => "content_filter",
        other => other,
    }
    .to_string()
}

fn gemini_usage(usage: &Value) -> ChatUsage {
    let prompt_tokens = usage
        .get("promptTokenCount")
        .and_then(Value::as_u64)
        .map(saturating_u32)
        .unwrap_or_default();
    let completion_tokens = usage
        .get("candidatesTokenCount")
        .or_else(|| usage.get("totalCandidatesTokenCount"))
        .and_then(Value::as_u64)
        .map(saturating_u32)
        .unwrap_or_default();
    let total_tokens = usage
        .get("totalTokenCount")
        .and_then(Value::as_u64)
        .map(saturating_u32)
        .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));
    ChatUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    }
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ChatMessage;

    #[test]
    fn policy_renames_and_drops_provider_parameters() {
        let policy = ParameterPolicy::mistral();
        let mut body = json!({
            "model": "mistral-small-latest",
            "messages": [],
            "max_completion_tokens": 64,
            "parallel_tool_calls": true,
            "presence_penalty": 0.2
        });

        policy.apply(&mut body);

        assert_eq!(body["max_tokens"], 64);
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("parallel_tool_calls").is_none());
        assert!(body.get("presence_penalty").is_none());
    }

    #[test]
    fn openai_stream_transform_includes_usage_by_default() {
        let spec = ProviderSpec::openai_compatible(
            "test",
            "Test",
            "https://example.test/v1/chat/completions",
            vec![Capability::Streaming],
        );
        let req = ChatRequest {
            model: "provider-model".to_string(),
            messages: vec![ChatMessage::text("user", "hello")],
            stream: false,
            stream_options: None,
            temperature: None,
            max_tokens: None,
            extra: serde_json::Map::new(),
        };

        let prepared = spec
            .prepare_chat(
                &req,
                RequestMode::Streaming,
                &ParameterPolicy::openai_compatible(),
                "key",
            )
            .unwrap();

        assert_eq!(prepared.url, "https://example.test/v1/chat/completions");
        assert_eq!(prepared.body["stream"], true);
        assert_eq!(prepared.body["stream_options"]["include_usage"], true);
        assert_eq!(
            prepared
                .headers
                .get(AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer key"
        );
    }

    #[test]
    fn provider_spec_keeps_auth_headers_separate_from_body_transform() {
        let spec = ProviderSpec::openai_compatible(
            "openrouter",
            "OpenRouter",
            "https://openrouter.ai/api/v1/chat/completions",
            vec![Capability::Streaming],
        )
        .with_static_header("http-referer", "https://free-token-energy.local")
        .with_static_header("x-title", "Free Token Energy");
        let req = ChatRequest {
            model: "provider-model".to_string(),
            messages: vec![ChatMessage::text("user", "hello")],
            stream: true,
            stream_options: None,
            temperature: None,
            max_tokens: None,
            extra: serde_json::Map::new(),
        };

        let prepared = spec
            .prepare_chat(
                &req,
                RequestMode::NonStreaming,
                &ParameterPolicy::openai_compatible(),
                "key",
            )
            .unwrap();

        assert_eq!(
            prepared
                .headers
                .get("http-referer")
                .unwrap()
                .to_str()
                .unwrap(),
            "https://free-token-energy.local"
        );
        assert_eq!(
            prepared.headers.get("x-title").unwrap().to_str().unwrap(),
            "Free Token Energy"
        );
        assert_eq!(prepared.body["stream"], false);
        assert!(prepared.body.get("stream_options").is_none());
    }

    #[test]
    fn anthropic_transform_maps_roles_tools_and_system() {
        let spec = ProviderSpec::anthropic(
            "anthropic",
            "Anthropic",
            "https://api.anthropic.com/v1/messages",
            vec![Capability::Streaming, Capability::Tools],
        );
        let mut extra = serde_json::Map::new();
        extra.insert(
            "tools".to_string(),
            json!([{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "Lookup a value",
                    "parameters": { "type": "object", "properties": {} }
                }
            }]),
        );
        extra.insert(
            "anthropic_beta".to_string(),
            Value::String("interleaved-thinking-2025-05-14".to_string()),
        );
        let req = ChatRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            messages: vec![
                ChatMessage::text("system", "system rules"),
                ChatMessage::text("developer", "developer rules"),
                ChatMessage::text("user", "hello"),
            ],
            stream: false,
            stream_options: None,
            temperature: None,
            max_tokens: Some(256),
            extra,
        };

        let prepared = spec
            .prepare_chat(
                &req,
                RequestMode::NonStreaming,
                &ParameterPolicy::anthropic(),
                "key",
            )
            .unwrap();

        assert_eq!(prepared.body["max_tokens"], 256);
        assert_eq!(prepared.body["system"].as_array().unwrap().len(), 2);
        assert_eq!(prepared.body["messages"][0]["role"], "user");
        assert_eq!(prepared.body["tools"][0]["name"], "lookup");
        assert!(prepared.body["anthropic_beta"].is_null());
        assert_eq!(
            prepared
                .headers
                .get("anthropic-version")
                .unwrap()
                .to_str()
                .unwrap(),
            "2023-06-01"
        );
        assert_eq!(
            prepared
                .headers
                .get("anthropic-beta")
                .unwrap()
                .to_str()
                .unwrap(),
            "interleaved-thinking-2025-05-14"
        );
    }

    #[test]
    fn gemini_transform_maps_system_generation_config_and_url() {
        let spec = ProviderSpec::gemini(
            "gemini",
            "Google Gemini",
            "https://generativelanguage.googleapis.com/v1beta",
            vec![Capability::Streaming, Capability::Tools],
        );
        let mut extra = serde_json::Map::new();
        extra.insert(
            "thinking_config".to_string(),
            json!({ "thinkingBudget": 1024 }),
        );
        let req = ChatRequest {
            model: "gemini-2.5-flash".to_string(),
            messages: vec![
                ChatMessage::text("system", "system rules"),
                ChatMessage::text("user", "hello"),
            ],
            stream: true,
            stream_options: None,
            temperature: Some(0.2),
            max_tokens: Some(512),
            extra,
        };

        let prepared = spec
            .prepare_chat(
                &req,
                RequestMode::Streaming,
                &ParameterPolicy::gemini(),
                "key",
            )
            .unwrap();

        assert_eq!(
            prepared.url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            prepared.body["systemInstruction"]["parts"][0]["text"],
            "system rules"
        );
        assert_eq!(prepared.body["contents"][0]["role"], "user");
        assert_eq!(prepared.body["generationConfig"]["maxOutputTokens"], 512);
        assert_eq!(
            prepared.body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            1024
        );
    }
}
