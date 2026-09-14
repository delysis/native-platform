//! Explicit microphone capture and local returned-audio synthesis. Neither
//! operation classifies intent, starts inference, or plays audio automatically.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::State;

use super::context_attachments::{StoredAttachment, import_recorded_wav};
use super::speech_input::{CapturedAudio, SpeechRecordingSnapshot};
use super::{
    IpcFailure, PluginState, lock_application_admission, lock_session, require_bound_store,
};

// At most one stopped recording can await persistence. A failed save keeps
// its original bytes in memory, and blocks another capture from replacing them.
#[derive(Clone, Debug, Default)]
pub(super) struct CapturePersistence {
    lifecycle: Arc<tokio::sync::Mutex<()>>,
    synthesis: Arc<tokio::sync::Mutex<()>>,
    pending: Arc<Mutex<Option<PendingCapture>>>,
}

#[derive(Debug)]
pub(super) struct AudioCloseGuard {
    _capture: tokio::sync::OwnedMutexGuard<()>,
    _synthesis: tokio::sync::OwnedMutexGuard<()>,
}

impl CapturePersistence {
    pub(super) fn close_guard(&self) -> Result<AudioCloseGuard, IpcFailure> {
        let guard = Arc::clone(&self.lifecycle).try_lock_owned().map_err(|_| {
            IpcFailure::new(
                "audio_save_in_progress",
                "Wait for the recording to finish saving before closing this project.",
                true,
            )
        })?;
        if self
            .pending
            .lock()
            .map_err(|_| failure("Recording state is unavailable."))?
            .is_some()
        {
            return Err(IpcFailure::new(
                "audio_save_pending",
                "A stopped recording is still in memory. Retry saving it before closing this project.",
                true,
            ));
        }
        let synthesis = Arc::clone(&self.synthesis).try_lock_owned().map_err(|_| {
            IpcFailure::new(
                "audio_synthesis_in_progress",
                "Wait for read aloud to finish preparing audio before closing.",
                true,
            )
        })?;
        Ok(AudioCloseGuard {
            _capture: guard,
            _synthesis: synthesis,
        })
    }
}

#[derive(Clone, Debug)]
struct PendingCapture {
    root: PathBuf,
    audio: Arc<CapturedAudio>,
}

#[derive(Debug, Serialize)]
pub(super) struct AudioRecording {
    pub recording_id: String,
    pub document_id: String,
    pub attachment: StoredAttachment,
    pub activity: AudioActivity,
}

#[derive(Debug, Serialize)]
pub(super) struct AudioSpeech {
    pub wav: Vec<u8>,
}

/// An energy gate, not a speech recognizer: noise and music can activate it.
/// The complete recording is retained regardless of these suggested intervals.
#[derive(Debug, Serialize)]
pub(super) struct AudioActivity {
    pub duration_ms: u64,
    pub signal_detected: bool,
    pub limit_reached: bool,
    pub segments: Vec<AudioInterval>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct AudioInterval {
    pub start_ms: u64,
    pub end_ms: u64,
}

#[tauri::command]
pub(super) async fn audio_record_start(
    project_id: String,
    session_id: String,
    document_id: String,
    state: State<'_, PluginState>,
) -> Result<SpeechRecordingSnapshot, IpcFailure> {
    let _persistence = state.audio_capture.lifecycle.lock().await;
    if state
        .audio_capture
        .pending
        .lock()
        .map_err(|_| failure("Recording state is unavailable."))?
        .is_some()
    {
        return Err(failure(
            "The previous recording is still in memory because saving failed. Retry saving it before recording again.",
        ));
    }
    {
        let _admission = lock_application_admission(&state, "microphone capture")?;
        let mut session = lock_session(&state)?;
        let store = require_bound_store(&mut session, &project_id, &session_id)?;
        let document = store
            .registered_document(
                document_id
                    .parse()
                    .map_err(|_| failure("Invalid document identity."))?,
            )
            .map_err(IpcFailure::store)?
            .ok_or_else(|| failure("The recording document does not exist."))?;
        store
            .read_document(&document.relative_path)
            .map_err(IpcFailure::store)?;
    }
    let recording = state
        .speech_input
        .record_start_capture(project_id.clone(), session_id.clone(), document_id)
        .await
        .map_err(|error| IpcFailure::speech_input(&error))?;
    let authority = (|| {
        let mut session = lock_session(&state)?;
        require_bound_store(&mut session, &project_id, &session_id)?;
        Ok::<(), IpcFailure>(())
    })();
    if let Err(error) = authority {
        let _ = state
            .speech_input
            .record_cancel(&project_id, &session_id, &recording.recording_id)
            .await;
        return Err(error);
    }
    Ok(recording)
}

#[tauri::command]
pub(super) async fn audio_record_stop(
    project_id: String,
    session_id: String,
    recording_id: String,
    state: State<'_, PluginState>,
) -> Result<AudioRecording, IpcFailure> {
    let root = {
        let mut session = lock_session(&state)?;
        require_bound_store(&mut session, &project_id, &session_id)?
            .root()
            .to_path_buf()
    };
    let service = Arc::clone(&state.speech_input);
    let persistence = state.audio_capture.clone();
    // An owned task retains and saves the result even if the renderer goes away
    // while stopping. The single pending buffer also survives storage errors.
    tokio::spawn(async move {
        let _persistence = persistence.lifecycle.lock().await;
        let pending = persistence
            .pending
            .lock()
            .map_err(|_| failure("Recording state is unavailable."))?
            .clone();
        let pending = if let Some(pending) = pending {
            let recording = &pending.audio.recording;
            if recording.recording_id != recording_id
                || recording.project_id != project_id
                || pending.root != root
            {
                return Err(failure(
                    "A different stopped recording is awaiting a save retry.",
                ));
            }
            pending
        } else {
            let captured = service
                .record_stop_capture(&project_id, &session_id, &recording_id)
                .await
                .map_err(|error| IpcFailure::speech_input(&error))?;
            let pending = PendingCapture {
                root,
                audio: Arc::new(captured),
            };
            *persistence
                .pending
                .lock()
                .map_err(|_| failure("Recording state is unavailable."))? = Some(pending.clone());
            pending
        };
        let result = tokio::task::spawn_blocking(move || persist_capture(&pending))
            .await
            .map_err(|_| failure("Recording remains in memory; the storage worker stopped."))?;
        if result.is_ok() {
            *persistence
                .pending
                .lock()
                .map_err(|_| failure("Recording state is unavailable."))? = None;
        }
        result
    })
    .await
    .map_err(|_| failure("The recording save worker stopped."))?
}

fn persist_capture(pending: &PendingCapture) -> Result<AudioRecording, IpcFailure> {
    let recording = &pending.audio.recording;
    let mut attachment = import_recorded_wav(
        &pending.root,
        format!("Recording-{}.wav", recording.recording_id),
        &pending.audio.wav,
    )
    .map_err(|error| {
        failure(format!(
            "Recording remains in memory; saving failed: {error}"
        ))
    })?;
    let activity = analyze_activity(&pending.audio.wav)?;
    if activity.limit_reached {
        attachment.warnings.push(
            "Recording reached the five-minute limit; the first five minutes were retained.".into(),
        );
    }
    Ok(AudioRecording {
        recording_id: recording.recording_id.clone(),
        document_id: recording.document_id.clone(),
        attachment,
        activity,
    })
}

#[tauri::command]
pub(super) async fn audio_synthesize(
    text: String,
    state: State<'_, PluginState>,
) -> Result<AudioSpeech, IpcFailure> {
    if text.trim().is_empty() || text.len() > 4_096 {
        return Err(failure(
            "Read aloud accepts between 1 and 4,096 bytes of text per request.",
        ));
    }
    let permit = {
        let _admission = lock_application_admission(&state, "read aloud")?;
        Arc::clone(&state.audio_capture.synthesis)
            .try_lock_owned()
            .map_err(|_| failure("Read aloud is already preparing audio."))?
    };
    #[cfg(target_os = "macos")]
    return apple::synthesize(text, permit).await;
    #[cfg(not(target_os = "macos"))]
    drop(permit);
    #[cfg(not(target_os = "macos"))]
    Err(failure(
        "Local returned-audio synthesis is not connected on this platform.",
    ))
}

fn analyze_activity(wav: &[u8]) -> Result<AudioActivity, IpcFailure> {
    let mut reader =
        hound::WavReader::new(Cursor::new(wav)).map_err(|error| failure(error.to_string()))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != 16_000
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err(failure("Recorded audio must be 16 kHz mono PCM16."));
    }
    let duration_ms = u64::from(reader.duration()) * 1000 / 16_000;
    let mut detector = ActivityDetector::default();
    let mut energy = 0.0;
    let mut count = 0;
    for sample in reader.samples::<i16>() {
        let sample = f64::from(sample.map_err(|error| failure(error.to_string()))?) / 32_768.0;
        energy += sample * sample;
        count += 1;
        if count == 320 {
            detector.frame(energy / 320.0 >= 0.0001);
            energy = 0.0;
            count = 0;
        }
    }
    if count > 0 {
        detector.frame(energy / f64::from(count) >= 0.0001);
    }
    detector.finish(duration_ms);
    Ok(AudioActivity {
        duration_ms,
        signal_detected: !detector.segments.is_empty(),
        limit_reached: duration_ms >= 300_000,
        segments: detector.segments,
    })
}

#[derive(Debug, Default)]
struct ActivityDetector {
    frames: u64,
    onset: u64,
    quiet: u64,
    active_start: Option<u64>,
    segments: Vec<AudioInterval>,
}

impl ActivityDetector {
    // 60 ms onset rejects isolated clicks; 300 ms release bridges brief gaps.
    fn frame(&mut self, audible: bool) {
        if audible {
            self.onset += 1;
            self.quiet = 0;
            if self.active_start.is_none() && self.onset >= 3 {
                let start_ms = (self.frames + 1 - self.onset).saturating_sub(5) * 20;
                self.active_start = Some(
                    self.segments
                        .last()
                        .map_or(start_ms, |last| start_ms.max(last.end_ms)),
                );
            }
        } else {
            self.onset = 0;
            self.quiet += 1;
            if self.quiet >= 15
                && let Some(start_ms) = self.active_start.take()
            {
                self.segments.push(AudioInterval {
                    start_ms,
                    end_ms: (self.frames + 1) * 20,
                });
            }
        }
        self.frames += 1;
    }

    fn finish(&mut self, duration_ms: u64) {
        if let Some(start_ms) = self.active_start.take() {
            self.segments.push(AudioInterval {
                start_ms,
                end_ms: duration_ms,
            });
        }
        for segment in &mut self.segments {
            segment.end_ms = segment.end_ms.min(duration_ms);
        }
    }
}

fn failure(message: impl Into<String>) -> IpcFailure {
    IpcFailure::new("audio_unavailable", message, false)
}

#[cfg(target_os = "macos")]
mod apple {
    use std::sync::Arc;

    use speech_native_host::SpeechHost;
    use speech_native_platform::apple_backend::AppleSpeechBackend;
    use speech_native_types::{
        AlignmentGranularity, AudioOutputFormat, SpeechDeadlinePolicy, SpeechPrivacyPolicy,
        SpeechRequestContext, SpeechRequestId, SpeechRouteProfile, SpeechRouteSelector,
        SpeechRoutingPolicy, SynthesisInput, SynthesisOutput, SynthesisRequest, VoiceSelector,
    };

    use super::{AudioSpeech, IpcFailure, failure};

    pub(super) async fn synthesize(
        text: String,
        permit: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<AudioSpeech, IpcFailure> {
        // This owned task always drains its host, including after the caller
        // navigates away. The native buffer API is not preemptively cancellable.
        tokio::spawn(async move {
            let _permit = permit;
            let host = SpeechHost::default();
            let backend = AppleSpeechBackend::discover()
                .await
                .map_err(|error| failure(error.to_string()))?;
            host.register_backend(Arc::new(backend))
                .map_err(|error| failure(error.to_string()))?;
            let result = async {
                let request = request(text);
                let ticket = host
                    .synthesize(request)
                    .await
                    .map_err(|error| failure(error.to_string()))?;
                let response = ticket
                    .final_response()
                    .await
                    .map_err(|error| failure(error.to_string()))?;
                match response.output {
                    SynthesisOutput::Complete {
                        audio,
                        format: AudioOutputFormat::Wav,
                    } if audio.len() <= 32 * 1024 * 1024 => Ok(AudioSpeech { wav: audio }),
                    _ => Err(failure(
                        "Local synthesis did not return a bounded WAV recording.",
                    )),
                }
            }
            .await;
            host.shutdown()
                .await
                .map_err(|error| failure(error.to_string()))?;
            result
        })
        .await
        .map_err(|_| failure("The local speech worker stopped."))?
    }

    fn request(text: String) -> SynthesisRequest {
        SynthesisRequest {
            context: SpeechRequestContext {
                request_id: SpeechRequestId::new(),
                client_id: "loom.read-aloud".to_owned(),
                route: SpeechRouteSelector::ExactBackend {
                    backend_id: "apple.av-speech".to_owned(),
                    model_id: None,
                    voice_id: None,
                },
                routing: SpeechRoutingPolicy {
                    privacy: SpeechPrivacyPolicy::LocalOnly,
                    profile: SpeechRouteProfile::NativePreferred,
                    allow_asset_download: false,
                    allow_fallback_before_output: false,
                },
                deadline: SpeechDeadlinePolicy {
                    total_ms: Some(60_000),
                    ..Default::default()
                },
            },
            input: SynthesisInput::Text { text },
            voice: VoiceSelector::Auto,
            language: None,
            rate: 1.0,
            pitch: 1.0,
            volume: 1.0,
            output: AudioOutputFormat::Wav,
            alignment: AlignmentGranularity::None,
            stream: false,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn synthesis_is_exact_local_wav_without_download_or_fallback() {
            let request = request("Read this.".to_owned());
            request.validate().expect("valid synthesis request");
            assert_eq!(
                request.context.routing.privacy,
                SpeechPrivacyPolicy::LocalOnly
            );
            assert!(!request.context.routing.allow_asset_download);
            assert!(!request.context.routing.allow_fallback_before_output);
            assert!(!request.stream);
            assert_eq!(request.output, AudioOutputFormat::Wav);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_capture_save_can_retry_with_the_exact_original_audio() {
        use super::super::speech_input::{SpeechInputTarget, SpeechRecordingPhase};
        let directory = tempfile::tempdir().expect("directory");
        let root = directory.path().join("project");
        std::fs::write(&root, b"blocked by a file").expect("invalid storage root");
        let mut wav = Cursor::new(Vec::new());
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(&mut wav, spec).expect("WAV");
        for _ in 0..1_600 {
            writer.write_sample(42_i16).expect("sample");
        }
        writer.finalize().expect("WAV complete");
        let original = wav.into_inner();
        let pending = PendingCapture {
            root: root.clone(),
            audio: Arc::new(CapturedAudio {
                recording: SpeechRecordingSnapshot {
                    recording_id: "capture".into(),
                    project_id: "project".into(),
                    session_id: "session".into(),
                    document_id: "document".into(),
                    target: SpeechInputTarget::Context,
                    phase: SpeechRecordingPhase::Captured,
                },
                wav: original.clone(),
            }),
        };
        let persistence = CapturePersistence::default();
        *persistence.pending.lock().expect("pending") = Some(pending.clone());
        assert!(persist_capture(&pending).is_err());
        assert_eq!(pending.audio.wav, original);
        assert_eq!(
            persistence
                .close_guard()
                .expect_err("unsaved audio blocks close")
                .code,
            "audio_save_pending"
        );
        assert!(
            CapturePersistence::default().close_guard().is_ok(),
            "other application instances are independent"
        );
        std::fs::remove_file(&root).expect("repair root");
        std::fs::create_dir(&root).expect("writable root");
        let saved = persist_capture(&pending).expect("same capture retries");
        let restored = super::super::context_attachments::resolve_for_generation_with_budget(
            &root,
            "document",
            &saved.attachment.inline_markdown,
            32_768,
            4,
            96,
        )
        .expect("read retained audio");
        assert_eq!(restored.media[0].bytes, original);
        assert_eq!(saved.document_id, "document");
        *persistence.pending.lock().expect("pending") = None;
        let guard = persistence
            .close_guard()
            .expect("saved audio permits close");
        assert!(
            Arc::clone(&persistence.lifecycle).try_lock_owned().is_err(),
            "close excludes another capture or save"
        );
        drop(guard);
        let speech = Arc::clone(&persistence.synthesis)
            .try_lock_owned()
            .expect("speech admitted");
        assert_eq!(
            persistence
                .close_guard()
                .expect_err("speech must drain before close")
                .code,
            "audio_synthesis_in_progress"
        );
        drop(speech);
    }

    #[test]
    fn energy_gate_rejects_clicks_and_bridges_short_pauses() {
        let mut detector = ActivityDetector::default();
        for frame in [true, false, false, false] {
            detector.frame(frame);
        }
        assert!(detector.active_start.is_none());
        for _ in 0..3 {
            detector.frame(true);
        }
        for _ in 0..14 {
            detector.frame(false);
        }
        assert!(detector.active_start.is_some());
        detector.frame(true);
        for _ in 0..15 {
            detector.frame(false);
        }
        assert_eq!(detector.segments.len(), 1);
        assert!(detector.active_start.is_none());
    }

    #[test]
    fn silent_pcm_is_retained_but_not_classified_as_active() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::new(&mut cursor, spec).expect("WAV writer");
            for _ in 0..1600 {
                writer.write_sample(0_i16).expect("silent sample");
            }
            writer.finalize().expect("complete WAV");
        }
        let activity = analyze_activity(&cursor.into_inner()).expect("valid captured format");
        assert_eq!(activity.duration_ms, 100);
        assert!(!activity.signal_detected);
        assert!(activity.segments.is_empty());
    }
}
