"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const composer = require("./composer-key-policy.js");

const kinds = (transition) => transition.effects.map((item) => item.kind);
const key = (state, keyName, fields = {}) => composer.reduce(state, {
  type: "key_down",
  key: keyName,
  ...fields,
});

test("composition suppresses send, mentions, AI, arrows, and Escape", () => {
  let transition = composer.reduce(composer.initialState(), { type: "composition_start" });
  assert.equal(transition.state.kind, composer.StateKind.Composing);
  for (const keyName of ["Enter", "ArrowDown", "ArrowRight", "Escape"]) {
    assert.deepEqual(kinds(key(transition.state, keyName)), []);
  }
  assert.equal(
    composer.reduce(transition.state, {
      type: "mention_open",
      optionCount: 2,
    }).state.kind,
    composer.StateKind.Composing,
  );
  assert.equal(
    composer.reduce(transition.state, {
      type: "ai_request",
      requestId: "ai-1",
      anchor: "draft-1",
    }).state.kind,
    composer.StateKind.Composing,
  );
  transition = composer.reduce(transition.state, { type: "composition_end" });
  assert.equal(transition.state.kind, composer.StateKind.Idle);

  const pending = composer.reduce(composer.initialState(), {
    type: "ai_request",
    requestId: "ai-1",
    anchor: "draft-1",
  }).state;
  transition = composer.reduce(pending, { type: "composition_start" });
  assert.deepEqual(transition.effects, [{ kind: "ai_cancel", requestId: "ai-1" }]);

  const mention = composer.reduce(composer.initialState(), {
    type: "mention_open",
    optionCount: 2,
  }).state;
  transition = composer.reduce(mention, { type: "composition_start" });
  assert.deepEqual(transition.effects, [{ kind: "mention_dismiss" }]);
});

test("native composition flags fail closed even before lifecycle state catches up", () => {
  const idle = composer.initialState();
  assert.deepEqual(kinds(key(idle, "Enter", { isComposing: true })), []);
  assert.deepEqual(kinds(key(idle, "Enter", { keyCode: 229 })), []);
  const mention = composer.reduce(idle, { type: "mention_open", optionCount: 1 }).state;
  assert.deepEqual(kinds(key(mention, "Enter", { isComposing: true })), []);
  assert.deepEqual(kinds(key(mention, "ArrowDown", { keyCode: 229 })), []);
});

test("mention navigation wraps, accepts, and dismisses before send", () => {
  assert.equal(composer.reduce(composer.initialState(), {
    type: "mention_open",
    optionCount: Number.NaN,
  }).state.kind, composer.StateKind.Idle);
  const mention = composer.reduce(composer.initialState(), {
    type: "mention_open",
    optionCount: 3,
  }).state;
  const previous = key(mention, "ArrowUp");
  assert.equal(previous.state.activeIndex, 2);
  assert.deepEqual(previous.effects, [{ kind: "mention_active_changed", activeIndex: 2 }]);
  const next = key(previous.state, "ArrowDown");
  assert.equal(next.state.activeIndex, 0);
  assert.deepEqual(kinds(key(next.state, "Enter")), ["mention_accept"]);
  const dismissed = key(next.state, "Escape");
  assert.equal(dismissed.state.kind, composer.StateKind.Idle);
  assert.deepEqual(kinds(dismissed), ["mention_dismiss"]);
});

test("AI state is request-and-anchor exact and Right Arrow is its only accept key", () => {
  const idle = composer.initialState();
  const pending = composer.reduce(idle, {
    type: "ai_request",
    requestId: "ai-1",
    anchor: "draft-1",
  }).state;
  assert.equal(pending.kind, composer.StateKind.AiPending);
  assert.equal(composer.reduce(pending, {
    type: "ai_present",
    requestId: "stale",
    anchor: "draft-1",
    suffix: " later",
  }).state.kind, composer.StateKind.AiPending);
  const presented = composer.reduce(pending, {
    type: "ai_present",
    requestId: "ai-1",
    anchor: "draft-1",
    suffix: " later",
  }).state;
  assert.equal(presented.kind, composer.StateKind.AiPresented);
  assert.deepEqual(kinds(key(presented, "Tab")), []);
  assert.deepEqual(kinds(key(presented, "ArrowRight")), ["ai_accept"]);
  assert.deepEqual(kinds(key(presented, "Escape")), ["ai_dismiss"]);
});

test("mention activation cancels AI and Enter retains both send modes", () => {
  const pending = composer.reduce(composer.initialState(), {
    type: "ai_request",
    requestId: "ai-1",
    anchor: "draft-1",
  }).state;
  const mention = composer.reduce(pending, { type: "mention_open", optionCount: 2 });
  assert.equal(mention.state.kind, composer.StateKind.Mention);
  assert.deepEqual(kinds(mention), ["ai_cancel"]);

  assert.deepEqual(kinds(key(composer.initialState(), "Enter")), ["submit"]);
  assert.deepEqual(kinds(key(composer.initialState(), "Enter", { shiftKey: true })), []);
  assert.deepEqual(kinds(key(composer.initialState(), "Enter", {
    sendOnEnter: false,
    metaKey: true,
  })), ["submit"]);
  assert.deepEqual(kinds(key(pending, "Enter")), ["ai_cancel", "submit"]);
  assert.deepEqual(kinds(key(composer.initialState(), "Tab")), []);
});
