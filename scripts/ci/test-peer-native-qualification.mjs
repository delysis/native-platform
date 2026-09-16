import assert from "node:assert/strict";
import test from "node:test";
import * as qualifier from "../qualify-peer-native.mjs";

const { decodeSite, instrumentDecodeSource, parseTraceLine } = qualifier;

test("qualification instrumentation binds exactly one real decode site", () => {
  const source = `before\n${decodeSite}\nafter\n`;
  const changed = instrumentDecodeSource(source);
  assert.ok(changed.includes(decodeSite));
  assert.ok(changed.indexOf("LOOM_NATIVE_BATCH_TRACE") > changed.indexOf(decodeSite));
  assert.throws(() => instrumentDecodeSource("no native decode here"));
  assert.throws(() => instrumentDecodeSource(`${source}${source}`));
  assert.throws(() => instrumentDecodeSource(changed));
});

test("ordinary logs and fixture callback messages are not native traces", () => {
  assert.equal(parseTraceLine("two callbacks executed"), null);
  assert.equal(parseTraceLine("native log: loaded model"), null);
  const record = { kind: "loom_native_decode_batch_v1", request_id: "batch", case_ids: ["a", "b"], sequence_ids: [0, 1], generated_counts: [1, 1] };
  assert.deepEqual(parseTraceLine(`LOOM_NATIVE_BATCH_TRACE ${JSON.stringify(record)}`), record);
  assert.throws(() => parseTraceLine("LOOM_NATIVE_BATCH_TRACE malformed"));
  assert.throws(() => parseTraceLine('LOOM_NATIVE_BATCH_TRACE {"kind":"fixture"}'));
});

test("production request-to-case mapping is observed at exactly one native admission", () => {
  const source = `before\n${qualifier.independentSite}\nafter\n`;
  const changed = qualifier.instrumentIndependentSource(source);
  assert.ok(changed.indexOf("LOOM_NATIVE_BATCH_MAPPING") > changed.indexOf(qualifier.independentSite));
  assert.throws(() => qualifier.instrumentIndependentSource("missing admission"));
  assert.throws(() => qualifier.instrumentIndependentSource(`${source}${source}`));
  assert.throws(() => qualifier.instrumentIndependentSource(changed));
  const record = { kind: "loom_native_batch_mapping_v1", request_id: "batch", members: [{ request_id: "owner", case_id: "a" }] };
  assert.deepEqual(qualifier.parseMappingLine(`LOOM_NATIVE_BATCH_MAPPING ${JSON.stringify(record)}`), record);
  assert.equal(qualifier.parseMappingLine("ordinary model log"), null);
  assert.throws(() => qualifier.parseMappingLine('LOOM_NATIVE_BATCH_MAPPING {"kind":"fixture"}'));
});

test("only the actual plugin libtest executable is selected from Cargo output", () => {
  const artifact = {
    reason: "compiler-artifact", target: { name: "tauri_plugin_loom", kind: ["lib"] },
    profile: { test: true }, executable: "/target/release/deps/tauri_plugin_loom-123",
  };
  assert.equal(qualifier.parseTestExecutable(JSON.stringify(artifact)), artifact.executable);
  assert.equal(qualifier.parseTestExecutable("ordinary build log"), null);
  for (const other of [
    { ...artifact, reason: "build-script-executed" },
    { ...artifact, target: { name: "tauri_plugin_loom", kind: ["example"] } },
    { ...artifact, target: { name: "another_crate", kind: ["lib"] } },
    { ...artifact, profile: { test: false } },
    { ...artifact, executable: null },
  ]) {
    assert.equal(qualifier.parseTestExecutable(JSON.stringify(other)), null);
  }
});
