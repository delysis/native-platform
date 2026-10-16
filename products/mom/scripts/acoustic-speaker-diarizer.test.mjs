import test from "node:test";
import assert from "node:assert/strict";
import { acousticWindows, clusterSpeakerEmbeddings } from "./acoustic-speaker-diarizer.mjs";

const record = (index, start_ms, embedding) => ({ index, start_ms, end_ms: start_ms + 900, embedding });

test("clusters acoustic embeddings and names speakers by first appearance", () => {
  const result = clusterSpeakerEmbeddings([
    record(0, 0, [1, 0, 0]),
    record(1, 1_000, [0, 1, 0]),
    record(2, 2_000, [0.99, 0.02, 0]),
    record(3, 3_000, [0.01, 0.99, 0]),
  ], { threshold: 0.8 });
  assert.deepEqual(result.map(({ speaker }) => speaker), [
    "acoustic_cluster_1", "acoustic_cluster_2", "acoustic_cluster_1", "acoustic_cluster_2",
  ]);
  assert.ok(result.every(({ confidence }) => confidence > 0.99));
  assert.ok(result.every(({ uncertain }) => !uncertain));
});

test("keeps a singleton uncertain instead of inventing speaker certainty", () => {
  const result = clusterSpeakerEmbeddings([
    record(0, 0, [1, 0]),
    record(1, 1_000, [0, 1]),
  ], { threshold: 0.9 });
  assert.equal(result.length, 2);
  assert.ok(result.every(({ uncertain }) => uncertain));
});

test("honors a human supplied maximum speaker count deterministically", () => {
  const result = clusterSpeakerEmbeddings([
    record(0, 0, [1, 0, 0]),
    record(1, 1_000, [0.9, 0.1, 0]),
    record(2, 2_000, [0, 1, 0]),
  ], { threshold: 0.999, maxSpeakers: 2 });
  assert.equal(new Set(result.map(({ speaker }) => speaker)).size, 2);
  assert.equal(result[0].speaker, result[1].speaker);
});

test("rejects malformed embeddings", () => {
  assert.throws(() => clusterSpeakerEmbeddings([record(0, 0, [0, 0])]), /zero embedding/);
  assert.throws(() => clusterSpeakerEmbeddings([record(0, 0, [1, Number.NaN])]), /invalid embedding/);
});

test("turns rolling caption cues into bounded acoustic windows", () => {
  const windows = acousticWindows([
    { start_seconds: 0, end_seconds: 0.01 },
    { start_seconds: 0, end_seconds: 2.4 },
    { start_seconds: 2.39, end_seconds: 5.8 },
    { start_seconds: 5.8, end_seconds: 6.2 },
  ]);
  assert.deepEqual(windows, [
    { index: 0, start_ms: 0, end_ms: 3_000 },
    { index: 1, start_ms: 3_000, end_ms: 6_200 },
  ]);
});

test("keeps real silence as an acoustic boundary", () => {
  const windows = acousticWindows([
    { start_ms: 100, end_ms: 2_100 },
    { start_ms: 2_800, end_ms: 4_800 },
  ]);
  assert.deepEqual(windows, [
    { index: 0, start_ms: 100, end_ms: 2_100 },
    { index: 1, start_ms: 2_800, end_ms: 4_800 },
  ]);
});
