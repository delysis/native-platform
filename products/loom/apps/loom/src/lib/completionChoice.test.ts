import { expect, it } from 'vitest';
import {
  clearCompletionSession,
  completionControllerView,
  cycleCompletion,
  initialCompletionControllerState,
  invalidateCompletionNavigation,
  observeTextMutation,
  reconcileCompletionController,
  resetCompletionDiscovery,
  resetCompletionSurface,
  retireCompletionCandidates
} from './completionController';
import type { CompletionControllerState } from './completionController';
import type { InlineGhostSuggestion } from './inlineSuggestionFamily';

const context = 'owned-session:owned-document:7:visual';
const family: InlineGhostSuggestion[] = ['a', 'b', 'c', 'd'].map(slot => ({
  runId: `run-${slot}`, candidateId: `candidate-${slot}`,
  presentationKey: `presentation-${slot}-1`, text: ` ${slot} ordinary prose`,
  targetByte: 5, insertsOnAccept: true
}));
function ready(): CompletionControllerState {
  return reconcileCompletionController(initialCompletionControllerState(), context, family);
}
function refreshed(): InlineGhostSuggestion[] {
  return family.map(candidate => candidate.runId === 'run-b'
    ? { ...candidate, text: `${candidate.text} continues`, presentationKey: 'presentation-b-2' }
    : candidate);
}

it('retains an explicit writer choice after a compatible immutable family refresh', () => {
  const writer = cycleCompletion(ready(), family, 1).state;
  const choices = refreshed();
  const current = reconcileCompletionController(writer, context, choices);
  expect(current.session).not.toBe(writer.session);
  expect(current.session?.selectedRunId).toBe('run-b');
  expect(completionControllerView(current, context, choices).selected?.runId).toBe('run-b');
});

it('retains writer choice through temporary absence and exact same-run re-admission', () => {
  const writer = cycleCompletion(ready(), family, 1).state;
  const absent = reconcileCompletionController(writer, context, []);
  expect(absent.session).toBeNull();
  expect(absent.pendingText).toBeNull();
  expect(completionControllerView(absent, context, []).selected).toBeNull();
  const choices = refreshed();
  const admitted = reconcileCompletionController(absent, context, choices);
  expect(admitted.session?.selectedRunId).toBe('run-b');
  expect(completionControllerView(admitted, context, choices).selected?.runId).toBe('run-b');
});

it.each(['context', 'candidate', 'anchor', 'missing-run'] as const)(
  'does not transfer writer intent to a replacement candidate (%s)', change => {
    const writer = cycleCompletion(ready(), family, 1).state;
    const absent = reconcileCompletionController(writer, context, []);
    const key = change === 'context' ? `${context}:new-context` : context;
    const choices = refreshed().map(candidate => change === 'anchor'
      ? { ...candidate, targetByte: 6 }
      : candidate.runId !== 'run-b' ? candidate
      : change === 'candidate' ? { ...candidate, candidateId: 'replacement-b' }
      : change === 'missing-run' ? { ...candidate, runId: 'replacement-run' }
      : candidate);
    const admitted = reconcileCompletionController(absent, key, choices);
    expect(admitted.selectionIntent.writerChoice).toBeNull();
    const selected = completionControllerView(admitted, key, choices).selected;
    expect(selected?.runId).toBe(admitted.activeRunId ?? choices[0].runId);
  }
);

it.each(['clear', 'surface', 'discovery', 'retire', 'mutation', 'navigation'] as const)(
  'revokes writer preference during absence at the existing %s owner', owner => {
    const absent = reconcileCompletionController(cycleCompletion(ready(), family, 1).state, context, []);
    const revoked = owner === 'clear' ? clearCompletionSession(absent)
      : owner === 'surface' ? resetCompletionSurface(absent)
      : owner === 'discovery' ? resetCompletionDiscovery(absent)
      : owner === 'retire' ? retireCompletionCandidates(absent)
      : owner === 'mutation' ? observeTextMutation(absent, 'Hello!', 'Hello', false).state
      : invalidateCompletionNavigation(absent, 'visual', true, 0).state;
    expect(revoked.selectionIntent.writerChoice).toBeNull();
    const choices = refreshed();
    const admitted = reconcileCompletionController(revoked, context, choices);
    expect(admitted.selectionIntent.writerChoice).toBeNull();
    expect(completionControllerView(admitted, context, choices).selected?.runId).toBe('run-b');
  }
);
