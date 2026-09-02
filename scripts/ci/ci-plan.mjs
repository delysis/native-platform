#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import {
  computeMetadataSelection,
  legacyEquivalenceReport,
  readCargoMetadata,
  unavailableSelection,
} from "./ci-metadata-shadow.mjs";

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

let risk = "docs";

function markBehavior() {
  if (risk === "docs" || risk === "import") risk = "behavior";
  flags.platform_linux = true;
}

function markDependency() {
  risk = "dependency";
  flags.dependency_graph = true;
  flags.platform_linux = true;
}

function markPlatform() {
  flags.platform_linux = true;
  flags.platform_macos = true;
}

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

function forceFull(nextRisk = "dependency") {
  forceFullFlags(flags);
  markPlatform();
  risk = nextRisk;
}

function under(candidate, prefix) {
  return candidate === prefix || candidate.startsWith(`${prefix}/`);
}

function isMomContractDependencyChange(changedPath) {
  const contractRoots = [
    "crates/native",
    "crates/services/attachment",
    "crates/services/information",
    "crates/services/speech",
    "products/fte",
  ];
  if (!contractRoots.some((prefix) => under(changedPath, prefix))) return false;
  if (
    changedPath.endsWith(".md") ||
    changedPath.includes("/docs/") ||
    changedPath.includes("/fixtures/") ||
    changedPath.includes("/receipts/") ||
    changedPath.includes("/research/") ||
    changedPath.includes("/README") ||
    changedPath.includes("/LICENSE")
  ) {
    return false;
  }
  return (
    /(?:^|\/)(?:Cargo\.toml|Cargo\.lock|build\.rs)$/.test(changedPath) ||
    /\.(?:rs|json|toml|proto|wit)$/.test(changedPath)
  );
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

for (const changedPath of changed) {
  let recognized = false;

  if (isIgnoredInventoryChange(changedPath)) flags.ignored_tests = true;

  if (
    changedPath.endsWith(".md") ||
    under(changedPath, "docs") ||
    under(changedPath, "templates") ||
    changedPath === "README.md" ||
    changedPath === "AGENTS.md" ||
    changedPath === "CONTRIBUTING.md" ||
    changedPath.startsWith("LICENSE") ||
    changedPath === "SECURITY.md"
  ) {
    recognized = true;
  }

  if (under(changedPath, "migration") || changedPath.endsWith(".commit-map")) {
    recognized = true;
    if (risk === "docs") risk = "import";
  }

  if (
    changedPath === "Cargo.toml" ||
    changedPath === "Cargo.lock" ||
    changedPath === "rust-toolchain.toml"
  ) {
    recognized = true;
    forceFull();
  } else if (
    changedPath.endsWith("/Cargo.toml") ||
    changedPath.endsWith("/Cargo.lock") ||
    changedPath.endsWith("/build.rs")
  ) {
    recognized = true;
    markDependency();
    flags.root = true;
    markPlatform();
  }

  if (
    under(changedPath, ".github") ||
    under(changedPath, "scripts/ci") ||
    under(changedPath, "ci") ||
    under(changedPath, "xtask")
  ) {
    recognized = true;
    flags.policy = true;
    if (
      changedPath.includes("ci-plan") ||
      changedPath.includes("ci-required") ||
      changedPath.endsWith("/ci-pr.yml") ||
      changedPath.endsWith("/ci-full.yml")
    ) {
      flags.root = true;
      markBehavior();
      flags.platform_macos = true;
    }
  }

  if (under(changedPath, "release") || changedPath.includes("release.yml")) {
    recognized = true;
    forceFull("release");
  }

  if (
    changedPath === "scripts/release-macos.sh" ||
    changedPath === "scripts/smoke-macos-app.sh" ||
    changedPath === "scripts/find-embedded-model.mjs" ||
    changedPath === "scripts/product-state-backup.mjs"
  ) {
    recognized = true;
    risk = "release";
    flags.platform_macos = true;
  }

  if (under(changedPath, "crates/native")) {
    recognized = true;
    flags.native = true;
    flags.root = true;
    markBehavior();
    markPlatform();
  }

  if (under(changedPath, "products/fte")) {
    recognized = true;
    flags.gateway = true;
    flags.root = true;
    markBehavior();
    if (
      changedPath.includes("/ui/") ||
      changedPath.endsWith("/package.json") ||
      /\.(?:js|mjs|ts|css|html)$/.test(changedPath)
    ) {
      flags.frontend_fte = true;
    }
    if (changedPath.includes("src-tauri") || changedPath.includes("tauri")) {
      markPlatform();
    }
  }

  if (under(changedPath, "crates/services/attachment")) {
    recognized = true;
    flags.attachment = true;
    markBehavior();
    if (
      changedPath.includes("/fuzz/") ||
      changedPath.includes("inspect") ||
      changedPath.includes("parser")
    ) {
      flags.fuzz = true;
    }
  }

  if (under(changedPath, "crates/services/information")) {
    recognized = true;
    flags.information = true;
    markBehavior();
    if (changedPath.includes("tauri") || changedPath.includes("platform")) {
      markPlatform();
    }
  }

  if (under(changedPath, "crates/services/speech")) {
    recognized = true;
    flags.speech = true;
    markBehavior();
    markPlatform();
  }

  if (under(changedPath, "products/mom")) {
    recognized = true;
    flags.mom = true;
    markBehavior();
    if (
      changedPath.includes("/ui/") ||
      changedPath.endsWith("/package.json") ||
      /\.(?:js|mjs|ts|css|html)$/.test(changedPath)
    ) {
      flags.frontend_mom = true;
    }
    if (changedPath.includes("src-tauri") || changedPath.includes("native_runtime")) {
      markPlatform();
    }
  }

  if (under(changedPath, "products/loom")) {
    recognized = true;
    flags.loom = true;
    markBehavior();
    if (
      changedPath.includes("/apps/loom/") ||
      /\.(?:svelte|js|mjs|ts|css|html)$/.test(changedPath)
    ) {
      flags.frontend_loom = true;
    }
    if (changedPath.includes("src-tauri") || changedPath.includes("backend-llama")) {
      markPlatform();
    }
  }

  if (
    changedPath === "pnpm-lock.yaml" ||
    changedPath === "pnpm-workspace.yaml" ||
    changedPath === "package.json"
  ) {
    recognized = true;
    flags.frontend_fte = true;
    flags.frontend_mom = presence.mom;
    flags.frontend_loom = presence.loom;
    markDependency();
  }

  if (!recognized) forceFull();
}

// Preserve the former path planner as an observational baseline. It no longer
// selects jobs; any unexplained reduction against it fails closed to full.
const momContractPaths = changed.filter(isMomContractDependencyChange);
const momContractOverlay = {
  applied: presence.mom && momContractPaths.length > 0,
  paths: momContractPaths,
  reason:
    "temporary conservative Mom coverage for Native, Attachment, Speech, Information, and FTE contract changes",
};
if (momContractOverlay.applied) {
  flags.mom = true;
  markBehavior();
}

const legacyFlags = { ...flags };
const legacyRisk = risk;
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

const generatedFlags = Object.fromEntries(
  Object.keys(flags).map((name) => [name, name === "policy"]),
);

function applyPrimaryGroup(group) {
  switch (group) {
    case "native":
      generatedFlags.native = true;
      generatedFlags.root = true;
      generatedFlags.platform_macos = true;
      break;
    case "gateway":
    case "product-fte":
      generatedFlags.gateway = true;
      generatedFlags.root = true;
      break;
    case "service-attachment":
      generatedFlags.attachment = true;
      break;
    case "service-information":
      generatedFlags.information = true;
      break;
    case "service-speech":
      generatedFlags.speech = true;
      generatedFlags.platform_macos = true;
      break;
    case "product-mom":
      generatedFlags.mom = presence.mom;
      break;
    case "product-loom":
      generatedFlags.loom = presence.loom;
      break;
    case "diagnostic":
      break;
    default:
      throw new Error(`primary group has no CI lane mapping: ${group}`);
  }
}

for (const group of dependencySelection.primary_groups) applyPrimaryGroup(group);
if (dependencySelection.primary_groups.length > 0) generatedFlags.platform_linux = true;

const closure = new Set(dependencySelection.reverse_dependency_closure);
const secondary = packageGroups.secondary ?? {};
if ((secondary["platform-linux"] ?? []).some((name) => closure.has(name))) {
  generatedFlags.platform_linux = true;
}
if ((secondary["platform-macos"] ?? []).some((name) => closure.has(name))) {
  generatedFlags.platform_macos = true;
}
if ((secondary.fuzz ?? []).some((name) => closure.has(name))) {
  generatedFlags.fuzz = true;
}
for (const packageName of (secondary.frontend ?? []).filter((name) => closure.has(name))) {
  if (packageGroups.primary["product-fte"]?.includes(packageName)) {
    generatedFlags.frontend_fte = true;
  }
  if (packageGroups.primary["product-mom"]?.includes(packageName) && presence.mom) {
    generatedFlags.frontend_mom = true;
  }
  if (packageGroups.primary["product-loom"]?.includes(packageName) && presence.loom) {
    generatedFlags.frontend_loom = true;
  }
}

for (const effect of dependencySelection.effects) {
  if (effect in generatedFlags) generatedFlags[effect] = true;
}
generatedFlags.frontend_mom &&= presence.mom;
generatedFlags.frontend_loom &&= presence.loom;
// Loom's WebKit interaction suite is a macOS product gate, not a Linux
// frontend step. Any renderer change that selects the Loom frontend must also
// schedule the macOS Loom matrix entry or the regression suite can be skipped.
if (generatedFlags.frontend_loom) generatedFlags.platform_macos = true;

for (const changedPath of changed) {
  const basename = path.posix.basename(changedPath);
  if (isIgnoredInventoryChange(changedPath)) generatedFlags.ignored_tests = true;
  if (["Cargo.toml", "Cargo.lock", "build.rs"].includes(basename)) {
    generatedFlags.dependency_graph = true;
    generatedFlags.root = true;
    generatedFlags.platform_linux = true;
    generatedFlags.platform_macos = true;
  }
}
if (dependencySelection.fallback === "full") forceFullFlags(generatedFlags);

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

function selectionSurface(selectedFlags) {
  return [
    ...jobsFor(selectedFlags).map((name) => `job:${name}`),
    ...macosMatrixFor(selectedFlags).map((name) => `macos:${name}`),
    ...["frontend_fte", "frontend_mom", "frontend_loom"]
      .filter((name) => selectedFlags[name])
      .map((name) => `flag:${name}`),
    ...(selectedFlags.full ? ["flag:full"] : []),
  ].sort();
}

const legacySurface = selectionSurface(legacyFlags);
const generatedSurface = selectionSurface(generatedFlags);
const generatedSet = new Set(generatedSurface);
const missingLegacy = legacySurface.filter((item) => !generatedSet.has(item));
const reductionEvidence = dependencySelection.file_classifications
  .filter((record) => record.authorizes_legacy_reduction)
  .map((record) => ({ path: record.path, rule: record.rule, evidence: record.evidence }));
const allChangesAuthorizeReduction =
  dependencySelection.file_classifications.length > 0 &&
  dependencySelection.file_classifications.every(
    (record) => record.authorizes_legacy_reduction === true,
  );
if (missingLegacy.length > 0 && !allChangesAuthorizeReduction) {
  forceFullFlags(generatedFlags);
  dependencySelection = {
    ...dependencySelection,
    fallback: "full",
    fallback_reasons: [
      ...dependencySelection.fallback_reasons,
      "legacy_reduction_without_evidence",
    ],
  };
}

Object.assign(flags, generatedFlags);
if (flags.full && legacyRisk !== "release") risk = "dependency";
else risk = legacyRisk;

const jobs = jobsFor(flags);
const macosMatrix = macosMatrixFor(flags);
const dependencyShadow = legacyEquivalenceReport({
  legacySurface,
  generatedSurface,
  finalSurface: selectionSurface(flags),
  reductionEvidence,
  fallbackReasons: dependencySelection.fallback_reasons,
});

const plan = {
  schema: "native-platform.ci-plan.v1",
  event: eventName,
  base,
  head,
  risk,
  changed,
  presence,
  flags,
  conservative_overlays: {
    mom_contracts: { ...momContractOverlay, applied_to_selection: false },
  },
  dependency_selection: dependencySelection,
  dependency_shadow: dependencyShadow,
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
