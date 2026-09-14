import { mount, unmount } from 'svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import ConfiguredDownloads from './ConfiguredDownloads.svelte';
import '../app.css';

let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = undefined; document.body.replaceChildren();
});
const writer = {
  url: 'https://models.example/writer.gguf?revision=one', file_name: 'writer.gguf',
  sha256: 'ab'.repeat(32), expected_bytes: 4_954_576_032, max_bytes: 8_589_934_592
};
function render(error = '', disabled = false) {
  const target = document.createElement('div'); document.body.append(target);
  const onStart = vi.fn(), onSettings = vi.fn();
  mounted = mount(ConfiguredDownloads, { target, props: { downloads: { writer }, error, disabled, onStart, onSettings } });
  return { onStart, onSettings };
}

it('offers exact download evidence and contacts no source until an explicit command', async () => {
  const { onStart, onSettings } = render();
  expect(onStart).not.toHaveBeenCalled();
  expect(document.querySelector('input')).toBeNull();
  await page.getByText('Download identity', { exact: true }).click();
  await expect.element(page.getByText(writer.url, { exact: true })).toBeVisible();
  await expect.element(page.getByText(writer.sha256, { exact: true })).toBeVisible();
  expect(onStart).not.toHaveBeenCalled();
  await page.getByRole('button', { name: 'Download and verify writer' }).click();
  expect(onStart).toHaveBeenCalledExactlyOnceWith(writer);
  await page.getByRole('button', { name: 'Open settings file' }).click();
  expect(onSettings).toHaveBeenCalledOnce();
});

it('offers repair and no download action for invalid settings', async () => {
  const { onStart, onSettings } = render('download settings: invalid checksum');
  await expect.element(page.getByRole('alert')).toHaveTextContent('invalid checksum');
  expect(document.querySelector('button[aria-label^="Download"]')).toBeNull();
  await page.getByRole('button', { name: 'Open settings file' }).click();
  expect(onSettings).toHaveBeenCalledOnce();
  expect(onStart).not.toHaveBeenCalled();
});

it('leaves settings readable while another exact request owns the download command', async () => {
  const { onStart, onSettings } = render('', true);
  expect((page.getByRole('button', { name: 'Download and verify writer' }).element() as HTMLButtonElement).disabled).toBe(true);
  await page.getByRole('button', { name: 'Open settings file' }).click();
  expect(onSettings).toHaveBeenCalledOnce();
  expect(onStart).not.toHaveBeenCalled();
});
