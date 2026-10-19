#![forbid(unsafe_code)]

use async_trait::async_trait;
use ptts::preprocess::Lang;
use ptts::synth::{SpeechOptions, SpeechStreamPoll, Synth};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use speech_native_types::{
    AudioOutputFormat, AudioOutputKind, CapabilityAvailability, CapabilityEvidence,
    DEFAULT_SPEECH_EVENT_CAPACITY, EvidenceKind, EvidenceOutcome, LatencyClass, NetworkBehavior,
    PcmFormat, PcmSampleFormat, SpeechBackend, SpeechBackendDescriptor, SpeechBackendKind,
    SpeechBackendReadiness, SpeechCancellation, SpeechCapability, SpeechCapabilityLimits,
    SpeechError, SpeechErrorClass, SpeechModelDescriptor, SpeechOperationCapability,
    SpeechRequestId, SpeechResolvedRoute, SpeechUsage, SynthesisCapabilities, SynthesisEvent,
    SynthesisInput, SynthesisOutput, SynthesisRequest, SynthesisResponse, SynthesisTicket,
    TaskSupervisor, TranscriptionRequest, TranscriptionTicket, UsageProvenance, VoiceDescriptor,
};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

pub const POCKET_BACKEND_ID: &str = "pocket-tts.native-cpu";
pub const POCKET_MODEL_ID: &str = "pocket-tts-3.3.0-native-cpu";
pub const POCKET_MODEL_REPOSITORY: &str = "kyutai/pocket-tts-without-voice-cloning";
pub const POCKET_MODEL_REVISION: &str = "575d14372df89a4534bcb989870a34e77544b09f";
pub const POCKET_MODEL_RELATIVE_PATH: &str = "languages/english/model.safetensors";
pub const POCKET_MODEL_SHA256: &str =
    "e106c451108e066119f070eb7cf6c9ff4a2ad93f3bf04c9914cc94c66b7a4eeb";
pub const POCKET_MODEL_BYTES: u64 = 219_029_196;
pub const POCKET_ENCODER_REVISION: &str = "7f364c05d1be3faf2b234cdbe4cbb68a026d4218";
pub const POCKET_SAMPLE_RATE_HZ: u32 = 24_000;
pub const POCKET_EMBEDDING_DIMENSION: usize = 1024;

const VOICE_MANIFEST: &str = "manifest.json";
const CONFIG_FILE: &str = "config.json";
const TOKENIZER_FILE: &str = "tokenizer.json";
const VOICE_PREFIX: &str = "mom.pocket.";
const MAX_EVENT_BACKPRESSURE: Duration = Duration::from_millis(2);
const STREAM_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PocketConfigError {
    #[error("Pocket voice resources are unavailable: {0}")]
    Resources(String),
}

#[derive(Debug, Clone)]
pub struct PocketBackendConfig {
    pub reference_root: Option<PathBuf>,
    pub model_weights: PathBuf,
}

impl PocketBackendConfig {
    /// Discovers only local, pinned resources. It never contacts the model hub.
    #[must_use]
    pub fn discover_with_reference_root(reference_root: Option<&Path>) -> Self {
        let model_weights = desktop_model_defaults::hugging_face_hub_cache_dir()
            .unwrap_or_default()
            .join(format!(
                "models--{}",
                POCKET_MODEL_REPOSITORY.replace('/', "--")
            ))
            .join("snapshots")
            .join(POCKET_MODEL_REVISION)
            .join(POCKET_MODEL_RELATIVE_PATH);
        Self {
            reference_root: reference_root.map(Path::to_path_buf),
            model_weights,
        }
    }

    /// Explicit paths are useful to the application acquisition layer and fixture tests.
    #[must_use]
    pub fn with_paths(reference_root: Option<PathBuf>, model_weights: PathBuf) -> Self {
        Self {
            reference_root,
            model_weights,
        }
    }
}

#[derive(Debug, Deserialize)]
struct VoiceManifest {
    schema_version: u32,
    model_revision: String,
    encoder_revision: String,
    config_sha256: String,
    tokenizer_sha256: String,
    entries: Vec<VoiceManifestEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct VoiceManifestEntry {
    persona_id: String,
    speaker: String,
    source_clip_path: String,
    source_clip_sha256: String,
    embedding_path: String,
    embedding_sha256: String,
    frames: usize,
    dimension: usize,
}

/// Verifies the packaged config, tokenizer, each projection, and its exact Qwen source clip.
pub fn validate_voice_resources(root: &Path) -> Result<(), PocketConfigError> {
    read_voice_manifest(root).map(|_| ())
}

fn read_voice_manifest(root: &Path) -> Result<VoiceManifest, PocketConfigError> {
    let root = fs::canonicalize(root).map_err(|error| {
        PocketConfigError::Resources(format!("cannot open resource root: {error}"))
    })?;
    let manifest_path = root.join(VOICE_MANIFEST);
    let manifest: VoiceManifest =
        serde_json::from_slice(&fs::read(&manifest_path).map_err(|error| {
            PocketConfigError::Resources(format!("cannot read manifest: {error}"))
        })?)
        .map_err(|error| PocketConfigError::Resources(format!("manifest is invalid: {error}")))?;
    if manifest.schema_version != 1
        || manifest.model_revision != POCKET_MODEL_REVISION
        || manifest.encoder_revision != POCKET_ENCODER_REVISION
        || manifest.entries.len() != 14
    {
        return Err(PocketConfigError::Resources(
            "manifest does not match the pinned model or fourteen-voice schema".into(),
        ));
    }
    if !digest_eq(
        &hash_file(&root.join(CONFIG_FILE))?,
        &manifest.config_sha256,
    ) || !digest_eq(
        &hash_file(&root.join(TOKENIZER_FILE))?,
        &manifest.tokenizer_sha256,
    ) {
        return Err(PocketConfigError::Resources(
            "config or tokenizer digest does not match the manifest".into(),
        ));
    }
    let source_root = root
        .parent()
        .ok_or_else(|| PocketConfigError::Resources("resource root has no parent".into()))?;
    let mut seen = std::collections::HashSet::new();
    for entry in &manifest.entries {
        if !valid_persona_id(&entry.persona_id)
            || !seen.insert(entry.persona_id.as_str())
            || entry.speaker.trim().is_empty()
            || entry.dimension != POCKET_EMBEDDING_DIMENSION
            || entry.frames == 0
            || !valid_digest(&entry.source_clip_sha256)
            || !valid_digest(&entry.embedding_sha256)
        {
            return Err(PocketConfigError::Resources(format!(
                "voice entry {} has invalid identity or shape metadata",
                entry.persona_id
            )));
        }
        let embedding_path = safe_relative(&entry.embedding_path)?;
        let embedding = root.join(embedding_path);
        let expected_bytes = entry
            .frames
            .checked_mul(entry.dimension)
            .and_then(|values| values.checked_mul(std::mem::size_of::<f32>()))
            .ok_or_else(|| PocketConfigError::Resources("embedding size overflow".into()))?;
        let metadata = fs::metadata(&embedding).map_err(|error| {
            PocketConfigError::Resources(format!(
                "cannot read embedding for {}: {error}",
                entry.persona_id
            ))
        })?;
        if metadata.len() != expected_bytes as u64
            || !digest_eq(&hash_file(&embedding)?, &entry.embedding_sha256)
        {
            return Err(PocketConfigError::Resources(format!(
                "embedding for {} does not match its manifest",
                entry.persona_id
            )));
        }
        let source_rel = safe_relative(&entry.source_clip_path)?;
        let source_clip = source_root.join(source_rel);
        if !digest_eq(&hash_file(&source_clip)?, &entry.source_clip_sha256) {
            return Err(PocketConfigError::Resources(format!(
                "source clip for {} does not match its manifest",
                entry.persona_id
            )));
        }
    }
    Ok(manifest)
}

fn read_admitted_voices(
    root: &Path,
    manifest: &VoiceManifest,
) -> Result<Vec<AdmittedVoice>, PocketConfigError> {
    let root = fs::canonicalize(root).map_err(|error| {
        PocketConfigError::Resources(format!("cannot open resource root: {error}"))
    })?;
    manifest
        .entries
        .iter()
        .map(|entry| {
            let path = root.join(safe_relative(&entry.embedding_path)?);
            let bytes = fs::read(&path).map_err(|error| {
                PocketConfigError::Resources(format!("cannot read voice projection: {error}"))
            })?;
            let expected_bytes = entry.frames * entry.dimension * std::mem::size_of::<f32>();
            if bytes.len() != expected_bytes
                || !digest_eq(
                    &format!("{:x}", Sha256::digest(&bytes)),
                    &entry.embedding_sha256,
                )
            {
                return Err(PocketConfigError::Resources(
                    "voice projection integrity check failed".into(),
                ));
            }
            let values: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect();
            if values.iter().any(|value| !value.is_finite()) {
                return Err(PocketConfigError::Resources(
                    "voice projection contains non-finite values".into(),
                ));
            }
            Ok(AdmittedVoice {
                persona_id: entry.persona_id.clone(),
                frames: entry.frames,
                values,
            })
        })
        .collect()
}

fn safe_relative(value: &str) -> Result<&Path, PocketConfigError> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(PocketConfigError::Resources(
            "manifest contains an unsafe relative path".into(),
        ));
    }
    Ok(path)
}

fn valid_persona_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn digest_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn hash_file(path: &Path) -> Result<String, PocketConfigError> {
    let mut file = fs::File::open(path).map_err(|error| {
        PocketConfigError::Resources(format!("cannot open {}: {error}", path.display()))
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            PocketConfigError::Resources(format!("cannot hash {}: {error}", path.display()))
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

struct BackendState {
    config: PocketBackendConfig,
    manifest: Option<VoiceManifest>,
    voices: Vec<AdmittedVoice>,
    descriptor: Mutex<SpeechBackendDescriptor>,
    synth: Mutex<Option<Arc<Synth>>>,
    active: Mutex<HashMap<SpeechRequestId, Arc<AtomicBool>>>,
    tasks: Arc<TaskSupervisor>,
    closed: AtomicBool,
}

struct AdmittedVoice {
    persona_id: String,
    frames: usize,
    values: Vec<f32>,
}

#[derive(Clone)]
pub struct PocketSpeechBackend {
    state: Arc<BackendState>,
}

impl PocketSpeechBackend {
    #[must_use]
    pub fn discover(config: PocketBackendConfig) -> Self {
        let manifest = config
            .reference_root
            .as_deref()
            .and_then(|root| read_voice_manifest(root).ok());
        let voices = match (config.reference_root.as_deref(), manifest.as_ref()) {
            (Some(root), Some(manifest)) => {
                read_admitted_voices(root, manifest).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let model_present = config.model_weights.is_file();
        let ready = manifest.is_some() && voices.len() == 14 && model_present;
        let descriptor = build_descriptor(ready, false, manifest.as_ref());
        Self {
            state: Arc::new(BackendState {
                config,
                manifest,
                voices,
                descriptor: Mutex::new(descriptor),
                synth: Mutex::new(None),
                active: Mutex::new(HashMap::new()),
                tasks: Arc::new(TaskSupervisor::with_scope("pocket-tts-cpu")),
                closed: AtomicBool::new(false),
            }),
        }
    }

    fn load_synth(&self, request_id: &SpeechRequestId) -> Result<Arc<Synth>, SpeechError> {
        let mut synth_guard = self.state.synth.lock().map_err(|_| {
            backend_error(
                request_id,
                "pocket_state_unavailable",
                SpeechErrorClass::Internal,
                false,
                "Pocket TTS state is unavailable",
            )
        })?;
        if let Some(synth) = synth_guard.as_ref() {
            return Ok(Arc::clone(synth));
        }
        if self.state.closed.load(Ordering::Acquire) {
            return Err(cancelled_error(request_id));
        }
        let root = self.state.config.reference_root.as_deref().ok_or_else(|| {
            unavailable(
                request_id,
                "pocket_voice_resources_unavailable",
                "Pocket voice resources are unavailable",
            )
        })?;
        let weights = &self.state.config.model_weights;
        verify_model_file(weights).map_err(|_| {
            unavailable(
                request_id,
                "pocket_model_unavailable",
                "The pinned Pocket model checkpoint is unavailable or invalid",
            )
        })?;
        xn::set_num_threads(1);
        let config = ptts::checkpoint::read_config(root.join(CONFIG_FILE)).map_err(|_| {
            unavailable(
                request_id,
                "pocket_config_invalid",
                "Pocket model configuration could not be loaded",
            )
        })?;
        let mut synth = Synth::builder(config, weights, Lang::En)
            .tokenizer_file(root.join(TOKENIZER_FILE))
            .device(ptts::synth::DeviceKind::Cpu)
            .precomputed_voices_only()
            .build()
            .map_err(|_| {
                unavailable(
                    request_id,
                    "pocket_model_load_failed",
                    "The pinned Pocket model could not be loaded",
                )
            })?;
        if synth.sample_rate() != POCKET_SAMPLE_RATE_HZ {
            return Err(unavailable(
                request_id,
                "pocket_sample_rate_mismatch",
                "Pocket model output did not match the admitted 24 kHz audio format",
            ));
        }
        for voice in &self.state.voices {
            synth
                .add_voice_from_embedding(
                    &format!("{VOICE_PREFIX}{}", voice.persona_id),
                    &voice.values,
                    voice.frames,
                    POCKET_EMBEDDING_DIMENSION,
                    None,
                )
                .map_err(|_| {
                    unavailable(
                        request_id,
                        "pocket_embedding_rejected",
                        "The Pocket model rejected a packaged voice projection",
                    )
                })?;
        }
        let synth = Arc::new(synth);
        *synth_guard = Some(Arc::clone(&synth));
        if let Ok(mut descriptor) = self.state.descriptor.lock()
            && let Some(model) = descriptor.models.first_mut()
        {
            model.resident = true;
        }
        Ok(synth)
    }
}

#[async_trait]
impl SpeechBackend for PocketSpeechBackend {
    fn descriptor(&self) -> SpeechBackendDescriptor {
        self.state
            .descriptor
            .lock()
            .map(|descriptor| descriptor.clone())
            .unwrap_or_else(|_| build_descriptor(false, false, None))
    }

    fn readiness(&self) -> SpeechBackendReadiness {
        self.descriptor().readiness
    }

    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionTicket, SpeechError> {
        Err(backend_error(
            &request.context.request_id,
            "pocket_transcription_unsupported",
            SpeechErrorClass::Capability,
            false,
            "Pocket TTS only synthesizes speech",
        ))
    }

    async fn synthesize(&self, request: SynthesisRequest) -> Result<SynthesisTicket, SpeechError> {
        validate_request(&request, &self.state)?;
        if self.state.closed.load(Ordering::Acquire) {
            return Err(unavailable(
                &request.context.request_id,
                "pocket_backend_closed",
                "Pocket TTS is shutting down",
            ));
        }
        let request_id = request.context.request_id.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut active = self.state.active.lock().map_err(|_| {
                backend_error(
                    &request_id,
                    "pocket_state_unavailable",
                    SpeechErrorClass::Internal,
                    false,
                    "Pocket TTS request state is unavailable",
                )
            })?;
            if !active.is_empty() {
                return Err(unavailable(
                    &request_id,
                    "pocket_backend_busy",
                    "Pocket TTS is already processing a request",
                ));
            }
            if active
                .insert(request_id.clone(), Arc::clone(&cancelled))
                .is_some()
            {
                return Err(SpeechError::invalid_request(
                    &request_id,
                    "pocket_request_duplicate",
                    "This Pocket request is already active",
                ));
            }
        }
        let (events_tx, events_rx) = mpsc::channel(DEFAULT_SPEECH_EVENT_CAPACITY);
        let (final_tx, final_rx) = oneshot::channel();
        let state = Arc::clone(&self.state);
        let worker_state = self.clone();
        let worker_request_id = request_id.clone();
        let worker_cancelled = Arc::clone(&cancelled);
        let dispatched_at = Instant::now();
        let spawn_result =
            self.state
                .tasks
                .spawn_blocking(format!("pocket-tts:{request_id}"), move || {
                    let _active_cleanup = ActiveCleanup {
                        state: Arc::clone(&state),
                        request_id: worker_request_id.clone(),
                        cancellation: Arc::clone(&worker_cancelled),
                    };
                    run_synthesis(
                        &worker_state,
                        request,
                        worker_cancelled,
                        events_tx,
                        final_tx,
                        dispatched_at,
                    )
                });
        if spawn_result.is_err() {
            if let Ok(mut active) = self.state.active.lock() {
                active.remove(&request_id);
            }
            return Err(unavailable(
                &request_id,
                "pocket_worker_unavailable",
                "Pocket TTS could not start a worker",
            ));
        }
        Ok(SynthesisTicket::new(
            request_id,
            events_rx,
            final_rx,
            Arc::new(PocketCancellation {
                state: Arc::clone(&self.state),
            }),
        ))
    }

    fn cancel(&self, request_id: &SpeechRequestId) -> usize {
        cancel_active(&self.state, request_id)
    }

    async fn shutdown(&self) -> Result<(), SpeechError> {
        self.state.closed.store(true, Ordering::Release);
        if let Ok(active) = self.state.active.lock() {
            for cancel in active.values() {
                cancel.store(true, Ordering::Release);
            }
        }
        self.state.tasks.begin_shutdown().map_err(|_| {
            backend_error(
                &SpeechRequestId("pocket-shutdown".into()),
                "pocket_shutdown_failed",
                SpeechErrorClass::Internal,
                false,
                "Pocket TTS workers could not be stopped",
            )
        })?;
        self.state.tasks.wait_for_idle().await.map_err(|_| {
            backend_error(
                &SpeechRequestId("pocket-shutdown".into()),
                "pocket_shutdown_failed",
                SpeechErrorClass::Internal,
                false,
                "Pocket TTS workers did not settle",
            )
        })?;
        if let Ok(mut synth) = self.state.synth.lock() {
            synth.take();
        }
        Ok(())
    }
}

struct PocketCancellation {
    state: Arc<BackendState>,
}

struct ActiveCleanup {
    state: Arc<BackendState>,
    request_id: SpeechRequestId,
    cancellation: Arc<AtomicBool>,
}

impl Drop for ActiveCleanup {
    fn drop(&mut self) {
        remove_active(&self.state, &self.request_id, &self.cancellation);
    }
}

fn remove_active(state: &BackendState, request_id: &SpeechRequestId, expected: &Arc<AtomicBool>) {
    if let Ok(mut active) = state.active.lock()
        && active
            .get(request_id)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
    {
        active.remove(request_id);
    }
}

impl SpeechCancellation for PocketCancellation {
    fn cancel(&self, request_id: &SpeechRequestId) -> usize {
        cancel_active(&self.state, request_id)
    }
}

fn cancel_active(state: &BackendState, request_id: &SpeechRequestId) -> usize {
    state
        .active
        .lock()
        .ok()
        .and_then(|active| active.get(request_id).cloned())
        .map_or(0, |cancelled| {
            let already = cancelled.swap(true, Ordering::AcqRel);
            usize::from(!already)
        })
}

fn validate_request(request: &SynthesisRequest, state: &BackendState) -> Result<(), SpeechError> {
    request.validate()?;
    let input_text = match &request.input {
        SynthesisInput::Text { text } => text,
        SynthesisInput::Ssml { .. } => {
            return Err(SpeechError::invalid_request(
                &request.context.request_id,
                "pocket_ssml_unsupported",
                "Pocket TTS accepts plain text only",
            ));
        }
    };
    if input_text.chars().count() > 3_000 {
        return Err(SpeechError::invalid_request(
            &request.context.request_id,
            "pocket_input_too_long",
            "Pocket TTS requests are limited to 3000 characters",
        ));
    }
    if !state
        .descriptor
        .lock()
        .map(|descriptor| descriptor.readiness.is_ready())
        .unwrap_or(false)
    {
        return Err(unavailable(
            &request.context.request_id,
            "pocket_backend_unavailable",
            "Pocket TTS assets are not ready",
        ));
    }
    if request
        .language
        .as_deref()
        .is_some_and(|language| !matches!(language, "en" | "en-US"))
    {
        return Err(SpeechError::invalid_request(
            &request.context.request_id,
            "pocket_language_unsupported",
            "Pocket TTS currently supports English only",
        ));
    }
    if request.rate != 1.0 || request.pitch != 1.0 || request.volume != 1.0 {
        return Err(backend_error(
            &request.context.request_id,
            "pocket_control_unsupported",
            SpeechErrorClass::Capability,
            false,
            "Pocket TTS does not expose rate, pitch, or volume controls",
        ));
    }
    if request.alignment != speech_native_types::AlignmentGranularity::None || !request.stream {
        return Err(backend_error(
            &request.context.request_id,
            "pocket_request_mode_unsupported",
            SpeechErrorClass::Capability,
            false,
            "Pocket TTS requires streamed audio without alignment",
        ));
    }
    match &request.output {
        AudioOutputFormat::Pcm { format } if *format == pocket_pcm_format() => {}
        _ => {
            return Err(backend_error(
                &request.context.request_id,
                "pocket_output_unsupported",
                SpeechErrorClass::Capability,
                false,
                "Pocket TTS returns mono 24 kHz PCM16 audio",
            ));
        }
    }
    let Some(voice_id) = exact_voice_id(request) else {
        return Err(SpeechError::invalid_request(
            &request.context.request_id,
            "pocket_voice_required",
            "Select one installed Pocket persona",
        ));
    };
    if !state.manifest.as_ref().is_some_and(|manifest| {
        manifest
            .entries
            .iter()
            .any(|entry| format!("{VOICE_PREFIX}{}", entry.persona_id) == voice_id)
    }) {
        return Err(SpeechError::invalid_request(
            &request.context.request_id,
            "pocket_voice_unknown",
            "The selected Pocket persona is not installed",
        ));
    }
    if let speech_native_types::SpeechRouteSelector::ExactBackend {
        backend_id,
        model_id,
        voice_id: route_voice,
    } = &request.context.route
        && (backend_id != POCKET_BACKEND_ID
            || model_id.as_deref() != Some(POCKET_MODEL_ID)
            || route_voice.as_deref() != Some(voice_id))
    {
        return Err(backend_error(
            &request.context.request_id,
            "pocket_route_mismatch",
            SpeechErrorClass::Capability,
            false,
            "Pocket TTS request selected a different route",
        ));
    }
    Ok(())
}

fn exact_voice_id(request: &SynthesisRequest) -> Option<&str> {
    match &request.voice {
        speech_native_types::VoiceSelector::Exact { voice_id }
            if voice_id.starts_with(VOICE_PREFIX) =>
        {
            Some(voice_id)
        }
        _ => None,
    }
}

fn run_synthesis(
    backend: &PocketSpeechBackend,
    request: SynthesisRequest,
    cancelled: Arc<AtomicBool>,
    events: mpsc::Sender<SynthesisEvent>,
    final_sender: oneshot::Sender<Result<SynthesisResponse, SpeechError>>,
    dispatched_at: Instant,
) -> Result<(), String> {
    let request_id = request.context.request_id.clone();
    let route = SpeechResolvedRoute {
        backend_id: POCKET_BACKEND_ID.into(),
        model_id: Some(POCKET_MODEL_ID.into()),
        voice_id: exact_voice_id(&request).map(str::to_owned),
        backend_kind: SpeechBackendKind::EmbeddedModel,
        network: NetworkBehavior::Never,
    };
    let result = synthesize_stream(
        backend,
        &request,
        &cancelled,
        &events,
        &route,
        dispatched_at,
    );
    remove_active(&backend.state, &request_id, &cancelled);
    match result {
        Ok(response) => {
            // The final channel is authoritative. Never block settlement on
            // a terminal notification after request ownership has been released.
            let _ = events.try_send(SynthesisEvent::Completed {
                request_id: request_id.clone(),
                response: response.clone(),
            });
            let _ = final_sender.send(Ok(response));
        }
        Err(error) => {
            let event = if error.class == SpeechErrorClass::Cancelled {
                SynthesisEvent::Cancelled {
                    request_id: request_id.clone(),
                    usage: SpeechUsage::default(),
                }
            } else {
                SynthesisEvent::Failed {
                    request_id: request_id.clone(),
                    error: error.clone(),
                }
            };
            let _ = events.try_send(event);
            let _ = final_sender.send(Err(error.clone()));
            if error.class != SpeechErrorClass::Cancelled {
                return Err(error.safe_detail);
            }
        }
    }
    Ok(())
}

fn synthesize_stream(
    backend: &PocketSpeechBackend,
    request: &SynthesisRequest,
    cancelled: &AtomicBool,
    events: &mpsc::Sender<SynthesisEvent>,
    route: &SpeechResolvedRoute,
    dispatched_at: Instant,
) -> Result<SynthesisResponse, SpeechError> {
    let request_id = &request.context.request_id;
    let queue_us = u64::try_from(dispatched_at.elapsed().as_micros()).unwrap_or(u64::MAX);
    check_deadlines(request, dispatched_at, dispatched_at, None)?;
    send_event(
        events,
        SynthesisEvent::Started {
            request_id: request_id.clone(),
            route: route.clone(),
        },
        cancelled,
        request,
        dispatched_at,
        None,
        None,
    )?;
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error(request_id));
    }
    let model_started = Instant::now();
    let synth = backend.load_synth(request_id)?;
    let model_load_ms = u64::try_from(model_started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if request
        .context
        .deadline
        .model_load_ms
        .is_some_and(|limit| model_load_ms > limit)
    {
        return Err(timeout_error(
            request_id,
            "pocket_model_load_timeout",
            "Pocket TTS model loading exceeded the request deadline",
        ));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error(request_id));
    }
    let text = match &request.input {
        SynthesisInput::Text { text } => text,
        SynthesisInput::Ssml { .. } => unreachable!(),
    };
    let voice_id = exact_voice_id(request).expect("request validation requires an exact voice");
    let options = SpeechOptions::default()
        .voice(voice_id)
        .temperature(0.3)
        .seed(4_242_424_242_424_242);
    let mut stream = synth.stream_with(text, &options).map_err(|_| {
        unavailable(
            request_id,
            "pocket_stream_start_failed",
            "Pocket TTS could not start the audio stream",
        )
    })?;
    let mut sequence = 0u64;
    let mut sample_offset = 0u64;
    let mut total_bytes = 0usize;
    let mut first_pcm_at = None;
    let mut last_progress_at = Instant::now();
    loop {
        if cancelled.load(Ordering::Acquire) {
            drop(stream);
            return Err(cancelled_error(request_id));
        }
        check_deadlines(request, dispatched_at, last_progress_at, first_pcm_at)?;
        match stream.recv_timeout(STREAM_POLL_INTERVAL) {
            SpeechStreamPoll::Pending => continue,
            SpeechStreamPoll::Finished => break,
            SpeechStreamPoll::Chunk(Err(_)) => {
                return Err(unavailable(
                    request_id,
                    "pocket_generation_failed",
                    "Pocket TTS generation failed",
                ));
            }
            SpeechStreamPoll::Chunk(Ok(samples)) => {
                if first_pcm_at.is_none() {
                    first_pcm_at = Some(Instant::now());
                }
                last_progress_at = Instant::now();
                let data = pcm_f32_to_i16le(&samples).map_err(|_| {
                    backend_error(
                        request_id,
                        "pocket_pcm_invalid",
                        SpeechErrorClass::Internal,
                        false,
                        "Pocket TTS returned invalid PCM samples",
                    )
                })?;
                total_bytes = total_bytes.checked_add(data.len()).ok_or_else(|| {
                    backend_error(
                        request_id,
                        "pocket_audio_size_overflow",
                        SpeechErrorClass::Internal,
                        false,
                        "Pocket TTS output size overflowed",
                    )
                })?;
                if total_bytes > MAX_OUTPUT_BYTES {
                    return Err(SpeechError::invalid_request(
                        request_id,
                        "pocket_audio_too_large",
                        "Pocket TTS output exceeded the local size limit",
                    ));
                }
                let chunk = speech_native_types::AudioChunk {
                    sequence,
                    sample_offset,
                    format: pocket_pcm_format(),
                    data: data.clone(),
                    end_of_stream: false,
                };
                send_event(
                    events,
                    SynthesisEvent::Audio {
                        request_id: request_id.clone(),
                        chunk,
                    },
                    cancelled,
                    request,
                    dispatched_at,
                    Some(last_progress_at),
                    first_pcm_at,
                )?;
                sample_offset = sample_offset.saturating_add((data.len() / 2) as u64);
                sequence = sequence.saturating_add(1);
            }
        }
    }
    drop(stream);
    if total_bytes == 0 {
        return Err(unavailable(
            request_id,
            "pocket_empty_audio",
            "Pocket TTS returned no audio",
        ));
    }
    let terminal = speech_native_types::AudioChunk {
        sequence,
        sample_offset,
        format: pocket_pcm_format(),
        data: Vec::new(),
        end_of_stream: true,
    };
    send_event(
        events,
        SynthesisEvent::Audio {
            request_id: request_id.clone(),
            chunk: terminal,
        },
        cancelled,
        request,
        dispatched_at,
        Some(last_progress_at),
        first_pcm_at,
    )?;
    let duration_ms = sample_offset.saturating_mul(1000) / u64::from(POCKET_SAMPLE_RATE_HZ);
    let output = SynthesisOutput::Streamed {
        format: request.output.clone(),
        emitted_bytes: total_bytes as u64,
    };
    let response = SynthesisResponse {
        request_id: request_id.clone(),
        route: route.clone(),
        output,
        duration_ms: Some(duration_ms),
        alignments: Vec::new(),
        usage: SpeechUsage {
            queue_ms: Some(queue_us / 1_000),
            queue_us: Some(queue_us),
            output_audio_ms: Some(duration_ms),
            input_characters: Some(u64::try_from(text.chars().count()).unwrap_or(u64::MAX)),
            model_load_ms: Some(model_load_ms),
            time_to_first_result_ms: first_pcm_at.map(|at| {
                u64::try_from(at.duration_since(dispatched_at).as_millis()).unwrap_or(u64::MAX)
            }),
            total_ms: Some(u64::try_from(dispatched_at.elapsed().as_millis()).unwrap_or(u64::MAX)),
            provenance: UsageProvenance::Exact,
            real_local_inference: true,
            ..SpeechUsage::default()
        },
    };
    Ok(response)
}

fn send_event(
    sender: &mpsc::Sender<SynthesisEvent>,
    mut event: SynthesisEvent,
    cancelled: &AtomicBool,
    request: &SynthesisRequest,
    dispatched_at: Instant,
    last_progress_at: Option<Instant>,
    first_pcm_at: Option<Instant>,
) -> Result<(), SpeechError> {
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(cancelled_error(&request.context.request_id));
        }
        check_deadlines(
            request,
            dispatched_at,
            last_progress_at.unwrap_or(dispatched_at),
            first_pcm_at,
        )?;
        match sender.try_send(event) {
            Ok(()) => return Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(cancelled_error(&request.context.request_id));
            }
            Err(mpsc::error::TrySendError::Full(returned)) => {
                event = returned;
                thread::sleep(MAX_EVENT_BACKPRESSURE);
            }
        }
    }
}

fn check_deadlines(
    request: &SynthesisRequest,
    dispatched_at: Instant,
    last_progress_at: Instant,
    first_pcm_at: Option<Instant>,
) -> Result<(), SpeechError> {
    let deadline = &request.context.deadline;
    let elapsed = dispatched_at.elapsed();
    if deadline
        .total_ms
        .is_some_and(|limit| elapsed > Duration::from_millis(limit))
    {
        return Err(timeout_error(
            &request.context.request_id,
            "pocket_total_timeout",
            "Pocket TTS exceeded the total request deadline",
        ));
    }
    if first_pcm_at.is_none()
        && deadline
            .first_result_ms
            .is_some_and(|limit| elapsed > Duration::from_millis(limit))
    {
        return Err(timeout_error(
            &request.context.request_id,
            "pocket_first_pcm_timeout",
            "Pocket TTS exceeded the first audio deadline",
        ));
    }
    if first_pcm_at.is_some()
        && deadline
            .idle_stream_ms
            .is_some_and(|limit| last_progress_at.elapsed() > Duration::from_millis(limit))
    {
        return Err(timeout_error(
            &request.context.request_id,
            "pocket_stream_idle_timeout",
            "Pocket TTS audio stream stopped making progress",
        ));
    }
    Ok(())
}

fn pcm_f32_to_i16le(samples: &[f32]) -> Result<Vec<u8>, ()> {
    let mut output = Vec::with_capacity(samples.len().saturating_mul(2));
    for sample in samples {
        if !sample.is_finite() {
            return Err(());
        }
        let value = sample.clamp(-1.0, 1.0);
        let pcm = if value <= -1.0 {
            i16::MIN
        } else {
            (value * f32::from(i16::MAX)).round() as i16
        };
        output.extend_from_slice(&pcm.to_le_bytes());
    }
    Ok(output)
}

fn verify_model_file(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    if metadata.len() != POCKET_MODEL_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pinned Pocket model has an unexpected size",
        ));
    }
    let digest = hash_file(path)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if !digest_eq(&digest, POCKET_MODEL_SHA256) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pinned Pocket model has an unexpected digest",
        ));
    }
    Ok(())
}

fn pocket_pcm_format() -> PcmFormat {
    PcmFormat {
        sample_rate_hz: POCKET_SAMPLE_RATE_HZ,
        channels: 1,
        sample_format: PcmSampleFormat::I16Le,
        interleaved: true,
    }
}

fn build_descriptor(
    ready: bool,
    resident: bool,
    manifest: Option<&VoiceManifest>,
) -> SpeechBackendDescriptor {
    let readiness = if ready {
        SpeechBackendReadiness::Ready
    } else {
        SpeechBackendReadiness::AssetInstallRequired { assets: Vec::new() }
    };
    let availability = if ready {
        CapabilityAvailability::DeferredLoad
    } else {
        CapabilityAvailability::AssetInstallRequired
    };
    let voices = manifest.map_or_else(Vec::new, |manifest| {
        manifest
            .entries
            .iter()
            .map(|entry| VoiceDescriptor {
                id: format!("{VOICE_PREFIX}{}", entry.persona_id),
                name: entry.speaker.clone(),
                language: "en-US".into(),
                gender: None,
                quality: None,
                expected_latency: Some(LatencyClass::Normal),
                network: NetworkBehavior::Never,
                installed: true,
            })
            .collect()
    });
    SpeechBackendDescriptor {
        id: POCKET_BACKEND_ID.into(),
        display_name: "Pocket TTS CPU".into(),
        kind: SpeechBackendKind::EmbeddedModel,
        readiness,
        capabilities: vec![SpeechCapability {
            id: "pocket-tts.native-cpu.synthesis".into(),
            backend_id: POCKET_BACKEND_ID.into(),
            model_id: Some(POCKET_MODEL_ID.into()),
            operation: SpeechOperationCapability::Synthesis(SynthesisCapabilities {
                streaming_audio: true,
                ssml: false,
                word_alignment: false,
                phoneme_alignment: false,
                pause_resume: false,
                voice_selection: true,
                returned_audio: vec![AudioOutputKind::Pcm],
            }),
            availability,
            network: NetworkBehavior::Never,
            languages: vec!["en-US".into()],
            limits: SpeechCapabilityLimits {
                max_concurrent_requests: Some(1),
                max_input_characters: Some(3_000),
                ..SpeechCapabilityLimits::default()
            },
            evidence: if ready {
                vec![CapabilityEvidence {
                    source_id: "pocket-tts-pinned-local-assets".into(),
                    source_version: Some(POCKET_MODEL_REVISION.into()),
                    kind: EvidenceKind::SystemInventory,
                outcome: EvidenceOutcome::Inconclusive,
                    observed_at_unix_ms: now_unix_ms(),
                    detail: "Pinned local checkpoint and manifest-bound voice projections are available; model load is deferred until synthesis.".into(),
                }]
            } else {
                Vec::new()
            },
        }],
        models: vec![SpeechModelDescriptor {
            id: POCKET_MODEL_ID.into(),
            display_name: "Pocket TTS English CPU".into(),
            family: "pocket-tts".into(),
            languages: vec!["en-US".into()],
            resident,
            estimated_memory_bytes: Some(2_400_000_000),
            content_hash: Some(POCKET_MODEL_SHA256.into()),
        }],
        voices,
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn backend_error(
    request_id: &SpeechRequestId,
    code: &str,
    class: SpeechErrorClass,
    retryable: bool,
    detail: &str,
) -> SpeechError {
    SpeechError {
        code: code.into(),
        class,
        retryable,
        request_id: request_id.clone(),
        backend_id: Some(POCKET_BACKEND_ID.into()),
        safe_detail: detail.into(),
    }
}

fn unavailable(request_id: &SpeechRequestId, code: &str, detail: &str) -> SpeechError {
    backend_error(
        request_id,
        code,
        SpeechErrorClass::Unavailable,
        true,
        detail,
    )
}

fn timeout_error(request_id: &SpeechRequestId, code: &str, detail: &str) -> SpeechError {
    backend_error(request_id, code, SpeechErrorClass::Timeout, false, detail)
}

fn cancelled_error(request_id: &SpeechRequestId) -> SpeechError {
    backend_error(
        request_id,
        "pocket_cancelled",
        SpeechErrorClass::Cancelled,
        false,
        "Pocket TTS request was cancelled",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_descriptor_is_valid_and_does_not_claim_runtime_capability() {
        let entries = (0..14)
            .map(|index| VoiceManifestEntry {
                persona_id: format!("voice_{index}"),
                speaker: format!("Voice {index}"),
                source_clip_path: format!("qwen-icl/clips/voice_{index}.wav"),
                source_clip_sha256: "0".repeat(64),
                embedding_path: format!("voices/voice_{index}.f32le"),
                embedding_sha256: "0".repeat(64),
                frames: 1,
                dimension: POCKET_EMBEDDING_DIMENSION,
            })
            .collect();
        let manifest = VoiceManifest {
            schema_version: 1,
            model_revision: POCKET_MODEL_REVISION.into(),
            encoder_revision: POCKET_ENCODER_REVISION.into(),
            config_sha256: "0".repeat(64),
            tokenizer_sha256: "0".repeat(64),
            entries,
        };
        let descriptor = build_descriptor(true, false, Some(&manifest));
        descriptor
            .validate()
            .expect("deferred descriptor must satisfy the shared contract");
        let capability = &descriptor.capabilities[0];
        assert!(capability.deferred_load_admissible());
        assert!(!capability.eligible_for_local_only());
        assert!(!descriptor.models[0].resident);
    }

    #[test]
    fn pcm_conversion_rejects_nonfinite_values_and_clamps_finite_samples() {
        assert!(pcm_f32_to_i16le(&[f32::NAN]).is_err());
        assert_eq!(
            pcm_f32_to_i16le(&[-1.0, 0.0, 1.0]).expect("finite PCM"),
            [0, 128, 0, 0, 255, 127]
        );
    }
}
