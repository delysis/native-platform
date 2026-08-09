import type { VerseNewlineKind } from './verseCodec';

export interface SourceGhostPresentation {
  active: boolean;
  candidateId: string;
  presentationKey: string;
  text: string;
}

export interface SourceGhostPlan {
  candidateId: string;
  presentationKey: string;
  prefix: string;
  text: string;
  suffix: string;
}

export interface SourceGhostPlanInput {
  presentation: SourceGhostPresentation | null;
  value: string;
  selectionStart: number;
  selectionEnd: number;
  focused: boolean;
  composing: boolean;
  readonly: boolean;
  exactGeometry: boolean;
  ltrContent: boolean;
  verseNewline: VerseNewlineKind | null;
}

export interface SourceMirrorMetrics {
  clientWidth: number;
  clientHeight: number;
  scrollWidth: number;
  scrollHeight: number;
  scrollLeft: number;
  scrollTop: number;
  offsetLeft: number;
  offsetTop: number;
  clientLeft: number;
  clientTop: number;
  wraps: boolean;
}

export interface SourceMirrorGeometry {
  viewportLeft: number;
  viewportTop: number;
  viewportWidth: number;
  viewportHeight: number;
  canvasWidth: number;
  canvasHeight: number;
  translateX: number;
  translateY: number;
}

export interface SourceGhostKey {
  key: string;
  keyCode: number;
  isComposing: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
}

export type SourceGhostKeyAction = 'accept' | 'dismiss' | null;

export function renderedSourceGhostPresentationKey(
  plan: SourceGhostPlan | null,
  viewportHidden: boolean
): string {
  return plan && !viewportHidden ? plan.presentationKey : '';
}

/** O(1): validates that a UTF-16 caret is in range and not inside a surrogate pair. */
export function isUtf16ScalarBoundary(text: string, offset: number): boolean {
  if (!Number.isSafeInteger(offset) || offset < 0 || offset > text.length) return false;
  if (offset === 0 || offset === text.length) return true;
  const before = text.charCodeAt(offset - 1);
  const after = text.charCodeAt(offset);
  return !(
    before >= 0xd800 && before <= 0xdbff &&
    after >= 0xdc00 && after <= 0xdfff
  );
}

/**
 * The textarea mirror currently proves native parity only for LTR runs.
 * Broadly reject strong RTL scripts and explicit RTL controls; false positives
 * are preferable to presenting a misplaced acceptance target.
 */
export function sourceTextHasStrongRtl(text: string): boolean {
  return /[\u0590-\u08ff\u200f\u202b\u202e\u2067\ufb1d-\ufdff\ufe70-\ufeff\u{10800}-\u{10fff}\u{1e800}-\u{1edff}]/u.test(text);
}

export function sourceMirrorDirectionIsSupported(computedDirection: string): boolean {
  return computedDirection === 'ltr';
}

export function sourceGhostTextForTextarea(
  text: string,
  verseNewline: VerseNewlineKind | null
): string | null {
  // A textarea always exposes LF line breaks. Prose is persisted as
  // deterministic LF Markdown, so a CR-bearing completion cannot be shown as
  // byte-faithful source ghost text.
  if (verseNewline === null || verseNewline === 'lf' || verseNewline === 'none') {
    return text.includes('\r') ? null : text;
  }
  if (verseNewline === 'mixed') return null;
  if (verseNewline === 'cr') {
    return text.includes('\n') ? null : text.replace(/\r/g, '\n');
  }

  // Reject lone CR and lone LF. Only a complete CRLF continuation can be
  // projected through the browser's LF-only textarea without ambiguity.
  let display = '';
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index];
    if (character === '\r') {
      if (text[index + 1] !== '\n') return null;
      display += '\n';
      index += 1;
    } else if (character === '\n') {
      return null;
    } else {
      display += character;
    }
  }
  return display;
}

export function planSourceGhostText(input: SourceGhostPlanInput): SourceGhostPlan | null {
  const { presentation } = input;
  if (
    !presentation?.active ||
    !presentation.candidateId ||
    !presentation.presentationKey ||
    !presentation.text ||
    !/\S/u.test(presentation.text) ||
    !input.focused ||
    input.composing ||
    input.readonly ||
    !input.exactGeometry ||
    !input.ltrContent ||
    input.selectionStart !== input.selectionEnd
  ) return null;

  if (!isUtf16ScalarBoundary(input.value, input.selectionStart)) return null;

  const text = sourceGhostTextForTextarea(presentation.text, input.verseNewline);
  if (text === null) return null;
  return {
    candidateId: presentation.candidateId,
    presentationKey: presentation.presentationKey,
    prefix: input.value.slice(0, input.selectionStart),
    text,
    suffix: input.value.slice(input.selectionEnd)
  };
}

export function sourceMirrorGeometry(
  metrics: SourceMirrorMetrics
): SourceMirrorGeometry | null {
  const values = [
    metrics.clientWidth,
    metrics.clientHeight,
    metrics.scrollWidth,
    metrics.scrollHeight,
    metrics.scrollLeft,
    metrics.scrollTop,
    metrics.offsetLeft,
    metrics.offsetTop,
    metrics.clientLeft,
    metrics.clientTop
  ];
  if (
    values.some((value) => !Number.isFinite(value)) ||
    metrics.clientWidth <= 0 ||
    metrics.clientHeight <= 0 ||
    metrics.scrollWidth < 0 ||
    metrics.scrollHeight < 0 ||
    metrics.scrollTop < 0
  ) return null;

  return {
    viewportLeft: metrics.offsetLeft + metrics.clientLeft,
    viewportTop: metrics.offsetTop + metrics.clientTop,
    viewportWidth: metrics.clientWidth,
    viewportHeight: metrics.clientHeight,
    canvasWidth: metrics.wraps
      ? metrics.clientWidth
      : Math.max(metrics.clientWidth, metrics.scrollWidth),
    canvasHeight: Math.max(metrics.clientHeight, metrics.scrollHeight),
    translateX: metrics.scrollLeft === 0 ? 0 : -metrics.scrollLeft,
    translateY: metrics.scrollTop === 0 ? 0 : -metrics.scrollTop
  };
}

export function sourceGhostKeyAction(
  event: SourceGhostKey,
  hasVisibleGhost: boolean
): SourceGhostKeyAction {
  if (
    !hasVisibleGhost ||
    event.isComposing ||
    event.keyCode === 229 ||
    event.metaKey ||
    event.ctrlKey ||
    event.altKey
  ) return null;
  if (event.key === 'Escape') return 'dismiss';
  if (event.key === 'Tab' && !event.shiftKey) return 'accept';
  return null;
}
