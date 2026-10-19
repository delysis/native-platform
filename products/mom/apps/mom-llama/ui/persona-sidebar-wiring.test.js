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
  assert.ok(actions.includes("await openPersonaConversation(persona);"));
  const draft = between(client, "const openNewDraft =", "const actionHandlers =");
  assert.ok(draft.indexOf("await refreshConversationProjection();") < draft.indexOf("focusComposer();"));
  assert.ok(draft.includes("const retainedDraft = retainComposerDraft();"));
  assert.ok(draft.includes("await retainedDraft;"));
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

// Execute actual Settings open/close owners and the existing close action.
// Synthetic focus objects do not qualify native menu/WebView focus behavior.
const settingsFocusVm = require("node:vm");
const settingsFocusHarness = (withToggle) => {
  const focusEvents = [];
  const classList = { add() {}, remove() {} };
  const composer = { selectionStart: 2, selectionEnd: 5, focus() { focusEvents.push("composer"); } };
  const toggle = withToggle ? { setAttribute() {}, focus() { focusEvents.push("toggle"); } } : null;
  const tab = { focus() { context.document.activeElement = tab; } };
  const outside = {};
  const sidebar = { hidden: true, classList, setAttribute() {},
    contains: (node) => node === tab, querySelector: () => tab };
  const calls = [];
  const context = {
    document: { activeElement: outside, getElementById: () => sidebar,
      querySelector(selector) { return selector === '[data-action="settings-open"]' ? toggle : composer; } },
    shell: () => ({ classList }), sizeComposer() {}, switchSettingsSection() {},
    refreshConversationSettingsProjection: async () => { calls.push("refresh-current-chat"); },
    reportError(error) { throw error; },
    invoke: async (command) => { calls.push(command); return { status: "passed" }; },
  };
  settingsFocusVm.runInNewContext(
    between(client, "  const openSettings =", "  const setModalVisibility =")
    + "\nconst actions = {" + between(client, '    "settings-open": async', '    "settings-section": async') + "};"
    + "\nglobalThis.api = { openSettings, close: actions['settings-close'] };",
    context,
  );
  return { ...context.api, composer, sidebar, focusEvents, calls, focusOutside() { context.document.activeElement = outside; } };
};

test("Settings native-menu open then actual Close returns focus to current composer", async () => {
  const h = settingsFocusHarness(false); h.openSettings();
  assert.equal(h.sidebar.hidden, false);
  await h.close();
  assert.equal(h.sidebar.hidden, true);
  assert.deepEqual(h.calls, ["refresh-current-chat", "mom_llama_settings_get"]);
  assert.deepEqual(h.focusEvents, ["composer"]);
  assert.deepEqual([h.composer.selectionStart, h.composer.selectionEnd], [2, 5]);
});

test("Settings Close preserves its DOM invoker when one exists", async () => {
  const h = settingsFocusHarness(true); h.openSettings(); await h.close();
  assert.deepEqual(h.focusEvents, ["toggle"]);
});

test("Settings Close does not move unrelated current focus", async () => {
  const h = settingsFocusHarness(false); h.openSettings(); h.focusOutside(); await h.close();
  assert.deepEqual(h.focusEvents, []);
});
