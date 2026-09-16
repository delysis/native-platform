import { EditorState, Plugin, PluginKey } from 'prosemirror-state';
import { Decoration, DecorationSet } from 'prosemirror-view';
import type { Node } from 'prosemirror-model';
import { documentReferenceDiagnostics } from './ipc';
import { parseVisualMarkdown } from './markdownSafety';

export interface ReferenceScope { projectId: string; sessionId: string; revision: string }
/** Native offsets are UTF-8 bytes; presentation offsets are UTF-16 indices. */
export interface ReferenceDiagnostic { start: number; end: number; message: string }

export function decodeReferenceDiagnostics(text: string, native: readonly ReferenceDiagnostic[]): ReferenceDiagnostic[] {
  const bytes = new TextEncoder().encode(text);
  const decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
  let previousByte = 0, previousIndex = 0;
  try {
    return native.map(item => {
      if (!Number.isSafeInteger(item.start) || !Number.isSafeInteger(item.end) || item.start < previousByte || item.end <= item.start || item.end > bytes.length) throw new Error('Invalid reference range');
      const start = previousIndex + decoder.decode(bytes.subarray(previousByte, item.start)).length;
      const end = start + decoder.decode(bytes.subarray(item.start, item.end)).length;
      previousByte = item.end; previousIndex = end;
      return { ...item, start, end };
    });
  } catch { return []; }
}

/** One pending read per surface; a result never crosses text or workspace changes. */
export class ReferenceDiagnostics {
  private key = '';
  private serial = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private active = false;
  private next: (() => Promise<void>) | null = null;
  constructor(private readonly read = documentReferenceDiagnostics) {}
  update(scope: ReferenceScope | null, text: string, publish: (items: ReferenceDiagnostic[]) => void): void {
    const key = JSON.stringify([scope, text]);
    if (key === this.key) return;
    this.key = key;
    const serial = ++this.serial;
    clearTimeout(this.timer);
    this.next = null;
    publish([]);
    if (!scope || !text.includes('@') && !text.includes('loom-material:') && !text.includes('loom-evidence:')) return;
    this.timer = setTimeout(() => {
      this.next = async () => {
        try {
          const result = await this.read(scope.projectId, scope.sessionId, text);
          if (serial === this.serial) publish(decodeReferenceDiagnostics(text, result));
        } catch { /* A failed check does not diagnose valid writing as broken. */ }
      };
      void this.drain();
    }, 650);
  }
  private async drain(): Promise<void> {
    if (this.active || !this.next) return;
    const next = this.next;
    this.next = null; this.active = true;
    try { await next(); } finally { this.active = false; void this.drain(); }
  }
  dispose(): void { this.serial++; this.next = null; clearTimeout(this.timer); }
}

export const referenceDiagnosticKey = new PluginKey<DecorationSet>('referenceDiagnostics');
export function referenceDiagnosticPlugin(): Plugin<DecorationSet> {
  return new Plugin({
    key: referenceDiagnosticKey,
    state: {
      init: () => DecorationSet.empty,
      apply: (transaction, current) => transaction.getMeta(referenceDiagnosticKey) ?? (transaction.docChanged ? DecorationSet.empty : current)
    },
    props: { decorations: state => referenceDiagnosticKey.getState(state) }
  });
}

/** Markers prove the Markdown-to-editor mapping without changing the live document. */
export function visualReferenceDecorations(doc: Node, text: string, items: readonly ReferenceDiagnostic[]): DecorationSet {
  const prefix = '\uE000loom-reference-';
  if (!items.length || text.includes(prefix)) return DecorationSet.empty;
  let marked = text;
  for (let index = items.length - 1; index >= 0; index--) {
    const { start, end } = items[index];
    marked = marked.slice(0, start) + `${prefix}${index}s\uE001` + marked.slice(start, end) + `${prefix}${index}e\uE001` + marked.slice(end);
  }
  try {
    const parsed = parseVisualMarkdown(marked, doc.type.schema);
    const markers: { from: number; to: number; index: number; edge: string }[] = [];
    parsed.descendants((node, position) => {
      if (!node.isText) return;
      for (const match of node.text!.matchAll(/\uE000loom-reference-(\d+)([se])\uE001/gu)) {
        markers.push({ from: position + match.index, to: position + match.index + match[0].length, index: Number(match[1]), edge: match[2] });
      }
    });
    if (markers.length !== items.length * 2) return DecorationSet.empty;
    const transform = EditorState.create({ doc: parsed }).tr;
    for (const marker of [...markers].reverse()) transform.delete(marker.from, marker.to);
    if (!transform.doc.eq(doc)) return DecorationSet.empty;
    const ranges = new Map<number, { from?: number; to?: number }>();
    for (const marker of markers) {
      const range = ranges.get(marker.index) ?? {};
      const position = transform.mapping.map(marker.from);
      if (marker.edge === 's') range.from = position; else range.to = position;
      ranges.set(marker.index, range);
    }
    return DecorationSet.create(doc, items.flatMap((item, index) => {
      const range = ranges.get(index);
      return range?.from !== undefined && range.to !== undefined && range.from < range.to
        ? [Decoration.inline(range.from, range.to, { class: 'reference-unavailable', title: item.message, 'aria-description': item.message })] : [];
    }));
  } catch { return DecorationSet.empty; }
}
