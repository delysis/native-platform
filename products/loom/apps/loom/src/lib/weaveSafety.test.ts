import { describe, expect, it } from 'vitest';
import type { DesktopGenerationEnvelope } from './types';
import { encodeVerseFromEditor } from './verseCodec';
import { generationEventBelongsToScope, utf8ByteOffset } from './weaveSafety';

describe('utf8ByteOffset', () => {
  it('converts textarea UTF-16 offsets to exact UTF-8 byte boundaries', () => {
    const text = 'Aé🧵e\u0301東';
    expect(utf8ByteOffset(text, 0)).toBe(0);
    expect(utf8ByteOffset(text, 1)).toBe(1);
    expect(utf8ByteOffset(text, 2)).toBe(3);
    expect(utf8ByteOffset(text, 4)).toBe(7);
    expect(utf8ByteOffset(text, 6)).toBe(10);
    expect(utf8ByteOffset(text, text.length)).toBe(13);
  });

  it('rejects out-of-range cursors and offsets inside a surrogate pair', () => {
    expect(() => utf8ByteOffset('🧵', 1)).toThrow(/splits a Unicode character/);
    expect(() => utf8ByteOffset('text', -1)).toThrow(/outside the manuscript/);
    expect(() => utf8ByteOffset('text', 5)).toThrow(/outside the manuscript/);
  });

  it('counts reconstructed verse newlines rather than the normalized textarea bytes', () => {
    const displayPrefix = 'é\n';
    const rawPrefix = encodeVerseFromEditor(displayPrefix, {
      newline: 'crlf',
      editable: true
    });
    expect(rawPrefix).toBe('é\r\n');
    expect(utf8ByteOffset(rawPrefix, rawPrefix.length)).toBe(4);
  });
});

describe('generationEventBelongsToScope', () => {
  const envelope: DesktopGenerationEnvelope = {
    project_id: 'project-a',
    session_id: 'session-new',
    document_id: 'document-a',
    request_id: 'weave-1',
    event: {
      event: 'generation',
      payload: {
        event_id: 'event-1',
        run_id: 'run-1',
        branch_id: 'branch-1',
        sequence: 1,
        kind: { kind: 'text_delta', text: 'possible' },
        occurred_at_ms: 1
      }
    }
  };

  it('accepts only the current project session and document', () => {
    expect(generationEventBelongsToScope(envelope, {
      projectId: 'project-a',
      sessionId: 'session-new',
      documentId: 'document-a'
    })).toBe(true);
    expect(generationEventBelongsToScope(envelope, {
      projectId: 'project-a',
      sessionId: 'session-old',
      documentId: 'document-a'
    })).toBe(false);
    expect(generationEventBelongsToScope(envelope, {
      projectId: 'project-a',
      sessionId: 'session-new',
      documentId: 'document-b'
    })).toBe(false);
    expect(generationEventBelongsToScope(envelope, {
      projectId: 'project-b',
      sessionId: 'session-new',
      documentId: 'document-a'
    })).toBe(false);
  });
});
