//! Opt-in request sharing on one resident owner thread.
//!
//! Each request has one uncached case, its own sampler, cancellation flag,
//! logical event sequence and completion channel. Physical KV slots are never
//! exposed as request-local sequence identities. No prefix is shared between
//! requests, even when their token prefixes happen to match.
use super::*;
use crate::operation_registry::RequestTerminalClass;
use std::collections::BTreeMap;
use std::sync::{Weak, atomic::AtomicU64};

const MAX_LIVE: usize = 8;
const PREFILL_QUANTUM: usize = 64;
const MAX_PROMPT_TOKENS: usize = 64 * 1024;
const MAX_OUTPUT_TOKENS: u32 = 2048;
const MAX_OBSERVERS: usize = 2;
const OBSERVATION_CAPACITY: usize = 4096;

/// One request's participation in a successful native decode call.
/// This contains opaque request identities and counts, never prompt/token bytes.
#[derive(Clone, Debug)]
pub struct DecodeMember {
    request_id: String,
    case_id: String,
    physical_sequence: usize,
    prefill_tokens: usize,
    decode_tokens: usize,
}

impl DecodeMember {
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    #[must_use]
    pub fn case_id(&self) -> &str {
        &self.case_id
    }
    #[must_use]
    pub const fn physical_sequence(&self) -> usize {
        self.physical_sequence
    }
    #[must_use]
    pub const fn prefill_tokens(&self) -> usize {
        self.prefill_tokens
    }
    #[must_use]
    pub const fn decode_tokens(&self) -> usize {
        self.decode_tokens
    }
}

/// Observation made only after the real context's decode call succeeds.
/// This is local diagnostic evidence, not a transferable execution attestation.
#[derive(Clone, Debug)]
pub struct DecodeSample {
    ordinal: u64,
    members: Vec<DecodeMember>,
}

impl DecodeSample {
    #[must_use]
    pub const fn ordinal(&self) -> u64 {
        self.ordinal
    }
    #[must_use]
    pub fn members(&self) -> &[DecodeMember] {
        &self.members
    }
}

#[derive(Debug, Default)]
struct ObservationState {
    dropped: AtomicU64,
}

/// A bounded, optional observation stream. Slow observers never block inference.
/// A qualification must reject a nonzero dropped-sample count.
#[derive(Debug)]
pub struct DecodeObserver {
    receiver: Receiver<DecodeSample>,
    state: Arc<ObservationState>,
}

impl DecodeObserver {
    pub fn receive_timeout(&self, timeout: Duration) -> NativeResult<Option<DecodeSample>> {
        match self.receiver.recv_timeout(timeout) {
            Ok(sample) => Ok(Some(sample)),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => Ok(None),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native decode observation owner stopped",
            )),
        }
    }

    #[must_use]
    pub fn dropped_samples(&self) -> u64 {
        self.state.dropped.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
struct Subscriber {
    sender: Sender<DecodeSample>,
    state: Weak<ObservationState>,
}

#[derive(Debug, Default)]
struct HubState {
    ordinal: u64,
    closed: bool,
    subscribers: Vec<Subscriber>,
}

#[derive(Debug, Default)]
pub(super) struct DecodeHub {
    state: Mutex<HubState>,
}

#[derive(Debug)]
pub(super) struct DecodeWorkerLifetime<'a>(&'a DecodeHub);
impl Drop for DecodeWorkerLifetime<'_> {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.subscribers.clear();
    }
}

impl DecodeHub {
    pub(super) fn worker_lifetime(&self) -> DecodeWorkerLifetime<'_> {
        DecodeWorkerLifetime(self)
    }
    fn subscribe(&self) -> NativeResult<DecodeObserver> {
        let mut hub = self
            .state
            .lock()
            .map_err(|_| internal("decode observer registry poisoned"))?;
        if hub.closed {
            return Err(NativeError::new(
                NativeErrorCode::WorkerStopped,
                "native decode observation owner stopped",
            ));
        }
        hub.subscribers
            .retain(|subscriber| subscriber.state.strong_count() != 0);
        if hub.subscribers.len() >= MAX_OBSERVERS {
            return Err(NativeError::new(
                NativeErrorCode::QueueFull,
                "native decode observers are full",
            ));
        }
        let (sender, receiver) = bounded(OBSERVATION_CAPACITY);
        let state = Arc::new(ObservationState::default());
        hub.subscribers.push(Subscriber {
            sender,
            state: Arc::downgrade(&state),
        });
        Ok(DecodeObserver { receiver, state })
    }

    fn publish(&self, members: Vec<DecodeMember>) -> NativeResult<()> {
        let mut hub = self
            .state
            .lock()
            .map_err(|_| internal("decode observer registry poisoned"))?;
        hub.ordinal = hub
            .ordinal
            .checked_add(1)
            .ok_or_else(|| internal("native decode ordinal overflow"))?;
        let sample = DecodeSample {
            ordinal: hub.ordinal,
            members,
        };
        hub.subscribers.retain(|subscriber| {
            let Some(state) = subscriber.state.upgrade() else {
                return false;
            };
            if subscriber.sender.try_send(sample.clone()).is_err() {
                let _ = state
                    .dropped
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                        Some(old.saturating_add(1))
                    });
            }
            true
        });
        Ok(())
    }
}

impl NativeModelHandle {
    /// Submit one independently owned, uncached exact-token case to the cooperative lane.
    /// Later cooperative requests may join while this request is decoding.
    /// Media, caller caches and out-of-band reasoning intervention are not
    /// supported on this lane. The resident must be loaded with `kv_unified`
    /// disabled; its descriptor reports the capacity of each independent stream.
    pub fn generate_cooperative(
        &self,
        request: GenerationBatchRequest,
    ) -> NativeResult<GenerationTicket> {
        if self.inner.worker_identity.kv_unified {
            return Err(unsupported(
                "cooperative generation requires independently allocated KV streams (kv_unified = false)",
            ));
        }
        validate_request(&request)?;
        self.submit_generation_batch(
            request,
            GenerationBatchAdmission::Cooperative,
            GenerationAdmissionClass::Foreground,
        )
    }

    /// Observe successful cooperative decode calls for this exact resident.
    /// Completion and shutdown authority remain with tickets and the owner.
    pub fn observe_cooperative_batches(&self) -> NativeResult<DecodeObserver> {
        self.inner.ensure_accepting()?;
        self.inner.worker_identity.cooperative_decodes.subscribe()
    }
}

fn validate_request(request: &GenerationBatchRequest) -> NativeResult<()> {
    let [case] = request.cases.as_slice() else {
        return Err(unsupported(
            "cooperative generation requires exactly one independently owned case",
        ));
    };
    if request.first_word_choices.is_some()
        || !request.media.is_empty()
        || case.cached_prefix.is_some()
    {
        return Err(unsupported(
            "cooperative generation does not accept media or caller KV caches",
        ));
    }
    if !(1..=MAX_OUTPUT_TOKENS).contains(&case.sampling.max_tokens) {
        return Err(unsupported(
            "cooperative output must be between 1 and 2048 tokens",
        ));
    }
    let bounded_input = match &case.input {
        GenerationInput::Completion { prompts } => match prompts.as_slice() {
            [CompletionPrompt::Tokens { token_ids }] => {
                !token_ids.is_empty() && token_ids.len() <= MAX_PROMPT_TOKENS
            }
            _ => false,
        },
        GenerationInput::Chat { .. } | GenerationInput::FillInMiddle { .. } => false,
    };
    if !bounded_input {
        return Err(unsupported(
            "cooperative generation requires one nonempty exact-token prompt of at most 65536 tokens",
        ));
    }
    Ok(())
}

fn internal(message: &str) -> NativeError {
    NativeError::new(NativeErrorCode::Internal, message)
}
fn unsupported(message: &str) -> NativeError {
    NativeError::new(NativeErrorCode::UnsupportedParameter, message)
}
fn copy_error(error: &NativeError) -> NativeError {
    NativeError::new(error.code, error.message.clone())
}
fn cancelled() -> NativeError {
    NativeError::new(NativeErrorCode::Cancelled, "cooperative request cancelled")
}

/// Fixed native stream geometry. `Pool::live` is the sole occupancy authority.
#[derive(Debug)]
struct StreamLayout {
    cells: usize,
    slots: usize,
}
impl StreamLayout {
    fn new(cells: usize, sequences: usize, batch: usize) -> Self {
        Self {
            cells,
            slots: sequences.min(batch).min(MAX_LIVE),
        }
    }
    fn vacant_slot(
        &self,
        cells: usize,
        occupied: impl Fn(usize) -> bool,
    ) -> NativeResult<Option<usize>> {
        if cells == 0 || cells > self.cells || self.slots == 0 {
            return Err(NativeError::new(
                NativeErrorCode::PromptTooLarge,
                "cooperative request exceeds its independent stream capacity",
            ));
        }
        Ok((0..self.slots).find(|slot| !occupied(*slot)))
    }
}

struct Job {
    request: GenerationBatchRequest,
    event_tx: Sender<GenerationEvent>,
    result_tx: Sender<NativeResult<GenerationCompletion>>,
    cancel: Arc<AtomicBool>,
    reasoning_force: Arc<AtomicBool>,
    lease: RequestLease,
}

impl Job {
    fn event(&self, event_index: u64, event: GenerationEventKind) -> GenerationEvent {
        GenerationEvent {
            request_id: self.request.request_id.clone(),
            branch_id: self.request.cases[0].case_id.clone(),
            // The shared physical slot is deliberately not the caller's index.
            sequence_id: 0,
            input_index: 0,
            event_index,
            event,
        }
    }

    fn from_command(command: WorkerCommand) -> NativeResult<Self> {
        let WorkerCommand::GenerateBatch {
            request,
            admission: GenerationBatchAdmission::Cooperative,
            event_tx,
            result_tx,
            mut cancellations,
            mut reasoning_forces,
            request_lease,
            ..
        } = command
        else {
            return Err(internal(
                "noncooperative command entered the cooperative executor",
            ));
        };
        if cancellations.len() != 1 || reasoning_forces.len() != 1 {
            let error = internal("cooperative request lost its independent controls");
            let _ = result_tx.send(Err(copy_error(&error)));
            return Err(error);
        }
        Ok(Self {
            request,
            event_tx,
            result_tx,
            cancel: cancellations.remove(0),
            reasoning_force: reasoning_forces.remove(0),
            lease: request_lease,
        })
    }

    fn finish(self, result: NativeResult<GenerationCompletion>) {
        self.publish(result, false);
    }

    fn publish(self, result: NativeResult<GenerationCompletion>, queued_rejection: bool) {
        // This lane owns one case. Cancelled partial output remains a
        // successful envelope; authority qualification is independent.
        let terminal = match &result {
            Ok(completion)
                if completion
                    .outputs
                    .iter()
                    .any(|output| output.state == GenerationState::Cancelled) =>
            {
                RequestTerminalClass::Cancelled
            }
            Ok(_) => RequestTerminalClass::Completed,
            Err(error) if error.code == NativeErrorCode::Cancelled => {
                RequestTerminalClass::Cancelled
            }
            Err(_) => RequestTerminalClass::Failed,
        };
        if queued_rejection {
            let _ = self.lease.reject_queued(terminal);
        } else {
            let _ = self.lease.finished(terminal);
        }
        if result.is_err() {
            if terminal == RequestTerminalClass::Cancelled {
                emit_cancelled_case_events(&self.event_tx, &self.request);
            } else {
                emit_failed_case_events(&self.event_tx, &self.request);
            }
        }
        let _ = self.result_tx.send(result);
    }

    fn reject(self, error: NativeError) {
        if error.code == NativeErrorCode::Cancelled {
            self.cancel.store(true, Ordering::Release);
        }
        self.publish(Err(error), true);
        // The executor lease, not the waiting ticket, releases this identity.
    }
}

struct Pending {
    job: Job,
    tokens: Vec<LlamaToken>,
    cells: usize,
}

impl Pending {
    fn prepare(
        job: Job,
        model: &LlamaModel,
        maximum_cells: usize,
    ) -> std::result::Result<Self, (Box<Job>, NativeError)> {
        let prepared = (|| {
            validate_request(&job.request)?;
            let tokens = generation_case_tokens(model, &job.request.cases[0], 0)?;
            let cells = tokens
                .len()
                .checked_add(job.request.cases[0].sampling.max_tokens.saturating_sub(1) as usize)
                .ok_or_else(|| internal("cooperative cell count overflow"))?;
            if tokens.is_empty() || cells > maximum_cells {
                return Err(NativeError::new(
                    NativeErrorCode::PromptTooLarge,
                    "cooperative prompt and maximum growth exceed the resident context",
                ));
            }
            Ok((tokens, cells))
        })();
        match prepared {
            Ok((tokens, cells)) => Ok(Self { job, tokens, cells }),
            Err(error) => Err((Box::new(job), error)),
        }
    }
}

struct Sequence {
    job: Job,
    prompt: Vec<LlamaToken>,
    prefilled: usize,
    next_position: usize,
    pending_token: Option<LlamaToken>,
    sampler: LlamaSampler,
    decoder: encoding_rs::Decoder,
    text: String,
    token_ids: Vec<i32>,
    trace: Option<TokenPieceTrace>,
    events: Option<Vec<GenerationEvent>>,
    authority_error: Option<NativeError>,
    terminal_token: Option<i32>,
    finish: Option<&'static str>,
    generating: bool,
    event_index: u64,
    started: Instant,
    first_token_ms: Option<u128>,
}

impl Sequence {
    fn start(
        pending: Pending,
        runtime: &Runtime<'_, '_>,
    ) -> std::result::Result<Self, (Box<Job>, NativeError)> {
        let Pending { job, tokens, .. } = pending;
        if let Err(error) = begin_generation_command(
            &job.lease,
            WorkerCommandClass::Foreground,
            runtime.speculative,
        ) {
            return Err((Box::new(job), error));
        }
        let authority_error = if is_statically_sealable_generation_batch(
            &job.request,
            GenerationBatchAdmission::Cooperative,
        ) {
            runtime
                .artifacts
                .verify_strict_unchanged(runtime.fingerprint)
                .err()
        } else {
            Some(unsupported(
                "this cooperative request does not carry exact-token authority",
            ))
        };
        let retain = authority_error.is_none();
        let maximum = job.request.cases[0].sampling.max_tokens as usize;
        let mut sampler = build_sampler(runtime.model, &job.request.cases[0].sampling);
        sampler.accept_many(tokens.iter());
        let mut sequence = Self {
            job,
            prompt: tokens,
            prefilled: 0,
            next_position: 0,
            pending_token: None,
            sampler,
            decoder: UTF_8.new_decoder(),
            text: String::new(),
            token_ids: Vec::new(),
            trace: retain.then(|| TokenPieceTrace::with_token_capacity(maximum)),
            events: retain.then(Vec::new),
            authority_error,
            terminal_token: None,
            finish: None,
            generating: false,
            event_index: 0,
            started: Instant::now(),
            first_token_ms: None,
        };
        sequence.emit(GenerationEventKind::State {
            state: GenerationState::Prefilling,
        });
        Ok(sequence)
    }

    fn emit(&mut self, event: GenerationEventKind) {
        let value = self.job.event(self.event_index, event);
        self.event_index += 1;
        if let Some(events) = &mut self.events {
            events.push(value.clone());
        }
        if matches!(&value.event, GenerationEventKind::State { state } if is_terminal_state(*state))
        {
            try_emit_terminal(&self.job.event_tx, value);
        } else {
            try_emit_nonterminal(&self.job.event_tx, value);
        }
    }

    fn sample(
        &mut self,
        model: &LlamaModel,
        context: &LlamaContext<'_>,
        row: i32,
    ) -> NativeResult<()> {
        if !self.generating {
            self.generating = true;
            self.emit(GenerationEventKind::State {
                state: GenerationState::Generating,
            });
        }
        if self.job.cancel.load(Ordering::Acquire) {
            self.finish = Some("cancelled");
            return Ok(());
        }
        let token = self.sampler.sample(context, row);
        if model.is_eog_token(token) {
            self.terminal_token = Some(token.0);
            self.finish = Some("end_of_generation");
            return Ok(());
        }
        self.token_ids.push(token.0);
        let bytes = generated_token_piece(model, token)
            .map_err(|error| native_decode_error("cooperative token piece", error))?;
        if let Some(trace) = &mut self.trace {
            trace.push_piece(&bytes)?;
        }
        let piece = decode_generated_utf8_piece(&mut self.decoder, &bytes, false)?;
        self.first_token_ms
            .get_or_insert_with(|| self.started.elapsed().as_millis());
        append_generated_utf8_piece(&mut self.text, &piece)?;
        if !piece.is_empty() {
            self.emit(GenerationEventKind::Delta { text: piece });
        }
        let sampling = &self.job.request.cases[0].sampling;
        if apply_stop_sequences(&mut self.text, &sampling.stop) {
            self.finish = Some("stop_sequence");
        } else if self.token_ids.len() >= sampling.max_tokens as usize {
            self.finish = Some("max_tokens");
        } else {
            self.pending_token = Some(token);
        }
        Ok(())
    }

    fn finish(mut self, runtime: &Runtime<'_, '_>) {
        let result = self.completion(|request, outputs, capture| {
            verify_generation_batch_authority(
                runtime.model,
                request,
                runtime.fingerprint.clone(),
                outputs,
                capture,
                runtime.artifacts,
            )
        });
        self.job.finish(result);
    }

    fn fail(self, error: NativeError) {
        self.job.finish(Err(error));
    }

    fn completion(
        &mut self,
        verify_authority: impl FnOnce(
            GenerationBatchRequest,
            &[GenerationOutput],
            GenerationAuthorityCapture,
        ) -> NativeResult<VerifiedGenerationEvidence>,
    ) -> NativeResult<GenerationCompletion> {
        let finish_reason = self
            .finish
            .ok_or_else(|| internal("cooperative completion has no terminal"))?;
        if !self.generating {
            // Cancellation before all prompt tokens were decoded is not a
            // completed execution, and cannot manufacture a generation seal.
            return Err(cancelled());
        }
        let piece = finalize_generated_text(
            &mut self.decoder,
            &mut self.text,
            finish_reason == "stop_sequence",
        )?;
        if !piece.is_empty() {
            self.emit(GenerationEventKind::Delta { text: piece });
        }
        let state = if finish_reason == "cancelled" {
            GenerationState::Cancelled
        } else {
            GenerationState::Completed
        };
        // Authority capture retains the original terminal and event index.
        // Public progress cannot claim it before final envelope selection.
        let terminal = self
            .job
            .event(self.event_index, GenerationEventKind::State { state });
        self.event_index += 1;
        if let Some(events) = &mut self.events {
            events.push(terminal.clone());
        }
        let duration_ms = self.started.elapsed().as_millis();
        let completion_tokens = self.token_ids.len();
        let output = GenerationOutput {
            first_word_choice: None,
            request_id: self.job.request.request_id.clone(),
            branch_id: self.job.request.cases[0].case_id.clone(),
            input_index: 0,
            model_id: self.job.request.model_id.clone(),
            text: std::mem::take(&mut self.text),
            generated_token_ids: std::mem::take(&mut self.token_ids),
            token_observations: None,
            state,
            finish_reason: finish_reason.to_owned(),
            metrics: GenerationMetrics {
                prompt_tokens: self.prompt.len(),
                completion_tokens,
                shared_prefix_tokens: 0,
                duration_ms,
                first_token_ms: self.first_token_ms,
                tokens_per_second: if duration_ms == 0 {
                    0.0
                } else {
                    completion_tokens as f64 * 1000.0 / duration_ms as f64
                },
                cache: GenerationCacheMetrics::default(),
            },
            real_engine_invoked: true,
            fake_fixture: false,
            transport: NativeTransport::InProcess,
        };
        let outputs = vec![output];
        let authority =
            match self.authority_error.take() {
                Some(error) => Err(error),
                None => verify_authority(
                    self.job.request.clone(),
                    &outputs,
                    GenerationAuthorityCapture {
                        terminal_sampled_token_ids: vec![self.terminal_token],
                        events: self
                            .events
                            .take()
                            .ok_or_else(|| internal("cooperative authority lost its events"))?,
                        token_piece_traces: vec![self.trace.take().ok_or_else(|| {
                            internal("cooperative authority lost its token trace")
                        })?],
                    },
                ),
            };
        let completion = match authority {
            Ok(evidence) => GenerationCompletion::verified(outputs, evidence),
            Err(error) => GenerationCompletion::authority_rejected(outputs, error),
        };
        try_emit_terminal(&self.job.event_tx, terminal);
        Ok(completion)
    }
}

pub(super) struct Runtime<'a, 'model> {
    pub model: &'model LlamaModel,
    pub context: &'a mut LlamaContext<'model>,
    pub fingerprint: &'a ModelFingerprint,
    pub artifacts: &'a ModelArtifactGuards,
    pub status: &'a Arc<RwLock<ResidentModelStatus>>,
    pub identity: &'a WorkerIdentity,
    pub commands: &'a Receiver<WorkerCommand>,
    pub shutdown: &'a Receiver<()>,
    pub admission: &'a Mutex<()>,
    pub speculative: &'a SpeculativeAdmission,
    pub deferred: &'a mut VecDeque<ReceivedWorkerCommand>,
}

struct Pool {
    layout: StreamLayout,
    live: BTreeMap<usize, Sequence>,
    pending: Option<Pending>,
    prefill_cursor: usize,
}

pub(super) fn run(first: WorkerCommand, mut runtime: Runtime<'_, '_>) {
    let mut pool = Pool {
        layout: StreamLayout::new(
            (runtime.context.n_ctx() / runtime.fingerprint.max_sequences) as usize,
            runtime.fingerprint.max_sequences as usize,
            runtime.context.n_batch() as usize,
        ),
        live: BTreeMap::new(),
        pending: None,
        prefill_cursor: 0,
    };
    if let Err(error) = run_inner(first, &mut runtime, &mut pool) {
        // A failed native mutation invalidates every sequence in that context.
        // Do not certify unaffected siblings or reuse a slot after uncertainty.
        runtime.context.clear_kv_cache();
        for (_, sequence) in std::mem::take(&mut pool.live) {
            sequence.fail(copy_error(&error));
        }
        if let Some(pending) = pool.pending.take() {
            pending.job.reject(error);
        }
    }
    runtime.context.clear_kv_cache();
    state_buffer::forget_live_exports();
    set_status_state(runtime.status, ModelRuntimeState::Ready, 0);
}

fn run_inner(
    first: WorkerCommand,
    runtime: &mut Runtime<'_, '_>,
    pool: &mut Pool,
) -> NativeResult<()> {
    let mut first = Some(first);
    let mut draining = false;
    loop {
        if !runtime.shutdown.is_empty() {
            draining = true;
            for sequence in pool.live.values() {
                sequence.job.cancel.store(true, Ordering::Release);
            }
            if let Some(pending) = pool.pending.take() {
                pending.job.reject(cancelled());
            }
            if let Some(command) = first.take() {
                reject_queued_command(command);
            }
        }
        settle(runtime, pool)?;
        if pool.pending.is_none() && !draining {
            let command = first.take().or_else(|| runtime.commands.try_recv().ok());
            if let Some(command) = command {
                // Admission commits cancellation/preemption under this lock.
                drop(
                    runtime
                        .admission
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
                if matches!(
                    &command,
                    WorkerCommand::GenerateBatch {
                        admission: GenerationBatchAdmission::Cooperative,
                        ..
                    }
                ) {
                    let job = Job::from_command(command)?;
                    if job.cancel.load(Ordering::Acquire) {
                        job.reject(cancelled());
                    } else {
                        match Pending::prepare(job, runtime.model, pool.layout.cells) {
                            Ok(pending) => pool.pending = Some(pending),
                            Err((job, error)) => (*job).reject(error),
                        }
                    }
                } else {
                    // Preserve FIFO barriers for snapshots, media, controls and
                    // other generation modes. New work cannot starve them.
                    runtime.deferred.push_back(ReceivedWorkerCommand {
                        command,
                        class: WorkerCommandClass::Foreground,
                        speculative_permit: None,
                    });
                    draining = true;
                }
            }
        }
        if let Some(pending) = pool.pending.take() {
            if pending.job.cancel.load(Ordering::Acquire) {
                pending.job.reject(cancelled());
            } else if let Some(slot) = pool
                .layout
                .vacant_slot(pending.cells, |slot| pool.live.contains_key(&slot))?
            {
                match Sequence::start(pending, runtime) {
                    Ok(sequence) => {
                        if let std::collections::btree_map::Entry::Vacant(entry) =
                            pool.live.entry(slot)
                        {
                            entry.insert(sequence);
                        } else {
                            let error = internal("cooperative slot was occupied during admission");
                            sequence.fail(copy_error(&error));
                            return Err(error);
                        }
                    }
                    Err((job, error)) => {
                        (*job).reject(error);
                    }
                }
            } else {
                pool.pending = Some(pending);
            }
        }
        if pool.live.is_empty() {
            if pool.pending.is_some() {
                return Err(internal(
                    "empty cooperative pool cannot admit its pending request",
                ));
            }
            return Ok(());
        }
        set_status_state(runtime.status, ModelRuntimeState::Ready, pool.live.len());
        step(runtime, pool)?;
    }
}

fn clear_slot(runtime: &mut Runtime<'_, '_>, slot: usize, used: bool) -> NativeResult<()> {
    if used {
        let cleared = runtime
            .context
            .clear_kv_cache_seq(Some(slot as u32), None, None)
            .map_err(|error| native_decode_error("cooperative sequence removal", error))?;
        if !cleared {
            return Err(internal(
                "native context refused cooperative sequence removal",
            ));
        }
    }
    Ok(())
}

fn settle(runtime: &mut Runtime<'_, '_>, pool: &mut Pool) -> NativeResult<()> {
    let terminal = pool
        .live
        .iter()
        .filter_map(|(slot, sequence)| {
            (sequence.finish.is_some()
                || sequence.job.cancel.load(Ordering::Acquire)
                || sequence.job.reasoning_force.load(Ordering::Acquire))
            .then_some(*slot)
        })
        .collect::<Vec<_>>();
    for slot in terminal {
        let mut sequence = pool
            .live
            .remove(&slot)
            .ok_or_else(|| internal("cooperative slot disappeared"))?;
        if let Err(error) = clear_slot(runtime, slot, sequence.prefilled != 0) {
            sequence.fail(copy_error(&error));
            return Err(error);
        }
        if sequence.job.reasoning_force.load(Ordering::Acquire) {
            sequence.fail(unsupported(
                "reasoning intervention is unsupported for an independently scheduled request",
            ));
        } else {
            // Cancellation observed before publication dominates provisional text.
            if sequence.job.cancel.load(Ordering::Acquire) {
                sequence.finish = Some("cancelled");
                sequence.terminal_token = None;
            }
            sequence.finish(runtime);
        }
    }
    Ok(())
}

fn step(runtime: &mut Runtime<'_, '_>, pool: &mut Pool) -> NativeResult<()> {
    let capacity = runtime.context.n_batch() as usize;
    let mut batch = LlamaBatch::new(capacity, 1);
    let mut logits = Vec::new();
    let mut members = Vec::new();
    for (slot, sequence) in &mut pool.live {
        // As in ordinary native generation, prefill excludes the final prompt
        // token. Ready requests share this one-token-per-sequence decode call.
        let final_prompt = sequence.prefilled + 1 == sequence.prompt.len();
        let token = if final_prompt {
            sequence.prompt.last().copied()
        } else {
            sequence.pending_token.take()
        };
        let Some(token) = token else {
            continue;
        };
        let row = logits.len() as i32;
        batch
            .add(token, sequence.next_position as i32, &[*slot as i32], true)
            .map_err(|error| native_decode_error("cooperative generation batch", error))?;
        sequence.next_position += 1;
        if final_prompt {
            sequence.prefilled += 1;
        }
        logits.push((*slot, row));
        members.push(DecodeMember {
            request_id: sequence.job.request.request_id.clone(),
            case_id: sequence.job.request.cases[0].case_id.clone(),
            physical_sequence: *slot,
            prefill_tokens: usize::from(final_prompt),
            decode_tokens: usize::from(!final_prompt),
        });
    }
    let mut progressed = !logits.is_empty();
    if progressed {
        runtime
            .context
            .decode(&mut batch)
            .map_err(|error| native_decode_error("cooperative native decode", error))?;
        runtime.identity.cooperative_decodes.publish(members)?;
        // A later prefill replaces the logit buffer; consume every owner first.
        for (slot, row) in logits {
            pool.live
                .get_mut(&slot)
                .ok_or_else(|| internal("cooperative logit owner disappeared"))?
                .sample(runtime.model, runtime.context, row)?;
        }
    }

    // Keep prefill bounded and rotate its owner, without combining a multi-token
    // prefill with another sequence's generation row. This retains the existing
    // native prompt boundary and avoids changing that request's cached states.
    let mut remaining = capacity.min(PREFILL_QUANTUM);
    let mut prefilling = pool
        .live
        .iter()
        .filter_map(|(slot, sequence)| {
            (sequence.prefilled + 1 < sequence.prompt.len()).then_some(*slot)
        })
        .collect::<Vec<_>>();
    prefilling.sort_by_key(|slot| (*slot + MAX_LIVE - pool.prefill_cursor) % MAX_LIVE);
    for slot in prefilling {
        if remaining == 0 {
            break;
        }
        let sequence = pool
            .live
            .get_mut(&slot)
            .ok_or_else(|| internal("cooperative prefill owner disappeared"))?;
        let count = remaining.min(sequence.prompt.len() - 1 - sequence.prefilled);
        let end = sequence.prefilled + count;
        decode_tokens_chunked(
            runtime.context,
            &sequence.prompt[sequence.prefilled..end],
            slot as i32,
            sequence.prefilled as i32,
            false,
        )?;
        runtime
            .identity
            .cooperative_decodes
            .publish(vec![DecodeMember {
                request_id: sequence.job.request.request_id.clone(),
                case_id: sequence.job.request.cases[0].case_id.clone(),
                physical_sequence: slot,
                prefill_tokens: count,
                decode_tokens: 0,
            }])?;
        sequence.prefilled = end;
        sequence.next_position = end;
        remaining -= count;
        pool.prefill_cursor = (slot + 1) % MAX_LIVE;
        progressed = true;
    }
    if !progressed {
        return Err(internal(
            "live cooperative requests produced no bounded decode work",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_registry::RequestTerminalClass;

    fn cooperative_owner_job(request_id: &str) -> (NativeModelHandle, Job, GenerationTicket) {
        let (handle, job, ticket) = cooperative_queued_job(request_id);
        job.lease.running().expect("owner started");
        (handle, job, ticket)
    }

    fn cooperative_queued_job(request_id: &str) -> (NativeModelHandle, Job, GenerationTicket) {
        let (mut handle, commands, _speculative) =
            crate::tests::test_admission_handle(crate::tests::admission_test_status(), 2);
        let inner = Arc::get_mut(&mut handle.inner).expect("unique admission fixture handle");
        Arc::get_mut(&mut inner.worker_identity)
            .expect("unique admission fixture worker")
            .kv_unified = false;
        let request = GenerationBatchRequest {
            request_id: request_id.into(),
            model_id: "model".into(),
            first_word_choices: None,
            media: Vec::new(),
            cases: vec![GenerationCase {
                case_id: "case".into(),
                input: GenerationInput::Completion {
                    prompts: vec![CompletionPrompt::Tokens {
                        token_ids: vec![1, 2],
                    }],
                },
                sampling: SamplingConfig {
                    max_tokens: 2,
                    ..Default::default()
                },
                cached_prefix: None,
            }],
        };
        let ticket = handle
            .generate_cooperative(request)
            .expect("actual cooperative admission");
        let job = Job::from_command(commands.recv().expect("actual queued command"))
            .expect("actual cooperative owner");
        (handle, job, ticket)
    }

    // A result-checkpoint fixture; no sampler, model, token decode or authority seal.
    fn cooperative_checkpoint_output(
        request_id: &str,
        state: GenerationState,
    ) -> GenerationCompletion {
        GenerationCompletion::unverified(vec![GenerationOutput {
            first_word_choice: None,
            request_id: request_id.into(),
            branch_id: "case".into(),
            input_index: 0,
            model_id: "model".into(),
            text: "retained partial".into(),
            generated_token_ids: vec![17],
            token_observations: None,
            state,
            finish_reason: if state == GenerationState::Cancelled {
                "cancelled"
            } else {
                "max_tokens"
            }
            .into(),
            metrics: GenerationMetrics::default(),
            real_engine_invoked: false,
            fake_fixture: true,
            transport: NativeTransport::InProcess,
        }])
    }

    // Model-free finalization checkpoint. The sampler is safely allocated and
    // dropped on this thread, but never samples or touches a model/context.
    // Production-shaped output fields are not evidence of native execution;
    // the private authority callable can only refuse authority in these tests.
    fn model_free_sequence_checkpoint(
        request_id: &str,
        finish_reason: &'static str,
    ) -> (NativeModelHandle, Sequence, GenerationTicket) {
        let (handle, job, ticket) = cooperative_owner_job(request_id);
        let mut sequence = Sequence {
            job,
            prompt: vec![LlamaToken(1), LlamaToken(2)],
            prefilled: 2,
            next_position: 3,
            pending_token: None,
            sampler: LlamaSampler::greedy(),
            decoder: UTF_8.new_decoder(),
            text: String::new(),
            token_ids: vec![17, 18],
            trace: Some(TokenPieceTrace::with_token_capacity(2)),
            events: Some(Vec::new()),
            authority_error: None,
            terminal_token: None,
            finish: Some(finish_reason),
            generating: true,
            event_index: 0,
            started: Instant::now(),
            first_token_ms: Some(0),
        };
        sequence.emit(GenerationEventKind::State {
            state: GenerationState::Prefilling,
        });
        sequence.emit(GenerationEventKind::State {
            state: GenerationState::Generating,
        });
        for bytes in [b"checkpoint ".as_slice(), "café\r\n".as_bytes()] {
            sequence
                .trace
                .as_mut()
                .expect("owned trace")
                .push_piece(bytes)
                .expect("bounded local piece");
            let text = decode_generated_utf8_piece(&mut sequence.decoder, bytes, false)
                .expect("local UTF-8 checkpoint");
            append_generated_utf8_piece(&mut sequence.text, &text)
                .expect("bounded checkpoint text");
            sequence.emit(GenerationEventKind::Delta { text });
        }
        (handle, sequence, ticket)
    }

    fn public_terminals(events: &Receiver<GenerationEvent>) -> Vec<GenerationState> {
        events
            .try_iter()
            .filter_map(|event| match event.event {
                GenerationEventKind::State { state } if is_terminal_state(state) => Some(state),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn cooperative_queued_failure_survives_late_cancel_and_held_final_delivery() {
        for drop_receiver in [false, true] {
            let (handle, mut job, mut ticket) =
                cooperative_queued_job("cooperative-queued-selected-error");
            let request = job.request.clone();
            let control = Arc::clone(&ticket.control);
            let events = ticket.events.clone();
            // Hold only the final delivery; admission/command/ticket/lease are real.
            let (sender, receiver) = bounded(0);
            job.result_tx = sender;
            ticket.result = receiver;
            let worker = std::thread::spawn(move || {
                job.reject(NativeError::new(
                    NativeErrorCode::PromptTooLarge,
                    "selected queued error",
                ));
            });
            let selected = events
                .recv_timeout(Duration::from_secs(3))
                .expect("actual selected error projection before the final send");
            let late_cancel = ticket.cancel_branch("case");
            let held = control.snapshot();
            let held_count = handle.inner.requests.active_count();
            let duplicate_code = handle
                .generate_cooperative(request.clone())
                .err()
                .map(|error| error.code);
            let delivered = if drop_receiver {
                drop(ticket);
                None
            } else {
                Some(ticket.wait().expect_err("original selected error"))
            };
            worker
                .join()
                .expect("owner finished its final send attempt");
            let released = control.snapshot();
            assert!(late_cancel);
            assert_eq!(selected.request_id, request.request_id);
            assert_eq!(selected.branch_id, "case");
            assert!(matches!(
                selected.event,
                GenerationEventKind::State {
                    state: GenerationState::Failed
                }
            ));
            assert_eq!(
                held.phase,
                crate::operation_registry::RequestPhase::Terminal
            );
            assert_eq!(held_count, 1);
            assert_eq!(
                duplicate_code,
                Some(NativeErrorCode::DuplicateActiveRequest)
            );
            assert!(held.cancellation_requested);
            assert_eq!(
                held.authoritative_terminal
                    .expect("selected terminal")
                    .class,
                RequestTerminalClass::Failed
            );
            if let Some(error) = delivered {
                assert_eq!(error.code, NativeErrorCode::PromptTooLarge);
                assert_eq!(error.message, "selected queued error");
            }
            assert_eq!(released.identity, held.identity);
            assert_eq!(
                released.phase,
                crate::operation_registry::RequestPhase::Released
            );
            assert_eq!(
                released
                    .authoritative_terminal
                    .expect("retained terminal")
                    .class,
                RequestTerminalClass::Failed
            );
            assert_eq!(handle.inner.requests.active_count(), 0);
            assert!(public_terminals(&events).is_empty());
        }
    }

    #[test]
    fn cooperative_queued_cancellation_keeps_its_existing_public_result() {
        let (handle, job, ticket) = cooperative_queued_job("cooperative-queued-cancel");
        let control = Arc::clone(&ticket.control);
        let events = ticket.events.clone();
        job.reject(cancelled());
        assert_eq!(
            ticket.wait().expect_err("queued cancellation").code,
            NativeErrorCode::Cancelled
        );
        assert_eq!(public_terminals(&events), [GenerationState::Cancelled]);
        assert_eq!(
            control
                .snapshot()
                .authoritative_terminal
                .expect("terminal")
                .class,
            RequestTerminalClass::Cancelled
        );
        assert_eq!(handle.inner.requests.active_count(), 0);
    }

    // These two faults violate private retention invariants. They harden final
    // envelope selection; they are not valid-input native inference witnesses.
    fn assert_model_free_retention_fault(mut sequence: Sequence, ticket: GenerationTicket) {
        let control = Arc::clone(&ticket.control);
        let events = ticket.events.clone();
        let result = sequence.completion(|_, _, _| {
            panic!("missing retained data must refuse before authority verification")
        });
        sequence.job.finish(result);
        assert_eq!(
            ticket.wait().expect_err("retention invariant failure").code,
            NativeErrorCode::Internal
        );
        assert_eq!(public_terminals(&events), [GenerationState::Failed]);
        assert_eq!(
            control
                .snapshot()
                .authoritative_terminal
                .expect("terminal")
                .class,
            RequestTerminalClass::Failed
        );
    }

    #[test]
    fn model_free_sequence_missing_events_publishes_only_selected_failure() {
        let (_handle, mut sequence, ticket) =
            model_free_sequence_checkpoint("cooperative-lost-events", "max_tokens");
        sequence.events = None;
        assert_model_free_retention_fault(sequence, ticket);
    }

    #[test]
    fn model_free_sequence_missing_trace_publishes_only_selected_failure() {
        let (_handle, mut sequence, ticket) =
            model_free_sequence_checkpoint("cooperative-lost-trace", "cancelled");
        sequence.trace = None;
        assert_model_free_retention_fault(sequence, ticket);
    }

    #[test]
    fn model_free_sequence_preserves_partial_results_and_retained_terminal_without_authority() {
        for finish_reason in ["max_tokens", "cancelled"] {
            for verified_wait in [false, true] {
                let (handle, mut sequence, ticket) =
                    model_free_sequence_checkpoint("cooperative-envelope-control", finish_reason);
                let expected = if finish_reason == "cancelled" {
                    GenerationState::Cancelled
                } else {
                    GenerationState::Completed
                };
                let control = Arc::clone(&ticket.control);
                let events = ticket.events.clone();
                let result = sequence.completion(|request, outputs, capture| {
                    assert_eq!(request.request_id, "cooperative-envelope-control");
                    assert_eq!(outputs[0].text, "checkpoint café\r\n");
                    assert_eq!(outputs[0].generated_token_ids, [17, 18]);
                    assert_eq!(capture.terminal_sampled_token_ids, [None]);
                    assert_eq!(
                        capture.token_piece_traces[0].raw_piece_bytes(),
                        "checkpoint café\r\n".as_bytes()
                    );
                    assert_eq!(
                        capture
                            .events
                            .last()
                            .expect("retained terminal")
                            .event_index,
                        4
                    );
                    assert!(matches!(
                        capture.events.last().expect("retained terminal").event,
                        GenerationEventKind::State { state } if state == expected
                    ));
                    // No model was loaded/decoded and no seal may be fabricated.
                    Err(unsupported(
                        "model-free checkpoint carries no native authority",
                    ))
                });
                assert!(
                    result
                        .as_ref()
                        .expect("selected execution envelope")
                        .authority
                        .is_err()
                );
                sequence.job.finish(result);
                if verified_wait {
                    assert_eq!(
                        ticket
                            .wait_verified()
                            .expect_err("no native authority")
                            .code,
                        NativeErrorCode::UnsupportedParameter
                    );
                } else {
                    let outputs = ticket.wait().expect("partial execution envelope");
                    assert_eq!(outputs[0].text, "checkpoint café\r\n");
                    assert_eq!(outputs[0].generated_token_ids, [17, 18]);
                    assert_eq!(outputs[0].state, expected);
                }
                assert_eq!(public_terminals(&events), [expected]);
                assert_eq!(
                    control
                        .snapshot()
                        .authoritative_terminal
                        .expect("terminal")
                        .class,
                    if expected == GenerationState::Cancelled {
                        RequestTerminalClass::Cancelled
                    } else {
                        RequestTerminalClass::Completed
                    }
                );
                assert_eq!(handle.inner.requests.active_count(), 0);
            }
        }
    }

    #[test]
    fn model_free_sequence_unsealable_and_prefill_controls_keep_their_refusals() {
        for prefill_cancel in [false, true] {
            let (_handle, mut sequence, ticket) =
                model_free_sequence_checkpoint("cooperative-refusal-control", "cancelled");
            let events = ticket.events.clone();
            sequence.authority_error = Some(unsupported("not statically sealable"));
            sequence.events = None;
            sequence.trace = None;
            sequence.generating = !prefill_cancel;
            let result = sequence
                .completion(|_, _, _| panic!("these paths never call the authority verifier"));
            sequence.job.finish(result);
            if prefill_cancel {
                assert_eq!(
                    ticket.wait().expect_err("prefill cannot complete").code,
                    NativeErrorCode::Cancelled
                );
            } else {
                assert_eq!(
                    ticket.wait().expect("unsealable partial result")[0].text,
                    "checkpoint café\r\n"
                );
            }
            assert_eq!(public_terminals(&events), [GenerationState::Cancelled]);
        }
    }

    #[test]
    fn cooperative_prefill_cancel_projects_the_selected_cancelled_error() {
        let (_handle, job, ticket) = cooperative_owner_job("cooperative-prefill-cancel");
        let control = Arc::clone(&ticket.control);
        let events = ticket.events.clone();
        assert!(ticket.cancel_branch("case"));
        job.finish(Err(cancelled()));
        assert_eq!(
            ticket.wait().expect_err("no prefill completion/seal").code,
            NativeErrorCode::Cancelled
        );
        assert_eq!(
            control
                .snapshot()
                .authoritative_terminal
                .expect("terminal")
                .class,
            RequestTerminalClass::Cancelled
        );
        let states: Vec<_> = events
            .try_iter()
            .filter_map(|event| match event.event {
                GenerationEventKind::State { state } => Some(state),
                _ => None,
            })
            .collect();
        assert_eq!(states, vec![GenerationState::Cancelled]);
    }

    #[test]
    fn cooperative_partial_cancel_retains_output_and_cancelled_registry_terminal() {
        let (_handle, job, ticket) = cooperative_owner_job("cooperative-partial-cancel");
        let control = Arc::clone(&ticket.control);
        assert!(ticket.cancel_branch("case"));
        job.finish(Ok(cooperative_checkpoint_output(
            &ticket.request_id,
            GenerationState::Cancelled,
        )));
        let outputs = ticket
            .wait()
            .expect("useful partial completion remains available");
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].state, GenerationState::Cancelled);
        assert_eq!(outputs[0].text, "retained partial");
        assert!(outputs[0].fake_fixture && !outputs[0].real_engine_invoked);
        assert_eq!(
            control
                .snapshot()
                .authoritative_terminal
                .expect("terminal")
                .class,
            RequestTerminalClass::Cancelled
        );
    }

    #[test]
    fn cooperative_decode_failure_is_not_reclassified_by_a_late_cancel_flag() {
        let (_handle, job, ticket) = cooperative_owner_job("cooperative-error-cancel");
        let control = Arc::clone(&ticket.control);
        let events = ticket.events.clone();
        assert!(ticket.cancel_branch("case"));
        job.finish(Err(native_decode_error(
            "cooperative checkpoint",
            "selected decode failure",
        )));
        assert_eq!(
            ticket.wait().expect_err("selected error").code,
            NativeErrorCode::DecodeFailed
        );
        assert_eq!(
            control
                .snapshot()
                .authoritative_terminal
                .expect("terminal")
                .class,
            RequestTerminalClass::Failed
        );
        let states: Vec<_> = events
            .try_iter()
            .filter_map(|event| match event.event {
                GenerationEventKind::State { state } => Some(state),
                _ => None,
            })
            .collect();
        assert_eq!(states, vec![GenerationState::Failed]);
    }

    #[test]
    fn cooperative_completed_output_keeps_execution_and_authority_independent() {
        for cancel in [false, true] {
            let (_handle, job, ticket) = cooperative_owner_job("cooperative-completed");
            let control = Arc::clone(&ticket.control);
            if cancel {
                assert!(ticket.cancel_branch("case"));
            }
            let completion =
                cooperative_checkpoint_output(&ticket.request_id, GenerationState::Completed);
            assert!(completion.authority.is_err());
            job.finish(Ok(completion));
            assert_eq!(
                ticket.wait().expect("execution result")[0].state,
                GenerationState::Completed
            );
            assert_eq!(
                control
                    .snapshot()
                    .authoritative_terminal
                    .expect("terminal")
                    .class,
                RequestTerminalClass::Completed
            );
        }
    }

    #[test]
    fn cooperative_selected_terminal_precedes_delivery_and_survives_a_dropped_receiver() {
        for drop_receiver in [false, true] {
            let (handle, mut job, mut ticket) = cooperative_owner_job("cooperative-final-delivery");
            let control = Arc::clone(&ticket.control);
            // Only the private final channel capacity changes at this checkpoint.
            // The public ticket still consumes the actual owner's selected result.
            let (sender, receiver) = bounded(0);
            job.result_tx = sender;
            ticket.result = receiver;
            while job
                .event_tx
                .try_send(GenerationEvent {
                    request_id: ticket.request_id.clone(),
                    branch_id: "case".into(),
                    sequence_id: 0,
                    input_index: 0,
                    event_index: 0,
                    event: GenerationEventKind::State {
                        state: GenerationState::Generating,
                    },
                })
                .is_ok()
            {}
            let result =
                cooperative_checkpoint_output(&ticket.request_id, GenerationState::Cancelled);
            let worker = std::thread::spawn(move || job.finish(Ok(result)));
            let deadline = Instant::now() + Duration::from_secs(3);
            let terminal = loop {
                let snapshot = control.snapshot();
                if let Some(terminal) = snapshot.authoritative_terminal {
                    break terminal;
                }
                assert!(
                    Instant::now() < deadline,
                    "owner did not select its terminal"
                );
                std::thread::yield_now();
            };
            assert_eq!(terminal.class, RequestTerminalClass::Cancelled);
            assert_eq!(
                control.snapshot().phase,
                crate::operation_registry::RequestPhase::Terminal
            );
            assert_eq!(handle.inner.requests.active_count(), 1);
            if drop_receiver {
                drop(ticket);
            } else {
                assert_eq!(
                    ticket.wait().expect("actual public delivery")[0].state,
                    GenerationState::Cancelled
                );
            }
            worker.join().expect("owner completed final send attempt");
            assert_eq!(
                control.snapshot().phase,
                crate::operation_registry::RequestPhase::Released
            );
            assert_eq!(
                control
                    .snapshot()
                    .authoritative_terminal
                    .expect("immutable final")
                    .class,
                RequestTerminalClass::Cancelled
            );
            assert_eq!(handle.inner.requests.active_count(), 0);
        }
    }

    #[test]
    fn live_occupancy_bounds_each_stream_and_reuses_only_vacant_slots() -> NativeResult<()> {
        let layout = StreamLayout::new(100, 2, 4);
        let mut occupied = std::collections::BTreeSet::new();
        let first = layout
            .vacant_slot(100, |slot| occupied.contains(&slot))?
            .expect("free independent stream");
        occupied.insert(first);
        let second = layout
            .vacant_slot(100, |slot| occupied.contains(&slot))?
            .expect("free independent stream");
        occupied.insert(second);
        assert_ne!(first, second);
        assert!(
            layout
                .vacant_slot(1, |slot| occupied.contains(&slot))?
                .is_none()
        );
        occupied.remove(&first);
        assert_eq!(
            layout.vacant_slot(100, |slot| occupied.contains(&slot))?,
            Some(first)
        );
        assert!(layout.vacant_slot(101, |_| false).is_err());
        assert!(layout.vacant_slot(usize::MAX, |_| false).is_err());
        Ok(())
    }

    #[test]
    fn batch_width_and_sequence_limits_are_both_admission_limits() -> NativeResult<()> {
        let layout = StreamLayout::new(100, 8, 2);
        assert_eq!(layout.vacant_slot(1, |_| false)?, Some(0));
        assert_eq!(layout.vacant_slot(1, |slot| slot == 0)?, Some(1));
        assert!(layout.vacant_slot(1, |_| true)?.is_none());
        assert!(
            StreamLayout::new(100, 0, 2)
                .vacant_slot(1, |_| false)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn worker_exit_closes_observers_after_retaining_queued_samples() -> NativeResult<()> {
        let hub = DecodeHub::default();
        let lifetime = hub.worker_lifetime();
        let observer = hub.subscribe()?;
        hub.publish(Vec::new())?;
        drop(lifetime);
        assert!(hub.subscribe().is_err());
        assert_eq!(
            observer
                .receive_timeout(Duration::ZERO)?
                .expect("queued decode sample retained")
                .ordinal(),
            1
        );
        assert_eq!(
            observer
                .receive_timeout(Duration::ZERO)
                .expect_err("joined worker closes observations")
                .code,
            NativeErrorCode::WorkerStopped
        );
        Ok(())
    }

    #[test]
    fn observers_are_bounded_and_dropped_receivers_release_their_slot() -> NativeResult<()> {
        let hub = DecodeHub::default();
        let first = hub.subscribe()?;
        let second = hub.subscribe()?;
        assert!(hub.subscribe().is_err());
        drop(second);
        let _replacement = hub.subscribe()?;
        for _ in 0..=OBSERVATION_CAPACITY {
            hub.publish(Vec::new())?;
        }
        assert_eq!(first.dropped_samples(), 1);
        assert_eq!(
            first
                .receive_timeout(Duration::ZERO)?
                .expect("queued decode sample retained")
                .ordinal(),
            1
        );
        Ok(())
    }
    #[test]
    fn public_cooperative_admission_rejects_text_and_chat_without_queuing() {
        let (mut handle, commands, _speculative) =
            crate::tests::test_admission_handle(crate::tests::admission_test_status(), 2);
        let inner = Arc::get_mut(&mut handle.inner).expect("unique fixture handle");
        Arc::get_mut(&mut inner.worker_identity)
            .expect("unique fixture worker")
            .kv_unified = false;
        let mut request = GenerationBatchRequest {
            request_id: "same-admission".into(),
            model_id: "model".into(),
            first_word_choices: None,
            media: Vec::new(),
            cases: vec![GenerationCase {
                case_id: "case".into(),
                input: GenerationInput::Completion {
                    prompts: vec![CompletionPrompt::Tokens {
                        token_ids: vec![1, 2],
                    }],
                },
                sampling: SamplingConfig {
                    max_tokens: 2,
                    ..Default::default()
                },
                cached_prefix: None,
            }],
        };
        let exact = request.cases[0].input.clone();
        for input in [
            GenerationInput::Completion {
                prompts: vec![CompletionPrompt::Text {
                    text: "hello".into(),
                    special_tokens: Default::default(),
                }],
            },
            GenerationInput::Chat {
                messages: vec![llama_native_types::ChatMessage {
                    role: llama_native_types::ChatRole::User,
                    content: "hello".into(),
                }],
                template: Default::default(),
            },
        ] {
            request.cases[0].input = input;
            let error = match handle.generate_cooperative(request.clone()) {
                Ok(_) => panic!("independent streams require exact tokens"),
                Err(error) => error,
            };
            assert_eq!(error.code, NativeErrorCode::UnsupportedParameter);
            assert!(
                commands.try_recv().is_err(),
                "unsupported input cannot queue native work"
            );
        }
        request.cases[0].input = exact;
        let ticket = handle
            .generate_cooperative(request)
            .expect("ordinary exact-token admission");
        let job = Job::from_command(commands.recv().expect("exact-token command"))
            .expect("actual cooperative job");
        drop(job);
        drop(ticket);
    }
}
