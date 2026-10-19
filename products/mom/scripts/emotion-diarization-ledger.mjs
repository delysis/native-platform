#!/usr/bin/env node

import { createHash, randomUUID } from "node:crypto";
import { execFileSync, spawn } from "node:child_process";
import { closeSync, existsSync, lstatSync, mkdirSync, mkdtempSync, openSync, readSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, relative, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import { ledgerSchema as speakerLedgerSchema, sha256 } from "./speaker-diarization-ledger.mjs";

export const emotionLedgerSchema = "mom-emotion-diarization-ledger-v1";
export const emotionReviewSchema = "mom-emotion-diarization-review-v1";
const digestPattern = /^[0-9a-f]{64}$/;
const supportedLabels = new Set(["angry", "disgusted", "fearful", "happy", "neutral", "other", "sad", "surprised", "unknown"]);

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function requireDigest(value, field) {
  if (!digestPattern.test(value ?? "")) throw new Error(`${field} must be a lowercase SHA-256 digest`);
}

function requireText(value, field) {
  if (typeof value !== "string" || value.trim() === "") throw new Error(`${field} must be a non-empty string`);
  return value.trim();
}

function probability(value, field) {
  const number = Number(value);
  if (!Number.isFinite(number) || number < 0 || number > 1) throw new Error(`${field} must be between 0 and 1`);
  return number;
}

function fileSha256(path) {
  const digest = createHash("sha256");
  const descriptor = openSync(path, "r");
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    for (;;) {
      const count = readSync(descriptor, buffer, 0, buffer.length, null);
      if (count === 0) break;
      digest.update(buffer.subarray(0, count));
    }
  } finally {
    closeSync(descriptor);
  }
  return digest.digest("hex");
}

function verifySpeakerLedger(ledger) {
  if (ledger?.schema !== speakerLedgerSchema) throw new Error("unsupported speaker diarization ledger schema");
  const { ledger_sha256: identity, ...contents } = ledger;
  requireDigest(identity, "speaker ledger identity");
  if (sha256(contents) !== identity) throw new Error("speaker diarization ledger content does not match its identity");
  return ledger;
}

function normalizedLabel(raw) {
  let label = String(raw ?? "").trim().toLowerCase().split("/").at(-1)?.replaceAll(/[<>|]/g, "");
  if (label === "unk") label = "unknown";
  if (!supportedLabels.has(label)) throw new Error(`unsupported emotion label: ${raw}`);
  return label;
}

function normalizeScores(prediction, segmentID) {
  const labels = prediction?.labels;
  const scores = prediction?.scores;
  if (!Array.isArray(labels) || !Array.isArray(scores) || labels.length !== scores.length || labels.length === 0) {
    throw new Error(`${segmentID} has invalid raw labels or scores`);
  }
  const seen = new Set();
  return labels.map((rawLabel, index) => {
    const label = normalizedLabel(rawLabel);
    if (seen.has(label)) throw new Error(`${segmentID} has duplicate normalized label ${label}`);
    seen.add(label);
    return { raw_label: String(rawLabel), label, score: probability(scores[index], `${segmentID}.scores[${index}]`) };
  }).sort((left, right) => right.score - left.score || left.label.localeCompare(right.label));
}

export function buildEmotionLedger({ speakerLedger, mediaPath, predictions, model, thresholds = {} }) {
  verifySpeakerLedger(speakerLedger);
  const absoluteMedia = resolve(mediaPath);
  if (!existsSync(absoluteMedia)) throw new Error(`media does not exist: ${absoluteMedia}`);
  const mediaSha256 = fileSha256(absoluteMedia);
  if (mediaSha256 !== speakerLedger.source.media_sha256) throw new Error("media hash does not match the speaker diarization ledger");
  const modelIdentity = {
    model_id: requireText(model?.model_id, "model.model_id"),
    model_revision: requireText(model?.model_revision, "model.model_revision"),
    model_manifest_sha256: requireText(model?.model_manifest_sha256, "model.model_manifest_sha256"),
    runtime: requireText(model?.runtime, "model.runtime"),
    runtime_version: requireText(model?.runtime_version, "model.runtime_version"),
    inference_contract: requireText(model?.inference_contract, "model.inference_contract"),
  };
  requireDigest(modelIdentity.model_manifest_sha256, "model.model_manifest_sha256");
  const minimumScore = probability(thresholds.minimum_score ?? 0.55, "thresholds.minimum_score");
  const minimumMargin = probability(thresholds.minimum_margin ?? 0.10, "thresholds.minimum_margin");
  const byID = new Map((predictions ?? []).map((prediction) => {
    const segmentID = requireText(prediction.segment_id, "prediction.segment_id");
    if (prediction.error != null) return [segmentID, { segment_id: segmentID, error: requireText(prediction.error, `${segmentID}.error`) }];
    return [segmentID, { segment_id: segmentID, raw_result: structuredClone(prediction.raw_result), scores: normalizeScores(prediction, segmentID) }];
  }));
  if (byID.size !== (predictions ?? []).length) throw new Error("predictions contain duplicate segment IDs");
  const known = new Set(speakerLedger.segments.map(({ segment_id }) => segment_id));
  for (const id of byID.keys()) if (!known.has(id)) throw new Error(`prediction refers to unknown speaker segment: ${id}`);

  const observations = speakerLedger.segments.map((segment) => {
    const prediction = byID.get(segment.segment_id);
    const reasons = [...(segment.uncertainty_reasons ?? []).map((reason) => `speaker:${reason}`)];
    if (!prediction) reasons.push("emotion:model_output_missing");
    if (prediction?.error) reasons.push("emotion:model_inference_failed");
    const scores = prediction?.scores ?? [];
    const top = scores[0] ?? null;
    const runnerUp = scores[1] ?? null;
    const margin = top == null ? null : top.score - (runnerUp?.score ?? 0);
    if (top?.score < minimumScore) reasons.push("emotion:low_uncalibrated_score");
    if (margin != null && margin < minimumMargin) reasons.push("emotion:low_score_margin");
    if (top?.label === "unknown") reasons.push("emotion:model_returned_unknown");
    if (top?.label === "other") reasons.push("emotion:model_returned_non_specific_other");
    const proposed = top != null && !new Set(["unknown", "other"]).has(top.label)
      ? { label: top.label, score: top.score }
      : null;
    return {
      segment_id: segment.segment_id,
      source: {
        source_id: speakerLedger.source.source_id,
        media_filename: speakerLedger.source.media_filename,
        media_sha256: mediaSha256,
        start_ms: segment.start_ms,
        end_ms: segment.end_ms,
      },
      speaker_diarization: {
        ledger_sha256: speakerLedger.ledger_sha256,
        speaker_id: segment.speaker_id,
        confidence: segment.confidence,
        overlap: segment.overlap,
        uncertain: segment.uncertain,
        uncertainty_reasons: structuredClone(segment.uncertainty_reasons ?? []),
      },
      transcript_refs: structuredClone(segment.transcript_refs ?? []),
      model: modelIdentity,
      observation: prediction?.error || !prediction ? {
        status: "unknown",
        raw_result: prediction?.error ? { error: prediction.error } : null,
        raw_scores: [],
        proposed_label: null,
      } : {
        status: proposed == null ? "unknown" : "machine_proposed",
        raw_result: prediction.raw_result,
        raw_scores: scores,
        proposed_label: proposed,
      },
      confidence: {
        calibration: "uncalibrated",
        top_score: top?.score ?? null,
        top_two_margin: margin,
        score_semantics: "model output score; not a calibrated probability or verified emotion",
      },
      uncertainty: {
        uncertain: reasons.length > 0,
        reasons,
      },
      human_review: null,
    };
  });
  const withoutIdentity = {
    schema: emotionLedgerSchema,
    source: {
      ...structuredClone(speakerLedger.source),
      media_path: absoluteMedia,
      media_file_url: pathToFileURL(absoluteMedia).href,
    },
    speaker_ledger_sha256: speakerLedger.ledger_sha256,
    model: modelIdentity,
    thresholds: { minimum_score: minimumScore, minimum_margin: minimumMargin },
    calibration_status: "uncalibrated",
    identity_boundary: "Emotion inference does not establish or verify speaker identity.",
    observations,
  };
  return { ...withoutIdentity, emotion_ledger_sha256: sha256(withoutIdentity) };
}

export function createEmotionReview(ledger) {
  verifyEmotionLedger(ledger);
  return {
    schema: emotionReviewSchema,
    source_id: ledger.source.source_id,
    emotion_ledger_sha256: ledger.emotion_ledger_sha256,
    reviewer: null,
    reviewed_at: null,
    decisions: ledger.observations.map(({ segment_id }) => ({
      segment_id,
      action: "pending",
      emotion_label: null,
      speaking_style_labels: [],
      usable_for_style_selection: false,
      notes: "",
    })),
  };
}

function verifyEmotionLedger(ledger) {
  if (ledger?.schema !== emotionLedgerSchema) throw new Error("unsupported emotion ledger schema");
  const { emotion_ledger_sha256: identity, ...contents } = ledger;
  requireDigest(identity, "emotion ledger identity");
  if (sha256(contents) !== identity) throw new Error("emotion ledger content does not match its identity");
  return ledger;
}

export function applyEmotionReview(ledger, review) {
  verifyEmotionLedger(ledger);
  if (review?.schema !== emotionReviewSchema) throw new Error("unsupported emotion review schema");
  if (review.source_id !== ledger.source.source_id || review.emotion_ledger_sha256 !== ledger.emotion_ledger_sha256) {
    throw new Error("emotion review is bound to a different emotion ledger");
  }
  const reviewer = requireText(review.reviewer, "review.reviewer");
  const reviewedAt = requireText(review.reviewed_at, "review.reviewed_at");
  const known = new Set(ledger.observations.map(({ segment_id }) => segment_id));
  const observations = new Map(ledger.observations.map((observation) => [observation.segment_id, observation]));
  const decisions = new Map();
  for (const decision of review.decisions ?? []) {
    const id = requireText(decision.segment_id, "decision.segment_id");
    if (!known.has(id) || decisions.has(id)) throw new Error("review has an unknown or duplicate segment decision");
    if (!new Set(["accept", "correct", "unknown", "reject"]).has(decision.action)) throw new Error(`${id} has an invalid review action`);
    const emotionLabel = new Set(["accept", "correct"]).has(decision.action)
      ? normalizedLabel(requireText(decision.emotion_label, `${id}.emotion_label`)) : null;
    if (emotionLabel === "unknown") throw new Error(`${id} must use the explicit unknown action`);
    const proposedLabel = observations.get(id).observation.proposed_label?.label ?? null;
    if (decision.action === "accept" && (proposedLabel == null || emotionLabel !== proposedLabel)) {
      throw new Error(`${id} accept must preserve the machine-proposed label; use correct or unknown instead`);
    }
    const styles = decision.speaking_style_labels ?? [];
    if (!Array.isArray(styles) || styles.some((value) => typeof value !== "string" || !value.trim())) {
      throw new Error(`${id}.speaking_style_labels must contain non-empty strings`);
    }
    const usableForStyleSelection = decision.usable_for_style_selection === true;
    if (usableForStyleSelection && !new Set(["accept", "correct"]).has(decision.action)) {
      throw new Error(`${id} cannot be a style reference without an accepted or corrected emotion`);
    }
    decisions.set(id, {
      segment_id: id,
      action: decision.action,
      emotion_label: emotionLabel,
      speaking_style_labels: styles.map((value) => value.trim()),
      usable_for_style_selection: usableForStyleSelection,
      notes: typeof decision.notes === "string" ? decision.notes : "",
    });
  }
  if (decisions.size !== known.size) throw new Error("review must decide every emotion segment");
  const withoutIdentity = {
    schema: emotionReviewSchema,
    source_id: review.source_id,
    emotion_ledger_sha256: review.emotion_ledger_sha256,
    reviewer,
    reviewed_at: reviewedAt,
    decisions: [...decisions.values()].sort((a, b) => a.segment_id.localeCompare(b.segment_id)),
  };
  const reviewSha256 = sha256(withoutIdentity);
  return {
    schema: "mom-reviewed-emotion-index-v1",
    source_id: ledger.source.source_id,
    emotion_ledger_sha256: ledger.emotion_ledger_sha256,
    review_sha256: reviewSha256,
    machine_observations_immutable: true,
    reviewer,
    reviewed_at: reviewedAt,
    segments: ledger.observations.map((observation) => ({
      segment_id: observation.segment_id,
      machine_observation: observation,
      human_review: decisions.get(observation.segment_id),
    })),
  };
}

function escapeHtml(value) {
  return String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;");
}

export function renderEmotionReviewHtml(ledger) {
  verifyEmotionLedger(ledger);
  const rows = ledger.observations.map((item, index) => {
    const proposed = item.observation.proposed_label?.label ?? "unknown";
    const transcript = item.transcript_refs.map(({ text }) => text).join(" ");
    const scores = item.observation.raw_scores.map(({ label, score }) => `${label} ${score.toFixed(3)}`).join(" · ");
    return `<article data-index="${index}" data-id="${escapeHtml(item.segment_id)}" data-start="${item.source.start_ms}" data-end="${item.source.end_ms}">
      <button class="play" aria-label="Play segment">▶</button>
      <div><strong>${escapeHtml(item.speaker_diarization.speaker_id)}</strong> <span>${(item.source.start_ms / 1000).toFixed(2)}–${(item.source.end_ms / 1000).toFixed(2)}s</span>
      <p>${escapeHtml(transcript || "No aligned transcript")}</p><small>${escapeHtml(scores || "No model output")}</small></div>
      <label>Decision<select class="action"><option value="pending">Pending</option><option value="accept">Accept</option><option value="correct">Correct</option><option value="unknown">Unknown</option><option value="reject">Reject</option></select></label>
      <label>Emotion<input class="emotion" value="${escapeHtml(proposed)}"></label>
      <label>Style<input class="styles" placeholder="warm, reflective"></label>
      <label class="check"><input class="usable" type="checkbox"> Style reference</label>
      <label>Notes<input class="notes"></label>
    </article>`;
  }).join("\n");
  return `<!doctype html><meta charset="utf-8"><title>Emotion review</title><style>
    :root{font:15px system-ui;color:#1d1d1f;background:#f5f5f7}body{max-width:1280px;margin:24px auto;padding:0 18px}header{display:flex;gap:12px;align-items:center;position:sticky;top:0;background:#f5f5f7;padding:12px 0;z-index:2}article{display:grid;grid-template-columns:48px minmax(280px,1fr) repeat(5,minmax(120px,.35fr));gap:12px;align-items:center;background:white;padding:14px;border-radius:14px;margin:10px 0}button,input,select{font:inherit;padding:8px;border:1px solid #ccc;border-radius:8px}.play{width:42px;height:42px;border-radius:50%}label{display:grid;gap:4px;font-size:12px;color:#666}.check{display:flex;align-items:center}p{margin:5px 0}small{color:#666} @media(max-width:900px){article{grid-template-columns:48px 1fr}article>label{grid-column:2}}</style>
    <header><h1>Emotion review</h1><input id="reviewer" placeholder="Reviewer"><button id="export">Export</button></header>
    <audio id="audio" src="${escapeHtml(ledger.source.media_file_url)}"></audio>${rows}<script>
    const audio=document.querySelector('#audio');let stopAt=0,timer=null;
    document.querySelectorAll('.play').forEach(b=>b.onclick=()=>{const row=b.closest('article');audio.currentTime=Number(row.dataset.start)/1000;stopAt=Number(row.dataset.end)/1000;audio.play();clearInterval(timer);timer=setInterval(()=>{if(audio.currentTime>=stopAt){audio.pause();clearInterval(timer)}},25)});
    document.querySelector('#export').onclick=()=>{const out={schema:'${emotionReviewSchema}',source_id:${JSON.stringify(ledger.source.source_id)},emotion_ledger_sha256:${JSON.stringify(ledger.emotion_ledger_sha256)},reviewer:document.querySelector('#reviewer').value,reviewed_at:new Date().toISOString(),decisions:[...document.querySelectorAll('article')].map(row=>({segment_id:row.dataset.id,action:row.querySelector('.action').value,emotion_label:row.querySelector('.emotion').value||null,speaking_style_labels:row.querySelector('.styles').value.split(',').map(x=>x.trim()).filter(Boolean),usable_for_style_selection:row.querySelector('.usable').checked,notes:row.querySelector('.notes').value}))};const a=document.createElement('a');a.href=URL.createObjectURL(new Blob([JSON.stringify(out,null,2)+'\\n'],{type:'application/json'}));a.download='emotion-review.json';a.click()};</script>`;
}

function modelManifest(directory) {
  const root = realpathSync(resolve(directory));
  const records = [];
  function walk(path) {
    for (const name of readdirSync(path).sort()) {
      if (name === ".cache" || name === ".locks") continue;
      const file = join(path, name);
      const stat = lstatSync(file);
      if (stat.isDirectory()) walk(file);
      else if (stat.isFile() || stat.isSymbolicLink()) {
        const resolvedFile = realpathSync(file);
        records.push({ path: relative(root, file), bytes: lstatSync(resolvedFile).size, sha256: fileSha256(resolvedFile) });
      }
    }
  }
  walk(root);
  if (!records.length) throw new Error("model directory has no files");
  return { files: records, sha256: sha256(records) };
}

const funasrWorkerProgram = String.raw`
import importlib.metadata, json, sys
from funasr import AutoModel
model = AutoModel(model=sys.argv[1], disable_update=True)
print("MOM_EMOTION_EVENT=" + json.dumps({"event":"ready","runtime_version":importlib.metadata.version("funasr")}), flush=True)
for line in sys.stdin:
    req = json.loads(line)
    try:
        result = model.generate(req["audio"], granularity="utterance", extract_embedding=False)
        value = result[0] if isinstance(result, list) else result
        print("MOM_EMOTION_EVENT=" + json.dumps({"event":"result","id":req["id"],"result":value}, ensure_ascii=False), flush=True)
    except Exception as error:
        print("MOM_EMOTION_EVENT=" + json.dumps({"event":"error","id":req["id"],"detail":str(error)}), flush=True)
`;

async function emotionWorker(python, modelDirectory) {
  const child = spawn(resolve(python), ["-c", funasrWorkerProgram, resolve(modelDirectory)], { stdio: ["pipe", "pipe", "inherit"], env: { ...process.env, HF_HUB_OFFLINE: "1", MODELSCOPE_OFFLINE: "1" } });
  const lines = createInterface({ input: child.stdout });
  const pending = new Map();
  let readyResolve; let readyReject;
  const ready = new Promise((resolveReady, rejectReady) => { readyResolve = resolveReady; readyReject = rejectReady; });
  let runtimeVersion = null;
  lines.on("line", (line) => {
    if (!line.startsWith("MOM_EMOTION_EVENT=")) return;
    let event;
    try { event = JSON.parse(line.slice("MOM_EMOTION_EVENT=".length)); } catch { return; }
    if (event.event === "ready") { runtimeVersion = event.runtime_version; readyResolve(); return; }
    const request = pending.get(event.id);
    if (!request) return;
    pending.delete(event.id);
    if (event.event === "result") request.resolve(event.result); else request.reject(new Error(event.detail || "emotion inference failed"));
  });
  child.once("error", readyReject);
  child.once("exit", (code) => {
    const error = new Error(`emotion worker exited with code ${code}`);
    readyReject(error);
    for (const request of pending.values()) request.reject(error);
    pending.clear();
  });
  await ready;
  return {
    runtimeVersion: () => runtimeVersion,
    classify(audio) {
      const id = randomUUID();
      const result = new Promise((resolveResult, reject) => pending.set(id, { resolve: resolveResult, reject }));
      child.stdin.write(`${JSON.stringify({ id, audio })}\n`);
      return result;
    },
    close() { child.stdin.end(); lines.close(); },
  };
}

function predictionsFromResult(segmentID, result) {
  const labels = result?.labels ?? result?.label;
  const scores = result?.scores ?? result?.score;
  return { segment_id: segmentID, labels, scores, raw_result: result };
}

function parseArgs(argv) {
  const [command, ...rest] = argv;
  const args = { command };
  for (let index = 0; index < rest.length; index += 2) {
    if (!rest[index]?.startsWith("--") || rest[index + 1] == null) throw new Error(`invalid argument: ${rest[index] ?? ""}`);
    args[rest[index].slice(2).replaceAll("-", "_")] = rest[index + 1];
  }
  return args;
}

async function annotate(args) {
  for (const field of ["speaker_ledger", "media", "python", "model", "model_id", "model_revision", "output_dir"]) {
    if (!args[field]) throw new Error(`--${field.replaceAll("_", "-")} is required`);
  }
  const speakerLedger = JSON.parse(readFileSync(resolve(args.speaker_ledger), "utf8"));
  verifySpeakerLedger(speakerLedger);
  const media = resolve(args.media);
  const manifest = modelManifest(args.model);
  const scratch = mkdtempSync(join(tmpdir(), "mom-emotion-diarization-"));
  const worker = await emotionWorker(args.python, args.model);
  try {
    const predictions = [];
    for (const [index, segment] of speakerLedger.segments.entries()) {
      const clip = join(scratch, `${String(index).padStart(6, "0")}.wav`);
      execFileSync(args.ffmpeg ?? "ffmpeg", ["-v", "error", "-y", "-ss", String(segment.start_ms / 1000), "-t", String((segment.end_ms - segment.start_ms) / 1000), "-i", media, "-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", clip]);
      try { predictions.push(predictionsFromResult(segment.segment_id, await worker.classify(clip))); }
      catch (error) { predictions.push({ segment_id: segment.segment_id, error: error instanceof Error ? error.message : String(error) }); }
    }
    const ledger = buildEmotionLedger({
      speakerLedger,
      mediaPath: media,
      predictions,
      model: {
        model_id: args.model_id,
        model_revision: args.model_revision,
        model_manifest_sha256: manifest.sha256,
        runtime: "FunASR local offline worker",
        runtime_version: worker.runtimeVersion(),
        inference_contract: "16 kHz mono, utterance granularity, nine categorical scores, extract_embedding=false",
      },
      thresholds: { minimum_score: Number(args.minimum_score ?? 0.55), minimum_margin: Number(args.minimum_margin ?? 0.10) },
    });
    const outputDirectory = resolve(args.output_dir);
    mkdirSync(outputDirectory, { recursive: true });
    const ledgerPath = join(outputDirectory, `${speakerLedger.source.source_id}.emotion-ledger.json`);
    const reviewPath = join(outputDirectory, `${speakerLedger.source.source_id}.emotion-review.json`);
    const htmlPath = join(outputDirectory, `${speakerLedger.source.source_id}.emotion-review.html`);
    writeFileSync(ledgerPath, `${JSON.stringify(ledger, null, 2)}\n`);
    writeFileSync(reviewPath, `${JSON.stringify(createEmotionReview(ledger), null, 2)}\n`);
    writeFileSync(htmlPath, renderEmotionReviewHtml(ledger));
    console.log(JSON.stringify({ status: "passed", ledger: ledgerPath, review: reviewPath, html: htmlPath, observations: ledger.observations.length, unknown: ledger.observations.filter(({ observation }) => observation.status === "unknown").length }));
  } finally {
    worker.close();
    rmSync(scratch, { recursive: true, force: true });
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const args = parseArgs(process.argv.slice(2));
  if (args.command === "annotate") annotate(args).catch((error) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; });
  else if (args.command === "resolve") {
    if (!args.ledger || !args.review || !args.output) throw new Error("resolve requires --ledger, --review, and --output");
    const result = applyEmotionReview(JSON.parse(readFileSync(resolve(args.ledger), "utf8")), JSON.parse(readFileSync(resolve(args.review), "utf8")));
    writeFileSync(resolve(args.output), `${JSON.stringify(result, null, 2)}\n`);
    console.log(JSON.stringify({ status: "passed", output: resolve(args.output), review_sha256: result.review_sha256 }));
  } else throw new Error("expected annotate or resolve");
}
