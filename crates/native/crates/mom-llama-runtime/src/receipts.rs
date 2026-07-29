use crate::{RECEIPT_SCHEMA, RESULT_SCHEMA, now_ms};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Blocker {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub next_actions: Vec<String>,
}

impl Blocker {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        next_actions: Vec<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            next_actions,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandReceipt {
    pub schema: String,
    pub command: String,
    pub task_id: String,
    pub role: String,
    pub status: String,
    pub readiness: String,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub tests_run: Vec<String>,
    #[serde(default)]
    pub artifacts_produced: Vec<String>,
    #[serde(default)]
    pub readiness_changes: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<Blocker>,
    #[serde(default)]
    pub reuse_decisions: Vec<String>,
    #[serde(default)]
    pub next_actions: Vec<String>,
    pub real_engine_invoked: bool,
    pub fake_fixture: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandResult<T>
where
    T: Serialize,
{
    pub schema: String,
    pub command: String,
    pub status: String,
    pub readiness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<Blocker>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    pub receipt: CommandReceipt,
}

impl<T> CommandResult<T>
where
    T: Serialize,
{
    pub fn passed(
        command: &str,
        readiness: &str,
        result: T,
        changed_paths: Vec<String>,
        artifacts: Vec<String>,
        real_engine_invoked: bool,
        fake_fixture: bool,
    ) -> Self {
        Self {
            schema: RESULT_SCHEMA.to_string(),
            command: command.to_string(),
            status: readiness.to_string(),
            readiness: readiness.to_string(),
            blocker: None,
            result: Some(result),
            receipt: receipt(ReceiptInput {
                command,
                status: readiness,
                readiness,
                changed_paths,
                artifacts,
                blockers: Vec::new(),
                real_engine_invoked,
                fake_fixture,
            }),
        }
    }

    pub fn blocked(command: &str, readiness: &str, blocker: Blocker) -> Self {
        Self::blocked_with_evidence(
            command,
            readiness,
            blocker,
            Vec::new(),
            Vec::new(),
            false,
            false,
        )
    }

    pub fn blocked_with_evidence(
        command: &str,
        readiness: &str,
        blocker: Blocker,
        changed_paths: Vec<String>,
        artifacts: Vec<String>,
        real_engine_invoked: bool,
        fake_fixture: bool,
    ) -> Self {
        let next_actions = blocker.next_actions.clone();
        Self {
            schema: RESULT_SCHEMA.to_string(),
            command: command.to_string(),
            status: "blocked".to_string(),
            readiness: readiness.to_string(),
            blocker: Some(blocker.clone()),
            result: None,
            receipt: CommandReceipt {
                schema: RECEIPT_SCHEMA.to_string(),
                command: command.to_string(),
                task_id: format!("{command}:{}", now_ms()),
                role: "mom_llama_runtime".to_string(),
                status: "blocked".to_string(),
                readiness: readiness.to_string(),
                changed_paths,
                tests_run: Vec::new(),
                artifacts_produced: artifacts,
                readiness_changes: Vec::new(),
                blockers: vec![blocker],
                reuse_decisions: vec![
                    "app-local Rust store and in-process llama.cpp owner-thread runtime"
                        .to_string(),
                ],
                next_actions,
                real_engine_invoked,
                fake_fixture,
                created_at: now_ms().to_string(),
            },
        }
    }

    pub fn to_json_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|error| {
            json!({
                "schema": RESULT_SCHEMA,
                "command": self.command,
                "status": "blocked",
                "readiness": "serialization_failed",
                "blocker": {"code": "result_serialization_failed", "message": error.to_string()}
            })
        })
    }
}

pub fn persist_command_receipt<T>(result: &T) -> anyhow::Result<()>
where
    T: Serialize,
{
    let value = serde_json::to_value(result)?;
    let Some(receipt) = value.get("receipt") else {
        return Ok(());
    };
    let task_id = receipt
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or("mom_llama.unknown");
    let command = receipt
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| value.get("command").and_then(Value::as_str))
        .unwrap_or("mom_llama.unknown");
    crate::store::RuntimeStore::current()?.write_receipt(task_id, command, receipt)
}

struct ReceiptInput<'a> {
    command: &'a str,
    status: &'a str,
    readiness: &'a str,
    changed_paths: Vec<String>,
    artifacts: Vec<String>,
    blockers: Vec<Blocker>,
    real_engine_invoked: bool,
    fake_fixture: bool,
}

fn receipt(input: ReceiptInput<'_>) -> CommandReceipt {
    CommandReceipt {
        schema: RECEIPT_SCHEMA.to_string(),
        command: input.command.to_string(),
        task_id: format!("{}:{}", input.command, now_ms()),
        role: "mom_llama_runtime".to_string(),
        status: input.status.to_string(),
        readiness: input.readiness.to_string(),
        changed_paths: input.changed_paths,
        tests_run: Vec::new(),
        artifacts_produced: input.artifacts,
        readiness_changes: vec![input.readiness.to_string()],
        blockers: input.blockers,
        reuse_decisions: vec![
            "app-local Rust store and in-process llama.cpp owner-thread runtime".to_string(),
        ],
        next_actions: Vec::new(),
        real_engine_invoked: input.real_engine_invoked,
        fake_fixture: input.fake_fixture,
        created_at: now_ms().to_string(),
    }
}
