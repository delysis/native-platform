import { describe, expect, it } from 'vitest';
import { EditorState, TextSelection } from 'prosemirror-state';
import { parseVisualMarkdown, serializeVisualMarkdown } from './markdownSafety';
import { visualTerminalRange } from './terminalSelection';

describe('terminal visual source capture', () => {
  it('captures exact UTF-8 selection boundaries without changing the document or selection', () => {
    const doc = parseVisualMarkdown('A café, then another paragraph.');
    const markdown = serializeVisualMarkdown(doc);
    const state = EditorState.create({ doc, selection: TextSelection.create(doc, 3, 7) });
    const range = visualTerminalRange(state, markdown);
    expect(range).not.toBeNull();
    const bytes = new TextEncoder().encode(markdown);
    expect(new TextDecoder().decode(bytes.slice(range!.start, range!.end))).toBe('café');
    expect(state.selection.from).toBe(3);
    expect(state.selection.to).toBe(7);
    expect(serializeVisualMarkdown(state.doc)).toBe(markdown);
  });

  it('leaves an empty caret range for native paragraph selection and rejects stale Markdown', () => {
    const doc = parseVisualMarkdown('One paragraph.\n\nAnother paragraph.');
    const markdown = serializeVisualMarkdown(doc);
    const state = EditorState.create({ doc, selection: TextSelection.create(doc, 5) });
    expect(visualTerminalRange(state, markdown)).toEqual({ start: 4, end: 4 });
    expect(visualTerminalRange(state, 'Different writing.')).toBeNull();
  });
});
