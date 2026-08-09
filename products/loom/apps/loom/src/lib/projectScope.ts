export interface ProjectSessionScope {
  projectId: string;
  sessionId: string;
}

export interface ProjectSessionLike {
  project_id: string;
  session_id: string;
}

export interface ProjectRestoreScope extends ProjectSessionScope {
  restoreSerial: number;
}

export interface NavigationScope extends ProjectRestoreScope {
  documentEpoch: number;
  editVersion: number;
  documentId: string | null;
}

export interface OpenDocumentIdentityLike {
  summary: { document_id: string };
}

export function projectSessionIsCurrent(
  project: ProjectSessionLike | null,
  scope: ProjectSessionScope
): boolean {
  return project?.project_id === scope.projectId && project.session_id === scope.sessionId;
}

export function projectRestoreScopeIsCurrent(
  project: ProjectSessionLike | null,
  restoreSerial: number,
  scope: ProjectRestoreScope
): boolean {
  return restoreSerial === scope.restoreSerial && projectSessionIsCurrent(project, scope);
}

export function navigationScopeIsCurrent(
  project: ProjectSessionLike | null,
  document: OpenDocumentIdentityLike | null,
  documentEpoch: number,
  editVersion: number,
  restoreSerial: number,
  scope: NavigationScope
): boolean {
  return projectRestoreScopeIsCurrent(project, restoreSerial, scope) &&
    documentEpoch === scope.documentEpoch &&
    editVersion === scope.editVersion &&
    (document?.summary.document_id ?? null) === scope.documentId;
}
