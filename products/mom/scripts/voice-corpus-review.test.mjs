import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { buildQueue, resolveReviews } from "./voice-corpus-review.mjs";

function sha(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function wav() {
  const samples = 24_000;
  const dataSize = samples * 2;
  const bytes = Buffer.alloc(44 + dataSize);
  bytes.write("RIFF", 0); bytes.writeUInt32LE(36 + dataSize, 4); bytes.write("WAVE", 8);
  bytes.write("fmt ", 12); bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20);
  bytes.writeUInt16LE(1, 22); bytes.writeUInt32LE(24_000, 24); bytes.writeUInt32LE(48_000, 28);
  bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34); bytes.write("data", 36); bytes.writeUInt32LE(dataSize, 40);
  for (let index = 0; index < samples; index += 1) bytes.writeInt16LE(Math.round(Math.sin(index / 20) * 1000), 44 + index * 2);
  return bytes;
}

function fixture({ machineStyle = false } = {}) {
  const directory = mkdtempSync(join(tmpdir(), "mom-voice-review-"));
  const clip = join(directory, "segment.wav");
  const audio = wav(); writeFileSync(clip, audio);
  const transcript = "A source-bound transcript.";
  const manifest = {
    schema_version: 1,
    diarization_provenance: { kind: "model", model_id: "example/diarizer", model_revision_or_version: "abc123", runtime: "test" },
    segments: [{
      persona_id_hypothesis: "example_person", speaker_name_hypothesis: "Example Person",
      clip_path: "segment.wav", clip_sha256: sha(audio), sample_rate_hz: 24000, channels: 1, duration_seconds: 1,
      source_url: "https://example.test/video", source_title: "Interview", source_channel: "Example",
      source_timestamp_start_seconds: 10, source_timestamp_end_seconds: 11,
      speaker_cluster_id: "speaker-0", speaker_cluster_confidence: null,
      speaker_identity_hypothesis: { status: "source_attributed", confidence: null, basis: "The source names the guest; a human has not reviewed the turn." },
      transcript: { text: transcript, sha256: sha(Buffer.from(transcript)), provenance: { model_id: "example/asr", model_revision_or_version: "def456", runtime: "test" } },
      style_observation: machineStyle ? { status: "machine_proposed", emotion_labels: ["calm"], speaking_style_labels: [], confidence: 0.7 } : { status: "unknown" },
    }],
  };
  const manifestPath = join(directory, "segments.json"); writeFileSync(manifestPath, JSON.stringify(manifest));
  return { directory, manifestPath, clip };
}

test("build preserves evidence and keeps absent style labels explicitly unknown", () => {
  const { directory, manifestPath, clip } = fixture();
  const before = sha(readFileSync(clip));
  const output = join(directory, "review");
  const result = buildQueue({ manifestPath, outputDirectory: output });
  assert.equal(result.itemCount, 1);
  assert.equal(sha(readFileSync(clip)), before);
  const queue = JSON.parse(readFileSync(result.queuePath));
  assert.equal(queue.items[0].style_observation.status, "unknown");
  assert.equal(queue.items[0].diarization.speaker_cluster_confidence, null);
  assert.equal(queue.qwen3_tts_control_contract.explicit_style_instruct_supported_with_clone_route, false);
  assert.equal(queue.qwen3_tts_control_contract.lower_level_speaker_embedding_plus_instruct_api_present, true);
  assert.equal(queue.qwen3_tts_control_contract.lower_level_speaker_embedding_plus_instruct_exposed_by_resident_wire_api, false);
  assert.equal(queue.qwen3_tts_control_contract.instruct, null);
});

test("manual turn selection is recorded as a method rather than a fabricated model", () => {
  const { directory, manifestPath } = fixture();
  const manifest = JSON.parse(readFileSync(manifestPath));
  manifest.diarization_provenance = {
    kind: "manual",
    method_id: "caption-bounded-continuous-turn",
    method_version: "1",
    runtime: "retained source captions plus source metadata",
  };
  writeFileSync(manifestPath, JSON.stringify(manifest));
  const built = buildQueue({ manifestPath, outputDirectory: join(directory, "review") });
  const queue = JSON.parse(readFileSync(built.queuePath));
  assert.equal(queue.items[0].diarization.provenance.kind, "manual");
  assert.equal(queue.items[0].diarization.speaker_cluster_confidence, null);
});

test("machine-proposed labels require exact model provenance", () => {
  const { directory, manifestPath } = fixture({ machineStyle: true });
  assert.throws(() => buildQueue({ manifestPath, outputDirectory: join(directory, "review") }), /style_observation\.provenance is required/);
});

test("resolve emits reviewed Base ICL reference markup without inventing instruct", () => {
  const { directory, manifestPath } = fixture();
  const built = buildQueue({ manifestPath, outputDirectory: join(directory, "review") });
  const queue = JSON.parse(readFileSync(built.queuePath));
  const review = [{
    review_id: "review-1", evidence_id: queue.items[0].evidence_id, decision: "accept", reviewer: "George",
    reviewed_at: "2026-10-16T10:00:00Z", notes: "Listened to the complete turn.",
    speaker_identity: { persona_id: "example_person", speaker_name: "Example Person", confidence: 1 },
    emotion_style: { status: "unknown" },
  }];
  const reviewsPath = join(directory, "reviews.json"); writeFileSync(reviewsPath, JSON.stringify(review));
  const outputPath = join(directory, "accepted.json");
  const result = resolveReviews({ queuePath: built.queuePath, reviewsPath, outputPath });
  assert.equal(result.acceptedCount, 1);
  const accepted = JSON.parse(readFileSync(outputPath));
  assert.equal(accepted.accepted_references[0].qwen3_tts_base_icl.instruct, null);
  assert.equal(accepted.accepted_references[0].qwen3_tts_base_icl.style_control, "implicit_from_reviewed_reference_performance");
  assert.equal(accepted.accepted_references[0].emotion_style.status, "unknown");
});
