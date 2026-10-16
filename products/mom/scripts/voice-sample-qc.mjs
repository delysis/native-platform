#!/usr/bin/env node

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const defaultSampleRoot = join(root, "products/mom/artifacts/voice-samples");

function fail(message) { console.error(message); process.exit(1); }
function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 2) {
    if (!argv[index]?.startsWith("--") || argv[index + 1] === undefined) fail(`invalid argument: ${argv[index] ?? ""}`);
    args[argv[index].slice(2).replaceAll("-", "_")] = argv[index + 1];
  }
  return args;
}
function percentile(sorted, fraction) {
  if (!sorted.length) return 0;
  return sorted[Math.min(sorted.length - 1, Math.floor((sorted.length - 1) * fraction))];
}
function db(value) { return value > 0 ? 20 * Math.log10(value) : -120; }
function round(value, digits = 3) { return Number(value.toFixed(digits)); }
function pcm16Wav(path) {
  const bytes = readFileSync(path);
  if (bytes.toString("ascii", 0, 4) !== "RIFF" || bytes.toString("ascii", 8, 12) !== "WAVE") fail(`${path}: not a WAV file`);
  let offset = 12, format, channels, rate, bits, dataOffset, dataSize;
  while (offset + 8 <= bytes.length) {
    const kind = bytes.toString("ascii", offset, offset + 4);
    const size = bytes.readUInt32LE(offset + 4);
    if (kind === "fmt ") {
      format = bytes.readUInt16LE(offset + 8);
      channels = bytes.readUInt16LE(offset + 10);
      rate = bytes.readUInt32LE(offset + 12);
      bits = bytes.readUInt16LE(offset + 22);
    } else if (kind === "data") {
      dataOffset = offset + 8;
      dataSize = Math.min(size, bytes.length - dataOffset);
    }
    offset += 8 + size + (size % 2);
  }
  if (format !== 1 || channels !== 1 || bits !== 16 || !rate || dataOffset === undefined) {
    fail(`${path}: quality screening requires mono PCM16 WAV input`);
  }
  const samples = new Float64Array(Math.floor(dataSize / 2));
  for (let index = 0; index < samples.length; index += 1) samples[index] = bytes.readInt16LE(dataOffset + index * 2) / 32768;
  return { rate, samples };
}
function analyze(path) {
  const { rate, samples } = pcm16Wav(path);
  const frameSize = Math.max(1, Math.round(rate * .02));
  const frameDb = [];
  let sum = 0, peak = 0, clipped = 0, dc = 0;
  for (const value of samples) {
    const absolute = Math.abs(value);
    sum += value * value; dc += value; peak = Math.max(peak, absolute);
    if (absolute >= .999) clipped += 1;
  }
  for (let start = 0; start < samples.length; start += frameSize) {
    let frameSum = 0;
    const end = Math.min(samples.length, start + frameSize);
    for (let index = start; index < end; index += 1) frameSum += samples[index] * samples[index];
    frameDb.push(db(Math.sqrt(frameSum / Math.max(1, end - start))));
  }
  const sorted = [...frameDb].sort((left, right) => left - right);
  const noiseFloor = percentile(sorted, .2);
  const activeThreshold = Math.max(-45, noiseFloor + 10);
  const active = frameDb.filter((value) => value >= activeThreshold).sort((left, right) => left - right);
  const quiet = frameDb.filter((value) => value < activeThreshold).sort((left, right) => left - right);
  const activeLevel = percentile(active, .5);
  const quietLevel = quiet.length ? percentile(quiet, .5) : noiseFloor;
  const estimatedSnr = Math.max(0, activeLevel - quietLevel);
  const rms = Math.sqrt(sum / Math.max(1, samples.length));
  const occupancy = active.length / Math.max(1, frameDb.length);
  const duration = samples.length / rate;
  const clippedFraction = clipped / Math.max(1, samples.length);
  const flags = [];
  if (duration < 8) flags.push("too_short");
  if (clippedFraction > .0001) flags.push("clipping");
  if (estimatedSnr < 18) flags.push("low_estimated_snr");
  if (occupancy < .55) flags.push("low_speech_occupancy");
  if (db(peak) < -12) flags.push("low_peak_level");
  return {
    duration_seconds: round(duration), sample_rate_hz: rate,
    rms_dbfs: round(db(rms), 1), peak_dbfs: round(db(peak), 1),
    dc_offset: round(dc / Math.max(1, samples.length), 6),
    clipped_sample_fraction: round(clippedFraction, 6),
    speech_occupancy_estimate: round(occupancy),
    noise_floor_dbfs_estimate: round(noiseFloor, 1),
    active_speech_dbfs_estimate: round(activeLevel, 1),
    estimated_snr_db: round(estimatedSnr, 1), flags,
  };
}

const args = parseArgs(process.argv.slice(2));
const sampleRoot = resolve(args.sample_root ?? defaultSampleRoot);
const manifestPath = resolve(args.manifest ?? join(sampleRoot, "manifest.json"));
const outputPath = resolve(args.output ?? join(sampleRoot, "signal-qc.json"));
const reviewPath = resolve(args.review ?? join(sampleRoot, "listening-review.json"));
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
const listeningReviews = existsSync(reviewPath)
  ? new Map(JSON.parse(readFileSync(reviewPath, "utf8")).entries.map((entry) => [entry.persona_id, entry]))
  : new Map();
const preferredContexts = new Set(["in_person_podcast", "in_person_studio_interview"]);
const entries = manifest.entries.flatMap((entry) => {
  const variants = entry.reference_variants
    ? Object.entries(entry.reference_variants)
    : [["shared", entry]];
  return variants.map(([variantName, variant]) => {
  const metrics = analyze(join(sampleRoot, variant.clip_path));
  const context = entry.recording_context ?? "unclassified";
  const flags = [...metrics.flags];
  if (context === "unclassified") flags.push("recording_context_unclassified");
  if (context !== "unclassified" && !preferredContexts.has(context)) flags.push("nonpreferred_recording_context");
  const listening = listeningReviews.get(entry.persona_id) ?? null;
  const signalScreening = flags.some((flag) => ["too_short", "clipping", "low_estimated_snr"].includes(flag))
    ? "replace"
    : flags.length ? "review" : "pass";
  return {
    persona_id: entry.persona_id, speaker: entry.speaker, reference_variant: variantName,
    source_url: variant.source_url ?? entry.source_url,
    source_title: variant.source_title ?? entry.source_title,
    recording_context: variant.recording_context ?? context, listening_review: listening, ...metrics, flags,
    signal_screening: signalScreening,
    recommended_action: listening?.verdict ?? signalScreening,
  };
  });
}).sort((left, right) => {
  const priority = { replace: 0, improve: 1, keep: 2 };
  const leftPriority = priority[left.listening_review?.verdict] ?? 1;
  const rightPriority = priority[right.listening_review?.verdict] ?? 1;
  return leftPriority - rightPriority || left.estimated_snr_db - right.estimated_snr_db;
});

writeFileSync(outputPath, `${JSON.stringify({
  schema_version: 1,
  method: "20 ms PCM frame-energy screening; estimated SNR is median active-frame level minus median quiet-frame level and is a ranking heuristic, not a laboratory SNR measurement",
  preferred_recording_contexts: [...preferredContexts],
  thresholds: { minimum_duration_seconds: 8, minimum_estimated_snr_db: 18, maximum_clipped_sample_fraction: .0001, minimum_speech_occupancy: .55 },
  listening_review_path: existsSync(reviewPath) ? reviewPath : null,
  entries,
}, null, 2)}\n`);
console.log(JSON.stringify({ status: "passed", output: outputPath, sample_count: entries.length,
  replace: entries.filter((entry) => entry.recommended_action === "replace").map((entry) => entry.persona_id),
  improve: entries.filter((entry) => entry.recommended_action === "improve").map((entry) => entry.persona_id),
  signal_flags: entries.filter((entry) => entry.signal_screening !== "pass").map((entry) => entry.persona_id) }, null, 2));
