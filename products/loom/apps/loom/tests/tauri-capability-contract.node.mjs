import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { auditTauriCapabilities } from '../../../../../scripts/check-tauri-capabilities.mjs';

const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

test('production Tauri calls are covered by the packaged ACL', () => {
  const result = auditTauriCapabilities({ appDir });
  assert.deepEqual(result.unknown, []);
  assert.deepEqual(result.missing, []);
  assert(result.required.some(item => item.command === 'core:window|set_focus'));
});

test('removing focus permission reproduces the Option-Right packaging defect', () => {
  const capabilityPath = path.join(appDir, 'src-tauri/capabilities/default.json');
  const capability = JSON.parse(fs.readFileSync(capabilityPath, 'utf8'));
  capability.permissions = capability.permissions.filter(permission => permission !== 'core:window:allow-set-focus');
  const result = auditTauriCapabilities({ appDir, capability });
  assert(result.missing.some(item => item.command === 'core:window|set_focus'));
});
