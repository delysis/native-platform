export interface NativeFullscreenWindow {
  isFullscreen(): Promise<boolean>;
  onResized(listener: () => void): Promise<() => void>;
}

/**
 * Observe AppKit fullscreen state without inferring it from viewport geometry.
 * The serial prevents an older asynchronous read from overwriting a newer one,
 * and disposal remains safe while Tauri is still installing its listener.
 */
export function observeNativeFullscreen(
  window: NativeFullscreenWindow,
  update: (fullscreen: boolean) => void,
  reportError: (error: unknown) => void = () => {}
): () => void {
  let disposed = false;
  let checkSerial = 0;
  let unlisten: (() => void) | undefined;

  const synchronize = async (): Promise<void> => {
    const serial = ++checkSerial;
    try {
      const fullscreen = await window.isFullscreen();
      if (!disposed && serial === checkSerial) update(fullscreen);
    } catch (error) {
      if (!disposed && serial === checkSerial) reportError(error);
    }
  };

  void synchronize();
  void window.onResized(() => void synchronize()).then(
    (stop) => {
      if (disposed) stop();
      else unlisten = stop;
    },
    (error) => {
      if (!disposed) reportError(error);
    }
  );

  return () => {
    disposed = true;
    checkSerial += 1;
    unlisten?.();
    unlisten = undefined;
  };
}
