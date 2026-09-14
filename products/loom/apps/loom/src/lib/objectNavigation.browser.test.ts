import { afterEach, describe, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { baseKeymap } from 'prosemirror-commands';
import { keymap } from 'prosemirror-keymap';
import { EditorState, NodeSelection, TextSelection } from 'prosemirror-state';
import { EditorView } from 'prosemirror-view';
import { flushMediaObjectDrafts, mediaObjectView } from './mediaObjectView';
import { objectNavigation } from './objectNavigation';
import { parseVisualMarkdown, serializeVisualMarkdown } from './markdownSafety';
import { visualMarkdownFenceEnter, visualMarkdownInputRules } from './visualInputRules';
import { shaderCodeBlockView } from './shaderCodeBlock';
import '../app.css';

const shader = '```wgsl\nfn shade(p: vec2<f32>) -> vec4<f32> { return vec4<f32>(p, 0.0, 1.0); }\n```';
// Only exercises the native compiler response boundary, never WGSL semantics.
const fragment = '#version 300 es\nprecision highp float; out vec4 color; void main() { color = vec4(1.0); }';
let view: EditorView;
afterEach(() => { view?.destroy(); document.body.replaceChildren(); });
function open(markdown: string) {
  const target = document.createElement('div');
  document.body.append(target);
  view = new EditorView(target, {
    state: EditorState.create({ doc: parseVisualMarkdown(markdown), plugins: [objectNavigation(), visualMarkdownInputRules(parseVisualMarkdown(markdown).type.schema), keymap({ Enter: visualMarkdownFenceEnter }), keymap(baseKeymap)] }),
    nodeViews: {
      code_block: (node, editor, getPos) => shaderCodeBlockView(node, editor, getPos, async () => ({ fragment })),
      image: (node, editor, getPos) => {
        const media = document.createElement(String(node.attrs.alt).startsWith('Audio:') ? 'audio' : 'img');
        if (media instanceof HTMLAudioElement) { media.controls = true; media.setAttribute('aria-label', node.attrs.alt); }
        else { media.alt = node.attrs.alt; media.src = 'data:image/svg+xml,' + encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40" fill="red"/></svg>'); }
        return mediaObjectView(node, editor, getPos, media);
      }
    },
    dispatchTransaction(transaction) { view.updateState(view.state.apply(transaction)); }
  });
  return view;
}
const markdown = () => serializeVisualMarkdown(view.state.doc);
async function editMedia() {
  await userEvent.click(document.querySelector('.loom-media-object')!, { button: 'right' });
  await page.getByRole('menuitem', { name: 'Edit', exact: true }).click();
  return page.getByRole('textbox', { name: 'Object Markdown' });
}

describe('rendered objects in ordinary Markdown', () => {
  it('selects an initially focused shader without hidden caret or byte changes, then Enter makes space after it', async () => {
    open(shader + '\r\n');
    await expect.poll(() => document.querySelector('canvas')?.hidden).toBe(false);
    expect(view.state.selection).toBeInstanceOf(NodeSelection);
    expect(markdown()).toBe(shader + '\r\n');
    view.focus();
    await userEvent.keyboard('{ArrowDown}');
    expect(markdown()).toBe(shader + '\r\n');
    await userEvent.keyboard('{Enter}After');
    expect(view.state.doc.lastChild?.textContent).toBe('After');
    expect(markdown()).toBe(shader + '\n\nAfter\r\n');
  });

  it('moves between prose and rendered shader with arrows and inserts before a leading object with Shift+Enter', async () => {
    open('Before\n\n' + shader + '\n\nAfter');
    await expect.poll(() => document.querySelector('canvas')?.hidden).toBe(false);
    view.dispatch(view.state.tr.setSelection(TextSelection.create(view.state.doc, 7)));
    view.focus();
    await userEvent.keyboard('{ArrowRight}');
    expect(view.state.selection).toBeInstanceOf(NodeSelection);
    await userEvent.keyboard('{ArrowRight}x');
    expect(view.state.doc.lastChild?.textContent).toBe('xAfter');
    view.destroy();
    document.body.replaceChildren();
    open(shader);
    await expect.poll(() => document.querySelector('canvas')?.hidden).toBe(false);
    view.focus();
    await userEvent.keyboard('{Shift>}{Enter}{/Shift}Before');
    expect(markdown()).toBe('Before\n\n' + shader);
  });

  it('renders a newly completed shader fence after its caret exits into prose', async () => {
    open('');
    view.focus();
    await userEvent.keyboard('```wgsl{Enter}fn shade(p: vec2<f32>) -> vec4<f32> {{ return vec4<f32>(p, 0.0, 1.0); }{Enter}```{Enter}After');
    await expect.poll(() => document.querySelector('canvas')?.hidden).toBe(false);
    expect(markdown()).toBe(shader + '\n\nAfter');
    expect(view.state.selection).toBeInstanceOf(TextSelection);
    expect(view.state.selection.$head.parent.textContent).toBe('After');
    expect((document.querySelector('pre') as HTMLElement).getBoundingClientRect().height).toBe(0);
  });

  it('edits image Markdown locally, cancels without mutation, and applies only a valid atom on Enter', async () => {
    const original = '![picture](assets/picture.png)\n';
    open(original);
    let input = await editMedia();
    expect((document.querySelector('img') as HTMLElement).getBoundingClientRect().height).toBe(0);
    await input.fill('discard this');
    expect(markdown()).toBe(original);
    await userEvent.keyboard('{Escape}');
    expect(document.querySelector('.loom-object-source')).toBeNull();
    expect(markdown()).toBe(original);
    input = await editMedia();
    await input.fill('ordinary prose');
    await userEvent.keyboard('{Enter}');
    expect(markdown()).toBe(original);
    expect(page.getByRole('status').element().textContent).toContain('one image or audio');
    await input.fill('![new caption](assets/picture.png)');
    await userEvent.keyboard('{Enter}');
    expect(markdown()).toBe('![new caption](assets/picture.png)\n');
    expect(document.querySelector('.loom-object-source')).toBeNull();
    expect(view.state.selection).toBeInstanceOf(TextSelection);
  });

  it('flushes a valid object draft and refuses invalid or composing drafts without losing them', async () => {
    const original = '![picture](assets/picture.png)';
    open(original);
    let input = await editMedia();
    await input.fill('unfinished reference');
    expect(flushMediaObjectDrafts(view)).toBe(false);
    expect((input.element() as HTMLInputElement).value).toBe('unfinished reference');
    expect(markdown()).toBe(original);
    await input.fill('![saved](assets/picture.png)');
    input.element().dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
    expect(flushMediaObjectDrafts(view)).toBe(false);
    expect(markdown()).toBe(original);
    input.element().dispatchEvent(new CompositionEvent('compositionend', { bubbles: true }));
    expect(flushMediaObjectDrafts(view)).toBe(true);
    expect(markdown()).toBe('![saved](assets/picture.png)');
    expect(document.querySelector('.loom-object-source')).toBeNull();
  });

  it('preserves native audio controls and moves or types before/after the final media atom', async () => {
    const original = '![Audio: sample](loom-attachment:clip)';
    open(original);
    expect(document.querySelector('audio')?.controls).toBe(true);
    const input = await editMedia();
    await input.fill('![Audio: renamed](loom-attachment:clip)');
    await userEvent.keyboard('{Escape}');
    expect(markdown()).toBe(original);
    view.focus();
    await userEvent.keyboard('{ArrowLeft}Before ');
    expect(markdown()).toBe('Before ' + original);
    view.dispatch(view.state.tr.setSelection(NodeSelection.create(view.state.doc, 8)));
    await userEvent.keyboard('{ArrowRight} after');
    expect(markdown()).toBe('Before ' + original + ' after');
    view.dispatch(view.state.tr.setSelection(NodeSelection.create(view.state.doc, 8)));
    await userEvent.keyboard('{Enter}Next');
    expect(markdown()).toBe('Before ' + original + '\n\nNext after');
    expect(document.querySelectorAll('audio')).toHaveLength(1);
  });
});
