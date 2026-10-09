#!/usr/bin/env node
/** Exact-head preservation in a separate local repository and source archive. */
import fs from 'node:fs';
import path from 'node:path';
import { check, exclusiveDirectory, git, parseOptions, sha256 } from './stack-lib.mjs';

const HEADS = {
  'easl-text-research': '54b955336590e97c04f40f2548c8a271d9c2b2ba',
  'easl-accessibility-lock-delta': '3fd6d84f4e99fa340d3b0b345e45712ded917374',
  'easl-accessibility-lock-companion': 'f9cff570edc1ac60f4965fc96c59f46d5d043e62',
  'easl-earlier-native': '78392f82762193b3c3f107d981c280f15d727b5d',
};
function main() {
  const options = parseOptions(process.argv.slice(2), ['--source', '--out']);
  check(options['--source'] && options['--out'], 'Usage: node tools/preserve-easl.mjs --source /repo --out /NEW-directory');
  const source = fs.realpathSync(options['--source']);
  const out = path.resolve(options['--out']);
  check(out !== source && !out.startsWith(source + path.sep), 'output must be outside source');
  // All heads must already exist locally. A missing head is not silently
  // omitted. This tool does not contact GitHub or rewrite the source refs.
  for (const sha of Object.values(HEADS)) check(git(source, ['rev-parse', `${sha}^{commit}`]).trim() === sha, `missing preserved head ${sha}`);
  exclusiveDirectory(out);
  const bare = path.join(out, 'easl-research.git');
  git(source, ['init', '--bare', bare]);
  const preservation = [];
  for (const [name, sha] of Object.entries(HEADS)) {
    git(bare, ['fetch', '--no-tags', '--', source, `${sha}:refs/heads/${name}`]);
    preservation.push({ branch: name, head: sha, tree: git(bare, ['rev-parse', `${sha}^{tree}`]).trim() });
  }
  git(bare, ['symbolic-ref', 'HEAD', 'refs/heads/easl-text-research']);
  const bundle = path.join(out, 'easl-preserved-history.bundle');
  git(bare, ['bundle', 'create', bundle, '--all']);
  git(bare, ['bundle', 'verify', bundle]);
  const head = HEADS['easl-text-research'];
  const rootNames = git(bare, ['ls-tree', '--name-only', head]).trim().split('\n');
  const wanted = ['Cargo.toml', 'Cargo.lock', 'LICENSE', 'LICENSE-APACHE', 'LICENSE-MIT', 'rust-toolchain.toml', 'vendor'];
  const paths = wanted.filter(name => rootNames.includes(name));
  check(git(bare, ['cat-file', '-t', `${head}:crates/services/easl`]).trim() === 'tree', 'EASL service tree missing');
  paths.push('crates/services/easl');
  const archive = path.join(out, 'easl-text-vendor-source.tar');
  git(bare, ['archive', '--format=tar', `--output=${archive}`, head, '--', ...paths]);
  fs.writeFileSync(path.join(out, 'preservation.json'), JSON.stringify({
    schema: 'native-platform.easl-preservation.v1', heads: preservation,
    history_bundle_sha256: sha256(fs.readFileSync(bundle)), source_archive_sha256: sha256(fs.readFileSync(archive)),
    source_archive_paths: paths, source_archive_base: head,
    source_archive_is_standalone_workspace: false,
    product_frontend: 'parked in complete preserved history; excluded from focused source archive',
    source_repository_changed: false, published: false, native_acceptance: false,
  }, null, 2) + '\n');
  console.log(`Preserved ${preservation.length} exact EASL heads and complete reachable history in ${out}.`);
  console.log('No source branches removed or merged. Focused source archive still needs workspace/dependency-closure qualification.');
}
try { main(); } catch (error) { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; }
