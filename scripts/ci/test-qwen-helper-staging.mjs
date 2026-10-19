import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';

const helper = 'qwen3-tts-helper-aarch64-apple-darwin';
const metal = 'mlx.metallib-aarch64-apple-darwin';

for (const linkedOutput of [helper, metal, null]) {
  test(`Qwen staging ${linkedOutput ? `rejects a linked ${linkedOutput}` : 'copies ordinary build outputs'}`, {
    skip: process.platform !== 'darwin' || process.arch !== 'arm64',
  }, () => {
    const root = mkdtempSync(join(tmpdir(), 'qwen-staging-'));
    try {
      const script = join(root, 'scripts/build-qwen3-tts-helper.sh');
      const product = join(root, 'products/mom/apps/mom-llama');
      const staging = join(product, 'src-tauri/binaries');
      const release = join(product, 'qwen3-tts-helper/.build/release');
      const library = join(release, 'mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib');
      const commands = join(root, 'commands');
      for (const directory of [dirname(script), staging, dirname(library), commands]) {
        mkdirSync(directory, { recursive: true });
      }
      copyFileSync(new URL('../build-qwen3-tts-helper.sh', import.meta.url), script);
      writeFileSync(join(release, 'Qwen3TTSHelper'), 'new helper');
      writeFileSync(library, 'new library');
      const marker = join(root, 'build-started');
      writeFileSync(join(commands, 'swift'), '#!/bin/sh\ntouch "$STAGING_TEST_MARKER"\n', { mode: 0o755 });
      const preserved = join(root, 'prior-candidate-helper');
      writeFileSync(preserved, 'preserved candidate');
      if (linkedOutput) symlinkSync(preserved, join(staging, linkedOutput));
      const result = spawnSync('/bin/sh', [script], {
        encoding: 'utf8',
        env: { ...process.env, PATH: `${commands}:${process.env.PATH}`, STAGING_TEST_MARKER: marker },
      });
      assert.equal(readFileSync(preserved, 'utf8'), 'preserved candidate');
      if (linkedOutput) {
        assert.equal(result.status, 1, result.stderr);
        assert.match(result.stderr, /staging output must be an ordinary file/);
        assert.equal(existsSync(marker), false, 'refuse unsafe staging before compiling');
        assert.equal(lstatSync(join(staging, linkedOutput)).isSymbolicLink(), true);
      } else {
        assert.equal(result.status, 0, result.stderr);
        assert.equal(readFileSync(join(staging, helper), 'utf8'), 'new helper');
        assert.equal(readFileSync(join(staging, metal), 'utf8'), 'new library');
        assert.ok(lstatSync(join(staging, helper)).mode & 0o100);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}
