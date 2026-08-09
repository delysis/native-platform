import { describe, expect, it, vi } from 'vitest';
import {
  restoreBeforeBackgroundWork,
  runCurrentWorkspaceStep,
  shouldDiscoverModelsOnStartup
} from './startupSafety';

describe('shouldDiscoverModelsOnStartup', () => {
  it('keeps the local model library cold until a project opts into suggestions', () => {
    expect(shouldDiscoverModelsOnStartup(false)).toBe(false);
    expect(shouldDiscoverModelsOnStartup(true)).toBe(true);
  });
});

describe('restoreBeforeBackgroundWork', () => {
  it('presents the restored document before model discovery begins', async () => {
    const order: string[] = [];
    await restoreBeforeBackgroundWork({
      restore: async () => {
        order.push('restore');
        return { sessionId: 'session-a' };
      },
      present: async () => {
        order.push('present');
      },
      isCurrent: () => true,
      background: async () => {
        order.push('discover');
      }
    });

    expect(order).toEqual(['restore', 'present', 'discover']);
  });

  it('does not start discovery when teardown follows presentation', async () => {
    let mounted = true;
    const discover = vi.fn(async () => {});
    await restoreBeforeBackgroundWork({
      restore: async () => ({ sessionId: 'session-a' }),
      present: async () => {
        mounted = false;
      },
      isCurrent: () => mounted,
      background: discover
    });

    expect(discover).not.toHaveBeenCalled();
  });

  it('ignores a restore reply for a superseded workspace', async () => {
    const discover = vi.fn(async () => {});
    await restoreBeforeBackgroundWork({
      restore: async () => ({ sessionId: 'old-session' }),
      present: async () => {},
      isCurrent: ({ sessionId }) => sessionId === 'current-session',
      background: discover
    });

    expect(discover).not.toHaveBeenCalled();
  });
});

describe('runCurrentWorkspaceStep', () => {
  it('rejects a policy reply after the project closes while awaiting it', async () => {
    let currentSession: string | null = 'session-a';
    let resolvePolicy: (() => void) | undefined;
    const pendingPolicy = new Promise<void>((resolve) => {
      resolvePolicy = resolve;
    });
    const step = runCurrentWorkspaceStep({
      capture: { sessionId: 'session-a' },
      isCurrent: ({ sessionId }) => currentSession === sessionId,
      run: () => pendingPolicy
    });

    currentSession = null;
    resolvePolicy?.();
    await expect(step).resolves.toEqual({ status: 'stale' });
  });

  it('rejects a recovery reply after another project replaces the capture', async () => {
    let currentSession = 'session-a';
    let resolveRecovery: ((value: { recovered: number }) => void) | undefined;
    const pendingRecovery = new Promise<{ recovered: number }>((resolve) => {
      resolveRecovery = resolve;
    });
    const step = runCurrentWorkspaceStep({
      capture: { sessionId: 'session-a' },
      isCurrent: ({ sessionId }) => currentSession === sessionId,
      run: () => pendingRecovery
    });

    currentSession = 'session-b';
    resolveRecovery?.({ recovered: 1 });
    await expect(step).resolves.toEqual({ status: 'stale' });
  });
});
