#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const PRODUCTS = Object.freeze({
  mom: { executable: "mom-llama-app", state_env: "LLAMA_NATIVE_KIT_DATA_DIR" },
  loom: { executable: "loom-app", state_env: "DELYSIS_LOOM_ACCEPTANCE_DIR" },
  fte: { executable: "free-token-energy", state_env: "DELYSIS_FTE_ACCEPTANCE_DIR" },
});
const MODEL_EXTENSIONS = new Set([
  ".gguf",
  ".safetensors",
  ".onnx",
  ".pt",
  ".pth",
  ".ckpt",
  ".mlmodel",
  ".mlpackage",
]);
const BACKUP_FILES = new Set([
  "manifest.json",
  "backup-receipt.json",
  "backup-receipt.json.sha256",
  "payload",
]);

function fail(message) {
  throw new Error(message);
}

function absolute(input) {
  return path.resolve(input);
}

function isInside(parent, candidate) {
  const relative = path.relative(parent, candidate);
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
}

function requireProduct(product) {
  if (!Object.hasOwn(PRODUCTS, product)) {
    fail(`unknown product '${product}'; expected mom, loom, or fte`);
  }
}

function sha256Bytes(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function sha256File(file) {
  const hash = createHash("sha256");
  const descriptor = fs.openSync(file, "r");
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    for (;;) {
      const read = fs.readSync(descriptor, buffer, 0, buffer.length, null);
      if (read === 0) break;
      hash.update(buffer.subarray(0, read));
    }
  } finally {
    fs.closeSync(descriptor);
  }
  return hash.digest("hex");
}

function writeJson(file, value) {
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
}

function writeDigestSidecar(file) {
  const sidecar = `${file}.sha256`;
  fs.writeFileSync(sidecar, `${sha256File(file)}  ${path.basename(file)}\n`, {
    mode: 0o600,
  });
}

function verifyDigestSidecar(file) {
  const sidecar = `${file}.sha256`;
  if (!fs.existsSync(sidecar)) fail(`receipt digest is missing: ${sidecar}`);
  const expected = fs.readFileSync(sidecar, "utf8").trim();
  const match = /^([0-9a-f]{64})  ([^/]+)$/.exec(expected);
  if (!match || match[2] !== path.basename(file)) {
    fail(`receipt digest is malformed: ${sidecar}`);
  }
  const observed = sha256File(file);
  if (observed !== match[1]) fail(`receipt digest mismatch: ${file}`);
  return observed;
}

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (error) {
    fail(`cannot read JSON ${file}: ${error.message}`);
  }
}

function isModelDirectory(name) {
  return path.extname(name).toLowerCase() === ".mlpackage";
}

function modelReason(file, stat) {
  const extension = path.extname(file).toLowerCase();
  if (MODEL_EXTENSIONS.has(extension)) return `model-extension:${extension}`;
  if (!stat.isFile() || stat.size < 4) return null;
  const descriptor = fs.openSync(file, "r");
  const magic = Buffer.alloc(4);
  try {
    fs.readSync(descriptor, magic, 0, magic.length, 0);
  } finally {
    fs.closeSync(descriptor);
  }
  return magic.equals(Buffer.from("GGUF")) ? "model-magic:GGUF" : null;
}

function entryMode(stat) {
  return stat.mode & 0o777;
}

function inventory(root) {
  const entries = [];
  const excluded_models = [];

  function visit(relative) {
    const directory = path.join(root, relative);
    const names = fs.readdirSync(directory).sort((a, b) => a.localeCompare(b, "en"));
    for (const name of names) {
      const childRelative = relative ? path.join(relative, name) : name;
      const manifestPath = childRelative.split(path.sep).join("/");
      const child = path.join(root, childRelative);
      const stat = fs.lstatSync(child);
      if (stat.isDirectory()) {
        if (isModelDirectory(name)) {
          excluded_models.push({ path: manifestPath, type: "directory", reason: "model-extension:.mlpackage" });
          continue;
        }
        entries.push({ path: manifestPath, type: "directory", mode: entryMode(stat) });
        visit(childRelative);
      } else if (stat.isFile()) {
        const reason = modelReason(child, stat);
        if (reason) {
          excluded_models.push({ path: manifestPath, type: "file", bytes: stat.size, reason });
          continue;
        }
        entries.push({
          path: manifestPath,
          type: "file",
          mode: entryMode(stat),
          bytes: stat.size,
          sha256: sha256File(child),
        });
      } else {
        fail(`unsupported special file in product state: ${child}`);
      }
    }
  }

  visit("");
  return { entries, excluded_models };
}

function treeDigest(entries) {
  return sha256Bytes(Buffer.from(JSON.stringify(entries)));
}

function assertSameEntries(expected, observed, label) {
  if (JSON.stringify(expected) !== JSON.stringify(observed)) {
    fail(`${label} does not match its manifest`);
  }
}

function entryPath(root, manifestPath) {
  if (typeof manifestPath !== "string" || manifestPath.length === 0) {
    fail("manifest contains an empty or non-string path");
  }
  const segments = manifestPath.split("/");
  if (segments.some((segment) => segment === "" || segment === "." || segment === "..")) {
    fail(`manifest path escapes its state root: ${manifestPath}`);
  }
  const candidate = path.resolve(root, ...segments);
  if (!isInside(root, candidate) || candidate === path.resolve(root)) {
    fail(`manifest path escapes its state root: ${manifestPath}`);
  }
  return candidate;
}

function copyEntries(source, destination, entries) {
  fs.mkdirSync(destination, { recursive: false, mode: 0o700 });
  for (const entry of entries) {
    const sourcePath = entryPath(source, entry.path);
    const destinationPath = entryPath(destination, entry.path);
    if (entry.type === "directory") {
      fs.mkdirSync(destinationPath, { mode: entry.mode });
      fs.chmodSync(destinationPath, entry.mode);
    } else if (entry.type === "file") {
      fs.copyFileSync(sourcePath, destinationPath, fs.constants.COPYFILE_EXCL);
      fs.chmodSync(destinationPath, entry.mode);
      if (sha256File(destinationPath) !== entry.sha256) {
        fail(`copied file digest mismatch: ${entry.path}`);
      }
    }
  }
}

function comparablePath(input) {
  const resolved = absolute(input);
  try {
    return fs.realpathSync.native(resolved);
  } catch {
    const parent = fs.realpathSync.native(path.dirname(resolved));
    return path.join(parent, path.basename(resolved));
  }
}

function runningProductProcesses(product, stateRoot) {
  requireProduct(product);
  if (process.platform === "win32") {
    fail("product-state backup process inspection is not implemented on Windows");
  }
  const output = execFileSync("ps", ["-axo", "pid=,state=,comm="], { encoding: "utf8" });
  const { executable, state_env: stateEnv } = PRODUCTS[product];
  const comparableStateRoot = comparablePath(stateRoot);
  const processes = [];
  for (const line of output.split("\n")) {
    const match = /^\s*(\d+)\s+(\S+)\s+(.+?)\s*$/.exec(line);
    if (!match || Number(match[1]) === process.pid) continue;
    if (match[2].startsWith("Z") || match[2].includes("E")) continue;
    if (path.basename(match[3]) !== executable) continue;
    const pid = Number(match[1]);
    const commandAndEnvironment = execFileSync(
      "ps",
      ["eww", "-p", String(pid), "-o", "command="],
      { encoding: "utf8" },
    );
    const marker = `${stateEnv}=`;
    const markerAt = commandAndEnvironment.indexOf(marker);
    if (markerAt >= 0) {
      const value = commandAndEnvironment.slice(markerAt + marker.length).split(/\s+[A-Za-z_][A-Za-z0-9_]*=/, 1)[0].trim();
      if (value && comparablePath(value) !== comparableStateRoot) continue;
    }
    processes.push({ pid, command: match[3] });
  }
  return processes;
}

function requireAppStopped(product, stateRoot) {
  const running = runningProductProcesses(product, stateRoot);
  if (running.length !== 0) {
    fail(`${product} must be stopped; found ${running.map(({ pid }) => pid).join(", ")}`);
  }
}

function requireDirectory(root, label) {
  const stat = fs.lstatSync(root, { throwIfNoEntry: false });
  if (!stat || !stat.isDirectory() || stat.isSymbolicLink()) {
    fail(`${label} must be an existing real directory: ${root}`);
  }
}

function requireAbsent(target, label) {
  if (fs.lstatSync(target, { throwIfNoEntry: false })) fail(`${label} already exists: ${target}`);
}

function requireEmptyOrAbsent(target) {
  const stat = fs.lstatSync(target, { throwIfNoEntry: false });
  if (!stat) return { existed: false, mode: 0o700 };
  if (!stat.isDirectory() || stat.isSymbolicLink()) {
    fail(`restore destination must be absent or an empty real directory: ${target}`);
  }
  if (fs.readdirSync(target).length !== 0) {
    fail(`restore destination is not empty: ${target}`);
  }
  return { existed: true, mode: entryMode(stat) };
}

function stagingPath(target) {
  return path.join(
    path.dirname(target),
    `.${path.basename(target)}.delysis-staging-${process.pid}-${randomBytes(6).toString("hex")}`,
  );
}

function verifyBackup(backupInput) {
  const backup = absolute(backupInput);
  requireDirectory(backup, "backup");
  const extras = fs.readdirSync(backup).filter((name) => !BACKUP_FILES.has(name));
  if (extras.length !== 0) fail(`backup contains unexpected top-level entries: ${extras.join(", ")}`);

  const manifestPath = path.join(backup, "manifest.json");
  const receiptPath = path.join(backup, "backup-receipt.json");
  const payload = path.join(backup, "payload");
  requireDirectory(payload, "backup payload");
  const manifest = readJson(manifestPath);
  const receipt = readJson(receiptPath);
  const receiptSha256 = verifyDigestSidecar(receiptPath);
  if (manifest.schema !== "delysis.product-state-manifest.v1") fail("unsupported backup manifest schema");
  if (receipt.schema !== "delysis.product-state-backup.v1") fail("unsupported backup receipt schema");
  requireProduct(manifest.product);
  if (receipt.product !== manifest.product) fail("backup receipt product mismatch");
  const manifestSha256 = sha256File(manifestPath);
  if (receipt.manifest_sha256 !== manifestSha256) fail("backup manifest digest mismatch");
  if (manifest.tree_sha256 !== treeDigest(manifest.entries)) fail("backup tree digest mismatch");
  if (receipt.tree_sha256 !== manifest.tree_sha256) fail("backup receipt tree digest mismatch");
  if (receipt.model_payload_files !== 0) fail("backup receipt does not exclude model payloads");
  const observed = inventory(payload);
  if (observed.excluded_models.length !== 0) fail("backup payload contains model weights");
  assertSameEntries(manifest.entries, observed.entries, "backup payload");
  return { backup, manifest, manifestPath, manifestSha256, receiptPath, receiptSha256, payload };
}

function backup(product, sourceInput, backupInput) {
  requireProduct(product);
  const source = absolute(sourceInput);
  const destination = absolute(backupInput);
  requireDirectory(source, "product state root");
  requireAbsent(destination, "backup destination");
  if (isInside(source, destination) || isInside(destination, source)) {
    fail("product state root and backup destination must not contain one another");
  }
  requireAppStopped(product, source);
  const before = inventory(source);
  const stage = stagingPath(destination);
  requireAbsent(stage, "backup staging path");
  try {
    fs.mkdirSync(stage, { mode: 0o700 });
    copyEntries(source, path.join(stage, "payload"), before.entries);
    const after = inventory(source);
    assertSameEntries(before.entries, after.entries, "product state changed during backup");
    if (JSON.stringify(before.excluded_models) !== JSON.stringify(after.excluded_models)) {
      fail("excluded model inventory changed during backup");
    }
    requireAppStopped(product, source);
    const manifest = {
      schema: "delysis.product-state-manifest.v1",
      created_at: new Date().toISOString(),
      product,
      source_root: source,
      entries: before.entries,
      excluded_models: before.excluded_models,
      tree_sha256: treeDigest(before.entries),
    };
    const manifestPath = path.join(stage, "manifest.json");
    writeJson(manifestPath, manifest);
    const receiptPath = path.join(stage, "backup-receipt.json");
    writeJson(receiptPath, {
      schema: "delysis.product-state-backup.v1",
      created_at: new Date().toISOString(),
      product,
      source_root: source,
      manifest_sha256: sha256File(manifestPath),
      tree_sha256: manifest.tree_sha256,
      entry_count: manifest.entries.length,
      excluded_model_count: manifest.excluded_models.length,
      model_payload_files: 0,
      application_stopped: true,
    });
    writeDigestSidecar(receiptPath);
    fs.renameSync(stage, destination);
  } catch (error) {
    fs.rmSync(stage, { recursive: true, force: true });
    throw error;
  }
  const verified = verifyBackup(destination);
  process.stdout.write(`backup verified: ${verified.backup}\n`);
}

function restore(backupInput, destinationInput, receiptInput) {
  const verified = verifyBackup(backupInput);
  const destination = absolute(destinationInput);
  const receiptPath = absolute(receiptInput);
  if (isInside(verified.backup, destination) || isInside(destination, verified.backup)) {
    fail("backup and restore destination must not contain one another");
  }
  if (isInside(destination, receiptPath) || isInside(verified.backup, receiptPath)) {
    fail("restore receipt must be outside the backup and restored state roots");
  }
  requireDirectory(path.dirname(receiptPath), "restore receipt directory");
  requireAbsent(receiptPath, "restore receipt");
  requireAbsent(`${receiptPath}.sha256`, "restore receipt digest");
  const destinationState = requireEmptyOrAbsent(destination);
  requireAppStopped(verified.manifest.product, destination);
  const stage = stagingPath(destination);
  requireAbsent(stage, "restore staging path");
  try {
    copyEntries(verified.payload, stage, verified.manifest.entries);
    const staged = inventory(stage);
    if (staged.excluded_models.length !== 0) fail("restore staging contains model weights");
    assertSameEntries(verified.manifest.entries, staged.entries, "restore staging");
    requireAppStopped(verified.manifest.product, destination);
    if (destinationState.existed) fs.rmdirSync(destination);
    try {
      fs.renameSync(stage, destination);
    } catch (error) {
      if (destinationState.existed && !fs.existsSync(destination)) {
        fs.mkdirSync(destination, { mode: destinationState.mode });
      }
      throw error;
    }
  } catch (error) {
    fs.rmSync(stage, { recursive: true, force: true });
    throw error;
  }

  const restored = inventory(destination);
  assertSameEntries(verified.manifest.entries, restored.entries, "restored state");
  writeJson(receiptPath, {
    schema: "delysis.product-state-restore.v1",
    created_at: new Date().toISOString(),
    product: verified.manifest.product,
    backup: verified.backup,
    backup_receipt_sha256: verified.receiptSha256,
    manifest_sha256: verified.manifestSha256,
    tree_sha256: verified.manifest.tree_sha256,
    destination_root: destination,
    entry_count: verified.manifest.entries.length,
    model_payload_files: 0,
    destination_was_absent_or_empty: true,
    application_stopped: true,
  });
  writeDigestSidecar(receiptPath);
  verifyRestore(receiptPath);
  process.stdout.write(`restore verified: ${destination}\nreceipt: ${receiptPath}\n`);
}

function verifyRestore(receiptInput) {
  const receiptPath = absolute(receiptInput);
  const receipt = readJson(receiptPath);
  verifyDigestSidecar(receiptPath);
  if (receipt.schema !== "delysis.product-state-restore.v1") fail("unsupported restore receipt schema");
  requireProduct(receipt.product);
  requireAppStopped(receipt.product, receipt.destination_root);
  const verified = verifyBackup(receipt.backup);
  if (verified.manifest.product !== receipt.product) fail("restore receipt product mismatch");
  if (verified.receiptSha256 !== receipt.backup_receipt_sha256) fail("restore backup receipt digest mismatch");
  if (verified.manifestSha256 !== receipt.manifest_sha256) fail("restore manifest digest mismatch");
  if (verified.manifest.tree_sha256 !== receipt.tree_sha256) fail("restore tree digest mismatch");
  if (receipt.model_payload_files !== 0) fail("restore receipt does not exclude model payloads");
  requireDirectory(receipt.destination_root, "restored state root");
  const observed = inventory(receipt.destination_root);
  if (observed.excluded_models.length !== 0) fail("restored state contains model weights");
  assertSameEntries(verified.manifest.entries, observed.entries, "restored state");
  process.stdout.write(`restore receipt verified: ${receiptPath}\n`);
}

function usage() {
  return [
    "usage:",
    "  node scripts/product-state-backup.mjs backup {mom|loom|fte} STATE_ROOT BACKUP_DIR",
    "  node scripts/product-state-backup.mjs verify BACKUP_DIR",
    "  node scripts/product-state-backup.mjs restore BACKUP_DIR STATE_ROOT RESTORE_RECEIPT.json",
    "  node scripts/product-state-backup.mjs verify-restore RESTORE_RECEIPT.json",
  ].join("\n");
}

function main(argv) {
  const [command, ...args] = argv;
  if (command === "backup" && args.length === 3) return backup(...args);
  if (command === "verify" && args.length === 1) {
    const verified = verifyBackup(args[0]);
    process.stdout.write(`backup verified: ${verified.backup}\n`);
    return;
  }
  if (command === "restore" && args.length === 3) return restore(...args);
  if (command === "verify-restore" && args.length === 1) return verifyRestore(args[0]);
  fail(usage());
}

try {
  main(process.argv.slice(2));
} catch (error) {
  process.stderr.write(`product-state-backup: ${error.message}\n`);
  process.exitCode = 1;
}
