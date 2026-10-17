"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const composer = require("./composer-key-policy.js");
const voiceCapturePolicy = require("./voice-capture-policy.js");

const modifierNames = ["shiftKey", "altKey", "metaKey", "ctrlKey"];
const modifiers = (mask) => Object.fromEntries(
  modifierNames.map((name, index) => [name, Boolean(mask & (1 << index))]),
);
const presentedState = () => {
  const pending = composer.reduce(composer.initialState(), {
    type: "ai_request", requestId: "fixture-request", anchor: "fixture-anchor",
  }).state;
  return composer.reduce(pending, {
    type: "ai_present", requestId: "fixture-request", anchor: "fixture-anchor", suffix: " more",
  }).state;
};

// Run the actual registered listener, not a second implementation of its policy.
// The minimal DOM deliberately has no live suggestion or native IPC. This proves
// effect dispatch and default prevention, not rendering, native mutation or undo.
const bridgeKey = async (key, fields, initialState = presentedState) => {
  const listeners = new Map();
  const keyEffects = [];
  const document = {
    getElementById: () => null,
    querySelector: () => null,
    querySelectorAll: () => [],
    addEventListener: (name, listener) => {
      const entries = listeners.get(name) || [];
      entries.push(listener);
      listeners.set(name, entries);
    },
  };
  const context = {
    document,
    window: { setTimeout: () => 1, clearTimeout: () => {}, addEventListener: () => {} },
    localStorage: { getItem: () => null, setItem: () => {} },
    TextEncoder,
    TextDecoder,
    MomVoiceCapturePolicy: voiceCapturePolicy,
    MomLlamaComposerKeyPolicy: {
      initialState,
      reduce: (state, event) => {
        const transition = composer.reduce(state, event);
        if (event.type === "key_down") {
          keyEffects.push(...transition.effects.map((effect) => effect.kind));
        }
        return transition;
      },
    },
  };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8"), context, {
    filename: "coop-hx.js", timeout: 1000,
  });
  let defaultPrevented = false;
  let submissions = 0;
  const event = {
    key, keyCode: key === "ArrowRight" ? 39 : 13, isComposing: false, ...fields,
    target: {
      id: "message", value: "draft", dataset: { executionProfileVersion: "1" },
      selectionStart: 5, selectionEnd: 5,
      matches: (selector) => selector === "#chat-form textarea[name='message']",
      closest: () => null,
      form: { querySelectorAll: () => [], requestSubmit: () => { submissions += 1; } },
    },
    preventDefault: () => { defaultPrevented = true; },
  };
  assert.ok(listeners.get("keydown")?.length, "production listener must be registered");
  for (const listener of listeners.get("keydown")) await listener(event);
  return { defaultPrevented, keyEffects, submissions };
};

for (let mask = 0; mask < 16; mask += 1) {
  const fields = modifiers(mask);
  const label = modifierNames.filter((name) => fields[name]).join("+") || "unmodified";
  test(`Right Arrow policy: ${label}`, () => {
    const state = presentedState();
    const transition = composer.reduce(state, { type: "key_down", key: "ArrowRight", ...fields });
    assert.deepEqual(transition.effects.map((effect) => effect.kind), mask === 0 ? ["ai_accept"] : []);
    assert.equal(transition.state.kind, mask === 0 ? composer.StateKind.Idle : composer.StateKind.AiPresented);
  });
  test(`Right Arrow production event bridge: ${label}`, async () => {
    const observed = await bridgeKey("ArrowRight", fields);
    assert.equal(observed.defaultPrevented, mask === 0);
    assert.deepEqual(observed.keyEffects, mask === 0 ? ["ai_accept"] : []);
  });
}

// Option-Return belongs to the text control. It must not be classified as
// either accepting a mention or submitting the current draft.
for (const [label, state] of [
  ["idle", composer.initialState()],
  ["pending", composer.reduce(composer.initialState(), {
    type: "ai_request", requestId: "fixture-request", anchor: "fixture-anchor",
  }).state],
  ["presented", presentedState()],
  ["mention", composer.reduce(composer.initialState(), { type: "mention_open", optionCount: 2 }).state],
]) {
  test(`Option-Return preserves native editing in ${label} state`, () => {
    const transition = composer.reduce(state, {
      type: "key_down", key: "Enter", altKey: true, sendOnEnter: true,
    });
    assert.deepEqual(transition.effects, []);
    assert.equal(transition.state, state);
  });
}
for (const altKey of [false, true]) {
  test(`production Enter bridge with Option=${altKey}`, async () => {
    const observed = await bridgeKey("Enter", { altKey }, composer.initialState);
    assert.equal(observed.defaultPrevented, !altKey);
    assert.equal(observed.submissions, altKey ? 0 : 1);
    assert.deepEqual(observed.keyEffects, altKey ? [] : ["submit"]);
  });
}
