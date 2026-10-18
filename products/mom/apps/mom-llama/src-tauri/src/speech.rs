use crate::live_transcription::{
    self, FinalTranscript, IngressMetrics, PcmReader, SourceReceipt, TurnIdentity,
};
use async_trait::async_trait;
use mom_llama_runtime::{
    AttachmentPreviewAnchor, AttachmentTranscriptionInput, Blocker, CommandResult,
    ConversationPersonaIdentity, MessageRole, conversation_persona_identity,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use speech_native_backend_parakeet::{
    PARAKEET_BACKEND_ID, PARAKEET_DEFERRED_LOAD_EVIDENCE_SOURCE_ID, PARAKEET_MODEL_CONTENT_SHA256,
    PARAKEET_MODEL_ID,
};
use speech_native_backend_qwen::{
    IclWarmStatus, QWEN_BACKEND_ID, QWEN_ICL_MODEL_ID, QwenSpeechBackend,
};
use speech_native_host::{SpeechHost, SpeechHostError};
use speech_native_types::{
    AcceptedAudio, AlignmentGranularity, AudioInput, AudioOutputFormat, AudioOutputKind,
    DiarizationPolicy, EncodedAudioFormat, NetworkBehavior, PcmFormat, PcmSampleFormat,
    SpeechBackendDescriptor, SpeechBackendReadiness, SpeechDeadlinePolicy, SpeechError,
    SpeechErrorClass, SpeechGenerationSegment, SpeechOperationCapability, SpeechPrivacyPolicy,
    SpeechRequestContext, SpeechRequestId, SpeechRouteProfile, SpeechRouteSelector,
    SpeechRoutingPolicy, SynthesisEvent, SynthesisInput, SynthesisOutput, SynthesisRequest,
    SynthesisResponse, TimestampGranularity, TranscriptionInput, TranscriptionRequest,
    TranscriptionResponse, TranscriptionTask, VoiceSelector,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::sync::watch;

#[cfg(test)]
use speech_native_types::PlatformTarget;
const MOM_SPEECH_CLIENT_ID: &str = "mom-llama";
const MAX_ACTIVE_SPEECH_OPERATIONS: usize = 4;
const SPEECH_STOP_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const COLD_TRANSCRIPTION_STOP_WAIT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);
const MAX_PENDING_CANCELS: usize = 32;
const MAX_PLAYBACKS: usize = 2;
const MAX_COMPLETE_WAV_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRANSCRIPT_CHARACTERS: usize = 200_000;
const MAX_PRECOMMIT_CHARACTERS: usize = 3_000;

pub type PcmStreamSink =
    Arc<dyn Fn(&speech_native_types::AudioChunk) -> Result<(), String> + Send + Sync>;

#[derive(Debug, Clone, Copy, Default)]
struct RealtimeTtsComputeMetrics {
    lease_acquisitions: u64,
    lease_releases: u64,
    generation_yields: u64,
    generation_yield_us: u64,
    generation_fair_passes: u64,
    lease_held_us: u64,
    lease_released_us: u64,
    minimum_lead_ms: Option<i64>,
    maximum_lead_ms: Option<i64>,
    low_water_ms: u64,
    high_water_ms: u64,
}

#[derive(Debug)]
struct RealtimeTtsComputeArbiter {
    lease: Option<mom_llama_runtime::RealtimeComputeLease>,
    lease_acquired_at: Option<Instant>,
    lease_released_at: Option<Instant>,
    first_pcm: Option<Instant>,
    previous_pcm: Option<Instant>,
    audio_ms: f64,
    maximum_gap_ms: f64,
    metrics: RealtimeTtsComputeMetrics,
}

impl RealtimeTtsComputeArbiter {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            lease: Some(mom_llama_runtime::acquire_realtime_compute()),
            lease_acquired_at: Some(now),
            lease_released_at: None,
            first_pcm: None,
            previous_pcm: None,
            audio_ms: 0.0,
            maximum_gap_ms: 0.0,
            metrics: RealtimeTtsComputeMetrics {
                lease_acquisitions: 1,
                low_water_ms: 120,
                high_water_ms: 220,
                ..RealtimeTtsComputeMetrics::default()
            },
        }
    }

    fn observe(&mut self, chunk: &speech_native_types::AudioChunk) {
        let bytes_per_frame = chunk.format.bytes_per_frame();
        if chunk.data.is_empty() || bytes_per_frame == 0 || chunk.format.sample_rate_hz == 0 {
            return;
        }
        let now = Instant::now();
        if let Some(lease) = &self.lease {
            lease.renew_priority();
        }
        let frames = chunk.data.len() / bytes_per_frame;
        let packet_audio_ms = frames as f64 * 1_000.0 / f64::from(chunk.format.sample_rate_hz);
        self.audio_ms += packet_audio_ms;
        let first = *self.first_pcm.get_or_insert(now);
        if let Some(previous) = self.previous_pcm.replace(now) {
            self.maximum_gap_ms = self
                .maximum_gap_ms
                .max(now.duration_since(previous).as_secs_f64() * 1_000.0);
        }
        let lead_ms = self.audio_ms - now.duration_since(first).as_secs_f64() * 1_000.0;
        let rounded_lead = lead_ms.round() as i64;
        self.metrics.minimum_lead_ms = Some(
            self.metrics
                .minimum_lead_ms
                .map_or(rounded_lead, |current| current.min(rounded_lead)),
        );
        self.metrics.maximum_lead_ms = Some(
            self.metrics
                .maximum_lead_ms
                .map_or(rounded_lead, |current| current.max(rounded_lead)),
        );
        // One pathological packet gap must not make Gemma wait for an
        // ever-growing PCM reservoir. Protect approximately one packet at the
        // low watermark and release after roughly two packets, while allowing
        // bounded adaptation to the measured producer cadence.
        let low_water = 120.0_f64.max((self.maximum_gap_ms * 0.75).min(180.0));
        let high_water = 220.0_f64
            .max((self.maximum_gap_ms * 1.25).min(280.0))
            .max(packet_audio_ms * 1.5);
        self.metrics.low_water_ms = low_water.round() as u64;
        self.metrics.high_water_ms = high_water.round() as u64;
        if self.lease.is_some() && lead_ms >= high_water {
            self.release();
        } else if self.lease.is_none() && lead_ms <= low_water {
            if let Some(released_at) = self.lease_released_at.take() {
                self.metrics.lease_released_us = self
                    .metrics
                    .lease_released_us
                    .saturating_add(elapsed_us(released_at));
            }
            self.lease = Some(mom_llama_runtime::acquire_realtime_compute());
            self.lease_acquired_at = Some(Instant::now());
            self.metrics.lease_acquisitions = self.metrics.lease_acquisitions.saturating_add(1);
        }
    }

    fn release(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        if let Some(acquired_at) = self.lease_acquired_at.take() {
            self.metrics.lease_held_us = self
                .metrics
                .lease_held_us
                .saturating_add(elapsed_us(acquired_at));
        }
        self.lease_released_at = Some(Instant::now());
        let yielded = lease.finish();
        self.metrics.lease_releases = self.metrics.lease_releases.saturating_add(1);
        self.metrics.generation_yields = self
            .metrics
            .generation_yields
            .saturating_add(yielded.generation_yields);
        self.metrics.generation_yield_us = self
            .metrics
            .generation_yield_us
            .saturating_add(yielded.generation_yield_us);
        self.metrics.generation_fair_passes = self
            .metrics
            .generation_fair_passes
            .saturating_add(yielded.generation_fair_passes);
    }

    fn finish(&mut self) -> RealtimeTtsComputeMetrics {
        self.release();
        if let Some(released_at) = self.lease_released_at.take() {
            self.metrics.lease_released_us = self
                .metrics
                .lease_released_us
                .saturating_add(elapsed_us(released_at));
        }
        self.metrics
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpeechPhase {
    Running,
    Quiescing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationKind {
    ReadAloud,
    Transcription,
    PrecommitSynthesis,
}

#[derive(Debug, Clone)]
struct ActiveOperation {
    request_id: SpeechRequestId,
    cancelled: bool,
}

#[derive(Debug, Clone)]
struct PlaybackAudio;

#[derive(Debug, Clone)]
struct SpeechState {
    phase: SpeechPhase,
    active: BTreeMap<String, ActiveOperation>,
    early_cancel: BTreeSet<String>,
    early_cancel_order: VecDeque<String>,
    playback: BTreeMap<String, PlaybackAudio>,
}

impl Default for SpeechState {
    fn default() -> Self {
        Self {
            phase: SpeechPhase::Running,
            active: BTreeMap::new(),
            early_cancel: BTreeSet::new(),
            early_cancel_order: VecDeque::new(),
            playback: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct AssistantMessageTarget {
    conversation_id: String,
    message_id: String,
    text: String,
    text_sha256: String,
    persona_voice_id: Option<String>,
}

#[derive(Debug, Clone)]
struct QwenSelection {
    descriptor_sha256: String,
    descriptor: SpeechBackendDescriptor,
    model_id: String,
    voice_id: String,
}

#[derive(Debug, Clone)]
enum ReadAloudSelection {
    Qwen(QwenSelection),
}

impl ReadAloudSelection {
    fn descriptor(&self) -> &SpeechBackendDescriptor {
        match self {
            Self::Qwen(value) => &value.descriptor,
        }
    }
    fn descriptor_sha256(&self) -> &str {
        match self {
            Self::Qwen(value) => &value.descriptor_sha256,
        }
    }
    fn backend_id(&self) -> &str {
        &self.descriptor().id
    }
    fn model_id(&self) -> Option<String> {
        let Self::Qwen(value) = self;
        Some(value.model_id.clone())
    }
    fn voice_id(&self) -> String {
        let Self::Qwen(value) = self;
        value.voice_id.clone()
    }
}

#[derive(Debug, Clone)]
struct ParakeetSelection {
    descriptor_sha256: String,
    descriptor: SpeechBackendDescriptor,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReadAloudBinding {
    pub conversation_id: String,
    pub message_id: String,
    pub text_sha256: String,
    pub backend_id: String,
    pub backend_descriptor_sha256: String,
    pub model_id: Option<String>,
    pub voice_id: String,
    pub request_id: String,
    pub duration_ms: Option<u64>,
    pub tts_time_to_first_audio_ms: Option<u64>,
    pub tts_total_ms: Option<u64>,
    pub network: NetworkBehavior,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReadAloudOutput {
    pub playback_id: String,
    pub binding: ReadAloudBinding,
}

/// Provenance for speech synthesized before a chat message exists. It has no
/// message identifier by design; callers must bind and retrieve by operation,
/// exact input text digest, and the admitted backend descriptor digest.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrecommitSpeechBinding {
    pub operation: String,
    pub conversation_id: String,
    pub text_sha256: String,
    pub backend_id: String,
    pub backend_descriptor_sha256: String,
    pub model_id: String,
    pub voice_id: String,
    pub request_id: String,
    pub duration_ms: Option<u64>,
    pub tts_queue_ms: Option<u64>,
    pub tts_queue_us: Option<u64>,
    pub tts_host_route_us: Option<u64>,
    pub tts_host_queue_us: Option<u64>,
    pub tts_host_dispatch_us: Option<u64>,
    pub tts_host_admission_us: Option<u64>,
    pub tts_model_load_ms: Option<u64>,
    pub tts_background_warm_active: Option<bool>,
    pub tts_background_warm_completed: Option<u64>,
    pub tts_background_warm_total: Option<u64>,
    pub tts_reference_admission_us: Option<u64>,
    pub tts_reference_cache: Option<String>,
    pub tts_reference_us: Option<u64>,
    pub tts_reference_identity_us: Option<u64>,
    pub tts_reference_memory_lookup_us: Option<u64>,
    pub tts_reference_disk_lookup_us: Option<u64>,
    pub tts_reference_source_read_us: Option<u64>,
    pub tts_reference_source_hash_us: Option<u64>,
    pub tts_reference_compute_us: Option<u64>,
    pub tts_reference_persist_us: Option<u64>,
    pub tts_helper_first_pcm_us: Option<u64>,
    pub tts_helper_text_chunk_us: Option<u64>,
    pub tts_helper_code_generation_us: Option<u64>,
    pub tts_helper_prepared_prefix_cache_hit: Option<bool>,
    pub tts_helper_first_decode_us: Option<u64>,
    pub tts_generation_segments: Vec<SpeechGenerationSegment>,
    pub tts_time_to_first_audio_ms: Option<u64>,
    pub tts_total_ms: Option<u64>,
    pub tts_voice_lookup_us: u64,
    pub tts_descriptor_snapshot_us: u64,
    pub tts_route_selection_us: u64,
    pub tts_operation_admission_us: u64,
    pub tts_postflight_voice_lookup_us: u64,
    pub tts_compute_lease_acquisitions: u64,
    pub tts_compute_lease_releases: u64,
    pub tts_generation_yields: u64,
    pub tts_generation_yield_us: u64,
    pub tts_generation_fair_passes: u64,
    pub tts_compute_lease_held_us: u64,
    pub tts_compute_lease_released_us: u64,
    pub tts_compute_minimum_lead_ms: Option<i64>,
    pub tts_compute_maximum_lead_ms: Option<i64>,
    pub tts_compute_low_water_ms: u64,
    pub tts_compute_high_water_ms: u64,
    pub network: NetworkBehavior,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrecommitSpeechOutput {
    pub binding: PrecommitSpeechBinding,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AttachmentTranscriptionProvenance {
    pub conversation_id: String,
    pub attachment_id: String,
    pub root_sha256: String,
    pub artifact_id: String,
    pub policy_fingerprint: String,
    pub source_object_id: String,
    pub blob_object_id: String,
    pub audio_sha256: String,
    pub media_type: String,
    pub byte_len: u64,
    pub validation: String,
    pub processor: String,
    pub processor_version: String,
    pub backend_id: String,
    pub backend_descriptor_sha256: String,
    pub model_id: String,
    pub model_content_sha256: String,
    pub request_id: String,
    pub network: NetworkBehavior,
    pub real_local_inference: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AttachmentTranscriptionOutput {
    pub transcript: String,
    pub language: Option<String>,
    pub provenance: AttachmentTranscriptionProvenance,
    pub timing: AttachmentTranscriptionTiming,
    /// The renderer may offer this as a separate normal composer edit. This
    /// command never changes a draft and never dispatches a message.
    pub insertion_requires_explicit_normal_edit: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AttachmentTranscriptionTiming {
    pub input_audio_ms: Option<u64>,
    pub queue_us: Option<u64>,
    pub host_admission_us: Option<u64>,
    pub model_load_ms: Option<u64>,
    pub first_result_ms: Option<u64>,
    pub total_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SpeechStopOutput {
    pub operation: Option<String>,
    pub playback: Option<String>,
    pub cancellation_requested: bool,
    pub playback_released: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SpeechStopWaitOutput {
    pub stop: SpeechStopOutput,
    pub terminal_acknowledged: bool,
}

fn speech_stop_wait_timeout(
    request_id: &SpeechRequestId,
    allow_cold_transcription_settlement: bool,
) -> std::time::Duration {
    if allow_cold_transcription_settlement && request_id.0.starts_with("mom-transcribe-") {
        COLD_TRANSCRIPTION_STOP_WAIT_TIMEOUT
    } else {
        SPEECH_STOP_WAIT_TIMEOUT
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SpeechShutdown {
    pub host_joined: bool,
    pub active_operation_count: usize,
    pub retained_playback_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GenerationBackpressureReceipt {
    pub request_id: String,
    pub active: bool,
    pub generation_yields: u64,
    pub generation_yield_us: u64,
}

#[async_trait]
trait SpeechExecutor: Send + Sync {
    fn descriptors(&self) -> Result<Arc<[SpeechBackendDescriptor]>, SpeechHostError>;
    fn current_descriptors(&self) -> Result<Arc<[SpeechBackendDescriptor]>, SpeechHostError> {
        self.descriptors()
    }
    async fn synthesize(&self, request: SynthesisRequest) -> Result<SynthesisResponse, String>;
    async fn synthesize_streaming(
        &self,
        request: SynthesisRequest,
        sink: PcmStreamSink,
    ) -> Result<SynthesisResponse, String> {
        let _ = sink;
        self.synthesize(request).await
    }
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, String>;
    // A default preserves existing test executors; only the host implementation
    // supplies a real streaming path. No complete-WAV emulation is permitted.
    async fn transcribe_live(
        &self,
        _request: TranscriptionRequest,
        _input: PcmReader,
    ) -> Result<(TranscriptionResponse, SourceReceipt), String> {
        Err("speech_live_transcription_unavailable".to_string())
    }
    fn cancel(&self, request_id: &SpeechRequestId) -> usize;
    fn quiesce(&self) -> Result<(), SpeechHostError>;
    async fn shutdown(&self) -> Result<(), SpeechHostError>;
}

struct HostSpeechExecutor {
    host: Arc<SpeechHost>,
    descriptors: Arc<[SpeechBackendDescriptor]>,
}

#[async_trait]
impl SpeechExecutor for HostSpeechExecutor {
    fn descriptors(&self) -> Result<Arc<[SpeechBackendDescriptor]>, SpeechHostError> {
        Ok(Arc::clone(&self.descriptors))
    }

    fn current_descriptors(&self) -> Result<Arc<[SpeechBackendDescriptor]>, SpeechHostError> {
        Ok(Arc::from(self.host.descriptors()?))
    }

    async fn synthesize(&self, request: SynthesisRequest) -> Result<SynthesisResponse, String> {
        self.synthesize_validated(request, None).await
    }

    async fn synthesize_streaming(
        &self,
        request: SynthesisRequest,
        sink: PcmStreamSink,
    ) -> Result<SynthesisResponse, String> {
        self.synthesize_validated(request, Some(sink)).await
    }

    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, String> {
        self.host
            .transcribe(request)
            .await
            .map_err(|error| error.to_string())?
            .final_response()
            .await
            .map_err(|error| speech_error_message(&error))
    }

    async fn transcribe_live(
        &self,
        request: TranscriptionRequest,
        input: PcmReader,
    ) -> Result<(TranscriptionResponse, SourceReceipt), String> {
        let status = input.status();
        let work = async {
            let ticket = self
                .host
                .transcribe(request)
                .await
                .map_err(|error| error.to_string())?;
            consume_live_transcription(ticket, input).await
        };
        // Overflow/cancel must interrupt both model admission and a blocked
        // audio push. Dropping a ticket requests its existing owned cancellation.
        tokio::select! {
            biased;
            error = live_transcription::failure(status) => Err(error.to_string()),
            result = tokio::time::timeout(std::time::Duration::from_secs(90), work) =>
                result.map_err(|_| "speech_live_transcription_deadline".to_string())?,
        }
    }

    fn cancel(&self, request_id: &SpeechRequestId) -> usize {
        self.host.cancel(request_id)
    }

    fn quiesce(&self) -> Result<(), SpeechHostError> {
        self.host.quiesce()
    }

    async fn shutdown(&self) -> Result<(), SpeechHostError> {
        self.host.shutdown().await
    }
}

impl HostSpeechExecutor {
    fn new(host: Arc<SpeechHost>) -> Result<Self, SpeechHostError> {
        // Local backend registration is complete before MomSpeech is exposed.
        // Keep the immutable routing inventory behind an Arc so every short
        // streamed TTS segment does not lock the host, query every backend,
        // and deep-clone all model and voice descriptors before dispatch.
        let descriptors = Arc::<[SpeechBackendDescriptor]>::from(host.descriptors()?);
        Ok(Self { host, descriptors })
    }

    async fn synthesize_validated(
        &self,
        request: SynthesisRequest,
        sink: Option<PcmStreamSink>,
    ) -> Result<SynthesisResponse, String> {
        let stream = request.stream;
        let mut ticket = self
            .host
            .synthesize(request)
            .await
            .map_err(|error| error.to_string())?;
        if !stream {
            return ticket
                .final_response()
                .await
                .map_err(|error| speech_error_message(&error));
        }
        let mut emitted_bytes = 0_u64;
        let mut format = None;
        let mut expected_sequence = 0_u64;
        let mut expected_sample_offset = 0_u64;
        let mut saw_end = false;
        while let Some(event) = ticket.events.recv().await {
            if let SynthesisEvent::Audio { chunk, .. } = event {
                if chunk.sequence != expected_sequence {
                    return Err(
                        "speech_stream_sequence_invalid: Qwen PCM chunks were not contiguous"
                            .to_string(),
                    );
                }
                expected_sequence = expected_sequence.saturating_add(1);
                if chunk.sample_offset != expected_sample_offset {
                    return Err(
                        "speech_stream_offset_invalid: Qwen PCM chunks overlapped or skipped samples"
                            .to_string(),
                    );
                }
                let frame_bytes = chunk.format.bytes_per_frame();
                let frames = u64::try_from(chunk.data.len() / frame_bytes).unwrap_or(u64::MAX);
                expected_sample_offset =
                    expected_sample_offset.checked_add(frames).ok_or_else(|| {
                        "speech_stream_offset_invalid: Qwen PCM sample offset overflowed"
                            .to_string()
                    })?;
                if let Some(expected) = &format {
                    if expected != &chunk.format {
                        return Err(
                            "speech_stream_format_changed: Qwen PCM format changed mid-stream"
                                .to_string(),
                        );
                    }
                } else {
                    format = Some(chunk.format);
                }
                emitted_bytes = emitted_bytes
                    .checked_add(u64::try_from(chunk.data.len()).unwrap_or(u64::MAX))
                    .ok_or_else(|| {
                        "speech_stream_too_large: PCM byte count overflowed".to_string()
                    })?;
                if emitted_bytes > u64::try_from(MAX_COMPLETE_WAV_BYTES).unwrap_or(u64::MAX) {
                    return Err(
                        "speech_stream_too_large: Qwen PCM exceeded the playback bound".to_string(),
                    );
                }
                if let Some(sink) = &sink {
                    sink(&chunk)?;
                }
                saw_end |= chunk.end_of_stream;
            }
        }
        let response = ticket
            .final_response()
            .await
            .map_err(|error| speech_error_message(&error))?;
        let format =
            format.ok_or_else(|| "speech_stream_empty: Qwen returned no PCM".to_string())?;
        if !saw_end {
            return Err("speech_stream_incomplete: Qwen closed without an end marker".to_string());
        }
        match &response.output {
            SynthesisOutput::Streamed { format: output, emitted_bytes: output_bytes }
                if output == &AudioOutputFormat::Pcm { format }
                    && *output_bytes == emitted_bytes => {}
            _ => return Err("speech_stream_receipt_invalid: Qwen's final streamed receipt did not match the emitted PCM".to_string()),
        }
        Ok(response)
    }
}

/// Final-only forwarding, with a separate source completeness proof. The
/// backend's EOF/final response alone cannot authorize a truncated stream.
async fn consume_live_transcription(
    ticket: speech_native_types::TranscriptionTicket,
    mut input: PcmReader,
) -> Result<(TranscriptionResponse, SourceReceipt), String> {
    let sink = ticket
        .audio_sink
        .clone()
        .ok_or_else(|| "speech_live_sink_missing".to_string())?;
    let format = live_pcm_format();
    let forward = async {
        let mut sequence = 0_u64;
        while let Some(packet) = input.next().await.map_err(|error| error.to_string())? {
            let mut bytes = Vec::with_capacity(packet.count * 4);
            for sample in &packet.samples[..packet.count] {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            let chunk = speech_native_types::AudioChunk {
                sequence,
                sample_offset: packet.offset as u64,
                format,
                data: bytes,
                end_of_stream: false,
            };
            tokio::time::timeout(std::time::Duration::from_secs(5), sink.push(chunk))
                .await
                .map_err(|_| "speech_live_ingress_backlog".to_string())?
                .map_err(|error| speech_error_message(&error))?;
            input
                .acknowledge(&packet)
                .map_err(|error| error.to_string())?;
            sequence += 1;
        }
        // All pushes are individually acknowledged before finish. The Speech
        // sink's finish may intentionally close the data channel; it is not a
        // guarantee that unacknowledged producer work was consumed.
        sink.finish()
            .await
            .map_err(|error| speech_error_message(&error))?;
        input.source_receipt().map_err(|error| error.to_string())
    };
    let final_response = async {
        ticket
            .final_response()
            .await
            .map_err(|error| speech_error_message(&error))
    };
    let (source, response) = tokio::try_join!(forward, final_response)?;
    Ok((response, source))
}

fn live_pcm_format() -> PcmFormat {
    PcmFormat {
        sample_rate_hz: live_transcription::RATE,
        channels: 1,
        sample_format: PcmSampleFormat::F32Le,
        interleaved: true,
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LiveTranscriptionTrace {
    pub(crate) schema: &'static str,
    pub(crate) operation: String,
    pub(crate) source: TurnIdentity,
    pub(crate) stage: &'static str,
    pub(crate) elapsed_us: u64,
    pub(crate) ingress: IngressMetrics,
    pub(crate) error: Option<String>,
}

// Cancelling/dropping the caller also settles the Mom registry, not only the
// Speech ticket. No detached app task or process is introduced by this stage.
struct LiveSpeechOperation<'a> {
    speech: &'a MomSpeech,
    operation: &'a str,
    request_id: SpeechRequestId,
    armed: bool,
}
impl Drop for LiveSpeechOperation<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self.speech.executor.cancel(&self.request_id);
        self.speech
            .finish_operation(self.operation, &self.request_id);
    }
}

pub struct MomSpeech {
    executor: Arc<dyn SpeechExecutor>,
    _qwen: Option<Arc<QwenSpeechBackend>>,
    _qwen_startup_warm: Option<IclWarmStatus>,
    state: Mutex<SpeechState>,
    operation_revision: watch::Sender<u64>,
    precommit_voices: RwLock<BTreeMap<String, String>>,
    generation_backpressure:
        Mutex<BTreeMap<String, mom_llama_runtime::GenerationBackpressureLease>>,
}

const MAX_GENERATION_BACKPRESSURE_REQUESTS: usize = 8;

impl MomSpeech {
    pub async fn discover(
        data_dir: &Path,
        installed_voice_root: &Path,
    ) -> Result<Arc<Self>, String> {
        Self::discover_with_prepared_qwen(data_dir, installed_voice_root, None).await
    }

    pub async fn discover_with_prepared_qwen(
        data_dir: &Path,
        installed_voice_root: &Path,
        prepared_qwen: Option<Arc<QwenSpeechBackend>>,
    ) -> Result<Arc<Self>, String> {
        #[cfg(target_os = "macos")]
        let (prepared_qwen, qwen_startup_warm) = {
            let qwen = prepared_qwen.unwrap_or_else(|| {
                desktop_speech::prepare_local_qwen(
                    Some(data_dir.join("speech-models")),
                    Some(installed_voice_root.to_path_buf()),
                )
            });
            let warm = qwen
                .wait_for_icl_background_warm()
                .map_err(|error| format!("Local Qwen startup warm failed: {error}"))?;
            if warm.total > 0 && warm.completed != warm.total {
                return Err(format!(
                    "Local Qwen startup warm completed only {} of {} compile probes",
                    warm.completed, warm.total
                ));
            }
            eprintln!(
                "mom-llama qwen resident warm: resident={} ms, probe={} ms, total={} ms",
                warm.resident_us / 1_000,
                warm.probe_us / 1_000,
                warm.total_us / 1_000
            );
            (Some(qwen), Some(warm))
        };
        #[cfg(not(target_os = "macos"))]
        let (prepared_qwen, qwen_startup_warm) = (prepared_qwen, None);
        let host = desktop_speech::discover_local_host_with_qwen(
            Some(data_dir.join("speech-models")),
            Some(installed_voice_root.to_path_buf()),
            prepared_qwen.clone(),
        )
        .await
        .map_err(|error| format!("Local speech registration failed: {error}"))?;

        let executor = HostSpeechExecutor::new(host)
            .map_err(|error| format!("Local speech inventory failed: {error}"))?;
        Ok(Arc::new(Self {
            executor: Arc::new(executor),
            _qwen: prepared_qwen,
            _qwen_startup_warm: qwen_startup_warm,
            state: Mutex::new(SpeechState::default()),
            operation_revision: watch::channel(0).0,
            precommit_voices: RwLock::new(BTreeMap::new()),
            generation_backpressure: Mutex::new(BTreeMap::new()),
        }))
    }

    #[cfg(test)]
    pub fn empty_for_tests() -> Arc<Self> {
        let host = Arc::new(SpeechHost::new(PlatformTarget::current()));
        let executor = HostSpeechExecutor::new(host).expect("empty speech inventory");
        Arc::new(Self {
            executor: Arc::new(executor),
            _qwen: None,
            _qwen_startup_warm: None,
            state: Mutex::new(SpeechState::default()),
            operation_revision: watch::channel(0).0,
            precommit_voices: RwLock::new(BTreeMap::new()),
            generation_backpressure: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn set_generation_backpressure(
        &self,
        request_id: &str,
        active: bool,
    ) -> Result<GenerationBackpressureReceipt, String> {
        let mut leases = self
            .generation_backpressure
            .lock()
            .map_err(|_| "voice generation backpressure is unavailable".to_string())?;
        if active {
            if !leases.contains_key(request_id) {
                if leases.len() >= MAX_GENERATION_BACKPRESSURE_REQUESTS {
                    return Err(
                        "voice generation backpressure has too many outstanding requests"
                            .to_string(),
                    );
                }
                let lease = mom_llama_runtime::acquire_generation_backpressure(request_id)
                    .map_err(|error| error.to_string())?;
                leases.insert(request_id.to_string(), lease);
            }
            return Ok(GenerationBackpressureReceipt {
                request_id: request_id.to_string(),
                active: true,
                generation_yields: 0,
                generation_yield_us: 0,
            });
        }
        let metrics = leases
            .remove(request_id)
            .map(mom_llama_runtime::GenerationBackpressureLease::finish)
            .unwrap_or_default();
        Ok(GenerationBackpressureReceipt {
            request_id: request_id.to_string(),
            active: false,
            generation_yields: metrics.generation_yields,
            generation_yield_us: metrics.generation_yield_us,
        })
    }

    /// Binds a conversation to the persona voice from the immutable turn
    /// preparation that already loaded it. Conversation persona identity is
    /// creation-time state; a conflicting second binding is therefore a hard
    /// provenance error rather than an overwrite.
    pub fn bind_precommit_voice(
        &self,
        conversation_id: &str,
        identity: &ConversationPersonaIdentity,
    ) -> Result<bool, String> {
        let Some(voice) = persona_voice_key_for_conversation(
            &identity.recipient_ids,
            identity.source_conversation_id.as_deref(),
        ) else {
            return Ok(false);
        };
        let voice_id = format!("mom.qwen.icl.{voice}");
        let mut bindings = self
            .precommit_voices
            .write()
            .map_err(|_| "speech_precommit_voice_bindings_poisoned".to_string())?;
        match bindings.get(conversation_id) {
            Some(existing) if existing == &voice_id => Ok(true),
            Some(_) => Err("speech_precommit_persona_binding_changed".to_string()),
            None => {
                bindings.insert(conversation_id.to_string(), voice_id);
                Ok(true)
            }
        }
    }

    fn precommit_voice(&self, conversation_id: &str) -> Result<Option<String>, String> {
        self.precommit_voices
            .read()
            .map_err(|_| "speech_precommit_voice_bindings_poisoned".to_string())
            .map(|bindings| bindings.get(conversation_id).cloned())
    }

    /// Returns the immutable consultant voice for a prepared turn. Normal
    /// microphone admission binds this while the model is being prepared. A
    /// renderer restart or an older persisted conversation can legitimately
    /// reach speech without that process-local cache entry, so recover it from
    /// the durable conversation identity once instead of failing a valid turn.
    fn resolve_precommit_voice(&self, conversation_id: &str) -> Result<Option<String>, String> {
        if let Some(voice) = self.precommit_voice(conversation_id)? {
            return Ok(Some(voice));
        }
        let Some(identity) = conversation_persona_identity(conversation_id)
            .map_err(|error| format!("speech_precommit_identity_lookup_failed: {error}"))?
        else {
            return Ok(None);
        };
        if !self.bind_precommit_voice(conversation_id, &identity)? {
            return Ok(None);
        }
        self.precommit_voice(conversation_id)
    }

    #[cfg(test)]
    pub async fn read_aloud(
        &self,
        conversation_id: &str,
        message_id: &str,
        operation: &str,
    ) -> anyhow::Result<CommandResult<ReadAloudOutput>> {
        self.read_aloud_with_mode(conversation_id, message_id, operation, false)
            .await
    }

    #[cfg(test)]
    pub async fn read_aloud_with_mode(
        &self,
        conversation_id: &str,
        message_id: &str,
        operation: &str,
        require_persona_voice: bool,
    ) -> anyhow::Result<CommandResult<ReadAloudOutput>> {
        self.read_aloud_streaming_with_mode(
            conversation_id,
            message_id,
            operation,
            require_persona_voice,
            None,
        )
        .await
    }

    pub async fn read_aloud_streaming_with_mode(
        &self,
        conversation_id: &str,
        message_id: &str,
        operation: &str,
        require_persona_voice: bool,
        stream_sink: Option<PcmStreamSink>,
    ) -> anyhow::Result<CommandResult<ReadAloudOutput>> {
        validate_operation_token(operation)?;
        let target = match assistant_message_target(conversation_id, message_id)? {
            Ok(target) => target,
            Err(blocker) => return Ok(blocked("mom_llama.speech_read_aloud", blocker)),
        };
        let descriptors = self.executor.descriptors()?;
        let selection = match select_read_aloud(
            &descriptors,
            target.text.chars().count(),
            target.persona_voice_id.as_deref(),
            require_persona_voice,
        ) {
            Ok(selection) => selection,
            Err(blocker) => return Ok(blocked("mom_llama.speech_read_aloud", blocker)),
        };
        let request_id = SpeechRequestId(format!("mom-read-aloud-{operation}"));
        if let Err(blocker) =
            self.begin_operation(operation, request_id.clone(), OperationKind::ReadAloud)
        {
            return Ok(blocked("mom_llama.speech_read_aloud", blocker));
        }
        let request = read_aloud_request(&target, &selection, request_id.clone());
        let response = match stream_sink.clone() {
            Some(sink) => self.executor.synthesize_streaming(request, sink).await,
            None => self.executor.synthesize(request).await,
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let cancelled = self.finish_operation(operation, &request_id);
                return Ok(blocked(
                    "mom_llama.speech_read_aloud",
                    if cancelled {
                        speech_cancelled_blocker()
                    } else {
                        speech_failure_blocker(&error)
                    },
                ));
            }
        };
        let current = assistant_message_target(conversation_id, message_id).inspect_err(|_| {
            self.finish_operation(operation, &request_id);
        })?;
        if match &current {
            Ok(current) => current.text_sha256 != target.text_sha256,
            Err(_) => true,
        } {
            self.finish_operation(operation, &request_id);
            return Ok(blocked(
                "mom_llama.speech_read_aloud",
                Blocker::new(
                    "speech_read_aloud_stale",
                    "The exact assistant message changed before synthesized audio could be published.",
                    vec!["Choose Read Aloud again on the current message.".to_string()],
                ),
            ));
        }
        let tts_time_to_first_audio_ms = response.usage.time_to_first_result_ms;
        let tts_total_ms = response.usage.total_ms;
        let validated = validate_read_aloud_response(response, &request_id, &selection);
        let duration_ms = match validated {
            Ok(output) => output,
            Err(blocker) => {
                self.finish_operation(operation, &request_id);
                return Ok(blocked("mom_llama.speech_read_aloud", blocker));
            }
        };
        let binding = ReadAloudBinding {
            conversation_id: target.conversation_id,
            message_id: target.message_id,
            text_sha256: target.text_sha256,
            backend_id: selection.backend_id().to_string(),
            backend_descriptor_sha256: selection.descriptor_sha256().to_string(),
            model_id: selection.model_id(),
            voice_id: selection.voice_id(),
            request_id: request_id.0.clone(),
            duration_ms,
            tts_time_to_first_audio_ms,
            tts_total_ms,
            network: NetworkBehavior::Never,
        };
        let playback_id = match self.publish_playback(operation, &request_id, binding.clone()) {
            Ok(playback_id) => playback_id,
            Err(blocker) => return Ok(blocked("mom_llama.speech_read_aloud", blocker)),
        };
        Ok(CommandResult::passed(
            "mom_llama.speech_read_aloud",
            "host_integrated",
            ReadAloudOutput {
                playback_id,
                binding: binding.clone(),
            },
            Vec::new(),
            vec![
                format!("message:{}", binding.message_id),
                format!("text-sha256:{}", binding.text_sha256),
                format!("speech-backend:{}", binding.backend_id),
                format!("voice:{}", binding.voice_id),
            ],
            true,
            false,
        ))
    }

    /// Synthesizes a bounded reply for the exact persona represented by this
    /// conversation, before a chat message is committed. The audio is retained
    /// only in this runtime's opaque-operation registry.
    pub async fn synthesize_precommit(
        &self,
        conversation_id: &str,
        operation: &str,
        text: &str,
        stream_sink: PcmStreamSink,
    ) -> anyhow::Result<CommandResult<PrecommitSpeechOutput>> {
        validate_operation_token(operation)?;
        let character_count = text.chars().count();
        if text.trim().is_empty() || character_count > MAX_PRECOMMIT_CHARACTERS {
            return Ok(blocked(
                "mom_llama.speech_precommit_synthesize",
                Blocker::new(
                    "speech_precommit_text_invalid",
                    "Pre-commit speech requires non-empty text within the local synthesis bound.",
                    Vec::new(),
                ),
            ));
        }
        // Claim the shared compute lane before voice lookup and routing. If
        // Gemma starts another non-preemptible Metal decode while this command
        // is being admitted, Qwen's first codec packet pays that entire token
        // quantum. The lease is RAII-owned, so every blocked or failed return
        // below still releases it without a separate cleanup branch.
        let compute_arbiter = Arc::new(Mutex::new(RealtimeTtsComputeArbiter::new()));
        let stage_started = Instant::now();
        let voice_id = match self
            .resolve_precommit_voice(conversation_id)
            .map_err(anyhow::Error::msg)?
        {
            Some(voice_id) => voice_id,
            None => {
                return Ok(blocked(
                    "mom_llama.speech_precommit_synthesize",
                    Blocker::new(
                        "speech_precommit_voice_unbound",
                        "The prepared turn has no bound consultant voice.",
                        Vec::new(),
                    ),
                ));
            }
        };
        let tts_voice_lookup_us = elapsed_us(stage_started);
        let stage_started = Instant::now();
        let descriptors = self.executor.descriptors()?;
        let tts_descriptor_snapshot_us = elapsed_us(stage_started);
        let stage_started = Instant::now();
        let selection =
            match select_read_aloud(&descriptors, character_count, Some(&voice_id), true) {
                Ok(selection) => selection,
                Err(blocker) => {
                    return Ok(blocked("mom_llama.speech_precommit_synthesize", blocker));
                }
            };
        let tts_route_selection_us = elapsed_us(stage_started);
        let ReadAloudSelection::Qwen(qwen) = &selection;
        let request_id = SpeechRequestId(format!("mom-precommit-{operation}"));
        let stage_started = Instant::now();
        if let Err(blocker) = self.begin_operation(
            operation,
            request_id.clone(),
            OperationKind::PrecommitSynthesis,
        ) {
            return Ok(blocked("mom_llama.speech_precommit_synthesize", blocker));
        }
        let tts_operation_admission_us = elapsed_us(stage_started);
        let observed_compute = Arc::clone(&compute_arbiter);
        let downstream_sink = Arc::clone(&stream_sink);
        let scheduled_sink: PcmStreamSink = Arc::new(move |chunk| {
            if let Ok(mut arbiter) = observed_compute.lock() {
                arbiter.observe(chunk);
            }
            downstream_sink(chunk)
        });
        let response = match self
            .executor
            .synthesize_streaming(
                precommit_synthesis_request(text, &selection, request_id.clone()),
                scheduled_sink,
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                if let Ok(mut arbiter) = compute_arbiter.lock() {
                    arbiter.release();
                }
                let cancelled = self.finish_operation(operation, &request_id);
                return Ok(blocked(
                    "mom_llama.speech_precommit_synthesize",
                    if cancelled {
                        speech_cancelled_blocker()
                    } else {
                        speech_failure_blocker(&error)
                    },
                ));
            }
        };
        let compute_metrics = compute_arbiter
            .lock()
            .map(|mut arbiter| arbiter.finish())
            .unwrap_or_default();
        let stage_started = Instant::now();
        let current_voice = self
            .precommit_voice(conversation_id)
            .inspect_err(|_| {
                self.finish_operation(operation, &request_id);
            })
            .map_err(anyhow::Error::msg)?;
        let tts_postflight_voice_lookup_us = elapsed_us(stage_started);
        if current_voice.as_ref() != Some(&qwen.voice_id) {
            self.finish_operation(operation, &request_id);
            return Ok(blocked(
                "mom_llama.speech_precommit_synthesize",
                Blocker::new(
                    "speech_precommit_stale",
                    "The conversation's selected persona changed before its audio was ready.",
                    Vec::new(),
                ),
            ));
        }
        let tts_queue_ms = response.usage.queue_ms;
        let tts_queue_us = response.usage.queue_us;
        let tts_host_route_us = response.usage.host_route_us;
        let tts_host_queue_us = response.usage.host_queue_us;
        let tts_host_dispatch_us = response.usage.host_dispatch_us;
        let tts_host_admission_us = response.usage.host_admission_us;
        let tts_model_load_ms = response.usage.model_load_ms;
        let tts_background_warm_active = response.usage.background_warm_active;
        let tts_background_warm_completed = response.usage.background_warm_completed;
        let tts_background_warm_total = response.usage.background_warm_total;
        let tts_reference_admission_us = response.usage.reference_admission_us;
        let tts_reference_cache = response.usage.reference_cache.clone();
        let tts_reference_us = response.usage.reference_us;
        let tts_reference_identity_us = response.usage.reference_identity_us;
        let tts_reference_memory_lookup_us = response.usage.reference_memory_lookup_us;
        let tts_reference_disk_lookup_us = response.usage.reference_disk_lookup_us;
        let tts_reference_source_read_us = response.usage.reference_source_read_us;
        let tts_reference_source_hash_us = response.usage.reference_source_hash_us;
        let tts_reference_compute_us = response.usage.reference_compute_us;
        let tts_reference_persist_us = response.usage.reference_persist_us;
        let tts_helper_first_pcm_us = response.usage.helper_first_pcm_us;
        let tts_helper_text_chunk_us = response.usage.helper_text_chunk_us;
        let tts_helper_code_generation_us = response.usage.helper_code_generation_us;
        let tts_helper_prepared_prefix_cache_hit = response.usage.helper_prepared_prefix_cache_hit;
        let tts_helper_first_decode_us = response.usage.helper_first_decode_us;
        let tts_generation_segments = response.usage.helper_generation_segments.clone();
        let tts_time_to_first_audio_ms = response.usage.time_to_first_result_ms;
        let tts_total_ms = response.usage.total_ms;
        let duration_ms = match validate_read_aloud_response(response, &request_id, &selection) {
            Ok(output) => output,
            Err(blocker) => {
                self.finish_operation(operation, &request_id);
                return Ok(blocked("mom_llama.speech_precommit_synthesize", blocker));
            }
        };
        let binding = PrecommitSpeechBinding {
            operation: operation.to_string(),
            conversation_id: conversation_id.to_string(),
            text_sha256: sha256_hex(text.as_bytes()),
            backend_id: qwen.descriptor.id.clone(),
            backend_descriptor_sha256: qwen.descriptor_sha256.clone(),
            model_id: qwen.model_id.clone(),
            voice_id: qwen.voice_id.clone(),
            request_id: request_id.0.clone(),
            duration_ms,
            tts_queue_ms,
            tts_queue_us,
            tts_host_route_us,
            tts_host_queue_us,
            tts_host_dispatch_us,
            tts_host_admission_us,
            tts_model_load_ms,
            tts_background_warm_active,
            tts_background_warm_completed,
            tts_background_warm_total,
            tts_reference_admission_us,
            tts_reference_cache,
            tts_reference_us,
            tts_reference_identity_us,
            tts_reference_memory_lookup_us,
            tts_reference_disk_lookup_us,
            tts_reference_source_read_us,
            tts_reference_source_hash_us,
            tts_reference_compute_us,
            tts_reference_persist_us,
            tts_helper_first_pcm_us,
            tts_helper_text_chunk_us,
            tts_helper_code_generation_us,
            tts_helper_prepared_prefix_cache_hit,
            tts_helper_first_decode_us,
            tts_generation_segments,
            tts_time_to_first_audio_ms,
            tts_total_ms,
            tts_voice_lookup_us,
            tts_descriptor_snapshot_us,
            tts_route_selection_us,
            tts_operation_admission_us,
            tts_postflight_voice_lookup_us,
            tts_compute_lease_acquisitions: compute_metrics.lease_acquisitions,
            tts_compute_lease_releases: compute_metrics.lease_releases,
            tts_generation_yields: compute_metrics.generation_yields,
            tts_generation_yield_us: compute_metrics.generation_yield_us,
            tts_generation_fair_passes: compute_metrics.generation_fair_passes,
            tts_compute_lease_held_us: compute_metrics.lease_held_us,
            tts_compute_lease_released_us: compute_metrics.lease_released_us,
            tts_compute_minimum_lead_ms: compute_metrics.minimum_lead_ms,
            tts_compute_maximum_lead_ms: compute_metrics.maximum_lead_ms,
            tts_compute_low_water_ms: compute_metrics.low_water_ms,
            tts_compute_high_water_ms: compute_metrics.high_water_ms,
            network: NetworkBehavior::Never,
        };
        if let Err(blocker) = self.complete_operation(operation, &request_id) {
            return Ok(blocked("mom_llama.speech_precommit_synthesize", blocker));
        }
        Ok(CommandResult::passed(
            "mom_llama.speech_precommit_synthesize",
            "host_integrated",
            PrecommitSpeechOutput {
                binding: binding.clone(),
            },
            Vec::new(),
            vec![
                format!("conversation:{}", binding.conversation_id),
                format!("text-sha256:{}", binding.text_sha256),
                format!("speech-backend:{}", binding.backend_id),
                format!("voice:{}", binding.voice_id),
            ],
            true,
            false,
        ))
    }

    /// The live capture owner holds this future under an AppWorkLease. A final
    /// result is still uncommitted until the exact saved turn is verified.
    pub(crate) async fn transcribe_live(
        &self,
        operation: &str,
        input: PcmReader,
        trace: Arc<dyn Fn(LiveTranscriptionTrace) + Send + Sync>,
    ) -> Result<FinalTranscript, String> {
        validate_operation_token(operation).map_err(|error| error.to_string())?;
        let observer = input.observer();
        let started = Instant::now();
        let emit = |stage, error| {
            trace(LiveTranscriptionTrace {
                schema: "mom_llama.live_transcription.v1",
                operation: operation.to_string(),
                source: observer.identity().clone(),
                stage,
                elapsed_us: started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
                ingress: observer.metrics(),
                error,
            })
        };
        emit("admission_started", None);
        let result: Result<FinalTranscript, String> = async {
            let descriptors = self.executor.current_descriptors().map_err(|error| error.to_string())?;
            let selection = select_parakeet(&descriptors).map_err(|error| error.message)?;
            let supports_stream = selection.descriptor.capabilities.iter().any(|capability| {
                capability.model_id.as_deref() == Some(PARAKEET_MODEL_ID)
                    && matches!(&capability.operation,
                        SpeechOperationCapability::Transcription(value)
                            if value.streaming && value.accepted_audio.contains(&AcceptedAudio::Pcm))
            });
            if !supports_stream { return Err("speech_live_pcm_route_unavailable".to_string()); }
            // A reused UI operation label must not let an old turn's guard cancel
            // a different turn. The activation owner must still allocate fresh
            // capture/turn identities and prohibit same-source re-admission.
            let mut request_hash = Sha256::new();
            for value in [operation, input.identity().turn.as_str(),
                input.identity().capture.as_str(), input.identity().conversation.as_str()] {
                request_hash.update((value.len() as u64).to_le_bytes());
                request_hash.update(value.as_bytes());
            }
            let request_id = SpeechRequestId(format!("mom-live-transcribe-{:x}", request_hash.finalize()));
            self.begin_operation(operation, request_id.clone(), OperationKind::Transcription)
                .map_err(|error| error.message)?;
            let mut guard = LiveSpeechOperation { speech: self, operation, request_id: request_id.clone(), armed: true };
            let request = TranscriptionRequest {
                context: request_context(request_id.clone(), SpeechRouteSelector::ExactBackend {
                    backend_id: PARAKEET_BACKEND_ID.to_string(),
                    model_id: Some(PARAKEET_MODEL_ID.to_string()), voice_id: None,
                }, 90_000),
                input: TranscriptionInput::Stream { stream_id: input.identity().turn.clone(), format: live_pcm_format() },
                language: Some("en".to_string()), task: TranscriptionTask::Transcribe,
                timestamps: TimestampGranularity::None, diarization: DiarizationPolicy::Disabled,
                partial_results: false, punctuation: true, hotwords: Vec::new(),
            };
            let (response, source) = self.executor.transcribe_live(request, input).await?;
            emit("final_result", None);
            validate_parakeet_response(&response, &request_id).map_err(|error| error.message)?;
            select_resident_parakeet(&self.executor.current_descriptors().map_err(|error| error.to_string())?)
                .map_err(|error| error.message)?;
            let candidate = FinalTranscript::from_complete(source, response.text).map_err(|error| error.to_string())?;
            self.complete_operation(operation, &request_id).map_err(|error| error.message)?;
            guard.armed = false;
            // A final candidate is not a store commit. No caller may present
            // this stage as successful replacement of audio in model history.
            emit("final_uncommitted", None);
            Ok(candidate)
        }.await;
        if let Err(error) = &result {
            emit("failed_audio_retained", Some(error.clone()));
        }
        result
    }

    /// Reports whether the exact Parakeet model is already resident. Live
    /// voice-call slots may only use a resident transcriber; loading it while
    /// the caller is waiting would add an unbounded cold-start interval.
    pub fn parakeet_is_resident(&self) -> Result<bool, String> {
        let descriptors = self
            .executor
            .current_descriptors()
            .map_err(|error| error.to_string())?;
        Ok(select_resident_parakeet(&descriptors).is_ok())
    }

    pub async fn transcribe_attachment(
        &self,
        conversation_id: &str,
        anchor: &AttachmentPreviewAnchor,
        operation: &str,
    ) -> anyhow::Result<CommandResult<AttachmentTranscriptionOutput>> {
        validate_operation_token(operation)?;
        let input =
            match mom_llama_runtime::attachment_transcription_input(conversation_id, anchor)? {
                Ok(input) => input,
                Err(blocker) => {
                    return Ok(blocked("mom_llama.speech_transcribe_attachment", blocker));
                }
            };
        let _deferred_selection = match select_parakeet(&self.executor.current_descriptors()?) {
            Ok(selection) => selection,
            Err(blocker) => {
                return Ok(blocked("mom_llama.speech_transcribe_attachment", blocker));
            }
        };
        let request_id = SpeechRequestId(format!("mom-transcribe-{operation}"));
        if let Err(blocker) =
            self.begin_operation(operation, request_id.clone(), OperationKind::Transcription)
        {
            return Ok(blocked("mom_llama.speech_transcribe_attachment", blocker));
        }
        let response = self
            .executor
            .transcribe(TranscriptionRequest {
                context: request_context(
                    request_id.clone(),
                    SpeechRouteSelector::ExactBackend {
                        backend_id: PARAKEET_BACKEND_ID.to_string(),
                        model_id: Some(PARAKEET_MODEL_ID.to_string()),
                        voice_id: None,
                    },
                    1_800_000,
                ),
                input: TranscriptionInput::Complete {
                    audio: AudioInput::Encoded {
                        format: EncodedAudioFormat::Wav,
                        data: input.bytes.clone(),
                    },
                },
                language: Some("en".to_string()),
                task: TranscriptionTask::Transcribe,
                timestamps: TimestampGranularity::None,
                diarization: DiarizationPolicy::Disabled,
                partial_results: false,
                punctuation: true,
                hotwords: Vec::new(),
            })
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let cancelled = self.finish_operation(operation, &request_id);
                return Ok(blocked(
                    "mom_llama.speech_transcribe_attachment",
                    if cancelled {
                        speech_cancelled_blocker()
                    } else {
                        speech_failure_blocker(&error)
                    },
                ));
            }
        };
        let current = mom_llama_runtime::attachment_transcription_input(conversation_id, anchor)
            .inspect_err(|_| {
                self.finish_operation(operation, &request_id);
            })?;
        if match &current {
            Ok(current) => {
                current.anchor != input.anchor
                    || current.source_object_id != input.source_object_id
                    || current.blob_object_id != input.blob_object_id
                    || current.bytes_sha256 != input.bytes_sha256
            }
            Err(_) => true,
        } {
            self.finish_operation(operation, &request_id);
            return Ok(blocked(
                "mom_llama.speech_transcribe_attachment",
                Blocker::new(
                    "speech_transcription_stale",
                    "The exact attachment artifact changed before its transcript could be published.",
                    vec!["Transcribe the current attachment again.".to_string()],
                ),
            ));
        }
        if let Err(blocker) = validate_parakeet_response(&response, &request_id) {
            self.finish_operation(operation, &request_id);
            return Ok(blocked("mom_llama.speech_transcribe_attachment", blocker));
        }
        let descriptors = self.executor.current_descriptors().inspect_err(|_| {
            self.finish_operation(operation, &request_id);
        })?;
        let selection = match select_resident_parakeet(&descriptors) {
            Ok(selection) => selection,
            Err(blocker) => {
                self.finish_operation(operation, &request_id);
                return Ok(blocked("mom_llama.speech_transcribe_attachment", blocker));
            }
        };
        let provenance = transcription_provenance(conversation_id, &input, &selection, &response);
        if let Err(blocker) = self.complete_operation(operation, &request_id) {
            return Ok(blocked("mom_llama.speech_transcribe_attachment", blocker));
        }
        let output = AttachmentTranscriptionOutput {
            transcript: response.text,
            language: response.language,
            provenance: provenance.clone(),
            timing: AttachmentTranscriptionTiming {
                input_audio_ms: response.usage.input_audio_ms,
                queue_us: response.usage.queue_us,
                host_admission_us: response.usage.host_admission_us,
                model_load_ms: response.usage.model_load_ms,
                first_result_ms: response.usage.time_to_first_result_ms,
                total_ms: response.usage.total_ms,
            },
            insertion_requires_explicit_normal_edit: true,
        };
        Ok(CommandResult::passed(
            "mom_llama.speech_transcribe_attachment",
            "host_integrated",
            output,
            Vec::new(),
            vec![
                format!("attachment:{}", provenance.attachment_id),
                format!("attachment-root:{}", provenance.root_sha256),
                format!("attachment-artifact:{}", provenance.artifact_id),
                format!("attachment-blob:{}", provenance.blob_object_id),
                format!("attachment-policy:{}", provenance.policy_fingerprint),
                format!("speech-model:{}", provenance.model_id),
                format!("speech-model-sha256:{}", provenance.model_content_sha256),
            ],
            true,
            false,
        ))
    }

    pub fn stop(
        &self,
        operation: Option<&str>,
        playback: Option<&str>,
    ) -> anyhow::Result<CommandResult<SpeechStopOutput>> {
        if operation.is_some() == playback.is_some() {
            return Ok(blocked(
                "mom_llama.speech_stop",
                Blocker::new(
                    "speech_stop_target_invalid",
                    "Stop must name exactly one opaque speech operation or playback.",
                    Vec::new(),
                ),
            ));
        }
        let mut cancellation_requested = false;
        let mut playback_released = false;
        if let Some(operation) = operation {
            validate_operation_token(operation)?;
            let request_id = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Mom speech operation state is unavailable"))?;
                let request_id = state.active.get_mut(operation).map(|active| {
                    active.cancelled = true;
                    active.request_id.clone()
                });
                if request_id.is_none() && state.phase == SpeechPhase::Running {
                    remember_early_cancel(&mut state, operation);
                }
                request_id
            };
            cancellation_requested = true;
            if let Some(request_id) = request_id {
                let _ = self.executor.cancel(&request_id);
            }
        }
        if let Some(playback_id) = playback {
            playback_released = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("Mom speech playback state is unavailable"))?
                .playback
                .remove(playback_id)
                .is_some();
        }
        Ok(CommandResult::passed(
            "mom_llama.speech_stop",
            "host_integrated",
            SpeechStopOutput {
                operation: operation.map(str::to_string),
                playback: playback.map(str::to_string),
                cancellation_requested,
                playback_released,
            },
            Vec::new(),
            Vec::new(),
            false,
            false,
        ))
    }

    /// Cancels one transcription and waits until its exact speech operation
    /// has left the active registry. The host removes the operation only
    /// after the backend ticket reaches its terminal, so foreground compute
    /// must not start based on the cancellation request alone.
    pub async fn stop_and_wait(
        &self,
        operation: &str,
        allow_cold_transcription_settlement: bool,
    ) -> Result<SpeechStopWaitOutput, String> {
        validate_operation_token(operation).map_err(|error| error.to_string())?;
        let stopped = self
            .stop(Some(operation), None)
            .map_err(|error| error.to_string())?
            .result
            .ok_or_else(|| "speech_stop_wait_receipt_missing".to_string())?;
        let request_id = self
            .state
            .lock()
            .map_err(|_| "Mom speech operation state is unavailable".to_string())?
            .active
            .get(operation)
            .map(|active| active.request_id.clone());
        let Some(request_id) = request_id else {
            // If stop raced ahead of begin_operation, stop() retained an
            // early-cancel fact; the future backend admission will be refused.
            return Ok(SpeechStopWaitOutput {
                stop: stopped,
                terminal_acknowledged: true,
            });
        };

        let mut changed = self.operation_revision.subscribe();
        let timeout = speech_stop_wait_timeout(&request_id, allow_cold_transcription_settlement);
        tokio::time::timeout(timeout, async {
            loop {
                let settled = self
                    .state
                    .lock()
                    .map_err(|_| "Mom speech operation state is unavailable".to_string())?
                    .active
                    .get(operation)
                    .is_none_or(|active| active.request_id != request_id);
                if settled {
                    return Ok::<(), String>(());
                }
                changed
                    .changed()
                    .await
                    .map_err(|_| "speech_stop_wait_state_closed".to_string())?;
            }
        })
        .await
        .map_err(|_| "speech_stop_wait_terminal_timeout".to_string())??;
        Ok(SpeechStopWaitOutput {
            stop: stopped,
            terminal_acknowledged: true,
        })
    }

    pub fn begin_quiesce(&self) {
        if let Ok(mut backpressure) = self.generation_backpressure.lock() {
            backpressure.clear();
        }
        let active = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if state.phase != SpeechPhase::Running {
                return;
            }
            state.phase = SpeechPhase::Quiescing;
            state.playback.clear();
            state
                .active
                .values_mut()
                .map(|active| {
                    active.cancelled = true;
                    active.request_id.clone()
                })
                .collect::<Vec<_>>()
        };
        let _ = self.executor.quiesce();
        for request_id in active {
            let _ = self.executor.cancel(&request_id);
        }
    }

    pub async fn shutdown(&self) -> Result<SpeechShutdown, String> {
        self.begin_quiesce();
        let joined = self.executor.shutdown().await;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Mom speech shutdown state is unavailable".to_string())?;
        if joined.is_ok() {
            state.phase = SpeechPhase::Closed;
        }
        let receipt = SpeechShutdown {
            host_joined: joined.is_ok(),
            active_operation_count: state.active.len(),
            retained_playback_count: state.playback.len(),
        };
        joined.map_err(|error| error.to_string())?;
        Ok(receipt)
    }

    fn begin_operation(
        &self,
        operation: &str,
        request_id: SpeechRequestId,
        _kind: OperationKind,
    ) -> Result<(), Blocker> {
        let mut state = self.state.lock().map_err(|_| {
            Blocker::new(
                "speech_state_unavailable",
                "Mom speech operation state is unavailable.",
                Vec::new(),
            )
        })?;
        if state.phase != SpeechPhase::Running {
            return Err(Blocker::new(
                "speech_admission_closed",
                "Mom is shutting down; new speech work is not admitted.",
                Vec::new(),
            ));
        }
        if state.early_cancel.remove(operation) {
            state.early_cancel_order.retain(|value| value != operation);
            return Err(Blocker::new(
                "speech_cancelled",
                "The speech operation was stopped before backend admission.",
                Vec::new(),
            ));
        }
        if state.active.len() >= MAX_ACTIVE_SPEECH_OPERATIONS {
            return Err(Blocker::new(
                "speech_operation_limit",
                "Mom already has the maximum number of bounded speech operations.",
                vec!["Stop an active speech operation and try again.".to_string()],
            ));
        }
        if state.active.contains_key(operation) {
            return Err(Blocker::new(
                "speech_operation_duplicate",
                "That opaque speech operation identifier is already active.",
                Vec::new(),
            ));
        }
        state.active.insert(
            operation.to_string(),
            ActiveOperation {
                request_id,
                cancelled: false,
            },
        );
        Ok(())
    }

    /// Releases an exact operation after a failed or rejected backend result.
    /// Returns whether Stop or application close won before this terminal.
    fn finish_operation(&self, operation: &str, request_id: &SpeechRequestId) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return true;
        };
        let cancelled = state.phase != SpeechPhase::Running
            || state
                .active
                .get(operation)
                .is_none_or(|active| active.request_id != *request_id || active.cancelled);
        let removed = state
            .active
            .get(operation)
            .is_some_and(|active| active.request_id == *request_id)
            && state.active.remove(operation).is_some();
        drop(state);
        if removed {
            self.operation_revision.send_modify(|revision| {
                *revision = revision.wrapping_add(1);
            });
        }
        cancelled
    }

    /// Linearization point for transcript publication. Stop and Quit mutate
    /// the same registry, so either they win and suppress the result or this
    /// exact request reaches one successful terminal first.
    fn complete_operation(
        &self,
        operation: &str,
        request_id: &SpeechRequestId,
    ) -> Result<(), Blocker> {
        let mut state = self.state.lock().map_err(|_| {
            Blocker::new(
                "speech_state_unavailable",
                "Mom speech operation state is unavailable.",
                Vec::new(),
            )
        })?;
        let cancelled = state.phase != SpeechPhase::Running
            || state
                .active
                .get(operation)
                .is_none_or(|active| active.request_id != *request_id || active.cancelled);
        let removed = state
            .active
            .get(operation)
            .is_some_and(|active| active.request_id == *request_id)
            && state.active.remove(operation).is_some();
        drop(state);
        if removed {
            self.operation_revision.send_modify(|revision| {
                *revision = revision.wrapping_add(1);
            });
        }
        if cancelled {
            return Err(speech_cancelled_blocker());
        }
        Ok(())
    }

    fn publish_playback(
        &self,
        operation: &str,
        request_id: &SpeechRequestId,
        binding: ReadAloudBinding,
    ) -> Result<String, Blocker> {
        if binding.request_id != request_id.0 {
            self.finish_operation(operation, request_id);
            return Err(speech_provenance_mismatch(
                "Qwen playback binding did not match the admitted request and streamed audio.",
            ));
        }
        let mut state = self.state.lock().map_err(|_| {
            Blocker::new(
                "speech_state_unavailable",
                "Mom speech playback state is unavailable.",
                Vec::new(),
            )
        })?;
        let cancelled = state.phase != SpeechPhase::Running
            || state
                .active
                .get(operation)
                .is_none_or(|active| active.request_id != *request_id || active.cancelled);
        if state
            .active
            .get(operation)
            .is_some_and(|active| active.request_id == *request_id)
        {
            state.active.remove(operation);
        }
        if cancelled {
            return Err(speech_cancelled_blocker());
        }
        while state.playback.len() >= MAX_PLAYBACKS {
            let Some(oldest) = state.playback.keys().next().cloned() else {
                break;
            };
            state.playback.remove(&oldest);
        }
        let playback_id = format!("mom-playback-{}", SpeechRequestId::new().0);
        state.playback.insert(playback_id.clone(), PlaybackAudio);
        Ok(playback_id)
    }
}

fn assistant_message_target(
    conversation_id: &str,
    message_id: &str,
) -> anyhow::Result<Result<AssistantMessageTarget, Blocker>> {
    let conversations = mom_llama_runtime::conversation_list()?;
    let Some(conversation) = conversations
        .result
        .unwrap_or_default()
        .into_iter()
        .find(|conversation| conversation.id == conversation_id)
    else {
        return Ok(Err(Blocker::new(
            "conversation_not_found",
            "The requested conversation is unavailable.",
            Vec::new(),
        )));
    };
    let Some(message) = conversation
        .messages
        .iter()
        .find(|message| message.id == message_id)
    else {
        return Ok(Err(Blocker::new(
            "message_not_found",
            "The requested message is unavailable.",
            Vec::new(),
        )));
    };
    if message.role != MessageRole::Assistant {
        return Ok(Err(Blocker::new(
            "speech_read_aloud_assistant_only",
            "Read Aloud accepts an exact assistant message only.",
            Vec::new(),
        )));
    }
    if message.content.trim().is_empty() {
        return Ok(Err(Blocker::new(
            "speech_read_aloud_empty",
            "The assistant message has no text to read aloud.",
            Vec::new(),
        )));
    }
    Ok(Ok(AssistantMessageTarget {
        conversation_id: conversation_id.to_string(),
        message_id: message_id.to_string(),
        text_sha256: sha256_hex(message.content.as_bytes()),
        text: message.content.clone(),
        persona_voice_id: persona_voice_key_for_conversation(
            &conversation.recipient_ids,
            conversation.source_conversation_id.as_deref(),
        )
        // Persona voices use manifest-admitted local references with the
        // Qwen Base ICL route; generic speech keeps the VoiceDesign route.
        .map(|key| format!("mom.qwen.icl.{key}")),
    }))
}

fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn persona_voice_key_for_conversation<'a>(
    recipient_ids: &'a [String],
    source_conversation_id: Option<&'a str>,
) -> Option<&'a str> {
    // Persona chats intentionally keep their visible recipient list empty.
    // Their durable persona identity lives on the source conversation.
    recipient_ids
        .first()
        .filter(|_| recipient_ids.len() == 1)
        .and_then(|persona| persona_voice_key(persona))
        .or_else(|| source_conversation_id.and_then(persona_voice_key))
}

fn persona_voice_key(persona_id: &str) -> Option<&str> {
    let persona = persona_id.strip_prefix("persona-").unwrap_or(persona_id);
    match persona {
        "robert_miller_fsap" => Some("robert_miller"),
        "arnold_popky_detur" => Some("arnold_popky"),
        "shirley_jean_schmidt_dnms" => Some("shirley_jean_schmidt"),
        value
            if !value.is_empty()
                && value.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                }) =>
        {
            Some(value)
        }
        _ => None,
    }
}

fn select_read_aloud(
    descriptors: &[SpeechBackendDescriptor],
    input_characters: usize,
    persona_voice_id: Option<&str>,
    _require_persona_voice: bool,
) -> Result<ReadAloudSelection, Blocker> {
    let Some(voice_id) = persona_voice_id.filter(|voice_id| voice_id.starts_with("mom.qwen.icl."))
    else {
        return Err(speech_unavailable(
            "The selected consultant's streaming Qwen voice is unavailable.",
        ));
    };
    if let Some(descriptor) = descriptors.iter().find(|descriptor| {
        descriptor.id == QWEN_BACKEND_ID
            && descriptor.readiness.is_ready()
            && descriptor.capabilities.iter().any(|capability| {
                (capability.eligible_for_local_only() || capability.deferred_load_admissible())
                    && capability.model_id.as_deref() == Some(QWEN_ICL_MODEL_ID)
                    && capability.limits.max_input_characters.is_none_or(|limit| {
                        u64::try_from(input_characters).unwrap_or(u64::MAX) <= limit
                    })
                    && matches!(
                        &capability.operation,
                        SpeechOperationCapability::Synthesis(synthesis)
                            if synthesis.streaming_audio
                                && synthesis.voice_selection
                                && synthesis.returned_audio.contains(&AudioOutputKind::Pcm)
                    )
            })
    }) && descriptor
        .voices
        .iter()
        .any(|voice| voice.installed && voice.id == voice_id)
    {
        return Ok(ReadAloudSelection::Qwen(QwenSelection {
            descriptor_sha256: descriptor_sha256(descriptor)?,
            descriptor: descriptor.clone(),
            model_id: QWEN_ICL_MODEL_ID.to_string(),
            voice_id: voice_id.to_string(),
        }));
    }
    Err(speech_unavailable(
        "The selected consultant's streaming Qwen voice is unavailable.",
    ))
}

fn qwen_stream_output() -> AudioOutputFormat {
    AudioOutputFormat::Pcm {
        format: PcmFormat {
            sample_rate_hz: 24_000,
            channels: 1,
            sample_format: PcmSampleFormat::I16Le,
            interleaved: true,
        },
    }
}

fn read_aloud_request(
    target: &AssistantMessageTarget,
    selection: &ReadAloudSelection,
    request_id: SpeechRequestId,
) -> SynthesisRequest {
    let voice_id = selection.voice_id();
    SynthesisRequest {
        context: request_context(
            request_id,
            SpeechRouteSelector::ExactBackend {
                backend_id: selection.backend_id().to_string(),
                model_id: selection.model_id(),
                voice_id: Some(voice_id.clone()),
            },
            240_000,
        ),
        input: SynthesisInput::Text {
            text: target.text.clone(),
        },
        voice: VoiceSelector::Exact { voice_id },
        language: Some(READ_ALOUD_LANGUAGE.to_string()),
        rate: 1.0,
        pitch: 1.0,
        volume: 1.0,
        output: qwen_stream_output(),
        alignment: AlignmentGranularity::None,
        stream: true,
    }
}

fn precommit_synthesis_request(
    text: &str,
    selection: &ReadAloudSelection,
    request_id: SpeechRequestId,
) -> SynthesisRequest {
    let voice_id = selection.voice_id();
    SynthesisRequest {
        context: request_context(
            request_id,
            SpeechRouteSelector::ExactBackend {
                backend_id: selection.backend_id().to_string(),
                model_id: selection.model_id(),
                voice_id: Some(voice_id.clone()),
            },
            240_000,
        ),
        input: SynthesisInput::Text {
            text: text.to_string(),
        },
        voice: VoiceSelector::Exact { voice_id },
        language: Some(READ_ALOUD_LANGUAGE.to_string()),
        rate: 1.0,
        pitch: 1.0,
        volume: 1.0,
        output: qwen_stream_output(),
        alignment: AlignmentGranularity::None,
        stream: true,
    }
}

const READ_ALOUD_LANGUAGE: &str = "en-US";

fn select_parakeet(descriptors: &[SpeechBackendDescriptor]) -> Result<ParakeetSelection, Blocker> {
    select_parakeet_with_residency(descriptors, true)
}

fn select_resident_parakeet(
    descriptors: &[SpeechBackendDescriptor],
) -> Result<ParakeetSelection, Blocker> {
    select_parakeet_with_residency(descriptors, false)
}

fn select_parakeet_with_residency(
    descriptors: &[SpeechBackendDescriptor],
    allow_deferred: bool,
) -> Result<ParakeetSelection, Blocker> {
    let Some(descriptor) = descriptors
        .iter()
        .find(|descriptor| descriptor.id == PARAKEET_BACKEND_ID)
    else {
        return Err(speech_unavailable(
            "The verified Parakeet backend is unavailable.",
        ));
    };
    if !matches!(descriptor.readiness, SpeechBackendReadiness::Ready) {
        return Err(Blocker::new(
            "speech_parakeet_model_not_ready",
            "The exact verified Parakeet model is not installed and ready.",
            vec!["Install the manifest-bound local Parakeet model, then restart Mom.".to_string()],
        ));
    }
    let deferred_loader = allow_deferred
        && descriptor
            .capabilities
            .iter()
            .any(is_deferred_parakeet_capability);
    let exact_model = descriptor.models.iter().any(|model| {
        model.id == PARAKEET_MODEL_ID
            && model.content_hash.as_deref() == Some(PARAKEET_MODEL_CONTENT_SHA256)
            && (model.resident || (allow_deferred && deferred_loader))
    });
    let executable = descriptor.capabilities.iter().any(|capability| {
        capability.model_id.as_deref() == Some(PARAKEET_MODEL_ID)
            && (capability.eligible_for_local_only()
                || (allow_deferred && is_deferred_parakeet_capability(capability)))
            && matches!(
                &capability.operation,
                SpeechOperationCapability::Transcription(transcription)
                    if transcription.accepted_audio.contains(&AcceptedAudio::Wav)
            )
    });
    if !exact_model || !executable {
        return Err(speech_unavailable(
            "Parakeet descriptor identity does not match the verified executable model.",
        ));
    }
    Ok(ParakeetSelection {
        descriptor_sha256: descriptor_sha256(descriptor)?,
        descriptor: descriptor.clone(),
    })
}

fn is_deferred_parakeet_capability(capability: &speech_native_types::SpeechCapability) -> bool {
    capability.model_id.as_deref() == Some(PARAKEET_MODEL_ID)
        && capability.deferred_load_admissible()
        && capability.evidence.iter().any(|evidence| {
            evidence.source_id == PARAKEET_DEFERRED_LOAD_EVIDENCE_SOURCE_ID
                && evidence.kind == speech_native_types::EvidenceKind::SystemInventory
                && evidence.outcome == speech_native_types::EvidenceOutcome::Inconclusive
                && !evidence.proves_runtime()
        })
}

fn request_context(
    request_id: SpeechRequestId,
    route: SpeechRouteSelector,
    total_ms: u64,
) -> SpeechRequestContext {
    SpeechRequestContext {
        request_id,
        client_id: MOM_SPEECH_CLIENT_ID.to_string(),
        route,
        routing: SpeechRoutingPolicy {
            privacy: SpeechPrivacyPolicy::LocalOnly,
            profile: SpeechRouteProfile::ConsistentLocal,
            allow_asset_download: false,
            allow_fallback_before_output: false,
        },
        deadline: SpeechDeadlinePolicy {
            queue_ms: Some(5_000),
            model_load_ms: Some(120_000),
            first_result_ms: Some(total_ms),
            idle_stream_ms: None,
            total_ms: Some(total_ms),
        },
    }
}

fn validate_read_aloud_response(
    response: SynthesisResponse,
    request_id: &SpeechRequestId,
    selection: &ReadAloudSelection,
) -> Result<Option<u64>, Blocker> {
    let ReadAloudSelection::Qwen(qwen) = selection;
    if response.request_id != *request_id
        || response.route.backend_id != QWEN_BACKEND_ID
        || response.route.model_id.as_deref() != Some(&qwen.model_id)
        || response.route.voice_id.as_deref() != Some(&qwen.voice_id)
        || response.route.network != NetworkBehavior::Never
        || !response.usage.real_local_inference
    {
        return Err(speech_provenance_mismatch(
            "Qwen3 TTS response route did not match the admitted local model and voice.",
        ));
    }
    match response.output {
        SynthesisOutput::Streamed {
            format,
            emitted_bytes,
        } if format == qwen_stream_output()
            && emitted_bytes > 0
            && emitted_bytes <= u64::try_from(MAX_COMPLETE_WAV_BYTES).unwrap_or(u64::MAX) =>
        {
            Ok(response.duration_ms)
        }
        _ => Err(Blocker::new(
            "speech_stream_invalid",
            "Qwen3 TTS did not complete the admitted PCM stream.",
            Vec::new(),
        )),
    }
}

fn validate_parakeet_response(
    response: &TranscriptionResponse,
    request_id: &SpeechRequestId,
) -> Result<(), Blocker> {
    desktop_speech::validate_parakeet_response(response, request_id).map_err(|_| {
        speech_provenance_mismatch(
            "Parakeet response provenance did not match the exact admitted local model route.",
        )
    })?;
    if response.text.chars().count() > MAX_TRANSCRIPT_CHARACTERS {
        return Err(Blocker::new(
            "speech_transcript_too_large",
            "The complete local transcript exceeds Mom's bounded preview limit.",
            Vec::new(),
        ));
    }
    Ok(())
}

fn transcription_provenance(
    conversation_id: &str,
    input: &AttachmentTranscriptionInput,
    selection: &ParakeetSelection,
    response: &TranscriptionResponse,
) -> AttachmentTranscriptionProvenance {
    AttachmentTranscriptionProvenance {
        conversation_id: conversation_id.to_string(),
        attachment_id: input.anchor.attachment_id.clone(),
        root_sha256: input.anchor.root_sha256.clone(),
        artifact_id: input.anchor.artifact_id.clone(),
        policy_fingerprint: input.anchor.policy_fingerprint.clone(),
        source_object_id: input.source_object_id.clone(),
        blob_object_id: input.blob_object_id.clone(),
        audio_sha256: input.bytes_sha256.clone(),
        media_type: input.media_type.clone(),
        byte_len: input.byte_len,
        validation: format!("{:?}", input.validation).to_lowercase(),
        processor: input.processor.clone(),
        processor_version: input.processor_version.clone(),
        backend_id: selection.descriptor.id.clone(),
        backend_descriptor_sha256: selection.descriptor_sha256.clone(),
        model_id: PARAKEET_MODEL_ID.to_string(),
        model_content_sha256: PARAKEET_MODEL_CONTENT_SHA256.to_string(),
        request_id: response.request_id.0.clone(),
        network: response.route.network,
        real_local_inference: response.usage.real_local_inference,
    }
}

fn validate_operation_token(operation: &str) -> anyhow::Result<()> {
    if !(8..=64).contains(&operation.len())
        || !operation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        anyhow::bail!("speech operation token must be 8-64 opaque ASCII identifier bytes");
    }
    Ok(())
}

fn remember_early_cancel(state: &mut SpeechState, operation: &str) {
    if state.early_cancel.insert(operation.to_string()) {
        state.early_cancel_order.push_back(operation.to_string());
    }
    while state.early_cancel_order.len() > MAX_PENDING_CANCELS {
        if let Some(expired) = state.early_cancel_order.pop_front() {
            state.early_cancel.remove(&expired);
        }
    }
}

fn descriptor_sha256(descriptor: &SpeechBackendDescriptor) -> Result<String, Blocker> {
    serde_json::to_vec(descriptor)
        .map(|bytes| sha256_hex(&bytes))
        .map_err(|_| {
            Blocker::new(
                "speech_descriptor_invalid",
                "The selected speech backend descriptor could not be bound.",
                Vec::new(),
            )
        })
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn blocked<T>(command: &str, blocker: Blocker) -> CommandResult<T>
where
    T: Serialize,
{
    CommandResult::blocked(command, "stub_blocked", blocker)
}

fn speech_unavailable(message: &str) -> Blocker {
    Blocker::new("speech_backend_unavailable", message, Vec::new())
}

fn speech_provenance_mismatch(message: &str) -> Blocker {
    Blocker::new("speech_provenance_mismatch", message, Vec::new())
}

fn speech_failure_blocker(error: &str) -> Blocker {
    let cancelled = error.contains("cancel") || error.contains("Cancelled");
    if cancelled {
        speech_cancelled_blocker()
    } else {
        Blocker::new(
            "speech_backend_failed",
            format!("The local speech backend failed: {error}"),
            Vec::new(),
        )
    }
}

fn speech_cancelled_blocker() -> Blocker {
    Blocker::new(
        "speech_cancelled",
        "The speech operation was stopped; no late result was published.",
        Vec::new(),
    )
}

fn speech_error_message(error: &SpeechError) -> String {
    if error.class == SpeechErrorClass::Cancelled {
        "speech request cancelled".to_string()
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    fn benchmark_pcm16_mono_16khz_wav(path: &Path) -> anyhow::Result<Vec<f32>> {
        use anyhow::{Context, ensure};

        let bytes = std::fs::read(path)
            .with_context(|| format!("read benchmark WAV {}", path.display()))?;
        ensure!(
            bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
            "benchmark audio must be a RIFF/WAVE file"
        );
        let mut cursor = 12usize;
        let mut format = None;
        let mut data = None;
        while cursor.checked_add(8).is_some_and(|end| end <= bytes.len()) {
            let chunk_id = &bytes[cursor..cursor + 4];
            let chunk_len = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into()?) as usize;
            let chunk_start = cursor + 8;
            let chunk_end = chunk_start
                .checked_add(chunk_len)
                .context("benchmark WAV chunk length overflow")?;
            ensure!(chunk_end <= bytes.len(), "truncated benchmark WAV chunk");
            match chunk_id {
                b"fmt " if chunk_len >= 16 => {
                    format = Some((
                        u16::from_le_bytes(bytes[chunk_start..chunk_start + 2].try_into()?),
                        u16::from_le_bytes(bytes[chunk_start + 2..chunk_start + 4].try_into()?),
                        u32::from_le_bytes(bytes[chunk_start + 4..chunk_start + 8].try_into()?),
                        u16::from_le_bytes(bytes[chunk_start + 14..chunk_start + 16].try_into()?),
                    ));
                }
                b"data" => data = Some(&bytes[chunk_start..chunk_end]),
                _ => {}
            }
            cursor = chunk_end + (chunk_len & 1);
        }
        let (encoding, channels, sample_rate, bits) =
            format.context("benchmark WAV has no fmt chunk")?;
        ensure!(
            (encoding, channels, sample_rate, bits) == (1, 1, 16_000, 16),
            "benchmark WAV must be PCM16 mono 16 kHz"
        );
        let data = data.context("benchmark WAV has no data chunk")?;
        ensure!(data.len() % 2 == 0, "benchmark WAV has a partial sample");
        Ok(data
            .chunks_exact(2)
            .map(|sample| f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32_768.0)
            .collect())
    }

    #[cfg(target_os = "macos")]
    fn benchmark_speech_chunk(
        text: &str,
        flush: bool,
        first: bool,
        word_limit: usize,
    ) -> Option<(String, String)> {
        let trimmed = text.trim_start();
        let mut words = Vec::new();
        let mut word_start = None;
        for (index, character) in trimmed.char_indices() {
            if character.is_whitespace() {
                if let Some(start) = word_start.take() {
                    words.push((start, index, true));
                }
            } else if word_start.is_none() {
                word_start = Some(index);
            }
        }
        if let Some(start) = word_start {
            words.push((start, trimmed.len(), false));
        }
        if words.is_empty() {
            return None;
        }
        let minimum = if first { 6 } else { 8 };
        let maximum = word_limit.clamp(minimum, if first { 16 } else { 24 });
        let sentence_boundary_minimum = if first { 2 } else { 3 };
        let weak_boundary_minimum = if first { 3 } else { minimum.min(6) };
        for (index, &(start, end, _)) in words.iter().enumerate().take(maximum) {
            let word = &trimmed[start..end];
            let strong = word
                .trim_end_matches(['"', '\'', ')', ']'])
                .ends_with(['.', '!', '?']);
            let weak = word
                .trim_end_matches(['"', '\'', ')', ']'])
                .ends_with([';', ':', ',']);
            let complete_sentence = strong && (flush || index + 1 >= sentence_boundary_minimum);
            let weak_boundary = weak && index + 1 >= weak_boundary_minimum;
            if complete_sentence || weak_boundary {
                return Some((
                    trimmed[..end].trim().to_string(),
                    trimmed[end..].trim_start().to_string(),
                ));
            }
        }
        if !flush && words.len() < minimum {
            return None;
        }
        let split_at = if flush {
            words.get(words.len().min(maximum) - 1)?.1
        } else {
            let &(_, end, complete) = words.get(minimum - 1)?;
            if !complete && words.len() == minimum {
                return None;
            }
            end
        };
        Some((
            trimmed[..split_at].trim().to_string(),
            trimmed[split_at..].trim_start().to_string(),
        ))
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resident_benchmark_uses_the_product_first_speech_boundary() {
        assert_eq!(
            benchmark_speech_chunk("One two three four five six", false, true, 6),
            None
        );
        assert_eq!(
            benchmark_speech_chunk("One two three four five six ", false, true, 6),
            Some(("One two three four five six".into(), String::new()))
        );
        assert_eq!(
            benchmark_speech_chunk("One two three four five six s", false, true, 6),
            Some(("One two three four five six".into(), "s".into()))
        );
        assert_eq!(
            benchmark_speech_chunk("One two three,", false, true, 6),
            Some(("One two three,".into(), String::new()))
        );
        assert_eq!(
            benchmark_speech_chunk("five six", true, false, 8),
            Some(("five six".into(), String::new()))
        );
        assert_eq!(
            benchmark_speech_chunk(
                "one two three four five six seven eight nine ten",
                true,
                false,
                8
            ),
            Some((
                "one two three four five six seven eight".into(),
                "nine ten".into()
            ))
        );
    }

    /// Runs Gemma audio prefill/decode and the app's Qwen precommit route in
    /// one process. Unlike the external comparison harness, this exercises
    /// the real process-local compute lease and proves whether Gemma actually
    /// yields Metal while Qwen is filling the playback buffer.
    #[test]
    #[ignore = "requires the exact Gemma audio model/projector, Qwen helper/model/voice archive, and benchmark WAV"]
    fn real_resident_audio_to_qwen_pipeline_exercises_compute_lease() -> anyhow::Result<()> {
        use anyhow::{Context, ensure};
        use mom_llama_runtime::config::{SettingsUpdate, set_data_dir_override_for_tests};
        use serde_json::json;
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        use std::sync::mpsc;
        use std::time::Duration;

        let model_path = PathBuf::from(
            std::env::var_os("MOM_LLAMA_MULTIMODAL_MODEL_PATH")
                .context("MOM_LLAMA_MULTIMODAL_MODEL_PATH")?,
        );
        let projector_path = PathBuf::from(
            std::env::var_os("MOM_LLAMA_MM_PROJ_PATH").context("MOM_LLAMA_MM_PROJ_PATH")?,
        );
        let wav_path = PathBuf::from(
            std::env::var_os("MOM_LLAMA_TEST_VOICE_WAV").context("MOM_LLAMA_TEST_VOICE_WAV")?,
        );
        let reference_root = PathBuf::from(
            std::env::var_os("MOM_QWEN3_TTS_VOICE_REFERENCE_ROOT")
                .context("MOM_QWEN3_TTS_VOICE_REFERENCE_ROOT")?,
        );
        ensure!(model_path.is_file(), "Gemma model is missing");
        ensure!(projector_path.is_file(), "audio projector is missing");
        let samples = benchmark_pcm16_mono_16khz_wav(&wav_path)?;
        ensure!(samples.len() >= 15_840, "benchmark WAV is too short");

        let store = tempfile::tempdir()?;
        set_data_dir_override_for_tests(Some(store.path().to_path_buf()));
        struct ResetDataDir;
        impl Drop for ResetDataDir {
            fn drop(&mut self) {
                mom_llama_runtime::config::set_data_dir_override_for_tests(None);
            }
        }
        let _reset = ResetDataDir;
        mom_llama_runtime::settings_update(SettingsUpdate {
            model_path: Some(Some(model_path)),
            mmproj_path: Some(Some(projector_path)),
            temperature: Some(1.0),
            max_tokens: Some(64),
            batch_tokens: Some(128),
            max_parallel_sequences: Some(1),
            native_device: Some(llama_native_types::NativeDevice::Metal),
            upstream_settings: Some(BTreeMap::from([(
                "customJson".into(),
                json!(r#"{"seed":17}"#),
            )])),
            ..SettingsUpdate::default()
        })?;
        let settings = mom_llama_runtime::config::resolve_settings()?;
        let native = mom_llama_runtime::native_runtime::ProductRuntimeOwner::initialize(&settings)?;
        let scope = native.operation_scope();
        let warm = mom_llama_runtime::runtime_warm(&scope)?;
        let warm = warm.result.context("Gemma runtime warm was blocked")?;
        ensure!(
            warm.model_loaded && warm.audio_projector_warmed,
            "Gemma did not warm"
        );

        let async_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let speech = async_runtime
            .block_on(MomSpeech::discover(store.path(), &reference_root))
            .map_err(anyhow::Error::msg)?;
        let conversation =
            mom_llama_runtime::conversation_new(Some("Voice pipeline benchmark".into()))?
                .result
                .context("benchmark conversation")?;
        speech
            .bind_precommit_voice(
                &conversation.id,
                &ConversationPersonaIdentity {
                    recipient_ids: vec!["persona-christine_courtois".into()],
                    source_conversation_id: Some("persona-christine_courtois".into()),
                },
            )
            .map_err(anyhow::Error::msg)?;

        let voice = mom_llama_runtime::voice_audio_begin_in_scope(
            &scope,
            &conversation.id,
            "[Phone call] Reply naturally in exactly three short sentences to what you hear.",
        )?;
        voice.wait_for_resume()?;
        let audio_started = Instant::now();
        for (index, chunk) in samples[..samples.len().min(4 * 16_000)]
            .chunks(160)
            .enumerate()
        {
            voice.push_pcm(chunk)?;
            // Pace from one monotonic audio clock. Repeated relative sleeps
            // accumulate scheduler slop and falsely give prefill extra time;
            // a microphone does not slow down when the consumer is busy.
            let audio_deadline =
                audio_started + Duration::from_millis(u64::try_from(index.saturating_add(1))? * 10);
            std::thread::sleep(audio_deadline.saturating_duration_since(Instant::now()));
        }
        let silence = Instant::now();
        voice.seal_input()?;

        let (speech_tx, speech_rx) = mpsc::channel::<Option<String>>();
        let (tts_tx, tts_rx) = mpsc::sync_channel(1);
        let speech_worker = Arc::clone(&speech);
        let conversation_id = conversation.id.clone();
        let tts_worker = std::thread::Builder::new()
            .name("resident-voice-pipeline-qwen".into())
            .spawn(move || {
                let result = (|| -> anyhow::Result<_> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    let first_pcm_ms = Arc::new(Mutex::new(None));
                    // (segment index, wall-clock arrival from silence, PCM duration).
                    // Keeping the segment identity prevents the deliberate reservoir
                    // wait between requests from being misreported as a producer
                    // packet stall.
                    let packets = Arc::new(Mutex::new(Vec::<(usize, f64, f64)>::new()));
                    let mut pending = String::new();
                    let mut closed = false;
                    let mut first = true;
                    let mut first_dispatch_ms = None;
                    let mut bindings = Vec::new();
                    let mut segments = Vec::new();
                    let mut scheduler_wait_ms = 0.0_f64;
                    let mut target_runway_ms = 180.0_f64;
                    loop {
                        while let Ok(event) = speech_rx.try_recv() {
                            match event {
                                Some(delta) => pending.push_str(&delta),
                                None => closed = true,
                            }
                        }
                        let word_limit = if first { 6 } else { 8 };
                        let Some((phrase, rest)) =
                            benchmark_speech_chunk(&pending, closed, first, word_limit)
                        else {
                            if closed {
                                break;
                            }
                            match speech_rx.recv()? {
                                Some(delta) => pending.push_str(&delta),
                                None => closed = true,
                            }
                            continue;
                        };
                        pending = rest;
                        if !first {
                            let wait_ms = packets
                                .lock()
                                .ok()
                                .and_then(|packets| {
                                    let first_arrival = packets.first()?.1;
                                    let audio_ms =
                                        packets.iter().map(|packet| packet.2).sum::<f64>();
                                    let now_ms = silence.elapsed().as_secs_f64() * 1_000.0;
                                    let consumed_ms = (now_ms - first_arrival - 16.0).max(0.0);
                                    Some((audio_ms - consumed_ms - target_runway_ms).max(0.0))
                                })
                                .unwrap_or(0.0);
                            if wait_ms > 0.0 {
                                std::thread::sleep(Duration::from_secs_f64(wait_ms / 1_000.0));
                                scheduler_wait_ms += wait_ms;
                            }
                        }
                        let dispatch_ms = silence.elapsed().as_secs_f64() * 1_000.0;
                        first_dispatch_ms.get_or_insert(dispatch_ms);
                        let segment_index = segments.len();
                        let observed_first_pcm = Arc::clone(&first_pcm_ms);
                        let observed_packets = Arc::clone(&packets);
                        let sink: PcmStreamSink = Arc::new(move |chunk| {
                            let arrival_ms = silence.elapsed().as_secs_f64() * 1_000.0;
                            if let Ok(mut first) = observed_first_pcm.lock() {
                                first.get_or_insert(arrival_ms);
                            }
                            if !chunk.data.is_empty()
                                && let Ok(mut packets) = observed_packets.lock()
                            {
                                let frames = chunk.data.len() / chunk.format.bytes_per_frame();
                                let audio_ms = frames as f64 * 1_000.0
                                    / f64::from(chunk.format.sample_rate_hz);
                                packets.push((segment_index, arrival_ms, audio_ms));
                            }
                            Ok(())
                        });
                        let operation =
                            format!("resident-pipeline-benchmark-{}", segments.len() + 1);
                        let packet_start = packets.lock().map(|packets| packets.len()).unwrap_or(0);
                        let result = runtime.block_on(speech_worker.synthesize_precommit(
                            &conversation_id,
                            &operation,
                            &phrase,
                            sink,
                        ))?;
                        let binding = result
                            .result
                            .with_context(|| {
                                format!(
                                    "Qwen segment was blocked: readiness={}, blocker={:?}",
                                    result.readiness, result.blocker
                                )
                            })?
                            .binding;
                        let (
                            segment_first_pcm_ms,
                            first_packet_audio_ms,
                            maximum_packet_gap_ms,
                            boundary_gap_ms,
                        ) = packets
                            .lock()
                            .map(|packets| {
                                let segment_packets = &packets[packet_start.min(packets.len())..];
                                let segment_first_pcm_ms =
                                    segment_packets.first().map(|packet| packet.1);
                                let first_audio =
                                    segment_packets.first().map_or(139.0, |packet| packet.2);
                                let maximum_gap = segment_packets
                                    .windows(2)
                                    .map(|pair| pair[1].1 - pair[0].1)
                                    .fold(0.0_f64, f64::max);
                                let boundary_gap = packet_start
                                    .checked_sub(1)
                                    .and_then(|previous| packets.get(previous))
                                    .zip(segment_packets.first())
                                    .map(|(previous, first)| first.1 - previous.1);
                                (segment_first_pcm_ms, first_audio, maximum_gap, boundary_gap)
                            })
                            .unwrap_or((None, 139.0, 139.0, None));
                        let first_audio_ms = binding
                            .tts_time_to_first_audio_ms
                            .map_or(140.0, |value| value as f64);
                        let production_rtf = match (binding.tts_total_ms, binding.duration_ms) {
                            (Some(synthesis), Some(audio)) if audio > 0 => {
                                synthesis as f64 / audio as f64
                            }
                            _ => 0.7,
                        };
                        let uncovered_packet_gap_ms =
                            (maximum_packet_gap_ms - first_packet_audio_ms).max(0.0);
                        let refill_runway_ms = (first_audio_ms + uncovered_packet_gap_ms + 16.0)
                            .max(first_packet_audio_ms * 1.25)
                            .max(120.0);
                        target_runway_ms = (refill_runway_ms
                            + (production_rtf - 0.8).max(0.0) * 800.0)
                            .clamp(180.0, 1_200.0);
                        segments.push(serde_json::json!({
                            "text": phrase,
                            "dispatch_ms": dispatch_ms,
                            "first_pcm_ms": segment_first_pcm_ms,
                            "first_packet_audio_ms": first_packet_audio_ms,
                            "maximum_internal_packet_gap_ms": maximum_packet_gap_ms,
                            "preceding_segment_boundary_gap_ms": boundary_gap_ms,
                            "tts_total_ms": binding.tts_total_ms,
                            "duration_ms": binding.duration_ms,
                            "generation_yield_us": binding.tts_generation_yield_us,
                            "compute_lease_held_us": binding.tts_compute_lease_held_us,
                            "next_target_runway_ms": target_runway_ms,
                        }));
                        bindings.push(binding);
                        first = false;
                    }
                    let first_pcm_ms = first_pcm_ms.lock().ok().and_then(|value| *value);
                    let packets = packets
                        .lock()
                        .map(|value| value.clone())
                        .unwrap_or_default();
                    anyhow::Ok((
                        first_dispatch_ms,
                        first_pcm_ms,
                        packets,
                        bindings,
                        segments,
                        scheduler_wait_ms,
                    ))
                })();
                // Preserve the worker's original error for the accepting test
                // thread. A dropped text receiver must not replace a Qwen
                // failure with an unrelated channel-send error.
                let _ = tts_tx.send(result);
            })?;

        let mut first_delta_ms = None;
        let completed = mom_llama_runtime::voice_audio_finish_in_scope(
            voice,
            mom_llama_runtime::ChatSendOptions {
                timeout_s: 600.0,
                fake_fixture: false,
            },
            Some(|event: mom_llama_runtime::ChatStreamEvent| {
                if matches!(event.event.as_str(), "delta" | "reasoning_delta")
                    && let Some(delta) = event.delta
                {
                    first_delta_ms.get_or_insert_with(|| silence.elapsed().as_secs_f64() * 1_000.0);
                    let _ = speech_tx.send(Some(delta));
                }
                Ok(())
            }),
        )?;
        let output = completed.result.context("Gemma voice output")?;
        let gemma_complete_ms = silence.elapsed().as_secs_f64() * 1_000.0;
        println!(
            "VOICE_RESIDENT_GEMMA_STREAMING_BENCHMARK {}",
            serde_json::to_string(&serde_json::json!({
                "silence_to_first_token_ms": first_delta_ms,
                "engine_start_to_first_token_ms": output.first_token_ms,
                "silence_to_gemma_complete_ms": gemma_complete_ms,
                "gemma_completion_tokens": output.completion_tokens,
                "audio": output.voice_audio_metrics,
            }))?
        );
        // The Qwen worker can fail and close its text receiver before Gemma
        // finishes. Preserve that original worker error instead of replacing
        // it with an unrelated channel-send failure here.
        let _ = speech_tx.send(None);
        drop(speech_tx);
        let (dispatch_ms, first_pcm_ms, packets, bindings, segments, scheduler_wait_ms) =
            tts_rx.recv_timeout(Duration::from_secs(120))??;
        let tts_pipeline_complete_ms = silence.elapsed().as_secs_f64() * 1_000.0;
        tts_worker
            .join()
            .map_err(|_| anyhow::anyhow!("Qwen worker panicked"))?;
        ensure!(!bindings.is_empty(), "Gemma emitted no speakable phrase");
        ensure!(
            bindings
                .first()
                .and_then(|binding| binding.tts_helper_prepared_prefix_cache_hit)
                .is_some(),
            "foreground Qwen synthesis omitted its persona-prefix cache telemetry"
        );
        let generation_yields = bindings
            .iter()
            .map(|binding| binding.tts_generation_yields)
            .sum::<u64>();
        let generation_yield_us = bindings
            .iter()
            .map(|binding| binding.tts_generation_yield_us)
            .sum::<u64>();
        let generation_fair_passes = bindings
            .iter()
            .map(|binding| binding.tts_generation_fair_passes)
            .sum::<u64>();
        let compute_lease_held_us = bindings
            .iter()
            .map(|binding| binding.tts_compute_lease_held_us)
            .sum::<u64>();
        let compute_lease_released_us = bindings
            .iter()
            .map(|binding| binding.tts_compute_lease_released_us)
            .sum::<u64>();
        let tts_total_ms = bindings
            .iter()
            .filter_map(|binding| binding.tts_total_ms)
            .sum::<u64>();
        let tts_output_audio_ms = bindings
            .iter()
            .filter_map(|binding| binding.duration_ms)
            .sum::<u64>();
        let minimum_lead_ms = bindings
            .iter()
            .filter_map(|binding| binding.tts_compute_minimum_lead_ms)
            .min();
        let maximum_lead_ms = bindings
            .iter()
            .filter_map(|binding| binding.tts_compute_maximum_lead_ms)
            .max();
        let maximum_internal_packet_gap_ms = packets
            .windows(2)
            .filter(|pair| pair[0].0 == pair[1].0)
            .map(|pair| pair[1].1 - pair[0].1)
            .fold(0.0_f64, f64::max);
        let maximum_segment_boundary_gap_ms = packets
            .windows(2)
            .filter(|pair| pair[0].0 != pair[1].0)
            .map(|pair| pair[1].1 - pair[0].1)
            .fold(0.0_f64, f64::max);
        let mut preceding_audio_ms = 0.0_f64;
        let required_startup_delay_ms = packets.first().map_or(0.0, |first| {
            let first_arrival_ms = first.1;
            packets.iter().fold(0.0_f64, |required, packet| {
                let deficit = packet.1 - first_arrival_ms - preceding_audio_ms;
                preceding_audio_ms += packet.2;
                required.max(deficit)
            })
        });
        let audio = output.voice_audio_metrics.as_ref();
        let audio_finish_to_last_projector_finish_us = audio.and_then(|metrics| {
            metrics
                .finish_us
                .zip(metrics.last_projector_finish_us)
                .map(|(finish, projector_finish)| projector_finish.saturating_sub(finish))
        });
        let audio_finish_to_decode_start_us = audio.and_then(|metrics| {
            metrics
                .finish_us
                .zip(metrics.decode_start_us)
                .map(|(finish, decode_start)| decode_start.saturating_sub(finish))
        });
        let audio_decode_start_to_first_token_ms = first_delta_ms
            .zip(audio_finish_to_decode_start_us.map(|elapsed| elapsed as f64 / 1_000.0))
            .map(|(first_token, finish_to_decode)| (first_token - finish_to_decode).max(0.0));
        let audio_receipt = serde_json::json!({
            "streamed_audio_ms": audio.map(|metrics| metrics.accepted_samples as f64 / 16.0),
            "projected_blocks": audio.map(|metrics| metrics.projected_blocks),
            "projected_samples": audio.map(|metrics| metrics.projected_samples),
            "tail_samples": audio.map(|metrics| metrics.tail_samples),
            "projector_us": audio.map(|metrics| metrics.projector_us),
            "tail_projector_us": audio.map(|metrics| metrics.tail_projector_us),
            "finish_us": audio.and_then(|metrics| metrics.finish_us),
            "last_projector_finish_us": audio.and_then(|metrics| metrics.last_projector_finish_us),
            "decode_start_us": audio.and_then(|metrics| metrics.decode_start_us),
            "finish_to_last_projector_finish_us": audio_finish_to_last_projector_finish_us,
            "finish_to_decode_start_us": audio_finish_to_decode_start_us,
            "decode_start_to_first_token_ms": audio_decode_start_to_first_token_ms,
        });
        let qwen_startup_warm = speech._qwen_startup_warm.map(|warm| {
            serde_json::json!({
                "completed": warm.completed,
                "total": warm.total,
                "resident_us": warm.resident_us,
                "probe_us": warm.probe_us,
                "total_us": warm.total_us,
            })
        });
        let receipt = serde_json::json!({
            "runtime_warm": {
                "audio_projector_warmed": warm.audio_projector_warmed,
                "audio_projector_warm_us": warm.audio_projector_warm_us,
                "model_acquire_us": warm.model_acquire_us,
            },
            "silence_to_first_token_ms": first_delta_ms,
            "engine_start_to_first_token_ms": output.first_token_ms,
            "audio": audio_receipt,
            "silence_to_tts_dispatch_ms": dispatch_ms,
            "silence_to_first_pcm_ms": first_pcm_ms,
            "silence_to_gemma_complete_ms": gemma_complete_ms,
            "gemma_completion_tokens": output.completion_tokens,
            "tts_generation_yields": generation_yields,
            "tts_generation_yield_us": generation_yield_us,
            "tts_generation_fair_passes": generation_fair_passes,
            "tts_compute_lease_held_us": compute_lease_held_us,
            "tts_compute_lease_released_us": compute_lease_released_us,
            "tts_reference_admission_us": bindings.first().and_then(|binding| binding.tts_reference_admission_us),
            "tts_reference_cache": bindings.first().and_then(|binding| binding.tts_reference_cache.clone()),
            "tts_reference_us": bindings.first().and_then(|binding| binding.tts_reference_us),
            "tts_first_pcm_us": bindings.first().and_then(|binding| binding.tts_helper_first_pcm_us),
            "tts_text_chunk_us": bindings.first().and_then(|binding| binding.tts_helper_text_chunk_us),
            "tts_code_generation_us": bindings.first().and_then(|binding| binding.tts_helper_code_generation_us),
            "tts_prepared_prefix_cache_hit": bindings.first().and_then(|binding| binding.tts_helper_prepared_prefix_cache_hit),
            "qwen_startup_warm": qwen_startup_warm,
            "tts_first_decode_us": bindings.first().and_then(|binding| binding.tts_helper_first_decode_us),
            "tts_generation_segments": bindings.first().map(|binding| binding.tts_generation_segments.clone()),
            "tts_total_ms": tts_total_ms,
            "tts_output_audio_ms": tts_output_audio_ms,
            "tts_pipeline_complete_ms": tts_pipeline_complete_ms,
            "tts_scheduler_wait_ms": scheduler_wait_ms,
            "tts_segment_count": bindings.len(),
            "tts_segments": segments,
            "tts_minimum_lead_ms": minimum_lead_ms,
            "tts_maximum_lead_ms": maximum_lead_ms,
            "tts_packet_count": packets.len(),
            "tts_first_packet_audio_ms": packets.first().map(|packet| packet.2),
            "tts_maximum_internal_packet_gap_ms": maximum_internal_packet_gap_ms,
            "tts_maximum_segment_boundary_gap_ms": maximum_segment_boundary_gap_ms,
            "tts_required_startup_delay_ms": required_startup_delay_ms,
            "silence_to_safe_playback_ms": first_pcm_ms.map(|first| first + required_startup_delay_ms),
            "assistant_text": output.assistant_text,
        });
        println!("VOICE_RESIDENT_PIPELINE_BENCHMARK {receipt}");
        let audio = audio.context("Gemma omitted incremental-audio metrics")?;
        let projector_backlog_us = audio
            .finish_us
            .zip(audio.last_projector_finish_us)
            .map_or(u64::MAX, |(finish, projector_finish)| {
                projector_finish.saturating_sub(finish)
            });
        ensure!(
            projector_backlog_us <= 80_000,
            "streaming audio projector retained {:.1} ms after physical silence",
            projector_backlog_us as f64 / 1_000.0
        );
        ensure!(
            audio.high_water_samples <= 3_200,
            "streaming audio ring accumulated {:.1} ms instead of consuming near real time",
            audio.high_water_samples as f64 / 16.0
        );
        ensure!(
            first_delta_ms.is_some_and(|elapsed| elapsed <= 500.0),
            "Gemma first token missed the 500 ms conversational ceiling: {first_delta_ms:?}"
        );
        ensure!(
            first_pcm_ms.is_some_and(|elapsed| elapsed <= 750.0),
            "Qwen first PCM missed the 750 ms end-to-end ceiling: {first_pcm_ms:?}"
        );
        ensure!(
            generation_yields > 0,
            "Qwen acquired the compute lease but resident Gemma never yielded"
        );
        ensure!(first_pcm_ms.is_some(), "Qwen emitted no PCM");
        async_runtime
            .block_on(speech.shutdown())
            .map_err(anyhow::Error::msg)?;
        drop(native);
        Ok(())
    }

    #[test]
    fn generation_backpressure_is_exact_idempotent_and_bounded() {
        let speech = MomSpeech::empty_for_tests();
        for index in 0..MAX_GENERATION_BACKPRESSURE_REQUESTS {
            let request = format!("voice-request-{index}");
            assert!(speech.set_generation_backpressure(&request, true).is_ok());
            assert!(speech.set_generation_backpressure(&request, true).is_ok());
        }
        assert!(
            speech
                .set_generation_backpressure("one-request-too-many", true)
                .is_err()
        );
        let released = speech
            .set_generation_backpressure("voice-request-0", false)
            .expect("release exact request");
        assert!(!released.active);
        assert!(
            speech
                .set_generation_backpressure("replacement-request", true)
                .is_ok()
        );
        speech.begin_quiesce();
        assert!(
            speech
                .generation_backpressure
                .lock()
                .expect("backpressure registry")
                .is_empty()
        );
    }

    #[test]
    fn production_executor_reuses_one_immutable_descriptor_snapshot() {
        let host = Arc::new(SpeechHost::new(PlatformTarget::current()));
        let executor = HostSpeechExecutor::new(host).expect("empty host inventory");
        let first = executor.descriptors().expect("first inventory snapshot");
        let second = executor.descriptors().expect("reused inventory snapshot");

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn persona_voice_keys_match_contact_ids_and_framework_aliases() {
        for (persona_id, expected) in [
            ("persona-gabor_mate", "gabor_mate"),
            ("persona-robert_miller_fsap", "robert_miller"),
            ("persona-arnold_popky_detur", "arnold_popky"),
            ("persona-shirley_jean_schmidt_dnms", "shirley_jean_schmidt"),
        ] {
            assert_eq!(persona_voice_key(persona_id), Some(expected));
        }
    }

    #[test]
    fn persona_voice_key_uses_persona_chat_source_when_recipients_are_empty() {
        assert_eq!(
            persona_voice_key_for_conversation(&[], Some("persona-gabor_mate")),
            Some("gabor_mate")
        );
        assert_eq!(
            persona_voice_key_for_conversation(
                &["persona-bessel_van_der_kolk".to_string()],
                Some("persona-gabor_mate")
            ),
            Some("bessel_van_der_kolk")
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "runs the exact app-discovered local Qwen Base ICL persona route"]
    async fn real_local_app_persona_qwen_route_selects_synthesizes_and_validates() {
        let data_dir = mom_llama_runtime::config::resolve_data_dir();
        let qwen_config = speech_native_backend_qwen::QwenBackendConfig::discover(Some(
            &data_dir.join("speech-models"),
        ));
        let expected_reference_root = std::env::var_os("MOM_QWEN3_TTS_VOICE_REFERENCE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| data_dir.join("speech-voices/qwen-icl"));
        assert_eq!(
            qwen_config.icl.expect("app ICL config").reference_root,
            expected_reference_root
        );
        let speech = MomSpeech::discover(&data_dir, &data_dir.join("speech-voices/qwen-icl"))
            .await
            .expect("app speech discovery");
        let descriptors = speech.executor.descriptors().expect("speech descriptors");
        let voice_id = "mom.qwen.icl.gabor_mate";
        let selection = select_read_aloud(&descriptors, 62, Some(voice_id), true)
            .expect("app must admit Gabor's local ICL voice");
        assert_eq!(selection.model_id().as_deref(), Some(QWEN_ICL_MODEL_ID));
        assert_eq!(selection.voice_id(), voice_id);

        let request_id = SpeechRequestId("mom-app-qwen-icl-qualification".into());
        let target = AssistantMessageTarget {
            conversation_id: "persona-gabor_mate-chat".into(),
            message_id: "qualification-message".into(),
            text: "Good morning. What is happening for this person right now?".into(),
            text_sha256: sha256_hex(b"Good morning. What is happening for this person right now?"),
            persona_voice_id: Some(voice_id.into()),
        };
        let response = speech
            .executor
            .synthesize(read_aloud_request(&target, &selection, request_id.clone()))
            .await
            .expect("app-route Qwen synthesis");
        assert_eq!(response.route.model_id.as_deref(), Some(QWEN_ICL_MODEL_ID));
        let duration_ms = validate_read_aloud_response(response, &request_id, &selection)
            .expect("app route must validate its selected ICL model");
        assert!(duration_ms.is_some_and(|duration| duration > 500));
        speech.shutdown().await.expect("speech host joined");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "diagnoses an existing exact voice attachment with the installed Parakeet model"]
    async fn real_local_saved_voice_transcription_diagnostic() {
        let conversation = std::env::var("MOM_DIAG_CONVERSATION").expect("conversation id");
        let attachment = std::env::var("MOM_DIAG_ATTACHMENT").expect("attachment id");
        let catalog = mom_llama_runtime::attachment_preview(&attachment)
            .expect("attachment preview")
            .result
            .expect("owned attachment preview");
        let anchor = AttachmentPreviewAnchor {
            attachment_id: catalog.attachment_id,
            root_sha256: catalog.root_sha256,
            artifact_id: catalog.primary.expect("canonical audio").artifact_id,
            policy_fingerprint: catalog.policy_fingerprint,
        };
        let input = mom_llama_runtime::attachment_transcription_input(&conversation, &anchor)
            .expect("audio input")
            .expect("owned audio input");
        let pcm = input.bytes.get(44..).expect("PCM WAV payload");
        let samples = pcm
            .chunks_exact(2)
            .map(|pair| f64::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0)
            .collect::<Vec<_>>();
        let rms = (samples.iter().map(|sample| sample * sample).sum::<f64>()
            / samples.len().max(1) as f64)
            .sqrt();
        let peak = samples
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0, f64::max);
        eprintln!(
            "saved voice input: bytes={}, samples={}, rms={rms:.5}, peak={peak:.5}",
            input.bytes.len(),
            samples.len()
        );
        let data_dir = mom_llama_runtime::config::resolve_data_dir();
        // Keep this diagnostic isolated from Qwen's independent voice warmup.
        let host = Arc::new(SpeechHost::default());
        let parakeet = speech_native_backend_parakeet::ParakeetSpeechBackend::discover(
            speech_native_backend_parakeet::ParakeetBackendConfig {
                model_dir: None,
                managed_model_root: Some(data_dir.join("speech-models")),
            },
        )
        .await;
        host.register_backend(Arc::new(parakeet))
            .expect("register installed Parakeet");
        let speech = MomSpeech {
            executor: Arc::new(HostSpeechExecutor::new(host).expect("speech inventory")),
            _qwen: None,
            _qwen_startup_warm: None,
            state: Mutex::new(SpeechState::default()),
            operation_revision: watch::channel(0).0,
            precommit_voices: RwLock::new(BTreeMap::new()),
            generation_backpressure: Mutex::new(BTreeMap::new()),
        };
        let result = speech
            .transcribe_attachment(&conversation, &anchor, "diagnose-existing-voice")
            .await
            .expect("transcription command");
        eprintln!(
            "saved voice diagnostic: status={}, blocker={:?}, transcript_bytes={}",
            result.status,
            result.blocker.as_ref().map(|blocker| &blocker.code),
            result
                .result
                .as_ref()
                .map_or(0, |output| output.transcript.len())
        );
        if result
            .result
            .as_ref()
            .is_some_and(|output| output.transcript.trim().is_empty())
        {
            // Diagnostic only: test whether Parakeet rejected a quiet or
            // abruptly terminated clip. The original stored WAV is untouched.
            let gain = if peak > 0.0 {
                (0.65 / peak).min(32.0)
            } else {
                1.0
            };
            let mut conditioned = vec![0_i16; 3_200];
            conditioned.extend(samples.iter().map(|sample| {
                (sample * gain * 32767.0).clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
            }));
            conditioned.extend(std::iter::repeat_n(0_i16, 8_000));
            let pcm_bytes = conditioned
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect::<Vec<_>>();
            let data_size = u32::try_from(pcm_bytes.len()).expect("diagnostic size");
            let mut wav = Vec::with_capacity(44 + pcm_bytes.len());
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&(36 + data_size).to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16_u32.to_le_bytes());
            wav.extend_from_slice(&1_u16.to_le_bytes());
            wav.extend_from_slice(&1_u16.to_le_bytes());
            wav.extend_from_slice(&16_000_u32.to_le_bytes());
            wav.extend_from_slice(&32_000_u32.to_le_bytes());
            wav.extend_from_slice(&2_u16.to_le_bytes());
            wav.extend_from_slice(&16_u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&data_size.to_le_bytes());
            wav.extend_from_slice(&pcm_bytes);
            let request_id = SpeechRequestId("diagnose-conditioned-voice".to_owned());
            let conditioned_result = speech
                .executor
                .transcribe(TranscriptionRequest {
                    context: request_context(
                        request_id,
                        SpeechRouteSelector::ExactBackend {
                            backend_id: PARAKEET_BACKEND_ID.to_owned(),
                            model_id: Some(PARAKEET_MODEL_ID.to_owned()),
                            voice_id: None,
                        },
                        120_000,
                    ),
                    input: TranscriptionInput::Complete {
                        audio: AudioInput::Encoded {
                            format: EncodedAudioFormat::Wav,
                            data: wav,
                        },
                    },
                    language: Some("en".to_owned()),
                    task: TranscriptionTask::Transcribe,
                    timestamps: TimestampGranularity::None,
                    diarization: DiarizationPolicy::Disabled,
                    partial_results: false,
                    punctuation: true,
                    hotwords: Vec::new(),
                })
                .await;
            eprintln!(
                "conditioned voice diagnostic: gain={gain:.2}, result={:?}",
                conditioned_result
                    .as_ref()
                    .map(|response| response.text.len())
            );
        }
        speech.shutdown().await.expect("speech host joined");
    }
    use speech_native_types::{
        CapabilityAvailability, CapabilityEvidence, EvidenceKind, EvidenceOutcome,
        SpeechBackendKind, SpeechCapability, SpeechCapabilityLimits, SpeechModelDescriptor,
        SynthesisCapabilities, TranscriptionCapabilities, VoiceDescriptor, VoiceQuality,
    };

    fn runtime_evidence() -> Vec<CapabilityEvidence> {
        vec![CapabilityEvidence {
            source_id: "mom-speech-test".to_string(),
            source_version: Some("1".to_string()),
            kind: EvidenceKind::RuntimeApi,
            outcome: EvidenceOutcome::Confirmed,
            observed_at_unix_ms: 1,
            detail: "fixture runtime evidence".to_string(),
        }]
    }

    fn parakeet_descriptor() -> SpeechBackendDescriptor {
        SpeechBackendDescriptor {
            id: PARAKEET_BACKEND_ID.to_string(),
            display_name: "Parakeet".to_string(),
            kind: SpeechBackendKind::EmbeddedModel,
            readiness: SpeechBackendReadiness::Ready,
            capabilities: vec![SpeechCapability {
                id: "parakeet-rs.eou-120m.transcription".to_string(),
                backend_id: PARAKEET_BACKEND_ID.to_string(),
                model_id: Some(PARAKEET_MODEL_ID.to_string()),
                operation: SpeechOperationCapability::Transcription(TranscriptionCapabilities {
                    accepted_audio: vec![AcceptedAudio::Wav],
                    ..TranscriptionCapabilities::default()
                }),
                availability: CapabilityAvailability::Available,
                network: NetworkBehavior::Never,
                languages: vec!["en".to_string()],
                limits: SpeechCapabilityLimits {
                    max_concurrent_requests: Some(1),
                    ..SpeechCapabilityLimits::default()
                },
                evidence: runtime_evidence(),
            }],
            models: vec![SpeechModelDescriptor {
                id: PARAKEET_MODEL_ID.to_string(),
                display_name: "Parakeet".to_string(),
                family: "nvidia-parakeet-eou".to_string(),
                languages: vec!["en".to_string()],
                resident: true,
                estimated_memory_bytes: None,
                content_hash: Some(PARAKEET_MODEL_CONTENT_SHA256.to_string()),
            }],
            voices: Vec::new(),
        }
    }

    fn qwen_descriptor() -> SpeechBackendDescriptor {
        SpeechBackendDescriptor {
            id: QWEN_BACKEND_ID.to_string(),
            display_name: "Qwen3 TTS".to_string(),
            kind: SpeechBackendKind::EmbeddedModel,
            readiness: SpeechBackendReadiness::Ready,
            capabilities: vec![SpeechCapability {
                id: "qwen3-tts.mlx-swift.synthesis".to_string(),
                backend_id: QWEN_BACKEND_ID.to_string(),
                model_id: Some(QWEN_ICL_MODEL_ID.to_string()),
                operation: SpeechOperationCapability::Synthesis(SynthesisCapabilities {
                    streaming_audio: true,
                    voice_selection: true,
                    returned_audio: vec![AudioOutputKind::Pcm],
                    ..SynthesisCapabilities::default()
                }),
                availability: CapabilityAvailability::Available,
                network: NetworkBehavior::Never,
                languages: vec!["en-US".to_string()],
                limits: SpeechCapabilityLimits {
                    max_input_characters: Some(4_000),
                    max_concurrent_requests: Some(1),
                    ..SpeechCapabilityLimits::default()
                },
                evidence: runtime_evidence(),
            }],
            models: vec![SpeechModelDescriptor {
                id: QWEN_ICL_MODEL_ID.to_string(),
                display_name: "Qwen3 TTS Base ICL".to_string(),
                family: "qwen3-tts".to_string(),
                languages: vec!["en-US".to_string()],
                resident: false,
                estimated_memory_bytes: None,
                content_hash: None,
            }],
            voices: vec![VoiceDescriptor {
                id: "mom.qwen.icl.gabor_mate".to_string(),
                name: "Warm clinician".to_string(),
                language: "en-US".to_string(),
                gender: None,
                quality: Some(VoiceQuality::Premium),
                expected_latency: None,
                network: NetworkBehavior::Never,
                installed: true,
            }],
        }
    }

    fn deferred_parakeet_descriptor() -> SpeechBackendDescriptor {
        let mut descriptor = parakeet_descriptor();
        descriptor.models[0].resident = false;
        descriptor.capabilities[0].availability = CapabilityAvailability::DeferredLoad;
        descriptor.capabilities[0].evidence = vec![CapabilityEvidence {
            source_id: PARAKEET_DEFERRED_LOAD_EVIDENCE_SOURCE_ID.to_string(),
            source_version: Some("1".to_string()),
            kind: EvidenceKind::SystemInventory,
            outcome: EvidenceOutcome::Inconclusive,
            observed_at_unix_ms: 1,
            detail: "fixture manifest-bound deferred loader".to_string(),
        }];
        descriptor
    }

    #[test]
    fn routes_are_exact_never_network_and_complete_input_only() {
        let voice_id = "mom.qwen.icl.gabor_mate";
        let qwen = select_read_aloud(&[qwen_descriptor()], 12, Some(voice_id), true)
            .expect("Qwen streaming ICL route");
        assert!(matches!(qwen, ReadAloudSelection::Qwen(_)));
        let request = read_aloud_request(
            &AssistantMessageTarget {
                conversation_id: "chat".to_string(),
                message_id: "message".to_string(),
                text: "hello".to_string(),
                text_sha256: sha256_hex(b"hello"),
                persona_voice_id: Some(voice_id.to_string()),
            },
            &qwen,
            SpeechRequestId("request".to_string()),
        );
        assert!(request.stream);
        assert_eq!(request.output, qwen_stream_output());
        assert!(qwen.descriptor().capabilities.iter().all(|capability| {
            capability.network == NetworkBehavior::Never
                && matches!(
                    &capability.operation,
                    SpeechOperationCapability::Synthesis(synthesis)
                        if synthesis.streaming_audio
                            && synthesis.returned_audio.contains(&AudioOutputKind::Pcm)
                )
        }));
        assert!(select_read_aloud(&[], 12, Some(voice_id), true).is_err());
        let parakeet = select_parakeet(&[parakeet_descriptor()]).expect("Parakeet route");
        assert_eq!(parakeet.descriptor.id, PARAKEET_BACKEND_ID);
        let deferred = select_parakeet(&[deferred_parakeet_descriptor()])
            .expect("manifest-bound deferred Parakeet route");
        assert!(!deferred.descriptor.models[0].resident);
        assert!(
            deferred.descriptor.capabilities[0].deferred_load_admissible()
                && !deferred.descriptor.capabilities[0].evidence[0].proves_runtime()
        );
        assert!(
            select_resident_parakeet(&[deferred_parakeet_descriptor()]).is_err(),
            "unverified deferred state must never be bound into response provenance"
        );
        select_resident_parakeet(&[parakeet_descriptor()])
            .expect("resident verified Parakeet provenance route");
        let context = request_context(
            SpeechRequestId("request".to_string()),
            SpeechRouteSelector::ExactBackend {
                backend_id: PARAKEET_BACKEND_ID.to_string(),
                model_id: Some(PARAKEET_MODEL_ID.to_string()),
                voice_id: None,
            },
            1_000,
        );
        assert_eq!(context.routing.privacy, SpeechPrivacyPolicy::LocalOnly);
        assert!(!context.routing.allow_asset_download);
        assert!(!context.routing.allow_fallback_before_output);
    }

    #[test]
    fn local_qwen_icl_persona_route_accepts_exact_deferred_inventory() {
        let voice_id = "mom.qwen.icl.gabor_mate";
        let mut descriptor = qwen_descriptor();
        let capability = &mut descriptor.capabilities[0];
        capability.model_id = Some(QWEN_ICL_MODEL_ID.to_string());
        capability.availability = CapabilityAvailability::DeferredLoad;
        let SpeechOperationCapability::Synthesis(synthesis) = &mut capability.operation else {
            panic!("Qwen ICL inventory must advertise synthesis");
        };
        synthesis.streaming_audio = true;
        synthesis.returned_audio = vec![AudioOutputKind::Wav, AudioOutputKind::Pcm];
        capability.evidence = vec![CapabilityEvidence {
            source_id: "mom-qwen3-base-icl-local-reference".to_string(),
            source_version: Some("1".to_string()),
            kind: EvidenceKind::SystemInventory,
            outcome: EvidenceOutcome::Inconclusive,
            observed_at_unix_ms: 1,
            detail: "validated local model and voice inventory".to_string(),
        }];
        descriptor.voices[0].id = voice_id.to_string();

        let selected = select_read_aloud(&[descriptor], 62, Some(voice_id), true)
            .expect("exact local ICL route is deferred-load admissible");
        assert!(matches!(selected, ReadAloudSelection::Qwen(_)));
        assert_eq!(selected.model_id().as_deref(), Some(QWEN_ICL_MODEL_ID));
        assert_eq!(selected.voice_id(), voice_id);
    }

    #[test]
    fn prepared_turn_voice_binding_is_resident_and_immutable() {
        let speech = MomSpeech::empty_for_tests();
        let conversation = "conversation-resident-voice";
        let gabor = ConversationPersonaIdentity {
            recipient_ids: vec!["persona-gabor_mate".to_string()],
            source_conversation_id: None,
        };
        assert!(
            speech
                .bind_precommit_voice(conversation, &gabor)
                .expect("bind prepared voice")
        );
        assert_eq!(
            speech
                .precommit_voice(conversation)
                .expect("resident lookup"),
            Some("mom.qwen.icl.gabor_mate".to_string())
        );

        let janina = ConversationPersonaIdentity {
            recipient_ids: vec!["persona-janina_fisher".to_string()],
            source_conversation_id: None,
        };
        assert_eq!(
            speech
                .bind_precommit_voice(conversation, &janina)
                .expect_err("a conversation cannot change persona voice"),
            "speech_precommit_persona_binding_changed"
        );
        assert_eq!(
            speech
                .precommit_voice(conversation)
                .expect("original binding"),
            Some("mom.qwen.icl.gabor_mate".to_string())
        );
    }

    #[test]
    fn qwen_icl_response_validates_against_the_selected_model() {
        let request_id = SpeechRequestId("qwen-icl-validation".to_string());
        let voice_id = "mom.qwen.icl.gabor_mate";
        let selection = ReadAloudSelection::Qwen(QwenSelection {
            descriptor_sha256: "descriptor".to_string(),
            descriptor: qwen_descriptor(),
            model_id: QWEN_ICL_MODEL_ID.to_string(),
            voice_id: voice_id.to_string(),
        });
        let response = SynthesisResponse {
            request_id: request_id.clone(),
            route: speech_native_types::SpeechResolvedRoute {
                backend_id: QWEN_BACKEND_ID.to_string(),
                model_id: Some(QWEN_ICL_MODEL_ID.to_string()),
                voice_id: Some(voice_id.to_string()),
                backend_kind: SpeechBackendKind::EmbeddedModel,
                network: NetworkBehavior::Never,
            },
            output: SynthesisOutput::Streamed {
                format: qwen_stream_output(),
                emitted_bytes: 3,
            },
            duration_ms: Some(800),
            alignments: Vec::new(),
            usage: speech_native_types::SpeechUsage {
                real_local_inference: true,
                ..Default::default()
            },
        };

        assert_eq!(
            validate_read_aloud_response(response, &request_id, &selection),
            Ok(Some(800))
        );
    }

    #[tokio::test]
    async fn cancel_before_registration_and_close_suppress_publication() {
        let speech = MomSpeech::empty_for_tests();
        let stopped = speech
            .stop(Some("operation-123"), None)
            .expect("stop result");
        assert!(
            stopped
                .result
                .as_ref()
                .is_some_and(|result| result.cancellation_requested)
        );
        let cancelled = speech.begin_operation(
            "operation-123",
            SpeechRequestId("request".to_string()),
            OperationKind::ReadAloud,
        );
        assert_eq!(
            cancelled.expect_err("early stop wins").code,
            "speech_cancelled"
        );

        let active_request = SpeechRequestId("active-request".to_string());
        speech
            .begin_operation(
                "active-operation",
                active_request.clone(),
                OperationKind::Transcription,
            )
            .expect("active operation");
        speech
            .stop(Some("active-operation"), None)
            .expect("active stop");
        assert_eq!(
            speech
                .complete_operation("active-operation", &active_request)
                .expect_err("stopped backend success must not publish")
                .code,
            "speech_cancelled"
        );

        let closing_request = SpeechRequestId("closing-request".to_string());
        speech
            .begin_operation(
                "closing-operation",
                closing_request.clone(),
                OperationKind::ReadAloud,
            )
            .expect("closing operation");
        speech.begin_quiesce();
        assert!(
            speech
                .publish_playback(
                    "closing-operation",
                    &closing_request,
                    ReadAloudBinding {
                        conversation_id: "chat".to_string(),
                        message_id: "message".to_string(),
                        text_sha256: "text".to_string(),
                        backend_id: QWEN_BACKEND_ID.to_string(),
                        backend_descriptor_sha256: "descriptor".to_string(),
                        model_id: Some(QWEN_ICL_MODEL_ID.to_string()),
                        voice_id: "mom.qwen.icl.gabor_mate".to_string(),
                        request_id: closing_request.0.clone(),
                        duration_ms: None,
                        tts_time_to_first_audio_ms: None,
                        tts_total_ms: None,
                        network: NetworkBehavior::Never,
                    },
                )
                .is_err()
        );
        let shutdown = speech.shutdown().await.expect("joined empty host");
        assert!(shutdown.host_joined);
        assert_eq!(shutdown.active_operation_count, 0);
        assert_eq!(shutdown.retained_playback_count, 0);
    }

    #[tokio::test]
    async fn stop_and_wait_requires_the_exact_active_operation_to_finish() {
        let speech = Arc::new(MomSpeech::empty_for_tests());
        let request_id = SpeechRequestId("waited-transcription".to_string());
        speech
            .begin_operation(
                "waited-operation",
                request_id.clone(),
                OperationKind::Transcription,
            )
            .expect("active transcription");

        let waiter_speech = Arc::clone(&speech);
        let waiter = tokio::spawn(async move {
            waiter_speech
                .stop_and_wait("waited-operation", false)
                .await
                .expect("terminal acknowledgment")
        });
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "cancel request alone is not terminal"
        );

        speech.finish_operation("waited-operation", &request_id);
        let output = waiter.await.expect("waiter task");
        assert!(output.stop.cancellation_requested);
        assert!(output.terminal_acknowledged);
    }

    #[test]
    fn cold_initialization_wait_is_bounded_and_only_applies_to_attachment_transcription() {
        assert_eq!(
            speech_stop_wait_timeout(
                &SpeechRequestId("mom-transcribe-history-op".to_string()),
                true,
            ),
            std::time::Duration::from_secs(30),
        );
        for request_id in [
            "mom-live-transcribe-op",
            "mom-read-aloud-op",
            "mom-precommit-op",
        ] {
            assert_eq!(
                speech_stop_wait_timeout(&SpeechRequestId(request_id.to_string()), true),
                std::time::Duration::from_secs(2),
            );
        }
        assert_eq!(
            speech_stop_wait_timeout(
                &SpeechRequestId("mom-transcribe-history-op".to_string()),
                false,
            ),
            std::time::Duration::from_secs(2),
        );
    }

    #[test]
    fn descriptor_hash_binds_request_to_exact_inventory() {
        let original = qwen_descriptor();
        let mut changed = original.clone();
        changed.voices[0].id = "voice.changed".to_string();
        assert_ne!(
            descriptor_sha256(&original).expect("original hash"),
            descriptor_sha256(&changed).expect("changed hash")
        );
    }

    #[test]
    fn active_registry_rejects_duplicate_target_and_finishes_exact_nonce() {
        let speech = MomSpeech::empty_for_tests();
        let first = SpeechRequestId("request-one".to_string());
        speech
            .begin_operation("operation-456", first.clone(), OperationKind::Transcription)
            .expect("first operation");
        let duplicate = speech.begin_operation(
            "operation-456",
            SpeechRequestId("request-two".to_string()),
            OperationKind::ReadAloud,
        );
        assert_eq!(
            duplicate.expect_err("duplicate operation").code,
            "speech_operation_duplicate"
        );
        speech.finish_operation("operation-456", &SpeechRequestId("stale".to_string()));
        assert_eq!(
            speech.state.lock().expect("state").active.len(),
            1,
            "a stale completion must not release the current target"
        );
        speech.finish_operation("operation-456", &first);
        assert!(speech.state.lock().expect("state").active.is_empty());
    }

    mod postflight_cleanup {
        use super::*;
        use mom_llama_runtime::conversation_store::{Message, load_db, save_db};
        use speech_native_types::{SpeechResolvedRoute, SpeechUsage};
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        const MESSAGE_TEXT: &str = "An exact owned assistant message.";
        const MOCK_WAV: &[u8] = b"mock-complete-wav";
        // The existing Runtime attachment fixture: one silent PCM sample.
        const ATTACHMENT_WAV: &[u8] = b"RIFF\x26\x00\x00\x00WAVE\
            fmt \x10\x00\x00\x00\x01\x00\x01\x00\x40\x1f\x00\x00\x80\x3e\x00\x00\x02\x00\x10\x00\
            data\x02\x00\x00\x00\x00\x00";

        struct OwnedSpeechStore {
            _directory: tempfile::TempDir,
            store: PathBuf,
            invalid_store: PathBuf,
            conversation_id: String,
            message_id: String,
        }

        impl Drop for OwnedSpeechStore {
            fn drop(&mut self) {
                mom_llama_runtime::config::set_data_dir_override_for_tests(None);
            }
        }

        impl OwnedSpeechStore {
            fn new() -> anyhow::Result<Self> {
                let directory = tempfile::tempdir()?;
                let store = directory.path().join("store");
                let invalid_store = directory.path().join("regular-file-not-a-directory");
                std::fs::write(&invalid_store, b"owned postflight fault")?;
                let mut fixture = Self {
                    _directory: directory,
                    store,
                    invalid_store,
                    conversation_id: String::new(),
                    message_id: "speech-owned-message".to_string(),
                };
                fixture.restore();
                fixture.conversation_id =
                    mom_llama_runtime::conversation_new(Some("Owned speech source".to_string()))?
                        .result
                        .expect("owned conversation")
                        .id;
                let mut db = load_db()?;
                let conversation = db
                    .conversations
                    .iter_mut()
                    .find(|item| item.id == fixture.conversation_id)
                    .expect("owned conversation persisted");
                conversation.recipient_ids = vec!["persona-gabor_mate".to_string()];
                conversation.messages.push(Message {
                    id: fixture.message_id.clone(),
                    conversation_id: fixture.conversation_id.clone(),
                    role: MessageRole::Assistant,
                    content: MESSAGE_TEXT.to_string(),
                    created_at: "1".to_string(),
                    parent_id: None,
                    model: None,
                    receipt_id: None,
                    prompt_tokens: None,
                    completion_tokens: None,
                    reasoning_content: None,
                    reasoning_incomplete: false,
                    branch_index: None,
                    branch_count: None,
                    attribution: None,
                    attachment_ids: Vec::new(),
                });
                conversation.active_leaf_message_id = Some(fixture.message_id.clone());
                save_db(&db)?;
                Ok(fixture)
            }

            fn restore(&self) {
                mom_llama_runtime::config::set_data_dir_override_for_tests(Some(
                    self.store.clone(),
                ));
            }

            fn audio_anchor(&self) -> anyhow::Result<AttachmentPreviewAnchor> {
                let path = self.store.join("owned.wav");
                std::fs::write(&path, ATTACHMENT_WAV)?;
                let imported = mom_llama_runtime::attachment_import(
                    &mom_llama_runtime::OperationScope::detached(),
                    &self.conversation_id,
                    &path,
                )?
                .result
                .expect("real owned attachment import");
                let catalog = mom_llama_runtime::attachment_preview(&imported.attachment.id)?
                    .result
                    .expect("current attachment catalog");
                let artifact_id = catalog
                    .primary
                    .as_ref()
                    .expect("canonical audio preview")
                    .artifact_id
                    .clone();
                Ok(AttachmentPreviewAnchor {
                    attachment_id: catalog.attachment_id,
                    root_sha256: catalog.root_sha256,
                    artifact_id,
                    policy_fingerprint: catalog.policy_fingerprint,
                })
            }
        }

        #[derive(Default)]
        struct MockPostflightExecutor {
            after_response: Mutex<Option<Box<dyn FnOnce() + Send>>>,
            inventory_error: Arc<AtomicBool>,
            deferred_snapshot: AtomicBool,
            foreign_request: AtomicBool,
            calls: AtomicUsize,
        }

        impl MockPostflightExecutor {
            fn respond(&self, request_id: SpeechRequestId) -> SpeechRequestId {
                self.calls.fetch_add(1, Ordering::Relaxed);
                let effect = self
                    .after_response
                    .lock()
                    .expect("owned response effect")
                    .take();
                if let Some(effect) = effect {
                    effect();
                }
                if self.foreign_request.load(Ordering::Relaxed) {
                    SpeechRequestId("foreign-response-request".to_string())
                } else {
                    request_id
                }
            }
        }

        #[async_trait]
        impl SpeechExecutor for MockPostflightExecutor {
            fn descriptors(&self) -> Result<Arc<[SpeechBackendDescriptor]>, SpeechHostError> {
                if self.inventory_error.load(Ordering::Relaxed) {
                    Err(SpeechHostError::StateUnavailable)
                } else if self.deferred_snapshot.load(Ordering::Relaxed) {
                    Ok(Arc::from([
                        qwen_descriptor(),
                        deferred_parakeet_descriptor(),
                    ]))
                } else {
                    Ok(Arc::from([qwen_descriptor(), parakeet_descriptor()]))
                }
            }

            fn current_descriptors(
                &self,
            ) -> Result<Arc<[SpeechBackendDescriptor]>, SpeechHostError> {
                if self.inventory_error.load(Ordering::Relaxed) {
                    Err(SpeechHostError::StateUnavailable)
                } else if self.deferred_snapshot.load(Ordering::Relaxed)
                    && self.calls.load(Ordering::Relaxed) == 0
                {
                    Ok(Arc::from([
                        qwen_descriptor(),
                        deferred_parakeet_descriptor(),
                    ]))
                } else {
                    Ok(Arc::from([qwen_descriptor(), parakeet_descriptor()]))
                }
            }

            async fn synthesize(
                &self,
                request: SynthesisRequest,
            ) -> Result<SynthesisResponse, String> {
                assert_eq!(
                    request.context.routing.privacy,
                    SpeechPrivacyPolicy::LocalOnly
                );
                assert_eq!(
                    request.input,
                    SynthesisInput::Text {
                        text: MESSAGE_TEXT.to_string()
                    }
                );
                Ok(SynthesisResponse {
                    request_id: self.respond(request.context.request_id),
                    route: SpeechResolvedRoute {
                        backend_id: QWEN_BACKEND_ID.to_string(),
                        model_id: Some(QWEN_ICL_MODEL_ID.to_string()),
                        voice_id: Some("mom.qwen.icl.gabor_mate".to_string()),
                        backend_kind: SpeechBackendKind::EmbeddedModel,
                        network: NetworkBehavior::Never,
                    },
                    // Envelope bytes only; no synthesis, decoding or playback.
                    output: SynthesisOutput::Streamed {
                        format: qwen_stream_output(),
                        emitted_bytes: u64::try_from(MOCK_WAV.len()).expect("mock WAV length"),
                    },
                    duration_ms: Some(1),
                    alignments: Vec::new(),
                    usage: SpeechUsage {
                        real_local_inference: true,
                        ..SpeechUsage::default()
                    },
                })
            }

            async fn transcribe(
                &self,
                request: TranscriptionRequest,
            ) -> Result<TranscriptionResponse, String> {
                assert_eq!(
                    request.context.routing.privacy,
                    SpeechPrivacyPolicy::LocalOnly
                );
                assert_eq!(
                    request.input,
                    TranscriptionInput::Complete {
                        audio: AudioInput::Encoded {
                            format: EncodedAudioFormat::Wav,
                            data: ATTACHMENT_WAV.to_vec()
                        }
                    }
                );
                Ok(TranscriptionResponse {
                    request_id: self.respond(request.context.request_id),
                    route: SpeechResolvedRoute {
                        backend_id: PARAKEET_BACKEND_ID.to_string(),
                        model_id: Some(PARAKEET_MODEL_ID.to_string()),
                        voice_id: None,
                        backend_kind: SpeechBackendKind::EmbeddedModel,
                        network: NetworkBehavior::Never,
                    },
                    text: "Owned mock transcript.".to_string(),
                    language: Some("en".to_string()),
                    segments: Vec::new(),
                    // Required response-envelope flag; this fixture proves no inference.
                    usage: SpeechUsage {
                        real_local_inference: true,
                        ..SpeechUsage::default()
                    },
                })
            }

            fn cancel(&self, _: &SpeechRequestId) -> usize {
                0
            }

            fn quiesce(&self) -> Result<(), SpeechHostError> {
                Ok(())
            }

            async fn shutdown(&self) -> Result<(), SpeechHostError> {
                Ok(())
            }
        }

        #[tokio::test(flavor = "current_thread")]
        async fn transcription_accepts_verified_deferred_to_resident_transition()
        -> anyhow::Result<()> {
            let fixture = OwnedSpeechStore::new()?;
            let anchor = fixture.audio_anchor()?;
            let executor = Arc::new(MockPostflightExecutor::default());
            executor.deferred_snapshot.store(true, Ordering::Relaxed);
            let speech = MomSpeech {
                executor: executor.clone(),
                _qwen: None,
                _qwen_startup_warm: None,
                state: Mutex::new(SpeechState::default()),
                operation_revision: watch::channel(0).0,
                precommit_voices: RwLock::new(BTreeMap::new()),
                generation_backpressure: Mutex::new(BTreeMap::new()),
            };

            let result = speech
                .transcribe_attachment(&fixture.conversation_id, &anchor, "stt-deferred-load")
                .await?;
            assert_eq!(executor.calls.load(Ordering::Relaxed), 1);
            assert_eq!(
                result.result.expect("resident transcript").transcript,
                "Owned mock transcript."
            );
            assert!(speech.state.lock().expect("speech state").active.is_empty());
            Ok(())
        }

        #[tokio::test(flavor = "current_thread")]
        #[allow(clippy::too_many_lines)]
        async fn read_aloud_releases_exact_slot_after_source_store_error() -> anyhow::Result<()> {
            let fixture = OwnedSpeechStore::new()?;
            let executor = Arc::new(MockPostflightExecutor::default());
            let speech = Arc::new(MomSpeech {
                executor: executor.clone(),
                _qwen: None,
                _qwen_startup_warm: None,
                state: Mutex::new(SpeechState::default()),
                operation_revision: watch::channel(0).0,
                precommit_voices: RwLock::new(BTreeMap::new()),
                generation_backpressure: Mutex::new(BTreeMap::new()),
            });
            // Prove the public path reaches ordinary publication before any fault.
            let output = speech
                .read_aloud(
                    &fixture.conversation_id,
                    &fixture.message_id,
                    "tts-positive-before-fault",
                )
                .await?
                .result
                .expect("ordinary playback");
            assert_eq!(
                output.binding.text_sha256,
                sha256_hex(MESSAGE_TEXT.as_bytes())
            );
            assert!(
                speech
                    .state
                    .lock()
                    .expect("speech state")
                    .playback
                    .contains_key(&output.playback_id)
            );
            assert!(speech.state.lock().expect("speech state").active.is_empty());
            assert!(
                speech
                    .stop(None, Some(&output.playback_id))?
                    .result
                    .expect("stop playback")
                    .playback_released
            );

            executor.foreign_request.store(true, Ordering::Relaxed);
            let foreign = speech
                .read_aloud(
                    &fixture.conversation_id,
                    &fixture.message_id,
                    "tts-foreign-response",
                )
                .await?;
            assert_eq!(
                foreign.blocker.expect("foreign response refusal").code,
                "speech_provenance_mismatch"
            );
            executor.foreign_request.store(false, Ordering::Relaxed);
            assert!(speech.state.lock().expect("speech state").active.is_empty());
            assert!(
                speech
                    .state
                    .lock()
                    .expect("speech state")
                    .playback
                    .is_empty()
            );

            let message_id = fixture.message_id.clone();
            *executor.after_response.lock().expect("response effect") = Some(Box::new(move || {
                let mut db = load_db().expect("owned source store");
                db.conversations
                    .iter_mut()
                    .flat_map(|conversation| conversation.messages.iter_mut())
                    .find(|message| message.id == message_id)
                    .expect("owned source message")
                    .content = "Changed source.".into();
                save_db(&db).expect("save changed source");
            }));
            let stale = speech
                .read_aloud(
                    &fixture.conversation_id,
                    &fixture.message_id,
                    "tts-source-changed",
                )
                .await?;
            assert_eq!(
                stale.blocker.expect("changed source refusal").code,
                "speech_read_aloud_stale"
            );
            assert!(speech.state.lock().expect("speech state").active.is_empty());
            assert!(
                speech
                    .state
                    .lock()
                    .expect("speech state")
                    .playback
                    .is_empty()
            );
            let mut db = load_db()?;
            db.conversations
                .iter_mut()
                .flat_map(|conversation| conversation.messages.iter_mut())
                .find(|message| message.id == fixture.message_id)
                .expect("owned source message")
                .content = MESSAGE_TEXT.into();
            save_db(&db)?;

            let weak = Arc::downgrade(&speech);
            *executor.after_response.lock().expect("response effect") = Some(Box::new(move || {
                weak.upgrade()
                    .expect("owned speech")
                    .stop(Some("tts-stop-after-response"), None)
                    .expect("stop admitted speech");
            }));
            let stopped = speech
                .read_aloud(
                    &fixture.conversation_id,
                    &fixture.message_id,
                    "tts-stop-after-response",
                )
                .await?;
            assert_eq!(
                stopped.blocker.expect("stopped response refusal").code,
                "speech_cancelled"
            );
            assert!(speech.state.lock().expect("speech state").active.is_empty());
            assert!(
                speech
                    .state
                    .lock()
                    .expect("speech state")
                    .playback
                    .is_empty()
            );

            for index in 0..MAX_ACTIVE_SPEECH_OPERATIONS + 2 {
                let invalid_store = fixture.invalid_store.clone();
                *executor.after_response.lock().expect("response effect") =
                    Some(Box::new(move || {
                        mom_llama_runtime::config::set_data_dir_override_for_tests(Some(
                            invalid_store,
                        ));
                    }));
                let calls = executor.calls.load(Ordering::Relaxed);
                let result = speech
                    .read_aloud(
                        &fixture.conversation_id,
                        &fixture.message_id,
                        &format!("tts-postflight-error-{index}"),
                    )
                    .await;
                fixture.restore();
                assert!(
                    result.is_err(),
                    "TTS must retain the postflight outer error at iteration {index}; public result={result:?}"
                );
                let error = result.expect_err("post-response Store error must remain outer error");
                assert!(
                    error.downcast_ref::<std::io::Error>().is_some(),
                    "retain the original I/O error: {error:#}"
                );
                assert_eq!(executor.calls.load(Ordering::Relaxed), calls + 1);
                let state = speech.state.lock().expect("speech state");
                assert!(
                    state.playback.is_empty(),
                    "failed source revalidation published audio"
                );
            }
            let recovered = speech
                .read_aloud(
                    &fixture.conversation_id,
                    &fixture.message_id,
                    "tts-positive-after-errors",
                )
                .await?
                .result
                .expect("capacity recovered");
            assert!(
                speech
                    .state
                    .lock()
                    .expect("speech state")
                    .playback
                    .contains_key(&recovered.playback_id)
            );
            assert!(speech.state.lock().expect("speech state").active.is_empty());
            assert_eq!(
                std::fs::read(&fixture.invalid_store)?,
                b"owned postflight fault"
            );
            Ok(())
        }

        #[allow(clippy::too_many_lines)]
        async fn transcription_postflight_error(inventory_error: bool) -> anyhow::Result<()> {
            let fixture = OwnedSpeechStore::new()?;
            let anchor = fixture.audio_anchor()?;
            let input = mom_llama_runtime::attachment_transcription_input(
                &fixture.conversation_id,
                &anchor,
            )?
            .expect("current actual attachment authority");
            assert_eq!(input.bytes, ATTACHMENT_WAV);
            assert_eq!(input.bytes_sha256, input.blob_object_id);
            let executor = Arc::new(MockPostflightExecutor::default());
            let speech = Arc::new(MomSpeech {
                executor: executor.clone(),
                _qwen: None,
                _qwen_startup_warm: None,
                state: Mutex::new(SpeechState::default()),
                operation_revision: watch::channel(0).0,
                precommit_voices: RwLock::new(BTreeMap::new()),
                generation_backpressure: Mutex::new(BTreeMap::new()),
            });
            let published = speech
                .transcribe_attachment(
                    &fixture.conversation_id,
                    &anchor,
                    "stt-positive-before-fault",
                )
                .await?
                .result
                .expect("ordinary transcript");
            assert_eq!(published.transcript, "Owned mock transcript.");
            assert_eq!(published.provenance.audio_sha256, input.bytes_sha256);
            assert_eq!(published.provenance.root_sha256, anchor.root_sha256);
            assert_eq!(published.provenance.artifact_id, anchor.artifact_id);
            assert_eq!(
                published.provenance.policy_fingerprint,
                anchor.policy_fingerprint
            );
            assert!(published.insertion_requires_explicit_normal_edit);
            assert!(speech.state.lock().expect("speech state").active.is_empty());

            let calls = executor.calls.load(Ordering::Relaxed);
            let foreign_source = speech
                .transcribe_attachment("foreign-conversation", &anchor, "stt-foreign-conversation")
                .await?;
            assert_eq!(
                foreign_source.blocker.expect("foreign source refusal").code,
                "attachment_transcription_conversation_mismatch"
            );
            let mut stale_anchor = anchor.clone();
            stale_anchor.policy_fingerprint = "changed-policy".to_string();
            let stale = speech
                .transcribe_attachment(&fixture.conversation_id, &stale_anchor, "stt-stale-source")
                .await?;
            assert_eq!(
                stale.blocker.expect("stale source refusal").code,
                "attachment_preview_stale"
            );
            assert_eq!(
                executor.calls.load(Ordering::Relaxed),
                calls,
                "source refusals precede speech dispatch"
            );
            assert!(speech.state.lock().expect("speech state").active.is_empty());

            executor.foreign_request.store(true, Ordering::Relaxed);
            let foreign = speech
                .transcribe_attachment(&fixture.conversation_id, &anchor, "stt-foreign-response")
                .await?;
            assert_eq!(
                foreign.blocker.expect("foreign response refusal").code,
                "speech_provenance_mismatch"
            );
            executor.foreign_request.store(false, Ordering::Relaxed);
            assert!(speech.state.lock().expect("speech state").active.is_empty());

            let weak = Arc::downgrade(&speech);
            *executor.after_response.lock().expect("response effect") = Some(Box::new(move || {
                weak.upgrade()
                    .expect("owned speech")
                    .stop(Some("stt-stop-after-response"), None)
                    .expect("stop admitted speech");
            }));
            let stopped = speech
                .transcribe_attachment(&fixture.conversation_id, &anchor, "stt-stop-after-response")
                .await?;
            assert_eq!(
                stopped.blocker.expect("stopped response refusal").code,
                "speech_cancelled"
            );
            assert!(speech.state.lock().expect("speech state").active.is_empty());

            for index in 0..MAX_ACTIVE_SPEECH_OPERATIONS + 2 {
                if inventory_error {
                    let fail_inventory = executor.inventory_error.clone();
                    *executor.after_response.lock().expect("response effect") =
                        Some(Box::new(move || {
                            fail_inventory.store(true, Ordering::Relaxed);
                        }));
                } else {
                    let invalid_store = fixture.invalid_store.clone();
                    *executor.after_response.lock().expect("response effect") =
                        Some(Box::new(move || {
                            mom_llama_runtime::config::set_data_dir_override_for_tests(Some(
                                invalid_store,
                            ));
                        }));
                }
                let calls = executor.calls.load(Ordering::Relaxed);
                let result = speech
                    .transcribe_attachment(
                        &fixture.conversation_id,
                        &anchor,
                        &format!("stt-postflight-error-{index}"),
                    )
                    .await;
                fixture.restore();
                executor.inventory_error.store(false, Ordering::Relaxed);
                assert!(
                    result.is_err(),
                    "STT must retain the postflight outer error: inventory={inventory_error}, iteration={index}; public result={result:?}"
                );
                let error = result.expect_err("post-response outer error must remain an error");
                if inventory_error {
                    assert_eq!(
                        error.downcast_ref::<SpeechHostError>(),
                        Some(&SpeechHostError::StateUnavailable)
                    );
                } else {
                    assert!(
                        error.downcast_ref::<std::io::Error>().is_some(),
                        "retain the original I/O error: {error:#}"
                    );
                }
                assert_eq!(executor.calls.load(Ordering::Relaxed), calls + 1);
                let state = speech.state.lock().expect("speech state");
                assert!(
                    state.playback.is_empty(),
                    "transcription must not publish playback"
                );
            }
            let recovered = speech
                .transcribe_attachment(
                    &fixture.conversation_id,
                    &anchor,
                    "stt-positive-after-errors",
                )
                .await?
                .result
                .expect("capacity recovered");
            assert_eq!(recovered.transcript, "Owned mock transcript.");
            assert_eq!(recovered.provenance.audio_sha256, input.bytes_sha256);
            assert!(speech.state.lock().expect("speech state").active.is_empty());
            assert_eq!(
                std::fs::read(fixture.store.join("owned.wav"))?,
                ATTACHMENT_WAV
            );
            assert_eq!(
                std::fs::read(&fixture.invalid_store)?,
                b"owned postflight fault"
            );
            Ok(())
        }

        #[tokio::test(flavor = "current_thread")]
        async fn transcribe_releases_exact_slot_after_attachment_store_error() -> anyhow::Result<()>
        {
            transcription_postflight_error(false).await
        }

        #[tokio::test(flavor = "current_thread")]
        async fn transcribe_releases_exact_slot_after_resident_inventory_error()
        -> anyhow::Result<()> {
            transcription_postflight_error(true).await
        }
    }
}

#[cfg(test)]
mod live_forwarding_tests {
    // Protocol tests only: these mock sinks perform no recognition or inference.
    use super::*;
    use speech_native_types::{
        AudioChunk, SpeechBackendKind, SpeechCancellation, SpeechResolvedRoute, SpeechUsage,
        TranscriptionAudioSink, TranscriptionEvent, TranscriptionTicket,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Notify, mpsc, oneshot};

    #[derive(Default)]
    struct Cancellation(AtomicUsize);
    impl SpeechCancellation for Cancellation {
        fn cancel(&self, _: &SpeechRequestId) -> usize {
            self.0.fetch_add(1, Ordering::AcqRel);
            1
        }
    }

    #[derive(Default)]
    struct Sink {
        chunks: Mutex<Vec<AudioChunk>>,
        entered: Notify,
        release: Notify,
        finished: Notify,
        finish_calls: AtomicUsize,
    }
    #[async_trait]
    impl TranscriptionAudioSink for Sink {
        async fn push(&self, chunk: AudioChunk) -> Result<(), SpeechError> {
            if chunk.sequence == 0 {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.chunks.lock().expect("test setup").push(chunk);
            Ok(())
        }
        async fn finish(&self) -> Result<(), SpeechError> {
            self.finish_calls.fetch_add(1, Ordering::AcqRel);
            self.finished.notify_one();
            Ok(())
        }
    }

    fn identity() -> TurnIdentity {
        TurnIdentity {
            turn: "turn-a".into(),
            capture: "capture-a".into(),
            conversation: "chat-a".into(),
        }
    }
    fn response() -> TranscriptionResponse {
        TranscriptionResponse {
            request_id: SpeechRequestId("mock-live-request".into()),
            route: SpeechResolvedRoute {
                backend_id: PARAKEET_BACKEND_ID.into(),
                model_id: Some(PARAKEET_MODEL_ID.into()),
                voice_id: None,
                backend_kind: SpeechBackendKind::EmbeddedModel,
                network: NetworkBehavior::Never,
            },
            text: "Complete final only.".into(),
            language: Some("en".into()),
            segments: Vec::new(),
            usage: SpeechUsage::default(), // deliberately no inference claim
        }
    }
    fn ticket(
        sink: Arc<Sink>,
        cancellation: Arc<Cancellation>,
    ) -> (
        TranscriptionTicket,
        mpsc::Sender<TranscriptionEvent>,
        oneshot::Sender<Result<TranscriptionResponse, SpeechError>>,
    ) {
        let (events_tx, events_rx) = mpsc::channel(1);
        let (final_tx, final_rx) = oneshot::channel();
        (
            TranscriptionTicket::new(
                SpeechRequestId("mock-live-request".into()),
                events_rx,
                final_rx,
                cancellation,
                Some(sink),
            ),
            events_tx,
            final_tx,
        )
    }
    async fn notified(notify: &Notify) {
        tokio::time::timeout(std::time::Duration::from_secs(2), notify.notified())
            .await
            .expect("bounded protocol checkpoint");
    }

    #[tokio::test]
    async fn live_forwarding_waits_for_all_pcm_and_final_not_partial_events() {
        let (mut producer, reader) = live_transcription::channel(identity()).expect("test setup");
        let sink = Arc::new(Sink::default());
        let cancellation = Arc::new(Cancellation::default());
        let (ticket, events, final_tx) = ticket(Arc::clone(&sink), cancellation);
        let task = tokio::spawn(consume_live_transcription(ticket, reader));
        producer.append_at(0, &[0.25; 640]).expect("test setup");
        notified(&sink.entered).await; // the first push is blocked
        events
            .try_send(TranscriptionEvent::Partial {
                request_id: SpeechRequestId("mock-live-request".into()),
                sequence: 0,
                text: "An incomplete partial".into(),
            })
            .expect("test setup");
        producer.append_at(640, &[0.5; 7]).expect("test setup"); // no wait on the blocked sink
        producer.seal(647).expect("test setup");
        assert!(!task.is_finished());
        sink.release.notify_one();
        notified(&sink.finished).await;
        assert!(
            !task.is_finished(),
            "a partial event is never the final result"
        );
        final_tx.send(Ok(response())).expect("test setup");
        let (response, receipt) = task.await.expect("test setup").expect("test setup");
        assert_eq!(response.text, "Complete final only.");
        assert_eq!(receipt.samples, 647);
        assert_eq!(receipt.metrics.backlog_samples, 0);
        let chunks = sink.chunks.lock().expect("test setup");
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            (
                chunks[0].sequence,
                chunks[0].sample_offset,
                chunks[0].data.len()
            ),
            (0, 0, 2560)
        );
        assert_eq!(
            (
                chunks[1].sequence,
                chunks[1].sample_offset,
                chunks[1].data.len()
            ),
            (1, 640, 28)
        );
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.format == live_pcm_format() && !chunk.end_of_stream)
        );
        assert_eq!(sink.finish_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn early_backend_final_cannot_certify_an_abandoned_source() {
        let (mut producer, reader) = live_transcription::channel(identity()).expect("test setup");
        let sink = Arc::new(Sink::default());
        let (ticket, _events, final_tx) =
            ticket(Arc::clone(&sink), Arc::new(Cancellation::default()));
        let task = tokio::spawn(consume_live_transcription(ticket, reader));
        producer.append_at(0, &[0.25]).expect("test setup");
        notified(&sink.entered).await;
        final_tx.send(Ok(response())).expect("test setup"); // even a backend "final" is insufficient
        drop(producer); // no physical seal; the tail could have been lost
        sink.release.notify_one();
        assert!(task.await.expect("test setup").is_err());
        assert_eq!(sink.finish_calls.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn dropping_live_forwarder_cancels_its_owned_ticket() {
        let (mut producer, reader) = live_transcription::channel(identity()).expect("test setup");
        let sink = Arc::new(Sink::default());
        let cancellation = Arc::new(Cancellation::default());
        let (ticket, _events, _final_tx) = ticket(Arc::clone(&sink), Arc::clone(&cancellation));
        let task = tokio::spawn(consume_live_transcription(ticket, reader));
        producer.append_at(0, &[0.25]).expect("test setup");
        notified(&sink.entered).await;
        task.abort();
        assert!(task.await.expect_err("expected error").is_cancelled());
        assert_eq!(cancellation.0.load(Ordering::Acquire), 1);
        assert_eq!(
            producer.append_at(1, &[0.5]),
            Err(live_transcription::IngressFailure::ConsumerClosed)
        );
    }
}
