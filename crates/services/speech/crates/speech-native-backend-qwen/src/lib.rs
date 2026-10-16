#![forbid(unsafe_code)]

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use speech_native_types::{
    AudioOutputFormat, AudioOutputKind, CapabilityAvailability, CapabilityEvidence,
    DEFAULT_SPEECH_EVENT_CAPACITY, EvidenceKind, EvidenceOutcome, LatencyClass, NetworkBehavior,
    SpeechBackend, SpeechBackendDescriptor, SpeechBackendKind, SpeechBackendReadiness,
    SpeechCancellation, SpeechCapability, SpeechCapabilityLimits, SpeechError, SpeechErrorClass,
    SpeechModelDescriptor, SpeechOperationCapability, SpeechRequestId, SpeechResolvedRoute,
    SpeechRouteSelector, SpeechUsage, SynthesisCapabilities, SynthesisEvent, SynthesisInput,
    SynthesisOutput, SynthesisRequest, SynthesisResponse, SynthesisTicket, TaskSupervisor,
    TranscriptionRequest, TranscriptionTicket, UsageProvenance, VoiceDescriptor, VoiceGender,
    VoiceQuality, VoiceSelector,
};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};

pub const QWEN_BACKEND_ID: &str = "qwen3-tts.mlx-swift";
pub const QWEN_MODEL_ID: &str = "qwen3-tts-12hz-1.7b-voice-design-4bit";
pub const QWEN_VOICE_ID: &str = "mom.warm-clinician-v1";
pub const QWEN_MODEL_DIRECTORY: &str = "qwen3-tts-1.7b-voice-design-4bit";
pub const QWEN_MODEL_REVISION: &str = "5c390979e4b93af5f2932f90742ca99c7dd04687";
pub const QWEN_ICL_MODEL_ID: &str = "qwen3-tts-1.7b-base-icl-mlx-audio";
pub const QWEN_ICL_MODEL_DIRECTORY: &str = "qwen3-tts-1.7b-base-bf16";
const ICL_VOICE_PREFIX: &str = "mom.qwen.icl.";
const ICL_HELPER_PROTOCOL: &str = "mom-qwen3-tts-resident-v2";
const ICL_MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
const ICL_SAMPLE_RATE_HZ: u32 = 24_000;
const ICL_MAX_CHUNK_BYTES: usize = 2 * 1024 * 1024;
const VOICE_DESCRIPTION: &str = "A warm, articulate, reassuring mature American woman with natural conversational rhythm and gentle humor. Speak like a trusted colleague, never like an announcer.";

#[derive(Debug, Clone)]
pub struct QwenBackendConfig {
    pub helper: PathBuf,
    pub model_dir: PathBuf,
    pub icl: Option<QwenIclConfig>,
}

#[derive(Debug, Clone)]
pub struct QwenIclConfig {
    pub helper: PathBuf,
    pub model_dir: PathBuf,
    pub reference_root: PathBuf,
}

impl QwenBackendConfig {
    #[must_use]
    pub fn discover(managed_model_root: Option<&Path>) -> Self {
        Self::discover_with_reference_root(managed_model_root, None)
    }

    #[must_use]
    pub fn discover_with_reference_root(
        managed_model_root: Option<&Path>,
        installed_reference_root: Option<&Path>,
    ) -> Self {
        let helper = std::env::var_os("MOM_QWEN3_TTS_HELPER")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|p| p.join("qwen3-tts-helper")))
            })
            .unwrap_or_else(|| PathBuf::from("qwen3-tts-helper"));
        let explicit_model = std::env::var_os("MOM_QWEN3_TTS_MODEL").map(PathBuf::from);
        let managed_model = managed_model_root.map(|root| root.join(QWEN_MODEL_DIRECTORY));
        let legacy_model = std::env::var_os("HOME").map(PathBuf::from).map(|home| {
            home.join(
                "Library/Application Support/com.delysis.llama-native-kit.mom-llama/speech-models",
            )
            .join(QWEN_MODEL_DIRECTORY)
        });
        let model_dir = explicit_model
            .or_else(|| managed_model.filter(|path| model_admitted(path)))
            .or_else(|| legacy_model.filter(|path| model_admitted(path)))
            .unwrap_or_else(|| PathBuf::from(QWEN_MODEL_DIRECTORY));
        let icl = discover_icl_config(managed_model_root, installed_reference_root, &helper);
        Self {
            helper,
            model_dir,
            icl,
        }
    }
}

#[derive(Clone)]
pub struct QwenSpeechBackend {
    descriptor: SpeechBackendDescriptor,
    config: QwenBackendConfig,
    icl_references: Arc<Vec<AdmittedIclReference>>,
    tasks: Arc<TaskSupervisor>,
    active: Arc<Mutex<HashMap<SpeechRequestId, Arc<AtomicBool>>>>,
    icl_worker: Arc<Mutex<Option<ResidentIclWorker>>>,
    icl_foreground_waiters: Arc<AtomicUsize>,
    icl_warm_active: Arc<AtomicBool>,
    icl_warm_cancelled: Arc<AtomicBool>,
    icl_warm_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    icl_warm_completed: Arc<AtomicUsize>,
    icl_warm_total: Arc<AtomicUsize>,
    icl_warm_resident_us: Arc<AtomicU64>,
    icl_warm_probe_us: Arc<AtomicU64>,
    icl_warm_total_us: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IclWarmStatus {
    pub active: bool,
    pub completed: usize,
    pub total: usize,
    pub resident_us: u64,
    pub probe_us: u64,
    pub total_us: u64,
}

struct ResidentIclWorker {
    child: Child,
    input: Arc<Mutex<BufWriter<ChildStdin>>>,
    output: BufReader<ChildStdout>,
    sample_rate_hz: u32,
}

#[derive(Clone)]
struct AdmittedIclReference {
    entry: ReferenceEntry,
    clip: PathBuf,
}

#[derive(Serialize)]
struct IclWorkerJob<'a> {
    id: &'a str,
    text: &'a str,
    persona_id: &'a str,
    ref_audio: &'a str,
    ref_sha256: &'a str,
    ref_text: &'a str,
    warm_only: bool,
}

#[derive(Serialize)]
struct IclWorkerCancel<'a> {
    op: &'static str,
    id: &'a str,
}

#[derive(Deserialize)]
struct IclWorkerMessage {
    event: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    sample_rate: Option<u32>,
    #[serde(default)]
    samples: Option<u64>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    reference_cache: Option<String>,
    #[serde(default)]
    reference_us: Option<u64>,
    #[serde(default)]
    reference_identity_us: Option<u64>,
    #[serde(default)]
    reference_memory_lookup_us: Option<u64>,
    #[serde(default)]
    reference_disk_lookup_us: Option<u64>,
    #[serde(default)]
    reference_source_read_us: Option<u64>,
    #[serde(default)]
    reference_source_hash_us: Option<u64>,
    #[serde(default)]
    reference_compute_us: Option<u64>,
    #[serde(default)]
    reference_persist_us: Option<u64>,
    #[serde(default)]
    first_pcm_us: Option<u64>,
    #[serde(default)]
    text_chunk_us: Option<u64>,
    #[serde(default)]
    code_generation_us: Option<u64>,
    #[serde(default)]
    prepared_prefix_cache_hit: Option<bool>,
    #[serde(default)]
    first_decode_us: Option<u64>,
}

struct IclSynthesisResult {
    sample_count: u64,
    time_to_first_audio_ms: u64,
    reference_cache: Option<String>,
    reference_us: Option<u64>,
    reference_identity_us: Option<u64>,
    reference_memory_lookup_us: Option<u64>,
    reference_disk_lookup_us: Option<u64>,
    reference_source_read_us: Option<u64>,
    reference_source_hash_us: Option<u64>,
    reference_compute_us: Option<u64>,
    reference_persist_us: Option<u64>,
    helper_first_pcm_us: Option<u64>,
    helper_text_chunk_us: Option<u64>,
    helper_code_generation_us: Option<u64>,
    helper_prepared_prefix_cache_hit: Option<bool>,
    helper_first_decode_us: Option<u64>,
}

impl QwenSpeechBackend {
    #[must_use]
    pub fn discover(config: QwenBackendConfig) -> Self {
        let design_ready = config.helper.is_file() && model_admitted(&config.model_dir);
        let icl_references = config
            .icl
            .as_ref()
            .map_or_else(Vec::new, admitted_icl_references);
        let icl_voices = config
            .icl
            .as_ref()
            .map_or_else(Vec::new, |icl| admitted_icl_voices(icl, &icl_references));
        let ready = design_ready || !icl_voices.is_empty();
        Self {
            descriptor: descriptor(ready, design_ready, &config, &icl_voices),
            config,
            icl_references: Arc::new(icl_references),
            tasks: Arc::new(TaskSupervisor::with_scope("qwen3-tts")),
            active: Arc::new(Mutex::new(HashMap::new())),
            icl_worker: Arc::new(Mutex::new(None)),
            icl_foreground_waiters: Arc::new(AtomicUsize::new(0)),
            icl_warm_active: Arc::new(AtomicBool::new(false)),
            icl_warm_cancelled: Arc::new(AtomicBool::new(false)),
            icl_warm_thread: Arc::new(Mutex::new(None)),
            icl_warm_completed: Arc::new(AtomicUsize::new(0)),
            icl_warm_total: Arc::new(AtomicUsize::new(0)),
            icl_warm_resident_us: Arc::new(AtomicU64::new(0)),
            icl_warm_probe_us: Arc::new(AtomicU64::new(0)),
            icl_warm_total_us: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Start the resident model and compile the shared fixed-shape acoustic
    /// graphs with one admitted reference. Warming every persona here keeps
    /// submitting Metal work long after the model is resident and can starve
    /// a live microphone projector. Per-persona reference artifacts remain
    /// content-addressed on disk and are admitted lazily on first use.
    pub fn warm_icl_in_background(&self) {
        let Some(config) = self.config.icl.clone() else {
            return;
        };
        if self
            .icl_warm_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.icl_warm_cancelled.store(false, Ordering::Release);
        if let Ok(mut prior) = self.icl_warm_thread.lock()
            && let Some(handle) = prior.take()
        {
            let _ = handle.join();
        }
        let worker_slot = Arc::clone(&self.icl_worker);
        let foreground_waiters = Arc::clone(&self.icl_foreground_waiters);
        let warm_active = Arc::clone(&self.icl_warm_active);
        let warm_cancelled = Arc::clone(&self.icl_warm_cancelled);
        let warm_completed = Arc::clone(&self.icl_warm_completed);
        let warm_total = Arc::clone(&self.icl_warm_total);
        let warm_resident_us = Arc::clone(&self.icl_warm_resident_us);
        let warm_probe_us = Arc::clone(&self.icl_warm_probe_us);
        let warm_total_us = Arc::clone(&self.icl_warm_total_us);
        let mut references = self.icl_references.as_ref().clone();
        references.sort_by_key(|reference| match reference.entry.persona_id.as_str() {
            "gabor_mate" => 0,
            "bessel_van_der_kolk" => 1,
            _ => 2,
        });
        references.truncate(1);
        warm_completed.store(0, Ordering::Release);
        warm_total.store(references.len(), Ordering::Release);
        warm_resident_us.store(0, Ordering::Release);
        warm_probe_us.store(0, Ordering::Release);
        warm_total_us.store(0, Ordering::Release);
        let spawned = std::thread::Builder::new()
            .name("qwen-tts-warm".into())
            .spawn(move || {
                let warm_started = Instant::now();
                let request_id = SpeechRequestId("qwen-startup-warm".into());
                {
                    let Some(mut slot) =
                        lock_background_worker(&worker_slot, &foreground_waiters, &warm_cancelled)
                    else {
                        warm_total_us.store(elapsed_us(warm_started), Ordering::Release);
                        warm_active.store(false, Ordering::Release);
                        return;
                    };
                    if slot.is_none() {
                        let Ok(worker) = ResidentIclWorker::start(&config, &request_id) else {
                            warm_total_us.store(elapsed_us(warm_started), Ordering::Release);
                            warm_active.store(false, Ordering::Release);
                            return;
                        };
                        *slot = Some(worker);
                    }
                }
                warm_resident_us.store(elapsed_us(warm_started), Ordering::Release);
                for reference in references {
                    if warm_cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    // Leave a scheduling window between cache entries. A live
                    // synthesis declares itself before contending for the
                    // worker and must always win the next admission.
                    std::thread::sleep(Duration::from_millis(1));
                    let Some(mut slot) =
                        lock_background_worker(&worker_slot, &foreground_waiters, &warm_cancelled)
                    else {
                        warm_total_us.store(elapsed_us(warm_started), Ordering::Release);
                        warm_active.store(false, Ordering::Release);
                        return;
                    };
                    if slot.is_none() {
                        let Ok(worker) = ResidentIclWorker::start(&config, &request_id) else {
                            continue;
                        };
                        *slot = Some(worker);
                    }
                    let probe_started = Instant::now();
                    let warm_failed = slot
                        .as_mut()
                        .expect("worker initialized")
                        .warm_voice(
                            &request_id,
                            &reference.entry.persona_id,
                            &reference.clip,
                            &reference.entry.clip_sha256,
                            &reference.entry.reference_transcript,
                        )
                        .is_err();
                    warm_probe_us.store(elapsed_us(probe_started), Ordering::Release);
                    if warm_failed && slot.as_mut().is_some_and(|worker| !worker.is_running()) {
                        // A broken pipe belongs to this process generation. A
                        // bad reference belongs only to this voice: the helper
                        // has consumed that complete request and remains useful
                        // for every other persona and the next live synthesis.
                        slot.take();
                    }
                    if !warm_failed {
                        warm_completed.fetch_add(1, Ordering::AcqRel);
                    }
                    drop(slot);
                }
                warm_total_us.store(elapsed_us(warm_started), Ordering::Release);
                warm_active.store(false, Ordering::Release);
            });
        match spawned {
            Ok(handle) => {
                if let Ok(mut thread) = self.icl_warm_thread.lock() {
                    *thread = Some(handle);
                } else {
                    self.icl_warm_cancelled.store(true, Ordering::Release);
                    let _ = handle.join();
                    self.icl_warm_active.store(false, Ordering::Release);
                }
            }
            Err(_) => self.icl_warm_active.store(false, Ordering::Release),
        }
    }

    /// Join the one startup compile probe before the application advertises
    /// resident speech readiness. This is intentionally outside any live
    /// microphone turn: once it returns, startup work cannot compete with
    /// Gemma's continuous audio prefill.
    pub fn wait_for_icl_background_warm(&self) -> Result<IclWarmStatus, SpeechError> {
        let warm_thread = self
            .icl_warm_thread
            .lock()
            .map_err(|_| {
                unavailable(
                    &SpeechRequestId("qwen-startup-warm".into()),
                    "qwen_warm_state_unavailable",
                    "Qwen warm worker state is unavailable",
                )
            })?
            .take();
        if let Some(warm_thread) = warm_thread {
            warm_thread.join().map_err(|_| {
                unavailable(
                    &SpeechRequestId("qwen-startup-warm".into()),
                    "qwen_warm_worker_panicked",
                    "Qwen warm worker stopped unexpectedly",
                )
            })?;
        }
        Ok(IclWarmStatus {
            active: self.icl_warm_active.load(Ordering::Acquire),
            completed: self.icl_warm_completed.load(Ordering::Acquire),
            total: self.icl_warm_total.load(Ordering::Acquire),
            resident_us: self.icl_warm_resident_us.load(Ordering::Acquire),
            probe_us: self.icl_warm_probe_us.load(Ordering::Acquire),
            total_us: self.icl_warm_total_us.load(Ordering::Acquire),
        })
    }
}

fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

struct ForegroundWorkerWait<'a> {
    waiters: &'a AtomicUsize,
}

impl<'a> ForegroundWorkerWait<'a> {
    fn begin(waiters: &'a AtomicUsize) -> Self {
        waiters.fetch_add(1, Ordering::AcqRel);
        Self { waiters }
    }
}

impl Drop for ForegroundWorkerWait<'_> {
    fn drop(&mut self) {
        self.waiters.fetch_sub(1, Ordering::AcqRel);
    }
}

fn lock_background_worker<'a, T>(
    worker_slot: &'a Mutex<T>,
    foreground_waiters: &AtomicUsize,
    cancelled: &AtomicBool,
) -> Option<MutexGuard<'a, T>> {
    loop {
        if cancelled.load(Ordering::Acquire) {
            return None;
        }
        if foreground_waiters.load(Ordering::Acquire) == 0 {
            match worker_slot.try_lock() {
                Ok(slot) if foreground_waiters.load(Ordering::Acquire) == 0 => return Some(slot),
                Ok(slot) => drop(slot),
                Err(TryLockError::Poisoned(_)) => return None,
                Err(TryLockError::WouldBlock) => {}
            }
        }
        // This is startup-only background work. Sleeping avoids stealing a
        // core from inference while bounding handoff delay to one millisecond.
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn model_admitted(model_dir: &Path) -> bool {
    [
        "config.json",
        "generation_config.json",
        "model.safetensors",
        "model.safetensors.index.json",
        "tokenizer_config.json",
        "vocab.json",
        "merges.txt",
        "speech_tokenizer/config.json",
        "speech_tokenizer/configuration.json",
        "speech_tokenizer/model.safetensors",
        "speech_tokenizer/preprocessor_config.json",
    ]
    .into_iter()
    .all(|relative| {
        let artifact = model_dir.join(relative);
        let metadata = model_dir
            .join(".cache/huggingface/download")
            .join(format!("{relative}.metadata"));
        artifact.metadata().is_ok_and(|value| value.len() > 0)
            && fs::read_to_string(metadata)
                .is_ok_and(|value| value.lines().next() == Some(QWEN_MODEL_REVISION))
    })
}

fn discover_icl_config(
    managed_model_root: Option<&Path>,
    installed_reference_root: Option<&Path>,
    helper: &Path,
) -> Option<QwenIclConfig> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let default_model = home
        .join("Library/Caches/delysis/voice-ab/models")
        .join(QWEN_ICL_MODEL_DIRECTORY);
    let model_dir = std::env::var_os("MOM_QWEN3_TTS_BASE_MODEL")
        .map(PathBuf::from)
        .unwrap_or(default_model);
    let colocated_reference_root = managed_model_root
        .and_then(Path::parent)
        .map(|data_dir| data_dir.join("speech-voices/qwen-icl"))
        .filter(|root| root.join("manifest.json").is_file());
    let installed_reference_root = installed_reference_root
        .map(Path::to_path_buf)
        .filter(|root| root.join("manifest.json").is_file());
    let development_reference_root = if cfg!(debug_assertions) {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .find(|path| {
                path.join("products/mom/artifacts/voice-samples/manifest.json")
                    .is_file()
            })
            .map(|root| root.join("products/mom/artifacts/voice-samples"))
    } else {
        None
    };
    let reference_root = std::env::var_os("MOM_QWEN3_TTS_VOICE_REFERENCE_ROOT")
        .map(PathBuf::from)
        .or(installed_reference_root)
        .or(colocated_reference_root)
        .or(development_reference_root)?;
    Some(QwenIclConfig {
        helper: helper.to_path_buf(),
        model_dir,
        reference_root,
    })
}

#[derive(Debug, Deserialize)]
struct ReferenceManifest {
    distribution: String,
    entries: Vec<ReferenceEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReferenceEntry {
    persona_id: String,
    speaker: String,
    status: String,
    clip_path: String,
    clip_sha256: String,
    reference_transcript: String,
    speaker_turn_verification: SpeakerTurnVerification,
}

#[derive(Debug, Clone, Deserialize)]
struct SpeakerTurnVerification {
    status: String,
}

fn manifest_entries(config: &QwenIclConfig) -> Vec<ReferenceEntry> {
    let manifest_path = config.reference_root.join("manifest.json");
    let Ok(bytes) = fs::read(manifest_path) else {
        return Vec::new();
    };
    let Ok(manifest) = serde_json::from_slice::<ReferenceManifest>(&bytes) else {
        return Vec::new();
    };
    if !manifest.distribution.starts_with("local-only") {
        return Vec::new();
    }
    manifest.entries
}

fn admitted_icl_references(config: &QwenIclConfig) -> Vec<AdmittedIclReference> {
    manifest_entries(config)
        .into_iter()
        .filter_map(|entry| {
            entry_is_admitted(config, &entry)
                .ok()
                .map(|clip| AdmittedIclReference { entry, clip })
        })
        .collect()
}

fn admitted_icl_voices(
    config: &QwenIclConfig,
    references: &[AdmittedIclReference],
) -> Vec<VoiceDescriptor> {
    if !helper_supports_icl_protocol(&config.helper) || !base_model_admitted(&config.model_dir) {
        return Vec::new();
    }
    references
        .iter()
        .map(|reference| VoiceDescriptor {
            id: icl_voice_id(&reference.entry.persona_id),
            name: reference.entry.speaker.clone(),
            language: "en-US".into(),
            gender: None,
            quality: Some(VoiceQuality::Premium),
            expected_latency: Some(LatencyClass::High),
            network: NetworkBehavior::Never,
            installed: true,
        })
        .collect()
}

fn helper_supports_icl_protocol(helper: &Path) -> bool {
    if !helper.is_file() {
        return false;
    }
    Command::new(helper)
        .arg("--protocol")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == ICL_HELPER_PROTOCOL
        })
}

fn base_model_admitted(model_dir: &Path) -> bool {
    ["config.json", "model.safetensors", "tokenizer_config.json"]
        .into_iter()
        .all(|relative| {
            model_dir
                .join(relative)
                .metadata()
                .is_ok_and(|metadata| metadata.len() > 0)
        })
}

fn icl_voice_id(persona_id: &str) -> String {
    format!("{ICL_VOICE_PREFIX}{persona_id}")
}

fn persona_key_from_voice_id(voice_id: &str) -> Option<&str> {
    voice_id.strip_prefix(ICL_VOICE_PREFIX).filter(|key| {
        !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

fn entry_is_admitted(
    config: &QwenIclConfig,
    entry: &ReferenceEntry,
) -> Result<PathBuf, &'static str> {
    if entry.status != "collected"
        || entry.speaker_turn_verification.status != "verified_for_local_evaluation"
        || entry.reference_transcript.trim().is_empty()
        || entry.clip_sha256.len() != 64
        || !entry
            .clip_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("reference entry lacks local-evaluation admission fields");
    }
    let relative = Path::new(&entry.clip_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("reference clip path is not a safe relative path");
    }
    let root =
        fs::canonicalize(&config.reference_root).map_err(|_| "reference root unavailable")?;
    let clip = fs::canonicalize(root.join(relative)).map_err(|_| "reference clip unavailable")?;
    if !clip.starts_with(&root) {
        return Err("reference clip escapes reference root");
    }
    let bytes = fs::read(&clip).map_err(|_| "reference clip unreadable")?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if !digest.eq_ignore_ascii_case(&entry.clip_sha256) {
        return Err("reference clip hash mismatch");
    }
    Ok(clip)
}

fn resolve_icl_entry(
    references: &[AdmittedIclReference],
    voice_id: &str,
) -> Option<AdmittedIclReference> {
    let key = persona_key_from_voice_id(voice_id)?;
    references
        .iter()
        .find(|reference| normalize_persona_key(&reference.entry.persona_id) == key)
        .cloned()
}

fn normalize_persona_key(persona_id: &str) -> &str {
    match persona_id {
        "arnold_popky_detur" => "arnold_popky",
        "robert_miller_fsap" => "robert_miller",
        "shirley_jean_schmidt_dnms" => "shirley_jean_schmidt",
        other => other,
    }
}

struct Cancellation {
    active: Arc<Mutex<HashMap<SpeechRequestId, Arc<AtomicBool>>>>,
}

impl SpeechCancellation for Cancellation {
    fn cancel(&self, request_id: &SpeechRequestId) -> usize {
        self.active
            .lock()
            .ok()
            .and_then(|map| map.get(request_id).cloned())
            .map_or(0, |flag| {
                flag.store(true, Ordering::Release);
                1
            })
    }
}

#[async_trait]
impl SpeechBackend for QwenSpeechBackend {
    fn descriptor(&self) -> SpeechBackendDescriptor {
        self.descriptor.clone()
    }
    fn readiness(&self) -> SpeechBackendReadiness {
        self.descriptor.readiness.clone()
    }

    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionTicket, SpeechError> {
        Err(error(
            &request.context.request_id,
            "qwen_tts_transcription_unsupported",
            SpeechErrorClass::Capability,
            false,
            "Qwen3 TTS does not transcribe audio",
        ))
    }

    async fn synthesize(&self, request: SynthesisRequest) -> Result<SynthesisTicket, SpeechError> {
        validate_request(&request, &self.descriptor)?;
        let request_id = request.context.request_id.clone();
        let route = request_route(&request);
        let (events_tx, events_rx) = mpsc::channel(DEFAULT_SPEECH_EVENT_CAPACITY);
        let (final_tx, final_rx) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.active
            .lock()
            .map_err(|_| {
                unavailable(
                    &request_id,
                    "qwen_state_unavailable",
                    "Qwen speech state is unavailable",
                )
            })?
            .insert(request_id.clone(), Arc::clone(&cancelled));
        let active = Arc::clone(&self.active);
        let icl_worker = Arc::clone(&self.icl_worker);
        let icl_references = Arc::clone(&self.icl_references);
        let icl_foreground_waiters = Arc::clone(&self.icl_foreground_waiters);
        let icl_warm_active = Arc::clone(&self.icl_warm_active);
        let icl_warm_completed = Arc::clone(&self.icl_warm_completed);
        let icl_warm_total = Arc::clone(&self.icl_warm_total);
        let config = self.config.clone();
        let worker_id = request_id.clone();
        let cancellation = Arc::clone(&cancelled);
        self.tasks
            .spawn_blocking(format!("qwen-tts:{request_id}"), move || {
                let result = if request.stream {
                    synthesize_streaming_icl(
                        &request,
                        &config,
                        &icl_references,
                        &route,
                        &cancellation,
                        &events_tx,
                        &icl_worker,
                        &icl_foreground_waiters,
                        &icl_warm_active,
                        &icl_warm_completed,
                        &icl_warm_total,
                    )
                } else {
                    synthesize_complete(
                        &request,
                        &config,
                        &icl_references,
                        &route,
                        &cancellation,
                        &events_tx,
                    )
                };
                let _ = match &result {
                    Ok(response) => events_tx.blocking_send(SynthesisEvent::Completed {
                        request_id: worker_id.clone(),
                        response: response.clone(),
                    }),
                    Err(err) if err.class == SpeechErrorClass::Cancelled => events_tx
                        .blocking_send(SynthesisEvent::Cancelled {
                            request_id: worker_id.clone(),
                            usage: SpeechUsage::default(),
                        }),
                    Err(err) => events_tx.blocking_send(SynthesisEvent::Failed {
                        request_id: worker_id.clone(),
                        error: err.clone(),
                    }),
                };
                let _ = final_tx.send(result);
                if let Ok(mut map) = active.lock() {
                    map.remove(&worker_id);
                }
                Ok(())
            })
            .map_err(|e| unavailable(&request_id, "qwen_worker_unavailable", &e.to_string()))?;
        Ok(SynthesisTicket::new(
            request_id,
            events_rx,
            final_rx,
            Arc::new(Cancellation {
                active: Arc::clone(&self.active),
            }),
        ))
    }

    fn cancel(&self, request_id: &SpeechRequestId) -> usize {
        Cancellation {
            active: Arc::clone(&self.active),
        }
        .cancel(request_id)
    }

    async fn shutdown(&self) -> Result<(), SpeechError> {
        self.icl_warm_cancelled.store(true, Ordering::Release);
        if let Ok(map) = self.active.lock() {
            for flag in map.values() {
                flag.store(true, Ordering::Release);
            }
        }
        self.tasks.begin_shutdown().map_err(|e| {
            unavailable(
                &SpeechRequestId("qwen-shutdown".into()),
                "qwen_shutdown_failed",
                &e.to_string(),
            )
        })?;
        self.tasks.wait_for_idle().await.map_err(|e| {
            unavailable(
                &SpeechRequestId("qwen-shutdown".into()),
                "qwen_shutdown_failed",
                &e.to_string(),
            )
        })?;
        let warm_thread = self
            .icl_warm_thread
            .lock()
            .map_err(|_| {
                unavailable(
                    &SpeechRequestId("qwen-shutdown".into()),
                    "qwen_shutdown_failed",
                    "Qwen warm worker state is unavailable",
                )
            })?
            .take();
        if let Some(warm_thread) = warm_thread {
            tokio::task::spawn_blocking(move || warm_thread.join())
                .await
                .map_err(|error| {
                    unavailable(
                        &SpeechRequestId("qwen-shutdown".into()),
                        "qwen_shutdown_failed",
                        &error.to_string(),
                    )
                })?
                .map_err(|_| {
                    unavailable(
                        &SpeechRequestId("qwen-shutdown".into()),
                        "qwen_shutdown_failed",
                        "Qwen warm worker panicked",
                    )
                })?;
        }
        if let Ok(mut worker) = self.icl_worker.lock() {
            worker.take();
        }
        Ok(())
    }
}

fn validate_request(
    request: &SynthesisRequest,
    descriptor: &SpeechBackendDescriptor,
) -> Result<(), SpeechError> {
    request.validate()?;
    let voice_id = match &request.voice {
        VoiceSelector::Exact { voice_id } => voice_id.as_str(),
        _ => "",
    };
    let is_icl_voice = persona_key_from_voice_id(voice_id).is_some();
    let route_ready = if is_icl_voice {
        descriptor
            .voices
            .iter()
            .any(|voice| voice.id == voice_id && voice.installed)
    } else {
        descriptor
            .voices
            .iter()
            .any(|voice| voice.id == QWEN_VOICE_ID && voice.installed)
    };
    if !route_ready {
        return Err(unavailable(
            &request.context.request_id,
            "qwen_assets_unavailable",
            "Selected Qwen3 TTS route or its local assets are unavailable",
        ));
    }
    let valid_stream_output = request.output
        == (AudioOutputFormat::Pcm {
            format: speech_native_types::PcmFormat {
                sample_rate_hz: ICL_SAMPLE_RATE_HZ,
                channels: 1,
                sample_format: speech_native_types::PcmSampleFormat::I16Le,
                interleaved: true,
            },
        });
    if (request.stream && (!is_icl_voice || !valid_stream_output))
        || (!request.stream && request.output != AudioOutputFormat::Wav)
        || !matches!(request.input, SynthesisInput::Text { .. })
        || request.alignment != speech_native_types::AlignmentGranularity::None
    {
        return Err(error(
            &request.context.request_id,
            "qwen_request_unsupported",
            SpeechErrorClass::Capability,
            false,
            "Qwen3 TTS accepts plain text; Base ICL streams 24 kHz mono PCM and other routes return WAV",
        ));
    }
    if let SpeechRouteSelector::ExactBackend {
        backend_id,
        model_id,
        voice_id,
    } = &request.context.route
        && (backend_id != QWEN_BACKEND_ID
            || model_id.as_deref()
                != Some(if is_icl_voice {
                    QWEN_ICL_MODEL_ID
                } else {
                    QWEN_MODEL_ID
                })
            || voice_id.as_deref() != Some(voice_id_from_selector(request).unwrap_or("")))
    {
        return Err(error(
            &request.context.request_id,
            "qwen_route_mismatch",
            SpeechErrorClass::Capability,
            false,
            "Qwen3 TTS request selected a different route",
        ));
    }
    if !matches!(&request.voice, VoiceSelector::Exact { voice_id } if voice_id == QWEN_VOICE_ID || persona_key_from_voice_id(voice_id).is_some())
    {
        return Err(SpeechError::invalid_request(
            &request.context.request_id,
            "qwen_voice_mismatch",
            "Qwen3 TTS requires an admitted local voice",
        ));
    }
    Ok(())
}

fn synthesize_complete(
    request: &SynthesisRequest,
    config: &QwenBackendConfig,
    references: &[AdmittedIclReference],
    route: &SpeechResolvedRoute,
    cancelled: &AtomicBool,
    events: &mpsc::Sender<SynthesisEvent>,
) -> Result<SynthesisResponse, SpeechError> {
    let request_id = &request.context.request_id;
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error(request_id));
    }
    events
        .blocking_send(SynthesisEvent::Started {
            request_id: request_id.clone(),
            route: route.clone(),
        })
        .map_err(|_| cancelled_error(request_id))?;
    let started = Instant::now();
    let text = match &request.input {
        SynthesisInput::Text { text } => text,
        SynthesisInput::Ssml { .. } => unreachable!(),
    };
    let audio = if let Some(voice_id) = voice_id_from_selector(request)
        .filter(|voice_id| persona_key_from_voice_id(voice_id).is_some())
    {
        let icl = config.icl.as_ref().ok_or_else(|| {
            unavailable(
                request_id,
                "qwen_icl_unavailable",
                "Local Qwen voice references are unavailable",
            )
        })?;
        let reference = resolve_icl_entry(references, voice_id).ok_or_else(|| {
            unavailable(
                request_id,
                "qwen_icl_reference_invalid",
                "The selected local reference failed manifest, transcript, or hash validation",
            )
        })?;
        let clip = entry_is_admitted(icl, &reference.entry).map_err(|_| {
            unavailable(
                request_id,
                "qwen_icl_reference_invalid",
                "The selected local reference changed after admission",
            )
        })?;
        let output = tempfile::Builder::new()
            .prefix("mom-qwen-icl-")
            .suffix(".wav")
            .tempfile()
            .map_err(|e| unavailable(request_id, "qwen_output_unavailable", &e.to_string()))?;
        let status = Command::new(&icl.helper)
            .arg("--icl")
            .arg(&icl.model_dir)
            .arg(clip)
            .arg(&reference.entry.reference_transcript)
            .arg(output.path())
            .arg(text)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| unavailable(request_id, "qwen_icl_launch_failed", &e.to_string()))?;
        if !status.success() {
            return Err(unavailable(
                request_id,
                "qwen_icl_synthesis_failed",
                "The bundled Swift Qwen Base ICL helper did not complete synthesis",
            ));
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(cancelled_error(request_id));
        }
        let metadata = fs::metadata(output.path())
            .map_err(|e| unavailable(request_id, "qwen_output_read_failed", &e.to_string()))?;
        if metadata.len() == 0 || metadata.len() > ICL_MAX_OUTPUT_BYTES {
            return Err(unavailable(
                request_id,
                "qwen_icl_output_invalid",
                "Qwen Base ICL WAV exceeded the local output size limit",
            ));
        }
        fs::read(output.path())
            .map_err(|e| unavailable(request_id, "qwen_output_read_failed", &e.to_string()))?
    } else {
        let output = tempfile::Builder::new()
            .prefix("mom-qwen-")
            .suffix(".wav")
            .tempfile()
            .map_err(|e| unavailable(request_id, "qwen_output_unavailable", &e.to_string()))?;
        let status = Command::new(&config.helper)
            .arg(&config.model_dir)
            .arg(output.path())
            .arg(VOICE_DESCRIPTION)
            .arg(text)
            .status()
            .map_err(|e| unavailable(request_id, "qwen_helper_launch_failed", &e.to_string()))?;
        if !status.success() {
            return Err(unavailable(
                request_id,
                "qwen_synthesis_failed",
                "The local Qwen3 TTS helper did not complete synthesis",
            ));
        }
        fs::read(output.path())
            .map_err(|e| unavailable(request_id, "qwen_output_read_failed", &e.to_string()))?
    };
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error(request_id));
    }
    let duration_ms = wav_duration_ms(&audio).ok_or_else(|| {
        unavailable(
            request_id,
            "qwen_wav_invalid",
            "Qwen3 TTS returned an invalid WAV",
        )
    })?;
    let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(SynthesisResponse {
        request_id: request_id.clone(),
        route: route.clone(),
        output: SynthesisOutput::Complete {
            audio,
            format: AudioOutputFormat::Wav,
        },
        duration_ms: Some(duration_ms),
        alignments: Vec::new(),
        usage: SpeechUsage {
            output_audio_ms: Some(duration_ms),
            input_characters: Some(u64::try_from(text.chars().count()).unwrap_or(u64::MAX)),
            total_ms: Some(total_ms),
            provenance: UsageProvenance::Exact,
            real_local_inference: true,
            ..SpeechUsage::default()
        },
    })
}

fn synthesize_streaming_icl(
    request: &SynthesisRequest,
    config: &QwenBackendConfig,
    references: &[AdmittedIclReference],
    route: &SpeechResolvedRoute,
    cancelled: &AtomicBool,
    events: &mpsc::Sender<SynthesisEvent>,
    worker_slot: &Mutex<Option<ResidentIclWorker>>,
    foreground_waiters: &AtomicUsize,
    warm_active: &AtomicBool,
    warm_completed: &AtomicUsize,
    warm_total: &AtomicUsize,
) -> Result<SynthesisResponse, SpeechError> {
    let started = Instant::now();
    let request_id = &request.context.request_id;
    let voice_id = voice_id_from_selector(request).ok_or_else(|| {
        SpeechError::invalid_request(
            request_id,
            "qwen_voice_mismatch",
            "An exact persona voice is required",
        )
    })?;
    let icl = config.icl.as_ref().ok_or_else(|| {
        unavailable(
            request_id,
            "qwen_icl_unavailable",
            "Local Qwen voice references are unavailable",
        )
    })?;
    let reference_started = Instant::now();
    let reference = resolve_icl_entry(references, voice_id).ok_or_else(|| {
        unavailable(
            request_id,
            "qwen_icl_reference_invalid",
            "The selected local reference failed manifest, transcript, or hash validation",
        )
    })?;
    let reference_admission_us =
        u64::try_from(reference_started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let text = match &request.input {
        SynthesisInput::Text { text } => text,
        SynthesisInput::Ssml { .. } => unreachable!(),
    };
    events
        .blocking_send(SynthesisEvent::Started {
            request_id: request_id.clone(),
            route: route.clone(),
        })
        .map_err(|_| cancelled_error(request_id))?;
    let warm_active_at_dispatch = warm_active.load(Ordering::Acquire);
    let warm_completed_at_dispatch = warm_completed.load(Ordering::Acquire);
    let warm_total_at_dispatch = warm_total.load(Ordering::Acquire);
    let worker_lock_started = Instant::now();
    let foreground_wait = ForegroundWorkerWait::begin(foreground_waiters);
    let mut worker = worker_slot.lock().map_err(|_| {
        unavailable(
            request_id,
            "qwen_icl_worker_state_unavailable",
            "The resident Qwen worker is unavailable",
        )
    })?;
    drop(foreground_wait);
    let queue_elapsed = worker_lock_started.elapsed();
    let queue_ms = u64::try_from(queue_elapsed.as_millis()).unwrap_or(u64::MAX);
    let queue_us = u64::try_from(queue_elapsed.as_micros()).unwrap_or(u64::MAX);
    let mut model_load_ms = None;
    if worker.is_none() {
        let model_load_started = Instant::now();
        *worker = Some(ResidentIclWorker::start(icl, request_id)?);
        model_load_ms =
            Some(u64::try_from(model_load_started.elapsed().as_millis()).unwrap_or(u64::MAX));
    }
    let result = worker.as_mut().expect("worker initialized").synthesize(
        request_id,
        text,
        &reference.entry.persona_id,
        &reference.clip,
        &reference.entry.clip_sha256,
        &reference.entry.reference_transcript,
        cancelled,
        events,
        started,
    );
    if result
        .as_ref()
        .is_err_and(|error| !worker_is_reusable_after(error))
    {
        worker.take();
    }
    let result = result?;
    let sample_count = result.sample_count;
    let duration_ms = sample_count.saturating_mul(1000) / u64::from(ICL_SAMPLE_RATE_HZ);
    Ok(SynthesisResponse {
        request_id: request_id.clone(),
        route: route.clone(),
        output: SynthesisOutput::Streamed {
            format: request.output.clone(),
            emitted_bytes: sample_count.saturating_mul(2),
        },
        duration_ms: Some(duration_ms),
        alignments: Vec::new(),
        usage: SpeechUsage {
            output_audio_ms: Some(duration_ms),
            input_characters: Some(u64::try_from(text.chars().count()).unwrap_or(u64::MAX)),
            queue_ms: Some(queue_ms),
            queue_us: Some(queue_us),
            model_load_ms,
            background_warm_active: Some(warm_active_at_dispatch),
            background_warm_completed: Some(
                u64::try_from(warm_completed_at_dispatch).unwrap_or(u64::MAX),
            ),
            background_warm_total: Some(u64::try_from(warm_total_at_dispatch).unwrap_or(u64::MAX)),
            reference_admission_us: Some(reference_admission_us),
            reference_cache: result.reference_cache,
            reference_us: result.reference_us,
            reference_identity_us: result.reference_identity_us,
            reference_memory_lookup_us: result.reference_memory_lookup_us,
            reference_disk_lookup_us: result.reference_disk_lookup_us,
            reference_source_read_us: result.reference_source_read_us,
            reference_source_hash_us: result.reference_source_hash_us,
            reference_compute_us: result.reference_compute_us,
            reference_persist_us: result.reference_persist_us,
            helper_first_pcm_us: result.helper_first_pcm_us,
            helper_text_chunk_us: result.helper_text_chunk_us,
            helper_code_generation_us: result.helper_code_generation_us,
            helper_prepared_prefix_cache_hit: result.helper_prepared_prefix_cache_hit,
            helper_first_decode_us: result.helper_first_decode_us,
            time_to_first_result_ms: Some(result.time_to_first_audio_ms),
            total_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            provenance: UsageProvenance::Exact,
            real_local_inference: true,
            ..SpeechUsage::default()
        },
    })
}

fn worker_is_reusable_after(error: &SpeechError) -> bool {
    // `error` and `fatal` are complete terminal protocol messages for the
    // submitted job. All other failures may have unread audio or a broken
    // pipe, so reusing that process could bind stale PCM to the next request.
    error.code == "qwen_icl_synthesis_failed" || error.code == "qwen_icl_cancelled_clean"
}

impl ResidentIclWorker {
    fn is_running(&mut self) -> bool {
        self.child.try_wait().is_ok_and(|status| status.is_none())
    }

    fn start(config: &QwenIclConfig, request_id: &SpeechRequestId) -> Result<Self, SpeechError> {
        let mut child = Command::new(&config.helper)
            .arg("--resident")
            .arg(&config.model_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| unavailable(request_id, "qwen_icl_launch_failed", &e.to_string()))?;
        let input = child.stdin.take().ok_or_else(|| {
            unavailable(
                request_id,
                "qwen_icl_launch_failed",
                "Could not open resident worker input",
            )
        })?;
        let output = child.stdout.take().ok_or_else(|| {
            unavailable(
                request_id,
                "qwen_icl_launch_failed",
                "Could not open resident worker output",
            )
        })?;
        let mut worker = Self {
            child,
            input: Arc::new(Mutex::new(BufWriter::new(input))),
            output: BufReader::new(output),
            sample_rate_hz: 0,
        };
        let ready = worker.read_message(request_id)?;
        if ready.event != "ready" || ready.sample_rate != Some(ICL_SAMPLE_RATE_HZ) {
            return Err(unavailable(
                request_id,
                "qwen_icl_worker_protocol_error",
                "The resident Qwen worker did not report the expected 24 kHz model",
            ));
        }
        worker.sample_rate_hz = ICL_SAMPLE_RATE_HZ;
        Ok(worker)
    }

    fn synthesize(
        &mut self,
        request_id: &SpeechRequestId,
        text: &str,
        persona_id: &str,
        clip: &Path,
        clip_sha256: &str,
        transcript: &str,
        cancelled: &AtomicBool,
        events: &mpsc::Sender<SynthesisEvent>,
        started: Instant,
    ) -> Result<IclSynthesisResult, SpeechError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(clean_icl_cancelled_error(request_id));
        }
        let clip_path = clip.to_string_lossy();
        let job = IclWorkerJob {
            id: request_id.0.as_str(),
            text,
            persona_id,
            ref_audio: &clip_path,
            ref_sha256: clip_sha256,
            ref_text: transcript,
            warm_only: false,
        };
        write_worker_command(&self.input, request_id, &job)?;

        let format = speech_native_types::PcmFormat {
            sample_rate_hz: self.sample_rate_hz,
            channels: 1,
            sample_format: speech_native_types::PcmSampleFormat::I16Le,
            interleaved: true,
        };
        let mut sequence = 0_u64;
        let mut sample_offset = 0_u64;
        let mut time_to_first_audio_ms = None;
        let mut reference_cache = None;
        let mut reference_us = None;
        let mut reference_identity_us = None;
        let mut reference_memory_lookup_us = None;
        let mut reference_disk_lookup_us = None;
        let mut reference_source_read_us = None;
        let mut reference_source_hash_us = None;
        let mut reference_compute_us = None;
        let mut reference_persist_us = None;
        let mut helper_first_pcm_us = None;
        let mut helper_text_chunk_us = None;
        let mut helper_code_generation_us = None;
        let mut helper_prepared_prefix_cache_hit = None;
        let mut helper_first_decode_us = None;
        let cancel_input = Arc::clone(&self.input);
        let watcher_done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while !watcher_done.load(Ordering::Acquire) {
                    if cancelled.load(Ordering::Acquire) {
                        let command = IclWorkerCancel {
                            op: "cancel",
                            id: request_id.0.as_str(),
                        };
                        let _ = write_worker_command(&cancel_input, request_id, &command);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            });
            let result = (|| loop {
                let message = self.read_message(request_id)?;
                if message.id.as_deref().is_some_and(|id| id != request_id.0) {
                    return Err(unavailable(
                        request_id,
                        "qwen_icl_worker_protocol_error",
                        "The resident Qwen worker returned audio for a different request",
                    ));
                }
                match message.event.as_str() {
                    "audio" => {
                        let first_audio_ms =
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                        if message.sample_rate != Some(self.sample_rate_hz) {
                            return Err(unavailable(
                                request_id,
                                "qwen_icl_audio_invalid",
                                "Qwen returned an unexpected sample rate",
                            ));
                        }
                        let encoded = message.data.as_deref().ok_or_else(|| {
                            unavailable(
                                request_id,
                                "qwen_icl_audio_invalid",
                                "Qwen returned an empty audio packet",
                            )
                        })?;
                        let data = base64::engine::general_purpose::STANDARD
                            .decode(encoded)
                            .map_err(|_| {
                                unavailable(
                                    request_id,
                                    "qwen_icl_audio_invalid",
                                    "Qwen returned an invalid audio packet",
                                )
                            })?;
                        if data.is_empty()
                            || data.len() > ICL_MAX_CHUNK_BYTES
                            || !data.len().is_multiple_of(2)
                        {
                            return Err(unavailable(
                                request_id,
                                "qwen_icl_audio_invalid",
                                "Qwen audio packet exceeded bounds or was misaligned",
                            ));
                        }
                        let samples = u64::try_from(data.len() / 2).unwrap_or(u64::MAX);
                        if message.samples != Some(samples) {
                            return Err(unavailable(
                                request_id,
                                "qwen_icl_audio_invalid",
                                "Qwen audio packet sample count did not match its data",
                            ));
                        }
                        if sequence == 0 {
                            if message.reference_cache.as_deref().is_some_and(|source| {
                                !matches!(source, "memory" | "disk" | "computed")
                            }) {
                                return Err(unavailable(
                                    request_id,
                                    "qwen_icl_worker_protocol_error",
                                    "Qwen returned an unknown reference cache source",
                                ));
                            }
                            reference_cache = message.reference_cache;
                            reference_us = message.reference_us;
                            reference_identity_us = message.reference_identity_us;
                            reference_memory_lookup_us = message.reference_memory_lookup_us;
                            reference_disk_lookup_us = message.reference_disk_lookup_us;
                            reference_source_read_us = message.reference_source_read_us;
                            reference_source_hash_us = message.reference_source_hash_us;
                            reference_compute_us = message.reference_compute_us;
                            reference_persist_us = message.reference_persist_us;
                            helper_first_pcm_us = message.first_pcm_us;
                            helper_text_chunk_us = message.text_chunk_us;
                            helper_code_generation_us = message.code_generation_us;
                            helper_prepared_prefix_cache_hit = message.prepared_prefix_cache_hit;
                            helper_first_decode_us = message.first_decode_us;
                        }
                        let chunk = speech_native_types::AudioChunk {
                            sequence,
                            sample_offset,
                            format: format.clone(),
                            data,
                            end_of_stream: false,
                        };
                        chunk.validate(request_id)?;
                        events
                            .blocking_send(SynthesisEvent::Audio {
                                request_id: request_id.clone(),
                                chunk,
                            })
                            .map_err(|_| cancelled_error(request_id))?;
                        time_to_first_audio_ms.get_or_insert(first_audio_ms);
                        sequence = sequence.saturating_add(1);
                        sample_offset = sample_offset.saturating_add(samples);
                    }
                    "done" => {
                        if sample_offset == 0 {
                            return Err(unavailable(
                                request_id,
                                "qwen_icl_audio_empty",
                                "Qwen returned no streaming audio",
                            ));
                        }
                        events
                            .blocking_send(SynthesisEvent::Audio {
                                request_id: request_id.clone(),
                                chunk: speech_native_types::AudioChunk {
                                    sequence,
                                    sample_offset,
                                    format,
                                    data: Vec::new(),
                                    end_of_stream: true,
                                },
                            })
                            .map_err(|_| cancelled_error(request_id))?;
                        return Ok(IclSynthesisResult {
                            sample_count: sample_offset,
                            time_to_first_audio_ms: time_to_first_audio_ms
                                .expect("nonempty audio has a first packet"),
                            reference_cache,
                            reference_us,
                            reference_identity_us,
                            reference_memory_lookup_us,
                            reference_disk_lookup_us,
                            reference_source_read_us,
                            reference_source_hash_us,
                            reference_compute_us,
                            reference_persist_us,
                            helper_first_pcm_us,
                            helper_text_chunk_us,
                            helper_code_generation_us,
                            helper_prepared_prefix_cache_hit,
                            helper_first_decode_us,
                        });
                    }
                    "cancelled" => return Err(clean_icl_cancelled_error(request_id)),
                    "error" | "fatal" => {
                        return Err(unavailable(
                            request_id,
                            "qwen_icl_synthesis_failed",
                            message
                                .detail
                                .as_deref()
                                .unwrap_or("The resident Qwen worker failed synthesis"),
                        ));
                    }
                    _ => {
                        return Err(unavailable(
                            request_id,
                            "qwen_icl_worker_protocol_error",
                            "The resident Qwen worker returned an unknown event",
                        ));
                    }
                }
            })();
            watcher_done.store(true, Ordering::Release);
            result
        })
    }

    fn warm_voice(
        &mut self,
        request_id: &SpeechRequestId,
        persona_id: &str,
        clip: &Path,
        clip_sha256: &str,
        transcript: &str,
    ) -> Result<(), SpeechError> {
        let clip_path = clip.to_string_lossy();
        let job = IclWorkerJob {
            id: request_id.0.as_str(),
            text: "",
            persona_id,
            ref_audio: &clip_path,
            ref_sha256: clip_sha256,
            ref_text: transcript,
            warm_only: true,
        };
        write_worker_command(&self.input, request_id, &job)?;
        let message = self.read_message(request_id)?;
        if message.event == "warmed" && message.id.as_deref() == Some(request_id.0.as_str()) {
            Ok(())
        } else {
            Err(unavailable(
                request_id,
                "qwen_icl_worker_protocol_error",
                message
                    .detail
                    .as_deref()
                    .unwrap_or("Qwen voice warmup failed"),
            ))
        }
    }

    fn read_message(
        &mut self,
        request_id: &SpeechRequestId,
    ) -> Result<IclWorkerMessage, SpeechError> {
        let mut line = String::new();
        let bytes = self
            .output
            .read_line(&mut line)
            .map_err(|e| unavailable(request_id, "qwen_icl_worker_read_failed", &e.to_string()))?;
        if bytes == 0 || line.len() > ICL_MAX_CHUNK_BYTES * 2 {
            return Err(unavailable(
                request_id,
                "qwen_icl_worker_protocol_error",
                "The resident Qwen worker closed or exceeded protocol bounds",
            ));
        }
        serde_json::from_str(&line).map_err(|_| {
            unavailable(
                request_id,
                "qwen_icl_worker_protocol_error",
                "The resident Qwen worker returned malformed protocol data",
            )
        })
    }
}

fn write_worker_command<T: Serialize>(
    input: &Arc<Mutex<BufWriter<ChildStdin>>>,
    request_id: &SpeechRequestId,
    command: &T,
) -> Result<(), SpeechError> {
    let mut input = input.lock().map_err(|_| {
        unavailable(
            request_id,
            "qwen_icl_worker_write_failed",
            "The resident Qwen worker input is unavailable",
        )
    })?;
    serde_json::to_writer(&mut *input, command)
        .map_err(|e| unavailable(request_id, "qwen_icl_worker_write_failed", &e.to_string()))?;
    input
        .write_all(b"\n")
        .and_then(|()| input.flush())
        .map_err(|e| unavailable(request_id, "qwen_icl_worker_write_failed", &e.to_string()))
}

impl Drop for ResidentIclWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wav_duration_ms(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let riff_len = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
    if riff_len.checked_add(8)? != bytes.len() {
        return None;
    }
    let mut position = 12usize;
    let mut format = None;
    let mut data_len = None;
    while position.checked_add(8)? <= bytes.len() {
        let chunk_id = &bytes[position..position + 4];
        let chunk_len =
            u32::from_le_bytes(bytes[position + 4..position + 8].try_into().ok()?) as usize;
        let chunk_start = position.checked_add(8)?;
        let chunk_end = chunk_start.checked_add(chunk_len)?;
        if chunk_end > bytes.len() {
            return None;
        }
        if chunk_id == b"fmt " {
            if chunk_len < 16 {
                return None;
            }
            let value = &bytes[chunk_start..chunk_end];
            format = Some((
                u16::from_le_bytes(value[0..2].try_into().ok()?),
                u16::from_le_bytes(value[2..4].try_into().ok()?),
                u32::from_le_bytes(value[4..8].try_into().ok()?),
                u32::from_le_bytes(value[8..12].try_into().ok()?),
                u16::from_le_bytes(value[12..14].try_into().ok()?),
                u16::from_le_bytes(value[14..16].try_into().ok()?),
            ));
        } else if chunk_id == b"data" {
            data_len = Some(chunk_len);
        }
        position = chunk_end.checked_add(chunk_len % 2)?;
    }
    let (encoding, channels, rate, byte_rate, block_align, bits) = format?;
    let data_len = data_len? as u64;
    if encoding != 1
        || rate != 24_000
        || channels != 1
        || bits != 16
        || byte_rate != rate * u32::from(channels) * u32::from(bits) / 8
        || block_align != channels * bits / 8
        || data_len == 0
        || !data_len.is_multiple_of(2)
    {
        return None;
    }
    Some(data_len.saturating_mul(1000) / u64::from(byte_rate))
}

#[cfg(test)]
#[cfg(test)]
fn route() -> SpeechResolvedRoute {
    SpeechResolvedRoute {
        backend_id: QWEN_BACKEND_ID.into(),
        model_id: Some(QWEN_MODEL_ID.into()),
        voice_id: Some(QWEN_VOICE_ID.into()),
        backend_kind: SpeechBackendKind::EmbeddedModel,
        network: NetworkBehavior::Never,
    }
}
fn request_route(request: &SynthesisRequest) -> SpeechResolvedRoute {
    let voice_id = voice_id_from_selector(request).unwrap_or(QWEN_VOICE_ID);
    let is_icl = persona_key_from_voice_id(voice_id).is_some();
    SpeechResolvedRoute {
        backend_id: QWEN_BACKEND_ID.into(),
        model_id: Some(
            if is_icl {
                QWEN_ICL_MODEL_ID
            } else {
                QWEN_MODEL_ID
            }
            .into(),
        ),
        voice_id: Some(voice_id.into()),
        backend_kind: SpeechBackendKind::EmbeddedModel,
        network: NetworkBehavior::Never,
    }
}

fn voice_id_from_selector(request: &SynthesisRequest) -> Option<&str> {
    match &request.voice {
        VoiceSelector::Exact { voice_id } => Some(voice_id),
        _ => None,
    }
}

fn descriptor(
    ready: bool,
    design_ready: bool,
    config: &QwenBackendConfig,
    icl_voices: &[VoiceDescriptor],
) -> SpeechBackendDescriptor {
    let readiness = if ready {
        SpeechBackendReadiness::Ready
    } else {
        SpeechBackendReadiness::AssetInstallRequired { assets: Vec::new() }
    };
    let availability = if design_ready {
        CapabilityAvailability::Available
    } else {
        CapabilityAvailability::AssetInstallRequired
    };
    let icl_availability = if icl_voices.is_empty() {
        CapabilityAvailability::AssetInstallRequired
    } else {
        CapabilityAvailability::DeferredLoad
    };
    SpeechBackendDescriptor {
        id: QWEN_BACKEND_ID.into(),
        display_name: "Qwen3 TTS MLX".into(),
        kind: SpeechBackendKind::EmbeddedModel,
        readiness,
        capabilities: vec![
            SpeechCapability {
                id: "qwen3-tts.mlx-swift.synthesis".into(),
                backend_id: QWEN_BACKEND_ID.into(),
                model_id: Some(QWEN_MODEL_ID.into()),
                operation: SpeechOperationCapability::Synthesis(SynthesisCapabilities {
                    streaming_audio: false,
                    ssml: false,
                    word_alignment: false,
                    phoneme_alignment: false,
                    pause_resume: false,
                    voice_selection: true,
                    returned_audio: vec![AudioOutputKind::Wav],
                }),
                availability,
                network: NetworkBehavior::Never,
                languages: vec!["en-US".into()],
                limits: SpeechCapabilityLimits {
                    // The helper owns tokenization and can synthesize complete
                    // conversational replies. Do not reject otherwise valid
                    // persona speech at the UI routing layer by character count.
                    max_input_characters: None,
                    max_concurrent_requests: Some(1),
                    ..SpeechCapabilityLimits::default()
                },
                evidence: if design_ready {
                    vec![CapabilityEvidence {
                        source_id: "mom-qwen3-tts-helper".into(),
                        source_version: Some(env!("CARGO_PKG_VERSION").into()),
                        kind: EvidenceKind::SystemInventory,
                        outcome: EvidenceOutcome::Confirmed,
                        observed_at_unix_ms: now_ms(),
                        detail: format!(
                            "Bundled helper and local model config admitted at {}",
                            config.model_dir.display()
                        ),
                    }]
                } else {
                    Vec::new()
                },
            },
            SpeechCapability {
                id: "qwen3-tts.mlx-base-icl.synthesis".into(),
                backend_id: QWEN_BACKEND_ID.into(),
                model_id: Some(QWEN_ICL_MODEL_ID.into()),
                operation: SpeechOperationCapability::Synthesis(SynthesisCapabilities {
                    streaming_audio: true,
                    ssml: false,
                    word_alignment: false,
                    phoneme_alignment: false,
                    pause_resume: false,
                    voice_selection: true,
                    returned_audio: vec![AudioOutputKind::Wav, AudioOutputKind::Pcm],
                }),
                availability: icl_availability,
                network: NetworkBehavior::Never,
                languages: vec!["en-US".into()],
                limits: SpeechCapabilityLimits {
                    max_input_characters: None,
                    max_concurrent_requests: Some(1),
                    ..SpeechCapabilityLimits::default()
                },
                evidence: if icl_voices.is_empty() {
                    Vec::new()
                } else {
                    vec![CapabilityEvidence {
                source_id: "mom-qwen3-base-icl-local-reference".into(),
                source_version: Some(env!("CARGO_PKG_VERSION").into()),
                kind: EvidenceKind::SystemInventory,
                        outcome: EvidenceOutcome::Inconclusive,
                observed_at_unix_ms: now_ms(),
                        detail: "Local-only evaluation references passed manifest transcript and SHA-256 inventory checks; model loads on demand".into(),
            }]
                },
            },
        ],
        models: vec![
            SpeechModelDescriptor {
                id: QWEN_MODEL_ID.into(),
                display_name: "Qwen3 TTS 1.7B VoiceDesign 4-bit".into(),
                family: "qwen3-tts".into(),
                languages: vec!["en-US".into()],
                resident: false,
                estimated_memory_bytes: Some(2_000_000_000),
                content_hash: Some(QWEN_MODEL_REVISION.into()),
            },
            SpeechModelDescriptor {
                id: QWEN_ICL_MODEL_ID.into(),
                display_name: "Qwen3 TTS 1.7B Base ICL (local evaluation)".into(),
                family: "qwen3-tts".into(),
                languages: vec!["en-US".into()],
                resident: false,
                estimated_memory_bytes: Some(10_000_000_000),
                content_hash: None,
            },
        ],
        voices: vec![VoiceDescriptor {
            id: QWEN_VOICE_ID.into(),
            name: "Warm clinician".into(),
            language: "en-US".into(),
            gender: Some(VoiceGender::Female),
            quality: Some(VoiceQuality::Premium),
            expected_latency: Some(LatencyClass::High),
            network: NetworkBehavior::Never,
            installed: design_ready,
        }]
        .into_iter()
        .chain(icl_voices.iter().cloned())
        .collect(),
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
fn unavailable(id: &SpeechRequestId, code: &str, detail: &str) -> SpeechError {
    let mut e = SpeechError::unavailable(id, code, detail);
    e.backend_id = Some(QWEN_BACKEND_ID.into());
    e
}
fn error(
    id: &SpeechRequestId,
    code: &str,
    class: SpeechErrorClass,
    retryable: bool,
    detail: &str,
) -> SpeechError {
    SpeechError {
        code: code.into(),
        class,
        retryable,
        request_id: id.clone(),
        backend_id: Some(QWEN_BACKEND_ID.into()),
        safe_detail: detail.into(),
    }
}
fn cancelled_error(id: &SpeechRequestId) -> SpeechError {
    error(
        id,
        "qwen_synthesis_cancelled",
        SpeechErrorClass::Cancelled,
        false,
        "Qwen3 TTS synthesis was cancelled",
    )
}

fn clean_icl_cancelled_error(id: &SpeechRequestId) -> SpeechError {
    error(
        id,
        "qwen_icl_cancelled_clean",
        SpeechErrorClass::Cancelled,
        false,
        "Qwen3 TTS synthesis was cancelled and the resident worker acknowledged quiescence",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use speech_native_types::{
        AlignmentGranularity, SpeechDeadlinePolicy, SpeechPrivacyPolicy, SpeechRequestContext,
        SpeechRouteProfile, SpeechRoutingPolicy,
    };

    #[test]
    fn foreground_waiter_wins_next_worker_admission() {
        let worker = Arc::new(Mutex::new(0_u8));
        let waiters = Arc::new(AtomicUsize::new(0));
        let initial = worker.lock().expect("initial worker owner");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (order_tx, order_rx) = std::sync::mpsc::channel();

        let foreground_worker = Arc::clone(&worker);
        let foreground_waiters = Arc::clone(&waiters);
        let foreground_order = order_tx.clone();
        let foreground = std::thread::spawn(move || {
            let waiting = ForegroundWorkerWait::begin(&foreground_waiters);
            ready_tx.send(()).expect("foreground ready");
            let mut worker = foreground_worker.lock().expect("foreground lock");
            drop(waiting);
            *worker = 1;
            foreground_order
                .send("foreground")
                .expect("foreground order");
            std::thread::sleep(Duration::from_millis(5));
        });
        ready_rx.recv().expect("foreground declared");

        let background_worker = Arc::clone(&worker);
        let background_waiters = Arc::clone(&waiters);
        let background_cancelled = Arc::new(AtomicBool::new(false));
        let background = std::thread::spawn(move || {
            let mut worker = lock_background_worker(
                &background_worker,
                &background_waiters,
                &background_cancelled,
            )
            .expect("background lock");
            *worker = 2;
            order_tx.send("background").expect("background order");
        });

        drop(initial);
        assert_eq!(order_rx.recv().expect("first admission"), "foreground");
        assert_eq!(order_rx.recv().expect("second admission"), "background");
        foreground.join().expect("foreground join");
        background.join().expect("background join");
        assert_eq!(waiters.load(Ordering::Acquire), 0);
        assert_eq!(*worker.lock().expect("final worker"), 2);
    }

    #[test]
    fn cancelled_background_warm_never_waits_for_worker_admission() {
        let worker = Mutex::new(0_u8);
        let waiters = AtomicUsize::new(1);
        let cancelled = AtomicBool::new(true);
        assert!(lock_background_worker(&worker, &waiters, &cancelled).is_none());
    }

    #[test]
    fn wav_duration_rejects_wrong_shape() {
        assert_eq!(wav_duration_ms(b"not wav"), None);
    }

    fn pcm_wav() -> Vec<u8> {
        Vec::from(&b"RIFF\x26\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\xc0\x5d\x00\x00\x80\xbb\x00\x00\x02\x00\x10\x00data\x02\x00\x00\x00\x00\x00"[..])
    }

    #[test]
    fn validates_pcm_wav_chunks_and_bounds() {
        assert_eq!(wav_duration_ms(&pcm_wav()), Some(0));
        let mut malformed = pcm_wav();
        malformed[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(wav_duration_ms(&malformed), None);
        let mut wrong_rate = pcm_wav();
        wrong_rate[24..28].copy_from_slice(&16_000_u32.to_le_bytes());
        assert_eq!(wav_duration_ms(&wrong_rate), None);
    }

    #[test]
    fn resident_worker_survives_only_a_consumed_terminal_job_error() {
        let request = SpeechRequestId("resident-reuse-policy".into());
        assert!(worker_is_reusable_after(&unavailable(
            &request,
            "qwen_icl_synthesis_failed",
            "job failed"
        )));
        assert!(worker_is_reusable_after(&clean_icl_cancelled_error(
            &request
        )));
        assert!(!worker_is_reusable_after(&unavailable(
            &request,
            "qwen_synthesis_cancelled",
            "stream may still have unread PCM"
        )));
        assert!(!worker_is_reusable_after(&unavailable(
            &request,
            "qwen_icl_audio_invalid",
            "stream framing is no longer trusted"
        )));
    }

    #[test]
    fn resident_job_binds_the_admitted_persona_identity() {
        let job = IclWorkerJob {
            id: "request-1",
            text: "hello",
            persona_id: "gabor_mate",
            ref_audio: "/immutable/reference.wav",
            ref_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ref_text: "reference",
            warm_only: false,
        };
        let value = serde_json::to_value(job).expect("serialize resident job");

        assert_eq!(value["persona_id"], "gabor_mate");
        assert_eq!(value["ref_sha256"], "a".repeat(64));
        assert_eq!(value["warm_only"], false);
    }

    #[test]
    fn reference_admission_checks_safe_path_and_exact_hash() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::create_dir(root.path().join("clips")).expect("clips dir");
        let audio = pcm_wav();
        fs::write(root.path().join("clips/ref.wav"), &audio).expect("clip");
        let entry = ReferenceEntry {
            persona_id: "gabor_mate".into(),
            speaker: "Gabor Mate".into(),
            status: "collected".into(),
            clip_path: "clips/ref.wav".into(),
            clip_sha256: format!("{:x}", Sha256::digest(&audio)),
            reference_transcript: "Exact reference words.".into(),
            speaker_turn_verification: SpeakerTurnVerification {
                status: "verified_for_local_evaluation".into(),
            },
        };
        let config = QwenIclConfig {
            helper: PathBuf::new(),
            model_dir: PathBuf::new(),
            reference_root: root.path().to_path_buf(),
        };
        assert_eq!(
            entry_is_admitted(&config, &entry),
            Ok(fs::canonicalize(root.path().join("clips/ref.wav")).expect("canonical clip"))
        );
        let mut wrong_hash = entry.clone();
        wrong_hash.clip_sha256 = "0".repeat(64);
        assert_eq!(
            entry_is_admitted(&config, &wrong_hash),
            Err("reference clip hash mismatch")
        );
        let mut traversal = entry.clone();
        traversal.clip_path = "../outside.wav".into();
        assert_eq!(
            entry_is_admitted(&config, &traversal),
            Err("reference clip path is not a safe relative path")
        );

        let admitted = AdmittedIclReference {
            entry,
            clip: fs::canonicalize(root.path().join("clips/ref.wav")).expect("canonical clip"),
        };
        fs::remove_file(root.path().join("clips/ref.wav")).expect("remove after admission");
        let resolved = resolve_icl_entry(&[admitted], &icl_voice_id("gabor_mate"))
            .expect("immutable admitted reference");
        assert_eq!(resolved.entry.persona_id, "gabor_mate");
    }

    #[test]
    fn installed_reference_root_is_independent_from_runtime_model_storage() {
        let fixture = tempfile::tempdir().expect("tempdir");
        let managed_model_root = fixture.path().join("encrypted-runtime/speech-models");
        let installed_reference_root = fixture
            .path()
            .join("platform-app-data/speech-voices/qwen-icl");
        fs::create_dir_all(&installed_reference_root).expect("installed voice root");
        fs::write(installed_reference_root.join("manifest.json"), b"{}")
            .expect("installed voice manifest");

        let config = QwenBackendConfig::discover_with_reference_root(
            Some(&managed_model_root),
            Some(&installed_reference_root),
        );

        assert_eq!(
            config
                .icl
                .expect("explicit installed voice root")
                .reference_root,
            installed_reference_root
        );
    }

    #[tokio::test]
    #[ignore = "requires the exact local Qwen model and compiled MLX helper"]
    async fn real_local_qwen_synthesis_returns_bound_wav() {
        let helper = PathBuf::from(std::env::var_os("MOM_QWEN3_TTS_HELPER").expect("helper"));
        let model_dir = PathBuf::from(std::env::var_os("MOM_QWEN3_TTS_MODEL").expect("model"));
        let backend = QwenSpeechBackend::discover(QwenBackendConfig {
            helper,
            model_dir,
            icl: None,
        });
        assert!(backend.readiness().is_ready());
        let request_id = SpeechRequestId("qwen-real-smoke".into());
        let ticket = backend
            .synthesize(SynthesisRequest {
                context: SpeechRequestContext {
                    request_id: request_id.clone(),
                    client_id: "qwen-backend-smoke".into(),
                    route: SpeechRouteSelector::ExactBackend {
                        backend_id: QWEN_BACKEND_ID.into(),
                        model_id: Some(QWEN_MODEL_ID.into()),
                        voice_id: Some(QWEN_VOICE_ID.into()),
                    },
                    routing: SpeechRoutingPolicy {
                        privacy: SpeechPrivacyPolicy::LocalOnly,
                        profile: SpeechRouteProfile::ConsistentLocal,
                        allow_asset_download: false,
                        allow_fallback_before_output: false,
                    },
                    deadline: SpeechDeadlinePolicy::default(),
                },
                input: SynthesisInput::Text {
                    text: "Hello. Let us think this through together.".into(),
                },
                voice: VoiceSelector::Exact {
                    voice_id: QWEN_VOICE_ID.into(),
                },
                language: Some("en-US".into()),
                rate: 1.0,
                pitch: 1.0,
                volume: 1.0,
                output: AudioOutputFormat::Wav,
                alignment: AlignmentGranularity::None,
                stream: false,
            })
            .await
            .expect("ticket");
        let response = ticket.final_response().await.expect("response");
        assert_eq!(response.route, route());
        assert!(response.usage.real_local_inference);
        let SynthesisOutput::Complete { audio, format } = response.output else {
            panic!("complete output required");
        };
        assert_eq!(format, AudioOutputFormat::Wav);
        assert!(wav_duration_ms(&audio).is_some_and(|duration| duration > 500));
        backend.shutdown().await.expect("joined shutdown");
    }

    #[tokio::test]
    #[ignore = "runs local Qwen Base ICL with local-only persona reference audio"]
    async fn real_local_qwen_base_icl_synthesis_returns_bound_wav() {
        let backend = QwenSpeechBackend::discover(QwenBackendConfig::discover(None));
        assert!(
            backend.readiness().is_ready(),
            "Qwen ICL setup must be discovered"
        );
        let request_id = SpeechRequestId("qwen-icl-gabor-smoke".into());
        let voice_id = icl_voice_id("gabor_mate");
        let ticket = backend
            .synthesize(SynthesisRequest {
                context: SpeechRequestContext {
                    request_id: request_id.clone(),
                    client_id: "qwen-icl-backend-smoke".into(),
                    route: SpeechRouteSelector::ExactBackend {
                        backend_id: QWEN_BACKEND_ID.into(),
                        model_id: Some(QWEN_ICL_MODEL_ID.into()),
                        voice_id: Some(voice_id.clone()),
                    },
                    routing: SpeechRoutingPolicy {
                        privacy: SpeechPrivacyPolicy::LocalOnly,
                        profile: SpeechRouteProfile::ConsistentLocal,
                        allow_asset_download: false,
                        allow_fallback_before_output: false,
                    },
                    deadline: SpeechDeadlinePolicy::default(),
                },
                input: SynthesisInput::Text {
                    text: "Good morning. What is happening for this person right now?".into(),
                },
                voice: VoiceSelector::Exact { voice_id },
                language: Some("en-US".into()),
                rate: 1.0,
                pitch: 1.0,
                volume: 1.0,
                output: AudioOutputFormat::Wav,
                alignment: AlignmentGranularity::None,
                stream: false,
            })
            .await
            .expect("ticket");
        let response = ticket.final_response().await.expect("response");
        assert_eq!(response.route.model_id.as_deref(), Some(QWEN_ICL_MODEL_ID));
        assert_eq!(
            response.route.voice_id.as_deref(),
            Some("mom.qwen.icl.gabor_mate")
        );
        assert!(response.usage.real_local_inference);
        let SynthesisOutput::Complete { audio, format } = response.output else {
            panic!("complete output required");
        };
        assert_eq!(format, AudioOutputFormat::Wav);
        assert!(wav_duration_ms(&audio).is_some_and(|duration| duration > 500));
        backend.shutdown().await.expect("joined shutdown");
    }
}
