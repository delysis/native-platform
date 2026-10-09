/** Local navigation bookmarks. A root string is never filesystem authority. */
export interface WorkspaceFolder { readonly root: string; readonly title: string }
const KEY = 'loom.workspace-folders';
const LIMIT = 32;
const MAX_PATH_LENGTH = 4096;

function validFolder(value: unknown): value is WorkspaceFolder {
  if (!value || typeof value !== 'object') return false;
  const item = value as Partial<WorkspaceFolder>;
  return typeof item.root === 'string' && item.root.length > 0 && item.root.length <= MAX_PATH_LENGTH &&
    typeof item.title === 'string' && item.title.length > 0 && item.title.length <= MAX_PATH_LENGTH;
}

// Reading neither probes the filesystem nor writes/prunes missing/offline roots.
// The current root/title representation is unchanged; labels are a projection,
// not a schema compatibility reader or a migration of project sidecars.
export function readWorkspaceFolders(storage: Pick<Storage, 'getItem'>): WorkspaceFolder[] {
  try {
    const value: unknown = JSON.parse(storage.getItem(KEY) ?? '[]');
    if (!Array.isArray(value)) return [];
    const roots = new Set<string>();
    return value.filter((item): item is WorkspaceFolder => {
      if (!validFolder(item) || roots.has(item.root)) return false;
      roots.add(item.root); return true;
    }).slice(-LIMIT).map(item => Object.freeze({ root: item.root, title: item.title }));
  } catch { return []; }
}

function persist(folders: WorkspaceFolder[], storage?: Pick<Storage, 'setItem'>): WorkspaceFolder[] {
  // An explicit edit must not look successful and resurrect on the next launch.
  // Let the owner report denied/quota failures before publishing the new array.
  storage?.setItem(KEY, JSON.stringify(folders));
  return folders;
}

export function rememberWorkspaceFolder(
  folders: readonly WorkspaceFolder[], folder: WorkspaceFolder, storage?: Pick<Storage, 'setItem'>
): WorkspaceFolder[] {
  if (!validFolder(folder)) throw new Error('Invalid workspace bookmark.');
  // Reopening is not permission to overwrite the author's sidebar label.
  const next = folders.some(item => item.root === folder.root)
    ? [...folders] : [...folders, Object.freeze({ ...folder })].slice(-LIMIT);
  return persist(next, storage);
}

export function forgetWorkspaceFolder(
  folders: readonly WorkspaceFolder[], root: string, storage: Pick<Storage, 'setItem'>
): WorkspaceFolder[] {
  return persist(folders.filter(item => item.root !== root), storage);
}

export function renameWorkspaceFolder(
  folders: readonly WorkspaceFolder[], root: string, title: string, storage: Pick<Storage, 'setItem'>
): WorkspaceFolder[] {
  const label = title.trim();
  if (!label || new TextEncoder().encode(label).length > 256 || /[\u0000-\u001f\u007f]/u.test(label)) {
    throw new Error('Use a nonempty sidebar label of at most 256 UTF-8 bytes without control characters.');
  }
  if (!folders.some(item => item.root === root)) throw new Error('This workspace bookmark was removed.');
  return persist(folders.map(item => item.root === root ? Object.freeze({ root, title: label }) : item), storage);
}

/** Full paths remain tooltips; the old title===root projection is not a label. */
export function workspaceFolderName(folder: WorkspaceFolder): string {
  return folder.title === folder.root
    ? folder.root.split(/[\\/]/u).filter(Boolean).at(-1) ?? folder.root
    : folder.title;
}

/** Shortest distinguishing ancestor suffix; exact spellings still own identity. */
export function workspaceFolderLabels(folders: readonly WorkspaceFolder[]): ReadonlyMap<string, string> {
  const labels = new Map<string, string>();
  const groups = new Map<string, WorkspaceFolder[]>();
  for (const folder of folders) {
    const name = workspaceFolderName(folder);
    const peers = groups.get(name) ?? [];
    peers.push(folder); groups.set(name, peers);
  }
  for (const [name, peers] of groups) {
    if (peers.length === 1) { labels.set(peers[0].root, name); continue; }
    const parts = peers.map(folder => folder.root.split(/[\\/]/u).filter(Boolean).slice(0, -1));
    for (let index = 0; index < peers.length; index++) {
      let suffix = '';
      for (let depth = 1; depth <= Math.max(1, parts[index].length); depth++) {
        suffix = parts[index].slice(-depth).join('/') || peers[index].root;
        if (parts.every((other, otherIndex) => otherIndex === index || other.slice(-depth).join('/') !== suffix)) break;
      }
      // Distinct spellings can have identical components (e.g. a trailing slash).
      // Use a stable ordinal rather than canonicalizing/collapsing either root.
      const label = `${name} — ${suffix}`;
      labels.set(peers[index].root, label);
    }
  }
  const repeated = new Map<string, string[]>();
  for (const [root, label] of labels) {
    const roots = repeated.get(label) ?? []; roots.push(root); repeated.set(label, roots);
  }
  const used = new Set(labels.values());
  for (const [label, roots] of repeated) {
    if (roots.length < 2) continue;
    let ordinal = 0;
    for (const root of roots.sort()) {
      let unique: string;
      do { unique = `${label} · ${++ordinal}`; } while (used.has(unique));
      labels.set(root, unique); used.add(unique);
    }
  }
  return labels;
}

/** A forgotten active bookmark must not strand its live document tree or reappear. */
export function workspaceFolderGroups(
  folders: readonly WorkspaceFolder[], activeRoot: string | null
): readonly (WorkspaceFolder | null)[] {
  return activeRoot && !folders.some(folder => folder.root === activeRoot) ? [...folders, null] : folders;
}
