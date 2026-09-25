import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

export const BASE = '637e60b6b044230ed24ed3118615a2e5538cae83';
export const BASE_TREE = '29f329624d3030a7d3f2e5b33fa3887b1d2beae3';
export const REPOSITORY = 'delysis/native-platform';
export const BRANCH_PREFIX = 'consolidation/20260924/';
export const SHA = /^[0-9a-f]{40}$/u;

export function check(condition, message) {
  if (!condition) throw new Error(message);
}
export function sha256(bytes) { return createHash('sha256').update(bytes).digest('hex'); }
export function blobId(bytes) {
  return createHash('sha1').update(`blob ${bytes.length}\0`).update(bytes).digest('hex');
}
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
export function safeRelative(value) {
  check(typeof value === 'string' && value.length > 0, 'empty path');
  check(!path.isAbsolute(value) && !value.includes('\\') && !value.includes('\0') && !value.includes(':'), `unsafe path: ${value}`);
  check(!value.split('/').some(part => part === '' || part === '.' || part === '..' || part.toLowerCase() === '.git'), `unsafe path: ${value}`);
  return value;
}
export function safeJoin(root, relative) {
  safeRelative(relative);
  let current = root;
  for (const part of relative.split('/')) {
    current = path.join(current, part);
    if (fs.existsSync(current) || (() => { try { return fs.lstatSync(current).isSymbolicLink(); } catch { return false; } })()) {
      const stat = fs.lstatSync(current);
      check(!stat.isSymbolicLink(), `symlink is not an edit target: ${relative}`);
    }
  }
  return current;
}
export function readUtf8(file) {
  const bytes = fs.readFileSync(file);
  return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
}
export function replaceExactly(source, before, after, label) {
  check(typeof before === 'string' && before.length > 0, `${label}: empty anchor`);
  const at = source.indexOf(before);
  check(at >= 0 && source.indexOf(before, at + 1) < 0, `${label}: anchor absent or non-unique`);
  return source.slice(0, at) + after + source.slice(at + before.length);
}

// Cargo-generated lockfiles use one [[package]] table per package. Preserve
// every existing block byte-for-byte except the selected local dependencies.
// Dependency identities are copied from existing owner blocks; there is no
// resolver, network access, dependency upgrade or checksum replacement here.
export function lockBlocks(source) {
  check(source.startsWith('#') && /^version = [34]$/mu.test(source), 'unsupported Cargo.lock format');
  const at = source.indexOf('[[package]]\n');
  check(at >= 0, 'lockfile has no package table');
  const prefix = source.slice(0, at);
  const blocks = source.slice(at).split(/(?=^\[\[package\]\]$)/mu).filter(Boolean);
  return { prefix, blocks };
}
export function lockName(block) {
  const match = /^name = "([^"\n]+)"$/mu.exec(block);
  check(match, 'lock package missing name');
  return match[1];
}
function uniqueBlock(blocks, name) {
  const selected = blocks.filter(block => lockName(block) === name);
  check(selected.length === 1, `lock package ${name} absent or ambiguous`);
  return selected[0];
}
export function lockDependencies(block) {
  const match = /^dependencies = \[\n([\s\S]*?)^\]$/mu.exec(block);
  if (!match) return [];
  return match[1].split('\n').filter(line => line.trim()).map(line => {
    const value = /^ "([^"\n]+)",$/u.exec(line);
    check(value, 'unsupported lock dependency layout');
    return value[1];
  });
}
function setLockDependencies(block, dependencies) {
  const list = [...dependencies].sort().map(value => ` "${value}",\n`).join('');
  if (/^dependencies = \[/mu.test(block)) {
    return block.replace(/^dependencies = \[\n[\s\S]*?^\]$/mu, `dependencies = [\n${list}]`);
  }
  return block.trimEnd() + `\ndependencies = [\n${list}]\n\n`;
}
export function changeLock(source, operation) {
  const { prefix, blocks } = lockBlocks(source);
  if (operation.op === 'lock_add_workspace') {
    check(!blocks.some(block => lockName(block) === operation.name), `lock package already exists: ${operation.name}`);
    const dependencies = Object.entries(operation.dependencies_from).map(([dependency, owner]) => {
      const values = lockDependencies(uniqueBlock(blocks, owner)).filter(value => value.split(' ')[0] === dependency);
      check(values.length === 1, `${owner}: dependency ${dependency} absent or ambiguous`);
      return values[0];
    }).sort();
    check(/^[a-z][a-z0-9-]*$/u.test(operation.name), 'invalid local package name');
    check(/^\d+\.\d+\.\d+$/u.test(operation.version), 'invalid local version');
    const block = `[[package]]\nname = "${operation.name}"\nversion = "${operation.version}"\ndependencies = [\n${dependencies.map(value => ` "${value}",\n`).join('')}]\n\n`;
    const at = blocks.findIndex(existing => lockName(existing) > operation.name);
    blocks.splice(at < 0 ? blocks.length : at, 0, block);
  } else {
    check(operation.op === 'lock_add_dependencies', 'unknown lock operation');
    const original = uniqueBlock(blocks, operation.name);
    check(!/^source = /mu.test(original), 'cannot edit third-party package dependencies');
    const dependencies = lockDependencies(original);
    for (const dependency of operation.dependencies) {
      const target = uniqueBlock(blocks, dependency);
      check(!/^source = /mu.test(target), 'only existing workspace dependencies may be added');
      check(!dependencies.some(value => value.split(' ')[0] === dependency), `duplicate lock dependency ${dependency}`);
      dependencies.push(dependency);
    }
    blocks[blocks.indexOf(original)] = setLockDependencies(original, dependencies);
  }
  return prefix + blocks.join('');
}

export function loadStages(bundle) {
  return fs.readdirSync(path.join(bundle, 'stages')).filter(name => /^\d\d-.*\.json$/u.test(name)).sort().map(name => {
    const stage = JSON.parse(readUtf8(path.join(bundle, 'stages', name)));
    check(stage.schema_version === 1 && /^[0-9]{2}-[a-z0-9-]+$/u.test(stage.id), `invalid stage ${name}`);
    check(Array.isArray(stage.operations) && stage.operations.length, `empty stage ${name}`);
    return stage;
  });
}
export function verifyBundle(bundle) {
  const manifest = JSON.parse(readUtf8(path.join(bundle, 'manifest.json')));
  check(manifest.base_commit === BASE && manifest.base_tree === BASE_TREE, 'bundle pin mismatch');
  for (const [name, digest] of Object.entries(manifest.files)) {
    check(sha256(fs.readFileSync(safeJoin(bundle, name))) === digest, `bundle hash mismatch: ${name}`);
  }
  return manifest;
}

// All operations in all stages are simulated before any repository file is
// written. A late mismatch therefore cannot leave a half-applied source tree.
export function planStages(repo, bundle, stages, base = BASE) {
  check(SHA.test(base), 'invalid base');
  const state = new Map();
  const baseChecked = new Set();
  const plans = [];
  for (const stage of stages) {
    const changed = new Map();
    for (const op of stage.operations) {
      safeJoin(repo, op.path);
      if (!state.has(op.path)) {
        const file = safeJoin(repo, op.path);
        state.set(op.path, fs.existsSync(file) ? fs.readFileSync(file) : null);
      }
      let previous = state.get(op.path);
      if (op.base_blob && !baseChecked.has(op.path)) {
        check(SHA.test(op.base_blob), `${op.path}: bad blob ID`);
        const actual = git(repo, ['rev-parse', `${base}:${op.path}`]).trim();
        check(actual === op.base_blob, `${op.path}: pinned blob differs (${actual})`);
        check(previous !== null && blobId(previous) === op.base_blob, `${op.path}: working bytes differ from pinned blob`);
        baseChecked.add(op.path);
      }
      let next;
      if (op.op === 'add') {
        check(previous === null, `${op.path}: refuses to overwrite an existing file`);
        next = fs.readFileSync(safeJoin(bundle, op.source));
      } else {
        check(previous !== null, `${op.path}: existing file required`);
        check(op.base_blob, `${op.path}: existing edits must declare a pinned blob`);
        let text = new TextDecoder('utf-8', { fatal: true }).decode(previous);
        if (op.op === 'replace') {
          const after = op.after_file ? readUtf8(safeJoin(bundle, op.after_file)) : op.after;
          check(typeof after === 'string', `${op.path}: replacement must be text`);
          text = replaceExactly(text, op.before, after, op.path);
        } else if (op.op === 'json_add_unique') {
          const object = JSON.parse(text);
          let selected = object;
          for (const key of op.keys) {
            check(Object.hasOwn(selected, key), `${op.path}: missing JSON path`);
            selected = selected[key];
          }
          check(Array.isArray(selected), `${op.path}: expected JSON array`);
          for (const value of op.values) {
            check(typeof value === 'string' && !selected.includes(value), `${op.path}: duplicate value ${value}`);
            selected.push(value);
          }
          selected.sort();
          text = JSON.stringify(object, null, 2) + '\n';
        } else if (op.op === 'lock_add_workspace' || op.op === 'lock_add_dependencies') {
          text = changeLock(text, op);
        } else { throw new Error(`unknown operation ${op.op}`); }
        next = Buffer.from(text);
      }
      state.set(op.path, next);
      changed.set(op.path, next);
    }
    plans.push({ stage, files: changed });
  }
  return plans;
}
export function writePlan(repo, plan) {
  for (const [relative, bytes] of plan.files) {
    const file = safeJoin(repo, relative);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, bytes, { mode: 0o644 });
  }
}
export function exclusiveDirectory(directory) {
  // mkdir without recursive/exist_ok: never erase or reuse a user's output.
  fs.mkdirSync(directory, { mode: 0o700 });
}
export function assertClean(repo) {
  check(git(repo, ['status', '--porcelain=v1', '--untracked-files=all']).trim() === '', 'repository is not clean');
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
