import { mount, unmount, type ComponentProps } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import MaterialView from './MaterialView.svelte';
import type { MaterialEntry, MaterialEvidence } from './materials';
import '../app.css';
const ipc = vi.hoisted(() => ({ read: vi.fn(), search: vi.fn(), evidence: vi.fn(), pin: vi.fn(), original: vi.fn(), remove: vi.fn() }));
vi.mock('./ipc', async original => ({ ...await original<typeof import('./ipc')>(), readMaterial: ipc.read, searchMaterial: ipc.search, readMaterialEvidence: ipc.evidence, pinMaterial: ipc.pin, removeMaterial: ipc.remove, revealAttachmentOriginal: ipc.original }));
const material: MaterialEntry = { id: 'material-' + 'a'.repeat(64), name: 'Research', reference: '@"materials/Research#a"', kind: 'library', pinned: false, available: true, source_path: '/private/library.sqlite', attachment_id: null };
const evidence: MaterialEvidence = { id: 'b'.repeat(64), reference: '@"evidence/b"', material_id: material.id, title: 'A source', text: 'Exact café evidence.\nSecond line.', source_revision: 'revision', text_sha256: 'hash', locator: { document_id: 'source', block_id: 7 } };
let view: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (view) await unmount(view); view = undefined; document.body.replaceChildren(); vi.resetAllMocks(); });
function render(props: Partial<ComponentProps<typeof MaterialView>> = {}) {
  ipc.read.mockResolvedValue({ material, text: '', complete: true, warnings: [], source_revision: '', evidence: [], presentation: null });
  ipc.search.mockResolvedValue({ material, query: 'history', hits: [evidence], complete: true, warnings: [] });
  ipc.evidence.mockResolvedValue(evidence);
  ipc.pin.mockResolvedValue({ ...material, pinned: true });
  const onUse = vi.fn().mockResolvedValue(true), onChanged = vi.fn(), onClose = vi.fn(), onReopen = vi.fn(), onRemoved = vi.fn();
  const target = document.createElement('div'); target.style.height = '500px'; document.body.append(target);
  view = mount(MaterialView, { target, props: { projectId: 'project', sessionId: 'session', material, originTitle: 'Draft', onUse, onChanged, onClose, onReopen, onRemoved, ...props } });
  return { onUse, onChanged, onClose, onRemoved };
}
describe('named material viewing', () => {
  it('searches only on request, opens exact retained passages, and uses the passage instead of the collection', async () => {
    const { onUse } = render();
    await expect.poll(() => ipc.read.mock.calls.length).toBe(1);
    expect(ipc.search).not.toHaveBeenCalled();
    await page.getByRole('searchbox', { name: 'Search Research' }).fill('history');
    await page.getByRole('button', { name: 'Search', exact: true }).click();
    await page.getByRole('button', { name: /A source/ }).click();
    await expect.element(page.getByText('Exact café evidence.\nSecond line.', { exact: true })).toBeVisible();
    expect(ipc.evidence).toHaveBeenCalledWith('project', 'session', material.id, evidence.id);
    await page.getByRole('button', { name: 'Use here' }).click();
    expect(onUse).toHaveBeenCalledWith(`[@A source](loom-evidence:${evidence.id})`, null);
  });
  it('pinning only changes navigation and explicit insertion supplies exact retained text', async () => {
    const { onUse, onChanged } = render({ initialEvidence: evidence });
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Pin', exact: true }).click();
    await expect.poll(() => onChanged.mock.calls.length).toBe(1);
    expect(onUse).not.toHaveBeenCalled();
    await page.getByRole('button', { name: 'Insert quotation' }).click();
    expect(onUse).toHaveBeenCalledWith(`[@A source](loom-evidence:${evidence.id})`, evidence.text);
    expect(ipc.read).not.toHaveBeenCalled();
  });
  it('quotes only an exact selected source range while retaining the source evidence reference', async () => {
    const exact = { ...evidence, text: 'Before.\n  café 🖋 @Another\nAfter.' };
    const { onUse } = render({ initialEvidence: exact });
    await expect.element(page.getByText(exact.text, { exact: true })).toBeVisible();
    const source = document.querySelector('.source-text')!;
    const range = document.createRange();
    const excerpt = '  café 🖋 @Another\n';
    const start = exact.text.indexOf(excerpt);
    range.setStart(source.firstChild!, start);
    range.setEnd(source.firstChild!, start + excerpt.length);
    window.getSelection()!.removeAllRanges(); window.getSelection()!.addRange(range);
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Insert quotation' }).click();
    expect(onUse).toHaveBeenCalledWith(`[@A source](loom-evidence:${evidence.id})`, excerpt);
  });
  it.each(['elsewhere', 'crossing'])('ignores a %s selection rather than inserting unrelated page text', async scope => {
    const { onUse } = render({ initialEvidence: evidence });
    await expect.element(page.getByText(evidence.text, { exact: true })).toBeVisible();
    const location = document.querySelector('.location')!;
    const source = document.querySelector('.source-text')!;
    const range = document.createRange();
    range.selectNodeContents(location);
    if (scope === 'crossing') range.setEnd(source.firstChild!, 5);
    window.getSelection()!.removeAllRanges(); window.getSelection()!.addRange(range);
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Insert quotation' }).click();
    expect(onUse).toHaveBeenCalledWith(`[@A source](loom-evidence:${evidence.id})`, evidence.text);
  });
  it('removes only the workspace binding and updates navigation after native confirmation', async () => {
    let finish!: () => void;
    ipc.remove.mockImplementation(() => new Promise<void>(resolve => { finish = resolve; }));
    const { onRemoved, onUse } = render({ initialEvidence: evidence });
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Remove from workspace' }).click();
    expect(ipc.remove).toHaveBeenCalledWith('project', 'session', material.id);
    expect(onRemoved).not.toHaveBeenCalled();
    await expect.element(page.getByRole('button', { name: 'Remove from workspace' })).toBeDisabled();
    finish();
    await expect.poll(() => onRemoved.mock.calls.length).toBe(1);
    expect(onRemoved).toHaveBeenCalledWith(material.id, 'session');
    expect(onUse).not.toHaveBeenCalled();
    expect(ipc.original).not.toHaveBeenCalled();
  });
  it('reads retained evidence after the live library is unavailable without reopening it', async () => {
    const { onUse } = render({ material: { ...material, available: false }, initialEvidence: evidence });
    await expect.element(page.getByText('Exact café evidence.\nSecond line.', { exact: true })).toBeVisible();
    expect(ipc.read).not.toHaveBeenCalled();
    await page.getByRole('button', { name: 'Use here' }).click();
    expect(onUse).toHaveBeenCalledOnce();
  });
  it('keeps a stale insertion failure in the source view without changing the selected passage', async () => {
    render({ initialEvidence: evidence, onUse: vi.fn().mockResolvedValue(false) });
    await page.getByRole('button', { name: 'Use here' }).click();
    await expect.element(page.getByRole('alert')).toHaveTextContent('The writing changed');
    await expect.element(page.getByText('Exact café evidence.\nSecond line.', { exact: true })).toBeVisible();
  });
});
