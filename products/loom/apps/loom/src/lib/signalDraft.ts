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
  private transition: Promise<unknown> | null = null;
  constructor(private readonly request = signalRequest) {}

  open(conversation: string): Promise<void> {
    return this.sequence(async () => {
      await this.flush();
      if (this.conversation === conversation) return;
      const result = await this.request({ kind: 'draft', conversation_id: conversation });
      this.expectDraft(result, conversation);
      if (result.kind !== 'draft') return;
      this.conversation = conversation;
      this.saved = result.draft;
      this.text = result.draft.text;
      this.pending = result.draft.pending;
    });
  }

  /** A newly mounted pane must attach after the previous pane's operations. */
  async settle(): Promise<void> {
    let failure: unknown;
    do {
      while (this.transition) {
        try { await this.transition; failure = undefined; }
        catch (error) { failure = error; }
      }
      await this.flush();
    } while (this.transition);
    if (failure !== undefined) throw failure;
  }

  private async sequence<T>(operation: () => Promise<T>): Promise<T> {
    const previous = this.transition;
    const next = (async () => {
      try { await previous; }
      catch { /* Each caller owns its failure; flush retries uncertain writes. */ }
      return operation();
    })();
    this.transition = next;
    try { return await next; } finally { if (this.transition === next) this.transition = null; }
  }

  /** Settlement belongs to the durable writer, even after its pane disappears. */
  send(check = false): Promise<SignalEvent> {
    return this.sequence(async () => {
      if (!this.conversation || (!check && (!this.text.trim() || this.pending)) || (check && !this.pending)) {
        throw new Error('Review this conversation and its pending send first.');
      }
      const attempt = check ? this.pending! : { id: newUlid(), conversation: this.conversation, text: this.text, timestamp: Date.now() };
      this.pending = attempt;
      await this.flush();
      const result = check
        ? await this.request({ kind: 'check_send', attempt })
        : await this.request({ kind: 'send', conversation_id: attempt.conversation, text: attempt.text, timestamp: attempt.timestamp }, attempt.id);
      if (result.kind === 'sent') {
        if (result.conversation_id !== attempt.conversation || result.timestamp !== attempt.timestamp) throw new Error('Signal returned an unrelated send receipt. Check this send before continuing.');
        if (this.text === attempt.text) this.text = '';
        this.pending = null;
        await this.flush();
      } else if (check && result.kind === 'not_sent') {
        this.pending = null;
        await this.flush();
      } else if (result.kind !== 'failure') throw new Error('Signal did not confirm this send. Check it before continuing.');
      return result;
    });
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
