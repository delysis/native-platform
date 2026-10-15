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
  const makeOption = (dataset) => ({
    dataset, hidden: true, attributes: {},
    setAttribute(name, value) { this.attributes[name] = value; },
    getAttribute(name) { return this.attributes[name] ?? null; },
  });
  const matching = makeOption({ search: "ad de jongh ad", recipientName: "ad de jongh", recipientHandle: "ad" });
  const other = makeOption({ search: "robert miller feeling-state addiction protocol robert", recipientName: "robert miller (feeling-state addiction protocol)", recipientHandle: "robert" });
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

test("recipient options expose a selected default and keyboard selection clicks it", () => {
  const listeners = new Map();
  const makeOption = (name, handle) => ({
    id: "", hidden: false, attributes: new Map(), clicked: 0,
    dataset: { recipientName: name, recipientHandle: handle },
    setAttribute(key, value) { this.attributes.set(key, value); },
    getAttribute(key) { return this.attributes.get(key) ?? null; },
    click() { this.clicked += 1; },
    closest(selector) { return selector === "#recipient-options .recipient-option" ? this : null; },
  });
  const options = [makeOption("ad de jongh", "ad"), makeOption("arnold popky", "arnold")];
  const input = {
    id: "recipient-query", value: "@a", attributes: new Map(),
    setAttribute(key, value) { this.attributes.set(key, value); },
    getAttribute(key) { return this.attributes.get(key) ?? null; },
    removeAttribute(key) { this.attributes.delete(key); },
    matches() { return false; },
  };
  const list = { hidden: true, querySelectorAll: () => options };
  const context = {
    document: {
      addEventListener(name, handler) {
        const handlers = listeners.get(name) || [];
        handlers.push(handler);
        listeners.set(name, handlers);
      },
      querySelectorAll: () => options,
      getElementById: (id) => id === "recipient-query" ? input : id === "recipient-options" ? list : null,
    },
  };
  const start = source.indexOf("  const normalizedRecipientQuery =");
  const end = source.indexOf("  const actionHandlers =", start);
  vm.createContext(context);
  vm.runInContext(`${source.slice(start, end)}`, context);
  const dispatch = (name, event) => listeners.get(name).forEach((handler) => handler(event));
  dispatch("input", { target: input, isComposing: false });
  assert.equal(list.hidden, false);
  assert.equal(options[0].getAttribute("aria-selected"), "true");
  assert.equal(options[1].getAttribute("aria-selected"), "false");

  let prevented = false;
  dispatch("keydown", { target: input, key: "ArrowDown", preventDefault() { prevented = true; } });
  assert.equal(prevented, true);
  assert.equal(options[0].getAttribute("aria-selected"), "false");
  assert.equal(options[1].getAttribute("aria-selected"), "true");
  dispatch("keydown", { target: input, key: "Tab", preventDefault() { prevented = true; } });
  assert.equal(options[1].clicked, 1);

  prevented = false;
  dispatch("pointerdown", { target: options[0], preventDefault() { prevented = true; } });
  assert.equal(prevented, true);
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
