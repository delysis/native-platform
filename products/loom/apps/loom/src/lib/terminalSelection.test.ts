import { describe, expect, it } from 'vitest';
import { EditorState, NodeSelection, TextSelection } from 'prosemirror-state';
import { parseVisualMarkdown, serializeVisualMarkdown } from './markdownSafety';
import { visualTerminalRange } from './terminalSelection';

describe('terminal visual source capture', () => {
  it('captures exact UTF-8 selection boundaries without changing the document or selection', () => {
    const doc = parseVisualMarkdown('A café, then another paragraph.\r\n\r\n');
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

  it('captures the selected inline audio atom exactly, including duplicate attachments and EOF bytes', () => {
    const audio = '![Audio: Café](loom-attachment:audio-id "loom-waveform:08ff")';
    const markdown = `First ${audio}\n\nSecond 🧵 ${audio} end.\r\n`;
    const doc = parseVisualMarkdown(markdown);
    let selectedPosition = -1;
    doc.descendants((node, position) => { if (node.type.name === 'image') selectedPosition = position; });
    const selection = NodeSelection.create(doc, selectedPosition);
    const state = EditorState.create({ doc, selection });
    const range = visualTerminalRange(state, markdown);
    expect(range).not.toBeNull();
    const expectedStart = new TextEncoder().encode(markdown.slice(0, markdown.lastIndexOf(audio))).length;
    expect(range).toEqual({ start: expectedStart, end: expectedStart + new TextEncoder().encode(audio).length });
    const bytes = new TextEncoder().encode(markdown);
    expect(new TextDecoder().decode(bytes.slice(range!.start, range!.end))).toBe(audio);
    expect(state.selection).toBe(selection);
    expect(serializeVisualMarkdown(state.doc)).toBe(markdown);
    expect(visualTerminalRange(state, markdown + 'stale')).toBeNull();
  });

  it('does not substitute an arbitrary source position for unmappable block selections', () => {
    const markdown = '```text\ncode\n```';
    const doc = parseVisualMarkdown(markdown);
    const state = EditorState.create({ doc, selection: NodeSelection.create(doc, 0) });
    expect(visualTerminalRange(state, markdown)).toBeNull();
  });

});
