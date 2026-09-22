import assert from 'node:assert/strict';
import { describe, it } from 'vitest';
import { readWorkspaceFolders, rememberWorkspaceFolder } from './workspaceFolders';
import { workspaceRows } from './workspaceTree';
import type { DocumentSummary } from './types';

const KEY = 'loom.workspace-folders';
const stored = (value: unknown) => ({ getItem: () => JSON.stringify(value) });

describe('workspace navigation path identity', () => {
  it('distinguishes same-title roots by their exact paths, without dropping either', () => {
    const roots = ['/Users/alice/writing', '/Users/bob/writing'];
    const rows = readWorkspaceFolders(stored(roots.map(root => ({ root, title: 'writing' }))));
    assert.deepEqual(rows, roots.map(root => ({ root, title: root })));
  });

  it('qualifies a single remembered root too, so a real same-name child is unambiguous', () => {
    assert.deepEqual(readWorkspaceFolders(stored([{ root: '/work/writing', title: 'writing' }])),
      [{ root: '/work/writing', title: '/work/writing' }]);
  });

  it('does not treat case, Unicode, separators, or path spellings as filesystem aliases', () => {
    const roots = ['/A/writing', '/a/writing', '/a/é', '/a/e\u0301', '/a/x/../writing', 'C:\\work\\writing'];
    assert.deepEqual(readWorkspaceFolders(stored(roots.map(root => ({ root, title: 'same' }))))
      .map(folder => folder.root), roots);
  });

  it('deduplicates exact roots only and never writes while reading', () => {
    let writes = 0;
    const storage = {
      ...stored([{ root: '/a', title: 'one' }, { root: '/a', title: 'two' }, { root: '/b', title: 'one' }]),
      setItem: () => { writes += 1; }
    };
    assert.deepEqual(readWorkspaceFolders(storage), [{ root: '/a', title: '/a' }, { root: '/b', title: '/b' }]);
    assert.equal(writes, 0);
  });

  it('does not mutate caller-owned rows and persists only navigation hints', () => {
    const folders = [{ root: '/a/writing', title: 'writing' }];
    const before = structuredClone(folders);
    let saved = '';
    const next = rememberWorkspaceFolder(folders, { root: '/b/writing', title: 'writing' }, {
      setItem: (key, value) => { assert.equal(key, KEY); saved = value; }
    });
    assert.deepEqual(folders, before);
    assert.deepEqual(next, [{ root: '/a/writing', title: '/a/writing' }, { root: '/b/writing', title: '/b/writing' }]);
    assert.deepEqual(readWorkspaceFolders({ getItem: () => saved }), next);
  });

  it('round-trips long legal root labels instead of silently discarding them on relaunch', () => {
    const root = `/${'a'.repeat(300)}/writing`;
    let saved = '';
    const next = rememberWorkspaceFolder([], { root, title: 'writing' }, { setItem: (_key, value) => { saved = value; } });
    assert.deepEqual(readWorkspaceFolders({ getItem: () => saved }), next);
    assert.equal(next[0].title, root);
  });

  it('reopens an exact root without accumulating duplicate entries', () => {
    const next = rememberWorkspaceFolder([{ root: '/a', title: 'old' }], { root: '/a', title: 'new' });
    assert.deepEqual(next, [{ root: '/a', title: '/a' }]);
  });

  it('bounds history and still works when storage is denied', () => {
    const folders = Array.from({ length: 32 }, (_, index) => ({ root: `/root/${index}`, title: 'writing' }));
    const next = rememberWorkspaceFolder(folders, { root: '/root/new', title: 'writing' }, {
      setItem: () => { throw new Error('storage unavailable'); }
    });
    assert.equal(next.length, 32);
    assert.equal(next[0].root, '/root/1');
    assert.equal(next.at(-1)?.root, '/root/new');
  });

  it('rejects malformed and overlong hints without manufacturing a path', () => {
    assert.deepEqual(readWorkspaceFolders({ getItem: () => 'broken' }), []);
    assert.deepEqual(readWorkspaceFolders(stored([null, {}, { root: 12, title: 'x' },
      { root: '', title: 'x' }, { root: '/x', title: 12 }, { root: 'x'.repeat(4097), title: 'x' } ])), []);
  });

  it('keeps a root document relative, and preserves a genuine same-name nested directory', () => {
    const document = (relative_path: string): DocumentSummary => ({
      document_id: relative_path, relative_path, title: 'Untitled', kind: 'prose',
      revision_id: null, active_blob_id: null, word_count: 0, externally_modified: false
    });
    const rootDocument = document('Untitled.md');
    assert.deepEqual(workspaceRows([rootDocument], new Set(), '').map(row => row.path), ['Untitled.md']);
    const nested = document('writing/Untitled.md');
    assert.deepEqual(workspaceRows([nested], new Set(), '').map(row => row.path), ['writing/', 'writing/Untitled.md']);
    assert.equal(rootDocument.relative_path, 'Untitled.md');
    assert.equal(nested.relative_path, 'writing/Untitled.md');
  });
});
