/** Presentation-only interactions. No project, path or document authority. */
export interface MenuPoint { readonly x: number; readonly y: number }

export interface RenameCompositionGuard {
  readonly active: boolean;
  reset(): void;
  start(): void;
  ownsCommandKey(
    event: Pick<KeyboardEvent, 'isComposing' | 'keyCode'>
  ): boolean;
  blurShouldCommit(): boolean;
  finish(): boolean;
}

/**
 * Own the rename input's IME lifetime independently from manuscript editing.
 *
 * WebKit may report an active composition through either `isComposing` or the
 * legacy 229 key code, and may deliver blur before compositionend. The guard
 * keeps those cases from committing a partial title while preserving the
 * explicit blur as a deferred commit request.
 */
export function createRenameCompositionGuard(): RenameCompositionGuard {
  let active = false;
  let commitAfterBlur = false;
  return {
    get active() {
      return active;
    },
    reset() {
      active = false;
      commitAfterBlur = false;
    },
    start() {
      active = true;
    },
    ownsCommandKey(event) {
      if (event.isComposing || event.keyCode === 229) active = true;
      return active;
    },
    blurShouldCommit() {
      if (!active) return true;
      commitAfterBlur = true;
      return false;
    },
    finish() {
      active = false;
      const shouldCommit = commitAfterBlur;
      commitAfterBlur = false;
      return shouldCommit;
    }
  };
}

export type ContextMenuKeyAction =
  | { readonly kind: 'focus'; readonly index: number }
  | { readonly kind: 'activate'; readonly index: number }
  | { readonly kind: 'dismiss' }
  | { readonly kind: 'none' };

export function isContextMenuTriggerKey(
  event: Pick<KeyboardEvent, 'key' | 'shiftKey' | 'altKey' | 'ctrlKey' | 'metaKey'>
): boolean {
  if (event.altKey || event.ctrlKey || event.metaKey) return false;
  return event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10');
}

export function contextMenuKeyAction(
  event: Pick<KeyboardEvent, 'key'>,
  currentIndex: number,
  itemCount: number
): ContextMenuKeyAction {
  if (!Number.isInteger(itemCount) || itemCount <= 0) return { kind: 'none' };
  const current = Number.isInteger(currentIndex)
    ? Math.min(Math.max(currentIndex, 0), itemCount - 1)
    : 0;
  switch (event.key) {
    case 'ArrowDown':
      return { kind: 'focus', index: (current + 1) % itemCount };
    case 'ArrowUp':
      return { kind: 'focus', index: (current - 1 + itemCount) % itemCount };
    case 'Home':
      return { kind: 'focus', index: 0 };
    case 'End':
      return { kind: 'focus', index: itemCount - 1 };
    case 'Enter':
    case ' ':
      return { kind: 'activate', index: current };
    case 'Escape':
      return { kind: 'dismiss' };
    default:
      return { kind: 'none' };
  }
}

export function clampMenuPoint(
  requested: MenuPoint,
  menuWidth: number,
  menuHeight: number,
  viewportWidth: number,
  viewportHeight: number,
  margin = 8
): MenuPoint {
  const width = Number.isFinite(menuWidth) ? Math.max(0, menuWidth) : 0;
  const height = Number.isFinite(menuHeight) ? Math.max(0, menuHeight) : 0;
  const viewportX = Number.isFinite(viewportWidth) ? Math.max(0, viewportWidth) : 0;
  const viewportY = Number.isFinite(viewportHeight) ? Math.max(0, viewportHeight) : 0;
  const inset = Number.isFinite(margin) ? Math.max(0, margin) : 0;
  const maximumX = Math.max(inset, viewportX - width - inset);
  const maximumY = Math.max(inset, viewportY - height - inset);
  return {
    x: Math.min(Math.max(Number.isFinite(requested.x) ? requested.x : inset, inset), maximumX),
    y: Math.min(Math.max(Number.isFinite(requested.y) ? requested.y : inset, inset), maximumY)
  };
}

/** Keep IME confirmation native; only an ordinary Return/Escape owns rename. */
export function handleInlineRenameKey(
  event: KeyboardEvent,
  composition: RenameCompositionGuard,
  commit: () => void,
  cancel: () => void
): void {
  if (composition.ownsCommandKey(event)) {
    if (event.key === 'Enter' || event.key === 'Escape') event.stopPropagation();
    return;
  }
  if (event.key !== 'Enter' && event.key !== 'Escape') return;
  event.preventDefault();
  event.stopPropagation();
  if (event.key === 'Enter') commit(); else cancel();
}
