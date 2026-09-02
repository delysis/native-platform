#!/usr/bin/env node

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  CARGO_BUILD_ARGUMENTS,
  TEST_HARNESS_LIST_ARGUMENTS,
  TEST_HARNESS_TIMEOUT_MS,
  assertNoCustomHarnessManifest,
  assertSafeCargoConfig,
  assertSafeHarnessEnvironment,
  assertStandardLibtestRustSource,
  assertSuccessfulCargoBuildFinished,
  createStandardLibtestGuard,
  discoverCanonicalIgnoredTests,
  expectedCargoInventory,
  parseCargoTestArtifacts,
  parseTestHarnessIgnoredList,
  parsePinnedRustToolchain,
  readMetadata,
  readPinnedToolIdentity,
  reconcileCargoInventory,
  selectGuardedTestProfileArtifacts,
  validateArtifactExecutable,
  validateStandardLibtestGuardAfterBuild,
  validateRegistry,
} from "./validate-ignored-tests.mjs";

const root = path.resolve(import.meta.dirname, "../..");
const registry = JSON.parse(fs.readFileSync(path.join(root, "ci/ignored-tests.json"), "utf8"));
let cachedMetadata;
function workspaceMetadata() {
  cachedMetadata ??= readMetadata(root);
  return cachedMetadata;
}

function fixtureWorkspace() {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), "libtest-policy-"));
  const manifestPath = path.join(repoRoot, "Cargo.toml");
  const sourcePath = path.join(repoRoot, "safe.rs");
  fs.writeFileSync(
    manifestPath,
    '[package]\nname = "safe-package"\nversion = "0.1.0"\n',
  );
  fs.writeFileSync(sourcePath, "pub fn safe() {}\n");
  const metadata = {
    workspace_members: ["safe-package 0.1.0"],
    packages: [
      {
        id: "safe-package 0.1.0",
        name: "safe-package",
        manifest_path: manifestPath,
        targets: [
          {
            name: "safe_package",
            kind: ["lib"],
            src_path: sourcePath,
            test: true,
          },
        ],
      },
    ],
  };
  return {
    repoRoot,
    sourcePath,
    metadata,
    registry: { reviewed_build_scripts: [] },
    environment: {
      CARGO_HOME: path.join(repoRoot, ".cargo-home"),
      HOME: repoRoot,
    },
  };
}

test("authoritative inventory requests no-run compilation and guarded list arguments", () => {
  assert.ok(CARGO_BUILD_ARGUMENTS.includes("--no-run"));
  assert.ok(!CARGO_BUILD_ARGUMENTS.includes("--ignored"));
  assert.deepEqual(TEST_HARNESS_LIST_ARGUMENTS, ["--ignored", "--list"]);
  assert.equal(TEST_HARNESS_TIMEOUT_MS, 30_000);
  const validatorSource = fs.readFileSync(
    path.join(root, "scripts/ci/validate-ignored-tests.mjs"),
    "utf8",
  );
  assert.match(validatorSource, /cargo_rustc_test_mode_requested = true/);
  assert.match(validatorSource, /guarded_list_execution = true/);
  assert.match(validatorSource, /guarded_harness_arguments/);
  assert.match(validatorSource, /killSignal: "SIGKILL"/);
  assert.doesNotMatch(validatorSource, /harness_list_only\s*=/);
  assert.doesNotMatch(validatorSource, /no_test_body_execution\s*=/);
  assert.doesNotMatch(validatorSource, /\.no_test_execution\s*=/);
});

test("toolchain policy requires one exact stable Rust version", () => {
  assert.equal(
    parsePinnedRustToolchain('[toolchain]\nchannel = "1.92.0"\n'),
    "1.92.0",
  );
  for (const source of [
    '[toolchain]\nchannel = "stable"\n',
    '[toolchain]\nchannel = "1.92"\n',
    '[toolchain]\nchannel = "1.92.0"\nchannel = "1.93.0"\n',
    String.raw`[toolchain]
"ch\u0061nnel" = "1.92.0"
`,
  ]) {
    assert.throws(
      () => parsePinnedRustToolchain(source),
      /exact stable Rust version|exactly one toolchain|Unicode escapes/,
    );
  }
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

test("artifact selection requires an unchanged post-build standard-libtest guard", (context) => {
  const fixture = fixtureWorkspace();
  context.after(() => fs.rmSync(fixture.repoRoot, { force: true, recursive: true }));
  const prebuild = createStandardLibtestGuard(fixture);
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
        name: "fabricated",
        kinds: ["custom-build"],
        src_path: "build.rs",
      },
    },
  ];
  assert.throws(
    () =>
      selectGuardedTestProfileArtifacts(artifacts, {
        metadata: fixture.metadata,
        repoRoot: fixture.repoRoot,
        guard: prebuild,
      }),
    /unchanged post-build standard-libtest guard/,
  );
  const postbuild = validateStandardLibtestGuardAfterBuild({
    guard: prebuild,
    ...fixture,
  });
  assert.deepEqual(
    selectGuardedTestProfileArtifacts(artifacts, {
      metadata: fixture.metadata,
      repoRoot: fixture.repoRoot,
      guard: postbuild,
    }).map((artifact) => artifact.executable),
    ["/tmp/safe"],
  );
});

test("metadata test target roots are guarded regardless of extension or symlinks", (context) => {
  const fixture = fixtureWorkspace();
  const outsideRoot = fs.mkdtempSync(path.join(os.tmpdir(), "outside-target-"));
  context.after(() => {
    fs.rmSync(fixture.repoRoot, { force: true, recursive: true });
    fs.rmSync(outsideRoot, { force: true, recursive: true });
  });
  const target = fixture.metadata.packages[0].targets[0];
  const nonRustRoot = path.join(fixture.repoRoot, "custom-root.data");
  fs.writeFileSync(nonRustRoot, "#![no_main]\npub fn hidden() {}\n");
  target.src_path = nonRustRoot;
  assert.throws(
    () => createStandardLibtestGuard(fixture),
    /custom test-framework crate attributes/,
  );
  fs.writeFileSync(nonRustRoot, "pub fn guarded() {}\n");
  assert.doesNotThrow(() => createStandardLibtestGuard(fixture));

  fs.writeFileSync(
    nonRustRoot,
    [
      '#[test]\n#[ignore = "registered"]\nfn registered() {}',
      '#[test]\n#[ignore = "unregistered"]\nfn unregistered() {}',
      "",
    ].join("\n"),
  );
  const fixtureRegistry = {
    schema: "native-platform.ignored-tests.v2",
    expected_test_count: 1,
    cargo_targets: [
      {
        package: "safe-package",
        selector: "lib",
        name: "safe_package",
        kinds: ["lib"],
        src_path: "custom-root.data",
        manifest_path: "Cargo.toml",
        platforms: ["linux", "macos", "windows"],
        harness: "libtest",
      },
    ],
    reviewed_build_scripts: [],
    entries: [
      {
        test_id: "registered",
        package: "safe-package",
        target: "lib",
        source: "custom-root.data",
        prerequisite: "fixture prerequisite",
        required_environment: [],
        evidence_class: "fixture",
        promotion_prohibition: "This fixture cannot promote any evidence.",
        platforms: ["linux", "macos", "windows"],
      },
    ],
  };
  assert.throws(
    () =>
      validateRegistry({
        registry: fixtureRegistry,
        metadata: fixture.metadata,
        repoRoot: fixture.repoRoot,
        environment: fixture.environment,
      }),
    /ignored source registry drift.*unregistered: custom-root\.data:unregistered/s,
  );
  fs.writeFileSync(nonRustRoot, "pub fn guarded() {}\n");

  if (process.platform !== "win32") {
    const linkedRoot = path.join(fixture.repoRoot, "linked-root.data");
    fs.symlinkSync(nonRustRoot, linkedRoot);
    target.src_path = linkedRoot;
    assert.throws(
      () => createStandardLibtestGuard(fixture),
      /test target root must not be a symlink/,
    );
  }

  const outsideSource = path.join(outsideRoot, "outside-root.data");
  fs.writeFileSync(outsideSource, "pub fn outside() {}\n");
  target.src_path = outsideSource;
  assert.throws(
    () => createStandardLibtestGuard(fixture),
    /outside its repository or package/,
  );

  target.src_path = path.dirname(fixture.repoRoot);
  assert.throws(
    () => createStandardLibtestGuard(fixture),
    /outside its repository or package/,
  );

  const directoryRoot = path.join(fixture.repoRoot, "directory-root");
  fs.mkdirSync(directoryRoot);
  target.src_path = directoryRoot;
  assert.throws(
    () => createStandardLibtestGuard(fixture),
    /test target root must be a regular file/,
  );
});

test("guarded artifacts must be regular nonsymlink files inside Cargo target output", (context) => {
  const fixtureRoot = fs.mkdtempSync(path.join(os.tmpdir(), "artifact-guard-"));
  context.after(() => fs.rmSync(fixtureRoot, { force: true, recursive: true }));
  const targetDirectory = path.join(fixtureRoot, "target");
  fs.mkdirSync(targetDirectory);
  const executable = path.join(targetDirectory, "safe-harness");
  fs.writeFileSync(executable, "guarded bytes");
  assert.equal(
    validateArtifactExecutable(executable, targetDirectory).sha256,
    createHash("sha256").update("guarded bytes").digest("hex"),
  );

  const outside = path.join(fixtureRoot, "outside-harness");
  fs.writeFileSync(outside, "outside");
  assert.throws(
    () => validateArtifactExecutable(outside, targetDirectory),
    /outside the metadata target directory/,
  );

  if (process.platform !== "win32") {
    const linked = path.join(targetDirectory, "linked-harness");
    fs.symlinkSync(executable, linked);
    assert.throws(
      () => validateArtifactExecutable(linked, targetDirectory),
      /must not be a symbolic link/,
    );
  }
});

test("Cargo JSON requires one successful build-finished message", () => {
  assert.doesNotThrow(() =>
    assertSuccessfulCargoBuildFinished(
      `${JSON.stringify({ reason: "build-finished", success: true })}\n`,
    ),
  );
  for (const output of [
    "",
    `${JSON.stringify({ reason: "build-finished", success: false })}\n`,
    `${JSON.stringify({ reason: "build-finished", success: true })}\n${JSON.stringify({ reason: "build-finished", success: true })}\n`,
  ]) {
    assert.throws(
      () => assertSuccessfulCargoBuildFinished(output),
      /exactly one successful build-finished/,
    );
  }
});

test("crate-level custom test-framework probes fail closed across Rust syntax", () => {
  for (const source of [
    "#![no_main]\n",
    "#! [ feature ( custom_test_frameworks ) ]\n",
    "#![r#test_runner(crate::runner)]\n",
    "#![reexport_test_harness_main = \"generated_main\"]\n",
    "#![cfg_attr(any(), no_main)]\n",
    "#![cfg_attr(any(), feature(r#custom_test_frameworks))]\n",
    "#![cfg_attr(any(), r#reexport_test_harness_main = \"main\")]\n",
    "#![cfg_attr(any(), r#crate_type = \"bin\")]\n",
    "macro_rules! crate_attr { ($name:ident) => { #![$name] }; }\n",
    "r#include!(concat!(env!(\"OUT_DIR\"), \"/runner.rs\"));\n",
  ]) {
    assert.throws(
      () => assertStandardLibtestRustSource(source, "adversarial.rs"),
      /custom test-framework|macro-generated crate attributes|include!/,
    );
  }
  assert.doesNotThrow(() =>
    assertStandardLibtestRustSource(
      '// #![no_main]\nconst TEXT: &str = r#"#![test_runner(fake)]"#;\n',
      "decoys.rs",
    ),
  );
});

test("Cargo configuration and environment cannot inject compiler flags or runners", () => {
  for (const config of [
    '[build]\nrustflags = ["-Zcrate-attr=no_main"]\n',
    '[build]\nrustdocflags = ["-Zcrate-attr=no_main"]\n',
    '[build]\nrustc-wrapper = "./injector"\n',
    '[target.x86_64-unknown-linux-gnu]\nlinker = "./injector"\n',
    '[target.x86_64-unknown-linux-gnu]\nrunner = "./injector"\n',
    '[env]\nRUSTC_BOOTSTRAP = "1"\n',
    '[env]\nLD_PRELOAD = "./injector.so"\n',
    '[env]\nLD_AUDIT = "./auditor.so"\n',
    '[env]\nDYLD_INSERT_LIBRARIES = "./injector.dylib"\n',
    'env.DYLD_PRINT_LIBRARIES = "1"\n',
    '[build]\ntarget = "./custom-target.json"\n',
    '[build]\ntarget-dir = "../untrusted-target"\n',
    '[source.crates-io]\nreplace-with = "fabricated"\n',
    'paths = ["../replacement"]\n',
    String.raw`[build]
"rust\u0066lags" = ["-Zcrate-attr=no_main"]
`,
  ]) {
    assert.throws(
      () => assertSafeCargoConfig(config, "fixture/.cargo/config.toml"),
      /standard-libtest policy|configuration is prohibited/,
    );
  }
  for (const environment of [
    { CARGO: "./fabricated-cargo" },
    { CARGO_TARGET_DIR: "../untrusted-target" },
    { RUSTC_BOOTSTRAP: "1" },
    { RUSTFLAGS: "-Zcrate-attr=no_main" },
    { RUSTDOCFLAGS: "-Zcrate-attr=no_main" },
    { CARGO_ENCODED_RUSTFLAGS: "-Zcrate-attr=no_main" },
    { CARGO_BUILD_RUSTC_WRAPPER: "./injector" },
    { CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER: "./injector" },
    { CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER: "./injector" },
    { CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS: "-Zcrate-attr=no_main" },
    { DYLD_INSERT_LIBRARIES: "./injector.dylib" },
    { LD_AUDIT: "./auditor.so" },
    { LD_LIBRARY_PATH: "./untrusted-libraries" },
    { LD_PRELOAD: "./injector.so" },
  ]) {
    assert.throws(
      () => assertSafeHarnessEnvironment(environment),
      /environment overrides/,
    );
  }
});

test("unreviewed build scripts and workspace procedural macros fail before listing", (context) => {
  const fixture = fixtureWorkspace();
  context.after(() => fs.rmSync(fixture.repoRoot, { force: true, recursive: true }));
  const workspacePackage = fixture.metadata.packages[0];
  const buildPath = path.join(fixture.repoRoot, "build.rs");
  fs.writeFileSync(buildPath, "fn main() {}\n");
  workspacePackage.targets.push({
    name: "build-script-build",
    kind: ["custom-build"],
    src_path: buildPath,
    test: false,
  });
  assert.throws(
    () => createStandardLibtestGuard(fixture),
    /unreviewed: safe-package:build.rs/,
  );

  fixture.registry.reviewed_build_scripts.push({
    package: "safe-package",
    src_path: "build.rs",
    sha256: "0".repeat(64),
  });
  assert.throws(
    () => createStandardLibtestGuard(fixture),
    /build-script digest/,
  );
  fixture.registry.reviewed_build_scripts[0].sha256 = createHash("sha256")
    .update(fs.readFileSync(buildPath))
    .digest("hex");
  assert.doesNotThrow(() => createStandardLibtestGuard(fixture));

  fs.writeFileSync(
    buildPath,
    'fn main() { println!("cargo:rustc-link-arg-tests=-Wl,-e,arbitrary"); }\n',
  );
  fixture.registry.reviewed_build_scripts[0].sha256 = createHash("sha256")
    .update(fs.readFileSync(buildPath))
    .digest("hex");
  assert.throws(
    () => createStandardLibtestGuard(fixture),
    /may not emit rustc-link-arg-tests/,
  );
  fs.writeFileSync(buildPath, "fn main() {}\n");
  fixture.registry.reviewed_build_scripts[0].sha256 = createHash("sha256")
    .update(fs.readFileSync(buildPath))
    .digest("hex");

  workspacePackage.targets.push({
    name: "injector",
    kind: ["proc-macro"],
    src_path: fixture.sourcePath,
    test: true,
  });
  assert.throws(
    () => createStandardLibtestGuard(fixture),
    /workspace procedural macros are outside/,
  );
});

test("post-build guard rejects source mutation before any artifact can spawn", (context) => {
  const fixture = fixtureWorkspace();
  context.after(() => fs.rmSync(fixture.repoRoot, { force: true, recursive: true }));
  const guard = createStandardLibtestGuard(fixture);
  fs.writeFileSync(fixture.sourcePath, "#![no_main]\npub fn safe() {}\n");
  assert.throws(
    () => validateStandardLibtestGuardAfterBuild({ guard, ...fixture }),
    /custom test-framework crate attributes/,
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
  const metadata = workspaceMetadata();
  const report = validateRegistry({ registry, metadata, repoRoot: root });
  assert.equal(report.registry_count, 37);
  assert.equal(report.cargo_target_count, 14);
  assert.equal(report.reviewed_build_script_count, 7);
  assert.equal(report.workspace_proc_macro_count, 0);
  assert.ok(report.guarded_source_count > 0);
  assert.ok(report.guarded_test_target_root_count > 0);
  assert.ok(registry.cargo_targets.every((target) => target.harness === "libtest"));
  assert.deepEqual(report.platform_counts, { linux: 36, macos: 37, windows: 33 });
  assert.ok(report.evidence_classes.includes("real-model-runtime"));
  assert.ok(report.evidence_classes.includes("real-corpus-read-only"));
  assert.ok(report.evidence_classes.includes("real-platform-tts-runtime"));
});

test("Cargo reconciliation resolves the repository-pinned Rust toolchain", () => {
  const identity = readPinnedToolIdentity({ repoRoot: root });
  assert.equal(identity.channel, "1.92.0");
  assert.match(identity.cargo_version, /^cargo 1\.92\.0(?: |$)/);
  assert.match(identity.rustc_version, /^rustc 1\.92\.0(?: |$)/);
  assert.ok(path.isAbsolute(identity.cargo_path));
  assert.ok(path.isAbsolute(identity.rustc_path));
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
  const metadata = workspaceMetadata();
  const packageMetadata = metadata.packages.find(
    (candidate) => candidate.name === "mom-llama-runtime",
  );
  const cargoTarget = packageMetadata.targets.find((candidate) => candidate.name === "runtime");
  const stdout = `${JSON.stringify({
    reason: "compiler-artifact",
    package_id: packageMetadata.id,
    target: cargoTarget,
    profile: { test: true },
    fresh: false,
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
  const metadata = workspaceMetadata();
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
