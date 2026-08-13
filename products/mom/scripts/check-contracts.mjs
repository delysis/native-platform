import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const root = process.cwd();
const readJson = (relative) =>
  JSON.parse(fs.readFileSync(path.join(root, relative), "utf8"));
const readText = (relative) =>
  fs.readFileSync(path.join(root, relative), "utf8");
const fail = (message) => {
  throw new Error(message);
};
const requiredText = (value, label) => {
  if (typeof value !== "string" || value.trim() === "") {
    fail(`${label} must be a non-empty string`);
  }
};
const sameSet = (left, right) =>
  left.length === right.length && left.every((value) => right.includes(value));

const commandsDocument = readJson("contracts/commands.json");
const effectsDocument = readJson("contracts/effects.json");
const parityDocument = readJson("contracts/upstream-parity.json");
const settingsParityDocument = readJson("contracts/settings-parity.json");
const viewSource = readText("apps/mom-llama/src-tauri/src/view.rs");
const configSource = readText("crates/mom-llama-runtime/src/config.rs");
const tauriSource = readText("apps/mom-llama/src-tauri/src/commands.rs");
const tauriMain = readText("apps/mom-llama/src-tauri/src/main.rs");

const effectIds = new Set();
for (const effect of effectsDocument.effects ?? []) {
  requiredText(effect.effect_id, "effect.effect_id");
  if (effectIds.has(effect.effect_id)) fail(`duplicate effect ${effect.effect_id}`);
  effectIds.add(effect.effect_id);
  for (const key of [
    "reads",
    "writes",
    "network",
    "process",
    "secrets",
    "model_calls",
    "persistence",
    "memory",
    "external_apps",
  ]) {
    if (!Array.isArray(effect[key])) fail(`${effect.effect_id}.${key} must be an array`);
  }
  if (effect.network.length !== 0) {
    fail(`${effect.effect_id} violates the native-local no-network profile`);
  }
  const processAllowed = new Set([
    "mom_llama.effects.mcp_stdio.v1",
    "mom_llama.effects.tool_loop.v1",
  ]);
  if (effect.process.length !== 0 && !processAllowed.has(effect.effect_id)) {
    fail(`${effect.effect_id} has process authority outside the MCP boundary`);
  }
  if (!["none", "reversible", "irreversible"].includes(effect.destructive)) {
    fail(`${effect.effect_id}.destructive is invalid`);
  }
}

const controlPattern =
  /ControlSpec \{\s*affordance: "([^"]+)",\s*command: "([^"]+)",\s*tauri_command: "([^"]+)",\s*cli: "([^"]+)",\s*effect: "([^"]+)",\s*label: "([^"]+)"/g;
const controls = [];
for (const match of viewSource.matchAll(controlPattern)) {
  controls.push({
    affordance: match[1],
    command: match[2],
    tauri_command: match[3],
    cli: match[4],
    effect: match[5],
  });
}
if (controls.length === 0) fail("no ControlSpec rows were parsed from the native view");

const controlsByCommand = new Map();
for (const control of controls) {
  if (controlsByCommand.has(control.affordance)) {
    fail(`duplicate visible affordance ${control.affordance}`);
  }
  controlsByCommand.set(control.affordance, control);
}

const commandIds = new Set();
const contractedAffordances = new Set();
for (const command of commandsDocument.commands ?? []) {
  for (const key of [
    "command_id",
    "input_schema",
    "output_schema",
    "effect_spec_id",
    "cli",
    "tauri_command",
    "readiness_required_for_visible_enablement",
    "blocker_behavior",
    "receipt_schema",
  ]) {
    requiredText(command[key], `${command.command_id ?? "command"}.${key}`);
  }
  if (!Array.isArray(command.affordances) || command.affordances.length === 0) {
    fail(`${command.command_id}.affordances must be a non-empty array`);
  }
  if (commandIds.has(command.command_id)) fail(`duplicate command ${command.command_id}`);
  commandIds.add(command.command_id);
  if (!effectIds.has(command.effect_spec_id)) {
    fail(`${command.command_id} references missing effect ${command.effect_spec_id}`);
  }
  if (command.blocker_behavior !== "typed_result") {
    fail(`${command.command_id} must fail through a typed result`);
  }
  if (!tauriSource.match(new RegExp(`pub (?:async )?fn ${command.tauri_command}\\b`))) {
    fail(`${command.command_id} has no Tauri handler ${command.tauri_command}`);
  }
  if (!tauriMain.includes(`commands::${command.tauri_command}`)) {
    fail(`${command.command_id} Tauri handler is not registered`);
  }
  const actual = controls.filter((control) => control.command === command.command_id);
  if (actual.length === 0) fail(`${command.command_id} has no native view projection`);
  const actualAffordances = actual.map((control) => control.affordance);
  if (!sameSet(command.affordances, actualAffordances)) {
    fail(
      `${command.command_id} affordances disagree: contract=${command.affordances.join(",")} view=${actualAffordances.join(",")}`,
    );
  }
  for (const control of actual) {
    contractedAffordances.add(control.affordance);
    if (
      control.tauri_command !== command.tauri_command ||
      control.cli !== command.cli ||
      control.effect !== command.effect_spec_id
    ) {
      fail(`${control.affordance} metadata disagrees with ${command.command_id}`);
    }
  }
}
for (const control of controls) {
  if (!contractedAffordances.has(control.affordance)) {
    fail(`visible affordance ${control.affordance} lacks a command contract`);
  }
}

const allowedClassifications = new Set(parityDocument.allowed_classifications ?? []);
const requiredFeatureIds = [];
for (const feature of parityDocument.features ?? []) {
  requiredText(feature.id, "parity feature id");
  if (!allowedClassifications.has(feature.classification)) {
    fail(`${feature.id} has invalid classification ${feature.classification}`);
  }
  if (feature.classification === "p0_implemented") {
    if (!Array.isArray(feature.evidence) || feature.evidence.length === 0) {
      fail(`${feature.id} claims implementation without evidence`);
    }
  } else {
    requiredText(feature.reason, `${feature.id}.reason`);
  }
  for (const commandId of feature.commands ?? []) {
    if (!commandIds.has(commandId)) {
      fail(`${feature.id} references missing command ${commandId}`);
    }
  }
  if (feature.classification === "p0_required") requiredFeatureIds.push(feature.id);
}
if (requiredFeatureIds.length > 0 && parityDocument.parity_claim?.achieved !== false) {
  fail("parity cannot be achieved while p0_required features remain");
}
const blockingFeatureIds = parityDocument.parity_claim?.blocking_feature_ids ?? [];
if (!sameSet(requiredFeatureIds, blockingFeatureIds)) {
  fail(
    `parity blockers disagree: required=${requiredFeatureIds.join(",")} claim=${blockingFeatureIds.join(",")}`,
  );
}

if (settingsParityDocument.upstream?.commit !== parityDocument.upstream?.commit) {
  fail("settings parity and upstream parity must pin the same upstream commit");
}
const settingRows = settingsParityDocument.settings ?? [];
if (settingRows.length !== settingsParityDocument.upstream?.live_key_count) {
  fail(
    `settings parity count disagrees: rows=${settingRows.length} expected=${settingsParityDocument.upstream?.live_key_count}`,
  );
}
const settingStatuses = new Set(settingsParityDocument.allowed_statuses ?? []);
const settingKeys = [];
for (const setting of settingRows) {
  requiredText(setting.key, "settings parity key");
  if (settingKeys.includes(setting.key)) fail(`duplicate settings parity key ${setting.key}`);
  settingKeys.push(setting.key);
  if (!settingStatuses.has(setting.status)) {
    fail(`${setting.key} has invalid settings parity status ${setting.status}`);
  }
  if (setting.status === "implemented") {
    requiredText(setting.evidence, `${setting.key}.evidence`);
  } else {
    requiredText(setting.reason, `${setting.key}.reason`);
  }
  if (setting.status === "blocked_by_parity_feature") {
    requiredText(setting.blocking_feature_id, `${setting.key}.blocking_feature_id`);
    if (!(parityDocument.features ?? []).some((feature) => feature.id === setting.blocking_feature_id)) {
      fail(`${setting.key} references missing parity feature ${setting.blocking_feature_id}`);
    }
  }
}

const extractConstStringValues = (source, name, nextName) => {
  const start = source.indexOf(`const ${name}`) >= 0
    ? source.indexOf(`const ${name}`)
    : source.indexOf(`pub const ${name}`);
  if (start < 0) fail(`missing Rust constant ${name}`);
  const end = nextName
    ? source.indexOf(nextName, start)
    : source.indexOf("];", start) + 2;
  if (end <= start) fail(`cannot determine Rust constant boundary for ${name}`);
  return [...source.slice(start, end).matchAll(/"([A-Za-z][A-Za-z0-9_]*)"/g)].map(
    (match) => match[1],
  );
};

const runtimeUpstreamKeys = extractConstStringValues(
  configSource,
  "UPSTREAM_SETTING_KEYS",
  "pub const NATIVE_SETTING_EXTENSION_KEYS",
);
if (!sameSet(settingKeys, runtimeUpstreamKeys)) {
  fail(
    `runtime upstream settings disagree with ledger: runtime=${runtimeUpstreamKeys.join(",")} ledger=${settingKeys.join(",")}`,
  );
}
const settingsFieldStart = viewSource.indexOf("const SETTINGS_FIELDS");
const settingsFieldEnd = viewSource.indexOf("const NATIVE_SETTINGS_FIELDS", settingsFieldStart);
if (settingsFieldStart < 0 || settingsFieldEnd < 0) fail("native settings field registry is missing");
const visibleUpstreamKeys = [
  ...viewSource
    .slice(settingsFieldStart, settingsFieldEnd)
    .matchAll(/key: "([A-Za-z][A-Za-z0-9_]*)"/g),
].map((match) => match[1]);
const directlyRenderedSettingKeys = settingsParityDocument.settings
  .filter((setting) => setting.ui_projection !== "derived")
  .map((setting) => setting.key);
if (!sameSet(directlyRenderedSettingKeys, visibleUpstreamKeys)) {
  fail(
    `visible upstream settings disagree with ledger: view=${visibleUpstreamKeys.join(",")} ledger=${directlyRenderedSettingKeys.join(",")}`,
  );
}
for (const setting of settingsParityDocument.settings.filter(
  (candidate) => candidate.ui_projection === "derived",
)) {
  if (!setting.derived_by || !setting.evidence) {
    fail(`derived setting ${setting.key} must name its native control and evidence`);
  }
  if (!viewSource.includes(`name="${setting.derived_by}"`)) {
    fail(`derived setting ${setting.key} references missing native control ${setting.derived_by}`);
  }
}
const runtimeExtensionKeys = extractConstStringValues(
  configSource,
  "NATIVE_SETTING_EXTENSION_KEYS",
  "const CUSTOM_JSON_SAMPLING_KEYS",
);
if (!sameSet(settingsParityDocument.native_extension_keys ?? [], runtimeExtensionKeys)) {
  fail("native settings extensions disagree with the settings parity ledger");
}

console.log(
  `contracts ok: ${commandIds.size} commands, ${controls.length} affordances, ${effectIds.size} effects, ${parityDocument.features.length} parity rows, ${settingKeys.length} upstream settings, ${requiredFeatureIds.length} blockers`,
);
