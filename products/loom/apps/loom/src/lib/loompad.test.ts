import { describe, expect, it } from 'vitest';
import { emptyLoompadChord, loompadKey, loompadPrefix } from './loompad';

describe('Loompad input authority', () => {
  it('requires Option and accepts one word exactly once per press', () => {
    const initial = emptyLoompadChord();
    expect(loompadKey(initial, 'KeyW', true)).toMatchObject({ handled: false, accept: false });
    let result = loompadKey(initial, 'KeyW', true, false, true);
    expect(result).toMatchObject({ choice: 0, length: 'word', accept: true });
    expect(loompadKey(result.state, 'KeyW', true, true, true).accept).toBe(false);
    result = loompadKey(result.state, 'KeyW', false, false, true);
    expect(loompadKey(result.state, 'KeyW', true, false, true).accept).toBe(true);
    expect(loompadKey(result.state, 'KeyW', true, true, true).accept).toBe(false);
  });
  it('leaves normal writing and old length keys untouched', () => {
    for (const code of ['KeyW', 'KeyA', 'KeyS', 'KeyD', 'KeyI', 'KeyJ', 'KeyK', 'KeyL', 'Space']) {
      expect(loompadKey(emptyLoompadChord(), code, true).handled).toBe(false);
    }
    for (const code of ['KeyI', 'KeyJ', 'KeyK', 'KeyL']) {
      expect(loompadKey(emptyLoompadChord(), code, true, false, true).handled).toBe(false);
    }
    expect(loompadKey(emptyLoompadChord(), 'KeyD', true, false, true)).toMatchObject({ choice: 3, accept: true });
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
