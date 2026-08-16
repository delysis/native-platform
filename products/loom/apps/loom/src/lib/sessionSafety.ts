import type { LoomFailure } from './types';

const DEFINITE_CONTENTION_CODES = new Set([
  'project_busy',
  'model_registry_busy',
  'model_lifecycle_busy'
]);

/** A native try-lock refusal happens before the requested operation starts. */
export function failureIsDefiniteContention(failure: LoomFailure): boolean {
  return failure.retryable === true && DEFINITE_CONTENTION_CODES.has(failure.code);
}

/**
 * A close command may have reached the Rust host even when its reply was lost.
 * Keep the editor locked so the caller can retry the same command identity.
 */
export function resultMayHaveCommitted(failure: LoomFailure): boolean {
  return !failureIsDefiniteContention(failure) && (
    failure.retryable === true ||
    failure.code === 'command_transport_failed' ||
    failure.code === 'command_failed'
  );
}

/** Retain an idempotent command payload only when its commit status is unknown. */
export function captureForIdempotentRetry<T>(capture: T, failure: LoomFailure): T | null {
  return resultMayHaveCommitted(failure) ? capture : null;
}

export const closeResultMayHaveCommitted = resultMayHaveCommitted;
