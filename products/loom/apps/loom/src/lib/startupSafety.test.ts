import { describe, expect, it, vi } from 'vitest';
import {
  acquireStartupProject,
  attachWorkspaceProjectReply,
  restoreBeforeBackgroundWork,
  runCurrentWorkspaceStep,
  shouldDiscoverModelsOnStartup,
  workspaceResumeAction
} from './startupSafety';

describe('attachWorkspaceProjectReply', () => {
  it('holds a chooser reply that succeeds after application close begins', async () => {
    let running = true;
    let resolveOpen: ((project: { id: string }) => void) | undefined;
    const opened = new Promise<{ id: string }>((resolve) => { resolveOpen = resolve; });
    const attach = vi.fn();
    const onHeld = vi.fn();
    const result = attachWorkspaceProjectReply({
      open: () => opened,
      mayAttach: () => running,
      attach,
      onHeld
    });

    running = false;
    resolveOpen?.({ id: 'chosen' });
    await expect(result).resolves.toBeNull();
    expect(attach).not.toHaveBeenCalled();
    expect(onHeld).toHaveBeenCalledOnce();
  });

  it('holds workspace restoration when a chooser rejects after close begins', async () => {
    let running = true;
    let rejectOpen: ((error: unknown) => void) | undefined;
    const opened = new Promise<{ id: string }>((_resolve, reject) => { rejectOpen = reject; });
    const onHeld = vi.fn();
    const failure = new Error('chooser cancelled');
    const result = attachWorkspaceProjectReply({
      open: () => opened,
      mayAttach: () => running,
      attach: vi.fn(),
      onHeld
    });

    running = false;
    rejectOpen?.(failure);
    await expect(result).rejects.toBe(failure);
    expect(onHeld).toHaveBeenCalledOnce();
  });
});

describe('workspaceResumeAction', () => {
  it('reruns acquisition without reinstalling infrastructure after an interrupted startup', () => {
    expect(workspaceResumeAction(true, true, true)).toBe('restore_workspace');
    expect(workspaceResumeAction(true, false, true)).toBe('start_infrastructure');
    expect(workspaceResumeAction(true, true, false)).toBe('none');
    expect(workspaceResumeAction(false, true, true)).toBe('none');
  });
});

describe('acquireStartupProject', () => {
  it('does not enqueue a default open when close begins behind pending current state', async () => {
    let running = true;
    const onHeld = vi.fn();
    let rejectCurrent: ((error: unknown) => void) | undefined;
    const current = new Promise<{ id: string }>((_resolve, reject) => {
      rejectCurrent = reject;
    });
    const openDefaultProject = vi.fn(async () => ({ id: 'default' }));
    const acquisition = acquireStartupProject({
      currentProject: () => current,
      openDefaultProject,
      mayContinue: () => running,
      projectIsAbsent: (error) => (error as { code?: string }).code === 'project_not_open',
      onHeld
    });

    running = false;
    rejectCurrent?.({ code: 'project_not_open' });
    await expect(acquisition).resolves.toBeNull();
    expect(openDefaultProject).not.toHaveBeenCalled();
    expect(onHeld).toHaveBeenCalledOnce();
  });

  it('does not attach a default project whose reply arrives after close begins', async () => {
    let running = true;
    const onHeld = vi.fn();
    let resolveDefault: ((project: { id: string }) => void) | undefined;
    const opened = new Promise<{ id: string }>((resolve) => {
      resolveDefault = resolve;
    });
    const acquisition = acquireStartupProject({
      currentProject: async () => { throw { code: 'project_not_open' }; },
      openDefaultProject: () => opened,
      mayContinue: () => running,
      projectIsAbsent: (error) => (error as { code?: string }).code === 'project_not_open',
      onHeld
    });
    await Promise.resolve();
    await Promise.resolve();
    running = false;
    resolveDefault?.({ id: 'default' });
    await expect(acquisition).resolves.toBeNull();
    expect(onHeld).toHaveBeenCalledOnce();
  });

  it('does not infer absence or open a default project after a current-project failure', async () => {
    const failure = { code: 'corrupt_project_session', message: 'invalid state' };
    const openDefaultProject = vi.fn(async () => ({ id: 'default' }));
    await expect(acquireStartupProject({
      currentProject: async () => { throw failure; },
      openDefaultProject,
      mayContinue: () => true,
      projectIsAbsent: (error) => (error as { code?: string }).code === 'project_not_open'
    })).rejects.toBe(failure);
    expect(openDefaultProject).not.toHaveBeenCalled();
  });
});

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

  it('marks startup held when close interrupts between restore and presentation', async () => {
    let running = true;
    const present = vi.fn(async () => {});
    const onInterrupted = vi.fn();
    await restoreBeforeBackgroundWork({
      restore: async () => {
        running = false;
        return { sessionId: 'session-a' };
      },
      present,
      isCurrent: () => running,
      background: async () => {},
      onInterrupted
    });

    expect(present).not.toHaveBeenCalled();
    expect(onInterrupted).toHaveBeenCalledOnce();
  });

  it('marks startup held when close interrupts presentation before background work', async () => {
    let running = true;
    const background = vi.fn(async () => {});
    const onInterrupted = vi.fn();
    await restoreBeforeBackgroundWork({
      restore: async () => ({ sessionId: 'session-a' }),
      present: async () => { running = false; },
      isCurrent: () => running,
      background,
      onInterrupted
    });

    expect(background).not.toHaveBeenCalled();
    expect(onInterrupted).toHaveBeenCalledOnce();
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
