//! Opt-in request sharing on one resident owner thread.
//!
//! Each request has one uncached case, its own sampler, cancellation flag,
//! logical event sequence and completion channel. Physical KV slots are never
//! exposed as request-local sequence identities. No prefix is shared between
//! requests, even when their token prefixes happen to match.
use super::*;
use std::collections::BTreeMap;
use std::sync::{Weak, atomic::AtomicU64};

const MAX_LIVE: usize = 8;
const PREFILL_QUANTUM: usize = 64;
const MAX_PROMPT_BYTES: usize = 64 * 1024;
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
    subscribers: Vec<Subscriber>,
}

#[derive(Debug, Default)]
pub(super) struct DecodeHub {
    state: Mutex<HubState>,
}

impl DecodeHub {
    fn subscribe(&self) -> NativeResult<DecodeObserver> {
        let mut hub = self
            .state
            .lock()
            .map_err(|_| internal("decode observer registry poisoned"))?;
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
    /// Submit one independently owned, uncached case to the cooperative lane.
    /// Later cooperative requests may join while this request is decoding.
    /// Media, caller caches and out-of-band reasoning intervention are not
    /// supported on this lane; the ordinary generation APIs remain unchanged.
    pub fn generate_cooperative(
        &self,
        request: GenerationBatchRequest,
    ) -> NativeResult<GenerationTicket> {
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
                !token_ids.is_empty() && token_ids.len() <= MAX_PROMPT_BYTES
            }
            [CompletionPrompt::Text { text, .. }] => {
                !text.is_empty() && text.len() <= MAX_PROMPT_BYTES
            }
            _ => false,
        },
        GenerationInput::Chat { messages, .. } => {
            !messages.is_empty()
                && messages
                    .iter()
                    .try_fold(0_usize, |total, message| {
                        total.checked_add(message.content.len())
                    })
                    .is_some_and(|bytes| bytes <= MAX_PROMPT_BYTES)
        }
        GenerationInput::FillInMiddle { .. } => false,
    };
    if !bounded_input {
        return Err(unsupported(
            "cooperative prompt exceeds its bounded text/token contract",
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

/// Conservative reservation: the complete prompt plus all permitted growth.
/// No accounting credit is taken for an identical prefix in another request.
#[derive(Debug)]
struct Reservations {
    cells: usize,
    slots: usize,
    held: BTreeMap<usize, usize>,
    used: usize,
}

impl Reservations {
    fn new(cells: usize, sequences: usize, batch: usize) -> Self {
        Self {
            cells,
            slots: sequences.min(batch).min(MAX_LIVE),
            held: BTreeMap::new(),
            used: 0,
        }
    }

    fn reserve(&mut self, cells: usize) -> NativeResult<Option<usize>> {
        if cells == 0 || cells > self.cells || self.slots == 0 {
            return Err(NativeError::new(
                NativeErrorCode::PromptTooLarge,
                "cooperative request exceeds the resident capacity",
            ));
        }
        let Some(total) = self
            .used
            .checked_add(cells)
            .filter(|total| *total <= self.cells)
        else {
            return Ok(None);
        };
        let Some(slot) = (0..self.slots).find(|slot| !self.held.contains_key(slot)) else {
            return Ok(None);
        };
        self.held.insert(slot, cells);
        self.used = total;
        Ok(Some(slot))
    }

    fn release(&mut self, slot: usize) -> NativeResult<()> {
        let cells = self
            .held
            .remove(&slot)
            .ok_or_else(|| internal("cooperative reservation released twice"))?;
        self.used = self
            .used
            .checked_sub(cells)
            .ok_or_else(|| internal("cooperative reservation underflow"))?;
        Ok(())
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

    fn reject(self, error: NativeError) {
        if error.code == NativeErrorCode::Cancelled {
            self.cancel.store(true, Ordering::Release);
            emit_cancelled_case_events(&self.event_tx, &self.request);
            let _ = self.lease.cancel_queued();
        } else {
            emit_failed_case_events(&self.event_tx, &self.request);
        }
        let _ = self.result_tx.send(Err(error));
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
    ) -> std::result::Result<Self, (Job, NativeError)> {
        let prepared = (|| {
            validate_request(&job.request)?;
            let tokens = generation_case_tokens(model, &job.request.cases[0], 0)?;
            let cells = tokens
                .len()
                .checked_add(job.request.cases[0].sampling.max_tokens as usize)
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
            Err(error) => Err((job, error)),
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
    ) -> std::result::Result<Self, (Job, NativeError)> {
        let Pending { job, tokens, .. } = pending;
        if let Err(error) = begin_generation_command(
            &job.lease,
            WorkerCommandClass::Foreground,
            runtime.speculative,
        ) {
            return Err((job, error));
        }
        let _ = job.lease.progress(0);
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
        let value = GenerationEvent {
            request_id: self.job.request.request_id.clone(),
            branch_id: self.job.request.cases[0].case_id.clone(),
            // The shared physical slot is deliberately not the caller's index.
            sequence_id: 0,
            input_index: 0,
            event_index: self.event_index,
            event,
        };
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
        let _ = self.job.lease.progress(self.token_ids.len() as u64);
        Ok(())
    }

    fn finish(mut self, runtime: &Runtime<'_, '_>) {
        let result = self.completion(runtime);
        let _ = self.job.lease.completed_or_failed(result.is_ok());
        if result.is_err() {
            emit_failed_case_events(&self.job.event_tx, &self.job.request);
        }
        let _ = self.job.result_tx.send(result);
    }

    fn fail(self, error: NativeError) {
        let _ = self.job.lease.completed_or_failed(false);
        emit_failed_case_events(&self.job.event_tx, &self.job.request);
        let _ = self.job.result_tx.send(Err(error));
    }

    fn completion(&mut self, runtime: &Runtime<'_, '_>) -> NativeResult<GenerationCompletion> {
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
        self.emit(GenerationEventKind::State { state });
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
                None => verify_generation_batch_authority(
                    runtime.model,
                    self.job.request.clone(),
                    runtime.fingerprint.clone(),
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
                    runtime.artifacts,
                ),
            };
        Ok(match authority {
            Ok(evidence) => GenerationCompletion::verified(outputs, evidence),
            Err(error) => GenerationCompletion::authority_rejected(outputs, error),
        })
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
    reservations: Reservations,
    live: BTreeMap<usize, Sequence>,
    pending: Option<Pending>,
    prefill_cursor: usize,
}

pub(super) fn run(first: WorkerCommand, mut runtime: Runtime<'_, '_>) {
    let mut pool = Pool {
        reservations: Reservations::new(
            runtime.context.n_ctx() as usize,
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
                        match Pending::prepare(job, runtime.model, pool.reservations.cells) {
                            Ok(pending) => pool.pending = Some(pending),
                            Err((job, error)) => job.reject(error),
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
            } else if let Some(slot) = pool.reservations.reserve(pending.cells)? {
                match Sequence::start(pending, runtime) {
                    Ok(sequence) => {
                        pool.live.insert(slot, sequence);
                    }
                    Err((job, error)) => {
                        pool.reservations.release(slot)?;
                        job.reject(error);
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
        pool.reservations.release(slot)?;
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
    let mut row_count = 0usize;
    let mut logits = Vec::<(usize, i32)>::new();
    let mut members = BTreeMap::<usize, DecodeMember>::new();
    // Every decoding sequence gets its next token before any prefill work.
    for (slot, sequence) in &mut pool.live {
        if let Some(token) = sequence.pending_token.take() {
            batch
                .add(token, sequence.next_position as i32, &[*slot as i32], true)
                .map_err(|error| native_decode_error("cooperative generation batch", error))?;
            logits.push((*slot, row_count as i32));
            row_count += 1;
            sequence.next_position += 1;
            members.insert(
                *slot,
                DecodeMember {
                    request_id: sequence.job.request.request_id.clone(),
                    case_id: sequence.job.request.cases[0].case_id.clone(),
                    physical_sequence: *slot,
                    prefill_tokens: 0,
                    decode_tokens: 1,
                },
            );
        }
    }
    let mut remaining = capacity.saturating_sub(row_count).min(PREFILL_QUANTUM);
    let mut prefilling = pool
        .live
        .iter()
        .filter_map(|(slot, sequence)| {
            (sequence.prefilled < sequence.prompt.len()).then_some(*slot)
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
            .ok_or_else(|| internal("cooperative prefill slot disappeared"))?;
        let count = remaining.min(sequence.prompt.len() - sequence.prefilled);
        let end = sequence.prefilled + count;
        for position in sequence.prefilled..end {
            let last = position + 1 == sequence.prompt.len();
            batch
                .add(
                    sequence.prompt[position],
                    position as i32,
                    &[slot as i32],
                    last,
                )
                .map_err(|error| native_decode_error("cooperative prefill batch", error))?;
            if last {
                logits.push((slot, row_count as i32));
            }
            row_count += 1;
        }
        sequence.prefilled = end;
        sequence.next_position = end;
        remaining -= count;
        members.insert(
            slot,
            DecodeMember {
                request_id: sequence.job.request.request_id.clone(),
                case_id: sequence.job.request.cases[0].case_id.clone(),
                physical_sequence: slot,
                prefill_tokens: count,
                decode_tokens: 0,
            },
        );
        pool.prefill_cursor = (slot + 1) % MAX_LIVE;
    }
    if row_count == 0 {
        return Err(internal(
            "live cooperative requests produced no bounded decode work",
        ));
    }
    runtime
        .context
        .decode(&mut batch)
        .map_err(|error| native_decode_error("cooperative native decode", error))?;
    // This is the instrumentation boundary: successful real decode, not a
    // scheduler callback, fixture count, or inferred overlapping task lifetime.
    runtime
        .identity
        .cooperative_decodes
        .publish(members.into_values().collect())?;
    // Consume every live logit row before another decode can replace it.
    for (slot, row) in logits {
        let sequence = pool
            .live
            .get_mut(&slot)
            .ok_or_else(|| internal("cooperative logit owner disappeared"))?;
        sequence.sample(runtime.model, runtime.context, row)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_bound_aggregate_growth_and_reuse_only_released_slots() {
        let mut budget = Reservations::new(100, 4, 4);
        let first = budget.reserve(60).unwrap().unwrap();
        assert!(budget.reserve(41).unwrap().is_none());
        let second = budget.reserve(40).unwrap().unwrap();
        assert_ne!(first, second);
        assert!(budget.reserve(1).unwrap().is_none());
        budget.release(first).unwrap();
        assert_eq!(budget.reserve(60).unwrap(), Some(first));
        assert_eq!(budget.used, 100);
        assert!(budget.reserve(101).is_err());
        assert!(budget.reserve(usize::MAX).is_err());
        budget.release(second).unwrap();
        assert!(budget.release(second).is_err());
    }

    #[test]
    fn batch_width_and_sequence_limits_are_both_admission_limits() {
        let mut budget = Reservations::new(100, 8, 2);
        assert_eq!(budget.reserve(1).unwrap(), Some(0));
        assert_eq!(budget.reserve(1).unwrap(), Some(1));
        assert!(budget.reserve(1).unwrap().is_none());
        assert!(Reservations::new(100, 0, 2).reserve(1).is_err());
    }

    #[test]
    fn observers_are_bounded_and_dropped_receivers_release_their_slot() {
        let hub = DecodeHub::default();
        let first = hub.subscribe().unwrap();
        let second = hub.subscribe().unwrap();
        assert!(hub.subscribe().is_err());
        drop(second);
        let _replacement = hub.subscribe().unwrap();
        for _ in 0..=OBSERVATION_CAPACITY {
            hub.publish(Vec::new()).unwrap();
        }
        assert_eq!(first.dropped_samples(), 1);
        assert_eq!(
            first
                .receive_timeout(Duration::ZERO)
                .unwrap()
                .unwrap()
                .ordinal(),
            1
        );
    }
}
