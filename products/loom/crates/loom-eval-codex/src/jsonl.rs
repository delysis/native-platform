use std::collections::{BTreeMap, BTreeSet};

use loom_types::BlobId;
use serde_json::Value;
use thiserror::Error;

use crate::{FRONTIER_MODEL, FRONTIER_REASONING_EFFORT};

pub const MAX_CODEX_JSONL_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_CODEX_JSONL_EVENTS: usize = 100_000;

/// Closed, tool-free facts extracted from one complete `codex exec --json`
/// stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedCodexJsonl {
    thread_id: String,
    final_agent_message: String,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    jsonl_fingerprint: BlobId,
}

impl CheckedCodexJsonl {
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn final_agent_message(&self) -> &str {
        &self.final_agent_message
    }

    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    pub const fn cached_input_tokens(&self) -> u64 {
        self.cached_input_tokens
    }

    pub const fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    pub const fn jsonl_fingerprint(&self) -> BlobId {
        self.jsonl_fingerprint
    }
}

/// Validate the complete documented noninteractive JSONL lifecycle.
///
/// Only reasoning and agent-message items are accepted. Commands, file
/// changes, MCP calls, searches, images, errors, unknown event kinds, duplicate
/// lifecycle events, and events after `turn.completed` all fail closed.
pub fn check_tool_free_codex_jsonl(bytes: &[u8]) -> Result<CheckedCodexJsonl, CodexJsonlError> {
    if bytes.is_empty() || bytes.len() > MAX_CODEX_JSONL_BYTES {
        return Err(CodexJsonlError::InvalidByteLength(bytes.len()));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| CodexJsonlError::InvalidUtf8)?;
    let mut state = ParseState::default();
    for line in text.lines() {
        if line.is_empty() {
            return Err(CodexJsonlError::EmptyLine);
        }
        state.event_count = state
            .event_count
            .checked_add(1)
            .ok_or(CodexJsonlError::TooManyEvents)?;
        if state.event_count > MAX_CODEX_JSONL_EVENTS {
            return Err(CodexJsonlError::TooManyEvents);
        }
        if state.completed {
            return Err(CodexJsonlError::EventAfterCompletion);
        }
        let event: Value = serde_json::from_str(line).map_err(CodexJsonlError::MalformedJson)?;
        let object = event.as_object().ok_or(CodexJsonlError::EventNotObject)?;
        validate_execution_metadata(object)?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or(CodexJsonlError::MissingEventType)?;
        match kind {
            "thread.started" => state.thread_started(object)?,
            "turn.started" => state.turn_started(object)?,
            "item.started" | "item.updated" | "item.completed" => {
                state.item_event(kind, object)?;
            }
            "turn.completed" => state.turn_completed(object)?,
            "error" | "turn.failed" => return Err(CodexJsonlError::FailureEvent),
            _ => return Err(CodexJsonlError::UnknownEventType),
        }
    }
    state.finish(bytes)
}

#[derive(Default)]
struct ParseState {
    event_count: usize,
    thread_id: Option<String>,
    turn_started: bool,
    completed: bool,
    final_agent_message: Option<String>,
    usage: Option<(u64, u64, u64)>,
    active_items: BTreeMap<String, &'static str>,
    completed_item_ids: BTreeSet<String>,
}

impl ParseState {
    fn thread_started(
        &mut self,
        event: &serde_json::Map<String, Value>,
    ) -> Result<(), CodexJsonlError> {
        if self.thread_id.is_some() || self.turn_started {
            return Err(CodexJsonlError::LifecycleOrder);
        }
        let id = event
            .get("thread_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or(CodexJsonlError::InvalidThreadId)?;
        self.thread_id = Some(id.to_owned());
        Ok(())
    }

    fn turn_started(
        &mut self,
        _event: &serde_json::Map<String, Value>,
    ) -> Result<(), CodexJsonlError> {
        if self.thread_id.is_none() || self.turn_started {
            return Err(CodexJsonlError::LifecycleOrder);
        }
        self.turn_started = true;
        Ok(())
    }

    fn item_event(
        &mut self,
        event_kind: &str,
        event: &serde_json::Map<String, Value>,
    ) -> Result<(), CodexJsonlError> {
        if !self.turn_started {
            return Err(CodexJsonlError::LifecycleOrder);
        }
        let item = event
            .get("item")
            .and_then(Value::as_object)
            .ok_or(CodexJsonlError::MissingItem)?;
        let item_kind = item
            .get("type")
            .and_then(Value::as_str)
            .ok_or(CodexJsonlError::MissingItemType)?;
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or(CodexJsonlError::InvalidItemId)?;
        if self.completed_item_ids.contains(item_id) {
            return Err(CodexJsonlError::DuplicateItemLifecycle);
        }
        let canonical_kind = match item_kind {
            "reasoning" => "reasoning",
            "agent_message" => "agent_message",
            _ => return Err(CodexJsonlError::ToolOrUnknownItem),
        };
        match event_kind {
            "item.started" => {
                if self
                    .active_items
                    .insert(item_id.to_owned(), canonical_kind)
                    .is_some()
                {
                    return Err(CodexJsonlError::DuplicateItemLifecycle);
                }
            }
            "item.updated" => {
                if self.active_items.get(item_id).copied() != Some(canonical_kind) {
                    return Err(CodexJsonlError::LifecycleOrder);
                }
            }
            "item.completed" => {
                if let Some(started_kind) = self.active_items.remove(item_id)
                    && started_kind != canonical_kind
                {
                    return Err(CodexJsonlError::LifecycleOrder);
                }
                self.completed_item_ids.insert(item_id.to_owned());
            }
            _ => return Err(CodexJsonlError::UnknownEventType),
        }
        match item_kind {
            "reasoning" => Ok(()),
            "agent_message" => {
                if event_kind != "item.completed" {
                    return Ok(());
                }
                if self.final_agent_message.is_some() {
                    return Err(CodexJsonlError::DuplicateAgentMessage);
                }
                let text = item
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .ok_or(CodexJsonlError::InvalidAgentMessage)?;
                self.final_agent_message = Some(text.to_owned());
                Ok(())
            }
            _ => unreachable!("item kind was checked above"),
        }
    }

    fn turn_completed(
        &mut self,
        event: &serde_json::Map<String, Value>,
    ) -> Result<(), CodexJsonlError> {
        if !self.turn_started
            || self.completed
            || self.final_agent_message.is_none()
            || !self.active_items.is_empty()
        {
            return Err(CodexJsonlError::LifecycleOrder);
        }
        let usage = event
            .get("usage")
            .and_then(Value::as_object)
            .ok_or(CodexJsonlError::MissingUsage)?;
        let input = exact_u64(usage, "input_tokens")?;
        let cached = exact_u64(usage, "cached_input_tokens")?;
        let output = exact_u64(usage, "output_tokens")?;
        if cached > input {
            return Err(CodexJsonlError::InvalidUsage);
        }
        self.usage = Some((input, cached, output));
        self.completed = true;
        Ok(())
    }

    fn finish(self, bytes: &[u8]) -> Result<CheckedCodexJsonl, CodexJsonlError> {
        if !self.completed {
            return Err(CodexJsonlError::IncompleteLifecycle);
        }
        let (input_tokens, cached_input_tokens, output_tokens) =
            self.usage.ok_or(CodexJsonlError::IncompleteLifecycle)?;
        Ok(CheckedCodexJsonl {
            thread_id: self.thread_id.ok_or(CodexJsonlError::IncompleteLifecycle)?,
            final_agent_message: self
                .final_agent_message
                .ok_or(CodexJsonlError::IncompleteLifecycle)?,
            input_tokens,
            cached_input_tokens,
            output_tokens,
            jsonl_fingerprint: BlobId::digest(bytes),
        })
    }
}

fn validate_execution_metadata(
    event: &serde_json::Map<String, Value>,
) -> Result<(), CodexJsonlError> {
    for (key, expected) in [
        ("model", FRONTIER_MODEL),
        ("reasoning_effort", FRONTIER_REASONING_EFFORT),
        ("sandbox", "read-only"),
    ] {
        if let Some(value) = event.get(key)
            && value.as_str() != Some(expected)
        {
            return Err(CodexJsonlError::UnexpectedExecutionMetadata);
        }
    }
    if let Some(config) = event.get("config") {
        let config = config
            .as_object()
            .ok_or(CodexJsonlError::UnexpectedExecutionMetadata)?;
        for (key, expected) in [
            ("model", FRONTIER_MODEL),
            ("model_reasoning_effort", FRONTIER_REASONING_EFFORT),
            ("sandbox", "read-only"),
        ] {
            if let Some(value) = config.get(key)
                && value.as_str() != Some(expected)
            {
                return Err(CodexJsonlError::UnexpectedExecutionMetadata);
            }
        }
    }
    Ok(())
}

fn exact_u64(
    object: &serde_json::Map<String, Value>,
    key: &'static str,
) -> Result<u64, CodexJsonlError> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(CodexJsonlError::InvalidUsage)
}

#[derive(Debug, Error)]
pub enum CodexJsonlError {
    #[error("Codex JSONL byte length {0} is empty or exceeds its bound")]
    InvalidByteLength(usize),
    #[error("Codex JSONL is not UTF-8")]
    InvalidUtf8,
    #[error("Codex JSONL contains an empty line")]
    EmptyLine,
    #[error("Codex JSONL exceeds its event bound")]
    TooManyEvents,
    #[error("Codex JSONL contains an event after turn completion")]
    EventAfterCompletion,
    #[error("Codex JSONL line is malformed")]
    MalformedJson(#[source] serde_json::Error),
    #[error("Codex JSONL event is not an object")]
    EventNotObject,
    #[error("Codex JSONL event has no type")]
    MissingEventType,
    #[error("Codex JSONL contains an unknown event type")]
    UnknownEventType,
    #[error("Codex JSONL lifecycle is duplicated or out of order")]
    LifecycleOrder,
    #[error("Codex JSONL thread ID is empty or unbounded")]
    InvalidThreadId,
    #[error("Codex JSONL item event has no item object")]
    MissingItem,
    #[error("Codex JSONL item has no type")]
    MissingItemType,
    #[error("Codex JSONL item ID is empty or unbounded")]
    InvalidItemId,
    #[error("Codex JSONL duplicates an item lifecycle")]
    DuplicateItemLifecycle,
    #[error("Codex JSONL contains tool activity or an unknown item kind")]
    ToolOrUnknownItem,
    #[error("Codex JSONL agent message is empty")]
    InvalidAgentMessage,
    #[error("Codex JSONL contains more than one completed agent message")]
    DuplicateAgentMessage,
    #[error("Codex JSONL completion has no valid usage")]
    MissingUsage,
    #[error("Codex JSONL usage is malformed")]
    InvalidUsage,
    #[error("Codex JSONL reports an execution failure")]
    FailureEvent,
    #[error("Codex JSONL contradicts the requested model, reasoning, or sandbox configuration")]
    UnexpectedExecutionMetadata,
    #[error("Codex JSONL lifecycle is incomplete")]
    IncompleteLifecycle,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Vec<u8> {
        [
            r#"{"type":"thread.started","thread_id":"thread-1"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.completed","item":{"id":"reasoning-1","type":"reasoning","text":"hidden"}}"#,
            r#"{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"{\"winner\":\"A\"}"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3}}"#,
        ]
        .join("\n")
        .into_bytes()
    }

    #[test]
    fn complete_tool_free_stream_is_accepted() {
        let checked = check_tool_free_codex_jsonl(&valid()).expect("valid JSONL");
        assert_eq!(checked.thread_id(), "thread-1");
        assert_eq!(checked.output_tokens(), 3);
        assert_eq!(checked.final_agent_message(), r#"{"winner":"A"}"#);
    }

    #[test]
    fn any_tool_or_incomplete_stream_fails_closed() {
        let mut tool = valid();
        let insertion = b"{\"type\":\"item.started\",\"item\":{\"id\":\"tool-1\",\"type\":\"command_execution\"}}\n";
        let position = tool
            .windows(b"{\"type\":\"turn.completed\"".len())
            .position(|window| window == b"{\"type\":\"turn.completed\"")
            .expect("completion");
        tool.splice(position..position, insertion.iter().copied());
        assert!(matches!(
            check_tool_free_codex_jsonl(&tool),
            Err(CodexJsonlError::ToolOrUnknownItem)
        ));

        let mut incomplete = valid();
        incomplete.truncate(
            incomplete
                .windows(b"{\"type\":\"turn.completed\"".len())
                .position(|window| window == b"{\"type\":\"turn.completed\"")
                .expect("completion"),
        );
        assert!(matches!(
            check_tool_free_codex_jsonl(&incomplete),
            Err(CodexJsonlError::IncompleteLifecycle)
        ));

        let mut duplicate = valid();
        let insertion =
            b"{\"type\":\"item.completed\",\"item\":{\"id\":\"message-2\",\"type\":\"agent_message\",\"text\":\"{}\"}}\n";
        let position = duplicate
            .windows(b"{\"type\":\"turn.completed\"".len())
            .position(|window| window == b"{\"type\":\"turn.completed\"")
            .expect("completion");
        duplicate.splice(position..position, insertion.iter().copied());
        assert!(matches!(
            check_tool_free_codex_jsonl(&duplicate),
            Err(CodexJsonlError::DuplicateAgentMessage)
        ));
    }

    #[test]
    fn unfinished_or_inconsistent_item_lifecycles_fail_closed() {
        let unfinished = [
            r#"{"type":"thread.started","thread_id":"thread-1"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.started","item":{"id":"reasoning-1","type":"reasoning"}}"#,
            r#"{"type":"item.completed","item":{"id":"message-1","type":"agent_message","text":"{}"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1}}"#,
        ]
        .join("\n");
        assert!(matches!(
            check_tool_free_codex_jsonl(unfinished.as_bytes()),
            Err(CodexJsonlError::LifecycleOrder)
        ));

        let changed_kind = [
            r#"{"type":"thread.started","thread_id":"thread-1"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.started","item":{"id":"item-1","type":"reasoning"}}"#,
            r#"{"type":"item.completed","item":{"id":"item-1","type":"agent_message","text":"{}"}}"#,
        ]
        .join("\n");
        assert!(matches!(
            check_tool_free_codex_jsonl(changed_kind.as_bytes()),
            Err(CodexJsonlError::LifecycleOrder)
        ));
    }

    #[test]
    fn contradictory_execution_metadata_fails_closed_without_becoming_attestation() {
        for contradiction in [
            r#"{"type":"thread.started","thread_id":"thread-1","model":"other-model"}"#,
            r#"{"type":"thread.started","thread_id":"thread-1","reasoning_effort":"low"}"#,
            r#"{"type":"thread.started","thread_id":"thread-1","config":{"sandbox":"danger-full-access"}}"#,
        ] {
            let mut lines = valid();
            let first_end = lines.iter().position(|byte| *byte == b'\n').expect("line");
            lines.splice(..first_end, contradiction.bytes());
            assert!(matches!(
                check_tool_free_codex_jsonl(&lines),
                Err(CodexJsonlError::UnexpectedExecutionMetadata)
            ));
        }

        let matching = valid();
        let text = std::str::from_utf8(&matching).expect("UTF-8");
        let text = text.replacen(
            r#"{"type":"thread.started","thread_id":"thread-1"}"#,
            r#"{"type":"thread.started","thread_id":"thread-1","model":"gpt-5.6-sol","reasoning_effort":"xhigh","sandbox":"read-only"}"#,
            1,
        );
        check_tool_free_codex_jsonl(text.as_bytes()).expect("matching request metadata");
    }
}
