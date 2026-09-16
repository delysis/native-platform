import { describe, expect, it, vi } from 'vitest';
import { ComputeSharing, type ComputeGrant, type ComputeGrantReview, type ComputeHostSnapshot, type ComputeScope } from './compute';

const scope: ComputeScope = { projectId: 'project', sessionId: 'session', cabalId: 'cabal' };
function review(): ComputeGrantReview {
  return { epoch: 2, modelName: 'My model', modalities: 'text', memberName: 'Bob', request: {
    id: 'grant', member_key: 'bob', roster_hash: 'roster', model_fingerprint: 'model',
    jobs: 16, max_output_tokens: 256, max_seconds: 30,
  } };
}
function grant(value = review()): ComputeGrant {
  return { id: value.request.id, cabal: scope.cabalId, epoch: value.epoch, peer: value.request.member_key,
    model: { media: [], name: value.modelName, fingerprint: value.request.model_fingerprint }, jobs: value.request.jobs,
    max_output_tokens: value.request.max_output_tokens, max_seconds: value.request.max_seconds };
}
function snapshot(grants: ComputeGrant[] = []): ComputeHostSnapshot {
  return { model: grant().model, idle: true, problem: null, grants: grants.map(grant => ({ grant, jobs_remaining: grant.jobs, current: true })) };
}
function fixture() {
  const api = { snapshot: vi.fn(async () => snapshot()), grant: vi.fn(async () => grant()), revoke: vi.fn(async () => {}) };
  return { api, sharing: new ComputeSharing(api) };
}

describe('app-owned compute grants', () => {
  it('reconciles a lost grant reply through read-only status after the pane disappears', async () => {
    const { api, sharing } = fixture();
    api.grant.mockRejectedValueOnce(new Error('reply lost'));
    const observer = vi.fn(); const hide = sharing.subscribe(observer);
    await expect(sharing.grant(scope, review())).rejects.toThrow('reply lost'); hide();
    expect(sharing.attempt(scope)?.request.id).toBe('grant');
    api.snapshot.mockResolvedValue(snapshot([grant()]));
    await sharing.snapshot({ ...scope, sessionId: 'reopened' });
    expect(sharing.attempt(scope)).toBeNull();
    expect(api.grant).toHaveBeenCalledTimes(1);
    expect(api.revoke).not.toHaveBeenCalled();
  });

  it('freezes the reviewed values and reuses the exact ID when retrying in a new session', async () => {
    const { api, sharing } = fixture(); const selected = review();
    api.grant.mockRejectedValueOnce(new Error('uncertain'));
    await expect(sharing.grant(scope, selected)).rejects.toThrow();
    selected.request.jobs = 256; selected.request.member_key = 'carol';
    await sharing.snapshot(scope); // Absence cannot discard an uncertain identity.
    await expect(sharing.grant(scope, selected)).rejects.toThrow('pending grant');
    await sharing.grant({ ...scope, sessionId: 'reopened' });
    expect(api.grant.mock.calls).toEqual([[scope, review().request], [{ ...scope, sessionId: 'reopened' }, review().request]]);
    expect(sharing.attempt(scope)).toBeNull();
  });

  it('keeps the exact pending grant until revocation itself is confirmed', async () => {
    const { api, sharing } = fixture(); api.grant.mockRejectedValue(new Error('uncertain'));
    await expect(sharing.grant(scope, review())).rejects.toThrow();
    api.revoke.mockRejectedValueOnce(new Error('disk full'));
    await expect(sharing.revoke(scope, 'grant')).rejects.toThrow('disk full');
    expect(sharing.attempt(scope)?.request.id).toBe('grant');
    await sharing.revoke(scope, 'grant');
    expect(sharing.attempt(scope)).toBeNull();
    expect(api.grant).toHaveBeenCalledTimes(1);
    expect(api.revoke.mock.calls).toEqual([[scope, 'grant'], [scope, 'grant']]);
  });

  it('does not settle an unrelated receipt or transplant pending authority into another workspace', async () => {
    const { api, sharing } = fixture(); api.grant.mockResolvedValue({ ...grant(), cabal: 'other' });
    await expect(sharing.grant(scope, review())).rejects.toThrow('unrelated');
    api.snapshot.mockResolvedValue(snapshot([{ ...grant(), epoch: 3 }]));
    await sharing.snapshot(scope);
    const other = { ...scope, projectId: 'different-project' };
    expect(sharing.attempt(other)).toBeNull();
    await expect(sharing.grant(other)).rejects.toThrow('Review a grant');
    expect(sharing.attempt(scope)?.request.id).toBe('grant');
  });

  it('keeps one writer through an in-flight change even with no pane observing it', async () => {
    const { api, sharing } = fixture();
    let finish!: (value: ComputeGrant) => void;
    api.grant.mockImplementation(() => new Promise(resolve => { finish = resolve; }));
    const hide = sharing.subscribe(() => {});
    const operation = sharing.grant(scope, review()); hide();
    await expect(sharing.revoke(scope, 'grant')).rejects.toThrow('still settling');
    await expect(sharing.grant(scope)).rejects.toThrow('still settling');
    expect(sharing.busy(scope)).toBe(true);
    finish(grant()); await operation;
    expect(sharing.busy(scope)).toBe(false); expect(sharing.attempt(scope)).toBeNull();
    expect(api.revoke).not.toHaveBeenCalled();
  });
});
