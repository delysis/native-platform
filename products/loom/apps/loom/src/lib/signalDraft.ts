import { signalRequest, type SignalCommand, type SignalDraft, type SignalEvent, type SignalSendAttempt } from './signal';
import { newUlid } from './ulid';

/** One writer, durable optimistic versions, exact retries after an uncertain
 * reply. This owner outlives the pane so hiding it never discards authored text. */
export class SignalDraftEditor {
  conversation = '';
  text = '';
  pending: SignalSendAttempt | null = null;
  private saved: SignalDraft = { version: 0, text: '', pending: null };
  private uncertain: { command: SignalCommand; id: string } | null = null;
  private running: Promise<void> | null = null;
  private sending: Promise<void> | null = null;
  constructor(private readonly request = signalRequest) {}

  async open(conversation: string): Promise<void> {
    if (this.sending) await this.sending;
    await this.flush();
    if (this.conversation === conversation) return;
    const result = await this.request({ kind: 'draft', conversation_id: conversation });
    this.expectDraft(result, conversation);
    if (result.kind !== 'draft') return;
    this.conversation = conversation;
    this.saved = result.draft;
    this.text = result.draft.text;
    this.pending = result.draft.pending;
  }

  async withSend(operation: () => Promise<void>): Promise<void> {
    if (this.sending) return this.sending;
    const sending = operation(); this.sending = sending;
    try { await sending; } finally { if (this.sending === sending) this.sending = null; }
  }

  async flush(): Promise<void> {
    if (this.running) return this.running;
    const operation = this.drain(); this.running = operation;
    try { await operation; } finally { if (this.running === operation) this.running = null; }
  }

  private async drain(): Promise<void> {
    if (!this.conversation) return;
    while (this.uncertain || this.text !== this.saved.text || JSON.stringify(this.pending) !== JSON.stringify(this.saved.pending)) {
      if (!this.uncertain && new TextEncoder().encode(this.text).length > 64 * 1024) {
        throw new Error('This draft exceeds 64 KiB. Shorten it before closing the pane. Your text is still here.');
      }
      const attempt = this.uncertain ?? { id: newUlid(), command: {
        kind: 'save_draft' as const, conversation_id: this.conversation,
        expected_version: this.saved.version, text: this.text, pending: this.pending,
      } };
      this.uncertain = attempt;
      const result = await this.request(attempt.command, attempt.id);
      this.expectDraft(result, this.conversation);
      if (result.kind !== 'draft') return;
      this.saved = result.draft; this.uncertain = null;
    }
  }

  private expectDraft(result: SignalEvent, conversation: string): void {
    if (result.kind === 'failure') throw new Error(result.message);
    if (result.kind !== 'draft' || result.conversation_id !== conversation) throw new Error('Signal returned an unrelated draft. Your text is still here.');
  }
}
