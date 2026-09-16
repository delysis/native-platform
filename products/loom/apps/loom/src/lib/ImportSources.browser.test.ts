import { mount, unmount } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import ImportSources from './ImportSources.svelte';
import type { ImportBatch } from './ipc';

const native = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', async (original) => ({
  ...await original<typeof import('@tauri-apps/api/core')>(),
  invoke: native.invoke
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

describe('source import cancellation', () => {
  it('sends Stop through the real IPC lane before completion and preserves partial results', async () => {
    const batch = deferred<ImportBatch>();
    const use = deferred<boolean>();
    const disconnect = deferred<void>();
    const target = document.createElement('div');
    document.body.append(target);
    const previousRuntime = Object.getOwnPropertyDescriptor(window, '__TAURI_INTERNALS__');
    Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {} });
    native.invoke.mockReset();
    native.invoke.mockImplementation(async (command: string) => {
      switch (command) {
        case 'plugin:loom|import_accounts': return [{ service: 'gmail', email: 'writer@example.test' }];
        case 'plugin:loom|attachment_import_batch_choose': return batch.promise;
        case 'plugin:loom|import_account_cancel': return;
        case 'plugin:loom|import_account_disconnect': return disconnect.promise;
        default: throw new Error(`Unexpected native command: ${command}`);
      }
    });
    const onUse = vi.fn(() => use.promise);
    const component = mount(ImportSources, { target, props: {
      projectId: 'project-a', sessionId: 'session-a', documentTitle: 'Draft', onUse
    } });
    const retained = {
      id: 'a'.repeat(64), file_name: 'Retained source.md', byte_count: 14,
      detected_format: 'markdown', coverage_complete: true, text_bytes: 14,
      media_kinds: [], warnings: [], inline_markdown: '[Retained source](loom-attachment:source)'
    } satisfies ImportBatch['imported'][number];
    try {
      await page.getByText('Import sources', { exact: true }).click();
      await page.getByRole('button', { name: 'Choose files', exact: true }).click();
      await vi.waitFor(() => expect(native.invoke).toHaveBeenCalledWith(
        'plugin:loom|attachment_import_batch_choose', expect.objectContaining({ folder: false })
      ));
      const admission = native.invoke.mock.calls.find(([command]) => command === 'plugin:loom|attachment_import_batch_choose')!;
      const operationId: string = admission[1].operationId;
      // ULID's first character must fit 128 bits, and its alphabet excludes I/L/O/U.
      expect(operationId).toMatch(/^[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
      await page.getByRole('button', { name: 'Stop import', exact: true }).click();
      await vi.waitFor(() => expect(native.invoke).toHaveBeenCalledWith('plugin:loom|import_account_cancel', {
        projectId: 'project-a', sessionId: 'session-a', operationId
      }));
      // The import has not resolved: cancel must bypass its pending ordering lane.
      await expect.element(page.getByRole('status')).toHaveTextContent('Stopping…');
      batch.resolve({ imported: [retained], failures: [{ name: 'Next source.pdf', message: 'Import stopped.' }], next_page_token: null });
      await expect.element(page.getByRole('checkbox', { name: /^Retained source\.md/ })).toBeVisible();
      await expect.element(page.getByText('Next source.pdf: Import stopped.', { exact: true })).toBeVisible();
      expect(page.getByRole('button', { name: 'Stop import', exact: true }).query()).toBeNull();

      await page.getByRole('checkbox').click();
      await page.getByRole('button', { name: 'Add selected to context (1/16)', exact: true }).click();
      expect(onUse).toHaveBeenCalledWith([retained]);
      expect(page.getByRole('button', { name: 'Stop import', exact: true }).query()).toBeNull();
      use.resolve(true);
      await expect.element(page.getByRole('status')).toHaveTextContent('Selected sources added');
      await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
      await vi.waitFor(() => expect(native.invoke).toHaveBeenCalledWith('plugin:loom|import_account_disconnect', {
        projectId: 'project-a', sessionId: 'session-a', service: 'gmail', accountEmail: 'writer@example.test'
      }));
      expect(page.getByRole('button', { name: 'Stop import', exact: true }).query()).toBeNull();
      disconnect.resolve();
      await expect.element(page.getByRole('status')).toHaveTextContent('Connection removed');
      expect(native.invoke.mock.calls.filter(([command]) => command === 'plugin:loom|import_account_cancel')).toHaveLength(1);
    } finally {
      batch.resolve({ imported: [], failures: [], next_page_token: null });
      use.resolve(false); disconnect.resolve();
      await unmount(component); target.remove();
      if (previousRuntime) Object.defineProperty(window, '__TAURI_INTERNALS__', previousRuntime);
      else Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
    }
  });
});
