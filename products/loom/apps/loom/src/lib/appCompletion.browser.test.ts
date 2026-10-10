import { mount, tick, unmount } from 'svelte';
import { afterEach, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { BoundaryEntry } from './completionBoundaryDiagnostics';
import type { BranchBody, BranchCard, CompletionSnapshot, DesktopGenerationEnvelope, ModelCapabilitySummary, OpenDocument, ProjectSnapshot, TerminalRun, WeaveStarted } from './types';
import '../app.css';

// Only the external native transport is injected. App, IPC ordering, snapshot
// validation, SHA-256 hydration, family/controller and editors are production.
// This deterministic integration test is NOT approved-model acceptance.
const transport = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, Set<(event: { payload: unknown }) => void>>()
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: transport.invoke, convertFileSrc: (path: string) => path }));
vi.mock('@tauri-apps/api/event', () => ({ listen: async (name: string, handler: (event: { payload: unknown }) => void) => {
  const handlers = transport.listeners.get(name) ?? new Set();
  handlers.add(handler); transport.listeners.set(name, handlers);
  return () => { handlers.delete(handler); };
} }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({
  onFocusChanged: async () => () => {}, onDragDropEvent: async () => () => {},
  onResized: async () => () => {}, isFullscreen: async () => false, setTitle: async () => {}
}) }));
import App from '../App.svelte';

let mounted: ReturnType<typeof mount> | null = null;
let restoreStorage: Array<[string, string]> = [];
let restoreNative: PropertyDescriptor | undefined;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = null;
  transport.listeners.clear(); transport.invoke.mockReset();
  if (restoreNative) Object.defineProperty(window, '__TAURI_INTERNALS__', restoreNative);
  else Reflect.deleteProperty(window, '__TAURI_INTERNALS__');
  localStorage.clear();
  for (const [key, value] of restoreStorage) localStorage.setItem(key, value);
  document.body.replaceChildren();
});

async function sha256(text: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text)));
  return Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
}
function completionWitness(): Record<string, any> {
  return JSON.parse(document.querySelector('[aria-label="Completion session witness"]')?.textContent ?? '{}');
}
function glyph(): string | null { return document.querySelector('.loom-visual-ghost')?.textContent ?? null; }

it('explicit Mom chat sends from the original pane without a manuscript writer and keeps its composer compact', async () => {
  restoreStorage = Object.entries(localStorage); localStorage.clear();
  restoreNative = Object.getOwnPropertyDescriptor(window, '__TAURI_INTERNALS__');
  Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {} });
  const blob = await sha256('Original manuscript');
  const opened: OpenDocument = { summary: {
    document_id: 'doc-1', relative_path: 'Untitled.md', title: 'Untitled', kind: 'prose',
    revision_id: 'revision-1', active_blob_id: blob, word_count: 2, externally_modified: false
  }, visible_blob_id: blob, text: 'Original manuscript', transient_draft: null };
  const project: ProjectSnapshot = { project_id: 'project-1', session_id: 'session-1', title: 'writing',
    root: '/injected/writing', schema_version: 1, pending_recovery: 0, documents: [opened.summary] };
  const calls: string[] = [];
  let submitted: Record<string, any> | null = null;
  let retainedRun: TerminalRun | null = null;
  transport.invoke.mockImplementation(async (command: string, args: Record<string, any> = {}) => {
    calls.push(command);
    switch (command) {
      case 'plugin:loom|application_close_pending': return false;
      case 'plugin:loom|workspace_chat_route': return 'mom_experimental';
      case 'plugin:loom|project_current': return project;
      case 'plugin:loom|workspace_roots_get': return { workspace_id: project.project_id, workspace_session_id: 'workspace-session', roots: [{ id: 'owner', name: project.title, owner: true, available: true, path: project.root, project_id: project.project_id }] };
      case 'plugin:loom|document_open': return opened;
      case 'plugin:loom|document_context_list': return { markdown: '', attachments: [], materials: [], revision: 'context-1' };
      case 'plugin:loom|workspace_template_get': return { enabled: true, document_id: 'workspace-config-document', revision_id: 'workspace-config-1', error: null,
        config: { panes: { chat: { kind: 'chat', position: 'right', visible: true, title: null, document: null, context: ['@document'] } } } };
      case 'plugin:loom|build_model_policy_get': return null;
      case 'plugin:loom|model_catalog_list': return [];
      case 'plugin:loom|model_list': return [];
      case 'plugin:loom|inference_status': return { suggestions: null };
      case 'plugin:loom|completion_snapshot': return { project_id: project.project_id, session_id: project.session_id,
        document_id: opened.summary.document_id, branches: [], active_operations: [], next_cursor: null, has_more: false };
      case 'plugin:loom|model_download_list':
      case 'plugin:loom|material_list':
      case 'plugin:loom|co_writer_list': return [];
      case 'plugin:loom|workspace_pane_list':
      case 'plugin:loom|terminal_list': return retainedRun ? [retainedRun] : [];
      case 'plugin:loom|suggestions_set':
      case 'plugin:loom|focus_mode_set': return;
      case 'plugin:loom|workspace_pane_run':
        submitted = args.request;
        retainedRun = { run_id: args.request.command_id, status: 'completed', expression: args.request.expression,
          presentation: { pane_id: args.request.pane_id, input: args.request.input },
          output_document_id: null, output_relative_path: null, preview: 'Transport fixture reply', error: null, created_at_ms: 1 };
        return { status: 'accepted', run: retainedRun };
      default: throw new Error(`Unexpected native operation: ${command}`);
    }
  });
  const target = document.createElement('div'); document.body.append(target);
  mounted = mount(App, { target });
  const input = page.getByRole('textbox', { name: 'Message', exact: true });
  await expect.element(input).toBeVisible();
  expect(input.element().getBoundingClientRect().height).toBeLessThanOrEqual(40);
  await page.getByRole('button', { name: 'Collapse main pane', exact: true }).click();
  expect(input.element().getBoundingClientRect().height).toBeLessThanOrEqual(40);
  await input.fill('@mom hello');
  const beforeSend = calls.length;
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => submitted).not.toBeNull();
  expect(submitted).toMatchObject({ workspace_id: project.project_id, workspace_session_id: 'workspace-session',
    pane_id: 'chat', configuration_revision_id: 'workspace-config-1', input: '@mom hello',
    captured_document: { project_id: project.project_id, session_id: project.session_id,
      document_id: 'doc-1', revision_id: 'revision-1', visible_blob_id: blob } });
  expect(calls.slice(beforeSend).filter(command => /model|weave_start/.test(command))).toEqual([]);
  expect(document.querySelector('[role="alert"]')?.textContent ?? '').not.toContain('Open and save');
  await expect.element(page.getByText('Transport fixture reply', { exact: true })).toBeVisible();
  expect(input.element().getBoundingClientRect().height).toBeLessThanOrEqual(40);
});

it.each([
  { loomModel: 'A', momModel: 'B', edited: false },
  { loomModel: 'A', momModel: 'B', edited: true },
  { loomModel: 'B', momModel: 'A', edited: false },
  { loomModel: 'B', momModel: 'A', edited: true }
])('ordinary Loom keeps pre-Mom history and writer $loomModel despite Mom $momModel (edited: $edited)', async ({ loomModel, momModel, edited }) => {
  // Real App, pane, IPC and revision-bound output loader; injected transport,
  // not a real model or Mom store and not native/shared-owner acceptance.
  restoreStorage = Object.entries(localStorage); localStorage.clear();
  restoreNative = Object.getOwnPropertyDescriptor(window, '__TAURI_INTERNALS__');
  Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {} });
  const sourceText = 'Unchanged manuscript';
  const sourceBlob = await sha256(sourceText);
  const answerText = edited ? 'Author-edited retained answer α' : 'Original pre-Mom answer';
  const answerBlob = await sha256(answerText);
  const source: OpenDocument = { summary: {
    document_id: 'source', relative_path: 'Draft.md', title: 'Draft', kind: 'prose',
    revision_id: 'source-revision', active_blob_id: sourceBlob, word_count: 2, externally_modified: false
  }, text: sourceText, visible_blob_id: sourceBlob, transient_draft: null };
  const answer: OpenDocument = { summary: {
    document_id: 'retained-answer', relative_path: 'Reply.md', title: 'Reply', kind: 'prose',
    revision_id: edited ? 'edited-revision' : 'original-revision', active_blob_id: answerBlob,
    word_count: 4, externally_modified: false
  }, text: answerText, visible_blob_id: answerBlob, transient_draft: null };
  const project: ProjectSnapshot = { project_id: 'project-1', session_id: 'session-1', title: 'writing',
    root: '/injected/writing', schema_version: 1, pending_recovery: 0, documents: [source.summary, answer.summary] };
  const legacy: TerminalRun = { run_id: 'pre-mom-run', status: 'completed', created_at_ms: 1,
    expression: 'Immutable original input', presentation: { pane_id: 'chat', input: 'Pre-Mom question' },
    output_document_id: answer.summary.document_id, output_relative_path: answer.summary.relative_path,
    preview: 'Preview must not substitute for retained output', error: null };
  const legacyBytes = JSON.stringify(legacy);
  const momProfile = Object.freeze({ model_id: momModel });
  const model: ModelCapabilitySummary = {
    model_id: loomModel, display_name: `Transport writer ${loomModel}`, local: true,
    loaded: true, header_verified: true, completion: true, chat: false, fill_in_middle: false,
    output_tokens: true, logprobs: false, model_path: `/injected/${loomModel}.gguf`, file_bytes: 1,
    architecture: 'gemma4', context_tokens: 4096, model_sha256: (loomModel === 'A' ? 'a' : 'b').repeat(64),
    projector_present: false, projector_sha256: null, media_kinds: [], policy_candidate: null,
    policy_verified: null, tested_profile: null
  };
  const calls: Array<{ command: string; args: Record<string, any> }> = [];
  let submitted: Record<string, any> | null = null;
  let retainedRun: TerminalRun | null = null;
  transport.invoke.mockImplementation(async (command: string, args: Record<string, any> = {}) => {
    calls.push({ command, args });
    switch (command) {
      case 'plugin:loom|application_close_pending': return false;
      case 'plugin:loom|workspace_chat_route': return 'loom';
      case 'plugin:loom|project_current': return project;
      case 'plugin:loom|workspace_roots_get': return { workspace_id: project.project_id, workspace_session_id: 'workspace-session', roots: [{ id: 'owner', name: project.title, owner: true, available: true, path: project.root, project_id: project.project_id }] };
      case 'plugin:loom|document_open': {
        const chosen = args.documentId === answer.summary.document_id ? answer : source;
        expect(args.expectedRevisionId).toBe(chosen.summary.revision_id);
        expect(args.expectedBlobId).toBe(chosen.summary.active_blob_id);
        return chosen;
      }
      case 'plugin:loom|workspace_pane_output':
        expect(args.projectId).toBe(project.project_id); expect(args.sessionId).toBe('workspace-session');
        expect(args.runId).toBe(legacy.run_id);
        return answer;
      case 'plugin:loom|document_context_list': return { markdown: '', attachments: [], materials: [], revision: 'context-1' };
      case 'plugin:loom|workspace_template_get': return { enabled: true, document_id: 'workspace-config-document', revision_id: 'workspace-config-1', error: null,
        config: { panes: { chat: { kind: 'chat', position: 'right', visible: true, title: null, document: null, context: [] } } } };
      case 'plugin:loom|build_model_policy_get': return null;
      case 'plugin:loom|model_catalog_list': return [];
      case 'plugin:loom|model_list': return [model];
      case 'plugin:loom|inference_status': return { suggestions: null };
      case 'plugin:loom|completion_snapshot': return { project_id: project.project_id, session_id: project.session_id,
        document_id: source.summary.document_id, branches: [], active_operations: [], next_cursor: null, has_more: false };
      case 'plugin:loom|model_download_list':
      case 'plugin:loom|material_list':
      case 'plugin:loom|co_writer_list': return [];
      case 'plugin:loom|workspace_pane_list':
      case 'plugin:loom|terminal_list': return retainedRun ? [legacy, retainedRun] : [legacy];
      case 'plugin:loom|suggestions_set':
      case 'plugin:loom|focus_mode_set': return;
      case 'plugin:loom|workspace_pane_run':
        submitted = args.request;
        retainedRun = { run_id: args.request.command_id, status: 'completed', expression: args.request.expression,
          presentation: { pane_id: args.request.pane_id, input: args.request.input },
          output_document_id: null, output_relative_path: null, preview: 'Transport fixture reply', error: null, created_at_ms: 2 };
        return { status: 'accepted', run: retainedRun };
      default: throw new Error(`Unexpected native operation: ${command}`);
    }
  });
  const target = document.createElement('div'); document.body.append(target);
  mounted = mount(App, { target });
  const input = page.getByRole('textbox', { name: 'Message', exact: true });
  await expect.element(input).toBeVisible();
  await expect.element(page.getByText(answerText, { exact: true })).toBeVisible();
  await expect.poll(() => calls.some(call => call.command === 'plugin:loom|model_list')).toBe(true);
  await tick();
  await input.fill('Continue');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => submitted).not.toBeNull();
  expect(submitted).toMatchObject({ workspace_id: project.project_id, workspace_session_id: 'workspace-session',
    pane_id: 'chat', configuration_revision_id: 'workspace-config-1', input: 'Continue',
    expression: `User: Pre-Mom question\nAssistant: ${answerText}\n\nUser: Continue\nAssistant:` });
  expect(calls.some(call => call.command === 'plugin:loom|workspace_chat_route')).toBe(true);
  expect(calls.filter(call => /mom_llama|model_load|model_unload/.test(call.command))).toEqual([]);
  expect(model.model_id).not.toBe(momProfile.model_id);
  expect(momProfile.model_id).toBe(momModel);
  expect(JSON.stringify(legacy)).toBe(legacyBytes);
  expect(source.text).toBe(sourceText);
});

const terminalSpaceSentinel = 'Loom native smoke prose: The lantern crossed the quiet room, casting a narrow pool of light across the ';
function authorDomText(editor: Element): string {
  const copy = editor.cloneNode(true) as Element;
  copy.querySelectorAll('.loom-ghost-widget').forEach(widget => widget.remove());
  return copy.textContent ?? '';
}

it.each([
  { unusableLastPartial: false, reenable: false, sourceText: 'hello', shuttle: 'none' },
  { unusableLastPartial: true, reenable: false, sourceText: 'hello', shuttle: 'none' },
  { unusableLastPartial: false, reenable: true, sourceText: 'hello', shuttle: 'none' },
  { unusableLastPartial: false, reenable: true, sourceText: terminalSpaceSentinel, shuttle: 'none' },
  { unusableLastPartial: false, reenable: false, sourceText: terminalSpaceSentinel, shuttle: 'accept' },
  { unusableLastPartial: false, reenable: false, sourceText: terminalSpaceSentinel, shuttle: 'unfocused' }
])('drives real App hydration and boundaries (partial: $unusableLastPartial; re-enable: $reenable; source: $sourceText; Shuttle: $shuttle)', async ({ unusableLastPartial, reenable, sourceText, shuttle }) => {
  restoreStorage = Object.entries(localStorage); localStorage.clear();
  restoreNative = Object.getOwnPropertyDescriptor(window, '__TAURI_INTERNALS__');
  Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {} });
  const sourceBytes = new TextEncoder().encode(sourceText).byteLength;
  if (sourceText === terminalSpaceSentinel) expect(sourceBytes).toBe(103);
  const sourceBlob = await sha256(sourceText);
  const opened: OpenDocument = { summary: {
    document_id: 'doc-1', relative_path: 'Untitled.md', title: 'Untitled', kind: 'prose',
    revision_id: 'revision-1', active_blob_id: sourceBlob, word_count: 1, externally_modified: false
  }, visible_blob_id: sourceBlob, text: sourceText, transient_draft: null };
  const project: ProjectSnapshot = {
    project_id: 'project-1', session_id: 'session-1', title: 'writing', root: '/injected/writing',
    schema_version: 1, pending_recovery: 0, documents: [opened.summary]
  };
  const model: ModelCapabilitySummary = {
    model_id: 'injected-writer', display_name: 'Injected native transport; no inference', local: true,
    loaded: true, header_verified: true, completion: true, chat: false, fill_in_middle: false,
    output_tokens: true, logprobs: false, model_path: '/injected/model.gguf', file_bytes: 1,
    architecture: 'gemma4', context_tokens: 4096, model_sha256: 'a'.repeat(64),
    projector_present: false, projector_sha256: null, media_kinds: [], policy_candidate: null,
    policy_verified: { profile_id: 'gemma_4_e2b_base_q8_loom_v1', rank: 0 }, tested_profile: 'gemma_4_e2b_base_q8_loom_v1'
  };
  const texts = [' world again', ' there friend', ' onward together', ' outside today'];
  const hashes = await Promise.all(texts.map(sha256));
  const partials = texts.map((text, index) => unusableLastPartial && index === 3 ? ' ' : text);
  let admission: WeaveStarted | null = null;
  let populated = 0, terminal = false, admissions = 0, snapshotReads = 0, bodyReads = 0;
  let expectedAdmissions = 1;
  let deferSuggestionPolicy = false;
  let releaseSuggestionPolicy: (() => void) | null = null;
  let releaseBodies!: () => void;
  const bodiesReady = new Promise<void>(resolve => { releaseBodies = resolve; });
  const unexpected: string[] = [];
  const visualGhostTelemetryPayloads: Record<string, unknown>[] = [];
  // This transport models receipts, not a native store. Only explicit actions
  // below authorize its writes; startup/toggling/hydration must never write.
  let allowedWrite: string | null = null;
  let persistedText = sourceText;
  let persistedBlob = sourceBlob;
  let persistedRevision = 'revision-1';
  let draftSequence = 0;
  const checkpoints: string[] = [];
  function branches(): BranchCard[] {
    return (admission?.branches ?? []).map((branch, index) => terminal ? {
      ...branch, status: 'ready', candidate_id: `${admissions === 1 ? 'candidate' : 'reenabled-candidate'}-${index}`, output_blob_id: hashes[index],
      output_byte_len: new TextEncoder().encode(texts[index]).byteLength
    } : { ...branch, status: 'generating' });
  }
  function snapshot(): CompletionSnapshot {
    snapshotReads += 1;
    const current = branches();
    return {
      project_id: project.project_id, session_id: project.session_id, document_id: opened.summary.document_id,
      branches: current, next_cursor: null, has_more: false,
      active_operations: !admission || terminal ? [] : [{
        request_id: admission.request_id, attempt_id: `${admission.request_id}:1`, operation_sequence: '1',
        phase: 'running', cancellation_requested: false, authoritative_terminal: null, final_projection: null,
        progress_sequences: ['7'], branches: current.map((branch, index) => ({
          run_id: branch.run_id, branch_id: branch.branch_id,
          partial_text: index < populated ? { text: partials[index], sequence: '7',
            utf8_byte_len: String(new TextEncoder().encode(partials[index]).byteLength) } : null
        }))
      }]
    };
  }
  transport.invoke.mockImplementation(async (command: string, args: Record<string, any> = {}) => {
    switch (command) {
      case 'plugin:loom|application_close_pending': return false;
      case 'plugin:loom|project_current': return project;
      case 'plugin:loom|workspace_roots_get': return { workspace_id: project.project_id, workspace_session_id: 'workspace-session', roots: [{ id: 'owner', name: project.title, owner: true, available: true, path: project.root, project_id: project.project_id }] };
      case 'plugin:loom|document_open': return opened;
      case 'plugin:loom|document_context_list': return { markdown: '', attachments: [], materials: [], revision: 'context-1' };
      case 'plugin:loom|workspace_template_get': return { enabled: false, document_id: null, revision_id: null, error: null, config: { panes: {} } };
      case 'plugin:loom|build_model_policy_get': return { name: 'writer-gemma4-base-v2', activation: 'quiet_default', canonical_sha256: '2d402d213b60ba65c4d018907e9eba67ccfbc1e97081cc0505f9713ae2dd89d2' };
      // Catalog availability is independent of an already-loaded verified writer.
      case 'plugin:loom|model_catalog_list': throw { code: 'injected_catalog_unavailable', message: 'catalog intentionally unavailable' };
      case 'plugin:loom|model_list': return [model];
      case 'plugin:loom|inference_status': return { suggestions: null };
      // This is an observability-only event. Keep its exact empty payload
      // checked so timing instrumentation cannot become a content channel.
      case 'plugin:loom|visual_ghost_rendered':
        expect(args).toEqual({});
        visualGhostTelemetryPayloads.push(args);
        return;
      case 'plugin:loom|model_download_list':
      case 'plugin:loom|workspace_pane_list':
      case 'plugin:loom|terminal_list':
      case 'plugin:loom|material_list':
      case 'plugin:loom|co_writer_list': return [];
      case 'plugin:loom|suggestions_set':
        if (deferSuggestionPolicy) {
          deferSuggestionPolicy = false;
          await new Promise<void>(resolve => { releaseSuggestionPolicy = resolve; });
          releaseSuggestionPolicy = null;
        }
        return;
      case 'plugin:loom|focus_mode_set': return;
      case 'plugin:loom|document_draft_upsert':
        expect(allowedWrite, 'no implicit manuscript write is authorized').not.toBeNull();
        expect(args.text).toBe(allowedWrite);
        return {
          document_id: args.documentId, source_revision_id: args.sourceRevisionId,
          blob_id: await sha256(args.text), version: String(++draftSequence), kind: args.kind,
          updated_at_unix_ms: Date.now(), replayed: false
        };
      case 'plugin:loom|document_checkpoint': {
        expect(allowedWrite, 'no implicit manuscript checkpoint is authorized').not.toBeNull();
        expect(args.text).toBe(allowedWrite);
        expect(args.expectedRevisionId).toBe(persistedRevision);
        expect(args.expectedVisibleBlobId).toBe(persistedBlob);
        const previousRevision = persistedRevision;
        persistedText = args.text;
        persistedBlob = await sha256(args.text);
        checkpoints.push(args.text);
        persistedRevision = `checkpoint-${checkpoints.length}`;
        return {
          command_id: args.commandId, command_kind: 'checkpoint', project_id: project.project_id,
          schema_version: 1, source_revision_id: previousRevision,
          result_revision_id: persistedRevision, result_blob_id: persistedBlob,
          request_fingerprint: null, replayed: false, visible_projection: { status: 'applied' },
          artifact_ids: [], completed_at_unix_ms: Date.now()
        };
      }
      case 'plugin:loom|completion_snapshot': return snapshot();
      case 'plugin:loom|weave_status':
        return admission ? { ...admission, branches: branches() } satisfies WeaveStarted : null;
      case 'plugin:loom|weave_start': {
        admissions += 1;
        expect(admissions, 'only an explicit enable may replace this completed fixture family').toBe(expectedAdmissions);
        expect(args.sourceRevisionId).toBe(opened.summary.revision_id);
        expect(args.expectedVisibleBlobId).toBe(sourceBlob);
        expect(args.cursorByte).toBe(sourceBytes);
        expect(args.policy.kind).toBe('automatic_v3');
        admission = {
          command_id: args.commandId, request_id: `weave-${args.commandId}`, project_id: project.project_id,
          session_id: project.session_id, document_id: opened.summary.document_id, source_revision_id: 'revision-1',
          exact_prompt_blob_id: sourceBlob,
          branches: texts.map((_, index): BranchCard => ({
            run_id: `${admissions === 1 ? 'run' : 'reenabled-run'}-${index}`,
            branch_id: `${admissions === 1 ? 'branch' : 'reenabled-branch'}-${index}`, weave_command_id: args.commandId,
            document_id: opened.summary.document_id, candidate_id: null, source_revision_id: 'revision-1',
            target_start_byte: sourceBytes, target_end_byte: sourceBytes, text: '', output_blob_id: null, output_byte_len: null,
            status: 'queued', seed: String(index), model_id: model.model_id, selection: null,
            error: null, error_truncated: false, created_at_unix_ms: index + 1
          }))
        };
        return admission;
      }
      case 'plugin:loom|branch_body': {
        bodyReads += 1;
        await bodiesReady;
        const branch = branches().find(item => item.run_id === args.runId)!;
        const index = Number(branch.run_id.split('-').at(-1));
        return {
          run_id: branch.run_id, branch_id: branch.branch_id, document_id: branch.document_id,
          candidate_id: branch.candidate_id!, source_revision_id: branch.source_revision_id,
          target_start_byte: sourceBytes, target_end_byte: sourceBytes, seed: branch.seed!, model_id: model.model_id,
          created_at_unix_ms: branch.created_at_unix_ms, output_blob_id: hashes[index],
          byte_len: new TextEncoder().encode(texts[index]).byteLength, text: texts[index]
        } satisfies BranchBody;
      }
      default: unexpected.push(command); throw new Error(`Unexpected native operation: ${command}`);
    }
  });
  function wake() {
    const active = admission!;
    const envelope: DesktopGenerationEnvelope = {
      project_id: active.project_id, session_id: active.session_id, document_id: active.document_id,
      request_id: active.request_id, event: { event: 'generation', payload: {
        event_id: `event-${admissions}-${populated}-${terminal}`,
        run_id: active.branches[0].run_id, branch_id: active.branches[0].branch_id, sequence: 7,
        kind: { kind: 'text_delta', text: 'wake only; snapshot owns projection' }, occurred_at_ms: 1
      } }
    };
    for (const listener of transport.listeners.get('loom://generation') ?? []) listener({ payload: envelope });
  }
  const target = document.createElement('div'); document.body.append(target);
  mounted = mount(App, { target });
  const keyboard = userEvent.setup();
  let stage = 'startup and hydration';
  try {
    await expect.element(page.getByRole('textbox', { name: 'Untitled, manuscript editor', exact: true })).toBeVisible();
    await expect.poll(() => admissions, { timeout: 10000 }).toBe(1);
    expect(glyph()).toBeNull();
    populated = 3; const before = snapshotReads; wake();
    await expect.poll(() => snapshotReads).toBeGreaterThan(before);
    expect(glyph()).toBeNull(); // Three arrivals cannot publish a four-choice family.
    populated = 4; wake();
    if (unusableLastPartial) {
      await expect.poll(() => completionWitness().family_phase?.kind).toBe('pending');
      expect(glyph()).toBeNull();
    } else {
      await expect.poll(glyph).toBe(texts[0]);
      await expect.poll(() => completionWitness().visual?.inline?.text).toBe(texts[0]);
      expect(completionWitness().candidates).toHaveLength(4);
      expect(completionWitness().selected_presentation_key).toMatch(/^stream:run-0:7/);
      expect(completionWitness().candidates[0].text_utf8_bytes).toBe(12);
      const editor = page.getByRole('textbox', { name: 'Untitled, manuscript editor', exact: true }).element();
      for (const key of ['ArrowDown', 'ArrowUp']) {
        editor.dispatchEvent(new KeyboardEvent('keydown', { key, altKey: true, bubbles: true, cancelable: true }));
        await expect.poll(() => completionWitness().selected_run_id).toBe(key === 'ArrowDown' ? 'run-1' : 'run-0');
      }
      editor.dispatchEvent(new KeyboardEvent('keyup', { key: 'Alt', code: 'AltLeft', bubbles: true }));
    }
    terminal = true; wake();
    await expect.poll(() => bodyReads).toBeGreaterThan(0);
    await tick();
    if (unusableLastPartial) {
      await expect.poll(() => completionWitness().family_phase?.kind).toBe('awaiting_hydration');
      expect(glyph()).toBeNull();
    } else {
      expect(glyph()).toBe(texts[0]); // A good live family does not blink during SHA hydration.
    }
    // The Source-mode unit regression executes the production retry planner
    // against this pending-body phase; this App test verifies its real wiring.
    expect(admissions).toBe(1);
    releaseBodies();
    await expect.poll(() => completionWitness().selected_presentation_key).toBe(`candidate-0:${hashes[0]}`);
    await expect.poll(() => completionWitness().visual?.inline?.text).toBe(texts[0]);
    expect(glyph()).toBe(texts[0]);
    expect(admissions).toBe(1);
    expect(unexpected).toEqual([]); // Includes all hidden writes, extra work, and hosted fallback.
    expect(visualGhostTelemetryPayloads.length).toBeGreaterThan(0);
    expect(transport.invoke.mock.calls.some(([command]) => String(command).includes('checkpoint'))).toBe(false);
    const writingSurface = page.getByRole('textbox', { name: 'Untitled, manuscript editor', exact: true }).element();
    expect(authorDomText(writingSurface)).toBe(sourceText);
    expect(completionWitness().pre_admission.caret_byte).toBe(sourceBytes);
    expect(completionWitness().boundary_trace.first_terminal_space_loss).toBeNull();
    if (shuttle !== 'none') {
      // Exercise App -> LoomEditor -> ghostText -> controller, not exported
      // editor methods. Focus alone is not proof of acceptance or persistence.
      const originalFamily = completionWitness().candidates.map((item: any) => item.run_id);
      const word = ' world';
      const acceptedText = sourceText + word; // Keep BOTH boundary spaces.
      const acceptedBytes = sourceBytes + new TextEncoder().encode(word).byteLength;
      const shortcut = (key: 's' | 'J') => {
        const event = new KeyboardEvent('keydown', {
          key, code: key === 's' ? 'KeyS' : 'KeyJ', metaKey: true, shiftKey: key === 'J',
          bubbles: true, cancelable: true
        });
        // Like this file's existing enable shortcut, dispatch through the real
        // DOM/global handler. No private App method or mocked editor insertion.
        (key === 's' ? writingSurface : window).dispatchEvent(event);
        expect(event.defaultPrevented, `App must handle ${key}`).toBe(true);
      };
      const saved = () => completionWitness().pre_admission.saved_version === completionWitness().pre_admission.edit_version;
      const checkpoint = async (expectedText: string) => {
        // updateText schedules draft at 750 ms; its completion re-arms the
        // 900 ms autosave. A default expect.poll(persistedText) is not an
        // editor-acceptance oracle. Explicit Save flushes the real draft path.
        expect(authorDomText(writingSurface)).toBe(expectedText);
        shortcut('s'); // Exactly one explicit save, never an action retry.
        await expect.poll(() => persistedText).toBe(expectedText);
        await expect.poll(saved).toBe(true);
        expect(completionWitness().pre_admission.revision_id).toBe(persistedRevision);
        expect(completionWitness().pre_admission.visible_blob_id).toBe(persistedBlob);
        expect(persistedBlob).toBe(await sha256(expectedText));
      };
      const exactCaret = async (bytes: number) => {
        await expect.poll(() => completionWitness().editor_selection).toMatchObject({
          available: true, empty: true, caret_at_end: true, caret_byte_offset: bytes
        });
        expect(completionWitness().pre_admission.caret_byte).toBe(bytes);
      };
      const reverseWord = async () => {
        allowedWrite = sourceText;
        await keyboard.keyboard('{Alt>}{ArrowLeft}{/Alt}');
        await expect.poll(() => authorDomText(writingSurface)).toBe(sourceText);
        await expect.poll(() => completionWitness().accepted_chunk_count).toBe(0);
        expect(completionWitness().accepted_utf8_bytes).toBe(0);
        await exactCaret(sourceBytes);
        await checkpoint(sourceText);
        await expect.poll(() => completionWitness().visual?.inline?.text).toBe(texts[0]);
        expect(glyph()).toBe(texts[0]);
        expect(completionWitness().candidates.map((item: any) => item.run_id)).toEqual(originalFamily);
      };

      stage = 'manual Option-Right: editor and controller, then explicit Save';
      // Do not click/reposition the caret after admission: navigation revokes
      // the family. Focus its existing exact selection and observe live pixels.
      writingSurface.focus();
      expect(document.activeElement).toBe(writingSurface);
      await exactCaret(sourceBytes);
      await expect.poll(() => completionWitness().inline_visible_key).toBe(`candidate-0:${hashes[0]}`);
      expect(glyph()).toBe(texts[0]);
      const previousActionSequence = completionWitness().last_action?.sequence ?? 0;
      allowedWrite = acceptedText;
      await keyboard.keyboard('{Alt>}{ArrowRight}{/Alt}');
      await expect.poll(() => authorDomText(writingSurface)).toBe(acceptedText);
      await expect.poll(() => completionWitness().last_action).toMatchObject({
        sequence: previousActionSequence + 1, kind: 'option_word', run_id: originalFamily[0],
        inserted_utf8_bytes: 6, accepted_utf8_bytes: 6
      });
      expect(completionWitness().accepted_chunk_count).toBe(1);
      await exactCaret(acceptedBytes);
      // Manual Option acceptance reports controller_insertion, not the
      // visual_shuttle_attempt emitted only by acceptGhostWord(false).
      expect(completionWitness().boundary_trace.entries.some((entry: BoundaryEntry) =>
        entry.kind === 'controller_insertion' && entry.facts.action === 'option_word' && entry.facts.authorized === true
      )).toBe(true);
      await checkpoint(acceptedText);

      stage = 'manual Option-Left: exact reversal, then explicit Save';
      await reverseWord();
      expect(checkpoints).toEqual([acceptedText, sourceText]);
      const manualActionSequence = completionWitness().last_action.sequence;
      expect(manualActionSequence).toBe(previousActionSequence + 1);
      const writeCount = () => transport.invoke.mock.calls.filter(([command]) =>
        command === 'plugin:loom|document_draft_upsert' || command === 'plugin:loom|document_checkpoint'
      ).length;
      const writesBeforeShuttle = writeCount();
      allowedWrite = shuttle === 'accept' ? acceptedText : null;
      let outside: HTMLButtonElement | null = null;
      let firstFire: BoundaryEntry | undefined;
      let firstAttemptEntries: BoundaryEntry[] = [];
      stage = `first hidden Shuttle attempt: ${shuttle}`;
      shortcut('J');
      try {
        await expect.poll(() => completionWitness().shuttle_enabled).toBe(true);
        await expect.poll(() => completionWitness().inline_hidden_requested).toBe(true);
        await expect.poll(() => completionWitness().inline_visible_key).toBe('');
        await expect.poll(() => completionWitness().visual?.inline).toBeNull();
        await expect.poll(() => completionWitness().shuttle_schedule.armed).toBe(true);
        expect(glyph()).toBe(''); // Real hidden widget, not just controller intent.
        expect(authorDomText(writingSurface)).toBe(sourceText);
        expect(document.activeElement).toBe(writingSurface);
        expect(completionWitness().shuttle_schedule.window_focused).toBe(true);
        const armedKey = completionWitness().shuttle_schedule.key;
        const fence = completionWitness().boundary_trace.sequence;
        const attemptEntries = (): BoundaryEntry[] => completionWitness().boundary_trace.entries.filter(
          (entry: BoundaryEntry) => entry.sequence > fence
        );
        if (shuttle === 'unfocused') {
          outside = document.createElement('button');
          outside.textContent = 'Outside manuscript';
          document.body.append(outside);
          outside.focus(); // Keep window focused; only the editor must refuse.
          expect(document.activeElement).toBe(outside);
          expect(completionWitness().shuttle_schedule.window_focused).toBe(true);
        }
        // Observe the FIRST completed fire for this armed key. A refusal fails
        // the positive case even if a later production retry would succeed.
        // The real 4,000 ms timer runs unchanged; no fake time or sleeps.
        await expect.poll(() => attemptEntries().find(entry =>
          entry.kind === 'shuttle_fire' && entry.facts.key === armedKey &&
          entry.facts.result !== 'attempting'
        ), { timeout: 6_000 }).not.toBeUndefined();
        firstAttemptEntries = attemptEntries();
        firstFire = firstAttemptEntries.find(entry =>
          entry.kind === 'shuttle_fire' && entry.facts.key === armedKey && entry.facts.result !== 'attempting'
        );
      } finally {
        // Stop via the production command before assertions/Save; in the
        // refusal case this cancels the existing re-arm, not a test retry.
        if (completionWitness().shuttle_enabled) {
          shortcut('J');
          await expect.poll(() => completionWitness().shuttle_enabled).toBe(false);
        }
        outside?.remove();
      }
      const attempts = firstAttemptEntries.filter(entry => entry.kind === 'visual_shuttle_attempt');
      expect(attempts).toHaveLength(1);
      expect(firstAttemptEntries.filter(entry => entry.kind === 'shuttle_fire' && entry.facts.result === 'attempting')).toHaveLength(1);
      expect(attempts[0].facts.require_visible).toBe(false);
      if (shuttle === 'unfocused') {
        expect(firstFire?.facts.result).toBe('editor_rejected');
        expect(attempts[0].facts).toMatchObject({ result: 'editor_unfocused', focused: false });
        expect(authorDomText(writingSurface)).toBe(sourceText);
        expect(persistedText).toBe(sourceText);
        expect(completionWitness().accepted_chunk_count).toBe(0);
        expect(completionWitness().accepted_utf8_bytes).toBe(0);
        expect(completionWitness().last_action.sequence).toBe(manualActionSequence);
        expect(checkpoints).toEqual([acceptedText, sourceText]);
        expect(writeCount()).toBe(writesBeforeShuttle); // Not even a failed draft/checkpoint attempt.
      } else {
        expect(firstFire?.facts.result).toBe('inserted');
        expect(attempts[0].facts).toMatchObject({ result: 'inserted', focused: true });
        await expect.poll(() => authorDomText(writingSurface)).toBe(acceptedText);
        expect(completionWitness().last_action).toMatchObject({
          sequence: manualActionSequence + 1, kind: 'shuttle_word', run_id: originalFamily[0],
          inserted_utf8_bytes: 6, accepted_utf8_bytes: 6
        });
        expect(completionWitness().accepted_chunk_count).toBe(1);
        await exactCaret(acceptedBytes);
        stage = 'Shuttle result: explicit Save and exact Option-Left reversal';
        await checkpoint(acceptedText);
        await reverseWord();
        expect(checkpoints).toEqual([acceptedText, sourceText, acceptedText, sourceText]);
      }
      expect(admissions).toBe(1); // Exactly four run IDs; no replacement admission.
      expect(completionWitness().candidates.map((item: any) => item.run_id)).toEqual(originalFamily);
      expect(completionWitness().boundary_trace.first_terminal_space_loss).toBeNull();
      expect(unexpected).toEqual([]);
    }
    if (reenable) {
      stage = 'same-loaded-writer re-enable at exact authored boundary';
      // Cold startup/model discovery may rescue a lost schedule. Toggle with the
      // SAME already-loaded writer, no edit or model load to rescue this path.
      const editor = page.getByRole('textbox', { name: 'Untitled, manuscript editor', exact: true }).element();
      const toggle = (target: EventTarget = editor) => target.dispatchEvent(new KeyboardEvent('keydown', {
        key: 'G', code: 'KeyG', metaKey: true, shiftKey: true, bubbles: true, cancelable: true
      }));
      toggle();
      await expect.poll(() => completionWitness().autocomplete_enabled).toBe(false);
      await expect.poll(() => completionWitness().session_cached).toBe(false);
      expect(admissions).toBe(1);
      expect(completionWitness().writer_id).toBe(model.model_id);
      const disabledWitness = completionWitness();
      expectedAdmissions = 2;
      admission = null; populated = 0; terminal = false;
      deferSuggestionPolicy = true;
      toggle(window);
      await expect.poll(() => releaseSuggestionPolicy).not.toBeNull();
      const text = editor.querySelector('p')?.firstChild;
      expect(text).not.toBeNull();
      const start = document.createRange();
      start.setStart(text!, 0);
      start.collapse(true);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(start);
      document.dispatchEvent(new Event('selectionchange'));
      await expect.poll(() => completionWitness().pre_admission?.caret_byte).toBe(0);
      (releaseSuggestionPolicy as (() => void) | null)?.();
      await expect.poll(() => completionWitness().autocomplete_enabled).toBe(true);
      await expect.poll(() => admissions, { timeout: 10000 }).toBe(2);
      await expect.poll(() => completionWitness().pre_admission?.caret_byte).toBe(sourceBytes);
      expect(disabledWitness.pre_admission?.scheduled).toBeNull();
      expect(disabledWitness.pre_admission?.project_root).toBe(project.root);
      expect(disabledWitness.pre_admission?.lifecycle?.reason).toBe('automation_disabled');
      expect(completionWitness().writer_id).toBe(model.model_id);
      populated = 4; wake();
      await expect.poll(glyph).toBe(texts[0]);
      await expect.poll(() => completionWitness().selected_presentation_key).toMatch(/^stream:reenabled-run-0:7/);
      expect(completionWitness().candidates).toHaveLength(4);
      terminal = true; wake();
      await expect.poll(() => completionWitness().selected_presentation_key).toBe(`reenabled-candidate-0:${hashes[0]}`);
      expect(glyph()).toBe(texts[0]);
      await new Promise(resolve => window.setTimeout(resolve, 2_100));
      expect(admissions).toBe(2);
      expect(unexpected).toEqual([]); // Also rejects model-load, writes and hosted fallback.
      expect(authorDomText(writingSurface)).toBe(sourceText);
      expect(persistedText).toBe(sourceText);
      expect(checkpoints).toEqual([]);
      expect(completionWitness().pre_admission.caret_byte).toBe(sourceBytes);
      expect(completionWitness().boundary_trace.first_terminal_space_loss).toBeNull();
    }
  } catch (error) {
    // Failure context only: distinguish DOM/controller refusal from an IPC or
    // save-boundary failure. All author bytes here are the synthetic fixture.
    console.error('Real-App completion fixture failure', JSON.stringify({
      stage, author_text: document.querySelector('.loom-prosemirror')
        ? authorDomText(document.querySelector('.loom-prosemirror')!) : null,
      persisted_text: persistedText, checkpoints, admissions, unexpected,
      witness: completionWitness(), native_commands: transport.invoke.mock.calls.map(([command]) => command)
    }));
    throw error;
  } finally {
    releaseBodies();
    (releaseSuggestionPolicy as (() => void) | null)?.();
    await keyboard.cleanup();
  }
}, 20_000);
