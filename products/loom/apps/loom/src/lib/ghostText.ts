import { defaultMarkdownParser, defaultMarkdownSerializer } from 'prosemirror-markdown';
import { Plugin, PluginKey, TextSelection, type EditorState, type Transaction } from 'prosemirror-state';
import { Decoration, DecorationSet, type EditorView } from 'prosemirror-view';
import {
  insertionPreservesExtendedGraphemeEdges,
  isExtendedGraphemeBoundary
} from './graphemeBoundary';
import { parseVisualMarkdown } from './markdownSafety';
import { nextSuggestionWord, type SuggestionAlternative } from './suggestionInteraction';

export interface GhostTextPresentation {
  active: boolean;
  candidateId: string;
  presentationKey: string;
  surfaceKey: string;
  anchorByteOffset: number;
  text: string;
  insertsOnAccept?: boolean;
  alternatives?: readonly SuggestionAlternative[];
  hidden?: boolean;
  unconsumeText?: string;
  fanVisible?: boolean;
}

export interface GhostTextPlan {
  candidateId: string;
  position: number;
  presentationKey: string;
  surfaceKey: string;
  anchorByteOffset: number;
  text: string;
  insertsOnAccept: boolean;
  alternatives: readonly SuggestionAlternative[];
  hidden: boolean;
  unconsumeText: string;
  fanVisible: boolean;
}

export interface GhostTextHandlers {
  /**
   * Synchronously consume the exact visible presentation. A false result
   * falls back to inserting an ordinary tab into the manuscript.
   */
  accept: (candidateId: string, presentationKey: string) => boolean;
  insert?: (candidateId: string, presentationKey: string, text: string) => boolean;
  unconsume?: (candidateId: string, presentationKey: string, text: string) => boolean;
  cycle?: (offset: number) => void;
  dismiss: (candidateId: string, presentationKey: string) => void;
  visible: (
    presentationKey: string,
    surfaceKey: string,
    anchorByteOffset: number
  ) => boolean;
}

export interface GhostClientRect {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

type GhostTextMeta =
  | { kind: 'set'; presentation: GhostTextPresentation }
  | { kind: 'clear' }
  | { kind: 'fan'; visible: boolean };

export const ghostTextPluginKey = new PluginKey<GhostTextPresentation | null>('loom-ghost-text');
export const VISUAL_TAB_INDENT = '\t';

// Private-use code points make the witness extremely unlikely to occur in a
// manuscript while remaining literal text under the CommonMark serializer.
// We still reject a collision rather than guessing.
const CARET_BOUNDARY_WITNESS = '\uE000LOOM_CARET_BOUNDARY_7F3A9D2C\uE001';
const GHOST_PRESENTATION_ATTRIBUTE = 'data-loom-ghost-presentation';

function transactionMeta(transaction: Transaction): GhostTextMeta | undefined {
  return transaction.getMeta(ghostTextPluginKey) as GhostTextMeta | undefined;
}

export function planGhostText(
  state: EditorState,
  presentation: GhostTextPresentation | null
): GhostTextPlan | null {
  if (
    !presentation?.active ||
    !presentation.candidateId ||
    !presentation.presentationKey ||
    !presentation.surfaceKey ||
    !Number.isSafeInteger(presentation.anchorByteOffset) ||
    presentation.anchorByteOffset < 0 ||
    !presentation.text ||
    !/\S/u.test(presentation.text) ||
    !state.selection.empty ||
    !(state.selection instanceof TextSelection)
  ) return null;

  return {
    candidateId: presentation.candidateId,
    position: state.selection.from,
    presentationKey: presentation.presentationKey,
    surfaceKey: presentation.surfaceKey,
    anchorByteOffset: presentation.anchorByteOffset,
    text: presentation.text,
    insertsOnAccept: Boolean(presentation.insertsOnAccept),
    alternatives: presentation.alternatives ?? [],
    hidden: Boolean(presentation.hidden),
    unconsumeText: presentation.unconsumeText ?? '',
    fanVisible: Boolean(presentation.fanVisible)
  };
}

interface FlattenedTextCaret {
  text: string;
  offset: number;
}

/**
 * Flatten the caret's visible textblock across mark boundaries. ProseMirror
 * text-node sizes are UTF-16 code units, so `parentOffset` is the matching
 * string offset while every inline child is text. Inline atoms need an
 * explicit visible-text policy of their own; fail closed rather than inventing
 * a placeholder that could make a grapheme proof lie.
 */
function flattenedTextCaret(state: EditorState): FlattenedTextCaret | null {
  if (!state.selection.empty || !(state.selection instanceof TextSelection)) return null;
  const cursor = state.selection.$cursor;
  if (!cursor || !cursor.parent.isTextblock || !cursor.parent.inlineContent) return null;

  let text = '';
  let exact = true;
  cursor.parent.forEach((node) => {
    if (!node.isText || node.text === undefined) {
      exact = false;
      return;
    }
    text += node.text;
  });
  if (
    !exact ||
    text.length !== cursor.parent.content.size ||
    cursor.parentOffset < 0 ||
    cursor.parentOffset > text.length
  ) return null;
  return { text, offset: cursor.parentOffset };
}

function visibleCaretIsExtendedGraphemeBoundary(state: EditorState): boolean {
  const caret = flattenedTextCaret(state);
  return caret !== null && isExtendedGraphemeBoundary(caret.text, caret.offset);
}

function insertionPreservesVisibleGraphemeEdges(
  state: EditorState,
  text: string
): boolean {
  const caret = flattenedTextCaret(state);
  if (!caret) return false;
  return insertionPreservesExtendedGraphemeEdges(caret.text, caret.offset, text);
}

function utf8BoundaryToUtf16Index(text: string, targetBytes: number): number | null {
  if (!Number.isSafeInteger(targetBytes) || targetBytes < 0) return null;
  const encoder = new TextEncoder();
  let bytes = 0;
  let utf16 = 0;
  for (const scalar of text) {
    if (bytes === targetBytes) return utf16;
    bytes += encoder.encode(scalar).byteLength;
    utf16 += scalar.length;
    if (bytes > targetBytes) return null;
  }
  return bytes === targetBytes ? utf16 : null;
}

/**
 * Prove an exact canonical Markdown byte boundary for the current visual
 * caret. The witness is inserted only into an uncommitted ProseMirror
 * transaction, serialized with the complete document, then removed again.
 * This lets the serializer account for block prefixes, marks, and list
 * structure without maintaining a second handwritten source map.
 */
export function exactMarkdownByteOffsetAtSelection(
  state: EditorState,
  canonicalMarkdown: string
): number | null {
  if (
    !state.selection.empty ||
    !(state.selection instanceof TextSelection) ||
    !visibleCaretIsExtendedGraphemeBoundary(state) ||
    canonicalMarkdown.includes(CARET_BOUNDARY_WITNESS)
  ) return null;

  try {
    const witnessed = defaultMarkdownSerializer.serialize(
      state.tr.insertText(CARET_BOUNDARY_WITNESS).doc
    );
    const boundary = witnessed.indexOf(CARET_BOUNDARY_WITNESS);
    if (
      boundary < 0 ||
      witnessed.lastIndexOf(CARET_BOUNDARY_WITNESS) !== boundary
    ) return null;
    const restored =
      witnessed.slice(0, boundary) +
      witnessed.slice(boundary + CARET_BOUNDARY_WITNESS.length);
    if (restored !== canonicalMarkdown) return null;
    const encoder = new TextEncoder();
    const prefixBytes = encoder.encode(witnessed.slice(0, boundary)).byteLength;
    const suffixBytes = encoder.encode(
      witnessed.slice(boundary + CARET_BOUNDARY_WITNESS.length)
    ).byteLength;
    if (prefixBytes + suffixBytes !== encoder.encode(canonicalMarkdown).byteLength) return null;
    return prefixBytes;
  } catch {
    return null;
  }
}

/**
 * Cheap context-neutral screen used while choosing among branch candidates.
 * The exact document-context proof below remains authoritative.
 */
export function visualGhostTextMayBePlainProse(text: string): boolean {
  if (!text || !/\S/u.test(text) || /\r/u.test(text)) return false;
  const prose = text.startsWith('\n\n') ? text.slice(2) : text;
  if (!prose || prose.startsWith('\n') || prose.endsWith('\n')) return false;
  const paragraphs = prose.split('\n\n');
  if (paragraphs.some((paragraph) => !paragraph || paragraph.includes('\n'))) return false;

  try {
    const left = '\uE100LOOM_LEFT\uE101';
    const right = '\uE102LOOM_RIGHT\uE103';
    return paragraphs.every((paragraphText) => {
      const raw = defaultMarkdownParser.parse(paragraphText);
      if (raw.childCount !== 1 || raw.firstChild?.type.name !== 'paragraph') return false;
      const wrapped = `${left}${paragraphText}${right}`;
      const parsed = defaultMarkdownParser.parse(wrapped);
      const paragraph = parsed.childCount === 1 ? parsed.firstChild : null;
      if (
        !paragraph ||
        paragraph.type.name !== 'paragraph' ||
        paragraph.textContent !== wrapped
      ) {
        return false;
      }
      let plain = true;
      paragraph.descendants((node) => {
        if (!node.isText || node.marks.length > 0) plain = false;
      });
      return plain && defaultMarkdownSerializer.serialize(parsed) === wrapped;
    });
  } catch {
    return false;
  }
}

/**
 * Return the longest exact leading slice that the visual editor can render as
 * literal prose. A completion may wander into Markdown or another paragraph
 * after a useful opening; hiding the entire suggestion in that case makes a
 * successful generation look broken. Partial slices are inserted as ordinary
 * edits and never promoted as if they were the full stored candidate.
 */
export function visualGhostTextSafePrefix(text: string): string | null {
  if (visualGhostTextMayBePlainProse(text)) return text;
  if (!text || !/\S/u.test(text)) return null;

  const boundaries = new Set<number>();
  for (const match of text.matchAll(/\n|[\s,.;:!?—]+/gu)) {
    if (match.index > 0) boundaries.add(match.index);
    boundaries.add(match.index + match[0].length);
  }
  for (const match of text.matchAll(/[*_`#[\]()>!-]/gu)) {
    if (match.index > 0) boundaries.add(match.index);
  }

  const ordered = [...boundaries]
    .filter((boundary) => boundary > 0 && boundary < text.length)
    .sort((left, right) => right - left);
  for (const boundary of ordered) {
    const prefix = text.slice(0, boundary).replace(/[ \t\n]+$/u, '');
    if (prefix && visualGhostTextMayBePlainProse(prefix)) return prefix;
  }
  return null;
}

/**
 * Prove that promoting the exact raw bytes yields canonical plain prose at the
 * exact visual caret. Inline text is checked against a literal ProseMirror
 * transaction. Paragraph continuations are checked by parsing and serializing
 * the complete promoted manuscript, so Markdown controls cannot masquerade as
 * prose and no normalization can silently change the admitted bytes.
 */
export function visualGhostTextIsFaithfulAtSelection(
  state: EditorState,
  canonicalMarkdown: string,
  anchorByteOffset: number,
  text: string
): boolean {
  if (
    exactMarkdownByteOffsetAtSelection(state, canonicalMarkdown) !== anchorByteOffset ||
    !visualGhostTextMayBePlainProse(text) ||
    !insertionPreservesVisibleGraphemeEdges(state, text)
  ) return false;
  const boundary = utf8BoundaryToUtf16Index(canonicalMarkdown, anchorByteOffset);
  if (boundary === null) return false;
  try {
    const promotedMarkdown =
      canonicalMarkdown.slice(0, boundary) + text + canonicalMarkdown.slice(boundary);
    if (text.includes('\n')) {
      const promotedDocument = parseVisualMarkdown(promotedMarkdown);
      return defaultMarkdownSerializer.serialize(promotedDocument) === promotedMarkdown;
    }
    const literalDocument = state.tr.insertText(text).doc;
    return defaultMarkdownSerializer.serialize(literalDocument) === promotedMarkdown &&
      parseVisualMarkdown(promotedMarkdown).eq(literalDocument);
  } catch {
    return false;
  }
}

export function renderedGhostPresentationKey(state: EditorState): string {
  return currentGhostTextPlan(state)?.presentationKey ?? '';
}

export function currentGhostTextPlan(state: EditorState): GhostTextPlan | null {
  return planGhostText(state, ghostTextPluginKey.getState(state) ?? null);
}

function validRect(rect: GhostClientRect, allowZeroWidth = false): boolean {
  return [rect.left, rect.top, rect.right, rect.bottom].every(Number.isFinite) &&
    (allowZeroWidth ? rect.right >= rect.left : rect.right > rect.left) &&
    rect.bottom > rect.top;
}

function verticallyIntersects(left: GhostClientRect, right: GhostClientRect): boolean {
  return Math.min(left.bottom, right.bottom) > Math.max(left.top, right.top);
}

/**
 * Require both the actual ProseMirror caret and the ghost's first rendered
 * fragment to begin inside the clipping viewport. Later wrapped fragments can
 * never authorize an offscreen insertion boundary.
 */
export function visualGhostInsertionIsVisible(
  caret: GhostClientRect,
  firstGhostFragment: GhostClientRect,
  clip: GhostClientRect,
  direction: 'ltr' | 'rtl'
): boolean {
  if (
    !validRect(caret, true) ||
    !validRect(firstGhostFragment, true) ||
    !validRect(clip)
  ) return false;
  const caretEdge = direction === 'rtl' ? caret.right : caret.left;
  const ghostEdge = direction === 'rtl' ? firstGhostFragment.right : firstGhostFragment.left;
  return caretEdge >= clip.left &&
    caretEdge < clip.right &&
    ghostEdge >= clip.left &&
    ghostEdge < clip.right &&
    verticallyIntersects(caret, clip) &&
    verticallyIntersects(firstGhostFragment, clip);
}

function elementAndAncestorsAreVisible(element: HTMLElement, root: HTMLElement): boolean {
  for (let current: HTMLElement | null = element; current; current = current.parentElement) {
    const style = current.ownerDocument.defaultView?.getComputedStyle(current);
    if (!style) return false;
    if (
      style.display === 'none' ||
      style.visibility === 'hidden' ||
      style.visibility === 'collapse' ||
      Number.parseFloat(style.opacity) <= 0
    ) return false;
    if (current === root) return true;
  }
  return false;
}

/** Return the key only when the exact widget is connected and on screen. */
export function visibleGhostWidgetPresentationKey(view: EditorView): string {
  const plan = currentGhostTextPlan(view.state);
  if (!plan) return '';
  const widget = Array.from(
    view.dom.querySelectorAll<HTMLElement>(`[${GHOST_PRESENTATION_ATTRIBUTE}]`)
  ).find((candidate) =>
    candidate.getAttribute(GHOST_PRESENTATION_ATTRIBUTE) === plan.presentationKey
  );
  if (
    !widget?.isConnected ||
    widget.hidden ||
    !elementAndAncestorsAreVisible(widget, view.dom)
  ) return '';
  const clip = view.dom.closest<HTMLElement>('.editor-pane')?.getBoundingClientRect();
  if (!clip) return '';
  // The union rectangle is authoritative for a text widget that can begin
  // with paragraph whitespace. WebKit may expose only zero-height fragment
  // rects for those leading newlines even while the prose glyphs are visible.
  const renderedGhost = widget.getBoundingClientRect();
  let caret: ReturnType<EditorView['coordsAtPos']>;
  try {
    caret = view.coordsAtPos(plan.position);
  } catch {
    return '';
  }
  const direction = widget.ownerDocument.defaultView?.getComputedStyle(widget).direction;
  if (!visualGhostInsertionIsVisible(
    caret,
    renderedGhost,
    clip,
    direction === 'rtl' ? 'rtl' : 'ltr'
  )) return '';
  return plan.presentationKey;
}

function ghostWidget(plan: GhostTextPlan): HTMLElement {
  const container = document.createElement('span');
  container.className = 'loom-ghost-widget';
  container.contentEditable = 'false';
  container.draggable = false;
  container.spellcheck = false;

  const widget = document.createElement('span');
  widget.className = 'loom-visual-ghost';
  widget.classList.toggle('ghost-text-hidden', plan.hidden || plan.fanVisible);
  widget.setAttribute(GHOST_PRESENTATION_ATTRIBUTE, plan.presentationKey);
  widget.setAttribute('aria-hidden', 'true');
  widget.contentEditable = 'false';
  widget.draggable = false;
  widget.spellcheck = false;
  widget.textContent = plan.text;
  container.append(widget);

  if (plan.alternatives.length > 1) {
    const fan = document.createElement('span');
    fan.className = 'loom-ghost-fan';
    fan.setAttribute('aria-hidden', 'true');
    plan.alternatives.forEach((alternative, index) => {
      const row = document.createElement('span');
      row.className = 'loom-ghost-fan-row';
      if (alternative.presentationKey === plan.presentationKey) row.classList.add('active');
      const number = document.createElement('span');
      number.className = 'loom-ghost-fan-index';
      number.textContent = String(index + 1);
      const text = document.createElement('span');
      text.textContent = alternative.text;
      row.append(number, text);
      fan.append(row);
    });
    const hint = document.createElement('span');
    hint.className = 'loom-ghost-fan-hint';
    hint.textContent = '↑↓ choose  ·  Return insert  ·  → next word';
    fan.append(hint);
    container.append(fan);
  }
  container.classList.toggle('fan-visible', plan.fanVisible);
  return container;
}

function setGhostFanVisible(view: EditorView, visible: boolean): void {
  view.dispatch(view.state.tr
    .setMeta(ghostTextPluginKey, { kind: 'fan', visible } satisfies GhostTextMeta)
    .setMeta('addToHistory', false));
}

function alternativesMatch(
  left: readonly SuggestionAlternative[] | undefined,
  right: readonly SuggestionAlternative[] | undefined
): boolean {
  const leftItems = left ?? [];
  const rightItems = right ?? [];
  return leftItems.length === rightItems.length && leftItems.every((item, index) => {
    const other = rightItems[index];
    return Boolean(other) && item.candidateId === other.candidateId &&
      item.presentationKey === other.presentationKey &&
      item.text === other.text;
  });
}

function clearTransaction(view: EditorView): Transaction {
  return view.state.tr
    .setMeta(ghostTextPluginKey, { kind: 'clear' } satisfies GhostTextMeta)
    .setMeta('addToHistory', false);
}

export function clearGhostText(view: EditorView): void {
  if (!ghostTextPluginKey.getState(view.state)) return;
  view.dispatch(clearTransaction(view));
}

export function setGhostText(
  view: EditorView,
  presentation: GhostTextPresentation | null
): void {
  const current = ghostTextPluginKey.getState(view.state);
  if (
    current?.presentationKey === presentation?.presentationKey &&
    current?.active === presentation?.active &&
    current?.candidateId === presentation?.candidateId &&
    current?.surfaceKey === presentation?.surfaceKey &&
    current?.anchorByteOffset === presentation?.anchorByteOffset &&
    current?.text === presentation?.text &&
    Boolean(current?.insertsOnAccept) === Boolean(presentation?.insertsOnAccept) &&
    Boolean(current?.hidden) === Boolean(presentation?.hidden) &&
    Boolean(current?.fanVisible) === Boolean(presentation?.fanVisible) &&
    (current?.unconsumeText ?? '') === (presentation?.unconsumeText ?? '') &&
    alternativesMatch(current?.alternatives, presentation?.alternatives)
  ) return;
  if (!presentation) {
    clearGhostText(view);
    return;
  }
  const next = {
    ...presentation,
    fanVisible: presentation.fanVisible === undefined
      ? Boolean(current?.fanVisible)
      : presentation.fanVisible
  };
  view.dispatch(view.state.tr
    .setMeta(ghostTextPluginKey, { kind: 'set', presentation: next } satisfies GhostTextMeta)
    .setMeta('addToHistory', false));
}

export function createGhostTextPlugin(handlers: GhostTextHandlers): Plugin<GhostTextPresentation | null> {
  return new Plugin<GhostTextPresentation | null>({
    key: ghostTextPluginKey,
    state: {
      init: () => null,
      apply(transaction, current) {
        const meta = transactionMeta(transaction);
        if (meta?.kind === 'set') return meta.presentation;
        if (meta?.kind === 'fan') return current ? { ...current, fanVisible: meta.visible } : current;
        if (meta?.kind === 'clear' || transaction.docChanged || transaction.selectionSet) return null;
        return current;
      }
    },
    props: {
      decorations(state) {
        const plan = currentGhostTextPlan(state);
        if (!plan) return null;
        return DecorationSet.create(state.doc, [
          Decoration.widget(plan.position, () => ghostWidget(plan), {
            key: plan.presentationKey,
            side: 1,
            ignoreSelection: true
          })
        ]);
      },
      handleKeyDown(view, event) {
        const plan = planGhostText(view.state, ghostTextPluginKey.getState(view.state) ?? null);
        if (event.isComposing || event.keyCode === 229) return false;
        if (event.key === 'Alt' && !event.metaKey && !event.ctrlKey) {
          setGhostFanVisible(view, true);
          return false;
        }
        if (
          plan &&
          event.altKey &&
          !event.metaKey &&
          !event.ctrlKey &&
          (event.key === 'ArrowUp' || event.key === 'ArrowDown')
        ) {
          event.preventDefault();
          if (!plan.fanVisible) setGhostFanVisible(view, true);
          handlers.cycle?.(event.key === 'ArrowDown' ? 1 : -1);
          return true;
        }
        if (
          plan &&
          plan.unconsumeText &&
          event.altKey &&
          !event.metaKey &&
          !event.ctrlKey &&
          event.key === 'ArrowLeft'
        ) {
          const end = view.state.selection.from;
          const start = end - plan.unconsumeText.length;
          if (
            start >= 0 &&
            view.state.doc.textBetween(start, end, '\n', '\n') === plan.unconsumeText &&
            handlers.unconsume?.(plan.candidateId, plan.presentationKey, plan.unconsumeText)
          ) {
            event.preventDefault();
            const removedBytes = new TextEncoder().encode(plan.unconsumeText).byteLength;
            const anchorByteOffset = plan.anchorByteOffset - removedBytes;
            if (anchorByteOffset < 0) return false;
            view.dispatch(view.state.tr
              .delete(start, end)
              .setMeta(ghostTextPluginKey, {
                kind: 'set',
                presentation: {
                  active: true,
                  candidateId: plan.candidateId,
                  presentationKey: `${plan.presentationKey}:rollback:${removedBytes}`,
                  surfaceKey: plan.surfaceKey,
                  anchorByteOffset,
                  text: `${plan.unconsumeText}${plan.text}`,
                  insertsOnAccept: true,
                  alternatives: plan.alternatives,
                  hidden: plan.hidden,
                  unconsumeText: '',
                  fanVisible: plan.fanVisible
                }
              } satisfies GhostTextMeta));
            return true;
          }
        }
        if (
          plan &&
          plan.fanVisible &&
          event.altKey &&
          !event.metaKey &&
          !event.ctrlKey &&
          (event.key === 'Enter' || event.key === 'Tab')
        ) {
          if (!handlers.insert?.(plan.candidateId, plan.presentationKey, plan.text)) return false;
          event.preventDefault();
          view.dispatch(view.state.tr.insertText(plan.text));
          return true;
        }
        if (
          plan &&
          event.altKey &&
          !event.metaKey &&
          !event.ctrlKey &&
          event.key === 'ArrowRight' &&
          (plan.fanVisible || handlers.visible(
            plan.presentationKey,
            plan.surfaceKey,
            plan.anchorByteOffset
          ))
        ) {
          const word = nextSuggestionWord(plan.text);
          if (!word || !handlers.insert?.(plan.candidateId, plan.presentationKey, word)) return false;
          view.dispatch(view.state.tr.insertText(word));
          return true;
        }
        if (plan && event.key === 'Escape' && !event.metaKey && !event.ctrlKey && !event.altKey) {
          view.dispatch(clearTransaction(view));
          handlers.dismiss(plan.candidateId, plan.presentationKey);
          return true;
        }
        if (
          event.key === 'Tab' &&
          !event.shiftKey &&
          !event.metaKey &&
          !event.ctrlKey &&
          !event.altKey
        ) {
          if (plan && handlers.visible(
            plan.presentationKey,
            plan.surfaceKey,
            plan.anchorByteOffset
          )) {
            // Claim parent authority while its exact visibility witness still
            // exists. Clearing first would invalidate every legitimate
            // acceptance before the parent can bind it to durable authority.
            const accepted = plan.insertsOnAccept
              ? Boolean(handlers.insert?.(plan.candidateId, plan.presentationKey, plan.text))
              : handlers.accept(plan.candidateId, plan.presentationKey);
            if (accepted) {
              view.dispatch(plan.insertsOnAccept
                ? view.state.tr.insertText(plan.text)
                : clearTransaction(view));
              return true;
            }
          }
          if (view.editable === false) return false;
          // Tab is a writing key in Loom. When no exact visible completion can
          // consume it, insert the literal tab byte; never
          // hand focus traversal to surrounding application chrome.
          view.dispatch(view.state.tr.insertText(VISUAL_TAB_INDENT));
          return true;
        }
        return false;
      },
      handleDOMEvents: {
        keyup(view, event) {
          if (event.key === 'Alt') setGhostFanVisible(view, false);
          return false;
        },
        blur(view) {
          setGhostFanVisible(view, false);
          return false;
        }
      }
    }
  });
}
