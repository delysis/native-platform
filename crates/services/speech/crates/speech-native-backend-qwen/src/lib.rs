#![forbid(unsafe_code)]

use async_trait::async_trait;
use speech_native_types::{
    AudioOutputFormat, AudioOutputKind, CapabilityAvailability, CapabilityEvidence,
    DEFAULT_SPEECH_EVENT_CAPACITY, EvidenceKind, EvidenceOutcome, LatencyClass, NetworkBehavior,
    SpeechBackend, SpeechBackendDescriptor, SpeechBackendKind, SpeechBackendReadiness,
    SpeechCancellation, SpeechCapability, SpeechCapabilityLimits, SpeechError, SpeechErrorClass,
    SpeechModelDescriptor, SpeechOperationCapability, SpeechRequestId, SpeechResolvedRoute,
    SpeechRouteSelector, SpeechUsage, SynthesisCapabilities, SynthesisEvent, SynthesisInput,
    SynthesisOutput, SynthesisRequest, SynthesisResponse, SynthesisTicket, TaskSupervisor,
    TranscriptionRequest, TranscriptionTicket, UsageProvenance, VoiceDescriptor, VoiceGender,
    VoiceQuality, VoiceSelector,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};

pub const QWEN_BACKEND_ID: &str = "qwen3-tts.mlx-swift";
pub const QWEN_MODEL_ID: &str = "qwen3-tts-12hz-1.7b-voice-design-4bit";
pub const QWEN_VOICE_ID: &str = "mom.warm-clinician-v1";
pub const QWEN_MODEL_DIRECTORY: &str = "qwen3-tts-1.7b-voice-design-4bit";
pub const QWEN_MODEL_REVISION: &str = "5c390979e4b93af5f2932f90742ca99c7dd04687";
const MAX_INPUT_CHARACTERS: u64 = 4_000;
const VOICE_DESCRIPTION: &str = "A warm, articulate, reassuring mature American woman with natural conversational rhythm and gentle humor. Speak like a trusted colleague, never like an announcer.";

#[derive(Debug, Clone)]
pub struct QwenBackendConfig {
    pub helper: PathBuf,
    pub model_dir: PathBuf,
}

impl QwenBackendConfig {
    #[must_use]
    pub fn discover(managed_model_root: Option<&Path>) -> Self {
        let helper = std::env::var_os("MOM_QWEN3_TTS_HELPER")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|p| p.join("qwen3-tts-helper")))
            })
            .unwrap_or_else(|| PathBuf::from("qwen3-tts-helper"));
        let model_dir = std::env::var_os("MOM_QWEN3_TTS_MODEL")
            .map(PathBuf::from)
            .or_else(|| managed_model_root.map(|root| root.join(QWEN_MODEL_DIRECTORY)))
            .filter(|path| path.is_dir())
            .or_else(|| {
                std::env::var_os("HOME").map(PathBuf::from).map(|home| {
                    home.join("Library/Application Support/com.delysis.llama-native-kit.mom-llama/speech-models")
                        .join(QWEN_MODEL_DIRECTORY)
                })
            })
            .unwrap_or_else(|| PathBuf::from(QWEN_MODEL_DIRECTORY));
        Self { helper, model_dir }
    }
}

#[derive(Clone)]
pub struct QwenSpeechBackend {
    descriptor: SpeechBackendDescriptor,
    config: QwenBackendConfig,
    tasks: Arc<TaskSupervisor>,
    active: Arc<Mutex<HashMap<SpeechRequestId, Arc<AtomicBool>>>>,
}

impl QwenSpeechBackend {
    #[must_use]
    pub fn discover(config: QwenBackendConfig) -> Self {
        let ready = config.helper.is_file() && model_admitted(&config.model_dir);
        Self {
            descriptor: descriptor(ready, &config),
            config,
            tasks: Arc::new(TaskSupervisor::with_scope("qwen3-tts")),
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

fn model_admitted(model_dir: &Path) -> bool {
    [
        "config.json",
        "generation_config.json",
        "model.safetensors",
        "model.safetensors.index.json",
        "tokenizer_config.json",
        "vocab.json",
        "merges.txt",
        "speech_tokenizer/config.json",
        "speech_tokenizer/configuration.json",
        "speech_tokenizer/model.safetensors",
        "speech_tokenizer/preprocessor_config.json",
    ]
    .into_iter()
    .all(|relative| {
        let artifact = model_dir.join(relative);
        let metadata = model_dir
            .join(".cache/huggingface/download")
            .join(format!("{relative}.metadata"));
        artifact.metadata().is_ok_and(|value| value.len() > 0)
            && fs::read_to_string(metadata)
                .is_ok_and(|value| value.lines().next() == Some(QWEN_MODEL_REVISION))
    })
}

struct Cancellation {
    active: Arc<Mutex<HashMap<SpeechRequestId, Arc<AtomicBool>>>>,
}

impl SpeechCancellation for Cancellation {
    fn cancel(&self, request_id: &SpeechRequestId) -> usize {
        self.active
            .lock()
            .ok()
            .and_then(|map| map.get(request_id).cloned())
            .map_or(0, |flag| {
                flag.store(true, Ordering::Release);
                1
            })
    }
}

#[async_trait]
impl SpeechBackend for QwenSpeechBackend {
    fn descriptor(&self) -> SpeechBackendDescriptor {
        self.descriptor.clone()
    }
    fn readiness(&self) -> SpeechBackendReadiness {
        self.descriptor.readiness.clone()
    }

    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionTicket, SpeechError> {
        Err(error(
            &request.context.request_id,
            "qwen_tts_transcription_unsupported",
            SpeechErrorClass::Capability,
            false,
            "Qwen3 TTS does not transcribe audio",
        ))
    }

    async fn synthesize(&self, request: SynthesisRequest) -> Result<SynthesisTicket, SpeechError> {
        validate_request(&request, &self.descriptor)?;
        let request_id = request.context.request_id.clone();
        let route = route();
        let (events_tx, events_rx) = mpsc::channel(DEFAULT_SPEECH_EVENT_CAPACITY);
        let (final_tx, final_rx) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.active
            .lock()
            .map_err(|_| {
                unavailable(
                    &request_id,
                    "qwen_state_unavailable",
                    "Qwen speech state is unavailable",
                )
            })?
            .insert(request_id.clone(), Arc::clone(&cancelled));
        let active = Arc::clone(&self.active);
        let config = self.config.clone();
        let worker_id = request_id.clone();
        let cancellation = Arc::clone(&cancelled);
        self.tasks
            .spawn_blocking(format!("qwen-tts:{request_id}"), move || {
                let result =
                    synthesize_complete(&request, &config, &route, &cancellation, &events_tx);
                let _ = match &result {
                    Ok(response) => events_tx.blocking_send(SynthesisEvent::Completed {
                        request_id: worker_id.clone(),
                        response: response.clone(),
                    }),
                    Err(err) if err.class == SpeechErrorClass::Cancelled => events_tx
                        .blocking_send(SynthesisEvent::Cancelled {
                            request_id: worker_id.clone(),
                            usage: SpeechUsage::default(),
                        }),
                    Err(err) => events_tx.blocking_send(SynthesisEvent::Failed {
                        request_id: worker_id.clone(),
                        error: err.clone(),
                    }),
                };
                let _ = final_tx.send(result);
                if let Ok(mut map) = active.lock() {
                    map.remove(&worker_id);
                }
                Ok(())
            })
            .map_err(|e| unavailable(&request_id, "qwen_worker_unavailable", &e.to_string()))?;
        Ok(SynthesisTicket::new(
            request_id,
            events_rx,
            final_rx,
            Arc::new(Cancellation {
                active: Arc::clone(&self.active),
            }),
        ))
    }

    fn cancel(&self, request_id: &SpeechRequestId) -> usize {
        Cancellation {
            active: Arc::clone(&self.active),
        }
        .cancel(request_id)
    }

    async fn shutdown(&self) -> Result<(), SpeechError> {
        if let Ok(map) = self.active.lock() {
            for flag in map.values() {
                flag.store(true, Ordering::Release);
            }
        }
        self.tasks.begin_shutdown().map_err(|e| {
            unavailable(
                &SpeechRequestId("qwen-shutdown".into()),
                "qwen_shutdown_failed",
                &e.to_string(),
            )
        })?;
        self.tasks.wait_for_idle().await.map_err(|e| {
            unavailable(
                &SpeechRequestId("qwen-shutdown".into()),
                "qwen_shutdown_failed",
                &e.to_string(),
            )
        })
    }
}

fn validate_request(
    request: &SynthesisRequest,
    descriptor: &SpeechBackendDescriptor,
) -> Result<(), SpeechError> {
    request.validate()?;
    if !descriptor.readiness.is_ready() {
        return Err(unavailable(
            &request.context.request_id,
            "qwen_assets_unavailable",
            "Qwen3 TTS helper or model is unavailable",
        ));
    }
    if request.stream
        || request.output != AudioOutputFormat::Wav
        || !matches!(request.input, SynthesisInput::Text { .. })
        || request.alignment != speech_native_types::AlignmentGranularity::None
    {
        return Err(error(
            &request.context.request_id,
            "qwen_request_unsupported",
            SpeechErrorClass::Capability,
            false,
            "Qwen3 TTS accepts plain text and returns one complete WAV",
        ));
    }
    if let SpeechRouteSelector::ExactBackend {
        backend_id,
        model_id,
        voice_id,
    } = &request.context.route
        && (backend_id != QWEN_BACKEND_ID
            || model_id.as_deref() != Some(QWEN_MODEL_ID)
            || voice_id.as_deref() != Some(QWEN_VOICE_ID))
    {
        return Err(error(
            &request.context.request_id,
            "qwen_route_mismatch",
            SpeechErrorClass::Capability,
            false,
            "Qwen3 TTS request selected a different route",
        ));
    }
    if !matches!(&request.voice, VoiceSelector::Exact { voice_id } if voice_id == QWEN_VOICE_ID) {
        return Err(SpeechError::invalid_request(
            &request.context.request_id,
            "qwen_voice_mismatch",
            "Qwen3 TTS requires the admitted designed voice",
        ));
    }
    Ok(())
}

fn synthesize_complete(
    request: &SynthesisRequest,
    config: &QwenBackendConfig,
    route: &SpeechResolvedRoute,
    cancelled: &AtomicBool,
    events: &mpsc::Sender<SynthesisEvent>,
) -> Result<SynthesisResponse, SpeechError> {
    let request_id = &request.context.request_id;
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error(request_id));
    }
    events
        .blocking_send(SynthesisEvent::Started {
            request_id: request_id.clone(),
            route: route.clone(),
        })
        .map_err(|_| cancelled_error(request_id))?;
    let started = Instant::now();
    let output = tempfile::Builder::new()
        .prefix("mom-qwen-")
        .suffix(".wav")
        .tempfile()
        .map_err(|e| unavailable(request_id, "qwen_output_unavailable", &e.to_string()))?;
    let text = match &request.input {
        SynthesisInput::Text { text } => text,
        SynthesisInput::Ssml { .. } => unreachable!(),
    };
    let status = Command::new(&config.helper)
        .arg(&config.model_dir)
        .arg(output.path())
        .arg(VOICE_DESCRIPTION)
        .arg(text)
        .status()
        .map_err(|e| unavailable(request_id, "qwen_helper_launch_failed", &e.to_string()))?;
    if !status.success() {
        return Err(unavailable(
            request_id,
            "qwen_synthesis_failed",
            "The local Qwen3 TTS helper did not complete synthesis",
        ));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error(request_id));
    }
    let audio = fs::read(output.path())
        .map_err(|e| unavailable(request_id, "qwen_output_read_failed", &e.to_string()))?;
    let duration_ms = wav_duration_ms(&audio).ok_or_else(|| {
        unavailable(
            request_id,
            "qwen_wav_invalid",
            "Qwen3 TTS returned an invalid WAV",
        )
    })?;
    let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(SynthesisResponse {
        request_id: request_id.clone(),
        route: route.clone(),
        output: SynthesisOutput::Complete {
            audio,
            format: AudioOutputFormat::Wav,
        },
        duration_ms: Some(duration_ms),
        alignments: Vec::new(),
        usage: SpeechUsage {
            output_audio_ms: Some(duration_ms),
            input_characters: Some(u64::try_from(text.chars().count()).unwrap_or(u64::MAX)),
            total_ms: Some(total_ms),
            provenance: UsageProvenance::Exact,
            real_local_inference: true,
            ..SpeechUsage::default()
        },
    })
}

fn wav_duration_ms(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let rate = u32::from_le_bytes(bytes[24..28].try_into().ok()?);
    let channels = u16::from_le_bytes(bytes[22..24].try_into().ok()?);
    let bits = u16::from_le_bytes(bytes[34..36].try_into().ok()?);
    if rate != 24_000 || channels != 1 || bits != 16 {
        return None;
    }
    let data_len = u32::from_le_bytes(bytes[40..44].try_into().ok()?) as u64;
    Some(data_len.saturating_mul(1000) / (u64::from(rate) * 2))
}

fn route() -> SpeechResolvedRoute {
    SpeechResolvedRoute {
        backend_id: QWEN_BACKEND_ID.into(),
        model_id: Some(QWEN_MODEL_ID.into()),
        voice_id: Some(QWEN_VOICE_ID.into()),
        backend_kind: SpeechBackendKind::EmbeddedModel,
        network: NetworkBehavior::Never,
    }
}
fn descriptor(ready: bool, config: &QwenBackendConfig) -> SpeechBackendDescriptor {
    let readiness = if ready {
        SpeechBackendReadiness::Ready
    } else {
        SpeechBackendReadiness::AssetInstallRequired { assets: Vec::new() }
    };
    let availability = if ready {
        CapabilityAvailability::Available
    } else {
        CapabilityAvailability::AssetInstallRequired
    };
    SpeechBackendDescriptor {
        id: QWEN_BACKEND_ID.into(),
        display_name: "Qwen3 TTS MLX".into(),
        kind: SpeechBackendKind::EmbeddedModel,
        readiness,
        capabilities: vec![SpeechCapability {
            id: "qwen3-tts.mlx-swift.synthesis".into(),
            backend_id: QWEN_BACKEND_ID.into(),
            model_id: Some(QWEN_MODEL_ID.into()),
            operation: SpeechOperationCapability::Synthesis(SynthesisCapabilities {
                streaming_audio: false,
                ssml: false,
                word_alignment: false,
                phoneme_alignment: false,
                pause_resume: false,
                voice_selection: true,
                returned_audio: vec![AudioOutputKind::Wav],
            }),
            availability,
            network: NetworkBehavior::Never,
            languages: vec!["en-US".into()],
            limits: SpeechCapabilityLimits {
                max_input_characters: Some(MAX_INPUT_CHARACTERS),
                max_concurrent_requests: Some(1),
                ..SpeechCapabilityLimits::default()
            },
            evidence: if ready {
                vec![CapabilityEvidence {
                    source_id: "mom-qwen3-tts-helper".into(),
                    source_version: Some(env!("CARGO_PKG_VERSION").into()),
                    kind: EvidenceKind::SystemInventory,
                    outcome: EvidenceOutcome::Confirmed,
                    observed_at_unix_ms: now_ms(),
                    detail: format!(
                        "Bundled helper and local model config admitted at {}",
                        config.model_dir.display()
                    ),
                }]
            } else {
                Vec::new()
            },
        }],
        models: vec![SpeechModelDescriptor {
            id: QWEN_MODEL_ID.into(),
            display_name: "Qwen3 TTS 1.7B VoiceDesign 4-bit".into(),
            family: "qwen3-tts".into(),
            languages: vec!["en-US".into()],
            resident: false,
            estimated_memory_bytes: Some(2_000_000_000),
            content_hash: Some(QWEN_MODEL_REVISION.into()),
        }],
        voices: vec![VoiceDescriptor {
            id: QWEN_VOICE_ID.into(),
            name: "Warm clinician".into(),
            language: "en-US".into(),
            gender: Some(VoiceGender::Female),
            quality: Some(VoiceQuality::Premium),
            expected_latency: Some(LatencyClass::High),
            network: NetworkBehavior::Never,
            installed: ready,
        }],
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
fn unavailable(id: &SpeechRequestId, code: &str, detail: &str) -> SpeechError {
    let mut e = SpeechError::unavailable(id, code, detail);
    e.backend_id = Some(QWEN_BACKEND_ID.into());
    e
}
fn error(
    id: &SpeechRequestId,
    code: &str,
    class: SpeechErrorClass,
    retryable: bool,
    detail: &str,
) -> SpeechError {
    SpeechError {
        code: code.into(),
        class,
        retryable,
        request_id: id.clone(),
        backend_id: Some(QWEN_BACKEND_ID.into()),
        safe_detail: detail.into(),
    }
}
fn cancelled_error(id: &SpeechRequestId) -> SpeechError {
    error(
        id,
        "qwen_synthesis_cancelled",
        SpeechErrorClass::Cancelled,
        false,
        "Qwen3 TTS synthesis was cancelled",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use speech_native_types::{
        AlignmentGranularity, SpeechDeadlinePolicy, SpeechPrivacyPolicy, SpeechRequestContext,
        SpeechRouteProfile, SpeechRoutingPolicy,
    };
    #[test]
    fn wav_duration_rejects_wrong_shape() {
        assert_eq!(wav_duration_ms(b"not wav"), None);
    }

    #[tokio::test]
    #[ignore = "requires the exact local Qwen model and compiled MLX helper"]
    async fn real_local_qwen_synthesis_returns_bound_wav() {
        let helper = PathBuf::from(std::env::var_os("MOM_QWEN3_TTS_HELPER").expect("helper"));
        let model_dir = PathBuf::from(std::env::var_os("MOM_QWEN3_TTS_MODEL").expect("model"));
        let backend = QwenSpeechBackend::discover(QwenBackendConfig { helper, model_dir });
        assert!(backend.readiness().is_ready());
        let request_id = SpeechRequestId("qwen-real-smoke".into());
        let ticket = backend
            .synthesize(SynthesisRequest {
                context: SpeechRequestContext {
                    request_id: request_id.clone(),
                    client_id: "qwen-backend-smoke".into(),
                    route: SpeechRouteSelector::ExactBackend {
                        backend_id: QWEN_BACKEND_ID.into(),
                        model_id: Some(QWEN_MODEL_ID.into()),
                        voice_id: Some(QWEN_VOICE_ID.into()),
                    },
                    routing: SpeechRoutingPolicy {
                        privacy: SpeechPrivacyPolicy::LocalOnly,
                        profile: SpeechRouteProfile::ConsistentLocal,
                        allow_asset_download: false,
                        allow_fallback_before_output: false,
                    },
                    deadline: SpeechDeadlinePolicy::default(),
                },
                input: SynthesisInput::Text {
                    text: "Hello. Let us think this through together.".into(),
                },
                voice: VoiceSelector::Exact {
                    voice_id: QWEN_VOICE_ID.into(),
                },
                language: Some("en-US".into()),
                rate: 1.0,
                pitch: 1.0,
                volume: 1.0,
                output: AudioOutputFormat::Wav,
                alignment: AlignmentGranularity::None,
                stream: false,
            })
            .await
            .expect("ticket");
        let response = ticket.final_response().await.expect("response");
        assert_eq!(response.route, route());
        assert!(response.usage.real_local_inference);
        let SynthesisOutput::Complete { audio, format } = response.output else {
            panic!("complete output required");
        };
        assert_eq!(format, AudioOutputFormat::Wav);
        assert!(wav_duration_ms(&audio).is_some_and(|duration| duration > 500));
        backend.shutdown().await.expect("joined shutdown");
    }
}
