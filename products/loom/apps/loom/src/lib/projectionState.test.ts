import { describe, expect, it } from 'vitest';
import { documentProjectionDecision } from './projectionState';

describe('documentProjectionDecision', () => {
  it('marks only an applied projection as a completed visible save', () => {
    expect(documentProjectionDecision({ status: 'applied' })).toBe('applied');
    expect(documentProjectionDecision(null)).toBe('missing');
  });

  it('routes a committed conflict to reconciliation', () => {
    expect(documentProjectionDecision({
      status: 'pending_conflict',
      outbox_id: 7,
      relative_path: 'manuscript/001.md'
    })).toBe('reconcile');
  });

  it('retains the exact command for a retryable projection failure', () => {
    expect(documentProjectionDecision({
      status: 'pending_retry',
      outbox_id: 8,
      relative_path: 'manuscript/001.md',
      error: 'temporary replacement failure'
    })).toBe('retry');
  });
});
