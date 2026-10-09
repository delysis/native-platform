import { captureDocumentTarget, capturedDocumentIdentityIsCurrent } from './documentContextActions';
import type { CapturedDocumentTarget, DocumentContextAction } from './documentContextActions';
import type { DocumentSummary, ProjectSnapshot } from './types';
import type { MaterialEntry } from './materials';
import type { WorkspaceFolder } from './workspaceFolders';
import { workspaceFolderName } from './workspaceFolders';
import { isContextMenuTriggerKey } from './interactionPrimitives';
import { compositionOwnsKey } from './textEditingInteractions';

export interface SidebarScope { readonly projectId: string; readonly sessionId: string }
export type SidebarItem =
  | { readonly kind: 'root'; readonly bookmark: WorkspaceFolder }
  | { readonly kind: 'folder'; readonly path: string; readonly title: string }
  | { readonly kind: 'document'; readonly summary: DocumentSummary }
  | { readonly kind: 'material'; readonly material: MaterialEntry };
interface TargetBase {
  readonly scope: SidebarScope;
  readonly key: string;
  readonly title: string;
  /** Copy/tooltip only. Never passed to a native mutation command. */
  readonly displayPath: string | null;
}
export type CapturedSidebarTarget = TargetBase & (
  | { readonly kind: 'root'; readonly bookmark: WorkspaceFolder }
  | { readonly kind: 'folder'; readonly path: string }
  | { readonly kind: 'document'; readonly summary: DocumentSummary; readonly document: CapturedDocumentTarget | null }
  | { readonly kind: 'material'; readonly material: Readonly<MaterialEntry>; readonly materialLease: MaterialEntry }
);
export type RootSidebarTarget = Extract<CapturedSidebarTarget, { kind: 'root' }>;
export interface SidebarLiveState {
  readonly project: ProjectSnapshot | null;
  readonly bookmarks: readonly WorkspaceFolder[];
  readonly folders: readonly string[];
  readonly materials: readonly MaterialEntry[];
}

export function workspaceDisplayPath(root: string, relative: string): string {
  return `${root}${/[\\/]$/u.test(root) ? '' : '/'}${relative}`;
}

/** Identity does not use labels, basename guesses or renderer canonicalization. */
export function sidebarItemKey(scope: SidebarScope, item: SidebarItem): string {
  switch (item.kind) {
    case 'root': return JSON.stringify(['root', item.bookmark.root]);
    case 'folder': return JSON.stringify(['folder', scope.projectId, scope.sessionId, item.path]);
    case 'document': return JSON.stringify(['document', scope.projectId, scope.sessionId, item.summary.document_id]);
    case 'material': return JSON.stringify(['material', scope.projectId, scope.sessionId, item.material.id]);
  }
}

export function captureSidebarTarget(project: ProjectSnapshot, item: SidebarItem): CapturedSidebarTarget {
  const scope = Object.freeze({ projectId: project.project_id, sessionId: project.session_id });
  const base = { scope, key: sidebarItemKey(scope, item) };
  switch (item.kind) {
    case 'root': return Object.freeze({ ...base, ...item, title: workspaceFolderName(item.bookmark), displayPath: item.bookmark.root });
    case 'folder': return Object.freeze({ ...base, ...item, displayPath: workspaceDisplayPath(project.root, item.path) });
    case 'document': return Object.freeze({ ...base, kind: item.kind, summary: Object.freeze({ ...item.summary }),
      document: captureDocumentTarget(project, item.summary), title: item.summary.title,
      displayPath: workspaceDisplayPath(project.root, item.summary.relative_path) });
    case 'material': return Object.freeze({ ...base, kind: item.kind, material: Object.freeze({ ...item.material }), materialLease: item.material, title: item.material.name,
      displayPath: item.material.source_path ?? (item.material.workspace_path ? workspaceDisplayPath(project.root, item.material.workspace_path) : null) });
  }
}

export function sidebarScopeIsCurrent(scope: SidebarScope, project: ProjectSnapshot | null): boolean {
  return project?.project_id === scope.projectId && project.session_id === scope.sessionId;
}

export function sidebarTargetIsCurrent(target: CapturedSidebarTarget, live: SidebarLiveState): boolean {
  if (!sidebarScopeIsCurrent(target.scope, live.project)) return false;
  switch (target.kind) {
    // Object identity rejects remove/re-add ABA and a superseded inline label.
    case 'root': return live.bookmarks.some(bookmark => bookmark === target.bookmark);
    case 'folder': return live.folders.includes(target.path);
    case 'document': return target.document !== null
      ? capturedDocumentIdentityIsCurrent(target.document, live.project)
      : Boolean(live.project?.documents.some(summary => summary.document_id === target.summary.document_id &&
          summary.revision_id === target.summary.revision_id && summary.active_blob_id === target.summary.active_blob_id));
    case 'material': {
      const current = live.materials.find(material => material.id === target.material.id);
      return current === target.materialLease && current !== undefined && current.reference === target.material.reference &&
        current.source_path === target.material.source_path && current.workspace_path === target.material.workspace_path &&
        current.attachment_id === target.material.attachment_id && current.kind === target.material.kind &&
        current.retention === target.material.retention && current.name === target.material.name && current.pinned === target.material.pinned;
    }
  }
}

type RootAction = 'open' | 'toggle' | 'rename_label' | 'forget' | 'copy_path';
type FolderAction = 'toggle' | 'copy_path';
type MaterialAction = 'open' | 'pin' | 'remove' | 'copy_reference' | 'copy_path';
interface Capability<T, A> { readonly target: T; readonly action: A; readonly label: string; readonly enabled: boolean; readonly destructive?: boolean }
export type SidebarCapability =
  | Capability<RootSidebarTarget, RootAction>
  | Capability<Extract<CapturedSidebarTarget, { kind: 'folder' }>, FolderAction>
  | Capability<Extract<CapturedSidebarTarget, { kind: 'document' }>, DocumentContextAction | 'copy_path'>
  | Capability<Extract<CapturedSidebarTarget, { kind: 'material' }>, MaterialAction>;
export interface SidebarCapabilityState {
  readonly idle: boolean;
  readonly editable: boolean;
  readonly activeRoot: boolean;
  readonly expanded: boolean;
  readonly revealLabel: string | null;
  readonly searching: boolean;
}

/** Exhaustive capabilities, not guessed filesystem permissions. */
export function sidebarCapabilities(target: CapturedSidebarTarget, state: SidebarCapabilityState): readonly SidebarCapability[] {
  const { idle, editable, expanded } = state;
  switch (target.kind) {
    case 'root': return [
      { target, action: state.activeRoot ? 'toggle' : 'open', label: state.activeRoot ? expanded ? 'Collapse' : 'Expand' : 'Open', enabled: idle },
      { target, action: 'rename_label', label: 'Rename Sidebar Label…', enabled: idle },
      { target, action: 'copy_path', label: 'Copy Path', enabled: idle },
      { target, action: 'forget', label: 'Remove from Sidebar', enabled: idle }
    ];
    case 'folder': return [
      { target, action: 'toggle', label: expanded ? 'Collapse' : 'Expand', enabled: idle && !state.searching },
      { target, action: 'copy_path', label: 'Copy Path', enabled: idle }
    ];
    case 'document': return [
      { target, action: 'open', label: 'Open', enabled: idle && editable && target.document !== null },
      { target, action: 'rename', label: 'Rename…', enabled: idle && editable && target.document !== null },
      { target, action: 'export_text', label: 'Export Text…', enabled: idle && target.document !== null },
      ...(state.revealLabel ? [{ target, action: 'reveal' as const, label: state.revealLabel, enabled: idle && target.document !== null }] : []),
      { target, action: 'copy_path', label: 'Copy Path', enabled: idle },
      { target, action: 'delete', label: 'Delete Manuscript…', enabled: idle && editable && target.document !== null, destructive: true }
    ];
    case 'material': return [
      { target, action: 'open', label: 'Open', enabled: idle && editable },
      { target, action: 'pin', label: target.material.pinned ? 'Unpin' : 'Pin', enabled: idle },
      { target, action: 'copy_reference', label: 'Copy Reference', enabled: idle },
      ...(target.displayPath ? [{ target, action: 'copy_path' as const, label: 'Copy Path', enabled: idle }] : []),
      { target, action: 'remove', label: 'Remove Source from Workspace', enabled: idle && target.material.retention !== 'protected' }
    ];
  }
}

export type SidebarKeyAction = 'context' | 'rename' | 'open' | 'remove' | 'previous' | 'next' | 'first' | 'last' | 'expand' | 'collapse' | 'none';
export function sidebarKeyAction(event: Pick<KeyboardEvent, 'key' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey' | 'isComposing' | 'keyCode'>): SidebarKeyAction {
  if (compositionOwnsKey(event)) return 'none';
  if (isContextMenuTriggerKey(event)) return 'context';
  if (!event.altKey && !event.shiftKey && (event.metaKey || event.ctrlKey)) {
    if (event.key === 'ArrowDown' || event.key.toLowerCase() === 'o') return 'open';
    if (event.key === 'Backspace') return 'remove';
  }
  if (event.altKey || event.shiftKey || event.metaKey || event.ctrlKey) return 'none';
  switch (event.key) {
    case 'Enter': case 'F2': return 'rename';
    case ' ': return 'open';
    case 'Delete': return 'remove';
    case 'ArrowUp': return 'previous';
    case 'ArrowDown': return 'next';
    case 'Home': return 'first';
    case 'End': return 'last';
    case 'ArrowRight': return 'expand';
    case 'ArrowLeft': return 'collapse';
    default: return 'none';
  }
}
