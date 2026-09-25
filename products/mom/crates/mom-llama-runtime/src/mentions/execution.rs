//! Batch only compatible inputs, then join each native request before reusing
//! its logical invocation ID. Every case retains its target cancellation ID.
//! Native errors do not authorize a different model or silent media removal.

use super::*;
use llama_native_engine::GenerationTicket;
use llama_native_types::{
    GenerationBatchRequest, GenerationCacheMetrics, GenerationCase, GenerationEvent,
    NativeError, NativeErrorCode, NativeTransport,
};

pub(super) type GroupCompletion = (
    std::result::Result<Vec<GenerationOutput>, NativeError>, Vec<PlannedTarget>,
);

#[derive(Clone, Debug, PartialEq, Eq)]
struct CaseShape {
    identity: String,
    cells: usize,
    capacity: usize,
    max_sequences: usize,
    has_media: bool,
}

fn error(message: impl Into<String>) -> NativeError {
    NativeError::new(NativeErrorCode::InvalidConfig, message)
}

fn shape(target: &PlannedTarget, settings: &Settings) -> Result<CaseShape> {
    let media = target.media.iter().map(|item|
        (&item.id, item.kind, &item.mime, &item.sha256, item.bytes.len())).collect::<Vec<_>>();
    let identity = sha256_json(&(&target.model_fingerprint, &target.model_config,
        &target.snapshot.profile.chat_template, media))?;
    let reserve = target.snapshot.profile.sampling.as_ref()
        .map_or(settings.default_max_tokens, |sampling| sampling.max_tokens) as usize;
    let cells = target.text_prompt_tokens.checked_add(reserve)
        .ok_or_else(|| anyhow!("consult context accounting overflowed"))?;
    Ok(CaseShape {
        identity, cells, capacity: target.model_fingerprint.context_tokens as usize,
        max_sequences: target.model_fingerprint.max_sequences as usize,
        has_media: !target.media.is_empty(),
    })
}

/// Text-only full-input counts conservatively bound a batch without assuming
/// any cache hit. Media counts need the native decoder, so media cases execute
/// individually rather than guessing that four image prompts fit a context.
fn pack(shapes: &[CaseShape]) -> std::result::Result<Vec<Vec<usize>>, NativeError> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, item) in shapes.iter().enumerate() {
        if item.max_sequences == 0 || item.capacity == 0 || item.cells > item.capacity {
            return Err(error("a consult case cannot fit its declared resident context"));
        }
        let compatible = (!item.has_media).then(|| groups.iter().position(|indices| {
            let first = &shapes[indices[0]];
            first.identity == item.identity && !first.has_media
                && indices.len() < item.max_sequences.min(MAX_TARGETS)
                && indices.iter().try_fold(item.cells, |sum, index|
                    sum.checked_add(shapes[*index].cells)).is_some_and(|sum| sum <= item.capacity)
        })).flatten();
        if let Some(group) = compatible { groups[group].push(index); }
        else { groups.push(vec![index]); }
    }
    Ok(groups)
}

pub(super) fn cache_was_reused(metrics: &GenerationCacheMetrics) -> bool {
    metrics.restored_prefix_tokens > 0 || metrics.resident_prefix_tokens > 0
        || metrics.batch_shared_prefix_tokens > 0
}

fn validate_outputs(
    request: &str, model: &str, target_ids: &[String], outputs: &[GenerationOutput],
) -> std::result::Result<(), NativeError> {
    if outputs.len() != target_ids.len() {
        return Err(error("native consult output count does not match its admitted cases"));
    }
    for (index, (target, output)) in target_ids.iter().zip(outputs).enumerate() {
        if output.request_id != request || output.model_id != model
            || output.branch_id != *target || output.input_index != index
            || output.fake_fixture || output.transport != NativeTransport::InProcess
            || !matches!(output.state, GenerationState::Completed | GenerationState::Cancelled | GenerationState::Failed)
            || (output.state == GenerationState::Completed && !output.real_engine_invoked)
        {
            return Err(error("native consult output identity or terminal evidence disagrees with its case"));
        }
    }
    Ok(())
}

trait ConsultTicket: Sized {
    fn events(&self) -> &crossbeam_channel::Receiver<GenerationEvent>;
    fn cancel_all(&self);
    fn cancel_branch(&self, branch: &str);
    fn finish(self) -> std::result::Result<Vec<GenerationOutput>, NativeError>;
}
impl ConsultTicket for GenerationTicket {
    fn events(&self) -> &crossbeam_channel::Receiver<GenerationEvent> { &self.events }
    fn cancel_all(&self) { GenerationTicket::cancel_all(self); }
    fn cancel_branch(&self, branch: &str) { GenerationTicket::cancel_branch(self, branch); }
    fn finish(self) -> std::result::Result<Vec<GenerationOutput>, NativeError> { self.wait() }
}

// Callback panics in unwinding builds follow the same cancellation/drain path.
// Returned errors omit the panic payload. The application-owned panic hook may
// still run; this does not claim to suppress panic-hook output.
fn observed_call<T>(call: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(call))
        .map_err(|_| anyhow!("consult observer or registry callback panicked"))?
}

struct Drained {
    outcome: std::result::Result<Vec<GenerationOutput>, NativeError>,
    observer_failure: Option<anyhow::Error>,
}

/// A broken observer or poisoned product registry cannot drop the only native
/// completion handle. Cancel, drain and wait first; then return the first error.
#[allow(clippy::too_many_arguments)]
fn drain<T: ConsultTicket>(
    ticket: T,
    request_id: &str,
    target_ids: &[String],
    started: Instant,
    timeout: Duration,
    mut cancellations: impl FnMut() -> Result<Vec<String>>,
    mut observe: impl FnMut(GenerationEvent) -> Result<()>,
) -> Drained {
    let mut failure = None;
    let mut last_events = vec![None; target_ids.len()];
    loop {
        if started.elapsed() >= timeout { ticket.cancel_all(); }
        if failure.is_none() {
            match observed_call(&mut cancellations) {
                Ok(cancelled) => for id in cancelled { ticket.cancel_branch(&id); },
                Err(error) => { failure = Some(error); ticket.cancel_all(); }
            }
        }
        match ticket.events().recv_timeout(Duration::from_millis(10)) {
            Ok(event) => {
                if failure.is_some() { continue; }
                let index = target_ids.iter().position(|id| id == &event.branch_id);
                let valid = index.is_some_and(|index| event.request_id == request_id
                    && event.input_index == index
                    && last_events[index].is_none_or(|previous| event.event_index > previous));
                if !valid {
                    failure = Some(anyhow!("native consult stream identity or event order changed"));
                    ticket.cancel_all();
                    continue;
                }
                let index = index.expect("validated case index");
                last_events[index] = Some(event.event_index);
                if let Err(error) = observed_call(|| observe(event)) { failure = Some(error); ticket.cancel_all(); }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    let outcome = ticket.finish();
    Drained { outcome, observer_failure: failure }
}

pub(super) fn terminal_cancelled(scope: &OperationScope, invocation: &str, target: &str) -> Result<bool> {
    scope.with_mention_registry(|registry, quiescing| {
        let control = registry.get(&(invocation.to_owned(), target.to_owned()))
            .ok_or_else(|| anyhow!("consult target lost its cancellation registration"))?;
        if quiescing { control.request_cancel(); }
        Ok(control.arbitrate_terminal())
    })
}

fn retain_native_outputs(
    data_dir: &Path, invocation_id: &str, outputs: &[GenerationOutput],
) -> Result<()> {
    let store = RuntimeStore::open(data_dir)?;
    for output in outputs {
        let key = sha256_json(&(invocation_id, &output.branch_id))?;
        let namespace = format!("mention-native-output.v1.{key}");
        let value = json!({
            "schema": "mom_llama.consult_initial_native_output.v1",
            "evidence_class": "native-output-dto-not-owner-worker-seal",
            "output": output,
        });
        store.mutate_documents(&namespace, || None::<Value>, |existing, _| {
            if existing.is_some() { return Err(anyhow!("an immutable consult output receipt already exists")); }
            *existing = Some(value.clone());
            Ok(())
        })?;
    }
    Ok(())
}

pub(super) fn execute_groups<F>(
    scope: &OperationScope, invocation_id: &str, planned: Vec<PlannedTarget>,
    settings: &Settings, options: ChatSendOptions, on_event: &mut Option<F>,
) -> Result<Vec<GroupCompletion>>
where F: FnMut(ChatDispatchStreamEvent) -> Result<()>
{
    let shapes = planned.iter().map(|target| shape(target, settings)).collect::<Result<Vec<_>>>()?;
    let groups = pack(&shapes).map_err(|error| anyhow!(error))?;
    let mut pending = planned.into_iter().map(Some).collect::<Vec<_>>();
    let mut finished = Vec::new();
    // One execution deadline, not a fresh timeout for every target/group.
    // Source/model preparation precedes this interval, as in the existing dispatcher.
    let started = Instant::now();
    let timeout = Duration::try_from_secs_f64(options.timeout_s)
        .map_err(|_| anyhow!("consult timeout must be finite and non-negative"))?
        .max(Duration::from_millis(1));
    for indices in groups {
        let mut group = indices.into_iter().map(|index| pending[index].take()
            .expect("every planned target belongs to one group")).collect::<Vec<_>>();
        for target in &group {
            emit(on_event, MentionStreamEvent {
                schema: "mom_llama.mention_stream_event.v1".into(), invocation_id: invocation_id.into(),
                target_id: target.snapshot.target_id.clone(), handle: target.snapshot.handle.clone(),
                label: target.snapshot.label.clone(), event: "started".into(), delta: None,
                state: Some(GenerationState::Queued), real_engine_invoked: false, fake_fixture: false,
            })?;
        }
        let mut targets = Vec::new();
        let mut cancelled = Vec::new();
        let submitted = scope.with_mention_registry(|registry, quiescing| {
            for target in group.drain(..) {
                let key = (invocation_id.to_owned(), target.snapshot.target_id.clone());
                let allowed = !quiescing && started.elapsed() < timeout
                    && registry.get(&key).is_some_and(|control| !control.cancellation_requested());
                if allowed { targets.push(target); } else { cancelled.push(target); }
            }
            if targets.is_empty() { return Ok(None); }
            if targets.iter().any(|target| target.handle.status().fingerprint.as_ref() != Some(&target.model_fingerprint)) {
                return Ok(Some(Err(error("the resident model identity changed before consult admission"))));
            }
            let cases = targets.iter().map(|target| GenerationCase {
                case_id: target.snapshot.target_id.clone(),
                input: GenerationInput::Chat { messages: target.messages.clone(),
                    template: profile_chat_template(&target.snapshot.profile) },
                sampling: target.snapshot.profile.sampling.clone().unwrap_or_else(|| settings.sampling_config()),
                cached_prefix: target.cached_prefix.clone(),
            }).collect();
            let media = std::mem::take(&mut targets[0].media);
            let handle = targets[0].handle.clone();
            Ok(Some(handle.generate_batch(GenerationBatchRequest {
                first_word_choices: None, request_id: invocation_id.into(),
                model_id: targets[0].model_fingerprint.model_id.clone(), media, cases,
            })))
        })?;
        if !cancelled.is_empty() {
            finished.push((Err(NativeError::new(NativeErrorCode::Cancelled,
                "consult cancelled before native admission")), cancelled));
        }
        let Some(submitted) = submitted else { continue; };
        let ticket = match submitted {
            Ok(ticket) => ticket,
            Err(error) => { finished.push((Err(error), targets)); continue; }
        };
        let ids = targets.iter().map(|target| target.snapshot.target_id.clone()).collect::<Vec<_>>();
        let drained = drain(ticket, invocation_id, &ids, started, timeout, || {
            scope.with_mention_registry(|registry, quiescing| {
                Ok(ids.iter().filter(|id| quiescing || registry
                    .get(&(invocation_id.to_owned(), (*id).clone()))
                    .is_none_or(|control| control.cancellation_requested())).cloned().collect())
            })
        }, |event| {
            let target = &targets[event.input_index];
            let (name, delta, state) = match event.event {
                GenerationEventKind::Delta { text } => ("delta", Some(text), None),
                GenerationEventKind::State { state } => ("state", None, Some(state)),
                GenerationEventKind::Warning { message, .. } => ("warning", Some(message), None),
            };
            emit(on_event, MentionStreamEvent {
                schema: "mom_llama.mention_stream_event.v1".into(), invocation_id: invocation_id.into(),
                target_id: target.snapshot.target_id.clone(), handle: target.snapshot.handle.clone(),
                label: target.snapshot.label.clone(), event: name.into(), delta, state,
                real_engine_invoked: name == "delta" || state == Some(GenerationState::Completed), fake_fixture: false,
            })
        });
        let outcome = drained.outcome.and_then(|outputs| {
            validate_outputs(invocation_id, &targets[0].model_fingerprint.model_id, &ids, &outputs)?;
            Ok(outputs)
        });
        if let Ok(outputs) = &outcome { retain_native_outputs(&settings.data_dir, invocation_id, outputs)?; }
        if let Some(error) = drained.observer_failure { return Err(error); }
        finished.push((outcome, targets));
    }
    Ok(finished)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn case(identity: &str, cells: usize, media: bool) -> CaseShape {
        CaseShape { identity: identity.into(), cells, capacity: 100, max_sequences: 4, has_media: media }
    }
    fn output() -> GenerationOutput {
        GenerationOutput {
            first_word_choice: None, request_id: "invocation".into(), branch_id: "expert".into(),
            input_index: 0, model_id: "model".into(), text: "real output".into(),
            generated_token_ids: vec![1], token_observations: None, state: GenerationState::Completed,
            finish_reason: "end_of_generation".into(), metrics: GenerationMetrics::default(),
            real_engine_invoked: true, fake_fixture: false, transport: NativeTransport::InProcess,
        }
    }
    fn event(index: u64) -> GenerationEvent {
        GenerationEvent { request_id: "invocation".into(), branch_id: "expert".into(),
            sequence_id: 0, input_index: 0, event_index: index,
            event: GenerationEventKind::Delta { text: "piece".into() } }
    }
    struct FakeTicket {
        events: crossbeam_channel::Receiver<GenerationEvent>,
        joined: Arc<AtomicBool>, cancelled: Arc<AtomicUsize>, branch_cancelled: Arc<Mutex<Vec<String>>>,
        output: std::result::Result<Vec<GenerationOutput>, NativeError>,
    }
    impl ConsultTicket for FakeTicket {
        fn events(&self) -> &crossbeam_channel::Receiver<GenerationEvent> { &self.events }
        fn cancel_all(&self) { self.cancelled.fetch_add(1, Ordering::SeqCst); }
        fn cancel_branch(&self, branch: &str) { self.branch_cancelled.lock().expect("fixture lock").push(branch.into()); }
        fn finish(self) -> std::result::Result<Vec<GenerationOutput>, NativeError> {
            assert!(self.events.is_empty(), "native completion must be drained before wait");
            self.joined.store(true, Ordering::SeqCst);
            self.output
        }
    }
    fn ticket(events: Vec<GenerationEvent>) -> FakeTicket {
        let (send, receive) = crossbeam_channel::unbounded();
        for event in events { send.send(event).expect("fixture stream"); }
        drop(send);
        FakeTicket { events: receive, joined: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicUsize::new(0)), branch_cancelled: Arc::new(Mutex::new(Vec::new())),
            output: Ok(vec![output()]) }
    }

    #[test]
    fn compatible_text_batches_obey_the_actual_aggregate_capacity() {
        let shapes = [case("a", 40, false), case("a", 40, false), case("a", 40, false), case("b", 10, false)];
        assert_eq!(pack(&shapes).expect("bounded groups"), [vec![0, 1], vec![2], vec![3]]);
    }
    #[test]
    fn media_cases_never_share_a_pool_across_experts() {
        assert_eq!(pack(&[case("same", 10, true), case("same", 10, true)]).expect("isolated"), [vec![0], vec![1]]);
        let mut single = case("same", 10, false);
        single.max_sequences = 1;
        assert_eq!(pack(&[single.clone(), single]).expect("single slot"), [vec![0], vec![1]]);
    }
    #[test]
    fn invalid_resident_capacity_fails_before_admission() {
        let mut missing = case("x", 1, false); missing.max_sequences = 0;
        assert!(pack(&[missing]).is_err());
        assert!(pack(&[case("x", 101, false)]).is_err());
    }
    #[test]
    fn supplied_or_replayed_prefix_is_not_a_cache_hit() {
        let mut metrics = GenerationCacheMetrics::default();
        metrics.supplied_prefix_tokens = 100;
        metrics.replayed_prefix_tokens = 100;
        assert!(!cache_was_reused(&metrics));
        metrics.restored_prefix_tokens = 1;
        assert!(cache_was_reused(&metrics));
        metrics.restored_prefix_tokens = 0;
        metrics.batch_shared_prefix_tokens = 1;
        assert!(cache_was_reused(&metrics));
        metrics.batch_shared_prefix_tokens = 0;
        metrics.resident_prefix_tokens = 1;
        assert!(cache_was_reused(&metrics));
    }
    #[test]
    fn outputs_require_exact_case_identity_and_complete_cardinality() {
        let ids = vec!["expert".into()];
        assert!(validate_outputs("invocation", "model", &ids, &[output()]).is_ok());
        assert!(validate_outputs("invocation", "model", &ids, &[]).is_err());
        for field in ["request", "model", "case", "index", "fixture", "transport", "state", "engine"] {
            let mut value = output();
            match field {
                "request" => value.request_id = "other".into(),
                "model" => value.model_id = "other".into(),
                "case" => value.branch_id = "other".into(),
                "index" => value.input_index = 1,
                "fixture" => value.fake_fixture = true,
                "transport" => value.transport = NativeTransport::FakeFixture,
                "state" => value.state = GenerationState::Generating,
                _ => value.real_engine_invoked = false,
            }
            assert!(validate_outputs("invocation", "model", &ids, &[value]).is_err(), "{field}");
        }
    }
    #[test]
    fn observer_failure_cancels_drains_and_waits_before_returning_its_error() {
        let ticket = ticket(vec![event(0), event(1), event(2)]);
        let joined = Arc::clone(&ticket.joined); let cancelled = Arc::clone(&ticket.cancelled);
        let failure = drain(ticket, "invocation", &["expert".into()], Instant::now(), Duration::from_secs(1),
            || Ok(Vec::new()), |_| Err(anyhow!("observer failed"))).observer_failure.expect("observer failure");
        assert_eq!(failure.to_string(), "observer failed");
        assert!(joined.load(Ordering::SeqCst));
        assert!(cancelled.load(Ordering::SeqCst) > 0);
    }
    #[test]
    fn corrupt_stream_identity_never_reaches_the_observer_and_still_waits() {
        let mut wrong = event(0); wrong.input_index = 9;
        let ticket = ticket(vec![wrong, event(1)]);
        let joined = Arc::clone(&ticket.joined);
        assert!(drain(ticket, "invocation", &["expert".into()], Instant::now(), Duration::from_secs(1),
            || Ok(Vec::new()), |_| panic!("corrupt event must not be observed")).observer_failure.is_some());
        assert!(joined.load(Ordering::SeqCst));
    }
    #[test]
    fn duplicate_stream_events_are_not_accepted_as_new_progress() {
        let ticket = ticket(vec![event(5), event(5), event(6)]);
        let joined = Arc::clone(&ticket.joined); let mut observed = 0;
        assert!(drain(ticket, "invocation", &["expert".into()], Instant::now(), Duration::from_secs(1),
            || Ok(Vec::new()), |_| { observed += 1; Ok(()) }).observer_failure.is_some());
        assert_eq!(observed, 1);
        assert!(joined.load(Ordering::SeqCst));
    }
    #[test]
    fn target_cancellation_and_total_deadline_reach_the_real_ticket_interface() {
        let ticket = ticket(vec![event(0)]);
        let joined = Arc::clone(&ticket.joined); let cancelled = Arc::clone(&ticket.cancelled);
        let branches = Arc::clone(&ticket.branch_cancelled);
        assert!(drain(ticket, "invocation", &["expert".into()], Instant::now(), Duration::ZERO,
            || Ok(vec!["expert".into()]), |_| Ok(())).outcome.is_ok());
        assert!(joined.load(Ordering::SeqCst));
        assert!(cancelled.load(Ordering::SeqCst) > 0);
        assert!(branches.lock().expect("fixture").iter().all(|id| id == "expert"));
        assert!(!branches.lock().expect("fixture").is_empty());
    }
    #[test]
    fn registry_failure_is_not_no_cancellation() {
        let ticket = ticket(vec![event(0), event(1)]);
        let joined = Arc::clone(&ticket.joined);
        assert!(drain(ticket, "invocation", &["expert".into()], Instant::now(), Duration::from_secs(1),
            || Err(anyhow!("poisoned registry")), |_| panic!("no observation after lost authority")).observer_failure.is_some());
        assert!(joined.load(Ordering::SeqCst));
    }
    #[test]
    fn observer_failure_does_not_discard_the_joined_native_output_receipt() {
        let ticket = ticket(vec![event(0)]);
        let drained = drain(ticket, "invocation", &["expert".into()], Instant::now(), Duration::from_secs(1),
            || Ok(Vec::new()), |_| Err(anyhow!("observer lost")));
        assert!(drained.observer_failure.is_some());
        assert_eq!(drained.outcome.expect("retained native completion"), vec![output()]);
    }

    #[test]
    #[cfg(panic = "unwind")]
    fn panicking_observer_cancels_and_drains_before_returning_a_content_free_error() {
        let ticket = ticket(vec![event(0), event(1)]);
        let joined = Arc::clone(&ticket.joined);
        let cancelled = Arc::clone(&ticket.cancelled);
        let drained = drain(ticket, "invocation", &["expert".into()], Instant::now(), Duration::from_secs(1),
            || Ok(Vec::new()), |_| panic!("private source text"));
        assert_eq!(drained.observer_failure.expect("panic mapped").to_string(),
            "consult observer or registry callback panicked");
        assert!(joined.load(Ordering::SeqCst));
        assert!(cancelled.load(Ordering::SeqCst) > 0);
        assert!(drained.outcome.is_ok());
    }

}
