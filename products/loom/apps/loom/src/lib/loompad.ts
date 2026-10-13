import { nextSuggestionWord } from './suggestionInteraction';

export const LOOMPAD_KEYS = ['KeyW', 'KeyA', 'KeyS', 'KeyD'] as const;
export type LoompadLength = 'word';
export interface LoompadChord { choices: string[] }
export const emptyLoompadChord = (): LoompadChord => ({ choices: [] });

/** Choice identity is the next word, not the independently sampled tail. */
export function loompadWordKey(text: string): string | null {
  const prefix = nextSuggestionWord(text);
  if (!prefix) return null;
  const word = prefix.match(/[\p{L}\p{N}\p{M}_]+(?:['’\-][\p{L}\p{N}\p{M}_]+)*/u)?.[0];
  return (word ?? prefix.trim()).normalize('NFC').toLowerCase();
}

/** Only an explicit Option chord promotes text; repeats and ordinary typing do not. */
export function loompadKey(state: LoompadChord, code: string, down: boolean, repeat = false, alt = false): {
  state: LoompadChord; handled: boolean; choice: number | null; length: LoompadLength | null; accept: boolean;
} {
  const index = LOOMPAD_KEYS.indexOf(code as typeof LOOMPAD_KEYS[number]);
  if (index < 0) return { state, handled: false, choice: null, length: null, accept: false };
  const held = state.choices.includes(code);
  const choices = down && alt ? held ? state.choices : [...state.choices, code] : state.choices.filter(key => key !== code);
  return { state: { choices }, handled: alt, choice: alt ? index : null, length: alt ? 'word' : null,
    accept: down && alt && !held && !repeat };
}

/** Prefixes are taken from the visible candidate, never re-tokenized or normalized. */
export function loompadPrefix(text: string, _length: LoompadLength, visual = false): string | null {
  let prefix = nextSuggestionWord(text) ?? '';
  // Visual Markdown does not preserve terminal paragraph whitespace.
  if (visual) prefix = prefix.replace(/\s+$/u, '');
  return /\S/u.test(prefix) ? prefix : null;
}
