#!/usr/bin/env node

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import {
  CARGO_BUILD_ARGUMENTS,
  TEST_HARNESS_LIST_ARGUMENTS,
  TEST_HARNESS_TIMEOUT_MS,
  assertNoCustomHarnessManifest,
  discoverCanonicalIgnoredTests,
  expectedCargoInventory,
  parseCargoTestArtifacts,
  parseTestHarnessIgnoredList,
  readMetadata,
  reconcileCargoInventory,
  selectStandardLibtestArtifacts,
  validateRegistry,
} from "./validate-ignored-tests.mjs";

const root = path.resolve(import.meta.dirname, "../..");
const registry = JSON.parse(fs.readFileSync(path.join(root, "ci/ignored-tests.json"), "utf8"));
const metadata = readMetadata(root);

test("authoritative inventory builds without test bodies and lists standard harnesses only", () => {
  assert.ok(CARGO_BUILD_ARGUMENTS.includes("--no-run"));
  assert.ok(!CARGO_BUILD_ARGUMENTS.includes("--ignored"));
  assert.deepEqual(TEST_HARNESS_LIST_ARGUMENTS, ["--ignored", "--list"]);
  assert.equal(TEST_HARNESS_TIMEOUT_MS, 30_000);
  const validatorSource = fs.readFileSync(
    path.join(root, "scripts/ci/validate-ignored-tests.mjs"),
    "utf8",
  );
  assert.match(validatorSource, /harness_list_only = true/);
  assert.match(validatorSource, /no_test_body_execution = true/);
  assert.doesNotMatch(validatorSource, /\.no_test_execution\s*=/);
});

test("custom Cargo harness configuration is rejected before executable listing", () => {
  for (const manifest of [
    "[lib]\nharness = false\n",
    "[[test]]\nname = 'custom'\n'harness' = false\n",
    String.raw`[[test]]
name = "escaped"
"h\u0061rness" = false
`,
  ]) {
    assert.throws(
      () => assertNoCustomHarnessManifest(manifest, "fixture/Cargo.toml"),
      /default-libtest policy|standard libtest is required/,
    );
  }
});

test("only metadata-confirmed standard libtest artifacts are executable candidates", () => {
  const syntheticMetadata = {
    workspace_members: ["safe-package 0.1.0"],
    packages: [
      {
        id: "safe-package 0.1.0",
        name: "safe-package",
        manifest_path: path.join(root, "Cargo.toml"),
        targets: [
          {
            name: "safe_package",
            kind: ["lib"],
            src_path: path.join(root, "safe.rs"),
            test: true,
          },
          {
            name: "build-script-build",
            kind: ["custom-build"],
            src_path: path.join(root, "build.rs"),
            test: true,
          },
        ],
      },
    ],
  };
  const artifacts = [
    {
      executable: "/tmp/safe",
      target: {
        package: "safe-package",
        name: "safe_package",
        kinds: ["lib"],
        src_path: "safe.rs",
      },
    },
    {
      executable: "/tmp/arbitrary-main",
      target: {
        package: "safe-package",
        name: "build-script-build",
        kinds: ["custom-build"],
        src_path: "build.rs",
      },
    },
  ];
  assert.deepEqual(
    selectStandardLibtestArtifacts(artifacts, {
      metadata: syntheticMetadata,
      repoRoot: root,
    }).map((artifact) => artifact.executable),
    ["/tmp/safe"],
  );
});

test("canonical source discovery ignores decoys and finds private libtest functions", () => {
  const source = String.raw`
// #[test] #[ignore = "comment"] fn comment_decoy() {}
const TEXT: &str = r#"#[test] #[ignore = "string"] fn string_decoy() {}"#;

#[cfg(unix)]
#[test]
#[ignore = "fixture"]
fn private_test() {}

#[tokio::test(flavor = "current_thread")]
#[ignore = "fixture"]
async fn private_async_test() {}
`;
  assert.deepEqual(discoverCanonicalIgnoredTests(source, "fixture.rs"), [
    "private_test",
    "private_async_test",
  ]);
});

test("noncanonical cfg_attr, macro, and public ignored tests fail closed", () => {
  const adversarial = [
    "#[test]\n#[cfg_attr(any(), ignore = \"conditional\")]\nfn conditional() {}\n",
    "macro_rules! ignored { () => { #[test] #[ignore = \"macro\"] fn generated() {} }; }\n",
    "#[test]\n#[ignore = \"public\"]\npub fn public_test() {}\n",
    "#[test]\n#[ignore]\nfn reasonless() {}\n",
  ];
  for (const source of adversarial) {
    assert.throws(
      () => discoverCanonicalIgnoredTests(source, "adversarial.rs"),
      /noncanonical|macro-generated|private functions/,
    );
  }
});

test("all ignored tests carry exact target, platform, evidence, and non-promotion metadata", () => {
  const report = validateRegistry({ registry, metadata, repoRoot: root });
  assert.equal(report.registry_count, 37);
  assert.equal(report.cargo_target_count, 14);
  assert.ok(registry.cargo_targets.every((target) => target.harness === "libtest"));
  assert.deepEqual(report.platform_counts, { linux: 36, macos: 37, windows: 33 });
  assert.ok(report.evidence_classes.includes("real-model-runtime"));
  assert.ok(report.evidence_classes.includes("real-corpus-read-only"));
  assert.ok(report.evidence_classes.includes("real-platform-tts-runtime"));
});

test("the only platform-limited tests match their source cfg gates", () => {
  const limited = registry.entries
    .filter((entry) => entry.platforms.length < 3)
    .map((entry) => [entry.test_id, entry.platforms]);
  assert.deepEqual(limited, [
    [
      "apple_backend::tests::real_apple_tts_returns_silent_wav_bytes_without_permission",
      ["macos"],
    ],
    [
      "real_native_tool_loop_cancels_an_active_model_request",
      ["linux", "macos"],
    ],
    [
      "real_native_tool_loop_invokes_model_and_persists_tool_lineage",
      ["linux", "macos"],
    ],
    [
      "resident_model_profiles_fail_closed_before_memory_overcommit_without_eviction",
      ["linux", "macos"],
    ],
  ]);
});

test("Cargo test-harness parsing preserves exact full test IDs", () => {
  const output = [
    "tests::one: test",
    "nested::tests::two: test",
    "",
    "2 tests, 0 benchmarks",
  ].join("\n");
  assert.deepEqual(parseTestHarnessIgnoredList(output), [
    "tests::one",
    "nested::tests::two",
  ]);
});

test("Cargo compiler artifacts retain the real package and target identity", () => {
  const packageMetadata = metadata.packages.find(
    (candidate) => candidate.name === "mom-llama-runtime",
  );
  const cargoTarget = packageMetadata.targets.find((candidate) => candidate.name === "runtime");
  const stdout = `${JSON.stringify({
    reason: "compiler-artifact",
    package_id: packageMetadata.id,
    target: cargoTarget,
    profile: { test: true },
    executable: "/tmp/mom-runtime-test-harness",
  })}\n`;
  const [artifact] = parseCargoTestArtifacts(stdout, { metadata, repoRoot: root });
  assert.deepEqual(artifact.target, {
    package: "mom-llama-runtime",
    name: "runtime",
    kinds: ["test"],
    src_path: "products/mom/crates/mom-llama-runtime/tests/runtime.rs",
  });
});

test("reconciliation rejects a fabricated namespace even when the fn segment is real", () => {
  const fabricated = structuredClone(registry);
  const original = fabricated.entries[0];
  const functionName = original.test_id.split("::").at(-1);
  original.test_id = `fabricated::namespace::${functionName}`;

  assert.throws(
    () => reconcileCargoInventory(fabricated, expectedCargoInventory(registry, "macos"), "macos"),
    /Cargo ignored inventory mismatch.*unregistered or unavailable/s,
  );
});

test("structural validation rejects a catalog identity for a nonexistent Cargo target", () => {
  const nonexistentSelector = structuredClone(registry);
  nonexistentSelector.entries[0].target = "nonexistent";
  assert.throws(
    () => validateRegistry({ registry: nonexistentSelector, metadata, repoRoot: root }),
    /nonexistent Cargo target/,
  );

  const nonexistent = structuredClone(registry);
  nonexistent.cargo_targets[0].name = "fabricated_target_that_does_not_exist";
  assert.throws(
    () => validateRegistry({ registry: nonexistent, metadata, repoRoot: root }),
    /Cargo target not found/,
  );
});

test("reconciliation compares only the explicitly available current-platform subset", () => {
  const macos = expectedCargoInventory(registry, "macos");
  const linux = expectedCargoInventory(registry, "linux");
  const windows = expectedCargoInventory(registry, "windows");
  assert.equal(macos.length, 37);
  assert.equal(linux.length, 36);
  assert.equal(windows.length, 33);
  assert.equal(reconcileCargoInventory(registry, macos, "darwin").cargo_count, 37);
  assert.equal(reconcileCargoInventory(registry, linux, "linux").cargo_count, 36);
  assert.equal(reconcileCargoInventory(registry, windows, "win32").cargo_count, 33);
});

test("reconciliation fails for missing available or present unavailable tests", () => {
  const linux = expectedCargoInventory(registry, "linux");
  assert.throws(
    () => reconcileCargoInventory(registry, linux.slice(1), "linux"),
    /missing:/,
  );

  const appleOnly = expectedCargoInventory(registry, "macos").find(
    (entry) => entry.test_id.includes("real_apple_tts"),
  );
  assert.ok(appleOnly);
  assert.throws(
    () => reconcileCargoInventory(registry, [...linux, appleOnly], "linux"),
    /unregistered or unavailable:/,
  );
});
