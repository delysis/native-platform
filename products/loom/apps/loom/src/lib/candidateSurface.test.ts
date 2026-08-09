import { describe, expect, it } from 'vitest';
import { candidateSurfaceDecision, candidateTextIsSurfaceable } from './candidateSurface';

describe('candidateSurfaceDecision', () => {
  it('keeps ordinary prose and intentional compact repetition', () => {
    expect(candidateTextIsSurfaceable(' She listened until the rain moved east.')).toBe(true);
    expect(candidateTextIsSurfaceable(' Never, never, never again.')).toBe(true);
  });

  it('suppresses empty, numeric, and adjacent-token degeneration', () => {
    expect(candidateSurfaceDecision('   ')).toEqual({ surface: false, reason: 'empty' });
    expect(candidateSurfaceDecision('1 1 1 1 1 1 1 1 1 1')).toEqual({
      surface: false,
      reason: 'numeric'
    });
    expect(candidateSurfaceDecision(`She put ${'her '.repeat(24)}`)).toEqual({
      surface: false,
      reason: 'repetition'
    });
  });

  it('suppresses a long dominant-token loop without rejecting short poems', () => {
    expect(candidateSurfaceDecision(
      `A door ${'opened '.repeat(18)}under the rain and night.`
    )).toEqual({ surface: false, reason: 'repetition' });
    expect(candidateTextIsSurfaceable('one\none\none\n\nand then two')).toBe(true);
  });

  it('suppresses repeated phrase walls while keeping Unicode prose', () => {
    expect(candidateSurfaceDecision('The door opened. '.repeat(8))).toEqual({
      surface: false,
      reason: 'repetition'
    });
    expect(candidateTextIsSurfaceable('雨が止み、彼女は静かな戸口で耳を澄ませた。')).toBe(true);
  });
});
