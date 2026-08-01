use async_stream::try_stream;
use futures::{stream::BoxStream, StreamExt};
use serde_json::{json, Map, Value};

use crate::providers::{ChatChunk, ChatChunkChoice, ChatDelta, ChatUsage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamParserKind {
    OpenAiSse,
    AnthropicSse,
    GeminiSse,
}

pub fn chat_chunks_from_response(
    response: reqwest::Response,
    parser_kind: StreamParserKind,
) -> BoxStream<'static, anyhow::Result<ChatChunk>> {
    let mut byte_stream = response.bytes_stream();
    let stream = try_stream! {
        let mut parser = parser_kind.parser();

        while let Some(item) = byte_stream.next().await {
            let bytes = item?;
            for event_data in parser.push_bytes(&bytes)? {
                let Some(chunk) = parser.parse_chat_event(&event_data)? else {
                    continue;
                };
                yield chunk;
            }
        }

        for event_data in parser.finish()? {
            let Some(chunk) = parser.parse_chat_event(&event_data)? else {
                continue;
            };
            yield chunk;
        }
    };

    Box::pin(stream)
}

impl StreamParserKind {
    fn parser(self) -> StreamParser {
        match self {
            StreamParserKind::OpenAiSse => StreamParser::OpenAi(SseParser::new()),
            StreamParserKind::AnthropicSse => StreamParser::Anthropic {
                sse: SseParser::new(),
                input_tokens: 0,
            },
            StreamParserKind::GeminiSse => StreamParser::Gemini(SseParser::new()),
        }
    }
}

enum StreamParser {
    OpenAi(SseParser),
    Anthropic { sse: SseParser, input_tokens: u32 },
    Gemini(SseParser),
}

impl StreamParser {
    fn push_bytes(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<String>> {
        match self {
            StreamParser::OpenAi(parser) => parser.push_bytes(bytes),
            StreamParser::Anthropic { sse, .. } => sse.push_bytes(bytes),
            StreamParser::Gemini(parser) => parser.push_bytes(bytes),
        }
    }

    fn finish(&mut self) -> anyhow::Result<Vec<String>> {
        match self {
            StreamParser::OpenAi(parser) => parser.finish(),
            StreamParser::Anthropic { sse, .. } => sse.finish(),
            StreamParser::Gemini(parser) => parser.finish(),
        }
    }

    fn parse_chat_event(&mut self, event_data: &str) -> anyhow::Result<Option<ChatChunk>> {
        match self {
            StreamParser::OpenAi(_) => parse_openai_sse_chat_event(event_data),
            StreamParser::Anthropic { input_tokens, .. } => {
                parse_anthropic_sse_chat_event(event_data, input_tokens)
            }
            StreamParser::Gemini(_) => parse_gemini_sse_chat_event(event_data),
        }
    }
}

fn parse_openai_sse_chat_event(event_data: &str) -> anyhow::Result<Option<ChatChunk>> {
    if event_data == "[DONE]" {
        return Ok(None);
    }

    Ok(Some(
        serde_json::from_str::<ChatChunk>(event_data).map_err(|error| {
            anyhow::anyhow!("failed to parse chat stream chunk: {error}; payload={event_data}")
        })?,
    ))
}

fn parse_anthropic_sse_chat_event(
    event_data: &str,
    input_tokens: &mut u32,
) -> anyhow::Result<Option<ChatChunk>> {
    let event: Value = serde_json::from_str(event_data).map_err(|error| {
        anyhow::anyhow!("failed to parse anthropic stream event: {error}; payload={event_data}")
    })?;

    match event.get("type").and_then(Value::as_str) {
        Some("message_start") => {
            let usage = event
                .get("message")
                .and_then(|message| message.get("usage"));
            *input_tokens = usage.map(anthropic_input_tokens).unwrap_or_default();
            Ok(usage.map(|_| {
                chat_chunk_with_delta(
                    None,
                    None,
                    None,
                    Some(ChatUsage {
                        prompt_tokens: *input_tokens,
                        completion_tokens: 0,
                        total_tokens: *input_tokens,
                    }),
                )
            }))
        }
        Some("content_block_start") => {
            let Some(block) = event.get("content_block") else {
                return Ok(None);
            };
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                let index = event
                    .get("index")
                    .and_then(Value::as_u64)
                    .map(saturating_u32)
                    .unwrap_or(0);
                Ok(Some(chat_chunk_with_delta(
                    None,
                    Some(extra_with_tool_call(
                        index,
                        block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("tool_use"),
                        block.get("name").and_then(Value::as_str).unwrap_or("tool"),
                        "",
                    )),
                    None,
                    None,
                )))
            } else {
                Ok(None)
            }
        }
        Some("content_block_delta") => {
            let Some(delta) = event.get("delta") else {
                return Ok(None);
            };
            let index = event
                .get("index")
                .and_then(Value::as_u64)
                .map(saturating_u32)
                .unwrap_or(0);
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => Ok(delta
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| chat_chunk_with_delta(Some(text.to_string()), None, None, None))),
                Some("thinking_delta") => {
                    Ok(delta
                        .get("thinking")
                        .and_then(Value::as_str)
                        .map(|thinking| {
                            let mut extra = Map::new();
                            extra.insert(
                                "reasoning_content".to_string(),
                                Value::String(thinking.to_string()),
                            );
                            chat_chunk_with_delta(None, Some(extra), None, None)
                        }))
                }
                Some("input_json_delta") => Ok(delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .map(|partial| {
                        chat_chunk_with_delta(
                            None,
                            Some(extra_with_tool_arguments(index, partial)),
                            None,
                            None,
                        )
                    })),
                Some("signature_delta") => {
                    Ok(delta
                        .get("signature")
                        .and_then(Value::as_str)
                        .map(|signature| {
                            let mut extra = Map::new();
                            extra.insert(
                                "thinking_signature".to_string(),
                                Value::String(signature.to_string()),
                            );
                            chat_chunk_with_delta(None, Some(extra), None, None)
                        }))
                }
                _ => Ok(None),
            }
        }
        Some("message_delta") => {
            let finish_reason = event
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
                .map(map_anthropic_finish_reason);
            let usage = event
                .get("usage")
                .map(|usage| anthropic_usage(usage, *input_tokens));
            Ok(Some(chat_chunk_with_delta(
                None,
                None,
                finish_reason,
                usage,
            )))
        }
        Some("message_stop") => Ok(None),
        Some("ping") => Ok(None),
        Some("error") => Err(anyhow::anyhow!(
            "anthropic stream error: {}",
            event.get("error").cloned().unwrap_or(event)
        )),
        _ => Ok(None),
    }
}

fn parse_gemini_sse_chat_event(event_data: &str) -> anyhow::Result<Option<ChatChunk>> {
    let event: Value = serde_json::from_str(event_data).map_err(|error| {
        anyhow::anyhow!("failed to parse gemini stream event: {error}; payload={event_data}")
    })?;

    let candidate = event
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first());

    let mut content = String::new();
    let mut extra = Map::new();

    if let Some(parts) = candidate
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
    {
        let mut tool_calls = Vec::new();
        let mut reasoning = Vec::new();
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if part
                    .get("thought")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    reasoning.push(text.to_string());
                } else {
                    content.push_str(text);
                }
            }
            if let Some(function_call) = part.get("functionCall") {
                tool_calls.push(gemini_function_call_delta(function_call, tool_calls.len()));
            }
            if let Some(signature) = part
                .get("thoughtSignature")
                .or_else(|| part.get("thought_signature"))
            {
                extra.insert("thought_signature".to_string(), signature.clone());
            }
        }
        if !tool_calls.is_empty() {
            extra.insert("tool_calls".to_string(), Value::Array(tool_calls));
        }
        if !reasoning.is_empty() {
            extra.insert(
                "reasoning_content".to_string(),
                Value::String(reasoning.join("")),
            );
        }
    }

    let finish_reason = candidate
        .and_then(|candidate| candidate.get("finishReason"))
        .and_then(Value::as_str)
        .map(map_gemini_finish_reason);
    let usage = event.get("usageMetadata").map(gemini_usage);

    if content.is_empty() && extra.is_empty() && finish_reason.is_none() && usage.is_none() {
        return Ok(None);
    }

    Ok(Some(chat_chunk_with_delta(
        if content.is_empty() {
            None
        } else {
            Some(content)
        },
        if extra.is_empty() { None } else { Some(extra) },
        finish_reason,
        usage,
    )))
}

fn chat_chunk_with_delta(
    content: Option<String>,
    extra: Option<Map<String, Value>>,
    finish_reason: Option<String>,
    usage: Option<ChatUsage>,
) -> ChatChunk {
    ChatChunk {
        id: "chatcmpl-stream".to_string(),
        object: None,
        created: None,
        model: None,
        choices: vec![ChatChunkChoice {
            index: 0,
            delta: ChatDelta {
                role: None,
                content,
                extra: extra.unwrap_or_default(),
            },
            finish_reason,
        }],
        usage,
    }
}

fn extra_with_tool_call(index: u32, id: &str, name: &str, arguments: &str) -> Map<String, Value> {
    let mut extra = Map::new();
    extra.insert(
        "tool_calls".to_string(),
        json!([{
            "index": index,
            "id": id,
            "type": "function",
            "function": {
                "name": name,
                "arguments": arguments
            }
        }]),
    );
    extra
}

fn extra_with_tool_arguments(index: u32, partial_json: &str) -> Map<String, Value> {
    let mut extra = Map::new();
    extra.insert(
        "tool_calls".to_string(),
        json!([{
            "index": index,
            "function": {
                "arguments": partial_json
            }
        }]),
    );
    extra
}

fn gemini_function_call_delta(function_call: &Value, index: usize) -> Value {
    json!({
        "index": index,
        "id": format!("call_{index}"),
        "type": "function",
        "function": {
            "name": function_call
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("function"),
            "arguments": function_call
                .get("args")
                .cloned()
                .unwrap_or_else(|| json!({}))
                .to_string()
        }
    })
}

fn anthropic_usage(usage: &Value, input_tokens: u32) -> ChatUsage {
    let completion_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .map(saturating_u32)
        .unwrap_or_default();
    ChatUsage {
        prompt_tokens: input_tokens,
        completion_tokens,
        total_tokens: input_tokens.saturating_add(completion_tokens),
    }
}

fn anthropic_input_tokens(usage: &Value) -> u32 {
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .filter_map(|key| usage.get(key).and_then(Value::as_u64))
    .map(saturating_u32)
    .fold(0, u32::saturating_add)
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

fn map_anthropic_finish_reason(reason: &str) -> String {
    match reason {
        "end_turn" | "stop_sequence" => "stop",
        "max_tokens" | "model_context_window_exceeded" => "length",
        "tool_use" => "tool_calls",
        "pause_turn" => "pause",
        "refusal" => "content_filter",
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

#[derive(Default)]
pub(crate) struct SseParser {
    buffered_line: Vec<u8>,
    event_data_lines: Vec<String>,
}

impl SseParser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push_bytes(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<String>> {
        self.buffered_line.extend_from_slice(bytes);
        self.drain_complete_lines()
    }

    pub(crate) fn finish(&mut self) -> anyhow::Result<Vec<String>> {
        if !self.buffered_line.is_empty() {
            let mut line = std::mem::take(&mut self.buffered_line);
            if line.ends_with(b"\r") {
                line.pop();
            }
            let line = String::from_utf8(line)?;
            self.handle_line(&line)?;
        }

        self.flush_event()
    }

    fn drain_complete_lines(&mut self) -> anyhow::Result<Vec<String>> {
        let mut events = Vec::new();

        while let Some(newline_index) = self.buffered_line.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.buffered_line.drain(..=newline_index).collect();
            if line.ends_with(b"\n") {
                line.pop();
            }
            if line.ends_with(b"\r") {
                line.pop();
            }

            let line = String::from_utf8(line)?;
            let new_events = self.handle_line(&line)?;
            events.extend(new_events);
        }

        Ok(events)
    }

    fn handle_line(&mut self, line: &str) -> anyhow::Result<Vec<String>> {
        if line.is_empty() {
            return self.flush_event();
        }

        if line.starts_with(':') {
            return Ok(Vec::new());
        }

        if let Some(data) = line.strip_prefix("data:") {
            self.event_data_lines.push(data.trim_start().to_string());
        }

        Ok(Vec::new())
    }

    fn flush_event(&mut self) -> anyhow::Result<Vec<String>> {
        if self.event_data_lines.is_empty() {
            return Ok(Vec::new());
        }

        let data = self.event_data_lines.join("\n");
        self.event_data_lines.clear();
        Ok(vec![data])
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_anthropic_sse_chat_event, parse_gemini_sse_chat_event, SseParser};

    #[test]
    fn parses_split_sse_data_events() {
        let mut parser = SseParser::new();
        let mut events = parser.push_bytes(b"data: {\"id\":\"1\"}\n\ndata:").unwrap();
        events.extend(parser.push_bytes(b" [DONE]\n\n").unwrap());

        assert_eq!(events, vec!["{\"id\":\"1\"}", "[DONE]"]);
    }

    #[test]
    fn joins_multiline_data_events() {
        let mut parser = SseParser::new();
        let events = parser.push_bytes(b"data: {\"a\":\ndata: 1}\n\n").unwrap();

        assert_eq!(events, vec!["{\"a\":\n1}"]);
    }

    #[test]
    fn handles_utf8_split_across_byte_chunks() {
        let mut parser = SseParser::new();
        let text = "data: {\"content\":\"ok 👍\"}\n\n".as_bytes();
        let mut events = parser.push_bytes(&text[..22]).unwrap();
        events.extend(parser.push_bytes(&text[22..]).unwrap());

        assert_eq!(events, vec!["{\"content\":\"ok 👍\"}"]);
    }

    #[test]
    fn anthropic_text_delta_maps_to_chat_chunk() {
        let mut input_tokens = 0;
        let chunk = parse_anthropic_sse_chat_event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
            &mut input_tokens,
        )
        .unwrap()
        .unwrap();

        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hi"));
    }

    #[test]
    fn anthropic_stream_usage_carries_input_tokens_to_final_total() {
        let mut input_tokens = 0;
        let start = parse_anthropic_sse_chat_event(
            r#"{"type":"message_start","message":{"usage":{"input_tokens":7,"cache_read_input_tokens":3}}}"#,
            &mut input_tokens,
        )
        .unwrap()
        .unwrap();
        let end = parse_anthropic_sse_chat_event(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            &mut input_tokens,
        )
        .unwrap()
        .unwrap();

        assert_eq!(start.usage.unwrap().prompt_tokens, 10);
        let usage = end.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn gemini_text_delta_maps_to_chat_chunk() {
        let chunk = parse_gemini_sse_chat_event(
            r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":2,"candidatesTokenCount":3,"totalTokenCount":5}}"#,
        )
        .unwrap()
        .unwrap();

        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hi"));
        assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(chunk.usage.unwrap().total_tokens, 5);
    }
}
