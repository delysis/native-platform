export type PanePosition = 'main' | 'right' | 'bottom';

/** The writing surface is the fallback when no other content pane remains. */
export function visiblePanes(equipped: readonly PanePosition[], hidden: ReadonlySet<string>) {
  const right = equipped.includes('right') && !hidden.has('right');
  const bottom = equipped.includes('bottom') && !hidden.has('bottom');
  return { main: !hidden.has('main') || (!right && !bottom), right, bottom };
}

export function togglePaneVisibility(
  position: PanePosition, equipped: readonly PanePosition[], hidden: ReadonlySet<string>
): Set<string> {
  const current = visiblePanes(equipped, hidden);
  const next = new Set(hidden);
  if (current.main) next.delete('main');
  if (position === 'main' && !current.right && !current.bottom) return next;
  if (next.has(position)) next.delete(position); else next.add(position);
  if (visiblePanes(equipped, next).main) next.delete('main');
  return next;
}
