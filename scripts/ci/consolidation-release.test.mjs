// These tests execute the production shell selectors and exact-pass gate with
// process doubles. They do not compile Rust, run a model, or qualify an app.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const release = readFileSync(path.join(root, 'scripts/release-macos.sh'), 'utf8');
const manifest = readFileSync(path.join(root, 'products/mom/apps/mom-llama/src-tauri/Cargo.toml'), 'utf8');
const shellOptions = { skip: process.platform === 'win32' };
const joinTest = 'app_runtime::tests::direct_native_operation_drains_before_final_join';
const momSpecs = [
  ['mom-llama-runtime', 'lib', 'unused', 'store::tests::unrelated_files_are_not_store_inputs_or_rewritten'],
  ['mom-llama-runtime', 'lib', 'unused', 'kv_cache::tests::persistent_cache_corruption_invalidates_and_falls_back_after_reopen'],
  ['mom-llama-app', 'lib', 'unused', joinTest],
];
const loomSpecs = [
  ['loom-store', 'lib', 'unused', 'generation::tests::exact_boundary_suggestion_promotion_survives_store_reopen'],
  ['tauri-plugin-loom', 'lib', 'unused', 'tests::close_cancels_active_family_waits_for_terminal_release_and_replays'],
];
const fteSpecs = [
  ['free-token-energy', 'lib', 'unused', 'db::tests::local_model_configuration_survives_database_reopen'],
  ['free-token-energy', 'lib', 'unused', 'db::tests::fresh_database_is_versioned_and_reopens_only_as_the_current_schema'],
  ['free-token-energy', 'lib', 'unused', 'gateway_runtime::tests::runtime_shutdown_reports_every_owned_worker_and_native_join'],
  ['fte-router', 'lib', 'unused', 'tests::shutdown_cancels_active_work_and_waits_for_authoritative_completion'],
];

function shellFunction(name, required = true) {
  // Known repository-owned POSIX shell functions terminate at an unindented }.
  const body = release.match(new RegExp(`^${name}\\(\\) \\{[\\s\\S]*?^\\}`, 'm'))?.[0];
  if (required) assert.ok(body, `missing production function ${name}`);
  return body ?? '';
}

function checkCase() {
  const install = release.indexOf('\nrun pnpm install ');
  const start = release.indexOf('\ncase "$COMPONENT" in\n', install);
  const end = release.indexOf('\nesac', start);
  assert.ok(install >= 0 && start > install && end > start, 'missing release check dispatch');
  assert.ok(end < release.indexOf('\nTARGET_DIR=$(', end), 'checks precede artifact build');
  return release.slice(start, end + '\nesac'.length);
}

function runShell(input, env = {}) {
  const result = spawnSync('/bin/sh', [], {
    input: `set -eu\n${input}`, encoding: 'utf8', timeout: 5_000,
    // Do not inherit model/credential/configuration variables into fixtures.
    env: { PATH: process.env.PATH, LC_ALL: 'C', ...env },
  });
  assert.ifError(result.error);
  assert.equal(result.signal, null, 'fixture shell must exit, not be killed');
  return result;
}

function selectedChecks(component, failTest = '') {
  const directory = `/fixture/${component}`;
  return runShell(`
${shellFunction('run_mom_preservation_checks', false)}
run_exact_test() {
  printf 'TEST\\t%s\\t%s\\t%s\\t%s\\n' "$1" "$2" "$3" "$4"
  if [ "$4" = "$FAIL_TEST" ]; then return 37; fi
}
run() { printf 'RUN'; printf '\\t%s' "$@"; printf '\\n'; }
record_check() { printf 'RECEIPT\\t%s\\n' "$1"; }
${checkCase()}
printf 'PRODUCT_DIR\\t%s\\n' "$PRODUCT_DIR"
printf 'FINISHED\\n'
`, { ROOT: '/fixture', PRODUCT_DIR: directory, COMPONENT: component, FAIL_TEST: failTest });
}

function records(output, type) {
  return output.split('\n').filter(line => line.startsWith(`${type}\t`))
    .map(line => line.split('\t').slice(1));
}

test('Mom tests remain owned by the library, not the compatibility launcher', () => {
  const library = manifest.match(/^\[lib\]\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1];
  const binary = manifest.match(/^\[\[bin\]\]\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1];
  assert.ok(library && binary, 'both targets remain explicit');
  assert.match(library, /^path = "src\/main.rs"$/m);
  assert.match(binary, /^path = "src\/launcher.rs"$/m);
});

for (const component of ['mom', 'loom']) {
  test(`${component} release checks retain the full Mom contract on the library target`, shellOptions, () => {
    const result = selectedChecks(component);
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(records(result.stdout, 'TEST'), [
      ...momSpecs, ...(component === 'loom' ? loomSpecs : []),
    ]);
    assert.deepEqual(records(result.stdout, 'RUN'), [
      ['pnpm', '--dir', '/fixture/products/mom/apps/mom-llama', 'run', 'check:frontend'],
      ['pnpm', '--dir', '/fixture/products/mom/apps/mom-llama', 'run', 'test:frontend'],
      ...(component === 'loom' ? [['pnpm', '--dir', '/fixture/loom', 'test']] : []),
    ]);
    assert.deepEqual(records(result.stdout, 'RECEIPT'), [
      ['@delysis/mom-llama::frontend-check'], ['@delysis/mom-llama::frontend-tests'],
      ...(component === 'loom' ? [['@delysis/loom::frontend-tests']] : []),
    ]);
    assert.deepEqual(records(result.stdout, 'PRODUCT_DIR'), [[`/fixture/${component}`]]);
  });
  test(`${component} stops before packaging when the inherited Mom join gate fails`, shellOptions, () => {
    const result = selectedChecks(component, joinTest);
    assert.equal(result.status, 37, result.stderr);
    assert.doesNotMatch(result.stdout, /FINISHED/);
    assert.deepEqual(records(result.stdout, 'TEST'), momSpecs);
    assert.deepEqual(records(result.stdout, 'RUN'), []);
  });
}

test('FTE release selection remains independent of Mom and Loom checks', shellOptions, () => {
  const result = selectedChecks('fte');
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(records(result.stdout, 'TEST'), fteSpecs);
  assert.deepEqual(records(result.stdout, 'RUN'), [['pnpm', '--dir', '/fixture/fte', 'run', 'test:frontend']]);
  assert.deepEqual(records(result.stdout, 'RECEIPT'), [['free-token-energy::frontend-tests']]);
});

const passed = 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.01s\n';
const outcomes = [
  ['one executed pass', passed, 0, 0],
  ['zero matching tests', passed.replace('1 passed', '0 passed'), 0, 1],
  ['an ignored test', passed.replace('1 passed', '0 passed').replace('0 ignored', '1 ignored'), 0, 1],
  ['two passing tests', passed.replace('1 passed', '2 passed'), 0, 1],
  ['two test summaries', passed + passed, 0, 1],
  ['no summary', 'running 0 tests\n', 0, 1],
  ['cargo failure after pass-shaped output', passed, 101, 101],
];
for (const [name, output, cargoStatus, expected] of outcomes) {
  test(`exact release gate handles ${name} without inventing a receipt`, shellOptions, () => {
    const dir = mkdtempSync(path.join(os.tmpdir(), 'consolidation-release-'));
    try {
      writeFileSync(path.join(dir, 'stdout'), output);
      const result = runShell(`
CHECKS=
${shellFunction('run')}
${shellFunction('record_check')}
${shellFunction('run_exact_test')}
rustup() {
  printf '%s\\n' "$@" > "$FIXTURE_DIR/args"
  cat "$FIXTURE_DIR/stdout"
  return "$CARGO_STATUS"
}
# Catch the status explicitly to inspect the production receipt accumulator.
status=0
run_exact_test mom-llama-app lib unused "$JOIN_TEST" || status=$?
printf 'CHECKS=%s\\n' "$CHECKS"
exit "$status"
`, { FIXTURE_DIR: dir, CARGO_STATUS: String(cargoStatus), JOIN_TEST: joinTest });
      assert.equal(result.status, expected, result.stderr);
      assert.deepEqual(readFileSync(path.join(dir, 'args'), 'utf8').trimEnd().split('\n'), [
        'run', '1.92.0', 'cargo', 'test', '--locked', '-p', 'mom-llama-app',
        '--lib', joinTest, '--', '--exact', '--format', 'pretty', '--color', 'never',
      ]);
      const receipts = result.stdout.match(/^CHECKS=(.*)$/m)?.[1];
      assert.equal(receipts, expected === 0 ? `mom-llama-app::${joinTest}` : '');
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
}

test('new release regressions run in the existing consolidation CI entry', () => {
  const source = readFileSync(path.join(root, 'scripts/ci/consolidation-contracts.test.mjs'), 'utf8');
  assert.match(source, /^import '\.\/consolidation-release\.test\.mjs';$/m);
});
