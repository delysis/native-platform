const assert = require("node:assert/strict");
const test = require("node:test");

const {
  AdaptiveVoiceLeadController,
  SemanticLeadController,
  shouldRunCallTranscriptSlot,
  shouldDisableCallTranscriptSlotsAfterStop,
  canContinueVoiceHistoryRecovery,
  shouldWaitForVoiceHistoryRecovery,
  createCallTranscriptIntent,
  enqueueCallTranscriptIntent,
} = require("./voice-pipeline-scheduler.js");

test("voice lead dispatches immediately for startup and only near its adaptive floor later", () => {
  const lead = new AdaptiveVoiceLeadController();
  assert.equal(lead.shouldDispatch({ first: true, playing: true, bufferedMs: 2_000 }), true);
  assert.equal(lead.shouldDispatch({ playing: true, bufferedMs: 1_000 }), false);
  assert.equal(lead.shouldDispatch({ playing: true, bufferedMs: lead.targetRunwayMs() }), true);
});

test("next call prefill waits for adaptive runway after generation completes", () => {
  const lead = new AdaptiveVoiceLeadController();
  assert.equal(lead.shouldPrepareNextCall({ generationComplete: false, playing: true, bufferedMs: 1_000 }), false);
  assert.equal(lead.shouldPrepareNextCall({ generationComplete: true, playing: false, bufferedMs: 1_000 }), false);
  assert.equal(lead.shouldPrepareNextCall({ generationComplete: true, playing: true, bufferedMs: 239 }), false);
  assert.equal(lead.shouldPrepareNextCall({ generationComplete: true, playing: true, bufferedMs: 240 }), true);
  lead.noteSegment({ synthesisMs: 1_450, audioMs: 1_000, firstAudioMs: 260 });
  assert.equal(lead.shouldPrepareNextCall({ generationComplete: true, playing: true, bufferedMs: 240 }), false);
  assert.equal(lead.shouldPrepareNextCall({ generationComplete: true, playing: true, bufferedMs: lead.targetRunwayMs() }), true);
});

test("call transcript work runs only in a resident-model, playback-safe unattached slot", () => {
  const ready = {
    generationComplete: true,
    playbackSafe: true,
    qwenIdle: true,
    captureUnattached: true,
    callerVoiced: false,
    parakeetResident: true,
    transcriptSlotActive: false,
    disabled: false,
  };
  assert.equal(shouldRunCallTranscriptSlot(ready), true);
  for (const changed of [
    { qwenIdle: false },
    { captureUnattached: false },
    { callerVoiced: true },
    { parakeetResident: false },
    { transcriptSlotActive: true },
    { disabled: true },
    { playbackSafe: false },
    { generationComplete: false },
  ]) {
    assert.equal(shouldRunCallTranscriptSlot({ ...ready, ...changed }), false);
  }
});

test("cold Parakeet skips only the current slot; slow acknowledged stop disables later slots", () => {
  const ready = {
    generationComplete: true,
    playbackSafe: true,
    qwenIdle: true,
    captureUnattached: true,
    callerVoiced: false,
    parakeetResident: false,
    transcriptSlotActive: false,
    disabled: false,
  };
  assert.equal(shouldRunCallTranscriptSlot(ready), false);
  assert.equal(shouldDisableCallTranscriptSlotsAfterStop(undefined), false);
  assert.equal(shouldDisableCallTranscriptSlotsAfterStop(500), false);
  assert.equal(shouldDisableCallTranscriptSlotsAfterStop(500.01), true);
});

test("foreground work gates recovery without awaiting metadata, but waits an active operation or commit", () => {
  assert.equal(canContinueVoiceHistoryRecovery(), true);
  assert.equal(canContinueVoiceHistoryRecovery({ suspended: true }), false);
  assert.equal(canContinueVoiceHistoryRecovery({ foregroundDispatch: true }), false);
  assert.equal(shouldWaitForVoiceHistoryRecovery({ stage: "metadata" }), false);
  assert.equal(shouldWaitForVoiceHistoryRecovery({ operation: "exact-operation", stage: "transcription" }), true);
  assert.equal(shouldWaitForVoiceHistoryRecovery({ stage: "recording_commit" }), true);
});

test("queued call transcript intents retain only durable identities and have a hard bound", () => {
  const sourceCapture = { capturedPcm: [new Float32Array(24_000 * 60)], transcription: { reader: {} } };
  const trace = { events: [] };
  const intent = createCallTranscriptIntent({
    conversation: "conversation-a",
    output: { user_message_id: "message-a", voice_attachment_id: "attachment-a", audio: sourceCapture },
    fallbackAttachment: "fallback-a",
    sourceTrace: trace,
    sourceCapture,
  });
  assert.deepEqual(intent, {
    conversation: "conversation-a",
    output: { voice_attachment_id: "attachment-a", user_message_id: "message-a" },
    fallbackAttachment: "fallback-a",
    sourceTrace: trace,
  });
  assert.equal("sourceCapture" in intent, false);
  const queue = [];
  for (let index = 0; index < 64; index += 1) {
    assert.equal(enqueueCallTranscriptIntent(queue, { ...intent, output: {
      voice_attachment_id: `attachment-${index}`,
      user_message_id: `message-${index}`,
    } }), true);
  }
  assert.equal(enqueueCallTranscriptIntent(queue, intent), false);
  assert.equal(queue.length, 64);
});

test("slow synthesis and packet jitter raise the runway target", () => {
  const lead = new AdaptiveVoiceLeadController();
  const baseline = lead.targetRunwayMs();
  lead.notePacket({ audioMs: 139, gapMs: 320 });
  lead.noteSegment({ synthesisMs: 1_450, audioMs: 1_000, firstAudioMs: 260 });
  assert.ok(lead.targetRunwayMs() > baseline);
});

test("a deliberate segment wait cannot inflate its own maintenance target", () => {
  const lead = new AdaptiveVoiceLeadController();
  const startup = lead.startupRunwayMs();
  const maintenance = lead.targetRunwayMs();
  lead.notePacket({ audioMs: 139, gapMs: 740, boundary: true });
  assert.equal(lead.targetRunwayMs(), maintenance);
  assert.equal(lead.startupRunwayMs(), startup);
  assert.ok(lead.startupRunwayMs() <= 500);
});

test("startup runway covers the unplayed portion of a measured packet gap", () => {
  const lead = new AdaptiveVoiceLeadController({
    initialFirstAudioMs: 165,
    initialPacketAudioMs: 139,
    initialMaximumPacketGapMs: 250,
  });
  assert.equal(lead.startupRunwayMs(), 120);
  assert.ok(lead.startupRunwayMs() < 139, "healthy playback must start on the first packet");
  assert.ok(lead.startupRunwayMs() < lead.targetRunwayMs());
});

test("a gap that exceeds one packet by more than the measured floor waits for protection", () => {
  const lead = new AdaptiveVoiceLeadController({
    initialPacketAudioMs: 139,
    initialMaximumPacketGapMs: 320,
  });
  assert.ok(lead.startupRunwayMs() > 139);
});

test("healthy packet cadence starts at the minimum instead of hoarding two packets", () => {
  const lead = new AdaptiveVoiceLeadController({
    initialPacketAudioMs: 139,
    initialMaximumPacketGapMs: 139,
  });
  assert.equal(lead.startupRunwayMs(), 120);
});

test("startup delay tracks uncovered wall-clock deficit separately from buffered audio", () => {
  const lead = new AdaptiveVoiceLeadController({
    initialPacketAudioMs: 139,
    initialRequiredStartupDelayMs: 0,
  });
  assert.equal(lead.startupRunwayMs(), 120);
  assert.equal(lead.startupDelayMs(), 2);
  lead.notePacket({ audioMs: 139, gapMs: 218, startupDeficitMs: 11 });
  assert.equal(lead.startupRunwayMs(), 120);
  assert.equal(lead.startupDelayMs(), 13);
  assert.equal(lead.snapshot().observed_required_startup_delay_ms, 11);
});

test("an underrun raises protection immediately and healthy segments decay it", () => {
  const lead = new AdaptiveVoiceLeadController();
  const baseline = lead.targetRunwayMs();
  lead.noteUnderrun(80);
  const protectedTarget = lead.targetRunwayMs();
  assert.ok(protectedTarget >= baseline + 120);
  lead.noteSegment({ synthesisMs: 500, audioMs: 1_000, firstAudioMs: 125 });
  assert.ok(lead.snapshot().underrun_penalty_ms < 280);
});

test("scheduler telemetry retains waits and maximum observed runway", () => {
  const lead = new AdaptiveVoiceLeadController();
  lead.shouldDispatch({ playing: true, bufferedMs: 900 });
  lead.noteWait(123.456);
  assert.equal(lead.snapshot().waits, 1);
  assert.equal(lead.snapshot().wait_ms, 123.46);
  assert.equal(lead.snapshot().maximum_runway_ms, 900);
});

test("a following turn starts from measured machine cadence without inheriting queue counters", () => {
  const first = new AdaptiveVoiceLeadController();
  first.notePacket({ audioMs: 139, gapMs: 280 });
  first.noteSegment({ synthesisMs: 900, audioMs: 1_400, firstAudioMs: 175, words: 4 });
  first.noteUnderrun(30);
  const second = new AdaptiveVoiceLeadController(first.calibration());
  assert.equal(second.targetRunwayMs(), first.targetRunwayMs());
  assert.equal(second.startupRunwayMs(), first.startupRunwayMs());
  assert.equal(second.startupDelayMs(), first.startupDelayMs());
  assert.equal(second.snapshot().observed_max_packet_gap_ms, first.snapshot().observed_max_packet_gap_ms);
  assert.equal(second.snapshot().observed_max_boundary_gap_ms, first.snapshot().observed_max_boundary_gap_ms);
  assert.equal(second.snapshot().waits, 0);
  assert.equal(second.snapshot().maximum_runway_ms, 0);
});

test("semantic lead uses hysteresis and never pauses on an unusable partial chunk", () => {
  const lead = new SemanticLeadController();
  assert.equal(lead.shouldPause({ queuedWords: 24, readyChunk: false }), false);
  assert.equal(lead.shouldPause({ queuedWords: 16, readyChunk: true }), true);
  assert.equal(lead.shouldPause({ queuedWords: 10, readyChunk: true }), true);
  assert.equal(lead.shouldPause({ queuedWords: 8, readyChunk: true }), false);
  assert.equal(lead.snapshot().maximum_queued_words, 24);
});

test("semantic terminal always releases exact-request backpressure", () => {
  const lead = new SemanticLeadController();
  assert.equal(lead.shouldPause({ queuedWords: 18, readyChunk: true }), true);
  assert.equal(lead.shouldPause({ queuedWords: 18, readyChunk: true, terminal: true }), false);
});

test("complete buffering activates after sustained slow segments or one severe first segment", () => {
  const sustained = new AdaptiveVoiceLeadController();
  assert.equal(sustained.shouldBufferCompleteReply(), false);
  sustained.noteSegment({ synthesisMs: 1_100, audioMs: 1_000 });
  assert.equal(sustained.shouldBufferCompleteReply(), false);
  sustained.noteSegment({ synthesisMs: 1_060, audioMs: 1_000 });
  assert.equal(sustained.shouldBufferCompleteReply(), true);

  const severe = new AdaptiveVoiceLeadController();
  severe.noteSegment({ synthesisMs: 1_450, audioMs: 1_000 });
  assert.equal(severe.shouldBufferCompleteReply(), true);
});

test("complete buffering recovers only after healthy segments and survives reply calibration", () => {
  const slowReply = new AdaptiveVoiceLeadController();
  slowReply.noteSegment({ synthesisMs: 1_100, audioMs: 1_000 });
  slowReply.noteSegment({ synthesisMs: 1_100, audioMs: 1_000 });
  assert.equal(slowReply.shouldBufferCompleteReply(), true);

  const firstBufferedReply = new AdaptiveVoiceLeadController(slowReply.calibration());
  assert.equal(firstBufferedReply.shouldBufferCompleteReply(), true);
  firstBufferedReply.noteSegment({ synthesisMs: 700, audioMs: 1_000 });
  const stillBuffered = new AdaptiveVoiceLeadController(firstBufferedReply.calibration());
  assert.equal(stillBuffered.shouldBufferCompleteReply(), true);
  assert.equal(stillBuffered.snapshot().healthy_segment_count, 1);

  stillBuffered.noteSegment({ synthesisMs: 800, audioMs: 1_000 });
  assert.equal(stillBuffered.shouldBufferCompleteReply(), false);
  const recoveredReply = new AdaptiveVoiceLeadController(stillBuffered.calibration());
  assert.equal(recoveredReply.shouldBufferCompleteReply(), false);
});

test("a cold-call probe releases only for a complete, clearly healthy segment", () => {
  const fs = require("node:fs");
  const path = require("node:path");
  const source = fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");
  assert.match(source, /voiceLeadCalibration\s*\|\|\s*\{\s*initialBufferComplete:\s*true,\s*initialProbePending:\s*true\s*\}/);

  const cold = new AdaptiveVoiceLeadController({ initialBufferComplete: true, initialProbePending: true });
  assert.equal(cold.shouldBufferCompleteReply(), true);
  cold.noteSegment({ synthesisMs: 700, audioMs: 1_000, firstAudioMs: 150 });
  assert.equal(cold.shouldBufferCompleteReply(), false, "one complete healthy segment releases the cold gate");

  const healthyHandoff = new AdaptiveVoiceLeadController(cold.calibration());
  assert.equal(healthyHandoff.shouldBufferCompleteReply(), false);
});

test("cold-call buffering remains for short, slow, unknown, jittered, or previously-underrun probes", () => {
  const makeCold = (extra = {}) => new AdaptiveVoiceLeadController({
    initialBufferComplete: true,
    initialProbePending: true,
    ...extra,
  });
  const cases = [
    ["short", {}, { synthesisMs: 500, audioMs: 900, firstAudioMs: 140 }],
    ["slow", {}, { synthesisMs: 1_050, audioMs: 1_000, firstAudioMs: 150 }],
    ["unknown", {}, { synthesisMs: 500, audioMs: 0, firstAudioMs: 150 }],
    ["previous-underrun", { initialUnderrunPenaltyMs: 120 }, { synthesisMs: 500, audioMs: 1_000, firstAudioMs: 150 }],
  ];
  for (const [name, calibration, segment] of cases) {
    const cold = makeCold(calibration);
    cold.noteSegment(segment);
    assert.equal(cold.shouldBufferCompleteReply(), true, `${name} probe must retain complete-reply buffering`);
    if (name !== "unknown") {
      assert.equal(cold.snapshot().startup_probe_pending, false, `${name} measurement settles the one-time probe`);
    }
  }

  const jittered = makeCold();
  jittered.notePacket({ audioMs: 139, gapMs: 400, startupDeficitMs: 251 });
  jittered.noteSegment({ synthesisMs: 500, audioMs: 1_000, firstAudioMs: 150 });
  assert.equal(jittered.shouldBufferCompleteReply(), true, "measured startup jitter keeps the full-reply gate");

  const noFirstAudioMeasurement = makeCold();
  noFirstAudioMeasurement.noteSegment({ synthesisMs: 500, audioMs: 1_000 });
  assert.equal(noFirstAudioMeasurement.shouldBufferCompleteReply(), true);
  assert.equal(noFirstAudioMeasurement.snapshot().startup_probe_pending, false);
});

test("cold PCM probe opens on a sustained healthy prefix from the recorded packet schedule", () => {
  const coldTimeline = [
    [3328, 0], [5760, 123.7], [9600, 319.3], [9600, 517.6], [9600, 720.5],
    [9600, 931.9], [9600, 1149.1], [9600, 1366.9], [9600, 1573.9],
    [9600, 1780.2], [5760, 1919.5], [4352, 2109.4],
  ];
  const controller = new AdaptiveVoiceLeadController({
    initialBufferComplete: true,
    initialProbePending: true,
  });
  let elapsed = 0;
  let pcmMs = 0;
  for (let index = 0; index < coldTimeline.length; index += 1) {
    const [samples, relativeArrivalMs] = coldTimeline[index];
    const audioMs = samples / 24;
    const gapMs = index === 0 ? null : relativeArrivalMs - elapsed;
    const startupDeficitMs = Math.max(0, relativeArrivalMs - pcmMs);
    controller.notePacket({ audioMs, gapMs, startupDeficitMs });
    elapsed = relativeArrivalMs;
    pcmMs += audioMs;

    if (index < 4) {
      assert.equal(controller.shouldBufferCompleteReply(), true, `packet ${index + 1} is insufficient evidence`);
    }
    if (index === 4) {
      assert.equal(controller.shouldBufferCompleteReply(), false, "packet five releases before sentence EOF");
      assert.equal(controller.snapshot().startup_probe_released_early, true);
      assert.equal(controller.snapshot().startup_probe_packets, 5);
      assert.ok(pcmMs - elapsed >= 600, "the released prefix retains the measured lead margin");
    }
  }
});

test("warm recorded packet schedule satisfies the cold probe at packet five too", () => {
  const warmTimeline = [
    [3328, 0], [5760, 112.7], [9600, 300.7], [9600, 487.3], [9600, 673.2],
    [9600, 866.4], [9600, 1063.0], [9600, 1280.1], [9600, 1495.7],
    [9600, 1711.4], [5760, 1857.3], [4352, 2040.9],
  ];
  const controller = new AdaptiveVoiceLeadController({
    initialBufferComplete: true,
    initialProbePending: true,
  });
  let elapsed = 0;
  let pcmMs = 0;
  for (let index = 0; index < warmTimeline.length; index += 1) {
    const [samples, relativeArrivalMs] = warmTimeline[index];
    const audioMs = samples / 24;
    controller.notePacket({
      audioMs,
      gapMs: index === 0 ? null : relativeArrivalMs - elapsed,
      startupDeficitMs: Math.max(0, relativeArrivalMs - pcmMs),
    });
    elapsed = relativeArrivalMs;
    pcmMs += audioMs;
    if (index < 4) assert.equal(controller.shouldBufferCompleteReply(), true);
    if (index === 4) {
      assert.equal(controller.shouldBufferCompleteReply(), false);
      assert.equal(controller.snapshot().startup_probe_released_early, true);
      assert.equal(controller.snapshot().startup_probe_packets, 5);
      assert.ok(pcmMs - elapsed >= 600);
    }
  }
});

test("cold PCM probe falls back for slow, deceptive, stalled, or previously-underrun schedules", () => {
  const coldTimeline = [
    [3328, 0], [5760, 123.7], [9600, 319.3], [9600, 517.6], [9600, 720.5],
    [9600, 931.9], [9600, 1149.1], [9600, 1366.9], [9600, 1573.9],
    [9600, 1780.2], [5760, 1919.5], [4352, 2109.4],
  ];
  const makeCold = (extra = {}) => new AdaptiveVoiceLeadController({
    initialBufferComplete: true,
    initialProbePending: true,
    ...extra,
  });
  const replay = (gapScale = () => 1, stall = null, extra = {}) => {
    const lead = makeCold(extra);
    let previousArrival = 0;
    let previousRawArrival = 0;
    let cumulativeAudioMs = 0;
    for (let index = 0; index < coldTimeline.length; index += 1) {
      const [samples, rawArrival] = coldTimeline[index];
      const arrival = index === 0 ? rawArrival : previousArrival
        + (rawArrival - previousRawArrival) * gapScale(index)
        + (stall && index === stall.index ? stall.ms : 0);
      const audioMs = samples / 24;
      const gapMs = index === 0 ? null : arrival - previousArrival;
      const startupDeficitMs = Math.max(0, arrival - cumulativeAudioMs);
      lead.notePacket({ audioMs, gapMs, startupDeficitMs });
      previousArrival = arrival;
      previousRawArrival = rawArrival;
      cumulativeAudioMs += audioMs;
    }
    return lead;
  };

  for (const scale of [2, 3]) {
    const slow = replay(() => scale);
    assert.equal(slow.shouldBufferCompleteReply(), true, `${scale}x slower cadence must use full buffering`);
    assert.equal(slow.snapshot().startup_probe_released_early, false);
  }

  const deceptive = replay((index) => (index <= 2 ? 0.5 : 2));
  assert.equal(deceptive.shouldBufferCompleteReply(), true, "two early fast intervals cannot mask later 2x cadence");
  assert.equal(deceptive.snapshot().startup_probe_released_early, false);

  const stalledBeforeRelease = replay(() => 1, { index: 3, ms: 1_100 });
  assert.equal(stalledBeforeRelease.shouldBufferCompleteReply(), true, "a 1.1 s stall before packet four keeps the cold probe closed");
  assert.equal(stalledBeforeRelease.snapshot().startup_probe_released_early, false);

  const hadUnderrun = replay(() => 1, null, { initialUnderrunPenaltyMs: 120 });
  assert.equal(hadUnderrun.shouldBufferCompleteReply(), true);
  assert.equal(hadUnderrun.snapshot().startup_probe_released_early, false);
});

test("warm controllers keep their existing buffer policy and do not enter the cold probe", () => {
  const warm = new AdaptiveVoiceLeadController();
  warm.notePacket({ audioMs: 138.667, gapMs: null, startupDeficitMs: 0 });
  warm.notePacket({ audioMs: 240, gapMs: 123.7, startupDeficitMs: 0 });
  assert.equal(warm.shouldBufferCompleteReply(), false);
  assert.equal(warm.snapshot().startup_probe_pending, false);
  assert.equal(warm.snapshot().startup_probe_released_early, false);
  assert.equal(warm.snapshot().startup_probe_packets, 0);
});

test("a post-open 1.1 s stall fits measured runway; a real queue deficit uses existing recovery", () => {
  const lead = new AdaptiveVoiceLeadController({ initialBufferComplete: true, initialProbePending: true });
  const prefix = [
    { audioMs: 138.667, gapMs: null, startupDeficitMs: 0 },
    { audioMs: 240, gapMs: 123.7, startupDeficitMs: 0 },
    { audioMs: 400, gapMs: 195.6, startupDeficitMs: 0 },
    { audioMs: 400, gapMs: 198.3, startupDeficitMs: 0 },
    { audioMs: 400, gapMs: 202.9, startupDeficitMs: 0 },
  ];
  for (const packet of prefix) lead.notePacket(packet);
  assert.equal(lead.shouldBufferCompleteReply(), false);
  assert.equal(lead.snapshot().startup_probe_released_early, true);

  let queuedMs = prefix.reduce((sum, packet) => sum + packet.audioMs, 0);
  const postOpenGapMs = 1_311.4;
  const postOpenDeficitMs = Math.max(0, postOpenGapMs - queuedMs);
  assert.equal(postOpenDeficitMs, 0, "the released prefix covers one 1.1 s late packet");
  queuedMs -= postOpenGapMs;
  lead.notePacket({ audioMs: 400, gapMs: postOpenGapMs, startupDeficitMs: 0 });
  queuedMs += 400;
  assert.equal(lead.snapshot().underrun_penalty_ms, 0, "a covered late packet does not invent an underrun");

  const before = lead.startupRunwayMs();
  const laterGapMs = 1_800;
  const laterDeficitMs = Math.max(0, laterGapMs - queuedMs);
  assert.ok(laterDeficitMs > 0, "a later gap exceeds queued PCM");
  lead.notePacket({ audioMs: 400, gapMs: laterGapMs, startupDeficitMs: 0 });
  lead.noteUnderrun(laterDeficitMs);
  assert.equal(lead.startupRunwayMs(), before, "the existing bounded startup runway stays capped");
  assert.ok(lead.snapshot().underrun_penalty_ms > 0, "an observed queue deficit activates existing underrun recovery");
  assert.equal(lead.snapshot().startup_probe_released_early, true, "the one-shot cold probe is not rerun");
});

test("progress timeout grows with measured slow-machine cadence and stays bounded", () => {
  const healthy = new AdaptiveVoiceLeadController();
  assert.equal(healthy.progressTimeoutMs(), 20_000);

  const slower = new AdaptiveVoiceLeadController({
    initialFirstAudioMs: 3_000,
    initialMaximumPacketGapMs: 4_500,
  });
  assert.equal(slower.progressTimeoutMs(), 36_000);

  const verySlow = new AdaptiveVoiceLeadController({
    initialFirstAudioMs: 15_000,
    initialMaximumPacketGapMs: 20_000,
  });
  assert.equal(verySlow.progressTimeoutMs(), 60_000);
});

test("call speech waits for complete sentences and voice notes retain phrase rules", () => {
  const fs = require("node:fs");
  const vm = require("node:vm");
  const source = fs.readFileSync(require.resolve("./coop-hx.js"), "utf8");
  const start = source.indexOf("  const takeLiveSpeechChunk =");
  const end = source.indexOf("\n  const beginLiveVoiceTts =", start);
  assert.ok(start >= 0 && end > start);
  const context = {};
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.takeLiveSpeechChunk = takeLiveSpeechChunk;`, context);
  const chunk = (text, flush = false, first = false, mode = "call") =>
    context.takeLiveSpeechChunk(text, flush, first, mode);

  assert.equal(chunk("One two three four five six"), null);
  assert.deepEqual(JSON.parse(JSON.stringify(chunk("One two three four five six. "))), {
    chunk: "One two three four five six.", rest: "", boundary: "sentence",
  });
  assert.equal(chunk("A partial sentence keeps arriving without punctuation"), null);
  assert.deepEqual(JSON.parse(JSON.stringify(chunk("Dr. Smith met the U.S. team at 3.14 p.m. today. Next sentence. "))), {
    chunk: "Dr. Smith met the U.S. team at 3.14 p.m. today.",
    rest: "Next sentence. ", boundary: "sentence",
  });
  const sentenceStream = "First one ends. Second one ends. ";
  const firstSentence = chunk(sentenceStream);
  const secondSentence = chunk(firstSentence.rest);
  assert.equal(`${firstSentence.chunk} ${secondSentence.chunk}`, sentenceStream.trim());

  const streamedInput = "Dr. Smith met the U.S. team at 3.14 p.m. today. The next sentence is complete. final unpunctuated tail";
  const streamedSentences = [];
  let pending = "";
  for (const character of streamedInput) {
    pending += character;
    const ready = chunk(pending);
    if (ready) {
      streamedSentences.push(ready.chunk);
      pending = ready.rest;
    }
  }
  assert.deepEqual(streamedSentences, [
    "Dr. Smith met the U.S. team at 3.14 p.m. today.",
    "The next sentence is complete.",
  ]);
  assert.equal(pending, "final unpunctuated tail");
  assert.deepEqual(JSON.parse(JSON.stringify(chunk(pending, true))), {
    chunk: "final unpunctuated tail", rest: "", boundary: "end",
  });

  assert.deepEqual(JSON.parse(JSON.stringify(chunk("A final unterminated sentence", true))), {
    chunk: "A final unterminated sentence", rest: "", boundary: "end",
  });

  const clauseWords = Array.from({ length: 70 }, (_, index) => `word${index + 1}`);
  clauseWords[18] += ",";
  const clause = chunk(`${clauseWords.join(" ")} `);
  assert.deepEqual(JSON.parse(JSON.stringify(clause)), {
    chunk: clauseWords.slice(0, 19).join(" "),
    rest: `${clauseWords.slice(19).join(" ")} `, boundary: "limit_clause",
  });
  const words = Array.from({ length: 70 }, (_, index) => `word${index + 1}`);
  const sixtyFourWithoutSeparator = words.slice(0, 64).join(" ");
  assert.equal(chunk(sixtyFourWithoutSeparator), null, "the 64th partial word is not dispatched");
  const sixtyFourComplete = chunk(`${sixtyFourWithoutSeparator} `);
  assert.equal(sixtyFourComplete.boundary, "limit_words");
  assert.equal(sixtyFourComplete.chunk.split(/\s+/).length, 64);
  const capped = chunk(`${words.join(" ")} `);
  assert.equal(capped.boundary, "limit_words");
  assert.equal(capped.chunk.split(/\s+/).length, 64);
  assert.equal(`${capped.chunk} ${capped.rest}`.trim().split(/\s+/).length, 70);

  const semanticLead = new SemanticLeadController();
  const longSentence = Array.from({ length: 24 }, (_, index) => `word${index + 1}`).join(" ");
  assert.equal(chunk(longSentence), null);
  assert.equal(semanticLead.shouldPause({ queuedWords: 24, readyChunk: false }), false);
  const completedLongSentence = chunk(`${longSentence}. `);
  assert.equal(completedLongSentence.boundary, "sentence");
  assert.equal(semanticLead.shouldPause({ queuedWords: 24, readyChunk: true }), true);

  assert.equal(chunk("One two three four five six seven eight nine ten", false, true, "voice_note"), null);
  const voiceNotePartial = Array.from({ length: 27 }, (_, index) => `word${index + 1}`).join(" ") + " partial";
  assert.equal(chunk(voiceNotePartial, false, false, "voice_note"), null);
  const voiceNoteComplete = `${voiceNotePartial} `;
  assert.deepEqual(JSON.parse(JSON.stringify(chunk(voiceNoteComplete, false, false, "voice_note"))), {
    chunk: voiceNotePartial, rest: "", boundary: "limit_words",
  });
  const voiceNoteNextWord = chunk(`${voiceNotePartial} next`, false, false, "voice_note");
  assert.deepEqual(JSON.parse(JSON.stringify(voiceNoteNextWord)), {
    chunk: voiceNotePartial, rest: "next", boundary: "limit_words",
  });
  assert.equal(`${voiceNoteNextWord.chunk} ${voiceNoteNextWord.rest}`, `${voiceNotePartial} next`);
  assert.equal(chunk("That sounds hard, and I am listening", false, true, "voice_note"), null);
  assert.deepEqual(JSON.parse(JSON.stringify(chunk(
    "That sounds hard, and I am listening carefully to you now and I can stay with you, then we can talk.", false, false, "voice_note",
  ))), {
    chunk: "That sounds hard, and I am listening carefully to you now and I can stay with you,",
    rest: "then we can talk.", boundary: "phrase",
  });
  assert.deepEqual(JSON.parse(JSON.stringify(chunk(
    "I hear you. That sounds difficult, and I am listening carefully to what happened.", true, true, "voice_note",
  ))), {
    chunk: "I hear you. That sounds difficult, and I am listening carefully to what happened.",
    rest: "", boundary: "phrase",
  });
});
