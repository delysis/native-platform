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

use llama_native_types::{GenerationOutput, GenerationState, ModelFingerprint};
use loom_backend_llama::{
    IndependentRawBatch, IndependentRawRequest, LocalModelProfile, NativeHostRuntime, SamplingConfig,
};
use loom_cabal::compute::{
    ClientRequest, ComputeBatchFuture, ComputeBatchJob, ComputeBatchOutput, ComputeCancellation,
    ComputeClient, ComputeExecutor, ComputeFailure, ComputeFuture, ComputeGrant, ComputeHost,
    ComputeInput, ComputeModel, ComputePromptFormat, ComputeReply, ComputeStatus, HostComputeJob,
    RemoteJobReceipt,
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
    // Only complete JSONL records are observations during a concurrent append.
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1);
    bytes.truncate(complete);
    Ok(String::from_utf8(bytes)?)
}

struct Scenario {
    host_network: Network,
    peers: [Network; 2],
    host: Arc<ComputeHost>,
    grants: [ComputeGrant; 2],
    inputs: [ComputeInput; 2],
    case_ids: [String; 2],
    job: Uuid,
    output: PathBuf,
    trace_path: PathBuf,
    profile: LocalModelProfile,
    fingerprint: ModelFingerprint,
}

impl Scenario {
    async fn new(
        runtime: Arc<NativeHostRuntime>,
        profile: LocalModelProfile,
        fingerprint: ModelFingerprint,
        output: PathBuf,
        trace_path: PathBuf,
    ) -> Outcome<Self> {
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
        let peers = [
            Network::start(&first_identity, NetworkMode::Direct {}).await?,
            Network::start(&second_identity, NetworkMode::Direct {}).await?,
        ];
        let host = host_network.host_compute(
            &output.join("host-ledger"),
            Arc::new(NativeProbe {
                runtime,
                profile: profile.clone(),
                claim: claim.clone(),
                output: output.clone(),
            }),
        )?;
        let grants = [first_identity.public_key(), second_identity.public_key()].map(|peer| {
            ComputeGrant {
                id: Uuid::new_v4(),
                cabal: cabal_id,
                epoch,
                peer,
                model: claim.clone(),
                max_output_tokens: 1024,
                max_seconds: 120,
                jobs: 1,
            }
        });
        for grant in &grants {
            host.grant(grant.clone())?;
        }
        // Collision is intentional: two authenticated request owners, one UUID.
        let job = Uuid::new_v4();
        let case_ids = [
            format!("{}-{job}", first_identity.public_key()),
            format!("{}-{job}", second_identity.public_key()),
        ];
        let mut inputs = [input(), input()];
        inputs[1].seed = 43;
        Ok(Self {
            host_network,
            peers,
            host,
            grants,
            inputs,
            case_ids,
            job,
            output,
            trace_path,
            profile,
            fingerprint,
        })
    }

    fn prepare_clients(&self) -> Outcome<[ComputeClient; 2]> {
        let mut clients = [
            ComputeClient::open(&self.output.join("first"), self.grants[0].peer)?,
            ComputeClient::open(&self.output.join("second"), self.grants[1].peer)?,
        ];
        for ((client, grant), input) in clients.iter_mut().zip(&self.grants).zip(&self.inputs) {
            client.prepare(ClientRequest {
                id: self.job,
                host: self.host_network.address().id,
                grant: grant.clone(),
                input: input.clone(),
            })?;
        }
        Ok(clients)
    }

    async fn submit(&self, clients: &mut [ComputeClient; 2]) -> Outcome<()> {
        // Establish both authenticated paths before measuring collection. A cold
        // handshake is not evidence against the native batching mechanism.
        let (first, second) = tokio::join!(
            self.peers[0].compute_offers(self.host_network.address(), self.grants[0].cabal),
            self.peers[1].compute_offers(self.host_network.address(), self.grants[1].cabal),
        );
        for (reply, expected) in [first?, second?].into_iter().zip(&self.grants) {
            if !matches!(reply, ComputeReply::Offers { grants } if grants.contains(expected)) {
                return Err("qualification model grant was not offered to its requester".into());
            }
        }
        let (first, second) = tokio::join!(
            self.peers[0].compute_submit(
                self.host_network.address(), self.job, self.grants[0].id, self.inputs[0].clone(),
            ),
            self.peers[1].compute_submit(
                self.host_network.address(), self.job, self.grants[1].id, self.inputs[1].clone(),
            ),
        );
        clients[0].record(receipt(first?)?)?;
        clients[1].record(receipt(second?)?)?;
        Ok(())
    }

    async fn shared_decode(&self) -> Outcome<String> {
        tokio::time::timeout(Duration::from_secs(110), async {
            loop {
                if let Some(id) = trace::find_shared(
                    &read_trace(&self.trace_path)?,
                    &self.case_ids[0],
                    &self.case_ids[1],
                )? {
                    return Ok(id);
                }
                for peer in &self.peers {
                    let value = receipt(peer.compute_status(self.host_network.address(), self.job).await?)?;
                    if value.payload.status.is_terminal() {
                        return Err("a job ended without an observed shared native decode".into());
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?
    }

    async fn cancel_and_finish(
        &self,
        clients: &mut [ComputeClient; 2],
    ) -> Outcome<[RemoteJobReceipt; 2]> {
        clients[0].request_cancel(self.job)?;
        clients[0].record(receipt(
            self.peers[0]
                .compute_cancel(
                    self.host_network.address(),
                    self.job,
                    self.grants[0].id,
                    self.inputs[0].clone(),
                )
                .await?,
        )?)?;
        let outcomes = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let first = receipt(self.peers[0].compute_status(self.host_network.address(), self.job).await?)?;
                let second = receipt(self.peers[1].compute_status(self.host_network.address(), self.job).await?)?;
                if first.payload.status.is_terminal() && second.payload.status.is_terminal() {
                    return Ok::<_, Box<dyn Error + Send + Sync>>([first, second]);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        for (client, outcome) in clients.iter_mut().zip(&outcomes) {
            client.record(outcome.clone())?;
        }
        Ok(outcomes)
    }

    fn check_native(&self, batch_id: &str, receipts: &[RemoteJobReceipt; 2]) -> Outcome<trace::Summary> {
        if receipts[0].payload.status
            != (ComputeStatus::Cancelled {
                reason: ComputeCancellation::Requested,
            })
        {
            return Err("first requester did not retain its own cancellation outcome".into());
        }
        let ComputeStatus::Completed { text } = &receipts[1].payload.status else {
            return Err("cancelling the first requester prevented the second from completing".into());
        };
        let native: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.output.join("native-batch.json"))?)?;
        let outputs: Vec<GenerationOutput> =
            serde_json::from_value(native["native_outputs"].clone())?;
        let first = outputs
            .iter()
            .find(|item| item.branch_id == self.case_ids[0])
            .ok_or("cancelled native case disappeared")?;
        let second = outputs
            .iter()
            .find(|item| item.branch_id == self.case_ids[1])
            .ok_or("surviving native case disappeared")?;
        if outputs.len() != 2
            || first.request_id != batch_id
            || second.request_id != batch_id
            || first.state != GenerationState::Cancelled
            || second.state != GenerationState::Completed
            || second.text != *text
            || second.generated_token_ids.len() < 2
            || second.generated_token_ids.len() > self.inputs[1].max_output_tokens as usize
            || second.generated_token_ids.len() != second.metrics.completion_tokens
            || native["model_fingerprint"] != serde_json::to_value(&self.fingerprint)?
        {
            return Err("native observations and signed peer outcomes disagree".into());
        }
        Ok(trace::verify(
            &read_trace(&self.trace_path)?,
            batch_id,
            &self.case_ids[0],
            &self.case_ids[1],
        )?)
    }

    async fn check_retries(&self, receipts: &[RemoteJobReceipt; 2]) -> Outcome<()> {
        for (index, expected) in receipts.iter().enumerate() {
            let retried = receipt(
                self.peers[index]
                    .compute_submit(
                        self.host_network.address(),
                        self.job,
                        self.grants[index].id,
                        self.inputs[index].clone(),
                    )
                    .await?,
            )?;
            if retried.hash()? != expected.hash()? {
                return Err("retry changed a signed terminal receipt".into());
            }
        }
        if self.host.grant_statuses()?.iter().any(|status| status.jobs_remaining != 0) {
            return Err("retry restored a spent allowance".into());
        }
        Ok(())
    }

    async fn run(&self) -> Outcome<serde_json::Value> {
        let mut clients = self.prepare_clients()?;
        self.submit(&mut clients).await?;
        let batch_id = self.shared_decode().await?;
        let outcomes = self.cancel_and_finish(&mut clients).await?;
        let summary = self.check_native(&batch_id, &outcomes)?;
        self.check_retries(&outcomes).await?;
        drop(clients);
        let reopened = ComputeClient::open_existing(&self.output.join("first"), self.grants[0].peer)?;
        let recovered = reopened.get(self.job)?.ok_or("requester lost its saved job")?;
        if !recovered.cancel_requested
            || recovered.receipt.ok_or("requester lost its terminal")?.hash()? != outcomes[0].hash()?
        {
            return Err("requester reopen changed cancellation or its signed outcome".into());
        }
        Ok(serde_json::json!({
            "kind": "loom_peer_native_component_qualification_v1",
            "source_sha": std::env::var("LOOM_QUALIFICATION_SOURCE_SHA")?,
            "instrumented_build": true,
            "physical_networks": false,
            "packaged_tauri_adapter": false,
            "continuous_admission": false,
            "shared_decode_observed": true,
            "model_fingerprint": self.fingerprint,
            "profile": self.profile,
            "trace_summary": summary,
            "cancelled_receipt": outcomes[0],
            "completed_receipt": outcomes[1],
        }))
    }

    async fn shutdown(&self) -> Outcome<()> {
        let host = self.host_network.shutdown().await;
        let first = self.peers[0].shutdown().await;
        let second = self.peers[1].shutdown().await;
        host?;
        first?;
        second?;
        Ok(())
    }
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
    let scenario = Scenario::new(
        runtime.clone(),
        profile.clone(),
        fingerprint,
        output.clone(),
        trace_path,
    )
    .await?;
    let result = scenario.run().await;
    let same_worker = if result.is_ok() {
        runtime
            .acquire_research_handle(&profile)
            .map(|after| before.is_same_worker(&after))
    } else {
        Ok(false)
    };
    // Drain on failure too. No failed observation authorizes abandoned native
    // execution or a passing qualification receipt.
    let network_closed = scenario.shutdown().await;
    let native_closed = runtime.shutdown_joined();
    network_closed?;
    let _joined = native_closed?;
    let mut evidence = result?;
    if !same_worker? {
        return Err("the model was reloaded between independent requests".into());
    }
    evidence["same_resident_worker"] = serde_json::json!(true);
    save(
        &output.join("qualification.json"),
        &serde_json::to_vec_pretty(&evidence)?,
    )?;
    println!("native component qualification passed; see {}", output.display());
    Ok(())
}
