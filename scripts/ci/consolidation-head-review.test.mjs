import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { BASELINE, INVENTORY_PATH, REVIEW_PATH, parseCsv, validateReview, verifyCheckout } from '../consolidation/verify-head-review.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const inventory = fs.readFileSync(path.join(root, INVENTORY_PATH));
const source = JSON.parse(fs.readFileSync(path.join(root, REVIEW_PATH), 'utf8'));
function changed(edit) { const value = structuredClone(source); edit(value); return value; }
function reject(edit, pattern) { assert.throws(() => validateReview(changed(edit), inventory), pattern); }

test('all original heads have pinned ancestry records, not automatic approval', () => {
  const receipt = verifyCheckout(root);
  assert.deepEqual(receipt.counts, { behind: 55, diverged: 20, ahead: 1, identical: 1 });
  assert.equal(receipt.semantic_qualification, false);
  assert.equal(receipt.automatic_merge, false);
  assert.equal(receipt.recovery_records, 1);
});
test('a missing original head is not a complete review', () => reject(v => v.entries.pop(), /missing or extra review entry/u));
test('a duplicate cannot conceal a missing original head', () => reject(v => v.entries[1] = v.entries[0], /duplicate review entry/u));
test('a moved ref invalidates its old comparison', () => reject(v => v.entries[0].head = '1'.repeat(40), /head moved/u));
test('a changed baseline cannot inherit the old ledger', () => reject(v => v.baseline = '1'.repeat(40)));
test('unknown ancestry cannot receive a completed disposition', () => reject(v => v.entries[0].status = 'unknown', /unknown ancestry/u));
test('an unmerged head cannot be recast as already integrated', () => reject(v => v.entries.find(e => e.status === 'diverged').status = 'behind'));
test('negative and noninteger counts cannot masquerade as compare evidence', () => {
  for (const count of [-1, 1.5, Number.MAX_SAFE_INTEGER + 1, '3']) {
    reject(v => v.entries[0].ahead = count, /invalid ahead count/u);
  }
});
test('original unknowns cannot be silently attributed to the original audit', () => reject(v => {
  v.entries.find(e => e.branch === 'chat/peer-continuous-admission').evidence = 'original-audit-exact-inputs';
}, /unknown original comparison/u));
test('comparison evidence and recovery families are closed inventories', () => {
  reject(v => v.entries[0].evidence = 'guessed', /unattributed comparison/u);
  reject(v => v.entries[0].family = 'approve-everything', /unknown recovery family/u);
  reject(v => v.entries[0].disposition = 'approved', /unreviewed disposition/u);
});
test('a branch census never grants merge or deletion authority', () => {
  reject(v => v.automatic_merge = true, /not merge authority/u);
  reject(v => v.automatic_deletion = true, /not deletion authority/u);
  reject(v => v.semantic_review_complete = true, /not semantic qualification/u);
});
test('recorded import identity cannot drift but future source edits are not frozen', () => {
  reject(v => v.selective_recoveries[0].blob = '0'.repeat(40));
  reject(v => v.selective_recoveries[0].commit = '0'.repeat(40));
  reject(v => v.selective_recoveries[0].destination_path = 'some/other/file.rs');
  const code = fs.readFileSync(path.join(root, 'scripts/consolidation/verify-head-review.mjs'), 'utf8');
  assert.doesNotMatch(code, /readBounded\(path\.join\(root, HISTORY_PATH\)\)/u);
});
test('the supplied CRLF and checked-in LF inventory have identical recorded content', () => {
  const crlf = Buffer.from(inventory.toString('utf8').replace(/\n/gu, '\r\n'));
  assert.deepEqual(validateReview(source, crlf), validateReview(source, inventory));
});
test('inventory content changes fail even when row counts still match', () => {
  const wrong = Buffer.from(inventory.toString('utf8').replace(BASELINE, '1'.repeat(40)));
  assert.throws(() => validateReview(source, wrong), /inventory identity changed/u);
});
test('strict CSV parsing preserves quoted commas, newlines and doubled quotes', () => {
  assert.deepEqual(parseCsv('a,b\r\n"x,y","line1\nline2"\r\n"say ""hi""",end'),
    [['a', 'b'], ['x,y', 'line1\nline2'], ['say "hi"', 'end']]);
  for (const malformed of ['"unclosed', 'ab"cd', '"closed"extra']) assert.throws(() => parseCsv(malformed));
});
test('the verifier remains a bounded, read-only evidence check', () => {
  assert.throws(() => parseCsv('x'.repeat(1024 * 1024 + 1)), /byte bound/u);
  const code = fs.readFileSync(path.join(root, 'scripts/consolidation/verify-head-review.mjs'), 'utf8');
  assert.doesNotMatch(code, /child_process|writeFile|unlink|renameSync|fetch\(/u);
});
