"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const productRoot = path.resolve(__dirname, "../../..");
const read = (relative) => fs.readFileSync(path.join(productRoot, relative), "utf8");
const commands = JSON.parse(read("contracts/commands.json")).commands;
const view = read("apps/mom-llama/src-tauri/src/view.rs");
const viewProduction = view.split("#[cfg(test)]", 1)[0];
const bridge = read("apps/mom-llama/ui/coop-hx.js");
const index = read("apps/mom-llama/ui/index.html");
const styles = read("apps/mom-llama/ui/style.css");
const chatRuntime = read("crates/mom-llama-runtime/src/chat.rs");
const attachmentRuntime = read("crates/mom-llama-runtime/src/attachments.rs");
const configRuntime = read("crates/mom-llama-runtime/src/config.rs");
const personasRuntime = read("crates/mom-llama-runtime/src/personas.rs");
const cli = read("crates/mom-llama-cli/src/main.rs");

const backendOnlyCommandIds = [
  "mom_llama.engine_check",
  "mom_llama.engine_configure",
  "mom_llama.kv_cache_clear",
  "mom_llama.kv_cache_restore",
  "mom_llama.kv_cache_save",
  "mom_llama.kv_cache_status",
  "mom_llama.model_slot_list",
  "mom_llama.model_slot_load",
  "mom_llama.model_slot_unload",
  "mom_llama.skill_apply",
  "mom_llama.skill_create",
  "mom_llama.skill_list",
  "mom_llama.skill_update",
];

const controlPattern =
  /ControlSpec \{\s*affordance: "([^"]+)",\s*command: "([^"]+)"/g;
const projectedCommandIds = [...viewProduction.matchAll(controlPattern)].map((match) => match[2]);

test("diagnostic and legacy commands remain available without claiming native-view controls", () => {
  const classified = commands
    .filter((command) => command.surface === "backend_only")
    .map((command) => command.command_id)
    .sort();
  assert.deepEqual(classified, backendOnlyCommandIds);

  for (const commandId of backendOnlyCommandIds) {
    const command = commands.find((candidate) => candidate.command_id === commandId);
    assert.ok(command, `missing command contract ${commandId}`);
    assert.equal(command.surface, "backend_only");
    assert.deepEqual(command.affordances, []);
    assert.ok(command.cli);
    assert.ok(command.tauri_command);
    assert.ok(!projectedCommandIds.includes(commandId), `${commandId} leaked into ControlSpec`);
  }
});

test("ordinary model selection stays visible while internal engine and resident controls stay hidden", () => {
  for (const commandId of [
    "mom_llama.model_list",
    "mom_llama.model_select",
    "mom_llama.path_select",
  ]) {
    const command = commands.find((candidate) => candidate.command_id === commandId);
    assert.ok(command);
    assert.equal(command.surface ?? "native_view", "native_view");
    assert.ok(command.affordances.length > 0);
    assert.ok(projectedCommandIds.includes(commandId));
  }
  const modelSelect = commands.find((candidate) => candidate.command_id === "mom_llama.model_select");
  assert.ok(modelSelect.cli.includes("[--conversation <id>]"));
  assert.ok(cli.includes("conversation_model_select_and_load"));
  assert.ok(cli.includes("None => mom_llama_runtime::model_select(model_path)?"));
});

test("model choice is a scoped searchable picker with automatic projector pairing", () => {
  const composerStart = viewProduction.indexOf("fn composer(");
  const composerEnd = viewProduction.indexOf("\nfn message_row(", composerStart);
  const composer = viewProduction.slice(composerStart, composerEnd);

  for (const marker of [
    'data-model-picker="true"',
    'data-model-search="true"',
    'data-action="model-select"',
    'data-action="model-browse"',
    'data-conversation=[conversation_id]',
    '"Loaded"',
    '"Available"',
    '"Choose GGUF file…"',
  ]) {
    assert.ok(viewProduction.includes(marker), `missing model picker marker: ${marker}`);
  }
  for (const marker of [
    "Conversation model",
    "conversation-model-chip",
    "runtime-dot",
    "Vision projector",
    'name="mmproj_path"',
    'name="persona_mmproj_path"',
    'name="persona_model_path"',
  ]) {
    assert.ok(!viewProduction.includes(marker), `obsolete model UI remains: ${marker}`);
  }
  assert.ok(composer.includes('"Model for this chat"'));
  assert.ok(composer.includes("effective_conversation_model_path(active, settings)"));

  for (const marker of [
    "const selectAndLoadModel",
    'conversation: button.dataset.conversation || null',
    'picker.dataset.state = "loading"',
    '.model-picker-error',
    '[data-model-search]',
  ]) {
    assert.ok(bridge.includes(marker), `missing model picker bridge behavior: ${marker}`);
  }
  const selectionStart = bridge.indexOf("const selectAndLoadModel");
  const selectionEnd = bridge.indexOf("\n  const actionHandlers", selectionStart);
  const selection = bridge.slice(selectionStart, selectionEnd);
  assert.ok(selection.includes("recordCommandResult(result)"));
  assert.ok(selection.indexOf("recordCommandResult(result)") < selection.indexOf("report(result)"));
  assert.ok(selection.includes('if (result?.status === "blocked")'));
  assert.ok(!bridge.includes('modelPath: formValue(form, "model_path")'));
  assert.ok(!bridge.includes('mmprojPath: formValue(form, "mmproj_path")'));
  assert.ok(styles.includes('.model-picker[data-state="loading"]'));
  assert.ok(styles.includes(".composer-model-picker"));
  assert.ok(configRuntime.includes('option_env!("MOM_LLAMA_DEFAULT_MODEL_PATH")'));
  assert.ok(configRuntime.includes('option_env!("MOM_LLAMA_DEFAULT_MMPROJ_PATH")'));
  assert.ok(configRuntime.includes("if !projector_path_explicit"));
  assert.ok(viewProduction.includes('name="persona_model_choice"'));
  assert.ok(viewProduction.includes('"Use default model"'));
  assert.ok(bridge.includes('formValue(editor, "persona_model_choice")'));
  assert.ok(!bridge.includes("persona_model_path"));
  assert.ok(bridge.includes("auto_discover_mmproj: modelPath !=="));
  assert.ok(personasRuntime.includes("if auto_discover_mmproj"));
  assert.ok(personasRuntime.includes("discover_projector_for_model(model_path)"));
  for (const marker of [
    "Choose the matching mmproj",
    "mmproj GGUF in Settings",
    "model check to verify the model and projector",
  ]) {
    assert.ok(!`${chatRuntime}\n${attachmentRuntime}`.includes(marker), `internal projector jargon remains: ${marker}`);
  }
});

test("the sidebar exposes only the compact authoritative Persona menu", () => {
  const sidebarStart = viewProduction.indexOf("fn sidebar(");
  const sidebarEnd = viewProduction.indexOf("\nfn composer(", sidebarStart);
  assert.ok(sidebarStart >= 0 && sidebarEnd > sidebarStart, "sidebar renderer missing");
  const sidebar = viewProduction.slice(sidebarStart, sidebarEnd);

  for (const marker of [
    'data-sidebar-section="personas"',
    'id="sidebar-persona-list"',
    'class="sidebar-persona-row"',
    'data-persona-menu-target="true"',
    'aria-haspopup="menu" aria-expanded="false"',
    'aria-controls="persona-context-menu"',
    'data-action="persona-menu-open"',
  ]) {
    assert.ok(sidebar.includes(marker), `missing compact Persona marker: ${marker}`);
  }
  assert.ok(viewProduction.includes("fn persona_projection()"));
  assert.ok(!sidebar.includes("mom_llama_runtime::persona_list()"));

  for (const marker of [
    "mom_llama_runtime::persona_group_list()",
    'data-sidebar-section="consult-groups"',
    'id="sidebar-consult-group-list"',
    'data-action="sidebar-persona-start"',
    'data-action="sidebar-consult-group-start"',
    'data-settings-card="skills"',
    'data-settings-card="cache"',
    'data-settings-card="engine"',
    "Resident models",
  ]) {
    assert.ok(!sidebar.includes(marker), `forbidden sidebar marker remains: ${marker}`);
  }

  for (const action of [
    'data-action="persona-menu-start"',
    'data-action="persona-menu-edit"',
    'data-action="persona-menu-removal-preview"',
  ]) {
    assert.ok(viewProduction.includes(action), `existing Persona menu action missing: ${action}`);
  }

  assert.ok(styles.includes(".sidebar-persona-row"));
  assert.ok(styles.includes(".sidebar-persona-copy"));
});

test("the native presentation contains no Skills, cache, engine, or resident-slot panels", () => {
  for (const marker of [
    'data-settings-card="skills"',
    'data-settings-card="cache"',
    'data-settings-card="engine"',
    'class="settings-subgrid"',
    'id="prompt-cache-card"',
    'class="settings-card cache-preferences"',
    'name="kv_cache_policy"',
    'class="native-number-grid resident-fields"',
    'id="skill-form"',
    "fn prompt_cache_card",
    "Gentle explainer",
    "Warm, plain language",
    "Explain simply and warmly.",
    "Create Skill",
    "No Skills yet.",
    "Policy: disabled until verified",
    "KV-cache persistence is surfaced",
  ]) {
    assert.ok(!viewProduction.includes(marker), `forbidden native-view marker remains: ${marker}`);
  }

  for (const marker of [
    "mom_llama_engine_check",
    "mom_llama_engine_configure",
    "mom_llama_skill_",
    "mom_llama_kv_cache_",
    "mom_llama_model_slot_",
    "kvCachePolicy",
    '"skills-open"',
    '"engine-check"',
    '"kv-status"',
    '"kv-clear"',
    '"resident-slot-load"',
    '"resident-slot-unload"',
  ]) {
    assert.ok(!bridge.includes(marker), `forbidden renderer bridge remains: ${marker}`);
  }

  assert.ok(!index.includes("cache-inspector.js"));
  for (const marker of [
    ".settings-subgrid",
    ".cache-card-heading",
    ".cache-action-status",
    ".skill-row",
    ".skill-form",
  ]) {
    assert.ok(!styles.includes(marker), `orphaned presentation style remains: ${marker}`);
  }
});
