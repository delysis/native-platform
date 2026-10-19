import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { buildDiarizationLedger, sha256 } from "./speaker-diarization-ledger.mjs";
import {
  applyEmotionReview,
  buildEmotionLedger,
  createEmotionReview,
  emotionReviewSchema,
  renderEmotionReviewHtml,
} from "./emotion-diarization-ledger.mjs";

function fixture() {
  const directory = mkdtempSync(join(tmpdir(), "mom-emotion-ledger-test-"));
  const media = join(directory, "source.wav");
  const bytes = Buffer.from("immutable fake audio bytes");
  writeFileSync(media, bytes);
  const transcript = [
    { segment_id: "turn-1", start_ms: 0, end_ms: 1000, text: "It is good to see you." },
    { segment_id: "turn-2", start_ms: 1100, end_ms: 2200, text: "Tell me what happened." },
  ];
  const diarization = [
    { start_ms: 0, end_ms: 1000, speaker: "cluster-a", confidence: 0.95 },
    { start_ms: 1100, end_ms: 2200, speaker: "cluster-a", confidence: 0.94 },
  ];
  const speakerLedger = buildDiarizationLedger({
    schema: "mom-speaker-diarization-input-v1",
    source: { source_id: "interview-1", source_url: "https://example.invalid/interview", media_filename: "source.wav", media_sha256: sha256(bytes) },
    transcript: { artifact_sha256: sha256(transcript), kind: "asr", tool: { id: "asr", revision: "one" }, segments: transcript },
    diarization: { artifact_sha256: sha256(diarization), config_sha256: sha256("config"), kind: "acoustic_diarization", tool: { id: "speaker-model", revision: "two" }, segments: diarization },
  });
  const predictions = [
    { segment_id: speakerLedger.segments[0].segment_id, labels: ["开心/happy", "中立/neutral", "<unk>"], scores: [0.72, 0.2, 0.08], raw_result: { labels: ["开心/happy", "中立/neutral", "<unk>"], scores: [0.72, 0.2, 0.08] } },
  ];
  const model = { model_id: "emotion2vec/emotion2vec_plus_large", model_revision: "6c303ba987b86b93193de93e34bb2b077a6bedc4", model_manifest_sha256: sha256("model"), runtime: "test", runtime_version: "1", inference_contract: "test contract" };
  return { media, speakerLedger, predictions, model };
}

test("emotion observations bind to exact speaker segments and preserve raw uncalibrated scores", () => {
  const { media, speakerLedger, predictions, model } = fixture();
  const ledger = buildEmotionLedger({ speakerLedger, mediaPath: media, predictions, model });
  assert.equal(ledger.observations[0].segment_id, speakerLedger.segments[0].segment_id);
  assert.equal(ledger.observations[0].speaker_diarization.speaker_id, "speaker_1");
  assert.equal(ledger.observations[0].observation.status, "machine_proposed");
  assert.equal(ledger.observations[0].observation.proposed_label.label, "happy");
  assert.equal(ledger.observations[0].observation.raw_scores[0].raw_label, "开心/happy");
  assert.equal(ledger.observations[0].confidence.calibration, "uncalibrated");
  assert.equal(ledger.observations[0].human_review, null);
});

test("missing model output is explicit unknown evidence", () => {
  const { media, speakerLedger, predictions, model } = fixture();
  const ledger = buildEmotionLedger({ speakerLedger, mediaPath: media, predictions, model });
  const missing = ledger.observations[1];
  assert.equal(missing.observation.status, "unknown");
  assert.ok(missing.uncertainty.reasons.includes("emotion:model_output_missing"));
  assert.equal(missing.observation.proposed_label, null);
});

test("human decisions remain separate and cannot silently omit a segment", () => {
  const { media, speakerLedger, predictions, model } = fixture();
  const ledger = buildEmotionLedger({ speakerLedger, mediaPath: media, predictions, model });
  const review = createEmotionReview(ledger);
  assert.equal(review.schema, emotionReviewSchema);
  review.reviewer = "George";
  review.reviewed_at = "2026-10-16T12:00:00Z";
  review.decisions[0] = { ...review.decisions[0], action: "accept", emotion_label: "happy", speaking_style_labels: ["warm"], usable_for_style_selection: true };
  review.decisions[1] = { ...review.decisions[1], action: "unknown" };
  const resolved = applyEmotionReview(ledger, review);
  assert.equal(resolved.segments[0].machine_observation.observation.proposed_label.label, "happy");
  assert.equal(resolved.segments[0].human_review.emotion_label, "happy");
  assert.equal(resolved.segments[1].human_review.action, "unknown");
  assert.equal(ledger.observations[0].human_review, null);
  assert.throws(() => applyEmotionReview(ledger, { ...review, decisions: review.decisions.slice(0, 1) }), /must decide every/);
  const mislabeledAccept = structuredClone(review);
  mislabeledAccept.decisions[0].emotion_label = "sad";
  assert.throws(() => applyEmotionReview(ledger, mislabeledAccept), /accept must preserve/);
  const unknownStyleReference = structuredClone(review);
  unknownStyleReference.decisions[1].usable_for_style_selection = true;
  assert.throws(() => applyEmotionReview(ledger, unknownStyleReference), /cannot be a style reference/);
});

test("tampered speaker evidence and stale emotion review are rejected", () => {
  const { media, speakerLedger, predictions, model } = fixture();
  const tampered = structuredClone(speakerLedger);
  tampered.segments[0].speaker_id = "speaker_2";
  assert.throws(() => buildEmotionLedger({ speakerLedger: tampered, mediaPath: media, predictions, model }), /does not match its identity/);
  const ledger = buildEmotionLedger({ speakerLedger, mediaPath: media, predictions, model });
  const review = createEmotionReview(ledger);
  review.emotion_ledger_sha256 = sha256("stale");
  assert.throws(() => applyEmotionReview(ledger, review), /different emotion ledger/);
});

test("review page exposes source-bound playback and exports explicit review state", () => {
  const { media, speakerLedger, predictions, model } = fixture();
  const ledger = buildEmotionLedger({ speakerLedger, mediaPath: media, predictions, model });
  const html = renderEmotionReviewHtml(ledger);
  assert.match(html, /Emotion review/);
  assert.match(html, /speaker_1/);
  assert.match(html, /emotion-review\.json/);
  assert.match(html, /Style reference/);
});
