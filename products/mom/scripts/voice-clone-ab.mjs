#!/usr/bin/env node

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const sampleRootDefault = join(root, "products/mom/artifacts/voice-samples");
const catalogDefault = join(root, "products/mom/crates/mom-llama-runtime/assets/therapy_consult_personas.yaml");
const cacheDefault = join(homedir(), "Library/Caches/delysis/voice-ab");
const promptDefault = "Take a breath. Tell me what you notice in your body right now, and we will decide together what comes next.";
const aliases = new Map([
  ["arnold_popky", "arnold_popky_detur"],
  ["robert_miller", "robert_miller_fsap"],
  ["shirley_jean_schmidt", "shirley_jean_schmidt_dnms"],
]);
const configurations = {
  qwen: { backend: "mlx-audio", model: "Qwen3-TTS-1.7B-Base" },
};

function fail(message) { console.error(message); process.exit(1); }
function sha256(path) { return createHash("sha256").update(readFileSync(path)).digest("hex"); }
function parseArgs(argv) {
  const args = { persona: [], seed: 42, prompt: promptDefault, engine: "qwen" };
  for (let index = 0; index < argv.length; index += 1) {
    const key = argv[index];
    if (key === "--validate-only") args.validateOnly = true;
    else if (key === "--persona") args.persona.push(argv[++index]);
    else if (key.startsWith("--")) args[key.slice(2).replaceAll("-", "_")] = argv[++index];
    else fail(`unexpected argument: ${key}`);
  }
  if (args.engine !== "qwen") fail("Qwen is Mom's only supported consultant-voice engine");
  args.seed = Number(args.seed);
  return args;
}
function wavFacts(path) {
  const bytes = readFileSync(path);
  if (bytes.toString("ascii", 0, 4) !== "RIFF" || bytes.toString("ascii", 8, 12) !== "WAVE") throw new Error(`not a WAV file: ${path}`);
  let offset = 12, rate, channels, blockAlign, frames;
  while (offset + 8 <= bytes.length) {
    const kind = bytes.toString("ascii", offset, offset + 4);
    const size = bytes.readUInt32LE(offset + 4);
    if (kind === "fmt ") {
      channels = bytes.readUInt16LE(offset + 10);
      rate = bytes.readUInt32LE(offset + 12);
      blockAlign = bytes.readUInt16LE(offset + 20);
    } else if (kind === "data" && blockAlign) frames = Math.floor(size / blockAlign);
    offset += 8 + size + (size % 2);
  }
  if (!rate || !channels || frames === undefined) throw new Error(`incomplete WAV header: ${path}`);
  return { sample_rate_hz: rate, channels, frames, duration_seconds: Number((frames / rate).toFixed(3)) };
}
function checkedEntries(sampleRoot, requested, engine) {
  const manifest = JSON.parse(readFileSync(join(sampleRoot, "manifest.json"), "utf8"));
  const entries = [];
  for (const source of manifest.entries) {
    const personaId = aliases.get(source.persona_id) ?? source.persona_id;
    if (requested.size && !requested.has(personaId)) continue;
    const variant = source.reference_variants?.[engine] ?? source;
    const clip = join(sampleRoot, variant.clip_path);
    const transcript = variant.reference_transcript?.trim();
    const verification = variant.speaker_turn_verification ?? source.speaker_turn_verification;
    if (verification?.status !== "verified_for_local_evaluation") fail(`${personaId}: speaker turn is not verified`);
    if (!transcript) fail(`${personaId}: exact reference transcript is missing`);
    if (!existsSync(clip) || sha256(clip) !== variant.clip_sha256) fail(`${personaId}: reference clip is missing or changed`);
    entries.push({ ...source, ...variant, sample_persona_id: source.persona_id, persona_id: personaId,
      reference_variant: source.reference_variants?.[engine] ? engine : "shared", speaker_turn_verification: verification, clip, transcript });
  }
  const actual = new Set(entries.map((entry) => entry.persona_id));
  const missing = [...requested].filter((persona) => !actual.has(persona));
  if (missing.length) fail(`unknown personas: ${missing.join(", ")}`);
  if (!entries.length) fail("no verified voice samples selected");
  return entries;
}
function catalogPersonas(path) {
  return new Set(readFileSync(path, "utf8").split("\n").filter((line) => line.startsWith("- id: ")).map((line) => line.slice(6).trim()));
}
function pythonProgram() {
  return String.raw`
import json, sys, time
import numpy as np
from mlx_audio.audio_io import write
from mlx_audio.tts.utils import load_model
spec=json.load(sys.stdin); model=load_model(spec["model"])
for job in spec["jobs"]:
 start=time.monotonic()
 try:
  values=list(model.generate(text=spec["prompt"],ref_audio=job["clip"],ref_text=job["transcript"]))
  if not values: raise RuntimeError("Qwen returned no audio")
  value=values[-1]; write(job["output"],np.asarray(value.audio),int(value.sample_rate),format="wav")
  result={"persona_id":job["persona_id"],"status":"passed"}
 except Exception as error: result={"persona_id":job["persona_id"],"status":"failed","error":f"{type(error).__name__}: {error}"}
 result["elapsed_seconds"]=round(time.monotonic()-start,3); print("VOICE_RESULT "+json.dumps(result),flush=True)
`;
}
function batchPython(interpreter, model, prompt, seed, jobs) {
  const completed = spawnSync(interpreter, ["-c", pythonProgram()], {
    input: JSON.stringify({ model, prompt, seed, jobs }), encoding: "utf8", maxBuffer: 64 * 1024 * 1024,
  });
  const results = new Map();
  for (const line of (completed.stdout ?? "").split("\n")) {
    if (line.startsWith("VOICE_RESULT ")) { const value = JSON.parse(line.slice(13)); results.set(value.persona_id, value); }
  }
  if (completed.status !== 0) console.error((completed.stderr || completed.stdout || "provider process failed").trim());
  return results;
}

const args = parseArgs(process.argv.slice(2));
const sampleRoot = resolve(args.sample_root ?? sampleRootDefault);
const catalog = resolve(args.persona_catalog ?? catalogDefault);
const entries = checkedEntries(sampleRoot, new Set(args.persona), args.engine);
if (!args.persona.length) {
  const expected = catalogPersonas(catalog), actual = new Set(entries.map((entry) => entry.persona_id));
  const missing = [...expected].filter((value) => !actual.has(value)), unexpected = [...actual].filter((value) => !expected.has(value));
  if (missing.length || unexpected.length) fail(`voice roster does not match Mom's persona catalog; missing: ${missing.join(", ") || "none"}; unexpected: ${unexpected.join(", ") || "none"}`);
}
if (args.validateOnly) {
  console.log(JSON.stringify({ status: "passed", sample_count: entries.length, personas: entries.map((entry) => entry.persona_id) }, null, 2));
  process.exit(0);
}
const model = resolve(args.model ?? join(cacheDefault, "models/qwen3-tts-1.7b-base-bf16"));
if (!existsSync(model)) fail(`missing ${args.engine} model: ${model}`);
const outputRoot = resolve(args.output_root ?? join(sampleRoot, "synthesis-ab"));
const outputDir = join(outputRoot, args.engine); mkdirSync(outputDir, { recursive: true });
const jobs = entries.map((entry) => ({ persona_id: entry.persona_id, clip: entry.clip, transcript: entry.transcript, output: join(outputDir, `${entry.persona_id}.wav`), prompt: args.prompt }));
const interpreter = join(cacheDefault, "qwen/bin/python");
if (!existsSync(interpreter)) fail(`missing Qwen evaluation environment: ${interpreter}`);
const generated = batchPython(interpreter, model, args.prompt, args.seed, jobs);
const receipt = { schema_version: 1, engine: args.engine, model_path: model, prompt: args.prompt, seed: args.seed,
  engine_configuration: configurations[args.engine], orchestrator: `node ${process.version}`, results: [] };
for (const [index, entry] of entries.entries()) {
  const output = jobs[index].output;
  const result = { persona_id: entry.persona_id, speaker: entry.speaker, reference_sha256: entry.clip_sha256,
    reference_variant: entry.reference_variant, reference_duration_seconds: entry.duration_seconds,
    reference_transcript: entry.transcript, speaker_turn_verification: entry.speaker_turn_verification, output_path: output,
    ...(generated.get(entry.persona_id) ?? { status: "failed", elapsed_seconds: 0, error: "provider returned no result" }) };
  if (result.status === "passed") Object.assign(result, { output_sha256: sha256(output), ...wavFacts(output) });
  receipt.results.push(result);
  writeFileSync(join(outputDir, "receipt.json"), `${JSON.stringify(receipt, null, 2)}\n`);
  console.log(`[${args.engine}] ${entry.persona_id}: ${result.status}`);
}
process.exit(receipt.results.every((result) => result.status === "passed") ? 0 : 1);
