use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use crossbeam_channel::{Receiver, Sender, TrySendError, after, bounded, never, select};
use serde::Serialize;
use thiserror::Error;

use crate::live_transcription::{
    IngressFailure, IngressMetrics, MAX_TURN_SAMPLES, PACKET_SAMPLES, PcmProducer,
};

const OUTPUT_RATE: u32 = 16_000;
const MAX_SAMPLES: usize = MAX_TURN_SAMPLES;
const INITIAL_CAPTURE_CAPACITY_SAMPLES: usize = OUTPUT_RATE as usize * 29;
const PRE_ROLL_SAMPLES: usize = OUTPUT_RATE as usize / 5;
// Match the resident model's 40 ms audio-projector frame. The capture owner
// can retain a much longer utterance, but every live or replay publication
// remains bounded to one frame and preserves source order.
const PREFILL_BATCH_SAMPLES: usize = OUTPUT_RATE as usize * 40 / 1_000;
const RETARGET_REPLAY_BATCH_SAMPLES: usize = PREFILL_BATCH_SAMPLES;
// Measure activity over the same 10 ms window used by conventional realtime
// speech detectors. One-millisecond RMS frames made ordinary microphone beds
// look perpetually modulated: tiny frame-to-frame fluctuations repeatedly
// reset the stationary-noise release and could hold a two-second utterance
// open for another thirteen seconds. Ten milliseconds remains comfortably
// inside Gemma's 40 ms audio frame while averaging away that false motion.
const VAD_FRAME_SAMPLES: usize = OUTPUT_RATE as usize / 100;
// CoreAudio can deliver a short route-opening transient at the beginning of a
// new stream. Ignore the first 40 ms for activity decisions while retaining it
// in the 200 ms pre-roll. The latest packaged trace opened on an 86-sample,
// barely-over-threshold device transient and spent six seconds generating for
// an empty room before the caller actually spoke.
const CAPTURE_SETTLE_SAMPLES: usize = OUTPUT_RATE as usize * 40 / 1_000;
const SPEECH_ATTACK_SAMPLES: usize = OUTPUT_RATE as usize * 4 / 1_000;
// After an utterance has closed, require a short sustained return before
// cancelling speculative work. Initial onset remains fast for quiet speech;
// this separate edge rejects speaker clicks and room impulses.
const SPEECH_RESUME_ATTACK_SAMPLES: usize = OUTPUT_RATE as usize * 40 / 1_000;
const SPEECH_RELEASE_SAMPLES: usize = OUTPUT_RATE as usize * 20 / 1_000;
// Preserve the 20 ms detector edge for exact timing, but do not transfer
// model ownership on one quiet analysis frame. Start speculation after a
// still-low 120 ms pause. If speech resumes, belay the speculative turn,
// retarget the retained PCM, and use the same boundary again. The native
// handoff now completes in roughly one audio quantum; learning an ordinary
// mid-sentence pause previously made the *real* endpoint wait as long as
// 800 ms, trading user-visible latency for compute we can cancel cheaply.
const SPECULATION_BASE_SILENCE_SAMPLES: usize = OUTPUT_RATE as usize * 120 / 1_000;
// Four milliseconds is deliberately enough to *open* the detector so quiet
// consonants are retained. A plausible spoken syllable normally supplies at
// least 80 ms of above-close-threshold material, but short acknowledgements
// are valid turns too. Classify them separately for telemetry while releasing
// them on the same adaptive speculative boundary. Waiting an extra 130 ms
// after we already have speech and sustained silence only lengthens the common
// "yes", "no", and greeting path; resumed speech is handled by the normal
// belay-and-retarget path.
const SPECULATION_MIN_VOICED_SAMPLES: usize = OUTPUT_RATE as usize * 80 / 1_000;
const STATIONARY_NOISE_RELEASE_SAMPLES: usize = OUTPUT_RATE as usize * 500 / 1_000;
const QUEUE_CAPACITY: usize = 16;
const CALLBACK_CHUNK_FRAMES: usize = 4_096;
// 256 frames is about 5.3 ms at the common 48 kHz CoreAudio rate. Ask for it
// explicitly so silence edges do not inherit an opaque device-default quantum;
// unsupported devices fall back to their default configuration.
const LOW_LATENCY_BUFFER_FRAMES: u32 = 256;
// A first-run macOS permission sheet may remain open while the user reads it.
// Keep ringing during that decision, but never let microphone authorization
// spill into consultant playback. A real callback, rather than stream.play(),
// is the readiness boundary because CoreAudio can accept the stream before
// permission or device routing has settled.
const PREPARE_FIRST_SAMPLE_TIMEOUT: Duration = Duration::from_secs(60);
// CoreAudio normally calls us every few milliseconds even when the room is
// completely silent. Once the first callback has arrived, a multi-second gap
// means the input route stopped advancing; leaving the silence gate armed in
// that state strands the entire call. Fail explicitly so the renderer can
// rebuild the capture once and then show the real error if the route remains
// unavailable.
const CAPTURE_CALLBACK_STALL_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) trait PcmSink: Send + Sync {
    fn push(&self, samples: &[f32]) -> Result<usize, String>;
    fn seal(&self) -> Result<(), String>;
}

impl PcmSink for mom_llama_runtime::VoiceAudioSession {
    fn push(&self, samples: &[f32]) -> Result<usize, String> {
        self.push_pcm(samples).map_err(|error| error.to_string())
    }

    fn seal(&self) -> Result<(), String> {
        self.seal_input()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
pub(crate) struct CaptureTarget {
    pub(crate) id: String,
    pub(crate) sink: Arc<dyn PcmSink>,
    pub(crate) cancel: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for CaptureTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureTarget")
            .field("id", &self.id)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CaptureEventKind {
    Started,
    FirstSample,
    VoiceOnset,
    SilenceOnset,
    SpeechResumed,
    SendReady,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CaptureEvent {
    pub(crate) schema: &'static str,
    pub(crate) capture: String,
    pub(crate) session: Option<String>,
    pub(crate) event: CaptureEventKind,
    /// Monotonic time on the capture owner at the point the event was emitted.
    /// This lets the renderer distinguish detector latency from event delivery.
    pub(crate) capture_elapsed_us: u64,
    pub(crate) emitted_at_unix_us: u64,
    pub(crate) sample_offset: u64,
    pub(crate) rms: f32,
    pub(crate) threshold: f32,
    pub(crate) close_threshold: f32,
    pub(crate) noise_floor: f32,
    pub(crate) silence_ms: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
}

type EventSink = Arc<dyn Fn(CaptureEvent) + Send + Sync>;

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct CaptureMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transcription_ingress: Option<IngressMetrics>,
    pub(crate) source_rate_hz: u32,
    pub(crate) requested_buffer_frames: u32,
    pub(crate) callbacks: u64,
    pub(crate) source_frames: u64,
    pub(crate) callback_frames_max: usize,
    pub(crate) output_samples: u64,
    pub(crate) vad_frames: u64,
    pub(crate) vad_settle_samples: u64,
    pub(crate) vad_voice_onsets: u64,
    pub(crate) vad_silence_onsets: u64,
    pub(crate) vad_deferred_silence_onsets: u64,
    pub(crate) vad_speech_resumes: u64,
    pub(crate) vad_latched_samples: u64,
    pub(crate) vad_voiced_evidence_samples: u64,
    pub(crate) vad_stationary_noise_releases: u64,
    pub(crate) vad_open_threshold_min: Option<f32>,
    pub(crate) vad_open_threshold_max: Option<f32>,
    pub(crate) vad_close_threshold_min: Option<f32>,
    pub(crate) vad_close_threshold_max: Option<f32>,
    pub(crate) vad_noise_floor_min: Option<f32>,
    pub(crate) vad_noise_floor_max: Option<f32>,
    pub(crate) prefill_pushes: u64,
    pub(crate) prefill_samples: u64,
    pub(crate) prefill_high_water_samples: usize,
    pub(crate) processing_cpu_us: u64,
    pub(crate) processing_max_us: u64,
    pub(crate) resample_cpu_us: u64,
    pub(crate) resample_max_us: u64,
    pub(crate) detector_cpu_us: u64,
    pub(crate) detector_max_us: u64,
    pub(crate) queue_delay_us: u64,
    pub(crate) queue_delay_max_us: u64,
    pub(crate) queue_high_water_chunks: usize,
    pub(crate) prefill_push_cpu_us: u64,
    pub(crate) prefill_push_max_us: u64,
    pub(crate) retained_copy_cpu_us: u64,
    pub(crate) retained_copy_max_us: u64,
    pub(crate) retained_only_samples: u64,
    pub(crate) speculation_prearms: u64,
    pub(crate) speculation_releases: u64,
    pub(crate) speculation_resume_cancels: u64,
    pub(crate) speculation_backoffs: u64,
    pub(crate) speculation_release_delay_samples_max: usize,
    pub(crate) speculation_short_utterance_releases: u64,
    pub(crate) input_seals: u64,
    pub(crate) input_seal_cpu_us: u64,
    pub(crate) input_seal_max_us: u64,
    pub(crate) finish_drain_us: u64,
    pub(crate) finish_flush_us: u64,
    pub(crate) wav_encode_us: u64,
    pub(crate) retained_samples: usize,
    pub(crate) first_sample_ms: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct RetargetMetrics {
    pub(crate) replay_samples: usize,
    pub(crate) replay_batches: u64,
    pub(crate) replay_push_cpu_us: u64,
    pub(crate) replay_push_max_us: u64,
    pub(crate) total_us: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct PrepareMetrics {
    pub(crate) source_rate_hz: u32,
    pub(crate) requested_buffer_frames: u32,
    pub(crate) first_sample_ms: u64,
}

#[derive(Debug)]
pub(crate) struct CaptureOutput {
    pub(crate) wav: Vec<u8>,
    pub(crate) heard_voice: bool,
    pub(crate) metrics: CaptureMetrics,
}

#[derive(Debug, Error)]
pub(crate) enum NativeVoiceCaptureError {
    #[error("a native voice capture is already active")]
    AlreadyRecording,
    #[error("there is no active native voice capture")]
    NotRecording,
    #[error("the active native voice capture does not match this request")]
    RecordingMismatch,
    #[error("no default microphone is available")]
    NoInputDevice,
    #[error("the default microphone configuration is unavailable: {0}")]
    DefaultConfiguration(String),
    #[error("the microphone reported an invalid configuration")]
    InvalidConfiguration,
    #[error("the microphone sample format is not supported: {0}")]
    UnsupportedSampleFormat(String),
    #[error("the microphone stream could not be created: {0}")]
    BuildStream(String),
    #[error("the microphone stream could not start: {0}")]
    StartStream(String),
    #[error("the microphone did not deliver audio after its stream started")]
    FirstSampleTimeout,
    #[error("the microphone stopped delivering audio while the call was active")]
    CallbackStall,
    #[error("the microphone stream failed while recording: {0}")]
    Stream(String),
    #[error("audio arrived faster than the bounded native capture could consume it")]
    QueueOverflow,
    #[error("the recorded voice exceeded the supported two-hour duration")]
    TooLong,
    #[error("the native voice capture controller is unavailable: {0}")]
    ControllerUnavailable(String),
    #[error("the native voice capture owner stopped unexpectedly")]
    OwnerStopped,
    #[error("the captured WAV exceeded its representable size")]
    WavSize,
    #[error("the native audio prefill target failed: {0}")]
    Prefill(String),
    #[error("native silence speculation is already armed for this capture")]
    SpeculationAlreadyArmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SilenceGateState {
    Armed,
    Released,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpeculationSilence {
    None,
    Plausible,
    DelayedShort,
}

fn speculation_silence(
    voice: &VoiceActivity,
    already_announced: bool,
    release_delay_samples: usize,
) -> SpeculationSilence {
    if already_announced || !voice.heard_voice {
        return SpeculationSilence::None;
    }
    if voice.utterance_voiced_samples >= SPECULATION_MIN_VOICED_SAMPLES
        && voice.silence_samples() >= release_delay_samples
    {
        SpeculationSilence::Plausible
    } else if !voice.voiced
        && voice.utterance_voiced_samples < SPECULATION_MIN_VOICED_SAMPLES
        && voice.silence_samples() >= release_delay_samples
    {
        SpeculationSilence::DelayedShort
    } else {
        SpeculationSilence::None
    }
}

pub(crate) struct CaptureSilenceGate {
    state: Mutex<SilenceGateState>,
    changed: std::sync::Condvar,
    cancel_request: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for CaptureSilenceGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureSilenceGate")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl CaptureSilenceGate {
    fn new(cancel_request: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            state: Mutex::new(SilenceGateState::Armed),
            changed: std::sync::Condvar::new(),
            cancel_request,
        }
    }

    pub(crate) fn wait(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "native silence gate is unavailable".to_string())?;
        while *state == SilenceGateState::Armed {
            state = self
                .changed
                .wait(state)
                .map_err(|_| "native silence gate is unavailable".to_string())?;
        }
        match *state {
            SilenceGateState::Released => Ok(()),
            SilenceGateState::Cancelled => Err("voice_speculation_aborted".to_string()),
            SilenceGateState::Armed => unreachable!("wait loop excludes armed state"),
        }
    }

    fn release(&self) -> bool {
        self.transition(SilenceGateState::Released)
    }

    fn cancel(&self) {
        let cancelled = self.state.lock().is_ok_and(|mut state| {
            if *state != SilenceGateState::Armed {
                return false;
            }
            *state = SilenceGateState::Cancelled;
            self.changed.notify_all();
            true
        });
        if cancelled {
            (self.cancel_request)();
        }
    }

    fn resume_speech(&self) {
        (self.cancel_request)();
    }

    fn transition(&self, next: SilenceGateState) -> bool {
        if let Ok(mut state) = self.state.lock()
            && *state == SilenceGateState::Armed
        {
            *state = next;
            self.changed.notify_all();
            return true;
        }
        false
    }
}

enum Command {
    Prepare {
        reply: Sender<Result<PrepareMetrics, NativeVoiceCaptureError>>,
    },
    Start {
        capture: String,
        transcription: Option<PcmProducer>,
        target: Option<CaptureTarget>,
        call: bool,
        events: EventSink,
        reply: Sender<Result<(), NativeVoiceCaptureError>>,
    },
    PausePrefill {
        capture: String,
        reply: Sender<Result<(), NativeVoiceCaptureError>>,
    },
    ArmSilence {
        capture: String,
        reply: Sender<Result<Arc<CaptureSilenceGate>, NativeVoiceCaptureError>>,
    },
    Retarget {
        capture: String,
        target: CaptureTarget,
        sample_offset: usize,
        reply: Sender<Result<RetargetMetrics, NativeVoiceCaptureError>>,
    },
    Stop {
        capture: String,
        reply: Sender<Result<CaptureOutput, NativeVoiceCaptureError>>,
    },
    Cancel {
        capture: String,
        reply: Sender<Result<CaptureMetrics, NativeVoiceCaptureError>>,
    },
    Shutdown {
        reply: Sender<()>,
    },
}

#[derive(Debug)]
struct Chunk {
    samples: Vec<f32>,
    queued_at: Instant,
}

struct Active {
    capture: String,
    // Owned by the physical capture, never by CaptureTarget or its replay cursor.
    transcription: Option<PcmProducer>,
    target: Option<CaptureTarget>,
    call: bool,
    events: EventSink,
    stream: Stream,
    audio_rx: Receiver<Chunk>,
    free_tx: Sender<Vec<f32>>,
    overflowed: Arc<AtomicBool>,
    stream_error: Arc<Mutex<Option<String>>>,
    resampler: Resampler,
    resampled: Vec<f32>,
    pre_roll: VecDeque<f32>,
    settle_samples_remaining: usize,
    samples: Vec<f32>,
    capture_too_long: bool,
    // First retained sample not yet published to the current model ring.
    // Keeping a cursor into `samples` avoids a second staging copy for every
    // microphone sample while preserving bounded 10 ms publications.
    prefill_cursor: usize,
    silence_gate: Option<Arc<CaptureSilenceGate>>,
    speculation_released: bool,
    speculation_release_delay_samples: usize,
    silence_announced: bool,
    voice: VoiceActivity,
    sent_ready: bool,
    terminal_error: Option<String>,
    started: Instant,
    last_callback: Instant,
    metrics: CaptureMetrics,
}

impl std::fmt::Debug for Active {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Active")
            .field("capture", &self.capture)
            .field("target", &self.target)
            .field("samples", &self.samples.len())
            .field("voice", &self.voice)
            .finish_non_exhaustive()
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        if let Some(gate) = self.silence_gate.take() {
            gate.cancel();
        }
    }
}

#[derive(Clone)]
pub(crate) struct NativeVoiceCapture {
    inner: Arc<NativeVoiceCaptureInner>,
}

struct NativeVoiceCaptureInner {
    command_tx: Sender<Command>,
    owner: Mutex<Option<JoinHandle<()>>>,
    initialization_error: Option<String>,
}

impl std::fmt::Debug for NativeVoiceCaptureInner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeVoiceCaptureInner")
            .field("initialization_error", &self.initialization_error)
            .finish_non_exhaustive()
    }
}

impl NativeVoiceCapture {
    pub(crate) fn new() -> Self {
        let (command_tx, command_rx) = bounded(8);
        let owner = thread::Builder::new()
            .name("mom-native-voice-capture".to_owned())
            .spawn(move || owner_loop(&command_rx));
        let (owner, initialization_error) = match owner {
            Ok(owner) => (Some(owner), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            inner: Arc::new(NativeVoiceCaptureInner {
                command_tx,
                owner: Mutex::new(owner),
                initialization_error,
            }),
        }
    }

    pub(crate) fn start(
        &self,
        capture: String,
        target: Option<CaptureTarget>,
        call: bool,
        events: EventSink,
    ) -> Result<(), NativeVoiceCaptureError> {
        self.start_with_transcription(capture, target, call, events, None)
    }

    // The current command deliberately calls start(), without activating this
    // staged path. The missing app_runtime owner must retain the reader's work
    // lease and bind its source receipt to the actual saved turn first.
    pub(crate) fn start_with_transcription(
        &self,
        capture: String,
        target: Option<CaptureTarget>,
        call: bool,
        events: EventSink,
        transcription: Option<PcmProducer>,
    ) -> Result<(), NativeVoiceCaptureError> {
        if transcription
            .as_ref()
            .is_some_and(|source| source.identity().capture != capture)
        {
            return Err(NativeVoiceCaptureError::RecordingMismatch);
        }
        self.request(|reply| Command::Start {
            capture,
            transcription,
            target,
            call,
            events,
            reply,
        })
    }

    pub(crate) fn prepare(&self) -> Result<PrepareMetrics, NativeVoiceCaptureError> {
        self.request(|reply| Command::Prepare { reply })
    }

    pub(crate) fn pause_prefill(&self, capture: String) -> Result<(), NativeVoiceCaptureError> {
        self.request(|reply| Command::PausePrefill { capture, reply })
    }

    pub(crate) fn arm_silence(
        &self,
        capture: String,
    ) -> Result<Arc<CaptureSilenceGate>, NativeVoiceCaptureError> {
        self.request(|reply| Command::ArmSilence { capture, reply })
    }

    pub(crate) fn retarget(
        &self,
        capture: String,
        target: CaptureTarget,
        sample_offset: usize,
    ) -> Result<RetargetMetrics, NativeVoiceCaptureError> {
        self.request(|reply| Command::Retarget {
            capture,
            target,
            sample_offset,
            reply,
        })
    }

    pub(crate) fn stop(&self, capture: String) -> Result<CaptureOutput, NativeVoiceCaptureError> {
        self.request(|reply| Command::Stop { capture, reply })
    }

    pub(crate) fn cancel(
        &self,
        capture: String,
    ) -> Result<CaptureMetrics, NativeVoiceCaptureError> {
        self.request(|reply| Command::Cancel { capture, reply })
    }

    fn request<T>(
        &self,
        command: impl FnOnce(Sender<Result<T, NativeVoiceCaptureError>>) -> Command,
    ) -> Result<T, NativeVoiceCaptureError> {
        self.ensure_initialized()?;
        let (tx, rx) = bounded(1);
        self.inner
            .command_tx
            .send(command(tx))
            .map_err(controller)?;
        rx.recv()
            .map_err(|_| NativeVoiceCaptureError::OwnerStopped)?
    }

    fn ensure_initialized(&self) -> Result<(), NativeVoiceCaptureError> {
        self.inner
            .initialization_error
            .as_ref()
            .map_or(Ok(()), |error| {
                Err(NativeVoiceCaptureError::ControllerUnavailable(
                    error.clone(),
                ))
            })
    }
}

impl Drop for NativeVoiceCaptureInner {
    fn drop(&mut self) {
        let Ok(mut owner) = self.owner.lock() else {
            return;
        };
        let Some(owner) = owner.take() else { return };
        let (tx, rx) = bounded(1);
        let _ = self.command_tx.send(Command::Shutdown { reply: tx });
        let _ = rx.recv_timeout(Duration::from_secs(2));
        let _ = owner.join();
    }
}

fn controller(error: impl ToString) -> NativeVoiceCaptureError {
    NativeVoiceCaptureError::ControllerUnavailable(error.to_string())
}

fn owner_loop(commands: &Receiver<Command>) {
    let mut active: Option<Active> = None;
    loop {
        let Some(capture) = active.as_mut() else {
            match commands.recv() {
                Ok(Command::Prepare { reply }) => {
                    let _ = reply.send(prepare());
                }
                Ok(Command::Start {
                    capture,
                    transcription,
                    target,
                    call,
                    events,
                    reply,
                }) => match begin(capture, target, call, events, transcription) {
                    Ok(next) => {
                        active = Some(next);
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                },
                Ok(Command::PausePrefill { reply, .. }) => {
                    let _ = reply.send(Err(NativeVoiceCaptureError::NotRecording));
                }
                Ok(Command::ArmSilence { reply, .. }) => {
                    let _ = reply.send(Err(NativeVoiceCaptureError::NotRecording));
                }
                Ok(Command::Retarget { reply, .. }) => {
                    let _ = reply.send(Err(NativeVoiceCaptureError::NotRecording));
                }
                Ok(Command::Cancel { reply, .. }) => {
                    let _ = reply.send(Err(NativeVoiceCaptureError::NotRecording));
                }
                Ok(Command::Stop { reply, .. }) => {
                    let _ = reply.send(Err(NativeVoiceCaptureError::NotRecording));
                }
                Ok(Command::Shutdown { reply }) => {
                    let _ = reply.send(());
                    return;
                }
                Err(_) => return,
            }
            continue;
        };
        let first_sample_timeout = if capture.metrics.callbacks == 0 {
            after(Duration::from_secs(3).saturating_sub(capture.started.elapsed()))
        } else {
            never()
        };
        let callback_stall_timeout = if capture.metrics.callbacks > 0 {
            after(CAPTURE_CALLBACK_STALL_TIMEOUT.saturating_sub(capture.last_callback.elapsed()))
        } else {
            never()
        };
        select! {
            recv(commands) -> command => match command {
                Ok(Command::Prepare { reply }) => { let _ = reply.send(Err(NativeVoiceCaptureError::AlreadyRecording)); }
                Ok(Command::Start { reply, .. }) => { let _ = reply.send(Err(NativeVoiceCaptureError::AlreadyRecording)); }
                Ok(Command::PausePrefill { capture: id, reply }) => {
                    let result = capture.matches(&id).and_then(|()| capture.pause_prefill());
                    let _ = reply.send(result);
                }
                Ok(Command::ArmSilence { capture: id, reply }) => {
                    let result = capture.matches(&id).and_then(|()| capture.arm_silence());
                    let _ = reply.send(result);
                }
                Ok(Command::Retarget { capture: id, target, sample_offset, reply }) => {
                    let result = capture.matches(&id).and_then(|()| capture.retarget(target, sample_offset));
                    let _ = reply.send(result);
                }
                Ok(Command::Stop { capture: id, reply }) => {
                    if capture.capture != id { let _ = reply.send(Err(NativeVoiceCaptureError::RecordingMismatch)); continue; }
                    let capture = active.take().expect("matched active capture");
                    let _ = reply.send(finish(capture));
                }
                Ok(Command::Cancel { capture: id, reply }) => {
                    if capture.capture != id { let _ = reply.send(Err(NativeVoiceCaptureError::RecordingMismatch)); continue; }
                    let capture = active.take().expect("matched active capture");
                    let metrics = capture.metrics.clone();
                    drop(capture);
                    let _ = reply.send(Ok(metrics));
                }
                Ok(Command::Shutdown { reply }) => { drop(active.take()); let _ = reply.send(()); return; }
                Err(_) => return,
            },
            recv(capture.audio_rx) -> chunk => if let Ok(chunk) = chunk { capture.push(chunk); },
            recv(first_sample_timeout) -> _ => {
                let capture = active.take().expect("first-sample timeout requires active capture");
                let error = first_sample_error(&capture);
                capture.emit(CaptureEventKind::Failed, 0.0, Some(error.to_string()));
                drop(capture);
            },
            recv(callback_stall_timeout) -> _ => {
                let capture = active.take().expect("callback-stall timeout requires active capture");
                let error = NativeVoiceCaptureError::CallbackStall;
                capture.emit(CaptureEventKind::Failed, 0.0, Some(error.to_string()));
                drop(capture);
            }
        }
    }
}

fn prepare() -> Result<PrepareMetrics, NativeVoiceCaptureError> {
    let mut active = begin(
        "microphone-preflight".to_owned(),
        None,
        true,
        Arc::new(|_| {}),
        None,
    )?;
    let first = active
        .audio_rx
        .recv_timeout(PREPARE_FIRST_SAMPLE_TIMEOUT)
        .map_err(|_| first_sample_error(&active))?;
    active.push(first);
    active
        .stream
        .pause()
        .map_err(|error| NativeVoiceCaptureError::Stream(error.to_string()))?;
    Ok(PrepareMetrics {
        source_rate_hz: active.metrics.source_rate_hz,
        requested_buffer_frames: active.metrics.requested_buffer_frames,
        first_sample_ms: active.metrics.first_sample_ms,
    })
}

impl Active {
    fn matches(&self, capture: &str) -> Result<(), NativeVoiceCaptureError> {
        (self.capture == capture)
            .then_some(())
            .ok_or(NativeVoiceCaptureError::RecordingMismatch)
    }

    fn push(&mut self, mut chunk: Chunk) {
        let processing = Instant::now();
        self.last_callback = processing;
        if self.metrics.callbacks == 0 {
            self.metrics.first_sample_ms = self.started.elapsed().as_millis() as u64;
            self.emit(CaptureEventKind::FirstSample, 0.0, None);
        }
        let queue_delay_us = processing
            .saturating_duration_since(chunk.queued_at)
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        self.metrics.queue_delay_us = self.metrics.queue_delay_us.saturating_add(queue_delay_us);
        self.metrics.queue_delay_max_us = self.metrics.queue_delay_max_us.max(queue_delay_us);
        self.metrics.queue_high_water_chunks = self
            .metrics
            .queue_high_water_chunks
            .max(self.audio_rx.len().saturating_add(1));
        self.metrics.callbacks += 1;
        self.metrics.source_frames += chunk.samples.len() as u64;
        self.metrics.callback_frames_max =
            self.metrics.callback_frames_max.max(chunk.samples.len());
        self.resampled.clear();
        let resample_started = Instant::now();
        self.resampler.push(&chunk.samples, &mut self.resampled);
        let resample_us = resample_started
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        self.metrics.resample_cpu_us = self.metrics.resample_cpu_us.saturating_add(resample_us);
        self.metrics.resample_max_us = self.metrics.resample_max_us.max(resample_us);
        self.metrics.output_samples += self.resampled.len() as u64;
        let pcm = std::mem::take(&mut self.resampled);
        let detector_started = Instant::now();
        self.process_pcm(&pcm);
        let detector_us = detector_started
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        self.metrics.detector_cpu_us = self.metrics.detector_cpu_us.saturating_add(detector_us);
        self.metrics.detector_max_us = self.metrics.detector_max_us.max(detector_us);
        self.resampled = pcm;
        self.resampled.clear();
        let processing_us = processing.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        self.metrics.processing_cpu_us =
            self.metrics.processing_cpu_us.saturating_add(processing_us);
        self.metrics.processing_max_us = self.metrics.processing_max_us.max(processing_us);
        chunk.samples.clear();
        let _ = self.free_tx.try_send(chunk.samples);
    }

    fn process_pcm(&mut self, pcm: &[f32]) {
        for frame in pcm.chunks(VAD_FRAME_SAMPLES) {
            self.metrics.vad_frames += 1;
            if retain_settling_frame(
                &mut self.settle_samples_remaining,
                &mut self.pre_roll,
                frame,
            ) {
                self.metrics.vad_settle_samples = self
                    .metrics
                    .vad_settle_samples
                    .saturating_add(frame.len() as u64);
                continue;
            }
            let power = frame.iter().map(|sample| sample * sample).sum::<f32>();
            let rms = (power / frame.len().max(1) as f32).sqrt();
            // The detector runs on 10 ms frames. Snapshot the two transition
            // bits directly; cloning VoiceActivity also cloned its heap-backed
            // streaming percentile state one thousand times per second.
            let prior_heard_voice = self.voice.heard_voice;
            let prior_voiced = self.voice.voiced;
            self.voice.update(rms, frame.len());
            let onset = !prior_heard_voice && self.voice.voiced;
            let silence = prior_voiced && !self.voice.voiced;
            let resumed = prior_heard_voice && !prior_voiced && self.voice.voiced;
            if self.voice.voiced {
                self.metrics.vad_latched_samples = self
                    .metrics
                    .vad_latched_samples
                    .saturating_add(frame.len() as u64);
            }
            if self.voice.voiced && rms >= self.voice.close_threshold {
                self.metrics.vad_voiced_evidence_samples = self
                    .metrics
                    .vad_voiced_evidence_samples
                    .saturating_add(frame.len() as u64);
            }
            if silence && self.voice.stationary_samples >= STATIONARY_NOISE_RELEASE_SAMPLES {
                self.metrics.vad_stationary_noise_releases =
                    self.metrics.vad_stationary_noise_releases.saturating_add(1);
            }
            update_metric_range(
                &mut self.metrics.vad_open_threshold_min,
                &mut self.metrics.vad_open_threshold_max,
                self.voice.threshold,
            );
            update_metric_range(
                &mut self.metrics.vad_close_threshold_min,
                &mut self.metrics.vad_close_threshold_max,
                self.voice.close_threshold,
            );
            update_metric_range(
                &mut self.metrics.vad_noise_floor_min,
                &mut self.metrics.vad_noise_floor_max,
                self.voice.noise_floor,
            );
            self.metrics.vad_voice_onsets += u64::from(onset);
            self.metrics.vad_silence_onsets += u64::from(silence);
            self.metrics.vad_speech_resumes += u64::from(resumed);

            if onset {
                self.append_pre_roll();
            }
            if self.voice.heard_voice {
                self.append_content(frame, self.voice.voiced || self.voice.silence_ms <= 600.0);
            } else {
                for sample in frame {
                    self.pre_roll.push_back(*sample);
                }
                while self.pre_roll.len() > PRE_ROLL_SAMPLES {
                    self.pre_roll.pop_front();
                }
            }

            if onset {
                self.emit(CaptureEventKind::VoiceOnset, rms, None);
            }
            let silence_decision = speculation_silence(
                &self.voice,
                self.silence_announced,
                self.speculation_release_delay_samples,
            );
            if silence && silence_decision == SpeculationSilence::None {
                self.metrics.vad_deferred_silence_onsets =
                    self.metrics.vad_deferred_silence_onsets.saturating_add(1);
            }
            if silence_decision != SpeculationSilence::None {
                self.silence_announced = true;
                if silence_decision == SpeculationSilence::DelayedShort {
                    self.metrics.speculation_short_utterance_releases = self
                        .metrics
                        .speculation_short_utterance_releases
                        .saturating_add(1);
                }
                let _ = self.flush_prefill();
                // Snapshot detector time and session identity before the
                // ownership transfer, then let the model tail run before
                // publishing presentation telemetry.
                let event = self.capture_event(CaptureEventKind::SilenceOnset, rms, None);
                if let Err(error) = self.seal_and_release_silence_gate() {
                    self.terminal_error = Some(error.to_string());
                    self.emit(CaptureEventKind::Failed, rms, Some(error.to_string()));
                } else {
                    (self.events)(event);
                }
            }
            if resumed {
                self.sent_ready = false;
                self.silence_announced = false;
                // A sub-syllabic gap never transferred ownership to decode;
                // keep the already-prefilled session and its ring attached.
                // Only belay a speculation that actually crossed silence.
                let cancelled_speculation = if self.speculation_released {
                    if let Some(gate) = self.silence_gate.take() {
                        gate.resume_speech();
                        self.speculation_released = false;
                        self.metrics.speculation_resume_cancels =
                            self.metrics.speculation_resume_cancels.saturating_add(1);
                        self.speculation_release_delay_samples = SPECULATION_BASE_SILENCE_SAMPLES;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if cancelled_speculation {
                    self.emit(CaptureEventKind::SpeechResumed, rms, None);
                }
            }
            if !self.call
                && self.voice.heard_voice
                && !self.voice.voiced
                && self.voice.silence_ms >= 900.0
                && !self.sent_ready
            {
                self.sent_ready = true;
                self.emit(CaptureEventKind::SendReady, rms, None);
            }
        }
    }

    fn append_pre_roll(&mut self) {
        let count = self
            .pre_roll
            .len()
            .min(MAX_SAMPLES.saturating_sub(self.samples.len()));
        if count < self.pre_roll.len() {
            self.mark_capture_too_long();
            return;
        }
        let start = self.samples.len();
        let copy_started = Instant::now();
        self.samples.extend(self.pre_roll.drain(..count));
        self.publish_transcription();
        self.record_retained_copy(copy_started);
        self.pre_roll.clear();
        if self.target.is_some() && self.terminal_error.is_none() {
            debug_assert!(self.prefill_cursor <= start);
            if self.samples.len().saturating_sub(self.prefill_cursor) >= PREFILL_BATCH_SAMPLES {
                let _ = self.flush_prefill();
            }
        }
    }

    fn append_content(&mut self, samples: &[f32], prefill: bool) {
        if self.capture_too_long {
            return;
        }
        if !prefill && self.target.is_some() && self.terminal_error.is_none() {
            // Publish everything through the content boundary *before* adding
            // this retention-only tail. The old order appended a 1 ms silence
            // frame, flushed it, and repeated for every detector frame after
            // 600 ms: a 29-second call produced 22,209 model-ring writes and
            // 28 meaningless projector blocks. Keep the tail in the archive
            // for an exact resumed-speech replay, but do not feed it to the
            // current speculative decode.
            let _ = self.flush_prefill();
        }
        let available = MAX_SAMPLES.saturating_sub(self.samples.len());
        if samples.len() > available {
            self.mark_capture_too_long();
            return;
        }
        let copy_started = Instant::now();
        self.samples.extend_from_slice(samples);
        self.publish_transcription();
        self.record_retained_copy(copy_started);
        if !prefill {
            self.metrics.retained_only_samples = self
                .metrics
                .retained_only_samples
                .saturating_add(samples.len() as u64);
        }
        if prefill && self.target.is_some() && self.terminal_error.is_none() {
            if self.samples.len().saturating_sub(self.prefill_cursor) >= PREFILL_BATCH_SAMPLES {
                let _ = self.flush_prefill();
            }
        } else if !prefill && self.target.is_some() && self.terminal_error.is_none() {
            // Exclude the retained tail from this session. A later retarget
            // uses its authoritative checkpoint offset and replays from
            // `samples` if speech resumes.
            self.prefill_cursor = self.samples.len();
        }
    }

    fn mark_capture_too_long(&mut self) {
        if self.capture_too_long {
            return;
        }
        self.capture_too_long = true;
        if let Some(source) = &mut self.transcription {
            source.reject(IngressFailure::TooLong);
        }
        self.emit(
            CaptureEventKind::Failed,
            0.0,
            Some(NativeVoiceCaptureError::TooLong.to_string()),
        );
    }

    fn publish_transcription(&mut self) {
        if let Some(source) = &mut self.transcription {
            // Transcription failure is isolated: preserve audio and Gemma work.
            // The reader observes the sticky failure, never a partial final.
            let _ = publish_transcription_retained(source, &self.samples, false);
            self.metrics.transcription_ingress = Some(source.metrics());
        }
    }

    fn record_retained_copy(&mut self, started: Instant) {
        let elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        self.metrics.retained_copy_cpu_us =
            self.metrics.retained_copy_cpu_us.saturating_add(elapsed_us);
        self.metrics.retained_copy_max_us = self.metrics.retained_copy_max_us.max(elapsed_us);
    }

    fn flush_prefill(&mut self) -> Result<(), NativeVoiceCaptureError> {
        let Some(target) = self.target.as_ref() else {
            return Ok(());
        };
        loop {
            let publication =
                match publish_retained(&self.samples, &mut self.prefill_cursor, target) {
                    Ok(publication) => publication,
                    Err(error) => {
                        self.terminal_error = Some(error.clone());
                        self.emit(CaptureEventKind::Failed, 0.0, Some(error.clone()));
                        return Err(NativeVoiceCaptureError::Prefill(error));
                    }
                };
            let Some(publication) = publication else {
                break;
            };
            let sample_count = publication.sample_count;
            let push_us = publication.push_us;
            self.metrics.prefill_push_cpu_us =
                self.metrics.prefill_push_cpu_us.saturating_add(push_us);
            self.metrics.prefill_push_max_us = self.metrics.prefill_push_max_us.max(push_us);
            self.metrics.prefill_pushes += 1;
            self.metrics.prefill_samples += sample_count as u64;
            self.metrics.prefill_high_water_samples = self
                .metrics
                .prefill_high_water_samples
                .max(publication.buffered);
        }
        Ok(())
    }

    fn pause_prefill(&mut self) -> Result<(), NativeVoiceCaptureError> {
        self.flush_prefill()?;
        self.target = None;
        Ok(())
    }

    fn arm_silence(&mut self) -> Result<Arc<CaptureSilenceGate>, NativeVoiceCaptureError> {
        if self.silence_gate.is_some() {
            return Err(NativeVoiceCaptureError::SpeculationAlreadyArmed);
        }
        let cancel_request = self
            .target
            .as_ref()
            .map(|target| Arc::clone(&target.cancel))
            .ok_or_else(|| {
                NativeVoiceCaptureError::Prefill(
                    "native silence speculation has no attached model session".to_string(),
                )
            })?;
        let gate = Arc::new(CaptureSilenceGate::new(cancel_request));
        self.silence_gate = Some(Arc::clone(&gate));
        self.speculation_released = false;
        self.metrics.speculation_prearms = self.metrics.speculation_prearms.saturating_add(1);
        if speculation_silence(
            &self.voice,
            self.silence_announced,
            self.speculation_release_delay_samples,
        ) != SpeculationSilence::None
        {
            if let Err(error) = self.flush_prefill() {
                self.silence_gate.take();
                gate.cancel();
                return Err(error);
            }
            self.seal_and_release_silence_gate()?;
        }
        Ok(gate)
    }

    fn seal_and_release_silence_gate(&mut self) -> Result<(), NativeVoiceCaptureError> {
        if let Some(gate) = self.silence_gate.as_ref().map(Arc::clone) {
            // This is the ownership boundary: after release the speculative
            // worker owns the current model session. Capture keeps retaining
            // PCM so resumed speech can be replayed into a replacement.
            if let Some(target) = self.target.as_ref() {
                let started = Instant::now();
                if let Err(error) = target.sink.seal() {
                    self.target = None;
                    self.silence_gate = None;
                    gate.cancel();
                    return Err(NativeVoiceCaptureError::Prefill(error));
                }
                let elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
                self.metrics.input_seals = self.metrics.input_seals.saturating_add(1);
                self.metrics.input_seal_cpu_us =
                    self.metrics.input_seal_cpu_us.saturating_add(elapsed_us);
                self.metrics.input_seal_max_us = self.metrics.input_seal_max_us.max(elapsed_us);
            }
            self.target = None;
            if gate.release() {
                self.speculation_released = true;
                self.metrics.speculation_releases =
                    self.metrics.speculation_releases.saturating_add(1);
                self.metrics.speculation_release_delay_samples_max = self
                    .metrics
                    .speculation_release_delay_samples_max
                    .max(self.speculation_release_delay_samples);
            }
        }
        Ok(())
    }

    fn retarget(
        &mut self,
        target: CaptureTarget,
        sample_offset: usize,
    ) -> Result<RetargetMetrics, NativeVoiceCaptureError> {
        let started = Instant::now();
        // Capture and voice-session startup race independently. If speech
        // resumes while speculation is still awaiting those futures, the
        // renderer may ask to attach the session that is already receiving
        // PCM. Replaying from zero into that same live ring duplicates audio.
        if target_already_attached(self.target.as_ref(), &target) {
            return Ok(RetargetMetrics {
                total_us: started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
                ..RetargetMetrics::default()
            });
        }
        let replay_samples = retained_replay(&self.samples, sample_offset)?;
        let mut metrics = RetargetMetrics {
            replay_samples: replay_samples.len(),
            ..RetargetMetrics::default()
        };
        self.target = Some(target.clone());
        self.terminal_error = None;
        for chunk in replay_samples.chunks(RETARGET_REPLAY_BATCH_SAMPLES) {
            let push_started = Instant::now();
            let buffered = target
                .sink
                .push(chunk)
                .map_err(NativeVoiceCaptureError::Prefill)?;
            let push_us = push_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
            metrics.replay_batches = metrics.replay_batches.saturating_add(1);
            metrics.replay_push_cpu_us = metrics.replay_push_cpu_us.saturating_add(push_us);
            metrics.replay_push_max_us = metrics.replay_push_max_us.max(push_us);
            self.metrics.prefill_push_cpu_us =
                self.metrics.prefill_push_cpu_us.saturating_add(push_us);
            self.metrics.prefill_push_max_us = self.metrics.prefill_push_max_us.max(push_us);
            self.metrics.prefill_pushes += 1;
            self.metrics.prefill_samples += chunk.len() as u64;
            self.metrics.prefill_high_water_samples =
                self.metrics.prefill_high_water_samples.max(buffered);
        }
        self.prefill_cursor = self.samples.len();
        metrics.total_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        Ok(metrics)
    }

    fn capture_event(
        &self,
        event: CaptureEventKind,
        rms: f32,
        message: Option<String>,
    ) -> CaptureEvent {
        CaptureEvent {
            schema: "mom_llama.native_voice_capture.v1",
            capture: self.capture.clone(),
            session: self.target.as_ref().map(|target| target.id.clone()),
            event,
            capture_elapsed_us: self.started.elapsed().as_micros() as u64,
            emitted_at_unix_us: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| {
                    elapsed.as_micros().min(u128::from(u64::MAX)) as u64
                }),
            sample_offset: self.samples.len() as u64,
            rms,
            threshold: self.voice.threshold,
            close_threshold: self.voice.close_threshold,
            noise_floor: self.voice.noise_floor,
            silence_ms: self.voice.silence_ms,
            message,
        }
    }

    fn emit(&self, event: CaptureEventKind, rms: f32, message: Option<String>) {
        (self.events)(self.capture_event(event, rms, message));
    }
}

fn retain_settling_frame(
    remaining: &mut usize,
    pre_roll: &mut VecDeque<f32>,
    frame: &[f32],
) -> bool {
    if *remaining == 0 {
        return false;
    }
    *remaining = remaining.saturating_sub(frame.len());
    pre_roll.extend(frame.iter().copied());
    while pre_roll.len() > PRE_ROLL_SAMPLES {
        pre_roll.pop_front();
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RetainedPublication {
    sample_count: usize,
    buffered: usize,
    push_us: u64,
}

fn target_already_attached(current: Option<&CaptureTarget>, next: &CaptureTarget) -> bool {
    current.is_some_and(|current| current.id == next.id)
}

fn publish_retained(
    samples: &[f32],
    cursor: &mut usize,
    target: &CaptureTarget,
) -> Result<Option<RetainedPublication>, String> {
    let end = samples
        .len()
        .min((*cursor).saturating_add(PREFILL_BATCH_SAMPLES));
    if *cursor >= end {
        return Ok(None);
    }
    let start = *cursor;
    let push_started = Instant::now();
    let buffered = target.sink.push(&samples[start..end])?;
    let push_us = push_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    // Advance only after the sink acknowledges the complete borrowed slice.
    // A rejected publication therefore remains retryable and cannot drop PCM.
    *cursor = end;
    Ok(Some(RetainedPublication {
        sample_count: end - start,
        buffered,
        push_us,
    }))
}

fn publish_transcription_retained(
    producer: &mut PcmProducer,
    samples: &[f32],
    flush: bool,
) -> Result<(), IngressFailure> {
    if producer.sample_offset() > samples.len() {
        producer.reject(IngressFailure::SourceOrder);
        return Err(IngressFailure::SourceOrder);
    }
    while producer.sample_offset() < samples.len() {
        if !flush && samples.len() - producer.sample_offset() < PACKET_SAMPLES {
            break;
        }
        let start = producer.sample_offset();
        let end = (start + PACKET_SAMPLES).min(samples.len());
        producer.append_at(start, &samples[start..end])?;
    }
    Ok(())
}

fn retained_replay(
    samples: &[f32],
    sample_offset: usize,
) -> Result<&[f32], NativeVoiceCaptureError> {
    samples.get(sample_offset..).ok_or_else(|| {
        NativeVoiceCaptureError::Prefill(format!(
            "resident audio checkpoint claimed {sample_offset} samples, but capture retained only {}",
            samples.len()
        ))
    })
}

fn begin(
    capture: String,
    target: Option<CaptureTarget>,
    call: bool,
    events: EventSink,
    transcription: Option<PcmProducer>,
) -> Result<Active, NativeVoiceCaptureError> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or(NativeVoiceCaptureError::NoInputDevice)?;
    let supported = device
        .default_input_config()
        .map_err(|error| NativeVoiceCaptureError::DefaultConfiguration(error.to_string()))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels;
    let source_rate = config.sample_rate.0;
    if channels == 0 || source_rate == 0 {
        return Err(NativeVoiceCaptureError::InvalidConfiguration);
    }
    let (audio_tx, audio_rx) = bounded(QUEUE_CAPACITY);
    let (free_tx, free_rx) = bounded(QUEUE_CAPACITY);
    for _ in 0..QUEUE_CAPACITY {
        free_tx
            .send(Vec::with_capacity(CALLBACK_CHUNK_FRAMES))
            .map_err(controller)?;
    }
    let overflowed = Arc::new(AtomicBool::new(false));
    let stream_error = Arc::new(Mutex::new(None));
    let low_latency_config = preferred_input_config(&config);
    let (stream, requested_buffer_frames) = match build_stream(
        &device,
        &low_latency_config,
        format,
        channels,
        &audio_tx,
        &free_rx,
        &overflowed,
        &stream_error,
    ) {
        Ok(stream) => (stream, LOW_LATENCY_BUFFER_FRAMES),
        Err(_) => (
            build_stream(
                &device,
                &config,
                format,
                channels,
                &audio_tx,
                &free_rx,
                &overflowed,
                &stream_error,
            )?,
            0,
        ),
    };
    let started = Instant::now();
    stream
        .play()
        .map_err(|error| NativeVoiceCaptureError::StartStream(error.to_string()))?;
    let active = Active {
        capture,
        transcription,
        target,
        call,
        events,
        stream,
        audio_rx,
        free_tx,
        overflowed,
        stream_error,
        resampler: Resampler::new(source_rate, OUTPUT_RATE),
        resampled: Vec::with_capacity(CALLBACK_CHUNK_FRAMES),
        pre_roll: VecDeque::with_capacity(PRE_ROLL_SAMPLES),
        settle_samples_remaining: CAPTURE_SETTLE_SAMPLES,
        samples: Vec::with_capacity(INITIAL_CAPTURE_CAPACITY_SAMPLES),
        capture_too_long: false,
        prefill_cursor: 0,
        silence_gate: None,
        speculation_released: false,
        speculation_release_delay_samples: SPECULATION_BASE_SILENCE_SAMPLES,
        silence_announced: false,
        voice: VoiceActivity::default(),
        sent_ready: false,
        terminal_error: None,
        started,
        last_callback: started,
        metrics: CaptureMetrics {
            source_rate_hz: source_rate,
            requested_buffer_frames,
            ..CaptureMetrics::default()
        },
    };
    active.emit(CaptureEventKind::Started, 0.0, None);
    Ok(active)
}

fn preferred_input_config(config: &StreamConfig) -> StreamConfig {
    let mut preferred = config.clone();
    preferred.buffer_size = BufferSize::Fixed(LOW_LATENCY_BUFFER_FRAMES);
    preferred
}

#[allow(clippy::too_many_arguments)]
fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    channels: u16,
    audio_tx: &Sender<Chunk>,
    free_rx: &Receiver<Vec<f32>>,
    overflowed: &Arc<AtomicBool>,
    stream_error: &Arc<Mutex<Option<String>>>,
) -> Result<Stream, NativeVoiceCaptureError> {
    macro_rules! stream {
        ($ty:ty) => {{
            let tx = audio_tx.clone();
            let free = free_rx.clone();
            let full = Arc::clone(overflowed);
            let errors = Arc::clone(stream_error);
            device.build_input_stream::<$ty, _, _>(
                config,
                move |data, _| capture_callback(data, channels, &tx, &free, &full),
                move |error| store_stream_error(&errors, error.to_string()),
                None,
            )
        }};
    }
    let stream = match format {
        SampleFormat::I8 => stream!(i8),
        SampleFormat::I16 => stream!(i16),
        SampleFormat::I24 => stream!(cpal::I24),
        SampleFormat::I32 => stream!(i32),
        SampleFormat::I64 => stream!(i64),
        SampleFormat::U8 => stream!(u8),
        SampleFormat::U16 => stream!(u16),
        SampleFormat::U32 => stream!(u32),
        SampleFormat::U64 => stream!(u64),
        SampleFormat::F32 => stream!(f32),
        SampleFormat::F64 => stream!(f64),
        _ => {
            return Err(NativeVoiceCaptureError::UnsupportedSampleFormat(
                format.to_string(),
            ));
        }
    };
    stream.map_err(|error| NativeVoiceCaptureError::BuildStream(error.to_string()))
}

fn capture_callback<T>(
    data: &[T],
    channels: u16,
    tx: &Sender<Chunk>,
    free: &Receiver<Vec<f32>>,
    overflowed: &AtomicBool,
) where
    T: Sample + SizedSample + Copy,
    f32: FromSample<T>,
{
    if overflowed.load(Ordering::Relaxed) {
        return;
    }
    let channel_count = usize::from(channels);
    for block in data.chunks(CALLBACK_CHUNK_FRAMES.saturating_mul(channel_count)) {
        if block.len() < channel_count {
            continue;
        }
        let Ok(mut mono) = free.try_recv() else {
            overflowed.store(true, Ordering::Release);
            return;
        };
        mono.clear();
        for frame in block.chunks_exact(channel_count) {
            mono.push(
                (frame.iter().copied().map(f32::from_sample).sum::<f32>() / f32::from(channels))
                    .clamp(-1.0, 1.0),
            );
        }
        let chunk = Chunk {
            samples: mono,
            queued_at: Instant::now(),
        };
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = tx.try_send(chunk) {
            overflowed.store(true, Ordering::Release);
            return;
        }
    }
}

fn store_stream_error(slot: &Mutex<Option<String>>, message: String) {
    if let Ok(mut error) = slot.lock()
        && error.is_none()
    {
        *error = Some(message);
    }
}

fn first_sample_error(active: &Active) -> NativeVoiceCaptureError {
    active
        .stream_error
        .lock()
        .ok()
        .and_then(|error| error.clone())
        .map_or(
            NativeVoiceCaptureError::FirstSampleTimeout,
            NativeVoiceCaptureError::Stream,
        )
}

fn finish(mut active: Active) -> Result<CaptureOutput, NativeVoiceCaptureError> {
    active
        .stream
        .pause()
        .map_err(|error| NativeVoiceCaptureError::Stream(error.to_string()))?;
    let drain_started = Instant::now();
    while let Ok(chunk) = active.audio_rx.try_recv() {
        active.push(chunk);
    }
    if active.capture_too_long {
        return Err(NativeVoiceCaptureError::TooLong);
    }
    active.metrics.finish_drain_us = drain_started
        .elapsed()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64;
    let flush_started = Instant::now();
    active.flush_prefill()?;
    active.seal_and_release_silence_gate()?;
    active.metrics.finish_flush_us = flush_started
        .elapsed()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64;
    active.target = None;
    if let Some(error) = active
        .stream_error
        .lock()
        .map_err(|_| controller("stream error lock poisoned"))?
        .take()
    {
        return Err(NativeVoiceCaptureError::Stream(error));
    }
    if active.overflowed.load(Ordering::Acquire) {
        if let Some(source) = &mut active.transcription {
            source.reject(IngressFailure::Overflow);
        }
        return Err(NativeVoiceCaptureError::QueueOverflow);
    }
    active.metrics.retained_samples = active.samples.len();
    let wav_started = Instant::now();
    let wav = encode_wav(&active.samples)?;
    active.metrics.wav_encode_us =
        wav_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    if let Some(source) = &mut active.transcription {
        let _ = publish_transcription_retained(source, &active.samples, true);
        let _ = source.seal(active.samples.len());
        active.metrics.transcription_ingress = Some(source.metrics());
    }
    Ok(CaptureOutput {
        wav,
        heard_voice: active.voice.heard_voice,
        metrics: active.metrics.clone(),
    })
}

#[derive(Debug)]
struct VoiceActivity {
    heard_voice: bool,
    voiced: bool,
    silence_ms: f32,
    noise_floor: f32,
    threshold: f32,
    close_threshold: f32,
    attack_samples: usize,
    release_samples: usize,
    utterance_voiced_samples: usize,
    stationary_samples: usize,
    level_mean: f32,
    quantile: P2Quantile,
}

impl Default for VoiceActivity {
    fn default() -> Self {
        Self {
            heard_voice: false,
            voiced: false,
            silence_ms: 0.0,
            noise_floor: 0.001_25,
            threshold: 0.003_6,
            close_threshold: 0.002_325,
            attack_samples: 0,
            release_samples: 0,
            utterance_voiced_samples: 0,
            stationary_samples: 0,
            level_mean: 0.0,
            quantile: P2Quantile::new(0.2),
        }
    }
}

impl VoiceActivity {
    fn update(&mut self, rms: f32, samples: usize) {
        let prior_mean = self.level_mean;
        self.level_mean = if prior_mean == 0.0 {
            rms
        } else {
            // Preserve the old roughly 50 ms envelope constant after moving
            // activity analysis from 1 ms to 10 ms windows.
            prior_mean * 0.82 + rms * 0.18
        };
        let stationary_tolerance = (self.level_mean * 0.12).max(0.000_2);
        if (rms - prior_mean).abs() <= stationary_tolerance {
            self.stationary_samples = self.stationary_samples.saturating_add(samples);
        } else {
            self.stationary_samples = 0;
        }
        self.threshold = (self.noise_floor * 2.6 + 0.000_35).clamp(0.001_5, 0.02);
        self.close_threshold =
            (self.noise_floor * 1.7 + 0.000_2).clamp(0.001, self.threshold * 0.78);

        if !self.voiced && rms < self.close_threshold {
            let percentile = self.quantile.push(rms);
            // Keep the former ~500 ms learning constant at the 10 ms analysis
            // cadence. Faster adaptation absorbs quiet speech; slower
            // adaptation leaves a changed room floor above the close gate.
            self.noise_floor = self.noise_floor * 0.98 + percentile * 0.02;
        }

        if self.voiced {
            if rms >= self.close_threshold {
                self.utterance_voiced_samples =
                    self.utterance_voiced_samples.saturating_add(samples);
            }
            if self.heard_voice
                && self.stationary_samples >= STATIONARY_NOISE_RELEASE_SAMPLES
                && self.level_mean < 0.02
            {
                // A steady post-speech bed is background, even if the prior
                // floor was learned in an unusually quiet instant. Rebase
                // conservatively above this stationary level; any renewed
                // modulation crosses the attack threshold and belays the
                // speculative reply within the normal 40 ms resume window.
                let rebased = ((self.level_mean * 1.12 - 0.000_2) / 1.7).max(0.0);
                self.noise_floor = self.noise_floor.max(rebased);
                self.quantile = P2Quantile::new(0.2);
                self.quantile.push(self.level_mean);
                self.threshold = (self.noise_floor * 2.6 + 0.000_35).clamp(0.001_5, 0.02);
                self.close_threshold =
                    (self.noise_floor * 1.7 + 0.000_2).clamp(0.001, self.threshold * 0.78);
                self.release_samples = SPEECH_RELEASE_SAMPLES;
                self.voiced = false;
                self.attack_samples = 0;
            } else if rms >= self.close_threshold {
                self.release_samples = 0;
            } else {
                self.release_samples = self.release_samples.saturating_add(samples);
                if self.release_samples >= SPEECH_RELEASE_SAMPLES {
                    self.voiced = false;
                    self.attack_samples = 0;
                }
            }
        } else {
            if rms >= self.threshold {
                self.attack_samples = self.attack_samples.saturating_add(samples);
                let required_attack = if self.heard_voice {
                    SPEECH_RESUME_ATTACK_SAMPLES
                } else {
                    SPEECH_ATTACK_SAMPLES
                };
                if self.attack_samples >= required_attack {
                    self.voiced = true;
                    self.heard_voice = true;
                    self.attack_samples = 0;
                    self.release_samples = 0;
                    self.stationary_samples = 0;
                }
            } else {
                self.attack_samples = 0;
            }
            if self.heard_voice && !self.voiced {
                self.release_samples = self.release_samples.saturating_add(samples);
            }
        }
        self.silence_ms = if self.heard_voice {
            self.release_samples as f32 * 1000.0 / OUTPUT_RATE as f32
        } else {
            0.0
        };
    }

    fn silence_samples(&self) -> usize {
        if self.heard_voice && !self.voiced {
            self.release_samples
        } else {
            0
        }
    }
}

fn update_metric_range(minimum: &mut Option<f32>, maximum: &mut Option<f32>, value: f32) {
    *minimum = Some(minimum.map_or(value, |current| current.min(value)));
    *maximum = Some(maximum.map_or(value, |current| current.max(value)));
}

#[derive(Debug, Clone)]
struct P2Quantile {
    percentile: f64,
    initial: Vec<f64>,
    heights: [f64; 5],
    positions: [f64; 5],
    desired: [f64; 5],
}

impl P2Quantile {
    fn new(percentile: f64) -> Self {
        Self {
            percentile,
            initial: Vec::with_capacity(5),
            heights: [0.0; 5],
            positions: [0.0; 5],
            desired: [0.0; 5],
        }
    }

    fn push(&mut self, sample: f32) -> f32 {
        let sample = f64::from(sample.max(0.0));
        if self.initial.len() < 5 {
            self.initial.push(sample);
            self.initial.sort_by(f64::total_cmp);
            if self.initial.len() == 5 {
                self.heights.copy_from_slice(&self.initial);
                self.positions = [1.0, 2.0, 3.0, 4.0, 5.0];
                let p = self.percentile;
                self.desired = [1.0, 1.0 + 2.0 * p, 1.0 + 4.0 * p, 3.0 + 2.0 * p, 5.0];
            }
            let index = ((self.initial.len() - 1) as f64 * self.percentile).floor() as usize;
            return self.initial[index] as f32;
        }
        let bucket = if sample < self.heights[0] {
            self.heights[0] = sample;
            0
        } else if sample >= self.heights[4] {
            self.heights[4] = sample;
            3
        } else {
            (0..4)
                .find(|index| sample >= self.heights[*index] && sample < self.heights[*index + 1])
                .unwrap_or(3)
        };
        for position in self.positions.iter_mut().skip(bucket + 1) {
            *position += 1.0;
        }
        let p = self.percentile;
        let increments = [0.0, p / 2.0, p, (1.0 + p) / 2.0, 1.0];
        for (desired, increment) in self.desired.iter_mut().zip(increments) {
            *desired += increment;
        }
        for index in 1..=3 {
            let distance = self.desired[index] - self.positions[index];
            let direction = distance.signum();
            if (distance >= 1.0 && self.positions[index + 1] - self.positions[index] > 1.0)
                || (distance <= -1.0 && self.positions[index - 1] - self.positions[index] < -1.0)
            {
                let left = self.positions[index] - self.positions[index - 1];
                let right = self.positions[index + 1] - self.positions[index];
                let proposed = self.heights[index]
                    + direction / (left + right)
                        * ((left + direction) * (self.heights[index + 1] - self.heights[index])
                            / right
                            + (right - direction)
                                * (self.heights[index] - self.heights[index - 1])
                                / left);
                self.heights[index] =
                    if proposed > self.heights[index - 1] && proposed < self.heights[index + 1] {
                        proposed
                    } else {
                        let adjacent = (index as isize + direction as isize) as usize;
                        self.heights[index]
                            + direction * (self.heights[adjacent] - self.heights[index])
                                / (self.positions[adjacent] - self.positions[index])
                    };
                self.positions[index] += direction;
            }
        }
        self.heights[2] as f32
    }
}

#[derive(Debug)]
struct Resampler {
    step: f64,
    index: u64,
    next: f64,
    previous: Option<f32>,
}

impl Resampler {
    fn new(source: u32, output: u32) -> Self {
        Self {
            step: f64::from(source) / f64::from(output),
            index: 0,
            next: 0.0,
            previous: None,
        }
    }

    fn push(&mut self, input: &[f32], output: &mut Vec<f32>) {
        for &current in input {
            let position = self.index as f64;
            if let Some(previous) = self.previous {
                while self.next <= position {
                    let fraction = (self.next - (position - 1.0)).clamp(0.0, 1.0);
                    output.push(previous + (current - previous) * fraction as f32);
                    self.next += self.step;
                }
            } else {
                output.push(current);
                self.next += self.step;
            }
            self.previous = Some(current);
            self.index = self.index.saturating_add(1);
        }
    }
}

fn encode_wav(samples: &[f32]) -> Result<Vec<u8>, NativeVoiceCaptureError> {
    let data_len = samples
        .len()
        .checked_mul(2)
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or(NativeVoiceCaptureError::WavSize)?;
    let riff_len = 36_u32
        .checked_add(data_len)
        .ok_or(NativeVoiceCaptureError::WavSize)?;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&OUTPUT_RATE.to_le_bytes());
    wav.extend_from_slice(&(OUTPUT_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for &sample in samples {
        let scaled = if sample.is_nan() {
            0.0
        } else if sample >= 0.0 {
            sample.min(1.0) * f32::from(i16::MAX)
        } else {
            sample.max(-1.0) * 32_768.0
        };
        wav.extend_from_slice(&(scaled.round() as i16).to_le_bytes());
    }
    Ok(wav)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        samples: Mutex<Vec<f32>>,
        fail: AtomicBool,
        sealed: AtomicBool,
    }

    impl PcmSink for RecordingSink {
        fn push(&self, samples: &[f32]) -> Result<usize, String> {
            if self.sealed.load(Ordering::Acquire) {
                return Err("sealed".to_string());
            }
            if self.fail.load(Ordering::Acquire) {
                return Err("rejected".to_string());
            }
            let mut retained = self.samples.lock().expect("recording sink lock");
            retained.extend_from_slice(samples);
            Ok(retained.len())
        }

        fn seal(&self) -> Result<(), String> {
            self.sealed.store(true, Ordering::Release);
            Ok(())
        }
    }

    fn recording_target(id: &str, sink: Arc<RecordingSink>) -> CaptureTarget {
        CaptureTarget {
            id: id.to_string(),
            sink,
            cancel: Arc::new(|| {}),
        }
    }

    #[tokio::test]
    async fn transcription_source_survives_model_seal_cancel_and_checkpoint_replay() {
        use crate::live_transcription::{TurnIdentity, channel};
        let (mut source, mut reader) = channel(TurnIdentity {
            turn: "turn-a".into(),
            capture: "capture-a".into(),
            conversation: "chat-a".into(),
        })
        .expect("test setup");
        let cancelled = Arc::new(AtomicBool::new(false));
        let old_sink = Arc::new(RecordingSink::default());
        let cancellation = Arc::clone(&cancelled);
        let old_target = CaptureTarget {
            id: "speculation-a".into(),
            sink: old_sink.clone(),
            cancel: Arc::new(move || {
                cancellation.store(true, Ordering::Release);
            }),
        };
        let mut retained = vec![0.25; 1280];
        let mut model_cursor = 0;
        publish_transcription_retained(&mut source, &retained, false).expect("test setup");
        while publish_retained(&retained, &mut model_cursor, &old_target)
            .expect("test setup")
            .is_some()
        {}
        old_target.sink.seal().expect("test setup");
        (old_target.cancel)(); // resumed speech belays this model answer only
        assert!(cancelled.load(Ordering::Acquire));
        assert!(
            reader.source_receipt().is_err(),
            "a model seal cannot finalize the source"
        );

        retained.extend_from_slice(&[0.5; 713]);
        let new_sink = Arc::new(RecordingSink::default());
        let new_target = recording_target("resumed-a", Arc::clone(&new_sink));
        // Exercise the actual retained-replay helper used by retarget().
        let replay = retained_replay(&retained, 640).expect("test setup");
        new_target.sink.push(replay).expect("test setup");
        model_cursor = retained.len();
        assert_eq!(
            source.sample_offset(),
            1280,
            "Gemma replay cannot rewind Parakeet"
        );
        publish_transcription_retained(&mut source, &retained, true).expect("test setup");
        source.seal(retained.len()).expect("test setup");
        let mut observed = Vec::new();
        while let Some(packet) = reader.next().await.expect("test setup") {
            observed.extend_from_slice(&packet.samples[..packet.count]);
            reader.acknowledge(&packet).expect("test setup");
        }
        assert_eq!(observed, retained, "no duplicate or missing source sample");
        assert_eq!(
            model_cursor,
            reader.source_receipt().expect("test setup").samples
        );
        assert_eq!(
            *new_sink.samples.lock().expect("test setup"),
            retained[640..]
        );
    }

    #[tokio::test]
    async fn transcription_overflow_does_not_poison_the_model_pcm_sink() {
        use crate::live_transcription::{TurnIdentity, channel};
        let (mut source, mut reader) = channel(TurnIdentity {
            turn: "turn-a".into(),
            capture: "capture-a".into(),
            conversation: "chat-a".into(),
        })
        .expect("test setup");
        let retained = vec![0.25; 65 * PACKET_SAMPLES];
        assert_eq!(
            publish_transcription_retained(&mut source, &retained, true),
            Err(IngressFailure::Overflow)
        );
        let sink = Arc::new(RecordingSink::default());
        let target = recording_target("gemma-a", Arc::clone(&sink));
        let mut cursor = 0;
        while publish_retained(&retained, &mut cursor, &target)
            .expect("test setup")
            .is_some()
        {}
        assert_eq!(*sink.samples.lock().expect("test setup"), retained);
        assert!(matches!(reader.next().await, Err(IngressFailure::Overflow)));
        assert!(reader.source_receipt().is_err());
    }

    #[test]
    fn a_shrinking_retained_source_is_not_a_transcription_retarget() {
        use crate::live_transcription::{TurnIdentity, channel};
        let (mut source, _reader) = channel(TurnIdentity {
            turn: "turn-a".into(),
            capture: "capture-a".into(),
            conversation: "chat-a".into(),
        })
        .expect("test setup");
        publish_transcription_retained(&mut source, &[0.25; PACKET_SAMPLES], true)
            .expect("test setup");
        assert_eq!(
            publish_transcription_retained(&mut source, &[0.25; 10], true),
            Err(IngressFailure::SourceOrder)
        );
        assert_eq!(
            source.seal(PACKET_SAMPLES),
            Err(IngressFailure::SourceOrder)
        );
    }

    #[test]
    fn percentile_uses_constant_storage_and_rejects_late_outliers() {
        let mut q = P2Quantile::new(0.2);
        for _ in 0..1_000 {
            q.push(0.001);
        }
        for _ in 0..100 {
            q.push(0.004);
        }
        assert_eq!(q.initial.capacity(), 5);
        assert!(q.push(0.001) < 0.001_5);
    }

    #[test]
    fn capture_settle_rejects_the_route_opening_transient_but_preserves_pre_roll() {
        let mut remaining = CAPTURE_SETTLE_SAMPLES;
        let mut pre_roll = VecDeque::with_capacity(PRE_ROLL_SAMPLES);
        let transient = vec![0.01; VAD_FRAME_SAMPLES];
        for _ in 0..CAPTURE_SETTLE_SAMPLES / VAD_FRAME_SAMPLES {
            assert!(retain_settling_frame(
                &mut remaining,
                &mut pre_roll,
                &transient,
            ));
        }
        assert_eq!(remaining, 0);
        assert_eq!(pre_roll.len(), CAPTURE_SETTLE_SAMPLES);
        assert!(!retain_settling_frame(
            &mut remaining,
            &mut pre_roll,
            &transient,
        ));
    }

    #[test]
    fn vad_opens_for_quiet_speech_and_rejects_microgaps() {
        let mut activity = VoiceActivity::default();
        for _ in 0..4 {
            activity.update(0.004, 16);
        }
        assert!(activity.voiced);
        for _ in 0..19 {
            activity.update(0.0, 16);
            assert!(activity.voiced);
        }
        activity.update(0.0, 16);
        assert!(!activity.voiced);
        assert_eq!(activity.silence_ms, 20.0);
        for _ in 0..39 {
            activity.update(0.004, 16);
            assert!(!activity.voiced);
        }
        activity.update(0.004, 16);
        assert!(activity.voiced);
        assert_eq!(activity.silence_ms, 0.0);
    }

    #[test]
    fn vad_resume_rejects_a_short_room_impulse() {
        let mut activity = VoiceActivity::default();
        for _ in 0..4 {
            activity.update(0.004, 16);
        }
        for _ in 0..20 {
            activity.update(0.0, 16);
        }
        assert!(!activity.voiced);
        for _ in 0..10 {
            activity.update(0.006, 16);
        }
        activity.update(0.0, 16);
        assert!(!activity.voiced);
        assert!(activity.heard_voice);
    }

    #[test]
    fn speculation_rejects_a_sub_syllabic_burst_without_discarding_its_session() {
        let mut activity = VoiceActivity::default();
        for _ in 0..6 {
            activity.update(0.006, VAD_FRAME_SAMPLES);
        }
        while activity.voiced {
            activity.update(0.0, VAD_FRAME_SAMPLES);
        }
        assert!(activity.utterance_voiced_samples < SPECULATION_MIN_VOICED_SAMPLES);
        assert_eq!(
            speculation_silence(&activity, false, SPECULATION_BASE_SILENCE_SAMPLES),
            SpeculationSilence::None
        );

        while activity.silence_samples() < SPECULATION_BASE_SILENCE_SAMPLES {
            activity.update(0.0, VAD_FRAME_SAMPLES);
        }
        assert_eq!(
            speculation_silence(&activity, false, SPECULATION_BASE_SILENCE_SAMPLES),
            SpeculationSilence::DelayedShort
        );
        assert_eq!(
            speculation_silence(&activity, true, SPECULATION_BASE_SILENCE_SAMPLES),
            SpeculationSilence::None
        );
    }

    #[test]
    fn short_utterance_uses_the_adaptive_speculation_boundary() {
        let mut activity = VoiceActivity::default();
        for _ in 0..6 {
            activity.update(0.006, VAD_FRAME_SAMPLES);
        }
        while activity.voiced {
            activity.update(0.0, VAD_FRAME_SAMPLES);
        }
        while activity.silence_samples() < SPECULATION_BASE_SILENCE_SAMPLES {
            activity.update(0.0, VAD_FRAME_SAMPLES);
        }
        assert_eq!(
            speculation_silence(&activity, false, SPECULATION_BASE_SILENCE_SAMPLES * 2,),
            SpeculationSilence::None,
        );
        while activity.silence_samples() < SPECULATION_BASE_SILENCE_SAMPLES * 2 {
            activity.update(0.0, VAD_FRAME_SAMPLES);
        }
        assert_eq!(
            speculation_silence(&activity, false, SPECULATION_BASE_SILENCE_SAMPLES * 2,),
            SpeculationSilence::DelayedShort,
        );
    }

    #[test]
    fn speculation_releases_a_plausible_syllable_after_one_model_frame_of_silence() {
        let mut activity = VoiceActivity::default();
        for index in 0..120 {
            let speech = if index % 8 < 4 { 0.012 } else { 0.006 };
            activity.update(speech, VAD_FRAME_SAMPLES);
        }
        while activity.voiced {
            activity.update(0.0, VAD_FRAME_SAMPLES);
        }
        assert!(activity.utterance_voiced_samples >= SPECULATION_MIN_VOICED_SAMPLES);
        assert!(
            activity.silence_samples() < SPECULATION_BASE_SILENCE_SAMPLES,
            "raw release {} must precede speculative release {}",
            activity.silence_samples(),
            SPECULATION_BASE_SILENCE_SAMPLES,
        );
        assert_eq!(
            speculation_silence(&activity, false, SPECULATION_BASE_SILENCE_SAMPLES),
            SpeculationSilence::None
        );
        while activity.silence_samples() < SPECULATION_BASE_SILENCE_SAMPLES {
            activity.update(0.0, VAD_FRAME_SAMPLES);
        }
        assert_eq!(
            speculation_silence(&activity, false, SPECULATION_BASE_SILENCE_SAMPLES),
            SpeculationSilence::Plausible
        );
    }

    #[test]
    fn renewed_silence_returns_to_the_low_latency_speculation_boundary() {
        let mut activity = VoiceActivity::default();
        for index in 0..120 {
            let speech = if index % 8 < 4 { 0.012 } else { 0.006 };
            activity.update(speech, VAD_FRAME_SAMPLES);
        }
        while activity.voiced {
            activity.update(0.0, VAD_FRAME_SAMPLES);
        }
        while activity.silence_samples() < SPECULATION_BASE_SILENCE_SAMPLES {
            activity.update(0.0, VAD_FRAME_SAMPLES);
        }
        assert_eq!(
            speculation_silence(&activity, false, SPECULATION_BASE_SILENCE_SAMPLES),
            SpeculationSilence::Plausible,
        );
    }

    #[test]
    fn vad_rejects_short_noise_spikes_and_adapts_below_old_floor() {
        let mut activity = VoiceActivity::default();
        for _ in 0..1_000 {
            activity.update(0.000_2, 16);
        }
        assert!(activity.threshold < 0.003);
        for _ in 0..3 {
            activity.update(0.004, 16);
        }
        assert!(!activity.voiced);
        activity.update(0.000_2, 16);
        assert!(!activity.voiced);
        assert!(!activity.heard_voice);
    }

    #[test]
    fn vad_hysteresis_keeps_sustained_speech_open() {
        let mut activity = VoiceActivity::default();
        for _ in 0..4 {
            activity.update(0.004, 16);
        }
        let between_thresholds = (activity.threshold + activity.close_threshold) / 2.0;
        assert!(between_thresholds < activity.threshold);
        assert!(between_thresholds > activity.close_threshold);
        for _ in 0..100 {
            activity.update(between_thresholds, 16);
            assert!(activity.voiced);
        }
    }

    #[test]
    fn vad_releases_and_rebases_a_stationary_device_noise_bed() {
        let mut activity = VoiceActivity::default();
        for _ in 0..4 {
            activity.update(0.006, VAD_FRAME_SAMPLES);
        }
        assert!(activity.voiced);

        for _ in 0..STATIONARY_NOISE_RELEASE_SAMPLES / VAD_FRAME_SAMPLES + 200 {
            activity.update(0.004_5, VAD_FRAME_SAMPLES);
        }
        assert!(
            !activity.voiced,
            "steady device noise must not latch the call open"
        );
        assert!(activity.threshold > 0.004_5);

        for _ in 0..100 {
            activity.update(0.004_5, VAD_FRAME_SAMPLES);
            assert!(
                !activity.voiced,
                "the rebased bed must remain classified as noise"
            );
        }
        for _ in 0..SPEECH_RESUME_ATTACK_SAMPLES / VAD_FRAME_SAMPLES {
            activity.update(0.014, VAD_FRAME_SAMPLES);
        }
        assert!(
            activity.voiced,
            "renewed modulated speech must belay speculation (open={}, close={}, floor={}, attack={})",
            activity.threshold,
            activity.close_threshold,
            activity.noise_floor,
            activity.attack_samples,
        );
    }

    #[test]
    fn vad_releases_a_fluctuating_microphone_bed_within_one_second() {
        let mut activity = VoiceActivity::default();
        for index in 0..200 {
            let speech_envelope = if index % 8 < 4 { 0.014 } else { 0.007 };
            activity.update(speech_envelope, VAD_FRAME_SAMPLES);
        }
        assert!(activity.voiced);

        let mut elapsed_samples = 0;
        while activity.voiced && elapsed_samples < OUTPUT_RATE as usize {
            let background = if (elapsed_samples / VAD_FRAME_SAMPLES).is_multiple_of(2) {
                0.004_3
            } else {
                0.004_7
            };
            activity.update(background, VAD_FRAME_SAMPLES);
            elapsed_samples += VAD_FRAME_SAMPLES;
        }

        assert!(
            !activity.voiced,
            "a varying room bed must not hold the call open"
        );
        assert!(
            elapsed_samples <= OUTPUT_RATE as usize,
            "background release must remain subsecond"
        );
    }

    #[test]
    fn resampler_is_chunk_stable() {
        let input = (0..48_000)
            .map(|index| (index % 1_000) as f32 / 1_000.0)
            .collect::<Vec<_>>();
        let mut output = Vec::new();
        let mut resampler = Resampler::new(48_000, 16_000);
        for chunk in input.chunks(137) {
            resampler.push(chunk, &mut output);
        }
        assert_eq!(output.len(), 16_000);
        assert!((output[1] - input[3]).abs() < 0.000_01);
    }

    #[test]
    fn wav_is_exact_pcm16_mono() {
        let wav = encode_wav(&[-1.0, 0.0, 1.0]).expect("wav");
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(wav[24..28].try_into().expect("rate")),
            16_000
        );
        assert_eq!(
            i16::from_le_bytes(wav[44..46].try_into().expect("sample")),
            i16::MIN
        );
        assert_eq!(
            i16::from_le_bytes(wav[48..50].try_into().expect("sample")),
            i16::MAX
        );
    }

    #[test]
    fn first_sample_has_a_distinct_wire_event() {
        assert_eq!(
            serde_json::to_value(CaptureEventKind::FirstSample).expect("serialize event"),
            serde_json::json!("first_sample")
        );
    }

    #[test]
    fn native_capture_requests_a_bounded_low_latency_callback() {
        let config = StreamConfig {
            channels: 2,
            sample_rate: cpal::SampleRate(48_000),
            buffer_size: BufferSize::Default,
        };
        let preferred = preferred_input_config(&config);
        assert_eq!(preferred.channels, config.channels);
        assert_eq!(preferred.sample_rate, config.sample_rate);
        assert_eq!(
            preferred.buffer_size,
            BufferSize::Fixed(LOW_LATENCY_BUFFER_FRAMES)
        );
    }

    #[test]
    fn retarget_replays_only_the_unprojected_capture_tail() {
        let samples = [0.0, 1.0, 2.0, 3.0];
        assert_eq!(
            retained_replay(&samples, 2).expect("valid checkpoint offset"),
            &[2.0, 3.0]
        );
        assert!(retained_replay(&samples, 5).is_err());
    }

    #[test]
    fn retained_publication_is_exactly_once_and_flushes_a_short_tail() {
        let sink = Arc::new(RecordingSink::default());
        let target = recording_target("session", Arc::clone(&sink));
        let mut samples = vec![0.0, 1.0];
        let mut cursor = 0;

        let first = publish_retained(&samples, &mut cursor, &target)
            .expect("first publication")
            .expect("nonempty publication");
        assert_eq!(first.sample_count, 2);
        assert_eq!(first.buffered, 2);
        assert_eq!(cursor, 2);
        assert_eq!(
            publish_retained(&samples, &mut cursor, &target).expect("empty publication"),
            None
        );

        samples.push(2.0);
        let tail = publish_retained(&samples, &mut cursor, &target)
            .expect("tail publication")
            .expect("nonempty tail");
        assert_eq!(tail.sample_count, 1);
        assert_eq!(cursor, 3);
        assert_eq!(*sink.samples.lock().expect("recorded samples"), samples);
    }

    #[test]
    fn retained_publication_bounds_catch_up_to_projector_frames() {
        let sink = Arc::new(RecordingSink::default());
        let target = recording_target("session", Arc::clone(&sink));
        let samples = (0..(PREFILL_BATCH_SAMPLES * 3 + 17))
            .map(|sample| sample as f32)
            .collect::<Vec<_>>();
        let mut cursor = 0;
        let mut lengths = Vec::new();
        while let Some(publication) =
            publish_retained(&samples, &mut cursor, &target).expect("publish")
        {
            lengths.push(publication.sample_count);
        }
        assert_eq!(
            lengths,
            [PREFILL_BATCH_SAMPLES; 3]
                .into_iter()
                .chain([17])
                .collect::<Vec<_>>()
        );
        assert_eq!(cursor, samples.len());
        assert_eq!(*sink.samples.lock().expect("recorded samples"), samples);
    }

    #[test]
    fn rejected_retained_publication_does_not_advance_or_drop_pcm() {
        let sink = Arc::new(RecordingSink::default());
        sink.fail.store(true, Ordering::Release);
        let target = recording_target("session", Arc::clone(&sink));
        let mut cursor = 0;

        assert!(publish_retained(&[1.0, 2.0], &mut cursor, &target).is_err());
        assert_eq!(cursor, 0);
        assert!(sink.samples.lock().expect("recorded samples").is_empty());
    }

    #[test]
    fn attaching_the_same_live_target_never_requests_replay() {
        let sink = Arc::new(RecordingSink::default());
        let current = recording_target("session", Arc::clone(&sink));
        let same = recording_target("session", Arc::clone(&sink));
        let replacement = recording_target("replacement", sink);

        assert!(target_already_attached(Some(&current), &same));
        assert!(!target_already_attached(Some(&current), &replacement));
        assert!(!target_already_attached(None, &same));
    }

    #[test]
    fn native_silence_gate_has_one_terminal_release_or_cancel_winner() {
        let release_cancels = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release_counter = Arc::clone(&release_cancels);
        let released = CaptureSilenceGate::new(Arc::new(move || {
            release_counter.fetch_add(1, Ordering::AcqRel);
        }));
        released.release();
        released.cancel();
        assert_eq!(released.wait(), Ok(()));
        assert_eq!(release_cancels.load(Ordering::Acquire), 0);
        released.resume_speech();
        assert_eq!(release_cancels.load(Ordering::Acquire), 1);

        let arm_cancels = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let arm_counter = Arc::clone(&arm_cancels);
        let cancelled = CaptureSilenceGate::new(Arc::new(move || {
            arm_counter.fetch_add(1, Ordering::AcqRel);
        }));
        cancelled.cancel();
        cancelled.release();
        assert_eq!(
            cancelled.wait(),
            Err("voice_speculation_aborted".to_string())
        );
        assert_eq!(arm_cancels.load(Ordering::Acquire), 1);
    }
}
