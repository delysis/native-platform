<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import LoomEditor from './lib/LoomEditor.svelte';
  import SourceEditor from './lib/SourceEditor.svelte';
  import {
    abortApplicationClose,
    applicationClosePending,
    cancelGeneration,
    cancelModelDownload,
    checkpointDocument,
    clearTransientDraft,
    applyDocumentReconciliation,
    chooseAndOpenProject,
    chooseModel,
    closeProject as closeProjectSession,
    currentProjectSession,
    getBranch,
    getBranchBody,
    getBranchPage,
    getBuildModelPolicy,
    getModelDownloadStatus,
    getWeaveStatus,
    isDesktopRuntime,
    listenForApplicationCloseRequests,
    listenForGenerationEvents,
    listenForModelDownloadEvents,
    loadPolicyModelCandidate,
    listModels,
    listModelDownloads,
    openDefaultProject,
    openDocument,
    previewDocumentReconciliation,
    promoteCandidate,
    recoverProject,
    requestApplicationClose,
    setFocusMode,
    setSuggestions as setSuggestionsPolicy,
    startWeave,
    startModelDownload,
    unloadModel,
    normalizeFailure,
    upsertTransientDraft
  } from './lib/ipc';
  import {
    decodeVerseForEditor,
    encodeVerseFromEditor,
    type VerseEditorCodec,
    type VerseNewlineKind
  } from './lib/verseCodec';
  import { canRoundTripMarkdownExactly, canUseVisualMarkdown } from './lib/markdownSafety';
  import {
    autocompleteDisposition,
    ghostReviewAffordance,
    verifiedGhostSuggestion,
    visibleVerifiedGhostSuggestion,
    type AutocompleteDisposition
  } from './lib/ghostSuggestion';
  import {
    emptyAutocompleteRetryLedger,
    planAutocompleteRetry,
    type AutocompleteRetryLedger
  } from './lib/autocompleteRetry';
  import {
    candidateSurfaceReason,
    candidateTextIsSurfaceable
  } from './lib/candidateSurface';
  import { sourceGhostPresentationCompatible } from './lib/sourceGhostText';
  import { visualGhostTextMayBePlainProse } from './lib/ghostText';
  import { isExtendedGraphemeBoundary } from './lib/graphemeBoundary';
  import { branchIsActionableOnShelf } from './lib/branchShelf';
  import {
    verifyBranchBody,
    verifiedBodyMatchesBranch,
    type VerifiedBranchBody
  } from './lib/branchBodyProof';
  import {
    appendUniquePage,
    branchBodyDisposition,
    mergeNewestPage
  } from './lib/branchPaging';
  import { writeRebindsStaleDraft } from './lib/draftRecovery';
  import { documentProjectionDecision } from './lib/projectionState';
  import {
    navigationScopeIsCurrent,
    projectRestoreScopeIsCurrent,
    projectSessionIsCurrent,
    type ProjectRestoreScope
  } from './lib/projectScope';
  import {
    captureForIdempotentRetry,
    closeResultMayHaveCommitted
  } from './lib/sessionSafety';
  import { drainGenerationsAndClose } from './lib/sessionCloseCoordinator';
  import {
    ApplicationCloseCoordinator,
    applicationAllowsModelPreparation,
    applicationStartupDisposition,
    isApplicationCloseAbortFailure,
    type ApplicationCloseOutcome,
    type ApplicationClosePhase,
    type ProjectCloseOutcome
  } from './lib/applicationCloseCoordinator';
  import { ApplicationCloseRetryScheduler } from './lib/applicationCloseRetry';
  import { suggestionsEnabledFromStoredPreference } from './lib/suggestionPreference';
  import {
    captureProjectCloseAgency,
    restoreProjectCloseAgency,
    type ProjectCloseAgencySnapshot
  } from './lib/projectCloseAgency';
  import {
    restoreBeforeBackgroundWork,
    runCurrentWorkspaceStep,
    shouldDiscoverModelsOnStartup
  } from './lib/startupSafety';
  import { newUlid } from './lib/ulid';
  import {
    DEFAULT_MODEL_DOWNLOAD_LIMIT_GIB,
    deriveGgufFileName,
    downloadProgressPercent,
    formatByteCount,
    validateVerifiedDownload,
    type VerifiedDownloadForm
  } from './lib/modelDownload';
  import {
    generationEventBelongsToScope,
    utf8ByteOffset
  } from './lib/weaveSafety';
  import {
    automaticWriterForBuildPolicy,
    isVerifiedPolicyWriter,
    orderedLocalWriterCandidates,
    preferredWriterModelPath,
    writerProfileForBuildPolicy
  } from './lib/modelPolicy';
  import type {
    BranchCard,
    BranchPageCursor,
    BranchSummary,
    BuildModelPolicySummary,
    CommandReceipt,
    DesktopGenerationEnvelope,
    DocumentKind,
    DocumentSummary,
    EditorMode,
    ModelCapabilitySummary,
    ModelDownloadPhase,
    ModelDownloadSnapshot,
    LoomFailure,
    OpenDocument,
    ProjectSnapshot,
    ReconciliationPreview,
    SaveState,
    TransientDraftSnapshot,
    WeaveStarted
  } from './lib/types';

  let desktop = false;
  let buildModelPolicy: BuildModelPolicySummary | null = null;
  let project: ProjectSnapshot | null = null;
  let document: OpenDocument | null = null;
  let documentText = '';
  let mode: EditorMode = 'visual';
  let preferredProseMode: EditorMode = 'visual';
  let saveState: SaveState = 'clean';
  let saveMessage = 'No project open';
  let errorMessage = '';
  let lastFailure: LoomFailure | null = null;
  let opening = false;
  let search = '';
  let outlineOpen = false;
  let outlineToggle: HTMLButtonElement | undefined;
  let outlinePanel: HTMLElement | undefined;
  let outlineSearch: HTMLInputElement | undefined;
  let models: ModelCapabilitySummary[] = [];
  let selectedModelPath = '';
  let compatibleWriterModels: ModelCapabilitySummary[] = [];
  let otherLocalModels: ModelCapabilitySummary[] = [];
  let modelSetupError = '';
  let modelLoading = false;
  let modelUnloading = false;
  let modelChoosing = false;
  let modelManagerOpen = false;
  let modelManagerPanel: HTMLElement | undefined;
  let modelManagerReturnFocus: HTMLElement | null = null;
  let strandReviewDialog: HTMLDialogElement | undefined;
  let strandReviewTrigger: HTMLButtonElement | undefined;
  let strandReviewOpen = false;
  let reviewCandidateId: string | null = null;
  let projectMenu: HTMLDetailsElement | undefined;
  let projectMenuTrigger: HTMLElement | undefined;
  let suggestionsEnabled = false;
  let suggestionsChanging = false;
  let suggestionsIdleTimer: number | undefined;
  let scheduledSuggestion: SuggestionSchedule | null = null;
  let suggestionIntentEpoch = 0;
  let autocompleteRetryLedger: AutocompleteRetryLedger = emptyAutocompleteRetryLedger();
  let dismissedCandidateIds: string[] = [];
  let unpresentableVisualGhostPresentationKeys: string[] = [];
  let announcedGhostPresentationKey = '';
  let modelDownloadUrl = '';
  let modelDownloadFileName = '';
  let lastDerivedModelFileName = '';
  let modelDownloadSha256 = '';
  let modelDownloadExpectedBytes = '';
  let modelDownloadMaximumGiB = String(DEFAULT_MODEL_DOWNLOAD_LIMIT_GIB);
  let modelDownloadStarting = false;
  let modelDownloadCancellingIds: string[] = [];
  let modelDownloadError = '';
  let pendingModelDownload: ModelDownloadCapture | null = null;
  let modelDownloadUncertain = false;
  let modelDownloadCanAbandon = false;
  let modelDownloads: ModelDownloadSnapshot[] = [];
  let modelDownloadSequenceByCommand: Record<string, number> = {};
  let handledModelDownloadCompletions: string[] = [];
  let unlistenModelDownloadEvents: (() => void) | undefined;
  let modelDownloadListenerDisposed = false;
  let modelDownloadListenerPromise: Promise<void> | null = null;
  let modelDownloadPollTimer: number | undefined;
  let modelDownloadPollInFlight = false;
  let modelDownloadPollAttempt = 0;
  let branches: BranchCard[] = [];
  let branchNextCursor: BranchPageCursor | null = null;
  let branchFirstPageCursor: BranchPageCursor | null = null;
  let branchHasMore = false;
  let branchLoadingMore = false;
  let branchLoadMoreOwner = 0;
  let branchLoadedPastFirstPage = false;
  let branchBodyBlobByRun: Record<string, string> = {};
  let verifiedBranchBodyByRun: Record<string, VerifiedBranchBody> = {};
  let branchBodyErrorByRun: Record<string, string> = {};
  let branchRefreshSerial = 0;
  let branchRefreshInFlightCount = 0;
  let sourceTextarea: HTMLTextAreaElement | undefined;
  let weaveStarting = false;
  let uncertainWeave: WeaveCapture | null = null;
  const staleWeaveCleanupTimers = new Set<number>();
  let branchRefreshTimer: number | undefined;
  let branchPollTimer: number | undefined;
  let branchPollInFlight = false;
  let branchPollAttempt = 0;
  let branchPollEpoch = 0;
  let liveBranchText: Record<string, string> = {};
  let liveBranchState: Record<string, BranchEventOverlay> = {};
  let generationSequenceByRun: Record<string, number> = {};
  let cancellingRunIds: string[] = [];
  let cancellationCommandByRun: Record<string, string> = {};
  let promotionArmedCandidateId: string | null = null;
  let promotionInFlight = false;
  let uncertainPromotion: PromotionCapture | null = null;
  let unlistenGenerationEvents: (() => void) | undefined;
  let generationListenerDisposed = false;
  let saveTimer: number | undefined;
  let saveInFlight: Promise<void> | null = null;
  let saveQueued = false;
  let documentEpoch = 0;
  let editVersion = 0;
  let savedVersion = 0;
  let liveRegion = '';
  let sourceDisplayText = '';
  let sourceSelectionStart = 0;
  let sourceSelectionEnd = 0;
  let visibleVisualGhostPresentationKey = '';
  let visibleSourceGhostPresentationKey = '';
  let visualSelectionByte: number | null = null;
  let visualMutationPending = false;
  let verseCodec: VerseEditorCodec | null = null;
  let compositionActive = false;
  let sourceComposing = false;
  let visualEditor: {
    flushPending: () => boolean;
    focusAtDocumentEnd: () => boolean;
  } | null = null;
  let sourceEditor: {
    focusAtDocumentEnd: () => boolean;
  } | null = null;
  let componentMounted = false;
  let desktopWorkspaceStarted = false;
  let startupHeldForApplicationClose = false;
  let workspaceRestoreSerial = 0;
  let modelRefreshSerial = 0;
  let modelRefreshInFlightCount = 0;
  let modelLoadSerial = 0;
  let preferredWriterPending: WorkspaceRestoreCapture | null = null;
  let preferredWriterEnsureInFlight: Promise<boolean> | null = null;
  let preferredWriterWakeQueued = false;
  let applicationClosePhase: ApplicationClosePhase = 'running';
  let unlistenApplicationCloseRequest: (() => void) | undefined;
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
  let pendingCloseMayHaveCommitted = false;
  let pendingCloseAgency: ProjectCloseAgencySnapshot | null = null;
  let closeInFlight: Promise<ProjectCloseOutcome> | null = null;
  let reconciliation: ReconciliationPreview | null = null;
  let reconciliationResolution = '';
  let pendingReconciliationApply: ReconciliationApplyCapture | null = null;
  let reconciliationApplying = false;

  interface SaveCapture {
    commandId: string;
    restoreSerial: number;
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
    restoreSerial: number;
    projectId: string;
    sessionId: string;
    preview: ReconciliationPreview;
    resolvedText: string;
    reason: string;
  }

  interface WorkspaceRestoreCapture {
    restoreSerial: number;
    projectId: string;
    sessionId: string;
  }

  interface BranchEventOverlay {
    branchId: string;
    status?: BranchCard['status'];
    candidateId?: string;
    error?: string;
  }

  interface PromotionCapture {
    commandId: string;
    restoreSerial: number;
    projectId: string;
    sessionId: string;
    documentId: string;
    relativePath: string;
    candidateId: string;
    runId: string;
    sourceRevisionId: string;
    visibleBlobId: string;
  }

  interface WeaveCapture {
    commandId: string;
    epoch: number;
    projectId: string;
    sessionId: string;
    documentId: string;
    relativePath: string;
    documentKind: DocumentKind;
    sourceRevisionId: string;
    visibleBlobId: string;
    cursorByte: number;
    editVersion: number;
    intentEpoch: number;
    modelId: string;
  }

  interface AutocompleteRetryTicket {
    projectId: string;
    sessionId: string;
    documentId: string;
    sourceRevisionId: string;
    visibleBlobId: string;
    documentEpoch: number;
    editVersion: number;
    intentEpoch: number;
    mode: 'visual' | 'source';
    targetByte: number;
    modelId: string;
    sourceNewline: VerseNewlineKind | null;
    waitsRemaining: number;
  }

  type SuggestionSchedule =
    | { kind: 'edit_pause'; editVersion: number }
    | { kind: 'exhausted_retry'; ticket: AutocompleteRetryTicket };

  interface ModelDownloadCapture extends VerifiedDownloadForm {
    commandId: string;
  }

  interface HydratedBranchBodies {
    cards: BranchCard[];
    bodyBlobByRun: Record<string, string>;
    verifiedBodyByRun: Record<string, VerifiedBranchBody>;
    bodyErrorByRun: Record<string, string>;
  }

  type WindowLifecycleInstallation =
    | { status: 'ready' }
    | { status: 'close_pending'; outcome: ApplicationCloseOutcome }
    | { status: 'disposed' };

  type PromotionReloadOutcome = 'unchanged' | 'promoted' | 'source_changed' | 'reconciliation';

  const saveDelayMs = 900;
  const draftIntervalMs = 750;
  const branchPollBaseMs = 500;
  const branchPollMaxMs = 4_000;
  const branchPageSize = 24;
  const branchShelfBodyMaxBytes = 1024 * 1024;
  const applicationCloseRetry = new ApplicationCloseRetryScheduler({
    schedule: (callback, delayMs) => window.setTimeout(callback, delayMs),
    cancel: (handle) => window.clearTimeout(handle)
  }, 300);

  const applicationCloseCoordinator = new ApplicationCloseCoordinator({
    begin: () => {
      applicationClosePhase = 'closing';
      clearPreferredWriterRequest();
      cancelSuggestionTimer();
      if (compositionActive) {
        recordLocalFailure(
          'composition_active',
          'Finish the active text composition before closing Loom.'
        );
        announce(errorMessage);
        return false;
      }
      return true;
    },
    closeProject: async () => project ? closeProject() : { status: 'closed' },
    authorizeNativeClose: requestApplicationClose,
    abortNativeClose: abortApplicationClose,
    reset: () => {
      applicationClosePhase = 'running';
      if (transition === 'idle') requestPreferredWriterForCurrentWorkspace();
    },
    fail: (error) => {
      recordFailure(error);
      if (isApplicationCloseAbortFailure(error)) {
        transition = 'closing';
        cancelSuggestionTimer();
        saveMessage = 'Application close state unknown';
        announce('Loom could not confirm whether native closing was cancelled; editing remains locked');
      }
    }
  });
  const modelDownloadPollBaseMs = 750;
  const modelDownloadPollMaxMs = 5_000;
  const suggestionsIdleDelayMs = 1_800;
  const suggestionsRetryDelayMs = 350;
  const maximumAutomaticSuggestionRetries = 1;
  const maximumAutocompleteRetryWaits = 50;

  $: visibleDocuments = project?.documents.filter((candidate) => {
    const query = search.trim().toLocaleLowerCase();
    return !query || candidate.title.toLocaleLowerCase().includes(query) || candidate.relative_path.toLocaleLowerCase().includes(query);
  }) ?? [];
  $: loadedModel = models.find((model) => model.loaded) ?? null;
  $: currentModel = automaticWriterForBuildPolicy(models, buildModelPolicy);
  $: suggestionSetupNeeded = Boolean(
    project &&
    document &&
    suggestionsEnabled &&
    !currentModel &&
    !modelLoading &&
    !modelUnloading &&
    !modelChoosing &&
    modelRefreshInFlightCount === 0 &&
    preferredWriterEnsureInFlight === null &&
    preferredWriterPending === null &&
    transition === 'idle'
  );
  $: selectedModel = models.find((model) => model.model_path === selectedModelPath) ?? null;
  $: compatibleWriterModels = orderedLocalWriterCandidates(models)
    .map((candidate) => models.find((model) => model.model_path === candidate.modelPath))
    .filter((model): model is ModelCapabilitySummary => Boolean(model));
  $: otherLocalModels = models.filter((model) =>
    model.local &&
    model.header_verified &&
    !model.loaded &&
    !model.policy_candidate
  );
  $: activeModelDownloads = modelDownloads.filter((download) => !modelDownloadIsTerminal(download));
  $: pendingModelDownloadSnapshot = pendingModelDownload
    ? modelDownloads.find((download) => download.command_id === pendingModelDownload?.commandId) ?? null
    : null;
  $: activeBranchCount = branches.filter(
    (branch) => branch.status === 'queued' || branch.status === 'generating'
  ).length;
  $: currentReadyBranches = branches.filter((branch) =>
    branch.status === 'ready' &&
    branch.selection !== 'promote' &&
    branch.selection !== 'reject' &&
    branch.source_revision_id === document?.summary.revision_id &&
    branch.model_id === currentModel?.model_id
  );
  $: shelfBranches = branches.filter((branch) =>
    branchIsActionableOnShelf(branch, document?.summary.revision_id)
  );
  $: branchPromotionReady = Boolean(
    project &&
    document &&
    document.summary.kind !== 'hybrid' &&
    document.summary.active_blob_id === document.visible_blob_id &&
    transition === 'idle' &&
    editVersion === savedVersion &&
    (saveState === 'clean' || saveState === 'saved') &&
    !sourceDirty &&
    !visualMutationPending &&
    !compositionActive &&
    !saveInFlight &&
    !weaveStarting &&
    !staleDraft &&
    !uncertainDraft &&
    !uncertainSave &&
    !reconciliation &&
    !promotionInFlight &&
    !uncertainPromotion
  );
  $: visualGhostTargetByte = mode === 'visual' ? visualSelectionByte : null;
  $: visualGhostSurfaceKey = project && document
    ? `${project.session_id}:${document.summary.document_id}:${documentEpoch}:visual`
    : '';
  $: sourceGhostTargetByte = sourceGhostTargetByteFor(
    mode,
    Boolean(sourceTextarea),
    sourceSelectionStart,
    sourceSelectionEnd,
    sourceDisplayText,
    document,
    documentText,
    verseCodec
  );
  $: visualAutocompleteDisposition = autocompleteDisposition({
    active: mode === 'visual' && suggestionsEnabled && !visualMutationPending && branchPromotionReady,
    branches: currentReadyBranches,
    verifiedBodyByRun: verifiedBranchBodyByRun,
    dismissedCandidateIds,
    unpresentablePresentationKeys: unpresentableVisualGhostPresentationKeys,
    targetByte: visualGhostTargetByte,
    presentationCompatible: visualGhostTextMayBePlainProse
  });
  $: ghostSuggestion = visualAutocompleteDisposition.kind === 'available'
    ? visualAutocompleteDisposition.suggestion
    : null;
  $: sourceGhostNewline = document?.summary.kind === 'verse'
    ? verseCodec?.newline ?? 'mixed'
    : null;
  $: sourceAutocompleteDisposition = autocompleteDisposition({
    active: mode === 'source' && suggestionsEnabled && !sourceDirty && !compositionActive && branchPromotionReady,
    branches: currentReadyBranches,
    verifiedBodyByRun: verifiedBranchBodyByRun,
    dismissedCandidateIds,
    unpresentablePresentationKeys: [],
    targetByte: sourceGhostTargetByte,
    presentationCompatible: (text) =>
      sourceGhostPresentationCompatible(sourceDisplayText, text, sourceGhostNewline)
  });
  $: sourceGhostCandidate = sourceAutocompleteDisposition.kind === 'available'
    ? sourceAutocompleteDisposition.suggestion
    : null;
  $: sourceGhostSuggestion = sourceGhostCandidate;
  $: activeGhostSuggestion = mode === 'visual'
    ? visibleVerifiedGhostSuggestion(
      ghostSuggestion,
      visibleVisualGhostPresentationKey
    )
    : mode === 'source'
      ? visibleVerifiedGhostSuggestion(
        sourceGhostSuggestion,
        visibleSourceGhostPresentationKey
      )
      : null;
  $: reviewTargetByte = mode === 'visual'
    ? visualGhostTargetByte
    : mode === 'source'
      ? sourceGhostTargetByte
      : null;
  $: exactReviewBranches = suggestionsEnabled && branchPromotionReady && reviewTargetByte !== null
    ? shelfBranches.filter((branch) =>
      branch.status === 'ready' &&
      branch.target_start_byte === reviewTargetByte &&
      branch.target_end_byte === reviewTargetByte &&
      Boolean(verifiedGhostSuggestion(branch, verifiedBranchBodyByRun[branch.run_id]))
    )
    : [];
  $: reviewableBranches = exactReviewBranches.filter((branch) =>
    candidateTextIsSurfaceable(branch.text)
  );
  $: suppressedReviewBranches = exactReviewBranches.filter((branch) =>
    !candidateTextIsSurfaceable(branch.text)
  );
  $: if (
    reviewableBranches.length > 0 &&
    !reviewableBranches.some((branch) => branch.candidate_id === reviewCandidateId)
  ) reviewCandidateId = reviewableBranches[0].candidate_id;
  $: reviewBranch = reviewableBranches.find(
    (branch) => branch.candidate_id === reviewCandidateId
  ) ?? reviewableBranches[0] ?? null;
  $: reviewBranchIndex = reviewBranch
    ? reviewableBranches.findIndex((branch) => branch.run_id === reviewBranch.run_id)
    : -1;
  $: reviewAffordance = ghostReviewAffordance(
    Boolean(activeGhostSuggestion),
    reviewableBranches.length
  );
  $: suggestionMenuState = suggestionsChanging
    ? '…'
    : modelLoading || modelChoosing || modelUnloading || modelDownloadStarting || activeModelDownloads.length > 0
      ? 'Preparing'
      : !suggestionsEnabled
        ? 'Off'
        : currentModel
          ? 'Ready'
          : 'Set up';
  $: if (
    activeGhostSuggestion &&
    activeGhostSuggestion.presentationKey !== announcedGhostPresentationKey
  ) {
    announcedGhostPresentationKey = activeGhostSuggestion.presentationKey;
    announce('Suggestion available. Tab accepts; Escape dismisses.');
  }
  $: automaticBoundaryIsExact = mode === 'visual'
    ? visualSelectionByte !== null
    : mode === 'source' && Boolean(sourceTextarea) && sourceSelectionStart === sourceSelectionEnd;
  $: canUseVisual = Boolean(
    document?.summary.kind === 'prose' && canUseVisualMarkdown(documentText, mode === 'visual')
  );
  $: weaveCursorAtStart = mode === 'source'
    ? sourceSelectionStart === 0
    : visualSelectionByte === 0;
  $: canStartAutomaticSuggestions = Boolean(
      document &&
      document.summary.kind !== 'hybrid' &&
      currentModel &&
      !modelLoading &&
      !modelUnloading &&
      suggestionsEnabled &&
      !uncertainWeave &&
      !compositionActive &&
      !visualMutationPending &&
      !sourceDirty &&
      !saveInFlight &&
      !weaveStarting &&
      !promotionInFlight &&
      transition === 'idle' &&
      !editorReadonly &&
      (saveState === 'clean' || saveState === 'saved') &&
      editVersion === savedVersion &&
      document.summary.revision_id &&
      document.summary.active_blob_id === document.visible_blob_id &&
      !weaveCursorAtStart &&
      automaticBoundaryIsExact &&
      activeBranchCount === 0
  );
  $: retryEvaluationSnapshot = {
    enabled: desktop && branchPromotionReady && suggestionsEnabled && Boolean(currentModel) && activeBranchCount === 0,
    disposition: mode === 'visual'
      ? visualAutocompleteDisposition
      : sourceAutocompleteDisposition
  };
  $: if (retryEvaluationSnapshot.enabled) {
    maybeRetryExhaustedAutocomplete(retryEvaluationSnapshot.disposition);
  }
  $: showVisual = mode === 'visual';
  $: showSource = mode === 'source';
  $: exactTextSurface = document?.summary.kind === 'verse';
  $: editorReadonly = transition !== 'idle' || staleDraft !== null || staleDraftRestoring || uncertainDraft !== null || uncertainSave !== null || reconciliation !== null || promotionInFlight || uncertainPromotion !== null;
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
    componentMounted = true;
    desktop = isDesktopRuntime();
    if (desktop) {
      void (async () => {
        try {
          const lifecycle = await installWindowLifecycleHandlers();
          switch (lifecycle.status) {
            case 'ready':
              startDesktopWorkspace();
              return;
            case 'close_pending':
              startupHeldForApplicationClose = true;
              return;
            case 'disposed':
              return;
            default: {
              const unreachable: never = lifecycle;
              return unreachable;
            }
          }
        } catch (error) {
          if (!componentMounted) return;
          recordFailure(error);
          announce('Loom could not install safe application close handling');
        }
      })();
    }
    window.addEventListener('keydown', handleGlobalKeydown);
    window.addEventListener('pointerdown', handleGlobalPointerdown);
    return () => {
      componentMounted = false;
      startupHeldForApplicationClose = false;
      workspaceRestoreSerial += 1;
      modelRefreshSerial += 1;
      modelLoadSerial += 1;
      clearPreferredWriterRequest();
      window.removeEventListener('keydown', handleGlobalKeydown);
      window.removeEventListener('pointerdown', handleGlobalPointerdown);
      if (saveTimer !== undefined) window.clearTimeout(saveTimer);
      if (sourceProjectionTimer !== undefined) window.clearTimeout(sourceProjectionTimer);
      if (draftTimer !== undefined) window.clearTimeout(draftTimer);
      if (branchRefreshTimer !== undefined) window.clearTimeout(branchRefreshTimer);
      if (branchPollTimer !== undefined) window.clearTimeout(branchPollTimer);
      branchPollEpoch += 1;
      branchPollInFlight = false;
      generationListenerDisposed = true;
      modelDownloadListenerDisposed = true;
      unlistenGenerationEvents?.();
      unlistenModelDownloadEvents?.();
      const closeRequestListener = unlistenApplicationCloseRequest;
      unlistenApplicationCloseRequest = undefined;
      closeRequestListener?.();
      if (modelDownloadPollTimer !== undefined) window.clearTimeout(modelDownloadPollTimer);
      if (suggestionsIdleTimer !== undefined) window.clearTimeout(suggestionsIdleTimer);
      applicationCloseRetry.dispose();
      for (const timer of staleWeaveCleanupTimers) window.clearTimeout(timer);
      staleWeaveCleanupTimers.clear();
      unlistenWindowFocus?.();
    };
  });

  function startDesktopWorkspace(): void {
    if (!componentMounted || desktopWorkspaceStarted) return;
    desktopWorkspaceStarted = true;
    void installWindowFocusHandler();
    void installGenerationEventListener();
    void restoreDesktopWorkspace();
  }

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

  function resetLiveGenerationView(): void {
    stopBranchPolling();
    unpresentableVisualGhostPresentationKeys = [];
    branchRefreshSerial += 1;
    branchNextCursor = null;
    branchFirstPageCursor = null;
    branchHasMore = false;
    branchLoadMoreOwner += 1;
    branchLoadingMore = false;
    branchLoadedPastFirstPage = false;
    branchBodyBlobByRun = {};
    verifiedBranchBodyByRun = {};
    branchBodyErrorByRun = {};
    liveBranchText = {};
    liveBranchState = {};
    generationSequenceByRun = {};
    cancellingRunIds = [];
    cancellationCommandByRun = {};
    uncertainWeave = null;
  }

  function stopBranchPolling(): void {
    if (branchPollTimer !== undefined) window.clearTimeout(branchPollTimer);
    branchPollTimer = undefined;
    branchPollInFlight = false;
    branchPollAttempt = 0;
    branchPollEpoch += 1;
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
    closeStrandReview();
    document = null;
    documentText = '';
    sourceDisplayText = '';
    sourceDirty = false;
    verseCodec = null;
    visualEditor = null;
    compositionActive = false;
    sourceComposing = false;
    branches = [];
    promotionArmedCandidateId = null;
    resetLiveGenerationView();
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
    appText: string | null,
    expectedScope?: ProjectRestoreScope
  ): Promise<ReconciliationPreview> {
    const scope = expectedScope ?? (project ? {
      projectId: project.project_id,
      sessionId: project.session_id,
      restoreSerial: workspaceRestoreSerial
    } : null);
    if (
      !scope ||
      !projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, scope) ||
      !summary.revision_id ||
      !summary.active_blob_id
    ) {
      throw new Error('External reconciliation requires an immutable source revision and base blob.');
    }
    const preview = await previewDocumentReconciliation(
      scope.projectId,
      scope.sessionId,
      summary.document_id,
      summary.relative_path,
      summary.revision_id,
      summary.active_blob_id,
      appText
    );
    if (!projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, scope)) {
      throw new Error('The project session changed while Loom prepared reconciliation.');
    }
    if (
      preview.project_id !== scope.projectId ||
      preview.session_id !== scope.sessionId ||
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
    if (!projectSessionIsCurrent(project, captured)) {
      throw new Error('The checkpoint belongs to a stale project session.');
    }
    if (!receipt.result_revision_id || !receipt.result_blob_id) {
      throw new Error('The committed checkpoint receipt is missing its result identity.');
    }
    const refreshed = await currentProjectSession();
    if (
      refreshed.project_id !== captured.projectId ||
      refreshed.session_id !== captured.sessionId ||
      !projectSessionIsCurrent(project, captured)
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
      if (!projectSessionIsCurrent(project, captured)) {
        throw new Error('The project session changed while Loom protected newer editor text.');
      }
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
    const preview = await requestReconciliationPreview(target, null, {
      projectId: captured.projectId,
      sessionId: captured.sessionId,
      restoreSerial: captured.restoreSerial
    });
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
    announce('The save is in history, but the changed visible file still needs reconciliation');
  }

  async function activateReconciliationProjectionConflict(
    captured: ReconciliationApplyCapture,
    receipt: CommandReceipt
  ): Promise<void> {
    if (!projectSessionIsCurrent(project, captured)) {
      throw new Error('The reconciliation belongs to a stale project session.');
    }
    const refreshed = await currentProjectSession();
    if (
      refreshed.project_id !== captured.projectId ||
      refreshed.session_id !== captured.sessionId ||
      !projectSessionIsCurrent(project, captured)
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
    const preview = await requestReconciliationPreview(target, captured.resolvedText, {
      projectId: captured.projectId,
      sessionId: captured.sessionId,
      restoreSerial: captured.restoreSerial
    });
    activateReconciliation(preview);
    announce('The resolution is in history; a newer external change now needs review');
  }

  async function installWindowLifecycleHandlers(): Promise<WindowLifecycleInstallation> {
    const unlisten = await listenForApplicationCloseRequests(() => {
      void closeWindowGracefully();
    });
    if (!componentMounted) {
      unlisten();
      return { status: 'disposed' };
    }
    const previousListener = unlistenApplicationCloseRequest;
    unlistenApplicationCloseRequest = unlisten;
    previousListener?.();
    let closePending: boolean;
    try {
      closePending = await applicationClosePending();
    } catch (error) {
      if (unlistenApplicationCloseRequest === unlisten) {
        unlistenApplicationCloseRequest = undefined;
        unlisten();
      }
      throw error;
    }
    if (!componentMounted) {
      if (unlistenApplicationCloseRequest === unlisten) {
        unlistenApplicationCloseRequest = undefined;
        unlisten();
      }
      return { status: 'disposed' };
    }
    if (!closePending) return { status: 'ready' };

    const outcome = await closeWindowGracefully();
    if (!componentMounted) return { status: 'disposed' };
    return applicationStartupDisposition(outcome) === 'continue'
      ? { status: 'ready' }
      : { status: 'close_pending', outcome };
  }

  async function installWindowFocusHandler(): Promise<void> {
    try {
      const unlisten = await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
        if (!focused && !compositionActive && !reconciliation) {
          flushEditors();
          void saveNow();
        }
      });
      if (!componentMounted) {
        unlisten();
        return;
      }
      unlistenWindowFocus?.();
      unlistenWindowFocus = unlisten;
    } catch (error) {
      if (componentMounted) recordFailure(error);
    }
  }

  async function installGenerationEventListener(): Promise<void> {
    try {
      const unlisten = await listenForGenerationEvents(handleGenerationEnvelope);
      if (generationListenerDisposed) {
        unlisten();
      } else {
        unlistenGenerationEvents?.();
        unlistenGenerationEvents = unlisten;
      }
    } catch (error) {
      if (!generationListenerDisposed) {
        recordFailure(error);
        announce('Private strand events are unavailable');
      }
    }
  }

  async function ensureModelDownloadEventListener(): Promise<void> {
    if (unlistenModelDownloadEvents || modelDownloadListenerDisposed) return;
    if (!modelDownloadListenerPromise) {
      modelDownloadListenerPromise = (async () => {
        const unlisten = await listenForModelDownloadEvents(handleModelDownloadEvent);
        if (modelDownloadListenerDisposed) {
          unlisten();
        } else {
          unlistenModelDownloadEvents = unlisten;
        }
      })();
    }
    try {
      await modelDownloadListenerPromise;
    } finally {
      modelDownloadListenerPromise = null;
    }
  }

  function handleModelDownloadEvent(snapshot: ModelDownloadSnapshot): void {
    try {
      applyModelDownloadSnapshot(snapshot, true);
    } catch (error) {
      modelDownloadError = error instanceof Error
        ? error.message
        : 'Loom ignored an invalid model download event.';
    }
  }

  function applyModelDownloadSnapshot(
    snapshot: ModelDownloadSnapshot,
    arrivedAsEvent: boolean
  ): void {
    validateModelDownloadSnapshot(snapshot);
    const priorSequence = modelDownloadSequenceByCommand[snapshot.command_id];
    const priorSnapshot = modelDownloads.find(
      (download) => download.command_id === snapshot.command_id
    );
    if (
      priorSequence !== undefined &&
      (
        snapshot.event_sequence < priorSequence ||
        (
          snapshot.event_sequence === priorSequence &&
          (arrivedAsEvent || snapshot.event_delivery_failures <= (priorSnapshot?.event_delivery_failures ?? 0))
        )
      )
    ) return;
    modelDownloadSequenceByCommand = {
      ...modelDownloadSequenceByCommand,
      [snapshot.command_id]: snapshot.event_sequence
    };
    const withoutCurrent = modelDownloads.filter(
      (download) => download.command_id !== snapshot.command_id
    );
    modelDownloads = [snapshot, ...withoutCurrent].sort(
      (left, right) => right.updated_at_unix_ms - left.updated_at_unix_ms
    );
    if (arrivedAsEvent) modelDownloadPollAttempt = 0;

    if (modelDownloadIsTerminal(snapshot)) {
      if (pendingModelDownload?.commandId === snapshot.command_id) {
        pendingModelDownload = null;
        modelDownloadUncertain = false;
        modelDownloadCanAbandon = false;
      }
      modelDownloadCancellingIds = modelDownloadCancellingIds.filter(
        (commandId) => commandId !== snapshot.command_id
      );
      if (snapshot.status.status === 'failed') {
        modelDownloadError = snapshot.status.message;
      } else if (snapshot.status.status === 'cancelled') {
        modelDownloadError = '';
        announce(`${snapshot.display_name} download cancelled`);
      } else if (snapshot.status.status === 'completed') {
        modelDownloadError = '';
        void reconcileCompletedModelDownload(snapshot);
      }
    }
    scheduleModelDownloadPoll();
  }

  function validateModelDownloadSnapshot(snapshot: ModelDownloadSnapshot): void {
    if (!/^[0-9A-HJKMNP-TV-Z]{26}$/u.test(snapshot.command_id)) {
      throw new Error('Loom ignored a model download with an invalid command identity.');
    }
    const numbers = [
      snapshot.downloaded_bytes,
      snapshot.resumed_from_bytes,
      snapshot.event_sequence,
      snapshot.event_delivery_failures,
      snapshot.updated_at_unix_ms
    ];
    if (snapshot.expected_bytes !== null) numbers.push(snapshot.expected_bytes);
    if (snapshot.total_bytes !== null) numbers.push(snapshot.total_bytes);
    if (numbers.some((value) => !Number.isSafeInteger(value) || value < 0)) {
      throw new Error('Loom ignored model download evidence outside the safe numeric range.');
    }
    if (!/^[0-9a-f]{64}$/u.test(snapshot.expected_sha256)) {
      throw new Error('Loom ignored a model download with an invalid expected checksum.');
    }
    if (snapshot.status.status === 'completed') {
      if (
        !Number.isSafeInteger(snapshot.status.bytes) ||
        snapshot.status.bytes < 0 ||
        snapshot.status.sha256 !== snapshot.expected_sha256
      ) {
        throw new Error('Loom ignored a completed model download whose evidence did not match its request.');
      }
    }
  }

  function modelDownloadIsTerminal(download: ModelDownloadSnapshot): boolean {
    return download.status.status === 'completed' ||
      download.status.status === 'cancelled' ||
      download.status.status === 'failed';
  }

  async function reconcileCompletedModelDownload(
    snapshot: ModelDownloadSnapshot
  ): Promise<void> {
    if (handledModelDownloadCompletions.includes(snapshot.command_id)) return;
    handledModelDownloadCompletions = [
      ...handledModelDownloadCompletions,
      snapshot.command_id
    ];
    await refreshCurrentModelsAndEnsureWriter();
    const downloaded = models.find((model) => model.model_path === snapshot.target_path);
    if (downloaded) {
      selectedModelPath = downloaded.model_path;
      announce(`${snapshot.display_name} passed checksum and GGUF verification and is ready to inspect`);
    } else {
      modelDownloadError = 'The verified file was installed, but model discovery did not return it yet.';
      announce('Model download verified; discovery needs another refresh');
    }
  }

  async function selectCompletedModelDownload(
    snapshot: ModelDownloadSnapshot
  ): Promise<void> {
    modelDownloadError = '';
    await refreshCurrentModelsAndEnsureWriter();
    const discovered = models.find((model) => model.model_path === snapshot.target_path);
    if (!discovered) {
      modelDownloadError = 'The verified file is installed, but model discovery did not return it.';
      return;
    }
    selectedModelPath = discovered.model_path;
    announce(`${discovered.display_name} selected; load it when you want local continuation`);
  }

  async function recoverModelDownloads(): Promise<void> {
    try {
      await ensureModelDownloadEventListener();
    } catch (error) {
      if (modelManagerOpen) {
        modelDownloadError = `${normalizeFailure(error).message} Command-status recovery remains active.`;
      }
    }
    try {
      const snapshots = await listModelDownloads();
      for (const snapshot of snapshots.slice().reverse()) {
        applyModelDownloadSnapshot(snapshot, false);
      }
      scheduleModelDownloadPoll();
    } catch (error) {
      if (modelManagerOpen) modelDownloadError = normalizeFailure(error).message;
    }
  }

  function scheduleModelDownloadPoll(): void {
    if (modelDownloadPollTimer !== undefined) {
      window.clearTimeout(modelDownloadPollTimer);
      modelDownloadPollTimer = undefined;
    }
    if (
      modelDownloadListenerDisposed ||
      modelDownloadPollInFlight ||
      modelDownloads.every(modelDownloadIsTerminal)
    ) return;
    const delay = Math.min(
      modelDownloadPollBaseMs * (2 ** Math.min(modelDownloadPollAttempt, 3)),
      modelDownloadPollMaxMs
    );
    modelDownloadPollTimer = window.setTimeout(() => {
      modelDownloadPollTimer = undefined;
      void pollActiveModelDownloads();
    }, delay);
  }

  async function pollActiveModelDownloads(): Promise<void> {
    if (modelDownloadPollInFlight) return;
    const commandIds = modelDownloads
      .filter((download) => !modelDownloadIsTerminal(download))
      .slice(0, 2)
      .map((download) => download.command_id);
    if (commandIds.length === 0) return;
    modelDownloadPollInFlight = true;
    try {
      for (const commandId of commandIds) {
        const snapshot = await getModelDownloadStatus(commandId);
        applyModelDownloadSnapshot(snapshot, false);
      }
      modelDownloadPollAttempt += 1;
    } catch (error) {
      modelDownloadPollAttempt += 1;
      if (modelManagerOpen) modelDownloadError = normalizeFailure(error).message;
    } finally {
      modelDownloadPollInFlight = false;
      scheduleModelDownloadPoll();
    }
  }

  function updateModelDownloadUrl(value: string): void {
    modelDownloadUrl = value;
    const derived = deriveGgufFileName(value);
    if (!modelDownloadFileName || modelDownloadFileName === lastDerivedModelFileName) {
      modelDownloadFileName = derived;
    }
    lastDerivedModelFileName = derived;
  }

  async function beginOrRetryModelDownload(): Promise<void> {
    if (modelDownloadStarting) return;
    let capture = pendingModelDownload;
    if (!capture) {
      let request: VerifiedDownloadForm;
      try {
        request = validateVerifiedDownload({
          url: modelDownloadUrl,
          fileName: modelDownloadFileName,
          sha256: modelDownloadSha256,
          expectedBytes: modelDownloadExpectedBytes,
          maximumGiB: modelDownloadMaximumGiB
        });
      } catch (error) {
        modelDownloadError = error instanceof Error ? error.message : 'Review the download request.';
        return;
      }
      capture = { commandId: newUlid(), ...request };
      pendingModelDownload = capture;
    }
    modelDownloadStarting = true;
    modelDownloadUncertain = false;
    modelDownloadCanAbandon = false;
    modelDownloadError = '';
    try {
      // Both event channels must exist before the command can emit its first
      // queued snapshot. Polling remains the recovery oracle for lost events.
      await ensureModelDownloadEventListener();
      const snapshot = await startModelDownload({
        commandId: capture.commandId,
        url: capture.url,
        fileName: capture.fileName,
        expectedSha256: capture.sha256,
        expectedBytes: capture.expectedBytes,
        maxBytes: capture.maxBytes
      });
      applyModelDownloadSnapshot(snapshot, false);
      const observed = modelDownloads.find(
        (download) => download.command_id === capture.commandId
      );
      if (observed && !modelDownloadIsTerminal(observed)) {
        announce(`Verified local download started for ${capture.fileName}`);
      }
    } catch (startError) {
      try {
        const snapshot = await getModelDownloadStatus(capture.commandId);
        applyModelDownloadSnapshot(snapshot, false);
        announce(`Recovered ${capture.fileName} download state after a lost command reply`);
      } catch (statusError) {
        const startFailure = normalizeFailure(startError);
        const statusFailure = normalizeFailure(statusError);
        modelDownloadUncertain = true;
        modelDownloadCanAbandon = statusFailure.code === 'model_download_not_found' && !startFailure.retryable;
        modelDownloadError = `${startFailure.message} The exact command can be retried without starting a duplicate transfer.`;
      }
    } finally {
      modelDownloadStarting = false;
      scheduleModelDownloadPoll();
    }
  }

  function abandonUnstartedModelDownload(): void {
    if (!modelDownloadUncertain || !modelDownloadCanAbandon) return;
    pendingModelDownload = null;
    modelDownloadUncertain = false;
    modelDownloadCanAbandon = false;
    modelDownloadError = '';
  }

  async function cancelVerifiedModelDownload(commandId: string): Promise<void> {
    if (modelDownloadCancellingIds.includes(commandId)) return;
    modelDownloadCancellingIds = [...modelDownloadCancellingIds, commandId];
    modelDownloadError = '';
    try {
      const snapshot = await cancelModelDownload(commandId);
      applyModelDownloadSnapshot(snapshot, false);
      announce('Model download cancellation requested');
    } catch (error) {
      modelDownloadError = normalizeFailure(error).message;
      modelDownloadCancellingIds = modelDownloadCancellingIds.filter(
        (candidate) => candidate !== commandId
      );
    }
  }

  function handleGenerationEnvelope(envelope: DesktopGenerationEnvelope): void {
    if (!project || !document || !generationEventBelongsToScope(envelope, {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id
    })) return;

    const stream = envelope.event;
    const generation = stream.payload;
    if (!Number.isSafeInteger(generation.sequence) || generation.sequence < 0) {
      recordLocalFailure(
        'unsafe_generation_sequence',
        'Loom ignored a generation event whose sequence cannot be represented safely.'
      );
      return;
    }
    const previousSequence = generationSequenceByRun[generation.run_id];
    if (previousSequence !== undefined && generation.sequence <= previousSequence) return;
    const nextSequences = {
      ...generationSequenceByRun,
      [generation.run_id]: generation.sequence
    };
    const sequencedRunIds = Object.keys(nextSequences);
    while (sequencedRunIds.length > 64) delete nextSequences[sequencedRunIds.shift() as string];
    generationSequenceByRun = nextSequences;

    if (stream.event === 'generation_terminal') {
      const status = stream.payload.status === 'completed' ? 'ready' : stream.payload.status;
      recordLiveBranchState(generation.run_id, generation.branch_id, {
        status,
        candidateId: stream.payload.candidate_id,
        error: stream.payload.error
      });
      updateBranchFromEvent(generation.run_id, generation.branch_id, (branch) => ({
        ...branch,
        candidate_id: stream.payload.candidate_id ?? branch.candidate_id,
        status,
        error: stream.payload.error ?? branch.error,
        text: liveBranchText[generation.run_id] ?? branch.text
      }));
      cancellingRunIds = cancellingRunIds.filter((runId) => runId !== generation.run_id);
      scheduleBranchRefresh();
      if (status !== 'ready') announce(`A private strand ${status}`);
      return;
    }

    const kind = stream.payload.kind;
    switch (kind.kind) {
      case 'queued':
        recordLiveBranchState(generation.run_id, generation.branch_id, { status: 'queued' });
        updateBranchFromEvent(generation.run_id, generation.branch_id, (branch) => ({
          ...branch,
          status: 'queued'
        }));
        break;
      case 'prefilling':
      case 'generating':
      case 'token':
        recordLiveBranchState(generation.run_id, generation.branch_id, { status: 'generating' });
        updateBranchFromEvent(generation.run_id, generation.branch_id, (branch) => ({
          ...branch,
          status: 'generating'
        }));
        break;
      case 'text_delta': {
        const text = `${liveBranchText[generation.run_id] ?? ''}${kind.text}`;
        recordLiveBranchText(generation.run_id, text);
        recordLiveBranchState(generation.run_id, generation.branch_id, { status: 'generating' });
        updateBranchFromEvent(generation.run_id, generation.branch_id, (branch) => ({
          ...branch,
          status: 'generating',
          text
        }));
        break;
      }
      case 'warning':
        recordLiveBranchState(generation.run_id, generation.branch_id, { error: kind.message });
        updateBranchFromEvent(generation.run_id, generation.branch_id, (branch) => ({
          ...branch,
          error: kind.message
        }));
        break;
      case 'cancellation_requested':
        recordLiveBranchState(generation.run_id, generation.branch_id, { status: 'generating' });
        if (!cancellingRunIds.includes(generation.run_id)) {
          cancellingRunIds = [...cancellingRunIds, generation.run_id];
        }
        break;
      case 'candidate_ready':
        recordLiveBranchState(generation.run_id, generation.branch_id, {
          status: 'ready',
          candidateId: kind.candidate_id
        });
        updateBranchFromEvent(generation.run_id, generation.branch_id, (branch) => ({
          ...branch,
          candidate_id: kind.candidate_id,
          status: 'ready',
          text: liveBranchText[generation.run_id] ?? branch.text
        }));
        break;
    }
  }

  function recordLiveBranchText(runId: string, text: string): void {
    const next = { ...liveBranchText, [runId]: text };
    const runIds = Object.keys(next);
    while (runIds.length > 32) delete next[runIds.shift() as string];
    liveBranchText = next;
  }

  function recordLiveBranchState(
    runId: string,
    branchId: string,
    patch: Omit<BranchEventOverlay, 'branchId'>
  ): void {
    const next = {
      ...liveBranchState,
      [runId]: {
        ...liveBranchState[runId],
        ...patch,
        branchId
      }
    };
    const runIds = Object.keys(next);
    while (runIds.length > 32) delete next[runIds.shift() as string];
    liveBranchState = next;
  }

  function applyLiveBranchState(branch: BranchCard, persisted: boolean): BranchCard {
    const overlay = liveBranchState[branch.run_id];
    if (!overlay || overlay.branchId !== branch.branch_id) return branch;
    const storedIsTerminal = branch.status !== 'queued' && branch.status !== 'generating';
    if (persisted && storedIsTerminal) return branch;
    return {
      ...branch,
      candidate_id: overlay.candidateId ?? branch.candidate_id,
      status: overlay.status ?? branch.status,
      error: overlay.error ?? branch.error,
      text: liveBranchText[branch.run_id] ?? branch.text
    };
  }

  function updateBranchFromEvent(
    runId: string,
    branchId: string,
    update: (branch: BranchCard) => BranchCard
  ): void {
    let matched = false;
    const next = branches.map((branch) => {
      if (branch.run_id !== runId || branch.branch_id !== branchId) return branch;
      matched = true;
      return update(branch);
    });
    if (matched) branches = next;
  }

  function validateBranchSnapshots(
    snapshots: BranchSummary[],
    expectedDocumentId: string
  ): void {
    for (const branch of snapshots) {
      if (branch.document_id !== expectedDocumentId) {
        throw new Error('The desktop returned a branch for a different manuscript.');
      }
      if (
        !Number.isSafeInteger(branch.target_start_byte) ||
        !Number.isSafeInteger(branch.target_end_byte) ||
        branch.target_start_byte < 0 ||
        branch.target_end_byte < branch.target_start_byte
      ) {
        throw new Error('The desktop returned a branch target outside JavaScript\'s safe integer range.');
      }
      if (
        branch.output_byte_len !== null &&
        (!Number.isSafeInteger(branch.output_byte_len) || branch.output_byte_len < 0)
      ) {
        throw new Error('The desktop returned a branch body length outside JavaScript\'s safe integer range.');
      }
      if ((branch.output_blob_id === null) !== (branch.output_byte_len === null)) {
        throw new Error('The desktop returned incomplete branch body metadata.');
      }
    }
  }

  function validateBranchCursor(cursor: BranchPageCursor | null): void {
    if (!cursor) return;
    if (!/^[1-9][0-9]*$/.test(cursor.sequence) || !cursor.run_id) {
      throw new Error('The desktop returned an invalid branch page cursor.');
    }
  }

  function branchScopeMatches(
    projectId: string,
    sessionId: string,
    documentId: string,
    expectedViewEpoch: number,
    refreshSerial: number
  ): boolean {
    return expectedViewEpoch === branchPollEpoch &&
      refreshSerial === branchRefreshSerial &&
      project?.project_id === projectId &&
      project.session_id === sessionId &&
      document?.summary.document_id === documentId;
  }

  function cardsFromSummaries(summaries: BranchSummary[]): BranchCard[] {
    return summaries.map((summary) => {
      const verifiedBody = verifiedBranchBodyByRun[summary.run_id];
      const text = verifiedBodyMatchesBranch(verifiedBody, summary)
        ? verifiedBody.text
        : liveBranchText[summary.run_id] ?? '';
      return applyLiveBranchState({ ...summary, text }, true);
    });
  }

  async function hydrateBranchBodies(
    projectId: string,
    sessionId: string,
    documentId: string,
    cards: BranchCard[],
    expectedViewEpoch: number,
    refreshSerial: number
  ): Promise<HydratedBranchBodies | null> {
    const hydrated = [...cards];
    const bodyBlobByRun = { ...branchBodyBlobByRun };
    const verifiedBodyByRun = { ...verifiedBranchBodyByRun };
    const bodyErrorByRun = { ...branchBodyErrorByRun };
    for (let index = 0; index < hydrated.length; index += 1) {
      const branch = hydrated[index];
      const outputBlobId = branch.output_blob_id;
      const disposition = branchBodyDisposition(
        branch,
        bodyBlobByRun[branch.run_id],
        branchShelfBodyMaxBytes
      );
      if (disposition === 'absent') continue;
      if (!outputBlobId) throw new Error('The desktop omitted the branch body identity.');
      if (branch.output_byte_len === null) {
        throw new Error('The desktop omitted the indexed branch body length.');
      }
      if (disposition === 'cached') {
        const verifiedBody = verifiedBodyByRun[branch.run_id];
        if (
          verifiedBodyMatchesBranch(verifiedBody, branch) &&
          branch.text === verifiedBody.text
        ) continue;
        if (bodyErrorByRun[branch.run_id]) {
          hydrated[index] = { ...branch, text: '' };
          delete verifiedBodyByRun[branch.run_id];
          continue;
        }
      }
      if (disposition === 'too_large') {
        hydrated[index] = { ...branch, text: '' };
        bodyBlobByRun[branch.run_id] = outputBlobId;
        delete verifiedBodyByRun[branch.run_id];
        bodyErrorByRun[branch.run_id] = `Candidate text is ${branch.output_byte_len.toLocaleString()} bytes; the shelf preview limit is ${branchShelfBodyMaxBytes.toLocaleString()} bytes.`;
        continue;
      }
      const body = await getBranchBody(
        projectId,
        sessionId,
        documentId,
        branch.run_id,
        branchShelfBodyMaxBytes
      );
      if (!branchScopeMatches(
        projectId,
        sessionId,
        documentId,
        expectedViewEpoch,
        refreshSerial
      )) return null;
      if (!body) {
        throw new Error('The desktop returned a branch body that does not match its immutable metadata.');
      }
      const verifiedBody = await verifyBranchBody(body, branch);
      if (!verifiedBody) {
        throw new Error('The desktop returned branch text that does not match its immutable SHA-256 identity.');
      }
      hydrated[index] = { ...branch, text: verifiedBody.text };
      bodyBlobByRun[branch.run_id] = outputBlobId;
      verifiedBodyByRun[branch.run_id] = verifiedBody;
      delete bodyErrorByRun[branch.run_id];
    }
    if (!branchScopeMatches(
      projectId,
      sessionId,
      documentId,
      expectedViewEpoch,
      refreshSerial
    )) return null;
    return { cards: hydrated, bodyBlobByRun, verifiedBodyByRun, bodyErrorByRun };
  }

  function reconcileBranchActionState(): void {
    const activeRunIds = new Set(
      branches.filter(isBranchActive).map((branch) => branch.run_id)
    );
    cancellingRunIds = cancellingRunIds.filter((runId) => activeRunIds.has(runId));
    const nextCancellationCommands = { ...cancellationCommandByRun };
    for (const runId of Object.keys(nextCancellationCommands)) {
      if (!activeRunIds.has(runId)) delete nextCancellationCommands[runId];
    }
    cancellationCommandByRun = nextCancellationCommands;
    if (
      promotionArmedCandidateId &&
      !branches.some((branch) => branch.candidate_id === promotionArmedCandidateId)
    ) {
      promotionArmedCandidateId = null;
    }
  }

  function maybeRetryExhaustedAutocomplete(
    disposition: AutocompleteDisposition
  ): void {
    if (
      !desktop ||
      !project ||
      !document ||
      !currentModel ||
      !suggestionsEnabled ||
      !branchPromotionReady
    ) return;
    const sourceRevisionId = document.summary.revision_id;
    if (!sourceRevisionId) return;
    const retryMode = mode === 'visual' || mode === 'source' ? mode : null;
    if (!retryMode) return;
    const targetByte = retryMode === 'visual'
      ? visualGhostTargetByte
      : sourceGhostTargetByte;
    if (targetByte === null) return;
    const budgetKey = [
      project.project_id,
      project.session_id,
      document.summary.document_id,
      sourceRevisionId,
      editVersion
    ].join(':');
    const decision = planAutocompleteRetry(autocompleteRetryLedger, {
      disposition,
      budgetKey,
      activeBranchCount: branches.filter(isBranchActive).length,
      weaveStarting,
      maximumRetries: maximumAutomaticSuggestionRetries
    });
    autocompleteRetryLedger = decision.ledger;
    if (decision.kind === 'schedule') {
      scheduleAutocompleteRetry({
        projectId: project.project_id,
        sessionId: project.session_id,
        documentId: document.summary.document_id,
        sourceRevisionId,
        visibleBlobId: document.visible_blob_id,
        documentEpoch,
        editVersion,
        intentEpoch: suggestionIntentEpoch,
        mode: retryMode,
        targetByte,
        modelId: currentModel.model_id,
        sourceNewline: retryMode === 'source' ? sourceGhostNewline : null,
        waitsRemaining: maximumAutocompleteRetryWaits
      });
    }
  }

  async function refreshBranchesFor(
    projectId: string,
    sessionId: string,
    documentId: string,
    reportFailure = true,
    expectedViewEpoch = branchPollEpoch
  ): Promise<boolean> {
    const refreshSerial = ++branchRefreshSerial;
    branchRefreshInFlightCount += 1;
    try {
      const page = await getBranchPage(
        projectId,
        sessionId,
        documentId,
        null,
        branchPageSize
      );
      if (!branchScopeMatches(
        projectId,
        sessionId,
        documentId,
        expectedViewEpoch,
        refreshSerial
      )) return false;
      validateBranchSnapshots(page.branches, documentId);
      validateBranchCursor(page.next_cursor);
      if (page.has_more !== (page.next_cursor !== null)) {
        throw new Error('The desktop returned inconsistent branch page metadata.');
      }
      const firstPageCards = cardsFromSummaries(page.branches);
      branches = mergeNewestPage(firstPageCards, branches);
      const firstPageCursorChanged =
        page.next_cursor?.sequence !== branchFirstPageCursor?.sequence ||
        page.next_cursor?.run_id !== branchFirstPageCursor?.run_id;
      if (!branchLoadedPastFirstPage || firstPageCursorChanged) {
        branchNextCursor = page.next_cursor;
        branchHasMore = page.has_more;
        branchLoadedPastFirstPage = false;
      }
      branchFirstPageCursor = page.next_cursor;
      const hydration = await hydrateBranchBodies(
        projectId,
        sessionId,
        documentId,
        firstPageCards,
        expectedViewEpoch,
        refreshSerial
      );
      if (!hydration || !branchScopeMatches(
        projectId,
        sessionId,
        documentId,
        expectedViewEpoch,
        refreshSerial
      )) return false;
      const hydratedByRun = new Map(hydration.cards.map((branch) => [branch.run_id, branch]));
      branchBodyBlobByRun = hydration.bodyBlobByRun;
      verifiedBranchBodyByRun = hydration.verifiedBodyByRun;
      branchBodyErrorByRun = hydration.bodyErrorByRun;
      branches = branches.map((branch) => hydratedByRun.get(branch.run_id) ?? branch);
      reconcileBranchActionState();
      if (branches.some(isBranchActive)) scheduleActiveBranchPoll();
      return true;
    } catch (error) {
      if (
        reportFailure &&
        project?.project_id === projectId &&
        project.session_id === sessionId &&
        document?.summary.document_id === documentId
      ) {
        recordFailure(error);
        announce('Stored strands could not be refreshed');
      }
      return false;
    } finally {
      branchRefreshInFlightCount = Math.max(0, branchRefreshInFlightCount - 1);
    }
  }

  function refreshCurrentBranches(reportFailure = true): Promise<boolean> {
    if (!project || !document) {
      branches = [];
      return Promise.resolve(false);
    }
    return refreshBranchesFor(
      project.project_id,
      project.session_id,
      document.summary.document_id,
      reportFailure
    );
  }

  async function loadMoreBranches(): Promise<void> {
    if (
      !project ||
      !document ||
      !branchNextCursor ||
      !branchHasMore ||
      branchLoadingMore ||
      branchRefreshInFlightCount > 0
    ) return;
    const scope = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id,
      viewEpoch: branchPollEpoch,
      cursor: branchNextCursor
    };
    const refreshSerial = ++branchRefreshSerial;
    const loadOwner = ++branchLoadMoreOwner;
    branchLoadingMore = true;
    try {
      const page = await getBranchPage(
        scope.projectId,
        scope.sessionId,
        scope.documentId,
        scope.cursor,
        branchPageSize
      );
      if (!branchScopeMatches(
        scope.projectId,
        scope.sessionId,
        scope.documentId,
        scope.viewEpoch,
        refreshSerial
      )) return;
      validateBranchSnapshots(page.branches, scope.documentId);
      validateBranchCursor(page.next_cursor);
      if (page.has_more !== (page.next_cursor !== null)) {
        throw new Error('The desktop returned inconsistent branch page metadata.');
      }
      const cards = cardsFromSummaries(page.branches);
      branches = appendUniquePage(branches, cards);
      branchNextCursor = page.next_cursor;
      branchHasMore = page.has_more;
      branchLoadedPastFirstPage = true;
      const hydration = await hydrateBranchBodies(
        scope.projectId,
        scope.sessionId,
        scope.documentId,
        cards,
        scope.viewEpoch,
        refreshSerial
      );
      if (!hydration || !branchScopeMatches(
        scope.projectId,
        scope.sessionId,
        scope.documentId,
        scope.viewEpoch,
        refreshSerial
      )) return;
      const hydratedByRun = new Map(hydration.cards.map((branch) => [branch.run_id, branch]));
      branchBodyBlobByRun = hydration.bodyBlobByRun;
      verifiedBranchBodyByRun = hydration.verifiedBodyByRun;
      branchBodyErrorByRun = hydration.bodyErrorByRun;
      branches = branches.map((branch) => hydratedByRun.get(branch.run_id) ?? branch);
      reconcileBranchActionState();
    } catch (error) {
      if (
        project?.project_id === scope.projectId &&
        project.session_id === scope.sessionId &&
        document?.summary.document_id === scope.documentId
      ) {
        recordFailure(error);
        announce('Older strands could not be loaded');
      }
    } finally {
      if (branchLoadMoreOwner === loadOwner) branchLoadingMore = false;
    }
  }

  function scheduleBranchRefresh(): void {
    if (branchRefreshTimer !== undefined) window.clearTimeout(branchRefreshTimer);
    branchRefreshTimer = window.setTimeout(() => {
      branchRefreshTimer = undefined;
      void refreshCurrentBranches(false);
    }, 120);
  }

  function scheduleActiveBranchPoll(): void {
    if (
      branchPollTimer !== undefined ||
      branchPollInFlight ||
      !project ||
      !document ||
      !branches.some(isBranchActive)
    ) return;
    const pollEpoch = branchPollEpoch;
    const scope = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id
    };
    const delay = Math.min(
      branchPollBaseMs * (2 ** Math.min(branchPollAttempt, 3)),
      branchPollMaxMs
    );
    branchPollTimer = window.setTimeout(() => {
      branchPollTimer = undefined;
      void pollActiveBranches(scope, pollEpoch);
    }, delay);
  }

  async function pollActiveBranches(
    scope: { projectId: string; sessionId: string; documentId: string },
    pollEpoch: number
  ): Promise<void> {
    if (
      branchPollInFlight ||
      pollEpoch !== branchPollEpoch ||
      project?.project_id !== scope.projectId ||
      project.session_id !== scope.sessionId ||
      document?.summary.document_id !== scope.documentId
    ) return;
    branchPollInFlight = true;
    const refreshed = await refreshBranchesFor(
      scope.projectId,
      scope.sessionId,
      scope.documentId,
      false,
      pollEpoch
    );
    if (pollEpoch !== branchPollEpoch) return;
    branchPollInFlight = false;
    if (
      project?.project_id !== scope.projectId ||
      project.session_id !== scope.sessionId ||
      document?.summary.document_id !== scope.documentId
    ) return;
    if (!refreshed || branches.some(isBranchActive)) {
      branchPollAttempt += 1;
      scheduleActiveBranchPoll();
      return;
    }

    // One final authoritative read closes the local transition even when the
    // terminal Tauri emit was lost after the store committed it.
    branchPollAttempt = 0;
    branchPollTimer = window.setTimeout(() => {
      branchPollTimer = undefined;
      if (
        pollEpoch === branchPollEpoch &&
        project?.project_id === scope.projectId &&
        project.session_id === scope.sessionId &&
        document?.summary.document_id === scope.documentId
      ) void refreshBranchesFor(
        scope.projectId,
        scope.sessionId,
        scope.documentId,
        false,
        pollEpoch
      );
    }, 150);
  }

  async function closeWindowGracefully(): Promise<ApplicationCloseOutcome> {
    const attemptEpoch = applicationCloseRetry.beginAttempt();
    const outcome = await applicationCloseCoordinator.request();
    applicationCloseRetry.settle(attemptEpoch, outcome, () => {
      if (componentMounted) void closeWindowGracefully();
    });
    if (
      startupHeldForApplicationClose &&
      applicationStartupDisposition(outcome) === 'continue'
    ) {
      startupHeldForApplicationClose = false;
      startDesktopWorkspace();
    }
    return outcome;
  }

  function workspaceRestoreIsCurrent(captured: WorkspaceRestoreCapture): boolean {
    return Boolean(
      componentMounted &&
      captured.restoreSerial === workspaceRestoreSerial &&
      project?.project_id === captured.projectId &&
      project.session_id === captured.sessionId
    );
  }

  function workspaceCapturesMatch(
    left: WorkspaceRestoreCapture | null,
    right: WorkspaceRestoreCapture | null
  ): boolean {
    return Boolean(
      left &&
      right &&
      left.restoreSerial === right.restoreSerial &&
      left.projectId === right.projectId &&
      left.sessionId === right.sessionId
    );
  }

  function currentWorkspaceCapture(): WorkspaceRestoreCapture | null {
    if (!project) return null;
    return {
      restoreSerial: workspaceRestoreSerial,
      projectId: project.project_id,
      sessionId: project.session_id
    };
  }

  function clearPreferredWriterRequest(captured?: WorkspaceRestoreCapture): void {
    if (!captured || workspaceCapturesMatch(preferredWriterPending, captured)) {
      preferredWriterPending = null;
    }
  }

  function queuePreferredWriterRequest(captured: WorkspaceRestoreCapture): void {
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      !suggestionsEnabled ||
      !workspaceRestoreIsCurrent(captured)
    ) return;
    preferredWriterPending = { ...captured };
  }

  function wakePreferredWriterEnsure(): void {
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      preferredWriterWakeQueued ||
      !preferredWriterPending
    ) return;
    preferredWriterWakeQueued = true;
    queueMicrotask(() => {
      preferredWriterWakeQueued = false;
      void drainPreferredWriterEnsure();
    });
  }

  function requestPreferredWriterEnsure(captured: WorkspaceRestoreCapture): void {
    queuePreferredWriterRequest(captured);
    wakePreferredWriterEnsure();
  }

  function requestPreferredWriterForCurrentWorkspace(): void {
    const captured = currentWorkspaceCapture();
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      !captured ||
      !suggestionsEnabled ||
      currentModel
    ) return;
    requestPreferredWriterEnsure(captured);
  }

  function drainPreferredWriterEnsure(): Promise<boolean> {
    if (preferredWriterEnsureInFlight) return preferredWriterEnsureInFlight;
    const captured = preferredWriterPending;
    if (!captured) return Promise.resolve(Boolean(currentModel));
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      !suggestionsEnabled ||
      !workspaceRestoreIsCurrent(captured)
    ) {
      clearPreferredWriterRequest(captured);
      return Promise.resolve(false);
    }
    if (
      modelLoading ||
      modelUnloading ||
      modelRefreshInFlightCount > 0 ||
      transition !== 'idle' ||
      !document
    ) return Promise.resolve(false);

    preferredWriterPending = null;
    const task = ensurePreferredWriterOnce(captured).catch(() => false);
    preferredWriterEnsureInFlight = task;
    void task.finally(() => {
      if (preferredWriterEnsureInFlight !== task) return;
      preferredWriterEnsureInFlight = null;
      wakePreferredWriterEnsure();
    });
    return task;
  }

  async function ensurePreferredWriterOnce(
    captured: WorkspaceRestoreCapture
  ): Promise<boolean> {
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      !suggestionsEnabled ||
      !workspaceRestoreIsCurrent(captured)
    ) return false;
    if (currentModel) {
      if (document) scheduleAutomaticSuggestions(editVersion);
      return true;
    }

    const refreshed = await refreshModels(captured);
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      !suggestionsEnabled ||
      !workspaceRestoreIsCurrent(captured)
    ) return false;
    if (!refreshed && modelRefreshInFlightCount > 0) {
      queuePreferredWriterRequest(captured);
      return false;
    }
    await tick();
    if (!applicationAllowsModelPreparation(applicationClosePhase)) return false;
    if (currentModel) {
      if (document) scheduleAutomaticSuggestions(editVersion);
      return true;
    }
    if (modelLoading || modelUnloading || transition !== 'idle' || !document) {
      queuePreferredWriterRequest(captured);
      return false;
    }
    return loadPreferredSuggestionModel(captured);
  }

  async function refreshModels(expectedWorkspace?: WorkspaceRestoreCapture): Promise<boolean> {
    const refreshSerial = ++modelRefreshSerial;
    modelRefreshInFlightCount += 1;
    try {
      const discovered = await listModels();
      if (
        !componentMounted ||
        refreshSerial !== modelRefreshSerial ||
        (expectedWorkspace && !workspaceRestoreIsCurrent(expectedWorkspace))
      ) return false;
      models = discovered;
      const rememberedPath = loadLastLocalModelPath();
      selectedModelPath = preferredWriterModelPath(
        discovered,
        rememberedPath,
        selectedModelPath
      );
      return true;
    } catch {
      if (
        !componentMounted ||
        refreshSerial !== modelRefreshSerial ||
        (expectedWorkspace && !workspaceRestoreIsCurrent(expectedWorkspace))
      ) return false;
      models = [];
      selectedModelPath = '';
      return false;
    } finally {
      modelRefreshInFlightCount = Math.max(0, modelRefreshInFlightCount - 1);
      if (
        applicationAllowsModelPreparation(applicationClosePhase) &&
        modelRefreshInFlightCount === 0
      ) wakePreferredWriterEnsure();
    }
  }

  async function refreshCurrentModelsAndEnsureWriter(): Promise<boolean> {
    const captured = currentWorkspaceCapture();
    const refreshed = await refreshModels(captured ?? undefined);
    if (
      applicationAllowsModelPreparation(applicationClosePhase) &&
      captured &&
      workspaceRestoreIsCurrent(captured)
    ) {
      requestPreferredWriterForCurrentWorkspace();
    }
    return refreshed;
  }

  function closeProjectMenu(): void {
    if (projectMenu) projectMenu.open = false;
  }

  async function setOutlineOpen(open: boolean): Promise<void> {
    outlineOpen = open;
    await tick();
    if (open) outlineSearch?.focus();
    else outlineToggle?.focus();
  }

  function openModelManager(trigger: HTMLElement): void {
    closeProjectMenu();
    if (lastFailure?.code.startsWith('model_') || lastFailure?.code.startsWith('writing_model_')) {
      clearFailure();
    }
    modelSetupError = '';
    modelManagerReturnFocus = trigger;
    modelManagerOpen = true;
    modelDownloadError = '';
    void recoverModelDownloads();
    void refreshCurrentModelsAndEnsureWriter();
    void tick().then(() => {
      if (!modelManagerPanel) return;
      const preferred = modelManagerPanel.querySelector<HTMLElement>(
        '[data-model-manager-initial-focus]:not([disabled])'
      );
      (preferred ?? focusableElementsWithin(modelManagerPanel)[0] ?? modelManagerPanel).focus();
    });
  }

  function closeModelManager(focusWritingSurface = false): void {
    modelManagerOpen = false;
    const trigger = modelManagerReturnFocus;
    modelManagerReturnFocus = null;
    void tick().then(() => {
      if (focusWritingSurface) {
        focusCurrentWritingSurfaceAtEnd();
        return;
      }
      if (focusConnectedControl(trigger)) return;
      if (focusConnectedControl(projectMenuTrigger)) return;
      focusCurrentWritingSurfaceAtEnd();
    });
  }

  function focusConnectedControl(target: HTMLElement | null | undefined): boolean {
    if (!target?.isConnected || target.hidden || target.getClientRects().length === 0) return false;
    target.focus();
    return window.document.activeElement === target;
  }

  function suggestionPreferenceKey(projectId: string): string {
    return `loom:suggestions:${projectId}`;
  }

  const lastLocalModelKey = 'loom:last-local-model';

  function loadLastLocalModelPath(): string | null {
    try {
      return window.localStorage.getItem(lastLocalModelKey);
    } catch {
      return null;
    }
  }

  function rememberLastLocalModelPath(modelPath: string): void {
    try {
      window.localStorage.setItem(lastLocalModelKey, modelPath);
    } catch {
      // Discovery remains available when browser persistence is unavailable.
    }
  }

  function forgetLastLocalModelPath(modelPath: string): void {
    try {
      if (window.localStorage.getItem(lastLocalModelKey) === modelPath) {
        window.localStorage.removeItem(lastLocalModelKey);
      }
    } catch {
      // Storage is only a convenience; native policy verification is authority.
    }
  }

  function rememberedWriterPathIsInvalid(code: string): boolean {
    switch (code) {
      case 'policy_model_not_found':
      case 'policy_model_path_error':
      case 'policy_model_size_mismatch':
      case 'policy_model_digest_mismatch':
      case 'policy_model_file_changed':
      case 'policy_model_header_unverified':
      case 'policy_model_identity_mismatch':
      case 'policy_model_capability_mismatch':
        return true;
      default:
        return false;
    }
  }

  function loadSuggestionPreference(projectId: string): boolean {
    try {
      return suggestionsEnabledFromStoredPreference(
        window.localStorage.getItem(suggestionPreferenceKey(projectId)),
        buildModelPolicy?.activation ?? null
      );
    } catch {
      return false;
    }
  }

  function cancelSuggestionTimer(): void {
    if (suggestionsIdleTimer !== undefined) window.clearTimeout(suggestionsIdleTimer);
    suggestionsIdleTimer = undefined;
    scheduledSuggestion = null;
    suggestionIntentEpoch += 1;
  }

  async function setSuggestionsEnabled(enabled: boolean, persist = true): Promise<void> {
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      !project ||
      suggestionsChanging
    ) return;
    if (enabled && !buildModelPolicy) {
      announce('Suggestions remain off because this build could not verify its local writer policy');
      return;
    }
    const boundProject = project;
    const previousEnabled = suggestionsEnabled;
    const previousDismissedCandidateIds = dismissedCandidateIds;
    suggestionsChanging = true;
    suggestionIntentEpoch += 1;
    if (!enabled) {
      suggestionsEnabled = false;
      cancelSuggestionTimer();
      dismissedCandidateIds = currentReadyBranches
        .map((branch) => branch.candidate_id)
        .filter((candidateId): candidateId is string => Boolean(candidateId));
    }
    try {
      await setSuggestionsPolicy(boundProject.project_id, boundProject.session_id, enabled);
      if (
        !applicationAllowsModelPreparation(applicationClosePhase) ||
        project?.project_id !== boundProject.project_id ||
        project.session_id !== boundProject.session_id
      ) return;
      suggestionsEnabled = enabled;
      if (persist) {
        try {
          window.localStorage.setItem(suggestionPreferenceKey(project.project_id), enabled ? 'on' : 'off');
        } catch {
          // The backend gate remains authoritative if browser persistence is unavailable.
        }
      }
      if (!enabled) {
        clearPreferredWriterRequest();
        scheduleActiveBranchPoll();
      }
      let writerReady = Boolean(currentModel);
      if (enabled && !writerReady) {
        const captured: WorkspaceRestoreCapture = {
          restoreSerial: workspaceRestoreSerial,
          projectId: boundProject.project_id,
          sessionId: boundProject.session_id
        };
        requestPreferredWriterEnsure(captured);
        writerReady = Boolean(currentModel);
      }
      announce(enabled
        ? writerReady
          ? 'Suggestions on; Loom will quietly prepare private strands when typing pauses'
          : 'Suggestions on; Loom is preparing a tested local writer'
        : 'Suggestions off');
      if (enabled && writerReady && document) {
        scheduleAutomaticSuggestions(editVersion);
      }
    } catch (error) {
      suggestionsEnabled = previousEnabled;
      dismissedCandidateIds = previousDismissedCandidateIds;
      clearPreferredWriterRequest();
      cancelSuggestionTimer();
      if (!enabled && activeBranchCount > 0) void cancelActiveBranches();
      recordFailure(error);
      announce('Suggestions remain off because the project gate could not be changed');
    } finally {
      suggestionsChanging = false;
    }
  }

  function focusableElementsWithin(container: HTMLElement): HTMLElement[] {
    return Array.from(container.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), summary, [href], [tabindex]:not([tabindex="-1"])'
    )).filter((element) => !element.hasAttribute('hidden') && element.getClientRects().length > 0);
  }

  function trapFocusWithin(event: KeyboardEvent, container: HTMLElement | undefined): void {
    if (event.key !== 'Tab' || !container) return;
    const focusable = focusableElementsWithin(container);
    if (focusable.length === 0) {
      event.preventDefault();
      container.focus();
      return;
    }
    const first = focusable[0];
    const last = focusable.at(-1);
    if (!first || !last) return;
    const active = window.document.activeElement;
    const activeIsContainer = active === container;
    const activeIsOutside = !active || !container.contains(active);
    if (event.shiftKey && (activeIsContainer || activeIsOutside || active === first)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (activeIsContainer || activeIsOutside || active === last)) {
      event.preventDefault();
      first.focus();
    }
  }

  function trapModelManagerFocus(event: KeyboardEvent): void {
    trapFocusWithin(event, modelManagerPanel);
  }

  async function installLoadedModel(
    loaded: ModelCapabilitySummary,
    quiet: boolean,
    expectedWorkspace?: WorkspaceRestoreCapture
  ): Promise<boolean> {
    if (!applicationAllowsModelPreparation(applicationClosePhase)) return false;
    if (expectedWorkspace && !workspaceRestoreIsCurrent(expectedWorkspace)) return false;
    models = [
      loaded,
      ...models
        .filter((model) => model.model_path !== loaded.model_path)
        .map((model) => ({ ...model, loaded: false }))
    ];
    selectedModelPath = loaded.model_path;
    rememberLastLocalModelPath(loaded.model_path);
    if (!quiet) {
      announce(`${loaded.display_name} is verified for exact local completion`);
    }
    if (suggestionsEnabled && loaded.completion && document) {
      await tick();
      if (!applicationAllowsModelPreparation(applicationClosePhase)) return false;
      scheduleAutomaticSuggestions(editVersion);
    }
    return true;
  }

  async function loadPreferredSuggestionModel(
    expectedWorkspace?: WorkspaceRestoreCapture
  ): Promise<boolean> {
    if (!applicationAllowsModelPreparation(applicationClosePhase)) return false;
    if (currentModel) return true;
    if (!document || transition !== 'idle' || modelLoading || modelUnloading) return false;
    if (expectedWorkspace && !workspaceRestoreIsCurrent(expectedWorkspace)) return false;
    const discoveredCandidates = orderedLocalWriterCandidates(models);
    const rememberedPath = loadLastLocalModelPath();
    const rememberedProfile = writerProfileForBuildPolicy(buildModelPolicy);
    const candidates = [
      ...(rememberedPath && rememberedProfile &&
      !discoveredCandidates.some((candidate) => candidate.modelPath === rememberedPath)
        ? [{
            modelPath: rememberedPath,
            profileId: rememberedProfile,
            policyRank: -1,
            remembered: true
          }]
        : []),
      ...discoveredCandidates.map((candidate) => ({ ...candidate, remembered: false }))
    ];
    for (const candidate of candidates) {
      if (!applicationAllowsModelPreparation(applicationClosePhase)) return false;
      if (expectedWorkspace && !workspaceRestoreIsCurrent(expectedWorkspace)) return false;
      const loadSerial = ++modelLoadSerial;
      modelLoading = true;
      try {
        const loaded = await loadPolicyModelCandidate(candidate.profileId, candidate.modelPath);
        if (
          !componentMounted ||
          !applicationAllowsModelPreparation(applicationClosePhase) ||
          loadSerial !== modelLoadSerial ||
          (expectedWorkspace && !workspaceRestoreIsCurrent(expectedWorkspace))
        ) return false;
        if (!isVerifiedPolicyWriter(loaded, candidate.profileId)) {
          await refreshModels(expectedWorkspace);
          continue;
        }
        return await installLoadedModel(loaded, true, expectedWorkspace);
      } catch (error) {
        if (
          !componentMounted ||
          !applicationAllowsModelPreparation(applicationClosePhase) ||
          loadSerial !== modelLoadSerial ||
          (expectedWorkspace && !workspaceRestoreIsCurrent(expectedWorkspace))
        ) return false;
        const failure = normalizeFailure(error);
        if (candidate.remembered && rememberedWriterPathIsInvalid(failure.code)) {
          forgetLastLocalModelPath(candidate.modelPath);
        }
        await refreshModels(expectedWorkspace);
      } finally {
        if (loadSerial === modelLoadSerial) {
          modelLoading = false;
          wakePreferredWriterEnsure();
        }
      }
    }
    return false;
  }

  function expectedPolicyWriterName(): string {
    switch (buildModelPolicy?.name) {
      case 'writer-gemma4-base-v1':
      case 'writer-gemma4-base-v2':
        return 'the tested Gemma 4 base writing model';
      case 'none-v1':
      case undefined:
        return 'the tested writing model for this version of Loom';
    }
  }

  async function activatePolicyWriter(
    selected: ModelCapabilitySummary,
    captured: WorkspaceRestoreCapture
  ): Promise<boolean> {
    if (modelLoading || modelUnloading || !workspaceRestoreIsCurrent(captured)) return false;
    const policyCandidate = selected.policy_candidate;
    if (!policyCandidate) {
      modelSetupError = `This file is not ${expectedPolicyWriterName()}. It cannot power suggestions in this build.`;
      announce('That model is not compatible with suggestions in this version of Loom');
      return false;
    }

    models = [selected, ...models.filter((model) => model.model_path !== selected.model_path)];
    selectedModelPath = selected.model_path;
    const loadSerial = ++modelLoadSerial;
    modelLoading = true;
    modelSetupError = '';
    announce('Verifying the writing model');
    try {
      const loaded = await loadPolicyModelCandidate(
        policyCandidate.profile_id,
        selected.model_path
      );
      if (
        !componentMounted ||
        !applicationAllowsModelPreparation(applicationClosePhase) ||
        loadSerial !== modelLoadSerial ||
        !workspaceRestoreIsCurrent(captured)
      ) return false;
      if (!isVerifiedPolicyWriter(loaded, policyCandidate.profile_id)) {
        throw new Error('Native verification did not return the exact writer capabilities required by this build.');
      }
      if (!(await installLoadedModel(loaded, false, captured))) {
        throw new Error('The verified writer could not be attached to the current writing session.');
      }
      clearFailure();
      modelSetupError = '';
      announce(`${loaded.display_name} is ready to suggest writing`);
      if (modelManagerOpen) closeModelManager(true);
      return true;
    } catch (error) {
      if (
        componentMounted &&
        applicationAllowsModelPreparation(applicationClosePhase) &&
        workspaceRestoreIsCurrent(captured) &&
        loadSerial === modelLoadSerial
      ) {
        const failure = normalizeFailure(error);
        modelSetupError = `Loom could not verify this as ${expectedPolicyWriterName()}. ${failure.message}`;
        announce('That model did not pass writing-model verification');
        await refreshModels(captured);
      }
      return false;
    } finally {
      if (loadSerial === modelLoadSerial) {
        modelLoading = false;
        wakePreferredWriterEnsure();
      }
    }
  }

  async function choosePolicyWriterModel(): Promise<void> {
    if (modelChoosing || modelLoading || modelUnloading) return;
    const captured = currentWorkspaceCapture();
    if (!captured || !buildModelPolicy || buildModelPolicy.name === 'none-v1') {
      modelSetupError = 'This build does not define a verified local writing model.';
      announce('Suggestions remain off because no verified writing model is configured');
      return;
    }

    modelChoosing = true;
    modelSetupError = '';
    announce('Locate the supported writing model on this computer');
    try {
      const selected = await chooseModel();
      if (!selected) {
        announce('Writing model choice cancelled');
        return;
      }
      if (!workspaceRestoreIsCurrent(captured)) return;
      await activatePolicyWriter(selected, captured);
    } catch (error) {
      if (componentMounted && workspaceRestoreIsCurrent(captured)) {
        const failure = normalizeFailure(error);
        modelSetupError = `Loom could not inspect that file. ${failure.message}`;
        announce('The selected model file could not be inspected');
      }
    } finally {
      modelChoosing = false;
    }
  }

  async function useDiscoveredPolicyWriter(model: ModelCapabilitySummary): Promise<void> {
    const captured = currentWorkspaceCapture();
    if (!captured) return;
    await activatePolicyWriter(model, captured);
  }

  async function unloadCurrentModel(): Promise<void> {
    if (!models.some((model) => model.loaded) || modelUnloading) return;
    modelUnloading = true;
    clearFailure();
    announce('Releasing the local writer model');
    try {
      const outcome = await unloadModel();
      await refreshModels();
      announce(outcome.model_id
        ? 'Local writer model released; editing remains fully available'
        : 'No local writer model was resident');
    } catch (error) {
      recordFailure(error);
      announce('The local writer model could not be released safely');
      await refreshModels();
    } finally {
      modelUnloading = false;
      wakePreferredWriterEnsure();
    }
  }

  function modelDownloadPhaseLabel(phase: ModelDownloadPhase | null): string {
    switch (phase) {
      case 'inspecting_existing': return 'Checking local files';
      case 'hashing_partial': return 'Verifying resumable data';
      case 'downloading': return 'Downloading';
      case 'verifying': return 'Verifying SHA-256 and GGUF';
      case 'installing': return 'Installing privately';
      case 'complete': return 'Verified';
      case null: return 'Queued';
    }
  }

  function modelDownloadStatusLabel(download: ModelDownloadSnapshot): string {
    switch (download.status.status) {
      case 'queued': return 'Queued';
      case 'running': return modelDownloadPhaseLabel(download.phase);
      case 'completed': return download.status.disposition === 'reused_existing'
        ? 'Existing file verified'
        : 'Download verified';
      case 'cancelled': return 'Cancelled';
      case 'failed': return download.status.retryable ? 'Interrupted · retryable' : 'Failed';
    }
  }

  function modelCapabilityMode(model: ModelCapabilitySummary): string {
    if (!model.loaded) return 'Capabilities inspected when loaded';
    if (model.completion && model.chat) return 'Raw completion and chat';
    if (model.completion) return 'Raw completion';
    if (model.chat) return 'Chat only';
    return 'No supported text prompt mode';
  }

  function modelMediaLabel(model: ModelCapabilitySummary): string {
    if (!model.loaded) return 'Media not inspected';
    if (model.media_kinds.length === 0) return 'Text only';
    return model.media_kinds.join(' + ');
  }

  async function reattachNativeProject(): Promise<boolean> {
    try {
      const current = await currentProjectSession();
      const restoreSerial = workspaceRestoreSerial;
      if (!componentMounted) return false;
      project = current;
      if (!(await finishOpeningProject(current, restoreSerial))) return false;
      wakePreferredWriterEnsure();
      announce(`Reattached ${current.title}`);
      return true;
    } catch {
      return false;
    }
  }

  async function openInitialProject(restoreSerial: number): Promise<WorkspaceRestoreCapture | null> {
    let current: ProjectSnapshot | null = null;
    try {
      current = await currentProjectSession();
    } catch {
      // No live native session is the normal first-launch path.
    }
    if (current) {
      if (!componentMounted || restoreSerial !== workspaceRestoreSerial) return null;
      project = current;
      if (!(await finishOpeningProject(current, restoreSerial))) return null;
      if (
        !componentMounted ||
        restoreSerial !== workspaceRestoreSerial ||
        project?.project_id !== current.project_id ||
        project.session_id !== current.session_id
      ) return null;
      announce(`Reattached ${current.title}`);
      return {
        restoreSerial,
        projectId: current.project_id,
        sessionId: current.session_id
      };
    }
    try {
      const opened = await openDefaultProject();
      if (!componentMounted || restoreSerial !== workspaceRestoreSerial) return null;
      project = opened;
      if (!(await finishOpeningProject(opened, restoreSerial))) return null;
      if (
        !componentMounted ||
        restoreSerial !== workspaceRestoreSerial ||
        project?.project_id !== opened.project_id ||
        project.session_id !== opened.session_id
      ) return null;
      announce('Ready to write');
      return {
        restoreSerial,
        projectId: opened.project_id,
        sessionId: opened.session_id
      };
    } catch (error) {
      if (componentMounted && restoreSerial === workspaceRestoreSerial) recordFailure(error);
      return null;
    }
  }

  function waitForWritingSurfacePaint(): Promise<void> {
    return new Promise((resolve) => {
      let settled = false;
      const finish = () => {
        if (settled) return;
        settled = true;
        resolve();
      };
      window.requestAnimationFrame(finish);
      window.setTimeout(finish, 100);
    });
  }

  function focusCurrentWritingSurfaceAtEnd(): boolean {
    return mode === 'source'
      ? sourceEditor?.focusAtDocumentEnd() ?? false
      : visualEditor?.focusAtDocumentEnd() ?? false;
  }

  async function restoreDesktopWorkspace(): Promise<void> {
    const restoreSerial = ++workspaceRestoreSerial;
    await restoreBeforeBackgroundWork({
      restore: () => openInitialProject(restoreSerial),
      present: async () => {
        await tick();
        await waitForWritingSurfacePaint();
        focusCurrentWritingSurfaceAtEnd();
      },
      isCurrent: workspaceRestoreIsCurrent,
      background: async (captured) => {
        await recoverModelDownloads();
        if (!workspaceRestoreIsCurrent(captured)) return;
        if (!shouldDiscoverModelsOnStartup(suggestionsEnabled)) return;
        requestPreferredWriterEnsure(captured);
      }
    });
  }

  async function doOpenProject(): Promise<void> {
    const restoreSerial = ++workspaceRestoreSerial;
    modelRefreshSerial += 1;
    opening = true;
    clearFailure();
    try {
      const opened = await chooseAndOpenProject();
      if (!componentMounted || restoreSerial !== workspaceRestoreSerial) return;
      project = opened;
      if (await finishOpeningProject(opened, restoreSerial)) {
        wakePreferredWriterEnsure();
        announce(`Opened ${opened.title}`);
      }
    } catch (error) {
      if (!(await reattachNativeProject())) recordFailure(error);
    } finally {
      opening = false;
    }
  }

  async function finishOpeningProject(
    opened: ProjectSnapshot,
    restoreSerial: number
  ): Promise<boolean> {
    const captured: WorkspaceRestoreCapture = {
      restoreSerial,
      projectId: opened.project_id,
      sessionId: opened.session_id
    };
    if (!workspaceRestoreIsCurrent(captured)) return false;
    outlineOpen = false;
    clearPreferredWriterRequest();
    cancelSuggestionTimer();
    try {
      const identity = await getBuildModelPolicy();
      if (!workspaceRestoreIsCurrent(captured)) return false;
      buildModelPolicy = identity;
    } catch {
      if (!workspaceRestoreIsCurrent(captured)) return false;
      buildModelPolicy = null;
      announce('Suggestions are off because this build could not verify its local writer policy');
    }
    const storedSuggestionsPreference = loadSuggestionPreference(captured.projectId);
    suggestionsEnabled = false;
    try {
      const policy = await runCurrentWorkspaceStep({
        capture: captured,
        isCurrent: workspaceRestoreIsCurrent,
        run: () => setSuggestionsPolicy(
          captured.projectId,
          captured.sessionId,
          storedSuggestionsPreference
        )
      });
      if (policy.status === 'stale') return false;
      suggestionsEnabled = storedSuggestionsPreference;
    } catch (error) {
      if (!workspaceRestoreIsCurrent(captured)) return false;
      suggestionsEnabled = false;
      recordFailure(error);
      announce('Suggestions remain off because the project gate could not be restored');
    }
    dismissedCandidateIds = [];
    if (opened.pending_recovery > 0) {
      const recovered = await runCurrentWorkspaceStep({
        capture: captured,
        isCurrent: workspaceRestoreIsCurrent,
        run: () => recoverProject(captured.projectId, captured.sessionId)
      });
      if (recovered.status === 'stale') return false;
      const report = recovered.value;
      if (report.conflicts.length > 0) {
        if (!workspaceRestoreIsCurrent(captured) || !project) return false;
        project = { ...project, pending_recovery: report.conflicts.length };
        document = null;
        recordLocalFailure('recovery_conflict', `Recovery stopped at ${report.conflicts.length} externally changed file${report.conflicts.length === 1 ? '' : 's'}: ${report.conflicts.join(', ')}`);
        announce('Recovery requires reconciliation before editing');
        return true;
      }
      announce(`Recovered ${report.recovered} interrupted save${report.recovered === 1 ? '' : 's'}`);
      if (!workspaceRestoreIsCurrent(captured) || !project) return false;
      project = { ...project, pending_recovery: 0 };
    }
    if (!workspaceRestoreIsCurrent(captured) || !project) return false;
    const first = project.documents[0];
    if (first) {
      await selectDocument(first);
      if (!workspaceRestoreIsCurrent(captured)) return false;
      await tick();
      if (!workspaceRestoreIsCurrent(captured)) return false;
      if (suggestionsEnabled && currentModel && document) {
        scheduleAutomaticSuggestions(editVersion);
      }
    } else {
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
      branches = [];
      promotionArmedCandidateId = null;
      resetLiveGenerationView();
      clearReconciliationState();
      saveState = 'clean';
      saveMessage = 'Project is ready';
    }
    if (storedSuggestionsPreference && suggestionsEnabled && !currentModel) {
      queuePreferredWriterRequest(captured);
    }
    return true;
  }

  async function selectDocument(
    summary: DocumentSummary,
    focusWritingSurface = false
  ): Promise<void> {
    if (transition !== 'idle' || !project) return;
    const requestedScope: ProjectRestoreScope = {
      projectId: project.project_id,
      sessionId: project.session_id,
      restoreSerial: workspaceRestoreSerial
    };
    if (compositionActive) {
      announce('Finish composing text before changing documents');
      return;
    }
    if (!flushEditors()) return;
    cancelSuggestionTimer();
    dismissedCandidateIds = [];
    transition = 'navigation';
    announce('Opening document; editing is briefly locked');
    const requestSerial = ++navigationSerial;
    if (!(await flushDraftJournal())) {
      if (
        requestSerial === navigationSerial &&
        projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, requestedScope)
      ) transition = 'idle';
      return;
    }
    if (!projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, requestedScope)) return;
    if (!(await flushCurrentDocument())) {
      if (
        requestSerial === navigationSerial &&
        projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, requestedScope)
      ) transition = 'idle';
      return;
    }
    if (!projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, requestedScope)) return;
    if (branchRefreshTimer !== undefined) {
      window.clearTimeout(branchRefreshTimer);
      branchRefreshTimer = undefined;
    }
    branches = [];
    promotionArmedCandidateId = null;
    resetLiveGenerationView();
    const source = {
      ...requestedScope,
      documentEpoch,
      editVersion,
      documentId: document?.summary.document_id ?? null
    };
    clearFailure();
    try {
      if (summary.externally_modified) {
        const preview = await requestReconciliationPreview(summary, null, source);
        if (
          requestSerial !== navigationSerial ||
          !navigationScopeIsCurrent(
            project,
            document,
            documentEpoch,
            editVersion,
            workspaceRestoreSerial,
            source
          )
        ) {
          return;
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
        source.projectId,
        source.sessionId,
        summary.document_id,
        summary.relative_path
      );
      if (
        requestSerial !== navigationSerial ||
        !navigationScopeIsCurrent(
          project,
          document,
          documentEpoch,
          editVersion,
          workspaceRestoreSerial,
          source
        )
      ) return;
      if (opened.summary.document_id !== summary.document_id) {
        throw new Error('The desktop returned a different document identity.');
      }
      if (summary.active_blob_id && opened.visible_blob_id !== summary.active_blob_id) {
        throw new Error('The desktop returned document bytes from a different active revision.');
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
        saveMessage = 'Recovered an unsaved local draft';
        scheduleSave();
        announce(`Recovered a local draft for ${summary.title}`);
      } else {
        saveState = 'clean';
        saveMessage = 'All changes saved';
        announce(`Opened ${summary.title}`);
      }
      mode = summary.kind === 'prose' && canRoundTripMarkdownExactly(effectiveText)
        ? preferredProseMode
        : 'source';
      await refreshBranchesFor(
        source.projectId,
        source.sessionId,
        opened.summary.document_id,
        false
      );
    } catch (error) {
      if (!projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, source)) return;
      recordFailure(error);
      if (navigationScopeIsCurrent(
        project,
        document,
        documentEpoch,
        editVersion,
        workspaceRestoreSerial,
        source
      )) {
        await refreshCurrentBranches(false);
      }
    } finally {
      if (
        requestSerial === navigationSerial &&
        projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, source)
      ) {
        transition = 'idle';
        wakePreferredWriterEnsure();
        if (focusWritingSurface && document?.summary.document_id === summary.document_id) {
          await tick();
          focusCurrentWritingSurfaceAtEnd();
        }
      }
    }
  }

  function updateText(text: string): void {
    if (transition !== 'idle') return;
    const mutationWasInvalidated = visualMutationPending;
    visualMutationPending = false;
    if (text === documentText) return;
    documentText = text;
    editVersion += 1;
    if (!mutationWasInvalidated) suggestionIntentEpoch += 1;
    uncertainWeave = null;
    saveState = 'dirty';
    saveMessage = saveInFlight ? 'Saving earlier changes…' : 'Unsaved changes';
    promotionArmedCandidateId = null;
    dismissedCandidateIds = [];
    unpresentableVisualGhostPresentationKeys = [];
    if (activeBranchCount > 0) void cancelActiveBranches();
    scheduleDraftJournal();
    scheduleSave();
    scheduleAutomaticSuggestions(editVersion);
  }

  function invalidateVisualSuggestionImmediately(): void {
    if (transition !== 'idle' || visualMutationPending) return;
    visualMutationPending = true;
    suggestionIntentEpoch += 1;
    uncertainWeave = null;
    promotionArmedCandidateId = null;
    dismissedCandidateIds = [];
    unpresentableVisualGhostPresentationKeys = [];
    if (activeBranchCount > 0) void cancelActiveBranches();
  }

  function setSourceDocument(text: string, kind: DocumentKind): void {
    if (sourceProjectionTimer !== undefined) {
      window.clearTimeout(sourceProjectionTimer);
      sourceProjectionTimer = undefined;
    }
    sourceDirty = false;
    sourceSelectionStart = 0;
    sourceSelectionEnd = 0;
    visibleVisualGhostPresentationKey = '';
    visibleSourceGhostPresentationKey = '';
    visualSelectionByte = null;
    visualMutationPending = false;
    unpresentableVisualGhostPresentationKeys = [];
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

  function finishSourceComposition(textarea: HTMLTextAreaElement): void {
    sourceComposing = false;
    compositionActive = false;
    updateSourceSelection(textarea);
    updateFromSource(textarea.value);
    scheduleSourceProjection(0);
    announce('Text composition committed');
  }

  function updateFromSource(display: string): void {
    if (transition !== 'idle') return;
    sourceDisplayText = display;
    sourceDirty = true;
    if (!sourceComposing) scheduleSourceProjection();
  }

  function updateSourceSelection(textarea: HTMLTextAreaElement): void {
    sourceSelectionStart = textarea.selectionStart;
    sourceSelectionEnd = textarea.selectionEnd;
  }

  function updateVisualSelection(markdownByteOffset: number | null): void {
    visualSelectionByte = markdownByteOffset;
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

  function scheduleAutomaticSuggestions(
    targetEditVersion: number,
    delay = suggestionsIdleDelayMs
  ): void {
    armSuggestionSchedule({ kind: 'edit_pause', editVersion: targetEditVersion }, delay);
  }

  function scheduleAutocompleteRetry(ticket: AutocompleteRetryTicket): void {
    armSuggestionSchedule(
      { kind: 'exhausted_retry', ticket },
      suggestionsRetryDelayMs
    );
  }

  function armSuggestionSchedule(schedule: SuggestionSchedule, delay: number): void {
    if (suggestionsIdleTimer !== undefined) window.clearTimeout(suggestionsIdleTimer);
    suggestionsIdleTimer = undefined;
    scheduledSuggestion = null;
    if (
      !desktop ||
      !suggestionsEnabled ||
      !currentModel ||
      !project ||
      !document ||
      document.summary.kind === 'hybrid'
    ) return;
    scheduledSuggestion = schedule;
    suggestionsIdleTimer = window.setTimeout(() => {
      suggestionsIdleTimer = undefined;
      void tryStartAutomaticSuggestions(schedule);
    }, delay);
  }

  function rearmBoundedSuggestionSchedule(
    schedule: SuggestionSchedule,
    delay: number
  ): boolean {
    if (schedule.kind === 'edit_pause') {
      armSuggestionSchedule(schedule, delay);
      return true;
    }
    if (schedule.ticket.waitsRemaining <= 0) {
      scheduledSuggestion = null;
      return false;
    }
    armSuggestionSchedule({
      kind: 'exhausted_retry',
      ticket: {
        ...schedule.ticket,
        waitsRemaining: schedule.ticket.waitsRemaining - 1
      }
    }, delay);
    return true;
  }

  function retryTicketDisposition(
    ticket: AutocompleteRetryTicket
  ): AutocompleteDisposition | null {
    if (
      !project ||
      !document ||
      !currentModel ||
      !suggestionsEnabled ||
      project.project_id !== ticket.projectId ||
      project.session_id !== ticket.sessionId ||
      document.summary.document_id !== ticket.documentId ||
      document.summary.revision_id !== ticket.sourceRevisionId ||
      document.visible_blob_id !== ticket.visibleBlobId ||
      documentEpoch !== ticket.documentEpoch ||
      editVersion !== ticket.editVersion ||
      suggestionIntentEpoch !== ticket.intentEpoch ||
      mode !== ticket.mode ||
      currentModel.model_id !== ticket.modelId ||
      sourceGhostNewline !== ticket.sourceNewline ||
      !branchPromotionReady
    ) return null;
    const targetByte = mode === 'visual' ? visualGhostTargetByte : sourceGhostTargetByte;
    if (targetByte !== ticket.targetByte) return null;
    return autocompleteDisposition({
      active: true,
      branches: currentReadyBranches,
      verifiedBodyByRun: verifiedBranchBodyByRun,
      dismissedCandidateIds,
      unpresentablePresentationKeys: mode === 'visual'
        ? unpresentableVisualGhostPresentationKeys
        : [],
      targetByte,
      presentationCompatible: mode === 'source'
        ? (text) => sourceGhostPresentationCompatible(
          sourceDisplayText,
          text,
          ticket.sourceNewline
        )
        : undefined
    });
  }

  async function tryStartAutomaticSuggestions(schedule: SuggestionSchedule): Promise<void> {
    const targetEditVersion = schedule.kind === 'edit_pause'
      ? schedule.editVersion
      : schedule.ticket.editVersion;
    if (
      scheduledSuggestion !== schedule ||
      targetEditVersion !== editVersion ||
      !suggestionsEnabled ||
      !currentModel ||
      !project ||
      !document ||
      compositionActive ||
      transition !== 'idle'
    ) {
      scheduledSuggestion = null;
      return;
    }
    if (schedule.kind === 'exhausted_retry') {
      const disposition = retryTicketDisposition(schedule.ticket);
      if (!disposition) {
        scheduledSuggestion = null;
        return;
      }
      if (disposition.kind === 'available' || disposition.kind === 'inactive') {
        scheduledSuggestion = null;
        return;
      }
      if (
        disposition.kind === 'awaiting_candidates' ||
        disposition.kind === 'awaiting_hydration'
      ) {
        rearmBoundedSuggestionSchedule(schedule, 200);
        return;
      }
    }
    if (activeBranchCount > 0 || weaveStarting) {
      rearmBoundedSuggestionSchedule(schedule, 450);
      return;
    }
    if (!canStartAutomaticSuggestions) {
      if (
        sourceDirty ||
        saveInFlight ||
        saveState === 'dirty' ||
        saveState === 'saving'
      ) rearmBoundedSuggestionSchedule(schedule, 350);
      else scheduledSuggestion = null;
      return;
    }
    scheduledSuggestion = null;
    await startAutomaticWeave();
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
          saveMessage = 'Draft protected locally';
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
      restoreSerial: workspaceRestoreSerial,
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
        saveMessage = 'Save recorded · visible file needs retry';
        const projectionError = receipt.visible_projection?.status === 'pending_retry'
          ? receipt.visible_projection.error
          : 'The visible file could not be replaced.';
        lastFailure = {
          code: 'visible_projection_pending',
          message: projectionError,
          retryable: true
        };
        errorMessage = projectionError;
        announce('The save is durable, but editing is locked until the same command projects its visible file');
        return;
      }
      if (projectionDecision === 'reconcile') {
        const heldAppText = documentText;
        uncertainSave = captured;
        saveState = 'uncertain';
        saveMessage = 'Save recorded · external file held for review';
        try {
          await activateCheckpointProjectionConflict(captured, receipt, heldAppText);
        } catch (projectionError) {
          recordFailure(projectionError);
          // The semantic receipt is confirmed committed. Retain the exact
          // original checkpoint command even if refreshing the new active
          // identity or opening its reconciliation preview was refused.
          uncertainSave = captured;
          saveState = 'uncertain';
          saveMessage = 'Save recorded · retry external-file review';
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
      if (
        documentEpoch !== captured.documentEpoch ||
        !projectSessionIsCurrent(project, captured)
      ) return;
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
          }, captured.text, {
            projectId: captured.projectId,
            sessionId: captured.sessionId,
            restoreSerial: captured.restoreSerial
          });
          activateReconciliation(preview);
        } catch (previewError) {
          if (!projectSessionIsCurrent(project, captured)) return;
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

  function sourceGhostTargetByteFor(
    currentMode: EditorMode,
    editorAvailable: boolean,
    selectionStart: number,
    selectionEnd: number,
    displayText: string,
    currentDocument: OpenDocument | null,
    manuscriptText: string,
    codec: VerseEditorCodec | null
  ): number | null {
    if (
      currentMode !== 'source' ||
      !editorAvailable ||
      selectionStart !== selectionEnd ||
      !isExtendedGraphemeBoundary(displayText, selectionStart) ||
      !currentDocument
    ) return null;
    try {
      const displayPrefix = displayText.slice(0, selectionStart);
      if (
        currentDocument.summary.kind === 'verse' &&
        (!codec || !codec.editable)
      ) return null;
      const manuscriptPrefix = currentDocument.summary.kind === 'verse' && codec
        ? encodeVerseFromEditor(displayPrefix, codec)
        : displayPrefix;
      if (!manuscriptText.startsWith(manuscriptPrefix)) return null;
      return utf8ByteOffset(manuscriptPrefix, manuscriptPrefix.length);
    } catch {
      return null;
    }
  }

  function dismissInlineSuggestion(candidateId: string | null | undefined): void {
    if (!candidateId || dismissedCandidateIds.includes(candidateId)) return;
    dismissedCandidateIds = [...dismissedCandidateIds, candidateId];
    announce('Suggestion dismissed; it remains available under alternatives');
  }

  function rejectVisualGhostPresentation(
    candidateId: string,
    presentationKey: string,
    surfaceKey: string,
    anchorByteOffset: number
  ): void {
    if (
      mode !== 'visual' ||
      ghostSuggestion?.candidateId !== candidateId ||
      ghostSuggestion.presentationKey !== presentationKey ||
      ghostSuggestion.targetByte !== anchorByteOffset ||
      surfaceKey !== visualGhostSurfaceKey ||
      unpresentableVisualGhostPresentationKeys.includes(presentationKey)
    ) return;
    unpresentableVisualGhostPresentationKeys = [
      ...unpresentableVisualGhostPresentationKeys,
      presentationKey
    ].slice(-64);
  }

  async function acceptInlineSuggestion(branch: BranchCard): Promise<void> {
    if (!branch.candidate_id || !canPromoteBranch(branch)) return;
    promotionArmedCandidateId = branch.candidate_id;
    await confirmPromotion(branch);
  }

  function eligibleGhostForCurrentMode() {
    return mode === 'visual'
      ? ghostSuggestion
      : mode === 'source'
        ? sourceGhostSuggestion
        : null;
  }

  function acceptActiveGhost(candidateId: string, presentationKey: string): boolean {
    const eligible = eligibleGhostForCurrentMode();
    const branch = branches.find((candidate) => candidate.candidate_id === candidateId);
    if (
      !branch ||
      eligible?.candidateId !== candidateId ||
      eligible.presentationKey !== presentationKey ||
      !canPromoteBranch(branch)
    ) return false;
    // LoomEditor and SourceEditor invoke this callback only after synchronously
    // proving the exact rendered key, surface, caret, and viewport witness.
    // Rechecking an asynchronously reported duplicate here can race that
    // stronger witness and turn Tab into focus traversal.
    void acceptInlineSuggestion(branch);
    return true;
  }

  function dismissActiveGhost(candidateId: string, presentationKey: string): void {
    const eligible = eligibleGhostForCurrentMode();
    if (
      eligible?.candidateId !== candidateId ||
      eligible.presentationKey !== presentationKey
    ) return;
    dismissInlineSuggestion(candidateId);
  }

  async function openStrandReview(): Promise<void> {
    if (reviewableBranches.length === 0) return;
    reviewCandidateId = activeGhostSuggestion?.candidateId ??
      reviewableBranches[0].candidate_id;
    promotionArmedCandidateId = null;
    strandReviewOpen = true;
    await tick();
    if (!strandReviewDialog) return;
    if (!strandReviewDialog.open) strandReviewDialog.showModal();
    strandReviewDialog.querySelector<HTMLElement>('[data-review-close]')?.focus();
  }

  function closeStrandReview(): void {
    promotionArmedCandidateId = null;
    if (strandReviewDialog?.open) {
      strandReviewDialog.close();
    } else {
      strandReviewOpen = false;
    }
  }

  function moveStrandReview(offset: number): void {
    if (reviewableBranches.length < 2 || reviewBranchIndex < 0) return;
    const nextIndex = (
      reviewBranchIndex + offset + reviewableBranches.length
    ) % reviewableBranches.length;
    reviewCandidateId = reviewableBranches[nextIndex].candidate_id;
    promotionArmedCandidateId = null;
    announce(`Alternative ${nextIndex + 1} of ${reviewableBranches.length}`);
  }

  function handleGlobalKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape' && modelManagerOpen) {
      event.preventDefault();
      closeModelManager();
      return;
    }
    if (event.key === 'Escape' && projectMenu?.open) {
      event.preventDefault();
      closeProjectMenu();
      projectMenuTrigger?.focus();
      return;
    }
    if (event.key === 'Escape' && outlineOpen) {
      event.preventDefault();
      void setOutlineOpen(false);
      return;
    }
    const modifier = event.metaKey || event.ctrlKey;
    if (modifier && event.key.toLocaleLowerCase() === 's') {
      event.preventDefault();
      if (reconciliation) {
        announce(pendingReconciliationApply
          ? 'Retry the exact reconciliation command with the review button'
          : 'Review and save the external-file resolution');
        return;
      }
      if (compositionActive) {
        announce('Finish composing text before saving');
        return;
      }
      flushEditors();
      void saveNow();
    }
  }

  function handleGlobalPointerdown(event: PointerEvent): void {
    if (
      projectMenu?.open &&
      event.target instanceof Node &&
      !projectMenu.contains(event.target)
    ) closeProjectMenu();
  }

  function captureWeaveCursorByte(): number {
    if (!document) throw new Error('Open a manuscript before weaving.');
    if (mode !== 'source') {
      if (visualSelectionByte === null) {
        throw new Error('The visual caret does not map exactly to the saved Markdown bytes.');
      }
      return visualSelectionByte;
    }
    if (!sourceTextarea) throw new Error('The source editor is not available.');
    const selectionStart = sourceTextarea.selectionStart;
    const displayPrefix = sourceTextarea.value.slice(0, selectionStart);
    if (document.summary.kind === 'verse' && (!verseCodec || !verseCodec.editable)) {
      throw new Error('This poem does not expose a lossless source-caret boundary.');
    }
    const manuscriptPrefix = document.summary.kind === 'verse' && verseCodec
      ? encodeVerseFromEditor(displayPrefix, verseCodec)
      : displayPrefix;
    if (!documentText.startsWith(manuscriptPrefix)) {
      throw new Error('The source caret no longer matches the saved manuscript bytes.');
    }
    return utf8ByteOffset(manuscriptPrefix, manuscriptPrefix.length);
  }

  function installWeaveSnapshot(started: WeaveStarted, captured: WeaveCapture): boolean {
    if (
      started.command_id !== captured.commandId ||
      started.request_id !== `weave-${captured.commandId}` ||
      started.project_id !== captured.projectId ||
      started.session_id !== captured.sessionId ||
      started.document_id !== captured.documentId ||
      started.source_revision_id !== captured.sourceRevisionId ||
      !started.exact_prompt_blob_id ||
      started.branches.length !== 3
    ) {
      throw new Error('The desktop returned a branch family for different source identities.');
    }
    validateBranchSnapshots(started.branches, captured.documentId);
    if (started.branches.some((branch) =>
      branch.source_revision_id !== captured.sourceRevisionId ||
      branch.target_start_byte !== captured.cursorByte ||
      branch.target_end_byte !== captured.cursorByte ||
      branch.model_id !== captured.modelId
    )) {
      throw new Error('The desktop returned a branch outside the requested manuscript boundary.');
    }
    if (!weaveCaptureStillCurrent(captured)) return false;
    const runIds = new Set(started.branches.map((branch) => branch.run_id));
    branches = [
      ...started.branches.map((branch) => applyLiveBranchState(branch, false)),
      ...branches.filter((branch) => !runIds.has(branch.run_id))
    ];
    // A lost-reply replay may already be terminal. Only the authoritative body
    // endpoint can certify immutable blob identity for presentation.
    scheduleBranchRefresh();
    scheduleActiveBranchPoll();
    return true;
  }

  function weaveCaptureStillCurrent(captured: WeaveCapture): boolean {
    return Boolean(
      project?.project_id === captured.projectId &&
      project.session_id === captured.sessionId &&
      document?.summary.document_id === captured.documentId &&
      document.summary.kind === captured.documentKind &&
      document.summary.revision_id === captured.sourceRevisionId &&
      document.visible_blob_id === captured.visibleBlobId &&
      documentEpoch === captured.epoch &&
      editVersion === captured.editVersion &&
      suggestionIntentEpoch === captured.intentEpoch &&
      currentModel?.model_id === captured.modelId &&
      suggestionsEnabled
    );
  }

  async function cancelDetachedWeave(started: WeaveStarted, captured: WeaveCapture): Promise<void> {
    if (
      project?.project_id !== captured.projectId ||
      project.session_id !== captured.sessionId
    ) return;
    await Promise.all(started.branches.filter(isBranchActive).map(async (branch) => {
      try {
        await cancelGeneration(
          captured.projectId,
          captured.sessionId,
          newUlid(),
          branch.run_id
        );
      } catch {
        // The run may already be terminal or the backend gate may have cancelled it.
      }
    }));
    scheduleActiveBranchPoll();
  }

  function scheduleStaleWeaveCleanup(captured: WeaveCapture, attempt = 0): void {
    if (attempt >= 8) return;
    const delay = Math.min(400 * (2 ** attempt), 4_000);
    const timer = window.setTimeout(async () => {
      staleWeaveCleanupTimers.delete(timer);
      if (
        project?.project_id !== captured.projectId ||
        project.session_id !== captured.sessionId ||
        document?.summary.document_id !== captured.documentId
      ) return;
      let retry = attempt < 2;
      try {
        const status = await getWeaveStatus(
          captured.projectId,
          captured.sessionId,
          captured.commandId
        );
        if (status) {
          await cancelDetachedWeave(status, captured);
          retry = status.branches.some(isBranchActive);
        } else {
          const refreshed = await refreshBranchesFor(
            captured.projectId,
            captured.sessionId,
            captured.documentId,
            false
          );
          if (refreshed) {
            const staleActive = branches.filter((branch) =>
              isBranchActive(branch) && branch.source_revision_id === captured.sourceRevisionId
            );
            await Promise.all(staleActive.map(async (branch) => {
              try {
                await cancelGeneration(
                  captured.projectId,
                  captured.sessionId,
                  newUlid(),
                  branch.run_id
                );
              } catch {
                // Retry through the authoritative status/page path below.
              }
            }));
            retry ||= staleActive.length > 0;
          } else {
            retry = true;
          }
        }
      } catch {
        retry = true;
      }
      if (retry) scheduleStaleWeaveCleanup(captured, attempt + 1);
    }, delay);
    staleWeaveCleanupTimers.add(timer);
  }

  async function startAutomaticWeave(): Promise<void> {
    if (weaveStarting || !project || !document || !currentModel) return;
    const startingEditVersion = editVersion;
    if (compositionActive || !flushEditors() || editVersion !== startingEditVersion) return;
    if (!canStartAutomaticSuggestions || uncertainWeave) return;

    let cursorByte: number;
    try {
      cursorByte = captureWeaveCursorByte();
    } catch (error) {
      recordFailure(error);
      return;
    }
    if (cursorByte === 0) return;
    const sourceRevisionId = document.summary.revision_id;
    if (!sourceRevisionId) return;
    const captured: WeaveCapture = {
      commandId: newUlid(),
      epoch: documentEpoch,
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id,
      relativePath: document.summary.relative_path,
      documentKind: document.summary.kind,
      sourceRevisionId,
      visibleBlobId: document.visible_blob_id,
      cursorByte,
      editVersion,
      intentEpoch: suggestionIntentEpoch,
      modelId: currentModel.model_id
    };
    weaveStarting = true;
    clearFailure();
    try {
      const started = await startWeave({
        projectId: captured.projectId,
        sessionId: captured.sessionId,
        commandId: captured.commandId,
        documentId: captured.documentId,
        relativePath: captured.relativePath,
        sourceRevisionId: captured.sourceRevisionId,
        expectedVisibleBlobId: captured.visibleBlobId,
        cursorByte: captured.cursorByte,
        policy: { kind: 'automatic_v2' }
      });
      if (installWeaveSnapshot(started, captured)) {
        uncertainWeave = null;
        announce(started.branches.some(isBranchActive)
          ? 'Suggestions are growing privately'
          : 'Stored strands were recovered');
      } else {
        await cancelDetachedWeave(started, captured);
      }
    } catch (error) {
      if (
        project?.project_id === captured.projectId &&
        project.session_id === captured.sessionId &&
        document?.summary.document_id === captured.documentId
      ) {
        const captureIsCurrent = weaveCaptureStillCurrent(captured);
        if (captureIsCurrent) {
          recordFailure(error);
          uncertainWeave = captured;
        }
        try {
          const status = await getWeaveStatus(
            captured.projectId,
            captured.sessionId,
            captured.commandId
          );
          if (status) {
            if (captureIsCurrent && installWeaveSnapshot(status, captured)) {
              uncertainWeave = null;
              clearFailure();
              announce('The durable Weave result was recovered');
            } else {
              await cancelDetachedWeave(status, captured);
            }
          } else if (captureIsCurrent) {
            uncertainWeave = null;
            announce('No Weave was committed; the request can be started again');
          }
        } catch {
          if (captureIsCurrent) {
            await refreshBranchesFor(
              captured.projectId,
              captured.sessionId,
              captured.documentId,
              false
            );
            announce('Suggestion result remains uncertain; editing stays available');
          }
          scheduleStaleWeaveCleanup(captured);
        }
      }
    } finally {
      weaveStarting = false;
    }
  }

  function isBranchActive(branch: BranchCard): boolean {
    return branch.status === 'queued' || branch.status === 'generating';
  }

  async function cancelBranch(branch: BranchCard): Promise<void> {
    if (!project || !isBranchActive(branch) || cancellingRunIds.includes(branch.run_id)) return;
    const captured = {
      projectId: project.project_id,
      sessionId: project.session_id,
      commandId: cancellationCommandByRun[branch.run_id] ?? newUlid(),
      runId: branch.run_id
    };
    cancellationCommandByRun = {
      ...cancellationCommandByRun,
      [branch.run_id]: captured.commandId
    };
    cancellingRunIds = [...cancellingRunIds, branch.run_id];
    try {
      const receipt = await cancelGeneration(
        captured.projectId,
        captured.sessionId,
        captured.commandId,
        captured.runId
      );
      if (
        receipt.command_id !== captured.commandId ||
        receipt.project_id !== captured.projectId
      ) {
        throw new Error('The desktop returned a cancellation receipt for another command.');
      }
      announce('Cancellation requested for one private strand');
    } catch (error) {
      if (
        project?.project_id === captured.projectId &&
        project.session_id === captured.sessionId
      ) {
        recordFailure(error);
        announce('Cancellation was not confirmed; stored strand state will be checked');
      }
    } finally {
      cancellingRunIds = cancellingRunIds.filter((runId) => runId !== captured.runId);
      scheduleBranchRefresh();
    }
  }

  async function cancelActiveBranches(): Promise<void> {
    const active = branches.filter(isBranchActive);
    if (active.length === 0) return;
    await Promise.all(active.map((branch) => cancelBranch(branch)));
  }

  function canPromoteBranch(branch: BranchCard): boolean {
    return Boolean(
      branchPromotionReady &&
      document &&
      branch.status === 'ready' &&
      branch.candidate_id &&
      branch.selection !== 'promote' &&
      branch.selection !== 'reject' &&
      branch.source_revision_id === document.summary.revision_id
    );
  }

  async function confirmPromotion(branch: BranchCard): Promise<void> {
    if (
      !project ||
      !document ||
      !branch.candidate_id ||
      promotionArmedCandidateId !== branch.candidate_id ||
      !flushEditors() ||
      !canPromoteBranch(branch)
    ) return;
    const captured: PromotionCapture = {
      commandId: newUlid(),
      restoreSerial: workspaceRestoreSerial,
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id,
      relativePath: document.summary.relative_path,
      candidateId: branch.candidate_id,
      runId: branch.run_id,
      sourceRevisionId: branch.source_revision_id,
      visibleBlobId: document.visible_blob_id
    };
    let restoreWritingFocus = false;
    promotionInFlight = true;
    uncertainPromotion = captured;
    promotionArmedCandidateId = null;
    clearFailure();
    announce('Accepting the suggestion');
    try {
      const receipt = await promoteCandidate(
        captured.projectId,
        captured.sessionId,
        captured.commandId,
        captured.candidateId,
        captured.sourceRevisionId,
        captured.visibleBlobId
      );
      if (
        receipt.command_id !== captured.commandId ||
        receipt.project_id !== captured.projectId ||
        receipt.source_revision_id !== captured.sourceRevisionId ||
        !receipt.result_revision_id ||
        !receipt.result_blob_id ||
        documentProjectionDecision(receipt.visible_projection) !== 'applied'
      ) {
        throw new Error('The promotion receipt did not prove an applied manuscript revision.');
      }
      const outcome = await reloadPromotionResult(
        captured,
        receipt.result_revision_id,
        receipt.result_blob_id
      );
      if (outcome !== 'promoted' && outcome !== 'reconciliation') {
        throw new Error('The promoted revision was not visible in the project snapshot.');
      }
      uncertainPromotion = null;
      closeStrandReview();
      restoreWritingFocus = true;
      announce(outcome === 'reconciliation'
        ? 'The suggestion is saved; an external file change now needs review'
        : 'Suggestion accepted');
    } catch (error) {
      recordFailure(error);
      try {
        const outcome = await reloadPromotionResult(captured);
        uncertainPromotion = null;
        clearFailure();
        switch (outcome) {
          case 'promoted':
            closeStrandReview();
            restoreWritingFocus = true;
            announce('Suggestion accepted');
            break;
          case 'source_changed':
            closeStrandReview();
            announce('The manuscript changed independently; Loom reopened it without attributing the change to this strand');
            break;
          case 'reconciliation':
            closeStrandReview();
            announce('An external manuscript change needs review before promotion can be attributed');
            break;
          case 'unchanged':
            await refreshCurrentBranches(false);
            announce('The suggestion was not accepted; the manuscript is unchanged');
            break;
        }
      } catch (refreshError) {
        recordFailure(refreshError);
        announce('Promotion result is uncertain; editing remains locked until checked');
      }
    } finally {
      promotionInFlight = false;
      if (restoreWritingFocus) {
        await tick();
        await waitForWritingSurfacePaint();
        focusCurrentWritingSurfaceAtEnd();
        // The authoritative revision swap remounts the editor. A second
        // post-paint focus prevents that remount's completion from stealing
        // the caret after the first focus succeeds.
        await waitForWritingSurfacePaint();
        focusCurrentWritingSurfaceAtEnd();
      }
    }
  }

  async function resolveUncertainPromotion(): Promise<void> {
    if (!uncertainPromotion || promotionInFlight) return;
    const captured = uncertainPromotion;
    promotionInFlight = true;
    try {
      const outcome = await reloadPromotionResult(captured);
      uncertainPromotion = null;
      clearFailure();
      if (outcome === 'unchanged') await refreshCurrentBranches(false);
      switch (outcome) {
        case 'promoted':
          announce('Promotion confirmed; authoritative manuscript reopened');
          break;
        case 'source_changed':
          announce('An independent revision was reopened; it was not attributed to this strand');
          break;
        case 'reconciliation':
          announce('External manuscript bytes need review before promotion can be attributed');
          break;
        case 'unchanged':
          announce('Promotion did not commit; editing unlocked');
          break;
      }
    } catch (error) {
      recordFailure(error);
      announce('Promotion result is still uncertain; manuscript editing stays locked');
    } finally {
      promotionInFlight = false;
    }
  }

  async function reloadPromotionResult(
    captured: PromotionCapture,
    expectedRevisionId?: string,
    expectedBlobId?: string
  ): Promise<PromotionReloadOutcome> {
    const refreshed = await currentProjectSession();
    if (
      refreshed.project_id !== captured.projectId ||
      refreshed.session_id !== captured.sessionId
    ) {
      throw new Error('The refreshed project does not match the promotion session.');
    }
    const target = refreshed.documents.find(
      (candidate) => candidate.document_id === captured.documentId
    );
    if (!target || target.relative_path !== captured.relativePath) {
      throw new Error('The promoted document disappeared from the project outline.');
    }
    project = refreshed;
    if (expectedRevisionId && (
      target.revision_id !== expectedRevisionId ||
      target.active_blob_id !== expectedBlobId
    )) {
      throw new Error('The project exposed a different revision than the promoted receipt.');
    }
    if (target.revision_id === captured.sourceRevisionId) {
      if (expectedRevisionId) {
        throw new Error('The project did not expose the revision proven by the promotion receipt.');
      }
      if (target.externally_modified) {
        const preview = await requestReconciliationPreview(target, null, {
          projectId: captured.projectId,
          sessionId: captured.sessionId,
          restoreSerial: captured.restoreSerial
        });
        activateReconciliation(preview);
        return 'reconciliation';
      }
      return 'unchanged';
    }
    if (!target.revision_id || !target.active_blob_id) {
      throw new Error('The project exposed an incomplete active manuscript identity.');
    }
    if (target.externally_modified) {
      const preview = await requestReconciliationPreview(target, null, {
        projectId: captured.projectId,
        sessionId: captured.sessionId,
        restoreSerial: captured.restoreSerial
      });
      activateReconciliation(preview);
      return 'reconciliation';
    }

    let outcome: PromotionReloadOutcome = 'promoted';
    if (!expectedRevisionId) {
      const candidate = await getBranch(
        captured.projectId,
        captured.sessionId,
        captured.documentId,
        captured.runId
      );
      if (candidate) validateBranchSnapshots([candidate], captured.documentId);
      outcome = candidate?.candidate_id === captured.candidateId &&
        candidate.source_revision_id === captured.sourceRevisionId &&
        candidate.selection === 'promote'
        ? 'promoted'
        : 'source_changed';
    }

    // Once a new semantic revision is known to exist, detach the old editor
    // before reading it. A failed reopen can never unlock stale source bytes.
    detachDocumentForReconciliation();
    clearReconciliationState();
    const opened = await openDocument(
      captured.projectId,
      captured.sessionId,
      captured.documentId,
      captured.relativePath
    );
    if (
      opened.summary.document_id !== captured.documentId ||
      opened.summary.revision_id !== target.revision_id ||
      opened.summary.active_blob_id !== target.active_blob_id ||
      opened.visible_blob_id !== target.active_blob_id
    ) {
      throw new Error('The reopened manuscript does not match the promoted project revision.');
    }
    installPromotedDocument(refreshed, opened, outcome === 'promoted');
    await refreshBranchesFor(
      captured.projectId,
      captured.sessionId,
      captured.documentId,
      false
    );
    return outcome;
  }

  function installPromotedDocument(
    refreshed: ProjectSnapshot,
    opened: OpenDocument,
    promotionConfirmed: boolean
  ): void {
    project = refreshed;
    document = { ...opened, text: opened.text };
    documentText = opened.text;
    setSourceDocument(opened.text, opened.summary.kind);
    editVersion = 0;
    savedVersion = 0;
    draftVersion = opened.transient_draft?.version ?? '0';
    draftSavedEditVersion = 0;
    staleDraft = opened.transient_draft;
    staleDraftRestoring = false;
    staleDraftDiscardArmed = false;
    uncertainDraft = null;
    uncertainSave = null;
    branches = [];
    resetLiveGenerationView();
    mode = opened.summary.kind === 'prose' && canRoundTripMarkdownExactly(opened.text)
      ? preferredProseMode
      : 'source';
    if (opened.transient_draft) {
      saveState = 'error';
      saveMessage = promotionConfirmed
        ? 'A preserved draft needs review after promotion'
        : 'A preserved draft needs review after the revision changed';
      recordLocalFailure(
        promotionConfirmed ? 'promotion_draft_requires_review' : 'revision_draft_requires_review',
        promotionConfirmed
          ? 'The promoted manuscript reopened, but a separate crash-safe draft remains preserved for explicit review.'
          : 'The authoritative manuscript reopened, but a separate crash-safe draft remains preserved for explicit review.'
      );
    } else {
      saveState = 'clean';
      saveMessage = promotionConfirmed ? 'Suggestion accepted' : 'Authoritative revision reopened';
      clearFailure();
    }
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
      restoreSerial: workspaceRestoreSerial,
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
    const scope: ProjectRestoreScope = {
      projectId: project.project_id,
      sessionId: project.session_id,
      restoreSerial: workspaceRestoreSerial
    };
    const previous = reconciliation;
    const appText = reconciliationResolution;
    reconciliationApplying = true;
    clearFailure();
    try {
      const refreshed = await currentProjectSession();
      if (
        !projectSessionIsCurrent(project, scope) ||
        refreshed.project_id !== scope.projectId ||
        refreshed.session_id !== scope.sessionId
      ) return;
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
      const preview = await requestReconciliationPreview(target, appText, scope);
      activateReconciliation(preview);
      announce('External comparison refreshed against the newest visible file');
    } catch (error) {
      if (projectSessionIsCurrent(project, scope)) recordFailure(error);
    } finally {
      if (projectSessionIsCurrent(project, scope)) reconciliationApplying = false;
    }
  }

  async function setMode(next: EditorMode): Promise<void> {
    if (compositionActive) {
      announce('Finish composing text before changing editor modes');
      return;
    }
    if (next === 'visual' && !canUseVisual) return;
    flushEditors();
    if (next === 'source' && document) setSourceDocument(documentText, document.summary.kind);
    if (document?.summary.kind === 'prose' && canRoundTripMarkdownExactly(documentText)) {
      preferredProseMode = next;
    }
    mode = next;
    announce(`${next} editor mode`);
  }

  function announce(message: string): void {
    liveRegion = '';
    window.setTimeout(() => (liveRegion = message), 0);
  }

  async function closeProject(): Promise<ProjectCloseOutcome> {
    if (closeInFlight) return closeInFlight;
    const operation = performCloseProject();
    closeInFlight = operation;
    try {
      return await operation;
    } finally {
      if (closeInFlight === operation) closeInFlight = null;
    }
  }

  async function resumeProjectAfterDefinitiveClose(
    closing: Pick<ProjectSnapshot, 'project_id' | 'session_id'>
  ): Promise<ProjectCloseOutcome> {
    const sameSession = Boolean(
      project?.project_id === closing.project_id &&
      project.session_id === closing.session_id
    );
    const agency = pendingCloseAgency;
    const restoreSuggestions = agency?.suggestionsEnabled ?? suggestionsEnabled;
    if (agency && sameSession) {
      try {
        await restoreProjectCloseAgency(agency, {
          setFocusMode: (enabled) => setFocusMode(
            closing.project_id,
            closing.session_id,
            enabled
          ),
          setSuggestionsEnabled: (enabled) => setSuggestionsPolicy(
            closing.project_id,
            closing.session_id,
            enabled
          )
        });
      } catch (error) {
        pendingCloseMayHaveCommitted = false;
        transition = 'closing';
        recordFailure(error);
        saveMessage = 'Close recovery is still settling';
        announce('The project remains safely locked until its writing policy can be restored');
        return { status: 'quiesced' };
      }
    }

    if (sameSession) suggestionsEnabled = restoreSuggestions;
    pendingCloseCommandId = null;
    pendingCloseMayHaveCommitted = false;
    pendingCloseAgency = null;
    transition = 'idle';
    scheduleActiveBranchPoll();
    if (
      sameSession &&
      restoreSuggestions &&
      applicationAllowsModelPreparation(applicationClosePhase)
    ) {
      requestPreferredWriterForCurrentWorkspace();
      if (currentModel && document) scheduleAutomaticSuggestions(editVersion);
    }
    return { status: 'resume' };
  }

  async function performCloseProject(): Promise<ProjectCloseOutcome> {
    if (!project) return { status: 'closed' };
    const retryingPreparedClose = transition === 'closing' && pendingCloseCommandId !== null;
    if (compositionActive && !retryingPreparedClose) {
      announce('Finish composing text before closing the project');
      return { status: 'resume' };
    }
    if (!retryingPreparedClose) {
      if (!flushEditors()) return { status: 'resume' };
      transition = 'closing';
      announce('Closing project; editing is briefly locked');
      stopBranchPolling();
      if (!(await flushCurrentDocument())) {
        transition = 'idle';
        scheduleActiveBranchPoll();
        return { status: 'resume' };
      }
    } else {
      stopBranchPolling();
    }
    const closing = project;
    const closingEpoch = documentEpoch;
    const closingVersion = editVersion;
    pendingCloseCommandId ??= newUlid();
    const closeCommandId = pendingCloseCommandId;
    clearFailure();

    const closeCaptureIsCurrent = () => Boolean(
      componentMounted &&
      project?.project_id === closing.project_id &&
      project.session_id === closing.session_id &&
      documentEpoch === closingEpoch &&
      editVersion === closingVersion &&
      pendingCloseCommandId === closeCommandId
    );
    const requestBoundClose = async () => {
      const receipt = await closeProjectSession(
        closing.project_id,
        closing.session_id,
        closeCommandId
      );
      if (
        receipt.command_id !== closeCommandId ||
        receipt.project_id !== closing.project_id ||
        receipt.session_id !== closing.session_id
      ) {
        throw new Error('The desktop returned a close receipt for a different project session.');
      }
      return receipt;
    };

    if (pendingCloseMayHaveCommitted) {
      try {
        await requestBoundClose();
      } catch (error) {
        const failure = recordFailure(error);
        if (closeResultMayHaveCommitted(failure)) {
          transition = 'closing';
          saveMessage = 'Close result uncertain — retry safely';
          announce('Close result uncertain; editing remains locked until the same close command is retried');
          return { status: 'quiesced' };
        } else {
          return await resumeProjectAfterDefinitiveClose(closing);
        }
      }
    } else {
      // Stop new automatic admission before native close drains any reserved
      // startup already in flight. Keep the persisted preference unchanged so
      // a later reopen can restore the author's choice deliberately.
      pendingCloseAgency ??= captureProjectCloseAgency(suggestionsEnabled);
      suggestionsEnabled = false;
      cancelSuggestionTimer();
      const outcome = await drainGenerationsAndClose({
        disableAutomation: () => setSuggestionsPolicy(
          closing.project_id,
          closing.session_id,
          false
        ),
        cancelKnownBranches: cancelActiveBranches,
        closeProject: requestBoundClose,
        normalizeFailure,
        closeResultMayHaveCommitted,
        wait: (delayMs) => new Promise((resolve) => window.setTimeout(resolve, delayMs)),
        isCurrent: closeCaptureIsCurrent
      });

      switch (outcome.status) {
        case 'closed':
          break;
        case 'stale':
          if (
            componentMounted &&
            project?.project_id === closing.project_id &&
            project.session_id === closing.session_id
          ) {
            recordLocalFailure('close_race', 'The manuscript changed while Loom prepared to close it.');
          }
          return await resumeProjectAfterDefinitiveClose(closing);
        case 'uncertain':
          recordFailure(outcome.failure);
          pendingCloseMayHaveCommitted = true;
          transition = 'closing';
          saveMessage = 'Close result uncertain — retry safely';
          announce('Close result uncertain; editing remains locked until the same close command is retried');
          return { status: 'quiesced' };
        case 'waiting':
          pendingCloseMayHaveCommitted = false;
          transition = 'closing';
          if (outcome.failure) recordFailure(outcome.failure);
          else recordLocalFailure(
            'generation_cancellation_in_progress',
            'Private strands are still preserving their terminal evidence. Loom kept the project open; retry close safely.'
          );
          saveMessage = 'Private strands are still stopping — retry close';
          announce('The project remains open while private strands stop; retry close safely');
          return { status: 'quiesced' };
        case 'refused':
          recordFailure(outcome.failure);
          return await resumeProjectAfterDefinitiveClose(closing);
        default: {
          const unreachable: never = outcome;
          return unreachable;
        }
      }
    }

    workspaceRestoreSerial += 1;
    modelRefreshSerial += 1;
    modelLoadSerial += 1;
    documentEpoch += 1;
    closeStrandReview();
    project = null;
    document = null;
    documentText = '';
    sourceDisplayText = '';
    verseCodec = null;
    editVersion = 0;
    savedVersion = 0;
    branches = [];
    promotionArmedCandidateId = null;
    uncertainPromotion = null;
    resetLiveGenerationView();
    saveState = 'clean';
    saveMessage = 'No project open';
    outlineOpen = false;
    suggestionsEnabled = false;
    clearPreferredWriterRequest();
    cancelSuggestionTimer();
    dismissedCandidateIds = [];
    pendingCloseCommandId = null;
    pendingCloseMayHaveCommitted = false;
    pendingCloseAgency = null;
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
    modelLoading = false;
    transition = 'idle';
    return { status: 'closed' };
  }

  function kindLabel(kind: DocumentKind): string {
    if (kind === 'verse') return 'Poem';
    if (kind === 'hybrid') return 'Hybrid';
    return 'Prose';
  }

  function promotionUnavailableReason(branch: BranchCard): string {
    if (document?.summary.kind === 'hybrid') {
      return 'Hybrid promotion waits for a lossless block-manifest IPC boundary.';
    }
    if (branch.source_revision_id !== document?.summary.revision_id) {
      return 'This strand belongs to an earlier manuscript revision. Keep it as an alternative instead.';
    }
    if (editVersion !== savedVersion || sourceDirty) return 'Wait for the manuscript to finish saving.';
    if (branch.selection === 'promote' || branch.selection === 'reject') {
      return 'This strand already has a final recorded selection.';
    }
    return 'Promotion is unavailable while manuscript state is unsettled.';
  }
</script>

<svelte:head>
  <meta name="description" content="Loom — a local-first writing environment for prose and poetry" />
</svelte:head>

<div class="app-shell">
  <a class="skip-link" href="#manuscript">Skip to manuscript</a>

  {#if project}
    <header class="topbar" aria-label="Writing controls">
      {#if project.documents.length > 1}
        <button
          bind:this={outlineToggle}
          class="outline-toggle"
          type="button"
          aria-controls="project-outline"
          aria-expanded={outlineOpen}
          aria-label={outlineOpen ? 'Close manuscript outline' : 'Open manuscript outline'}
          on:click={() => void setOutlineOpen(!outlineOpen)}
        >☰</button>
      {/if}
      <h1 class="context-title" title={document?.summary.title ?? project.title}>
        {document?.summary.title ?? project.title}
      </h1>
      <div class="topbar-spacer"></div>
      {#if document && (saveState === 'error' || saveState === 'uncertain')}
        <div class="save-status state-{saveState}" role="status" aria-live="polite">
          <span class="status-dot"></span>{saveMessage}
        </div>
      {/if}
      {#if document && reviewAffordance.visible}
        <button
          class="alternatives-button"
          bind:this={strandReviewTrigger}
          type="button"
          aria-haspopup="dialog"
          aria-label={reviewAffordance.ariaLabel}
          on:click={() => void openStrandReview()}
        >
          {reviewAffordance.label}
        </button>
      {/if}
      <details class="project-menu" bind:this={projectMenu}>
        <summary class="more-button" bind:this={projectMenuTrigger} title="Writing options">
          <span aria-hidden="true">•••</span>
          <span class="sr-only">Writing options for {project.title}</span>
        </summary>
        <div class="project-menu-popover" aria-label="Project and editor options">
          <div class="project-menu-title">{project.title}</div>
          {#if document}
            <div class="project-menu-label">Editor</div>
            <button class:active={mode === 'visual'} type="button" aria-pressed={mode === 'visual'} disabled={!canUseVisual || editorReadonly} on:click={() => { closeProjectMenu(); void setMode('visual'); }}>
              <span>Visual editor</span><span aria-hidden="true">{mode === 'visual' ? '✓' : ''}</span>
            </button>
            <button class:active={mode === 'source'} type="button" aria-pressed={mode === 'source'} disabled={editorReadonly} on:click={() => { closeProjectMenu(); void setMode('source'); }}>
              <span>Source editor</span><span aria-hidden="true">{mode === 'source' ? '✓' : ''}</span>
            </button>
            <div class="project-menu-separator"></div>
          {/if}
          <button
            class:active={suggestionsEnabled && Boolean(currentModel)}
            type="button"
            aria-haspopup="dialog"
            on:click={(event) => openModelManager(projectMenuTrigger ?? event.currentTarget)}
          >
            <span>Suggestions</span>
            <span class:ready={suggestionMenuState === 'Ready'} class="menu-state">
              {suggestionMenuState}
            </span>
          </button>
          <div class="project-menu-separator"></div>
          <button type="button" disabled={reconciliationResolutionLocked || (editorReadonly && transition !== 'closing' && !(reconciliation && !document))} on:click={() => { closeProjectMenu(); void closeProject(); }}>
            {transition === 'closing' ? 'Retry closing project' : 'Close project'}
          </button>
        </div>
      </details>
    </header>
  {/if}

  {#if project}
    {#if outlineOpen}
      <button class="outline-scrim" type="button" aria-label="Close manuscript outline" on:click={() => void setOutlineOpen(false)}></button>
    {/if}
    <div class:single-document={project.documents.length === 1} class="workspace-grid">
      <div
        bind:this={outlinePanel}
        id="project-outline"
        class:open={outlineOpen}
        class="outline-panel"
        role="dialog"
        aria-modal="true"
        aria-label="Manuscript outline"
        tabindex="-1"
        on:keydown={(event) => trapFocusWithin(event, outlinePanel)}
      >
        <div class="panel-heading">
          <span>Manuscript</span>
          <button
            class="dialog-close"
            type="button"
            aria-label="Close manuscript outline"
            on:click={() => void setOutlineOpen(false)}
          >×</button>
        </div>
        <label class="search-field">
          <span class="sr-only">Search project</span>
          <span aria-hidden="true">⌕</span>
          <input bind:this={outlineSearch} bind:value={search} type="search" placeholder="Find in project" />
        </label>
        <nav class="document-list" aria-label="Documents">
          {#each visibleDocuments as candidate (candidate.document_id)}
            <button
              class:active={candidate.document_id === (reconciliation?.document_id ?? document?.summary.document_id)}
              type="button"
              disabled={editorReadonly}
              on:click={() => void (async () => {
                await setOutlineOpen(false);
                await selectDocument(candidate, true);
              })()}
            >
              <span class="document-glyph" aria-hidden="true">{candidate.kind === 'verse' ? '≋' : '¶'}</span>
              <span class="document-label">
                <strong>{candidate.title}</strong>
                <small>{kindLabel(candidate.kind)} · {candidate.word_count.toLocaleString()} words</small>
              </span>
            </button>
          {:else}
            <p class="empty-copy">No notes.</p>
          {/each}
        </nav>
      </div>

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
              <div class="panel-heading"><span id="resolution-title">Resolution</span></div>
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
                {reconciliationApplying ? 'Checking exact identities…' : pendingReconciliationApply ? 'Retry exact reconciliation' : 'Save resolution'}
              </button>
            </footer>
          </section>
        {:else if document}
          {#if staleDraft}
            <div class="runtime-note" role="alert">
              A crash-safe draft from revision {staleDraft.source_revision_id} is preserved separately. Editing is locked until you explicitly restore or discard it; Loom will not overwrite either version.
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

          <section class="editor-stage" aria-label="Writing surface">
            {#if showVisual}
              <div class="editor-pane visual-pane" aria-label="Visual editor pane">
                {#if exactTextSurface}
                  <div class="verse-notice">Verse stays in the exact-whitespace source surface.</div>
                {:else}
                  {#if canUseVisual}
                    <LoomEditor
                      bind:this={visualEditor}
                      value={documentText}
                      label={`${document.summary.title}, manuscript editor`}
                      ghostText={ghostSuggestion?.text ?? ''}
                      ghostCandidateId={ghostSuggestion?.candidateId ?? ''}
                      ghostPresentationKey={ghostSuggestion?.presentationKey ?? ''}
                      ghostAnchorByteOffset={ghostSuggestion?.targetByte ?? null}
                      surfaceKey={visualGhostSurfaceKey}
                      onChange={updateText}
                      onCompositionChange={setVisualComposition}
                      onImmediateDocumentMutation={invalidateVisualSuggestionImmediately}
                      onGhostAccept={acceptActiveGhost}
                      onGhostDismiss={dismissActiveGhost}
                      onGhostPresentationRejected={rejectVisualGhostPresentation}
                      onGhostVisibilityChange={(presentationKey) => {
                        visibleVisualGhostPresentationKey = presentationKey;
                      }}
                      onSelectionChange={updateVisualSelection}
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
                <SourceEditor
                  bind:this={sourceEditor}
                  bind:element={sourceTextarea}
                  value={sourceDisplayText}
                  readonly={editorReadonly || document.summary.kind === 'hybrid' || Boolean(exactTextSurface && verseCodec && !verseCodec.editable)}
                  verse={exactTextSurface}
                  verseNewline={exactTextSurface ? verseCodec?.newline ?? 'mixed' : null}
                  surfaceKey={`${project.session_id}:${document.summary.document_id}:${documentEpoch}:${mode}`}
                  ghostText={sourceGhostSuggestion?.text ?? ''}
                  ghostCandidateId={sourceGhostSuggestion?.candidateId ?? ''}
                  ghostPresentationKey={sourceGhostSuggestion?.presentationKey ?? ''}
                  onCompositionStart={beginSourceComposition}
                  onCompositionEnd={finishSourceComposition}
                  onValueInput={(textarea) => {
                    updateSourceSelection(textarea);
                    updateFromSource(textarea.value);
                  }}
                  onSelectionChange={updateSourceSelection}
                  onGhostAccept={acceptActiveGhost}
                  onGhostDismiss={dismissActiveGhost}
                  onGhostVisibilityChange={(presentationKey) => {
                    visibleSourceGhostPresentationKey = presentationKey;
                  }}
                  label={exactTextSurface ? 'Exact-whitespace verse editor' : 'Markdown source editor'}
                />
              </div>
            {/if}
          </section>

          {#if strandReviewOpen}
            <dialog
              class="strand-review"
              bind:this={strandReviewDialog}
              aria-labelledby="strand-review-title"
              on:click={(event) => {
                if (event.target === strandReviewDialog) closeStrandReview();
              }}
              on:close={() => {
                strandReviewOpen = false;
                promotionArmedCandidateId = null;
                strandReviewTrigger?.focus();
              }}
              on:cancel={() => {
                strandReviewOpen = false;
                promotionArmedCandidateId = null;
              }}
            >
              <div class="strand-review-shell">
              <header class="strand-review-header">
                <div>
                  <h2 id="strand-review-title">Alternatives</h2>
                  {#if reviewBranchIndex >= 0}
                    <span>{reviewBranchIndex + 1} of {reviewableBranches.length}</span>
                  {/if}
                </div>
                <button
                  class="dialog-close"
                  data-review-close
                  type="button"
                  aria-label="Close alternatives"
                  on:click={closeStrandReview}
                >×</button>
              </header>

              {#if reviewBranch}
                <div class="strand-review-prose">{reviewBranch.text}</div>

                {#if reviewableBranches.length > 1}
                  <nav class="strand-review-navigation" aria-label="Alternative navigation">
                    <button type="button" on:click={() => moveStrandReview(-1)}>Previous</button>
                    <button type="button" on:click={() => moveStrandReview(1)}>Next</button>
                  </nav>
                {/if}

                <details class="strand-evidence">
                  <summary>Evidence</summary>
                  <dl>
                    <div><dt>Model</dt><dd>{reviewBranch.model_id ?? 'Recorded model'}</dd></div>
                    <div><dt>Seed</dt><dd>{reviewBranch.seed ?? 'Recorded in provenance'}</dd></div>
                    <div><dt>Boundary</dt><dd>{reviewBranch.target_start_byte}</dd></div>
                    <div><dt>Output</dt><dd>{reviewBranch.output_blob_id ?? 'Pending immutable body'}</dd></div>
                    <div><dt>Run</dt><dd>{reviewBranch.run_id}</dd></div>
                  </dl>
                  {#if suppressedReviewBranches.length > 0}
                    <details class="suppressed-output">
                      <summary>{suppressedReviewBranches.length} malformed {suppressedReviewBranches.length === 1 ? 'output' : 'outputs'} held back</summary>
                      {#each suppressedReviewBranches as suppressed (suppressed.run_id)}
                        <div>
                          <strong>{candidateSurfaceReason(suppressed.text) ?? 'Held back'}</strong>
                          <pre>{suppressed.text}</pre>
                        </div>
                      {/each}
                    </details>
                  {/if}
                </details>

                <footer class="strand-review-actions">
                  <button class="primary-button" type="button" on:click={() => void acceptInlineSuggestion(reviewBranch)} disabled={!canPromoteBranch(reviewBranch)} title={promotionUnavailableReason(reviewBranch)}>
                    Use this
                  </button>
                </footer>
              {:else}
                <p class="strand-review-empty">No suitable alternative remains at this caret.</p>
              {/if}
              </div>
            </dialog>
          {/if}

          {#if uncertainPromotion}
            <div class="attention-action">
              <button class="secondary-button" type="button" on:click={() => void resolveUncertainPromotion()} disabled={promotionInFlight}>
                {promotionInFlight ? 'Checking suggestion…' : 'Check suggestion result'}
              </button>
            </div>
          {/if}
        {:else}
          <section class="empty-project">
            {#if uncertainPromotion}
              <h1>Promotion needs confirmation.</h1>
              <p>The active editor was detached so stale bytes cannot overwrite a promotion that may already be durable.</p>
              <button class="primary-button" type="button" on:click={() => void resolveUncertainPromotion()} disabled={promotionInFlight}>
                {promotionInFlight ? 'Checking authoritative state…' : 'Check promotion result'}
              </button>
            {:else}
              <h1>No notes.</h1>
            {/if}
          </section>
        {/if}
      </main>
    </div>
  {:else}
    <main class="welcome" id="manuscript">
      <section class="welcome-note" aria-labelledby="welcome-title">
        <h1 id="welcome-title">{errorMessage ? 'Your writing did not open.' : desktop ? 'Opening your writing…' : 'Desktop app required.'}</h1>
        {#if !desktop}
          <div class="runtime-note" role="note">Writing and local models are available in the desktop app.</div>
        {/if}
        {#if errorMessage}
          <div class="error-banner" role="alert">
            {errorMessage}{#if lastFailure}<small> · {lastFailure.code}{lastFailure.retryable ? ' · retryable' : ''}</small>{/if}
          </div>
        {/if}
        <div class="welcome-actions">
          <button class="secondary-button" type="button" on:click={doOpenProject} disabled={!desktop || opening}>
            {opening ? 'Opening…' : 'Choose another folder…'}
          </button>
        </div>
      </section>
    </main>
  {/if}

  {#if modelManagerOpen}
    <div
      class="model-manager-backdrop"
      role="presentation"
      on:click={(event) => {
        if (event.target === event.currentTarget) closeModelManager();
      }}
    >
      <div
        bind:this={modelManagerPanel}
        class="model-manager"
        role="dialog"
        aria-modal="true"
        aria-labelledby="model-manager-title"
        tabindex="-1"
        on:keydown={trapModelManagerFocus}
      >
        <header class="model-manager-header">
          <h2 id="model-manager-title">Suggestions</h2>
          <button class="icon-button" type="button" on:click={() => closeModelManager()} aria-label="Close suggestions">×</button>
        </header>

        <div class="model-manager-body">
          <section class="model-manager-summary" aria-label="Suggestion settings">
            <label class="suggestions-setting">
              <input
                data-model-manager-initial-focus
                type="checkbox"
                checked={suggestionsEnabled}
                disabled={!project || suggestionsChanging}
                on:change={(event) => void setSuggestionsEnabled(event.currentTarget.checked)}
              />
              <span>
                <strong>Suggestions</strong>
              </span>
            </label>

            <div class="model-readiness" role="status" aria-live="polite">
              <span
                class:ready={Boolean(currentModel) && !modelLoading && !modelUnloading}
                class:preparing={modelLoading || modelChoosing || modelUnloading || modelDownloadStarting || activeModelDownloads.length > 0}
                class="status-dot"
              ></span>
              <strong>
                {modelLoading || modelChoosing || modelUnloading || modelDownloadStarting || activeModelDownloads.length > 0
                  ? 'Preparing'
                  : currentModel
                    ? 'Ready'
                    : 'Needs setup'}
              </strong>
            </div>
          </section>

          {#if suggestionSetupNeeded}
            <section class="model-setup-callout">
              <div class="model-setup-intro">
                <strong>Private writing model</strong>
                <p>Loom uses {expectedPolicyWriterName()} for suggestions. The model stays on this computer.</p>
              </div>

              {#if compatibleWriterModels.length > 0}
                <div class="writer-model-list" aria-label="Compatible writing models">
                  {#each compatibleWriterModels as model (model.model_path)}
                    <article class="writer-model-choice">
                      <div>
                        <strong>{model.display_name}</strong>
                        <span>{formatByteCount(model.file_bytes)} · Possible local match</span>
                      </div>
                      <button
                        class="primary-button compact"
                        type="button"
                        on:click={() => void useDiscoveredPolicyWriter(model)}
                        disabled={!desktop || modelChoosing || modelLoading || modelUnloading}
                      >{modelLoading && selectedModelPath === model.model_path ? 'Verifying…' : 'Verify and use'}</button>
                    </article>
                  {/each}
                </div>
              {:else}
                <div class="model-empty-state">
                  <strong>No compatible writing model found.</strong>
                  <span>Loom checked its model library and the local Hugging Face cache. You can locate an existing GGUF file without moving it.</span>
                </div>
              {/if}

              {#if modelSetupError}
                <p class="model-setup-error" role="alert">{modelSetupError}</p>
              {/if}

              <div class="model-setup-actions">
                <button
                  class={compatibleWriterModels.length > 0 ? 'bare-button compact' : 'secondary-button'}
                  type="button"
                  on:click={() => void choosePolicyWriterModel()}
                  disabled={!desktop || modelChoosing || modelLoading || modelUnloading}
                >{modelChoosing ? 'Locating…' : 'Locate compatible model file…'}</button>
                <button class="bare-button compact" type="button" on:click={() => void refreshCurrentModelsAndEnsureWriter()} disabled={!desktop || modelChoosing || modelLoading || modelUnloading}>Refresh library</button>
              </div>
            </section>
          {/if}

          <details class="model-advanced-panel">
            <summary>Advanced</summary>
            <div class="model-advanced-content">
              <section class="model-library" aria-labelledby="model-library-title">
            <div class="section-heading">
              <div>
                <h3 id="model-library-title">Other local models</h3>
                <p>Visible for diagnosis only. This build will not use an unverified model for suggestions.</p>
              </div>
            </div>

            {#if otherLocalModels.length > 0}
              <div class="other-model-list">
                {#each otherLocalModels as model (model.model_path)}
                  <article class="other-model-row">
                    <div>
                      <strong>{model.display_name}</strong>
                      <span>{formatByteCount(model.file_bytes)}</span>
                    </div>
                    <span class="model-incompatible">Not compatible with suggestions</span>
                  </article>
                {/each}
              </div>
            {:else}
              <div class="model-empty-state">
                <strong>No other local GGUF models found.</strong>
              </div>
            {/if}

            {#if selectedModel?.loaded}
              <article class="model-facts">
                <details class="model-technical">
                  <summary>Loaded writer details</summary>
                  <dl>
                    <div><dt>Compatibility</dt><dd>{selectedModel.tested_profile ? 'Tested for Loom' : selectedModel.header_verified ? 'File inspected' : 'Unavailable'}</dd></div>
                    <div><dt>Prompt mode</dt><dd>{modelCapabilityMode(selectedModel)}</dd></div>
                    <div><dt>Architecture</dt><dd>{selectedModel.architecture ?? 'Inspect on load'}</dd></div>
                    <div><dt>Context</dt><dd>{selectedModel.context_tokens === null ? 'Inspect on load' : `${selectedModel.context_tokens.toLocaleString()} tokens`}</dd></div>
                    <div><dt>Media</dt><dd>{modelMediaLabel(selectedModel)}</dd></div>
                    <div><dt>Generated tokens</dt><dd>{selectedModel.loaded ? (selectedModel.output_tokens ? 'Available' : 'Unavailable') : 'Inspect on load'}</dd></div>
                    <div><dt>Log probabilities</dt><dd>{selectedModel.loaded ? (selectedModel.logprobs ? 'Available' : 'Unavailable') : 'Inspect on load'}</dd></div>
                    <div><dt>Fill in middle</dt><dd>{selectedModel.loaded ? (selectedModel.fill_in_middle ? 'Verified' : 'Unavailable') : 'Inspect on load'}</dd></div>
                    <div><dt>Projector</dt><dd>{selectedModel.projector_present === null ? 'Inspect on load' : selectedModel.projector_present ? 'Present' : 'None'}</dd></div>
                  </dl>
                  <dl class="model-evidence">
                    <div><dt>File</dt><dd><code>{selectedModel.model_path}</code></dd></div>
                    <div><dt>SHA-256</dt><dd><code>{selectedModel.model_sha256 ?? 'Computed during native load'}</code></dd></div>
                  </dl>
                </details>
                <div class="model-manager-actions">
                  <button class="secondary-button" type="button" on:click={() => void unloadCurrentModel()} disabled={modelUnloading || activeBranchCount > 0} title={activeBranchCount > 0 ? 'Finish or cancel active strands first' : 'Release model weights from memory'}>
                    {modelUnloading ? 'Releasing…' : 'Unload from memory'}
                  </button>
                </div>
              </article>
            {/if}
              </section>

              <details class="model-download-panel">
            <summary>Add a model from a verified URL</summary>
            <div class="model-download-content">
            <div class="section-heading">
              <div>
                <h3>Add a verified GGUF</h3>
                <p>Bring a publisher URL and its exact checksum. Loom will not guess either one.</p>
              </div>
              {#if activeModelDownloads.length > 0}
                <span class="fact-chip verified">{activeModelDownloads.length} active</span>
              {/if}
            </div>

            {#if !desktop}
              <div class="runtime-note" role="note">Verified downloads are available in the Tauri desktop build.</div>
            {/if}

            <form class="model-download-form" on:submit|preventDefault={() => void beginOrRetryModelDownload()}>
              <label class="wide-field">
                <span>HTTPS model URL</span>
                <input
                  value={modelDownloadUrl}
                  on:input={(event) => updateModelDownloadUrl(event.currentTarget.value)}
                  type="url"
                  inputmode="url"
                  autocomplete="off"
                  placeholder="https://publisher.example/model.gguf"
                  disabled={!desktop || pendingModelDownload !== null || modelDownloadStarting}
                  required
                />
              </label>
              <label class="wide-field">
                <span>Local file name</span>
                <input bind:value={modelDownloadFileName} autocomplete="off" spellcheck="false" placeholder="writer-base.Q8_0.gguf" disabled={!desktop || pendingModelDownload !== null || modelDownloadStarting} required />
              </label>
              <label class="wide-field">
                <span>Expected SHA-256 <small>required · 64 hexadecimal characters</small></span>
                <input bind:value={modelDownloadSha256} autocomplete="off" spellcheck="false" inputmode="text" placeholder="Publisher checksum" disabled={!desktop || pendingModelDownload !== null || modelDownloadStarting} required />
              </label>
              <label>
                <span>Exact bytes <small>optional</small></span>
                <input bind:value={modelDownloadExpectedBytes} autocomplete="off" inputmode="numeric" placeholder="4954576032" disabled={!desktop || pendingModelDownload !== null || modelDownloadStarting} />
              </label>
              <label>
                <span>Hard ceiling <small>GiB</small></span>
                <input bind:value={modelDownloadMaximumGiB} type="number" min="0.001" max="1024" step="0.001" disabled={!desktop || pendingModelDownload !== null || modelDownloadStarting} required />
              </label>
              <p class="download-boundary wide-field">The URL is contacted only after you press download. Credentials in URLs are refused. A partial file may be resumed, but installation occurs only after a cold SHA-256 check and GGUF validation.</p>

              {#if modelDownloadError}
                <div class="download-error wide-field" role="alert">{modelDownloadError}</div>
              {/if}
              {#if modelDownloadUncertain && pendingModelDownload}
                <div class="uncertain-download wide-field" role="status">
                  <strong>Command reply uncertain.</strong>
                  Retrying preserves command <code>{pendingModelDownload.commandId}</code> and every request byte.
                  {#if modelDownloadCanAbandon}
                    The desktop confirmed that this non-retryable request was not registered, so it is safe to edit.
                    <button class="bare-button compact" type="button" on:click={abandonUnstartedModelDownload}>Edit rejected request</button>
                  {:else}
                    Inputs remain locked until authoritative status is recovered.
                  {/if}
                </div>
              {/if}

              <div class="model-download-actions wide-field">
                <button
                  class="primary-button"
                  type="submit"
                  disabled={!desktop || modelDownloadStarting || (pendingModelDownload !== null && !modelDownloadUncertain)}
                >
                  {modelDownloadStarting
                    ? 'Registering verified transfer…'
                    : modelDownloadUncertain
                      ? 'Retry exact command safely'
                      : pendingModelDownloadSnapshot
                        ? 'Download in progress'
                        : 'Download and verify'}
                </button>
              </div>
            </form>

            {#if modelDownloads.length > 0}
              <div class="download-history" aria-label="Recent model downloads">
                <h4>Transfers on this app session</h4>
                {#each modelDownloads.slice(0, 6) as download (download.command_id)}
                  {@const percent = downloadProgressPercent(download.downloaded_bytes, download.total_bytes)}
                  <article class:terminal={modelDownloadIsTerminal(download)} class="download-card">
                    <header>
                      <div>
                        <strong>{download.display_name}</strong>
                        <span>{modelDownloadStatusLabel(download)}</span>
                      </div>
                      <span>{formatByteCount(download.downloaded_bytes)}{download.total_bytes === null ? '' : ` / ${formatByteCount(download.total_bytes)}`}</span>
                    </header>
                    {#if percent === null && !modelDownloadIsTerminal(download)}
                      <progress aria-label={`${download.display_name} download progress`}></progress>
                    {:else if percent !== null}
                      <progress max="100" value={percent} aria-label={`${download.display_name} download progress`}>{percent.toFixed(0)}%</progress>
                    {/if}
                    {#if download.resumed_from_bytes > 0}
                      <small>Resumed after verifying {formatByteCount(download.resumed_from_bytes)} of partial data.</small>
                    {/if}
                    {#if download.cancel_requested && !modelDownloadIsTerminal(download)}
                      <small>Cancellation requested; waiting for the transfer to reach a safe stop.</small>
                    {/if}
                    {#if download.status.status === 'failed'}
                      <p class="download-card-error">{download.status.message}</p>
                    {/if}
                    {#if download.event_delivery_failures > 0}
                      <small>Desktop event delivery missed {download.event_delivery_failures} update{download.event_delivery_failures === 1 ? '' : 's'}; this view reconciles from command status.</small>
                    {/if}
                    <footer>
                      <code title={download.command_id}>{download.expected_sha256.slice(0, 12)}…</code>
                      {#if !modelDownloadIsTerminal(download)}
                        <button class="secondary-button compact" type="button" on:click={() => void cancelVerifiedModelDownload(download.command_id)} disabled={download.cancel_requested || modelDownloadCancellingIds.includes(download.command_id)}>
                          {download.cancel_requested || modelDownloadCancellingIds.includes(download.command_id) ? 'Cancelling…' : 'Cancel'}
                        </button>
                      {:else if download.status.status === 'completed'}
                        <button class="secondary-button compact" type="button" on:click={() => void selectCompletedModelDownload(download)}>Select model</button>
                      {/if}
                    </footer>
                  </article>
                {/each}
              </div>
            {/if}
            </div>
              </details>
            </div>
          </details>
        </div>
      </div>
    </div>
  {/if}

  {#if errorMessage && project}
    <div class="toast error" role="alert">
      <span>{errorMessage}{#if lastFailure}<small> · {lastFailure.code}{lastFailure.retryable ? ' · retryable' : ''}</small>{/if}</span>
      {#if transition === 'closing' && pendingCloseCommandId}
        <button type="button" on:click={() => void closeProject()} aria-label="Retry close safely">Retry close</button>
      {:else if uncertainDraft}
        <button type="button" on:click={() => void persistTransientDraft()} aria-label="Retry draft safely">Retry draft</button>
      {:else if uncertainSave}
        <button type="button" on:click={() => void saveNow()} aria-label="Retry save safely">Retry save</button>
      {:else}
        <button type="button" on:click={clearFailure} aria-label="Dismiss error">×</button>
      {/if}
    </div>
  {/if}
  <div class="sr-only" aria-live="polite">{liveRegion}</div>
</div>
