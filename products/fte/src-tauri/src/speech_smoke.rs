//! Opt-in launched-app proofs for the speech gateway registered in Tauri.
//!
//! This never runs during a normal application launch. Setting
//! `FTE_APPLE_TTS_SMOKE_RECEIPT` makes the app synthesize one fixed sentence,
//! write a WAV and JSON receipt beside the requested path, and exit.

use fte_speech_gateway::SpeechGateway;
use fte_speech_types::{
    AlignmentGranularity, AudioInput, AudioOutputFormat, DiarizationPolicy, EncodedAudioFormat,
    SpeechBackendReadiness, SpeechDeadlinePolicy, SpeechRequestContext, SpeechRequestId,
    SpeechRouteSelector, SpeechRoutingPolicy, SynthesisInput, SynthesisRequest,
    TimestampGranularity, TranscriptionInput, TranscriptionRequest, TranscriptionTask,
    VoiceSelector,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri_plugin_free_token_energy::FreeTokenEnergyExt;

const RECEIPT_ENV: &str = "FTE_APPLE_TTS_SMOKE_RECEIPT";
const STT_RECEIPT_ENV: &str = "FTE_PARAKEET_STT_SMOKE_RECEIPT";
const STT_WAV_ENV: &str = "FTE_SPEECH_TEST_WAV";

pub fn start_if_requested(app: tauri::AppHandle) {
    let tts_receipt = std::env::var_os(RECEIPT_ENV).map(PathBuf::from);
    let stt_receipt = std::env::var_os(STT_RECEIPT_ENV).map(PathBuf::from);
    let Some((kind, receipt_path)) = tts_receipt
        .map(|path| ("apple_tts", path))
        .or_else(|| stt_receipt.map(|path| ("parakeet_stt", path)))
    else {
        return;
    };
    let speech = app.free_token_energy_speech();
    tauri::async_runtime::spawn(async move {
        // Let AppKit enter its normal event loop and asynchronous speech
        // backend registration begin before running either product proof.
        tokio::time::sleep(Duration::from_millis(750)).await;
        let result = if kind == "apple_tts" {
            run_tts_smoke(speech, &receipt_path).await
        } else {
            run_stt_smoke(speech).await
        };
        let passed = result.is_ok();
        let receipt = match result {
            Ok(evidence) => json!({
                "schema": format!("fte.speech.{kind}_smoke.v1"),
                "status": "passed",
                "real_platform_synthesis": kind == "apple_tts",
                "real_embedded_transcription": kind == "parakeet_stt",
                "fake_fixture": false,
                "evidence": evidence,
            }),
            Err(error) => json!({
                "schema": format!("fte.speech.{kind}_smoke.v1"),
                "status": "failed",
                "real_platform_synthesis": false,
                "real_embedded_transcription": false,
                "fake_fixture": false,
                "error": error,
            }),
        };
        let _ = write_json(&receipt_path, &receipt);
        app.exit(if passed { 0 } else { 1 });
    });
}

async fn run_tts_smoke(speech: Arc<SpeechGateway>, receipt_path: &Path) -> Result<Value, String> {
    let request = SynthesisRequest {
        context: SpeechRequestContext {
            request_id: SpeechRequestId("tauri-apple-tts-smoke".to_string()),
            client_id: "tauri-launch-smoke".to_string(),
            route: SpeechRouteSelector::ExactBackend {
                backend_id: "apple.av-speech".to_string(),
                model_id: None,
                voice_id: None,
            },
            routing: SpeechRoutingPolicy::default(),
            deadline: SpeechDeadlinePolicy {
                total_ms: Some(30_000),
                ..SpeechDeadlinePolicy::default()
            },
        },
        input: SynthesisInput::Text {
            text: "Free Token Energy native speech smoke.".to_string(),
        },
        voice: VoiceSelector::Auto,
        language: Some("en-US".to_string()),
        rate: 1.0,
        pitch: 1.0,
        volume: 1.0,
        output: AudioOutputFormat::Wav,
        alignment: AlignmentGranularity::None,
        stream: false,
    };
    let ticket = speech
        .synthesize(request)
        .await
        .map_err(|error| error.to_string())?;
    let response = tokio::time::timeout(Duration::from_secs(30), ticket.final_response())
        .await
        .map_err(|_| {
            "Apple speech synthesis exceeded the 30-second app smoke deadline".to_string()
        })?
        .map_err(|error| error.to_string())?;
    if !response.audio.starts_with(b"RIFF") || response.audio.get(8..12) != Some(b"WAVE".as_slice())
    {
        return Err("Apple speech synthesis did not return a valid WAV header".to_string());
    }
    let wav_path = receipt_path.with_extension("wav");
    if let Some(parent) = wav_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::write(&wav_path, &response.audio).map_err(|error| error.to_string())?;
    Ok(json!({
        "backend_id": response.route.backend_id,
        "voice_id": response.route.voice_id,
        "network": response.route.network,
        "audio_bytes": response.audio.len(),
        "duration_ms": response.duration_ms,
        "real_local_inference": response.usage.real_local_inference,
        "wav_path": wav_path,
    }))
}

async fn run_stt_smoke(speech: Arc<SpeechGateway>) -> Result<Value, String> {
    let wav_path = std::env::var_os(STT_WAV_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{STT_WAV_ENV} must point at a real English WAV"))?;
    let wav = std::fs::read(&wav_path).map_err(|error| error.to_string())?;
    wait_for_parakeet(&speech).await?;
    let request = TranscriptionRequest {
        context: SpeechRequestContext {
            request_id: SpeechRequestId("tauri-parakeet-stt-smoke".to_string()),
            client_id: "tauri-launch-smoke".to_string(),
            route: SpeechRouteSelector::ExactBackend {
                backend_id: fte_speech_parakeet::PARAKEET_BACKEND_ID.to_string(),
                model_id: Some(fte_speech_parakeet::PARAKEET_MODEL_ID.to_string()),
                voice_id: None,
            },
            routing: SpeechRoutingPolicy::default(),
            deadline: SpeechDeadlinePolicy {
                total_ms: Some(60_000),
                ..SpeechDeadlinePolicy::default()
            },
        },
        input: TranscriptionInput::Complete {
            audio: AudioInput::Encoded {
                format: EncodedAudioFormat::Wav,
                data: wav,
            },
        },
        language: Some("en-US".to_string()),
        task: TranscriptionTask::Transcribe,
        timestamps: TimestampGranularity::None,
        diarization: DiarizationPolicy::Disabled,
        partial_results: true,
        punctuation: true,
        hotwords: Vec::new(),
    };
    let mut ticket = speech
        .transcribe(request)
        .await
        .map_err(|error| error.to_string())?;
    let mut event_count = 0_u64;
    let mut terminal_count = 0_u64;
    while let Some(event) = ticket.events.recv().await {
        event_count = event_count.saturating_add(1);
        if event.is_terminal() {
            terminal_count = terminal_count.saturating_add(1);
            break;
        }
    }
    let response = tokio::time::timeout(Duration::from_secs(60), ticket.final_response())
        .await
        .map_err(|_| {
            "Parakeet transcription exceeded the 60-second app smoke deadline".to_string()
        })?
        .map_err(|error| error.to_string())?;
    if response.text.trim().is_empty() {
        return Err("Parakeet returned an empty real-audio transcript".to_string());
    }
    if terminal_count != 1 {
        return Err(format!(
            "Parakeet emitted {terminal_count} terminal events instead of one"
        ));
    }
    Ok(json!({
        "backend_id": response.route.backend_id,
        "model_id": response.route.model_id,
        "network": response.route.network,
        "transcript": response.text,
        "input_audio_ms": response.usage.input_audio_ms,
        "model_load_ms": response.usage.model_load_ms,
        "total_ms": response.usage.total_ms,
        "real_local_inference": response.usage.real_local_inference,
        "event_count": event_count,
        "terminal_count": terminal_count,
        "wav_path": wav_path,
    }))
}

async fn wait_for_parakeet(speech: &SpeechGateway) -> Result<(), String> {
    let started = std::time::Instant::now();
    loop {
        let status = speech.status().map_err(|error| error.to_string())?;
        if let Some(backend) = status
            .backends
            .iter()
            .find(|backend| backend.id == fte_speech_parakeet::PARAKEET_BACKEND_ID)
        {
            return if backend.readiness == SpeechBackendReadiness::Ready {
                Ok(())
            } else {
                Err(format!(
                    "Parakeet registered without readiness: {:?}",
                    backend.readiness
                ))
            };
        }
        if started.elapsed() > Duration::from_secs(30) {
            return Err("Parakeet did not register within 30 seconds".to_string());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let encoded = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    std::fs::write(path, encoded).map_err(|error| error.to_string())
}
