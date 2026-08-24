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
  reconcileCompletionController,
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
