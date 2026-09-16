<script lang="ts">
  import { onMount } from 'svelte';
  import { convertFileSrc } from '@tauri-apps/api/core';
  import { readMaterial, searchMaterial, readMaterialEvidence, revealAttachmentOriginal, normalizeFailure, pinMaterial, removeMaterial } from './ipc';
  import { materialLocatorLabel, materialLocatorPage, materialPdfPageText, materialWritingDocument, materialReferenceMarkdown, evidenceReferenceMarkdown, type MaterialEntry, type MaterialRead, type MaterialEvidence, type MaterialSearch } from './materials';

  export let projectId: string;
  export let sessionId: string;
  export let material: MaterialEntry;
  export let initialEvidence: MaterialEvidence | null = null;
  export let originTitle: string | null = null;
  export let onClose: () => void;
  export let onUse: (reference: string, text: string | null) => Promise<boolean>;
  export let removable = true;
  export let onRemoved: (id: string, session: string) => void = () => {};
  export let onChanged: (entry: MaterialEntry) => void;
  export let onReopen: () => void;
  export let onOpenDocument: ((id: string) => Promise<void>) | undefined = undefined;

  let source: MaterialRead | null = null;
  let results: MaterialSearch | null = null;
  let selected: MaterialEvidence | null = initialEvidence;
  let query = '';
  let error = '';
  let busy = false;
  let serial = 0;
  let mounted = false;
  let sourceTextElement: HTMLDivElement | undefined;
  let contentElement: HTMLDivElement | undefined;
  let pageIndex = 0;
  $: pdfPages = source?.presentation?.pdf_pages ?? [];
  $: pdfBytes = pdfPages.length && source ? new TextEncoder().encode(source.text) : null;
  $: currentPage = pdfPages[pageIndex];
  $: pageText = pdfBytes && currentPage ? materialPdfPageText(pdfBytes, currentPage) : null;
  $: isPdf = source?.presentation?.detected_format.toLowerCase() === 'pdf';
  $: selectedPage = materialLocatorPage(selected?.locator);
  $: reference = selected ? evidenceReferenceMarkdown(selected) : material.kind === 'folder' ? material.reference : materialReferenceMarkdown(material);
  $: writingDocumentId = materialWritingDocument(selected?.locator, projectId);
  $: text = selected?.text ?? pageText ?? source?.text ?? '';
  $: warnings = [...new Set([...(source?.warnings ?? []), ...(results?.warnings ?? []), ...(selected?.warnings ?? [])])];

  async function load(): Promise<void> {
    const request = ++serial; busy = true; error = '';
    try { const value = await readMaterial(projectId, sessionId, material.id); if (mounted && serial === request) source = value; }
    catch (failure) { if (mounted && serial === request) error = normalizeFailure(failure).message; }
    finally { if (mounted && serial === request) busy = false; }
  }
  async function search(): Promise<void> {
    if (!query.trim() || busy) return;
    const request = ++serial; busy = true; error = ''; selected = null;
    try { const value = await searchMaterial(projectId, sessionId, material.id, query.trim()); if (mounted && serial === request) results = value; }
    catch (failure) { if (mounted && serial === request) error = normalizeFailure(failure).message; }
    finally { if (mounted && serial === request) busy = false; }
  }
  async function openEvidence(hit: MaterialEvidence): Promise<void> {
    const request = ++serial; busy = true; error = '';
    try { const value = await readMaterialEvidence(projectId, sessionId, material.id, hit.id); if (mounted && serial === request) selected = value; }
    catch (failure) { if (mounted && serial === request) error = normalizeFailure(failure).message; }
    finally { if (mounted && serial === request) busy = false; }
  }
  function changePage(index: number): void {
    if (busy || !Number.isInteger(index) || index < 0 || index >= pdfPages.length) return;
    pageIndex = index;
    contentElement?.scrollTo(0, 0);
  }
  async function openSourcePage(): Promise<void> {
    if (!selectedPage || busy || !material.available || !material.attachment_id) return;
    const number = selectedPage;
    const request = ++serial; busy = true; error = '';
    try {
      const value = source ?? await readMaterial(projectId, sessionId, material.id);
      if (!mounted || serial !== request) return;
      const index = value.presentation?.pdf_pages?.findIndex(page => page.number === number) ?? -1;
      if (index < 0 || value.source_revision !== selected?.source_revision) {
        error = 'This retained passage has no matching extracted page in the available source.';
        return;
      }
      source = value; pageIndex = index; selected = null; results = null;
      contentElement?.scrollTo(0, 0);
    } catch (failure) { if (mounted && serial === request) error = normalizeFailure(failure).message; }
    finally { if (mounted && serial === request) busy = false; }
  }
  function quotationText(): string {
    const selection = window.getSelection();
    if (!sourceTextElement || sourceTextElement.textContent !== text || !selection ||
        selection.isCollapsed || selection.rangeCount !== 1) return text;
    const range = selection.getRangeAt(0);
    if (!sourceTextElement.contains(range.startContainer) || !sourceTextElement.contains(range.endContainer)) return text;
    // Derive offsets from DOM ranges, then slice the canonical source itself.
    // In particular, retain whitespace and Unicode exactly; selection elsewhere
    // (including a range crossing out of this source) never becomes an excerpt.
    const prefix = document.createRange();
    prefix.selectNodeContents(sourceTextElement);
    prefix.setEnd(range.startContainer, range.startOffset);
    const start = prefix.toString().length;
    return text.slice(start, start + range.toString().length);
  }

  async function use(contents = false): Promise<void> {
    if (busy || !originTitle) return;
    busy = true; error = '';
    try {
      const capturedReference = contents && !selected && source?.evidence[0] ? evidenceReferenceMarkdown(source.evidence[0]) : reference;
      if (!await onUse(capturedReference, contents ? quotationText() : null)) error = 'The writing changed. Return to your document and choose a new insertion point.'; }
    catch (failure) { if (mounted) error = normalizeFailure(failure).message; }
    finally { if (mounted) busy = false; }
  }
  async function pin(): Promise<void> {
    busy = true;
    try { const entry = await pinMaterial(projectId, sessionId, material.id, !material.pinned); if (mounted) { material = entry; onChanged(entry); } }
    catch (failure) { if (mounted) error = normalizeFailure(failure).message; }
    finally { if (mounted) busy = false; }
  }
  async function remove(): Promise<void> {
    if (busy || !removable) return;
    const id = material.id, session = sessionId;
    busy = true; error = '';
    try {
      await removeMaterial(projectId, session, id);
      // The removal may settle after the view closes. Its owner checks the
      // originating session before updating navigation, even after unmount.
      onRemoved(id, session);
    } catch (failure) { if (mounted) error = normalizeFailure(failure).message; }
    finally { if (mounted) busy = false; }
  }
  async function original(): Promise<void> {
    if (!material.attachment_id) return;
    try { await revealAttachmentOriginal(projectId, sessionId, material.attachment_id); }
    catch (failure) { if (mounted) error = normalizeFailure(failure).message; }
  }
  async function copyReference(): Promise<void> {
    try { await navigator.clipboard.writeText(reference); }
    catch { error = 'The reference could not be copied.'; }
  }
  async function openWriting(): Promise<void> {
    if (!writingDocumentId || !onOpenDocument || busy) return;
    busy = true; error = '';
    try { await onOpenDocument(writingDocumentId); }
    catch (failure) { if (mounted) error = normalizeFailure(failure).message; }
    finally { if (mounted) busy = false; }
  }
  onMount(() => { mounted = true; if (material.available && material.kind !== 'folder' && !initialEvidence) void load(); return () => { mounted = false; serial += 1; }; });
</script>

<section class="material-view" aria-label={material.name} aria-busy={busy}>
  <header>
    <h1>{selected?.title ?? material.name}</h1>
    {#if !selected && !results && pdfPages.length}
      <nav class="pages" aria-label="Extracted PDF pages">
        <button disabled={busy || pageIndex === 0} aria-label="Previous page" on:click={() => changePage(pageIndex - 1)}>‹</button>
        <select aria-label="Page" value={pageIndex} disabled={busy} on:change={event => changePage(Number(event.currentTarget.value))}>
          {#each pdfPages as pdfPage, index}<option value={index}>Page {pdfPage.number}</option>{/each}
        </select>
        <button disabled={busy || pageIndex >= pdfPages.length - 1} aria-label="Next page" on:click={() => changePage(pageIndex + 1)}>›</button>
      </nav>
    {/if}
    <details class="actions"><summary aria-label="Source actions" on:mousedown|preventDefault>•••</summary><div class="action-menu">
      {#if originTitle}<button disabled={busy || (!material.available && !selected)} on:click={() => void use()}>Insert reference</button>{/if}
      <button on:click={() => void copyReference()}>Copy reference</button>
      {#if text && originTitle}<button disabled={busy} on:mousedown|preventDefault on:click={() => void use(true)}>Insert quotation</button>{/if}
      {#if writingDocumentId && onOpenDocument}<button disabled={busy} on:click={() => void openWriting()}>Open current writing</button>{/if}
      {#if selectedPage && material.available && material.attachment_id}<button disabled={busy} on:click={() => void openSourcePage()}>Open page {selectedPage}</button>{/if}
      {#if material.attachment_id}<button on:click={() => void original()}>Reveal original</button>{/if}
      {#if material.available && material.kind !== 'folder'}<button disabled={busy} on:click={() => void pin()}>{material.pinned ? 'Unpin' : 'Pin'}</button>{/if}
      {#if removable && material.kind !== 'folder'}<button disabled={busy} on:click={() => void remove()}>Remove from workspace</button>{/if}
    </div></details>
    <button class="close" on:click={onClose} aria-label="Close source" title="Close source"><svg aria-hidden="true" viewBox="0 0 16 16"><path d="m4 4 8 8M12 4l-8 8" /></svg></button>
  </header>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if material.kind === 'library' && !material.available && !selected}
    <p class="notice">Choose this library again to open it on this device.</p>
    <button class="reopen" on:click={onReopen}>Choose library…</button>
  {:else}
    {#if material.kind === 'library' && !selected}
      <form class="search" on:submit|preventDefault={() => void search()}>
        <input type="search" bind:value={query} aria-label={`Search ${material.name}`} placeholder={`Search ${material.name}`} />
        <button disabled={busy || !query.trim()}>Search</button>
      </form>
    {/if}
    {#if selected && results}<button class="return-results" on:click={() => selected = null}>‹ Results</button>{/if}
    {#if warnings.length || (material.kind === 'attachment' && source?.complete === false) || results?.complete === false || selected?.complete === false}
      <details class="coverage"><summary>{(material.kind === 'attachment' && source?.complete === false) || selected?.complete === false ? 'Only part of this source is readable' : results?.complete === false ? 'Partial results' : 'Source notes'}</summary>
        {#each warnings as warning}<p>{warning}</p>{/each}
      </details>
    {/if}
    <div class="content" bind:this={contentElement}>
      {#if selected}
        <p class="location">{materialLocatorLabel(selected.locator)}</p>
        <div class="source-text" bind:this={sourceTextElement}>{selected.text}</div>
      {:else if results}
        <p class="result-count">{results.hits.length} matching {results.hits.length === 1 ? 'passage' : 'passages'}</p>
        {#each results.hits as hit (hit.id)}
          <button class="result" on:click={() => void openEvidence(hit)} disabled={busy}>
            <strong>{hit.title}</strong><span class="location">{materialLocatorLabel(hit.locator)}</span>
            <span class="excerpt">{hit.text}</span>
          </button>
        {/each}
      {:else if source}
        {#if isPdf && (!pdfPages.length || pageText === null)}<p class="notice">Page navigation is unavailable for this copy. The extracted text and original are retained.</p>{/if}
        {#each source.presentation?.media ?? [] as media (media.sha256)}
          {#if media.preview_token}
            {#if media.kind === 'image'}<img src={convertFileSrc(media.preview_token, 'loom-asset')} alt={material.name} />
            {:else}<audio controls preload="metadata" src={convertFileSrc(media.preview_token, 'loom-asset')} aria-label={`Play ${material.name}`}></audio>{/if}
          {/if}
        {/each}
        {#if source.text}<div class="source-text" bind:this={sourceTextElement}>{text}</div>
        {:else if material.kind === 'attachment' && !source.presentation?.media.length}<p class="notice">The original is retained. No readable text is available.</p>{/if}
      {:else if busy}<p class="notice" role="status">Opening…</p>{/if}
    </div>
  {/if}
</section>

<style>
  .material-view { display:flex; flex-direction:column; min-width:0; min-height:0; height:100%; color:inherit; }
  header { display:flex; gap:8px; align-items:center; padding:6px 10px; border-bottom:1px solid #8883; }
  h1 { margin:0; flex:1; min-width:0; font:inherit; font-weight:600; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
  button,input,select,summary { font:inherit; color:inherit; }
  .pages { display:flex; align-items:center; gap:2px; }
  .pages button { border:0; padding:2px 5px; }
  .pages select { background:transparent; border:0; max-width:100px; }
  button { cursor:pointer; border:1px solid #8883; border-radius:5px; background:transparent; padding:4px 8px; min-height:28px; }
  button:disabled { opacity:.5; cursor:default; }
  .close { display:grid; place-items:center; width:28px; min-width:28px; border:0; padding:4px; }
  .close:hover { background:var(--chrome-hover); }
  .close svg { width:14px; height:14px; stroke:currentColor; stroke-width:1.5; fill:none; }
  .actions { position:relative; }
  summary { cursor:pointer; padding:5px; list-style:none; }
  summary::-webkit-details-marker { display:none; }
  .action-menu { position:absolute; z-index:12; right:0; top:100%; min-width:160px; padding:4px; border:1px solid #8885; border-radius:6px; background:var(--paper); box-shadow:0 3px 12px #0002; }
  .action-menu button { display:block; width:100%; text-align:left; border:0; }
  .action-menu button:hover { background:#8882; }
  .search { display:flex; gap:6px; margin:12px; }
  input { flex:1; min-width:0; padding:7px 9px; border:1px solid #8884; border-radius:5px; background:transparent; }
  .content { flex:1; min-height:0; overflow:auto; padding:12px 18px; }
  .source-text { white-space:pre-wrap; overflow-wrap:anywhere; line-height:1.55; user-select:text; }
  .result { display:block; width:100%; margin:0 0 8px; text-align:left; border:0; border-bottom:1px solid #8883; border-radius:0; padding:8px 0 12px; }
  .result strong,.result span { display:block; }
  .excerpt { white-space:pre-wrap; line-height:1.4; display:-webkit-box !important; -webkit-line-clamp:4; line-clamp:4; -webkit-box-orient:vertical; overflow:hidden; margin-top:5px; }
  .location,.result-count { font-size:.8rem; opacity:.6; overflow-wrap:anywhere; }
  .coverage { margin:6px 12px; font-size:.85rem; opacity:.8; }
  .coverage p { margin:4px; }
  .notice { margin:12px; opacity:.65; }
  .error { margin:8px 12px; color:var(--danger,#b64238); white-space:pre-wrap; }
  .return-results,.reopen { align-self:flex-start; margin:8px 12px; }
  img { max-width:100%; height:auto; }
  audio { width:100%; max-width:520px; }
</style>
