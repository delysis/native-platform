//! Staged, turn-owned PCM ingress. No store, Gemma, IPC, or task-spawn authority.
//!
//! The producer is intentionally not Clone: capture is the sole source of
//! samples. A Gemma checkpoint is never a source offset for this stream.
//! The application owner must retain/join `MomSpeech::transcribe_live` under
//! its work lease before enabling these hooks in the shipped UI.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::{mpsc, watch};

pub(crate) const RATE: u32 = 16_000;
pub(crate) const PACKET_SAMPLES: usize = 640;
// Match the streaming Parakeet backend's two-hour admission budget. The
// former 29-second app limit ended the parallel reader during ordinary long
// speech even though the backend could continue consuming it.
pub(crate) const MAX_TURN_SAMPLES: usize = RATE as usize * 2 * 60 * 60;
// 2.56 seconds, plus at most one in-flight packet. Overflow fails the whole
// transcript, never the recording, and never produces a shortened final.
const QUEUE_PACKETS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TurnIdentity {
    pub(crate) turn: String,
    pub(crate) capture: String,
    pub(crate) conversation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IngressFailure {
    Identity,
    SourceOrder,
    InvalidPcm,
    TooLong,
    Overflow,
    ConsumerClosed,
    Cancelled,
    Abandoned,
    Incomplete,
}

impl std::fmt::Display for IngressFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "live_transcription_ingress_{self:?}")
    }
}
impl std::error::Error for IngressFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Terminal {
    Open,
    Sealed(usize),
    Failed(IngressFailure),
}

#[derive(Debug)]
pub(crate) struct Packet {
    pub(crate) offset: usize,
    pub(crate) count: usize,
    pub(crate) samples: [f32; PACKET_SAMPLES],
    capture_us: u64,
}

#[derive(Debug, Default)]
struct Counters {
    accepted: AtomicU64,
    backend_accepted: AtomicU64,
    high_water: AtomicU64,
    first_capture: AtomicU64,
    last_capture: AtomicU64,
    first_backend: AtomicU64,
    last_backend: AtomicU64,
    max_ingress_delay: AtomicU64,
    overflow: AtomicU64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct IngressMetrics {
    pub(crate) accepted_samples: u64,
    pub(crate) backend_sink_accepted_samples: u64,
    pub(crate) backlog_samples: u64,
    pub(crate) backlog_high_water_samples: u64,
    pub(crate) overflow_count: u64,
    pub(crate) first_sample_offset: Option<usize>,
    pub(crate) last_sample_exclusive: u64,
    pub(crate) first_capture_us: Option<u64>,
    pub(crate) last_capture_us: Option<u64>,
    pub(crate) first_backend_ingress_us: Option<u64>,
    pub(crate) last_backend_ingress_us: Option<u64>,
    pub(crate) max_capture_to_ingress_us: u64,
    pub(crate) sealed_samples: Option<usize>,
    pub(crate) failure: Option<IngressFailure>,
}

#[derive(Debug)]
struct Shared {
    identity: TurnIdentity,
    started: Instant,
    counters: Counters,
}
impl Shared {
    fn elapsed_us(&self) -> u64 {
        self.started
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX - 1)) as u64
    }
    fn metrics(&self, terminal: Terminal) -> IngressMetrics {
        let c = &self.counters;
        let accepted = c.accepted.load(Ordering::Acquire);
        let backend = c.backend_accepted.load(Ordering::Acquire);
        let time = |value: &AtomicU64| value.load(Ordering::Acquire).checked_sub(1);
        IngressMetrics {
            accepted_samples: accepted,
            backend_sink_accepted_samples: backend,
            backlog_samples: accepted.saturating_sub(backend),
            backlog_high_water_samples: c.high_water.load(Ordering::Acquire),
            overflow_count: c.overflow.load(Ordering::Acquire),
            first_sample_offset: (accepted > 0).then_some(0),
            last_sample_exclusive: accepted,
            first_capture_us: time(&c.first_capture),
            last_capture_us: time(&c.last_capture),
            first_backend_ingress_us: time(&c.first_backend),
            last_backend_ingress_us: time(&c.last_backend),
            max_capture_to_ingress_us: c.max_ingress_delay.load(Ordering::Acquire),
            sealed_samples: match terminal {
                Terminal::Sealed(n) => Some(n),
                _ => None,
            },
            failure: match terminal {
                Terminal::Failed(e) => Some(e),
                _ => None,
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct PcmProducer {
    sender: Option<mpsc::Sender<Packet>>,
    terminal: watch::Sender<Terminal>,
    shared: Arc<Shared>,
    next: usize,
}

#[derive(Clone)]
pub(crate) struct PcmObserver {
    shared: Arc<Shared>,
    terminal: watch::Receiver<Terminal>,
}
impl PcmObserver {
    pub(crate) fn identity(&self) -> &TurnIdentity {
        &self.shared.identity
    }
    pub(crate) fn metrics(&self) -> IngressMetrics {
        self.shared.metrics(*self.terminal.borrow())
    }
}

pub(crate) struct PcmReader {
    receiver: mpsc::Receiver<Packet>,
    terminal: watch::Receiver<Terminal>,
    shared: Arc<Shared>,
    dequeued: usize,
    acknowledged: usize,
    source_hash: Sha256,
    saw_end: bool,
}

pub(crate) fn channel(identity: TurnIdentity) -> Result<(PcmProducer, PcmReader), IngressFailure> {
    if [&identity.turn, &identity.capture, &identity.conversation]
        .iter()
        .any(|value| value.is_empty() || value.len() > 128 || value.chars().any(char::is_control))
    {
        return Err(IngressFailure::Identity);
    }
    let (sender, receiver) = mpsc::channel(QUEUE_PACKETS);
    let (terminal, terminal_reader) = watch::channel(Terminal::Open);
    let shared = Arc::new(Shared {
        identity,
        started: Instant::now(),
        counters: Counters::default(),
    });
    Ok((
        PcmProducer {
            sender: Some(sender),
            terminal,
            shared: Arc::clone(&shared),
            next: 0,
        },
        PcmReader {
            receiver,
            terminal: terminal_reader,
            shared,
            dequeued: 0,
            acknowledged: 0,
            source_hash: Sha256::new(),
            saw_end: false,
        },
    ))
}

impl PcmProducer {
    pub(crate) fn identity(&self) -> &TurnIdentity {
        &self.shared.identity
    }
    pub(crate) fn sample_offset(&self) -> usize {
        self.next
    }
    pub(crate) fn metrics(&self) -> IngressMetrics {
        self.shared.metrics(*self.terminal.borrow())
    }

    fn fail(&mut self, failure: IngressFailure) -> IngressFailure {
        // Failure is sticky. Even a later successful producer close cannot
        // authorize text from a missing packet or a cancelled source.
        let current = *self.terminal.borrow();
        let failure = match current {
            Terminal::Failed(first) => first,
            _ => failure,
        };
        self.terminal.send_replace(Terminal::Failed(failure));
        drop(self.sender.take());
        failure
    }

    /// Bounded, synchronous try-admission only. No await, IO, locks owned by
    /// inference, model work, or publication to the conversation store.
    pub(crate) fn append_at(
        &mut self,
        offset: usize,
        samples: &[f32],
    ) -> Result<(), IngressFailure> {
        let terminal = *self.terminal.borrow();
        match terminal {
            Terminal::Failed(error) => return Err(error),
            Terminal::Sealed(_) => return Err(self.fail(IngressFailure::SourceOrder)),
            Terminal::Open => {}
        }
        if offset != self.next {
            return Err(self.fail(IngressFailure::SourceOrder));
        }
        if samples.is_empty()
            || samples.len() > PACKET_SAMPLES
            || samples
                .iter()
                .any(|sample| !sample.is_finite() || sample.abs() > 1.0)
        {
            return Err(self.fail(IngressFailure::InvalidPcm));
        }
        if self
            .next
            .checked_add(samples.len())
            .is_none_or(|end| end > MAX_TURN_SAMPLES)
        {
            return Err(self.fail(IngressFailure::TooLong));
        }
        let now = self.shared.elapsed_us();
        let mut packet = Packet {
            offset,
            count: samples.len(),
            samples: [0.0; PACKET_SAMPLES],
            capture_us: now,
        };
        packet.samples[..samples.len()].copy_from_slice(samples);
        let result = self
            .sender
            .as_ref()
            .ok_or(IngressFailure::ConsumerClosed)?
            .try_send(packet);
        match result {
            Ok(()) => {
                self.next += samples.len();
                let c = &self.shared.counters;
                c.accepted.store(self.next as u64, Ordering::Release);
                let _ = c.first_capture.compare_exchange(
                    0,
                    now + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                c.last_capture.store(now + 1, Ordering::Release);
                let backlog =
                    (self.next as u64).saturating_sub(c.backend_accepted.load(Ordering::Acquire));
                c.high_water.fetch_max(backlog, Ordering::AcqRel);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.shared.counters.overflow.fetch_add(1, Ordering::AcqRel);
                Err(self.fail(IngressFailure::Overflow))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(self.fail(IngressFailure::ConsumerClosed))
            }
        }
    }

    /// Only physical end-of-capture may seal. Never call at a speculative
    /// Gemma end-of-input, VAD pause, checkpoint, or retarget.
    pub(crate) fn seal(&mut self, expected_samples: usize) -> Result<(), IngressFailure> {
        let terminal = *self.terminal.borrow();
        match terminal {
            Terminal::Failed(error) => return Err(error),
            Terminal::Sealed(n) if n == expected_samples => return Ok(()),
            Terminal::Sealed(_) => return Err(self.fail(IngressFailure::SourceOrder)),
            Terminal::Open => {}
        }
        if expected_samples == 0 || expected_samples != self.next {
            return Err(self.fail(IngressFailure::Incomplete));
        }
        // EOS is out of band: a full data queue cannot lose the seal. Dropping
        // the only sender wakes the consumer, which drains every queued packet.
        self.terminal.send_replace(Terminal::Sealed(self.next));
        drop(self.sender.take());
        Ok(())
    }

    pub(crate) fn reject(&mut self, reason: IngressFailure) {
        self.fail(reason);
    }
    pub(crate) fn cancel(&mut self) {
        self.fail(IngressFailure::Cancelled);
    }
}
impl Drop for PcmProducer {
    fn drop(&mut self) {
        let terminal = *self.terminal.borrow();
        if terminal == Terminal::Open {
            self.fail(IngressFailure::Abandoned);
        }
    }
}

impl PcmReader {
    pub(crate) fn observer(&self) -> PcmObserver {
        PcmObserver {
            shared: Arc::clone(&self.shared),
            terminal: self.terminal.clone(),
        }
    }
    pub(crate) fn identity(&self) -> &TurnIdentity {
        &self.shared.identity
    }
    pub(crate) fn status(&self) -> watch::Receiver<Terminal> {
        self.terminal.clone()
    }
    pub(crate) fn metrics(&self) -> IngressMetrics {
        self.shared.metrics(*self.terminal.borrow())
    }

    pub(crate) async fn next(&mut self) -> Result<Option<Packet>, IngressFailure> {
        if let Terminal::Failed(error) = *self.terminal.borrow() {
            return Err(error);
        }
        let packet = self.receiver.recv().await;
        if let Terminal::Failed(error) = *self.terminal.borrow() {
            return Err(error);
        }
        match packet {
            Some(packet) => {
                if packet.offset != self.dequeued || self.acknowledged != self.dequeued {
                    return Err(IngressFailure::SourceOrder);
                }
                self.dequeued += packet.count;
                Ok(Some(packet))
            }
            None => {
                if *self.terminal.borrow() != Terminal::Sealed(self.dequeued) || self.dequeued == 0
                {
                    return Err(IngressFailure::Incomplete);
                }
                self.saw_end = true;
                Ok(None)
            }
        }
    }

    /// Call only after the Speech ticket's audio sink accepts this whole packet.
    pub(crate) fn acknowledge(&mut self, packet: &Packet) -> Result<(), IngressFailure> {
        if packet.offset != self.acknowledged || packet.offset + packet.count != self.dequeued {
            return Err(IngressFailure::SourceOrder);
        }
        if let Terminal::Failed(error) = *self.terminal.borrow() {
            return Err(error);
        }
        for sample in &packet.samples[..packet.count] {
            self.source_hash.update(sample.to_le_bytes());
        }
        self.acknowledged = self.dequeued;
        let now = self.shared.elapsed_us();
        let c = &self.shared.counters;
        c.backend_accepted
            .store(self.acknowledged as u64, Ordering::Release);
        let _ = c
            .first_backend
            .compare_exchange(0, now + 1, Ordering::AcqRel, Ordering::Acquire);
        c.last_backend.store(now + 1, Ordering::Release);
        c.max_ingress_delay
            .fetch_max(now.saturating_sub(packet.capture_us), Ordering::AcqRel);
        Ok(())
    }

    pub(crate) fn source_receipt(&self) -> Result<SourceReceipt, IngressFailure> {
        if !self.saw_end
            || self.acknowledged != self.dequeued
            || *self.terminal.borrow() != Terminal::Sealed(self.acknowledged)
        {
            return Err(IngressFailure::Incomplete);
        }
        Ok(SourceReceipt {
            identity: self.shared.identity.clone(),
            samples: self.acknowledged,
            // Raw captured mono F32LE, not a WAV hash and not lossy I16 PCM.
            pcm_f32le_sha256: format!("{:x}", self.source_hash.clone().finalize()),
            metrics: self.metrics(),
        })
    }
}

/// Wake even while the consumer is blocked in model admission or sink.push.
/// Sealing is not failure; the final must still drain and acknowledge the tail.
pub(crate) async fn failure(mut status: watch::Receiver<Terminal>) -> IngressFailure {
    loop {
        if let Terminal::Failed(error) = *status.borrow_and_update() {
            return error;
        }
        if status.changed().await.is_err() {
            let terminal = *status.borrow();
            return match terminal {
                Terminal::Sealed(_) => std::future::pending().await,
                Terminal::Failed(error) => error,
                Terminal::Open => IngressFailure::Abandoned,
            };
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SourceReceipt {
    pub(crate) identity: TurnIdentity,
    pub(crate) samples: usize,
    pub(crate) pcm_f32le_sha256: String,
    pub(crate) metrics: IngressMetrics,
}

/// Staged result only: this type has no method that writes conversation state.
/// Backend partial events cannot construct it through the live runner.
pub(crate) struct FinalTranscript {
    pub(crate) source: SourceReceipt,
    text: String,
}

/// To be built by the trusted save owner, NOT reconstructed from the currently
/// selected UI row. Validate `current` inside the eventual store transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedVoiceSource {
    pub(crate) identity: TurnIdentity,
    pub(crate) message: String,
    pub(crate) attachment: String,
    pub(crate) root_sha256: String,
    pub(crate) artifact: String,
    pub(crate) policy_fingerprint: String,
    pub(crate) samples: usize,
    pub(crate) pcm_f32le_sha256: String,
}

impl FinalTranscript {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn from_complete(
        source: SourceReceipt,
        text: String,
    ) -> Result<Self, IngressFailure> {
        let text = text.trim().to_owned();
        if text.is_empty() || text.len() > 32_768 {
            return Err(IngressFailure::Incomplete);
        }
        Ok(Self { source, text })
    }
    /// An identity precondition, NOT an atomic commit or KV invalidation.
    pub(crate) fn validate_target(
        &self,
        expected: &SavedVoiceSource,
        current: &SavedVoiceSource,
    ) -> Result<&str, IngressFailure> {
        if expected != current
            || expected.identity != self.source.identity
            || expected.samples != self.source.samples
            || expected.pcm_f32le_sha256 != self.source.pcm_f32le_sha256
            || [
                &expected.message,
                &expected.attachment,
                &expected.root_sha256,
                &expected.artifact,
                &expected.policy_fingerprint,
            ]
            .iter()
            .any(|value| value.is_empty())
        {
            return Err(IngressFailure::Identity);
        }
        Ok(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> TurnIdentity {
        TurnIdentity {
            turn: "turn-a".into(),
            capture: "capture-a".into(),
            conversation: "chat-a".into(),
        }
    }
    async fn drain(reader: &mut PcmReader) -> Vec<f32> {
        let mut samples = Vec::new();
        while let Some(packet) = reader.next().await.expect("packet") {
            samples.extend_from_slice(&packet.samples[..packet.count]);
            reader.acknowledge(&packet).expect("accepted");
        }
        samples
    }

    #[tokio::test]
    async fn resumed_speech_does_not_seal_or_rewind_the_source() {
        let (mut producer, mut reader) = channel(identity()).expect("test setup");
        producer.append_at(0, &[0.1, 0.2]).expect("test setup");
        let packet = reader
            .next()
            .await
            .expect("test setup")
            .expect("test setup");
        reader.acknowledge(&packet).expect("test setup");
        // Gemma seals/cancels/retargets elsewhere. The same producer survives.
        assert!(reader.source_receipt().is_err(), "a pause is not final");
        producer.append_at(2, &[0.3, 0.4]).expect("test setup");
        producer.seal(4).expect("test setup");
        assert_eq!(drain(&mut reader).await, [0.3, 0.4]);
        assert_eq!(reader.source_receipt().expect("test setup").samples, 4);
    }
    #[tokio::test]
    async fn a_long_spoken_turn_delivers_its_final_source_past_29_seconds() {
        let (mut producer, mut reader) = channel(identity()).expect("test setup");
        let frame = [0.25; PACKET_SAMPLES];
        for index in 0..750 {
            let offset = index * PACKET_SAMPLES;
            producer
                .append_at(offset, &frame)
                .expect("long source accepted");
            let packet = reader.next().await.expect("reader healthy").expect("frame");
            assert_eq!(packet.offset, offset);
            reader.acknowledge(&packet).expect("frame acknowledged");
        }
        producer
            .seal(750 * PACKET_SAMPLES)
            .expect("complete source");
        assert!(reader.next().await.expect("reader healthy").is_none());
        assert_eq!(
            reader.source_receipt().expect("final receipt").samples,
            480_000
        );
    }
    #[tokio::test]
    async fn delayed_consumer_and_full_queue_seal_deliver_every_sample_once() {
        let (mut producer, mut reader) = channel(identity()).expect("test setup");
        let mut expected = Vec::new();
        for index in 0..QUEUE_PACKETS {
            let sample = index as f32 / QUEUE_PACKETS as f32;
            producer.append_at(index, &[sample]).expect("test setup");
            expected.push(sample);
        }
        producer.seal(expected.len()).expect("test setup"); // cannot be lost to queue pressure
        assert_eq!(drain(&mut reader).await, expected);
        assert_eq!(
            reader
                .source_receipt()
                .expect("test setup")
                .metrics
                .backlog_samples,
            0
        );
    }
    #[tokio::test]
    async fn overflow_is_sticky_and_cannot_become_an_incomplete_final() {
        let (mut producer, mut reader) = channel(identity()).expect("test setup");
        for offset in 0..QUEUE_PACKETS {
            producer.append_at(offset, &[0.1]).expect("test setup");
        }
        assert_eq!(
            producer.append_at(QUEUE_PACKETS, &[0.2]),
            Err(IngressFailure::Overflow)
        );
        assert_eq!(producer.seal(QUEUE_PACKETS), Err(IngressFailure::Overflow));
        assert!(matches!(reader.next().await, Err(IngressFailure::Overflow)));
        assert!(reader.source_receipt().is_err());
        assert_eq!(producer.metrics().overflow_count, 1);
    }
    #[tokio::test]
    async fn duplicate_source_delivery_is_rejected_not_silently_appended() {
        let (mut producer, mut reader) = channel(identity()).expect("test setup");
        producer.append_at(0, &[0.1, 0.2]).expect("test setup");
        assert_eq!(
            producer.append_at(0, &[0.1, 0.2]),
            Err(IngressFailure::SourceOrder)
        );
        assert!(matches!(
            reader.next().await,
            Err(IngressFailure::SourceOrder)
        ));
    }
    #[tokio::test]
    async fn dropping_an_unsealed_source_is_not_successful_eof() {
        let (mut producer, mut reader) = channel(identity()).expect("test setup");
        producer.append_at(0, &[0.1]).expect("test setup");
        drop(producer);
        assert!(matches!(
            reader.next().await,
            Err(IngressFailure::Abandoned)
        ));
        assert!(reader.source_receipt().is_err());
    }
    #[tokio::test]
    async fn cancellation_wakes_an_inflight_consumer_and_suppresses_final() {
        let (mut producer, mut reader) = channel(identity()).expect("test setup");
        let status = reader.status();
        producer.append_at(0, &[0.1]).expect("test setup");
        let packet = reader
            .next()
            .await
            .expect("test setup")
            .expect("test setup");
        producer.cancel();
        assert_eq!(failure(status).await, IngressFailure::Cancelled);
        assert_eq!(reader.acknowledge(&packet), Err(IngressFailure::Cancelled));
        assert!(reader.source_receipt().is_err());
    }
    #[tokio::test]
    async fn final_target_checks_turn_message_attachment_and_exact_source() {
        let (mut producer, mut reader) = channel(identity()).expect("test setup");
        producer.append_at(0, &[0.1, 0.2]).expect("test setup");
        // Neither a partial result nor a source prefix supplies a receipt.
        assert!(reader.source_receipt().is_err());
        producer.seal(2).expect("test setup");
        drain(&mut reader).await;
        let source = reader.source_receipt().expect("test setup");
        let target = SavedVoiceSource {
            identity: identity(),
            message: "message-a".into(),
            attachment: "attachment-a".into(),
            root_sha256: "root-a".into(),
            artifact: "artifact-a".into(),
            policy_fingerprint: "policy-a".into(),
            samples: 2,
            pcm_f32le_sha256: source.pcm_f32le_sha256.clone(),
        };
        let final_text =
            FinalTranscript::from_complete(source, "Complete final.".into()).expect("test setup");
        assert_eq!(
            final_text
                .validate_target(&target, &target)
                .expect("test setup"),
            "Complete final."
        );
        for field in 0..10 {
            let mut changed = target.clone();
            match field {
                0 => changed.identity.turn.push('x'),
                1 => changed.identity.conversation.push('x'),
                2 => changed.message.push('x'),
                3 => changed.attachment.push('x'),
                4 => changed.root_sha256.push('x'),
                5 => changed.artifact.push('x'),
                6 => changed.policy_fingerprint.push('x'),
                7 => changed.identity.capture.push('x'),
                8 => changed.samples += 1,
                _ => changed.pcm_f32le_sha256.push('x'),
            }
            assert_eq!(
                final_text.validate_target(&target, &changed),
                Err(IngressFailure::Identity)
            );
        }
    }
}
