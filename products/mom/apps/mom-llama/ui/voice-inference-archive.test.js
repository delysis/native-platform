"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");
const start = source.indexOf("  const sealedVoiceInferenceRecording =");
const end = source.indexOf("  const confirmRecordedSpeculation =", start);
assert.notEqual(start, -1, "sealed inference recording helper exists");
assert.notEqual(end, -1, "sealed inference helper has a stable boundary");
const context = { Array, Number, Uint8Array };
vm.runInNewContext(
  source.slice(start, end) + "\nglobalThis.sealInference = sealedVoiceInferenceRecording;",
  context,
);

test("native capture preserves exact disjoint accepted source ranges", () => {
  const result = context.sealInference({
    modelInputWavBytes: new Uint8Array(46),
    sourceSampleCount: 12,
    modelInputSampleCount: 5,
    modelInputSourceRanges: [
      { start: 1, end_exclusive: 3 },
      { start: 8, end_exclusive: 11 },
    ],
  });
  assert.equal(result.sourceSampleCount, 12);
  assert.equal(result.acceptedSampleCount, 5);
  assert.deepEqual(JSON.parse(JSON.stringify(result.sourceRanges)), [
    { start: 1, end_exclusive: 3 },
    { start: 8, end_exclusive: 11 },
  ]);
  assert.equal(result.recording.length, 46);
});

test("browser contiguous capture remains supported and cancelled capture has no derivative", () => {
  const browser = context.sealInference({
    modelInputWavBytes: new Uint8Array(46),
    sourceSampleCount: 12,
    modelInputSampleCount: 4,
    modelInputStartSample: 6,
  });
  assert.equal(browser.sourceStartSample, 6);
  assert.equal(browser.sourceRanges, undefined);
  assert.equal(context.sealInference({ heardVoice: true }), null);
});

test("native source ranges without a complete archive cannot claim exact provenance", () => {
  assert.equal(context.sealInference({
    modelInputWavBytes: new Uint8Array(44),
    sourceSampleCount: 12,
    modelInputSampleCount: 4,
    modelInputSourceRanges: [{ start: 0, end_exclusive: 4 }],
  }), null);
  assert.equal(context.sealInference({
    modelInputWavBytes: new Uint8Array(46),
    sourceSampleCount: 12,
    modelInputSampleCount: 4,
    modelInputSourceRanges: [],
  }), null);
  assert.equal(context.sealInference({
    modelInputWavBytes: new Uint8Array(46),
    sourceSampleCount: 12,
    modelInputSampleCount: 4,
    modelInputSourceRanges: [
      { start: 8, end_exclusive: 11 },
      { start: 1, end_exclusive: 2 },
    ],
  }), null);
});
