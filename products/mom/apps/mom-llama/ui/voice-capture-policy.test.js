"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const {
  analyzeVoiceSamples,
  createStreamingMonoResampler,
  initialPlaybackEchoGuard,
  initialVoiceActivity,
  nextVoiceActivity,
  speechThreshold,
  splitPrefillPcm,
  updatePlaybackEchoGuard,
} = require("./voice-capture-policy.js");

test("resumed call prefill keeps a long captured utterance in bounded ordered blocks", () => {
  const samples = Float32Array.from({ length: 16_000 * 5 + 17 }, (_, index) => index);
  const blocks = splitPrefillPcm(samples);
  assert.equal(blocks.length, 126);
  assert.ok(blocks.slice(0, -1).every((block) => block.length === 640));
  assert.equal(blocks.at(-1).length, 17);
  assert.deepEqual(Array.from(blocks.flatMap((block) => Array.from(block))), Array.from(samples));
});

test("subframe detector rejects intra-speech microgaps and locates sustained silence", () => {
  const onset = new Float32Array(256);
  onset.fill(.001, 0, 64);
  onset.fill(.02, 64);
  const started = analyzeVoiceSamples(initialVoiceActivity(), onset, 48000, 64);
  const microgap = analyzeVoiceSamples(started.activity, new Float32Array(256), 48000, 64);
  assert.equal(started.voiceOnsetSample, 64);
  assert.equal(microgap.silenceOnsetSample, null);
  const result = analyzeVoiceSamples(microgap.activity, new Float32Array(768), 48000, 64);
  assert.equal(result.silenceOnsetSample, 704);
  assert.equal(result.hadVoiced, true);
  assert.equal(result.subframes, 12);
  assert.equal(result.activity.voiced, false);
});

test("streaming resampling preserves phase across render quanta", () => {
  const resampler = createStreamingMonoResampler(48000, 16000);
  const first = resampler.process(Float32Array.from([1, 1, 1, 2]));
  const second = resampler.process(Float32Array.from([2, 2, 3, 3, 3]));
  assert.deepEqual(Array.from(first), [1]);
  assert.deepEqual(Array.from(second), [2, 3]);
});

test("identity streaming resampling returns an owned copy", () => {
  const input = Float32Array.from([0.25, -0.5]);
  const output = createStreamingMonoResampler(16000).process(input);
  assert.deepEqual(Array.from(output), Array.from(input));
  assert.notEqual(output, input);
});

test("quiet speech opens the adaptive gate", () => {
  const next = nextVoiceActivity(initialVoiceActivity(), .004, 85);
  assert.equal(next.voiced, true);
  assert.equal(next.heardVoice, true);
  assert.equal(next.silentMs, 0);
});

test("post-speech silence advances continuously through the send threshold", () => {
  let activity = nextVoiceActivity(initialVoiceActivity(), .02, 85);
  for (let frame = 0; frame < 11; frame += 1) {
    activity = nextVoiceActivity(activity, 0, 85);
  }
  assert.equal(activity.voiced, false);
  assert.ok(activity.silentMs >= 900);
});

test("noise learning is bounded and does not absorb speech", () => {
  let activity = initialVoiceActivity();
  for (let frame = 0; frame < 20; frame += 1) {
    activity = nextVoiceActivity(activity, .0008, 85);
  }
  assert.ok(speechThreshold(activity.noiseFloor) >= .003);
  const speech = nextVoiceActivity(activity, .006, 85);
  assert.equal(speech.voiced, true);
  assert.equal(speech.noiseFloor, activity.noiseFloor);
});

test("streaming noise percentile follows a changed quiet floor in constant memory", () => {
  let activity = initialVoiceActivity();
  for (let frame = 0; frame < 100; frame += 1) {
    activity = nextVoiceActivity(activity, .0018 + ((frame % 3) * .00005), 1.33);
  }
  assert.ok(activity.noiseFloor > .0016);
  assert.ok(activity.noiseFloor < .0021);
  assert.equal(activity.noiseQuantile.heights.length, 5);
  assert.equal(nextVoiceActivity(activity, .008, 1.33).voiced, true);
});

test("playback echo is suppressed while the canceller converges", () => {
  let guard = initialPlaybackEchoGuard();
  for (let frame = 0; frame < 40; frame += 1) {
    const result = updatePlaybackEchoGuard(guard, .0074, 5.33);
    guard = result.guard;
    assert.equal(result.suppressed, true);
  }
  assert.ok(guard.residualRms > .006);
});

test("sustained double talk crosses the adaptive playback guard", () => {
  let guard = initialPlaybackEchoGuard();
  for (let frame = 0; frame < 48; frame += 1) {
    guard = updatePlaybackEchoGuard(guard, .006, 5.33).guard;
  }
  let result;
  for (let frame = 0; frame < 10; frame += 1) {
    result = updatePlaybackEchoGuard(guard, .03, 5.33);
    guard = result.guard;
  }
  assert.equal(result.suppressed, false);
  assert.ok(result.threshold > .009);
  assert.ok(result.threshold < .03);
});

test("speech during convergence does not poison the residual estimate", () => {
  let guard = initialPlaybackEchoGuard();
  for (let frame = 0; frame < 40; frame += 1) {
    guard = updatePlaybackEchoGuard(guard, .04, 5.33).guard;
  }
  assert.equal(guard.residualRms, .003);
});
