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
      case 'get_models':
        return [];
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
