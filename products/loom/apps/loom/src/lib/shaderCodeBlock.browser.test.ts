import { afterEach, describe, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { EditorState, NodeSelection, TextSelection } from 'prosemirror-state';
import { EditorView } from 'prosemirror-view';
import { parseVisualMarkdown, serializeVisualMarkdown } from './markdownSafety';
import { objectNavigation } from './objectNavigation';
import { shaderCodeBlockView } from './shaderCodeBlock';
import '../app.css';

const source = 'fn shade(p: vec2<f32>) -> vec4<f32> { return vec4<f32>(p.x, p.y, 0.0, 1.0); }';
// Compiler-boundary fixture: these tests prove WebGL/NodeView behavior, not WGSL translation.
const checkerFragment = `#version 300 es
precision highp float;
out vec4 color;
void main() {
  float c = mod(floor(gl_FragCoord.x / 32.0) + floor(gl_FragCoord.y / 32.0), 2.0);
  color = vec4(c, 0.0, 1.0 - c, 1.0);
}`;
let view: EditorView | undefined;
afterEach(() => { view?.destroy(); view = undefined; document.body.replaceChildren(); });

function render(code: string, compiler: (source: string) => Promise<{ fragment: string }>, language = 'wgsl', suffix = ''): EditorView {
  const target = document.createElement('div');
  document.body.append(target);
  view = new EditorView(target, {
    state: EditorState.create({ doc: parseVisualMarkdown('```' + language + '\n' + code + '\n```' + suffix), plugins: [objectNavigation()] }),
    attributes: { role: 'textbox', 'aria-label': 'Code editor' },
    nodeViews: { code_block: (node, editor, getPos) => shaderCodeBlockView(node, editor, getPos, compiler) },
    dispatchTransaction(transaction) { view!.updateState(view!.state.apply(transaction)); }
  });
  return view;
}

function pixel(canvas: HTMLCanvasElement, x: number, y: number): number[] {
  const retained = canvas.getContext('2d');
  if (!retained) throw new Error('The rendered WebGL2 frame was not copied into its retained image.');
  return [...retained.getImageData(x, y, 1, 1).data];
}

describe('inline shader code block', () => {
  it('rasterizes compiler-provided GLSL and preserves real editing in contentDOM', async () => {
    const compiler = vi.fn(async () => ({ fragment: checkerFragment }));
    const editor = render(source, compiler, 'wgsl', '\n');
    const canvas = document.querySelector('canvas')!;
    await expect.poll(() => canvas.hidden).toBe(false);
    expect(pixel(canvas, 16, 239)).toEqual([0, 0, 255, 255]);
    expect(pixel(canvas, 48, 239)).toEqual([255, 0, 0, 255]);
    expect(serializeVisualMarkdown(editor.state.doc)).toBe('```wgsl\n' + source + '\n```\n');
    expect((document.querySelector('pre') as HTMLElement).getBoundingClientRect().height).toBe(0);
    await page.getByRole('img', { name: 'Shader preview' }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Edit', exact: true }).click();
    expect(canvas.hidden).toBe(true);
    expect((document.querySelector('pre') as HTMLElement).getBoundingClientRect().height).toBeGreaterThan(0);
    editor.dispatch(editor.state.tr.setSelection(TextSelection.create(editor.state.doc, 1)));
    editor.focus();
    await userEvent.keyboard('x');
    await expect.poll(() => editor.state.doc.firstChild!.textContent).toBe('x' + source);
    await expect.poll(() => compiler.mock.calls.length).toBe(2);
    expect(serializeVisualMarkdown(editor.state.doc)).toBe('```wgsl\nx' + source + '\n```\n');
    await userEvent.keyboard('{Escape}');
    await expect.poll(() => canvas.hidden).toBe(false);
    expect(editor.state.selection).toBeInstanceOf(NodeSelection);
    expect((document.querySelector('pre') as HTMLElement).getBoundingClientRect().height).toBe(0);
    const reopened = parseVisualMarkdown(serializeVisualMarkdown(editor.state.doc));
    expect(reopened.eq(editor.state.doc)).toBe(true);
    editor.updateState(EditorState.create({ doc: reopened }));
    expect(document.querySelector('pre > code')?.textContent).toBe('x' + source);
  });

  it('does not compile other fences and bounds oversized authored shader source', async () => {
    const compiler = vi.fn(async () => ({ fragment: checkerFragment }));
    const editor = render(source, compiler, 'text');
    expect(document.querySelector('canvas')).toBeNull();
    expect(compiler).not.toHaveBeenCalled();
    editor.dispatch(editor.state.tr.setNodeMarkup(0, undefined, { params: 'wgsl' }));
    editor.dispatch(editor.state.tr.insertText('x'.repeat(16 * 1024 + 1), 1, editor.state.doc.firstChild!.nodeSize - 1));
    await expect.poll(() => document.querySelector('.loom-shader-error')?.textContent).toBe('Shader source exceeds 16 KiB.');
    expect(compiler).not.toHaveBeenCalled();
    expect(editor.state.doc.firstChild!.textContent.length).toBe(16 * 1024 + 1);
  });

  it('serializes simultaneous fences and retains completed images without WebGL contexts', async () => {
    let release: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    let active = 0;
    let peak = 0;
    const compiler = vi.fn(async () => {
      active += 1;
      peak = Math.max(peak, active);
      await gate;
      active -= 1;
      return { fragment: checkerFragment };
    });
    render(source + '\n```\n\n```wgsl\n' + source, compiler);
    await expect.poll(() => compiler.mock.calls.length).toBe(1);
    release!();
    await expect.poll(() => document.querySelectorAll('canvas:not([hidden])').length).toBe(2);
    expect(compiler).toHaveBeenCalledTimes(2);
    expect(peak).toBe(1);
    for (const canvas of document.querySelectorAll('canvas')) {
      expect(canvas.getContext('webgl2')).toBeNull();
      expect(pixel(canvas, 16, 239)).toEqual([0, 0, 255, 255]);
    }
  });

  it('ignores a compiler reply after the editable code block is removed', async () => {
    let resolve: ((result: { fragment: string }) => void) | undefined;
    const compiler = vi.fn(() => new Promise<{ fragment: string }>((done) => { resolve = done; }));
    const editor = render(source, compiler);
    const canvas = document.querySelector('canvas')!;
    await expect.poll(() => compiler.mock.calls.length).toBe(1);
    editor.dispatch(editor.state.tr.setNodeMarkup(0, undefined, { params: 'text' }));
    resolve!({ fragment: checkerFragment });
    await Promise.resolve();
    expect(canvas.hidden).toBe(true);
    expect(document.querySelector('canvas')).toBeNull();
    expect(serializeVisualMarkdown(editor.state.doc)).toBe('```text\n' + source + '\n```');
  });
});
