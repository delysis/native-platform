import type { LoomFailure } from './types';

/**
 * A close command may have reached the Rust host even when its reply was lost.
 * Keep the editor locked so the caller can retry the same command identity.
 */
export function resultMayHaveCommitted(failure: LoomFailure): boolean {
  return failure.retryable === true ||
    failure.code === 'command_transport_failed' ||
    failure.code === 'command_failed';
}

/** Retain an idempotent command payload only when its commit status is unknown. */
export function captureForIdempotentRetry<T>(capture: T, failure: LoomFailure): T | null {
  return resultMayHaveCommitted(failure) ? capture : null;
}

export const closeResultMayHaveCommitted = resultMayHaveCommitted;
