"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const source = fs.readFileSync(`${__dirname}/coop-hx.js`, "utf8");

test("voice call activity guard waits for activation and clears only its own session", async () => {
  const start = source.indexOf("  const createVoiceCallActivityGuard = (sessionId) => {");
  const end = source.indexOf("\n  const sha256Hex =", start);
  assert.ok(start >= 0 && end > start, "the production activity guard must remain testable");
  const calls = [];
  let activateOld;
  const context = {
    invoke: (command, args) => {
      calls.push([command, args]);
      if (args.sessionId === "old-call" && args.active) {
        return new Promise((resolve) => { activateOld = resolve; });
      }
      return Promise.resolve({ status: "passed" });
    },
  };
  vm.createContext(context);
  vm.runInContext(
    `${source.slice(start, end)}\nthis.createGuard = createVoiceCallActivityGuard;`,
    context,
  );

  const oldCall = context.createGuard("old-call");
  const oldClose = oldCall.clear();
  const nextCall = context.createGuard("new-call");
  await nextCall.ready;
  await Promise.resolve();
  assert.deepEqual(calls.map(([, args]) => [args.sessionId, args.active]), [
    ["old-call", true],
    ["new-call", true],
  ], "closing must wait for its in-flight activation before sending the matching clear");

  activateOld({ status: "passed" });
  assert.equal(await oldClose, true);
  assert.equal(await nextCall.clear(), true);
  assert.deepEqual(calls.map(([, args]) => [args.sessionId, args.active]), [
    ["old-call", true],
    ["new-call", true],
    ["old-call", false],
    ["new-call", false],
  ], "a delayed old close must never clear the newer call's guard");
  assert.ok(calls.every(([command]) => command === "mom_llama_kv_cache_set_voice_call_activity"));
});

test("unacknowledged speech preemption permanently disables call transcript admission", async () => {
  const start = source.indexOf("  const stopSpeechOperationAndWait = async (");
  const end = source.indexOf("\n  const startStreamedCallGreeting", start);
  assert.ok(start >= 0 && end > start, "the production preemption helper must remain testable");
  const definition = source.slice(start, end);

  const makeContext = (response, invokeError = null) => {
    const context = {
      voiceCallTranscriptSlotsDisabled: false,
      performance: { now: (() => { let now = 0; return () => ++now; })() },
      markVoiceLatency() {},
      invoke: async () => {
        if (invokeError) throw invokeError;
        return response;
      },
    };
    vm.createContext(context);
    vm.runInContext(`${definition}\nthis.stop = stopSpeechOperationAndWait;`, context);
    return context;
  };

  const acknowledged = makeContext({ result: { terminal_acknowledged: true } });
  await acknowledged.stop("operation", "test");
  assert.equal(acknowledged.voiceCallTranscriptSlotsDisabled, false);

  for (const [response, invokeError] of [
    [{ status: "blocked" }, null],
    [{ result: { terminal_acknowledged: false } }, null],
    [null, new Error("transport failed")],
  ]) {
    const unacknowledged = makeContext(response, invokeError);
    await assert.rejects(unacknowledged.stop("operation", "test"));
    assert.equal(unacknowledged.voiceCallTranscriptSlotsDisabled, true);
  }
});

test("call persona preparation is awaited, epoch-bound, and required before greeting dispatch", async () => {
  const helperStart = source.indexOf("  const prepareVoicePersonaForCall = async");
  const helperEnd = source.indexOf("\n  const startStreamedCallGreeting", helperStart);
  assert.ok(helperStart >= 0 && helperEnd > helperStart, "the production preparation helper must remain testable");

  const epoch = {};
  const calls = [];
  const stages = [];
  let now = 0;
  const context = {
    voiceCallEpoch: epoch,
    voiceCallActive: true,
    performance: { now: () => ++now },
    errorMessage: (error) => error.message,
    markVoiceLatency: (stage, detail) => stages.push([stage, detail]),
    invoke: async (command, args) => {
      calls.push([command, args]);
      return {
        status: "prompt_smoke_verified",
        result: {
          audio_projector_warmed: true,
          metadata: { id: "prepared-prefix" },
          prefill_us: 17,
          verification_us: 29,
          persisted: false,
        },
      };
    },
  };
  vm.createContext(context);
  vm.runInContext(
    `${source.slice(helperStart, helperEnd)}\nthis.prepareVoicePersonaForCall = prepareVoicePersonaForCall;`,
    context,
  );
  assert.equal(await context.prepareVoicePersonaForCall("contact-a", epoch), true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], "mom_llama_voice_persona_prepare");
  assert.equal(calls[0][1].personaId, "contact-a");
  assert.deepEqual(stages.map(([stage]) => stage), [
    "call_persona_prepare_started",
    "call_persona_prepare_completed",
  ]);
  assert.equal(stages[1][1].prefix_cache_id, "prepared-prefix");

  const blockedStages = [];
  const blockedContext = {
    voiceCallEpoch: epoch,
    voiceCallActive: true,
    performance: { now: () => 1 },
    errorMessage: (error) => error.message,
    markVoiceLatency: (stage) => blockedStages.push(stage),
    invoke: async () => ({ status: "blocked", blocker: { message: "persona unavailable" } }),
  };
  vm.createContext(blockedContext);
  vm.runInContext(
    `${source.slice(helperStart, helperEnd)}\nthis.prepareVoicePersonaForCall = prepareVoicePersonaForCall;`,
    blockedContext,
  );
  await assert.rejects(blockedContext.prepareVoicePersonaForCall("contact-a", epoch), /persona unavailable/);
  assert.deepEqual(blockedStages, ["call_persona_prepare_started", "call_persona_prepare_failed"]);

  const coldEncoderStages = [];
  const coldEncoderContext = {
    voiceCallEpoch: epoch,
    voiceCallActive: true,
    performance: { now: () => 1 },
    errorMessage: (error) => error.message,
    markVoiceLatency: (stage) => coldEncoderStages.push(stage),
    invoke: async () => ({
      status: "resident_prefix_prepared",
      result: { audio_projector_warmed: false, resident_prefix_tokens: 76 },
    }),
  };
  vm.createContext(coldEncoderContext);
  vm.runInContext(
    `${source.slice(helperStart, helperEnd)}\nthis.prepareVoicePersonaForCall = prepareVoicePersonaForCall;`,
    coldEncoderContext,
  );
  await assert.rejects(
    coldEncoderContext.prepareVoicePersonaForCall("chat-a", epoch),
    /could not be prepared for voice/,
  );
  assert.deepEqual(coldEncoderStages, ["call_persona_prepare_started", "call_persona_prepare_failed"]);

  let resolvePreparation;
  const staleStages = [];
  const staleEpoch = {};
  const staleContext = {
    voiceCallEpoch: staleEpoch,
    voiceCallActive: true,
    performance: { now: () => 1 },
    errorMessage: (error) => error.message,
    markVoiceLatency: (stage) => staleStages.push(stage),
    invoke: () => new Promise((resolve) => { resolvePreparation = resolve; }),
  };
  vm.createContext(staleContext);
  vm.runInContext(
    `${source.slice(helperStart, helperEnd)}\nthis.prepareVoicePersonaForCall = prepareVoicePersonaForCall;`,
    staleContext,
  );
  const pending = staleContext.prepareVoicePersonaForCall("contact-a", staleEpoch);
  await Promise.resolve();
  staleContext.voiceCallEpoch = {};
  resolvePreparation({ status: "prompt_smoke_verified", result: { metadata: { id: "stale-prefix" } } });
  assert.equal(await pending, false);
  assert.deepEqual(staleStages, ["call_persona_prepare_started"]);

  const openStart = source.indexOf("  const openVoiceCallSurface = async");
  const openEnd = source.indexOf("\n  const closeVoiceCallSurface = async", openStart);
  const open = source.slice(openStart, openEnd);
  assert.ok(open.indexOf("const recipientId = recipient.dataset.recipientId") < open.indexOf("await prepareVoicePersonaForCall"));
  assert.ok(open.indexOf("await warmReady") < open.indexOf("await prepareVoicePersonaForCall"));
  assert.ok(!/if\s*\(warm\.status/.test(open), "a failed opportunistic startup warm must not reject a call before exact conversation preparation");
  assert.ok(open.indexOf("await prepareVoicePersonaForCall") < open.indexOf("await startStreamedCallGreeting"));
});

test("failed startup warm still admits a call only after the exact conversation encoder is ready", async () => {
  const openStart = source.indexOf("  const openVoiceCallSurface = async");
  const warmStart = source.indexOf("      await warmReady;", openStart);
  const catchStart = source.indexOf("\n    } catch (error)", warmStart);
  const helperStart = source.indexOf("  const prepareVoicePersonaForCall = async");
  const helperEnd = source.indexOf("\n  const startStreamedCallGreeting", helperStart);
  assert.ok(openStart >= 0 && warmStart > openStart && catchStart > warmStart);
  assert.ok(helperStart >= 0 && helperEnd > helperStart);
  const admission = source.slice(warmStart, catchStart);

  const makeContext = (encoderWarmed, warmReady = Promise.resolve({
    status: "blocked",
    message: "startup used a different profile",
  }), prepareResponse = null) => {
    // The caller has already opened the surface and started its ring before
    // this test enters the later model-admission portion of the handler.
    const stages = [];
    const calls = [];
    const greetings = [];
    const epoch = {};
    const context = {
      name: "Jan",
      modal: { dataset: { callState: "ringing" }, setAttribute() {} },
      voiceCallEpoch: epoch,
      voiceCallActive: true,
      voiceCallConversation: null,
      voiceLatencyTrace: { mode: "call_greeting" },
      warmReady,
      ringingReady: Promise.resolve({ started: true }),
      performance: { now: () => 1 },
      errorMessage: (error) => error.message,
      markVoiceLatency: (stage) => stages.push(stage),
      setText() {},
      selectedConversation: () => "chat-frozen-profile",
      selectedConversationKind: () => "chat",
      startStreamedCallGreeting: async (...args) => greetings.push(args),
      invoke: async (command, args) => {
        calls.push([command, args]);
        if (prepareResponse) return prepareResponse;
        return {
          status: "resident_prefix_prepared",
          result: { audio_projector_warmed: encoderWarmed, resident_prefix_tokens: 76 },
        };
      },
    };
    vm.createContext(context);
    vm.runInContext(
      `${source.slice(helperStart, helperEnd)}\nconst epoch = voiceCallEpoch;\nasync function runAdmission() {\n${admission}\n}\nthis.runAdmission = runAdmission;`,
      context,
    );
    return { context, calls, greetings, stages, epoch };
  };

  const ready = makeContext(true);
  await ready.context.runAdmission();
  assert.equal(ready.calls.length, 1);
  assert.equal(ready.calls[0][0], "mom_llama_voice_persona_prepare");
  assert.equal(ready.calls[0][1].personaId, "chat-frozen-profile");
  assert.equal(ready.context.modal.dataset.callState, "ringing");
  assert.deepEqual(ready.greetings, [[ready.epoch, "chat-frozen-profile"]]);
  assert.deepEqual(ready.stages, [
    "call_conversation_ready",
    "call_persona_prepare_started",
    "call_persona_prepare_completed",
  ]);

  const cold = makeContext(false);
  await assert.rejects(cold.context.runAdmission(), /could not be prepared for voice/);
  assert.equal(cold.calls.length, 1);
  assert.deepEqual(cold.greetings, [], "a ready startup result cannot bypass the exact conversation encoder check");

  let settleWarm;
  const stale = makeContext(true, new Promise((resolve) => { settleWarm = resolve; }));
  const pendingAdmission = stale.context.runAdmission();
  await Promise.resolve();
  stale.context.voiceCallEpoch = {};
  settleWarm({ status: "blocked", message: "startup was superseded" });
  await pendingAdmission;
  assert.equal(stale.calls.length, 0, "a superseded call must not prepare a model");
  assert.deepEqual(stale.greetings, [], "a superseded call must not greet");

  const rejectedWarm = makeContext(true, Promise.reject(new Error("startup warm failed")));
  await assert.rejects(rejectedWarm.context.runAdmission(), /startup warm failed/);
  assert.equal(rejectedWarm.calls.length, 0);
  assert.deepEqual(rejectedWarm.greetings, []);

  let settlePreparation;
  const latePreparation = makeContext(
    true,
    Promise.resolve({ status: "ready" }),
    new Promise((resolve) => { settlePreparation = resolve; }),
  );
  const lateAdmission = latePreparation.context.runAdmission();
  for (let attempts = 0; attempts < 8 && latePreparation.calls.length === 0; attempts += 1) {
    await Promise.resolve();
  }
  assert.equal(latePreparation.calls.length, 1, "the exact-profile preparation is in flight");
  latePreparation.context.voiceCallEpoch = {};
  latePreparation.context.voiceCallActive = false;
  settlePreparation({
    status: "resident_prefix_prepared",
    result: { audio_projector_warmed: true, resident_prefix_tokens: 76 },
  });
  await lateAdmission;
  assert.deepEqual(latePreparation.greetings, [], "hangup during exact preparation must not greet later");
});

test("the real call-open prefix shows Calling before the first ring tone paint boundary", async () => {
  const openStart = source.indexOf("  const openVoiceCallSurface = async () => {");
  const readyBoundary = source.indexOf("      await activityGuard.ready;", openStart);
  assert.ok(openStart >= 0 && readyBoundary > openStart);
  const prefix = source.slice(openStart, readyBoundary);
  let resolveRing;
  const animationFrames = [];
  const stages = [];
  const calls = [];
  const recipient = {
    dataset: { recipientId: "contact-live" },
    querySelector: (selector) => selector.startsWith(":scope") ? { textContent: "Jan" } : null,
  };
  const modal = {
    dataset: {},
    attributes: {},
    querySelector: (selector) => selector.includes("voice-call-close") ? { focus() { calls.push("focused"); } } : null,
    setAttribute(name, value) { this.attributes[name] = value; },
  };
  const context = {
    document: {
      getElementById: (id) => id === "voice-call-modal" ? modal : null,
      querySelector: (selector) => selector === ".recipients-header .recipient-chip" ? recipient : null,
    },
    voiceCallActive: false,
    voiceCallPreparing: false,
    voiceCallEpoch: null,
    voiceLatencyTrace: { mode: "call_greeting", stages: [] },
    conversationNavigationRevision: 0,
    voiceCallConversation: null,
    voiceCallActivityGuard: null,
    voiceLatencyTrace: { mode: "call_greeting", stages: [] },
    createVoiceCallActivityGuard: () => {
      calls.push("guard-created");
      return { ready: new Promise(() => {}), clear: async () => {} };
    },
    speechOperationToken: () => "session-live",
    reportError: (error) => { throw error; },
    clearPersistentVoiceCallFailure() {},
    startVoiceLatency() {},
    markVoiceLatency: (stage) => stages.push(stage),
    markVoiceLatencyIn() {},
    setText: (id, value) => { if (id === "voice-call-title") modal.title = value; },
    setModalVisibility: (_id, visible) => { modal.visible = visible; },
    startCallRinging: (epoch) => {
      // Model the production helper: resume is requested synchronously, while
      // oscillator scheduling waits until the second animation frame.
      calls.push(["resume-requested", epoch]);
      return (async () => {
        await new Promise((resolve) => animationFrames.push(resolve));
        await new Promise((resolve) => animationFrames.push(resolve));
        calls.push("paint-opportunity");
        calls.push(["ring-start", epoch]);
        return new Promise((resolve) => { resolveRing = resolve; });
      })();
    },
    requestAnimationFrame: (callback) => { animationFrames.push(callback); return animationFrames.length; },
    performance: { now: () => 1 },
    window: {},
  };
  context.globalThis = context;
  vm.createContext(context);
  vm.runInContext(
    `${prefix}\n      return { epoch, ringingReady };\n    } catch (error) { throw error; }\n  };\nthis.openCall = openVoiceCallSurface;`,
    context,
  );
  const opening = context.openCall();
  await Promise.resolve();
  const opened = await opening;
  assert.equal(modal.visible, true);
  assert.equal(modal.dataset.callState, "ringing");
  assert.equal(modal.attributes["aria-label"], "Calling Jan");
  assert.equal(modal.title, "Calling");
  assert.ok(calls.includes("guard-created"));
  assert.equal(calls.filter((entry) => Array.isArray(entry) && entry[0] === "resume-requested").length, 1);
  assert.equal(calls.filter((entry) => Array.isArray(entry) && entry[0] === "ring-start").length, 0,
    "resume may start on the gesture, but no ring tone is scheduled before paint");
  assert.equal(animationFrames.length, 1);
  animationFrames.shift()();
  await Promise.resolve();
  assert.equal(animationFrames.length, 1);
  calls.push("browser-paint");
  animationFrames.shift()();
  await Promise.resolve();
  assert.equal(calls.indexOf("browser-paint") < calls.findIndex((entry) => Array.isArray(entry) && entry[0] === "ring-start"), true);
  assert.equal(typeof resolveRing, "function", "ring starts only after the paint opportunity");

  resolveRing(true);
  const ringResult = await opened.ringingReady;
  assert.equal(ringResult.started, true);
  assert.deepEqual(stages, ["call_activity_guard_wait_started", "call_ringing_started"],
    "the ring and setup-wait boundary are recorded before later preparation");
  assert.equal(opened.epoch, calls.find((entry) => Array.isArray(entry) && entry[0] === "ring-start")[1]);
});

test("call startup failure persists in the status surface and checkpoints only safe diagnostics", async () => {
  const statusHelperStart = source.indexOf("  const renderStatusContent = (status, label, onDismiss) => {");
  const statusHelperEnd = source.indexOf("\n  const report = (value) =>", statusHelperStart);
  const statusHelper = source.slice(statusHelperStart, statusHelperEnd);
  const reportStart = source.indexOf("  const report = (value) =>");
  const reportEnd = source.indexOf("\n  const errorMessage =", reportStart);
  const report = source.slice(reportStart, reportEnd);
  const errorMessageStart = reportEnd + 1;
  const safeHelpersStart = source.indexOf("  let persistentVoiceCallFailure = null;", errorMessageStart);
  const safeHelpersEnd = source.indexOf("\n  const visiblyReportedErrors", safeHelpersStart);
  const safeHelpers = source.slice(safeHelpersStart, safeHelpersEnd);
  const callStart = source.indexOf("  const openVoiceCallSurface = async () => {");
  const callEnd = source.indexOf("\n  const closeVoiceCallSurface = async", callStart);
  const call = source.slice(callStart, callEnd);
  const statusClasses = new Set(["is-hidden"]);
  const status = {
    hideTimer: 9,
    children: [],
    replaceChildren(...children) { this.children = children; },
    classList: {
      add: (name) => statusClasses.add(name),
      remove: (name) => statusClasses.delete(name),
      toggle: (name, enabled) => enabled ? statusClasses.add(name) : statusClasses.delete(name),
    },
  };
  let dismiss;
  const modal = {
    dataset: {},
    setAttribute() {},
    querySelector: () => null,
  };
  const recipient = {
    dataset: { recipientId: "persona-safe-id" },
    querySelector: () => ({ textContent: "Contact" }),
  };
  const trace = { stages: [] };
  const context = {
    document: {
      getElementById: (id) => id === "voice-call-modal" ? modal : (id === "command-status" ? status : null),
      querySelector: () => recipient,
      createElement() {
        return {
          className: "", textContent: "", type: "",
          setAttribute() {},
          addEventListener(name, callback) { if (name === "click") dismiss = callback; },
        };
      },
    },
    window: { clearTimeout() {}, setTimeout: () => 10 },
    recordCommandResult() {},
    errorMessage: (error) => error.message,
    setText() {},
    setModalVisibility() {},
    createVoiceCallActivityGuard: () => ({
      ready: Promise.reject(Object.assign(new Error("Microphone access was denied."), { code: "permission_denied" })),
      clear: async () => {},
    }),
    startVoiceLatency(mode, stage) { trace.mode = mode; trace.stages.push({ stage }); },
    markVoiceLatency(stage, detail = {}) { trace.stages.push({ stage, ...detail }); },
    voiceLatencyTrace: trace,
    markVoiceLatencyIn(target, stage, detail = {}) { target.stages.push({ stage, ...detail }); },
    performance: { now: () => 1 },
    speechOperationToken: () => "safe-operation-id",
    startCallRinging: async () => false,
    closeVoiceCallSurface: async () => {
      context.voiceCallActive = false;
      context.voiceCallEpoch = null;
      context.voiceCallPreparing = false;
      context.report(null);
    },
    reportError(error) {
      context.report({ status: "blocked", blocker: { message: error.message } });
    },
    voiceCallActive: false,
    voiceCallPreparing: false,
    voiceCallEpoch: null,
    voiceCallActivityGuard: null,
    voiceCallConversation: null,
    conversationNavigationRevision: 0,
    runtimeWarmPromise: Promise.resolve({ ready: true }),
    voiceCallMicrophoneStream: null,
    voiceCallCaptureGraph: null,
    voiceCallAudio: null,
    retryCallPrefillAfterTranscript: null,
    voiceReplyConversation: null,
    callReplyConversation: null,
    activeDispatchConversation: null,
    liveVoiceTts: null,
    activeCallPlayback: null,
    voiceCallTranscriptSlotsDisabled: false,
  };
  vm.createContext(context);
  vm.runInContext(
    `${statusHelper}\n${report}\nthis.report = report;\nconst errorMessage = (error) => error?.message || String(error);\n${safeHelpers}\n${call}\nthis.openCall = openVoiceCallSurface;`,
    context,
  );

  await context.openCall();
  const failure = trace.stages.find(({ stage }) => stage === "call_start_failed");
  assert.deepEqual(failure, {
    stage: "call_start_failed", phase: "activity_guard", error_code: "permission_denied",
  });
  assert.doesNotMatch(JSON.stringify(failure), /Microphone access|transcript|prompt|persona-safe-id/);
  assert.equal(statusClasses.has("is-hidden"), false);
  assert.equal(status.children[0].textContent, "Microphone access was denied.");
  assert.equal(status.hideTimer, undefined);

  context.report(null);
  context.report({ status: "passed", result: { message: "cleanup complete" } });
  assert.equal(status.children[0].textContent, "Microphone access was denied.", "cleanup reports cannot hide the call failure");
  dismiss();
  assert.equal(statusClasses.has("is-hidden"), true, "the explicit dismiss control clears the failure");

  context.report({ status: "blocked", blocker: { message: "ordinary failure" } });
  assert.equal(status.children[0].textContent, "ordinary failure");
  dismiss();
  assert.equal(statusClasses.has("is-hidden"), true, "ordinary status dismissal follows its own report(null) route");
  assert.ok(source.includes('"call_requested", "call_start_failed", "call_ringing_started"'));
});

test("a late ring-context resume after hangup closes without installing a timer", async () => {
  const start = source.indexOf("  const stopCallBed = async () => {");
  const end = source.indexOf("\n  const prepareVoicePersonaForCall", start);
  assert.ok(start >= 0 && end > start, "the production ring helpers must remain directly testable");
  let resolveResume;
  const animationFrames = [];
  let constructedContext;
  let intervals = 0;
  let closes = 0;
  const epoch = {};
  const context = {
    voiceCallEpoch: epoch,
    voiceCallActive: true,
    voiceCallAudio: null,
    window: { clearInterval() {}, setInterval() { intervals += 1; return intervals; } },
    requestAnimationFrame: (callback) => { animationFrames.push(callback); return animationFrames.length; },
    AudioContext: function AudioContextMock() {
      constructedContext = {
        resume: () => new Promise((resolve) => { resolveResume = resolve; }),
        close: async () => { closes += 1; },
      };
      return constructedContext;
    },
  };
  vm.createContext(context);
  vm.runInContext(
    `${source.slice(start, end)}\nthis.startRing = startCallRinging;`,
    context,
  );
  const starting = context.startRing(epoch);
  for (let attempts = 0; attempts < 4 && !resolveResume; attempts += 1) await Promise.resolve();
  assert.equal(typeof resolveResume, "function", "the audio context resume is pending");
  assert.equal(intervals, 0, "ring tones wait for the visible call paint");
  assert.equal(animationFrames.length, 1);
  animationFrames.shift()();
  assert.equal(animationFrames.length, 1, "a second animation frame provides the paint opportunity");
  animationFrames.shift()();
  context.voiceCallActive = false;
  context.voiceCallEpoch = null;
  resolveResume();
  assert.equal(await starting, false);
  assert.equal(closes, 1, "the stale resumed context is closed");
  assert.equal(intervals, 0, "hangup cannot install a late ring timer");
  assert.equal(context.voiceCallAudio, null);
});

test("late microphone permission after hangup cannot replace or release a newer call's stream", async () => {
  const start = source.indexOf("  const prepareCallMicrophoneStream = async (epoch) => {");
  const end = source.indexOf("\n  const openVoiceCallSurface = async", start);
  assert.ok(start >= 0 && end > start);
  const oldEpoch = {};
  const newEpoch = {};
  let resolvePermission;
  const oldTrack = { stopped: false, stop() { this.stopped = true; } };
  const oldStream = {
    active: true,
    getTracks: () => [oldTrack],
  };
  const newTrack = { readyState: "live", stop() {} };
  const newStream = { active: true, getAudioTracks: () => [newTrack], getTracks: () => [newTrack] };
  const context = {
    voiceCallActive: true,
    voiceCallEpoch: oldEpoch,
    voiceCallMicrophoneStream: null,
    voiceCallMicrophoneEpoch: null,
    voiceCallCaptureGraph: null,
    performance: { now: () => 1 },
    navigator: { mediaDevices: { getUserMedia: () => new Promise((resolve) => { resolvePermission = resolve; }) } },
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.prepareMicrophone = prepareCallMicrophoneStream;`, context);
  const pending = context.prepareMicrophone(oldEpoch);
  await Promise.resolve();
  context.voiceCallEpoch = newEpoch;
  context.voiceCallMicrophoneStream = newStream;
  context.voiceCallMicrophoneEpoch = newEpoch;
  resolvePermission(oldStream);
  await assert.rejects(pending, /call was closed during microphone setup/);
  assert.equal(oldTrack.stopped, true, "stale permission stops its own stream");
  assert.equal(context.voiceCallMicrophoneStream, newStream, "the late result cannot replace the new owner");
  assert.equal(context.voiceCallMicrophoneEpoch, newEpoch);
});

test("microphone graph and resume failures release the exact stream before propagating", async () => {
  const start = source.indexOf("  const prepareCallMicrophoneStream = async (epoch) => {");
  const end = source.indexOf("\n  const openVoiceCallSurface =", start);
  assert.ok(start >= 0 && end > start);
  const epoch = {};
  const track = { readyState: "live", stopped: false, stop() { this.stopped = true; }, getSettings: () => ({}) };
  const stream = { active: true, getAudioTracks: () => [track], getTracks: () => [track] };
  let closed = 0;
  const node = () => ({ connect() {}, disconnect() {}, gain: { value: 1 } });
  class FailingAudioContext {
    constructor() { this.state = "running"; this.sampleRate = 48_000; this.destination = {}; }
    createMediaStreamSource() { return node(); }
    createScriptProcessor() { return Object.assign(node(), { onaudioprocess: null }); }
    createGain() { return node(); }
    resume() { return Promise.reject(new Error("audio route unavailable")); }
    close() { closed += 1; this.state = "closed"; return Promise.resolve(); }
  }
  const context = {
    voiceCallActive: true,
    voiceCallEpoch: epoch,
    voiceCallMicrophoneStream: null,
    voiceCallMicrophoneEpoch: null,
    voiceCallCaptureGraph: null,
    performance: { now: () => 1 },
    navigator: { mediaDevices: { getUserMedia: async () => stream } },
    AudioContext: FailingAudioContext,
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.prepareMicrophone = prepareCallMicrophoneStream;`, context);
  await assert.rejects(context.prepareMicrophone(epoch), /audio route unavailable/);
  assert.equal(track.stopped, true);
  assert.equal(closed, 1);
  assert.equal(context.voiceCallMicrophoneStream, null);
  assert.equal(context.voiceCallMicrophoneEpoch, null);
  assert.equal(context.voiceCallCaptureGraph, null);
});

test("microphone graph construction failure stops its stream and closes its partial context", async () => {
  const start = source.indexOf("  const prepareCallMicrophoneStream = async (epoch) => {");
  const end = source.indexOf("\n  const openVoiceCallSurface =", start);
  const epoch = {};
  const track = { readyState: "live", stopped: false, stop() { this.stopped = true; } };
  const stream = { active: true, getAudioTracks: () => [track], getTracks: () => [track] };
  let closed = 0;
  class FailingAudioContext {
    constructor() { this.state = "running"; this.destination = {}; }
    createMediaStreamSource() { throw new Error("graph creation failed"); }
    close() { closed += 1; return Promise.resolve(); }
  }
  const context = {
    voiceCallActive: true,
    voiceCallEpoch: epoch,
    voiceCallMicrophoneStream: null,
    voiceCallMicrophoneEpoch: null,
    voiceCallCaptureGraph: null,
    performance: { now: () => 1 },
    navigator: { mediaDevices: { getUserMedia: async () => stream } },
    AudioContext: FailingAudioContext,
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.prepareMicrophone = prepareCallMicrophoneStream;`, context);
  await assert.rejects(context.prepareMicrophone(epoch), /graph creation failed/);
  assert.equal(track.stopped, true);
  assert.equal(closed, 1);
  assert.equal(context.voiceCallMicrophoneStream, null);
  assert.equal(context.voiceCallMicrophoneEpoch, null);
});

test("a late greeting completion cannot clear a newer same-conversation dispatch", async () => {
  const start = source.indexOf("  const startStreamedCallGreeting = async");
  const end = source.indexOf("\n  const prepareCallMicrophoneStream =", start);
  assert.ok(start >= 0 && end > start);
  const epoch = {};
  let resolveGeneration;
  const newerOwner = { mode: "call" };
  const trace = {};
  const greetingTts = {
    mode: "call",
    clientRequest: "old-request",
    lastProgressAt: 0,
    text: "",
    trackGeneration: (promise) => promise,
    finish: async () => true,
  };
  const context = {
    voiceCallEpoch: epoch,
    voiceCallActive: true,
    voiceLatencyTrace: trace,
    liveVoiceTts: null,
    activeDispatchConversation: null,
    activeDispatchOwner: null,
    callReplyConversation: null,
    speechOperationToken: () => "old-operation",
    beginLiveVoiceTts: () => greetingTts,
    markVoiceLatencyIn() {},
    markVoiceLatency() {},
    withProgressDeadline: (operation) => operation,
    invoke: (command) => {
      assert.equal(command, "mom_llama_chat_send");
      return new Promise((resolve) => { resolveGeneration = resolve; });
    },
    stopCallBed: async () => {},
    setText() {},
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.startGreeting = startStreamedCallGreeting;`, context);
  const pending = context.startGreeting(epoch, "same-conversation");
  await Promise.resolve();
  context.liveVoiceTts = newerOwner;
  context.activeDispatchOwner = newerOwner;
  context.activeDispatchConversation = "same-conversation";
  context.callReplyConversation = "same-conversation";
  resolveGeneration({ result: { assistant_text: "A greeting" } });
  assert.equal(await pending, false);
  assert.equal(context.activeDispatchConversation, "same-conversation");
  assert.equal(context.callReplyConversation, "same-conversation");
  assert.equal(context.liveVoiceTts, newerOwner);
});

test("a short greeting releases its dispatch when playback ends before generation settles", async () => {
  const start = source.indexOf("  const startStreamedCallGreeting = async");
  const end = source.indexOf("\n  const prepareCallMicrophoneStream =", start);
  assert.ok(start >= 0 && end > start);
  const epoch = {};
  let resolveGeneration;
  const trace = {};
  const greetingTts = {
    mode: "call",
    clientRequest: "short-request",
    lastProgressAt: 0,
    text: "A short greeting",
    trackGeneration: (promise) => promise,
    // The final packet can end playback immediately while the native result
    // is still completing. The stable dispatch owner must survive this.
    finish: async () => {
      context.liveVoiceTts = null;
      return true;
    },
  };
  const context = {
    voiceCallEpoch: epoch,
    voiceCallActive: true,
    voiceLatencyTrace: trace,
    liveVoiceTts: null,
    activeDispatchConversation: null,
    activeDispatchOwner: null,
    callReplyConversation: null,
    speechOperationToken: () => "short-operation",
    beginLiveVoiceTts: () => greetingTts,
    markVoiceLatencyIn() {},
    markVoiceLatency() {},
    withProgressDeadline: (operation) => operation,
    invoke: () => new Promise((resolve) => { resolveGeneration = resolve; }),
    stopCallBed: async () => {},
    setText() {},
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.startGreeting = startStreamedCallGreeting;`, context);
  const pending = context.startGreeting(epoch, "same-conversation");
  await Promise.resolve();
  resolveGeneration({ result: { assistant_text: "A short greeting" } });
  assert.equal(await pending, true);
  assert.equal(context.liveVoiceTts, null);
  assert.equal(context.activeDispatchConversation, null);
  assert.equal(context.activeDispatchOwner, null);
  assert.equal(context.callReplyConversation, null);
});

test("a late speculation abort cannot clear a replacement TTS dispatch in the same conversation", async () => {
  const start = source.indexOf("  const abortCallSpeculation = async");
  const end = source.indexOf("\n  const sealedVoiceRecordingBytes", start);
  assert.ok(start >= 0 && end > start);
  let resolveAbort;
  const conversation = "same-chat";
  const oldTts = { stop: async () => {} };
  const replacementTts = {};
  const speculation = { id: "old-spec", conversation, tts: oldTts, confirmed: false, aborting: false };
  const replacementSpeculation = { id: "new-spec" };
  const recording = { speculation, trace: {}, native: true, call: true };
  const context = {
    performance: { now: () => 1 },
    markVoiceLatencyIn() {},
    voiceModePrompt: () => "",
    invoke: async () => new Promise((resolve) => { resolveAbort = resolve; }),
    recording,
    activeVoiceSpeculation: speculation,
    liveVoiceTts: oldTts,
    activeDispatchConversation: conversation,
    activeDispatchOwner: oldTts,
    callReplyConversation: conversation,
    voiceReplyConversation: null,
    voiceRecording: recording,
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.abortSpeculation = abortCallSpeculation;`, context);
  const aborting = context.abortSpeculation(recording);
  await Promise.resolve();
  assert.equal(typeof resolveAbort, "function");
  context.recording.speculation = replacementSpeculation;
  context.activeVoiceSpeculation = replacementSpeculation;
  context.liveVoiceTts = replacementTts;
  context.activeDispatchOwner = replacementTts;
  context.activeDispatchConversation = conversation;
  context.callReplyConversation = conversation;
  resolveAbort({ status: "passed" });
  await aborting;
  assert.equal(context.recording.speculation, replacementSpeculation);
  assert.equal(context.activeVoiceSpeculation, replacementSpeculation);
  assert.equal(context.liveVoiceTts, replacementTts);
  assert.equal(context.activeDispatchConversation, conversation);
  assert.equal(context.callReplyConversation, conversation);
  assert.equal(context.activeDispatchOwner, replacementTts);
});

test("hangup dismisses and releases startup ownership before slow stops, without touching a newer call", async () => {
  const start = source.indexOf("  const closeVoiceCallSurface = async () => {");
  const end = source.indexOf("\n  const setText =", start);
  assert.ok(start >= 0 && end > start);
  let resolveStops;
  let released = null;
  let visible = true;
  const oldEpoch = {};
  const oldStream = { id: "old" };
  const newEpoch = {};
  const newStream = { id: "new" };
  const context = {
    voiceCallActive: true,
    voiceCallEpoch: oldEpoch,
    voiceCallPreparing: true,
    voiceCallConversation: null,
    voiceCallMicrophoneStream: oldStream,
    voiceCallActivityGuard: null,
    liveVoiceTts: null,
    activeCallPlayback: null,
    activeDispatchConversation: "unrelated-note-chat",
    callReplyConversation: null,
    retryCallPrefillAfterTranscript: null,
    voiceReplyConversation: null,
    setModalVisibility(_id, shown) { visible = shown; },
    stopVoiceCapture: () => new Promise((resolve) => { resolveStops = resolve; }),
    stopCallTranscriptForClose: () => Promise.resolve(),
    stopSpeechPlayback: () => Promise.resolve(),
    stopCallBed: () => Promise.resolve(),
    releaseCallMicrophoneStream(stream) { released = stream; },
    recoverPendingVoiceHistory: () => Promise.resolve(),
    scheduleVoiceHistoryRecovery() {},
    invoke: () => Promise.resolve(),
    console: { warn() {} },
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.closeCall = closeVoiceCallSurface;`, context);
  const closing = context.closeCall();
  assert.equal(visible, false, "hangup hides the call immediately");
  assert.equal(context.voiceCallActive, false);
  assert.equal(context.voiceCallPreparing, false, "startup UI ownership clears before any stop settles");
  assert.equal(context.voiceCallEpoch, null);

  context.voiceCallActive = true;
  context.voiceCallPreparing = true;
  context.voiceCallEpoch = newEpoch;
  context.voiceCallMicrophoneStream = newStream;
  resolveStops();
  await closing;
  assert.equal(released, oldStream, "cleanup releases only the microphone owned by the closed call");
  assert.equal(context.voiceCallActive, true);
  assert.equal(context.voiceCallPreparing, true, "late cleanup leaves the newer call's flags alone");
  assert.equal(context.voiceCallEpoch, newEpoch);
  assert.equal(context.voiceCallMicrophoneStream, newStream);
  assert.equal(context.activeDispatchConversation, "unrelated-note-chat",
    "hangup does not clear an unrelated voice-note dispatch");
});

test("hangup clears its held greeting dispatch before waiting and preserves a newer same-chat owner", async () => {
  const start = source.indexOf("  const closeVoiceCallSurface = async () => {");
  const end = source.indexOf("\n  const setText =", start);
  assert.ok(start >= 0 && end > start);
  let resolveGreetingStop;
  const conversation = "same-chat";
  const oldOwner = { mode: "call", stop: () => new Promise((resolve) => { resolveGreetingStop = resolve; }) };
  const newerOwner = { mode: "call" };
  const context = {
    voiceCallActive: true,
    voiceCallEpoch: {},
    voiceCallPreparing: false,
    voiceCallConversation: conversation,
    voiceCallMicrophoneStream: null,
    voiceCallActivityGuard: null,
    liveVoiceTts: oldOwner,
    activeCallPlayback: null,
    activeDispatchConversation: conversation,
    activeDispatchOwner: oldOwner,
    callReplyConversation: conversation,
    retryCallPrefillAfterTranscript: null,
    voiceReplyConversation: null,
    setModalVisibility() {},
    stopVoiceCapture: async () => {},
    stopCallTranscriptForClose: async () => {},
    stopSpeechPlayback: async () => {},
    stopCallBed: async () => {},
    releaseCallMicrophoneStream() {},
    recoverPendingVoiceHistory: async () => {},
    scheduleVoiceHistoryRecovery() {},
    invoke: async () => ({}),
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.closeCall = closeVoiceCallSurface;`, context);
  const closing = context.closeCall();
  assert.equal(context.activeDispatchConversation, null,
    "hangup releases the greeting's dispatch slot synchronously");
  assert.equal(context.activeDispatchOwner, null);
  context.voiceCallActive = true;
  context.voiceCallEpoch = {};
  context.voiceCallPreparing = true;
  context.voiceCallConversation = conversation;
  context.liveVoiceTts = newerOwner;
  context.activeDispatchConversation = conversation;
  context.activeDispatchOwner = newerOwner;
  context.callReplyConversation = conversation;
  resolveGreetingStop();
  await closing;
  assert.equal(context.activeDispatchConversation, conversation);
  assert.equal(context.callReplyConversation, conversation);
  assert.equal(context.liveVoiceTts, newerOwner);
  assert.equal(context.voiceCallPreparing, true);
});

test("a stale call startup finally cannot clear a newer call's preparing state", async () => {
  const start = source.indexOf("  const openVoiceCallSurface = async () => {");
  const end = source.indexOf("\n  const closeVoiceCallSurface = async", start);
  assert.ok(start >= 0 && end > start);
  let rejectGuard;
  const firstEpoch = {};
  const secondEpoch = {};
  const targetAvatar = {
    className: "",
    classList: { add() {} },
    setAttribute() {},
    replaceChildren() {},
  };
  const modal = {
    dataset: {},
    setAttribute() {},
    querySelector: (selector) => selector === "#voice-call-avatar" ? targetAvatar : ({ focus() {} }),
    classList: { add() {} },
  };
  const sourceAvatar = {
    className: "contact-avatar",
    getAttribute: () => "",
    childNodes: [],
  };
  const recipient = {
    dataset: { recipientId: "contact" },
    querySelector: (selector) => selector === ".contact-avatar"
      ? sourceAvatar
      : ({ textContent: "Contact" }),
  };
  const context = {
    voiceCallActive: false,
    voiceCallPreparing: false,
    voiceCallEpoch: null,
    voiceLatencyTrace: { mode: "call_greeting", stages: [] },
    conversationNavigationRevision: 0,
    document: { getElementById: () => modal, querySelector: () => recipient },
    clearPersistentVoiceCallFailure() {},
    speechOperationToken: () => ({}),
    createVoiceCallActivityGuard: () => ({ ready: new Promise((_resolve, reject) => { rejectGuard = reject; }), clear: async () => {} }),
    startVoiceLatency() {},
    markVoiceLatency() {},
    markVoiceLatencyIn() {},
    startCallRinging: () => new Promise(() => {}),
    reportError: () => assert.fail("a stale cancellation must not be presented as a call failure"),
    recordCommandResult() {},
    errorMessage: (error) => error.message,
    report() {},
    setModalVisibility() {},
    setText() {},
    releaseHistoryRecovery() {},
    releaseCallMicrophoneStream() {},
    performance: { now: () => 1 },
    console,
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.openCall = openVoiceCallSurface;`, context);
  const opening = context.openCall();
  await Promise.resolve();
  assert.equal(typeof rejectGuard, "function");
  const staleEpoch = context.voiceCallEpoch;
  context.voiceCallEpoch = secondEpoch;
  context.voiceCallActive = true;
  context.voiceCallPreparing = true;
  rejectGuard(Object.assign(new Error("call closed"), { code: "call_closed" }));
  await opening;
  assert.notEqual(staleEpoch, secondEpoch);
  assert.equal(context.voiceCallEpoch, secondEpoch);
  assert.equal(context.voiceCallPreparing, true, "the old call's finally is conditional on its epoch");
});

test("a late draft-call result cannot retarget a chat after hangup and navigation", async () => {
  const start = source.indexOf("  const openVoiceCallSurface = async () => {");
  const end = source.indexOf("\n  const closeVoiceCallSurface = async", start);
  assert.ok(start >= 0 && end > start);
  let resolveDraft;
  const modal = {
    dataset: {}, setAttribute() {},
    querySelector: () => ({ focus() {} }),
  };
  const recipient = {
    dataset: { recipientId: "contact" },
    querySelector: (selector) => selector === ".contact-avatar"
      ? null : ({ textContent: "Contact" }),
  };
  const chatView = { dataset: { currentConversation: "default", conversationKind: "chat" } };
  let conversationMutation = Promise.resolve();
  let nativeSelection = "default";
  const commands = [];
  let refreshes = 0;
  const context = {
    voiceCallActive: false,
    voiceCallPreparing: false,
    voiceCallEpoch: null,
    voiceCallActivityGuard: null,
    voiceCallConversation: null,
    voiceLatencyTrace: { mode: "call_greeting" },
    conversationNavigationRevision: 0,
    document: { getElementById: () => modal, querySelector: () => recipient },
    clearPersistentVoiceCallFailure() {},
    speechOperationToken: () => "op",
    createVoiceCallActivityGuard: () => ({ ready: Promise.resolve(), clear: async () => {} }),
    startVoiceLatency() {},
    markVoiceLatency() {},
    markVoiceLatencyIn() {},
    startCallRinging: async () => true,
    setModalVisibility() {}, setText() {},
    suspendVoiceHistoryRecoveryForForeground: async () => async () => {},
    runtimeWarmPromise: Promise.resolve({ ready: true }),
    preparePcmWorklet: async () => {},
    prepareCallMicrophoneStream: async () => ({ stream: { id: "old-mic" }, metrics: {} }),
    releaseCallMicrophoneStream() {},
    selectedConversation: () => chatView.dataset.currentConversation,
    selectedConversationKind: () => "chat",
    chat: () => chatView,
    mutateConversationSelection(operation) {
      const mutation = conversationMutation.catch(() => {}).then(operation);
      conversationMutation = mutation.catch(() => {});
      return mutation;
    },
    invoke: (command) => {
      commands.push(command);
      if (command === "mom_llama_conversation_draft_call_start") {
        return new Promise((resolve) => {
          resolveDraft = () => {
            nativeSelection = "stale-draft";
            resolve({ result: { id: "stale-draft", source_conversation_id: "contact" } });
          };
        });
      }
      if (command === "mom_llama_conversation_select") {
        nativeSelection = "newly-selected-chat";
        return Promise.resolve({ status: "passed" });
      }
      assert.fail(`unexpected native command ${command}`);
    },
    report() {},
    setPersistentVoiceCallFailure() {},
    errorMessage: (error) => error.message,
    closeVoiceCallSurface: async () => {},
    reportError: (error) => { throw error; },
    performance: { now: () => 1 },
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.openCall = openVoiceCallSurface;`, context);
  const opening = context.openCall();
  for (let i = 0; i < 30 && !resolveDraft; i += 1) await Promise.resolve();
  assert.equal(typeof resolveDraft, "function", "startup reached the deferred conversation creation");
  context.conversationNavigationRevision += 1;
  const navigation = context.mutateConversationSelection(
    () => context.invoke("mom_llama_conversation_select", { conversation: "newly-selected-chat" }),
  );
  chatView.dataset.currentConversation = "newly-selected-chat";
  resolveDraft();
  await Promise.all([opening, navigation]);
  assert.deepEqual(commands, ["mom_llama_conversation_draft_call_start", "mom_llama_conversation_select"]);
  assert.equal(nativeSelection, "newly-selected-chat", "the serialized user navigation wins native selection");
  assert.equal(chatView.dataset.currentConversation, "newly-selected-chat");
});

test("a draft call that finishes after hangup is published for the next call", async () => {
  const start = source.indexOf("  const openVoiceCallSurface = async () => {");
  const end = source.indexOf("\n  const closeVoiceCallSurface = async", start);
  assert.ok(start >= 0 && end > start);
  let resolveDraft;
  const modal = { dataset: {}, setAttribute() {}, querySelector: () => ({ focus() {} }) };
  const recipient = {
    dataset: { recipientId: "contact" },
    querySelector: (selector) => selector === ".contact-avatar" ? null : ({ textContent: "Contact" }),
  };
  const chatView = { dataset: { currentConversation: "default", conversationKind: "chat" } };
  let mutation = Promise.resolve();
  let nativeSelection = "default";
  const commands = [];
  let refreshes = 0;
  const context = {
    voiceCallActive: false, voiceCallPreparing: false, voiceCallEpoch: null,
    voiceCallActivityGuard: null, voiceCallConversation: null,
    voiceLatencyTrace: { mode: "call_greeting" }, conversationNavigationRevision: 0,
    document: { getElementById: () => modal, querySelector: () => recipient },
    clearPersistentVoiceCallFailure() {}, speechOperationToken: () => "op",
    createVoiceCallActivityGuard: () => ({ ready: Promise.resolve(), clear: async () => {} }),
    startVoiceLatency() {}, markVoiceLatency() {}, markVoiceLatencyIn() {},
    startCallRinging: async () => true, setModalVisibility() {}, setText() {},
    suspendVoiceHistoryRecoveryForForeground: async () => async () => {},
    runtimeWarmPromise: Promise.resolve({ ready: true }), preparePcmWorklet: async () => {},
    prepareCallMicrophoneStream: async () => ({ stream: { id: "mic" }, metrics: {} }),
    releaseCallMicrophoneStream() {},
    prepareVoicePersonaForCall: async () => false,
    refreshChat: async () => { refreshes += 1; },
    selectedConversation: () => chatView.dataset.currentConversation,
    selectedConversationKind: () => chatView.dataset.conversationKind,
    chat: () => chatView,
    mutateConversationSelection(operation) {
      const next = mutation.catch(() => {}).then(operation);
      mutation = next.catch(() => {});
      return next;
    },
    invoke: (command) => {
      commands.push(command);
      if (command === "mom_llama_conversation_draft_call_start") {
        return new Promise((resolve) => {
          resolveDraft = () => {
            nativeSelection = "materialized-chat";
            resolve({ result: { id: "materialized-chat", source_conversation_id: "contact" } });
          };
        });
      }
      assert.fail(`unexpected native command ${command}`);
    },
    report() {}, setPersistentVoiceCallFailure() {}, errorMessage: (error) => error.message,
    closeVoiceCallSurface: async () => {}, reportError: (error) => { throw error; },
    performance: { now: () => 1 },
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.openCall = openVoiceCallSurface;`, context);
  const opening = context.openCall();
  for (let i = 0; i < 30 && !resolveDraft; i += 1) await Promise.resolve();
  assert.equal(typeof resolveDraft, "function");
  context.voiceCallActive = false;
  context.voiceCallEpoch = null;
  context.voiceCallPreparing = false;
  resolveDraft();
  await opening;
  assert.equal(nativeSelection, "materialized-chat");
  assert.equal(chatView.dataset.currentConversation, "materialized-chat",
    "native selection is reflected even though hangup canceled the call");
  assert.equal(chatView.dataset.conversationKind, "chat");
  assert.equal(refreshes, 1, "the canceled materialization refreshes the visible projection");

  // A queued redial sees the materialized native selection inside its turn
  // and does not issue a second draft creation.
  await context.openCall();
  assert.equal(context.voiceCallConversation, "materialized-chat");
  assert.deepEqual(commands, ["mom_llama_conversation_draft_call_start"]);
});

test("call persona preparation stops when its epoch changes while native preparation is pending", async () => {
  const helperStart = source.indexOf("  const prepareVoicePersonaForCall = async");
  const helperEnd = source.indexOf("\n  const startStreamedCallGreeting", helperStart);
  let resolvePreparation;
  const stages = [];
  const epoch = {};
  const context = {
    voiceCallEpoch: epoch,
    voiceCallActive: true,
    performance: { now: () => 1 },
    errorMessage: (error) => error.message,
    markVoiceLatency: (stage) => stages.push(stage),
    invoke: () => new Promise((resolve) => { resolvePreparation = resolve; }),
  };
  vm.createContext(context);
  vm.runInContext(
    `${source.slice(helperStart, helperEnd)}\nthis.prepareVoicePersonaForCall = prepareVoicePersonaForCall;`,
    context,
  );
  const pending = context.prepareVoicePersonaForCall("contact-a", epoch);
  await Promise.resolve();
  context.voiceCallEpoch = {};
  resolvePreparation({ status: "prompt_smoke_verified", result: { metadata: { id: "stale" } } });
  assert.equal(await pending, false);
  assert.deepEqual(stages, ["call_persona_prepare_started"]);
});

test("Stop appears only for a registered live chat request", () => {
  const definition = source.match(/const chatRequestIsCancellable = \(busyLeases, streamLeases\) => \([\s\S]*?\n  \);/);
  assert.ok(definition, "production cancellable-request policy must remain directly testable");
  const context = {};
  vm.createContext(context);
  vm.runInContext(`${definition[0]}; this.chatRequestIsCancellable = chatRequestIsCancellable;`, context);

  const preparing = "chat-dispatch:1";
  const chat = "chat-request:native-1";
  const mention = "mention-request:persona-1:ad";
  assert.equal(context.chatRequestIsCancellable(new Set([preparing]), new Set()), false);
  assert.equal(context.chatRequestIsCancellable(new Set([mention]), new Set([mention])), false);
  assert.equal(context.chatRequestIsCancellable(new Set([preparing, chat]), new Set([chat])), true);
  assert.equal(context.chatRequestIsCancellable(new Set([preparing]), new Set([chat])), false);
});

for (const switchAt of ["persistence", "dispatch"]) {
  for (const outcome of ["throw", "blocked"]) {
    test(`pending A dispatch ${outcome} preserves B draft and Stop after selection during ${switchAt}`, async () => {
      let submit;
      let selected = "A";
      const drafts = new Map([["A", "message A"], ["B", "precious B"]]);
      const textarea = { value: "message A" };
      const form = { id: "chat-form", dataset: {} };
      const calls = [];
      const switchToB = () => { selected = "B"; textarea.value = "precious B"; };
      const context = {
        document: { addEventListener: (_, handler) => { submit = handler; }, getElementById: () => null },
        recipientMutation: Promise.resolve(),
        activeDispatchStreamLeases: new Set(),
        autocompleteAccepting: false, cancelComposerAutocomplete() {},
        formValue: () => textarea.value, draftAttachmentIds: () => [],
        acquireChatBusy: () => "lease", releaseChatBusy() {},
        suspendVoiceHistoryRecoveryForForeground: async () => () => {},
        selectedConversation: () => selected, selectedConversationKind: () => "chat",
        formField: () => textarea, closeMentions() {}, appendLiveMessage() {}, sizeComposer() {},
        report() {}, reportError() {}, releaseMentionInvocationBusy() {},
        refreshChat: async () => {}, refreshConversationProjection: async () => {},
        persistDraftNow: async (message, _, conversation = selected) => {
          drafts.set(conversation, message);
          if (switchAt === "persistence") switchToB();
        },
        invoke: async (command, args) => {
          calls.push([command, args]);
          if (command === "mom_llama_chat_dispatch") {
            if (switchAt === "dispatch") switchToB();
            await context.cancel();
            if (outcome === "throw") throw new Error("dispatch failed");
            return { status: "blocked" };
          }
        },
      };
      vm.createContext(context);
      const start = source.indexOf('  document.addEventListener("submit"');
      const end = start + source.slice(start).indexOf('\n  });') + 7;
      vm.runInContext(`const isInternalVoicePrompt = (message) => message.startsWith("[Voice note]") || message.startsWith("[Phone call]");\nconst activeDispatchStreamLeaseOwners = new Map();\nlet activeDispatchConversation = null;\nlet activeDispatchOwner = null;\nlet pendingVoiceDispatchPrompt = null;\nlet pendingVoiceTtsOwner = null;\n${source.slice(start, end)}\nthis.cancel = ${source.match(/"chat-cancel": (async \(\) =>[^\n]+)/)[1].replace(/,$/, "")};`, context);
      await submit({ preventDefault() {}, target: form });
      assert.equal(drafts.get("B"), "precious B");
      assert.equal(textarea.value, "precious B");
      assert.equal(drafts.get("A"), "message A");
      assert.equal(calls.find(([command]) => command === "mom_llama_chat_cancel")[1].conversation, "A");
    });
  }
}

test("an unresolved To entry cannot create or dispatch a conversation", async () => {
  let submit;
  let focused = false;
  let invalid = false;
  const context = {
    document: {
      addEventListener: (_, handler) => { submit = handler; },
      getElementById: () => ({ value: "gab", focus: () => { focused = true; }, setAttribute: (key) => { invalid = key === "aria-invalid"; } }),
    },
    autocompleteAccepting: false,
    filterRecipientOptions() {}, exactRecipientOption: () => null,
    reportError(error) { throw error; },
    invoke: () => { throw new Error("Unselected contact must not reach native dispatch"); },
  };
  vm.createContext(context);
  const start = source.indexOf('  document.addEventListener("submit"');
  const end = start + source.slice(start).indexOf('\n  });') + 7;
  vm.runInContext(source.slice(start, end), context);
  await submit({ preventDefault() {}, target: { id: "chat-form", dataset: {} } });
  assert.equal(focused, true);
  assert.equal(invalid, true);
});

for (const outcome of ["blocked", "throw"]) {
  test(`native ${outcome} without a terminal event releases only this dispatch's stream leases`, async () => {
    let submit;
    const leases = new Set();
    const leaseOwners = new Map();
    const released = [];
    const textarea = { value: "Hello" };
    const context = {
      document: { addEventListener: (_, handler) => { submit = handler; }, getElementById: () => null, querySelectorAll: () => [] },
      recipientMutation: Promise.resolve(), activeDispatchStreamLeases: leases,
      activeDispatchStreamLeaseOwners: leaseOwners,
      leaseOwners,
      autocompleteAccepting: false, cancelComposerAutocomplete() {},
      formValue: () => textarea.value, draftAttachmentIds: () => [],
      acquireChatBusy: () => "dispatch", releaseChatBusy: (lease) => {
        released.push(lease); leases.delete(lease); leaseOwners.delete(lease);
      },
      suspendVoiceHistoryRecoveryForForeground: async () => () => {},
      selectedConversation: () => "A", selectedConversationKind: () => "chat",
      formField: () => textarea, closeMentions() {}, appendLiveMessage() {}, sizeComposer() {},
      report() {}, reportError(error) { assert.equal(error.message, "rejected"); },
      releaseMentionInvocationBusy() {}, refreshChat: async () => {}, refreshConversationProjection: async () => {},
      persistDraftNow: async () => {},
      invoke: async () => {
        leases.add("mention-request:failed:contact");
        leaseOwners.set("mention-request:failed:contact", context.getOwner());
        if (outcome === "throw") throw new Error("rejected");
        return { status: "blocked" };
      },
    };
    vm.createContext(context);
    const start = source.indexOf('  document.addEventListener("submit"');
    const end = start + source.slice(start).indexOf('\n  });') + 7;
    vm.runInContext(`const isInternalVoicePrompt = (message) => message.startsWith("[Voice note]") || message.startsWith("[Phone call]");\nconst activeDispatchStreamLeaseOwners = leaseOwners;\nlet activeDispatchConversation = null;\nlet activeDispatchOwner = null;\nthis.getOwner = () => activeDispatchOwner;\nlet voiceReplyConversation = null;\nlet callReplyConversation = null;\nlet pendingVoiceDispatchPrompt = null;\nlet pendingVoiceTtsOwner = null;\n${source.slice(start, end)}`, context);
    await submit({ preventDefault() {}, target: { id: "chat-form", dataset: {} } });
    assert.deepEqual(released, ["mention-request:failed:contact", "dispatch"]);
    assert.equal(leases.size, 0);
    assert.equal(textarea.value, "Hello");
  });
}

test("a direct voice fallback keeps its capture through indexing and admits reply speech", async () => {
  let submit;
  const stages = [];
  const indexed = [];
  const finishedSpeech = [];
  const ordering = [];
  const boundVoiceTargets = [];
  let resolveSpeech;
  let signalFinishStarted;
  const finishStarted = new Promise((resolve) => { signalFinishStarted = resolve; });
  const persistedDrafts = [];
  const capture = { trace: { mode: "voice_note" } };
  const attachment = { id: "audio-attachment" };
  const output = { assistant_text: "A spoken reply.", assistant_message_id: "assistant-message" };
  const result = { status: "passed", result: { kind: "direct", conversation_id: "conversation-a", output } };
  const owner = {
    conversation: "conversation-a",
    trace: capture.trace,
    finish: (text) => {
      finishedSpeech.push(text);
      ordering.push("finish");
      signalFinishStarted();
      return new Promise((resolve) => { resolveSpeech = resolve; });
    },
    trackGeneration: (operation) => operation,
    bindDurableVoiceReply: (target) => {
      ordering.push("persist");
      boundVoiceTargets.push(target);
      return Promise.resolve("voice-attachment");
    },
    stop: async () => true,
  };
  const context = {
    document: {
      addEventListener: (_, handler) => { submit = handler; },
      getElementById: () => null,
      querySelectorAll: () => [],
    },
    recipientMutation: Promise.resolve(),
    activeDispatchStreamLeases: new Set(),
    leaseOwners: new Map(),
    autocompleteAccepting: false,
    cancelComposerAutocomplete() {},
    formValue: () => "",
    draftAttachmentIds: () => [],
    acquireChatBusy: () => "voice-dispatch",
    releaseChatBusy() {},
    suspendVoiceHistoryRecoveryForForeground: async () => () => {},
    selectedConversation: () => "conversation-a",
    selectedConversationKind: () => "chat",
    formField: () => null,
    closeMentions() {}, sizeComposer() {}, appendLiveMessage() {},
    updateComposerDraft: async () => ({ status: "passed" }),
    persistDraftNow: async (_message, ids) => persistedDrafts.push(ids),
    refreshChat: async () => {}, refreshConversationProjection: async () => {},
    refreshSidebar: async () => {}, refreshConversationSettingsProjection: async () => {},
    report() {}, reportError(error) { throw error; },
    releaseMentionInvocationBusy() {},
    markVoiceLatencyIn: (_trace, stage, detail) => stages.push([stage, detail]),
    indexVoiceTurn: (...args) => { ordering.push("index"); indexed.push(args); },
    owner, attachment, capture,
    invoke: async (command) => {
      if (command === "mom_llama_chat_dispatch") return result;
      if (command === "mom_llama_draft_update") return { status: "passed" };
      throw new Error(`unexpected invocation: ${command}`);
    },
  };
  const helperStart = source.indexOf("  const finishVoiceReplyAndIndexTurn = async");
  const helperEnd = source.indexOf("\n  const startPendingCallTranscriptSlot", helperStart);
  const helper = source.slice(helperStart, helperEnd);
  vm.createContext(context);
  const start = source.indexOf('  document.addEventListener("submit"');
  const end = start + source.slice(start).indexOf('\n  });') + 7;
  vm.runInContext(`
    ${helper}
    const isInternalVoicePrompt = (message) => message.startsWith("[Voice note]") || message.startsWith("[Phone call]");
    const activeDispatchStreamLeaseOwners = leaseOwners;
    let activeDispatchConversation = null;
    let activeDispatchOwner = null;
    this.replaceDispatchOwner = (owner, conversation) => {
      activeDispatchOwner = owner;
      activeDispatchConversation = conversation;
    };
    this.getOwner = () => activeDispatchOwner;
    this.getConversation = () => activeDispatchConversation;
    let voiceReplyConversation = null;
    let callReplyConversation = null;
    let voiceReplyMessageId = null;
    let pendingVoiceDispatchPrompt = "[Voice note]";
    let pendingVoiceTtsOwner = owner;
    let pendingVoiceAudioSession = null;
    let pendingVoiceFallbackAttachment = { conversation: "conversation-a", attachment, sourceCapture: capture };
    let liveVoiceTts = owner;
    let voiceLatencyTrace = capture.trace;
    ${source.slice(start, end)}
  `, context);
  // The production listener is the unit under test; capture it from the mock document.
  assert.equal(typeof submit, "function");
  const submitted = submit({ preventDefault() {}, target: { id: "chat-form", dataset: {}, querySelector: () => null } });
  assert.ok(context.getOwner(), "generic dispatch claims cleanup ownership synchronously at admission");
  const replacementOwner = { mode: "call" };
  context.replaceDispatchOwner(replacementOwner, "conversation-a");
  context.activeDispatchStreamLeases.add("newer-chat-request");
  context.leaseOwners.set("newer-chat-request", replacementOwner);
  await finishStarted;
  assert.deepEqual(indexed, [], "voice-note ASR waits while reply TTS is still active");
  assert.deepEqual(ordering, ["finish", "persist"], "audio publication starts without waiting for the UI row");
  resolveSpeech(true);
  await submitted;
  assert.equal(persistedDrafts.length, 1);
  assert.equal(persistedDrafts[0][0], attachment);
  assert.equal(indexed.length, 1, "the completed direct result must reach post-commit indexing");
  assert.equal(indexed[0][2], attachment);
  assert.equal(indexed[0][3], capture, "the fallback source capture remains in scope after dispatch");
  assert.deepEqual(finishedSpeech, [output.assistant_text], "the retained owner must receive the direct output");
  assert.deepEqual(JSON.parse(JSON.stringify(boundVoiceTargets)), [{
    conversation: "conversation-a", message: "assistant-message", text: "A spoken reply.",
  }], "the committed assistant identity binds the completed reply audio");
  assert.deepEqual(ordering, ["finish", "persist", "index"], "backfill waits for audio publication");
  assert.ok(stages.some(([stage]) => stage === "tts_text_admitted"));
  assert.equal(context.getOwner(), replacementOwner,
    "an older generic submit cannot clear a newer greeting owner in the same chat");
  assert.equal(context.getConversation(), "conversation-a");
  assert.equal(context.activeDispatchStreamLeases.has("newer-chat-request"), true,
    "an older submit releases only its own stream busy leases");
});

test("voice replies do not create an incoming card or expose streamed text", () => {
  const events = [];
  const context = {
    voiceReplyConversation: "conversation-a",
    callReplyConversation: null,
    activeDispatchConversation: "conversation-a",
    chatRequestLease: ({ request_id }) => request_id,
    acquireStableChatBusy() {},
    appendLiveVoiceReply: (id) => events.push(["voice", id]),
    appendLiveMessage: (...args) => events.push(["text", ...args]),
    appendLiveDelta: (...args) => events.push(["delta", ...args]),
    document: { getElementById: () => null },
    finishLiveMessage() {}, releaseChatBusy() {},
    settingEnabled: () => true, keepLiveTailVisible() {},
    liveVoiceTts: {
      conversation: "conversation-a", mode: "voice_note",
      acceptsRequest: (request) => request === "r1", push: () => {},
      unavailable: (error) => events.push(["unavailable", error.message]),
      cancel: () => events.push(["cancel"]),
    },
  };
  vm.createContext(context);
  const start = source.indexOf("  const voiceStreamRequestIds =");
  const end = source.indexOf("\n  const mentionLiveId", start);
  vm.runInContext(`let voiceLatencyTrace = null;\nconst markVoiceLatency = () => {};\nconst markVoiceLatencyIn = () => {};\nconst startVoiceLatency = () => { voiceLatencyTrace = {}; };\n${source.slice(start, end)}\nglobalThis.testOnChatEvent = onChatEvent;`, context);
  context.testOnChatEvent({ payload: { event: "started", request_id: "r1", conversation_id: "conversation-a" } });
  context.testOnChatEvent({ payload: { event: "delta", request_id: "r1", conversation_id: "conversation-a", delta: "hidden assistant text" } });
  context.testOnChatEvent({ payload: { event: "reasoning_delta", request_id: "r1", conversation_id: "conversation-a", delta: "hidden reasoning" } });
  context.testOnChatEvent({ payload: { event: "failed", request_id: "r1", conversation_id: "conversation-a", message: "generation failed" } });
  assert.deepEqual(events, [["unavailable", "generation failed"]]);
});

test("call replies suppress live text but still own the chat request lease", () => {
  const events = [];
  const context = {
    callReplyConversation: "conversation-a",
    voiceReplyConversation: null,
    activeDispatchConversation: "conversation-a",
    chatRequestLease: ({ request_id }) => `chat-request:${request_id}`,
    acquireStableChatBusy: (lease) => events.push(["acquire", lease]),
    appendLiveVoiceReply: (id) => events.push(["voice", id]),
    appendLiveMessage: (...args) => events.push(["text", ...args]),
    appendLiveDelta: (...args) => events.push(["delta", ...args]),
    document: { getElementById: () => null, querySelector: () => null },
    finishLiveMessage: (message) => events.push(["finish", message]),
    releaseChatBusy: (lease) => events.push(["release", lease]),
    settingEnabled: () => true, keepLiveTailVisible() {},
    liveVoiceTts: {
      conversation: "conversation-a", mode: "call",
      acceptsRequest: (request) => request === "r-call", push: () => {},
    },
  };
  vm.createContext(context);
  const start = source.indexOf("  const voiceStreamRequestIds =");
  const end = source.indexOf("\n  const mentionLiveId", start);
  vm.runInContext(`let voiceLatencyTrace = null;\nconst markVoiceLatency = () => {};\nconst markVoiceLatencyIn = () => {};\nconst startVoiceLatency = () => { voiceLatencyTrace = {}; };\n${source.slice(start, end)}\nglobalThis.testOnChatEvent = onChatEvent;`, context);
  context.testOnChatEvent({ payload: { event: "started", request_id: "r-call", conversation_id: "conversation-a" } });
  context.testOnChatEvent({ payload: { event: "delta", request_id: "r-call", conversation_id: "conversation-a", delta: "hidden reply" } });
  context.testOnChatEvent({ payload: { event: "reasoning_delta", request_id: "r-call", conversation_id: "conversation-a", delta: "hidden reasoning" } });
  context.testOnChatEvent({ payload: { event: "completed", request_id: "r-call", conversation_id: "conversation-a" } });
  assert.deepEqual(events, [
    ["acquire", "chat-request:r-call"],
    ["finish", null],
    ["release", "chat-request:r-call"],
  ]);
});

test("an exact call request keeps its TTS owner after stale conversation cleanup", () => {
  const events = [];
  const context = {
    callReplyConversation: null,
    voiceReplyConversation: null,
    activeDispatchConversation: null,
    chatRequestLease: ({ request_id }) => `chat-request:${request_id}`,
    acquireStableChatBusy: (lease) => events.push(["acquire", lease]),
    appendLiveMessage: (...args) => events.push(["text", ...args]),
    appendLiveDelta: (...args) => events.push(["delta", ...args]),
    document: { getElementById: () => null, querySelector: () => null },
    finishLiveMessage() {}, releaseChatBusy() {},
    settingEnabled: () => true, keepLiveTailVisible() {},
    liveVoiceTts: {
      conversation: "conversation-a", mode: "call",
      acceptsRequest: (request) => request === "replacement-request",
      push: (delta) => events.push(["tts", delta]),
    },
  };
  vm.createContext(context);
  const start = source.indexOf("  const voiceStreamRequestIds =");
  const end = source.indexOf("\n  const mentionLiveId", start);
  vm.runInContext(`let voiceLatencyTrace = null;\nconst markVoiceLatency = () => {};\nconst markVoiceLatencyIn = () => {};\nconst startVoiceLatency = () => { voiceLatencyTrace = {}; };\n${source.slice(start, end)}\nglobalThis.testOnChatEvent = onChatEvent;`, context);
  context.testOnChatEvent({ payload: {
    event: "started", request_id: "replacement-request", conversation_id: "conversation-a",
  } });
  context.testOnChatEvent({ payload: {
    event: "delta", request_id: "replacement-request", conversation_id: "conversation-a",
    delta: "This must reach speech.",
  } });
  assert.deepEqual(events, [
    ["acquire", "chat-request:replacement-request"],
    ["tts", "This must reach speech."],
  ]);
});

test("text EOF flush is routed only to the exact uncancelled voice owner", () => {
  const events = [];
  let cancelled = false;
  const owner = {
    conversation: "conversation-a",
    mode: "call",
    trace: null,
    lastProgressAt: 0,
    acceptsRequest: (request) => !cancelled && request === "current-request",
    finish: (text) => events.push(["finish", text]),
  };
  const context = {
    liveVoiceTts: owner,
    performance: { now: () => 1 },
    markVoiceLatencyIn() {},
    releaseChatBusy: (lease) => events.push(["release", lease]),
    finishLiveMessage() {},
  };
  vm.createContext(context);
  const start = source.indexOf("  const voiceStreamRequestIds = new Set();");
  const end = source.indexOf("\n  const mentionLiveId", start);
  assert.ok(start >= 0 && end > start);
  vm.runInContext(
    `${source.slice(start, end)}\nglobalThis.testOnChatEvent = onChatEvent;`,
    context,
  );
  const sendTextEof = (requestId, text) => context.testOnChatEvent({ payload: {
    event: "text_completed",
    request_id: requestId,
    conversation_id: "conversation-a",
    delta: text,
  } });

  sendTextEof("stale-request", "stale response");
  assert.deepEqual(events, [], "a stale request cannot flush the newer live owner");
  sendTextEof("current-request", "current response");
  assert.deepEqual(events, [["finish", "current response"]]);

  cancelled = true;
  sendTextEof("current-request", "late cancelled response");
  assert.deepEqual(events, [["finish", "current response"]],
    "a late EOF after cancellation must be ignored");
});

test("late voice events cannot become text or enter the next turn's trace", () => {
  const events = [];
  const trace = { stages: [] };
  const context = {
    voiceReplyConversation: null,
    callReplyConversation: null,
    activeDispatchConversation: null,
    liveVoiceTts: null,
    chatRequestLease: ({ request_id }) => `chat-request:${request_id}`,
    acquireStableChatBusy: (lease) => events.push(["acquire", lease]),
    appendLiveMessage: (...args) => events.push(["text", ...args]),
    appendLiveDelta: (...args) => events.push(["delta", ...args]),
    document: { getElementById: () => null, querySelector: () => null },
    finishLiveMessage() {},
    releaseChatBusy: (lease) => events.push(["release", lease]),
    settingEnabled: () => true,
    keepLiveTailVisible() {},
  };
  vm.createContext(context);
  const start = source.indexOf("  const voiceStreamRequestIds =");
  const end = source.indexOf("\n  const mentionLiveId", start);
  vm.runInContext(`let voiceLatencyTrace = globalThis.nextTrace;
const markVoiceLatencyIn = (trace, stage) => trace.stages.push(stage);
${source.slice(start, end)}
globalThis.testOnChatEvent = onChatEvent;
globalThis.rememberVoice = rememberVoiceStreamRequest;`, Object.assign(context, { nextTrace: trace }));
  context.rememberVoice("retired-voice");
  for (const event of ["started", "delta", "completed"]) {
    context.testOnChatEvent({ payload: {
      event, request_id: "retired-voice", conversation_id: "conversation-a", delta: "late speech",
    } });
  }
  assert.deepEqual(events, [["release", "chat-request:retired-voice"]]);
  assert.deepEqual(trace.stages, []);
  context.testOnChatEvent({ payload: {
    event: "started", request_id: "ordinary-text", conversation_id: "conversation-b",
  } });
  assert.deepEqual(events.slice(1), [
    ["acquire", "chat-request:ordinary-text"],
    ["text", "assistant", "", "live-assistant-ordinary-text"],
  ]);
  assert.deepEqual(trace.stages, [], "unrelated text work never changes a voice-turn trace");
});

test("a replacement speculation gets its own first-content timing", () => {
  const trace = { stages: [] };
  const owner = (requestId) => ({
    conversation: "conversation-a", mode: "call", trace, firstDeltaSeen: false,
    acceptsRequest: (candidate) => candidate === requestId,
    push() {},
  });
  const context = {
    liveVoiceTts: owner("first-attempt"),
    chatRequestLease: () => null,
    acquireStableChatBusy() {}, releaseChatBusy() {},
    document: { getElementById: () => null },
  };
  vm.createContext(context);
  const start = source.indexOf("  const voiceStreamRequestIds =");
  const end = source.indexOf("\n  const mentionLiveId", start);
  vm.runInContext(`const markVoiceLatencyIn = (trace, stage) => trace?.stages.push(stage);
${source.slice(start, end)}
globalThis.testOnChatEvent = onChatEvent;`, context);
  for (const requestId of ["first-attempt", "replacement-attempt"]) {
    if (requestId === "replacement-attempt") context.liveVoiceTts = owner(requestId);
    context.testOnChatEvent({ payload: {
      event: "delta", request_id: requestId, conversation_id: "conversation-a", delta: "Hello",
    } });
  }
  assert.equal(trace.stages.filter((stage) => stage === "first_content_delta").length, 2);
  assert.equal(trace.stages.filter((stage) => stage === "first_content_delta_received").length, 2);
});

test("the native voice dispatch routes a direct chat stream event", () => {
  const received = [];
  const context = {
    voiceLatencyTrace: null,
    onChatEvent: (event) => received.push(event.payload),
  };
  vm.createContext(context);
  const start = source.indexOf("  const onDispatchEvent =");
  const end = source.indexOf("\n  let mentionSearchSerial", start);
  vm.runInContext(`${source.slice(start, end)}\nthis.testOnDispatchEvent = onDispatchEvent;`, context);
  const event = {
    schema: "mom_llama.chat.stream.v1",
    event: "delta",
    request_id: "voice-request",
    conversation_id: "conversation-a",
    delta: "Speak this.",
  };
  context.testOnDispatchEvent({ payload: event });
  assert.deepEqual(received, [event]);
});

test("voice replies keep typing chrome until a durable voice note is complete", () => {
  const rowClasses = [];
  let pendingAvatarRemoved = false;
  const card = { closest: () => row, replaceChildren() {} };
  const row = {
    classList: { add: (...classes) => rowClasses.push(...classes) },
    querySelector: (selector) => selector === ".message-speaker-avatar"
      ? { remove: () => { pendingAvatarRemoved = true; } }
      : null,
  };
  const voiceCardContext = {
    appendLiveMessage: () => card,
    pendingVoiceReply: () => ({ nodeType: 1 }),
  };
  vm.createContext(voiceCardContext);
  const voiceStart = source.indexOf("  const appendLiveVoiceReply =");
  const voiceEnd = source.indexOf("\n  const keepLiveTailVisible", voiceStart);
  vm.runInContext(`${source.slice(voiceStart, voiceEnd)}\nthis.makeVoiceReply = appendLiveVoiceReply;`, voiceCardContext);
  voiceCardContext.makeVoiceReply("live-mention-i-p");
  assert.deepEqual(rowClasses, ["voice-reply-row", "voice-reply-pending"]);
  assert.equal(pendingAvatarRemoved, true, "pending voice replies must not expose an orphaned portrait");
  assert.notEqual(row.hidden, true);

  const pendingStart = source.indexOf("  const pendingVoiceReply =");
  const pendingEnd = source.indexOf("\n  const unavailableVoiceMessage", pendingStart);
  const pending = source.slice(pendingStart, pendingEnd);
  assert.match(pending, /return createTypingIndicator\(\)/);
  assert.match(pending, /const pendingVoiceReplyPlayer =/);
  assert.doesNotMatch(source, /firstPcmSeen = true;\s*mountVoiceReplyPlayer\(\)/);
  assert.match(source, /voiceNoteComplete = true;\s*settleVoiceAudioReady\(true\);\s*const voiceButton = mountVoiceReplyPlayer\(\)/);
  assert.match(source, /bindPendingRow\(\) \{\s*if \(!voiceNoteComplete\) return button/);

  const calls = [];
  let pendingReplyRemoved = false;
  const mentionRow = { querySelector: () => null, setAttribute() {} };
  const dispatchContext = {
    voiceReplyConversation: "conversation-a",
    callReplyConversation: null,
    activeDispatchConversation: "conversation-a",
    activeDispatchStreamLeases: new Set(),
    chatBusyLeases: new Set(),
    chatRequestLease: () => null,
    acquireStableChatBusy() {},
    mentionLiveId: (payload) => `live-mention-${payload.invocation_id}-${payload.target_id}`,
    appendMentionMessage: (payload, voiceReply) => calls.push(["mention", voiceReply]),
    appendLiveDelta: (...args) => calls.push(["delta", ...args]),
    document: { getElementById: (id) => {
      if (id === "live-voice-reply-pending") {
        return { remove: () => { pendingReplyRemoved = true; } };
      }
      return id.startsWith("live-mention-") ? mentionRow : null;
    } },
    finishLiveMessage() {}, releaseChatBusy() {},
  };
  vm.createContext(dispatchContext);
  const dispatchStart = source.indexOf("  const onDispatchEvent =");
  const dispatchEnd = source.indexOf("\n  let mentionSearchSerial", dispatchStart);
  vm.runInContext(`${source.slice(dispatchStart, dispatchEnd)}\nthis.dispatch = onDispatchEvent;`, dispatchContext);
  dispatchContext.dispatch({ kind: "mention", event: { event: "started", invocation_id: "i", target_id: "p" } });
  dispatchContext.dispatch({ kind: "mention", event: { event: "delta", invocation_id: "i", target_id: "p", delta: "hidden text" } });
  assert.deepEqual(calls, []);
  assert.equal(pendingReplyRemoved, false, "the voice reply's own stream must not destroy its PCM control");
});

test("voice completion remains owned until durable PCM succeeds or fails", () => {
  const speculateStart = source.indexOf("  const startVoiceSpeculation = async");
  const speculateEnd = source.indexOf("\n  const sendVoiceCapture = async", speculateStart);
  const speculate = source.slice(speculateStart, speculateEnd);
  assert.match(speculate, /finishVoiceReplyAndIndexTurn\(recording\.call,[\s\S]*tts\.finish\(response\.result\.assistant_text \|\| tts\.text\),/);
  assert.match(speculate, /if \(!synthesized\)[\s\S]*voice_reply_incomplete/);

  const greetingStart = source.indexOf("  const startStreamedCallGreeting = async");
  const greetingEnd = source.indexOf("\n  const openVoiceCallSurface = async", greetingStart);
  const greeting = source.slice(greetingStart, greetingEnd);
  assert.match(greeting, /const synthesized = await tts\.finish\(greeting\)/);
  assert.match(greeting, /if \(!synthesized\)[\s\S]*voice could not be produced/);
});

test("silent voice-note confirmation removes only its optimistic rows", async () => {
  const sourceStart = source.indexOf("  const confirmVoiceNoteSpeculation = async");
  const sourceEnd = source.indexOf("\n  const startVoiceSpeculation = async", sourceStart);
  assert.ok(sourceStart >= 0 && sourceEnd > sourceStart,
    "the production voice-note confirmation handler must remain testable");

  const makeRow = () => {
    const classes = new Set(["voice-reply-pending"]);
    return {
      hidden: false,
      removed: false,
      classList: { remove: (name) => classes.delete(name) },
      card: { replaceChildren(value) { this.contents = [value]; } },
      querySelector(selector) { return selector === ".message-card" ? this.card : null; },
      remove() { this.removed = true; },
    };
  };
  const outgoing = makeRow();
  outgoing.dataset = { objectUrl: "blob:outgoing" };
  const pendingReply = makeRow();
  pendingReply.dataset = {};
  const owner = { stop: async () => {}, bindButton() {}, bindPendingRow() {} };
  const recording = { speculation: null, sending: false, trace: {} };
  const speculation = { id: "spec-silent", conversation: "conversation-a", tts: owner,
    confirmed: false, aborting: false, resolveCapture(promise) { promise.catch(() => {}); } };
  recording.speculation = speculation;
  const stages = [];
  const revoked = [];
  const context = {
    voiceRecording: recording,
    voiceCallActive: false,
    voiceReplyConversation: null,
    liveVoiceTts: null,
    document: { querySelector: () => ({}) },
    URL: { revokeObjectURL: (url) => revoked.push(url) },
    appendOptimisticVoiceMessage: () => outgoing,
    appendLiveVoiceReply: () => pendingReply,
    setVoiceRecordingVisible() {},
    markVoiceLatencyIn: (_trace, stage, detail) => stages.push([stage, detail]),
    stopVoiceCapture: async () => ({ heardVoice: false, wavBytes: new Uint8Array(44) }),
    confirmRecordedSpeculation: async () => { throw new Error("The voice turn has no complete recording."); },
    hydrateOptimisticVoiceMessage() { assert.fail("silent capture must not become playable audio"); },
    unavailableVoiceMessage: () => ({ unavailable: true }),
    errorMessage: (error) => error.message,
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(sourceStart, sourceEnd)}\nthis.confirm = confirmVoiceNoteSpeculation;`, context);
  context.liveVoiceTts = owner;
  context.voiceReplyConversation = "conversation-a";

  await assert.rejects(context.confirm(recording, speculation), /no complete recording/);
  assert.equal(outgoing.removed, true);
  assert.equal(pendingReply.removed, true);
  assert.deepEqual(revoked, ["blob:outgoing"]);
  assert.equal(context.liveVoiceTts, null);
  assert.equal(context.voiceReplyConversation, null);
  assert.ok(stages.some(([stage, detail]) => stage === "voice_note_confirmation_failed"
    && detail.heard_voice === false));
});

test("failed voice-note confirmation preserves valid audio without clearing a newer reply", async () => {
  const sourceStart = source.indexOf("  const confirmVoiceNoteSpeculation = async");
  const sourceEnd = source.indexOf("\n  const startVoiceSpeculation = async", sourceStart);
  const classes = new Set(["voice-reply-pending"]);
  const pendingReply = {
    hidden: false,
    removed: false,
    dataset: {},
    classList: { remove: (name) => classes.delete(name) },
    card: { replaceChildren(value) { this.contents = [value]; } },
    querySelector(selector) { return selector === ".message-card" ? this.card : null; },
    remove() { this.removed = true; },
  };
  const outgoing = { dataset: {}, removed: false };
  const recordingBytes = new Uint8Array(128);
  const newerReplyContent = { text: "new reply" };
  const oldOwner = { stop: async () => {}, bindButton() {}, bindPendingRow() {} };
  const newOwner = { newer: true };
  const recording = { speculation: null, sending: false, trace: {} };
  const speculation = { id: "spec-old", conversation: "conversation-a", tts: oldOwner,
    confirmed: false, aborting: false, resolveCapture(promise) { promise.catch(() => {}); } };
  recording.speculation = speculation;
  let context;
  context = {
    voiceRecording: recording,
    voiceCallActive: false,
    voiceReplyConversation: "conversation-a",
    liveVoiceTts: newOwner,
    document: { querySelector: () => ({}) },
    URL: { revokeObjectURL() {} },
    appendOptimisticVoiceMessage: () => outgoing,
    appendLiveVoiceReply: () => pendingReply,
    setVoiceRecordingVisible() {},
    markVoiceLatencyIn() {},
    stopVoiceCapture: async () => ({ heardVoice: true, samples: () => new Float32Array([0.25]) }),
    confirmRecordedSpeculation: async () => {
      await Promise.resolve();
      context.liveVoiceTts = newOwner;
      pendingReply.dataset.voiceSpeculationId = "spec-new";
      pendingReply.card.replaceChildren(newerReplyContent);
      throw new Error("native confirmation failed");
    },
    encodePcmWav: () => recordingBytes,
    hydrateOptimisticVoiceMessage: (row, bytes) => { row.hydratedWith = bytes; },
    unavailableVoiceMessage: () => unavailable,
    errorMessage: (error) => error.message,
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(sourceStart, sourceEnd)}\nthis.confirm = confirmVoiceNoteSpeculation;`, context);

  await assert.rejects(context.confirm(recording, speculation), /native confirmation failed/);
  assert.equal(outgoing.hydratedWith, recordingBytes, "the original captured message stays playable");
  assert.equal(pendingReply.removed, false);
  assert.deepEqual(pendingReply.card.contents, [newerReplyContent], "late failure does not mutate a reused reply row");
  assert.equal(pendingReply.hidden, false);
  assert.equal(classes.has("voice-reply-pending"), true);
  assert.equal(context.liveVoiceTts, newOwner, "late failure cannot clear a newer reply owner");
  assert.equal(context.voiceReplyConversation, "conversation-a");
});

test("browser voice-note capture samples are encoded and hydrated after confirmation", async () => {
  const sourceStart = source.indexOf("  const confirmVoiceNoteSpeculation = async");
  const sourceEnd = source.indexOf("\n  const startVoiceSpeculation = async", sourceStart);
  const outgoing = { dataset: {}, hydratedWith: null };
  const pendingReply = { dataset: {}, classList: { add() {}, remove() {} }, hidden: false };
  const owner = { bindButton() {}, bindPendingRow() {} };
  const recording = { speculation: null, sending: false, trace: {} };
  const speculation = { id: "browser-spec", conversation: "conversation-b", tts: owner,
    confirmed: false, aborting: false, resolveCapture(promise) { promise.catch(() => {}); } };
  recording.speculation = speculation;
  const samples = new Float32Array([0.25, -0.25]);
  const encoded = new Uint8Array(128);
  const encodedSamples = [];
  const context = {
    voiceRecording: recording,
    voiceCallActive: false,
    voiceReplyConversation: null,
    liveVoiceTts: null,
    document: { querySelector: () => ({}) },
    URL: { revokeObjectURL() {} },
    appendOptimisticVoiceMessage: () => outgoing,
    appendLiveVoiceReply: () => pendingReply,
    setVoiceRecordingVisible() {},
    markVoiceLatencyIn() {},
    stopVoiceCapture: async () => ({ heardVoice: true, samples: () => samples }),
    confirmRecordedSpeculation: async () => ({}),
    encodePcmWav: (input) => { encodedSamples.push(input); return encoded; },
    hydrateOptimisticVoiceMessage: (row, bytes) => { row.hydratedWith = bytes; },
    unavailableVoiceMessage: () => ({ unavailable: true }),
    errorMessage: (error) => error.message,
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(sourceStart, sourceEnd)}\nthis.confirm = confirmVoiceNoteSpeculation;`, context);

  assert.equal(await context.confirm(recording, speculation), true);
  assert.equal(encodedSamples.length, 1);
  assert.deepEqual(Array.from(encodedSamples[0]), [0.25, -0.25]);
  assert.equal(outgoing.hydratedWith, encoded);
});

test("WAV encoding failure settles owned rows without replacing the confirmation error", async () => {
  const sourceStart = source.indexOf("  const confirmVoiceNoteSpeculation = async");
  const sourceEnd = source.indexOf("\n  const startVoiceSpeculation = async", sourceStart);
  const outgoing = { dataset: { objectUrl: "blob:unsealed" }, removed: false,
    remove() { this.removed = true; } };
  const pendingReply = { dataset: {}, removed: false, hidden: false,
    remove() { this.removed = true; } };
  const owner = { bindButton() {}, bindPendingRow() {} };
  const recording = { speculation: null, sending: false, trace: {} };
  const speculation = { id: "bad-wav-spec", conversation: "conversation-c", tts: owner,
    confirmed: false, aborting: false, resolveCapture(promise) { promise.catch(() => {}); } };
  recording.speculation = speculation;
  const revoked = [];
  const stages = [];
  const context = {
    voiceRecording: recording,
    voiceCallActive: false,
    voiceReplyConversation: null,
    liveVoiceTts: null,
    document: { querySelector: () => ({}) },
    URL: { revokeObjectURL: (url) => revoked.push(url) },
    appendOptimisticVoiceMessage: () => outgoing,
    appendLiveVoiceReply: () => pendingReply,
    setVoiceRecordingVisible() {},
    markVoiceLatencyIn: (_trace, stage, detail) => stages.push([stage, detail]),
    stopVoiceCapture: async () => ({ heardVoice: true, samples: () => new Float32Array([0.5]) }),
    confirmRecordedSpeculation: async () => { throw new Error("native confirmation failed"); },
    encodePcmWav: () => { throw new Error("WAV encoder failed"); },
    unavailableVoiceMessage: () => ({ unavailable: true }),
    errorMessage: (error) => error.message,
  };
  vm.createContext(context);
  vm.runInContext(`${source.slice(sourceStart, sourceEnd)}\nthis.confirm = confirmVoiceNoteSpeculation;`, context);
  context.liveVoiceTts = owner;
  context.voiceReplyConversation = "conversation-c";

  await assert.rejects(context.confirm(recording, speculation), /native confirmation failed/);
  assert.equal(outgoing.removed, true);
  assert.equal(pendingReply.removed, true);
  assert.deepEqual(revoked, ["blob:unsealed"]);
  assert.equal(context.liveVoiceTts, null);
  assert.ok(stages.some(([stage, detail]) => stage === "voice_note_confirmation_failed"
    && detail.message === "native confirmation failed"));
});

test("voice transcript backfill queues during calls but waits for successful note playback", async () => {
  const helperStart = source.indexOf("  const finishVoiceReplyAndIndexTurn = async");
  const helperEnd = source.indexOf("\n  const startPendingCallTranscriptSlot", helperStart);
  assert.ok(helperStart >= 0 && helperEnd > helperStart, "production reply/index ordering helper must remain testable");
  const context = {};
  vm.createContext(context);
  vm.runInContext(
    `${source.slice(helperStart, helperEnd)}\nthis.finishReply = finishVoiceReplyAndIndexTurn;`,
    context,
  );

  const deferred = () => {
    let resolve;
    const promise = new Promise((done) => { resolve = done; });
    return { promise, resolve };
  };

  const callEvents = [];
  const callFinish = deferred();
  const callTask = context.finishReply(true,
    () => callEvents.push("index"),
    () => { callEvents.push("finish"); return callFinish.promise; });
  assert.deepEqual(callEvents, ["index", "finish"], "call transcript is enqueued before playback completion");
  callFinish.resolve(false);
  assert.equal(await callTask, false);
  assert.deepEqual(callEvents, ["index", "finish"], "a failed call reply still leaves its caller transcript queued");

  const noteEvents = [];
  const noteFinish = deferred();
  const noteTask = context.finishReply(false,
    () => noteEvents.push("index"),
    () => { noteEvents.push("finish"); return noteFinish.promise; });
  assert.deepEqual(noteEvents, ["finish"], "voice-note indexing must not compete with active synthesis");
  noteFinish.resolve(true);
  assert.equal(await noteTask, true);
  assert.deepEqual(noteEvents, ["finish", "index"], "successful playback releases voice-note backfill");

  const failedNoteEvents = [];
  const failedNote = await context.finishReply(false,
    () => failedNoteEvents.push("index"),
    async () => { failedNoteEvents.push("finish"); return false; });
  assert.equal(failedNote, false);
  assert.deepEqual(failedNoteEvents, ["finish"], "failed or cancelled playback preserves the original recording without starting ASR");

  const cancelledNoteEvents = [];
  await assert.rejects(context.finishReply(false,
    () => cancelledNoteEvents.push("index"),
    async () => { cancelledNoteEvents.push("finish"); throw new Error("cancelled"); }), /cancelled/);
  assert.deepEqual(cancelledNoteEvents, ["finish"], "a cancelled note reply does not start ASR or replace its source recording");

  const speculativeStart = source.indexOf("  const startVoiceSpeculation = async");
  const speculativeEnd = source.indexOf("\n  const sendVoiceCapture = async", speculativeStart);
  const speculative = source.slice(speculativeStart, speculativeEnd);
  assert.match(speculative, /finishVoiceReplyAndIndexTurn\(recording\.call,[\s\S]*tts\.finish/);
  const submitStart = source.indexOf('  document.addEventListener("submit"');
  const submitEnd = source.indexOf("\n  });", submitStart);
  const submit = source.slice(submitStart, submitEnd);
  assert.match(submit, /finishVoiceReplyAndIndexTurn\([\s\S]*callReplyConversation === conversation[\s\S]*voiceTtsOwner\.finish/);
});

test("fallback voice replies admit authoritative direct output without a brittle kind tag", () => {
  assert.match(source, /const output = result\?\.result\?\.output \|\| null;/);
  assert.doesNotMatch(
    source,
    /const output = result\?\.result\?\.kind === ["']direct["'] \? result\.result\.output : null;/,
  );
  assert.match(
    source,
    /if \(result\?\.status !== ["']blocked["'] && output\?\.assistant_text\)[\s\S]*voiceTtsOwner\.finish\(output\.assistant_text\)/,
  );
});

test("a completed speech-resume handoff re-arms speculation after a missed silence edge", () => {
  const abortStart = source.indexOf("  const abortCallSpeculation = async");
  const abortEnd = source.indexOf("\n  const confirmCallSpeculation = async", abortStart);
  const abort = source.slice(abortStart, abortEnd);
  assert.match(abort, /if \(replacement && voiceRecording === recording/);
  assert.match(abort, /speculation_handoff_rearmed/);
  assert.match(abort, /recording\.voiceActivity\.voiced[\s\S]*recording\.silentMs < recording\.speculationReleaseMs/);
  assert.match(abort, /prearm: recording\.native/);
});

test("native prefill and retarget failures are retained in the latency trace", () => {
  const captureStart = source.indexOf("  const beginNativeVoiceCapture = async");
  const captureEnd = source.indexOf("\n  const beginVoiceCapture = async", captureStart);
  const capture = source.slice(captureStart, captureEnd);
  assert.match(capture, /audio_prefill_begin_failed/);
  assert.match(capture, /audio_prefill_target_failed/);
  assert.match(capture, /message: errorMessage\(error\)/);
});

test("a visible voice reply control retains its exact PCM controller", () => {
  assert.match(source, /const voiceReplyPlayback = new WeakMap\(\)/);
  assert.match(source, /voiceReplyPlayback\.set\(pendingButton, liveVoiceTts\)/);
  const action = source.slice(
    source.indexOf('"voice-reply-pending-play": async'),
    source.indexOf('"voice-call-open": async'),
  );
  assert.match(action, /const playback = voiceReplyPlayback\.get\(button\)/);
  assert.match(action, /playback\.controller\.playing/);
  assert.match(action, /await playback\.pause\(\)/);
  assert.match(action, /await playback\.play\(\)/);
  assert.doesNotMatch(action, /await liveVoiceTts\.play\(\)/);
});

test("stream playback failure demotes the exact reply instead of leaving a dead play control", () => {
  const live = source.slice(
    source.indexOf("  const beginLiveVoiceTts ="),
    source.indexOf("\n  const renderChatBusyState", source.indexOf("  const beginLiveVoiceTts =")),
  );
  assert.match(live, /onPlaybackError: \(error\) => fail\(error\)/);
  assert.match(live, /voiceReplyPlayback\.delete\(button\)/);
  assert.match(live, /settleUnavailableVoiceReply\(button\)/);
  assert.match(live, /async stop\(\)[\s\S]*voiceReplyPlayback\.delete\(button\)[\s\S]*settleUnavailableVoiceReply\(button\)/);
});

test("a voice reply that fails after first PCM cannot leave a dead play button", () => {
  const replacements = [];
  const row = {
    hidden: true,
    classList: {
      contains: (name) => name === "voice-reply-row",
      remove() {},
    },
    querySelector: (selector) => selector === ".message-card"
      ? { replaceChildren: (node) => replacements.push(node) }
      : null,
  };
  const context = {
    unavailableVoiceMessage: () => ({ state: "unavailable" }),
    button: { closest: () => row },
  };
  vm.createContext(context);
  const start = source.indexOf("  const settleUnavailableVoiceReply =");
  const end = source.indexOf("\n  const restoreReadAloudButton", start);
  vm.runInContext(`${source.slice(start, end)}\nsettleUnavailableVoiceReply(button);`, context);
  assert.deepEqual(replacements, [{ state: "unavailable" }]);
  assert.equal(row.hidden, false);
});

test("a completed voice reply keeps the validated PCM controller as its replay owner", () => {
  const inMemory = source.slice(
    source.indexOf("  const settleInMemoryVoiceReply ="),
    source.indexOf("\n  const restoreReadAloudButton", source.indexOf("  const settleInMemoryVoiceReply =")),
  );
  assert.match(inMemory, /controller\?\.wavBytes\?\.\(\)/);
  assert.match(inMemory, /controller\?\.ready/);
  assert.match(inMemory, /isCanonicalPcmWav\(bytes\)/);
  assert.match(inMemory, /button\.dataset\.audioProof = "pcm-complete"/);
  assert.match(inMemory, /button\.disabled = false/);
  assert.match(inMemory, /in_memory_voice_audio_ready/);
  assert.doesNotMatch(inMemory, /createObjectURL|compactAudioPlayer|voiceReplyPlayback\.delete/);

  const live = source.slice(
    source.indexOf("  const beginLiveVoiceTts ="),
    source.indexOf("\n  const renderChatBusyState", source.indexOf("  const beginLiveVoiceTts =")),
  );
  assert.match(live, /if \(controller\.playing\) durablePending = true/);
  assert.match(live, /else settleInMemoryVoiceReply\(voiceButton, controller, ttsLatencyTrace\)/);
  assert.match(live, /settleInMemoryVoiceReply\(button, controller, ttsLatencyTrace\);[\s\S]*if \(mode === "call"/);
  assert.doesNotMatch(
    live,
    /onPlayable: \(\) => \{[\s\S]{0,500}button\.dataset\.audioState = "ready"/,
    "transient live PCM must not publish a durable-looking play button",
  );
  assert.doesNotMatch(
    live,
    /bindButton\(nextButton\)[\s\S]{0,500}controller\.ready[\s\S]{0,200}audioState/,
    "rebinding a controller with partial PCM must keep the reply pending",
  );
  assert.match(live, /finished\.finally\(\(\) => \{[\s\S]*liveVoiceTts === playback/);
});

test("voice reply chrome derives its glyph from proven audio state", () => {
  const styles = fs.readFileSync(`${__dirname}/style.css`, "utf8");
  assert.doesNotMatch(styles, /\.voice-reply-play::before\s*\{\s*content:\s*"▶"/);
  assert.doesNotMatch(styles, /\.voice-reply-play\[aria-pressed="true"\]::before/);
  assert.match(styles, /\.voice-message-play\[data-audio-state="ready"\]::before/);
  assert.match(styles, /\.voice-message-play\[data-audio-state="pending"\]::before/);
  assert.match(
    styles,
    /voice-reply-pending[\s\S]*\.message-card:not\(:has\(> \.typing-indicator\)\):not\(:has\(> \.voice-message-player\)\)::after/,
    "a pending reply row must retain a typing mark if projection replaces its live child",
  );
});

test("voice and call prompts stay internal through dispatch recovery and clear staged state on success", () => {
  const helperStart = source.indexOf("  const isInternalVoicePrompt =");
  const helperEnd = source.indexOf("  const persistDraftNow =", helperStart);
  assert.ok(helperStart >= 0 && helperEnd > helperStart);
  const context = {};
  vm.createContext(context);
  vm.runInContext(`${source.slice(helperStart, helperEnd)}\nthis.isInternalVoicePrompt = isInternalVoicePrompt;`, context);
  assert.equal(context.isInternalVoicePrompt("[Voice note] private instructions"), true);
  assert.equal(context.isInternalVoicePrompt("[Phone call] private instructions"), true);
  assert.equal(context.isInternalVoicePrompt("Please summarize my recording"), false);

  const submitStart = source.indexOf('  document.addEventListener("submit"');
  const submitEnd = submitStart + source.slice(submitStart).indexOf('\n  });') + 7;
  const submit = source.slice(submitStart, submitEnd);
  assert.ok(submitStart >= 0 && submitEnd > submitStart);
  assert.match(submit, /const voiceDispatch = isInternalVoicePrompt\(message\)/);
  assert.match(submit, /if \(voiceDispatch\) \{\s*form\.querySelector\("#composer-attachments"\)\?\.remove\(\);/);
  assert.match(submit, /if \(!voiceDispatch && selectedConversation\(\) === conversation && textarea && !textarea\.value\) textarea\.value = message/);
  assert.equal(
    (submit.match(/if \(voiceDispatch\) \{\s*await updateComposerDraft\(\{\s*conversation,\s*message: "",\s*attachmentIds: \[\],/g) || []).length,
    2,
    "successful or blocked results and exceptions both clear the internal voice draft",
  );
  assert.match(submit, /if \(!voiceDispatch && result\?\.status === "blocked"[\s\S]*?\n          if \(voiceDispatch\) \{\s*await updateComposerDraft\(/);

  const bootStart = source.indexOf("  const boot = async () => {");
  const bootEnd = source.indexOf("\n  boot();", bootStart);
  const boot = source.slice(bootStart, bootEnd);
  assert.ok(bootStart >= 0 && bootEnd > bootStart);
  assert.match(boot, /dataset\.internalVoiceDraft !== undefined/);
  assert.match(boot, /if \(interruptedVoiceDispatch\) \{\s*document\.getElementById\("chat-form"\)\?\.setAttribute\("hidden", ""\);[\s\S]*?message: "",\s*attachmentIds: \[\],[\s\S]*?invokeMarkup\("mom_llama_render_app"\)/);
});

test("chat switch projection skips model catalog discovery on the hot path", () => {
  const view = fs.readFileSync(`${__dirname}/../src-tauri/src/view.rs`, "utf8");
  const start = view.indexOf("pub fn render_chat_fragment(");
  const end = view.indexOf("\npub fn ", start + 1);
  const fragment = view.slice(start, end);
  assert.ok(start >= 0 && end > start);
  assert.doesNotMatch(fragment, /mom_llama_runtime::model_list\(scope\)/);
  assert.match(fragment, /"not_requested"[\s\S]*Vec::<ModelInfo>::new\(\)/);
});

test("runtime warm begins as soon as the authoritative shell is restored", () => {
  const bootStart = source.indexOf("  const boot = async () => {");
  const bootEnd = source.indexOf("\n  boot();", bootStart);
  const boot = source.slice(bootStart, bootEnd);
  const finalInterruptedRender = boot.lastIndexOf('root.innerHTML = await invokeMarkup("mom_llama_render_app")');
  const warm = boot.indexOf('invoke("mom_llama_runtime_warm")', finalInterruptedRender);
  const hydration = boot.indexOf("await hydrateAttachmentPreviews(root)", warm);
  assert.ok(finalInterruptedRender >= 0 && finalInterruptedRender < warm);
  assert.ok(warm < hydration, "model restoration must start before attachment hydration");
  assert.doesNotMatch(boot.slice(warm, hydration), /setTimeout|requestIdleCallback/);
});

test("a send rejection remains visible while the user repairs or retries it", () => {
  const classes = new Set(["is-hidden"]);
  const timers = [];
  const status = { classList: {
    add: (c) => classes.add(c), remove: (c) => classes.delete(c),
    toggle: (c, enabled) => enabled ? classes.add(c) : classes.delete(c),
  }, replaceChildren(...children) { this.children = children; } };
  const context = {
    document: {
      getElementById: () => status,
      createElement: (tag) => ({ tag, className: "", textContent: "", setAttribute() {}, addEventListener() {} }),
    }, recordCommandResult() {},
    window: { clearTimeout() {}, setTimeout: (callback) => { timers.push(callback); return timers.length; } },
  };
  vm.createContext(context);
  const helperStart = source.indexOf("  const renderStatusContent = (status, label, onDismiss) => {");
  const helperEnd = source.indexOf("\n  const report =", helperStart);
  const start = source.indexOf("  const report =");
  const end = source.indexOf("\n  const reportError =", start);
  vm.runInContext(`${source.slice(helperStart, helperEnd)}\n${source.slice(start, end)}\nthis.report = report;`, context);
  context.report({ status: "blocked", blocker: { message: "The model rejected this request" } });
  assert.equal(status.children[0].textContent, "The model rejected this request");
  assert.equal(classes.has("is-hidden"), false);
  timers.forEach((timer) => timer());
  assert.equal(status.children.length, 0);
  assert.equal(classes.has("is-hidden"), true);
});
