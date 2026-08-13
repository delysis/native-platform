import type { BranchCard } from './types';

export function branchIsActionableOnShelf(
  branch: BranchCard,
  activeRevisionId: string | null | undefined
): boolean {
  if (!activeRevisionId || branch.source_revision_id !== activeRevisionId) return false;
  if (branch.status === 'ready') {
    return Boolean(
      branch.candidate_id &&
      branch.selection !== 'promote' &&
      branch.selection !== 'reject'
    );
  }
  return branch.status === 'failed' || branch.status === 'interrupted';
}
