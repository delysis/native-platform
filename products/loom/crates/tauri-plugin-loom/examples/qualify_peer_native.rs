//! Diagnostic integration of cabal grants, local QUIC and the production raw
//! batch runtime. Run through scripts/qualify-peer-native.mjs for decode tracing.
//! This does not qualify the packaged Tauri adapter or two physical networks.
#[path = "qualification/trace.rs"]
mod trace;

use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use llama_native_types::{GenerationOutput, GenerationState};
use loom_backend_llama::{
    IndependentRawBatch, IndependentRawRequest, LocalModelProfile, NativeHostRuntime, SamplingConfig,
};
use loom_cabal::compute::{
    ClientRequest, ComputeBatchFuture, ComputeBatchJob, ComputeBatchOutput, ComputeCancellation,
    ComputeClient, ComputeExecutor, ComputeFailure, ComputeFuture, ComputeGrant, ComputeInput,
    ComputeModel, ComputePromptFormat, ComputeReply, ComputeStatus, HostComputeJob, RemoteJobReceipt,
};
use loom_cabal::{Cabal, Identity, Network, NetworkMode};
use loom_types::BlobId;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type Outcome<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Clone, Debug)]
struct NativeProbe {
    runtime: Arc<NativeHostRuntime>,
    profile: LocalModelProfile,
    claim: ComputeModel,
    output: PathBuf,
}

impl ComputeExecutor for NativeProbe {
    fn available(&self, model: &ComputeModel) -> bool {
        *model == self.claim
    }

    fn batch_limit(&self) -> usize {
        4
    }

    fn batch_compatible(&self, first: &HostComputeJob, next: &HostComputeJob) -> bool {
        first.grant.model == self.claim
            && next.grant.model == self.claim
            && first.grant.cabal == next.grant.cabal
            && first.grant.epoch == next.grant.epoch
            && first.input.prompt == next.input.prompt
            && first.input.media.is_empty()
            && next.input.media.is_empty()
            && first.input.format == ComputePromptFormat::Raw
            && next.input.format == ComputePromptFormat::Raw
    }

    fn execute(&self, job: HostComputeJob, cancel: CancellationToken) -> ComputeFuture {
        let work = self.execute_batch(vec![ComputeBatchJob { job, cancel }]);
        Box::pin(async move {
            work.await
                .into_iter()
                .next()
                .ok_or(ComputeFailure::InvalidOutput)?
                .result
        })
    }

    fn execute_batch(&self, jobs: Vec<ComputeBatchJob>) -> ComputeBatchFuture {
        let probe = self.clone();
        let keys = jobs
            .iter()
            .map(|item| (item.job.peer, item.job.id))
            .collect::<Vec<_>>();
        Box::pin(async move {
            let result = tokio::task::spawn_blocking(move || probe.run(&jobs)).await;
            match result {
                Ok(Ok(outputs)) => outputs,
                failed => {
                    eprintln!("native qualification executor failed: {failed:?}");
                    keys.into_iter()
                        .map(|(peer, job)| ComputeBatchOutput {
                            peer,
                            job,
                            result: Err(ComputeFailure::ExecutionFailed),
                        })
                        .collect()
                }
            }
        })
    }
}

impl NativeProbe {
    fn run(&self, jobs: &[ComputeBatchJob]) -> Outcome<Vec<ComputeBatchOutput>> {
        let requests = jobs
            .iter()
            .map(|item| {
                let id = format!("{}-{}", item.job.peer, item.job.id);
                IndependentRawRequest {
                    request_id: id.clone(),
                    case_id: id,
                    prompt: item.job.input.prompt.clone(),
                    sampling: SamplingConfig {
                        seed: item.job.input.seed,
                        max_tokens: item.job.input.max_output_tokens,
                        ..SamplingConfig::default()
                    },
                }
            })
            .collect::<Vec<_>>();
        let ids = requests
            .iter()
            .map(|request| request.request_id.clone())
            .collect::<Vec<_>>();
        let mut batch = IndependentRawBatch::start(&self.runtime, &self.profile, requests)?;
        let completed = loop {
            for (job, id) in jobs.iter().zip(&ids) {
                if job.cancel.is_cancelled() {
                    let _ = batch.cancel_request(id);
                }
            }
            if let Some(completed) = batch.try_complete()? {
                break completed;
            }
            let _ = batch.receive_event_timeout(Duration::from_millis(5));
        };
        save(
            &self.output.join("native-batch.json"),
            &completed.receipt_bytes()?,
        )?;
        if completed.outputs().len() != jobs.len() {
            return Err("native output count changed".into());
        }
        Ok(jobs
            .iter()
            .zip(completed.outputs())
            .map(|(item, output)| ComputeBatchOutput {
                peer: item.job.peer,
                job: item.job.id,
                result: Ok(output.text.clone()),
            })
            .collect())
    }
}

fn input() -> ComputeInput {
    ComputeInput {
        prompt: "Continue this story at length, without concluding it.\nThe cartographer opened a second map. Beyond the last inked coast, she found".into(),
        format: ComputePromptFormat::Raw,
        max_output_tokens: 1024,
        seed: 42,
        media: Vec::new(),
    }
}

fn receipt(reply: ComputeReply) -> Outcome<RemoteJobReceipt> {
    match reply {
        ComputeReply::Receipt { receipt } => {
            receipt.verify()?;
            Ok(*receipt)
        }
        other => Err(format!("expected signed receipt, received {other:?}").into()),
    }
}

fn save(path: &Path, bytes: &[u8]) -> Outcome<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn read_trace(path: &Path) -> Outcome<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("native trace limit exceeded".into());
    }
    // The parent may be in the middle of appending its next JSONL record.
    // Only complete records are observations; never parse a partial last line.
    bytes.truncate(bytes.iter().rposition(|byte| *byte == b'\n').map_or(0, |at| at + 1));
    Ok(String::from_utf8(bytes)?)
}

#[tokio::main]
async fn main() -> Outcome<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: qualify_peer_native <model.gguf> <new-receipt-directory>".into());
    }
    let trace_path = PathBuf::from(
        std::env::var_os("LOOM_NATIVE_BATCH_TRACE_PATH")
            .ok_or("run through scripts/qualify-peer-native.mjs; missing native trace path")?,
    );
    let output = PathBuf::from(&args[1]);
    let profile = LocalModelProfile::for_gguf(PathBuf::from(&args[0]));
    let runtime = Arc::new(NativeHostRuntime::default());
    let before = runtime.acquire_research_handle(&profile)?;
    let fingerprint = before
        .status()
        .fingerprint
        .ok_or("no live model fingerprint")?;
    let claim = ComputeModel {
        fingerprint: BlobId::digest(&serde_json::to_vec(&(&profile, &fingerprint))?).to_string(),
        name: "Native batch qualification".into(),
        media: Vec::new(),
    };
    let host_identity = Identity::generate()?;
    let first_identity = Identity::generate()?;
    let second_identity = Identity::generate()?;
    let mut cabal = Cabal::create(
        &output.join("cabal.db"),
        host_identity.clone(),
        "Qualification",
        "Host",
    )?;
    for (identity, name) in [(&first_identity, "First"), (&second_identity, "Second")] {
        let invitation = cabal.invite(host_identity.public_key().into())?;
        cabal.admit(&invitation.token, identity.public_key(), name)?;
    }
    let cabal_id = cabal.id();
    let epoch = cabal.roster().payload.epoch;
    let host_network = Network::start(&host_identity, NetworkMode::Direct {}).await?;
    host_network.add(Arc::new(Mutex::new(cabal)))?;
    let first = Network::start(&first_identity, NetworkMode::Direct {}).await?;
    let second = Network::start(&second_identity, NetworkMode::Direct {}).await?;
    let host = host_network.host_compute(
        &output.join("host-ledger"),
        Arc::new(NativeProbe {
            runtime: runtime.clone(),
            profile: profile.clone(),
            claim: claim.clone(),
            output: output.clone(),
        }),
    )?;
    let grant = |peer| ComputeGrant {
        id: Uuid::new_v4(),
        cabal: cabal_id,
        epoch,
        peer,
        model: claim.clone(),
        max_output_tokens: 1024,
        max_seconds: 120,
        jobs: 1,
    };
    let first_grant = grant(first_identity.public_key());
    let second_grant = grant(second_identity.public_key());
    host.grant(first_grant.clone())?;
    host.grant(second_grant.clone())?;
    // Collision is intentional: two authenticated request owners, one UUID.
    let job = Uuid::new_v4();
    let first_case = format!("{}-{job}", first_identity.public_key());
    let second_case = format!("{}-{job}", second_identity.public_key());
    let first_input = input();
    let mut second_input = input();
    second_input.seed = 43;
    let result = async {
        let mut first_client = ComputeClient::open(&output.join("first"), first_identity.public_key())?;
        let mut second_client = ComputeClient::open(&output.join("second"), second_identity.public_key())?;
        for (client, grant, input) in [
            (&mut first_client, &first_grant, &first_input),
            (&mut second_client, &second_grant, &second_input),
        ] {
            client.prepare(ClientRequest {
                id: job,
                host: host_identity.public_key(),
                grant: grant.clone(),
                input: input.clone(),
            })?;
        }
        let (a, b) = tokio::join!(
            first.compute_submit(host_network.address(), job, first_grant.id, first_input.clone()),
            second.compute_submit(host_network.address(), job, second_grant.id, second_input.clone()),
        );
        first_client.record(receipt(a?)?)?;
        second_client.record(receipt(b?)?)?;
        let batch_id = tokio::time::timeout(Duration::from_secs(110), async {
            loop {
                if let Some(id) = trace::find_shared(
                    &read_trace(&trace_path)?, &first_case, &second_case,
                )? {
                    return Ok::<_, Box<dyn Error + Send + Sync>>(id);
                }
                let a = receipt(first.compute_status(host_network.address(), job).await?)?;
                let b = receipt(second.compute_status(host_network.address(), job).await?)?;
                if a.payload.status.is_terminal() || b.payload.status.is_terminal() {
                    return Err("a job ended without an observed shared native decode".into());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await??;
        first_client.request_cancel(job)?;
        first_client.record(receipt(first.compute_cancel(
            host_network.address(), job, first_grant.id, first_input.clone(),
        ).await?)?)?;
        let (cancelled, completed) = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let a = receipt(first.compute_status(host_network.address(), job).await?)?;
                let b = receipt(second.compute_status(host_network.address(), job).await?)?;
                if a.payload.status.is_terminal() && b.payload.status.is_terminal() {
                    return Ok::<_, Box<dyn Error + Send + Sync>>((a, b));
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await??;
        first_client.record(cancelled.clone())?;
        second_client.record(completed.clone())?;
        if cancelled.payload.status != (ComputeStatus::Cancelled {
            reason: ComputeCancellation::Requested,
        }) {
            return Err("first requester did not retain its own cancellation outcome".into());
        }
        let ComputeStatus::Completed { text } = &completed.payload.status else {
            return Err("cancelling the first requester prevented the second from completing".into());
        };
        let native: serde_json::Value = serde_json::from_slice(
            &std::fs::read(output.join("native-batch.json"))?,
        )?;
        let outputs: Vec<GenerationOutput> =
            serde_json::from_value(native["native_outputs"].clone())?;
        let a = outputs.iter().find(|item| item.branch_id == first_case)
            .ok_or("cancelled native case disappeared")?;
        let b = outputs.iter().find(|item| item.branch_id == second_case)
            .ok_or("surviving native case disappeared")?;
        if outputs.len() != 2
            || a.request_id != batch_id || b.request_id != batch_id
            || a.state != GenerationState::Cancelled || b.state != GenerationState::Completed
            || b.text != *text || b.generated_token_ids.len() < 2
            || native["model_fingerprint"] != serde_json::to_value(&fingerprint)?
        {
            return Err("native observations and signed peer outcomes disagree".into());
        }
        let summary = trace::verify(&read_trace(&trace_path)?, &batch_id, &first_case, &second_case)?;
        let a = receipt(first.compute_submit(
            host_network.address(), job, first_grant.id, first_input.clone(),
        ).await?)?;
        let b = receipt(second.compute_submit(
            host_network.address(), job, second_grant.id, second_input.clone(),
        ).await?)?;
        if a.hash()? != cancelled.hash()? || b.hash()? != completed.hash()?
            || host.grant_statuses()?.iter().any(|status| status.jobs_remaining != 0)
        {
            return Err("retry changed a terminal receipt or restored a spent allowance".into());
        }
        drop(first_client);
        let reopened = ComputeClient::open_existing(&output.join("first"), first_identity.public_key())?;
        let recovered = reopened.get(job)?.ok_or("requester lost its saved job")?;
        if !recovered.cancel_requested
            || recovered.receipt.ok_or("requester lost its terminal")?.hash()? != cancelled.hash()?
        {
            return Err("requester reopen changed cancellation or its signed outcome".into());
        }
        let after = runtime.acquire_research_handle(&profile)?;
        if !before.is_same_worker(&after) {
            return Err("the model was reloaded between independent requests".into());
        }
        Ok::<_, Box<dyn Error + Send + Sync>>(serde_json::json!({
            "kind": "loom_peer_native_component_qualification_v1",
            "source_sha": std::env::var("LOOM_QUALIFICATION_SOURCE_SHA")?,
            "instrumented_build": true,
            "physical_networks": false,
            "packaged_tauri_adapter": false,
            "continuous_admission": false,
            "shared_decode_observed": true,
            "same_resident_worker": true,
            "model_fingerprint": fingerprint,
            "profile": profile,
            "trace_summary": summary,
            "cancelled_receipt": cancelled,
            "completed_receipt": completed,
        }))
    }.await;
    // Drain even when qualification failed. No error path is permission to
    // leave native execution or authenticated listeners running.
    let host_closed = host_network.shutdown().await;
    let first_closed = first.shutdown().await;
    let second_closed = second.shutdown().await;
    let native_closed = runtime.shutdown_joined();
    host_closed?;
    first_closed?;
    second_closed?;
    let _joined = native_closed?;
    let evidence = result?;
    save(
        &output.join("qualification.json"),
        &serde_json::to_vec_pretty(&evidence)?,
    )?;
    println!("native component qualification passed; see {}", output.display());
    Ok(())
}
