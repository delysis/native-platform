/** Shared ownership rules, not a replacement editor or a second undo stack. */
export type TextHistoryOwner = 'native' | 'prosemirror';
export type NativeEditingCommand = 'select_all' | 'copy' | 'cut' | 'paste' | 'undo' | 'redo';
export type CompositionKey = Pick<KeyboardEvent, 'isComposing' | 'keyCode'>;

export function compositionOwnsKey(event: CompositionKey, active = false): boolean {
  return active || event.isComposing || event.keyCode === 229;
}

/** Includes Shift-paste/redo/copy chords; app chrome must never reinterpret them. */
export function nativeEditingCommand(event: Pick<KeyboardEvent, 'key' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey'>): NativeEditingCommand | null {
  if (!(event.metaKey || event.ctrlKey) || event.altKey) return null;
  switch (event.key.toLowerCase()) {
    case 'a': return 'select_all';
    case 'c': return 'copy';
    case 'x': return 'cut';
    case 'v': return 'paste';
    case 'z': return event.shiftKey ? 'redo' : 'undo';
    case 'y': return 'redo';
    default: return null;
  }
}

export function textEditingElement(target: EventTarget | null): HTMLElement | null {
  if (!(target instanceof Element)) return null;
  const node = target.closest<HTMLElement>('input, textarea, [contenteditable]');
  if (node instanceof HTMLTextAreaElement) return node;
  if (node instanceof HTMLInputElement) {
    return ['text', 'search', 'url', 'email', 'password', 'tel', 'number'].includes(node.type) ? node : null;
  }
  return node?.isContentEditable ? node : null;
}

/** Observe every mounted text surface, including forms outside the manuscript. */
export function createTextCompositionBoundary() {
  const composing = new WeakSet<EventTarget>();
  return {
    start(event: Event): void { if (event.target) composing.add(event.target); },
    end(event: Event): void { if (event.target) composing.delete(event.target); },
    owns(event: KeyboardEvent): boolean {
      return compositionOwnsKey(event, Boolean(event.target && composing.has(event.target)));
    }
  };
}

/** Native context-menu history events must enter the rich editor's own history. */
export function handleHistoryInput(
  event: Pick<InputEvent, 'inputType' | 'isComposing' | 'preventDefault'>,
  owner: TextHistoryOwner,
  readonly: boolean,
  undo: () => boolean,
  redo: () => boolean
): boolean {
  if (owner !== 'prosemirror' || event.isComposing || (event.inputType !== 'historyUndo' && event.inputType !== 'historyRedo')) return false;
  // Even an empty/locked PM history must not fall back to the browser's unrelated
  // contenteditable history and bypass document transaction/readonly checks.
  event.preventDefault();
  if (!readonly) (event.inputType === 'historyUndo' ? undo : redo)();
  return true;
}

/** Ctrl-C is an interrupt only when it cannot be a selected-text copy. */
export function terminalInterruptOwnsKey(
  event: Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'metaKey' | 'altKey' | 'shiftKey'>,
  busy: boolean, selectionStart: number, selectionEnd: number
): boolean {
  return busy && selectionStart === selectionEnd && event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey && event.key.toLowerCase() === 'c';
}

export interface NativeTextEdit {
  readonly value: string;
  readonly selectionStart: number;
  readonly selectionEnd: number;
}

/** An ordinary textarea edit must enter native undo, not use value/setRangeText.
 * The browser owns the transaction. No synthetic fallback/second history stack.
 * A platform refusing the command leaves the old selection and normal Tab focus
 * navigation intact; exact-byte success is checked rather than trusting a flag.
 */
export function applyNativeTextEdit(input: HTMLTextAreaElement, edit: NativeTextEdit): boolean {
  if (input.disabled || input.readOnly || input.ownerDocument.activeElement !== input ||
    typeof input.ownerDocument.execCommand !== 'function') return false;
  const before = input.value, start = input.selectionStart, end = input.selectionEnd;
  if (edit.value === before) return false;
  let from = 0, oldEnd = before.length, newEnd = edit.value.length;
  while (from < oldEnd && from < newEnd && before[from] === edit.value[from]) from++;
  // Never select half of a UTF-16 surrogate pair.
  if (from > 0 && /[\uDC00-\uDFFF]/u.test(before[from] ?? '')) from--;
  while (oldEnd > from && newEnd > from && before[oldEnd - 1] === edit.value[newEnd - 1]) { oldEnd--; newEnd--; }
  if (/[\uDC00-\uDFFF]/u.test(before[oldEnd] ?? '')) { oldEnd++; newEnd++; }
  input.setSelectionRange(from, oldEnd);
  let notified = false;
  const noteInput = () => { notified = true; };
  input.addEventListener('input', noteInput);
  try { input.ownerDocument.execCommand('insertText', false, edit.value.slice(from, newEnd)); }
  finally {
    input.removeEventListener('input', noteInput);
    if (input.value === before) input.setSelectionRange(start, end);
  }
  if (input.value !== edit.value) return false;
  input.setSelectionRange(edit.selectionStart, edit.selectionEnd);
  // Some native command implementations omit the input notification. Deliver
  // exactly one notification, never a second value assignment or undo record.
  if (!notified) input.dispatchEvent(new Event('input', { bubbles: true }));
  return true;
}
