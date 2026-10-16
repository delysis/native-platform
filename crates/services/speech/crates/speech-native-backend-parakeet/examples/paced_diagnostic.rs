//! Opt-in local diagnostic: feed an English f32/16 kHz WAV at microphone pace.
//! This measures Parakeet alone; it does not qualify the packaged voice call.

use speech_native_backend_parakeet::{
    PARAKEET_BACKEND_ID, PARAKEET_MODEL_ID, ParakeetBackendConfig, ParakeetSpeechBackend,
};
use speech_native_host::SpeechHost;
use speech_native_types::{
    AudioChunk, AudioInput, DiarizationPolicy, EncodedAudioFormat, PcmFormat, PcmSampleFormat,
    SpeechDeadlinePolicy, SpeechRequestContext, SpeechRequestId, SpeechRouteSelector,
    SpeechRoutingPolicy, TimestampGranularity, TranscriptionInput, TranscriptionRequest,
    TranscriptionTask,
};
use std::{
    env, fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

fn request(id: &str, input: TranscriptionInput) -> TranscriptionRequest {
    TranscriptionRequest {
        context: SpeechRequestContext {
            request_id: SpeechRequestId(id.to_string()),
            client_id: "blessenger-paced-diagnostic".into(),
            route: SpeechRouteSelector::ExactBackend {
                backend_id: PARAKEET_BACKEND_ID.into(),
                model_id: Some(PARAKEET_MODEL_ID.into()),
                voice_id: None,
            },
            routing: SpeechRoutingPolicy::default(),
            deadline: SpeechDeadlinePolicy::default(),
        },
        input,
        language: Some("en".into()),
        task: TranscriptionTask::Transcribe,
        timestamps: TimestampGranularity::None,
        diarization: DiarizationPolicy::Disabled,
        partial_results: false,
        punctuation: true,
        hotwords: Vec::new(),
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let mut args = env::args_os().skip(1);
    let model_dir = PathBuf::from(args.next().expect("model directory"));
    let wav_path = PathBuf::from(args.next().expect("WAV input"));
    let wav = fs::read(&wav_path).expect("read WAV");
    let mut reader = hound::WavReader::new(std::io::Cursor::new(&wav)).expect("decode WAV");
    let spec = reader.spec();
    assert_eq!(spec.sample_rate, 16_000);
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
    let samples = reader
        .samples::<f32>()
        .collect::<Result<Vec<_>, _>>()
        .expect("samples");
    let backend = Arc::new(
        ParakeetSpeechBackend::discover(ParakeetBackendConfig {
            model_dir: Some(model_dir),
            managed_model_root: None,
        })
        .await,
    );
    let host = SpeechHost::default();
    host.register_backend(backend).expect("register Parakeet");
    let warm_start = Instant::now();
    let warm = host
        .transcribe(request(
            "paced-warm",
            TranscriptionInput::Complete {
                audio: AudioInput::Encoded {
                    format: EncodedAudioFormat::Wav,
                    data: wav,
                },
            },
        ))
        .await
        .expect("warm route");
    let warm_ticket_ms = warm_start.elapsed().as_millis();
    let warm_result = warm.final_response().await.expect("warm complete");
    assert!(!warm_result.text.trim().is_empty(), "warm transcript");
    eprintln!(
        "warm_ticket_ms={} warm_total_ms={} model_load_ms={:?}",
        warm_ticket_ms,
        warm_start.elapsed().as_millis(),
        warm_result.usage.model_load_ms
    );

    let format = PcmFormat {
        sample_rate_hz: 16_000,
        channels: 1,
        sample_format: PcmSampleFormat::F32Le,
        interleaved: true,
    };
    let ticket = host
        .transcribe(request(
            "paced-stream",
            TranscriptionInput::Stream {
                stream_id: "paced-stream-audio".into(),
                format,
            },
        ))
        .await
        .expect("start stream");
    let sink = ticket.audio_sink.as_ref().expect("audio sink").clone();
    let started = Instant::now();
    let mut max_push_ms = 0.0_f64;
    let mut total_push_ms = 0.0_f64;
    let mut sample_offset = 0_u64;
    let total_chunks = samples.chunks(640).len();
    for (sequence, chunk) in samples.chunks(640).enumerate() {
        let data = chunk
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let push_started = Instant::now();
        sink.push(AudioChunk {
            sequence: sequence as u64,
            sample_offset,
            format,
            data,
            end_of_stream: sequence + 1 == total_chunks,
        })
        .await
        .expect("push");
        let push_ms = push_started.elapsed().as_secs_f64() * 1_000.0;
        total_push_ms += push_ms;
        max_push_ms = max_push_ms.max(push_ms);
        sample_offset += chunk.len() as u64;
        let due = started + Duration::from_secs_f64(sample_offset as f64 / 16_000.0);
        tokio::time::sleep_until(due.into()).await;
    }
    let input_end = Instant::now();
    sink.finish().await.expect("finish stream");
    let result = ticket.final_response().await.expect("final transcript");
    assert!(!result.text.trim().is_empty(), "stream transcript");
    println!(
        "audio_ms={} input_wall_ms={} final_after_input_ms={} max_push_ms={:.2} total_push_ms={:.2} first_result_ms={:?} total_backend_ms={:?} transcript_chars={}",
        sample_offset * 1_000 / 16_000,
        input_end.duration_since(started).as_millis(),
        input_end.elapsed().as_millis(),
        max_push_ms,
        total_push_ms,
        result.usage.time_to_first_result_ms,
        result.usage.total_ms,
        result.text.chars().count()
    );
}
