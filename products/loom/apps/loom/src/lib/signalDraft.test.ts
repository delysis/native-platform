import { describe, expect, it, vi } from 'vitest';
import { SignalDraftEditor } from './signalDraft';
import type { SignalEvent } from './signal';

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
    const request = vi.fn().mockResolvedValueOnce(saved('', 0));
    const editor = new SignalDraftEditor(request); await editor.open('friend');
    const sending = editor.withSend(() => pending);
    const switching = editor.open('friend');
    let switched = false; void switching.then(() => switched = true);
    await Promise.resolve(); expect(switched).toBe(false);
    finish(); await sending; await switching; expect(switched).toBe(true);
  });
});
