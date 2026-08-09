import { Plugin, PluginKey, Selection, type EditorState, type Transaction } from 'prosemirror-state';
import type { EditorView } from 'prosemirror-view';

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
  accept: (candidateId: string, presentationKey: string) => void;
  dismiss: (candidateId: string, presentationKey: string) => void;
}

export interface GhostOverlayGeometry {
  left: number;
  top: number;
  maxWidth: number;
}

export interface GhostOverlayBounds {
  caret: { left: number; top: number; bottom: number };
  shell: { left: number; top: number };
  text: { left: number; right: number };
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

export function renderedGhostPresentationKey(state: EditorState): string {
  return currentGhostTextPlan(state)?.presentationKey ?? '';
}

export function currentGhostTextPlan(state: EditorState): GhostTextPlan | null {
  return planGhostText(state, ghostTextPluginKey.getState(state) ?? null);
}

/** Position a visual-only sibling overlay without entering the textbox tree. */
export function planGhostOverlayGeometry(bounds: GhostOverlayBounds): GhostOverlayGeometry | null {
  const values = [
    bounds.caret.left,
    bounds.caret.top,
    bounds.caret.bottom,
    bounds.shell.left,
    bounds.shell.top,
    bounds.text.left,
    bounds.text.right
  ];
  if (!values.every(Number.isFinite) || bounds.text.right <= bounds.text.left) return null;

  const sameLineWidth = bounds.text.right - bounds.caret.left;
  if (sameLineWidth >= 72) {
    return {
      left: Math.max(0, bounds.caret.left - bounds.shell.left),
      top: Math.max(0, bounds.caret.top - bounds.shell.top),
      maxWidth: sameLineWidth
    };
  }
  return {
    left: Math.max(0, bounds.text.left - bounds.shell.left),
    top: Math.max(0, bounds.caret.bottom - bounds.shell.top),
    maxWidth: bounds.text.right - bounds.text.left
  };
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
      handleKeyDown(view, event) {
        const plan = planGhostText(view.state, ghostTextPluginKey.getState(view.state) ?? null);
        if (!plan || event.isComposing || event.keyCode === 229) return false;
        if (event.key === 'Escape' && !event.metaKey && !event.ctrlKey && !event.altKey) {
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
          view.dispatch(clearTransaction(view));
          handlers.accept(plan.candidateId, plan.presentationKey);
          return true;
        }
        return false;
      }
    }
  });
}
