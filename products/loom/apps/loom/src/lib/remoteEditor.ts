import type { Node } from 'prosemirror-model';
import type { EditorState } from 'prosemirror-state';
import { diffArrays, diffChars, type ChangeObject } from 'diff';

interface Span { from: number; to: number; nextFrom: number; nextTo: number }

function textSpans(before: string, after: string, deadline: number): Span[] {
  const changes = diffChars(before, after, { maxEditLength: 256, timeout: Math.max(1, deadline - performance.now()) });
  if (!changes) return [textEnvelope(before, after)];
  return spans(changes, value => value.length);
}

function spans<T>(changes: readonly ChangeObject<T>[], size: (value: T) => number): Span[] {
  const result: Span[] = [];
  let oldPosition = 0, newPosition = 0, pending: Span | null = null;
  for (const change of changes) {
    const length = size(change.value);
    if (change.added || change.removed) {
      pending ??= { from: oldPosition, to: oldPosition, nextFrom: newPosition, nextTo: newPosition };
      if (change.removed) oldPosition += length;
      if (change.added) newPosition += length;
      pending.to = oldPosition; pending.nextTo = newPosition;
    } else {
      if (pending) { result.push(pending); pending = null; }
      oldPosition += length; newPosition += length;
    }
  }
  if (pending) result.push(pending);
  return result;
}

function documentSpans(before: Node, after: Node, from = 0, nextFrom = 0, deadline = performance.now() + 16): Span[] {
  if (before.eq(after)) return [];
  if (before.isText && after.isText && before.sameMarkup(after)) {
    return textSpans(before.text!, after.text!, deadline).map(span => ({
      from: from + span.from, to: from + span.to, nextFrom: nextFrom + span.nextFrom, nextTo: nextFrom + span.nextTo,
    }));
  }
  if (!before.isLeaf && !after.isLeaf && before.sameMarkup(after)
      && before.childCount + after.childCount <= 4096 && performance.now() < deadline) {
    const left: Node[] = [], right: Node[] = [];
    before.forEach(node => left.push(node)); after.forEach(node => right.push(node));
    const changes = diffArrays(left, right, { comparator: (a, b) => a.eq(b), maxEditLength: 128, timeout: Math.max(1, deadline - performance.now()) });
    if (changes) {
      const result: Span[] = [];
      const contentFrom = from + (before.type.name === 'doc' ? 0 : 1);
      const nextContentFrom = nextFrom + (after.type.name === 'doc' ? 0 : 1);
      for (const span of spans(changes, nodes => nodes.length)) {
        let oldOffset = contentFrom + left.slice(0, span.from).reduce((sum, node) => sum + node.nodeSize, 0);
        let newOffset = nextContentFrom + right.slice(0, span.nextFrom).reduce((sum, node) => sum + node.nodeSize, 0);
        if (span.to - span.from === span.nextTo - span.nextFrom) {
          for (let i = span.from, j = span.nextFrom; i < span.to; i++, j++) {
            result.push(...documentSpans(left[i], right[j], oldOffset, newOffset, deadline));
            oldOffset += left[i].nodeSize; newOffset += right[j].nodeSize;
          }
        } else result.push({ from: oldOffset, to: oldOffset + left.slice(span.from, span.to).reduce((sum, node) => sum + node.nodeSize, 0),
          nextFrom: newOffset, nextTo: newOffset + right.slice(span.nextFrom, span.nextTo).reduce((sum, node) => sum + node.nodeSize, 0) });
      }
      return result;
    }
  }
  return [{ from, to: from + before.nodeSize, nextFrom, nextTo: nextFrom + after.nodeSize }];
}

/** Keep this editor's history and map its selection through a remote change. */
export function applyRemoteDocument(state: EditorState, next: Node): EditorState {
  // Ignore root source metadata while matching child nodes; apply it below.
  const comparable = next.type.create(state.doc.attrs, next.content, next.marks);
  const edits = documentSpans(state.doc, comparable);
  let transaction = state.tr.setMeta('addToHistory', false);
  try {
    for (const edit of edits.reverse()) transaction.replace(edit.from, edit.to, next.slice(edit.nextFrom, edit.nextTo));
    if (!transaction.doc.content.eq(next.content)) throw new Error('Remote transform was incomplete');
  } catch {
    // A structural rewrite or an exhausted diff budget may have no useful
    // interior anchor. The bounded envelope still preserves the exact result.
    transaction = envelopeTransaction(state, next);
  }
  for (const [key, value] of Object.entries(next.attrs)) {
    if (state.doc.attrs[key] !== value) transaction.setDocAttribute(key, value);
  }
  return state.apply(transaction);
}

function envelopeTransaction(state: EditorState, next: Node) {
  const transaction = state.tr.setMeta('addToHistory', false);
  const start = state.doc.content.findDiffStart(next.content);
  if (start !== null) {
    const end = state.doc.content.findDiffEnd(next.content);
    if (end) {
      const overlap = Math.max(0, start - Math.min(end.a, end.b));
      transaction.replace(start, end.a + overlap, next.slice(start, end.b + overlap));
    }
  }
  return transaction;
}

/** UTF-16 coordinates match textarea selections. Retain a cursor in unchanged
 * text; a cursor inside removed text lands at that replacement's end. */
export function mapRemoteOffset(before: string, after: string, offset: number): number {
  let delta = 0;
  for (const span of textSpans(before, after, performance.now() + 8)) {
    if (offset < span.from) break;
    if (offset <= span.to) return span.nextTo;
    delta += (span.nextTo - span.nextFrom) - (span.to - span.from);
  }
  return Math.max(0, Math.min(after.length, offset + delta));
}

function textEnvelope(before: string, after: string): Span {
  let start = 0;
  while (start < before.length && start < after.length && before[start] === after[start]) start++;
  let oldEnd = before.length, newEnd = after.length;
  while (oldEnd > start && newEnd > start && before[oldEnd - 1] === after[newEnd - 1]) { oldEnd--; newEnd--; }
  return { from: start, to: oldEnd, nextFrom: start, nextTo: newEnd };
}
