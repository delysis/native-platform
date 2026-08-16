import { describe, expect, it, vi } from 'vitest';
import { observeNativeFullscreen, type NativeFullscreenWindow } from './nativeFullscreen';

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe('observeNativeFullscreen', () => {
  it('tracks native fullscreen through Tauri resize notifications', async () => {
    let fullscreen = false;
    let resized: (() => void) | undefined;
    const stopListener = vi.fn();
    const update = vi.fn();
    const window: NativeFullscreenWindow = {
      isFullscreen: async () => fullscreen,
      onResized: async (listener) => {
        resized = listener;
        return stopListener;
      }
    };

    const dispose = observeNativeFullscreen(window, update);
    await settle();
    expect(update).toHaveBeenLastCalledWith(false);

    fullscreen = true;
    resized?.();
    await settle();
    expect(update).toHaveBeenLastCalledWith(true);

    fullscreen = false;
    resized?.();
    await settle();
    expect(update).toHaveBeenLastCalledWith(false);
    dispose();
    expect(stopListener).toHaveBeenCalledOnce();
  });

  it('cannot update after disposal while listener installation is pending', async () => {
    let resolveFullscreen: ((value: boolean) => void) | undefined;
    let resolveListener: ((stop: () => void) => void) | undefined;
    const stopListener = vi.fn();
    const update = vi.fn();
    const window: NativeFullscreenWindow = {
      isFullscreen: () => new Promise((resolve) => { resolveFullscreen = resolve; }),
      onResized: () => new Promise((resolve) => { resolveListener = resolve; })
    };

    const dispose = observeNativeFullscreen(window, update);
    dispose();
    resolveFullscreen?.(true);
    resolveListener?.(stopListener);
    await settle();

    expect(update).not.toHaveBeenCalled();
    expect(stopListener).toHaveBeenCalledOnce();
  });
});
