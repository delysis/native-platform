// Build the isolated SQLCipher client with the root toolchain before packaging.
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync } from 'node:fs';
import { delimiter, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const worker = join(root, 'products/loom/signal');
const toolchain = /^channel\s*=\s*"([^"]+)"/m.exec(readFileSync(join(root, 'rust-toolchain.toml'), 'utf8'))?.[1];
if (!toolchain) throw new Error('The root toolchain pin is missing.');
// Select the entire toolchain: Cargo otherwise finds a possibly different
// compiler or Clippy in PATH on machines with both Homebrew and Rustup.
const compiler = spawnSync('rustup', ['which', '--toolchain', toolchain, 'rustc'], { encoding: 'utf8' });
if (compiler.status !== 0) throw new Error('Install the root toolchain with rustup first.');
const compilerPath = compiler.stdout.trim();
const env = { ...process.env, RUSTUP_TOOLCHAIN: toolchain, CARGO_TARGET_DIR: join(worker, 'target'),
  RUSTC: compilerPath, PATH: `${dirname(compilerPath)}${delimiter}${process.env.PATH ?? ''}` };
const release = process.argv.includes('--release') || process.env.TAURI_ENV_DEBUG === 'false';
const hostResult = spawnSync(compilerPath, ['-vV'], { cwd: worker, env, encoding: 'utf8' });
if (hostResult.status !== 0) throw new Error('The pinned Signal Rust toolchain is unavailable.');
const host = /^host: (.+)$/m.exec(hostResult.stdout)?.[1];
const target = process.env.TAURI_ENV_TARGET_TRIPLE ?? host;
if (!target || !/^[a-zA-Z0-9_-]+$/.test(target)) throw new Error('Invalid Signal build target.');
const args = ['build', '--locked', '-j', process.env.CARGO_BUILD_JOBS ?? '4'];
// Host builds share the artifacts produced by cargo test and Clippy. An
// explicit host --target would compile the same graph into a second directory.
if (target !== host) args.push('--target', target);
if (release) args.push('--release');
const build = spawnSync(join(dirname(compilerPath), process.platform === 'win32' ? 'cargo.exe' : 'cargo'), args, { cwd: worker, env, stdio: 'inherit' });
if (build.status !== 0) process.exit(build.status ?? 1);
const suffix = target.includes('windows') ? '.exe' : '';
const artifacts = target === host ? join(worker, 'target') : join(worker, 'target', target);
const binary = join(artifacts, release ? 'release' : 'debug', `loom-signal${suffix}`);
const destination = join(root, 'products/loom/apps/loom/src-tauri/binaries');
mkdirSync(destination, { recursive: true });
copyFileSync(binary, join(destination, `loom-signal-${target}${suffix}`));
