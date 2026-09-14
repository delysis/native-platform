#![forbid(unsafe_code)]

//! Shared desktop composition for the existing local speech services.
//! Products retain microphone permission, capture, playback, source bindings,
//! transcript promotion, cancellation, and the lifetime of the returned host.

use std::path::PathBuf;
use std::sync::Arc;

use speech_native_backend_parakeet::{
    PARAKEET_BACKEND_ID, PARAKEET_MODEL_ID, ParakeetBackendConfig, ParakeetSpeechBackend,
};
use speech_native_host::{SpeechHost, SpeechHostError};
use speech_native_types::{
    AudioOutputFormat, NetworkBehavior, SpeechBackendKind, SpeechRequestId, SynthesisOutput,
    SynthesisResponse, TranscriptionResponse,
};

pub const APPLE_BACKEND_ID: &str = "apple.av-speech";

/// Noninteractive discovery only. This does not record, synthesize, play,
/// download, or load a transcription model. The application must retain and
/// drain this one host rather than creating a new host for each operation.
pub async fn discover_local_host(
    managed_model_root: Option<PathBuf>,
) -> Result<Arc<SpeechHost>, SpeechHostError> {
    let host = Arc::new(SpeechHost::default());
    let parakeet = ParakeetSpeechBackend::discover(ParakeetBackendConfig {
        model_dir: None,
        managed_model_root,
    })
    .await;
    host.register_backend(Arc::new(parakeet))?;
    #[cfg(target_os = "macos")]
    if let Ok(apple) = speech_native_platform::apple_backend::AppleSpeechBackend::discover().await {
        host.register_backend(Arc::new(apple))?;
    }
    Ok(host)
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum CompleteWavError {
    #[error("speech response did not match the admitted local Apple request and voice")]
    RouteMismatch,
    #[error("speech response was not one complete WAV output")]
    OutputKind,
    #[error("speech response did not return WAV audio")]
    OutputFormat,
    #[error("speech response was empty or exceeded the complete-audio byte limit")]
    ByteLimit,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("transcription response did not match the admitted local Parakeet request and model")]
pub struct TranscriptionProvenanceError;

/// Checks the actual response, in addition to the requested route. Source
/// artifact identity and each product's text limit remain product-owned.
pub fn validate_parakeet_response(
    response: &TranscriptionResponse,
    request_id: &SpeechRequestId,
) -> Result<(), TranscriptionProvenanceError> {
    if response.request_id != *request_id
        || response.route.backend_id != PARAKEET_BACKEND_ID
        || response.route.model_id.as_deref() != Some(PARAKEET_MODEL_ID)
        || response.route.voice_id.is_some()
        || response.route.backend_kind != SpeechBackendKind::EmbeddedModel
        || response.route.network != NetworkBehavior::Never
        || !response.usage.real_local_inference
    {
        return Err(TranscriptionProvenanceError);
    }
    Ok(())
}

/// Validates the response envelope and returns its original bytes unchanged.
/// The native backend owns WAV encoding. This function does not decode or
/// certify arbitrary WAV files, nor does it turn backend duration into a newly
/// measured value. `None` for voice permits the backend's automatic selection.
pub fn complete_apple_wav(
    response: SynthesisResponse,
    request_id: &SpeechRequestId,
    voice_id: Option<&str>,
    max_bytes: usize,
) -> Result<(Vec<u8>, Option<u64>), CompleteWavError> {
    if response.request_id != *request_id
        || response.route.backend_id != APPLE_BACKEND_ID
        || response.route.model_id.is_some()
        || voice_id.is_some_and(|voice| response.route.voice_id.as_deref() != Some(voice))
        || response.route.backend_kind != SpeechBackendKind::PlatformOnDevice
        || response.route.network != NetworkBehavior::Never
    {
        return Err(CompleteWavError::RouteMismatch);
    }
    let SynthesisOutput::Complete { audio, format } = response.output else {
        return Err(CompleteWavError::OutputKind);
    };
    if format != AudioOutputFormat::Wav {
        return Err(CompleteWavError::OutputFormat);
    }
    if audio.is_empty() || audio.len() > max_bytes {
        return Err(CompleteWavError::ByteLimit);
    }
    Ok((audio, response.duration_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use speech_native_types::SpeechResolvedRoute;

    fn response() -> SynthesisResponse {
        SynthesisResponse {
            request_id: SpeechRequestId("current".into()),
            route: SpeechResolvedRoute {
                backend_id: APPLE_BACKEND_ID.into(),
                model_id: None,
                voice_id: Some("voice".into()),
                backend_kind: SpeechBackendKind::PlatformOnDevice,
                network: NetworkBehavior::Never,
            },
            // These are envelope fixtures, deliberately not an audio decoding proof.
            output: SynthesisOutput::Complete {
                audio: vec![1, 2, 3],
                format: AudioOutputFormat::Wav,
            },
            duration_ms: Some(123),
            alignments: Vec::new(),
            usage: Default::default(),
        }
    }

    #[test]
    fn exact_and_automatic_voice_share_identity_and_bounds_checks() {
        let id = SpeechRequestId("current".into());
        assert_eq!(
            complete_apple_wav(response(), &id, Some("voice"), 3),
            Ok((vec![1, 2, 3], Some(123)))
        );
        assert!(complete_apple_wav(response(), &id, None, 3).is_ok());
        assert_eq!(
            complete_apple_wav(response(), &id, Some("other"), 3),
            Err(CompleteWavError::RouteMismatch)
        );
        assert_eq!(
            complete_apple_wav(response(), &SpeechRequestId("stale".into()), None, 3),
            Err(CompleteWavError::RouteMismatch)
        );
        assert_eq!(
            complete_apple_wav(response(), &id, None, 2),
            Err(CompleteWavError::ByteLimit)
        );
    }

    #[test]
    fn never_publishes_streamed_empty_or_remote_output() {
        let id = SpeechRequestId("current".into());
        let mut candidate = response();
        candidate.route.network = NetworkBehavior::Required;
        assert_eq!(
            complete_apple_wav(candidate, &id, None, 3),
            Err(CompleteWavError::RouteMismatch)
        );
        let mut candidate = response();
        candidate.output = SynthesisOutput::Streamed {
            format: AudioOutputFormat::Wav,
            emitted_bytes: 3,
        };
        assert_eq!(
            complete_apple_wav(candidate, &id, None, 3),
            Err(CompleteWavError::OutputKind)
        );
        let mut candidate = response();
        candidate.output = SynthesisOutput::Complete {
            audio: Vec::new(),
            format: AudioOutputFormat::Wav,
        };
        assert_eq!(
            complete_apple_wav(candidate, &id, None, 3),
            Err(CompleteWavError::ByteLimit)
        );
    }

    #[test]
    fn transcription_publication_requires_the_exact_route_and_native_evidence() {
        let id = SpeechRequestId("current".into());
        let mut response = TranscriptionResponse {
            request_id: id.clone(),
            route: SpeechResolvedRoute {
                backend_id: PARAKEET_BACKEND_ID.into(),
                model_id: Some(PARAKEET_MODEL_ID.into()),
                voice_id: None,
                backend_kind: SpeechBackendKind::EmbeddedModel,
                network: NetworkBehavior::Never,
            },
            text: "unpromoted transcript".into(),
            language: Some("en".into()),
            segments: Vec::new(),
            usage: Default::default(),
        };
        assert!(validate_parakeet_response(&response, &id).is_err());
        response.usage.real_local_inference = true;
        assert!(validate_parakeet_response(&response, &id).is_ok());
        assert!(validate_parakeet_response(&response, &SpeechRequestId("other".into())).is_err());
        response.route.model_id = Some("different-model".into());
        assert!(validate_parakeet_response(&response, &id).is_err());
        response.route.model_id = Some(PARAKEET_MODEL_ID.into());
        response.route.network = NetworkBehavior::Unknown;
        assert!(validate_parakeet_response(&response, &id).is_err());
    }
}
