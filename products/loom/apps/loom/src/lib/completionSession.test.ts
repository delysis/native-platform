import { describe, expect, it } from 'vitest';
import {
  acceptedCompletionText,
  completionPresentation,
  completionSessionContextKey,
  completionShouldRequestNextBatch,
  completionTextAtBoundary,
  consumeCompletionRemainder,
  consumeCompletionWord,
  cycleCompletionSession,
  insertAtUtf8Boundary,
  remainingCompletionText,
  removeBeforeUtf8Boundary,
  startCompletionSession,
  unconsumeCompletionWord,
  updateCompletionCandidate,
  type CompletionCandidate
} from './completionSession';

const candidates: CompletionCandidate[] = [
  { candidateId: 'a', presentationKey: 'a:1', text: ' one two', runId: 'run-a', targetByte: 5, insertsOnAccept: true },
  { candidateId: 'b', presentationKey: 'b:1', text: ' another path', runId: 'run-b', targetByte: 5, insertsOnAccept: true }
];

describe('cached completion session', () => {
  it('uses document lifetime identity rather than autosave revision identity', () => {
    expect(completionSessionContextKey('session', 'document', 7, 'visual'))
      .toBe('session:document:7:visual');
    expect(completionSessionContextKey('session', 'document', 7, 'visual'))
      .toBe(completionSessionContextKey('session', 'document', 7, 'visual'));
    expect(completionSessionContextKey('', 'document', 7, 'visual')).toBe('');
  });

  it('consumes and reverses words without requesting a new candidate', () => {
    const started = startCompletionSession('doc:visual', candidates, 'run-a');
    expect(started).not.toBeNull();
    const first = consumeCompletionWord(started!);
    expect(completionShouldRequestNextBatch(first!.session, false, true)).toBe(false);
    expect(first?.text).toBe(' one ');
    expect(acceptedCompletionText(first!.session)).toBe(' one ');
    expect(remainingCompletionText(first!.session)).toBe('two');
    expect(completionPresentation(first!.session)).toMatchObject({
      text: 'two',
      targetByte: 10,
      insertsOnAccept: true
    });
    const reversed = unconsumeCompletionWord(first!.session);
    expect(reversed?.text).toBe(' one ');
    expect(reversed?.session.acceptedChunks).toEqual([]);
    expect(remainingCompletionText(reversed!.session)).toBe(' one two');
  });

  it('locks cycling after consumption and restores it after reversal', () => {
    const started = startCompletionSession('doc:source', candidates, 'run-a')!;
    const cycled = cycleCompletionSession(started, 1);
    expect(cycled.selectedRunId).toBe('run-b');
    const consumed = consumeCompletionWord(cycled)!.session;
    expect(cycleCompletionSession(consumed, 1)).toBe(consumed);
    const reversed = unconsumeCompletionWord(consumed)!.session;
    expect(cycleCompletionSession(reversed, 1).selectedRunId).toBe('run-a');
  });

  it('extends the selected cached stream and rejects a changed prefix', () => {
    const consumed = consumeCompletionWord(startCompletionSession('doc', candidates, 'run-a')!)!.session;
    const extended = updateCompletionCandidate(consumed, 'run-a', ' one two three', 'a:2');
    expect(remainingCompletionText(extended!)).toBe('two three');
    expect(updateCompletionCandidate(consumed, 'run-a', ' wrong', 'a:bad')).toBeNull();
  });

  it('consumes the remainder and edits exact UTF-8 boundaries', () => {
    const started = startCompletionSession('doc', candidates, 'run-a')!;
    const consumed = consumeCompletionRemainder(started)!;
    expect(consumed.text).toBe(' one two');
    expect(completionShouldRequestNextBatch(consumed.session, true, true)).toBe(false);
    expect(completionShouldRequestNextBatch(consumed.session, false, false)).toBe(false);
    expect(completionShouldRequestNextBatch(consumed.session, false, true)).toBe(true);
    expect(insertAtUtf8Boundary('héllo', 3, ' brave')).toBe('hé bravello');
    expect(removeBeforeUtf8Boundary('hé bravello', 9, ' brave')).toBe('héllo');
    expect(insertAtUtf8Boundary('héllo', 2, 'x')).toBeNull();
  });

  it('adds only the editor-owned separator required by the insertion boundary', () => {
    expect(completionTextAtBoundary('I am trying', 11, 'to continue')).toBe(' to continue');
    expect(completionTextAtBoundary('I am trying.', 12, 'Then')).toBe(' Then');
    expect(completionTextAtBoundary('I am trying ', 12, 'again')).toBe('again');
    expect(completionTextAtBoundary('Wait', 4, ', please')).toBe(', please');
    expect(completionTextAtBoundary('hé', 2, 'x')).toBeNull();
  });
});
