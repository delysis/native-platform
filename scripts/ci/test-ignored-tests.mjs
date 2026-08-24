#!/usr/bin/env node

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import {
  parseCargoIgnoredList,
  readMetadata,
  reconcileCargoList,
  validateRegistry,
} from "./validate-ignored-tests.mjs";

const root = path.resolve(import.meta.dirname, "../..");
const registry = JSON.parse(fs.readFileSync(path.join(root, "ci/ignored-tests.json"), "utf8"));

test("all ignored tests carry source, prerequisite, evidence, and non-promotion metadata", () => {
  const report = validateRegistry({ registry, metadata: readMetadata(root), repoRoot: root });
  assert.equal(report.registry_count, 37);
  assert.ok(report.evidence_classes.includes("real-model-runtime"));
  assert.ok(report.evidence_classes.includes("real-corpus-read-only"));
  assert.ok(report.evidence_classes.includes("real-platform-tts-runtime"));
});

test("Cargo ignored-list parsing preserves exact test IDs", () => {
  const output = [
    "tests::one: test",
    "tests::two: test",
    "",
    "2 tests, 0 benchmarks",
  ].join("\n");
  assert.deepEqual(parseCargoIgnoredList(output), ["tests::one", "tests::two"]);
});

test("reconciliation rejects an unregistered ignored test", () => {
  assert.throws(
    () => reconcileCargoList(registry, ["not_registered"], "linux"),
    /unregistered ignored tests/,
  );
});

test("darwin reconciliation requires the exact registry count and IDs", () => {
  const ids = registry.entries.map((entry) => entry.test_id);
  const report = reconcileCargoList(registry, ids, "darwin");
  assert.equal(report.cargo_count, 37);
  assert.equal(report.exact_registry_match, true);
  assert.throws(
    () => reconcileCargoList(registry, ids.slice(1), "darwin"),
    /Cargo ignored count 36 != registry count 37/,
  );
});
