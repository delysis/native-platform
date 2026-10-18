const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");

class FakeElement {
  constructor(dataset = {}) {
    this.dataset = dataset;
    this.children = [];
    this.hidden = false;
    this.style = {};
    this.isConnected = true;
    this.listeners = new Map();
    this.classNames = new Set();
    this.classList = {
      add: (name) => this.classNames.add(name),
      remove: (name) => this.classNames.delete(name),
      contains: (name) => this.classNames.has(name),
    };
  }
  setAttribute(name, value) { this[name] = value; }
  setCustomValidity(value) { this.validityMessage = value; }
  reportValidity() { this.reportedValidity = true; }
  addEventListener(name, fn) { this.listeners.set(name, fn); }
  fire(name, event = {}) { this.listeners.get(name)?.({ preventDefault() {}, ...event }); }
  focus() { this.ownerDocument.activeElement = this; }
  blur() {
    if (this.ownerDocument?.activeElement === this) this.ownerDocument.activeElement = null;
    this.fire("blur");
  }
  select() { this.selected = true; }
  remove() {
    if (this.parentNode) this.parentNode.children = this.parentNode.children.filter((child) => child !== this);
    this.parentNode = null;
    if (this.ownerDocument?.activeElement === this) this.ownerDocument.activeElement = null;
  }
  insertBefore(child, next) {
    const index = next ? this.children.indexOf(next) : -1;
    child.parentNode = this;
    if (index < 0) this.children.push(child);
    else this.children.splice(index, 0, child);
  }
  append(child) {
    child.parentNode = this;
    this.children.push(child);
  }
  getBoundingClientRect() { return this.rect || { left: 0, top: 0, width: 260, height: 34 }; }
  get nextSibling() {
    if (!this.parentNode) return null;
    return this.parentNode.children[this.parentNode.children.indexOf(this) + 1] || null;
  }
  closest(selector) {
    return selector.includes("data-conversation-menu-target") ? this.row || this : null;
  }
  querySelector(selector) {
    if (selector === '[data-action="conversation-select"]') return this.button || null;
    if (selector === ".conversation-title") return this.title || null;
    return null;
  }
}

const makeHarness = ({ invoke = async () => ({ status: "passed" }) } = {}) => {
  const document = {
    activeElement: null,
    createElement: () => {
      const input = new FakeElement();
      input.ownerDocument = document;
      return input;
    },
  };
  const window = { addEventListener() {} };
  const row = new FakeElement({
    conversation: "conversation-7",
    conversationTitle: "Old name",
    renameAffordance: "conversation.rename",
    renameCommand: "mom_llama.conversation_rename",
    renameTauriCommand: "mom_llama_conversation_rename",
    renameCli: "mom-llama conversation rename --conversation <id> --title <title> --json",
    renameEffect: "mom_llama.effects.conversation_store.v1",
  });
  row.ownerDocument = document;
  row.rect = { left: 0, top: 0, width: 280, height: 46 };
  const button = new FakeElement({ conversation: "conversation-7", action: "conversation-select" });
  button.ownerDocument = document;
  button.row = row;
  const title = new FakeElement();
  title.textContent = "Old name";
  title.rect = { left: 46, top: 14, width: 160, height: 18 };
  title.ownerDocument = document;
  button.title = title;
  button.children.push(title);
  row.button = button;
  row.children.push(button);
  const errors = [];
  const reports = [];
  const context = {
    document,
    window,
    invoke,
    report: (result) => reports.push(result),
    reportError: (error) => errors.push(error),
  };
  vm.createContext(context);
  const start = source.indexOf("  let activeConversationRename = null;");
  const end = source.indexOf("\n  let conversationNavigationRevision", start);
  assert.ok(start >= 0 && end > start);
  vm.runInContext(`let conversationNavigationRevision = 0;\n${source.slice(start, end)}\nthis.beginRename = beginConversationRename;\nthis.rebindRename = rebindConversationRename;\nthis.changeNavigation = () => { conversationNavigationRevision += 1; };`, context);
  return { context, row, button, title, document, errors, reports };
};

test("inline rename commits once on Enter and keeps conversation authority metadata", async () => {
  const calls = [];
  const h = makeHarness({ invoke: async (...args) => { calls.push(args); return { status: "passed" }; } });
  h.context.beginRename(h.row);
  const input = h.row.children[1];
  assert.equal(input.className, "conversation-rename-editor");
  assert.equal(input.value, "Old name");
  assert.equal(input.getAttribute?.("aria-label") || input["aria-label"], "Conversation name");
  assert.equal(input.dataset.command, "mom_llama.conversation_rename");
  assert.equal(h.button.hidden, false);
  assert.equal(h.button.children.includes(h.title), true);
  assert.equal(h.title.style.visibility, "hidden");
  assert.equal(input.style.left, "46px");
  assert.equal(input.style.width, "160px");
  input.value = "New name";
  input.fire("keydown", { key: "Enter" });
  await new Promise((resolve) => setImmediate(resolve));
  input.fire("blur");
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], "mom_llama_conversation_rename");
  assert.deepEqual(JSON.parse(JSON.stringify(calls[0][1])), {
    conversation: "conversation-7", title: "New name",
  });
  assert.equal(h.row.dataset.conversationTitle, "New name");
  assert.equal(h.title.textContent, "New name");
  assert.equal(h.button.hidden, false);
  assert.equal(h.row.classList.contains("is-renaming"), false);
  assert.equal(h.document.activeElement, h.button);
});

test("Escape cancels inline rename without invoking or changing the title", () => {
  let calls = 0;
  const h = makeHarness({ invoke: async () => { calls += 1; return { status: "passed" }; } });
  h.context.beginRename(h.row);
  const input = h.row.children[1];
  input.value = "Unsaved name";
  input.fire("keydown", { key: "Escape" });
  assert.equal(calls, 0);
  assert.equal(h.title.textContent, "Old name");
  assert.equal(h.button.hidden, false);
  assert.equal(h.document.activeElement, h.button);
});

test("a rename completion does not steal focus after navigation changes", async () => {
  let resolveRename;
  const h = makeHarness({ invoke: () => new Promise((resolve) => { resolveRename = resolve; }) });
  h.context.beginRename(h.row);
  const input = h.row.children[1];
  input.value = "New name";
  input.fire("keydown", { key: "Enter" });
  h.context.changeNavigation();
  resolveRename({ status: "passed" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.row.dataset.conversationTitle, "New name");
  assert.notEqual(h.document.activeElement, h.button);
});

test("Escape cannot dismiss an in-flight rename whose native write cannot be canceled", async () => {
  let resolveRename;
  const h = makeHarness({ invoke: () => new Promise((resolve) => { resolveRename = resolve; }) });
  h.context.beginRename(h.row);
  const input = h.row.children.at(-1);
  input.value = "Committed name";
  input.fire("keydown", { key: "Enter" });
  input.fire("keydown", { key: "Escape" });
  assert.equal(h.row.children.includes(input), true);
  assert.equal(h.button.hidden, false);
  assert.equal(input.readOnly, true);
  resolveRename({ status: "passed" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(h.title.textContent, "Committed name");
  assert.equal(h.row.children.includes(input), false);
});

test("sidebar projection replacement preserves a failed edit in the same conversation row", async () => {
  const h = makeHarness({ invoke: async () => ({ status: "blocked" }) });
  h.context.beginRename(h.row);
  const input = h.row.children.at(-1);
  input.value = "Still editing";
  input.fire("blur");
  await new Promise((resolve) => setImmediate(resolve));

  const replacement = new FakeElement();
  const replacementRow = new FakeElement({ conversation: "conversation-7" });
  const replacementButton = new FakeElement();
  const replacementTitle = new FakeElement();
  replacementButton.title = replacementTitle;
  replacementRow.button = replacementButton;
  replacement.querySelectorAll = () => [replacementRow];
  h.context.rebindRename(replacement);
  assert.equal(replacementRow.children.at(-1), input);
  assert.equal(input.value, "Still editing");
  assert.equal(replacementButton.hidden, false);
  assert.equal(replacementTitle.style.visibility, "hidden");
});

test("a failed edit keeps ownership when another row is selected for rename", async () => {
  const h = makeHarness({ invoke: async () => ({ status: "blocked" }) });
  h.context.beginRename(h.row);
  const input = h.row.children.at(-1);
  input.value = "Keep pending edit";
  input.fire("blur");
  await new Promise((resolve) => setImmediate(resolve));

  const secondRow = new FakeElement({ conversation: "conversation-8", conversationTitle: "Other" });
  const secondButton = new FakeElement();
  const secondTitle = new FakeElement();
  secondTitle.textContent = "Other";
  secondButton.title = secondTitle;
  secondRow.button = secondButton;
  secondRow.querySelector = (selector) => selector === '[data-action="conversation-select"]' ? secondButton : null;
  h.context.beginRename(secondRow);
  assert.equal(h.row.children.includes(input), true);
  assert.equal(input.value, "Keep pending edit");
  assert.equal(h.document.activeElement, input);
});

test("blur saves, while a failed rename preserves the edited value for recovery", async () => {
  const h = makeHarness({ invoke: async () => ({ status: "blocked", blocker: { message: "temporarily unavailable" } }) });
  h.context.beginRename(h.row);
  const input = h.row.children[1];
  input.value = "Keep this edit";
  input.fire("blur");
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(input.value, "Keep this edit");
  assert.equal(input.readOnly, false);
  assert.equal(h.title.textContent, "Old name");
  assert.equal(h.button.hidden, false);
  assert.equal(h.title.style.visibility, "hidden");
  assert.equal(h.row.classList.contains("is-renaming"), true);
  assert.equal(h.reports.length, 1);
});

test("selected title clicks enter edit; first clicks and other row areas keep selection behavior", () => {
  const start = source.indexOf('"conversation-select": async');
  const select = source.slice(start, source.indexOf('\n    "chat-cancel"', start));
  assert.match(select, /button\.dataset\.conversation === selectedConversation\(\)/);
  assert.match(select, /event\?\.target\?\.closest\?\.\("\.conversation-title"\)/);
  assert.match(select, /else await selectConversation\(button\.dataset\.conversation\)/);
  assert.match(source, /\{ id: "rename", text: "Rename…", action: safeAction\(\(\) => beginConversationRename\(target\)\) \}/);
  assert.doesNotMatch(source, /conversation-rename-modal|conversation-rename-form/);
  const css = fs.readFileSync(path.join(__dirname, "style.css"), "utf8");
  assert.match(css, /\.conversation-sidebar-row \{ position: relative;/);
  assert.match(css, /\.conversation-rename-editor \{\s*position: absolute;/);
  assert.match(css, /\.compact-dialog[^\n]*background: var\(--dialog-surface\)/);
  assert.match(css, /--dialog-surface: #ffffff;/);
});
