"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const source = fs.readFileSync(`${__dirname}/coop-hx.js`, "utf8");

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
      vm.runInContext(`let activeDispatchConversation = null;\n${source.slice(source.indexOf('  document.addEventListener("submit"'), source.indexOf('  document.addEventListener("submit"') + source.slice(source.indexOf('  document.addEventListener("submit"')).indexOf('\n  });') + 7)}\nthis.cancel = ${source.match(/"chat-cancel": (async \(\) =>[^\n]+)/)[1].replace(/,$/, "")};`, context);
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
    vm.runInContext(`let activeDispatchConversation = null;\nlet voiceReplyConversation = null;\nlet callReplyConversation = null;\n${source.slice(start, end)}`, context);
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
  };
  vm.createContext(context);
  const start = source.indexOf("  const chatRequestLease =");
  const end = source.indexOf("\n  const mentionLiveId", start);
  vm.runInContext(`let voiceLatencyTrace = null;\nconst markVoiceLatency = () => {};\nconst startVoiceLatency = () => { voiceLatencyTrace = {}; };\n${source.slice(start, end)}\nglobalThis.testOnChatEvent = onChatEvent;`, context);
  context.testOnChatEvent({ payload: { event: "started", request_id: "r1" } });
  context.testOnChatEvent({ payload: { event: "delta", request_id: "r1", delta: "hidden assistant text" } });
  context.testOnChatEvent({ payload: { event: "reasoning_delta", request_id: "r1", delta: "hidden reasoning" } });
  context.testOnChatEvent({ payload: { event: "failed", request_id: "r1" } });
  assert.deepEqual(events, []);
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
  };
  vm.createContext(context);
  const start = source.indexOf("  const chatRequestLease =");
  const end = source.indexOf("\n  const mentionLiveId", start);
  vm.runInContext(`let voiceLatencyTrace = null;\nconst markVoiceLatency = () => {};\nconst startVoiceLatency = () => { voiceLatencyTrace = {}; };\n${source.slice(start, end)}\nglobalThis.testOnChatEvent = onChatEvent;`, context);
  context.testOnChatEvent({ payload: { event: "started", request_id: "r-call" } });
  context.testOnChatEvent({ payload: { event: "delta", request_id: "r-call", delta: "hidden reply" } });
  context.testOnChatEvent({ payload: { event: "reasoning_delta", request_id: "r-call", delta: "hidden reasoning" } });
  context.testOnChatEvent({ payload: { event: "completed", request_id: "r-call" } });
  assert.deepEqual(events, [
    ["acquire", "chat-request:r-call"],
    ["finish", null],
    ["release", "chat-request:r-call"],
  ]);
});

test("voice mention replies keep streamed text and cards hidden", () => {
  const rowClasses = [];
  const waveformNode = { kind: "waveform" };
  const playButton = { setAttribute() {} };
  const card = { replaceChildren(...children) { this.children = children; }, closest: () => row };
  const row = { classList: { add: (...classes) => rowClasses.push(...classes) } };
  const voiceCardContext = {
    DYNAMIC_CONTROL_SPECS: { readAloud: {} },
    appendLiveMessage: () => card,
    createCommandElement: () => playButton,
    waveform: () => waveformNode,
  };
  vm.createContext(voiceCardContext);
  const voiceStart = source.indexOf("  const appendLiveVoiceReply =");
  const voiceEnd = source.indexOf("\n  const keepLiveTailVisible", voiceStart);
  vm.runInContext(`${source.slice(voiceStart, voiceEnd)}\nthis.makeVoiceReply = appendLiveVoiceReply;`, voiceCardContext);
  voiceCardContext.makeVoiceReply("live-mention-i-p");
  assert.deepEqual(rowClasses, ["voice-reply-row", "voice-reply-pending"]);
  assert.equal(card.children[1], waveformNode);
  assert.equal(playButton.disabled, true);

  const calls = [];
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
    document: { getElementById: (id) => id.startsWith("live-mention-") ? mentionRow : null },
    finishLiveMessage() {}, releaseChatBusy() {},
  };
  vm.createContext(dispatchContext);
  const dispatchStart = source.indexOf("  const onDispatchEvent =");
  const dispatchEnd = source.indexOf("\n  let mentionSearchSerial", dispatchStart);
  vm.runInContext(`${source.slice(dispatchStart, dispatchEnd)}\nthis.dispatch = onDispatchEvent;`, dispatchContext);
  dispatchContext.dispatch({ kind: "mention", event: { event: "started", invocation_id: "i", target_id: "p" } });
  dispatchContext.dispatch({ kind: "mention", event: { event: "delta", invocation_id: "i", target_id: "p", delta: "hidden text" } });
  assert.deepEqual(calls, []);
});

test("runtime warm waits for paint and idle time before touching the model", () => {
  const listen = source.indexOf("await listen();");
  const firstPaint = source.indexOf("requestAnimationFrame", listen);
  const secondPaint = source.indexOf("requestAnimationFrame", firstPaint + 1);
  const idle = source.indexOf("requestIdleCallback", secondPaint);
  const warm = source.indexOf('invoke("mom_llama_runtime_warm")', listen);
  assert.ok(listen >= 0 && listen < firstPaint);
  assert.ok(firstPaint < secondPaint && secondPaint < idle);
  assert.ok(warm > listen);
  assert.match(source.slice(listen, idle + 200), /setTimeout\([\s\S]*?, 1000\)/);
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
