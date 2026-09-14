import { invoke } from '@tauri-apps/api/core';
import type { OpenDocument } from './types';

export interface SharedView { id: string; name: string; kind: 'prose' | 'verse'; deleted: boolean; text: string; heads: string[] }
export interface SharedDocument { shared: SharedView; local: OpenDocument }
export interface CabalSnapshot {
  id: string; name: string; my_key: string; orphaned_changes: number; removed_documents: number;
  roster_hash: string;
  roster: { payload: { owner: string; epoch: number; members: { key: string; name: string }[] } };
  peers: { cabal: string; key: string; connected: boolean; changed: boolean }[];
  documents: SharedDocument[];
  deleted_document_ids: string[];
  problems: { document_id: string | null; name: string; message: string }[];
  read_only: boolean;
}
export interface CabalEdit { document: string; client: string; basis: string[]; text: string }
export interface CabalEditReply extends SharedDocument { local_heads: string[] }

export function cabalSnapshot(projectId: string, sessionId: string, protectedDocumentId: string | null): Promise<CabalSnapshot | null> {
  return invoke('plugin:loom|cabal_snapshot', { projectId, sessionId, protectedDocumentId });
}
export function shareCabal(projectId: string, sessionId: string, name: string, displayName = 'Loom'): Promise<string> {
  return invoke('plugin:loom|cabal_share', { projectId, sessionId, name, displayName });
}
export function joinCabal(invitation: string, displayName = 'Loom'): Promise<string | null> {
  return invoke('plugin:loom|cabal_join', { invitation, displayName });
}
export function editCabal(projectId: string, sessionId: string, edit: CabalEdit): Promise<CabalEditReply> {
  return invoke('plugin:loom|cabal_edit', { projectId, sessionId, edit });
}
export function revokeCabalMember(projectId: string, sessionId: string, memberKey: string, rosterHash: string): Promise<void> {
  return invoke('plugin:loom|cabal_revoke', { projectId, sessionId, memberKey, rosterHash });
}
export function recoverCabalEdits(projectId: string, sessionId: string, edit: CabalEdit | null = null): Promise<{ paths: string[]; draft_path: string | null }> {
  return invoke('plugin:loom|cabal_recover', { projectId, sessionId, edit });
}

/** One editor's causal cursor. Edits queued during a round trip continue from
 * their own local heads; unseen remote text is never used as their basis. */
export class CabalEditor {
  readonly id: string;
  version = 0;
  private readonly client: string;
  private basis: string[];
  private baseText: string;
  private desired: string;
  private running: Promise<boolean> | null = null;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private deferred: CabalEditReply | null = null;
  private uncertain: CabalEdit | null = null;
  private disposed = false;
  private recovered = false;

  constructor(initial: SharedDocument, private readonly send: (edit: CabalEdit) => Promise<CabalEditReply>,
    private readonly apply: (document: SharedDocument) => boolean, private readonly composing: () => boolean,
    private readonly failure: (error: unknown) => void, client: string = crypto.randomUUID()) {
    this.id = initial.shared.id; this.client = client;
    this.basis = initial.shared.heads; this.baseText = initial.shared.text; this.desired = initial.shared.text;
  }

  change(text: string): void {
    if (this.disposed || this.recovered) return;
    this.desired = text; this.version++;
    if (!this.timer) this.timer = setTimeout(() => { this.timer = undefined; void this.flush(); }, 100);
  }

  receive(document: SharedDocument, expectedVersion: number): void {
    if (this.disposed || this.recovered || this.version !== expectedVersion || this.running || this.composing() || this.uncertain || this.desired !== this.baseText) return;
    this.accept(document);
  }

  private accept(document: SharedDocument): void {
    // A navigation or an editor transaction may begin during the round trip.
    // Advance the causal cursor only when that text actually reaches the editor.
    if (!this.apply(document)) return;
    this.basis = document.shared.heads;
    this.baseText = document.shared.text;
    this.desired = document.shared.text;
    this.deferred = null;
    this.version++;
  }

  async flush(): Promise<boolean> {
    if (this.timer) { clearTimeout(this.timer); this.timer = undefined; }
    if (this.running) return this.running;
    if (this.disposed || this.composing()) return false;
    if (this.recovered) return true;
    const operation = this.drain();
    this.running = operation;
    try { return await operation; }
    finally { if (this.running === operation) this.running = null; }
  }

  recoveryEdit(): CabalEdit | null {
    if (this.disposed || this.recovered || this.running || this.composing() || (!this.uncertain && this.desired === this.baseText)) return null;
    return { document: this.id, client: this.client, basis: [...this.basis], text: this.desired };
  }

  /** Call only after an explicit recovery action has durably retained this exact
   * text. This freezes the old editor so navigation can open the private copy. */
  acknowledgeRecovery(edit: CabalEdit): boolean {
    if (this.disposed || this.running || this.composing() || edit.document !== this.id || edit.client !== this.client || edit.text !== this.desired || edit.basis.join() !== this.basis.join()) return false;
    this.recovered = true;
    this.version++;
    return true;
  }

  private async drain(): Promise<boolean> {
    try {
      while (!this.disposed && !this.composing() && (this.uncertain || this.desired !== this.baseText)) {
        const edit = this.uncertain ?? { document: this.id, client: this.client, basis: this.basis, text: this.desired };
        this.uncertain = edit;
        const reply = await this.send(edit);
        if (this.disposed) return false;
        this.uncertain = null;
        this.basis = reply.local_heads;
        this.baseText = edit.text;
        this.deferred = reply;
        if (this.desired === edit.text && !this.composing()) this.accept(reply);
      }
      if (!this.composing() && this.deferred && this.desired === this.baseText) this.accept(this.deferred);
      return !this.disposed && !this.composing() && !this.uncertain && this.desired === this.baseText;
    } catch (error) { this.failure(error); return false; }
  }

  dispose(): void { this.disposed = true; if (this.timer) clearTimeout(this.timer); }
}
