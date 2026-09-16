//! The first peer batch class is deliberately narrow: identical raw text, no
//! media, one cabal epoch and exact model. Neither membership nor prefix overlap
//! grants permission to reuse another requester's unknown private context.
use super::*;
use loom_backend_llama::{IndependentRawBatch, IndependentRawRequest};
use loom_cabal::compute::ComputePromptFormat;

pub(super) fn compatible(first: &HostComputeJob, next: &HostComputeJob) -> bool {
    first.grant.cabal == next.grant.cabal
        && first.grant.epoch == next.grant.epoch
        && first.grant.model == next.grant.model
        && first.input.format == ComputePromptFormat::Raw
        && next.input.format == ComputePromptFormat::Raw
        && first.input.media.is_empty()
        && next.input.media.is_empty()
        && !first.input.prompt.is_empty()
        && first.input.prompt == next.input.prompt
        && first.input.seed != u32::MAX
        && next.input.seed != u32::MAX
}

pub(super) fn execute(executor: NativeExecutor, jobs: Vec<ComputeBatchJob>) -> ComputeBatchFuture {
    Box::pin(async move {
        let identities = jobs.iter().map(|item| (item.job.peer, item.job.id)).collect::<Vec<_>>();
        let rejected = |failure| identities.iter().map(|&(peer, job)| ComputeBatchOutput {
            peer,
            job,
            result: Err(failure),
        }).collect::<Vec<_>>();
        let Some(first) = jobs.first() else {
            return Vec::new();
        };
        let unique = identities.iter().copied().collect::<BTreeSet<_>>();
        if jobs.len() < 2 || jobs.len() > executor.batch_limit()
            || unique.len() != jobs.len()
            || jobs.iter().any(|item| !compatible(&first.job, &item.job))
        {
            return rejected(ComputeFailure::InputUnsupported);
        }
        // This lease is not a child of any request token. Individual request
        // cancellation must never preempt siblings; foreground work cancels it.
        let lease = match executor.owner.reserve(&CancellationToken::new()) {
            Ok(lease) => lease,
            Err(failure) => return rejected(failure),
        };
        match tokio::task::spawn_blocking(move || run(&executor, &jobs, &lease)).await {
            Ok(Ok(outcomes)) => outcomes,
            Ok(Err(failure)) => rejected(failure),
            Err(_) => rejected(ComputeFailure::WorkerPanicked),
        }
    })
}

fn native_requests(prepared: &[Prepared]) -> Vec<IndependentRawRequest> {
    prepared.iter().map(|item| IndependentRawRequest {
        request_id: item.request_id.clone(),
        case_id: item.request.cases[0].generation.branch_id.to_string(),
        prompt: item.request.exact_manuscript_prefix.clone(),
        sampling: item.request.cases[0].sampling.clone(),
    }).collect()
}

fn run(
    executor: &NativeExecutor,
    jobs: &[ComputeBatchJob],
    lease: &IdleJob,
) -> Result<Vec<ComputeBatchOutput>, ComputeFailure> {
    let root = executor.root.as_ref().ok_or(ComputeFailure::ModelUnavailable)?;
    let admission = executor.application.try_lock().map_err(|_| ComputeFailure::HostBusy)?;
    if *admission != ApplicationPhase::Running
        || executor.close_requested.load(Ordering::Acquire)
        || lease.cancel.is_cancelled()
        || executor.generations.active_local_branch_count().map_err(failed)? != 0
    {
        return Err(ComputeFailure::HostBusy);
    }
    let lifecycle = executor.model_lifecycle.try_lock().map_err(|_| ComputeFailure::HostBusy)?;
    let model = executor.selected()?;
    let claim = model_claim(&model)?;
    if jobs.len() > model.profile.max_parallel_cases as usize
        || jobs.len() > model.descriptor.capabilities.max_cases as usize
        || jobs.iter().any(|item| item.job.grant.model != claim)
    {
        return Err(ComputeFailure::ModelUnavailable);
    }
    let mut store = private_store(root)?;
    let prepared = jobs.iter().map(|item| prepare(&mut store, &model, &item.job))
        .collect::<Result<Vec<_>, _>>()?;
    let mapping = jobs.iter().zip(&prepared).enumerate().map(|(index, (item, source))| {
        Ok(serde_json::json!({
            "peer": item.job.peer,
            "job": item.job.id,
            "grant": item.job.grant.id,
            "request_fingerprint": item.job.input.fingerprint(item.job.grant.id).map_err(failed)?,
            "original_request_id": source.request_id,
            "native_input_index": index,
            "prompt_blob": source.prompt_blob,
            "generation": source.request.cases[0].generation,
        }))
    }).collect::<Result<Vec<_>, ComputeFailure>>()?;
    // Freeze all original source identities before native dispatch. Do not
    // copy the first request's document/authority artifacts onto its siblings.
    let mapping_bytes = serde_json::to_vec(&serde_json::json!({
        "kind": "loom_peer_batch_mapping_v1",
        "members": mapping,
    })).map_err(failed)?;
    let mapping_blob = store.store_provenance_blob(&mapping_bytes).map_err(failed)?;
    if lease.cancel.is_cancelled() || jobs.iter().all(|item| item.cancel.is_cancelled()) {
        return Err(ComputeFailure::HostBusy);
    }
    let mut batch = IndependentRawBatch::start(
        &executor.native_runtime,
        &model.profile,
        native_requests(&prepared),
    ).map_err(|_| ComputeFailure::ExecutionFailed)?;
    drop(lifecycle);
    drop(admission);
    let completion = loop {
        if lease.cancel.is_cancelled() {
            batch.cancel_all();
        }
        for (item, source) in jobs.iter().zip(&prepared) {
            if item.cancel.is_cancelled() {
                let _ = batch.cancel_request(&source.request_id);
            }
        }
        if let Some(completion) = batch.try_complete().map_err(|_| ComputeFailure::ExecutionFailed)? {
            break completion;
        }
        let _ = batch.receive_event_timeout(Duration::from_millis(10));
    };
    // Native completion is observed before returning any permit to foreground.
    // The resident worker stays loaded; this only finishes this exact request.
    let native_receipt = completion.receipt_bytes().map_err(failed)?;
    let native_blob = store.store_provenance_blob(&native_receipt).map_err(failed)?;
    let outputs = completion.outputs();
    if outputs.len() != jobs.len() {
        return Err(ComputeFailure::InvalidOutput);
    }
    let mut outcomes = Vec::with_capacity(jobs.len());
    for (index, ((item, source), output)) in jobs.iter().zip(&prepared).zip(outputs).enumerate() {
        let result = (|| {
            if lease.cancel.is_cancelled() || item.cancel.is_cancelled() {
                return Err(ComputeFailure::HostBusy);
            }
            if output.input_index != index
                || output.branch_id != source.request.cases[0].generation.branch_id.to_string()
                || output.model_id != model.profile.model_id
                || output.state != llama_native_types::GenerationState::Completed
                || !output.real_engine_invoked || output.fake_fixture
                || output.transport != llama_native_types::NativeTransport::InProcess
                || output.text.len() > loom_cabal::compute::MAX_COMPUTE_TEXT_BYTES
                || output.generated_token_ids.len() > item.job.input.max_output_tokens as usize
                || output.generated_token_ids.len() != output.metrics.completion_tokens
            {
                return Err(ComputeFailure::InvalidOutput);
            }
            let receipt = serde_json::to_vec(&serde_json::json!({
                "kind": "loom_peer_batched_result_v1",
                "peer": item.job.peer,
                "job": item.job.id,
                "request_fingerprint": item.job.input.fingerprint(item.job.grant.id).map_err(failed)?,
                "original_request_id": source.request_id,
                "batch_mapping_blob": mapping_blob,
                "native_execution_blob": native_blob,
                "native_input_index": index,
                "native_output": output,
            })).map_err(failed)?;
            let evidence = store.store_provenance_blob(&receipt).map_err(failed)?;
            store.create_generated_document_if_absent(
                format!("Results/{}/{}.md", item.job.peer, item.job.id),
                DocumentContent::Prose(output.text.clone()),
                "Peer batch completion",
                evidence,
            ).map_err(failed)?;
            Ok(output.text.clone())
        })();
        outcomes.push(ComputeBatchOutput { peer: item.job.peer, job: item.job.id, result });
    }
    Ok(outcomes)
}

#[cfg(test)]
#[path = "peer_compute_batch_tests.rs"]
mod tests;
