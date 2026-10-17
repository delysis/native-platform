#!/usr/bin/env node
// Build one reviewable candidate from the exact clean tree that passed local-ci.
// Never installs, launches, modifies user state, or claims device acceptance.
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { closeSync, cpSync, existsSync, mkdirSync, openSync, readFileSync, readdirSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { basename, delimiter, dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), '../../..'));
// CONTRIBUTING.md and supported CI/release workflows pin Node 22. Reject other
// majors before creating the output directory or writing any candidate receipt.
const pinnedNodeMajor = 22;
const nodeMajor = Number(process.versions.node.split('.')[0]);
if (nodeMajor !== pinnedNodeMajor) {
  throw new Error(`Node ${pinnedNodeMajor}.x is required; found ${process.version}`);
}
const [receiptPath, outputPath] = process.argv.slice(2);
if (!receiptPath || !outputPath || process.argv.length !== 4) {
  throw new Error('Usage: node products/mom/scripts/build-voice-candidate.mjs <local-ci/summary.json> <new-output-directory>');
}
if (process.platform !== 'darwin' || process.arch !== 'arm64') throw new Error('Apple Silicon macOS required');
const sysctl = '/usr/sbin/sysctl';
const swVers = '/usr/bin/sw_vers';
if (!existsSync(sysctl) || !existsSync(swVers)) throw new Error('Required macOS identity tools are absent');
// Resolve the existing parent before any write: an outside-looking symlink
// must not redirect a candidate or its evidence into this checkout.
const requestedOutput = resolve(outputPath);
const output = join(realpathSync(dirname(requestedOutput)), basename(requestedOutput));
if (existsSync(output)) throw new Error('Preserve prior receipts: choose a new output directory');
if (output === root || output.startsWith(`${root}/`)) throw new Error('Candidate output must be outside the source checkout');
for (const key of ['GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE']) {
  if (process.env[key]) throw new Error(`Unset ${key}`);
}
const read = (command, args, cwd = root, env = process.env) => execFileSync(command, args, { cwd, env, encoding: 'utf8' }).trim();
const git = (...args) => read('git', ['--no-optional-locks', '-c', 'core.fsmonitor=false', ...args]);
function source() {
  if (git('status', '--porcelain=v1', '--untracked-files=all', '--ignore-submodules=none')) throw new Error('Source must be clean');
  if (git('ls-files', '-v').split('\n').some(line => !line.startsWith('H '))) throw new Error('Hidden/sparse index entries are not allowed');
  return { sha: git('rev-parse', 'HEAD'), tree: git('rev-parse', 'HEAD^{tree}') };
}
const before = source();
const bundleIdentifier = 'com.delysis.blessenger.voice-qualification';
const dataNamespace = 'voice-qualification';
const receiptBytes = readFileSync(resolve(receiptPath));
const receipt = JSON.parse(receiptBytes);
if (receipt.status !== 'component-pass' || !receipt.source_unchanged || !receipt.component_gate_complete
    || receipt.source_start?.sha !== before.sha || receipt.source_end?.sha !== before.sha
    || receipt.source_start?.tree !== before.tree || receipt.source_end?.tree !== before.tree) {
  throw new Error('A passing complete local-ci receipt for this exact commit/tree is required');
}
const cargo = read('rustup', ['which', '--toolchain', '1.95.0', 'cargo']);
const rustc = read('rustup', ['which', '--toolchain', '1.95.0', 'rustc']);
const nodeDirectory = dirname(realpathSync(process.execPath));
const npx = join(nodeDirectory, 'npx');
if (!existsSync(npx)) throw new Error(`Pinned Node installation has no npx: ${npx}`);
const env = { ...process.env, RUSTUP_TOOLCHAIN: '1.95.0', CARGO: cargo, RUSTC: rustc,
  PATH: [nodeDirectory, dirname(cargo), process.env.PATH || ''].filter(Boolean).join(delimiter),
  MOM_LLAMA_DATA_NAMESPACE: dataNamespace };
// Resolve and validate all tool identities before the first output write. The
// pinned Node directory leads PATH so npx/pnpm child scripts cannot drift to a
// different `node` even if the caller's PATH does.
const pnpmVersion = read(npx, ['--offline', '--yes=false', 'pnpm@11.16.0', '--version'], root, env);
if (pnpmVersion !== '11.16.0') throw new Error(`Pinned pnpm 11.16.0 required; found ${pnpmVersion}`);
const tools = {
  node: { version: process.version, path: realpathSync(process.execPath) },
  cargo: { version: read(cargo, ['--version'], root, env), path: realpathSync(cargo) },
  rustc: { version: read(rustc, ['--version'], root, env), path: realpathSync(rustc) },
  pnpm: { package: 'pnpm@11.16.0', version: pnpmVersion, node_version: process.version, npx_path: realpathSync(npx) }
};
const hardware = read(sysctl, ['-n', 'hw.model'], root, env);
const os = read(swVers, ['-buildVersion'], root, env);
mkdirSync(output, { recursive: true });
const manifest = { bundle_identifier: bundleIdentifier, status: 'incomplete', source: before, started: new Date().toISOString(),
  data_namespace: dataNamespace,
  data_directory_policy: 'compiled namespace is appended only to the default fallback; LLAMA_NATIVE_KIT_DATA_DIR takes precedence',
  build_profile: 'release',
  tools,
  component_receipt: resolve(receiptPath), component_receipt_sha256: createHash('sha256').update(receiptBytes).digest('hex'),
  hardware, os,
  acceptance: { packaged_voice: 'pending', physical_echo_double_talk: 'pending', m2: 'pending', thirty_minute_soak: 'pending' } };
const publish = () => writeFileSync(join(output, 'candidate.json'), `${JSON.stringify(manifest, null, 2)}\n`);
publish();
try {
  const log = openSync(join(output, 'build.log'), 'wx');
  let build;
  try {
    // Tauri builds release by default. Keep the final latency candidate on the
    // optimized path; debug crypto makes multi-gigabyte state persistence
    // timings unrepresentative.
    build = spawnSync(npx, ['--offline', '--yes=false', 'pnpm@11.16.0', '--filter', '@delysis/mom-llama', 'exec', 'tauri', 'build', '--bundles', 'app', '--config', JSON.stringify({ identifier: bundleIdentifier })],
      { cwd: root, env, stdio: ['ignore', log, log] });
  } finally { closeSync(log); }
  if (build.error || build.status !== 0) throw new Error(`Candidate build failed: ${build.error?.message || build.status}; preserve build.log`);
  const after = source();
  if (JSON.stringify(before) !== JSON.stringify(after)) throw new Error('Source changed during build');
  const metadata = JSON.parse(execFileSync(cargo, ['metadata', '--locked', '--no-deps', '--format-version', '1'], { cwd: root, env, encoding: 'utf8' }));
  const bundle = join(metadata.target_directory, 'release/bundle/macos/Blessenger.app');
  if (!existsSync(bundle)) throw new Error('Expected packaged app is absent');
  const destination = join(output, 'Blessenger Voice Candidate.app');
  cpSync(bundle, destination, { recursive: true, dereference: false });
  const files = [];
  function inventory(path) {
    for (const entry of readdirSync(path, { withFileTypes: true })) {
      const full = join(path, entry.name);
      if (entry.isDirectory()) inventory(full);
      else if (entry.isFile()) files.push({ path: relative(destination, full), bytes: statSync(full).size,
        sha256: createHash('sha256').update(readFileSync(full)).digest('hex') });
      else throw new Error(`Unexpected bundle link or special file: ${full}`);
    }
  }
  inventory(destination);
  manifest.bundle = realpathSync(destination);
  manifest.files = files.sort((a, b) => a.path.localeCompare(b.path));
  manifest.status = 'built-candidate-not-device-qualified';
  manifest.finished = new Date().toISOString();
  publish();
  console.log(manifest.bundle);
} catch (error) {
  manifest.status = 'failed'; manifest.error = String(error); publish(); throw error;
}
