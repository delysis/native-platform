import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  listen: vi.fn(),
  invoke: vi.fn()
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: mocks.listen
}));

vi.mock('@tauri-apps/api/core', () => ({
  invoke: mocks.invoke
}));

import {
  abortApplicationClose,
  applicationClosePending,
  listenForApplicationCloseRequests
} from './ipc';

const priorWindow = globalThis.window;

afterEach(() => {
  mocks.listen.mockReset();
  mocks.invoke.mockReset();
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: priorWindow
  });
});

describe('application close request IPC', () => {
  it('binds the exact native exit-request event and returns its unlisten authority', async () => {
    const unlisten = vi.fn();
    mocks.listen.mockResolvedValue(unlisten);
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { __TAURI_INTERNALS__: {} }
    });
    const handler = vi.fn();

    const result = await listenForApplicationCloseRequests(handler);

    expect(mocks.listen).toHaveBeenCalledWith(
      'loom://application-close-requested',
      handler
    );
    expect(result).toBe(unlisten);
  });

  it('refuses listener installation outside the desktop runtime', async () => {
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {}
    });

    await expect(listenForApplicationCloseRequests(vi.fn())).rejects.toMatchObject({
      code: 'desktop_runtime_required'
    });
    expect(mocks.listen).not.toHaveBeenCalled();
  });

  it('invokes the exact native close-abort command', async () => {
    mocks.invoke.mockResolvedValue(undefined);
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { __TAURI_INTERNALS__: {} }
    });

    await expect(abortApplicationClose()).resolves.toBeUndefined();

    expect(mocks.invoke).toHaveBeenCalledWith(
      'plugin:loom|application_close_abort',
      {}
    );
  });

  it('queries the exact native pending-close command after listener installation', async () => {
    mocks.invoke.mockResolvedValue(true);
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { __TAURI_INTERNALS__: {} }
    });

    await expect(applicationClosePending()).resolves.toBe(true);

    expect(mocks.invoke).toHaveBeenCalledWith(
      'plugin:loom|application_close_pending',
      {}
    );
  });
});
