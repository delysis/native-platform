import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { branchIsActionableOnShelf } from './branchShelf';
import { mergeNewestPage } from './branchPaging';
import { verifyBranchBody, type VerifiedBranchBody } from './branchBodyProof';
import {
  armCompletionGeneration,
  completionGenerationIsArmed
} from './completionGenerationIntent';
import {
  automaticCompletionLifecycle,
  retainsScheduledCompletion,
  type CompletionLifecycleInput
} from './completionLifecycle';
import {
  completionPresentation,
  completionSessionContextKey,
  consumeCompletionText,
  startCompletionSession,
  synchronizeCompletionCandidates
} from './completionSession';
import { exactVerifiedSuggestionFamily } from './ghostSuggestion';
import { inlineSuggestionFamily } from './inlineSuggestionFamily';
import type {
  BranchBody,
  BranchCard,
  DesktopGenerationEnvelope,
  GenerationEventKind,
  ModelCapabilitySummary,
  OpenDocument
} from './types';
import { generationEventBelongsToScope, type GenerationScope } from './weaveSafety';

type TerminalStatus = 'ready' | 'failed' | 'cancelled';

interface CompletionRecoveryFixture {
  schema_version: number;
  fixture_id: string;
  evidence_class: string;
  source: {
    relative_path: string;
    revision_text: string;
    revision_id: string;
    visible_blob_id: string;
    caret_byte: number;
  };
  autosave: {
    revision_text: string;
    revision_id: string;
    visible_blob_id: string;
    accepted_text: string;
  };
  models: {
    initial: string;
    replacement: string;
  };
  interaction: {
    project_session_id: string;
    document_id: string;
    document_epoch: number;
    edit_version: number;
    post_composition_edit_version: number;
  };
  family: Array<{
    run_id: string;
    branch_id: string;
    seed: number;
    terminal: {
      status: TerminalStatus;
      candidate_id: string | null;
      text: string | null;
      sha256: string | null;
      error: string | null;
    };
  }>;
  primary_event_stream: Array<{
    sequence: number;
    event: 'queued' | 'generating' | 'text_delta' | 'candidate_ready' | 'terminal';
    text?: string;
    candidate_id?: string;
    status?: 'completed';
  }>;
  delivery_cases: Array<{
    case_id: string;
    event_indexes: number[];
    expected_applied_sequences: number[];
    expected_event_projection: {
      status: BranchCard['status'];
      candidate_id: string | null;
      text: string;
    };
  }>;
}

const fixture = JSON.parse(readFileSync(
  new URL('../../../../fixtures/compat/completion-recovery-v1.json', import.meta.url),
  'utf8'
)) as CompletionRecoveryFixture;

const scope: GenerationScope = {
  projectId: 'project-1',
  sessionId: fixture.interaction.project_session_id,
  documentId: fixture.interaction.document_id
};

function model(modelId = fixture.models.initial): ModelCapabilitySummary {
  return {
    model_id: modelId,
    display_name: modelId,
    local: true,
    loaded: true,
    chat: false,
    completion: true,
    fill_in_middle: false,
    output_tokens: true,
    logprobs: false,
    model_path: `/models/${modelId.replace('/', '-')}.gguf`,
    file_bytes: 1,
    header_verified: true,
    architecture: 'test',
    context_tokens: 4096,
    model_sha256: null,
    projector_present: false,
    media_kinds: [],
    policy_candidate: null,
    policy_verified: null,
    tested_profile: null
  };
}

function openDocument(
  revisionId = fixture.source.revision_id,
  visibleBlobId = fixture.source.visible_blob_id,
  text = fixture.source.revision_text
): OpenDocument {
  return {
    summary: {
      document_id: fixture.interaction.document_id,
      relative_path: fixture.source.relative_path,
      title: 'Recovery fixture',
      kind: 'prose',
      revision_id: revisionId,
      active_blob_id: visibleBlobId,
      word_count: 12,
      externally_modified: false
    },
    visible_blob_id: visibleBlobId,
    text,
    transient_draft: null
  };
}

function durableMetadata(): BranchCard[] {
  return fixture.family.map((item, index) => ({
    run_id: item.run_id,
    branch_id: item.branch_id,
    document_id: fixture.interaction.document_id,
    candidate_id: item.terminal.candidate_id,
    source_revision_id: fixture.source.revision_id,
    target_start_byte: fixture.source.caret_byte,
    target_end_byte: fixture.source.caret_byte,
    text: '',
    output_blob_id: item.terminal.sha256,
    output_byte_len: item.terminal.text === null
      ? null
      : new TextEncoder().encode(item.terminal.text).byteLength,
    status: item.terminal.status,
    seed: String(item.seed),
    model_id: fixture.models.initial,
    selection: null,
    error: item.terminal.error,
    error_truncated: false,
    created_at_unix_ms: index + 1
  }));
}

async function hydrateDurableProjection(branches: BranchCard[]): Promise<{
  branches: BranchCard[];
  verifiedBodyByRun: Record<string, VerifiedBranchBody>;
}> {
  const verifiedBodyByRun: Record<string, VerifiedBranchBody> = {};
  const hydrated: BranchCard[] = [];
  for (const branch of branches) {
    const item = fixture.family.find((candidate) => candidate.run_id === branch.run_id);
    if (!item) throw new Error(`unknown recovery run ${branch.run_id}`);
    if (item.terminal.status !== 'ready') {
      hydrated.push(branch);
      continue;
    }
    if (!item.terminal.candidate_id || !item.terminal.text || !item.terminal.sha256) {
      throw new Error(`incomplete ready fixture ${branch.run_id}`);
    }
    const body: BranchBody = {
      run_id: branch.run_id,
      branch_id: branch.branch_id,
      document_id: branch.document_id,
      candidate_id: item.terminal.candidate_id,
      source_revision_id: branch.source_revision_id,
      target_start_byte: branch.target_start_byte,
      target_end_byte: branch.target_end_byte,
      seed: branch.seed!,
      model_id: branch.model_id!,
      created_at_unix_ms: branch.created_at_unix_ms,
      output_blob_id: item.terminal.sha256,
      byte_len: new TextEncoder().encode(item.terminal.text).byteLength,
      text: item.terminal.text
    };
    const verified = await verifyBranchBody(body, branch);
    if (!verified) throw new Error(`fixture body did not verify for ${branch.run_id}`);
    verifiedBodyByRun[branch.run_id] = verified;
    hydrated.push({ ...branch, text: verified.text });
  }
  return { branches: hydrated, verifiedBodyByRun };
}

function eventEnvelope(
  event: CompletionRecoveryFixture['primary_event_stream'][number]
): DesktopGenerationEnvelope {
  const primary = fixture.family[0];
  const common = {
    event_id: `event-${event.sequence}`,
    run_id: primary.run_id,
    branch_id: primary.branch_id,
    sequence: event.sequence,
    occurred_at_ms: event.sequence + 1
  };
  if (event.event === 'terminal') {
    return {
      project_id: scope.projectId,
      session_id: scope.sessionId,
      document_id: scope.documentId,
      request_id: 'request-1',
      event: {
        event: 'generation_terminal',
        payload: {
          ...common,
          status: event.status!,
          candidate_id: event.candidate_id
        }
      }
    };
  }
  let kind: GenerationEventKind;
  switch (event.event) {
    case 'queued': kind = { kind: 'queued' }; break;
    case 'generating': kind = { kind: 'generating' }; break;
    case 'text_delta': kind = { kind: 'text_delta', text: event.text! }; break;
    case 'candidate_ready':
      kind = {
        kind: 'candidate_ready',
        candidate_id: event.candidate_id!,
        generated_span_artifact_id: 'artifact-1'
      };
      break;
  }
  return {
    project_id: scope.projectId,
    session_id: scope.sessionId,
    document_id: scope.documentId,
    request_id: 'request-1',
    event: {
      event: 'generation',
      payload: { ...common, kind }
    }
  };
}

function eventOnlyProjection(
  delivery: CompletionRecoveryFixture['delivery_cases'][number],
  activeScope: GenerationScope = scope
): { branch: BranchCard; appliedSequences: number[] } {
  let branch: BranchCard = {
    ...durableMetadata()[0],
    candidate_id: null,
    output_blob_id: null,
    output_byte_len: null,
    status: 'queued',
    text: '',
    error: null
  };
  let previousSequence: number | undefined;
  const appliedSequences: number[] = [];
  for (const index of delivery.event_indexes) {
    const envelope = eventEnvelope(fixture.primary_event_stream[index]);
    if (!generationEventBelongsToScope(envelope, activeScope)) continue;
    const stream = envelope.event;
    const generation = stream.payload;
    if (previousSequence !== undefined && generation.sequence <= previousSequence) continue;
    previousSequence = generation.sequence;
    appliedSequences.push(generation.sequence);
    if (stream.event === 'generation_terminal') {
      branch = {
        ...branch,
        candidate_id: stream.payload.candidate_id ?? branch.candidate_id,
        status: stream.payload.status === 'completed' ? 'ready' : stream.payload.status,
        error: stream.payload.error ?? branch.error
      };
      continue;
    }
    const kind = stream.payload.kind;
    switch (kind.kind) {
      case 'queued': branch = { ...branch, status: 'queued' }; break;
      case 'prefilling':
      case 'generating':
      case 'token': branch = { ...branch, status: 'generating' }; break;
      case 'text_delta':
        branch = { ...branch, status: 'generating', text: `${branch.text}${kind.text}` };
        break;
      case 'warning': branch = { ...branch, error: kind.message }; break;
      case 'cancellation_requested': branch = { ...branch, status: 'generating' }; break;
      case 'candidate_ready':
        branch = { ...branch, candidate_id: kind.candidate_id, status: 'ready' };
        break;
    }
  }
  return { branch, appliedSequences };
}

function terminalDigest(branches: BranchCard[]) {
  return branches.map((branch) => ({
    run_id: branch.run_id,
    status: branch.status,
    candidate_id: branch.candidate_id,
    output_blob_id: branch.output_blob_id,
    output_byte_len: branch.output_byte_len,
    error: branch.error,
    source_revision_id: branch.source_revision_id,
    model_id: branch.model_id
  }));
}

function expectedTerminalDigest() {
  return fixture.family.map((item) => ({
    run_id: item.run_id,
    status: item.terminal.status,
    candidate_id: item.terminal.candidate_id,
    output_blob_id: item.terminal.sha256,
    output_byte_len: item.terminal.text === null
      ? null
      : new TextEncoder().encode(item.terminal.text).byteLength,
    error: item.terminal.error,
    source_revision_id: fixture.source.revision_id,
    model_id: fixture.models.initial
  }));
}

function lifecycleInput(overrides: Partial<CompletionLifecycleInput> = {}): CompletionLifecycleInput {
  return {
    desktop: true,
    automationEnabled: true,
    projectAvailable: true,
    documentAvailable: true,
    hybridDocument: false,
    intentArmed: true,
    modelAvailable: true,
    modelTransitioning: false,
    compositionActive: false,
    visualMutationPending: false,
    sourceDirty: false,
    savePending: false,
    weavePending: false,
    promotionPending: false,
    workspaceIdle: true,
    editorReadonly: false,
    recoveryPending: false,
    saveSettled: true,
    revisionAvailable: true,
    visibleBlobCurrent: true,
    caretExact: true,
    caretAtStart: false,
    activeBranchCount: 0,
    ...overrides
  };
}

function suggestionFamily(
  branches: BranchCard[],
  verifiedBodyByRun: Record<string, VerifiedBranchBody>,
  currentDocument = openDocument(),
  currentModel = model(),
  liveTextByRun: Record<string, string> = {}
) {
  return inlineSuggestionFamily(fixture.source.caret_byte, 'visual', {
    branches,
    verifiedBodyByRun,
    liveTextByRun,
    currentModel,
    document: currentDocument,
    suggestionsEnabled: true,
    promotionReady: true,
    dismissedCandidateIds: [],
    unpresentableVisualKeys: [],
    manuscriptText: currentDocument.text,
    sourceNewline: null
  });
}

describe('model-free completion recovery characterization', () => {
  it('freezes dropped, duplicated, and reordered event-only projections', () => {
    expect(fixture.schema_version).toBe(1);
    expect(fixture.fixture_id).toBe('loom-completion-recovery-v1');
    expect(fixture.evidence_class).toBe('model_free_characterization');
    for (const delivery of fixture.delivery_cases) {
      const projected = eventOnlyProjection(delivery);
      expect(projected.appliedSequences, delivery.case_id)
        .toEqual(delivery.expected_applied_sequences);
      expect({
        status: projected.branch.status,
        candidate_id: projected.branch.candidate_id,
        text: projected.branch.text
      }, delivery.case_id).toEqual(delivery.expected_event_projection);
    }

    const crossSession = eventOnlyProjection(fixture.delivery_cases[0], {
      ...scope,
      sessionId: 'superseded-session'
    });
    expect(crossSession.appliedSequences).toEqual([]);
    expect(crossSession.branch.status).toBe('queued');
  });

  it('rebuilds the same exact terminal family from durable rows after every delivery and reload', async () => {
    const metadata = durableMetadata();
    const hydrated = await hydrateDurableProjection(metadata);
    const expectedDigest = expectedTerminalDigest();
    const expectedReadyRuns = fixture.family
      .filter((item) => item.terminal.status === 'ready')
      .map((item) => item.run_id);

    for (const delivery of fixture.delivery_cases) {
      const eventProjection = eventOnlyProjection(delivery);
      const reconciledMetadata = mergeNewestPage(metadata, [eventProjection.branch]);
      expect(terminalDigest(reconciledMetadata), delivery.case_id).toEqual(expectedDigest);

      const afterReload = mergeNewestPage(metadata, []);
      expect(terminalDigest(afterReload), delivery.case_id).toEqual(expectedDigest);

      const reconciledBodies = mergeNewestPage(hydrated.branches, [eventProjection.branch]);
      const liveTextByRun = eventProjection.branch.text
        ? { [eventProjection.branch.run_id]: eventProjection.branch.text }
        : {};
      const family = suggestionFamily(
        reconciledBodies,
        hydrated.verifiedBodyByRun,
        openDocument(),
        model(),
        liveTextByRun
      );
      expect(family.map((candidate) => candidate.runId), delivery.case_id)
        .toEqual(expectedReadyRuns);
      expect(family.map((candidate) => candidate.text), delivery.case_id).toEqual(
        fixture.family
          .filter((item) => item.terminal.status === 'ready')
          .map((item) => item.terminal.text)
      );
    }

    const forgedEventProjection = {
      ...hydrated.branches[0],
      status: 'failed' as const,
      candidate_id: 'event-only-candidate',
      output_blob_id: null,
      output_byte_len: null,
      text: 'event-only bytes',
      error: 'event-only failure'
    };
    expect(terminalDigest(mergeNewestPage(metadata, [forgedEventProjection])))
      .toEqual(expectedDigest);

    expect(exactVerifiedSuggestionFamily({
      active: true,
      branches: hydrated.branches,
      verifiedBodyByRun: hydrated.verifiedBodyByRun,
      targetByte: fixture.source.caret_byte
    }).map((branch) => branch.candidate_id)).toEqual([
      'candidate-a',
      'candidate-b',
      'candidate-c',
      'candidate-d'
    ]);
    expect(hydrated.branches.filter((branch) =>
      branchIsActionableOnShelf(branch, fixture.source.revision_id)
    ).map((branch) => branch.run_id)).toEqual([
      'run-ready-a',
      'run-ready-b',
      'run-ready-c',
      'run-ready-d',
      'run-failed'
    ]);
  });

  it('keeps frozen rollback authority across autosave while excluding the old revision family', async () => {
    const hydrated = await hydrateDurableProjection(durableMetadata());
    const family = suggestionFamily(hydrated.branches, hydrated.verifiedBodyByRun);
    const contextKey = completionSessionContextKey(
      fixture.interaction.project_session_id,
      fixture.interaction.document_id,
      fixture.interaction.document_epoch,
      'visual'
    );
    const session = startCompletionSession(contextKey, family, family[0].runId)!;
    const consumed = consumeCompletionText(session, fixture.autosave.accepted_text)!;
    expect(consumed.session.authorityFrozen).toBe(true);
    expect(completionPresentation(consumed.session)).toMatchObject({
      runId: 'run-ready-a',
      targetByte: fixture.source.caret_byte + fixture.autosave.accepted_text.length,
      text: 'somewhere beyond the rain a signal clicked from red to green.'
    });

    const autosaved = openDocument(
      fixture.autosave.revision_id,
      fixture.autosave.visible_blob_id,
      fixture.autosave.revision_text
    );
    expect(completionSessionContextKey(
      fixture.interaction.project_session_id,
      fixture.interaction.document_id,
      fixture.interaction.document_epoch,
      'visual'
    )).toBe(contextKey);
    expect(suggestionFamily(
      hydrated.branches,
      hydrated.verifiedBodyByRun,
      autosaved
    )).toEqual([]);
    expect(synchronizeCompletionCandidates(session, [])).toBeNull();
    expect(synchronizeCompletionCandidates(consumed.session, [])).toBe(consumed.session);

    const saving = automaticCompletionLifecycle(lifecycleInput({
      savePending: true,
      saveSettled: false
    }));
    expect(saving).toEqual({ phase: 'waiting', reason: 'save_pending' });
    expect(retainsScheduledCompletion(saving)).toBe(true);
    expect(automaticCompletionLifecycle(lifecycleInput()))
      .toEqual({ phase: 'ready', reason: null });
  });

  it('retains intent through IME/model waits but rejects stale edits and old-model candidates', async () => {
    const contextKey = completionSessionContextKey(
      fixture.interaction.project_session_id,
      fixture.interaction.document_id,
      fixture.interaction.document_epoch,
      'visual'
    );
    const intent = armCompletionGeneration(
      contextKey,
      fixture.interaction.edit_version,
      'document_edit',
      fixture.source.caret_byte
    );
    const composing = automaticCompletionLifecycle(lifecycleInput({ compositionActive: true }));
    expect(composing).toEqual({ phase: 'waiting', reason: 'composition_active' });
    expect(retainsScheduledCompletion(composing)).toBe(true);
    expect(completionGenerationIsArmed(
      intent,
      contextKey,
      fixture.interaction.edit_version
    )).toBe(true);
    expect(completionGenerationIsArmed(
      intent,
      contextKey,
      fixture.interaction.post_composition_edit_version
    )).toBe(false);
    const committedIntent = armCompletionGeneration(
      contextKey,
      fixture.interaction.post_composition_edit_version,
      'document_edit',
      fixture.source.caret_byte
    );
    expect(completionGenerationIsArmed(
      committedIntent,
      contextKey,
      fixture.interaction.post_composition_edit_version
    )).toBe(true);

    for (const waiting of [
      automaticCompletionLifecycle(lifecycleInput({ modelTransitioning: true })),
      automaticCompletionLifecycle(lifecycleInput({ modelAvailable: false }))
    ]) {
      expect(waiting.phase).toBe('waiting');
      expect(retainsScheduledCompletion(waiting)).toBe(true);
    }

    const hydrated = await hydrateDurableProjection(durableMetadata());
    const originalFamily = suggestionFamily(hydrated.branches, hydrated.verifiedBodyByRun);
    expect(originalFamily).toHaveLength(4);
    expect(suggestionFamily(
      hydrated.branches,
      hydrated.verifiedBodyByRun,
      openDocument(),
      model(fixture.models.replacement)
    )).toEqual([]);

    const replacementMetadata = {
      ...hydrated.branches[0],
      run_id: 'run-replacement',
      branch_id: 'branch-replacement',
      candidate_id: 'candidate-replacement',
      model_id: fixture.models.replacement,
      created_at_unix_ms: 100
    };
    const originalText = hydrated.branches[0].text;
    const replacementBody = await verifyBranchBody({
      run_id: replacementMetadata.run_id,
      branch_id: replacementMetadata.branch_id,
      document_id: replacementMetadata.document_id,
      candidate_id: replacementMetadata.candidate_id!,
      source_revision_id: replacementMetadata.source_revision_id,
      target_start_byte: replacementMetadata.target_start_byte,
      target_end_byte: replacementMetadata.target_end_byte,
      seed: replacementMetadata.seed!,
      model_id: replacementMetadata.model_id!,
      created_at_unix_ms: replacementMetadata.created_at_unix_ms,
      output_blob_id: replacementMetadata.output_blob_id!,
      byte_len: replacementMetadata.output_byte_len!,
      text: originalText
    }, replacementMetadata);
    expect(replacementBody).not.toBeNull();
    const replacementFamily = suggestionFamily(
      [replacementMetadata],
      { 'run-replacement': replacementBody! },
      openDocument(),
      model(fixture.models.replacement)
    );
    expect(replacementFamily.map((candidate) => candidate.runId))
      .toEqual(['run-replacement']);

    const unfrozen = startCompletionSession(contextKey, originalFamily, originalFamily[0].runId)!;
    expect(synchronizeCompletionCandidates(unfrozen, replacementFamily)?.selectedRunId)
      .toBe('run-replacement');
    const frozen = consumeCompletionText(unfrozen, fixture.autosave.accepted_text)!.session;
    expect(synchronizeCompletionCandidates(frozen, replacementFamily)).toBe(frozen);
  });
});
