import { describe, expect, it, vi } from 'vitest';
import { SignalDraftEditor } from './signalDraft';
import type { SignalCommand, SignalEvent } from './signal';

const saved = (text: string, version: number): SignalEvent => ({ kind: 'draft', conversation_id: 'friend', draft: { version, text, pending: null } });
describe('encrypted Signal draft writer', () => {
  it('preserves newer typing while retrying an uncertain write exactly', async () => {
    const request = vi.fn().mockResolvedValueOnce(saved('', 0))
      .mockRejectedValueOnce(new Error('lost response'))
      .mockResolvedValueOnce(saved('  Hello 🌻\r\n', 1)).mockResolvedValueOnce(saved('  Hello 🌻\r\nfriend', 2));
    const editor = new SignalDraftEditor(request);
    await editor.open('friend'); editor.text = '  Hello 🌻\r\n';
    await expect(editor.flush()).rejects.toThrow('lost response');
    editor.text += 'friend'; await editor.flush();
    expect(request.mock.calls[2]).toEqual(request.mock.calls[1]);
    expect(request.mock.calls[3][0]).toMatchObject({ expected_version: 1, text: '  Hello 🌻\r\nfriend' });
    expect(editor.text).toBe('  Hello 🌻\r\nfriend');
  });

  it('cannot switch the draft owner while its explicit send is settling', async () => {
    let finish!: () => void;
    const pending = new Promise<void>(resolve => finish = resolve);
    const request = vi.fn(async (command: SignalCommand): Promise<SignalEvent> => {
      if (command.kind === 'draft') return { kind: 'draft', conversation_id: command.conversation_id, draft: { version: 0, text: '', pending: null } };
      if (command.kind === 'save_draft') return { kind: 'draft', conversation_id: command.conversation_id, draft: { version: command.expected_version + 1, text: command.text, pending: command.pending } };
      if (command.kind === 'send') { await pending; return { kind: 'sent', conversation_id: command.conversation_id, timestamp: command.timestamp }; }
      throw new Error('Unexpected request');
    });
    const editor = new SignalDraftEditor(request); await editor.open('friend'); editor.text = 'Hello';
    const sending = editor.send();
    await vi.waitFor(() => expect(request.mock.calls.some(([command]) => command.kind === 'send')).toBe(true));
    const switching = editor.open('other');
    await Promise.resolve(); expect(editor.conversation).toBe('friend');
    finish(); await sending; await switching;
    expect(editor.conversation).toBe('other');
    expect(request.mock.calls.filter(([command]) => command.kind === 'save_draft').map(([command]) => command)).toEqual([
      expect.objectContaining({ conversation_id: 'friend', text: 'Hello', pending: expect.objectContaining({ text: 'Hello' }) }),
      expect.objectContaining({ conversation_id: 'friend', text: '', pending: null }),
    ]);
  });

  it('serializes conversation loads when a new pane opens before the old load returns', async () => {
    let finish!: (event: SignalEvent) => void;
    const request = vi.fn().mockImplementationOnce(() => new Promise(resolve => finish = resolve))
      .mockResolvedValueOnce({ kind: 'draft', conversation_id: 'other', draft: { version: 1, text: 'New friend', pending: null } });
    const editor = new SignalDraftEditor(request);
    const first = editor.open('friend'), second = editor.open('other');
    await vi.waitFor(() => expect(request).toHaveBeenCalledOnce());
    finish(saved('First friend', 1)); await first; await second; await editor.settle();
    expect(editor.conversation).toBe('other'); expect(editor.text).toBe('New friend');
  });

  it('checks an uncertain send without submitting it again', async () => {
    const request = vi.fn().mockResolvedValueOnce(saved('', 0))
      .mockImplementationOnce(async (command: Extract<SignalCommand, { kind: 'save_draft' }>) => ({ kind: 'draft', conversation_id: 'friend', draft: { version: 1, text: command.text, pending: command.pending } }))
      .mockRejectedValueOnce(new Error('Lost send reply'))
      .mockResolvedValueOnce({ kind: 'not_sent' }).mockResolvedValueOnce(saved('Hello', 2));
    const editor = new SignalDraftEditor(request); await editor.open('friend'); editor.text = 'Hello';
    await expect(editor.send()).rejects.toThrow('Lost send reply');
    const attempt = editor.pending;
    await expect(editor.send()).rejects.toThrow('Review');
    await editor.send(true);
    expect(request.mock.calls[3][0]).toEqual({ kind: 'check_send', attempt });
    expect(request.mock.calls.filter(([command]) => command.kind === 'send')).toHaveLength(1);
    expect(editor.pending).toBeNull(); expect(editor.text).toBe('Hello');
  });

  it('waits for a queued conversation even when the preceding load fails', async () => {
    let fail!: (error: Error) => void, finish!: (event: SignalEvent) => void;
    const request = vi.fn().mockImplementationOnce(() => new Promise((_resolve, reject) => fail = reject))
      .mockImplementationOnce(() => new Promise(resolve => finish = resolve));
    const editor = new SignalDraftEditor(request);
    const failed = editor.open('friend').catch(() => {}), opened = editor.open('other');
    let settled = false; const ready = editor.settle().then(() => settled = true);
    await vi.waitFor(() => expect(request).toHaveBeenCalledOnce()); fail(new Error('Load failed'));
    await vi.waitFor(() => expect(request).toHaveBeenCalledTimes(2)); expect(settled).toBe(false);
    finish({ kind: 'draft', conversation_id: 'other', draft: { version: 1, text: 'Other friend', pending: null } });
    await failed; await opened; await ready;
    expect(editor.conversation).toBe('other'); expect(editor.text).toBe('Other friend');
  });
});
