import { describe, expect, it, vi } from 'vitest';
import { CabalEditor, type CabalEditReply, type SharedDocument } from './cabal';

function document(text: string, head: string): SharedDocument {
  return { shared: { id: 'shared', name: 'Draft.md', text, heads: [head] }, local: {
    summary: { document_id: 'local', relative_path: 'Draft.md', title: 'Draft', kind: 'prose', revision_id: head, active_blob_id: head, word_count: 1, externally_modified: false },
    text, visible_blob_id: head, transient_draft: null,
  } };
}

function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(done => resolve = done); return { promise, resolve }; }

describe('one editor causal cursor', () => {
  it('bases queued typing on its own acknowledged heads, then adopts both authors', async () => {
    const first = deferred<CabalEditReply>();
    const send = vi.fn().mockReturnValueOnce(first.promise).mockResolvedValueOnce({ ...document('Alice Bob!', 'merged-two'), local_heads: ['local-two'] });
    const apply = vi.fn().mockReturnValue(true), failure = vi.fn();
    const editor = new CabalEditor(document('A', 'initial'), send, apply, () => false, failure, 'device');
    editor.change('Alice');
    const flush = editor.flush();
    editor.change('Alice!');
    first.resolve({ ...document('Alice Bob', 'merged-one'), local_heads: ['local-one'] });
    expect(await flush).toBe(true);
    expect(send.mock.calls[1][0]).toMatchObject({ basis: ['local-one'], text: 'Alice!' });
    expect(apply).toHaveBeenCalledTimes(1);
    expect(apply.mock.calls[0][0].shared.text).toBe('Alice Bob!');
    expect(failure).not.toHaveBeenCalled(); editor.dispose();
  });

  it('retries the same uncertain edit before accepting newer input', async () => {
    const send = vi.fn().mockRejectedValueOnce(new Error('lost reply')).mockResolvedValueOnce({ ...document('AB', 'two'), local_heads: ['two'] });
    const editor = new CabalEditor(document('A', 'one'), send, () => true, () => false, vi.fn(), 'device');
    editor.change('AB'); expect(await editor.flush()).toBe(false);
    expect(await editor.flush()).toBe(true);
    expect(send.mock.calls[1][0]).toEqual(send.mock.calls[0][0]); editor.dispose();
  });

  it('defers external text through composition and rejects late snapshots', async () => {
    let composing = false;
    const pending = deferred<CabalEditReply>();
    const apply = vi.fn().mockReturnValue(true);
    const editor = new CabalEditor(document('A', 'one'), () => pending.promise, apply, () => composing, vi.fn(), 'device');
    const before = editor.version;
    editor.change('AB'); const flush = editor.flush(); composing = true;
    pending.resolve({ ...document('AB remotely', 'merged'), local_heads: ['local'] });
    expect(await flush).toBe(false); expect(apply).not.toHaveBeenCalled();
    editor.receive(document('old snapshot', 'old'), before); expect(apply).not.toHaveBeenCalled();
    composing = false; expect(await editor.flush()).toBe(true);
    expect(apply.mock.calls[0][0].shared.text).toBe('AB remotely'); editor.dispose();
  });
});
