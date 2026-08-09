import type { BranchSummary } from './types';

export type BranchBodyDisposition = 'absent' | 'cached' | 'fetch' | 'too_large';

export function branchBodyDisposition(
  branch: BranchSummary,
  cachedBlobId: string | undefined,
  maxBytes: number
): BranchBodyDisposition {
  if (!branch.output_blob_id || branch.output_byte_len === null) return 'absent';
  if (cachedBlobId === branch.output_blob_id) return 'cached';
  return branch.output_byte_len > maxBytes ? 'too_large' : 'fetch';
}

/** Replace refreshed newest rows while retaining explicitly loaded older rows. */
export function mergeNewestPage<T extends { run_id: string }>(
  newest: T[],
  loaded: T[]
): T[] {
  const newestRunIds = new Set(newest.map((branch) => branch.run_id));
  return [...newest, ...loaded.filter((branch) => !newestRunIds.has(branch.run_id))];
}

/** Append one cursor page without duplicating a boundary row. */
export function appendUniquePage<T extends { run_id: string }>(
  loaded: T[],
  older: T[]
): T[] {
  const loadedRunIds = new Set(loaded.map((branch) => branch.run_id));
  return [...loaded, ...older.filter((branch) => !loadedRunIds.has(branch.run_id))];
}
