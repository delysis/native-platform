"use strict";

const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const client = readFileSync(path.join(__dirname, "coop-hx.js"), "utf8");
const view = readFileSync(path.join(__dirname, "..", "src-tauri", "src", "view.rs"), "utf8");

const between = (source, start, end) => {
  const first = source.indexOf(start);
  const last = source.indexOf(end, first + start.length);
  assert.ok(first >= 0 && last > first, `missing source range: ${start}`);
  return source.slice(first, last);
};

test("Persona data is projected once per Rust render path", () => {
  assert.equal(
    view.match(/mom_llama_runtime::persona_list\(\)/g)?.length,
    1,
    "Persona storage must have one projection helper rather than renderer-local reads",
  );

  for (const [renderer, end] of [
    ["render_app", "\npub fn render_chat_fragment"],
    ["render_sidebar_fragment", "\npub fn render_settings_fragment"],
    ["render_settings_fragment", "\nstruct AppProjection"],
  ]) {
    const body = between(view, `pub fn ${renderer}(`, end);
    assert.ok(body.includes("let personas = persona_projection();"), `${renderer} must project Personas once`);
  }

  const sidebar = between(view, "fn sidebar(", "\nfn composer(");
  assert.ok(!sidebar.includes("mom_llama_runtime::persona_list()"));
});

test("sidebar disclosure collapses locally and refreshes only on expansion", () => {
  const handler = between(
    client,
    '"sidebar-section-toggle": async (button) => {',
    '\n    "settings-open":',
  );
  assert.ok(handler.includes('if (!expanding) {'));
  assert.ok(handler.includes("list.hidden = true;"));
  assert.ok(handler.includes("collapsedSidebarSections.add(section);"));
  assert.ok(handler.includes("const replacement = await refreshSidebar();"));
  assert.ok(handler.includes("CSS.escape(section)"));
  assert.ok(!handler.includes("mom_llama_persona_list"));
});

test("Persona menu actions move focus out of the hidden menu", () => {
  const actions = between(
    client,
    '"persona-menu-start": async () => {',
    '\n    "persona-menu-removal-preview":',
  );
  assert.ok(
    actions.indexOf("await refreshConversationProjection();") < actions.indexOf("focusComposer();"),
    "Start Conversation must focus the replaced composer after refresh",
  );
  assert.ok(actions.includes('formField(document.getElementById("persona-editor"), "persona_name")?.focus();'));
});
