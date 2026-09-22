import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { test } from 'vitest';
import { verifyBranchBody } from './branchBodyProof';
import { emptyAutocompleteRetryLedger, planAutocompleteRetry } from './autocompleteRetry';
import { evaluateInlineSuggestionFamily, type InlineSuggestionState } from './inlineSuggestionFamily';
import type { BranchBody, BranchCard } from './types';

function fixture(text = ' world again'): { state: InlineSuggestionState; bodies: BranchBody[] } {
  const blob = createHash('sha256').update(text).digest('hex');
  const branches: BranchCard[] = Array.from({ length: 4 }, (_, i) => ({
    run_id: `run-${i}`, branch_id: `branch-${i}`, weave_command_id: 'family-1',
    document_id: 'document-1', candidate_id: `candidate-${i}`, source_revision_id: 'revision-1',
    target_start_byte: 5, target_end_byte: 5, text: '', output_blob_id: blob,
    output_byte_len: Buffer.byteLength(text), status: 'ready', seed: String(i), model_id: 'model-1',
    selection: null, error: null, error_truncated: false, created_at_unix_ms: i + 1
  }));
  const bodies: BranchBody[] = branches.map(branch => ({
    ...branch, candidate_id: branch.candidate_id!, output_blob_id: blob,
    seed: branch.seed!,
    model_id: branch.model_id!,
    byte_len: Buffer.byteLength(text), text
  }));
  return { bodies, state: {
    branches, authoritativeFamilyId: 'family-1', requireExplicitFamily: true,
    verifiedBodyByRun: {}, liveTextByRun: Object.fromEntries(branches.map(b => [b.run_id, ' '])),
    liveTextSequenceByRun: Object.fromEntries(branches.map(b => [b.run_id, '7'])),
    currentModel: { model_id: 'model-1' },
    document: { summary: { document_id: 'document-1', relative_path: 'Untitled.md', title: 'Untitled',
      kind: 'prose', revision_id: 'revision-1', active_blob_id: 'source-blob', word_count: 1,
      externally_modified: false }, visible_blob_id: 'source-blob', text: 'hello', transient_draft: null },
    suggestionsEnabled: true, promotionReady: true, dismissedCandidateIds: [],
    unpresentableVisualKeys: [], manuscriptText: 'hello', sourceNewline: null
  } };
}
function evaluation(state: InlineSuggestionState) {
  return evaluateInlineSuggestionFamily(5, 'source', state);
}
function retry(state: InlineSuggestionState, ledger = emptyAutocompleteRetryLedger()) {
  return planAutocompleteRetry(ledger, { disposition: evaluation(state).disposition,
    budgetKey: 'session:document:revision:edit', activeBranchCount: 0,
    weaveStarting: false, maximumRetries: 1 });
}
async function hydrate(state: InlineSuggestionState, bodies: BranchBody[], indices = [0, 1, 2, 3]) {
  for (const index of indices) {
    const branch = state.branches[index];
    const body = bodies[index];
    const proof = await verifyBranchBody(body, branch);
    assert.ok(proof, 'fixture passes the real SHA-256 and exact-identity verifier');
    state.verifiedBodyByRun[branch.run_id] = proof;
    branch.text = body.text;
  }
}

test('terminal summaries with unusable live partials wait for body authority without spending retry budget', () => {
  const { state } = fixture();
  assert.deepEqual(evaluation(state).phase, { kind: 'awaiting_hydration', runIds: ['run-0', 'run-1', 'run-2', 'run-3'] });
  const ledger = emptyAutocompleteRetryLedger();
  for (let i = 0; i < 20; i++) {
    const decision = retry(state, ledger);
    assert.equal(decision.kind, 'none');
    assert.equal(decision.ledger, ledger);
  }
});

test('the last unverified member prevents a partially hydrated family from triggering replacement', async () => {
  const { state, bodies } = fixture();
  await hydrate(state, bodies, [0, 1, 2]);
  assert.deepEqual(evaluation(state).phase, { kind: 'awaiting_hydration', runIds: ['run-3'] });
  assert.equal(retry(state).kind, 'none');
  await hydrate(state, bodies, [3]);
  assert.equal(evaluation(state).phase.kind, 'ready');
  assert.equal(evaluation(state).candidates.length, 4);
  assert.equal(evaluation(state).candidates[0].text, ' world again');
  assert.equal(retry(state).kind, 'none');
});

test('a complete authorized live family remains visible while terminal bodies are fetched', () => {
  const { state } = fixture();
  for (const branch of state.branches) state.liveTextByRun[branch.run_id] = ' world';
  const result = evaluation(state);
  assert.equal(result.phase.kind, 'ready');
  assert.equal(result.candidates.length, 4);
  assert.equal(result.candidates[0].presentationKey, 'stream:run-0:7');
  assert.equal(retry(state).kind, 'none');
});

for (const text of ['', ' ']) test(`verified unusable output ${JSON.stringify(text)} exhausts once instead of waiting for another proof`, async () => {
  const { state, bodies } = fixture(text);
  await hydrate(state, bodies);
  state.liveTextByRun = {};
  state.liveTextSequenceByRun = {};
  assert.equal(evaluation(state).phase.kind, 'terminal_shortfall');
  const first = retry(state);
  assert.equal(first.kind, 'schedule');
  assert.equal(retry(state, first.ledger).kind, 'none');
});

test('a proof for a different immutable blob does not end hydration', async () => {
  const { state, bodies } = fixture();
  await hydrate(state, bodies);
  state.branches[3].output_blob_id = 'a'.repeat(64);
  state.branches[3].text = '';
  assert.deepEqual(evaluation(state).phase, { kind: 'awaiting_hydration', runIds: ['run-3'] });
  assert.equal(retry(state).kind, 'none');
});

test('stale document authority cannot make either streamed or hydrated bytes visible', async () => {
  const { state, bodies } = fixture();
  await hydrate(state, bodies);
  state.document!.summary.revision_id = 'revision-2';
  assert.equal(evaluation(state).phase.kind, 'stale_scope');
  assert.deepEqual(evaluation(state).candidates, []);
  assert.equal(retry(state).kind, 'none');
});
