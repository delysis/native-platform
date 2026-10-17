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

const queueStart = source.indexOf("  const enqueueVoicePrefill =");
const queueEnd = source.indexOf("\n  const prepareVoicePrefill =", queueStart);
const deferredStart = source.indexOf("    recording.resumeDeferredPrefill = (live) => {");
const deferredEnd = source.indexOf("\n    processor.onaudioprocess =", deferredStart);
const resumeStart = source.indexOf("  const resumeBrowserVoicePrefill =");
const resumeEnd = source.indexOf("\n  const abortCallSpeculation =", resumeStart);
assert.notEqual(queueStart, -1, "voice prefill queue has a stable source boundary");
assert.notEqual(queueEnd, -1, "voice prefill queue has a stable end boundary");
assert.notEqual(deferredStart, -1, "deferred resume has a stable source boundary");
assert.notEqual(deferredEnd, -1, "deferred resume has a stable end boundary");
assert.notEqual(resumeStart, -1, "browser checkpoint resume has a stable source boundary");
assert.notEqual(resumeEnd, -1, "browser checkpoint resume has a stable end boundary");

function makePrefillHarness({ rejectPush = false, holdFirstPush = false } = {}) {
  const calls = [];
  const events = [];
  let firstPushHeld = false;
  let releaseHeldPush = null;
  const policy = require("./voice-capture-policy.js");
  const prefillContext = {
    Array, DataView, Float32Array, Int16Array, Number, Uint8Array,
    performance: require("node:perf_hooks").performance,
    invoke: async (name, args) => {
      calls.push({ name, args: JSON.parse(JSON.stringify(args)) });
      if (name === "mom_llama_voice_audio_push" && rejectPush) throw new Error("push rejected");
      if (name === "mom_llama_voice_audio_push" && holdFirstPush && !firstPushHeld) {
        firstPushHeld = true;
        return new Promise((resolve) => {
          releaseHeldPush = () => resolve({ buffered_samples: args.samples?.length || 0 });
        });
      }
      return { buffered_samples: args.samples?.length || 0 };
    },
    markVoiceLatencyIn: (_trace, stage, detail = {}) => events.push({ stage, detail }),
    errorMessage: (error) => String(error?.message || error),
    mergeFloat32: (parts) => {
      const total = parts.reduce((sum, part) => sum + part.length, 0);
      const merged = new Float32Array(total);
      let offset = 0;
      for (const part of parts) { merged.set(part, offset); offset += part.length; }
      return merged;
    },
    voiceCallConversation: "call-1",
  };
  prefillContext.globalThis = prefillContext;
  prefillContext.MomVoiceCapturePolicy = policy;
  for (const name of ["appendInferenceSourceRange", "sliceInferenceSourceRanges",
    "inferenceSourceOffset", "truncateInferencePcm16", "quantizeInferencePcm16"]) {
    prefillContext[name] = policy[name];
  }
  vm.runInNewContext([
    "let voiceRecording = null; let voiceCallActive = true;",
    source.slice(queueStart, queueEnd),
    `const installDeferredResume = (recording) => {\n${source.slice(deferredStart, deferredEnd)}\n};`,
    source.slice(resumeStart, resumeEnd),
    `globalThis.prefillHarness = {\n      enqueueVoicePrefill,\n      installDeferredResume,\n      resumeBrowserVoicePrefill,\n      setCurrent(recording) { voiceRecording = recording; },\n      calls: ${"null"},\n    };`,
  ].join("\n"), prefillContext);
  prefillContext.prefillHarness.calls = calls;
  prefillContext.prefillHarness.events = events;
  return {
    harness: prefillContext.prefillHarness,
    calls,
    events,
    policy,
    releaseFirstPush() {
      assert.equal(firstPushHeld, true, "the first mocked native push is in flight");
      assert.equal(typeof releaseHeldPush, "function");
      releaseHeldPush();
      releaseHeldPush = null;
    },
  };
}

function makeRecording(capturedPcm, { deferred = true } = {}) {
  return {
    capturedPcm: [capturedPcm],
    capturedSamples: capturedPcm.length,
    trace: [],
    prefillFailed: false,
    modelInputReliable: true,
    prefillEpoch: 0,
    prefillDeferred: deferred,
    prefillPending: deferred,
    prefillRetryPending: false,
    prefillSession: Promise.resolve({ session: "initial-session" }),
    prefillInputStartSample: null,
    prefillSourceRanges: [],
    prefillLastSourceEnd: 0,
    prefillParts: [],
    prefillSamples: 0,
    prefillQueuedSamples: 0,
    prefillQueueHighWaterSamples: 0,
    pushChain: Promise.resolve(),
    modelInputPcm16Parts: [],
    modelInputSampleCount: 0,
    modelInputSourceRanges: [],
    firstPrefillChunkAccepted: false,
    prefillPushes: 0,
    prefillAcceptedSamples: 0,
    prefillHighWaterSamples: 0,
    prefillIpcMs: 0,
    prefillMaxIpcMs: 0,
  };
}

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

test("deferred handoff excludes physical-only audio and serializes PCM arriving during a pending IPC", async () => {
  const { harness, calls, policy, releaseFirstPush } = makePrefillHarness({ holdFirstPush: true });
  const captured = new Float32Array(1200).fill(0.91); // playback and idle room prefix
  for (let index = 120; index < 300; index += 1) captured[index] = (index - 200) / 1000;
  captured.fill(0.72, 300, 700); // discarded silence between caller ranges
  for (let index = 700; index < 1200; index += 1) captured[index] = (index - 950) / 1200;
  const recording = makeRecording(captured);

  // These are the only slices admitted by the voice-onset path: retained
  // onset pre-roll followed by caller speech. The entire physical archive
  // deliberately also contains playback/idle PCM and a silence gap.
  harness.enqueueVoicePrefill(recording, captured.subarray(120, 300), false, 120);
  harness.enqueueVoicePrefill(recording, captured.subarray(700, 1200), false, 700);
  assert.equal(calls.length, 0, "the native queue remains deferred until handoff");
  assert.equal(recording.prefillSamples, 680);

  harness.installDeferredResume(recording);
  harness.setCurrent(recording);
  const handoff = recording.resumeDeferredPrefill({ session: "selected-turn" });
  await new Promise((resolve) => setImmediate(resolve));
  const pushesDuringFirstIpc = calls.filter((call) => call.name === "mom_llama_voice_audio_push");
  assert.equal(pushesDuringFirstIpc.length, 1, "only the first bounded block enters the pending IPC");

  // New caller PCM can arrive while that IPC acknowledgement is pending. It
  // is captured and queued behind the first chain, never raced around it.
  const laterSpeech = new Float32Array(640);
  for (let index = 0; index < laterSpeech.length; index += 1) laterSpeech[index] = (index - 320) / 1800;
  recording.capturedPcm.push(laterSpeech);
  recording.capturedSamples += laterSpeech.length;
  harness.enqueueVoicePrefill(recording, laterSpeech, false, recording.capturedSamples - laterSpeech.length);
  assert.equal(calls.filter((call) => call.name === "mom_llama_voice_audio_push").length, 1,
    "later capture stays behind the pending native acknowledgement");
  releaseFirstPush();
  assert.equal(await handoff, true);
  await recording.pushChain;

  const pushes = calls.filter((call) => call.name === "mom_llama_voice_audio_push");
  assert.equal(pushes.length, 3, "both original blocks and later speech are acknowledged in order");
  const pushed = pushes.flatMap((call) => call.args.samples);
  const expected = new Float32Array(1320);
  expected.set(captured.subarray(120, 300), 0);
  expected.set(captured.subarray(700, 1200), 180);
  expected.set(laterSpeech, 680);
  const quantized = policy.quantizeInferencePcm16(expected);
  assert.deepEqual(pushed, Array.from(quantized.decoded));
  assert.deepEqual(JSON.parse(JSON.stringify(recording.modelInputSourceRanges)), [
    { start: 120, end_exclusive: 300 },
    { start: 700, end_exclusive: 1840 },
  ]);
  assert.equal(recording.modelInputSampleCount, 1320);
  assert.equal(recording.prefillQueuedSamples, 0);
  assert.ok(!pushed.includes(0.91), "unselected playback/idle prefix never enters Gemma");
  assert.ok(!pushed.includes(0.72), "discarded silence never enters Gemma");
});

test("browser handoff truncates to acknowledged q16 prefix and resumes through a source gap once", async () => {
  const { harness, calls, policy } = makePrefillHarness();
  const captured = Float32Array.from({ length: 40 }, (_, index) => (index - 20) / 50);
  const firstRange = policy.quantizeInferencePcm16(captured.subarray(10, 14)).pcm16;
  const secondRange = policy.quantizeInferencePcm16(captured.subarray(20, 24)).pcm16;
  const recording = makeRecording(captured, { deferred: false });
  recording.native = false;
  recording.call = true;
  recording.modelInputPcm16Parts = [Int16Array.from([...firstRange, ...secondRange])];
  recording.modelInputSampleCount = 8;
  recording.modelInputSourceRanges = [
    { start: 10, end_exclusive: 14 },
    { start: 20, end_exclusive: 24 },
  ];
  harness.setCurrent(recording);

  // Six acknowledged model samples are four from the first source span and
  // two from the second. The physical cursor must therefore be 22, not the
  // six-sample arithmetic offset into the uncompressed recording.
  harness.resumeBrowserVoicePrefill(recording, {
    session: "replacement-turn",
    request_id: "request-2",
    resume_reused: true,
    resume_sample_offset: 6,
    conversation_id: "call-1",
  });
  await recording.pushChain;

  const pushes = calls.filter((call) => call.name === "mom_llama_voice_audio_push");
  assert.equal(pushes.length, 1, "only the unacknowledged source suffix is replayed once");
  const tail = policy.quantizeInferencePcm16(captured.subarray(22));
  assert.deepEqual(pushes[0].args.samples, Array.from(tail.decoded));
  const expectedPcm16 = [...firstRange, ...secondRange.subarray(0, 2), ...tail.pcm16];
  const actualPcm16 = Array.from(recording.modelInputPcm16Parts.flatMap((part) => Array.from(part)));
  assert.deepEqual(actualPcm16, expectedPcm16);
  assert.equal(recording.modelInputSampleCount, expectedPcm16.length);
  assert.deepEqual(JSON.parse(JSON.stringify(recording.modelInputSourceRanges)), [
    { start: 10, end_exclusive: 14 },
    { start: 20, end_exclusive: 40 },
  ]);
  assert.deepEqual(JSON.parse(JSON.stringify(
    harness.events.find((event) => event.stage === "speculation_handoff_replay_enqueued").detail,
  )), {
    checkpoint_samples: 6,
    replay_samples: 18,
    captured_samples: 40,
  });
});

test("rejected native push invalidates the exact derivative", async () => {
  const { harness, calls, policy } = makePrefillHarness({ rejectPush: true });
  const captured = new Float32Array(640).fill(0.125);
  const recording = makeRecording(captured, { deferred: false });
  recording.prefillDeferred = false;
  recording.prefillPending = false;
  recording.prefillSession = Promise.resolve({ session: "rejected-turn" });
  harness.enqueueVoicePrefill(recording, captured, false, 0);
  await recording.pushChain;

  assert.ok(calls.some((call) => call.name === "mom_llama_voice_audio_push"));
  assert.ok(calls.some((call) => call.name === "mom_llama_voice_audio_abort"));
  assert.equal(recording.prefillFailed, true);
  assert.equal(recording.modelInputReliable, false);
  assert.equal(recording.modelInputSampleCount, 0);
  assert.deepEqual(recording.modelInputSourceRanges, []);
  const modelInputWavBytes = recording.modelInputReliable && !recording.prefillFailed
      && recording.modelInputSourceRanges.length > 0
    ? policy.encodeInferencePcm16Wav(recording.modelInputPcm16Parts, recording.modelInputSampleCount)
    : null;
  assert.equal(modelInputWavBytes, null);
  assert.equal(context.sealInference({
    modelInputWavBytes,
    sourceSampleCount: recording.capturedSamples,
    modelInputSampleCount: recording.modelInputSampleCount,
    modelInputSourceRanges: recording.modelInputSourceRanges,
  }), null);
});
