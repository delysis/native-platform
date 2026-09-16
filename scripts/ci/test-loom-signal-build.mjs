import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';

const repository = fileURLToPath(new URL('../..', import.meta.url));

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'loom signal build '));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const write = (relative, text, mode) => {
    const destination = path.join(root, relative);
    fs.mkdirSync(path.dirname(destination), { recursive: true });
    fs.writeFileSync(destination, text, { mode });
  };
  write('scripts/build-loom-signal.mjs', fs.readFileSync(path.join(repository, 'scripts/build-loom-signal.mjs')));
  write('rust-toolchain.toml', '[toolchain]\nchannel = "1.95.0"\n');
  write('bin/rustup', `#!/usr/bin/env node
const assert = require('node:assert/strict');
assert.deepEqual(process.argv.slice(2), ['which', '--toolchain', '1.95.0', 'rustc']);
console.log(${JSON.stringify(path.join(root, 'toolchain/rustc'))});
`, 0o755);
  write('toolchain/rustc', '#!/usr/bin/env node\nconsole.log("host: aarch64-apple-darwin");\n', 0o755);
  write('toolchain/cargo', `#!/usr/bin/env node
require('node:fs').writeFileSync(${JSON.stringify(path.join(root, 'build.json'))}, JSON.stringify({args: process.argv.slice(2), rustc: process.env.RUSTC}));
`, 0o755);
  // Different existing outputs expose both a wrong compiler profile and a stale
  // copy from the wrong directory. No real compilation or network is needed.
  write('products/loom/signal/target/debug/loom-signal', 'development worker fixture');
  write('products/loom/signal/target/release/loom-signal', 'release worker fixture');
  return {
    root,
    run(args, debug) {
      const env = { ...process.env, PATH: [path.join(root, 'bin'), path.dirname(process.execPath), process.env.PATH].join(path.delimiter) };
      delete env.TAURI_ENV_DEBUG;
      delete env.TAURI_ENV_TARGET_TRIPLE;
      if (debug !== undefined) env.TAURI_ENV_DEBUG = debug;
      return spawnSync(process.execPath, [path.join(root, 'scripts/build-loom-signal.mjs'), ...args], { env, encoding: 'utf8' });
    },
  };
}

for (const { name, args, debug, release } of [
  { name: 'direct invocation defaults to the development worker', args: [], release: false },
  { name: 'explicit release invocation builds and copies the release worker', args: ['--release'], release: true },
  { name: 'Tauri release hook with an unset debug flag builds and copies the release worker', args: ['--tauri-build'], release: true },
  { name: 'Tauri debug hook reuses the development worker', args: ['--tauri-build'], debug: 'true', release: false },
]) {
  test(name, { skip: process.platform === 'win32' ? 'Unix executable fixtures for the macOS release hook' : false }, (t) => {
    const f = fixture(t);
    const result = f.run(args, debug);
    assert.equal(result.status, 0, result.stderr);
    const build = JSON.parse(fs.readFileSync(path.join(f.root, 'build.json'), 'utf8'));
    assert.equal(build.args.includes('--release'), release, 'Cargo must receive the requested profile');
    assert.equal(build.rustc, path.join(f.root, 'toolchain/rustc'));
    const copied = fs.readFileSync(path.join(f.root, 'products/loom/apps/loom/src-tauri/binaries/loom-signal-aarch64-apple-darwin'), 'utf8');
    assert.equal(copied, release ? 'release worker fixture' : 'development worker fixture');
  });
}
