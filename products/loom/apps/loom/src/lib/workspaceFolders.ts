export interface WorkspaceFolder { root: string; title: string }
const KEY = 'loom.workspace-folders';
const LIMIT = 32;
const MAX_PATH_LENGTH = 4096;

// A root's label is its exact path, not an ambiguous basename. This is only a
// navigation projection: never canonicalize aliases, strip real subdirectories,
// or change a document's native project-relative path to tidy these labels.
function pathLabel(folder: WorkspaceFolder): WorkspaceFolder {
  return { root: folder.root, title: folder.root };
}

// Remembered paths are navigation hints, never filesystem authority. Native
// preparation reopens and validates the directory before replacing a session.
export function readWorkspaceFolders(storage: Pick<Storage, 'getItem'>): WorkspaceFolder[] {
  try {
    const value: unknown = JSON.parse(storage.getItem(KEY) ?? '[]');
    if (!Array.isArray(value)) return [];
    const roots = new Set<string>();
    return value.filter((item): item is WorkspaceFolder => {
      if (!item || typeof item !== 'object' || typeof item.root !== 'string' || typeof item.title !== 'string' ||
        !item.root || item.root.length > MAX_PATH_LENGTH || !item.title || item.title.length > MAX_PATH_LENGTH || roots.has(item.root)) return false;
      roots.add(item.root); return true;
    }).slice(-LIMIT).map(pathLabel);
  } catch { return []; }
}

export function rememberWorkspaceFolder(folders: WorkspaceFolder[], folder: WorkspaceFolder, storage?: Pick<Storage, 'setItem'>): WorkspaceFolder[] {
  const next = (folders.some(item => item.root === folder.root)
    ? folders.map(item => item.root === folder.root ? folder : item) : [...folders, folder])
    .slice(-LIMIT).map(pathLabel);
  try { storage?.setItem(KEY, JSON.stringify(next)); } catch { /* Navigation still works without persistence. */ }
  return next;
}
