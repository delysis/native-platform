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

  it('holds trivial ASCII fragments without rejecting compact Unicode writing', () => {
    for (const text of ['.', 'S', 'Hi', '...']) {
      expect(candidateSurfaceDecision(text)).toEqual({
        surface: false,
        reason: 'too_short'
      });
    }
    expect(candidateTextIsSurfaceable('wait')).toBe(true);
    expect(candidateTextIsSurfaceable('…')).toBe(true);
    expect(candidateTextIsSurfaceable('雨')).toBe(true);
  });

  it('suppresses control-only invisible Unicode without rejecting visible Unicode', () => {
    for (const text of ['\u200b', '\u2067', '\u2060', '\0', '\u200b\u2067\0']) {
      expect(candidateSurfaceDecision(text)).toEqual({
        surface: false,
        reason: 'invisible'
      });
    }
    expect(candidateTextIsSurfaceable('\u0301')).toBe(true);
    expect(candidateTextIsSurfaceable('👩‍💻')).toBe(true);
    expect(candidateTextIsSurfaceable('…')).toBe(true);
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

  it('suppresses unbroken periodic loops and generated media markers', () => {
    expect(candidateSurfaceDecision(` ${'Be'.repeat(180)}[image]\n\nS`)).toEqual({
      surface: false,
      reason: 'repetition'
    });
    expect(candidateSurfaceDecision('ha'.repeat(64))).toEqual({
      surface: false,
      reason: 'repetition'
    });
    for (const marker of ['[image]\n\nS', '<image>\r\nS', '<|image_pad|>\nS']) {
      expect(candidateSurfaceDecision(marker)).toEqual({ surface: false, reason: 'artifact' });
    }
    expect(candidateTextIsSurfaceable('She wrote “[image]” beside the sketch.')).toBe(true);
    expect(candidateTextIsSurfaceable('![image](moon.png)')).toBe(true);
    expect(candidateTextIsSurfaceable('[imagination]')).toBe(true);
    expect(candidateTextIsSurfaceable(
      '彼女は雨の音を聞きながら誰も知らない古い名前をゆっくりと思い出した'
    )).toBe(true);
    expect(candidateTextIsSurfaceable('counterrevolutionaries')).toBe(true);
    expect(candidateSurfaceDecision('こだま'.repeat(40))).toEqual({
      surface: false,
      reason: 'repetition'
    });
  });
});
