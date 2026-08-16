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
    expect(source).toContain('data-tauri-drag-region');
    expect(source).not.toContain('Autosave is always on');
    expect(source).toContain('aria-label={autosaveLabel}');
    expect(source).toContain('New document (⌘N)');
  });

  it('uses the macOS overlay titlebar for one integrated toolbar', () => {
    const config = JSON.parse(readFileSync(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
    const mainWindow = config.app.windows[0];
    expect(mainWindow.titleBarStyle).toBe('Overlay');
    expect(mainWindow.hiddenTitle).toBe(false);
  });

});
