/** Focused, dependency-light production-handler regressions.
 * Run: node --test tests/sidebar-interactions.node.mjs (TypeScript installed).
 * This transpiles real modules and extracts named real Svelte script handlers;
 * transport, DOM/focus objects and scheduling are injected. It is NOT a Svelte
 * compilation, browser, native filesystem-authority, or signed-product test.
 */
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const ts = require('typescript');
const src = fileURLToPath(new URL('../src/', import.meta.url));
const lib = resolve(src, 'lib');
function transpile(text, fileName) {
  const result = ts.transpileModule(text, { fileName, reportDiagnostics: true, compilerOptions: {
    module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, isolatedModules: true
  } });
  const errors = result.diagnostics?.filter(item => item.category === ts.DiagnosticCategory.Error) ?? [];
  assert.deepEqual(errors.map(item => ts.flattenDiagnosticMessageText(item.messageText, '\n')), [], fileName);
  return result.outputText;
}
function productionLoader(mutate = (_path, text) => text, externals = {}) {
  const cache = new Map();
  const load = path => {
    path = resolve(path);
    if (cache.has(path)) return cache.get(path).exports;
    assert(path.startsWith(`${lib}/`), `Out-of-scope test import ${path}`);
    const module = { exports: {} }; cache.set(path, module);
    const text = mutate(path, readFileSync(path, 'utf8'));
    const localRequire = name => {
      if (Object.hasOwn(externals, name)) return externals[name];
      assert(name.startsWith('.'), `Unexpected runtime dependency: ${name}`);
      return load(resolve(dirname(path), `${name}.ts`));
    };
    new Function('require', 'module', 'exports', transpile(text, path))(localRequire, module, module.exports);
    return module.exports;
  };
  return name => load(resolve(lib, `${name}.ts`));
}
const load = productionLoader();
const folders = load('workspaceFolders');
const sidebar = load('sidebarInteractions');
const primitives = load('interactionPrimitives');
const editing = load('textEditingInteractions');
const documents = load('documentContextActions');
const tree = load('workspaceTree');
const metadata = load('materialMetadata');

/** No rewritten handler logic: function declarations come from the current App. */
function handlers(file, names, bindings, mutate = text => text) {
  const content = mutate(readFileSync(resolve(src, file), 'utf8'));
  const start = content.indexOf('<script lang="ts">') + '<script lang="ts">'.length;
  const script = content.slice(start, content.indexOf('</script>', start));
  const ast = ts.createSourceFile(file + '.ts', script, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
  const code = names.map(name => {
    const declaration = ast.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === name);
    assert(declaration, `Missing production handler ${file}:${name}`);
    return declaration.getText(ast);
  }).join('\n');
  const keys = Object.keys(bindings);
  const declarations = keys.map(key => `let ${key} = bindings[${JSON.stringify(key)}];`).join('\n');
  const descriptors = keys.map(key => `${JSON.stringify(key)}: {get: () => ${key}, set: v => ${key} = v}`).join(',');
  return new Function('bindings', `${declarations}\n${transpile(code, file + '.ts')}\nreturn {
    ${names.join(',')}, state: Object.defineProperties({}, {${descriptors}})
  };`)(bindings);
}
function key(name, overrides = {}) {
  return { key: name, keyCode: 0, isComposing: false, ctrlKey: false, metaKey: false, altKey: false,
    shiftKey: false, defaultPrevented: false, stopped: false,
    preventDefault() { this.defaultPrevented = true; }, stopPropagation() { this.stopped = true; }, ...overrides };
}
const summary = { document_id: 'doc', relative_path: 'Notes/Draft.md', title: 'Draft', kind: 'prose',
  revision_id: 'revision', active_blob_id: 'a'.repeat(64), word_count: 2, externally_modified: false };
const project = () => ({ project_id: 'project', session_id: 'session', root: '/work/writing', title: 'writing',
  schema_version: 4, documents: [{ ...summary }], pending_recovery: 0 });
const bookmark = root => Object.freeze({ root, title: root });
const material = overrides => ({ id: 'material-a', name: 'Paper', reference: 'Paper.pdf', kind: 'attachment',
  source_path: '/external/Paper.pdf', workspace_path: null, attachment_id: 'b'.repeat(64),
  pinned: false, available: true, retention: 'ordinary', metadata_revision: 'a'.repeat(64), ...overrides });
const policy = { idle: true, editable: true, activeRoot: false, expanded: false, revealLabel: 'Reveal in Finder', searching: false };
function targets(p = project(), b = bookmark('/missing/writing'), m = material({})) {
  return [sidebar.captureSidebarTarget(p, { kind: 'root', bookmark: b }),
    sidebar.captureSidebarTarget(p, { kind: 'folder', path: 'Notes/', title: 'Notes' }),
    sidebar.captureSidebarTarget(p, { kind: 'document', summary: p.documents[0] }),
    sidebar.captureSidebarTarget(p, { kind: 'material', material: m })];
}
function capability(target, action) { return sidebar.sidebarCapabilities(target, policy).find(item => item.action === action); }
function deferred() { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
const pause = () => Promise.resolve();
class Control {
  isConnected = true; hidden = false; disabled = false; focused = 0; selected = 0; value = '';
  dataset = {}; children = [];
  focus() { this.focused++; }
  select() { this.selected++; }
  scrollIntoView() {}
  getClientRects() { return [1]; }
  getBoundingClientRect() { return { left: 0, right: 100, bottom: 40, width: 100, height: 40 }; }
  querySelectorAll() { return this.children; }
  closest() { return null; }
}
const appNames = [
  'sidebarLiveState', 'sidebarCapabilitiesFor', 'reconcileSidebarSelection', 'sidebarButtons', 'focusSidebarButton', 'restoreSidebarFocus',
  'selectSidebarRow', 'handleSidebarContextPointer', 'handleSidebarRowKeydown', 'activateSidebarRow',
  'openSidebarContextMenu', 'runSidebarContextAction', 'executeSidebarCapability',
  'beginWorkspaceLabelRename', 'cancelWorkspaceLabelRename', 'commitWorkspaceLabelRename',
  'handleWorkspaceRenameInput', 'handleWorkspaceRenameCompositionEnd',
  'closeDocumentContextMenu', 'documentContextMenuItems', 'focusDocumentContextMenu', 'handleDocumentContextMenuKeydown',
  'doOpenProject', 'beginMaterialRename', 'cancelMaterialRename', 'commitMaterialRename',
  'handleMaterialRenameInput', 'handleMaterialRenameCompositionEnd', 'handleMaterialRenameKeydown', 'handleMaterialRenameBlur'
];
function app(overrides = {}, mutate) {
  const errors = [], calls = [], writes = [], announcements = [];
  const p = project(), root = bookmark(p.root), missing = bookmark('/private/var/folders/gone/writing');
  const outline = new Control(), input = new Control();
  const props = {
    ...folders, ...sidebar, ...editing, ...documents, ...metadata, ...primitives,
    project: p, document: { summary: p.documents[0], text: 'unsaved manuscript', visible_blob_id: summary.active_blob_id },
    documentText: 'unsaved manuscript', saveState: 'dirty',
    workspaceFolders: [root, missing], fileRows: tree.workspaceRows(p.documents, new Set(), ''), materialEntries: [],
    componentMounted: true, applicationClosePhase: 'running', transition: 'idle', fileCommandInFlight: false,
    opening: false, documentContextActionInFlight: false, sidebarActionInFlight: false,
    renamingWorkspaceRoot: null, renamingMaterialTarget: null, renamingDocumentId: null, deleteDocumentTarget: null, editorReadonly: false,
    workspaceRootExpanded: true, collapsedFolders: new Set(), search: '', documentContextRevealLabel: 'Reveal in Finder',
    sidebarContextTarget: null, sidebarSelection: null, documentContextTarget: null, documentContextTrigger: null,
    documentContextPoint: { x: 0, y: 0 }, documentContextFocusIndex: 0, documentContextMenu: new Control(),
    outlineElement: outline, outlineToggle: new Control(),
    renameWorkspaceComposition: primitives.createRenameCompositionGuard(), renameWorkspaceTitle: '', renameWorkspaceInput: input,
    renameMaterialName: '', renameMaterialInput: input, renameMaterialInFlight: false,
    renameMaterialComposition: primitives.createRenameCompositionGuard(), materialRefreshSerial: 0,
    newUlid: () => '01ARZ3NDEKTSV4RRFFQ69G5FAV',
    renameMaterial: async () => { throw new Error('Unexpected rename'); },
    Element: Control, tick: pause,
    window: { localStorage: { setItem: (key, value) => writes.push({ key, value }) }, innerWidth: 1200, innerHeight: 800 },
    navigator: { clipboard: { writeText: async text => calls.push(['clipboard', text]) } },
    clearDocumentContextLongPress: () => {}, closeFormatMenu: () => {},
    focusConnectedControl: control => { if (!control?.isConnected) return false; control.focus(); return true; },
    focusCurrentWritingSurfaceAtEnd: () => calls.push(['focus-editor']),
    announce: text => announcements.push(text), recordFailure: error => errors.push(error),
    materialReferenceMarkdown: m => m.reference,
    pinMaterial: async (...args) => { calls.push(['pin', ...args]); return { ...props.materialEntries[0], pinned: args[3], metadata_revision: 'b'.repeat(64) }; },
    removeMaterial: async (...args) => { calls.push(['remove-source', ...args]); },
    materialRemoved: (...args) => calls.push(['project-source-removal', ...args]), materialChanged: m => calls.push(['project-pin', m]),
    refreshMaterials: async () => calls.push(['refresh-materials']), openMaterial: m => calls.push(['open-material', m]),
    runDocumentContextAction: async action => calls.push(['document-action', action]),
    prepareProjectOpenPath: async path => { calls.push(['prepare-path', path]); throw { code: 'selected_folder_unavailable', message: 'Selected folder is unavailable' }; },
    prepareProjectOpen: async () => { throw new Error('Unexpected picker'); },
    closeProject: async () => { throw new Error('Must not close an editor for an unavailable bookmark'); },
    reattachNativeProject: async () => { throw new Error('Must not reattach for a validation failure'); },
    clearFailure: () => {}, workspaceDocuments: {}, workspaceRestoreSerial: 1, modelRefreshSerial: 1,
    ...overrides
  };
  const instance = handlers('App.svelte', appNames, props, mutate);
  return { ...instance, errors, calls, writes, announcements, root, missing, input, outline };
}
function selectTarget(a, kind = 'root', object = a.missing) {
  const item = kind === 'root' ? { kind, bookmark: object } : { kind, material: object };
  return sidebar.captureSidebarTarget(a.state.project, item);
}

// Pure contracts, including old-behavior negative witnesses.
test('read labels and exact paths separately, retaining unavailable and arbitrarily spelled roots without writes', () => {
  const roots = ['/A/writing', '/a/writing', '/a/é', '/a/e\u0301', '/a/x/../writing', 'C:\\work\\writing'];
  const rows = folders.readWorkspaceFolders({ getItem: () => JSON.stringify(roots.map(root => ({ root, title: 'same' }))), setItem: () => assert.fail('read wrote storage') });
  assert.deepEqual(rows.map(row => row.root), roots);
  assert(rows.every(row => row.title === 'same'));
  assert.equal(new Set(folders.workspaceFolderLabels(rows).values()).size, roots.length);
  assert.equal(folders.workspaceFolderName(bookmark('/private/var/folders/really-long/gone')), 'gone');
});
test('shortest ancestor suffixes and alias collisions remain uniquely readable and deterministic', () => {
  const rows = [bookmark('/a/writing'), bookmark('/b/writing')];
  assert.deepEqual([...folders.workspaceFolderLabels(rows).values()], ['writing — a', 'writing — b']);
  const tricky = [bookmark('/a/writing'), bookmark('/a/writing/'), { root: '/c', title: 'writing — a · 1' }];
  const first = folders.workspaceFolderLabels(tricky), second = folders.workspaceFolderLabels([...tricky].reverse());
  assert.equal(new Set(first.values()).size, tricky.length);
  for (const row of tricky) assert.equal(first.get(row.root), second.get(row.root));
});
test('labels never change exact bookmark identity; reopening preserves custom label and lease', () => {
  const old = Object.freeze({ root: '/a', title: 'Novel' });
  const next = folders.rememberWorkspaceFolder([old], { root: '/a', title: 'a' });
  assert.equal(next[0], old);
  const renamed = folders.renameWorkspaceFolder(next, '/a', 'New novel', { setItem() {} });
  assert.equal(renamed[0].root, '/a'); assert.equal(old.title, 'Novel');
  assert.equal(sidebar.sidebarItemKey({ projectId: 'p', sessionId: 's' }, { kind: 'root', bookmark: old }),
    sidebar.sidebarItemKey({ projectId: 'other', sessionId: 'other' }, { kind: 'root', bookmark: renamed[0] }));
});
test('explicit storage failures reject before publication; invalid label and root bounds fail closed', () => {
  const rows = [bookmark('/a')], storage = { setItem() { throw new Error('quota'); } };
  assert.throws(() => folders.forgetWorkspaceFolder(rows, '/a', storage), /quota/);
  assert.throws(() => folders.renameWorkspaceFolder(rows, '/a', 'Name', storage), /quota/);
  for (const label of ['', '\u0000', '界'.repeat(86)]) assert.throws(() => folders.renameWorkspaceFolder(rows, '/a', label, { setItem() { assert.fail(); } }));
  assert.deepEqual(folders.readWorkspaceFolders({ getItem: () => 'bad JSON' }), []);
  assert.deepEqual(folders.readWorkspaceFolders({ getItem: () => JSON.stringify([{ root: 'x'.repeat(4097), title: 'x' }]) }), []);
  const many = Array.from({ length: 40 }, (_, i) => bookmark(`/r/${i}`));
  assert.equal(folders.readWorkspaceFolders({ getItem: () => JSON.stringify(many) }).length, 32);
});
test('forgetting an active root retains a live tree sentinel but no root row and no phantom bookmark', () => {
  const active = bookmark('/active'), other = bookmark('/other');
  const remaining = folders.forgetWorkspaceFolder([active, other], active.root, { setItem() {} });
  assert.deepEqual(folders.workspaceFolderGroups(remaining, active.root), [other, null]);
  assert.equal(folders.workspaceFolderGroups(remaining, active.root).some(row => row?.root === active.root), false);
});
test('negative control: the old auto-append-active decision fails the non-resurrection regression', () => {
  const mutant = productionLoader((path, text) => path.endsWith('/workspaceFolders.ts')
    ? text.replace('[...folders, null] : folders', "[...folders, { root: activeRoot, title: activeRoot }] : folders") : text)('workspaceFolders');
  assert.throws(() => assert.equal(mutant.workspaceFolderGroups([], '/missing').some(row => row?.root === '/missing'), false), assert.AssertionError);
});
test('negative control: forcing title=root fails the preserved readable-label regression', () => {
  const mutant = productionLoader((path, text) => path.endsWith('/workspaceFolders.ts')
    ? text.replace('title: item.title', 'title: item.root') : text)('workspaceFolders');
  assert.throws(() => assert.equal(mutant.readWorkspaceFolders({ getItem: () => '[{"root":"/tmp/writing","title":"My novel"}]' })[0].title, 'My novel'), assert.AssertionError);
});
test('all row kinds have nonempty typed capabilities; unsupported filesystem operations are absent', () => {
  const all = targets();
  assert.deepEqual(all.map(target => target.kind), ['root', 'folder', 'document', 'material']);
  for (const target of all) assert(sidebar.sidebarCapabilities(target, policy).length > 0);
  assert.deepEqual(sidebar.sidebarCapabilities(all[1], policy).map(item => item.action), ['toggle', 'copy_path']);
  assert.deepEqual(sidebar.sidebarCapabilities(all[0], policy).map(item => item.action), ['open', 'rename_label', 'copy_path', 'forget']);
  assert.equal(sidebar.sidebarCapabilities(all[2], policy).at(-1).action, 'delete');
  assert.equal('relative_path' in all[2].document, false);
  assert.equal('displayPath' in all[2].document, false);
  const protectedTarget = targets(project(), bookmark('/a'), material({ retention: 'protected' }))[3];
  assert.equal(capability(protectedTarget, 'remove').enabled, false);
  assert.equal(sidebar.sidebarCapabilities(all[1], { ...policy, searching: true })[0].enabled, false);
});
test('all scopes reject stale project/session; root remove-readd and material replacement invalidate leases', () => {
  const p = project(), b = bookmark('/a'), m = material({}), all = targets(p, b, m);
  const live = { project: p, bookmarks: [b], folders: ['Notes/'], materials: [m] };
  for (const target of all) {
    assert(sidebar.sidebarTargetIsCurrent(target, live));
    assert.equal(sidebar.sidebarTargetIsCurrent(target, { ...live, project: { ...p, session_id: 'new' } }), false);
    assert.equal(sidebar.sidebarTargetIsCurrent(target, { ...live, project: { ...p, project_id: 'new' } }), false);
  }
  assert.equal(sidebar.sidebarTargetIsCurrent(all[0], { ...live, bookmarks: [{ ...b }] }), false);
  assert.equal(sidebar.sidebarTargetIsCurrent(all[3], { ...live, materials: [{ ...m }] }), false);
  assert.equal(sidebar.sidebarTargetIsCurrent(all[2], { ...live, project: { ...p, documents: [{ ...summary, revision_id: 'new' }] } }), false);
});
test('Finder-style single selection commands, context keys, modifiers and IME stay distinct', () => {
  for (const [name, result] of Object.entries({ Enter: 'rename', F2: 'rename', ' ': 'open', ArrowUp: 'previous', ArrowDown: 'next', Home: 'first', End: 'last', ArrowRight: 'expand', ArrowLeft: 'collapse', Delete: 'remove', ContextMenu: 'context' })) assert.equal(sidebar.sidebarKeyAction(key(name)), result);
  assert.equal(sidebar.sidebarKeyAction(key('F10', { shiftKey: true })), 'context');
  assert.equal(sidebar.sidebarKeyAction(key('ArrowDown', { metaKey: true })), 'open');
  assert.equal(sidebar.sidebarKeyAction(key('Backspace', { metaKey: true })), 'remove');
  for (const event of [key('Enter', { keyCode: 229 }), key('Escape', { isComposing: true }), key('Enter', { altKey: true })]) assert.equal(sidebar.sidebarKeyAction(event), 'none');
});
test('shared rename guard owns Return/Escape, preserves IME default and defers blur exactly once', () => {
  for (const create of [primitives.createRenameCompositionGuard, documents.createDocumentRenameCompositionGuard]) {
    const composition = create(); let commits = 0, cancels = 0;
    const handle = event => primitives.handleInlineRenameKey(event, composition, () => commits++, () => cancels++);
    const enter = key('Enter'); handle(enter); assert(enter.defaultPrevented && enter.stopped); assert.equal(commits, 1);
    handle(key('Escape')); assert.equal(cancels, 1);
    const ime = key('Enter', { keyCode: 229 }); handle(ime); assert(!ime.defaultPrevented && ime.stopped); assert.equal(commits, 1);
    assert.equal(composition.blurShouldCommit(), false); assert.equal(composition.finish(), true); assert.equal(composition.finish(), false);
    composition.start(); composition.blurShouldCommit(); composition.reset(); assert.equal(composition.finish(), false);
  }
});
test('native text chords remain editing-owned; rich context history enters only its own transaction stack', () => {
  for (const k of ['a', 'c', 'v', 'x', 'z', 'y']) assert(editing.nativeEditingCommand(key(k, { metaKey: true })));
  let undo = 0, redo = 0;
  const input = type => ({ inputType: type, isComposing: false, prevented: false, preventDefault() { this.prevented = true; } });
  let event = input('historyUndo'); assert(editing.handleHistoryInput(event, 'prosemirror', false, () => ++undo > 0, () => ++redo > 0)); assert(event.prevented);
  event = input('historyRedo'); editing.handleHistoryInput(event, 'prosemirror', true, () => ++undo > 0, () => ++redo > 0); assert(event.prevented); assert.equal(redo, 0);
  event = input('historyUndo'); assert.equal(editing.handleHistoryInput(event, 'native', false, () => ++undo > 0, () => true), false); assert(!event.prevented); assert.equal(undo, 1);
});
test('ordinary source edit uses one native insert transaction, exact UTF-16 selection, no synthetic fallback', () => {
  const input = { value: 'α😀\n\tβ', selectionStart: 0, selectionEnd: 7, disabled: false, readOnly: false,
    setSelectionRange(a, b) { this.selectionStart = a; this.selectionEnd = b; },
    addEventListener() {}, removeEventListener() {}, dispatchEvent() {}, ownerDocument: null };
  const commands = [];
  input.ownerDocument = { activeElement: input, execCommand(command, ui, text) { commands.push([command, ui, text]); input.value = input.value.slice(0, input.selectionStart) + text + input.value.slice(input.selectionEnd); return true; } };
  assert(editing.applyNativeTextEdit(input, { value: 'α😀\nβ', selectionStart: 0, selectionEnd: 6 }));
  assert.deepEqual(commands, [['insertText', false, '']]); assert.equal(input.selectionEnd, 6);
  input.ownerDocument.execCommand = () => false;
  assert.equal(editing.applyNativeTextEdit(input, { value: 'x', selectionStart: 1, selectionEnd: 1 }), false);
  assert.equal(input.value, 'α😀\nβ'); assert.equal(input.selectionStart, 0); assert.equal(input.selectionEnd, 6);
});

// Actual App lifecycle handlers, with injected transport and focus scheduling.
test('real App missing-root context is handled and forgotten with zero filesystem/native operations', async () => {
  const a = app(), target = selectTarget(a), button = new Control();
  const event = key('contextmenu', { currentTarget: button, clientX: 12, clientY: 34 });
  a.handleSidebarContextPointer(event, target);
  assert(event.defaultPrevented && event.stopped); assert.equal(a.state.sidebarContextTarget, target);
  await a.runSidebarContextAction(capability(target, 'forget'));
  assert.equal(a.state.workspaceFolders.includes(a.missing), false); assert.equal(a.writes.length, 1);
  assert.deepEqual(a.calls, []); assert.equal(a.errors.length, 0);
});
test('real App forgetting active root never closes/checkpoints a dirty manuscript or detaches its project', async () => {
  const a = app(), target = selectTarget(a, 'root', a.root), beforeProject = a.state.project, beforeDocument = a.state.document;
  await a.executeSidebarCapability(capability(target, 'forget'), null);
  assert.equal(a.state.project, beforeProject); assert.equal(a.state.document, beforeDocument);
  assert.equal(a.state.documentText, 'unsaved manuscript'); assert.equal(a.state.saveState, 'dirty');
  assert.deepEqual(a.calls, []); assert.equal(a.state.workspaceRootExpanded, true);
  assert.equal(folders.workspaceFolderGroups(a.state.workspaceFolders, a.state.project.root).at(-1), null);
});
test('negative control: old no-removal/stranded bookmark behavior fails the actual App regression', async () => {
  const a = app({}, text => text.replace('workspaceFolders = forgetWorkspaceFolder(workspaceFolders, target.bookmark.root, window.localStorage);', 'workspaceFolders = [...workspaceFolders];'));
  await a.executeSidebarCapability(capability(selectTarget(a), 'forget'), null);
  assert.throws(() => assert.equal(a.state.workspaceFolders.includes(a.missing), false), assert.AssertionError);
});
test('real App failed open preserves current editor and missing bookmark, surfaces error and permits retry then removal', async () => {
  const a = app(), before = a.state.document;
  assert.equal(await a.doOpenProject(a.missing.root), false);
  assert.equal(await a.doOpenProject(a.missing.root), false);
  assert.equal(a.errors.length, 2); assert.equal(a.errors[0].code, 'selected_folder_unavailable');
  assert.equal(a.state.document, before); assert.equal(a.state.documentText, 'unsaved manuscript');
  assert(a.state.workspaceFolders.includes(a.missing));
  assert.deepEqual(a.calls, [['prepare-path', a.missing.root], ['prepare-path', a.missing.root]]);
  await a.executeSidebarCapability(capability(selectTarget(a), 'forget'), null);
  assert(!a.state.workspaceFolders.includes(a.missing)); assert.equal(a.calls.length, 2);
});
test('real App failed persistence does not remove the row or falsely announce success', async () => {
  const a = app({ window: { localStorage: { setItem() { throw new Error('Quota exceeded'); } } } });
  await a.executeSidebarCapability(capability(selectTarget(a), 'forget'), null);
  assert(a.state.workspaceFolders.includes(a.missing)); assert.equal(a.errors[0].message, 'Quota exceeded');
  assert.deepEqual(a.announcements, []); assert.deepEqual(a.calls, []);
});
test('real App detached old menu cannot mutate either old or newly selected workspace', async () => {
  const a = app(), old = selectTarget(a); a.state.sidebarContextTarget = old;
  a.state.project = { ...a.state.project, session_id: 'new-session' };
  const next = selectTarget(a); a.state.sidebarContextTarget = next;
  await a.runSidebarContextAction(capability(old, 'forget'));
  assert(a.state.workspaceFolders.includes(a.missing)); assert.deepEqual(a.writes, []);
  // Even a forged/replayed capability bypassing the menu lease fails the scope check.
  await a.executeSidebarCapability(capability(old, 'forget'), null);
  assert.deepEqual(a.writes, []);
});
test('real App rename begins with full selection, Return commits one label only, Escape and blur retain identity', async () => {
  const a = app(), target = selectTarget(a);
  await a.beginWorkspaceLabelRename(target); assert.equal(a.input.focused, 1); assert.equal(a.input.selected, 1);
  a.state.renameWorkspaceTitle = 'My research';
  primitives.handleInlineRenameKey(key('Enter'), a.state.renameWorkspaceComposition, () => a.commitWorkspaceLabelRename(), () => a.cancelWorkspaceLabelRename());
  assert.equal(a.state.workspaceFolders[1].title, 'My research'); assert.equal(a.state.workspaceFolders[1].root, a.missing.root);
  a.commitWorkspaceLabelRename(false); assert.equal(a.writes.length, 1);
  const renamed = selectTarget(a, 'root', a.state.workspaceFolders[1]);
  await a.beginWorkspaceLabelRename(renamed); a.state.renameWorkspaceTitle = 'Discard me';
  primitives.handleInlineRenameKey(key('Escape'), a.state.renameWorkspaceComposition, () => a.commitWorkspaceLabelRename(), () => a.cancelWorkspaceLabelRename());
  assert.equal(a.writes.length, 1); assert.equal(a.state.renamingWorkspaceRoot, null);
  await a.beginWorkspaceLabelRename(renamed); a.state.renameWorkspaceTitle = 'Blur label';
  if (a.state.renameWorkspaceComposition.blurShouldCommit()) a.commitWorkspaceLabelRename(false);
  assert.equal(a.writes.length, 2); assert.equal(a.state.workspaceFolders[1].title, 'Blur label'); assert.deepEqual(a.calls, []);
});
test('real App label IME blur-before-end commits only final text; stale rename and failed storage keep their boundaries', async () => {
  const a = app(), target = selectTarget(a); await a.beginWorkspaceLabelRename(target);
  a.state.renameWorkspaceComposition.start(); a.input.value = '途中'; a.handleWorkspaceRenameInput({ currentTarget: a.input });
  assert.equal(a.state.renameWorkspaceComposition.blurShouldCommit(), false);
  a.commitWorkspaceLabelRename(); assert.equal(a.writes.length, 0);
  a.input.value = '完成'; a.handleWorkspaceRenameCompositionEnd({ currentTarget: a.input }); assert.equal(a.writes.length, 1);
  assert.equal(a.state.workspaceFolders[1].title, '完成');
  await a.beginWorkspaceLabelRename(selectTarget(a, 'root', a.state.workspaceFolders[1]));
  a.state.project = { ...a.state.project, session_id: 'other' }; a.commitWorkspaceLabelRename(); assert.equal(a.writes.length, 1);
  const b = app({ window: { localStorage: { setItem() { throw new Error('read only'); } } } });
  await b.beginWorkspaceLabelRename(selectTarget(b)); b.state.renameWorkspaceTitle = 'Changed'; b.commitWorkspaceLabelRename(); await pause();
  assert(b.state.renamingWorkspaceRoot); assert.equal(b.input.focused, 2); assert.equal(b.errors.length, 1);
});
test('real App delayed focus restoration never steals focus in a new workspace or a newer popup', async () => {
  const gate = deferred(), a = app({ tick: () => gate.promise }), trigger = new Control();
  a.state.sidebarContextTarget = selectTarget(a); a.state.documentContextTrigger = trigger;
  a.closeDocumentContextMenu(); a.state.project = { ...a.state.project, session_id: 'other' };
  gate.resolve(); await pause(); assert.equal(trigger.focused, 0); assert.deepEqual(a.calls, []);
});
test('real App selection never implicitly opens/renames; ContextMenu and Shift-F10 do not invoke native menus', () => {
  for (const event of [key('ContextMenu'), key('F10', { shiftKey: true })]) {
    const a = app(), target = selectTarget(a), button = new Control(); event.currentTarget = button;
    a.selectSidebarRow({ target: button, currentTarget: button }, target);
    assert.equal(a.state.sidebarSelection, target.key); assert.equal(a.state.renamingWorkspaceRoot, null); assert.deepEqual(a.calls, []);
    a.handleSidebarRowKeydown(event, target); assert(event.defaultPrevented && event.stopped); assert.equal(a.state.sidebarContextTarget, target);
  }
});
test('real App capability recheck prevents arbitrary folder deletion, readonly document rename and concurrent mutation', async () => {
  const a = app();
  const folder = targets(a.state.project)[1];
  await a.executeSidebarCapability({ target: folder, action: 'delete', enabled: true }, null);
  a.state.editorReadonly = true;
  await a.executeSidebarCapability(capability(targets(a.state.project)[2], 'rename'), null);
  a.state.sidebarActionInFlight = true;
  await a.executeSidebarCapability(capability(selectTarget(a), 'forget'), null);
  assert.deepEqual(a.calls, []); assert.deepEqual(a.writes, []);
});
test('real App document actions retain the existing captured revision/blob owner', async () => {
  const a = app(), target = targets(a.state.project)[2];
  await a.executeSidebarCapability(capability(target, 'rename'), null);
  assert.deepEqual(a.calls, [['document-action', 'rename']]);
  assert.equal(a.state.documentContextTarget, target.document);
  assert.equal(a.state.documentContextTarget.expectedRevisionId, 'revision');
  assert.equal(a.state.documentContextTarget.expectedBlobId, summary.active_blob_id);
});
test('real App material mutation uses only native session/id, serializes awaits and discards stale receipts', async () => {
  const pending = deferred(), m = material({}), calls = [];
  const a = app({ materialEntries: [m], pinMaterial: (...args) => { calls.push(args); return pending.promise; } });
  const target = selectTarget(a, 'material', m), operation = a.executeSidebarCapability(capability(target, 'pin'), null);
  assert(a.state.fileCommandInFlight && a.state.sidebarActionInFlight);
  await a.executeSidebarCapability(capability(selectTarget(a), 'forget'), null); assert.deepEqual(a.writes, []);
  a.state.project = { ...a.state.project, session_id: 'replacement' };
  pending.resolve({ ...m, pinned: true }); await operation;
  assert.deepEqual(calls, [['project', 'session', m.id, true, m.metadata_revision]]); assert.deepEqual(a.calls, []);
  assert.equal(a.state.fileCommandInFlight, false); assert.equal(a.state.sidebarActionInFlight, false);
});
test('real App material remove receipt refreshes instead of deleting a replacement row with the same id', async () => {
  const pending = deferred(), m = material({}), a = app({ materialEntries: [m], removeMaterial: () => pending.promise });
  const operation = a.executeSidebarCapability(capability(selectTarget(a, 'material', m), 'remove'), null);
  a.state.materialEntries = [{ ...m }]; pending.resolve(); await operation;
  assert.deepEqual(a.calls, [['refresh-materials']]);
});

function terminal(overrides = {}, mutate) {
  const calls = [], input = { selectionStart: 0, selectionEnd: 0 };
  return { calls, ...handlers('lib/TerminalPane.svelte', ['handleKey'], {
    ...editing, composing: false, input, busy: true, disabled: false, runDisabled: false, embedded: false,
    entry: 'draft', historyIndex: -1, commands: ['older'], draft: '', runs: [], cleared: new Set(),
    onRun: () => calls.push('run'), onCancel: () => calls.push('cancel'), onClose: () => calls.push('close'), ...overrides
  }, mutate) };
}
test('real Terminal selected Ctrl-C copies normally, while unselected busy Ctrl-C interrupts once', () => {
  const a = terminal({ input: { selectionStart: 1, selectionEnd: 3 } }), copy = key('c', { ctrlKey: true });
  a.handleKey(copy); assert(!copy.defaultPrevented); assert.deepEqual(a.calls, []);
  a.state.input.selectionEnd = 1; const interrupt = key('c', { ctrlKey: true }); a.handleKey(interrupt);
  assert(interrupt.defaultPrevented && interrupt.stopped); assert.deepEqual(a.calls, ['cancel']);
});
test('real Terminal IME/legacy-229 Return and modified/selected arrow keys never submit or replace text', () => {
  const a = terminal({ busy: false });
  for (const event of [key('Enter', { keyCode: 229 }), key('Enter', { isComposing: true }), key('ArrowUp', { shiftKey: true }), key('ArrowUp', { metaKey: true })]) { a.handleKey(event); assert(!event.defaultPrevented); }
  a.state.input.selectionEnd = 3; a.handleKey(key('ArrowUp')); assert.equal(a.state.entry, 'draft'); assert.deepEqual(a.calls, []);
});
test('negative control: losing the selected-copy guard in real Terminal reproduces the original Ctrl-C theft', () => {
  const a = terminal({ input: { selectionStart: 1, selectionEnd: 3 } }, text => text.replace('terminalInterruptOwnsKey(event, busy, input.selectionStart, input.selectionEnd)', "event.ctrlKey && event.key.toLowerCase() === 'c'"));
  const event = key('c', { ctrlKey: true }); a.handleKey(event);
  assert.throws(() => assert.equal(event.defaultPrevented, false), assert.AssertionError);
});
test('real WorkspacePane composer guards IME/legacy keys and Alt-Return without changing chat submission', () => {
  let sent = 0;
  const a = handlers('lib/WorkspacePane.svelte', ['keydown'], { ...editing, composing: false, config: { kind: 'chat' }, submit: async () => sent++ });
  for (const event of [key('Enter', { keyCode: 229 }), key('Enter', { altKey: true }), key('Enter', { shiftKey: true })]) { a.keydown(event); assert(!event.defaultPrevented); }
  a.keydown(key('Enter')); assert.equal(sent, 1);
});
test('real SourceEditor composition guard runs before any completion/navigation/indentation logic', () => {
  const a = handlers('lib/SourceEditor.svelte', ['handleKeydown'], { ...editing, composing: false, currentPlan: () => assert.fail('IME entered editor command dispatch') });
  a.handleKeydown(key('Tab', { keyCode: 229 })); a.state.composing = true; a.handleKeydown(key('Escape'));
});
test('real App global routing reserves copy/cut/paste/undo/select-all and active composition for every text surface', () => {
  const a = handlers('App.svelte', ['handleGlobalKeydown'], { ...editing, textCompositionBoundary: editing.createTextCompositionBoundary(), textEditingElement: () => ({}) });
  for (const name of ['a', 'c', 'x', 'v', 'z', 'y']) { const event = key(name, { metaKey: true, shiftKey: true }); a.handleGlobalKeydown(event); assert(!event.defaultPrevented); }
  const target = {}; a.state.textCompositionBoundary.start({ target });
  const escape = key('Escape', { target }); a.handleGlobalKeydown(escape); assert(!escape.defaultPrevented);
  a.handleGlobalKeydown(key('Escape', { keyCode: 229 }));
});

test('real App clears a filtered-away selection without stealing focus or altering a newer scope', async () => {
  const a = app(); a.state.sidebarSelection = 'removed-row';
  await a.reconcileSidebarSelection('removed-row', a.state.project); assert.equal(a.state.sidebarSelection, null); assert.deepEqual(a.calls, []);
  const gate = deferred(), b = app({ tick: () => gate.promise }); b.state.sidebarSelection = 'same-root-key';
  const pending = b.reconcileSidebarSelection(b.state.sidebarSelection, b.state.project);
  b.state.project = { ...b.state.project, session_id: 'new' }; gate.resolve(); await pending;
  assert.equal(b.state.sidebarSelection, 'same-root-key');
});
test('real App Left during search moves to the parent rather than advertising an impossible collapse', () => {
  const a = app({ search: 'Draft' }), root = selectTarget(a, 'root', a.root), folder = targets(a.state.project)[1];
  const parent = new Control(), child = new Control(); parent.dataset.sidebarRow = root.key;
  child.dataset = { sidebarRow: folder.key, sidebarParent: root.key }; a.outline.children = [parent, child];
  a.handleSidebarRowKeydown(key('ArrowLeft', { currentTarget: child }), folder);
  assert.equal(parent.focused, 1); assert.equal(a.state.sidebarSelection, root.key); assert.deepEqual(a.calls, []);
});
test('native textarea command notification is delivered exactly once, whether the browser emits input or not', () => {
  for (const emitsInput of [false, true]) {
    const listeners = new Set(); let notifications = 0;
    const input = { value: 'plain', selectionStart: 0, selectionEnd: 0, disabled: false, readOnly: false,
      setSelectionRange(a, b) { this.selectionStart = a; this.selectionEnd = b; },
      addEventListener(_type, fn) { listeners.add(fn); }, removeEventListener(_type, fn) { listeners.delete(fn); },
      dispatchEvent(event) { notifications++; for (const fn of listeners) fn(event); }, ownerDocument: null };
    input.ownerDocument = { activeElement: input, execCommand(_command, _ui, text) {
      input.value = input.value.slice(0, input.selectionStart) + text + input.value.slice(input.selectionEnd);
      if (emitsInput) input.dispatchEvent(new Event('input')); return true;
    } };
    assert(editing.applyNativeTextEdit(input, { value: '\tplain', selectionStart: 1, selectionEnd: 1 }));
    assert.equal(notifications, 1); assert.equal(listeners.size, 0);
  }
});


test('real App reports explicit-open bookmark persistence failures after navigation; startup never repins', async () => {
  for (const remember of [false, true]) {
    const p = project(), storageError = new Error('storage denied'), calls = [];
    let reported = null;
    const instance = handlers('App.svelte', ['finishOpeningProject'], {
      ...folders, project: p, workspaceRestoreIsCurrent: () => true,
      closeDocumentContextMenu: () => {}, closeDocumentDeleteConfirmation: () => {},
      missingDocumentCapturePending: false, missingDocumentRecovery: null, missingDocumentCopyState: 'idle',
      missingDocumentRecoveryRequiresCopy: () => false, recordLocalFailure: () => assert.fail('Unexpected recovery failure'),
      workspaceFolders: [], workspaceRootExpanded: false, hiddenPaneSlots: new Set(), paneSelection: {},
      clearPreferredWriterRequest: () => {}, cancelSuggestionTimer: () => {}, clearCompletionSession: () => {},
      suggestionsEnabled: false, shuttleEnabled: false, completionController: {}, resetCompletionDiscovery: () => ({}),
      workspaceDocuments: {}, selectDocument: async () => { calls.push('select'); reported = null; }, tick: pause,
      window: { localStorage: { setItem: () => { calls.push('persist'); throw storageError; } } },
      recordFailure: error => { reported = error; }, scheduleProjectFilesystemRefresh: () => calls.push('refresh')
    });
    assert.equal(await instance.finishOpeningProject(p, 1, remember), true);
    assert.deepEqual(calls, remember ? ['select', 'persist', 'refresh'] : ['select', 'refresh']);
    assert.equal(reported, remember ? storageError : null);
    assert.equal(instance.state.workspaceFolders.length, remember ? 1 : 0);
    assert.equal(instance.state.project, p);
  }
});


// Continuation: metadata transactions and target/renderer lifetimes. These invoke
// the real production handlers, not an imitation of their decision rules.
function renameReceipt(request, before, fields = {}) {
  return { project_id: request.projectId, session_id: request.sessionId, request_id: request.requestId,
    expected_metadata_revision: request.expectedMetadataRevision,
    material: { ...before, name: request.name, metadata_revision: 'c'.repeat(64) }, ...fields };
}
test('material metadata names retain exact UTF-8 and are never interpreted as paths', () => {
  for (const name of ['  café 🦉  ', '../a/b', '@"someone"', '界'.repeat(170)]) {
    assert.equal(metadata.materialDisplayNameError(name), null);
  }
  for (const name of ['', '  ', 'a\n', '\0', '界'.repeat(171)]) assert(metadata.materialDisplayNameError(name));
  for (const m of [material({ metadata_revision: null }), material({ kind: 'folder' }), material({ retention: 'protected' })]) {
    assert.equal(metadata.canEditMaterialMetadata(m), false);
    assert.throws(() => metadata.materialMetadataRevision(m));
    assert.equal(capability(targets(project(), bookmark('/root'), m)[3], 'rename').enabled, false);
  }
});
test('every receipt dimension is checked; mutable labels cannot substitute source authority', () => {
  const before = material({}), request = { projectId: 'p', sessionId: 's', id: before.id,
    expectedMetadataRevision: before.metadata_revision, requestId: 'r', name: 'Renamed' };
  const receipt = renameReceipt(request, before);
  assert(metadata.materialRenameReceiptMatches(request, before, receipt));
  for (const key of ['project_id', 'session_id', 'request_id', 'expected_metadata_revision']) {
    assert.equal(metadata.materialRenameReceiptMatches(request, before, { ...receipt, [key]: 'wrong' }), false, key);
  }
  const badFields = { id: 'replacement', reference: '@"replacement"', name: 'different', kind: 'library',
    source_path: '/elsewhere', attachment_id: 'd'.repeat(64), workspace_path: 'moved/a',
    retention: 'protected', pinned: true, available: false, metadata_revision: before.metadata_revision };
  for (const [key, value] of Object.entries(badFields)) {
    assert.equal(metadata.materialRenameReceiptMatches(request, before,
      { ...receipt, material: { ...receipt.material, [key]: value } }), false, key);
  }
});
test('real App source rename captures native metadata generation and keeps dirty text and chat drafts untouched', async () => {
  const before = material({ workspace_path: 'Notes/Paper.pdf' }), requests = [];
  const a = app({ materialEntries: [before], pendingChat: 'not sent',
    renameMaterial: async request => { requests.push(request); return renameReceipt(request, before); },
    flushEditors: () => assert.fail('metadata rename must not flush manuscript'),
    checkpoint: () => assert.fail('metadata rename must not checkpoint manuscript') });
  const target = selectTarget(a, 'material', before);
  await a.executeSidebarCapability(capability(target, 'rename'), null);
  assert.equal(a.state.renamingMaterialTarget, target); assert.equal(a.input.selected, 1);
  const exact = '  café 🦉 / display-only  ';
  a.input.value = exact; a.handleMaterialRenameInput({ currentTarget: a.input });
  await a.commitMaterialRename(false);
  assert.deepEqual(requests, [{ projectId: 'project', sessionId: 'session', id: before.id,
    expectedMetadataRevision: before.metadata_revision, requestId: '01ARZ3NDEKTSV4RRFFQ69G5FAV', name: exact }]);
  assert.equal(a.state.documentText, 'unsaved manuscript'); assert.equal(a.state.saveState, 'dirty');
  assert.equal(a.state.pendingChat, 'not sent'); assert.equal(a.state.renamingMaterialTarget, null);
  assert.equal(a.calls[0][0], 'project-pin'); assert.equal(a.calls[0][1].name, exact);
  assert.deepEqual(a.errors, []); assert.deepEqual(a.writes, []);
});
test('real App duplicate Return and blur submit exactly once; Escape cannot cancel a committed-in-flight operation', async () => {
  const before = material({}), pending = deferred(), requests = [];
  const a = app({ materialEntries: [before], renameMaterial: request => { requests.push(request); return pending.promise; } });
  await a.beginMaterialRename(selectTarget(a, 'material', before));
  a.state.renameMaterialName = 'Changed'; const first = a.commitMaterialRename(false);
  await a.commitMaterialRename(false); a.handleMaterialRenameBlur({ currentTarget: a.input }); a.cancelMaterialRename();
  assert.equal(requests.length, 1); assert(a.state.renamingMaterialTarget); assert(a.state.fileCommandInFlight);
  pending.resolve(renameReceipt(requests[0], before)); await first;
  assert.equal(a.state.renamingMaterialTarget, null); assert.equal(a.state.fileCommandInFlight, false);
});
test('real App material IME blur-before-compositionend commits only final exact input', async () => {
  const before = material({}), requests = [];
  const a = app({ materialEntries: [before], renameMaterial: async request => { requests.push(request); return renameReceipt(request, before); } });
  await a.beginMaterialRename(selectTarget(a, 'material', before));
  a.state.renameMaterialComposition.start(); a.input.value = '途中'; a.handleMaterialRenameInput({ currentTarget: a.input });
  const enter = key('Enter', { keyCode: 229, currentTarget: a.input }); a.handleMaterialRenameKeydown(enter);
  assert.equal(enter.defaultPrevented, false); a.handleMaterialRenameBlur({ currentTarget: a.input });
  assert.deepEqual(requests, []);
  a.input.value = '  完成🦉  '; a.handleMaterialRenameCompositionEnd({ currentTarget: a.input });
  await pause(); await pause(); assert.equal(requests.length, 1); assert.equal(requests[0].name, a.input.value);
});
test('real App failed receipt retains exact rename input, refreshes only, and never rebases onto a replaced row', async () => {
  const before = material({}), requests = [];
  const a = app({ materialEntries: [before], renameMaterial: async request => {
    requests.push(request); return renameReceipt(request, before, { request_id: 'stale' });
  } });
  await a.beginMaterialRename(selectTarget(a, 'material', before)); a.state.renameMaterialName = '  not lost  ';
  await a.commitMaterialRename(false);
  assert.equal(a.errors.length, 1); assert.equal(a.state.renameMaterialName, '  not lost  ');
  assert.deepEqual(a.calls, [['refresh-materials']]);
  a.state.materialEntries = [{ ...before, metadata_revision: 'd'.repeat(64) }];
  await a.commitMaterialRename(false); assert.equal(requests.length, 1);
  assert.equal(a.state.renameMaterialName, '  not lost  ');
});
test('real App response for a previous session cannot rename, refocus or report into the next session', async () => {
  const before = material({}), pending = deferred(); let request;
  const a = app({ materialEntries: [before], renameMaterial: captured => { request = captured; return pending.promise; } });
  await a.beginMaterialRename(selectTarget(a, 'material', before)); a.state.renameMaterialName = 'Old workspace';
  const operation = a.commitMaterialRename(); const focuses = a.input.focused;
  a.state.project = { ...a.state.project, session_id: 'new' }; a.state.materialEntries = [material({ name: 'New workspace' })];
  pending.resolve(renameReceipt(request, before)); await operation;
  assert.deepEqual(a.calls, []); assert.deepEqual(a.errors, []); assert.equal(a.input.focused, focuses);
  assert.equal(a.state.materialEntries[0].name, 'New workspace');
});
test('real App detached input, blur, key and composition events cannot edit or commit the next material rename', async () => {
  const before = material({}), a = app({ materialEntries: [before] }), stale = new Control();
  await a.beginMaterialRename(selectTarget(a, 'material', before)); a.state.renameMaterialName = 'intact';
  stale.value = 'must not leak'; a.handleMaterialRenameInput({ currentTarget: stale });
  a.handleMaterialRenameBlur({ currentTarget: stale });
  a.handleMaterialRenameKeydown(key('Escape', { currentTarget: stale }));
  a.state.renameMaterialComposition.start(); a.handleMaterialRenameCompositionEnd({ currentTarget: stale });
  assert.equal(a.state.renameMaterialName, 'intact'); assert(a.state.renameMaterialComposition.active);
  assert(a.state.renamingMaterialTarget); assert.deepEqual(a.calls, []);
});
test('negative control: removing the native metadata-revision receipt check fails stale receipt rejection', () => {
  const mutant = productionLoader((path, text) => path.endsWith('/materialMetadata.ts')
    ? text.replace('receipt.expected_metadata_revision === request.expectedMetadataRevision', 'true') : text)('materialMetadata');
  const before = material({}), request = { projectId: 'p', sessionId: 's', id: before.id,
    expectedMetadataRevision: before.metadata_revision, requestId: 'r', name: 'new' };
  const receipt = renameReceipt(request, before, { expected_metadata_revision: 'e'.repeat(64) });
  assert.throws(() => assert.equal(mutant.materialRenameReceiptMatches(request, before, receipt), false), assert.AssertionError);
});
function materialProjection(overrides = {}, mutate) {
  const calls = [], errors = [];
  return { calls, errors, ...handlers('App.svelte', ['refreshMaterials', 'materialChanged', 'materialRemoved'], {
    ...metadata, componentMounted: true, project: project(), materialEntries: [], materialRefreshSerial: 0, activeMaterial: null,
    listMaterials: async () => [], recordFailure: error => errors.push(error),
    closeMaterial: () => calls.push('close'), ...overrides
  }, mutate) };
}
test('real App latest-started list response wins; stale errors are suppressed', async () => {
  const first = deferred(), second = deferred(), queue = [first, second];
  const a = materialProjection({ listMaterials: () => queue.shift().promise });
  const one = a.refreshMaterials(), two = a.refreshMaterials(), current = material({ name: 'current' });
  second.resolve([current]); await two; first.resolve([material({ name: 'old' })]); await one;
  assert.equal(a.state.materialEntries[0], current); assert.deepEqual(a.errors, []);
});
test('real App mutation invalidates a pre-mutation list without remounting retained evidence', async () => {
  const pending = deferred(), old = material({}), changed = material({ name: 'changed', metadata_revision: 'c'.repeat(64) });
  const a = materialProjection({ materialEntries: [old], activeMaterial: old, listMaterials: () => pending.promise });
  const operation = a.refreshMaterials(); a.materialChanged(changed); pending.resolve([old]); await operation;
  assert.equal(a.state.materialEntries[0], changed); assert.equal(a.state.activeMaterial, changed); assert.deepEqual(a.calls, []);
});
test('negative control: removing the list serial check reproduces a renamed label resurrection', async () => {
  const pending = deferred(), old = material({}), changed = material({ name: 'changed' });
  const a = materialProjection({ materialEntries: [old], listMaterials: () => pending.promise },
    text => text.replace('request === materialRefreshSerial && project?.project_id', 'project?.project_id'));
  const operation = a.refreshMaterials(); a.materialChanged(changed); pending.resolve([old]); await operation;
  assert.throws(() => assert.equal(a.state.materialEntries[0], changed), assert.AssertionError);
});
test('real App view removal carrying a replaced lease refreshes without removing the re-added source', async () => {
  const old = material({}), replacement = material({ name: 'replacement' }), pending = deferred();
  const a = materialProjection({ materialEntries: [replacement], activeMaterial: replacement, listMaterials: () => pending.promise });
  a.materialRemoved(old.id, 'session', old);
  assert.equal(a.state.materialEntries[0], replacement); assert.deepEqual(a.calls, []);
  pending.resolve([replacement]); await pause();
});
test('rename markup has one event owner rather than an unguarded two-way binding on detached inputs', () => {
  const text = readFileSync(resolve(src, 'App.svelte'), 'utf8');
  for (const name of ['renameWorkspaceTitle', 'renameDocumentTitle', 'renameMaterialName']) {
    assert.equal(text.includes(`bind:value={${name}}`), false, name);
    assert(text.includes(`value={${name}}`), name);
  }
});

test('real App same-generation list preserves an active rename lease; a new generation never does', async () => {
  const before = material({}); let entries = [{ ...before }];
  const a = materialProjection({ materialEntries: [before], listMaterials: async () => entries });
  await a.refreshMaterials(); assert.equal(a.state.materialEntries[0], before);
  entries = [{ ...before, metadata_revision: 'c'.repeat(64) }];
  await a.refreshMaterials(); assert.notEqual(a.state.materialEntries[0], before);
});
test('real App a stale list rejection never reports after a newer list succeeds', async () => {
  const old = deferred(), next = deferred(), queue = [old, next];
  const a = materialProjection({ listMaterials: () => queue.shift().promise });
  const first = a.refreshMaterials(), second = a.refreshMaterials();
  next.resolve([material({})]); await second; old.reject(new Error('stale error')); await first;
  assert.deepEqual(a.errors, []);
});
test('real App detached document rename events cannot alter the next document title, composition or transaction', () => {
  const input = new Control(), old = new Control(); old.value = 'wrong title';
  const a = handlers('App.svelte', ['synchronizeDocumentRenameTitle', 'handleDocumentRenameInput',
    'handleDocumentRenameCompositionStart', 'handleDocumentRenameCompositionEnd', 'handleDocumentRenameBlur', 'handleDocumentRenameKeydown'], {
    ...primitives, ...documents, renameDocumentInput: input, renameDocumentTitle: 'intact',
    renameDocumentComposition: primitives.createRenameCompositionGuard(), renameDocumentInFlight: false,
    commitDocumentRename: () => assert.fail('detached input committed'), cancelDocumentRename: () => assert.fail('detached input cancelled')
  });
  a.handleDocumentRenameInput({ currentTarget: old });
  a.handleDocumentRenameCompositionStart({ currentTarget: old });
  a.handleDocumentRenameCompositionEnd({ currentTarget: old });
  a.handleDocumentRenameBlur({ currentTarget: old }); a.handleDocumentRenameKeydown(key('Enter', { currentTarget: old }));
  assert.equal(a.state.renameDocumentTitle, 'intact'); assert.equal(a.state.renameDocumentComposition.active, false);
});


test('real IPC orders metadata rename behind pending draft persistence and snapshots the exact request', async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  Object.defineProperty(globalThis, 'window', { configurable: true, value: { __TAURI_INTERNALS__: {} } });
  const pending = deferred(), calls = [];
  const ipc = productionLoader(undefined, {
    '@tauri-apps/api/core': { invoke: async (command, args) => {
      calls.push([command, args]);
      if (command === 'plugin:loom|document_draft_upsert') return pending.promise;
      return undefined;
    } }, '@tauri-apps/api/event': { listen: async () => () => {} }
  })('ipc');
  try {
    const draft = ipc.upsertTransientDraft('p', 's', 'doc', 'Draft.md', 'exact café 🦉\r\n', 'prose', 'revision', '0');
    const request = { projectId: 'p', sessionId: 's', id: 'material-id', requestId: 'request', expectedMetadataRevision: 'a'.repeat(64), name: 'exact name' };
    const rename = ipc.renameMaterial(request); request.name = 'late mutation';
    const pin = ipc.pinMaterial('p', 's', request.id, true, 'b'.repeat(64));
    const remove = ipc.removeMaterial('p', 's', request.id, 'c'.repeat(64));
    await pause(); await pause();
    assert.deepEqual(calls.map(([command]) => command), ['plugin:loom|document_draft_upsert']);
    pending.resolve({ testTransportReceipt: true }); // ordering only, not a native draft receipt proof
    await Promise.all([draft, rename, pin, remove]);
    assert.deepEqual(calls.map(([command]) => command), ['plugin:loom|document_draft_upsert', 'plugin:loom|material_rename', 'plugin:loom|material_set_pinned', 'plugin:loom|material_remove']);
    assert.equal(calls[0][1].text, 'exact café 🦉\r\n'); assert.equal(calls[1][1].name, 'exact name');
    assert.equal(calls[1][1].expectedMetadataRevision, 'a'.repeat(64));
    assert.equal(calls[2][1].expectedMetadataRevision, 'b'.repeat(64)); assert.equal(calls[3][1].expectedMetadataRevision, 'c'.repeat(64));
  } finally {
    if (oldWindow) Object.defineProperty(globalThis, 'window', oldWindow); else Reflect.deleteProperty(globalThis, 'window');
  }
});
test('real IPC does not replay a failed metadata mutation and leaves subsequent observation usable', async () => {
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  Object.defineProperty(globalThis, 'window', { configurable: true, value: { __TAURI_INTERNALS__: {} } });
  const calls = [], ipc = productionLoader(undefined, {
    '@tauri-apps/api/core': { invoke: async command => {
      calls.push(command);
      if (command === 'plugin:loom|material_rename') throw { code: 'material_failed', message: 'commit status uncertain', retryable: true };
      return [];
    } }, '@tauri-apps/api/event': { listen: async () => () => {} }
  })('ipc');
  try {
    await assert.rejects(ipc.renameMaterial({ projectId: 'p', sessionId: 's', id: 'm', requestId: 'r', expectedMetadataRevision: 'a'.repeat(64), name: 'new' }));
    assert.deepEqual(await ipc.listMaterials('p', 's'), []);
    assert.deepEqual(calls, ['plugin:loom|material_rename', 'plugin:loom|material_list']);
  } finally {
    if (oldWindow) Object.defineProperty(globalThis, 'window', oldWindow); else Reflect.deleteProperty(globalThis, 'window');
  }
});
function materialView(overrides = {}) {
  const changed = [], removed = [];
  return { changed, removed, ...handlers('lib/MaterialView.svelte', ['pin', 'remove'], {
    ...metadata, material: material({}), projectId: 'p', sessionId: 's', mounted: true, busy: false, serial: 0, error: '', removable: true,
    normalizeFailure: failure => ({ message: String(failure.message ?? failure) }),
    pinMaterial: async () => assert.fail('unexpected pin'), removeMaterial: async () => assert.fail('unexpected removal'),
    onChanged: value => changed.push(value), onRemoved: (...args) => removed.push(args), ...overrides
  }) };
}
test('real MaterialView rejects a stale pin response after a new source lease and preserves retained passage text', async () => {
  const before = material({}), pending = deferred(), calls = [], selected = { id: 'evidence', text: '  retained café 🦉\r\n' };
  const a = materialView({ material: before, selected, pinMaterial: (...args) => { calls.push(args); return pending.promise; } });
  const operation = a.pin(), replacement = material({ name: 'replacement', metadata_revision: 'c'.repeat(64) });
  a.state.material = replacement; pending.resolve({ ...before, pinned: true, metadata_revision: 'b'.repeat(64) }); await operation;
  assert.deepEqual(calls, [['p', 's', before.id, true, before.metadata_revision]]);
  assert.deepEqual(a.changed, []); assert.equal(a.state.material, replacement); assert.equal(a.state.selected, selected); assert.equal(a.state.error, '');
});
test('real MaterialView refuses a pin receipt that substitutes the retained source', async () => {
  const before = material({});
  const a = materialView({ material: before, pinMaterial: async () => ({ ...before, pinned: true, attachment_id: 'c'.repeat(64) }) });
  await a.pin(); assert.equal(a.state.material, before); assert.deepEqual(a.changed, []); assert(a.state.error.includes('receipt'));
});
test('real MaterialView removal sends the observed generation and returns the original lease even after unmount', async () => {
  const before = material({}), pending = deferred(), calls = [];
  const a = materialView({ material: before, removeMaterial: (...args) => { calls.push(args); return pending.promise; } });
  const operation = a.remove(); a.state.mounted = false; pending.resolve(); await operation;
  assert.deepEqual(calls, [['p', 's', before.id, before.metadata_revision]]);
  assert.deepEqual(a.removed, [[before.id, 's', before]]);
});


test('real App removal during an in-flight rename releases the rename owner after native completion', async () => {
  const before = material({}), pending = deferred(); let request;
  const a = app({ materialEntries: [before], renameMaterial: value => { request = value; return pending.promise; } });
  await a.beginMaterialRename(selectTarget(a, 'material', before)); a.state.renameMaterialName = 'Changed';
  const operation = a.commitMaterialRename(), focused = a.input.focused;
  a.state.materialEntries = []; a.cancelMaterialRename(false); // reactive disappearance while the owner is in flight
  assert(a.state.renamingMaterialTarget);
  pending.resolve(renameReceipt(request, before)); await operation;
  assert.equal(a.state.renamingMaterialTarget, null); assert.equal(a.state.renameMaterialInFlight, false);
  assert.equal(a.state.fileCommandInFlight, false); assert.equal(a.input.focused, focused);
  assert.deepEqual(a.calls, [['refresh-materials']]);
});
test('real App acknowledges an exact rename generation already published by a read without replay or stale rebasing', async () => {
  const before = material({}), pending = deferred(); let request, invocations = 0;
  const a = app({ materialEntries: [before], renameMaterial: value => { request = value; invocations++; return pending.promise; } });
  await a.beginMaterialRename(selectTarget(a, 'material', before)); a.state.renameMaterialName = 'Changed';
  const operation = a.commitMaterialRename(false), receipt = renameReceipt(request, before);
  a.state.materialEntries = [{ ...receipt.material }]; pending.resolve(receipt); await operation;
  assert.equal(a.state.renamingMaterialTarget, null); assert.equal(invocations, 1);
  assert.deepEqual(a.calls, []); assert.deepEqual(a.errors, []);
});
test('real App ignores rename completion and failure after unmount without publication, reread or focus', async () => {
  for (const reject of [false, true]) {
    const before = material({}), pending = deferred(); let request;
    const a = app({ materialEntries: [before], renameMaterial: value => { request = value; return pending.promise; } });
    await a.beginMaterialRename(selectTarget(a, 'material', before)); a.state.renameMaterialName = 'Changed';
    const operation = a.commitMaterialRename(), focused = a.input.focused;
    a.state.componentMounted = false;
    if (reject) pending.reject(new Error('late failure')); else pending.resolve(renameReceipt(request, before));
    await operation;
    assert.equal(a.state.renamingMaterialTarget, null); assert.equal(a.state.materialEntries[0], before);
    assert.equal(a.input.focused, focused); assert.deepEqual(a.calls, []); assert.deepEqual(a.errors, []);
  }
});
test('real App ignores material list and view callbacks after unmount', async () => {
  for (const reject of [false, true]) {
    const before = material({}), pending = deferred();
    const a = materialProjection({ materialEntries: [before], listMaterials: () => pending.promise });
    const operation = a.refreshMaterials(); a.state.componentMounted = false;
    if (reject) pending.reject(new Error('late list')); else pending.resolve([material({ name: 'Changed' })]);
    await operation;
    a.materialRemoved(before.id, 'session', before); a.materialChanged(material({ name: 'Changed' }));
    assert.equal(a.state.materialEntries[0], before); assert.deepEqual(a.calls, []); assert.deepEqual(a.errors, []);
  }
});


test('real App and MaterialView both reject a pin receipt with an unchanged metadata generation', async () => {
  const before = material({});
  const a = app({ materialEntries: [before], pinMaterial: async () => ({ ...before, pinned: true }) });
  const target = selectTarget(a, 'material', before);
  await a.executeSidebarCapability(capability(target, 'pin'), null);
  assert.equal(a.errors.length, 1); assert(a.errors[0].message.includes('receipt'));
  assert.deepEqual(a.calls, [['refresh-materials']]);
  const view = materialView({ material: before, pinMaterial: async () => ({ ...before, pinned: true }) });
  await view.pin(); assert.equal(view.state.material, before); assert.deepEqual(view.changed, []);
  assert(view.state.error.includes('receipt'));
});

// A selected row must keep its original lease through the actual open/remove
// owner chain; a presentation copy otherwise strands the removed source view.
test('real App source opening preserves the row lease required to close its removed view', async () => {
  const before = material({});
  const a = app({ materialEntries: [before] });
  const target = selectTarget(a, 'material', before);
  await a.executeSidebarCapability(capability(target, 'open'));
  const opened = a.calls.find(call => call[0] === 'open-material')[1];
  const owner = materialProjection({ materialEntries: [before], activeMaterial: opened });
  owner.materialRemoved(before.id, 'session', opened);
  assert.deepEqual(owner.calls, ['close']);
  assert.equal(owner.state.materialEntries.length, 0);
});
