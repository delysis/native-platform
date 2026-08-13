const tauri = window.__TAURI__;
const isDesktopRuntime = typeof tauri?.core?.invoke === 'function';
const RUNTIME_UNAVAILABLE_MESSAGE = 'Open this interface from the Free Token Energy desktop application.';
const invoke = isDesktopRuntime
  ? (...args) => tauri.core.invoke(...args)
  : async () => {
    throw new Error(RUNTIME_UNAVAILABLE_MESSAGE);
  };
const openUrl = tauri?.opener?.openUrl;

const PROVIDER_DETAILS = {
  openrouter: {
    description: 'Routes across a broad catalog through one provider account.',
    url: 'https://openrouter.ai/keys',
  },
  groq: {
    description: 'Low-latency inference for supported open-weight models.',
    url: 'https://console.groq.com/keys',
  },
  gemini: {
    description: 'Native Gemini generation, tools, vision, and streaming.',
    url: 'https://aistudio.google.com/app/apikey',
  },
  mistral: {
    description: 'Mistral chat models through the modern Gateway.',
    url: 'https://console.mistral.ai/api-keys/',
  },
  cerebras: {
    description: 'Fast chat and native raw text continuation through the Cerebras cloud API.',
    url: 'https://cloud.cerebras.ai/',
  },
  nvidia: {
    description: 'NVIDIA-hosted NIM model endpoints.',
    url: 'https://build.nvidia.com/',
  },
  anthropic: {
    description: 'Native Claude Messages requests with tools and streaming.',
    url: 'https://console.anthropic.com/',
  },
};

const PROFILE_FIELDS = {
  'profile-email': 'email',
  'profile-name': 'name',
  'profile-password-hint': 'password_hint',
};

const chatMessages = [];
let availableModels = [];
let toastTimer;

const PROMPT_SEMANTICS = {
  direct_continuation: 'direct continuation',
  fill_in_middle: 'FIM continuation',
  provider_native_unverified: 'provider-native; template behavior unverified',
  legacy_prompt_protocol: 'legacy prompt protocol',
};

function errorMessage(error) {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  try {
    return JSON.stringify(error);
  } catch {
    return 'An unexpected error occurred.';
  }
}

function showToast(message, kind = 'info') {
  const toast = document.getElementById('toast');
  toast.textContent = message;
  toast.dataset.kind = kind;
  toast.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    toast.hidden = true;
  }, 4000);
}

function setText(id, value) {
  document.getElementById(id).textContent = value;
}

function makeEmptyState(title, detail, className = '') {
  const state = document.createElement('div');
  state.className = `empty-state ${className}`.trim();
  const heading = document.createElement('strong');
  heading.textContent = title;
  const copy = document.createElement('span');
  copy.textContent = detail;
  state.append(heading, copy);
  return state;
}

function setButtonPending(button, pending, pendingLabel, defaultLabel) {
  if (!button) return;
  button.disabled = pending;
  button.setAttribute('aria-busy', String(pending));
  button.textContent = pending ? pendingLabel : defaultLabel;
}

function renderRuntimeState() {
  const banner = document.getElementById('runtime-banner');
  const status = document.getElementById('runtime-shell-status');
  banner.hidden = isDesktopRuntime;
  status.className = `runtime-status status-pill ${isDesktopRuntime ? 'status-ready' : 'status-warn'}`;
  status.textContent = isDesktopRuntime ? 'Desktop connected' : 'Preview only';
}

function renderPreviewState() {
  document.body.classList.add('preview-mode');
  setText('stat-headroom', '—');
  setText('stat-latency', '—');
  setText('stat-tokens', '—');
  setText('stat-requests', '—');

  document.getElementById('live-health-list').replaceChildren(
    makeEmptyState('Desktop connection required', 'Provider health is available when the desktop application is running.'),
  );
  document.getElementById('onboarding-grid').replaceChildren(
    makeEmptyState('Desktop connection required', 'Launch the application to add provider credentials and inspect available routes.'),
  );

  const modelSelect = document.getElementById('chat-model');
  modelSelect.replaceChildren();
  const option = document.createElement('option');
  option.textContent = 'Desktop application required';
  option.disabled = true;
  option.selected = true;
  modelSelect.append(option);
  setText('playground-model-note', 'Model routes are loaded from the local gateway catalog.');

  setText('proxy-status', 'Unavailable in interface preview.');
  setText('proxy-binding', '127.0.0.1:—');
  const proxyPill = document.getElementById('proxy-status-pill');
  proxyPill.className = 'status-pill status-muted';
  proxyPill.textContent = 'Offline';

  setText('local-model-name', 'Desktop application required');
  setText('local-model-detail', 'Local model selection is available when the desktop application is running.');
  const localModelPill = document.getElementById('local-model-status-pill');
  localModelPill.className = 'status-pill status-muted';
  localModelPill.textContent = 'Offline';

  const logsEmpty = document.getElementById('logs-empty');
  logsEmpty.textContent = 'Activity is available when the desktop application is running.';
  logsEmpty.hidden = false;

  for (const control of document.querySelectorAll('main button, main input, main select, main textarea')) {
    control.disabled = true;
  }
}

function formatNumber(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number.toLocaleString() : '0';
}

function statusLabel(status) {
  const labels = {
    ready: 'Ready',
    needs_key: 'Needs key',
    needs_model: 'Needs model',
    not_configured: 'Not configured',
    invalid: 'Needs attention',
    loading: 'Loading',
    unavailable: 'Unavailable',
    quota_exhausted: 'Quota exhausted',
    upstream_error: 'Last call failed',
  };
  return labels[status] || 'Unknown';
}

function statusClass(status) {
  const classes = {
    ready: 'status-ready',
    needs_key: 'status-muted',
    needs_model: 'status-muted',
    not_configured: 'status-muted',
    invalid: 'status-error',
    loading: 'status-warn',
    unavailable: 'status-error',
    quota_exhausted: 'status-warn',
    upstream_error: 'status-error',
  };
  return classes[status] || 'status-muted';
}

function makeStatusPill(status) {
  const pill = document.createElement('span');
  pill.className = `status-pill ${statusClass(status)}`;
  pill.textContent = statusLabel(status);
  return pill;
}

function renderLocalModelStatus(status) {
  const state = status?.state || 'invalid';
  const pill = document.getElementById('local-model-status-pill');
  pill.className = `status-pill ${statusClass(state)}`;
  pill.textContent = state === 'ready' ? 'Configured' : statusLabel(state);
  setText('local-model-name', status?.display_name || 'No model selected');
  setText(
    'local-model-detail',
    status?.detail || 'The desktop did not return a local model status.',
  );
}

async function refreshLocalModelStatus() {
  try {
    renderLocalModelStatus(await invoke('get_local_model_status'));
  } catch (error) {
    renderLocalModelStatus({
      state: 'invalid',
      detail: `Could not read local model status: ${errorMessage(error)}`,
    });
  }
}

async function chooseLocalModel() {
  const button = document.getElementById('choose-local-model');
  const digestInput = document.getElementById('local-model-sha256');
  const expectedSha256 = digestInput.value.trim().toLowerCase();
  if (expectedSha256 && !/^[0-9a-f]{64}$/.test(expectedSha256)) {
    showToast('Expected SHA-256 must be exactly 64 hexadecimal characters.', 'error');
    digestInput.focus();
    return;
  }

  setButtonPending(button, true, 'Choosing…', 'Choose GGUF file');
  try {
    const status = await invoke('choose_local_model', {
      expectedSha256: expectedSha256 || null,
    });
    renderLocalModelStatus(status);
    if (status.state === 'ready') {
      digestInput.value = '';
      showToast(`${status.display_name || 'Local model'} is configured.`);
      await Promise.all([renderProviderGrid(), refreshDashboard(), loadModels()]);
    }
  } catch (error) {
    showToast(`Could not configure local model: ${errorMessage(error)}`, 'error');
    await refreshLocalModelStatus();
  } finally {
    setButtonPending(button, false, 'Choosing…', 'Choose GGUF file');
  }
}

async function refreshDashboard() {
  const healthList = document.getElementById('live-health-list');
  const refreshButton = document.getElementById('refresh-dashboard');
  setButtonPending(refreshButton, true, 'Refreshing…', 'Refresh');
  try {
    const [stats, providers] = await Promise.all([
      invoke('get_dashboard_stats'),
      invoke('get_providers'),
    ]);

    setText(
      'stat-headroom',
      stats.headroom == null ? '—' : `${Number(stats.headroom).toFixed(1)}%`,
    );
    setText('stat-latency', stats.request_count > 0 ? `${formatNumber(stats.avg_latency)} ms` : '—');
    setText('stat-tokens', formatNumber(stats.total_tokens));
    setText('stat-requests', formatNumber(stats.request_count));

    healthList.replaceChildren();
    if (providers.length === 0) {
      healthList.append(
        makeEmptyState('No providers registered', 'The local gateway did not report any provider adapters.'),
      );
      return;
    }

    for (const provider of providers) {
      const row = document.createElement('div');
      row.className = 'provider-health-row';

      const summary = document.createElement('div');
      const name = document.createElement('strong');
      name.textContent = provider.name;
      const detail = document.createElement('span');
      detail.className = 'muted';
      const isLocal =
        provider.backend_kind === 'local_embedded' || provider.backend_kind === 'local_service';
      const headroom =
        provider.headroom == null
          ? (isLocal ? 'not quota-metered' : 'quota varies by account')
          : `${Math.max(0, Math.min(100, Number(provider.headroom) * 100)).toFixed(0)}% local headroom`;
      const autocomplete = provider.text_completion_model_count
        ? ` · ${provider.text_completion_model_count} raw completion${provider.text_completion_model_count === 1 ? '' : 's'}`
        : '';
      detail.textContent = `${provider.model_count} model${provider.model_count === 1 ? '' : 's'}${autocomplete} · ${headroom} · ${formatNumber(provider.request_count)} requests`;
      summary.append(name, detail);

      row.append(summary, makeStatusPill(provider.status));
      healthList.append(row);
    }
  } catch (error) {
    setText('stat-headroom', '—');
    setText('stat-latency', '—');
    setText('stat-tokens', '—');
    setText('stat-requests', '—');
    healthList.replaceChildren();
    healthList.append(
      makeEmptyState('Could not load provider status', errorMessage(error), 'error-copy'),
    );
  } finally {
    setButtonPending(refreshButton, false, 'Refreshing…', 'Refresh');
  }
}

function parseSqliteTimestamp(value) {
  if (typeof value !== 'string') return null;
  const normalized = value.includes('T') ? value : `${value.replace(' ', 'T')}Z`;
  const date = new Date(normalized);
  return Number.isNaN(date.getTime()) ? null : date;
}

async function refreshLogs() {
  const body = document.getElementById('logs-body');
  const empty = document.getElementById('logs-empty');
  const refreshButton = document.getElementById('refresh-logs');
  setButtonPending(refreshButton, true, 'Refreshing…', 'Refresh');
  body.replaceChildren();
  empty.classList.remove('error-copy');
  empty.textContent = 'No requests recorded yet.';
  empty.hidden = true;

  try {
    const logs = await invoke('get_recent_logs');
    if (logs.length === 0) {
      empty.hidden = false;
      return;
    }

    for (const log of logs) {
      const row = document.createElement('tr');
      const timestamp = parseSqliteTimestamp(log.timestamp);
      const cells = [
        timestamp ? timestamp.toLocaleString() : String(log.timestamp),
        String(log.provider),
        String(log.model),
        formatNumber(log.tokens),
        `${formatNumber(log.latency)} ms`,
        String(log.status),
      ];
      for (const value of cells) {
        const cell = document.createElement('td');
        cell.textContent = value;
        row.append(cell);
      }
      const status = Number(log.status);
      row.lastElementChild.className = status >= 500
        ? 'log-status error-copy'
        : status >= 400
          ? 'log-status warn-copy'
          : 'log-status success-copy';
      body.append(row);
    }
  } catch (error) {
    empty.textContent = `Could not load activity: ${errorMessage(error)}`;
    empty.classList.add('error-copy');
    empty.hidden = false;
  } finally {
    setButtonPending(refreshButton, false, 'Refreshing…', 'Refresh');
  }
}

async function loadProfile() {
  const profile = await invoke('get_master_profile');
  for (const [elementId, field] of Object.entries(PROFILE_FIELDS)) {
    document.getElementById(elementId).value = profile[field] || '';
  }
  return profile;
}

async function saveProfileInput(input) {
  const key = PROFILE_FIELDS[input.id];
  if (!key) return;
  try {
    await invoke('save_profile_field', { key, value: input.value });
    showToast('Profile saved locally.');
  } catch (error) {
    showToast(errorMessage(error), 'error');
    input.focus();
  }
}

async function copyText(text, label, button) {
  if (!text) {
    showToast(`${label} is empty.`, 'error');
    return;
  }
  try {
    await navigator.clipboard.writeText(text);
    const original = button.textContent;
    button.textContent = 'Copied';
    setTimeout(() => {
      button.textContent = original;
    }, 1500);
  } catch (error) {
    showToast(`Could not copy ${label.toLowerCase()}: ${errorMessage(error)}`, 'error');
  }
}

async function openProviderConsole(providerId) {
  const url = PROVIDER_DETAILS[providerId]?.url;
  if (!url || !openUrl) {
    showToast('This provider console cannot be opened from the app.', 'error');
    return;
  }
  try {
    await openUrl(url);
  } catch (error) {
    showToast(`Could not open provider console: ${errorMessage(error)}`, 'error');
  }
}

function providerCard(provider, profile) {
  const requiresCredential = provider.credential_required !== false;
  const isLocal = provider.backend_kind === 'local_embedded' || provider.backend_kind === 'local_service';
  const details = PROVIDER_DETAILS[provider.id] || {
    description: isLocal
      ? 'Runs through a local inference runtime without a provider API key.'
      : 'Supported by the local gateway.',
  };
  const card = document.createElement('article');
  card.className = 'card provider-card';

  const heading = document.createElement('div');
  heading.className = 'provider-card-heading';
  const name = document.createElement('h2');
  name.textContent = provider.name;
  const badge = document.createElement('span');
  badge.className = `badge ${provider.status === 'ready' ? 'badge-free' : 'badge-muted'}`;
  badge.textContent = statusLabel(provider.status);
  heading.append(name, badge);

  const description = document.createElement('p');
  description.className = 'description';
  description.textContent = details.description;

  const providerMeta = document.createElement('div');
  providerMeta.className = 'provider-meta';
  const modelCount = document.createElement('span');
  modelCount.className = 'provider-meta-item';
  modelCount.textContent = `${formatNumber(provider.model_count)} model${provider.model_count === 1 ? '' : 's'}`;
  providerMeta.append(modelCount);
  if (provider.text_completion_model_count) {
    const completionCount = document.createElement('span');
    completionCount.className = 'provider-meta-item';
    completionCount.textContent = `${formatNumber(provider.text_completion_model_count)} raw completion${provider.text_completion_model_count === 1 ? '' : 's'}`;
    providerMeta.append(completionCount);
  }

  card.append(heading, description, providerMeta);
  if (!requiresCredential) return card;

  const helperRow = document.createElement('div');
  helperRow.className = 'helper-row';
  for (const [label, value] of [['Email', profile.email], ['Name', profile.name]]) {
    if (!value) continue;
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'btn-helper';
    button.textContent = `Copy ${label.toLowerCase()}`;
    button.addEventListener('click', () => copyText(value || '', label, button));
    helperRow.append(button);
  }

  const consoleButton = details.url ? document.createElement('button') : null;
  if (consoleButton) {
    consoleButton.type = 'button';
    consoleButton.className = 'provider-link';
    consoleButton.textContent = 'Open provider console';
    consoleButton.addEventListener('click', () => openProviderConsole(provider.id));
  }

  const keyLabel = document.createElement('label');
  keyLabel.className = 'key-label';
  const labelText = document.createElement('span');
  labelText.textContent = provider.configured ? 'Replace saved API key' : 'API key';
  const keyInput = document.createElement('input');
  keyInput.type = 'password';
  keyInput.className = 'input-field';
  keyInput.autocomplete = 'off';
  keyInput.placeholder = provider.configured ? 'Enter a replacement key' : 'Paste API key';
  keyInput.maxLength = 16384;
  keyLabel.append(labelText, keyInput);

  const actionRow = document.createElement('div');
  actionRow.className = 'provider-actions';
  const saveButton = document.createElement('button');
  saveButton.type = 'button';
  saveButton.className = 'btn-primary';
  saveButton.textContent = provider.configured ? 'Update key' : 'Save key';
  saveButton.addEventListener('click', async () => {
    const keyValue = keyInput.value.trim();
    if (!keyValue) {
      showToast('Enter an API key first.', 'error');
      keyInput.focus();
      return;
    }
    saveButton.disabled = true;
    try {
      await invoke('save_key', { providerId: provider.id, keyValue });
      keyInput.value = '';
      showToast(`${provider.name} key saved locally.`);
      await Promise.all([renderProviderGrid(), refreshDashboard()]);
    } catch (error) {
      showToast(`Could not save key: ${errorMessage(error)}`, 'error');
    } finally {
      saveButton.disabled = false;
    }
  });
  actionRow.append(saveButton);

  if (provider.configured) {
    const removeButton = document.createElement('button');
    removeButton.type = 'button';
    removeButton.className = 'btn-danger';
    removeButton.textContent = 'Remove';
    removeButton.addEventListener('click', async () => {
      if (!window.confirm(`Remove the saved ${provider.name} API key from this device?`)) {
        return;
      }
      removeButton.disabled = true;
      try {
        await invoke('delete_key', { providerId: provider.id });
        showToast(`${provider.name} key removed.`);
        await Promise.all([renderProviderGrid(), refreshDashboard()]);
      } catch (error) {
        showToast(`Could not remove key: ${errorMessage(error)}`, 'error');
      } finally {
        removeButton.disabled = false;
      }
    });
    actionRow.append(removeButton);
  }

  if (helperRow.childElementCount > 0) card.append(helperRow);
  if (consoleButton) card.append(consoleButton);
  card.append(keyLabel, actionRow);
  return card;
}

async function renderProviderGrid() {
  const grid = document.getElementById('onboarding-grid');
  grid.replaceChildren(
    makeEmptyState('Loading providers', 'Reading the local model catalog and saved configuration.'),
  );
  try {
    const [profile, providers] = await Promise.all([
      loadProfile(),
      invoke('get_providers'),
    ]);
    grid.replaceChildren();
    if (providers.length === 0) {
      grid.append(makeEmptyState('No providers registered', 'The local gateway did not report any provider adapters.'));
      return;
    }
    for (const provider of providers) {
      grid.append(providerCard(provider, profile));
    }
  } catch (error) {
    grid.replaceChildren(
      makeEmptyState('Could not load providers', errorMessage(error), 'error-copy'),
    );
  }
}

function contentToText(content) {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    return content
      .map((part) => typeof part === 'string' ? part : part?.text || JSON.stringify(part))
      .join('\n');
  }
  return content == null ? '(empty response)' : JSON.stringify(content, null, 2);
}

function appendMessage(role, text, labelOverride = null) {
  document.getElementById('chat-placeholder')?.remove();
  const history = document.getElementById('chat-history');
  const message = document.createElement('article');
  message.className = `chat-message chat-${role}`;
  const label = document.createElement('strong');
  label.textContent = labelOverride || (role === 'assistant' ? 'Assistant' : role === 'error' ? 'Error' : 'You');
  const content = document.createElement('p');
  content.textContent = text;
  message.append(label, content);
  history.append(message);
  history.scrollTop = history.scrollHeight;
}

async function sendMessage() {
  const input = document.getElementById('chat-input');
  const sendButton = document.getElementById('chat-send');
  const mode = document.getElementById('playground-mode').value;
  const rawInput = input.value;
  const message = mode === 'completion' ? rawInput : rawInput.trim();
  if (!message) return;

  if (mode === 'chat') {
    chatMessages.push({ role: 'user', content: message });
    appendMessage('user', message);
  } else {
    appendMessage('user', message, 'Raw prompt');
  }
  input.value = '';
  input.disabled = true;
  sendButton.disabled = true;
  sendButton.textContent = 'Routing…';

  try {
    if (mode === 'completion') {
      const response = await invoke('completion_request', {
        req: {
          model: document.getElementById('chat-model').value,
          prompt: message,
          max_tokens: 256,
          stream: false,
        },
      });
      const choices = response.choices || [];
      const content = choices.length > 1
        ? choices.map((choice) => `[${choice.index}] ${choice.text || ''}`).join('\n\n')
        : choices[0]?.text || '(empty completion)';
      appendMessage('assistant', content, 'Continuation');
    } else {
      const response = await invoke('chat_request', {
        req: {
          model: document.getElementById('chat-model').value,
          messages: chatMessages,
          stream: false,
        },
      });
      const responseMessage = response.choices?.[0]?.message;
      const content = contentToText(responseMessage?.content);
      chatMessages.push(responseMessage || { role: 'assistant', content });
      appendMessage('assistant', content);
    }
    await refreshDashboard();
  } catch (error) {
    appendMessage('error', errorMessage(error));
  } finally {
    input.disabled = false;
    sendButton.disabled = false;
    sendButton.textContent = 'Send request';
    input.focus();
  }
}

async function loadModels() {
  try {
    availableModels = await invoke('get_models');
    renderPlaygroundModels();
  } catch (error) {
    availableModels = [];
    renderPlaygroundModels();
    showToast(`Could not load models: ${errorMessage(error)}`, 'error');
  }
}

function renderPlaygroundModels() {
  const mode = document.getElementById('playground-mode').value;
  const select = document.getElementById('chat-model');
  const previous = select.value;
  const models = availableModels.filter((model) => (
    mode === 'completion' ? model.supports_text_completions : model.supports_chat_completions
  ));
  select.replaceChildren();
  select.disabled = models.length === 0;
  if (models.length === 0) {
    const option = document.createElement('option');
    option.textContent = 'No compatible model routes';
    option.disabled = true;
    option.selected = true;
    select.append(option);
    updatePlaygroundModelNote();
    return;
  }
  for (const model of models) {
    const option = document.createElement('option');
    option.value = model.id;
    const semantics = mode === 'completion'
      ? (model.prompt_semantics || []).map((item) => PROMPT_SEMANTICS[item] || item).join(', ')
      : '';
    option.textContent = semantics ? `${model.display_name} — ${semantics}` : model.display_name;
    select.append(option);
  }
  if ([...select.options].some((option) => option.value === previous)) {
    select.value = previous;
  }
  updatePlaygroundModelNote();
}

function updatePlaygroundModelNote() {
  const mode = document.getElementById('playground-mode').value;
  const selectedId = document.getElementById('chat-model').value;
  const model = availableModels.find((item) => item.id === selectedId);
  if (!model) {
    setText('playground-model-note', 'No compatible model route is available for this request type.');
    return;
  }

  if (model.id === 'auto') {
    setText(
      'playground-model-note',
      'Automatic routing prefers a ready local model; configured hosted providers are fallback routes.',
    );
    return;
  }

  const providerCount = Array.isArray(model.providers) ? model.providers.length : 0;
  const providerSummary = providerCount === 0
    ? 'Provider availability is unknown'
    : `${providerCount} provider route${providerCount === 1 ? '' : 's'}`;
  if (mode === 'completion') {
    const semantics = (model.prompt_semantics || [])
      .map((item) => PROMPT_SEMANTICS[item] || item)
      .join(', ');
    setText('playground-model-note', `${providerSummary} · ${semantics || 'native prompt semantics'}`);
  } else {
    setText('playground-model-note', `${providerSummary} · OpenAI-compatible chat request`);
  }
}

function resetPlaygroundForMode() {
  const mode = document.getElementById('playground-mode').value;
  chatMessages.length = 0;
  const history = document.getElementById('chat-history');
  const placeholder = makeEmptyState(
    'No requests yet',
    mode === 'completion'
      ? 'The exact prompt and native continuation will appear here.'
      : 'Responses from the selected route will appear here.',
    'chat-empty',
  );
  placeholder.id = 'chat-placeholder';
  history.replaceChildren(placeholder);
  const input = document.getElementById('chat-input');
  input.placeholder = mode === 'completion'
    ? 'Enter the exact text to continue. Whitespace is preserved…'
    : 'Enter a message…';
  document.getElementById('playground-subtitle').textContent = mode === 'completion'
    ? 'Send an unwrapped prompt to a native text-completion route.'
    : 'Test automatic chat routing or pin a public model.';
  renderPlaygroundModels();
}

function renderProxyStatus(status) {
  const pill = document.getElementById('proxy-status-pill');
  const portInput = document.getElementById('setting-port');
  const running = Boolean(status.enabled);
  const address = status.addresses?.find((value) => value.startsWith('127.0.0.1:'))
    ?? status.addresses?.[0]
    ?? null;
  const port = address ? Number(address.slice(address.lastIndexOf(':') + 1)) : null;
  setText('proxy-token-path', status.token_path ?? 'Created when the service starts');
  pill.className = `status-pill ${running ? 'status-ready' : 'status-error'}`;
  pill.textContent = running ? 'Running' : 'Stopped';
  if (port) {
    portInput.value = port;
    setText('proxy-status', `Listening on http://127.0.0.1:${port}`);
    setText('proxy-binding', `127.0.0.1:${port}`);
  } else {
    setText('proxy-status', 'The local API proxy is not running.');
    setText('proxy-binding', '127.0.0.1:—');
  }
}

async function refreshProxyStatus() {
  try {
    renderProxyStatus(await invoke('plugin:free-token-energy|loopback_status'));
  } catch (error) {
    const pill = document.getElementById('proxy-status-pill');
    pill.className = 'status-pill status-error';
    pill.textContent = 'Unavailable';
    setText('proxy-status', errorMessage(error));
    setText('proxy-binding', '127.0.0.1:—');
    showToast(`Could not read proxy status: ${errorMessage(error)}`, 'error');
  }
}

async function restartProxy() {
  const input = document.getElementById('setting-port');
  const button = document.getElementById('restart-proxy');
  const port = Number(input.value);
  if (!Number.isInteger(port) || port < 1024 || port > 65535) {
    showToast('Choose a port between 1024 and 65535.', 'error');
    input.focus();
    return;
  }
  button.disabled = true;
  button.textContent = 'Applying…';
  try {
    const status = await invoke('plugin:free-token-energy|loopback_start', { port });
    renderProxyStatus(status);
    showToast(`Proxy is listening on port ${port}.`);
  } catch (error) {
    showToast(`Could not restart proxy: ${errorMessage(error)}`, 'error');
    await refreshProxyStatus();
  } finally {
    button.disabled = false;
    button.textContent = 'Apply changes';
  }
}

function showView(viewName) {
  for (const section of document.querySelectorAll('.view-section')) {
    section.hidden = section.id !== `view-${viewName}`;
  }
  for (const item of document.querySelectorAll('.nav-item')) {
    item.classList.toggle('active', item.dataset.view === viewName);
  }
  const activeItem = document.querySelector(`.nav-item[data-view="${viewName}"]`);
  setText('workspace-context', activeItem?.dataset.label || 'Workspace');
  if (!isDesktopRuntime) return;
  if (viewName === 'dashboard') refreshDashboard();
  if (viewName === 'setup') Promise.all([renderProviderGrid(), refreshLocalModelStatus()]);
  if (viewName === 'logs') refreshLogs();
  if (viewName === 'settings') refreshProxyStatus();
}

function bindEvents() {
  for (const item of document.querySelectorAll('.nav-item')) {
    item.addEventListener('click', () => showView(item.dataset.view));
  }
  for (const inputId of Object.keys(PROFILE_FIELDS)) {
    const input = document.getElementById(inputId);
    input.addEventListener('change', () => saveProfileInput(input));
  }
  document.getElementById('refresh-dashboard').addEventListener('click', refreshDashboard);
  document.getElementById('refresh-logs').addEventListener('click', refreshLogs);
  document.getElementById('choose-local-model').addEventListener('click', chooseLocalModel);
  document.getElementById('chat-form').addEventListener('submit', (event) => {
    event.preventDefault();
    sendMessage();
  });
  document.getElementById('playground-mode').addEventListener('change', resetPlaygroundForMode);
  document.getElementById('chat-model').addEventListener('change', updatePlaygroundModelNote);
  document.getElementById('chat-input').addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      document.getElementById('chat-form').requestSubmit();
    }
  });
  document.getElementById('proxy-form').addEventListener('submit', (event) => {
    event.preventDefault();
    restartProxy();
  });
}

async function bootstrap() {
  bindEvents();
  renderRuntimeState();
  if (!isDesktopRuntime) {
    renderPreviewState();
    return;
  }
  await Promise.all([
    refreshDashboard(),
    loadModels(),
    refreshProxyStatus(),
    refreshLocalModelStatus(),
  ]);
}

bootstrap();
