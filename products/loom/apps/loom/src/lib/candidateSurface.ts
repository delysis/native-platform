export type CandidateSurfaceDecision =
  | { surface: true }
  | { surface: false; reason: 'artifact' | 'empty' | 'invisible' | 'numeric' | 'repetition' | 'too_short' };

const wordPattern = /[\p{L}\p{N}]+(?:['’][\p{L}\p{N}]+)*/gu;
const letterPattern = /\p{L}/u;
const digitPattern = /\p{N}/u;
const visibleScalarPattern = /[^\p{C}\p{Z}\p{Default_Ignorable_Code_Point}]/u;
const generatedMediaMarkerPattern = /^[\p{Zs}\t\r]*(?:\[image\]|<image>|<\|image(?:_pad)?\|>|<start_of_image>|<end_of_image>)[\p{Zs}\t\r]*$/imu;
const unbrokenWordPattern = /[^\s\p{P}\p{S}]{64,}/gu;
const maxScannedCodeUnits = 8_192;
const maxScannedRunCodePoints = 512;
const maxPeriodCodePoints = 16;
const minimumPeriodRepeats = 12;
const minimumPeriodAgreement = 0.95;

function hasDegenerateUnbrokenPeriod(text: string): boolean {
  const runs = text.slice(0, maxScannedCodeUnits).match(unbrokenWordPattern) ?? [];
  for (const run of runs) {
    const codePoints = Array.from(run).slice(0, maxScannedRunCodePoints);
    const maximumPeriod = Math.min(
      maxPeriodCodePoints,
      Math.floor(codePoints.length / minimumPeriodRepeats)
    );
    for (let period = 1; period <= maximumPeriod; period += 1) {
      let matching = 0;
      for (let index = period; index < codePoints.length; index += 1) {
        if (codePoints[index] === codePoints[index - period]) matching += 1;
      }
      if (matching / (codePoints.length - period) >= minimumPeriodAgreement) return true;
    }
  }
  return false;
}

/**
 * A deliberately conservative presentation gate for obviously broken model
 * continuations. It never deletes or rewrites the immutable candidate.
 */
export function candidateSurfaceDecision(text: string): CandidateSurfaceDecision {
  if (!/\S/u.test(text)) return { surface: false, reason: 'empty' };
  if (!visibleScalarPattern.test(text)) return { surface: false, reason: 'invisible' };
  if (generatedMediaMarkerPattern.test(text.slice(0, maxScannedCodeUnits))) {
    return { surface: false, reason: 'artifact' };
  }
  const compactAscii = text.trim();
  if (
    /^[\x00-\x7f]*$/u.test(compactAscii) &&
    Array.from(compactAscii).filter((scalar) => /\S/u.test(scalar)).length < 4
  ) return { surface: false, reason: 'too_short' };
  if (hasDegenerateUnbrokenPeriod(text)) {
    return { surface: false, reason: 'repetition' };
  }

  const tokens = (text.slice(0, maxScannedCodeUnits).match(wordPattern) ?? [])
    .slice(0, 512)
    .map((token) => token.toLowerCase());
  if (tokens.length === 0) return { surface: true };

  const numericTokens = tokens.filter((token) => digitPattern.test(token)).length;
  const letterTokens = tokens.filter((token) => letterPattern.test(token)).length;
  if (
    tokens.length >= 8 &&
    letterTokens === 0 &&
    numericTokens / tokens.length >= 0.8
  ) return { surface: false, reason: 'numeric' };

  let repeatedRun = 1;
  let longestRepeatedRun = 1;
  for (let index = 1; index < tokens.length; index += 1) {
    repeatedRun = tokens[index] === tokens[index - 1] ? repeatedRun + 1 : 1;
    longestRepeatedRun = Math.max(longestRepeatedRun, repeatedRun);
  }
  if (longestRepeatedRun >= 6) return { surface: false, reason: 'repetition' };

  if (tokens.length >= 20) {
    const counts = new Map<string, number>();
    for (const token of tokens) counts.set(token, (counts.get(token) ?? 0) + 1);
    const dominantCount = Math.max(...counts.values());
    if (dominantCount / tokens.length >= 0.6) {
      return { surface: false, reason: 'repetition' };
    }
  }

  if (tokens.length >= 12) {
    const minimumLoopCoverage = Math.ceil(tokens.length * 0.6);
    const maximumWindow = Math.min(8, Math.floor(tokens.length / 3));
    for (let width = 2; width <= maximumWindow; width += 1) {
      for (let offset = 0; offset < width; offset += 1) {
        let repeatedWindows = 1;
        for (let start = offset + width; start + width <= tokens.length; start += width) {
          let equal = true;
          for (let index = 0; index < width; index += 1) {
            if (tokens[start + index] !== tokens[start - width + index]) {
              equal = false;
              break;
            }
          }
          repeatedWindows = equal ? repeatedWindows + 1 : 1;
          if (repeatedWindows >= 3 && repeatedWindows * width >= minimumLoopCoverage) {
            return { surface: false, reason: 'repetition' };
          }
        }
      }
    }
  }

  return { surface: true };
}

export function candidateTextIsSurfaceable(text: string): boolean {
  return candidateSurfaceDecision(text).surface;
}

export function candidateSurfaceReason(text: string): string | null {
  const decision = candidateSurfaceDecision(text);
  return decision.surface ? null : decision.reason;
}
