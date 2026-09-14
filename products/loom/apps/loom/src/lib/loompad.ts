import { nextSuggestionWord } from './suggestionInteraction';

export const LOOMPAD_KEYS = ['KeyW', 'KeyA', 'KeyS', 'KeyD'] as const;
export type LoompadLength = 'word' | 'phrase' | 'sentence' | 'paragraph';
export interface LoompadChord { choices: string[] }
export const emptyLoompadChord = (): LoompadChord => ({ choices: [] });

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
export function loompadPrefix(text: string, length: LoompadLength, visual = false): string | null {
  let prefix = '';
  if (length === 'word' || length === 'phrase') {
    let remainder = text;
    for (let count = 0; count < (length === 'word' ? 1 : 4); count++) {
      const word = nextSuggestionWord(remainder);
      if (!word) break;
      prefix += word;
      remainder = remainder.slice(word.length);
    }
  } else if (length === 'sentence') {
    const segmenter = new Intl.Segmenter(undefined, { granularity: 'sentence' });
    prefix = [...segmenter.segment(text)][0]?.segment ?? text;
  } else {
    const boundary = text.search(/\r?\n[\t ]*\r?\n/u);
    prefix = boundary < 0 ? text : text.slice(0, boundary);
  }
  // Visual Markdown does not preserve terminal paragraph whitespace.
  if (visual) prefix = prefix.replace(/\s+$/u, '');
  return /\S/u.test(prefix) ? prefix : null;
}
