#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";

const root = process.argv[2];
if (!root || process.argv.length !== 3) {
  console.error("usage: find-embedded-model.mjs DIRECTORY");
  process.exit(2);
}

const modelExtensions = new Set([
  ".gguf",
  ".safetensors",
  ".onnx",
  ".pt",
  ".pth",
  ".ckpt",
  ".mlmodel",
  ".mlpackage",
]);
const ggufMagic = Buffer.from("GGUF");

function hasGgufMagic(file) {
  const descriptor = fs.openSync(file, "r");
  try {
    const header = Buffer.alloc(ggufMagic.length);
    return fs.readSync(descriptor, header, 0, header.length, 0) === header.length &&
      header.equals(ggufMagic);
  } finally {
    fs.closeSync(descriptor);
  }
}

function findModel(directory) {
  const entries = fs.readdirSync(directory, { withFileTypes: true })
    .sort((left, right) => left.name.localeCompare(right.name, "en"));
  for (const entry of entries) {
    const candidate = path.join(directory, entry.name);
    const extension = path.extname(entry.name).toLowerCase();

    if (entry.isDirectory()) {
      if (extension === ".mlpackage") return candidate;
      const nested = findModel(candidate);
      if (nested) return nested;
      continue;
    }

    // Application bundles legitimately contain framework symlinks. Do not
    // follow them outside the bundle or inspect the same payload twice.
    if (!entry.isFile()) continue;
    if (modelExtensions.has(extension) || hasGgufMagic(candidate)) return candidate;
  }
  return null;
}

const stat = fs.statSync(root);
if (!stat.isDirectory()) {
  console.error(`model scan root is not a directory: ${root}`);
  process.exit(2);
}

const model = findModel(root);
if (model) process.stdout.write(`${model}\n`);
