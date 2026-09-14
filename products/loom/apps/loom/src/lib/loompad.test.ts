import { describe, expect, it } from 'vitest';
import { emptyLoompadChord, loompadKey, loompadPrefix } from './loompad';

describe('Loompad input authority', () => {
  it('accepts only complete chords in either press order, never auto-repeat', () => {
    for (const keys of [['KeyW','KeyI'], ['KeyI','KeyW']]) {
      let result = loompadKey(emptyLoompadChord(), keys[0], true);
      expect(result.accept).toBe(false);
      result = loompadKey(result.state, keys[1], true);
      expect(result).toMatchObject({ accept: true, choice: 0, length: 'word' });
      expect(loompadKey(result.state, keys[1], true, true).accept).toBe(false);
      result = loompadKey(result.state, keys[1], false);
      expect(result.accept).toBe(false);
      expect(loompadKey(result.state, keys[1], true).accept).toBe(true);
    }
  });
  it('supports rolling choices and leaves ordinary keys alone', () => {
    let state = loompadKey(emptyLoompadChord(), 'KeyL', true).state;
    const a = loompadKey(state, 'KeyA', true);
    expect(a).toMatchObject({ choice: 1, length: 'paragraph', accept: true });
    const d = loompadKey(a.state, 'KeyD', true);
    expect(d).toMatchObject({ choice: 3, accept: true });
    state = loompadKey(d.state, 'KeyD', false).state;
    expect(state.choices).toEqual(['KeyA']);
    expect(loompadKey(state, 'KeyQ', true).handled).toBe(false);
  });
  it('preserves exact Unicode and CRLF prefixes at each length', () => {
    const text = ' 👩🏽‍🚀 Hello, café! Next sentence.\r\n\r\nSecond paragraph.';
    for (const length of ['word','phrase','sentence','paragraph'] as const) {
      const prefix = loompadPrefix(text, length)!;
      expect(text.startsWith(prefix)).toBe(true);
      expect(prefix).not.toMatch(/[\ud800-\udbff]$/u);
    }
    expect(loompadPrefix(' One two three four five', 'phrase')).toBe(' One two three four ');
    expect(loompadPrefix(' One two three four five', 'phrase', true)).toBe(' One two three four');
    expect(loompadPrefix(' First. Second.', 'sentence')).toBe(' First. ');
    expect(loompadPrefix(' First\r\n\r\nSecond', 'paragraph')).toBe(' First');
    expect(loompadPrefix('  ', 'word')).toBeNull();
  });
});
