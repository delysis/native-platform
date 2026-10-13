import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';

class FakeClassList {
  constructor() {
    this.values = new Set();
  }

  add(...values) {
    for (const value of values) this.values.add(value);
  }

  remove(...values) {
    for (const value of values) this.values.delete(value);
  }

  contains(value) {
    return this.values.has(value);
  }

  toggle(value, force) {
    const enabled = force ?? !this.values.has(value);
    if (enabled) this.values.add(value);
    else this.values.delete(value);
    return enabled;
  }
}

class FakeElement {
  constructor(id = '') {
    this.id = id;
    this.children = [];
    this.classList = new FakeClassList();
    this.dataset = {};
    this.disabled = false;
    this.hidden = false;
    this.listeners = new Map();
    this.textContent = '';
    this.value = '';
  }

  get childElementCount() {
    return this.children.length;
  }

  get options() {
    return this.children;
  }

  get lastElementChild() {
    return this.children.at(-1);
  }

  addEventListener(name, listener) {
    this.listeners.set(name, listener);
  }

  append(...children) {
    this.children.push(...children);
  }

  replaceChildren(...children) {
    this.children = [...children];
  }

  setAttribute(name, value) {
    this[name] = value;
  }

  focus() {}

  remove() {}
}

const elementIds = [
  'runtime-banner',
  'runtime-shell-status',
  'stat-headroom',
  'stat-latency',
  'stat-tokens',
  'stat-tokens-context',
  'stat-requests',
  'live-health-list',
  'onboarding-grid',
  'chat-model',
  'playground-model-note',
  'proxy-status',
  'proxy-binding',
  'proxy-status-pill',
  'proxy-token-path',
  'setting-port',
  'logs-empty',
  'logs-body',
  'profile-email',
  'profile-name',
  'profile-password-hint',
  'refresh-dashboard',
  'refresh-logs',
  'chat-form',
  'playground-mode',
  'chat-input',
  'chat-send',
  'chat-history',
  'playground-subtitle',
  'proxy-form',
  'restart-proxy',
  'workspace-context',
  'local-model-status-pill',
  'local-model-name',
  'local-model-detail',
  'local-model-sha256',
  'choose-local-model',
  'toast',
];

test('direct HTML loading renders an explicit non-interactive preview', async () => {
  const elements = new Map(elementIds.map((id) => [id, new FakeElement(id)]));
  elements.get('runtime-banner').hidden = true;

  const navItems = ['dashboard', 'setup', 'chat', 'logs', 'settings'].map((view) => {
    const item = new FakeElement();
    item.dataset.view = view;
    item.dataset.label = view;
    return item;
  });
  const runtimeControls = elementIds
    .filter((id) => !['runtime-banner', 'runtime-shell-status', 'workspace-context'].includes(id))
    .map((id) => elements.get(id));
  const body = new FakeElement('body');

  globalThis.window = { __TAURI__: undefined };
  globalThis.document = {
    body,
    createElement: () => new FakeElement(),
    getElementById: (id) => elements.get(id),
    querySelector: () => null,
    querySelectorAll: (selector) => {
      if (selector === '.nav-item') return navItems;
      if (selector === 'main button, main input, main select, main textarea') {
        return runtimeControls;
      }
      return [];
    },
  };

  await import(`../src/main.js?preview-test=${Date.now()}`);

  assert.equal(elements.get('runtime-banner').hidden, false);
  assert.equal(elements.get('runtime-shell-status').textContent, 'Preview only');
  assert.equal(elements.get('live-health-list').children[0].children[0].textContent, 'Desktop connection required');
  assert.equal(elements.get('chat-model').children[0].textContent, 'Desktop application required');
  assert.equal(elements.get('proxy-status').textContent, 'Unavailable in interface preview.');
  assert.equal(elements.get('local-model-status-pill').textContent, 'Offline');
  assert.equal(elements.get('local-model-name').textContent, 'Desktop application required');
  assert.ok(runtimeControls.every((control) => control.disabled));
  assert.ok(navItems.every((item) => !item.disabled));
  assert.ok(body.classList.contains('preview-mode'));
});

test('desktop setup reports saved-model failure and picker success without exposing a path', async () => {
  const elements = new Map(elementIds.map((id) => [id, new FakeElement(id)]));
  elements.get('playground-mode').value = 'chat';
  elements.get('setting-port').value = '1337';
  const body = new FakeElement('body');
  const calls = [];
  const invoke = async (command, arguments_) => {
    calls.push([command, arguments_]);
    switch (command) {
      case 'get_dashboard_stats':
        return { headroom: 0, avg_latency: 0, total_tokens: 0, request_count: 0 };
      case 'get_providers':
        return [];
      case 'get_models':
        return [{
          id: 'auto',
          display_name: 'Automatic best available route',
          providers: ['Hosted provider one', 'Hosted provider two'],
          supports_chat_completions: true,
          supports_text_completions: false,
          prompt_semantics: [],
        }];
      case 'get_master_profile':
        return {};
      case 'get_recent_logs':
        return [499, 503].map((status) => ({
          timestamp: '2026-09-09 12:00:00', provider: 'Local llama.cpp',
          model: 'local/default', tokens: 12, latency: 25, status,
        }));
      case 'plugin:free-token-energy|loopback_status':
        return { enabled: false, addresses: [], token_path: null };
      case 'get_local_model_status':
        return {
          state: 'invalid',
          display_name: 'missing.gguf',
          detail: 'The saved model cannot be used: local model path must name a regular file',
        };
      case 'choose_local_model':
        return {
          state: 'ready',
          display_name: 'private-model.gguf',
          detail: 'The model selection is saved locally and restored at startup.',
        };
      default:
        throw new Error(`unexpected command: ${command}`);
    }
  };

  globalThis.window = {
    __TAURI__: { core: { invoke }, opener: { openUrl: async () => {} } },
    confirm: () => true,
  };
  globalThis.document = {
    body,
    createElement: () => new FakeElement(),
    getElementById: (id) => elements.get(id),
    querySelector: () => null,
    querySelectorAll: (selector) => {
      if (selector === '.nav-item' || selector === '.view-section') return [];
      return [];
    },
  };

  await import(`../src/main.js?desktop-model-test=${Date.now()}`);
  await new Promise((resolve) => setImmediate(resolve));
  elements.get('chat-model').value = 'auto';
  elements.get('chat-model').listeners.get('change')();
  assert.equal(
    elements.get('playground-model-note').textContent,
    'Automatic routing prefers a ready local model; configured hosted providers are fallback routes.',
  );
  assert.ok(!elements.get('playground-model-note').textContent.includes('provider routes'));
  assert.equal(elements.get('local-model-status-pill').textContent, 'Needs attention');
  assert.equal(elements.get('local-model-name').textContent, 'missing.gguf');
  assert.ok(!elements.get('local-model-detail').textContent.includes('/'));

  elements.get('local-model-sha256').value = 'A'.repeat(64);
  await elements.get('choose-local-model').listeners.get('click')();
  assert.equal(elements.get('local-model-status-pill').textContent, 'Configured');
  assert.equal(elements.get('local-model-name').textContent, 'private-model.gguf');
  assert.ok(!elements.get('local-model-detail').textContent.includes('/'));
  const pickerCall = calls.find(([command]) => command === 'choose_local_model');
  assert.deepEqual(pickerCall, [
    'choose_local_model',
    { expectedSha256: 'a'.repeat(64) },
  ]);
  await elements.get('refresh-logs').listeners.get('click')();
  const [cancelled, failed] = elements.get('logs-body').children.map((row) => row.lastElementChild);
  assert.equal(cancelled.textContent, 'Cancelled');
  assert.equal(cancelled.className, 'log-status');
  assert.equal(failed.textContent, '503');
  assert.equal(failed.className, 'log-status error-copy');
});

test('Playground Stop retains intent before start reply and enables another request after cancellation', async () => {
  const elements = new Map([...elementIds, 'chat-history', 'chat-placeholder'].map((id) => [id, new FakeElement(id)]));
  elements.get('playground-mode').value = 'chat';
  elements.get('setting-port').value = '1337';
  const calls = [];
  let resolveStart;
  let rejectResult;
  let requests = 0;
  const firstStart = new Promise((resolve) => { resolveStart = resolve; });
  const firstResult = new Promise((_, reject) => { rejectResult = reject; });
  const invoke = async (command, args) => {
    calls.push([command, args]);
    switch (command) {
      case 'get_dashboard_stats': return { headroom: null, avg_latency: 0, total_tokens: 0, unknown_usage_requests: 2, request_count: 2 };
      case 'get_providers': return [];
      case 'get_models': return [{ id: 'auto', display_name: 'Auto', providers: [], supports_chat_completions: true, prompt_semantics: [] }];
      case 'get_master_profile': return {};
      case 'get_local_model_status': return { state: 'not_configured', detail: 'Not configured' };
      case 'plugin:free-token-energy|loopback_status': return { enabled: false, addresses: [] };
      case 'playground_start': requests += 1; return requests === 1 ? firstStart : 'request-two';
      case 'playground_cancel': return true;
      case 'playground_wait': return args.requestId === 'request-one' ? firstResult : { choices: [{ message: { role: 'assistant', content: 'finished' } }] };
      default: throw new Error(`Unexpected command: ${command}`);
    }
  };
  globalThis.window = { __TAURI__: { core: { invoke } } };
  globalThis.document = {
    body: new FakeElement('body'), createElement: () => new FakeElement(),
    getElementById: (id) => elements.get(id), querySelector: () => null, querySelectorAll: () => [],
  };
  await import(`../src/main.js?stop-test=${Date.now()}`);
  const flush = () => new Promise((resolve) => setImmediate(resolve));
  await flush();
  assert.equal(elements.get('stat-tokens-context').textContent, 'Partial · 2 requests without usage');
  elements.get('chat-model').value = 'auto';
  elements.get('chat-input').value = 'hello';
  const submit = () => elements.get('chat-form').listeners.get('submit')({ preventDefault() {} });
  submit();
  assert.equal(elements.get('chat-send').textContent, 'Stop');
  assert.equal(elements.get('chat-send').disabled, false);
  submit();
  assert.equal(elements.get('chat-send').textContent, 'Stopping…');
  assert.equal(calls.filter(([command]) => command === 'playground_cancel').length, 0);
  resolveStart('request-one');
  await flush();
  assert.deepEqual(calls.filter(([command]) => command === 'playground_cancel'), [['playground_cancel', { requestId: 'request-one' }]]);
  rejectResult(new Error('request_cancelled: cancellation acknowledged'));
  await flush();
  assert.equal(elements.get('chat-send').textContent, 'Send request');
  assert.equal(elements.get('chat-input').disabled, false);
  assert.equal(elements.get('playground-mode').disabled, false);
  assert.equal(elements.get('chat-history').children.filter((item) => item.className === 'chat-message chat-error').length, 1);
  elements.get('chat-input').value = 'next';
  submit();
  await flush();
  assert.equal(requests, 2);
  assert.equal(elements.get('chat-send').textContent, 'Send request');
  assert.equal(elements.get('chat-history').children.at(-1).children[1].textContent, 'finished');
});

test('Playground displays a real refusal message and retains complete history for the next request', async () => {
  const fixture = JSON.parse(await readFile(new URL('./fixtures/chat-refusal.json', import.meta.url), 'utf8'));
  const refusalMessage = fixture.choices[0].message;
  const toolMessage = { role: 'assistant', content: null, tool_calls: [{
    id: 'call_1', type: 'function', function: { name: 'lookup', arguments: '{"term":"日本語"}' },
  }] };
  const responses = [
    { choices: [{ message: { role: 'assistant', content: 'Ordinary 日本語.' } }] },
    { ...fixture, choices: [...fixture.choices, { index: 1, message: { role: 'assistant', content: 'Other choice' } }] },
    { choices: [{ message: toolMessage }] },
    { choices: [{ message: { role: 'assistant', content: null } }] },
    { choices: [{ index: 0, text: ' continuation\n' }, { index: 1, text: 'other' }] },
  ];
  const elements = new Map([...elementIds, 'chat-placeholder'].map((id) => [id, new FakeElement(id)]));
  elements.get('playground-mode').value = 'chat';
  elements.get('setting-port').value = '1337';
  const starts = [];
  const invoke = async (command, args) => {
    switch (command) {
      case 'get_dashboard_stats': return { headroom: null, avg_latency: 0, total_tokens: 0, request_count: 0 };
      case 'get_providers': return [];
      case 'get_models': return [{ id: 'fixture-model', display_name: 'Fixture', providers: [], supports_chat_completions: true, supports_text_completions: true, prompt_semantics: [] }];
      case 'get_local_model_status': return { state: 'not_configured' };
      case 'plugin:free-token-energy|loopback_status': return { enabled: false, addresses: [] };
      case 'playground_start': starts.push(structuredClone(args)); return String(starts.length - 1);
      case 'playground_wait': return responses[Number(args.requestId)];
      default: throw new Error(`unexpected command: ${command}`);
    }
  };
  globalThis.window = { __TAURI__: { core: { invoke } } };
  globalThis.document = {
    body: new FakeElement('body'), createElement: () => new FakeElement(),
    getElementById: (id) => elements.get(id), querySelector: () => null, querySelectorAll: () => [],
  };
  await import(`../src/main.js?refusal-history-test=${Date.now()}`);
  const flush = () => new Promise((resolve) => setImmediate(resolve));
  await flush();
  elements.get('chat-model').value = 'fixture-model';
  const submit = async (text) => {
    elements.get('chat-input').value = text;
    elements.get('chat-form').listeners.get('submit')({ preventDefault() {} });
    await flush();
  };
  await submit('ordinary');
  assert.equal(elements.get('chat-history').children.at(-1).children[1].textContent, 'Ordinary 日本語.');
  await submit('refusal');
  assert.equal(elements.get('chat-history').children.at(-1).children[1].textContent, refusalMessage.refusal,
    JSON.stringify({ expectedMessage: refusalMessage, displayed: elements.get('chat-history').children.at(-1).children[1].textContent, requests: starts }));
  await submit('follow up');
  assert.deepEqual(starts[2].req.messages[3], refusalMessage);
  assert.equal(starts[2].req.messages[3].content, null);
  assert.equal(starts[2].req.messages[3].role, 'assistant');
  await submit('after tool');
  assert.deepEqual(starts[3].req.messages[5], toolMessage);
  assert.equal(elements.get('chat-history').children.at(-1).children[1].textContent, '(empty response)');
  assert.equal(elements.get('chat-send').disabled, false);
  elements.get('playground-mode').value = 'completion';
  elements.get('playground-mode').listeners.get('change')();
  elements.get('chat-model').value = 'fixture-model';
  await submit('\n  raw tail\t');
  assert.equal(starts[4].mode, 'completion');
  assert.equal(starts[4].req.prompt, '\n  raw tail\t');
  assert.ok(!Object.hasOwn(starts[4].req, 'messages'));
  assert.equal(elements.get('chat-history').children.at(-1).children[1].textContent, '[0]  continuation\n\n\n[1] other');
});

class ProviderElement extends FakeElement {
  append(...children) {
    for (const child of children) {
      child.parentElement?.children.splice(child.parentElement.children.indexOf(child), 1);
      child.parentElement = this;
    }
    super.append(...children);
  }

  remove() {
    if (this.parentElement) {
      this.parentElement.children.splice(this.parentElement.children.indexOf(this), 1);
      this.parentElement = null;
    }
  }
}

const providerDescendants = (node) => [node, ...node.children.flatMap(providerDescendants)];
const deferredProviderReply = () => {
  let resolve;
  const promise = new Promise((accept) => { resolve = accept; });
  return { promise, resolve };
};

async function mountProviderDraftFixture({ credentialRequiredB = true } = {}) {
  const elements = new Map(elementIds.map((id) => [id, new ProviderElement(id)]));
  elements.get('playground-mode').value = 'chat';
  elements.get('setting-port').value = '1337';
  const nav = ['setup', 'dashboard'].map((view) => {
    const item = new ProviderElement(); item.dataset.view = view; item.dataset.label = view; return item;
  });
  const calls = [];
  const copied = [];
  const providers = ['groq', 'openrouter'].map((id, index) => ({
    id, name: `Provider ${index === 0 ? 'A' : 'B'}`, backend_kind: 'hosted',
    credential_required: index === 0 || credentialRequiredB, configured: true, status: 'ready',
    model_count: 1, text_completion_model_count: 0,
  }));
  const profile = { email: 'first@example.test', name: 'Original Name', password_hint: 'non-secret fixture' };
  const fixture = {
    elements, nav, calls, copied, providers, profile,
    readProviders: async () => structuredClone(providers),
    readProfile: async () => structuredClone(profile),
    saveKey: async () => {},
    deleteKey: async () => true,
    saveProfile: async () => {},
  };
  const invoke = async (command, args) => {
    calls.push([command, structuredClone(args)]);
    switch (command) {
      case 'get_dashboard_stats': return { headroom: null, avg_latency: 0, total_tokens: 0, request_count: 0 };
      case 'get_providers': return fixture.readProviders();
      case 'get_master_profile': return fixture.readProfile();
      case 'get_models': return [{ id: 'auto', display_name: 'Auto', providers: [], supports_chat_completions: true, prompt_semantics: [] }];
      case 'get_local_model_status': return { state: 'not_configured' };
      case 'plugin:free-token-energy|loopback_status': return { enabled: false, addresses: [] };
      case 'save_key':
        await fixture.saveKey(args);
        providers.find((provider) => provider.id === args.providerId).configured = true;
        return;
      case 'delete_key': {
        const removed = await fixture.deleteKey(args);
        providers.find((provider) => provider.id === args.providerId).configured = false;
        return removed;
      }
      case 'save_profile_field': await fixture.saveProfile(args); profile[args.key] = args.value; return;
      case 'choose_local_model': return { state: 'ready', display_name: 'fixture.gguf', detail: 'Fixture metadata only' };
      default: throw new Error(`unexpected command: ${command}`);
    }
  };
  globalThis.window = { __TAURI__: { core: { invoke } }, confirm: () => true };
  for (const property of ['localStorage', 'sessionStorage', 'indexedDB']) {
    Object.defineProperty(window, property, { get() { throw new Error(`browser persistence forbidden: ${property}`); } });
  }
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true, value: { clipboard: { writeText: async (text) => { copied.push(text); } } },
  });
  globalThis.document = {
    body: new ProviderElement('body'), createElement: () => new ProviderElement(),
    getElementById: (id) => elements.get(id),
    querySelector: (selector) => nav.find((item) => selector.includes(`"${item.dataset.view}"`)) || null,
    querySelectorAll: (selector) => selector === '.nav-item' ? nav : [],
  };
  await import(`../src/main.js?provider-draft-test=${Date.now()}-${Math.random()}`);
  fixture.flush = () => new Promise((resolve) => setImmediate(resolve));
  fixture.setup = async () => { nav[0].listeners.get('click')(); await fixture.flush(); };
  fixture.card = (name) => elements.get('onboarding-grid').children.find((card) => card.children[0]?.children[0]?.textContent === name);
  fixture.input = (name) => providerDescendants(fixture.card(name)).find((element) => element.type === 'password');
  fixture.button = (name, className) => providerDescendants(fixture.card(name)).find((element) => element.className === className);
  fixture.click = (name, className) => fixture.button(name, className).listeners.get('click')();
  await fixture.flush();
  await fixture.setup();
  return fixture;
}

test('Provider save and removal retain another mounted card draft and denied-write state', async () => {
  const fixture = await mountProviderDraftFixture();
  fixture.input('Provider B').value = '  fixture ordinary key  ';
  await fixture.click('Provider B', 'btn-primary');
  assert.equal(fixture.input('Provider B').value, '');
  assert.deepEqual(fixture.calls.find(([command]) => command === 'save_key'), [
    'save_key', { providerId: 'openrouter', keyValue: 'fixture ordinary key' },
  ]);
  const inputA = fixture.input('Provider A');
  inputA.value = 'fixture unsaved A 日本語';
  fixture.input('Provider B').value = 'fixture replacement B';
  await fixture.click('Provider B', 'btn-primary');
  assert.equal(fixture.input('Provider A').value, 'fixture unsaved A 日本語',
    JSON.stringify({ oldDraft: inputA.value, mountedDraft: fixture.input('Provider A').value, calls: fixture.calls }));
  assert.equal(fixture.input('Provider A'), inputA);
  assert.equal(fixture.input('Provider B').value, '');
  fixture.input('Provider B').value = 'fixture unsaved B replacement';
  await fixture.click('Provider B', 'btn-danger');
  assert.equal(fixture.input('Provider A'), inputA);
  assert.equal(inputA.value, 'fixture unsaved A 日本語');
  assert.equal(fixture.input('Provider B').value, 'fixture unsaved B replacement');
  assert.equal(fixture.button('Provider B', 'btn-danger').hidden, true);
  fixture.saveKey = async () => { throw new Error('fixture OS credential store denied'); };
  await fixture.click('Provider A', 'btn-primary');
  assert.equal(fixture.input('Provider A'), inputA);
  assert.equal(inputA.value, 'fixture unsaved A 日本語');
  assert.equal(fixture.button('Provider A', 'btn-primary').disabled, false);
  assert.match(fixture.elements.get('toast').textContent, /credential store denied/);
  fixture.deleteKey = async () => { throw new Error('fixture credential removal denied'); };
  await fixture.click('Provider A', 'btn-danger');
  assert.equal(fixture.input('Provider A'), inputA);
  assert.equal(inputA.value, 'fixture unsaved A 日本語');
  assert.equal(fixture.button('Provider A', 'btn-danger').disabled, false);
  assert.match(fixture.elements.get('toast').textContent, /credential removal denied/);
  await fixture.setup();
  assert.equal(fixture.input('Provider A'), inputA);
  await fixture.elements.get('choose-local-model').listeners.get('click')();
  assert.equal(fixture.input('Provider A'), inputA);
  assert.equal(inputA.value, 'fixture unsaved A 日本語');
});

test('Provider overlapping saves preserve edits made after submission and latest metadata', async () => {
  const fixture = await mountProviderDraftFixture();
  fixture.input('Provider B').value = 'fixture ordinary key';
  await fixture.click('Provider B', 'btn-primary');
  assert.equal(fixture.input('Provider B').value, '');
  const heldA = deferredProviderReply();
  const heldB = deferredProviderReply();
  fixture.saveKey = async ({ providerId }) => providerId === 'groq' ? heldA.promise : heldB.promise;
  const inputA = fixture.input('Provider A');
  const inputB = fixture.input('Provider B');
  inputA.value = 'fixture submitted A'; inputB.value = 'fixture submitted B';
  const savingA = fixture.click('Provider A', 'btn-primary');
  const savingB = fixture.click('Provider B', 'btn-primary');
  assert.equal(fixture.button('Provider A', 'btn-primary').disabled, true);
  inputA.value = 'fixture newer A'; inputB.value = 'fixture newer B';
  heldB.resolve(); await savingB;
  const afterB = {
    draftA: fixture.input('Provider A').value, draftB: fixture.input('Provider B').value,
    saveAPending: fixture.button('Provider A', 'btn-primary').disabled,
  };
  heldA.resolve(); await savingA;
  assert.equal(afterB.draftA, 'fixture newer A', JSON.stringify({ afterB, calls: fixture.calls }));
  assert.equal(afterB.draftB, 'fixture newer B');
  assert.equal(afterB.saveAPending, true);
  assert.equal(fixture.input('Provider A'), inputA);
  assert.equal(fixture.input('Provider B'), inputB);
  assert.equal(inputA.value, 'fixture newer A'); assert.equal(inputB.value, 'fixture newer B');
  const older = deferredProviderReply();
  fixture.readProviders = () => older.promise;
  fixture.nav[0].listeners.get('click')();
  const oldSnapshot = structuredClone(fixture.providers);
  oldSnapshot[0].status = 'needs_key'; oldSnapshot[0].configured = false;
  fixture.readProviders = async () => structuredClone(fixture.providers);
  await fixture.setup();
  older.resolve(oldSnapshot); await fixture.flush();
  assert.equal(fixture.card('Provider A').children[0].children[1].textContent, 'Ready');
  assert.equal(fixture.input('Provider A'), inputA);
  assert.equal(inputA.value, 'fixture newer A');
  fixture.readProviders = async () => { throw new Error('fixture metadata unavailable'); };
  await fixture.setup();
  assert.equal(fixture.input('Provider A'), inputA);
  assert.equal(inputA.value, 'fixture newer A');
  assert.match(fixture.elements.get('toast').textContent, /metadata unavailable/);
});

test('Delayed profile reads preserve local edits and mounted Copy uses the current fields', async () => {
  const fixture = await mountProviderDraftFixture();
  const copyEmail = () => providerDescendants(fixture.card('Provider A'))
    .filter((element) => element.className === 'btn-helper')[0].listeners.get('click')();
  await copyEmail();
  assert.equal(fixture.copied.at(-1), 'first@example.test');
  const delayed = deferredProviderReply();
  fixture.readProfile = () => delayed.promise;
  fixture.nav[0].listeners.get('click')();
  const email = fixture.elements.get('profile-email');
  email.value = 'new@example.test'; email.listeners.get('input')?.();
  await email.listeners.get('change')();
  assert.equal(fixture.profile.email, 'new@example.test');
  delayed.resolve({ email: 'first@example.test', name: 'Old Name', password_hint: 'old hint' });
  await fixture.flush();
  assert.equal(email.value, 'new@example.test');
  await copyEmail();
  assert.equal(fixture.copied.at(-1), 'new@example.test');
  fixture.readProfile = async () => { throw new Error('fixture optional profile unavailable'); };
  await fixture.setup();
  assert.equal(fixture.elements.get('onboarding-grid').children.length, 2);
  assert.ok(fixture.input('Provider A'));
  assert.equal(email.value, 'new@example.test');
  assert.match(fixture.elements.get('toast').textContent, /optional profile unavailable/);
  const name = fixture.elements.get('profile-name');
  name.value = 'New Name'; name.listeners.get('input')?.();
  await name.listeners.get('change')();
  const copyName = providerDescendants(fixture.card('Provider A'))
    .filter((element) => element.className === 'btn-helper')[1];
  await copyName.listeners.get('click')();
  assert.equal(fixture.copied.at(-1), 'New Name');
  fixture.saveProfile = async () => { throw new Error('fixture profile write denied'); };
  email.value = 'unsaved@example.test'; email.listeners.get('input')?.();
  await email.listeners.get('change')();
  fixture.readProfile = async () => structuredClone(fixture.profile);
  await fixture.setup();
  assert.equal(email.value, 'unsaved@example.test');
  assert.equal(fixture.profile.email, 'new@example.test');
  await copyEmail();
  assert.equal(fixture.copied.at(-1), 'unsaved@example.test');
  assert.equal(fixture.profile.password_hint, 'non-secret fixture');
});


test('Provider metadata updates honor credential requirements without discarding a mounted draft', async () => {
  const fixture = await mountProviderDraftFixture({ credentialRequiredB: false });
  assert.equal(fixture.input('Provider B'), undefined);
  fixture.providers.push({
    id: 'llama-native', name: 'Local llama.cpp', backend_kind: 'local_embedded',
    credential_required: false, configured: false, status: 'needs_model',
    model_count: 0, text_completion_model_count: 0,
  });
  await fixture.setup();
  assert.equal(fixture.input('Local llama.cpp'), undefined);
  assert.equal(fixture.card('Local llama.cpp').children[0].children[1].textContent, 'Needs model');
  const inputA = fixture.input('Provider A');
  inputA.value = 'fixture unsubmitted A';
  fixture.providers[1].credential_required = true;
  fixture.providers[0].credential_required = false;
  fixture.providers[0].configured = false;
  fixture.providers[0].status = 'unavailable';
  fixture.providers[0].model_count = 2;
  fixture.providers[0].text_completion_model_count = 1;
  await fixture.setup();
  assert.equal(fixture.input('Provider A')?.value, 'fixture unsubmitted A');
  assert.equal(fixture.input('Provider A'), inputA);
  assert.equal(inputA.parentElement.hidden, true);
  assert.equal(fixture.button('Provider A', 'btn-primary').parentElement.hidden, true);
  assert.ok(fixture.input('Provider B'));
  assert.equal(fixture.card('Provider A').children[0].children[1].textContent, 'Unavailable');
  const metadata = providerDescendants(fixture.card('Provider A')).filter((element) => element.className === 'provider-meta-item');
  assert.equal(metadata[0].textContent, '2 models');
  assert.equal(metadata[1].hidden, false); assert.equal(metadata[1].textContent, '1 raw completion');
  fixture.providers[0].credential_required = true;
  fixture.providers[0].configured = true;
  fixture.providers[0].status = 'ready';
  await fixture.setup();
  assert.equal(fixture.input('Provider A'), inputA);
  assert.equal(inputA.value, 'fixture unsubmitted A');
  assert.equal(inputA.parentElement.hidden, false);
  assert.equal(fixture.button('Provider A', 'btn-danger').hidden, false);
  assert.equal(fixture.card('Provider A').children[0].children[1].textContent, 'Ready');
  assert.equal(fixture.calls.filter(([command]) => command === 'save_key' || command === 'delete_key').length, 0);
});
