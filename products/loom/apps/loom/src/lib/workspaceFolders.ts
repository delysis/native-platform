import type { ProjectSnapshot } from './types';

/** Declarations and availability come from the owning workspace and its private grants. */
export interface WorkspaceFolder {
  id: string;
  name: string;
  owner: boolean;
  available: boolean;
  path: string | null;
  project_id: string | null;
}

export interface WorkspaceRootsSnapshot {
  workspace_id: string;
  workspace_session_id: string;
  roots: WorkspaceFolder[];
}

export type WorkspaceFolderRow = WorkspaceFolder & { transient?: true };

export function workspaceRootIsActive(folder: WorkspaceFolder, project: Pick<ProjectSnapshot, 'root' | 'project_id'>): boolean {
  return folder.path === project.root && folder.project_id === project.project_id;
}

/** Never show another session's folders while the native snapshot is arriving. */
export function workspaceFoldersForProject(
  snapshot: WorkspaceRootsSnapshot | null,
  snapshotScope: string,
  project: Pick<ProjectSnapshot, 'root' | 'project_id' | 'session_id' | 'title'> | null
): WorkspaceFolderRow[] {
  if (!project) return [];
  const roots = snapshot && snapshotScope === `${project.project_id}/${project.session_id}` ? snapshot.roots : [];
  if (roots.some(folder => workspaceRootIsActive(folder, project))) return roots;
  // A declaration can disappear during editing. Keep the open writing visible
  // until the author leaves it, without resurrecting a declaration or a grant.
  return [...roots, { id: 'active-root', name: project.title, owner: false, available: true, path: project.root, project_id: project.project_id, transient: true }];
}
