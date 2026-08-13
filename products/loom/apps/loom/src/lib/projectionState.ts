import type { VisibleProjectionState } from './types';

export type DocumentProjectionDecision = 'applied' | 'reconcile' | 'retry' | 'missing';

/** A semantic receipt is not a completed save until its visible projection applied. */
export function documentProjectionDecision(
  projection: VisibleProjectionState | null
): DocumentProjectionDecision {
  if (!projection) return 'missing';
  switch (projection.status) {
    case 'applied':
      return 'applied';
    case 'pending_conflict':
      return 'reconcile';
    case 'pending_retry':
      return 'retry';
  }
}
