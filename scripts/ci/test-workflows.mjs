#!/usr/bin/env node

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

const root = path.resolve(import.meta.dirname, "../..");
const prPath = path.join(root, ".github/workflows/ci-pr.yml");
const fullPath = path.join(root, ".github/workflows/ci-full.yml");
const releasePath = path.join(root, ".github/workflows/release-macos.yml");
const releaseScriptPath = path.join(root, "scripts/release-macos.sh");
const smokeScriptPath = path.join(root, "scripts/smoke-macos-app.sh");
const momWindowsIconPath = path.join(
  root,
  "products/mom/apps/mom-llama/src-tauri/icons/icon.ico",
);

function read(file) {
  return fs.readFileSync(file, "utf8");
}

function externalActionUses(source) {
  return [...source.matchAll(/^\s*-\s+uses:\s+([^\s#]+).*$/gm)]
    .map((match) => match[1])
    .filter((value) => !value.startsWith("./") && !value.startsWith("docker://"));
}

test("only the targeted PR, full, and asynchronous release workflows remain active", () => {
  assert.equal(fs.existsSync(path.join(root, ".github/workflows/ci.yml")), false);
  assert.equal(fs.existsSync(prPath), true);
  assert.equal(fs.existsSync(fullPath), true);
  assert.equal(fs.existsSync(releasePath), true);
});

test("Mom retains the Windows resource icon required by Tauri builds", () => {
  const icon = fs.readFileSync(momWindowsIconPath);
  assert.deepEqual([...icon.subarray(0, 4)], [0, 0, 1, 0]);
});

test("local macOS smoke can verify the exact emitted archive", () => {
  const release = read(releaseScriptPath);
  const smoke = read(smokeScriptPath);
  assert.match(release, /exact-archive smoke:/);
  assert.match(smoke, /ditto -x -k "\$INPUT_ARCHIVE" "\$INSTALL_ROOT"/);
  assert.match(smoke, /input_archive_sha256:/);
});

test("macOS remote candidates are tag or manual artifacts and never PR requirements", () => {
  const source = read(releasePath);
  assert.match(source, /^\s+tags:\s*$/m);
  assert.match(source, /^\s+workflow_dispatch:\s*$/m);
  assert.doesNotMatch(source, /^\s+pull_request:/m);
  assert.match(source, /^\s+package:\s*$/m);
  assert.match(source, /^\s+runs-on: macos-latest$/m);
  assert.match(source, /\.\/scripts\/release-macos\.sh/);
  assert.match(source, /actions\/upload-artifact@[0-9a-f]{40}/);
  assert.match(source, /release tag\/version mismatch/);
  assert.match(source, /expected_tag="\$tag_prefix-v\$version"/);
  assert.match(source, /remote-candidate-/);
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
    "dependency-graph",
    "fuzz-build",
  ]) {
    assert.match(source, new RegExp(`^  ${job}:`, "m"), `missing ${job}`);
  }
  assert.match(source, /mom_present == 'true'/);
  assert.match(source, /loom_present == 'true'/);
  assert.match(source, /group: ci-pr-/);
  assert.match(source, /cancel-in-progress: true/);
  assert.doesNotMatch(source, /^  platform-windows:/m);
  assert.doesNotMatch(source, /platform_windows/);
});

test("root workspace tests can inspect the retained migration evidence", () => {
  const rootLinux = read(prPath).match(/^  root-linux:[\s\S]*?(?=^  native-linux:)/m)?.[0];
  assert.ok(rootLinux, "root-linux job block is missing");
  assert.match(rootLinux, /actions\/checkout@[0-9a-f]{40}\n\s+with:\n\s+fetch-depth: 0/);
});

test("frontend jobs use the single root pnpm workspace", () => {
  const prFrontend = read(prPath).match(/^  frontend:[\s\S]*?(?=^  platform-macos:)/m)?.[0];
  const fullFrontend = read(fullPath).match(/^  frontend:[\s\S]*?(?=^  policy-and-graphs:)/m)?.[0];
  for (const block of [prFrontend, fullFrontend]) {
    assert.ok(block, "frontend job block is missing");
    assert.match(block, /dtolnay\/rust-toolchain@[0-9a-f]{40}/);
    assert.match(block, /components: clippy,rustfmt/);
    assert.match(block, /libwebkit2gtk-4\.1-dev/);
    assert.match(block, /pnpm install --frozen-lockfile/);
    assert.match(block, /pnpm -r --if-present run test/);
    assert.doesNotMatch(block, /loom:install|--dir products\/loom/);
  }
});

test("the required macOS lane runs Mom parity when Mom changes", () => {
  const macos = read(prPath).match(/^  platform-macos:[\s\S]*?(?=^  dependency-graph:)/m)?.[0];
  assert.ok(macos, "platform-macos job block is missing");
  assert.match(macos, /name: Mom macOS parity/);
  assert.match(macos, /mom_present == 'true'/);
  assert.match(macos, /cargo-group\.mjs test product-mom/);
  assert.doesNotMatch(macos, /unstable-w1/);
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

test("Mom and Loom Linux coverage provisions desktop build dependencies", () => {
  const pr = read(prPath);
  const full = read(fullPath);
  const blocks = [
    pr.match(/^  mom-linux:[\s\S]*?(?=^  loom-linux:)/m)?.[0],
    pr.match(/^  loom-linux:[\s\S]*?(?=^  frontend:)/m)?.[0],
    full.match(/^  mom:[\s\S]*?(?=^  loom:)/m)?.[0],
    full.match(/^  loom:[\s\S]*?(?=^  frontend:)/m)?.[0],
  ];
  for (const block of blocks) {
    assert.ok(block, "product job block is missing");
    assert.match(block, /libglib2\.0-dev/);
    assert.match(block, /libgtk-3-dev/);
  }
  assert.match(blocks[2], /if: runner\.os == 'Linux'/);
  assert.match(blocks[3], /if: runner\.os == 'Linux'/);
});

test("fuzz workflows select the owned nested fuzz workspace explicitly", () => {
  for (const source of [read(prPath), read(fullPath)]) {
    assert.match(source, /^\s{2}fuzz-build:/m);
    assert.match(source, /cargo fuzz build --fuzz-dir crates\/services\/attachment\/fuzz inspect/);
    assert.match(source, /cargo fuzz build --fuzz-dir crates\/services\/attachment\/fuzz pipeline/);
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

test("Windows compatibility remains in full CI, not the blocking PR lane", () => {
  const pr = read(prPath);
  const full = read(fullPath);
  assert.doesNotMatch(pr, /windows-latest/);
  assert.match(full, /windows-latest/);
  assert.match(full, /ci-full-/);
});

test("all third-party actions are pinned to immutable commits", () => {
  for (const file of [prPath, fullPath, releasePath]) {
    for (const action of externalActionUses(read(file))) {
      assert.match(action, /^[^/@]+\/[^/@]+@[0-9a-f]{40}$/, `${file}: ${action}`);
    }
  }
});
