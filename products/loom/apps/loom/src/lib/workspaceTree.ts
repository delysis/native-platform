import type { DocumentSummary } from './types';
import type { MaterialEntry } from './materials';
export type WorkspaceRow = { path: string; depth: number; folder: true; title: string } | { path: string; depth: number; folder: false; document: DocumentSummary } | { path: string; depth: number; folder: false; material: MaterialEntry };
type Entry = { relative_path: string; title: string } & ({ document: DocumentSummary } | { material: MaterialEntry } | { directory: true });

/** Only the active tree grants a destination; another root must be opened first. */
export function workspaceCopyDestination(element: Element | null, outline: HTMLElement | undefined): string | null {
  if (!element || !outline?.contains(element)) return null;
  const folder = element.closest<HTMLElement>('[data-copy-folder]');
  if (folder && outline.contains(folder)) return folder.dataset.copyFolder ?? null;
  if (element.closest('.workspace-root')) return null;
  return '';
}

export function workspaceRows(documents: DocumentSummary[], collapsed: Set<string>, query: string, materials: MaterialEntry[] = [], directories: string[] = []): WorkspaceRow[] {
  const rows: WorkspaceRow[] = [];
  const needle = query.trim().toLocaleLowerCase();
  const entries: Entry[] = [
    ...directories.map(path => ({ relative_path: `${path.replace(/\/$/, '')}/`, title: path.split('/').filter(Boolean).at(-1) ?? path, directory: true as const })),
    ...documents.map(document => ({ relative_path: document.relative_path, title: document.title, document })),
    ...materials.filter(material => material.workspace_path).map(material => ({ relative_path: material.workspace_path!, title: material.name, material }))
  ];
  const selected = entries.filter(d => !needle || `${d.title}\n${d.relative_path}`.toLocaleLowerCase().includes(needle));
  const walk = (prefix: string, depth: number, members: Entry[]) => {
    const folders = new Map<string, Entry[]>();
    const leaves: Entry[] = [];
    for (const doc of members) {
      const rest = doc.relative_path.slice(prefix.length);
      if (!rest) continue;
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
    for (const entry of leaves) {
      if ('document' in entry) rows.push({ path: entry.relative_path, depth, folder: false, document: entry.document });
      else if ('material' in entry) rows.push({ path: entry.relative_path, depth, folder: false, material: entry.material });
    }
  };
  walk('', 0, selected);
  return rows;
}
