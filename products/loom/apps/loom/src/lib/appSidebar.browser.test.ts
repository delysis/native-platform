import { mount, tick, unmount } from 'svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { OpenDocument, ProjectSnapshot } from './types';
import type { MaterialEntry } from './materials';
import { workspaceCopyDestination } from './workspaceTree';
import '../app.css';

// Real App, sidebar, editors and owning IPC. Only external native transport is
// injected. These cases do not establish native filesystem or signed acceptance.
const transport = vi.hoisted(() => ({ invoke: vi.fn(), listeners: new Map<string, Set<(event: { payload: unknown }) => void>>() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: transport.invoke, convertFileSrc: (path: string) => path }));
vi.mock('@tauri-apps/api/event', () => ({ listen: async (name: string, handler: (event: { payload: unknown }) => void) => {
  const handlers = transport.listeners.get(name) ?? new Set(); handlers.add(handler); transport.listeners.set(name, handlers);
  return () => { handlers.delete(handler); };
} }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({
  onFocusChanged: async () => () => {}, onDragDropEvent: async () => () => {}, onResized: async () => () => {},
  isFullscreen: async () => false, setTitle: async () => {}
}) }));
import App from '../App.svelte';

let mounted: ReturnType<typeof mount> | null = null;
let oldStorage: Array<[string, string]> = [];
let oldNative: PropertyDescriptor | undefined;
afterEach(async () => {
  if (mounted) await unmount(mounted); mounted = null;
  transport.listeners.clear(); transport.invoke.mockReset();
  if (oldNative) Object.defineProperty(window, '__TAURI_INTERNALS__', oldNative); else Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
  localStorage.clear(); for (const [key, value] of oldStorage) localStorage.setItem(key, value);
  document.body.replaceChildren();
});
const KEY = 'loom.workspace-folders';
const activeRoot = '/selected/writing';
const missingRoot = '/private/var/folders/unavailable/writing';
function rootRow(root: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll<HTMLButtonElement>('[data-sidebar-kind="root"]')].find(row => row.title === root);
}
function key(target: HTMLElement, key: string, overrides: KeyboardEventInit = {}): KeyboardEvent {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...overrides }); target.dispatchEvent(event); return event;
}
async function context(row: HTMLElement): Promise<MouseEvent> {
  const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 120, clientY: 160 });
  row.dispatchEvent(event); await tick();
  await expect.element(page.getByRole('menu')).toBeVisible();
  return event;
}
async function setup(emptyProject = false, placement: string | null = 'Notes/Paper.pdf', allowedDraft: string | null = null) {
  oldStorage = Object.entries(localStorage); localStorage.clear();
  localStorage.setItem(KEY, JSON.stringify([{ root: activeRoot, title: activeRoot }, { root: missingRoot, title: missingRoot }]));
  oldNative = Object.getOwnPropertyDescriptor(window, '__TAURI_INTERNALS__');
  Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {} });
  const text = 'Original manuscript';
  const blob = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text))), byte => byte.toString(16).padStart(2, '0')).join('');
  const opened: OpenDocument = { summary: { document_id: 'document-1', relative_path: 'Draft.md', title: 'Draft', kind: 'prose',
    revision_id: 'revision-1', active_blob_id: blob, word_count: 2, externally_modified: false }, text, visible_blob_id: blob, transient_draft: null };
  const project: ProjectSnapshot = { project_id: 'project-1', session_id: 'session-1', root: activeRoot, title: 'writing',
    schema_version: 4, documents: emptyProject ? [] : [opened.summary], pending_recovery: 0 };
  const materials: MaterialEntry[] = [{ id: 'material-' + 'a'.repeat(64), name: 'Paper.pdf', reference: '@"material-' + 'a'.repeat(64) + '"', kind: 'attachment', retention: 'ordinary',
    pinned: false, available: true, source_path: '/external/Paper.pdf', workspace_path: placement, attachment_id: 'b'.repeat(64), metadata_revision: 'd'.repeat(64) }];
  let draftVersion = 0;
  const calls: Array<{ command: string; args: Record<string, unknown> }> = [];
  transport.invoke.mockImplementation(async (command: string, args: Record<string, unknown> = {}) => {
    calls.push({ command, args });
    switch (command) {
      case 'plugin:loom|preferences_get': return { revision: '0', last_local_model: null, project_suggestions: {} };
      case 'plugin:loom|application_close_pending': return false;
      case 'plugin:loom|workspace_chat_route': return 'loom';
      case 'plugin:loom|project_current': return project;
      case 'plugin:loom|workspace_roots_get': return { workspace_id: project.project_id, workspace_session_id: 'workspace-session', roots: [{ id: 'owner', name: project.title, owner: true, available: true, path: project.root, project_id: project.project_id }] };
      case 'plugin:loom|workspace_pane_list': return [];
      case 'plugin:loom|document_open': return opened;
      case 'plugin:loom|document_context_list': return { markdown: '', attachments: [], materials: [], revision: 'context-1' };
      case 'plugin:loom|workspace_template_get': return { enabled: true, document_id: null, revision_id: null, error: null,
        config: { panes: { chat: { kind: 'chat', position: 'right', visible: true, title: null, document: null, context: [] } } } };
      case 'plugin:loom|build_model_policy_get': return null;
      case 'plugin:loom|model_catalog_list': case 'plugin:loom|model_list': case 'plugin:loom|model_download_list':
      case 'plugin:loom|co_writer_list': case 'plugin:loom|terminal_list': return [];
      case 'plugin:loom|material_list': return materials.map(item => ({ ...item }));
      case 'plugin:loom|material_read': return { material: { ...materials[0] }, text: 'Retained source text', complete: true, warnings: [], source_revision: 'source-revision', evidence: [], presentation: null };
      case 'plugin:loom|material_remove': {
        expect(args.id).toBe(materials[0].id); expect(args.expectedMetadataRevision).toBe(materials[0].metadata_revision);
        materials.splice(0, 1); return;
      }
      case 'plugin:loom|material_rename': {
        expect(args.projectId).toBe(project.project_id); expect(args.sessionId).toBe('workspace-session');
        expect(args.id).toBe(materials[0].id); expect(args.expectedMetadataRevision).toBe(materials[0].metadata_revision);
        const previous = materials[0];
        materials[0] = { ...previous, name: String(args.name), metadata_revision: 'e'.repeat(64) };
        return { project_id: project.project_id, session_id: 'workspace-session', request_id: args.requestId,
          expected_metadata_revision: args.expectedMetadataRevision, material: materials[0] };
      }
      case 'plugin:loom|inference_status': return { suggestions: null };
      case 'plugin:loom|completion_snapshot': return { project_id: project.project_id, session_id: project.session_id,
        document_id: opened.summary.document_id, branches: [], active_operations: [], next_cursor: null, has_more: false };
      case 'plugin:loom|suggestions_set': case 'plugin:loom|focus_mode_set': return;
      case 'plugin:loom|project_prepare_open_path': throw { code: 'selected_folder_unavailable', message: 'Selected folder is unavailable' };
      // Existing dirty-editor cases keep timer-driven persistence pending. Only
      // the exact authorized edit below receives a mock transport receipt.
      case 'plugin:loom|document_draft_upsert': {
        if (allowedDraft === null) return new Promise(() => {});
        // Same exact-byte draft transport fixture as appCompletion.browser.test.
        // It journals only the explicitly authorized user edit, never a rename.
        expect(args.text).toBe(allowedDraft); expect(args.sourceRevisionId).toBe(opened.summary.revision_id);
        const hash = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(allowedDraft))), byte => byte.toString(16).padStart(2, '0')).join('');
        return { document_id: args.documentId, source_revision_id: args.sourceRevisionId, blob_id: hash,
          version: String(++draftVersion), kind: args.kind, updated_at_unix_ms: Date.now(), replayed: false };
      }
      default: throw new Error(`Unexpected native operation: ${command}`);
    }
  });
  const host = document.createElement('div'); document.body.append(host); mounted = mount(App, { target: host });
  await expect.poll(() => rootRow(missingRoot)?.isConnected).toBe(true);
  if (!emptyProject) {
    await expect.poll(() => document.querySelector('.ProseMirror')?.textContent).toBe(text);
    await expect.element(page.getByRole('textbox', { name: 'Message', exact: true })).toBeVisible();
  }
  await page.getByRole('button', { name: 'Open manuscript outline', exact: true }).click();
  return { calls, project, opened, materials };
}

it('missing bookmark has readable disambiguated names, exact path tooltip and a handled remove menu without native calls', async () => {
  const { calls } = await setup(); const row = rootRow(missingRoot)!;
  expect(row.textContent).toContain('writing — unavailable'); expect(row.textContent).not.toContain('/private/var/folders');
  expect(rootRow(activeRoot)!.textContent).toContain('writing — selected'); expect(row.title).toBe(missingRoot);
  const before = calls.length; expect((await context(row)).defaultPrevented).toBe(true);
  await page.getByRole('menuitem', { name: 'Remove from Sidebar', exact: true }).click();
  await expect.poll(() => rootRow(missingRoot)).toBeUndefined();
  expect(JSON.parse(localStorage.getItem(KEY)!)).toEqual([{ root: activeRoot, title: activeRoot }]);
  expect(calls.slice(before)).toEqual([]);
});

it('single-click selects without opening; a failed double-click open retains editor and bookmark, which remains removable', async () => {
  const { calls } = await setup(); const row = rootRow(missingRoot)!; const editor = document.querySelector('.ProseMirror');
  const before = calls.length; row.click(); await tick();
  expect(row.getAttribute('aria-current')).toBe('true'); expect(calls.slice(before)).toEqual([]);
  row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
  await expect.poll(() => document.body.textContent).toContain('Selected folder is unavailable');
  expect(document.querySelector('.ProseMirror')).toBe(editor); expect(rootRow(missingRoot)).toBe(row);
  expect(calls.slice(before).map(call => call.command)).toEqual(['plugin:loom|project_prepare_open_path']);
  expect(calls.at(-1)!.args).toEqual({ path: missingRoot });
  await context(row); await page.getByRole('menuitem', { name: 'Remove from Sidebar', exact: true }).click();
  await expect.poll(() => rootRow(missingRoot)).toBeUndefined();
});

it('forgetting active root retains unsaved document, the original chat composer and a reachable document tree', async () => {
  const { calls } = await setup();
  const editor = document.querySelector<HTMLElement>('.ProseMirror')!, composer = page.getByRole('textbox', { name: 'Message', exact: true }).element();
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Unsent chat draft');
  editor.focus(); await userEvent.keyboard('{End} plus unsaved words');
  await expect.poll(() => editor.textContent).toContain('plus unsaved words');
  const before = calls.length;
  await context(rootRow(activeRoot)!); await page.getByRole('menuitem', { name: 'Remove from Sidebar', exact: true }).click();
  await expect.poll(() => rootRow(activeRoot)).toBeUndefined();
  expect(document.querySelector('.ProseMirror')).toBe(editor); expect(editor.textContent).toContain('plus unsaved words');
  expect(page.getByRole('textbox', { name: 'Message', exact: true }).element()).toBe(composer);
  await expect.element(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('Unsent chat draft');
  expect(document.querySelector('[data-document-row="document-1"]')).not.toBeNull();
  expect(calls.slice(before).some(call => /project_(close|prepare|commit)|document_(checkpoint|delete|rename)/.test(call.command))).toBe(false);
  expect(JSON.parse(localStorage.getItem(KEY)!).some((item: { root: string }) => item.root === activeRoot)).toBe(false);
});

it('all rendered row kinds suppress WebKit default context menus and expose only defined capabilities', async () => {
  await setup();
  for (const kind of ['root', 'folder', 'document', 'material']) {
    const row = document.querySelector<HTMLButtonElement>(`[data-sidebar-kind="${kind}"]`)!; expect(row).not.toBeNull();
    expect((await context(row)).defaultPrevented).toBe(true);
    expect(document.querySelectorAll('[role="menuitem"]').length).toBeGreaterThan(0);
    const labels = [...document.querySelectorAll('[role="menuitem"]')].map(item => item.textContent);
    if (kind === 'folder') expect(labels).toEqual(['Collapse', 'Copy Path']);
    if (kind === 'material') expect(labels).toContain('Remove Source from Workspace');
    if (kind === 'document') expect(labels.at(-1)).toBe('Delete Manuscript…');
    key(document.querySelector<HTMLElement>('[role="menu"]')!, 'Escape'); await tick();
  }
});

it('ContextMenu and Shift-F10 select/focus a menu; Escape restores the exact row and native input menus remain unclaimed', async () => {
  await setup();
  for (const event of [{ key: 'ContextMenu' }, { key: 'F10', shiftKey: true }]) {
    const row = rootRow(missingRoot)!; row.focus(); const pressed = key(row, event.key, event);
    expect(pressed.defaultPrevented).toBe(true);
    await expect.element(page.getByRole('menuitem', { name: 'Open', exact: true })).toHaveFocus();
    key(document.querySelector<HTMLElement>('[role="menu"]')!, 'Escape');
    await expect.poll(() => document.activeElement).toBe(row);
  }
  const input = document.querySelector<HTMLInputElement>('input[type="search"]')!;
  const native = new MouseEvent('contextmenu', { bubbles: true, cancelable: true }); input.dispatchEvent(native);
  expect(native.defaultPrevented).toBe(false);
});

it('root inline rename owns selection, Return/Escape/blur and IME without changing the stored filesystem path', async () => {
  const { calls } = await setup();
  key(rootRow(missingRoot)!, 'Enter');
  const label = page.getByRole('textbox', { name: 'Sidebar label for writing', exact: true });
  await expect.element(label).toHaveFocus();
  let input = label.element() as HTMLInputElement;
  // Editing an inactive bookmark must not turn it into an active-root file-drop destination.
  expect(workspaceCopyDestination(input, document.querySelector<HTMLElement>('#project-outline')!)).toBeNull();
  expect(input.selectionStart).toBe(0); expect(input.selectionEnd).toBe(input.value.length);
  await label.fill('A readable label'); key(input, 'Enter'); await tick();
  expect(rootRow(missingRoot)!.textContent).toContain('A readable label');
  key(rootRow(missingRoot)!, 'F2'); await tick(); input = document.querySelector<HTMLInputElement>('.workspace-label-edit input')!;
  input.value = 'Cancelled'; input.dispatchEvent(new Event('input', { bubbles: true })); key(input, 'Escape'); await tick();
  expect(rootRow(missingRoot)!.textContent).toContain('A readable label');
  key(rootRow(missingRoot)!, 'F2'); await tick(); input = document.querySelector<HTMLInputElement>('.workspace-label-edit input')!;
  input.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
  input.value = '途中'; input.dispatchEvent(new InputEvent('input', { bubbles: true, isComposing: true }));
  const ime = key(input, 'Enter', { keyCode: 229 }); expect(ime.defaultPrevented).toBe(false);
  input.dispatchEvent(new FocusEvent('blur')); await tick(); expect(input.isConnected).toBe(true);
  input.value = '完成'; input.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: '完成' }));
  await expect.poll(() => rootRow(missingRoot)?.textContent).toContain('完成');
  expect(JSON.parse(localStorage.getItem(KEY)!).find((item: { root: string }) => item.root === missingRoot)).toEqual({ root: missingRoot, title: '完成' });
  expect(calls.some(call => /document_rename|project_prepare_open_path/.test(call.command))).toBe(false);
});

it('closing and reopening a menu creates a new captured DOM target rather than reusing a stale action button', async () => {
  await setup(); await context(rootRow(missingRoot)!);
  const old = page.getByRole('menuitem', { name: 'Remove from Sidebar', exact: true }).element();
  await context(rootRow(activeRoot)!);
  const current = page.getByRole('menuitem', { name: 'Remove from Sidebar', exact: true }).element();
  expect(old).not.toBe(current); expect(old.isConnected).toBe(false);
  (old as HTMLButtonElement).click(); await tick();
  expect(rootRow(missingRoot)).toBeDefined(); expect(rootRow(activeRoot)).toBeDefined();
});

it('an empty active project still exposes remembered roots for removal without opening any of them', async () => {
  const { calls } = await setup(true); expect(rootRow(missingRoot)).toBeDefined();
  await context(rootRow(missingRoot)!);
  await page.getByRole('menuitem', { name: 'Remove from Sidebar', exact: true }).click();
  await expect.poll(() => rootRow(missingRoot)).toBeUndefined();
  expect(calls.some(call => /project_prepare|project_close|document_delete/.test(call.command))).toBe(false);
});


it.each(['Notes/Paper.pdf', null])('material display rename works in placement %s without changing editor, chat, source path or full ID', async placement => {
  const { calls, materials } = await setup(false, placement, 'Original manuscript plus exact unsaved words');
  const before = { ...materials[0] }, editor = document.querySelector<HTMLElement>('.ProseMirror')!;
  const composer = page.getByRole('textbox', { name: 'Message', exact: true });
  const composerElement = composer.element(); await composer.fill('Unsent café 🦉 chat');
  // Wait for the existing editor's mount/caret lifecycle before making real input.
  await tick(); await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  editor.focus(); await userEvent.keyboard('{End} plus exact unsaved words');
  await expect.poll(() => editor.textContent).toContain('plus exact unsaved words');
  const exactText = editor.textContent;
  await expect.poll(() => document.querySelector('[data-sidebar-kind="material"]')).not.toBeNull();
  const row = document.querySelector<HTMLButtonElement>('[data-sidebar-kind="material"]')!;
  await context(row); await page.getByRole('menuitem', { name: 'Rename…', exact: true }).click();
  const field = page.getByRole('textbox', { name: 'Rename Paper.pdf', exact: true });
  await expect.element(field).toHaveFocus(); const input = field.element() as HTMLInputElement;
  expect(input.selectionStart).toBe(0); expect(input.selectionEnd).toBe(input.value.length);
  const name = '  café 🦉 / display name  '; await field.fill(name); key(input, 'Enter');
  await expect.poll(() => materials[0].name).toBe(name);
  await expect.poll(() => document.querySelector('.material-row.editing input')).toBeNull();
  expect(document.querySelector('.ProseMirror')).toBe(editor); expect(editor.textContent).toBe(exactText);
  expect(composer.element()).toBe(composerElement); await expect.element(composer).toHaveValue('Unsent café 🦉 chat');
  expect(materials[0].id).toBe(before.id); expect(materials[0].reference).toBe(before.reference);
  expect(materials[0].workspace_path).toBe(before.workspace_path); expect(materials[0].attachment_id).toBe(before.attachment_id);
  const renames = calls.filter(call => call.command === 'plugin:loom|material_rename'); expect(renames).toHaveLength(1);
  expect(renames[0].args.name).toBe(name);
  expect(calls.some(call => /document_(rename|checkpoint|delete)|workspace_copy_files|project_(close|commit_open)/.test(call.command))).toBe(false);
});

it('material IME blur waits for compositionend; Escape cancels without a native mutation', async () => {
  const { calls, materials } = await setup();
  await expect.poll(() => document.querySelector('[data-sidebar-kind="material"]')).not.toBeNull();
  key(document.querySelector<HTMLElement>('[data-sidebar-kind="material"]')!, 'F2');
  const field = page.getByRole('textbox', { name: 'Rename Paper.pdf', exact: true }); await expect.element(field).toHaveFocus();
  let input = field.element() as HTMLInputElement;
  await field.fill('not committed'); key(input, 'Escape'); await tick();
  expect(calls.some(call => call.command === 'plugin:loom|material_rename')).toBe(false);
  key(document.querySelector<HTMLElement>('[data-sidebar-kind="material"]')!, 'Enter'); await expect.element(field).toHaveFocus();
  input = field.element() as HTMLInputElement;
  input.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
  input.value = '途中'; input.dispatchEvent(new InputEvent('input', { bubbles: true, isComposing: true }));
  expect(key(input, 'Enter', { keyCode: 229 }).defaultPrevented).toBe(false); input.dispatchEvent(new FocusEvent('blur'));
  await tick(); expect(calls.some(call => call.command === 'plugin:loom|material_rename')).toBe(false);
  input.value = '完成'; input.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: '完成' }));
  await expect.poll(() => materials[0].name).toBe('完成');
  expect(calls.filter(call => call.command === 'plugin:loom|material_rename')).toHaveLength(1);
});

it('removing a source opened through its sidebar row closes that same source view', async () => {
  const { calls, materials } = await setup();
  const before = { ...materials[0] };
  const row = document.querySelector<HTMLButtonElement>('[data-sidebar-kind="material"]')!;
  await context(row); await page.getByRole('menuitem', { name: 'Open', exact: true }).click();
  await expect.element(page.getByRole('region', { name: 'Paper.pdf', exact: true })).toBeVisible();
  await userEvent.click(document.querySelector<HTMLElement>('.material-view summary[aria-label="Source actions"]')!);
  await page.getByRole('button', { name: 'Remove from workspace', exact: true }).click();
  await expect.poll(() => document.querySelector('.material-view')).toBeNull();
  await expect.poll(() => document.querySelector('[data-sidebar-kind="material"]')).toBeNull();
  const removed = calls.filter(call => call.command === 'plugin:loom|material_remove');
  expect(removed).toHaveLength(1); expect(removed[0].args.id).toBe(before.id);
  expect(removed[0].args.expectedMetadataRevision).toBe(before.metadata_revision);
  expect(calls.some(call => /document_(rename|checkpoint|delete)|workspace_copy_files/.test(call.command))).toBe(false);
});
