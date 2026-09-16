import { mount, unmount } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import ImportSources from './ImportSources.svelte';
import CollectionProgress from './CollectionProgress.svelte';
import CollectionMembers from './CollectionMembers.svelte';
import type { CollectionStatus } from './types';
import type { MaterialEntry } from './materials';

const native = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', async (original) => ({
  ...await original<typeof import('@tauri-apps/api/core')>(), invoke: native.invoke
}));
const entry: MaterialEntry = {
  id: `material-${'a'.repeat(64)}`, name: 'Research', kind: 'collection',
  reference: '@Research', available: true, pinned: false, attachment_id: null, source_path: null
};
const active: CollectionStatus = {
  scope: { kind: 'drive_folder', id: 'selected-folder' }, definition_fingerprint: 'reviewed-scope',
  id: entry.id, job_id: 'job-one', phase: 'running', retained_count: 2, failures: [],
  resumable: false, local_readable: true, refresh_authorized: true, coverage_complete: false
};
function setup() {
  const target = document.createElement('div');
  document.body.append(target);
  const previous = Object.getOwnPropertyDescriptor(window, '__TAURI_INTERNALS__');
  Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {} });
  native.invoke.mockReset();
  return { target, cleanup() {
    target.remove();
    if (previous) Object.defineProperty(window, '__TAURI_INTERNALS__', previous);
    else Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
  } };
}

describe('named connected collections', () => {
  it('requires a chosen account and concrete Drive folder, then adds one collection without inserting or opening sources', async () => {
    const runtime = setup();
    native.invoke.mockImplementation(async (command: string) => {
      switch (command) {
        case 'plugin:loom|import_accounts': return [{ service: 'drive', email: 'writer@example.test' }];
        case 'plugin:loom|collection_add': return entry;
        case 'plugin:loom|collection_refresh':
        case 'plugin:loom|collection_status': return active;
        default: throw new Error(`Unexpected command ${command}`);
      }
    });
    const onImported = vi.fn(async () => {});
    const onCollectionAdded = vi.fn(async () => {});
    const onOpenCollection = vi.fn(async () => {});
    const component = mount(ImportSources, { target: runtime.target, props: {
      projectId: 'project', sessionId: 'session', onImported, onOpen: vi.fn(async () => {}), onCollectionAdded, onOpenCollection
    } });
    try {
      await page.getByRole('combobox', { name: 'Connected source', exact: true }).selectOptions('drive');
      await expect.element(page.getByRole('combobox', { name: 'Account', exact: true })).toBeVisible();
      await page.getByRole('textbox', { name: 'Name', exact: true }).fill('Research');
      await page.getByRole('textbox', { name: 'Drive folder ID', exact: true }).fill('folder-one');
      await expect.element(page.getByRole('button', { name: 'Add collection', exact: true })).toBeDisabled();
      await page.getByRole('combobox', { name: 'Account', exact: true }).selectOptions('writer@example.test');
      await page.getByRole('textbox', { name: 'Drive folder ID', exact: true }).fill('  ');
      await expect.element(page.getByRole('button', { name: 'Add collection', exact: true })).toBeDisabled();
      expect(native.invoke.mock.calls.some(([command]) => command === 'plugin:loom|collection_add')).toBe(false);
      await page.getByRole('textbox', { name: 'Drive folder ID', exact: true }).fill('folder-one');
      await page.getByRole('button', { name: 'Add collection', exact: true }).click();
      await expect.element(page.getByRole('button', { name: 'Research', exact: true })).toBeVisible();
      expect(native.invoke).toHaveBeenCalledWith('plugin:loom|collection_add', {
        projectId: 'project', sessionId: 'session', operationId: expect.any(String),
        name: 'Research', accountEmail: 'writer@example.test', scope: { kind: 'drive_folder', id: 'folder-one' }
      });
      expect(native.invoke).toHaveBeenCalledWith('plugin:loom|collection_refresh', {
        projectId: 'project', sessionId: 'session', id: entry.id, mode: 'fresh'
      });
      expect(onCollectionAdded).toHaveBeenCalledExactlyOnceWith(entry, 'project', 'session');
      expect(onImported).not.toHaveBeenCalled();
      expect(onOpenCollection).not.toHaveBeenCalled();
      expect(page.getByRole('button', { name: /Use here/ }).query()).toBeNull();
      await page.getByRole('button', { name: 'Research', exact: true }).click();
      expect(onOpenCollection).toHaveBeenCalledExactlyOnceWith(entry, 'project', 'session');
    } finally { await unmount(component); runtime.cleanup(); }
    expect(native.invoke.mock.calls.some(([command]) => command.includes('cancel'))).toBe(false);
  });

  it('detaches without cancelling, reloads durable progress, and sends explicit Resume and Stop', async () => {
    const runtime = setup();
    let status = { ...active };
    native.invoke.mockImplementation(async (command: string) => {
      if (command === 'plugin:loom|collection_status') return status;
      if (command === 'plugin:loom|collection_refresh') { status = { ...active, job_id: 'job-two' }; return status; }
      if (command === 'plugin:loom|collection_cancel') { status = { ...status, phase: 'paused', resumable: true }; return status; }
      throw new Error(`Unexpected command ${command}`);
    });
    const props = { projectId: 'project', sessionId: 'session', collectionId: entry.id };
    let component = mount(CollectionProgress, { target: runtime.target, props });
    try {
      await expect.element(page.getByRole('status')).toHaveTextContent('2 sources retained · Updating…');
      await unmount(component);
      expect(native.invoke.mock.calls.some(([command]) => command.includes('cancel'))).toBe(false);
      status = { ...active, phase: 'interrupted', resumable: true, retained_count: 3 };
      component = mount(CollectionProgress, { target: runtime.target, props });
      await expect.element(page.getByRole('status')).toHaveTextContent('3 sources retained · Interrupted');
      await page.getByRole('button', { name: 'Resume', exact: true }).click();
      expect(native.invoke).toHaveBeenCalledWith('plugin:loom|collection_refresh', {
        projectId: 'project', sessionId: 'session', id: entry.id, mode: 'resume'
      });
      await page.getByRole('button', { name: 'Stop', exact: true }).click();
      expect(native.invoke).toHaveBeenCalledWith('plugin:loom|collection_cancel', {
        projectId: 'project', sessionId: 'session', id: entry.id, jobId: 'job-two'
      });
      await expect.element(page.getByRole('status')).toHaveTextContent('Paused');
      await page.getByRole('button', { name: 'Start again', exact: true }).click();
      expect(native.invoke).toHaveBeenCalledWith('plugin:loom|collection_refresh', {
        projectId: 'project', sessionId: 'session', id: entry.id, mode: 'fresh'
      });
      await unmount(component);
      status = { ...status, phase: 'interrupted', refresh_authorized: false };
      component = mount(CollectionProgress, { target: runtime.target, props });
      await expect.element(page.getByText('Retained sources are available. Reconnect to refresh.', { exact: true })).toBeVisible();
      expect(page.getByRole('button', { name: /Refresh|Resume|Stop/ }).query()).toBeNull();
    } finally { await unmount(component); runtime.cleanup(); }
  });

  it('browses retained member pages without acquisition and opens only the chosen occurrence', async () => {
    const runtime = setup();
    const first = { snapshot_id: 'frozen-version', occurrence_id: 'one', name: 'First source', attachment_id: '1'.repeat(64), source_uri: 'https://example.test/1' };
    const second = { ...first, occurrence_id: 'two', name: 'Second source' };
    native.invoke.mockImplementation(async (command: string, args: { offset: number }) => {
      if (command === 'plugin:loom|collection_members') return args.offset === 0
        ? { snapshot_id: first.snapshot_id, members: [first], total: 2, next_offset: 1 }
        : { snapshot_id: first.snapshot_id, members: [second], total: 2, next_offset: null };
      throw new Error(`Unexpected command ${command}`);
    });
    const onOpenMember = vi.fn();
    const component = mount(CollectionMembers, { target: runtime.target, props: {
      projectId: 'project', sessionId: 'session', collectionId: entry.id, onOpenMember
    } });
    try {
      await expect.element(page.getByRole('button', { name: 'First source', exact: true })).toBeVisible();
      expect(onOpenMember).not.toHaveBeenCalled();
      await page.getByRole('button', { name: 'More sources', exact: true }).click();
      expect(native.invoke).toHaveBeenCalledWith('plugin:loom|collection_members', { projectId:'project', sessionId:'session', id:entry.id, offset:1, snapshotId:first.snapshot_id });
      await page.getByRole('button', { name: 'Second source', exact: true }).click();
      expect(onOpenMember).toHaveBeenCalledExactlyOnceWith(second);
      expect(page.getByRole('button', { name: 'More sources', exact: true }).query()).toBeNull();
      expect(native.invoke.mock.calls.every(([command]) => command === 'plugin:loom|collection_members')).toBe(true);
    } finally { await unmount(component); runtime.cleanup(); }
  });

  it('reconnects only the selected account and displayed scope without starting acquisition', async () => {
    const runtime = setup();
    native.invoke.mockImplementation(async (command: string) => {
      if (command === 'plugin:loom|collection_status') return { ...active, phase: 'interrupted', refresh_authorized: false };
      if (command === 'plugin:loom|import_accounts') return [{ service: 'drive', email: 'writer@example.test' }];
      if (command === 'plugin:loom|collection_authorize') return { ...active, phase: 'interrupted', resumable: true };
      throw new Error(`Unexpected command ${command}`);
    });
    const component = mount(CollectionProgress, { target: runtime.target, props: {
      projectId: 'project', sessionId: 'session', collectionId: entry.id
    } });
    try {
      await page.getByRole('button', { name: 'Reconnect', exact: true }).click();
      await expect.element(page.getByText('Drive folder: selected-folder', { exact: true })).toBeVisible();
      await expect.element(page.getByRole('button', { name: 'Connect', exact: true })).toBeDisabled();
      await page.getByRole('combobox', { name: 'Account', exact: true }).selectOptions('writer@example.test');
      await page.getByRole('button', { name: 'Connect', exact: true }).click();
      expect(native.invoke).toHaveBeenCalledWith('plugin:loom|collection_authorize', {
        projectId: 'project', sessionId: 'session', id: entry.id,
        definitionFingerprint: 'reviewed-scope', accountEmail: 'writer@example.test'
      });
      await expect.element(page.getByRole('button', { name: 'Resume', exact: true })).toBeVisible();
      expect(native.invoke.mock.calls.some(([command]) => command === 'plugin:loom|collection_refresh')).toBe(false);
    } finally { await unmount(component); runtime.cleanup(); }
  });
});
