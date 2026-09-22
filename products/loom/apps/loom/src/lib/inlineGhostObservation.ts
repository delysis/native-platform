import { nextSuggestionWord } from './suggestionInteraction';

export const GHOST_PRESENTATION_ATTRIBUTE = 'data-loom-ghost-presentation';

export interface GhostClientRect {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** The glyphs read from a connected, visible DOM widget, not a candidate buffer. */
export interface InlineGhostObservation {
  presentationKey: string;
  text: string;
  utf8Bytes: number;
}

export function inlineGhostPreview(text: string): string {
  return nextSuggestionWord(text)?.trimEnd() ?? '';
}

function validRect(rect: GhostClientRect, allowZeroWidth = false): boolean {
  return [rect.left, rect.top, rect.right, rect.bottom].every(Number.isFinite) &&
    (allowZeroWidth ? rect.right >= rect.left : rect.right > rect.left) &&
    rect.bottom > rect.top;
}

function verticallyIntersects(left: GhostClientRect, right: GhostClientRect): boolean {
  return Math.min(left.bottom, right.bottom) > Math.max(left.top, right.top);
}

/** A later visible fragment cannot authorize an offscreen insertion boundary. */
export function visualGhostInsertionIsVisible(
  caret: GhostClientRect,
  firstGhostFragment: GhostClientRect,
  clip: GhostClientRect,
  direction: 'ltr' | 'rtl'
): boolean {
  if (!validRect(caret, true) || !validRect(firstGhostFragment) || !validRect(clip)) return false;
  const caretEdge = direction === 'rtl' ? caret.right : caret.left;
  const ghostEdge = direction === 'rtl' ? firstGhostFragment.right : firstGhostFragment.left;
  return caretEdge >= clip.left && caretEdge < clip.right &&
    ghostEdge >= clip.left && ghostEdge < clip.right &&
    verticallyIntersects(caret, clip) && verticallyIntersects(firstGhostFragment, clip);
}

export function observeInlineGhost(
  widget: HTMLElement,
  presentationKey: string,
  expectedText: string,
  caret: GhostClientRect,
  clip: GhostClientRect
): InlineGhostObservation | null {
  const text = widget.textContent ?? '';
  if (!presentationKey || !widget.isConnected || widget.hidden || !/\S/u.test(text) ||
      text !== expectedText || widget.getAttribute(GHOST_PRESENTATION_ATTRIBUTE) !== presentationKey) return null;

  // The app/pane may be hidden even when the editor root itself is visible.
  for (let node: HTMLElement | null = widget; node; node = node.parentElement) {
    const style = node.ownerDocument.defaultView?.getComputedStyle(node);
    if (!style || style.display === 'none' || Number.parseFloat(style.opacity) <= 0 ||
        style.visibility === 'hidden' || style.visibility === 'collapse') return null;
  }
  const direction = widget.ownerDocument.defaultView?.getComputedStyle(widget).direction;
  if (!visualGhostInsertionIsVisible(caret, widget.getBoundingClientRect(), clip,
    direction === 'rtl' ? 'rtl' : 'ltr')) return null;
  return { presentationKey, text, utf8Bytes: new TextEncoder().encode(text).byteLength };
}
