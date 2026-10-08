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
  assert.ok(actions.includes("await openNewDraft(persona);"));
  const draft = between(client, "const openNewDraft =", "const actionHandlers =");
  assert.ok(draft.indexOf("await refreshConversationProjection();") < draft.indexOf("focusComposer();"));
  assert.ok(draft.includes("await retainComposerDraft();"));
  assert.ok(draft.includes('invoke("mom_llama_conversation_draft_open", { persona })'));
  assert.ok(!draft.includes("instantiatePersona"));
  assert.ok(actions.includes('formField(document.getElementById("persona-editor"), "persona_name")?.focus();'));
});


test("Persona updates use Rust's tagged template policy and exclude retired controls", () => {
  const vm = require("node:vm");
  const factory = between(client, "  const personaProfileFromEditor =", "\n  const setPersonaGroupEditor =");
  for (const policy of ["model_default", "frozen_source"]) {
    const values = { persona_id: "persona", persona_name: "Name", persona_handle: "name",
      persona_model_choice: "", persona_system_message: "instructions",
      persona_chat_template_policy: policy, persona_chat_template: "exact template" };
    const context = { document: { getElementById: () => ({ dataset: { personaJson: "{}" } }) },
      formValue: (_, name) => values[name] || "" };
    vm.createContext(context);
    vm.runInContext(`${factory}\nthis.profile = personaProfileFromEditor();`, context);
    const profile = JSON.parse(JSON.stringify(context.profile));
    assert.deepEqual(profile.chat_template, policy === "model_default"
      ? { kind: "model_default" } : { kind: "frozen_source", template: "exact template" });
    for (const retired of ["tool_bindings", "source_history_tokens", "host_context_tokens"]) {
      assert.ok(!(retired in profile), `${retired} must not retain update authority`);
    }
  }
});
