import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { collectVoiceDiagnostics, parseOptions, projectLog, projectTrace } from "./collect-voice-diagnostics.mjs";

const secret = "PRIVATE_SENTENCE_NEVER_REPORT_THIS";

test("collector accepts only documented paths and requires output", () => {
  assert.throws(() => parseOptions([]));
  assert.throws(() => parseOptions(["--out", "/tmp/report.json", "--unknown", "x"]));
  assert.throws(() => parseOptions(["--out", "/tmp/a", "--out", "/tmp/b"]));
  assert.equal(parseOptions(["--help"]).help, true);
});

test("trace projection keeps allowlisted numbers and hides arbitrary fields and error text", () => {
  const projected = projectTrace({
    id: secret,
    mode: "voice_note",
    began_at: 1792512000000,
    phase: "persona_prepare",
    finish_reason: secret,
    conversation: secret,
    summary: { first_content_to_tts_pcm_ms: 123, arbitrary_private_field: secret, error: secret },
    stages: [
      { stage: "tts_first_pcm", elapsed_ms: 124.5, message: secret, transcript: secret },
      { stage: "call_start_failed", elapsed_ms: 130, phase: "persona_prepare", error_code: "voice_generation_timeout", finish_reason: "max_tokens" },
      { stage: secret, elapsed_ms: 150 },
    ],
  });
  const serialized = JSON.stringify(projected);
  assert.match(serialized, /"mode":"voice_note"/);
  assert.match(serialized, /"began_at":"2026-10-20T16:00:00.000Z"/);
  assert.match(serialized, /"phase":"persona_prepare"/);
  assert.match(serialized, /"finish_reason":"max_tokens"/);
  assert.match(serialized, /"error_code":"voice_generation_timeout"/);
  assert.match(serialized, /"first_content_to_tts_pcm_ms":123/);
  assert.equal(projected.stages[0].stage, "tts_first_pcm");
  assert.equal(projected.unknown_stage_count, 1);
  assert.equal(projected.unknown_error_code_count, 1);
  assert.doesNotMatch(serialized, new RegExp(secret));
  assert.doesNotMatch(serialized, /conversation|transcript|message|arbitrary_private_field/);
});

test("log projection emits numeric setup/navigation events and fixed failure labels only", () => {
  const projected = projectLog([
    "mom-llama startup voice-cache progress: contexts=2/14 checkpoint_reused=false elapsed_ms=4500 cache_positions=120 state_bytes=8192",
    "mom-llama startup voice caches prepared: checkpoints_prepared=14, checkpoints_reused=0, contexts_total=14, elapsed_ms=45000",
    "mom-llama prepared speech claimed after 50000 ms",
    "mom-llama navigation paint switch=7 selection_queue_ms=1.2 selection_command_ms=2.3 chat_render_ms=4.5 switch_to_paint_ms=8.1",
    "mom-llama setup failed: " + secret + " /Users/someone/private",
    "arbitrary log line containing " + secret,
  ].join("\n"));
  const serialized = JSON.stringify(projected);
  assert.equal(projected.length, 5);
  assert.equal(projected[0].elapsed_ms, 4500);
  assert.equal(projected[1].total, 14);
  assert.equal(projected[2].event, "prepared_speech_claimed");
  assert.equal(projected[4].stage, "setup_or_initialization_failed");
  assert.doesNotMatch(serialized, new RegExp(secret));
  assert.doesNotMatch(serialized, /someone|private/);
});

test("end-to-end report uses explicit fixture paths without copying them or raw evidence", async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "blessenger-diagnostics-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const app = path.join(root, "fixture.app");
  const logs = path.join(root, "private-log-location");
  const data = path.join(root, "private-data-location");
  const out = path.join(root, "report.json");
  await mkdir(path.join(app, "Contents", "MacOS"), { recursive: true });
  await mkdir(path.join(logs, "voice-latency-history"), { recursive: true });
  await mkdir(path.join(app, "Contents", "Resources", "speech-voices", "qwen-icl", "audio"), { recursive: true });
  await mkdir(path.join(app, "Contents", "Resources", "speech-voices", "pocket", "embeddings"), { recursive: true });
  await mkdir(path.join(data, "speech-models", "qwen3-tts-1.7b-base-bf16"), { recursive: true });
  await mkdir(data, { recursive: true });
  await writeFile(path.join(app, "Contents", "Info.plist"), [
    "<?xml version=\"1.0\"?><plist><dict>",
    "<key>CFBundleIdentifier</key><string>com.delysis.llama-native-kit.mom-llama</string>",
    "<key>CFBundleExecutable</key><string>TestApp</string>",
    "<key>CFBundleShortVersionString</key><string>0.1.0</string>",
    "<key>CFBundleVersion</key><string>42</string>",
    "<key>SourceRevision</key><string>abc1234</string>",
    "</dict></plist>",
  ].join(""));
  await writeFile(path.join(app, "Contents", "MacOS", "TestApp"), "synthetic executable");
  const voices = path.join(app, "Contents", "Resources", "speech-voices");
  await writeFile(path.join(voices, "qwen-icl", "manifest.json"), JSON.stringify({ entries: [{ clip_path: "audio/reference.wav", speaker: secret }] }));
  await writeFile(path.join(voices, "qwen-icl", "audio", "reference.wav"), "wav fixture");
  await writeFile(path.join(voices, "pocket", "manifest.json"), JSON.stringify({ entries: [{ embedding_path: "embeddings/reference.f32le", speaker: secret }] }));
  await writeFile(path.join(voices, "pocket", "config.json"), "{}");
  await writeFile(path.join(voices, "pocket", "tokenizer.json"), "{}");
  await writeFile(path.join(voices, "pocket", "embeddings", "reference.f32le"), "embedding fixture");
  for (const name of ["config.json", "generation_config.json", "model.safetensors", "model.safetensors.index.json", "preprocessor_config.json", "tokenizer_config.json", "vocab.json", "merges.txt", "speech_tokenizer/config.json", "speech_tokenizer/configuration.json", "speech_tokenizer/model.safetensors", "speech_tokenizer/preprocessor_config.json"]) {
    await mkdir(path.dirname(path.join(data, "speech-models", "qwen3-tts-1.7b-base-bf16", name)), { recursive: true });
    await writeFile(path.join(data, "speech-models", "qwen3-tts-1.7b-base-bf16", name), "present");
  }
  await writeFile(path.join(logs, "mom-llama.log"), "mom-llama prepared speech claimed after 987 ms\n" + secret);
  const trace = (checkpoint, stage) => JSON.stringify({
    id: secret, checkpoint, mode: "call", began_at: 1792512000000, transcript: secret,
    summary: { runtime_warm_total_ms: 300, private_summary: secret },
    stages: [{ stage, elapsed_ms: 301, detail: secret }],
  });
  await writeFile(path.join(logs, "voice-latency-history", "privateid-0001-01.json"), trace(1, "call_runtime_ready"));
  await writeFile(path.join(logs, "voice-latency-history", "privateid-0002-02.json"), trace(2, "generation_timeout"));
  const result = await collectVoiceDiagnostics(parseOptions(["--app", app, "--candidate-app", app, "--data-dir", data, "--log-dir", logs, "--out", out]));
  await assert.rejects(collectVoiceDiagnostics(parseOptions(["--app", app, "--data-dir", data, "--log-dir", logs, "--out", out])), { code: "EEXIST" });
  const reportText = await readFile(out, "utf8");
  const report = JSON.parse(reportText);
  assert.equal(result.installed_app.identity.source_revision, "abc1234");
  assert.equal(result.candidate_app.identity.executable_sha256, result.installed_app.identity.executable_sha256);
  assert.equal(result.installed_app.identity.build, "42");
  assert.equal(report.voice_latency.traces[0].numeric_summary.runtime_warm_total_ms, 300);
  assert.equal(report.voice_latency.traces.length, 1, "latest checkpoints of the same private trace are collapsed");
  assert.equal(report.voice_latency.traces[0].checkpoint, 2);
  assert.equal(report.resources[0].manifest_entries, 1);
  assert.equal(report.resources[0].present_files, 2);
  assert.equal(report.resources[1].manifest_entries, 1);
  assert.equal(report.resources[1].present_files, 4);
  const managedBase = report.resources.find((asset) => asset.role === "qwen_managed_base_model_alternate");
  assert.equal(managedBase.status, "present");
  assert.equal(managedBase.optional, true);
  assert.equal(report.startup_and_navigation.events[0].elapsed_ms, 987);
  assert.equal(report.collection.encrypted_settings_read, false);
  assert.equal(report.paths.data_directory.source, "explicit");
  assert.doesNotMatch(reportText, new RegExp(secret));
  assert.doesNotMatch(reportText, /private-log-location|private-data-location|privateid|PRIVATE_SENTENCE_NEVER_REPORT_THIS|TestApp/);
  assert.ok(report.missing_evidence.includes("asset_integrity_not_checked"));
});

test("explicit missing paths stay missing instead of falling back to a standard location", async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "blessenger-diagnostics-missing-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const absent = path.join(root, "explicitly-absent");
  const report = await collectVoiceDiagnostics(parseOptions([
    "--app", absent,
    "--data-dir", absent,
    "--log-dir", absent,
    "--out", path.join(root, "report.json"),
  ]));
  assert.equal(report.installed_app.status, "not_found");
  assert.equal(report.paths.data_directory.status, "not_found");
  assert.equal(report.paths.log_directory.status, "not_found");
  assert.equal(report.installed_app.location_source, "explicit");
  assert.doesNotMatch(JSON.stringify(report), /explicitly-absent/);
});
