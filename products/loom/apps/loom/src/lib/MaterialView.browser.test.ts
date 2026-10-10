import { mount, unmount, type ComponentProps } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import MaterialView from './MaterialView.svelte';
import type { MaterialEntry, MaterialEvidence, MaterialRead, MaterialNavigation } from './materials';
import '../app.css';
const ipc = vi.hoisted(() => ({ read: vi.fn(), pdf: vi.fn(), search: vi.fn(), evidence: vi.fn(), pin: vi.fn(), original: vi.fn(), remove: vi.fn() }));
vi.mock('./ipc', async original => ({ ...await original<typeof import('./ipc')>(), readMaterial: ipc.read, readMaterialPdfPage: ipc.pdf, searchMaterial: ipc.search, readMaterialEvidence: ipc.evidence, pinMaterial: ipc.pin, removeMaterial: ipc.remove, revealAttachmentOriginal: ipc.original }));
const material: MaterialEntry = { id: 'material-' + 'a'.repeat(64), name: 'Research', reference: '@"materials/Research#a"', kind: 'library', pinned: false, available: true, source_path: '/private/library.sqlite', attachment_id: null, metadata_revision: 'a'.repeat(64) };
const evidence: MaterialEvidence = { id: 'b'.repeat(64), reference: '@"evidence/b"', material_id: material.id, title: 'A source', text: 'Exact café evidence.\nSecond line.', source_revision: 'revision', text_sha256: 'hash', locator: { document_id: 'source', block_id: 7 } };
let view: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (view) await unmount(view); view = undefined; document.body.replaceChildren(); vi.resetAllMocks(); });
function render(props: Partial<ComponentProps<typeof MaterialView>> = {}, read?: MaterialRead) {
  ipc.read.mockResolvedValue(read ?? { material, text: '', complete: true, warnings: [], source_revision: '', evidence: [], presentation: null });
  ipc.search.mockResolvedValue({ material, query: 'history', hits: [evidence], complete: true, warnings: [] });
  ipc.evidence.mockResolvedValue(evidence);
  ipc.pin.mockResolvedValue({ ...material, pinned: true, metadata_revision: 'c'.repeat(64) });
  const onUse = vi.fn().mockResolvedValue(true), onChanged = vi.fn(), onClose = vi.fn(), onReopen = vi.fn(), onRemoved = vi.fn();
  const target = document.createElement('div'); target.style.height = '500px'; document.body.append(target);
  view = mount(MaterialView, { target, props: { projectId: 'project', sessionId: 'session', material, originTitle: 'Draft', onUse, onChanged, onClose, onReopen, onRemoved, ...props } });
  return { onUse, onChanged, onClose, onRemoved };
}
function pdfSource(): MaterialRead {
  const pdf: MaterialEntry = { ...material, kind: 'attachment', name: 'Paper.pdf', attachment_id: 'd'.repeat(64), source_path: null };
  const first = '## Page 1\nFirst café page.\n\n', third = '## Page 3\nSecond extracted page 🖋.\n';
  const text = first + third;
  return { material: pdf, text, complete: false, warnings: ['Page 2 has no extracted text.'], source_revision: 'revision', evidence: [{ ...evidence, text }], presentation: {
    id: pdf.attachment_id!, source_revision: 'revision', excerpt: null, file_name: 'Paper.pdf', detected_format: 'pdf', coverage_complete: false, text_bytes: new TextEncoder().encode(text).length, presentation_kind: 'text', media: [], warnings: [],
    pdf_pages: [{ number: 1, start_byte: 0, end_byte: new TextEncoder().encode(first).length }, { number: 3, start_byte: new TextEncoder().encode(first).length, end_byte: new TextEncoder().encode(text).length }]
  } };
}
describe('named material viewing', () => {
  it('restores query and exact evidence across remounts without caching payloads, while explicit links override selection', async () => {
    let navigation: MaterialNavigation | undefined;
    const remember = (value: MaterialNavigation) => { navigation = value; };
    render({ onNavigationChange: remember });
    await page.getByRole('searchbox', { name: 'Search Research' }).fill('history');
    await page.getByRole('button', { name: 'Search', exact: true }).click();
    await page.getByRole('button', { name: /A source/ }).click();
    await expect.poll(() => navigation?.evidenceId).toBe(evidence.id);
    expect(navigation?.query).toBe('history');
    expect(JSON.stringify(navigation)).not.toContain(evidence.text);
    await unmount(view!); view = undefined; document.body.replaceChildren();
    render({ navigation, onNavigationChange: remember });
    await expect.element(page.getByText(evidence.text, { exact: true })).toBeVisible();
    expect(ipc.evidence).toHaveBeenCalledTimes(2);
    expect(ipc.evidence).toHaveBeenLastCalledWith('project', 'session', material.id, evidence.id);
    expect(ipc.search).toHaveBeenCalledOnce();
    await page.getByRole('button', { name: '‹ Search', exact: true }).click();
    await expect.element(page.getByRole('searchbox', { name: 'Search Research' })).toHaveValue('history');
    await unmount(view!); view = undefined; document.body.replaceChildren();
    const explicit = { ...evidence, id: 'explicit', text: 'Explicit retained link wins.' };
    render({ navigation: { ...navigation!, evidenceId: evidence.id }, initialEvidence: explicit, onNavigationChange: remember });
    await expect.element(page.getByText(explicit.text, { exact: true })).toBeVisible();
    expect(ipc.evidence).toHaveBeenCalledTimes(2);
    await expect.poll(() => navigation?.evidenceId).toBe(explicit.id);
  });

  it('restores the selected PDF page and text view using a fresh native source read', async () => {
    const read = pdfSource(); read.presentation!.pdf_preview_token = 'retained-pdf';
    ipc.pdf.mockImplementation(async (_token: string, number: number) => ({ page: number, page_count: 3, width: 1, height: 1, incomplete: false, png_base64: '' }));
    let navigation: MaterialNavigation | undefined;
    const remember = (value: MaterialNavigation) => { navigation = value; };
    render({ material: read.material, onNavigationChange: remember }, read);
    await expect.poll(() => navigation?.pdfPageCount).toBe(3);
    await page.getByRole('button', { name: 'Next page' }).click();
    await expect.poll(() => navigation?.pageIndex).toBe(1);
    await expect.element(page.getByRole('button', { name: 'Next page' })).toBeEnabled();
    await page.getByRole('button', { name: 'Next page' }).click();
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Show extracted text' }).click();
    await expect.poll(() => navigation?.pdfText).toBe(true);
    await unmount(view!); view = undefined; document.body.replaceChildren();
    render({ material: read.material, navigation, onNavigationChange: remember }, read);
    await expect.poll(() => document.querySelector('.source-text')?.textContent).toBe('## Page 3\nSecond extracted page 🖋.\n');
    await expect.element(page.getByRole('combobox', { name: 'Page', exact: true })).toHaveValue('2');
    await expect.element(page.getByRole('button', { name: 'Previous page' })).toBeEnabled();
    expect(ipc.read).toHaveBeenCalledTimes(2);
    expect(ipc.pdf).toHaveBeenCalledTimes(3);
  });

  it('opens original pages including pages without extracted text, and keeps quotations on canonical text', async () => {
    const read = pdfSource();
    read.presentation!.pdf_preview_token = 'retained-pdf';
    ipc.pdf.mockImplementation(async (_token: string, number: number) => ({ page: number, page_count: 3, width: 1, height: 1, incomplete: false, png_base64: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=' }));
    const { onUse } = render({ material: read.material }, read);
    await expect.element(page.getByRole('img', { name: 'Paper.pdf, page 1' })).toBeVisible();
    expect(document.querySelector('.source-text')).toBeNull();
    await page.getByRole('button', { name: 'Next page' }).click();
    await expect.element(page.getByRole('img', { name: 'Paper.pdf, page 2' })).toBeVisible();
    expect(ipc.pdf).toHaveBeenLastCalledWith('retained-pdf', 2, expect.any(AbortSignal));
    await page.getByText('•••', { exact: true }).click();
    expect(page.getByRole('button', { name: 'Insert quotation' }).query()).toBeNull();
    await page.getByRole('button', { name: 'Show extracted text' }).click();
    await expect.element(page.getByText('No text was extracted from this page.', { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Next page' }).click();
    await expect.poll(() => document.querySelector('.source-text')?.textContent).toBe('## Page 3\nSecond extracted page 🖋.\n');
    await page.getByRole('button', { name: 'Insert quotation' }).click();
    expect(onUse).toHaveBeenCalledWith(`[@A source](loom-evidence:${evidence.id})`, '## Page 3\nSecond extracted page 🖋.\n');
    expect(ipc.pdf).toHaveBeenCalledTimes(2);
  });
  it('discards an in-flight page when switching to extracted text', async () => {
    const read = pdfSource(); read.presentation!.pdf_preview_token = 'retained-pdf';
    let settle!: (value: unknown) => void;
    ipc.pdf.mockImplementation(() => new Promise(resolve => { settle = resolve; }));
    render({ material: read.material }, read);
    await expect.poll(() => ipc.pdf.mock.calls.length).toBe(1);
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Show extracted text' }).click();
    await expect.poll(() => document.querySelector('.source-text')?.textContent).toBe('## Page 1\nFirst café page.\n\n');
    settle({ page: 1, page_count: 3, width: 1, height: 1, png_base64: '', incomplete: true });
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(document.querySelector('.pdf-page')).toBeNull();
    expect(page.getByText('Some page content could not be drawn. The original is retained.').query()).toBeNull();
  });
  it('keeps unmapped extracted text readable when original-page rendering fails', async () => {
    const read = pdfSource();
    read.presentation!.pdf_preview_token = 'retained-pdf'; read.presentation!.pdf_pages = [];
    ipc.pdf.mockRejectedValue({ code: 'pdf_render_failed', message: 'This PDF page could not be drawn.', retryable: false });
    render({ material: read.material }, read);
    await expect.element(page.getByRole('alert')).toHaveTextContent('This PDF page could not be drawn.');
    expect(document.querySelector('.pdf-page')).toBeNull();
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Show extracted text' }).click();
    await expect.poll(() => document.querySelector('.source-text')?.textContent).toBe(read.text);
    await expect.element(page.getByText('The extracted text has no page mapping. Showing the whole document.')).toBeVisible();
  });
  it('navigates only mapped PDF pages and quotes the exact displayed page with retained lineage', async () => {
    const read = pdfSource();
    const { onUse } = render({ material: read.material }, read);
    await expect.element(page.getByRole('combobox', { name: 'Page', exact: true })).toBeVisible();
    await expect.element(page.getByRole('button', { name: 'Previous page' })).toBeDisabled();
    expect(Array.from(document.querySelectorAll('select[aria-label="Page"] option')).map(option => option.textContent)).toEqual(['Page 1', 'Page 3']);
    await page.getByRole('button', { name: 'Next page' }).click();
    const third = '## Page 3\nSecond extracted page 🖋.\n';
    await expect.poll(() => document.querySelector('.source-text')?.textContent).toBe(third);
    await expect.element(page.getByRole('button', { name: 'Next page' })).toBeDisabled();
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Insert quotation' }).click();
    expect(onUse).toHaveBeenCalledWith(`[@A source](loom-evidence:${evidence.id})`, third);
    expect(ipc.read).toHaveBeenCalledOnce();
  });
  it('opens a retained PDF passage at its matching source page only on explicit request', async () => {
    const read = pdfSource();
    const retained = { ...evidence, locator: { kind: 'attachment_text', pdf_pages: [read.presentation!.pdf_pages![1]] } };
    render({ material: read.material, initialEvidence: retained }, read);
    await expect.element(page.getByText('Page 3', { exact: true })).toBeVisible();
    expect(ipc.read).not.toHaveBeenCalled();
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Open page 3' }).click();
    await expect.poll(() => document.querySelector('.source-text')?.textContent).toBe('## Page 3\nSecond extracted page 🖋.\n');
    await expect.element(page.getByRole('combobox', { name: 'Page', exact: true })).toHaveValue('1');
    expect(ipc.read).toHaveBeenCalledWith('project', 'session', read.material.id);
  });
  it('does not infer PDF pages from text when the retained extraction has no mapping', async () => {
    const read = pdfSource();
    read.presentation!.pdf_pages = [];
    render({ material: read.material }, read);
    await expect.element(page.getByText('Page navigation is unavailable for this copy. The extracted text and original are retained.', { exact: true })).toBeVisible();
    expect(page.getByRole('combobox', { name: 'Page', exact: true }).query()).toBeNull();
    await expect.poll(() => document.querySelector('.source-text')?.textContent).toBe(read.text);
  });
  it('keeps insertion actions out of the header and always exposes closing the source', async () => {
    const { onClose } = render({ initialEvidence: evidence, originTitle: null });
    await expect.element(page.getByRole('button', { name: 'Close source' })).toBeVisible();
    expect(page.getByRole('button', { name: 'Use here' }).query()).toBeNull();
    expect(page.getByRole('button', { name: 'Insert reference' }).query()).toBeNull();
    await page.getByRole('button', { name: 'Close source' }).click();
    expect(onClose).toHaveBeenCalledOnce();
  });
  it('searches only on request, opens exact retained passages, and uses the passage instead of the collection', async () => {
    const { onUse } = render();
    await expect.poll(() => ipc.read.mock.calls.length).toBe(1);
    expect(ipc.search).not.toHaveBeenCalled();
    await page.getByRole('searchbox', { name: 'Search Research' }).fill('history');
    await page.getByRole('button', { name: 'Search', exact: true }).click();
    await page.getByRole('button', { name: /A source/ }).click();
    await expect.element(page.getByText('Exact café evidence.\nSecond line.', { exact: true })).toBeVisible();
    expect(ipc.evidence).toHaveBeenCalledWith('project', 'session', material.id, evidence.id);
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Insert reference' }).click();
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
    expect(ipc.remove).toHaveBeenCalledWith('project', 'session', material.id, material.metadata_revision);
    expect(onRemoved).not.toHaveBeenCalled();
    await expect.element(page.getByRole('button', { name: 'Remove from workspace' })).toBeDisabled();
    finish();
    await expect.poll(() => onRemoved.mock.calls.length).toBe(1);
    expect(onRemoved).toHaveBeenCalledWith(material.id, 'session', material);
    expect(onUse).not.toHaveBeenCalled();
    expect(ipc.original).not.toHaveBeenCalled();
  });
  it('reads retained evidence after the live library is unavailable without reopening it', async () => {
    const { onUse } = render({ material: { ...material, available: false }, initialEvidence: evidence });
    await expect.element(page.getByText('Exact café evidence.\nSecond line.', { exact: true })).toBeVisible();
    expect(ipc.read).not.toHaveBeenCalled();
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Insert reference' }).click();
    expect(onUse).toHaveBeenCalledOnce();
  });
  it('opens folder evidence without imported-source controls and keeps current writing navigation explicit', async () => {
    const retained = { ...evidence, locator: { kind: 'document_revision', project_id: 'project', document_id: 'writing-id', revision_id: 'old-revision', path: 'Notes/rain.md', start_byte: 2, end_byte: 40 } };
    const folder: MaterialEntry = { ...material, id: 'folder-' + 'c'.repeat(64), kind: 'folder', name: 'Notes', reference: '@"Notes/"', available: false, source_path: 'Notes/' };
    const onOpenDocument = vi.fn().mockResolvedValue(undefined);
    const { onUse } = render({ material: folder, initialEvidence: retained, onOpenDocument });
    await expect.element(page.getByText(retained.text, { exact: true })).toBeVisible();
    await expect.element(page.getByText('Notes/rain.md', { exact: true })).toBeVisible();
    expect(ipc.read).not.toHaveBeenCalled();
    expect(page.getByRole('button', { name: 'Choose library…' }).query()).toBeNull();
    await page.getByText('•••', { exact: true }).click();
    expect(page.getByRole('button', { name: 'Pin', exact: true }).query()).toBeNull();
    expect(page.getByRole('button', { name: 'Remove from workspace' }).query()).toBeNull();
    await page.getByRole('button', { name: 'Open current writing' }).click();
    expect(onOpenDocument).toHaveBeenCalledWith('writing-id');
    expect(onUse).not.toHaveBeenCalled();
    await expect.element(page.getByText(retained.text, { exact: true })).toBeVisible();
    await page.getByRole('button', { name: 'Insert quotation' }).click();
    expect(onUse).toHaveBeenCalledWith(`[@A source](loom-evidence:${retained.id})`, retained.text);
  });
  it('keeps a stale insertion failure in the source view without changing the selected passage', async () => {
    render({ initialEvidence: evidence, onUse: vi.fn().mockResolvedValue(false) });
    await page.getByText('•••', { exact: true }).click();
    await page.getByRole('button', { name: 'Insert reference' }).click();
    await expect.element(page.getByRole('alert')).toHaveTextContent('The writing changed');
    await expect.element(page.getByText('Exact café evidence.\nSecond line.', { exact: true })).toBeVisible();
  });
});
