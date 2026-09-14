import type { DocumentSummary } from './types';
export type WorkspaceRow = { path: string; depth: number; folder: true; title: string } | { path: string; depth: number; folder: false; document: DocumentSummary };
export function workspaceRows(documents: DocumentSummary[], collapsed: Set<string>, query: string): WorkspaceRow[] {
  const rows: WorkspaceRow[] = [];
  const needle = query.trim().toLocaleLowerCase();
  const selected = documents.filter(d => !needle || `${d.title}\n${d.relative_path}`.toLocaleLowerCase().includes(needle));
  const walk = (prefix: string, depth: number, members: DocumentSummary[]) => {
    const folders = new Map<string, DocumentSummary[]>();
    const leaves: DocumentSummary[] = [];
    for (const doc of members) {
      const rest = doc.relative_path.slice(prefix.length);
      const slash = rest.indexOf('/');
      if (slash < 0) leaves.push(doc);
      else { const name = rest.slice(0, slash); const group = folders.get(name) ?? []; group.push(doc); folders.set(name, group); }
    }
    for (const [title, group] of [...folders].sort(([a], [b]) => a.localeCompare(b))) {
      const path = `${prefix}${title}/`;
      rows.push({ path, depth, folder: true, title });
      if (needle || !collapsed.has(path)) walk(path, depth + 1, group);
    }
    leaves.sort((a, b) => Number(a.relative_path.slice(prefix.length).startsWith('.')) - Number(b.relative_path.slice(prefix.length).startsWith('.')) || a.title.localeCompare(b.title));
    for (const document of leaves) rows.push({ path: document.relative_path, depth, folder: false, document });
  };
  walk('', 0, selected);
  return rows;
}
