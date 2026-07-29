use crate::conversation_store::{
    Message, MessageRole, get_or_create_conversation, upsert_conversation,
};
use crate::mcp::mcp_call_tool;
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLoopStep {
    pub server: String,
    pub tool: String,
    pub arguments: Value,
    pub result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLoopOutput {
    pub conversation_id: String,
    pub prompt: String,
    pub steps: Vec<ToolLoopStep>,
    pub transcript_message_id: String,
}

pub fn tool_loop_run(
    conversation_id: &str,
    prompt: String,
    server: String,
    tool: String,
    arguments: Value,
    max_turns: u32,
) -> Result<CommandResult<ToolLoopOutput>> {
    if max_turns == 0 || max_turns > 8 {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_run",
            "stub_blocked",
            Blocker::new(
                "tool_loop_turn_limit_invalid",
                "Tool loops must be bounded between 1 and 8 turns.",
                vec!["Choose `--max-turns` between 1 and 8.".to_string()],
            ),
        ));
    }
    if prompt.trim().is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_run",
            "stub_blocked",
            Blocker::new(
                "tool_loop_prompt_empty",
                "Tool loop prompt is empty.",
                vec!["Provide the user-visible reason for the tool call.".to_string()],
            ),
        ));
    }
    let call = mcp_call_tool(&server, &tool, arguments.clone())?;
    if call.status == "blocked" {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_run",
            &call.readiness,
            call.blocker.unwrap_or_else(|| {
                Blocker::new(
                    "tool_loop_mcp_blocked",
                    "MCP tool call was blocked.",
                    vec!["Check MCP status.".to_string()],
                )
            }),
        ));
    }
    let call_result = call
        .result
        .map(|result| result.content)
        .unwrap_or(Value::Null);
    let step = ToolLoopStep {
        server,
        tool,
        arguments,
        result: call_result,
    };
    let (db, mut conversation) = get_or_create_conversation(conversation_id)?;
    let transcript = serde_json::to_string_pretty(&step)?;
    let message_id = Uuid::new_v4().to_string();
    conversation.messages.push(Message {
        id: message_id.clone(),
        conversation_id: conversation.id.clone(),
        role: MessageRole::System,
        content: format!("Tool loop for: {prompt}\n\n```json\n{transcript}\n```"),
        created_at: now_ms().to_string(),
        parent_id: conversation
            .messages
            .last()
            .map(|message| message.id.clone()),
        model: None,
        receipt_id: None,
        prompt_tokens: None,
        completion_tokens: None,
    });
    conversation.updated_at = now_ms().to_string();
    let path = upsert_conversation(db, conversation.clone())?;
    Ok(CommandResult::passed(
        "mom_llama.tool_loop_run",
        "host_integrated",
        ToolLoopOutput {
            conversation_id: conversation.id,
            prompt,
            steps: vec![step],
            transcript_message_id: message_id,
        },
        vec![path.display().to_string()],
        Vec::new(),
        true,
        false,
    ))
}
