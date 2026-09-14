import { nextSuggestionWord } from './suggestionInteraction';

export const LOOMPAD_KEYS = ['KeyW', 'KeyA', 'KeyS', 'KeyD'] as const;
export const LOOMPAD_LENGTHS = { KeyI: 'word', KeyJ: 'phrase', KeyK: 'sentence', KeyL: 'paragraph' } as const;
export type LoompadLength = typeof LOOMPAD_LENGTHS[keyof typeof LOOMPAD_LENGTHS];
export interface LoompadChord { choices: string[]; lengths: string[] }
export const emptyLoompadChord = (): LoompadChord => ({ choices: [], lengths: [] });

/** Both press orders work. Auto-repeat never promotes text; release/repress is deliberate. */
export function loompadKey(state: LoompadChord, code: string, down: boolean, repeat = false): {
  state: LoompadChord; handled: boolean; choice: number | null; length: LoompadLength | null; accept: boolean;
} {
  const choiceKey = LOOMPAD_KEYS.includes(code as typeof LOOMPAD_KEYS[number]);
  const lengthKey = Object.hasOwn(LOOMPAD_LENGTHS, code);
  if (!choiceKey && !lengthKey) return { state, handled: false, choice: null, length: null, accept: false };
  const group = choiceKey ? 'choices' : 'lengths';
  const held = state[group].includes(code);
  const next = { ...state, [group]: down ? held ? state[group] : [...state[group], code] : state[group].filter(key => key !== code) };
  const choice = LOOMPAD_KEYS.indexOf(next.choices.at(-1) as typeof LOOMPAD_KEYS[number]);
  const length = LOOMPAD_LENGTHS[next.lengths.at(-1) as keyof typeof LOOMPAD_LENGTHS] ?? null;
  return { state: next, handled: true, choice: choice < 0 ? null : choice, length,
    accept: down && !held && !repeat && choice >= 0 && length !== null };
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
