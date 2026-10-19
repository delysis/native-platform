use speech_native_backend_pocket::{
    POCKET_BACKEND_ID, POCKET_MODEL_ID, POCKET_SAMPLE_RATE_HZ, PocketBackendConfig,
    PocketSpeechBackend,
};
use speech_native_host::SpeechHost;
use speech_native_types::{
    AlignmentGranularity, AudioChunk, AudioOutputFormat, PcmFormat, PcmSampleFormat, SpeechBackend,
    SpeechDeadlinePolicy, SpeechPrivacyPolicy, SpeechRequestContext, SpeechRequestId,
    SpeechResolvedRoute, SpeechRouteSelector, SpeechRoutingPolicy, SynthesisEvent, SynthesisInput,
    SynthesisOutput, SynthesisRequest, VoiceSelector,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const VOICE_ID: &str = "mom.pocket.dolores_mosquera";

fn request(id: &str, text: String) -> SynthesisRequest {
    SynthesisRequest {
        context: SpeechRequestContext {
            request_id: SpeechRequestId(id.to_string()),
            client_id: "real-pocket-integration-test".to_string(),
            route: SpeechRouteSelector::ExactBackend {
                backend_id: POCKET_BACKEND_ID.to_string(),
                model_id: Some(POCKET_MODEL_ID.to_string()),
                voice_id: Some(VOICE_ID.to_string()),
            },
            routing: SpeechRoutingPolicy {
                privacy: SpeechPrivacyPolicy::LocalOnly,
                allow_asset_download: false,
                allow_fallback_before_output: false,
                ..SpeechRoutingPolicy::default()
            },
            deadline: SpeechDeadlinePolicy {
                queue_ms: Some(30_000),
                model_load_ms: Some(120_000),
                first_result_ms: Some(30_000),
                idle_stream_ms: Some(30_000),
                total_ms: Some(180_000),
            },
        },
        input: SynthesisInput::Text { text },
        voice: VoiceSelector::Exact {
            voice_id: VOICE_ID.to_string(),
        },
        language: Some("en-US".to_string()),
        rate: 1.0,
        pitch: 1.0,
        volume: 1.0,
        output: AudioOutputFormat::Pcm {
            format: PcmFormat {
                sample_rate_hz: POCKET_SAMPLE_RATE_HZ,
                channels: 1,
                sample_format: PcmSampleFormat::I16Le,
                interleaved: true,
            },
        },
        alignment: AlignmentGranularity::None,
        stream: true,
    }
}

fn assert_route(route: &SpeechResolvedRoute) {
    assert_eq!(route.backend_id, POCKET_BACKEND_ID);
    assert_eq!(route.model_id.as_deref(), Some(POCKET_MODEL_ID));
    assert_eq!(route.voice_id.as_deref(), Some(VOICE_ID));
    assert_eq!(route.network, speech_native_types::NetworkBehavior::Never);
}

fn assert_pcm_chunk(chunk: &AudioChunk) {
    assert_eq!(chunk.format.sample_rate_hz, POCKET_SAMPLE_RATE_HZ);
    assert_eq!(chunk.format.channels, 1);
    assert_eq!(chunk.format.sample_format, PcmSampleFormat::I16Le);
    assert!(chunk.data.len().is_multiple_of(2));
}

#[tokio::test]
#[ignore = "requires pinned local Pocket assets in SPEECH_NATIVE_POCKET_VOICES and the HF cache"]
async fn real_pocket_host_stream_cancel_recovery_and_join() -> anyhow::Result<()> {
    let voice_root = std::env::var_os("SPEECH_NATIVE_POCKET_VOICES")
        .map(PathBuf::from)
        .expect("set SPEECH_NATIVE_POCKET_VOICES to the pinned local voice-resource root");
    let backend = Arc::new(PocketSpeechBackend::discover(
        PocketBackendConfig::discover_with_reference_root(Some(&voice_root)),
    ));
    let initial_descriptor = backend.descriptor();
    assert!(initial_descriptor.readiness.is_ready());
    assert_eq!(initial_descriptor.id, POCKET_BACKEND_ID);
    assert!(
        initial_descriptor.capabilities.iter().all(|capability| {
            capability.network == speech_native_types::NetworkBehavior::Never
        })
    );

    let host = SpeechHost::new(speech_native_types::PlatformTarget::current());
    host.register_backend(backend.clone())?;

    let complete_request_id = SpeechRequestId("pocket-real-complete".to_string());
    let mut ticket = host
        .synthesize(request(
            &complete_request_id.0,
            "We can take this one step at a time.".to_string(),
        ))
        .await?;
    assert_eq!(ticket.request_id, complete_request_id);
    let mut sequence = 0_u64;
    let mut sample_offset = 0_u64;
    let mut byte_count = 0_u64;
    let mut saw_start = false;
    let mut saw_eof = false;
    while let Some(event) = ticket.events.recv().await {
        match event {
            SynthesisEvent::Started { request_id, route } => {
                assert_eq!(request_id, complete_request_id);
                assert_route(&route);
                saw_start = true;
            }
            SynthesisEvent::Audio { request_id, chunk } => {
                assert_eq!(request_id, complete_request_id);
                assert_pcm_chunk(&chunk);
                assert_eq!(chunk.sequence, sequence);
                assert_eq!(chunk.sample_offset, sample_offset);
                if chunk.end_of_stream {
                    assert!(chunk.data.is_empty());
                    saw_eof = true;
                } else {
                    assert!(!saw_eof);
                    byte_count += chunk.data.len() as u64;
                    sample_offset += (chunk.data.len() / 2) as u64;
                }
                sequence += 1;
            }
            SynthesisEvent::Completed {
                request_id,
                response,
            } => {
                assert_eq!(request_id, complete_request_id);
                assert_eq!(response.request_id, complete_request_id);
                assert_route(&response.route);
                assert!(matches!(response.output,
                    SynthesisOutput::Streamed { emitted_bytes, .. } if emitted_bytes == byte_count));
                assert!(saw_start && saw_eof && byte_count > 0);
            }
            SynthesisEvent::Cancelled { .. } | SynthesisEvent::Failed { .. } => {
                panic!("unexpected terminal event in completed Pocket stream")
            }
            SynthesisEvent::Alignment { .. }
            | SynthesisEvent::UsageUpdated { .. }
            | SynthesisEvent::Warning { .. } => {}
        }
    }
    let completed = ticket.final_response().await?;
    assert_route(&completed.route);
    assert!(matches!(completed.output,
        SynthesisOutput::Streamed { emitted_bytes, .. } if emitted_bytes == byte_count));
    eprintln!(
        "{}",
        serde_json::json!({
            "kind": "pocket_real_integration_usage",
            "phase": "initial_complete",
            "request_id": complete_request_id.0,
            "usage": completed.usage,
        })
    );

    let mut max_cancel_settle_ms = 0_u128;
    let mut recovery_byte_counts = Vec::with_capacity(20);
    for cycle in 0..20 {
        let cancel_request_id = SpeechRequestId(format!("pocket-real-cancel-{cycle:02}"));
        let long_fixture = "This bounded integration fixture verifies cancellation while streamed audio events are not drained. ".repeat(16);
        let mut cancelled_ticket = host
            .synthesize(request(&cancel_request_id.0, long_fixture))
            .await?;
        let first_audio = tokio::time::timeout(Duration::from_secs(120), async {
            while let Some(event) = cancelled_ticket.events.recv().await {
                match event {
                    SynthesisEvent::Audio { request_id, chunk } => {
                        assert_eq!(request_id, cancel_request_id);
                        assert_pcm_chunk(&chunk);
                        if !chunk.data.is_empty() {
                            return true;
                        }
                    }
                    SynthesisEvent::Started { request_id, route } => {
                        assert_eq!(request_id, cancel_request_id);
                        assert_route(&route);
                    }
                    SynthesisEvent::Failed { error, .. } => {
                        panic!("cancel fixture failed: {}", error.code)
                    }
                    SynthesisEvent::Completed { .. } => return false,
                    SynthesisEvent::Alignment { .. }
                    | SynthesisEvent::UsageUpdated { .. }
                    | SynthesisEvent::Warning { .. }
                    | SynthesisEvent::Cancelled { .. } => {}
                }
            }
            false
        })
        .await
        .expect("Pocket must emit its first audio within the test guard");
        assert!(
            first_audio,
            "cancel fixture must produce audio before cancellation"
        );

        // Do not drain the event channel after first PCM. Keep events queued
        // behind the caller while the request-scoped cancellation is sent.
        let pending_deadline = Instant::now() + Duration::from_millis(250);
        while cancelled_ticket.events.is_empty() && Instant::now() < pending_deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(
            !cancelled_ticket.events.is_empty(),
            "audio events should remain queued behind the caller"
        );
        let cancel_started = Instant::now();
        assert!(
            host.cancel(&cancel_request_id) > 0,
            "cancellation is scoped to the active request"
        );
        let cancelled =
            tokio::time::timeout(Duration::from_secs(2), cancelled_ticket.final_response())
                .await
                .expect("Pocket cancellation must settle within the test guard");
        let cancel_settle_ms = cancel_started.elapsed().as_millis();
        assert!(cancel_settle_ms < 2_000);
        assert_eq!(
            cancelled
                .expect_err("cancelled Pocket request returns a cancellation error")
                .class,
            speech_native_types::SpeechErrorClass::Cancelled
        );
        max_cancel_settle_ms = max_cancel_settle_ms.max(cancel_settle_ms);

        let recovery_request_id = SpeechRequestId(format!("pocket-real-recovery-{cycle:02}"));
        let mut recovery = host
            .synthesize(request(
                &recovery_request_id.0,
                "This request verifies that cancellation leaves the same backend ready."
                    .to_string(),
            ))
            .await?;
        let mut recovery_audio_bytes = 0_u64;
        let mut recovery_eof = false;
        while let Some(event) = recovery.events.recv().await {
            match event {
                SynthesisEvent::Started { request_id, route } => {
                    assert_eq!(request_id, recovery_request_id);
                    assert_route(&route);
                }
                SynthesisEvent::Audio { request_id, chunk } => {
                    assert_eq!(request_id, recovery_request_id);
                    assert_pcm_chunk(&chunk);
                    if chunk.end_of_stream {
                        recovery_eof = true;
                    } else {
                        recovery_audio_bytes += chunk.data.len() as u64;
                    }
                }
                SynthesisEvent::Completed {
                    request_id,
                    response,
                } => {
                    assert_eq!(request_id, recovery_request_id);
                    assert_route(&response.route);
                }
                SynthesisEvent::Cancelled { .. } | SynthesisEvent::Failed { .. } => {
                    panic!("recovery request failed after cancellation")
                }
                SynthesisEvent::Alignment { .. }
                | SynthesisEvent::UsageUpdated { .. }
                | SynthesisEvent::Warning { .. } => {}
            }
        }
        let recovered = recovery.final_response().await?;
        assert_route(&recovered.route);
        assert!(recovery_eof && recovery_audio_bytes > 0);
        assert!(matches!(recovered.output,
            SynthesisOutput::Streamed { emitted_bytes, .. } if emitted_bytes == recovery_audio_bytes));
        eprintln!(
            "{}",
            serde_json::json!({
                "kind": "pocket_real_integration_usage",
                "phase": "post_cancel_recovery",
                "cycle": cycle,
                "request_id": recovery_request_id.0,
                "usage": recovered.usage,
            })
        );
        recovery_byte_counts.push(recovery_audio_bytes);
    }

    let shutdown_started = Instant::now();
    host.shutdown().await?;
    let shutdown_ms = shutdown_started.elapsed().as_millis();
    eprintln!(
        "Pocket integration evidence: host_os={}, host_arch={}, complete_bytes={byte_count}, cancellation_recovery_cycles={}, max_cancel_settle_ms={max_cancel_settle_ms}, minimum_recovery_bytes={}, shutdown_join_ms={shutdown_ms}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        recovery_byte_counts.len(),
        recovery_byte_counts.into_iter().min().unwrap_or_default()
    );
    Ok(())
}
