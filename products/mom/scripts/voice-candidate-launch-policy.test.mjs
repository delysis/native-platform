import assert from 'node:assert/strict';
import { chmodSync, mkdtempSync, mkdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import {
  parseStartOptions,
  readPrivateStoreKeyFile,
  resolveCandidateDataDirectory,
  runtimeReadyLogConfirmsPath,
  startupCacheProgressCompleted,
  startupProgressDeadline,
} from './voice-candidate-launch-policy.mjs';

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

test('startup cache progress accepts only bounded numeric context completions', () => {
  const prefix = 'mom-llama startup voice-cache progress: ';
  const record = (completed, total = 14) =>
    `${prefix}contexts=${completed}/${total} checkpoint_reused=false elapsed_ms=1200 cache_positions=64 state_bytes=512`;
  assert.equal(startupCacheProgressCompleted(`${record(2)}\n${record(5)}\n`), 5);
  assert.equal(startupCacheProgressCompleted(`${record(5)}\n${record(3)}\n`), 5);
  assert.equal(startupCacheProgressCompleted(`${record(15)}\n`), 0);
  assert.equal(startupCacheProgressCompleted(`${record(1, 0)}\n`), 0);
  assert.equal(startupCacheProgressCompleted(`${prefix}contexts=1/14\n`), 0);
  assert.equal(startupCacheProgressCompleted(`prefix ${record(1)}\n`), 0);
});

test('startup progress extends only the idle deadline and never the hard cap', () => {
  assert.equal(startupProgressDeadline(10_000, 1_000_000, 180_000), 190_000);
  assert.equal(startupProgressDeadline(900_000, 1_000_000, 180_000), 1_000_000);
});

test('start options accept data and key files in either order', () => {
  assert.deepEqual(parseStartOptions(['--store-key-file', '/tmp/key', '--data-dir', '/tmp/data']), {
    dataDirectory: '/tmp/data', storeKeyFile: '/tmp/key',
  });
  assert.deepEqual(parseStartOptions(['--data-dir', '/tmp/data', '--store-key-file', '/tmp/key']), {
    dataDirectory: '/tmp/data', storeKeyFile: '/tmp/key',
  });
});

test('start options reject unknown, duplicate, and valueless flags', () => {
  assert.throws(() => parseStartOptions(['--unexpected', 'x']), /Unknown start option/);
  assert.throws(() => parseStartOptions(['--data-dir', '/tmp/a', '--data-dir', '/tmp/b']), /Duplicate/);
  assert.throws(() => parseStartOptions(['--store-key-file']), /Missing value/);
  assert.throws(() => parseStartOptions(['--store-key-file', '--data-dir', '/tmp/data']), /Missing value/);
});

test('private key loader accepts a mode-0600 synthetic key without exposing it in errors', {
  skip: process.platform === 'win32' && 'POSIX file modes are unavailable on Windows',
}, () => {
  const root = mkdtempSync(join(tmpdir(), 'voice-candidate-key-'));
  const key = 'a1'.repeat(32);
  const path = join(root, 'store-key.hex');
  try {
    writeFileSync(path, `${key}\n`, { mode: 0o600 });
    chmodSync(path, 0o600);
    assert.equal(readPrivateStoreKeyFile(path), key);

    writeFileSync(path, `${key}not-hex\n`, { mode: 0o600 });
    assert.throws(() => readPrivateStoreKeyFile(path), error => {
      assert.match(error.message, /exactly 64 hexadecimal characters/);
      assert.equal(error.message.includes(key), false);
      return true;
    });

    const invalidBytes = Buffer.alloc(64, 0xe1);
    writeFileSync(path, invalidBytes, { mode: 0o600 });
    assert.throws(() => readPrivateStoreKeyFile(path), error => {
      assert.match(error.message, /exactly 64 hexadecimal characters/);
      assert.equal(error.message.includes(invalidBytes.toString('hex')), false);
      return true;
    });

    writeFileSync(path, `${key}\n`, { mode: 0o600 });
    chmodSync(path, 0o644);
    assert.throws(() => readPrivateStoreKeyFile(path), error => {
      assert.match(error.message, /private to its owner/);
      assert.equal(error.message.includes(key), false);
      return true;
    });
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('private key loader rejects symlinks', {
  skip: process.platform === 'win32' && 'POSIX symlink/file mode semantics are unavailable on Windows',
}, () => {
  const root = mkdtempSync(join(tmpdir(), 'voice-candidate-key-link-'));
  try {
    const file = join(root, 'key');
    const link = join(root, 'key-link');
    writeFileSync(file, `${'b2'.repeat(32)}\n`, { mode: 0o600 });
    chmodSync(file, 0o600);
    symlinkSync(file, link);
    assert.throws(() => readPrivateStoreKeyFile(link), /regular, non-symlink/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
