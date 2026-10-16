#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

export const schema = "mom-persona-training-admission-v1";

const digest = /^[0-9a-f]{64}$/;
const persona = /^[a-z0-9]+(?:[_-][a-z0-9]+)*$/;

function requireDigest(value, field, errors) {
  if (!digest.test(value ?? "")) errors.push(`${field} must be a lowercase SHA-256 digest`);
}

export function validateTrainingAdmission(value) {
  const errors = [];
  if (value?.schema !== schema) errors.push(`schema must be ${schema}`);
  if (!persona.test(value?.persona_id ?? "")) errors.push("persona_id is invalid");
  if (value?.rights_record != null) {
    requireDigest(value.rights_record.sha256, "rights_record.sha256", errors);
    if (!value.rights_record.status) errors.push("rights_record.status is required when a rights record is supplied");
  }
  if (!Array.isArray(value?.sources) || value.sources.length === 0) errors.push("sources must be non-empty");
  if (!Array.isArray(value?.segments) || value.segments.length === 0) errors.push("segments must be non-empty");
  if (!Array.isArray(value?.tools) || value.tools.length === 0) errors.push("tools must be non-empty");

  const sourceIDs = new Set();
  for (const [index, source] of (value?.sources ?? []).entries()) {
    const at = `sources[${index}]`;
    if (!source?.source_id || sourceIDs.has(source.source_id)) errors.push(`${at}.source_id must be unique`);
    sourceIDs.add(source?.source_id);
    requireDigest(source?.content_sha256, `${at}.content_sha256`, errors);
    requireDigest(source?.retrieval_record_sha256, `${at}.retrieval_record_sha256`, errors);
  }
  for (const [index, tool] of (value?.tools ?? []).entries()) {
    const at = `tools[${index}]`;
    if (!tool?.kind || !tool?.id || !tool?.revision) errors.push(`${at} must identify kind, id, and revision`);
    requireDigest(tool?.artifact_sha256, `${at}.artifact_sha256`, errors);
  }

  const segmentIDs = new Set();
  const sourceSplits = new Map();
  for (const [index, segment] of (value?.segments ?? []).entries()) {
    const at = `segments[${index}]`;
    if (!segment?.segment_id || segmentIDs.has(segment.segment_id)) errors.push(`${at}.segment_id must be unique`);
    segmentIDs.add(segment?.segment_id);
    if (!sourceIDs.has(segment?.source_id)) errors.push(`${at}.source_id is unknown`);
    if (!Number.isInteger(segment?.start_ms) || !Number.isInteger(segment?.end_ms)
        || segment.start_ms < 0 || segment.end_ms <= segment.start_ms) errors.push(`${at} has an invalid time range`);
    requireDigest(segment?.audio_sha256, `${at}.audio_sha256`, errors);
    requireDigest(segment?.transcript?.sha256, `${at}.transcript.sha256`, errors);
    requireDigest(segment?.diarization?.sha256, `${at}.diarization.sha256`, errors);
    requireDigest(segment?.identity?.sha256, `${at}.identity.sha256`, errors);
    requireDigest(segment?.admission?.decision_sha256, `${at}.admission.decision_sha256`, errors);

    const status = segment?.admission?.status;
    if (status !== "admitted" && status !== "rejected") errors.push(`${at}.admission.status is invalid`);
    if (status === "rejected" && !(segment.admission.reasons?.length > 0)) {
      errors.push(`${at} must preserve rejection reasons`);
    }
    if (status === "admitted") {
      if (segment?.identity?.decision !== "verified_persona"
          || segment.identity.persona_id !== value.persona_id) errors.push(`${at} lacks verified persona identity`);
      if (segment?.transcript?.verified !== true) errors.push(`${at} transcript is not verified`);
      if (!digest.test(segment?.identity?.review_sha256 ?? "")) errors.push(`${at} lacks a content-addressed human speaker review`);
      if (segment?.diarization?.speaker_count !== 1
          || segment.diarization.overlap !== false
          || segment.diarization.music !== false
          || segment.diarization.ambiguous !== false
          || segment.diarization.contaminated_turn !== false) {
        errors.push(`${at} is not speaker-pure`);
      }
      if (!["train", "validation", "test"].includes(segment?.split)) errors.push(`${at}.split is invalid`);
      const previous = sourceSplits.get(segment.source_id);
      if (previous && previous !== segment.split) errors.push(`${at} leaks one source across dataset splits`);
      sourceSplits.set(segment.source_id, segment.split);
    }
  }
  return errors;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const path = process.argv[2];
  if (!path) throw new Error("usage: persona-training-admission.mjs MANIFEST.json");
  const errors = validateTrainingAdmission(JSON.parse(readFileSync(path, "utf8")));
  if (errors.length) {
    for (const error of errors) console.error(error);
    process.exit(1);
  }
  console.log(JSON.stringify({ status: "passed", schema }));
}
