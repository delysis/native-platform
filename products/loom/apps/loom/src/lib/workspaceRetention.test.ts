import { describe, expect, it } from 'vitest';
import { visibleWorkspaceDocuments, readPinnedOutputs, rememberPinnedOutputs } from './workspaceRetention';
import { workspaceRows } from './workspaceTree';
import type { DocumentSummary } from './types';
const doc = (id: string, path: string): DocumentSummary => ({ document_id: id, relative_path: path, title: path.split('/').at(-1)!, kind: 'prose', revision_id: 'revision', active_blob_id: 'blob', word_count: 1, externally_modified: false });
describe('retained output navigation', () => {
  it('hides retained identities and empty generated folders while preserving user files regardless of directory name', () => {
    const documents = [doc('draft', 'Draft.md'), doc('generated', 'Runs/123/Answer.md'), doc('user', 'Runs/My own note.md')];
    const visible = visibleWorkspaceDocuments(documents, ['generated'], new Set());
    expect(visible.map(item => item.document_id)).toEqual(['draft', 'user']);
    const rows = workspaceRows(visible, new Set(), '');
    expect(rows.some(row => row.path === 'Runs/')).toBe(true);
    expect(rows.some(row => row.path === 'Runs/123/')).toBe(false);
    expect(workspaceRows(visible.filter(item => item.document_id !== 'user'), new Set(), '').map(row => row.path)).toEqual(['Draft.md']);
  });
  it('pins retain navigation intent only, survive reopening, and do not duplicate documents', () => {
    const values = new Map<string, string>();
    const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
    rememberPinnedOutputs(storage, 'project', new Set(['generated']));
    const documents = [doc('generated', 'Runs/Answer.md')];
    expect(visibleWorkspaceDocuments(documents, ['generated'], readPinnedOutputs(storage, 'project'))).toEqual(documents);
    expect(visibleWorkspaceDocuments(documents, ['generated'], readPinnedOutputs(storage, 'other'))).toEqual([]);
  });
});
