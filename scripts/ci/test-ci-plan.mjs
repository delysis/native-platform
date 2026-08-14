#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const planner = path.resolve(import.meta.dirname, "ci-plan.mjs");

function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8" }).trim();
}

function write(repo, relativePath, contents = "fixture\n") {
  const absolutePath = path.join(repo, relativePath);
  fs.mkdirSync(path.dirname(absolutePath), { recursive: true });
  fs.writeFileSync(absolutePath, contents);
}

function commit(repo, message) {
  git(repo, "add", "--all");
  git(repo, "commit", "-qm", message);
  return git(repo, "rev-parse", "HEAD");
}

function makeRepo() {
  const repo = fs.mkdtempSync(path.join(os.tmpdir(), "native-platform-ci-plan-"));
  git(repo, "init", "-q");
  git(repo, "config", "user.email", "ci-plan@example.invalid");
  git(repo, "config", "user.name", "CI planner tests");
  write(repo, "README.md", "base\n");
  const base = commit(repo, "base");
  return { repo, base };
}

function plan(repo, base, head, outputPath) {
  const result = spawnSync(process.execPath, [planner], {
    cwd: repo,
    encoding: "utf8",
    env: {
      ...process.env,
      CI_BASE_SHA: base,
      CI_HEAD_SHA: head,
      GITHUB_EVENT_NAME: "pull_request",
      ...(outputPath ? { GITHUB_OUTPUT: outputPath } : {}),
    },
  });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
}

function fixture(relativePath, { contents = "changed\n", present = [] } = {}) {
  const { repo, base } = makeRepo();
  for (const presentPath of present) write(repo, presentPath, "[workspace]\n");
  const fixtureBase = present.length > 0 ? commit(repo, "fixture presence") : base;
  write(repo, relativePath, contents);
  const head = commit(repo, relativePath);
  return { repo, result: plan(repo, fixtureBase, head) };
}

function fixtureMany(relativePaths, { present = [] } = {}) {
  const { repo, base } = makeRepo();
  for (const presentPath of present) write(repo, presentPath, "[workspace]\n");
  const fixtureBase = present.length > 0 ? commit(repo, "fixture presence") : base;
  for (const relativePath of relativePaths) write(repo, relativePath);
  const head = commit(repo, "fixture changes");
  return { repo, result: plan(repo, fixtureBase, head) };
}

test("docs-only changes require policy and nothing else", () => {
  const { result } = fixture("docs/architecture.md");
  assert.equal(result.risk, "docs");
  assert.deepEqual(result.jobs, ["policy"]);
});

test("CI policy changes use root Linux and macOS without expanding to full", () => {
  const { result } = fixture("scripts/ci/ci-plan.mjs");
  assert.equal(result.risk, "behavior");
  assert.equal(result.flags.root, true);
  assert.equal(result.flags.platform_macos, true);
  assert.equal(result.flags.full, false);
  assert.deepEqual(result.jobs, ["policy", "root-linux", "platform-macos"]);
});

test("macOS release tooling selects only policy and the macOS syntax lane", () => {
  for (const relativePath of [
    "scripts/release-macos.sh",
    "scripts/smoke-macos-app.sh",
    "scripts/find-embedded-model.mjs",
    "scripts/product-state-backup.mjs",
  ]) {
    const { result } = fixture(relativePath);
    assert.equal(result.risk, "release");
    assert.equal(result.flags.root, false);
    assert.equal(result.flags.platform_macos, true);
    assert.equal(result.flags.full, false);
    assert.deepEqual(result.jobs, ["policy", "platform-macos"]);
  }
});

test("Native changes require root, Native, and macOS product coverage", () => {
  const { result } = fixture("crates/native/crates/llama-native-engine/src/lib.rs");
  assert.equal(result.risk, "behavior");
  assert.equal(result.flags.root, true);
  assert.equal(result.flags.native, true);
  assert.equal(result.flags.platform_linux, true);
  assert.equal(result.flags.platform_macos, true);
  assert.ok(!("platform_windows" in result.flags));
  assert.ok(result.jobs.includes("native-linux"));
});

test("Attachment inspection changes select Attachment and fuzz only", () => {
  const { result } = fixture(
    "crates/services/attachment/crates/attachment-native-inspect/src/lib.rs",
  );
  assert.equal(result.flags.attachment, true);
  assert.equal(result.flags.fuzz, true);
  assert.equal(result.flags.speech, false);
  assert.ok(result.jobs.includes("attachment-linux"));
  assert.ok(result.jobs.includes("fuzz-build"));
});

test("Information changes select Information", () => {
  const { result } = fixture(
    "crates/services/information/crates/information-native-store/src/lib.rs",
  );
  assert.equal(result.flags.information, true);
  assert.equal(result.flags.full, false);
});

test("Speech Apple changes select Speech and platform coverage", () => {
  const { result } = fixture(
    "crates/services/speech/crates/speech-native-platform/src/apple.rs",
  );
  assert.equal(result.flags.speech, true);
  assert.equal(result.flags.platform_macos, true);
  assert.ok(!result.jobs.includes("platform-windows"));
});

test("Mom native source selects its product and macOS parity without root duplication", () => {
  const { result } = fixture("products/mom/apps/mom-llama/src-tauri/src/commands.rs", {
    present: ["products/mom/Cargo.toml"],
  });
  assert.equal(result.presence.mom, true);
  assert.equal(result.flags.mom, true);
  assert.equal(result.flags.root, false);
  assert.equal(result.flags.platform_macos, true);
  assert.deepEqual(result.jobs, ["policy", "mom-linux", "platform-macos"]);
});

test("the PR 22 Mom diff has the focused product, frontend, and macOS plan", () => {
  const { result } = fixtureMany(
    [
      "products/mom/apps/mom-llama/src-tauri/src/commands.rs",
      "products/mom/apps/mom-llama/src-tauri/src/view.rs",
      "products/mom/apps/mom-llama/ui/coop-hx.js",
      "products/mom/crates/mom-llama-cli/src/main.rs",
      "products/mom/crates/mom-llama-runtime/src/config.rs",
      "products/mom/crates/mom-llama-runtime/src/server.rs",
      "products/mom/crates/mom-llama-runtime/tests/runtime.rs",
    ],
    { present: ["products/mom/Cargo.toml"] },
  );
  assert.equal(result.flags.root, false);
  assert.equal(result.flags.mom, true);
  assert.equal(result.flags.frontend_mom, true);
  assert.equal(result.flags.platform_macos, true);
  assert.deepEqual(result.jobs, [
    "policy",
    "mom-linux",
    "frontend",
    "platform-macos",
  ]);
});

test("product package scripts select their owned frontend checks", () => {
  const mom = fixture("products/mom/apps/mom-llama/package.json", {
    contents: '{"scripts":{"check:frontend":"node --check ui/coop-hx.js"}}\n',
    present: ["products/mom/Cargo.toml"],
  }).result;
  assert.equal(mom.flags.frontend_mom, true);
  assert.deepEqual(mom.jobs, ["policy", "mom-linux", "frontend"]);

  const fte = fixture("products/fte/package.json").result;
  assert.equal(fte.flags.frontend_fte, true);
  assert.ok(fte.jobs.includes("frontend"));
});

test("Mom dependency metadata remains conservative", () => {
  const { result } = fixture("products/mom/Cargo.toml", {
    contents: "[workspace]\nmembers = []\n",
    present: ["products/mom/Cargo.toml"],
  });
  assert.equal(result.flags.root, true);
  assert.equal(result.flags.mom, true);
  assert.equal(result.flags.dependency_graph, true);
  assert.equal(result.flags.platform_macos, true);
  assert.deepEqual(result.jobs, [
    "policy",
    "root-linux",
    "mom-linux",
    "platform-macos",
    "dependency-graph",
  ]);
});

test("Loom Svelte source selects Loom frontend when Loom is present", () => {
  const { result } = fixture("products/loom/apps/loom/src/App.svelte", {
    present: ["products/loom/apps/loom/src-tauri/Cargo.toml"],
  });
  assert.equal(result.presence.loom, true);
  assert.equal(result.flags.loom, true);
  assert.equal(result.flags.frontend_loom, true);
  assert.ok(result.jobs.includes("loom-linux"));
  assert.ok(result.jobs.includes("frontend"));
});

test("root Cargo metadata forces the complete present graph", () => {
  const { result } = fixture("Cargo.toml", {
    contents: "[workspace]\n",
    present: [
      "products/mom/Cargo.toml",
      "products/loom/apps/loom/src-tauri/Cargo.toml",
    ],
  });
  assert.equal(result.risk, "dependency");
  assert.equal(result.flags.full, true);
  assert.equal(result.flags.dependency_graph, true);
  assert.ok(result.jobs.includes("mom-linux"));
  assert.ok(result.jobs.includes("loom-linux"));
  assert.ok(result.jobs.includes("platform-macos"));
  assert.ok(!result.jobs.includes("platform-windows"));
});

test("migration maps retain policy verification without history replay", () => {
  const { result } = fixture("migration/example.commit-map");
  assert.equal(result.risk, "import");
  assert.deepEqual(result.jobs, ["policy"]);
});

test("unknown additions and deletions fail closed to full", () => {
  const added = fixture("unexpected/new-input.bin").result;
  assert.equal(added.flags.full, true);

  const { repo, base } = makeRepo();
  write(repo, "unexpected/old-input.bin");
  const withUnknown = commit(repo, "add unknown");
  fs.rmSync(path.join(repo, "unexpected/old-input.bin"));
  const deleted = commit(repo, "delete unknown");
  assert.equal(plan(repo, withUnknown, deleted).flags.full, true);
  assert.notEqual(base, withUnknown);
});

test("renaming runtime source into docs retains the source-side coverage", () => {
  const { repo } = makeRepo();
  write(repo, "crates/native/crates/llama-native-types/src/old.rs");
  const sourceHead = commit(repo, "add native source");
  fs.mkdirSync(path.join(repo, "docs"), { recursive: true });
  git(
    repo,
    "mv",
    "crates/native/crates/llama-native-types/src/old.rs",
    "docs/old.md",
  );
  const renamedHead = commit(repo, "move source into docs");
  const result = plan(repo, sourceHead, renamedHead);
  assert.equal(result.flags.native, true);
  assert.equal(result.flags.root, true);
});

test("GitHub output carries the compact plan and every declared flag", () => {
  const { repo, base } = makeRepo();
  write(repo, "docs/note.md");
  const head = commit(repo, "docs");
  const outputPath = path.join(repo, "github-output.txt");
  const result = plan(repo, base, head, outputPath);
  const output = fs.readFileSync(outputPath, "utf8");
  assert.match(output, /^plan_json=/m);
  for (const flag of Object.keys(result.flags)) {
    assert.match(output, new RegExp(`^${flag}=(?:true|false)$`, "m"));
  }
});
