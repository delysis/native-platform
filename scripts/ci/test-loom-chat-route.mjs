// Node >=22.13. Executes the production App/WorkspacePane function bodies and
// retained-output/model selectors, with only their external dependencies injected.
// This is NOT a Svelte DOM test, a Rust test, or native writer/shared-owner acceptance.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const read = name => readFileSync(path.join(root, name), 'utf8');
const appSource = read('products/loom/apps/loom/src/App.svelte');
const paneSource = read('products/loom/apps/loom/src/lib/WorkspacePane.svelte');
const plugin = 'products/loom/crates/tauri-plugin-loom/';

function svelteFunction(source, name) {
  const match = source.match(new RegExp(`^  (?:async )?function ${name}\\([\\s\\S]*?^  \\}`, 'mu'));
  assert(match, `missing production function ${name}`);
  return stripTypeScriptTypes(match[0]);
}
function moduleBody(name) {
  return stripTypeScriptTypes(read(name).replace(/^import[\s\S]*?;\s*/gmu, ''))
    .replace(/^export /gmu, '');
}
function freeze(value) {
  if (value && typeof value === 'object' && !Object.isFrozen(value)) {
    for (const entry of Object.values(value)) freeze(entry);
    Object.freeze(value);
  }
  return value;
}
function snapshot(id, text, revision = 'rev-1', blob = 'blob-1') {
  return { summary: { document_id: id, relative_path: `${id}.md`, title: id, kind: 'prose',
    revision_id: revision, active_blob_id: blob, word_count: 1, externally_modified: false },
    visible_blob_id: blob, text, transient_draft: null };
}
function writer(id, loaded) {
  return { model_id: id, display_name: `Component ${id}; not native evidence`, model_path: `/component/${id}.gguf`,
    loaded, local: true, header_verified: true, completion: true, output_tokens: true,
    projector_present: false, media_kinds: [], policy_verified: null };
}

function fixture({ route = 'loom', loomModel = 'A', momModel = 'B', loaded = false, kind = 'chat' } = {}) {
  const calls = [], requests = [], reads = [];
  const opened = snapshot('source', 'Unchanged source manuscript', 'source-revision', 'source-blob');
  const answer = snapshot('old-answer', 'Original pre-Mom answer', 'answer-revision-1', 'answer-blob-1');
  const outputStore = new Map([[answer.summary.document_id, answer]]);
  const oldRun = freeze({ run_id: 'pre-mom-run', status: 'completed', created_at_ms: 1,
    expression: 'Original immutable expression', presentation: { pane_id: 'chat', input: 'Pre-Mom question' },
    output_document_id: answer.summary.document_id, output_relative_path: answer.summary.relative_path,
    preview: 'Preview is not canonical history', error: null });
  const retained = JSON.stringify(oldRun);
  const mom = freeze({ model_id: momModel, persona: { model_id: momModel }, canonical_messages: ['Private Mom history'] });
  let seq = 0;
  const context = {
    TextEncoder, console, Map, Set, Promise,
    project: { project_id: 'project-1', session_id: 'session-1' }, document: opened,
    projectId: 'project-1', sessionId: 'session-1', documentEpoch: 1, documentText: opened.text,
    editorReadonly: false, compositionActive: false, mounted: true,
    busy: false, composing: false, readonly: false, dispatching: false, cancelRequested: false,
    entry: 'Continue', error: '', pending: null, scope: 'project-1/session-1/chat',
    paneId: 'chat', config: { kind, context: [], document: null },
    documents: [opened.summary, answer.summary], runs: [oldRun], MAX_PROMPT_BYTES: 65536,
    models: [writer(loomModel, loaded)], selectedModelPath: `/component/${loomModel}.gguf`,
    workspaceTemplate: { enabled: true, error: null, config: { panes: {} } },
    momProfile: mom,
    flushEditors: () => true,
    terminalScopeIsCurrent: (project, session) => project === context.project.project_id && session === context.project.session_id,
    cancelSuggestionTimer: () => calls.push('cancel-suggestions'),
    cancelActiveBranches: async () => { calls.push('cancel-branches'); },
    flushCurrentDocument: async () => { calls.push('checkpoint'); return true; },
    currentWorkspaceCapture: () => ({ projectId: context.project.project_id, sessionId: context.project.session_id }),
    invoke: async command => {
      assert.equal(command, 'plugin:loom|workspace_chat_route');
      calls.push('route');
      return typeof route === 'function' ? route(context) : route;
    },
    // External native activation is injected, not reimplemented or claimed as
    // verified. The selected writer after activation is the PRODUCTION selector.
    loadPreferredSuggestionModel: async () => {
      calls.push(`load:${context.selectedModelPath}`);
      for (const model of context.models) model.loaded = model.model_path === context.selectedModelPath;
      return true;
    },
    openDocument: async (project, session, id, revision, blob) => {
      reads.push([project, session, id, revision, blob]);
      const found = outputStore.get(id);
      if (!found || found.summary.revision_id !== revision || found.summary.active_blob_id !== blob) {
        throw new Error('Retained output revision/identity changed');
      }
      return structuredClone(found);
    },
    newUlid: () => `component-command-${++seq}`,
    runTerminal: async request => {
      // Capture actual IPC input; do NOT pretend to execute either Rust route.
      requests.push({ request: structuredClone(request), writerAtIpc: context.currentModel?.model_id ?? null });
      return { run_id: request.commandId, status: 'completed', created_at_ms: 2 + seq,
        expression: request.expression, presentation: request.presentation,
        output_document_id: null, output_relative_path: null, preview: 'Transport-only response', error: null };
    },
    onRunsChanged: () => calls.push('runs-changed'),
    refresh: async () => {},
    normalizeFailure: failure => ({ message: failure?.message ?? String(failure) })
  };
  vm.createContext(context);
  for (const source of ['modelPolicy.ts', 'workspaceTemplate.ts', 'retainedOutput.ts']) {
    vm.runInContext(moduleBody(`products/loom/apps/loom/src/lib/${source}`), context, { filename: source });
  }
  vm.runInContext('this.outputLoader = new RetainedOutputLoader()', context);
  Object.defineProperty(context, 'currentModel', {
    get: () => context.workspaceWriterModel(context.models, null, [], context.workspaceTemplate, true)
  });
  vm.runInContext(svelteFunction(appSource, 'preparePaneRun'), context);
  for (const name of ['readOutput', 'referenceName', 'prompt', 'submit']) {
    vm.runInContext(svelteFunction(paneSource, name), context);
  }
  const before = appSource.match(/beforeRun=\{\(\) => (preparePaneRun\([^}]*\))\}/u);
  assert(before, 'original WorkspacePane must use the actual App admission callback');
  context.paneConfig = context.config;
  context.beforeRun = vm.runInContext(`() => ${before[1]}`, context);
  return { context, calls, requests, reads, outputStore, opened, answer, oldRun, mom,
    assertImmutable: () => assert.equal(JSON.stringify(oldRun), retained) };
}

test('source contract: ordinary root is Loom; only native opt-in installs Mom', () => {
  const source = read('products/loom/apps/loom/src-tauri/src/application.rs');
  assert.match(source, /self\.configure_workspace_chat\(builder, tauri_plugin_loom::WorkspaceChatRoute::default\(\)\)/u);
  assert.match(source, /WorkspaceChatRoute::Loom => builder/u);
  assert.match(source, /WorkspaceChatRoute::MomExperimental =>\s*(?:\{\s*)?builder\.manage/u);
  assert.match(source, /self\.mom\.configure\(builder\)/u);
  const route = read(`${plugin}src/workspace_chat.rs`);
  assert.match(route, /#\[default\]\s*Loom/u);
  assert.match(route, /app\.try_state::<WorkspaceChatService<R>>\(\)\.is_some\(\)/u);
  const terminal = read(`${plugin}src/terminal.rs`);
  assert.match(terminal, /app\.try_state::<crate::WorkspaceChatService<R>>\(\)/u);
  assert.match(terminal, /else \{\s*parse_neural_command\(&entry\)/u);
  assert.match(terminal, /Some\(loaded_model\(&state\)\?\)/u);
  assert.match(terminal, /else \{\s*evaluator\.evaluate_command\(&command\)/u);
  assert.doesNotMatch(source, /conversation_store|save_document|save_messages|std::fs::write/u);
});

test('source contract: capability IPC has generated metadata and a read-only permission', () => {
  assert.match(read(`${plugin}src/lib.rs`), /workspace_chat::workspace_chat_route,/u);
  assert.match(read(`${plugin}build.rs`), /"workspace_chat_route",/u);
  assert.match(read(`${plugin}permissions/default.toml`), /"allow-workspace-chat-route",/u);
  const permission = read(`${plugin}permissions/autogenerated/commands/workspace_chat_route.toml`);
  assert.match(permission, /commands\.allow = \["workspace_chat_route"\]/u);
  assert.match(permission, /commands\.deny = \["workspace_chat_route"\]/u);
  assert.doesNotMatch(permission, /terminal_run|model_load|chat_send/u);
});

for (const [loomModel, momModel] of [['A', 'B'], ['B', 'A']]) {
  for (const loaded of [false, true]) {
    test(`ordinary App send keeps pre-Mom history and Loom ${loomModel}, not Mom ${momModel}; loaded=${loaded}`, async () => {
      const f = fixture({ loomModel, momModel, loaded });
      await f.context.submit();
      assert.equal(f.requests.length, 1, f.context.error);
      assert.equal(f.requests[0].writerAtIpc, loomModel);
      assert.equal(f.requests[0].request.expression,
        'User: Pre-Mom question\nAssistant: Original pre-Mom answer\n\nUser: Continue\nAssistant:');
      assert.deepEqual(f.requests[0].request.presentation, { pane_id: 'chat', input: 'Continue' });
      assert.equal(f.requests[0].request.sourceRevisionId, 'source-revision');
      assert.equal(f.requests[0].request.expectedVisibleBlobId, 'source-blob');
      assert.equal(f.requests[0].request.turnBoundary, 'chat');
      assert.equal(f.calls.filter(call => call.startsWith('load:')).length, loaded ? 0 : 1);
      assert(f.calls.indexOf('checkpoint') < f.calls.indexOf('route'));
      assert.equal(f.context.momProfile, f.mom);
      assert.equal(f.mom.persona.model_id, momModel);
      f.assertImmutable();
    });
  }
}

test('output-document edits change the next prompt at the new revision without rewriting the old run', async () => {
  const f = fixture();
  const original = await f.context.prompt('Continue', f.opened);
  assert.match(original.expression, /Original pre-Mom answer/u);
  const edited = snapshot('old-answer', 'Author-edited answer α', 'answer-revision-2', 'answer-blob-2');
  f.outputStore.set('old-answer', edited);
  f.context.documents = [f.opened.summary, edited.summary];
  await f.context.submit();
  assert.equal(f.requests.length, 1, f.context.error);
  assert.equal(f.requests[0].request.expression,
    'User: Pre-Mom question\nAssistant: Author-edited answer α\n\nUser: Continue\nAssistant:');
  assert.deepEqual(f.reads, [
    ['project-1', 'session-1', 'old-answer', 'answer-revision-1', 'answer-blob-1'],
    ['project-1', 'session-1', 'old-answer', 'answer-revision-2', 'answer-blob-2']
  ]);
  f.assertImmutable();
  assert.equal(f.oldRun.expression, 'Original immutable expression');
});

test('output cache identity includes both blob and revision, even if one changes alone', async () => {
  const f = fixture();
  await f.context.prompt('One', f.opened);
  for (const [revision, blob, text] of [
    ['answer-revision-1', 'new-blob', 'Different content'],
    ['new-revision', 'new-blob', 'Different content']
  ]) {
    const next = snapshot('old-answer', text, revision, blob);
    f.outputStore.set('old-answer', next); f.context.documents = [next.summary];
    assert.match((await f.context.prompt('Next', f.opened)).expression, /Different content/u);
  }
  assert.equal(f.reads.length, 3);
  f.assertImmutable();
});

test('same-path replacement cannot impersonate a retained output document', async () => {
  const f = fixture();
  const replacement = snapshot('different-identity', 'Not the retained output');
  replacement.summary.relative_path = f.answer.summary.relative_path;
  f.context.documents = [f.opened.summary, replacement.summary];
  f.outputStore.set(replacement.summary.document_id, replacement);
  await f.context.submit();
  assert.equal(f.requests.length, 0);
  assert.equal(f.context.entry, 'Continue');
  assert.match(f.context.error, /retained document is not available/u);
  f.assertImmutable();
});

test('a stale exact-output read blocks dispatch and retains the draft; refreshed revision retries', async () => {
  const f = fixture();
  const edited = snapshot('old-answer', 'New exact answer', 'new-revision', 'new-blob');
  f.outputStore.set('old-answer', edited);
  await f.context.submit();
  assert.equal(f.requests.length, 0);
  assert.equal(f.context.entry, 'Continue');
  assert.match(f.context.error, /revision\/identity changed/u);
  f.context.documents = [f.opened.summary, edited.summary];
  await f.context.submit();
  assert.equal(f.requests.length, 1, f.context.error);
  assert.match(f.requests[0].request.expression, /New exact answer/u);
  f.assertImmutable();
});

test('failed output hydration can retry the same revision rather than caching rejection', async () => {
  const f = fixture();
  f.outputStore.delete('old-answer');
  await assert.rejects(f.context.prompt('Continue', f.opened), /revision\/identity changed/u);
  f.outputStore.set('old-answer', f.answer);
  assert.match((await f.context.prompt('Continue', f.opened)).expression, /Original pre-Mom answer/u);
  assert.equal(f.reads.length, 2);
});

test('output cache does not cross project or session identity', async () => {
  const f = fixture();
  await f.context.prompt('Continue', f.opened);
  f.context.projectId = 'project-2'; f.context.sessionId = 'session-2';
  await f.context.prompt('Continue', f.opened);
  assert.equal(f.reads.length, 2);
  assert.deepEqual(f.reads[1].slice(0, 2), ['project-2', 'session-2']);
});

test('ordinary cold chat cannot dispatch when writer admission fails', async () => {
  const f = fixture();
  f.context.loadPreferredSuggestionModel = async () => false;
  await f.context.submit();
  assert.equal(f.requests.length, 0);
  assert.equal(f.context.entry, 'Continue');
  assert.equal(f.context.dispatching, false);
  assert.equal(f.context.currentModel, undefined);
  f.assertImmutable();
});

for (const route of [null, 'unknown-route', () => { throw new Error('capability unavailable'); }]) {
  test(`unknown/unavailable native route cannot grant writer exemption (${String(route)})`, async () => {
    const f = fixture({ route });
    await f.context.submit();
    assert.equal(f.requests.length, 0);
    assert.equal(f.calls.filter(call => call.startsWith('load:')).length, 0);
    assert.equal(f.context.entry, 'Continue');
    assert.match(f.context.error, /route|capability unavailable/u);
  });
}

test('scope change while reading native capability cannot load a writer or dispatch stale text', async () => {
  const f = fixture({ route: context => { context.documentEpoch += 1; return 'loom'; } });
  await f.context.submit();
  assert.equal(f.requests.length, 0);
  assert.equal(f.calls.filter(call => call.startsWith('load:')).length, 0);
  assert.equal(f.context.entry, 'Continue');
});

test('explicit experimental Mom route preserves its profile and never activates the Loom writer', async () => {
  const f = fixture({ route: 'mom_experimental', loomModel: 'A', momModel: 'B' });
  f.context.entry = '@expert Continue';
  await f.context.submit();
  assert.equal(f.requests.length, 1, f.context.error);
  assert.equal(f.requests[0].writerAtIpc, null);
  assert.equal(f.calls.filter(call => call.startsWith('load:')).length, 0);
  assert.equal(f.requests[0].request.presentation.input, '@expert Continue');
  assert.equal(f.context.momProfile.persona.model_id, 'B');
  assert.equal(f.context.momProfile, f.mom);
});

test('non-chat panes retain the existing writer admission and do not query a chat capability', async () => {
  const f = fixture({ kind: 'browser', route: 'mom_experimental' });
  await f.context.submit();
  assert.equal(f.requests.length, 1, f.context.error);
  assert.equal(f.requests[0].writerAtIpc, 'A');
  assert.equal(f.calls.includes('route'), false);
});

test('history budget failure preserves the unsent input and dispatches nothing', async () => {
  const f = fixture();
  const huge = snapshot('old-answer', 'α'.repeat(40000), 'large-revision', 'large-blob');
  f.outputStore.set('old-answer', huge); f.context.documents = [huge.summary];
  await f.context.submit();
  assert.equal(f.requests.length, 0);
  assert.equal(f.context.entry, 'Continue');
  assert.match(f.context.error, /64 KiB/u);
  f.assertImmutable();
});

for (const [selected, other] of [['A', 'B'], ['B', 'A']]) {
  test(`explicit workspace profile ${selected} rejects resident ${other} and loads the selected writer`, async () => {
    const f = fixture({ loomModel: selected, momModel: other });
    f.context.workspaceTemplate.config.model = { profile: `profile-${selected}` };
    f.context.models[0].policy_verified = { profile_id: `profile-${selected}`, rank: 0 };
    const resident = writer(other, true);
    resident.policy_verified = { profile_id: `profile-${other}`, rank: 0 };
    f.context.models.push(resident);
    assert.equal(f.context.currentModel, undefined, 'the other loaded model cannot satisfy an explicit profile');
    await f.context.submit();
    assert.equal(f.requests.length, 1, f.context.error);
    assert.equal(f.requests[0].writerAtIpc, selected);
    assert(f.calls.includes(`load:/component/${selected}.gguf`));
    assert.equal(f.context.momProfile.persona.model_id, other);
  });
}
