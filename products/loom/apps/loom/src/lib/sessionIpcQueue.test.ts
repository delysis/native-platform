import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  invoke: vi.fn()
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn()
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: mocks.invoke
}));

import {
  applicationClosePending,
  closeProject,
  currentProjectSession,
  exportDocumentCopy,
  getBranch,
  getBranchPage,
  listModels
} from './ipc';

const priorWindow = globalThis.window;

function installDesktopRuntime(): void {
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { __TAURI_INTERNALS__: {} }
  });
}

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((settle, fail) => {
    resolve = settle;
    reject = fail;
  });
  return { promise, resolve, reject };
}

afterEach(() => {
  vi.useRealTimers();
  mocks.invoke.mockReset();
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: priorWindow
  });
});

describe('session IPC admission', () => {
  it('starts session-bound commands in FIFO order and continues after a failure', async () => {
    installDesktopRuntime();
    const first = deferred<never>();
    const order: string[] = [];
    mocks.invoke.mockImplementation((command: string) => {
      order.push(command);
      if (command === 'plugin:loom|branch_page') return first.promise;
      return Promise.resolve(null);
    });

    const page = getBranchPage('project', 'session', 'document', null, 10);
    await vi.waitFor(() => expect(mocks.invoke).toHaveBeenCalledTimes(1));
    const branch = getBranch('project', 'session', 'document', 'run');

    await Promise.resolve();
    expect(mocks.invoke).toHaveBeenCalledTimes(1);
    first.reject(new Error('page failed'));

    await expect(page).rejects.toThrow('page failed');
    await expect(branch).resolves.toBeNull();
    expect(order).toEqual([
      'plugin:loom|branch_page',
      'plugin:loom|branch_get'
    ]);
  });

  it('waits for a detached native chooser or close transition to settle', async () => {
    installDesktopRuntime();
    vi.useFakeTimers();
    mocks.invoke
      .mockRejectedValueOnce({
        code: 'project_transition_in_progress',
        message: 'choosing',
        retryable: true
      })
      .mockResolvedValueOnce({ project_id: 'project', session_id: 'session' });

    const observed = currentProjectSession();
    await vi.runAllTimersAsync();
    await expect(observed).resolves.toMatchObject({ project_id: 'project', session_id: 'session' });
    expect(mocks.invoke).toHaveBeenCalledTimes(2);
  });

  it('never replays a failed mutating project command', async () => {
    installDesktopRuntime();
    mocks.invoke.mockRejectedValueOnce({ code: 'project_busy', retryable: true });
    await expect(getBranch('project', 'session', 'document', 'run')).rejects.toMatchObject({
      code: 'project_busy'
    });

    mocks.invoke.mockRejectedValueOnce({ code: 'project_busy', retryable: true });
    await expect(
      exportDocumentCopy('project', 'session', 'document', 'draft.md')
    ).rejects.toMatchObject({ code: 'project_busy' });

    mocks.invoke.mockRejectedValueOnce({ code: 'project_busy', retryable: true });
    await expect(closeProject('project', 'session', 'command')).rejects.toMatchObject({
      code: 'project_busy'
    });
    expect(mocks.invoke).toHaveBeenCalledTimes(3);
  });

  it('keeps model and application lifecycle calls outside the session FIFO', async () => {
    installDesktopRuntime();
    const heldPage = deferred<Record<string, unknown>>();
    mocks.invoke.mockImplementation((command: string) => {
      if (command === 'plugin:loom|branch_page') return heldPage.promise;
      if (command === 'plugin:loom|model_list') return Promise.resolve([]);
      if (command === 'plugin:loom|application_close_pending') return Promise.resolve(true);
      return Promise.reject(new Error(`unexpected command: ${command}`));
    });

    const page = getBranchPage('project', 'session', 'document', null, 10);
    await vi.waitFor(() => expect(mocks.invoke).toHaveBeenCalledTimes(1));

    await expect(listModels()).resolves.toEqual([]);
    await expect(applicationClosePending()).resolves.toBe(true);
    expect(mocks.invoke.mock.calls.map(([command]) => command)).toEqual([
      'plugin:loom|branch_page',
      'plugin:loom|model_list',
      'plugin:loom|application_close_pending'
    ]);

    heldPage.resolve({ branches: [], next_cursor: null });
    await expect(page).resolves.toEqual({ branches: [], next_cursor: null });
  });
});
