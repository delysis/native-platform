import { invoke } from '@tauri-apps/api/core';

export interface ComputeModel { fingerprint: string; name: string }
export interface ComputeGrant {
  id: string; cabal: string; epoch: number; peer: string; model: ComputeModel;
  max_output_tokens: number; max_seconds: number; jobs: number;
}
export interface ComputeGrantStatus { grant: ComputeGrant; jobs_remaining: number; current: boolean }
export interface ComputeHostSnapshot {
  model: ComputeModel | null; idle: boolean; grants: ComputeGrantStatus[]; problem: string | null;
}
export interface ComputeScope { projectId: string; sessionId: string; cabalId: string }
export interface ComputeGrantRequest {
  id: string; member_key: string; roster_hash: string; model_fingerprint: string;
  max_output_tokens: number; max_seconds: number; jobs: number;
}
export interface ComputeGrantReview {
  request: ComputeGrantRequest; epoch: number; modelName: string; memberName: string;
}
export interface PendingComputeGrant extends ComputeGrantReview { projectId: string; cabalId: string }

export function computeHostSnapshot(scope: ComputeScope): Promise<ComputeHostSnapshot | null> {
  return invoke('plugin:loom|compute_host_snapshot', { projectId: scope.projectId, sessionId: scope.sessionId });
}
function grantCompute(scope: ComputeScope, request: ComputeGrantRequest): Promise<ComputeGrant> {
  return invoke('plugin:loom|compute_grant', { projectId: scope.projectId, sessionId: scope.sessionId, request });
}
function revokeCompute(scope: ComputeScope, grantId: string): Promise<void> {
  return invoke('plugin:loom|compute_revoke', { projectId: scope.projectId, sessionId: scope.sessionId, grantId });
}

/** The app owns uncertain grants, so hiding or replacing a pane cannot mint a
 * new retry ID. Native storage owns the committed grant across process restarts. */
export class ComputeSharing {
  private pending = new Map<string, PendingComputeGrant>();
  private running = new Set<string>();
  private observers = new Set<() => void>();
  constructor(private readonly api = { snapshot: computeHostSnapshot, grant: grantCompute, revoke: revokeCompute }) {}

  subscribe(observer: () => void): () => void {
    this.observers.add(observer); observer();
    return () => { this.observers.delete(observer); };
  }
  private changed(): void { for (const observer of this.observers) observer(); }
  attempt(scope: ComputeScope): PendingComputeGrant | null {
    const attempt = this.pending.get(scopeKey(scope));
    return attempt?.projectId === scope.projectId ? attempt : null;
  }
  busy(scope: ComputeScope): boolean { return this.running.has(scopeKey(scope)); }

  async snapshot(scope: ComputeScope): Promise<ComputeHostSnapshot | null> {
    const snapshot = await this.api.snapshot(scope);
    const attempt = this.attempt(scope);
    // Absence is not permission to forget an uncertain write. Only an exact
    // committed grant or an explicit revocation settles its identity.
    if (attempt && snapshot?.grants.some(item => matchesGrant(item.grant, attempt))) {
      this.pending.delete(scopeKey(scope)); this.changed();
    }
    return snapshot;
  }

  grant(scope: ComputeScope, review?: ComputeGrantReview): Promise<void> {
    return this.run(scope, async () => {
      let attempt = this.attempt(scope);
      if (attempt && review) throw new Error('Check the pending grant before making another.');
      if (!attempt) {
        if (!review) throw new Error('Review a grant before sharing compute.');
        if (this.pending.size >= 16) throw new Error('Settle a pending compute grant before making another.');
        // Copy the reviewed values before dispatch; UI edits cannot mutate a retry.
        attempt = { ...review, request: { ...review.request }, projectId: scope.projectId, cabalId: scope.cabalId };
        this.pending.set(scopeKey(scope), attempt); this.changed();
      }
      const grant = await this.api.grant(scope, attempt.request);
      if (!matchesGrant(grant, attempt)) throw new Error('Loom returned an unrelated grant. Check this grant before continuing.');
      if (this.pending.get(scopeKey(scope)) === attempt) this.pending.delete(scopeKey(scope));
    });
  }

  revoke(scope: ComputeScope, grantId: string): Promise<void> {
    return this.run(scope, async () => {
      await this.api.revoke(scope, grantId);
      if (this.attempt(scope)?.request.id === grantId) this.pending.delete(scopeKey(scope));
    });
  }

  private async run(scope: ComputeScope, operation: () => Promise<void>): Promise<void> {
    if (this.busy(scope)) throw new Error('The previous compute change is still settling.');
    this.running.add(scopeKey(scope)); this.changed();
    try { await operation(); }
    finally { this.running.delete(scopeKey(scope)); this.changed(); }
  }
}

function matchesGrant(grant: ComputeGrant, attempt: PendingComputeGrant): boolean {
  const request = attempt.request;
  return grant.id === request.id && grant.cabal === attempt.cabalId && grant.epoch === attempt.epoch
    && grant.peer === request.member_key && grant.model.fingerprint === request.model_fingerprint
    && grant.max_output_tokens === request.max_output_tokens && grant.max_seconds === request.max_seconds
    && grant.jobs === request.jobs;
}

function scopeKey(scope: ComputeScope): string { return `${scope.projectId}/${scope.cabalId}`; }

export interface PeerTarget { host: string; grant: ComputeGrant; roster_hash: string }
export interface PeerOffers { host: string; roster_hash: string; grants: ComputeGrant[] }
export function peerOffers(projectId: string, sessionId: string, host: string): Promise<PeerOffers> {
  return invoke('plugin:loom|compute_peer_offers', { projectId, sessionId, host });
}
