import { readFileSync } from 'node:fs';
import { compile } from 'svelte/compiler';
import { describe, expect, it } from 'vitest';

function dependencyThunkFor(compiled: string, assignment: string): string {
  const assignmentIndex = compiled.indexOf(assignment);
  expect(assignmentIndex).toBeGreaterThan(0);
  const effectIndex = compiled.lastIndexOf('$.legacy_pre_effect(', assignmentIndex);
  expect(effectIndex).toBeGreaterThan(0);
  return compiled.slice(effectIndex, assignmentIndex);
}

describe('App ghost reactivity wiring', () => {
  it('tracks late branch hydration and caret changes in both ghost effects', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const compiled = compile(source, {
      filename: 'App.svelte',
      generate: 'client',
      dev: false
    }).js.code;

    const visual = dependencyThunkFor(compiled, '$.set(visualAutocompleteDisposition');
    expect(visual).toContain('verifiedBranchBodyByRun');
    expect(visual).toContain('currentReadyBranches');
    expect(visual).toContain('visualGhostTargetByte');
    expect(visual).toContain('branchPromotionReady');

    const sourceGhost = dependencyThunkFor(compiled, '$.set(sourceAutocompleteDisposition');
    expect(sourceGhost).toContain('verifiedBranchBodyByRun');
    expect(sourceGhost).toContain('currentReadyBranches');
    expect(sourceGhost).toContain('sourceGhostTargetByte');
    expect(sourceGhost).toContain('branchPromotionReady');

    const retry = dependencyThunkFor(compiled, '$.set(retryEvaluationSnapshot');
    expect(retry).toContain('visualAutocompleteDisposition');
    expect(retry).toContain('sourceAutocompleteDisposition');

    const visualFamily = dependencyThunkFor(compiled, '$.set(visualSuggestionFamily');
    expect(visualFamily).toContain('branches');
    expect(visualFamily).toContain('verifiedBranchBodyByRun');
    expect(visualFamily).toContain('liveBranchTextByRun');
    expect(visualFamily).toContain('liveBranchTextSequenceByRun');
    expect(visualFamily).toContain('currentModel');
    expect(visualFamily).toContain('branchPromotionReady');
    expect(visualFamily).toContain('documentText');

    const sourceFamily = dependencyThunkFor(compiled, '$.set(sourceSuggestionFamily');
    expect(sourceFamily).toContain('sourceDisplayText');
    expect(source).not.toContain('A private strand is ready');
  });

  it('keeps cached completion identity stable across autosave revision changes', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const context = source.slice(
      source.indexOf('$: completionContextKey ='),
      source.indexOf('$: visualGhostSurfaceKey =')
    );
    expect(context).toContain('completionSessionContextKey');
    expect(context).not.toContain('revision_id');
    expect(context).not.toContain('visible_blob_id');
  });

  it('treats generation events only as coalesced completion snapshot wakeups', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const handler = source.slice(
      source.indexOf('function handleGenerationEnvelope'),
      source.indexOf('function validateBranchSnapshots')
    );
    const refresh = source.slice(
      source.indexOf('async function refreshBranchesFor'),
      source.indexOf('function refreshCurrentBranches')
    );

    expect(handler).toContain('generationEventBelongsToScope');
    expect(handler).toContain('branchRefreshTimer === undefined');
    expect(handler).toContain('scheduleBranchRefresh()');
    expect(handler).not.toContain('envelope.event');
    expect(handler).not.toContain('text_delta');
    expect(refresh).toContain('getCompletionSnapshot');
    expect(refresh).toContain('completionSnapshotFacts');
    expect(refresh).toContain('liveBranchTextByRun = completionFacts.liveTextByRun');
    expect(refresh).toContain(
      'liveBranchTextSequenceByRun = completionFacts.liveTextSequenceByRun'
    );
  });

  it('forces authoritative completion recovery whenever the renderer resumes', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const recovery = source.slice(
      source.indexOf('function resumeCompletionObservation'),
      source.indexOf('function installGenerationEventListener')
    );
    const focus = source.slice(
      source.indexOf('async function installWindowFocusHandler'),
      source.indexOf('function handleRendererResume')
    );

    expect(source).toContain("window.addEventListener('pageshow', handleRendererResume)");
    expect(source).toContain(
      "window.document.addEventListener('visibilitychange', handleRendererResume)"
    );
    expect(focus).toContain("if (mode === 'visual') visualEditor?.refreshGhostPresentation()");
    expect(focus).toContain('resumeCompletionObservation()');
    expect(recovery).toContain('installGenerationEventListener()');
    expect(recovery).toContain('window.clearTimeout(branchPollTimer)');
    expect(recovery).toContain('branchPollAttempt = 0');
    expect(recovery).toContain('scheduleBranchRefresh()');
  });

  it('keeps suggestion review and implementation evidence out of the quiet titlebar', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    expect(source).not.toContain('Skip to manuscript');
    expect(source).not.toContain('alternatives-button');
    expect(source).not.toContain('Review suggestions');
    expect(source).not.toContain('Insert suggestion');
    expect(source).not.toContain('strand-evidence');
    expect(source).toContain('class="canvas-controls"');
    expect(source).toContain('on:mousedown={startTitlebarDrag}');
    expect(source).not.toContain('Autosave is always on');
    expect(source).not.toContain('class="save-status');
    expect(source).not.toContain('autosaveLabel');
    expect(source).toContain('New document (⌘N)');
  });

  it('uses compact stateful controls for mode, autocomplete, and Shuttle', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    expect(source).toContain('class="titlebar-button mode-toggle"');
    expect(source).toContain("aria-label={mode === 'visual' ? 'Switch to Markdown editor' : 'Switch to visual editor'}");
    expect(source).toContain('class="titlebar-button suggestions-toggle"');
    expect(source).toContain("aria-label={suggestionsEnabled ? 'Turn autocomplete off' : 'Turn autocomplete on'}");
    expect(source).toContain('class="titlebar-button shuttle-toggle"');
    expect(source).toContain("aria-label={shuttleEnabled ? 'Turn Shuttle off' : 'Turn Shuttle on'}");
    expect(source).toContain('disabled={!project || suggestionsChanging}');
    expect(source).not.toContain('disabled={!suggestionsEnabled || !currentModel}');
    expect(source).toContain('completionAutomationEnabled(suggestionsEnabled, enabled)');
    expect(source).toContain('inlineGhostHidden({ autocomplete: suggestionsEnabled, shuttle: shuttleEnabled })');
    expect(source).toContain("if (event.key === 'Escape' && shuttleEnabled)");
    expect(source).not.toContain('>Write</button>');
    expect(source).not.toContain('>Shuttle</button>');
    const toggle = source.slice(
      source.indexOf('async function toggleSuggestionsFromTitlebar'),
      source.indexOf('function clearAutocompleteModelMenuLongPress')
    );
    expect(toggle).toContain('setSuggestionsEnabled(!suggestionsEnabled)');
    expect(toggle).not.toContain('openModelManager');
    const modelMenu = source.slice(
      source.indexOf('function clearAutocompleteModelMenuLongPress'),
      source.indexOf('function focusableElementsWithin')
    );
    expect(modelMenu).toContain('isAutocompleteModelMenuKey(event)');
    expect(modelMenu).toContain('openModelManager(event.currentTarget as HTMLButtonElement)');
    expect(modelMenu).toContain('AUTOCOMPLETE_MODEL_MENU_LONG_PRESS_MS');
    expect(source).toContain('on:contextmenu={openAutocompleteModelMenu}');
    expect(source).toContain('aria-keyshortcuts="Shift+F10"');
    expect(source).not.toContain('class="titlebar-button gear-button"');
    expect(source).not.toContain('class="project-menu"');
    expect(source).not.toContain('class="writer-onboarding"');
    expect(source).not.toContain('Set up private writing suggestions');
    expect(source).toContain('class="model-setup-callout"');
    expect(source).toContain('Private writing model');
  });

  it('keeps system-aware appearance persistence behind one direct toggle and binds curated downloads', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const catalogLoad = source.slice(
      source.indexOf('async function refreshCuratedModels'),
      source.indexOf('function closeModelManager')
    );
    const catalogDownload = source.slice(
      source.indexOf('async function beginCatalogModelDownload'),
      source.indexOf('async function beginOrRetryModelDownload')
    );
    const appearance = source.slice(
      source.indexOf('function setAppearance'),
      source.indexOf('function startTitlebarDrag')
    );
    const catalogAdmission = source.slice(
      source.indexOf('async function activateSuggestionWriter'),
      source.indexOf('async function chooseSuggestionWriterModel')
    );

    expect(catalogLoad).toContain('validateCuratedModelCatalog(await listCuratedModels())');
    expect(catalogDownload).toContain('catalogDownloadRequest(entry)');
    expect(catalogDownload).toContain('pendingModelDownload = { commandId: newUlid(), ...request }');
    expect(catalogAdmission).toContain(
      'await loadCatalogModelCandidate(catalogEntry.catalog_id, selected.model_path)'
    );
    expect(catalogAdmission).toContain('!isVerifiedCatalogWriter(catalogEntry, loaded)');
    expect(source).toContain('useCatalogSuggestionWriter(entry, installed)');
    expect(source).not.toContain('useDiscoveredSuggestionWriter(installed)');
    expect(appearance).toContain('persistAppearancePreference(window, next)');
    expect(source).toContain('on:click={toggleAppearance}');
    expect(source).not.toContain("{#each ['system', 'light', 'dark'] as choice}");
  });

  it('keeps both empty writing surfaces accessible without instructional placeholder copy', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const visual = readFileSync(new URL('./LoomEditor.svelte', import.meta.url), 'utf8');
    const markdown = readFileSync(new URL('./SourceEditor.svelte', import.meta.url), 'utf8');
    expect(visual).not.toContain('Start writing');
    expect(markdown).not.toContain('Start writing');
    expect(visual).toContain("'aria-label': currentLabel");
    expect(visual).toContain('attributes: editorAttributes(snapshot.label)');
    expect(markdown).toContain('aria-label={label}');
    expect(source).toContain('autofocus={true}');
  });

  it('announces image success only from an editor insertion acknowledgement', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const storage = source.slice(
      source.indexOf('async function storeImageAttachments'),
      source.indexOf('function resolveImageAssetUrl')
    );
    const committed = source.slice(
      source.indexOf('function reportImageAttachmentsCommitted'),
      source.indexOf('function updateSourceSelection')
    );

    expect(storage).not.toContain('Image attached');
    expect(storage).not.toContain('images attached');
    expect(storage).not.toContain('Promise.all');
    expect(storage).toContain('for (const file of files)');
    expect(committed).toContain("announce(count === 1 ? 'Image attached'");
    expect(source.match(/onImageAttachmentsCommitted=\{reportImageAttachmentsCommitted\}/gu))
      .toHaveLength(2);
    expect(source).toContain("convertFileSrc(token, 'loom-asset')");
  });

  it('opens the writing surface before optional completion setup', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const openProject = source.slice(
      source.indexOf('async function finishOpeningProject'),
      source.indexOf('async function selectDocument')
    );
    const background = source.slice(
      source.indexOf('async function restoreCompletionBackground'),
      source.indexOf('async function restoreDesktopWorkspace')
    );

    expect(openProject).toContain('await selectDocument(first)');
    expect(openProject).not.toContain('getBuildModelPolicy');
    expect(openProject).not.toContain('setSuggestionsPolicy');
    expect(background).toContain('await restoreCompletionAutomation(captured)');
    expect(source).toContain('void refreshBranchesFor(\n        source.projectId');
    expect(source).not.toContain('await refreshBranchesFor(\n        source.projectId');
    expect(source).toContain("{#if transition === 'idle'}\n          <button\n            class=\"titlebar-button new-document-button\"");
    expect(source).toContain('Opening your writing…');
  });

  it('rebinds completion after an incompatible caret navigation settles', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const visual = readFileSync(new URL('./LoomEditor.svelte', import.meta.url), 'utf8');
    const controller = readFileSync(new URL('./completionController.ts', import.meta.url), 'utf8');
    const invalidate = source.slice(
      source.indexOf('function invalidateCompletionForCaretNavigation'),
      source.indexOf('function acceptActiveGhost')
    );
    const sourceSelection = source.slice(
      source.indexOf('function updateSourceSelection'),
      source.indexOf('function updateVisualSelection')
    );
    const visualSelection = source.slice(
      source.indexOf('function updateVisualSelection'),
      source.indexOf('function scheduleSourceProjection')
    );

    expect(invalidate).toContain('invalidateControllerNavigation(');
    expect(invalidate).toContain('clearSuggestionTimerHandle()');
    expect(invalidate).toContain('applyCompletionEffects(invalidated.effects)');
    expect(controller).toContain("{ kind: 'cancel_active_branches' }");
    expect(sourceSelection).toContain('invalidateCompletionForCaretNavigation()');
    expect(sourceSelection).toContain("scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'caret_navigation')");
    expect(visualSelection).toContain('invalidateCompletionForCaretNavigation()');
    expect(visualSelection).toContain('settleCompletionNavigation(');
    expect(visualSelection).toContain("scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'caret_navigation')");
    expect(source).toContain('onCaretNavigation={invalidateCompletionForCaretNavigation}');
    expect(visual).toContain('onCaretNavigation();');
    expect(source).toContain('completionGenerationIsArmed(completionGenerationIntent, completionContextKey, editVersion)');
  });

  it('keeps completion state and transition authority in one private pure controller', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const controller = readFileSync(new URL('./completionController.ts', import.meta.url), 'utf8');

    expect(source).toContain('let completionController = initialCompletionControllerState();');
    expect(source).not.toContain('let completionSession:');
    expect(source).not.toContain('let pendingCompletionText:');
    expect(source).not.toContain('let scheduledSuggestion:');
    expect(source).toContain('authorizeCompletionInsertion(completionController');
    expect(source).toContain('completionExhausted(');
    const immediateVisualMutation = source.slice(
      source.indexOf('function invalidateVisualSuggestionImmediately'),
      source.indexOf('function setSourceDocument')
    );
    const immediateSourceMutation = source.slice(
      source.indexOf('function updateFromSource'),
      source.indexOf('function scheduleSourceProjection')
    );
    expect(immediateVisualMutation).toContain('completionController.pendingText !== null');
    expect(immediateVisualMutation).not.toContain('pendingCompletionText !== null');
    expect(immediateSourceMutation).toContain('completionController.pendingText !== null');
    expect(immediateSourceMutation).toContain('completionController.session');
    expect(immediateSourceMutation).not.toContain('pendingCompletionText !== null');
    expect(immediateSourceMutation).not.toMatch(/\bcompletionSession\b/u);
    expect(source).toContain('authoritativeCompletionFamilyId = started.command_id;');
    expect(source).toContain('authoritativeFamilyId: authoritativeCompletionFamilyId');
    expect(source).toContain('authoritativeCompletionFamilyId = null;');
    expect(source).toContain(
      'started.branches.some((branch) => branch.weave_command_id !== started.command_id)'
    );
    expect(controller).not.toContain('window.');
    expect(controller).not.toContain('invoke(');
    expect(controller).toContain('export type CompletionControllerEffect');
  });

  it('keeps source completion echoes synchronous and source surface identity stable across autosave', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const sourceEditor = readFileSync(new URL('./SourceEditor.svelte', import.meta.url), 'utf8');
    const sourceMutation = source.slice(
      source.indexOf('function updateFromSource'),
      source.indexOf('function updateSourceSelection')
    );
    expect(sourceMutation).toContain('if (completionController.pendingText !== null) commitSourceDraft();');
    expect(source).toContain('surfaceKey={completionContextKey}');
    expect(source).not.toContain('surfaceKey={`${project.session_id}:${document.summary.document_id}:${document.summary.revision_id}');
    expect(sourceEditor).toContain('observedValue = element.value;');
  });

  it('arms a fresh completion batch after changing editor modes', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const modeChange = source.slice(source.indexOf('async function setMode'), source.indexOf('function announce'));
    expect(modeChange).toContain("scheduleAutomaticSuggestions(editVersion, suggestionsIdleDelayMs, 'document_open')");
  });

  it('retains one exact completion intent across transient readiness changes', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const arm = source.slice(
      source.indexOf('function armSuggestionSchedule'),
      source.indexOf('function queueScheduledSuggestionAttempt')
    );
    const start = source.slice(
      source.indexOf('async function tryStartAutomaticSuggestions'),
      source.indexOf('function scheduleDraftJournal')
    );

    expect(source).toContain('automaticCompletionLifecycle({');
    expect(source).toContain('resumeScheduledAutomaticSuggestion(completionSchedulerWakeKey)');
    expect(arm).not.toContain('!currentModel');
    expect(start).toContain('retainsScheduledCompletion(completionLifecycle)');
    expect(start).not.toContain("saveState === 'dirty'");
    expect(source).toContain(
      'aria-describedby="completion-lifecycle-help autocomplete-model-menu-help"'
    );
    expect(source).toContain('{completionLifecycleHelp}</span>');
  });

  it('uses the macOS overlay titlebar for one integrated toolbar', () => {
    const config = JSON.parse(readFileSync(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
    const capability = JSON.parse(readFileSync(new URL('../../src-tauri/capabilities/default.json', import.meta.url), 'utf8'));
    const mainWindow = config.app.windows[0];
    expect(mainWindow.titleBarStyle).toBe('Overlay');
    expect(mainWindow.hiddenTitle).toBe(true);
    expect(capability.permissions).toContain('core:window:allow-start-dragging');
    expect(capability.permissions).toContain('core:window:allow-is-fullscreen');
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const styles = readFileSync(new URL('../app.css', import.meta.url), 'utf8');
    expect(source).toContain('class:native-fullscreen={nativeFullscreen}');
    expect(source).toContain('observeNativeFullscreen(');
    expect(styles).toContain('--titlebar-leading-inset: 76px;');
    expect(styles).toContain('.canvas-controls.native-fullscreen { --titlebar-leading-inset: 8px; }');
    expect(source).toContain('class="titlebar-drag-surface"');
    expect(source).not.toContain('data-tauri-drag-region');
    expect(source).toContain('on:mousedown={startTitlebarDrag}');
    expect(source).toContain('getCurrentWindow().startDragging()');
    expect(source).toContain('<span class="titlebar-document-title">{nativeWindowTitle}</span>');
  });

  it('keeps the document sidebar nonmodal and persistent', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const sidebar = source.slice(source.indexOf('<aside'), source.indexOf('</aside>') + '</aside>'.length);
    expect(source).toContain('<aside');
    expect(source).not.toContain('class="outline-scrim"');
    expect(sidebar).not.toContain('aria-modal="true"');
    expect(sidebar).not.toContain('<span>Manuscript</span>');
    expect(source).not.toContain("await setOutlineOpen(false);\n                await selectDocument");
  });

  it('binds an accessible document context menu to one captured immutable target', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const ipc = readFileSync(new URL('./ipc.ts', import.meta.url), 'utf8');
    const sidebar = source.slice(source.indexOf('<aside'), source.indexOf('</aside>') + '</aside>'.length);
    const action = source.slice(
      source.indexOf('async function runDocumentContextAction'),
      source.indexOf('function suggestionPreferenceKey')
    );
    const documentCalls = ipc.slice(
      ipc.indexOf('export function openDocument'),
      ipc.indexOf('export function checkpointDocument')
    );
    const rename = source.slice(
      source.indexOf('async function beginDocumentRename'),
      source.indexOf('function cancelDocumentRename')
    );
    const cancelRename = source.slice(
      source.indexOf('function cancelDocumentRename'),
      source.indexOf('async function commitDocumentRename')
    );
    const commitRename = source.slice(
      source.indexOf('async function commitDocumentRename'),
      source.indexOf('function handleDocumentRenameKeydown')
    );
    const readonly = source.slice(
      source.indexOf('$: editorReadonly ='),
      source.indexOf('$: reconciliationResolutionLocked')
    );

    expect(sidebar).toContain('aria-haspopup="menu"');
    expect(sidebar).toContain('on:contextmenu={(event) => handleDocumentContextPointer(event, candidate)}');
    expect(sidebar).toContain('on:keydown={(event) => handleDocumentContextKey(event, candidate)}');
    expect(sidebar).toContain('role="menu"');
    expect(sidebar).toContain('role="menuitem"');
    expect(sidebar).toContain('>Open</button>');
    expect(sidebar).toContain('>Rename…</button>');
    expect(sidebar).toContain('>Export Text…</button>');
    expect(sidebar).toContain('{documentContextRevealLabel}</button>');
    expect(action).toContain('const target = documentContextTarget;');
    expect(action).toContain('closeDocumentContextMenu(false);');
    expect(action.indexOf('closeDocumentContextMenu(false);'))
      .toBeLessThan(action.indexOf('await exportDocumentCopy('));
    expect(documentCalls).toContain('expectedRevisionId');
    expect(documentCalls).toContain('expectedBlobId');
    expect(documentCalls).not.toContain('relativePath');
    expect(source).toContain('data-document-title');
    expect(source).toContain('bind:value={renameDocumentTitle}');
    expect(sidebar).toContain('on:compositionstart={handleDocumentRenameCompositionStart}');
    expect(sidebar).toContain('on:compositionend={handleDocumentRenameCompositionEnd}');
    expect(sidebar).toContain('on:blur={handleDocumentRenameBlur}');
    expect(rename.indexOf('flushEditors()'))
      .toBeLessThan(rename.indexOf('refreshDocumentRenameTarget('));
    expect(rename.indexOf('renameDocumentEditorLocked = true'))
      .toBeLessThan(rename.indexOf('flushEditors()'));
    expect(rename).toContain('flushCurrentDocument');
    expect(cancelRename).toContain('renameDocumentEditorLocked = false');
    expect(readonly).toContain('renameDocumentEditorLocked');
    expect(commitRename).toContain('capturedDocumentIdentityIsCurrent(target, project)');
    expect(commitRename).toContain('renameDocumentComposition.active');
    expect(source).toContain('boundedDocumentTitleInput(input.value)');
    expect(action).toContain("case 'rename':");
  });

  it('does not let a handled rename or menu key escape into global Shuttle controls', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const renameKeydown = source.slice(
      source.indexOf('function handleDocumentRenameKeydown'),
      source.indexOf('function handleDocumentContextMenuKeydown')
    );
    const menuKeydown = source.slice(
      source.indexOf('function handleDocumentContextMenuKeydown'),
      source.indexOf('function recordDocumentContextFailure')
    );
    const globalKeydown = source.slice(
      source.indexOf('function handleGlobalKeydown(event:'),
      source.indexOf('function newDocumentShortcut')
    );
    const captureKeydown = source.slice(
      source.indexOf('function handleGlobalKeydownCapture'),
      source.indexOf('function handleGlobalKeydown(event:')
    );

    expect(renameKeydown).toContain("if (event.key === 'Escape')");
    expect(renameKeydown).toContain('renameDocumentComposition.ownsCommandKey(event)');
    expect(renameKeydown).toContain('event.stopPropagation()');
    expect(renameKeydown).toContain('event.preventDefault()');
    expect(menuKeydown).toContain("case 'dismiss':");
    expect(menuKeydown).toContain('event.preventDefault()');
    expect(globalKeydown.indexOf('if (event.defaultPrevented) return;'))
      .toBeLessThan(globalKeydown.indexOf("if (event.key === 'Escape' && shuttleEnabled)"));
    expect(source).toContain("window.addEventListener('keydown', handleGlobalKeydownCapture, true)");
    expect(source).toContain("window.removeEventListener('keydown', handleGlobalKeydownCapture, true)");
    expect(captureKeydown).toContain('shouldCaptureFormatMenuEscape');
    expect(captureKeydown).toContain('compositionActive');
    expect(captureKeydown).toContain('documentRenameOwnsEscape');
    expect(captureKeydown).toContain('documentMenuOwnsEscape');
    expect(captureKeydown).toContain('modelManagerOwnsEscape');
    expect(captureKeydown).toContain('event.stopPropagation()');
    expect(captureKeydown).toContain('closeFormatMenu()');
  });

  it('keeps the source-to-visual control available and checks exactness after flushing source edits', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const mode = source.slice(
      source.indexOf('async function setMode'),
      source.indexOf('function announce')
    );
    expect(source).toContain('disabled={editorReadonly}');
    expect(source).not.toContain("disabled={editorReadonly || (mode === 'source' && !canUseVisual)}");
    expect(mode.indexOf('flushEditors()')).toBeLessThan(mode.indexOf('canUseVisualMarkdown(documentText, false)'));
    expect(mode).toContain("'visual_markdown_not_exact'");
  });

});
