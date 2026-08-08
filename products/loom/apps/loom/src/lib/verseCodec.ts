export type VerseNewlineKind = 'none' | 'lf' | 'crlf' | 'cr' | 'mixed';

export interface VerseEditorCodec {
  newline: VerseNewlineKind;
  editable: boolean;
}

export interface DecodedVerse {
  display: string;
  codec: VerseEditorCodec;
}

export function decodeVerseForEditor(raw: string): DecodedVerse {
  const delimiters = raw.match(/\r\n|\r|\n/g) ?? [];
  const kinds = new Set(delimiters);
  let newline: VerseNewlineKind = 'none';
  if (kinds.size > 1) newline = 'mixed';
  else if (kinds.has('\r\n')) newline = 'crlf';
  else if (kinds.has('\r')) newline = 'cr';
  else if (kinds.has('\n')) newline = 'lf';

  return {
    display: raw.replace(/\r\n|\r/g, '\n'),
    codec: { newline, editable: newline !== 'mixed' }
  };
}

export function encodeVerseFromEditor(display: string, codec: VerseEditorCodec): string {
  if (!codec.editable || codec.newline === 'mixed') {
    throw new Error('mixed verse line endings require an explicit normalization decision');
  }
  if (codec.newline === 'crlf') return display.replace(/\n/g, '\r\n');
  if (codec.newline === 'cr') return display.replace(/\n/g, '\r');
  return display;
}
