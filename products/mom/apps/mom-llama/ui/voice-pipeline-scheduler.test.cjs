const assert = require("node:assert/strict");
const test = require("node:test");

const {
  AdaptiveVoiceLeadController,
  SemanticLeadController,
  shouldRunCallTranscriptSlot,
  replayDeferredPcm,
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

test("deferred PCM replay includes audio captured during settlement exactly once before handoff", async () => {
  const captured = ["preroll", "onset"];
  const enqueued = [];
  let settlements = 0;
  let handedOff = false;
  const count = await replayDeferredPcm({
    readChunks: (cursor) => captured.slice(cursor),
    enqueue: (chunk) => enqueued.push(chunk),
    settle: async () => {
      settlements += 1;
      if (settlements === 1) captured.push("during-push");
    },
    handoff: () => { handedOff = true; },
  });
  assert.deepEqual(enqueued, ["preroll", "onset", "during-push"]);
  assert.equal(count, 3);
  assert.equal(settlements, 2);
  assert.equal(handedOff, true);
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

test("fast TTS uses short quanta so one segment cannot grossly overfill PCM", () => {
  const lead = new AdaptiveVoiceLeadController();
  lead.noteSegment({ synthesisMs: 600, audioMs: 2_000, firstAudioMs: 120, words: 5 });
  const words = lead.chunkWordLimit({ bufferedMs: lead.targetRunwayMs() });
  assert.ok(words >= 4 && words <= 6, `expected a short fast-producer quantum, got ${words}`);
});

test("near-real-time TTS uses a larger quantum to amortize switching", () => {
  const lead = new AdaptiveVoiceLeadController();
  for (let index = 0; index < 4; index += 1) {
    lead.noteSegment({ synthesisMs: 2_100, audioMs: 2_000, firstAudioMs: 220, words: 5 });
  }
  const slowWords = lead.chunkWordLimit({ bufferedMs: 0 });
  assert.ok(slowWords >= 8 && slowWords <= 16, `expected a bounded slow-producer quantum, got ${slowWords}`);
  assert.equal(lead.chunkWordLimit({ first: true, bufferedMs: 0 }), 4);
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

test("an uncalibrated first call buffers once, then healthy cadence restores streaming", () => {
  const fs = require("node:fs");
  const path = require("node:path");
  const source = fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");
  assert.match(source, /voiceLeadCalibration\s*\|\|\s*\{\s*initialBufferComplete:\s*true\s*\}/);

  const cold = new AdaptiveVoiceLeadController({ initialBufferComplete: true });
  assert.equal(cold.shouldBufferCompleteReply(), true);
  cold.noteSegment({ synthesisMs: 700, audioMs: 1_000 });
  assert.equal(cold.shouldBufferCompleteReply(), true);
  cold.noteSegment({ synthesisMs: 700, audioMs: 1_000 });
  assert.equal(cold.shouldBufferCompleteReply(), false);

  const healthyHandoff = new AdaptiveVoiceLeadController(cold.calibration());
  assert.equal(healthyHandoff.shouldBufferCompleteReply(), false);
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

test("speech-rate observations tune segment size without escaping hard bounds", () => {
  const lead = new AdaptiveVoiceLeadController();
  lead.noteSegment({ synthesisMs: 400, audioMs: 1_000, firstAudioMs: 90, words: 5 });
  assert.ok(lead.snapshot().observed_speech_ms_per_word < 400);
  assert.ok(lead.chunkWordLimit({ bufferedMs: 10_000 }) >= 4);
  assert.ok(lead.chunkWordLimit({ bufferedMs: 0 }) <= 16);
});

test("speech chunks preserve call latency and voice-note prosody", () => {
  const fs = require("node:fs");
  const vm = require("node:vm");
  const source = fs.readFileSync(require.resolve("./coop-hx.js"), "utf8");
  const start = source.indexOf("  const takeLiveSpeechChunk =");
  const end = source.indexOf("\n  const beginLiveVoiceTts =", start);
  assert.ok(start >= 0 && end > start);
  const context = {};
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.takeLiveSpeechChunk = takeLiveSpeechChunk;`, context);
  assert.equal(context.takeLiveSpeechChunk("One two three four five six", false, true, 6), null);
  assert.deepEqual(
    JSON.parse(JSON.stringify(context.takeLiveSpeechChunk("One two three four five six ", false, true, 6))),
    { chunk: "One two three four five six", rest: "" },
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(context.takeLiveSpeechChunk("One two three four five six s", false, true, 6))),
    { chunk: "One two three four five six", rest: "s" },
  );
  assert.equal(
    context.takeLiveSpeechChunk("One two three four five six seven eight nine ten", false, true, 10, "voice_note"),
    null,
  );
  assert.equal(
    context.takeLiveSpeechChunk(
      "One two three four five six seven eight nine ten eleven ",
      false,
      true,
      10,
      "voice_note",
    ),
    null,
  );
  assert.equal(
    context.takeLiveSpeechChunk(
      "That sounds hard, and I am listening",
      false,
      true,
      10,
      "voice_note",
    ),
    null,
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(context.takeLiveSpeechChunk(
      "That sounds hard, and I am listening carefully to you now.",
      false,
      true,
      10,
      "voice_note",
    ))),
    { chunk: "That sounds hard, and I am listening carefully to you now.", rest: "" },
  );
  assert.deepEqual(
    JSON.parse(JSON.stringify(context.takeLiveSpeechChunk(
      "I hear you. That sounds difficult, and I am listening carefully to what happened.",
      true,
      true,
      10,
      "voice_note",
    ))),
    {
      chunk: "I hear you. That sounds difficult, and I am listening carefully to what happened.",
      rest: "",
    },
  );
});
