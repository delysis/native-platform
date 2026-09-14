import { describe, expect, it } from 'vitest';
import { workspaceCommand } from './workspaceCommand';

describe('workspace commands', () => {
  it('recognizes the complete command and retains the invitation for native validation', () => {
    expect(workspaceCommand('  :cabal \n')).toEqual({ kind: 'cabal' });
    expect(workspaceCommand(':signal')).toEqual({ kind: 'signal' });
    expect(workspaceCommand(' :join  loom://cabal/a-ticket ')).toEqual({ kind: 'join', invitation: 'loom://cabal/a-ticket' });
  });
  it('does not treat prose, prefixes, or incomplete joins as workspace commands', () => {
    for (const text of ['', ':join', ':join ', ':cabals', ':signal later', 'Say :cabal', '@prompt']) {
      expect(workspaceCommand(text)).toBeNull();
    }
  });
});
