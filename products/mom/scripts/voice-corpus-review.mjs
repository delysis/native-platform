#!/usr/bin/env node

import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

function fail(message) {
  throw new Error(message);
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function sha256Bytes(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function sha256File(path) {
  return sha256Bytes(readFileSync(path));
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function text(value, field) {
  if (typeof value !== "string" || value.trim() === "") fail(`${field} must be a non-empty string`);
  return value.trim();
}

function probability(value, field, { nullable = false } = {}) {
  if (nullable && value === null) return null;
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0 || value > 1) {
    fail(`${field} must be between 0 and 1${nullable ? " or null" : ""}`);
  }
  return value;
}

function labels(value, field) {
  if (!Array.isArray(value)) fail(`${field} must be an array`);
  return value.map((label, index) => text(label, `${field}[${index}]`));
}

function exactModelProvenance(value, field) {
  if (!value || typeof value !== "object") fail(`${field} is required`);
  text(value.model_id, `${field}.model_id`);
  text(value.model_revision_or_version, `${field}.model_revision_or_version`);
  text(value.runtime, `${field}.runtime`);
  return structuredClone(value);
}

function normalizeDiarizationProvenance(value, field) {
  if (!value || typeof value !== "object") fail(`${field} is required`);
  if (value.kind === "model") return { kind: "model", ...exactModelProvenance(value, field) };
  if (value.kind === "manual") {
    return {
      kind: "manual",
      method_id: text(value.method_id, `${field}.method_id`),
      method_version: text(value.method_version, `${field}.method_version`),
      runtime: text(value.runtime, `${field}.runtime`),
    };
  }
  fail(`${field}.kind must be model or manual`);
}

function normalizeStyleObservation(value, field) {
  if (!value || value.status === "unknown") {
    return {
      status: "unknown",
      emotion_labels: [],
      speaking_style_labels: [],
      confidence: null,
      provenance: null,
    };
  }
  if (value.status !== "machine_proposed") fail(`${field}.status must be unknown or machine_proposed`);
  return {
    status: "machine_proposed",
    emotion_labels: labels(value.emotion_labels ?? [], `${field}.emotion_labels`),
    speaking_style_labels: labels(value.speaking_style_labels ?? [], `${field}.speaking_style_labels`),
    confidence: probability(value.confidence, `${field}.confidence`, { nullable: true }),
    provenance: exactModelProvenance(value.provenance, `${field}.provenance`),
  };
}

function normalizeSegment(segment, index, manifestPath, diarizationProvenance) {
  const field = `segments[${index}]`;
  const clipPath = resolve(dirname(manifestPath), text(segment.clip_path, `${field}.clip_path`));
  if (!existsSync(clipPath)) fail(`${field}.clip_path does not exist: ${clipPath}`);
  const actualClipSha = sha256File(clipPath);
  if (actualClipSha !== text(segment.clip_sha256, `${field}.clip_sha256`)) {
    fail(`${field}.clip_sha256 does not match ${clipPath}`);
  }
  const transcript = text(segment.transcript?.text, `${field}.transcript.text`);
  const transcriptSha = sha256Bytes(Buffer.from(transcript, "utf8"));
  if (transcriptSha !== text(segment.transcript?.sha256, `${field}.transcript.sha256`)) {
    fail(`${field}.transcript.sha256 does not match its text`);
  }
  const start = segment.source_timestamp_start_seconds;
  const end = segment.source_timestamp_end_seconds;
  if (!Number.isFinite(start) || !Number.isFinite(end) || start < 0 || end <= start) {
    fail(`${field} has invalid source timestamps`);
  }
  const hypothesis = segment.speaker_identity_hypothesis ?? {};
  const hypothesisStatus = hypothesis.status ?? "unverified";
  if (!new Set(["unverified", "machine_proposed", "source_attributed"]).has(hypothesisStatus)) {
    fail(`${field}.speaker_identity_hypothesis.status is invalid`);
  }
  const evidenceCore = {
    source_url: text(segment.source_url, `${field}.source_url`),
    source_timestamp_start_seconds: start,
    source_timestamp_end_seconds: end,
    clip_sha256: actualClipSha,
    speaker_cluster_id: text(segment.speaker_cluster_id, `${field}.speaker_cluster_id`),
    transcript_sha256: transcriptSha,
    diarization_model: diarizationProvenance,
  };
  return {
    evidence_id: `voice-evidence-${sha256Bytes(Buffer.from(stableJson(evidenceCore))).slice(0, 20)}`,
    persona_id_hypothesis: text(segment.persona_id_hypothesis, `${field}.persona_id_hypothesis`),
    speaker_name_hypothesis: text(segment.speaker_name_hypothesis, `${field}.speaker_name_hypothesis`),
    source: {
      url: evidenceCore.source_url,
      title: text(segment.source_title, `${field}.source_title`),
      channel: text(segment.source_channel, `${field}.source_channel`),
      timestamp_start_seconds: start,
      timestamp_end_seconds: end,
    },
    clip: {
      path: clipPath,
      file_url: pathToFileURL(clipPath).href,
      sha256: actualClipSha,
      sample_rate_hz: segment.sample_rate_hz,
      channels: segment.channels,
      duration_seconds: segment.duration_seconds,
    },
    diarization: {
      speaker_cluster_id: evidenceCore.speaker_cluster_id,
      speaker_cluster_confidence: probability(
        segment.speaker_cluster_confidence ?? null,
        `${field}.speaker_cluster_confidence`,
        { nullable: true },
      ),
      provenance: diarizationProvenance,
    },
    speaker_identity_hypothesis: {
      status: hypothesisStatus,
      confidence: probability(hypothesis.confidence ?? null, `${field}.speaker_identity_hypothesis.confidence`, { nullable: true }),
      basis: text(hypothesis.basis, `${field}.speaker_identity_hypothesis.basis`),
    },
    transcript: {
      text: transcript,
      sha256: transcriptSha,
      provenance: exactModelProvenance(segment.transcript.provenance, `${field}.transcript.provenance`),
    },
    style_observation: normalizeStyleObservation(segment.style_observation, `${field}.style_observation`),
  };
}

export function buildQueue({ manifestPath, outputDirectory }) {
  const source = readJson(manifestPath);
  if (source.schema_version !== 1) fail("segment manifest schema_version must be 1");
  const diarizationProvenance = normalizeDiarizationProvenance(source.diarization_provenance, "diarization_provenance");
  if (!Array.isArray(source.segments) || source.segments.length === 0) fail("segments must be a non-empty array");
  const items = source.segments.map((segment, index) => normalizeSegment(segment, index, manifestPath, diarizationProvenance));
  const evidenceIds = new Set();
  for (const item of items) {
    if (evidenceIds.has(item.evidence_id)) fail(`duplicate evidence: ${item.evidence_id}`);
    evidenceIds.add(item.evidence_id);
  }
  items.sort((left, right) => left.persona_id_hypothesis.localeCompare(right.persona_id_hypothesis)
    || left.source.timestamp_start_seconds - right.source.timestamp_start_seconds);
  const manifestSha = sha256File(manifestPath);
  const queue = {
    schema_version: 1,
    source_manifest_path: resolve(manifestPath),
    source_manifest_sha256: manifestSha,
    evidence_immutability: "Queue and reviews reference source hashes; human decisions never rewrite source observations.",
    qwen3_tts_control_contract: {
      integrated_clone_route: "qwen3_tts_base_icl",
      identity_conditioning: "reference transcript plus reference audio codec codes",
      explicit_style_instruct_supported_with_clone_route: false,
      lower_level_speaker_embedding_plus_instruct_api_present: true,
      lower_level_speaker_embedding_plus_instruct_exposed_by_resident_wire_api: false,
      lower_level_speaker_embedding_plus_instruct_qualified_for_consultants: false,
      emotion_tokens_or_style_ids_present: false,
      style_strategy: "select a human-reviewed reference segment whose performed style matches the requested style",
      instruct: null,
    },
    items,
  };
  mkdirSync(outputDirectory, { recursive: true });
  const queuePath = join(outputDirectory, "review-queue.json");
  writeFileSync(queuePath, `${JSON.stringify(queue, null, 2)}\n`);
  writeFileSync(join(outputDirectory, "review-queue.html"), renderHtml(queue));
  return { queuePath, itemCount: items.length };
}

function parseReviews(path) {
  const body = readFileSync(path, "utf8").trim();
  if (!body) return [];
  if (body.startsWith("[")) return JSON.parse(body);
  return body.split("\n").filter(Boolean).map((line, index) => {
    try { return JSON.parse(line); } catch { fail(`invalid review JSON on line ${index + 1}`); }
  });
}

function normalizeReview(review, index, evidenceIds) {
  const field = `reviews[${index}]`;
  const evidenceId = text(review.evidence_id, `${field}.evidence_id`);
  if (!evidenceIds.has(evidenceId)) fail(`${field}.evidence_id is not in the review queue`);
  const decision = text(review.decision, `${field}.decision`);
  if (!new Set(["accept", "correct", "reject"]).has(decision)) fail(`${field}.decision is invalid`);
  const normalized = {
    review_id: text(review.review_id, `${field}.review_id`),
    evidence_id: evidenceId,
    decision,
    reviewer: text(review.reviewer, `${field}.reviewer`),
    reviewed_at: text(review.reviewed_at, `${field}.reviewed_at`),
    notes: typeof review.notes === "string" ? review.notes : "",
  };
  if (decision !== "reject") {
    normalized.speaker_identity = {
      persona_id: text(review.speaker_identity?.persona_id, `${field}.speaker_identity.persona_id`),
      speaker_name: text(review.speaker_identity?.speaker_name, `${field}.speaker_identity.speaker_name`),
      confidence: probability(review.speaker_identity?.confidence, `${field}.speaker_identity.confidence`),
    };
    const style = review.emotion_style ?? { status: "unknown" };
    if (!new Set(["accepted", "corrected", "unknown"]).has(style.status)) fail(`${field}.emotion_style.status is invalid`);
    normalized.emotion_style = {
      status: style.status,
      emotion_labels: style.status === "unknown" ? [] : labels(style.emotion_labels ?? [], `${field}.emotion_style.emotion_labels`),
      speaking_style_labels: style.status === "unknown" ? [] : labels(style.speaking_style_labels ?? [], `${field}.emotion_style.speaking_style_labels`),
      confidence: style.status === "unknown" ? null : probability(style.confidence, `${field}.emotion_style.confidence`),
    };
  }
  return normalized;
}

export function resolveReviews({ queuePath, reviewsPath, outputPath }) {
  const queue = readJson(queuePath);
  const byEvidence = new Map(queue.items.map((item) => [item.evidence_id, item]));
  const reviews = parseReviews(reviewsPath).map((review, index) => normalizeReview(review, index, new Set(byEvidence.keys())));
  const reviewIds = new Set();
  const decidedEvidence = new Set();
  for (const review of reviews) {
    if (reviewIds.has(review.review_id)) fail(`duplicate review_id: ${review.review_id}`);
    if (decidedEvidence.has(review.evidence_id)) fail(`multiple decisions for evidence require a new queue; duplicate: ${review.evidence_id}`);
    reviewIds.add(review.review_id);
    decidedEvidence.add(review.evidence_id);
  }
  const accepted = reviews.filter((review) => review.decision !== "reject").map((review) => {
    const evidence = byEvidence.get(review.evidence_id);
    return {
      persona_id: review.speaker_identity.persona_id,
      speaker_name: review.speaker_identity.speaker_name,
      evidence_id: review.evidence_id,
      review_id: review.review_id,
      reviewer: review.reviewer,
      source: evidence.source,
      diarization: evidence.diarization,
      emotion_style: review.emotion_style,
      qwen3_tts_base_icl: {
        ref_audio: evidence.clip.path,
        ref_audio_sha256: evidence.clip.sha256,
        ref_text: evidence.transcript.text,
        ref_text_sha256: evidence.transcript.sha256,
        instruct: null,
        style_control: "implicit_from_reviewed_reference_performance",
      },
    };
  });
  const result = {
    schema_version: 1,
    queue_path: resolve(queuePath),
    queue_sha256: sha256File(queuePath),
    reviews_path: resolve(reviewsPath),
    reviews_sha256: sha256File(reviewsPath),
    reviews,
    accepted_references: accepted,
  };
  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, `${JSON.stringify(result, null, 2)}\n`);
  return { outputPath, acceptedCount: accepted.length, rejectedCount: reviews.length - accepted.length };
}

function escapeHtml(value) {
  return String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;");
}

function renderHtml(queue) {
  const cards = queue.items.map((item) => `
    <article data-evidence="${escapeHtml(item.evidence_id)}">
      <h2>${escapeHtml(item.speaker_name_hypothesis)}</h2>
      <audio controls preload="metadata" src="${escapeHtml(item.clip.file_url)}"></audio>
      <dl><dt>Source</dt><dd><a href="${escapeHtml(item.source.url)}">${escapeHtml(item.source.title)}</a> ${item.source.timestamp_start_seconds}-${item.source.timestamp_end_seconds}s</dd>
      <dt>Identity evidence</dt><dd>${escapeHtml(item.speaker_identity_hypothesis.status)}; ${escapeHtml(item.speaker_identity_hypothesis.basis)}</dd>
      <dt>Cluster</dt><dd>${escapeHtml(item.diarization.speaker_cluster_id)}; confidence ${escapeHtml(item.diarization.speaker_cluster_confidence ?? "unknown")}</dd>
      <dt>Clip hash</dt><dd><code>${escapeHtml(item.clip.sha256)}</code></dd>
      <dt>Transcript</dt><dd>${escapeHtml(item.transcript.text)}</dd>
      <dt>Machine style</dt><dd>${escapeHtml(item.style_observation.status === "unknown" ? "unknown" : [...item.style_observation.emotion_labels, ...item.style_observation.speaking_style_labels].join(", "))}</dd></dl>
      <label>Decision <select class="decision"><option value="" selected>pending</option><option>accept</option><option>correct</option><option>reject</option></select></label>
      <label>Persona ID <input class="persona" value="${escapeHtml(item.persona_id_hypothesis)}"></label>
      <label>Speaker <input class="speaker" value="${escapeHtml(item.speaker_name_hypothesis)}"></label>
      <label>Identity confidence <input class="identity-confidence" type="number" min="0" max="1" step="0.01"></label>
      <label>Emotion labels <input class="emotions" placeholder="comma separated; blank means unknown"></label>
      <label>Style labels <input class="styles" placeholder="comma separated; blank means unknown"></label>
      <label>Label confidence <input class="style-confidence" type="number" min="0" max="1" step="0.01"></label>
      <label>Notes <textarea class="notes"></textarea></label>
    </article>`).join("\n");
  return `<!doctype html><meta charset="utf-8"><title>Mom voice evidence review</title>
  <style>body{font:16px system-ui;max-width:900px;margin:32px auto;padding:0 20px}article{border:1px solid #bbb;border-radius:16px;padding:20px;margin:20px 0}audio{width:100%}label{display:block;margin:10px 0}input,select,textarea{font:inherit;width:100%;box-sizing:border-box;padding:8px}dt{font-weight:700;margin-top:8px}dd{margin-left:0}.toolbar{position:sticky;top:0;background:#fff;padding:12px 0}</style>
  <div class="toolbar"><label>Reviewer <input id="reviewer" value="George"></label><button id="export">Export review decisions</button></div>
  <h1>Mom voice evidence review</h1>${cards}
  <script>const split=v=>v.split(',').map(x=>x.trim()).filter(Boolean);document.querySelector('#export').onclick=()=>{const reviewer=document.querySelector('#reviewer').value.trim();if(!reviewer){alert('Reviewer is required');return}const cards=[...document.querySelectorAll('article')];if(cards.some(card=>!card.querySelector('.decision').value)){alert('Every clip needs an explicit decision');return}const invalid=cards.find(card=>card.querySelector('.decision').value!=='reject'&&!card.querySelector('.identity-confidence').value);if(invalid){invalid.scrollIntoView();alert('Identity confidence is required for accepted or corrected clips');return}const missingStyleConfidence=cards.find(card=>{const hasLabels=split(card.querySelector('.emotions').value).length||split(card.querySelector('.styles').value).length;return hasLabels&&!card.querySelector('.style-confidence').value});if(missingStyleConfidence){missingStyleConfidence.scrollIntoView();alert('Label confidence is required when style labels are present');return}const reviews=cards.map((card,index)=>{const decision=card.querySelector('.decision').value;const emotions=split(card.querySelector('.emotions').value);const styles=split(card.querySelector('.styles').value);const styleConfidence=card.querySelector('.style-confidence').value;const value={review_id:'human-'+Date.now()+'-'+index,evidence_id:card.dataset.evidence,decision,reviewer,reviewed_at:new Date().toISOString(),notes:card.querySelector('.notes').value};if(decision!=='reject'){value.speaker_identity={persona_id:card.querySelector('.persona').value.trim(),speaker_name:card.querySelector('.speaker').value.trim(),confidence:Number(card.querySelector('.identity-confidence').value)};value.emotion_style=emotions.length||styles.length?{status:decision==='correct'?'corrected':'accepted',emotion_labels:emotions,speaking_style_labels:styles,confidence:Number(styleConfidence)}:{status:'unknown'}}return value});const blob=new Blob([JSON.stringify(reviews,null,2)+'\\n'],{type:'application/json'});const a=document.createElement('a');a.href=URL.createObjectURL(blob);a.download='voice-reviews-'+new Date().toISOString().replaceAll(':','-')+'.json';a.click();URL.revokeObjectURL(a.href)}</script>`;
}

function parseArgs(argv) {
  const command = argv.shift();
  const args = {};
  for (let index = 0; index < argv.length; index += 2) {
    if (!argv[index]?.startsWith("--") || argv[index + 1] === undefined) fail(`invalid argument: ${argv[index] ?? ""}`);
    args[argv[index].slice(2).replaceAll("-", "_")] = argv[index + 1];
  }
  return { command, args };
}

function main() {
  const { command, args } = parseArgs(process.argv.slice(2));
  if (command === "build") {
    const result = buildQueue({ manifestPath: resolve(text(args.manifest, "--manifest")), outputDirectory: resolve(text(args.output_dir, "--output-dir")) });
    console.log(JSON.stringify({ status: "passed", ...result }, null, 2));
  } else if (command === "resolve") {
    const result = resolveReviews({ queuePath: resolve(text(args.queue, "--queue")), reviewsPath: resolve(text(args.reviews, "--reviews")), outputPath: resolve(text(args.output, "--output")) });
    console.log(JSON.stringify({ status: "passed", ...result }, null, 2));
  } else {
    fail("usage: voice-corpus-review.mjs build --manifest PATH --output-dir DIR\n       voice-corpus-review.mjs resolve --queue PATH --reviews PATH --output PATH");
  }
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(new URL(import.meta.url).pathname)) {
  try { main(); } catch (error) { console.error(error.message); process.exit(1); }
}
