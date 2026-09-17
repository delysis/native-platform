import { describe, expect, it } from 'vitest';
import { workspaceFoldersForProject, workspaceRootIsActive, type WorkspaceRootsSnapshot } from './workspaceFolders';

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
