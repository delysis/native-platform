/**
 * Restore the writing surface before starting optional background work.
 *
 * `present` must not resolve until the restored document has reached the DOM
 * and the browser has had an opportunity to place its caret. Every boundary
 * is guarded because Tauri replies can arrive after a project switch or
 * component teardown.
 */
export async function restoreBeforeBackgroundWork<T>(operations: {
  restore: () => Promise<T | null>;
  present: (restored: T) => Promise<void>;
  isCurrent: (restored: T) => boolean;
  background: (restored: T) => Promise<void>;
}): Promise<void> {
  const restored = await operations.restore();
  if (restored === null || !operations.isCurrent(restored)) return;

  await operations.present(restored);
  if (!operations.isCurrent(restored)) return;

  await operations.background(restored);
}

export function shouldDiscoverModelsOnStartup(suggestionsEnabled: boolean): boolean {
  return suggestionsEnabled;
}

export type CurrentAsyncResult<T> =
  | { status: 'current'; value: T }
  | { status: 'stale' };

/**
 * Runs one project-bound async step and refuses its reply if the project,
 * session, restore serial, or component lifetime changed while awaiting it.
 */
export async function runCurrentWorkspaceStep<C, T>(operations: {
  capture: C;
  isCurrent: (capture: C) => boolean;
  run: () => Promise<T>;
}): Promise<CurrentAsyncResult<T>> {
  if (!operations.isCurrent(operations.capture)) return { status: 'stale' };
  const value = await operations.run();
  return operations.isCurrent(operations.capture)
    ? { status: 'current', value }
    : { status: 'stale' };
}
