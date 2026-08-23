import { describe, expect, it } from 'vitest';
import {
  automaticCompletionLifecycle,
  completionLifecycleDescription,
  retainsScheduledCompletion,
  type CompletionLifecycleInput
} from './completionLifecycle';

const readyInput: CompletionLifecycleInput = {
  desktop: true,
  automationEnabled: true,
  projectAvailable: true,
  documentAvailable: true,
  hybridDocument: false,
  intentArmed: true,
  modelAvailable: true,
  modelTransitioning: false,
  compositionActive: false,
  visualMutationPending: false,
  sourceDirty: false,
  savePending: false,
  weavePending: false,
  promotionPending: false,
  workspaceIdle: true,
  editorReadonly: false,
  recoveryPending: false,
  saveSettled: true,
  revisionAvailable: true,
  visibleBlobCurrent: true,
  caretExact: true,
  caretAtStart: false,
  activeBranchCount: 0
};

describe('automatic completion lifecycle', () => {
  it('is ready only when the exact saved editor context can start a batch', () => {
    expect(automaticCompletionLifecycle(readyInput)).toEqual({ phase: 'ready', reason: null });
  });

  it.each([
    ['model_unavailable', { modelAvailable: false }],
    ['model_transition', { modelTransitioning: true }],
    ['editor_projection_pending', { visualMutationPending: true }],
    ['save_pending', { saveSettled: false }],
    ['caret_unmapped', { caretExact: false }],
    ['branches_active', { activeBranchCount: 4 }]
  ] as const)('retains the pending intent while %s resolves', (reason, change) => {
    const lifecycle = automaticCompletionLifecycle({ ...readyInput, ...change });
    expect(lifecycle).toEqual({ phase: 'waiting', reason });
    expect(retainsScheduledCompletion(lifecycle)).toBe(true);
  });

  it('drops an intent whose document context is no longer armed', () => {
    const lifecycle = automaticCompletionLifecycle({ ...readyInput, intentArmed: false });
    expect(lifecycle).toEqual({ phase: 'inactive', reason: 'intent_unarmed' });
    expect(retainsScheduledCompletion(lifecycle)).toBe(false);
  });

  it('describes an unmapped visual boundary without claiming readiness', () => {
    const lifecycle = automaticCompletionLifecycle({ ...readyInput, caretExact: false });
    expect(completionLifecycleDescription(lifecycle)).toBe(
      'Autocomplete is waiting for an exact caret position'
    );
  });
});
