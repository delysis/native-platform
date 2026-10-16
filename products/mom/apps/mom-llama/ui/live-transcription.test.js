"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { callPrefillRetryDecision, createLiveTranscriptionTap } = require("./voice-capture-policy.js");
const tick = () => new Promise((resolve) => setImmediate(resolve));
const deferred = () => {
  let resolve; let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
const make = (overrides = {}) => createLiveTranscriptionTap({
  turn: "turn-a", capture: "capture-a", send: async () => {},
  finish: async () => ({ text: "Complete final.", final: true }), cancel: async () => {}, ...overrides,
});

test("a late transcript retries only the same idle call capture", () => {
  const state = { expectedConversation: "chat-a", settledConversation: "chat-a",
    callActive: true, expectedEpoch: "call-a", currentEpoch: "call-a",
    captureStarted: true, recording: { call: true, heardVoice: false, sending: false } };
  assert.equal(callPrefillRetryDecision(state), "retry");
  assert.equal(callPrefillRetryDecision({ ...state, recording: { call: true, heardVoice: true } }),
    "capture_advanced", "a speaking caller continues on the already chosen fallback");
  assert.equal(callPrefillRetryDecision({ ...state, settledConversation: "chat-b" }), "stale");
  assert.equal(callPrefillRetryDecision({ ...state, currentEpoch: "call-b" }), "stale");
  assert.equal(callPrefillRetryDecision({ ...state, callActive: false }), "stale");
});

test("browser tap coalesces callbacks and delivers source offsets exactly once", async () => {
  const packets = []; let sealed;
  const tap = make({ send: async (packet) => packets.push(packet), finish: async (end) => { sealed = end; } });
  const expected = new Float32Array(1603).map((_, index) => (index % 17) / 17);
  for (let start = 0; start < expected.length; start += 83) {
    assert.equal(tap.append(expected.subarray(start, start + 83)), true);
  }
  await tap.seal();
  assert.deepEqual(packets.map((packet) => packet.sample_offset), [0, 640, 1280]);
  assert.deepEqual(packets.map((packet) => packet.samples.length), [640, 640, 323]);
  assert.deepEqual(packets.flatMap((packet) => Array.from(packet.samples)), Array.from(expected));
  assert.deepEqual(sealed, { turn: "turn-a", capture: "capture-a", samples: 1603 });
  assert.equal(tap.metrics().backlog_samples, 0);
});

test("a spoken turn longer than 29 seconds keeps its complete transcription source", async () => {
  let delivered = 0;
  const tap = make({ send: async ({ samples }) => { delivered += samples.length; } });
  const frame = new Float32Array(640).fill(0.25);
  for (let index = 0; index < 750; index += 1) {
    assert.equal(tap.append(frame), true);
    await tick();
  }
  await tap.seal();
  assert.equal(tap.metrics().phase, "complete");
  assert.equal(delivered, 480_000);
  assert.equal(tap.metrics().bridge_accepted_samples, 480_000);
});

test("delayed transcription leaves capture admission synchronous and final pending", async () => {
  const first = deferred(); const final = deferred();
  let pushes = 0; let finalCalled = false; let settled = false;
  const tap = make({
    send: async () => { pushes += 1; if (pushes === 1) await first.promise; return { text: "partial" }; },
    finish: async () => { finalCalled = true; return final.promise; },
  });
  void tap.finished.then(() => { settled = true; });
  assert.equal(tap.append(new Float32Array(640)), true);
  await tick();
  assert.equal(pushes, 1);
  assert.equal(tap.append(new Float32Array(640)), true, "capture does not await the first ingress");
  tap.seal();
  await tick();
  assert.equal(settled, false);
  assert.equal(finalCalled, false, "EOS follows all acknowledged packets");
  first.resolve(); await tick();
  assert.equal(finalCalled, true);
  assert.equal(settled, false, "a push/partial result cannot resolve final transcription");
  final.resolve({ text: "Only the complete final.", final: true });
  assert.equal((await tap.finished).text, "Only the complete final.");
});

test("resumed speech uses the same tap while Gemma's cursor rewinds independently", async () => {
  const packets = [];
  const tap = make({ send: async (packet) => packets.push(packet) });
  const prefix = new Float32Array(640).fill(0.25);
  const continuation = new Float32Array(671).fill(0.5);
  tap.append(prefix); await tick();
  // The tap deliberately exposes no Gemma session, replay, or reset method.
  // Simulate the independent model owner's rewind, then append only new PCM.
  let gemmaCursor = prefix.length;
  gemmaCursor = 0;
  assert.equal(tap.metrics().accepted_samples, prefix.length);
  tap.append(continuation);
  await tap.seal();
  assert.equal(gemmaCursor, 0);
  assert.deepEqual(packets.map((packet) => packet.sample_offset), [0, 640, 1280]);
  assert.equal(packets.reduce((n, packet) => n + packet.samples.length, 0), 1311);
});

test("overflow includes in-flight PCM and rejects every late final", async () => {
  const delayed = deferred(); const cancellations = []; const events = [];
  let finishCalls = 0;
  const tap = make({ maxQueuedSamples: 640, send: () => delayed.promise,
    finish: async () => { finishCalls += 1; }, cancel: async (value) => cancellations.push(value),
    onTrace: (event) => events.push(event) });
  tap.append(new Float32Array(640)); await tick();
  assert.equal(tap.append(new Float32Array(1)), false);
  await assert.rejects(tap.finished, /overflow/);
  delayed.resolve({ text: "incomplete" }); await tick();
  assert.equal(finishCalls, 0);
  assert.equal(cancellations.length, 1);
  assert.equal(events.filter((event) => event.stage === "overflow").length, 1);
  assert.equal(tap.metrics().bridge_accepted_samples, 0);
});

test("a changed selected turn cannot retarget an existing browser source", async () => {
  let selected = "turn-a"; const packets = []; let seal;
  const tap = make({ turn: selected, send: async (packet) => packets.push(packet),
    finish: async (value) => { seal = value; } });
  tap.append(new Float32Array(640)); await tick();
  selected = "turn-b";
  tap.append(new Float32Array(3)); await tap.seal();
  assert.equal(selected, "turn-b");
  assert.ok(packets.every((packet) => packet.turn === "turn-a" && packet.capture === "capture-a"));
  assert.equal(seal.turn, "turn-a");
});

test("cancel during final inference never resolves the delayed result", async () => {
  const final = deferred(); let cancelled = 0;
  const tap = make({ finish: () => final.promise, cancel: async () => { cancelled += 1; } });
  tap.append(new Float32Array(1)); tap.seal(); await tick();
  tap.cancel(); await assert.rejects(tap.finished, /cancelled/);
  final.resolve({ final: true, text: "Too late" }); await tick();
  assert.equal(tap.metrics().phase, "failed");
  assert.equal(cancelled, 1);
});

test("backlog deadline is a failure, not EOF or partial success", async () => {
  const events = []; let cancellations = 0;
  const tap = make({ deadlineMs: 5, send: () => new Promise(() => {}),
    cancel: async () => { cancellations += 1; }, onTrace: (event) => events.push(event) });
  tap.append(new Float32Array(640));
  await assert.rejects(tap.finished, /backlog_timeout/);
  await tick();
  assert.equal(cancellations, 1);
  assert.ok(events.some((event) => event.stage === "failed_audio_retained"));
});

test("invalid PCM and empty turns fail without publishing text", async () => {
  const invalid = make();
  assert.equal(invalid.append(Float32Array.of(NaN)), false);
  await assert.rejects(invalid.finished, /invalid_pcm/);
  const empty = make();
  await assert.rejects(empty.seal(), /empty/);
});

test("source-bound hook is outside Gemma prefill and seals only at physical browser stop", () => {
  const bridge = fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");
  const prefill = bridge.slice(bridge.indexOf("  const enqueueVoicePrefill ="), bridge.indexOf("  const prepareVoicePrefill ="));
  assert.ok(!prefill.includes("transcription"), "Gemma replay cannot deliver PCM to the tap");
  const capture = bridge.slice(bridge.indexOf("  const beginBrowserVoiceCapture ="), bridge.indexOf("  const beginNativeVoiceCapture ="));
  assert.ok(capture.indexOf("recording.transcription?.append(pcm)") > capture.indexOf("recording.capturedPcm.push(pcm)"));
  assert.equal((bridge.match(/recording\.transcription\?\.append\(pcm\)/g) || []).length, 1);
  assert.equal((bridge.match(/recording\.transcription\?\.seal\(\)/g) || []).length, 1);
  assert.ok(!bridge.includes("await recording.transcription"));
  const tapSource = fs.readFileSync(path.join(__dirname, "voice-capture-policy.js"), "utf8");
  assert.ok(!tapSource.includes("message_record_voice_transcript"));
  assert.ok(bridge.includes("const indexVoiceTurn ="), "this stage does not silently remove existing indexing");
});
