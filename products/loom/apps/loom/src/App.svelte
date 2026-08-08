<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import LoomEditor from './lib/LoomEditor.svelte';
  import {
    checkpointDocument,
    clearTransientDraft,
    applyDocumentReconciliation,
    chooseAndCreateProject,
    chooseAndOpenProject,
    closeProject as closeProjectSession,
    currentProjectSession,
    isDesktopRuntime,
    listModels,
    openDocument,
    previewDocumentReconciliation,
    recoverProject,
    requestApplicationClose,
    setFocusMode,
    normalizeFailure,
    upsertTransientDraft
  } from './lib/ipc';
  import {
    decodeVerseForEditor,
    encodeVerseFromEditor,
    type VerseEditorCodec
  } from './lib/verseCodec';
  import { canRoundTripMarkdownExactly } from './lib/markdownSafety';
  import { writeRebindsStaleDraft } from './lib/draftRecovery';
  import { documentProjectionDecision } from './lib/projectionState';
  import {
    captureForIdempotentRetry,
    closeResultMayHaveCommitted
  } from './lib/sessionSafety';
  import { newUlid } from './lib/ulid';
  import type {
    BranchCard,
    CommandReceipt,
    DocumentKind,
    DocumentSummary,
    EditorMode,
    ModelCapabilitySummary,
    LoomFailure,
    OpenDocument,
    ProjectSnapshot,
    ReconciliationPreview,
    SaveState,
    TransientDraftSnapshot
  } from './lib/types';

  let desktop = false;
  let project: ProjectSnapshot | null = null;
  let document: OpenDocument | null = null;
  let documentText = '';
  let mode: EditorMode = 'visual';
  let saveState: SaveState = 'clean';
  let saveMessage = 'No project open';
  let errorMessage = '';
  let lastFailure: LoomFailure | null = null;
  let projectTitle = 'Untitled Loom';
  let creating = false;
  let opening = false;
  let focusMode = false;
  let search = '';
  let models: ModelCapabilitySummary[] = [];
  let branches: BranchCard[] = [];
  let saveTimer: number | undefined;
  let saveInFlight: Promise<void> | null = null;
  let saveQueued = false;
  let documentEpoch = 0;
  let editVersion = 0;
  let savedVersion = 0;
  let liveRegion = '';
  let sourceDisplayText = '';
  let verseCodec: VerseEditorCodec | null = null;
  let compositionActive = false;
  let sourceComposing = false;
  let visualEditor: { flushPending: () => boolean } | null = null;
  let allowWindowClose = false;
  let unlistenWindowClose: (() => void) | undefined;
  let unlistenWindowFocus: (() => void) | undefined;
  let transition: 'idle' | 'navigation' | 'closing' = 'idle';
  let navigationSerial = 0;
  let uncertainSave: SaveCapture | null = null;
  let sourceProjectionTimer: number | undefined;
  let sourceDirty = false;
  let draftVersion = '0';
  let draftSavedEditVersion = 0;
  let draftTimer: number | undefined;
  let draftInFlight: Promise<boolean> | null = null;
  let staleDraft: TransientDraftSnapshot | null = null;
  let staleDraftRestoring = false;
  let staleDraftDiscardArmed = false;
  let uncertainDraft: DraftCapture | null = null;
  let pendingCloseCommandId: string | null = null;
  let reconciliation: ReconciliationPreview | null = null;
  let reconciliationResolution = '';
  let pendingReconciliationApply: ReconciliationApplyCapture | null = null;
  let reconciliationApplying = false;

  interface SaveCapture {
    commandId: string;
    documentEpoch: number;
    projectId: string;
    sessionId: string;
    documentId: string;
    relativePath: string;
    kind: DocumentKind;
    revisionId: string;
    visibleBlobId: string;
    draftVersion: string;
    text: string;
    editVersion: number;
  }

  interface DraftCapture {
    epoch: number;
    projectId: string;
    sessionId: string;
    documentId: string;
    relativePath: string;
    kind: DocumentKind;
    sourceRevisionId: string;
    expectedVersion: string;
    text: string;
    editVersion: number;
  }

  interface ReconciliationApplyCapture {
    commandId: string;
    projectId: string;
    sessionId: string;
    preview: ReconciliationPreview;
    resolvedText: string;
    reason: string;
  }

  const saveDelayMs = 900;
  const draftIntervalMs = 750;

  $: visibleDocuments = project?.documents.filter((candidate) => {
    const query = search.trim().toLocaleLowerCase();
    return !query || candidate.title.toLocaleLowerCase().includes(query) || candidate.relative_path.toLocaleLowerCase().includes(query);
  }) ?? [];
  $: wordCount = countWords(documentText);
  $: characterCount = [...documentText].length;
  $: currentModel = models.find((model) => model.loaded && model.completion);
  $: canUseVisual = Boolean(
    document?.summary.kind === 'prose' && canRoundTripMarkdownExactly(documentText)
  );
  $: canWeave = Boolean(
    document &&
      currentModel &&
      !focusMode &&
      !compositionActive &&
      !saveInFlight &&
      editVersion === savedVersion
  );
  $: showVisual = mode === 'visual' || mode === 'split';
  $: showSource = mode === 'source' || mode === 'split';
  $: exactTextSurface = document?.summary.kind === 'verse';
  $: editorReadonly = transition !== 'idle' || staleDraft !== null || staleDraftRestoring || uncertainDraft !== null || uncertainSave !== null || reconciliation !== null;
  $: reconciliationResolutionLocked = reconciliationApplying || pendingReconciliationApply !== null;
  $: reconciliationResolutionIsExact = Boolean(
    reconciliation && (
      reconciliation.kind === 'prose' ||
      reconciliationResolution === reconciliation.app_text ||
      reconciliationResolution === reconciliation.external_text ||
      (reconciliation.outcome.status === 'merged' && reconciliationResolution === reconciliation.outcome.content)
    )
  );

  onMount(() => {
    desktop = isDesktopRuntime();
    if (desktop) {
      void refreshModels();
      void installWindowLifecycleHandlers();
      void reattachNativeProject();
    }
    window.addEventListener('keydown', handleGlobalKeydown);
    return () => {
      window.removeEventListener('keydown', handleGlobalKeydown);
      if (saveTimer !== undefined) window.clearTimeout(saveTimer);
      if (sourceProjectionTimer !== undefined) window.clearTimeout(sourceProjectionTimer);
      if (draftTimer !== undefined) window.clearTimeout(draftTimer);
      unlistenWindowClose?.();
      unlistenWindowFocus?.();
    };
  });

  function countWords(text: string): number {
    return text.match(/[\p{L}\p{N}]+(?:[’'-][\p{L}\p{N}]+)*/gu)?.length ?? 0;
  }

  function clearFailure(): void {
    errorMessage = '';
    lastFailure = null;
  }

  function recordFailure(error: unknown): LoomFailure {
    const failure = normalizeFailure(error);
    errorMessage = failure.message;
    lastFailure = failure;
    return failure;
  }

  function recordLocalFailure(code: string, message: string): void {
    lastFailure = { code, message, retryable: false };
    errorMessage = message;
  }

  function clearReconciliationState(): void {
    reconciliation = null;
    reconciliationResolution = '';
    pendingReconciliationApply = null;
    reconciliationApplying = false;
  }

  function detachDocumentForReconciliation(): void {
    if (saveTimer !== undefined) {
      window.clearTimeout(saveTimer);
      saveTimer = undefined;
    }
    if (draftTimer !== undefined) {
      window.clearTimeout(draftTimer);
      draftTimer = undefined;
    }
    if (sourceProjectionTimer !== undefined) {
      window.clearTimeout(sourceProjectionTimer);
      sourceProjectionTimer = undefined;
    }
    if (document) documentEpoch += 1;
    document = null;
    documentText = '';
    sourceDisplayText = '';
    sourceDirty = false;
    verseCodec = null;
    visualEditor = null;
    compositionActive = false;
    sourceComposing = false;
    editVersion = 0;
    savedVersion = 0;
    draftVersion = '0';
    draftSavedEditVersion = 0;
    staleDraft = null;
    staleDraftRestoring = false;
    staleDraftDiscardArmed = false;
    uncertainDraft = null;
    uncertainSave = null;
  }

  function activateReconciliation(preview: ReconciliationPreview): void {
    if (
      !project ||
      preview.project_id !== project.project_id ||
      preview.session_id !== project.session_id
    ) {
      throw new Error('The desktop returned a reconciliation preview for another project session.');
    }
    // A reconciliation preview is a complete, source-bound working state. Do
    // not leave the stale editor mounted: blur handlers and Cmd/Ctrl-S must not
    // be able to issue another checkpoint against the superseded file bytes.
    detachDocumentForReconciliation();
    reconciliation = preview;
    reconciliationResolution = preview.outcome.status === 'merged'
      ? preview.outcome.content
      : preview.app_text;
    pendingReconciliationApply = null;
    saveState = 'error';
    saveMessage = 'External change held for explicit review';
    clearFailure();
    announce('External manuscript change held for review; nothing has been overwritten');
  }

  async function requestReconciliationPreview(
    summary: Pick<DocumentSummary, 'document_id' | 'relative_path' | 'kind' | 'revision_id' | 'active_blob_id'>,
    appText: string | null
  ): Promise<ReconciliationPreview> {
    if (!project || !summary.revision_id || !summary.active_blob_id) {
      throw new Error('External reconciliation requires an immutable source revision and base blob.');
    }
    const preview = await previewDocumentReconciliation(
      project.project_id,
      project.session_id,
      summary.document_id,
      summary.relative_path,
      summary.revision_id,
      summary.active_blob_id,
      appText
    );
    if (
      preview.project_id !== project.project_id ||
      preview.session_id !== project.session_id ||
      preview.document_id !== summary.document_id ||
      preview.relative_path !== summary.relative_path ||
      preview.kind !== summary.kind ||
      preview.active_revision_id !== summary.revision_id ||
      preview.base_blob_id !== summary.active_blob_id
    ) {
      throw new Error('The desktop reconciliation preview does not match the requested document source.');
    }
    return preview;
  }

  async function activateCheckpointProjectionConflict(
    captured: SaveCapture,
    receipt: CommandReceipt,
    appText: string
  ): Promise<void> {
    if (!receipt.result_revision_id || !receipt.result_blob_id) {
      throw new Error('The committed checkpoint receipt is missing its result identity.');
    }
    const refreshed = await currentProjectSession();
    if (
      refreshed.project_id !== captured.projectId ||
      refreshed.session_id !== captured.sessionId
    ) {
      throw new Error('The refreshed project does not match the committed checkpoint session.');
    }
    const target = refreshed.documents.find(
      (candidate) => candidate.document_id === captured.documentId
    );
    if (
      !target ||
      target.revision_id !== receipt.result_revision_id ||
      target.active_blob_id !== receipt.result_blob_id ||
      target.kind !== captured.kind ||
      target.relative_path !== captured.relativePath
    ) {
      throw new Error('The project did not expose the newly committed checkpoint identity.');
    }
    project = refreshed;
    let reboundDraftVersion: string | null = null;
    if (appText !== captured.text) {
      const rebound = await upsertTransientDraft(
        captured.projectId,
        captured.sessionId,
        captured.documentId,
        captured.relativePath,
        appText,
        captured.kind,
        receipt.result_revision_id,
        '0'
      );
      if (
        rebound.document_id !== captured.documentId ||
        rebound.source_revision_id !== receipt.result_revision_id ||
        rebound.kind !== captured.kind ||
        rebound.version === '0'
      ) {
        throw new Error('The desktop did not bind the newer editor text to the committed checkpoint.');
      }
      reboundDraftVersion = rebound.version;
    }
    const preview = await requestReconciliationPreview(target, null);
    if (
      reboundDraftVersion &&
      (
        preview.app_source !== 'transient_draft' ||
        preview.draft_version !== reboundDraftVersion ||
        preview.app_text !== appText
      )
    ) {
      throw new Error('The reconciliation preview omitted the newly rebound transient draft.');
    }
    activateReconciliation(preview);
    announce('The checkpoint is in history, but the changed visible file still needs reconciliation');
  }

  async function activateReconciliationProjectionConflict(
    captured: ReconciliationApplyCapture,
    receipt: CommandReceipt
  ): Promise<void> {
    const refreshed = await currentProjectSession();
    if (
      refreshed.project_id !== captured.projectId ||
      refreshed.session_id !== captured.sessionId
    ) {
      throw new Error('The refreshed project does not match the committed reconciliation session.');
    }
    const target = refreshed.documents.find(
      (candidate) => candidate.document_id === captured.preview.document_id
    );
    if (
      !target ||
      target.revision_id !== receipt.result_revision_id ||
      target.active_blob_id !== receipt.result_blob_id
    ) {
      throw new Error('The project did not expose the newly committed reconciliation identity.');
    }
    project = refreshed;
    const preview = await requestReconciliationPreview(target, captured.resolvedText);
    activateReconciliation(preview);
    announce('The resolution is in history; a newer external change now needs review');
  }

  async function installWindowLifecycleHandlers(): Promise<void> {
    const appWindow = getCurrentWindow();
    unlistenWindowFocus = await appWindow.onFocusChanged(({ payload: focused }) => {
      if (!focused && !compositionActive && !reconciliation) {
        flushEditors();
        void saveNow();
      }
    });
    unlistenWindowClose = await appWindow.onCloseRequested((event) => {
      if (allowWindowClose) return;
      event.preventDefault();
      void closeWindowGracefully();
    });
  }

  async function closeWindowGracefully(): Promise<void> {
    if (compositionActive) {
      recordLocalFailure('composition_active', 'Finish the active text composition before closing Loom.');
      announce(errorMessage);
      return;
    }
    if (project && !(await closeProject())) return;
    try {
      await requestApplicationClose();
      allowWindowClose = true;
    } catch (error) {
      allowWindowClose = false;
      recordFailure(error);
    }
  }

  async function refreshModels(): Promise<void> {
    try {
      models = await listModels();
    } catch {
      models = [];
    }
  }

  async function reattachNativeProject(): Promise<boolean> {
    try {
      const current = await currentProjectSession();
      project = current;
      await finishOpeningProject();
      announce(`Reattached ${current.title}`);
      return true;
    } catch {
      return false;
    }
  }

  async function doCreateProject(): Promise<void> {
    if (!projectTitle.trim()) return;
    creating = true;
    clearFailure();
    try {
      project = await chooseAndCreateProject(projectTitle.trim());
      await finishOpeningProject();
      announce(`Created ${project.title}`);
    } catch (error) {
      if (!(await reattachNativeProject())) recordFailure(error);
    } finally {
      creating = false;
    }
  }

  async function doOpenProject(): Promise<void> {
    opening = true;
    clearFailure();
    try {
      project = await chooseAndOpenProject();
      await finishOpeningProject();
      announce(`Opened ${project.title}`);
    } catch (error) {
      if (!(await reattachNativeProject())) recordFailure(error);
    } finally {
      opening = false;
    }
  }

  async function finishOpeningProject(): Promise<void> {
    if (!project) {
      transition = 'idle';
      return;
    }
    if (project.pending_recovery > 0) {
      const report = await recoverProject(project.project_id, project.session_id);
      if (report.conflicts.length > 0) {
        project = { ...project, pending_recovery: report.conflicts.length };
        document = null;
        recordLocalFailure('recovery_conflict', `Recovery stopped at ${report.conflicts.length} externally changed file${report.conflicts.length === 1 ? '' : 's'}: ${report.conflicts.join(', ')}`);
        announce('Recovery requires reconciliation before editing');
        return;
      }
      announce(`Recovered ${report.recovered} interrupted save${report.recovered === 1 ? '' : 's'}`);
      project = { ...project, pending_recovery: 0 };
    }
    const first = project.documents[0];
    if (first) await selectDocument(first);
    else {
      documentEpoch += 1;
      document = null;
      documentText = '';
      sourceDisplayText = '';
      verseCodec = null;
      editVersion = 0;
      savedVersion = 0;
      draftVersion = '0';
      draftSavedEditVersion = 0;
      staleDraft = null;
      uncertainDraft = null;
      clearReconciliationState();
      saveState = 'clean';
      saveMessage = 'Project is ready';
    }
  }

  async function selectDocument(summary: DocumentSummary): Promise<void> {
    if (transition !== 'idle') return;
    if (compositionActive) {
      announce('Finish composing text before changing documents');
      return;
    }
    if (!flushEditors()) return;
    transition = 'navigation';
    announce('Opening document; editing is briefly locked');
    const requestSerial = ++navigationSerial;
    if (!(await flushDraftJournal())) {
      if (requestSerial === navigationSerial) transition = 'idle';
      return;
    }
    if (!(await flushCurrentDocument())) {
      if (requestSerial === navigationSerial) transition = 'idle';
      return;
    }
    if (!project) {
      transition = 'idle';
      return;
    }
    const source = {
      epoch: documentEpoch,
      version: editVersion,
      documentId: document?.summary.document_id ?? null
    };
    clearFailure();
    try {
      if (summary.externally_modified) {
        const preview = await requestReconciliationPreview(summary, null);
        if (
          requestSerial !== navigationSerial ||
          documentEpoch !== source.epoch ||
          editVersion !== source.version ||
          (document?.summary.document_id ?? null) !== source.documentId
        ) {
          throw new Error('The active document changed while Loom prepared reconciliation.');
        }
        documentEpoch += 1;
        document = null;
        documentText = '';
        sourceDisplayText = '';
        verseCodec = null;
        editVersion = 0;
        savedVersion = 0;
        draftVersion = '0';
        draftSavedEditVersion = 0;
        staleDraft = null;
        uncertainDraft = null;
        activateReconciliation(preview);
        return;
      }
      const opened = await openDocument(
        project.project_id,
        project.session_id,
        summary.document_id,
        summary.relative_path
      );
      if (opened.summary.document_id !== summary.document_id) {
        throw new Error('The desktop returned a different document identity.');
      }
      if (summary.active_blob_id && opened.visible_blob_id !== summary.active_blob_id) {
        throw new Error('The desktop returned document bytes from a different active revision.');
      }
      if (
        requestSerial !== navigationSerial ||
        documentEpoch !== source.epoch ||
        editVersion !== source.version ||
        (document?.summary.document_id ?? null) !== source.documentId
      ) {
        throw new Error('The active document changed while Loom was opening another document.');
      }
      documentEpoch += 1;
      if (draftTimer !== undefined) {
        window.clearTimeout(draftTimer);
        draftTimer = undefined;
      }
      draftVersion = opened.transient_draft?.version ?? '0';
      draftSavedEditVersion = 0;
      staleDraft = null;
      uncertainDraft = null;
      const draft = opened.transient_draft;
      const draftIsCurrent = Boolean(
        draft &&
          draft.document_id === opened.summary.document_id &&
          draft.source_revision_id === opened.summary.revision_id &&
          draft.kind === opened.summary.kind
      );
      const effectiveText = draftIsCurrent && draft ? draft.text : opened.text;
      document = { ...opened, text: effectiveText };
      documentText = effectiveText;
      setSourceDocument(effectiveText, opened.summary.kind);
      editVersion = draftIsCurrent ? 1 : 0;
      savedVersion = 0;
      draftSavedEditVersion = draftIsCurrent ? 1 : 0;
      uncertainSave = null;
      clearReconciliationState();
      if (draft && !draftIsCurrent) {
        staleDraft = draft;
        saveState = 'error';
        saveMessage = 'A draft from another source revision needs reconciliation';
        recordLocalFailure(
          'stale_transient_draft',
          'Loom preserved a transient draft from another source revision and locked editing until it can be reconciled.'
        );
      } else if (draftIsCurrent) {
        saveState = 'dirty';
        saveMessage = 'Recovered a crash-safe local draft · checkpoint pending';
        scheduleSave();
        announce(`Recovered a local draft for ${summary.title}`);
      } else {
        saveState = 'clean';
        saveMessage = 'All changes saved';
        announce(`Opened ${summary.title}`);
      }
      if (summary.kind !== 'prose' || !canRoundTripMarkdownExactly(effectiveText)) mode = 'source';
    } catch (error) {
      recordFailure(error);
    } finally {
      if (requestSerial === navigationSerial) transition = 'idle';
    }
  }

  function updateText(text: string): void {
    if (transition !== 'idle') return;
    if (text === documentText) return;
    documentText = text;
    editVersion += 1;
    saveState = 'dirty';
    saveMessage = saveInFlight ? 'Saving earlier changes…' : 'Unsaved changes';
    scheduleDraftJournal();
    scheduleSave();
  }

  function setSourceDocument(text: string, kind: DocumentKind): void {
    if (sourceProjectionTimer !== undefined) {
      window.clearTimeout(sourceProjectionTimer);
      sourceProjectionTimer = undefined;
    }
    sourceDirty = false;
    if (kind === 'verse') {
      const decoded = decodeVerseForEditor(text);
      verseCodec = decoded.codec;
      sourceDisplayText = decoded.display;
    } else {
      verseCodec = null;
      sourceDisplayText = text;
    }
  }

  function flushEditors(): boolean {
    if (!(visualEditor?.flushPending() ?? true) || sourceComposing) return false;
    commitSourceDraft();
    return true;
  }

  function beginSourceComposition(): void {
    sourceComposing = true;
    compositionActive = true;
  }

  function finishSourceComposition(event: CompositionEvent & { currentTarget: HTMLTextAreaElement }): void {
    sourceComposing = false;
    compositionActive = false;
    updateFromSource(event.currentTarget.value);
    scheduleSourceProjection(0);
    announce('Text composition committed');
  }

  function updateFromSource(display: string): void {
    if (transition !== 'idle') return;
    sourceDisplayText = display;
    sourceDirty = true;
    if (!sourceComposing) scheduleSourceProjection();
  }

  function scheduleSourceProjection(delay = 240): void {
    if (sourceProjectionTimer !== undefined) return;
    sourceProjectionTimer = window.setTimeout(commitSourceDraft, delay);
  }

  function commitSourceDraft(): void {
    if (!sourceDirty || sourceComposing || transition !== 'idle') return;
    if (sourceProjectionTimer !== undefined) {
      window.clearTimeout(sourceProjectionTimer);
      sourceProjectionTimer = undefined;
    }
    if (document?.summary.kind === 'hybrid') return;
    sourceDirty = false;
    if (document?.summary.kind === 'verse') {
      if (!verseCodec?.editable) return;
      updateText(encodeVerseFromEditor(sourceDisplayText, verseCodec));
    } else {
      updateText(sourceDisplayText);
    }
  }

  function setVisualComposition(active: boolean): void {
    compositionActive = active;
    if (!active) scheduleSave();
  }

  function scheduleSave(): void {
    if (!desktop || !document) return;
    if (saveTimer !== undefined) window.clearTimeout(saveTimer);
    saveTimer = window.setTimeout(() => void saveNow(), saveDelayMs);
  }

  function scheduleDraftJournal(delay = draftIntervalMs): void {
    if (
      !desktop ||
      !project ||
      !document ||
      document.summary.kind === 'hybrid' ||
      staleDraft ||
      draftTimer !== undefined ||
      draftInFlight
    ) return;
    draftTimer = window.setTimeout(() => {
      draftTimer = undefined;
      void persistTransientDraft();
    }, delay);
  }

  async function persistTransientDraft(): Promise<boolean> {
    if (draftInFlight) {
      return draftInFlight;
    }
    if (saveInFlight) {
      scheduleDraftJournal(250);
      return false;
    }
    if (
      !project ||
      !document ||
      !document.summary.revision_id ||
      document.summary.kind === 'hybrid' ||
      (draftSavedEditVersion >= editVersion && !uncertainDraft)
    ) return true;
    const captured: DraftCapture = uncertainDraft ?? {
      epoch: documentEpoch,
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id,
      relativePath: document.summary.relative_path,
      kind: document.summary.kind,
      sourceRevisionId: document.summary.revision_id,
      expectedVersion: draftVersion,
      text: documentText,
      editVersion
    };
    const operation = (async (): Promise<boolean> => {
      try {
        const draft = await upsertTransientDraft(
          captured.projectId,
          captured.sessionId,
          captured.documentId,
          captured.relativePath,
          captured.text,
          captured.kind,
          captured.sourceRevisionId,
          captured.expectedVersion
        );
        if (
          draft.document_id !== captured.documentId ||
          draft.source_revision_id !== captured.sourceRevisionId ||
          draft.kind !== captured.kind
        ) {
          throw new Error('The desktop returned a transient draft for a different document source.');
        }
        if (draft.version === captured.expectedVersion) {
          throw new Error('The desktop did not advance the transient draft identity.');
        }
        if (
          documentEpoch !== captured.epoch ||
          project?.project_id !== captured.projectId ||
          project.session_id !== captured.sessionId ||
          document?.summary.document_id !== captured.documentId
        ) return true;
        const completedUncertainWrite = uncertainDraft === captured;
        const reboundStaleDraft = writeRebindsStaleDraft(staleDraft, captured);
        draftVersion = draft.version;
        draftSavedEditVersion = Math.max(draftSavedEditVersion, captured.editVersion);
        if (completedUncertainWrite) uncertainDraft = null;
        if (reboundStaleDraft) {
          staleDraft = null;
          staleDraftDiscardArmed = false;
        }
        clearFailure();
        if (completedUncertainWrite || reboundStaleDraft || saveState === 'error' || saveState === 'uncertain') {
          saveState = 'dirty';
        }
        if (saveState === 'dirty') {
          saveMessage = 'Draft protected locally · checkpoint pending';
          scheduleSave();
        }
        return true;
      } catch (error) {
        if (documentEpoch !== captured.epoch) return true;
        const failure = recordFailure(error);
        const retryCapture = captureForIdempotentRetry(captured, failure);
        if (retryCapture) {
          uncertainDraft = retryCapture;
          saveState = 'uncertain';
          saveMessage = 'Draft result uncertain — retry safely';
          announce('Draft result uncertain; editing is locked until the identical draft write is retried');
        } else {
          if (uncertainDraft === captured) uncertainDraft = null;
          saveState = 'error';
          saveMessage = 'Draft journal failed — keep this window open';
        }
        return false;
      }
    })();
    draftInFlight = operation;
    try {
      return await operation;
    } finally {
      if (draftInFlight === operation) draftInFlight = null;
      if (
        editVersion > draftSavedEditVersion &&
        saveState !== 'error' &&
        saveState !== 'uncertain'
      ) {
        scheduleDraftJournal();
      }
    }
  }

  async function flushDraftJournal(): Promise<boolean> {
    if (draftTimer !== undefined) {
      window.clearTimeout(draftTimer);
      draftTimer = undefined;
    }
    if (draftInFlight && !(await draftInFlight)) return false;
    if ((draftSavedEditVersion < editVersion || uncertainDraft) && !staleDraft) {
      return persistTransientDraft();
    }
    return true;
  }

  async function saveNow(): Promise<void> {
    if (!document || !desktop || editVersion === savedVersion) return;
    if (!(await flushDraftJournal())) return;
    if (saveInFlight) {
      saveQueued = true;
      await saveInFlight;
      return;
    }
    if (saveTimer !== undefined) {
      window.clearTimeout(saveTimer);
      saveTimer = undefined;
    }
    const captured = uncertainSave ?? captureSave();
    if (!captured) return;
    const operation = persistCapturedDocument(captured);
    saveInFlight = operation;
    try {
      await operation;
    } finally {
      if (saveInFlight === operation) saveInFlight = null;
      const needsFollowUp = saveQueued || (
        documentEpoch === captured.documentEpoch && editVersion > savedVersion
      );
      saveQueued = false;
      if (needsFollowUp && saveState !== 'error' && saveState !== 'uncertain') scheduleSave();
    }
  }

  function captureSave(): SaveCapture | null {
    if (!project || !document || !document.summary.revision_id) return null;
    return {
      commandId: newUlid(),
      documentEpoch,
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id,
      relativePath: document.summary.relative_path,
      kind: document.summary.kind,
      revisionId: document.summary.revision_id,
      visibleBlobId: document.visible_blob_id,
      draftVersion,
      text: documentText,
      editVersion
    };
  }

  async function persistCapturedDocument(captured: SaveCapture): Promise<void> {
    saveState = 'saving';
    saveMessage = 'Saving…';
    try {
      const receipt = await checkpointDocument(
        captured.projectId,
        captured.sessionId,
        captured.documentId,
        captured.relativePath,
        captured.text,
        captured.kind,
        captured.revisionId,
        captured.visibleBlobId,
        captured.commandId,
        captured.draftVersion
      );
      if (
        receipt.project_id !== captured.projectId ||
        receipt.command_id !== captured.commandId ||
        receipt.source_revision_id !== captured.revisionId ||
        !receipt.result_revision_id ||
        !receipt.result_blob_id ||
        receipt.command_kind !== 'checkpoint'
      ) {
        throw new Error('The desktop returned a checkpoint receipt that does not match this save.');
      }
      if (
        documentEpoch !== captured.documentEpoch ||
        project?.project_id !== captured.projectId ||
        project.session_id !== captured.sessionId ||
        document?.summary.document_id !== captured.documentId ||
        document.summary.relative_path !== captured.relativePath
      ) return;

      const projectionDecision = documentProjectionDecision(receipt.visible_projection);
      if (projectionDecision === 'missing') {
        throw new Error('The checkpoint receipt omitted its visible-file projection state.');
      }
      if (projectionDecision === 'retry') {
        uncertainSave = captured;
        saveState = 'uncertain';
        saveMessage = 'Checkpoint committed · visible file projection needs retry';
        const projectionError = receipt.visible_projection?.status === 'pending_retry'
          ? receipt.visible_projection.error
          : 'The visible file could not be replaced.';
        lastFailure = {
          code: 'visible_projection_pending',
          message: projectionError,
          retryable: true
        };
        errorMessage = projectionError;
        announce('The checkpoint is durable, but editing is locked until the same command projects its visible file');
        return;
      }
      if (projectionDecision === 'reconcile') {
        const heldAppText = documentText;
        uncertainSave = captured;
        saveState = 'uncertain';
        saveMessage = 'Checkpoint committed · external file held for review';
        try {
          await activateCheckpointProjectionConflict(captured, receipt, heldAppText);
        } catch (projectionError) {
          recordFailure(projectionError);
          // The semantic receipt is confirmed committed. Retain the exact
          // original checkpoint command even if refreshing the new active
          // identity or opening its reconciliation preview was refused.
          uncertainSave = captured;
          saveState = 'uncertain';
          saveMessage = 'Checkpoint committed · retry external-file review';
        }
        return;
      }

      if (uncertainSave?.commandId === captured.commandId) uncertainSave = null;
      clearFailure();
      if (draftVersion === captured.draftVersion) draftVersion = '0';
      uncertainDraft = null;
      savedVersion = Math.max(savedVersion, captured.editVersion);
      const nextSummary: DocumentSummary = {
        ...document.summary,
        revision_id: receipt.result_revision_id,
        active_blob_id: receipt.result_blob_id,
        word_count: countWords(captured.text),
        externally_modified: false
      };
      document = {
        ...document,
        visible_blob_id: receipt.result_blob_id,
        text: documentText,
        summary: nextSummary
      };
      if (project) {
        project = {
          ...project,
          documents: project.documents.map((candidate) =>
            candidate.document_id === captured.documentId ? nextSummary : candidate
          )
        };
      }
      if (editVersion === savedVersion) {
        saveState = 'saved';
        saveMessage = 'All changes saved';
        window.setTimeout(() => {
          if (saveState === 'saved' && editVersion === savedVersion) saveState = 'clean';
        }, 1200);
      } else {
        saveState = 'dirty';
        saveMessage = 'Unsaved changes';
      }
    } catch (error) {
      if (documentEpoch !== captured.documentEpoch) return;
      const failure = recordFailure(error);
      if (
        failure.code === 'external_file_change' ||
        failure.code === 'source_blob_conflict' ||
        failure.code === 'visible_file_conflict'
      ) {
        uncertainSave = null;
        try {
          const preview = await requestReconciliationPreview({
            document_id: captured.documentId,
            relative_path: captured.relativePath,
            kind: captured.kind,
            revision_id: captured.revisionId,
            active_blob_id: captured.visibleBlobId
          }, captured.text);
          activateReconciliation(preview);
        } catch (previewError) {
          recordFailure(previewError);
          saveState = 'error';
          saveMessage = 'External change needs reconciliation';
        }
        return;
      }
      const retryCapture = captureForIdempotentRetry(captured, failure);
      if (retryCapture) {
        uncertainSave = retryCapture;
        saveState = 'uncertain';
        saveMessage = 'Save result uncertain — retry safely';
        announce('Save result uncertain; retrying will use the identical source-bound command');
      } else {
        if (uncertainSave?.commandId === captured.commandId) uncertainSave = null;
        saveState = 'error';
        saveMessage = 'Save refused — correct the error before retrying';
      }
    }
  }

  async function flushCurrentDocument(): Promise<boolean> {
    if (!desktop || !document) return true;
    if (saveTimer !== undefined) {
      window.clearTimeout(saveTimer);
      saveTimer = undefined;
    }
    for (let attempt = 0; attempt < 8; attempt += 1) {
      if (savedVersion === editVersion && !saveInFlight) return true;
      await saveNow();
      if (saveState === 'error' || saveState === 'uncertain') return false;
    }
    saveState = 'error';
    saveMessage = 'Save did not settle';
    recordLocalFailure('save_did_not_settle', 'Loom could not finish saving this document; it remains open and unchanged.');
    return false;
  }

  async function toggleFocusMode(): Promise<void> {
    if (!project) return;
    focusMode = !focusMode;
    if (desktop) {
      try {
        await setFocusMode(project.project_id, project.session_id, focusMode);
      } catch (error) {
        focusMode = !focusMode;
        recordFailure(error);
      }
    }
    announce(focusMode ? 'Focus mode on' : 'Focus mode off');
  }

  function handleGlobalKeydown(event: KeyboardEvent): void {
    const modifier = event.metaKey || event.ctrlKey;
    if (modifier && event.key.toLocaleLowerCase() === 's') {
      event.preventDefault();
      if (reconciliation) {
        announce(pendingReconciliationApply
          ? 'Retry the exact reconciliation command with the review button'
          : 'Review and checkpoint the external-file resolution');
        return;
      }
      if (compositionActive) {
        announce('Finish composing text before checkpointing');
        return;
      }
      flushEditors();
      void saveNow();
    }
    if (modifier && event.shiftKey && event.key.toLocaleLowerCase() === 'l') {
      event.preventDefault();
      void toggleFocusMode();
    }
  }

  async function checkpointNow(): Promise<void> {
    if (compositionActive) {
      announce('Finish composing text before checkpointing');
      return;
    }
    if (!flushEditors()) return;
    await saveNow();
    if (saveState !== 'error' && saveState !== 'uncertain') announce('Checkpoint saved');
  }

  async function restoreStaleDraft(): Promise<void> {
    if (
      staleDraftRestoring ||
      uncertainDraft ||
      !staleDraft ||
      !document ||
      !project ||
      !document.summary.revision_id
    ) return;
    const recovered = staleDraft;
    const activeText = document.text;
    const previousEditVersion = editVersion;
    const previousDraftSavedEditVersion = draftSavedEditVersion;
    staleDraftRestoring = true;
    staleDraftDiscardArmed = false;

    // Keep the old draft version as the optimistic predecessor, but bind the
    // replacement write to the current active revision. The store advances the
    // version in one operation, so there is never a clear-then-write loss gap.
    draftVersion = recovered.version;
    documentText = recovered.text;
    setSourceDocument(recovered.text, document.summary.kind);
    editVersion += 1;
    draftSavedEditVersion = Math.min(draftSavedEditVersion, editVersion - 1);
    saveState = 'dirty';
    saveMessage = 'Rebinding recovered draft to the active revision…';
    clearFailure();
    try {
      const rebound = await persistTransientDraft();
      if (rebound) {
        announce('Recovered draft protected against the active revision; checkpoint pending');
        return;
      }
      if (uncertainDraft) {
        announce('Recovered draft write is uncertain; retrying will use the identical bytes and source revision');
        return;
      }

      // A deterministic refusal did not write anything. Restore the active
      // editor projection and leave the original stale draft inspectable.
      if (staleDraft === recovered && document) {
        documentText = activeText;
        setSourceDocument(activeText, document.summary.kind);
        editVersion = previousEditVersion;
        draftSavedEditVersion = previousDraftSavedEditVersion;
        draftVersion = recovered.version;
      }
    } finally {
      staleDraftRestoring = false;
    }
  }

  function armStaleDraftDiscard(): void {
    if (!staleDraft) return;
    staleDraftDiscardArmed = true;
    announce('Confirm permanent discard of the recovered unsaved text');
  }

  function cancelStaleDraftDiscard(): void {
    staleDraftDiscardArmed = false;
    announce('Recovered draft remains protected');
  }

  async function discardStaleDraft(): Promise<void> {
    if (!staleDraftDiscardArmed || !staleDraft || !document || !project) return;
    const discarded = staleDraft;
    try {
      const cleared = await clearTransientDraft(
        project.project_id,
        project.session_id,
        document.summary.document_id,
        document.summary.relative_path,
        discarded.version
      );
      if (!cleared) throw new Error('The desktop did not confirm that the recovered draft was cleared.');
      staleDraft = null;
      staleDraftDiscardArmed = false;
      draftVersion = '0';
      draftSavedEditVersion = editVersion;
      saveState = 'clean';
      saveMessage = 'All changes saved';
      clearFailure();
      announce('Recovered unsaved text permanently discarded; active manuscript kept');
    } catch (error) {
      recordFailure(error);
    }
  }

  async function applyReconciliationResolution(): Promise<void> {
    if (
      !project ||
      !reconciliation ||
      reconciliationApplying ||
      !reconciliationResolutionIsExact
    ) return;
    const captured = pendingReconciliationApply ?? {
      commandId: newUlid(),
      projectId: project.project_id,
      sessionId: project.session_id,
      preview: reconciliation,
      resolvedText: reconciliationResolution,
      reason: reconciliation.outcome.status === 'conflict'
        ? 'author resolved external-file conflict'
        : 'author accepted external-file reconciliation'
    };
    pendingReconciliationApply = captured;
    const { commandId, preview, projectId, reason, resolvedText, sessionId } = captured;
    if (project.project_id !== projectId || project.session_id !== sessionId) {
      pendingReconciliationApply = null;
      recordLocalFailure('stale_project_session', 'The reconciliation command belongs to another project session.');
      return;
    }
    reconciliationApplying = true;
    clearFailure();
    try {
      const receipt = await applyDocumentReconciliation(
        projectId,
        sessionId,
        preview.document_id,
        preview.relative_path,
        preview.active_revision_id,
        preview.base_blob_id,
        preview.external_visible_blob_id,
        resolvedText,
        preview.kind,
        reason,
        commandId
      );
      if (
        receipt.command_id !== commandId ||
        receipt.command_kind !== 'reconcile_external' ||
        receipt.project_id !== projectId ||
        receipt.source_revision_id !== preview.active_revision_id ||
        !receipt.result_revision_id ||
        !receipt.result_blob_id
      ) {
        throw new Error('The desktop returned a reconciliation receipt that does not match this resolution.');
      }

      const projectionDecision = documentProjectionDecision(receipt.visible_projection);
      if (projectionDecision === 'missing') {
        throw new Error('The reconciliation receipt omitted its visible-file projection state.');
      }
      if (projectionDecision === 'retry') {
        saveState = 'uncertain';
        saveMessage = 'Resolution committed · visible file projection needs retry';
        const projectionError = receipt.visible_projection?.status === 'pending_retry'
          ? receipt.visible_projection.error
          : 'The visible file could not be replaced.';
        lastFailure = {
          code: 'visible_projection_pending',
          message: projectionError,
          retryable: true
        };
        errorMessage = projectionError;
        announce('The resolution is durable and locked until the identical command projects its visible file');
        return;
      }
      if (projectionDecision === 'reconcile') {
        if (preview.draft_version) {
          try {
            await clearTransientDraft(
              projectId,
              sessionId,
              preview.document_id,
              preview.relative_path,
              preview.draft_version
            );
          } catch {
            // The incorporated draft remains recoverable if its exact clear
            // cannot be confirmed after the semantic reconciliation commit.
          }
        }
        try {
          await activateReconciliationProjectionConflict(captured, receipt);
        } catch (projectionError) {
          recordFailure(projectionError);
          pendingReconciliationApply = captured;
          saveState = 'uncertain';
          saveMessage = 'Resolution committed · retry external-file review';
        }
        return;
      }

      if (preview.draft_version) {
        try {
          await clearTransientDraft(
            projectId,
            sessionId,
            preview.document_id,
            preview.relative_path,
            preview.draft_version
          );
        } catch {
          // The semantic merge is already durable. An uncleared draft remains
          // recoverable and will be shown explicitly on the next document open.
        }
      }

      const refreshed = await currentProjectSession();
      const target = refreshed.documents.find(
        (candidate) => candidate.document_id === preview.document_id
      );
      if (!target) throw new Error('The reconciled document disappeared from the project outline.');
      project = refreshed;
      documentEpoch += 1;
      document = null;
      documentText = '';
      sourceDisplayText = '';
      verseCodec = null;
      editVersion = 0;
      savedVersion = 0;
      draftVersion = '0';
      draftSavedEditVersion = 0;
      staleDraft = null;
      uncertainDraft = null;
      uncertainSave = null;
      clearReconciliationState();
      saveState = 'clean';
      saveMessage = 'Reconciliation saved';
      await tick();
      await selectDocument(target);
      announce('External change reconciled and preserved in history');
    } catch (error) {
      const failure = recordFailure(error);
      const retryCapture = captureForIdempotentRetry(captured, failure);
      if (retryCapture) {
        pendingReconciliationApply = retryCapture;
        saveMessage = 'Reconciliation result uncertain — retry safely';
        announce('Reconciliation result uncertain; the resolution is locked for an identical retry');
      } else {
        pendingReconciliationApply = null;
        saveMessage = failure.code === 'visible_file_conflict' || failure.code === 'external_file_conflict'
          ? 'The external file changed again — refresh the comparison'
          : 'Reconciliation refused — review the bound inputs';
      }
    } finally {
      reconciliationApplying = false;
    }
  }

  async function refreshReconciliationComparison(): Promise<void> {
    if (!project || !reconciliation || reconciliationApplying || pendingReconciliationApply) return;
    const previous = reconciliation;
    const appText = reconciliationResolution;
    reconciliationApplying = true;
    clearFailure();
    try {
      const refreshed = await currentProjectSession();
      const target = refreshed.documents.find(
        (candidate) => candidate.document_id === previous.document_id
      );
      if (!target) throw new Error('The document is no longer registered in this project.');
      project = refreshed;
      if (!target.externally_modified) {
        clearReconciliationState();
        document = null;
        await tick();
        await selectDocument(target);
        return;
      }
      const preview = await requestReconciliationPreview(target, appText);
      activateReconciliation(preview);
      announce('External comparison refreshed against the newest visible file');
    } catch (error) {
      recordFailure(error);
    } finally {
      reconciliationApplying = false;
    }
  }

  async function setMode(next: EditorMode): Promise<void> {
    if (compositionActive) {
      announce('Finish composing text before changing editor modes');
      return;
    }
    if (next === 'split') {
      announce('Split mode is disabled until cross-view edits preserve history exactly');
      return;
    }
    if (next === 'visual' && !canUseVisual) return;
    flushEditors();
    if (next === 'source' && document) setSourceDocument(documentText, document.summary.kind);
    mode = next;
    announce(`${next} editor mode`);
  }

  function announce(message: string): void {
    liveRegion = '';
    window.setTimeout(() => (liveRegion = message), 0);
  }

  async function closeProject(): Promise<boolean> {
    if (!project) return true;
    const retryingUncertainClose = transition === 'closing' && pendingCloseCommandId !== null;
    if (compositionActive && !retryingUncertainClose) {
      announce('Finish composing text before closing the project');
      return false;
    }
    if (!retryingUncertainClose) {
      if (!flushEditors()) return false;
      transition = 'closing';
      announce('Closing project; editing is briefly locked');
      if (!(await flushCurrentDocument())) {
        transition = 'idle';
        return false;
      }
    }
    const closing = project;
    const closingEpoch = documentEpoch;
    const closingVersion = editVersion;
    if (documentEpoch !== closingEpoch || editVersion !== closingVersion) {
      transition = 'idle';
      recordLocalFailure('close_race', 'The manuscript changed while Loom prepared to close it.');
      return false;
    }
    try {
      pendingCloseCommandId ??= newUlid();
      const receipt = await closeProjectSession(
        closing.project_id,
        closing.session_id,
        pendingCloseCommandId
      );
      if (
        receipt.command_id !== pendingCloseCommandId ||
        receipt.project_id !== closing.project_id ||
        receipt.session_id !== closing.session_id
      ) {
        throw new Error('The desktop returned a close receipt for a different project session.');
      }
    } catch (error) {
      const failure = recordFailure(error);
      if (closeResultMayHaveCommitted(failure)) {
        transition = 'closing';
        saveMessage = 'Close result uncertain — retry safely';
        announce('Close result uncertain; editing remains locked until the same close command is retried');
      } else {
        transition = 'idle';
      }
      return false;
    }
    documentEpoch += 1;
    project = null;
    document = null;
    documentText = '';
    sourceDisplayText = '';
    verseCodec = null;
    editVersion = 0;
    savedVersion = 0;
    branches = [];
    saveState = 'clean';
    saveMessage = 'No project open';
    focusMode = false;
    pendingCloseCommandId = null;
    uncertainSave = null;
    draftVersion = '0';
    draftSavedEditVersion = 0;
    staleDraft = null;
    uncertainDraft = null;
    clearReconciliationState();
    if (draftTimer !== undefined) {
      window.clearTimeout(draftTimer);
      draftTimer = undefined;
    }
    transition = 'idle';
    return true;
  }

  function kindLabel(kind: DocumentKind): string {
    if (kind === 'verse') return 'Poem';
    if (kind === 'hybrid') return 'Hybrid';
    return 'Prose';
  }
</script>

<svelte:head>
  <meta name="description" content="Loom — a local-first writing environment for prose and poetry" />
</svelte:head>

<div class:focus-mode={focusMode} class="app-shell">
  <a class="skip-link" href="#manuscript">Skip to manuscript</a>

  <header class="topbar">
    <div class="brand" aria-label="Loom">
      <span class="brand-mark" aria-hidden="true">∿</span>
      <span>Loom</span>
    </div>
    {#if project}
      <button class="project-switcher" type="button" on:click={() => void closeProject()} disabled={reconciliationResolutionLocked || (editorReadonly && transition !== 'closing' && !(reconciliation && !document))} title={transition === 'closing' ? 'Retry the same close command safely' : 'Close project'}>
        <span>{transition === 'closing' ? 'Retry close' : project.title}</span>
        <span class="muted">⌄</span>
      </button>
    {:else}
      <span class="topbar-caption">local-first writing</span>
    {/if}
    <div class="topbar-spacer"></div>
    {#if document}
      <div class="save-status state-{saveState}" role="status" aria-live="polite">
        <span class="status-dot"></span>{saveMessage}
      </div>
      <div class="mode-switch" aria-label="Editor view">
        <button class:active={mode === 'visual'} disabled={!canUseVisual || editorReadonly} type="button" on:click={() => void setMode('visual')} title={canUseVisual ? 'Visual editor' : 'Visual editing requires lossless CommonMark round-trip'}>Visual</button>
        <button class:active={mode === 'source'} disabled={editorReadonly} type="button" on:click={() => void setMode('source')}>Source</button>
        <button class:active={mode === 'split'} disabled type="button" title="Split editing is disabled until cross-view history is lossless">Split</button>
      </div>
    {/if}
    <button class:active={focusMode} class="icon-button" type="button" on:click={toggleFocusMode} aria-pressed={focusMode} disabled={!project || editorReadonly} title="Focus mode (⌘⇧L)">
      <span aria-hidden="true">◉</span><span class="button-label">Focus</span>
    </button>
  </header>

  {#if project}
    <div class="workspace-grid">
      <aside class="outline-panel" aria-label="Project outline">
        <div class="panel-heading">
          <span>Manuscript</span>
          <button class="bare-button" type="button" title="New document" aria-label="New document" disabled>＋</button>
        </div>
        <label class="search-field">
          <span class="sr-only">Search project</span>
          <span aria-hidden="true">⌕</span>
          <input bind:value={search} type="search" placeholder="Find in project" />
        </label>
        <nav class="document-list" aria-label="Documents">
          {#each visibleDocuments as candidate (candidate.document_id)}
            <button
              class:active={candidate.document_id === (reconciliation?.document_id ?? document?.summary.document_id)}
              type="button"
              disabled={editorReadonly}
              on:click={() => selectDocument(candidate)}
            >
              <span class="document-glyph" aria-hidden="true">{candidate.kind === 'verse' ? '≋' : '¶'}</span>
              <span class="document-label">
                <strong>{candidate.title}</strong>
                <small>{kindLabel(candidate.kind)} · {candidate.word_count.toLocaleString()} words</small>
              </span>
            </button>
          {:else}
            <p class="empty-copy">No documents yet. The CLI can import or checkpoint the first UTF-8 manuscript.</p>
          {/each}
        </nav>
        <div class="outline-footer">
          <span title={project.root}>Offline project</span>
          <span>{project.documents.length} {project.documents.length === 1 ? 'piece' : 'pieces'}</span>
        </div>
      </aside>

      <main id="manuscript" class="manuscript-area" tabindex="-1">
        {#if reconciliation}
          <section class="reconciliation-workspace" aria-labelledby="reconciliation-title">
            <header class="document-header">
              <span class="eyebrow">External change · {kindLabel(reconciliation.kind)}</span>
              <h1 id="reconciliation-title">Review before anything changes</h1>
              <div class="document-meta">
                <span>{reconciliation.relative_path}</span>
                <span>{reconciliation.app_source === 'base' ? 'No Loom draft' : reconciliation.app_source === 'caller' ? 'Current Loom draft' : 'Recovered Loom draft'}</span>
              </div>
            </header>

            {#if pendingReconciliationApply}
              <div class="runtime-note" role="status">
                {saveMessage}. The approved resolution is already durable or may be durable, so its exact bytes and command identity are locked until retry confirms the visible-file projection.
              </div>
            {:else}
              <div class="runtime-note" role="status">
                The visible file changed outside Loom and has not been overwritten. This comparison is bound to revision {reconciliation.active_revision_id} and the exact external file hash.
              </div>
            {/if}

            <div class="reconciliation-columns" aria-label="Three-way manuscript comparison">
              <details open>
                <summary>Immutable base</summary>
                <pre>{reconciliation.base_text}</pre>
              </details>
              <details open>
                <summary>Loom side</summary>
                <pre>{reconciliation.app_text}</pre>
              </details>
              <details open>
                <summary>External file</summary>
                <pre>{reconciliation.external_text}</pre>
              </details>
            </div>

            {#if reconciliation.outcome.status === 'conflict'}
              <section class="conflict-list" aria-labelledby="conflict-title">
                <h2 id="conflict-title">{reconciliation.outcome.conflicts.length} incompatible {reconciliation.outcome.conflicts.length === 1 ? 'change' : 'changes'}</h2>
                {#each reconciliation.outcome.conflicts as conflict, index}
                  <article>
                    <strong>{conflict.kind === 'competing_insertions' ? 'Competing insertions' : 'Overlapping edits'} · conflict {index + 1}</strong>
                    <dl>
                      <div><dt>Base</dt><dd><code>{conflict.base.text || '∅'}</code></dd></div>
                      <div><dt>Loom</dt><dd><code>{conflict.app.text || '∅'}</code></dd></div>
                      <div><dt>External</dt><dd><code>{conflict.external.text || '∅'}</code></dd></div>
                    </dl>
                  </article>
                {/each}
              </section>
            {:else}
              <div class="runtime-note success" role="status">The changes do not overlap. Loom prepared a deterministic merge for your approval.</div>
            {/if}

            <section class="resolution-panel" aria-labelledby="resolution-title">
              <div class="panel-heading"><span id="resolution-title">Resolution to checkpoint</span></div>
              {#if reconciliation.kind === 'prose'}
                <textarea bind:value={reconciliationResolution} readonly={reconciliationResolutionLocked} aria-label="Resolved Markdown" spellcheck="true"></textarea>
              {:else}
                <pre class="exact-resolution">{reconciliationResolution}</pre>
                <p class="muted">Verse resolution remains byte-exact. Choose a preserved side or the automatic non-overlapping merge; manual textarea normalization is disabled.</p>
              {/if}
              <div class="resolution-choices">
                <button class="secondary-button" type="button" on:click={() => (reconciliationResolution = reconciliation?.app_text ?? '')} disabled={reconciliationResolutionLocked}>Use Loom side</button>
                <button class="secondary-button" type="button" on:click={() => (reconciliationResolution = reconciliation?.external_text ?? '')} disabled={reconciliationResolutionLocked}>Use external side</button>
                {#if reconciliation.outcome.status === 'merged'}
                  <button class="secondary-button" type="button" on:click={() => (reconciliationResolution = reconciliation?.outcome.status === 'merged' ? reconciliation.outcome.content : '')} disabled={reconciliationResolutionLocked}>Use safe merge</button>
                {/if}
              </div>
            </section>

            <footer class="reconciliation-actions">
              <button class="secondary-button" type="button" on:click={refreshReconciliationComparison} disabled={reconciliationResolutionLocked}>Refresh comparison</button>
              <button class="primary-button" type="button" on:click={applyReconciliationResolution} disabled={reconciliationApplying || !reconciliationResolutionIsExact}>
                {reconciliationApplying ? 'Checking exact identities…' : pendingReconciliationApply ? 'Retry exact reconciliation' : 'Checkpoint this resolution'}
              </button>
            </footer>
          </section>
        {:else if document}
          <header class="document-header">
            <span class="eyebrow">{kindLabel(document.summary.kind)}</span>
            <h1>{document.summary.title}</h1>
            <div class="document-meta">
              <span>{wordCount.toLocaleString()} words</span>
              <span>{characterCount.toLocaleString()} characters</span>
              <span>{document.summary.relative_path}</span>
            </div>
          </header>

          {#if staleDraft}
            <div class="runtime-note" role="alert">
              A crash-safe draft from revision {staleDraft.source_revision_id} is preserved. Editing is locked because the active revision differs; Loom will not overwrite either version.
              <details>
                <summary>Inspect recovered draft</summary>
                <pre>{staleDraft.text}</pre>
              </details>
              <p class="muted">Keeping the active manuscript permanently removes this recovered unsaved text from Loom's crash-safe draft storage.</p>
              <div class="project-actions">
                <button class="primary-button" type="button" on:click={() => void restoreStaleDraft()} disabled={staleDraftRestoring || uncertainDraft !== null}>{staleDraftRestoring ? 'Protecting recovered draft…' : 'Use recovered draft'}</button>
                {#if staleDraftDiscardArmed}
                  <button class="secondary-button" type="button" on:click={() => void discardStaleDraft()} disabled={staleDraftRestoring || uncertainDraft !== null}>Confirm permanent discard</button>
                  <button class="bare-button" type="button" on:click={cancelStaleDraftDiscard} disabled={staleDraftRestoring}>Cancel</button>
                {:else}
                  <button class="secondary-button" type="button" on:click={armStaleDraftDiscard} disabled={staleDraftRestoring || uncertainDraft !== null}>Discard recovered draft…</button>
                {/if}
              </div>
            </div>
          {/if}

          <section class:split={mode === 'split'} class="editor-stage" aria-label="Writing surface">
            {#if showVisual}
              <div class="editor-pane visual-pane" aria-label="Visual editor pane">
                {#if exactTextSurface}
                  <div class="verse-notice">Verse stays in the exact-whitespace source surface.</div>
                {:else}
                  {#if canUseVisual}
                    <LoomEditor
                      bind:this={visualEditor}
                      value={documentText}
                      onChange={updateText}
                      onCompositionChange={setVisualComposition}
                      readonly={editorReadonly}
                      autofocus={true}
                    />
                  {:else}
                    <div class="verse-notice">Visual editing is read-only for content that cannot round-trip exactly through the current CommonMark schema. Use Source mode; GFM and hybrid block support are not silently approximated.</div>
                  {/if}
                {/if}
              </div>
            {/if}
            {#if showSource}
              <div class="editor-pane source-pane" aria-label="Source editor pane">
                {#if exactTextSurface && verseCodec && !verseCodec.editable}
                  <div class="verse-notice" role="alert">This poem uses mixed line-ending encodings. Loom will not normalize them silently; source editing stays locked until a lossless boundary editor is available.</div>
                {/if}
                {#if document.summary.kind === 'hybrid'}
                  <div class="verse-notice" role="alert">Hybrid source editing is locked until its prose/verse block manifest can cross the IPC boundary losslessly.</div>
                {/if}
                <textarea
                  class:verse={exactTextSurface}
                  value={sourceDisplayText}
                  readonly={editorReadonly || document.summary.kind === 'hybrid' || Boolean(exactTextSurface && verseCodec && !verseCodec.editable)}
                  on:compositionstart={beginSourceComposition}
                  on:compositionend={finishSourceComposition}
                  on:input={(event) => updateFromSource(event.currentTarget.value)}
                  aria-label={exactTextSurface ? 'Exact-whitespace verse editor' : 'Markdown source editor'}
                  spellcheck="true"
                  wrap={exactTextSurface ? 'off' : 'soft'}
                ></textarea>
              </div>
            {/if}
          </section>

          {#if branches.length > 0}
            <aside class="branch-shelf" aria-label="Private strands">
              <div class="panel-heading"><span>Strands ready</span><span class="count-badge">{branches.length}</span></div>
              {#each branches as branch (branch.branch_id)}
                <article class="branch-card">
                  <p>{branch.text}</p>
                  <footer><span>{branch.model_id}</span><span>seed {branch.seed}</span></footer>
                </article>
              {/each}
            </aside>
          {/if}

          <footer class="authoring-footer">
            <div class="model-state">
              <span class:ready={currentModel} class="status-dot"></span>
              {#if currentModel}
                <span>{currentModel.display_name} · local</span>
              {:else}
                <span>No writer model loaded — {models.length > 0 ? `${models.length} local GGUF ${models.length === 1 ? 'file' : 'files'} discovered` : 'editing remains fully available'}</span>
              {/if}
            </div>
            <div class="authoring-actions">
              <button class="secondary-button" type="button" on:click={checkpointNow} disabled={!document || compositionActive || transition !== 'idle' || (editorReadonly && uncertainSave === null)}>
                {uncertainSave ? 'Retry save safely' : 'Checkpoint'}
              </button>
              <button class="weave-button" type="button" disabled={!canWeave} title={canWeave ? 'Generate private strands' : focusMode ? 'Focus mode blocks generation' : 'Load a local raw-completion model to weave'}>
                <span aria-hidden="true">∿</span> Weave
              </button>
            </div>
          </footer>
        {:else}
          <section class="empty-project">
            <span class="empty-mark" aria-hidden="true">∿</span>
            <h1>Your project is ready.</h1>
            <p>Import or checkpoint a Markdown manuscript with the CLI; document creation is the next UI command to land.</p>
          </section>
        {/if}
      </main>
    </div>
  {:else}
    <main class="welcome" id="manuscript">
      <section class="welcome-copy">
        <div class="large-mark" aria-hidden="true">∿</div>
        <span class="eyebrow">Write beside the possible</span>
        <h1>A quiet place for prose, poetry, and private branches.</h1>
        <p>Loom keeps your manuscript as ordinary files. Local models may suggest strands, but only you can promote one into the work.</p>
        <ul>
          <li><span aria-hidden="true">✓</span> Readable Markdown and exact-whitespace verse</li>
          <li><span aria-hidden="true">✓</span> Offline by default, with no silent cloud route</li>
          <li><span aria-hidden="true">✓</span> Immutable revision and generation provenance</li>
        </ul>
      </section>
      <section class="project-card" aria-labelledby="project-card-title">
        <span class="eyebrow">Open folder</span>
        <h2 id="project-card-title">Begin with a project you own</h2>
        {#if !desktop}
          <div class="runtime-note" role="note">Browser preview: project commands are available in the Tauri desktop build.</div>
        {/if}
        <label>
          <span>Title for a new project</span>
          <input bind:value={projectTitle} placeholder="Untitled Loom" autocomplete="off" />
        </label>
        {#if errorMessage}
          <div class="error-banner" role="alert">
            {errorMessage}{#if lastFailure}<small> · {lastFailure.code}{lastFailure.retryable ? ' · retryable' : ''}</small>{/if}
          </div>
        {/if}
        <div class="project-actions">
          <button class="secondary-button" type="button" on:click={doOpenProject} disabled={!desktop || opening || creating}>
            {opening ? 'Choosing…' : 'Choose existing folder'}
          </button>
          <button class="primary-button" type="button" on:click={doCreateProject} disabled={!desktop || !projectTitle.trim() || opening || creating}>
            {creating ? 'Choosing…' : 'Choose folder & create'}
          </button>
        </div>
        <small>Nothing is uploaded. Hosted providers remain off until explicitly configured.</small>
      </section>
    </main>
  {/if}

  {#if errorMessage && project}
    <div class="toast error" role="alert">
      <span>{errorMessage}{#if lastFailure}<small> · {lastFailure.code}{lastFailure.retryable ? ' · retryable' : ''}</small>{/if}</span>
      {#if transition === 'closing' && pendingCloseCommandId}
        <button type="button" on:click={() => void closeProject()} aria-label="Retry close safely">Retry close</button>
      {:else if uncertainDraft}
        <button type="button" on:click={() => void persistTransientDraft()} aria-label="Retry draft safely">Retry draft</button>
      {:else if uncertainSave}
        <button type="button" on:click={() => void saveNow()} aria-label="Retry checkpoint safely">Retry checkpoint</button>
      {:else}
        <button type="button" on:click={clearFailure} aria-label="Dismiss error">×</button>
      {/if}
    </div>
  {/if}
  <div class="sr-only" aria-live="polite">{liveRegion}</div>
</div>
