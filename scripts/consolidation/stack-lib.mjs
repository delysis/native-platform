import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

export const BASE = '637e60b6b044230ed24ed3118615a2e5538cae83';
export const REPOSITORY = 'delysis/native-platform';
export const SHA = /^[0-9a-f]{40}$/u;

export function check(condition, message) {
  if (!condition) throw new Error(message);
}
export function sha256(bytes) { return createHash('sha256').update(bytes).digest('hex'); }
export function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    encoding: 'utf8', maxBuffer: 64 * 1024 * 1024,
    timeout: 300_000, ...options,
  });
  if (result.error) throw new Error(`${command}: ${result.error.message}`);
  check(result.status === 0, `${command} ${args.join(' ')} failed (${result.status}):\n${result.stderr || ''}`);
  return result.stdout;
}
export function git(repo, args, options = {}) {
  return run('git', ['-c', 'core.hooksPath=/dev/null', '-C', repo, ...args], options);
}
export function exclusiveDirectory(directory, source) {
  // mkdir without recursive/exist_ok: never erase or reuse a user's output.
  const canonicalSource = fs.realpathSync(source);
  const canonicalParent = fs.realpathSync(path.dirname(directory));
  const materialized = path.join(canonicalParent, path.basename(directory));
  check(
    materialized !== canonicalSource && !materialized.startsWith(canonicalSource + path.sep),
    'output must materialize outside source',
  );
  fs.mkdirSync(directory, { mode: 0o700 });
  check(fs.realpathSync(directory) === materialized, 'created output identity changed');
}
export function parseOptions(argv, allowed, switches = []) {
  const result = {};
  for (let i = 0; i < argv.length; i++) {
    const key = argv[i];
    check(allowed.includes(key) || switches.includes(key), `unknown option: ${key}`);
    check(!Object.hasOwn(result, key), `duplicate option: ${key}`);
    if (switches.includes(key)) { result[key] = true; continue; }
    check(i + 1 < argv.length && !argv[i + 1].startsWith('--'), `${key} needs a value`);
    result[key] = argv[++i];
  }
  return result;
}
