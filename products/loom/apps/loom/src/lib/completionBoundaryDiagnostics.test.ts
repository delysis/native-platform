import assert from 'node:assert/strict';
import { it } from 'vitest';
import {
  appendCompletionBoundaryObservation, boundaryKeyKind, COMPLETION_BOUNDARY_TRACE_LIMIT,
  emptyCompletionBoundaryTrace, textBoundaryDelta, type BoundaryScope
} from './completionBoundaryDiagnostics';

const scope: BoundaryScope = {
  session_id: 'session', document_id: 'document', document_epoch: 1, edit_version: 1,
  source_revision_id: 'revision', visible_blob_id: 'blob'
};
const sentinel = 'Loom native smoke prose: The lantern crossed the quiet room, casting a narrow pool of light across the ';

it('distinguishes the exact 103-to-102 terminal-space loss without retaining text', () => {
  const delta = textBoundaryDelta(sentinel, sentinel.slice(0, -1));
  assert.deepEqual(delta, {
    before_utf8_bytes: 103, after_utf8_bytes: 102, equal: false, first_changed_byte: 102,
    removed_utf8_bytes: 1, inserted_utf8_bytes: 0, exact_terminal_space_removed: true
  });
  assert.equal(JSON.stringify(delta).includes('lantern'), false);
});

it('does not hide a terminal space by trimming the expected surface', () => {
  const delta = textBoundaryDelta(sentinel, sentinel);
  assert.equal(delta.before_utf8_bytes, 103);
  assert.equal(delta.after_utf8_bytes, 103);
  assert.equal(delta.equal, true);
  assert.equal(delta.first_changed_byte, null);
  assert.equal(delta.exact_terminal_space_removed, false);
});

it('does not misclassify an interior deletion, substitution, or newline loss as EOF space loss', () => {
  for (const [before, after] of [['a b', 'ab'], ['ab ', 'ac'], ['ab\n', 'ab'], ['ab\u00a0', 'ab']]) {
    assert.equal(textBoundaryDelta(before, after).exact_terminal_space_removed, false);
  }
});

it('counts UTF-8 bytes rather than UTF-16 units', () => {
  assert.deepEqual(textBoundaryDelta('é🜁 ', 'é🜁'), {
    before_utf8_bytes: 7, after_utf8_bytes: 6, equal: false, first_changed_byte: 6,
    removed_utf8_bytes: 1, inserted_utf8_bytes: 0, exact_terminal_space_removed: true
  });
});

it('records an exact insertion and the empty boundary', () => {
  assert.equal(textBoundaryDelta('', '').equal, true);
  assert.equal(textBoundaryDelta('', ' ').exact_terminal_space_removed, false);
  assert.equal(textBoundaryDelta('hello ', 'hello world').inserted_utf8_bytes, 5);
  assert.equal(textBoundaryDelta('hello ', 'hello world').removed_utf8_bytes, 0);
});

it('does not treat equal-sized replacement bytes as equal text', () => {
  const delta = textBoundaryDelta('aéz', 'aèz');
  assert.equal(delta.equal, false);
  assert.equal(delta.first_changed_byte, 2);
  assert.equal(delta.removed_utf8_bytes, 1);
  assert.equal(delta.inserted_utf8_bytes, 1);
});

it('retains the first loss when later Shuttle observations fill the bounded ring', () => {
  let trace = appendCompletionBoundaryObservation(emptyCompletionBoundaryTrace(), {
    kind: 'visual_transaction', delta: textBoundaryDelta(sentinel, sentinel.slice(0, -1)), facts: {}
  }, scope, 100);
  const first = trace.first_terminal_space_loss;
  for (let index = 0; index < 40; index += 1) {
    trace = appendCompletionBoundaryObservation(trace, {
      kind: 'shuttle_fire', facts: { result: 'editor_rejected' }
    }, scope, 101 + index);
  }
  assert.equal(trace.entries.length, COMPLETION_BOUNDARY_TRACE_LIMIT);
  assert.equal(trace.sequence, 41);
  assert.equal(trace.dropped_entries, 41 - COMPLETION_BOUNDARY_TRACE_LIMIT);
  assert.equal(trace.first_terminal_space_loss, first);
  assert.equal(trace.first_terminal_space_loss?.sequence, 1);
  assert.equal(trace.entries.some(entry => entry.sequence === 1), false);
});

it('does not overwrite the original transaction with its later App projection', () => {
  const delta = textBoundaryDelta('hello ', 'hello');
  const first = appendCompletionBoundaryObservation(emptyCompletionBoundaryTrace(), {
    kind: 'visual_transaction', delta, facts: { input_type: 'deleteContentBackward' }
  }, scope, 100);
  const second = appendCompletionBoundaryObservation(first, {
    kind: 'app_update_text', delta, facts: { transition: 'idle' }
  }, scope, 340);
  assert.equal(second.first_terminal_space_loss?.kind, 'visual_transaction');
  assert.equal(second.entries[1].kind, 'app_update_text');
  assert.equal(first.entries.length, 1);
});

it('keeps input association and authorization independent of the byte relationship', () => {
  const trace = appendCompletionBoundaryObservation(emptyCompletionBoundaryTrace(), {
    kind: 'visual_transaction', delta: textBoundaryDelta('hello ', 'hello'),
    facts: { input_type: 'deleteContentBackward', input_trusted: true, completion_authorized: false }
  }, scope, 100);
  assert.equal(trace.first_terminal_space_loss?.facts.input_trusted, true);
  assert.equal(trace.first_terminal_space_loss?.facts.completion_authorized, false);
  assert.equal('cause' in trace.first_terminal_space_loss!, false);
});

it('snapshots scope and metadata rather than retaining caller-owned mutable records', () => {
  const mutableScope = { ...scope };
  const facts = { focused: true };
  const trace = appendCompletionBoundaryObservation(emptyCompletionBoundaryTrace(), {
    kind: 'shuttle_fire', facts
  }, mutableScope, 100);
  mutableScope.edit_version = 9;
  facts.focused = false;
  assert.equal(trace.entries[0].scope.edit_version, 1);
  assert.equal(trace.entries[0].facts.focused, true);
});

it('starts a new empty diagnostic lifetime without carrying an old document loss', () => {
  const trace = emptyCompletionBoundaryTrace();
  assert.equal(trace.first_terminal_space_loss, null);
  assert.equal(trace.sequence, 0);
  assert.equal(trace.dropped_entries, 0);
  assert.deepEqual(trace.entries, []);
});

it('records relevant controls but never stores printable manuscript key characters', () => {
  const key = (value: string, metaKey = false, shiftKey = false) => boundaryKeyKind({ key: value, metaKey, shiftKey });
  assert.equal(key('G', true, true), 'suggestions_shortcut');
  assert.equal(key('j', true, true), 'shuttle_shortcut');
  assert.equal(key('Backspace'), 'Backspace');
  assert.equal(key(' '), 'other');
  assert.equal(key('é'), 'other');
  assert.equal(key('secret string'), 'other');
});
