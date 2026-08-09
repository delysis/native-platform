import { describe, expect, it, vi } from 'vitest';
import {
  ApplicationCloseCoordinator,
  applicationAllowsModelPreparation,
  applicationStartupDisposition,
  isApplicationCloseAbortFailure,
  type ApplicationCloseOperations,
  type ProjectCloseOutcome
} from './applicationCloseCoordinator';

function operations(overrides: Partial<ApplicationCloseOperations> = {}) {
  return {
    begin: vi.fn(() => true),
    closeProject: vi.fn(async (): Promise<ProjectCloseOutcome> => ({ status: 'closed' })),
    authorizeNativeClose: vi.fn(async () => {}),
    abortNativeClose: vi.fn(async () => {}),
    reset: vi.fn(() => {}),
    fail: vi.fn((_error: unknown) => {}),
    ...overrides
  } satisfies ApplicationCloseOperations;
}

describe('ApplicationCloseCoordinator', () => {
  it('permits preferred-model preparation only while the application is running', () => {
    expect(applicationAllowsModelPreparation('running')).toBe(true);
    expect(applicationAllowsModelPreparation('closing')).toBe(false);
  });

  it('continues startup only after native close was definitively abandoned', () => {
    expect(applicationStartupDisposition({ status: 'resumed' })).toBe('continue');
    expect(applicationStartupDisposition({ status: 'failed', error: new Error('refused') }))
      .toBe('continue');
    expect(applicationStartupDisposition({ status: 'exit_requested' })).toBe('hold_for_close');
    expect(applicationStartupDisposition({ status: 'quiesced' })).toBe('hold_for_close');
    expect(applicationStartupDisposition({
      status: 'lifecycle_unknown',
      error: {
        code: 'application_close_abort_failed',
        message: 'unknown',
        retryable: false,
        close_error: undefined,
        abort_error: undefined
      }
    })).toBe('hold_for_close');
  });

  it('coalesces simultaneous native and window close signals', async () => {
    let releaseProject: ((outcome: ProjectCloseOutcome) => void) | undefined;
    const ops = operations({
      closeProject: vi.fn(() => new Promise<ProjectCloseOutcome>((resolve) => {
        releaseProject = resolve;
      }))
    });
    const coordinator = new ApplicationCloseCoordinator(ops);

    const nativeExit = coordinator.request();
    const windowClose = coordinator.request();

    expect(windowClose).toBe(nativeExit);
    expect(ops.begin).toHaveBeenCalledTimes(1);
    expect(ops.closeProject).toHaveBeenCalledTimes(1);
    releaseProject?.({ status: 'closed' });
    await expect(nativeExit).resolves.toEqual({ status: 'exit_requested' });
    expect(ops.authorizeNativeClose).toHaveBeenCalledTimes(1);
  });

  it('latches successful native close authority against later duplicate events', async () => {
    const ops = operations();
    const coordinator = new ApplicationCloseCoordinator(ops);

    await expect(coordinator.request()).resolves.toEqual({ status: 'exit_requested' });
    await expect(coordinator.request()).resolves.toEqual({ status: 'exit_requested' });

    expect(ops.begin).toHaveBeenCalledTimes(1);
    expect(ops.closeProject).toHaveBeenCalledTimes(1);
    expect(ops.authorizeNativeClose).toHaveBeenCalledTimes(1);
  });

  it('aborts native quiescing before releasing a composition-blocked retry', async () => {
    let releaseAbort: (() => void) | undefined;
    const abortNativeClose = vi.fn(() => new Promise<void>((resolve) => {
      releaseAbort = resolve;
    }));
    const ops = operations({
      begin: vi.fn()
        .mockReturnValueOnce(false)
        .mockReturnValueOnce(true),
      abortNativeClose
    });
    const coordinator = new ApplicationCloseCoordinator(ops);

    const blocked = coordinator.request();

    expect(ops.closeProject).not.toHaveBeenCalled();
    expect(ops.authorizeNativeClose).not.toHaveBeenCalled();
    expect(ops.abortNativeClose).toHaveBeenCalledTimes(1);
    expect(ops.reset).not.toHaveBeenCalled();
    releaseAbort?.();
    await expect(blocked).resolves.toEqual({ status: 'resumed' });
    expect(ops.reset).toHaveBeenCalledTimes(1);

    await expect(coordinator.request()).resolves.toEqual({ status: 'exit_requested' });
    expect(ops.begin).toHaveBeenCalledTimes(2);
    expect(ops.authorizeNativeClose).toHaveBeenCalledTimes(1);
  });

  it('releases a definitive project-close refusal for an exact retry', async () => {
    const recoveryOrder: string[] = [];
    const ops = operations({
      closeProject: vi.fn()
        .mockResolvedValueOnce({ status: 'resume' })
        .mockResolvedValueOnce({ status: 'closed' }),
      abortNativeClose: vi.fn(async () => { recoveryOrder.push('abort'); }),
      reset: vi.fn(() => { recoveryOrder.push('reset'); })
    });
    const coordinator = new ApplicationCloseCoordinator(ops);

    await expect(coordinator.request()).resolves.toEqual({ status: 'resumed' });
    await expect(coordinator.request()).resolves.toEqual({ status: 'exit_requested' });

    expect(ops.begin).toHaveBeenCalledTimes(2);
    expect(ops.abortNativeClose).toHaveBeenCalledTimes(1);
    expect(ops.reset).toHaveBeenCalledTimes(1);
    expect(recoveryOrder).toEqual(['abort', 'reset']);
    expect(ops.authorizeNativeClose).toHaveBeenCalledTimes(1);
  });

  it('keeps a draining project quiesced without falsely aborting native close', async () => {
    const ops = operations({
      closeProject: vi.fn()
        .mockResolvedValueOnce({ status: 'quiesced' })
        .mockResolvedValueOnce({ status: 'closed' })
    });
    const coordinator = new ApplicationCloseCoordinator(ops);

    await expect(coordinator.request()).resolves.toEqual({ status: 'quiesced' });
    expect(ops.abortNativeClose).not.toHaveBeenCalled();
    expect(ops.reset).not.toHaveBeenCalled();
    expect(ops.authorizeNativeClose).not.toHaveBeenCalled();

    await expect(coordinator.request()).resolves.toEqual({ status: 'exit_requested' });
    expect(ops.begin).toHaveBeenCalledTimes(2);
    expect(ops.authorizeNativeClose).toHaveBeenCalledTimes(1);
  });

  it('records native refusal and permits a later retry after a successful abort', async () => {
    const refusal = new Error('native teardown is still draining');
    const recoveryOrder: string[] = [];
    const ops = operations({
      authorizeNativeClose: vi.fn()
        .mockRejectedValueOnce(refusal)
        .mockResolvedValueOnce(undefined),
      abortNativeClose: vi.fn(async () => { recoveryOrder.push('abort'); }),
      reset: vi.fn(() => { recoveryOrder.push('reset'); })
    });
    const coordinator = new ApplicationCloseCoordinator(ops);

    await expect(coordinator.request()).resolves.toEqual({ status: 'failed', error: refusal });
    expect(ops.abortNativeClose).toHaveBeenCalledTimes(1);
    expect(ops.reset).toHaveBeenCalledTimes(1);
    expect(recoveryOrder).toEqual(['abort', 'reset']);
    expect(ops.fail).toHaveBeenCalledWith(refusal);

    await expect(coordinator.request()).resolves.toEqual({ status: 'exit_requested' });
    expect(ops.authorizeNativeClose).toHaveBeenCalledTimes(2);
  });

  it('latches lifecycle-unknown when native abort fails', async () => {
    const abortError = new Error('native quiescing state did not release');
    const ops = operations({
      closeProject: vi.fn(async (): Promise<ProjectCloseOutcome> => ({ status: 'resume' })),
      abortNativeClose: vi.fn(async () => { throw abortError; })
    });
    const coordinator = new ApplicationCloseCoordinator(ops);

    const firstRequest = coordinator.request();
    const outcome = await firstRequest;

    expect(outcome.status).toBe('lifecycle_unknown');
    if (outcome.status !== 'lifecycle_unknown') {
      throw new Error('expected a lifecycle-unknown close outcome');
    }
    expect(isApplicationCloseAbortFailure(outcome.error)).toBe(true);
    expect(outcome.error).toMatchObject({
      code: 'application_close_abort_failed',
      retryable: false,
      abort_error: abortError
    });
    expect(ops.reset).not.toHaveBeenCalled();
    expect(ops.fail).toHaveBeenCalledWith(outcome.error);

    const repeatedRequest = coordinator.request();
    expect(repeatedRequest).toBe(firstRequest);
    await expect(repeatedRequest).resolves.toBe(outcome);
    expect(ops.begin).toHaveBeenCalledTimes(1);
    expect(ops.closeProject).toHaveBeenCalledTimes(1);
    expect(ops.abortNativeClose).toHaveBeenCalledTimes(1);
  });
});
