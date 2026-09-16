export type WorkspaceCommand = { kind: 'signal' | 'cabal' } | { kind: 'join'; invitation: string };

/** Workspace toys do not require an editable manuscript or a loaded model. */
export function workspaceCommand(expression: string): WorkspaceCommand | null {
  const text = expression.trim();
  if (text === ':signal') return { kind: 'signal' };
  if (text === ':cabal') return { kind: 'cabal' };
  if (text.startsWith(':join ')) {
    const invitation = text.slice(6).trim();
    if (invitation) return { kind: 'join', invitation };
  }
  return null;
}
