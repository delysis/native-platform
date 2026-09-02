export interface DocumentFilesystemHint {
  project_id: string;
  session_id: string;
}

export interface ProjectSessionIdentity {
  project_id: string;
  session_id: string;
}

/**
 * A native watcher event is only a scoped wakeup. It never carries document
 * state and therefore may only ask the authoritative project refresh lane to
 * run for the exact session that is still open.
 */
export function routeDocumentFilesystemHint(
  hint: unknown,
  currentProject: ProjectSessionIdentity | null,
  scheduleRefresh: () => void
): boolean {
  if (!hint || typeof hint !== 'object') return false;
  const candidate = hint as Partial<DocumentFilesystemHint>;
  if (
    !currentProject ||
    typeof candidate.project_id !== 'string' ||
    typeof candidate.session_id !== 'string' ||
    candidate.project_id !== currentProject.project_id ||
    candidate.session_id !== currentProject.session_id
  ) return false;

  scheduleRefresh();
  return true;
}
