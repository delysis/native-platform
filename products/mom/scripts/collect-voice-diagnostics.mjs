#!/usr/bin/env node

import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const MAX_TRACE_BYTES = 64 * 1024;
const MAX_LOG_BYTES = 2 * 1024 * 1024;
const MAX_TRACES = 12;
const safeStages = new Set(("audio_prefill_begin_failed audio_prefill_begin_requested audio_prefill_deferred_pcm_replayed audio_prefill_drain_deferred audio_prefill_drain_failed audio_prefill_first_chunk_accepted audio_prefill_native_targeted audio_prefill_producer_overflow audio_prefill_push_failed audio_prefill_retry_after_transcript audio_prefill_session_ready audio_prefill_target_failed audio_prefill_wait_expired barge_in_detected barge_in_playback_stop_failed barge_in_playback_stopped call_activity_guard_ready call_activity_guard_wait_started call_conversation_ready call_greeting call_history_recovery_suspend_wait_finished call_history_recovery_suspend_wait_started call_microphone_ready call_persona_prepare_completed call_persona_prepare_failed call_persona_prepare_started call_ringing_started call_runtime_ready call_start_failed call_startup_stage_timed_out capture_processing capture_stopped chat_stream_event_received chat_stream_route_decision fallback_capture_encoded fallback_import_finished first_content_delta first_content_delta_received foreground_or_recovery generation_dispatched generation_finished generation_timeout local_replay_encoded native_capture_event_started native_capture_failed native_capture_first_sample native_capture_recovery_failed native_capture_recovery_ready native_capture_recovery_started native_capture_requested native_capture_started native_capture_unavailable next_turn_prefill_started pcm_worklet_warm_state playback_renderer playback_started prefill_flush_accepted prefill_flush_started runtime_warm_completed runtime_warm_failed runtime_warm_state silence_onset speculation_confirmed speculation_dispatch_started speculation_handoff_failed speculation_handoff_ready speculation_handoff_rearmed speculation_handoff_replay_enqueued speculation_handoff_started speculation_native_abort_complete speculation_started speculation_tts_stopped speech_preemption_acknowledged speech_preemption_started speech_resumed text_completed tts_finished tts_first_pcm tts_segment_finished tts_started tts_text_admitted tts_timed_out voice_activity_first_detected voice_capture_prefill_abort_failed voice_history_recovery_failed voice_history_recovery_finished voice_history_recovery_skipped voice_history_recovery_started voice_inference_archive voice_note voice_note_confirmation_failed voice_note_recording_hydration_failed voice_onset_prefill_failed voice_transcript_deferred voice_transcript_failed voice_transcript_finished voice_transcript_inference_finished voice_transcript_inference_started voice_transcript_live_failed voice_transcript_live_verified voice_transcript_queue_deferred_overflow voice_transcript_queued_for_idle_playback voice_transcript_record_finished voice_transcript_record_started voice_transcript_skipped voice_transcript_source_ready voice_transcript_started read_aloud_operation_started read_aloud_prior_playback_stop_failed read_aloud_started read_aloud_finished read_aloud_failed tts_backend_complete tts_pcm_complete durable_voice_audio_failed assistant_voice_reply_persisted").split(" "));
const safeErrorCodes = new Set(("audio_prefill_begin_failed audio_prefill_drain_failed audio_prefill_push_failed audio_prefill_target_failed audio_prefill_wait_expired barge_in_playback_stop_failed call_capture_start_skipped call_closed call_persona_prepare_failed call_start_failed call_startup_stage_timed_out call_transcript_slot_failed durable_voice_audio_failed generation_timeout in_memory_voice_audio_failed invalid_config mcp_platform_unsupported mention_target_not_found native_audio_finish_failed native_capture_failed native_capture_recovery_failed native_capture_unavailable next_turn_prefill_failed parakeet_not_resident parakeet_residency_check_failed persona_template_staged_attachments read_aloud_failed read_aloud_prior_playback_stop_failed record_failed runtime_warm_failed semantic_backpressure_failed speech_admission_closed speech_backend_failed speech_backend_unavailable speech_cancelled speech_descriptor_invalid speech_live_ingress_backlog speech_live_pcm_route_unavailable speech_live_sink_missing speech_live_transcription_deadline speech_live_transcription_unavailable speech_operation_duplicate speech_operation_limit speech_parakeet_model_not_ready speech_precommit_identity_lookup_failed speech_precommit_persona_binding_changed speech_precommit_stale speech_precommit_text_invalid speech_precommit_voice_bindings_poisoned speech_precommit_voice_unbound speech_provenance_mismatch speech_read_aloud_assistant_only speech_read_aloud_empty speech_read_aloud_stale speech_read_aloud_timeout speech_state_unavailable speech_stop_target_invalid speech_stop_wait_receipt_missing speech_stop_wait_state_closed speech_stop_wait_terminal_timeout speech_stream_invalid speech_transcript_too_large speech_transcription_stale speculation_handoff_failed startup_error transcript_residency_check_failed transcript_unavailable view_command_failed voice_attachment_import_lock_poisoned voice_audio_prefill_drain_timeout voice_audio_restart_prepared_in_scope voice_call_startup_timeout voice_capture_prefill_abort_failed voice_generation_timeout voice_history_recovery_failed voice_message_silent voice_note_cancelled voice_note_confirmation_failed voice_note_recording_hydration_failed voice_onset_prefill_failed voice_reply_attachment_target_conflict voice_reply_attachment_target_missing voice_speculation_preparation_identity_changed voice_transcript_blocked voice_transcript_failed voice_transcript_live_failed voice_transcript_queue_deferred_overflow voice_transcript_record_failed").split(" "));
const safePhases = new Set(["activity_guard", "ringing", "history_recovery", "microphone", "runtime_warm", "conversation", "persona_prepare", "greeting_generation", "call_turn", "voice_note", "read_aloud"]);
const safeFinishReasons = new Set(["end_of_generation", "stop_sequence", "max_tokens", "cancelled", "length", "stop", "first_word_choices_exhausted", "visual_token_support_exhausted"]);
const safeRendererReasons = new Set(["ended", "stopped"]);
const safeTtsBackendIds = new Set(["qwen3-tts.mlx-swift", "pocket-tts.native-cpu"]);
const safeTtsModelIds = new Set(["qwen3-tts-1.7b-base-icl-mlx-audio", "qwen3-tts-12hz-1.7b-voice-design-4bit", "pocket-tts-3.3.0-native-cpu"]);
const numericStageKeys = ["backend_ms", "phase_elapsed_ms", "deadline_ms", "output_audio_ms", "pcm_packets", "frames", "underruns"];
const numericSummaryKeys = ["runtime_warm_total_ms", "runtime_warm_settings_ms", "runtime_warm_model_acquire_ms", "call_request_to_microphone_ready_ms", "microphone_ready_to_ringing_ms", "microphone_native_first_sample_ms", "model_load_total_ms", "model_load_backend_ms", "model_load_artifact_open_ms", "model_load_weights_ms", "model_load_projector_ms", "model_load_artifact_verify_ms", "model_load_context_create_ms", "model_load_fingerprint_ms", "first_content_to_tts_pcm_ms", "tts_pcm_to_playback_ms", "playback_startup_target_ms", "playback_startup_buffered_ms", "dispatch_to_native_worker_ms", "native_worker_to_first_content_ms", "audio_first_accept_ms", "audio_first_read_ms", "generation_prompt_tokens", "generation_completion_tokens", "generation_sampled_tokens", "generation_cache_resident_prefix_tokens", "generation_cache_restored_prefix_tokens", "generation_cache_replayed_prefix_tokens", "audio_accepted_samples", "audio_consumed_samples", "tts_total_output_audio_ms", "tts_pcm_packets", "playback_renderer_frames", "playback_renderer_underruns"];
const home = os.homedir();

function safeNumber(value) {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}

async function exists(target) {
  try { const info = await stat(target); return info.isFile() || info.isDirectory(); } catch { return false; }
}

async function hashFile(filename) {
  const hash = createHash("sha256");
  await new Promise((resolve, reject) => {
    const stream = createReadStream(filename);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("error", reject);
    stream.on("end", resolve);
  });
  return hash.digest("hex");
}

async function installedIdentity(app) {
  try {
    const plistPath = path.join(app, "Contents", "Info.plist");
    const bytes = await readFile(plistPath);
    if (bytes.length > 256 * 1024 || bytes.includes(0)) return { status: "unavailable" };
    const plist = bytes.toString("utf8");
    const value = (key) => {
      const escaped = key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      return plist.match(new RegExp("<key>" + escaped + "</key>\\s*<(?:string|integer)>([^<]{1,128})</(?:string|integer)>"))?.[1] ?? null;
    };
    const bundle = value("CFBundleIdentifier");
    const executable = value("CFBundleExecutable");
    const version = value("CFBundleShortVersionString");
    const build = value("CFBundleVersion");
    const rev = value("SourceRevision") || value("BuildCommit") || value("GitCommit");
    const allowedBundles = ["com.delysis.llama-native-kit.mom-llama", "com.delysis.blessenger.voice-qualification"];
    let executableSha = null;
    let executableBytes = null;
    if (executable && /^[A-Za-z0-9._-]{1,128}$/.test(executable)) {
      const binary = path.join(app, "Contents", "MacOS", executable);
      const info = await lstat(binary).catch(() => null);
      if (info?.isFile() && info.size <= 2 * 1024 * 1024 * 1024) {
        executableBytes = info.size;
        executableSha = await hashFile(binary).catch(() => null);
      }
    }
    return {
      status: "available",
      bundle_id: allowedBundles.includes(bundle) ? bundle : null,
      version: version && /^[A-Za-z0-9.+_-]{1,64}$/.test(version) ? version : null,
      build: build && /^[A-Za-z0-9.+_-]{1,64}$/.test(build) ? build : null,
      source_revision: rev && /^[a-fA-F0-9]{7,40}$/.test(rev) ? rev.toLowerCase() : null,
      executable_bytes: executableBytes,
      executable_sha256: executableSha,
    };
  } catch { return { status: "unavailable" }; }
}

function cacheRoot() {
  if (process.env.HF_HUB_CACHE || process.env.HUGGINGFACE_HUB_CACHE) return path.resolve(process.env.HF_HUB_CACHE || process.env.HUGGINGFACE_HUB_CACHE);
  if (process.env.HF_HOME) return path.resolve(process.env.HF_HOME, "hub");
  if (process.env.XDG_CACHE_HOME) return path.resolve(process.env.XDG_CACHE_HOME, "huggingface", "hub");
  return path.join(home, ".cache", "huggingface", "hub");
}

function snapshot(repo, rev, filename) {
  return path.join(cacheRoot(), "models--" + repo.replaceAll("/", "--"), "snapshots", rev, filename);
}

async function inventory(role, files, optional = false) {
  let present = 0;
  let bytes = 0;
  for (const file of files) {
    try {
      const info = await stat(file);
      if (info.isFile() && info.size > 0) { present += 1; bytes += info.size; }
    } catch { /* A missing asset is evidence, not a collector error. */ }
  }
  return { role, expected_files: files.length, present_files: present, missing_files: files.length - present, present_bytes: bytes, status: present === files.length ? "present" : present === 0 ? "missing" : "partial", integrity: "not_checked", ...(optional ? { optional: true } : {}) };
}

function manifestRelativePath(value) {
  if (typeof value !== "string" || !value || path.posix.isAbsolute(value)) return null;
  const parts = value.split(/[\\/]+/);
  if (parts.some((part) => !part || part === "." || part === "..")) return null;
  return parts.join(path.sep);
}

async function manifestInventory(role, root, entryKey, pathKey, extraFiles = []) {
  const manifestPath = path.join(root, "manifest.json");
  try {
    const info = await stat(manifestPath);
    if (!info.isFile() || info.size > 256 * 1024) throw new Error("unavailable");
    const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
    const entries = manifest?.[entryKey];
    if (!Array.isArray(entries) || entries.length === 0 || entries.length > 128) throw new Error("unavailable");
    const files = [manifestPath, ...extraFiles.map((item) => path.join(root, item))];
    for (const entry of entries) {
      const relative = manifestRelativePath(entry?.[pathKey]);
      if (!relative) throw new Error("unsafe_manifest_path");
      files.push(path.join(root, relative));
    }
    const result = await inventory(role, [...new Set(files)]);
    return { ...result, manifest_entries: entries.length, inventory_source: "bundled_manifest" };
  } catch {
    return { role, expected_files: null, present_files: 0, missing_files: null, present_bytes: 0, status: "manifest_unavailable", manifest_entries: null, inventory_source: "bundled_manifest", integrity: "not_checked" };
  }
}

function projectTrace(raw) {
  const stages = [];
  let unknownStages = 0;
  let unknownErrors = 0;
  const sourceStages = Array.isArray(raw?.stages) ? raw.stages.slice(-512) : [];
  for (const item of sourceStages) {
    if (!item || typeof item !== "object" || !safeStages.has(item.stage)) { unknownStages += 1; continue; }
    const entry = { stage: item.stage, elapsed_ms: safeNumber(item.elapsed_ms) };
    if (safePhases.has(item.phase)) entry.phase = item.phase;
    if (safeFinishReasons.has(item.finish_reason)) entry.finish_reason = item.finish_reason;
    if (item.stage === "playback_renderer" && safeRendererReasons.has(item.reason)) entry.reason = item.reason;
    if (safeTtsBackendIds.has(item.backend_id)) entry.backend_id = item.backend_id;
    if (safeTtsModelIds.has(item.model_id)) entry.model_id = item.model_id;
    for (const key of numericStageKeys) {
      const value = safeNumber(item[key]);
      if (value !== null) entry[key] = value;
    }
    const possibleErrors = [item.error_code, item.error, item.message].filter((value) => typeof value === "string");
    if (possibleErrors.length) {
      const safeCode = possibleErrors.map((value) => value.split(/[\s:]/, 1)[0]).find((code) => safeErrorCodes.has(code));
      if (safeCode) entry.error_code = safeCode;
      else unknownErrors += 1;
    }
    stages.push(entry);
  }
  const summary = {};
  for (const key of numericSummaryKeys) {
    const value = safeNumber(raw?.summary?.[key]);
    if (value !== null) summary[key] = value;
  }
  for (const [key, allowed, stageKey] of [
    ["tts_backend_ids", safeTtsBackendIds, "backend_id"],
    ["tts_model_ids", safeTtsModelIds, "model_id"],
  ]) {
    const values = [
      ...(Array.isArray(raw?.summary?.[key]) ? raw.summary[key] : []),
      ...stages.map((stage) => stage[stageKey]),
    ];
    const safeValues = [...new Set(values.filter((value) => allowed.has(value)))];
    if (safeValues.length) summary[key] = safeValues;
  }
  const parsedDate = typeof raw?.began_at === "string" ? Date.parse(raw.began_at)
    : typeof raw?.began_at === "number" && Number.isFinite(raw.began_at)
      ? (raw.began_at >= 100_000_000_000 ? raw.began_at : raw.began_at * 1000) : NaN;
  const phase = safePhases.has(raw?.phase) ? raw.phase : null;
  const finishReason = safeFinishReasons.has(raw?.finish_reason) ? raw.finish_reason : null;
  return {
    mode: ["call", "voice_note", "read_aloud", "call_greeting"].includes(raw?.mode) ? raw.mode : "unknown",
    began_at: Number.isFinite(parsedDate) ? new Date(parsedDate).toISOString() : null,
    checkpoint: safeNumber(raw?.checkpoint),
    phase,
    finish_reason: finishReason,
    stage_count: Array.isArray(raw?.stages) ? raw.stages.length : 0,
    stages,
    unknown_stage_count: unknownStages,
    unknown_error_code_count: unknownErrors,
    numeric_summary: summary,
  };
}

async function latencyTraces(logDir) {
  const history = path.join(logDir, "voice-latency-history");
  let names;
  try { names = await readdir(history); } catch { return { status: "not_found", traces: [] }; }
  const candidates = [];
  for (const name of names) {
    if (!/^[A-Za-z0-9_-]+-\d{4,}-\d{2,}\.json$/.test(name)) continue;
    const file = path.join(history, name);
    try {
      const info = await stat(file);
      if (info.isFile() && info.size <= MAX_TRACE_BYTES) {
        const match = /-([0-9]{4,})-([0-9]{2,})\.json$/.exec(name);
        candidates.push({ file, modified: info.mtimeMs, filenameCheckpoint: match ? Number(match[1]) : 0, filenameStages: match ? Number(match[2]) : 0 });
      }
    } catch { /* Rotated during collection. */ }
  }
  candidates.sort((a, b) => b.modified - a.modified);
  const latestById = new Map();
  for (const candidate of candidates) {
    try {
      const raw = JSON.parse(await readFile(candidate.file, "utf8"));
      const projected = projectTrace(raw);
      const id = typeof raw?.id === "string" && raw.id.length <= 256 ? raw.id : path.basename(candidate.file);
      const checkpoint = safeNumber(raw?.checkpoint) ?? candidate.filenameCheckpoint;
      const ranked = { projected: { ...projected, checkpoint }, checkpoint, stageCount: projected.stage_count || candidate.filenameStages, modified: candidate.modified };
      const previous = latestById.get(id);
      if (!previous || ranked.checkpoint > previous.checkpoint
          || (ranked.checkpoint === previous.checkpoint && ranked.stageCount > previous.stageCount)
          || (ranked.checkpoint === previous.checkpoint && ranked.stageCount === previous.stageCount && ranked.modified > previous.modified)) latestById.set(id, ranked);
    } catch { /* Ignore malformed raw evidence. */ }
  }
  const traces = [...latestById.values()].sort((a, b) => b.modified - a.modified).slice(0, MAX_TRACES).map((item) => item.projected);
  return { status: traces.length ? "available" : "not_found", traces };
}

const progressRe = /mom-llama startup voice-cache progress: contexts=(\d+)\/(\d+) checkpoint_reused=(true|false) elapsed_ms=(\d+) cache_positions=(\d+) state_bytes=(\d+)/;
const preparedRe = /mom-llama startup voice caches prepared: checkpoints_prepared=(\d+), checkpoints_reused=(\d+), contexts_total=(\d+), elapsed_ms=(\d+)/;
const claimedRe = /mom-llama prepared speech claimed after (\d+) ms/;
const navigationRe = /mom-llama navigation paint switch=(\d+) selection_queue_ms=([\d.]+) selection_command_ms=([\d.]+) chat_render_ms=([\d.]+) switch_to_paint_ms=([\d.]+)/;
function projectLog(text) {
  const events = [];
  for (const line of text.split(/\r?\n/)) {
    let m = progressRe.exec(line);
    if (m) { events.push({ event: "voice_cache_progress", completed: Number(m[1]), total: Number(m[2]), reused: m[3] === "true", elapsed_ms: Number(m[4]), cache_positions: Number(m[5]), state_bytes: Number(m[6]) }); continue; }
    m = preparedRe.exec(line);
    if (m) { events.push({ event: "voice_caches_prepared", prepared: Number(m[1]), reused: Number(m[2]), total: Number(m[3]), elapsed_ms: Number(m[4]) }); continue; }
    m = claimedRe.exec(line);
    if (m) { events.push({ event: "prepared_speech_claimed", elapsed_ms: Number(m[1]) }); continue; }
    m = navigationRe.exec(line);
    if (m) { events.push({ event: "navigation_paint", switch: Number(m[1]), selection_queue_ms: Number(m[2]), selection_command_ms: Number(m[3]), chat_render_ms: Number(m[4]), switch_to_paint_ms: Number(m[5]) }); continue; }
    if (/mom-llama (?:setup|initialization) failed/.test(line)) events.push({ event: "known_failure_marker", stage: "setup_or_initialization_failed" });
    else if (line.includes("startup voice-cache preparation failed")) events.push({ event: "known_failure_marker", stage: "voice_cache_preparation_failed" });
    else if (line.includes("prepared speech startup failed")) events.push({ event: "known_failure_marker", stage: "prepared_speech_startup_failed" });
  }
  return events;
}

async function safeLogEvents(logDir) {
  let names;
  try { names = await readdir(logDir); } catch { return { status: "not_found", events: [] }; }
  const files = [];
  for (const name of names) {
    if (!/^(Blessenger|mom-llama|app|launch\.stdout|launch\.stderr|stdout|stderr)\w*\.log$/.test(name)) continue;
    try {
      const file = path.join(logDir, name);
      const info = await stat(file);
      if (info.isFile() && info.size <= MAX_LOG_BYTES) files.push({ file, mtime: info.mtimeMs });
    } catch { /* Ignore disappearing files. */ }
  }
  files.sort((a, b) => b.mtime - a.mtime);
  const events = [];
  for (const item of files.slice(0, 4)) {
    try { events.push(...projectLog(await readFile(item.file, "utf8"))); } catch { /* No raw errors. */ }
  }
  return { status: events.length ? "available" : "no_allowlisted_events", events: events.slice(-100) };
}

function parseOptions(argv) {
  const opts = {};
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === "--help" || flag === "-h") return { help: true };
    if (!["--app", "--candidate-app", "--data-dir", "--log-dir", "--out"].includes(flag) || opts[flag] || !argv[i + 1] || argv[i + 1].startsWith("--")) throw new Error("invalid options");
    opts[flag] = path.resolve(argv[++i]);
  }
  if (!opts["--out"] && !opts.help) throw new Error("missing output");
  return opts;
}

export { parseOptions, projectLog, projectTrace };

export async function collectVoiceDiagnostics(opts) {
  const appCandidates = [opts["--app"], "/Applications/Blessenger.app", path.join(home, "Applications/Blessenger.app")].filter(Boolean);
  const selectedApp = opts["--app"] || await firstExisting(appCandidates) || appCandidates[0];
  const dataCandidates = [opts["--data-dir"], path.join(home, "Library/Application Support/com.delysis.llama-native-kit.mom-llama"), path.join(home, "Library/Application Support/com.delysis.blessenger.voice-qualification"), path.join(home, ".local/share/llama-native-kit/mom-llama")].filter(Boolean);
  const dataPath = opts["--data-dir"] || await firstExisting(dataCandidates) || dataCandidates[0];
  const logCandidates = [opts["--log-dir"], path.join(home, "Library/Logs/com.delysis.llama-native-kit.mom-llama"), path.join(home, "Library/Logs/com.delysis.blessenger.voice-qualification"), path.join(home, "Library/Logs/Blessenger")].filter(Boolean);
  const logPath = opts["--log-dir"] || await firstExisting(logCandidates) || logCandidates[0];
  const appStatus = await exists(selectedApp) ? "present" : "not_found";
  const candidateStatus = opts["--candidate-app"]
    ? (await exists(opts["--candidate-app"]) ? "present" : "not_found")
    : "not_provided";
  const appResources = path.join(selectedApp, "Contents/Resources/speech-voices");
  const qwenRev = "a6eb4f68e4b056f1215157bb696209bc82a6db48";
  const pocketRev = "575d14372df89a4534bcb989870a34e77544b09f";
  const qwenBaseDirectory = process.env.MOM_QWEN3_TTS_BASE_MODEL
    ? path.resolve(process.env.MOM_QWEN3_TTS_BASE_MODEL)
    : snapshot("mlx-community/Qwen3-TTS-12Hz-1.7B-Base-bf16", qwenRev, "");
  const qwenDesignDirectory = process.env.MOM_QWEN3_TTS_MODEL
    ? path.resolve(process.env.MOM_QWEN3_TTS_MODEL)
    : path.join(dataPath, "speech-models/qwen3-tts-1.7b-voice-design-4bit");
  const qwenModelFiles = ["config.json", "generation_config.json", "model.safetensors", "model.safetensors.index.json", "preprocessor_config.json", "tokenizer_config.json", "vocab.json", "merges.txt", "speech_tokenizer/config.json", "speech_tokenizer/configuration.json", "speech_tokenizer/model.safetensors", "speech_tokenizer/preprocessor_config.json"];
  const parakeetExplicitDir = process.env.SPEECH_NATIVE_PARAKEET_MODEL_DIR || process.env.FTE_PARAKEET_MODEL_DIR;
  const parakeetManagedRoot = process.env.SPEECH_NATIVE_PARAKEET_MANAGED_ROOT
    ? path.resolve(process.env.SPEECH_NATIVE_PARAKEET_MANAGED_ROOT)
    : path.join(cacheRoot(), "local/speech/models");
  const parakeetModelDir = parakeetExplicitDir
    ? path.resolve(parakeetExplicitDir)
    : path.join(parakeetManagedRoot, "parakeet-realtime-eou-120m-v1-onnx", "c710ae82b52aa969f89874e7e7b35ad570fec50cc3d943a4fdde0bb874948756");
  const assetResults = await Promise.all([
    manifestInventory("qwen_voice_references", path.join(appResources, "qwen-icl"), "entries", "clip_path"),
    manifestInventory("pocket_voice_resources", path.join(appResources, "pocket"), "entries", "embedding_path", ["config.json", "tokenizer.json"]),
    inventory("qwen_icl_model", qwenModelFiles.map((f) => path.join(qwenBaseDirectory, f))),
    inventory("qwen_managed_base_model_alternate", qwenModelFiles.map((f) => path.join(dataPath, "speech-models/qwen3-tts-1.7b-base-bf16", f)), true),
    inventory("qwen_voice_design_model_optional", qwenModelFiles.map((f) => path.join(qwenDesignDirectory, f)), true),
    inventory("pocket_tts_model", [snapshot("kyutai/pocket-tts-without-voice-cloning", pocketRev, "languages/english/model.safetensors")]),
    inventory("parakeet_asr_model", ["encoder.onnx", "decoder_joint.onnx", "tokenizer.json"].map((f) => path.join(parakeetModelDir, f))),
    inventory("gemma_chat_model", [snapshot("google/gemma-4-12B-it-qat-q4_0-gguf", "29d097773436b69ff9feafd636ab4cf873786537", "gemma-4-12b-it-qat-q4_0.gguf"), snapshot("google/gemma-4-12B-it-qat-q4_0-gguf", "29d097773436b69ff9feafd636ab4cf873786537", "mmproj-gemma-4-12b-it-qat-q4_0.gguf")]),
    inventory("qwen_tts_helper", [path.join(selectedApp, "Contents/MacOS/qwen3-tts-helper")]),
    inventory("mlx_runtime", [path.join(selectedApp, "Contents/MacOS/mlx.metallib")]),
  ]);
  const traces = await latencyTraces(logPath);
  const logs = await safeLogEvents(logPath);
  const report = {
    schema: "blessenger.voice-diagnostics.v1",
    collected_at: new Date().toISOString(),
    collection: { read_only: true, encrypted_settings_read: false, conversation_content_read: false, raw_logs_included: false },
    installed_app: { status: appStatus, location_source: opts["--app"] ? "explicit" : "standard_location", identity: appStatus === "present" ? await installedIdentity(selectedApp) : { status: "not_found" } },
    candidate_app: { status: candidateStatus, identity: candidateStatus === "present" ? await installedIdentity(opts["--candidate-app"]) : { status: candidateStatus } },
    system: {
      platform: process.platform,
      architecture: process.arch,
      os_release: /^[A-Za-z0-9._-]{1,64}$/.test(os.release()) ? os.release() : null,
      total_memory_bytes: os.totalmem(),
      free_memory_bytes: os.freemem(),
      logical_cpu_count: typeof os.availableParallelism === "function" ? os.availableParallelism() : os.cpus().length,
      load_average_1m_5m_15m: process.platform === "win32" ? null : os.loadavg().map((value) => Number.isFinite(value) ? value : null),
      process_memory_inspected: false,
    },
    paths: { data_directory: { status: await exists(dataPath) ? "present" : "not_found", source: opts["--data-dir"] ? "explicit" : "standard_location" }, log_directory: { status: await exists(logPath) ? "present" : "not_found", source: opts["--log-dir"] ? "explicit" : "standard_location" }, model_cache: { source: "huggingface_standard_cache", environment_override_used: Boolean(process.env.HF_HUB_CACHE || process.env.HUGGINGFACE_HUB_CACHE || process.env.HF_HOME || process.env.XDG_CACHE_HOME) } },
    speech_sampling: { status: "not_inspected", reason: "encrypted_settings_not_read" },
    resources: assetResults,
    voice_latency: traces,
    startup_and_navigation: logs,
    missing_evidence: [
      ...(appStatus === "present" ? [] : ["installed_app_bundle"]),
      ...(traces.status === "not_found" ? ["voice_latency_history"] : []),
      ...(logs.status === "not_found" || logs.status === "no_allowlisted_events" ? ["allowlisted_startup_or_navigation_log_events"] : []),
      "running_process_identity_not_inspected",
      "encrypted_model_settings_and_sampling_limit_not_read",
      "asset_integrity_not_checked",
    ],
  };
  await mkdir(path.dirname(opts["--out"]), { recursive: true });
  await writeFile(opts["--out"], JSON.stringify(report, null, 2) + "\n", { mode: 0o600, flag: "wx" });
  return report;
}

async function firstExisting(candidates) {
  for (const candidate of candidates) if (await exists(candidate)) return candidate;
  return null;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const opts = parseOptions(process.argv.slice(2));
    if (opts.help) process.stdout.write("Read-only voice diagnostics. Usage: node collect-voice-diagnostics.mjs [--app PATH] [--candidate-app PATH] [--data-dir PATH] [--log-dir PATH] --out REPORT.json\n");
    else {
      const report = await collectVoiceDiagnostics(opts);
      process.stdout.write("Voice diagnostics saved. App: " + report.installed_app.status + "; traces: " + report.voice_latency.status + "; startup/navigation: " + report.startup_and_navigation.status + ".\n");
    }
  } catch {
    process.stderr.write("Could not collect diagnostics. Check the documented options and destination.\n");
    process.exitCode = 2;
  }
}
