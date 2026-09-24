// Source-policy checks for the native observer's UI authority, not macOS tests.
// Its pure projection checks still run in the compiled Swift --self-test lane.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

const source = fs.readFileSync(path.resolve(import.meta.dirname,
  "../macos-smoke-support/start_loom_live_streaming_monitor.swift"), "utf8");

test("live-stream observation cannot activate, focus, type, or invoke UI actions", () => {
  for (const forbidden of [
    /AXUIElementSetAttributeValue\s*\(/,
    /AXUIElementPerformAction\s*\(/,
    /\.activate\s*\(/,
    /CGEvent\s*\(/,
    /CGEventCreate\w*\s*\(/,
    /CGEventPost\w*\s*\(/,
    /\.postToPid\s*\(/,
  ]) {
    assert.equal(forbidden.test(source), false,
      `the observer must not mutate the measured UI via ${forbidden}`);
  }
});

test("zero-admission diagnostics retain first and last actual AX and App caret observations", () => {
  const start = source.indexOf("while ProcessInfo.processInfo.systemUptime < (terminalDeadline");
  const familyGate = source.indexOf('reject("family_pending")', start);
  const capture = source.indexOf("let observation: [String: Any]", start);
  assert.ok(start >= 0 && capture > start && familyGate > capture,
    "capture raw observations even when no family was admitted");
  for (const key of ["ax_caret_utf16", "ax_selection_length_utf16", "ax_focused",
    "frontmost", "witness_present", "internal_caret_byte", "lifecycle_reason"]) {
    assert.ok(source.slice(capture, familyGate).includes(`"${key}"`), key);
  }
  assert.ok(source.includes("if firstAdmissionObservation.isEmpty { firstAdmissionObservation = observation }"));
  assert.ok(source.includes("lastAdmissionObservation = observation"));
  for (const entry of ['"first_admission_observation": firstAdmissionObservation',
    '"last_admission_observation": lastAdmissionObservation']) assert.ok(source.includes(entry), entry);
});

test("passive-observer checks run from the existing required policy entrypoint", () => {
  const entry = fs.readFileSync(path.join(import.meta.dirname, "test-ci-required.mjs"), "utf8");
  assert.ok(entry.includes('import "./passive-observer-cases.mjs";'));
});
