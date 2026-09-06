use std::collections::BTreeMap;
use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use serde::{Deserialize, Serialize};
use speech_native_backend_parakeet::{
    PARAKEET_BACKEND_ID, PARAKEET_MODEL_ID, ParakeetBackendConfig, ParakeetSpeechBackend,
};
use speech_native_host::{SpeechHost, SpeechHostError, SpeechHostStatus};
use speech_native_types::{
    AudioInput, DiarizationPolicy, EncodedAudioFormat, SpeechBackendReadiness,
    SpeechDeadlinePolicy, SpeechPrivacyPolicy, SpeechRequestContext, SpeechRequestId,
    SpeechRouteProfile, SpeechRouteSelector, SpeechRoutingPolicy, TimestampGranularity,
    TranscriptionInput, TranscriptionRequest, TranscriptionTask,
};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use tokio::task::JoinHandle;

use crate::microphone_capture::{MicrophoneCaptureError, NativeMicrophoneCapture};

const MAX_INPUT_SESSIONS: usize = 32;
const MAX_WAV_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRANSCRIPT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpeechInputTarget {
    Manuscript,
    Context,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpeechInputPhase {
    Transcribing,
    CancelRequested,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpeechRecordingPhase {
    Recording,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct SpeechRecordingSnapshot {
    pub(crate) recording_id: String,
    pub(crate) project_id: String,
    pub(crate) session_id: String,
    pub(crate) document_id: String,
    pub(crate) target: SpeechInputTarget,
    pub(crate) phase: SpeechRecordingPhase,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveRecording {
    recording_id: String,
    project_id: String,
    session_id: String,
    document_id: String,
    target: SpeechInputTarget,
}

impl ActiveRecording {
    fn snapshot(&self, phase: SpeechRecordingPhase) -> SpeechRecordingSnapshot {
        SpeechRecordingSnapshot {
            recording_id: self.recording_id.clone(),
            project_id: self.project_id.clone(),
            session_id: self.session_id.clone(),
            document_id: self.document_id.clone(),
            target: self.target,
            phase,
        }
    }
}

impl SpeechInputPhase {
    const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct SpeechInputSnapshot {
    pub(crate) request_id: String,
    pub(crate) project_id: String,
    pub(crate) session_id: String,
    pub(crate) document_id: String,
    pub(crate) target: SpeechInputTarget,
    pub(crate) phase: SpeechInputPhase,
    pub(crate) transcript: String,
    pub(crate) error_code: Option<String>,
    pub(crate) error_message: Option<String>,
}

#[derive(Debug, Error)]
pub(crate) enum SpeechInputError {
    #[error("speech service failed: {0}")]
    Host(#[from] SpeechHostError),
    #[error("speech input state is unavailable")]
    State,
    #[error("too many speech input sessions are retained")]
    Capacity,
    #[error("the speech input request does not exist")]
    NotFound,
    #[error("the speech input request does not belong to the active project session")]
    Scope,
    #[error("the recording is empty, is not a WAV file, or exceeds Loom's 64 MB limit")]
    InvalidWav,
    #[error("microphone capture failed: {0}")]
    Microphone(#[from] MicrophoneCaptureError),
    #[error("there is no active microphone recording")]
    RecordingNotFound,
    #[error("the microphone recording does not belong to the active project session")]
    RecordingScope,
    #[error("speech backend failed: {0}")]
    Backend(#[from] speech_native_types::SpeechError),
    #[error("local speech recognition is unavailable: {0}")]
    Unavailable(String),
    #[error("a speech input worker failed to join: {0}")]
    Task(String),
}

// The owner remains synchronous: all device operations run on blocking workers.
// Keeping this boundary explicit also permits deterministic lifecycle tests.
trait MicrophoneCapture: std::fmt::Debug + Send + Sync {
    fn start(&self, id: String) -> Result<(), MicrophoneCaptureError>;
    fn stop(&self, id: String) -> Result<Vec<u8>, MicrophoneCaptureError>;
    fn cancel(&self, id: String) -> Result<(), MicrophoneCaptureError>;
    fn shutdown(&self) -> Result<(), MicrophoneCaptureError>;
}

impl MicrophoneCapture for NativeMicrophoneCapture {
    fn start(&self, id: String) -> Result<(), MicrophoneCaptureError> {
        Self::start(self, id)
    }
    fn stop(&self, id: String) -> Result<Vec<u8>, MicrophoneCaptureError> {
        Self::stop(self, id)
    }
    fn cancel(&self, id: String) -> Result<(), MicrophoneCaptureError> {
        Self::cancel(self, id)
    }
    fn shutdown(&self) -> Result<(), MicrophoneCaptureError> {
        Self::shutdown(self)
    }
}

#[derive(Debug)]
pub(crate) struct SpeechInputService {
    host: OnceCell<Arc<SpeechHost>>,
    sessions: Arc<Mutex<BTreeMap<SpeechRequestId, Arc<AsyncMutex<SpeechInputSnapshot>>>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    microphone: Arc<dyn MicrophoneCapture>,
    recording: Mutex<Option<ActiveRecording>>,
    recording_lifecycle: AsyncMutex<()>,
    active_scope: Mutex<Option<(String, String)>>,
    managed_model_root: Option<PathBuf>,
}

impl SpeechInputService {
    pub(crate) fn new(app_local_data_root: Option<PathBuf>) -> Self {
        Self {
            host: OnceCell::new(),
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            tasks: Mutex::new(Vec::new()),
            microphone: Arc::new(NativeMicrophoneCapture::new()),
            recording: Mutex::new(None),
            recording_lifecycle: AsyncMutex::new(()),
            active_scope: Mutex::new(None),
            managed_model_root: app_local_data_root.map(|root| root.join("speech-models")),
        }
    }

    pub(crate) fn bind_scope(
        &self,
        project_id: String,
        session_id: String,
    ) -> Result<(), SpeechInputError> {
        *self
            .active_scope
            .lock()
            .map_err(|_| SpeechInputError::State)? = Some((project_id, session_id));
        Ok(())
    }

    async fn admit_recording(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> Result<tokio::sync::MutexGuard<'_, ()>, SpeechInputError> {
        let lifecycle = self.recording_lifecycle.lock().await;
        let scope = self
            .active_scope
            .lock()
            .map_err(|_| SpeechInputError::State)?;
        if !scope
            .as_ref()
            .is_some_and(|(project, session)| project == project_id && session == session_id)
        {
            return Err(SpeechInputError::Scope);
        }
        Ok(lifecycle)
    }

    fn revoke_scope(&self, project_id: &str, session_id: &str) -> Result<(), SpeechInputError> {
        let mut scope = self
            .active_scope
            .lock()
            .map_err(|_| SpeechInputError::State)?;
        if scope
            .as_ref()
            .is_some_and(|(project, session)| project == project_id && session == session_id)
        {
            *scope = None;
        }
        Ok(())
    }

    pub(crate) async fn record_start(
        &self,
        project_id: String,
        session_id: String,
        document_id: String,
        target: SpeechInputTarget,
    ) -> Result<SpeechRecordingSnapshot, SpeechInputError> {
        self.ensure_transcription_ready().await?;
        self.record_start_ready(project_id, session_id, document_id, target)
            .await
    }

    async fn record_start_ready(
        &self,
        project_id: String,
        session_id: String,
        document_id: String,
        target: SpeechInputTarget,
    ) -> Result<SpeechRecordingSnapshot, SpeechInputError> {
        let _lifecycle = self.admit_recording(&project_id, &session_id).await?;
        if self
            .recording
            .lock()
            .map_err(|_| SpeechInputError::State)?
            .is_some()
        {
            return Err(MicrophoneCaptureError::AlreadyRecording.into());
        }
        let recording = ActiveRecording {
            recording_id: SpeechRequestId::new().to_string(),
            project_id,
            session_id,
            document_id,
            target,
        };
        let microphone = Arc::clone(&self.microphone);
        let recording_id = recording.recording_id.clone();
        tokio::task::spawn_blocking(move || microphone.start(recording_id))
            .await
            .map_err(|error| SpeechInputError::Task(error.to_string()))??;
        let snapshot = recording.snapshot(SpeechRecordingPhase::Recording);
        *self.recording.lock().map_err(|_| SpeechInputError::State)? = Some(recording);
        Ok(snapshot)
    }

    async fn host(&self) -> Result<Arc<SpeechHost>, SpeechInputError> {
        self.host
            .get_or_try_init(|| async {
                let host = Arc::new(SpeechHost::default());
                let backend = Arc::new(
                    ParakeetSpeechBackend::discover(ParakeetBackendConfig {
                        model_dir: None,
                        managed_model_root: self.managed_model_root.clone(),
                    })
                    .await,
                );
                host.register_backend(backend)?;
                Ok::<_, SpeechHostError>(host)
            })
            .await
            .cloned()
            .map_err(Into::into)
    }

    pub(crate) async fn capability_status(&self) -> Result<SpeechHostStatus, SpeechInputError> {
        self.host().await?.status().map_err(Into::into)
    }

    async fn ensure_transcription_ready(&self) -> Result<(), SpeechInputError> {
        let status = self.capability_status().await?;
        let backend = status
            .backends
            .iter()
            .find(|backend| backend.id == PARAKEET_BACKEND_ID)
            .ok_or_else(|| {
                SpeechInputError::Unavailable("the Parakeet backend is not registered".to_owned())
            })?;
        match &backend.readiness {
            SpeechBackendReadiness::Ready => Ok(()),
            readiness => Err(SpeechInputError::Unavailable(format!(
                "Parakeet is not ready ({readiness:?}); install the exact local model before recording"
            ))),
        }
    }

    pub(crate) async fn transcribe_wav(
        &self,
        project_id: String,
        session_id: String,
        document_id: String,
        target: SpeechInputTarget,
        wav: Vec<u8>,
    ) -> Result<SpeechInputSnapshot, SpeechInputError> {
        if wav.len() < 12
            || wav.len() > MAX_WAV_BYTES
            || &wav[..4] != b"RIFF"
            || &wav[8..12] != b"WAVE"
        {
            return Err(SpeechInputError::InvalidWav);
        }
        let host = self.host().await?;
        let request_id = SpeechRequestId::new();
        let request = transcription_request(request_id.clone(), wav);
        request.validate()?;
        let snapshot = SpeechInputSnapshot {
            request_id: request_id.to_string(),
            project_id,
            session_id,
            document_id,
            target,
            phase: SpeechInputPhase::Transcribing,
            transcript: String::new(),
            error_code: None,
            error_message: None,
        };
        let session = Arc::new(AsyncMutex::new(snapshot.clone()));
        {
            // Serialize publication with scope revocation. A request delayed in
            // host initialization must not publish after close has drained it.
            let scope = self
                .active_scope
                .lock()
                .map_err(|_| SpeechInputError::State)?;
            if !scope.as_ref().is_some_and(|(project, session)| {
                project == &snapshot.project_id && session == &snapshot.session_id
            }) {
                return Err(SpeechInputError::Scope);
            }
            let mut sessions = self.sessions.lock().map_err(|_| SpeechInputError::State)?;
            prune_terminal_sessions(&mut sessions);
            if sessions.len() >= MAX_INPUT_SESSIONS {
                return Err(SpeechInputError::Capacity);
            }
            sessions.insert(request_id, Arc::clone(&session));
        }
        let task = tokio::spawn(run_transcription(host, request, session));
        let mut tasks = self.tasks.lock().map_err(|_| SpeechInputError::State)?;
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
        Ok(snapshot)
    }

    pub(crate) async fn record_stop(
        &self,
        project_id: &str,
        session_id: &str,
        recording_id: &str,
    ) -> Result<SpeechInputSnapshot, SpeechInputError> {
        let _lifecycle = self.recording_lifecycle.lock().await;
        let recording = self
            .recording
            .lock()
            .map_err(|_| SpeechInputError::State)?
            .clone()
            .ok_or(SpeechInputError::RecordingNotFound)?;
        validate_recording_scope(&recording, project_id, session_id, recording_id)?;
        let microphone = Arc::clone(&self.microphone);
        let id = recording_id.to_owned();
        let wav = tokio::task::spawn_blocking(move || microphone.stop(id))
            .await
            .map_err(|error| SpeechInputError::Task(error.to_string()))?;
        *self.recording.lock().map_err(|_| SpeechInputError::State)? = None;
        self.transcribe_wav(
            recording.project_id,
            recording.session_id,
            recording.document_id,
            recording.target,
            wav?,
        )
        .await
    }

    pub(crate) async fn record_cancel(
        &self,
        project_id: &str,
        session_id: &str,
        recording_id: &str,
    ) -> Result<SpeechRecordingSnapshot, SpeechInputError> {
        let _lifecycle = self.recording_lifecycle.lock().await;
        let recording = self
            .recording
            .lock()
            .map_err(|_| SpeechInputError::State)?
            .clone()
            .ok_or(SpeechInputError::RecordingNotFound)?;
        validate_recording_scope(&recording, project_id, session_id, recording_id)?;
        let microphone = Arc::clone(&self.microphone);
        let id = recording_id.to_owned();
        let result = tokio::task::spawn_blocking(move || microphone.cancel(id))
            .await
            .map_err(|error| SpeechInputError::Task(error.to_string()))?;
        *self.recording.lock().map_err(|_| SpeechInputError::State)? = None;
        result?;
        Ok(recording.snapshot(SpeechRecordingPhase::Cancelled))
    }

    pub(crate) async fn cancel(
        &self,
        project_id: &str,
        session_id: &str,
        request_id: &str,
    ) -> Result<SpeechInputSnapshot, SpeechInputError> {
        let id = SpeechRequestId(request_id.to_owned());
        let session = self
            .scoped_session(project_id, session_id, request_id)
            .await?;
        let mut snapshot = session.lock().await;
        if let Some(host) = self.host.get() {
            let _ = host.cancel(&id);
        }
        if !snapshot.phase.is_terminal() {
            snapshot.phase = SpeechInputPhase::CancelRequested;
        }
        Ok(snapshot.clone())
    }

    pub(crate) async fn status(
        &self,
        project_id: &str,
        session_id: &str,
        request_id: &str,
    ) -> Result<SpeechInputSnapshot, SpeechInputError> {
        let session = self
            .scoped_session(project_id, session_id, request_id)
            .await?;
        let snapshot = session.lock().await.clone();
        Ok(snapshot)
    }

    pub(crate) async fn cancel_scope(
        &self,
        project_id: &str,
        session_id: &str,
    ) -> Result<(), SpeechInputError> {
        // Revoke before waiting: an admitted start is drained below, and a
        // start still waiting for readiness/lifecycle cannot reopen this scope.
        self.revoke_scope(project_id, session_id)?;
        let _lifecycle = self.recording_lifecycle.lock().await;
        let scoped_recording = self
            .recording
            .lock()
            .map_err(|_| SpeechInputError::State)?
            .as_ref()
            .filter(|recording| {
                recording.project_id == project_id && recording.session_id == session_id
            })
            .map(|recording| recording.recording_id.clone());
        if let Some(recording_id) = scoped_recording {
            let microphone = Arc::clone(&self.microphone);
            tokio::task::spawn_blocking(move || microphone.cancel(recording_id))
                .await
                .map_err(|error| SpeechInputError::Task(error.to_string()))??;
            *self.recording.lock().map_err(|_| SpeechInputError::State)? = None;
        }
        let candidates = self
            .sessions
            .lock()
            .map_err(|_| SpeechInputError::State)?
            .iter()
            .map(|(id, session)| (id.clone(), Arc::clone(session)))
            .collect::<Vec<_>>();
        for (id, session) in candidates {
            let mut snapshot = session.lock().await;
            if snapshot.project_id == project_id
                && snapshot.session_id == session_id
                && !snapshot.phase.is_terminal()
            {
                if let Some(host) = self.host.get() {
                    let _ = host.cancel(&id);
                }
                snapshot.phase = SpeechInputPhase::CancelRequested;
            }
        }
        Ok(())
    }

    pub(crate) async fn shutdown(&self) -> Result<(), SpeechInputError> {
        *self
            .active_scope
            .lock()
            .map_err(|_| SpeechInputError::State)? = None;
        let _lifecycle = self.recording_lifecycle.lock().await;
        let microphone = Arc::clone(&self.microphone);
        tokio::task::spawn_blocking(move || microphone.shutdown())
            .await
            .map_err(|error| SpeechInputError::Task(error.to_string()))??;
        *self.recording.lock().map_err(|_| SpeechInputError::State)? = None;
        if let Some(host) = self.host.get() {
            host.shutdown().await?;
        }
        let tasks = self
            .tasks
            .lock()
            .map_err(|_| SpeechInputError::State)?
            .drain(..)
            .collect::<Vec<_>>();
        for task in tasks {
            task.await
                .map_err(|error| SpeechInputError::Task(error.to_string()))?;
        }
        Ok(())
    }

    fn session(
        &self,
        request_id: &str,
    ) -> Result<Arc<AsyncMutex<SpeechInputSnapshot>>, SpeechInputError> {
        self.sessions
            .lock()
            .map_err(|_| SpeechInputError::State)?
            .get(&SpeechRequestId(request_id.to_owned()))
            .cloned()
            .ok_or(SpeechInputError::NotFound)
    }

    async fn scoped_session(
        &self,
        project_id: &str,
        session_id: &str,
        request_id: &str,
    ) -> Result<Arc<AsyncMutex<SpeechInputSnapshot>>, SpeechInputError> {
        let session = self.session(request_id)?;
        let in_scope = {
            let snapshot = session.lock().await;
            snapshot.project_id == project_id && snapshot.session_id == session_id
        };
        if !in_scope {
            return Err(SpeechInputError::Scope);
        }
        Ok(session)
    }
}

fn validate_recording_scope(
    recording: &ActiveRecording,
    project_id: &str,
    session_id: &str,
    recording_id: &str,
) -> Result<(), SpeechInputError> {
    if recording.project_id != project_id || recording.session_id != session_id {
        return Err(SpeechInputError::RecordingScope);
    }
    if recording.recording_id != recording_id {
        return Err(SpeechInputError::RecordingNotFound);
    }
    Ok(())
}

fn transcription_request(request_id: SpeechRequestId, wav: Vec<u8>) -> TranscriptionRequest {
    TranscriptionRequest {
        context: SpeechRequestContext {
            request_id,
            client_id: "loom.microphone".to_owned(),
            route: SpeechRouteSelector::ExactBackend {
                backend_id: PARAKEET_BACKEND_ID.to_owned(),
                model_id: Some(PARAKEET_MODEL_ID.to_owned()),
                voice_id: None,
            },
            routing: SpeechRoutingPolicy {
                privacy: SpeechPrivacyPolicy::LocalOnly,
                profile: SpeechRouteProfile::ConsistentLocal,
                allow_asset_download: false,
                allow_fallback_before_output: false,
            },
            deadline: SpeechDeadlinePolicy::default(),
        },
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
    }
}

// SpeechHost::transcribe reserves its cancellable route before its first
// suspension. Poll admission once while holding the same snapshot lock used by
// cancellation, so cancellation either prevents admission or sees that route.
// Never hold this lock while waiting for backend readiness or inference.
async fn admit_transcription<T>(
    session: &AsyncMutex<SpeechInputSnapshot>,
    admission: impl Future<Output = T>,
) -> Option<T> {
    let mut admission = std::pin::pin!(admission);
    let first_poll = {
        let mut snapshot = session.lock().await;
        if snapshot.phase == SpeechInputPhase::CancelRequested {
            snapshot.phase = SpeechInputPhase::Cancelled;
            return None;
        }
        poll_fn(|cx| Poll::Ready(admission.as_mut().poll(cx))).await
    };
    Some(match first_poll {
        Poll::Ready(result) => result,
        Poll::Pending => admission.await,
    })
}

async fn run_transcription(
    host: Arc<SpeechHost>,
    request: TranscriptionRequest,
    session: Arc<AsyncMutex<SpeechInputSnapshot>>,
) {
    let Some(started) = admit_transcription(&session, host.transcribe(request)).await else {
        return;
    };
    let ticket = match started {
        Ok(ticket) => ticket,
        Err(error) => {
            fail_session(&session, "speech_start_failed", error.to_string()).await;
            return;
        }
    };
    match ticket.final_response().await {
        Ok(response) if response.text.len() <= MAX_TRANSCRIPT_BYTES => {
            let mut snapshot = session.lock().await;
            if snapshot.phase == SpeechInputPhase::CancelRequested {
                snapshot.phase = SpeechInputPhase::Cancelled;
            } else {
                snapshot.transcript = response.text;
                snapshot.phase = SpeechInputPhase::Completed;
            }
        }
        Ok(_) => {
            fail_session(
                &session,
                "speech_transcript_too_large",
                "the transcript exceeded Loom's 4 MB limit".to_owned(),
            )
            .await;
        }
        Err(error) if error.class == speech_native_types::SpeechErrorClass::Cancelled => {
            session.lock().await.phase = SpeechInputPhase::Cancelled;
        }
        Err(error) => fail_session(&session, &error.code, error.safe_detail).await,
    }
}

async fn fail_session(session: &AsyncMutex<SpeechInputSnapshot>, code: &str, message: String) {
    let mut snapshot = session.lock().await;
    if snapshot.phase == SpeechInputPhase::CancelRequested {
        snapshot.phase = SpeechInputPhase::Cancelled;
        return;
    }
    snapshot.phase = SpeechInputPhase::Failed;
    snapshot.error_code = Some(code.to_owned());
    snapshot.error_message = Some(message);
}

fn prune_terminal_sessions(
    sessions: &mut BTreeMap<SpeechRequestId, Arc<AsyncMutex<SpeechInputSnapshot>>>,
) {
    if sessions.len() < MAX_INPUT_SESSIONS {
        return;
    }
    if let Some(id) = sessions.iter().find_map(|(id, session)| {
        session
            .try_lock()
            .ok()
            .filter(|snapshot| snapshot.phase.is_terminal())
            .map(|_| id.clone())
    }) {
        sessions.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_request_is_complete_wav_exact_local_only_and_never_downloads() {
        let request = transcription_request(SpeechRequestId("request".to_owned()), wav_header());
        request.validate().expect("valid microphone request");
        assert_eq!(
            request.context.routing.privacy,
            SpeechPrivacyPolicy::LocalOnly
        );
        assert!(!request.context.routing.allow_asset_download);
        assert!(!request.context.routing.allow_fallback_before_output);
        assert!(matches!(
            request.context.route,
            SpeechRouteSelector::ExactBackend { ref backend_id, ref model_id, voice_id: None }
                if backend_id == PARAKEET_BACKEND_ID
                    && model_id.as_deref() == Some(PARAKEET_MODEL_ID)
        ));
        assert!(matches!(
            request.input,
            TranscriptionInput::Complete {
                audio: AudioInput::Encoded {
                    format: EncodedAudioFormat::Wav,
                    ..
                }
            }
        ));
    }

    #[tokio::test]
    async fn speech_request_cannot_cross_project_session_scope() {
        let service = SpeechInputService::new(None);
        service.sessions.lock().expect("session lock").insert(
            SpeechRequestId("request".to_owned()),
            Arc::new(AsyncMutex::new(SpeechInputSnapshot {
                request_id: "request".to_owned(),
                project_id: "project".to_owned(),
                session_id: "session".to_owned(),
                document_id: "document".to_owned(),
                target: SpeechInputTarget::Context,
                phase: SpeechInputPhase::Transcribing,
                transcript: String::new(),
                error_code: None,
                error_message: None,
            })),
        );
        assert!(matches!(
            service.status("other", "session", "request").await,
            Err(SpeechInputError::Scope)
        ));
    }

    #[test]
    fn microphone_recording_scope_binds_project_session_and_id() {
        let recording = ActiveRecording {
            recording_id: "recording".to_owned(),
            project_id: "project".to_owned(),
            session_id: "session".to_owned(),
            document_id: "document".to_owned(),
            target: SpeechInputTarget::Manuscript,
        };
        assert!(validate_recording_scope(&recording, "project", "session", "recording").is_ok());
        assert!(matches!(
            validate_recording_scope(&recording, "other", "session", "recording"),
            Err(SpeechInputError::RecordingScope)
        ));
        assert!(matches!(
            validate_recording_scope(&recording, "project", "session", "other"),
            Err(SpeechInputError::RecordingNotFound)
        ));
    }

    #[derive(Default)]
    struct TestSpeechBackend {
        dispatches: std::sync::atomic::AtomicUsize,
        cancellations: std::sync::atomic::AtomicUsize,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    #[async_trait::async_trait]
    impl speech_native_types::SpeechBackend for TestSpeechBackend {
        fn descriptor(&self) -> speech_native_types::SpeechBackendDescriptor {
            use speech_native_types::*;
            SpeechBackendDescriptor {
                id: PARAKEET_BACKEND_ID.into(),
                display_name: "model-free fixture".into(),
                kind: SpeechBackendKind::EmbeddedModel,
                readiness: SpeechBackendReadiness::Ready,
                capabilities: vec![SpeechCapability {
                    id: "fixture.transcription".into(),
                    backend_id: PARAKEET_BACKEND_ID.into(),
                    model_id: Some(PARAKEET_MODEL_ID.into()),
                    operation: SpeechOperationCapability::Transcription(
                        TranscriptionCapabilities {
                            accepted_audio: vec![AcceptedAudio::Wav],
                            ..TranscriptionCapabilities::default()
                        },
                    ),
                    availability: CapabilityAvailability::Available,
                    network: NetworkBehavior::Never,
                    languages: vec!["en".into()],
                    limits: SpeechCapabilityLimits::default(),
                    evidence: vec![CapabilityEvidence {
                        source_id: "test".into(),
                        source_version: None,
                        kind: EvidenceKind::RuntimeApi,
                        outcome: EvidenceOutcome::Confirmed,
                        observed_at_unix_ms: 1,
                        detail: "model-free fixture".into(),
                    }],
                }],
                models: Vec::new(),
                voices: Vec::new(),
            }
        }
        fn readiness(&self) -> SpeechBackendReadiness {
            SpeechBackendReadiness::Ready
        }
        async fn transcribe(
            &self,
            request: TranscriptionRequest,
        ) -> Result<speech_native_types::TranscriptionTicket, speech_native_types::SpeechError>
        {
            self.dispatches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.entered.notify_one();
            self.release.notified().await;
            Err(speech_native_types::SpeechError::unavailable(
                &request.context.request_id,
                "fixture_finished",
                "fixture released",
            ))
        }
        async fn synthesize(
            &self,
            request: speech_native_types::SynthesisRequest,
        ) -> Result<speech_native_types::SynthesisTicket, speech_native_types::SpeechError>
        {
            Err(speech_native_types::SpeechError::unavailable(
                &request.context.request_id,
                "fixture_unsupported",
                "transcription only",
            ))
        }
        fn cancel(&self, _id: &SpeechRequestId) -> usize {
            self.cancellations
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.release.notify_one();
            1
        }
        async fn shutdown(&self) -> Result<(), speech_native_types::SpeechError> {
            Ok(())
        }
    }

    fn test_host(service: &SpeechInputService) -> (Arc<SpeechHost>, Arc<TestSpeechBackend>) {
        let host = Arc::new(SpeechHost::default());
        let backend = Arc::new(TestSpeechBackend::default());
        host.register_backend(backend.clone())
            .expect("register fixture");
        service.host.set(Arc::clone(&host)).expect("install host");
        (host, backend)
    }

    #[tokio::test]
    async fn cancellation_after_host_route_registration_reaches_backend_and_terminalizes() {
        let service = SpeechInputService::new(None);
        let session = test_session(&service);
        let (host, backend) = test_host(&service);
        let task_session = Arc::clone(&session);
        let worker = tokio::spawn(run_transcription(
            host.clone(),
            transcription_request(SpeechRequestId("request".into()), wav_header()),
            task_session,
        ));
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            backend.entered.notified(),
        )
        .await
        .unwrap_or_else(|error| {
            panic!(
                "backend dispatch: {error}; snapshot: {:?}",
                session.try_lock()
            )
        });
        service
            .cancel("project", "session", "request")
            .await
            .expect("cancel");
        tokio::time::timeout(std::time::Duration::from_secs(10), worker)
            .await
            .expect("terminal acknowledgement")
            .expect("worker joined");
        host.shutdown().await.expect("host joined");
        assert!(
            backend
                .cancellations
                .load(std::sync::atomic::Ordering::SeqCst)
                > 0
        );
        assert_eq!(session.lock().await.phase, SpeechInputPhase::Cancelled);
        assert!(session.lock().await.transcript.is_empty());
    }

    fn test_session(service: &SpeechInputService) -> Arc<AsyncMutex<SpeechInputSnapshot>> {
        let session = Arc::new(AsyncMutex::new(SpeechInputSnapshot {
            request_id: "request".to_owned(),
            project_id: "project".to_owned(),
            session_id: "session".to_owned(),
            document_id: "document".to_owned(),
            target: SpeechInputTarget::Manuscript,
            phase: SpeechInputPhase::Transcribing,
            transcript: String::new(),
            error_code: None,
            error_message: None,
        }));
        service
            .sessions
            .lock()
            .expect("sessions")
            .insert(SpeechRequestId("request".to_owned()), Arc::clone(&session));
        session
    }

    #[tokio::test]
    async fn individual_and_scope_cancellation_before_first_poll_prevent_admission() {
        for cancel_scope in [false, true] {
            let service = SpeechInputService::new(None);
            service
                .bind_scope("project".into(), "session".into())
                .expect("bind");
            let session = test_session(&service);
            if cancel_scope {
                service
                    .cancel_scope("project", "session")
                    .await
                    .expect("cancel scope");
            } else {
                service
                    .cancel("project", "session", "request")
                    .await
                    .expect("cancel");
            }
            let (host, backend) = test_host(&service);
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                run_transcription(
                    host.clone(),
                    transcription_request(SpeechRequestId("request".into()), wav_header()),
                    Arc::clone(&session),
                ),
            )
            .await
            .expect("cancelled before admission");
            assert_eq!(
                backend.dispatches.load(std::sync::atomic::Ordering::SeqCst),
                0
            );
            host.shutdown().await.expect("host shutdown");
            assert_eq!(session.lock().await.phase, SpeechInputPhase::Cancelled);
        }
    }

    #[tokio::test]
    async fn revoked_scope_cannot_publish_transcription_after_host_initialization() {
        let service = SpeechInputService::new(None);
        let (host, backend) = test_host(&service);
        service
            .bind_scope("project".into(), "session".into())
            .expect("bind");
        service
            .cancel_scope("project", "session")
            .await
            .expect("close");
        assert!(matches!(
            service
                .transcribe_wav(
                    "project".into(),
                    "session".into(),
                    "document".into(),
                    SpeechInputTarget::Manuscript,
                    wav_header()
                )
                .await,
            Err(SpeechInputError::Scope)
        ));
        assert!(service.sessions.lock().expect("sessions").is_empty());
        assert_eq!(
            backend.dispatches.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        host.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn cancellation_cannot_interleave_with_the_admission_first_poll() {
        let service = SpeechInputService::new(None);
        let session = test_session(&service);
        let (registered, registration) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let admission_session = Arc::clone(&session);
        let worker = tokio::spawn(async move {
            admit_transcription(&admission_session, async {
                // Model-free stand-in for the host's synchronous route reserve.
                assert!(admission_session.try_lock().is_err());
                registered.send(()).expect("registration observer");
                released.await.expect("release admission");
            })
            .await
        });
        registration.await.expect("registered");
        // The lock must have been released at the first suspension, permitting
        // cancellation while the host is still awaiting readiness/dispatch.
        service
            .cancel("project", "session", "request")
            .await
            .expect("cancel");
        assert_eq!(
            session.lock().await.phase,
            SpeechInputPhase::CancelRequested
        );
        release.send(()).expect("admission alive");
        assert!(worker.await.expect("join").is_some());
        fail_session(&session, "cancelled", "host joined".into()).await;
        assert_eq!(session.lock().await.phase, SpeechInputPhase::Cancelled);
    }

    #[derive(Debug, Default)]
    struct TestMicrophone {
        starts: std::sync::atomic::AtomicUsize,
        active: std::sync::atomic::AtomicBool,
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl MicrophoneCapture for TestMicrophone {
        fn start(&self, _id: String) -> Result<(), MicrophoneCaptureError> {
            use std::sync::atomic::Ordering::SeqCst;
            self.starts.fetch_add(1, SeqCst);
            self.active.store(true, SeqCst);
            if let Some(entered) = self.entered.lock().expect("entered").take() {
                entered.send(()).expect("start observer");
            }
            if let Some(release) = self.release.lock().expect("release").take() {
                release
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("release start");
            }
            Ok(())
        }
        fn stop(&self, _id: String) -> Result<Vec<u8>, MicrophoneCaptureError> {
            self.active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            Ok(wav_header())
        }
        fn cancel(&self, _id: String) -> Result<(), MicrophoneCaptureError> {
            self.active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn shutdown(&self) -> Result<(), MicrophoneCaptureError> {
            self.active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    fn service_with_microphone(microphone: Arc<TestMicrophone>) -> Arc<SpeechInputService> {
        let mut service = SpeechInputService::new(None);
        service.microphone = microphone;
        let _ = test_host(&service);
        service
            .bind_scope("project".into(), "session".into())
            .expect("bind");
        Arc::new(service)
    }

    #[tokio::test]
    async fn close_revokes_start_still_waiting_for_readiness_and_new_session_can_record() {
        use std::sync::atomic::Ordering::SeqCst;
        let microphone = Arc::new(TestMicrophone::default());
        let service = service_with_microphone(Arc::clone(&microphone));
        let (ready, readiness) = tokio::sync::oneshot::channel();
        let start_service = Arc::clone(&service);
        let start = tokio::spawn(async move {
            // Pause at the boundary immediately after asynchronous readiness.
            readiness.await.expect("readiness");
            start_service
                .record_start_ready(
                    "project".into(),
                    "session".into(),
                    "document".into(),
                    SpeechInputTarget::Manuscript,
                )
                .await
        });
        service
            .cancel_scope("project", "session")
            .await
            .expect("close drain");
        ready.send(()).expect("pending start");
        assert!(matches!(
            start.await.expect("join"),
            Err(SpeechInputError::Scope)
        ));
        assert_eq!(microphone.starts.load(SeqCst), 0);
        assert!(!microphone.active.load(SeqCst));
        assert!(service.recording.lock().expect("recording").is_none());

        service
            .bind_scope("project".into(), "new-session".into())
            .expect("reopen");
        service
            .record_start_ready(
                "project".into(),
                "new-session".into(),
                "document".into(),
                SpeechInputTarget::Manuscript,
            )
            .await
            .expect("fresh start");
        // Delayed close from an old session cannot revoke the new authority.
        service
            .cancel_scope("project", "session")
            .await
            .expect("old close");
        assert!(microphone.active.load(SeqCst));
        service
            .cancel_scope("project", "new-session")
            .await
            .expect("new close");
        assert!(!microphone.active.load(SeqCst));
    }

    #[tokio::test]
    async fn close_waits_for_admitted_start_and_stops_capture_before_returning() {
        use std::sync::atomic::Ordering::SeqCst;
        let (entered, entrance) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let microphone = Arc::new(TestMicrophone {
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(Some(released)),
            ..TestMicrophone::default()
        });
        let service = service_with_microphone(Arc::clone(&microphone));
        let start_service = Arc::clone(&service);
        let start = tokio::spawn(async move {
            start_service
                .record_start(
                    "project".into(),
                    "session".into(),
                    "document".into(),
                    SpeechInputTarget::Manuscript,
                )
                .await
        });
        entrance.await.expect("capture admitted");
        let mut close = std::pin::pin!(service.cancel_scope("project", "session"));
        assert!(
            poll_fn(|cx| Poll::Ready(close.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert!(service.active_scope.lock().expect("scope").is_none());
        assert!(microphone.active.load(SeqCst));
        release.send(()).expect("release capture");
        start.await.expect("join start").expect("started");
        close.await.expect("close drained");
        assert!(!microphone.active.load(SeqCst));
        assert!(service.recording.lock().expect("recording").is_none());
    }

    fn wav_header() -> Vec<u8> {
        let mut wav = vec![0_u8; 46];
        wav[..4].copy_from_slice(b"RIFF");
        wav[4..8].copy_from_slice(&38_u32.to_le_bytes());
        wav[8..12].copy_from_slice(b"WAVE");
        wav[12..16].copy_from_slice(b"fmt ");
        wav[16..20].copy_from_slice(&16_u32.to_le_bytes());
        wav[20..22].copy_from_slice(&1_u16.to_le_bytes());
        wav[22..24].copy_from_slice(&1_u16.to_le_bytes());
        wav[24..28].copy_from_slice(&16_000_u32.to_le_bytes());
        wav[28..32].copy_from_slice(&32_000_u32.to_le_bytes());
        wav[32..34].copy_from_slice(&2_u16.to_le_bytes());
        wav[34..36].copy_from_slice(&16_u16.to_le_bytes());
        wav[36..40].copy_from_slice(b"data");
        wav[40..44].copy_from_slice(&2_u32.to_le_bytes());
        wav
    }
}
