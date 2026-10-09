"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const source = fs.readFileSync(`${__dirname}/coop-hx.js`, "utf8");
const start = source.indexOf("  let recipientMutation =");
const end = source.indexOf("  const actionHandlers =", start);

for (const navigate of [false, true]) {
  test(`recipient update ${navigate ? "refuses a stale selection" : "keeps draft text out of recipient routing"}`, async () => {
    let active = {};
    let selected = "default";
    const calls = [];
    const context = {
      document: {
        addEventListener() {},
        querySelectorAll: () => [{ dataset: { recipientId: "stable-contact-id" } }],
        getElementById: (id) => id === "recipient-group-name" ? { value: "Friends" } : null,
      },
      chat: () => active,
      selectedConversation: () => selected,
      retainComposerDraft: async () => {
        calls.push(["persist draft"]);
        if (navigate) { active = {}; selected = "saved-chat"; }
      },
      invoke: async (command, input) => { calls.push([command, input]); return { status: "passed" }; },
      report() {},
      refreshChat: async () => calls.push(["refresh"]),
      reportError(error) { throw error; },
    };
    vm.createContext(context);
    vm.runInContext(`${source.slice(start, end)}\nthis.update = updateRecipients;`, context);
    await context.update((ids) => [...ids, "second-stable-id"]);
    const updates = calls.filter(([command]) => command === "mom_llama_conversation_draft_recipients_update");
    assert.equal(updates.length, navigate ? 0 : 1);
    if (!navigate) {
      assert.deepEqual(JSON.parse(JSON.stringify(updates[0][1])), {
        recipientIds: ["stable-contact-id", "second-stable-id"], name: "Friends",
      });
    }
    assert.equal(calls.filter(([command]) => command === "refresh").length, navigate ? 0 : 1);
  });
}
