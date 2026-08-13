import { describe, expect, it } from 'vitest';
import {
  isUtf16ScalarBoundary,
  planSourceGhostText,
  renderedSourceGhostPresentationKey,
  sourceGhostAnchorMatches,
  sourceGhostKeyAction,
  sourceGhostPresentationCompatible,
  sourceGhostRectIntersectsViewport,
  sourceGhostVisibilityWitnessMatches,
  sourceTabEdit,
  sourceTextHasStrongRtl,
  sourceGhostTextForTextarea,
  sourceMirrorDirectionIsSupported,
  sourceMirrorGeometry
} from './sourceGhostText';

const presentation = {
  active: true,
  candidateId: 'candidate-1',
  presentationKey: 'candidate-1:blob-1',
  text: ' rain\nthen light'
};

function plan(overrides: Partial<Parameters<typeof planSourceGhostText>[0]> = {}) {
  return planSourceGhostText({
    presentation,
    value: 'A 🧵 waits.',
    selectionStart: 4,
    selectionEnd: 4,
    focused: true,
    composing: false,
    readonly: false,
    exactGeometry: true,
    ltrContent: true,
    verseNewline: null,
    ...overrides
  });
}

describe('planSourceGhostText', () => {
  it('projects an insertion without changing the textarea value', () => {
    const value = 'A 🧵 waits.';
    expect(plan({ value })).toEqual({
      candidateId: 'candidate-1',
      presentationKey: 'candidate-1:blob-1',
      prefix: 'A 🧵',
      text: ' rain\nthen light',
      suffix: ' waits.'
    });
    expect(value).toBe('A 🧵 waits.');
  });

  it('fails closed for blur, IME, selection, readonly, uncertain geometry, and split Unicode', () => {
    expect(plan({ focused: false })).toBeNull();
    expect(plan({ composing: true })).toBeNull();
    expect(plan({ selectionEnd: 5 })).toBeNull();
    expect(plan({ readonly: true })).toBeNull();
    expect(plan({ exactGeometry: false })).toBeNull();
    expect(plan({ value: '🧵', selectionStart: 1, selectionEnd: 1 })).toBeNull();
  });

  it('fails closed inside extended graphemes and when candidate edges join them', () => {
    expect(plan({
      value: 'e\u0301 waits',
      selectionStart: 1,
      selectionEnd: 1
    })).toBeNull();
    expect(plan({
      value: '👩‍👩 waits',
      selectionStart: 2,
      selectionEnd: 2
    })).toBeNull();
    expect(plan({
      value: 'e',
      selectionStart: 1,
      selectionEnd: 1,
      presentation: { ...presentation, text: '\u0301 morning' }
    })).toBeNull();
    expect(plan({
      value: '👩 waits',
      selectionStart: 0,
      selectionEnd: 0,
      presentation: { ...presentation, text: '👩‍' }
    })).toBeNull();
    expect(plan({
      value: '🇳 waits',
      selectionStart: 0,
      selectionEnd: 0,
      presentation: { ...presentation, text: '🇺' }
    })).toBeNull();
  });

  it('explicitly fails closed for strong RTL text until the mirror proves native parity', () => {
    expect(plan({
      value: 'שלום', selectionStart: 4, selectionEnd: 4, ltrContent: false
    })).toBeNull();
    expect(plan({
      presentation: { ...presentation, text: ' مرحبا' }, ltrContent: false
    })).toBeNull();
    expect(plan({ value: '東🧵', selectionStart: 3, selectionEnd: 3 })).not.toBeNull();
  });

  it('normalizes only provably uniform verse newline encodings', () => {
    expect(plan({
      presentation: { ...presentation, text: ' rain\r\nthen light' },
      verseNewline: 'crlf'
    })?.text).toBe(' rain\nthen light');
    expect(plan({
      presentation: { ...presentation, text: ' rain\rthen light' },
      verseNewline: 'cr'
    })?.text).toBe(' rain\nthen light');
    expect(plan({
      presentation: { ...presentation, text: ' rain\nthen light' },
      verseNewline: 'crlf'
    })).toBeNull();
    expect(plan({ verseNewline: 'mixed' })).toBeNull();
    expect(plan({
      presentation: { ...presentation, text: ' rain\r\nthen light' },
      verseNewline: null
    })).toBeNull();
  });
});

describe('constant-time source caret guards', () => {
  it('checks only the UTF-16 scalar boundary contract', () => {
    expect(isUtf16ScalarBoundary('A🧵Z', 1)).toBe(true);
    expect(isUtf16ScalarBoundary('A🧵Z', 2)).toBe(false);
    expect(isUtf16ScalarBoundary('A🧵Z', 3)).toBe(true);
    expect(isUtf16ScalarBoundary('A🧵Z', -1)).toBe(false);
  });

  it('identifies strong RTL scripts and controls without rejecting CJK or emoji', () => {
    expect(sourceTextHasStrongRtl('עברית')).toBe(true);
    expect(sourceTextHasStrongRtl('\u2067isolated')).toBe(true);
    expect(sourceTextHasStrongRtl('東🧵é')).toBe(false);
  });

  it('explicitly rejects non-LTR computed textarea direction', () => {
    expect(sourceMirrorDirectionIsSupported('ltr')).toBe(true);
    expect(sourceMirrorDirectionIsSupported('rtl')).toBe(false);
    expect(sourceMirrorDirectionIsSupported('auto')).toBe(false);
  });
});

describe('sourceGhostTextForTextarea', () => {
  it('keeps exact Unicode and tabs while projecting only the declared newline encoding', () => {
    expect(sourceGhostTextForTextarea('\t東🧵\r\nnext', 'crlf')).toBe('\t東🧵\nnext');
    expect(sourceGhostTextForTextarea('\t東🧵\rnext', 'crlf')).toBeNull();
  });

  it('shares the complete newline and RTL presentation contract with scheduling', () => {
    expect(sourceGhostPresentationCompatible('A waits', ' then light', null)).toBe(true);
    expect(sourceGhostPresentationCompatible('עברית', ' then light', null)).toBe(false);
    expect(sourceGhostPresentationCompatible('A waits', ' עברית', null)).toBe(false);
    expect(sourceGhostPresentationCompatible('A waits', ' then\rbroken', null)).toBe(false);
  });
});

describe('sourceMirrorGeometry', () => {
  it('tracks textarea border origin, wrapping width, and both scroll axes', () => {
    expect(sourceMirrorGeometry({
      clientWidth: 500,
      clientHeight: 320,
      scrollWidth: 500,
      scrollHeight: 900,
      scrollLeft: 0,
      scrollTop: 140,
      offsetLeft: 12,
      offsetTop: 8,
      clientLeft: 1,
      clientTop: 2,
      wraps: true
    })).toEqual({
      viewportLeft: 13,
      viewportTop: 10,
      viewportWidth: 500,
      viewportHeight: 320,
      canvasWidth: 500,
      canvasHeight: 900,
      translateX: 0,
      translateY: -140
    });
  });

  it('preserves the full horizontal canvas for exact no-wrap verse', () => {
    expect(sourceMirrorGeometry({
      clientWidth: 480,
      clientHeight: 300,
      scrollWidth: 1_240,
      scrollHeight: 300,
      scrollLeft: 275,
      scrollTop: 0,
      offsetLeft: 0,
      offsetTop: 0,
      clientLeft: 0,
      clientTop: 0,
      wraps: false
    })?.canvasWidth).toBe(1_240);
  });

  it('keeps signed browser scroll coordinates separate from LTR eligibility', () => {
    expect(sourceMirrorGeometry({
      clientWidth: 480,
      clientHeight: 300,
      scrollWidth: 900,
      scrollHeight: 300,
      scrollLeft: -120,
      scrollTop: 0,
      offsetLeft: 0,
      offsetTop: 0,
      clientLeft: 0,
      clientTop: 0,
      wraps: false
    })?.translateX).toBe(120);
  });

  it('refuses invalid or zero-sized layout measurements', () => {
    expect(sourceMirrorGeometry({
      clientWidth: 0,
      clientHeight: 300,
      scrollWidth: 0,
      scrollHeight: 300,
      scrollLeft: 0,
      scrollTop: 0,
      offsetLeft: 0,
      offsetTop: 0,
      clientLeft: 0,
      clientTop: 0,
      wraps: true
    })).toBeNull();
  });
});

describe('sourceGhostKeyAction', () => {
  const key = (overrides: Partial<Parameters<typeof sourceGhostKeyAction>[0]> = {}) => ({
    key: 'Tab',
    keyCode: 9,
    isComposing: false,
    shiftKey: false,
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    ...overrides
  });

  it('accepts a visible ghost and otherwise inserts an ordinary tab', () => {
    expect(sourceGhostKeyAction(key(), true)).toBe('accept');
    expect(sourceGhostKeyAction(key({ shiftKey: true }), true)).toBeNull();
    expect(sourceGhostKeyAction(key({ metaKey: true }), true)).toBeNull();
    expect(sourceGhostKeyAction(key(), false)).toBe('insert_tab');
  });

  it('dismisses with Escape and ignores IME key events', () => {
    expect(sourceGhostKeyAction(key({ key: 'Escape', keyCode: 27 }), true)).toBe('dismiss');
    expect(sourceGhostKeyAction(key({ isComposing: true }), true)).toBeNull();
    expect(sourceGhostKeyAction(key({ keyCode: 229 }), true)).toBeNull();
  });
});

describe('sourceTabEdit', () => {
  it('inserts a literal tab at the caret and replaces selections', () => {
    expect(sourceTabEdit('beforeafter', 6, 6)).toEqual({
      value: 'before\tafter',
      caret: 7
    });
    expect(sourceTabEdit('before selected after', 7, 15)).toEqual({
      value: 'before \t after',
      caret: 8
    });
  });

  it('uses textarea UTF-16 offsets without splitting adjacent emoji', () => {
    expect(sourceTabEdit('A 🧵 waits', 4, 4)).toEqual({
      value: 'A 🧵\t waits',
      caret: 5
    });
    expect(sourceTabEdit('text', -1, 2)).toBeNull();
    expect(sourceTabEdit('text', 3, 2)).toBeNull();
    expect(sourceTabEdit('text', 0, 5)).toBeNull();
  });
});

describe('renderedSourceGhostPresentationKey', () => {
  it('reports eligibility only after the overlay is actually visible', () => {
    const planned = plan();
    expect(planned).not.toBeNull();
    expect(renderedSourceGhostPresentationKey(planned, true)).toBe('');
    expect(renderedSourceGhostPresentationKey(planned, false)).toBe('candidate-1:blob-1');
    expect(renderedSourceGhostPresentationKey(null, false)).toBe('');
  });
});

describe('source ghost visibility authority', () => {
  it('binds a presentation to its exact value, surface, and caret but permits return', () => {
    const anchor = {
      value: 'A waits.',
      surfaceKey: 'document-1:revision-2',
      selectionStart: 2,
      selectionEnd: 2
    };
    expect(sourceGhostAnchorMatches(anchor, 'A waits.', 'document-1:revision-2', 2, 2)).toBe(true);
    expect(sourceGhostAnchorMatches(anchor, 'A waits.', 'document-1:revision-2', 3, 3)).toBe(false);
    expect(sourceGhostAnchorMatches(anchor, 'A waits!', 'document-1:revision-2', 2, 2)).toBe(false);
    expect(sourceGhostAnchorMatches(anchor, 'A waits.', 'document-2:revision-2', 2, 2)).toBe(false);
    // Returning after a transient blur/caret excursion does not mutate the
    // immutable anchor or permanently suppress the same presentation key.
    expect(sourceGhostAnchorMatches(anchor, 'A waits.', 'document-1:revision-2', 2, 2)).toBe(true);
  });

  it('requires the first rendered ghost rect to intersect the viewport', () => {
    const viewport = { left: 20, top: 10, right: 220, bottom: 110 };
    expect(sourceGhostRectIntersectsViewport(
      { left: 30, top: 20, right: 80, bottom: 40 },
      viewport
    )).toBe(true);
    expect(sourceGhostRectIntersectsViewport(
      { left: 30, top: 120, right: 80, bottom: 140 },
      viewport
    )).toBe(false);
    expect(sourceGhostRectIntersectsViewport(
      { left: 30, top: -40, right: 80, bottom: 10 },
      viewport
    )).toBe(false);
    expect(sourceGhostRectIntersectsViewport(
      { left: 220, top: 20, right: 250, bottom: 40 },
      viewport
    )).toBe(false);
    expect(sourceGhostRectIntersectsViewport(
      { left: -100, top: 20, right: 80, bottom: 40 },
      viewport
    )).toBe(false);
  });

  it('allows a visible zero-width newline insertion point but rejects invalid geometry', () => {
    const viewport = { left: 20, top: 10, right: 220, bottom: 110 };
    expect(sourceGhostRectIntersectsViewport(
      { left: 30, top: 20, right: 30, bottom: 40 },
      viewport
    )).toBe(true);
    expect(sourceGhostRectIntersectsViewport(
      { left: 30, top: 20, right: 30, bottom: 20 },
      viewport
    )).toBe(false);
    expect(sourceGhostRectIntersectsViewport(
      { left: Number.NaN, top: 20, right: 80, bottom: 40 },
      viewport
    )).toBe(false);
  });

  it('requires the live keydown witness to match the exact presentation', () => {
    expect(sourceGhostVisibilityWitnessMatches('candidate:blob', 'candidate:blob'))
      .toBe(true);
    expect(sourceGhostVisibilityWitnessMatches('candidate:blob', '')).toBe(false);
    expect(sourceGhostVisibilityWitnessMatches('', '')).toBe(false);
    expect(sourceGhostVisibilityWitnessMatches('candidate:blob', 'other:blob')).toBe(false);
  });
});
