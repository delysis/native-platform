import { describe, expect, it } from 'vitest';
import { workspaceRows } from './workspaceTree';
import type { DocumentSummary } from './types';
import type { MaterialEntry } from './materials';

const document = (path: string): DocumentSummary => ({ document_id: path, relative_path: path, title: path.split('/').at(-1)!, kind: 'prose', revision_id: 'revision', active_blob_id: 'blob', word_count: 1, externally_modified: false });
const material = (id: string, path: string | null, name: string): MaterialEntry => ({ id, name, reference: '@"source"', kind: 'attachment', pinned: false, available: true, source_path: null, attachment_id: id, workspace_path: path });
const documents = [document('Notes/Draft.md'), document('Notes/Clippings/Thought.md'), document('Writing.md')];
const paper = material('paper', 'Notes/Clippings/Paper.pdf', 'Research paper');
const photo = material('photo', 'Notes/Photo.png', 'Photograph');
const external = material('external', null, 'Private external library');

describe('workspace physical material navigation', () => {
  it('shows empty physical directories and merges them with populated paths without duplicate rows', () => {
    const directories = ['Empty', 'Empty/Nested', 'Notes', 'Notes/Clippings'];
    expect(workspaceRows(documents, new Set(), '', [paper], directories).map(row => row.path)).toEqual([
      'Empty/', 'Empty/Nested/', 'Notes/', 'Notes/Clippings/', 'Notes/Clippings/Paper.pdf',
      'Notes/Clippings/Thought.md', 'Notes/Draft.md', 'Writing.md'
    ]);
    expect(workspaceRows([], new Set(['Empty/']), '', [], directories).map(row => row.path)).toEqual([
      'Empty/', 'Notes/', 'Notes/Clippings/'
    ]);
    expect(workspaceRows([], new Set(['Empty/']), 'nested', [], directories).map(row => row.path)).toEqual([
      'Empty/', 'Empty/Nested/'
    ]);
    expect(workspaceRows([], new Set(), 'missing', [], directories)).toEqual([]);
  });

  it('places copied material in its real folder alongside writing without inventing a material section', () => {
    const rows = workspaceRows(documents, new Set(), '', [paper, photo, external]);
    expect(rows.map(row => [row.path, row.depth])).toEqual([
      ['Notes/', 0], ['Notes/Clippings/', 1], ['Notes/Clippings/Paper.pdf', 2],
      ['Notes/Clippings/Thought.md', 2], ['Notes/Draft.md', 1], ['Notes/Photo.png', 1], ['Writing.md', 0]
    ]);
    expect(rows.filter(row => 'material' in row).map(row => row.material)).toEqual([paper, photo]);
    expect(rows.filter(row => 'document' in row).map(row => row.document)).toHaveLength(3);
    expect(rows.some(row => row.path.includes('external'))).toBe(false);
  });

  it('collapses mixed descendants as one physical folder and retains the sibling rows', () => {
    const rows = workspaceRows(documents, new Set(['Notes/Clippings/']), '', [paper, photo]);
    expect(rows.map(row => row.path)).toEqual(['Notes/', 'Notes/Clippings/', 'Notes/Draft.md', 'Notes/Photo.png', 'Writing.md']);
    expect(workspaceRows(documents, new Set(['Notes/']), '', [paper, photo]).map(row => row.path)).toEqual(['Notes/', 'Writing.md']);
  });

  it('searches material names and physical paths through collapsed folders but leaves unplaced sources separate', () => {
    const collapsed = new Set(['Notes/', 'Notes/Clippings/']);
    const named = workspaceRows(documents, collapsed, 'research', [paper, photo, external]);
    expect(named.map(row => row.path)).toEqual(['Notes/', 'Notes/Clippings/', 'Notes/Clippings/Paper.pdf']);
    const byPath = workspaceRows(documents, collapsed, 'clippings', [paper, photo, external]);
    expect(byPath.map(row => row.path)).toEqual(['Notes/', 'Notes/Clippings/', 'Notes/Clippings/Paper.pdf', 'Notes/Clippings/Thought.md']);
    expect(workspaceRows(documents, collapsed, 'private external', [external])).toEqual([]);
    expect(collapsed).toEqual(new Set(['Notes/', 'Notes/Clippings/']));
    expect(external.workspace_path).toBeNull();
  });
});
