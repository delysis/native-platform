"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");

const source = fs.readFileSync(`${__dirname}/coop-hx.js`, "utf8");
const start = source.indexOf("  const indexVoiceTurn = (");
const end = source.indexOf("\n  const startPendingCallTranscriptSlot =", start);
assert.ok(start >= 0 && end > start, "production indexVoiceTurn must remain extractable");
const definition = source.slice(start, end);
const canContinueStart = source.indexOf("  const canContinueVoiceHistoryRecovery = () =>");
const canContinueEnd = source.indexOf("\n  const recoverPendingVoiceHistory =", canContinueStart);
const canContinueDefinition = source.slice(canContinueStart, canContinueEnd);
const suspendStart = source.indexOf("  const suspendVoiceHistoryRecoveryForForeground = async");
const suspendEnd = source.indexOf("\n  const startVoicePrefill =", suspendStart);
const suspendDefinition = source.slice(suspendStart, suspendEnd);
assert.ok(canContinueStart >= 0 && canContinueEnd > canContinueStart);
assert.ok(suspendStart >= 0 && suspendEnd > suspendStart);

function makeHarness({ transcription, commit, preview, stopSpeechOperationAndWait,
  attachment = "recording-a", message = "message-a", sourceCapture = null } = {}) {
  const calls = [];
  const stages = [];
  const errors = [];
  const retries = [];
  const hashInputs = [];
  let now = 0;
  const context = {
    voiceCallActive: false,
    voiceCallPreparing: false,
    voiceRecording: null,
    activeDispatchConversation: null,
    voiceLatencyTrace: "trace-a",
    pendingCallTranscriptJobs: [],
    activeCallTranscriptJob: null,
    activeCallTranscriptTask: null,
    activeCallTranscriptOperation: null,
    canContinueResult: false,
    recoveryEligibility: null,
    stopSpeechOperationAndWait: stopSpeechOperationAndWait || (async () => {}),
    MomVoicePipelineScheduler: {
      canContinueVoiceHistoryRecovery: (eligibility) => {
        context.recoveryEligibility = eligibility;
        return context.canContinueResult;
      },
      shouldWaitForVoiceHistoryRecovery: ({ operation, stage }) => Boolean(operation) || stage === "recording_commit",
      shouldDisableCallTranscriptSlotsAfterStop: () => false,
    },
    performance: { now: () => ++now },
    speechOperationToken: () => "operation-a",
    markVoiceLatencyIn: (_trace, stage, detail) => stages.push({ stage, detail }),
    errorMessage: (error) => error instanceof Error ? error.message : String(error),
    reportError: (error) => errors.push(error),
    retryCallPrefillAfterTranscript: (conversation) => retries.push(conversation),
    attachmentPreviewResult: (value) => value?.result ?? value,
    attachmentPreviewAnchor: () => ({}),
    sha256Hex: async (bytes) => { hashInputs.push(bytes.byteLength); return "archive-sha"; },
    encodePcmWav: () => new Uint8Array([1, 2, 3]),
    invoke: async (command, args) => {
      calls.push({ command, args });
      if (command === "mom_llama_attachment_preview") {
        if (preview) return preview(args);
        return { result: { primary: { kind: "audio", id: "audio-a" }, root_sha256: "root-a" } };
      }
      if (command === "mom_llama_speech_transcribe_attachment") {
        return typeof transcription === "function" ? transcription(args) : transcription;
      }
      if (command === "mom_llama_message_record_voice_transcript") {
        return typeof commit === "function" ? commit(args) : commit;
      }
      throw new Error(`unexpected command ${command}`);
    },
  };
  vm.createContext(context);
  vm.runInContext(`
    let voiceHistoryRecoverySuspensions = 0;
    let voiceHistoryRecovery = null;
    let voiceHistoryRecoveryOperation = null;
    let voiceHistoryRecoveryStage = "idle";
    let voiceHistoryRecoveryColdInitialization = false;
    let voiceCallTranscriptSlotsDisabled = false;
    const idleVoiceTranscriptJobs = new Set();
    ${definition}
    ${canContinueDefinition}
    ${suspendDefinition}
    this.indexVoiceTurn = indexVoiceTurn;
    this.suspend = suspendVoiceHistoryRecoveryForForeground;
    this.canContinueRecovery = canContinueVoiceHistoryRecovery;
    this.idleJobs = idleVoiceTranscriptJobs;
    this.setRecovery = (value) => { voiceHistoryRecovery = value; };
  `, context);
  return {
    calls,
    stages,
    errors,
    retries,
    context,
    hashInputs,
    idleJobs: context.idleJobs,
    canContinueRecovery: () => context.canContinueRecovery(),
    suspend: (reason = "foreground") => context.suspend(reason),
    run: (options = {}) => context.indexVoiceTurn("conversation-a", {
      voice_attachment_id: attachment,
      user_message_id: message,
    }, null, sourceCapture, "trace-a", options),
  };
}

const contractedTranscript = (transcript) => ({ status: "contracted", result: { transcript } });
const contractedCommit = { status: "contracted", result: { recorded: true } };
const deferred = () => {
  let resolve;
  let reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};

test("empty successful ASR is retained as a skip and never commits or retries", async () => {
  const harness = makeHarness({ transcription: contractedTranscript(" \n "), commit: contractedCommit });
  assert.equal(await harness.run(), false);
  assert.equal(harness.calls.filter(({ command }) => command === "mom_llama_message_record_voice_transcript").length, 0);
  assert.deepEqual(harness.errors, []);
  assert.deepEqual(harness.retries, []);
  assert.equal(harness.stages.some(({ stage }) => stage === "voice_transcript_finished"), false);
  assert.ok(harness.stages.some(({ stage, detail }) => stage === "voice_transcript_skipped"
    && detail?.reason === "empty_transcript" && detail?.recording_retained === true));
});

test("only a successful durable transcript commit returns true and finishes the turn", async () => {
  const harness = makeHarness({ transcription: contractedTranscript("Please call me tomorrow."), commit: contractedCommit });
  assert.equal(await harness.run(), true);
  assert.equal(harness.calls.filter(({ command }) => command === "mom_llama_message_record_voice_transcript").length, 1);
  assert.deepEqual(harness.errors, []);
  assert.deepEqual(harness.retries, ["conversation-a"]);
  assert.equal(harness.stages.filter(({ stage }) => stage === "voice_transcript_finished").length, 1);
  assert.equal(harness.stages.filter(({ stage }) => stage === "voice_transcript_failed").length, 0);
});

test("live PCM uses the native accepted-capture receipt and hashes only its archived WAV", async () => {
  const identity = { turn: "turn-a", capture: "capture-a", conversation: "conversation-a" };
  const samples = new Float32Array([0.25, -0.5]);
  const sourceCapture = {
    transcription: {
      finished: Promise.resolve({ source: {
        identity,
        samples: samples.length,
        // The accepted PCM digest is native-authored. The renderer does not
        // rescan retained PCM to recreate this binding.
        pcm_f32le_sha256: "native-digest",
      }, transcript: "A grounded live transcript." }),
      metrics: () => identity,
    },
    samples: () => samples,
  };
  const harness = makeHarness({
    sourceCapture,
    preview: async () => ({ result: {
      primary: { kind: "audio", id: "audio-a" }, root_sha256: "archive-sha",
    } }),
    transcription: contractedTranscript("Fallback should not run."),
    commit: contractedCommit,
  });

  assert.equal(await harness.run(), true);
  assert.deepEqual(harness.hashInputs, [3], "only the encoded archival WAV is hashed");
  assert.equal(harness.calls.some(({ command }) => command === "mom_llama_speech_transcribe_attachment"), false);
  const record = harness.calls.find(({ command }) => command === "mom_llama_message_record_voice_transcript");
  assert.equal(record?.args.transcript, "A grounded live transcript.");
  assert.ok(harness.stages.some(({ stage }) => stage === "voice_transcript_live_verified"));
});

test("live source identity and archived-audio mismatches still fall back to stored-audio ASR", async () => {
  for (const mismatch of ["capture", "archive"]) {
    const identity = { turn: "turn-a", capture: "capture-a", conversation: "conversation-a" };
    const sourceIdentity = mismatch === "capture" ? { ...identity, capture: "stale-capture" } : identity;
    const samples = new Float32Array([0.25, -0.5]);
    const harness = makeHarness({
      sourceCapture: {
        transcription: {
          finished: Promise.resolve({ source: {
            identity: sourceIdentity,
            samples: samples.length,
            pcm_f32le_sha256: "native-digest",
          }, transcript: "Unadmitted live transcript." }),
          metrics: () => identity,
        },
        samples: () => samples,
      },
      preview: async () => ({ result: {
        primary: { kind: "audio", id: "audio-a" },
        root_sha256: mismatch === "archive" ? "different-archive" : "archive-sha",
      } }),
      transcription: contractedTranscript("Stored-audio fallback."),
      commit: contractedCommit,
    });

    assert.equal(await harness.run(), true, `${mismatch} mismatch falls back safely`);
    assert.deepEqual(harness.hashInputs, [3]);
    assert.equal(harness.calls.filter(({ command }) => command === "mom_llama_speech_transcribe_attachment").length, 1);
    const record = harness.calls.find(({ command }) => command === "mom_llama_message_record_voice_transcript");
    assert.equal(record?.args.transcript, "Stored-audio fallback.");
    assert.equal(harness.stages.some(({ stage }) => stage === "voice_transcript_live_verified"), false);
  }
});

test("blocked ASR stays an error and cannot commit or retry", async () => {
  const harness = makeHarness({
    transcription: { status: "blocked", blocker: { code: "asr_unavailable", message: "ASR unavailable" } },
    commit: contractedCommit,
  });
  await assert.rejects(harness.run(), /ASR unavailable/);
  assert.equal(harness.calls.filter(({ command }) => command === "mom_llama_message_record_voice_transcript").length, 0);
  assert.equal(harness.errors.length, 1);
  assert.deepEqual(harness.retries, []);
  assert.equal(harness.stages.filter(({ stage }) => stage === "voice_transcript_finished").length, 0);
});

test("missing recording identity remains an error before ASR admission", async () => {
  const harness = makeHarness({ transcription: contractedTranscript("unreachable"), commit: contractedCommit, attachment: null });
  await assert.rejects(harness.run(), /recording identity/);
  assert.equal(harness.calls.length, 0);
  assert.equal(harness.errors.length, 1);
  assert.deepEqual(harness.retries, []);
});

test("blocked durable commit remains an error and cannot finish or retry", async () => {
  const harness = makeHarness({
    transcription: contractedTranscript("Keep this recording."),
    commit: { status: "blocked", blocker: { code: "commit_denied", message: "Transcript storage denied" } },
  });
  await assert.rejects(harness.run(), /Transcript storage denied/);
  assert.equal(harness.calls.filter(({ command }) => command === "mom_llama_message_record_voice_transcript").length, 1);
  assert.equal(harness.errors.length, 1);
  assert.deepEqual(harness.retries, []);
  assert.equal(harness.stages.filter(({ stage }) => stage === "voice_transcript_finished").length, 0);
  assert.equal(harness.stages.filter(({ stage }) => stage === "voice_transcript_failed").length, 1);
});

test("foreground waits for exact idle ASR stop acknowledgement and task settlement", async () => {
  const asr = deferred();
  const asrStarted = deferred();
  const stopAck = deferred();
  const stopRequested = deferred();
  const events = [];
  const harness = makeHarness({
    transcription: () => {
      events.push("asr_admitted");
      asrStarted.resolve();
      return asr.promise;
    },
    commit: contractedCommit,
    stopSpeechOperationAndWait: async (operation, reason) => {
      events.push(`stop:${operation}:${reason}`);
      stopRequested.resolve();
      await stopAck.promise;
      events.push("stop_ack");
    },
  });
  const task = harness.run();
  await asrStarted.promise;
  assert.equal(harness.idleJobs.size, 1);
  assert.equal(harness.canContinueRecovery(), false, "idle ASR occupies the recovery scheduler slot");
  assert.equal(harness.context.recoveryEligibility.recording, true);

  let suspended = false;
  const suspension = harness.suspend("call_start").then((release) => {
    suspended = true;
    return release;
  });
  await stopRequested.promise;
  assert.deepEqual(events, ["asr_admitted", "stop:operation-a:call_start"]);
  assert.equal(suspended, false);

  stopAck.resolve();
  await Promise.resolve();
  assert.equal(suspended, false, "foreground also waits for the transcription task to settle after stop ack");
  asr.resolve(contractedTranscript("This pending result must not replace the saved recording."));
  assert.equal(await task, false, "preempted ASR is ignored and cannot commit a transcript");
  const release = await suspension;
  events.push("foreground_model_admitted");
  assert.deepEqual(events, [
    "asr_admitted", "stop:operation-a:call_start", "stop_ack", "foreground_model_admitted",
  ]);
  assert.equal(harness.calls.some(({ command }) => command === "mom_llama_message_record_voice_transcript"), false);
  assert.deepEqual(harness.errors, [], "expected foreground preemption is not surfaced as a toast/error");
  assert.equal(harness.stages.some(({ stage }) => stage === "voice_transcript_failed"), false);
  assert.equal(harness.idleJobs.size, 0);
  release({ resume: false });
});

test("foreground cancelling a pending catalog read prevents later ASR admission", async () => {
  const catalog = deferred();
  const catalogStarted = deferred();
  const events = [];
  const harness = makeHarness({
    preview: () => {
      catalogStarted.resolve();
      return catalog.promise;
    },
    transcription: () => { events.push("asr_admitted"); return contractedTranscript("late"); },
    commit: contractedCommit,
  });
  const task = harness.run();
  await catalogStarted.promise;
  const suspension = harness.suspend("voice_capture");
  catalog.resolve({ result: { primary: { kind: "audio", id: "audio-a" }, root_sha256: "root-a" } });
  assert.equal(await task, false);
  const release = await suspension;
  assert.deepEqual(events, [], "preempted metadata work never admits the native transcription operation");
  assert.equal(harness.calls.some(({ command }) => command === "mom_llama_message_record_voice_transcript"), false);
  assert.deepEqual(harness.errors, []);
  assert.equal(harness.idleJobs.size, 0);
  release({ resume: false });
});

test("foreground waits for an already-started transcript commit", async () => {
  const commit = deferred();
  const commitStarted = deferred();
  const events = [];
  let stopCalls = 0;
  const harness = makeHarness({
    transcription: contractedTranscript("A completed transcript."),
    commit: () => {
      events.push("commit_started");
      commitStarted.resolve();
      return commit.promise;
    },
    stopSpeechOperationAndWait: async () => { stopCalls += 1; },
  });
  const task = harness.run();
  await commitStarted.promise;
  assert.equal(harness.idleJobs.size, 1);
  assert.equal(harness.canContinueRecovery(), false, "recovery cannot overlap the owned transcript job");

  let suspended = false;
  const suspension = harness.suspend("chat_send").then((release) => {
    suspended = true;
    return release;
  });
  await Promise.resolve();
  assert.equal(stopCalls, 0, "ASR already terminated before transcript storage began");
  assert.equal(suspended, false, "the foreground waits for the exact record operation to finish");
  commit.resolve(contractedCommit);
  assert.equal(await task, true);
  const release = await suspension;
  events.push("foreground_model_admitted");
  assert.deepEqual(events, ["commit_started", "foreground_model_admitted"]);
  assert.equal(harness.calls.some(({ command }) => command === "mom_llama_message_record_voice_transcript"), true);
  assert.equal(harness.errors.length, 0);
  assert.equal(harness.idleJobs.size, 0);
  release({ resume: false });
});

test("ordinary capture starts before waiting for ASR and discard blocks later prefill", async () => {
  const captureStart = source.indexOf("  const beginNativeVoiceCapture = async");
  const captureEnd = source.indexOf("\n  const beginVoiceCapture =", captureStart);
  const prefillStart = source.indexOf("  const prepareVoicePrefill = async");
  const prefillEnd = source.indexOf("\n  let pendingCallTranscriptJobs", prefillStart);
  const stopStart = source.indexOf("  const stopVoiceCapture = async", prefillEnd);
  const abortStart = source.indexOf("  const abortStoppedVoicePrefill = (", stopStart - 5000);
  const stopEnd = source.indexOf("\n  const interruptCallPlaybackForSpeech", stopStart);
  assert.ok(captureStart >= 0 && captureEnd > captureStart);
  assert.ok(prefillStart >= 0 && prefillEnd > prefillStart);
  assert.ok(stopStart >= 0 && stopEnd > stopStart);
  assert.ok(abortStart >= 0 && stopStart > abortStart);
  const events = [];
  const prefillGate = deferred();
  const suspensionGate = deferred();
  const context = {
    events,
    performance: { now: () => 1 },
    crypto: { randomUUID: () => "capture-a" },
    chatBusyLeases: new Set(),
    voiceRecording: null,
    voiceCallActive: false,
    voiceCallConversation: null,
    voiceLatencyTrace: { mode: "voice_note" },
    selectedConversation: () => "conversation-a",
    selectedConversationKind: () => "chat",
    startVoiceLatency: () => events.push("latency_start"),
    setVoiceRecordingVisible: () => {},
    markVoiceLatencyIn: (_trace, stage) => events.push(stage),
    errorMessage: (error) => String(error),
    reportError: (error) => { throw error; },
    clearTimeout() {},
    queueMicrotask,
    invoke: async (command) => {
      if (command === "mom_llama_voice_capture_start") {
        events.push("capture_start");
        return { status: "passed" };
      }
      if (command === "mom_llama_voice_capture_cancel") {
        events.push("capture_cancel");
        return { status: "passed" };
      }
      if (command === "mom_llama_voice_audio_begin") {
        events.push("audio_prefill_begin");
        return { session: "session-a" };
      }
      throw new Error(`unexpected command ${command}`);
    },
    recoverPendingVoiceHistory: async () => { events.push("recovery_resumed"); },
    suspendVoiceHistoryRecoveryForForeground: async () => {
      events.push("foreground_wait_started");
      await suspensionGate.promise;
      return (options) => events.push(`foreground_wait_released:${options?.resume}`);
    },
    instantiatePersona: async () => { throw new Error("ordinary chat should not instantiate"); },
    chat: () => ({ dataset: {} }),
    startVoicePrefill: async () => {
      events.push("voice_prefill_requested");
      return prefillGate.promise;
    },
  };
  vm.createContext(context);
  vm.runInContext(`
    let voiceRecording = null;
    const voiceCallActive = false;
    const voiceCallConversation = null;
    const voiceLatencyTrace = { mode: "voice_note" };
    const chatBusyLeases = new Set();
    const recoverPendingVoiceHistory = globalThis.recoverPendingVoiceHistory;
    ${source.slice(prefillStart, prefillEnd)}
    ${source.slice(captureStart, captureEnd)}
    ${source.slice(abortStart, stopStart)}
    ${source.slice(stopStart, stopEnd)}
    this.beginCapture = beginNativeVoiceCapture;
    this.stopCapture = stopVoiceCapture;
    this.getRecording = () => voiceRecording;
  `, context);

  const startTask = context.beginCapture();
  const recording = await startTask;
  assert.equal(context.getRecording(), recording);
  assert.deepEqual(events.slice(0, 3), ["latency_start", "native_capture_requested", "capture_start"]);
  assert.ok(events.includes("foreground_wait_started"), "prefill waits on the active idle ASR after capture admission");
  assert.equal(events.includes("audio_prefill_begin"), false);

  const stopped = await context.stopCapture({ discard: true });
  assert.equal(stopped, null);
  assert.equal(recording.discarded, true, "discard authority is installed before cancellation waits");
  assert.ok(events.includes("capture_cancel"));
  suspensionGate.resolve();
  await recording.prefillSession;
  await recording.targetChain;
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(events.includes("audio_prefill_begin"), false, "discarded capture cannot admit Gemma prefill after stop settles");
  assert.ok(events.includes("foreground_wait_released:false"), "capture teardown does not restart recovery in the dispatch gap");
  assert.ok(events.includes("recovery_resumed"), "recovery resumes after the prefill cancellation settles");
  assert.ok(events.indexOf("foreground_wait_released:false") < events.indexOf("recovery_resumed"));
  prefillGate.resolve(null);
});

test("native capture start failure resumes idle recovery only after pending prefill cancellation", async () => {
  const captureStart = source.indexOf("  const beginNativeVoiceCapture = async");
  const captureEnd = source.indexOf("\n  const beginVoiceCapture =", captureStart);
  const prefillStart = source.indexOf("  const prepareVoicePrefill = async");
  const prefillEnd = source.indexOf("\n  let pendingCallTranscriptJobs", prefillStart);
  const voiceModeStart = source.indexOf("  const voiceModePrompt = (");
  const voiceModeEnd = source.indexOf("\n  const enqueueVoicePrefill", voiceModeStart);
  const startPrefillStart = source.indexOf("  const startVoicePrefill = async", prefillEnd - 1000);
  const abortStart = source.indexOf("  const abortStoppedVoicePrefill = (");
  const stopStart = source.indexOf("  const stopVoiceCapture = async", prefillEnd);
  const events = [];
  const suspensionGate = deferred();
  const captureStartGate = deferred();
  const context = {
    events,
    performance: { now: () => 1 },
    crypto: { randomUUID: () => "capture-failed" },
    chatBusyLeases: new Set(),
    voiceRecording: null,
    voiceCallActive: false,
    voiceCallConversation: null,
    voiceLatencyTrace: { mode: "voice_note" },
    selectedConversation: () => "conversation-a",
    selectedConversationKind: () => "chat",
    startVoiceLatency: () => {},
    setVoiceRecordingVisible: () => {},
    markVoiceLatencyIn: (_trace, stage) => events.push(stage),
    errorMessage: (error) => String(error),
    reportError: (error) => { throw error; },
    clearTimeout() {},
    queueMicrotask,
    invoke: async (command) => {
      if (command === "mom_llama_voice_capture_start") {
        events.push("capture_start");
        return captureStartGate.promise;
      }
      if (command === "mom_llama_voice_audio_begin") {
        events.push("audio_prefill_begin");
        return { session: "session-a" };
      }
      if (command === "mom_llama_voice_audio_abort") {
        events.push("audio_prefill_abort");
        return { status: "passed" };
      }
      throw new Error(`unexpected command ${command}`);
    },
    suspendVoiceHistoryRecoveryForForeground: async () => {
      events.push("foreground_wait_started");
      await suspensionGate.promise;
      return (options) => events.push(`foreground_wait_released:${options?.resume}`);
    },
    recoverPendingVoiceHistory: async () => { events.push("recovery_resumed"); },
    instantiatePersona: async () => { throw new Error("ordinary chat should not instantiate"); },
    chat: () => ({ dataset: {} }),
    selectedConversation: () => "conversation-a",
  };
  vm.createContext(context);
  vm.runInContext(`
    let voiceRecording = null;
    const voiceCallActive = false;
    const voiceCallConversation = null;
    const voiceLatencyTrace = { mode: "voice_note" };
    const chatBusyLeases = new Set();
    const recoverPendingVoiceHistory = globalThis.recoverPendingVoiceHistory;
    ${source.slice(prefillStart, prefillEnd)}
    ${source.slice(voiceModeStart, voiceModeEnd)}
    ${source.slice(startPrefillStart, abortStart)}
    ${source.slice(captureStart, captureEnd)}
    ${source.slice(abortStart, stopStart)}
    this.beginCapture = beginNativeVoiceCapture;
    this.getRecording = () => voiceRecording;
  `, context);

  const startTask = context.beginCapture();
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(events.includes("foreground_wait_started"));
  assert.ok(events.includes("capture_start"));
  captureStartGate.reject(new Error("capture device refused start"));
  await assert.rejects(startTask, /capture device refused start/);
  const recording = context.getRecording();
  assert.equal(recording, null, "failed capture is removed before asynchronous prefill cleanup");
  suspensionGate.resolve();
  await recording?.prefillSession;
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(events.includes("audio_prefill_begin"), false, "failed capture cannot admit model prefill after cancellation");
  assert.ok(events.includes("foreground_wait_released:false"));
  assert.ok(events.includes("recovery_resumed"));
  assert.ok(events.indexOf("foreground_wait_released:false") < events.indexOf("recovery_resumed"));
});

test("failed native prefill abort does not restart idle recovery without stop acknowledgement", async () => {
  const captureStart = source.indexOf("  const beginNativeVoiceCapture = async");
  const captureEnd = source.indexOf("\n  const beginVoiceCapture =", captureStart);
  const prefillStart = source.indexOf("  const prepareVoicePrefill = async");
  const prefillEnd = source.indexOf("\n  let pendingCallTranscriptJobs", prefillStart);
  const voiceModeStart = source.indexOf("  const voiceModePrompt = (");
  const voiceModeEnd = source.indexOf("\n  const enqueueVoicePrefill", voiceModeStart);
  const startPrefillStart = source.indexOf("  const startVoicePrefill = async", prefillEnd - 1000);
  const abortStart = source.indexOf("  const abortStoppedVoicePrefill = (");
  const stopStart = source.indexOf("  const stopVoiceCapture = async", prefillEnd);
  const stopEnd = source.indexOf("\n  const interruptCallPlaybackForSpeech", stopStart);
  const events = [];
  const suspensionGate = deferred();
  const abortGate = deferred();
  const context = {
    events,
    performance: { now: () => 1 },
    crypto: { randomUUID: () => "capture-abort-fails" },
    chatBusyLeases: new Set(),
    voiceRecording: null,
    voiceCallActive: false,
    voiceCallConversation: null,
    voiceLatencyTrace: { mode: "voice_note" },
    selectedConversation: () => "conversation-a",
    selectedConversationKind: () => "chat",
    startVoiceLatency: () => {},
    setVoiceRecordingVisible: () => {},
    markVoiceLatencyIn: (_trace, stage) => events.push(stage),
    errorMessage: (error) => String(error),
    reportError: (error) => { throw error; },
    clearTimeout() {},
    queueMicrotask,
    invoke: async (command) => {
      if (command === "mom_llama_voice_capture_start") return { status: "passed" };
      if (command === "mom_llama_voice_capture_cancel") return { status: "passed" };
      if (command === "mom_llama_voice_audio_begin") return { session: "session-a" };
      if (command === "mom_llama_voice_audio_abort") {
        events.push("audio_prefill_abort");
        return abortGate.promise;
      }
      throw new Error(`unexpected command ${command}`);
    },
    suspendVoiceHistoryRecoveryForForeground: async () => {
      await suspensionGate.promise;
      return (options) => events.push(`foreground_wait_released:${options?.resume}`);
    },
    recoverPendingVoiceHistory: async () => { events.push("recovery_resumed"); },
    instantiatePersona: async () => { throw new Error("ordinary chat should not instantiate"); },
    chat: () => ({ dataset: {} }),
  };
  vm.createContext(context);
  vm.runInContext(`
    let voiceRecording = null;
    const voiceCallActive = false;
    const voiceCallConversation = null;
    const voiceLatencyTrace = { mode: "voice_note" };
    const chatBusyLeases = new Set();
    const recoverPendingVoiceHistory = globalThis.recoverPendingVoiceHistory;
    ${source.slice(prefillStart, prefillEnd)}
    ${source.slice(voiceModeStart, voiceModeEnd)}
    ${source.slice(startPrefillStart, abortStart)}
    ${source.slice(captureStart, captureEnd)}
    ${source.slice(abortStart, stopStart)}
    ${source.slice(stopStart, stopEnd)}
    this.beginCapture = beginNativeVoiceCapture;
    this.stopCapture = stopVoiceCapture;
  `, context);

  const recording = await context.beginCapture();
  suspensionGate.resolve();
  await recording.prefillSession;
  assert.ok(events.includes("audio_prefill_session_ready"), JSON.stringify(events));
  await context.stopCapture({ discard: true });
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(events.includes("audio_prefill_abort"));
  assert.equal(events.includes("recovery_resumed"), false, "recovery must wait for successful prefill-stop acknowledgement");
  abortGate.reject(new Error("native stop failed"));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(events.includes("recovery_resumed"), false, "failed stop acknowledgement keeps background ASR paused");
  assert.ok(events.includes("voice_capture_prefill_abort_failed"), "failed stop is recorded diagnostically");
});
