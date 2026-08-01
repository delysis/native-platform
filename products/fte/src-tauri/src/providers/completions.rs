use async_stream::try_stream;
use futures::{stream::BoxStream, StreamExt};
use serde_json::{json, Map, Value};

use crate::providers::streaming::SseParser;
use crate::providers::{
    CompletionChoice, CompletionChunk, CompletionRequest, CompletionResponse, CompletionUsage,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionProtocol {
    OpenAi,
    AnthropicLegacy,
    MistralFim,
}

#[derive(Debug, Clone, Copy)]
pub struct CompletionEndpoint {
    pub url: &'static str,
    pub protocol: CompletionProtocol,
}

impl CompletionEndpoint {
    pub fn new(url: &'static str, protocol: CompletionProtocol) -> Self {
        Self { url, protocol }
    }

    pub fn request_body(
        &self,
        request: &CompletionRequest,
        streaming: bool,
    ) -> anyhow::Result<Value> {
        match self.protocol {
            CompletionProtocol::OpenAi => openai_request_body(request, streaming),
            CompletionProtocol::AnthropicLegacy => anthropic_request_body(request, streaming),
            CompletionProtocol::MistralFim => mistral_fim_request_body(request, streaming),
        }
    }

    pub fn response(&self, body: Value) -> anyhow::Result<CompletionResponse> {
        response_for_protocol(self.protocol, body)
    }
}

pub fn completion_chunks_from_response(
    response: reqwest::Response,
    protocol: CompletionProtocol,
) -> BoxStream<'static, anyhow::Result<CompletionChunk>> {
    let mut bytes = response.bytes_stream();
    let stream = try_stream! {
        let mut parser = SseParser::new();

        while let Some(item) = bytes.next().await {
            let item = item?;
            for event in parser.push_bytes(&item)? {
                if let Some(chunk) = parse_stream_event(protocol, &event)? {
                    yield chunk;
                }
            }
        }

        for event in parser.finish()? {
            if let Some(chunk) = parse_stream_event(protocol, &event)? {
                yield chunk;
            }
        }
    };

    Box::pin(stream)
}

fn openai_request_body(request: &CompletionRequest, streaming: bool) -> anyhow::Result<Value> {
    let mut body = serde_json::to_value(request)?;
    let object = body
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("completion request did not serialize as an object"))?;
    object.insert("stream".to_string(), Value::Bool(streaming));
    if !streaming {
        object.remove("stream_options");
    }
    debug_assert!(!object.contains_key("messages"));
    Ok(body)
}

fn anthropic_request_body(request: &CompletionRequest, streaming: bool) -> anyhow::Result<Value> {
    let prompt = request.prompt.as_text()?;
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(request.model.clone()));
    body.insert("prompt".to_string(), Value::String(prompt.to_string()));
    body.insert(
        "max_tokens_to_sample".to_string(),
        json!(request.max_tokens.unwrap_or(16)),
    );
    body.insert("stream".to_string(), Value::Bool(streaming));

    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    copy_extra(request, &mut body, "top_p", "top_p");
    copy_extra(request, &mut body, "top_k", "top_k");
    if let Some(stop) = request.extra.get("stop") {
        let stop_sequences = match stop {
            Value::String(_) => Value::Array(vec![stop.clone()]),
            Value::Array(_) => stop.clone(),
            Value::Null => Value::Array(Vec::new()),
            _ => {
                return Err(anyhow::anyhow!(
                    "stop must be a string or an array of strings"
                ))
            }
        };
        body.insert("stop_sequences".to_string(), stop_sequences);
    }

    debug_assert!(!body.contains_key("messages"));
    Ok(Value::Object(body))
}

fn mistral_fim_request_body(request: &CompletionRequest, streaming: bool) -> anyhow::Result<Value> {
    let prompt = request.prompt.as_text()?;
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(request.model.clone()));
    body.insert("prompt".to_string(), Value::String(prompt.to_string()));
    body.insert("stream".to_string(), Value::Bool(streaming));

    if let Some(max_tokens) = request.max_tokens {
        body.insert("max_tokens".to_string(), json!(max_tokens));
    }
    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    for parameter in [
        "suffix",
        "top_p",
        "stop",
        "min_tokens",
        "metadata",
        "prompt_cache_key",
    ] {
        copy_extra(request, &mut body, parameter, parameter);
    }
    copy_extra(request, &mut body, "seed", "random_seed");

    debug_assert!(!body.contains_key("messages"));
    Ok(Value::Object(body))
}

fn copy_extra(
    request: &CompletionRequest,
    target: &mut Map<String, Value>,
    source: &str,
    destination: &str,
) {
    if let Some(value) = request.extra.get(source) {
        target.insert(destination.to_string(), value.clone());
    }
}

fn response_for_protocol(
    protocol: CompletionProtocol,
    body: Value,
) -> anyhow::Result<CompletionResponse> {
    match protocol {
        CompletionProtocol::OpenAi => serde_json::from_value(body)
            .map_err(|error| anyhow::anyhow!("invalid completion response: {error}")),
        CompletionProtocol::AnthropicLegacy => anthropic_response(body),
        CompletionProtocol::MistralFim => mistral_fim_response(body),
    }
}

fn anthropic_response(body: Value) -> anyhow::Result<CompletionResponse> {
    let mut object = body
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("anthropic completion response was not an object"))?;
    let id = take_string(&mut object, "id").unwrap_or_else(|| "compl-anthropic".to_string());
    let model = take_string(&mut object, "model");
    let text = take_string(&mut object, "completion").unwrap_or_default();
    let finish_reason =
        take_string(&mut object, "stop_reason").map(|reason| match reason.as_str() {
            "stop_sequence" => "stop".to_string(),
            "max_tokens" => "length".to_string(),
            _ => reason,
        });
    object.remove("type");

    Ok(CompletionResponse {
        id,
        object: Some("text_completion".to_string()),
        created: None,
        model,
        choices: vec![CompletionChoice {
            text,
            index: 0,
            logprobs: None,
            finish_reason,
            extra: Map::new(),
        }],
        usage: None,
        extra: object,
    })
}

fn mistral_fim_response(body: Value) -> anyhow::Result<CompletionResponse> {
    let mut object = body
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Mistral FIM response was not an object"))?;
    let id = take_string(&mut object, "id").unwrap_or_else(|| "cmpl-mistral".to_string());
    let model = take_string(&mut object, "model");
    let created = take_u64(&mut object, "created");
    object.remove("object");
    let choices = object
        .remove("choices")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .map(mistral_choice)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let usage = object.remove("usage").map(completion_usage).transpose()?;

    Ok(CompletionResponse {
        id,
        object: Some("text_completion".to_string()),
        created,
        model,
        choices,
        usage,
        extra: object,
    })
}

fn mistral_choice(choice: Value) -> anyhow::Result<CompletionChoice> {
    let mut object = choice
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Mistral FIM choice was not an object"))?;
    let index = object
        .remove("index")
        .and_then(|value| value.as_u64())
        .map(saturating_u32)
        .unwrap_or_default();
    let finish_reason = take_string(&mut object, "finish_reason");
    let logprobs = object.remove("logprobs").filter(|value| !value.is_null());
    let text = object
        .remove("text")
        .and_then(|value| value.as_str().map(ToString::to_string))
        .or_else(|| {
            object
                .remove("message")
                .and_then(|message| message.get("content").cloned())
                .and_then(|content| content.as_str().map(ToString::to_string))
        })
        .or_else(|| {
            object
                .remove("delta")
                .and_then(|delta| delta.get("content").cloned())
                .and_then(|content| content.as_str().map(ToString::to_string))
        })
        .unwrap_or_default();

    Ok(CompletionChoice {
        text,
        index,
        logprobs,
        finish_reason,
        extra: object,
    })
}

fn completion_usage(value: Value) -> anyhow::Result<CompletionUsage> {
    serde_json::from_value(value)
        .map_err(|error| anyhow::anyhow!("invalid completion usage: {error}"))
}

fn parse_stream_event(
    protocol: CompletionProtocol,
    event: &str,
) -> anyhow::Result<Option<CompletionChunk>> {
    if event == "[DONE]" {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(event).map_err(|error| {
        anyhow::anyhow!("failed to parse completion stream event: {error}; payload={event}")
    })?;
    if value.get("type").and_then(Value::as_str) == Some("error") || value.get("error").is_some() {
        return Err(anyhow::anyhow!("completion stream error: {value}"));
    }
    response_for_protocol(protocol, value).map(Some)
}

fn take_string(object: &mut Map<String, Value>, key: &str) -> Option<String> {
    object
        .remove(key)
        .and_then(|value| value.as_str().map(ToString::to_string))
}

fn take_u64(object: &mut Map<String, Value>, key: &str) -> Option<u64> {
    object.remove(key).and_then(|value| value.as_u64())
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{CompletionPrompt, StreamOptions};

    fn request(prompt: CompletionPrompt) -> CompletionRequest {
        CompletionRequest {
            model: "test-model".to_string(),
            prompt,
            stream: false,
            stream_options: None,
            temperature: Some(0.2),
            max_tokens: Some(32),
            extra: Map::new(),
        }
    }

    #[test]
    fn openai_completion_fixture_preserves_token_batches_without_messages() {
        let endpoint = CompletionEndpoint::new("unused", CompletionProtocol::OpenAi);
        let mut request = request(CompletionPrompt::TokenBatches(vec![vec![1, 2], vec![3, 4]]));
        request.stream_options = Some(StreamOptions {
            include_usage: Some(true),
        });
        let body = endpoint.request_body(&request, true).unwrap();

        assert_eq!(body["prompt"], json!([[1, 2], [3, 4]]));
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert!(body.get("messages").is_none());
    }

    #[test]
    fn anthropic_completion_fixture_uses_legacy_prompt_protocol_fields() {
        let endpoint = CompletionEndpoint::new("unused", CompletionProtocol::AnthropicLegacy);
        let mut request = request(CompletionPrompt::Text(
            "\n\nHuman: Continue this\n\nAssistant:".to_string(),
        ));
        request.extra.insert("stop".to_string(), json!(["END"]));
        let body = endpoint.request_body(&request, false).unwrap();

        assert_eq!(body["prompt"], "\n\nHuman: Continue this\n\nAssistant:");
        assert_eq!(body["max_tokens_to_sample"], 32);
        assert_eq!(body["stop_sequences"], json!(["END"]));
        assert!(body.get("messages").is_none());
    }

    #[test]
    fn anthropic_completion_rejects_multi_prompt_input() {
        let endpoint = CompletionEndpoint::new("unused", CompletionProtocol::AnthropicLegacy);
        let request = request(CompletionPrompt::Texts(vec![
            "a".to_string(),
            "b".to_string(),
        ]));

        assert!(endpoint.request_body(&request, false).is_err());
    }

    #[test]
    fn mistral_fim_fixture_maps_seed_and_normalizes_chat_shaped_response() {
        let endpoint = CompletionEndpoint::new("unused", CompletionProtocol::MistralFim);
        let mut request = request(CompletionPrompt::Text("fn add".to_string()));
        request.extra.insert("seed".to_string(), json!(42));
        let body = endpoint.request_body(&request, false).unwrap();
        assert_eq!(body["random_seed"], 42);
        assert!(body.get("messages").is_none());

        let response = endpoint
            .response(json!({
                "id": "fim-1",
                "object": "chat.completion",
                "created": 10,
                "model": "codestral-latest",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "(a, b): return a + b"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}
            }))
            .unwrap();

        assert_eq!(response.object.as_deref(), Some("text_completion"));
        assert_eq!(response.choices[0].text, "(a, b): return a + b");
        assert_eq!(response.usage.unwrap().total_tokens, 5);
    }

    #[test]
    fn openai_completion_fixture_keeps_all_choices_and_logprobs() {
        let endpoint = CompletionEndpoint::new("unused", CompletionProtocol::OpenAi);
        let response = endpoint
            .response(json!({
                "id": "cmpl-1",
                "object": "text_completion",
                "created": 10,
                "model": "gpt-oss-120b",
                "choices": [
                    {"text": "a", "index": 0, "logprobs": {"tokens": ["a"]}, "finish_reason": "stop"},
                    {"text": "b", "index": 1, "logprobs": null, "finish_reason": "length"}
                ],
                "usage": {"prompt_tokens": 2, "completion_tokens": 2, "total_tokens": 4}
            }))
            .unwrap();

        assert_eq!(response.choices.len(), 2);
        assert_eq!(
            response.choices[0].logprobs.as_ref().unwrap()["tokens"],
            json!(["a"])
        );
    }

    #[test]
    fn openai_completion_fixture_accepts_null_prompt_and_raw_token_output() {
        let request: CompletionRequest = serde_json::from_value(json!({
            "model": "gpt-oss-120b",
            "prompt": null,
            "return_raw_tokens": true
        }))
        .unwrap();
        assert_eq!(request.prompt, CompletionPrompt::Text(String::new()));

        let response = CompletionEndpoint::new("unused", CompletionProtocol::OpenAi)
            .response(json!({
                "id": "cmpl-raw",
                "object": "text_completion",
                "created": 10,
                "model": "gpt-oss-120b",
                "choices": [{
                    "text": null,
                    "tokens": [123, 456],
                    "index": 0,
                    "logprobs": null,
                    "finish_reason": "stop"
                }]
            }))
            .unwrap();

        assert_eq!(response.choices[0].text, "");
        assert_eq!(response.choices[0].extra["tokens"], json!([123, 456]));
    }

    #[test]
    fn openai_completion_stream_fixture_keeps_choice_indexes() {
        let chunk = parse_stream_event(
            CompletionProtocol::OpenAi,
            r#"{"id":"cmpl-stream","object":"text_completion","created":1,"model":"test","choices":[{"text":"a","index":0,"logprobs":null,"finish_reason":null},{"text":"b","index":1,"logprobs":null,"finish_reason":"stop"}]}"#,
        )
        .unwrap()
        .unwrap();

        assert_eq!(chunk.choices.len(), 2);
        assert_eq!(chunk.choices[1].index, 1);
        assert_eq!(chunk.choices[1].text, "b");
    }

    #[test]
    fn anthropic_completion_stream_fixture_maps_completion_delta() {
        let chunk = parse_stream_event(
            CompletionProtocol::AnthropicLegacy,
            r#"{"id":"compl_1","completion":" continued","model":"claude-test","stop_reason":null,"type":"completion"}"#,
        )
        .unwrap()
        .unwrap();

        assert_eq!(chunk.choices[0].text, " continued");
        assert_eq!(chunk.object.as_deref(), Some("text_completion"));
    }

    #[test]
    fn mistral_fim_stream_fixture_maps_delta_without_chat_wrapper() {
        let chunk = parse_stream_event(
            CompletionProtocol::MistralFim,
            r#"{"id":"fim-1","object":"chat.completion.chunk","created":1,"model":"codestral","choices":[{"index":0,"delta":{"content":" body"},"finish_reason":null}]}"#,
        )
        .unwrap()
        .unwrap();

        assert_eq!(chunk.choices[0].text, " body");
        assert_eq!(chunk.object.as_deref(), Some("text_completion"));
    }

    #[test]
    fn completion_stream_fixture_propagates_error_events() {
        let error = parse_stream_event(
            CompletionProtocol::OpenAi,
            r#"{"error":{"message":"upstream failed"}}"#,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("upstream failed"));
    }
}
