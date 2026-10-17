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
  assert.ok(open.indexOf("await prepareVoicePersonaForCall") < open.indexOf("await startStreamedCallGreeting"));
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
      vm.runInContext(`const isInternalVoicePrompt = (message) => message.startsWith("[Voice note]") || message.startsWith("[Phone call]");\nlet activeDispatchConversation = null;\nlet pendingVoiceDispatchPrompt = null;\nlet pendingVoiceTtsOwner = null;\n${source.slice(start, end)}\nthis.cancel = ${source.match(/"chat-cancel": (async \(\) =>[^\n]+)/)[1].replace(/,$/, "")};`, context);
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
    const released = [];
    const textarea = { value: "Hello" };
    const context = {
      document: { addEventListener: (_, handler) => { submit = handler; }, getElementById: () => null, querySelectorAll: () => [] },
      recipientMutation: Promise.resolve(), activeDispatchStreamLeases: leases,
      autocompleteAccepting: false, cancelComposerAutocomplete() {},
      formValue: () => textarea.value, draftAttachmentIds: () => [],
      acquireChatBusy: () => "dispatch", releaseChatBusy: (lease) => released.push(lease),
      suspendVoiceHistoryRecoveryForForeground: async () => () => {},
      selectedConversation: () => "A", selectedConversationKind: () => "chat",
      formField: () => textarea, closeMentions() {}, appendLiveMessage() {}, sizeComposer() {},
      report() {}, reportError(error) { assert.equal(error.message, "rejected"); },
      releaseMentionInvocationBusy() {}, refreshChat: async () => {}, refreshConversationProjection: async () => {},
      persistDraftNow: async () => {},
      invoke: async () => {
        leases.add("mention-request:failed:contact");
        if (outcome === "throw") throw new Error("rejected");
        return { status: "blocked" };
      },
    };
    vm.createContext(context);
    const start = source.indexOf('  document.addEventListener("submit"');
    const end = start + source.slice(start).indexOf('\n  });') + 7;
    vm.runInContext(`const isInternalVoicePrompt = (message) => message.startsWith("[Voice note]") || message.startsWith("[Phone call]");\nlet activeDispatchConversation = null;\nlet voiceReplyConversation = null;\nlet callReplyConversation = null;\nlet pendingVoiceDispatchPrompt = null;\nlet pendingVoiceTtsOwner = null;\n${source.slice(start, end)}`, context);
    await submit({ preventDefault() {}, target: { id: "chat-form", dataset: {} } });
    assert.deepEqual(released, ["mention-request:failed:contact", "dispatch"]);
    assert.equal(leases.size, 0);
    assert.equal(textarea.value, "Hello");
  });
}

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
  assert.match(source, /voiceNoteComplete = true;\s*const voiceButton = mountVoiceReplyPlayer\(\)/);
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
  assert.match(speculate, /const synthesized = await tts\.finish\(response\.result\.assistant_text \|\| tts\.text\)/);
  assert.match(speculate, /if \(!synthesized\)[\s\S]*voice_reply_incomplete/);

  const greetingStart = source.indexOf("  const startStreamedCallGreeting = async");
  const greetingEnd = source.indexOf("\n  const openVoiceCallSurface = async", greetingStart);
  const greeting = source.slice(greetingStart, greetingEnd);
  assert.match(greeting, /const synthesized = await tts\.finish\(greeting\)/);
  assert.match(greeting, /if \(!synthesized\)[\s\S]*voice could not be produced/);
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
  const durable = source.slice(
    source.indexOf("  const settleDurableVoiceReply ="),
    source.indexOf("\n  const restoreReadAloudButton", source.indexOf("  const settleDurableVoiceReply =")),
  );
  assert.match(durable, /controller\?\.wavBytes\?\.\(\)/);
  assert.match(durable, /controller\?\.ready/);
  assert.match(durable, /isCanonicalPcmWav\(bytes\)/);
  assert.match(durable, /button\.dataset\.audioProof = "pcm-complete"/);
  assert.match(durable, /button\.disabled = false/);
  assert.match(durable, /durable_voice_audio_ready/);
  assert.doesNotMatch(durable, /createObjectURL|compactAudioPlayer|voiceReplyPlayback\.delete/);

  const live = source.slice(
    source.indexOf("  const beginLiveVoiceTts ="),
    source.indexOf("\n  const renderChatBusyState", source.indexOf("  const beginLiveVoiceTts =")),
  );
  assert.match(live, /if \(controller\.playing\) durablePending = true/);
  assert.match(live, /else settleDurableVoiceReply\(voiceButton, controller\)/);
  assert.match(live, /settleDurableVoiceReply\(button, controller\);[\s\S]*if \(mode === "call"/);
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
    (submit.match(/if \(voiceDispatch\) \{\s*await invoke\("mom_llama_draft_update", \{\s*conversation,\s*message: "",\s*attachmentIds: \[\],/g) || []).length,
    2,
    "successful or blocked results and exceptions both clear the internal voice draft",
  );
  assert.match(submit, /if \(!voiceDispatch && result\?\.status === "blocked"[\s\S]*?\n          if \(voiceDispatch\) \{\s*await invoke\("mom_llama_draft_update"/);

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
  const start = source.indexOf("  const report =");
  const end = source.indexOf("\n  const reportError =", start);
  vm.runInContext(`${source.slice(start, end)}\nthis.report = report;`, context);
  context.report({ status: "blocked", blocker: { message: "The model rejected this request" } });
  assert.equal(status.children[0].textContent, "The model rejected this request");
  assert.equal(classes.has("is-hidden"), false);
  timers.forEach((timer) => timer());
  assert.equal(status.children.length, 0);
  assert.equal(classes.has("is-hidden"), true);
});
