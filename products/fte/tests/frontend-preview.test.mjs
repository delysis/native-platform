import assert from 'node:assert/strict';
import test from 'node:test';

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
