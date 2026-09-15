//! A peer owns one native completion at a time. The requesting terminal owns the
//! expression, immutable context, deterministic job IDs, and explicit recovery.
use super::*;
use crate::cabals::requesting::{JobDelivery, JobReply, find_job};
use loom_cabal::compute::{
    ClientJob, ClientRequest, ComputeGrant, ComputeInput, ComputeModel, ComputeRejection,
    ComputeStatus,
};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PeerTarget {
    pub(crate) host: String,
    pub(crate) grant: ComputeGrant,
    pub(crate) roster_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct PeerModel {
    host: String,
    model: ComputeModel,
}

impl From<&PeerTarget> for PeerModel {
    fn from(target: &PeerTarget) -> Self {
        Self {
            host: target.host.clone(),
            model: target.grant.model.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryMode {
    Check,
    Resume,
}

fn job_id(run: &str, step: u32) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"loom_terminal_peer_step_v1\0");
    hash.update(run.as_bytes());
    hash.update(step.to_be_bytes());
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

pub(super) fn output_id(run: &str, step: u32) -> CommandId {
    let mut hash = Sha256::new();
    hash.update(b"loom_terminal_peer_output_v1\0");
    hash.update(run.as_bytes());
    hash.update(step.to_be_bytes());
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    CommandId::from_ulid(u128::from_be_bytes(bytes).into())
}

fn unconfirmed(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("terminal_remote_unconfirmed", message, true)
}

fn cancelled() -> IpcFailure {
    IpcFailure::new(
        "terminal_remote_cancelled",
        "The peer experiment was cancelled.",
        false,
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PeerTerminalRequest {
    project_id: String,
    session_id: String,
    command_id: String,
    document_id: String,
    source_revision_id: String,
    expected_visible_blob_id: String,
    source_start_byte: u64,
    source_end_byte: u64,
    expression: String,
    presentation: Option<TerminalPresentation>,
    context_references: Option<Vec<String>>,
    turn_boundary: Option<TerminalTurnBoundary>,
    literal_input: Option<bool>,
    remote_target: PeerTarget,
}

// A separate command keeps local-generation permission from gaining network
// authority. The selected grant is required; this command cannot fall back.
#[tauri::command]
pub(crate) async fn terminal_run_peer<R: Runtime>(
    request: PeerTerminalRequest,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<TerminalRun, IpcFailure> {
    terminal_start(
        &request.project_id,
        &request.session_id,
        &request.command_id,
        &request.document_id,
        &request.source_revision_id,
        &request.expected_visible_blob_id,
        request.source_start_byte,
        request.source_end_byte,
        &request.expression,
        request.presentation,
        request.context_references,
        request.turn_boundary,
        request.literal_input,
        Some(request.remote_target),
        app,
        &state,
    )
}

#[tauri::command]
pub(crate) async fn terminal_recover<R: Runtime>(
    project_id: String,
    session_id: String,
    run_id: String,
    mode: RecoveryMode,
    app: AppHandle<R>,
    state: State<'_, PluginState>,
) -> Result<TerminalRun, IpcFailure> {
    let admission = lock_application_admission(&state, "peer experiment recovery")?;
    let mut session = lock_session(&state)?;
    session
        .agency
        .admit_manual_generation()
        .map_err(io_failure)?;
    let store = require_bound_store(&mut session, &project_id, &session_id)?;
    if let Some(receipt) = read_receipt(store.root(), &run_id, true)? {
        return Ok(receipt.run);
    }
    let receipt = read_receipt(store.root(), &run_id, false)?
        .ok_or_else(|| failure("This experiment does not exist."))?;
    if receipt.remote.is_none() {
        return Err(failure("Only peer experiments have network recovery."));
    }
    let identity = GenerationFamilyIdentity {
        request_id: format!("terminal-{run_id}"),
        project_id: store.manifest().project_id,
        session_id: parse_command_id(&session_id)?,
        document_id: receipt.source_document_id,
    };
    if state
        .generation_lifecycle
        .current_lease(&identity.request_id)
        .map_err(io_failure)?
        .is_some()
    {
        return Ok(receipt.run);
    }
    let command = if receipt.literal_input {
        NeuralCommand::Prompt(bounded(receipt.run.expression.clone())?)
    } else {
        parse_neural_command(&receipt.run.expression).map_err(io_failure)?
    };
    let input = String::from_utf8(
        store
            .read_blob(receipt.input_blob_id)
            .map_err(IpcFailure::store)?,
    )
    .map_err(io_failure)?;
    let root = store.root().to_owned();
    if receipt.media.len() > loom_cabal::compute::MAX_COMPUTE_MEDIA_OBJECTS {
        return Err(failure("Saved peer media exceeds its object limit."));
    }
    let mut media_bytes = 0_usize;
    let media = receipt
        .media
        .iter()
        .map(|item| {
            let bytes = store
                .read_blob(item.bytes_blob_id)
                .map_err(IpcFailure::store)?;
            media_bytes = media_bytes
                .checked_add(bytes.len())
                .ok_or_else(|| failure("Saved peer media exceeds its byte limit."))?;
            if media_bytes > loom_cabal::compute::MAX_COMPUTE_MEDIA_BYTES {
                return Err(failure("Saved peer media exceeds its byte limit."));
            }
            Ok(llama_native_types::MediaInput {
                id: item.id.clone(),
                kind: item.kind,
                mime: item.mime.clone(),
                sha256: item.bytes_blob_id.to_string(),
                bytes,
            })
        })
        .collect::<Result<Vec<_>, IpcFailure>>()?;
    spawn_worker(
        app,
        &state,
        RunWork {
            root,
            identity,
            receipt,
            input,
            command,
            source: None,
            model: None,
            media,
            recovery: mode,
        },
        &admission,
        session,
    )
}

pub(super) async fn cancel_saved_jobs(
    project: &str,
    session: &str,
    receipt: &RunReceipt,
    state: State<'_, PluginState>,
) -> Result<(), IpcFailure> {
    for step in 1..=u32::try_from(MAX_CALLS).expect("bounded step count") {
        let id = job_id(&receipt.run.run_id, step);
        if find_job(project, session, id, &state).await?.is_some() {
            // This command records per-job intent before attempting delivery.
            compute_job_cancel(project.into(), session.into(), id, state.clone()).await?;
        }
    }
    Ok(())
}

impl Evaluator<'_> {
    pub(super) fn remote_cancel_requested(&self) -> Result<bool, IpcFailure> {
        Ok(self.control.cancelled.load(Ordering::Acquire)
            || crate::terminal_receipts::cancel_requested(self.root, &self.receipt.run.run_id)?)
    }

    pub(super) fn settle_remote_cancellation(&mut self) -> Result<String, IpcFailure> {
        let project = self.identity.project_id.to_string();
        let session = self.identity.session_id.to_string();
        let mut confirmed = true;
        for step in 1..=u32::try_from(MAX_CALLS).expect("bounded step count") {
            let id = job_id(&self.receipt.run.run_id, step);
            let Some(mut job) =
                tauri::async_runtime::block_on(find_job(&project, &session, id, &self.state))?
            else {
                continue;
            };
            if !terminal(&job) {
                let reply = tauri::async_runtime::block_on(async {
                    match self.recovery {
                        RecoveryMode::Check => {
                            compute_job_check(
                                project.clone(),
                                session.clone(),
                                id,
                                self.state.clone(),
                            )
                            .await
                        }
                        RecoveryMode::Resume => {
                            compute_job_cancel(
                                project.clone(),
                                session.clone(),
                                id,
                                self.state.clone(),
                            )
                            .await
                        }
                    }
                })?;
                job = reply.job;
            }
            confirmed &= terminal(&job);
            if let Some(receipt) = &job.receipt
                && let ComputeStatus::Completed { text } = &receipt.payload.status
            {
                self.step = step;
                self.retain_remote(&job, text)?;
            }
        }
        if confirmed {
            Err(cancelled())
        } else {
            Err(unconfirmed(
                "Cancellation is saved; the friend's final outcome is still unconfirmed.",
            ))
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn complete_remote(&mut self, prompt: String) -> Result<String, IpcFailure> {
        if self.remote_cancel_requested()? {
            return self.settle_remote_cancellation();
        }
        self.step += 1;
        let target = self
            .receipt
            .remote
            .as_ref()
            .expect("remote evaluator")
            .clone();
        let request = ClientRequest {
            id: job_id(&self.receipt.run.run_id, self.step),
            host: target.host.parse().map_err(io_failure)?,
            grant: target.grant.clone(),
            input: ComputeInput {
                media: crate::peer_media::encode(&self.media, &target.grant.model)?,
                prompt,
                max_output_tokens: 512.min(target.grant.max_output_tokens),
                seed: terminal_sampling(
                    parse_command_id(&self.receipt.run.run_id)?,
                    self.step,
                    None,
                )
                .seed,
            },
        };
        let project = self.identity.project_id.to_string();
        let session = self.identity.session_id.to_string();
        let saved =
            tauri::async_runtime::block_on(find_job(&project, &session, request.id, &self.state))?;
        let mut job = match saved {
            Some(job) if job.request == request => job,
            Some(_) => {
                return Err(failure(
                    "The saved peer step has different inputs. No work was submitted.",
                ));
            }
            None if self.recovery == RecoveryMode::Check => {
                return Err(unconfirmed(
                    "Saved steps are checked. Resume to continue this experiment.",
                ));
            }
            None => tauri::async_runtime::block_on(compute_job_prepare(
                project.clone(),
                session.clone(),
                request.clone(),
                target.roster_hash,
                self.state.clone(),
            ))?,
        };
        let deadline = std::time::Instant::now()
            + Duration::from_secs(u64::from(target.grant.max_seconds) + 10);
        while !terminal(&job) {
            if self.remote_cancel_requested()? {
                return self.settle_remote_cancellation();
            }
            let reply = tauri::async_runtime::block_on(async {
                if self.recovery == RecoveryMode::Resume && job.receipt.is_none() {
                    compute_job_submit(
                        project.clone(),
                        session.clone(),
                        request.id,
                        self.state.clone(),
                    )
                    .await
                } else {
                    compute_job_check(
                        project.clone(),
                        session.clone(),
                        request.id,
                        self.state.clone(),
                    )
                    .await
                }
            })?;
            job = checked_reply(reply)?;
            if terminal(&job) {
                break;
            }
            if self.recovery == RecoveryMode::Check || std::time::Instant::now() >= deadline {
                return Err(unconfirmed(
                    "The friend has not returned a final result. Check again when connected.",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let status = &job
            .receipt
            .as_ref()
            .expect("terminal receipt")
            .payload
            .status;
        match status {
            ComputeStatus::Completed { text } => {
                self.retain_remote(&job, text)?;
                if self.remote_cancel_requested()? {
                    return self.settle_remote_cancellation();
                }
                Ok(text.clone())
            }
            ComputeStatus::Cancelled { .. } => Err(cancelled()),
            _ => Err(IpcFailure::new(
                "terminal_remote_failed",
                format!("The peer stopped this step: {status:?}"),
                false,
            )),
        }
    }

    fn retain_remote(&mut self, job: &ClientJob, text: &str) -> Result<(), IpcFailure> {
        let evidence = self.with_store(|store| {
            store
                .store_provenance_blob(
                    &serde_json::to_vec(&serde_json::json!({
                            "kind": "loom_terminal_peer_step_v1",
                            "run_id": self.receipt.run.run_id,
                            "step": self.step,
                            "source_revision_id": self.receipt.source_revision_id,
                            "sources": self.receipt.sources,
                            "request": job.request,
                    "receipt": job.receipt,
                        }))
                    .map_err(io_failure)?,
                )
                .map_err(IpcFailure::store)
        })?;
        self.receipt.steps.push(evidence);
        self.retain(text, evidence)
    }
}

fn terminal(job: &ClientJob) -> bool {
    job.receipt
        .as_ref()
        .is_some_and(|receipt| receipt.payload.status.is_terminal())
}

fn checked_reply(reply: JobReply) -> Result<ClientJob, IpcFailure> {
    if terminal(&reply.job) {
        return Ok(reply.job);
    }
    match reply.delivery {
        JobDelivery::Rejected { reason } => Err(unconfirmed(match reason {
            ComputeRejection::Denied => {
                "The friend refused this request: access denied. Check the compute grant with them."
            }
            ComputeRejection::Busy => "The friend's model is busy. Resume when it is available.",
            ComputeRejection::InvalidRequest => "The friend refused this input or its limits.",
            ComputeRejection::MismatchedRetry => {
                "The friend reported different saved input for this job. Its input cannot be replaced."
            }
            ComputeRejection::Exhausted => {
                "The friend's compute grant or storage budget is exhausted."
            }
            ComputeRejection::Stopped => "The friend's compute service has stopped.",
            ComputeRejection::Unavailable => {
                "The friend could not return a saved result for this job."
            }
        })),
        JobDelivery::Unconfirmed { message } => Err(unconfirmed(message)),
        JobDelivery::Stored | JobDelivery::Receipt => Ok(reply.job),
    }
}

#[cfg(all(test, unix))]
#[path = "terminal_remote_tests.rs"]
mod tests;
