import assert from 'node:assert/strict';
import { it } from 'vitest';
import {
  authorizeCompletionInsertion,
  authorizeCompletionUnconsume,
  completionControllerView,
  initialCompletionControllerState,
  observeTextMutation,
  reconcileCompletionController,
  refreshCompletionCandidate,
  retireCompletionCandidates
} from './completionController';
import { remainingCompletionText } from './completionSession';

const contextKey = 'session:document:1:visual';
const family = [' one two', ' another path', ' third road', ' final turn'].map((text, i) => ({
  candidateId: `candidate-${i}`, runId: `run-${i}`,
  presentationKey: `stream:run-${i}:1`, text, targetByte: 5, insertsOnAccept: true
}));

function acceptedWord() {
  const initial = reconcileCompletionController(initialCompletionControllerState(), contextKey, family);
  const view = completionControllerView(initial, contextKey, family);
  assert(view.selected);
  const authorized = authorizeCompletionInsertion(initial, {
    contextKey, family: view.activeFamily, eligible: view.selected,
    candidateId: view.selected.candidateId, presentationKey: view.selected.presentationKey,
    text: ' one', action: 'inline_tab', manuscriptText: 'Hello', promotionReady: true
  });
  assert.equal(authorized.authorized, true);
  const settled = observeTextMutation(authorized.state, 'Hello one', 'Hello', false).state;
  assert(settled.session);
  return { pending: authorized.state, settled };
}

it('continues the same Ghost stream after accepting a word without changing accepted bytes', () => {
  const { settled } = acceptedWord();
  const original = settled.session!;
  const next = refreshCompletionCandidate(settled, original, 'run-0', ' one two more words', 'stream:run-0:2');
  assert(next.session);
  assert.equal(remainingCompletionText(next.session), ' two more words');
  assert.deepEqual(next.session.acceptedChunks, [' one']);
  assert.equal(next.session.selectedRunId, original.selectedRunId);
  assert.equal(next.session.candidates[0].targetByte, 5);
  assert.equal(next.session.candidates[0].candidateId, 'candidate-0');
  assert.equal(next.pendingText, null);
  assert.equal(next.actionSequence, settled.actionSequence);
  assert.strictEqual(next.lastAction, settled.lastAction);
  assert.equal(remainingCompletionText(original), ' two');
  assert.strictEqual(next.session.candidates[1], original.candidates[1]);

  const view = completionControllerView(next, contextKey, []);
  assert(view.selected);
  const reverse = authorizeCompletionUnconsume(next, {
    eligible: view.selected, candidateId: view.selected.candidateId,
    presentationKey: view.selected.presentationKey, text: ' one', manuscriptText: 'Hello one'
  });
  assert.equal(reverse.authorized, true);
  assert.equal(reverse.state.pendingText, 'Hello');
  assert.equal(remainingCompletionText(reverse.state.session!), ' one two more words');
});

for (const [label, runId, text, key] of [
  ['changed accepted prefix', 'run-0', ' other two more', 'stream:run-0:2'],
  ['changed unaccepted prefix', 'run-0', ' one three more', 'stream:run-0:2'],
  ['truncated prefix', 'run-0', ' one t', 'stream:run-0:2'],
  ['same-length replacement', 'run-0', ' one six', 'stream:run-0:2'],
  ['reused presentation identity', 'run-0', ' one two more', 'stream:run-0:1'],
  ['unknown run', 'run-unknown', ' one two more', 'stream:run-unknown:2'],
  ['incompatible sibling', 'run-1', ' another path more', 'stream:run-1:2'],
] as const) {
  it(`does not append a ${label} to accepted Ghost authority`, () => {
    const { settled } = acceptedWord();
    assert.strictEqual(refreshCompletionCandidate(settled, settled.session!, runId, text, key), settled);
  });
}

it('does not replace an in-flight insertion with a late stream update', () => {
  const { pending } = acceptedWord();
  assert.strictEqual(refreshCompletionCandidate(pending, pending.session!, 'run-0', ' one two more', 'stream:run-0:2'), pending);
});

it('does not revive a retired sampling policy with late stream bytes', () => {
  const { settled } = acceptedWord();
  const retired = retireCompletionCandidates(settled);
  assert(retired.session);
  assert.strictEqual(refreshCompletionCandidate(retired, retired.session, 'run-0', ' one two more', 'stream:run-0:2'), retired);
});

it('ignores refreshes captured before the session changed', () => {
  const { settled } = acceptedWord();
  const stale = { ...settled.session! };
  assert.strictEqual(refreshCompletionCandidate(settled, stale, 'run-0', ' one two more', 'stream:run-0:2'), settled);
});

it('rejects late growth from an earlier document even when run ids match', () => {
  const { settled } = acceptedWord();
  const current = reconcileCompletionController(settled, 'session:other-document:2:visual', family);
  assert.strictEqual(refreshCompletionCandidate(current, settled.session!, 'run-0', ' one two more', 'stream:run-0:2'), current);
});
