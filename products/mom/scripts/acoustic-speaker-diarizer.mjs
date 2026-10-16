#!/usr/bin/env node

import { createHash, randomUUID } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { execFileSync, spawn } from "node:child_process";
import { createInterface } from "node:readline";

const DEFAULT_THRESHOLD = 0.78;
const DEFAULT_WINDOW_MS = 3_000;
const DEFAULT_MIN_WINDOW_MS = 1_500;
const DEFAULT_JOIN_GAP_MS = 250;

function parseArtifact(path) {
  const text = readFileSync(path, "utf8");
  return path.endsWith(".jsonl")
    ? text.split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line))
    : JSON.parse(text);
}

function interval(segment, index) {
  const startMs = segment.start_ms ?? Math.round(Number(segment.start_seconds) * 1000);
  const endMs = segment.end_ms ?? Math.round(Number(segment.end_seconds) * 1000);
  if (!Number.isInteger(startMs) || !Number.isInteger(endMs) || startMs < 0 || endMs <= startMs) {
    throw new Error(`transcript segment ${index} has an invalid time range`);
  }
  return { index, start_ms: startMs, end_ms: endMs };
}

// Caption files commonly contain rolling, heavily overlapping cues and even
// 10 ms correction cues. Those are useful text evidence, but they are invalid
// speaker-encoder inputs. Convert their union into bounded acoustic windows.
// The transcript remains unchanged in the ledger and is aligned back onto
// these windows by timestamp.
export function acousticWindows(input, options = {}) {
  const windowMs = Number(options.windowMs ?? DEFAULT_WINDOW_MS);
  const minWindowMs = Number(options.minWindowMs ?? DEFAULT_MIN_WINDOW_MS);
  const joinGapMs = Number(options.joinGapMs ?? DEFAULT_JOIN_GAP_MS);
  if (!Number.isInteger(windowMs) || !Number.isInteger(minWindowMs)
      || !Number.isInteger(joinGapMs) || windowMs < minWindowMs
      || minWindowMs < 250 || joinGapMs < 0) {
    throw new Error("invalid acoustic window configuration");
  }
  const intervals = input.map(interval)
    .sort((left, right) => left.start_ms - right.start_ms || left.end_ms - right.end_ms);
  if (!intervals.length) throw new Error("at least one transcript segment is required");
  const regions = [];
  for (const current of intervals) {
    const prior = regions.at(-1);
    if (prior && current.start_ms <= prior.end_ms + joinGapMs) {
      prior.end_ms = Math.max(prior.end_ms, current.end_ms);
    } else {
      regions.push({ start_ms: current.start_ms, end_ms: current.end_ms });
    }
  }
  const windows = [];
  for (const region of regions) {
    const first = windows.length;
    for (let start = region.start_ms; start < region.end_ms; start += windowMs) {
      windows.push({ start_ms: start, end_ms: Math.min(region.end_ms, start + windowMs) });
    }
    const tail = windows.at(-1);
    if (tail && windows.length > first + 1 && tail.end_ms - tail.start_ms < minWindowMs) {
      windows[windows.length - 2].end_ms = tail.end_ms;
      windows.pop();
    }
  }
  return windows.map((window, index) => ({ index, ...window }));
}

function normalize(vector) {
  if (!Array.isArray(vector) || vector.length === 0 || vector.some((value) => !Number.isFinite(value))) {
    throw new Error("speaker encoder returned an invalid embedding");
  }
  const magnitude = Math.sqrt(vector.reduce((sum, value) => sum + value * value, 0));
  if (!(magnitude > 0)) throw new Error("speaker encoder returned a zero embedding");
  return vector.map((value) => value / magnitude);
}

function cosine(left, right) {
  if (left.length !== right.length) throw new Error("speaker embeddings have inconsistent dimensions");
  return left.reduce((sum, value, index) => sum + value * right[index], 0);
}

function centroid(records) {
  const width = records[0].embedding.length;
  return normalize(Array.from({ length: width }, (_, column) =>
    records.reduce((sum, record) => sum + record.embedding[column], 0) / records.length));
}

export function clusterSpeakerEmbeddings(input, options = {}) {
  const threshold = Number(options.threshold ?? DEFAULT_THRESHOLD);
  const maxSpeakers = options.maxSpeakers == null ? null : Number(options.maxSpeakers);
  if (!Number.isFinite(threshold) || threshold < -1 || threshold > 1) throw new Error("threshold must be between -1 and 1");
  if (maxSpeakers != null && (!Number.isInteger(maxSpeakers) || maxSpeakers < 1)) throw new Error("maxSpeakers must be a positive integer");
  const records = input.map((record) => ({ ...record, embedding: normalize(record.embedding) }));
  if (!records.length) throw new Error("at least one transcript segment is required");
  let clusters = records.map((record) => ({ records: [record], first: record.start_ms, centroid: record.embedding }));
  while (clusters.length > 1) {
    let best = null;
    for (let left = 0; left < clusters.length; left += 1) {
      for (let right = left + 1; right < clusters.length; right += 1) {
        const similarity = cosine(clusters[left].centroid, clusters[right].centroid);
        const candidate = { left, right, similarity,
          first: Math.min(clusters[left].first, clusters[right].first) };
        if (!best || similarity > best.similarity
            || (similarity === best.similarity && candidate.first < best.first)
            || (similarity === best.similarity && candidate.first === best.first && left < best.left)) best = candidate;
      }
    }
    const mustMerge = maxSpeakers != null && clusters.length > maxSpeakers;
    if (!mustMerge && best.similarity < threshold) break;
    const combined = [...clusters[best.left].records, ...clusters[best.right].records]
      .sort((a, b) => a.start_ms - b.start_ms || a.index - b.index);
    const merged = { records: combined, first: combined[0].start_ms, centroid: centroid(combined) };
    clusters = clusters.filter((_, index) => index !== best.left && index !== best.right);
    clusters.push(merged);
    clusters.sort((a, b) => a.first - b.first);
  }
  clusters.sort((a, b) => a.first - b.first);
  return clusters.flatMap((cluster, clusterIndex) => cluster.records.map((record) => {
    const ownSimilarity = cosine(record.embedding, cluster.centroid);
    const competingSimilarity = clusters
      .filter((other) => other !== cluster)
      .reduce((maximum, other) => Math.max(maximum, cosine(record.embedding, other.centroid)), -1);
    const margin = clusters.length === 1 ? 2 : ownSimilarity - competingSimilarity;
    return {
      start_ms: record.start_ms,
      end_ms: record.end_ms,
      speaker: `acoustic_cluster_${clusterIndex + 1}`,
      confidence: Math.round(Math.max(0, Math.min(1, ownSimilarity)) * 1_000_000) / 1_000_000,
      overlap: record.overlap === true,
      uncertain: cluster.records.length === 1 || margin < 0.05 || ownSimilarity < threshold,
    };
  })).sort((a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms || a.speaker.localeCompare(b.speaker));
}

async function embeddingWorker(helper, model) {
  const child = spawn(helper, ["--embedding-resident", model], { stdio: ["pipe", "pipe", "inherit"] });
  const lines = createInterface({ input: child.stdout });
  const pending = new Map();
  let readyResolve;
  let readyReject;
  const ready = new Promise((resolveReady, rejectReady) => { readyResolve = resolveReady; readyReject = rejectReady; });
  lines.on("line", (line) => {
    let event;
    try { event = JSON.parse(line); } catch { return; }
    if (event.event === "ready") readyResolve();
    else if (event.id && pending.has(event.id)) {
      const { resolve: resolveRequest, reject } = pending.get(event.id);
      pending.delete(event.id);
      if (event.event === "embedding") resolveRequest(event.embedding);
      else reject(new Error(event.detail || "speaker embedding failed"));
    }
  });
  child.once("error", readyReject);
  child.once("exit", (code) => {
    const error = new Error(`speaker embedding worker exited with code ${code}`);
    readyReject(error);
    for (const request of pending.values()) request.reject(error);
    pending.clear();
  });
  await ready;
  return {
    async extract(audio) {
      const id = randomUUID();
      const result = new Promise((resolveRequest, reject) => pending.set(id, { resolve: resolveRequest, reject }));
      child.stdin.write(`${JSON.stringify({ id, audio })}\n`);
      return result;
    },
    close() { child.stdin.end(); lines.close(); },
  };
}

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 2) {
    if (!argv[index]?.startsWith("--") || argv[index + 1] == null) throw new Error(`invalid argument: ${argv[index] ?? ""}`);
    args[argv[index].slice(2).replaceAll("-", "_")] = argv[index + 1];
  }
  return args;
}

async function run(args) {
  for (const required of ["media", "transcript", "helper", "model", "output"]) {
    if (!args[required]) throw new Error(`--${required.replaceAll("_", "-")} is required`);
  }
  const media = resolve(args.media);
  const transcriptPath = resolve(args.transcript);
  const transcript = parseArtifact(transcriptPath);
  if (!Array.isArray(transcript)) throw new Error("transcript must be a JSON array or JSONL records");
  const segments = acousticWindows(transcript, {
    windowMs: args.window_ms == null ? DEFAULT_WINDOW_MS : Number(args.window_ms),
    minWindowMs: args.min_window_ms == null ? DEFAULT_MIN_WINDOW_MS : Number(args.min_window_ms),
    joinGapMs: args.join_gap_ms == null ? DEFAULT_JOIN_GAP_MS : Number(args.join_gap_ms),
  });
  const scratch = mkdtempSync(join(tmpdir(), "mom-acoustic-diarizer-"));
  const worker = await embeddingWorker(resolve(args.helper), resolve(args.model));
  try {
    const embedded = [];
    for (const segment of segments) {
      const wav = join(scratch, `${String(segment.index).padStart(6, "0")}.wav`);
      execFileSync(args.ffmpeg || "ffmpeg", ["-v", "error", "-y", "-ss", String(segment.start_ms / 1000),
        "-t", String((segment.end_ms - segment.start_ms) / 1000), "-i", media,
        "-vn", "-ac", "1", "-ar", "24000", "-c:a", "pcm_s16le", wav]);
      embedded.push({ ...segment, embedding: await worker.extract(wav), overlap: transcript[segment.index]?.overlap === true });
    }
    const output = clusterSpeakerEmbeddings(embedded, {
      threshold: args.threshold == null ? DEFAULT_THRESHOLD : Number(args.threshold),
      maxSpeakers: args.max_speakers == null ? null : Number(args.max_speakers),
    });
    writeFileSync(resolve(args.output), `${JSON.stringify(output, null, 2)}\n`);
    const digest = createHash("sha256").update(readFileSync(resolve(args.output))).digest("hex");
    console.log(JSON.stringify({ status: "passed", source: basename(media), segments: output.length,
      speakers: new Set(output.map(({ speaker }) => speaker)).size, artifact_sha256: digest }));
  } finally {
    worker.close();
    rmSync(scratch, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  run(parseArgs(process.argv.slice(2))).catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
