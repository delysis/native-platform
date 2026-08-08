import { describe, expect, it } from 'vitest';
import { writeRebindsStaleDraft } from './draftRecovery';

const stale = {
  document_id: 'document-1',
  source_revision_id: 'revision-old',
  version: '41',
  text: 'recovered\r\ntext'
};

describe('writeRebindsStaleDraft', () => {
  it('recognizes an exact recovered-text write bound to the current revision', () => {
    expect(writeRebindsStaleDraft(stale, {
      documentId: 'document-1',
      sourceRevisionId: 'revision-current',
      expectedVersion: '41',
      text: 'recovered\r\ntext'
    })).toBe(true);
  });

  it('does not retire the stale state for altered bytes or the old source', () => {
    expect(writeRebindsStaleDraft(stale, {
      documentId: 'document-1',
      sourceRevisionId: 'revision-current',
      expectedVersion: '41',
      text: 'recovered\ntext'
    })).toBe(false);
    expect(writeRebindsStaleDraft(stale, {
      documentId: 'document-1',
      sourceRevisionId: 'revision-old',
      expectedVersion: '41',
      text: stale.text
    })).toBe(false);
  });

  it('requires the exact predecessor identity', () => {
    expect(writeRebindsStaleDraft(stale, {
      documentId: 'document-1',
      sourceRevisionId: 'revision-current',
      expectedVersion: '42',
      text: stale.text
    })).toBe(false);
  });
});
