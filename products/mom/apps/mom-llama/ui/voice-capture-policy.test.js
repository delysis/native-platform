"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const {
  analyzeVoiceSamples,
  createPlaybackEchoMatcher,
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
  const onset = new Float32Array(4096);
  onset.fill(.001, 0, 64);
  onset.fill(.02, 64);
  const started = analyzeVoiceSamples(initialVoiceActivity(), onset, 48000, 64);
  const microgap = analyzeVoiceSamples(started.activity, new Float32Array(256), 48000, 64);
  assert.ok(started.voiceOnsetSample >= 2000);
  assert.equal(microgap.silenceOnsetSample, null);
  const result = analyzeVoiceSamples(microgap.activity, new Float32Array(768), 48000, 64);
  assert.equal(result.silenceOnsetSample, 704);
  assert.equal(result.hadVoiced, true);
  assert.equal(result.subframes, 12);
  assert.equal(result.activity.voiced, false);
});

test("a short microphone spike never becomes a user turn", () => {
  const spike = new Float32Array(4096);
  spike.fill(.02, 0, 48000 * .032);
  const result = analyzeVoiceSamples(initialVoiceActivity(), spike, 48000, 64);
  assert.equal(result.activity.heardVoice, false);
  assert.equal(result.hadVoiced, false);
  assert.equal(result.voiceOnsetSample, null);
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

test("brief noise after a spoken turn cannot cancel speculative speech", () => {
  let activity = nextVoiceActivity(initialVoiceActivity(), .02, 120);
  assert.equal(activity.voiced, true);
  activity = nextVoiceActivity(activity, 0, 180);
  assert.equal(activity.voiced, false);
  activity = nextVoiceActivity(activity, .02, 8);
  assert.equal(activity.voiced, false);
  activity = nextVoiceActivity(activity, 0, 20);
  assert.equal(activity.voiced, false);
  activity = nextVoiceActivity(activity, .02, 40);
  assert.equal(activity.voiced, true);
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
  assert.equal(nextVoiceActivity(activity, .008, 85).voiced, true);
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

const deterministicAudio = (length, seed) => {
  let state = seed >>> 0;
  return Float32Array.from({ length }, () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return ((state / 0x1_0000_0000) * 2 - 1) * .12;
  });
};

const expandAt48k = (audio2k) => {
  const output = new Float32Array(audio2k.length * 24);
  for (let sample = 0; sample < audio2k.length; sample += 1) {
    output.fill(audio2k[sample], sample * 24, (sample + 1) * 24);
  }
  return output;
};

test("playback matcher recognizes correlated echo after stable 40 ms evidence", () => {
  const reference2k = deterministicAudio(2_000, 71);
  const start = 700;
  const microphone = expandAt48k(reference2k.slice(start, start + 80));
  const matcher = createPlaybackEchoMatcher();
  assert.equal(matcher.process(microphone, 48_000, expandAt48k(reference2k), 48_000), null);
  const match = matcher.process(microphone, 48_000, expandAt48k(reference2k), 48_000);
  assert.ok(match);
  assert.ok(match.correlation > .99);
  assert.ok(match.residualRms < .001);
});

test("echo-only startup does not open VAD before the playback match stabilizes", () => {
  const reference2k = deterministicAudio(2_000, 211);
  const reference = expandAt48k(reference2k);
  const matcher = createPlaybackEchoMatcher();
  let guard = initialPlaybackEchoGuard();
  let activity = initialVoiceActivity();
  let matchAtMs = null;
  let detectorReleaseAtMs = null;
  let voiceOnsetAtMs = null;
  const callbackSamples = 256;
  const silence = new Float32Array(callbackSamples);

  for (let callback = 0; callback < 24; callback += 1) {
    const start = callback * callbackSamples;
    const microphone = reference.subarray(start, start + callbackSamples);
    const match = matcher.process(microphone, 48_000, reference, 48_000);
    const elapsedMs = (start + callbackSamples) * 1_000 / 48_000;
    if (match && matchAtMs === null) matchAtMs = elapsedMs;
    const rawRms = Math.sqrt(microphone.reduce((sum, value) => sum + value * value, 0)
      / microphone.length);
    const echo = updatePlaybackEchoGuard(
      guard,
      match ? match.residualRms : rawRms,
      callbackSamples * 1_000 / 48_000,
      { matchedEcho: Boolean(match) },
    );
    guard = echo.guard;
    if (!echo.suppressed && detectorReleaseAtMs === null) detectorReleaseAtMs = elapsedMs;
    const analysis = analyzeVoiceSamples(
      activity,
      echo.suppressed ? silence : microphone,
      48_000,
    );
    activity = analysis.activity;
    if (analysis.voiceOnsetSample !== null && voiceOnsetAtMs === null) {
      voiceOnsetAtMs = start * 1_000 / 48_000
        + analysis.voiceOnsetSample * 1_000 / 48_000;
    }
  }

  assert.equal(detectorReleaseAtMs, 48, "unmatched playback energy reaches the detector at 48 ms");
  assert.equal(matchAtMs, 64, "256-sample callbacks quantize the stable match to 64 ms");
  assert.equal(voiceOnsetAtMs, null, "the brief unmatched window is shorter than VAD open time");
});

test("independent caller speech is not silenced when playback correlation is absent", () => {
  const reference = expandAt48k(deterministicAudio(2_000, 19));
  const speech = expandAt48k(deterministicAudio(80, 997));
  const matcher = createPlaybackEchoMatcher();
  assert.equal(matcher.process(speech, 48_000, reference, 48_000), null);
  assert.equal(matcher.process(speech, 48_000, reference, 48_000), null);

  let guard = initialPlaybackEchoGuard();
  for (let frame = 0; frame < 10; frame += 1) {
    const result = updatePlaybackEchoGuard(guard, .025, 5.33, { matchedEcho: false });
    guard = result.guard;
    if (frame < 9) assert.equal(result.suppressed, true);
    else assert.equal(result.suppressed, false);
  }
});

test("double talk leaves an independent residual and opens the caller gate", () => {
  const reference2k = deterministicAudio(2_000, 313);
  const start = 700;
  const referenceWindow = reference2k.slice(start, start + 80);
  const speech = deterministicAudio(80, 773);
  const mixed = Float32Array.from(referenceWindow, (value, index) => value + speech[index] * .7);
  const reference = expandAt48k(reference2k);
  const microphone = expandAt48k(mixed);
  const matcher = createPlaybackEchoMatcher();
  matcher.process(microphone, 48_000, reference, 48_000);
  const match = matcher.process(microphone, 48_000, reference, 48_000);
  assert.ok(match, "speaker echo should still be found during double talk");
  assert.ok(match.residualRms > .02, "independent caller energy remains in the residual");

  let guard = initialPlaybackEchoGuard();
  let result;
  for (let frame = 0; frame < 10; frame += 1) {
    result = updatePlaybackEchoGuard(guard, match.residualRms, 5.33, { matchedEcho: true });
    guard = result.guard;
  }
  assert.equal(result.suppressed, false);
});

test("voice progress deadline tolerates progress and bounds total generation time", () => {
  const { voiceProgressDeadlineExpired } = require("./voice-capture-policy.js");
  assert.equal(voiceProgressDeadlineExpired({
    now: 11_000, started: 0, lastProgress: 10_500, idleMs: 2_000,
  }), false, "slow but progressing token/audio delivery remains alive");
  assert.equal(voiceProgressDeadlineExpired({
    now: 12_001, started: 0, lastProgress: 10_000, idleMs: 2_000,
  }), true, "a stalled producer expires on inactivity");
  assert.equal(voiceProgressDeadlineExpired({
    now: 120_001, started: 0, lastProgress: 119_999, idleMs: 5_000,
  }), true, "regular progress cannot exceed the hard elapsed-time cap");
  assert.equal(voiceProgressDeadlineExpired({
    now: 500, started: 0, lastProgress: 500, idleMs: 1_000, maxElapsedMs: 0,
  }), true);
});
