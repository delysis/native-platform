import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const BASELINE = '637e60b6b044230ed24ed3118615a2e5538cae83';
export const INVENTORY_PATH = 'docs/consolidation/20260924/branches.csv';
export const REVIEW_PATH = 'docs/consolidation/20260925/head-review.json';
export const INVENTORY_SHA256 = '63bafd4c6c378d2a86e3db3673f9043e56755ec1e08d7f7e55d47af196c45227';
const LIMIT = 1024 * 1024;
const SHA = /^[0-9a-f]{40}$/u;
const EVIDENCE = new Set(['original-audit-exact-inputs', 'this-turn-exact-compare', 'earlier-conversation-exact-compare']);
const DISPOSITIONS = new Set(['in-main-history', 'baseline', 'semantic-rebase-required', 'park-research', 'selective-recovery', 'qualify-candidate']);
const HISTORY_PATH = 'crates/workspace-document/src/history.rs';
const HISTORY_BLOB = 'd3c20b65b1e1329f2bd6d82fece7de2324b6c168';
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

// Small strict CSV reader: quoted commas/newlines and doubled quotes are data.
// This reads a frozen evidence file, not an arbitrary filesystem or Git ref.
export function parseCsv(text) {
  assert.equal(typeof text, 'string', 'CSV must be text');
  assert(Buffer.byteLength(text) <= LIMIT, 'CSV exceeds its byte bound');
  const rows = [];
  let row = [], cell = '', quoted = false, closed = false;
  for (let index = 0; index < text.length; index += 1) {
    const ch = text[index];
    if (quoted) {
      if (ch === '"' && text[index + 1] === '"') { cell += '"'; index += 1; }
      else if (ch === '"') { quoted = false; closed = true; }
      else cell += ch;
      continue;
    }
    if (closed && ch !== ',' && ch !== '\n' && ch !== '\r') throw new Error('text after closing CSV quote');
    if (ch === '"') {
      assert(!closed && cell.length === 0, 'quote inside unquoted CSV field');
      quoted = true;
    } else if (ch === ',') {
      row.push(cell); cell = ''; closed = false;
    } else if (ch === '\n' || ch === '\r') {
      if (ch === '\r' && text[index + 1] === '\n') index += 1;
      row.push(cell); rows.push(row); row = []; cell = ''; closed = false;
    } else cell += ch;
  }
  assert(!quoted, 'unterminated CSV field');
  if (closed || cell.length || row.length) { row.push(cell); rows.push(row); }
  return rows;
}

function originalRows(bytes) {
  assert(Buffer.isBuffer(bytes) && bytes.length <= LIMIT, 'inventory exceeds its byte bound');
  // The supplied archive used CRLF and the checked-in copy uses LF. Compare
  // only the declared canonical LF representation; never rewrite either file.
  const text = bytes.toString('utf8').replace(/\r\n/gu, '\n');
  assert.equal(sha256(text), INVENTORY_SHA256, 'original inventory identity changed');
  const [header, ...rows] = parseCsv(text);
  assert.equal(new Set(header).size, header.length, 'duplicate CSV heading');
  const originals = new Map();
  for (const cells of rows) {
    assert.equal(cells.length, header.length, 'CSV row width changed');
    const record = Object.fromEntries(header.map((key, index) => [key, cells[index]]));
    assert.equal(record.main_sha, BASELINE, 'original baseline changed');
    assert(SHA.test(record.head_sha), 'invalid original head');
    assert(!originals.has(record.branch), 'duplicate original head');
    originals.set(record.branch, record);
  }
  assert.equal(originals.size, 77, 'original inventory count changed');
  return originals;
}

function relation(entry) {
  assert(SHA.test(entry.head) && SHA.test(entry.merge_base), 'invalid comparison identity');
  assert(Number.isSafeInteger(entry.ahead) && entry.ahead >= 0, 'invalid ahead count');
  assert(Number.isSafeInteger(entry.behind) && entry.behind >= 0, 'invalid behind count');
  switch (entry.status) {
    case 'identical':
      assert.equal(entry.head, BASELINE); assert.equal(entry.merge_base, BASELINE);
      assert.equal(entry.ahead, 0); assert.equal(entry.behind, 0);
      assert.equal(entry.disposition, 'baseline'); break;
    case 'behind':
      assert.equal(entry.ahead, 0); assert(entry.behind > 0);
      assert.equal(entry.merge_base, entry.head); assert.notEqual(entry.head, BASELINE);
      assert.equal(entry.disposition, 'in-main-history'); break;
    case 'ahead':
      assert(entry.ahead > 0); assert.equal(entry.behind, 0);
      assert.equal(entry.merge_base, BASELINE); assert.notEqual(entry.head, BASELINE);
      assert.notEqual(entry.disposition, 'in-main-history'); break;
    case 'diverged':
      assert(entry.ahead > 0 && entry.behind > 0);
      assert.notEqual(entry.merge_base, BASELINE); assert.notEqual(entry.merge_base, entry.head);
      assert.notEqual(entry.disposition, 'in-main-history'); break;
    default: throw new Error('unknown ancestry cannot be marked reviewed');
  }
}

/** Validate coverage and recorded provenance, NOT semantic correctness or Git
 * ancestry itself. Comparisons remain separately attributed evidence. */
export function validateReview(review, inventory) {
  const originals = originalRows(inventory);
  assert.equal(review.schema, 'delysis.consolidation-head-review.v1');
  assert.equal(review.repository, 'delysis/native-platform');
  assert.equal(review.baseline, BASELINE);
  assert.equal(review.original_inventory_path, INVENTORY_PATH);
  assert.equal(review.original_inventory_sha256, INVENTORY_SHA256);
  assert.equal(review.original_head_count, originals.size);
  assert.equal(review.automatic_merge, false, 'inventory is not merge authority');
  assert.equal(review.automatic_deletion, false, 'inventory is not deletion authority');
  assert.equal(review.semantic_review_complete, false, 'ancestry is not semantic qualification');
  assert(Array.isArray(review.entries) && review.entries.length === originals.size, 'missing or extra review entry');
  const seen = new Set(), counts = { behind: 0, diverged: 0, ahead: 0, identical: 0 };
  for (const entry of review.entries) {
    const original = originals.get(entry.branch);
    assert(original, 'review includes a head outside the original scope');
    assert(!seen.has(entry.branch), 'duplicate review entry'); seen.add(entry.branch);
    assert.equal(entry.head, original.head_sha, `head moved: ${entry.branch}`);
    assert(EVIDENCE.has(entry.evidence), 'unattributed comparison');
    assert(DISPOSITIONS.has(entry.disposition), 'unreviewed disposition');
    assert(Object.hasOwn(review.families, entry.family), 'unknown recovery family');
    assert.equal(review.families[entry.family].disposition, entry.disposition);
    relation(entry); counts[entry.status] += 1;
    if (entry.evidence === 'original-audit-exact-inputs') {
      assert.notEqual(original.status, 'unknown', 'unknown original comparison cannot be reused');
      assert.equal(entry.status, original.status);
      assert.equal(entry.ahead, Number(original.ahead)); assert.equal(entry.behind, Number(original.behind));
      assert.equal(entry.merge_base, original.merge_base);
    }
  }
  assert.equal(seen.size, originals.size, 'not all original heads are accounted for');
  assert(Array.isArray(review.selective_recoveries) && review.selective_recoveries.length === 1);
  const recovered = review.selective_recoveries[0];
  assert.equal(recovered.source_head, originals.get('codex/mom-loom-convergence').head_sha);
  assert.equal(recovered.source_path, HISTORY_PATH); assert.equal(recovered.destination_path, HISTORY_PATH);
  assert.equal(recovered.blob, HISTORY_BLOB);
  assert.equal(recovered.commit, 'b09be65ce15233815b6ea56c813b0c39246f2934'); assert.equal(recovered.pr, 103);
  // This is a historical import record, not a freeze on future source edits.
  // The blob was independently checked at the recorded commit. Current source
  // may evolve without rewriting that provenance or duplicating old code.
  return { scope: 'original-77-heads', evidence_class: 'recorded-inventory-consistency',
    counts, recovery_records: 1, semantic_qualification: false, automatic_merge: false };
}

function readBounded(file) {
  const fd = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NONBLOCK ?? 0));
  try {
    const stat = fs.fstatSync(fd);
    assert(stat.isFile() && stat.size <= LIMIT, 'evidence must be a bounded regular file');
    const data = Buffer.alloc(LIMIT + 1);
    let length = 0;
    while (length <= LIMIT) {
      const count = fs.readSync(fd, data, length, data.length - length, null);
      if (!count) break;
      length += count;
    }
    assert(length <= LIMIT, 'evidence grew beyond its byte bound');
    return data.subarray(0, length);
  } finally { fs.closeSync(fd); }
}

export function verifyCheckout(root) {
  return validateReview(JSON.parse(readBounded(path.join(root, REVIEW_PATH)).toString('utf8')),
    readBounded(path.join(root, INVENTORY_PATH)));
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  assert.equal(process.argv.length, 2, 'this verifier takes no Git refs or output destinations');
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
  console.log(JSON.stringify(verifyCheckout(root), null, 2));
}
