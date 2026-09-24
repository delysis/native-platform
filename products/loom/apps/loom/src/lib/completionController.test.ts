import { describe, expect, it } from 'vitest';
import {
  armCompletionScheduleIntent,
  authorizeCompletionInsertion,
  authorizeCompletionUnconsume,
  completionControllerView,
  completionExhausted,
  cycleCompletion,
  initialCompletionControllerState,
  invalidateCompletionNavigation,
  observeTextMutation,
  rejectVisualPresentation,
  reconcileCompletionController,
  retireCompletionCandidates,
  setCompletionSchedule,
  settleCompletionNavigation,
  shuttleScheduleKey
} from './completionController';
import type { InlineGhostSuggestion } from './inlineSuggestionFamily';
import type { CompletionInsertionAction } from './suggestionInteraction';

const contextKey = 'session:document:7:visual';
const manuscript = 'Hello';
const family: InlineGhostSuggestion[] = [
  suggestion('run-a', 'candidate-a', ' one two'),
  suggestion('run-b', 'candidate-b', ' another path'),
  suggestion('run-c', 'candidate-c', ' third road'),
  suggestion('run-d', 'candidate-d', ' final turn')
];

function suggestion(
  runId: string,
  candidateId: string,
  text: string
): InlineGhostSuggestion {
  return {
    runId,
    candidateId,
    presentationKey: `${candidateId}:presentation`,
    text,
    targetByte: 5,
    insertsOnAccept: true
  };
}

function readyController() {
  return reconcileCompletionController(
    initialCompletionControllerState(),
    contextKey,
    family
  );
}

function insert(
  action: CompletionInsertionAction,
  text = family[0].text
) {
  const state = readyController();
  const view = completionControllerView(state, contextKey, family);
  return authorizeCompletionInsertion(state, {
    contextKey,
    family: view.activeFamily,
    eligible: view.selected,
    candidateId: view.selected!.candidateId,
    presentationKey: view.selected!.presentationKey,
    text,
    action,
    manuscriptText: manuscript,
    promotionReady: true
  });
}

describe('pure completion controller', () => {
  it('retires a sampling policy without exposing its tails or losing exact reversal', () => {
    expect(retireCompletionCandidates(readyController()).session).toBeNull();
    const first = insert('option_word', ' one ');
    const state = retireCompletionCandidates(observeTextMutation(first.state, 'Hello one ', manuscript, false).state);
    const view = completionControllerView(state, contextKey, []);
    expect(view.activeFamily).toEqual([]);
    expect(view.selected).toMatchObject({ text: '', targetByte: 10 });
    expect(view.unconsumeText).toBe(' one ');
    expect(state.session?.candidates).toBe(first.state.session?.candidates);
    const undo = authorizeCompletionUnconsume(state, {
      eligible: view.selected, candidateId: view.selected!.candidateId,
      presentationKey: view.selected!.presentationKey, text: view.unconsumeText,
      manuscriptText: 'Hello one '
    });
    expect(undo.authorized).toBe(true);
    expect(undo.state.pendingText).toBe(manuscript);
    const restored = observeTextMutation(undo.state, manuscript, 'Hello one ', false).state;
    expect(completionControllerView(restored, contextKey, []).activeFamily).toEqual([]);
    const fresh = family.map((candidate, index) => ({ ...candidate,
      runId: `fresh-${index}`, candidateId: `fresh-${index}`, presentationKey: `fresh-${index}` }));
    const refilled = reconcileCompletionController(restored, contextKey, fresh, true, true);
    expect(completionControllerView(refilled, contextKey, fresh).activeFamily).toEqual(fresh);
  });
  it('cycles compatible cached ghost suffixes in both directions without changing prior prose', () => {
    const shared = [suggestion('run-a', 'a', ' one alpha'), suggestion('run-b', 'b', ' one beta'), suggestion('run-c', 'c', ' other')];
    let state = reconcileCompletionController(initialCompletionControllerState(), contextKey, shared);
    let view = completionControllerView(state, contextKey, shared);
    const first = authorizeCompletionInsertion(state, {
      contextKey, family: view.activeFamily, eligible: view.selected,
      candidateId: view.selected!.candidateId, presentationKey: view.selected!.presentationKey,
      text: ' one ', action: 'option_word', manuscriptText: manuscript, promotionReady: true
    });
    expect(first.authorized).toBe(true);
    state = observeTextMutation(first.state, 'Hello one ', manuscript, false).state;
    expect(completionControllerView(state, contextKey, [], false).activeFamily).toHaveLength(1);
    view = completionControllerView(state, contextKey, []);
    expect(view.activeFamily.map(candidate => candidate.text)).toEqual(['alpha', 'beta']);
    state = cycleCompletion(state, view.activeFamily, 1).state;
    view = completionControllerView(state, contextKey, []);
    expect(view.selected?.runId).toBe('run-b');
    const reverse = cycleCompletion(state, view.activeFamily, -1).state;
    expect(completionControllerView(reverse, contextKey, []).selected?.runId).toBe('run-a');
    expect(reverse.pendingText).toBeNull();
    expect(reverse.session?.acceptedChunks).toEqual([' one ']);
    expect(reverse.session?.candidates).toBe(state.session?.candidates);
    state = cycleCompletion(reverse, view.activeFamily, 1).state;
    const second = authorizeCompletionInsertion(state, {
      contextKey, family: view.activeFamily, eligible: view.selected,
      candidateId: view.selected!.candidateId, presentationKey: view.selected!.presentationKey,
      text: 'beta', action: 'option_word', manuscriptText: 'Hello one ', promotionReady: true
    });
    expect(second.authorized).toBe(true);
    expect(second.state.pendingText).toBe('Hello one beta');
    expect(second.state.session?.acceptedChunks).toEqual([' one ', 'beta']);
    expect(second.state.session?.candidates.map(candidate => candidate.text)).toEqual(shared.map(candidate => candidate.text));
  });

  it('keeps one four-run family and cycles it in both Option directions', () => {
    const state = readyController();
    expect(completionControllerView(state, contextKey, family).activeFamily)
      .toHaveLength(4);

    const previous = cycleCompletion(state, family, -1);
    expect(previous.state.activeRunId).toBe('run-d');
    expect(previous.effects).toEqual([
      { kind: 'announce', message: 'Suggestion 4 of 4' }
    ]);

    const next = cycleCompletion(previous.state, family, 1);
    expect(next.state.activeRunId).toBe('run-a');
    expect(next.effects).toEqual([
      { kind: 'announce', message: 'Suggestion 1 of 4' }
    ]);
  });

  it('advances an untouched visual selection past an acknowledged rejected presentation', () => {
    let state = readyController();
    const first = completionControllerView(state, contextKey, family, true, true);
    state = rejectVisualPresentation(state, {
      mode: 'visual', eligible: first.selected, candidateId: first.selected!.candidateId,
      presentationKey: first.selected!.presentationKey, surfaceKey: 'surface',
      currentSurfaceKey: 'surface', anchorByte: first.selected!.targetByte
    });
    const advanced = completionControllerView(state, contextKey, family, true, true);
    expect(state.activeRunId).toBe('run-b');
    expect(state.session?.selectedRunId).toBe('run-b');
    expect(advanced.selected?.runId).toBe('run-b');
    expect(advanced.witnessSelected?.runId).toBe('run-b');
    expect(advanced.alternatives.map((candidate) => candidate.runId)).toEqual(['run-b', 'run-c', 'run-d']);
    expect(state.session?.candidates).toEqual(family);
    expect(state.lastAction).toBeNull();
    expect(rejectVisualPresentation(state, {
      mode: 'visual', eligible: first.selected, candidateId: first.selected!.candidateId,
      presentationKey: first.selected!.presentationKey, surfaceKey: 'surface',
      currentSurfaceKey: 'surface', anchorByte: first.selected!.targetByte
    })).toBe(state);
  });

  it('records rejected visual presentations without overriding a writer selection', () => {
    let state = cycleCompletion(readyController(), family, 1).state;
    const selected = completionControllerView(state, contextKey, family, true, true).selected!;
    state = rejectVisualPresentation(state, {
      mode: 'visual', eligible: selected, candidateId: selected.candidateId,
      presentationKey: selected.presentationKey, surfaceKey: 'surface',
      currentSurfaceKey: 'surface', anchorByte: selected.targetByte
    });
    const view = completionControllerView(state, contextKey, family, true, true);
    expect(state.activeRunId).toBe('run-b');
    expect(state.session?.selectedRunId).toBe('run-b');
    expect(view.selected).toBeNull();
    expect(view.witnessSelected).toBeNull();
    expect(view.alternatives.map((candidate) => candidate.runId)).toEqual(['run-a', 'run-c', 'run-d']);
    state = cycleCompletion(state, view.activeFamily, 1).state;
    expect(state.session?.selectedRunId).toBe('run-c');
    expect(completionControllerView(state, contextKey, family, true, true).selected?.runId).toBe('run-c');
  });

  it('settles with no visual selection after every default family member is rejected', () => {
    let state = readyController();
    for (const expectedRunId of ['run-a', 'run-b', 'run-c', 'run-d']) {
      const view = completionControllerView(state, contextKey, family, true, true);
      expect(view.selected?.runId).toBe(expectedRunId);
      state = rejectVisualPresentation(state, {
        mode: 'visual', eligible: view.selected, candidateId: view.selected!.candidateId,
        presentationKey: view.selected!.presentationKey, surfaceKey: 'surface',
        currentSurfaceKey: 'surface', anchorByte: view.selected!.targetByte
      });
    }
    const settled = completionControllerView(state, contextKey, family, true, true);
    expect(state.activeRunId).toBeNull();
    expect(settled.selected).toBeNull();
    expect(settled.witnessSelected).toBeNull();
    expect(settled.alternatives).toEqual([]);
    expect(state.session?.candidates).toEqual(family);
    expect(state.lastAction).toBeNull();
  });

  it.each<CompletionInsertionAction>([
    'option_word',
    'fan_return',
    'fan_tab',
    'inline_tab',
    'shuttle_word'
  ])('routes %s through the same exact reversible insertion authority', (action) => {
    const text = action === 'option_word' || action === 'shuttle_word'
      ? ' one '
      : family[0].text;
    const authorization = insert(action, text);

    expect(authorization.authorized).toBe(true);
    expect(authorization.state.pendingText).toBe(`${manuscript}${text}`);
    expect(authorization.state.session).toMatchObject({
      selectedRunId: 'run-a',
      acceptedChunks: [text],
      authorityFrozen: true
    });
    expect(authorization.state.lastAction).toMatchObject({
      sequence: 1,
      kind: action,
      context_key: contextKey,
      run_id: 'run-a',
      candidate_id: 'candidate-a'
    });
  });

  it('reverses the last Option word against the exact inserted bytes', () => {
    const inserted = insert('option_word', ' one ');
    const projected = observeTextMutation(
      inserted.state,
      'Hello one ',
      manuscript,
      false
    );
    expect(projected.completionOwned).toBe(true);

    const view = completionControllerView(projected.state, contextKey, family);
    expect(view.selected).toMatchObject({ text: 'two', targetByte: 10 });
    expect(view.unconsumeText).toBe(' one ');
    const reversed = authorizeCompletionUnconsume(projected.state, {
      eligible: view.selected,
      candidateId: view.selected!.candidateId,
      presentationKey: view.selected!.presentationKey,
      text: view.unconsumeText,
      manuscriptText: 'Hello one '
    });

    expect(reversed.authorized).toBe(true);
    expect(reversed.state.pendingText).toBe(manuscript);
    expect(reversed.state.session?.acceptedChunks).toEqual([]);
    expect(reversed.state.session?.authorityFrozen).toBe(true);
  });

  it('preserves the remainder across same-turn selection callbacks and repeated Option-Right', () => {
    let state = readyController();
    let currentText = manuscript;

    const firstView = completionControllerView(state, contextKey, family);
    const first = authorizeCompletionInsertion(state, {
      contextKey,
      family: firstView.activeFamily,
      eligible: firstView.selected,
      candidateId: firstView.selected!.candidateId,
      presentationKey: firstView.selected!.presentationKey,
      text: ' one ',
      action: 'option_word',
      manuscriptText: currentText,
      promotionReady: true
    });
    expect(first.authorized).toBe(true);
    state = first.state;

    // Both editor surfaces synchronously report their new caret before Svelte
    // can update any reactive projection of the controller. The controller is
    // the only same-turn authority and must still identify this mutation.
    expect(state.pendingText).toBe('Hello one ');
    currentText = state.pendingText!;
    const firstProjection = observeTextMutation(state, currentText, manuscript, false);
    expect(firstProjection.completionOwned).toBe(true);
    state = firstProjection.state;
    const remainder = completionControllerView(state, contextKey, family);
    expect(remainder.selected).toMatchObject({ text: 'two', targetByte: 10 });

    const second = authorizeCompletionInsertion(state, {
      contextKey,
      family: remainder.activeFamily,
      eligible: remainder.selected,
      candidateId: remainder.selected!.candidateId,
      presentationKey: remainder.selected!.presentationKey,
      text: 'two',
      action: 'option_word',
      manuscriptText: currentText,
      promotionReady: true
    });
    expect(second.authorized).toBe(true);
    expect(second.state.pendingText).toBe('Hello one two');
    expect(second.state.session?.acceptedChunks).toEqual([' one ', 'two']);
  });

  it('retains hidden rollback authority after the full remainder is accepted', () => {
    const inserted = insert('fan_return');
    expect(inserted.authorized).toBe(true);
    const accepted = `${manuscript}${family[0].text}`;
    const state = observeTextMutation(inserted.state, accepted, manuscript, false).state;
    const view = completionControllerView(state, contextKey, []);
    expect(view.activeFamily).toEqual([]);
    expect(view.alternatives).toEqual([]);
    expect(view.selected).toMatchObject({
      candidateId: 'candidate-a', text: '', targetByte: 13
    });
    expect(view.unconsumeText).toBe(family[0].text);
    const reversed = authorizeCompletionUnconsume(state, {
      eligible: view.selected, candidateId: view.selected!.candidateId,
      presentationKey: view.selected!.presentationKey, text: view.unconsumeText,
      manuscriptText: accepted
    });
    expect(reversed.authorized).toBe(true);
    expect(reversed.state.pendingText).toBe(manuscript);
    expect(completionControllerView(reversed.state, contextKey, []).selected).toBeNull();
    expect(completionControllerView(state, 'another-context', []).selected).toBeNull();
    const restored = observeTextMutation(reversed.state, manuscript, accepted, false).state;
    expect(completionControllerView(restored, contextKey, []).activeFamily).toHaveLength(4);
  });

  it('edge-triggers exhaustion while preserving immediate rollback authority', () => {
    const inserted = insert('inline_tab');
    const projected = observeTextMutation(
      inserted.state,
      `Hello${family[0].text}`,
      manuscript,
      false
    );
    const key = `${contextKey}:run-a:${family[0].text.length}`;
    const first = completionExhausted(projected.state, key, 9, 1_800);
    expect(first.state.session?.authorityFrozen).toBe(true);
    expect(first.effects).toEqual([{
      kind: 'schedule_generation',
      editVersion: 9,
      delayMs: 1_800,
      trigger: 'candidate_exhausted'
    }]);
    expect(completionExhausted(first.state, key, 9, 1_800).effects).toEqual([]);

    const reset = completionExhausted(first.state, '', 9, 1_800);
    expect(completionExhausted(reset.state, key, 9, 1_800).effects).toHaveLength(1);
  });

  it('owns schedule invalidation and emits cancellation as an explicit effect', () => {
    let state = armCompletionScheduleIntent(
      readyController(),
      contextKey,
      4,
      'document_edit',
      5
    );
    state = setCompletionSchedule(state, { kind: 'edit_pause', editVersion: 4 });

    const invalidated = invalidateCompletionNavigation(state, 'visual', true, 4);
    expect(invalidated.state).toMatchObject({
      session: null,
      pendingText: null,
      generationIntent: null,
      scheduled: null,
      navigationPending: true,
      intentEpoch: 1
    });
    expect(invalidated.effects).toEqual([{ kind: 'cancel_active_branches' }]);

    const settled = settleCompletionNavigation(invalidated.state, true);
    expect(settled.scheduleFresh).toBe(true);
    expect(settled.state.navigationPending).toBe(false);
  });

  it('does not invalidate a cached family for a no-op editor echo', () => {
    const state = readyController();
    const observed = observeTextMutation(state, manuscript, manuscript, false);
    expect(observed.state.session).toBe(state.session);
    expect(observed.state.intentEpoch).toBe(0);
    expect(observed.cancelActiveBranches).toBe(false);
  });

  it('derives a stable Shuttle timer identity from candidate and accepted offset', () => {
    const state = readyController();
    const view = completionControllerView(state, contextKey, family);
    expect(shuttleScheduleKey(true, true, view.selected, 0, 12, 'visual'))
      .toBe('candidate-a:0:12:visual');
    expect(shuttleScheduleKey(true, false, view.selected, 0, 12, 'visual')).toBe('');
    expect(shuttleScheduleKey(false, true, view.selected, 0, 12, 'visual')).toBe('');
  });
});
