import { mount, unmount } from 'svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import LoomEditor from './LoomEditor.svelte';
import SourceEditor from './SourceEditor.svelte';
import '../app.css';

const read = vi.hoisted(() => vi.fn());
vi.mock('./ipc', async original => ({ ...await original<typeof import('./ipc')>(), documentReferenceDiagnostics: read }));
let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (mounted) await unmount(mounted); mounted = undefined; document.body.replaceChildren(); vi.resetAllMocks(); });
const referenceScope = { projectId: 'project', sessionId: 'session', revision: 'one' };

it('marks the original writing even when visual serialization normalizes whitespace', async () => {
  const value = 'The quiet moon.  @Missing\n\n@“Notes.md” ';
  const target = document.createElement('div'); document.body.append(target);
  const onChange = vi.fn();
  read.mockResolvedValue([{ start: value.indexOf('@Missing'), end: value.indexOf('@Missing') + 8, message: 'Source missing' }]);
  mounted = mount(LoomEditor, { target, props: { value, referenceScope, onChange, onGhostPresentationRejected: () => {} } });
  await expect.element(page.getByTitle('Source missing')).toBeVisible();
  expect(page.getByTitle('Source missing').element().textContent).toBe('@Missing');
  expect(onChange).not.toHaveBeenCalled();
});

it('underlines the missing link in the live visual editor without changing bytes, selection or surrounding layout', async () => {
  const value = 'Before [@Lost](loom-material:missing) after.';
  const target = document.createElement('div'); document.body.append(target);
  const onChange = vi.fn();
  read.mockResolvedValue([{ start: 7, end: value.indexOf(' after.'), message: 'Source missing' }]);
  mounted = mount(LoomEditor, { target, props: { value, referenceScope, onChange, onGhostPresentationRejected: () => {} } });
  const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
  await expect.element(editor).toBeVisible();
  const before = editor.element().getBoundingClientRect().height;
  await expect.element(page.getByTitle('Source missing')).toBeVisible();
  expect(page.getByTitle('Source missing').element().textContent).toBe('@Lost');
  expect(getComputedStyle(page.getByTitle('Source missing').element()).textDecorationLine).toBe('underline');
  expect(editor.element().getBoundingClientRect().height).toBe(before);
  expect(onChange).not.toHaveBeenCalled();
});

it('marks the source reference through its mirror while preserving the native textarea', async () => {
  const target = document.createElement('div'); target.className = 'source-pane'; target.style.height = '240px'; document.body.append(target);
  read.mockResolvedValue([{ start: 7, end: 12, message: 'Source missing' }]);
  mounted = mount(SourceEditor, { target, props: { value: 'Before @Lost after.', referenceScope, element: undefined } });
  await expect.poll(() => target.querySelector('.reference-unavailable')?.textContent).toBe('@Lost');
  const marked = target.querySelector('.reference-unavailable')!;
  expect(getComputedStyle(marked).textDecorationLine).toBe('underline');
  await expect.element(page.getByRole('textbox', { name: 'Markdown source editor' })).toHaveValue('Before @Lost after.');
  expect(target.querySelector('.source-ghost-viewport')?.hasAttribute('hidden')).toBe(false);
});

it('follows the visual caret with accessible diagnostics without changing the writing', async () => {
  const target = document.createElement('div'); document.body.append(target);
  const onChange = vi.fn();
  read.mockResolvedValue([{ start: 7, end: 12, message: 'Source missing' }]);
  mounted = mount(LoomEditor, { target, props: { value: 'Before @Lost', referenceScope, onChange, onGhostPresentationRejected: () => {} } });
  const editor = page.getByRole('textbox', { name: 'Manuscript editor' });
  await expect.element(page.getByTitle('Source missing')).toBeVisible();
  await userEvent.click(editor);
  await userEvent.keyboard('{ArrowRight}');
  await userEvent.keyboard('{Meta>}{ArrowRight}{/Meta}');
  await expect.element(editor).toHaveAttribute('aria-description', 'Source missing');
  await userEvent.keyboard('{Meta>}{ArrowLeft}{/Meta}');
  await expect.element(editor).toHaveAttribute('aria-description', '');
  expect(onChange).not.toHaveBeenCalled();
});
