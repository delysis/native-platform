import { describe, expect, it } from 'vitest';
import { verifyBranchBody } from './branchBodyProof';
import type { BranchBody, BranchSummary } from './types';

const exactText = ' rain gathered under the station lamps.';
const exactDigest = '02efe1fe3a1301dbadd7f376733d734b8dd36711f0ed02324cd20033d28f3098';

function summary(overrides: Partial<BranchSummary> = {}): BranchSummary {
  return {
    run_id: 'run-1',
    branch_id: 'branch-1',
    document_id: 'document-1',
    candidate_id: 'candidate-1',
    source_revision_id: 'revision-1',
    target_start_byte: 9,
    target_end_byte: 9,
    output_blob_id: exactDigest,
    output_byte_len: new TextEncoder().encode(exactText).byteLength,
    status: 'ready',
    seed: '7',
    model_id: 'writer',
    selection: null,
    error: null,
    error_truncated: false,
    created_at_unix_ms: 1,
    ...overrides
  };
}

function body(overrides: Partial<BranchBody> = {}): BranchBody {
  return {
    run_id: 'run-1',
    branch_id: 'branch-1',
    document_id: 'document-1',
    candidate_id: 'candidate-1',
    source_revision_id: 'revision-1',
    target_start_byte: 9,
    target_end_byte: 9,
    seed: '7',
    model_id: 'writer',
    created_at_unix_ms: 1,
    output_blob_id: exactDigest,
    byte_len: new TextEncoder().encode(exactText).byteLength,
    text: exactText,
    ...overrides
  };
}

describe('verified branch body', () => {
  it('brands only text whose SHA-256 matches the immutable blob identity', async () => {
    const verified = await verifyBranchBody(body(), summary());
    expect(verified).toMatchObject({
      runId: 'run-1',
      branchId: 'branch-1',
      candidateId: 'candidate-1',
      sourceRevisionId: 'revision-1',
      targetStartByte: 9,
      blobId: exactDigest,
      text: exactText
    });
  });

  it('rejects same-length substituted bytes', async () => {
    const substituted = exactText.replace('rain', 'hail');
    expect(new TextEncoder().encode(substituted).byteLength).toBe(
      new TextEncoder().encode(exactText).byteLength
    );
    expect(await verifyBranchBody(body({ text: substituted }), summary())).toBeNull();
  });

  it('rejects noncanonical and cross-run identities before hashing', async () => {
    expect(await verifyBranchBody(body(), summary({ output_blob_id: 'not-a-digest' }))).toBeNull();
    expect(await verifyBranchBody(body({ run_id: 'run-2' }), summary())).toBeNull();
  });

  it('rejects cross-occurrence candidate, target, source, branch, and model substitution', async () => {
    expect(await verifyBranchBody(body({ candidate_id: 'candidate-2' }), summary())).toBeNull();
    expect(await verifyBranchBody(body({ branch_id: 'branch-2' }), summary())).toBeNull();
    expect(await verifyBranchBody(body({ source_revision_id: 'revision-2' }), summary())).toBeNull();
    expect(await verifyBranchBody(body({ target_start_byte: 8 }), summary())).toBeNull();
    expect(await verifyBranchBody(body({ target_end_byte: 10 }), summary())).toBeNull();
    expect(await verifyBranchBody(body({ model_id: 'other-writer' }), summary())).toBeNull();
  });
});
