import { describe, expect, it } from 'vitest';
import {
  armCompletionGeneration,
  bindCompletionGenerationAnchor,
  completionGenerationIsArmed,
  disarmCompletionGeneration
} from './completionGenerationIntent';

describe('completion generation intent', () => {
  it('binds an explicit trigger to one document, mode, and edit version', () => {
    const intent = armCompletionGeneration('session:document:3:visual', 7, 'document_edit', 42);

    expect(completionGenerationIsArmed(intent, 'session:document:3:visual', 7)).toBe(true);
    expect(intent?.anchorByte).toBe(42);
    expect(completionGenerationIsArmed(intent, 'session:document:3:source', 7)).toBe(false);
    expect(completionGenerationIsArmed(intent, 'session:document:3:visual', 8)).toBe(false);
  });

  it('cannot arm without a concrete editor context', () => {
    expect(armCompletionGeneration('', 0, 'model_ready')).toBeNull();
    expect(armCompletionGeneration('context', -1, 'model_ready')).toBeNull();
  });

  it('stays disarmed after navigation until an allowed event explicitly re-arms it', () => {
    const context = 'session:document:3:visual';
    let intent = armCompletionGeneration(context, 7, 'document_edit');
    expect(completionGenerationIsArmed(intent, context, 7)).toBe(true);

    intent = disarmCompletionGeneration();
    expect(completionGenerationIsArmed(intent, context, 7)).toBe(false);

    intent = armCompletionGeneration(context, 7, 'candidate_exhausted');
    expect(completionGenerationIsArmed(intent, context, 7)).toBe(true);
  });

  it('binds a delayed visual caret once without moving the generation anchor', () => {
    const context = 'session:document:3:visual';
    const pending = armCompletionGeneration(context, 7, 'document_edit');
    const bound = bindCompletionGenerationAnchor(pending, context, 7, 24);

    expect(bound?.anchorByte).toBe(24);
    expect(bindCompletionGenerationAnchor(bound, context, 7, 48)).toBe(bound);
  });
});
