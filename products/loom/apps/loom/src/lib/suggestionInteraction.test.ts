import { describe, expect, it } from 'vitest';
import {
  cycleSuggestionIndex,
  nextSuggestionWord,
  nextVisualSuggestionWord
} from './suggestionInteraction';

describe('suggestion interaction', () => {
  it('accepts one word with its surrounding prose spacing and punctuation', () => {
    expect(nextSuggestionWord(' The observatory breathed again.')).toBe(' The ');
    expect(nextSuggestionWord('observatory breathed again.')).toBe('observatory ');
    expect(nextSuggestionWord('again. Then')).toBe('again. ');
    expect(nextSuggestionWord(' “Listen,” she said.')).toBe(' “Listen,” ');
  });

  it('keeps contractions and hyphenated words together', () => {
    expect(nextSuggestionWord(" don't stop")).toBe(" don't ");
    expect(nextSuggestionWord(' well-made thing')).toBe(' well-made ');
  });

  it('handles live partial words and unicode text', () => {
    expect(nextSuggestionWord(' breat')).toBe(' breat');
    expect(nextSuggestionWord(' 雨が止み、次')).toBe(' 雨が止み、次');
    expect(nextSuggestionWord('   ')).toBeNull();
  });

  it('leaves terminal paragraph whitespace in the visual ghost', () => {
    expect(nextVisualSuggestionWord(' world again')).toBe(' world');
    expect(nextVisualSuggestionWord(' world')).toBe(' world');
    expect(nextVisualSuggestionWord(' “Listen,” she said.')).toBe(' “Listen,”');
  });

  it('cycles in both directions without leaving the family', () => {
    expect(cycleSuggestionIndex(4, 0, 1)).toBe(1);
    expect(cycleSuggestionIndex(4, 0, -1)).toBe(3);
    expect(cycleSuggestionIndex(0, 0, 1)).toBe(-1);
  });
});
