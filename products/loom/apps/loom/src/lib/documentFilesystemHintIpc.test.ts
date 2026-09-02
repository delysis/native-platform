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

import { listenForDocumentFilesystemHints } from './ipc';

const priorWindow = globalThis.window;

afterEach(() => {
  mocks.listen.mockReset();
  mocks.invoke.mockReset();
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: priorWindow
  });
});

describe('document filesystem hint IPC', () => {
  it('binds the exact event, forwards only its typed payload, and returns unlisten authority', async () => {
    const unlisten = vi.fn();
    let nativeHandler: ((event: {
      payload: { project_id: string; session_id: string };
    }) => void) | undefined;
    mocks.listen.mockImplementation(async (_eventName, handler) => {
      nativeHandler = handler;
      return unlisten;
    });
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { __TAURI_INTERNALS__: {} }
    });
    const handler = vi.fn();

    const result = await listenForDocumentFilesystemHints(handler);

    expect(mocks.listen).toHaveBeenCalledWith(
      'loom://document-filesystem-hint',
      expect.any(Function)
    );
    nativeHandler?.({
      payload: { project_id: 'project-1', session_id: 'session-1' }
    });
    expect(handler).toHaveBeenCalledWith({
      project_id: 'project-1',
      session_id: 'session-1'
    });
    expect(result).toBe(unlisten);
  });

  it('refuses listener installation outside the desktop runtime', async () => {
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {}
    });

    await expect(listenForDocumentFilesystemHints(vi.fn())).rejects.toMatchObject({
      code: 'desktop_runtime_required'
    });
    expect(mocks.listen).not.toHaveBeenCalled();
  });
});
