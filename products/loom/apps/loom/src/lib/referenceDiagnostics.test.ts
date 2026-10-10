import { afterEach, describe, expect, it, vi } from 'vitest';
import { ReferenceDiagnostics, decodeReferenceDiagnostics, visualReferenceDecorations } from './referenceDiagnostics';
import { parseVisualMarkdown, serializeVisualMarkdown } from './markdownSafety';

afterEach(() => vi.useRealTimers());
describe('reference diagnostics', () => {
  it('converts exact UTF-8 ranges and refuses split characters or overlapping results', () => {
    const text = '🌒 @失踪';
    expect(decodeReferenceDiagnostics(text, [{ start: 5, end: 12, message: 'Missing' }])).toEqual([{ start: 3, end: 6, message: 'Missing' }]);
    expect(decodeReferenceDiagnostics(text, [{ start: 1, end: 12, message: 'Missing' }])).toEqual([]);
    expect(decodeReferenceDiagnostics(text, [{ start: 5, end: 12, message: 'Missing' }, { start: 5, end: 12, message: 'Again' }])).toEqual([]);
  });

  it('discards a delayed result after text or session changes and after disposal', async () => {
    vi.useFakeTimers();
    let finish!: (items: { start: number; end: number; message: string }[]) => void;
    const read = vi.fn(() => new Promise<{ start: number; end: number; message: string }[]>(resolve => { finish = resolve; }));
    const publish = vi.fn();
    const controller = new ReferenceDiagnostics(read);
    const scope = { projectId: 'p', sessionId: 'one', revision: 'r' };
    controller.update(scope, '@Lost', publish);
    await vi.advanceTimersByTimeAsync(650);
    controller.update({ ...scope, sessionId: 'two' }, '@Other', publish);
    finish([{ start: 0, end: 5, message: 'Wrong session' }]);
    await Promise.resolve();
    expect(publish.mock.calls.every(([items]) => items.length === 0)).toBe(true);
    await vi.advanceTimersByTimeAsync(650);
    controller.dispose();
    finish([{ start: 0, end: 6, message: 'Disposed' }]);
    await Promise.resolve();
    expect(publish.mock.calls.every(([items]) => items.length === 0)).toBe(true);
  });

  it('maps plain, formatted and retained-link references without altering the document', () => {
    for (const token of ['@Lost', '@"A **missing** name"', '[@Lost](loom-material:missing)']) {
      const text = `🌒 Before ${token} after.`;
      const doc = parseVisualMarkdown(text);
      const from = text.indexOf(token);
      const decorations = visualReferenceDecorations(doc, text, [{ start: from, end: from + token.length, message: 'Missing source' }]).find();
      expect(decorations).toHaveLength(1);
      expect(doc.textBetween(decorations[0].from, decorations[0].to)).toBe(token === '@Lost' ? token : token.includes('**') ? '@"A missing name"' : '@Lost');
      expect(serializeVisualMarkdown(doc)).toBe(text);
    }
  });
});
