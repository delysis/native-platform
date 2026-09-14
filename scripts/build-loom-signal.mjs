// Build the separately pinned Rust/SQLCipher client before Tauri packages it.
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync } from 'node:fs';
import { delimiter, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const worker = join(root, 'products/loom/signal');
const toolchain = /^channel\s*=\s*"([^"]+)"/m.exec(readFileSync(join(worker, 'rust-toolchain.toml'), 'utf8'))?.[1];
if (!toolchain) throw new Error('The Signal toolchain pin is missing.');
// CI pins the main workspace through RUSTUP_TOOLCHAIN. The isolated worker
// must honor its own checked-in compiler and target directory instead.
const compiler = spawnSync('rustup', ['which', '--toolchain', toolchain, 'rustc'], { encoding: 'utf8' });
if (compiler.status !== 0) throw new Error('Install the checked-in Signal toolchain with rustup first.');
const compilerPath = compiler.stdout.trim();
const env = { ...process.env, RUSTUP_TOOLCHAIN: toolchain, CARGO_TARGET_DIR: join(worker, 'target'),
  RUSTC: compilerPath, PATH: `${dirname(compilerPath)}${delimiter}${process.env.PATH ?? ''}` };
const release = process.argv.includes('--release');
const hostResult = spawnSync(compilerPath, ['-vV'], { cwd: worker, env, encoding: 'utf8' });
if (hostResult.status !== 0) throw new Error('The pinned Signal Rust toolchain is unavailable.');
const host = /^host: (.+)$/m.exec(hostResult.stdout)?.[1];
const target = process.env.TAURI_ENV_TARGET_TRIPLE ?? host;
if (!target || !/^[a-zA-Z0-9_-]+$/.test(target)) throw new Error('Invalid Signal build target.');
const args = ['build', '--locked', '--target', target, '-j', process.env.CARGO_BUILD_JOBS ?? '4'];
if (release) args.push('--release');
const build = spawnSync(join(dirname(compilerPath), process.platform === 'win32' ? 'cargo.exe' : 'cargo'), args, { cwd: worker, env, stdio: 'inherit' });
if (build.status !== 0) process.exit(build.status ?? 1);
const suffix = target.includes('windows') ? '.exe' : '';
const binary = join(worker, 'target', target, release ? 'release' : 'debug', `loom-signal${suffix}`);
const destination = join(root, 'products/loom/apps/loom/src-tauri/binaries');
mkdirSync(destination, { recursive: true });
copyFileSync(binary, join(destination, `loom-signal-${target}${suffix}`));
