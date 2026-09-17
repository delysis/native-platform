<script lang="ts">
  import { onMount } from 'svelte';
  import CollectionProgress from './CollectionProgress.svelte';
  import CollectionMembers from './CollectionMembers.svelte';
  import type { CollectionMember, MaterialPdfPage } from './types';
  import { convertFileSrc } from '@tauri-apps/api/core';
  import { readMaterialPdfPage, readMaterial, readCollectionMember, searchMaterial, readMaterialEvidence, revealAttachmentOriginal, normalizeFailure, pinMaterial, removeMaterial } from './ipc';
  import { materialLocatorLabel, materialLocatorPage, materialPdfPageText, materialWritingDocument, materialReferenceMarkdown, evidenceReferenceMarkdown, type MaterialEntry, type MaterialRead, type MaterialEvidence, type MaterialSearch, type MaterialNavigation } from './materials';

  export let projectId: string;
  export let sessionId: string;
  export let material: MaterialEntry;
  export let initialEvidence: MaterialEvidence | null = null;
  export let navigation: MaterialNavigation | undefined = undefined;
  export let onNavigationChange: (navigation: MaterialNavigation) => void = () => {};
  export let originTitle: string | null = null;
  export let onClose: () => void;
  export let onUse: (reference: string, text: string | null) => Promise<boolean>;
  export let removable = true;
  export let workspaceOwned = true;
  export let onRemoved: (id: string, session: string) => void = () => {};
  export let onChanged: (entry: MaterialEntry) => void;
  export let onReopen: () => void;
  export let onOpenDocument: ((id: string) => Promise<void>) | undefined = undefined;

  let source: MaterialRead | null = null;
  let results: MaterialSearch | null = null;
  let selected: MaterialEvidence | null = initialEvidence;
  let memberOpen = false;
  let memberTarget: MaterialNavigation['member'] = initialEvidence ? null : navigation?.member ?? null;
  let restoring = true;
  let collectionRevision = '';
  let query = navigation?.query ?? '';
  let error = '';
  let busy = false;
  let serial = 0;
  let mounted = false;
  let sourceTextElement: HTMLDivElement | undefined;
  let contentElement: HTMLDivElement | undefined;
  let pageIndex = navigation?.pageIndex ?? 0;
  let pdfImage: MaterialPdfPage | null = null;
  let pdfCount = 0;
  let pdfCountToken = '';
  let pdfText = navigation?.pdfText ?? false;
  let pdfLoading = false;
  let pdfError = '';
  let pdfRequestKey = '';
  let pdfSerial = 0;
  let pdfAbort: AbortController | undefined;
  $: pdfToken = source?.presentation?.pdf_preview_token ?? '';
  $: pdfPages = source?.presentation?.pdf_pages ?? [];
  $: pdfBytes = pdfPages.length && source ? new TextEncoder().encode(source.text) : null;
  $: pageNumber = pdfToken ? pageIndex + 1 : pdfPages[pageIndex]?.number ?? 1;
  $: pageNumbers = pdfToken ? (pdfCountToken === pdfToken && pdfCount ? Array.from({ length: pdfCount }, (_, i) => i + 1) : [pageNumber]) : pdfPages.map(page => page.number);
  $: currentPage = pdfPages.find(page => page.number === pageNumber);
  $: previewKey = mounted && pdfToken && !selected && !results && !pdfText ? pdfToken + ':' + pageNumber : '';
  $: if (previewKey !== pdfRequestKey) {
    pdfRequestKey = previewKey;
    void drawPdf(previewKey, pdfToken, pageNumber);
  }
  $: pageText = pdfBytes && currentPage ? materialPdfPageText(pdfBytes, currentPage) : null;
  $: isPdf = source?.presentation?.detected_format.toLowerCase() === 'pdf';
  $: selectedPage = materialLocatorPage(selected?.locator);
  $: selectedLocation = selected?.locator && typeof selected.locator === 'object' ? selected.locator as Record<string, unknown> : null;
  $: originalId = source?.material.attachment_id ?? (typeof selectedLocation?.attachment_id === 'string' ? selectedLocation.attachment_id : material.attachment_id);
  $: reference = selected ? evidenceReferenceMarkdown(selected) : memberOpen && source?.evidence[0] ? evidenceReferenceMarkdown(source.evidence[0]) : material.kind === 'folder' ? material.reference : materialReferenceMarkdown(material);
  $: writingDocumentId = materialWritingDocument(selected?.locator, projectId);
  $: text = selected?.text ?? (pdfToken && pdfPages.length ? pageText ?? '' : pageText ?? source?.text ?? '');
  $: warnings = [...new Set([...(source?.warnings ?? []), ...(results?.warnings ?? []), ...(selected?.warnings ?? [])])];
  $: if (mounted && !restoring) onNavigationChange({ query, pageIndex, pdfText, sourceRevision: source?.source_revision ?? null, pdfPageCount: pdfCountToken === pdfToken ? pdfCount : 0, evidenceId: selected?.id ?? null, member: memberTarget });

  function restoreSource(value: MaterialRead): void {
    source = value;
    if (navigation?.sourceRevision && navigation.sourceRevision !== value.source_revision) pageIndex = 0;
    const token = value.presentation?.pdf_preview_token;
    if (token && navigation?.sourceRevision === value.source_revision && navigation.pdfPageCount > 0) {
      pdfCount = navigation.pdfPageCount; pdfCountToken = token;
      pageIndex = Math.min(pageIndex, pdfCount - 1);
    } else if (!token) pageIndex = Math.min(pageIndex, Math.max(0, (value.presentation?.pdf_pages?.length ?? 0) - 1));
  }

  async function drawPdf(key: string, token: string, number: number): Promise<void> {
    const request = ++pdfSerial;
    pdfAbort?.abort();
    pdfAbort = new AbortController();
    pdfImage = null; pdfError = ''; pdfLoading = !!key;
    if (!key) return;
    try {
      const image = await readMaterialPdfPage(token, number, pdfAbort.signal);
      if (mounted && request === pdfSerial) { pdfImage = image; pdfCount = image.page_count; pdfCountToken = token; }
    } catch (failure) { if (mounted && request === pdfSerial) pdfError = normalizeFailure(failure).message; }
    finally { if (mounted && request === pdfSerial) pdfLoading = false; }
  }
  async function load(): Promise<void> {
    const request = ++serial; busy = true; error = '';
    try { const value = await readMaterial(projectId, sessionId, material.id); if (mounted && serial === request) restoreSource(value); }
    catch (failure) { if (mounted && serial === request) error = normalizeFailure(failure).message; }
    finally { if (mounted && serial === request) busy = false; }
  }
  async function search(): Promise<void> {
    if (!query.trim() || busy) return;
    const request = ++serial; busy = true; error = ''; selected = null;
    if (material.kind === 'collection') { source = null; memberOpen = false; memberTarget = null; }
    try { const value = await searchMaterial(projectId, sessionId, material.id, query.trim()); if (mounted && serial === request) results = value; }
    catch (failure) { if (mounted && serial === request) error = normalizeFailure(failure).message; }
    finally { if (mounted && serial === request) busy = false; }
  }
  async function openEvidence(hit: Pick<MaterialEvidence, 'id'>): Promise<void> {
    const request = ++serial; busy = true; error = '';
    try { const value = await readMaterialEvidence(projectId, sessionId, material.id, hit.id); if (mounted && serial === request) selected = value; }
    catch (failure) { if (mounted && serial === request) error = normalizeFailure(failure).message; }
    finally { if (mounted && serial === request) busy = false; }
  }
  async function openMember(member: CollectionMember): Promise<void> {
    const request = ++serial; busy = true; error = '';
    try {
      const value = await readCollectionMember(projectId, sessionId, material.id, member.occurrence_id, member.snapshot_id);
      if (mounted && request === serial) { source = value; selected = null; results = null; memberOpen = true; memberTarget = { occurrenceId: member.occurrence_id, snapshotId: member.snapshot_id }; pageIndex = 0; }
    } catch (failure) { if (mounted && request === serial) error = normalizeFailure(failure).message; }
    finally { if (mounted && request === serial) busy = false; }
  }
  function backToCollection(): void { ++serial; busy = false; selected = null; source = null; results = null; memberOpen = false; memberTarget = null; pageIndex = 0; }
  function changePage(index: number): void {
    if (busy || pdfLoading || !Number.isInteger(index) || index < 0 || index >= (pdfToken ? pdfCount : pdfPages.length)) return;
    pageIndex = index;
    contentElement?.scrollTo(0, 0);
  }
  async function openSourcePage(): Promise<void> {
    if (!selectedPage || busy || !material.available || !originalId) return;
    const number = selectedPage;
    const request = ++serial; busy = true; error = '';
    try {
      const location = selectedLocation;
      const value = source ?? (material.kind === 'collection' && typeof location?.collection_snapshot === 'string' && typeof location?.occurrence_id === 'string'
        ? await readCollectionMember(projectId, sessionId, material.id, location.occurrence_id, location.collection_snapshot)
        : await readMaterial(projectId, sessionId, material.id));
      if (!mounted || serial !== request) return;
      const index = value.presentation?.pdf_pages?.findIndex(page => page.number === number) ?? -1;
      if ((!value.presentation?.pdf_preview_token && index < 0) || value.source_revision !== selected?.source_revision) {
        error = 'This retained passage has no matching extracted page in the available source.';
        return;
      }
      source = value; memberOpen = material.kind === 'collection';
      memberTarget = memberOpen && typeof location?.collection_snapshot === 'string' && typeof location?.occurrence_id === 'string' ? { occurrenceId: location.occurrence_id, snapshotId: location.collection_snapshot } : null;
      pageIndex = value.presentation?.pdf_preview_token ? number - 1 : index; pdfText = false; selected = null; results = null;
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
    if (!originalId) return;
    try { await revealAttachmentOriginal(projectId, sessionId, originalId); }
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
  async function restoreNavigation(): Promise<void> {
    try {
      if (initialEvidence) return;
      if (navigation?.evidenceId) {
        await openEvidence({ id: navigation.evidenceId });
      } else if (memberTarget && material.kind === 'collection') {
        const request = ++serial; busy = true;
        try {
          const value = await readCollectionMember(projectId, sessionId, material.id, memberTarget.occurrenceId, memberTarget.snapshotId);
          if (mounted && request === serial) { restoreSource(value); memberOpen = true; }
        } catch (failure) {
          if (mounted && request === serial) { error = normalizeFailure(failure).message; memberTarget = null; }
        } finally { if (mounted && request === serial) busy = false; }
      } else if (material.available && material.kind !== 'folder') await load();
    } finally { if (mounted) restoring = false; }
  }
  onMount(() => { mounted = true; void restoreNavigation(); return () => { mounted = false; serial += 1; pdfSerial += 1; pdfAbort?.abort(); }; });
</script>

<section class="material-view" aria-label={material.name} aria-busy={busy}>
  <header>
    {#if material.kind === 'collection' && (selected || memberOpen || results)}<button on:click={backToCollection} aria-label="Back to collection">‹</button>{/if}
    <h1>{selected?.title ?? (memberOpen ? source?.material.name : material.name)}</h1>
    {#if !selected && !results && (pdfToken || pdfPages.length)}
      <nav class="pages" aria-label={pdfToken ? "PDF pages" : "Extracted PDF pages"}>
        <button disabled={busy || pdfLoading || pageIndex === 0} aria-label="Previous page" on:click={() => changePage(pageIndex - 1)}>‹</button>
        <select aria-label="Page" value={pageIndex} disabled={busy || pdfLoading} on:change={event => changePage(Number(event.currentTarget.value))}>
          {#each pageNumbers as number, index}<option value={pdfToken ? number - 1 : index}>Page {number}</option>{/each}
        </select>
        <button disabled={busy || pdfLoading || pageIndex >= (pdfToken ? pdfCount : pdfPages.length) - 1} aria-label="Next page" on:click={() => changePage(pageIndex + 1)}>›</button>
      </nav>
    {/if}
    <details class="actions"><summary aria-label="Source actions" on:mousedown|preventDefault>•••</summary><div class="action-menu">
      {#if originTitle}<button disabled={busy || (!material.available && !selected)} on:click={() => void use()}>Insert reference</button>{/if}
      <button on:click={() => void copyReference()}>Copy reference</button>
      {#if text && originTitle && (!pdfToken || pdfText || selected)}<button disabled={busy} on:mousedown|preventDefault on:click={() => void use(true)}>Insert quotation</button>{/if}
      {#if writingDocumentId && onOpenDocument}<button disabled={busy} on:click={() => void openWriting()}>Open current writing</button>{/if}
      {#if selectedPage && material.available && originalId}<button disabled={busy} on:click={() => void openSourcePage()}>Open page {selectedPage}</button>{/if}
      {#if pdfToken && !selected && !results}<button on:click={() => pdfText = !pdfText}>{pdfText ? "Show original page" : "Show extracted text"}</button>{/if}
      {#if originalId}<button on:click={() => void original()}>Reveal original</button>{/if}
      {#if workspaceOwned && material.available && material.kind !== 'folder'}<button disabled={busy} on:click={() => void pin()}>{material.pinned ? 'Unpin' : 'Pin'}</button>{/if}
      {#if removable && material.kind !== 'folder'}<button disabled={busy} on:click={() => void remove()}>Remove from workspace</button>{/if}
    </div></details>
    <button class="close" on:click={onClose} aria-label="Close source" title="Close source"><svg aria-hidden="true" viewBox="0 0 16 16"><path d="m4 4 8 8M12 4l-8 8" /></svg></button>
  </header>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  {#if material.kind === 'library' && !material.available && !selected}
    <p class="notice">Choose this library again to open it on this device.</p>
    <button class="reopen" on:click={onReopen}>Choose library…</button>
  {:else}
    {#if (material.kind === 'library' || material.kind === 'collection') && !selected && !memberOpen}
      <form class="search" on:submit|preventDefault={() => void search()}>
        <input type="search" bind:value={query} aria-label={`Search ${material.name}`} placeholder={`Search ${material.name}`} />
        <button disabled={busy || !query.trim()}>Search</button>
      </form>
    {/if}
    {#if material.kind === 'collection' && !selected && !memberOpen}
      <div class="collection-state"><CollectionProgress {projectId} {sessionId} collectionId={material.id} onChanged={status => collectionRevision = `${status.retained_count}:${status.phase}`} /></div>
    {/if}
    {#if selected && (results || material.kind === 'library')}<button class="return-results" on:click={() => selected = null}>{results ? '‹ Results' : '‹ Search'}</button>{/if}
    {#if warnings.length || ((material.kind === 'attachment' || memberOpen) && source?.complete === false) || results?.complete === false || selected?.complete === false}
      <details class="coverage"><summary>{((material.kind === 'attachment' || memberOpen) && source?.complete === false) || selected?.complete === false ? 'Only part of this source is readable' : results?.complete === false ? 'Partial results' : 'Source notes'}</summary>
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
      {:else if material.kind === 'collection' && !memberOpen}
        <CollectionMembers {projectId} {sessionId} collectionId={material.id} revision={collectionRevision} onOpenMember={member => void openMember(member)} />
      {:else if source}
        {#if isPdf && !pdfToken && (!pdfPages.length || pageText === null)}<p class="notice">Page navigation is unavailable for this copy. The extracted text and original are retained.</p>{/if}
        {#if pdfToken && !pdfText}
          {#if pdfLoading}<p class="notice" role="status">Opening page…</p>
          {:else if pdfError}<p class="error" role="alert">{pdfError}</p><button on:click={() => void drawPdf(previewKey, pdfToken, pageNumber)}>Try again</button>
          {:else if pdfImage}
            {#if pdfImage.incomplete}<p class="notice">Some page content could not be drawn. The original is retained.</p>{/if}
            <img class="pdf-page" src={'data:image/png;base64,' + pdfImage.png_base64} width={pdfImage.width} height={pdfImage.height} alt={source.material.name + ', page ' + pdfImage.page} />
          {/if}
        {:else}
        {#if pdfToken && !pdfPages.length && source.text}<p class="notice">The extracted text has no page mapping. Showing the whole document.</p>{/if}
        {#each source.presentation?.media ?? [] as media (media.sha256)}
          {#if media.preview_token}
            {#if media.kind === 'image'}<img src={convertFileSrc(media.preview_token, 'loom-asset')} alt={material.name} />
            {:else}<audio controls preload="metadata" src={convertFileSrc(media.preview_token, 'loom-asset')} aria-label={`Play ${material.name}`}></audio>{/if}
          {/if}
        {/each}
        {#if text}<div class="source-text" bind:this={sourceTextElement}>{text}</div>
        {:else if pdfToken}<p class="notice">No text was extracted from this page.</p>
        {:else if material.kind === 'attachment' && !source.presentation?.media.length}<p class="notice">The original is retained. No readable text is available.</p>{/if}
        {/if}
      {:else if busy}<p class="notice" role="status">Opening…</p>{/if}
    </div>
  {/if}
</section>

<style>
  .collection-state { margin:6px 12px; font-size:.85rem; }
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
  .pdf-page { display:block; margin:0 auto; object-fit:contain; }
  audio { width:100%; max-width:520px; }
</style>
