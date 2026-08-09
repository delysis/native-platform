import type { LoomFailure } from './types';

const RETRY_DELAYS_MS = [40, 80, 160, 320, 500, 750] as const;

export type SessionCloseOutcome<T> =
  | { status: 'closed'; value: T }
  | { status: 'waiting'; stage: 'disable_automation' | 'project_close'; failure?: LoomFailure }
  | { status: 'refused'; stage: 'disable_automation' | 'project_close'; failure: LoomFailure }
  | { status: 'uncertain'; failure: LoomFailure }
  | { status: 'stale' };

interface SessionCloseOperations<T> {
  disableAutomation: () => Promise<void>;
  cancelKnownBranches: () => Promise<void>;
  closeProject: () => Promise<T>;
  normalizeFailure: (error: unknown) => LoomFailure;
  closeResultMayHaveCommitted: (failure: LoomFailure) => boolean;
  wait: (delayMs: number) => Promise<void>;
  isCurrent: () => boolean;
}

function isRetryableAutomationFailure(failure: LoomFailure): boolean {
  return failure.retryable === true ||
    failure.code === 'command_transport_failed' ||
    failure.code === 'command_failed' ||
    failure.code === 'project_busy';
}

function isKnownCloseWait(failure: LoomFailure): boolean {
  return failure.code === 'generation_active' ||
    failure.code === 'generation_cancellation_in_progress' ||
    failure.code === 'project_busy';
}

async function waitWhileCurrent<T>(
  operations: SessionCloseOperations<T>,
  delayMs: number
): Promise<boolean> {
  await operations.wait(delayMs);
  return operations.isCurrent();
}

/**
 * Close one native project session without racing automatic generation.
 *
 * Disabling automation is both an admission barrier and a session-wide
 * cancellation request in the Rust host. Native reservation retains that
 * cancellation even while a generation handle is still attaching. Native
 * close remains the authoritative bounded wait for workers to persist
 * terminal state and unregister.
 */
export async function drainGenerationsAndClose<T>(
  operations: SessionCloseOperations<T>
): Promise<SessionCloseOutcome<T>> {
  for (let attempt = 0; ; attempt += 1) {
    if (!operations.isCurrent()) return { status: 'stale' };
    try {
      await operations.disableAutomation();
      break;
    } catch (error) {
      const failure = operations.normalizeFailure(error);
      const delay = RETRY_DELAYS_MS[attempt];
      if (!isRetryableAutomationFailure(failure)) {
        return { status: 'refused', stage: 'disable_automation', failure };
      }
      if (delay === undefined) {
        return {
          status: 'waiting',
          stage: 'disable_automation',
          failure
        };
      }
      if (!(await waitWhileCurrent(operations, delay))) return { status: 'stale' };
    }
  }

  // Per-run cancellation improves immediate UI state. The preceding policy
  // command is the authoritative session-wide cancellation request. Native
  // route reservation also retains cancellation while startup is attaching
  // its handle, so neither `weaveStarting` nor a stale local branch page must
  // make close unsafe.
  try {
    await operations.cancelKnownBranches();
  } catch {
    // Native close below determines whether any generation remains active.
  }

  for (let attempt = 0; ; attempt += 1) {
    if (!operations.isCurrent()) return { status: 'stale' };
    try {
      return { status: 'closed', value: await operations.closeProject() };
    } catch (error) {
      const failure = operations.normalizeFailure(error);
      if (!isKnownCloseWait(failure)) {
        return operations.closeResultMayHaveCommitted(failure)
          ? { status: 'uncertain', failure }
          : { status: 'refused', stage: 'project_close', failure };
      }

      // The native close command already disabled admission, requested
      // session-wide cancellation, and waited its full three-second evidence
      // preservation bound. It definitively did not close. Yield a recoverable
      // retry to the user instead of stacking more three-second native waits.
      if (failure.code === 'generation_cancellation_in_progress') {
        return { status: 'waiting', stage: 'project_close', failure };
      }

      const delay = RETRY_DELAYS_MS[attempt];
      if (delay === undefined) {
        return { status: 'waiting', stage: 'project_close', failure };
      }

      // Repeating `false` is idempotent and reissues session-wide
      // cancellation for a family that registered after the first request.
      try {
        await operations.disableAutomation();
      } catch {
        // The close refusal already proves the session is open. Retry after a
        // bounded delay; the next close remains authoritative.
      }
      try {
        await operations.cancelKnownBranches();
      } catch {
        // See the authoritative close check above.
      }
      if (!(await waitWhileCurrent(operations, delay))) return { status: 'stale' };
    }
  }
}
