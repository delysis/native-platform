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
  'logs-empty',
  'profile-email',
  'profile-name',
  'profile-password-hint',
  'refresh-dashboard',
  'refresh-logs',
  'chat-form',
  'playground-mode',
  'chat-input',
  'chat-send',
  'proxy-form',
  'workspace-context',
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
  assert.ok(runtimeControls.every((control) => control.disabled));
  assert.ok(navItems.every((item) => !item.disabled));
  assert.ok(body.classList.contains('preview-mode'));
});
