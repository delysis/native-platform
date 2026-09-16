//! One bounded static envelope, with a separate deadline, cancellation token and
//! durable terminal per (peer, job). This is not a continuous-admission worker.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

const COLLECTION_WINDOW: Duration = Duration::from_millis(20);
const CANCELLATION_POLL: Duration = Duration::from_millis(10);

pub(super) async fn supervise(
    state: Arc<Mutex<HostState>>,
    authority: Authority,
    executor: Arc<dyn ComputeExecutor>,
    mut pending: mpsc::Receiver<PendingComputeJob>,
    stop: CancellationToken,
) -> Result<()> {
    let outcome = loop {
        let first = tokio::select! {
            biased;
            value = pending.recv() => match value { Some(job) => job, None => break Ok(()) },
            () = stop.cancelled() => break Ok(()),
        };
        if executor.batch_limit() > 1 {
            tokio::select! {
                () = stop.cancelled() => (),
                () = tokio::time::sleep_until(first.admitted_at + COLLECTION_WINDOW) => (),
            }
        }
        // Closing collection and draining accepted sends share the admission
        // lock. No request can be durably accepted into a departing envelope.
        let batch = (|| {
            let mut state = state.lock().map_err(|_| Error::Invalid("Compute owner stopped"))?;
            state.collecting = false;
            let mut batch = vec![first];
            while let Ok(job) = pending.try_recv() {
                batch.push(job);
            }
            if batch.len() > MAX_COMPUTE_BATCH_JOBS {
                return Err(Error::Invalid("Compute batch exceeded admission capacity"));
            }
            Ok(batch)
        })();
        let result = match batch {
            Ok(batch) => run_batch(&state, &authority, executor.clone(), batch, &stop).await,
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            stop.cancel();
            break Err(error);
        }
        if stop.is_cancelled() {
            break Ok(());
        }
    };
    let cleanup = (|| {
        let mut state = state.lock().map_err(|_| Error::Invalid("Compute owner stopped"))?;
        state.closed = true;
        state.collecting = false;
        for job in std::mem::take(&mut state.active) {
            state.ledger.transition(job.peer, job.id, ComputeStatus::Interrupted)?;
        }
        Ok(())
    })();
    outcome.and(cleanup)
}

fn cancellation(
    state: &mut HostState,
    authority: &Authority,
    job: &PendingComputeJob,
    stop: &CancellationToken,
) -> Result<Option<ComputeCancellation>> {
    let expired = job.admitted_at.elapsed() >= Duration::from_secs(u64::from(job.job.grant.max_seconds));
    let job = &job.job;
    let reason = if stop.is_cancelled() {
        Some(ComputeCancellation::HostStopping)
    } else if state.ledger.find_grant(job.grant.id)?.is_none() || !authority(&job.grant) {
        Some(ComputeCancellation::GrantRevoked)
    } else if expired {
        Some(ComputeCancellation::TimeLimit)
    } else {
        None
    };
    let receipt = if let Some(reason) = reason {
        state.ledger.cancel(job.peer, job.id, reason)?
    } else {
        state.ledger.get(job.peer, job.id)?
    }.ok_or(Error::Invalid("Compute job disappeared"))?;
    Ok(match receipt.payload.status {
        ComputeStatus::Cancelling { reason } => Some(reason),
        _ => None,
    })
}

fn release(state: &mut HostState, job: &HostComputeJob) {
    state.active.retain(|item| item.peer != job.peer || item.id != job.id);
}

async fn run_batch(
    state: &Mutex<HostState>,
    authority: &Authority,
    executor: Arc<dyn ComputeExecutor>,
    batch: Vec<PendingComputeJob>,
    stop: &CancellationToken,
) -> Result<()> {
    let mut running = Vec::with_capacity(batch.len());
    {
        let mut state = state.lock().map_err(|_| Error::Invalid("Compute owner stopped"))?;
        for pending in batch {
            let job = &pending.job;
            if let Some(reason) = cancellation(&mut state, authority, &pending, stop)? {
                state.ledger.transition(job.peer, job.id, ComputeStatus::Cancelled { reason })?;
                release(&mut state, job);
            } else {
                state.ledger.transition(job.peer, job.id, ComputeStatus::Running)?;
                running.push((pending, CancellationToken::new()));
            }
        }
    }
    if running.is_empty() {
        return Ok(());
    }
    let jobs = running.iter().map(|(pending, cancel)| ComputeBatchJob {
        job: pending.job.clone(),
        cancel: cancel.clone(),
    }).collect();
    // Adapter panics are contained; cancellation never drops the join handle.
    let mut worker = tokio::spawn(async move { executor.execute_batch(jobs).await });
    let mut persistence_failure = None;
    let outcome = loop {
        tokio::select! {
            result = &mut worker => break result,
            () = tokio::time::sleep(CANCELLATION_POLL) => {
                let observed = (|| {
                    let mut state = state.lock().map_err(|_| Error::Invalid("Compute owner stopped"))?;
                    for (pending, cancel) in &running {
                        if cancellation(&mut state, authority, pending, stop)?.is_some() {
                            cancel.cancel();
                        }
                    }
                    Ok::<(), Error>(())
                })();
                if let Err(error) = observed {
                    persistence_failure.get_or_insert(error);
                    stop.cancel();
                    for (_, cancel) in &running {
                        cancel.cancel();
                    }
                }
            }
        }
    };
    // A broken durable ledger still does not grant permission to abandon native work.
    if let Some(error) = persistence_failure {
        return Err(error);
    }
    let expected = running.iter().map(|(pending, _)| (pending.job.peer, pending.job.id)).collect::<BTreeSet<_>>();
    let (mut outputs, invalid) = match outcome {
        Ok(outputs) => match index_outputs(outputs, &expected) {
            Some(outputs) => (outputs, None),
            None => (BTreeMap::new(), Some(ComputeFailure::InvalidOutput)),
        },
        Err(_) => (BTreeMap::new(), Some(ComputeFailure::WorkerPanicked)),
    };
    let mut state = state.lock().map_err(|_| Error::Invalid("Compute owner stopped"))?;
    for (pending, _) in running {
        let job = &pending.job;
        let status = if let Some(reason) = cancellation(&mut state, authority, &pending, stop)? {
            ComputeStatus::Cancelled { reason }
        } else {
            let result = outputs.remove(&(job.peer, job.id)).unwrap_or_else(|| Err(invalid.unwrap_or(ComputeFailure::InvalidOutput)));
            match result {
                Ok(text) if text.len() <= MAX_COMPUTE_TEXT_BYTES => ComputeStatus::Completed { text },
                Ok(_) => ComputeStatus::Failed { failure: ComputeFailure::InvalidOutput },
                Err(failure) => ComputeStatus::Failed { failure },
            }
        };
        state.ledger.transition(job.peer, job.id, status)?;
        release(&mut state, job);
    }
    Ok(())
}

type Outcomes = BTreeMap<(PublicKey, Uuid), std::result::Result<String, ComputeFailure>>;

fn index_outputs(outputs: Vec<ComputeBatchOutput>, expected: &BTreeSet<(PublicKey, Uuid)>) -> Option<Outcomes> {
    if outputs.len() != expected.len() {
        return None;
    }
    let mut indexed = BTreeMap::new();
    for output in outputs {
        let key = (output.peer, output.job);
        if !expected.contains(&key) || indexed.insert(key, output.result).is_some() {
            return None;
        }
    }
    Some(indexed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_output_identity_must_be_bijective_not_just_the_right_length() -> Result<()> {
        let peer = Identity::generate()?.public_key();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let expected = BTreeSet::from([(peer, first), (peer, second)]);
        let output = |job| ComputeBatchOutput { peer, job, result: Ok("output".into()) };
        assert!(index_outputs(vec![output(second), output(first)], &expected).is_some());
        assert!(index_outputs(vec![output(first), output(first)], &expected).is_none());
        assert!(index_outputs(vec![output(first), output(Uuid::new_v4())], &expected).is_none());
        assert!(index_outputs(vec![output(first)], &expected).is_none());
        Ok(())
    }
}
