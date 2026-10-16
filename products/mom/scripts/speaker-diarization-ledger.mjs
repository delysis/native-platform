#!/usr/bin/env node

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";

export const ledgerSchema = "mom-speaker-diarization-ledger-v1";
export const reviewSchema = "mom-speaker-diarization-review-v1";
export const attributedTranscriptSchema = "mom-speaker-attributed-transcript-v1";
const digestPattern = /^[0-9a-f]{64}$/;
const personaPattern = /^[a-z0-9]+(?:[_-][a-z0-9]+)*$/;
const speakerPattern = /^speaker_[1-9][0-9]*$/;

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

export function sha256(value) {
  const bytes = Buffer.isBuffer(value) ? value : Buffer.from(typeof value === "string" ? value : canonical(value));
  return createHash("sha256").update(bytes).digest("hex");
}

function requireDigest(value, field) {
  if (!digestPattern.test(value ?? "")) throw new Error(`${field} must be a lowercase SHA-256 digest`);
}

function milliseconds(segment, prefix) {
  const start = segment.start_ms ?? Math.round(Number(segment.start_seconds) * 1000);
  const end = segment.end_ms ?? Math.round(Number(segment.end_seconds) * 1000);
  if (!Number.isInteger(start) || !Number.isInteger(end) || start < 0 || end <= start) {
    throw new Error(`${prefix} has an invalid time range`);
  }
  return [start, end];
}

function transcriptRecord(segment, index) {
  const [startMs, endMs] = milliseconds(segment, `transcript segment ${index}`);
  const text = String(segment.text ?? "").trim();
  if (!text) throw new Error(`transcript segment ${index} has no text`);
  return {
    transcript_segment_id: segment.segment_id ?? `transcript_${index + 1}`,
    start_ms: startMs,
    end_ms: endMs,
    text,
    text_sha256: sha256(text),
  };
}

function diarizationRecord(segment, index) {
  const [startMs, endMs] = milliseconds(segment, `diarization segment ${index}`);
  const cluster = String(segment.speaker ?? segment.cluster ?? "").trim();
  if (!cluster) throw new Error(`diarization segment ${index} has no speaker cluster`);
  const confidence = segment.confidence == null ? null : Number(segment.confidence);
  if (confidence != null && (!Number.isFinite(confidence) || confidence < 0 || confidence > 1)) {
    throw new Error(`diarization segment ${index} has an invalid confidence`);
  }
  return {
    input_index: index,
    start_ms: startMs,
    end_ms: endMs,
    cluster,
    confidence,
    input_overlap: segment.overlap === true,
    input_uncertain: segment.uncertain === true,
  };
}

function intersection(leftStart, leftEnd, rightStart, rightEnd) {
  return Math.max(0, Math.min(leftEnd, rightEnd) - Math.max(leftStart, rightStart));
}

export function buildDiarizationLedger(input) {
  if (input?.schema !== "mom-speaker-diarization-input-v1") throw new Error("unsupported diarization input schema");
  if (!input.source?.source_id) throw new Error("source.source_id is required");
  requireDigest(input.source.media_sha256, "source.media_sha256");
  requireDigest(input.transcript?.artifact_sha256, "transcript.artifact_sha256");
  requireDigest(input.diarization?.artifact_sha256, "diarization.artifact_sha256");
  requireDigest(input.diarization?.config_sha256, "diarization.config_sha256");
  if (input.diarization?.kind !== "acoustic_diarization") {
    throw new Error("diarization.kind must be acoustic_diarization; captions are not diarization");
  }
  if (!input.transcript?.tool?.id || !input.transcript?.tool?.revision) throw new Error("transcript tool identity is incomplete");
  if (!input.diarization?.tool?.id || !input.diarization?.tool?.revision) throw new Error("diarization tool identity is incomplete");

  const transcript = (input.transcript.segments ?? []).map(transcriptRecord)
    .sort((a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms || a.transcript_segment_id.localeCompare(b.transcript_segment_id));
  const diarization = (input.diarization.segments ?? []).map(diarizationRecord)
    .sort((a, b) => a.start_ms - b.start_ms || a.end_ms - b.end_ms || a.cluster.localeCompare(b.cluster));
  if (!transcript.length) throw new Error("transcript.segments must be non-empty");
  if (!diarization.length) throw new Error("diarization.segments must be non-empty");

  const firstAppearance = new Map();
  for (const segment of diarization) {
    const current = firstAppearance.get(segment.cluster);
    if (current == null || segment.start_ms < current) firstAppearance.set(segment.cluster, segment.start_ms);
  }
  const clusters = [...firstAppearance].sort((a, b) => a[1] - b[1] || a[0].localeCompare(b[0]));
  const anonymous = new Map(clusters.map(([cluster], index) => [cluster, `speaker_${index + 1}`]));

  const segments = diarization.map((segment) => {
    const concurrentSpeakers = new Set(diarization.filter((other) =>
      other.input_index !== segment.input_index && other.cluster !== segment.cluster
      && intersection(segment.start_ms, segment.end_ms, other.start_ms, other.end_ms) > 0
    ).map((other) => anonymous.get(other.cluster)));
    const transcriptRefs = transcript.map((item) => ({
      item,
      overlap_ms: intersection(segment.start_ms, segment.end_ms, item.start_ms, item.end_ms),
    })).filter(({ overlap_ms }) => overlap_ms > 0).map(({ item, overlap_ms }) => ({
      transcript_segment_id: item.transcript_segment_id,
      transcript_sha256: item.text_sha256,
      start_ms: item.start_ms,
      end_ms: item.end_ms,
      overlap_ms: overlap_ms,
      text: item.text,
    }));
    const overlap = segment.input_overlap || concurrentSpeakers.size > 0;
    const reasons = [];
    if (segment.input_uncertain) reasons.push("diarizer_marked_uncertain");
    if (segment.confidence == null) reasons.push("confidence_unavailable");
    else if (segment.confidence < (input.diarization.minimum_confidence ?? 0.75)) reasons.push("low_diarization_confidence");
    if (!transcriptRefs.length) reasons.push("no_overlapping_transcript");
    if (overlap) reasons.push("overlapping_speakers");
    const speakerID = anonymous.get(segment.cluster);
    return {
      segment_id: `sha256:${sha256({ source_id: input.source.source_id, start_ms: segment.start_ms, end_ms: segment.end_ms, speaker_id: speakerID, diarization_sha256: input.diarization.artifact_sha256 })}`,
      start_ms: segment.start_ms,
      end_ms: segment.end_ms,
      speaker_id: speakerID,
      confidence: segment.confidence,
      overlap,
      overlapping_speakers: [...concurrentSpeakers].sort(),
      uncertain: reasons.length > 0,
      uncertainty_reasons: reasons,
      transcript_refs: transcriptRefs,
    };
  });

  const withoutIdentity = {
    schema: ledgerSchema,
    source: structuredClone(input.source),
    transcript_provenance: {
      artifact_sha256: input.transcript.artifact_sha256,
      kind: input.transcript.kind ?? "asr_or_caption_segments",
      tool: structuredClone(input.transcript.tool),
      segment_count: transcript.length,
    },
    diarization_provenance: {
      artifact_sha256: input.diarization.artifact_sha256,
      config_sha256: input.diarization.config_sha256,
      kind: input.diarization.kind,
      tool: structuredClone(input.diarization.tool),
      minimum_confidence: input.diarization.minimum_confidence ?? 0.75,
    },
    speakers: clusters.map(([cluster], index) => ({
      speaker_id: `speaker_${index + 1}`,
      diarizer_cluster_sha256: sha256(cluster),
      first_observed_ms: firstAppearance.get(cluster),
    })),
    segments,
  };
  return { ...withoutIdentity, ledger_sha256: sha256(withoutIdentity) };
}

export function createReviewTemplate(ledger) {
  return {
    schema: reviewSchema,
    source_id: ledger.source.source_id,
    ledger_sha256: ledger.ledger_sha256,
    reviewer: null,
    reviewed_at: null,
    decisions: ledger.speakers.map(({ speaker_id }) => ({ speaker_id, action: "pending", evidence: "" })),
  };
}

export function attributeTranscriptSegments(ledger) {
  if (ledger?.schema !== ledgerSchema) throw new Error("unsupported ledger schema");
  const { ledger_sha256: ledgerIdentity, ...ledgerContents } = ledger;
  if (!digestPattern.test(ledgerIdentity ?? "") || sha256(ledgerContents) !== ledgerIdentity) {
    throw new Error("diarization ledger content does not match its identity");
  }

  const transcripts = new Map();
  for (const acousticSegment of ledger.segments) {
    if (!speakerPattern.test(acousticSegment.speaker_id ?? "")) {
      throw new Error(`invalid anonymous speaker id: ${acousticSegment.speaker_id ?? ""}`);
    }
    for (const reference of acousticSegment.transcript_refs ?? []) {
      let record = transcripts.get(reference.transcript_segment_id);
      if (!record) {
        record = {
          transcript_segment_id: reference.transcript_segment_id,
          transcript_sha256: reference.transcript_sha256,
          start_ms: reference.start_ms,
          end_ms: reference.end_ms,
          text: reference.text,
          speakers: new Map(),
        };
        transcripts.set(reference.transcript_segment_id, record);
      } else if (record.transcript_sha256 !== reference.transcript_sha256
          || record.start_ms !== reference.start_ms || record.end_ms !== reference.end_ms
          || record.text !== reference.text) {
        throw new Error(`inconsistent transcript evidence: ${reference.transcript_segment_id}`);
      }
      const speaker = record.speakers.get(acousticSegment.speaker_id) ?? {
        speaker_id: acousticSegment.speaker_id,
        overlap_ms: 0,
        weighted_confidence: 0,
        confidence_weight_ms: 0,
        uncertain: false,
        overlap: false,
      };
      speaker.overlap_ms += reference.overlap_ms;
      if (acousticSegment.confidence != null) {
        speaker.weighted_confidence += acousticSegment.confidence * reference.overlap_ms;
        speaker.confidence_weight_ms += reference.overlap_ms;
      }
      speaker.uncertain ||= acousticSegment.uncertain === true;
      speaker.overlap ||= acousticSegment.overlap === true;
      record.speakers.set(acousticSegment.speaker_id, speaker);
    }
  }

  const segments = [...transcripts.values()]
    .sort((left, right) => left.start_ms - right.start_ms || left.end_ms - right.end_ms
      || left.transcript_segment_id.localeCompare(right.transcript_segment_id))
    .map((record) => {
      const speakers = [...record.speakers.values()]
        .sort((left, right) => right.overlap_ms - left.overlap_ms
          || left.speaker_id.localeCompare(right.speaker_id))
        .map((speaker) => ({
          speaker_id: speaker.speaker_id,
          overlap_ms: speaker.overlap_ms,
          confidence: speaker.confidence_weight_ms > 0
            ? Math.round((speaker.weighted_confidence / speaker.confidence_weight_ms) * 1_000_000) / 1_000_000
            : null,
          uncertain: speaker.uncertain,
          overlap: speaker.overlap,
        }));
      const ambiguous = speakers.length !== 1 || speakers.some(({ uncertain, overlap }) => uncertain || overlap);
      return {
        transcript_segment_id: record.transcript_segment_id,
        transcript_sha256: record.transcript_sha256,
        start_ms: record.start_ms,
        end_ms: record.end_ms,
        text: record.text,
        speaker_id: speakers.length === 1 ? speakers[0].speaker_id : null,
        speaker_ids: speakers.map(({ speaker_id }) => speaker_id),
        speaker_evidence: speakers,
        speaker_assignment: speakers.length === 0 ? "unassigned" : speakers.length === 1 ? "single" : "multiple",
        speaker_uncertain: ambiguous,
      };
    });
  const withoutIdentity = {
    schema: attributedTranscriptSchema,
    source_id: ledger.source.source_id,
    ledger_sha256: ledger.ledger_sha256,
    segments,
  };
  return { ...withoutIdentity, attributed_transcript_sha256: sha256(withoutIdentity) };
}

export function applySpeakerReview(ledger, review) {
  if (ledger?.schema !== ledgerSchema) throw new Error("unsupported ledger schema");
  const { ledger_sha256: ledgerIdentity, ...ledgerContents } = ledger;
  if (!digestPattern.test(ledgerIdentity ?? "") || sha256(ledgerContents) !== ledgerIdentity) {
    throw new Error("diarization ledger content does not match its identity");
  }
  if (review?.schema !== reviewSchema) throw new Error("unsupported review schema");
  if (review.source_id !== ledger.source.source_id || review.ledger_sha256 !== ledger.ledger_sha256) {
    throw new Error("review is bound to a different diarization ledger");
  }
  if (!review.reviewer || !review.reviewed_at) throw new Error("reviewer and reviewed_at are required");
  const known = new Set(ledger.speakers.map(({ speaker_id }) => {
    if (!speakerPattern.test(speaker_id)) throw new Error(`invalid anonymous speaker id: ${speaker_id}`);
    return speaker_id;
  }));
  const decisions = new Map();
  for (const decision of review.decisions ?? []) {
    if (!known.has(decision.speaker_id) || decisions.has(decision.speaker_id)) throw new Error("review has an unknown or duplicate speaker decision");
    if (!decision.evidence?.trim()) throw new Error(`${decision.speaker_id} review evidence is required`);
    if (decision.action === "bind_persona") {
      if (!personaPattern.test(decision.persona_id ?? "")) throw new Error(`${decision.speaker_id} has an invalid persona_id`);
    } else if (decision.action === "merge_into") {
      if (!known.has(decision.target_speaker_id) || decision.target_speaker_id === decision.speaker_id) throw new Error(`${decision.speaker_id} has an invalid merge target`);
    } else if (decision.action !== "reject") {
      throw new Error(`${decision.speaker_id} review is still pending or invalid`);
    }
    decisions.set(decision.speaker_id, structuredClone(decision));
  }
  if (decisions.size !== known.size) throw new Error("review must decide every anonymous speaker");

  function resolve(speakerID, path = []) {
    if (path.includes(speakerID)) throw new Error(`speaker merge cycle: ${[...path, speakerID].join(" -> ")}`);
    const decision = decisions.get(speakerID);
    if (decision.action === "merge_into") return resolve(decision.target_speaker_id, [...path, speakerID]);
    return decision;
  }
  const resolved = Object.fromEntries([...known].sort().map((speakerID) => [speakerID, resolve(speakerID)]));
  const reviewWithoutIdentity = {
    schema: reviewSchema,
    source_id: review.source_id,
    ledger_sha256: review.ledger_sha256,
    reviewer: review.reviewer,
    reviewed_at: review.reviewed_at,
    decisions: [...decisions.values()].sort((a, b) => a.speaker_id.localeCompare(b.speaker_id)),
  };
  const reviewSha256 = sha256(reviewWithoutIdentity);
  return {
    ...reviewWithoutIdentity,
    review_sha256: reviewSha256,
    resolved_speakers: Object.fromEntries(Object.entries(resolved).map(([speakerID, decision]) => [speakerID, {
      status: decision.action === "bind_persona" ? "verified_persona" : "rejected",
      persona_id: decision.action === "bind_persona" ? decision.persona_id : null,
      decision_sha256: sha256({ ledger_sha256: ledger.ledger_sha256, review_sha256: reviewSha256, speaker_id: speakerID, decision }),
    }])),
    segments: ledger.segments.map((segment) => {
      const identity = resolved[segment.speaker_id];
      return {
        ...segment,
        identity: {
          decision: identity.action === "bind_persona" ? "verified_persona" : "rejected",
          persona_id: identity.action === "bind_persona" ? identity.persona_id : null,
          review_sha256: reviewSha256,
        },
        admission_candidate: identity.action === "bind_persona" && !segment.overlap && !segment.uncertain,
      };
    }),
  };
}

function readJSONOrJSONL(path) {
  const text = readFileSync(path, "utf8");
  if (path.endsWith(".jsonl")) return text.split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
  return JSON.parse(text);
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

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = parseArgs(process.argv.slice(2));
  if (args.command === "build") {
    if (!args.input || !args.output) throw new Error("usage: speaker-diarization-ledger.mjs build --input INPUT.json --output LEDGER.json [--review-template REVIEW.json] [--attributed-transcript TRANSCRIPT.json]");
    const specPath = resolve(args.input);
    const spec = readJSONOrJSONL(specPath);
    for (const field of ["transcript", "diarization"]) {
      if (spec[field]?.path) {
        const artifactPath = resolve(dirname(specPath), spec[field].path);
        const bytes = readFileSync(artifactPath);
        if (sha256(bytes) !== spec[field].artifact_sha256) throw new Error(`${field} artifact hash does not match`);
        spec[field].segments = readJSONOrJSONL(artifactPath);
      }
    }
    const ledger = buildDiarizationLedger(spec);
    writeFileSync(resolve(args.output), `${JSON.stringify(ledger, null, 2)}\n`);
    if (args.review_template) writeFileSync(resolve(args.review_template), `${JSON.stringify(createReviewTemplate(ledger), null, 2)}\n`);
    if (args.attributed_transcript) writeFileSync(resolve(args.attributed_transcript), `${JSON.stringify(attributeTranscriptSegments(ledger), null, 2)}\n`);
    console.log(JSON.stringify({ status: "passed", source_id: ledger.source.source_id, speakers: ledger.speakers.length, segments: ledger.segments.length, ledger_sha256: ledger.ledger_sha256 }));
  } else if (args.command === "attribute") {
    if (!args.ledger || !args.output) throw new Error("usage: speaker-diarization-ledger.mjs attribute --ledger LEDGER.json --output TRANSCRIPT.json");
    const attributed = attributeTranscriptSegments(readJSONOrJSONL(resolve(args.ledger)));
    writeFileSync(resolve(args.output), `${JSON.stringify(attributed, null, 2)}\n`);
    console.log(JSON.stringify({ status: "passed", source_id: attributed.source_id, segments: attributed.segments.length, attributed_transcript_sha256: attributed.attributed_transcript_sha256 }));
  } else if (args.command === "review") {
    if (!args.ledger || !args.review || !args.output) throw new Error("usage: speaker-diarization-ledger.mjs review --ledger LEDGER.json --review REVIEW.json --output RESOLVED.json");
    const resolvedReview = applySpeakerReview(readJSONOrJSONL(resolve(args.ledger)), readJSONOrJSONL(resolve(args.review)));
    writeFileSync(resolve(args.output), `${JSON.stringify(resolvedReview, null, 2)}\n`);
    console.log(JSON.stringify({ status: "passed", review_sha256: resolvedReview.review_sha256, admission_candidates: resolvedReview.segments.filter((segment) => segment.admission_candidate).length }));
  } else {
    throw new Error("expected build, attribute, or review command");
  }
}
