use async_trait::async_trait;
use mom_llama_runtime::{
    AttachmentPreviewAnchor, AttachmentTranscriptionInput, Blocker, CommandResult, MessageRole,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use speech_native_backend_parakeet::{
    PARAKEET_BACKEND_ID, PARAKEET_DEFERRED_LOAD_EVIDENCE_SOURCE_ID, PARAKEET_MODEL_CONTENT_SHA256,
    PARAKEET_MODEL_ID, ParakeetBackendConfig, ParakeetSpeechBackend,
};
use speech_native_host::{SpeechHost, SpeechHostError};
use speech_native_types::{
    AcceptedAudio, AlignmentGranularity, AudioInput, AudioOutputFormat, AudioOutputKind,
    DiarizationPolicy, EncodedAudioFormat, NetworkBehavior, PlatformTarget,
    SpeechBackendDescriptor, SpeechBackendReadiness, SpeechDeadlinePolicy, SpeechError,
    SpeechErrorClass, SpeechOperationCapability, SpeechPrivacyPolicy, SpeechRequestContext,
    SpeechRequestId, SpeechRouteProfile, SpeechRouteSelector, SpeechRoutingPolicy, SynthesisInput,
    SynthesisOutput, SynthesisRequest, SynthesisResponse, TimestampGranularity, TranscriptionInput,
    TranscriptionRequest, TranscriptionResponse, TranscriptionTask, VoiceSelector,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};

const APPLE_BACKEND_ID: &str = "apple.av-speech";
const MOM_SPEECH_CLIENT_ID: &str = "mom-llama";
const MAX_ACTIVE_SPEECH_OPERATIONS: usize = 4;
const MAX_PENDING_CANCELS: usize = 32;
const MAX_PLAYBACKS: usize = 2;
const MAX_COMPLETE_WAV_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRANSCRIPT_CHARACTERS: usize = 200_000;

/// Apple's wrapper produces a complete WAV in one synchronous native call.
/// Stop suppresses publication and playback immediately, but cannot pre-empt
/// that inner call; shutdown therefore joins it before reporting completion.
pub const APPLE_SYNTHESIS_IS_NON_PREEMPTIVE: bool = true;

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
}

#[derive(Debug, Clone)]
struct ActiveOperation {
    request_id: SpeechRequestId,
    cancelled: bool,
}

#[derive(Debug, Clone)]
struct PlaybackAudio {
    binding: ReadAloudBinding,
    bytes: Vec<u8>,
}

#[derive(Debug)]
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
}

#[derive(Debug, Clone)]
struct AppleSelection {
    descriptor_sha256: String,
    descriptor: SpeechBackendDescriptor,
    voice_id: String,
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
    pub voice_id: String,
    pub request_id: String,
    pub wav_sha256: String,
    pub wav_bytes: u64,
    pub duration_ms: Option<u64>,
    pub network: NetworkBehavior,
    pub apple_inner_call_non_preemptive: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReadAloudOutput {
    pub playback_id: String,
    pub binding: ReadAloudBinding,
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
    /// The renderer may offer this as a separate normal composer edit. This
    /// command never changes a draft and never dispatches a message.
    pub insertion_requires_explicit_normal_edit: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SpeechStopOutput {
    pub operation: Option<String>,
    pub playback: Option<String>,
    pub cancellation_requested: bool,
    pub playback_released: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SpeechShutdown {
    pub host_joined: bool,
    pub active_operation_count: usize,
    pub retained_playback_count: usize,
    pub apple_inner_call_non_preemptive: bool,
}

#[async_trait]
trait SpeechExecutor: Send + Sync {
    fn descriptors(&self) -> Result<Vec<SpeechBackendDescriptor>, SpeechHostError>;
    async fn synthesize(&self, request: SynthesisRequest) -> Result<SynthesisResponse, String>;
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResponse, String>;
    fn cancel(&self, request_id: &SpeechRequestId) -> usize;
    fn quiesce(&self) -> Result<(), SpeechHostError>;
    async fn shutdown(&self) -> Result<(), SpeechHostError>;
}

struct HostSpeechExecutor {
    host: Arc<SpeechHost>,
}

#[async_trait]
impl SpeechExecutor for HostSpeechExecutor {
    fn descriptors(&self) -> Result<Vec<SpeechBackendDescriptor>, SpeechHostError> {
        self.host.descriptors()
    }

    async fn synthesize(&self, request: SynthesisRequest) -> Result<SynthesisResponse, String> {
        self.host
            .synthesize(request)
            .await
            .map_err(|error| error.to_string())?
            .final_response()
            .await
            .map_err(|error| speech_error_message(&error))
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

pub struct MomSpeech {
    executor: Arc<dyn SpeechExecutor>,
    state: Mutex<SpeechState>,
}

impl MomSpeech {
    pub async fn discover(data_dir: &Path) -> Result<Arc<Self>, String> {
        let host = Arc::new(SpeechHost::new(PlatformTarget::current()));
        let parakeet = ParakeetSpeechBackend::discover(ParakeetBackendConfig {
            model_dir: None,
            managed_model_root: Some(data_dir.join("speech-models")),
        })
        .await;
        host.register_backend(Arc::new(parakeet))
            .map_err(|error| format!("Parakeet registration failed: {error}"))?;

        #[cfg(target_os = "macos")]
        if let Ok(apple) =
            speech_native_platform::apple_backend::AppleSpeechBackend::discover().await
        {
            host.register_backend(Arc::new(apple))
                .map_err(|error| format!("Apple speech registration failed: {error}"))?;
        }

        Ok(Arc::new(Self {
            executor: Arc::new(HostSpeechExecutor { host }),
            state: Mutex::new(SpeechState::default()),
        }))
    }

    #[cfg(test)]
    pub fn empty_for_tests() -> Arc<Self> {
        let host = Arc::new(SpeechHost::new(PlatformTarget::current()));
        Arc::new(Self {
            executor: Arc::new(HostSpeechExecutor { host }),
            state: Mutex::new(SpeechState::default()),
        })
    }

    pub async fn read_aloud(
        &self,
        conversation_id: &str,
        message_id: &str,
        operation: &str,
    ) -> anyhow::Result<CommandResult<ReadAloudOutput>> {
        validate_operation_token(operation)?;
        let target = match assistant_message_target(conversation_id, message_id)? {
            Ok(target) => target,
            Err(blocker) => return Ok(blocked("mom_llama.speech_read_aloud", blocker)),
        };
        let selection =
            match select_apple(&self.executor.descriptors()?, target.text.chars().count()) {
                Ok(selection) => selection,
                Err(blocker) => return Ok(blocked("mom_llama.speech_read_aloud", blocker)),
            };
        let request_id = SpeechRequestId(format!("mom-read-aloud-{operation}"));
        if let Err(blocker) =
            self.begin_operation(operation, request_id.clone(), OperationKind::ReadAloud)
        {
            return Ok(blocked("mom_llama.speech_read_aloud", blocker));
        }
        let response = self
            .executor
            .synthesize(SynthesisRequest {
                context: request_context(
                    request_id.clone(),
                    SpeechRouteSelector::ExactBackend {
                        backend_id: APPLE_BACKEND_ID.to_string(),
                        model_id: None,
                        voice_id: Some(selection.voice_id.clone()),
                    },
                    180_000,
                ),
                input: SynthesisInput::Text {
                    text: target.text.clone(),
                },
                voice: VoiceSelector::Exact {
                    voice_id: selection.voice_id.clone(),
                },
                language: None,
                rate: 1.0,
                pitch: 1.0,
                volume: 1.0,
                output: AudioOutputFormat::Wav,
                alignment: AlignmentGranularity::None,
                stream: false,
            })
            .await;
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
        let current = assistant_message_target(conversation_id, message_id)?;
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
        let (audio, duration_ms) =
            match validate_apple_response(response, &request_id, &selection.voice_id) {
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
            backend_id: selection.descriptor.id,
            backend_descriptor_sha256: selection.descriptor_sha256,
            voice_id: selection.voice_id,
            request_id: request_id.0.clone(),
            wav_sha256: sha256_hex(&audio),
            wav_bytes: u64::try_from(audio.len()).unwrap_or(u64::MAX),
            duration_ms,
            network: NetworkBehavior::Never,
            apple_inner_call_non_preemptive: APPLE_SYNTHESIS_IS_NON_PREEMPTIVE,
        };
        let playback_id =
            match self.publish_playback(operation, &request_id, binding.clone(), audio) {
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
        let _deferred_selection = match select_parakeet(&self.executor.descriptors()?) {
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
        let current = mom_llama_runtime::attachment_transcription_input(conversation_id, anchor)?;
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
        let selection = match select_resident_parakeet(&self.executor.descriptors()?) {
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

    pub fn playback_bytes(
        &self,
        playback_id: &str,
        message_id: &str,
        text_sha256: &str,
        backend_descriptor_sha256: &str,
    ) -> Result<Vec<u8>, String> {
        self.playback_bytes_with_target(
            playback_id,
            message_id,
            text_sha256,
            backend_descriptor_sha256,
            |conversation_id, message_id| {
                assistant_message_target(conversation_id, message_id)
                    .map_err(|error| error.to_string())?
                    .map(|target| target.text_sha256)
                    .map_err(|blocker| format!("{}: {}", blocker.code, blocker.message))
            },
        )
    }

    fn playback_bytes_with_target<F>(
        &self,
        playback_id: &str,
        message_id: &str,
        text_sha256: &str,
        backend_descriptor_sha256: &str,
        current_text_sha256: F,
    ) -> Result<Vec<u8>, String>
    where
        F: FnOnce(&str, &str) -> Result<String, String>,
    {
        let binding = {
            let state = self
                .state
                .lock()
                .map_err(|_| "Mom speech playback state is unavailable".to_string())?;
            if state.phase != SpeechPhase::Running {
                return Err("Mom speech playback is closing".to_string());
            }
            let playback = state.playback.get(playback_id).ok_or_else(|| {
                "The requested speech playback is no longer available".to_string()
            })?;
            if playback.binding.message_id != message_id
                || playback.binding.text_sha256 != text_sha256
                || playback.binding.backend_descriptor_sha256 != backend_descriptor_sha256
            {
                return Err("Speech playback identity changed before byte publication".to_string());
            }
            playback.binding.clone()
        };

        let current_hash = current_text_sha256(&binding.conversation_id, &binding.message_id)?;
        if current_hash != binding.text_sha256 {
            return Err(
                "speech_read_aloud_stale: The exact assistant message changed before WAV byte publication."
                    .to_string(),
            );
        }

        let state = self
            .state
            .lock()
            .map_err(|_| "Mom speech playback state is unavailable".to_string())?;
        if state.phase != SpeechPhase::Running {
            return Err("Mom speech playback is closing".to_string());
        }
        let playback = state
            .playback
            .get(playback_id)
            .ok_or_else(|| "The requested speech playback is no longer available".to_string())?;
        if playback.binding != binding {
            return Err("Speech playback identity changed before byte publication".to_string());
        }
        Ok(playback.bytes.clone())
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

    pub fn begin_quiesce(&self) {
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
            apple_inner_call_non_preemptive: APPLE_SYNTHESIS_IS_NON_PREEMPTIVE,
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
        if state
            .active
            .get(operation)
            .is_some_and(|active| active.request_id == *request_id)
        {
            state.active.remove(operation);
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
        Ok(())
    }

    fn publish_playback(
        &self,
        operation: &str,
        request_id: &SpeechRequestId,
        binding: ReadAloudBinding,
        bytes: Vec<u8>,
    ) -> Result<String, Blocker> {
        if binding.request_id != request_id.0 {
            self.finish_operation(operation, request_id);
            return Err(speech_provenance_mismatch(
                "Apple playback binding did not match the admitted request.",
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
        state
            .playback
            .insert(playback_id.clone(), PlaybackAudio { binding, bytes });
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
    }))
}

fn select_apple(
    descriptors: &[SpeechBackendDescriptor],
    input_characters: usize,
) -> Result<AppleSelection, Blocker> {
    let Some(descriptor) = descriptors
        .iter()
        .find(|descriptor| descriptor.id == APPLE_BACKEND_ID)
    else {
        return Err(speech_unavailable(
            "Apple complete-WAV synthesis is unavailable.",
        ));
    };
    if !matches!(descriptor.readiness, SpeechBackendReadiness::Ready) {
        return Err(speech_unavailable("Apple speech synthesis is not ready."));
    }
    let capability = descriptor.capabilities.iter().find(|capability| {
        capability.eligible_for_local_only()
            && matches!(
                &capability.operation,
                SpeechOperationCapability::Synthesis(synthesis)
                    if !synthesis.streaming_audio
                        && synthesis.voice_selection
                        && synthesis.returned_audio == [AudioOutputKind::Wav]
            )
    });
    let Some(capability) = capability else {
        return Err(speech_unavailable(
            "Apple speech does not advertise executable local complete-WAV synthesis.",
        ));
    };
    if capability
        .limits
        .max_input_characters
        .is_some_and(|limit| u64::try_from(input_characters).unwrap_or(u64::MAX) > limit)
    {
        return Err(Blocker::new(
            "speech_read_aloud_too_long",
            "The exact assistant message exceeds the Apple synthesis input limit.",
            Vec::new(),
        ));
    }
    let Some(voice) = descriptor
        .voices
        .iter()
        .filter(|voice| voice.installed && voice.network == NetworkBehavior::Never)
        .min_by(|left, right| left.id.cmp(&right.id))
    else {
        return Err(speech_unavailable(
            "No installed never-network Apple voice is available.",
        ));
    };
    Ok(AppleSelection {
        descriptor_sha256: descriptor_sha256(descriptor)?,
        descriptor: descriptor.clone(),
        voice_id: voice.id.clone(),
    })
}

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

fn validate_apple_response(
    response: SynthesisResponse,
    request_id: &SpeechRequestId,
    voice_id: &str,
) -> Result<(Vec<u8>, Option<u64>), Blocker> {
    if response.request_id != *request_id
        || response.route.backend_id != APPLE_BACKEND_ID
        || response.route.model_id.is_some()
        || response.route.voice_id.as_deref() != Some(voice_id)
        || response.route.backend_kind != speech_native_types::SpeechBackendKind::PlatformOnDevice
        || response.route.network != NetworkBehavior::Never
    {
        return Err(speech_provenance_mismatch(
            "Apple response route did not match the admitted message and voice binding.",
        ));
    }
    let SynthesisOutput::Complete { audio, format } = response.output else {
        return Err(speech_provenance_mismatch(
            "Apple synthesis returned streaming state instead of one complete WAV.",
        ));
    };
    if format != AudioOutputFormat::Wav || audio.is_empty() || audio.len() > MAX_COMPLETE_WAV_BYTES
    {
        return Err(Blocker::new(
            "speech_wav_invalid",
            "Apple synthesis did not return a non-empty bounded complete WAV.",
            Vec::new(),
        ));
    }
    Ok((audio, response.duration_ms))
}

fn validate_parakeet_response(
    response: &TranscriptionResponse,
    request_id: &SpeechRequestId,
) -> Result<(), Blocker> {
    if response.request_id != *request_id
        || response.route.backend_id != PARAKEET_BACKEND_ID
        || response.route.model_id.as_deref() != Some(PARAKEET_MODEL_ID)
        || response.route.voice_id.is_some()
        || response.route.backend_kind != speech_native_types::SpeechBackendKind::EmbeddedModel
        || response.route.network != NetworkBehavior::Never
        || !response.usage.real_local_inference
    {
        return Err(speech_provenance_mismatch(
            "Parakeet response provenance did not match the exact admitted local model route.",
        ));
    }
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
    use speech_native_types::{
        CapabilityAvailability, CapabilityEvidence, EvidenceKind, EvidenceOutcome,
        SpeechBackendKind, SpeechCapability, SpeechCapabilityLimits, SpeechModelDescriptor,
        SynthesisCapabilities, TranscriptionCapabilities, VoiceDescriptor,
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

    fn apple_descriptor() -> SpeechBackendDescriptor {
        SpeechBackendDescriptor {
            id: APPLE_BACKEND_ID.to_string(),
            display_name: "Apple".to_string(),
            kind: SpeechBackendKind::PlatformOnDevice,
            readiness: SpeechBackendReadiness::Ready,
            capabilities: vec![SpeechCapability {
                id: "apple.av-speech.synthesis".to_string(),
                backend_id: APPLE_BACKEND_ID.to_string(),
                model_id: None,
                operation: SpeechOperationCapability::Synthesis(SynthesisCapabilities {
                    streaming_audio: false,
                    voice_selection: true,
                    returned_audio: vec![AudioOutputKind::Wav],
                    ..SynthesisCapabilities::default()
                }),
                availability: CapabilityAvailability::Available,
                network: NetworkBehavior::Never,
                languages: vec!["en-US".to_string()],
                limits: SpeechCapabilityLimits {
                    max_input_characters: Some(10_000),
                    max_concurrent_requests: Some(1),
                    ..SpeechCapabilityLimits::default()
                },
                evidence: runtime_evidence(),
            }],
            models: Vec::new(),
            voices: vec![VoiceDescriptor {
                id: "voice.local".to_string(),
                name: "Local".to_string(),
                language: "en-US".to_string(),
                gender: None,
                quality: None,
                expected_latency: None,
                network: NetworkBehavior::Never,
                installed: true,
            }],
        }
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
        let apple = select_apple(&[apple_descriptor()], 12).expect("Apple route");
        assert_eq!(apple.voice_id, "voice.local");
        assert!(apple.descriptor.capabilities.iter().all(|capability| {
            capability.network == NetworkBehavior::Never
                && matches!(
                    &capability.operation,
                    SpeechOperationCapability::Synthesis(synthesis)
                        if !synthesis.streaming_audio
                            && synthesis.returned_audio == [AudioOutputKind::Wav]
                )
        }));
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
                        backend_id: APPLE_BACKEND_ID.to_string(),
                        backend_descriptor_sha256: "descriptor".to_string(),
                        voice_id: "voice".to_string(),
                        request_id: closing_request.0.clone(),
                        wav_sha256: "wav".to_string(),
                        wav_bytes: 4,
                        duration_ms: None,
                        network: NetworkBehavior::Never,
                        apple_inner_call_non_preemptive: true,
                    },
                    b"RIFF".to_vec(),
                )
                .is_err()
        );
        let shutdown = speech.shutdown().await.expect("joined empty host");
        assert!(shutdown.host_joined);
        assert_eq!(shutdown.active_operation_count, 0);
        assert_eq!(shutdown.retained_playback_count, 0);
        assert!(shutdown.apple_inner_call_non_preemptive);
    }

    #[test]
    fn stale_and_provenance_checks_reject_mismatched_results() {
        let request_id = SpeechRequestId("read".to_string());
        let response = SynthesisResponse {
            request_id: request_id.clone(),
            route: speech_native_types::SpeechResolvedRoute {
                backend_id: APPLE_BACKEND_ID.to_string(),
                model_id: None,
                voice_id: Some("other-voice".to_string()),
                backend_kind: SpeechBackendKind::PlatformOnDevice,
                network: NetworkBehavior::Never,
            },
            output: SynthesisOutput::Complete {
                audio: b"RIFF".to_vec(),
                format: AudioOutputFormat::Wav,
            },
            duration_ms: None,
            alignments: Vec::new(),
            usage: Default::default(),
        };
        assert_eq!(
            validate_apple_response(response, &request_id, "voice.local")
                .expect_err("voice mismatch")
                .code,
            "speech_provenance_mismatch"
        );
        assert_ne!(sha256_hex(b"before"), sha256_hex(b"after"));
    }

    #[test]
    fn raw_wav_publication_revalidates_the_current_message_after_storage_lookup() {
        let speech = MomSpeech::empty_for_tests();
        let binding = ReadAloudBinding {
            conversation_id: "chat-current".to_string(),
            message_id: "message-current".to_string(),
            text_sha256: sha256_hex(b"before"),
            backend_id: APPLE_BACKEND_ID.to_string(),
            backend_descriptor_sha256: "descriptor-current".to_string(),
            voice_id: "voice.local".to_string(),
            request_id: "request-current".to_string(),
            wav_sha256: sha256_hex(b"RIFF"),
            wav_bytes: 4,
            duration_ms: None,
            network: NetworkBehavior::Never,
            apple_inner_call_non_preemptive: true,
        };
        speech.state.lock().expect("state").playback.insert(
            "playback-current".to_string(),
            PlaybackAudio {
                binding: binding.clone(),
                bytes: b"RIFF".to_vec(),
            },
        );

        let stale = speech.playback_bytes_with_target(
            "playback-current",
            &binding.message_id,
            &binding.text_sha256,
            &binding.backend_descriptor_sha256,
            |conversation_id, message_id| {
                assert_eq!(conversation_id, "chat-current");
                assert_eq!(message_id, "message-current");
                Ok(sha256_hex(b"after"))
            },
        );
        assert!(
            stale
                .expect_err("edited message must suppress old WAV")
                .contains("speech_read_aloud_stale")
        );
        assert_eq!(
            speech
                .playback_bytes_with_target(
                    "playback-current",
                    &binding.message_id,
                    &binding.text_sha256,
                    &binding.backend_descriptor_sha256,
                    |_, _| Ok(binding.text_sha256.clone()),
                )
                .expect("unchanged exact message"),
            b"RIFF"
        );
    }

    #[test]
    fn descriptor_hash_binds_request_to_exact_inventory() {
        let original = apple_descriptor();
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
}
