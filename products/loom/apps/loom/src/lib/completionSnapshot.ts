import type {
  CompletionOperationSnapshot,
  CompletionSnapshot,
  CompletionTerminalSnapshot
} from './types';

export interface CompletionSnapshotScope {
  projectId: string;
  sessionId: string;
  documentId: string;
}

export interface CompletionSnapshotFacts {
  activeRunIds: string[];
  cancellationRequestedRunIds: string[];
  /** Full replacement map for the partial text in this exact snapshot. */
  liveTextByRun: Record<string, string>;
  /** Full replacement map for sequence-based renderer presentation identity. */
  liveTextSequenceByRun: Record<string, string>;
}

const U64_MAX_DECIMAL = '18446744073709551615';
const MAX_COMPLETION_PARTIAL_TEXT_UTF8_BYTES = 256 * 1024;

function isDecimalU64(value: string): boolean {
  if (!/^(0|[1-9][0-9]*)$/.test(value)) return false;
  return value.length < U64_MAX_DECIMAL.length ||
    (value.length === U64_MAX_DECIMAL.length && value <= U64_MAX_DECIMAL);
}

function isActiveOperationPhase(value: string): boolean {
  return value === 'reserved' ||
    value === 'queued' ||
    value === 'running' ||
    value === 'terminal';
}

function isTerminalClass(value: string): boolean {
  return value === 'completed' || value === 'cancelled' || value === 'failed';
}

function terminalMatchesOperation(
  terminal: CompletionTerminalSnapshot | null,
  operation: CompletionOperationSnapshot
): boolean {
  return terminal === null || (
    terminal.operation_id === operation.request_id &&
    terminal.attempt_id === operation.attempt_id &&
    terminal.sequence === operation.operation_sequence &&
    isDecimalU64(terminal.sequence) &&
    isTerminalClass(terminal.class)
  );
}

function terminalProjectionsMatch(
  authoritative: CompletionTerminalSnapshot | null,
  projected: CompletionTerminalSnapshot | null
): boolean {
  if (authoritative === null || projected === null) return authoritative === projected;
  return authoritative.operation_id === projected.operation_id &&
    authoritative.attempt_id === projected.attempt_id &&
    authoritative.sequence === projected.sequence &&
    authoritative.class === projected.class;
}

/**
 * Validates that one native completion snapshot belongs to the exact renderer
 * scope that requested it and returns its bounded process-local run facts.
 * The caller still treats the durable branch rows as terminal/candidate truth.
 */
export function completionSnapshotFacts(
  snapshot: CompletionSnapshot,
  scope: CompletionSnapshotScope
): CompletionSnapshotFacts {
  if (
    snapshot.project_id !== scope.projectId ||
    snapshot.session_id !== scope.sessionId ||
    snapshot.document_id !== scope.documentId
  ) {
    throw new Error('The desktop returned completion state for a stale manuscript scope.');
  }

  const durableBranches = new Map<string, string>();
  for (const branch of snapshot.branches) {
    if (branch.document_id !== scope.documentId) {
      throw new Error('The desktop returned a completion branch for another manuscript.');
    }
    if (durableBranches.has(branch.run_id)) {
      throw new Error('The desktop repeated a completion branch occurrence.');
    }
    durableBranches.set(branch.run_id, branch.branch_id);
  }

  const requestIds = new Set<string>();
  const activeRunIds = new Set<string>();
  const activeBranchIds = new Set<string>();
  const cancellationRequestedRunIds = new Set<string>();
  const liveTextByRun: Record<string, string> = {};
  const liveTextSequenceByRun: Record<string, string> = {};
  const textEncoder = new TextEncoder();
  for (const operation of snapshot.active_operations) {
    if (
      !operation.request_id ||
      !operation.attempt_id ||
      !isDecimalU64(operation.operation_sequence) ||
      !isActiveOperationPhase(operation.phase) ||
      requestIds.has(operation.request_id) ||
      !terminalMatchesOperation(operation.authoritative_terminal, operation) ||
      !terminalMatchesOperation(operation.final_projection, operation) ||
      !terminalProjectionsMatch(
        operation.authoritative_terminal,
        operation.final_projection
      ) ||
      (operation.phase === 'terminal') !== (operation.authoritative_terminal !== null) ||
      operation.branches.length === 0 ||
      operation.progress_sequences.some((sequence) => !isDecimalU64(sequence))
    ) {
      throw new Error('The desktop returned inconsistent completion operation facts.');
    }
    requestIds.add(operation.request_id);
    for (const branch of operation.branches) {
      if (
        activeRunIds.has(branch.run_id) ||
        activeBranchIds.has(branch.branch_id) ||
        durableBranches.get(branch.run_id) !== branch.branch_id
      ) {
        throw new Error('The desktop returned an active route without its durable branch identity.');
      }
      activeRunIds.add(branch.run_id);
      activeBranchIds.add(branch.branch_id);
      const partial = branch.partial_text;
      if (partial !== null) {
        const encodedByteLength = typeof partial === 'object' && typeof partial.text === 'string'
          ? textEncoder.encode(partial.text).byteLength
          : -1;
        if (
          typeof partial !== 'object' ||
          typeof partial.text !== 'string' ||
          typeof partial.sequence !== 'string' ||
          typeof partial.utf8_byte_len !== 'string' ||
          !isDecimalU64(partial.sequence) ||
          !isDecimalU64(partial.utf8_byte_len) ||
          encodedByteLength > MAX_COMPLETION_PARTIAL_TEXT_UTF8_BYTES ||
          partial.utf8_byte_len !== String(encodedByteLength)
        ) {
          throw new Error('The desktop returned an invalid completion partial-text projection.');
        }
        liveTextByRun[branch.run_id] = partial.text;
        liveTextSequenceByRun[branch.run_id] = partial.sequence;
      }
      if (operation.cancellation_requested) {
        cancellationRequestedRunIds.add(branch.run_id);
      }
    }
  }

  return {
    activeRunIds: [...activeRunIds],
    cancellationRequestedRunIds: [...cancellationRequestedRunIds],
    liveTextByRun,
    liveTextSequenceByRun
  };
}
