import { describe, expect, it } from 'vitest';
import {
  completionEngineBecameDisabled,
  completionEngineBecameEnabled,
  completionEngineEnabled,
  inlineGhostHidden
} from './completionModes';

describe('independent completion modes', () => {
  it.each([
    [{ autocomplete: false, shuttle: false }, false, true],
    [{ autocomplete: true, shuttle: false }, true, false],
    [{ autocomplete: false, shuttle: true }, true, true],
    [{ autocomplete: true, shuttle: true }, true, true]
  ] as const)('derives engine and ghost behavior for %o', (modes, engine, hidden) => {
    expect(completionEngineEnabled(modes)).toBe(engine);
    expect(inlineGhostHidden(modes)).toBe(hidden);
  });

  it('mints work only when the shared engine crosses from off to on', () => {
    expect(completionEngineBecameEnabled(
      { autocomplete: false, shuttle: false },
      { autocomplete: true, shuttle: false }
    )).toBe(true);
    expect(completionEngineBecameEnabled(
      { autocomplete: true, shuttle: false },
      { autocomplete: true, shuttle: true }
    )).toBe(false);
    expect(completionEngineBecameEnabled(
      { autocomplete: false, shuttle: true },
      { autocomplete: true, shuttle: true }
    )).toBe(false);
    expect(completionEngineBecameEnabled(
      { autocomplete: true, shuttle: true },
      { autocomplete: false, shuttle: true }
    )).toBe(false);
  });

  it('clears cached work only when the shared engine crosses from on to off', () => {
    expect(completionEngineBecameDisabled(
      { autocomplete: true, shuttle: false },
      { autocomplete: false, shuttle: false }
    )).toBe(true);
    expect(completionEngineBecameDisabled(
      { autocomplete: false, shuttle: true },
      { autocomplete: false, shuttle: false }
    )).toBe(true);
    expect(completionEngineBecameDisabled(
      { autocomplete: true, shuttle: false },
      { autocomplete: false, shuttle: true }
    )).toBe(false);
    expect(completionEngineBecameDisabled(
      { autocomplete: true, shuttle: true },
      { autocomplete: false, shuttle: true }
    )).toBe(false);
    expect(completionEngineBecameDisabled(
      { autocomplete: true, shuttle: true },
      { autocomplete: true, shuttle: false }
    )).toBe(false);
  });
});
