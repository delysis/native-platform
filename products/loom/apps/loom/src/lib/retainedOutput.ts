import { openDocument } from './ipc';
import type { DocumentSummary, OpenDocument, TerminalRun } from './types';

/** Revision-bound reads shared by retained-output surfaces; failed reads may retry. */
export class RetainedOutputLoader {
  private readonly loads = new Map<string, Promise<OpenDocument>>();

  clear(): void { this.loads.clear(); }

  async read(projectId: string, sessionId: string, run: TerminalRun, summaries: readonly DocumentSummary[]): Promise<string> {
    const summary = summaries.find((document) => document.document_id === run.output_document_id);
    if (!summary?.revision_id || !summary.active_blob_id) throw new Error('The retained document is not available yet.');
    const key = `${projectId}/${sessionId}/${summary.document_id}/${summary.revision_id}/${summary.active_blob_id}`;
    let loading = this.loads.get(key);
    if (!loading) {
      loading = openDocument(projectId, sessionId, summary.document_id, summary.revision_id, summary.active_blob_id);
      this.loads.set(key, loading);
      // Native terminal history retains at most 64 runs.
      if (this.loads.size > 64) this.loads.delete(this.loads.keys().next().value!);
    }
    try { return (await loading).text; }
    catch (failure) { if (this.loads.get(key) === loading) this.loads.delete(key); throw failure; }
  }
}
