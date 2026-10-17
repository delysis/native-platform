import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const repository = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const wrapper = join(repository, 'products/mom/scripts/run-voice-check.mjs');

test('voice-check wrapper refuses an existing log before starting its child', () => {
  const directory = mkdtempSync(join(tmpdir(), 'voice-check-wrapper-'));
  try {
    const log = join(directory, 'existing.log');
    const marker = join(directory, 'child-ran');
    writeFileSync(log, 'preserve this prior receipt\n');
    const child = `require('node:fs').writeFileSync(${JSON.stringify(marker)}, 'ran')`;
    const result = spawnSync(
      process.execPath,
      [wrapper, log, '--', process.execPath, '-e', child],
      { encoding: 'utf8' },
    );

    assert.equal(result.status, 2);
    assert.match(result.stderr, /refusing existing voice-check log/);
    assert.equal(readFileSync(log, 'utf8'), 'preserve this prior receipt\n');
    assert.throws(() => readFileSync(marker));
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('voice-check wrapper joins stdout/stderr and records only terminal metadata', () => {
  const directory = mkdtempSync(join(tmpdir(), 'voice-check-wrapper-'));
  try {
    const log = join(directory, 'run.log');
    const child = [
      "process.stdout.write('stdout-marker\\n')",
      "process.stderr.write('stderr-marker\\n')",
      'process.exitCode = 7',
    ].join(';');
    const secret = 'voice-check-private-environment-sentinel';
    const result = spawnSync(
      process.execPath,
      [wrapper, log, '--', process.execPath, '-e', child],
      {
        encoding: 'utf8',
        env: { ...process.env, VOICE_CHECK_TEST_SECRET: secret },
      },
    );

    assert.equal(result.status, 7);
    const contents = readFileSync(log, 'utf8');
    assert.match(contents, /stdout-marker/);
    assert.match(contents, /stderr-marker/);
    assert.doesNotMatch(contents, new RegExp(secret));
    const records = contents
      .split('\n')
      .filter((line) => line.startsWith('{'))
      .map((line) => JSON.parse(line));
    assert.equal(records.length, 2);
    assert.deepEqual(records[0], {
      record: 'start',
      started_unix_ms: records[0].started_unix_ms,
    });
    assert.equal(records[1].record, 'exit');
    assert.equal(records[1].exit_code, 7);
    assert.equal(records[1].signal, null);
    assert.equal(typeof records[1].duration_ms, 'number');
    assert.equal(records[1].spawn_error_code, null);
    assert.doesNotMatch(contents, /VOICE_CHECK_TEST_SECRET/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
