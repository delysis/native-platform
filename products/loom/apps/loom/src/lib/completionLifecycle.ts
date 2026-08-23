export type CompletionLifecycleInactiveReason =
  | 'desktop_unavailable'
  | 'automation_disabled'
  | 'project_unavailable'
  | 'document_unavailable'
  | 'hybrid_document'
  | 'intent_unarmed'
  | 'caret_at_start';

export type CompletionLifecycleWaitReason =
  | 'model_unavailable'
  | 'model_transition'
  | 'composition_active'
  | 'editor_projection_pending'
  | 'source_projection_pending'
  | 'save_pending'
  | 'weave_pending'
  | 'promotion_pending'
  | 'workspace_transition'
  | 'editor_readonly'
  | 'revision_unavailable'
  | 'visible_blob_stale'
  | 'caret_unmapped'
  | 'branches_active'
  | 'recovery_pending';

export type CompletionLifecycle =
  | { phase: 'inactive'; reason: CompletionLifecycleInactiveReason }
  | { phase: 'waiting'; reason: CompletionLifecycleWaitReason }
  | { phase: 'ready'; reason: null };

export interface CompletionLifecycleInput {
  desktop: boolean;
  automationEnabled: boolean;
  projectAvailable: boolean;
  documentAvailable: boolean;
  hybridDocument: boolean;
  intentArmed: boolean;
  modelAvailable: boolean;
  modelTransitioning: boolean;
  compositionActive: boolean;
  visualMutationPending: boolean;
  sourceDirty: boolean;
  savePending: boolean;
  weavePending: boolean;
  promotionPending: boolean;
  workspaceIdle: boolean;
  editorReadonly: boolean;
  recoveryPending: boolean;
  saveSettled: boolean;
  revisionAvailable: boolean;
  visibleBlobCurrent: boolean;
  caretExact: boolean;
  caretAtStart: boolean;
  activeBranchCount: number;
}

export function automaticCompletionLifecycle(
  input: CompletionLifecycleInput
): CompletionLifecycle {
  if (!input.desktop) return { phase: 'inactive', reason: 'desktop_unavailable' };
  if (!input.automationEnabled) return { phase: 'inactive', reason: 'automation_disabled' };
  if (!input.projectAvailable) return { phase: 'inactive', reason: 'project_unavailable' };
  if (!input.documentAvailable) return { phase: 'inactive', reason: 'document_unavailable' };
  if (input.hybridDocument) return { phase: 'inactive', reason: 'hybrid_document' };
  if (!input.intentArmed) return { phase: 'inactive', reason: 'intent_unarmed' };
  if (input.caretAtStart) return { phase: 'inactive', reason: 'caret_at_start' };

  if (!input.modelAvailable) return { phase: 'waiting', reason: 'model_unavailable' };
  if (input.modelTransitioning) return { phase: 'waiting', reason: 'model_transition' };
  if (input.compositionActive) return { phase: 'waiting', reason: 'composition_active' };
  if (input.visualMutationPending) return { phase: 'waiting', reason: 'editor_projection_pending' };
  if (input.sourceDirty) return { phase: 'waiting', reason: 'source_projection_pending' };
  if (input.savePending || !input.saveSettled) return { phase: 'waiting', reason: 'save_pending' };
  if (input.weavePending) return { phase: 'waiting', reason: 'weave_pending' };
  if (input.promotionPending) return { phase: 'waiting', reason: 'promotion_pending' };
  if (!input.workspaceIdle) return { phase: 'waiting', reason: 'workspace_transition' };
  if (input.editorReadonly) return { phase: 'waiting', reason: 'editor_readonly' };
  if (input.recoveryPending) return { phase: 'waiting', reason: 'recovery_pending' };
  if (!input.revisionAvailable) return { phase: 'waiting', reason: 'revision_unavailable' };
  if (!input.visibleBlobCurrent) return { phase: 'waiting', reason: 'visible_blob_stale' };
  if (!input.caretExact) return { phase: 'waiting', reason: 'caret_unmapped' };
  if (input.activeBranchCount > 0) return { phase: 'waiting', reason: 'branches_active' };

  return { phase: 'ready', reason: null };
}

export function completionLifecycleDescription(lifecycle: CompletionLifecycle): string {
  if (lifecycle.phase === 'ready') return 'Autocomplete ready';
  switch (lifecycle.reason) {
    case 'model_unavailable':
    case 'model_transition':
      return 'Autocomplete is preparing the local model';
    case 'editor_projection_pending':
    case 'source_projection_pending':
      return 'Autocomplete is waiting for the editor';
    case 'save_pending':
    case 'visible_blob_stale':
      return 'Autocomplete is waiting for autosave';
    case 'caret_unmapped':
      return 'Autocomplete is waiting for an exact caret position';
    case 'branches_active':
    case 'weave_pending':
      return 'Autocomplete is generating a four-choice batch';
    case 'automation_disabled':
      return 'Autocomplete off';
    default:
      return lifecycle.phase === 'inactive'
        ? 'Autocomplete is inactive'
        : 'Autocomplete is waiting for the document';
  }
}

export function retainsScheduledCompletion(lifecycle: CompletionLifecycle): boolean {
  return lifecycle.phase !== 'inactive';
}
