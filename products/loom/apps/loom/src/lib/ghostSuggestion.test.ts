import { describe, expect, it } from 'vitest';
import type { BranchCard } from './types';
import { verifyBranchBody, type VerifiedBranchBody } from './branchBodyProof';
import {
  autocompleteDisposition,
  ghostReviewAffordance,
  selectVerifiedGhostSuggestion,
  verifiedGhostSuggestion,
  visibleVerifiedGhostSuggestion
} from './ghostSuggestion';

describe('ghostReviewAffordance', () => {
  it('keeps a compact readable review action for one active suggestion', () => {
    expect(ghostReviewAffordance(true, 1)).toEqual({
      visible: true,
      label: 'Review',
      ariaLabel: 'Review the current writing suggestion'
    });
  });

  it('describes additional and non-inline alternatives without empty controls', () => {
    expect(ghostReviewAffordance(true, 3)).toEqual({
      visible: true,
      label: '2 more',
      ariaLabel: 'Review the current writing suggestion and 2 more alternatives'
    });
    expect(ghostReviewAffordance(false, 1)).toEqual({
      visible: true,
      label: '1 alternative',
      ariaLabel: 'Review 1 writing alternative'
    });
    expect(ghostReviewAffordance(false, 0)).toEqual({
      visible: false,
      label: '',
      ariaLabel: ''
    });
  });
});

function branch(overrides: Partial<BranchCard> = {}): BranchCard {
  const text = ' rain.\n\nThen light.';
  return {
    run_id: 'run-1',
    branch_id: 'branch-1',
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

describe('visibleVerifiedGhostSuggestion', () => {
  it('exposes menu and announcement state only for the child-rendered identity', async () => {
    const hydrated = await hydratedBranch();
    const suggestion = verifiedGhostSuggestion(hydrated.branch, hydrated.body);
    expect(visibleVerifiedGhostSuggestion(suggestion, '')).toBeNull();
    expect(visibleVerifiedGhostSuggestion(suggestion, 'candidate-1:another-blob')).toBeNull();
    expect(visibleVerifiedGhostSuggestion(
      suggestion,
      `candidate-1:${hydrated.body.blobId}`
    )).toBe(suggestion);
  });
});

describe('selectVerifiedGhostSuggestion', () => {
  it('reacts to an explicit immutable-body and caret snapshot', async () => {
    const hydrated = await hydratedBranch();
    const waiting = {
      active: true,
      branches: [hydrated.branch],
      verifiedBodyByRun: {},
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [],
      targetByte: 9
    };
    expect(selectVerifiedGhostSuggestion(waiting)).toBeNull();
    expect(selectVerifiedGhostSuggestion({
      ...waiting,
      verifiedBodyByRun: { 'run-1': hydrated.body }
    })).toEqual({
      candidateId: 'candidate-1',
      presentationKey: `candidate-1:${hydrated.body.blobId}`,
      targetByte: 9,
      text: ' rain.\n\nThen light.'
    });
  });

  it('fails closed away from the exact boundary or after dismissal', async () => {
    const hydrated = await hydratedBranch();
    const selection = {
      active: true,
      branches: [hydrated.branch],
      verifiedBodyByRun: { 'run-1': hydrated.body },
      dismissedCandidateIds: [] as string[],
      unpresentablePresentationKeys: [] as string[],
      targetByte: 9
    };
    expect(selectVerifiedGhostSuggestion({ ...selection, targetByte: 8 })).toBeNull();
    expect(selectVerifiedGhostSuggestion({
      ...selection,
      dismissedCandidateIds: ['candidate-1']
    })).toBeNull();
    expect(selectVerifiedGhostSuggestion({ ...selection, active: false })).toBeNull();
  });

  it('keeps degenerate model loops in provenance without presenting them', async () => {
    const loop = ` She ${'her '.repeat(24)}`;
    const hydrated = await hydratedBranch({ text: loop });
    expect(selectVerifiedGhostSuggestion({
      active: true,
      branches: [hydrated.branch],
      verifiedBodyByRun: { 'run-1': hydrated.body },
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [],
      targetByte: 9
    })).toBeNull();
  });

  it('skips a hydrated c630-shaped loop and selects the next exact branch', async () => {
    const loop = ` ${'Be'.repeat(180)}[image]\n\nS`;
    const clean = ' Beyond the wet glass, a bicycle bell answered.';
    const loopHydrated = await hydratedBranch({ text: loop });
    const cleanHydrated = await hydratedBranch({
      run_id: 'run-2',
      branch_id: 'branch-2',
      candidate_id: 'candidate-2',
      text: clean
    });

    expect(selectVerifiedGhostSuggestion({
      active: true,
      branches: [loopHydrated.branch, cleanHydrated.branch],
      verifiedBodyByRun: {
        'run-1': loopHydrated.body,
        'run-2': cleanHydrated.body
      },
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [],
      targetByte: 9
    })).toEqual({
      candidateId: 'candidate-2',
      presentationKey: `candidate-2:${cleanHydrated.body.blobId}`,
      targetByte: 9,
      text: clean
    });
    expect(loopHydrated.branch.text).toBe(loop);
    expect(loopHydrated.branch.output_blob_id).toBe(loopHydrated.body.blobId);
  });

  it('types a fully hydrated rejected family as exhausted instead of ready', async () => {
    const repeatedUpper = `\.\n\n${'The platform smelled of wet iron. '.repeat(18)}`.trimEnd();
    const repeatedLower = `\.\n\n${'the platform smelled of wet iron.\n\n'.repeat(16)}`.trimEnd();
    const hydrated = await Promise.all([
      hydratedBranch({
        run_id: 'run-upper',
        candidate_id: 'candidate-upper',
        text: repeatedUpper
      }),
      hydratedBranch({
        run_id: 'run-lower',
        candidate_id: 'candidate-lower',
        text: repeatedLower
      }),
      hydratedBranch({
        run_id: 'run-period',
        candidate_id: 'candidate-period',
        text: '.'
      })
    ]);
    const cards = hydrated.map((item) => item.branch);
    const disposition = autocompleteDisposition({
      active: true,
      branches: cards,
      verifiedBodyByRun: {
        'run-upper': hydrated[0].body,
        'run-lower': hydrated[1].body,
        'run-period': hydrated[2].body
      },
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [],
      targetByte: 9
    });
    expect(disposition).toEqual({
      kind: 'exhausted',
      candidates: [
        { candidateId: 'candidate-upper', reason: 'repetition' },
        { candidateId: 'candidate-lower', reason: 'repetition' },
        { candidateId: 'candidate-period', reason: 'too_short' }
      ]
    });
  });

  it('distinguishes immutable-body hydration from an exhausted family', () => {
    expect(autocompleteDisposition({
      active: true,
      branches: [branch()],
      verifiedBodyByRun: {},
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [],
      targetByte: 9
    })).toEqual({ kind: 'awaiting_hydration', runIds: ['run-1'] });
  });

  it('keeps presentation compatibility inside the typed disposition', async () => {
    const incompatible = await hydratedBranch({ text: ' rain\rbreak' });
    expect(autocompleteDisposition({
      active: true,
      branches: [incompatible.branch],
      verifiedBodyByRun: { 'run-1': incompatible.body },
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [],
      targetByte: 9,
      presentationCompatible: (text) => !text.includes('\r')
    })).toEqual({
      kind: 'exhausted',
      candidates: [{ candidateId: 'candidate-1', reason: 'unpresentable' }]
    });
  });

  it('skips a surface-rejected presentation and selects the next exact branch', async () => {
    const first = await hydratedBranch({
      run_id: 'run-first',
      candidate_id: 'candidate-first',
      text: ' first'
    });
    const second = await hydratedBranch({
      run_id: 'run-second',
      candidate_id: 'candidate-second',
      text: ' second'
    });

    expect(autocompleteDisposition({
      active: true,
      branches: [first.branch, second.branch],
      verifiedBodyByRun: {
        'run-first': first.body,
        'run-second': second.body
      },
      dismissedCandidateIds: [],
      unpresentablePresentationKeys: [`candidate-first:${first.body.blobId}`],
      targetByte: 9
    })).toEqual({
      kind: 'available',
      suggestion: {
        candidateId: 'candidate-second',
        presentationKey: `candidate-second:${second.body.blobId}`,
        targetByte: 9,
        text: ' second'
      }
    });
  });
});
