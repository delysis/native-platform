import assert from "node:assert/strict";
import test from "node:test";
import { decodeSite, instrumentDecodeSource, parseTraceLine } from "../qualify-peer-native.mjs";

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
