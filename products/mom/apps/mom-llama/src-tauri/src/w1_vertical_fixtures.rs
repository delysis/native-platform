use crate::app_runtime::w1_fixture_runtime;
use crate::command_registry::command_spec;
use crate::operation_supervisor::{
    LifecyclePhase, OperationSupervisor, TerminalClass, validate_worker_sets,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct ChatFixture {
    schema: String,
    conversation_id: String,
    cancelled_operation_id: String,
    retry_operation_id: String,
    cancelled_request_id: String,
    cancelled_attempt_id: String,
    retry_request_id: String,
    retry_attempt_id: String,
    message: String,
    initial_draft: String,
    assistant_text: String,
}

fn chat_fixture() -> Result<ChatFixture> {
    serde_json::from_str(include_str!(
        "../../../../crates/mom-llama-runtime/fixtures/w1/chat-cancel-retry-v1.json"
    ))
    .context("parse checked-in Mom chat fixture")
}

struct TestDataDir {
    _guard: MutexGuard<'static, ()>,
    path: PathBuf,
}

impl TestDataDir {
    fn new() -> Result<Self> {
        let guard = crate::APP_DATA_DIR_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = std::env::temp_dir().join(format!(
            "mom-w1-app-transcript-{}",
            mom_llama_runtime::now_ms()
        ));
        std::fs::create_dir_all(&path)?;
        mom_llama_runtime::config::set_data_dir_override_for_tests(Some(path.clone()));
        Ok(Self {
            _guard: guard,
            path,
        })
    }
}

impl Drop for TestDataDir {
    fn drop(&mut self) {
        mom_llama_runtime::config::set_data_dir_override_for_tests(None);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn app_admission_chat_cancel_native_control_and_retry_form_one_transcript() -> Result<()> {
    let session = TestDataDir::new()?;
    let fixture = chat_fixture()?;
    ensure!(fixture.schema == "mom_llama.w1.chat_cancel_retry_fixture.v1");
    mom_llama_runtime::draft_update(
        Some(&fixture.conversation_id),
        fixture.initial_draft.clone(),
        Vec::new(),
    )?;
    let runtime = w1_fixture_runtime();
    let cancelled_lease = runtime
        .admit(command_spec("mom_llama_chat_send"))
        .map_err(anyhow::Error::msg)?;
    let cancelled_identity = cancelled_lease
        .w1_attempt_identity()
        .context("cancelled admission attempt identity")?;
    ensure!(cancelled_identity.operation_id == fixture.cancelled_operation_id);
    ensure!(cancelled_identity.attempt_id == fixture.cancelled_attempt_id);

    let events = Arc::new(Mutex::new(Vec::new()));
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let cancelled_task = {
        let events = Arc::clone(&events);
        let conversation_id = fixture.conversation_id.clone();
        let message = fixture.message.clone();
        let request_id = fixture.cancelled_request_id.clone();
        tokio::spawn(async move {
            cancelled_lease
                .run_blocking_w1::<(), _>(move |operation_lease| {
                    let result = mom_llama_runtime::chat_send_stream_waiting_for_fixture_cancel(
                        mom_llama_runtime::ChatSendInput {
                            conversation_id,
                            message,
                        },
                        &request_id,
                        |event| {
                            if event.event == "started" {
                                started_tx
                                    .send(())
                                    .map_err(|_| anyhow::anyhow!("start observer disappeared"))?;
                            }
                            events
                                .lock()
                                .map_err(|_| anyhow::anyhow!("event log is unavailable"))?
                                .push(event);
                            Ok(())
                        },
                    )
                    .map_err(|error| error.to_string())?;
                    if result
                        .blocker
                        .as_ref()
                        .is_none_or(|blocker| blocker.code != "chat_cancelled")
                    {
                        return Err("fixture chat did not reach cancelled terminal".to_string());
                    }
                    operation_lease
                        .request_cancellation_from_executor()
                        .map_err(|error| error.to_string())?;
                    Err("cancelled fixture attempt".to_string())
                })
                .await
        })
    };
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .context("chat request did not reach started")?;
    let cancel_lease = runtime
        .admit(command_spec("mom_llama_chat_cancel"))
        .map_err(anyhow::Error::msg)?;
    let cancel = mom_llama_runtime::chat_cancel(&fixture.conversation_id)?
        .result
        .context("chat_cancel did not reach native cancellation")?;
    drop(cancel_lease);
    ensure!(cancel.request_id == fixture.cancelled_request_id);
    ensure!(cancelled_task.await?.is_err());
    ensure!(
        mom_llama_runtime::draft_get(Some(&fixture.conversation_id))?
            .result
            .context("draft after cancellation")?
            .message
            == fixture.initial_draft
    );

    let retry_lease = runtime
        .admit(command_spec("mom_llama_chat_send"))
        .map_err(anyhow::Error::msg)?;
    let retry_identity = retry_lease
        .w1_attempt_identity()
        .context("retry admission attempt identity")?;
    ensure!(retry_identity.operation_id == fixture.retry_operation_id);
    ensure!(retry_identity.attempt_id == fixture.retry_attempt_id);
    ensure!(retry_identity != cancelled_identity);
    let retry_events = Arc::new(Mutex::new(Vec::new()));
    let retry_request_id = fixture.retry_request_id.clone();
    let retry_conversation_id = fixture.conversation_id.clone();
    let retry_message = fixture.message.clone();
    let recorded_retry_events = Arc::clone(&retry_events);
    let retry = retry_lease
        .run_blocking_w1(move |_| {
            mom_llama_runtime::chat_send_stream_with_fixture_identity(
                mom_llama_runtime::ChatSendInput {
                    conversation_id: retry_conversation_id,
                    message: retry_message,
                },
                &retry_request_id,
                |event| {
                    recorded_retry_events
                        .lock()
                        .map_err(|_| anyhow::anyhow!("retry event log is unavailable"))?
                        .push(event);
                    Ok(())
                },
            )
            .map_err(|error| error.to_string())
        })
        .await
        .map_err(anyhow::Error::msg)?
        .result
        .context("retry chat output")?;
    ensure!(retry.request_id == fixture.retry_request_id);
    ensure!(retry.assistant_text == fixture.assistant_text);
    ensure!(runtime.w1_active_operation_count() == 0);
    ensure!(runtime.w1_retained_task_count() == 0);
    let terminal_facts = runtime.w1_terminal_facts();
    ensure!(terminal_facts.len() == 2);
    ensure!(terminal_facts[0].identity == cancelled_identity);
    ensure!(terminal_facts[0].terminal.class == TerminalClass::Cancelled);
    ensure!(terminal_facts[1].identity == retry_identity);
    ensure!(terminal_facts[1].terminal.class == TerminalClass::Completed);
    ensure!(
        events
            .lock()
            .map_err(|_| anyhow::anyhow!("event log is unavailable"))?
            .iter()
            .map(|event| event.event.as_str())
            .collect::<Vec<_>>()
            == ["started", "cancelled"]
    );

    mom_llama_runtime::config::set_data_dir_override_for_tests(None);
    mom_llama_runtime::config::set_data_dir_override_for_tests(Some(session.path.clone()));
    let reopened = mom_llama_runtime::conversation_list()?
        .result
        .context("reopened conversation list")?
        .into_iter()
        .find(|conversation| conversation.id == fixture.conversation_id)
        .context("reopened fixture conversation")?;
    ensure!(reopened.messages.len() == 2);
    ensure!(reopened.messages[0].content == fixture.message);
    ensure!(reopened.messages[1].content == fixture.assistant_text);

    use platform_contracts_v0_vertical::TerminalClass as ContractTerminalClass;
    use platform_vertical_fixtures_v0::{
        DurableStateFactV0, EquivalenceProjectionV0, EventFactV0, FactValueV0, LifecycleFactV0,
        OwnershipFactsV0, StateDispositionV0, VerticalIdV0, sha256_identity,
    };
    let cancelled_stream = events
        .lock()
        .map_err(|_| anyhow::anyhow!("event log is unavailable"))?
        .iter()
        .map(|event| event.event.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let retry_stream = retry_events
        .lock()
        .map_err(|_| anyhow::anyhow!("retry event log is unavailable"))?
        .iter()
        .map(|event| event.event.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let correlation_id = Some(fixture.conversation_id.clone());
    let projection = EquivalenceProjectionV0 {
        ordered_events: vec![
            EventFactV0 {
                sequence: 0,
                operation_id: cancelled_identity.operation_id.clone(),
                attempt_id: Some(cancelled_identity.attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                kind: "cancelled".to_owned(),
                payload: Some(sha256_identity(
                    "chat.cancelled.events",
                    cancelled_stream.as_bytes(),
                )),
            },
            EventFactV0 {
                sequence: 1,
                operation_id: retry_identity.operation_id.clone(),
                attempt_id: Some(retry_identity.attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                kind: "completed".to_owned(),
                payload: Some(sha256_identity(
                    "chat.retry.assistant_text",
                    fixture.assistant_text.as_bytes(),
                )),
            },
        ],
        durable_state: vec![DurableStateFactV0 {
            state_id: "mom.chat.store".to_owned(),
            schema_id: "runtime.sqlite3/encrypted_documents.v1".to_owned(),
            before: Some(sha256_identity(
                "chat.initial_draft",
                fixture.initial_draft.as_bytes(),
            )),
            after: Some(sha256_identity(
                "chat.retry.assistant_text",
                fixture.assistant_text.as_bytes(),
            )),
            disposition: StateDispositionV0::Updated,
        }],
        lifecycle: terminal_facts
            .iter()
            .map(|fact| LifecycleFactV0 {
                operation_id: fact.identity.operation_id.clone(),
                attempt_id: Some(fact.identity.attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                terminal: match fact.terminal.class {
                    TerminalClass::Completed => ContractTerminalClass::Completed,
                    TerminalClass::Cancelled => ContractTerminalClass::Cancelled,
                    TerminalClass::Failed => ContractTerminalClass::Failed,
                },
                released: true,
            })
            .collect(),
        ownership: OwnershipFactsV0 {
            active_operations: runtime.w1_active_operation_count(),
            retained_tasks: runtime.w1_retained_task_count(),
            expected_workers: terminal_facts.len(),
            joined_workers: terminal_facts.len(),
        },
        output_facts: BTreeMap::from([
            ("active_chat_requests".to_owned(), FactValueV0::Integer(0)),
            (
                "cancelled_attempt_retained".to_owned(),
                FactValueV0::Boolean(terminal_facts[0].terminal.class == TerminalClass::Cancelled),
            ),
            (
                "cancelled_stream".to_owned(),
                FactValueV0::Text(cancelled_stream),
            ),
            (
                "draft_preserved_after_cancel".to_owned(),
                FactValueV0::Boolean(true),
            ),
            (
                "reopened_message_count".to_owned(),
                FactValueV0::Integer(reopened.messages.len() as i64),
            ),
            (
                "retry_attempt_distinct".to_owned(),
                FactValueV0::Boolean(retry_identity != cancelled_identity),
            ),
            ("retry_stream".to_owned(), FactValueV0::Text(retry_stream)),
        ]),
        fail_closed_facts: vec![
            "cancelled attempt committed no conversation messages".to_owned(),
            "cancel command reached the registered native request before cancellation completed"
                .to_owned(),
        ],
    };
    mom_llama_runtime::validate_w1_fixture_projection(
        VerticalIdV0::MomChatCancelRetry,
        projection,
    )?;
    Ok(())
}

#[test]
fn quit_drains_fake_owner_and_a_fresh_supervisor_relaunches_cleanly() -> Result<()> {
    let supervisor = OperationSupervisor::with_config(81, 4);
    let owner = supervisor.spawn_controlled("mom-w1-quit-active")?;
    supervisor.publish_controlled_progress(&owner, 1)?;
    supervisor.begin_quiesce();
    ensure!(supervisor.phase() == LifecyclePhase::Quiescing);
    ensure!(supervisor.cancellation_requested_by_id("mom-w1-quit-active"));
    supervisor.request_controlled_terminal(&owner, TerminalClass::Cancelled)?;
    let released = supervisor.wait_controlled_released(&owner, Duration::from_secs(2))?;
    ensure!(
        released
            .authoritative_terminal
            .is_some_and(|terminal| terminal.class == TerminalClass::Cancelled)
    );
    supervisor.allow_controlled_exit(&owner)?;
    let outcome = supervisor.shutdown();
    ensure!(outcome.phase == LifecyclePhase::Closed);
    ensure!(outcome.active_operations == 0);
    ensure!(outcome.retained_tasks == 0);
    ensure!(validate_worker_sets(&outcome));

    let relaunched = OperationSupervisor::with_config(82, 4);
    ensure!(relaunched.phase() == LifecyclePhase::Running);
    ensure!(relaunched.active_count() == 0);
    let outcome = relaunched.shutdown();
    ensure!(outcome.phase == LifecyclePhase::Closed);
    ensure!(validate_worker_sets(&outcome));
    Ok(())
}
