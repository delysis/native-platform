#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import {
  computeReverseDependencyShadow,
  readCargoMetadata,
  unavailableShadow,
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
const ignoredTestSources = new Set(
  ignoredRegistry.entries.map((entry) => entry.source),
);
const ignoredTargetManifests = new Set(
  ignoredRegistry.cargo_targets.map((target) => target.manifest_path),
);
const ignoredInventoryInfrastructure = new Set([
  ".github/workflows/ci-full.yml",
  ".github/workflows/ci-pr.yml",
  "Cargo.lock",
  "Cargo.toml",
  "ci/ignored-tests.json",
  "scripts/ci/ci-plan.mjs",
  "scripts/ci/ci-required.mjs",
  "scripts/ci/test-ci-plan.mjs",
  "scripts/ci/test-ci-required.mjs",
  "scripts/ci/test-ignored-tests.mjs",
  "scripts/ci/test-workflows.mjs",
  "scripts/ci/validate-ignored-tests.mjs",
]);

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

function forceFull(nextRisk = "dependency") {
  flags.full = true;
  flags.root = true;
  flags.native = true;
  flags.gateway = true;
  flags.attachment = true;
  flags.information = true;
  flags.speech = true;
  flags.mom = presence.mom;
  flags.loom = presence.loom;
  flags.frontend_fte = true;
  flags.frontend_mom = presence.mom;
  flags.frontend_loom = presence.loom;
  flags.dependency_graph = true;
  flags.fuzz = true;
  flags.ignored_tests = true;
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

function gitBlobContains(revision, changedPath, pattern) {
  try {
    const source = execFileSync("git", ["show", `${revision}:${changedPath}`], {
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    });
    return pattern.test(source);
  } catch {
    return false;
  }
}

function isIgnoredInventoryChange(changedPath) {
  if (
    ignoredInventoryInfrastructure.has(changedPath) ||
    ignoredTestSources.has(changedPath) ||
    ignoredTargetManifests.has(changedPath)
  ) {
    return true;
  }
  if (!changedPath.endsWith(".rs")) return false;
  const ignoredAttribute = /#\s*\[\s*ignore\b/;
  return (
    gitBlobContains(base, changedPath, ignoredAttribute) ||
    gitBlobContains(head, changedPath, ignoredAttribute)
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

// This is an explicitly conservative bridge while metadata selection remains
// observational. Mom already consumes Native, Attachment and FTE contracts,
// and the staged architecture will consume Speech and Information contracts.
// A contract-family change therefore exercises Mom even where today's Cargo
// graph has not acquired the future edge yet.
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

function selectedPrimaryGroups(primary) {
  if (flags.full || flags.root) return Object.keys(primary);
  const groups = [];
  if (flags.native) groups.push("native");
  if (flags.gateway) groups.push("gateway");
  if (flags.attachment) groups.push("service-attachment");
  if (flags.information) groups.push("service-information");
  if (flags.speech) groups.push("service-speech");
  if (flags.mom) groups.push("product-mom");
  if (flags.loom) groups.push("product-loom");
  return groups;
}

let dependencyShadow;
try {
  const packageGroups = JSON.parse(fs.readFileSync("ci/package-groups.json", "utf8"));
  const pathExceptions = JSON.parse(
    fs.readFileSync("ci/ci-path-exceptions.json", "utf8"),
  );
  dependencyShadow = computeReverseDependencyShadow({
    metadata: readCargoMetadata(process.cwd()),
    repoRoot: process.cwd(),
    changed,
    packageGroups,
    pathExceptions,
    authoritativePrimaryGroups: selectedPrimaryGroups(packageGroups.primary),
  });
} catch (error) {
  dependencyShadow = unavailableShadow(String(error.message ?? error));
}

const jobs = ["policy"];
if (flags.root || flags.full) jobs.push("root-linux");
if (flags.native || flags.full) jobs.push("native-linux");
if (flags.gateway || flags.full) jobs.push("gateway-linux");
if (flags.attachment || flags.full) jobs.push("attachment-linux");
if (flags.information || flags.full) jobs.push("information-linux");
if (flags.speech || flags.full) jobs.push("speech-linux");
if (presence.mom && (flags.mom || flags.full)) jobs.push("mom-linux");
if (presence.loom && (flags.loom || flags.full)) jobs.push("loom-linux");
if (flags.frontend_fte || flags.frontend_mom || flags.frontend_loom || flags.full) {
  jobs.push("frontend");
}
if (flags.platform_macos || flags.full) jobs.push("platform-macos");
if (flags.ignored_tests || flags.full) jobs.push("ignored-tests");
if (flags.dependency_graph || flags.full) jobs.push("dependency-graph");
if (flags.fuzz || flags.full) jobs.push("fuzz-build");

const macosMatrix = [];
if (flags.platform_macos || flags.full) {
  macosMatrix.push("release");
  if (flags.root || flags.native || flags.gateway || flags.full) {
    macosMatrix.push("root");
  }
  if (presence.mom && (flags.mom || flags.full)) macosMatrix.push("mom");
  if (flags.attachment || flags.full) macosMatrix.push("attachment");
  if (flags.information || flags.full) macosMatrix.push("information");
  if (flags.speech || flags.full) macosMatrix.push("speech");
  if (presence.loom && (flags.loom || flags.full)) macosMatrix.push("loom");
}

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
    mom_contracts: momContractOverlay,
  },
  dependency_shadow: dependencyShadow,
  macos_matrix: macosMatrix,
  jobs: [...new Set(jobs)],
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
