import { describe, expect, it } from 'vitest';
import {
  advanceCompletionExhaustionLatch,
  acceptedCompletionText,
  completionPresentation,
  completionSessionContextKey,
  completionSessionMatchesPresentation,
  completionShouldRequestNextBatch,
  completionTextAtBoundary,
  consumeCompletionRemainder,
  consumeCompletionWord,
  cycleCompletionSession,
  insertAtUtf8Boundary,
  remainingCompletionText,
  removeBeforeUtf8Boundary,
  startCompletionSession,
  synchronizeCompletionCandidates,
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

  it('grows a visible family before consumption and freezes it afterward', () => {
    const started = startCompletionSession('doc:visual', candidates.slice(0, 1), 'run-a')!;
    const grown = synchronizeCompletionCandidates(started, candidates)!;
    expect(grown.candidates).toHaveLength(2);
    expect(grown.selectedRunId).toBe('run-a');
    expect(synchronizeCompletionCandidates(grown, candidates)).toBe(grown);

    const consumed = consumeCompletionWord(grown)!.session;
    expect(synchronizeCompletionCandidates(consumed, [
      ...candidates,
      { candidateId: 'c', presentationKey: 'c:1', text: ' third', runId: 'run-c', targetByte: 5, insertsOnAccept: true }
    ])).toBe(consumed);
  });

  it('drops a dismissed empty family unless an authorized chunk needs reversal', () => {
    const started = startCompletionSession('doc:visual', candidates, 'run-a')!;
    expect(synchronizeCompletionCandidates(started, [])).toBeNull();

    const consumed = consumeCompletionWord(started)!.session;
    expect(consumed.authorityFrozen).toBe(true);
    expect(synchronizeCompletionCandidates(consumed, [])).toBe(consumed);

    const reversed = unconsumeCompletionWord(consumed)!.session;
    expect(reversed.acceptedChunks).toEqual([]);
    expect(reversed.authorityFrozen).toBe(true);
    expect(synchronizeCompletionCandidates(reversed, [])).toBe(reversed);
    expect(completionPresentation(reversed)).toMatchObject({
      ...candidates[0],
      presentationKey: 'a:1:session:0'
    });
  });

  it('atomically hands exhausted rollback authority to a fresh post-accept family', () => {
    const exhausted = consumeCompletionRemainder(
      startCompletionSession('doc:visual', candidates, 'run-a')!
    )!.session;
    expect(synchronizeCompletionCandidates(exhausted, candidates)).toBe(exhausted);

    const fresh: CompletionCandidate[] = [
      { candidateId: 'next-a', presentationKey: 'next-a:1', text: ' begins', runId: 'next-run-a', targetByte: 13, insertsOnAccept: true },
      { candidateId: 'next-b', presentationKey: 'next-b:1', text: ' continues', runId: 'next-run-b', targetByte: 13, insertsOnAccept: true }
    ];
    const handedOff = synchronizeCompletionCandidates(exhausted, fresh)!;
    expect(handedOff).toMatchObject({
      contextKey: 'doc:visual',
      selectedRunId: 'next-run-a',
      acceptedChunks: [],
      authorityFrozen: false
    });
    expect(handedOff.candidates).toEqual(fresh);
    expect(unconsumeCompletionWord(handedOff)).toBeNull();
  });

  it('re-arms the same exhaustion identity after rollback', () => {
    const first = advanceCompletionExhaustionLatch('', 'context:run:8');
    expect(first).toEqual({ handledKey: 'context:run:8', shouldSchedule: true });
    expect(advanceCompletionExhaustionLatch(first.handledKey, 'context:run:8'))
      .toEqual({ handledKey: 'context:run:8', shouldSchedule: false });
    const rolledBack = advanceCompletionExhaustionLatch(first.handledKey, '');
    expect(rolledBack).toEqual({ handledKey: '', shouldSchedule: false });
    expect(advanceCompletionExhaustionLatch(rolledBack.handledKey, 'context:run:8'))
      .toEqual({ handledKey: 'context:run:8', shouldSchedule: true });
  });

  it('extends an unfrozen stream but ignores late extension and replacement after consumption', () => {
    const started = startCompletionSession('doc', candidates, 'run-a')!;
    const extended = updateCompletionCandidate(started, 'run-a', ' one two three', 'a:2')!;
    expect(remainingCompletionText(extended)).toBe(' one two three');

    const frozen = consumeCompletionWord(started)!.session;
    expect(updateCompletionCandidate(frozen, 'run-a', ' one two three', 'a:2')).toBe(frozen);
    expect(updateCompletionCandidate(frozen, 'run-a', ' replacement', 'a:replacement'))
      .toBe(frozen);
    expect(remainingCompletionText(frozen)).toBe('two');
    expect(completionPresentation(frozen)?.presentationKey).toBe('a:1:session:5');
  });

  it('authorizes reuse only for the exact document and consumed presentation', () => {
    const started = startCompletionSession('session:document-a:1:visual', candidates, 'run-a')!;
    expect(completionSessionMatchesPresentation(started, started.contextKey, candidates[0])).toBe(true);
    expect(completionSessionMatchesPresentation(
      started,
      'session:document-b:2:visual',
      candidates[0]
    )).toBe(false);

    const consumed = consumeCompletionWord(started)!.session;
    const remainder = completionPresentation(consumed)!;
    expect(completionSessionMatchesPresentation(consumed, consumed.contextKey, remainder)).toBe(true);
    expect(completionSessionMatchesPresentation(consumed, consumed.contextKey, candidates[0])).toBe(false);
    expect(completionSessionMatchesPresentation(
      consumed,
      consumed.contextKey,
      { ...remainder, runId: 'run-b' }
    )).toBe(false);
  });

  it('consumes the remainder and edits exact UTF-8 boundaries', () => {
    const started = startCompletionSession('doc', candidates, 'run-a')!;
    const consumed = consumeCompletionRemainder(started)!;
    expect(consumed.text).toBe(' one two');
    expect(completionPresentation(consumed.session)).toMatchObject({
      runId: 'run-a',
      text: '',
      targetByte: 13,
      presentationKey: 'a:1:session:8'
    });
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
