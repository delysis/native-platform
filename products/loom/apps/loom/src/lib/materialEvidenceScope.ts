import type { MaterialEntry, MaterialEvidence } from './materials';

export interface MaterialSourceScope { projectId: string; sessionId: string }

export interface MaterialReferenceResolution {
  project_id: string;
  session_id: string;
  material: MaterialEntry | null;
  evidence: MaterialEvidence | null;
}

/** Native resolution checks both admitted origins atomically; the UI never chooses a winner. */
export async function resolveScopedMaterialReference(
  active: MaterialSourceScope,
  owner: MaterialSourceScope | null,
  reference: string,
  current: () => boolean,
  resolve: (projectId: string, sessionId: string, reference: string) => Promise<MaterialReferenceResolution>
): Promise<MaterialReferenceResolution | null> {
  if (!current()) return null;
  try {
    const result = await resolve(active.projectId, active.sessionId, reference);
    if (!current()) return null;
    const admitted = [active, owner].some(scope => scope && scope.projectId === result.project_id && scope.sessionId === result.session_id);
    if (!admitted || !result.material && !result.evidence) throw new Error('The source response does not belong to this workspace.');
    return result;
  } catch (error) {
    if (!current()) return null;
    throw error;
  }
}
