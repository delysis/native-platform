import { describe, expect, it } from 'vitest';
import { completionEngineEnabled, inlineGhostHidden } from './completionModes';

describe('independent completion modes', () => {
  it.each([
    [{ autocomplete: false, shuttle: false }, false, true],
    [{ autocomplete: true, shuttle: false }, true, false],
    [{ autocomplete: false, shuttle: true }, true, true],
    [{ autocomplete: true, shuttle: true }, true, false]
  ] as const)('derives engine and ghost behavior for %o', (modes, engine, hidden) => {
    expect(completionEngineEnabled(modes)).toBe(engine);
    expect(inlineGhostHidden(modes)).toBe(hidden);
  });
});
