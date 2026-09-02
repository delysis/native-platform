export interface PopupRect {
  left: number;
  top: number;
  right: number;
  bottom: number;
  width: number;
  height: number;
}

export interface PopupPlacement {
  left: number;
  top: number;
  maxHeight: number;
  side: 'above' | 'below';
}

export interface CompletionPopupDomIds {
  listboxId: string;
  optionId(index: number): string;
}

const VIEWPORT_MARGIN = 12;
const CARET_GAP = 8;
let completionPopupInstanceSequence = 0;

/** Allocate one stable, document-safe ID namespace for an editor instance. */
export function allocateCompletionPopupDomIds(
  surface: 'visual' | 'source'
): CompletionPopupDomIds {
  completionPopupInstanceSequence += 1;
  const prefix = `loom-${surface}-completion-${completionPopupInstanceSequence}`;
  return {
    listboxId: `${prefix}-listbox`,
    optionId: (index: number) => `${prefix}-option-${index + 1}`
  };
}

export function completionPopupPlacement(
  anchor: PopupRect,
  popup: Pick<PopupRect, 'width' | 'height'>,
  viewportWidth: number,
  viewportHeight: number
): PopupPlacement | null {
  const values = [
    anchor.left,
    anchor.top,
    anchor.right,
    anchor.bottom,
    popup.width,
    popup.height,
    viewportWidth,
    viewportHeight
  ];
  if (
    values.some((value) => !Number.isFinite(value)) ||
    popup.width <= 0 ||
    popup.height <= 0 ||
    viewportWidth <= VIEWPORT_MARGIN * 2 ||
    viewportHeight <= VIEWPORT_MARGIN * 2
  ) return null;

  const availableBelow = viewportHeight - VIEWPORT_MARGIN - anchor.bottom - CARET_GAP;
  const availableAbove = anchor.top - VIEWPORT_MARGIN - CARET_GAP;
  const side = availableBelow >= Math.min(popup.height, availableAbove) ? 'below' : 'above';
  const available = Math.max(1, side === 'below' ? availableBelow : availableAbove);
  const maxHeight = Math.min(popup.height, available);
  const unclampedTop = side === 'below'
    ? anchor.bottom + CARET_GAP
    : anchor.top - CARET_GAP - maxHeight;
  const maximumLeft = Math.max(VIEWPORT_MARGIN, viewportWidth - VIEWPORT_MARGIN - popup.width);
  return {
    left: Math.min(Math.max(anchor.left, VIEWPORT_MARGIN), maximumLeft),
    top: Math.min(
      Math.max(unclampedTop, VIEWPORT_MARGIN),
      viewportHeight - VIEWPORT_MARGIN - maxHeight
    ),
    maxHeight,
    side
  };
}

export function placeCompletionPopup(
  popup: HTMLElement,
  anchor: Pick<DOMRect, 'left' | 'top' | 'right' | 'bottom' | 'width' | 'height'>
): boolean {
  const bounds = popup.getBoundingClientRect();
  const placement = completionPopupPlacement(
    anchor,
    bounds,
    window.innerWidth,
    window.innerHeight
  );
  if (!placement) return false;
  popup.style.left = `${placement.left}px`;
  popup.style.top = `${placement.top}px`;
  popup.style.maxHeight = `${placement.maxHeight}px`;
  popup.dataset.side = placement.side;
  return true;
}
