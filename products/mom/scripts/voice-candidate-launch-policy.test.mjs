import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { resolveCandidateDataDirectory, runtimeReadyLogConfirmsPath } from './voice-candidate-launch-policy.mjs';

const home = '/Users/tester';

test('candidate namespace fallback isolates the release default directory', () => {
  assert.deepEqual(resolveCandidateDataDirectory({ home, namespace: 'candidate-02' }), {
    path: '/Users/tester/.local/share/llama-native-kit/mom-llama/candidate-02',
    source: 'compiled-candidate-namespace',
  });
});

test('explicit isolated data directory is preserved after canonicalization', () => {
  const root = mkdtempSync(join(tmpdir(), 'voice-candidate-data-'));
  try {
    const explicitPath = join(root, 'prepared', 'call-02');
    mkdirSync(explicitPath, { recursive: true });
    assert.deepEqual(resolveCandidateDataDirectory({
      home, namespace: 'candidate-02', explicitPath,
    }), {
      path: realpathSync(explicitPath),
      source: 'explicit-isolated-directory',
    });
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('candidate data directory rejects both protected default stores', () => {
  for (const path of [
    '/Users/tester/.local/share/llama-native-kit/mom-llama',
    '/Users/tester/.local/share/llama-native-kit/mom-llama-development-mom1-v1',
  ]) {
    assert.throws(
      () => resolveCandidateDataDirectory({ home, namespace: 'candidate-02', explicitPath: path }),
      /installed\/default Mom data directory/,
    );
  }
});

test('candidate namespace and explicit paths reject traversal and relative paths', () => {
  assert.throws(() => resolveCandidateDataDirectory({ home, namespace: '../default' }), /unsafe/);
  assert.throws(() => resolveCandidateDataDirectory({ home, namespace: 'candidate-02', explicitPath: 'relative/store' }), /absolute/);
  assert.throws(() => resolveCandidateDataDirectory({ home, namespace: 'candidate-02', explicitPath: '/tmp/missing-voice-data-dir' }), /already exist/);
});

test('symlink aliases of the protected default are rejected', () => {
  const root = mkdtempSync(join(tmpdir(), 'voice-candidate-data-'));
  try {
    const testHome = join(root, 'home');
    const protectedPath = join(testHome, '.local', 'share', 'llama-native-kit', 'mom-llama');
    const alias = join(root, 'alias');
    mkdirSync(protectedPath, { recursive: true });
    symlinkSync(protectedPath, alias, 'dir');
    assert.throws(
      () => resolveCandidateDataDirectory({ home: testHome, namespace: 'candidate-02', explicitPath: alias }),
      /installed\/default Mom data directory/,
    );
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('startup identity matches the exact data path, not a path prefix', () => {
  const expected = '/tmp/voice-data/call-02';
  assert.equal(runtimeReadyLogConfirmsPath(`mom-llama runtime ready: ${expected}\n`, expected), true);
  assert.equal(runtimeReadyLogConfirmsPath(`mom-llama runtime ready: ${expected}-other\n`, expected), false);
  assert.equal(runtimeReadyLogConfirmsPath(`prefix mom-llama runtime ready: ${expected}\n`, expected), false);
});
