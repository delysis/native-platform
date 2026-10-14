import { describe, expect, it } from 'vitest';
import type { BranchCard } from './types';
import { verifyBranchBody, type VerifiedBranchBody } from './branchBodyProof';
import { verifiedGhostSuggestion } from './ghostSuggestion';

function branch(overrides: Partial<BranchCard> = {}): BranchCard {
  const text = ' rain.\n\nThen light.';
  return {
    run_id: 'run-1',
    branch_id: 'branch-1',
    weave_command_id: null,
    document_id: 'document-1',
    candidate_id: 'candidate-1',
    source_revision_id: 'revision-1',
    target_start_byte: 9,
    target_end_byte: 9,
    text,
    output_blob_id: 'blob-1',
    output_byte_len: new TextEncoder().encode(text).byteLength,
    status: 'ready',
    seed: '7',
    model_id: 'model-1',
    selection: null,
    error: null,
    error_truncated: false,
    created_at_unix_ms: 1,
    ...overrides
  };
}

async function digestText(text: string): Promise<string> {
  const bytes = new TextEncoder().encode(text);
  const owned = new Uint8Array(bytes.byteLength);
  owned.set(bytes);
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', owned.buffer));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

async function hydratedBranch(
  overrides: Partial<BranchCard> = {}
): Promise<{ branch: BranchCard; body: VerifiedBranchBody }> {
  const candidate = branch(overrides);
  const digest = await digestText(candidate.text);
  candidate.output_blob_id = digest;
  candidate.output_byte_len = new TextEncoder().encode(candidate.text).byteLength;
  const body = await verifyBranchBody({
    run_id: candidate.run_id,
    branch_id: candidate.branch_id,
    document_id: 'document-1',
    candidate_id: candidate.candidate_id!,
    source_revision_id: candidate.source_revision_id,
    target_start_byte: candidate.target_start_byte,
    target_end_byte: candidate.target_end_byte,
    seed: candidate.seed!,
    model_id: candidate.model_id!,
    created_at_unix_ms: candidate.created_at_unix_ms,
    output_blob_id: digest,
    byte_len: candidate.output_byte_len,
    text: candidate.text
  }, candidate);
  if (!body) throw new Error('test branch body did not verify');
  return { branch: candidate, body };
}

describe('verifiedGhostSuggestion', () => {
  it('returns exact text only after immutable branch-body hydration', async () => {
    const hydrated = await hydratedBranch();
    expect(verifiedGhostSuggestion(hydrated.branch, hydrated.body)).toEqual({
      candidateId: 'candidate-1',
      presentationKey: `candidate-1:${hydrated.body.blobId}`,
      targetByte: 9,
      text: ' rain.\n\nThen light.'
    });
  });

  it('fails closed for live text, identity mismatch, length mismatch, or non-ready state', async () => {
    const hydrated = await hydratedBranch();
    const other = await hydratedBranch({ run_id: 'run-other', candidate_id: 'candidate-other' });
    expect(verifiedGhostSuggestion(branch(), undefined)).toBeNull();
    expect(verifiedGhostSuggestion(hydrated.branch, other.body)).toBeNull();
    expect(verifiedGhostSuggestion({ ...hydrated.branch, output_byte_len: 1 }, hydrated.body)).toBeNull();
    expect(verifiedGhostSuggestion({ ...hydrated.branch, status: 'generating' }, hydrated.body)).toBeNull();
  });

  it('measures UTF-8 bytes rather than JavaScript code units', async () => {
    const text = ' 🌧️';
    const hydrated = await hydratedBranch({ text });
    expect(verifiedGhostSuggestion(hydrated.branch, hydrated.body)?.text)
      .toBe(text);
    expect(verifiedGhostSuggestion({ ...hydrated.branch, output_byte_len: text.length }, hydrated.body))
      .toBeNull();
  });
});
