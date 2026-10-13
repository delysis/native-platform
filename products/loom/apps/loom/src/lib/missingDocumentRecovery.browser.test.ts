import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import MissingDocumentRecoveryNotice from './MissingDocumentRecoveryNotice.svelte';

let mounted: ReturnType<typeof mount> | null = null;

afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = null;
  document.body.replaceChildren();
});

function render(overrides: Partial<Parameters<typeof mount>[1]['props']> = {}): void {
  const target = document.createElement('div');
  document.body.append(target);
  mounted = mount(MissingDocumentRecoveryNotice, {
    target,
    props: {
      title: 'Draft chapter',
      relativePath: 'manuscript/Draft-chapter.md',
      text: 'unsaved exact text\nsecond line',
      hadUnsavedText: true,
      journalDurable: true,
      draftWasUncertain: false,
      saveWasUncertain: false,
      copyState: 'idle',
      onCopy: () => {},
      ...overrides
    }
  });
}

describe('missing document recovery notice', () => {
  it('keeps exact dirty text inspectable after the current file disappears', async () => {
    render();
    await expect.element(page.getByRole('alert')).toHaveTextContent('latest draft journal is durable');
    await page.getByText('Recovery', { exact: true }).click();
    await expect.element(page.getByText('Its latest draft journal is durable.', { exact: false })).toBeVisible();
    await expect.element(page.getByRole('textbox', { name: 'Preserved text for Draft chapter' }))
      .toHaveValue('unsaved exact text\nsecond line');
    expect(page.getByRole('alert').element().textContent).not.toContain('I/O failure');
  });

  it('makes an uncertain journal explicit and keeps copy recovery actionable', async () => {
    const onCopy = vi.fn();
    render({
      journalDurable: false,
      draftWasUncertain: true,
      saveWasUncertain: true,
      copyState: 'failed',
      onCopy
    });
    await expect.element(page.getByRole('alert')).toHaveTextContent('Draft durability could not be confirmed');
    await expect.element(page.getByRole('alert')).toHaveTextContent(
      'Draft and save results were uncertain when the deletion was detected'
    );
    await expect.element(page.getByRole('status')).toHaveTextContent('Failed');
    await expect.element(page.getByRole('status')).toHaveAttribute('title', 'Copy failed; open Recovery and select the preserved text');
    await expect.element(page.getByRole('img', { name: 'File deleted outside Loom' })).toHaveAttribute('title', 'Draft durability could not be confirmed. Keep Loom open and copy the preserved text.');
    await page.getByText('Recovery', { exact: true }).click();
    await expect.element(page.getByRole('textbox', { name: 'Preserved text for Draft chapter' })).toHaveValue('unsaved exact text\nsecond line');
    await page.getByRole('button', { name: 'Copy preserved text' }).click();
    expect(onCopy).toHaveBeenCalledOnce();
  });
});
