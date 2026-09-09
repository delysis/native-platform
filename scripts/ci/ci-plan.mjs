#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import {
  computeMetadataSelection,
  readCargoMetadata,
  unavailableSelection,
} from "./ci-metadata-selection.mjs";

function requiredEnv(name) {
  const value = process.env[name];
  if (!value) throw new Error(`missing ${name}`);
  return value;
}

const base = requiredEnv("CI_BASE_SHA");
const head = requiredEnv("CI_HEAD_SHA");
const eventName = process.env.GITHUB_EVENT_NAME ?? "local";
const plannerRoot = path.resolve(import.meta.dirname, "../..");
const ignoredRegistry = JSON.parse(
  fs.readFileSync(path.join(plannerRoot, "ci/ignored-tests.json"), "utf8"),
);
if (ignoredRegistry.schema !== "native-platform.ignored-tests.v2") {
  throw new Error("CI planner requires ignored-test registry schema v2");
}

// Include deletions. A removed build, policy, or dependency file can be at
// least as consequential as an addition, and must never disappear from the
// plan merely because it no longer exists at HEAD.
const changed = execFileSync(
  "git",
  [
    "diff",
    "--name-only",
    "--no-renames",
    "-z",
    "--diff-filter=ACDMRTUXB",
    base,
    head,
  ],
  { encoding: "utf8" },
)
  .split("\0")
  .filter(Boolean)
  .sort();

const presence = {
  mom:
    fs.existsSync("products/mom/Cargo.toml") ||
    fs.existsSync("products/mom/crates/mom-llama-runtime/Cargo.toml"),
  loom: fs.existsSync("products/loom/apps/loom/src-tauri/Cargo.toml"),
};

const flags = {
  policy: true,
  root: false,
  native: false,
  gateway: false,
  attachment: false,
  information: false,
  speech: false,
  mom: false,
  loom: false,
  frontend_fte: false,
  frontend_mom: false,
  frontend_loom: false,
  dependency_graph: false,
  fuzz: false,
  platform_linux: false,
  platform_macos: false,
  ignored_tests: false,
  full: false,
};

function forceFullFlags(target) {
  target.full = true;
  target.root = true;
  target.native = true;
  target.gateway = true;
  target.attachment = true;
  target.information = true;
  target.speech = true;
  target.mom = presence.mom;
  target.loom = presence.loom;
  target.frontend_fte = true;
  target.frontend_mom = presence.mom;
  target.frontend_loom = presence.loom;
  target.dependency_graph = true;
  target.fuzz = true;
  target.ignored_tests = true;
  target.platform_linux = true;
  target.platform_macos = true;
}

function under(candidate, prefix) {
  return candidate === prefix || candidate.startsWith(`${prefix}/`);
}

function isIgnoredInventoryChange(changedPath) {
  const basename = path.posix.basename(changedPath);
  return (
    under(changedPath, ".github/workflows") ||
    under(changedPath, "ci") ||
    under(changedPath, "scripts/ci") ||
    changedPath.endsWith(".rs") ||
    ["Cargo.toml", "Cargo.lock", "build.rs"].includes(basename) ||
    changedPath === "rust-toolchain" ||
    changedPath === "rust-toolchain.toml" ||
    under(changedPath, ".cargo")
  );
}

const packageGroups = JSON.parse(
  fs.readFileSync(path.join(plannerRoot, "ci/package-groups.json"), "utf8"),
);
const pathExceptions = JSON.parse(
  fs.readFileSync(path.join(plannerRoot, "ci/ci-path-exceptions.json"), "utf8"),
);

let dependencySelection;
try {
  dependencySelection = computeMetadataSelection({
    metadata: readCargoMetadata(process.cwd()),
    repoRoot: process.cwd(),
    changed,
    packageGroups,
    pathExceptions,
  });
} catch (error) {
  dependencySelection = unavailableSelection(
    String(error.message ?? error),
    packageGroups,
  );
}

function applyPrimaryGroup(group) {
  switch (group) {
    case "desktop":
      flags.root = true;
      break;
    case "native":
      flags.native = true;
      flags.root = true;
      flags.platform_macos = true;
      break;
    case "gateway":
    case "product-fte":
      flags.gateway = true;
      flags.root = true;
      break;
    case "service-attachment":
      flags.attachment = true;
      break;
    case "service-information":
      flags.information = true;
      break;
    case "service-speech":
      flags.speech = true;
      flags.platform_macos = true;
      break;
    case "product-mom":
      flags.mom = presence.mom;
      break;
    case "product-loom":
      flags.loom = presence.loom;
      break;
    case "diagnostic":
      break;
    default:
      throw new Error(`primary group has no CI lane mapping: ${group}`);
  }
}

for (const group of dependencySelection.primary_groups) applyPrimaryGroup(group);
if (dependencySelection.primary_groups.length > 0) flags.platform_linux = true;

const closure = new Set(dependencySelection.reverse_dependency_closure);
const secondary = packageGroups.secondary ?? {};
if ((secondary["platform-linux"] ?? []).some((name) => closure.has(name))) {
  flags.platform_linux = true;
}
if ((secondary["platform-macos"] ?? []).some((name) => closure.has(name))) {
  flags.platform_macos = true;
}
if ((secondary.fuzz ?? []).some((name) => closure.has(name))) {
  flags.fuzz = true;
}
for (const packageName of (secondary.frontend ?? []).filter((name) => closure.has(name))) {
  if (packageGroups.primary["product-fte"]?.includes(packageName)) {
    flags.frontend_fte = true;
  }
  if (packageGroups.primary["product-mom"]?.includes(packageName) && presence.mom) {
    flags.frontend_mom = true;
  }
  if (packageGroups.primary["product-loom"]?.includes(packageName) && presence.loom) {
    flags.frontend_loom = true;
  }
}

for (const effect of dependencySelection.effects) {
  if (effect in flags) flags[effect] = true;
}
flags.frontend_mom &&= presence.mom;
flags.frontend_loom &&= presence.loom;
// Loom's WebKit interaction suite is a macOS product gate, not a Linux
// frontend step. Any renderer change that selects the Loom frontend must also
// schedule the macOS Loom matrix entry or the regression suite can be skipped.
if (flags.frontend_loom) flags.platform_macos = true;

for (const changedPath of changed) {
  const basename = path.posix.basename(changedPath);
  if (isIgnoredInventoryChange(changedPath)) flags.ignored_tests = true;
  if (["Cargo.toml", "Cargo.lock", "build.rs"].includes(basename)) {
    flags.dependency_graph = true;
    flags.root = true;
    flags.platform_linux = true;
    flags.platform_macos = true;
  }
}
if (dependencySelection.fallback === "full") forceFullFlags(flags);

function jobsFor(selectedFlags) {
  const selected = ["policy"];
  if (selectedFlags.root || selectedFlags.full) selected.push("root-linux");
  if (selectedFlags.native || selectedFlags.full) selected.push("native-linux");
  if (selectedFlags.gateway || selectedFlags.full) selected.push("gateway-linux");
  if (selectedFlags.attachment || selectedFlags.full) selected.push("attachment-linux");
  if (selectedFlags.information || selectedFlags.full) {
    selected.push("information-linux", "information-windows");
  }
  if (selectedFlags.speech || selectedFlags.full) selected.push("speech-linux");
  if (presence.mom && (selectedFlags.mom || selectedFlags.full)) {
    selected.push("mom-linux", "mom-windows");
  }
  if (presence.loom && (selectedFlags.loom || selectedFlags.full)) {
    selected.push("loom-linux", "loom-windows");
  }
  if (
    selectedFlags.frontend_fte ||
    selectedFlags.frontend_mom ||
    selectedFlags.frontend_loom ||
    selectedFlags.full
  ) {
    selected.push("frontend");
  }
  if (selectedFlags.platform_macos || selectedFlags.full) selected.push("platform-macos");
  if (selectedFlags.ignored_tests || selectedFlags.full) selected.push("ignored-tests");
  if (selectedFlags.dependency_graph || selectedFlags.full) selected.push("dependency-graph");
  if (selectedFlags.fuzz || selectedFlags.full) selected.push("fuzz-build");
  return [...new Set(selected)];
}

function macosMatrixFor(selectedFlags) {
  const matrix = [];
  if (selectedFlags.platform_macos || selectedFlags.full) {
    matrix.push("release");
    if (selectedFlags.root || selectedFlags.native || selectedFlags.gateway || selectedFlags.full) {
      matrix.push("root");
    }
    if (presence.mom && (selectedFlags.mom || selectedFlags.full)) matrix.push("mom");
    if (selectedFlags.attachment || selectedFlags.full) matrix.push("attachment");
    if (selectedFlags.information || selectedFlags.full) matrix.push("information");
    if (selectedFlags.speech || selectedFlags.full) matrix.push("speech");
    if (
      presence.loom &&
      (selectedFlags.loom || selectedFlags.frontend_loom || selectedFlags.full)
    ) {
      matrix.push("loom");
    }
  }
  return matrix;
}

const risk = dependencySelection.effects.includes("release") ? "release"
  : flags.dependency_graph || flags.full ? "dependency"
  : dependencySelection.primary_groups.length > 0 || flags.platform_macos ? "behavior"
  : dependencySelection.effects.includes("import") ? "import" : "docs";
const jobs = jobsFor(flags);
const macosMatrix = macosMatrixFor(flags);

const plan = {
  schema: "native-platform.ci-plan.v1",
  event: eventName,
  base,
  head,
  risk,
  changed,
  presence,
  flags,
  dependency_selection: dependencySelection,
  macos_matrix: macosMatrix,
  jobs,
};

const compact = JSON.stringify(plan);
console.log(JSON.stringify(plan, null, 2));

if (process.env.GITHUB_OUTPUT) {
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `plan_json=${compact}\n`);
  for (const [key, value] of Object.entries(flags)) {
    fs.appendFileSync(process.env.GITHUB_OUTPUT, `${key}=${value}\n`);
  }
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `mom_present=${presence.mom}\n`);
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `loom_present=${presence.loom}\n`);
  fs.appendFileSync(
    process.env.GITHUB_OUTPUT,
    `macos_matrix=${JSON.stringify(macosMatrix)}\n`,
  );
}
