use crate::app_runtime::{w1_fixture_runtime, w1_fixture_runtime_with_sequence};
use crate::command_registry::command_spec;
use crate::operation_supervisor::{
    LifecyclePhase, OperationSupervisor, TerminalClass, validate_worker_sets,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
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

#[derive(Debug, Deserialize)]
struct QuitRelaunchFixture {
    schema: String,
    conversation_id: String,
    active_operation_id: String,
    draft: String,
}

fn chat_fixture() -> Result<ChatFixture> {
    serde_json::from_str(include_str!(
        "../../../../crates/mom-llama-runtime/fixtures/w1/chat-cancel-retry-v1.json"
    ))
    .context("parse checked-in Mom chat fixture")
}

fn quit_relaunch_fixture() -> Result<QuitRelaunchFixture> {
    serde_json::from_str(include_str!(
        "../../../../crates/mom-llama-runtime/fixtures/w1/quit-relaunch-v1.json"
    ))
    .context("parse checked-in Mom quit/relaunch fixture")
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
                .run_blocking_with_cancellation_evidence(move || {
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
                    let authoritative_cancellation =
                        result.has_authoritative_cancellation_evidence();
                    Ok(((), authoritative_cancellation))
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
    cancelled_task.await?.map_err(anyhow::Error::msg)?;
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
        .run_blocking_with_cancellation_evidence(move || {
            let result = mom_llama_runtime::chat_send_stream_with_fixture_identity(
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
            .map_err(|error| error.to_string())?;
            let authoritative_cancellation = result.has_authoritative_cancellation_evidence();
            Ok((result, authoritative_cancellation))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_app_runtime_quit_joins_fake_owner_and_reopens_same_store() -> Result<()> {
    let session = TestDataDir::new()?;
    let fixture = quit_relaunch_fixture()?;
    ensure!(fixture.schema == "mom_llama.w1.quit_relaunch_fixture.v1");

    let runtime = w1_fixture_runtime();
    let write_lease = runtime
        .admit(command_spec("mom_llama_draft_update"))
        .map_err(anyhow::Error::msg)?;
    mom_llama_runtime::draft_update(
        Some(&fixture.conversation_id),
        fixture.draft.clone(),
        Vec::new(),
    )?;
    write_lease
        .finish(TerminalClass::Completed)
        .map_err(anyhow::Error::msg)?;

    let supervisor = runtime.w1_operation_supervisor();
    let active_lease = runtime
        .admit(command_spec("mom_llama_chat_send"))
        .map_err(anyhow::Error::msg)?;
    let cancelled_identity = active_lease
        .w1_attempt_identity()
        .context("active application operation identity")?;
    ensure!(cancelled_identity.operation_id == fixture.active_operation_id);
    let cancellation = active_lease
        .w1_cancellation_signal()
        .context("long application operation cancellation signal")?;
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (cancel_seen_tx, cancel_seen_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let active_task = tokio::spawn(async move {
        active_lease
            .run_blocking(move || {
                started_tx
                    .send(())
                    .map_err(|_| "fixture start observer disappeared".to_owned())?;
                while !cancellation.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                cancel_seen_tx
                    .send(())
                    .map_err(|_| "fixture cancellation observer disappeared".to_owned())?;
                release_rx
                    .recv()
                    .map_err(|_| "fixture release authority disappeared".to_owned())?;
                Err::<(), _>("fixture owner observed application cancellation".to_owned())
            })
            .await
    });
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .context("active application worker started")?;
    let shutdown = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.shutdown().await })
    };
    while supervisor.phase() == LifecyclePhase::Running {
        tokio::task::yield_now().await;
    }
    ensure!(supervisor.phase() == LifecyclePhase::Quiescing);
    ensure!(
        runtime.admit(command_spec("mom_llama_draft_get")).is_err(),
        "application admission must be closed before the owner exits"
    );
    ensure!(
        supervisor.cancellation_requested_by_id(&fixture.active_operation_id),
        "AppRuntime quit must publish cancellation before owner terminalization"
    );
    cancel_seen_rx
        .recv_timeout(Duration::from_secs(2))
        .context("application worker observed cancellation")?;
    release_tx
        .send(())
        .context("release cancelled application worker")?;
    ensure!(
        active_task
            .await
            .context("active application task")?
            .is_err(),
        "cancelled application worker must not report success"
    );
    let first_summary = shutdown
        .await
        .context("first AppRuntime shutdown task")?
        .map_err(anyhow::Error::msg)?;
    ensure!(first_summary.operation_supervisor_phase == LifecyclePhase::Closed);
    ensure!(first_summary.active_operation_count == 0);
    ensure!(first_summary.retained_operation_task_count == 0);
    ensure!(first_summary.application_work_drained);
    ensure!(first_summary.gateway_drained);
    ensure!(first_summary.native_host_joined);
    ensure!(first_summary.expected_worker_ids == first_summary.joined_worker_ids);
    ensure!(first_summary.expected_operation_worker_count == 1);
    ensure!(first_summary.joined_operation_worker_count == 1);
    ensure!(first_summary.expected_worker_ids == ["mom-operation-worker-41"]);
    let first_terminals = runtime.w1_terminal_facts();
    ensure!(first_terminals.len() == 1);
    ensure!(first_terminals[0].identity == cancelled_identity);
    ensure!(first_terminals[0].terminal.class == TerminalClass::Cancelled);

    mom_llama_runtime::config::set_data_dir_override_for_tests(None);
    mom_llama_runtime::config::set_data_dir_override_for_tests(Some(session.path.clone()));
    let relaunched = w1_fixture_runtime_with_sequence(42);
    let read_lease = relaunched
        .admit(command_spec("mom_llama_draft_get"))
        .map_err(anyhow::Error::msg)?;
    let reopened = mom_llama_runtime::draft_get(Some(&fixture.conversation_id))?
        .result
        .context("reopened draft")?;
    drop(read_lease);
    ensure!(reopened.message == fixture.draft);

    let completed_lease = relaunched
        .admit(command_spec("mom_llama_chat_send"))
        .map_err(anyhow::Error::msg)?;
    let completed_identity = completed_lease
        .w1_attempt_identity()
        .context("relaunched operation identity")?;
    completed_lease
        .run_blocking(|| Ok(()))
        .await
        .map_err(anyhow::Error::msg)?;
    let second_summary = relaunched.shutdown().await.map_err(anyhow::Error::msg)?;
    ensure!(second_summary.operation_supervisor_phase == LifecyclePhase::Closed);
    ensure!(second_summary.active_operation_count == 0);
    ensure!(second_summary.retained_operation_task_count == 0);
    ensure!(second_summary.application_work_drained);
    ensure!(second_summary.gateway_drained);
    ensure!(second_summary.native_host_joined);
    ensure!(second_summary.expected_worker_ids == second_summary.joined_worker_ids);
    ensure!(second_summary.expected_operation_worker_count == 1);
    ensure!(second_summary.joined_operation_worker_count == 1);
    ensure!(second_summary.expected_worker_ids == ["mom-operation-worker-42"]);
    let second_terminals = relaunched.w1_terminal_facts();
    ensure!(second_terminals.len() == 1);
    ensure!(second_terminals[0].identity == completed_identity);
    ensure!(second_terminals[0].terminal.class == TerminalClass::Completed);

    use platform_contracts_v0_vertical::TerminalClass as ContractTerminalClass;
    use platform_vertical_fixtures_v0::{
        DurableStateFactV0, EquivalenceProjectionV0, EventFactV0, FactValueV0, LifecycleFactV0,
        OwnershipFactsV0, StateDispositionV0, VerticalIdV0, sha256_identity,
    };
    let correlation_id = Some(fixture.conversation_id.clone());
    let projection = EquivalenceProjectionV0 {
        ordered_events: vec![
            EventFactV0 {
                sequence: 0,
                operation_id: cancelled_identity.operation_id.clone(),
                attempt_id: Some(cancelled_identity.attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                kind: "admission_closed".to_owned(),
                payload: None,
            },
            EventFactV0 {
                sequence: 1,
                operation_id: cancelled_identity.operation_id.clone(),
                attempt_id: Some(cancelled_identity.attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                kind: "cancellation_requested".to_owned(),
                payload: None,
            },
            EventFactV0 {
                sequence: 2,
                operation_id: cancelled_identity.operation_id.clone(),
                attempt_id: Some(cancelled_identity.attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                kind: "cancelled".to_owned(),
                payload: None,
            },
            EventFactV0 {
                sequence: 3,
                operation_id: cancelled_identity.operation_id.clone(),
                attempt_id: Some(cancelled_identity.attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                kind: "worker_joined".to_owned(),
                payload: None,
            },
            EventFactV0 {
                sequence: 4,
                operation_id: completed_identity.operation_id.clone(),
                attempt_id: Some(completed_identity.attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                kind: "completed".to_owned(),
                payload: None,
            },
            EventFactV0 {
                sequence: 5,
                operation_id: completed_identity.operation_id.clone(),
                attempt_id: Some(completed_identity.attempt_id.clone()),
                correlation_id: correlation_id.clone(),
                kind: "worker_joined".to_owned(),
                payload: None,
            },
        ],
        durable_state: vec![DurableStateFactV0 {
            state_id: "mom.drafts.store".to_owned(),
            schema_id: "runtime.sqlite3/encrypted_documents.v1".to_owned(),
            before: Some(sha256_identity(
                "mom.quit-relaunch.draft",
                fixture.draft.as_bytes(),
            )),
            after: Some(sha256_identity(
                "mom.quit-relaunch.draft",
                reopened.message.as_bytes(),
            )),
            disposition: StateDispositionV0::Unchanged,
        }],
        lifecycle: vec![
            LifecycleFactV0 {
                operation_id: cancelled_identity.operation_id,
                attempt_id: Some(cancelled_identity.attempt_id),
                correlation_id: correlation_id.clone(),
                terminal: ContractTerminalClass::Cancelled,
                released: true,
            },
            LifecycleFactV0 {
                operation_id: completed_identity.operation_id,
                attempt_id: Some(completed_identity.attempt_id),
                correlation_id,
                terminal: ContractTerminalClass::Completed,
                released: true,
            },
        ],
        ownership: OwnershipFactsV0 {
            active_operations: first_summary.active_operation_count
                + second_summary.active_operation_count,
            retained_tasks: first_summary.retained_operation_task_count
                + second_summary.retained_operation_task_count,
            expected_workers: first_summary.expected_worker_ids.len()
                + second_summary.expected_worker_ids.len(),
            joined_workers: first_summary.joined_worker_ids.len()
                + second_summary.joined_worker_ids.len(),
        },
        output_facts: BTreeMap::from([
            (
                "first_application_work_drained".to_owned(),
                FactValueV0::Boolean(first_summary.application_work_drained),
            ),
            (
                "first_gateway_drained".to_owned(),
                FactValueV0::Boolean(first_summary.gateway_drained),
            ),
            (
                "first_native_host_joined".to_owned(),
                FactValueV0::Boolean(first_summary.native_host_joined),
            ),
            (
                "first_worker_id".to_owned(),
                FactValueV0::Text(first_summary.expected_worker_ids[0].clone()),
            ),
            (
                "fresh_worker_id".to_owned(),
                FactValueV0::Text(second_summary.expected_worker_ids[0].clone()),
            ),
            (
                "fresh_runtime_admitted_work".to_owned(),
                FactValueV0::Boolean(
                    second_terminals[0].terminal.class == TerminalClass::Completed,
                ),
            ),
            (
                "same_durable_state_reopened".to_owned(),
                FactValueV0::Boolean(reopened.message == fixture.draft),
            ),
            (
                "worker_epochs_distinct".to_owned(),
                FactValueV0::Boolean(
                    first_summary.expected_worker_ids != second_summary.expected_worker_ids,
                ),
            ),
            (
                "zero_orphan_workers".to_owned(),
                FactValueV0::Boolean(
                    first_summary.expected_worker_ids == first_summary.joined_worker_ids
                        && second_summary.expected_worker_ids == second_summary.joined_worker_ids,
                ),
            ),
        ]),
        fail_closed_facts: vec![
            "quit closed admission before the active owner published its terminal".to_owned(),
            "fresh runtime did not reuse the closed supervisor lifetime".to_owned(),
        ],
    };
    mom_llama_runtime::validate_w1_fixture_projection(
        VerticalIdV0::QuitRelaunchFakeOwners,
        projection,
    )?;
    Ok(())
}
