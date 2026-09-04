use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, select};
use thiserror::Error;

const OUTPUT_RATE: u32 = 16_000;
const MAX_SAMPLES: usize = OUTPUT_RATE as usize * 5 * 60;
const QUEUE_CAPACITY: usize = 16;
const CHUNK_FRAMES: usize = 16_384;

#[derive(Debug, Error)]
pub(crate) enum MicrophoneCaptureError {
    #[error("a microphone recording is already active")]
    AlreadyRecording,
    #[error("there is no active microphone recording")]
    NotRecording,
    #[error("the active microphone recording does not match this request")]
    RecordingMismatch,
    #[error("no default microphone is available")]
    NoInputDevice,
    #[error("the default microphone configuration is unavailable: {0}")]
    DefaultConfiguration(String),
    #[error("the microphone reported an invalid configuration")]
    InvalidConfiguration,
    #[error("the microphone sample format is not supported: {0}")]
    UnsupportedSampleFormat(String),
    #[error("the microphone stream could not be created: {0}")]
    BuildStream(String),
    #[error("the microphone stream could not start: {0}")]
    StartStream(String),
    #[error("the microphone did not deliver audio after its stream started")]
    FirstSampleTimeout,
    #[error("the microphone stream failed while recording: {0}")]
    Stream(String),
    #[error("audio arrived faster than Loom's bounded recorder could consume it")]
    QueueOverflow,
    #[error("the recording exceeded Loom's five-minute limit")]
    DurationLimit,
    #[error("the recording did not contain any audio")]
    Empty,
    #[error("the microphone controller is unavailable: {0}")]
    ControllerUnavailable(String),
    #[error("the microphone owner thread stopped unexpectedly")]
    OwnerStopped,
    #[error("the microphone owner thread failed to join")]
    OwnerJoin,
    #[error("the captured WAV exceeded its representable size")]
    WavSize,
}

#[derive(Debug)]
enum Command {
    Start {
        id: String,
        reply: Sender<Result<(), MicrophoneCaptureError>>,
    },
    Stop {
        id: String,
        reply: Sender<Result<Vec<u8>, MicrophoneCaptureError>>,
    },
    Cancel {
        id: String,
        reply: Sender<Result<(), MicrophoneCaptureError>>,
    },
    Shutdown {
        reply: Sender<()>,
    },
}

#[derive(Debug)]
struct Chunk(Vec<f32>);

struct Active {
    id: String,
    stream: Stream,
    audio_rx: Receiver<Chunk>,
    free_tx: Sender<Vec<f32>>,
    overflowed: Arc<AtomicBool>,
    stream_error: Arc<Mutex<Option<String>>>,
    resampler: Resampler,
    samples: Vec<f32>,
    too_long: bool,
}

impl std::fmt::Debug for Active {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Active")
            .field("id", &self.id)
            .field("samples", &self.samples.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct NativeMicrophoneCapture {
    command_tx: Sender<Command>,
    owner: Mutex<Option<JoinHandle<()>>>,
    initialization_error: Option<String>,
}

impl NativeMicrophoneCapture {
    pub(crate) fn new() -> Self {
        let (command_tx, command_rx) = bounded(8);
        let owner = thread::Builder::new()
            .name("loom-microphone-owner".to_owned())
            .spawn(move || owner_loop(&command_rx));
        let (owner, initialization_error) = match owner {
            Ok(owner) => (Some(owner), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            command_tx,
            owner: Mutex::new(owner),
            initialization_error,
        }
    }

    pub(crate) fn start(&self, id: String) -> Result<(), MicrophoneCaptureError> {
        self.ensure_initialized()?;
        let (tx, rx) = bounded(1);
        self.command_tx
            .try_send(Command::Start { id, reply: tx })
            .map_err(controller)?;
        rx.recv()
            .map_err(|_| MicrophoneCaptureError::OwnerStopped)?
    }

    pub(crate) fn stop(&self, id: String) -> Result<Vec<u8>, MicrophoneCaptureError> {
        self.ensure_initialized()?;
        let (tx, rx) = bounded(1);
        self.command_tx
            .try_send(Command::Stop { id, reply: tx })
            .map_err(controller)?;
        rx.recv()
            .map_err(|_| MicrophoneCaptureError::OwnerStopped)?
    }

    pub(crate) fn cancel(&self, id: String) -> Result<(), MicrophoneCaptureError> {
        self.ensure_initialized()?;
        let (tx, rx) = bounded(1);
        self.command_tx
            .try_send(Command::Cancel { id, reply: tx })
            .map_err(controller)?;
        rx.recv()
            .map_err(|_| MicrophoneCaptureError::OwnerStopped)?
    }

    pub(crate) fn shutdown(&self) -> Result<(), MicrophoneCaptureError> {
        let owner = self
            .owner
            .lock()
            .map_err(|_| controller("owner lock poisoned"))?
            .take();
        let Some(owner) = owner else {
            return Ok(());
        };
        let (tx, rx) = bounded(1);
        self.command_tx
            .send(Command::Shutdown { reply: tx })
            .map_err(|_| MicrophoneCaptureError::OwnerStopped)?;
        rx.recv()
            .map_err(|_| MicrophoneCaptureError::OwnerStopped)?;
        owner.join().map_err(|_| MicrophoneCaptureError::OwnerJoin)
    }

    fn ensure_initialized(&self) -> Result<(), MicrophoneCaptureError> {
        self.initialization_error
            .as_ref()
            .map_or(Ok(()), |error| Err(controller(error.clone())))
    }
}

#[allow(clippy::needless_pass_by_value)]
fn controller(error: impl ToString) -> MicrophoneCaptureError {
    MicrophoneCaptureError::ControllerUnavailable(error.to_string())
}

fn owner_loop(commands: &Receiver<Command>) {
    let mut active: Option<Active> = None;
    loop {
        let Some(capture) = active.as_mut() else {
            match commands.recv() {
                Ok(Command::Start { id, reply }) => match begin(id) {
                    Ok(capture) => {
                        active = Some(capture);
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                },
                Ok(Command::Stop { reply, .. }) => {
                    let _ = reply.send(Err(MicrophoneCaptureError::NotRecording));
                }
                Ok(Command::Cancel { reply, .. }) => {
                    let _ = reply.send(Err(MicrophoneCaptureError::NotRecording));
                }
                Ok(Command::Shutdown { reply }) => {
                    let _ = reply.send(());
                    return;
                }
                Err(_) => return,
            }
            continue;
        };
        select! {
            recv(commands) -> command => match command {
                Ok(Command::Start { reply, .. }) => { let _ = reply.send(Err(MicrophoneCaptureError::AlreadyRecording)); }
                Ok(Command::Stop { id, reply }) => {
                    if capture.id != id { let _ = reply.send(Err(MicrophoneCaptureError::RecordingMismatch)); continue; }
                    let capture = active.take().expect("matched active capture");
                    let _ = reply.send(finish(capture));
                }
                Ok(Command::Cancel { id, reply }) => {
                    if capture.id != id { let _ = reply.send(Err(MicrophoneCaptureError::RecordingMismatch)); continue; }
                    drop(active.take()); let _ = reply.send(Ok(()));
                }
                Ok(Command::Shutdown { reply }) => { drop(active.take()); let _ = reply.send(()); return; }
                Err(_) => return,
            },
            recv(capture.audio_rx) -> chunk => if let Ok(chunk) = chunk { capture.push(chunk); }
        }
    }
}

impl Active {
    fn push(&mut self, mut chunk: Chunk) {
        if !self.too_long {
            self.resampler.push(&chunk.0, &mut self.samples);
            if self.samples.len() > MAX_SAMPLES {
                self.samples.truncate(MAX_SAMPLES);
                self.too_long = true;
            }
        }
        chunk.0.clear();
        let _ = self.free_tx.try_send(chunk.0);
    }
}

fn begin(id: String) -> Result<Active, MicrophoneCaptureError> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or(MicrophoneCaptureError::NoInputDevice)?;
    let supported = device
        .default_input_config()
        .map_err(|error| MicrophoneCaptureError::DefaultConfiguration(error.to_string()))?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels;
    let source_rate = config.sample_rate.0;
    if channels == 0 || source_rate == 0 {
        return Err(MicrophoneCaptureError::InvalidConfiguration);
    }
    let (audio_tx, audio_rx) = bounded(QUEUE_CAPACITY);
    let (free_tx, free_rx) = bounded(QUEUE_CAPACITY);
    for _ in 0..QUEUE_CAPACITY {
        free_tx
            .send(Vec::with_capacity(CHUNK_FRAMES))
            .map_err(controller)?;
    }
    let overflowed = Arc::new(AtomicBool::new(false));
    let stream_error = Arc::new(Mutex::new(None));
    let stream = build_stream(
        &device,
        &config,
        format,
        channels,
        &audio_tx,
        &free_rx,
        &overflowed,
        &stream_error,
    )?;
    stream
        .play()
        .map_err(|error| MicrophoneCaptureError::StartStream(error.to_string()))?;
    let mut capture = Active {
        id,
        stream,
        audio_rx,
        free_tx,
        overflowed,
        stream_error,
        resampler: Resampler::new(source_rate, OUTPUT_RATE),
        samples: Vec::with_capacity(OUTPUT_RATE as usize * 30),
        too_long: false,
    };
    let first = capture
        .audio_rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| first_sample_error(&capture))?;
    capture.push(first);
    Ok(capture)
}

#[allow(clippy::too_many_arguments)]
fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    format: SampleFormat,
    channels: u16,
    audio_tx: &Sender<Chunk>,
    free_rx: &Receiver<Vec<f32>>,
    overflowed: &Arc<AtomicBool>,
    stream_error: &Arc<Mutex<Option<String>>>,
) -> Result<Stream, MicrophoneCaptureError> {
    macro_rules! stream {
        ($ty:ty) => {{
            let tx = audio_tx.clone();
            let free = free_rx.clone();
            let full = Arc::clone(&overflowed);
            let errors = Arc::clone(&stream_error);
            device.build_input_stream::<$ty, _, _>(
                config,
                move |data, _| capture_callback(data, channels, &tx, &free, &full),
                move |error| store_stream_error(&errors, error.to_string()),
                None,
            )
        }};
    }
    let result = match format {
        SampleFormat::I8 => stream!(i8),
        SampleFormat::I16 => stream!(i16),
        SampleFormat::I24 => stream!(cpal::I24),
        SampleFormat::I32 => stream!(i32),
        SampleFormat::I64 => stream!(i64),
        SampleFormat::U8 => stream!(u8),
        SampleFormat::U16 => stream!(u16),
        SampleFormat::U32 => stream!(u32),
        SampleFormat::U64 => stream!(u64),
        SampleFormat::F32 => stream!(f32),
        SampleFormat::F64 => stream!(f64),
        _ => {
            return Err(MicrophoneCaptureError::UnsupportedSampleFormat(
                format.to_string(),
            ));
        }
    };
    result.map_err(|error| MicrophoneCaptureError::BuildStream(error.to_string()))
}

fn capture_callback<T>(
    data: &[T],
    channels: u16,
    tx: &Sender<Chunk>,
    free: &Receiver<Vec<f32>>,
    overflowed: &AtomicBool,
) where
    T: Sample + SizedSample + Copy,
    f32: FromSample<T>,
{
    if overflowed.load(Ordering::Relaxed) {
        return;
    }
    let channel_count = usize::from(channels);
    for block in data.chunks(CHUNK_FRAMES.saturating_mul(channel_count)) {
        if block.len() < channel_count {
            continue;
        }
        let Ok(mut mono) = free.try_recv() else {
            overflowed.store(true, Ordering::Release);
            return;
        };
        mono.clear();
        for frame in block.chunks_exact(channel_count) {
            let sum = frame.iter().copied().map(f32::from_sample).sum::<f32>();
            mono.push((sum / f32::from(channels)).clamp(-1.0, 1.0));
        }
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = tx.try_send(Chunk(mono))
        {
            overflowed.store(true, Ordering::Release);
            return;
        }
    }
}

fn store_stream_error(slot: &Mutex<Option<String>>, message: String) {
    if let Ok(mut error) = slot.lock()
        && error.is_none()
    {
        *error = Some(message);
    }
}

fn first_sample_error(capture: &Active) -> MicrophoneCaptureError {
    capture
        .stream_error
        .lock()
        .ok()
        .and_then(|error| error.clone())
        .map_or(
            MicrophoneCaptureError::FirstSampleTimeout,
            MicrophoneCaptureError::Stream,
        )
}

fn finish(mut capture: Active) -> Result<Vec<u8>, MicrophoneCaptureError> {
    capture
        .stream
        .pause()
        .map_err(|error| MicrophoneCaptureError::Stream(error.to_string()))?;
    while let Ok(chunk) = capture.audio_rx.try_recv() {
        capture.push(chunk);
    }
    if let Some(error) = capture
        .stream_error
        .lock()
        .map_err(|_| controller("stream error lock poisoned"))?
        .take()
    {
        return Err(MicrophoneCaptureError::Stream(error));
    }
    if capture.overflowed.load(Ordering::Acquire) {
        return Err(MicrophoneCaptureError::QueueOverflow);
    }
    if capture.too_long {
        return Err(MicrophoneCaptureError::DurationLimit);
    }
    encode_wav(&capture.samples)
}

#[allow(clippy::cast_possible_truncation)]
fn encode_wav(samples: &[f32]) -> Result<Vec<u8>, MicrophoneCaptureError> {
    if samples.is_empty() {
        return Err(MicrophoneCaptureError::Empty);
    }
    let data_len = samples
        .len()
        .checked_mul(2)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(MicrophoneCaptureError::WavSize)?;
    let riff_len = 36_u32
        .checked_add(data_len)
        .ok_or(MicrophoneCaptureError::WavSize)?;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&OUTPUT_RATE.to_le_bytes());
    wav.extend_from_slice(&(OUTPUT_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for &sample in samples {
        let scaled = if sample.is_nan() {
            0.0
        } else if sample >= 0.0 {
            sample.min(1.0) * f32::from(i16::MAX)
        } else {
            sample.max(-1.0) * 32_768.0
        };
        wav.extend_from_slice(&(scaled.round() as i16).to_le_bytes());
    }
    Ok(wav)
}

#[derive(Debug)]
struct Resampler {
    step: f64,
    index: u32,
    next: f64,
    previous: Option<f32>,
}
impl Resampler {
    fn new(source: u32, output: u32) -> Self {
        Self {
            step: f64::from(source) / f64::from(output),
            index: 0,
            next: 0.0,
            previous: None,
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    fn push(&mut self, input: &[f32], output: &mut Vec<f32>) {
        for &current in input {
            let position = f64::from(self.index);
            if let Some(previous) = self.previous {
                while self.next <= position {
                    let fraction = (self.next - (position - 1.0)).clamp(0.0, 1.0);
                    output.push(previous + (current - previous) * fraction as f32);
                    self.next += self.step;
                }
            } else {
                output.push(current);
                self.next += self.step;
            }
            self.previous = Some(current);
            self.index = self.index.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resampler_is_chunk_stable() {
        let input = (0..48_000)
            .map(|i| f32::from(u16::try_from(i % 1_000).expect("bounded")) / 1_000.0)
            .collect::<Vec<_>>();
        let mut output = Vec::new();
        let mut r = Resampler::new(48_000, 16_000);
        for c in input.chunks(137) {
            r.push(c, &mut output);
        }
        assert_eq!(output.len(), 16_000);
        assert!((output[1] - input[3]).abs() < 0.000_01);
    }
    #[test]
    fn wav_is_exact_pcm16_mono() {
        let wav = encode_wav(&[-1.0, 0.0, 1.0]).expect("wav");
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(wav[24..28].try_into().expect("rate")),
            16_000
        );
        assert_eq!(
            i16::from_le_bytes(wav[44..46].try_into().expect("sample")),
            i16::MIN
        );
        assert_eq!(
            i16::from_le_bytes(wav[48..50].try_into().expect("sample")),
            i16::MAX
        );
    }
    #[test]
    fn callback_pool_exhaustion_is_terminal() {
        let (tx, rx) = bounded(1);
        let (free_tx, free_rx) = bounded(1);
        free_tx
            .send(Vec::with_capacity(CHUNK_FRAMES))
            .expect("prime");
        let overflow = AtomicBool::new(false);
        capture_callback(&[-1.0_f32, 1.0, 0.5, 0.5], 2, &tx, &free_rx, &overflow);
        assert_eq!(rx.recv().expect("chunk").0, vec![0.0, 0.5]);
        capture_callback(&[0.25_f32, 0.75], 2, &tx, &free_rx, &overflow);
        assert!(overflow.load(Ordering::Acquire));
    }
}
