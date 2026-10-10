import { expect, it, vi } from 'vitest';
const transport = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: transport.invoke }));
import { updatePreferences } from './ipc';
it('preserves author preference write order while native storage is pending', async () => {
  vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
  let release!: (value: unknown) => void;
  const firstReply = new Promise(resolve => { release = resolve; });
  transport.invoke.mockImplementationOnce(() => firstReply)
    .mockResolvedValue({ revision: '2', last_local_model: '/models/second.gguf', project_suggestions: {} });
  const first = updatePreferences({ kind: 'remember_model', path: '/models/first.gguf' });
  const second = updatePreferences({ kind: 'remember_model', path: '/models/second.gguf' });
  try {
    await vi.waitFor(() => expect(transport.invoke).toHaveBeenCalled());
    expect(transport.invoke).toHaveBeenCalledTimes(1);
  } finally {
    release({ revision: '1', last_local_model: '/models/first.gguf', project_suggestions: {} });
    await Promise.allSettled([first, second]);
    vi.unstubAllGlobals();
  }
  expect(transport.invoke).toHaveBeenLastCalledWith('plugin:loom|preferences_update', { change: { kind: 'remember_model', path: '/models/second.gguf' } });
});

it('drains later preferences after a failed write', async () => {
  vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
  const damaged = { code: 'preferences_failed' };
  transport.invoke.mockReset().mockRejectedValueOnce(damaged)
    .mockResolvedValueOnce({ revision: '1', last_local_model: '/models/next.gguf', project_suggestions: {} });
  try {
    const first = updatePreferences({ kind: 'remember_model', path: '/models/bad.gguf' });
    const rejected = expect(first).rejects.toBe(damaged);
    const second = updatePreferences({ kind: 'remember_model', path: '/models/next.gguf' });
    await rejected;
    expect((await second).last_local_model).toBe('/models/next.gguf');
    expect(transport.invoke).toHaveBeenCalledTimes(2);
  } finally { vi.unstubAllGlobals(); }
});

it('bounds accepted pending preferences and recovers capacity after drain', async () => {
  vi.stubGlobal('window', { __TAURI_INTERNALS__: {} });
  let release!: (value: unknown) => void;
  const pending = new Promise(resolve => { release = resolve; });
  transport.invoke.mockReset().mockImplementationOnce(() => pending)
    .mockResolvedValue({ revision: '1', last_local_model: null, project_suggestions: {} });
  const accepted = Array.from({ length: 8 }, (_, index) => updatePreferences({ kind: 'remember_model', path: `/models/${index}.gguf` }));
  try {
    await expect(updatePreferences({ kind: 'remember_model', path: '/models/overflow.gguf' })).rejects.toMatchObject({ code: 'preferences_busy' });
    expect(transport.invoke.mock.calls.length).toBeLessThanOrEqual(1);
  } finally {
    release({ revision: '0', last_local_model: null, project_suggestions: {} });
    await Promise.allSettled(accepted);
  }
  try {
    await updatePreferences({ kind: 'remember_model', path: '/models/after-drain.gguf' });
    expect(transport.invoke).toHaveBeenCalledTimes(9);
  } finally { vi.unstubAllGlobals(); }
});
