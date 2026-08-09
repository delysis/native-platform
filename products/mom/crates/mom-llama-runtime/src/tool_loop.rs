use crate::config::resolve_settings;
use crate::conversation_store::{
    ChatTemplatePolicy, Conversation, Message, MessageRole, active_leaf_id, active_path_messages,
    get_or_create_conversation, upsert_conversation,
};
use crate::mcp::{McpTool, mcp_call_tool_supervised, mcp_list_tools};
use crate::native_runtime::{cancel_native_request, resident_model_for_profile};
use crate::now_ms;
use crate::receipts::{Blocker, CommandResult};
use crate::store::RuntimeStore;
use anyhow::Result;
use llama_native_types::{
    ChatMessage, ChatRole, ChatTemplateChoice, GenerationEventKind, GenerationInput,
    GenerationOutput, GenerationRequest, GenerationState, SamplingConfig,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

const MAX_TOOL_TURNS: u32 = 8;
const MODEL_TOOL_PREVIEW_BYTES: usize = 32 * 1024;
const MODEL_TURN_TIMEOUT: Duration = Duration::from_secs(120);
const TOOL_APPROVALS_NAMESPACE: &str = "tool-loop-approvals.v1";
const TOOL_PERMISSIONS_NAMESPACE: &str = "tool-permissions.v1";
const ACTIVE_TOOL_LOOPS_NAMESPACE: &str = "active-tool-loops.v1";
const TOOL_APPROVAL_TTL_MS: u128 = 5 * 60 * 1000;
const TOOL_LOOP_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLoopApproval {
    pub id: String,
    pub conversation_id: String,
    pub prompt: String,
    pub prompt_sha256: String,
    pub server: String,
    pub tool: String,
    pub arguments: Value,
    pub arguments_sha256: String,
    pub max_turns: u32,
    pub created_at_ms: u128,
    pub expires_at_ms: u128,
    pub consumed_at_ms: Option<u128>,
    #[serde(default = "default_requires_confirmation")]
    pub requires_confirmation: bool,
}

const fn default_requires_confirmation() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct ToolLoopApprovalDb {
    #[serde(default)]
    approvals: Vec<ToolLoopApproval>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionPolicy {
    Ask,
    AlwaysAllow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolPermission {
    pub server: String,
    pub tool: String,
    pub policy: ToolPermissionPolicy,
    pub updated_at_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct ToolPermissionDb {
    #[serde(default)]
    permissions: Vec<ToolPermission>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolLoopState {
    Running,
    CancelRequested,
    Cancelled,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveToolLoop {
    pub request_id: String,
    pub conversation_id: String,
    pub current_model_request_id: Option<String>,
    pub started_at_ms: u128,
    pub updated_at_ms: u128,
    pub state: ToolLoopState,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct ActiveToolLoopDb {
    #[serde(default)]
    loops: Vec<ActiveToolLoop>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolLoopCancelOutput {
    pub request_id: String,
    pub conversation_id: String,
    pub current_model_request_id: Option<String>,
    pub native_sequences_cancelled: usize,
}

#[derive(Clone)]
struct ToolLoopControl {
    cancel_requested: Arc<AtomicBool>,
    current_model_request_id: Arc<Mutex<Option<String>>>,
}

struct ToolModelWaitContext<'a> {
    data_dir: &'a Path,
    request_id: &'a str,
    conversation_id: &'a str,
    control: &'a ToolLoopControl,
    timeout: Duration,
    turn: u32,
    model_request_id: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLoopStep {
    pub turn: u32,
    pub server: String,
    pub tool: String,
    pub arguments: Value,
    pub result: Value,
    pub result_sha256: String,
    pub model_preview_truncated: bool,
    pub mcp_receipt_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLoopOutput {
    pub request_id: String,
    pub conversation_id: String,
    pub prompt: String,
    pub steps: Vec<ToolLoopStep>,
    pub final_answer: String,
    pub model_request_ids: Vec<String>,
    pub transcript_message_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLoopRunInput {
    pub conversation_id: String,
    pub prompt: String,
    pub server: String,
    pub tool: String,
    pub arguments: Value,
    pub max_turns: u32,
    pub approval_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolLoopStreamEvent {
    pub schema: String,
    pub command: String,
    pub request_id: String,
    pub conversation_id: String,
    pub event: String,
    pub turn: Option<u32>,
    pub model_request_id: Option<String>,
    pub server: Option<String>,
    pub tool: Option<String>,
    pub arguments: Option<Value>,
    pub result: Option<Value>,
    pub result_sha256: Option<String>,
    pub delta: Option<String>,
    pub message: Option<String>,
    pub real_engine_invoked: bool,
    pub fake_fixture: bool,
}

impl ToolLoopStreamEvent {
    fn new(request_id: &str, conversation_id: &str, event: &str, turn: Option<u32>) -> Self {
        Self {
            schema: "mom_llama.tool_loop_stream_event.v1".to_string(),
            command: "mom_llama.tool_loop_run".to_string(),
            request_id: request_id.to_string(),
            conversation_id: conversation_id.to_string(),
            event: event.to_string(),
            turn,
            model_request_id: None,
            server: None,
            tool: None,
            arguments: None,
            result: None,
            result_sha256: None,
            delta: None,
            message: None,
            real_engine_invoked: false,
            fake_fixture: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ToolDecision {
    Call { arguments: Value },
    Final { answer: String },
}

struct ToolLoopLifecycle {
    data_dir: PathBuf,
    request_id: String,
    finished: bool,
}

impl ToolLoopLifecycle {
    fn finish(&mut self, state: ToolLoopState) -> Result<()> {
        set_tool_loop_state(&self.data_dir, &self.request_id, state)?;
        unregister_tool_loop_control(&self.request_id);
        self.finished = true;
        Ok(())
    }
}

impl Drop for ToolLoopLifecycle {
    fn drop(&mut self) {
        if !self.finished {
            let _ = set_tool_loop_state(&self.data_dir, &self.request_id, ToolLoopState::Failed);
            unregister_tool_loop_control(&self.request_id);
        }
    }
}

pub fn tool_permission_list() -> Result<CommandResult<Vec<ToolPermission>>> {
    let store = RuntimeStore::current()?;
    let mut permissions = store
        .get::<ToolPermissionDb>(TOOL_PERMISSIONS_NAMESPACE)?
        .unwrap_or_default()
        .permissions;
    permissions.sort_by(|left, right| {
        left.server
            .cmp(&right.server)
            .then_with(|| left.tool.cmp(&right.tool))
    });
    Ok(CommandResult::passed(
        "mom_llama.tool_permission_list",
        "contracted",
        permissions,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn tool_permission_set(
    server: String,
    tool: String,
    policy: ToolPermissionPolicy,
) -> Result<CommandResult<ToolPermission>> {
    let server = server.trim().to_string();
    let tool = tool.trim().to_string();
    if server.is_empty() || tool.is_empty() {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_permission_set",
            "stub_blocked",
            Blocker::new(
                "tool_permission_target_missing",
                "A server and tool name are required for a tool permission.",
                vec!["Choose an advertised MCP tool.".to_string()],
            ),
        ));
    }
    let permission = ToolPermission {
        server,
        tool,
        policy,
        updated_at_ms: now_ms(),
    };
    let store = RuntimeStore::current()?;
    store.mutate(
        TOOL_PERMISSIONS_NAMESPACE,
        ToolPermissionDb::default,
        |db| {
            db.permissions.retain(|candidate| {
                candidate.server != permission.server || candidate.tool != permission.tool
            });
            db.permissions.push(permission.clone());
            Ok(())
        },
    )?;
    Ok(CommandResult::passed(
        "mom_llama.tool_permission_set",
        "contracted",
        permission,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn tool_permission_revoke(server: &str, tool: &str) -> Result<CommandResult<ToolPermission>> {
    let store = RuntimeStore::current()?;
    let removed = store.mutate(
        TOOL_PERMISSIONS_NAMESPACE,
        ToolPermissionDb::default,
        |db| {
            let removed = db
                .permissions
                .iter()
                .find(|candidate| candidate.server == server && candidate.tool == tool)
                .cloned();
            db.permissions
                .retain(|candidate| candidate.server != server || candidate.tool != tool);
            Ok(removed)
        },
    )?;
    let permission = removed.unwrap_or_else(|| ToolPermission {
        server: server.to_string(),
        tool: tool.to_string(),
        policy: ToolPermissionPolicy::Ask,
        updated_at_ms: now_ms(),
    });
    Ok(CommandResult::passed(
        "mom_llama.tool_permission_revoke",
        "contracted",
        permission,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn tool_loop_prepare(
    conversation_id: &str,
    prompt: String,
    server: String,
    tool: String,
    arguments: Value,
    max_turns: u32,
) -> Result<CommandResult<ToolLoopApproval>> {
    if let Some(blocked) = validate_loop_input(&prompt, &arguments, max_turns) {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_prepare",
            "stub_blocked",
            blocked,
        ));
    }
    let tool_contract = match resolve_tool_contract(&server, &tool)? {
        Ok(tool_contract) => tool_contract,
        Err((readiness, blocker)) => {
            return Ok(CommandResult::blocked(
                "mom_llama.tool_loop_prepare",
                &readiness,
                blocker,
            ));
        }
    };
    if let Some(blocker) = validate_tool_arguments(&tool_contract.input_schema, &arguments) {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_prepare",
            "stub_blocked",
            blocker,
        ));
    }
    let permission = tool_permission_policy(&server, &tool)?;
    if permission == ToolPermissionPolicy::Deny {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_prepare",
            "stub_blocked",
            Blocker::new(
                "tool_permission_denied",
                format!("Tool `{server}/{tool}` is denied by local policy."),
                vec!["Change or revoke the tool permission in Settings.".to_string()],
            ),
        ));
    }
    let now = now_ms();
    let approval = ToolLoopApproval {
        id: Uuid::new_v4().to_string(),
        conversation_id: conversation_id.to_string(),
        prompt: prompt.trim().to_string(),
        prompt_sha256: sha256_string(prompt.trim()),
        server,
        tool,
        arguments_sha256: sha256_json(&arguments)?,
        arguments,
        max_turns,
        created_at_ms: now,
        expires_at_ms: now.saturating_add(TOOL_APPROVAL_TTL_MS),
        consumed_at_ms: None,
        requires_confirmation: permission != ToolPermissionPolicy::AlwaysAllow,
    };
    let store = RuntimeStore::current()?;
    store.mutate(
        TOOL_APPROVALS_NAMESPACE,
        ToolLoopApprovalDb::default,
        |db| {
            db.approvals.retain(|candidate| {
                candidate.expires_at_ms >= now && candidate.consumed_at_ms.is_none()
            });
            db.approvals.push(approval.clone());
            Ok(())
        },
    )?;
    Ok(CommandResult::passed(
        "mom_llama.tool_loop_prepare",
        "contracted",
        approval,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn tool_loop_run(
    conversation_id: &str,
    prompt: String,
    server: String,
    tool: String,
    arguments: Value,
    max_turns: u32,
    approval_id: Option<String>,
) -> Result<CommandResult<ToolLoopOutput>> {
    tool_loop_run_with_events(
        ToolLoopRunInput {
            conversation_id: conversation_id.to_string(),
            prompt,
            server,
            tool,
            arguments,
            max_turns,
            approval_id,
        },
        None,
    )
}

pub fn tool_loop_run_stream<F>(
    input: ToolLoopRunInput,
    mut on_event: F,
) -> Result<CommandResult<ToolLoopOutput>>
where
    F: FnMut(ToolLoopStreamEvent) -> Result<()>,
{
    tool_loop_run_with_events(input, Some(&mut on_event))
}

fn tool_loop_run_with_events(
    input: ToolLoopRunInput,
    mut on_event: Option<&mut dyn FnMut(ToolLoopStreamEvent) -> Result<()>>,
) -> Result<CommandResult<ToolLoopOutput>> {
    let ToolLoopRunInput {
        conversation_id,
        prompt,
        server,
        tool,
        arguments,
        max_turns,
        approval_id,
    } = input;
    let conversation_id = conversation_id.as_str();
    if let Some(blocked) = validate_loop_input(&prompt, &arguments, max_turns) {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_run",
            "stub_blocked",
            blocked,
        ));
    }
    let tool_contract = match resolve_tool_contract(&server, &tool)? {
        Ok(tool_contract) => tool_contract,
        Err((readiness, blocker)) => {
            return Ok(CommandResult::blocked(
                "mom_llama.tool_loop_run",
                &readiness,
                blocker,
            ));
        }
    };
    if let Some(blocker) = validate_tool_arguments(&tool_contract.input_schema, &arguments) {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_run",
            "stub_blocked",
            blocker,
        ));
    }
    if let Some(blocker) = consume_approval(
        conversation_id,
        &prompt,
        &server,
        &tool,
        &arguments,
        max_turns,
        approval_id.as_deref(),
    )? {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_run",
            "stub_blocked",
            blocker,
        ));
    }
    let settings = resolve_settings()?;
    let request_id = Uuid::new_v4().to_string();
    let control = register_tool_loop(&settings.data_dir, &request_id, conversation_id)?;
    emit_tool_loop_event(
        &mut on_event,
        ToolLoopStreamEvent::new(&request_id, conversation_id, "started", None),
    )?;
    let mut lifecycle = ToolLoopLifecycle {
        data_dir: settings.data_dir.clone(),
        request_id: request_id.clone(),
        finished: false,
    };
    let (_, source_conversation) = get_or_create_conversation(conversation_id)?;
    let model_path = source_conversation
        .execution_profile
        .model_path
        .as_deref()
        .or(source_conversation.selected_model_path.as_deref())
        .or(settings.model_path.as_deref());
    let Some(model_path) = model_path else {
        lifecycle.finish(ToolLoopState::Failed)?;
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_run",
            "blocked_missing_model",
            Blocker::new(
                "model_path_missing",
                "No GGUF model is configured for this conversation.",
                vec!["Choose a model in the conversation profile or Settings.".to_string()],
            ),
        ));
    };
    let handle = match resident_model_for_profile(
        &settings,
        model_path,
        source_conversation
            .execution_profile
            .mmproj_path
            .as_deref()
            .or(settings.mmproj_path.as_deref()),
    ) {
        Ok(handle) => handle,
        Err(blocked) => {
            lifecycle.finish(ToolLoopState::Failed)?;
            return Ok(CommandResult::blocked(
                "mom_llama.tool_loop_run",
                &blocked.readiness,
                blocked.blocker,
            ));
        }
    };
    let model_status = handle.status();

    macro_rules! finish_return {
        ($state:expr, $result:expr) => {{
            lifecycle.finish($state)?;
            return Ok($result);
        }};
    }
    let prompt = prompt.trim().to_string();
    let mut changed_paths = Vec::new();
    let mut transcript_message_ids = Vec::new();
    let (user_message_id, store_path) = append_message(
        conversation_id,
        MessageRole::User,
        prompt.clone(),
        None,
        None,
    )?;
    transcript_message_ids.push(user_message_id);
    push_unique_path(&mut changed_paths, store_path);

    let mut steps = Vec::new();
    let mut model_request_ids = Vec::new();
    let mut next_arguments = arguments;

    loop {
        if tool_loop_cancel_requested(&settings.data_dir, &request_id, &control)? {
            finish_return!(
                ToolLoopState::Cancelled,
                tool_loop_cancelled_result(changed_paths, !model_request_ids.is_empty())
            );
        }
        let turn = u32::try_from(steps.len()).unwrap_or(MAX_TOOL_TURNS) + 1;
        let mut call_event = ToolLoopStreamEvent::new(
            &request_id,
            conversation_id,
            "tool_call_started",
            Some(turn),
        );
        call_event.server = Some(server.clone());
        call_event.tool = Some(tool.clone());
        call_event.arguments = Some(next_arguments.clone());
        emit_tool_loop_event(&mut on_event, call_event)?;
        let cancellation_check = || {
            tool_loop_cancel_requested(&settings.data_dir, &request_id, &control).unwrap_or(true)
        };
        let step = match execute_tool_step(
            turn,
            &server,
            &tool,
            next_arguments,
            &tool_contract.input_schema,
            &cancellation_check,
        )? {
            Ok(step) => step,
            Err((readiness, blocker)) => {
                let cancelled =
                    tool_loop_cancel_requested(&settings.data_dir, &request_id, &control)?;
                finish_return!(
                    if cancelled {
                        ToolLoopState::Cancelled
                    } else {
                        ToolLoopState::Failed
                    },
                    if cancelled {
                        tool_loop_cancelled_result(changed_paths, !model_request_ids.is_empty())
                    } else {
                        CommandResult::blocked_with_evidence(
                            "mom_llama.tool_loop_run",
                            &readiness,
                            blocker,
                            changed_paths,
                            Vec::new(),
                            !model_request_ids.is_empty(),
                            false,
                        )
                    }
                );
            }
        };
        let mut result_event =
            ToolLoopStreamEvent::new(&request_id, conversation_id, "tool_result", Some(turn));
        result_event.server = Some(step.server.clone());
        result_event.tool = Some(step.tool.clone());
        result_event.arguments = Some(step.arguments.clone());
        result_event.result = Some(step.result.clone());
        result_event.result_sha256 = Some(step.result_sha256.clone());
        emit_tool_loop_event(&mut on_event, result_event)?;
        if tool_loop_cancel_requested(&settings.data_dir, &request_id, &control)? {
            finish_return!(
                ToolLoopState::Cancelled,
                tool_loop_cancelled_result(changed_paths, !model_request_ids.is_empty())
            );
        }
        let tool_message = serde_json::to_string_pretty(&json!({
            "turn": turn,
            "server": &step.server,
            "tool": &step.tool,
            "arguments": &step.arguments,
            "result": &step.result,
            "result_sha256": &step.result_sha256,
        }))?;
        let (message_id, store_path) = append_message(
            conversation_id,
            MessageRole::Tool,
            tool_message,
            None,
            Some(step.mcp_receipt_id.clone()),
        )?;
        transcript_message_ids.push(message_id.clone());
        push_unique_path(&mut changed_paths, store_path);
        steps.push(step);

        let conversation = get_or_create_conversation(conversation_id)?.1;
        let model_request_id = format!("{request_id}:turn:{turn}");
        let conversation_sampling = conversation
            .execution_profile
            .sampling
            .clone()
            .unwrap_or_else(|| settings.sampling_config());
        let request = GenerationRequest {
            request_id: model_request_id.clone(),
            model_id: model_status.model_id.clone(),
            input: GenerationInput::Chat {
                messages: build_model_messages(&conversation, &tool_contract),
                template: match &conversation.execution_profile.chat_template {
                    ChatTemplatePolicy::ModelDefault => ChatTemplateChoice::ModelDefault,
                    ChatTemplatePolicy::FrozenSource(template) => {
                        ChatTemplateChoice::Override(template.clone())
                    }
                },
            },
            sampling: tool_loop_sampling(&conversation_sampling),
            media: Vec::new(),
            cached_prefix: None,
        };
        set_current_model_request(
            &settings.data_dir,
            &request_id,
            &control,
            Some(model_request_id.clone()),
        )?;
        let mut model_started =
            ToolLoopStreamEvent::new(&request_id, conversation_id, "model_started", Some(turn));
        model_started.model_request_id = Some(model_request_id.clone());
        model_started.real_engine_invoked = true;
        emit_tool_loop_event(&mut on_event, model_started)?;
        let ticket = handle
            .generate(request)
            .map_err(|error| anyhow::anyhow!(error))?;
        let Some(outputs) = wait_for_tool_model(
            &ticket,
            ToolModelWaitContext {
                data_dir: &settings.data_dir,
                request_id: &request_id,
                conversation_id,
                control: &control,
                timeout: MODEL_TURN_TIMEOUT,
                turn,
                model_request_id: &model_request_id,
            },
            &mut on_event,
        )?
        else {
            finish_return!(
                ToolLoopState::Cancelled,
                tool_loop_cancelled_result(changed_paths, true)
            );
        };
        set_current_model_request(&settings.data_dir, &request_id, &control, None)?;
        model_request_ids.push(model_request_id);
        let Some(output) = outputs.into_iter().next() else {
            finish_return!(
                ToolLoopState::Failed,
                CommandResult::blocked_with_evidence(
                    "mom_llama.tool_loop_run",
                    "blocked_native_runtime",
                    Blocker::new(
                        "tool_loop_model_response_missing",
                        "The local model returned no tool-loop response.",
                        vec!["Try the request again.".to_string()],
                    ),
                    changed_paths,
                    Vec::new(),
                    true,
                    false,
                )
            );
        };
        if !output.real_engine_invoked || output.fake_fixture {
            finish_return!(
                ToolLoopState::Failed,
                CommandResult::blocked_with_evidence(
                    "mom_llama.tool_loop_run",
                    "stub_blocked",
                    Blocker::new(
                        "tool_loop_real_model_not_invoked",
                        "The tool loop did not receive evidence of native llama.cpp generation.",
                        vec!["Run with a configured local GGUF model.".to_string()],
                    ),
                    changed_paths,
                    Vec::new(),
                    false,
                    output.fake_fixture,
                )
            );
        }
        if output.state != GenerationState::Completed {
            let cancelled = output.state == GenerationState::Cancelled
                || tool_loop_cancel_requested(&settings.data_dir, &request_id, &control)?;
            finish_return!(
                if cancelled {
                    ToolLoopState::Cancelled
                } else {
                    ToolLoopState::Failed
                },
                if cancelled {
                    tool_loop_cancelled_result(changed_paths, output.real_engine_invoked)
                } else {
                    CommandResult::blocked_with_evidence(
                        "mom_llama.tool_loop_run",
                        "stub_blocked",
                        Blocker::new(
                            "tool_loop_generation_not_completed",
                            format!("The local model ended the tool turn as {:?}.", output.state),
                            vec!["Retry the tool loop.".to_string()],
                        ),
                        changed_paths,
                        Vec::new(),
                        output.real_engine_invoked,
                        false,
                    )
                }
            );
        }
        update_message_metrics(
            conversation_id,
            &message_id,
            output.metrics.prompt_tokens,
            output.metrics.completion_tokens,
        )?;

        match parse_tool_decision(&output.text) {
            ToolDecision::Final { answer } => {
                let answer = answer.trim().to_string();
                if answer.is_empty() {
                    finish_return!(
                        ToolLoopState::Failed,
                        CommandResult::blocked_with_evidence(
                            "mom_llama.tool_loop_run",
                            "blocked_native_runtime",
                            Blocker::new(
                                "tool_loop_final_answer_empty",
                                "The local model returned an empty final answer.",
                                vec!["Retry the tool loop.".to_string()],
                            ),
                            changed_paths,
                            Vec::new(),
                            true,
                            false,
                        )
                    );
                }
                let (assistant_message_id, store_path) = append_message(
                    conversation_id,
                    MessageRole::Assistant,
                    answer.clone(),
                    Some(model_status.model_id.clone()),
                    model_request_ids.last().cloned(),
                )?;
                transcript_message_ids.push(assistant_message_id);
                push_unique_path(&mut changed_paths, store_path);
                let mut completed =
                    ToolLoopStreamEvent::new(&request_id, conversation_id, "completed", Some(turn));
                completed.model_request_id = model_request_ids.last().cloned();
                completed.message = Some(answer.clone());
                completed.real_engine_invoked = true;
                emit_tool_loop_event(&mut on_event, completed)?;
                finish_return!(
                    ToolLoopState::Completed,
                    CommandResult::passed(
                        "mom_llama.tool_loop_run",
                        "real_prompt_smoke_passed",
                        ToolLoopOutput {
                            request_id,
                            conversation_id: conversation_id.to_string(),
                            prompt,
                            steps,
                            final_answer: answer,
                            model_request_ids,
                            transcript_message_ids,
                        },
                        changed_paths,
                        Vec::new(),
                        true,
                        false,
                    )
                );
            }
            ToolDecision::Call { arguments } => {
                let mut requested = ToolLoopStreamEvent::new(
                    &request_id,
                    conversation_id,
                    "tool_call_requested",
                    Some(turn.saturating_add(1)),
                );
                requested.server = Some(server.clone());
                requested.tool = Some(tool.clone());
                requested.arguments = Some(arguments.clone());
                requested.real_engine_invoked = true;
                emit_tool_loop_event(&mut on_event, requested)?;
                if steps.len() >= max_turns as usize {
                    finish_return!(
                        ToolLoopState::Failed,
                        CommandResult::blocked_with_evidence(
                            "mom_llama.tool_loop_run",
                            "stub_blocked",
                            Blocker::new(
                                "tool_loop_turn_limit_reached",
                                format!(
                                    "The local model requested another tool call after the configured {max_turns}-turn limit."
                                ),
                                vec![
                                    "Raise the bounded turn limit or simplify the request."
                                        .to_string(),
                                ],
                            ),
                            changed_paths,
                            Vec::new(),
                            true,
                            false,
                        )
                    );
                }
                if let Some(blocker) =
                    validate_tool_arguments(&tool_contract.input_schema, &arguments)
                {
                    finish_return!(
                        ToolLoopState::Failed,
                        CommandResult::blocked_with_evidence(
                            "mom_llama.tool_loop_run",
                            "stub_blocked",
                            blocker,
                            changed_paths,
                            Vec::new(),
                            true,
                            false,
                        )
                    );
                }
                next_arguments = arguments;
            }
        }
    }
}

pub fn tool_loop_cancel(conversation_id: &str) -> Result<CommandResult<ToolLoopCancelOutput>> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    let db = store
        .get::<ActiveToolLoopDb>(ACTIVE_TOOL_LOOPS_NAMESPACE)?
        .unwrap_or_default();
    let Some(active) = db
        .loops
        .iter()
        .rev()
        .find(|active| {
            active.conversation_id == conversation_id
                && matches!(
                    active.state,
                    ToolLoopState::Running | ToolLoopState::CancelRequested
                )
        })
        .cloned()
    else {
        return Ok(CommandResult::blocked(
            "mom_llama.tool_loop_cancel",
            "stub_blocked",
            Blocker::new(
                "no_active_tool_loop",
                format!("No tool loop is active for conversation {conversation_id}."),
                vec!["Start an approved tool loop before cancelling.".to_string()],
            ),
        ));
    };

    let control = tool_loop_controls()
        .lock()
        .ok()
        .and_then(|registry| registry.get(&active.request_id).cloned());
    if let Some(control) = &control {
        control.cancel_requested.store(true, Ordering::Release);
    }
    set_tool_loop_state(
        &settings.data_dir,
        &active.request_id,
        ToolLoopState::CancelRequested,
    )?;
    let current_model_request_id = control
        .as_ref()
        .and_then(|control| {
            control
                .current_model_request_id
                .lock()
                .ok()
                .and_then(|request| request.clone())
        })
        .or(active.current_model_request_id);
    let native_sequences_cancelled = current_model_request_id
        .as_deref()
        .map(|request_id| cancel_native_request(request_id, None))
        .unwrap_or_default();

    Ok(CommandResult::passed(
        "mom_llama.tool_loop_cancel",
        "contracted",
        ToolLoopCancelOutput {
            request_id: active.request_id,
            conversation_id: conversation_id.to_string(),
            current_model_request_id,
            native_sequences_cancelled,
        },
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

pub fn tool_loop_status(
    conversation_id: Option<&str>,
) -> Result<CommandResult<Vec<ActiveToolLoop>>> {
    let settings = resolve_settings()?;
    let store = RuntimeStore::open(&settings.data_dir)?;
    let mut loops = store
        .get::<ActiveToolLoopDb>(ACTIVE_TOOL_LOOPS_NAMESPACE)?
        .unwrap_or_default()
        .loops;
    if let Some(conversation_id) = conversation_id {
        loops.retain(|active| active.conversation_id == conversation_id);
    }
    loops.sort_by_key(|active| std::cmp::Reverse(active.updated_at_ms));
    Ok(CommandResult::passed(
        "mom_llama.tool_loop_status",
        "contracted",
        loops,
        vec![store.path().display().to_string()],
        Vec::new(),
        false,
        false,
    ))
}

fn tool_loop_cancelled_result(
    changed_paths: Vec<String>,
    real_engine_invoked: bool,
) -> CommandResult<ToolLoopOutput> {
    CommandResult::blocked_with_evidence(
        "mom_llama.tool_loop_run",
        "cancelled",
        Blocker::new(
            "tool_loop_cancelled",
            "The approved tool loop was cancelled.",
            vec!["Review the partial local transcript before retrying.".to_string()],
        ),
        changed_paths,
        Vec::new(),
        real_engine_invoked,
        false,
    )
}

fn wait_for_tool_model(
    ticket: &llama_native_engine::GenerationTicket,
    context: ToolModelWaitContext<'_>,
    on_event: &mut Option<&mut dyn FnMut(ToolLoopStreamEvent) -> Result<()>>,
) -> Result<Option<Vec<GenerationOutput>>> {
    let ToolModelWaitContext {
        data_dir,
        request_id,
        conversation_id,
        control,
        timeout,
        turn,
        model_request_id,
    } = context;
    let started = Instant::now();
    let mut cancelled = false;
    loop {
        while let Ok(event) = ticket.events.try_recv() {
            match event.event {
                GenerationEventKind::Delta { text } => {
                    let mut streamed = ToolLoopStreamEvent::new(
                        request_id,
                        conversation_id,
                        "model_delta",
                        Some(turn),
                    );
                    streamed.model_request_id = Some(model_request_id.to_string());
                    streamed.delta = Some(text);
                    streamed.real_engine_invoked = true;
                    emit_tool_loop_event(on_event, streamed)?;
                }
                GenerationEventKind::State { state } => {
                    let mut streamed = ToolLoopStreamEvent::new(
                        request_id,
                        conversation_id,
                        "model_state",
                        Some(turn),
                    );
                    streamed.model_request_id = Some(model_request_id.to_string());
                    streamed.message = Some(format!("{state:?}").to_lowercase());
                    streamed.real_engine_invoked = true;
                    emit_tool_loop_event(on_event, streamed)?;
                }
                GenerationEventKind::Warning { code, message } => {
                    let mut streamed = ToolLoopStreamEvent::new(
                        request_id,
                        conversation_id,
                        "warning",
                        Some(turn),
                    );
                    streamed.model_request_id = Some(model_request_id.to_string());
                    streamed.message = Some(format!("{code}: {message}"));
                    streamed.real_engine_invoked = true;
                    emit_tool_loop_event(on_event, streamed)?;
                }
            }
        }
        if tool_loop_cancel_requested(data_dir, request_id, control)? {
            ticket.cancel_all();
            cancelled = true;
        }
        if let Some(outputs) = ticket.try_wait().map_err(anyhow::Error::new)? {
            return Ok((!cancelled).then_some(outputs));
        }
        if started.elapsed() >= timeout {
            ticket.cancel_all();
            anyhow::bail!(
                "native tool-loop generation did not finish within {:.3}s",
                timeout.as_secs_f64()
            );
        }
        std::thread::sleep(TOOL_LOOP_POLL_INTERVAL);
    }
}

fn emit_tool_loop_event(
    on_event: &mut Option<&mut dyn FnMut(ToolLoopStreamEvent) -> Result<()>>,
    event: ToolLoopStreamEvent,
) -> Result<()> {
    if let Some(callback) = on_event.as_deref_mut() {
        callback(event)?;
    }
    Ok(())
}

fn register_tool_loop(
    data_dir: &Path,
    request_id: &str,
    conversation_id: &str,
) -> Result<ToolLoopControl> {
    let control = ToolLoopControl {
        cancel_requested: Arc::new(AtomicBool::new(false)),
        current_model_request_id: Arc::new(Mutex::new(None)),
    };
    tool_loop_controls()
        .lock()
        .map_err(|_| anyhow::anyhow!("tool-loop control registry is unavailable"))?
        .insert(request_id.to_string(), control.clone());
    let now = now_ms();
    RuntimeStore::open(data_dir)?.mutate(
        ACTIVE_TOOL_LOOPS_NAMESPACE,
        ActiveToolLoopDb::default,
        |db| {
            db.loops.retain(|active| {
                matches!(
                    active.state,
                    ToolLoopState::Running | ToolLoopState::CancelRequested
                ) || active.updated_at_ms.saturating_add(24 * 60 * 60 * 1000) >= now
            });
            db.loops.push(ActiveToolLoop {
                request_id: request_id.to_string(),
                conversation_id: conversation_id.to_string(),
                current_model_request_id: None,
                started_at_ms: now,
                updated_at_ms: now,
                state: ToolLoopState::Running,
            });
            Ok(())
        },
    )?;
    Ok(control)
}

fn set_current_model_request(
    data_dir: &Path,
    request_id: &str,
    control: &ToolLoopControl,
    model_request_id: Option<String>,
) -> Result<()> {
    if let Ok(mut current) = control.current_model_request_id.lock() {
        *current = model_request_id.clone();
    }
    mutate_active_tool_loop(data_dir, request_id, |active| {
        active.current_model_request_id = model_request_id;
    })
}

fn set_tool_loop_state(data_dir: &Path, request_id: &str, state: ToolLoopState) -> Result<()> {
    mutate_active_tool_loop(data_dir, request_id, |active| {
        active.state = state;
        if !matches!(
            active.state,
            ToolLoopState::Running | ToolLoopState::CancelRequested
        ) {
            active.current_model_request_id = None;
        }
    })
}

fn mutate_active_tool_loop(
    data_dir: &Path,
    request_id: &str,
    mutation: impl FnOnce(&mut ActiveToolLoop),
) -> Result<()> {
    RuntimeStore::open(data_dir)?.mutate(
        ACTIVE_TOOL_LOOPS_NAMESPACE,
        ActiveToolLoopDb::default,
        |db| {
            if let Some(active) = db
                .loops
                .iter_mut()
                .find(|active| active.request_id == request_id)
            {
                mutation(active);
                active.updated_at_ms = now_ms();
            }
            Ok(())
        },
    )
}

fn tool_loop_cancel_requested(
    data_dir: &Path,
    request_id: &str,
    control: &ToolLoopControl,
) -> Result<bool> {
    if control.cancel_requested.load(Ordering::Acquire) {
        return Ok(true);
    }
    let persisted = RuntimeStore::open(data_dir)?
        .get::<ActiveToolLoopDb>(ACTIVE_TOOL_LOOPS_NAMESPACE)?
        .unwrap_or_default()
        .loops
        .into_iter()
        .find(|active| active.request_id == request_id)
        .is_some_and(|active| {
            matches!(
                active.state,
                ToolLoopState::CancelRequested | ToolLoopState::Cancelled
            )
        });
    if persisted {
        control.cancel_requested.store(true, Ordering::Release);
    }
    Ok(persisted)
}

fn unregister_tool_loop_control(request_id: &str) {
    if let Ok(mut registry) = tool_loop_controls().lock() {
        registry.remove(request_id);
    }
}

fn tool_loop_controls() -> &'static Mutex<HashMap<String, ToolLoopControl>> {
    static CONTROLS: OnceLock<Mutex<HashMap<String, ToolLoopControl>>> = OnceLock::new();
    CONTROLS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn validate_loop_input(prompt: &str, arguments: &Value, max_turns: u32) -> Option<Blocker> {
    if max_turns == 0 || max_turns > MAX_TOOL_TURNS {
        return Some(Blocker::new(
            "tool_loop_turn_limit_invalid",
            format!("Tool loops must be bounded between 1 and {MAX_TOOL_TURNS} turns."),
            vec![format!(
                "Choose `--max-turns` between 1 and {MAX_TOOL_TURNS}."
            )],
        ));
    }
    if prompt.trim().is_empty() {
        return Some(Blocker::new(
            "tool_loop_prompt_empty",
            "Tool loop prompt is empty.",
            vec!["Describe what you want the local model to accomplish.".to_string()],
        ));
    }
    if !arguments.is_object() {
        return Some(Blocker::new(
            "tool_loop_arguments_invalid",
            "Tool arguments must be a JSON object.",
            vec!["Provide an object such as `{}`.".to_string()],
        ));
    }
    None
}

fn consume_approval(
    conversation_id: &str,
    prompt: &str,
    server: &str,
    tool: &str,
    arguments: &Value,
    max_turns: u32,
    approval_id: Option<&str>,
) -> Result<Option<Blocker>> {
    match tool_permission_policy(server, tool)? {
        ToolPermissionPolicy::Deny => {
            return Ok(Some(Blocker::new(
                "tool_permission_denied",
                format!("Tool `{server}/{tool}` is denied by local policy."),
                vec!["Change or revoke the tool permission in Settings.".to_string()],
            )));
        }
        ToolPermissionPolicy::AlwaysAllow if approval_id.is_none() => return Ok(None),
        ToolPermissionPolicy::Ask | ToolPermissionPolicy::AlwaysAllow => {}
    }
    let Some(approval_id) = approval_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(Some(Blocker::new(
            "tool_loop_approval_required",
            "This tool call requires explicit approval.",
            vec![
                "Run `mom-llama tool-loop prepare ... --json`, review the exact call, then pass its approval ID."
                    .to_string(),
            ],
        )));
    };
    let now = now_ms();
    let prompt_sha256 = sha256_string(prompt.trim());
    let arguments_sha256 = sha256_json(arguments)?;
    let store = RuntimeStore::current()?;
    store.mutate(
        TOOL_APPROVALS_NAMESPACE,
        ToolLoopApprovalDb::default,
        |db| {
            let Some(approval) = db
                .approvals
                .iter_mut()
                .find(|approval| approval.id == approval_id)
            else {
                return Ok(Some(Blocker::new(
                    "tool_loop_approval_not_found",
                    "The tool approval was not found.",
                    vec!["Prepare and review the tool call again.".to_string()],
                )));
            };
            if approval.consumed_at_ms.is_some() {
                return Ok(Some(Blocker::new(
                    "tool_loop_approval_consumed",
                    "This one-time tool approval has already been used.",
                    vec!["Prepare and review a new tool call.".to_string()],
                )));
            }
            if approval.expires_at_ms < now {
                return Ok(Some(Blocker::new(
                    "tool_loop_approval_expired",
                    "The tool approval expired before it was used.",
                    vec!["Prepare and review the tool call again.".to_string()],
                )));
            }
            let exact_match = approval.conversation_id == conversation_id
                && approval.prompt_sha256 == prompt_sha256
                && approval.server == server
                && approval.tool == tool
                && approval.arguments_sha256 == arguments_sha256
                && approval.max_turns == max_turns;
            if !exact_match {
                return Ok(Some(Blocker::new(
                    "tool_loop_approval_mismatch",
                    "The approved tool call does not match this request.",
                    vec!["Prepare and review the exact request again.".to_string()],
                )));
            }
            approval.consumed_at_ms = Some(now);
            Ok(None)
        },
    )
}

pub(crate) fn tool_permission_policy(server: &str, tool: &str) -> Result<ToolPermissionPolicy> {
    Ok(RuntimeStore::current()?
        .get::<ToolPermissionDb>(TOOL_PERMISSIONS_NAMESPACE)?
        .unwrap_or_default()
        .permissions
        .into_iter()
        .find(|permission| permission.server == server && permission.tool == tool)
        .map(|permission| permission.policy)
        .unwrap_or(ToolPermissionPolicy::Ask))
}

fn sha256_string(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn sha256_json(value: &Value) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

pub(crate) fn resolve_tool_contract(
    server: &str,
    tool: &str,
) -> Result<std::result::Result<McpTool, (String, Blocker)>> {
    let tools = mcp_list_tools(server)?;
    if tools.status == "blocked" {
        return Ok(Err((
            tools.readiness,
            tools.blocker.unwrap_or_else(|| {
                Blocker::new(
                    "tool_loop_mcp_blocked",
                    "The configured MCP server is unavailable.",
                    vec!["Check MCP status.".to_string()],
                )
            }),
        )));
    }
    let Some(contract) = tools
        .result
        .unwrap_or_default()
        .into_iter()
        .find(|candidate| candidate.name == tool)
    else {
        return Ok(Err((
            "stub_blocked".to_string(),
            Blocker::new(
                "tool_loop_tool_not_advertised",
                format!("The configured MCP server did not advertise tool `{tool}`."),
                vec!["Choose a tool returned by `mcp list-tools`.".to_string()],
            ),
        )));
    };
    Ok(Ok(contract))
}

fn execute_tool_step(
    turn: u32,
    server: &str,
    tool: &str,
    arguments: Value,
    input_schema: &Value,
    should_cancel: &dyn Fn() -> bool,
) -> Result<std::result::Result<ToolLoopStep, (String, Blocker)>> {
    if let Some(blocker) = validate_tool_arguments(input_schema, &arguments) {
        return Ok(Err(("stub_blocked".to_string(), blocker)));
    }
    let call = match mcp_call_tool_supervised(server, tool, arguments.clone(), should_cancel) {
        Ok(call) => call,
        Err(_) if should_cancel() => {
            return Ok(Err((
                "cancelled".to_string(),
                Blocker::new(
                    "tool_loop_cancelled",
                    "The approved tool loop was cancelled while the local tool was running.",
                    vec!["Review the partial local transcript before retrying.".to_string()],
                ),
            )));
        }
        Err(error) => return Err(error),
    };
    if call.status == "blocked" {
        return Ok(Err((
            call.readiness,
            call.blocker.unwrap_or_else(|| {
                Blocker::new(
                    "tool_loop_mcp_blocked",
                    "MCP tool execution was blocked.",
                    vec!["Check MCP status.".to_string()],
                )
            }),
        )));
    }
    let receipt_id = call.receipt.task_id.clone();
    let result = call
        .result
        .map(|result| result.content)
        .unwrap_or(Value::Null);
    let encoded = serde_json::to_vec(&result)?;
    let result_sha256 = format!("{:x}", Sha256::digest(&encoded));
    Ok(Ok(ToolLoopStep {
        turn,
        server: server.to_string(),
        tool: tool.to_string(),
        arguments,
        result,
        result_sha256,
        model_preview_truncated: encoded.len() > MODEL_TOOL_PREVIEW_BYTES,
        mcp_receipt_id: receipt_id,
    }))
}

fn build_model_messages(conversation: &Conversation, tool: &McpTool) -> Vec<ChatMessage> {
    let schema = serde_json::to_string(&tool.input_schema).unwrap_or_else(|_| "{}".to_string());
    let mut messages = vec![ChatMessage {
        role: ChatRole::System,
        content: format!(
            "You are a bounded local tool planner. The user explicitly authorized only the MCP tool `{}` for this run. Its JSON input schema is: {schema}\n\
             Use the tool result already present in the conversation. If another call to that same tool is strictly required, return only one JSON object in this exact shape: \
             {{\"action\":\"call\",\"arguments\":{{...}}}}. Otherwise return only one JSON object in this exact shape: \
             {{\"action\":\"final\",\"answer\":\"your complete answer to the user\"}}. \
             Never select another tool, invent a tool result, or imply clinical authority.",
            tool.name
        ),
    }];
    messages.extend(active_path_messages(conversation).iter().map(|message| ChatMessage {
        role: match message.role {
            MessageRole::System => ChatRole::System,
            MessageRole::User => ChatRole::User,
            MessageRole::Assistant => ChatRole::Assistant,
            MessageRole::Tool => ChatRole::System,
        },
        content: if message.role == MessageRole::Tool {
            let (preview, truncated) = utf8_prefix(&message.content, MODEL_TOOL_PREVIEW_BYTES);
            if truncated {
                format!(
                    "Authorized tool result (preview; full result is encrypted in local storage):\n{preview}"
                )
            } else {
                format!("Authorized tool result:\n{preview}")
            }
        } else {
            message.content.clone()
        },
    }));
    messages
}

fn tool_loop_sampling(settings: &SamplingConfig) -> SamplingConfig {
    let mut sampling = settings.clone();
    sampling.max_tokens = sampling.max_tokens.clamp(64, 512);
    sampling.temperature = sampling.temperature.min(0.3);
    sampling.dynamic_temperature_range = 0.0;
    sampling
}

fn parse_tool_decision(output: &str) -> ToolDecision {
    let trimmed = output.trim();
    if let Some(value) = extract_json_object(trimmed)
        && let Ok(decision) = serde_json::from_str::<ToolDecision>(value)
    {
        return decision;
    }
    ToolDecision::Final {
        answer: trimmed.to_string(),
    }
}

fn extract_json_object(value: &str) -> Option<&str> {
    let start = value.find('{')?;
    let end = value.rfind('}')?;
    (end >= start).then_some(&value[start..=end])
}

pub(crate) fn validate_tool_arguments(schema: &Value, arguments: &Value) -> Option<Blocker> {
    let Some(object) = arguments.as_object() else {
        return Some(Blocker::new(
            "tool_loop_arguments_invalid",
            "Tool arguments must be a JSON object.",
            vec!["Provide an object such as `{}`.".to_string()],
        ));
    };
    if schema
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "object")
    {
        return Some(Blocker::new(
            "tool_loop_schema_unsupported",
            "The selected tool does not expose an object input schema.",
            vec!["Choose a tool with a JSON object input schema.".to_string()],
        ));
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let missing = required
            .iter()
            .filter_map(Value::as_str)
            .filter(|name| !object.contains_key(*name))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Some(Blocker::new(
                "tool_loop_required_argument_missing",
                format!(
                    "The tool call is missing required arguments: {}.",
                    missing.join(", ")
                ),
                vec!["Provide every required tool argument.".to_string()],
            ));
        }
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, value) in object {
            let Some(property) = properties.get(name) else {
                continue;
            };
            let Some(expected) = property.get("type").and_then(Value::as_str) else {
                continue;
            };
            let matches = match expected {
                "array" => value.is_array(),
                "boolean" => value.is_boolean(),
                "integer" => value.is_i64() || value.is_u64(),
                "number" => value.is_number(),
                "object" => value.is_object(),
                "string" => value.is_string(),
                "null" => value.is_null(),
                _ => true,
            };
            if !matches {
                return Some(Blocker::new(
                    "tool_loop_argument_type_invalid",
                    format!("Tool argument `{name}` must have JSON type `{expected}`."),
                    vec!["Correct the tool arguments and retry.".to_string()],
                ));
            }
        }
    }
    None
}

fn append_message(
    conversation_id: &str,
    role: MessageRole,
    content: String,
    model: Option<String>,
    receipt_id: Option<String>,
) -> Result<(String, PathBuf)> {
    let (db, mut conversation) = get_or_create_conversation(conversation_id)?;
    let message_id = Uuid::new_v4().to_string();
    let parent_id = active_leaf_id(&conversation);
    conversation.messages.push(Message {
        id: message_id.clone(),
        conversation_id: conversation.id.clone(),
        role,
        content,
        created_at: now_ms().to_string(),
        parent_id,
        model,
        receipt_id,
        prompt_tokens: None,
        completion_tokens: None,
        reasoning_content: None,
        reasoning_incomplete: false,
        branch_index: None,
        branch_count: None,
        attribution: None,
        attachment_ids: Vec::new(),
    });
    conversation.active_leaf_message_id = Some(message_id.clone());
    conversation.updated_at = now_ms().to_string();
    let path = upsert_conversation(db, conversation)?;
    Ok((message_id, path))
}

fn update_message_metrics(
    conversation_id: &str,
    message_id: &str,
    prompt_tokens: usize,
    completion_tokens: usize,
) -> Result<()> {
    let (db, mut conversation) = get_or_create_conversation(conversation_id)?;
    if let Some(message) = conversation
        .messages
        .iter_mut()
        .find(|message| message.id == message_id)
    {
        message.prompt_tokens = Some(prompt_tokens);
        message.completion_tokens = Some(completion_tokens);
        conversation.updated_at = now_ms().to_string();
        upsert_conversation(db, conversation)?;
    }
    Ok(())
}

fn utf8_prefix(value: &str, max_bytes: usize) -> (&str, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (&value[..end], true)
}

fn push_unique_path(paths: &mut Vec<String>, path: PathBuf) {
    let path = path.display().to_string();
    if !paths.contains(&path) {
        paths.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ToolDecision, ToolLoopState, parse_tool_decision, register_tool_loop, set_tool_loop_state,
        tool_loop_cancel_requested, tool_loop_controls, unregister_tool_loop_control, utf8_prefix,
        validate_tool_arguments,
    };
    use serde_json::json;
    use std::sync::atomic::Ordering;

    #[test]
    fn parses_only_the_bounded_call_shape_as_a_tool_request() {
        let decision = parse_tool_decision(
            "```json\n{\"action\":\"call\",\"arguments\":{\"city\":\"Boston\"}}\n```",
        );
        match decision {
            ToolDecision::Call { arguments } => {
                assert_eq!(arguments, json!({"city":"Boston"}));
            }
            ToolDecision::Final { .. } => panic!("expected a call decision"),
        }
    }

    #[test]
    fn treats_unstructured_model_text_as_a_terminal_answer() {
        let decision = parse_tool_decision("Here is the answer.");
        match decision {
            ToolDecision::Final { answer } => assert_eq!(answer, "Here is the answer."),
            ToolDecision::Call { .. } => panic!("unstructured text cannot authorize a tool call"),
        }
    }

    #[test]
    fn rejects_missing_and_wrong_typed_tool_arguments() {
        let schema = json!({
            "type":"object",
            "required":["city"],
            "properties":{"city":{"type":"string"}}
        });
        assert_eq!(
            validate_tool_arguments(&schema, &json!({}))
                .map(|blocker| blocker.code)
                .as_deref(),
            Some("tool_loop_required_argument_missing")
        );
        assert_eq!(
            validate_tool_arguments(&schema, &json!({"city": 3}))
                .map(|blocker| blocker.code)
                .as_deref(),
            Some("tool_loop_argument_type_invalid")
        );
    }

    #[test]
    fn tool_preview_truncation_preserves_utf8_boundaries() {
        let (preview, truncated) = utf8_prefix("abé", 3);
        assert_eq!(preview, "ab");
        assert!(truncated);
    }

    #[test]
    fn persisted_cancellation_reaches_the_live_tool_loop_control() {
        let data_dir = std::env::temp_dir().join(format!(
            "mom-llama-tool-loop-cancel-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&data_dir).expect("create isolated cancellation store");
        let request_id = uuid::Uuid::new_v4().to_string();
        let control =
            register_tool_loop(&data_dir, &request_id, "conversation").expect("register tool loop");
        assert!(
            !tool_loop_cancel_requested(&data_dir, &request_id, &control)
                .expect("read initial cancellation state")
        );

        set_tool_loop_state(&data_dir, &request_id, ToolLoopState::CancelRequested)
            .expect("persist cancellation request");
        assert!(
            tool_loop_cancel_requested(&data_dir, &request_id, &control)
                .expect("read persisted cancellation state")
        );
        assert!(control.cancel_requested.load(Ordering::Acquire));
        assert!(
            tool_loop_controls()
                .lock()
                .expect("read control registry")
                .contains_key(&request_id)
        );
        unregister_tool_loop_control(&request_id);
    }
}
