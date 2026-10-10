import assert from 'node:assert/strict';
import { describe, expect, it } from 'vitest';
import { readWorkspaceFolders, rememberWorkspaceFolder, workspaceFolderLabels, workspaceFolderName, workspaceFolderGroups, forgetWorkspaceFolder, renameWorkspaceFolder } from './workspaceFolders';
import { workspaceRows } from './workspaceTree';
import type { DocumentSummary } from './types';
import { workspaceFoldersForProject, workspaceRootIsActive, type WorkspaceRootsSnapshot } from './workspaceFolders';

const KEY = 'loom.workspace-folders';
const stored = (value: unknown) => ({ getItem: () => JSON.stringify(value) });

describe('workspace navigation path identity', () => {
  it('distinguishes same-title roots by their exact paths, without dropping either', () => {
    const roots = ['/Users/alice/writing', '/Users/bob/writing'];
    const rows = readWorkspaceFolders(stored(roots.map(root => ({ root, title: 'writing' }))));
    assert.deepEqual(rows, roots.map(root => ({ root, title: 'writing' })));
    assert.deepEqual([...workspaceFolderLabels(rows).values()], ['writing — alice', 'writing — bob']);
  });

  it('keeps a readable label and a separate exact root even for a single bookmark', () => {
    assert.deepEqual(readWorkspaceFolders(stored([{ root: '/work/writing', title: 'writing' }])),
      [{ root: '/work/writing', title: 'writing' }]);
    assert.equal(workspaceFolderName({ root: '/private/var/folders/gone/writing', title: '/private/var/folders/gone/writing' }), 'writing');
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
    assert.deepEqual(readWorkspaceFolders(storage), [{ root: '/a', title: 'one' }, { root: '/b', title: 'one' }]);
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
    assert.deepEqual(next, [{ root: '/a/writing', title: 'writing' }, { root: '/b/writing', title: 'writing' }]);
    assert.deepEqual(readWorkspaceFolders({ getItem: () => saved }), next);
  });

  it('round-trips long legal root labels instead of silently discarding them on relaunch', () => {
    const root = `/${'a'.repeat(300)}/writing`;
    let saved = '';
    const next = rememberWorkspaceFolder([], { root, title: 'writing' }, { setItem: (_key, value) => { saved = value; } });
    assert.deepEqual(readWorkspaceFolders({ getItem: () => saved }), next);
    assert.equal(next[0].title, 'writing');
    assert.equal(next[0].root, root);
  });

  it('reopens an exact root without accumulating duplicate entries', () => {
    const next = rememberWorkspaceFolder([{ root: '/a', title: 'old' }], { root: '/a', title: 'new' });
    assert.deepEqual(next, [{ root: '/a', title: 'old' }]);
  });

  it('bounds history but makes explicit storage failures visible rather than reporting a durable edit', () => {
    const folders = Array.from({ length: 32 }, (_, index) => ({ root: `/root/${index}`, title: 'writing' }));
    assert.throws(() => rememberWorkspaceFolder(folders, { root: '/root/new', title: 'writing' }, {
      setItem: () => { throw new Error('storage unavailable'); }
    }), /storage unavailable/);
    const next = rememberWorkspaceFolder(folders, { root: '/root/new', title: 'writing' });
    assert.equal(next.length, 32);
    assert.equal(next[0].root, '/root/1');
    assert.equal(next.at(-1)?.root, '/root/new');
  });

  it('rejects malformed and overlong hints without manufacturing a path', () => {
    assert.deepEqual(readWorkspaceFolders({ getItem: () => 'broken' }), []);
    assert.deepEqual(readWorkspaceFolders(stored([null, {}, { root: 12, title: 'x' },
      { root: '', title: 'x' }, { root: '/x', title: 12 }, { root: 'x'.repeat(4097), title: 'x' } ])), []);
  });

  it('forgets a missing active bookmark without resurrecting it or losing the live document group', () => {
    const root = { root: '/missing/writing', title: 'Novel' };
    let saved = '';
    const next = forgetWorkspaceFolder([root], root.root, { setItem: (_key, value) => saved = value });
    assert.deepEqual(next, []); assert.equal(saved, '[]');
    assert.deepEqual(workspaceFolderGroups(next, root.root), [null]);
    assert.deepEqual(workspaceFolderGroups(next, null), []);
  });

  it('renames only a bounded sidebar label and preserves the exact root through reopening', () => {
    const root = { root: '/missing/writing', title: 'Novel' };
    const renamed = renameWorkspaceFolder([root], root.root, '  Research  ', { setItem() {} });
    assert.deepEqual(renamed, [{ root: root.root, title: 'Research' }]);
    assert.equal(rememberWorkspaceFolder(renamed, root)[0], renamed[0]);
    assert.equal(root.title, 'Novel');
    for (const label of ['', '\u0000', '界'.repeat(86)]) assert.throws(() => renameWorkspaceFolder([root], root.root, label, { setItem() { assert.fail(); } }));
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


const project = { project_id: 'writing', session_id: 'open-writing', root: '/writing', title: 'Writing' };
const snapshot: WorkspaceRootsSnapshot = {
  workspace_id: 'writing', workspace_session_id: 'workspace',
  roots: [
    { id: 'owner', name: 'Writing', owner: true, available: true, path: '/writing', project_id: 'writing' },
    { id: 'root-notes', name: 'Notes', owner: false, available: true, path: '/notes', project_id: 'notes' },
    { id: 'root-offline', name: 'Offline', owner: false, available: false, path: null, project_id: null }
  ]
};

describe('workspace root navigation', () => {
  it('shows exactly the owning workspace declarations, including unavailable roots', () => {
    expect(workspaceFoldersForProject(snapshot, 'writing/open-writing', project)).toEqual(snapshot.roots);
    expect(workspaceFoldersForProject(null, '', null)).toEqual([]);
  });

  it('never leaks earlier folders into a new session or a different workspace', () => {
    const reopened = { ...project, session_id: 'reopened' };
    expect(workspaceFoldersForProject(snapshot, 'writing/open-writing', reopened)).toEqual([
      { id: 'active-root', name: 'Writing', owner: false, available: true, path: '/writing', project_id: 'writing', transient: true }
    ]);
    const other = { project_id: 'other', session_id: 'open-other', root: '/other', title: 'Other' };
    expect(workspaceFoldersForProject(snapshot, 'writing/open-writing', other).map(folder => folder.path)).toEqual(['/other']);
  });

  it('keeps removed active writing visible only until the author switches roots', () => {
    const active = { project_id: 'detached', session_id: 'open-detached', root: '/detached', title: 'Detached' };
    const rows = workspaceFoldersForProject(snapshot, 'detached/open-detached', active);
    expect(rows.slice(0, -1)).toEqual(snapshot.roots);
    expect(rows.at(-1)).toMatchObject({ path: '/detached', transient: true, owner: false });
    expect(workspaceFoldersForProject(snapshot, 'writing/open-writing', project)).toEqual(snapshot.roots);
    expect(snapshot.roots).toHaveLength(3);
  });

  it('does not confuse a copied folder with its original or an unavailable grant', () => {
    expect(workspaceRootIsActive(snapshot.roots[0], project)).toBe(true);
    expect(workspaceRootIsActive(snapshot.roots[0], { ...project, root: '/copied-writing' })).toBe(false);
    expect(workspaceRootIsActive(snapshot.roots[0], { ...project, project_id: 'replacement' })).toBe(false);
    expect(workspaceRootIsActive(snapshot.roots[2], project)).toBe(false);
  });
});
