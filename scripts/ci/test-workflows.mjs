#!/usr/bin/env node

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

const root = path.resolve(import.meta.dirname, "../..");
const prPath = path.join(root, ".github/workflows/ci-pr.yml");
const fullPath = path.join(root, ".github/workflows/ci-full.yml");

function read(file) {
  return fs.readFileSync(file, "utf8");
}

function externalActionUses(source) {
  return [...source.matchAll(/^\s*-\s+uses:\s+([^\s#]+).*$/gm)]
    .map((match) => match[1])
    .filter((value) => !value.startsWith("./") && !value.startsWith("docker://"));
}

test("only the targeted PR and full workflows remain active", () => {
  assert.equal(fs.existsSync(path.join(root, ".github/workflows/ci.yml")), false);
  assert.equal(fs.existsSync(prPath), true);
  assert.equal(fs.existsSync(fullPath), true);
});

test("PR workflow is always triggered and has one truthful aggregate", () => {
  const source = read(prPath);
  assert.match(source, /^on:\n\s+pull_request:\s*$/m);
  assert.doesNotMatch(source, /^\s+paths(?:-ignore)?:/m);
  assert.match(source, /^\s{2}ci-required:\n\s{4}name: ci-required$/m);
  assert.match(source, /^\s{4}if: always\(\)$/m);
  assert.match(source, /CI_NEEDS_JSON:\s*\$\{\{ toJSON\(needs\) \}\}/);
  assert.match(source, /node scripts\/ci\/ci-required\.mjs/);
  assert.match(source, /node --test scripts\/ci\/test-ci-plan\.mjs scripts\/ci\/test-ci-required\.mjs scripts\/ci\/test-workflows\.mjs/);
});

test("PR workflow exposes every targeted partition and future product guards", () => {
  const source = read(prPath);
  for (const job of [
    "plan",
    "policy",
    "root-linux",
    "native-linux",
    "gateway-linux",
    "attachment-linux",
    "information-linux",
    "speech-linux",
    "mom-linux",
    "loom-linux",
    "frontend",
    "platform-macos",
    "platform-windows",
    "dependency-graph",
    "import-history",
    "fuzz-build",
  ]) {
    assert.match(source, new RegExp(`^  ${job}:`, "m"), `missing ${job}`);
  }
  assert.match(source, /mom_present == 'true'/);
  assert.match(source, /loom_present == 'true'/);
  assert.match(source, /group: ci-pr-/);
  assert.match(source, /cancel-in-progress: true/);
});

test("frontend jobs provision the Rust tooling used by current package scripts", () => {
  const prFrontend = read(prPath).match(/^  frontend:[\s\S]*?(?=^  platform-macos:)/m)?.[0];
  const fullFrontend = read(fullPath).match(/^  frontend:[\s\S]*?(?=^  policy-and-graphs:)/m)?.[0];
  for (const block of [prFrontend, fullFrontend]) {
    assert.ok(block, "frontend job block is missing");
    assert.match(block, /dtolnay\/rust-toolchain@[0-9a-f]{40}/);
    assert.match(block, /components: clippy,rustfmt/);
    assert.match(block, /libwebkit2gtk-4\.1-dev/);
  }
});

test("Speech Linux coverage provisions its GLib build dependencies", () => {
  const prSpeech = read(prPath).match(/^  speech-linux:[\s\S]*?(?=^  mom-linux:)/m)?.[0];
  const fullSpeech = read(fullPath).match(/^  speech:[\s\S]*?(?=^  mom:)/m)?.[0];
  for (const block of [prSpeech, fullSpeech]) {
    assert.ok(block, "Speech job block is missing");
    assert.match(block, /libglib2\.0-dev/);
    assert.match(block, /libwebkit2gtk-4\.1-dev/);
  }
  assert.match(fullSpeech, /if: runner\.os == 'Linux'/);
});

test("W1 ancestry-bound jobs retain full Git history", () => {
  const pr = read(prPath);
  const full = read(fullPath);
  const blocks = [
    pr.match(/^  native-linux:[\s\S]*?(?=^  gateway-linux:)/m)?.[0],
    pr.match(/^  information-linux:[\s\S]*?(?=^  speech-linux:)/m)?.[0],
    pr.match(/^  speech-linux:[\s\S]*?(?=^  mom-linux:)/m)?.[0],
    full.match(/^  root:[\s\S]*?(?=^  attachment:)/m)?.[0],
    full.match(/^  information-platform-linux:[\s\S]*?(?=^  speech:)/m)?.[0],
    full.match(/^  speech:[\s\S]*?(?=^  mom:)/m)?.[0],
  ];
  for (const block of blocks) {
    assert.ok(block, "W1 job block is missing");
    assert.match(block, /fetch-depth: 0/);
  }
});

test("full workflow covers main, nightly, dispatch, products, policy, and fuzz", () => {
  const source = read(fullPath);
  assert.match(source, /^\s+push:\n\s+branches: \[main\]/m);
  assert.match(source, /^\s+schedule:/m);
  assert.match(source, /^\s+workflow_dispatch:/m);
  assert.match(source, /^\s{2}mom:/m);
  assert.match(source, /^\s{2}loom:/m);
  assert.match(source, /^\s{2}frontend:/m);
  assert.match(source, /^\s{2}fuzz-build:/m);
  assert.match(source, /cargo clippy --locked --workspace --all-targets -- -D warnings/);
  assert.doesNotMatch(source, /self-hosted|real[-_ ]hardware/i);
  assert.match(source, /^\s{4}if: always\(\)$/m);
});

test("all third-party actions are pinned to immutable commits", () => {
  for (const file of [prPath, fullPath]) {
    for (const action of externalActionUses(read(file))) {
      assert.match(action, /^[^/@]+\/[^/@]+@[0-9a-f]{40}$/, `${file}: ${action}`);
    }
  }
});
