import { expect, it } from 'vitest';
import {
  authorizeCompletionInsertion,
  clearCompletionSession,
  completionControllerView,
  cycleCompletion,
  initialCompletionControllerState,
  invalidateCompletionNavigation,
  observeTextMutation,
  reconcileCompletionController,
  rejectVisualPresentation,
  resetCompletionDiscovery,
  resetCompletionSurface,
  retireCompletionCandidates
} from './completionController';
import type { CompletionControllerState } from './completionController';
import type { InlineGhostSuggestion } from './inlineSuggestionFamily';

// Owned public-controller inputs; these are not native model/seal fixtures.
const context = 'owned-session:owned-document:7:visual';
const family: InlineGhostSuggestion[] = ['a', 'b', 'c', 'd'].map(slot => ({
  runId: `run-${slot}`, candidateId: `candidate-${slot}`,
  presentationKey: `presentation-${slot}-1`, text: ` ${slot} ordinary prose`,
  targetByte: 5, insertsOnAccept: true
}));
function ready(): CompletionControllerState {
  return reconcileCompletionController(initialCompletionControllerState(), context, family);
}
function rejectSelected(state: CompletionControllerState, choices = family, key = context) {
  const eligible = completionControllerView(state, key, choices, true, true).selected;
  expect(eligible, 'the causal acknowledgement must name actual eligible output').not.toBeNull();
  return rejectVisualPresentation(state, {
    mode: 'visual', eligible, candidateId: eligible!.candidateId,
    presentationKey: eligible!.presentationKey, surfaceKey: 'owned-surface',
    currentSurfaceKey: 'owned-surface', anchorByte: eligible!.targetByte
  });
}
function ordinary(): void {
  expect(rejectSelected(ready()).activeRunId).toBe('run-b');
  const writer = cycleCompletion(ready(), family, 1).state;
  expect(writer.activeRunId).toBe('run-b');
  expect(rejectSelected(writer).activeRunId).toBe('run-b');
}
function refreshed(): InlineGhostSuggestion[] {
  return family.map(candidate => candidate.runId === 'run-b'
    ? { ...candidate, text: `${candidate.text} continues`, presentationKey: 'presentation-b-2' }
    : candidate);
}

it('retains writer choice after a compatible immutable family refresh', () => {
  ordinary();
  const writer = cycleCompletion(ready(), family, 1).state;
  const choices = refreshed();
  const current = reconcileCompletionController(writer, context, choices);
  expect(current.session).not.toBe(writer.session);
  expect(current.session?.selectedRunId).toBe('run-b');
  const rejected = rejectSelected(current, choices);
  expect(rejected.activeRunId, 'refreshed writer B must refuse without selecting unchosen C').toBe('run-b');
  expect(completionControllerView(rejected, context, choices, true, true).selected).toBeNull();
});

it('retains writer choice through empty refusal and exact same-run re-admission', () => {
  ordinary();
  const writer = rejectSelected(cycleCompletion(ready(), family, 1).state);
  const absent = reconcileCompletionController(writer, context, []);
  expect(absent.session).toBeNull();
  expect(absent.pendingText).toBeNull();
  expect(completionControllerView(absent, context, [], true, true).selected).toBeNull();
  expect(authorizeCompletionInsertion(absent, {
    contextKey: context, family: [], eligible: null, candidateId: 'candidate-b',
    presentationKey: 'presentation-b-1', text: ' ', action: 'word',
    manuscriptText: 'Hello', promotionReady: true
  }).authorized).toBe(false);
  const choices = refreshed();
  const admitted = reconcileCompletionController(absent, context, choices);
  expect(admitted.session?.selectedRunId).toBe('run-b');
  const rejected = rejectSelected(admitted, choices);
  expect(rejected.activeRunId, 're-admitted writer B must refuse without selecting unchosen C').toBe('run-b');
  expect(completionControllerView(rejected, context, choices, true, true).selected).toBeNull();
});

it.each(['context', 'candidate', 'anchor', 'missing-run'] as const)(
  'does not label a replacement default as writer intent (%s)', change => {
    ordinary();
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
    const selected = admitted.session!.selectedRunId;
    const rejected = rejectSelected(admitted, choices, key);
    expect(rejected.activeRunId).not.toBe(selected);
  }
);

it.each(['clear', 'surface', 'discovery', 'retire', 'mutation', 'navigation'] as const)(
  'revokes writer preference during absence at the existing %s owner', owner => {
    ordinary();
    const absent = reconcileCompletionController(cycleCompletion(ready(), family, 1).state, context, []);
    const revoked = owner === 'clear' ? clearCompletionSession(absent)
      : owner === 'surface' ? resetCompletionSurface(absent)
      : owner === 'discovery' ? resetCompletionDiscovery(absent)
      : owner === 'retire' ? retireCompletionCandidates(absent)
      : owner === 'mutation' ? observeTextMutation(absent, 'Hello!', 'Hello', false).state
      : invalidateCompletionNavigation(absent, 'visual', true, 0).state;
    const choices = refreshed();
    const admitted = reconcileCompletionController(revoked, context, choices);
    expect(rejectSelected(admitted, choices).activeRunId).toBe('run-c');
  }
);
