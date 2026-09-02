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
  assert.match(ui, /acceptance\?\.status !== "passed"/);
  assert.match(ui, /if \(!responseIsExact \|\| !viewIsExact\) \{[\s\S]*await refreshChat\(\);/);
});
