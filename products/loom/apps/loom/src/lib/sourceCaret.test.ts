import { describe, expect, it } from 'vitest';
import { decodeVerseForEditor, encodeVerseFromEditor } from './verseCodec';
import { sourceCaretByte } from './sourceCaret';

describe('exact source caret mapping', () => {
  it.each(['\n', '\r\n', '\r'])('maps normalized textarea boundaries back to %j source bytes', (newline) => {
    const raw = `Café${newline}🧵 second${newline}`;
    const { display, codec } = decodeVerseForEditor(raw);
    const caret = display.indexOf(' second');
    expect(sourceCaretByte(display, caret, raw, codec)).toBe(new TextEncoder().encode(`Café${newline}🧵`).length);
    const edited = display.slice(0, caret) + ' new' + display.slice(caret);
    expect(encodeVerseFromEditor(edited, codec)).toBe(`Café${newline}🧵 new second${newline}`);
    const reopened = decodeVerseForEditor(encodeVerseFromEditor(edited, codec));
    expect(sourceCaretByte(reopened.display, caret + 4, encodeVerseFromEditor(edited, codec), reopened.codec))
      .toBe(new TextEncoder().encode(`Café${newline}🧵 new`).length);
  });

  it('rejects stale text after the caret, mixed delimiters, and partial characters', () => {
    const { display, codec } = decodeVerseForEditor('one\r\n🧵 two');
    expect(() => sourceCaretByte(display + 'changed', 1, 'one\r\n🧵 two', codec)).toThrow(/no longer matches/);
    expect(() => sourceCaretByte(display, 5, 'one\r\n🧵 two', codec)).toThrow(/character boundary/);
    const mixed = decodeVerseForEditor('one\r\ntwo\n');
    expect(() => sourceCaretByte(mixed.display, 0, 'one\r\ntwo\n', mixed.codec)).toThrow(/lossless/);
  });
});
