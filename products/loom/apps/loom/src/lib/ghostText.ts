import { Plugin, PluginKey, Selection, type EditorState, type Transaction } from 'prosemirror-state';
import { Decoration, DecorationSet, type EditorView } from 'prosemirror-view';

export interface GhostTextPresentation {
  active: boolean;
  candidateId: string;
  presentationKey: string;
  text: string;
}

export interface GhostTextPlan {
  candidateId: string;
  position: number;
  presentationKey: string;
  text: string;
}

export interface GhostTextHandlers {
  accept: (candidateId: string) => void;
  dismiss: (candidateId: string) => void;
}

type GhostTextMeta =
  | { kind: 'set'; presentation: GhostTextPresentation }
  | { kind: 'clear' };

export const ghostTextPluginKey = new PluginKey<GhostTextPresentation | null>('loom-ghost-text');

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
    !presentation.text ||
    !/\S/u.test(presentation.text) ||
    !state.selection.empty
  ) return null;

  const documentEnd = Selection.atEnd(state.doc);
  if (
    state.selection.from !== documentEnd.from ||
    state.selection.to !== documentEnd.to
  ) return null;

  return {
    candidateId: presentation.candidateId,
    position: state.selection.from,
    presentationKey: presentation.presentationKey,
    text: presentation.text
  };
}

export function createGhostTextElement(
  ownerDocument: Document,
  text: string
): HTMLElement {
  const element = ownerDocument.createElement('span');
  element.className = 'loom-ghost-text';
  element.textContent = text;
  element.contentEditable = 'false';
  element.setAttribute('aria-hidden', 'true');
  element.setAttribute('data-loom-ghost-text', '');
  element.setAttribute('draggable', 'false');
  element.setAttribute('spellcheck', 'false');
  return element;
}

export function createGhostTextDecorations(
  state: EditorState,
  presentation: GhostTextPresentation | null
): DecorationSet {
  const plan = planGhostText(state, presentation);
  if (!plan) return DecorationSet.empty;

  return DecorationSet.create(state.doc, [
    Decoration.widget(
      plan.position,
      (view) => createGhostTextElement(view.dom.ownerDocument, plan.text),
      {
        ignoreSelection: true,
        key: `loom-ghost:${plan.presentationKey}`,
        marks: [],
        side: 1
      }
    )
  ]);
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
    current?.text === presentation?.text
  ) return;
  if (!presentation) {
    clearGhostText(view);
    return;
  }
  view.dispatch(view.state.tr
    .setMeta(ghostTextPluginKey, { kind: 'set', presentation } satisfies GhostTextMeta)
    .setMeta('addToHistory', false));
}

export function createGhostTextPlugin(handlers: GhostTextHandlers): Plugin<GhostTextPresentation | null> {
  return new Plugin<GhostTextPresentation | null>({
    key: ghostTextPluginKey,
    state: {
      init: () => null,
      apply(transaction, current) {
        if (transaction.docChanged) return null;
        const meta = transactionMeta(transaction);
        if (!meta) return current;
        return meta.kind === 'set' ? meta.presentation : null;
      }
    },
    props: {
      decorations(state) {
        return createGhostTextDecorations(state, ghostTextPluginKey.getState(state) ?? null);
      },
      handleKeyDown(view, event) {
        const plan = planGhostText(view.state, ghostTextPluginKey.getState(view.state) ?? null);
        if (!plan || event.isComposing || event.keyCode === 229) return false;
        if (event.key === 'Escape' && !event.metaKey && !event.ctrlKey && !event.altKey) {
          view.dispatch(clearTransaction(view));
          handlers.dismiss(plan.candidateId);
          return true;
        }
        if (
          event.key === 'Tab' &&
          !event.shiftKey &&
          !event.metaKey &&
          !event.ctrlKey &&
          !event.altKey
        ) {
          view.dispatch(clearTransaction(view));
          handlers.accept(plan.candidateId);
          return true;
        }
        return false;
      }
    }
  });
}
