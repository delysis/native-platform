use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const HELPER_TIMEOUT: Duration = Duration::from_secs(120);
const STARTUP_RUNWAY_MS: f64 = 120.0;

#[derive(Debug, Parser)]
#[command(about = "Measure the audio-in to playable-audio Gemma/Qwen pipeline")]
struct Args {
    #[arg(long)]
    helper: PathBuf,
    #[arg(long)]
    tts_model: PathBuf,
    #[arg(long)]
    reference: PathBuf,
    #[arg(long)]
    reference_text: String,
    #[arg(long, default_value = "christine_courtois")]
    persona: String,
    #[arg(long)]
    gemma_model: PathBuf,
    #[arg(long)]
    mtp_assistant: Option<PathBuf>,
    #[arg(long, default_value_t = 2)]
    mtp_max: i32,
    #[arg(long, default_value_t = 5)]
    mtp_warmup_tokens: usize,
    #[arg(long)]
    projector: PathBuf,
    #[arg(long)]
    voice_wav: PathBuf,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long, default_value = "Yes, that makes sense.")]
    phrase: String,
    #[arg(long, default_value = "Reply briefly and naturally to what you hear.")]
    direct_instruction: String,
}

#[derive(Debug)]
struct LineReader {
    lines: Receiver<Option<String>>,
}

impl LineReader {
    fn new<R>(stream: R) -> Self
    where
        R: std::io::Read + Send + 'static,
    {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                if sender.send(line.ok()).is_err() {
                    return;
                }
            }
            let _ = sender.send(None);
        });
        Self { lines }
    }

    fn json_event(&self, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("resident helper stopped making progress");
            }
            match self.lines.recv_timeout(remaining) {
                Ok(Some(line)) => {
                    if let Ok(value) = serde_json::from_str(&line) {
                        return Ok(value);
                    }
                }
                Ok(None) => bail!("resident helper exited"),
                Err(RecvTimeoutError::Timeout) => {
                    bail!("resident helper stopped making progress")
                }
                Err(RecvTimeoutError::Disconnected) => bail!("resident helper output closed"),
            }
        }
    }
}

#[derive(Clone, Debug)]
struct HelperRequestBase {
    persona_id: String,
    ref_audio: String,
    ref_sha256: String,
    ref_text: String,
}

impl HelperRequestBase {
    fn request(&self, id: &str, text: &str, warm_only: bool) -> Value {
        json!({
            "id": id,
            "text": text,
            "warm_only": warm_only,
            "persona_id": self.persona_id,
            "ref_audio": self.ref_audio,
            "ref_sha256": self.ref_sha256,
            "ref_text": self.ref_text,
            "chunk_size": 2,
        })
    }
}

fn send_request(stdin: &mut ChildStdin, payload: &Value) -> Result<()> {
    serde_json::to_writer(&mut *stdin, payload)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
struct TtsMetrics {
    first_pcm_ms: Option<f64>,
    helper_first_pcm_us: Option<u64>,
    total_ms: f64,
    audio_ms: f64,
    rtf: Option<f64>,
    packets: usize,
    first_packet_audio_ms: Option<f64>,
    maximum_packet_gap_ms: f64,
    required_startup_runway_ms: f64,
    startup_runway_ms: f64,
    playable_ms: Option<f64>,
}

fn earliest_safe_playback_ms(packet_timeline: &[(f64, u64)], startup_floor_ms: f64) -> Option<f64> {
    let &(first_arrival_ms, _) = packet_timeline.first()?;
    let mut cumulative_audio_ms = 0.0_f64;
    let mut startup_floor_met_ms = None;
    let mut underflow_lower_bound_ms = first_arrival_ms;

    for &(arrival_ms, packet_samples) in packet_timeline {
        // Playback starting before this lower bound would consume all PCM
        // received so far before the next packet arrived.
        underflow_lower_bound_ms = underflow_lower_bound_ms.max(arrival_ms - cumulative_audio_ms);
        cumulative_audio_ms += packet_samples as f64 / 24.0;
        if startup_floor_met_ms.is_none() && cumulative_audio_ms >= startup_floor_ms {
            startup_floor_met_ms = Some(arrival_ms);
        }
    }

    startup_floor_met_ms.map(|floor_met_ms| floor_met_ms.max(underflow_lower_bound_ms))
}

fn drain_request(reader: &LineReader, request_id: &str) -> Result<TtsMetrics> {
    let started = Instant::now();
    let mut first_pcm_ms = None;
    let mut helper_first_pcm_us = None;
    let mut samples = 0_u64;
    let mut packets = 0_usize;
    let mut first_packet_audio_ms = None;
    let mut previous_packet_ms = None;
    let mut maximum_packet_gap_ms = 0.0_f64;
    let mut required_startup_runway_ms = 0.0_f64;
    let mut packet_timeline = Vec::<(f64, u64)>::new();
    loop {
        let event = reader.json_event(HELPER_TIMEOUT)?;
        if event
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id != request_id)
        {
            continue;
        }
        match event.get("event").and_then(Value::as_str) {
            Some("audio") => {
                let arrival_ms = started.elapsed().as_secs_f64() * 1_000.0;
                if first_pcm_ms.is_none() {
                    first_pcm_ms = Some(arrival_ms);
                    helper_first_pcm_us = event.get("first_pcm_us").and_then(Value::as_u64);
                }
                let packet_samples = event.get("samples").and_then(Value::as_u64).unwrap_or(0);
                let packet_audio_ms = packet_samples as f64 / 24.0;
                first_packet_audio_ms.get_or_insert(packet_audio_ms);
                if let Some(previous) = previous_packet_ms {
                    maximum_packet_gap_ms = maximum_packet_gap_ms.max(arrival_ms - previous);
                }
                if let Some(first) = first_pcm_ms {
                    required_startup_runway_ms =
                        required_startup_runway_ms.max(arrival_ms - first - samples as f64 / 24.0);
                }
                previous_packet_ms = Some(arrival_ms);
                packet_timeline.push((arrival_ms, packet_samples));
                samples = samples.saturating_add(packet_samples);
                packets = packets.saturating_add(1);
            }
            Some("done" | "warmed") => {
                let total_ms = started.elapsed().as_secs_f64() * 1_000.0;
                let audio_ms = samples as f64 / 24.0;
                let playable_ms = earliest_safe_playback_ms(&packet_timeline, STARTUP_RUNWAY_MS);
                return Ok(TtsMetrics {
                    first_pcm_ms,
                    helper_first_pcm_us,
                    total_ms,
                    audio_ms,
                    rtf: (audio_ms > 0.0).then_some(total_ms / audio_ms),
                    packets,
                    first_packet_audio_ms,
                    maximum_packet_gap_ms,
                    required_startup_runway_ms,
                    startup_runway_ms: STARTUP_RUNWAY_MS,
                    playable_ms,
                });
            }
            Some("error" | "fatal") => bail!("resident helper failed: {event}"),
            _ => {}
        }
    }
}

fn first_speakable_chunk(text: &str, flush: bool) -> Option<String> {
    let words = text
        .split_whitespace()
        .scan(0_usize, |search_from, word| {
            let relative = text[*search_from..].find(word)?;
            let end = *search_from + relative + word.len();
            *search_from = end;
            Some((word, end))
        })
        .collect::<Vec<_>>();
    if words.is_empty() {
        return None;
    }
    for (index, &(word, end)) in words.iter().take(12).enumerate() {
        let trimmed = word.trim_end_matches([']', '"', '\'', ')']);
        let strong = trimmed.ends_with(['.', '!', '?']);
        let weak = trimmed.ends_with([';', ':', ',']);
        if (strong && (flush || index >= 2)) || (weak && index >= 2) {
            return Some(text[..end].trim().to_owned());
        }
    }
    if !flush && words.len() < 4 {
        return None;
    }
    if !flush && words.len() == 4 && text.chars().last().is_some_and(char::is_whitespace) {
        return Some(text[..words[3].1].trim().to_owned());
    }
    if !flush && words.len() > 4 {
        return Some(text[..words[3].1].trim().to_owned());
    }
    flush.then(|| {
        text[..words.last().expect("nonempty words").1]
            .trim()
            .to_owned()
    })
}

#[derive(Debug, Default)]
struct TurnState {
    text: String,
    first_token: Option<Value>,
    tts: Option<TtsMetrics>,
    speech_chunk: Option<String>,
    speech_chunk_ms: Option<f64>,
}

fn marker(line: &str, prefix: &str) -> Result<Option<Value>> {
    line.strip_prefix(prefix)
        .map(serde_json::from_str)
        .transpose()
        .map_err(Into::into)
}

fn spawn_cargo_test(package: &str, test: &str, env: &[(String, String)]) -> Result<Child> {
    let mut command = Command::new("cargo");
    command
        .args([
            "test",
            "-p",
            package,
            test,
            "--",
            "--ignored",
            "--nocapture",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command.envs(env.iter().cloned());
    command.spawn().context("launching native benchmark")
}

fn native_environment(args: &Args) -> Vec<(String, String)> {
    vec![
        (
            "MOM_LLAMA_MULTIMODAL_MODEL_PATH".into(),
            args.gemma_model.display().to_string(),
        ),
        (
            "MOM_LLAMA_MM_PROJ_PATH".into(),
            args.projector.display().to_string(),
        ),
        (
            "MOM_LLAMA_TEST_VOICE_WAV".into(),
            args.voice_wav.display().to_string(),
        ),
    ]
}

fn run_product_turns(
    args: &Args,
    helper_stdin: &mut ChildStdin,
    helper_reader: &LineReader,
    base: &HelperRequestBase,
) -> Result<Vec<Value>> {
    let mut child = spawn_cargo_test(
        "mom-llama-runtime",
        "real_incremental_voice_prefill_reports_silence_to_first_token",
        &native_environment(args),
    )?;
    let stdout = child.stdout.take().context("native benchmark stdout")?;
    let mut states = BTreeMap::<u64, TurnState>::new();
    let mut native_rows = BTreeMap::<u64, Value>::new();
    let mut tail = VecDeque::<String>::with_capacity(200);
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if tail.len() == 200 {
            tail.pop_front();
        }
        tail.push_back(line.clone());
        if let Some(value) = marker(&line, "VOICE_FIRST_TOKEN_BENCHMARK ")? {
            let turn = value["turn"].as_u64().context("first-token turn")?;
            states.entry(turn).or_default().first_token = Some(value);
        } else if let Some(value) = marker(&line, "VOICE_CONTENT_DELTA_BENCHMARK ")? {
            if value["event"] != "delta" {
                continue;
            }
            let turn = value["turn"].as_u64().context("delta turn")?;
            let state = states.entry(turn).or_default();
            state
                .text
                .push_str(value["delta"].as_str().unwrap_or_default());
            if state.tts.is_none()
                && let Some(chunk) = first_speakable_chunk(&state.text, false)
            {
                let request_id = format!("contended-{turn}");
                state.speech_chunk_ms = value["silence_elapsed_ms"].as_f64();
                state.speech_chunk = Some(chunk.clone());
                send_request(helper_stdin, &base.request(&request_id, &chunk, false))?;
                state.tts = Some(drain_request(helper_reader, &request_id)?);
            }
        } else if let Some(value) = marker(&line, "VOICE_LATENCY_BENCHMARK ")? {
            let turn = value["turn"].as_u64().context("latency turn")?;
            let state = states.entry(turn).or_default();
            if state.tts.is_none() {
                let text = value["assistant_text"].as_str().unwrap_or(&state.text);
                if let Some(chunk) = first_speakable_chunk(text, true) {
                    let request_id = format!("contended-{turn}");
                    state.speech_chunk_ms = value["silence_to_finish_ms"].as_f64();
                    state.speech_chunk = Some(chunk.clone());
                    send_request(helper_stdin, &base.request(&request_id, &chunk, false))?;
                    state.tts = Some(drain_request(helper_reader, &request_id)?);
                }
            }
            native_rows.insert(turn, value);
        }
    }
    let status = child.wait()?;
    if !status.success() {
        bail!(
            "native benchmark exited with {status}\n{}",
            tail.into_iter().collect::<Vec<_>>().join("\n")
        );
    }
    states
        .into_iter()
        .map(|(turn, state)| {
            let first = state.first_token.context("turn omitted first-token marker")?;
            let tts = state.tts.context("turn omitted TTS output")?;
            let speech_ms = state.speech_chunk_ms.context("turn omitted speech handoff")?;
            let first_pcm = tts.first_pcm_ms;
            let playable = tts.playable_ms;
            Ok(json!({
                "turn": turn,
                "first_token": first,
                "speech_chunk": state.speech_chunk,
                "silence_to_speech_chunk_ms": speech_ms,
                "tts": tts,
                "native": native_rows.remove(&turn).unwrap_or(Value::Null),
                "silence_to_first_pcm_ms": first_pcm.map(|value| speech_ms + value),
                "first_token_to_speech_chunk_ms": speech_ms - first["silence_to_first_token_ms"].as_f64().unwrap_or(0.0),
                "first_pcm_to_playable_ms": playable.zip(first_pcm).map(|(end, start)| end - start),
                "silence_to_playable_ms": playable.map(|value| speech_ms + value),
            }))
        })
        .collect()
}

fn run_direct_leg(
    args: &Args,
    enabled: bool,
    helper_stdin: &mut ChildStdin,
    helper_reader: &LineReader,
    base: &HelperRequestBase,
) -> Result<Value> {
    let assistant = args.mtp_assistant.as_ref().context("MTP assistant path")?;
    let mut env = native_environment(args);
    env.extend([
        (
            "MOM_LLAMA_MTP_ASSISTANT_PATH".into(),
            assistant.display().to_string(),
        ),
        (
            "MOM_LLAMA_MTP_DRAFT".into(),
            if enabled { "1" } else { "0" }.into(),
        ),
        (
            "MOM_LLAMA_MTP_AUDIO_INSTRUCTION".into(),
            args.direct_instruction.clone(),
        ),
        ("MOM_LLAMA_MTP_MAX".into(), args.mtp_max.to_string()),
        (
            "MOM_LLAMA_MTP_WARMUP_TOKENS".into(),
            args.mtp_warmup_tokens.to_string(),
        ),
    ]);
    let mut child = spawn_cargo_test(
        "llama-native-engine",
        "real_gemma4_mtp_audio_reports_silence_to_streamed_text",
        &env,
    )?;
    let stdout = child.stdout.take().context("direct benchmark stdout")?;
    let label = if enabled { "mtp" } else { "no-draft-control" };
    let mut text = String::new();
    let mut first_token_ms = None;
    let mut tts = None;
    let mut speech_chunk = None;
    let mut speech_chunk_ms = None;
    let mut summary = None;
    let mut tail = VecDeque::<String>::with_capacity(200);
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if tail.len() == 200 {
            tail.pop_front();
        }
        tail.push_back(line.clone());
        if let Some(value) = marker(&line, "VOICE_MTP_DELTA_BENCHMARK ")? {
            first_token_ms
                .get_or_insert_with(|| value["silence_elapsed_ms"].as_f64().unwrap_or(0.0));
            text.push_str(value["delta"].as_str().unwrap_or_default());
            if tts.is_none()
                && let Some(chunk) = first_speakable_chunk(&text, false)
            {
                let request_id = format!("direct-{label}");
                speech_chunk_ms = value["silence_elapsed_ms"].as_f64();
                speech_chunk = Some(chunk.clone());
                send_request(helper_stdin, &base.request(&request_id, &chunk, false))?;
                tts = Some(drain_request(helper_reader, &request_id)?);
            }
        } else if let Some(value) = marker(&line, "GEMMA4_MTP_AUDIO_BENCHMARK ")? {
            summary = Some(value);
        }
    }
    let status = child.wait()?;
    if !status.success() || summary.is_none() {
        bail!(
            "direct audio {label} benchmark failed: {status}\n{}",
            tail.into_iter().collect::<Vec<_>>().join("\n")
        );
    }
    let summary = summary.expect("checked");
    if tts.is_none() {
        let chunk = first_speakable_chunk(&text, true).context("direct leg emitted no speech")?;
        let request_id = format!("direct-{label}");
        speech_chunk_ms = summary["silence_to_completion_ms"].as_f64();
        speech_chunk = Some(chunk.clone());
        send_request(helper_stdin, &base.request(&request_id, &chunk, false))?;
        tts = Some(drain_request(helper_reader, &request_id)?);
    }
    let tts = tts.expect("filled");
    let speech_ms = speech_chunk_ms.context("direct leg omitted handoff")?;
    Ok(json!({
        "mtp_enabled": enabled,
        "native": summary,
        "text": text,
        "first_token_ms": first_token_ms,
        "speech_chunk": speech_chunk,
        "silence_to_speech_chunk_ms": speech_ms,
        "silence_to_first_pcm_ms": tts.first_pcm_ms.map(|value| speech_ms + value),
        "silence_to_playable_ms": tts.playable_ms.map(|value| speech_ms + value),
        "tts": tts,
    }))
}

fn validate_paths(args: &Args) -> Result<()> {
    for path in [
        &args.helper,
        &args.tts_model,
        &args.reference,
        &args.gemma_model,
        &args.projector,
        &args.voice_wav,
    ] {
        if !path.exists() {
            bail!("missing required local input: {}", path.display());
        }
    }
    if let Some(path) = &args.mtp_assistant
        && !path.exists()
    {
        bail!("missing MTP assistant: {}", path.display());
    }
    if args.mtp_max < 1 {
        bail!("--mtp-max must be positive");
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    validate_paths(&args)?;
    let reference_bytes = fs::read(&args.reference)?;
    let reference_sha256 = format!("{:x}", Sha256::digest(&reference_bytes));
    let base = HelperRequestBase {
        persona_id: args.persona.clone(),
        ref_audio: args.reference.display().to_string(),
        ref_sha256: reference_sha256.clone(),
        ref_text: args.reference_text.clone(),
    };
    let mut helper = Command::new(&args.helper)
        .args(["--resident", &args.tts_model.display().to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("launching resident Qwen helper")?;
    let mut helper_stdin = helper.stdin.take().context("Qwen helper stdin")?;
    let helper_reader = LineReader::new(helper.stdout.take().context("Qwen helper stdout")?);
    let result = (|| -> Result<Value> {
        let _ready = helper_reader.json_event(HELPER_TIMEOUT)?;
        send_request(&mut helper_stdin, &base.request("warm", "Ready.", true))?;
        let _warm = drain_request(&helper_reader, "warm")?;
        send_request(
            &mut helper_stdin,
            &base.request("baseline", &args.phrase, false),
        )?;
        let baseline_tts = drain_request(&helper_reader, "baseline")?;
        let turns = run_product_turns(&args, &mut helper_stdin, &helper_reader, &base)?;
        let (control, mut mtp) = if let Some(assistant) = args.mtp_assistant.as_ref() {
            let control = run_direct_leg(&args, false, &mut helper_stdin, &helper_reader, &base)?;
            let mut mtp = run_direct_leg(&args, true, &mut helper_stdin, &helper_reader, &base)?;
            if control["text"] != mtp["text"] {
                bail!("MTP changed sampled output under the same seed and prompt");
            }
            mtp["assistant_path"] = json!(assistant.display().to_string());
            mtp["assistant_sha256"] = json!(format!("{:x}", Sha256::digest(fs::read(assistant)?)));
            (Some(control), Some(mtp))
        } else {
            (None, None)
        };
        Ok(json!({
            "schema": "mom.voice_pipeline_contention.v5",
            "reference_sha256": reference_sha256,
            "phrase": args.phrase,
            "direct_instruction": args.direct_instruction,
            "mtp_max": args.mtp_max,
            "mtp_warmup_tokens": args.mtp_warmup_tokens,
            "baseline_tts": baseline_tts,
            "turns": turns,
            "direct_no_draft_control": control,
            "mtp": mtp.take(),
            "mtp_outputs_identical": args.mtp_assistant.is_some(),
        }))
    })();
    drop(helper_stdin);
    let _ = helper.kill();
    let _ = helper.wait();
    let receipt = result?;
    let output = serde_json::to_string_pretty(&receipt)? + "\n";
    if let Some(path) = &args.output {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &output)?;
    }
    print!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_playback_can_begin_between_packet_arrivals() {
        let timeline = [(100.0, 3_336), (250.0, 3_336)]; // 139 ms per packet.
        let playable = earliest_safe_playback_ms(&timeline, 120.0).expect("playable");
        assert!((playable - 111.0).abs() < 0.001);
    }

    #[test]
    fn startup_floor_waits_for_enough_received_audio() {
        let timeline = [(100.0, 1_440), (130.0, 1_440)]; // 60 ms per packet.
        let playable = earliest_safe_playback_ms(&timeline, 120.0).expect("playable");
        assert!((playable - 130.0).abs() < 0.001);
    }
}
