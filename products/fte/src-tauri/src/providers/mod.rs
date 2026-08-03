use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

pub mod anthropic;
pub mod completions;
pub mod gemini;
pub mod groq;
pub mod openai_compatible;
pub mod openrouter;
pub mod spec;
pub mod streaming;
// pub mod mistral;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: Value,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl ChatMessage {
    pub fn text(role: &str, content: &str) -> Self {
        Self {
            role: role.to_string(),
            content: Value::String(content.to_string()),
            extra: Map::new(),
        }
    }

    pub fn content_text(&self) -> String {
        text_from_value(&self.content)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StreamOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_usage: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum CompletionPrompt {
    Text(String),
    Texts(Vec<String>),
    Tokens(Vec<u32>),
    TokenBatches(Vec<Vec<u32>>),
}

impl Default for CompletionPrompt {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl CompletionPrompt {
    pub fn kind(&self) -> CompletionPromptKind {
        match self {
            Self::Text(_) => CompletionPromptKind::Text,
            Self::Texts(_) => CompletionPromptKind::Texts,
            Self::Tokens(_) => CompletionPromptKind::Tokens,
            Self::TokenBatches(_) => CompletionPromptKind::TokenBatches,
        }
    }

    pub fn as_text(&self) -> anyhow::Result<&str> {
        match self {
            Self::Text(prompt) => Ok(prompt),
            _ => Err(anyhow::anyhow!(
                "this provider requires prompt to be a single string"
            )),
        }
    }

    pub fn item_count(&self) -> usize {
        match self {
            Self::Text(_) | Self::Tokens(_) => 1,
            Self::Texts(prompts) => prompts.len(),
            Self::TokenBatches(prompts) => prompts.len(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionPromptKind {
    Text,
    Texts,
    Tokens,
    TokenBatches,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    #[serde(default, deserialize_with = "deserialize_completion_prompt")]
    pub prompt: CompletionPrompt,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl CompletionRequest {
    pub fn requested_parameters(&self) -> Vec<&str> {
        let mut parameters = Vec::new();
        if self.stream {
            parameters.push("stream");
        }
        if self.stream_options.is_some() {
            parameters.push("stream_options");
        }
        if self.temperature.is_some() {
            parameters.push("temperature");
        }
        if self.max_tokens.is_some() {
            parameters.push("max_tokens");
        }
        parameters.extend(
            self.extra
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, _)| key.as_str()),
        );
        parameters
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<CompletionChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<CompletionUsage>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl CompletionResponse {
    pub fn total_tokens(&self) -> Option<u32> {
        self.usage.as_ref().map(|usage| usage.total_tokens)
    }

    pub fn normalize(&mut self, model: &str, created: u64) {
        self.model = Some(model.to_string());
        self.object = Some("text_completion".to_string());
        if self.created.is_none() {
            self.created = Some(created);
        }
    }
}

pub type CompletionChunk = CompletionResponse;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionChoice {
    #[serde(default, deserialize_with = "deserialize_completion_text")]
    pub text: String,
    #[serde(default)]
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn deserialize_completion_prompt<'de, D>(deserializer: D) -> Result<CompletionPrompt, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<CompletionPrompt>::deserialize(deserializer)?.unwrap_or_default())
}

fn deserialize_completion_text<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<ChatUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChunk {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub choices: Vec<ChatChunkChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ChatUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChunkChoice {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub delta: ChatDelta,
    pub finish_reason: Option<String>,
}

impl Default for ChatDelta {
    fn default() -> Self {
        Self {
            role: None,
            content: None,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatStreamFrame {
    pub chunk: Option<ChatChunk>,
    pub done: bool,
}

impl ChatChunk {
    pub fn total_tokens(&self) -> Option<u32> {
        self.usage.as_ref().map(|usage| usage.total_tokens)
    }

    pub fn normalize(&mut self, model: &str, created: u64) {
        self.model = Some(model.to_string());
        if self.object.is_none() {
            self.object = Some("chat.completion.chunk".to_string());
        }
        if self.created.is_none() {
            self.created = Some(created);
        }
    }
}

pub fn text_from_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if let Some(text) = part.as_str() {
                    return Some(text.to_string());
                }
                let object = part.as_object()?;
                if object.get("type").and_then(Value::as_str) == Some("text") {
                    return object
                        .get("text")
                        .and_then(Value::as_str)
                        .map(ToString::to_string);
                }
                None
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyChatChunk {
    pub id: String,
    pub delta: ChatDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Capability {
    Vision,
    Tools,
    LongContext,
    Streaming,
}

impl Capability {
    pub const ALL: [Self; 4] = [
        Self::Vision,
        Self::Tools,
        Self::LongContext,
        Self::Streaming,
    ];
}
