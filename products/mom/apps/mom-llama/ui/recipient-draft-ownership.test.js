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

test("recipient filtering accepts the visible @handle form", () => {
  const input = {
    value: "@ad",
    attributes: {},
    setAttribute(name, value) { this.attributes[name] = value; },
    removeAttribute(name) { delete this.attributes[name]; },
  };
  const matching = { dataset: { search: "ad de jongh ad", recipientName: "ad de jongh", recipientHandle: "ad" }, hidden: true };
  const other = { dataset: { search: "robert miller feeling-state addiction protocol robert", recipientName: "robert miller (feeling-state addiction protocol)", recipientHandle: "robert" }, hidden: true };
  const list = {
    hidden: true,
    querySelectorAll: () => [matching, other],
  };
  const context = {
    document: {
      addEventListener() {},
      getElementById: (id) => id === "recipient-query" ? input : id === "recipient-options" ? list : null,
      querySelectorAll: () => [matching, other],
    },
  };
  const filterStart = source.indexOf("  const normalizedRecipientQuery =");
  const filterEnd = source.indexOf("  document.addEventListener(\"input\"", filterStart);
  vm.createContext(context);
  vm.runInContext(`${source.slice(filterStart, filterEnd)}\nthis.filter = filterRecipientOptions;`, context);

  context.filter();

  assert.equal(list.hidden, false);
  assert.equal(matching.hidden, false);
  assert.equal(other.hidden, true);
  assert.equal(input.attributes["aria-expanded"], "true");
});

test("a unique exact recipient name or handle can be committed by send", () => {
  const ad = { dataset: { recipient: "persona-ad", recipientName: "ad de jongh", recipientHandle: "ad" } };
  const janina = { dataset: { recipient: "persona-janina", recipientName: "janina fisher", recipientHandle: "janina" } };
  const context = {
    document: { querySelectorAll: () => [ad, janina] },
  };
  const start = source.indexOf("  const normalizedRecipientQuery =");
  const end = source.indexOf("  const filterRecipientOptions =", start);
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}\nthis.exact = exactRecipientOption;`, context);

  assert.equal(context.exact("Ad de Jongh"), ad);
  assert.equal(context.exact("@ad"), ad);
  assert.equal(context.exact("Ad"), ad);
  assert.equal(context.exact("unknown"), null);
});
