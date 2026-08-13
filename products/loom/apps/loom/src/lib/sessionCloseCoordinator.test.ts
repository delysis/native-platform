import { describe, expect, it, vi } from 'vitest';
import { drainGenerationsAndClose } from './sessionCloseCoordinator';
import type { LoomFailure } from './types';

function failure(code: string, retryable = false): LoomFailure {
  return { code, message: code, retryable };
}

function operations(overrides: Partial<Parameters<typeof drainGenerationsAndClose<string>>[0]> = {}) {
  return {
    disableAutomation: vi.fn(async () => {}),
    cancelKnownBranches: vi.fn(async () => {}),
    closeProject: vi.fn(async () => 'receipt'),
    normalizeFailure: (error: unknown) => error as LoomFailure,
    closeResultMayHaveCommitted: (value: LoomFailure) => value.code === 'command_transport_failed',
    wait: vi.fn(async () => {}),
    isCurrent: vi.fn(() => true),
    ...overrides
  };
}

describe('drainGenerationsAndClose', () => {
  it('disables automation and requests branch cancellation before close', async () => {
    const order: string[] = [];
    const ops = operations({
      disableAutomation: vi.fn(async () => { order.push('disable'); }),
      cancelKnownBranches: vi.fn(async () => { order.push('cancel'); }),
      closeProject: vi.fn(async () => {
        order.push('close');
        return 'receipt';
      })
    });

    await expect(drainGenerationsAndClose(ops)).resolves.toEqual({
      status: 'closed',
      value: 'receipt'
    });
    expect(order).toEqual(['disable', 'cancel', 'close']);
  });

  it('reissues cancellation and retries generation-active close refusals', async () => {
    let attempts = 0;
    const ops = operations({
      closeProject: vi.fn(async () => {
        attempts += 1;
        if (attempts < 3) throw failure('generation_active', true);
        return 'receipt';
      })
    });

    await expect(drainGenerationsAndClose(ops)).resolves.toMatchObject({ status: 'closed' });
    expect(ops.disableAutomation).toHaveBeenCalledTimes(3);
    expect(ops.cancelKnownBranches).toHaveBeenCalledTimes(3);
    expect(ops.closeProject).toHaveBeenCalledTimes(3);
  });

  it('stops after a bounded wait without claiming the project closed', async () => {
    const ops = operations({
      closeProject: vi.fn(async () => {
        throw failure('generation_active', true);
      })
    });

    await expect(drainGenerationsAndClose(ops)).resolves.toMatchObject({
      status: 'waiting',
      stage: 'project_close'
    });
    expect(ops.closeProject).toHaveBeenCalledTimes(7);
  });

  it('yields after the native three-second cancellation wait without stacking retries', async () => {
    const ops = operations({
      closeProject: vi.fn(async () => {
        throw failure('generation_cancellation_in_progress', true);
      })
    });

    await expect(drainGenerationsAndClose(ops)).resolves.toMatchObject({
      status: 'waiting',
      stage: 'project_close',
      failure: { code: 'generation_cancellation_in_progress' }
    });
    expect(ops.closeProject).toHaveBeenCalledTimes(1);
    expect(ops.wait).not.toHaveBeenCalled();
  });

  it('preserves uncertainty only for a close result that may have committed', async () => {
    const ops = operations({
      closeProject: vi.fn(async () => {
        throw failure('command_transport_failed', true);
      })
    });

    await expect(drainGenerationsAndClose(ops)).resolves.toEqual({
      status: 'uncertain',
      failure: failure('command_transport_failed', true)
    });
    expect(ops.closeProject).toHaveBeenCalledTimes(1);
  });

  it('never closes a superseded session', async () => {
    const ops = operations({
      isCurrent: vi.fn(() => false)
    });

    await expect(drainGenerationsAndClose(ops)).resolves.toEqual({ status: 'stale' });
    expect(ops.disableAutomation).not.toHaveBeenCalled();
    expect(ops.closeProject).not.toHaveBeenCalled();
  });
});
