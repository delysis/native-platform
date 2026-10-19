"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const ui = fs.readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");
const view = fs.readFileSync(
  path.join(__dirname, "..", "src-tauri", "src", "view.rs"),
  "utf8",
);
const runtime = fs.readFileSync(
  path.join(__dirname, "..", "..", "..", "crates", "mom-llama-runtime", "src", "composer.rs"),
  "utf8",
);

const block = (source, start, end) => {
  const begin = source.indexOf(start);
  assert.notEqual(begin, -1, `missing ${start}`);
  const finish = source.indexOf(end, begin + start.length);
  assert.notEqual(finish, -1, `missing ${end}`);
  return source.slice(begin, finish);
};

test("autocomplete is resident-only, bounded, single-request, and transient", () => {
  assert.match(runtime, /resident_model_for_profile_if_loaded/);
  assert.match(runtime, /generate_speculative\(GenerationRequest/);
  assert.match(runtime, /MAX_COMPLETION_TOKENS: u32 = 32/);
  assert.match(runtime, /COMPLETION_WALL_LIMIT: Duration = Duration::from_secs\(4\)/);
  assert.match(runtime, /MAX_SUFFIX_BYTES: usize = 512/);
  assert.doesNotMatch(runtime, /load_model|model_slot_load|persist_command_receipt/);
  assert.match(runtime, /WaitOutcome::TimedOut\(ticket\)[\s\S]*ticket\.cancel_all\(\);[\s\S]*drop\(ticket\);/);

  const request = block(
    ui,
    "const scheduleComposerAutocomplete =",
    "const acceptComposerAutocomplete =",
  );
  assert.match(request, /}, 350\);/);
  assert.equal(
    request.match(/invoke\("mom_llama_composer_autocomplete"/g)?.length,
    1,
    "one debounced attempt must issue exactly one generation request",
  );
  assert.doesNotMatch(request, /report\(|reportError\(|busy/i);
});

test("Right Arrow atomically accepts the full anchor as one normal draft edit", () => {
  const accept = block(
    ui,
    "const acceptComposerAutocomplete =",
    "const ensureMessageStream =",
  );
  const validation = accept.indexOf("mom_llama_composer_autocomplete_accept");
  const edit = accept.indexOf("textarea.setRangeText");
  const input = accept.indexOf('new Event("input", { bubbles: true })');
  assert.ok(validation >= 0 && validation < edit);
  assert.ok(edit >= 0 && edit < input);
  assert.equal(accept.match(/setRangeText/g)?.length, 1);
  assert.equal(accept.match(/new Event\("input"/g)?.length, 1);
  assert.match(accept, /model_fingerprint_sha256/);
  assert.match(accept, /generation_input_sha256/);
  assert.match(accept, /autocompleteCommittedDraft/);
  assert.match(accept, /if \(!responseIsExact \|\| !viewIsExact\)[\s\S]*await refreshChat\(\)/);
  assert.match(runtime, /mutate_documents\(/);
  assert.match(runtime, /selected_conversation_id\.as_deref\(\)/);
  assert.match(runtime, /draft\.message != anchor\.draft/);
  assert.match(runtime, /draft\.attachment_ids != anchor\.attachment_ids/);
});

test("composer markup preserves combobox and live suggestion semantics", () => {
  assert.match(view, /aria-controls="mention-candidates"/);
  assert.match(view, /aria-describedby="composer-ai-status"/);
  assert.match(view, /id="composer-ai-status" class="sr-only" aria-live="polite"/);
  assert.match(view, /data-autocomplete-accept-tauri-command="mom_llama_composer_autocomplete_accept"/);
});

test("reducer cancellation effects reach the native speculative operation", () => {
  assert.match(ui, /keyEffect\.kind === "ai_cancel"[\s\S]*forceNative: true/);
  assert.match(ui, /mentionEffects\.some\(\(effect\) => effect\.kind === "ai_cancel"\)[\s\S]*forceNative: true/);
});

test("acceptance gates interaction and reconciles every committed backend result", () => {
  assert.match(ui, /let autocompleteAccepting = false/);
  assert.match(ui, /if \(autocompleteAccepting\) return;[\s\S]*form\.dataset\.busy/);
  assert.match(ui, /if \(autocompleteAccepting\) \{[\s\S]*event\.preventDefault\(\);[\s\S]*return;/);
  assert.match(ui, /acceptance\?\.status !== "host_integrated"/);
  assert.match(ui, /if \(!responseIsExact \|\| !viewIsExact\) \{[\s\S]*await refreshChat\(\);/);
});

const vm = require("node:vm");
const composer = require("./composer-key-policy.js");
const voiceCapturePolicy = require("./voice-capture-policy.js");
const wireFixtures = JSON.parse(fs.readFileSync(path.join(
  __dirname, "..", "..", "..", "crates", "mom-llama-runtime", "tests", "fixtures",
  "composer-autocomplete-results.json",
), "utf8"));

// The public Rust DTO/serde test checks these exact envelopes. Run the complete
// bridge and its registered input/key listeners; no private selector is replaced.
// This minimal DOM/IPC fixture does not boot an app, run a model or prove undo.
const autocompleteBridge = async (generation, acceptance, selectionOffset = 0) => {
  const listeners = new Map();
  const timers = new Map();
  const calls = [];
  const edits = [];
  const inputEvents = [];
  const readOnlyAtAcceptance = [];
  let nextTimer = 0;
  const anchor = wireFixtures.generation.result.anchor;
  const textNode = () => ({ textContent: "", replaceChildren() { this.textContent = ""; } });
  const ghostAnchor = textNode();
  const ghostSuffix = textNode();
  const hidden = new Set(["hidden"]);
  const ghost = {
    setAttribute: (name) => hidden.add(name),
    removeAttribute: (name) => hidden.delete(name),
    querySelector: (selector) => selector === ".composer-ai-anchor" ? ghostAnchor : ghostSuffix,
  };
  const status = textNode();
  const chat = { dataset: { currentConversation: anchor.conversation_id, conversationKind: "chat" } };
  const form = { id: "chat-form", dataset: {}, querySelectorAll: () => [] };
  const textarea = {
    id: "message", value: anchor.draft, form, readOnly: false, style: {}, scrollHeight: 24,
    selectionStart: anchor.selection_start_utf16 - selectionOffset,
    selectionEnd: anchor.selection_end_utf16 - selectionOffset,
    dataset: { activeLeaf: anchor.active_leaf_message_id, executionProfileVersion: "7" },
    matches: (selector) => selector === "#chat-form textarea[name='message']",
    closest: () => null, setAttribute() {}, removeAttribute() {},
    setRangeText(suffix, start, end, mode) {
      edits.push({ suffix, start, end, mode });
      this.value = this.value.slice(0, start) + suffix + this.value.slice(end);
      this.selectionStart = this.selectionEnd = start + suffix.length;
    },
    dispatchEvent(event) {
      inputEvents.push(event.type);
      event.target = this;
      for (const listener of listeners.get(event.type) || []) listener(event);
      return true;
    },
  };
  form.elements = { namedItem: (name) => name === "message" ? textarea : null };
  const document = {
    activeElement: textarea,
    getElementById: (id) => ({ chat, "chat-form": form, "composer-ai-ghost": ghost,
      "composer-ai-status": status }[id] || null),
    querySelector: (selector) => selector === "#chat-form textarea[name='message']" ? textarea : null,
    querySelectorAll: () => [],
    addEventListener(name, listener) {
      const entries = listeners.get(name) || [];
      entries.push(listener);
      listeners.set(name, entries);
    },
  };
  const window = {
    setTimeout: (callback, delay) => { const id = ++nextTimer; timers.set(id, { callback, delay }); return id; },
    clearTimeout: (id) => timers.delete(id), addEventListener() {},
  };
  const context = {
    document, window, setTimeout: window.setTimeout, clearTimeout: window.clearTimeout,
    localStorage: { getItem: () => null, setItem() {} },
    TextEncoder, TextDecoder, MomLlamaComposerKeyPolicy: composer,
    MomVoiceCapturePolicy: voiceCapturePolicy,
    Event: class { constructor(type, fields) { this.type = type; Object.assign(this, fields); } },
  };
  vm.runInNewContext(
    fs.readFileSync(path.join(__dirname, "voice-pipeline-scheduler.js"), "utf8"),
    context,
    { filename: "voice-pipeline-scheduler.js", timeout: 1000 },
  );
  vm.runInNewContext(ui, context, { filename: "coop-hx.js", timeout: 1000 });
  // The existing boot refusal settles before the fixture enables IPC. None of
  // the app startup/native capabilities are supplied by this component control.
  await new Promise(setImmediate);
  window.__TAURI__ = { core: { invoke: async (command, payload) => {
    calls.push({ command, payload: JSON.parse(JSON.stringify(payload)) });
    if (command === "mom_llama_draft_update") return { status: "host_integrated" };
    if (command === "mom_llama_composer_autocomplete") return structuredClone(generation);
    if (command === "mom_llama_composer_autocomplete_accept") {
      readOnlyAtAcceptance.push(textarea.readOnly);
      return structuredClone(acceptance);
    }
    throw new Error(`Unexpected component IPC: ${command}`);
  } } };
  assert.ok(listeners.get("input")?.length);
  assert.ok(listeners.get("keydown")?.length);
  for (const listener of listeners.get("input")) listener({ target: textarea, isComposing: false });
  for (const [id, timer] of [...timers].sort((a, b) => a[1].delay - b[1].delay)) {
    if (!timers.delete(id)) continue;
    await timer.callback();
  }
  const presented = { hidden: hidden.has("hidden"), suffix: ghostSuffix.textContent,
    draft: textarea.value, announcement: status.textContent };
  let defaultPrevented = false;
  for (const listener of listeners.get("keydown")) {
    listener({ target: textarea, key: "ArrowRight", keyCode: 39, isComposing: false,
      preventDefault: () => { defaultPrevented = true; } });
  }
  await new Promise(setImmediate);
  assert.ok(calls.every(({ command }) => ["mom_llama_draft_update",
    "mom_llama_composer_autocomplete", "mom_llama_composer_autocomplete_accept"].includes(command)),
  JSON.stringify(calls));
  return { calls, edits, inputEvents, presented, defaultPrevented, readOnlyAtAcceptance,
    value: textarea.value, readOnly: textarea.readOnly, hidden: hidden.has("hidden") };
};

test("public command envelopes reach autocomplete presentation and exact normal acceptance", async () => {
  const anchor = wireFixtures.generation.result.anchor;
  // Ordinary typing is persisted unchanged, even when a non-end selection makes
  // autocomplete ineligible. This control precedes the causal status assertions.
  const ordinary = await autocompleteBridge(wireFixtures.generation, wireFixtures.acceptance, 1);
  assert.equal(ordinary.value, anchor.draft);
  assert.equal(ordinary.calls.length, 1);
  assert.equal(ordinary.calls[0].command, "mom_llama_draft_update");
  assert.equal(ordinary.calls[0].payload.message, anchor.draft);
  assert.equal(ordinary.defaultPrevented, false);
  assert.deepEqual(ordinary.edits, []);
  for (const result of [wireFixtures.generation_blocked, wireFixtures.generation_unready]) {
    const refused = await autocompleteBridge(result, wireFixtures.acceptance);
    assert.equal(refused.calls.filter(({ command }) => command === "mom_llama_composer_autocomplete").length, 1);
    assert.equal(refused.presented.hidden, true);
    assert.equal(refused.value, anchor.draft);
    assert.equal(refused.defaultPrevented, false);
    assert.deepEqual(refused.edits, []);
  }
  // The first BEFORE failure is here: the actual typed host_integrated envelope
  // is discarded rather than presented. Acceptance assertions are then unreached.
  const admitted = await autocompleteBridge(wireFixtures.generation, wireFixtures.acceptance);
  assert.equal(admitted.presented.hidden, false, JSON.stringify(admitted));
  assert.equal(admitted.presented.suffix, " world");
  assert.equal(admitted.presented.draft, anchor.draft);
  const requested = admitted.calls.find(({ command }) => command === "mom_llama_composer_autocomplete");
  assert.deepEqual(requested.payload, { conversation: "conversation", draft: anchor.draft,
    activeLeafMessageId: "leaf", executionProfileVersion: 7, selectionStartUtf16: 8,
    selectionEndUtf16: 8, attachmentIds: [] });
  const accepted = admitted.calls.find(({ command }) => command === "mom_llama_composer_autocomplete_accept");
  assert.deepEqual(accepted.payload, { anchor, suffix: " world" });
  assert.equal(admitted.defaultPrevented, true);
  assert.deepEqual(admitted.readOnlyAtAcceptance, [true]);
  assert.equal(admitted.value, wireFixtures.acceptance.result.message);
  assert.deepEqual(admitted.edits, [{ suffix: " world", start: 8, end: 8, mode: "end" }]);
  assert.deepEqual(admitted.inputEvents, ["input"]);
  assert.equal(admitted.readOnly, false);
  assert.equal(admitted.hidden, true);
  for (const result of [wireFixtures.acceptance_blocked, wireFixtures.acceptance_unready]) {
    const refused = await autocompleteBridge(wireFixtures.generation, result);
    assert.equal(refused.presented.hidden, false);
    assert.equal(refused.calls.filter(({ command }) => command === "mom_llama_composer_autocomplete_accept").length, 1);
    assert.deepEqual(refused.readOnlyAtAcceptance, [true]);
    assert.equal(refused.value, anchor.draft);
    assert.equal(refused.readOnly, false);
    assert.equal(refused.hidden, true);
    assert.deepEqual(refused.edits, []);
    assert.deepEqual(refused.inputEvents, []);
  }
});
