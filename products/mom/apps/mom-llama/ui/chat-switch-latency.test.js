"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const source = fs.readFileSync(`${__dirname}/coop-hx.js`, "utf8");

const definition = (name, nextName) => {
  const start = source.indexOf(`  const ${name} =`);
  assert.ok(start >= 0, `${name} must remain directly testable`);
  const end = source.indexOf(`  const ${nextName} =`, start + 1);
  assert.ok(end > start, `${nextName} must follow ${name}`);
  return source.slice(start, end);
};

test("chat projection returns before attachment hydration can settle", async () => {
  const frames = [];
  let hydrated = false;
  const chat = { dataset: {}, querySelector: () => null };
  const context = {
    cancelComposerAutocomplete() {},
    captureChatViewport: () => null,
    swap: async () => chat,
    hydrateAttachmentPreviews: () => {
      hydrated = true;
      return new Promise(() => {});
    },
    reportError(error) { throw error; },
    renderChatBusyState() {},
    restoreChatViewport() {},
    requestAnimationFrame: (callback) => frames.push(callback),
  };
  vm.createContext(context);
  vm.runInContext(`${definition("refreshChat", "collapsedSidebarSections")}\nthis.refreshChat = refreshChat;`, context);

  assert.equal(await context.refreshChat(), chat);
  assert.equal(hydrated, false);
  frames.shift()();
  frames.shift()();
  assert.equal(hydrated, true);
});

test("conversation selection measures completed paint and does not await secondary projections", async () => {
  const frames = [];
  const pending = new Promise(() => {});
  const measures = [];
  const timingReports = [];
  let now = 100;
  const context = {
    performance: { now: () => now, measure: (...args) => measures.push(args) },
    requestAnimationFrame: (callback) => frames.push(callback),
    retainComposerDraft: async () => {},
    invoke: async (command, args) => {
      if (command === "mom_llama_navigation_timing_record") timingReports.push(args.timing);
      return { status: "passed" };
    },
    report() {},
    refreshChat: async () => ({ dataset: { currentConversation: "next" } }),
    refreshSidebar: () => pending,
    refreshConversationSettingsProjection: () => pending,
    recoverPendingVoiceHistory: async () => {},
    reportError() {},
  };
  vm.createContext(context);
  vm.runInContext(
    `let conversationSwitchTimingId = 0;\n${definition("recordConversationSwitchTiming", "recordConversationSwitchPaint")}${definition("recordConversationSwitchPaint", "formField")}let conversationNavigationRevision = 0; let conversationMutation = Promise.resolve();\n${definition("mutateConversationSelection", "selectConversation")}${definition("selectConversation", "openPersonaFreeze")}\nthis.selectConversation = selectConversation;`,
    context,
  );

  assert.deepEqual(await context.selectConversation("next"), { status: "passed" });
  assert.equal(frames.length, 1);
  assert.deepEqual(measures.map(([name]) => name), [
    "mom.conversation.switch.select-queue.1",
    "mom.conversation.switch.select-command.1",
    "mom.conversation.switch.chat-render.1",
  ]);
  now = 125;
  frames.shift()();
  frames.shift()();
  assert.equal(measures.length, 4);
  assert.equal(measures[3][0], "mom.conversation.switch-to-paint.1");
  assert.equal(measures[3][1].start, 100);
  assert.equal(measures[3][1].end, 125);
  await Promise.resolve();
  assert.deepEqual(JSON.parse(JSON.stringify(timingReports)), [{
    switchId: 1,
    selectionQueueMs: 0,
    selectionCommandMs: 0,
    chatRenderMs: 0,
    switchToPaintMs: 25,
  }]);
});

test("conversation settings refresh stays idle while hidden and refreshes when visible", async () => {
  let currentConversation = "next";
  let refreshed = 0;
  const oldInstructions = { replaceWith(next) { this.replacedWith = next; } };
  const settings = {
    hidden: true,
    dataset: { currentConversation: "previous" },
    querySelector: () => oldInstructions,
  };
  const projectedInstructions = {};
  const projection = {
    dataset: { currentConversation: "next" },
    querySelector: () => projectedInstructions,
  };
  const context = {
    document: { getElementById: () => settings },
    selectedConversation: () => currentConversation,
    invokeMarkup: async () => { refreshed += 1; return "projection"; },
    parseFragment: () => projection,
  };
  vm.createContext(context);
  vm.runInContext(
    `${definition("refreshConversationSettingsProjection", "refreshConversationProjection")}\nthis.refreshConversationSettingsProjection = refreshConversationSettingsProjection;`,
    context,
  );

  await context.refreshConversationSettingsProjection();
  assert.equal(refreshed, 0);

  settings.hidden = false;
  await context.refreshConversationSettingsProjection();
  assert.equal(refreshed, 1);
  assert.equal(oldInstructions.hidden, true, "stale instructions cannot be edited during refresh");
  assert.equal(oldInstructions.replacedWith, projectedInstructions);
  assert.equal(settings.dataset.currentConversation, currentConversation);
});

test("conversation selections serialize only their mutation and skip stale projection work", async () => {
  const releases = [];
  const selected = [];
  const context = {
    performance: { now: () => 0, measure() {} },
    requestAnimationFrame() {},
    retainComposerDraft: async () => {},
    invoke: (_command, { conversation }) => new Promise((resolve) => {
      selected.push(conversation);
      releases.push(() => resolve({ status: "passed" }));
    }),
    report() {},
    refreshChat: async () => ({ dataset: { currentConversation: selected.at(-1) } }),
    refreshSidebar: async () => {},
    refreshConversationSettingsProjection: async () => {},
    reportError(error) { throw error; },
  };
  vm.createContext(context);
  vm.runInContext(
    `let conversationSwitchTimingId = 0;\n${definition("recordConversationSwitchTiming", "recordConversationSwitchPaint")}${definition("recordConversationSwitchPaint", "formField")}let conversationNavigationRevision = 0; let conversationMutation = Promise.resolve();\n${definition("mutateConversationSelection", "selectConversation")}${definition("selectConversation", "openPersonaFreeze")}\nthis.selectConversation = selectConversation;`,
    context,
  );

  const first = context.selectConversation("first");
  const second = context.selectConversation("second");
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(selected, ["first"]);
  assert.equal(releases.length, 1);
  releases.shift()();
  await first;
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(selected, ["first", "second"]);
  assert.equal(releases.length, 1);
  releases.shift()();
  await second;
  assert.deepEqual(selected, ["first", "second"]);
});

test("composer draft writes serialize captured payloads and recover after a failed write", async () => {
  const submitted = [];
  let rejectFirst;
  let resolveSecond;
  const context = {
    invoke(command, payload) {
      assert.equal(command, "mom_llama_draft_update");
      submitted.push(JSON.parse(JSON.stringify(payload)));
      return new Promise((resolve, reject) => {
        if (submitted.length === 1) rejectFirst = () => reject(new Error("first draft write failed"));
        if (submitted.length === 2) resolveSecond = () => resolve({ status: "passed" });
      });
    },
  };
  vm.createContext(context);
  vm.runInContext(
    `let conversationMutation = Promise.resolve();\n${definition("mutateConversationSelection", "selectConversation")}${definition("updateComposerDraft", "persistDraftNow")}\nthis.updateComposerDraft = updateComposerDraft;`,
    context,
  );

  const firstPayload = { conversation: "first", message: "first text", attachmentIds: ["a"] };
  const first = context.updateComposerDraft(firstPayload);
  firstPayload.conversation = "changed";
  firstPayload.message = "changed text";
  firstPayload.attachmentIds.push("b");
  const second = context.updateComposerDraft({
    conversation: "second",
    message: "second text",
    attachmentIds: [],
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(submitted, [{ conversation: "first", message: "first text", attachmentIds: ["a"] }]);

  rejectFirst();
  await assert.rejects(first, /first draft write failed/);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(submitted, [
    { conversation: "first", message: "first text", attachmentIds: ["a"] },
    { conversation: "second", message: "second text", attachmentIds: [] },
  ]);
  resolveSecond();
  assert.deepEqual(await second, { status: "passed" });
});

test("draft retention and rapid selections share one FIFO in click order", async () => {
  const calls = [];
  let releaseFirstDraft;
  let draftText = "first visible draft";
  const context = {
    performance: { now: () => 1, measure() {} },
    requestAnimationFrame() {},
    retainComposerDraft() {
      return context.updateComposerDraft({
        conversation: "visible-conversation",
        message: draftText,
        attachmentIds: [],
      });
    },
    invoke(command, payload) {
      calls.push({ command, payload: JSON.parse(JSON.stringify(payload)) });
      if (command === "mom_llama_draft_update" && calls.filter((call) => call.command === command).length === 1) {
        return new Promise((resolve) => { releaseFirstDraft = () => resolve({ status: "passed" }); });
      }
      return Promise.resolve({ status: "passed" });
    },
    report() {},
    refreshChat: async () => ({ dataset: { currentConversation: "second" } }),
    refreshSidebar: async () => {},
    refreshConversationSettingsProjection: async () => {},
    recoverPendingVoiceHistory: async () => {},
    reportError(error) { throw error; },
  };
  vm.createContext(context);
  vm.runInContext(
    `let conversationMutation = Promise.resolve();\nlet conversationNavigationRevision = 0;\nlet conversationSwitchTimingId = 0;\n${definition("mutateConversationSelection", "selectConversation")}${definition("recordConversationSwitchTiming", "recordConversationSwitchPaint")}${definition("recordConversationSwitchPaint", "formField")}${definition("updateComposerDraft", "persistDraftNow")}${definition("selectConversation", "openPersonaFreeze")}\nthis.updateComposerDraft = updateComposerDraft; this.selectConversation = selectConversation;`,
    context,
  );

  const first = context.selectConversation("first");
  draftText = "second visible draft";
  const second = context.selectConversation("second");
  draftText = "later edit";
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls.map((call) => call.command), ["mom_llama_draft_update"]);

  releaseFirstDraft();
  await Promise.all([first, second]);
  assert.deepEqual(calls, [
    {
      command: "mom_llama_draft_update",
      payload: { conversation: "visible-conversation", message: "first visible draft", attachmentIds: [] },
    },
    {
      command: "mom_llama_conversation_select",
      payload: { conversation: "first" },
    },
    {
      command: "mom_llama_draft_update",
      payload: { conversation: "visible-conversation", message: "second visible draft", attachmentIds: [] },
    },
    {
      command: "mom_llama_conversation_select",
      payload: { conversation: "second" },
    },
  ]);
});

test("a blocked draft retention stops conversation navigation", async () => {
  const commands = [];
  const reports = [];
  const form = { dataset: { busy: "false" } };
  const context = {
    window: { clearTimeout() {} },
    document: { getElementById: () => form },
    formField: () => ({ value: "unsaved draft" }),
    draftAttachmentIds: () => [],
    selectedConversation: () => "current",
    report: (result) => reports.push(result),
    performance: { now: () => 1, measure() {} },
    requestAnimationFrame() {},
    invoke: async (command) => {
      commands.push(command);
      return { status: "blocked", blocker: { message: "draft store is unavailable" } };
    },
    refreshChat: async () => ({ dataset: { currentConversation: "next" } }),
    refreshSidebar: async () => {},
    refreshConversationSettingsProjection: async () => {},
    recoverPendingVoiceHistory: async () => {},
    reportError(error) { throw error; },
  };
  vm.createContext(context);
  vm.runInContext(
    `let conversationMutation = Promise.resolve();\nlet conversationNavigationRevision = 0;\nlet conversationSwitchTimingId = 0;\nlet draftTimer = null;\n${definition("mutateConversationSelection", "selectConversation")}${definition("recordConversationSwitchTiming", "recordConversationSwitchPaint")}${definition("recordConversationSwitchPaint", "formField")}${definition("updateComposerDraft", "persistDraftNow")}${definition("persistDraftNow", "scheduleDraft")}${definition("retainComposerDraft", "openNewDraft")}${definition("selectConversation", "openPersonaFreeze")}\nthis.selectConversation = selectConversation;`,
    context,
  );

  await assert.rejects(context.selectConversation("next"), /draft store is unavailable/);
  assert.deepEqual(commands, ["mom_llama_draft_update"]);
  assert.equal(reports.length, 1);
});
