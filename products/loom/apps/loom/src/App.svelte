<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { convertFileSrc } from '@tauri-apps/api/core';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import LoomEditor from './lib/LoomEditor.svelte';
  import TerminalPane from './lib/TerminalPane.svelte';
  import { installManuscriptRunShortcut } from './lib/manuscriptShortcut';
  import { workspaceCommand } from './lib/workspaceCommand';
  import PaneDivider from './lib/PaneDivider.svelte';
  import type { TerminalSourceRange } from './lib/terminalSelection';
  import { workspaceRows } from './lib/workspaceTree';
  import { readWorkspaceFolders, rememberWorkspaceFolder, type WorkspaceFolder } from './lib/workspaceFolders';
  import WorkspacePane from './lib/WorkspacePane.svelte';
  import SignalPane from './lib/SignalPane.svelte';
  import CabalPane from './lib/CabalPane.svelte';
  import { SignalDraftEditor } from './lib/signalDraft';
  import { CabalEditor, cabalSnapshot, cabalWorkspace, openCabal, editCabal, shareCabal, joinCabal, recoverCabalEdits, revokeCabalMember, type CabalSnapshot, type SharedDocument } from './lib/cabal';
  import { rememberSignalWorkspace, type SignalConversation } from './lib/signal';
  import { RetainedOutputLoader } from './lib/retainedOutput';
  import { workspaceWriterCandidates, workspaceWriterModel, type WorkspaceTemplateSnapshot } from './lib/workspaceTemplate';
  import { getWorkspaceTemplate, enableWorkspaceTemplate } from './lib/ipc';
  import { startAudioRecording, stopAudioRecording, synthesizeAudio, type AudioRecording } from './lib/ipc';
  import VisualFormatMenu from './lib/VisualFormatMenu.svelte';
  import SourceEditor from './lib/SourceEditor.svelte';
  import MissingDocumentRecoveryNotice from './lib/MissingDocumentRecoveryNotice.svelte';
  import {
    abortApplicationClose,
    runTerminal,
    listTerminalRuns,
    cancelTerminalRun,
    addDocumentContexts,
    applyCoWriter,
    applicationClosePending,
    cancelGeneration,
    cancelSpeechInput,
    cancelSpeechRecording,
    cancelModelDownload,
    checkpointDocument,
    clearTransientDraft,
    applyDocumentReconciliation,
    chooseAttachments,
    prepareProjectOpen,
    commitProjectOpen,
    discardProjectOpen,
    chooseModel,
    closeProject as closeProjectSession,
    createDocument,
    currentProjectSession,
    deleteDocument,
    deleteCoWriter,
    exportDocumentCopy,
    getBranch,
    getBranchBody,
    getBranchPage,
    getCompletionSnapshot,
    getBuildModelPolicy,
    getModelDownloadStatus,
    getSpeechInputStatus,
    getWeaveStatus,
    ingestImageAttachment,
    importAttachmentPaths,
    isDesktopRuntime,
    listenForApplicationCloseRequests,
    listenForDocumentFilesystemHints,
    listenForFileCommands,
    listenForGenerationEvents,
    listenForModelDownloadEvents,
    loadCatalogModelCandidate,
    loadModel,
    loadPolicyModelCandidate,
    listCoWriters,
    listCuratedModels,
    listDocumentContext,
    listModels,
    listModelDownloads,
    openDefaultProject,
    prepareProjectOpenPath,
    projectDropDirectories,
    openDocument,
    importExternalDocument,
    previewDocumentReconciliation,
    promoteCandidate,
    recoverProject,
    renameDocument,
    removeDocumentContext,
    revealDocument,
    requestApplicationClose,
    saveCoWriter,
    setFocusMode,
    setDocumentContextSnapshot,
    setSuggestions as setSuggestionsPolicy,
    startSpeechRecording,
    startWeave,
    startModelDownload,
    stopSpeechRecording,
    unloadModel,
    normalizeFailure,
    upsertTransientDraft
  } from './lib/ipc';
  import {
    attachmentMarkdown,
    encodeImageAttachment,
    imageAttachmentTransferError,
    projectAssetProtocolToken
  } from './lib/attachments';
  import {
    decodeVerseForEditor as decodeSourceForEditor,
    encodeVerseFromEditor as encodeSourceFromEditor,
    type VerseEditorCodec as SourceEditorCodec
  } from './lib/verseCodec';
  import { sourceCaretByte } from './lib/sourceCaret';
  import { canUseVisualMarkdown } from './lib/markdownSafety';
  import {
    autocompleteDisposition,
    verifiedGhostSuggestion,
    type AutocompleteDisposition
  } from './lib/ghostSuggestion';
  import {
    emptyAutocompleteRetryLedger,
    planAutocompleteRetry,
    type AutocompleteRetryLedger
  } from './lib/autocompleteRetry';
  import {
    candidateTextIsSurfaceable
  } from './lib/candidateSurface';
  import { sourceGhostPresentationCompatible } from './lib/sourceGhostText';
  import {
    inlineSuggestionFamily,
    projectInlineCandidateText,
    projectedInlinePresentationKey,
    type InlineGhostSuggestion
  } from './lib/inlineSuggestionFamily';
  import {
    visualGhostTextMayBePlainProse,
    type VisualCaretBoundaryFailure
  } from './lib/ghostText';
  import { isExtendedGraphemeBoundary } from './lib/graphemeBoundary';
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
  import { completionSnapshotFacts } from './lib/completionSnapshot';
  import { shouldCaptureFormatMenuEscape } from './lib/appKeyboardRouting';
  import { writeRebindsStaleDraft } from './lib/draftRecovery';
  import { documentProjectionDecision } from './lib/projectionState';
  import {
    navigationScopeIsCurrent,
    projectRestoreScopeIsCurrent,
    projectSessionIsCurrent,
    type ProjectRestoreScope
  } from './lib/projectScope';
  import {
    cancellationFailureNeedsUserAttention,
    captureForIdempotentRetry,
    closeResultMayHaveCommitted,
    failureIsDefiniteContention
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
  import { DetachedProjectCloseCoordinator } from './lib/detachedProjectClose';
  import { editableImportMarkdown, normalizeImportedMarkdown } from './lib/importedMarkdown';
  import { nativeDropPoint as convertNativeDropPoint, nativeDropScope } from './lib/nativeAttachmentDrop';
  import { suggestionsEnabledFromStoredPreference } from './lib/suggestionPreference';
  import {
    resolveAppearance,
    toggledAppearance,
    type AppearancePreference
  } from './lib/appearance';
  import {
    catalogDownloadRequests,
    isVerifiedCatalogWriter,
    legacyLocalCatalogMatch,
    validateCuratedModelCatalog
  } from './lib/modelCatalog';
  import {
    captureProjectCloseAgency,
    restoreProjectCloseAgency,
    type ProjectCloseAgencySnapshot
  } from './lib/projectCloseAgency';
  import {
    acquireStartupProject,
    attachWorkspaceProjectReply,
    restoreBeforeBackgroundWork,
    runCurrentWorkspaceStep,
    shouldDiscoverModelsOnStartup,
    workspaceResumeAction
  } from './lib/startupSafety';
  import { newUlid } from './lib/ulid';
  import {
    type CompletionInsertionAction
  } from './lib/suggestionInteraction';
  import {
    acceptedCompletionText,
    completionPresentation as completionSessionPresentation,
    completionSessionContextKey,
    completionShouldRequestNextBatch,
    selectedCompletionCandidate,
    type CompletionSession
  } from './lib/completionSession';
  import {
    completionEngineBecameDisabled,
    completionEngineBecameEnabled,
    completionEngineEnabled,
    inlineGhostHidden
  } from './lib/completionModes';
  import {
    unavailableVisualCompletionWitness,
    unavailableVisualSelectionWitness,
    type VisualCompletionAccessibilityWitness,
    type VisualSelectionAccessibilityWitness
  } from './lib/completionAccessibility';
  import { observeNativeFullscreen } from './lib/nativeFullscreen';
  import {
    applyDocumentRenameProjection,
    boundedDocumentTitleInput,
    captureDocumentTarget,
    capturedDocumentBelongsToSession,
    capturedDocumentIdentityIsCurrent,
    clampDocumentMenuPoint,
    createDocumentRenameCompositionGuard,
    documentDeleteMenuIndex,
    documentMenuKeyAction,
    documentRevealLabel,
    isDocumentContextTriggerKey,
    refreshDocumentDeleteTarget,
    refreshDocumentRenameTarget,
    releaseDocumentRenameAndRestoreFocus,
    visibleDocumentActionsMenuPoint,
    type CapturedDocumentTarget,
    type DocumentContextAction,
    type MenuPoint
  } from './lib/documentContextActions';
  import {
    applyGuardedProjectFilesystemRefresh,
    beginMissingDocumentCaptureBoundary,
    captureProjectFilesystemRefreshBoundary,
    documentBoundaryNeedsRecovery,
    documentRefreshDecision,
    missingDocumentJournalIsDurable,
    missingDocumentRecoveryRequiresCopy,
    openedDocumentSubsumesMissingRecovery,
    projectFilesystemRefreshBoundaryDisposition,
    type MissingDocumentCaptureIdentity,
    type ProjectFilesystemRefreshBoundaryState
  } from './lib/documentLifecycle';
  import { routeDocumentFilesystemHint } from './lib/documentFilesystemHint';
  import {
    completionGenerationIsArmed,
    type CompletionGenerationTrigger
  } from './lib/completionGenerationIntent';
  import {
    armCompletionScheduleIntent,
    authorizeCompletionInsertion,
    authorizeCompletionUnconsume,
    bindCompletionAnchor,
    cancelCompletionSchedule,
    clearCompletionSession as clearCompletionControllerSession,
    clearUnpresentableVisualKeys,
    completionActivityExists as controllerHasCompletionActivity,
    completionControllerView,
    completionExhausted,
    cycleCompletion,
    dismissCompletion,
    initialCompletionControllerState,
    invalidateCompletionNavigation as invalidateControllerNavigation,
    invalidateVisualMutation,
    observeTextMutation,
    reconcileCompletionController,
    refreshCompletionCandidate,
    rejectVisualPresentation,
    resetCompletionDiscovery,
    resetCompletionSurface,
    setCompletionSchedule,
    setDismissedCompletionCandidates,
    settleCompletionNavigation,
    shuttleScheduleKey as completionShuttleScheduleKey,
    type AutocompleteRetryTicket,
    type CompletionControllerEffect,
    type CompletionSchedule
  } from './lib/completionController';
  import {
    automaticCompletionLifecycle,
    completionLifecycleDescription,
    retainsScheduledCompletion
  } from './lib/completionLifecycle';
  import type {
    VisualFormatAction,
    VisualFormatState
  } from './lib/visualFormatting';
  import {
    DEFAULT_MODEL_DOWNLOAD_LIMIT_GIB,
    deriveGgufFileName,
    downloadProgressPercent,
    formatByteCount,
    validateVerifiedDownload,
    type VerifiedDownloadForm
  } from './lib/modelDownload';
  import {
    generationEventBelongsToScope
  } from './lib/weaveSafety';
  import {
    isEphemeralAcceptanceModelPath,
    isOfficialGemma4CatalogHint,
    isVerifiedPolicyWriter,
    isUsableSuggestionWriter,
    looksLikeVisionAdapter,
    orderedLocalTextModels,
    preferredWriterModelPath,
    suggestionWriter
  } from './lib/modelPolicy';
  import type {
    BranchCard,
    BranchPageCursor,
    BranchSummary,
    BuildModelPolicySummary,
    CoWriterSummary,
    CommandReceipt,
    ContextAttachmentPresentation,
    ContextMediaPresentation,
    ContextTextSourcePresentation,
    CuratedModelCatalogEntry,
    DesktopGenerationEnvelope,
    DocumentKind,
    DocumentSummary,
    EditorMode,
    ModelCapabilitySummary,
    ModelDownloadPhase,
    ModelDownloadSnapshot,
    LoomFailure,
    OpenDocument,
    ProjectCloseReceipt,
    ProjectSnapshot,
    ReconciliationPreview,
    SaveState,
    SpeechInputSnapshot,
    SpeechInputTarget,
    SpeechRecordingSnapshot,
    TransientDraftSnapshot,
    DocumentContextSnapshot,
    WeaveStarted,
    TerminalRun,
    TerminalRunRequest
  } from './lib/types';

  type VisualTextInsertionAnchor = {
    surfaceKey: string;
    markdown: string;
    from: number;
    to: number;
  };
  type SourceTextInsertionAnchor = {
    surfaceKey: string;
    value: string;
    start: number;
    end: number;
  };
  type SpeechInsertionAnchor =
    | { kind: 'visual'; target: SpeechInputTarget; anchor: VisualTextInsertionAnchor }
    | { kind: 'source'; target: 'manuscript'; anchor: SourceTextInsertionAnchor }
    | { kind: 'context-source'; target: 'context'; value: string; start: number; end: number };

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
  let workspaceFolders: WorkspaceFolder[] = [];
  let workspaceRootExpanded = true;
  let workspaceDocuments: Record<string, string> = {};
  let workspaceDropActive = false;
  let outlineElement: HTMLElement | undefined;
  $: visibleWorkspaceFolders = project && !workspaceFolders.some(folder => folder.root === project?.root)
    ? [...workspaceFolders, { root: project.root, title: project.title }] : workspaceFolders;
  let workspaceTemplate: WorkspaceTemplateSnapshot | null = null;
  let workspaceTemplateScope = '';
  let deferredWorkspaceTemplate: WorkspaceTemplateSnapshot | null = null;
  let deferredTemplateSession = '';
  $: if (deferredWorkspaceTemplate && !compositionActive) applyDeferredTemplate();
  let templateKey = '';
  let templateSerial = 0;
  let paneSelection: Record<string, string> = {};
  let hiddenPaneSlots = new Set<string>();
  let workspaceWidth = 1000;
  let workspaceHeight = 600;
  let outlineWidth = 220;
  let rightWidth = 320;
  let bottomHeight = 240;
  let terminalHeight = 220;
  let terminalDockHeight = 0;
  $: terminalLimit = Math.max(100, (workspaceHeight + terminalDockHeight) * 0.6);
  $: effectiveTerminalHeight = Math.min(terminalHeight, terminalLimit);
  $: outlineLimit = Math.max(150, workspaceWidth * 0.35);
  $: rightLimit = Math.max(180, workspaceWidth - (outlineOpen ? Math.min(outlineWidth, outlineLimit) : 0) - 200);
  $: rightPaneOpen = signalOpen || cabalOpen || paneSlots.some(slot => slot.position === 'right' && slot.selected && !hiddenPaneSlots.has('right'));
  $: bottomPaneOpen = paneSlots.some(slot => slot.position === 'bottom' && slot.selected && !hiddenPaneSlots.has('bottom'));
  let paneEditors: Record<string, WorkspacePane> = {};
  let paneBusy: Record<string, boolean> = {};
  let paneComposing: Record<string, boolean> = {};
  $: configuredPanes = workspaceTemplate?.enabled && !workspaceTemplate.error ? Object.entries(workspaceTemplate.config.panes) : [];
  $: busyPaneSlots = new Set(configuredPanes.filter(([id]) => paneBusy[id]).map(([, config]) => config.position));
  $: paneSlots = (['main', 'right', 'bottom'] as const).map(position => {
    const choices = configuredPanes.filter(([, config]) => config.position === position && config.visible);
    return { position, choices, selected: choices.find(([id]) => id === paneSelection[position]) ?? choices[0] };
  });
  $: mainPane = paneSlots.find(slot => slot.position === 'main')?.selected;
  $: customMain = Boolean(mainPane && (mainPane[1].kind !== 'editor' || mainPane[1].document));
  $: if (desktop && project) {
    const key = `${project.project_id}/${project.session_id}/${project.documents.find(item => item.relative_path === '.loom.md')?.revision_id ?? ''}`;
    if (key !== templateKey) { templateKey = key; void refreshWorkspaceTemplate(); }
  } else { workspaceTemplate = null; workspaceTemplateScope = ''; deferredWorkspaceTemplate = null; templateKey = ''; }
  let outlineOpen = false;
  let collapsedFolders = new Set<string>(['Runs/']);
  let terminalOpen = false;
  let signalOpen = false;
  let cabalOpen = false;
  const signalDraftEditor = new SignalDraftEditor();
  let cabal: CabalSnapshot | null = null;
  let cabalEditor: CabalEditor | null = null;
  let cabalScope = '';
  let cabalAttaching = false;
  let cabalRefreshing = false;
  let cabalSnapshotPending: Promise<CabalSnapshot | null> | null = null;
  let cabalApplying = false;
  let cabalRecovered = false;
  let cabalTimer: ReturnType<typeof setTimeout> | undefined;
  $: cabalDocumentRemoved = Boolean(cabal?.documents.find(item => item.local.summary.document_id === document?.summary.document_id)?.shared.deleted);
  $: cabalDocumentProblem = cabal?.problems.find(item => item.document_id === document?.summary.document_id);
  $: cabalWriteBlocked = Boolean(cabalRecovered || (cabal?.read_only && cabal.documents.some(item => item.local.summary.document_id === document?.summary.document_id)) || cabalDocumentRemoved || cabalDocumentProblem) && !compositionActive && !sourceDirty && !visualMutationPending;
  $: if (componentMounted && desktop && project && transition === 'idle') {
    const scope = `${project.project_id}/${project.session_id}/${document?.summary.document_id ?? ''}`;
    if (scope !== cabalScope) { cabalScope = scope; void attachCabal(scope); }
  }
  let terminalEntry = '';
  let terminalRuns: TerminalRun[] = [];
  let terminalDispatching = false;
  let terminalCancelRequested = false;
  let cancelledTerminalIds = new Set<string>();
  let terminalPendingRequest: TerminalRunRequest | null = null;
  let terminalError = '';
  let terminalSessionKey = '';
  let terminalRefreshSerial = 0;
  let terminalPollTimer: number | undefined;
  let contextPaneOpen = false;
  let focusedSpeechTarget: SpeechInputTarget = 'manuscript';
  let contextPaneElement: HTMLDivElement | undefined;
  let contextToggleElement: HTMLButtonElement | undefined;
  let contextAttachments: ContextAttachmentPresentation[] = [];
  let contextTextSources: ContextTextSourcePresentation[] = [];
  let contextText = '';
  let persistedContextText = '';
  let contextTextSaveState: 'clean' | 'dirty' | 'saving' | 'error' = 'clean';
  let contextTextSaveTimer: number | undefined;
  let contextTextSaveQueue: Promise<void> = Promise.resolve();
  let contextCompositionActive = false;
  let contextVisualEditor: {
    flushPending: () => boolean;
    focusAtDocumentEnd: () => boolean;
    focusPreservingSelection: () => boolean;
    captureFormattingSelection: (focusTransitionFrom?: EventTarget | null) => boolean;
    clearFormattingSelection: () => void;
    applyFormatting: (action: VisualFormatAction, href?: string) => boolean;
    formattingDiagnostic: () => string;
    insertTextAtSelection: (text: string) => boolean;
    captureTextInsertionAnchor: () => VisualTextInsertionAnchor | null;
    insertTextAtAnchor: (anchor: VisualTextInsertionAnchor, text: string) => boolean;
  } | null = null;
  let contextSourceTextarea: HTMLTextAreaElement | undefined;
  let contextFormatting: VisualFormatState = {
    block: 'body',
    bold: false,
    italic: false,
    blockquote: false,
    bulletList: false,
    orderedList: false,
    linkHref: '',
    selectionEmpty: true
  };
  let contextAttachmentBusy = false;
  let contextDropActive = false;
  let contextDocumentId = '';
  let contextRefreshSerial = 0;
  let contextEpoch = 0;
  let coWriterOpen = false;
  let coWriterTrigger: HTMLButtonElement | undefined;
  let coWriterPopover: HTMLDivElement | undefined;
  let coWriters: CoWriterSummary[] = [];
  let coWriterProjectId = '';
  let coWriterName = '';
  let coWriterBusy = false;
  let coWriterOperationSerial = 0;
  let coWriterError = '';
  let speechRecording: SpeechRecordingSnapshot | null = null;
  let speechInput: SpeechInputSnapshot | null = null;
  let speechStarting = false;
  let speechError = '';
  let speechPollTimer: number | undefined;
  let speechInsertionAnchor: SpeechInsertionAnchor | null = null;
  let speechRecovery: { transcript: string; target: SpeechInputTarget } | null = null;
  let speechProjectId = '';
  let unlistenNativeAttachmentDrop: (() => void) | undefined;
  let outlineToggle: HTMLButtonElement | undefined;
  const documentContextLongPressMilliseconds = 550;
  const documentContextLongPressSlop = 10;
  let documentContextTarget: CapturedDocumentTarget | null = null;
  let documentContextTrigger: HTMLButtonElement | null = null;
  let documentContextMenu: HTMLDivElement | undefined;
  let documentContextPoint: MenuPoint = { x: 8, y: 8 };
  let documentContextFocusIndex = 0;
  let documentContextActionInFlight = false;
  let documentContextRevealLabel: string | null = null;
  let documentContextLongPressTimer: number | undefined;
  let documentContextLongPress: {
    pointerId: number;
    x: number;
    y: number;
    target: CapturedDocumentTarget;
    trigger: HTMLButtonElement;
  } | null = null;
  let documentContextSuppressClickId: string | null = null;
  let documentContextSuppressClickTimer: number | undefined;
  let retainedRecording: AudioRecording | null = null;
  let spokenAudio: HTMLAudioElement | null = null;
  let spokenAudioUrl: string | null = null;
  let speechPlaybackSerial = 0;
  let speechPlaybackScope = '';
  $: if (speechPlaybackScope !== `${project?.session_id ?? ''}/${document?.summary.document_id ?? ''}`) { speechPlaybackScope = `${project?.session_id ?? ''}/${document?.summary.document_id ?? ''}`; stopReadAloud(); }
  let appearance: AppearancePreference = 'system';
  let systemDark = false;
  let appearanceMedia: MediaQueryList | null = null;
  let models: ModelCapabilitySummary[] = [];
  let curatedModels: CuratedModelCatalogEntry[] = [];
  let curatedModelsLoading = false;
  let curatedModelsError = '';
  let curatedModelsRefreshPromise: Promise<void> | null = null;
  let selectedModelPath = '';
  let compatibleWriterModels: ModelCapabilitySummary[] = [];
  let otherLocalModels: ModelCapabilitySummary[] = [];
  let modelSetupError = '';
  let quietModelLoadFailure: LoomFailure | null = null;
  let modelLoading = false;
  let modelUnloading = false;
  let modelChoosing = false;
  let modelManagerOpen = false;
  let modelManagerPanel: HTMLElement | undefined;
  let modelManagerReturnFocus: HTMLElement | null = null;
  let completionController = initialCompletionControllerState();
  let visualCompletionAccessibility: VisualCompletionAccessibilityWitness =
    unavailableVisualCompletionWitness();
  let visualSelectionAccessibility: VisualSelectionAccessibilityWitness =
    unavailableVisualSelectionWitness();
  let formatMenu: VisualFormatMenu | undefined;
  let visualFormatting: VisualFormatState = {
    block: 'body',
    bold: false,
    italic: false,
    blockquote: false,
    bulletList: false,
    orderedList: false,
    linkHref: '',
    selectionEmpty: true
  };
  let unlistenFileCommands: (() => void) | undefined;
  let unlistenDocumentFilesystemHints: (() => void) | undefined;
  let fileCommandInFlight = false;
  let renamingDocumentId: string | null = null;
  let renameDocumentTitle = '';
  let renameDocumentInput: HTMLInputElement | undefined;
  let renameDocumentTarget: CapturedDocumentTarget | null = null;
  let renameDocumentTrigger: HTMLElement | null = null;
  let renameDocumentInFlight = false;
  let renameDocumentEditorLocked = false;
  const renameDocumentComposition = createDocumentRenameCompositionGuard();
  let deleteDocumentTarget: CapturedDocumentTarget | null = null;
  let deleteDocumentTrigger: HTMLElement | null = null;
  let deleteDocumentCommandId: string | null = null;
  let deleteDocumentDialog: HTMLDivElement | undefined;
  let deleteDocumentCancelButton: HTMLButtonElement | undefined;
  let deleteDocumentInFlight = false;
  let deleteDocumentUncertain = false;
  let deleteDocumentEditorLocked = false;
  let projectFilesystemRefreshTimer: number | undefined;
  let projectFilesystemRefreshInFlight = false;
  let missingDocumentBoundaryInFlight = false;
  let missingDocumentCapturePending: MissingDocumentCaptureIdentity | null = null;
  let projectFilesystemRefreshQueued = false;
  let projectFilesystemRefreshSerial = 0;
  let missingDocumentRecovery: MissingDocumentRecovery | null = null;
  let missingDocumentCopyState: 'idle' | 'copied' | 'failed' = 'idle';
  let appliedNativeTitle = '';
  let suggestionsEnabled = false;
  let suggestionsChanging = false;
  let reportedGenerationFailureRun: string | null = null;
  let contextPresentationFingerprint = '';
  let requireExplicitCompletionFamily = false;
  let suggestionsIdleTimer: number | undefined;
  let suggestionWakeQueued = false;
  let autocompleteRetryLedger: AutocompleteRetryLedger = emptyAutocompleteRetryLedger();
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
  let liveBranchTextByRun: Record<string, string> = {};
  let liveBranchTextSequenceByRun: Record<string, string> = {};
  let branchBodyErrorByRun: Record<string, string> = {};
  let branchRefreshSerial = 0;
  let branchRefreshInFlightCount = 0;
  let branchRefreshQueued = false;
  let sourceTextarea: HTMLTextAreaElement | undefined;
  let weaveStarting = false;
  let uncertainWeave: WeaveCapture | null = null;
  const staleWeaveCleanupTimers = new Set<number>();
  const weaveStatusPollTimers = new Set<number>();
  let branchRefreshTimer: number | undefined;
  let branchPollTimer: number | undefined;
  let branchPollInFlight = false;
  let branchPollAttempt = 0;
  let branchPollEpoch = 0;
  let completionActiveRunIds: string[] = [];
  let authoritativeCompletionFamilyId: string | null = null;
  let cancellingRunIds: string[] = [];
  let cancellationCommandByRun: Record<string, string> = {};
  let promotionArmedCandidateId: string | null = null;
  let promotionInFlight = false;
  let shuttleEnabled = false;
  let shuttleTimer: number | undefined;
  let shuttleTimerKey = '';
  let windowFocused = true;
  let uncertainPromotion: PromotionCapture | null = null;
  let unlistenGenerationEvents: (() => void) | undefined;
  let generationListenerDisposed = false;
  let generationListenerPromise: Promise<void> | null = null;
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
  let visualBoundaryFailure: VisualCaretBoundaryFailure | 'selection_settling' | 'uninitialized' | null = 'uninitialized';
  let visualBoundaryDiagnostic: string | null = null;
  let visualMutationPending = false;
  let sourceCodec: SourceEditorCodec | null = null;
  let mainCompositionActive = false;
  $: compositionActive = mainCompositionActive || Object.values(paneComposing).some(Boolean);
  let sourceComposing = false;
  let visualEditor: {
    captureTerminalSourceRange: () => TerminalSourceRange | null;
    flushPending: () => boolean;
    focusAtDocumentEnd: () => boolean;
    focusCurrentSelection: () => boolean;
    focusPreservingSelection: () => boolean;
    captureFormattingSelection: () => boolean;
    clearFormattingSelection: () => void;
    refreshGhostPresentation: () => boolean;
    applyFormatting: (action: VisualFormatAction, href?: string) => boolean;
    acceptGhostWord: (requireVisible?: boolean) => boolean;
    insertAttachmentMarkdown: (markdown: string, clientX?: number, clientY?: number) => boolean;
    captureAttachmentAnchor: (x: number, y: number) => VisualTextInsertionAnchor | null;
    insertMarkdownAtAnchor: (anchor: VisualTextInsertionAnchor, markdown: string) => boolean;
    insertTextAtSelection: (text: string) => boolean;
    captureTextInsertionAnchor: () => VisualTextInsertionAnchor | null;
    insertTextAtAnchor: (anchor: VisualTextInsertionAnchor, text: string) => boolean;
  } | null = null;
  let sourceEditor: {
    applyRemoteValue: (text: string) => void;
    focusAtDocumentEnd: () => boolean;
    focusCurrentSelection: () => boolean;
    acceptGhostWord: (requireVisible?: boolean) => boolean;
    insertAttachmentMarkdown: (markdown: string) => boolean;
    insertTextAtSelection: (text: string) => boolean;
    captureTextInsertionAnchor: () => SourceTextInsertionAnchor | null;
    insertTextAtAnchor: (anchor: SourceTextInsertionAnchor, text: string) => boolean;
  } | null = null;
  let componentMounted = false;
  let nativeFullscreen = false;
  let stopNativeFullscreenObservation: (() => void) | undefined;
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
  let pendingCloseInlineSuggestionsEnabled: boolean | null = null;
  let pendingCloseShuttleEnabled: boolean | null = null;
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

  interface MissingDocumentRecovery {
    projectId: string;
    sessionId: string;
    documentId: string;
    relativePath: string;
    title: string;
    text: string;
    hadUnsavedText: boolean;
    journalDurable: boolean;
    sourceRevisionId: string | null;
    visibleBlobId: string;
    draftVersion: string;
    draftWasUncertain: boolean;
    saveWasUncertain: boolean;
  }

  type MissingDocumentRecoveryBoundaryResult =
    | { readonly kind: 'ready'; readonly project: ProjectSnapshot }
    | {
        readonly kind: 'deferred';
        readonly reason:
          | 'live_document_changed'
          | 'editor_not_flushable'
          | 'workspace_changed'
          | 'document_reappeared';
      };

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
    contextEpoch: number;
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

  const detachedProjectCloseCoordinator = new DetachedProjectCloseCoordinator({
    currentProject: currentProjectSession,
    disableAutomation: (projectId, sessionId) => setSuggestionsPolicy(
      projectId,
      sessionId,
      false
    ),
    closeProject: closeProjectSession,
    newCommandId: newUlid,
    normalizeFailure
  });

  const applicationCloseCoordinator = new ApplicationCloseCoordinator({
    begin: () => {
      applicationClosePhase = 'closing';
      clearDocumentContextLongPress();
      closeDocumentContextMenu(false);
      closeDocumentDeleteConfirmation(false);
      clearPreferredWriterRequest();
      cancelSuggestionTimer();
      if (
        missingDocumentRecovery &&
        !missingDocumentRecovery.journalDurable &&
        missingDocumentCopyState !== 'copied'
      ) {
        recordLocalFailure(
          'missing_document_recovery_not_durable',
          'Copy the preserved missing-document text before closing Loom; its draft durability is not confirmed.'
        );
        announce(errorMessage);
        return false;
      }
      if (missingDocumentCapturePending) {
        recordLocalFailure(
          'missing_document_capture_pending',
          'Wait for Loom to preserve the newly missing manuscript before closing.'
        );
        announce(errorMessage);
        return false;
      }
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
    closeProject: async () => {
      try { await signalDraftEditor.flush(); }
      catch (failure) { recordFailure(failure); signalOpen = true; return { status: 'resume' }; }
      return project ? closeProject() : detachedProjectCloseCoordinator.close();
    },
    authorizeNativeClose: requestApplicationClose,
    abortNativeClose: abortApplicationClose,
    reset: () => {
      detachedProjectCloseCoordinator.reset();
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
  function completionAutomationEnabled(
    autocomplete = suggestionsEnabled,
    shuttle = shuttleEnabled
  ): boolean {
    return completionEngineEnabled({ autocomplete, shuttle });
  }

  $: completionSession = completionController.session;
  $: pendingCompletionText = completionController.pendingText;
  $: completionGenerationIntent = completionController.generationIntent;
  $: dismissedCandidateIds = completionController.dismissedCandidateIds;
  $: unpresentableVisualGhostPresentationKeys = completionController.unpresentableVisualKeys;
  $: scheduledSuggestion = completionController.scheduled;

  $: terminalBusy = terminalDispatching || terminalPendingRequest !== null || terminalRuns.some((run) => run.status === 'running') || Object.values(paneBusy).some(Boolean);
  $: if (terminalSessionKey !== `${project?.project_id ?? ''}:${project?.session_id ?? ''}`) {
    terminalSessionKey = `${project?.project_id ?? ''}:${project?.session_id ?? ''}`;
    terminalRefreshSerial += 1;
    terminalRuns = [];
    terminalEntry = '';
    terminalError = '';
    terminalDispatching = false;
    terminalCancelRequested = false;
    cancelledTerminalIds = new Set();
    terminalPendingRequest = null;
    terminalOpen = false;
    if (terminalPollTimer !== undefined) window.clearTimeout(terminalPollTimer);
    terminalPollTimer = undefined;
    if (desktop && project) void refreshTerminalRuns();
  }

  $: folderWarnings = project?.folder_warnings ?? [];
  $: fileRows = workspaceRows(project?.documents ?? [], collapsedFolders, search);
  $: loadedModel = models.find((model) => model.loaded) ?? null;
  $: currentModel = workspaceWriterModel(models, buildModelPolicy, curatedModels, workspaceTemplate, workspaceTemplateScope === `${project?.project_id}/${project?.session_id}`);
  $: suggestionSetupNeeded = Boolean(
    project &&
    document &&
    completionAutomationEnabled() &&
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
  $: availableWriterModels = orderedLocalTextModels(models, loadLastLocalModelPath());
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
  $: completionContextKey = project && document
    ? `${completionSessionContextKey(
        project.session_id,
        document.summary.document_id,
        documentEpoch,
        mode
      )}:context-${contextEpoch}`
    : '';
  $: visualGhostSurfaceKey = project && document
    ? `${project.session_id}:${document.summary.document_id}:${document.summary.revision_id}:${document.visible_blob_id}:${documentEpoch}:visual`
    : '';
  $: sourceGhostTargetByte = sourceGhostTargetByteFor(
    mode,
    Boolean(sourceTextarea),
    sourceSelectionStart,
    sourceSelectionEnd,
    sourceDisplayText,
    document,
    documentText,
    sourceCodec
  );
  $: sourceGhostNewline = sourceCodec?.newline ?? null;
  $: visualSuggestionFamily = inlineSuggestionFamily(visualGhostTargetByte, 'visual', {
    branches,
    authoritativeFamilyId: authoritativeCompletionFamilyId,
    requireExplicitFamily: requireExplicitCompletionFamily,
    verifiedBodyByRun: verifiedBranchBodyByRun,
    liveTextByRun: liveBranchTextByRun,
    liveTextSequenceByRun: liveBranchTextSequenceByRun,
    currentModel,
    document,
    suggestionsEnabled: completionAutomationEnabled(),
    promotionReady: branchPromotionReady,
    dismissedCandidateIds,
    unpresentableVisualKeys: unpresentableVisualGhostPresentationKeys,
    manuscriptText: documentText,
    sourceNewline: sourceGhostNewline
  });
  $: sourceSuggestionFamily = inlineSuggestionFamily(sourceGhostTargetByte, 'source', {
    branches,
    authoritativeFamilyId: authoritativeCompletionFamilyId,
    requireExplicitFamily: requireExplicitCompletionFamily,
    verifiedBodyByRun: verifiedBranchBodyByRun,
    liveTextByRun: liveBranchTextByRun,
    liveTextSequenceByRun: liveBranchTextSequenceByRun,
    currentModel,
    document,
    suggestionsEnabled: completionAutomationEnabled(),
    promotionReady: branchPromotionReady,
    dismissedCandidateIds,
    unpresentableVisualKeys: unpresentableVisualGhostPresentationKeys,
    manuscriptText: sourceDisplayText,
    sourceNewline: sourceGhostNewline
  });
  $: baseSuggestionFamily = mode === 'visual'
    ? visualSuggestionFamily
    : mode === 'source'
      ? sourceSuggestionFamily
      : [];
  $: reconcileVisibleCompletionController(completionContextKey, baseSuggestionFamily);
  $: completionView = completionControllerView(
    completionController,
    completionContextKey,
    baseSuggestionFamily
  );
  $: boundCompletionSession = completionView.boundSession;
  $: if (boundCompletionSession) {
    const selected = selectedCompletionCandidate(boundCompletionSession);
    const branch = selected
      ? branches.find((candidate) => candidate.run_id === selected.runId)
      : null;
    const verified = selected && branch
      ? verifiedGhostSuggestion(branch, verifiedBranchBodyByRun[selected.runId])
      : null;
    const liveText = selected ? liveBranchTextByRun[selected.runId] : undefined;
    const liveSequence = selected ? liveBranchTextSequenceByRun[selected.runId] : undefined;
    const hasLiveProjection = liveText !== undefined && liveSequence !== undefined;
    const rawText = selected && branch
      ? verified?.text ?? (hasLiveProjection ? liveText : branch.text)
      : '';
    const text = selected
      ? projectInlineCandidateText(
          selected.targetByte,
          mode,
          mode === 'visual' ? documentText : sourceDisplayText,
          rawText,
          sourceGhostNewline
        )
      : null;
    if (selected && text && candidateTextIsSurfaceable(text)) {
      const rawPresentationKey = verified?.presentationKey ??
        (hasLiveProjection
          ? `stream:${selected.runId}:${liveSequence}`
          : `branch:${branch?.branch_id ?? selected.runId}`);
      refreshVisibleCompletionCandidate(
        boundCompletionSession,
        selected.runId,
        text,
        projectedInlinePresentationKey(rawPresentationKey, rawText, text)
      );
    }
  }
  $: activeSuggestionFamily = completionView.activeFamily;
  $: selectedInlineSuggestion = completionView.selected;
  $: ghostSuggestion = mode === 'visual' ? selectedInlineSuggestion : null;
  $: sourceGhostSuggestion = mode === 'source' ? selectedInlineSuggestion : null;
  $: ghostAlternatives = completionView.alternatives;
  $: ghostUnconsumeText = completionView.unconsumeText;
  $: activeGhostSuggestion = mode === 'visual'
    ? ghostSuggestion?.presentationKey === visibleVisualGhostPresentationKey
      ? ghostSuggestion
      : null
    : mode === 'source'
      ? sourceGhostSuggestion?.presentationKey === visibleSourceGhostPresentationKey
        ? sourceGhostSuggestion
        : null
      : null;
  $: completionWitnessSelected = completionView.witnessSelected;
  $: completionAccessibilityWitness = JSON.stringify({
    schema: 'delysis.loom-completion-witness.v1',
    mode,
    context_key: boundCompletionSession?.contextKey ?? '',
    session_cached: Boolean(boundCompletionSession),
    family_count: boundCompletionSession?.candidates.length ?? 0,
    candidates: boundCompletionSession?.candidates.map((candidate) => ({
      candidate_id: candidate.candidateId,
      presentation_key: candidate.presentationKey,
      run_id: candidate.runId,
      target_byte: candidate.targetByte,
      text_utf8_bytes: new TextEncoder().encode(candidate.text).byteLength
    })) ?? [],
    selected_run_id: completionWitnessSelected?.runId ?? '',
    selected_candidate_id: completionWitnessSelected?.candidateId ?? '',
    selected_presentation_key: completionWitnessSelected?.presentationKey ?? '',
    rendered_presentation_key: selectedInlineSuggestion?.presentationKey ?? '',
    accepted_chunk_count: boundCompletionSession?.acceptedChunks.length ?? 0,
    authority_frozen: boundCompletionSession?.authorityFrozen ?? false,
    accepted_utf8_bytes: boundCompletionSession
      ? new TextEncoder().encode(acceptedCompletionText(boundCompletionSession)).byteLength
      : 0,
    autocomplete_enabled: suggestionsEnabled,
    shuttle_enabled: shuttleEnabled,
    inline_hidden_requested: inlineGhostHidden({
      autocomplete: suggestionsEnabled,
      shuttle: shuttleEnabled
    }),
    inline_visible_key: mode === 'visual'
      ? visibleVisualGhostPresentationKey
      : mode === 'source'
        ? visibleSourceGhostPresentationKey
        : '',
    visual: visualCompletionAccessibility,
    editor_selection: {
      available: visualSelectionAccessibility.available,
      epoch: visualSelectionAccessibility.epoch,
      selection_kind: visualSelectionAccessibility.selectionKind,
      from: visualSelectionAccessibility.from,
      to: visualSelectionAccessibility.to,
      empty: visualSelectionAccessibility.empty,
      all_visible_text: visualSelectionAccessibility.allVisibleText,
      caret_at_end: visualSelectionAccessibility.caretAtEnd,
      caret_byte_offset: visualSelectionAccessibility.caretByteOffset
    },
    last_action: completionController.lastAction
  });
  $: completionExhaustionKey = boundCompletionSession && completionShouldRequestNextBatch(
    boundCompletionSession,
    pendingCompletionText !== null,
    branches.find((branch) => branch.run_id === boundCompletionSession.selectedRunId)?.status === 'ready'
  )
      ? `${boundCompletionSession.contextKey}:${boundCompletionSession.selectedRunId}:${acceptedCompletionText(boundCompletionSession).length}`
      : '';
  $: finishCompletionIfExhausted(completionExhaustionKey);
  $: visualAutocompleteDisposition = autocompleteDisposition({
    active: mode === 'visual' && completionAutomationEnabled() && !visualMutationPending && branchPromotionReady,
    branches: currentReadyBranches,
    verifiedBodyByRun: verifiedBranchBodyByRun,
    dismissedCandidateIds,
    unpresentablePresentationKeys: unpresentableVisualGhostPresentationKeys,
    targetByte: visualGhostTargetByte,
    presentationCompatible: visualGhostTextMayBePlainProse
  });
  $: sourceAutocompleteDisposition = autocompleteDisposition({
    active: mode === 'source' && completionAutomationEnabled() && !sourceDirty && !compositionActive && branchPromotionReady,
    branches: currentReadyBranches,
    verifiedBodyByRun: verifiedBranchBodyByRun,
    dismissedCandidateIds,
    unpresentablePresentationKeys: [],
    targetByte: sourceGhostTargetByte,
    presentationCompatible: (text) =>
      sourceGhostPresentationCompatible(sourceDisplayText, text, sourceGhostNewline)
  });
  $: suggestionMenuState = suggestionsChanging
    ? '…'
    : modelLoading || modelChoosing || modelUnloading || modelDownloadStarting || activeModelDownloads.length > 0
      ? 'Preparing'
      : suggestionsEnabled && quietModelLoadFailure
        ? 'Needs attention'
        : !suggestionsEnabled
          ? 'Off'
          : currentModel
            ? 'Ready'
            : 'Set up';
  $: nativeWindowTitle = document?.summary.title ?? project?.title ?? 'Loom';
  $: resolvedAppearance = resolveAppearance(appearance, systemDark);
  $: if (desktop) void syncNativeWindowTitle(nativeWindowTitle);
  $: if (componentMounted) {
    window.document.documentElement.dataset.theme = resolvedAppearance;
    window.document.documentElement.style.colorScheme = resolvedAppearance;
  }
  $: if (
    suggestionsEnabled &&
    !shuttleEnabled &&
    activeGhostSuggestion &&
    activeGhostSuggestion.presentationKey !== announcedGhostPresentationKey
  ) {
    announcedGhostPresentationKey = activeGhostSuggestion.presentationKey;
    announce('Suggestion available. Tab accepts all; Option Right accepts one word; Option Up or Down switches.');
  }
  $: shuttleCandidate = shuttleEnabled ? selectedInlineSuggestion : activeGhostSuggestion;
  $: shuttleScheduleKey = completionShuttleScheduleKey(
    shuttleEnabled && !terminalBusy,
    windowFocused,
    shuttleCandidate,
    boundCompletionSession?.acceptedChunks.length ?? 0,
    editVersion,
    mode
  );
  $: syncShuttleTimer(shuttleScheduleKey);
  $: automaticBoundaryIsExact = mode === 'visual'
    ? visualSelectionByte !== null
    : mode === 'source' && Boolean(sourceTextarea) && sourceSelectionStart === sourceSelectionEnd;
  $: canUseVisual = Boolean(
    document?.summary.kind === 'prose' && canUseVisualMarkdown(documentText, mode === 'visual')
  );
  $: weaveCursorAtStart = mode === 'source'
    ? sourceSelectionStart === 0
    : visualSelectionByte === 0;
  $: completionLifecycle = automaticCompletionLifecycle({
    desktop,
    automationEnabled: completionAutomationEnabled(),
    projectAvailable: Boolean(project),
    documentAvailable: Boolean(document),
    hybridDocument: document?.summary.kind === 'hybrid',
    intentArmed: completionGenerationIsArmed(
      completionGenerationIntent,
      completionContextKey,
      editVersion
    ),
    modelAvailable: Boolean(currentModel),
    modelTransitioning: modelLoading || modelUnloading,
    compositionActive,
    visualMutationPending,
    sourceDirty,
    savePending: Boolean(saveInFlight) || saveState === 'dirty' || saveState === 'saving',
    weavePending: weaveStarting,
    promotionPending: promotionInFlight,
    workspaceIdle: transition === 'idle',
    editorReadonly,
    recoveryPending: Boolean(
      staleDraft || uncertainDraft || uncertainSave || reconciliation || uncertainWeave || uncertainPromotion
    ),
    saveSettled: editVersion === savedVersion && (saveState === 'clean' || saveState === 'saved'),
    revisionAvailable: Boolean(document?.summary.revision_id),
    visibleBlobCurrent: Boolean(
      document && document.summary.active_blob_id === document.visible_blob_id
    ),
    caretExact: automaticBoundaryIsExact,
    caretAtStart: weaveCursorAtStart,
    activeBranchCount
  });
  $: canStartAutomaticSuggestions = completionLifecycle.phase === 'ready';
  $: completionLifecycleHelp = suggestionsChanging
    ? 'Autocomplete setting is changing'
    : completionLifecycle.reason === 'caret_unmapped' && mode === 'visual'
      ? `${completionLifecycleDescription(completionLifecycle)} Boundary proof: ${visualBoundaryFailure ?? 'unknown'}${visualBoundaryDiagnostic ? ` (${visualBoundaryDiagnostic})` : ''}.`
      : completionLifecycleDescription(completionLifecycle);
  $: completionSchedulerWakeKey = scheduledSuggestion
    ? `${completionContextKey}:${editVersion}:${completionLifecycle.phase}:${completionLifecycle.reason ?? 'ready'}`
    : '';
  $: resumeScheduledAutomaticSuggestion(completionSchedulerWakeKey);
  $: retryEvaluationSnapshot = {
    enabled: desktop &&
      branchPromotionReady &&
      completionAutomationEnabled() &&
      Boolean(currentModel) &&
      activeBranchCount === 0 &&
      completionGenerationIsArmed(completionGenerationIntent, completionContextKey, editVersion),
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
  $: editorNavigationLocked = cabalAttaching || transition !== 'idle' || renameDocumentEditorLocked || deleteDocumentEditorLocked || missingDocumentBoundaryInFlight || missingDocumentCapturePending !== null || staleDraft !== null || staleDraftRestoring || uncertainDraft !== null || uncertainSave !== null || reconciliation !== null || promotionInFlight || uncertainPromotion !== null;
  $: editorReadonly = editorNavigationLocked || cabalWriteBlocked;
  $: reconciliationResolutionLocked = reconciliationApplying || pendingReconciliationApply !== null;
  $: reconciliationResolutionIsExact = Boolean(
    reconciliation && (
      reconciliation.kind === 'prose' ||
      reconciliationResolution === reconciliation.app_text ||
      reconciliationResolution === reconciliation.external_text ||
      (reconciliation.outcome.status === 'merged' && reconciliationResolution === reconciliation.outcome.content)
    )
  );

  $: if ((document?.summary.document_id ?? '') !== contextDocumentId) {
    contextDocumentId = document?.summary.document_id ?? '';
    contextAttachments = [];
    contextTextSources = [];
    contextText = '';
    persistedContextText = '';
    contextTextSaveState = 'clean';
    contextCompositionActive = false;
    contextVisualEditor = null;
    contextSourceTextarea = undefined;
    speechInsertionAnchor = null;
    speechRecovery = null;
    contextFormatting = {
      block: 'body',
      bold: false,
      italic: false,
      blockquote: false,
      bulletList: false,
      orderedList: false,
      linkHref: '',
      selectionEmpty: true
    };
    contextEpoch += 1;
    if (desktop && project && document) void refreshDocumentContext();
  }

  $: if ((project?.project_id ?? '') !== coWriterProjectId) {
    coWriterProjectId = project?.project_id ?? '';
    coWriters = [];
    coWriterOpen = false;
    coWriterName = '';
    coWriterError = '';
    speechInsertionAnchor = null;
    speechRecovery = null;
  }

  $: if ((project?.project_id ?? '') !== speechProjectId) {
    speechProjectId = project?.project_id ?? '';
    if (speechPollTimer !== undefined) window.clearTimeout(speechPollTimer);
    speechPollTimer = undefined;
    speechRecording = null;
    speechInput = null;
    speechInsertionAnchor = null;
    speechRecovery = null;
    speechStarting = false;
    speechError = '';
  }

  async function refreshDocumentContext(): Promise<void> {
    if (!desktop || !project || !document) return;
    const serial = ++contextRefreshSerial;
    const captured = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id
    };
    try {
      const snapshot = await listDocumentContext(
        captured.projectId,
        captured.sessionId,
        captured.documentId
      );
      if (
        serial === contextRefreshSerial &&
        project?.project_id === captured.projectId &&
        project.session_id === captured.sessionId &&
        document?.summary.document_id === captured.documentId
      ) {
        updateContextPresentation(snapshot);
        if (contextTextSaveState === 'clean') {
          contextText = snapshot.markdown;
          persistedContextText = snapshot.markdown;
        }
      }
    } catch (error) {
      if (serial === contextRefreshSerial) recordFailure(error);
    }
  }

  function updateContextPresentation(snapshot: DocumentContextSnapshot): void {
    const fingerprint = JSON.stringify([contextDocumentId, snapshot]);
    contextAttachments = snapshot.attachments;
    contextTextSources = snapshot.text_sources;
    if (fingerprint === contextPresentationFingerprint) return;
    contextPresentationFingerprint = fingerprint;
    authoritativeCompletionFamilyId = null;
    requireExplicitCompletionFamily = true;
    contextEpoch += 1;
    invalidateCompletionForCaretNavigation();
    if (completionAutomationEnabled()) scheduleAutomaticSuggestions(editVersion, 0);
  }

  function contextMediaUrl(media: ContextMediaPresentation): string | null {
    return media.preview_token ? convertFileSrc(media.preview_token, 'loom-asset') : null;
  }

  async function refreshCoWriters(): Promise<void> {
    if (!desktop || !project) return;
    const captured = { projectId: project.project_id, sessionId: project.session_id };
    const serial = ++coWriterOperationSerial;
    coWriterBusy = true;
    coWriterError = '';
    try {
      const summaries = await listCoWriters(captured.projectId, captured.sessionId);
      if (
        project?.project_id === captured.projectId &&
        project.session_id === captured.sessionId &&
        serial === coWriterOperationSerial
      ) coWriters = summaries;
    } catch (error) {
      if (
        serial === coWriterOperationSerial &&
        project?.project_id === captured.projectId &&
        project.session_id === captured.sessionId
      ) coWriterError = normalizeFailure(error).message;
    } finally {
      if (serial === coWriterOperationSerial) coWriterBusy = false;
    }
  }

  function toggleCoWriter(): void {
    coWriterOpen = !coWriterOpen;
    coWriterError = '';
    if (coWriterOpen) void refreshCoWriters();
  }

  async function saveCurrentCoWriter(): Promise<void> {
    if (!project || !document || coWriterBusy || !coWriterName.trim()) return;
    const captured = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id
    };
    const serial = ++coWriterOperationSerial;
    coWriterBusy = true;
    coWriterError = '';
    try {
      if (!await persistCurrentContextText()) return;
      if (
        project?.project_id !== captured.projectId ||
        project.session_id !== captured.sessionId ||
        document?.summary.document_id !== captured.documentId
      ) return;
      const saved = await saveCoWriter(
        captured.projectId,
        captured.sessionId,
        captured.documentId,
        coWriterName
      );
      if (
        serial !== coWriterOperationSerial ||
        project?.project_id !== captured.projectId ||
        project.session_id !== captured.sessionId ||
        document?.summary.document_id !== captured.documentId
      ) return;
      coWriters = [saved, ...coWriters.filter((candidate) => candidate.id !== saved.id)];
      coWriterName = '';
      announce(`${saved.name} saved as a co-writer`);
    } catch (error) {
      if (serial === coWriterOperationSerial) coWriterError = normalizeFailure(error).message;
    } finally {
      if (serial === coWriterOperationSerial) coWriterBusy = false;
    }
  }

  async function applySelectedCoWriter(profile: CoWriterSummary): Promise<void> {
    if (!project || !document || coWriterBusy) return;
    const captured = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id
    };
    const serial = ++coWriterOperationSerial;
    coWriterBusy = true;
    coWriterError = '';
    try {
      if (!await persistCurrentContextText()) return;
      if (
        project?.project_id !== captured.projectId ||
        project.session_id !== captured.sessionId ||
        document?.summary.document_id !== captured.documentId
      ) return;
      const snapshot = await applyCoWriter(
        captured.projectId,
        captured.sessionId,
        captured.documentId,
        profile.id
      );
      if (serial !== coWriterOperationSerial) return;
      if (adoptAuthoritativeContext(
        snapshot,
        captured.projectId,
        captured.sessionId,
        captured.documentId
      )) {
        contextPaneOpen = true;
        coWriterOpen = false;
        announce(`${profile.name} is steering this manuscript`);
        await tick();
        focusContextEditorAtEnd();
      }
    } catch (error) {
      if (serial === coWriterOperationSerial) coWriterError = normalizeFailure(error).message;
    } finally {
      if (serial === coWriterOperationSerial) coWriterBusy = false;
    }
  }

  async function removeCoWriter(profile: CoWriterSummary): Promise<void> {
    if (!project || coWriterBusy) return;
    const captured = { projectId: project.project_id, sessionId: project.session_id };
    const serial = ++coWriterOperationSerial;
    coWriterBusy = true;
    coWriterError = '';
    try {
      const summaries = await deleteCoWriter(captured.projectId, captured.sessionId, profile.id);
      if (
        serial !== coWriterOperationSerial ||
        project?.project_id !== captured.projectId ||
        project.session_id !== captured.sessionId
      ) return;
      coWriters = summaries;
      announce(`${profile.name} removed from co-writers`);
    } catch (error) {
      if (serial === coWriterOperationSerial) coWriterError = normalizeFailure(error).message;
    } finally {
      if (serial === coWriterOperationSerial) coWriterBusy = false;
    }
  }

  function speechInputActive(): boolean {
    return Boolean(speechRecording || speechInput);
  }

  function speechTarget(): SpeechInputTarget {
    return contextPaneOpen ? focusedSpeechTarget : 'manuscript';
  }

  function rememberSpeechEditor(event: FocusEvent, target: SpeechInputTarget): void {
    if (event.target instanceof HTMLElement && event.target.closest('[contenteditable="true"], textarea')) {
      focusedSpeechTarget = target;
    }
  }

  function captureSpeechInsertionAnchor(target: SpeechInputTarget): SpeechInsertionAnchor | null {
    if (target === 'context') {
      if (mode === 'visual') {
        const anchor = contextVisualEditor?.captureTextInsertionAnchor() ?? null;
        return anchor ? { kind: 'visual', target, anchor } : null;
      }
      const textarea = contextSourceTextarea;
      return textarea && !textarea.disabled
        ? {
            kind: 'context-source',
            target,
            value: textarea.value,
            start: textarea.selectionStart,
            end: textarea.selectionEnd
          }
        : null;
    }
    if (mode === 'visual') {
      const anchor = visualEditor?.captureTextInsertionAnchor() ?? null;
      return anchor ? { kind: 'visual', target, anchor } : null;
    }
    const anchor = sourceEditor?.captureTextInsertionAnchor() ?? null;
    return anchor ? { kind: 'source', target, anchor } : null;
  }

  function insertSpeechTranscript(snapshot: SpeechInputSnapshot): boolean {
    if (!snapshot.transcript) return false;
    const insertion = speechInsertionAnchor;
    if (!insertion || insertion.target !== snapshot.target) return false;
    if (insertion.kind === 'visual') {
      const editor = insertion.target === 'context' ? contextVisualEditor : visualEditor;
      return editor?.insertTextAtAnchor(insertion.anchor, snapshot.transcript) ?? false;
    }
    if (insertion.kind === 'context-source') {
      if (!contextPaneOpen || mode !== 'source') return false;
      const textarea = contextSourceTextarea;
      if (!textarea || textarea.disabled || textarea.value !== insertion.value) return false;
      textarea.setRangeText(
        snapshot.transcript,
        insertion.start,
        insertion.end,
        'end'
      );
      updateContextText(textarea.value);
      textarea.focus({ preventScroll: true });
      return true;
    }
    return mode === 'source'
      ? sourceEditor?.insertTextAtAnchor(insertion.anchor, snapshot.transcript) ?? false
      : false;
  }

  function insertRecoveredSpeech(): void {
    const recovery = speechRecovery;
    if (!recovery) return;
    const inserted = recovery.target === 'context'
      ? mode === 'visual'
        ? contextVisualEditor?.insertTextAtSelection(recovery.transcript) ?? false
        : (() => {
            const textarea = contextSourceTextarea;
            if (!textarea || textarea.disabled) return false;
            textarea.setRangeText(
              recovery.transcript,
              textarea.selectionStart,
              textarea.selectionEnd,
              'end'
            );
            updateContextText(textarea.value);
            textarea.focus({ preventScroll: true });
            return true;
          })()
      : mode === 'visual'
        ? visualEditor?.insertTextAtSelection(recovery.transcript) ?? false
        : sourceEditor?.insertTextAtSelection(recovery.transcript) ?? false;
    if (inserted) {
      speechRecovery = null;
      speechError = '';
      announce('Recovered dictation inserted at the current caret');
    }
  }

  function scheduleSpeechStatusPoll(snapshot: SpeechInputSnapshot): void {
    if (speechPollTimer !== undefined) window.clearTimeout(speechPollTimer);
    speechPollTimer = window.setTimeout(() => {
      speechPollTimer = undefined;
      void pollSpeechStatus(snapshot);
    }, 350);
  }

  async function acceptSpeechSnapshot(snapshot: SpeechInputSnapshot): Promise<void> {
    if (speechInput?.request_id !== snapshot.request_id) return;
    speechInput = snapshot;
    if (snapshot.phase === 'transcribing' || snapshot.phase === 'cancel_requested') {
      scheduleSpeechStatusPoll(snapshot);
      return;
    }
    if (snapshot.phase === 'completed') {
      const inScope =
        project?.project_id === snapshot.project_id &&
        project.session_id === snapshot.session_id &&
        document?.summary.document_id === snapshot.document_id;
      if (!inScope || !insertSpeechTranscript(snapshot)) {
        speechRecovery = snapshot.transcript
          ? { transcript: snapshot.transcript, target: snapshot.target }
          : null;
        speechError = snapshot.transcript
          ? 'Dictation finished after its insertion point changed. The transcript is preserved for recovery.'
          : 'Dictation finished without any text.';
        announce(speechError);
      } else {
        speechRecovery = null;
        speechError = '';
        announce('Dictation inserted');
      }
    } else if (snapshot.phase === 'failed') {
      speechError = snapshot.error_message ?? 'Local speech recognition failed.';
      recordLocalFailure(snapshot.error_code ?? 'speech_input_failed', speechError);
      announce('Dictation needs attention');
    } else {
      announce('Dictation cancelled');
    }
    speechInsertionAnchor = null;
    speechInput = null;
  }

  async function pollSpeechStatus(expected: SpeechInputSnapshot): Promise<void> {
    if (speechInput?.request_id !== expected.request_id) return;
    try {
      const snapshot = await getSpeechInputStatus(
        expected.project_id,
        expected.session_id,
        expected.request_id
      );
      await acceptSpeechSnapshot(snapshot);
    } catch (error) {
      if (speechInput?.request_id !== expected.request_id) return;
      speechError = normalizeFailure(error).message;
      recordFailure(error);
      speechInput = null;
    }
  }

  async function beginSpeechRecording(): Promise<void> {
    if (!desktop || !project || !document || speechStarting || speechInputActive()) return;
    const captured = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id,
      target: speechTarget()
    };
    const insertion = captureSpeechInsertionAnchor('manuscript');
    speechStarting = true;
    speechError = '';
    try {
      const recording = await startAudioRecording(captured.projectId, captured.sessionId, captured.documentId);
      const inScope =
        project?.project_id === captured.projectId &&
        project.session_id === captured.sessionId &&
        document?.summary.document_id === captured.documentId;
      if (!inScope) {
        await cancelSpeechRecording(
          recording.project_id,
          recording.session_id,
          recording.recording_id
        );
        return;
      }
      speechRecording = recording;
      speechInsertionAnchor = insertion;
      announce('Recording audio locally');
    } catch (error) {
      speechRecording = null;
      speechInput = null;
      speechInsertionAnchor = null;
      speechError = normalizeFailure(error).message;
      recordFailure(error);
      announce('Loom could not start the microphone');
    } finally {
      speechStarting = false;
    }
  }

  async function finishSpeechRecording(): Promise<void> {
    const recording = speechRecording;
    if (!recording || speechStarting) return;
    speechStarting = true;
    try {
      const captured = retainedRecording ?? await stopAudioRecording(recording.project_id, recording.session_id, recording.recording_id);
      retainedRecording = captured;
      const snapshot = await addDocumentContexts(recording.project_id, recording.session_id, captured.document_id, [captured.attachment.id]);
      adoptAuthoritativeContext(snapshot, recording.project_id, recording.session_id, captured.document_id);
      const markdown = captured.attachment.media_markdown;
      const insertion = speechInsertionAnchor;
      if (markdown && insertion && project?.project_id === recording.project_id && document?.summary.document_id === captured.document_id) {
        if (insertion.kind === 'visual') visualEditor?.insertMarkdownAtAnchor(insertion.anchor, markdown);
        else if (insertion.kind === 'source') sourceEditor?.insertTextAtAnchor(insertion.anchor, markdown);
      }
      speechInsertionAnchor = null;
      speechRecording = null;
      retainedRecording = null;
      speechError = '';
      announce(captured.activity.limit_reached ? 'Five minutes of audio retained and attached to this document' : captured.activity.signal_detected ? 'Audio retained and attached to this document' : 'Quiet recording retained and attached to this document');
    } catch (error) {
      speechError = `${normalizeFailure(error).message} Click the microphone to retry saving this recording.`;
      recordFailure(error);
      announce(speechError);
    } finally {
      speechStarting = false;
    }
  }

  async function cancelActiveSpeech(): Promise<void> {
    if (speechStarting) return;
    speechStarting = true;
    try {
      if (speechRecording && (retainedRecording || speechError)) {
        speechStarting = false;
        await finishSpeechRecording();
        return;
      }
      if (speechRecording) {
        const recording = speechRecording;
        await cancelSpeechRecording(
          recording.project_id,
          recording.session_id,
          recording.recording_id
        );
        speechRecording = null;
        speechInsertionAnchor = null;
      } else if (speechInput) {
        const snapshot = await cancelSpeechInput(
          speechInput.project_id,
          speechInput.session_id,
          speechInput.request_id
        );
        await acceptSpeechSnapshot(snapshot);
      }
      announce('Dictation cancellation requested');
    } catch (error) {
      speechError = normalizeFailure(error).message;
      recordFailure(error);
    } finally {
      speechStarting = false;
    }
  }

  function toggleSpeechInput(): void {
    if (speechRecording) void finishSpeechRecording();
    else if (speechInput) void cancelActiveSpeech();
    else void beginSpeechRecording();
  }

  function scheduleContextTextSave(delay = 300): void {
    if (!project || !document) return;
    if (contextTextSaveTimer !== undefined) window.clearTimeout(contextTextSaveTimer);
    const captured = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id,
      text: contextText,
      attachmentIds: contextAttachments.map((attachment) => attachment.id)
    };
    contextTextSaveTimer = window.setTimeout(() => {
      contextTextSaveTimer = undefined;
      void enqueueContextTextPersistence(captured);
    }, delay);
  }

  async function persistContextText(captured: {
    projectId: string;
    sessionId: string;
    documentId: string;
    text: string;
    attachmentIds: string[];
  }): Promise<boolean> {
    if (
      captured.text === persistedContextText &&
      captured.documentId === contextDocumentId
    ) {
      contextTextSaveState = 'clean';
      return true;
    }
    if (captured.documentId === contextDocumentId) contextTextSaveState = 'saving';
    try {
      const snapshot = await setDocumentContextSnapshot(
        captured.projectId,
        captured.sessionId,
        captured.documentId,
        captured.text,
        captured.attachmentIds
      );
      if (
        project?.project_id === captured.projectId &&
        project.session_id === captured.sessionId &&
        document?.summary.document_id === captured.documentId
      ) {
        updateContextPresentation(snapshot);
        persistedContextText = snapshot.markdown;
        if (contextText === snapshot.markdown) {
          contextTextSaveState = 'clean';
          contextEpoch += 1;
          if (completionAutomationEnabled()) scheduleAutomaticSuggestions(editVersion, 0);
        } else {
          // A newer edit (including reverting an in-flight save) must be sent
          // after the now-authoritative reply. Never label stale backend text
          // clean merely because it matched the value before this request.
          contextTextSaveState = 'dirty';
          scheduleContextTextSave(0);
        }
      }
      return true;
    } catch (error) {
      if (captured.documentId === contextDocumentId) contextTextSaveState = 'error';
      recordFailure(error);
      return false;
    }
  }

  function enqueueContextTextPersistence(captured: {
    projectId: string;
    sessionId: string;
    documentId: string;
    text: string;
    attachmentIds: string[];
  }): Promise<boolean> {
    const persistence = contextTextSaveQueue.then(() => persistContextText(captured));
    contextTextSaveQueue = persistence.then(() => undefined, () => undefined);
    return persistence;
  }

  function updateContextText(value: string): void {
    if (new TextEncoder().encode(value).byteLength > 256 * 1024) {
      announce('Completion context is limited to 256 KiB of UTF-8 text');
      return;
    }
    if (contextText === value) return;
    contextText = value;
    authoritativeCompletionFamilyId = null;
    requireExplicitCompletionFamily = true;
    contextEpoch += 1;
    contextTextSaveState = value === persistedContextText ? 'clean' : 'dirty';
    invalidateCompletionForCaretNavigation();
    if (contextTextSaveState === 'dirty') {
      scheduleContextTextSave();
    } else if (contextTextSaveTimer !== undefined) {
      window.clearTimeout(contextTextSaveTimer);
      contextTextSaveTimer = undefined;
    }
  }

  function flushContextText(): void {
    if (contextTextSaveState !== 'dirty') return;
    scheduleContextTextSave(0);
  }

  function flushContextEditorProjection(): boolean {
    if (contextCompositionActive || !(contextVisualEditor?.flushPending() ?? true)) {
      announce('Finish composing context before leaving it');
      return false;
    }
    flushContextText();
    return true;
  }

  async function persistCurrentContextText(): Promise<boolean> {
    if (!project || !document || !flushContextEditorProjection()) return false;
    while (true) {
      if (contextTextSaveTimer !== undefined) {
        window.clearTimeout(contextTextSaveTimer);
        contextTextSaveTimer = undefined;
      }
      await contextTextSaveQueue;
      if (contextTextSaveState === 'error') return false;
      if (contextTextSaveState !== 'dirty') return true;
      if (!await enqueueContextTextPersistence({
        projectId: project.project_id,
        sessionId: project.session_id,
        documentId: document.summary.document_id,
        text: contextText,
        attachmentIds: contextAttachments.map((attachment) => attachment.id)
      })) return false;
    }
  }

  function closeContextPane(): boolean {
    if (
      (speechRecording?.target === 'context' || speechInput?.target === 'context') &&
      speechInputActive()
    ) {
      announce('Finish or cancel dictation before closing its context insertion point');
      return false;
    }
    if (!flushContextEditorProjection()) return false;
    contextPaneOpen = false;
    contextToggleElement?.focus();
    return true;
  }

  function focusContextEditorAtEnd(): boolean {
    if (mode === 'visual') return contextVisualEditor?.focusAtDocumentEnd() ?? false;
    if (!contextSourceTextarea || editorReadonly) return false;
    contextSourceTextarea.focus({ preventScroll: true });
    const end = contextSourceTextarea.value.length;
    contextSourceTextarea.setSelectionRange(end, end);
    return window.document.activeElement === contextSourceTextarea;
  }

  function adoptAuthoritativeContext(
    snapshot: DocumentContextSnapshot,
    projectId: string,
    sessionId: string,
    documentId: string
  ): boolean {
    if (
      project?.project_id !== projectId ||
      project.session_id !== sessionId ||
      document?.summary.document_id !== documentId
    ) return false;
    if (contextTextSaveTimer !== undefined) {
      window.clearTimeout(contextTextSaveTimer);
      contextTextSaveTimer = undefined;
    }
    contextText = snapshot.markdown;
    persistedContextText = snapshot.markdown;
    contextTextSaveState = 'clean';
    updateContextPresentation(snapshot);
    return true;
  }

  async function normalizeImportedContext(previousText: string): Promise<void> {
    // Source-mode author text is not part of import conversion. In visual mode
    // the existing text already belongs to the admitted editor dialect.
    if (!contextText.startsWith(previousText) || contextText === previousText) return;
    const suffix = normalizeImportedMarkdown(contextText.slice(previousText.length).trimStart());
    const combined = previousText ? `${previousText}\n\n${suffix}` : suffix;
    const normalized = mode === 'visual' ? normalizeImportedMarkdown(combined) : combined;
    if (normalized !== contextText) {
      updateContextText(normalized);
      await persistCurrentContextText();
    }
  }

  async function addContextAttachmentsFromPicker(): Promise<void> {
    if (!project || !document || contextAttachmentBusy) return;
    const captured = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id
    };
    contextAttachmentBusy = true;
    try {
      if (!await persistCurrentContextText()) return;
      if (
        project?.project_id !== captured.projectId ||
        project.session_id !== captured.sessionId ||
        document?.summary.document_id !== captured.documentId
      ) return;
      const previousContextText = contextText;
      const imported = await chooseAttachments(captured.projectId, captured.sessionId);
      const snapshot = imported.length === 0
        ? null
        : await addDocumentContexts(
          captured.projectId,
          captured.sessionId,
          captured.documentId,
          imported.map((item) => item.id)
        );
      if (imported.length > 0) {
        if (!adoptAuthoritativeContext(
          snapshot!,
          captured.projectId,
          captured.sessionId,
          captured.documentId
        )) return;
        await normalizeImportedContext(previousContextText);
      }
      if (imported.length > 0) announce(`${imported.length} context attachment${imported.length === 1 ? '' : 's'} ready`);
    } catch (error) {
      recordFailure(error);
      announce('Loom could not add that context attachment');
    } finally {
      contextAttachmentBusy = false;
    }
  }

  async function removeContextAttachment(attachmentId: string): Promise<void> {
    if (!project || !document || contextAttachmentBusy) return;
    const captured = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id
    };
    contextAttachmentBusy = true;
    try {
      if (!await persistCurrentContextText()) return;
      if (
        project?.project_id !== captured.projectId ||
        project.session_id !== captured.sessionId ||
        document?.summary.document_id !== captured.documentId
      ) return;
      const snapshot = await removeDocumentContext(
        captured.projectId,
        captured.sessionId,
        captured.documentId,
        attachmentId
      );
      adoptAuthoritativeContext(
        snapshot,
        captured.projectId,
        captured.sessionId,
        captured.documentId
      );
      announce('Context attachment removed');
    } catch (error) {
      recordFailure(error);
    } finally {
      contextAttachmentBusy = false;
    }
  }

  function nativeDropPoint(position: { x: number; y: number }): { x: number; y: number } {
    return convertNativeDropPoint(position, window.navigator.platform, window.devicePixelRatio);
  }

  function nativeAttachmentDropScope(point: { x: number; y: number }): 'context' | 'inline' | null {
    return nativeDropScope(point, Array.from(window.document.querySelectorAll<HTMLElement>('[data-attachment-drop]'))
      .map(surface => ({ scope: surface.dataset.attachmentDrop as 'context' | 'inline', bounds: surface.getBoundingClientRect() })));
  }

  async function importNativeAttachmentDrop(
    paths: string[],
    point: { x: number; y: number },
    scope: 'context' | 'inline'
  ): Promise<void> {
    if (!project || !document || contextAttachmentBusy || paths.length === 0) return;
    const captured = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id,
      markdown: documentText,
      mode
    };
    const visualAnchor = scope === 'inline' && mode === 'visual' ? visualEditor?.captureAttachmentAnchor(point.x, point.y) : null;
    const sourceAnchor = scope === 'inline' && mode === 'source' ? sourceEditor?.captureTextInsertionAnchor() : null;
    contextAttachmentBusy = true;
    try {
      if (scope === 'context' && !await persistCurrentContextText()) return;
      if (
        project?.project_id !== captured.projectId ||
        project.session_id !== captured.sessionId ||
        document?.summary.document_id !== captured.documentId
      ) return;
      const previousContextText = contextText;
      const imported = await importAttachmentPaths(captured.projectId, captured.sessionId, paths);
      if (project?.project_id !== captured.projectId || project.session_id !== captured.sessionId ||
          document?.summary.document_id !== captured.documentId || mode !== captured.mode ||
          (scope === 'inline' && documentText !== captured.markdown)) {
        throw new Error('The editor changed during import. The file is stored; drop it again at the intended location.');
      }
      if (scope === 'context') {
        const snapshot = await addDocumentContexts(
            captured.projectId,
            captured.sessionId,
            captured.documentId,
            imported.map((item) => item.id)
          );
        if (!adoptAuthoritativeContext(
          snapshot,
          captured.projectId,
          captured.sessionId,
          captured.documentId
        )) return;
        await normalizeImportedContext(previousContextText);
      } else {
        const markdown = imported.map(editableImportMarkdown).join('\n\n');
        const before = sourceAnchor?.value.slice(0, sourceAnchor.start) ?? '';
        const after = sourceAnchor?.value.slice(sourceAnchor.end) ?? '';
        const prefix = before && !before.endsWith('\n\n') ? (before.endsWith('\n') ? '\n' : '\n\n') : '';
        const suffix = after && !after.startsWith('\n\n') ? (after.startsWith('\n') ? '\n' : '\n\n') : '';
        const inserted = mode === 'visual'
          ? visualAnchor && visualEditor?.insertMarkdownAtAnchor(visualAnchor, markdown)
          : sourceAnchor && sourceEditor?.insertTextAtAnchor(sourceAnchor, `${prefix}${markdown}${suffix}`);
        if (!inserted) throw new Error('The attachment was stored, but the current editor could not insert its card.');
      }
      announce(`${imported.length} attachment${imported.length === 1 ? '' : 's'} added`);
    } catch (error) {
      recordFailure(error);
      announce('Loom could not attach those files');
    } finally {
      contextAttachmentBusy = false;
      contextDropActive = false;
    }
  }

  async function openDroppedFolders(paths: string[]): Promise<void> {
    if (fileCommandInFlight || opening || !paths.length) return;
    // Each folder uses the same prepared-open/save/commit path as the picker.
    for (const path of paths.slice(0, 32)) {
      if (!await doOpenProject(path)) break;
    }
  }

  async function handleNativeDrop(paths: string[], point: { x: number; y: number }): Promise<void> {
    if (fileCommandInFlight || opening || !paths.length) return;
    const captured = { session: project?.session_id, document: document?.summary.document_id, markdown: documentText, mode };
    try {
      const directories = await projectDropDirectories(paths);
      if (!componentMounted || applicationClosePhase !== 'running' || fileCommandInFlight || opening ||
          project?.session_id !== captured.session || document?.summary.document_id !== captured.document ||
          documentText !== captured.markdown || mode !== captured.mode) return;
      if (directories.length) {
        await openDroppedFolders(directories);
      } else {
        const scope = nativeAttachmentDropScope(point);
        if (scope) await importNativeAttachmentDrop(paths, point, scope);
      }
    } catch (error) {
      recordFailure(error);
    }
  }

  async function installNativeAttachmentDrop(): Promise<void> {
    unlistenNativeAttachmentDrop = await getCurrentWindow().onDragDropEvent(({ payload }) => {
      if (payload.type === 'leave') {
        contextDropActive = false;
        workspaceDropActive = false;
        return;
      }
      const point = nativeDropPoint(payload.position);
      const outline = outlineElement?.getBoundingClientRect();
      const inOutline = Boolean(outlineOpen && outline && point.x >= outline.left && point.x < outline.right && point.y >= outline.top && point.y < outline.bottom);
      workspaceDropActive = inOutline;
      if (payload.type === 'drop') {
        workspaceDropActive = false;
        contextDropActive = false;
        void handleNativeDrop(payload.paths, point);
        return;
      }
      const scope = nativeAttachmentDropScope(point);
      contextDropActive = scope === 'context';
    });
  }

  onMount(() => {
    componentMounted = true;
    appearance = 'system';
    appearanceMedia = window.matchMedia('(prefers-color-scheme: dark)');
    systemDark = appearanceMedia.matches;
    const syncSystemAppearance = (event: MediaQueryListEvent): void => {
      systemDark = event.matches;
    };
    appearanceMedia.addEventListener('change', syncSystemAppearance);
    desktop = isDesktopRuntime();
    try { workspaceFolders = readWorkspaceFolders(window.localStorage); } catch { workspaceFolders = []; }
    if (desktop) void refreshCuratedModels();
    if (desktop) void installNativeAttachmentDrop();
    documentContextRevealLabel = desktop
      ? documentRevealLabel(window.navigator.platform, window.navigator.userAgent)
      : null;
    if (desktop) {
      stopNativeFullscreenObservation = observeNativeFullscreen(
        getCurrentWindow(),
        (fullscreen) => nativeFullscreen = fullscreen
      );
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
    window.addEventListener('keydown', handleGlobalKeydownCapture, true);
    const stopManuscriptRunShortcut = installManuscriptRunShortcut(() => void runRetainedOutput(''));
    window.addEventListener('keydown', handleGlobalKeydown);
    window.addEventListener('pointerdown', handleGlobalPointerdown);
    window.addEventListener('pageshow', handleRendererResume);
    window.document.addEventListener('visibilitychange', handleRendererResume);
    return () => {
      componentMounted = false;
      cabalEditor?.dispose();
      if (cabalTimer) clearTimeout(cabalTimer);
      if (terminalPollTimer !== undefined) window.clearTimeout(terminalPollTimer);
      startupHeldForApplicationClose = false;
      workspaceRestoreSerial += 1;
      projectFilesystemRefreshSerial += 1;
      modelRefreshSerial += 1;
      modelLoadSerial += 1;
      clearPreferredWriterRequest();
      window.removeEventListener('keydown', handleGlobalKeydownCapture, true);
      stopManuscriptRunShortcut();
      window.removeEventListener('keydown', handleGlobalKeydown);
      window.removeEventListener('pointerdown', handleGlobalPointerdown);
      window.removeEventListener('pageshow', handleRendererResume);
      window.document.removeEventListener('visibilitychange', handleRendererResume);
      stopReadAloud();
      appearanceMedia?.removeEventListener('change', syncSystemAppearance);
      appearanceMedia = null;
      clearDocumentContextLongPress();
      if (documentContextSuppressClickTimer !== undefined) {
        window.clearTimeout(documentContextSuppressClickTimer);
        documentContextSuppressClickTimer = undefined;
      }
      closeDocumentContextMenu(false);
      stopNativeFullscreenObservation?.();
      stopNativeFullscreenObservation = undefined;
      unlistenNativeAttachmentDrop?.();
      unlistenNativeAttachmentDrop = undefined;
      if (saveTimer !== undefined) window.clearTimeout(saveTimer);
      if (contextTextSaveTimer !== undefined) window.clearTimeout(contextTextSaveTimer);
      if (speechPollTimer !== undefined) window.clearTimeout(speechPollTimer);
      if (sourceProjectionTimer !== undefined) window.clearTimeout(sourceProjectionTimer);
      if (projectFilesystemRefreshTimer !== undefined) {
        window.clearTimeout(projectFilesystemRefreshTimer);
        projectFilesystemRefreshTimer = undefined;
      }
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
      if (shuttleTimer !== undefined) window.clearTimeout(shuttleTimer);
      applicationCloseRetry.dispose();
      for (const timer of staleWeaveCleanupTimers) window.clearTimeout(timer);
      staleWeaveCleanupTimers.clear();
      for (const timer of weaveStatusPollTimers) window.clearTimeout(timer);
      weaveStatusPollTimers.clear();
      unlistenWindowFocus?.();
      unlistenFileCommands?.();
      unlistenDocumentFilesystemHints?.();
    };
  });

  function startDesktopWorkspace(forceRestore = false): void {
    if (!componentMounted) return;
    if (!desktopWorkspaceStarted) {
      desktopWorkspaceStarted = true;
      void installWindowFocusHandler();
      void installFileCommandListener();
      void installDocumentFilesystemHintListener();
      void installGenerationEventListener();
      void restoreDesktopWorkspace();
      return;
    }
    if (forceRestore) void restoreDesktopWorkspace();
  }

  function holdWorkspaceForApplicationClose(): void {
    if (componentMounted && applicationClosePhase === 'closing') {
      startupHeldForApplicationClose = true;
    }
  }

  async function syncNativeWindowTitle(title: string): Promise<void> {
    if (!componentMounted || !desktop || appliedNativeTitle === title) return;
    try {
      await getCurrentWindow().setTitle(title);
      if (componentMounted) appliedNativeTitle = title;
    } catch (error) {
      if (componentMounted) recordFailure(error);
    }
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
    for (const timer of weaveStatusPollTimers) window.clearTimeout(timer);
    weaveStatusPollTimers.clear();
    completionController = clearUnpresentableVisualKeys(completionController);
    branchRefreshSerial += 1;
    branchRefreshQueued = false;
    branchNextCursor = null;
    branchFirstPageCursor = null;
    branchHasMore = false;
    branchLoadMoreOwner += 1;
    branchLoadingMore = false;
    branchLoadedPastFirstPage = false;
    branchBodyBlobByRun = {};
    verifiedBranchBodyByRun = {};
    liveBranchTextByRun = {};
    liveBranchTextSequenceByRun = {};
    branchBodyErrorByRun = {};
    completionActiveRunIds = [];
    authoritativeCompletionFamilyId = null;
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
    cabalEditor?.dispose(); cabalEditor = null; cabalScope = ''; cabalAttaching = false;
    if (cabalTimer) clearTimeout(cabalTimer);
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
    promotionArmedCandidateId = null;
    document = null;
    documentText = '';
    sourceDisplayText = '';
    sourceDirty = false;
    sourceCodec = null;
    visualEditor = null;
    mainCompositionActive = false;
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
    summary: Pick<DocumentSummary, 'document_id' | 'kind' | 'revision_id' | 'active_blob_id'>,
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
        windowFocused = focused;
        if (focused) {
          resumeCompletionObservation();
          scheduleProjectFilesystemRefresh();
          // A hidden WKWebView may stay DOM-focused and emit neither browser
          // focus nor visibilitychange on native resume. Rebuild the exact
          // cached visual decoration from the native window focus edge.
          if (mode === 'visual') visualEditor?.refreshGhostPresentation();
        }
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

  function handleRendererResume(): void {
    if (window.document.visibilityState === 'hidden') return;
    resumeCompletionObservation();
    scheduleProjectFilesystemRefresh();
  }

  async function installDocumentFilesystemHintListener(): Promise<void> {
    try {
      const unlisten = await listenForDocumentFilesystemHints((hint) => {
        routeDocumentFilesystemHint(hint, project, scheduleProjectFilesystemRefresh);
      });
      if (!componentMounted) {
        unlisten();
        return;
      }
      unlistenDocumentFilesystemHints?.();
      unlistenDocumentFilesystemHints = unlisten;
    } catch (error) {
      if (componentMounted) recordFailure(error);
    }
  }

  function scheduleProjectFilesystemRefresh(delayMilliseconds = 80): void {
    if (!componentMounted || !desktop || window.document.visibilityState === 'hidden') return;
    projectFilesystemRefreshQueued = true;
    if (projectFilesystemRefreshTimer !== undefined) {
      window.clearTimeout(projectFilesystemRefreshTimer);
    }
    projectFilesystemRefreshTimer = window.setTimeout(() => {
      projectFilesystemRefreshTimer = undefined;
      void refreshProjectFilesystemState();
    }, Math.max(0, delayMilliseconds));
  }

  function currentProjectFilesystemRefreshBoundary(): ProjectFilesystemRefreshBoundaryState {
    return {
      projectToken: project,
      documentId: document?.summary.document_id ?? null,
      documentEpoch,
      editVersion,
      navigationSerial,
      lifecycleIdle: Boolean(
        applicationClosePhase === 'running' &&
        transition === 'idle' &&
        !compositionActive &&
        !renamingDocumentId &&
        !renameDocumentInFlight &&
        !deleteDocumentInFlight &&
        !fileCommandInFlight &&
        !documentContextActionInFlight
      )
    };
  }

  async function settleMissingDocumentRecoveryBoundary(
    boundProject: ProjectSnapshot,
    missingDocumentId: string
  ): Promise<MissingDocumentRecoveryBoundaryResult> {
    if (!document || document.summary.document_id !== missingDocumentId) {
      return { kind: 'deferred', reason: 'live_document_changed' };
    }
    if (!flushEditors()) {
      projectFilesystemRefreshQueued = true;
      return { kind: 'deferred', reason: 'editor_not_flushable' };
    }
    if (saveTimer !== undefined) {
      window.clearTimeout(saveTimer);
      saveTimer = undefined;
    }

    const saveWasUncertain = uncertainSave !== null || saveState === 'uncertain';
    const draftWasUncertain = uncertainDraft !== null;
    const hadUnsavedText = documentBoundaryNeedsRecovery({
      sourceDirty,
      editVersion,
      savedVersion,
      saveState,
      saveInFlight: saveInFlight !== null,
      draftInFlight: draftInFlight !== null,
      uncertainSave: uncertainSave !== null,
      uncertainDraft: uncertainDraft !== null
    });

    if (saveInFlight) await saveInFlight;
    if (draftInFlight) await draftInFlight;
    if (
      !componentMounted ||
      project?.project_id !== boundProject.project_id ||
      project.session_id !== boundProject.session_id ||
      document?.summary.document_id !== missingDocumentId
    ) return {
      kind: 'deferred',
      reason: document?.summary.document_id === missingDocumentId
        ? 'workspace_changed'
        : 'live_document_changed'
    };

    // A save that was already admitted may have restored the visible file.
    // Recheck native authority before classifying the document as missing.
    const rechecked = await currentProjectSession();
    if (
      rechecked.project_id !== boundProject.project_id ||
      rechecked.session_id !== boundProject.session_id
    ) return { kind: 'deferred', reason: 'workspace_changed' };
    if (rechecked.documents.some((candidate) => candidate.document_id === missingDocumentId)) {
      project = rechecked;
      clearFailure();
      projectFilesystemRefreshQueued = true;
      return { kind: 'deferred', reason: 'document_reappeared' };
    }

    const journalSettled = await flushDraftJournal();
    if (
      !componentMounted ||
      project?.project_id !== boundProject.project_id ||
      project.session_id !== boundProject.session_id ||
      document?.summary.document_id !== missingDocumentId
    ) return {
      kind: 'deferred',
      reason: document?.summary.document_id === missingDocumentId
        ? 'workspace_changed'
        : 'live_document_changed'
    };
    const latestText = documentText;
    const latestEditVersion = editVersion;
    const journalDurable = missingDocumentJournalIsDurable(
      hadUnsavedText,
      journalSettled,
      uncertainDraft !== null,
      draftSavedEditVersion,
      latestEditVersion
    );
    missingDocumentRecovery = {
      projectId: boundProject.project_id,
      sessionId: boundProject.session_id,
      documentId: missingDocumentId,
      relativePath: document.summary.relative_path,
      title: document.summary.title,
      text: latestText,
      hadUnsavedText,
      journalDurable,
      sourceRevisionId: document.summary.revision_id,
      visibleBlobId: document.visible_blob_id,
      draftVersion,
      draftWasUncertain: draftWasUncertain || uncertainDraft !== null,
      saveWasUncertain: saveWasUncertain || uncertainSave !== null
    };
    missingDocumentCopyState = 'idle';
    clearFailure();
    return { kind: 'ready', project: rechecked };
  }

  async function copyMissingDocumentRecoveryText(): Promise<void> {
    const recovery = missingDocumentRecovery;
    if (!recovery) return;
    try {
      await window.navigator.clipboard.writeText(recovery.text);
      if (
        missingDocumentRecovery !== recovery ||
        project?.project_id !== recovery.projectId ||
        project.session_id !== recovery.sessionId
      ) return;
      missingDocumentCopyState = 'copied';
      announce('Preserved manuscript text copied');
      scheduleProjectFilesystemRefresh(0);
    } catch {
      if (
        missingDocumentRecovery !== recovery ||
        project?.project_id !== recovery.projectId ||
        project.session_id !== recovery.sessionId
      ) return;
      missingDocumentCopyState = 'failed';
      announce('Select and copy the preserved manuscript text manually');
    }
  }

  function clearMissingDocumentCapturePending(
    capture: MissingDocumentCaptureIdentity
  ): void {
    if (missingDocumentCapturePending === capture) {
      missingDocumentCapturePending = null;
    }
  }

  function clearReappearedMissingDocumentCapture(
    projectId: string,
    sessionId: string,
    documentId: string
  ): void {
    if (
      missingDocumentCapturePending?.projectId === projectId &&
      missingDocumentCapturePending.sessionId === sessionId &&
      missingDocumentCapturePending.documentId === documentId
    ) missingDocumentCapturePending = null;
  }

  async function reconcileMissingCurrentDocument(
    boundProject: ProjectSnapshot,
    previousDocuments: readonly DocumentSummary[],
    currentDocumentId: string
  ): Promise<void> {
    const liveDocument = document;
    if (!liveDocument || liveDocument.summary.document_id !== currentDocumentId) {
      projectFilesystemRefreshQueued = true;
      return;
    }
    const admission = beginMissingDocumentCaptureBoundary(
      missingDocumentRecovery && {
        documentId: missingDocumentRecovery.documentId,
        journalDurable: missingDocumentRecovery.journalDurable,
        copied: missingDocumentCopyState === 'copied'
      },
      {
        projectId: boundProject.project_id,
        sessionId: boundProject.session_id,
        documentId: currentDocumentId,
        revisionId: liveDocument.summary.revision_id,
        blobId: liveDocument.visible_blob_id
      },
      missingDocumentCapturePending
    );
    missingDocumentCapturePending = admission.pending;
    if (admission.kind === 'wait_for_recovery_copy') {
      // Retain the row and mounted editor until the earlier recovery is safe.
      // Copy success or a later native wakeup retries; do not timer-spin here.
      projectFilesystemRefreshQueued = false;
      announce('Copy the earlier preserved manuscript before Loom captures another missing file');
      return;
    }

    missingDocumentBoundaryInFlight = true;
    try {
      const boundary = await settleMissingDocumentRecoveryBoundary(boundProject, currentDocumentId);
      if (boundary.kind !== 'ready') {
        if (boundary.reason !== 'editor_not_flushable') {
          clearMissingDocumentCapturePending(admission.pending);
        }
        projectFilesystemRefreshQueued = true;
        return;
      }
      const settledProject = boundary.project;
      project = settledProject;
      const settledDecision = documentRefreshDecision(
        previousDocuments,
        settledProject.documents,
        currentDocumentId
      );
      closeDocumentContextMenu(false);
      if (renamingDocumentId === currentDocumentId) cancelDocumentRename(false);
      if (deleteDocumentTarget?.documentId === currentDocumentId) {
        closeDocumentDeleteConfirmation(false);
      }
      cancelSuggestionTimer();
      clearCompletionSession();
      detachDocumentForReconciliation();
      clearMissingDocumentCapturePending(admission.pending);
      clearReconciliationState();
      saveState = 'clean';
      saveMessage = settledProject.documents.length === 0
        ? 'Recovery text preserved'
        : 'All changes saved';
      if (settledProject.documents.length === 0) outlineOpen = false;
      clearFailure();
      announce('A manuscript deleted outside Loom was removed from the outline; its editor text remains available for recovery');
      await tick();
      if (settledDecision.successor) await selectDocument(settledDecision.successor, true);
    } finally {
      missingDocumentBoundaryInFlight = false;
    }
  }

  async function refreshProjectFilesystemState(): Promise<void> {
    if (projectFilesystemRefreshInFlight) {
      projectFilesystemRefreshQueued = true;
      return;
    }
    if (
      !componentMounted ||
      !desktop ||
      !project ||
      applicationClosePhase !== 'running'
    ) return;
    if (deleteDocumentUncertain) {
      // The identical delete retry is the only authority that can classify
      // this result. A watcher hint must not evict its frozen target or spin.
      projectFilesystemRefreshQueued = false;
      return;
    }
    if (reconciliation) {
      scheduleProjectFilesystemRefresh(240);
      return;
    }
    if (
      transition !== 'idle' ||
      compositionActive ||
      renamingDocumentId ||
      deleteDocumentInFlight
    ) {
      scheduleProjectFilesystemRefresh(180);
      return;
    }
    if (fileCommandInFlight) {
      scheduleProjectFilesystemRefresh(160);
      return;
    }

    const boundProject = project;
    const previousDocuments = boundProject.documents;
    const currentDocumentId = document?.summary.document_id ?? null;
    const restoreSerial = workspaceRestoreSerial;
    const refreshSerial = ++projectFilesystemRefreshSerial;
    const refreshBoundary = captureProjectFilesystemRefreshBoundary(
      currentProjectFilesystemRefreshBoundary()
    );
    let retryDelayMilliseconds = 100;
    projectFilesystemRefreshQueued = false;
    projectFilesystemRefreshInFlight = true;
    try {
      const guarded = await applyGuardedProjectFilesystemRefresh(
        refreshBoundary,
        currentProjectSession,
        currentProjectFilesystemRefreshBoundary,
        async (refreshed) => {
          if (
            refreshSerial !== projectFilesystemRefreshSerial ||
            !componentMounted ||
            workspaceRestoreSerial !== restoreSerial ||
            project?.project_id !== boundProject.project_id ||
            project.session_id !== boundProject.session_id ||
            refreshed.project_id !== boundProject.project_id ||
            refreshed.session_id !== boundProject.session_id
          ) return;

          const decision = documentRefreshDecision(
            previousDocuments,
            refreshed.documents,
            currentDocumentId
          );
          if (!currentDocumentId || !document) {
            project = refreshed;
            missingDocumentCapturePending = null;
            return;
          }

          if (decision.currentDisappeared) {
            await reconcileMissingCurrentDocument(
              boundProject,
              previousDocuments,
              currentDocumentId
            );
            return;
          }

          const current = decision.current;
          if (!current) return;
          clearReappearedMissingDocumentCapture(
            refreshed.project_id,
            refreshed.session_id,
            currentDocumentId
          );
          project = refreshed;
          if (cabalEditor) { void refreshCabal(cabalScope); return; }
          if (current.externally_modified) {
            const previewBoundary = captureProjectFilesystemRefreshBoundary(
              currentProjectFilesystemRefreshBoundary()
            );
            if (editVersion === savedVersion && documentText === document.text) {
              await selectDocument(current, true);
              return;
            }
            const preview = await requestReconciliationPreview(current, documentText, {
              projectId: refreshed.project_id,
              sessionId: refreshed.session_id,
              restoreSerial
            });
            const previewDisposition = projectFilesystemRefreshBoundaryDisposition(
              previewBoundary,
              currentProjectFilesystemRefreshBoundary()
            );
            if (previewDisposition.kind === 'retry') {
              retryDelayMilliseconds = 180;
              projectFilesystemRefreshQueued = !deleteDocumentUncertain;
              return;
            }
            if (
              refreshSerial === projectFilesystemRefreshSerial &&
              project?.project_id === refreshed.project_id &&
              project.session_id === refreshed.session_id
            ) activateReconciliation(preview);
            return;
          }

          const liveCurrent = document?.summary.document_id === currentDocumentId
            ? document.summary
            : null;
          if (
            !liveCurrent ||
            current.revision_id !== liveCurrent.revision_id ||
            current.active_blob_id !== liveCurrent.active_blob_id
          ) {
            if (compositionActive || !flushEditors() || editVersion !== savedVersion) {
              projectFilesystemRefreshQueued = true;
              return;
            }
            cancelSuggestionTimer();
            clearCompletionSession();
            detachDocumentForReconciliation();
            clearReconciliationState();
            await tick();
            await selectDocument(current, true);
            return;
          }
          document = { ...document, summary: current };
          if (
            lastFailure?.code === 'external_file_deleted' ||
            lastFailure?.code === 'filesystem_error'
          ) clearFailure();
        }
      );
      if (guarded.kind === 'retry') {
        retryDelayMilliseconds = guarded.reason === 'lifecycle_busy' ? 180 : 100;
        projectFilesystemRefreshQueued = !deleteDocumentUncertain;
        return;
      }
      if (project?.session_id === boundProject.session_id) await refreshWorkspaceTemplate();
    } catch (error) {
      const failure = normalizeFailure(error);
      if (
        failure.code === 'external_file_deleted' ||
        failureIsDefiniteContention(failure)
      ) {
        retryDelayMilliseconds = 240;
        projectFilesystemRefreshQueued = true;
        return;
      }
      if (
        componentMounted &&
        project?.project_id === boundProject.project_id &&
        project.session_id === boundProject.session_id
      ) recordFailure(failure);
    } finally {
      projectFilesystemRefreshInFlight = false;
      if (projectFilesystemRefreshQueued) {
        scheduleProjectFilesystemRefresh(retryDelayMilliseconds);
      }
    }
  }

  function resumeCompletionObservation(): void {
    if (!componentMounted || !desktop) return;
    if (!unlistenGenerationEvents) void installGenerationEventListener();
    if (branchPollTimer !== undefined) window.clearTimeout(branchPollTimer);
    branchPollTimer = undefined;
    branchPollAttempt = 0;
    // A suspended WebView can miss every event and timer. The scoped native
    // snapshot is authoritative, so foregrounding always forces a fresh pull;
    // that pull rearms active polling without depending on an event replay.
    scheduleBranchRefresh();
  }

  function installGenerationEventListener(): Promise<void> {
    if (unlistenGenerationEvents || generationListenerDisposed) return Promise.resolve();
    if (!generationListenerPromise) {
      generationListenerPromise = (async () => {
        try {
          const unlisten = await listenForGenerationEvents(handleGenerationEnvelope);
          if (generationListenerDisposed) {
            unlisten();
          } else {
            unlistenGenerationEvents = unlisten;
          }
        } catch (error) {
          if (!generationListenerDisposed) {
            recordFailure(error);
            announce('Private strand events are unavailable');
          }
        } finally {
          generationListenerPromise = null;
        }
      })();
    }
    return generationListenerPromise;
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
      if (looksLikeVisionAdapter(downloaded)) {
        announce(`${snapshot.display_name} passed checksum and is ready beside Gemma 4`);
      } else {
        selectedModelPath = downloaded.model_path;
        announce(`${snapshot.display_name} passed checksum and GGUF verification and is ready to inspect`);
      }
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

  function localCatalogModel(
    entry: CuratedModelCatalogEntry
  ): ModelCapabilitySummary | undefined {
    return models.find((model) => legacyLocalCatalogMatch(entry, model));
  }

  function loadedCatalogModel(
    entry: CuratedModelCatalogEntry
  ): ModelCapabilitySummary | undefined {
    return models.find((model) => isVerifiedCatalogWriter(entry, model));
  }

  function localCatalogProjector(entry: CuratedModelCatalogEntry): boolean {
    return models.some((model) =>
      model.local &&
      model.header_verified &&
      !model.loaded &&
      model.display_name.toLocaleLowerCase('en-US') ===
        entry.projector.artifact_name.toLocaleLowerCase('en-US') &&
      model.file_bytes === entry.projector.expected_bytes
    );
  }

  function catalogDownload(
    entry: CuratedModelCatalogEntry
  ): ModelDownloadSnapshot | undefined {
    const identities = new Set(catalogDownloadRequests(entry).map((request) => request.sha256));
    return modelDownloads.find((download) =>
      identities.has(download.expected_sha256) && !modelDownloadIsTerminal(download)
    ) ?? modelDownloads.find((download) => identities.has(download.expected_sha256));
  }

  async function beginCatalogModelDownload(entry: CuratedModelCatalogEntry): Promise<void> {
    const requests = catalogDownloadRequests(entry);
    if (
      pendingModelDownload ||
      modelDownloadStarting ||
      modelDownloads.some((download) => requests.some((request) =>
        download.expected_sha256 === request.sha256 &&
        !modelDownloadIsTerminal(download)
      ))
    ) return;
    modelDownloadStarting = true;
    modelDownloadError = '';
    try {
      await ensureModelDownloadEventListener();
      for (const request of requests) {
        const commandId = newUlid();
        const snapshot = await startModelDownload({
          commandId,
          url: request.url,
          fileName: request.fileName,
          expectedSha256: request.sha256,
          expectedBytes: request.expectedBytes,
          maxBytes: request.maxBytes
        });
        applyModelDownloadSnapshot(snapshot, false);
      }
      announce('Verified Gemma 4 model and multimodal projector downloads started');
    } catch (error) {
      modelDownloadError = error instanceof Error
        ? error.message
        : 'The curated model entry could not be downloaded safely.';
    } finally {
      modelDownloadStarting = false;
      scheduleModelDownloadPoll();
    }
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
    // Desktop delivery is intentionally lossy. A scoped event only wakes the
    // identity-bound completion snapshot; it never projects lifecycle,
    // candidate, terminal, or text facts into renderer state.
    if (branchRefreshTimer === undefined) scheduleBranchRefresh();
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
        branch.weave_command_id !== null &&
        !/^[0-9A-HJKMNP-TV-Z]{26}$/u.test(branch.weave_command_id)
      ) {
        throw new Error('The desktop returned an invalid weave-family identity.');
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
        : '';
      return { ...summary, text };
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
      !completionAutomationEnabled() ||
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
        intentEpoch: completionController.intentEpoch,
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
    if (branchRefreshInFlightCount > 0) {
      branchRefreshQueued = true;
      return false;
    }
    if (draftInFlight || saveInFlight) {
      branchRefreshQueued = true;
      scheduleBranchRefresh();
      return false;
    }
    branchRefreshQueued = false;
    const refreshSerial = ++branchRefreshSerial;
    branchRefreshInFlightCount += 1;
    try {
      const snapshot = await getCompletionSnapshot(
        projectId,
        sessionId,
        documentId,
        [...completionActiveRunIds]
      );
      if (!branchScopeMatches(
        projectId,
        sessionId,
        documentId,
        expectedViewEpoch,
        refreshSerial
      )) return false;
      const completionFacts = completionSnapshotFacts(snapshot, {
        projectId,
        sessionId,
        documentId
      });
      validateBranchSnapshots(snapshot.branches, documentId);
      validateBranchCursor(snapshot.next_cursor);
      if (snapshot.has_more !== (snapshot.next_cursor !== null)) {
        throw new Error('The desktop returned inconsistent branch page metadata.');
      }
      const completingActivePresentation = completionActiveRunIds.length > 0 &&
        completionFacts.activeRunIds.length === 0;
      if (!completingActivePresentation) {
        // Active partials are already bounded and scope-validated. Publish them
        // before unrelated terminal-body I/O so streaming latency never depends
        // on shelf hydration. The all-terminal transition is applied atomically
        // with immutable candidate bodies below to avoid a blank frame.
        liveBranchTextByRun = completionFacts.liveTextByRun;
        liveBranchTextSequenceByRun = completionFacts.liveTextSequenceByRun;
        completionActiveRunIds = completionFacts.activeRunIds;
      }
      cancellingRunIds = [...new Set([
        ...cancellingRunIds,
        ...completionFacts.cancellationRequestedRunIds
      ])];
      if (completionFacts.activeRunIds.length > 0) scheduleActiveBranchPoll();
      const firstPageCards = cardsFromSummaries(snapshot.branches);
      branches = mergeNewestPage(firstPageCards, branches);
      const firstPageCursorChanged =
        snapshot.next_cursor?.sequence !== branchFirstPageCursor?.sequence ||
        snapshot.next_cursor?.run_id !== branchFirstPageCursor?.run_id;
      if (!branchLoadedPastFirstPage || firstPageCursorChanged) {
        branchNextCursor = snapshot.next_cursor;
        branchHasMore = snapshot.has_more;
        branchLoadedPastFirstPage = false;
      }
      branchFirstPageCursor = snapshot.next_cursor;
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
      liveBranchTextByRun = completionFacts.liveTextByRun;
      liveBranchTextSequenceByRun = completionFacts.liveTextSequenceByRun;
      branchBodyErrorByRun = hydration.bodyErrorByRun;
      branches = branches.map((branch) => hydratedByRun.get(branch.run_id) ?? branch);
      completionActiveRunIds = completionFacts.activeRunIds;
      const failed = branches.find(branch => branch.status === 'failed' &&
        branch.source_revision_id === document?.summary.revision_id && branch.error);
      if (failed && failed.run_id !== reportedGenerationFailureRun) {
        reportedGenerationFailureRun = failed.run_id;
        recordLocalFailure('generation_runtime_failed', failed.error!);
        announce('Ghost text is unavailable.');
      }
      reconcileBranchActionState();
      if (completionActiveRunIds.length > 0) scheduleActiveBranchPoll();
      return true;
    } catch (error) {
      const failure = normalizeFailure(error);
      if (failureIsDefiniteContention(failure)) {
        branchRefreshQueued = true;
        return false;
      }
      if (
        reportFailure &&
        project?.project_id === projectId &&
        project.session_id === sessionId &&
        document?.summary.document_id === documentId
      ) {
        recordFailure(failure);
        announce('Stored strands could not be refreshed');
      }
      return false;
    } finally {
      branchRefreshInFlightCount = Math.max(0, branchRefreshInFlightCount - 1);
      if (branchRefreshInFlightCount === 0 && branchRefreshQueued) {
        branchRefreshQueued = false;
        scheduleBranchRefresh();
      }
    }
  }

  function refreshCurrentBranches(reportFailure = true): Promise<boolean> {
    if (!project || !document) {
      branches = [];
      liveBranchTextByRun = {};
      liveBranchTextSequenceByRun = {};
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
      completionActiveRunIds.length === 0
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
    if (!refreshed || completionActiveRunIds.length > 0) {
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
    const resumeAction = workspaceResumeAction(
      startupHeldForApplicationClose,
      desktopWorkspaceStarted,
      applicationStartupDisposition(outcome) === 'continue'
    );
    if (resumeAction !== 'none') {
      startupHeldForApplicationClose = false;
      startDesktopWorkspace(resumeAction === 'restore_workspace');
    }
    return outcome;
  }

  function workspaceRestoreIsCurrent(captured: WorkspaceRestoreCapture): boolean {
    return Boolean(
      componentMounted &&
      applicationClosePhase === 'running' &&
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

  let unavailableWorkspaceWriterKey = '';

  function workspaceWriterKey(): string {
    return `${workspaceTemplateScope}/${workspaceTemplate?.revision_id ?? ''}/${JSON.stringify(workspaceTemplate?.config.model ?? null)}`;
  }

  function workspaceWriterSwapIsIdle(): boolean {
    return transition === 'idle' && !compositionActive && !weaveStarting &&
      completionActiveRunIds.length === 0 && !terminalDispatching &&
      !terminalPendingRequest && !terminalRuns.some((run) => run.status === 'running') &&
      (!models.some((model) => model.loaded) || !Object.values(paneBusy).some(Boolean));
  }

  async function prepareWorkspaceWriterTemplate(captured: WorkspaceRestoreCapture): Promise<boolean> {
    const scope = `${captured.projectId}/${captured.sessionId}`;
    if (workspaceTemplateScope !== scope || !workspaceTemplate) await refreshWorkspaceTemplate();
    await tick();
    return workspaceRestoreIsCurrent(captured) && workspaceTemplateScope === scope &&
      Boolean(workspaceTemplate && !workspaceTemplate.error);
  }

  $: if (preferredWriterPending && !modelLoading && !modelUnloading &&
    transition === 'idle' && !compositionActive && !weaveStarting &&
    completionActiveRunIds.length === 0 && !terminalDispatching &&
    !terminalPendingRequest && !terminalRuns.some((run) => run.status === 'running') &&
    (!models.some((model) => model.loaded) || !Object.values(paneBusy).some(Boolean))) wakePreferredWriterEnsure();

  function clearPreferredWriterRequest(captured?: WorkspaceRestoreCapture): void {
    if (!captured || workspaceCapturesMatch(preferredWriterPending, captured)) {
      preferredWriterPending = null;
    }
  }

  function queuePreferredWriterRequest(captured: WorkspaceRestoreCapture): void {
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      !completionAutomationEnabled() ||
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
      !completionAutomationEnabled() ||
      currentModel || unavailableWorkspaceWriterKey === workspaceWriterKey()
    ) return;
    requestPreferredWriterEnsure(captured);
  }

  function drainPreferredWriterEnsure(): Promise<boolean> {
    if (preferredWriterEnsureInFlight) return preferredWriterEnsureInFlight;
    const captured = preferredWriterPending;
    if (!captured) return Promise.resolve(Boolean(currentModel));
    if (unavailableWorkspaceWriterKey === workspaceWriterKey()) {
      clearPreferredWriterRequest(captured);
      return Promise.resolve(false);
    }
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      !completionAutomationEnabled() ||
      !workspaceRestoreIsCurrent(captured)
    ) {
      clearPreferredWriterRequest(captured);
      return Promise.resolve(false);
    }
    if (
      modelLoading ||
      modelUnloading ||
      modelRefreshInFlightCount > 0 ||
      !workspaceWriterSwapIsIdle() ||
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
      !completionAutomationEnabled() ||
      !workspaceRestoreIsCurrent(captured)
    ) return false;
    if (!await prepareWorkspaceWriterTemplate(captured)) return false;
    if (currentModel) {
      if (document) scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'model_ready');
      return true;
    }

    const refreshed = await refreshModels(captured);
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      !completionAutomationEnabled() ||
      !workspaceRestoreIsCurrent(captured)
    ) return false;
    if (!refreshed && modelRefreshInFlightCount > 0) {
      queuePreferredWriterRequest(captured);
      return false;
    }
    await tick();
    if (!applicationAllowsModelPreparation(applicationClosePhase)) return false;
    if (currentModel) {
      if (document) scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'model_ready');
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

  async function retryPreferredWriter(): Promise<void> {
    unavailableWorkspaceWriterKey = '';
    quietModelLoadFailure = null;
    if (modelSetupError.startsWith('Automatic writer setup failed.')) modelSetupError = '';
    await refreshCurrentModelsAndEnsureWriter();
  }

  function closeFormatMenu(refocus = true): void {
    formatMenu?.close(refocus);
  }

  async function setOutlineOpen(open: boolean): Promise<void> {
    if (!open) closeDocumentContextMenu(false);
    outlineOpen = open;
    await tick();
  }

  function stopReadAloud(): void {
    speechPlaybackSerial++;
    spokenAudio?.pause();
    spokenAudio = null;
    if (spokenAudioUrl) URL.revokeObjectURL(spokenAudioUrl);
    spokenAudioUrl = null;
  }

  async function readAloud(): Promise<void> {
    if (spokenAudio) { stopReadAloud(); return; }
    if (!document) return;
    const text = window.getSelection()?.toString().trim() || (sourceTextarea && sourceTextarea.selectionStart !== sourceTextarea.selectionEnd ? sourceTextarea.value.slice(sourceTextarea.selectionStart, sourceTextarea.selectionEnd) : documentText);
    if (!text.trim()) return;
    if (new TextEncoder().encode(text).length > 4096) { announce('Select a shorter passage to read aloud'); return; }
    const serial = ++speechPlaybackSerial;
    const scope = document.summary.document_id;
    try {
      const result = await synthesizeAudio(text);
      if (serial !== speechPlaybackSerial || document?.summary.document_id !== scope) return;
      spokenAudioUrl = URL.createObjectURL(new Blob([new Uint8Array(result.wav)], { type: 'audio/wav' }));
      spokenAudio = new Audio(spokenAudioUrl);
      spokenAudio.onended = stopReadAloud;
      await spokenAudio.play();
      announce('Reading aloud');
    } catch (error) { if (serial === speechPlaybackSerial) { stopReadAloud(); recordFailure(error); } }
  }

  function setAppearance(next: AppearancePreference): void {
    appearance = next;
    const label = next === 'system' ? 'Appearance follows the system' : `${next} appearance`;
    announce(`${label} for this session`);
  }

  function toggleAppearance(): void {
    setAppearance(toggledAppearance(appearance, systemDark));
  }

  function startTitlebarDrag(event: MouseEvent): void {
    if (!desktop || event.button !== 0) return;
    void getCurrentWindow().startDragging().catch(recordFailure);
  }

  function openModelManager(trigger: HTMLElement): void {
    if (lastFailure?.code.startsWith('model_') || lastFailure?.code.startsWith('writing_model_')) {
      clearFailure();
    }
    modelSetupError = '';
    modelManagerReturnFocus = trigger;
    modelManagerOpen = true;
    modelDownloadError = '';
    if (curatedModels.length === 0) void refreshCuratedModels();
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

  async function refreshCuratedModels(): Promise<void> {
    if (!desktop) return;
    if (curatedModelsRefreshPromise) return curatedModelsRefreshPromise;
    const refresh = (async () => {
      curatedModelsLoading = true;
      curatedModelsError = '';
      try {
        curatedModels = validateCuratedModelCatalog(await listCuratedModels());
      } catch (error) {
        curatedModels = [];
        curatedModelsError = normalizeFailure(error).message;
      } finally {
        curatedModelsLoading = false;
      }
    })();
    curatedModelsRefreshPromise = refresh;
    try {
      await refresh;
    } finally {
      if (curatedModelsRefreshPromise === refresh) curatedModelsRefreshPromise = null;
    }
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
      focusCurrentWritingSurfaceAtEnd();
    });
  }

  function focusConnectedControl(target: HTMLElement | null | undefined): boolean {
    if (!target?.isConnected || target.hidden || target.getClientRects().length === 0) return false;
    target.focus();
    return window.document.activeElement === target;
  }

  function clearDocumentContextLongPress(): void {
    if (documentContextLongPressTimer !== undefined) {
      window.clearTimeout(documentContextLongPressTimer);
      documentContextLongPressTimer = undefined;
    }
    const pending = documentContextLongPress;
    if (pending?.trigger.hasPointerCapture(pending.pointerId)) {
      pending.trigger.releasePointerCapture(pending.pointerId);
    }
    documentContextLongPress = null;
  }

  function closeDocumentContextMenu(refocus = true): void {
    clearDocumentContextLongPress();
    const trigger = documentContextTrigger;
    documentContextTarget = null;
    documentContextTrigger = null;
    documentContextFocusIndex = 0;
    if (!refocus) return;
    void tick().then(() => {
      if (focusConnectedControl(trigger)) return;
      if (focusConnectedControl(outlineToggle)) return;
      focusCurrentWritingSurfaceAtEnd();
    });
  }

  function documentContextMenuItems(): HTMLButtonElement[] {
    if (!documentContextMenu) return [];
    return Array.from(
      documentContextMenu.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not([disabled])')
    );
  }

  async function focusDocumentContextMenu(
    target: CapturedDocumentTarget,
    requestedIndex = 0
  ): Promise<void> {
    await tick();
    if (documentContextTarget !== target || !documentContextMenu) return;
    const bounds = documentContextMenu.getBoundingClientRect();
    documentContextPoint = clampDocumentMenuPoint(
      documentContextPoint,
      bounds.width,
      bounds.height,
      window.innerWidth,
      window.innerHeight
    );
    await tick();
    if (documentContextTarget !== target) return;
    const items = documentContextMenuItems();
    if (items.length === 0) return;
    documentContextFocusIndex = Math.min(Math.max(requestedIndex, 0), items.length - 1);
    items[documentContextFocusIndex]?.focus();
  }

  function openDocumentContextMenu(
    target: CapturedDocumentTarget,
    trigger: HTMLButtonElement,
    point: MenuPoint
  ): void {
    if (
      documentContextActionInFlight ||
      fileCommandInFlight ||
      applicationClosePhase !== 'running' ||
      transition !== 'idle'
    ) return;
    closeFormatMenu(false);
    closeDocumentContextMenu(false);
    closeDocumentDeleteConfirmation(false);
    documentContextTarget = target;
    documentContextTrigger = trigger;
    documentContextPoint = point;
    documentContextFocusIndex = 0;
    void focusDocumentContextMenu(target);
  }

  function captureDocumentContextTarget(summary: DocumentSummary): CapturedDocumentTarget | null {
    if (!project) return null;
    const target = captureDocumentTarget(project, summary);
    if (!target) {
      announce(`${summary.title} does not expose a complete active revision yet`);
    }
    return target;
  }

  function handleDocumentContextPointer(
    event: MouseEvent,
    summary: DocumentSummary
  ): void {
    event.preventDefault();
    event.stopPropagation();
    clearDocumentContextLongPress();
    const target = captureDocumentContextTarget(summary);
    if (!target) return;
    openDocumentContextMenu(target, event.currentTarget as HTMLButtonElement, {
      x: event.clientX,
      y: event.clientY
    });
  }

  function handleDocumentContextKey(
    event: KeyboardEvent,
    summary: DocumentSummary
  ): void {
    if (!isDocumentContextTriggerKey(event)) return;
    event.preventDefault();
    event.stopPropagation();
    const target = captureDocumentContextTarget(summary);
    if (!target) return;
    const trigger = event.currentTarget as HTMLButtonElement;
    const bounds = trigger.getBoundingClientRect();
    openDocumentContextMenu(target, trigger, {
      x: bounds.left + 12,
      y: bounds.top + Math.min(bounds.height, 28)
    });
  }

  function handleVisibleDocumentActions(
    event: MouseEvent,
    summary: DocumentSummary
  ): void {
    event.preventDefault();
    event.stopPropagation();
    const target = captureDocumentContextTarget(summary);
    if (!target) return;
    const trigger = event.currentTarget as HTMLButtonElement;
    openDocumentContextMenu(
      target,
      trigger,
      visibleDocumentActionsMenuPoint(trigger.getBoundingClientRect())
    );
  }

  function beginDocumentContextLongPress(
    event: PointerEvent,
    summary: DocumentSummary
  ): void {
    if (event.pointerType !== 'touch' || event.button !== 0) return;
    clearDocumentContextLongPress();
    const target = captureDocumentContextTarget(summary);
    if (!target) return;
    const trigger = event.currentTarget as HTMLButtonElement;
    const pending = {
      pointerId: event.pointerId,
      x: event.clientX,
      y: event.clientY,
      target,
      trigger
    };
    documentContextLongPress = pending;
    try {
      trigger.setPointerCapture(event.pointerId);
    } catch {
      // A detached row cancels through the session/menu guards below.
    }
    documentContextLongPressTimer = window.setTimeout(() => {
      if (documentContextLongPress !== pending) return;
      documentContextLongPressTimer = undefined;
      clearDocumentContextLongPress();
      documentContextSuppressClickId = pending.target.documentId;
      if (documentContextSuppressClickTimer !== undefined) {
        window.clearTimeout(documentContextSuppressClickTimer);
      }
      documentContextSuppressClickTimer = window.setTimeout(() => {
        if (documentContextSuppressClickId === pending.target.documentId) {
          documentContextSuppressClickId = null;
        }
        documentContextSuppressClickTimer = undefined;
      }, 1_000);
      openDocumentContextMenu(pending.target, pending.trigger, {
        x: pending.x,
        y: pending.y
      });
    }, documentContextLongPressMilliseconds);
  }

  function updateDocumentContextLongPress(event: PointerEvent): void {
    const pending = documentContextLongPress;
    if (!pending || pending.pointerId !== event.pointerId) return;
    if (
      Math.abs(event.clientX - pending.x) > documentContextLongPressSlop ||
      Math.abs(event.clientY - pending.y) > documentContextLongPressSlop
    ) clearDocumentContextLongPress();
  }

  function finishDocumentContextLongPress(event: PointerEvent): void {
    if (documentContextLongPress?.pointerId === event.pointerId) {
      clearDocumentContextLongPress();
    }
  }

  function handleDocumentRowClick(event: MouseEvent, summary: DocumentSummary): void {
    if (documentContextSuppressClickId === summary.document_id) {
      documentContextSuppressClickId = null;
      if (documentContextSuppressClickTimer !== undefined) {
        window.clearTimeout(documentContextSuppressClickTimer);
        documentContextSuppressClickTimer = undefined;
      }
      event.preventDefault();
      return;
    }
    if (
      summary.document_id === document?.summary.document_id &&
      event.target instanceof Element &&
      event.target.closest('[data-document-title]')
    ) {
      event.preventDefault();
      const target = captureDocumentContextTarget(summary);
      if (target) void beginDocumentRename(target, event.currentTarget as HTMLElement);
      return;
    }
    void selectDocument(summary, true);
  }

  async function beginDocumentRename(
    target: CapturedDocumentTarget,
    trigger: HTMLElement | null
  ): Promise<void> {
    if (
      renameDocumentInFlight ||
      renameDocumentEditorLocked ||
      fileCommandInFlight ||
      editorReadonly ||
      !capturedDocumentBelongsToSession(target, project)
    ) return;
    closeDocumentContextMenu(false);
    const targetIsCurrent = document?.summary.document_id === target.documentId;
    if (compositionActive) {
      announce('Finish composing text before renaming a manuscript');
      return;
    }
    // Freeze the manuscript before preparing rename authority and retain that
    // lock until the rename is committed or explicitly cancelled. Otherwise a
    // click back into the editor can start an autosave against the captured
    // revision while the native rename command is still in flight.
    renameDocumentEditorLocked = true;
    if (targetIsCurrent && !flushEditors()) {
      renameDocumentEditorLocked = false;
      return;
    }
    fileCommandInFlight = true;
    let refreshedTarget: CapturedDocumentTarget | null = null;
    try {
      refreshedTarget = await refreshDocumentRenameTarget(
        target,
        document?.summary.document_id ?? null,
        flushCurrentDocument,
        () => project
      );
    } catch (error) {
      recordDocumentContextFailure(target, error);
    } finally {
      fileCommandInFlight = false;
    }
    if (!refreshedTarget) {
      renameDocumentEditorLocked = false;
      announce('The manuscript changed before its rename authority could be prepared');
      return;
    }
    renameDocumentComposition.reset();
    renamingDocumentId = refreshedTarget.documentId;
    renameDocumentTitle = refreshedTarget.title;
    renameDocumentTarget = refreshedTarget;
    renameDocumentTrigger = trigger;
    await tick();
    renameDocumentInput?.focus();
    renameDocumentInput?.select();
  }

  function synchronizeDocumentRenameTitle(input: HTMLInputElement): void {
    const bounded = boundedDocumentTitleInput(input.value);
    if (input.value !== bounded) input.value = bounded;
    renameDocumentTitle = bounded;
  }

  function handleDocumentRenameInput(event: Event): void {
    synchronizeDocumentRenameTitle(event.currentTarget as HTMLInputElement);
  }

  function handleDocumentRenameCompositionStart(): void {
    renameDocumentComposition.start();
  }

  function handleDocumentRenameCompositionEnd(event: CompositionEvent): void {
    const input = event.currentTarget as HTMLInputElement;
    synchronizeDocumentRenameTitle(input);
    const commitAfterBlur = renameDocumentComposition.finish();
    if (
      commitAfterBlur &&
      input === renameDocumentInput &&
      renamingDocumentId !== null &&
      !renameDocumentInFlight
    ) void commitDocumentRename(false);
  }

  function handleDocumentRenameBlur(): void {
    if (renameDocumentInFlight || !renameDocumentComposition.blurShouldCommit()) return;
    void commitDocumentRename(false);
  }

  function cancelDocumentRename(refocus = true): void {
    const documentId = renamingDocumentId;
    const trigger = renameDocumentTrigger;
    renamingDocumentId = null;
    renameDocumentTitle = '';
    renameDocumentTarget = null;
    renameDocumentTrigger = null;
    renameDocumentEditorLocked = false;
    renameDocumentComposition.reset();
    if (refocus) void tick().then(() => {
      const row = Array.from(
        window.document.querySelectorAll<HTMLButtonElement>('[data-document-row]')
      ).find((candidate) => candidate.dataset.documentRow === documentId);
      if (focusConnectedControl(row)) return;
      if (focusConnectedControl(trigger)) return;
      if (focusConnectedControl(outlineToggle)) return;
      focusCurrentWritingSurfaceAtEnd();
    });
  }

  async function commitDocumentRename(refocus = true): Promise<void> {
    const target = renameDocumentTarget;
    if (!target || renameDocumentInFlight || renameDocumentComposition.active) return;
    if (!capturedDocumentBelongsToSession(target, project)) {
      cancelDocumentRename(false);
      announce('The project session changed before the manuscript could be renamed');
      return;
    }
    const title = renameDocumentTitle.trim();
    renameDocumentInFlight = true;
    fileCommandInFlight = true;
    let restoreFailedRenameFocus = false;
    try {
      const renamed = await renameDocument(
        target.projectId,
        target.sessionId,
        target.documentId,
        target.expectedRevisionId,
        target.expectedBlobId,
        title
      );
      if (
        !project ||
        !capturedDocumentBelongsToSession(target, project) ||
        !capturedDocumentIdentityIsCurrent(target, project) ||
        renamed.document_id !== target.documentId ||
        renamed.revision_id !== target.expectedRevisionId ||
        renamed.active_blob_id !== target.expectedBlobId
      ) throw new Error('The rename receipt did not match the captured manuscript.');
      const projection = applyDocumentRenameProjection(project, document, renamed);
      project = projection.project;
      document = projection.document;
      cancelDocumentRename(refocus);
      announce(`Renamed manuscript to ${renamed.title}`);
    } catch (error) {
      recordDocumentContextFailure(target, error);
      restoreFailedRenameFocus = true;
    } finally {
      if (restoreFailedRenameFocus) {
        await releaseDocumentRenameAndRestoreFocus(
          () => {
            fileCommandInFlight = false;
            renameDocumentInFlight = false;
          },
          tick,
          () => renameDocumentInput
        );
      } else {
        fileCommandInFlight = false;
        renameDocumentInFlight = false;
      }
    }
  }

  function handleDocumentRenameKeydown(event: KeyboardEvent): void {
    const compositionOwnsCommand = renameDocumentComposition.ownsCommandKey(event);
    if (
      compositionOwnsCommand &&
      (event.key === 'Escape' || event.key === 'Enter')
    ) {
      // Keep the event's default behavior available to the IME, but prevent a
      // rename-owned composition command from reaching global Shuttle/menu
      // routing.
      event.stopPropagation();
      return;
    }
    if (event.key === 'Escape') {
      event.preventDefault();
      cancelDocumentRename();
      return;
    }
    if (event.key === 'Enter') {
      event.preventDefault();
      void commitDocumentRename(true);
    }
  }

  function openDocumentDeleteConfirmation(
    target: CapturedDocumentTarget,
    trigger: HTMLElement | null
  ): void {
    if (
      deleteDocumentInFlight ||
      fileCommandInFlight ||
      editorReadonly ||
      !capturedDocumentBelongsToSession(target, project)
    ) return;
    closeDocumentContextMenu(false);
    deleteDocumentTarget = target;
    deleteDocumentTrigger = trigger;
    deleteDocumentCommandId = newUlid();
    deleteDocumentUncertain = false;
    void tick().then(() => {
      if (deleteDocumentTarget === target) {
        (deleteDocumentCancelButton ?? deleteDocumentDialog)?.focus();
      }
    });
  }

  function closeDocumentDeleteConfirmation(refocus = true): void {
    if (deleteDocumentUncertain) return;
    const trigger = deleteDocumentTrigger;
    deleteDocumentTarget = null;
    deleteDocumentTrigger = null;
    deleteDocumentCommandId = null;
    deleteDocumentUncertain = false;
    deleteDocumentEditorLocked = false;
    if (!refocus) return;
    void tick().then(() => {
      if (focusConnectedControl(trigger)) return;
      if (focusConnectedControl(outlineToggle)) return;
      focusCurrentWritingSurfaceAtEnd();
    });
  }

  function handleDocumentDeleteDialogKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape' && !deleteDocumentInFlight && !deleteDocumentUncertain) {
      event.preventDefault();
      event.stopPropagation();
      closeDocumentDeleteConfirmation();
      return;
    }
    trapFocusWithin(event, deleteDocumentDialog);
  }

  async function confirmDocumentDelete(): Promise<void> {
    const initialTarget = deleteDocumentTarget;
    const commandId = deleteDocumentCommandId;
    if (
      !initialTarget ||
      !commandId ||
      deleteDocumentInFlight ||
      !capturedDocumentBelongsToSession(initialTarget, project)
    ) return;
    if (compositionActive) {
      announce('Finish composing text before deleting a manuscript');
      return;
    }

    deleteDocumentInFlight = true;
    deleteDocumentEditorLocked = true;
    fileCommandInFlight = true;
    let committed:
      | {
          deletedWasCurrent: boolean;
          successor: DocumentSummary | null;
          title: string;
        }
      | null = null;
    try {
      const retryingUncertainDelete = deleteDocumentUncertain;
      const targetIsCurrent = document?.summary.document_id === initialTarget.documentId;
      if (targetIsCurrent && !retryingUncertainDelete && !flushEditors()) return;
      const prepared = await refreshDocumentDeleteTarget(
        initialTarget,
        document?.summary.document_id ?? null,
        flushCurrentDocument,
        () => project,
        retryingUncertainDelete
      );
      if (!prepared || !project) {
        announce('The manuscript changed before deletion could be authorized');
        return;
      }
      deleteDocumentTarget = prepared;
      const previousDocuments = project.documents;
      const currentDocumentId = document?.summary.document_id ?? null;
      const refreshed = await deleteDocument(
        prepared.projectId,
        prepared.sessionId,
        prepared.documentId,
        prepared.expectedRevisionId,
        prepared.expectedBlobId,
        commandId
      );
      if (
        !project ||
        refreshed.project_id !== prepared.projectId ||
        refreshed.session_id !== prepared.sessionId ||
        refreshed.documents.some((candidate) => candidate.document_id === prepared.documentId)
      ) throw new Error('The desktop did not return the authoritative project after deletion.');

      const decision = documentRefreshDecision(
        previousDocuments,
        refreshed.documents,
        currentDocumentId
      );
      const deletedWasCurrent = currentDocumentId === prepared.documentId;
      project = refreshed;
      if (deletedWasCurrent) {
        cancelSuggestionTimer();
        clearCompletionSession();
        detachDocumentForReconciliation();
        clearReconciliationState();
        saveState = 'clean';
        saveMessage = refreshed.documents.length === 0 ? 'Project is ready' : 'All changes saved';
        if (refreshed.documents.length === 0) outlineOpen = false;
      } else if (document && decision.current) {
        document = { ...document, summary: decision.current };
      }
      clearFailure();
      committed = {
        deletedWasCurrent,
        successor: deletedWasCurrent ? decision.successor : null,
        title: prepared.title
      };
    } catch (error) {
      const failure = normalizeFailure(error);
      if (
        failure.code === 'external_file_deleted' ||
        failure.code === 'document_not_found' ||
        failure.code === 'stale_document_action'
      ) {
        deleteDocumentUncertain = false;
        announce(`${initialTarget.title} changed outside Loom; refreshing the outline`);
        closeDocumentDeleteConfirmation(false);
        scheduleProjectFilesystemRefresh(0);
      } else {
        deleteDocumentUncertain = captureForIdempotentRetry(commandId, failure) !== null;
        recordDocumentContextFailure(deleteDocumentTarget ?? initialTarget, failure);
        if (deleteDocumentUncertain) {
          announce('Deletion result is uncertain; check the identical command before continuing');
        }
      }
    } finally {
      fileCommandInFlight = false;
      deleteDocumentInFlight = false;
      deleteDocumentEditorLocked = deleteDocumentUncertain;
    }

    if (!committed) return;
    const returnFocus = deleteDocumentTrigger;
    deleteDocumentUncertain = false;
    closeDocumentDeleteConfirmation(false);
    announce(`Deleted ${committed.title}`);
    if (!committed.deletedWasCurrent) {
      await tick();
      if (!focusConnectedControl(returnFocus)) focusConnectedControl(outlineToggle);
      return;
    }
    await tick();
    if (committed.successor) {
      await selectDocument(committed.successor, true);
    } else {
      const newDocumentButton = window.document.querySelector<HTMLButtonElement>(
        '.new-document-button:not([disabled])'
      );
      focusConnectedControl(newDocumentButton);
    }
  }

  function handleDocumentContextMenuKeydown(event: KeyboardEvent): void {
    const items = documentContextMenuItems();
    const action = documentMenuKeyAction(event, documentContextFocusIndex, items.length);
    switch (action.kind) {
      case 'focus':
        event.preventDefault();
        documentContextFocusIndex = action.index;
        items[action.index]?.focus();
        return;
      case 'activate':
        event.preventDefault();
        items[action.index]?.click();
        return;
      case 'dismiss':
        event.preventDefault();
        closeDocumentContextMenu();
        return;
      case 'none':
        return;
      default: {
        const unreachable: never = action;
        return unreachable;
      }
    }
  }

  function recordDocumentContextFailure(
    target: CapturedDocumentTarget,
    error: unknown
  ): void {
    if (
      applicationClosePhase !== 'running' ||
      !capturedDocumentBelongsToSession(target, project)
    ) return;
    const failure = normalizeFailure(error);
    if (
      failure.code === 'application_quiescing' ||
      failure.code === 'application_close_in_progress' ||
      failure.code === 'application_exit_authorized'
    ) return;
    if (
      failure.code === 'stale_document_action' ||
      failure.code === 'document_not_found' ||
      failure.code === 'source_revision_conflict' ||
      failure.code === 'source_blob_conflict'
    ) {
      recordLocalFailure(
        failure.code,
        `${target.title} changed after its menu was opened. Open the current outline entry and try again.`
      );
      return;
    }
    if (
      failure.code === 'external_file_change' ||
      failure.code === 'external_file_deleted' ||
      failure.code === 'external_file_conflict'
    ) {
      recordLocalFailure(
        failure.code,
        `${target.title} changed outside Loom. Open it from the outline to review the exact external bytes before continuing.`
      );
      return;
    }
    if (
      failure.code === 'document_reveal_path_changed' ||
      failure.code === 'document_reveal_path_refused' ||
      failure.code === 'document_reveal_path_unavailable'
    ) {
      recordLocalFailure(
        failure.code,
        `${target.title} could not be revealed from its current registered file. Open it from the outline and try again.`
      );
      return;
    }
    recordFailure(error);
  }

  async function runDocumentContextAction(action: DocumentContextAction): Promise<void> {
    if (documentContextActionInFlight || fileCommandInFlight) return;
    const target = documentContextTarget;
    const trigger = documentContextTrigger;
    if (!target) return;
    documentContextActionInFlight = true;
    closeDocumentContextMenu(false);
    if (
      applicationClosePhase !== 'running' ||
      !capturedDocumentBelongsToSession(target, project)
    ) {
      documentContextActionInFlight = false;
      return;
    }

    let restoreTrigger = action !== 'open';
    try {
      switch (action) {
        case 'open':
          await selectCapturedDocument(target, true, true);
          restoreTrigger =
            document?.summary.document_id !== target.documentId &&
            reconciliation?.document_id !== target.documentId;
          break;
        case 'rename':
          await beginDocumentRename(target, trigger);
          restoreTrigger = false;
          break;
        case 'delete':
          openDocumentDeleteConfirmation(target, trigger);
          restoreTrigger = false;
          break;
        case 'export_text': {
          fileCommandInFlight = true;
          const receipt = await exportDocumentCopy(
            target.projectId,
            target.sessionId,
            target.documentId,
            target.expectedRevisionId,
            target.expectedBlobId
          );
          if (receipt) announce(`Exported ${target.title} as text`);
          break;
        }
        case 'reveal':
          fileCommandInFlight = true;
          await revealDocument(
            target.projectId,
            target.sessionId,
            target.documentId,
            target.expectedRevisionId,
            target.expectedBlobId
          );
          announce(`${target.title} revealed in its containing folder`);
          break;
        default: {
          const unreachable: never = action;
          return unreachable;
        }
      }
    } catch (error) {
      recordDocumentContextFailure(target, error);
      restoreTrigger = action !== 'open';
    } finally {
      fileCommandInFlight = false;
      documentContextActionInFlight = false;
      if (restoreTrigger) {
        await tick();
        if (!focusConnectedControl(trigger)) focusConnectedControl(outlineToggle);
      }
    }
  }

  function suggestionPreferenceKey(projectId: string): string {
    return `loom:suggestions:${projectId}`;
  }

  const lastLocalModelKey = 'loom:last-local-model';

  function loadLastLocalModelPath(): string | null {
    try {
      const remembered = window.localStorage.getItem(lastLocalModelKey);
      if (remembered && isEphemeralAcceptanceModelPath(remembered)) {
        window.localStorage.removeItem(lastLocalModelKey);
        return null;
      }
      return remembered;
    } catch {
      return null;
    }
  }

  function rememberLastLocalModelPath(modelPath: string): void {
    if (isEphemeralAcceptanceModelPath(modelPath)) return;
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

  function clearSuggestionTimerHandle(): void {
    if (suggestionsIdleTimer !== undefined) window.clearTimeout(suggestionsIdleTimer);
    suggestionsIdleTimer = undefined;
  }

  function cancelSuggestionTimer(): void {
    clearSuggestionTimerHandle();
    completionController = cancelCompletionSchedule(completionController);
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
    const previousDismissedCandidateIds = completionController.dismissedCandidateIds;
    const engineBecameEnabled = completionEngineBecameEnabled(
      { autocomplete: previousEnabled, shuttle: shuttleEnabled },
      { autocomplete: enabled, shuttle: shuttleEnabled }
    );
    const engineBecameDisabled = completionEngineBecameDisabled(
      { autocomplete: previousEnabled, shuttle: shuttleEnabled },
      { autocomplete: enabled, shuttle: shuttleEnabled }
    );
    suggestionsChanging = true;
    if (!enabled && !shuttleEnabled) {
      suggestionsEnabled = false;
      cancelSuggestionTimer();
      completionController = setDismissedCompletionCandidates(
        completionController,
        currentReadyBranches
          .map((branch) => branch.candidate_id)
          .filter((candidateId): candidateId is string => Boolean(candidateId))
      );
    }
    try {
      const automationEnabled = completionAutomationEnabled(enabled, shuttleEnabled);
      await setSuggestionsPolicy(
        boundProject.project_id,
        boundProject.session_id,
        automationEnabled
      );
      if (
        !applicationAllowsModelPreparation(applicationClosePhase) ||
        project?.project_id !== boundProject.project_id ||
        project.session_id !== boundProject.session_id
      ) return;
      suggestionsEnabled = enabled;
      if (engineBecameDisabled) clearCompletionSession();
      if (persist) {
        try {
          window.localStorage.setItem(suggestionPreferenceKey(project.project_id), enabled ? 'on' : 'off');
        } catch {
          // The backend gate remains authoritative if browser persistence is unavailable.
        }
      }
      if (!automationEnabled) {
        clearPreferredWriterRequest();
        scheduleActiveBranchPoll();
      }
      let writerReady = Boolean(currentModel);
      if (automationEnabled && !writerReady) {
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
      if (engineBecameEnabled && writerReady && document) {
        scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'explicit_enable');
      }
    } catch (error) {
      suggestionsEnabled = previousEnabled;
      completionController = setDismissedCompletionCandidates(
        completionController,
        previousDismissedCandidateIds
      );
      clearPreferredWriterRequest();
      cancelSuggestionTimer();
      if (!enabled && activeBranchCount > 0) void cancelActiveBranches();
      recordFailure(error);
      announce('Suggestions remain off because the project gate could not be changed');
    } finally {
      suggestionsChanging = false;
    }
  }

  async function toggleSuggestionsFromTitlebar(): Promise<void> {
    await setSuggestionsEnabled(!suggestionsEnabled);
    await tick();
    if (mode === 'source') sourceEditor?.focusCurrentSelection();
    else visualEditor?.focusCurrentSelection();
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
    quietModelLoadFailure = null;
    if (modelSetupError.startsWith('Automatic writer setup failed.')) modelSetupError = '';
    selectedModelPath = loaded.model_path;
    rememberLastLocalModelPath(loaded.model_path);
    if (!quiet) {
      announce(`${loaded.display_name} is verified for exact local completion`);
    }
    if (completionAutomationEnabled() && loaded.completion && document) {
      await tick();
      if (!applicationAllowsModelPreparation(applicationClosePhase)) return false;
      if (quiet) announce('Suggestions ready');
      scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'model_ready');
    }
    return true;
  }

  async function loadPreferredSuggestionModel(
    expectedWorkspace?: WorkspaceRestoreCapture
  ): Promise<boolean> {
    const captured = expectedWorkspace ?? currentWorkspaceCapture();
    if (!captured || !applicationAllowsModelPreparation(applicationClosePhase) ||
      !await prepareWorkspaceWriterTemplate(captured)) return false;
    if (currentModel) return true;
    const requestKey = workspaceWriterKey();
    const selection = workspaceTemplate?.config.model;
    const selectionIsCurrent = () => workspaceRestoreIsCurrent(captured) && workspaceWriterKey() === requestKey;
    if (unavailableWorkspaceWriterKey === requestKey || !workspaceWriterSwapIsIdle()) return false;
    if (!document || transition !== 'idle' || modelLoading || modelUnloading) return false;
    if (!selectionIsCurrent()) return false;
    quietModelLoadFailure = null;
    // Existing local Gemma files must never race the embedded catalog and be
    // admitted as a generic text-only model before the pinned projector is
    // known. All callers share the same in-flight catalog read.
    if (curatedModels.length === 0) await refreshCuratedModels();
    await tick();
    if (!selectionIsCurrent()) return false;
    if (currentModel) return true;
    if (selection && models.some((model) => model.loaded)) {
      if (!workspaceWriterSwapIsIdle()) return false;
      modelUnloading = true;
      try {
        await unloadModel();
        await refreshModels(captured);
      } catch (error) {
        if (selectionIsCurrent()) {
          quietModelLoadFailure = normalizeFailure(error);
          unavailableWorkspaceWriterKey = requestKey;
        }
        return false;
      } finally { modelUnloading = false; }
      if (!selectionIsCurrent()) return false;
    }
    const rememberedPath = loadLastLocalModelPath();
    const candidates = workspaceWriterCandidates(selection, models, curatedModels, rememberedPath);
    let terminalFailure: LoomFailure | null = null;
    for (const candidate of candidates) {
      if (!applicationAllowsModelPreparation(applicationClosePhase)) return false;
      if (!selectionIsCurrent()) return false;
      const loadSerial = ++modelLoadSerial;
      modelLoading = true;
      try {
        const discovered = models.find((model) => model.model_path === candidate.modelPath);
        const catalogEntry = candidate.catalogId
          ? curatedModels.find((entry) => entry.catalog_id === candidate.catalogId) ?? null
          : discovered ? curatedModels.find((entry) => legacyLocalCatalogMatch(entry, discovered)) ?? null : null;
        if (selection && !candidate.profileId && !catalogEntry) continue;
        if (discovered && isOfficialGemma4CatalogHint(discovered) && !catalogEntry) {
          continue;
        }
        const loaded = candidate.profileId
          ? await loadPolicyModelCandidate(candidate.profileId, candidate.modelPath)
          : catalogEntry
            ? await loadCatalogModelCandidate(catalogEntry.catalog_id, candidate.modelPath)
            : await loadModel(candidate.modelPath);
        if (
          !componentMounted ||
          !applicationAllowsModelPreparation(applicationClosePhase) ||
          loadSerial !== modelLoadSerial ||
          !selectionIsCurrent()
        ) return false;
        if (candidate.profileId
          ? !isVerifiedPolicyWriter(loaded, candidate.profileId)
          : catalogEntry
            ? !isVerifiedCatalogWriter(catalogEntry, loaded)
            : !isUsableSuggestionWriter(loaded)) {
          if (candidate.remembered && !candidate.profileId) {
            forgetLastLocalModelPath(candidate.modelPath);
          }
          await refreshModels(captured);
          continue;
        }
        return await installLoadedModel(loaded, true, captured);
      } catch (error) {
        if (
          !componentMounted ||
          !applicationAllowsModelPreparation(applicationClosePhase) ||
          loadSerial !== modelLoadSerial ||
          !selectionIsCurrent()
        ) return false;
        const failure = normalizeFailure(error);
        terminalFailure = failure;
        if (candidate.remembered && (
          rememberedWriterPathIsInvalid(failure.code) ||
          (!candidate.profileId && ['model_path_error', 'model_header_unverified'].includes(failure.code))
        )) {
          forgetLastLocalModelPath(candidate.modelPath);
        }
        await refreshModels(captured);
      } finally {
        if (loadSerial === modelLoadSerial) {
          modelLoading = false;
          wakePreferredWriterEnsure();
        }
      }
    }
    if (selectionIsCurrent()) {
      unavailableWorkspaceWriterKey = requestKey;
      if (selection && !terminalFailure) terminalFailure = {
        code: 'workspace_model_unavailable', message: 'The model named in .loom.md is not available in the local model library.', retryable: false
      };
    }
    if (
      terminalFailure &&
      componentMounted &&
      applicationAllowsModelPreparation(applicationClosePhase) &&
      selectionIsCurrent()
    ) {
      quietModelLoadFailure = terminalFailure;
      modelSetupError = `Automatic writer setup failed. ${terminalFailure.message}`;
      announce('Ghost text is unavailable.');
    }
    return false;
  }

  function writerSetupName(model: ModelCapabilitySummary): string {
    return model.display_name.toLocaleLowerCase('en-US') === 'gemma-4-12b-it-qat-q4_0.gguf'
      ? 'Gemma 4 12B QAT'
      : model.display_name;
  }

  async function activateSuggestionWriter(
    selected: ModelCapabilitySummary,
    captured: WorkspaceRestoreCapture,
    catalogEntry: CuratedModelCatalogEntry | null = null
  ): Promise<boolean> {
    if (modelLoading || modelUnloading || !workspaceRestoreIsCurrent(captured)) return false;
    if (catalogEntry && !legacyLocalCatalogMatch(catalogEntry, selected)) {
      modelSetupError = 'That local file no longer matches the embedded catalog hint.';
      announce('The catalog model changed before verification');
      return false;
    }
    const policyCandidate = selected.policy_candidate;
    if (looksLikeVisionAdapter(selected)) {
      modelSetupError = 'That file is a vision or projector adapter, not a standalone writing model.';
      announce('Choose a standalone GGUF language model');
      return false;
    }

    models = [selected, ...models.filter((model) => model.model_path !== selected.model_path)];
    selectedModelPath = selected.model_path;
    const loadSerial = ++modelLoadSerial;
    modelLoading = true;
    modelSetupError = '';
    announce('Inspecting the writing model locally');
    try {
      const loaded = catalogEntry
        ? await loadCatalogModelCandidate(catalogEntry.catalog_id, selected.model_path)
        : policyCandidate
          ? await loadPolicyModelCandidate(policyCandidate.profile_id, selected.model_path)
          : await loadModel(selected.model_path);
      if (
        !componentMounted ||
        !applicationAllowsModelPreparation(applicationClosePhase) ||
        loadSerial !== modelLoadSerial ||
        !workspaceRestoreIsCurrent(captured)
      ) return false;
      if (catalogEntry
        ? !isVerifiedCatalogWriter(catalogEntry, loaded)
        : policyCandidate
          ? !isVerifiedPolicyWriter(loaded, policyCandidate.profile_id)
          : !isUsableSuggestionWriter(loaded)) {
        throw new Error('Native inspection did not find a text-completion model suitable for writing suggestions.');
      }
      if (!(await installLoadedModel(loaded, true, captured))) {
        throw new Error('The verified writer could not be attached to the current writing session.');
      }
      clearFailure();
      modelSetupError = '';
      announce(`${writerSetupName(selected)} is ready to suggest writing`);
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
        modelSetupError = `Loom could not use this local model for writing suggestions. ${failure.message}`;
        announce('That file could not be loaded as a text writing model');
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

  async function chooseSuggestionWriterModel(): Promise<void> {
    if (modelChoosing || modelLoading || modelUnloading) return;
    const captured = currentWorkspaceCapture();
    if (!captured) return;

    modelChoosing = true;
    modelSetupError = '';
    announce('Locate a local GGUF writing model');
    try {
      const selected = await chooseModel();
      if (!selected) {
        announce('Writing model choice cancelled');
        return;
      }
      if (!workspaceRestoreIsCurrent(captured)) return;
      await activateSuggestionWriter(selected, captured);
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

  async function useDiscoveredSuggestionWriter(model: ModelCapabilitySummary): Promise<void> {
    const captured = currentWorkspaceCapture();
    if (!captured) return;
    await activateSuggestionWriter(model, captured);
  }

  async function useCatalogSuggestionWriter(
    entry: CuratedModelCatalogEntry,
    model: ModelCapabilitySummary
  ): Promise<void> {
    const captured = currentWorkspaceCapture();
    if (!captured) return;
    await activateSuggestionWriter(model, captured, entry);
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
    const restoreSerial = workspaceRestoreSerial;
    try {
      const current = await attachWorkspaceProjectReply({
        open: currentProjectSession,
        mayAttach: () => Boolean(
          componentMounted &&
          applicationClosePhase === 'running' &&
          restoreSerial === workspaceRestoreSerial
        ),
        attach: (opened) => { project = opened; },
        onHeld: holdWorkspaceForApplicationClose
      });
      if (!current) return false;
      if (!(await finishOpeningProject(current, restoreSerial))) return false;
      const captured = currentWorkspaceCapture();
      if (captured) void restoreCompletionBackground(captured);
      announce(`Reattached ${current.title}`);
      return true;
    } catch {
      return false;
    }
  }

  async function openInitialProject(restoreSerial: number): Promise<WorkspaceRestoreCapture | null> {
    const startupIsCurrent = () => Boolean(
      componentMounted &&
      restoreSerial === workspaceRestoreSerial &&
      applicationClosePhase === 'running'
    );
    try {
      const acquisition = await acquireStartupProject({
        currentProject: currentProjectSession,
        openDefaultProject,
        mayContinue: startupIsCurrent,
        projectIsAbsent: (error) => normalizeFailure(error).code === 'project_not_open',
        onHeld: holdWorkspaceForApplicationClose
      });
      if (!acquisition || !startupIsCurrent()) {
        holdWorkspaceForApplicationClose();
        return null;
      }
      const opened = acquisition.project;
      project = opened;
      if (!(await finishOpeningProject(opened, restoreSerial)) || !startupIsCurrent()) {
        holdWorkspaceForApplicationClose();
        return null;
      }
      if (
        project?.project_id !== opened.project_id ||
        project.session_id !== opened.session_id
      ) return null;
      announce(acquisition.source === 'current' ? `Reattached ${opened.title}` : 'Ready to write');
      return {
        restoreSerial,
        projectId: opened.project_id,
        sessionId: opened.session_id
      };
    } catch (error) {
      if (startupIsCurrent()) recordFailure(error);
      else holdWorkspaceForApplicationClose();
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

  async function restoreCompletionAutomation(
    captured: WorkspaceRestoreCapture
  ): Promise<boolean> {
    if (!workspaceRestoreIsCurrent(captured)) return false;
    try {
      const identity = await getBuildModelPolicy();
      if (!workspaceRestoreIsCurrent(captured)) return false;
      buildModelPolicy = identity;
    } catch {
      if (!workspaceRestoreIsCurrent(captured)) return false;
      buildModelPolicy = null;
      suggestionsEnabled = false;
      announce('Suggestions are off because this build could not verify its local writer policy');
      return false;
    }

    const storedSuggestionsPreference = loadSuggestionPreference(captured.projectId);
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
      return false;
    }

    if (suggestionsEnabled && currentModel && document) {
      scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'document_open');
    } else if (suggestionsEnabled && !currentModel) {
      queuePreferredWriterRequest(captured);
    }
    return true;
  }

  async function restoreCompletionBackground(
    captured: WorkspaceRestoreCapture
  ): Promise<void> {
    await restoreCompletionAutomation(captured);
    if (!workspaceRestoreIsCurrent(captured)) return;
    await recoverModelDownloads();
    if (!workspaceRestoreIsCurrent(captured)) return;
    if (!shouldDiscoverModelsOnStartup(completionAutomationEnabled())) return;
    requestPreferredWriterEnsure(captured);
  }

  async function restoreDesktopWorkspace(): Promise<void> {
    const restoreSerial = ++workspaceRestoreSerial;
    await restoreBeforeBackgroundWork({
      restore: () => openInitialProject(restoreSerial),
      present: async (captured) => {
        await tick();
        if (!workspaceRestoreIsCurrent(captured)) {
          holdWorkspaceForApplicationClose();
          return;
        }
        await waitForWritingSurfacePaint();
        if (!workspaceRestoreIsCurrent(captured)) {
          holdWorkspaceForApplicationClose();
          return;
        }
        focusCurrentWritingSurfaceAtEnd();
      },
      isCurrent: workspaceRestoreIsCurrent,
      background: async (captured) => {
        await restoreCompletionBackground(captured);
      },
      onInterrupted: () => {
        holdWorkspaceForApplicationClose();
      }
    });
  }

  async function doOpenProject(source?: string | (() => Promise<string | null>)): Promise<boolean> {
    if (fileCommandInFlight || opening || applicationClosePhase !== 'running') return false;
    opening = true;
    fileCommandInFlight = true;
    let preparationId: string | null = null;
    let handoffStarted = false;
    try {
      // The chooser and validation leave the current editor/session intact.
      preparationId = await (typeof source === 'function' ? source() : source ? prepareProjectOpenPath(source) : prepareProjectOpen());
      if (!preparationId) return typeof source === 'string';
      if (!componentMounted || applicationClosePhase !== 'running') return false;
      clearFailure();
      if (project) {
        if (document) workspaceDocuments = { ...workspaceDocuments, [project.root]: document.summary.document_id };
        const outcome = await closeProject();
        if (outcome.status !== 'closed') return false;
      }
      if (!componentMounted || applicationClosePhase !== 'running') return false;
      const restoreSerial = ++workspaceRestoreSerial;
      modelRefreshSerial += 1;
      handoffStarted = true;
      const selectedPreparation = preparationId;
      const opened = await attachWorkspaceProjectReply({
        open: () => commitProjectOpen(selectedPreparation),
        mayAttach: () => Boolean(
          componentMounted &&
          applicationClosePhase === 'running' &&
          restoreSerial === workspaceRestoreSerial
        ),
        attach: (selected) => { project = selected; },
        onHeld: holdWorkspaceForApplicationClose
      });
      if (!opened) return false;
      if (await finishOpeningProject(opened, restoreSerial)) {
        const captured = currentWorkspaceCapture();
        if (!captured || !workspaceRestoreIsCurrent(captured)) {
          holdWorkspaceForApplicationClose();
          return false;
        }
        await tick();
        if (!workspaceRestoreIsCurrent(captured)) {
          holdWorkspaceForApplicationClose();
          return false;
        }
        await waitForWritingSurfacePaint();
        if (!workspaceRestoreIsCurrent(captured)) {
          holdWorkspaceForApplicationClose();
          return false;
        }
        focusCurrentWritingSurfaceAtEnd();
        if (workspaceRestoreIsCurrent(captured)) void restoreCompletionBackground(captured);
        outlineOpen = true;
        workspaceRootExpanded = true;
        announce(`Opened ${opened.title}`);
        return true;
      }
    } catch (error) {
      // Only an uncertain commit needs native reattachment. Picker/validation
      // failures must not replace the current editor or conceal the failure.
      if (handoffStarted) await reattachNativeProject();
      recordFailure(error);
    } finally {
      if (preparationId) {
        try {
          await discardProjectOpen(preparationId);
        } catch (error) {
          recordFailure(error);
        }
      }
      opening = false;
      fileCommandInFlight = false;
    }
    return false;
  }

  async function openAnotherProject(): Promise<void> {
    await doOpenProject();
  }

  async function retryInitialProject(): Promise<void> {
    if (opening || fileCommandInFlight || project || applicationClosePhase !== 'running') return;
    opening = true;
    clearFailure();
    try {
      await restoreDesktopWorkspace();
    } finally {
      opening = false;
    }
  }

  async function newDocument(): Promise<void> {
    if (!project || fileCommandInFlight || editorReadonly) return;
    if (!flushEditors()) return;
    fileCommandInFlight = true;
    try {
      await saveNow();
      if (!project || uncertainSave || saveState === 'error' || saveState === 'uncertain') return;
      const previousDocumentIds = new Set(project.documents.map((candidate) => candidate.document_id));
      const refreshed = await createDocument(project.project_id, project.session_id);
      if (
        !project ||
        refreshed.project_id !== project.project_id ||
        refreshed.session_id !== project.session_id
      ) return;
      const created = refreshed.documents.find(
        (candidate) => !previousDocumentIds.has(candidate.document_id)
      );
      if (!created) throw new Error('The desktop did not expose the newly created document.');
      project = refreshed;
      await selectDocument(created, true);
      announce(`Created ${created.title}. Changes autosave as you write.`);
    } catch (error) {
      recordFailure(error);
    } finally {
      fileCommandInFlight = false;
    }
  }

  async function exportCopy(): Promise<void> {
    if (!project || !document || fileCommandInFlight || editorReadonly) return;
    if (!flushEditors()) return;
    fileCommandInFlight = true;
    try {
      await saveNow();
      if (!project || !document || uncertainSave || saveState === 'error' || saveState === 'uncertain') return;
      if (!document.summary.revision_id || !document.summary.active_blob_id) {
        throw new Error('The active document does not expose a complete export identity.');
      }
      const receipt = await exportDocumentCopy(
        project.project_id,
        project.session_id,
        document.summary.document_id,
        document.summary.revision_id,
        document.summary.active_blob_id
      );
      if (receipt) announce(`Exported ${document.summary.title} as text`);
    } catch (error) {
      recordFailure(error);
    } finally {
      fileCommandInFlight = false;
    }
  }

  async function installFileCommandListener(): Promise<void> {
    try {
      const unlisten = await listenForFileCommands(({ command }) => {
        switch (command) {
          case 'new_document':
            void newDocument();
            break;
          case 'open_project':
            void openAnotherProject();
            break;
          case 'save':
            if (flushEditors()) void saveNow();
            break;
          case 'export_copy':
            void exportCopy();
            break;
        }
      });
      if (!componentMounted) {
        unlisten();
        return;
      }
      unlistenFileCommands?.();
      unlistenFileCommands = unlisten;
    } catch (error) {
      if (componentMounted) recordFailure(error);
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
    closeDocumentContextMenu(false);
    closeDocumentDeleteConfirmation(false);
    if (missingDocumentCapturePending) {
      recordLocalFailure(
        'missing_document_capture_pending',
        'Loom refused to replace the workspace while a missing manuscript still needs preservation.'
      );
      return false;
    }
    const unsafeRecovery = missingDocumentRecoveryRequiresCopy(
      missingDocumentRecovery && {
        documentId: missingDocumentRecovery.documentId,
        journalDurable: missingDocumentRecovery.journalDurable,
        copied: missingDocumentCopyState === 'copied'
      }
    );
    if (
      unsafeRecovery &&
      missingDocumentRecovery &&
      (
        missingDocumentRecovery.projectId !== opened.project_id ||
        missingDocumentRecovery.sessionId !== opened.session_id
      )
    ) {
      recordLocalFailure(
        'missing_document_recovery_not_durable',
        'Loom refused to replace the workspace before its preserved manuscript text was copied.'
      );
      return false;
    }
    if (!unsafeRecovery) {
      missingDocumentRecovery = null;
      missingDocumentCopyState = 'idle';
    }
    let folderStorage: Storage | undefined;
    try { folderStorage = window.localStorage; } catch { /* Workspace navigation does not require storage. */ }
    workspaceFolders = rememberWorkspaceFolder(workspaceFolders, { root: opened.root, title: opened.title }, folderStorage);
    workspaceRootExpanded = true;
    hiddenPaneSlots = new Set();
    paneSelection = {};
    clearPreferredWriterRequest();
    cancelSuggestionTimer();
    clearCompletionSession();
    suggestionsEnabled = false;
    shuttleEnabled = false;
    completionController = resetCompletionDiscovery(completionController);
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
    const first = project.documents.find(item => item.document_id === workspaceDocuments[opened.root])
      ?? project.documents.find(item => !item.relative_path.split('/').at(-1)?.startsWith('.'))
      ?? project.documents[0];
    if (first) {
      await selectDocument(first);
      if (!workspaceRestoreIsCurrent(captured)) return false;
      await tick();
      if (!workspaceRestoreIsCurrent(captured)) return false;
    } else {
      documentEpoch += 1;
      document = null;
      documentText = '';
      sourceDisplayText = '';
      sourceCodec = null;
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
    if (!workspaceRestoreIsCurrent(captured)) return false;
    // Close the listener/snapshot handoff gap: a watcher hint emitted before
    // this project became current was correctly ignored, so pull once now.
    scheduleProjectFilesystemRefresh(0);
    return true;
  }

  async function selectDocument(
    summary: DocumentSummary,
    focusWritingSurface = false
  ): Promise<void> {
    if (!project) return;
    const target = captureDocumentTarget(project, summary);
    if (!target) {
      recordLocalFailure(
        'incomplete_document_identity',
        `${summary.title} does not expose the complete active revision required to open it safely.`
      );
      return;
    }
    await selectCapturedDocument(target, focusWritingSurface);
  }

  async function selectCapturedDocument(
    target: CapturedDocumentTarget,
    focusWritingSurface = false,
    fromContextMenu = false
  ): Promise<void> {
    if (
      transition !== 'idle' ||
      applicationClosePhase !== 'running' ||
      !capturedDocumentBelongsToSession(target, project)
    ) return;
    if (renamingDocumentId && renamingDocumentId !== target.documentId) {
      cancelDocumentRename(false);
    }
    const requestedScope: ProjectRestoreScope = {
      projectId: target.projectId,
      sessionId: target.sessionId,
      restoreSerial: workspaceRestoreSerial
    };
    const projectNavigationIsCurrent = () => Boolean(
      applicationClosePhase === 'running' &&
      projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, requestedScope)
    );
    const targetWasCurrent = Boolean(
      document?.summary.document_id === target.documentId &&
      document.summary.revision_id === target.expectedRevisionId &&
      document.summary.active_blob_id === target.expectedBlobId
    );
    if (compositionActive) {
      announce('Finish composing text before changing documents');
      return;
    }
    if (!flushEditors()) return;
    cancelSuggestionTimer();
    clearCompletionSession();
    completionController = resetCompletionDiscovery(completionController);
    transition = 'navigation';
    announce('Opening document; editing is briefly locked');
    const requestSerial = ++navigationSerial;
    if (!(await flushDraftJournal())) {
      if (
        requestSerial === navigationSerial &&
        projectNavigationIsCurrent()
      ) transition = 'idle';
      return;
    }
    if (!projectNavigationIsCurrent()) return;
    if (!targetWasCurrent && !(await flushCurrentDocument())) {
      if (
        requestSerial === navigationSerial &&
        projectNavigationIsCurrent()
      ) transition = 'idle';
      return;
    }
    if (!projectNavigationIsCurrent()) return;
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
      if (target.externallyModified) {
        const imported = !targetWasCurrent || editVersion === savedVersion
          ? await importExternalDocument(source.projectId, source.sessionId, target.documentId,
              target.expectedRevisionId, target.expectedBlobId)
          : null;
        if (!projectNavigationIsCurrent() || requestSerial !== navigationSerial) return;
        if (imported && project) {
          project = { ...project, documents: project.documents.map(item =>
            item.document_id === imported.document_id ? imported : item) };
          const fresh = captureDocumentTarget(project, imported);
          if (!fresh) throw new Error('External document returned an incomplete identity.');
          target = fresh;
        } else {
          const preview = await requestReconciliationPreview({
            document_id: target.documentId, kind: target.kind,
            revision_id: target.expectedRevisionId, active_blob_id: target.expectedBlobId
          }, targetWasCurrent ? documentText : null, source);
          if (!projectNavigationIsCurrent() || requestSerial !== navigationSerial) return;
          documentEpoch += 1;
          document = null;
          documentText = '';
          sourceDisplayText = '';
          sourceCodec = null;
          editVersion = 0;
          savedVersion = 0;
          draftVersion = '0';
          draftSavedEditVersion = 0;
          staleDraft = null;
          uncertainDraft = null;
          activateReconciliation(preview);
          return;
        }
      }
      const opened = await openDocument(
        source.projectId,
        source.sessionId,
        target.documentId,
        target.expectedRevisionId,
        target.expectedBlobId
      );
      if (
        applicationClosePhase !== 'running' ||
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
      if (
        opened.summary.document_id !== target.documentId ||
        opened.summary.revision_id !== target.expectedRevisionId ||
        opened.summary.active_blob_id !== target.expectedBlobId
      ) {
        throw new Error('The desktop returned a different document identity.');
      }
      if (opened.visible_blob_id !== target.expectedBlobId) {
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
      if (
        missingDocumentRecovery &&
        openedDocumentSubsumesMissingRecovery(
          {
            documentId: missingDocumentRecovery.documentId,
            text: missingDocumentRecovery.text,
            journalDurable: missingDocumentRecovery.journalDurable,
            copied: missingDocumentCopyState === 'copied'
          },
          opened.summary.document_id,
          effectiveText
        )
      ) {
        missingDocumentRecovery = null;
        missingDocumentCopyState = 'idle';
      }
      documentText = effectiveText;
      setSourceDocument(effectiveText);
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
        announce(`Recovered a local draft for ${target.title}`);
      } else {
        saveState = 'clean';
        saveMessage = 'All changes saved';
        announce(`Opened ${target.title}`);
      }
      mode = opened.summary.kind === 'prose' && canUseVisualMarkdown(effectiveText, false)
        ? preferredProseMode
        : 'source';
      void refreshBranchesFor(
        source.projectId,
        source.sessionId,
        opened.summary.document_id,
        false
      );
    } catch (error) {
      if (
        applicationClosePhase !== 'running' ||
        !projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, source)
      ) return;
      if (normalizeFailure(error).code === 'external_file_deleted') {
        clearFailure();
        announce(`${target.title} was deleted outside Loom; refreshing the outline`);
        scheduleProjectFilesystemRefresh(0);
        return;
      }
      let reportedError = error;
      if (normalizeFailure(error).code === 'external_file_change') {
        try {
          const refreshed = await currentProjectSession();
          if (
            refreshed.project_id === target.projectId &&
            refreshed.session_id === target.sessionId &&
            requestSerial === navigationSerial &&
            navigationScopeIsCurrent(
              project,
              document,
              documentEpoch,
              editVersion,
              workspaceRestoreSerial,
              source
            )
          ) {
            const changedTarget = refreshed.documents.find(
              (candidate) => candidate.document_id === target.documentId
            );
            if (
              changedTarget?.externally_modified &&
              changedTarget.revision_id === target.expectedRevisionId &&
              changedTarget.active_blob_id === target.expectedBlobId
            ) {
              project = refreshed;
              const preview = await requestReconciliationPreview(changedTarget, null, source);
              if (
                requestSerial === navigationSerial &&
                navigationScopeIsCurrent(
                  project,
                  document,
                  documentEpoch,
                  editVersion,
                  workspaceRestoreSerial,
                  source
                )
              ) {
                documentEpoch += 1;
                activateReconciliation(preview);
                return;
              }
            }
          }
        } catch (reconciliationError) {
          reportedError = reconciliationError;
        }
      }
      if (fromContextMenu) recordDocumentContextFailure(target, reportedError);
      else recordFailure(reportedError);
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
        applicationClosePhase === 'running' &&
        requestSerial === navigationSerial &&
        projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, source)
      ) {
        transition = 'idle';
        wakePreferredWriterEnsure();
        if (focusWritingSurface && document?.summary.document_id === target.documentId) {
          await tick();
          if (
            applicationClosePhase === 'running' &&
            requestSerial === navigationSerial &&
            projectRestoreScopeIsCurrent(project, workspaceRestoreSerial, source) &&
            document?.summary.document_id === target.documentId
          ) focusCurrentWritingSurfaceAtEnd();
        }
      }
    }
  }

  function updateText(text: string): void {
    if (transition !== 'idle') return;
    const mutationWasInvalidated = visualMutationPending;
    const mutation = observeTextMutation(
      completionController,
      text,
      documentText,
      mutationWasInvalidated
    );
    completionController = mutation.state;
    visualMutationPending = false;
    if (text === documentText) return;
    documentText = text;
    editVersion += 1;
    if (cabalEditor && !cabalApplying) cabalEditor.change(text);
    uncertainWeave = null;
    saveState = 'dirty';
    saveMessage = saveInFlight ? 'Saving earlier changes…' : 'Unsaved changes';
    promotionArmedCandidateId = null;
    if (mutation.cancelActiveBranches && activeBranchCount > 0) void cancelActiveBranches();
    scheduleDraftJournal();
    scheduleSave();
    if (!mutation.completionOwned) scheduleAutomaticSuggestions(editVersion);
  }

  function clearCompletionSession(): void {
    completionController = clearCompletionControllerSession(completionController);
  }

  function invalidateVisualSuggestionImmediately(): void {
    if (transition !== 'idle' || visualMutationPending) return;
    if (completionController.pendingText !== null) return;
    visualMutationPending = true;
    const invalidated = invalidateVisualMutation(completionController);
    completionController = invalidated.state;
    uncertainWeave = null;
    promotionArmedCandidateId = null;
    applyCompletionEffects(invalidated.effects);
  }

  function setSourceDocument(text: string): void {
    completionController = resetCompletionSurface(completionController);
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
    visualBoundaryFailure = 'uninitialized';
    visualBoundaryDiagnostic = null;
    visualMutationPending = false;
    const decoded = decodeSourceForEditor(text);
    sourceCodec = decoded.codec;
    sourceDisplayText = decoded.display;
  }

  function flushEditors(): boolean {
    if (Object.values(paneEditors).some(pane => pane && !pane.flush())) return false;
    if (contextPaneOpen && !flushContextEditorProjection()) return false;
    if (!(visualEditor?.flushPending() ?? true) || sourceComposing) return false;
    commitSourceDraft();
    return true;
  }

  function beginSourceComposition(): void {
    sourceComposing = true;
    mainCompositionActive = true;
  }

  function finishSourceComposition(textarea: HTMLTextAreaElement): void {
    sourceComposing = false;
    mainCompositionActive = false;
    updateSourceSelection(textarea);
    updateFromSource(textarea.value);
    scheduleSourceProjection(0);
    announce('Text composition committed');
  }

  function updateFromSource(display: string): void {
    if (transition !== 'idle') return;
    sourceDisplayText = display;
    sourceDirty = true;
    if (!sourceComposing) {
      // Completion-owned textarea mutations are exact and already authorized
      // synchronously. Project them in the same event turn so the cached
      // remainder and reversal affordance never disappear behind the ordinary
      // source-edit debounce.
      if (completionController.pendingText !== null) commitSourceDraft();
      else scheduleSourceProjection();
    }
  }

  async function storeImageAttachments(files: readonly File[]): Promise<readonly string[]> {
    if (!project || !document || editorReadonly || files.length === 0) return [];
    const transferError = imageAttachmentTransferError(files);
    if (transferError) {
      reportImageAttachmentError(transferError);
      return [];
    }
    const captured = {
      projectId: project.project_id,
      sessionId: project.session_id,
      documentId: document.summary.document_id,
      relativePath: document.summary.relative_path,
      documentEpoch
    };
    const snippets: string[] = [];
    try {
      // Encode and transmit one image at a time. A single paste/drop therefore
      // never retains every ArrayBuffer, binary string, and base64 payload at
      // once, even at the explicit transfer ceiling.
      for (const file of files) {
        const encoded = await encodeImageAttachment(file);
        const receipt = await ingestImageAttachment(
          captured.projectId,
          captured.sessionId,
          encoded.mediaType,
          encoded.base64
        );
        if (
          project?.project_id !== captured.projectId ||
          project.session_id !== captured.sessionId ||
          document?.summary.document_id !== captured.documentId ||
          documentEpoch !== captured.documentEpoch
        ) {
          const message =
            'The image was stored in the original project, but the manuscript changed before insertion.';
          recordLocalFailure('image_attachment_stale', message);
          announce(message);
          return [];
        }
        snippets.push(attachmentMarkdown(receipt, encoded.originalName, captured.relativePath));
      }
      return snippets;
    } catch (error) {
      recordFailure(error);
      if (snippets.length > 0) {
        return snippets;
      }
      announce('The image could not be attached; your manuscript is unchanged');
      return [];
    }
  }

  function resolveImageAssetUrl(markdownPath: string): string | null {
    if (!project) return null;
    const media = /^loom-attachment:([a-f0-9]{64})\/([a-f0-9]{64})$/.exec(markdownPath);
    if (media && document) {
      return convertFileSrc(`v2-${project.project_id}-${project.session_id}-${document.summary.document_id}-${media[1]}-${media[2]}`, 'loom-asset');
    }
    const token = projectAssetProtocolToken(
      project.project_id,
      project.session_id,
      markdownPath
    );
    return token ? convertFileSrc(token, 'loom-asset') : null;
  }

  function reportImageAttachmentError(message: string): void {
    recordLocalFailure('image_attachment_unreadable', message);
    announce(message);
  }

  function reportImageAttachmentsCommitted(count: number): void {
    if (!Number.isSafeInteger(count) || count <= 0) return;
    announce(count === 1 ? 'Image attached' : `${count} images attached`);
  }

  function updateSourceSelection(textarea: HTMLTextAreaElement): void {
    const previousStart = sourceSelectionStart;
    const previousEnd = sourceSelectionEnd;
    const completionWasActive = completionActivityExists();
    sourceSelectionStart = textarea.selectionStart;
    sourceSelectionEnd = textarea.selectionEnd;
    if (
      completionController.pendingText !== null ||
      (previousStart === sourceSelectionStart && previousEnd === sourceSelectionEnd)
    ) return;
    const target = sourceGhostTargetByteFor(
      mode,
      true,
      sourceSelectionStart,
      sourceSelectionEnd,
      sourceDisplayText,
      document,
      documentText,
      sourceCodec
    );
    const expected = completionController.session
      ? completionSessionPresentation(completionController.session)?.targetByte ?? null
      : selectedInlineSuggestion?.targetByte ?? completionController.generationIntent?.anchorByte ?? null;
    if (expected === null && target !== null) {
      if (completionWasActive) {
        completionController = bindCompletionAnchor(
          completionController,
          completionContextKey,
          editVersion,
          target
        );
      } else {
        scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'caret_navigation');
      }
      return;
    }
    if (target !== null && expected !== null && target !== expected) {
      invalidateCompletionForCaretNavigation();
      scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'caret_navigation');
    }
  }

  function updateVisualSelection(
    markdownByteOffset: number | null,
    failure: VisualCaretBoundaryFailure | 'selection_settling' | null = null,
    diagnostic: string | null = null
  ): void {
    const previous = visualSelectionByte;
    const completionWasActive = completionActivityExists();
    visualSelectionByte = markdownByteOffset;
    visualBoundaryFailure = failure;
    visualBoundaryDiagnostic = diagnostic;
    const settledNavigation = settleCompletionNavigation(
      completionController,
      markdownByteOffset !== null
    );
    completionController = settledNavigation.state;
    if (settledNavigation.scheduleFresh) {
      scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'caret_navigation');
      return;
    }
    if (
      completionController.pendingText !== null ||
      markdownByteOffset === null ||
      markdownByteOffset === previous ||
      !completionWasActive
    ) return;
    const expected = completionController.session
      ? completionSessionPresentation(completionController.session)?.targetByte ?? null
      : selectedInlineSuggestion?.targetByte ?? completionController.generationIntent?.anchorByte ?? null;
    if (expected === null) {
      completionController = bindCompletionAnchor(
        completionController,
        completionContextKey,
        editVersion,
        markdownByteOffset
      );
      return;
    }
    if (expected !== null && markdownByteOffset !== expected) {
      invalidateCompletionForCaretNavigation();
    }
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
    if (!sourceCodec?.editable) return;
    updateText(encodeSourceFromEditor(sourceDisplayText, sourceCodec));
  }

  function setVisualComposition(active: boolean): void {
    mainCompositionActive = active;
    if (!active) scheduleSave();
  }

  function scheduleSave(): void {
    if (!desktop || !document) return;
    if (cabalEditor) return;
    if (saveTimer !== undefined) window.clearTimeout(saveTimer);
    saveTimer = window.setTimeout(() => void saveNow(), saveDelayMs);
  }

  function scheduleAutomaticSuggestions(
    targetEditVersion: number,
    delay = suggestionsIdleDelayMs,
    trigger: CompletionGenerationTrigger = 'document_edit'
  ): void {
    const armed = armCompletionScheduleIntent(
      completionController,
      completionContextKey,
      targetEditVersion,
      trigger,
      mode === 'visual' ? visualSelectionByte : sourceGhostTargetByte
    );
    completionController = armed;
    if (!armed.generationIntent) return;
    armSuggestionSchedule({ kind: 'edit_pause', editVersion: targetEditVersion }, delay);
  }

  function scheduleAutocompleteRetry(ticket: AutocompleteRetryTicket): void {
    if (!completionGenerationIsArmed(
      completionController.generationIntent,
      completionContextKey,
      ticket.editVersion
    )) return;
    armSuggestionSchedule(
      { kind: 'exhausted_retry', ticket },
      suggestionsRetryDelayMs
    );
  }

  function armSuggestionSchedule(schedule: CompletionSchedule, delay: number): void {
    clearSuggestionTimerHandle();
    completionController = setCompletionSchedule(completionController, null);
    if (
      !desktop ||
      !completionAutomationEnabled() ||
      !project ||
      !document ||
      !completionGenerationIsArmed(
        completionController.generationIntent,
        completionContextKey,
        editVersion
      ) ||
      document.summary.kind === 'hybrid'
    ) return;
    completionController = setCompletionSchedule(completionController, schedule);
    queueScheduledSuggestionAttempt(schedule, delay);
  }

  function queueScheduledSuggestionAttempt(schedule: CompletionSchedule, delay: number): void {
    if (suggestionsIdleTimer !== undefined) window.clearTimeout(suggestionsIdleTimer);
    suggestionsIdleTimer = window.setTimeout(() => {
      suggestionsIdleTimer = undefined;
      void tryStartAutomaticSuggestions(schedule);
    }, delay);
  }

  function resumeScheduledAutomaticSuggestion(wakeKey: string): void {
    if (
      !wakeKey ||
      suggestionWakeQueued ||
      suggestionsIdleTimer !== undefined ||
      !completionController.scheduled ||
      completionLifecycle.phase !== 'ready'
    ) return;
    const schedule = completionController.scheduled;
    suggestionWakeQueued = true;
    queueMicrotask(() => {
      suggestionWakeQueued = false;
      if (
        completionController.scheduled === schedule &&
        suggestionsIdleTimer === undefined &&
        completionLifecycle.phase === 'ready'
      ) void tryStartAutomaticSuggestions(schedule);
    });
  }

  function rearmBoundedSuggestionSchedule(
    schedule: CompletionSchedule,
    delay: number
  ): boolean {
    if (schedule.kind === 'edit_pause') {
      completionController = setCompletionSchedule(completionController, schedule);
      queueScheduledSuggestionAttempt(schedule, delay);
      return true;
    }
    if (schedule.ticket.waitsRemaining <= 0) {
      completionController = setCompletionSchedule(completionController, null);
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
      !completionAutomationEnabled() ||
      project.project_id !== ticket.projectId ||
      project.session_id !== ticket.sessionId ||
      document.summary.document_id !== ticket.documentId ||
      document.summary.revision_id !== ticket.sourceRevisionId ||
      document.visible_blob_id !== ticket.visibleBlobId ||
      documentEpoch !== ticket.documentEpoch ||
      editVersion !== ticket.editVersion ||
      completionController.intentEpoch !== ticket.intentEpoch ||
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

  async function tryStartAutomaticSuggestions(schedule: CompletionSchedule): Promise<void> {
    const targetEditVersion = schedule.kind === 'edit_pause'
      ? schedule.editVersion
      : schedule.ticket.editVersion;
    if (
      terminalIsBusy() ||
      completionController.scheduled !== schedule ||
      targetEditVersion !== editVersion ||
      !completionAutomationEnabled() ||
      !project ||
      !document ||
      !completionGenerationIsArmed(
        completionController.generationIntent,
        completionContextKey,
        targetEditVersion
      )
    ) {
      completionController = setCompletionSchedule(completionController, null);
      return;
    }
    if (!canStartAutomaticSuggestions) {
      if (!retainsScheduledCompletion(completionLifecycle)) {
        completionController = setCompletionSchedule(completionController, null);
      }
      return;
    }
    if (schedule.kind === 'exhausted_retry') {
      const disposition = retryTicketDisposition(schedule.ticket);
      if (!disposition) {
        completionController = setCompletionSchedule(completionController, null);
        return;
      }
      if (disposition.kind === 'available' || disposition.kind === 'inactive') {
        completionController = setCompletionSchedule(completionController, null);
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
    const attempted = await startAutomaticWeave();
    if (completionController.scheduled !== schedule) return;
    if (attempted || !retainsScheduledCompletion(completionLifecycle)) {
      completionController = setCompletionSchedule(completionController, null);
    } else {
      // A preflight race (for example, an editor projection completing while
      // it is flushed) must not erase the only request. Recheck once the
      // resulting reactive state has settled.
      queueScheduledSuggestionAttempt(schedule, 100);
    }
  }

  function scheduleDraftJournal(delay = draftIntervalMs): void {
    if (cabalEditor) return;
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
    if (cabalEditor) return cabalEditor.flush();
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
    let retryAfterContention = false;
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
        const failure = normalizeFailure(error);
        if (failureIsDefiniteContention(failure)) {
          if (uncertainDraft === captured) uncertainDraft = null;
          saveState = 'dirty';
          saveMessage = 'Autosave pending';
          retryAfterContention = true;
          return false;
        }
        recordFailure(failure);
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
        scheduleDraftJournal(retryAfterContention ? 200 : undefined);
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
    if (cabalEditor) { await cabalEditor.flush(); return; }
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
      const failure = normalizeFailure(error);
      if (failureIsDefiniteContention(failure)) {
        if (uncertainSave?.commandId === captured.commandId) uncertainSave = null;
        saveState = 'dirty';
        saveMessage = 'Autosave pending';
        saveQueued = true;
        return;
      }
      recordFailure(failure);
      if (
        failure.code === 'external_file_change' ||
        failure.code === 'source_blob_conflict' ||
        failure.code === 'visible_file_conflict'
      ) {
        uncertainSave = null;
        try {
          const preview = await requestReconciliationPreview({
            document_id: captured.documentId,
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
    if (cabalSnapshotPending) { try { await cabalSnapshotPending; } catch { /* The refresh reports its own error. */ } }
    if (cabalEditor) { if (!flushEditors()) return false; return cabalEditor.flush(); }
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
    codec: SourceEditorCodec | null
  ): number | null {
    if (
      currentMode !== 'source' ||
      !editorAvailable ||
      selectionStart !== selectionEnd ||
      !isExtendedGraphemeBoundary(displayText, selectionStart) ||
      !currentDocument
    ) return null;
    try {
      return sourceCaretByte(displayText, selectionStart, manuscriptText, codec);
    } catch {
      return null;
    }
  }

  function rejectVisualGhostPresentation(
    candidateId: string,
    presentationKey: string,
    surfaceKey: string,
    anchorByteOffset: number
  ): void {
    completionController = rejectVisualPresentation(completionController, {
      mode,
      eligible: ghostSuggestion,
      candidateId,
      presentationKey,
      surfaceKey,
      currentSurfaceKey: visualGhostSurfaceKey,
      anchorByte: anchorByteOffset
    });
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

  function reconcileVisibleCompletionController(
    contextKey: string,
    family: readonly InlineGhostSuggestion[]
  ): void {
    const reconciled = reconcileCompletionController(
      completionController,
      contextKey,
      family
    );
    if (reconciled !== completionController) completionController = reconciled;
  }

  function refreshVisibleCompletionCandidate(
    expected: CompletionSession,
    runId: string,
    text: string,
    presentationKey: string
  ): void {
    const refreshed = refreshCompletionCandidate(
      completionController,
      expected,
      runId,
      text,
      presentationKey
    );
    if (refreshed !== completionController) completionController = refreshed;
  }

  function applyCompletionEffects(effects: readonly CompletionControllerEffect[]): void {
    for (const effect of effects) {
      switch (effect.kind) {
        case 'announce':
          announce(effect.message);
          break;
        case 'cancel_active_branches':
          if (activeBranchCount > 0) void cancelActiveBranches();
          break;
        case 'schedule_generation':
          scheduleAutomaticSuggestions(
            effect.editVersion,
            effect.delayMs,
            effect.trigger
          );
          break;
      }
    }
  }

  function finishCompletionIfExhausted(key: string): void {
    const exhausted = completionExhausted(
      completionController,
      key,
      editVersion,
      suggestionsIdleDelayMs
    );
    completionController = exhausted.state;
    applyCompletionEffects(exhausted.effects);
  }

  function completionActivityExists(): boolean {
    return controllerHasCompletionActivity(completionController, {
      weaveStarting,
      activeBranchCount,
      selected: selectedInlineSuggestion
    });
  }

  function invalidateCompletionForCaretNavigation(): void {
    const active = completionActivityExists();
    const invalidated = invalidateControllerNavigation(
      completionController,
      mode,
      active,
      activeBranchCount
    );
    completionController = invalidated.state;
    if (!active) return;
    promotionArmedCandidateId = null;
    clearSuggestionTimerHandle();
    applyCompletionEffects(invalidated.effects);
  }

  function acceptActiveGhost(candidateId: string, presentationKey: string): boolean {
    const eligible = eligibleGhostForCurrentMode();
    const branch = branches.find((candidate) => candidate.run_id === eligible?.runId);
    if (
      !branch ||
      eligible?.candidateId !== candidateId ||
      eligible.presentationKey !== presentationKey ||
      eligible.insertsOnAccept ||
      !canPromoteBranch(branch)
    ) return false;
    // LoomEditor and SourceEditor invoke this callback only after synchronously
    // proving the exact rendered key, surface, caret, and viewport witness.
    // Rechecking an asynchronously reported duplicate here can race that
    // stronger witness and turn Tab into focus traversal.
    void acceptInlineSuggestion(branch);
    return true;
  }

  function authorizeGhostInsertion(
    candidateId: string,
    presentationKey: string,
    text: string,
    action: CompletionInsertionAction
  ): boolean {
    const authorization = authorizeCompletionInsertion(completionController, {
      contextKey: completionContextKey,
      family: activeSuggestionFamily,
      eligible: eligibleGhostForCurrentMode(),
      candidateId,
      presentationKey,
      text,
      action,
      manuscriptText: documentText,
      promotionReady: branchPromotionReady
    });
    completionController = authorization.state;
    return authorization.authorized;
  }

  function authorizeGhostUnconsume(
    candidateId: string,
    presentationKey: string,
    text: string
  ): boolean {
    const authorization = authorizeCompletionUnconsume(completionController, {
      eligible: eligibleGhostForCurrentMode(),
      candidateId,
      presentationKey,
      text,
      manuscriptText: documentText
    });
    completionController = authorization.state;
    return authorization.authorized;
  }

  function cycleActiveSuggestion(offset: number): void {
    const cycled = cycleCompletion(
      completionController,
      activeSuggestionFamily,
      offset
    );
    if (cycled.state === completionController) return;
    completionController = cycled.state;
    promotionArmedCandidateId = null;
    applyCompletionEffects(cycled.effects);
  }

  async function setShuttleEnabled(enabled: boolean): Promise<void> {
    if (
      !applicationAllowsModelPreparation(applicationClosePhase) ||
      !project ||
      suggestionsChanging
    ) return;
    if (enabled && !buildModelPolicy) {
      announce('Shuttle remains off because this build could not verify its local writer policy');
      return;
    }
    const boundProject = project;
    const previousEnabled = shuttleEnabled;
    const engineBecameEnabled = completionEngineBecameEnabled(
      { autocomplete: suggestionsEnabled, shuttle: previousEnabled },
      { autocomplete: suggestionsEnabled, shuttle: enabled }
    );
    const engineBecameDisabled = completionEngineBecameDisabled(
      { autocomplete: suggestionsEnabled, shuttle: previousEnabled },
      { autocomplete: suggestionsEnabled, shuttle: enabled }
    );
    suggestionsChanging = true;
    try {
      await setSuggestionsPolicy(
        boundProject.project_id,
        boundProject.session_id,
        completionAutomationEnabled(suggestionsEnabled, enabled)
      );
      if (
        !applicationAllowsModelPreparation(applicationClosePhase) ||
        project?.project_id !== boundProject.project_id ||
        project.session_id !== boundProject.session_id
      ) return;
      shuttleEnabled = enabled;
      if (engineBecameDisabled) clearCompletionSession();
      if (!enabled) {
        if (shuttleTimer !== undefined) window.clearTimeout(shuttleTimer);
        shuttleTimer = undefined;
        shuttleTimerKey = '';
      }
      const automationEnabled = completionAutomationEnabled();
      if (!automationEnabled) {
        clearPreferredWriterRequest();
        cancelSuggestionTimer();
        scheduleActiveBranchPoll();
      }
      let writerReady = Boolean(currentModel);
      if (automationEnabled && !writerReady) {
        const captured: WorkspaceRestoreCapture = {
          restoreSerial: workspaceRestoreSerial,
          projectId: boundProject.project_id,
          sessionId: boundProject.session_id
        };
        await tick();
        requestPreferredWriterEnsure(captured);
        writerReady = Boolean(currentModel);
      }
      if (engineBecameEnabled && writerReady && document) {
        scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'explicit_enable');
      }
      announce(enabled
        ? writerReady
          ? 'Shuttle on. One suggestion word advances after four idle seconds; Escape stops.'
          : 'Shuttle on. Loom is preparing a local writer.'
        : 'Shuttle off');
    } catch (error) {
      shuttleEnabled = previousEnabled;
      recordFailure(error);
      announce('Shuttle could not change because the project gate did not respond');
    } finally {
      suggestionsChanging = false;
    }
  }

  async function toggleShuttleFromTitlebar(): Promise<void> {
    await setShuttleEnabled(!shuttleEnabled);
    await tick();
    if (mode === 'source') sourceEditor?.focusCurrentSelection();
    else visualEditor?.focusCurrentSelection();
  }

  function syncShuttleTimer(key: string): void {
    if (key === shuttleTimerKey) return;
    if (shuttleTimer !== undefined) window.clearTimeout(shuttleTimer);
    shuttleTimer = undefined;
    shuttleTimerKey = key;
    if (!key) return;
    shuttleTimer = window.setTimeout(() => {
      shuttleTimer = undefined;
      if (shuttleTimerKey !== key || !windowFocused || !shuttleEnabled || terminalIsBusy()) return;
      const accepted = mode === 'visual'
        ? visualEditor?.acceptGhostWord(false) ?? false
        : sourceEditor?.acceptGhostWord(false) ?? false;
      if (!accepted) {
        shuttleTimerKey = '';
        syncShuttleTimer(key);
      }
    }, 4_000);
  }

  function dismissActiveGhost(candidateId: string, presentationKey: string): void {
    const eligible = eligibleGhostForCurrentMode();
    if (
      eligible?.candidateId !== candidateId ||
      eligible.presentationKey !== presentationKey
    ) return;
    if (shuttleEnabled) {
      void setShuttleEnabled(false);
      return;
    }
    const dismissed = dismissCompletion(
      completionController,
      completionContextKey,
      eligible,
      candidateId,
      presentationKey
    );
    completionController = dismissed.state;
    if (dismissed.authorized) announce('Suggestion dismissed');
  }

  function terminalIsBusy(): boolean {
    return terminalDispatching || terminalPendingRequest !== null || terminalRuns.some((run) => run.status === 'running') || Object.values(paneBusy).some(Boolean);
  }

  function terminalScopeIsCurrent(projectId: string, sessionId: string): boolean {
    return componentMounted && project?.project_id === projectId && project.session_id === sessionId;
  }

  async function refreshTerminalRuns(): Promise<void> {
    if (!project || !desktop || applicationClosePhase !== 'running') return;
    const { project_id: projectId, session_id: sessionId } = project;
    const refreshSerial = ++terminalRefreshSerial;
    try {
      const runs = await listTerminalRuns(projectId, sessionId);
      if (!terminalScopeIsCurrent(projectId, sessionId) || refreshSerial !== terminalRefreshSerial) return;
      const newlyRetained = runs.some((run) => run.output_document_id &&
        !terminalRuns.some((previous) => previous.run_id === run.run_id && previous.output_document_id));
      terminalRuns = runs;
      if (terminalPendingRequest && (!terminalDispatching || runs.some((run) => run.run_id === terminalPendingRequest?.commandId))) {
        terminalPendingRequest = null;
      }
      if (cancelledTerminalIds.size) {
        for (const run of runs.filter((candidate) => candidate.status === 'running' && cancelledTerminalIds.has(candidate.run_id))) {
          await cancelTerminalRun(projectId, sessionId, run.run_id);
        }
      }
      if (newlyRetained) scheduleProjectFilesystemRefresh(0);
    } catch (error) {
      if (terminalScopeIsCurrent(projectId, sessionId)) terminalError = normalizeFailure(error).message;
    }
    if (terminalScopeIsCurrent(projectId, sessionId) && terminalRuns.some((run) => run.status === 'running')) {
      if (terminalPollTimer !== undefined) window.clearTimeout(terminalPollTimer);
      terminalPollTimer = window.setTimeout(() => {
        terminalPollTimer = undefined;
        void refreshTerminalRuns();
      }, 800);
    }
  }

  function closeTerminal(): void {
    terminalOpen = false;
    if (mode === 'source') sourceEditor?.focusCurrentSelection();
    else visualEditor?.focusCurrentSelection();
  }

  function captureTerminalRange(): TerminalSourceRange {
    if (!document) throw new Error('Open a document first.');
    if (mode === 'visual') {
      const range = visualEditor?.captureTerminalSourceRange();
      if (!range) throw new Error('This selection needs the Markdown editor before it can run.');
      return range;
    }
    const anchor = sourceEditor?.captureTextInsertionAnchor();
    if (!anchor) throw new Error('Finish editing the selection before running.');
    const byteAt = (offset: number): number => sourceCaretByte(anchor.value, offset, documentText, sourceCodec);
    return { start: byteAt(anchor.start), end: byteAt(anchor.end) };
  }

  async function runRetainedOutput(expression: string): Promise<void> {
    if (applicationClosePhase !== 'running' || editorNavigationLocked) return;
    const command = workspaceCommand(expression);
    if (command?.kind === 'signal') { await toggleSignal(); terminalEntry = ''; return; }
    if (command?.kind === 'join') { terminalEntry = ''; await doOpenProject(() => joinCabal(command.invitation)); return; }
    if (command?.kind === 'cabal') { cabalOpen = !cabalOpen; signalOpen = false; terminalEntry = ''; return; }
    if (!project || !document || terminalIsBusy() || editorReadonly || compositionActive || applicationClosePhase !== 'running') return;
    terminalOpen = true;
    terminalError = '';
    if (!expression && !currentModel?.completion) { terminalError = 'Load a local completion model to try this.'; return; }
    if (!flushEditors()) return;
    const scope = { projectId: project.project_id, sessionId: project.session_id };
    const documentId = document.summary.document_id;
    const text = documentText;
    const epoch = documentEpoch;
    terminalDispatching = true;
    terminalCancelRequested = false;
    cancelSuggestionTimer();
    try {
      const range = captureTerminalRange();
      await cancelActiveBranches();
      if (!(await flushCurrentDocument())) throw new Error('Finish saving this document before running.');
      if (!terminalScopeIsCurrent(scope.projectId, scope.sessionId) ||
        document?.summary.document_id !== documentId || documentEpoch !== epoch || documentText !== text || terminalCancelRequested) return;
      const sourceRevisionId = document.summary.revision_id;
      if (!sourceRevisionId) throw new Error('The document does not have a saved source revision.');
      const request: TerminalRunRequest = {
        ...scope, commandId: newUlid(), documentId, sourceRevisionId,
        expectedVisibleBlobId: document.visible_blob_id,
        sourceStartByte: range.start, sourceEndByte: range.end, expression, presentation: { pane_id: 'terminal', input: expression || new TextDecoder().decode(new TextEncoder().encode(documentText).slice(range.start, range.end)) }
      };
      terminalPendingRequest = request;
      const run = await runTerminal(request);
      if (!terminalScopeIsCurrent(scope.projectId, scope.sessionId)) return;
      if (run.run_id !== request.commandId) throw new Error('The result belongs to a different run.');
      terminalPendingRequest = null;
      if (terminalEntry === expression) terminalEntry = '';
      terminalRefreshSerial += 1;
      terminalRuns = [run, ...terminalRuns.filter((previous) => previous.run_id !== run.run_id)];
      if (run.output_document_id) scheduleProjectFilesystemRefresh(0);
      void refreshTerminalRuns();
    } catch (error) {
      if (terminalScopeIsCurrent(scope.projectId, scope.sessionId)) {
        terminalError = normalizeFailure(error).message;
        terminalDispatching = false;
        // A lost response never authorizes a second generation. Read retained
        // native receipts before the author decides whether to try again.
        await refreshTerminalRuns();
      }
    } finally {
      if (terminalScopeIsCurrent(scope.projectId, scope.sessionId)) terminalDispatching = false;
    }
  }

  async function stopTerminalRun(): Promise<void> {
    if (!project) return;
    terminalCancelRequested = true;
    const { project_id: projectId, session_id: sessionId } = project;
    try {
      const runIds = new Set(terminalRuns.filter((run) => run.status === 'running').map((run) => run.run_id));
      if (terminalPendingRequest) runIds.add(terminalPendingRequest.commandId);
      cancelledTerminalIds = new Set([...cancelledTerminalIds, ...runIds]);
      for (const runId of runIds) await cancelTerminalRun(projectId, sessionId, runId);
      if (terminalScopeIsCurrent(projectId, sessionId)) await refreshTerminalRuns();
    } catch (error) {
      if (terminalScopeIsCurrent(projectId, sessionId)) terminalError = normalizeFailure(error).message;
    }
  }

  async function openTerminalOutput(run: TerminalRun): Promise<void> {
    if (!project || !run.output_document_id) return;
    const { project_id: projectId, session_id: sessionId } = project;
    try {
      const refreshed = await currentProjectSession();
      if (!terminalScopeIsCurrent(projectId, sessionId)) return;
      const output = refreshed.documents.find((candidate) => candidate.document_id === run.output_document_id);
      if (!output) throw new Error('This retained output is no longer in the folder.');
      project = refreshed;
      await selectDocument(output, true);
    } catch (error) {
      if (terminalScopeIsCurrent(projectId, sessionId)) terminalError = normalizeFailure(error).message;
    }
  }

  function handleGlobalKeydownCapture(event: KeyboardEvent): void {
    if (!shouldCaptureFormatMenuEscape(event, {
      formatMenuOpen: formatMenu?.isOpen() ?? false,
      compositionActive,
      documentRenameOwnsEscape: renameDocumentEditorLocked || renamingDocumentId !== null,
      documentMenuOwnsEscape: documentContextTarget !== null,
      modelManagerOwnsEscape: modelManagerOpen
    })) return;
    event.preventDefault();
    event.stopPropagation();
    closeFormatMenu();
  }

  function handleGlobalKeydown(event: KeyboardEvent): void {
    if (event.defaultPrevented) return;
    if (event.key === 'Escape' && spokenAudio) { event.preventDefault(); stopReadAloud(); return; }
    if (event.key === 'Escape' && documentContextTarget) {
      event.preventDefault();
      closeDocumentContextMenu();
      return;
    }
    if (event.key === 'Escape' && modelManagerOpen) {
      event.preventDefault();
      closeModelManager();
      return;
    }
    if (event.key === 'Escape' && formatMenu?.isOpen()) {
      event.preventDefault();
      closeFormatMenu();
      return;
    }
    if (event.key === 'Escape' && coWriterOpen) {
      event.preventDefault();
      coWriterOpen = false;
      coWriterTrigger?.focus();
      return;
    }
    if (event.key === 'Escape' && speechInputActive()) {
      event.preventDefault();
      void cancelActiveSpeech();
      return;
    }
    if (event.key === 'Escape' && contextPaneOpen) {
      event.preventDefault();
      closeContextPane();
      return;
    }
    if (event.key === 'Escape' && shuttleEnabled) {
      event.preventDefault();
      void setShuttleEnabled(false);
      return;
    }
    const modifier = event.metaKey || event.ctrlKey;
    if (modifier && event.shiftKey && !event.altKey && !event.isComposing) {
      const key = event.key.toLowerCase();
      if (key === 'y') { event.preventDefault(); void toggleSignal(); return; }
      if (key === 'p') { event.preventDefault(); openModelManager(window.document.activeElement as HTMLElement); return; }
      if (key === 'u') { event.preventDefault(); void readAloud(); return; }
      if (event.code === 'Comma') { event.preventDefault(); void refreshWorkspaceTemplate(true); return; }
      if (key === 'l') { event.preventDefault(); toggleAppearance(); return; }
      if (key === 'm' && document) { event.preventDefault(); void setMode(mode === 'visual' ? 'source' : 'visual'); return; }
      if (key === 'f' && formatMenu) { event.preventDefault(); formatMenu.toggleOpen(); return; }
      if (key === 'j') { event.preventDefault(); void toggleShuttleFromTitlebar(); return; }
    }
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
      coWriterOpen &&
      event.target instanceof Node &&
      !coWriterPopover?.contains(event.target) &&
      !coWriterTrigger?.contains(event.target)
    ) coWriterOpen = false;
    if (
      documentContextTarget &&
      event.target instanceof Node &&
      !documentContextMenu?.contains(event.target)
    ) closeDocumentContextMenu(false);
    if (
      formatMenu?.isOpen() &&
      event.target instanceof Node &&
      !formatMenu.contains(event.target)
    ) closeFormatMenu(false);
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
    return sourceCaretByte(sourceTextarea.value, sourceTextarea.selectionStart, documentText, sourceCodec);
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
      started.branches.length !== 4 ||
      new Set(started.branches.map((branch) => branch.run_id)).size !== 4 ||
      started.branches.some((branch) => branch.weave_command_id !== started.command_id)
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
    authoritativeCompletionFamilyId = started.command_id;
    branches = [
      ...started.branches,
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
      !terminalIsBusy() &&
      project?.project_id === captured.projectId &&
      project.session_id === captured.sessionId &&
      document?.summary.document_id === captured.documentId &&
      document.summary.kind === captured.documentKind &&
      document.summary.revision_id === captured.sourceRevisionId &&
      document.visible_blob_id === captured.visibleBlobId &&
      documentEpoch === captured.epoch &&
      contextEpoch === captured.contextEpoch &&
      editVersion === captured.editVersion &&
      completionController.intentEpoch === captured.intentEpoch &&
      currentModel?.model_id === captured.modelId &&
      completionAutomationEnabled()
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

  function scheduleWeaveStatusPoll(captured: WeaveCapture, attempt = 0): void {
    if (attempt >= 240 || !weaveCaptureStillCurrent(captured)) return;
    const delay = Math.min(250 + attempt * 50, 1_000);
    const timer = window.setTimeout(async () => {
      weaveStatusPollTimers.delete(timer);
      if (!weaveCaptureStillCurrent(captured)) return;
      try {
        const status = await getWeaveStatus(
          captured.projectId,
          captured.sessionId,
          captured.commandId
        );
        if (!status) {
          scheduleWeaveStatusPoll(captured, attempt + 1);
          return;
        }
        if (!weaveCaptureStillCurrent(captured) || !installWeaveSnapshot(status, captured)) {
          return;
        }
        if (status.branches.some(isBranchActive)) {
          scheduleWeaveStatusPoll(captured, attempt + 1);
        }
      } catch {
        if (weaveCaptureStillCurrent(captured)) {
          scheduleWeaveStatusPoll(captured, attempt + 1);
        }
      }
    }, delay);
    weaveStatusPollTimers.add(timer);
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

  async function startAutomaticWeave(): Promise<boolean> {
    if (terminalIsBusy() || weaveStarting || !project || !document || !currentModel) return false;
    const startingEditVersion = editVersion;
    if (compositionActive || !flushEditors()) return false;
    if (editVersion !== startingEditVersion) {
      scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'retry');
      return false;
    }
    if (!canStartAutomaticSuggestions || uncertainWeave) return false;

    let cursorByte: number;
    try {
      cursorByte = captureWeaveCursorByte();
    } catch (error) {
      recordFailure(error);
      return false;
    }
    if (cursorByte === 0) return false;
    const sourceRevisionId = document.summary.revision_id;
    if (!sourceRevisionId) return false;
    const captured: WeaveCapture = {
      contextEpoch,
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
      intentEpoch: completionController.intentEpoch,
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
        if (started.branches.some(isBranchActive)) scheduleWeaveStatusPoll(captured);
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
        const failure = normalizeFailure(error);
        if (captureIsCurrent && failure.code === 'automatic_generation_throttled') {
          uncertainWeave = null;
          scheduleAutomaticSuggestions(editVersion, 5_000, 'retry');
          return true;
        }
        if (captureIsCurrent && failureIsDefiniteContention(failure)) {
          uncertainWeave = null;
          scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'retry');
          return true;
        }
        if (captureIsCurrent) {
          recordFailure(failure);
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
    return true;
  }

  function isBranchActive(branch: BranchCard): boolean {
    return branch.status === 'queued' || branch.status === 'generating';
  }

  async function cancelBranch(branch: BranchCard, contentionAttempt = 0): Promise<void> {
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
        const failure = normalizeFailure(error);
        if (failureIsDefiniteContention(failure)) {
          if (contentionAttempt < 4) {
            const delay = 40 * (contentionAttempt + 1);
            window.setTimeout(() => {
              if (
                project?.project_id !== captured.projectId ||
                project.session_id !== captured.sessionId
              ) return;
              const current = branches.find((candidate) => candidate.run_id === captured.runId);
              if (current && isBranchActive(current)) void cancelBranch(current, contentionAttempt + 1);
            }, delay);
          }
        } else if (cancellationFailureNeedsUserAttention(failure)) {
          recordFailure(failure);
          announce('Cancellation was not confirmed; stored strand state will be checked');
        }
      }
    } finally {
      cancellingRunIds = cancellingRunIds.filter((runId) => runId !== captured.runId);
      scheduleBranchRefresh();
    }
  }

  async function cancelActiveBranches(): Promise<void> {
    const active = branches.filter(isBranchActive);
    if (active.length === 0) return;
    for (const branch of active) await cancelBranch(branch);
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
      promotionArmedCandidateId = null;
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
            promotionArmedCandidateId = null;
            restoreWritingFocus = true;
            announce('Suggestion accepted');
            break;
          case 'source_changed':
            promotionArmedCandidateId = null;
            announce('The manuscript changed independently; Loom reopened it without attributing the change to this strand');
            break;
          case 'reconciliation':
            promotionArmedCandidateId = null;
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
      target.revision_id,
      target.active_blob_id
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
    setSourceDocument(opened.text);
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
    mode = opened.summary.kind === 'prose' && canUseVisualMarkdown(opened.text, false)
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
    setSourceDocument(recovered.text);
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
        setSourceDocument(activeText);
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
      sourceCodec = null;
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
    if (speechInputActive()) {
      announce('Finish or cancel dictation before changing its insertion surface');
      return;
    }
    if (compositionActive) {
      announce('Finish composing text before changing editor modes');
      return;
    }
    if (next === mode) return;
    if (!flushEditors()) return;
    if (next === 'visual' && document?.summary.kind !== 'prose') {
      announce('Visual editing is available for prose manuscripts');
      return;
    }
    if (next === 'visual' && !canUseVisualMarkdown(documentText, false)) {
      recordLocalFailure(
        'visual_markdown_not_exact',
        'This Markdown uses syntax the visual editor cannot preserve exactly yet. The Markdown editor remains available without changing your text.'
      );
      announce('Visual editor unavailable for this Markdown; your source text is unchanged');
      return;
    }
    if (next === 'visual' && contextPaneOpen && !canUseVisualMarkdown(contextText, false)) {
      recordLocalFailure(
        'visual_context_markdown_not_exact',
        'This context uses Markdown the visual editor cannot preserve exactly. Its source remains unchanged.'
      );
      announce('Visual editor unavailable for this context; its Markdown is unchanged');
      return;
    }
    invalidateCompletionForCaretNavigation();
    if (next === 'source' && document) setSourceDocument(documentText);
    if (document?.summary.kind === 'prose' && canUseVisualMarkdown(documentText, mode === 'visual')) {
      preferredProseMode = next;
    }
    mode = next;
    await tick();
    if (contextPaneOpen) focusContextEditorAtEnd();
    else focusCurrentWritingSurfaceAtEnd();
    if (completionAutomationEnabled() && currentModel && document) {
      scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'document_open');
    }
    announce(`${next} editor mode`);
  }

  function announce(message: string): void {
    liveRegion = '';
    window.setTimeout(() => (liveRegion = message), 0);
  }

  async function closeProject(): Promise<ProjectCloseOutcome> {
    if (closeInFlight) return closeInFlight;
    clearDocumentContextLongPress();
    closeDocumentContextMenu(false);
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
    const restoreAutomation = agency?.suggestionsEnabled ?? completionAutomationEnabled();
    const restoreSuggestions = pendingCloseInlineSuggestionsEnabled ?? suggestionsEnabled;
    const restoreShuttle = pendingCloseShuttleEnabled ?? shuttleEnabled;
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

    if (sameSession) {
      suggestionsEnabled = restoreSuggestions;
      shuttleEnabled = restoreShuttle;
    }
    pendingCloseCommandId = null;
    pendingCloseMayHaveCommitted = false;
    pendingCloseAgency = null;
    pendingCloseInlineSuggestionsEnabled = null;
    pendingCloseShuttleEnabled = null;
    transition = 'idle';
    scheduleActiveBranchPoll();
    if (
      sameSession &&
      restoreAutomation &&
      applicationAllowsModelPreparation(applicationClosePhase)
    ) {
      requestPreferredWriterForCurrentWorkspace();
      if (currentModel && document) {
        scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'document_open');
      }
    }
    return { status: 'resume' };
  }

  async function performCloseProject(): Promise<ProjectCloseOutcome> {
    try { await signalDraftEditor.flush(); }
    catch (failure) { recordFailure(failure); signalOpen = true; return { status: 'resume' }; }
    if (!project) return { status: 'closed' };
    if (speechStarting) { announce('Finishing the microphone operation before closing'); return { status: 'resume' }; }
    if (speechRecording) {
      await finishSpeechRecording();
      if (speechRecording) return { status: 'resume' };
    }
    stopReadAloud();
    const retryingPreparedClose = transition === 'closing' && pendingCloseCommandId !== null;
    if (
      missingDocumentRecoveryRequiresCopy(
        missingDocumentRecovery && {
          documentId: missingDocumentRecovery.documentId,
          journalDurable: missingDocumentRecovery.journalDurable,
          copied: missingDocumentCopyState === 'copied'
        }
      ) &&
      !retryingPreparedClose
    ) {
      announce('Copy the preserved missing-document text before closing the project');
      return { status: 'resume' };
    }
    if (missingDocumentCapturePending && !retryingPreparedClose) {
      announce('Wait for Loom to preserve the newly missing manuscript before closing the project');
      scheduleProjectFilesystemRefresh(0);
      return { status: 'resume' };
    }
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
    const requestBoundClose = () => closeProjectSession(
      closing.project_id,
      closing.session_id,
      closeCommandId
    );
    const validateBoundCloseReceipt = (receipt: ProjectCloseReceipt) => {
      if (
        receipt.command_id !== closeCommandId ||
        receipt.project_id !== closing.project_id ||
        receipt.session_id !== closing.session_id
      ) {
        throw new Error('The desktop returned a close receipt for a different project session.');
      }
    };

    if (pendingCloseMayHaveCommitted) {
      let receipt: ProjectCloseReceipt;
      try {
        receipt = await requestBoundClose();
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
      validateBoundCloseReceipt(receipt);
    } else {
      // Stop new automatic admission before native close drains any reserved
      // startup already in flight. Keep the persisted preference unchanged so
      // a later reopen can restore the author's choice deliberately.
      pendingCloseAgency ??= captureProjectCloseAgency(completionAutomationEnabled());
      pendingCloseInlineSuggestionsEnabled ??= suggestionsEnabled;
      pendingCloseShuttleEnabled ??= shuttleEnabled;
      suggestionsEnabled = false;
      shuttleEnabled = false;
      cancelSuggestionTimer();
      const outcome = await drainGenerationsAndClose({
        disableAutomation: () => setSuggestionsPolicy(
          closing.project_id,
          closing.session_id,
          false
        ),
        cancelKnownBranches: cancelActiveBranches,
        closeProject: requestBoundClose,
        validateCloseResult: validateBoundCloseReceipt,
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
    promotionArmedCandidateId = null;
    project = null;
    document = null;
    documentText = '';
    sourceDisplayText = '';
    sourceCodec = null;
    editVersion = 0;
    savedVersion = 0;
    branches = [];
    promotionArmedCandidateId = null;
    uncertainPromotion = null;
    resetLiveGenerationView();
    saveState = 'clean';
    saveMessage = 'No project open';
    cancelDocumentRename(false);
    closeDocumentContextMenu(false);
    outlineOpen = false;
    suggestionsEnabled = false;
    shuttleEnabled = false;
    clearPreferredWriterRequest();
    cancelSuggestionTimer();
    completionController = resetCompletionDiscovery(completionController);
    pendingCloseCommandId = null;
    pendingCloseMayHaveCommitted = false;
    pendingCloseAgency = null;
    pendingCloseInlineSuggestionsEnabled = null;
    pendingCloseShuttleEnabled = null;
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

  async function refreshWorkspaceTemplate(enable = false): Promise<void> {
    if (!project || compositionActive || !flushEditors()) return;
    if (document?.summary.relative_path === '.loom.md' && editVersion !== savedVersion) return;
    const scope = { projectId: project.project_id, sessionId: project.session_id };
    const serial = ++templateSerial;
    try {
      const snapshot = await (enable ? enableWorkspaceTemplate : getWorkspaceTemplate)(scope.projectId, scope.sessionId);
      if (serial !== templateSerial || !terminalScopeIsCurrent(scope.projectId, scope.sessionId)) return;
      if (!flushEditors()) { deferredTemplateSession = scope.sessionId; deferredWorkspaceTemplate = snapshot; return; }
      workspaceTemplate = snapshot;
      workspaceTemplateScope = `${scope.projectId}/${scope.sessionId}`;
      void tick().then(requestPreferredWriterForCurrentWorkspace);
      const registered = project?.documents.find(item => item.document_id === snapshot.document_id);
      if (snapshot.document_id && registered?.revision_id !== snapshot.revision_id) {
        scheduleProjectFilesystemRefresh(0);
      }
      if (enable && snapshot.document_id) {
        await refreshProjectFilesystemState();
        await openPaneDocument(snapshot.document_id);
        outlineOpen = true;
      }
    } catch (error) {
      if (serial === templateSerial && terminalScopeIsCurrent(scope.projectId, scope.sessionId)) recordFailure(error);
    }
  }

  async function openPaneDocument(id: string): Promise<void> {
    const candidate = project?.documents.find(item => item.document_id === id);
    if (candidate) await selectDocument(candidate, true);
  }

  function applyDeferredTemplate(): void {
    if (project?.session_id !== deferredTemplateSession) { deferredWorkspaceTemplate = null; return; }
    if (!flushEditors()) return;
    workspaceTemplate = deferredWorkspaceTemplate;
    workspaceTemplateScope = `${project?.project_id}/${project?.session_id}`;
    deferredWorkspaceTemplate = null;
    void tick().then(requestPreferredWriterForCurrentWorkspace);
  }

  function togglePane(position: 'main' | 'right' | 'bottom'): void {
    if (!flushEditors() || busyPaneSlots.has(position)) return;
    const next = new Set(hiddenPaneSlots);
    if (next.has(position)) next.delete(position); else next.add(position);
    hiddenPaneSlots = next;
  }

  function selectPane(position: 'main' | 'right' | 'bottom', id: string): void {
    if (!flushEditors() || busyPaneSlots.has(position)) return;
    paneSelection = { ...paneSelection, [position]: id };
  }

  async function preparePaneRun(): Promise<OpenDocument | null> {
    if (!project || !document || editorReadonly || compositionActive || !flushEditors()) return null;
    const expected = { projectId: project.project_id, sessionId: project.session_id, documentId: document.summary.document_id, epoch: documentEpoch, text: documentText };
    const current = () => terminalScopeIsCurrent(expected.projectId, expected.sessionId) && document?.summary.document_id === expected.documentId && documentEpoch === expected.epoch && documentText === expected.text;
    cancelSuggestionTimer();
    await cancelActiveBranches();
    if (!current() || !await flushCurrentDocument() || !current()) return null;
    if (!currentModel && !await loadPreferredSuggestionModel(currentWorkspaceCapture() ?? undefined)) return null;
    if (!current() || !currentModel) return null;
    return document;
  }

  function currentCabalScope(): string {
    return project ? `${project.project_id}/${project.session_id}/${document?.summary.document_id ?? ''}` : '';
  }

  async function attachCabal(scope: string): Promise<void> {
    cabalRecovered = false;
    if (cabalTimer) clearTimeout(cabalTimer);
    cabalTimer = undefined;
    cabalEditor?.dispose(); cabalEditor = null; cabal = null;
    cabalAttaching = true;
    try {
      if (saveInFlight) await saveInFlight;
      if (draftInFlight) await draftInFlight;
      if (currentCabalScope() !== scope) return;
      if (editVersion !== savedVersion && !await flushCurrentDocument()) {
        cabalAttaching = false;
        return;
      }
      await refreshCabal(scope, true);
    } catch (failure) { if (currentCabalScope() === scope) recordFailure(failure); }
    finally {
      if (componentMounted && currentCabalScope() === scope && !cabalTimer && !cabalEditor) {
        cabalTimer = setTimeout(() => void attachCabal(scope), 1000);
      }
    }
  }

  async function refreshCabal(scope: string, attach = false): Promise<void> {
    if (!project || !componentMounted || currentCabalScope() !== scope) return;
    if (transition !== 'idle' || fileCommandInFlight || renameDocumentEditorLocked || deleteDocumentEditorLocked) {
      if (cabalTimer) clearTimeout(cabalTimer);
      cabalTimer = setTimeout(() => void refreshCabal(scope, attach), 500);
      return;
    }
    if (cabalRefreshing) {
      if (attach) cabalTimer = setTimeout(() => void refreshCabal(scope, true), 100);
      return;
    }
    cabalRefreshing = true;
    const projectId = project.project_id, sessionId = project.session_id;
    const controller = cabalEditor, version = controller?.version ?? 0;
    try {
      cabalSnapshotPending = cabalSnapshot(projectId, sessionId, document?.summary.document_id ?? null);
      const snapshot = await cabalSnapshotPending;
      if (!componentMounted || currentCabalScope() !== scope || !project) return;
      cabal = snapshot;
      if (!snapshot) { cabalAttaching = false; return; }
      const summaries = new Map(project.documents.map(item => [item.document_id, item]));
      for (const id of snapshot.deleted_document_ids) summaries.delete(id);
      for (const item of snapshot.documents) summaries.set(item.local.summary.document_id, item.local.summary);
      project = { ...project, documents: [...summaries.values()] };
      if (!document) { cabalAttaching = false; return; }
      const current = snapshot.documents.find(item => item.local.summary.document_id === document?.summary.document_id);
      if (!current) { cabalAttaching = snapshot.problems.some(item => item.document_id === document?.summary.document_id); return; }
      if (transition !== 'idle' || compositionActive || visualMutationPending || sourceDirty) return;
      if (attach && !cabalEditor) {
        cabalEditor = new CabalEditor(current, edit => editCabal(projectId, sessionId, edit),
          item => currentCabalScope() === scope && adoptCabalDocument(item),
          () => compositionActive || sourceDirty || visualMutationPending,
          failure => { if (currentCabalScope() === scope) { saveState = 'error'; saveMessage = 'Shared edit needs attention'; recordFailure(failure); } });
        if (saveTimer !== undefined) { clearTimeout(saveTimer); saveTimer = undefined; }
        if (draftTimer !== undefined) { clearTimeout(draftTimer); draftTimer = undefined; }
        adoptCabalDocument(current);
      } else if (controller && controller === cabalEditor) controller.receive(current, version);
      cabalAttaching = false;
    } catch (failure) {
      if (currentCabalScope() === scope) recordFailure(failure);
    } finally {
      cabalRefreshing = false;
      cabalSnapshotPending = null;
      if (componentMounted && currentCabalScope() === scope) {
        if (cabalTimer) clearTimeout(cabalTimer);
        cabalTimer = setTimeout(() => void refreshCabal(scope, attach && !cabalEditor), cabal ? 1000 : 5000);
      }
    }
  }

  function adoptCabalDocument(item: SharedDocument): boolean {
    if (!document || !project || item.local.summary.document_id !== document.summary.document_id || compositionActive || sourceDirty || visualMutationPending || transition !== 'idle') return false;
    cabalApplying = true;
    try {
      updateText(item.shared.text);
      document = item.local;
      for (const editor of Object.values(paneEditors)) editor?.applyRemoteValue(item.shared.text);
      if (mode === 'source') {
        const decoded = decodeSourceForEditor(item.shared.text);
        sourceCodec = decoded.codec; sourceDisplayText = decoded.display;
        sourceEditor?.applyRemoteValue(decoded.display);
      }
      project = { ...project, documents: project.documents.map(summary => summary.document_id === item.local.summary.document_id ? item.local.summary : summary) };
      savedVersion = editVersion; draftSavedEditVersion = editVersion;
      saveState = 'clean'; saveMessage = item.shared.deleted ? 'Removed from cabal · text retained' : cabal?.read_only ? 'Saved copy · no longer a member' : 'Saved · shared';
      return true;
    } finally { cabalApplying = false; }
  }

  async function inviteCabal(name: string): Promise<string> {
    const captured = project;
    if (!captured || compositionActive || !flushEditors() || !await flushCurrentDocument() || !terminalScopeIsCurrent(captured.project_id, captured.session_id)) {
      throw new Error('Finish saving this workspace before sharing it.');
    }
    const invitation = await shareCabal(captured.project_id, captured.session_id, captured.title, name);
    if (!terminalScopeIsCurrent(captured.project_id, captured.session_id)) throw new Error('The workspace changed while preparing its invitation.');
    cabalScope = '';
    return invitation;
  }

  async function removeCabalMember(member: string, rosterHash: string): Promise<void> {
    if (!project || !await flushCurrentDocument()) throw new Error('Finish saving before changing membership.');
    await revokeCabalMember(project.project_id, project.session_id, member, rosterHash);
    await refreshCabal(currentCabalScope());
  }

  async function recoverCabalCopies(): Promise<string[]> {
    if (!project) throw new Error('Open the cabal workspace first.');
    if (!flushEditors()) throw new Error('Finish composing before recovering this writing.');
    const scope = currentCabalScope(), controller = cabalEditor;
    if (controller) await controller.flush();
    if (!project || currentCabalScope() !== scope) throw new Error('The workspace changed.');
    const edit = controller?.recoveryEdit() ?? null;
    const { paths, draft_path } = await recoverCabalEdits(project.project_id, project.session_id, edit);
    if (!project || currentCabalScope() !== scope) return paths;
    if (edit && controller === cabalEditor && controller?.acknowledgeRecovery(edit)) {
      cabalRecovered = true;
      saveState = 'clean'; saveMessage = 'Private recovery copy saved';
    }
    await refreshProjectFilesystemState(); outlineOpen = true;
    if (edit && currentCabalScope() === scope) {
      const recovered = project?.documents.find(item => item.relative_path === draft_path);
      if (recovered) await selectDocument(recovered, true);
    }
    return paths;
  }

  async function inviteSignalConversation(conversation: SignalConversation): Promise<string> {
    const captured = project;
    if (!captured) throw new Error('Open a workspace before inviting someone.');
    const invitation = await inviteCabal('Loom');
    const workspace = await cabalWorkspace(captured.project_id, captured.session_id);
    if (!workspace || !terminalScopeIsCurrent(captured.project_id, captured.session_id)) throw new Error('The workspace changed.');
    await rememberSignalWorkspace(conversation.id, workspace);
    return `Make something with me in ${workspace.title}:\n${invitation}\n\nWorkspace: loom://workspace/${workspace.id}`;
  }

  async function openSignalWorkspace(conversation: SignalConversation, value: string, join = false): Promise<void> {
    if (!await doOpenProject(() => join ? joinCabal(value) : openCabal(value))) return;
    const captured = project;
    if (!captured) return;
    const workspace = await cabalWorkspace(captured.project_id, captured.session_id);
    if (workspace) await rememberSignalWorkspace(conversation.id, workspace);
  }

  async function toggleSignal(): Promise<void> {
    if (signalOpen) {
      try { await signalDraftEditor.flush(); }
      catch (failure) { recordFailure(failure); return; }
    }
    signalOpen = !signalOpen;
    if (signalOpen) cabalOpen = false;
  }

  async function draftSignalReply(prompt: string): Promise<string> {
    const captured = project;
    const current = await preparePaneRun();
    if (!current?.summary.revision_id || !captured || !terminalScopeIsCurrent(captured.project_id, captured.session_id)) throw new Error('Open a saved document before drafting.');
    const projectId = captured.project_id, sessionId = captured.session_id;
    const commandId = newUlid();
    const run = await runTerminal({ projectId, sessionId, commandId, documentId: current.summary.document_id,
      sourceRevisionId: current.summary.revision_id, expectedVisibleBlobId: current.visible_blob_id,
      sourceStartByte: 0, sourceEndByte: 0, expression: prompt, contextReferences: [], literalInput: true,
      turnBoundary: 'chat', presentation: { pane_id: 'signal', input: 'Draft a Signal reply' } });
    const loader = new RetainedOutputLoader();
    let result = run;
    const deadline = Date.now() + 180_000;
    while (result.status === 'running') {
      if (!signalOpen || !terminalScopeIsCurrent(projectId, sessionId) || Date.now() >= deadline) {
        await cancelTerminalRun(projectId, sessionId, commandId);
        throw new Error('Drafting stopped. Its retained result remains in Runs.');
      }
      await new Promise(resolve => setTimeout(resolve, 600));
      result = (await listTerminalRuns(projectId, sessionId)).find(item => item.run_id === commandId) ?? result;
    }
    if (result.status !== 'completed') throw new Error(result.error || 'The local draft did not complete.');
    const refreshed = await currentProjectSession();
    if (!terminalScopeIsCurrent(projectId, sessionId)) throw new Error('The workspace changed while drafting.');
    project = refreshed;
    return loader.read(projectId, sessionId, result, refreshed.documents);
  }

  function kindLabel(kind: DocumentKind): string {
    if (kind === 'verse') return 'Poem';
    if (kind === 'hybrid') return 'Hybrid';
    return 'Prose';
  }

</script>

<svelte:head>
  <title>{nativeWindowTitle}</title>
  <meta name="description" content="Loom — a local-first writing environment for prose and poetry" />
</svelte:head>

<div class="app-shell">
  {#if project}
    <div
      class="canvas-controls"
      class:native-fullscreen={nativeFullscreen}
      aria-label="Writing controls"
    >
      <div class="canvas-controls-left" data-no-window-drag>
        {#if project.documents.length > 0 || folderWarnings.length > 0}
          <button
            bind:this={outlineToggle}
            class="titlebar-button outline-toggle"
            type="button"
            aria-controls="project-outline"
            aria-expanded={outlineOpen}
            aria-label={`${outlineOpen ? 'Close' : 'Open'} manuscript outline${folderWarnings.length ? `, ${folderWarnings.length} files not opened` : ''}`}
            title={folderWarnings.length ? `${folderWarnings.length} files not opened — see folder details` : 'Manuscript outline'}
            on:click={() => void setOutlineOpen(!outlineOpen)}
          ><svg aria-hidden="true" viewBox="0 0 16 16"><rect x="2" y="2.5" width="12" height="11" rx="2"/><path d="M6 2.5v11"/></svg>
            {#if folderWarnings.length}<span class="folder-warning-dot" aria-hidden="true"></span>{/if}
          </button>
        {/if}
        {#if transition === 'idle'}
          <button
            class="titlebar-button new-document-button"
            type="button"
            aria-label="New document"
            title="New document (⌘N)"
            disabled={fileCommandInFlight || editorReadonly}
            on:click={() => void newDocument()}
          ><svg aria-hidden="true" viewBox="0 0 16 16"><path d="M8 3v10M3 8h10" /></svg></button>
        {/if}

      </div>
      <div
        class="titlebar-drag-surface"
        aria-hidden="true"
        on:mousedown={startTitlebarDrag}
      ><span class="titlebar-document-title">{nativeWindowTitle}</span></div>
      <div class="canvas-controls-right" data-no-window-drag>
        {#each paneSlots.filter(slot => slot.position !== 'main' && slot.selected) as slot (slot.position)}
          {@const title = slot.selected![1].title ?? slot.selected![0]}
          <button class="titlebar-button" class:active={!hiddenPaneSlots.has(slot.position)} type="button"
            aria-label={`${hiddenPaneSlots.has(slot.position) ? 'Show' : 'Hide'} ${title}`} aria-pressed={!hiddenPaneSlots.has(slot.position)} title={title}
            disabled={busyPaneSlots.has(slot.position)} on:click={() => togglePane(slot.position)}>
            <svg aria-hidden="true" viewBox="0 0 16 16"><rect x="2" y="2.5" width="12" height="11" rx="2"/>{#if slot.position === 'right'}<path d="M10 2.5v11"/>{:else}<path d="M2 10h12"/>{/if}</svg>
          </button>
        {/each}
        {#if document && mode === 'visual' && canUseVisual && (!contextPaneOpen || canUseVisualMarkdown(contextText, true))}
          <VisualFormatMenu
            bind:this={formatMenu}
            hiddenTrigger
            editor={contextPaneOpen ? contextVisualEditor : visualEditor}
            formatting={contextPaneOpen ? contextFormatting : visualFormatting}
            onCommandResult={(action, applied) => {
              announce(applied
                ? 'Formatting applied'
                : `${action.replaceAll('_', ' ')} could not be applied at this selection`);
            }}
          />
        {/if}
        {#if document}
          <button
            class:recording={Boolean(speechRecording) && !speechError}
            class:transcribing={Boolean(speechInput)}
            class:needs-attention={Boolean(speechError)}
            class="titlebar-button microphone-toggle"
            type="button"
            aria-label={speechRecording
              ? speechError ? 'Retry saving recording' : 'Stop recording'
              : speechInput
                ? 'Cancel speech recognition'
                : 'Record audio'}
            aria-describedby="speech-input-help"
            aria-pressed={Boolean(speechRecording)}
            title={speechError || (speechRecording
              ? 'Stop recording'
              : speechInput
                ? 'Recognizing locally — click to cancel'
                : 'Record audio for this document')}
            disabled={!desktop || !document || speechStarting || (editorReadonly && !speechInputActive())}
            on:click={toggleSpeechInput}
          ><svg aria-hidden="true" viewBox="0 0 18 18"><rect x="6.4" y="2.5" width="5.2" height="8.3" rx="2.6"/><path d="M4.4 8.8a4.6 4.6 0 0 0 9.2 0M9 13.4v2.1M6.8 15.5h4.4"/></svg></button>
          <span id="speech-input-help" class="sr-only">{speechError || (speechRecording ? 'Recording locally' : speechInput ? 'Recognizing speech locally' : 'Audio stays on this device and is attached to this document')}</span>
        {/if}
        <button
          class:active={suggestionsEnabled && Boolean(currentModel)}
          class:preparing={suggestionsEnabled && !currentModel && !quietModelLoadFailure && (modelLoading || preferredWriterEnsureInFlight !== null || preferredWriterPending !== null)}
          class:needs-attention={suggestionsEnabled && !currentModel && Boolean(quietModelLoadFailure)}
          class="titlebar-button suggestions-toggle"
          type="button"
          aria-label={suggestionsEnabled ? 'Turn autocomplete off' : 'Turn autocomplete on'}
          aria-pressed={suggestionsEnabled}
          title={suggestionsEnabled ? 'Ghost text on' : 'Ghost text off'}
          disabled={!project || suggestionsChanging}
          on:click={() => void toggleSuggestionsFromTitlebar()}
        >
          <svg aria-hidden="true" viewBox="0 0 18 18"><path d="M3.5 15V8a5.5 5.5 0 0 1 11 0v7l-2.75-2-2.75 2-2.75-2-2.75 2Z"/><path d="M7 7v1M11 7v1"/></svg>
        </button>

      </div>
    </div>
  {/if}

  {#if project}
    <div bind:clientWidth={workspaceWidth} bind:clientHeight={workspaceHeight} class:outline-open={outlineOpen} class="workspace-grid"
      style={`grid-template-columns:${outlineOpen ? Math.min(outlineWidth, outlineLimit) : 0}px minmax(0,1fr) ${rightPaneOpen ? Math.min(rightWidth, rightLimit) : 0}px; grid-template-rows:minmax(0,1fr) ${bottomPaneOpen ? Math.min(bottomHeight, workspaceHeight * 0.6) : 0}px;`}>
      <aside
        id="project-outline"
        bind:this={outlineElement}
        class:drop-active={workspaceDropActive}
        class:open={outlineOpen}
        class="outline-panel"
        aria-label="Documents"
      >
        {#if outlineOpen}<PaneDivider edge="right" label="Resize documents" size={Math.min(outlineWidth, outlineLimit)} min={150} max={outlineLimit} onResize={(size) => outlineWidth = size} />{/if}
        <label class="search-field">
          <span class="sr-only">Search folder</span>
          <span aria-hidden="true">⌕</span>
          <input bind:value={search} type="search" placeholder="Find in folder" />
        </label>
        <nav class="document-list" aria-label="Workspace folders">
          {#each visibleWorkspaceFolders as folder (folder.root)}
            {@const active = folder.root === project.root}
            <button class="folder-row workspace-root" class:active type="button" title={folder.root}
              aria-label={`${active && workspaceRootExpanded ? 'Collapse' : 'Open'} folder ${folder.title}`}
              aria-expanded={active && workspaceRootExpanded} disabled={fileCommandInFlight || opening}
              on:click={() => { if (active) workspaceRootExpanded = !workspaceRootExpanded; else void doOpenProject(folder.root); }}>
              <svg aria-hidden="true" viewBox="0 0 16 16"><path d="M2 4h4l1.5 1.5H14v7H2Z"/></svg><span>{folder.title}</span>
            </button>
            {#if active && workspaceRootExpanded}
            <div class="workspace-root-documents">
          {#each fileRows as row (row.path)}
            {#if row.folder}
              <button class="folder-row" type="button" style={`padding-left: ${8 + row.depth * 14}px`} aria-expanded={!collapsedFolders.has(row.path) || Boolean(search.trim())} on:click={() => { const next = new Set(collapsedFolders); if (next.has(row.path)) next.delete(row.path); else next.add(row.path); collapsedFolders = next; }}>
                <svg aria-hidden="true" viewBox="0 0 16 16"><path d="M2 4h4l1.5 1.5H14v7H2Z"/></svg><span>{row.title}</span>
              </button>
            {:else}
            {@const candidate = row.document}
            {#if renamingDocumentId === candidate.document_id}
              <div class:active={candidate.document_id === document?.summary.document_id} class="document-row editing">

                <span class="document-label">
                  <input
                    bind:this={renameDocumentInput}
                    bind:value={renameDocumentTitle}
                    type="text"
                    maxlength="256"
                    aria-label={`Rename ${candidate.title}`}
                    disabled={renameDocumentInFlight}
                    on:input={handleDocumentRenameInput}
                    on:compositionstart={handleDocumentRenameCompositionStart}
                    on:compositionend={handleDocumentRenameCompositionEnd}
                    on:keydown={handleDocumentRenameKeydown}
                    on:blur={handleDocumentRenameBlur}
                  />

                </span>
              </div>
            {:else}
            <div
              class="document-row-group"
              class:template-file={row.path.split('/').at(-1)?.startsWith('.')}
              style={`padding-left: ${row.depth * 14}px`}
              class:active={candidate.document_id === (reconciliation?.document_id ?? document?.summary.document_id)}
            >
              <button
                class="document-row"
                data-document-row={candidate.document_id}
                type="button"
                disabled={editorNavigationLocked}
                aria-haspopup="menu"
                aria-expanded={documentContextTarget?.documentId === candidate.document_id}
                on:click={(event) => handleDocumentRowClick(event, candidate)}
                on:contextmenu={(event) => handleDocumentContextPointer(event, candidate)}
                on:keydown={(event) => handleDocumentContextKey(event, candidate)}
                on:pointerdown={(event) => beginDocumentContextLongPress(event, candidate)}
                on:pointermove={updateDocumentContextLongPress}
                on:pointerup={finishDocumentContextLongPress}
                on:pointercancel={finishDocumentContextLongPress}
              >

                <span class="document-label">
                  <strong data-document-title>{candidate.title}</strong>

                </span>
              </button>
              <button
                class="document-row-actions"
                type="button"
                aria-label={`Actions for ${candidate.title}`}
                aria-haspopup="menu"
                aria-expanded={documentContextTarget?.documentId === candidate.document_id}
                title={`Actions for ${candidate.title}`}
                disabled={editorReadonly || fileCommandInFlight || documentContextActionInFlight}
                on:click={(event) => handleVisibleDocumentActions(event, candidate)}
              ><span aria-hidden="true">•••</span></button>
            </div>
            {/if}
            {/if}
          {:else}
            <p class="empty-copy">No notes.</p>
          {/each}
            </div>
            {/if}
          {/each}
        </nav>
        {#if folderWarnings.length > 0}
          <details class="folder-warnings">
            <summary>{folderWarnings.length} {folderWarnings.length === 1 ? 'file' : 'files'} not opened</summary>
            <ul>{#each folderWarnings as warning}<li>{warning}</li>{/each}</ul>
          </details>
        {/if}
        {#if documentContextTarget}
          <div
            bind:this={documentContextMenu}
            class="document-context-menu"
            role="menu"
            tabindex="-1"
            aria-label={`Actions for ${documentContextTarget.title}`}
            style={`left: ${documentContextPoint.x}px; top: ${documentContextPoint.y}px;`}
            on:keydown={handleDocumentContextMenuKeydown}
            on:contextmenu|preventDefault={() => {}}
          >
            <button
              type="button"
              role="menuitem"
              tabindex={documentContextFocusIndex === 0 ? 0 : -1}
              disabled={editorReadonly || fileCommandInFlight || documentContextActionInFlight}
              on:focus={() => documentContextFocusIndex = 0}
              on:click={() => void runDocumentContextAction('open')}
            >Open</button>
            <button
              type="button"
              role="menuitem"
              tabindex={documentContextFocusIndex === 1 ? 0 : -1}
              disabled={editorReadonly || fileCommandInFlight || documentContextActionInFlight}
              on:focus={() => documentContextFocusIndex = 1}
              on:click={() => void runDocumentContextAction('rename')}
            >Rename…</button>
            <button
              type="button"
              role="menuitem"
              tabindex={documentContextFocusIndex === 2 ? 0 : -1}
              disabled={fileCommandInFlight || documentContextActionInFlight}
              on:focus={() => documentContextFocusIndex = 2}
              on:click={() => void runDocumentContextAction('export_text')}
            >Export Text…</button>
            {#if documentContextRevealLabel}
              <button
                type="button"
                role="menuitem"
                tabindex={documentContextFocusIndex === 3 ? 0 : -1}
                disabled={fileCommandInFlight || documentContextActionInFlight}
                on:focus={() => documentContextFocusIndex = 3}
                on:click={() => void runDocumentContextAction('reveal')}
              >{documentContextRevealLabel}</button>
            {/if}
            <button
              class="document-delete-menu-item"
              type="button"
              role="menuitem"
              tabindex={documentContextFocusIndex === documentDeleteMenuIndex(Boolean(documentContextRevealLabel)) ? 0 : -1}
              disabled={editorReadonly || fileCommandInFlight || documentContextActionInFlight}
              on:focus={() => documentContextFocusIndex = documentDeleteMenuIndex(Boolean(documentContextRevealLabel))}
              on:click={() => void runDocumentContextAction('delete')}
            >Delete Manuscript…</button>
          </div>
        {/if}
      </aside>

      <main id="manuscript" class="manuscript-area" tabindex="-1" class:workspace-main-hidden={customMain}>
        {#if document && contextPaneOpen}
          <div
            bind:this={contextPaneElement}
            id="completion-context-pane"
            class:drop-active={contextDropActive}
            class="completion-context-pane"
            data-attachment-drop="context"
            on:focusin={(event) => rememberSpeechEditor(event, 'context')}
            aria-label="Completion context"
            role="region"
          >
            <div class="completion-context-heading">
              <strong>Context</strong>
              <div class="context-heading-actions">
                <button
                  class="attachment-remove context-close"
                  type="button"
                  aria-label="Close completion context"
                  on:click={closeContextPane}
                >×</button>
              </div>
            </div>
            <div class="context-composer">
              <div class="context-editor-surface" on:focusout={flushContextEditorProjection}>
                {#if mode === 'visual' && canUseVisualMarkdown(contextText, true)}
                  <LoomEditor
                    bind:this={contextVisualEditor}
                    value={contextText}
                    label="Steering context"
                    readonly={editorReadonly || contextAttachmentBusy}
                    surfaceKey={`context:${contextDocumentId}`}
                    acceptImageAttachments={false}
                    onChange={updateContextText}
                    onCompositionChange={(active) => contextCompositionActive = active}
                    onFormatStateChange={(state) => contextFormatting = state}
                    onGhostPresentationRejected={() => {}}
                  />
                {:else}
                  <SourceEditor
                    bind:element={contextSourceTextarea}
                    label="Steering context Markdown"
                    value={contextText}
                    surfaceKey={`context:${contextDocumentId}`}
                    readonly={editorReadonly || contextAttachmentBusy}
                    onValueInput={(textarea) => updateContextText(textarea.value)}
                    onCompositionStart={() => contextCompositionActive = true}
                    onCompositionEnd={() => contextCompositionActive = false}
                  />
                {/if}
              </div>
              <div class="context-composer-actions">
                <button
                  class="context-attach-button"
                  type="button"
                  aria-label="Attach files to completion context"
                  title="Attach files"
                  disabled={contextAttachmentBusy || editorReadonly}
                  on:click={() => void addContextAttachmentsFromPicker()}
                ><svg aria-hidden="true" viewBox="0 0 18 18"><circle cx="9" cy="9" r="6.25"/><path d="M9 5.75v6.5M5.75 9h6.5"/></svg></button>
                <span class:error={contextTextSaveState === 'error'}>
                  {contextTextSaveState === 'saving' ? 'Saving…' : contextTextSaveState === 'dirty' ? 'Unsaved' : contextTextSaveState === 'error' ? 'Couldn’t save' : 'Saved locally'}
                </span>
              </div>
            </div>
            {#if contextTextSources.length > 0}
              <p class="context-source-note" title={contextTextSources.map((source) => source.file_name).join('\n')}>
                {contextTextSources.length} imported text {contextTextSources.length === 1 ? 'source is' : 'sources are'} editable above
              </p>
            {/if}
            {#if contextAttachments.length > 0}
              <div class="completion-context-items">
                {#each contextAttachments as attachment (attachment.id)}
                  <article class="completion-context-card media-card" title={attachment.warnings.join('\n')}>
                    <div class="completion-context-card-heading">
                      <span class="attachment-glyph" aria-hidden="true">{attachment.presentation_kind === 'image' ? '▧' : attachment.presentation_kind === 'audio' ? '◖' : '◫'}</span>
                      <span class="completion-context-card-copy">
                        <strong>{attachment.file_name}</strong>
                        <small>{attachment.detected_format} · {formatByteCount(attachment.text_bytes + attachment.media.reduce((sum, media) => sum + media.byte_count, 0))}{attachment.coverage_complete ? '' : ' · excerpted'}</small>
                      </span>
                      <button
                        class="attachment-remove"
                        type="button"
                        aria-label={`Remove ${attachment.file_name} from completion context`}
                        disabled={contextAttachmentBusy}
                        on:click={() => void removeContextAttachment(attachment.id)}
                      >×</button>
                    </div>
                    {#each attachment.media as media (media.sha256)}
                      {@const previewUrl = contextMediaUrl(media)}
                      <div class="context-media-preview">
                        {#if media.kind === 'image' && previewUrl}
                          <img src={previewUrl} alt={attachment.file_name} />
                        {:else if media.kind === 'audio' && previewUrl}
                          {#if media.waveform_peaks && media.waveform_peaks.length > 0}
                            <div class="audio-waveform" aria-hidden="true">
                              {#each media.waveform_peaks as peak}
                                <i style={`height:${Math.max(5, peak / 255 * 100)}%`}></i>
                              {/each}
                            </div>
                          {/if}
                          <audio src={previewUrl} controls preload="metadata" aria-label={`Play ${attachment.file_name}`}></audio>
                        {:else}
                          <small class="context-media-error" role="status">Preview unavailable</small>
                        {/if}
                      </div>
                    {/each}
                  </article>
                {/each}
              </div>
            {/if}
          </div>
        {/if}
        {#if missingDocumentRecovery}
          <MissingDocumentRecoveryNotice
            title={missingDocumentRecovery.title}
            relativePath={missingDocumentRecovery.relativePath}
            text={missingDocumentRecovery.text}
            hadUnsavedText={missingDocumentRecovery.hadUnsavedText}
            journalDurable={missingDocumentRecovery.journalDurable}
            draftWasUncertain={missingDocumentRecovery.draftWasUncertain}
            saveWasUncertain={missingDocumentRecovery.saveWasUncertain}
            copyState={missingDocumentCopyState}
            onCopy={() => void copyMissingDocumentRecoveryText()}
          />
        {/if}
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

          <section class="editor-stage" data-attachment-drop="inline" aria-label="Writing surface" on:focusin={(event) => rememberSpeechEditor(event, 'manuscript')}>
            {#if showVisual}
              <div class="editor-pane visual-pane" aria-label="Visual editor pane">
                {#if exactTextSurface}
                  <div class="verse-notice">Verse stays in the exact-whitespace source surface.</div>
                {:else}
                  {#if canUseVisual}
                    <LoomEditor
                      bind:this={visualEditor}
                      collaborative={Boolean(cabalEditor)}
                      value={documentText}
                      label={`${document.summary.title}, manuscript editor`}
                      ghostText={ghostSuggestion?.text ?? ''}
                      ghostCandidateId={ghostSuggestion?.candidateId ?? ''}
                      ghostPresentationKey={ghostSuggestion?.presentationKey ?? ''}
                      ghostAnchorByteOffset={ghostSuggestion?.targetByte ?? null}
                      ghostInsertsOnAccept={ghostSuggestion?.insertsOnAccept ?? false}
                      ghostAlternatives={ghostAlternatives}
                      ghostHidden={inlineGhostHidden({ autocomplete: suggestionsEnabled, shuttle: shuttleEnabled })}
                      {ghostUnconsumeText}
                      surfaceKey={visualGhostSurfaceKey}
                      onChange={updateText}
                      onImageAttachments={storeImageAttachments}
                      onImageAttachmentsCommitted={reportImageAttachmentsCommitted}
                      onImageAttachmentError={reportImageAttachmentError}
                      {resolveImageAssetUrl}
                      onCompositionChange={setVisualComposition}
                      onImmediateDocumentMutation={invalidateVisualSuggestionImmediately}
                      onGhostAccept={acceptActiveGhost}
                      onGhostInsert={authorizeGhostInsertion}
                      onGhostCycle={cycleActiveSuggestion}
                      onGhostUnconsume={authorizeGhostUnconsume}
                      onGhostDismiss={dismissActiveGhost}
                      onGhostPresentationRejected={rejectVisualGhostPresentation}
                      onGhostVisibilityChange={(presentationKey) => {
                        visibleVisualGhostPresentationKey = presentationKey;
                      }}
                      onCompletionAccessibilityChange={(witness) => {
                        visualCompletionAccessibility = witness;
                      }}
                      onSelectionAccessibilityChange={(witness) => {
                        visualSelectionAccessibility = witness;
                      }}
                      onSelectionChange={updateVisualSelection}
                      onCaretNavigation={invalidateCompletionForCaretNavigation}
                      onFormatStateChange={(state) => {
                        visualFormatting = state;
                      }}
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
                {#if sourceCodec && !sourceCodec.editable}
                  <div class="verse-notice" role="alert">This document uses mixed line-ending encodings. Loom will not normalize them silently; source editing stays locked until a lossless boundary editor is available.</div>
                {/if}
                {#if document.summary.kind === 'hybrid'}
                  <div class="verse-notice" role="alert">Hybrid source editing is locked until its prose/verse block manifest can cross the IPC boundary losslessly.</div>
                {/if}
                <SourceEditor
                  bind:this={sourceEditor}
                  bind:element={sourceTextarea}
                  value={sourceDisplayText}
                  readonly={editorReadonly || document.summary.kind === 'hybrid' || Boolean(sourceCodec && !sourceCodec.editable)}
                  verse={exactTextSurface}
                  verseNewline={sourceCodec?.newline ?? null}
                  surfaceKey={completionContextKey}
                  ghostText={sourceGhostSuggestion?.text ?? ''}
                  ghostCandidateId={sourceGhostSuggestion?.candidateId ?? ''}
                  ghostPresentationKey={sourceGhostSuggestion?.presentationKey ?? ''}
                  ghostInsertsOnAccept={sourceGhostSuggestion?.insertsOnAccept ?? false}
                  ghostAlternatives={ghostAlternatives}
                  ghostHidden={inlineGhostHidden({ autocomplete: suggestionsEnabled, shuttle: shuttleEnabled })}
                  {ghostUnconsumeText}
                  onCompositionStart={beginSourceComposition}
                  onCompositionEnd={finishSourceComposition}
                  onValueInput={(textarea) => {
                    updateSourceSelection(textarea);
                    updateFromSource(textarea.value);
                  }}
                  onImageAttachments={storeImageAttachments}
                  onImageAttachmentsCommitted={reportImageAttachmentsCommitted}
                  onImageAttachmentError={reportImageAttachmentError}
                  onSelectionChange={updateSourceSelection}
                  onGhostAccept={acceptActiveGhost}
                  onGhostInsert={authorizeGhostInsertion}
                  onGhostCycle={cycleActiveSuggestion}
                  onGhostUnconsume={authorizeGhostUnconsume}
                  onGhostDismiss={dismissActiveGhost}
                  onGhostVisibilityChange={(presentationKey) => {
                    visibleSourceGhostPresentationKey = presentationKey;
                  }}
                  label={exactTextSurface ? 'Exact-whitespace verse editor' : 'Markdown source editor'}
                />
              </div>
            {/if}
          </section>

          {#if uncertainPromotion}
            <div class="attention-action">
              <button class="secondary-button" type="button" on:click={() => void resolveUncertainPromotion()} disabled={promotionInFlight}>
                {promotionInFlight ? 'Checking suggestion…' : 'Check suggestion result'}
              </button>
            </div>
          {/if}
        {:else}
          <section class="empty-project">
            {#if transition === 'navigation'}
              <h1>Opening your writing…</h1>
              <p>The document controls will appear when the writing surface is ready.</p>
            {:else if uncertainPromotion}
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
      {#each paneSlots as slot (slot.position)}
        {#if slot.selected && !((signalOpen || cabalOpen) && slot.position === 'right') && (slot.position !== 'main' || customMain)}
          {@const selected = slot.selected}
          <aside class:hidden-pane={hiddenPaneSlots.has(slot.position)} class={`workspace-pane-slot workspace-pane-${slot.position}`} aria-label={selected[1].title ?? selected[0]}>
            {#if slot.position === 'right'}<PaneDivider edge="left" label="Resize right pane" size={Math.min(rightWidth, rightLimit)} min={180} max={rightLimit} onResize={(size) => rightWidth = size} />{/if}
            {#if slot.position === 'bottom'}<PaneDivider edge="top" label="Resize bottom pane" size={Math.min(bottomHeight, workspaceHeight * 0.6)} min={100} max={workspaceHeight * 0.6} onResize={(size) => bottomHeight = size} />{/if}
            <header class="workspace-pane-header">
              {#if slot.choices.length > 1}
                <select aria-label="Pane" value={selected[0]} disabled={busyPaneSlots.has(slot.position)} on:change={(event) => selectPane(slot.position, event.currentTarget.value)}>
                  {#each slot.choices as [id, config]}<option value={id}>{config.title ?? id}</option>{/each}
                </select>
              {:else}<span>{selected[1].title ?? selected[0]}</span>{/if}
            </header>
            {#each slot.choices as [paneId, paneConfig] (paneId)}
              <div class="workspace-pane-content" class:hidden-pane={paneId !== selected[0]}>
            <WorkspacePane bind:this={paneEditors[paneId]} paneId={paneId} config={paneConfig} projectId={project.project_id} sessionId={project.session_id} documents={project.documents} source={document} value={documentText} readonly={editorReadonly} collaborative={Boolean(cabalEditor)} onImmediateDocumentMutation={invalidateVisualSuggestionImmediately} onChange={updateText} beforeRun={preparePaneRun} onOpenDocument={(id) => void openPaneDocument(id)} onRunsChanged={() => { void refreshTerminalRuns(); scheduleProjectFilesystemRefresh(0); }} onCompositionChange={(active) => paneComposing = { ...paneComposing, [paneId]: active }} onBusyChange={(busy) => paneBusy = { ...paneBusy, [paneId]: busy }} />
              </div>
            {/each}
          </aside>
        {/if}
      {/each}

      {#if signalOpen}
        <aside class="workspace-pane-slot workspace-pane-right" aria-label="Signal pane">
          <PaneDivider edge="left" label="Resize Signal" size={Math.min(rightWidth, rightLimit)} min={200} max={rightLimit} onResize={size => rightWidth = size} />
          <SignalPane editor={signalDraftEditor} onClose={() => void toggleSignal()} onDraft={draftSignalReply}
            onCabal={inviteSignalConversation} onJoin={(conversation, invitation) => openSignalWorkspace(conversation, invitation, true)}
            onWorkspace={openSignalWorkspace} workspaceScope={project ? `${project.project_id}/${project.session_id}` : ''}
            modelLabel={currentModel?.display_name ?? 'Local model'} />
        </aside>
      {/if}
      {#if cabalOpen}
        <aside class="workspace-pane-slot workspace-pane-right" aria-label="Cabal pane">
          <PaneDivider edge="left" label="Resize cabal" size={Math.min(rightWidth, rightLimit)} min={200} max={rightLimit} onResize={size => rightWidth = size} />
          {#key project?.session_id}
            <CabalPane {cabal} unsaved={Boolean(cabalEditor) && saveState === 'error'} projectName={project?.title ?? 'Workspace'}
              onClose={() => cabalOpen = false} onInvite={inviteCabal}
              onJoin={async (invitation, name) => { await doOpenProject(() => joinCabal(invitation, name)); }}
              onRemove={removeCabalMember} onRecover={recoverCabalCopies} />
          {/key}
        </aside>
      {/if}

    </div>
      <div class="terminal-dock" bind:clientHeight={terminalDockHeight} class:closed={!terminalOpen} style={`height:${effectiveTerminalHeight}px`}>
      {#if terminalOpen}<PaneDivider edge="top" label="Resize terminal" size={effectiveTerminalHeight} min={100} max={terminalLimit} onResize={(size) => terminalHeight = size} />{/if}
      <TerminalPane
        bind:open={terminalOpen}
        bind:entry={terminalEntry}
        projectId={project.project_id}
        sessionId={project.session_id}
        documents={project.documents}
        runs={terminalRuns}
        busy={terminalBusy}
        disabled={applicationClosePhase !== 'running'}
        runDisabled={editorNavigationLocked || (!workspaceCommand(terminalEntry) && (!document || (!terminalEntry && !currentModel?.completion) || editorReadonly))}
        error={terminalError}
        uncertain={terminalPendingRequest !== null}
        onCheck={() => void refreshTerminalRuns()}
        modelLabel={currentModel?.display_name ?? ''}
        onRun={() => void runRetainedOutput(terminalEntry)}
        onCancel={() => void stopTerminalRun()}
        onOpen={(run) => void openTerminalOutput(run)}
        onClose={closeTerminal}
      />
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
          {#if errorMessage && desktop}
            <button class="secondary-button" type="button" on:click={retryInitialProject} disabled={opening}>
              Retry
            </button>
          {/if}
          <button class="secondary-button" type="button" on:click={() => void doOpenProject()} disabled={!desktop || opening}>
            {opening ? 'Opening…' : 'Open folder…'}
          </button>
        </div>
      </section>
    </main>
  {/if}

  {#if deleteDocumentTarget}
    <div
      class="document-delete-backdrop"
      role="presentation"
      on:click={(event) => {
        if (
          event.target === event.currentTarget &&
          !deleteDocumentInFlight &&
          !deleteDocumentUncertain
        ) {
          closeDocumentDeleteConfirmation();
        }
      }}
    >
      <div
        bind:this={deleteDocumentDialog}
        class="document-delete-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="document-delete-title"
        aria-describedby="document-delete-description"
        aria-busy={deleteDocumentInFlight}
        tabindex="-1"
        on:keydown={handleDocumentDeleteDialogKeydown}
      >
        <h2 id="document-delete-title">Delete “{deleteDocumentTarget.title}”?</h2>
        <p id="document-delete-description">
          This deletes the manuscript file and removes it from this project. Other manuscripts are not affected.
          {#if deleteDocumentUncertain} The first result was interrupted, so Loom will check the identical deletion command without issuing a new one.{/if}
        </p>
        <div class="document-delete-actions">
          <button
            bind:this={deleteDocumentCancelButton}
            class="secondary-button"
            type="button"
            disabled={deleteDocumentInFlight || deleteDocumentUncertain}
            on:click={() => closeDocumentDeleteConfirmation()}
          >Cancel</button>
          <button
            class="danger-button"
            type="button"
            disabled={deleteDocumentInFlight}
            on:click={() => void confirmDocumentDelete()}
          >{deleteDocumentInFlight ? 'Checking…' : deleteDocumentUncertain ? 'Check Deletion' : 'Delete Manuscript'}</button>
        </div>
      </div>
    </div>
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
                <strong>Suggestions {suggestionsEnabled ? 'on' : 'off'}</strong>
              </span>
            </label>

            <div class="model-readiness" role="status" aria-live="polite">
              <span
                class:ready={Boolean(currentModel) && !modelLoading && !modelUnloading}
                class:preparing={modelLoading || modelChoosing || modelUnloading || modelDownloadStarting || activeModelDownloads.length > 0}
                class:failed={Boolean(quietModelLoadFailure) && !currentModel}
                class="status-dot"
              ></span>
              <strong>
                {modelLoading || modelChoosing || modelUnloading || modelDownloadStarting || activeModelDownloads.length > 0
                  ? 'Preparing'
                  : currentModel
                    ? 'Model ready'
                    : quietModelLoadFailure
                      ? 'Needs attention'
                      : 'Needs setup'}
              </strong>
            </div>
          </section>

          {#if quietModelLoadFailure && !currentModel}
            <div class="model-setup-error quiet-load-failure" role="alert">
              <span>{quietModelLoadFailure.message}</span>
              <button class="secondary-button compact" type="button" on:click={() => void retryPreferredWriter()}>Retry local writer</button>
            </div>
          {/if}

          <section class="curated-model-catalog" aria-labelledby="curated-model-catalog-title">
            <div class="section-heading">
              <div>
                <h3 id="curated-model-catalog-title">Recommended local model</h3>
                <p>Publisher artifact, revision, size, and checksum are embedded in this Loom build.</p>
              </div>
            </div>

            {#if curatedModelsLoading}
              <div class="runtime-note" role="status">Reading the embedded catalog…</div>
            {:else if curatedModelsError}
              <div class="model-setup-error" role="alert">
                The embedded catalog is unavailable. No download metadata was accepted.
                <button class="bare-button compact" type="button" on:click={() => void refreshCuratedModels()}>Retry</button>
              </div>
            {:else}
              {#each curatedModels as entry (entry.catalog_id)}
                {@const installed = localCatalogModel(entry)}
                {@const projectorInstalled = localCatalogProjector(entry)}
                {@const resident = loadedCatalogModel(entry)}
                {@const transfer = catalogDownload(entry)}
                <article class="curated-model-card">
                  <div class="curated-model-copy">
                    <strong>{entry.display_name}</strong>
                    <span>{entry.publisher} · {formatByteCount(entry.expected_bytes)} · {entry.context_tokens.toLocaleString()} token context</span>
                    <span>{formatByteCount(entry.memory_fit.recommended_system_memory_bytes)} or more system memory recommended</span>
                    <span>Local only · text, image, and audio · {entry.license.name} · native inspection required before use</span>
                    <details class="model-technical">
                      <summary>Pinned artifact details</summary>
                      <dl class="model-evidence">
                        <div><dt>Repository</dt><dd><code>{entry.repository}</code></dd></div>
                        <div><dt>Revision</dt><dd><code>{entry.revision}</code></dd></div>
                        <div><dt>File</dt><dd><code>{entry.artifact_name}</code></dd></div>
                        <div><dt>SHA-256</dt><dd><code>{entry.expected_sha256}</code></dd></div>
                        <div><dt>Projector</dt><dd><code>{entry.projector.artifact_name}</code></dd></div>
                        <div><dt>Projector SHA-256</dt><dd><code>{entry.projector.expected_sha256}</code></dd></div>
                        <div><dt>License</dt><dd>{entry.license.spdx_id}</dd></div>
                        <div><dt>License source</dt><dd><code>{entry.license.url}</code></dd></div>
                      </dl>
                    </details>
                  </div>
                  <div class="curated-model-action">
                    {#if resident}
                      <button class="secondary-button compact" type="button" disabled>In use</button>
                    {:else if installed && projectorInstalled}
                      <button
                        class="primary-button compact"
                        type="button"
                        on:click={() => void useCatalogSuggestionWriter(entry, installed)}
                        disabled={!desktop || modelLoading || modelChoosing || modelUnloading}
                      >{modelLoading && selectedModelPath === installed.model_path ? 'Verifying…' : 'Verify local copy'}</button>
                    {:else if transfer?.status.status === 'completed' && projectorInstalled}
                      <button class="secondary-button compact" type="button" on:click={() => void refreshCurrentModelsAndEnsureWriter()}>Refresh installed copy</button>
                    {:else}
                      <button
                        class="primary-button compact"
                        type="button"
                        on:click={() => void beginCatalogModelDownload(entry)}
                        disabled={!desktop || modelDownloadStarting || pendingModelDownload !== null || (transfer !== undefined && !modelDownloadIsTerminal(transfer))}
                      >{transfer !== undefined && !modelDownloadIsTerminal(transfer) ? 'Downloading…' : transfer?.status.status === 'failed' || transfer?.status.status === 'cancelled' ? 'Retry verified download' : 'Download and verify'}</button>
                    {/if}
                  </div>
                </article>
              {/each}
            {/if}
          </section>

          {#if suggestionSetupNeeded}
            <section class="model-setup-callout">
              <div class="model-setup-intro">
                <strong>Private writing model</strong>
                <p>Choose any local text GGUF. Loom inspects it locally before use; vision adapters are excluded and nothing leaves this computer.</p>
              </div>

              {#if availableWriterModels.length > 0}
                <div class="writer-model-list" aria-label="Compatible writing models">
                  {#each availableWriterModels as model (model.model_path)}
                    <article class="writer-model-choice">
                      <div>
                        <strong>{model.display_name}</strong>
                        <span>{formatByteCount(model.file_bytes)} · Local text GGUF</span>
                      </div>
                      <button
                        class="primary-button compact"
                        type="button"
                        on:click={() => void useDiscoveredSuggestionWriter(model)}
                        disabled={!desktop || modelChoosing || modelLoading || modelUnloading}
                      >{modelLoading && selectedModelPath === model.model_path ? 'Verifying…' : 'Verify and use'}</button>
                    </article>
                  {/each}
                </div>
              {:else}
                <div class="model-empty-state">
                  <strong>No recommended writing model found.</strong>
                  <span>Choose any local text-model GGUF below or locate one without moving it.</span>
                </div>
              {/if}

              {#if modelSetupError}
                <p class="model-setup-error" role="alert">{modelSetupError}</p>
              {/if}

              <div class="model-setup-actions">
                <button
                  class={availableWriterModels.length > 0 ? 'bare-button compact' : 'secondary-button'}
                  type="button"
                  on:click={() => void chooseSuggestionWriterModel()}
                  disabled={!desktop || modelChoosing || modelLoading || modelUnloading}
                >{modelChoosing ? 'Locating…' : 'Locate model file…'}</button>
                <button class="bare-button compact" type="button" on:click={() => void refreshCurrentModelsAndEnsureWriter()} disabled={!desktop || modelChoosing || modelLoading || modelUnloading}>Refresh library</button>
              </div>
            </section>
          {/if}

          <details class="model-advanced-panel">
            <summary>Advanced</summary>
            <div class="model-advanced-content">
            {#if selectedModel?.loaded}
              <section class="model-library" aria-labelledby="model-library-title">
            <div class="section-heading">
              <div>
                <h3 id="model-library-title">Loaded model</h3>
                <p>Native runtime details for the writer currently in memory.</p>
              </div>
            </div>
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
              </section>
            {/if}

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
  {#if speechRecovery && project}
    <div class="toast speech-recovery" role="alert">
      <span>Dictation is preserved because its original insertion point changed.</span>
      <button type="button" on:click={insertRecoveredSpeech}>Insert at current caret</button>
      <button type="button" on:click={() => { speechRecovery = null; }} aria-label="Discard preserved dictation">×</button>
    </div>
  {/if}
  <div class="sr-only" aria-live="polite">{liveRegion}</div>
  <div class="sr-only" role="note" aria-label="Completion session witness">
    {completionAccessibilityWitness}
  </div>
</div>
