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
    expect(focus).toContain('scheduleProjectFilesystemRefresh()');
    expect(source).toContain('function refreshProjectFilesystemState()');
    expect(source).toContain('const refreshed = await currentProjectSession()');
    expect(recovery).toContain('installGenerationEventListener()');
    expect(recovery).toContain('window.clearTimeout(branchPollTimer)');
    expect(recovery).toContain('branchPollAttempt = 0');
    expect(recovery).toContain('scheduleBranchRefresh()');
  });

  it('uses native filesystem events only as exact-session coalesced refresh wakeups', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const install = source.slice(
      source.indexOf('async function installDocumentFilesystemHintListener'),
      source.indexOf('function scheduleProjectFilesystemRefresh')
    );
    const schedule = source.slice(
      source.indexOf('function scheduleProjectFilesystemRefresh'),
      source.indexOf('async function settleMissingDocumentRecoveryBoundary')
    );

    expect(source).toContain('void installDocumentFilesystemHintListener()');
    expect(source).toContain('unlistenDocumentFilesystemHints?.()');
    expect(install).toContain('routeDocumentFilesystemHint(');
    expect(install).toContain('scheduleProjectFilesystemRefresh');
    expect(install).not.toContain('refreshProjectFilesystemState(');
    expect(schedule).toContain('window.clearTimeout(projectFilesystemRefreshTimer)');
    expect(schedule).toContain('window.setTimeout(() =>');
    expect(schedule).toContain('void refreshProjectFilesystemState()');
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
    expect(source).toContain('class:needs-attention={suggestionsEnabled && !currentModel && Boolean(quietModelLoadFailure)}');
    expect(source).toContain("modelSetupError = `Automatic writer setup failed. ${terminalFailure.message}`");
    expect(source).toContain('Retry local writer');
    expect(source).not.toContain('class:preparing={suggestionsEnabled && !currentModel}');
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
    expect(catalogDownload).toContain('const requests = catalogDownloadRequests(entry)');
    expect(catalogDownload).toContain('for (const request of requests)');
    expect(catalogDownload).toContain('const commandId = newUlid()');
    expect(catalogDownload).toContain('expectedSha256: request.sha256');
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

  it('flushes the embedded context editor before disruptive pane transitions', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const close = source.slice(
      source.indexOf('function closeContextPane'),
      source.indexOf('function focusContextEditorAtEnd')
    );
    const modeSwitch = source.slice(source.indexOf('async function setMode'));

    expect(source).toContain('bind:this={contextVisualEditor}');
    expect(source).toContain('acceptImageAttachments={false}');
    expect(close).toContain('flushContextEditorProjection()');
    expect(close).toContain('contextToggleElement?.focus()');
    expect(modeSwitch).toContain('flushEditors()');
    expect(modeSwitch).toContain('if (contextPaneOpen) focusContextEditorAtEnd()');
    expect(source).toContain("on:compositionstart={() => contextCompositionActive = true}");
    expect(source).toContain("on:compositionend={() => contextCompositionActive = false}");
    expect(source).toContain("editor={contextPaneOpen ? contextVisualEditor : visualEditor}");
  });

  it('adopts backend-owned context Markdown after attachment mutations', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const add = source.slice(
      source.indexOf('async function addContextAttachmentsFromPicker'),
      source.indexOf('async function removeContextAttachment')
    );
    const remove = source.slice(
      source.indexOf('async function removeContextAttachment'),
      source.indexOf('function nativeDropPoint')
    );

    expect(add).toContain('await persistCurrentContextText()');
    expect(add).toContain('adoptAuthoritativeContext(');
    expect(remove).toContain('await persistCurrentContextText()');
    expect(remove).toContain('adoptAuthoritativeContext(');
    expect(source).not.toContain('appendContextAttachmentMarkers');
  });

  it('keeps the context and outline controls independent', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const outsidePointer = source.slice(
      source.indexOf('function handleGlobalPointerdown'),
      source.indexOf('function captureWeaveCursorByte')
    );

    expect(outsidePointer).toContain("event.target.closest('.canvas-controls')");
    expect(source).toContain('on:click={() => void setOutlineOpen(!outlineOpen)}');
    expect(source).not.toContain('outlineOpen = open;\n    contextPaneOpen = false;');
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
    expect(openProject).toContain('scheduleProjectFilesystemRefresh(0)');
    expect(openProject.indexOf('scheduleProjectFilesystemRefresh(0)'))
      .toBeGreaterThan(openProject.indexOf('await selectDocument(first)'));
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
      'aria-describedby="completion-lifecycle-help autocomplete-model-menu-help autocomplete-model-failure-help"'
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

  it('uses a text-first pop-down for saved completion context', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const styles = readFileSync(new URL('../app.css', import.meta.url), 'utf8');
    const ipc = readFileSync(new URL('./ipc.ts', import.meta.url), 'utf8');

    expect(source).toContain('label="Steering context"');
    expect(source).toContain('aria-label="Steering context Markdown"');
    expect(source).not.toContain('aria-label="Visual context editor"');
    expect(source).not.toContain('aria-label="Markdown context editor"');
    expect(source).toContain("{#if mode === 'visual' && canUseVisualMarkdown(contextText, true)}");
    expect(source).toContain('on:input={(event) => updateContextText(event.currentTarget.value)}');
    expect(source).toContain('adoptAuthoritativeContext(');
    expect(source).not.toContain('appendContextAttachmentMarkers');
    expect(source).toContain('setDocumentContextSnapshot(');
    expect(source).toContain('Attach files to completion context');
    expect(source).toContain('<path d="M2.75 7h12.5"/>');
    expect(source).not.toContain('M6.2 9.8 10.8 5');
    expect(styles).toContain('.completion-context-pane { position: absolute;');
    expect(styles).toContain('padding-inline: max(var(--writing-gutter), calc((100% - 82ch) / 2));');
    expect(styles).toContain('.context-editor-surface .loom-editor-shell, .context-editor-surface .editor-mount { height: 100%;');
    expect(ipc).toContain("call('document_context_snapshot_set'");
    expect(source).toContain('media.preview_token');
  });

  it('projects native context media through document-scoped asset tokens without duplicating imported text', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const styles = readFileSync(new URL('../app.css', import.meta.url), 'utf8');
    const tauri = readFileSync(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8');

    expect(source).toContain('contextText = snapshot.markdown');
    expect(source).toContain('contextTextSources = snapshot.text_sources');
    expect(source).toContain("convertFileSrc(media.preview_token, 'loom-asset')");
    expect(source).toContain('<audio src={previewUrl} controls preload="metadata"');
    expect(source).toContain('media.waveform_peaks');
    expect(source).toContain('<img src={previewUrl} alt={attachment.file_name} />');
    expect(source).not.toContain('URL.createObjectURL');
    expect(source).not.toContain('maxlength={65536}');
    expect(styles).toContain('.context-media-preview audio');
    expect(styles).toContain('.audio-waveform');
    expect(tauri).toContain("img-src 'self' loom-asset: http://loom-asset.localhost data:");
    expect(tauri).toContain("media-src 'self' loom-asset: http://loom-asset.localhost");
  });

  it('keeps the low-noise co-writer popover independent from completion context', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const ipc = readFileSync(new URL('./ipc.ts', import.meta.url), 'utf8');

    expect(source).toContain('aria-label="Choose a co-writer"');
    expect(source).toContain('Reusable completion context');
    expect(source).toContain('contextPaneOpen = true;');
    expect(source).not.toContain('contextPaneOpen = false;\n    coWriterOpen = true');
    expect(ipc).toContain("call('co_writer_list'");
    expect(ipc).toContain("call('co_writer_save'");
    expect(ipc).toContain("call('co_writer_apply'");
    expect(ipc).toContain("call('co_writer_delete'");
  });

  it('routes dictation through native capture and inserts only at the retained editor selection', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const ipc = readFileSync(new URL('./ipc.ts', import.meta.url), 'utf8');

    expect(source).toContain('aria-label={speechRecording');
    expect(source).toContain("return contextPaneOpen ? 'context' : 'manuscript'");
    expect(source).toContain('captureSpeechInsertionAnchor(captured.target)');
    expect(source).toContain('editor?.insertTextAtAnchor(insertion.anchor, snapshot.transcript)');
    expect(source).toContain('sourceEditor?.insertTextAtAnchor(insertion.anchor, snapshot.transcript)');
    expect(source).toContain('Dictation is preserved because its original insertion point changed.');
    expect(source).not.toContain('microphoneCapture');
    expect(ipc).toContain("call('speech_input_record_start'");
    expect(ipc).toContain("call('speech_input_record_stop'");
    expect(ipc).toContain("call('speech_input_record_cancel'");
    expect(ipc).not.toContain('wavBytes');
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
    const visibleActions = source.slice(
      source.indexOf('function handleVisibleDocumentActions'),
      source.indexOf('function beginDocumentContextLongPress')
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
    expect(sidebar).toContain('>Delete Manuscript…</button>');
    expect(sidebar.indexOf('{documentContextRevealLabel}</button>'))
      .toBeLessThan(sidebar.indexOf('>Delete Manuscript…</button>'));
    expect(sidebar).toContain('documentDeleteMenuIndex(Boolean(documentContextRevealLabel))');
    expect(sidebar).toContain('class="document-row-actions"');
    expect(sidebar).toContain('aria-label={`Actions for ${candidate.title}`}');
    expect(sidebar).toContain('on:click={(event) => handleVisibleDocumentActions(event, candidate)}');
    expect(visibleActions).toContain('captureDocumentContextTarget(summary)');
    expect(visibleActions).toContain('openDocumentContextMenu(');
    expect(visibleActions).toContain('visibleDocumentActionsMenuPoint(');
    expect(source).toContain('{#if project.documents.length > 0}');
    expect(source).not.toContain('class:single-document=');
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
    expect(commitRename).toContain('applyDocumentRenameProjection(project, document, renamed)');
    expect(commitRename).not.toContain('title === target.title');
    expect(source).toContain('boundedDocumentTitleInput(input.value)');
    expect(action).toContain("case 'rename':");
    expect(action).toContain("case 'delete':");
    expect(action).toContain('openDocumentDeleteConfirmation(target, trigger)');
    expect(source).toContain('role="alertdialog"');
    expect(source).toContain('aria-modal="true"');
    expect(source).toContain('deleteDocumentCommandId = newUlid()');
    expect(source).toContain('refreshDocumentDeleteTarget(');
    expect(source).toContain('await deleteDocument(');
    expect(source).toContain('documentRefreshDecision(');
  });

  it('treats a deleted visible file as an authoritative outline refresh', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const navigation = source.slice(
      source.indexOf('async function selectCapturedDocument'),
      source.indexOf('function updateText')
    );
    const refresh = source.slice(
      source.indexOf('async function refreshProjectFilesystemState'),
      source.indexOf('function resumeCompletionObservation')
    );
    const missing = source.slice(
      source.indexOf('async function reconcileMissingCurrentDocument'),
      source.indexOf('async function refreshProjectFilesystemState')
    );

    expect(navigation).toContain("normalizeFailure(error).code === 'external_file_deleted'");
    expect(navigation).toContain('scheduleProjectFilesystemRefresh(0)');
    expect(refresh).toContain('documentRefreshDecision(');
    expect(refresh).toContain('applyGuardedProjectFilesystemRefresh(');
    expect(refresh).toContain('captureProjectFilesystemRefreshBoundary(');
    expect(refresh).toContain('await reconcileMissingCurrentDocument(');
    expect(missing).toContain('await settleMissingDocumentRecoveryBoundary(');
    expect(missing).toContain("boundary.kind !== 'ready'");
    expect(missing).toContain('detachDocumentForReconciliation()');
    expect(missing).toContain('await selectDocument(settledDecision.successor, true)');
    expect(refresh).toContain("failure.code === 'external_file_deleted'");
    expect(refresh).toContain('failureIsDefiniteContention(failure)');
    expect(refresh).toContain('scheduleProjectFilesystemRefresh(retryDelayMilliseconds)');
    expect(refresh).not.toContain("recordLocalFailure('filesystem_error'");
  });

  it('keeps normal watcher refreshes editable and locks only the missing-document boundary', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const readonly = source.slice(
      source.indexOf('$: editorReadonly ='),
      source.indexOf('$: reconciliationResolutionLocked')
    );
    const missingBoundary = source.slice(
      source.indexOf('async function reconcileMissingCurrentDocument'),
      source.indexOf('async function refreshProjectFilesystemState')
    );
    const refresh = source.slice(
      source.indexOf('async function refreshProjectFilesystemState'),
      source.indexOf('function resumeCompletionObservation')
    );

    expect(readonly).toContain('missingDocumentBoundaryInFlight');
    expect(readonly).toContain('missingDocumentCapturePending !== null');
    expect(readonly).not.toContain('projectFilesystemRefreshInFlight');
    expect(refresh).toContain('projectFilesystemRefreshInFlight = true');
    expect(missingBoundary).toContain('missingDocumentBoundaryInFlight = true');
    expect(missingBoundary).toContain('finally');
    expect(missingBoundary).toContain('missingDocumentBoundaryInFlight = false');
  });

  it('retains and locks a second missing manuscript until the earlier recovery is copied', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const reconcile = source.slice(
      source.indexOf('async function reconcileMissingCurrentDocument'),
      source.indexOf('async function refreshProjectFilesystemState')
    );
    const refresh = source.slice(
      source.indexOf('async function refreshProjectFilesystemState'),
      source.indexOf('function resumeCompletionObservation')
    );
    const missingBranch = refresh.slice(
      refresh.indexOf('if (decision.currentDisappeared)'),
      refresh.indexOf('const current = decision.current')
    );
    const blockedBranch = reconcile.slice(
      reconcile.indexOf("if (admission.kind === 'wait_for_recovery_copy')"),
      reconcile.indexOf('missingDocumentBoundaryInFlight = true')
    );
    const applicationClose = source.slice(
      source.indexOf('const applicationCloseCoordinator'),
      source.indexOf('const modelDownloadPollBaseMs')
    );
    const projectClose = source.slice(
      source.indexOf('async function performCloseProject'),
      source.indexOf('const closing = project')
    );
    const openProject = source.slice(
      source.indexOf('async function finishOpeningProject'),
      source.indexOf('async function selectDocument')
    );
    const openDocument = source.slice(
      source.indexOf('async function selectCapturedDocument'),
      source.indexOf('function updateText')
    );
    const copy = source.slice(
      source.indexOf('async function copyMissingDocumentRecoveryText'),
      source.indexOf('function clearMissingDocumentCapturePending')
    );

    expect(missingBranch).toContain('await reconcileMissingCurrentDocument(');
    expect(missingBranch).not.toContain('project = refreshed');
    expect(reconcile).toContain('missingDocumentCapturePending = admission.pending');
    expect(blockedBranch).toContain('projectFilesystemRefreshQueued = false');
    expect(blockedBranch).not.toContain('scheduleProjectFilesystemRefresh');
    expect(blockedBranch).not.toContain('project =');
    expect(applicationClose).toContain('if (missingDocumentCapturePending)');
    expect(projectClose).toContain('missingDocumentRecoveryRequiresCopy(');
    expect(projectClose).toContain('if (missingDocumentCapturePending && !retryingPreparedClose)');
    expect(openProject).toContain("'missing_document_recovery_not_durable'");
    expect(openProject).toContain('if (!unsafeRecovery)');
    expect(openDocument).toContain('openedDocumentSubsumesMissingRecovery(');
    expect(openDocument).not.toContain(
      'if (missingDocumentRecovery?.documentId === opened.summary.document_id)'
    );
    expect(copy).toContain('missingDocumentRecovery !== recovery');
    expect(copy).toContain('project.session_id !== recovery.sessionId');
    expect(copy).toContain('scheduleProjectFilesystemRefresh(0)');
  });

  it('keeps an uncertain delete target frozen and retryable across watcher hints', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const refresh = source.slice(
      source.indexOf('async function refreshProjectFilesystemState'),
      source.indexOf('function resumeCompletionObservation')
    );
    const confirmDelete = source.slice(
      source.indexOf('async function confirmDocumentDelete'),
      source.indexOf('function handleDocumentContextMenuKeydown')
    );
    const uncertainGuard = refresh.slice(
      refresh.indexOf('if (deleteDocumentUncertain)'),
      refresh.indexOf('if (reconciliation)')
    );

    expect(uncertainGuard).toContain('projectFilesystemRefreshQueued = false');
    expect(uncertainGuard).toContain('return;');
    expect(uncertainGuard).not.toContain('scheduleProjectFilesystemRefresh');
    expect(refresh.indexOf('if (deleteDocumentUncertain)'))
      .toBeLessThan(refresh.indexOf('const guarded = await applyGuardedProjectFilesystemRefresh('));
    expect(confirmDelete).toContain('const retryingUncertainDelete = deleteDocumentUncertain');
    expect(confirmDelete).toContain('!retryingUncertainDelete && !flushEditors()');
    expect(confirmDelete).toContain('retryingUncertainDelete\n      );');
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
