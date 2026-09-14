import { isExtendedGraphemeBoundary } from './graphemeBoundary';
import { encodeVerseFromEditor, type VerseEditorCodec } from './verseCodec';
import { utf8ByteOffset } from './weaveSafety';

/** Translate a browser LF/UTF-16 caret only after proving the whole source identity. */
export function sourceCaretByte(display: string, offset: number, manuscript: string, codec: VerseEditorCodec | null): number {
  if (!codec?.editable) throw new Error('This document does not expose a lossless source boundary.');
  if (!Number.isInteger(offset) || offset < 0 || offset > display.length || !isExtendedGraphemeBoundary(display, offset)) {
    throw new Error('The source caret is not a complete character boundary.');
  }
  if (encodeVerseFromEditor(display, codec) !== manuscript) {
    throw new Error('The source editor no longer matches the saved manuscript bytes.');
  }
  const prefix = encodeVerseFromEditor(display.slice(0, offset), codec);
  return utf8ByteOffset(prefix, prefix.length);
}
