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
