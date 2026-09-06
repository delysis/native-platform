"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const source = fs.readFileSync(`${__dirname}/coop-hx.js`, "utf8");

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
        document: { addEventListener: (_, handler) => { submit = handler; } },
        autocompleteAccepting: false, cancelComposerAutocomplete() {},
        formValue: () => textarea.value, draftAttachmentIds: () => [],
        acquireChatBusy: () => "lease", releaseChatBusy() {},
        selectedConversation: () => selected, selectedConversationKind: () => "chat",
        formField: () => textarea, closeMentions() {}, appendLiveMessage() {},
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
