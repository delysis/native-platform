use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub mod anthropic;
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

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn capabilities(&self) -> &[Capability];
    async fn chat(
        &self,
        req: &ChatRequest,
        api_key: &str,
        policy: &spec::ParameterPolicy,
    ) -> anyhow::Result<ChatResponse>;
    async fn chat_stream(
        &self,
        req: &ChatRequest,
        api_key: &str,
        policy: &spec::ParameterPolicy,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<ChatChunk>>>;
}
