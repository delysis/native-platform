export const AUTOCOMPLETE_MODEL_MENU_LONG_PRESS_MS = 550;
export const AUTOCOMPLETE_MODEL_MENU_LONG_PRESS_SLOP = 10;

export function isAutocompleteModelMenuKey(
  event: Pick<KeyboardEvent, 'key' | 'shiftKey' | 'altKey' | 'ctrlKey' | 'metaKey'>
): boolean {
  if (event.altKey || event.ctrlKey || event.metaKey) return false;
  return event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10');
}

export function canStartAutocompleteModelMenuLongPress(
  event: Pick<PointerEvent, 'pointerType' | 'button' | 'isPrimary'>
): boolean {
  return event.isPrimary && event.pointerType !== 'mouse' && event.button === 0;
}

export function autocompleteModelMenuLongPressMoved(
  origin: { readonly x: number; readonly y: number },
  current: { readonly x: number; readonly y: number }
): boolean {
  return Math.abs(current.x - origin.x) > AUTOCOMPLETE_MODEL_MENU_LONG_PRESS_SLOP ||
    Math.abs(current.y - origin.y) > AUTOCOMPLETE_MODEL_MENU_LONG_PRESS_SLOP;
}
