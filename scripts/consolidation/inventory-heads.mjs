#!/usr/bin/env node
/** Inventory all locally visible branch/remote/PR heads; never infer correctness. */
import fs from 'node:fs';
import path from 'node:path';
import { BASE, REPOSITORY, SHA, check, exclusiveDirectory, git, parseOptions } from './stack-lib.mjs';

function main() {
  const options = parseOptions(process.argv.slice(2), ['--source', '--out'], ['--fetch']);
  check(options['--source'] && options['--out'], 'Usage: node tools/inventory-heads.mjs --source /repo --out /NEW-directory [--fetch]');
  const repo = fs.realpathSync(options['--source']);
  const out = path.resolve(options['--out']);
  check(out !== repo && !out.startsWith(repo + path.sep), 'output must be outside source tree');
  if (options['--fetch']) {
    const origin = git(repo, ['remote', 'get-url', 'origin']).trim();
    check([`https://github.com/${REPOSITORY}.git`, `git@github.com:${REPOSITORY}.git`].includes(origin), 'fetch origin must be the audited repository');
    // Explicit opt-in, no prune, no local branch updates, no tags or working
    // tree changes. Remote-tracking and review refs retain current head data.
    git(repo, ['fetch', '--no-tags', 'origin',
      '+refs/heads/*:refs/remotes/origin/*', '+refs/pull/*/head:refs/review/github-pr/*']);
  }
  check(git(repo, ['rev-parse', `${BASE}^{commit}`]).trim() === BASE, 'base commit missing');
  exclusiveDirectory(out);
  const refs = git(repo, ['for-each-ref', '--format=%(refname)%00%(objectname)%00%(objecttype)',
    'refs/heads', 'refs/remotes', 'refs/pull', 'refs/review']).trim().split('\n').filter(Boolean);
  const cache = new Map();
  const results = [];
  for (const row of refs) {
    const [ref, head, type] = row.split('\0');
    check(SHA.test(head), 'unsupported object ID');
    if (type !== 'commit' || ref.endsWith('/HEAD')) continue;
    if (!cache.has(head)) {
      try {
        const [behind, ahead] = git(repo, ['rev-list', '--left-right', '--count', `${BASE}...${head}`]).trim().split(/\s+/u).map(Number);
        const mergeBases = git(repo, ['merge-base', '--all', BASE, head]).trim().split('\n');
        const from = mergeBases.length === 1 ? mergeBases[0] : BASE;
        const changedPaths = git(repo, ['diff', '--name-only', '-z', from, head, '--']).split('\0').filter(Boolean);
        const patchEquivalence = ahead > 0 ? git(repo, ['cherry', '-v', BASE, head]).trim().split('\n').filter(Boolean) : [];
        cache.set(head, {
          head, tree: git(repo, ['rev-parse', `${head}^{tree}`]).trim(), ahead, behind,
          merge_bases: mergeBases, path_comparison_base: from,
          status: ahead === 0 ? (behind === 0 ? 'identical' : 'ancestor') : (behind === 0 ? 'ahead' : 'diverged'),
          changed_paths: changedPaths, patch_equivalence: patchEquivalence,
          code_review: 'not established by ancestry or patch equivalence',
          promotion: 'requires source review, explicit capability defaults and affected acceptance gates',
        });
      } catch (error) {
        cache.set(head, { head, status: 'comparison_failed', error: error instanceof Error ? error.message : String(error),
          promotion: 'preserve; do not merge or delete based on incomplete comparison' });
      }
    }
    results.push({ ref, ...cache.get(head) });
    fs.writeFileSync(path.join(out, 'heads.json'), JSON.stringify({ repository: REPOSITORY, base: BASE,
      scope: 'locally visible heads/remotes/PR refs; fetch opt-in is recorded', fetched: Boolean(options['--fetch']),
      complete: false, results }, null, 2) + '\n');
  }
  fs.writeFileSync(path.join(out, 'heads.json'), JSON.stringify({ repository: REPOSITORY, base: BASE,
    scope: 'locally visible heads/remotes/PR refs', fetched: Boolean(options['--fetch']),
    complete: true, comparison_failures: results.filter(row => row.status === 'comparison_failed').length, results }, null, 2) + '\n');
  console.log(`Inventoried ${results.length} refs (${cache.size} distinct heads). No merges, branch deletions or correctness claims.`);
}
try { main(); } catch (error) { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; }
