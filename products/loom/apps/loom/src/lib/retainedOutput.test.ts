import { describe, expect, it, vi } from 'vitest';
import { RetainedOutputLoader } from './retainedOutput';
import type { DocumentSummary, TerminalRun } from './types';
const ipc = vi.hoisted(() => ({ open: vi.fn() }));
vi.mock('./ipc', () => ({ openDocument: ipc.open }));
const summary: DocumentSummary = { document_id: 'output', relative_path: 'Output.md', title: 'Output', kind: 'prose', revision_id: 'r1', active_blob_id: 'b1', word_count: 1, externally_modified: false };
const run = { output_document_id: 'output' } as TerminalRun;

describe('retained document output reads', () => {
  it('shares identical reads but reloads changed revisions and sessions', async () => {
    ipc.open.mockReset().mockResolvedValue({ text: 'Full output beyond the preview' });
    const loader = new RetainedOutputLoader();
    await Promise.all([loader.read('p', 's1', run, [summary]), loader.read('p', 's1', run, [summary])]);
    expect(ipc.open).toHaveBeenCalledTimes(1);
    await loader.read('p', 's1', run, [{ ...summary, revision_id: 'r2', active_blob_id: 'b2' }]);
    expect(ipc.open).toHaveBeenLastCalledWith('p', 's1', 'output', 'r2', 'b2');
    await loader.read('p', 's2', run, [summary]);
    expect(ipc.open).toHaveBeenCalledTimes(3);
  });
  it('does not cache a failed revision read or guess missing document authority', async () => {
    ipc.open.mockReset().mockRejectedValueOnce(new Error('Revision changed')).mockResolvedValue({ text: 'Retry result' });
    const loader = new RetainedOutputLoader();
    await expect(loader.read('p', 's', run, [summary])).rejects.toThrow('Revision changed');
    await expect(loader.read('p', 's', run, [summary])).resolves.toBe('Retry result');
    await expect(loader.read('p', 's', run, [])).rejects.toThrow('not available');
    expect(ipc.open).toHaveBeenCalledTimes(2);
  });
});
