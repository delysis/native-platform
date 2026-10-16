import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { applySpeakerReview, attributeTranscriptSegments, buildDiarizationLedger, createReviewTemplate, sha256 } from "./speaker-diarization-ledger.mjs";

const digest = (character) => character.repeat(64);
const input = () => ({
  schema: "mom-speaker-diarization-input-v1",
  source: { source_id: "interview-1", media_sha256: digest("a"), source_url: "https://example.test/interview" },
  transcript: {
    artifact_sha256: digest("b"), kind: "asr_segments", tool: { id: "whisper", revision: "rev-1" },
    segments: [
      { segment_id: "t1", start_ms: 0, end_ms: 2_000, text: "Good morning." },
      { segment_id: "t2", start_ms: 2_000, end_ms: 5_000, text: "Thank you for having me." },
      { segment_id: "t3", start_ms: 5_000, end_ms: 7_000, text: "Both people speak." },
      { segment_id: "t4", start_ms: 7_000, end_ms: 8_000, text: "Unclear audio." },
    ],
  },
  diarization: {
    artifact_sha256: digest("c"), config_sha256: digest("d"), kind: "acoustic_diarization",
    tool: { id: "local-diarizer", revision: "rev-2" }, minimum_confidence: 0.8,
    segments: [
      { start_ms: 0, end_ms: 2_000, speaker: "raw-host", confidence: 0.98 },
      { start_ms: 2_000, end_ms: 6_000, speaker: "raw-guest", confidence: 0.96 },
      { start_ms: 5_000, end_ms: 7_000, speaker: "raw-host", confidence: 0.91 },
      { start_ms: 7_000, end_ms: 8_000, speaker: "raw-third", confidence: 0.52, uncertain: true },
    ],
  },
});

test("assigns stable anonymous speakers and retains transcript provenance", () => {
  const first = buildDiarizationLedger(input());
  const renamed = input();
  renamed.diarization.segments.forEach((segment) => { segment.speaker = `renamed-${segment.speaker}`; });
  const second = buildDiarizationLedger(renamed);
  assert.deepEqual(first.segments.map(({ start_ms, speaker_id }) => [start_ms, speaker_id]), second.segments.map(({ start_ms, speaker_id }) => [start_ms, speaker_id]));
  assert.equal(first.segments[0].transcript_refs[0].transcript_segment_id, "t1");
  assert.match(first.segments[0].transcript_refs[0].transcript_sha256, /^[0-9a-f]{64}$/);
  assert.match(first.ledger_sha256, /^[0-9a-f]{64}$/);
});

test("marks overlap and uncertainty rather than hiding ambiguous audio", () => {
  const ledger = buildDiarizationLedger(input());
  assert.equal(ledger.segments[1].overlap, true);
  assert.deepEqual(ledger.segments[1].overlapping_speakers, ["speaker_1"]);
  assert.ok(ledger.segments[1].uncertainty_reasons.includes("overlapping_speakers"));
  assert.ok(ledger.segments[3].uncertainty_reasons.includes("diarizer_marked_uncertain"));
  assert.ok(ledger.segments[3].uncertainty_reasons.includes("low_diarization_confidence"));
});

test("attaches acoustic speaker labels to the original transcript without rewriting text", () => {
  const ledger = buildDiarizationLedger(input());
  const attributed = attributeTranscriptSegments(ledger);
  assert.equal(attributed.segments.length, 4);
  assert.deepEqual(attributed.segments[0], {
    transcript_segment_id: "t1",
    transcript_sha256: sha256("Good morning."),
    start_ms: 0,
    end_ms: 2_000,
    text: "Good morning.",
    speaker_id: "speaker_1",
    speaker_ids: ["speaker_1"],
    speaker_evidence: [{ speaker_id: "speaker_1", overlap_ms: 2_000, confidence: 0.98, uncertain: false, overlap: false }],
    speaker_assignment: "single",
    speaker_uncertain: false,
  });
  assert.equal(attributed.segments[2].speaker_id, null);
  assert.deepEqual(attributed.segments[2].speaker_ids, ["speaker_1", "speaker_2"]);
  assert.equal(attributed.segments[2].speaker_assignment, "multiple");
  assert.equal(attributed.segments[2].speaker_uncertain, true);
  assert.match(attributed.attributed_transcript_sha256, /^[0-9a-f]{64}$/);
});

test("refuses to attribute a ledger changed after it was identified", () => {
  const ledger = buildDiarizationLedger(input());
  ledger.segments[0].speaker_id = "speaker_2";
  assert.throws(() => attributeTranscriptSegments(ledger), /content does not match its identity/);
});

test("human review binds, merges, or rejects every anonymous speaker", () => {
  const ledger = buildDiarizationLedger(input());
  const review = createReviewTemplate(ledger);
  Object.assign(review, { reviewer: "local-human", reviewed_at: "2026-10-15T22:00:00Z" });
  review.decisions = [
    { speaker_id: "speaker_1", action: "reject", evidence: "Interviewer, not the target persona." },
    { speaker_id: "speaker_2", action: "bind_persona", persona_id: "christine_courtois", evidence: "Reviewed against the named guest introduction and clean anchor." },
    { speaker_id: "speaker_3", action: "merge_into", target_speaker_id: "speaker_2", evidence: "Same guest split by the diarizer." },
  ];
  const resolved = applySpeakerReview(ledger, review);
  assert.equal(resolved.resolved_speakers.speaker_1.status, "rejected");
  assert.equal(resolved.resolved_speakers.speaker_2.persona_id, "christine_courtois");
  assert.equal(resolved.resolved_speakers.speaker_3.persona_id, "christine_courtois");
  assert.equal(resolved.segments[0].admission_candidate, false);
  assert.equal(resolved.segments[3].admission_candidate, false, "uncertain merged audio remains excluded");
  assert.match(resolved.review_sha256, /^[0-9a-f]{64}$/);
});

test("rejects stale reviews and merge cycles", () => {
  const ledger = buildDiarizationLedger(input());
  const stale = createReviewTemplate(ledger);
  stale.ledger_sha256 = digest("f");
  assert.throws(() => applySpeakerReview(ledger, stale), /different diarization ledger/);

  const cycle = createReviewTemplate(ledger);
  Object.assign(cycle, { reviewer: "local-human", reviewed_at: "2026-10-15T22:00:00Z" });
  cycle.decisions = [
    { speaker_id: "speaker_1", action: "merge_into", target_speaker_id: "speaker_2", evidence: "Proposed merge." },
    { speaker_id: "speaker_2", action: "merge_into", target_speaker_id: "speaker_1", evidence: "Contradictory merge." },
    { speaker_id: "speaker_3", action: "reject", evidence: "Unknown noise." },
  ];
  assert.throws(() => applySpeakerReview(ledger, cycle), /merge cycle/);
});

test("rejects a ledger changed after its identity was calculated", () => {
  const ledger = buildDiarizationLedger(input());
  const review = createReviewTemplate(ledger);
  ledger.segments[0].end_ms += 1;
  assert.throws(() => applySpeakerReview(ledger, review), /content does not match its identity/);
});

test("refuses caption labels masquerading as acoustic diarization", () => {
  const value = input();
  value.diarization.kind = "caption_speaker_labels";
  assert.throws(() => buildDiarizationLedger(value), /captions are not diarization/);
});

test("CLI verifies artifact hashes and writes a bound review template", () => {
  const directory = mkdtempSync(join(tmpdir(), "mom-diarization-ledger-"));
  const transcriptPath = join(directory, "transcript.jsonl");
  const diarizationPath = join(directory, "diarization.jsonl");
  const specPath = join(directory, "input.json");
  const ledgerPath = join(directory, "ledger.json");
  const reviewPath = join(directory, "review.json");
  const attributedPath = join(directory, "attributed.json");
  const transcriptBytes = `${JSON.stringify({ segment_id: "t1", start_ms: 0, end_ms: 1000, text: "Hello." })}\n`;
  const diarizationBytes = `${JSON.stringify({ start_ms: 0, end_ms: 1000, speaker: "cluster-a", confidence: 0.99 })}\n`;
  writeFileSync(transcriptPath, transcriptBytes);
  writeFileSync(diarizationPath, diarizationBytes);
  writeFileSync(specPath, JSON.stringify({
    schema: "mom-speaker-diarization-input-v1",
    source: { source_id: "source-cli", media_sha256: digest("a") },
    transcript: { path: "transcript.jsonl", artifact_sha256: sha256(transcriptBytes), tool: { id: "asr", revision: "1" } },
    diarization: { path: "diarization.jsonl", artifact_sha256: sha256(diarizationBytes), config_sha256: digest("d"), kind: "acoustic_diarization", tool: { id: "diarizer", revision: "1" } },
  }));
  const completed = spawnSync(process.execPath, [
    new URL("./speaker-diarization-ledger.mjs", import.meta.url).pathname,
    "build", "--input", specPath, "--output", ledgerPath, "--review-template", reviewPath,
    "--attributed-transcript", attributedPath,
  ], { encoding: "utf8" });
  assert.equal(completed.status, 0, completed.stderr);
  const ledger = JSON.parse(readFileSync(ledgerPath, "utf8"));
  const review = JSON.parse(readFileSync(reviewPath, "utf8"));
  const attributed = JSON.parse(readFileSync(attributedPath, "utf8"));
  assert.equal(ledger.speakers[0].speaker_id, "speaker_1");
  assert.equal(review.ledger_sha256, ledger.ledger_sha256);
  assert.equal(attributed.segments[0].speaker_id, "speaker_1");
  assert.equal(attributed.ledger_sha256, ledger.ledger_sha256);
});
