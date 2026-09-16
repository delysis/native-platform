//! Opt-in real-model gate. No fixture executor stands in for NativeExecutor.
//! The runner instruments successful native decodes and the original-to-case
//! mapping in an isolated build. A mock app is not packaged-app acceptance.
use super::*;
use loom_cabal::compute::{
    ClientRequest, ComputeCancellation, ComputeClient, ComputeGrant, ComputeHost, ComputeInput,
    ComputeReply, ComputeStatus, RemoteJobReceipt,
};
use loom_cabal::{Cabal, Identity, Network, NetworkMode};
use serde::Deserialize;
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use tauri::Manager;
use uuid::Uuid;

#[path = "../examples/qualification/trace.rs"]
mod trace;

type Outcome<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    kind: String,
    request_id: String,
    members: Vec<Member>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    request_id: String,
    case_id: String,
}

struct Selection {
    batch: String,
    cases: [String; 2],
}

struct Peers {
    network: Network,
    callers: [Network; 2],
    host: Arc<ComputeHost>,
    grants: [ComputeGrant; 2],
    inputs: [ComputeInput; 2],
    job: Uuid,
    output: PathBuf,
    decode_path: PathBuf,
    mapping_path: PathBuf,
}

impl Peers {
    async fn new(state: &PluginState, model: &LoadedModel, output: &Path) -> Outcome<Self> {
        let claim = model_claim(model).map_err(|failure| format!("model claim: {failure:?}"))?;
        let executor = Arc::new(NativeExecutor::from_state(state));
        if executor.batch_limit() < 2 {
            return Err("the selected production model does not admit two cases".into());
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while !executor.available(&claim) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        let owner = Identity::generate()?;
        let first = Identity::generate()?;
        let second = Identity::generate()?;
        let mut cabal = Cabal::create(&output.join("cabal.db"), owner.clone(), "Batch gate", "Host")?;
        for (peer, name) in [(&first, "First requester"), (&second, "Second requester")] {
            let invitation = cabal.invite(owner.public_key().into())?;
            cabal.admit(&invitation.token, peer.public_key(), name)?;
        }
        let cabal_id = cabal.id();
        let epoch = cabal.roster().payload.epoch;
        let network = Network::start(&owner, NetworkMode::Direct {}).await?;
        network.add(Arc::new(Mutex::new(cabal)))?;
        let callers = [
            Network::start(&first, NetworkMode::Direct {}).await?,
            Network::start(&second, NetworkMode::Direct {}).await?,
        ];
        let host = network.host_compute(&output.join("host-ledger"), executor)?;
        let grants = [first.public_key(), second.public_key()].map(|peer| ComputeGrant {
            id: Uuid::new_v4(),
            cabal: cabal_id,
            epoch,
            peer,
            model: claim.clone(),
            max_output_tokens: 1024,
            max_seconds: 120,
            jobs: 1,
        });
        for grant in &grants {
            host.grant(grant.clone())?;
        }
        let input = ComputeInput {
            prompt: "Continue this story at length, without concluding it.\nThe cartographer opened a second map. Beyond the last inked coast, she found".into(),
            format: ComputePromptFormat::Raw,
            max_output_tokens: 1024,
            seed: 42,
            media: Vec::new(),
        };
        let mut inputs = [input.clone(), input];
        inputs[1].seed = 43;
        Ok(Self {
            network,
            callers,
            host,
            grants,
            inputs,
            // The actual identity is (authenticated requester, job), not UUID alone.
            job: Uuid::new_v4(),
            output: output.to_path_buf(),
            decode_path: required_path("LOOM_NATIVE_BATCH_TRACE_PATH")?,
            mapping_path: required_path("LOOM_NATIVE_BATCH_MAPPING_PATH")?,
        })
    }

    fn clients(&self) -> Outcome<[ComputeClient; 2]> {
        let mut clients = [
            ComputeClient::open(&self.output.join("first"), self.grants[0].peer)?,
            ComputeClient::open(&self.output.join("second"), self.grants[1].peer)?,
        ];
        for ((client, grant), input) in clients.iter_mut().zip(&self.grants).zip(&self.inputs) {
            client.prepare(ClientRequest {
                id: self.job,
                host: self.network.address().id,
                grant: grant.clone(),
                input: input.clone(),
            })?;
        }
        Ok(clients)
    }

    async fn submit(&self, clients: &mut [ComputeClient; 2]) -> Outcome<()> {
        // Warm both authenticated paths before the short collection window.
        let (first, second) = tokio::join!(
            self.callers[0].compute_offers(self.network.address(), self.grants[0].cabal),
            self.callers[1].compute_offers(self.network.address(), self.grants[1].cabal),
        );
        for (reply, expected) in [first?, second?].into_iter().zip(&self.grants) {
            if !matches!(reply, ComputeReply::Offers { grants } if grants.contains(expected)) {
                return Err("production executor did not offer the exact reviewed model".into());
            }
        }
        let (first, second) = tokio::join!(
            self.callers[0].compute_submit(
                self.network.address(), self.job, self.grants[0].id, self.inputs[0].clone(),
            ),
            self.callers[1].compute_submit(
                self.network.address(), self.job, self.grants[1].id, self.inputs[1].clone(),
            ),
        );
        clients[0].record(receipt(first?)?)?;
        clients[1].record(receipt(second?)?)?;
        Ok(())
    }

    fn selection(&self) -> Outcome<Option<Selection>> {
        let expected = self.grants.each_ref().map(|grant| {
            serde_json::to_vec(&(grant.peer, self.job))
                .map(|bytes| format!("peer-{}", BlobId::digest(&bytes)))
        });
        let [first, second] = expected;
        let expected = [first?, second?];
        for line in complete_lines(&self.mapping_path, 64 * 1024)?.lines() {
            let mapping: Mapping = serde_json::from_str(line)?;
            if mapping.kind != "loom_native_batch_mapping_v1"
                || mapping.request_id.is_empty()
                || mapping.members.is_empty()
                || mapping.members.len() > MAX_COMPUTE_BATCH_JOBS
                || mapping.members.iter().map(|item| &item.request_id).collect::<BTreeSet<_>>().len()
                    != mapping.members.len()
                || mapping.members.iter().map(|item| &item.case_id).collect::<BTreeSet<_>>().len()
                    != mapping.members.len()
            {
                return Err("malformed native admission mapping".into());
            }
            if mapping.members.len() != 2 {
                continue;
            }
            let cases = expected.each_ref().map(|id| {
                mapping.members.iter().find(|member| member.request_id == *id)
                    .map(|member| member.case_id.clone())
            });
            let [Some(first), Some(second)] = cases else {
                continue;
            };
            let shared = trace::find_shared(
                &complete_lines(&self.decode_path, 8 * 1024 * 1024)?, &first, &second,
            )?;
            if let Some(batch) = shared {
                if batch != mapping.request_id {
                    return Err("native decode and production admission identities differ".into());
                }
                return Ok(Some(Selection { batch, cases: [first, second] }));
            }
        }
        Ok(None)
    }

    async fn wait_shared(&self) -> Outcome<Selection> {
        tokio::time::timeout(Duration::from_secs(110), async {
            loop {
                if let Some(selection) = self.selection()? {
                    return Ok(selection);
                }
                for caller in &self.callers {
                    let value = receipt(caller.compute_status(self.network.address(), self.job).await?)?;
                    if value.payload.status.is_terminal() {
                        return Err("a peer job ended before shared native decoding was observed".into());
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?
    }

    async fn cancel_and_finish(&self, clients: &mut [ComputeClient; 2]) -> Outcome<[RemoteJobReceipt; 2]> {
        clients[0].request_cancel(self.job)?;
        clients[0].record(receipt(self.callers[0].compute_cancel(
            self.network.address(), self.job, self.grants[0].id, self.inputs[0].clone(),
        ).await?)?)?;
        let outcomes = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let first = receipt(self.callers[0].compute_status(self.network.address(), self.job).await?)?;
                let second = receipt(self.callers[1].compute_status(self.network.address(), self.job).await?)?;
                if first.payload.status.is_terminal() && second.payload.status.is_terminal() {
                    return Ok::<_, Box<dyn Error + Send + Sync>>([first, second]);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await??;
        for (client, outcome) in clients.iter_mut().zip(&outcomes) {
            client.record(outcome.clone())?;
        }
        Ok(outcomes)
    }

    fn verify_native(
        &self,
        selection: &Selection,
        outcomes: &[RemoteJobReceipt; 2],
        fingerprint: &serde_json::Value,
    ) -> Outcome<trace::Summary> {
        if outcomes[0].payload.status != (ComputeStatus::Cancelled {
            reason: ComputeCancellation::Requested,
        }) {
            return Err("the first requester did not retain its cancellation outcome".into());
        }
        let ComputeStatus::Completed { text } = &outcomes[1].payload.status else {
            return Err("the independently owned sibling did not complete".into());
        };
        let writing = self.output.join("device/peer-compute-writing");
        let store = ProjectStore::open(&writing)?;
        let result = store.read_document(format!("Results/{}/{}.md", self.grants[1].peer, self.job))?;
        if result.text != *text || writing.join(format!("Results/{}/{}.md", self.grants[0].peer, self.job)).exists() {
            return Err("production result retention disagrees with cancellation or signed output".into());
        }
        let metadata: String = store.connection().query_row(
            "SELECT metadata_json FROM artifacts WHERE artifact_id = ?1",
            [result.artifact_id.to_string()], |row| row.get(0),
        )?;
        let metadata: serde_json::Value = serde_json::from_str(&metadata)?;
        let evidence: BlobId = serde_json::from_value(metadata["provenance_blob_id"].clone())?;
        let evidence: serde_json::Value = serde_json::from_str(&store.read_blob_text(evidence)?)?;
        let native_blob: BlobId = serde_json::from_value(evidence["native_execution_blob"].clone())?;
        let native: serde_json::Value = serde_json::from_str(&store.read_blob_text(native_blob)?)?;
        let outputs: Vec<llama_native_types::GenerationOutput> = serde_json::from_value(native["native_outputs"].clone())?;
        let first = outputs.iter().find(|output| output.branch_id == selection.cases[0])
            .ok_or("cancelled native case is missing")?;
        let second = outputs.iter().find(|output| output.branch_id == selection.cases[1])
            .ok_or("surviving native case is missing")?;
        if outputs.len() != 2
            || first.request_id != selection.batch || second.request_id != selection.batch
            || first.state != llama_native_types::GenerationState::Cancelled
            || second.state != llama_native_types::GenerationState::Completed
            || second.text != *text || second.generated_token_ids.len() < 2
            || second.generated_token_ids.len() > self.inputs[1].max_output_tokens as usize
            || second.generated_token_ids.len() != second.metrics.completion_tokens
            || native["model_fingerprint"] != *fingerprint
            || native["evidence_class"] != "operational_native"
            || evidence["kind"] != "loom_peer_batched_result_v1"
        {
            return Err("actual native provenance and signed peer outcomes disagree".into());
        }
        let mapping_blob: BlobId = serde_json::from_value(evidence["batch_mapping_blob"].clone())?;
        let mapping: serde_json::Value = serde_json::from_str(&store.read_blob_text(mapping_blob)?)?;
        let members = mapping["members"].as_array().ok_or("missing durable batch mapping")?;
        if members.len() != 2 {
            return Err("durable batch mapping lost an owner".into());
        }
        for (index, grant) in self.grants.iter().enumerate() {
            let member = members.iter().find(|item| item["peer"] == serde_json::json!(grant.peer))
                .ok_or("missing original requester in batch mapping")?;
            let generation: GenerationStart = serde_json::from_value(member["generation"].clone())?;
            let source = store.read_document(format!("Requests/{}/{}.md", grant.peer, self.job))?;
            if generation.document_id != source.document_id
                || generation.source_revision_id != source.revision_id
                || generation.branch_id.to_string() != selection.cases[index]
                || member["job"] != serde_json::json!(self.job)
                || member["request_fingerprint"] != self.inputs[index].fingerprint(grant.id)?
            {
                return Err("the native batch borrowed another request's source or identity".into());
            }
        }
        Ok(trace::verify(
            &complete_lines(&self.decode_path, 8 * 1024 * 1024)?,
            &selection.batch, &selection.cases[0], &selection.cases[1],
        )?)
    }

    async fn run(&self, fingerprint: &serde_json::Value) -> Outcome<serde_json::Value> {
        let mut clients = self.clients()?;
        self.submit(&mut clients).await?;
        let selection = self.wait_shared().await?;
        let outcomes = self.cancel_and_finish(&mut clients).await?;
        let trace = self.verify_native(&selection, &outcomes, fingerprint)?;
        for (index, expected) in outcomes.iter().enumerate() {
            let retry = receipt(self.callers[index].compute_submit(
                self.network.address(), self.job, self.grants[index].id, self.inputs[index].clone(),
            ).await?)?;
            if retry.hash()? != expected.hash()? {
                return Err("an exact retry changed a terminal result".into());
            }
        }
        if self.host.grant_statuses()?.iter().any(|grant| grant.jobs_remaining != 0) {
            return Err("an exact retry restored a spent allowance".into());
        }
        drop(clients);
        let reopened = ComputeClient::open_existing(&self.output.join("first"), self.grants[0].peer)?;
        let recovered = reopened.get(self.job)?.ok_or("requester lost its durable job")?;
        if !recovered.cancel_requested
            || recovered.receipt.ok_or("missing requester receipt")?.hash()? != outcomes[0].hash()?
        {
            return Err("requester reopen changed cancellation or its result".into());
        }
        Ok(serde_json::json!({
            "kind": "loom_peer_native_qualification_v1",
            "source_sha": std::env::var("LOOM_QUALIFICATION_SOURCE_SHA")?,
            "production_peer_executor": true,
            "instrumented_build": true,
            "packaged_app": false,
            "physical_networks": false,
            "continuous_admission": false,
            "shared_decode_observed": true,
            "model_fingerprint": fingerprint,
            "target_os": std::env::consts::OS,
            "target_arch": std::env::consts::ARCH,
            "trace_summary": trace,
            "cancelled_receipt": outcomes[0],
            "completed_receipt": outcomes[1],
        }))
    }

    async fn shutdown(&self) -> Outcome<()> {
        let host = self.network.shutdown().await;
        let first = self.callers[0].shutdown().await;
        let second = self.callers[1].shutdown().await;
        host?;
        first?;
        second?;
        Ok(())
    }
}

fn receipt(reply: ComputeReply) -> Outcome<RemoteJobReceipt> {
    match reply {
        ComputeReply::Receipt { receipt } => {
            receipt.verify()?;
            Ok(*receipt)
        }
        other => Err(format!("expected a signed receipt, received {other:?}").into()),
    }
}

fn required_path(name: &str) -> Outcome<PathBuf> {
    Ok(PathBuf::from(std::env::var_os(name).ok_or_else(|| {
        format!("{name} is required; run scripts/qualify-peer-native.mjs")
    })?))
}

fn complete_lines(path: &Path, limit: u64) -> Outcome<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("native qualification observation exceeded its limit".into());
    }
    let complete = bytes.iter().rposition(|byte| *byte == b'\n').map_or(0, |at| at + 1);
    bytes.truncate(complete);
    Ok(String::from_utf8(bytes)?)
}

#[test]
#[ignore = "requires a real GGUF and instrumented native decode observations; run scripts/qualify-peer-native.mjs"]
fn independent_peers_share_native_decode() -> Outcome<()> {
    let model_path = required_path("MOM_LLAMA_MODEL_PATH")?;
    let output = required_path("LOOM_NATIVE_BATCH_OUTPUT_PATH")?;
    let mut writing = ProjectStore::initialize(output.join("writing"), "Unchanged source")?.0;
    let original = writing.write_document("draft.md", "The original manuscript.\n")?;
    let device = output.join("device");
    let app = tauri::test::mock_app();
    app.manage(PluginState::with_app_local_data_root(
        Some(device.clone()), true, BuildModelPolicy::default(),
    ));
    tauri::async_runtime::block_on(crate::model_load(
        app.handle().clone(), app.state::<PluginState>(), ModelLoadRequest {
            path: model_path.to_string_lossy().into_owned(),
            projector_path: None, projector_kind: None,
        },
    )).map_err(|error| format!("load qualification model: {error:?}"))?;
    let state = app.state::<PluginState>();
    let model = loaded_model_for_state(&state).map_err(|error| format!("loaded model: {error:?}"))?;
    let before = state.native_runtime.acquire_research_handle(&model.profile)?;
    let fingerprint = serde_json::to_value(before.status().fingerprint.ok_or("missing native fingerprint")?)?;
    let result = tauri::async_runtime::block_on(async {
        let peers = Peers::new(&state, &model, &output).await?;
        let result = peers.run(&fingerprint).await;
        let closed = peers.shutdown().await;
        closed?;
        result
    });
    let same_worker = if result.is_ok() {
        state.native_runtime.acquire_research_handle(&model.profile)
            .map(|after| before.is_same_worker(&after))
    } else {
        Ok(false)
    };
    drop(before);
    // Cleanup precedes every final observation or passing receipt.
    let unloaded = tauri::async_runtime::block_on(crate::model_unload(
        app.handle().clone(), app.state::<PluginState>(),
    )).map_err(|error| format!("unload qualification model: {error:?}"));
    let closed = state.native_runtime.shutdown_joined();
    let _joined = closed?;
    let unloaded = unloaded?;
    let mut evidence = result?;
    if !same_worker? || !unloaded.resident_slot_released || !state.peer_compute.idle() {
        return Err("native model ownership or final drain did not qualify".into());
    }
    let unchanged = writing.read_document("draft.md")?;
    if unchanged.revision_id != original.revision_id || unchanged.text != original.text {
        return Err("peer inference modified the original manuscript".into());
    }
    evidence["same_resident_worker"] = serde_json::json!(true);
    evidence["original_manuscript_unchanged"] = serde_json::json!(true);
    evidence["native_model_released"] = serde_json::json!(true);
    evidence["model_profile"] = serde_json::to_value(&model.profile)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut receipt = options.open(output.join("qualification.json"))?;
    receipt.write_all(&serde_json::to_vec_pretty(&evidence)?)?;
    receipt.sync_all()?;
    Ok(())
}
