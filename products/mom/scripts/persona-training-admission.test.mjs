import assert from "node:assert/strict";
import test from "node:test";
import { validateTrainingAdmission } from "./persona-training-admission.mjs";

const sha = (character) => character.repeat(64);
const valid = () => ({
  schema: "mom-persona-training-admission-v1",
  persona_id: "gabor_mate",
  tools: [{ kind: "diarization", id: "speaker-pipeline", revision: "exact-revision", artifact_sha256: sha("b") }],
  sources: [{ source_id: "podcast-1", content_sha256: sha("c"), retrieval_record_sha256: sha("d") }],
  segments: [{
    segment_id: "podcast-1-0001", source_id: "podcast-1", start_ms: 1_000, end_ms: 31_000,
    audio_sha256: sha("e"), split: "train",
    transcript: { text: "Verified words.", sha256: sha("f"), verified: true },
    diarization: { sha256: sha("1"), speaker_count: 1, overlap: false, music: false, ambiguous: false, contaminated_turn: false },
    identity: { sha256: sha("2"), review_sha256: sha("4"), decision: "verified_persona", persona_id: "gabor_mate" },
    admission: { decision_sha256: sha("3"), status: "admitted", reasons: [] },
  }],
});

test("admits only a fully content-addressed speaker-pure persona segment", () => {
  assert.deepEqual(validateTrainingAdmission(valid()), []);
});

test("rejects an admitted interviewer-contaminated segment", () => {
  const value = valid();
  value.segments[0].diarization.contaminated_turn = true;
  assert.match(validateTrainingAdmission(value).join("\n"), /not speaker-pure/);
});

test("requires rejection provenance", () => {
  const value = valid();
  value.segments[0].admission = { decision_sha256: sha("3"), status: "rejected", reasons: [] };
  assert.match(validateTrainingAdmission(value).join("\n"), /preserve rejection reasons/);
});

test("forbids source leakage across train and evaluation splits", () => {
  const value = valid();
  value.segments.push({ ...structuredClone(value.segments[0]), segment_id: "podcast-1-0002", split: "test" });
  assert.match(validateTrainingAdmission(value).join("\n"), /leaks one source/);
});

test("does not require rights paperwork for a local personal project", () => {
  const value = valid();
  assert.equal(value.rights_record, undefined);
  assert.deepEqual(validateTrainingAdmission(value), []);
});

test("requires a human-reviewed speaker binding", () => {
  const value = valid();
  delete value.segments[0].identity.review_sha256;
  assert.match(validateTrainingAdmission(value).join("\n"), /human speaker review/);
});
