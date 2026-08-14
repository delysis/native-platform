#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const root = path.resolve(import.meta.dirname, "../..");
const prPath = path.join(root, ".github/workflows/ci-pr.yml");
const fullPath = path.join(root, ".github/workflows/ci-full.yml");
const releasePath = path.join(root, ".github/workflows/release-macos.yml");
const releaseScriptPath = path.join(root, "scripts/release-macos.sh");
const smokeScriptPath = path.join(root, "scripts/smoke-macos-app.sh");
const embeddedModelScriptPath = path.join(root, "scripts/find-embedded-model.mjs");
const momPackagePath = path.join(
  root,
  "products/mom/apps/mom-llama/package.json",
);
const momWindowsIconPath = path.join(
  root,
  "products/mom/apps/mom-llama/src-tauri/icons/icon.ico",
);

function read(file) {
  return fs.readFileSync(file, "utf8");
}

function sha256(file) {
  return createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function writeExecutable(file, source) {
  fs.writeFileSync(file, source, { mode: 0o755 });
}

function macSmokeFixture(t, weightPath = null, weightContents = "fixture model bytes\n") {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "delysis-smoke-identity-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));

  const candidate = path.join(directory, "candidate");
  const fakeTools = path.join(directory, "bin");
  const sourceBundle = path.join(directory, "source", "Mom Llama.app");
  const contents = path.join(sourceBundle, "Contents");
  const executable = path.join(contents, "MacOS", "mom-llama-app");
  const archive = path.join(candidate, "Mom Llama.app.zip");
  const receipt = path.join(candidate, "release-receipt.json");
  fs.mkdirSync(path.dirname(executable), { recursive: true });
  fs.mkdirSync(candidate, { recursive: true });
  fs.mkdirSync(fakeTools, { recursive: true });
  writeExecutable(executable, "#!/bin/sh\nexit 0\n");
  fs.writeFileSync(path.join(contents, "Info.plist"), "fixture plist\n");
  fs.writeFileSync(archive, "fixture archive bytes\n");

  if (weightPath) {
    const target = path.join(contents, "Resources", weightPath);
    if (weightPath.endsWith(".mlpackage")) {
      fs.mkdirSync(target, { recursive: true });
    } else {
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, weightContents);
    }
  }

  writeExecutable(path.join(fakeTools, "uname"), "#!/bin/sh\nprintf 'Darwin\\n'\n");
  writeExecutable(
    path.join(fakeTools, "ditto"),
    "#!/bin/sh\ncp -R \"$FAKE_APP_SOURCE\" \"$4/\"\n",
  );
  writeExecutable(
    path.join(fakeTools, "shasum"),
    `#!/bin/sh
node -e 'const fs=require("fs"),crypto=require("crypto"),p=process.argv[1]; console.log(crypto.createHash("sha256").update(fs.readFileSync(p)).digest("hex")+"  "+p)' "$3"
`,
  );

  const validReceipt = {
    schema: "delysis.macos-release-receipt.v1",
    component: "mom",
    macos: {
      bundle_id: "com.delysis.llama-native-kit.mom-llama",
      archive_sha256: sha256(archive),
      executable_sha256: sha256(executable),
    },
  };
  const run = () =>
    spawnSync("sh", [smokeScriptPath, "mom", archive], {
      encoding: "utf8",
      env: {
        ...process.env,
        FAKE_APP_SOURCE: sourceBundle,
        PATH: `${fakeTools}${path.delimiter}${process.env.PATH}`,
        TMPDIR: directory,
      },
    });

  return { archive, receipt, run, validReceipt };
}

test("extracted macOS ZIP rejects an extensionless GGUF payload", (t) => {
  const fixture = macSmokeFixture(
    t,
    "weights/0123456789abcdef",
    Buffer.from("GGUFextensionless model bytes"),
  );
  fs.writeFileSync(fixture.receipt, `${JSON.stringify(fixture.validReceipt)}\n`);
  const result = fixture.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /model weights must remain runtime-discovered/);
  assert.match(result.stderr, /0123456789abcdef/);
});

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
  assert.match(smoke, /input_release_receipt_sha256:/);
});

test("supplied macOS ZIP fails closed without its exact adjacent release receipt", (t) => {
  const fixture = macSmokeFixture(t);

  let result = fixture.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /adjacent release receipt is missing/);

  for (const [field, value, expected] of [
    ["component", "loom", /release receipt component mismatch/],
    ["bundle_id", "example.invalid", /release receipt bundle ID mismatch/],
    ["archive_sha256", "0".repeat(64), /release receipt archive SHA-256 mismatch/],
    ["executable_sha256", "0".repeat(64), /release receipt executable SHA-256 mismatch/],
  ]) {
    const receipt = structuredClone(fixture.validReceipt);
    if (field === "component") receipt.component = value;
    else receipt.macos[field] = value;
    fs.writeFileSync(fixture.receipt, `${JSON.stringify(receipt)}\n`);
    result = fixture.run();
    assert.notEqual(result.status, 0, `${field} mismatch must fail`);
    assert.match(result.stderr, expected);
  }
});

for (const extension of [
  "gguf",
  "safetensors",
  "onnx",
  "pt",
  "pth",
  "ckpt",
  "mlmodel",
  "mlpackage",
]) {
  test(`extracted macOS ZIP rejects .${extension} model weights`, (t) => {
    const fixture = macSmokeFixture(t, `weights/model.${extension}`);
    fs.writeFileSync(fixture.receipt, `${JSON.stringify(fixture.validReceipt)}\n`);
    const result = fixture.run();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /model weights must remain runtime-discovered/);
  });
}

test("the packaged executable is not mistaken for model weights", (t) => {
  const fixture = macSmokeFixture(t);
  fs.writeFileSync(fixture.receipt, `${JSON.stringify(fixture.validReceipt)}\n`);
  const result = fixture.run();
  assert.notEqual(result.status, 0, "the fake unsigned app cannot complete the full smoke");
  assert.doesNotMatch(result.stderr, /model weights must remain runtime-discovered/);
});

test("the macOS builder rejects the same common model-weight formats as the archive smoke", () => {
  const releaseSource = read(releaseScriptPath);
  const smokeSource = read(smokeScriptPath);
  const scannerSource = read(embeddedModelScriptPath);
  assert.match(releaseSource, /scripts\/find-embedded-model\.mjs/);
  assert.match(smokeSource, /scripts\/find-embedded-model\.mjs/);
  for (const extension of [
    "gguf",
    "safetensors",
    "onnx",
    "pt",
    "pth",
    "ckpt",
    "mlmodel",
    "mlpackage",
  ]) {
    assert.ok(scannerSource.includes(`".${extension}"`));
  }
  assert.match(scannerSource, /Buffer\.from\("GGUF"\)/);
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
  assert.match(source, /node --test scripts\/ci\/test-ci-plan\.mjs scripts\/ci\/test-ci-required\.mjs scripts\/ci\/test-product-state-backup\.mjs scripts\/ci\/test-workflows\.mjs/);
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

test("PR frontend runs only selected product frontend commands", () => {
  const prFrontend = read(prPath).match(/^  frontend:[\s\S]*?(?=^  platform-macos:)/m)?.[0];
  assert.ok(prFrontend, "PR frontend job block is missing");
  assert.match(prFrontend, /pnpm install --frozen-lockfile/);
  assert.doesNotMatch(prFrontend, /pnpm -r/);
  assert.doesNotMatch(prFrontend, /apt-get|libwebkit2gtk|dtolnay\/rust-toolchain/);
  assert.match(prFrontend, /name: FTE frontend/);
  assert.match(prFrontend, /pnpm --filter free-token-energy run check:frontend/);
  assert.match(prFrontend, /pnpm --filter free-token-energy run test:frontend/);
  assert.doesNotMatch(
    prFrontend,
    /free-token-energy run (?:build|check|check:rust|test|test:rust)\s*$/m,
  );
  assert.match(prFrontend, /name: Mom frontend/);
  assert.match(prFrontend, /pnpm --filter @delysis\/mom-llama run check:frontend/);
  assert.match(prFrontend, /name: Loom frontend/);
  assert.match(prFrontend, /pnpm --filter @delysis\/loom run test/);
  assert.match(prFrontend, /pnpm --filter @delysis\/loom run check/);
  assert.match(prFrontend, /pnpm --filter @delysis\/loom run build/);
  for (const flag of ["frontend_fte", "frontend_mom", "frontend_loom"]) {
    assert.match(prFrontend, new RegExp(`needs\\.plan\\.outputs\\.${flag}`));
  }
});

test("Mom exposes the frontend syntax check used by PR CI", () => {
  const scripts = JSON.parse(read(momPackagePath)).scripts;
  assert.equal(
    scripts["check:frontend"],
    "node --check ui/cache-inspector.js && node --check ui/coop-hx.js",
  );
});

test("full frontend coverage remains unchanged", () => {
  const fullFrontend = read(fullPath).match(/^  frontend:[\s\S]*?(?=^  policy-and-graphs:)/m)?.[0];
  assert.ok(fullFrontend, "full frontend job block is missing");
  assert.match(fullFrontend, /dtolnay\/rust-toolchain@[0-9a-f]{40}/);
  assert.match(fullFrontend, /components: clippy,rustfmt/);
  assert.match(fullFrontend, /libwebkit2gtk-4\.1-dev/);
  assert.match(fullFrontend, /pnpm install --frozen-lockfile/);
  assert.match(fullFrontend, /pnpm -r --if-present run test/);
  assert.match(fullFrontend, /pnpm -r --if-present run check/);
  assert.match(fullFrontend, /pnpm -r --if-present run build/);
  assert.doesNotMatch(fullFrontend, /loom:install|--dir products\/loom/);
});

test("the required macOS lane runs Mom parity when Mom changes", () => {
  const macos = read(prPath).match(/^  platform-macos:[\s\S]*?(?=^  dependency-graph:)/m)?.[0];
  const rootGraph = macos?.match(
    /- name: Root platform graph[\s\S]*?(?=\n      - name: Mom macOS parity)/,
  )?.[0];
  assert.ok(macos, "platform-macos job block is missing");
  assert.ok(rootGraph, "Root platform graph step is missing");
  assert.match(macos, /name: Release tooling shell syntax\n\s+run: sh -n scripts\/release-macos\.sh scripts\/smoke-macos-app\.sh/);
  assert.match(macos, /dtolnay\/rust-toolchain@[0-9a-f]{40}\n\s+if: \$\{\{[^}]*needs\.plan\.outputs\.root/);
  assert.match(macos, /Swatinem\/rust-cache@[0-9a-f]{40}\n\s+if: \$\{\{[^}]*needs\.plan\.outputs\.root/);
  assert.doesNotMatch(rootGraph, /needs\.plan\.outputs\.mom/);
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
