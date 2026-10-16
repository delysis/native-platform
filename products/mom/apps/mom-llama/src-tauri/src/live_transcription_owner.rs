//! Owns one physical microphone stream and its independent Parakeet reader.
//! Gemma speculation may restart, but this source and its cursor never do.

use crate::app_runtime::AppWorkLease;
use crate::live_transcription::{self, FinalTranscript, IngressMetrics, PcmProducer, TurnIdentity};
use crate::operation_supervisor::TerminalClass;
use crate::speech::MomSpeech;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, Window};

struct Session {
    identity: TurnIdentity,
    producer: PcmProducer,
    result: tokio::task::JoinHandle<Result<FinalTranscript, String>>,
    started: Instant,
}

#[derive(Default)]
pub(crate) struct LiveTranscriptionOwner {
    sessions: Mutex<HashMap<String, Session>>,
}

#[derive(Serialize)]
pub(crate) struct LiveTranscriptionFinal {
    pub(crate) source: live_transcription::SourceReceipt,
    pub(crate) transcript: String,
}

impl LiveTranscriptionOwner {
    pub(crate) fn begin(
        &self,
        identity: TurnIdentity,
        operation: String,
        speech: Arc<MomSpeech>,
        lease: AppWorkLease,
        window: Window,
    ) -> Result<(), String> {
        let (producer, reader) =
            live_transcription::channel(identity.clone()).map_err(|error| error.to_string())?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "live transcription owner poisoned")?;
        // A renderer crash can abandon a source. Its backend has a shorter
        // deadline; release only expired identities before admitting another.
        sessions.retain(|_, session| session.started.elapsed() < Duration::from_secs(120));
        if sessions.len() >= 4 {
            return Err("live_transcription_owner_full".into());
        }
        if sessions.contains_key(&identity.turn)
            || sessions
                .values()
                .any(|session| session.identity.capture == identity.capture)
        {
            return Err("live_transcription_source_already_owned".into());
        }
        let trace = Arc::new(move |event| {
            let _ = window.emit("mom_llama_live_transcription_trace", event);
        });
        let result = tokio::spawn(async move {
            let result = speech.transcribe_live(&operation, reader, trace).await;
            let class = if lease.cancellation_requested() {
                TerminalClass::Cancelled
            } else if result.is_ok() {
                TerminalClass::Completed
            } else {
                TerminalClass::Failed
            };
            lease.finish(class)?;
            result
        });
        sessions.insert(
            identity.turn.clone(),
            Session {
                identity,
                producer,
                result,
                started: Instant::now(),
            },
        );
        Ok(())
    }

    pub(crate) fn push(
        &self,
        turn: &str,
        capture: &str,
        offset: usize,
        samples: &[f32],
    ) -> Result<IngressMetrics, String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "live transcription owner poisoned")?;
        let session = sessions
            .get_mut(turn)
            .ok_or("live_transcription_source_missing")?;
        if session.identity.capture != capture {
            return Err("live_transcription_capture_mismatch".into());
        }
        if session.result.is_finished() {
            return Err("live_transcription_backend_unavailable".into());
        }
        session
            .producer
            .append_at(offset, samples)
            .map_err(|error| error.to_string())?;
        Ok(session.producer.metrics())
    }

    pub(crate) async fn finish(
        &self,
        turn: &str,
        capture: &str,
        samples: usize,
    ) -> Result<LiveTranscriptionFinal, String> {
        let session = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| "live transcription owner poisoned")?;
            if sessions
                .get(turn)
                .is_none_or(|session| session.identity.capture != capture)
            {
                return Err("live_transcription_source_missing".into());
            }
            sessions.remove(turn).expect("checked owner")
        };
        let mut producer = session.producer;
        producer.seal(samples).map_err(|error| error.to_string())?;
        let result = session.result.await.map_err(|error| error.to_string())??;
        let transcript = result.text().to_owned();
        Ok(LiveTranscriptionFinal {
            source: result.source,
            transcript,
        })
    }

    pub(crate) fn cancel(&self, turn: &str, capture: &str) -> Result<(), String> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| "live transcription owner poisoned")?;
        if sessions
            .get(turn)
            .is_some_and(|session| session.identity.capture != capture)
        {
            return Err("live_transcription_capture_mismatch".into());
        }
        if !sessions.contains_key(turn) {
            return Ok(());
        }
        let mut session = sessions.remove(turn).expect("checked owner");
        session.producer.cancel();
        // The spawned task retains the application work lease until Parakeet
        // acknowledges cancellation. Dropping the join handle does not abort it.
        drop(session.result);
        Ok(())
    }
}
