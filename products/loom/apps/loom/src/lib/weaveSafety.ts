import type { DesktopGenerationEnvelope } from './types';

export interface GenerationScope {
  projectId: string;
  sessionId: string;
  documentId: string;
}

/**
 * Converts a textarea's UTF-16 selection offset into the byte boundary used by
 * the persisted UTF-8 manuscript. A browser caret cannot honestly identify a
 * point inside a surrogate pair, so reject that state instead of guessing.
 */
export function utf8ByteOffset(text: string, utf16Offset: number): number {
  if (!Number.isSafeInteger(utf16Offset) || utf16Offset < 0 || utf16Offset > text.length) {
    throw new RangeError('the editor cursor is outside the manuscript');
  }
  if (
    utf16Offset > 0 &&
    utf16Offset < text.length &&
    isHighSurrogate(text.charCodeAt(utf16Offset - 1)) &&
    isLowSurrogate(text.charCodeAt(utf16Offset))
  ) {
    throw new RangeError('the editor cursor splits a Unicode character');
  }
  return new TextEncoder().encode(text.slice(0, utf16Offset)).byteLength;
}

/** Events are useful only inside the exact live project session and document. */
export function generationEventBelongsToScope(
  envelope: DesktopGenerationEnvelope,
  scope: GenerationScope
): boolean {
  return envelope.project_id === scope.projectId &&
    envelope.session_id === scope.sessionId &&
    envelope.document_id === scope.documentId;
}

function isHighSurrogate(value: number): boolean {
  return value >= 0xd800 && value <= 0xdbff;
}

function isLowSurrogate(value: number): boolean {
  return value >= 0xdc00 && value <= 0xdfff;
}
