import { describe, expect, it } from 'vitest';
import { decodeVerseForEditor, encodeVerseFromEditor } from './verseCodec';

describe('verse editor codec', () => {
  it.each([
    ['line one\nline two\n', 'lf'],
    ['line one\r\nline two\r\n', 'crlf'],
    ['line one\rline two\r', 'cr'],
    ['one stanza', 'none']
  ] as const)('round-trips uniform exact line endings', (raw, expectedKind) => {
    const decoded = decodeVerseForEditor(raw);
    expect(decoded.codec.newline).toBe(expectedKind);
    expect(encodeVerseFromEditor(decoded.display, decoded.codec)).toBe(raw);
  });

  it('refuses mixed line endings instead of normalizing them', () => {
    const decoded = decodeVerseForEditor('one\r\ntwo\nthree\r');
    expect(decoded.codec).toEqual({ newline: 'mixed', editable: false });
    expect(() => encodeVerseFromEditor(decoded.display, decoded.codec)).toThrow(/explicit normalization/);
  });

  it('uses LF for new boundaries when a one-line poem had no boundary', () => {
    const decoded = decodeVerseForEditor('one');
    expect(encodeVerseFromEditor('one\ntwo', decoded.codec)).toBe('one\ntwo');
  });
});
