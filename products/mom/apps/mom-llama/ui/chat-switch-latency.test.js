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
  let now = 100;
  const context = {
    performance: { now: () => now, measure: (...args) => measures.push(args) },
    requestAnimationFrame: (callback) => frames.push(callback),
    retainComposerDraft: async () => {},
    invoke: async () => ({ status: "passed" }),
    report() {},
    refreshChat: async () => ({ dataset: { currentConversation: "next" } }),
    refreshSidebar: () => pending,
    refreshConversationSettingsProjection: () => pending,
    recoverPendingVoiceHistory: async () => {},
    reportError() {},
  };
  vm.createContext(context);
  vm.runInContext(
    `let conversationSwitchTimingId = 0;\n${definition("recordConversationSwitchPaint", "formField")}let conversationNavigationRevision = 0; let conversationMutation = Promise.resolve();\n${definition("mutateConversationSelection", "selectConversation")}${definition("selectConversation", "openPersonaFreeze")}\nthis.selectConversation = selectConversation;`,
    context,
  );

  assert.deepEqual(await context.selectConversation("next"), { status: "passed" });
  assert.equal(frames.length, 1);
  assert.equal(measures.length, 0);
  now = 125;
  frames.shift()();
  frames.shift()();
  assert.equal(measures.length, 1);
  assert.equal(measures[0][0], "mom.conversation.switch-to-paint.1");
  assert.equal(measures[0][1].start, 100);
  assert.equal(measures[0][1].end, 125);
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
    `let conversationSwitchTimingId = 0;\n${definition("recordConversationSwitchPaint", "formField")}let conversationNavigationRevision = 0; let conversationMutation = Promise.resolve();\n${definition("mutateConversationSelection", "selectConversation")}${definition("selectConversation", "openPersonaFreeze")}\nthis.selectConversation = selectConversation;`,
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
