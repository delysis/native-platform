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
    expect(visualFamily).toContain('currentModel');
    expect(visualFamily).toContain('branchPromotionReady');
    expect(source).not.toContain('A private strand is ready');
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
    expect(source).toContain('aria-label={autosaveLabel}');
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
      source.indexOf('function focusableElementsWithin')
    );
    expect(toggle).toContain('setSuggestionsEnabled(!suggestionsEnabled)');
    expect(toggle).not.toContain('openModelManager');
    expect(source).toContain('class="writer-onboarding"');
    expect(source).toContain('Set up private writing suggestions');
  });

  it('makes an empty writing surface visibly writable in both editor modes', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const visual = readFileSync(new URL('./LoomEditor.svelte', import.meta.url), 'utf8');
    const markdown = readFileSync(new URL('./SourceEditor.svelte', import.meta.url), 'utf8');
    const css = readFileSync(new URL('../app.css', import.meta.url), 'utf8');
    expect(visual).toContain("export let placeholder = 'Start writing…'");
    expect(visual).toContain("'aria-placeholder': placeholder");
    expect(markdown).toContain("export let placeholder = 'Start writing…'");
    expect(markdown).toContain('{placeholder}');
    expect(visual).toContain('class="loom-editor-placeholder"');
    expect(css).toContain('.loom-editor-placeholder');
    expect(css).toMatch(/\.loom-editor-placeholder \{[^}]*z-index: 3/);
    expect(css).toContain('.source-pane textarea::placeholder');
    expect(source).toContain('autofocus={true}');
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

  it('disarms completion on caret navigation without scheduling replacement inference', () => {
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
    const visual = readFileSync(new URL('./LoomEditor.svelte', import.meta.url), 'utf8');
    const invalidate = source.slice(
      source.indexOf('function invalidateCompletionForCaretNavigation'),
      source.indexOf('function sessionForEligibleGhost')
    );
    const sourceSelection = source.slice(
      source.indexOf('function updateSourceSelection'),
      source.indexOf('function updateVisualSelection')
    );
    const visualSelection = source.slice(
      source.indexOf('function updateVisualSelection'),
      source.indexOf('function scheduleSourceProjection')
    );

    expect(invalidate).toContain('cancelSuggestionTimer()');
    expect(invalidate).toContain('cancelActiveBranches()');
    expect(invalidate).not.toContain('scheduleAutomaticSuggestions');
    expect(sourceSelection).toContain('invalidateCompletionForCaretNavigation()');
    expect(visualSelection).toContain('invalidateCompletionForCaretNavigation()');
    expect(source).toContain('onCaretNavigation={invalidateCompletionForCaretNavigation}');
    expect(visual).toContain('onCaretNavigation();');
    expect(source).toContain('completionGenerationIsArmed(completionGenerationIntent, completionContextKey, editVersion)');
  });

  it('uses the macOS overlay titlebar for one integrated toolbar', () => {
    const config = JSON.parse(readFileSync(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
    const capability = JSON.parse(readFileSync(new URL('../../src-tauri/capabilities/default.json', import.meta.url), 'utf8'));
    const mainWindow = config.app.windows[0];
    expect(mainWindow.titleBarStyle).toBe('Overlay');
    expect(mainWindow.hiddenTitle).toBe(true);
    expect(capability.permissions).toContain('core:window:allow-start-dragging');
    const source = readFileSync(new URL('../App.svelte', import.meta.url), 'utf8');
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

});
