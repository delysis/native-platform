import type { ImportSource } from './ipc';
import type { CollectionScope, CollectionStatus } from './types';

/** Scope construction never interprets an empty folder as an entire account. */
export function connectedScope(source: ImportSource, input: string): CollectionScope | null {
  const query = input.trim();
  if (!query) return null;
  if (source === 'drive') return { kind: 'drive_folder', id: query };
  const prefix = source === 'google_alerts' ? 'from:googlealerts-noreply@google.com'
    : source === 'linked_in' ? 'from:linkedin.com' : '';
  return { kind: 'gmail_query', query: prefix ? `${prefix} (${query})` : query };
}

export function collectionProgressText(status: CollectionStatus): string {
  const count = `${status.retained_count} ${status.retained_count === 1 ? 'source' : 'sources'} retained`;
  switch (status.phase) {
    case 'running': return `${count} · Updating…`;
    case 'paused': return `${count} · Paused`;
    case 'interrupted': return `${count} · Interrupted`;
    case 'complete': return status.coverage_complete ? count : `${count} · Some sources could not be read`;
    default: return count;
  }
}

export function collectionErrorText(error: unknown): string {
  if (error && typeof error === 'object' && 'message' in error && typeof error.message === 'string') return error.message;
  return String(error);
}
