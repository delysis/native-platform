export interface FormatMenuEscapeState {
  readonly formatMenuOpen: boolean;
  readonly compositionActive: boolean;
  readonly documentRenameOwnsEscape: boolean;
  readonly documentMenuOwnsEscape: boolean;
  readonly modelManagerOwnsEscape: boolean;
}

/**
 * The formatting popover deliberately leaves focus in ProseMirror so format
 * commands can retain an immutable selection. Route Escape at the window's
 * capture boundary before the editor can interpret it as a completion command,
 * while preserving the higher-priority controls that own their own Escape key.
 */
export function shouldCaptureFormatMenuEscape(
  event: Pick<KeyboardEvent, 'key' | 'keyCode' | 'defaultPrevented' | 'isComposing'>,
  state: FormatMenuEscapeState
): boolean {
  return event.key === 'Escape' &&
    !event.isComposing &&
    event.keyCode !== 229 &&
    !event.defaultPrevented &&
    state.formatMenuOpen &&
    !state.compositionActive &&
    !state.documentRenameOwnsEscape &&
    !state.documentMenuOwnsEscape &&
    !state.modelManagerOwnsEscape;
}
