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
  onInterrupted?: () => void;
}): Promise<void> {
  const restored = await operations.restore();
  if (restored === null) return;
  if (!operations.isCurrent(restored)) {
    operations.onInterrupted?.();
    return;
  }

  await operations.present(restored);
  if (!operations.isCurrent(restored)) {
    operations.onInterrupted?.();
    return;
  }

  await operations.background(restored);
}

export function shouldDiscoverModelsOnStartup(suggestionsEnabled: boolean): boolean {
  return suggestionsEnabled;
}

export type StartupProjectAcquisition<T> = {
  project: T;
  source: 'current' | 'default';
};

export type WorkspaceResumeAction = 'none' | 'start_infrastructure' | 'restore_workspace';

/** Attach a project reply only while its renderer/application owner is live. */
export async function attachWorkspaceProjectReply<T>(operations: {
  open: () => Promise<T>;
  mayAttach: () => boolean;
  attach: (project: T) => void;
  onHeld: () => void;
}): Promise<T | null> {
  let project: T;
  try {
    project = await operations.open();
  } catch (error) {
    if (!operations.mayAttach()) operations.onHeld();
    throw error;
  }
  if (!operations.mayAttach()) {
    operations.onHeld();
    return null;
  }
  operations.attach(project);
  return project;
}

/** Resume an interrupted startup without reinstalling already-live listeners. */
export function workspaceResumeAction(
  startupHeld: boolean,
  infrastructureStarted: boolean,
  closeAllowsStartup: boolean
): WorkspaceResumeAction {
  if (!startupHeld || !closeAllowsStartup) return 'none';
  return infrastructureStarted ? 'restore_workspace' : 'start_infrastructure';
}

/**
 * Never enqueue or attach startup project work after application close begins.
 * A default-open already admitted before close may still finish natively, but
 * its stale reply is left for the close path's detached-session reconciliation.
 */
export async function acquireStartupProject<T>(operations: {
  currentProject: () => Promise<T>;
  openDefaultProject: () => Promise<T>;
  mayContinue: () => boolean;
  projectIsAbsent: (error: unknown) => boolean;
  onHeld?: () => void;
}): Promise<StartupProjectAcquisition<T> | null> {
  const hold = (): null => {
    operations.onHeld?.();
    return null;
  };
  if (!operations.mayContinue()) return hold();
  let current: T | null = null;
  try {
    current = await operations.currentProject();
  } catch (error) {
    if (!operations.projectIsAbsent(error)) throw error;
    // Native absence is the normal first-launch path. The caller owns any
    // user-facing open-default failure after the close gate below.
  }
  if (!operations.mayContinue()) return hold();
  if (current !== null) return { project: current, source: 'current' };

  // This check is intentionally adjacent to the enqueue point. A close that
  // begins while project_current is pending must win before project_open_default.
  if (!operations.mayContinue()) return hold();
  const opened = await operations.openDefaultProject();
  return operations.mayContinue()
    ? { project: opened, source: 'default' }
    : hold();
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
