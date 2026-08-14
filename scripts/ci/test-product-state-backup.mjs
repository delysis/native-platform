#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const root = path.resolve(import.meta.dirname, "../..");
const tool = path.join(root, "scripts/product-state-backup.mjs");

function fixture(t, product) {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), `delysis-${product}-backup-`));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const source = path.join(temporary, "source");
  const backup = path.join(temporary, "backup");
  const restored = path.join(temporary, "restored");
  const receipt = path.join(temporary, "receipts", "restore.json");
  fs.mkdirSync(path.join(source, "nested"), { recursive: true });
  fs.mkdirSync(path.dirname(receipt), { mode: 0o700 });
  fs.writeFileSync(path.join(source, "state.sqlite3"), `${product} durable state\n`, { mode: 0o600 });
  fs.writeFileSync(path.join(source, "nested", "draft.txt"), "unfinished thought\n");
  fs.writeFileSync(path.join(source, "cached-model.gguf"), "GGUFnamed model bytes");
  fs.writeFileSync(path.join(source, "content-addressed-blob"), "GGUFextensionless model bytes");
  fs.mkdirSync(path.join(source, "CoreML.mlpackage"));
  fs.writeFileSync(path.join(source, "CoreML.mlpackage", "weights.bin"), "model");
  return { temporary, source, backup, restored, receipt };
}

function runWithEnvironment(environment, ...args) {
  return spawnSync(process.execPath, [tool, ...args], {
    encoding: "utf8",
    env: { ...process.env, ...environment },
  });
}

function run(...args) {
  return runWithEnvironment({}, ...args);
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

for (const product of ["mom", "loom", "fte"]) {
  test(`${product} state backs up, verifies, restores, and excludes model bytes`, (t) => {
    const f = fixture(t, product);
    let result = run("backup", product, f.source, f.backup);
    assert.equal(result.status, 0, result.stderr);
    result = run("verify", f.backup);
    assert.equal(result.status, 0, result.stderr);
    if (product === "loom") fs.mkdirSync(f.restored);
    result = run("restore", f.backup, f.restored, f.receipt);
    assert.equal(result.status, 0, result.stderr);
    result = run("verify-restore", f.receipt);
    assert.equal(result.status, 0, result.stderr);

    assert.equal(fs.readFileSync(path.join(f.restored, "state.sqlite3"), "utf8"), `${product} durable state\n`);
    assert.equal(fs.existsSync(path.join(f.restored, "cached-model.gguf")), false);
    assert.equal(fs.existsSync(path.join(f.restored, "content-addressed-blob")), false);
    assert.equal(fs.existsSync(path.join(f.restored, "CoreML.mlpackage")), false);

    const manifest = JSON.parse(fs.readFileSync(path.join(f.backup, "manifest.json"), "utf8"));
    assert.equal(manifest.product, product);
    assert.deepEqual(
      manifest.excluded_models.map(({ path: excluded }) => excluded),
      ["cached-model.gguf", "content-addressed-blob", "CoreML.mlpackage"],
    );
    assert.equal(fs.existsSync(path.join(f.backup, "backup-receipt.json.sha256")), true);
    assert.equal(fs.existsSync(`${f.receipt}.sha256`), true);
  });
}

test("restore refuses a nonempty destination without changing it", (t) => {
  const f = fixture(t, "mom");
  assert.equal(run("backup", "mom", f.source, f.backup).status, 0);
  fs.mkdirSync(f.restored);
  fs.writeFileSync(path.join(f.restored, "keep.txt"), "do not replace\n");
  const result = run("restore", f.backup, f.restored, f.receipt);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /restore destination is not empty/);
  assert.equal(fs.readFileSync(path.join(f.restored, "keep.txt"), "utf8"), "do not replace\n");
  assert.equal(fs.existsSync(f.receipt), false);
});

test("backup refuses symbolic links instead of restoring an escaping target", (t) => {
  const f = fixture(t, "mom");
  fs.symlinkSync("../outside", path.join(f.source, "nested", "state-link"));
  const result = run("backup", "mom", f.source, f.backup);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /unsupported special file/);
  assert.equal(fs.existsSync(f.backup), false);
});

test("backup and restore verification reject byte and receipt tampering", (t) => {
  const f = fixture(t, "loom");
  assert.equal(run("backup", "loom", f.source, f.backup).status, 0);
  fs.appendFileSync(path.join(f.backup, "payload", "state.sqlite3"), "tamper");
  let result = run("verify", f.backup);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /backup payload does not match its manifest/);

  const clean = fixture(t, "fte");
  assert.equal(run("backup", "fte", clean.source, clean.backup).status, 0);
  assert.equal(run("restore", clean.backup, clean.restored, clean.receipt).status, 0);
  fs.appendFileSync(clean.receipt, " ");
  result = run("verify-restore", clean.receipt);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /receipt digest mismatch/);
});

test("restore rejects a digest-consistent manifest path that escapes the destination", (t) => {
  const f = fixture(t, "mom");
  assert.equal(run("backup", "mom", f.source, f.backup).status, 0);
  const manifestPath = path.join(f.backup, "manifest.json");
  const receiptPath = path.join(f.backup, "backup-receipt.json");
  const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  manifest.entries[0].path = "../escaped";
  manifest.tree_sha256 = sha256(JSON.stringify(manifest.entries));
  fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  const receipt = JSON.parse(fs.readFileSync(receiptPath, "utf8"));
  receipt.manifest_sha256 = sha256(fs.readFileSync(manifestPath));
  receipt.tree_sha256 = manifest.tree_sha256;
  fs.writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`);
  fs.writeFileSync(
    `${receiptPath}.sha256`,
    `${sha256(fs.readFileSync(receiptPath))}  backup-receipt.json\n`,
  );

  const result = run("restore", f.backup, f.restored, f.receipt);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /backup payload does not match|manifest path escapes its state root/);
  assert.equal(fs.existsSync(path.join(f.temporary, "escaped")), false);
});

test("backup refuses to run while the product executable is alive", (t) => {
  if (process.platform === "win32") return;
  const f = fixture(t, "mom");
  const bin = path.join(f.temporary, "bin");
  const ps = path.join(bin, "ps");
  fs.mkdirSync(bin);
  fs.writeFileSync(
    ps,
    `#!/bin/sh
case "$*" in
  "-axo pid=,state=,comm=") printf '4242 S /Applications/Mom Llama.app/Contents/MacOS/mom-llama-app\\n' ;;
  *) printf '/Applications/Mom Llama.app/Contents/MacOS/mom-llama-app\\n' ;;
esac
`,
    { mode: 0o755 },
  );
  const result = runWithEnvironment(
    { PATH: `${bin}${path.delimiter}${process.env.PATH}` },
    "backup",
    "mom",
    f.source,
    f.backup,
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /mom must be stopped/);
  assert.equal(fs.existsSync(f.backup), false);
});
