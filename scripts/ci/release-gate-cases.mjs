// Executes the production shell gate with a controlled Cargo transcript.
// This tests release orchestration, not Rust compilation or product behavior.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const releasePath = path.resolve(import.meta.dirname, "../release-macos.sh");
const testName = "tests::release_boundary";
const success = `\nrunning 1 test\ntest ${testName} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.00s\n`;

function runGate(t, { output = success, exit = 0, listed = true, kind = "lib" } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "release-exact-test-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const source = fs.readFileSync(releasePath, "utf8");
  const functions = ["run", "record_check", "run_exact_test"].map((name) => {
    const match = source.match(new RegExp(`^${name}\\(\\) \\{[\\s\\S]*?^\\}`, "m"));
    assert.ok(match, `missing production function ${name}`);
    return match[0];
  }).join("\n\n");
  const calls = path.join(root, "calls");
  const outputPath = path.join(root, "stdout");
  fs.writeFileSync(outputPath, output);
  const script = `set -eu
CHECKS=
${functions}
rustup() {
  printf '%s\\n' "$*" >> "$GATE_CALLS"
  case " $* " in
    *" --list "*)
      if [ "$GATE_LISTED" = yes ]; then printf '%s: test\\n' "$GATE_TEST_NAME"; fi
      return 0
      ;;
  esac
  cat "$GATE_STDOUT"
  return "$GATE_EXIT"
}
run_exact_test fixture "$GATE_KIND" fixture-bin "$GATE_TEST_NAME"
printf 'RECORDED=%s\\n' "$CHECKS"
`;
  const result = spawnSync("sh", ["-c", script], {
    encoding: "utf8", timeout: 10_000,
    env: {
      ...process.env, TMPDIR: root,
      GATE_CALLS: calls, GATE_STDOUT: outputPath, GATE_TEST_NAME: testName,
      GATE_EXIT: String(exit), GATE_LISTED: listed ? "yes" : "no", GATE_KIND: kind,
    },
  });
  assert.equal(result.error, undefined);
  return { ...result, calls: fs.existsSync(calls) ? fs.readFileSync(calls, "utf8") : "" };
}

for (const kind of ["lib", "bin"]) {
  test(`release exact gate accepts one executed ${kind} test`, (t) => {
    const result = runGate(t, { kind });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /RECORDED=fixture::tests::release_boundary/);
    assert.match(result.calls, kind === "lib" ? /--lib/ : /--bin fixture-bin/);
    assert.match(result.calls, /tests::release_boundary -- --exact/);
  });
}

for (const [name, output] of [
  ["ignored test", `test ${testName} ... ignored\n\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 12 filtered out; finished in 0.00s\n`],
  ["zero execution", "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 13 filtered out; finished in 0.00s\n"],
  ["missing summary", `test ${testName} ... ok\n`],
  ["ambiguous summaries", success + success],
]) {
  test(`release exact gate rejects ${name}`, (t) => {
    const result = runGate(t, { output });
    assert.notEqual(result.status, 0, result.stdout);
    assert.doesNotMatch(result.stdout, /RECORDED=/);
  });
}

test("release exact gate preserves failing Cargo status", (t) => {
  const result = runGate(t, { output: success, exit: 101 });
  assert.equal(result.status, 101);
  assert.doesNotMatch(result.stdout, /RECORDED=/);
});

test("release exact checks contain no duplicate invocations", () => {
  const source = fs.readFileSync(releasePath, "utf8");
  const checks = [...source.matchAll(/^\s+run_exact_test (.+)$/gm)].map((match) => match[1]);
  assert.ok(checks.length > 0);
  assert.equal(new Set(checks).size, checks.length, "repeated checks are not independent evidence");
});

// This is a selection-contract check; actual Rust execution remains a Mac gate.
test("Mom release selects the current no-migration storage contract", () => {
  const source = fs.readFileSync(releasePath, "utf8");
  assert.match(source, /^    run_exact_test mom-llama-runtime lib unused store::tests::unrelated_files_are_not_store_inputs_or_rewritten$/m);
  assert.doesNotMatch(source, /prior_logical_store_import_cleans_plaintext/);
});

function runToolchainSelection(t, missing = false) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "release-toolchain-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const shadow = path.join(directory, "shadow");
  const pinned = path.join(directory, "pinned toolchain");
  fs.mkdirSync(shadow);
  fs.mkdirSync(pinned);
  const write = (file, text) => fs.writeFileSync(file, `#!/bin/sh\n${text}\n`, { mode: 0o755 });
  for (const tool of ["cargo", "rustc", "rustdoc"]) {
    write(path.join(shadow, tool), `echo wrong-${tool}`);
    write(path.join(pinned, tool), `echo pinned-${tool}`);
  }
  write(path.join(shadow, "rustup"), missing
    ? "exit 7"
    : 'test "$*" = "which --toolchain 1.92.0 cargo" || exit 8; printf "%s\\n" "$TEST_PINNED_CARGO"');
  const source = fs.readFileSync(releasePath, "utf8");
  const definition = source.match(/^pin_rust_toolchain\(\) \{[\s\S]*?^\}/m)?.[0];
  // Without an explicit selection the nested build inherits the ambient tools.
  const setup = definition ? `${definition}\npin_rust_toolchain` : ":";
  return spawnSync("sh", ["-c", `set -eu\n${setup}\ncargo -V\nrustc -V\nrustdoc -V\n"$CARGO" -V\n"$RUSTC" -V\n"$RUSTDOC" -V\nprintf '%s\\n' "$RUSTUP_TOOLCHAIN"`], {
    encoding: "utf8", timeout: 10_000,
    env: {
      ...process.env,
      PATH: `${shadow}${path.delimiter}${process.env.PATH}`,
      TEST_PINNED_CARGO: path.join(pinned, "cargo"),
      CARGO: path.join(shadow, "cargo"),
      RUSTC: path.join(shadow, "rustc"),
      RUSTDOC: path.join(shadow, "rustdoc"),
      RUSTUP_TOOLCHAIN: "wrong-toolchain",
    },
  });
}

test("release pins tools inherited by nested pnpm/Tauri invocations", (t) => {
  const result = runToolchainSelection(t);
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, "pinned-cargo\npinned-rustc\npinned-rustdoc\npinned-cargo\npinned-rustc\npinned-rustdoc\n1.92.0\n");
  const source = fs.readFileSync(releasePath, "utf8");
  const selection = source.indexOf("\npin_rust_toolchain\n");
  assert.ok(selection > 0 && selection < source.indexOf("run pnpm install"), "selection must execute before nested package commands");
});

test("release fails before nested execution when the pinned toolchain is missing", (t) => {
  const result = runToolchainSelection(t, true);
  assert.equal(result.error, undefined);
  assert.equal(result.status, 7, result.stderr);
  assert.equal(result.stdout, "");
});

test("release exact gate rejects unsupported target kinds before Cargo", (t) => {
  const result = runGate(t, { kind: "example" });
  assert.equal(result.status, 2);
  assert.equal(result.calls, "");
  assert.doesNotMatch(result.stdout, /RECORDED=/);
});
