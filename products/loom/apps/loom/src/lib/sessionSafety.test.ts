import { describe, expect, it } from 'vitest';
import {
  captureForIdempotentRetry,
  closeResultMayHaveCommitted,
  resultMayHaveCommitted
} from './sessionSafety';

describe('closeResultMayHaveCommitted', () => {
  it('keeps the session locked for lost or untyped replies', () => {
    expect(closeResultMayHaveCommitted({
      code: 'command_transport_failed',
      message: 'connection closed'
    })).toBe(true);
    expect(closeResultMayHaveCommitted({
      code: 'command_failed',
      message: 'unknown host error'
    })).toBe(true);
  });

  it('keeps the session locked for explicitly retryable host failures', () => {
    expect(resultMayHaveCommitted({
      code: 'host_busy',
      message: 'retry',
      retryable: true
    })).toBe(true);
  });

  it('unlocks only for a typed refusal known not to have committed', () => {
    expect(closeResultMayHaveCommitted({
      code: 'project_session_mismatch',
      message: 'wrong session',
      retryable: false
    })).toBe(false);
  });

  it('never pins an old command capture after a deterministic refusal', () => {
    const capture = { commandId: 'command-1', text: 'exact bytes' };
    expect(captureForIdempotentRetry(capture, {
      code: 'source_revision_conflict',
      message: 'stale source',
      retryable: false
    })).toBeNull();
    expect(captureForIdempotentRetry(capture, {
      code: 'command_transport_failed',
      message: 'reply lost'
    })).toBe(capture);
  });
});
