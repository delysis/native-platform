import { describe, expect, it, vi } from 'vitest';
import { DetachedProjectCloseCoordinator } from './detachedProjectClose';
import type { LoomFailure } from './types';

function normalizeFailure(error: unknown): LoomFailure {
  if (error && typeof error === 'object' && 'code' in error) {
    const value = error as Partial<LoomFailure>;
    return {
      code: String(value.code),
      message: value.message ?? String(value.code),
      retryable: value.retryable ?? false
    };
  }
  return { code: 'command_failed', message: String(error), retryable: true };
}

function operations(overrides: Partial<ConstructorParameters<typeof DetachedProjectCloseCoordinator>[0]> = {}) {
  return {
    currentProject: vi.fn(async () => ({ project_id: 'project', session_id: 'session' })),
    disableAutomation: vi.fn(async () => undefined),
    closeProject: vi.fn(async (projectId: string, sessionId: string, commandId: string) => ({
      project_id: projectId,
      session_id: sessionId,
      command_id: commandId
    })),
    newCommandId: vi.fn(() => 'command'),
    normalizeFailure,
    ...overrides
  };
}

describe('detached native project close', () => {
  it('closes the exact native session that appeared before renderer attachment', async () => {
    const ops = operations();
    const coordinator = new DetachedProjectCloseCoordinator(ops);
    await expect(coordinator.close()).resolves.toEqual({ status: 'closed' });
    expect(ops.disableAutomation).toHaveBeenCalledWith('project', 'session');
    expect(ops.closeProject).toHaveBeenCalledWith('project', 'session', 'command');
  });

  it('treats definitive native absence before or during close as closed', async () => {
    const absent = { code: 'project_not_open', message: 'absent', retryable: false };
    await expect(new DetachedProjectCloseCoordinator(operations({
      currentProject: vi.fn(async () => { throw absent; })
    })).close()).resolves.toEqual({ status: 'closed' });

    await expect(new DetachedProjectCloseCoordinator(operations({
      closeProject: vi.fn(async () => { throw absent; })
    })).close()).resolves.toEqual({ status: 'closed' });
  });

  it('re-reads native presence after an uncertain detached-session reply', async () => {
    const currentProject = vi.fn()
      .mockRejectedValueOnce(new Error('reply lost'))
      .mockResolvedValueOnce({ project_id: 'project', session_id: 'session' });
    const ops = operations({ currentProject });
    const coordinator = new DetachedProjectCloseCoordinator(ops);

    await expect(coordinator.close()).resolves.toEqual({ status: 'quiesced' });
    await expect(coordinator.close()).resolves.toEqual({ status: 'closed' });
    expect(currentProject).toHaveBeenCalledTimes(2);
    expect(ops.closeProject).toHaveBeenCalledWith('project', 'session', 'command');
  });

  it('retains one close identity across bounded drain and lost-reply retries', async () => {
    const closeProject = vi.fn()
      .mockRejectedValueOnce({
        code: 'generation_cancellation_in_progress',
        message: 'draining',
        retryable: true
      })
      .mockRejectedValueOnce(new Error('reply lost'))
      .mockResolvedValueOnce({
        project_id: 'project',
        session_id: 'session',
        command_id: 'command'
      });
    const ops = operations({ closeProject });
    const coordinator = new DetachedProjectCloseCoordinator(ops);

    await expect(coordinator.close()).resolves.toEqual({ status: 'quiesced' });
    await expect(coordinator.close()).resolves.toEqual({ status: 'quiesced' });
    await expect(coordinator.close()).resolves.toEqual({ status: 'closed' });
    expect(ops.currentProject).toHaveBeenCalledTimes(1);
    expect(ops.disableAutomation).toHaveBeenCalledTimes(1);
    expect(closeProject).toHaveBeenCalledTimes(3);
    expect(closeProject.mock.calls.every((call) => call[2] === 'command')).toBe(true);
  });

  it('retries an uncertain automation reply without changing close identity', async () => {
    const disableAutomation = vi.fn()
      .mockRejectedValueOnce(new Error('reply lost'))
      .mockResolvedValueOnce(undefined);
    const ops = operations({ disableAutomation });
    const coordinator = new DetachedProjectCloseCoordinator(ops);

    await expect(coordinator.close()).resolves.toEqual({ status: 'quiesced' });
    await expect(coordinator.close()).resolves.toEqual({ status: 'closed' });
    expect(ops.currentProject).toHaveBeenCalledTimes(1);
    expect(ops.newCommandId).toHaveBeenCalledTimes(1);
    expect(disableAutomation).toHaveBeenCalledTimes(2);
  });

  it('rejects a receipt for a different native session and reset drops its capture', async () => {
    const ops = operations({
      closeProject: vi.fn(async () => ({
        project_id: 'other',
        session_id: 'session',
        command_id: 'command'
      }))
    });
    const coordinator = new DetachedProjectCloseCoordinator(ops);
    await expect(coordinator.close()).rejects.toThrow('another project session');
    coordinator.reset();
    await expect(coordinator.close()).rejects.toThrow('another project session');
    expect(ops.currentProject).toHaveBeenCalledTimes(2);
  });
});
