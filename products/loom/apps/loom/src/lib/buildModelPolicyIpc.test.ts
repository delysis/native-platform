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

import { getBuildModelPolicy } from './ipc';

const priorWindow = globalThis.window;
const quietPolicy = {
  name: 'writer-gemma4-base-v2',
  activation: 'quiet_default',
  canonical_sha256: '2d402d213b60ba65c4d018907e9eba67ccfbc1e97081cc0505f9713ae2dd89d2'
};

afterEach(() => {
  mocks.invoke.mockReset();
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: priorWindow
  });
});

function installDesktopRuntime(): void {
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { __TAURI_INTERNALS__: {} }
  });
}

describe('build model policy IPC', () => {
  it('invokes the read-only native command and accepts the exact V2 contract', async () => {
    installDesktopRuntime();
    mocks.invoke.mockResolvedValue(quietPolicy);

    await expect(getBuildModelPolicy()).resolves.toEqual(quietPolicy);
    expect(mocks.invoke).toHaveBeenCalledWith(
      'plugin:loom|build_model_policy_get',
      {}
    );
  });

  it('accepts the immutable V1 project-opt-in contract', async () => {
    installDesktopRuntime();
    const v1 = {
      name: 'writer-gemma4-base-v1',
      activation: 'project_opt_in',
      canonical_sha256: 'c0492fb2285ad0922f89ab7288d63ef68fd17f5133f00ea4276622a15c2dc4e6'
    };
    mocks.invoke.mockResolvedValue(v1);

    await expect(getBuildModelPolicy()).resolves.toEqual(v1);
  });

  it('accepts the immutable no-preferred-writer contract', async () => {
    installDesktopRuntime();
    const none = {
      name: 'none-v1',
      activation: 'project_opt_in',
      canonical_sha256: 'ce3bdf5e3dbcac6f7bcc164ec4cc5c78b4a7b5bef7c49b3cd52c61e123b75fe0'
    };
    mocks.invoke.mockResolvedValue(none);

    await expect(getBuildModelPolicy()).resolves.toEqual(none);
  });

  it.each([
    [
      'missing activation',
      { name: quietPolicy.name, canonical_sha256: quietPolicy.canonical_sha256 }
    ],
    ['unknown activation', { ...quietPolicy, activation: 'ambient_default' }],
    ['V1 semantic drift', { ...quietPolicy, name: 'writer-gemma4-base-v1' }],
    ['unknown policy', { ...quietPolicy, name: 'writer-future-v9' }],
    ['wrong digest', { ...quietPolicy, canonical_sha256: '0'.repeat(64) }],
    ['unknown field', { ...quietPolicy, model_path: '/builder/model.gguf' }]
  ])('fails closed on %s', async (_label, payload) => {
    installDesktopRuntime();
    mocks.invoke.mockResolvedValue(payload);

    await expect(getBuildModelPolicy()).rejects.toMatchObject({
      code: 'build_model_policy_contract_invalid',
      retryable: false
    });
  });

  it('refuses policy reads outside the desktop runtime', async () => {
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {}
    });

    await expect(getBuildModelPolicy()).rejects.toMatchObject({
      code: 'desktop_runtime_required'
    });
    expect(mocks.invoke).not.toHaveBeenCalled();
  });
});
